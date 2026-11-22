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
    DropoutPolicy, RecordingError, RecordingEvent, RecordingEvents, RecordingPorts,
    RecordingService, RecordingStatus, RecordingWarning, RecordingWarningCode, StartOptions,
    DROPOUT_WARN_COUNT,
};
use tan_application::{RepositoryError, RepositoryResult};
use tan_domain::{
    AppSettings, AudioTrack, EpochMs, Frame, FrameId, LanguageCode, LanguageSetting, Session,
    SessionId, SessionKind, SessionStatus, SettingsPatch, TrackKind, TrackMute,
};

// ---- fakes -----------------------------------------------------------------------------

#[derive(Default)]
struct MemSessions {
    sessions: Mutex<BTreeMap<String, Session>>,
    tracks: Mutex<Vec<AudioTrack>>,
    mutes: Mutex<Vec<TrackMute>>,
    /// While set, closing a mute interval fails.
    fail_close: AtomicBool,
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

    async fn open_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        start_ms: u64,
    ) -> RepositoryResult<()> {
        self.mutes.lock().unwrap().push(TrackMute {
            session_id: session_id.clone(),
            kind,
            start_ms,
            end_ms: None,
        });
        Ok(())
    }

    async fn close_track_mute(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        end_ms: u64,
    ) -> RepositoryResult<()> {
        if self.fail_close.load(Ordering::SeqCst) {
            return Err(RepositoryError::Invalid("cannot write".to_owned()));
        }
        for mute in self.mutes.lock().unwrap().iter_mut() {
            if &mute.session_id == session_id && mute.kind == kind && mute.end_ms.is_none() {
                mute.end_ms = Some(end_ms.max(mute.start_ms));
            }
        }
        Ok(())
    }

    async fn list_track_mutes(&self, session_id: &SessionId) -> RepositoryResult<Vec<TrackMute>> {
        let mut mutes: Vec<TrackMute> = self
            .mutes
            .lock()
            .unwrap()
            .iter()
            .filter(|m| &m.session_id == session_id)
            .cloned()
            .collect();
        mutes.sort_by_key(|m| m.start_ms);
        Ok(mutes)
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
    /// Streams of the same channel that are open right now.
    kind_live: Option<Arc<AtomicU32>>,
}

impl FakeStream {
    fn open(stopped: &Arc<AtomicBool>, live: &Arc<AtomicU32>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self {
            stopped: stopped.clone(),
            live: live.clone(),
            kind_live: None,
        }
    }

    fn open_for_kind(
        stopped: &Arc<AtomicBool>,
        live: &Arc<AtomicU32>,
        kind_live: &Arc<AtomicU32>,
    ) -> Self {
        kind_live.fetch_add(1, Ordering::SeqCst);
        Self {
            kind_live: Some(kind_live.clone()),
            ..Self::open(stopped, live)
        }
    }

    fn close(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.live.fetch_sub(1, Ordering::SeqCst);
        if let Some(kind_live) = &self.kind_live {
            kind_live.fetch_sub(1, Ordering::SeqCst);
        }
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
    /// Open streams per channel.
    live_by_kind: Mutex<HashMap<TrackKind, Arc<AtomicU32>>>,
    /// While set, `start` waits (a slow OS call).
    gate: AtomicBool,
}

impl FakeAudio {
    fn sink(&self, kind: TrackKind) -> Arc<dyn AudioSink> {
        self.sinks.lock().unwrap()[&kind].clone()
    }

    /// Streams of `kind` that are open right now.
    fn live_of(&self, kind: TrackKind) -> u32 {
        self.live_by_kind
            .lock()
            .unwrap()
            .get(&kind)
            .map_or(0, |live| live.load(Ordering::SeqCst))
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
        let kind_live = self
            .live_by_kind
            .lock()
            .unwrap()
            .entry(request.source)
            .or_default()
            .clone();
        Ok(Box::new(FakeStream::open_for_kind(
            &self.stopped,
            &self.live,
            &kind_live,
        )))
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
            .any(|w| w.message.contains("system audio could not start")),
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
        .any(|w| w.message.contains("screen could not be captured")));
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
        summary
            .warnings
            .iter()
            .any(|w| w.message.contains("silent")),
        "{:?}",
        summary.warnings
    );
}

/// The dropout warnings among `warnings`.
fn dropout_warnings(warnings: &[RecordingWarning]) -> Vec<&RecordingWarning> {
    warnings
        .iter()
        .filter(|w| w.code == RecordingWarningCode::AudioDropouts)
        .collect()
}

#[test]
fn a_few_dropouts_are_counted_without_a_warning() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("blips"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    h.audio
        .sink(TrackKind::System)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    for _ in 0..DROPOUT_WARN_COUNT - 1 {
        mic.on_dropout();
    }
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.tracks[0].kind, TrackKind::Microphone);
    assert_eq!(summary.tracks[0].dropouts, 4);
    assert_eq!(summary.tracks[0].stream_issues, 0);
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
    assert!(!h
        .events
        .all()
        .iter()
        .any(|e| matches!(e, RecordingEvent::Warning { .. })));
}

#[test]
fn a_burst_of_dropouts_raises_exactly_one_coded_warning() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("burst"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    h.audio
        .sink(TrackKind::System)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    // More than the threshold: still one warning for the track.
    for _ in 0..DROPOUT_WARN_COUNT + 3 {
        mic.on_dropout();
    }
    let summary = block_on(h.service.stop()).unwrap();

    let warnings = dropout_warnings(&summary.warnings);
    assert_eq!(warnings.len(), 1, "{:?}", summary.warnings);
    assert_eq!(warnings[0].track, Some(TrackKind::Microphone));
    assert_eq!(
        warnings[0].message,
        "microphone audio had short dropouts; recording continues"
    );
    let mic_summary = summary
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Microphone)
        .unwrap();
    assert_eq!(mic_summary.dropouts, 8);

    // It was published as an event too, carrying the same structure.
    let events = h.events.all();
    let published: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RecordingEvent::Warning {
                session_id,
                warning,
            } if warning.code == RecordingWarningCode::AudioDropouts => {
                Some((session_id.clone(), warning.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, session.id);
    assert_eq!(published[0].1, *warnings[0]);
}

#[test]
fn dropouts_spread_over_the_window_never_warn() {
    // The 5-in-60-s rule at 1:150 scale: 5 in 400 ms. A dropout every 120 ms (at least; a
    // sleep never returns early) puts the fifth of any run of five 480 ms after the first,
    // so the window never holds five at once.
    let mut h = harness();
    h.service = h.service.with_dropout_policy(DropoutPolicy {
        count: DROPOUT_WARN_COUNT,
        window: Duration::from_millis(400),
    });
    block_on(h.service.start(StartOptions::titled("sparse"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    h.audio
        .sink(TrackKind::System)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    for _ in 0..12 {
        mic.on_dropout();
        std::thread::sleep(Duration::from_millis(120));
    }
    assert!(!h
        .events
        .all()
        .iter()
        .any(|e| matches!(e, RecordingEvent::Warning { .. })));
    // The same number in a burst does warn, so the policy is really in effect.
    for _ in 0..DROPOUT_WARN_COUNT {
        mic.on_dropout();
    }
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(summary.tracks[0].dropouts, 12 + 5);
    let warnings = dropout_warnings(&summary.warnings);
    assert_eq!(warnings.len(), 1, "{:?}", summary.warnings);
    assert_eq!(warnings[0].track, Some(TrackKind::Microphone));
}

#[test]
fn microphone_and_system_dropouts_are_counted_separately() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("two sources"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    let system = h.audio.sink(TrackKind::System);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    system.on_chunk(chunk(480, 0.2, Instant::now()));
    // Four on each source: eight in total, but no track reaches the threshold.
    for _ in 0..DROPOUT_WARN_COUNT - 1 {
        mic.on_dropout();
        system.on_dropout();
    }
    let summary = block_on(h.service.stop()).unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);

    // A new recording: only the system source bursts.
    block_on(h.service.start(StartOptions::titled("system burst"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    let system = h.audio.sink(TrackKind::System);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    system.on_chunk(chunk(480, 0.2, Instant::now()));
    mic.on_dropout();
    for _ in 0..DROPOUT_WARN_COUNT {
        system.on_dropout();
    }
    let summary = block_on(h.service.stop()).unwrap();

    let warnings = dropout_warnings(&summary.warnings);
    assert_eq!(warnings.len(), 1, "{:?}", summary.warnings);
    assert_eq!(warnings[0].track, Some(TrackKind::System));
    let dropouts = |kind| {
        summary
            .tracks
            .iter()
            .find(|t| t.kind == kind)
            .unwrap()
            .dropouts
    };
    assert_eq!(dropouts(TrackKind::Microphone), 1);
    assert_eq!(dropouts(TrackKind::System), 5);
}

#[test]
fn a_backend_error_is_still_an_uncoded_warning_and_a_stream_issue() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("device change"))).unwrap();
    let mic = h.audio.sink(TrackKind::Microphone);
    mic.on_chunk(chunk(480, 0.2, Instant::now()));
    h.audio
        .sink(TrackKind::System)
        .on_chunk(chunk(480, 0.2, Instant::now()));
    mic.on_error("the device was unplugged".to_owned());
    mic.on_dropout();
    let summary = block_on(h.service.stop()).unwrap();

    assert_eq!(
        summary.warnings,
        [RecordingWarning {
            code: RecordingWarningCode::Other,
            track: None,
            message: "microphone audio: the device was unplugged".to_owned(),
        }]
    );
    // The dropout is not a stream issue.
    assert_eq!(summary.tracks[0].stream_issues, 1);
    assert_eq!(summary.tracks[0].dropouts, 1);
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
    assert!(summary
        .warnings
        .iter()
        .any(|w| w.message.contains("nothing")));
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
            .any(|w| w.message.contains("system audio could not resume")),
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

// ---- muting one channel ----------------------------------------------------------------

fn mute_status(h: &Harness) -> (bool, bool) {
    match h.service.status() {
        RecordingStatus::Recording(active) | RecordingStatus::Paused(active) => {
            (active.microphone_muted, active.system_muted)
        }
        RecordingStatus::Idle => panic!("expected a recording"),
    }
}

fn sample_ms(h: &Harness, kind: TrackKind) -> u64 {
    h.media.log(kind).lock().unwrap().sample_frames * 1_000 / 48_000
}

fn silence_ms(h: &Harness, kind: TrackKind) -> u64 {
    h.media.log(kind).lock().unwrap().silence_frames * 1_000 / 48_000
}

fn mute_events(h: &Harness) -> Vec<(TrackKind, bool)> {
    h.events
        .all()
        .into_iter()
        .filter_map(|event| match event {
            RecordingEvent::TrackMuteChanged { kind, muted, .. } => Some((kind, muted)),
            _ => None,
        })
        .collect()
}

fn unmute_warnings(h: &Harness) -> usize {
    h.events
        .all()
        .iter()
        .filter(|event| {
            matches!(
                event,
                RecordingEvent::Warning { warning, .. }
                    if warning.code == RecordingWarningCode::TrackUnmuteFailed
                        && warning.track == Some(TrackKind::Microphone)
            )
        })
        .count()
}

#[test]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn muting_the_microphone_closes_only_its_stream_and_the_system_channel_keeps_recording() {
    // 0.3 s both, 0.4 s with the microphone muted, 0.3 s both again.
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("mute"))).unwrap();
    let [mic, system] = both_sinks(&h);
    assert_eq!(mute_status(&h), (false, false));

    feed_for(&[mic.clone(), system.clone()], Duration::from_millis(300));
    let status = h
        .service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    assert!(status.microphone_muted && !status.system_muted);
    assert_eq!(mute_status(&h), (true, false));
    assert_eq!(
        h.audio.live_of(TrackKind::Microphone),
        0,
        "mic stream closed"
    );
    assert_eq!(h.audio.live_of(TrackKind::System), 1, "system stream open");
    assert_eq!(h.screen.live.load(Ordering::SeqCst), 1, "screen untouched");

    // The ambient speech the microphone would have picked up is not captured, and the
    // system channel is not interrupted.
    std::thread::sleep(Duration::from_millis(50));
    let mic_before = sample_ms(&h, TrackKind::Microphone);
    let system_before = sample_ms(&h, TrackKind::System);
    let end = Instant::now() + Duration::from_millis(400);
    let mut last = Instant::now();
    while last < end {
        std::thread::sleep(Duration::from_millis(10));
        let now = Instant::now();
        mic.on_chunk(chunk(480, 0.9, now));
        let frames = ((now - last).as_secs_f64() * 48_000.0) as usize;
        system.on_chunk(chunk(frames, 0.2, now));
        last = now;
    }

    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(mute_status(&h), (false, false));
    assert_eq!(
        h.audio.live_of(TrackKind::Microphone),
        1,
        "mic stream reopened"
    );
    feed_for(&[mic, system], Duration::from_millis(300));
    let summary = block_on(h.service.stop()).unwrap();

    // The microphone file holds the two stretches of speech around a silent gap; the
    // system file has audio all along.
    let mic_audio = sample_ms(&h, TrackKind::Microphone);
    let system_audio = sample_ms(&h, TrackKind::System);
    assert!(mic_audio >= mic_before, "mic {mic_audio} ms");
    assert!(
        system_audio >= system_before + 650,
        "system audio went on: {system_before} -> {system_audio} ms"
    );
    assert!(
        system_audio > mic_audio + 300,
        "the muted span holds no microphone audio: mic {mic_audio} ms, system {system_audio} ms"
    );
    let gap = silence_ms(&h, TrackKind::Microphone);
    assert!(
        (300..700).contains(&gap),
        "silence in the mic file: {gap} ms"
    );

    // The timeline is not squeezed: both files have the same length, which is the session's.
    let mic_len = file_ms(&h, TrackKind::Microphone);
    let system_len = file_ms(&h, TrackKind::System);
    assert!(
        mic_len.abs_diff(system_len) < 250,
        "mic {mic_len} ms, system {system_len} ms"
    );
    let session_ms = summary.session.duration_ms().unwrap();
    assert!(
        mic_len.abs_diff(session_ms) < 300,
        "file {mic_len} ms, session {session_ms} ms"
    );
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);

    // One closed interval on the active time axis.
    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 1, "{mutes:?}");
    assert_eq!(mutes[0].kind, TrackKind::Microphone);
    let end_ms = mutes[0].end_ms.expect("the interval was closed");
    assert!(
        (150..600).contains(&mutes[0].start_ms),
        "start {}",
        mutes[0].start_ms
    );
    assert!(
        (300..700).contains(&(end_ms - mutes[0].start_ms)),
        "length {}",
        end_ms - mutes[0].start_ms
    );
    assert_eq!(
        mute_events(&h),
        [
            (TrackKind::Microphone, true),
            (TrackKind::Microphone, false)
        ]
    );
}

#[test]
fn muting_the_system_channel_leaves_the_microphone_open() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("system mute"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));

    let status = h.service.set_track_muted(TrackKind::System, true).unwrap();
    assert!(status.system_muted && !status.microphone_muted);
    assert_eq!(h.audio.live_of(TrackKind::System), 0);
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);

    // Asking for the state a channel is already in changes and publishes nothing.
    h.service.set_track_muted(TrackKind::System, true).unwrap();
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(mute_events(&h), [(TrackKind::System, true)]);
    block_on(h.service.stop()).unwrap();
}

#[test]
fn a_pause_wins_over_a_mute_and_the_channel_stays_closed_at_resume() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("pause + mute"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(150));
    h.service.pause().unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);

    // The state can be changed while paused; nothing opens or closes.
    let status = h
        .service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    assert!(status.microphone_muted);
    assert!(matches!(h.service.status(), RecordingStatus::Paused(_)));
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);

    h.service.resume().unwrap();
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 0, "still muted");
    assert_eq!(h.audio.live_of(TrackKind::System), 1);

    // A second pause and resume keep it closed.
    h.service.pause().unwrap();
    h.service.resume().unwrap();
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 0);

    // Unmuting while paused opens it at resume, not before.
    h.service.pause().unwrap();
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0, "paused: all closed");
    h.service.resume().unwrap();
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);
    assert_eq!(mute_status(&h), (false, false));

    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 1);
    assert!(mutes[0].end_ms.is_some());
}

#[test]
fn a_channel_that_cannot_reopen_stays_muted_and_warns_every_time() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("denied"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();

    *h.audio.failing.lock().unwrap() = vec![TrackKind::Microphone];
    let status = h
        .service
        .set_track_muted(TrackKind::Microphone, false)
        .expect("a source that will not open is a warning, not an error");
    assert!(status.microphone_muted, "the mute is not lifted");
    assert_eq!(mute_status(&h), (true, false));
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 0);
    assert_eq!(unmute_warnings(&h), 1);
    let warning = h
        .events
        .all()
        .into_iter()
        .find_map(|event| match event {
            RecordingEvent::Warning { warning, .. } => Some(warning),
            _ => None,
        })
        .unwrap();
    assert_eq!(warning.code, RecordingWarningCode::TrackUnmuteFailed);
    assert_eq!(warning.track, Some(TrackKind::Microphone));

    // Trying again is reported again, even though it is the same warning.
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(unmute_warnings(&h), 2);
    assert_eq!(mute_events(&h), [(TrackKind::Microphone, true)]);

    // Once the device works it unmutes.
    h.audio.failing.lock().unwrap().clear();
    let status = h
        .service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert!(!status.microphone_muted);
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);

    let summary = block_on(h.service.stop()).unwrap();
    let warnings: Vec<_> = summary
        .warnings
        .iter()
        .filter(|w| w.code == RecordingWarningCode::TrackUnmuteFailed)
        .collect();
    assert_eq!(warnings.len(), 1, "the summary keeps it once");
}

#[test]
fn pausing_and_resuming_with_every_audio_channel_muted_is_not_an_error() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("all muted"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    h.service.set_track_muted(TrackKind::System, true).unwrap();
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);

    h.service.pause().unwrap();
    h.service
        .resume()
        .expect("nothing to open is not a failure");
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);
    assert_eq!(mute_status(&h), (true, true));

    std::thread::sleep(Duration::from_millis(300));
    let summary = block_on(h.service.stop()).unwrap();
    assert_eq!(summary.session.status(), SessionStatus::Recorded);
    assert_eq!(summary.tracks.len(), 2);
    // Both files are padded with silence up to the end, so they stay aligned.
    let mic = file_ms(&h, TrackKind::Microphone);
    let system = file_ms(&h, TrackKind::System);
    assert!(
        mic.abs_diff(system) < 250,
        "mic {mic} ms, system {system} ms"
    );
    assert!(mic >= 300, "{mic} ms");

    // Both spans were closed at the stop.
    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 2);
    assert!(mutes.iter().all(|m| m.end_ms.is_some()), "{mutes:?}");
}

#[test]
fn a_microphone_only_recording_with_its_microphone_muted_survives_a_pause() {
    let h = harness();
    *h.audio.failing.lock().unwrap() = vec![TrackKind::System];
    block_on(h.service.start(StartOptions::titled("mic only"))).unwrap();
    h.audio.failing.lock().unwrap().clear();
    feed_for(
        &[h.audio.sink(TrackKind::Microphone)],
        Duration::from_millis(100),
    );

    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    h.service.pause().unwrap();
    h.service.resume().expect("no source to open, none failed");
    assert_eq!(h.audio.live.load(Ordering::SeqCst), 0);

    // The channel that never started cannot be muted.
    match h.service.set_track_muted(TrackKind::System, true) {
        Err(RecordingError::Invalid(_)) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);
    block_on(h.service.stop()).unwrap();
}

#[test]
fn muting_needs_a_recording() {
    let h = harness();
    match h.service.set_track_muted(TrackKind::Microphone, true) {
        Err(RecordingError::NotRecording) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(mute_events(&h).len(), 0);
}

#[test]
fn a_mute_still_open_at_the_stop_ends_at_the_stop() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("muted to the end"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(200));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let summary = block_on(h.service.stop()).unwrap();

    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 1);
    let session_ms = summary.session.duration_ms().unwrap();
    assert_eq!(mutes[0].end_ms, Some(session_ms));
    let mic = file_ms(&h, TrackKind::Microphone);
    assert!(mic.abs_diff(session_ms) < 250, "{mic} vs {session_ms}");
}

#[test]
fn recovery_closes_the_mute_a_crash_left_open() {
    let h = harness();
    let crashed = interrupted_session("crashed-muted", 1_000_000);
    block_on(h.sessions.create(&crashed)).unwrap();
    h.media.on_disk.lock().unwrap().insert(
        ("crashed-muted".to_owned(), TrackKind::System),
        AudioFileInfo {
            relative_path: "audio/system.wav".to_owned(),
            sample_rate: 48_000,
            channels: 1,
            frames: 48_000 * 60,
        },
    );
    block_on(
        h.sessions
            .open_track_mute(&crashed.id, TrackKind::Microphone, 20_000),
    )
    .unwrap();

    block_on(h.service.recover_interrupted()).unwrap();

    let mutes = block_on(h.sessions.list_track_mutes(&crashed.id)).unwrap();
    assert_eq!(
        mutes,
        [TrackMute {
            session_id: crashed.id.clone(),
            kind: TrackKind::Microphone,
            start_ms: 20_000,
            end_ms: Some(60_000),
        }]
    );
}

#[test]
fn a_mute_lifted_during_a_pause_stays_muted_with_a_warning_when_the_source_will_not_reopen() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("pause + unmute"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    h.service.pause().unwrap();
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert_eq!(mute_status(&h), (false, false), "paused: looks unmuted");

    // The microphone is gone by the time the recording resumes.
    *h.audio.failing.lock().unwrap() = vec![TrackKind::Microphone];
    h.service
        .resume()
        .expect("the system channel opened, so the recording resumes");
    assert!(matches!(h.service.status(), RecordingStatus::Recording(_)));
    assert_eq!(
        mute_status(&h),
        (true, false),
        "the channel is not reported open while nothing is captured"
    );
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 0);
    assert_eq!(h.audio.live_of(TrackKind::System), 1);
    assert_eq!(unmute_warnings(&h), 1);
    assert_eq!(
        mute_events(&h).last(),
        Some(&(TrackKind::Microphone, true)),
        "the correction is published"
    );

    // And it can be tried again once the device is back.
    h.audio.failing.lock().unwrap().clear();
    let status = h
        .service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    assert!(!status.microphone_muted);
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);

    block_on(h.service.stop()).unwrap();
    // Closed (zero length at the pause), then muted again from the resume until the retry.
    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 2, "{mutes:?}");
    assert!(mutes.iter().all(|m| m.end_ms.is_some()), "{mutes:?}");
}

#[test]
fn a_mute_lifted_during_a_pause_that_cannot_reopen_does_not_fail_the_resume_when_all_else_is_muted()
{
    let h = harness();
    block_on(h.service.start(StartOptions::titled("pause + unmute"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service.set_track_muted(TrackKind::System, true).unwrap();
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    h.service.pause().unwrap();
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();

    *h.audio.failing.lock().unwrap() = vec![TrackKind::Microphone];
    h.service.resume().expect("everything is muted again");
    assert_eq!(mute_status(&h), (true, true));
    assert_eq!(unmute_warnings(&h), 1);
    block_on(h.service.stop()).unwrap();
}

#[test]
fn a_failed_interval_closing_does_not_stretch_the_interval_over_the_next_mute() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("close fails"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));

    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    h.sessions.fail_close.store(true, Ordering::SeqCst);
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    h.sessions.fail_close.store(false, Ordering::SeqCst);
    assert_eq!(
        mute_status(&h),
        (false, false),
        "unmuted despite the failure"
    );

    // The microphone records for a while, then is muted again, then unmuted.
    feed_for(&both_sinks(&h), Duration::from_millis(300));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    block_on(h.service.stop()).unwrap();

    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 2, "{mutes:?}");
    let first_end = mutes[0].end_ms.expect("the first interval was closed");
    assert!(
        first_end < mutes[1].start_ms,
        "the microphone recorded between the intervals: {mutes:?}"
    );
    assert!(mutes[1].end_ms.is_some(), "{mutes:?}");
}

#[test]
fn quick_toggles_from_two_places_each_flip_the_state_they_find() {
    let h = harness();
    block_on(h.service.start(StartOptions::titled("toggles"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(50));

    // Two presses racing (tray and shortcut): one ends muted, the other unmuted again, never
    // two presses that both mean "mute".
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                h.service.toggle_track_muted(TrackKind::Microphone).unwrap();
            });
        }
    });
    assert_eq!(mute_status(&h), (false, false));
    assert_eq!(
        mute_events(&h),
        [
            (TrackKind::Microphone, true),
            (TrackKind::Microphone, false)
        ]
    );

    let status = h.service.toggle_track_muted(TrackKind::Microphone).unwrap();
    assert!(status.microphone_muted);
    block_on(h.service.stop()).unwrap();
    match h.service.toggle_track_muted(TrackKind::Microphone) {
        Err(RecordingError::NotRecording) => {}
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn a_failed_interval_closing_is_retried_at_the_stop_with_the_real_end() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("close fails at stop"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    h.sessions.fail_close.store(true, Ordering::SeqCst);
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();
    h.sessions.fail_close.store(false, Ordering::SeqCst);

    // The microphone records for a good while after the unmute, with no further mute.
    feed_for(&both_sinks(&h), Duration::from_millis(500));
    let summary = block_on(h.service.stop()).unwrap();

    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 1, "{mutes:?}");
    let end = mutes[0].end_ms.expect("closed at the stop");
    let session_ms = summary.session.duration_ms().unwrap();
    assert!(
        end + 400 < session_ms,
        "the interval ended at the unmute ({end} ms), not at the stop ({session_ms} ms)"
    );
}

#[test]
fn a_mute_is_refused_while_the_previous_interval_still_cannot_be_closed() {
    let h = harness();
    let session = block_on(h.service.start(StartOptions::titled("still failing"))).unwrap();
    feed_for(&both_sinks(&h), Duration::from_millis(100));
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    h.sessions.fail_close.store(true, Ordering::SeqCst);
    h.service
        .set_track_muted(TrackKind::Microphone, false)
        .unwrap();

    match h.service.set_track_muted(TrackKind::Microphone, true) {
        Err(RecordingError::Storage(_)) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(
        mute_status(&h),
        (false, false),
        "the channel is left as it was"
    );
    assert_eq!(h.audio.live_of(TrackKind::Microphone), 1);

    // Once the database works again the mute goes through, with no stale open row.
    h.sessions.fail_close.store(false, Ordering::SeqCst);
    h.service
        .set_track_muted(TrackKind::Microphone, true)
        .unwrap();
    block_on(h.service.stop()).unwrap();
    let mutes = block_on(h.sessions.list_track_mutes(&session.id)).unwrap();
    assert_eq!(mutes.len(), 2, "{mutes:?}");
    assert!(mutes.iter().all(|m| m.end_ms.is_some()), "{mutes:?}");
    assert!(mutes[0].end_ms.unwrap() <= mutes[1].start_ms, "{mutes:?}");
}
