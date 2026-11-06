//! Markdown export of a session.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tan_application::sessions::{
    render_markdown, ExportSessionMarkdown, NoteWithItems, SessionDetail, SessionFileStore,
    StorageUsage, EXPORT_FILE_NAME,
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
}

impl SessionFileStore for RecordingFiles {
    fn read_frame(&self, _: &SessionId, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other("not used"))
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
