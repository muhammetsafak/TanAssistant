//! Recording use case against in-memory fakes of every port.

// The in-memory fakes implement async-shaped ports without awaiting anything.
#![allow(clippy::unused_async_trait_impl)]

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// Streams of this fake that are open right now.
    live: Arc<AtomicU32>,
}

impl FakeStream {
    fn open(stopped: &Arc<AtomicBool>, live: &Arc<AtomicU32>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self {
            stopped: stopped.clone(),
            live: live.clone(),
        }
    }

    fn close(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AudioStream for FakeStream {
    fn format(&self) -> AudioFormat {
        MONO_48K
    }

    fn device_name(&self) -> &'static str {
        "fake device"
    }

    fn stop(self: Box<Self>) {
        self.close();
    }
}

impl ScreenStream for FakeStream {
    fn stop(self: Box<Self>) {
        self.close();
    }
}

#[derive(Default)]
struct FakeAudio {
    sinks: Mutex<HashMap<TrackKind, Arc<dyn AudioSink>>>,
    failing: Mutex<Vec<TrackKind>>,
    stopped: Arc<AtomicBool>,
    live: Arc<AtomicU32>,
    /// While set, `start` waits (a slow OS call).
    gate: AtomicBool,
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
        while self.gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.failing.lock().unwrap().contains(&request.source) {
            return Err(CaptureError::new(
                CaptureErrorKind::PermissionDenied,
                "access denied",
            ));
        }
        self.sinks.lock().unwrap().insert(request.source, sink);
        Ok(Box::new(FakeStream::open(&self.stopped, &self.live)))
    }
}

#[derive(Default)]
struct FakeScreen {
    sink: Mutex<Option<Arc<dyn FrameSink>>>,
    fail: AtomicBool,
    unavailable: AtomicBool,
    live: Arc<AtomicU32>,
    /// While set, `start` waits (a portal dialog, a slow OS call).
    gate: AtomicBool,
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
        while self.gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(CaptureError::unsupported("no display server"));
        }
        *self.sink.lock().unwrap() = Some(sink);
        *self.started_with.lock().unwrap() = Some(request.clone());
        Ok(Box::new(FakeStream::open(&Arc::default(), &self.live)))
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
        RecordingStatus::Idle | RecordingStatus::Paused(_) => {
            panic!("expected a running recording")
        }
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

// ---- pause and resume ------------------------------------------------------------------

/// Feeds `duration` of audio in real time: a block every ~10 ms, each exactly as long as the
/// time since the previous one, so the file keeps pace with the clock.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn feed_for(sinks: &[Arc<dyn AudioSink>], duration: Duration) {
    let end = Instant::now() + duration;
    let mut last = Instant::now();
    while last < end {
        std::thread::sleep(Duration::from_millis(10));
        let now = Instant::now();
        let frames = ((now - last).as_secs_f64() * 48_000.0) as usize;
        for sink in sinks {
            sink.on_chunk(chunk(frames, 0.2, now));
        }
        last = now;
    }
}

fn both_sinks(h: &Harness) -> [Arc<dyn AudioSink>; 2] {
    [
        h.audio.sink(TrackKind::Microphone),
        h.audio.sink(TrackKind::System),
    ]
}

fn file_ms(h: &Harness, kind: TrackKind) -> u64 {
    let log = h.media.log(kind);
    let log = log.lock().unwrap();
    (log.sample_frames + log.silence_frames) * 1_000 / 48_000
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap()
}

#[test]
fn a_pause_is_skipped_by_the_audio_files_and_the_session_length() {
    // "10 s recording, 10 s pause, 10 s recording" at 1:20 scale: 0.5 s, 1 s pause, 0.5 s.
    let h = harness();
    block_on(h.service.start(StartOptions::titled("pause"))).unwrap();
    let sinks = both_sinks(&h);

    let first = Instant::now();
    feed_for(&sinks, Duration::from_millis(500));
    h.service.pause().unwrap();
    let first = first.elapsed();

    std::thread::sleep(Duration::from_millis(1_000));

    let resumed = Instant::now();
    h.service.resume().unwrap();
    feed_for(&sinks, Duration::from_millis(500));
    let summary = block_on(h.service.stop()).unwrap();
    let active = millis(first + resumed.elapsed());

    assert!(
        (900..1_300).contains(&active),
        "test timing is off: {active}"
    );
    let session_ms = summary.session.duration_ms().unwrap();
    assert!(
        session_ms.abs_diff(active) < 250,
        "session {session_ms} ms vs active {active} ms"
    );
    for kind in [TrackKind::Microphone, TrackKind::System] {
        let ms = file_ms(&h, kind);
        assert!(
            ms.abs_diff(active) < 250,
            "{kind} file {ms} ms vs active {active} ms (the 1000 ms pause must not be in it)"
        );
        // The pause is not rendered as silence either.
        let log = h.media.log(kind);
        let silence_ms = log.lock().unwrap().silence_frames * 1_000 / 48_000;
        assert!(silence_ms < 300, "{kind}: {silence_ms} ms of silence");
    }
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}

#[test]
fn pause_and_resume_follow_the_state_machine() {
    let h = harness();
    // Nothing recording.
    assert!(matches!(
        h.service.pause().unwrap_err(),
        RecordingError::NotRecording
    ));
    assert!(matches!(
        h.service.resume().unwrap_err(),
        RecordingError::NotRecording
    ));

    block_on(h.service.start(StartOptions::titled("states"))).unwrap();
    // Running: cannot resume.
    assert!(matches!(
        h.service.resume().unwrap_err(),
        RecordingError::NotPaused
    ));

    let paused = h.service.pause().unwrap();
    assert!(
        matches!(h.service.status(), RecordingStatus::Paused(a) if a.session_id == paused.session_id)
    );
    assert!(h.service.is_busy());
    assert!(matches!(
        h.service.pause().unwrap_err(),
        RecordingError::AlreadyPaused
    ));
    // A paused recording still blocks a second start.
    assert!(matches!(
        block_on(h.service.start(StartOptions::titled("again"))).unwrap_err(),
        RecordingError::AlreadyRecording
    ));

    let resumed = h.service.resume().unwrap();
    assert_eq!(resumed.session_id, paused.session_id);
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    // And it can pause again.
    h.service.pause().unwrap();
    h.service.resume().unwrap();
    block_on(h.service.stop()).unwrap();
    assert!(matches!(h.service.status(), RecordingStatus::Idle));
}

#[test]
fn pausing_closes_every_stream_and_drops_what_still_arrives() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("streams"))).unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 2);
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 1);
    let [mic, system] = both_sinks(&h);
    let screen = h.screen.sink.lock().unwrap().clone().unwrap();

    feed_for(&[mic.clone(), system.clone()], Duration::from_millis(100));
    h.service.pause().unwrap();
    assert_eq!(
        h.audio.live.load(Ordering::SeqCst),
        0,
        "microphone and system closed"
    );
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 0, "screen closed");

    // A late callback of a stopped stream is not captured.
    let written = |kind| {
        let log = h.media.log(kind);
        let log = log.lock().unwrap();
        log.sample_frames
    };
    // Let the worker drain what was queued before the pause.
    std::thread::sleep(Duration::from_millis(100));
    let before = written(TrackKind::Microphone);
    mic.on_chunk(chunk(48_000, 0.9, Instant::now()));
    screen.on_frame(frame_with_hash(u64::MAX, Instant::now()));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(written(TrackKind::Microphone), before);

    h.service.resume().unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 2, "sources reopened");
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 1, "screen reopened");
    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(
        summary.frame_count, 0,
        "the frame sent during the pause was dropped"
    );
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 0);
}

#[test]
fn frame_timestamps_stay_monotonic_across_a_pause() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("frames"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    let screen = h.screen.sink.lock().unwrap().clone().unwrap();
    let send = |hash: u64| {
        screen.on_frame(frame_with_hash(hash, Instant::now()));
        // The sink drops frames while the worker is busy (by design).
        std::thread::sleep(Duration::from_millis(50));
    };

    send(0);
    h.service.pause().unwrap();
    std::thread::sleep(Duration::from_millis(900));
    h.service.resume().unwrap();
    send(u64::MAX);
    send(0);
    block_on(h.service.stop()).unwrap();

    let session = h
        .sessions
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    let rows = block_on(h.frames.list_for_session(&session.id)).unwrap();
    assert_eq!(rows.len(), 3);
    let stamps: Vec<u64> = rows.iter().map(|f| f.timestamp_ms).collect();
    assert!(stamps.windows(2).all(|w| w[0] <= w[1]), "{stamps:?}");
    assert!(
        stamps[2] < 700,
        "the 900 ms pause must not be on the frame axis: {stamps:?}"
    );
}

#[test]
fn a_paused_recording_can_be_stopped_directly() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("stop paused"))).unwrap();
    let sinks = both_sinks(&h);
    let started = Instant::now();
    feed_for(&sinks, Duration::from_millis(400));
    h.service.pause().unwrap();
    let active = millis(started.elapsed());
    std::thread::sleep(Duration::from_millis(800));

    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert_eq!(summary.tracks.len(), 2);
    let session_ms = summary.session.duration_ms().unwrap();
    assert!(
        session_ms.abs_diff(active) < 250,
        "session {session_ms} ms vs active {active} ms"
    );
    for track in &summary.tracks {
        assert!(
            track.duration_ms.abs_diff(active) < 250,
            "{} track {} ms vs {active} ms",
            track.kind,
            track.duration_ms
        );
        assert!(h.media.log(track.kind).lock().unwrap().finished);
    }
    assert_eq!(
        h.sessions.get_now(session.id.as_str()).unwrap().status(),
        SessionStatus::Recorded
    );
    assert!(matches!(
        h.events.all().last(),
        Some(RecordingEvent::Stopped { .. })
    ));
    assert!(!h.service.is_busy());
}

#[test]
fn resume_that_cannot_reopen_any_audio_source_stays_paused() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("denied"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.pause().unwrap();

    *h.audio.failing.lock().unwrap() = vec![TrackKind::Microphone, TrackKind::System];
    match h.service.resume().unwrap_err() {
        RecordingError::Capture(e) => assert_eq!(e.kind, CaptureErrorKind::PermissionDenied),
        other => panic!("unexpected error: {other}"),
    }
    assert!(matches!(h.service.status(), RecordingStatus::Paused(_)));
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.screen.live.load(Ordering::SeqCst),
        0,
        "no screen without audio"
    );

    // Once the problem is gone it resumes, and the failed attempt left no gap behind.
    h.audio.failing.lock().unwrap().clear();
    h.service.resume().unwrap();
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    block_on(h.service.stop()).unwrap();
}

#[test]
fn one_source_that_cannot_reopen_is_only_a_warning() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("half"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.pause().unwrap();

    *h.audio.failing.lock().unwrap() = vec![TrackKind::System];
    h.service.resume().unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 1);
    feed_for(
        &[h.audio.sink(TrackKind::Microphone)],
        Duration::from_millis(100),
    );
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert_eq!(
        summary.tracks.len(),
        2,
        "both files were written before the pause"
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("system audio could not resume")),
        "{:?}",
        summary.warnings
    );
    // The track whose source stayed closed is padded up to the end, not left short.
    let mic = file_ms(&h, TrackKind::Microphone);
    let system = file_ms(&h, TrackKind::System);
    assert!(
        mic.abs_diff(system) < 250,
        "mic {mic} ms, system {system} ms"
    );
}

#[test]
fn pause_and_resume_are_published_and_progress_stops_while_paused() {
    let progress = |h: &Harness| {
        h.events
            .all()
            .iter()
            .filter(|e| matches!(e, RecordingEvent::Progress(_)))
            .count()
    };
    let h = harness();
    block_on(h.service.start(StartOptions::titled("events"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(300));
    // Wait for the first progress tick instead of assuming it fits into a fixed delay: a slow
    // CI runner may not have published one yet.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while progress(&h) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    h.service.pause().unwrap();
    // Let a tick that was already due go out.
    std::thread::sleep(Duration::from_millis(400));

    let before = progress(&h);
    assert!(before > 0, "a progress tick was published before the pause");
    let elapsed_at_pause = match h.service.status() {
        RecordingStatus::Paused(a) => a.elapsed_ms,
        other => panic!("expected paused, got {other:?}"),
    };
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(progress(&h), before, "no progress while paused");
    match h.service.status() {
        RecordingStatus::Paused(a) => {
            assert_eq!(a.elapsed_ms, elapsed_at_pause, "the clock stands still");
            assert!(a.microphone_level.abs() < f32::EPSILON);
        }
        other => panic!("expected paused, got {other:?}"),
    }

    h.service.resume().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(progress(&h) > before, "progress continues after resume");
    block_on(h.service.stop()).unwrap();

    let kinds: Vec<&str> = h
        .events
        .all()
        .iter()
        .filter_map(|e| match e {
            RecordingEvent::Started { .. } => Some("started"),
            RecordingEvent::Paused { .. } => Some("paused"),
            RecordingEvent::Resumed { .. } => Some("resumed"),
            RecordingEvent::Stopped { .. } => Some("stopped"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["started", "paused", "resumed", "stopped"]);
}

#[test]
fn a_crash_while_paused_recovers_exactly_the_active_time() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("crash"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(400));
    h.service.pause().unwrap();
    // The pause goes on for much longer than the recording itself, then the process dies.
    std::thread::sleep(Duration::from_millis(1_200));

    // The files were padded up to the pause instant and made durable when it began.
    let mut length_ms = 0;
    for kind in [TrackKind::Microphone, TrackKind::System] {
        let log = h.media.log(kind);
        let log = log.lock().unwrap();
        assert!(log.checkpoints >= 1, "{kind}: no checkpoint at the pause");
        assert!(!log.finished);
        let frames = log.sample_frames + log.silence_frames;
        length_ms = length_ms.max(frames * 1_000 / 48_000);
        h.media.on_disk.lock().unwrap().insert(
            (session.id.to_string(), kind),
            AudioFileInfo {
                relative_path: format!("audio/{kind}.wav"),
                sample_rate: 48_000,
                channels: 1,
                frames,
            },
        );
    }

    // A fresh process (a new service over the same database and files) settles it.
    let restarted = RecordingService::new(
        h.sessions.clone(),
        h.frames.clone(),
        Arc::new(MemSettings(AppSettings::default())),
        RecordingPorts {
            audio: h.audio.clone(),
            screen: h.screen.clone(),
            hasher: Arc::new(FakeHasher),
            media: h.media.clone(),
            ids: Arc::new(SequentialIds::default()),
            clock: Arc::new(FixedClock),
            events: h.events.clone(),
        },
    );
    // The paused recording itself is left alone by its own service...
    assert_eq!(block_on(h.service.recover_interrupted()).unwrap().len(), 0);
    // ...and the new one finds it interrupted.
    let recovered = block_on(restarted.recover_interrupted()).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].tracks, 2);

    let stored = h.sessions.get_now(session.id.as_str()).unwrap();
    assert_eq!(stored.status(), SessionStatus::Recorded);
    let recovered_ms = stored.duration_ms().unwrap();
    assert_eq!(recovered_ms, length_ms);
    assert!(
        (350..700).contains(&recovered_ms),
        "the 1200 ms pause must not count: {recovered_ms} ms"
    );

    // Release the original service's workers.
    let _ = block_on(h.service.stop());
}

#[test]
fn status_and_is_busy_answer_while_a_resume_waits_on_the_os() {
    let h = Arc::new(harness());
    block_on(h.service.start(StartOptions::titled("slow resume"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.pause().unwrap();
    let frozen = match h.service.status() {
        RecordingStatus::Paused(a) => a.elapsed_ms,
        other => panic!("expected paused, got {other:?}"),
    };

    h.audio.gate.store(true, Ordering::SeqCst);
    let resuming = {
        let h = h.clone();
        std::thread::spawn(move || h.service.resume())
    };
    std::thread::sleep(Duration::from_millis(150));

    // The OS call is still pending, yet the questions are answered at once: the sources are
    // not capturing, so the recording still reads as paused with a standing clock.
    let asked = Instant::now();
    assert!(h.service.is_busy());
    match h.service.status() {
        RecordingStatus::Paused(a) => assert_eq!(a.elapsed_ms, frozen),
        other => panic!("expected paused while the sources open, got {other:?}"),
    }
    assert!(
        asked.elapsed() < Duration::from_millis(100),
        "status waited for the OS"
    );

    // A stop requested meanwhile waits for the resume and then ends the recording.
    let stopping = {
        let h = h.clone();
        std::thread::spawn(move || block_on(h.service.stop()))
    };
    std::thread::sleep(Duration::from_millis(100));
    h.audio.gate.store(false, Ordering::SeqCst);
    resuming.join().unwrap().unwrap();
    let summary = stopping.join().unwrap().unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(!h.service.is_busy());
}

#[test]
fn a_slow_screen_does_not_hold_back_the_audio_or_the_resumed_state() {
    let h = Arc::new(harness());
    block_on(h.service.start(StartOptions::titled("slow screen"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.pause().unwrap();

    h.screen.gate.store(true, Ordering::SeqCst);
    let resuming = {
        let h = h.clone();
        std::thread::spawn(move || h.service.resume())
    };
    // Both audio sources run while the screen is still opening.
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.audio.live.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline, "audio never reopened");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    assert!(h
        .events
        .all()
        .iter()
        .any(|e| matches!(e, RecordingEvent::Resumed { .. })));
    let mic = h.audio.sink(TrackKind::Microphone);
    let before = h
        .media
        .log(TrackKind::Microphone)
        .lock()
        .unwrap()
        .sample_frames;
    feed_for(&[mic], Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(50));
    let after = h
        .media
        .log(TrackKind::Microphone)
        .lock()
        .unwrap()
        .sample_frames;
    assert!(after > before, "audio was lost while the screen opened");

    h.screen.gate.store(false, Ordering::SeqCst);
    resuming.join().unwrap().unwrap();
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 1);
    block_on(h.service.stop()).unwrap();
}

#[test]
fn racing_pauses_and_resumes_publish_events_in_state_order() {
    let h = Arc::new(harness());
    block_on(h.service.start(StartOptions::titled("race"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(50));

    let workers: Vec<_> = (0..2)
        .map(|_| {
            let h = h.clone();
            std::thread::spawn(move || {
                for _ in 0..40 {
                    // Losing a race is an ordinary typed error here.
                    let _ = h.service.pause();
                    let _ = h.service.resume();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    // Leave it running so the last event is checked too.
    let _ = h.service.resume();

    let transitions: Vec<bool> = h
        .events
        .all()
        .iter()
        .filter_map(|e| match e {
            RecordingEvent::Paused { .. } => Some(true),
            RecordingEvent::Resumed { .. } => Some(false),
            _ => None,
        })
        .collect();
    assert!(
        transitions.len() > 4,
        "the threads never overlapped usefully"
    );
    for pair in transitions.windows(2) {
        assert_ne!(pair[0], pair[1], "events out of order: {transitions:?}");
    }
    assert!(transitions[0], "the first transition is a pause");
    assert!(
        !transitions[transitions.len() - 1],
        "the last one is a resume"
    );
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    block_on(h.service.stop()).unwrap();
}

#[test]
fn begin_stop_waits_for_a_resume_and_the_service_stays_busy_until_finish_stop() {
    let h = Arc::new(harness());
    block_on(h.service.start(StartOptions::titled("split stop"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.pause().unwrap();

    h.audio.gate.store(true, Ordering::SeqCst);
    let resuming = {
        let h = h.clone();
        std::thread::spawn(move || h.service.resume())
    };
    std::thread::sleep(Duration::from_millis(100));
    let taking = {
        let h = h.clone();
        std::thread::spawn(move || h.service.begin_stop())
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!taking.is_finished(), "begin_stop must wait for the resume");

    h.audio.gate.store(false, Ordering::SeqCst);
    resuming.join().unwrap().unwrap();
    let stopping = taking.join().unwrap().unwrap();

    // Taken but not yet settled: still busy, nothing else can start.
    assert!(h.service.is_busy());
    assert!(matches!(h.service.status(), RecordingStatus::Idle));
    assert!(matches!(
        block_on(h.service.start(StartOptions::titled("too early"))).unwrap_err(),
        RecordingError::AlreadyRecording
    ));

    let summary = block_on(h.service.finish_stop(stopping)).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(!h.service.is_busy());
    assert!(matches!(
        h.events.all().last(),
        Some(RecordingEvent::Stopped { .. })
    ));
    // Nothing left to take.
    assert!(matches!(
        h.service.begin_stop().err(),
        Some(RecordingError::NotRecording)
    ));
}
