//! Shared runner for the CLI providers (ADR-013): finds the installed binary, runs it as a child
//! process in an empty scratch directory and turns failures into [`AiError`]s.
//!
//! The application never reads or uses the CLI's OAuth token; it only starts the official
//! binary, which signs in on its own. Prompts and content travel over **stdin**, never argv
//! (argv is visible to every process of the machine and has a length limit). The process
//! environment is inherited as it is; the application puts none of its own secrets into it.
//!
//! On timeout the CLI and, on Unix, its whole process group are killed.
//!
//! Nothing here logs a prompt or a reply.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tan_application::ports::{AiError, AiResult, ChatMessage, ChatRole};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::http::redact;

/// How long one CLI call may run before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// How long the login-shell lookup may take.
const SHELL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest provider message that is passed on (characters).
const MAX_MESSAGE_CHARS: usize = 500;

/// Image types both CLIs accept.
pub(crate) const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Settings of the CLI providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliSettings {
    /// A call that runs longer is killed and reported as [`AiError::Timeout`].
    pub timeout: Duration,
}

impl Default for CliSettings {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

// ---- binary detection ------------------------------------------------------------------

/// Where the environment lets us look for a binary. A struct (instead of reading the process
/// environment inside) so the lookup can be tested with a minimal `PATH`.
#[derive(Debug, Clone, Default)]
pub struct LookupEnv {
    /// The `PATH` variable.
    pub path: Option<OsString>,
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// `%APPDATA%` (Windows).
    pub app_data: Option<PathBuf>,
    /// The user's login shell (`$SHELL`, Unix only).
    pub shell: Option<PathBuf>,
}

impl LookupEnv {
    /// The environment of this process.
    #[must_use]
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        Self {
            path: std::env::var_os("PATH"),
            home,
            app_data: std::env::var_os("APPDATA").map(PathBuf::from),
            shell: if cfg!(unix) {
                std::env::var_os("SHELL").map(PathBuf::from)
            } else {
                None
            },
        }
    }

    /// The directories CLI installers use, in lookup order. A GUI application started from
    /// Finder or the Start menu has a narrow `PATH` that lacks most of them.
    fn known_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(home) = &self.home {
            dirs.push(home.join(".local").join("bin"));
            dirs.push(home.join(".claude").join("local"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        if let Some(home) = &self.home {
            dirs.push(home.join(".npm-global").join("bin"));
        }
        if let Some(app_data) = &self.app_data {
            dirs.push(app_data.join("npm"));
        }
        dirs
    }
}

/// File names a binary called `name` can have on this platform.
fn candidate_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_owned()]
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_in_dir(dir: &Path, names: &[String]) -> Option<PathBuf> {
    names
        .iter()
        .map(|n| dir.join(n))
        .find(|candidate| is_executable_file(candidate))
}

/// `PATH` first, then the known install directories. No process is started.
#[must_use]
pub fn find_in_environment(name: &str, env: &LookupEnv) -> Option<PathBuf> {
    let names = candidate_names(name);
    let from_path = env
        .path
        .as_deref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| find_in_dir(&dir, &names));
    from_path.or_else(|| {
        env.known_dirs()
            .iter()
            .find_map(|dir| find_in_dir(dir, &names))
    })
}

/// On Windows a console program started from a GUI process gets a console window of its own,
/// so every CLI call would flash a terminal in front of the user. `CREATE_NO_WINDOW` runs it
/// without one; stdin/stdout/stderr are pipes, so nothing is lost. A no-op elsewhere.
fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        /// `CREATE_NO_WINDOW` from `winbase.h`.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Asks the login shell where `name` is (`$SHELL -lc 'command -v <name>'`): an application
/// opened from Finder does not get the `PATH` of the user's shell profile.
async fn find_via_login_shell(name: &str, shell: &Path) -> Option<PathBuf> {
    // `name` is one of our own constants; this guard keeps it that way.
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut command = Command::new(shell);
    hide_console_window(&mut command);
    command
        .arg("-lc")
        .arg(format!("command -v {name}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(SHELL_LOOKUP_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // A profile script may print its own lines before the answer: take the last absolute path
    // that really is an executable file.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .map(PathBuf::from)
        .find(|path| path.is_absolute() && is_executable_file(path))
}

/// Looks `name` up in `env` (see [`find_in_environment`]) and, on Unix, in the login shell.
pub async fn locate_with(name: &str, env: &LookupEnv) -> Option<PathBuf> {
    if let Some(found) = find_in_environment(name, env) {
        return Some(found);
    }
    match &env.shell {
        Some(shell) => find_via_login_shell(name, shell).await,
        None => None,
    }
}

fn cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn forget(name: &str) {
    if let Ok(mut map) = cache().lock() {
        map.remove(name);
    }
}

/// Finds the binary `name` on this machine. A hit is cached for the life of the process (and
/// dropped again when starting it fails); a miss is looked up again next time, because the
/// user may install the CLI while the application runs.
pub async fn locate(name: &str) -> Option<PathBuf> {
    let cached = cache().lock().ok().and_then(|map| map.get(name).cloned());
    if let Some(path) = cached {
        if is_executable_file(&path) {
            return Some(path);
        }
        forget(name);
    }
    let found = locate_with(name, &LookupEnv::from_process()).await?;
    if let Ok(mut map) = cache().lock() {
        map.insert(name.to_owned(), found.clone());
    }
    Some(found)
}

// ---- running ---------------------------------------------------------------------------

/// The binary a runner starts.
#[derive(Debug, Clone)]
enum Binary {
    /// Found by name at call time.
    Detect(&'static str),
    /// A fixed path (tests, or a future "choose the binary" setting).
    Path(PathBuf),
}

/// An empty scratch directory a call runs in; removed on drop. The CLI never sees the
/// application's working directory, and files the adapter needs (images, a schema) go here.
#[derive(Debug)]
pub struct Workdir(TempDir);

impl Workdir {
    /// Creates a new empty directory.
    pub fn new() -> AiResult<Self> {
        tempfile::Builder::new()
            .prefix("tan-cli-")
            .tempdir()
            .map(Self)
            .map_err(|e| AiError::Provider {
                status: 0,
                message: format!("could not create a scratch directory: {e}"),
            })
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

/// What a finished CLI call left behind.
#[derive(Debug)]
pub struct CliOutput {
    /// Everything the CLI wrote to stdout.
    pub stdout: Vec<u8>,
    /// Everything the CLI wrote to stderr (lossy UTF-8).
    pub stderr: String,
    /// The exit code; `None` when a signal ended the process.
    pub code: Option<i32>,
}

impl CliOutput {
    /// The process ended with code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Starts one CLI binary.
#[derive(Debug, Clone)]
pub struct CliRunner {
    binary: Binary,
    name: String,
    settings: CliSettings,
}

impl CliRunner {
    /// A runner that finds `name` on this machine on every call (cached after the first hit).
    #[must_use]
    pub fn detect(name: &'static str, settings: CliSettings) -> Self {
        Self {
            binary: Binary::Detect(name),
            name: name.to_owned(),
            settings,
        }
    }

    /// A runner for the binary at `path`.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>, settings: CliSettings) -> Self {
        let path = path.into();
        let name = path
            .file_stem()
            .map_or_else(|| "cli".to_owned(), |s| s.to_string_lossy().into_owned());
        Self {
            binary: Binary::Path(path),
            name,
            settings,
        }
    }

    async fn resolve(&self) -> AiResult<PathBuf> {
        match &self.binary {
            Binary::Path(path) => Ok(path.clone()),
            Binary::Detect(name) => locate(name).await.ok_or_else(|| {
                AiError::CliUnavailable(format!(
                    "`{name}` was not found on PATH or in the usual install locations"
                ))
            }),
        }
    }

    /// Runs the CLI with `args` in `workdir`, feeding it `stdin`, and waits for it to exit.
    ///
    /// Errors: [`AiError::CliUnavailable`] when the binary is missing or cannot be started,
    /// [`AiError::Timeout`] when the call outlives [`CliSettings::timeout`] (the process is
    /// killed). A non-zero exit is *not* an error here: the adapter reads the output first,
    /// because the reason is often in stdout.
    pub async fn run(
        &self,
        workdir: &Workdir,
        args: &[OsString],
        stdin: Vec<u8>,
    ) -> AiResult<CliOutput> {
        let path = self.resolve().await?;
        let mut child = spawn(&path, args, workdir.path()).await.map_err(|error| {
            if let Binary::Detect(name) = &self.binary {
                forget(name);
            }
            AiError::CliUnavailable(format!("`{}` could not be started: {error}", self.name))
        })?;
        tracing::debug!(cli = %self.name, "CLI process started");
        // Ends the whole process group if this call is cancelled, dropped or times out.
        let mut group = GroupGuard::new(child.id());

        let mut pipe = child.stdin.take();
        let feed = async move {
            if let Some(pipe) = pipe.as_mut() {
                // The CLI may exit before it has read everything (a bad flag, a failed login):
                // a broken pipe is not our error, its exit status will tell.
                let _ = pipe.write_all(&stdin).await;
                let _ = pipe.shutdown().await;
            }
            drop(pipe);
        };
        let wait = async {
            // Dropping this future (on timeout) drops the child, which kills it.
            let ((), output) = tokio::join!(feed, child.wait_with_output());
            output
        };
        let output = match tokio::time::timeout(self.settings.timeout, wait).await {
            Err(_elapsed) => {
                tracing::warn!(cli = %self.name, "CLI call timed out; the process was killed");
                return Err(AiError::Timeout);
            }
            Ok(Err(error)) => {
                return Err(AiError::CliUnavailable(format!(
                    "`{}` could not be waited for: {error}",
                    self.name
                )))
            }
            Ok(Ok(output)) => {
                // The CLI is gone and reaped: its group number may be reused by now, so there
                // is nothing left to kill by it.
                group.disarm();
                output
            }
        };
        tracing::debug!(cli = %self.name, code = ?output.status.code(), "CLI process finished");
        Ok(CliOutput {
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: output.status.code(),
        })
    }
}

/// Starts the process. A binary that was just replaced (the CLIs update themselves) is
/// briefly "text file busy"; that is retried a few times.
async fn spawn(
    path: &Path,
    args: &[OsString],
    workdir: &Path,
) -> std::io::Result<tokio::process::Child> {
    let mut attempt = 0;
    loop {
        let mut command = Command::new(path);
        hide_console_window(&mut command);
        // Its own process group, so a timeout can take down what the CLI started as well.
        #[cfg(unix)]
        command.process_group(0);
        command
            .args(args)
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match command.spawn() {
            Err(error) if error.kind() == ErrorKind::ExecutableFileBusy && attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
            }
            other => return other,
        }
    }
}

/// Kills the process group a CLI was started in when dropped while armed: its children (e.g. a
/// shell command an agent ran) would outlive `kill_on_drop`, which ends the CLI itself only.
/// Armed from the start of a call until the CLI has exited, so a timeout, a cancelled job and
/// an application shutdown are all covered. Best effort: the group may already be gone. On
/// other platforms than Unix only the CLI process itself is ended.
struct GroupGuard {
    pid: Option<u32>,
}

impl GroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // A few milliseconds of blocking, only on the cancel path; no `unsafe` needed.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--"])
                .arg(format!("-{pid}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        // Elsewhere `kill_on_drop` ends the CLI itself; the pid is not needed.
        #[cfg(not(unix))]
        let _ = self.pid;
    }
}

// ---- shared request helpers ------------------------------------------------------------

/// The model name goes to the CLI as an argument value, so it must not be able to pass for a
/// flag or carry anything but a model name.
pub(crate) fn validate_model(model: &str) -> AiResult<()> {
    let ok = !model.is_empty()
        && model.len() <= 128
        && model.starts_with(|c: char| c.is_ascii_alphanumeric())
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '[' | ']' | '@')
        });
    if ok {
        Ok(())
    } else {
        Err(AiError::InvalidRequest(
            "the model name contains characters a model name does not have".to_owned(),
        ))
    }
}

/// The instruction text as one stdin document: the system prompt (if any) in an
/// `<instructions>` block, then the conversation. A single user turn is passed through bare.
pub(crate) fn compose_prompt(system: Option<&str>, messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    if let Some(system) = system.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("<instructions>\n");
        out.push_str(system);
        out.push_str("\n</instructions>\n\n");
    }
    match messages {
        [only] if only.role == ChatRole::User => out.push_str(&only.content),
        _ => {
            for (index, message) in messages.iter().enumerate() {
                if index > 0 {
                    out.push_str("\n\n");
                }
                out.push_str(match message.role {
                    ChatRole::User => "User:\n",
                    ChatRole::Assistant => "Assistant:\n",
                });
                out.push_str(&message.content);
            }
        }
    }
    out
}

/// File extension for an image MIME type.
pub(crate) fn image_extension(mime: &str) -> AiResult<&'static str> {
    match mime {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        "image/webp" => Ok("webp"),
        "image/gif" => Ok("gif"),
        other => Err(AiError::InvalidRequest(format!(
            "unsupported image type {other}"
        ))),
    }
}

// ---- failure classification ------------------------------------------------------------

/// What a CLI's complaint amounts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// Not signed in / not allowed.
    Auth,
    /// Quota or limit used up.
    RateLimited,
    /// Anything else.
    Other,
}

/// Whether `text` contains `number` as a word of its own: `429` yes, `14290` no, and no inside
/// an identifier either (`req_5f3a401c-...`, `a-429-b`): a letter, digit, `-` or `_` next to it
/// means it is part of something else.
fn has_number(text: &str, number: &str) -> bool {
    let part_of_a_word = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(number).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + number.len()..].chars().next();
        !before.is_some_and(part_of_a_word) && !after.is_some_and(part_of_a_word)
    })
}

/// Wording of a used-up quota or limit (lower case).
const RATE: &[&str] = &[
    "rate limit",
    "rate_limit",
    "usage limit",
    "limit reached",
    "limit exceeded",
    "hit your limit",
    "quota",
    "too many requests",
    "credit balance",
];

/// Wording of a missing or refused sign-in (lower case).
const AUTH: &[&str] = &[
    "not logged in",
    "not signed in",
    "please run /login",
    "run `codex login`",
    "codex login",
    "log in again",
    "please log in",
    "please sign in",
    "unauthorized",
    "invalid api key",
    "authentication fail",
    "authentication_fail",
    "authentication_error",
    "authentication required",
    "login required",
];

/// Classifies a CLI's error text from the wording both CLIs (and the APIs behind them) use.
/// Rate limits are checked first: a "usage limit" message may also mention the plan or login.
pub(crate) fn classify_text(text: &str) -> FailureKind {
    let lower = text.to_ascii_lowercase();
    if RATE.iter().any(|p| lower.contains(p)) || has_number(&lower, "429") {
        FailureKind::RateLimited
    } else if AUTH.iter().any(|p| lower.contains(p))
        || has_number(&lower, "401")
        || has_number(&lower, "403")
    {
        FailureKind::Auth
    } else {
        FailureKind::Other
    }
}

/// The text shown for a failure: trimmed, shortened and scrubbed of anything key-like.
pub(crate) fn clean_message(text: &str) -> String {
    let trimmed = text.trim();
    let short: String = trimmed.chars().take(MAX_MESSAGE_CHARS).collect();
    let mut message = redact(&short, None);
    if trimmed.chars().count() > MAX_MESSAGE_CHARS {
        message.push_str("...");
    }
    message
}

/// The [`AiError`] for a failed call whose complaint is `text` and whose exit code is `code`.
pub(crate) fn failure(kind: FailureKind, code: Option<i32>, text: &str) -> AiError {
    match kind {
        FailureKind::Auth => AiError::Auth,
        FailureKind::RateLimited => AiError::RateLimited { retry_after: None },
        FailureKind::Other => {
            let message = clean_message(text);
            // `status` carries an exit code only while it cannot be taken for an HTTP status
            // (below 100); `127`, `137`, `255`... go into the message instead.
            let status = code
                .filter(|c| (1..100).contains(c))
                .and_then(|c| u16::try_from(c).ok())
                .unwrap_or(0);
            let large_code = code.filter(|c| *c >= 100 || *c < 0);
            let message = match (message.is_empty(), code) {
                (true, Some(c)) => format!("the CLI exited with code {c}"),
                (true, None) => "the CLI was ended by a signal".to_owned(),
                (false, _) => match large_code {
                    Some(c) => format!("exit code {c}: {message}"),
                    None => message,
                },
            };
            AiError::Provider { status, message }
        }
    }
}

/// The error for output that could not be understood.
pub(crate) fn unreadable(code: Option<i32>, stderr: &str) -> AiError {
    let detail = clean_message(stderr);
    failure(
        FailureKind::Other,
        code,
        &if detail.is_empty() {
            "the CLI printed no result".to_owned()
        } else {
            detail
        },
    )
}

/// Parses `bytes` as JSON lines, skipping blank and non-JSON lines (a CLI may print a banner
/// or a warning before its events).
pub(crate) fn json_lines(bytes: &[u8]) -> impl Iterator<Item = serde_json::Value> + '_ {
    bytes
        .split(|b| *b == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(serde_json::Value::is_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_login_and_limit_wording() {
        assert_eq!(
            classify_text("Not logged in · Please run /login"),
            FailureKind::Auth
        );
        assert_eq!(
            classify_text("unexpected status 401 Unauthorized: Missing bearer"),
            FailureKind::Auth
        );
        assert_eq!(
            classify_text("You've hit your usage limit. Try again later."),
            FailureKind::RateLimited
        );
        assert_eq!(
            classify_text("Claude AI usage limit reached|1760000000"),
            FailureKind::RateLimited
        );
        assert_eq!(classify_text("stream error: 429"), FailureKind::RateLimited);
        assert_eq!(classify_text("error code 14290"), FailureKind::Other);
        // Numbers inside identifiers are not status codes.
        for text in [
            "stream error, request id: 5f3a401c-7d2e-4b1a-9c3d-0123456789ab",
            "request id 9b0e429a-1111-2222-3333-444455556666 failed",
            "req_403_abc broke",
            "trace a-401-b",
        ] {
            assert_eq!(classify_text(text), FailureKind::Other, "{text}");
        }
        assert_eq!(classify_text("HTTP 403 Forbidden"), FailureKind::Auth);
        assert_eq!(classify_text("status: 429,"), FailureKind::RateLimited);
        assert_eq!(
            classify_text("authentication failed for this account"),
            FailureKind::Auth
        );
        assert_eq!(
            classify_text("the authentication module crashed"),
            FailureKind::Other
        );
        assert_eq!(classify_text("segmentation fault"), FailureKind::Other);
    }

    #[test]
    fn failure_messages_are_short_and_scrubbed() {
        let long = format!("boom sk-live-abcdefghij1234 {}", "x".repeat(900));
        let AiError::Provider { status, message } = failure(FailureKind::Other, Some(3), &long)
        else {
            panic!("expected a provider error");
        };
        assert_eq!(status, 3);
        assert!(!message.contains("abcdefghij1234"));
        assert!(message.chars().count() <= MAX_MESSAGE_CHARS + 3);
        assert!(message.ends_with("..."));
        // Exit codes that could pass for an HTTP status stay in the message.
        assert_eq!(
            failure(FailureKind::Other, Some(127), "sh: codex: not found"),
            AiError::Provider {
                status: 0,
                message: "exit code 127: sh: codex: not found".to_owned()
            }
        );
        assert_eq!(
            failure(FailureKind::Other, Some(255), ""),
            AiError::Provider {
                status: 0,
                message: "the CLI exited with code 255".to_owned()
            }
        );
        assert_eq!(
            failure(FailureKind::Other, None, "  "),
            AiError::Provider {
                status: 0,
                message: "the CLI was ended by a signal".to_owned()
            }
        );
    }

    #[test]
    fn model_names_cannot_pass_for_flags() {
        for ok in [
            "sonnet",
            "claude-sonnet-5-5",
            "gpt-6.1-sol",
            "sonnet[1m]",
            "a/b:c",
        ] {
            assert!(validate_model(ok).is_ok(), "{ok}");
        }
        for bad in ["", "--help", "-m", "a b", "a;b", "a\nb", "$(x)"] {
            assert!(validate_model(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn prompts_are_composed_without_losing_the_system_text() {
        assert_eq!(compose_prompt(None, &[ChatMessage::user("hi")]), "hi");
        let text = compose_prompt(Some(" be brief "), &[ChatMessage::user("hi")]);
        assert_eq!(text, "<instructions>\nbe brief\n</instructions>\n\nhi");
        let two = compose_prompt(
            None,
            &[
                ChatMessage::user("a"),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: "b".to_owned(),
                },
                ChatMessage::user("c"),
            ],
        );
        assert_eq!(two, "User:\na\n\nAssistant:\nb\n\nUser:\nc");
    }

    #[test]
    fn json_lines_skip_noise() {
        let bytes = b"warning: something\n\n{\"a\":1}\n[1]\n{\"b\":2}\r\n";
        let all: Vec<_> = json_lines(bytes).collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1]["b"], 2);
    }

    #[cfg(unix)]
    mod lookup {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        fn install(dir: &Path, name: &str, mode: u32) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        fn run<F: std::future::Future>(future: F) -> F::Output {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(future)
        }

        #[test]
        fn finds_a_binary_on_path() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("bin");
            let wanted = install(&bin, "tan-fake-cli", 0o755);
            let env = LookupEnv {
                path: Some(std::env::join_paths([Path::new("/nonexistent"), &bin]).unwrap()),
                ..LookupEnv::default()
            };
            assert_eq!(find_in_environment("tan-fake-cli", &env), Some(wanted));
        }

        #[test]
        fn a_minimal_path_still_finds_the_binary_in_a_known_directory() {
            let home = tempfile::tempdir().unwrap();
            let wanted = install(
                &home.path().join(".local").join("bin"),
                "tan-fake-cli",
                0o755,
            );
            let env = LookupEnv {
                // What a macOS GUI application gets.
                path: Some(OsString::from("/usr/bin:/bin")),
                home: Some(home.path().to_path_buf()),
                ..LookupEnv::default()
            };
            assert_eq!(find_in_environment("tan-fake-cli", &env), Some(wanted));
            assert!(run(locate_with("tan-fake-cli", &env)).is_some());
            // Not installed anywhere: not found, and no shell to ask.
            assert_eq!(run(locate_with("tan-no-such-cli", &env)), None);
        }

        #[test]
        fn skips_files_that_are_not_executable_and_relative_path_entries() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("bin");
            install(&bin, "tan-fake-cli", 0o644);
            let env = LookupEnv {
                path: Some(std::env::join_paths([bin.as_path(), Path::new("relative")]).unwrap()),
                ..LookupEnv::default()
            };
            assert_eq!(find_in_environment("tan-fake-cli", &env), None);
        }

        #[test]
        fn falls_back_to_the_login_shell() {
            let tmp = tempfile::tempdir().unwrap();
            let wanted = install(&tmp.path().join("odd place"), "tan-fake-cli", 0o755);
            // A "shell" that ignores its arguments and answers like `command -v` would, after
            // some profile noise.
            let shell = tmp.path().join("fake-shell");
            std::fs::write(
                &shell,
                format!(
                    "#!/bin/sh\necho 'welcome back'\necho '{}'\n",
                    wanted.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
            let env = LookupEnv {
                path: Some(OsString::from("/usr/bin:/bin")),
                shell: Some(shell),
                ..LookupEnv::default()
            };
            assert_eq!(run(locate_with("tan-fake-cli", &env)), Some(wanted));
        }
    }
}
