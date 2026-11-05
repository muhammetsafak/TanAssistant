//! Recording use case against in-memory fakes of every port.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_executor::block_on;
use tan_application::ports::{
    AudioCapture, AudioDevice, AudioFileInfo, AudioFormat, AudioSink, AudioStream,
    AudioStreamRequest, AudioWriter, CaptureError, CaptureErrorKind, CaptureResult, Clock,
    DisplayInfo, FrameHasher, FrameRepository, FrameSink, IdGenerator, MediaStore, PcmChunk,
    RawFrame, ScreenAvailability, ScreenCapture, ScreenStream, ScreenStreamRequest,
    ScreenUnavailableReason, SessionRepository, SettingsRepository,
};
use tan_application::recording::{
    RecordingError, RecordingEvent, RecordingEvents, RecordingPorts, RecordingService,
    RecordingStatus, StartOptions,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    AppSettings, AudioTrack, EpochMs, Frame, FrameId, LanguageCode, LanguageSetting, Session,
    SessionId, SessionKind, SessionStatus, SettingsPatch, TrackKind,
};

// ---- fakes -----------------------------------------------------------------------------

#[derive(Default)]
struct MemSessions {
    sessions: Mutex<BTreeMap<String, Session>>,
    tracks: Mutex<Vec<AudioTrack>>,
}

impl MemSessions {
    fn get_now(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(id).cloned()
    }
}

impl SessionRepository for MemSessions {
    async fn create(&self, session: &Session) -> RepositoryResult<()> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.contains_key(session.id.as_str()) {
            return Err(RepositoryError::Conflict("duplicate session".to_owned()));
        }
        sessions.insert(session.id.to_string(), session.clone());
        Ok(())
    }

    async fn update(&self, session: &Session) -> RepositoryResult<()> {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get_mut(session.id.as_str()) {
            Some(slot) => {
                *slot = session.clone();
                Ok(())
            }
            None => Err(RepositoryError::not_found(
                "session",
                session.id.to_string(),
            )),
        }
    }

    async fn get(&self, id: &SessionId) -> RepositoryResult<Option<Session>> {
        Ok(self.get_now(id.as_str()))
    }

    async fn list(&self) -> RepositoryResult<Vec<Session>> {
        Ok(self.sessions.lock().unwrap().values().cloned().collect())
    }

    async fn delete(&self, id: &SessionId) -> RepositoryResult<()> {
        self.sessions.lock().unwrap().remove(id.as_str());
        Ok(())
    }

    async fn save_audio_track(&self, track: &AudioTrack) -> RepositoryResult<()> {
        let mut tracks = self.tracks.lock().unwrap();
        tracks.retain(|t| !(t.session_id == track.session_id && t.kind == track.kind));
        tracks.push(track.clone());
        Ok(())
    }

    async fn list_audio_tracks(&self, session_id: &SessionId) -> RepositoryResult<Vec<AudioTrack>> {
        Ok(self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| &t.session_id == session_id)
            .cloned()
            .collect())
    }
}

#[derive(Default)]
struct MemFrames(Mutex<Vec<Frame>>);

impl FrameRepository for MemFrames {
    async fn add(&self, frame: &Frame) -> RepositoryResult<()> {
        self.0.lock().unwrap().push(frame.clone());
        Ok(())
    }

    async fn get(&self, id: &FrameId) -> RepositoryResult<Option<Frame>> {
        Ok(self.0.lock().unwrap().iter().find(|f| &f.id == id).cloned())
    }

    async fn list_for_session(&self, session_id: &SessionId) -> RepositoryResult<Vec<Frame>> {
        let mut frames: Vec<Frame> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|f| &f.session_id == session_id)
            .cloned()
            .collect();
        frames.sort_by_key(|f| f.timestamp_ms);
        Ok(frames)
    }

    async fn set_description(
        &self,
        _id: &FrameId,
        _description: Option<&str>,
    ) -> RepositoryResult<()> {
        Ok(())
    }

    async fn delete(&self, id: &FrameId) -> RepositoryResult<()> {
        self.0.lock().unwrap().retain(|f| &f.id != id);
        Ok(())
    }
}

struct MemSettings(AppSettings);

impl SettingsRepository for MemSettings {
    async fn load(&self) -> RepositoryResult<AppSettings> {
        Ok(self.0.clone())
    }

    async fn update(&self, patch: &SettingsPatch) -> RepositoryResult<AppSettings> {
        self.0
            .patched(patch)
            .map_err(|e| RepositoryError::Invalid(e.to_string()))
    }
}

#[derive(Default)]
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    fn new_id(&self) -> String {
        format!("id-{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> EpochMs {
        EpochMs::new(1_700_000_000_000)
    }
}

#[derive(Default)]
struct CollectedEvents(Mutex<Vec<RecordingEvent>>);

impl RecordingEvents for CollectedEvents {
    fn publish(&self, event: RecordingEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl CollectedEvents {
    fn all(&self) -> Vec<RecordingEvent> {
        self.0.lock().unwrap().clone()
    }
}

const MONO_48K: AudioFormat = AudioFormat {
    sample_rate: 48_000,
    channels: 1,
};

struct FakeStream {
    stopped: Arc<AtomicBool>,
}

impl AudioStream for FakeStream {
    fn format(&self) -> AudioFormat {
        MONO_48K
    }

    fn device_name(&self) -> &'static str {
        "fake device"
    }

    fn stop(self: Box<Self>) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl ScreenStream for FakeStream {
    fn stop(self: Box<Self>) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct FakeAudio {
    sinks: Mutex<HashMap<TrackKind, Arc<dyn AudioSink>>>,
    failing: Mutex<Vec<TrackKind>>,
    stopped: Arc<AtomicBool>,
}

impl FakeAudio {
    fn sink(&self, kind: TrackKind) -> Arc<dyn AudioSink> {
        self.sinks.lock().unwrap()[&kind].clone()
    }
}

impl AudioCapture for FakeAudio {
    fn list_input_devices(&self) -> CaptureResult<Vec<AudioDevice>> {
        Ok(Vec::new())
    }

    fn start(
        &self,
        request: &AudioStreamRequest,
        sink: Arc<dyn AudioSink>,
    ) -> CaptureResult<Box<dyn AudioStream>> {
        if self.failing.lock().unwrap().contains(&request.source) {
            return Err(CaptureError::new(
                CaptureErrorKind::PermissionDenied,
                "access denied",
            ));
        }
        self.sinks.lock().unwrap().insert(request.source, sink);
        Ok(Box::new(FakeStream {
            stopped: self.stopped.clone(),
        }))
    }
}

#[derive(Default)]
struct FakeScreen {
    sink: Mutex<Option<Arc<dyn FrameSink>>>,
    fail: AtomicBool,
    unavailable: AtomicBool,
    started_with: Mutex<Option<ScreenStreamRequest>>,
}

impl ScreenCapture for FakeScreen {
    fn availability(&self) -> ScreenAvailability {
        if self.unavailable.load(Ordering::SeqCst) {
            ScreenAvailability::Unavailable(ScreenUnavailableReason::WaylandUnsupported)
        } else {
            ScreenAvailability::Available
        }
    }

    fn list_displays(&self) -> CaptureResult<Vec<DisplayInfo>> {
        Ok(Vec::new())
    }

    fn capture_once(&self, _display_id: Option<&str>) -> CaptureResult<RawFrame> {
        Ok(frame_with_hash(0, Instant::now()))
    }

    fn start(
        &self,
        request: &ScreenStreamRequest,
        sink: Arc<dyn FrameSink>,
    ) -> CaptureResult<Box<dyn ScreenStream>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(CaptureError::unsupported("no display server"));
        }
        *self.sink.lock().unwrap() = Some(sink);
        *self.started_with.lock().unwrap() = Some(request.clone());
        Ok(Box::new(FakeStream {
            stopped: Arc::default(),
        }))
    }
}

/// A tiny frame whose first eight bytes carry the hash [`FakeHasher`] returns.
fn frame_with_hash(hash: u64, at: Instant) -> RawFrame {
    let mut rgba = hash.to_le_bytes().to_vec();
    rgba.extend_from_slice(&[0; 8]);
    RawFrame {
        captured_at: at,
        width: 2,
        height: 2,
        rgba,
    }
}

struct FakeHasher;

impl FrameHasher for FakeHasher {
    fn hash(&self, frame: &RawFrame) -> u64 {
        u64::from_le_bytes(frame.rgba[..8].try_into().unwrap())
    }
}

#[derive(Default)]
struct WriterLog {
    format: Option<AudioFormat>,
    sample_frames: u64,
    silence_frames: u64,
    checkpoints: u32,
    finished: bool,
}

#[derive(Default)]
struct FakeMedia {
    writers: Mutex<HashMap<TrackKind, Arc<Mutex<WriterLog>>>>,
    saved_frames: Mutex<Vec<(u64, String)>>,
    prepared: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    on_disk: Mutex<HashMap<(String, TrackKind), AudioFileInfo>>,
}

impl FakeMedia {
    fn log(&self, kind: TrackKind) -> Arc<Mutex<WriterLog>> {
        self.writers.lock().unwrap()[&kind].clone()
    }
}

struct FakeWriter {
    log: Arc<Mutex<WriterLog>>,
    kind: TrackKind,
}

impl AudioWriter for FakeWriter {
    fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        let mut log = self.log.lock().unwrap();
        let channels = usize::from(log.format.unwrap().channels);
        log.sample_frames += (samples.len() / channels) as u64;
        Ok(())
    }

    fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        self.log.lock().unwrap().silence_frames += frames;
        Ok(())
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        self.log.lock().unwrap().checkpoints += 1;
        Ok(())
    }

    fn finish(self: Box<Self>) -> io::Result<AudioFileInfo> {
        let mut log = self.log.lock().unwrap();
        log.finished = true;
        let format = log.format.unwrap();
        Ok(AudioFileInfo {
            relative_path: format!("audio/{}.wav", self.kind),
            sample_rate: format.sample_rate,
            channels: format.channels,
            frames: log.sample_frames + log.silence_frames,
        })
    }
}

impl MediaStore for FakeMedia {
    fn prepare_session(&self, session_id: &SessionId) -> io::Result<()> {
        self.prepared.lock().unwrap().push(session_id.to_string());
        Ok(())
    }

    fn remove_session(&self, session_id: &SessionId) -> io::Result<()> {
        self.removed.lock().unwrap().push(session_id.to_string());
        Ok(())
    }

    fn open_audio_writer(
        &self,
        _session_id: &SessionId,
        kind: TrackKind,
        format: AudioFormat,
    ) -> io::Result<Box<dyn AudioWriter>> {
        let log = Arc::new(Mutex::new(WriterLog {
            format: Some(format),
            ..WriterLog::default()
        }));
        self.writers.lock().unwrap().insert(kind, log.clone());
        Ok(Box::new(FakeWriter { log, kind }))
    }

    fn probe_audio(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
    ) -> io::Result<Option<AudioFileInfo>> {
        Ok(self
            .on_disk
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), kind))
            .cloned())
    }

    fn save_frame(
        &self,
        _session_id: &SessionId,
        timestamp_ms: u64,
        _frame: &RawFrame,
    ) -> io::Result<String> {
        let path = format!("frames/frame-{timestamp_ms:09}.png");
        self.saved_frames
            .lock()
            .unwrap()
            .push((timestamp_ms, path.clone()));
        Ok(path)
    }
}

// ---- harness ---------------------------------------------------------------------------

struct Harness {
    service: RecordingService<MemSessions, MemFrames, MemSettings>,
    sessions: Arc<MemSessions>,
    frames: Arc<MemFrames>,
    audio: Arc<FakeAudio>,
    screen: Arc<FakeScreen>,
    media: Arc<FakeMedia>,
    events: Arc<CollectedEvents>,
}

fn harness_with(settings: AppSettings) -> Harness {
    let sessions = Arc::new(MemSessions::default());
    let frames = Arc::new(MemFrames::default());
    let audio = Arc::new(FakeAudio::default());
    let screen = Arc::new(FakeScreen::default());
    let media = Arc::new(FakeMedia::default());
    let events = Arc::new(CollectedEvents::default());
    let service = RecordingService::new(
        sessions.clone(),
        frames.clone(),
        Arc::new(MemSettings(settings)),
        RecordingPorts {
            audio: audio.clone(),
            screen: screen.clone(),
            hasher: Arc::new(FakeHasher),
            media: media.clone(),
            ids: Arc::new(SequentialIds::default()),
            clock: Arc::new(FixedClock),
            events: events.clone(),
        },
    );
    Harness {
        service,
        sessions,
        frames,
        audio,
        screen,
        media,
        events,
    }
}

fn harness() -> Harness {
    harness_with(AppSettings::default())
}

fn chunk(frames: usize, amplitude: f32, at: Instant) -> PcmChunk {
    PcmChunk {
        format: MONO_48K,
        captured_at: at,
        samples: vec![amplitude; frames],
    }
}

fn recording_session_id(h: &Harness) -> String {
    match h.service.status() {
        RecordingStatus::Recording(active) => active.session_id.to_string(),
        RecordingStatus::Idle => panic!("expected a running recording"),
    }
}

// ---- tests -----------------------------------------------------------------------------

#[test]
fn start_and_stop_store_both_tracks_and_only_changed_frames() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("Weekly sync"))).unwrap();
    assert_eq!(session.status(), SessionStatus::Recording);
    assert_eq!(session.kind, SessionKind::Meeting);
    assert_eq!(session.title, "Weekly sync");
    assert_eq!(
        h.sessions.get_now(session.id.as_str()).unwrap().status(),
        SessionStatus::Recording
    );
    assert_eq!(h.media.prepared.lock().unwrap().len(), 1);

    let mic = h.audio.sink(TrackKind::Microphone);
    let system = h.audio.sink(TrackKind::System);
    for _ in 0..10 {
        mic.on_chunk(chunk(480, 0.25, Instant::now()));
        system.on_chunk(chunk(480, 0.1, Instant::now()));
    }

    // Frames: first, an identical one, then a very different one, then identical to that.
    let screen = h.screen.sink.lock().unwrap().clone().unwrap();
    let t0 = Instant::now();
    for (hash, seconds) in [(0, 0), (0, 3), (u64::MAX, 6), (u64::MAX, 9)] {
        screen.on_frame(frame_with_hash(hash, t0 + Duration::from_secs(seconds)));
        // The sink drops frames when the worker is busy (by design); real captures are
        // seconds apart, so give the worker a moment between the test's instant pushes.
        std::thread::sleep(Duration::from_millis(50));
    }

    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(summary.session.ended_at().is_some());
    assert_eq!(summary.tracks.len(), 2);
    for track in &summary.tracks {
        assert!(track.peak > 0.0);
        assert_eq!(track.sample_rate, 48_000);
    }
    assert_eq!(summary.frame_count, 2, "identical frames are not stored");

    // The session is persisted as Recorded with one row per track.
    let stored = h.sessions.get_now(session.id.as_str()).unwrap();
    assert_eq!(stored.status(), SessionStatus::Recorded);
    let tracks = block_on(h.sessions.list_audio_tracks(&session.id)).unwrap();
    let mut paths: Vec<_> = tracks.iter().map(|t| t.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, ["audio/microphone.wav", "audio/system.wav"]);

    // The files: all samples written, finalised, and padded on the same timeline.
    for kind in [TrackKind::Microphone, TrackKind::System] {
        let log = h.media.log(kind);
        let log = log.lock().unwrap();
        assert_eq!(log.sample_frames, 4_800);
        assert!(log.finished);
    }

    // The frames: rows and files exist for the two kept frames only.
    let rows = block_on(h.frames.list_for_session(&session.id)).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].perceptual_hash, 0);
    assert_eq!(rows[1].perceptual_hash, u64::MAX);
    assert!(rows[1].timestamp_ms >= rows[0].timestamp_ms + 6_000);
    assert!(rows[0].path.starts_with("frames/"));
    assert!(rows.iter().all(|f| f.description.is_none()));
    assert_eq!(h.media.saved_frames.lock().unwrap().len(), 2);

    // Started and Stopped were published, in that order.
    let events = h.events.all();
    assert!(matches!(
        events.first(),
        Some(RecordingEvent::Started { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(RecordingEvent::Stopped { .. })
    ));
    assert!(matches!(h.service.status(), RecordingStatus::Idle));
}

#[test]
fn a_second_start_is_rejected_until_the_first_recording_stops() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("first"))).unwrap();
    assert!(h.service.is_busy());

    let err = block_on(h.service.start(StartOptions::titled("second"))).unwrap_err();
    assert!(matches!(err, RecordingError::AlreadyRecording), "{err}");
    assert_eq!(
        h.sessions.sessions.lock().unwrap().len(),
        1,
        "the rejected start left no session behind"
    );

    block_on(h.service.stop()).unwrap();
    assert!(!h.service.is_busy());
    block_on(h.service.start(StartOptions::titled("third"))).unwrap();
    block_on(h.service.stop()).unwrap();
    assert_eq!(h.sessions.sessions.lock().unwrap().len(), 2);
}

#[test]
fn stop_without_a_recording_is_rejected() {
    let h = harness();
    let err = block_on(h.service.stop()).unwrap_err();
    assert!(matches!(err, RecordingError::NotRecording), "{err}");
}

#[test]
fn blank_and_overlong_titles_are_rejected() {
    let h = harness();
    for title in ["", "   ", &"x".repeat(201)] {
        let err = block_on(h.service.start(StartOptions::titled(title))).unwrap_err();
        assert!(
            matches!(err, RecordingError::Invalid(_)),
            "{title:?}: {err}"
        );
    }
    assert!(!h.service.is_busy());
    assert!(h.sessions.sessions.lock().unwrap().is_empty());
}

#[test]
fn options_override_settings_and_settings_drive_the_screen_capture() {
    let settings = AppSettings {
        frame_interval_ms: 5_000,
        default_note_language: LanguageCode::english(),
        ..AppSettings::default()
    };
    let h = harness_with(settings);

    let session = block_on(h.service.start(StartOptions {
        speech_language: Some(LanguageSetting::Auto),
        display_id: Some("2".to_owned()),
        ..StartOptions::titled("lang")
    }))
    .unwrap();
    assert_eq!(session.note_language, LanguageCode::english());
    assert_eq!(session.language, LanguageSetting::Auto);

    let request = h.screen.started_with.lock().unwrap().clone().unwrap();
    assert_eq!(request.interval, Duration::from_secs(5));
    assert_eq!(request.display_id.as_deref(), Some("2"));
    block_on(h.service.stop()).unwrap();
}

#[test]
fn start_fails_cleanly_when_no_audio_source_can_start() {
    let h = harness();
    *h.audio.failing.lock().unwrap() = vec![TrackKind::Microphone, TrackKind::System];

    let err = block_on(h.service.start(StartOptions::titled("nothing works"))).unwrap_err();
    match err {
        RecordingError::Capture(e) => assert_eq!(e.kind, CaptureErrorKind::PermissionDenied),
        other => panic!("unexpected error: {other}"),
    }
    assert!(!h.service.is_busy());
    assert!(
        h.sessions.sessions.lock().unwrap().is_empty(),
        "no empty session is left behind"
    );
    assert_eq!(h.media.removed.lock().unwrap().len(), 1, "folder removed");

    // The service is usable again once the problem is gone.
    h.audio.failing.lock().unwrap().clear();
    block_on(h.service.start(StartOptions::titled("retry"))).unwrap();
    block_on(h.service.stop()).unwrap();
}

#[test]
fn one_failing_audio_source_is_only_a_warning() {
    let h = harness();
    *h.audio.failing.lock().unwrap() = vec![TrackKind::System];
    block_on(h.service.start(StartOptions::titled("mic only"))).unwrap();
    h.audio
        .sink(TrackKind::Microphone)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.tracks.len(), 1);
    assert_eq!(summary.tracks[0].kind, TrackKind::Microphone);
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("system audio could not start")),
        "{:?}",
        summary.warnings
    );
    assert!(h
        .events
        .all()
        .iter()
        .any(|e| matches!(e, RecordingEvent::Warning { .. })));
}

#[test]
fn a_screen_that_cannot_be_captured_is_only_a_warning() {
    let h = harness();
    h.screen.fail.store(true, Ordering::SeqCst);
    block_on(h.service.start(StartOptions::titled("no screen"))).unwrap();
    h.audio
        .sink(TrackKind::Microphone)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert_eq!(summary.frame_count, 0);
    assert!(summary
        .warnings
        .iter()
        .any(|w| w.contains("screen could not be captured")));
}

#[test]
fn an_unavailable_screen_is_skipped_without_error_or_warning() {
    let h = harness();
    h.screen.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        h.service.screen_availability(),
        ScreenAvailability::Unavailable(ScreenUnavailableReason::WaylandUnsupported)
    );

    block_on(h.service.start(StartOptions::titled("wayland"))).unwrap();
    assert!(
        h.screen.started_with.lock().unwrap().is_none(),
        "the screen stream must not be started"
    );
    h.audio
        .sink(TrackKind::Microphone)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    h.audio
        .sink(TrackKind::System)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert_eq!(summary.tracks.len(), 2);
    assert_eq!(summary.frame_count, 0);
    assert_eq!(summary.warnings.len(), 0, "{:?}", summary.warnings);
}

#[test]
fn a_digitally_silent_source_is_flagged() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("muted"))).unwrap();
    h.audio
        .sink(TrackKind::Microphone)
        .on_chunk(chunk(480, 0.0, Instant::now()));
    let summary = block_on(h.service.stop()).unwrap();
    assert!(summary.tracks[0].peak.abs() < f32::EPSILON);
    assert!(
        summary.warnings.iter().any(|w| w.contains("silent")),
        "{:?}",
        summary.warnings
    );
}

#[test]
fn a_recording_that_captured_nothing_is_marked_failed() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("empty"))).unwrap();
    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Failed);
    assert_eq!(summary.tracks.len(), 0);
    assert_eq!(
        h.sessions.get_now(session.id.as_str()).unwrap().status(),
        SessionStatus::Failed
    );
    assert!(summary.warnings.iter().any(|w| w.contains("nothing")));
}

#[test]
fn a_gap_in_delivery_is_filled_with_silence_to_stay_on_the_timeline() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("gap"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    let now = Instant::now();
    mic.on_chunk(chunk(480, 0.2, now));
    // The next block "arrives" two seconds later: the device went quiet in between.
    mic.on_chunk(chunk(480, 0.2, now + Duration::from_secs(2)));
    let summary = block_on(h.service.stop()).unwrap();

    let log = h.media.log(TrackKind::Microphone);
    let log = log.lock().unwrap();
    assert_eq!(log.sample_frames, 960);
    assert!(
        log.silence_frames > 90_000,
        "about two seconds of silence expected, got {} frames",
        log.silence_frames
    );
    assert!(summary.tracks[0].silence_inserted_ms >= 1_900);
    assert!(summary.tracks[0].duration_ms >= 2_000);
}

#[test]
fn audio_files_are_checkpointed_while_recording() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("checkpoints"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    // 12 seconds of audio in one-second blocks: two 5-second checkpoints.
    let now = Instant::now();
    for _ in 0..12 {
        mic.on_chunk(chunk(48_000, 0.2, now));
    }
    block_on(h.service.stop()).unwrap();
    let log = h.media.log(TrackKind::Microphone);
    assert!(log.lock().unwrap().checkpoints >= 2);
}

#[test]
fn progress_and_levels_are_reported_while_recording() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("levels"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    // Keep feeding loud audio until the 250 ms ticker has published at least one update.
    let deadline = Instant::now() + Duration::from_secs(5);
    let level = loop {
        mic.on_chunk(chunk(480, 0.5, Instant::now()));
        if let RecordingStatus::Recording(active) = h.service.status() {
            if active.microphone_level > 0.0 {
                break active.microphone_level;
            }
        }
        assert!(Instant::now() < deadline, "no progress was published");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(level > 0.4 && level <= 1.0, "RMS of a 0.5 signal: {level}");
    assert!(h
        .events
        .all()
        .iter()
        .any(|e| matches!(e, RecordingEvent::Progress(p) if p.elapsed_ms > 0)));
    block_on(h.service.stop()).unwrap();
}

// ---- recovery --------------------------------------------------------------------------

fn interrupted_session(id: &str, started_at: i64) -> Session {
    Session::start_recording(
        SessionId::new(id),
        SessionKind::Meeting,
        "interrupted",
        EpochMs::new(started_at),
        LanguageSetting::Auto,
        LanguageCode::turkish(),
    )
}

#[test]
fn interrupted_recordings_are_recovered_as_recorded() {
    let h = harness();
    let survivor = interrupted_session("crashed-with-audio", 1_000_000);
    block_on(h.sessions.create(&survivor)).unwrap();
    h.media.on_disk.lock().unwrap().insert(
        ("crashed-with-audio".to_owned(), TrackKind::Microphone),
        AudioFileInfo {
            relative_path: "audio/microphone.wav".to_owned(),
            sample_rate: 48_000,
            channels: 1,
            frames: 48_000 * 90, // 90 s survived the crash
        },
    );
    // A frame kept at 120 s: the recording lasted at least that long.
    block_on(h.frames.add(&Frame {
        id: FrameId::new("f1"),
        session_id: survivor.id.clone(),
        timestamp_ms: 120_000,
        path: "frames/frame-000120000.png".to_owned(),
        perceptual_hash: 1,
        description: None,
    }))
    .unwrap();

    let empty = interrupted_session("crashed-empty", 2_000_000);
    block_on(h.sessions.create(&empty)).unwrap();

    let mut done = interrupted_session("already-done", 3_000_000);
    done.finish_recording(EpochMs::new(3_060_000)).unwrap();
    block_on(h.sessions.create(&done)).unwrap();

    let recovered = block_on(h.service.recover_interrupted()).unwrap();
    assert_eq!(recovered.len(), 2, "the finished session is left alone");

    let session = h.sessions.get_now("crashed-with-audio").unwrap();
    assert_eq!(session.status(), SessionStatus::Recorded);
    assert_eq!(session.ended_at(), Some(EpochMs::new(1_000_000 + 120_000)));
    let tracks = block_on(h.sessions.list_audio_tracks(&survivor.id)).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].kind, TrackKind::Microphone);
    assert_eq!(tracks[0].duration_ms, 90_000);

    let empty = h.sessions.get_now("crashed-empty").unwrap();
    assert_eq!(empty.status(), SessionStatus::Failed);
    assert_eq!(empty.ended_at(), Some(EpochMs::new(2_000_000)));

    let done = h.sessions.get_now("already-done").unwrap();
    assert_eq!(done.status(), SessionStatus::Recorded);
    assert_eq!(done.ended_at(), Some(EpochMs::new(3_060_000)));

    // Running it again changes nothing.
    assert_eq!(block_on(h.service.recover_interrupted()).unwrap().len(), 0);
}

#[test]
fn recovery_leaves_a_running_recording_alone() {
    let h = harness();
    let live = block_on(h.service.start(StartOptions::titled("live"))).unwrap();
    assert_eq!(recording_session_id(&h), live.id.to_string());

    let recovered = block_on(h.service.recover_interrupted()).unwrap();
    assert_eq!(recovered.len(), 0);
    assert_eq!(
        h.sessions.get_now(live.id.as_str()).unwrap().status(),
        SessionStatus::Recording
    );
    block_on(h.service.stop()).unwrap();
}
