//! Markdown export of a session: the note, the key frames and the transcript.

use std::fmt::{self, Write as _};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;

use tan_domain::{Frame, LanguageCode, ProcessingStep, StepState};

use super::{NoteWithItems, SessionDetail, SessionFileStore, SessionsError, SessionsResult};
use crate::processing::{format_offset, utc_date_string, utc_datetime_string};
use crate::transcription::{SPEAKER_ME, SPEAKER_OTHERS};

/// Name of the exported file inside the session folder. Frame paths are relative to that
/// folder (`frames/...`), so the image links in the file work where it is.
pub const EXPORT_FILE_NAME: &str = "notes.md";

/// Appended to the Markdown file's name (without extension) to name the folder that holds the
/// key frame images when the user exports to a place of their choice.
pub const FRAMES_DIR_SUFFIX: &str = "_frames";

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
fn write_note(
    detail: &SessionDetail,
    note: &NoteWithItems,
    labels: &Labels,
    link: &dyn Fn(&Frame) -> Option<String>,
    out: &mut String,
) {
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

    let key_frames: Vec<_> = key_frames(detail, note)
        .into_iter()
        .filter_map(|frame| link(frame).map(|target| (frame, target)))
        .collect();
    if !key_frames.is_empty() {
        let _ = write!(
            out,
            "
## {}

",
            labels.key_frames
        );
        for (frame, target) in key_frames {
            let _ = write!(
                out,
                "![{}]({target})

",
                caption(frame.description.as_deref(), frame.timestamp_ms),
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
    render(detail, &|frame| Some(link_safe(&frame.path)))
}

/// The frames the note refers to, in the note's order.
fn key_frames<'a>(detail: &'a SessionDetail, note: &NoteWithItems) -> Vec<&'a Frame> {
    note.note
        .frame_refs
        .iter()
        .filter_map(|id| detail.frames.iter().find(|frame| &frame.id == id))
        .collect()
}

/// Escapes what would end or break a Markdown link target (the path stays readable).
fn link_safe(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            _ => out.push(c),
        }
    }
    out
}

/// Renders the session; `link` gives the image target of a key frame (`None`: leave it out).
fn render(detail: &SessionDetail, link: &dyn Fn(&Frame) -> Option<String>) -> String {
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
        Some(note) => write_note(detail, note, labels, link, &mut out),
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

    /// Renders `detail` into the Markdown file `destination` the user chose.
    ///
    /// The key frames the note refers to are copied into a folder next to the file, named after
    /// it (`notes.md` -> `notes_frames/`), and the image links point there relatively, so the
    /// file keeps working when it is moved or shared together with that folder and nothing in
    /// it points into Tan's data folder. A frame whose image is gone is left out of the file.
    ///
    /// `destination` must be absolute, free of `..`, on a local drive (no UNC or device paths)
    /// and end in `.md`. Returns the path written.
    pub fn execute_to(
        &self,
        detail: &SessionDetail,
        destination: &Path,
    ) -> SessionsResult<PathBuf> {
        let frames_dir_name = frames_dir_name(destination)?;
        let frames_dir = destination.with_file_name(&frames_dir_name);

        let mut available: Vec<(String, String)> = Vec::new(); // (frame id, link target)
        if let Some(note) = &detail.note {
            let mut dir_created = false;
            for frame in key_frames(detail, note) {
                let Some(file_name) = Path::new(&frame.path).file_name().and_then(|n| n.to_str())
                else {
                    continue;
                };
                let bytes = match self.files.read_frame(&detail.session.id, &frame.path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if !dir_created {
                    self.files.create_export_dir(&frames_dir)?;
                    dir_created = true;
                }
                self.files
                    .write_export_to(&frames_dir.join(file_name), &bytes)?;
                available.push((
                    frame.id.to_string(),
                    link_safe(&format!("{frames_dir_name}/{file_name}")),
                ));
            }
        }

        let markdown = render(detail, &|frame| {
            available
                .iter()
                .find(|(id, _)| id == frame.id.as_str())
                .map(|(_, target)| target.clone())
        });
        self.files
            .write_export_to(destination, markdown.as_bytes())?;
        Ok(destination.to_path_buf())
    }
}

/// Why a Markdown export destination was refused. Callers react to the variant; the text is
/// for logs and details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDestinationProblem {
    /// The path is not absolute.
    NotAbsolute,
    /// The path contains `..`.
    ParentReference,
    /// The path is a UNC, network or device path (only local drives are allowed).
    NetworkPath,
    /// The path has no file name.
    MissingName,
    /// The file has an extension other than `.md`.
    InvalidExtension,
    /// The path is not the one the user picked in the save dialog.
    NotDialogSelected,
    /// The path is inside Tan's own data folder.
    InsideDataDir,
    /// The folder the file should go into does not exist.
    FolderMissing,
    /// `.md` was appended to the picked name and a file with that name already exists (the
    /// dialog only confirmed replacing the name as picked).
    AlreadyExists,
}

impl fmt::Display for ExportDestinationProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "the path must be absolute",
            Self::ParentReference => "the path must not contain `..`",
            Self::NetworkPath => "network and device paths are not allowed",
            Self::MissingName => "the file must have a name",
            Self::InvalidExtension => "the file must have the .md extension",
            Self::NotDialogSelected => "the destination was not chosen in the save dialog",
            Self::InsideDataDir => "the destination must be outside Tan's data folder",
            Self::FolderMissing => "the destination's folder does not exist",
            Self::AlreadyExists => {
                "a file with that name and the .md extension already exists; pick another name"
            }
        })
    }
}

impl From<ExportDestinationProblem> for SessionsError {
    fn from(problem: ExportDestinationProblem) -> Self {
        Self::ExportDestination(problem)
    }
}

/// Where an export goes, derived from the path the user picked in the save dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDestination {
    /// The Markdown file to write.
    pub path: PathBuf,
    /// Whether `.md` was appended because the picked path had no extension (some desktops'
    /// save dialogs do not add one). The dialog only asked the user to confirm replacing the
    /// path as picked, so the caller should not silently replace an existing `path` then.
    pub extension_added: bool,
}

/// Checks the path picked in the save dialog without touching the file system, and completes
/// a missing extension to `.md`.
///
/// The path must be absolute, free of `..`, on a local drive (no UNC or device paths) and
/// either end in `.md` or have no extension at all; any other extension is refused (the user
/// asked for a file that is not Markdown).
pub fn resolve_export_destination(chosen: &Path) -> SessionsResult<ExportDestination> {
    check_destination_path(chosen)?;
    if chosen.extension().is_some() {
        frames_dir_name(chosen)?;
        return Ok(ExportDestination {
            path: chosen.to_path_buf(),
            extension_added: false,
        });
    }
    // `with_extension` would also be right for names ending in a dot or without one; keep the
    // picked name as is and append.
    let mut name = chosen.as_os_str().to_os_string();
    name.push(".md");
    let path = PathBuf::from(name);
    frames_dir_name(&path)?;
    Ok(ExportDestination {
        path,
        extension_added: true,
    })
}

/// Checks `destination` and returns the name of the folder for the frame images.
fn frames_dir_name(destination: &Path) -> SessionsResult<String> {
    check_destination_path(destination)?;
    let is_markdown = destination
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    let stem = destination.file_stem().and_then(|stem| stem.to_str());
    match stem {
        Some(stem) if !stem.is_empty() && is_markdown => Ok(format!("{stem}{FRAMES_DIR_SUFFIX}")),
        Some(stem) if !stem.is_empty() => Err(ExportDestinationProblem::InvalidExtension.into()),
        _ => Err(ExportDestinationProblem::MissingName.into()),
    }
}

/// The part of the destination checks that does not depend on the extension.
fn check_destination_path(destination: &Path) -> SessionsResult<()> {
    if !destination.is_absolute() {
        return Err(ExportDestinationProblem::NotAbsolute.into());
    }
    // Only local drives: a UNC or device path would make the write open a network connection
    // (and leak credentials to whoever runs that server).
    if let Some(Component::Prefix(prefix)) = destination.components().next() {
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
            return Err(ExportDestinationProblem::NetworkPath.into());
        }
    }
    if destination
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ExportDestinationProblem::ParentReference.into());
    }
    Ok(())
}
