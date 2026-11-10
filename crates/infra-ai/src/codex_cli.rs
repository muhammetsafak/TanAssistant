//! `OpenAI` Codex CLI (`codex`) as a provider (ADR-013): chat and vision on the user's `ChatGPT`
//! subscription.
//!
//! **Not verified against a real binary.** `codex` was not installed on the machine this
//! adapter was written on, so the flags and the event format below come from the upstream
//! source (`codex-rs/exec/src/cli.rs` and `exec_events.rs`, read on 2026-11-10), not from a
//! run. The ignored tests in `tests/cli.rs` check the real binary once it is installed.
//!
//! The call is
//!
//! ```text
//! codex exec - --json --skip-git-repo-check --sandbox read-only --ephemeral
//!       --ignore-user-config --color never -m <model>
//!       [--output-schema <file>] [--image <file>]...
//! ```
//!
//! The prompt (`-`) comes from stdin. `--image` is a comma-separated list (upstream:
//! `num_args = 1`, `value_delimiter = ','`), so the flag is repeated once per file and the
//! files get fixed names in the scratch directory; a comma in that directory's path (a custom
//! `TMPDIR` on Linux) would split a path, so a call with images is refused there. Images and
//! the schema are written to the scratch directory the process runs in. `--ephemeral` (no
//! session files on disk) and `--ignore-user-config` (no MCP servers or instructions from the
//! user's `config.toml`; the sign-in still comes from `CODEX_HOME`) are newer flags: when the
//! installed version does not know them the call is repeated once without them.
//!
//! The reply is the last `agent_message` item of the `--json` event stream; token usage comes
//! from `turn.completed`. There is no flag for the reply length or the temperature: both are
//! ignored.
//!
//! Safety: `--sandbox read-only` stops writes and network access but not reading local files
//! or running read-only commands, and Codex has no documented switch to turn its shell tool off
//! (checked upstream on 2026-11-10; not confirmed on a real binary). The only guard against a
//! prompt injection in the meeting content is the instruction in `PREAMBLE`. On timeout the
//! whole process group is killed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, TokenUsage, VisionModel, VisionRequest,
};

use crate::cli::{
    classify_text, compose_prompt, failure, image_extension, json_lines, unreadable,
    validate_model, CliRunner, CliSettings, FailureKind, Workdir, IMAGE_TYPES,
};
use crate::http::parse_schema;

/// The binary's name.
pub const BINARY: &str = "codex";

/// Codex is an agent: tell it plainly that this is a text task.
const PREAMBLE: &str = "This is a plain text task. Do not run commands, read files or edit \
anything; answer only from the text and images given here, and reply with the answer only.";

/// The flags that older versions do not know.
const NEWER_FLAGS: [&str; 2] = ["--ephemeral", "--ignore-user-config"];

/// Adapter for the Codex CLI.
#[derive(Debug)]
pub struct CodexCliClient {
    runner: CliRunner,
    model: String,
}

impl CodexCliClient {
    /// An adapter that finds `codex` on this machine.
    #[must_use]
    pub fn new(model: impl Into<String>, settings: CliSettings) -> Self {
        Self {
            runner: CliRunner::detect(BINARY, settings),
            model: model.into(),
        }
    }

    /// An adapter for the binary at `path` (tests, or a user-chosen binary).
    #[must_use]
    pub fn with_binary(
        path: impl Into<PathBuf>,
        model: impl Into<String>,
        settings: CliSettings,
    ) -> Self {
        Self {
            runner: CliRunner::at(path, settings),
            model: model.into(),
        }
    }

    async fn run(
        &self,
        prompt: String,
        images: &[(&str, &[u8])],
        schema: Option<Value>,
    ) -> AiResult<ChatResponse> {
        validate_model(&self.model)?;
        let workdir = Workdir::new()?;
        let schema_file = match &schema {
            Some(schema) => {
                Some(write_file(&workdir, "schema.json", schema.to_string().as_bytes()).await?)
            }
            None => None,
        };
        if !images.is_empty() && workdir.path().to_string_lossy().contains(',') {
            return Err(AiError::Unsupported(
                "the temporary directory path contains a comma, which `codex --image` splits on"
                    .to_owned(),
            ));
        }
        let mut image_files = Vec::with_capacity(images.len());
        for (index, (mime, data)) in images.iter().enumerate() {
            let name = format!("image-{index}.{}", image_extension(mime)?);
            image_files.push(write_file(&workdir, &name, data).await?);
        }

        let stdin = prompt.into_bytes();
        let mut args = build_args(&self.model, schema_file.as_deref(), &image_files, true);
        let mut output = self.runner.run(&workdir, &args, stdin.clone()).await?;
        if !output.success() && rejects_a_newer_flag(&output.stderr) {
            args = build_args(&self.model, schema_file.as_deref(), &image_files, false);
            output = self.runner.run(&workdir, &args, stdin).await?;
        }
        parse_output(&output.stdout, output.code, &output.stderr)
    }
}

async fn write_file(workdir: &Workdir, name: &str, data: &[u8]) -> AiResult<PathBuf> {
    let path = workdir.path().join(name);
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| AiError::Provider {
            status: 0,
            message: format!("could not write to the scratch directory: {e}"),
        })?;
    Ok(path)
}

impl ChatModel for CodexCliClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let schema = request.json_schema.as_ref().map(parse_schema).transpose()?;
        let prompt = compose_prompt(
            Some(&with_preamble(request.system.as_deref())),
            &request.messages,
        );
        self.run(prompt, &[], schema).await
    }
}

impl VisionModel for CodexCliClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        if request.images.is_empty() {
            return Err(AiError::InvalidRequest("no image was given".to_owned()));
        }
        for image in &request.images {
            if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
                return Err(AiError::InvalidRequest(format!(
                    "unsupported image type {}",
                    image.mime_type
                )));
            }
        }
        let schema = request.json_schema.as_ref().map(parse_schema).transpose()?;
        let prompt = compose_prompt(
            Some(&with_preamble(request.system.as_deref())),
            &[tan_application::ports::ChatMessage::user(request.prompt)],
        );
        let images: Vec<(&str, &[u8])> = request
            .images
            .iter()
            .map(|i| (i.mime_type.as_str(), i.data.as_slice()))
            .collect();
        self.run(prompt, &images, schema).await
    }
}

fn with_preamble(system: Option<&str>) -> String {
    match system.map(str::trim).filter(|s| !s.is_empty()) {
        Some(system) => format!("{PREAMBLE}\n\n{system}"),
        None => PREAMBLE.to_owned(),
    }
}

/// The command line. Nothing from the request except the model name and file paths of our own
/// scratch directory.
fn build_args(
    model: &str,
    schema_file: Option<&Path>,
    images: &[PathBuf],
    newer_flags: bool,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "-",
        "--json",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--color",
        "never",
        "-m",
        model,
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if newer_flags {
        args.extend(NEWER_FLAGS.iter().map(OsString::from));
    }
    if let Some(schema_file) = schema_file {
        args.push("--output-schema".into());
        args.push(schema_file.into());
    }
    for image in images {
        args.push("--image".into());
        args.push(image.into());
    }
    args
}

/// Whether clap refused one of [`NEWER_FLAGS`].
fn rejects_a_newer_flag(stderr: &str) -> bool {
    stderr.contains("unexpected argument") && NEWER_FLAGS.iter().any(|flag| stderr.contains(flag))
}

/// Reads the `--json` event stream of a finished call.
fn parse_output(stdout: &[u8], code: Option<i32>, stderr: &str) -> AiResult<ChatResponse> {
    let mut reply: Option<String> = None;
    let mut usage: Option<TokenUsage> = None;
    let mut turn_failed: Option<String> = None;
    let mut last_error: Option<String> = None;
    for event in json_lines(stdout) {
        match event["type"].as_str() {
            Some("item.completed") => {
                if event["item"]["type"].as_str() == Some("agent_message") {
                    if let Some(text) = event["item"]["text"].as_str() {
                        reply = Some(text.to_owned());
                    }
                }
            }
            Some("turn.completed") => usage = usage_of(&event["usage"]),
            Some("turn.failed") => {
                turn_failed = Some(
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            // Errors can be transient (a reconnect notice) and the turn still succeed, so they
            // only count when the process fails too.
            Some("error") => {
                last_error = Some(event["message"].as_str().unwrap_or_default().to_owned());
            }
            _ => {}
        }
    }

    let complaint = if let Some(message) = turn_failed {
        Some(message)
    } else if code == Some(0) {
        None
    } else {
        Some(last_error.unwrap_or_else(|| stderr.to_owned()))
    };
    if let Some(complaint) = complaint {
        // The wording may sit in the event or on stderr.
        let kind = match classify_text(&complaint) {
            FailureKind::Other => classify_text(stderr),
            kind => kind,
        };
        let text = if complaint.trim().is_empty() {
            stderr
        } else {
            &complaint
        };
        return Err(failure(kind, code, text));
    }
    match reply {
        Some(text) => Ok(ChatResponse {
            text,
            usage,
            truncated: false,
        }),
        None => Err(unreadable(code, stderr)),
    }
}

fn usage_of(usage: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: usage["input_tokens"].as_u64()?,
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(events: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for event in events {
            out.extend_from_slice(event.to_string().as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn message(text: &str) -> Value {
        json!({ "type": "item.completed", "item": { "id": "item_0", "type": "agent_message", "text": text } })
    }

    #[test]
    fn reads_the_last_agent_message_and_the_usage() {
        let out = lines(&[
            json!({ "type": "thread.started", "thread_id": "t" }),
            json!({ "type": "turn.started" }),
            json!({ "type": "item.completed", "item": { "id": "r", "type": "reasoning", "text": "thinking" } }),
            message("first"),
            message("pong"),
            json!({ "type": "turn.completed",
                    "usage": { "input_tokens": 120, "cached_input_tokens": 100, "output_tokens": 7 } }),
        ]);
        let reply = parse_output(&out, Some(0), "").unwrap();
        assert_eq!(reply.text, "pong");
        assert_eq!(
            reply.usage,
            Some(TokenUsage {
                input_tokens: 120,
                output_tokens: 7
            })
        );
    }

    #[test]
    fn a_transient_error_event_does_not_fail_a_good_turn() {
        let out = lines(&[
            json!({ "type": "error", "message": "Reconnecting... 1/5" }),
            message("ok"),
            json!({ "type": "turn.completed", "usage": { "input_tokens": 1, "output_tokens": 1 } }),
        ]);
        assert_eq!(parse_output(&out, Some(0), "").unwrap().text, "ok");
    }

    #[test]
    fn failed_turns_map_to_auth_limit_or_provider_errors() {
        let failed = |message: &str| {
            lines(&[json!({ "type": "turn.failed", "error": { "message": message } })])
        };
        assert_eq!(
            parse_output(&failed("unexpected status 401 Unauthorized"), Some(1), ""),
            Err(AiError::Auth)
        );
        assert_eq!(
            parse_output(&failed("You've hit your usage limit."), Some(1), ""),
            Err(AiError::RateLimited { retry_after: None })
        );
        assert!(matches!(
            parse_output(&failed("model exploded"), Some(1), ""),
            Err(AiError::Provider { status: 1, message }) if message == "model exploded"
        ));
        // Not signed in is announced on stderr and the stream stays empty.
        assert_eq!(
            parse_output(b"", Some(1), "Not logged in. Run `codex login`."),
            Err(AiError::Auth)
        );
        // A non-zero exit with a good-looking reply is still a failure.
        let out = lines(&[message("half")]);
        assert!(matches!(
            parse_output(&out, Some(3), "crashed"),
            Err(AiError::Provider { status: 3, .. })
        ));
    }

    #[test]
    fn no_reply_or_garbage_is_a_provider_error() {
        assert!(matches!(
            parse_output(b"hello\n", Some(0), ""),
            Err(AiError::Provider { status: 0, .. })
        ));
        let out = lines(&[
            json!({ "type": "turn.completed", "usage": { "input_tokens": 1, "output_tokens": 0 } }),
        ]);
        assert!(matches!(
            parse_output(&out, Some(0), ""),
            Err(AiError::Provider { .. })
        ));
    }

    #[test]
    fn the_prompt_is_read_from_stdin_and_images_come_last() {
        let args = build_args(
            "gpt-x",
            Some(Path::new("/tmp/w/schema.json")),
            &[
                PathBuf::from("/tmp/w/image-0.png"),
                PathBuf::from("/tmp/w/image-1.png"),
            ],
            true,
        );
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&text[..2], ["exec", "-"]);
        assert!(text.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(text.windows(2).any(|w| w == ["-m", "gpt-x"]));
        assert!(text
            .windows(2)
            .any(|w| w == ["--output-schema", "/tmp/w/schema.json"]));
        let first_image = text.iter().position(|a| a == "--image").unwrap();
        assert!(text[first_image..]
            .iter()
            .all(|a| a == "--image" || a.starts_with("/tmp/w/image-")));
        assert!(text.contains(&"--ephemeral".to_owned()));
        assert!(!build_args("gpt-x", None, &[], false)
            .iter()
            .any(|a| a == "--ephemeral"));
    }

    #[test]
    fn only_a_refused_newer_flag_triggers_the_retry() {
        assert!(rejects_a_newer_flag(
            "error: unexpected argument '--ephemeral' found"
        ));
        assert!(!rejects_a_newer_flag(
            "error: unexpected argument '--bogus' found"
        ));
        assert!(!rejects_a_newer_flag(
            "--ephemeral was fine, the network was not"
        ));
    }
}
