//! IPC commands for in-app updates (ADR-012).
//!
//! The updater plugin reads `latest.json` from the endpoint in `tauri.conf.json`, verifies the
//! bundle's signature against the public key in the same file and installs it. Nothing the UI
//! sends can change the endpoint, the key or the page [`open_release_page`] opens.
//!
//! The flow is: [`check_for_update`] -> [`download_update`] (progress arrives as
//! [`UpdateDownloadProgress`] events) -> [`install_update_and_restart`]. Installing is refused
//! while a recording or a processing run is active, so an update never cuts one short.
//!
//! The other direction is closed too: from the moment an install starts until the application
//! restarts, no new recording or processing run may start. That is the *install gate* kept in
//! [`Slots`]: the install takes it first and only then looks at what is active; every start
//! takes a [`StartPermit`] from it for as long as the start takes. Both go through the same
//! mutex, so either the start is admitted before the gate closes (and the install sees it as
//! active) or it is refused; a recording or a run cannot slip in between.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tauri_specta::Event;

use crate::commands::recording;
use crate::error::{ErrorCode, IpcError};
use crate::state::AppState;

/// The release page the "download manually" path opens. A constant: the UI cannot pass a URL.
pub const RELEASES_URL: &str = "https://github.com/muhammetsafak/TanAssistant/releases/latest";

/// How long the check (the manifest request) may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Release notes are shown in the UI as text; anything longer is cut.
const MAX_NOTES_CHARS: usize = 20_000;
/// The manifest's publication date is shown as is; anything longer is cut.
const MAX_PUB_DATE_CHARS: usize = 64;
/// How often a download reports progress (plus once at the start and once at the end).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Why an update action was refused or failed. Mapped to an [`IpcError`] with a generic message;
/// the cause of a [`Self::Failed`] is logged where it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateError {
    /// Not possible right now.
    Blocked(BlockedReason),
    /// The update is installed, but a recording or a processing run is active, so the restart
    /// is held back. Unlike [`Self::Blocked`], nothing has to be installed again.
    RestartPending,
    /// There is nothing to download or install.
    NotFound,
    /// This installation cannot update itself.
    Unsupported,
    /// The updater, the network or the installer failed.
    Failed,
}

/// What stands in the way of an update right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
    /// A recording is running, paused, starting or stopping.
    Recording,
    /// A session is being processed.
    Processing,
    /// An update is already downloading.
    DownloadRunning,
    /// An update is already being installed.
    InstallRunning,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(BlockedReason::Recording) => {
                f.write_str("cannot install an update while a recording is active")
            }
            Self::Blocked(BlockedReason::Processing) => {
                f.write_str("cannot install an update while a session is being processed")
            }
            Self::Blocked(BlockedReason::DownloadRunning) => {
                f.write_str("an update is already downloading")
            }
            Self::Blocked(BlockedReason::InstallRunning) => {
                f.write_str("an update is already being installed")
            }
            Self::RestartPending => f.write_str(
                "the update is installed; it takes effect at the next start, or when you \
                 restart again once no recording or processing is active",
            ),
            Self::NotFound => f.write_str("there is no update to download or install"),
            Self::Unsupported => {
                f.write_str("this installation cannot update itself; use the release page")
            }
            Self::Failed => f.write_str("the update failed; see the log for details"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<UpdateError> for IpcError {
    fn from(error: UpdateError) -> Self {
        let code = match error {
            UpdateError::Blocked(_) => ErrorCode::UpdateBlocked,
            UpdateError::RestartPending => ErrorCode::UpdateRestartPending,
            UpdateError::NotFound => ErrorCode::UpdateNotFound,
            UpdateError::Unsupported => ErrorCode::UpdateUnsupported,
            UpdateError::Failed => ErrorCode::UpdateFailed,
        };
        Self::new(code, error.to_string())
    }
}

/// A new recording or processing run was refused because an update is being installed (or is
/// installed and only waits for the restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateInProgress;

impl std::fmt::Display for UpdateInProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "an update is being installed; recording and processing are unavailable until the \
             application restarts",
        )
    }
}

impl std::error::Error for UpdateInProgress {}

/// Which kind of start a [`StartPermit`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// A recording starts or resumes.
    Recording,
    /// A processing run starts (the permit is held for the whole run).
    Processing,
}

/// Whether an installation can replace itself. `is_linux` and `appimage` are the facts, passed
/// in so the rule is testable: on Linux only an `AppImage` can (a `.deb` is owned by the package
/// manager, and its files are not writable by the user); Windows and macOS always can.
#[must_use]
pub fn can_self_update_for(is_linux: bool, appimage: bool) -> bool {
    !is_linux || appimage
}

/// [`can_self_update_for`] for the running process: Linux sets `APPIMAGE` inside an `AppImage`.
#[must_use]
pub fn can_self_update() -> bool {
    let appimage = std::env::var_os("APPIMAGE").is_some_and(|value| !value.is_empty());
    can_self_update_for(cfg!(target_os = "linux"), appimage)
}

/// The facts [`ensure_can_download`] decides on.
#[derive(Debug, Clone, Copy)]
pub struct DownloadContext {
    /// See [`can_self_update`].
    pub can_self_update: bool,
    /// A check found an update.
    pub update_found: bool,
    /// A download is running.
    pub download_running: bool,
}

/// Decides whether [`download_update`] may start. Pure, so it needs no `AppHandle`.
pub fn ensure_can_download(context: DownloadContext) -> Result<(), UpdateError> {
    if !context.can_self_update {
        return Err(UpdateError::Unsupported);
    }
    if !context.update_found {
        return Err(UpdateError::NotFound);
    }
    if context.download_running {
        return Err(UpdateError::Blocked(BlockedReason::DownloadRunning));
    }
    Ok(())
}

/// What the application is doing that a restart would cut short.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    /// A recording is running, paused, starting or stopping.
    pub recording: bool,
    /// A session is being processed.
    pub processing: bool,
}

/// The facts [`ensure_can_install`] decides on.
#[derive(Debug, Clone, Copy)]
pub struct InstallContext {
    /// See [`can_self_update`].
    pub can_self_update: bool,
    /// An update was downloaded and verified.
    pub update_downloaded: bool,
    /// What a restart would interrupt.
    pub activity: Activity,
}

/// Decides whether [`install_update_and_restart`] may install and restart. Pure, so it needs no
/// `AppHandle`. A restart ends the process: it must never cut a recording or a processing run
/// short.
pub fn ensure_can_install(context: InstallContext) -> Result<(), UpdateError> {
    if !context.can_self_update {
        return Err(UpdateError::Unsupported);
    }
    if !context.update_downloaded {
        return Err(UpdateError::NotFound);
    }
    if context.activity.recording {
        return Err(UpdateError::Blocked(BlockedReason::Recording));
    }
    if context.activity.processing {
        return Err(UpdateError::Blocked(BlockedReason::Processing));
    }
    Ok(())
}

/// Decides whether the application may restart now that the update is installed. `activity` is
/// what was read just before. The update stays installed either way, hence
/// [`UpdateError::RestartPending`] and not [`UpdateError::Blocked`].
pub fn ensure_can_restart(activity: Activity) -> Result<(), UpdateError> {
    if activity == Activity::default() {
        Ok(())
    } else {
        Err(UpdateError::RestartPending)
    }
}

/// An update that was downloaded and verified, waiting for the install.
pub struct Downloaded<U> {
    version: String,
    update: U,
    bytes: Vec<u8>,
}

/// The bookkeeping of the update flow, generic over the update type so that its transitions can
/// be tested without a real `Update` (which only the plugin can build).
pub struct Slots<U> {
    /// The version the last check found, with its update.
    found: Option<(String, U)>,
    /// A download is running.
    downloading: bool,
    /// The verified bundle of a finished download.
    downloaded: Option<Downloaded<U>>,
    /// The bundle is installed; only the restart is left.
    installed: bool,
    /// The install gate is closed: an install runs, or it finished and waits for the restart.
    /// No recording or processing run may start meanwhile.
    install_gate_closed: bool,
    /// Recording starts that were admitted and have not finished starting.
    recordings_starting: usize,
    /// Processing runs that were admitted and have not ended.
    processing_runs: usize,
}

impl<U> Default for Slots<U> {
    fn default() -> Self {
        Self {
            found: None,
            downloading: false,
            downloaded: None,
            installed: false,
            install_gate_closed: false,
            recordings_starting: 0,
            processing_runs: 0,
        }
    }
}

impl<U: Clone> Slots<U> {
    /// Remembers what a check found. A bundle downloaded for another version is of no use any
    /// more and is dropped.
    pub fn record_found(&mut self, version: &str, update: U) {
        if self
            .downloaded
            .as_ref()
            .is_some_and(|downloaded| downloaded.version != version)
        {
            self.downloaded = None;
        }
        self.found = Some((version.to_owned(), update));
    }

    /// A check found nothing newer.
    pub fn clear_found(&mut self) {
        self.found = None;
    }

    /// Starts a download: returns the update to download, or `None` when its bundle is already
    /// downloaded. Marks the download as running; [`Self::finish_download`] ends it.
    pub fn start_download(&mut self, can_self_update: bool) -> Result<Option<U>, UpdateError> {
        ensure_can_download(DownloadContext {
            can_self_update,
            update_found: self.found.is_some(),
            download_running: self.downloading,
        })?;
        // A download resets what is installed: not while an install is running.
        if self.install_gate_closed && !self.installed {
            return Err(UpdateError::Blocked(BlockedReason::InstallRunning));
        }
        let (version, update) = self.found.clone().ok_or(UpdateError::NotFound)?;
        if self
            .downloaded
            .as_ref()
            .is_some_and(|downloaded| downloaded.version == version)
        {
            return Ok(None);
        }
        self.downloading = true;
        Ok(Some(update))
    }

    /// Ends a download, whatever its outcome: a failure leaves no bundle and no running flag.
    pub fn finish_download(&mut self, version: &str, outcome: Option<(U, Vec<u8>)>) {
        self.downloading = false;
        if let Some((update, bytes)) = outcome {
            self.downloaded = Some(Downloaded {
                version: version.to_owned(),
                update,
                bytes,
            });
            // A fresh bundle replaces what was installed; its install closes the gate again.
            self.installed = false;
            self.install_gate_closed = false;
        }
    }

    /// Whether there is something to install or (after an install) to restart for.
    #[must_use]
    pub fn has_package(&self) -> bool {
        self.downloaded.is_some() || self.installed
    }

    /// Hands the bundle over for installing; `None` once it is installed already.
    pub fn take_for_install(&mut self) -> Option<Downloaded<U>> {
        self.downloaded.take()
    }

    /// Puts the bundle back after a failed install, so the install can be tried again without
    /// downloading it a second time, and opens the install gate again.
    pub fn restore_after_failed_install(&mut self, package: Downloaded<U>) {
        self.downloaded = Some(package);
        self.install_gate_closed = false;
    }

    /// Whether the bundle is installed and only the restart is left.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.installed
    }

    /// The bundle was installed; only the restart is left. The install gate stays closed until
    /// then.
    pub fn mark_installed(&mut self) {
        self.installed = true;
    }

    /// Closes the install gate: from now on no recording or processing run is admitted. Refused
    /// while another install holds it. Call it *before* looking at what is active, so nothing
    /// can start between the look and the install.
    pub fn close_install_gate(&mut self) -> Result<(), UpdateError> {
        if self.install_gate_closed && !self.installed {
            return Err(UpdateError::Blocked(BlockedReason::InstallRunning));
        }
        self.install_gate_closed = true;
        Ok(())
    }

    /// Opens the install gate again, for an install that was refused or did not happen.
    pub fn open_install_gate(&mut self) {
        self.install_gate_closed = false;
    }

    /// Whether the install gate is closed.
    #[must_use]
    pub fn is_install_gate_closed(&self) -> bool {
        self.install_gate_closed
    }

    /// Admits a start, unless the install gate is closed. Every admitted start must be ended
    /// with [`Self::end_start`].
    pub fn admit_start(&mut self, kind: StartKind) -> Result<(), UpdateInProgress> {
        if self.install_gate_closed {
            return Err(UpdateInProgress);
        }
        match kind {
            StartKind::Recording => self.recordings_starting += 1,
            StartKind::Processing => self.processing_runs += 1,
        }
        Ok(())
    }

    /// Ends a start admitted by [`Self::admit_start`].
    pub fn end_start(&mut self, kind: StartKind) {
        let counter = match kind {
            StartKind::Recording => &mut self.recordings_starting,
            StartKind::Processing => &mut self.processing_runs,
        };
        *counter = counter.saturating_sub(1);
    }

    /// The starts admitted and not yet ended. With the gate closed this can only shrink.
    #[must_use]
    pub fn starts_in_flight(&self) -> Activity {
        Activity {
            recording: self.recordings_starting > 0,
            processing: self.processing_runs > 0,
        }
    }
}

/// The update found by the last check and its downloaded bundle. Managed by Tauri.
#[derive(Default)]
pub struct UpdaterState {
    inner: Mutex<Slots<Update>>,
}

impl UpdaterState {
    fn lock(&self) -> MutexGuard<'_, Slots<Update>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits a recording start or a processing run, or refuses it while an update is being
    /// installed. Hold the permit for as long as the start takes (a recording start) or the run
    /// lasts (processing); the install counts it as activity until it is dropped.
    pub fn admit(&self, kind: StartKind) -> Result<StartPermit<'_>, UpdateInProgress> {
        self.lock().admit_start(kind)?;
        Ok(StartPermit { state: self, kind })
    }

    /// See [`Slots::close_install_gate`].
    pub fn close_install_gate(&self) -> Result<(), UpdateError> {
        self.lock().close_install_gate()
    }

    /// See [`Slots::starts_in_flight`].
    #[must_use]
    pub fn starts_in_flight(&self) -> Activity {
        self.lock().starts_in_flight()
    }
}

/// Runs `work` (a recording start or a processing run) under a [`StartPermit`]: refused with
/// [`UpdateInProgress`] while an update installs, otherwise the permit is held until `work` is
/// done. Every gated command goes through this, so the permit cannot be forgotten or dropped
/// early; `work` is not polled when the start is refused.
pub async fn gated<T, E, F>(updater: &UpdaterState, kind: StartKind, work: F) -> Result<T, E>
where
    E: From<UpdateInProgress>,
    F: std::future::Future<Output = Result<T, E>>,
{
    let _permit = updater.admit(kind)?;
    work.await
}

/// An admitted recording start or processing run; see [`UpdaterState::admit`].
#[must_use = "the permit counts as activity only while it is held"]
pub struct StartPermit<'a> {
    state: &'a UpdaterState,
    kind: StartKind,
}

impl Drop for StartPermit<'_> {
    fn drop(&mut self) {
        self.state.lock().end_start(self.kind);
    }
}

/// The result of a check.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// Whether a newer version exists.
    pub available: bool,
    /// The running version.
    pub current_version: String,
    /// The newer version, when there is one.
    pub version: Option<String>,
    /// Its release notes (plain text), when it has any.
    pub notes: Option<String>,
    /// Its publication date as the manifest states it (RFC 3339), when it has one.
    pub pub_date: Option<String>,
    /// Whether this installation can download and install it itself.
    pub can_self_update: bool,
    /// The release page, for installations that cannot update themselves.
    pub release_url: String,
}

/// Emitted while an update downloads (about five times a second, plus at the start and end).
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadProgress {
    /// Bytes received so far.
    pub downloaded: u32,
    /// Total size, when the server states it.
    pub total: Option<u32>,
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn truncated(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn describe(update: &Update) -> UpdateInfo {
    UpdateInfo {
        available: true,
        current_version: update.current_version.clone(),
        version: Some(update.version.clone()),
        notes: update
            .body
            .as_deref()
            .map(|notes| truncated(notes, MAX_NOTES_CHARS))
            .filter(|notes| !notes.trim().is_empty()),
        pub_date: update
            .raw_json
            .get("pub_date")
            .and_then(|date| date.as_str())
            .map(|date| truncated(date, MAX_PUB_DATE_CHARS)),
        can_self_update: can_self_update(),
        release_url: RELEASES_URL.to_owned(),
    }
}

/// Asks the update endpoint whether a newer version exists.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn check_for_update(
    app: AppHandle,
    updater_state: State<'_, UpdaterState>,
) -> Result<UpdateInfo, IpcError> {
    let current = app.package_info().version.to_string();
    // Windows only: the installer ends the process without an exit request, so a recording that
    // started after the install check is settled here, as an ordinary quit would settle it.
    let hook_app = app.clone();
    let updater = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .on_before_exit(move || {
            if recording::is_busy(&hook_app) {
                tracing::warn!("a recording is active as the installer starts; settling it");
                tauri::async_runtime::block_on(recording::stop_and_settle(&hook_app));
            }
        })
        .build()
        .map_err(|error| {
            tracing::warn!(%error, "cannot set up the update check");
            UpdateError::Failed
        })?;
    let checked = updater.check().await.map_err(|error| {
        tracing::warn!(%error, "the update check failed");
        UpdateError::Failed
    })?;

    let Some(mut update) = checked else {
        tracing::info!(current = %current, "no update is available");
        updater_state.lock().clear_found();
        return Ok(UpdateInfo {
            available: false,
            current_version: current,
            version: None,
            notes: None,
            pub_date: None,
            can_self_update: can_self_update(),
            release_url: RELEASES_URL.to_owned(),
        });
    };

    // The check timeout must not cap the download of a large bundle.
    update.timeout = None;
    tracing::info!(current = %current, version = %update.version, "an update is available");
    let info = describe(&update);
    let version = update.version.clone();
    updater_state.lock().record_found(&version, update);
    Ok(info)
}

/// Downloads the update the last check found and verifies its signature. Progress arrives as
/// [`UpdateDownloadProgress`] events. A second download while one runs is refused.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn download_update(
    app: AppHandle,
    updater_state: State<'_, UpdaterState>,
) -> Result<(), IpcError> {
    let Some(update) = updater_state.lock().start_download(can_self_update())? else {
        // Already downloaded and verified: nothing to do.
        return Ok(());
    };
    let version = update.version.clone();

    tracing::info!(version = %update.version, "downloading the update");
    let mut downloaded_bytes: u64 = 0;
    let mut last_report: Option<Instant> = None;
    let outcome = update
        .download(
            |chunk, total| {
                downloaded_bytes += chunk as u64;
                let due = last_report.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL);
                if due {
                    last_report = Some(Instant::now());
                    announce(&app, downloaded_bytes, total);
                }
            },
            || {},
        )
        .await;

    match outcome {
        Ok(bytes) => {
            // The final report, whatever the throttle skipped.
            announce(&app, bytes.len() as u64, Some(bytes.len() as u64));
            tracing::info!(version = %version, "the update was downloaded and verified");
            updater_state
                .lock()
                .finish_download(&version, Some((update, bytes)));
            Ok(())
        }
        Err(error) => {
            tracing::warn!(%error, "the update download failed");
            updater_state.lock().finish_download(&version, None);
            Err(UpdateError::Failed.into())
        }
    }
}

fn announce(app: &AppHandle, downloaded: u64, total: Option<u64>) {
    let event = UpdateDownloadProgress {
        downloaded: saturating_u32(downloaded),
        total: total.map(saturating_u32),
    };
    if let Err(error) = event.emit(app) {
        tracing::debug!(%error, "could not deliver an update progress event to the UI");
    }
}

/// What a restart would interrupt right now. Read it *after* the install gate was closed: the
/// starts still in flight are read first and the live state after them, so a start that ends in
/// between is seen as the recording or the run it became.
fn current_activity(app: &AppHandle, state: &AppState, updater_state: &UpdaterState) -> Activity {
    let in_flight = updater_state.starts_in_flight();
    Activity {
        recording: in_flight.recording || recording::is_busy(app),
        processing: in_flight.processing || state.process_session.is_any_running(),
    }
}

/// Installs the downloaded update and restarts the application. Refused while a recording or a
/// processing run is active.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
pub async fn install_update_and_restart(
    app: AppHandle,
    state: State<'_, AppState>,
    updater_state: State<'_, UpdaterState>,
) -> Result<(), IpcError> {
    // An earlier call installed the update and was only held back from the restart: nothing is
    // left to install, so the request is answered by the restart check alone (and says
    // `update_restart_pending`, not `update_blocked`, while something is still active).
    let already_installed = updater_state.lock().is_installed();
    if !already_installed {
        // Close the gate first, look at what is active second: nothing can start in between.
        // Whatever stops the install from going through opens the gate again.
        updater_state
            .close_install_gate()
            .inspect_err(|error| tracing::info!(%error, "the update install was refused"))?;
        let package = {
            let activity = current_activity(&app, &state, &updater_state);
            let mut inner = updater_state.lock();
            if let Err(error) = ensure_can_install(InstallContext {
                can_self_update: can_self_update(),
                update_downloaded: inner.has_package(),
                activity,
            }) {
                tracing::info!(%error, "the update install was refused");
                inner.open_install_gate();
                return Err(error.into());
            }
            inner.take_for_install()
        };

        if let Some(package) = package {
            tracing::info!(version = %package.version, "installing the update");
            // Installing copies files (and may run an installer): keep it off the async executor.
            // The bundle comes back with the outcome, so a failed install keeps it.
            let installed = tauri::async_runtime::spawn_blocking(move || {
                let outcome = package.update.install(&package.bytes);
                (package, outcome)
            })
            .await;
            match installed {
                // The gate stays closed: only the restart is left.
                Ok((_, Ok(()))) => updater_state.lock().mark_installed(),
                Ok((package, Err(error))) => {
                    tracing::warn!(%error, "the update install failed");
                    updater_state.lock().restore_after_failed_install(package);
                    return Err(UpdateError::Failed.into());
                }
                Err(error) => {
                    tracing::warn!(%error, "the update install task failed");
                    updater_state.lock().open_install_gate();
                    return Err(UpdateError::Failed.into());
                }
            }
        }
    }

    // The gate keeps new starts out, but look once more right before the restart (a safety net
    // for a start that does not take a permit). When something is active the update stays
    // installed and takes effect at the next start (or at a retry of this command once idle).
    ensure_can_restart(current_activity(&app, &state, &updater_state)).inspect_err(|error| {
        tracing::warn!(%error, "the update is installed; the restart is held back");
    })?;
    tracing::info!("the update was installed; restarting");
    app.restart()
}

/// Opens the release page in the system browser, for installations that cannot update
/// themselves. The address is fixed: the UI cannot choose it.
#[tauri::command]
#[specta::specta]
#[tracing::instrument(skip_all)]
#[allow(clippy::unused_async)] // commands are async so Tauri runs them off the main thread
pub async fn open_release_page() -> Result<(), IpcError> {
    tauri_plugin_opener::open_url(RELEASES_URL, None::<&str>).map_err(|error| {
        tracing::warn!(%error, "cannot open the release page");
        UpdateError::Failed.into()
    })
}
