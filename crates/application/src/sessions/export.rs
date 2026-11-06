//! Markdown export of a session: the note, the key frames and the transcript.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use tan_domain::{LanguageCode, ProcessingStep, StepState};

use super::{NoteWithItems, SessionDetail, SessionFileStore, SessionsResult};
use crate::processing::{format_offset, utc_date_string, utc_datetime_string};
use crate::transcription::{SPEAKER_ME, SPEAKER_OTHERS};

/// Name of the exported file inside the session folder. Frame paths are relative to that
/// folder (`frames/...`), so the image links in the file work where it is.
pub const EXPORT_FILE_NAME: &str = "notes.md";

/// Longest image caption, in characters.
const MAX_CAPTION_CHARS: usize = 120;

struct Labels {
    date: &'static str,
    duration: &'static str,
    participants: &'static str,
    summary: &'static str,
    decisions: &'static str,
    action_items: &'static str,
    open_questions: &'static str,
    key_frames: &'static str,
    transcript: &'static str,
    nothing: &'static str,
    no_note: &'static str,
    frames_not_analysed: &'static str,
    me: &'static str,
    others: &'static str,
    owner: &'static str,
    due: &'static str,
}

const TURKISH: Labels = Labels {
    date: "Tarih",
    duration: "Süre",
    participants: "Konuşmacılar",
    summary: "Özet",
    decisions: "Kararlar",
    action_items: "Aksiyon listesi",
    open_questions: "Açık sorular",
    key_frames: "Önemli ekran görüntüleri",
    transcript: "Transkript",
    nothing: "Yok",
    no_note: "Bu oturum için henüz not oluşturulmadı.",
    frames_not_analysed: "Ekran görüntüleri analiz edilmedi (görsel model atanmamış).",
    me: "Ben",
    others: "Diğerleri",
    owner: "sorumlu",
    due: "tarih",
};

const ENGLISH: Labels = Labels {
    date: "Date",
    duration: "Duration",
    participants: "Speakers",
    summary: "Summary",
    decisions: "Decisions",
    action_items: "Action items",
    open_questions: "Open questions",
    key_frames: "Key frames",
    transcript: "Transcript",
    nothing: "None",
    no_note: "No note has been generated for this session yet.",
    frames_not_analysed: "Screen frames were not analysed (no vision model is assigned).",
    me: "Me",
    others: "Others",
    owner: "owner",
    due: "due",
};

fn labels_for(language: &LanguageCode) -> &'static Labels {
    if language.as_str() == "tr" || language.as_str().starts_with("tr-") {
        &TURKISH
    } else {
        &ENGLISH
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn speaker_name(speaker: Option<&str>, labels: &Labels) -> String {
    match speaker {
        Some(SPEAKER_ME) => labels.me.to_owned(),
        Some(SPEAKER_OTHERS) => labels.others.to_owned(),
        Some(other) => one_line(other),
        None => "?".to_owned(),
    }
}

fn bullet_list(items: &[String], labels: &Labels, out: &mut String) {
    if items.is_empty() {
        let _ = writeln!(out, "_{}_", labels.nothing);
    }
    for item in items {
        let _ = writeln!(out, "- {}", one_line(item));
    }
}

/// A caption safe inside `![...]`.
fn caption(frame_description: Option<&str>, timestamp_ms: u64) -> String {
    let first_line = frame_description
        .and_then(|d| d.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|line| line.replace(['[', ']'], ""));
    match first_line {
        Some(line) => {
            let short: String = line.chars().take(MAX_CAPTION_CHARS).collect();
            format!("{} {short}", format_offset(timestamp_ms))
        }
        None => format_offset(timestamp_ms),
    }
}

/// Title, date, duration and speakers.
fn write_header(detail: &SessionDetail, labels: &Labels, out: &mut String) {
    let session = &detail.session;
    let _ = writeln!(
        out,
        "# {}
",
        one_line(&session.title)
    );
    let _ = writeln!(
        out,
        "- **{}:** {}",
        labels.date,
        utc_datetime_string(session.started_at)
    );
    if let Some(duration_ms) = session.duration_ms() {
        let _ = writeln!(
            out,
            "- **{}:** {}",
            labels.duration,
            format_offset(duration_ms)
        );
    }
    let mut speakers: Vec<String> = Vec::new();
    for segment in &detail.transcript {
        let name = speaker_name(segment.speaker.as_deref(), labels);
        if !speakers.contains(&name) {
            speakers.push(name);
        }
    }
    if !speakers.is_empty() {
        let _ = writeln!(
            out,
            "- **{}:** {}",
            labels.participants,
            speakers.join(", ")
        );
    }
    out.push('\n');
}

fn write_action_items(note: &NoteWithItems, labels: &Labels, out: &mut String) {
    if note.action_items.is_empty() {
        let _ = writeln!(out, "_{}_", labels.nothing);
    }
    for item in &note.action_items {
        let mark = if item.done { 'x' } else { ' ' };
        let mut details = Vec::new();
        if let Some(owner) = &item.owner {
            details.push(format!("{}: {}", labels.owner, one_line(owner)));
        }
        if let Some(due) = item.due {
            details.push(format!("{}: {}", labels.due, utc_date_string(due)));
        }
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!(" ({})", details.join(", "))
        };
        let _ = writeln!(out, "- [{mark}] {}{suffix}", one_line(&item.task));
    }
}

/// Summary, decisions, action items, open questions and key frames.
fn write_note(detail: &SessionDetail, note: &NoteWithItems, labels: &Labels, out: &mut String) {
    let _ = write!(
        out,
        "## {}

{}

",
        labels.summary,
        note.note.summary.trim()
    );
    let frames_not_analysed = !detail.frames.is_empty()
        && detail
            .job
            .as_ref()
            .is_some_and(|job| *job.state(ProcessingStep::DescribeFrames) == StepState::Skipped);
    if frames_not_analysed {
        let _ = write!(
            out,
            "> {}

",
            labels.frames_not_analysed
        );
    }

    let _ = write!(
        out,
        "## {}

",
        labels.decisions
    );
    bullet_list(&note.note.decisions, labels, out);

    let _ = write!(
        out,
        "
## {}

",
        labels.action_items
    );
    write_action_items(note, labels, out);

    let _ = write!(
        out,
        "
## {}

",
        labels.open_questions
    );
    bullet_list(&note.note.open_questions, labels, out);

    let key_frames: Vec<_> = note
        .note
        .frame_refs
        .iter()
        .filter_map(|id| detail.frames.iter().find(|frame| &frame.id == id))
        .collect();
    if !key_frames.is_empty() {
        let _ = write!(
            out,
            "
## {}

",
            labels.key_frames
        );
        for frame in key_frames {
            let _ = write!(
                out,
                "![{}]({})

",
                caption(frame.description.as_deref(), frame.timestamp_ms),
                frame.path.replace(' ', "%20")
            );
        }
    }
}

/// Renders the session as Markdown: title, date, speakers, summary, decisions, the action list
/// as `- [ ]` / `- [x]` lines, open questions, the key frames as image links (paths relative
/// to the session folder) and the time-stamped transcript. Headings follow the note language
/// (Turkish or English); the transcript stays in the spoken language.
#[must_use]
pub fn render_markdown(detail: &SessionDetail) -> String {
    let language = detail
        .note
        .as_ref()
        .map_or(&detail.session.note_language, |n| &n.note.language);
    let labels = labels_for(language);
    let mut out = String::new();

    write_header(detail, labels, &mut out);
    match &detail.note {
        None => {
            let _ = write!(
                out,
                "_{}_

",
                labels.no_note
            );
        }
        Some(note) => write_note(detail, note, labels, &mut out),
    }

    let _ = write!(
        out,
        "
## {}

",
        labels.transcript
    );
    for segment in &detail.transcript {
        let _ = write!(
            out,
            "**[{}] {}:** {}

",
            format_offset(segment.start_ms),
            speaker_name(segment.speaker.as_deref(), labels),
            one_line(&segment.text)
        );
    }
    out.trim_end().to_owned()
        + "
"
}

/// Writes the Markdown export of a session next to its frames.
pub struct ExportSessionMarkdown<W> {
    files: Arc<W>,
}

impl<W: SessionFileStore> ExportSessionMarkdown<W> {
    /// Creates the use case.
    #[must_use]
    pub fn new(files: Arc<W>) -> Self {
        Self { files }
    }

    /// Renders `detail` and stores it as [`EXPORT_FILE_NAME`] in the session folder; returns
    /// the absolute path of the file.
    pub fn execute(&self, detail: &SessionDetail) -> SessionsResult<PathBuf> {
        let markdown = render_markdown(detail);
        Ok(self
            .files
            .write_export(&detail.session.id, EXPORT_FILE_NAME, &markdown)?)
    }
}
