//! The recording use case.
//!
//! ```text
//! mic audio thread     --PcmChunk--> bounded queue --> track worker --> audio/microphone.wav
//! system audio thread  --PcmChunk--> bounded queue --> track worker --> audio/system.wav
//! screen thread        --RawFrame--> bounded queue --> frame worker --> frames/*.png + DB rows
//! ticker thread (250 ms)                                      --> RecordingEvent::Progress
//! ```
//!
//! Capture callbacks only enqueue (and drop on overflow, counting it); all file I/O, hashing
//! and database writes happen on worker threads. Each audio track has a [`TrackTimeline`]
//! that places every block on the shared monotonic timeline and inserts silence for gaps, so
//! both files start at the recording start and stay aligned.
//!
//! Crash safety: audio files are checkpointed every few seconds
//! ([`CHECKPOINT_INTERVAL`]), frame rows are inserted as they are kept, and the session
//! stays `Recording` in the database until `stop`. [`RecordingService::recover_interrupted`]
//! settles whatever a crash left behind.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tan_domain::{
    AudioTrack, EpochMs, Frame, FrameId, Session, SessionId, SessionKind, SessionStatus, TrackKind,
};

use crate::ports::{
    AudioCapture, AudioFileInfo, AudioFormat, AudioSink, AudioStream, AudioStreamRequest,
    CaptureError, Clock, FrameHasher, FrameRepository, FrameSink, IdGenerator, MediaStore,
    PcmChunk, RawFrame, ScreenAvailability, ScreenCapture, ScreenStream, ScreenStreamRequest,
    SessionRepository, SettingsRepository,
};

use super::frame_selector::{FrameDecision, FrameSelector, FrameSelectorConfig};
use super::{
    ActiveRecordingStatus, RecordingError, RecordingEvent, RecordingEvents, RecordingProgress,
    RecordingStatus, RecordingSummary, RecoveredSession, StartOptions, TrackSummary, MAX_TITLE_LEN,
};

/// How often the audio file headers are made durable. A crash loses at most this much audio.
pub const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);

/// A track that falls behind the shared timeline by more than this gets silence inserted
/// (smaller lags are ordinary delivery jitter).
const GAP_THRESHOLD: Duration = Duration::from_millis(250);

/// Audio blocks a track queue can hold (about ten seconds of 10 ms blocks).
const TRACK_QUEUE_CAPACITY: usize = 1_024;

/// Frames waiting for the frame worker. Captures are seconds apart, so two is plenty; more
/// would only hold large pixel buffers in memory.
const FRAME_QUEUE_CAPACITY: usize = 2;

/// Interval of [`RecordingEvent::Progress`].
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Warnings kept for the summary; further distinct ones are only counted by the event stream.
const MAX_WARNINGS: usize = 32;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked; the data is plain counters/lists.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------
// block_on
// ---------------------------------------------------------------------------------------

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs a future to completion on the current (non-async) worker thread.
///
/// The frame worker is a plain thread but the repository ports are async-shaped. This
/// executor polls on the calling thread and parks while pending. It does not provide a
/// reactor: the repositories used here must not depend on a runtime to make progress (the
/// SQLite adapters complete synchronously).
fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------------------

/// Where an audio track stands on the shared timeline.
///
/// Pure bookkeeping, no I/O: it says how much silence must precede the next block so that
/// the file position matches the monotonic clock.
#[derive(Debug, Clone)]
struct TrackTimeline {
    sample_rate: u32,
    written_frames: u64,
    started: bool,
}

impl TrackTimeline {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            written_frames: 0,
            started: false,
        }
    }

    fn frames_at(&self, offset: Duration) -> u64 {
        let frames = offset.as_nanos() * u128::from(self.sample_rate) / 1_000_000_000;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// Frames of silence to write before a block whose audio starts `block_start` after the
    /// recording origin. The very first block is aligned exactly (it carries the source's
    /// start-up delay); later blocks only when the lag exceeds [`GAP_THRESHOLD`].
    fn silence_before(&self, block_start: Duration) -> u64 {
        let lag = self
            .frames_at(block_start)
            .saturating_sub(self.written_frames);
        if !self.started || lag > self.frames_at(GAP_THRESHOLD) {
            lag
        } else {
            0
        }
    }

    /// Frames of silence needed to bring the track up to `stop` (never negative: a track that
    /// is already longer is not trimmed).
    fn tail_silence(&self, stop: Duration) -> u64 {
        self.frames_at(stop).saturating_sub(self.written_frames)
    }

    fn advance(&mut self, frames: u64) {
        self.written_frames += frames;
        self.started = true;
    }

    fn frames_to_ms(&self, frames: u64) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        frames.saturating_mul(1_000) / u64::from(self.sample_rate)
    }
}

// ---------------------------------------------------------------------------------------
// Shared bookkeeping
// ---------------------------------------------------------------------------------------

/// Sum of squares of the samples since the last [`LevelMeter::take_rms`].
#[derive(Default)]
struct LevelMeter {
    inner: Mutex<(f64, u64)>,
}

impl LevelMeter {
    fn add(&self, samples: &[f32]) {
        let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
        let mut inner = lock(&self.inner);
        inner.0 += sum;
        inner.1 += samples.len() as u64;
    }

    /// RMS since the previous call (`0.0` when no audio arrived), clamped to `0.0..=1.0`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn take_rms(&self) -> f32 {
        let (sum, count) = std::mem::take(&mut *lock(&self.inner));
        if count == 0 {
            return 0.0;
        }
        ((sum / count as f64).sqrt() as f32).clamp(0.0, 1.0)
    }
}

/// Non-fatal problems of a recording: kept for the summary and published as events.
struct WarningLog {
    session_id: SessionId,
    events: Arc<dyn RecordingEvents>,
    items: Mutex<Vec<String>>,
}

impl WarningLog {
    fn push(&self, message: String) {
        {
            let mut items = lock(&self.items);
            if items.len() >= MAX_WARNINGS || items.contains(&message) {
                return;
            }
            items.push(message.clone());
        }
        self.events.publish(RecordingEvent::Warning {
            session_id: self.session_id.clone(),
            message,
        });
    }

    fn snapshot(&self) -> Vec<String> {
        lock(&self.items).clone()
    }
}

#[derive(Default)]
struct TrackShared {
    meter: LevelMeter,
    dropped_chunks: AtomicU64,
    stream_issues: AtomicU32,
}

#[derive(Default)]
struct FrameShared {
    kept: AtomicU32,
    dropped: AtomicU32,
}

fn load_level(bits: &AtomicU32) -> f32 {
    f32::from_bits(bits.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------------------
// Audio track worker
// ---------------------------------------------------------------------------------------

enum TrackMessage {
    Chunk(PcmChunk),
    /// Finish the file, padding it with silence up to `stop_offset` from the origin.
    Finish {
        stop_offset: Duration,
    },
}

struct TrackSink {
    kind: TrackKind,
    queue: SyncSender<TrackMessage>,
    shared: Arc<TrackShared>,
    warnings: Arc<WarningLog>,
}

impl AudioSink for TrackSink {
    fn on_chunk(&self, chunk: PcmChunk) {
        match self.queue.try_send(TrackMessage::Chunk(chunk)) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.shared.dropped_chunks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn on_error(&self, message: String) {
        self.shared.stream_issues.fetch_add(1, Ordering::Relaxed);
        self.warnings
            .push(format!("{} audio: {message}", self.kind));
    }
}

struct TrackOutcome {
    file: Option<AudioFileInfo>,
    peak: f32,
    first_chunk_offset_ms: u64,
    silence_ms: u64,
    failure: Option<String>,
}

struct TrackWorker {
    session_id: SessionId,
    kind: TrackKind,
    origin: Instant,
    media: Arc<dyn MediaStore>,
    shared: Arc<TrackShared>,
    writer: Option<Box<dyn crate::ports::AudioWriter>>,
    format: Option<AudioFormat>,
    timeline: Option<TrackTimeline>,
    failure: Option<String>,
    peak: f32,
    first_chunk_offset: Option<Duration>,
    silence_frames: u64,
    frames_since_checkpoint: u64,
}

impl TrackWorker {
    fn run(mut self, queue: &Receiver<TrackMessage>) -> TrackOutcome {
        let stop_offset = loop {
            match queue.recv() {
                Ok(TrackMessage::Chunk(chunk)) => self.on_chunk(&chunk),
                Ok(TrackMessage::Finish { stop_offset }) => break Some(stop_offset),
                // Every sender is gone (the recording was dropped without `stop`).
                Err(_) => break None,
            }
        };
        self.finish(stop_offset)
    }

    fn fail(&mut self, what: &str, error: &dyn std::fmt::Display) {
        if self.failure.is_none() {
            self.failure = Some(format!("{what}: {error}"));
        }
    }

    fn on_chunk(&mut self, chunk: &PcmChunk) {
        if chunk.frame_count() == 0 {
            return;
        }
        // Levels and peak keep working even if the file failed, so the UI still shows a
        // live source.
        self.shared.meter.add(&chunk.samples);
        let peak = chunk.samples.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        self.peak = self.peak.max(peak);
        if self.failure.is_some() {
            return;
        }

        if self.writer.is_none() {
            match self
                .media
                .open_audio_writer(&self.session_id, self.kind, chunk.format)
            {
                Ok(writer) => {
                    self.writer = Some(writer);
                    self.format = Some(chunk.format);
                    self.timeline = Some(TrackTimeline::new(chunk.format.sample_rate));
                }
                Err(e) => {
                    self.fail("cannot create the audio file", &e);
                    return;
                }
            }
        } else if self.format != Some(chunk.format) {
            // A device that changes format mid-stream cannot be appended to the same file.
            self.shared.stream_issues.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let (Some(timeline), Some(writer)) = (self.timeline.as_mut(), self.writer.as_mut()) else {
            return;
        };
        let frames = chunk.frame_count();
        // `captured_at` is the end of the block; the audio covers the `duration` before it.
        let block_start = chunk
            .captured_at
            .saturating_duration_since(self.origin)
            .saturating_sub(chunk.duration());
        self.first_chunk_offset.get_or_insert(block_start);

        let silence = timeline.silence_before(block_start);
        let mut written = 0;
        let mut result = Ok(());
        if silence > 0 {
            result = writer.write_silence(silence);
            if result.is_ok() {
                written += silence;
                self.silence_frames += silence;
            }
        }
        if result.is_ok() {
            result = writer.write(&chunk.samples);
            if result.is_ok() {
                written += frames;
            }
        }
        timeline.advance(written);
        self.frames_since_checkpoint += written;

        if result.is_ok() {
            let interval_frames =
                u64::from(chunk.format.sample_rate).saturating_mul(CHECKPOINT_INTERVAL.as_secs());
            if self.frames_since_checkpoint >= interval_frames {
                self.frames_since_checkpoint = 0;
                result = writer.checkpoint();
            }
        }
        if let Err(e) = result {
            self.fail("cannot write audio", &e);
        }
    }

    fn finish(mut self, stop_offset: Option<Duration>) -> TrackOutcome {
        let mut file = None;
        if let (Some(mut writer), Some(timeline)) = (self.writer.take(), self.timeline.as_ref()) {
            if self.failure.is_none() {
                if let Some(stop) = stop_offset {
                    let tail = timeline.tail_silence(stop);
                    if tail > 0 {
                        match writer.write_silence(tail) {
                            Ok(()) => self.silence_frames += tail,
                            Err(e) => self.fail("cannot write audio", &e),
                        }
                    }
                }
            }
            match writer.finish() {
                Ok(info) => file = Some(info),
                Err(e) => self.fail("cannot finish the audio file", &e),
            }
        }
        let silence_ms = self
            .timeline
            .as_ref()
            .map_or(0, |t| t.frames_to_ms(self.silence_frames));
        TrackOutcome {
            file,
            peak: self.peak,
            first_chunk_offset_ms: self
                .first_chunk_offset
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            silence_ms,
            failure: self.failure,
        }
    }
}

struct TrackHandle {
    kind: TrackKind,
    queue: SyncSender<TrackMessage>,
    worker: JoinHandle<TrackOutcome>,
    shared: Arc<TrackShared>,
    stream: Box<dyn AudioStream>,
}

// ---------------------------------------------------------------------------------------
// Frame worker
// ---------------------------------------------------------------------------------------

enum FrameMessage {
    Frame(RawFrame),
    Stop,
}

struct ScreenSink {
    queue: SyncSender<FrameMessage>,
    shared: Arc<FrameShared>,
    warnings: Arc<WarningLog>,
}

impl FrameSink for ScreenSink {
    fn on_frame(&self, frame: RawFrame) {
        match self.queue.try_send(FrameMessage::Frame(frame)) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn on_error(&self, message: String) {
        self.warnings.push(format!("screen: {message}"));
    }
}

struct FrameWorker<F> {
    session_id: SessionId,
    origin: Instant,
    selector: FrameSelector,
    hasher: Arc<dyn FrameHasher>,
    media: Arc<dyn MediaStore>,
    frames: Arc<F>,
    ids: Arc<dyn IdGenerator>,
    shared: Arc<FrameShared>,
    warnings: Arc<WarningLog>,
}

impl<F: FrameRepository> FrameWorker<F> {
    fn run(mut self, queue: &Receiver<FrameMessage>) {
        while let Ok(FrameMessage::Frame(frame)) = queue.recv() {
            self.process(&frame);
        }
    }

    fn process(&mut self, frame: &RawFrame) {
        let timestamp_ms = u64::try_from(
            frame
                .captured_at
                .saturating_duration_since(self.origin)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let hash = self.hasher.hash(frame);
        if self.selector.decide(timestamp_ms, hash) == FrameDecision::Skip {
            return;
        }
        let path = match self.media.save_frame(&self.session_id, timestamp_ms, frame) {
            Ok(path) => path,
            Err(e) => {
                self.warnings.push(format!("cannot store a frame: {e}"));
                return;
            }
        };
        let row = Frame {
            id: FrameId::new(self.ids.new_id()),
            session_id: self.session_id.clone(),
            timestamp_ms,
            path,
            perceptual_hash: hash,
            description: None,
        };
        match block_on(self.frames.add(&row)) {
            Ok(()) => {
                self.shared.kept.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => self
                .warnings
                .push(format!("cannot record a frame in the database: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------------------

/// The capture-side ports and helpers a [`RecordingService`] needs.
pub struct RecordingPorts {
    /// Microphone and system audio.
    pub audio: Arc<dyn AudioCapture>,
    /// Screen sampling.
    pub screen: Arc<dyn ScreenCapture>,
    /// Perceptual hashing of frames.
    pub hasher: Arc<dyn FrameHasher>,
    /// Session files.
    pub media: Arc<dyn MediaStore>,
    /// Id generation for sessions and frames.
    pub ids: Arc<dyn IdGenerator>,
    /// Wall clock for the session start time.
    pub clock: Arc<dyn Clock>,
    /// Where progress and warnings go.
    pub events: Arc<dyn RecordingEvents>,
}

struct ScreenHandle {
    stream: Box<dyn ScreenStream>,
    worker: JoinHandle<()>,
    queue: SyncSender<FrameMessage>,
}

struct ActiveRecording {
    session: Session,
    origin: Instant,
    tracks: Vec<TrackHandle>,
    screen: Option<ScreenHandle>,
    frame_shared: Arc<FrameShared>,
    ticker: (mpsc::Sender<()>, Option<JoinHandle<()>>),
    levels: Arc<Levels>,
    warnings: Arc<WarningLog>,
}

/// The levels the ticker last measured, readable without waiting for the next event.
#[derive(Default)]
struct Levels {
    microphone: AtomicU32,
    system: AtomicU32,
}

enum State {
    Idle,
    Starting,
    Recording(Box<ActiveRecording>),
    Stopping,
}

/// Starts, stops and recovers recordings. At most one recording runs at a time.
pub struct RecordingService<S, F, P> {
    sessions: Arc<S>,
    frames: Arc<F>,
    settings: Arc<P>,
    audio: Arc<dyn AudioCapture>,
    screen: Arc<dyn ScreenCapture>,
    hasher: Arc<dyn FrameHasher>,
    media: Arc<dyn MediaStore>,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
    events: Arc<dyn RecordingEvents>,
    state: Mutex<State>,
}

impl<S, F, P> RecordingService<S, F, P>
where
    S: SessionRepository + 'static,
    F: FrameRepository + 'static,
    P: SettingsRepository + 'static,
{
    /// Creates the service.
    #[must_use]
    pub fn new(sessions: Arc<S>, frames: Arc<F>, settings: Arc<P>, ports: RecordingPorts) -> Self {
        Self {
            sessions,
            frames,
            settings,
            audio: ports.audio,
            screen: ports.screen,
            hasher: ports.hasher,
            media: ports.media,
            ids: ports.ids,
            clock: ports.clock,
            events: ports.events,
            state: Mutex::new(State::Idle),
        }
    }

    /// Whether the screen can be sampled in this session. When it cannot, `start` records
    /// audio only and reports no error.
    #[must_use]
    pub fn screen_availability(&self) -> ScreenAvailability {
        self.screen.availability()
    }

    /// Whether a recording is running (or starting, or stopping).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        !matches!(*lock(&self.state), State::Idle)
    }

    /// What is being recorded right now.
    #[must_use]
    pub fn status(&self) -> RecordingStatus {
        match &*lock(&self.state) {
            State::Recording(active) => RecordingStatus::Recording(ActiveRecordingStatus {
                session_id: active.session.id.clone(),
                title: active.session.title.clone(),
                started_at: active.session.started_at,
                elapsed_ms: elapsed_ms(active.origin),
                microphone_level: load_level(&active.levels.microphone),
                system_level: load_level(&active.levels.system),
                frame_count: active.frame_shared.kept.load(Ordering::Relaxed),
            }),
            State::Idle | State::Starting | State::Stopping => RecordingStatus::Idle,
        }
    }

    /// Starts recording a new session.
    ///
    /// The microphone and the system audio start independently: if one fails the recording
    /// goes on with the other (the failure is reported as a warning). If neither can start,
    /// nothing is left behind and the first error is returned. A screen that cannot be
    /// captured is a warning too.
    pub async fn start(&self, options: StartOptions) -> Result<Session, RecordingError> {
        let title = options.title.trim().to_owned();
        if title.is_empty() {
            return Err(RecordingError::Invalid(
                "the title must not be blank".to_owned(),
            ));
        }
        if title.chars().count() > MAX_TITLE_LEN {
            return Err(RecordingError::Invalid(format!(
                "the title must not exceed {MAX_TITLE_LEN} characters"
            )));
        }

        {
            let mut state = lock(&self.state);
            if !matches!(*state, State::Idle) {
                return Err(RecordingError::AlreadyRecording);
            }
            *state = State::Starting;
        }

        match self.start_inner(options, title).await {
            Ok(active) => {
                let session = active.session.clone();
                *lock(&self.state) = State::Recording(Box::new(active));
                self.events.publish(RecordingEvent::Started {
                    session_id: session.id.clone(),
                    title: session.title.clone(),
                });
                Ok(session)
            }
            Err(error) => {
                *lock(&self.state) = State::Idle;
                Err(error)
            }
        }
    }

    async fn start_inner(
        &self,
        options: StartOptions,
        title: String,
    ) -> Result<ActiveRecording, RecordingError> {
        let settings = self.settings.load().await?;
        let session = Session::start_recording(
            SessionId::new(self.ids.new_id()),
            SessionKind::Meeting,
            title,
            self.clock.now(),
            options
                .speech_language
                .unwrap_or_else(|| settings.default_speech_language.clone()),
            options
                .note_language
                .unwrap_or_else(|| settings.default_note_language.clone()),
        );

        self.sessions.create(&session).await?;
        if let Err(e) = self.media.prepare_session(&session.id) {
            let _ = self.sessions.delete(&session.id).await;
            return Err(e.into());
        }

        let selector = FrameSelectorConfig::with_threshold(settings.frame_change_threshold);
        let interval = Duration::from_millis(u64::from(settings.frame_interval_ms));
        match self.launch(
            session.clone(),
            options.microphone_device_id,
            options.display_id,
            selector,
            interval,
        ) {
            Ok(active) => Ok(active),
            Err(error) => {
                // Nothing was captured: leave no empty session or folder behind.
                let _ = self.media.remove_session(&session.id);
                let _ = self.sessions.delete(&session.id).await;
                Err(error)
            }
        }
    }

    /// Starts the capture streams and the worker threads (synchronous part of `start`).
    fn launch(
        &self,
        session: Session,
        microphone_device_id: Option<String>,
        display_id: Option<String>,
        selector: FrameSelectorConfig,
        interval: Duration,
    ) -> Result<ActiveRecording, RecordingError> {
        let origin = Instant::now();
        let warnings = Arc::new(WarningLog {
            session_id: session.id.clone(),
            events: self.events.clone(),
            items: Mutex::default(),
        });

        let mut tracks = Vec::new();
        let mut first_error: Option<CaptureError> = None;
        for (kind, device_id) in [
            (TrackKind::Microphone, microphone_device_id),
            (TrackKind::System, None),
        ] {
            match self.start_track(&session.id, kind, device_id, origin, &warnings) {
                Ok(track) => tracks.push(track),
                Err(error) => {
                    warnings.push(format!("{kind} audio could not start: {error}"));
                    first_error.get_or_insert(error);
                }
            }
        }
        if tracks.is_empty() {
            return Err(RecordingError::Capture(first_error.unwrap_or_else(|| {
                CaptureError::failed("no audio source is available")
            })));
        }

        let frame_shared = Arc::new(FrameShared::default());
        let screen = self.start_screen(
            &session.id,
            display_id,
            selector,
            interval,
            origin,
            &frame_shared,
            &warnings,
        );

        let levels = Arc::new(Levels::default());
        let ticker = self.start_ticker(&session.id, origin, &tracks, &levels, &frame_shared);

        Ok(ActiveRecording {
            session,
            origin,
            tracks,
            screen,
            frame_shared,
            ticker,
            levels,
            warnings,
        })
    }

    fn start_track(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        device_id: Option<String>,
        origin: Instant,
        warnings: &Arc<WarningLog>,
    ) -> Result<TrackHandle, CaptureError> {
        let shared = Arc::new(TrackShared::default());
        let (queue, receiver) = mpsc::sync_channel(TRACK_QUEUE_CAPACITY);
        let sink = Arc::new(TrackSink {
            kind,
            queue: queue.clone(),
            shared: shared.clone(),
            warnings: warnings.clone(),
        });
        let stream = self.audio.start(
            &AudioStreamRequest {
                source: kind,
                device_id,
            },
            sink,
        )?;

        let worker = TrackWorker {
            session_id: session_id.clone(),
            kind,
            origin,
            media: self.media.clone(),
            shared: shared.clone(),
            writer: None,
            format: None,
            timeline: None,
            failure: None,
            peak: 0.0,
            first_chunk_offset: None,
            silence_frames: 0,
            frames_since_checkpoint: 0,
        };
        let spawned = thread::Builder::new()
            .name(format!("tan-track-{kind}"))
            .spawn(move || worker.run(&receiver));
        match spawned {
            Ok(worker) => Ok(TrackHandle {
                kind,
                queue,
                worker,
                shared,
                stream,
            }),
            Err(e) => {
                stream.stop();
                Err(CaptureError::failed(format!(
                    "cannot start the {kind} writer thread: {e}"
                )))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_screen(
        &self,
        session_id: &SessionId,
        display_id: Option<String>,
        selector: FrameSelectorConfig,
        interval: Duration,
        origin: Instant,
        shared: &Arc<FrameShared>,
        warnings: &Arc<WarningLog>,
    ) -> Option<ScreenHandle> {
        // An unusable screen (Wayland) is a platform fact, not a failure: skip it quietly and
        // record audio only. The UI explains it from `screen_availability`.
        if self.screen.availability() != ScreenAvailability::Available {
            return None;
        }
        let (queue, receiver) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let sink = Arc::new(ScreenSink {
            queue: queue.clone(),
            shared: shared.clone(),
            warnings: warnings.clone(),
        });
        let stream = match self.screen.start(
            &ScreenStreamRequest {
                display_id,
                interval,
            },
            sink,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                warnings.push(format!("the screen could not be captured: {error}"));
                return None;
            }
        };

        let worker = FrameWorker {
            session_id: session_id.clone(),
            origin,
            selector: FrameSelector::new(selector),
            hasher: self.hasher.clone(),
            media: self.media.clone(),
            frames: self.frames.clone(),
            ids: self.ids.clone(),
            shared: shared.clone(),
            warnings: warnings.clone(),
        };
        match thread::Builder::new()
            .name("tan-frames".to_owned())
            .spawn(move || worker.run(&receiver))
        {
            Ok(worker) => Some(ScreenHandle {
                stream,
                worker,
                queue,
            }),
            Err(e) => {
                stream.stop();
                warnings.push(format!("cannot start the frame worker: {e}"));
                None
            }
        }
    }

    fn start_ticker(
        &self,
        session_id: &SessionId,
        origin: Instant,
        tracks: &[TrackHandle],
        levels: &Arc<Levels>,
        frame_shared: &Arc<FrameShared>,
    ) -> (mpsc::Sender<()>, Option<JoinHandle<()>>) {
        let (stop, stopped) = mpsc::channel::<()>();
        let session_id = session_id.clone();
        let events = self.events.clone();
        let levels = levels.clone();
        let frame_shared = frame_shared.clone();
        let mic = shared_of(tracks, TrackKind::Microphone);
        let system = shared_of(tracks, TrackKind::System);
        // A ticker is cosmetic: if no thread can be had, recording still works.
        let handle = thread::Builder::new()
            .name("tan-progress".to_owned())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(PROGRESS_INTERVAL) {
                    let measure = |track: &Option<Arc<TrackShared>>, slot: &AtomicU32| {
                        let level = track.as_ref().map_or(0.0, |t| t.meter.take_rms());
                        slot.store(level.to_bits(), Ordering::Relaxed);
                        level
                    };
                    let microphone_level = measure(&mic, &levels.microphone);
                    let system_level = measure(&system, &levels.system);
                    events.publish(RecordingEvent::Progress(RecordingProgress {
                        session_id: session_id.clone(),
                        elapsed_ms: elapsed_ms(origin),
                        microphone_level,
                        system_level,
                        frame_count: frame_shared.kept.load(Ordering::Relaxed),
                    }));
                }
            })
            .ok();
        (stop, handle)
    }

    /// Stops the running recording and settles its session (`Recorded`, or `Failed` when
    /// nothing at all was captured).
    ///
    /// Blocks the calling thread briefly while the streams stop and the files are closed.
    /// If the database fails at the very end the recording is over regardless; the session
    /// then stays `Recording` and [`Self::recover_interrupted`] settles it at next start.
    pub async fn stop(&self) -> Result<RecordingSummary, RecordingError> {
        let active = {
            let mut state = lock(&self.state);
            match std::mem::replace(&mut *state, State::Stopping) {
                State::Recording(active) => active,
                other => {
                    *state = other;
                    return Err(RecordingError::NotRecording);
                }
            }
        };
        let result = self.settle(*active).await;
        *lock(&self.state) = State::Idle;
        if let Ok(summary) = &result {
            self.events.publish(RecordingEvent::Stopped {
                session_id: summary.session.id.clone(),
            });
        }
        result
    }

    async fn settle(&self, active: ActiveRecording) -> Result<RecordingSummary, RecordingError> {
        let ActiveRecording {
            mut session,
            origin,
            tracks,
            screen,
            frame_shared,
            ticker,
            levels: _,
            warnings,
        } = active;

        // One stop instant for every stream; tracks are padded up to it.
        let stop_offset = origin.elapsed();
        let (ticker_stop, ticker_handle) = ticker;
        drop(ticker_stop);
        if let Some(handle) = ticker_handle {
            let _ = handle.join();
        }

        if let Some(ScreenHandle {
            stream,
            worker,
            queue,
        }) = screen
        {
            stream.stop();
            let _ = queue.send(FrameMessage::Stop);
            if worker.join().is_err() {
                warnings.push("the frame worker stopped unexpectedly".to_owned());
            }
        }

        let (summaries, stored) = finish_tracks(tracks, stop_offset, &session.id, &warnings);

        for track in &stored {
            self.sessions.save_audio_track(track).await?;
        }

        let frame_count = frame_shared.kept.load(Ordering::Relaxed);
        let dropped_frames = frame_shared.dropped.load(Ordering::Relaxed);
        if dropped_frames > 0 {
            warnings.push(format!(
                "{dropped_frames} frame(s) were dropped because processing could not keep up"
            ));
        }

        let ended_at = EpochMs::new(
            session
                .started_at
                .get()
                .saturating_add(i64::try_from(stop_offset.as_millis()).unwrap_or(i64::MAX)),
        );
        let nothing_captured = stored.is_empty() && frame_count == 0;
        let transition = if nothing_captured {
            warnings.push("nothing was captured".to_owned());
            session.mark_failed(ended_at)
        } else {
            session.finish_recording(ended_at)
        };
        transition.map_err(|e| RecordingError::Invalid(e.to_string()))?;
        self.sessions.update(&session).await?;

        Ok(RecordingSummary {
            session,
            tracks: summaries,
            frame_count,
            dropped_frames,
            warnings: warnings.snapshot(),
        })
    }

    /// Settles sessions a crash left in `Recording`: audio found on disk is attached as
    /// tracks, the end time is derived from the longest audio file or the last frame, and
    /// the session becomes `Recorded` (or `Failed` when neither audio nor frames survived).
    ///
    /// Meant to run once at startup. Does nothing while a recording is running.
    pub async fn recover_interrupted(&self) -> Result<Vec<RecoveredSession>, RecordingError> {
        if self.is_busy() {
            return Ok(Vec::new());
        }
        let mut recovered = Vec::new();
        for mut session in self.sessions.list().await? {
            if session.status() != SessionStatus::Recording {
                continue;
            }
            let mut tracks = Vec::new();
            for &kind in TrackKind::ALL {
                // An unreadable file is the same as no file: there is nothing to attach.
                if let Ok(Some(file)) = self.media.probe_audio(&session.id, kind) {
                    if file.frames > 0 {
                        tracks.push(AudioTrack {
                            session_id: session.id.clone(),
                            kind,
                            path: file.relative_path.clone(),
                            sample_rate: file.sample_rate,
                            duration_ms: file.duration_ms(),
                        });
                    }
                }
            }
            let frames = self.frames.list_for_session(&session.id).await?;

            let audio_ms = tracks.iter().map(|t| t.duration_ms).max().unwrap_or(0);
            let last_frame_ms = frames.iter().map(|f| f.timestamp_ms).max().unwrap_or(0);
            let length_ms = audio_ms.max(last_frame_ms);
            let ended_at = EpochMs::new(
                session
                    .started_at
                    .get()
                    .saturating_add(i64::try_from(length_ms).unwrap_or(i64::MAX)),
            );

            for track in &tracks {
                self.sessions.save_audio_track(track).await?;
            }
            let transition = if tracks.is_empty() && frames.is_empty() {
                session.mark_failed(session.started_at)
            } else {
                session.finish_recording(ended_at)
            };
            transition.map_err(|e| RecordingError::Invalid(e.to_string()))?;
            self.sessions.update(&session).await?;
            recovered.push(RecoveredSession {
                session_id: session.id.clone(),
                tracks: tracks.len(),
                frames: frames.len(),
                status: session.status(),
            });
        }
        Ok(recovered)
    }
}

/// Stops every audio stream and closes its file, padded up to `stop_offset`. Returns what to
/// report and what to store; a track that produced nothing is left out (with a warning).
fn finish_tracks(
    tracks: Vec<TrackHandle>,
    stop_offset: Duration,
    session_id: &SessionId,
    warnings: &WarningLog,
) -> (Vec<TrackSummary>, Vec<AudioTrack>) {
    let mut summaries = Vec::new();
    let mut stored = Vec::new();
    for track in tracks {
        let TrackHandle {
            kind,
            queue,
            worker,
            shared,
            stream,
        } = track;
        stream.stop();
        let _ = queue.send(TrackMessage::Finish { stop_offset });
        let Ok(outcome) = worker.join() else {
            warnings.push(format!("the {kind} writer stopped unexpectedly"));
            continue;
        };
        if let Some(failure) = &outcome.failure {
            warnings.push(format!("{kind} audio: {failure}"));
        }
        match outcome.file {
            Some(file) if file.frames > 0 => {
                if outcome.peak <= 0.0 {
                    warnings.push(format!(
                        "{kind} audio is silent (peak 0): check the device and the system privacy settings"
                    ));
                }
                summaries.push(TrackSummary {
                    kind,
                    duration_ms: file.duration_ms(),
                    sample_rate: file.sample_rate,
                    channels: file.channels,
                    peak: outcome.peak,
                    first_chunk_offset_ms: outcome.first_chunk_offset_ms,
                    silence_inserted_ms: outcome.silence_ms,
                    dropped_chunks: shared.dropped_chunks.load(Ordering::Relaxed),
                    stream_issues: shared.stream_issues.load(Ordering::Relaxed),
                });
                stored.push(AudioTrack {
                    session_id: session_id.clone(),
                    kind,
                    duration_ms: file.duration_ms(),
                    path: file.relative_path,
                    sample_rate: file.sample_rate,
                });
            }
            _ => warnings.push(format!("{kind} audio produced no data")),
        }
    }
    (summaries, stored)
}

fn shared_of(tracks: &[TrackHandle], kind: TrackKind) -> Option<Arc<TrackShared>> {
    tracks
        .iter()
        .find(|t| t.kind == kind)
        .map(|t| t.shared.clone())
}

fn elapsed_ms(origin: Instant) -> u64 {
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn timeline() -> TrackTimeline {
        TrackTimeline::new(RATE)
    }

    #[test]
    fn first_block_is_aligned_to_its_start_offset() {
        let t = timeline();
        // The source started delivering 500 ms after the recording began.
        assert_eq!(t.silence_before(Duration::from_millis(500)), 24_000);
    }

    #[test]
    fn first_block_at_the_origin_needs_no_padding() {
        assert_eq!(timeline().silence_before(Duration::ZERO), 0);
    }

    #[test]
    fn small_jitter_after_the_first_block_is_ignored() {
        let mut t = timeline();
        t.advance(u64::from(RATE)); // one second written
                                    // The next block "starts" 100 ms late: below the 250 ms threshold.
        assert_eq!(t.silence_before(Duration::from_millis(1_100)), 0);
    }

    #[test]
    fn a_gap_above_the_threshold_is_filled_with_silence() {
        let mut t = timeline();
        t.advance(u64::from(RATE));
        // The next block starts at 1.6 s although only 1 s was written: 0.6 s missing.
        assert_eq!(t.silence_before(Duration::from_millis(1_600)), 28_800);
    }

    #[test]
    fn blocks_that_arrive_early_never_trigger_silence() {
        let mut t = timeline();
        t.advance(2 * u64::from(RATE));
        assert_eq!(t.silence_before(Duration::from_secs(1)), 0);
    }

    #[test]
    fn tail_silence_pads_up_to_the_stop_instant_but_never_trims() {
        let mut t = timeline();
        t.advance(u64::from(RATE));
        assert_eq!(t.tail_silence(Duration::from_millis(1_500)), 24_000);
        assert_eq!(t.tail_silence(Duration::from_millis(500)), 0);
    }

    #[test]
    fn frames_convert_to_milliseconds() {
        assert_eq!(timeline().frames_to_ms(24_000), 500);
        assert_eq!(TrackTimeline::new(0).frames_to_ms(10), 0);
    }

    #[test]
    fn level_meter_reports_rms_and_resets() {
        let meter = LevelMeter::default();
        assert!(meter.take_rms().abs() < f32::EPSILON);
        meter.add(&[0.5, -0.5, 0.5, -0.5]);
        assert!((meter.take_rms() - 0.5).abs() < 1e-6);
        assert!(meter.take_rms().abs() < f32::EPSILON, "reset after take");
    }

    struct YieldOnce(bool);

    impl Future for YieldOnce {
        type Output = u8;

        fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u8> {
            if self.0 {
                Poll::Ready(9)
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    #[test]
    fn block_on_resolves_ready_and_woken_futures() {
        assert_eq!(block_on(async { 7 }), 7);
        assert_eq!(block_on(YieldOnce(false)), 9);
    }
}
