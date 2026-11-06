//! Markdown export of a session.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tan_application::sessions::{
    render_markdown, resolve_export_destination, ExportDestinationProblem, ExportSessionMarkdown,
    NoteWithItems, SessionDetail, SessionFileStore, SessionsError, StorageUsage, EXPORT_FILE_NAME,
};
use tan_domain::{
    ActionItem, ActionItemId, EpochMs, Frame, FrameId, LanguageCode, LanguageSetting, Note, NoteId,
    ProcessingJob, ProcessingStep, Session, SessionId, SessionKind, SessionRecord, SessionStatus,
    TrackKind, TranscriptSegment,
};

const STARTED_AT: i64 = 1_793_959_200_000; // 2026-11-06 10:00 UTC
const DUE: i64 = 1_796_083_200_000; // 2026-12-01

fn session(note_language: LanguageCode) -> Session {
    Session::restore(SessionRecord {
        id: SessionId::new("s1"),
        kind: SessionKind::Meeting,
        title: "Bütçe\ntoplantısı".to_owned(),
        started_at: EpochMs::new(STARTED_AT),
        ended_at: Some(EpochMs::new(STARTED_AT + 2_535_000)),
        language: LanguageSetting::Auto,
        note_language,
        status: SessionStatus::Processed,
    })
    .unwrap()
}

fn frame(id: &str, timestamp_ms: u64, path: &str, description: Option<&str>) -> Frame {
    Frame {
        id: FrameId::new(id),
        session_id: SessionId::new("s1"),
        timestamp_ms,
        path: path.to_owned(),
        perceptual_hash: 0,
        description: description.map(str::to_owned),
    }
}

fn segment(
    track: TrackKind,
    start_ms: u64,
    speaker: Option<&str>,
    text: &str,
) -> TranscriptSegment {
    TranscriptSegment {
        session_id: SessionId::new("s1"),
        track,
        start_ms,
        end_ms: start_ms + 1_000,
        language: None,
        text: text.to_owned(),
        speaker: speaker.map(str::to_owned),
    }
}

fn detail(note_language: LanguageCode) -> SessionDetail {
    let note_id = NoteId::new("n1");
    let item =
        |id: &str, task: &str, owner: Option<&str>, due: Option<i64>, done: bool| ActionItem {
            id: ActionItemId::new(id),
            note_id: note_id.clone(),
            task: task.to_owned(),
            owner: owner.map(str::to_owned),
            due: due.map(EpochMs::new),
            done,
        };
    SessionDetail {
        session: session(note_language.clone()),
        audio_tracks: Vec::new(),
        frames: vec![
            frame("f1", 3_000, "frames/a.png", Some("Açılış slaytı")),
            frame(
                "f2",
                6_000,
                "frames/b c.png",
                Some("Fiyat [tablosu]\nText on screen: Plan A 10 TL"),
            ),
        ],
        transcript: vec![
            segment(
                TrackKind::Microphone,
                0,
                Some("me"),
                "Merhaba,\nbaşlayalım.",
            ),
            segment(
                TrackKind::System,
                5_000,
                Some("others"),
                "Bütçeyi onaylıyoruz.",
            ),
            segment(
                TrackKind::System,
                8_000,
                Some("Ayşe Hanım"),
                "Raporu ben hazırlarım.",
            ),
        ],
        note: Some(NoteWithItems {
            note: Note {
                id: note_id.clone(),
                session_id: SessionId::new("s1"),
                language: note_language,
                summary: "Bütçe görüşüldü ve onaylandı.".to_owned(),
                decisions: vec!["Bütçe onaylandı".to_owned()],
                open_questions: vec!["Lansman tarihi nedir?".to_owned()],
                frame_refs: vec![FrameId::new("f2")],
                created_at: EpochMs::new(STARTED_AT + 3_000_000),
            },
            action_items: vec![
                item("a1", "Raporu hazırla", Some("Ayşe"), Some(DUE), false),
                item("a2", "Toplantı odasını ayır", None, None, true),
            ],
        }),
        job: None,
    }
}

#[test]
fn the_turkish_export_has_every_section_and_checklist_lines() {
    let md = render_markdown(&detail(LanguageCode::turkish()));

    for expected in [
        "# Bütçe toplantısı\n",
        "- **Tarih:** 2026-11-06 10:00 UTC",
        "- **Süre:** 00:42:15",
        "- **Konuşmacılar:** Ben, Diğerleri, Ayşe Hanım",
        "## Özet\n\nBütçe görüşüldü ve onaylandı.",
        "## Kararlar\n\n- Bütçe onaylandı",
        "## Aksiyon listesi\n",
        "- [ ] Raporu hazırla (sorumlu: Ayşe, tarih: 2026-12-01)\n",
        "- [x] Toplantı odasını ayır\n",
        "## Açık sorular\n\n- Lansman tarihi nedir?",
        "## Önemli ekran görüntüleri\n",
        "## Transkript\n",
        "**[00:00:00] Ben:** Merhaba, başlayalım.",
        "**[00:00:05] Diğerleri:** Bütçeyi onaylıyoruz.",
        "**[00:00:08] Ayşe Hanım:** Raporu ben hazırlarım.",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
    // Sections come in the documented order.
    let order = [
        "## Özet",
        "## Kararlar",
        "## Aksiyon listesi",
        "## Açık sorular",
        "## Önemli ekran görüntüleri",
        "## Transkript",
    ];
    let positions: Vec<usize> = order.iter().map(|h| md.find(h).unwrap()).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
}

#[test]
fn key_frames_are_image_links_relative_to_the_session_folder() {
    let md = render_markdown(&detail(LanguageCode::turkish()));
    // Only the referenced frame, captioned with its first line (brackets stripped), the path
    // kept relative and made link-safe.
    assert!(
        md.contains("![00:00:06 Fiyat tablosu](frames/b%20c.png)"),
        "{md}"
    );
    assert!(
        !md.contains("a.png"),
        "frames the note does not reference are left out"
    );
    assert!(
        !md.contains("Text on screen"),
        "the caption is a single line"
    );
}

#[test]
fn the_english_export_uses_english_headings() {
    let md = render_markdown(&detail(LanguageCode::english()));
    for expected in [
        "## Summary",
        "## Decisions",
        "## Action items",
        "- [ ] Raporu hazırla (owner: Ayşe, due: 2026-12-01)",
        "- [x] Toplantı odasını ayır",
        "## Open questions",
        "## Key frames",
        "## Transcript",
        "**[00:00:00] Me:** Merhaba, başlayalım.",
        "- **Speakers:** Me, Others, Ayşe Hanım",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
    assert!(!md.contains("## Özet"));
}

#[test]
fn empty_note_sections_say_so_instead_of_disappearing() {
    let mut d = detail(LanguageCode::english());
    let note = d.note.as_mut().unwrap();
    note.note.decisions.clear();
    note.note.open_questions.clear();
    note.note.frame_refs.clear();
    note.action_items.clear();
    let md = render_markdown(&d);
    assert!(md.contains("## Decisions\n\n_None_"));
    assert!(md.contains("## Action items\n\n_None_"));
    assert!(md.contains("## Open questions\n\n_None_"));
    assert!(!md.contains("## Key frames"));
    assert!(!md.contains("- [ ]") && !md.contains("- [x]"));
}

#[test]
fn a_session_without_a_note_still_exports_the_transcript() {
    let mut d = detail(LanguageCode::turkish());
    d.note = None;
    let md = render_markdown(&d);
    assert!(md.contains("# Bütçe toplantısı"));
    assert!(md.contains("Bu oturum için henüz not oluşturulmadı."));
    assert!(md.contains("## Transkript"));
    assert!(md.contains("Bütçeyi onaylıyoruz."));
    assert!(!md.contains("## Özet"));
}

fn job_with_skipped_frames() -> ProcessingJob {
    let mut job = ProcessingJob::new(SessionId::new("s1"), EpochMs::new(1));
    job.start_step(ProcessingStep::Transcribe, EpochMs::new(2))
        .unwrap();
    job.complete_step(ProcessingStep::Transcribe, EpochMs::new(3))
        .unwrap();
    job.start_step(ProcessingStep::DescribeFrames, EpochMs::new(4))
        .unwrap();
    job.skip_step(ProcessingStep::DescribeFrames, EpochMs::new(5))
        .unwrap();
    job
}

#[test]
fn the_note_says_when_the_screen_frames_were_not_analysed() {
    let mut d = detail(LanguageCode::turkish());
    d.job = Some(job_with_skipped_frames());
    let md = render_markdown(&d);
    assert!(
        md.contains("> Ekran görüntüleri analiz edilmedi (görsel model atanmamış)."),
        "{md}"
    );

    d.note.as_mut().unwrap().note.language = LanguageCode::english();
    assert!(render_markdown(&d).contains("> Screen frames were not analysed"));

    // No notice when there were no frames, or when they were analysed.
    d.frames.clear();
    assert!(!render_markdown(&d).contains("not analysed"));
    let mut analysed = detail(LanguageCode::english());
    analysed.job = Some(ProcessingJob::new(SessionId::new("s1"), EpochMs::new(1)));
    assert!(!render_markdown(&analysed).contains("not analysed"));
}

#[derive(Default)]
struct RecordingFiles {
    written: Mutex<Vec<(String, String, String)>>,
    // Paths written with `write_export_to` and the bytes.
    external: Mutex<Vec<(PathBuf, Vec<u8>)>>,
    dirs: Mutex<Vec<PathBuf>>,
    // Frame images that exist, by relative path.
    images: Mutex<Vec<String>>,
}

impl SessionFileStore for RecordingFiles {
    fn read_frame(&self, _: &SessionId, relative_path: &str) -> io::Result<Vec<u8>> {
        if self
            .images
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == relative_path)
        {
            Ok(format!("image of {relative_path}").into_bytes())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such frame"))
        }
    }

    fn audio_exists(&self, _: &SessionId, _: &str) -> bool {
        false
    }

    fn delete_audio(&self, _: &SessionId, _: &str) -> io::Result<u64> {
        Ok(0)
    }

    fn remove_session(&self, _: &SessionId) -> io::Result<()> {
        Ok(())
    }

    fn write_export(
        &self,
        session_id: &SessionId,
        file_name: &str,
        content: &str,
    ) -> io::Result<PathBuf> {
        self.written.lock().unwrap().push((
            session_id.to_string(),
            file_name.to_owned(),
            content.to_owned(),
        ));
        Ok(PathBuf::from(format!(
            "/data/sessions/{session_id}/{file_name}"
        )))
    }

    fn create_export_dir(&self, dir: &Path) -> io::Result<()> {
        self.dirs.lock().unwrap().push(dir.to_path_buf());
        Ok(())
    }

    fn write_export_to(&self, destination: &Path, content: &[u8]) -> io::Result<()> {
        self.external
            .lock()
            .unwrap()
            .push((destination.to_path_buf(), content.to_vec()));
        Ok(())
    }

    fn measure_usage(&self) -> io::Result<StorageUsage> {
        Ok(StorageUsage::default())
    }
}

#[test]
fn exporting_writes_the_markdown_next_to_the_frames_and_returns_the_path() {
    let files = Arc::new(RecordingFiles::default());
    let export = ExportSessionMarkdown::new(files.clone());
    let d = detail(LanguageCode::turkish());

    let path = export.execute(&d).unwrap();
    assert_eq!(path, PathBuf::from("/data/sessions/s1/notes.md"));
    let written = files.written.lock().unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].0, "s1");
    assert_eq!(written[0].1, EXPORT_FILE_NAME);
    assert_eq!(written[0].2, render_markdown(&d));
}

fn destination() -> PathBuf {
    std::env::temp_dir().join("my notes (final).md")
}

#[test]
fn exporting_to_a_chosen_file_copies_the_key_frames_next_to_it_and_links_them_relatively() {
    let files = Arc::new(RecordingFiles::default());
    files
        .images
        .lock()
        .unwrap()
        .push("frames/b c.png".to_owned());
    let export = ExportSessionMarkdown::new(files.clone());
    let d = detail(LanguageCode::turkish());
    let target = destination();

    let written = export.execute_to(&d, &target).unwrap();
    assert_eq!(written, target);

    let frames_dir = std::env::temp_dir().join("my notes (final)_frames");
    assert_eq!(*files.dirs.lock().unwrap(), vec![frames_dir.clone()]);
    let external = files.external.lock().unwrap();
    assert_eq!(external.len(), 2, "one image and the file");
    assert_eq!(external[0].0, frames_dir.join("b c.png"));
    assert_eq!(external[0].1, b"image of frames/b c.png");
    assert_eq!(external[1].0, target);
    let md = String::from_utf8(external[1].1.clone()).unwrap();
    assert!(
        md.contains("(my%20notes%20%28final%29_frames/b%20c.png)"),
        "{md}"
    );
    assert!(
        !md.contains("(frames/"),
        "no link points into the data folder"
    );
    assert!(
        files.written.lock().unwrap().is_empty(),
        "nothing is written into the session folder"
    );
}

#[test]
fn a_key_frame_whose_image_is_gone_is_left_out_of_the_chosen_export() {
    let files = Arc::new(RecordingFiles::default());
    let export = ExportSessionMarkdown::new(files.clone());
    export
        .execute_to(&detail(LanguageCode::turkish()), &destination())
        .unwrap();
    assert!(
        files.dirs.lock().unwrap().is_empty(),
        "no folder without images"
    );
    let external = files.external.lock().unwrap();
    assert_eq!(external.len(), 1);
    let md = String::from_utf8(external[0].1.clone()).unwrap();
    assert!(!md.contains("!["), "{md}");
    assert!(md.contains("## Transkript"));
}

#[test]
fn an_export_destination_must_be_an_absolute_md_path_without_parent_references() {
    let files = Arc::new(RecordingFiles::default());
    let export = ExportSessionMarkdown::new(files.clone());
    let d = detail(LanguageCode::turkish());
    let temp = std::env::temp_dir();
    for bad in [
        PathBuf::from("notes.md"),
        PathBuf::from("relative/notes.md"),
        temp.join("notes.txt"),
        temp.join("notes"),
        temp.join(".md"),
        temp.join("..").join("notes.md"),
        temp.clone(),
    ] {
        let error = export.execute_to(&d, &bad).unwrap_err();
        assert!(
            matches!(error, SessionsError::ExportDestination(_)),
            "{bad:?}: {error}"
        );
    }
    assert!(files.external.lock().unwrap().is_empty());
    assert!(files.dirs.lock().unwrap().is_empty());
    // The extension check ignores case.
    export.execute_to(&d, &temp.join("NOTES.MD")).unwrap();
}

#[cfg(windows)]
#[test]
fn network_and_device_paths_are_refused_as_export_destinations() {
    let files = Arc::new(RecordingFiles::default());
    let export = ExportSessionMarkdown::new(files.clone());
    let d = detail(LanguageCode::turkish());
    for bad in [
        r"\\attacker\share\notes.md",
        r"\\?\UNC\attacker\share\notes.md",
        r"\\.\C:\notes.md",
        r"\\?\GLOBALROOT\notes.md",
    ] {
        let error = export.execute_to(&d, Path::new(bad)).unwrap_err();
        assert!(
            matches!(error, SessionsError::ExportDestination(_)),
            "{bad}: {error}"
        );
    }
    assert!(files.external.lock().unwrap().is_empty());
    assert!(files.dirs.lock().unwrap().is_empty());
    // A local drive, plain or in the verbatim form, is fine.
    export
        .execute_to(&d, Path::new(r"C:\Users\me\notes.md"))
        .unwrap();
    export
        .execute_to(&d, Path::new(r"\\?\C:\Users\me\notes.md"))
        .unwrap();
}

#[test]
fn a_picked_path_without_an_extension_gets_md_and_a_foreign_extension_is_refused() {
    let temp = std::env::temp_dir();

    // The path as the dialog returned it is kept when it already ends in `.md`.
    let picked = temp.join("notes.md");
    let resolved = resolve_export_destination(&picked).unwrap();
    assert_eq!((resolved.path, resolved.extension_added), (picked, false));
    let upper = temp.join("NOTES.MD");
    assert!(!resolve_export_destination(&upper).unwrap().extension_added);

    // No extension (a Linux dialog that adds none): `.md` is appended to the name as picked.
    let resolved = resolve_export_destination(&temp.join("toplanti")).unwrap();
    assert_eq!(resolved.path, temp.join("toplanti.md"));
    assert!(resolved.extension_added);
    let resolved = resolve_export_destination(&temp.join("meeting v2 (final)")).unwrap();
    assert_eq!(resolved.path, temp.join("meeting v2 (final).md"));

    // Another extension is a refusal, not a silent rename.
    for bad in ["notes.txt", "notes.markdown", "archive.tar.gz"] {
        let error = resolve_export_destination(&temp.join(bad)).unwrap_err();
        assert!(
            matches!(error, SessionsError::ExportDestination(_)),
            "{bad}: {error}"
        );
    }
    // The shape checks run before and regardless of the extension.
    for bad in [
        PathBuf::from("toplanti"),
        temp.join("..").join("toplanti"),
        temp.clone().join(".."),
    ] {
        assert!(
            matches!(
                resolve_export_destination(&bad),
                Err(SessionsError::ExportDestination(_))
            ),
            "{bad:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn network_paths_are_refused_by_the_pure_check_before_any_file_system_access() {
    for bad in [
        r"\\attacker\share\notes",
        r"\\attacker\share\notes.md",
        r"\\.\C:\notes",
    ] {
        let error = resolve_export_destination(Path::new(bad)).unwrap_err();
        assert!(
            matches!(error, SessionsError::ExportDestination(_)),
            "{bad}: {error}"
        );
    }
}

fn problem_of(path: &Path) -> ExportDestinationProblem {
    match resolve_export_destination(path) {
        Err(SessionsError::ExportDestination(problem)) => problem,
        other => panic!("{}: expected a refusal, got {other:?}", path.display()),
    }
}

#[test]
fn every_refusal_carries_its_own_typed_reason() {
    let temp = std::env::temp_dir();
    assert_eq!(
        problem_of(Path::new("notes.md")),
        ExportDestinationProblem::NotAbsolute
    );
    assert_eq!(
        problem_of(&temp.join("..").join("notes.md")),
        ExportDestinationProblem::ParentReference
    );
    assert_eq!(
        problem_of(&temp.join("notes.txt")),
        ExportDestinationProblem::InvalidExtension
    );
    assert_eq!(
        problem_of(&temp.join("notes.markdown")),
        ExportDestinationProblem::InvalidExtension
    );
}

#[cfg(windows)]
#[test]
fn a_network_path_has_its_own_reason() {
    for bad in [r"\\attacker\share\notes.md", r"\\.\C:\notes.md"] {
        assert_eq!(
            problem_of(Path::new(bad)),
            ExportDestinationProblem::NetworkPath,
            "{bad}"
        );
    }
}

#[test]
fn the_reason_shows_in_the_error_text_and_converts_into_the_error() {
    let error: SessionsError = ExportDestinationProblem::AlreadyExists.into();
    assert!(matches!(
        error,
        SessionsError::ExportDestination(ExportDestinationProblem::AlreadyExists)
    ));
    assert!(error.to_string().contains("already exists"), "{error}");
}
