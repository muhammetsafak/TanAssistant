//! Master-password-encrypted file secret store (the fallback when no OS vault exists).
//!
//! ```text
//! master password --Argon2id(salt, params)--> 32-byte key
//! every record: XChaCha20-Poly1305(key, random 24-byte nonce, aad = "record" + id)
//! ```
//!
//! # File format (`secrets.bin`, integers little-endian)
//!
//! ```text
//! magic "TANSECV1"                      8
//! argon2 memory KiB, iterations, lanes  3 x u32
//! salt                                  16
//! verifier nonce + ciphertext           24 + (len(verifier plaintext) + 16)
//! record count                          u32
//! per record: id_len u16, id, nonce[24], ct_len u32, ciphertext (value + 16-byte tag)
//! footer nonce + tag                    24 + 16
//! ```
//!
//! * The **verifier** is a known plaintext sealed with the key (aad = the header bytes): it
//!   tells a wrong password apart from a damaged file.
//! * The **footer** is an AEAD tag over *all preceding bytes* (sealing an empty message with the
//!   file as aad). It makes any edit anywhere in the file, including removing or re-ordering
//!   records, detectable, which per-record tags alone cannot do.
//! * Secret ids are stored in clear text (they are random identifiers, not secrets) but are
//!   bound into each record's aad, so records cannot be swapped between ids.
//!
//! Every write replaces the file atomically (temp file + rename).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use tan_application::ports::{
    SecretBackend, SecretStore, SecretStoreControl, SecretStoreError, SecretStoreResult,
    SecretStoreStatus, SecretValue,
};
use tan_domain::SecretId;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"TANSECV1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;
const VERIFIER_PLAINTEXT: &[u8] = b"tan-secret-store-verifier";
const VERIFIER_CT_LEN: usize = VERIFIER_PLAINTEXT.len() + TAG_LEN;
const HEADER_LEN: usize = MAGIC.len() + 12 + SALT_LEN;
const FOOTER_LEN: usize = NONCE_LEN + TAG_LEN;
const FOOTER_AAD: &[u8] = b"tan-secrets-footer";
const RECORD_AAD: &[u8] = b"tan-secrets-record:";
const MAX_ID_LEN: usize = 256;

/// Argon2id cost parameters. They are stored in the file, so they can be raised later without
/// breaking existing files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of passes.
    pub iterations: u32,
    /// Degree of parallelism (lanes).
    pub parallelism: u32,
}

impl KdfParams {
    /// Production cost: 64 MiB, 3 passes, 1 lane.
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };

    /// A deliberately cheap setting for tests. Never use it for real data.
    pub const INSECURE_FAST_FOR_TESTS: Self = Self {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    };

    // Bounds also protect against a tampered header requesting absurd costs.
    fn validate(self) -> SecretStoreResult<()> {
        let ok = (8..=256 * 1024).contains(&self.memory_kib)
            && (1..=10).contains(&self.iterations)
            && (1..=4).contains(&self.parallelism)
            && self.memory_kib >= 8 * self.parallelism;
        if ok {
            Ok(())
        } else {
            Err(SecretStoreError::Corrupt(
                "key derivation parameters are out of range".to_owned(),
            ))
        }
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone)]
struct Sealed {
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

#[derive(Clone)]
struct Contents {
    params: KdfParams,
    salt: [u8; SALT_LEN],
    verifier: Sealed,
    records: BTreeMap<String, Sealed>,
}

enum State {
    /// No file yet: a master password has to be set first.
    Unconfigured,
    /// A file exists; the password has not been entered.
    Locked,
    Unlocked {
        key: Zeroizing<[u8; KEY_LEN]>,
        contents: Box<Contents>,
    },
}

/// [`SecretStore`] backed by an encrypted file.
pub struct EncryptedFileSecretStore {
    path: PathBuf,
    params: KdfParams,
    state: Mutex<State>,
}

impl std::fmt::Debug for EncryptedFileSecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileSecretStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn corrupt(why: &str) -> SecretStoreError {
    SecretStoreError::Corrupt(why.to_owned())
}

fn random<const N: usize>() -> SecretStoreResult<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|_| SecretStoreError::Backend("no secure random source".to_owned()))?;
    Ok(bytes)
}

fn cipher(key: &[u8; KEY_LEN]) -> SecretStoreResult<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| SecretStoreError::Backend("invalid cipher key".to_owned()))
}

fn seal(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> SecretStoreResult<Sealed> {
    let nonce_bytes = random::<NONCE_LEN>()?;
    let ciphertext = cipher(key)?
        .encrypt(
            &XNonce::from(nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| SecretStoreError::Backend("encryption failed".to_owned()))?;
    Ok(Sealed {
        nonce: nonce_bytes,
        ciphertext,
    })
}

/// Opens a sealed blob. The plaintext is wiped when the returned buffer is dropped.
fn open(key: &[u8; KEY_LEN], sealed: &Sealed, aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, ()> {
    let cipher = cipher(key).map_err(|_| ())?;
    cipher
        .decrypt(
            &XNonce::from(sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| ())
}

fn derive_key(
    password: &SecretValue,
    params: KdfParams,
    salt: &[u8; SALT_LEN],
) -> SecretStoreResult<Zeroizing<[u8; KEY_LEN]>> {
    params.validate()?;
    let argon_params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|_| SecretStoreError::Backend("invalid key derivation parameters".to_owned()))?;
    let mut key = Zeroizing::new([0_u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params)
        .hash_password_into(password.expose().as_bytes(), salt, key.as_mut())
        .map_err(|_| SecretStoreError::Backend("key derivation failed".to_owned()))?;
    Ok(key)
}

fn record_aad(id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(RECORD_AAD.len() + id.len());
    aad.extend_from_slice(RECORD_AAD);
    aad.extend_from_slice(id.as_bytes());
    aad
}

fn footer_aad(body: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(FOOTER_AAD.len() + body.len());
    aad.extend_from_slice(FOOTER_AAD);
    aad.extend_from_slice(body);
    aad
}

impl Contents {
    /// Fresh, empty contents protected by `key`.
    fn new(
        key: &[u8; KEY_LEN],
        params: KdfParams,
        salt: [u8; SALT_LEN],
    ) -> SecretStoreResult<Self> {
        let mut contents = Self {
            params,
            salt,
            verifier: Sealed {
                nonce: [0; NONCE_LEN],
                ciphertext: Vec::new(),
            },
            records: BTreeMap::new(),
        };
        contents.verifier = seal(key, VERIFIER_PLAINTEXT, &contents.header_bytes())?;
        Ok(contents)
    }

    fn header_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.params.memory_kib.to_le_bytes());
        out.extend_from_slice(&self.params.iterations.to_le_bytes());
        out.extend_from_slice(&self.params.parallelism.to_le_bytes());
        out.extend_from_slice(&self.salt);
        out
    }

    /// Serialises the contents and appends the integrity footer.
    fn encode(&self, key: &[u8; KEY_LEN]) -> SecretStoreResult<Vec<u8>> {
        let mut out = self.header_bytes();
        out.extend_from_slice(&self.verifier.nonce);
        out.extend_from_slice(&self.verifier.ciphertext);
        let count = u32::try_from(self.records.len())
            .map_err(|_| SecretStoreError::Backend("too many records".to_owned()))?;
        out.extend_from_slice(&count.to_le_bytes());
        for (id, record) in &self.records {
            let id_len = u16::try_from(id.len())
                .map_err(|_| SecretStoreError::Backend("secret id too long".to_owned()))?;
            let ct_len = u32::try_from(record.ciphertext.len())
                .map_err(|_| SecretStoreError::Backend("record too large".to_owned()))?;
            out.extend_from_slice(&id_len.to_le_bytes());
            out.extend_from_slice(id.as_bytes());
            out.extend_from_slice(&record.nonce);
            out.extend_from_slice(&ct_len.to_le_bytes());
            out.extend_from_slice(&record.ciphertext);
        }
        let footer = seal(key, b"", &footer_aad(&out))?;
        out.extend_from_slice(&footer.nonce);
        out.extend_from_slice(&footer.ciphertext);
        Ok(out)
    }

    /// Parses a file image. Structure only: nothing is authenticated yet.
    fn decode(bytes: &[u8]) -> SecretStoreResult<Self> {
        let mut reader = Reader { bytes, pos: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(corrupt("not a Tan secret store file"));
        }
        let params = KdfParams {
            memory_kib: reader.u32()?,
            iterations: reader.u32()?,
            parallelism: reader.u32()?,
        };
        params.validate()?;
        let salt = reader.array::<SALT_LEN>()?;
        let verifier = Sealed {
            nonce: reader.array::<NONCE_LEN>()?,
            ciphertext: reader.take(VERIFIER_CT_LEN)?.to_vec(),
        };
        let count = reader.u32()?;
        let mut records = BTreeMap::new();
        for _ in 0..count {
            let id_len = usize::from(reader.u16()?);
            if id_len == 0 || id_len > MAX_ID_LEN {
                return Err(corrupt("invalid record id length"));
            }
            let id = std::str::from_utf8(reader.take(id_len)?)
                .map_err(|_| corrupt("record id is not valid text"))?
                .to_owned();
            let nonce = reader.array::<NONCE_LEN>()?;
            let ct_len = usize::try_from(reader.u32()?).map_err(|_| corrupt("record too large"))?;
            if ct_len < TAG_LEN {
                return Err(corrupt("record is too short"));
            }
            let ciphertext = reader.take(ct_len)?.to_vec();
            if records.insert(id, Sealed { nonce, ciphertext }).is_some() {
                return Err(corrupt("duplicate record id"));
            }
        }
        // The footer is verified by the caller; here it only has to be present and last.
        reader.take(FOOTER_LEN)?;
        if reader.pos != bytes.len() {
            return Err(corrupt("unexpected trailing data"));
        }
        Ok(Self {
            params,
            salt,
            verifier,
            records,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> SecretStoreResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| corrupt("the file is truncated"))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> SecretStoreResult<[u8; N]> {
        let mut out = [0_u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u16(&mut self) -> SecretStoreResult<u16> {
        Ok(u16::from_le_bytes(self.array::<2>()?))
    }

    fn u32(&mut self) -> SecretStoreResult<u32> {
        Ok(u32::from_le_bytes(self.array::<4>()?))
    }
}

/// Parses `bytes`, derives the key from `password` and authenticates the whole file.
fn open_file(
    bytes: &[u8],
    password: &SecretValue,
) -> SecretStoreResult<(Zeroizing<[u8; KEY_LEN]>, Contents)> {
    let contents = Contents::decode(bytes)?;
    let key = derive_key(password, contents.params, &contents.salt)?;
    let verified = open(&key, &contents.verifier, &contents.header_bytes())
        .is_ok_and(|plain| plain.as_slice() == VERIFIER_PLAINTEXT);
    if !verified {
        return Err(SecretStoreError::InvalidPassword);
    }
    let split = bytes.len() - FOOTER_LEN;
    let (body, footer) = bytes.split_at(split);
    let mut nonce = [0_u8; NONCE_LEN];
    nonce.copy_from_slice(&footer[..NONCE_LEN]);
    let footer = Sealed {
        nonce,
        ciphertext: footer[NONCE_LEN..].to_vec(),
    };
    if open(&key, &footer, &footer_aad(body)).is_err() {
        return Err(corrupt("the file failed its integrity check"));
    }
    Ok((key, contents))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> SecretStoreResult<()> {
    let io =
        |_: std::io::Error| SecretStoreError::Backend("could not write the secret file".to_owned());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| SecretStoreError::Backend("invalid secret file path".to_owned()))?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io)
}

impl EncryptedFileSecretStore {
    /// A store at `path` using the production [`KdfParams::DEFAULT`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_params(path, KdfParams::DEFAULT)
    }

    /// A store with explicit key-derivation costs for *new* files (existing files keep the
    /// parameters stored in them).
    #[must_use]
    pub fn with_params(path: impl Into<PathBuf>, params: KdfParams) -> Self {
        let path = path.into();
        let state = if path.exists() {
            State::Locked
        } else {
            State::Unconfigured
        };
        Self {
            path,
            params,
            state: Mutex::new(state),
        }
    }

    /// The file this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read_file(&self) -> SecretStoreResult<Vec<u8>> {
        fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SecretStoreError::NotConfigured
            } else {
                SecretStoreError::Backend("could not read the secret file".to_owned())
            }
        })
    }

    fn require_unlocked(state: &mut State) -> SecretStoreResult<(&[u8; KEY_LEN], &mut Contents)> {
        match state {
            State::Unconfigured => Err(SecretStoreError::NotConfigured),
            State::Locked => Err(SecretStoreError::Locked),
            State::Unlocked { key, contents } => Ok((&**key, &mut **contents)),
        }
    }

    fn put_value(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        let mut guard = self.state();
        let (key, contents) = Self::require_unlocked(&mut guard)?;
        let record = seal(key, value.expose().as_bytes(), &record_aad(id.as_str()))?;
        let mut next = contents.clone();
        next.records.insert(id.to_string(), record);
        write_atomic(&self.path, &next.encode(key)?)?;
        *contents = next;
        Ok(())
    }

    fn get_value(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        let mut guard = self.state();
        let (key, contents) = Self::require_unlocked(&mut guard)?;
        let Some(record) = contents.records.get(id.as_str()) else {
            return Ok(None);
        };
        let plain = open(key, record, &record_aad(id.as_str()))
            .map_err(|()| corrupt("a record failed authentication"))?;
        // `Zeroizing<Vec<u8>>` cannot be moved into a `String`; copy, then the buffer wipes
        // itself on drop.
        let text = std::str::from_utf8(&plain)
            .map_err(|_| corrupt("a record is not valid text"))?
            .to_owned();
        Ok(Some(SecretValue::new(text)))
    }

    fn delete_value(&self, id: &SecretId) -> SecretStoreResult<()> {
        let mut guard = self.state();
        let (key, contents) = Self::require_unlocked(&mut guard)?;
        if !contents.records.contains_key(id.as_str()) {
            return Ok(());
        }
        let mut next = contents.clone();
        next.records.remove(id.as_str());
        write_atomic(&self.path, &next.encode(key)?)?;
        *contents = next;
        Ok(())
    }
}

impl SecretStoreControl for EncryptedFileSecretStore {
    fn status(&self) -> SecretStoreStatus {
        let state = self.state();
        SecretStoreStatus {
            backend: SecretBackend::EncryptedFile,
            available: true,
            locked: !matches!(*state, State::Unlocked { .. }),
            master_password_set: !matches!(*state, State::Unconfigured),
        }
    }

    fn unlock(&self, password: &SecretValue) -> SecretStoreResult<()> {
        let mut state = self.state();
        if matches!(*state, State::Unconfigured) && !self.path.exists() {
            return Err(SecretStoreError::NotConfigured);
        }
        let bytes = self.read_file()?;
        let (key, contents) = open_file(&bytes, password)?;
        *state = State::Unlocked {
            key,
            contents: Box::new(contents),
        };
        Ok(())
    }

    fn set_master_password(
        &self,
        new_password: &SecretValue,
        current_password: Option<&SecretValue>,
    ) -> SecretStoreResult<()> {
        let mut state = self.state();
        let new_salt = random::<SALT_LEN>()?;

        let next = if matches!(*state, State::Unconfigured) && !self.path.exists() {
            let key = derive_key(new_password, self.params, &new_salt)?;
            let contents = Contents::new(&key, self.params, new_salt)?;
            (key, contents)
        } else {
            // Changing the password: the current one must be proven against the file itself.
            let current = current_password.ok_or(SecretStoreError::InvalidPassword)?;
            let bytes = self.read_file()?;
            let (old_key, old) = open_file(&bytes, current)?;
            let params = old.params;
            let key = derive_key(new_password, params, &new_salt)?;
            let mut contents = Contents::new(&key, params, new_salt)?;
            for (id, record) in &old.records {
                let aad = record_aad(id);
                let plain = open(&old_key, record, &aad)
                    .map_err(|()| corrupt("a record failed authentication"))?;
                contents
                    .records
                    .insert(id.clone(), seal(&key, &plain, &aad)?);
            }
            (key, contents)
        };

        let (key, contents) = next;
        write_atomic(&self.path, &contents.encode(&key)?)?;
        *state = State::Unlocked {
            key,
            contents: Box::new(contents),
        };
        Ok(())
    }

    fn lock(&self) {
        let mut state = self.state();
        if matches!(*state, State::Unlocked { .. }) {
            // Dropping the key wipes it (`Zeroizing`).
            *state = State::Locked;
        }
    }
}

impl SecretStore for EncryptedFileSecretStore {
    async fn put(&self, id: &SecretId, value: &SecretValue) -> SecretStoreResult<()> {
        self.put_value(id, value)
    }

    async fn get(&self, id: &SecretId) -> SecretStoreResult<Option<SecretValue>> {
        self.get_value(id)
    }

    async fn delete(&self, id: &SecretId) -> SecretStoreResult<()> {
        self.delete_value(id)
    }

    fn is_available(&self) -> bool {
        true
    }
}
