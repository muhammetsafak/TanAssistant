//! Claude Code CLI (`claude`) as a provider (ADR-013): chat and vision on the user's Claude
//! subscription.
//!
//! Verified against `claude` 2.1.322 (the oldest version tested; older ones were not tried).
//! The call is
//!
//! ```text
//! claude -p --output-format stream-json --verbose --input-format stream-json
//!        --model <m> --system-prompt <fixed text> --tools "" --strict-mcp-config
//!        --no-session-persistence --disable-slash-commands --setting-sources project
//!        [--json-schema <schema>]
//! ```
//!
//! with one `user` message on stdin. Differences from the obvious design, all observed on the
//! real binary:
//!
//! * `--input-format stream-json` is only accepted together with `--output-format stream-json`
//!   (plus `--verbose`), so the reply is read from the final `{"type":"result"}` event instead
//!   of the single object `--output-format json` would print.
//! * `--setting-sources project` keeps the user's global `CLAUDE.md`, hooks and settings out of
//!   the call (it cut the prompt from about 8.9k to 0.6k tokens); the sign-in is unaffected.
//!   The project settings are those of the empty scratch directory, i.e. none.
//! * `--tools ""` leaves no tools, yet `--json-schema` still works (the reply arrives in the
//!   result's `structured_output`).
//! * There is no flag for the reply length or the temperature: both are ignored.
//!
//! The fixed `--system-prompt` is the only prompt text on the command line; the request's own
//! system prompt and all content go through stdin.

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::{json, Value};
use tan_application::ports::{
    AiError, AiResult, ChatModel, ChatRequest, ChatResponse, TokenUsage, VisionModel, VisionRequest,
};

use crate::cli::{
    classify_text, compose_prompt, failure, json_lines, unreadable, validate_model, CliRunner,
    CliSettings, FailureKind, Workdir, IMAGE_TYPES,
};
use crate::http::{base64_encode, parse_schema};

/// The binary's name.
pub const BINARY: &str = "claude";

/// Replaces Claude Code's own (coding-agent) system prompt. Fixed text: the request's system
/// prompt travels over stdin.
const SYSTEM_PROMPT: &str = "You are a text-processing engine inside a note-taking application. \
Follow the instructions in the user's message and reply with the answer only. You have no tools \
and cannot read files or browse; work only from the text and images in the message.";

/// Adapter for the Claude Code CLI.
#[derive(Debug)]
pub struct ClaudeCliClient {
    runner: CliRunner,
    model: String,
}

impl ClaudeCliClient {
    /// An adapter that finds `claude` on this machine.
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

    async fn run(&self, content: Vec<Value>, schema: Option<Value>) -> AiResult<ChatResponse> {
        validate_model(&self.model)?;
        let args = build_args(&self.model, schema.as_ref());
        let message = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
        });
        let mut stdin = serde_json::to_vec(&message).map_err(|e| AiError::Provider {
            status: 0,
            message: format!("could not encode the request: {e}"),
        })?;
        stdin.push(b'\n');

        let workdir = Workdir::new()?;
        let output = self.runner.run(&workdir, &args, stdin).await?;
        parse_output(&output.stdout, output.code, &output.stderr)
    }
}

impl ChatModel for ClaudeCliClient {
    async fn complete(&self, request: ChatRequest) -> AiResult<ChatResponse> {
        let schema = request.json_schema.as_ref().map(parse_schema).transpose()?;
        let text = compose_prompt(request.system.as_deref(), &request.messages);
        self.run(vec![json!({ "type": "text", "text": text })], schema)
            .await
    }
}

impl VisionModel for ClaudeCliClient {
    async fn describe(&self, request: VisionRequest) -> AiResult<ChatResponse> {
        if request.images.is_empty() {
            return Err(AiError::InvalidRequest("no image was given".to_owned()));
        }
        let schema = request.json_schema.as_ref().map(parse_schema).transpose()?;
        // Images first, the instruction after them.
        let mut content = Vec::with_capacity(request.images.len() + 1);
        for image in &request.images {
            if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
                return Err(AiError::InvalidRequest(format!(
                    "unsupported image type {}",
                    image.mime_type
                )));
            }
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": base64_encode(&image.data),
                },
            }));
        }
        let mut text = String::new();
        if let Some(system) = request
            .system
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            text.push_str("<instructions>\n");
            text.push_str(system);
            text.push_str("\n</instructions>\n\n");
        }
        text.push_str(&request.prompt);
        content.push(json!({ "type": "text", "text": text }));
        self.run(content, schema).await
    }
}

/// The command line. Nothing from the request except the model name and the (application
/// defined) schema.
fn build_args(model: &str, schema: Option<&Value>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--model",
        model,
        "--system-prompt",
        SYSTEM_PROMPT,
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--disable-slash-commands",
        "--setting-sources",
        "project",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if let Some(schema) = schema {
        args.push("--json-schema".into());
        args.push(schema.to_string().into());
    }
    args
}

/// Reads the event stream of a finished call.
fn parse_output(stdout: &[u8], code: Option<i32>, stderr: &str) -> AiResult<ChatResponse> {
    let mut result: Option<Value> = None;
    let mut assistant_error: Option<String> = None;
    for event in json_lines(stdout) {
        match event["type"].as_str() {
            Some("result") => result = Some(event),
            Some("assistant") => {
                if let Some(error) = event["error"].as_str() {
                    assistant_error = Some(error.to_owned());
                }
            }
            _ => {}
        }
    }
    let Some(result) = result else {
        // No result event: the process died or printed something else. Its stderr (or the
        // assistant error) says why.
        let complaint = assistant_error.as_deref().unwrap_or(stderr);
        return Err(match classify_text(complaint) {
            FailureKind::Other => unreadable(code, stderr),
            kind => failure(kind, code, complaint),
        });
    };

    let text = result["result"].as_str().unwrap_or_default();
    let subtype_ok = result["subtype"].as_str().is_none_or(|s| s == "success");
    if result["is_error"].as_bool().unwrap_or(false) || !subtype_ok {
        return Err(result_error(
            &result,
            assistant_error.as_deref(),
            code,
            text,
        ));
    }

    let reply = match &result["structured_output"] {
        Value::Null => text.to_owned(),
        structured => structured.to_string(),
    };
    Ok(ChatResponse {
        text: reply,
        usage: usage_of(&result["usage"]),
        truncated: result["stop_reason"].as_str() == Some("max_tokens"),
    })
}

/// The error for a result event that reports failure.
fn result_error(
    result: &Value,
    assistant_error: Option<&str>,
    code: Option<i32>,
    text: &str,
) -> AiError {
    let http_status = result["api_error_status"].as_u64();
    let kind = match (assistant_error, http_status) {
        (Some("authentication_failed"), _) | (_, Some(401 | 403)) => FailureKind::Auth,
        (Some("rate_limit" | "billing_error"), _) | (_, Some(429)) => FailureKind::RateLimited,
        _ => classify_text(text),
    };
    let described = if text.is_empty() {
        result["subtype"].as_str().unwrap_or_default()
    } else {
        text
    };
    failure(kind, code, described)
}

/// Input tokens count the cached ones too: they are all part of the prompt.
fn usage_of(usage: &Value) -> Option<TokenUsage> {
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let output_tokens = usage["output_tokens"].as_u64()?;
    Some(TokenUsage {
        input_tokens: count("input_tokens")
            + count("cache_creation_input_tokens")
            + count("cache_read_input_tokens"),
        output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(events: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for event in events {
            out.extend_from_slice(event.to_string().as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn init() -> Value {
        json!({ "type": "system", "subtype": "init", "tools": [] })
    }

    #[test]
    fn reads_text_and_usage_from_the_result_event() {
        let out = lines(&[
            init(),
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "pong" }] } }),
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": "pong",
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 2, "cache_creation_input_tokens": 600,
                               "cache_read_input_tokens": 5, "output_tokens": 4 } }),
        ]);
        let reply = parse_output(&out, Some(0), "").unwrap();
        assert_eq!(reply.text, "pong");
        assert!(!reply.truncated);
        assert_eq!(
            reply.usage,
            Some(TokenUsage {
                input_tokens: 607,
                output_tokens: 4
            })
        );
    }

    #[test]
    fn structured_output_wins_over_the_result_text() {
        let out = lines(&[json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "{\"color\": \"red\"}",
            "structured_output": { "color": "red" },
            "usage": { "input_tokens": 1, "output_tokens": 1 },
        })]);
        let reply = parse_output(&out, Some(0), "").unwrap();
        assert_eq!(reply.text, r#"{"color":"red"}"#);
    }

    #[test]
    fn a_cut_off_reply_is_marked() {
        let out = lines(&[
            json!({ "type": "result", "subtype": "success", "is_error": false,
                                  "result": "abc", "stop_reason": "max_tokens" }),
        ]);
        let reply = parse_output(&out, Some(0), "").unwrap();
        assert!(reply.truncated);
        assert_eq!(reply.usage, None);
    }

    #[test]
    fn not_logged_in_is_an_auth_error() {
        // As printed by the real binary with an empty config directory.
        let out = lines(&[
            init(),
            json!({ "type": "assistant", "error": "authentication_failed",
                    "message": { "content": [{ "type": "text", "text": "Not logged in · Please run /login" }] } }),
            json!({ "type": "result", "subtype": "success", "is_error": true,
                    "result": "Not logged in · Please run /login", "api_error_status": null }),
        ]);
        assert_eq!(parse_output(&out, Some(1), ""), Err(AiError::Auth));
    }

    #[test]
    fn limits_and_http_statuses_map_to_their_errors() {
        let limited = lines(&[json!({ "type": "result", "is_error": true,
                                      "result": "Claude AI usage limit reached|1760000000" })]);
        assert_eq!(
            parse_output(&limited, Some(1), ""),
            Err(AiError::RateLimited { retry_after: None })
        );
        let by_status = lines(&[json!({ "type": "result", "is_error": true,
                                        "result": "API Error", "api_error_status": 429 })]);
        assert_eq!(
            parse_output(&by_status, Some(1), ""),
            Err(AiError::RateLimited { retry_after: None })
        );
        let forbidden = lines(&[json!({ "type": "result", "is_error": true,
                                        "result": "API Error", "api_error_status": 403 })]);
        assert_eq!(parse_output(&forbidden, Some(1), ""), Err(AiError::Auth));
    }

    #[test]
    fn other_failures_and_garbage_are_provider_errors() {
        let failed = lines(&[
            json!({ "type": "result", "subtype": "error_during_execution",
                                     "is_error": true, "result": "" }),
        ]);
        assert!(matches!(
            parse_output(&failed, Some(1), ""),
            Err(AiError::Provider { status: 1, message }) if message == "error_during_execution"
        ));
        assert!(matches!(
            parse_output(b"this is not json\n", Some(0), ""),
            Err(AiError::Provider { status: 0, .. })
        ));
        assert!(matches!(
            parse_output(b"", Some(2), "boom: it broke"),
            Err(AiError::Provider { status: 2, message }) if message.contains("it broke")
        ));
        assert_eq!(
            parse_output(b"", Some(1), "Not logged in"),
            Err(AiError::Auth)
        );
    }

    #[test]
    fn the_command_line_carries_no_request_content() {
        let schema = json!({ "type": "object" });
        let args = build_args("sonnet", Some(&schema));
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(text.contains(&"--tools".to_owned()));
        assert!(text.contains(&String::new()), "the empty tool list");
        assert!(text.contains(&"--no-session-persistence".to_owned()));
        assert!(text.contains(&"--strict-mcp-config".to_owned()));
        assert_eq!(
            text.last().map(String::as_str),
            Some(r#"{"type":"object"}"#)
        );
        assert_eq!(build_args("sonnet", None).len(), args.len() - 2);
    }
}
