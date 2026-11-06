//! Whisper model files: list, download (verified), delete.
//!
//! Models live in one directory (`<data>/models/`). A download streams into
//! `<file>.part`; only a file whose SHA-256 matches the catalog is renamed to its final name,
//! so **a final file in the directory is always a verified one**. A failed hash check deletes
//! the partial file. An interrupted download keeps its `.part` and is resumed with an HTTP
//! `Range` request the next time.

use std::collections::HashSet;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::{header, Client, StatusCode};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::catalog::{ModelSpec, BUILTIN_MODELS};

/// Progress callbacks are throttled to this interval (the first and last one always fire).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Whether the response status can be resumed or restarted from; see `download`.
const MAX_ATTEMPTS: u32 = 2;

/// File in the models directory that holds the id of the chosen model.
const SELECTED_FILE: &str = "selected";

/// Failure of a model operation. Messages never contain a URL with credentials (there are
/// none) or a local path outside the models directory.
#[derive(Debug)]
pub enum ModelError {
    /// The model id is not in the catalog.
    UnknownModel(String),
    /// The model is in the catalog but its file is not installed.
    NotInstalled(String),
    /// A download or delete of this model is already running.
    Busy(String),
    /// The server could not be reached or the transfer broke off; a retry resumes it.
    Network(String),
    /// The server answered with an unexpected HTTP status.
    HttpStatus(u16),
    /// The server sent more bytes than the catalog says the file has.
    TooLarge,
    /// The transfer ended before the whole file arrived; a retry resumes it.
    Incomplete {
        /// Bytes received so far.
        received: u64,
        /// Bytes expected.
        expected: u64,
    },
    /// The downloaded file's SHA-256 does not match the catalog. The file was deleted.
    ChecksumMismatch(String),
    /// A file operation failed.
    Io(io::Error),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModel(id) => write!(f, "unknown whisper model: {id}"),
            Self::NotInstalled(id) => write!(f, "the whisper model {id} is not installed"),
            Self::Busy(id) => write!(f, "the model {id} is being downloaded or deleted"),
            Self::Network(why) => write!(f, "model download failed: {why}"),
            Self::HttpStatus(status) => write!(f, "model server answered HTTP {status}"),
            Self::TooLarge => f.write_str("the model server sent more data than expected"),
            Self::Incomplete { received, expected } => write!(
                f,
                "model download is incomplete ({received} of {expected} bytes); retry to resume"
            ),
            Self::ChecksumMismatch(id) => write!(
                f,
                "the downloaded model {id} failed its checksum and was deleted"
            ),
            Self::Io(e) => write!(f, "model file operation failed: {e}"),
        }
    }
}

impl Error for ModelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ModelError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Bytes transferred so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes on disk (including what an earlier attempt already fetched).
    pub downloaded: u64,
    /// Total size of the model.
    pub total: u64,
}

/// A catalog model and its state on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// The catalog entry.
    pub spec: ModelSpec,
    /// Whether the verified file is installed.
    pub installed: bool,
    /// Whether this is the model local transcription uses (the chosen one, or the most
    /// preferred installed one when none was chosen).
    pub selected: bool,
    /// Bytes of an interrupted download waiting to be resumed (0 when none).
    pub partial_bytes: u64,
}

/// An installed model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModel {
    /// The catalog entry.
    pub spec: ModelSpec,
    /// The model file.
    pub path: PathBuf,
}

/// Removes the id from the busy set when dropped.
struct BusyGuard {
    busy: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        lock(&self.busy).remove(&self.id);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Manages the whisper model files of one models directory.
pub struct ModelManager {
    dir: PathBuf,
    catalog: Vec<ModelSpec>,
    client: Client,
    busy: Arc<Mutex<HashSet<String>>>,
}

impl ModelManager {
    /// A manager for the built-in catalog. Downloads are HTTPS only.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, ModelError> {
        let client = Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("tan/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ModelError::Network(e.to_string()))?;
        Ok(Self::with_client(dir, BUILTIN_MODELS.to_vec(), client))
    }

    /// A manager for a custom catalog (tests, mirrors). Plain HTTP is allowed here, the
    /// checksum still protects the content.
    pub fn with_catalog(
        dir: impl Into<PathBuf>,
        catalog: Vec<ModelSpec>,
    ) -> Result<Self, ModelError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| ModelError::Network(e.to_string()))?;
        Ok(Self::with_client(dir, catalog, client))
    }

    fn with_client(dir: impl Into<PathBuf>, catalog: Vec<ModelSpec>, client: Client) -> Self {
        Self {
            dir: dir.into(),
            catalog,
            client,
            busy: Arc::default(),
        }
    }

    /// The models directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn spec(&self, id: &str) -> Result<&ModelSpec, ModelError> {
        self.catalog
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| ModelError::UnknownModel(id.to_owned()))
    }

    fn final_path(&self, spec: &ModelSpec) -> PathBuf {
        self.dir.join(spec.file_name)
    }

    fn part_path(&self, spec: &ModelSpec) -> PathBuf {
        self.dir.join(format!("{}.part", spec.file_name))
    }

    fn is_installed(&self, spec: &ModelSpec) -> bool {
        std::fs::metadata(self.final_path(spec))
            .is_ok_and(|m| m.is_file() && m.len() == spec.size_bytes)
    }

    fn begin(&self, id: &str) -> Result<BusyGuard, ModelError> {
        if !lock(&self.busy).insert(id.to_owned()) {
            return Err(ModelError::Busy(id.to_owned()));
        }
        Ok(BusyGuard {
            busy: self.busy.clone(),
            id: id.to_owned(),
        })
    }

    /// Every catalog model with its state, in catalog (preference) order.
    #[must_use]
    pub fn list(&self) -> Vec<ModelInfo> {
        let in_use = self.default_installed().map(|m| m.spec.id);
        self.catalog
            .iter()
            .map(|spec| ModelInfo {
                spec: spec.clone(),
                installed: self.is_installed(spec),
                selected: in_use == Some(spec.id),
                partial_bytes: std::fs::metadata(self.part_path(spec)).map_or(0, |m| m.len()),
            })
            .collect()
    }

    /// The path of an installed model, if it is installed.
    #[must_use]
    pub fn installed_path(&self, id: &str) -> Option<PathBuf> {
        let spec = self.spec(id).ok()?;
        self.is_installed(spec).then(|| self.final_path(spec))
    }

    /// The model local transcription uses: the one chosen with [`Self::select`] if it is still
    /// installed, otherwise the most preferred installed model (the first installed entry of
    /// the catalog).
    #[must_use]
    pub fn default_installed(&self) -> Option<InstalledModel> {
        let chosen = std::fs::read_to_string(self.dir.join(SELECTED_FILE)).ok();
        let chosen = chosen.as_deref().map(str::trim);
        self.catalog
            .iter()
            .find(|spec| Some(spec.id) == chosen && self.is_installed(spec))
            .or_else(|| self.catalog.iter().find(|spec| self.is_installed(spec)))
            .map(|spec| InstalledModel {
                spec: spec.clone(),
                path: self.final_path(spec),
            })
    }

    /// Chooses the installed model local transcription uses from now on.
    pub fn select(&self, id: &str) -> Result<(), ModelError> {
        let spec = self.spec(id)?;
        if !self.is_installed(spec) {
            return Err(ModelError::NotInstalled(id.to_owned()));
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(SELECTED_FILE), spec.id)?;
        Ok(())
    }

    /// Deletes a model and any partial download of it. Returns whether something was removed.
    pub fn delete(&self, id: &str) -> Result<bool, ModelError> {
        let spec = self.spec(id)?;
        let _guard = self.begin(id)?;
        let mut removed = false;
        for path in [self.final_path(spec), self.part_path(spec)] {
            match std::fs::remove_file(&path) {
                Ok(()) => removed = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(removed)
    }

    /// Downloads and verifies a model; returns the installed path. An already installed model
    /// is returned at once. `on_progress` is called at most every 200 ms, plus once at the
    /// start and once at the end.
    ///
    /// On a checksum mismatch the partial file is deleted and the model stays uninstalled.
    pub async fn download(
        &self,
        id: &str,
        mut on_progress: impl FnMut(DownloadProgress) + Send,
    ) -> Result<PathBuf, ModelError> {
        let spec = self.spec(id)?.clone();
        let _guard = self.begin(id)?;
        let total = spec.size_bytes;
        let final_path = self.final_path(&spec);
        if self.is_installed(&spec) {
            on_progress(DownloadProgress {
                downloaded: total,
                total,
            });
            return Ok(final_path);
        }
        tokio::fs::create_dir_all(&self.dir).await?;
        let part = self.part_path(&spec);

        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut have = match tokio::fs::metadata(&part).await {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            };
            if have > total {
                tokio::fs::remove_file(&part).await?;
                have = 0;
            }
            on_progress(DownloadProgress {
                downloaded: have,
                total,
            });

            if have < total {
                match self.fetch_into(&spec, &part, have, &mut on_progress).await {
                    Ok(()) => {}
                    // The server did not like our Range (e.g. the part is stale): restart once.
                    Err(Fetch::RestartFromScratch) if attempt < MAX_ATTEMPTS => {
                        tokio::fs::remove_file(&part).await?;
                        continue;
                    }
                    Err(Fetch::RestartFromScratch) => return Err(ModelError::HttpStatus(416)),
                    Err(Fetch::Failed(e)) => return Err(e),
                }
            }
            break;
        }

        let expected = spec.sha256;
        let part_for_hash = part.clone();
        let actual = tokio::task::spawn_blocking(move || sha256_file(&part_for_hash))
            .await
            .map_err(|e| ModelError::Io(io::Error::other(e)))??;
        if actual != expected {
            // Never keep a file that failed verification, not even for a resume.
            let _ = tokio::fs::remove_file(&part).await;
            return Err(ModelError::ChecksumMismatch(spec.id.to_owned()));
        }
        tokio::fs::rename(&part, &final_path).await?;
        on_progress(DownloadProgress {
            downloaded: total,
            total,
        });
        Ok(final_path)
    }

    /// Streams the model into the `.part` file, resuming at `have` bytes.
    async fn fetch_into(
        &self,
        spec: &ModelSpec,
        part: &Path,
        have: u64,
        on_progress: &mut (impl FnMut(DownloadProgress) + Send),
    ) -> Result<(), Fetch> {
        let total = spec.size_bytes;
        let mut request = self.client.get(spec.url);
        if have > 0 {
            request = request.header(header::RANGE, format!("bytes={have}-"));
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| Fetch::Failed(ModelError::Network(describe(e))))?;

        let resuming = match response.status() {
            StatusCode::PARTIAL_CONTENT if have > 0 => true,
            StatusCode::OK => false,
            StatusCode::RANGE_NOT_SATISFIABLE => return Err(Fetch::RestartFromScratch),
            other => return Err(Fetch::Failed(ModelError::HttpStatus(other.as_u16()))),
        };

        let mut file = if resuming {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .await
                .map_err(|e| Fetch::Failed(e.into()))?
        } else {
            tokio::fs::File::create(part)
                .await
                .map_err(|e| Fetch::Failed(e.into()))?
        };
        let mut downloaded = if resuming { have } else { 0 };
        let mut last_emit = Instant::now();

        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    let _ = file.flush().await;
                    return Err(Fetch::Failed(ModelError::Network(describe(e))));
                }
            };
            downloaded += chunk.len() as u64;
            if downloaded > total {
                drop(file);
                let _ = tokio::fs::remove_file(part).await;
                return Err(Fetch::Failed(ModelError::TooLarge));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| Fetch::Failed(e.into()))?;
            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                last_emit = Instant::now();
                on_progress(DownloadProgress { downloaded, total });
            }
        }
        file.flush().await.map_err(|e| Fetch::Failed(e.into()))?;
        drop(file);

        if downloaded < total {
            return Err(Fetch::Failed(ModelError::Incomplete {
                received: downloaded,
                expected: total,
            }));
        }
        Ok(())
    }
}

/// How a transfer attempt ended unsuccessfully.
enum Fetch {
    /// Throw the `.part` away and start over without a `Range`.
    RestartFromScratch,
    Failed(ModelError),
}

/// The error text of a transport failure without the URL (it is the catalog's, never secret,
/// but it only adds noise).
fn describe(error: reqwest::Error) -> String {
    error.without_url().to_string()
}

/// Lower-case hex SHA-256 of a file.
pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}
