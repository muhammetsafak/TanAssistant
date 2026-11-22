//! Action briefs: from one action item of a note, on request, a work brief that turns the task
//! into a plan. The brief holds the context, the details the meeting gave, the decisions and
//! open questions of the note that bear on the task, the frames and time-stamped quotes that
//! matter, and steps the AI *suggests* (they are labelled as suggestions: nobody agreed to them).
//!
//! A brief is read-only for the user, stored with its action item (one per item; generating
//! again replaces it) and can be exported as Markdown. It is written by the provider assigned
//! to the summary task, in the language of the note, from the same time-ordered text the note
//! was written from ([`build_timeline`]); meeting content always travels between random
//! delimiters and the model is told to treat it as data, never as instructions.
//!
//! A meeting that fits the model's budget is handled in one call. A longer one is cut into
//! chunks first: each chunk yields working notes about the action item only (map), the notes are
//! condensed until they fit one call, and one final call writes the brief (the budget and
//! chunking policy of the note's `Synthesize` step).
//!
//! The model's reply is validated against what it was given: a frame marker it did not receive,
//! a quote outside the time span of the transcript, a decision or question that is not one of
//! the note's own are dropped.
//!
//! Every error type here is safe to show to the user: none carries an API key.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tan_domain::{
    ActionBrief, ActionItem, ActionItemId, EpochMs, Frame, FrameId, LanguageCode, Note, Person,
    ProviderProfile, ProviderProfileId, Session, TaskKind, TranscriptSegment,
};

use crate::error::RepositoryError;
use crate::people::normalize_task;
use crate::ports::{
    ActionBriefRepository, AiError, ChatMessage, ChatModel, ChatResponse, ChatRole, Clock,
    FrameRepository, IdGenerator, JsonSchema, NoteRepository, PersonRepository, ProviderFactory,
    ProviderProfileRepository, SessionRepository, TaskAssignmentRepository, TranscriptRepository,
};
use crate::processing::{
    ask, build_timeline, clean, extract_json, format_offset, frame_alias, join_entries,
    language_name, render, span_of, split_into_chunks, utc_date_string, working_notes,
    ProcessError, Prompter, TimelineEntry, TokenBudget, MAX_MERGE_ROUNDS, MAX_PROBLEM_CHARS,
    MAX_REPAIR_REPLY_TOKENS, REPAIR_USER,
};
use crate::sessions::render_action_brief_markdown;

const BRIEF_SYSTEM: &str = include_str!("prompts/action_brief.md");
const BRIEF_USER: &str = include_str!("prompts/action_brief_user.md");
const MAP_SYSTEM: &str = include_str!("prompts/action_brief_map.md");
const MAP_USER: &str = include_str!("prompts/action_brief_map_user.md");

/// Most quotes a brief keeps (the prompt asks for no more).
const MAX_QUOTES: usize = 8;

// ---------------------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------------------

/// A frame a brief points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefFrame {
    /// The frame; always one of the session's described frames when the brief was written.
    pub frame_id: FrameId,
    /// Why it matters for the task.
    pub reason: String,
}

/// A moment of the conversation a brief quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefQuote {
    /// Offset into the meeting, in milliseconds (second precision); always inside the time span
    /// of the transcript.
    pub at_ms: u64,
    /// Who spoke, as written in the transcript (`me`, `others` or a name).
    pub speaker: String,
    /// The words.
    pub text: String,
}

/// What a brief says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBriefContent {
    /// Why the task exists and what it is about.
    pub context: String,
    /// What the meeting said about how, with whom, by when or under which constraints.
    pub details: Vec<String>,
    /// Decisions of the note that bear on the task (the note's own words).
    pub related_decisions: Vec<String>,
    /// Open questions of the note that bear on the task (the note's own words).
    pub open_questions: Vec<String>,
    /// Frames that matter, in the order the model gave them.
    pub frames: Vec<BriefFrame>,
    /// Quotes, in time order.
    pub quotes: Vec<BriefQuote>,
    /// Steps the AI suggests; they were not agreed in the meeting.
    pub suggested_steps: Vec<String>,
}

/// The stored shape of the content (`action_briefs.content_json`). The ids are plain strings
/// here: the stored text is read back by [`ActionBriefContent::from_json`].
#[derive(Serialize, Deserialize)]
struct StoredContent {
    context: String,
    #[serde(default)]
    details: Vec<String>,
    #[serde(default)]
    related_decisions: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    frames: Vec<StoredFrame>,
    #[serde(default)]
    quotes: Vec<StoredQuote>,
    #[serde(default)]
    suggested_steps: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredFrame {
    frame_id: String,
    reason: String,
}

#[derive(Serialize, Deserialize)]
struct StoredQuote {
    at_ms: u64,
    speaker: String,
    text: String,
}

impl ActionBriefContent {
    /// The JSON text that is stored.
    ///
    /// # Errors
    ///
    /// Fails only if serialization does, which plain strings and numbers never do.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&StoredContent {
            context: self.context.clone(),
            details: self.details.clone(),
            related_decisions: self.related_decisions.clone(),
            open_questions: self.open_questions.clone(),
            frames: self
                .frames
                .iter()
                .map(|frame| StoredFrame {
                    frame_id: frame.frame_id.to_string(),
                    reason: frame.reason.clone(),
                })
                .collect(),
            quotes: self
                .quotes
                .iter()
                .map(|quote| StoredQuote {
                    at_ms: quote.at_ms,
                    speaker: quote.speaker.clone(),
                    text: quote.text.clone(),
                })
                .collect(),
            suggested_steps: self.suggested_steps.clone(),
        })
    }

    /// Reads stored JSON text back.
    ///
    /// # Errors
    ///
    /// Fails when the text is not a stored brief.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let stored: StoredContent = serde_json::from_str(json)?;
        Ok(Self {
            context: stored.context,
            details: stored.details,
            related_decisions: stored.related_decisions,
            open_questions: stored.open_questions,
            frames: stored
                .frames
                .into_iter()
                .map(|frame| BriefFrame {
                    frame_id: FrameId::new(frame.frame_id),
                    reason: frame.reason,
                })
                .collect(),
            quotes: stored
                .quotes
                .into_iter()
                .map(|quote| BriefQuote {
                    at_ms: quote.at_ms,
                    speaker: quote.speaker,
                    text: quote.text,
                })
                .collect(),
            suggested_steps: stored.suggested_steps,
        })
    }
}

/// A stored brief with its content decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBriefView {
    /// The action item the brief is about.
    pub action_item_id: ActionItemId,
    /// The language it was written in (the note's).
    pub language: LanguageCode,
    /// What it says.
    pub content: ActionBriefContent,
    /// The profile whose model wrote it; `None` when that profile was deleted since.
    pub provider_profile_id: Option<ProviderProfileId>,
    /// When it was generated.
    pub created_at: EpochMs,
}

impl ActionBriefView {
    fn from_brief(brief: &ActionBrief) -> BriefResult<Self> {
        let content = ActionBriefContent::from_json(&brief.content_json).map_err(|error| {
            BriefError::Storage(RepositoryError::CorruptData(format!(
                "the stored brief of action item {} cannot be read: {error}",
                brief.action_item_id
            )))
        })?;
        Ok(Self {
            action_item_id: brief.action_item_id.clone(),
            language: brief.language.clone(),
            content,
            provider_profile_id: brief.provider_profile_id.clone(),
            created_at: brief.created_at,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

/// Failure of an action brief use case.
#[derive(Debug)]
pub enum BriefError {
    /// The addressed entity (`action item`, `note`, `session`, `action brief`) does not exist
    /// (any more: regenerating the note replaces its action items).
    NotFound(&'static str),
    /// A brief for this action item is being generated right now.
    AlreadyRunning,
    /// No provider is assigned to the summary task.
    NoChatModel,
    /// An AI call failed (or a provider adapter could not be built).
    Provider(AiError),
    /// The chat model's reply was not a valid brief, even after one repair request.
    InvalidModelOutput(String),
    /// There is neither a transcript nor a screen description to write a brief from.
    NothingToSummarize,
    /// The database failed.
    Storage(RepositoryError),
    /// A bug, not a user error.
    Internal(String),
}

impl fmt::Display for BriefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(entity) => write!(f, "{entity} not found"),
            Self::AlreadyRunning => {
                f.write_str("a brief for this action item is already being generated")
            }
            Self::NoChatModel => f.write_str(
                "no AI provider is assigned to the summary task; assign one in the settings",
            ),
            Self::Provider(error) => write!(f, "{error}"),
            Self::InvalidModelOutput(why) => {
                write!(f, "the model did not return a valid brief: {why}")
            }
            Self::NothingToSummarize => {
                f.write_str("the session has no transcript or screen content to write a brief from")
            }
            // Details stay in `source()` for logs; the user-facing text is generic.
            Self::Storage(_) => f.write_str("the database operation failed"),
            Self::Internal(why) => write!(f, "internal error: {why}"),
        }
    }
}

impl Error for BriefError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RepositoryError> for BriefError {
    fn from(error: RepositoryError) -> Self {
        Self::Storage(error)
    }
}

impl From<ProcessError> for BriefError {
    /// The shared prompt helpers report their failures as processing errors; only provider
    /// failures can occur there.
    fn from(error: ProcessError) -> Self {
        match error {
            ProcessError::Provider(error) => Self::Provider(error),
            ProcessError::Storage(error) => Self::Storage(error),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<AiError> for BriefError {
    fn from(error: AiError) -> Self {
        Self::Provider(error)
    }
}

/// Result alias for action brief use cases.
pub type BriefResult<T> = Result<T, BriefError>;

// ---------------------------------------------------------------------------------------
// Validation of the model's reply
// ---------------------------------------------------------------------------------------

/// What the reply is checked against: only what the model was given may come back.
struct Allowed<'a> {
    /// Prompt aliases (`F1`..) to frame ids.
    frames: &'a HashMap<String, FrameId>,
    /// The note's decisions.
    decisions: &'a [String],
    /// The note's open questions.
    open_questions: &'a [String],
    /// First and last offset of the transcript; `None` when there is no transcript, so there is
    /// nothing to quote.
    span_ms: Option<(u64, u64)>,
}

/// The JSON Schema of the brief: strict-compatible (every property required, nothing extra).
fn brief_schema() -> JsonSchema {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "context",
            "details",
            "related_decisions",
            "open_questions",
            "frames",
            "quotes",
            "suggested_steps"
        ],
        "properties": {
            "context": { "type": "string" },
            "details": strings,
            "related_decisions": strings,
            "open_questions": strings,
            "frames": {
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
            },
            "quotes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["at", "speaker", "text"],
                    "properties": {
                        "at": { "type": "string" },
                        "speaker": { "type": "string" },
                        "text": { "type": "string" }
                    }
                }
            },
            "suggested_steps": strings
        }
    });
    JsonSchema {
        name: "action_brief".to_owned(),
        schema_json: schema.to_string(),
    }
}

#[derive(Deserialize)]
struct RawBrief {
    context: String,
    #[serde(default)]
    details: Vec<String>,
    #[serde(default)]
    related_decisions: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    frames: Vec<RawFrame>,
    #[serde(default)]
    quotes: Vec<RawQuote>,
    #[serde(default)]
    suggested_steps: Vec<String>,
}

#[derive(Deserialize)]
struct RawFrame {
    frame_id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct RawQuote {
    at: String,
    #[serde(default)]
    speaker: Option<String>,
    text: String,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cleans a list of texts: trimmed, no empty entries.
fn clean_list(items: &[String]) -> Vec<String> {
    items.iter().filter_map(|item| clean(item)).collect()
}

/// The entries of `allowed` the model named (compared by the normalized text, so case and
/// spacing do not matter), as the note wrote them, without repeats. Anything else is dropped.
fn pick_from(named: Vec<String>, allowed: &[String]) -> Vec<String> {
    let mut picked: Vec<String> = Vec::new();
    for text in named {
        let key = normalize_task(&text);
        if let Some(own) = allowed.iter().find(|own| normalize_task(own) == key) {
            if !picked.contains(own) {
                picked.push(own.clone());
            }
        }
    }
    picked
}

/// The offset in milliseconds of `HH:MM:SS` (or `MM:SS`; square brackets are tolerated).
fn parse_clock(text: &str) -> Option<u64> {
    let text = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut numbers = Vec::with_capacity(3);
    for part in &parts {
        if part.is_empty() || part.len() > 2 || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        numbers.push(part.parse::<u64>().ok()?);
    }
    let (hours, minutes, seconds) = match numbers.as_slice() {
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        [minutes, seconds] => (0, *minutes, *seconds),
        _ => return None,
    };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    Some((hours * 3_600 + minutes * 60 + seconds) * 1_000)
}

/// Validates a reply into brief content. The error text is what the repair request tells the
/// model.
fn parse_brief(
    response: &ChatResponse,
    allowed: &Allowed<'_>,
) -> Result<ActionBriefContent, String> {
    if response.truncated {
        return Err("the reply was cut off by the length limit before the JSON object was complete; write a more concise brief (shorter paragraphs, fewer and shorter items)"
            .to_owned());
    }
    let raw: RawBrief = serde_json::from_str(extract_json(&response.text)).map_err(|e| {
        let mut why = format!("it is not a valid JSON brief object ({e})");
        if why.chars().count() > MAX_PROBLEM_CHARS {
            why = why.chars().take(MAX_PROBLEM_CHARS).collect();
        }
        why
    })?;
    let context = clean(&raw.context).ok_or("the `context` field is empty")?;

    let mut frames: Vec<BriefFrame> = Vec::new();
    for frame in raw.frames {
        if let Some(id) = allowed.frames.get(frame.frame_id.trim()) {
            if !frames.iter().any(|known| known.frame_id == *id) {
                frames.push(BriefFrame {
                    frame_id: id.clone(),
                    reason: frame.reason.as_deref().and_then(clean).unwrap_or_default(),
                });
            }
        }
    }

    let mut quotes: Vec<BriefQuote> = Vec::new();
    if let Some((first, last)) = allowed.span_ms {
        // The prompt shows whole seconds, so a quote may lie up to a second before the first
        // segment's exact start.
        let first = first / 1_000 * 1_000;
        for quote in raw.quotes {
            let (Some(at_ms), Some(text)) = (parse_clock(&quote.at), clean(&quote.text)) else {
                continue;
            };
            if at_ms < first || at_ms > last {
                continue;
            }
            quotes.push(BriefQuote {
                at_ms,
                speaker: quote
                    .speaker
                    .as_deref()
                    .map(one_line)
                    .filter(|speaker| !speaker.is_empty())
                    .unwrap_or_else(|| "unknown".to_owned()),
                text: one_line(&text),
            });
        }
        // Stable: quotes of the same second keep the order the model gave them.
        quotes.sort_by_key(|quote| quote.at_ms);
        quotes.truncate(MAX_QUOTES);
    }

    Ok(ActionBriefContent {
        context,
        details: clean_list(&raw.details),
        related_decisions: pick_from(clean_list(&raw.related_decisions), allowed.decisions),
        open_questions: pick_from(clean_list(&raw.open_questions), allowed.open_questions),
        frames,
        quotes,
        suggested_steps: clean_list(&raw.suggested_steps),
    })
}

// ---------------------------------------------------------------------------------------
// Writing a brief
// ---------------------------------------------------------------------------------------

/// What a brief is written from.
struct BriefInput<'a> {
    session: &'a Session,
    note: &'a Note,
    item: &'a ActionItem,
    owner: Option<&'a Person>,
    transcript: &'a [TranscriptSegment],
    /// The frames that have a description, in time order.
    frames: &'a [Frame],
}

/// The action item as the prompts show it. Everything here was written by a model that read
/// untrusted meeting text (or by the user), so it is flattened to single lines and only ever
/// placed inside the delimited block.
fn action_text(input: &BriefInput<'_>) -> String {
    let mut lines = vec![format!("Task: {}", one_line(&input.item.task))];
    if let Some(owner) = input.owner {
        let name = if owner.is_self {
            "me (the person who made the recording)".to_owned()
        } else {
            one_line(&owner.display_name)
        };
        lines.push(format!("Owner: {name}"));
    }
    if let Some(due) = input.item.due {
        lines.push(format!("Due: {}", utc_date_string(due)));
    }
    lines.push(format!(
        "Status: {}",
        if input.item.done { "done" } else { "open" }
    ));
    lines.join("\n")
}

/// The note's summary, decisions and open questions as the prompt shows them.
fn note_text(note: &Note) -> String {
    let list = |items: &[String]| -> String {
        if items.is_empty() {
            "(none)".to_owned()
        } else {
            items
                .iter()
                .map(|item| format!("- {}", one_line(item)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    format!(
        "SUMMARY\n{}\n\nDECISIONS\n{}\n\nOPEN QUESTIONS\n{}",
        note.summary.trim(),
        list(&note.decisions),
        list(&note.open_questions)
    )
}

/// Working notes of one chunk, or of a run of chunks that were condensed into one.
#[derive(Clone)]
struct Partial {
    first_part: usize,
    last_part: usize,
    from_ms: u64,
    to_ms: u64,
    text: String,
}

impl Partial {
    fn render(&self, parts: usize) -> String {
        let label = if self.first_part == self.last_part {
            self.first_part.to_string()
        } else {
            format!("{}-{}", self.first_part, self.last_part)
        };
        format!(
            "Part {label} of {parts} ({} - {})\n{}",
            format_offset(self.from_ms),
            format_offset(self.to_ms),
            self.text
        )
    }
}

/// Condenses working notes until together they fit `limit` characters (or cannot shrink any
/// further), by running the map prompt over groups of them.
async fn condense<M: ChatModel>(
    model: &M,
    prompter: &Prompter<'_>,
    action: &str,
    mut partials: Vec<Partial>,
    parts: usize,
    limit: usize,
    budget: TokenBudget,
) -> BriefResult<Vec<Partial>> {
    let system = prompter.system(MAP_SYSTEM);
    let size_of = |partial: &Partial| partial.render(parts).chars().count();
    for _ in 0..MAX_MERGE_ROUNDS {
        let total: usize = partials.iter().map(size_of).sum();
        if total <= limit || partials.len() < 2 {
            break;
        }
        let groups = split_into_chunks(&partials, limit, size_of);
        if groups.len() == partials.len() {
            break; // every part is already as large as a call allows
        }
        let mut merged = Vec::with_capacity(groups.len());
        for group in groups {
            let (Some(first), Some(last)) = (group.first(), group.last()) else {
                continue;
            };
            if group.len() == 1 {
                merged.push(first.clone());
                continue;
            }
            let content = group
                .iter()
                .map(|partial| partial.render(parts))
                .collect::<Vec<_>>()
                .join("\n\n");
            let label = format!("{}-{}", first.first_part, last.last_part);
            let user = prompter.user(
                MAP_USER,
                &[
                    ("part", &label),
                    ("parts", &parts.to_string()),
                    ("from", &format_offset(first.from_ms)),
                    ("to", &format_offset(last.to_ms)),
                    ("action", action),
                    ("content", &content),
                ],
            );
            merged.push(Partial {
                first_part: first.first_part,
                last_part: last.last_part,
                from_ms: first.from_ms,
                to_ms: last.to_ms,
                text: working_notes(model, &system, user, budget).await?,
            });
        }
        partials = merged;
    }
    Ok(partials)
}

/// The final call plus validation, with one repair request.
async fn final_brief<M: ChatModel>(
    model: &M,
    system: &str,
    user: String,
    allowed: &Allowed<'_>,
    budget: TokenBudget,
) -> BriefResult<ActionBriefContent> {
    let first = ask(
        model,
        system,
        vec![ChatMessage::user(user.clone())],
        Some(brief_schema()),
        budget.reply_tokens,
    )
    .await?;
    let problem = match parse_brief(&first, allowed) {
        Ok(content) => return Ok(content),
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
    let second = ask(model, system, messages, Some(brief_schema()), max_tokens).await?;
    parse_brief(&second, allowed).map_err(BriefError::InvalidModelOutput)
}

/// Writes the brief content for one action item.
async fn write_brief<M: ChatModel>(
    model: &M,
    budget: TokenBudget,
    input: &BriefInput<'_>,
    ids: &dyn IdGenerator,
) -> BriefResult<ActionBriefContent> {
    let language = &input.note.language;
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
        return Err(BriefError::NothingToSummarize);
    }
    let span_ms = input
        .transcript
        .iter()
        .map(|segment| segment.start_ms)
        .min()
        .zip(input.transcript.iter().map(|segment| segment.end_ms).max());
    let allowed = Allowed {
        frames: &aliases,
        decisions: &input.note.decisions,
        open_questions: &input.note.open_questions,
        span_ms,
    };

    let action = action_text(input);
    let note = note_text(input.note);
    // The room the meeting content has in one call: what is left after the action item and the
    // note, but never less than half (a huge note must not starve the transcript).
    let all_chars = budget.content_chars();
    let limit = all_chars
        .saturating_sub(action.chars().count() + note.chars().count())
        .max(all_chars / 2);
    let size_of = |entry: &TimelineEntry| entry.text.chars().count();
    let total: usize = timeline.iter().map(size_of).sum::<usize>() + timeline.len();

    let content = if total <= limit {
        join_entries(&timeline)
    } else {
        let chunks = split_into_chunks(&timeline, limit, size_of);
        let parts = chunks.len();
        let system = prompter.system(MAP_SYSTEM);
        let mut partials = Vec::with_capacity(parts);
        for (index, chunk) in chunks.iter().enumerate() {
            let (from_ms, to_ms) = span_of(chunk).unwrap_or((0, 0));
            let user = prompter.user(
                MAP_USER,
                &[
                    ("part", &(index + 1).to_string()),
                    ("parts", &parts.to_string()),
                    ("from", &format_offset(from_ms)),
                    ("to", &format_offset(to_ms)),
                    ("action", &action),
                    ("content", &join_entries(chunk)),
                ],
            );
            partials.push(Partial {
                first_part: index + 1,
                last_part: index + 1,
                from_ms,
                to_ms,
                text: working_notes(model, &system, user, budget).await?,
            });
        }
        let partials = condense(model, &prompter, &action, partials, parts, limit, budget).await?;
        partials
            .iter()
            .map(|partial| partial.render(parts))
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let duration_ms = input
        .session
        .duration_ms()
        .or_else(|| timeline.last().map(|entry| entry.at_ms))
        .unwrap_or(0);
    let user = prompter.user(
        BRIEF_USER,
        &[
            ("date", &utc_date_string(input.session.started_at)),
            ("duration", &format_offset(duration_ms)),
            ("title", &one_line(&input.session.title)),
            ("action", &action),
            ("note", &note),
            ("content", &content),
        ],
    );
    final_brief(
        model,
        &prompter.system(BRIEF_SYSTEM),
        user,
        &allowed,
        budget,
    )
    .await
}

// ---------------------------------------------------------------------------------------
// The use case
// ---------------------------------------------------------------------------------------

/// Everything [`ActionBriefService`] depends on.
pub struct BriefPorts<S, Fr, T, N, A, P, F, B, D> {
    /// Sessions (title, date).
    pub sessions: Arc<S>,
    /// Frames and their descriptions.
    pub frames: Arc<Fr>,
    /// The transcript.
    pub transcripts: Arc<T>,
    /// Notes and action items.
    pub notes: Arc<N>,
    /// Which profile performs which task.
    pub assignments: Arc<A>,
    /// The provider profiles.
    pub profiles: Arc<P>,
    /// Builds chat adapters.
    pub factory: Arc<F>,
    /// The stored briefs.
    pub briefs: Arc<B>,
    /// The person directory (the owner's name).
    pub people: Arc<D>,
    /// Mints prompt delimiters.
    pub ids: Arc<dyn IdGenerator>,
    /// The current time.
    pub clock: Arc<dyn Clock>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked; the data is a plain set.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Marks an action item as being worked on; unmarks it on drop.
struct ActiveRun<'a> {
    registry: &'a Mutex<HashSet<ActionItemId>>,
    id: ActionItemId,
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        lock(self.registry).remove(&self.id);
    }
}

/// Writes, stores, reads and exports the brief of an action item.
///
/// At most one generation per action item at a time. Generating does not depend on the state of
/// the session: it works while the session is still recording or being processed, from the
/// transcript and the note that exist at that moment. If the note is regenerated meanwhile the
/// action item is gone and the call fails with [`BriefError::NotFound`].
/// Generating while the session is reprocessed is not serialized with the reprocessing run: a
/// brief that finishes in the instant a run is replacing the note can be replaced by the
/// action item's earlier brief (the one the run carried over). Generating again fixes it.
pub struct ActionBriefService<S, Fr, T, N, A, P, F, B, D> {
    ports: BriefPorts<S, Fr, T, N, A, P, F, B, D>,
    active: Mutex<HashSet<ActionItemId>>,
    budget_for: fn(&ProviderProfile) -> TokenBudget,
}

impl<S, Fr, T, N, A, P, F, B, D> ActionBriefService<S, Fr, T, N, A, P, F, B, D>
where
    S: SessionRepository,
    Fr: FrameRepository,
    T: TranscriptRepository,
    N: NoteRepository,
    A: TaskAssignmentRepository,
    P: ProviderProfileRepository,
    F: ProviderFactory,
    B: ActionBriefRepository,
    D: PersonRepository,
{
    /// Creates the use case with the default token budgets ([`TokenBudget::for_profile`]).
    #[must_use]
    pub fn new(ports: BriefPorts<S, Fr, T, N, A, P, F, B, D>) -> Self {
        Self {
            ports,
            active: Mutex::new(HashSet::new()),
            budget_for: TokenBudget::for_profile,
        }
    }

    /// Replaces how the token budget is chosen for a profile (provider/model specific tuning).
    #[must_use]
    pub fn with_budget(mut self, budget_for: fn(&ProviderProfile) -> TokenBudget) -> Self {
        self.budget_for = budget_for;
        self
    }

    /// Whether a brief for the action item is being generated right now.
    #[must_use]
    pub fn is_generating(&self, id: &ActionItemId) -> bool {
        lock(&self.active).contains(id)
    }

    /// The stored brief of an action item; `None` when none was generated. An action item that
    /// does not exist is [`BriefError::NotFound`].
    pub async fn get(&self, id: &ActionItemId) -> BriefResult<Option<ActionBriefView>> {
        if self.ports.notes.get_action_item(id).await?.is_none() {
            return Err(BriefError::NotFound("action item"));
        }
        self.ports
            .briefs
            .get(id)
            .await?
            .as_ref()
            .map(ActionBriefView::from_brief)
            .transpose()
    }

    /// Generates the brief of an action item with the provider assigned to the summary task and
    /// stores it, replacing the earlier one.
    ///
    /// Fails with [`BriefError::AlreadyRunning`] while another call for the same action item is
    /// in flight, [`BriefError::NoChatModel`] when no summary provider is assigned,
    /// [`BriefError::NothingToSummarize`] when the session has neither transcript nor screen
    /// descriptions, and [`BriefError::InvalidModelOutput`] when the reply is unusable even after
    /// one repair request. Nothing is stored in the failing cases.
    pub async fn generate(&self, id: &ActionItemId) -> BriefResult<ActionBriefView> {
        let _run = self.register(id)?;
        let (item, note, session) = self.load(id).await?;
        let profile = self
            .assigned_summary()
            .await?
            .ok_or(BriefError::NoChatModel)?;
        let model = self.ports.factory.chat(&profile).await?;
        let transcript = self.ports.transcripts.list_for_session(&session.id).await?;
        let described: Vec<Frame> = self
            .ports
            .frames
            .list_for_session(&session.id)
            .await?
            .into_iter()
            .filter(|frame| frame.description.is_some())
            .collect();
        let owner = self.owner_of(&item).await?;

        let content = write_brief(
            &model,
            (self.budget_for)(&profile),
            &BriefInput {
                session: &session,
                note: &note,
                item: &item,
                owner: owner.as_ref(),
                transcript: &transcript,
                frames: &described,
            },
            &*self.ports.ids,
        )
        .await?;

        let brief = ActionBrief {
            action_item_id: item.id.clone(),
            language: note.language.clone(),
            content_json: content
                .to_json()
                .map_err(|error| BriefError::Internal(error.to_string()))?,
            provider_profile_id: Some(profile.id.clone()),
            created_at: self.ports.clock.now(),
        };
        match self.ports.briefs.save(&brief).await {
            Ok(()) => {}
            // The note was regenerated while the model worked: the action item is gone.
            Err(RepositoryError::Conflict(_)) => return Err(BriefError::NotFound("action item")),
            Err(error) => return Err(error.into()),
        }
        Ok(ActionBriefView {
            action_item_id: brief.action_item_id,
            language: brief.language,
            content,
            provider_profile_id: brief.provider_profile_id,
            created_at: brief.created_at,
        })
    }

    /// The stored brief as Markdown, in the language it was written in; the suggested steps sit
    /// under the heading "Öneri" (Turkish) or "Suggestion" and say they were not agreed.
    /// [`BriefError::NotFound`] when the action item has no brief.
    pub async fn export_markdown(&self, id: &ActionItemId) -> BriefResult<String> {
        let (item, _note, session) = self.load(id).await?;
        let brief = self
            .ports
            .briefs
            .get(id)
            .await?
            .ok_or(BriefError::NotFound("action brief"))?;
        let view = ActionBriefView::from_brief(&brief)?;
        let owner = self.owner_of(&item).await?;
        let frames = self.ports.frames.list_for_session(&session.id).await?;
        Ok(render_action_brief_markdown(
            &session,
            &item,
            owner.as_ref(),
            &view.language,
            &view.content,
            &frames,
        ))
    }

    // ----- helpers ---------------------------------------------------------------------

    fn register(&self, id: &ActionItemId) -> BriefResult<ActiveRun<'_>> {
        let mut active = lock(&self.active);
        if !active.insert(id.clone()) {
            return Err(BriefError::AlreadyRunning);
        }
        Ok(ActiveRun {
            registry: &self.active,
            id: id.clone(),
        })
    }

    /// The action item with its note and session.
    async fn load(&self, id: &ActionItemId) -> BriefResult<(ActionItem, Note, Session)> {
        let item = self
            .ports
            .notes
            .get_action_item(id)
            .await?
            .ok_or(BriefError::NotFound("action item"))?;
        let note = self
            .ports
            .notes
            .get(&item.note_id)
            .await?
            .ok_or(BriefError::NotFound("note"))?;
        let session = self
            .ports
            .sessions
            .get(&note.session_id)
            .await?
            .ok_or(BriefError::NotFound("session"))?;
        Ok((item, note, session))
    }

    async fn owner_of(&self, item: &ActionItem) -> BriefResult<Option<Person>> {
        match &item.owner_person_id {
            Some(person_id) => Ok(self.ports.people.get(person_id).await?),
            None => Ok(None),
        }
    }

    /// The profile assigned to the summary task, if any (an assignment whose profile is gone
    /// counts as none).
    async fn assigned_summary(&self) -> BriefResult<Option<ProviderProfile>> {
        let Some(assignment) = self.ports.assignments.get(TaskKind::Summary).await? else {
            return Ok(None);
        };
        Ok(self.ports.profiles.get(&assignment.profile_id).await?)
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)] // `assert!(x.is_empty())` reads fine in tests
mod tests {
    use super::*;

    fn response(text: &str) -> ChatResponse {
        ChatResponse {
            text: text.to_owned(),
            usage: None,
            truncated: false,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn clock_text_is_read_as_an_offset() {
        assert_eq!(parse_clock("00:00:05"), Some(5_000));
        assert_eq!(parse_clock("01:02:03"), Some(3_723_000));
        assert_eq!(parse_clock(" [00:01:30] "), Some(90_000));
        assert_eq!(parse_clock("02:30"), Some(150_000));
        for bad in [
            "",
            "5",
            "00:61:00",
            "00:00:61",
            "a:b:c",
            "1:2:3:4",
            "00:00:005",
        ] {
            assert_eq!(parse_clock(bad), None, "{bad}");
        }
    }

    #[test]
    fn content_survives_the_stored_json() {
        let content = ActionBriefContent {
            context: "Why".to_owned(),
            details: strings(&["a", "b"]),
            related_decisions: strings(&["d"]),
            open_questions: strings(&["q"]),
            frames: vec![BriefFrame {
                frame_id: FrameId::new("f1"),
                reason: "table".to_owned(),
            }],
            quotes: vec![BriefQuote {
                at_ms: 5_000,
                speaker: "me".to_owned(),
                text: "words".to_owned(),
            }],
            suggested_steps: strings(&["do it"]),
        };
        let json = content.to_json().unwrap();
        assert_eq!(ActionBriefContent::from_json(&json).unwrap(), content);
        assert!(ActionBriefContent::from_json("{}").is_err());
    }

    #[test]
    fn only_entries_the_note_has_are_picked() {
        let allowed = strings(&["Bütçe onaylandı", "Lansman Mayıs'ta"]);
        assert_eq!(
            pick_from(
                strings(&[
                    "  bütçe   ONAYLANDI ",
                    "Invented decision",
                    "Bütçe onaylandı",
                    "Lansman Mayıs'ta"
                ]),
                &allowed
            ),
            ["Bütçe onaylandı", "Lansman Mayıs'ta"]
        );
    }

    #[test]
    fn a_reply_is_validated_against_the_input() {
        let frames = HashMap::from([("F1".to_owned(), FrameId::new("frame-1"))]);
        let decisions = strings(&["Budget approved"]);
        let questions = strings(&["When is the launch?"]);
        let allowed = Allowed {
            frames: &frames,
            decisions: &decisions,
            open_questions: &questions,
            span_ms: Some((2_500, 20_000)),
        };
        let reply = response(
            r#"{"context": " Why. ", "details": ["x", " "],
 "related_decisions": ["budget approved", "made up"],
 "open_questions": ["made up too"],
 "frames": [{"frame_id": "F1", "reason": "table"}, {"frame_id": "F1", "reason": "again"},
            {"frame_id": "F7", "reason": "invented"}],
 "quotes": [{"at": "00:00:19", "speaker": "others", "text": "late"},
            {"at": "00:00:02", "speaker": "me", "text": "early but inside"},
            {"at": "00:00:01", "speaker": "me", "text": "before the transcript"},
            {"at": "00:01:00", "speaker": "me", "text": "after the transcript"},
            {"at": "soon", "speaker": "me", "text": "no time"},
            {"at": "00:00:10", "speaker": " ", "text": "  "}],
 "suggested_steps": ["Start", ""]}"#,
        );
        let content = parse_brief(&reply, &allowed).unwrap();
        assert_eq!(content.context, "Why.");
        assert_eq!(content.details, ["x"]);
        assert_eq!(content.related_decisions, ["Budget approved"]);
        assert!(content.open_questions.is_empty());
        assert_eq!(
            content.frames,
            [BriefFrame {
                frame_id: FrameId::new("frame-1"),
                reason: "table".to_owned()
            }]
        );
        let at: Vec<u64> = content.quotes.iter().map(|quote| quote.at_ms).collect();
        assert_eq!(
            at,
            [2_000, 19_000],
            "out-of-span quotes are dropped, rest sorted"
        );
        assert_eq!(content.suggested_steps, ["Start"]);
    }

    #[test]
    fn without_a_transcript_nothing_can_be_quoted() {
        let frames = HashMap::new();
        let allowed = Allowed {
            frames: &frames,
            decisions: &[],
            open_questions: &[],
            span_ms: None,
        };
        let reply = response(
            r#"{"context": "c", "quotes": [{"at": "00:00:01", "speaker": "me", "text": "x"}]}"#,
        );
        assert!(parse_brief(&reply, &allowed).unwrap().quotes.is_empty());
    }

    #[test]
    fn unusable_replies_are_rejected_with_a_reason() {
        let frames = HashMap::new();
        let allowed = Allowed {
            frames: &frames,
            decisions: &[],
            open_questions: &[],
            span_ms: None,
        };
        for bad in [
            "I could not do that.",
            r#"{"details": []}"#,
            r#"{"context": "  "}"#,
            r#"{"context": 5}"#,
            "",
        ] {
            assert!(parse_brief(&response(bad), &allowed).is_err(), "{bad}");
        }
        let cut = ChatResponse {
            truncated: true,
            ..response(r#"{"context": "x"}"#)
        };
        assert!(parse_brief(&cut, &allowed).unwrap_err().contains("cut off"));
    }

    #[test]
    fn the_schema_is_strict_compatible() {
        let schema: serde_json::Value = serde_json::from_str(&brief_schema().schema_json).unwrap();
        let required = schema["required"].as_array().unwrap().len();
        assert_eq!(required, schema["properties"].as_object().unwrap().len());
        assert_eq!(schema["additionalProperties"], false);
        for list in ["frames", "quotes"] {
            let item = &schema["properties"][list]["items"];
            assert_eq!(
                item["required"].as_array().unwrap().len(),
                item["properties"].as_object().unwrap().len()
            );
            assert_eq!(item["additionalProperties"], false);
        }
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
            "action",
            "note",
            "content",
            "part",
            "parts",
            "from",
            "to",
        ];
        for template in [BRIEF_SYSTEM, BRIEF_USER, MAP_SYSTEM, MAP_USER] {
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
