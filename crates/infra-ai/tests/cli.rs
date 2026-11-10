//! The CLI providers (ADR-013) against fake CLIs: shell scripts written to a temporary
//! directory that record how they were called and print what a real binary prints.
//!
//! The scripts run on Unix only (`sh`); the pure parsing and classification logic is unit
//! tested inside the modules on every platform. The tests at the bottom talk to the real,
//! installed and signed-in CLIs; they are `#[ignore]`d (run them with
//! `cargo test -p tan-infra-ai --test cli -- --ignored`).

mod common;

use std::time::Duration;

use tan_application::ports::{ChatModel, VisionModel};
use tan_infra_ai::claude_cli::ClaudeCliClient;
use tan_infra_ai::codex_cli::CodexCliClient;
use tan_infra_ai::CliSettings;

use common::{chat_request, vision_request};

#[cfg(unix)]
mod fake {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use serde_json::{json, Value};
    use tan_application::ports::{AiError, ChatRequest};

    use super::*;
    use common::{b64, schema, PNG};

    /// A fake CLI. It records its arguments (one per line), stdin and working directory in
    /// `dir`, then runs `body`.
    struct Fake {
        dir: tempfile::TempDir,
        binary: PathBuf,
    }

    impl Fake {
        fn new(body: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path().display();
            let script = format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > '{d}/argv'\n\
                 pwd > '{d}/cwd'\n\
                 cat > '{d}/stdin'\n\
                 {body}\n"
            );
            let binary = dir.path().join("fake-cli");
            std::fs::write(&binary, script).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, binary }
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn args(&self) -> Vec<String> {
            self.read("argv").lines().map(str::to_owned).collect()
        }

        fn claude(&self) -> ClaudeCliClient {
            ClaudeCliClient::with_binary(&self.binary, "sonnet", settings())
        }

        fn codex(&self) -> CodexCliClient {
            CodexCliClient::with_binary(&self.binary, "gpt-test", settings())
        }
    }

    fn settings() -> CliSettings {
        CliSettings {
            timeout: Duration::from_secs(30),
        }
    }

    /// Prints the given text through a quoted heredoc (no shell expansion).
    fn print(text: &str) -> String {
        format!("cat <<'TAN_EOF'\n{text}\nTAN_EOF")
    }

    fn lines(events: &[Value]) -> String {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn claude_success(text: &str) -> String {
        print(&lines(&[
            json!({ "type": "system", "subtype": "init", "tools": [] }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } }),
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": text,
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 3, "cache_creation_input_tokens": 10,
                               "cache_read_input_tokens": 0, "output_tokens": 4 } }),
        ]))
    }

    fn codex_success(text: &str) -> String {
        print(&lines(&[
            json!({ "type": "thread.started", "thread_id": "t" }),
            json!({ "type": "turn.started" }),
            json!({ "type": "item.completed", "item": { "id": "i", "type": "agent_message", "text": text } }),
            json!({ "type": "turn.completed",
                    "usage": { "input_tokens": 9, "cached_input_tokens": 0, "output_tokens": 2 } }),
        ]))
    }

    /// The scratch directory the call ran in is gone and is not where the tests run.
    fn assert_ran_in_a_removed_scratch_dir(fake: &Fake) {
        let cwd = PathBuf::from(fake.read("cwd").trim());
        assert!(cwd.is_absolute());
        assert!(
            !cwd.exists(),
            "the scratch directory is removed after the call"
        );
        assert!(!cwd.starts_with(std::env::current_dir().unwrap()));
    }

    // ---- Claude --------------------------------------------------------------------------

    #[tokio::test]
    async fn claude_chat_sends_the_prompt_on_stdin_only() {
        let fake = Fake::new(&claude_success("hello back"));
        let mut request = chat_request();
        request.system = Some("SYSTEM-SECRET-TEXT".to_owned());
        request.messages = vec![tan_application::ports::ChatMessage::user(
            "MEETING-CONTENT-XYZ",
        )];

        let reply = fake.claude().complete(request).await.unwrap();
        assert_eq!(reply.text, "hello back");
        assert!(!reply.truncated);
        let usage = reply.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (13, 4));

        let raw_argv = fake.read("argv");
        assert!(
            !raw_argv.contains("MEETING-CONTENT-XYZ"),
            "content in argv:\n{raw_argv}"
        );
        assert!(
            !raw_argv.contains("SYSTEM-SECRET-TEXT"),
            "system prompt in argv:\n{raw_argv}"
        );
        let args = fake.args();
        for expected in [
            "-p",
            "--input-format",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--tools",
        ] {
            assert!(
                args.iter().any(|a| a == expected),
                "missing {expected}: {args:?}"
            );
        }
        assert!(args.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));

        let message: Value = serde_json::from_str(fake.read("stdin").trim()).unwrap();
        assert_eq!(message["type"], "user");
        let text = message["message"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("MEETING-CONTENT-XYZ") && text.contains("SYSTEM-SECRET-TEXT"));
        assert_ran_in_a_removed_scratch_dir(&fake);
    }

    #[tokio::test]
    async fn claude_vision_sends_the_image_as_a_base64_block_on_stdin() {
        let fake = Fake::new(&claude_success("a slide"));
        let reply = fake.claude().describe(vision_request()).await.unwrap();
        assert_eq!(reply.text, "a slide");

        let message: Value = serde_json::from_str(fake.read("stdin").trim()).unwrap();
        let content = &message["message"]["content"];
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], b64(PNG));
        assert_eq!(content[1]["text"], "what is on the slide?");
        assert!(!fake.read("argv").contains(&b64(PNG)));
    }

    #[tokio::test]
    async fn claude_structured_output_uses_json_schema_and_returns_the_object() {
        let fake = Fake::new(&print(&lines(&[json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "{\"summary\": \"ok\"}",
            "structured_output": { "summary": "ok" },
            "usage": { "input_tokens": 1, "output_tokens": 1 },
        })])));
        let mut request: ChatRequest = chat_request();
        request.json_schema = Some(schema());
        let reply = fake.claude().complete(request).await.unwrap();
        assert_eq!(reply.text, r#"{"summary":"ok"}"#);

        let args = fake.args();
        let at = args.iter().position(|a| a == "--json-schema").unwrap();
        let sent: Value = serde_json::from_str(&args[at + 1]).unwrap();
        assert_eq!(sent["required"][0], "summary");
    }

    #[tokio::test]
    async fn claude_not_logged_in_is_an_auth_error() {
        let fake = Fake::new(&format!(
            "{}\nexit 1",
            print(&lines(&[
                json!({ "type": "system", "subtype": "init" }),
                json!({ "type": "assistant", "error": "authentication_failed",
                        "message": { "content": [{ "type": "text", "text": "Not logged in · Please run /login" }] } }),
                json!({ "type": "result", "subtype": "success", "is_error": true,
                        "result": "Not logged in · Please run /login" }),
            ]))
        ));
        let error = fake.claude().complete(chat_request()).await.unwrap_err();
        assert_eq!(error, AiError::Auth);
    }

    #[tokio::test]
    async fn claude_usage_limit_is_rate_limited() {
        let fake = Fake::new(&format!(
            "{}\nexit 1",
            print(&lines(&[
                json!({ "type": "result", "subtype": "success", "is_error": true,
                                   "result": "Claude AI usage limit reached|1760000000" })
            ]))
        ));
        let error = fake.claude().complete(chat_request()).await.unwrap_err();
        assert_eq!(error, AiError::RateLimited { retry_after: None });
    }

    #[tokio::test]
    async fn garbage_output_and_failing_exits_are_provider_errors() {
        let garbage = Fake::new("echo 'this is not json at all'");
        let error = garbage.claude().complete(chat_request()).await.unwrap_err();
        assert!(
            matches!(error, AiError::Provider { status: 0, .. }),
            "{error:?}"
        );
        let error = garbage.codex().complete(chat_request()).await.unwrap_err();
        assert!(
            matches!(error, AiError::Provider { status: 0, .. }),
            "{error:?}"
        );

        let crashing = Fake::new("echo 'boom sk-live-abcdefghij1234 happened' >&2\nexit 7");
        let error = crashing
            .claude()
            .complete(chat_request())
            .await
            .unwrap_err();
        let AiError::Provider { status, message } = error else {
            panic!("expected a provider error");
        };
        assert_eq!(status, 7);
        assert!(
            message.contains("boom") && !message.contains("abcdefghij1234"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_missing_binary_is_cli_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-cli");
        let claude = ClaudeCliClient::with_binary(&missing, "sonnet", settings());
        let error = claude.complete(chat_request()).await.unwrap_err();
        assert!(matches!(error, AiError::CliUnavailable(_)), "{error:?}");
        assert!(error
            .to_string()
            .starts_with("the CLI is not installed or could not be started"));
        let codex = CodexCliClient::with_binary(&missing, "gpt-test", settings());
        assert!(matches!(
            codex.describe(vision_request()).await.unwrap_err(),
            AiError::CliUnavailable(_)
        ));
        // A file that is not executable cannot be started either.
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "not a program").unwrap();
        let claude = ClaudeCliClient::with_binary(&plain, "sonnet", settings());
        assert!(matches!(
            claude.complete(chat_request()).await.unwrap_err(),
            AiError::CliUnavailable(_)
        ));
    }

    #[tokio::test]
    async fn a_call_that_outlives_the_timeout_is_killed() {
        let fake = Fake::new(&format!("echo $$ > '{}'\nexec sleep 60", "PIDFILE"));
        // The pid file lives next to the script; patch the placeholder with its real path.
        let script = std::fs::read_to_string(&fake.binary)
            .unwrap()
            .replace("PIDFILE", &fake.path("pid").display().to_string());
        std::fs::write(&fake.binary, script).unwrap();

        let client = ClaudeCliClient::with_binary(
            &fake.binary,
            "sonnet",
            CliSettings {
                timeout: Duration::from_millis(500),
            },
        );
        let started = std::time::Instant::now();
        let error = client.complete(chat_request()).await.unwrap_err();
        assert_eq!(error, AiError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(20));

        let pid = fake.read("pid").trim().to_owned();
        assert!(!pid.is_empty(), "the fake CLI never started");
        let mut gone = false;
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "process {pid} is still running after the timeout");
    }

    #[tokio::test]
    async fn a_timeout_also_takes_down_what_the_cli_started() {
        let fake = Fake::new("sleep 60 &\necho $! > 'GPIDFILE'\nexec sleep 60");
        let script = std::fs::read_to_string(&fake.binary)
            .unwrap()
            .replace("GPIDFILE", &fake.path("child-pid").display().to_string());
        std::fs::write(&fake.binary, script).unwrap();
        let client = ClaudeCliClient::with_binary(
            &fake.binary,
            "sonnet",
            CliSettings {
                timeout: Duration::from_millis(500),
            },
        );
        assert_eq!(
            client.complete(chat_request()).await.unwrap_err(),
            AiError::Timeout
        );
        let pid = fake.read("child-pid").trim().to_owned();
        assert!(!pid.is_empty(), "the fake CLI never started its child");
        let mut gone = false;
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "child {pid} of the CLI survived the timeout");
    }

    #[tokio::test]
    async fn dropping_a_call_also_takes_down_what_the_cli_started() {
        let fake = Fake::new("sleep 60 &\necho $! > 'GPIDFILE'\nexec sleep 60");
        let script = std::fs::read_to_string(&fake.binary)
            .unwrap()
            .replace("GPIDFILE", &fake.path("child-pid").display().to_string());
        std::fs::write(&fake.binary, script).unwrap();
        // A long CLI timeout: it is the caller that gives up (a cancelled job).
        let client = ClaudeCliClient::with_binary(&fake.binary, "sonnet", settings());
        let cancelled =
            tokio::time::timeout(Duration::from_millis(700), client.complete(chat_request())).await;
        assert!(
            cancelled.is_err(),
            "the call should still have been running"
        );
        let pid = fake.read("child-pid").trim().to_owned();
        assert!(!pid.is_empty(), "the fake CLI never started its child");
        let mut gone = false;
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "child {pid} of the CLI survived the cancelled call");
    }

    #[tokio::test]
    async fn a_bad_model_name_never_reaches_the_command_line() {
        let fake = Fake::new(&claude_success("x"));
        let client = ClaudeCliClient::with_binary(&fake.binary, "--dangerous", settings());
        assert!(matches!(
            client.complete(chat_request()).await.unwrap_err(),
            AiError::InvalidRequest(_)
        ));
        assert!(
            fake.read("argv").is_empty(),
            "the CLI must not have been started"
        );
    }

    // ---- Codex ---------------------------------------------------------------------------

    #[tokio::test]
    async fn codex_chat_reads_the_prompt_from_stdin() {
        let fake = Fake::new(&codex_success("codex says hi"));
        let mut request = chat_request();
        request.system = Some("SYSTEM-SECRET-TEXT".to_owned());
        request.messages = vec![tan_application::ports::ChatMessage::user(
            "MEETING-CONTENT-XYZ",
        )];

        let reply = fake.codex().complete(request).await.unwrap();
        assert_eq!(reply.text, "codex says hi");
        let usage = reply.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (9, 2));

        let raw_argv = fake.read("argv");
        assert!(
            !raw_argv.contains("MEETING-CONTENT-XYZ") && !raw_argv.contains("SYSTEM-SECRET-TEXT")
        );
        let args = fake.args();
        assert_eq!(&args[..2], ["exec", "-"]);
        assert!(args.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(args.windows(2).any(|w| w == ["-m", "gpt-test"]));
        assert!(args.iter().any(|a| a == "--json"));
        let stdin = fake.read("stdin");
        assert!(stdin.contains("MEETING-CONTENT-XYZ") && stdin.contains("SYSTEM-SECRET-TEXT"));
        assert_ran_in_a_removed_scratch_dir(&fake);
    }

    #[tokio::test]
    async fn codex_vision_passes_the_images_as_files() {
        // Copies every `--image` argument while the scratch directory still exists.
        let body = format!(
            "n=0\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --image ]; then cp \"$2\" \"{dir}/copy-$n\"; n=$((n+1)); fi\n  shift\ndone\n{ok}",
            dir = "COPYDIR",
            ok = codex_success("a slide")
        );
        let fake = Fake::new(&body);
        let script = std::fs::read_to_string(&fake.binary)
            .unwrap()
            .replace("COPYDIR", &fake.dir.path().display().to_string());
        std::fs::write(&fake.binary, script).unwrap();

        let mut request = vision_request();
        request.images.push(request.images[0].clone());
        let reply = fake.codex().describe(request).await.unwrap();
        assert_eq!(reply.text, "a slide");
        assert_eq!(std::fs::read(fake.path("copy-0")).unwrap(), PNG);
        assert_eq!(std::fs::read(fake.path("copy-1")).unwrap(), PNG);
        // The image bytes are in files, never in argv or on stdin.
        assert!(!fake.read("argv").contains(&b64(PNG)));
        assert!(fake.read("stdin").contains("what is on the slide?"));
        let args = fake.args();
        let first = args.iter().position(|a| a == "--image").unwrap();
        assert!(args[first..]
            .iter()
            .all(|a| a == "--image" || a.contains("image-")));
    }

    #[tokio::test]
    async fn codex_structured_output_writes_the_schema_to_a_file() {
        let body = format!(
            "while [ $# -gt 0 ]; do\n  if [ \"$1\" = --output-schema ]; then cp \"$2\" 'COPYDIR/schema-copy'; fi\n  shift\ndone\n{}",
            codex_success("{\"summary\":\"ok\"}")
        );
        let fake = Fake::new(&body);
        let script = std::fs::read_to_string(&fake.binary)
            .unwrap()
            .replace("COPYDIR", &fake.dir.path().display().to_string());
        std::fs::write(&fake.binary, script).unwrap();

        let mut request = chat_request();
        request.json_schema = Some(schema());
        let reply = fake.codex().complete(request).await.unwrap();
        assert_eq!(reply.text, r#"{"summary":"ok"}"#);
        let sent: Value = serde_json::from_str(&fake.read("schema-copy")).unwrap();
        assert_eq!(sent["additionalProperties"], false);
    }

    #[tokio::test]
    async fn codex_not_logged_in_is_an_auth_error_and_a_limit_is_rate_limited() {
        let fake = Fake::new("echo 'Not logged in. Run `codex login`.' >&2\nexit 1");
        assert_eq!(
            fake.codex().complete(chat_request()).await.unwrap_err(),
            AiError::Auth
        );

        let fake = Fake::new(&format!(
            "{}\nexit 1",
            print(&lines(&[
                json!({ "type": "turn.started" }),
                json!({ "type": "turn.failed", "error": { "message": "You've hit your usage limit. Try again later." } }),
            ]))
        ));
        assert_eq!(
            fake.codex().complete(chat_request()).await.unwrap_err(),
            AiError::RateLimited { retry_after: None }
        );
    }

    #[tokio::test]
    async fn codex_retries_without_flags_an_older_version_does_not_know() {
        // Refuses `--ephemeral` the way clap does, but only when it is given.
        let body = format!(
            "for a in \"$@\"; do\n  if [ \"$a\" = --ephemeral ]; then\n    echo \"error: unexpected argument '--ephemeral' found\" >&2\n    exit 2\n  fi\ndone\n{}",
            codex_success("fine")
        );
        let fake = Fake::new(&body);
        let reply = fake.codex().complete(chat_request()).await.unwrap();
        assert_eq!(reply.text, "fine");
        // The recorded call is the second one.
        let args = fake.args();
        assert!(!args
            .iter()
            .any(|a| a == "--ephemeral" || a == "--ignore-user-config"));
        assert!(args.iter().any(|a| a == "--json"));
    }

    #[tokio::test]
    async fn codex_timeout_is_reported() {
        let fake = Fake::new("exec sleep 60");
        let client = CodexCliClient::with_binary(
            &fake.binary,
            "gpt-test",
            CliSettings {
                timeout: Duration::from_millis(300),
            },
        );
        assert_eq!(
            client.complete(chat_request()).await.unwrap_err(),
            AiError::Timeout
        );
    }
}

// ---- real CLIs (need them installed and signed in) -------------------------------------

#[tokio::test]
#[ignore = "needs the real `claude` CLI, signed in"]
async fn real_claude_answers() {
    let client = ClaudeCliClient::new("sonnet", CliSettings::default());
    let mut request = chat_request();
    request.messages = vec![tan_application::ports::ChatMessage::user(
        "Reply with the single word: pong",
    )];
    let reply = client.complete(request).await.expect("claude chat");
    assert!(reply.text.to_lowercase().contains("pong"), "{}", reply.text);
}

/// A real 32x32 solid red PNG.
const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

fn red_image_request() -> tan_application::ports::VisionRequest {
    use base64::Engine as _;
    let mut request = vision_request();
    request.images[0].data = base64::engine::general_purpose::STANDARD
        .decode(RED_PNG_BASE64)
        .expect("valid base64");
    "What is the main colour of this image? Answer with one word.".clone_into(&mut request.prompt);
    request
}

#[tokio::test]
#[ignore = "needs the real `claude` CLI, signed in"]
async fn real_claude_sees_an_image() {
    let client = ClaudeCliClient::new("sonnet", CliSettings::default());
    let reply = client
        .describe(red_image_request())
        .await
        .expect("claude vision");
    assert!(reply.text.to_lowercase().contains("red"), "{}", reply.text);
}

#[tokio::test]
#[ignore = "needs the real `claude` CLI, signed in"]
async fn real_claude_structured_output_follows_the_schema() {
    let client = ClaudeCliClient::new("sonnet", CliSettings::default());
    let mut request = red_image_request();
    "What is the main colour of this image?".clone_into(&mut request.prompt);
    request.json_schema = Some(tan_application::ports::JsonSchema {
        name: "colour".to_owned(),
        schema_json: r#"{"type":"object","properties":{"colour":{"type":"string"}},"required":["colour"],"additionalProperties":false}"#
            .to_owned(),
    });
    let reply = client.describe(request).await.expect("claude structured");
    let parsed: serde_json::Value = serde_json::from_str(&reply.text).expect("JSON reply");
    let colour = parsed["colour"].as_str().expect("a colour string");
    assert!(colour.to_lowercase().contains("red"), "{colour}");
}

#[tokio::test]
#[ignore = "needs the real `codex` CLI, signed in"]
async fn real_codex_answers() {
    let client = CodexCliClient::new("gpt-6.1-sol", CliSettings::default());
    let mut request = chat_request();
    request.messages = vec![tan_application::ports::ChatMessage::user(
        "Reply with the single word: pong",
    )];
    let reply = client.complete(request).await.expect("codex chat");
    assert!(reply.text.to_lowercase().contains("pong"), "{}", reply.text);
}
