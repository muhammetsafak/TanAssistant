//! The `Synthesize` step: transcript + frame descriptions -> a validated note.
//!
//! A meeting that fits the model's budget is summarised in one call. A longer one is first cut
//! into chunks ([`split_into_chunks`]); each chunk gets working notes (map), the working notes
//! are merged until they fit one call, and the final call turns them into the structured note
//! (reduce). The final reply is validated; an invalid reply gets exactly one repair request.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;
use tan_domain::{EpochMs, Frame, FrameId, LanguageCode, Session, TranscriptSegment};

use super::chunking::{
    build_timeline, format_offset, frame_alias, join_entries, parse_iso_date, span_of,
    split_into_chunks, utc_date_string, TimelineEntry, TokenBudget,
};
use super::{ProcessError, ProcessResult};
use crate::ports::{
    AiError, ChatMessage, ChatModel, ChatRequest, ChatResponse, ChatRole, IdGenerator, JsonSchema,
};

const SYNTHESIZE_SYSTEM: &str = include_str!("prompts/synthesize.md");
const SYNTHESIZE_USER: &str = include_str!("prompts/synthesize_user.md");
const SYNTHESIZE_USER_PARTS: &str = include_str!("prompts/synthesize_user_parts.md");
const MAP_SYSTEM: &str = include_str!("prompts/map_chunk.md");
const MAP_USER: &str = include_str!("prompts/chunk_user.md");
const MERGE_SYSTEM: &str = include_str!("prompts/merge_partials.md");
const MERGE_USER: &str = include_str!("prompts/merge_user.md");
const REPAIR_USER: &str = include_str!("prompts/repair.md");

/// Merge rounds before giving up on shrinking the working notes further.
const MAX_MERGE_ROUNDS: usize = 4;
/// Longest problem description quoted back to the model in a repair request, in characters.
const MAX_PROBLEM_CHARS: usize = 300;
/// Largest reply a repair request asks for when the first reply was cut off by the length
/// limit (twice the normal bound, but never more than this).
const MAX_REPAIR_REPLY_TOKENS: u32 = 16_384;
/// Sampling temperature: notes should be faithful, not creative.
const TEMPERATURE: f32 = 0.2;

/// Replaces every `{{key}}` in `template` with its value. Values are substituted verbatim and
/// are never re-scanned, so content cannot inject a placeholder.
pub(super) fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        if let Some((_, value)) = values.iter().find(|(k, _)| *k == key) {
            out.push_str(value);
        } else {
            out.push_str("{{");
            out.push_str(key);
            out.push_str("}}");
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The English name of a language code for use in prompts (the code itself when unknown).
pub(super) fn language_name(code: &LanguageCode) -> String {
    let primary = code.as_str().split('-').next().unwrap_or("");
    match primary {
        "tr" => "Turkish",
        "en" => "English",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "pl" => "Polish",
        "ar" => "Arabic",
        "fa" => "Persian",
        "hi" => "Hindi",
        "ja" => "Japanese",
        "ko" => "Korean",
        "zh" => "Chinese",
        "az" => "Azerbaijani",
        "sv" => "Swedish",
        _ => return code.as_str().to_owned(),
    }
    .to_owned()
}

/// What the step works from.
pub(super) struct SynthesisInput<'a> {
    /// The session (title, date, note language).
    pub session: &'a Session,
    /// The stored transcript.
    pub transcript: &'a [TranscriptSegment],
    /// The frames that have a description, in time order.
    pub frames: &'a [Frame],
}

/// An action item as the model reported it, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActionItemDraft {
    pub task: String,
    pub owner: Option<String>,
    pub due: Option<EpochMs>,
}

/// A validated note, not yet stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NoteDraft {
    pub summary: String,
    pub decisions: Vec<String>,
    pub action_items: Vec<ActionItemDraft>,
    pub open_questions: Vec<String>,
    pub key_frames: Vec<FrameId>,
}

/// The JSON Schema of the note: strict-compatible (every property required, nothing extra);
/// optional values are nullable.
fn note_schema() -> JsonSchema {
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "decisions", "action_items", "open_questions", "key_frames"],
        "properties": {
            "summary": { "type": "string" },
            "decisions": { "type": "array", "items": { "type": "string" } },
            "action_items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task", "owner", "due"],
                    "properties": {
                        "task": { "type": "string" },
                        "owner": { "type": ["string", "null"] },
                        "due": { "type": ["string", "null"] }
                    }
                }
            },
            "open_questions": { "type": "array", "items": { "type": "string" } },
            "key_frames": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["frame_id", "reason"],
                    "properties": {
                        "frame_id": { "type": "string" },
                        "reason": { "type": "string" }
                    }
                }
            }
        }
    });
    JsonSchema {
        name: "meeting_note".to_owned(),
        schema_json: schema.to_string(),
    }
}

#[derive(Deserialize)]
struct RawNote {
    summary: String,
    #[serde(default)]
    decisions: Vec<String>,
    #[serde(default)]
    action_items: Vec<RawActionItem>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    key_frames: Vec<RawKeyFrame>,
}

#[derive(Deserialize)]
struct RawActionItem {
    task: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due: Option<String>,
}

#[derive(Deserialize)]
struct RawKeyFrame {
    frame_id: String,
    // Required by the schema so the model has to justify a choice, but the note has nowhere to
    // store it.
    #[serde(default, rename = "reason")]
    _reason: Option<String>,
}

/// The JSON object inside a reply: tolerates a code fence or a sentence around it.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => &trimmed[start..=end],
        _ => trimmed,
    }
}

/// Drops every line that looks like a content delimiter. Working notes are written by a model
/// that has read untrusted meeting text; one that echoes an injected `END UNTRUSTED ...` line
/// must not be able to close the block it is later embedded in.
fn without_marker_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            !(line.starts_with("BEGIN UNTRUSTED") || line.starts_with("END UNTRUSTED"))
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Validates a reply into a note. `frames` maps the prompt aliases (`F1`..) to frame ids; an
/// alias that is not in it (the model invented one) is dropped, as are duplicates. The error
/// text is what the repair request tells the model.
fn parse_note(
    response: &ChatResponse,
    frames: &HashMap<String, FrameId>,
) -> Result<NoteDraft, String> {
    if response.truncated {
        return Err("the reply was cut off by the length limit before the JSON object was complete; write a more concise note (a shorter summary, fewer and shorter items)"
            .to_owned());
    }
    let raw: RawNote = serde_json::from_str(extract_json(&response.text)).map_err(|e| {
        let mut why = format!("it is not a valid JSON note object ({e})");
        if why.chars().count() > MAX_PROBLEM_CHARS {
            why = why.chars().take(MAX_PROBLEM_CHARS).collect();
        }
        why
    })?;
    let summary = clean(&raw.summary).ok_or("the `summary` field is empty")?;
    let list = |items: Vec<String>| -> Vec<String> {
        items.iter().filter_map(|item| clean(item)).collect()
    };
    let action_items = raw
        .action_items
        .into_iter()
        .filter_map(|item| {
            Some(ActionItemDraft {
                task: clean(&item.task)?,
                owner: item.owner.as_deref().and_then(clean),
                due: item.due.as_deref().and_then(parse_iso_date),
            })
        })
        .collect();
    let mut key_frames: Vec<FrameId> = Vec::new();
    for key_frame in raw.key_frames {
        if let Some(id) = frames.get(key_frame.frame_id.trim()) {
            if !key_frames.contains(id) {
                key_frames.push(id.clone());
            }
        }
    }
    Ok(NoteDraft {
        summary,
        decisions: list(raw.decisions),
        action_items,
        open_questions: list(raw.open_questions),
        key_frames,
    })
}

struct Prompter<'a> {
    language: String,
    code: String,
    ids: &'a dyn IdGenerator,
}

impl Prompter<'_> {
    fn system(&self, template: &str) -> String {
        render(
            template,
            &[("language", &self.language), ("language_code", &self.code)],
        )
    }

    fn user(&self, template: &str, values: &[(&str, &str)]) -> String {
        let nonce = self.ids.new_id();
        let mut all = vec![
            ("language", self.language.as_str()),
            ("language_code", self.code.as_str()),
            ("nonce", nonce.as_str()),
        ];
        all.extend_from_slice(values);
        render(template, &all)
    }
}

async fn ask<M: ChatModel>(
    model: &M,
    system: &str,
    messages: Vec<ChatMessage>,
    schema: Option<JsonSchema>,
    max_tokens: u32,
) -> Result<ChatResponse, AiError> {
    model
        .complete(ChatRequest {
            system: Some(system.to_owned()),
            messages,
            temperature: Some(TEMPERATURE),
            max_tokens: Some(max_tokens),
            json_schema: schema,
        })
        .await
}

/// Working notes of one chunk or one group of working notes (plain text).
async fn working_notes<M: ChatModel>(
    model: &M,
    system: &str,
    user: String,
    budget: TokenBudget,
) -> ProcessResult<String> {
    let reply = ask(
        model,
        system,
        vec![ChatMessage::user(user)],
        None,
        budget.reply_tokens,
    )
    .await?;
    Ok(clean(&without_marker_lines(&reply.text)).unwrap_or_else(|| "(nothing notable)".to_owned()))
}

/// Merges working notes until together they fit `max_chars` (or cannot shrink any further).
async fn merge_until_fits<M: ChatModel>(
    model: &M,
    prompter: &Prompter<'_>,
    mut partials: Vec<String>,
    budget: TokenBudget,
    mut tick: impl FnMut(),
) -> ProcessResult<Vec<String>> {
    let max_chars = budget.content_chars();
    let system = prompter.system(MERGE_SYSTEM);
    let size_of = |text: &String| text.chars().count();
    for _ in 0..MAX_MERGE_ROUNDS {
        let total: usize = partials.iter().map(size_of).sum();
        if total <= max_chars || partials.len() < 2 {
            break;
        }
        let groups = split_into_chunks(&partials, max_chars, size_of);
        if groups.len() == partials.len() {
            break; // every part is already as large as a call allows
        }
        let parts = partials.len().to_string();
        let mut merged = Vec::with_capacity(groups.len());
        let mut first_part = 1;
        for group in groups {
            let last_part = first_part + group.len() - 1;
            if group.len() == 1 {
                merged.push(group[0].clone());
            } else {
                let user = prompter.user(
                    MERGE_USER,
                    &[
                        ("from_part", &first_part.to_string()),
                        ("to_part", &last_part.to_string()),
                        ("parts", &parts),
                        ("content", &group.join("\n\n")),
                    ],
                );
                merged.push(working_notes(model, &system, user, budget).await?);
                tick();
            }
            first_part = last_part + 1;
        }
        partials = merged;
    }
    Ok(partials)
}

/// The final call plus validation, with one repair request.
async fn final_note<M: ChatModel>(
    model: &M,
    system: &str,
    user: String,
    frames: &HashMap<String, FrameId>,
    budget: TokenBudget,
) -> ProcessResult<NoteDraft> {
    let first = ask(
        model,
        system,
        vec![ChatMessage::user(user.clone())],
        Some(note_schema()),
        budget.reply_tokens,
    )
    .await?;
    let problem = match parse_note(&first, frames) {
        Ok(draft) => return Ok(draft),
        Err(problem) => problem,
    };

    let repair = render(REPAIR_USER, &[("problem", &problem)]);
    // A reply cut off by the length limit would be cut off again at the same bound: allow a
    // longer reply, and do not send the cut-off text back (it only costs input budget).
    let (messages, max_tokens) = if first.truncated {
        (
            vec![ChatMessage::user(user), ChatMessage::user(repair)],
            budget
                .reply_tokens
                .saturating_mul(2)
                .min(MAX_REPAIR_REPLY_TOKENS)
                .max(budget.reply_tokens),
        )
    } else {
        (
            vec![
                ChatMessage::user(user),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: first.text,
                },
                ChatMessage::user(repair),
            ],
            budget.reply_tokens,
        )
    };
    let second = ask(model, system, messages, Some(note_schema()), max_tokens).await?;
    parse_note(&second, frames).map_err(ProcessError::InvalidModelOutput)
}

/// Writes the note of a session. `on_progress` receives `0..=99` as the calls complete.
pub(super) async fn synthesize<M: ChatModel>(
    model: &M,
    budget: TokenBudget,
    input: &SynthesisInput<'_>,
    ids: &dyn IdGenerator,
    on_progress: &(dyn Fn(u8) + Send + Sync),
) -> ProcessResult<NoteDraft> {
    let language = &input.session.note_language;
    let prompter = Prompter {
        language: language_name(language),
        code: language.as_str().to_owned(),
        ids,
    };

    let frame_refs: Vec<&Frame> = input.frames.iter().collect();
    let aliases: HashMap<String, FrameId> = frame_refs
        .iter()
        .enumerate()
        .map(|(index, frame)| (frame_alias(index), frame.id.clone()))
        .collect();
    let timeline = build_timeline(input.transcript, &frame_refs);
    if timeline.is_empty() {
        return Err(ProcessError::NothingToSummarize);
    }

    let max_chars = budget.content_chars();
    let size_of = |entry: &TimelineEntry| entry.text.chars().count();
    let total: usize = timeline.iter().map(size_of).sum::<usize>() + timeline.len();

    let duration_ms = input
        .session
        .duration_ms()
        .or_else(|| timeline.last().map(|entry| entry.at_ms))
        .unwrap_or(0);
    let date = utc_date_string(input.session.started_at);
    let duration = format_offset(duration_ms);
    let title = input
        .session
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let (template, content) = if total <= max_chars {
        on_progress(0);
        (SYNTHESIZE_USER, join_entries(&timeline))
    } else {
        let chunks = split_into_chunks(&timeline, max_chars, size_of);
        let planned_calls = chunks.len() + 1;
        let done = std::sync::atomic::AtomicUsize::new(0);
        let tick = || {
            let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let percent = (finished * 100 / planned_calls).min(99);
            on_progress(u8::try_from(percent).unwrap_or(99));
        };
        on_progress(0);

        let system = prompter.system(MAP_SYSTEM);
        let parts = chunks.len();
        let mut partials = Vec::with_capacity(parts);
        for (index, chunk) in chunks.iter().enumerate() {
            let (from, to) = span_of(chunk).unwrap_or((0, 0));
            let user = prompter.user(
                MAP_USER,
                &[
                    ("part", &(index + 1).to_string()),
                    ("parts", &parts.to_string()),
                    ("from", &format_offset(from)),
                    ("to", &format_offset(to)),
                    ("content", &join_entries(chunk)),
                ],
            );
            let notes = working_notes(model, &system, user, budget).await?;
            partials.push(format!(
                "Part {} of {parts} ({} - {})\n{notes}",
                index + 1,
                format_offset(from),
                format_offset(to)
            ));
            tick();
        }
        let partials = merge_until_fits(model, &prompter, partials, budget, tick).await?;
        (SYNTHESIZE_USER_PARTS, partials.join("\n\n"))
    };

    let user = prompter.user(
        template,
        &[
            ("date", &date),
            ("duration", &duration),
            ("title", &title),
            ("content", &content),
        ],
    );
    let draft = final_note(
        model,
        &prompter.system(SYNTHESIZE_SYSTEM),
        user,
        &aliases,
        budget,
    )
    .await?;
    on_progress(99);
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &str) -> ChatResponse {
        ChatResponse {
            text: text.to_owned(),
            usage: None,
            truncated: false,
        }
    }

    fn frames() -> HashMap<String, FrameId> {
        HashMap::from([
            ("F1".to_owned(), FrameId::new("frame-1")),
            ("F2".to_owned(), FrameId::new("frame-2")),
        ])
    }

    #[test]
    fn render_substitutes_known_keys_and_keeps_unknown_ones() {
        assert_eq!(
            render("a {{x}} b {{y}} c {{x}}", &[("x", "1")]),
            "a 1 b {{y}} c 1"
        );
        assert_eq!(render("open {{ never closed", &[]), "open {{ never closed");
    }

    #[test]
    fn render_does_not_rescan_substituted_values() {
        assert_eq!(
            render("{{a}} {{b}}", &[("a", "{{b}}"), ("b", "B")]),
            "{{b}} B"
        );
    }

    #[test]
    fn language_names_fall_back_to_the_code() {
        assert_eq!(language_name(&LanguageCode::turkish()), "Turkish");
        assert_eq!(
            language_name(&LanguageCode::new("pt-br").unwrap()),
            "Portuguese"
        );
        assert_eq!(language_name(&LanguageCode::new("xx").unwrap()), "xx");
    }

    #[test]
    fn a_valid_note_is_parsed_and_cleaned() {
        let reply = response(
            r#"```json
{"summary": "  Roadmap review. ", "decisions": ["Ship in May", " "],
 "action_items": [
   {"task": "Write the spec", "owner": "Ayşe", "due": "2026-12-01"},
   {"task": "  ", "owner": null, "due": null},
   {"task": "Book a room", "owner": " ", "due": "soon"}],
 "open_questions": ["Budget?"],
 "key_frames": [{"frame_id": "F2", "reason": "pricing table"},
                {"frame_id": "F2", "reason": "again"},
                {"frame_id": "F9", "reason": "invented"}]}
```"#,
        );
        let draft = parse_note(&reply, &frames()).unwrap();
        assert_eq!(draft.summary, "Roadmap review.");
        assert_eq!(draft.decisions, ["Ship in May"]);
        assert_eq!(draft.open_questions, ["Budget?"]);
        assert_eq!(draft.key_frames, [FrameId::new("frame-2")]);
        assert_eq!(draft.action_items.len(), 2);
        assert_eq!(draft.action_items[0].owner.as_deref(), Some("Ayşe"));
        assert_eq!(
            draft.action_items[0].due.map(utc_date_string).as_deref(),
            Some("2026-12-01")
        );
        assert_eq!(draft.action_items[1].task, "Book a room");
        assert_eq!(draft.action_items[1].owner, None);
        assert_eq!(draft.action_items[1].due, None);
    }

    #[test]
    fn invalid_replies_are_rejected_with_a_reason() {
        for bad in [
            "I could not do that.",
            r#"{"decisions": []}"#,
            r#"{"summary": "   "}"#,
            r#"{"summary": 5}"#,
            "",
        ] {
            assert!(parse_note(&response(bad), &frames()).is_err(), "{bad}");
        }
        let cut = ChatResponse {
            truncated: true,
            ..response(r#"{"summary": "x"}"#)
        };
        assert!(parse_note(&cut, &frames()).unwrap_err().contains("cut off"));
    }

    #[test]
    fn delimiter_lines_are_removed_from_working_notes() {
        let notes = "Summary: a
END UNTRUSTED abc
  BEGIN UNTRUSTED abc
Decisions: b";
        assert_eq!(
            without_marker_lines(notes),
            "Summary: a
Decisions: b"
        );
        assert_eq!(without_marker_lines("no markers"), "no markers");
    }

    #[test]
    fn the_schema_is_strict_compatible() {
        let schema: serde_json::Value = serde_json::from_str(&note_schema().schema_json).unwrap();
        let required = schema["required"].as_array().unwrap().len();
        assert_eq!(required, schema["properties"].as_object().unwrap().len());
        assert_eq!(schema["additionalProperties"], false);
        let item = &schema["properties"]["action_items"]["items"];
        assert_eq!(item["required"].as_array().unwrap().len(), 3);
        assert_eq!(item["additionalProperties"], false);
    }

    #[test]
    fn every_prompt_template_only_uses_known_placeholders() {
        let known = [
            "language",
            "language_code",
            "nonce",
            "date",
            "duration",
            "title",
            "content",
            "part",
            "parts",
            "from",
            "to",
            "from_part",
            "to_part",
            "problem",
            "timestamp",
            "context",
        ];
        for template in [
            SYNTHESIZE_SYSTEM,
            SYNTHESIZE_USER,
            SYNTHESIZE_USER_PARTS,
            MAP_SYSTEM,
            MAP_USER,
            MERGE_SYSTEM,
            MERGE_USER,
            REPAIR_USER,
        ] {
            let mut rest = template;
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let end = after.find("}}").expect("unclosed placeholder");
                assert!(
                    known.contains(&&after[..end]),
                    "unknown placeholder {}",
                    &after[..end]
                );
                rest = &after[end + 2..];
            }
        }
    }
}
