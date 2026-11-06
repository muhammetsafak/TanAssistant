//! The `DescribeFrames` step: one vision-model call per captured frame.
//!
//! Each frame is shown to the vision model together with what was being said around the
//! moment it was captured, so the description can say *what the screen was about*. The result
//! is stored on the frame as soon as it exists, which makes the step resumable frame by frame.

use serde::Deserialize;
use serde_json::json;
use tan_domain::{Frame, Session, TranscriptSegment};

use super::chunking::format_offset;
use super::synthesize::{language_name, render};
use super::{ProcessError, ProcessResult};
use crate::ports::{
    AiError, FrameRepository, IdGenerator, ImageInput, JsonSchema, VisionModel, VisionRequest,
};
use crate::sessions::SessionFileStore;

const SYSTEM: &str = include_str!("prompts/describe_frame.md");
const USER: &str = include_str!("prompts/describe_frame_user.md");

/// Transcript time before and after a frame that counts as its context.
const CONTEXT_WINDOW_MS: u64 = 20_000;
/// Most characters of transcript context given with a frame.
const MAX_CONTEXT_CHARS: usize = 800;
/// Reply bound: a description plus up to ~1,500 characters of on-screen text.
const MAX_REPLY_TOKENS: u32 = 900;
/// Longest stored description, in characters.
const MAX_DESCRIPTION_CHARS: usize = 2_500;

/// How a run of the step went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct DescribeOutcome {
    /// Frames that have a description after the run (including earlier runs').
    pub described: usize,
    /// Frames left without one: unreadable image file, or refused by the model.
    pub left_out: usize,
}

fn description_schema() -> JsonSchema {
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["description", "visible_text"],
        "properties": {
            "description": { "type": "string" },
            "visible_text": { "type": "string" }
        }
    });
    JsonSchema {
        name: "frame_description".to_owned(),
        schema_json: schema.to_string(),
    }
}

#[derive(Deserialize)]
struct RawDescription {
    description: String,
    #[serde(default)]
    visible_text: String,
}

fn mime_type_of(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        text.chars().take(max).collect()
    }
}

/// The stored form of a model reply: the description, then the on-screen text on its own line.
/// A reply that is not the JSON object is kept as the description as it is.
fn to_description(reply: &str) -> Option<String> {
    let trimmed = reply.trim();
    let json_part = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => &trimmed[start..=end],
        _ => trimmed,
    };
    let text = match serde_json::from_str::<RawDescription>(json_part) {
        Ok(raw) => {
            let description = raw.description.trim();
            let visible = raw.visible_text.trim();
            match (description.is_empty(), visible.is_empty()) {
                (true, true) => String::new(),
                (false, true) => description.to_owned(),
                (true, false) => format!("Text on screen: {visible}"),
                (false, false) => format!("{description}\nText on screen: {visible}"),
            }
        }
        Err(_) => trimmed.to_owned(),
    };
    (!text.is_empty()).then(|| truncate_chars(&text, MAX_DESCRIPTION_CHARS))
}

/// What was said around `frame`, as `speaker: text` lines.
fn context_for(frame: &Frame, transcript: &[TranscriptSegment]) -> String {
    let from = frame.timestamp_ms.saturating_sub(CONTEXT_WINDOW_MS);
    let to = frame.timestamp_ms.saturating_add(CONTEXT_WINDOW_MS);
    let mut context = String::new();
    for segment in transcript
        .iter()
        .filter(|s| s.end_ms >= from && s.start_ms <= to)
    {
        let line = format!(
            "{}: {}\n",
            segment.speaker.as_deref().unwrap_or("unknown"),
            segment
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
        if context.chars().count() + line.chars().count() > MAX_CONTEXT_CHARS {
            break;
        }
        context.push_str(&line);
    }
    if context.is_empty() {
        "(nobody was speaking)".to_owned()
    } else {
        context.trim_end().to_owned()
    }
}

/// Describes every frame of `frames` that has no description yet, storing each result at once.
///
/// A frame whose image cannot be read, or that the model refuses (`InvalidRequest`), is left
/// without a description and counted in [`DescribeOutcome::left_out`]; any other model error
/// stops the step so a retry can pick up from the next undescribed frame.
/// `on_progress` receives `0..=100`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn describe_frames<M, Fr, W>(
    model: &M,
    frame_repo: &Fr,
    files: &W,
    ids: &dyn IdGenerator,
    session: &Session,
    frames: &[Frame],
    transcript: &[TranscriptSegment],
    on_progress: &(dyn Fn(u8) + Send + Sync),
) -> ProcessResult<DescribeOutcome>
where
    M: VisionModel,
    Fr: FrameRepository,
    W: SessionFileStore,
{
    let language = &session.note_language;
    let system = render(
        SYSTEM,
        &[
            ("language", &language_name(language)),
            ("language_code", language.as_str()),
        ],
    );
    let total = frames.len();
    let mut outcome = DescribeOutcome::default();
    for (index, frame) in frames.iter().enumerate() {
        on_progress(u8::try_from(index * 100 / total.max(1)).unwrap_or(100));
        if frame.description.is_some() {
            outcome.described += 1;
            continue;
        }
        let Ok(data) = files.read_frame(&session.id, &frame.path) else {
            outcome.left_out += 1;
            continue;
        };
        let nonce = ids.new_id();
        let prompt = render(
            USER,
            &[
                ("timestamp", &format_offset(frame.timestamp_ms)),
                ("nonce", &nonce),
                ("context", &context_for(frame, transcript)),
            ],
        );
        let request = VisionRequest {
            system: Some(system.clone()),
            prompt,
            images: vec![ImageInput {
                mime_type: mime_type_of(&frame.path).to_owned(),
                data,
            }],
            temperature: Some(0.2),
            max_tokens: Some(MAX_REPLY_TOKENS),
            json_schema: Some(description_schema()),
        };
        match model.describe(request).await {
            Ok(reply) => match to_description(&reply.text) {
                Some(description) => {
                    frame_repo
                        .set_description(&frame.id, Some(&description))
                        .await?;
                    outcome.described += 1;
                }
                None => outcome.left_out += 1,
            },
            Err(AiError::InvalidRequest(_)) => outcome.left_out += 1,
            Err(error) => return Err(ProcessError::Provider(error)),
        }
    }
    on_progress(100);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use tan_domain::{FrameId, SessionId, TrackKind};

    use super::*;

    fn segment(start_ms: u64, end_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            session_id: SessionId::new("s"),
            track: TrackKind::System,
            start_ms,
            end_ms,
            language: None,
            text: text.to_owned(),
            speaker: Some("others".to_owned()),
        }
    }

    fn frame_at(ts: u64) -> Frame {
        Frame {
            id: FrameId::new("f"),
            session_id: SessionId::new("s"),
            timestamp_ms: ts,
            path: "frames/f.png".to_owned(),
            perceptual_hash: 0,
            description: None,
        }
    }

    #[test]
    fn replies_become_description_plus_screen_text() {
        assert_eq!(
            to_description(r#"{"description": "A pricing slide.", "visible_text": "Plan A  $10"}"#)
                .as_deref(),
            Some("A pricing slide.\nText on screen: Plan A  $10")
        );
        assert_eq!(
            to_description(r#"{"description": "A call grid.", "visible_text": ""}"#).as_deref(),
            Some("A call grid.")
        );
        assert_eq!(
            to_description("```json\n{\"description\": \"x\", \"visible_text\": \"\"}\n```")
                .as_deref(),
            Some("x")
        );
        // Not JSON: kept as is.
        assert_eq!(
            to_description("  A code editor.  ").as_deref(),
            Some("A code editor.")
        );
        assert_eq!(to_description("   "), None);
        assert_eq!(
            to_description(r#"{"description": "", "visible_text": ""}"#),
            None
        );
    }

    #[test]
    fn context_covers_the_window_around_the_frame_only() {
        let transcript = [
            segment(0, 5_000, "far before"),
            segment(70_000, 75_000, "just before"),
            segment(90_000, 95_000, "after"),
            segment(200_000, 205_000, "far after"),
        ];
        let context = context_for(&frame_at(80_000), &transcript);
        assert!(context.contains("just before") && context.contains("after"));
        assert!(!context.contains("far"));
        assert_eq!(
            context_for(&frame_at(500_000), &transcript),
            "(nobody was speaking)"
        );
    }

    #[test]
    fn mime_type_follows_the_extension() {
        assert_eq!(mime_type_of("frames/a.PNG"), "image/png");
        assert_eq!(mime_type_of("frames/a.jpeg"), "image/jpeg");
        assert_eq!(mime_type_of("frames/a.webp"), "image/webp");
        assert_eq!(mime_type_of("frames/a"), "image/png");
    }
}
