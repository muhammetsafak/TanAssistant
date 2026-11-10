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
//! Pausing stops every stream (so the microphone indicator goes off and nothing is captured)
//! and resuming opens them again onto the same queues and workers. The shared timeline is the
//! *active* time: a [`PauseLedger`] removes the paused intervals from every timestamp, so audio
//! continues where the pause began, frame timestamps stay monotonic on the same axis and the
//! session length counts only active time.
//!
//! Crash safety: audio files are checkpointed every few seconds
//! ([`CHECKPOINT_INTERVAL`]), frame rows are inserted as they are kept, and the session
//! stays `Recording` in the database until `stop`. [`RecordingService::recover_interrupted`]
//! settles whatever a crash left behind.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    RecordingStatus, RecordingSummary, RecordingWarning, RecoveredSession, StartOptions,
    TrackSummary, MAX_TITLE_LEN,
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

/// A track that reports this many dropouts within [`DROPOUT_WARN_WINDOW`] raises a warning
/// (once per track). A few dropouts are ordinary; a burst means the source is struggling.
pub const DROPOUT_WARN_COUNT: usize = 5;

/// The sliding window [`DROPOUT_WARN_COUNT`] dropouts must fall into.
pub const DROPOUT_WARN_WINDOW: Duration = Duration::from_secs(60);

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
// Pause ledger
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct LedgerIntervals {
    /// Finished pauses, oldest first.
    closed: Vec<(Instant, Instant)>,
    /// Start of the pause in progress.
    open_since: Option<Instant>,
}

/// The paused intervals of a recording, and the conversion from a monotonic instant to the
/// recording's *active* time (time since the start minus everything paused before that
/// instant).
///
/// Conversion by instant rather than a running total keeps late items right: an audio block
/// or frame that is still queued when the pause begins (or resumes) is placed by when it was
/// captured, not by when a worker gets to it.
struct PauseLedger {
    origin: Instant,
    paused: AtomicBool,
    intervals: Mutex<LedgerIntervals>,
}

impl PauseLedger {
    fn new(origin: Instant) -> Self {
        Self {
            origin,
            paused: AtomicBool::new(false),
            intervals: Mutex::default(),
        }
    }

    /// Whether capture is paused. Capture callbacks drop what arrives while this is set.
    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Starts a pause at `at`.
    fn begin(&self, at: Instant) {
        lock(&self.intervals).open_since.get_or_insert(at);
        self.paused.store(true, Ordering::Release);
    }

    /// Ends the pause in progress at `at`.
    fn end(&self, at: Instant) {
        let mut intervals = lock(&self.intervals);
        if let Some(start) = intervals.open_since.take() {
            intervals.closed.push((start, at.max(start)));
        }
        drop(intervals);
        self.paused.store(false, Ordering::Release);
    }

    /// Active time at `at`: the time since the origin without the paused intervals.
    /// Non-decreasing in `at`.
    fn active_offset(&self, at: Instant) -> Duration {
        let total = at.saturating_duration_since(self.origin);
        let intervals = lock(&self.intervals);
        let overlap = |start: Instant, end: Instant| {
            let start = start.max(self.origin);
            end.min(at).saturating_duration_since(start)
        };
        let mut paused = intervals
            .closed
            .iter()
            .map(|&(start, end)| overlap(start, end))
            .sum::<Duration>();
        if let Some(start) = intervals.open_since {
            paused += overlap(start, at);
        }
        total.saturating_sub(paused)
    }

    /// Active time right now.
    fn active_now(&self) -> Duration {
        self.active_offset(Instant::now())
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

    /// After a pause: the next block is aligned exactly again (it carries the start-up delay
    /// of the reopened source), like the first one.
    fn realign(&mut self) {
        self.started = false;
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
    items: Mutex<Vec<RecordingWarning>>,
}

impl WarningLog {
    /// Records a warning that has no specific class.
    fn push(&self, message: String) {
        self.push_warning(RecordingWarning::other(message));
    }

    fn push_warning(&self, warning: RecordingWarning) {
        {
            let mut items = lock(&self.items);
            if items.len() >= MAX_WARNINGS || items.contains(&warning) {
                return;
            }
            items.push(warning.clone());
        }
        self.events.publish(RecordingEvent::Warning {
            session_id: self.session_id.clone(),
            warning,
        });
    }

    fn snapshot(&self) -> Vec<RecordingWarning> {
        lock(&self.items).clone()
    }
}

/// When a burst of dropouts on one track is worth a warning: `count` dropouts within
/// `window`. The default is [`DROPOUT_WARN_COUNT`] within [`DROPOUT_WARN_WINDOW`]; a
/// different policy (see [`RecordingService::with_dropout_policy`]) lets tests exercise the
/// sliding window at a small time scale instead of waiting a minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropoutPolicy {
    /// Dropouts within `window` that raise the warning.
    pub count: usize,
    /// The sliding window the dropouts must fall into.
    pub window: Duration,
}

impl Default for DropoutPolicy {
    fn default() -> Self {
        Self {
            count: DROPOUT_WARN_COUNT,
            window: DROPOUT_WARN_WINDOW,
        }
    }
}

/// Sliding-window detector for bursts of dropouts on one track. Pure bookkeeping on
/// monotonic instants, so it is tested without waiting.
#[derive(Debug, Default)]
struct DropoutWindow {
    policy: DropoutPolicy,
    /// The most recent dropouts, oldest first; never longer than `policy.count`.
    recent: VecDeque<Instant>,
    /// Whether the warning was already raised for this track.
    warned: bool,
}

impl DropoutWindow {
    fn new(policy: DropoutPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// Notes a dropout at `at`. Returns `true` exactly once: when this dropout is the
    /// `policy.count`th within `policy.window`.
    fn record(&mut self, at: Instant) -> bool {
        if self.warned {
            return false;
        }
        let window = self.policy.window;
        while self
            .recent
            .front()
            .is_some_and(|&oldest| at.saturating_duration_since(oldest) >= window)
        {
            self.recent.pop_front();
        }
        self.recent.push_back(at);
        if self.recent.len() >= self.policy.count {
            self.warned = true;
            self.recent.clear();
            return true;
        }
        false
    }
}

#[derive(Default)]
struct TrackShared {
    meter: LevelMeter,
    dropped_chunks: AtomicU64,
    stream_issues: AtomicU32,
    dropouts: AtomicU32,
    dropout_window: Mutex<DropoutWindow>,
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
    /// The recording was paused at `offset` (active time): pad the file up to it, make it
    /// durable and align the next block exactly.
    Pause {
        offset: Duration,
    },
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
    ledger: Arc<PauseLedger>,
}

impl AudioSink for TrackSink {
    fn on_chunk(&self, chunk: PcmChunk) {
        // A block still in flight when the stream was stopped belongs to the pause.
        if self.ledger.is_paused() {
            return;
        }
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

    fn on_dropout(&self) {
        self.note_dropout(Instant::now());
    }
}

impl TrackSink {
    /// Counts a dropout seen at `at` and raises the burst warning when it is due.
    fn note_dropout(&self, at: Instant) {
        // A dropout reported while paused belongs to the stream being torn down.
        if self.ledger.is_paused() {
            return;
        }
        self.shared.dropouts.fetch_add(1, Ordering::Relaxed);
        if lock(&self.shared.dropout_window).record(at) {
            self.warnings
                .push_warning(RecordingWarning::audio_dropouts(self.kind));
        }
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
    ledger: Arc<PauseLedger>,
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
                Ok(TrackMessage::Pause { offset }) => self.on_pause(offset),
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
        let block_start = self
            .ledger
            .active_offset(chunk.captured_at)
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

    /// Brings the file up to the pause instant and makes it durable, so a crash while paused
    /// recovers exactly the active time.
    fn on_pause(&mut self, offset: Duration) {
        let (Some(timeline), Some(writer)) = (self.timeline.as_mut(), self.writer.as_mut()) else {
            return;
        };
        if self.failure.is_some() {
            return;
        }
        let tail = timeline.tail_silence(offset);
        let mut result = Ok(());
        if tail > 0 {
            result = writer.write_silence(tail);
            if result.is_ok() {
                timeline.advance(tail);
                self.silence_frames += tail;
            }
        }
        timeline.realign();
        self.frames_since_checkpoint = 0;
        if result.is_ok() {
            result = writer.checkpoint();
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
    /// Where the stream delivers to; reused when the recording resumes.
    sink: Arc<dyn AudioSink>,
    /// The microphone chosen at start (`None` = system default); reused on resume.
    device_id: Option<String>,
    /// `None` while paused (or when the source could not be reopened).
    stream: Option<Box<dyn AudioStream>>,
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
    ledger: Arc<PauseLedger>,
}

impl FrameSink for ScreenSink {
    fn on_frame(&self, frame: RawFrame) {
        if self.ledger.is_paused() {
            return;
        }
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
    ledger: Arc<PauseLedger>,
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
        let timestamp_ms = duration_ms(self.ledger.active_offset(frame.captured_at));
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
    /// `None` while paused (or when the screen could not be reopened).
    stream: Option<Box<dyn ScreenStream>>,
    /// What the stream was started with; reused on resume.
    request: ScreenStreamRequest,
    sink: Arc<dyn FrameSink>,
    worker: JoinHandle<()>,
    queue: SyncSender<FrameMessage>,
}

struct ActiveRecording {
    session: Session,
    ledger: Arc<PauseLedger>,
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
    /// Every stream is stopped; the workers and files stay open.
    Paused(Box<ActiveRecording>),
    /// A pause or resume is opening or closing streams. The state lock is not held meanwhile
    /// (so `status` and `is_busy` never wait on the OS); the view answers `status` from the
    /// pause flag: capturing or not, right now.
    Switching(StatusView),
    Stopping,
}

/// Everything `status` needs, shareable without the recording itself.
#[derive(Clone)]
struct StatusView {
    session_id: SessionId,
    title: String,
    started_at: EpochMs,
    ledger: Arc<PauseLedger>,
    levels: Arc<Levels>,
    frame_shared: Arc<FrameShared>,
}

impl StatusView {
    fn of(active: &ActiveRecording) -> Self {
        Self {
            session_id: active.session.id.clone(),
            title: active.session.title.clone(),
            started_at: active.session.started_at,
            ledger: active.ledger.clone(),
            levels: active.levels.clone(),
            frame_shared: active.frame_shared.clone(),
        }
    }

    fn status(&self) -> ActiveRecordingStatus {
        ActiveRecordingStatus {
            session_id: self.session_id.clone(),
            title: self.title.clone(),
            started_at: self.started_at,
            elapsed_ms: duration_ms(self.ledger.active_now()),
            microphone_level: load_level(&self.levels.microphone),
            system_level: load_level(&self.levels.system),
            frame_count: self.frame_shared.kept.load(Ordering::Relaxed),
        }
    }
}

/// A recording taken out of the service by [`RecordingService::begin_stop`], waiting for
/// [`RecordingService::finish_stop`].
#[must_use = "pass it to `finish_stop`; the service stays busy until then"]
pub struct StoppingRecording(Box<ActiveRecording>);

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
    dropout_policy: DropoutPolicy,
    state: Mutex<State>,
    /// Serialises start/pause/resume (and the taking of the recording by `stop`) together
    /// with the events they publish, so events reach subscribers in the order the state
    /// changed. Always taken before `state`; `status` and `is_busy` never take it.
    transition: Mutex<()>,
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
            dropout_policy: DropoutPolicy::default(),
            state: Mutex::new(State::Idle),
            transition: Mutex::new(()),
        }
    }

    /// Replaces the dropout warning threshold ([`DropoutPolicy::default`] otherwise). Only
    /// affects recordings started afterwards.
    #[must_use]
    pub fn with_dropout_policy(mut self, policy: DropoutPolicy) -> Self {
        self.dropout_policy = policy;
        self
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
            State::Recording(active) => RecordingStatus::Recording(StatusView::of(active).status()),
            State::Paused(active) => RecordingStatus::Paused(StatusView::of(active).status()),
            State::Switching(view) if view.ledger.is_paused() => {
                RecordingStatus::Paused(view.status())
            }
            State::Switching(view) => RecordingStatus::Recording(view.status()),
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
                // Under the transition lock so a pause racing the start cannot publish
                // `Paused` before `Started`.
                let _transition = lock(&self.transition);
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
        let ledger = Arc::new(PauseLedger::new(Instant::now()));
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
            match self.start_track(&session.id, kind, device_id, &ledger, &warnings) {
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
            &ledger,
            &frame_shared,
            &warnings,
        );

        let levels = Arc::new(Levels::default());
        let ticker = self.start_ticker(&session.id, &ledger, &tracks, &levels, &frame_shared);

        Ok(ActiveRecording {
            session,
            ledger,
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
        ledger: &Arc<PauseLedger>,
        warnings: &Arc<WarningLog>,
    ) -> Result<TrackHandle, CaptureError> {
        let shared = Arc::new(TrackShared {
            dropout_window: Mutex::new(DropoutWindow::new(self.dropout_policy)),
            ..TrackShared::default()
        });
        let (queue, receiver) = mpsc::sync_channel(TRACK_QUEUE_CAPACITY);
        let sink: Arc<dyn AudioSink> = Arc::new(TrackSink {
            kind,
            queue: queue.clone(),
            shared: shared.clone(),
            warnings: warnings.clone(),
            ledger: ledger.clone(),
        });
        let stream = self.audio.start(
            &AudioStreamRequest {
                source: kind,
                device_id: device_id.clone(),
            },
            sink.clone(),
        )?;

        let worker = TrackWorker {
            session_id: session_id.clone(),
            kind,
            ledger: ledger.clone(),
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
                sink,
                device_id,
                stream: Some(stream),
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
        ledger: &Arc<PauseLedger>,
        shared: &Arc<FrameShared>,
        warnings: &Arc<WarningLog>,
    ) -> Option<ScreenHandle> {
        // An unusable screen (Wayland) is a platform fact, not a failure: skip it quietly and
        // record audio only. The UI explains it from `screen_availability`.
        if self.screen.availability() != ScreenAvailability::Available {
            return None;
        }
        let (queue, receiver) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let sink: Arc<dyn FrameSink> = Arc::new(ScreenSink {
            queue: queue.clone(),
            shared: shared.clone(),
            warnings: warnings.clone(),
            ledger: ledger.clone(),
        });
        let request = ScreenStreamRequest {
            display_id,
            interval,
        };
        let stream = match self.screen.start(&request, sink.clone()) {
            Ok(stream) => stream,
            Err(error) => {
                warnings.push(format!("the screen could not be captured: {error}"));
                return None;
            }
        };

        let worker = FrameWorker {
            session_id: session_id.clone(),
            ledger: ledger.clone(),
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
                stream: Some(stream),
                request,
                sink,
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
        ledger: &Arc<PauseLedger>,
        tracks: &[TrackHandle],
        levels: &Arc<Levels>,
        frame_shared: &Arc<FrameShared>,
    ) -> (mpsc::Sender<()>, Option<JoinHandle<()>>) {
        let (stop, stopped) = mpsc::channel::<()>();
        let session_id = session_id.clone();
        let events = self.events.clone();
        let levels = levels.clone();
        let frame_shared = frame_shared.clone();
        let ledger = ledger.clone();
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
                    if ledger.is_paused() {
                        continue;
                    }
                    events.publish(RecordingEvent::Progress(RecordingProgress {
                        session_id: session_id.clone(),
                        elapsed_ms: duration_ms(ledger.active_now()),
                        microphone_level,
                        system_level,
                        frame_count: frame_shared.kept.load(Ordering::Relaxed),
                    }));
                }
            })
            .ok();
        (stop, handle)
    }

    /// Pauses the running recording: every stream is stopped (no microphone, no system
    /// audio, no screen capture while paused), the audio files are padded up to the pause
    /// instant and made durable, and the clock of the recording stops.
    ///
    /// Returns the paused status. Fails with [`RecordingError::AlreadyPaused`] when the
    /// recording is already paused and [`RecordingError::NotRecording`] when there is none.
    /// Blocks the calling thread while the streams stop, but not the state: `status` and
    /// `is_busy` answer meanwhile. Calls to `pause`, `resume` and `stop` queue up behind it.
    /// `Paused` is published before the next transition can start, in the order of the
    /// state changes; subscribers must not call `pause`/`resume` from `publish`.
    pub fn pause(&self) -> Result<ActiveRecordingStatus, RecordingError> {
        let _transition = lock(&self.transition);
        let mut active = {
            let mut state = lock(&self.state);
            match std::mem::replace(&mut *state, State::Idle) {
                State::Recording(active) => {
                    *state = State::Switching(StatusView::of(&active));
                    active
                }
                other => {
                    let error = if matches!(other, State::Paused(_)) {
                        RecordingError::AlreadyPaused
                    } else {
                        RecordingError::NotRecording
                    };
                    *state = other;
                    return Err(error);
                }
            }
        };

        let at = Instant::now();
        // From here on the capture callbacks drop what is still in flight.
        active.ledger.begin(at);
        for track in &mut active.tracks {
            if let Some(stream) = track.stream.take() {
                stream.stop();
            }
        }
        if let Some(stream) = active.screen.as_mut().and_then(|s| s.stream.take()) {
            stream.stop();
        }
        let offset = active.ledger.active_offset(at);
        for track in &active.tracks {
            let _ = track.queue.send(TrackMessage::Pause { offset });
        }

        let status = StatusView::of(&active).status();
        let event = RecordingEvent::Paused {
            session_id: active.session.id.clone(),
            elapsed_ms: status.elapsed_ms,
        };
        *lock(&self.state) = State::Paused(active);
        self.events.publish(event);
        Ok(status)
    }

    /// Resumes a paused recording: the streams are opened again onto the same files, so the
    /// audio continues where the pause began.
    ///
    /// The audio sources open first; the recording clock restarts and `Resumed` is published
    /// once they run, then the screen opens (a slow screen, such as a portal dialog, does not
    /// delay or lose audio). A source that cannot be reopened is a warning (the recording
    /// goes on with the rest); if no audio source can be reopened the recording stays paused
    /// and the first error is returned. Fails with [`RecordingError::NotPaused`] when the
    /// recording is running and [`RecordingError::NotRecording`] when there is none.
    /// Blocks like [`Self::pause`]; `status` answers meanwhile.
    pub fn resume(&self) -> Result<ActiveRecordingStatus, RecordingError> {
        let _transition = lock(&self.transition);
        let mut active = {
            let mut state = lock(&self.state);
            match std::mem::replace(&mut *state, State::Idle) {
                State::Paused(active) => {
                    *state = State::Switching(StatusView::of(&active));
                    active
                }
                other => {
                    let error = if matches!(other, State::Recording(_)) {
                        RecordingError::NotPaused
                    } else {
                        RecordingError::NotRecording
                    };
                    *state = other;
                    return Err(error);
                }
            }
        };

        // Blocks that arrive while the sources open are still dropped: the pause lasts until
        // they all run.
        if let Err(error) = self.reopen_audio(&mut active) {
            *lock(&self.state) = State::Paused(active);
            return Err(error);
        }
        active.ledger.end(Instant::now());
        self.events.publish(RecordingEvent::Resumed {
            session_id: active.session.id.clone(),
        });
        self.reopen_screen(&mut active);

        let status = StatusView::of(&active).status();
        *lock(&self.state) = State::Recording(active);
        Ok(status)
    }

    /// Opens the audio streams of a paused recording again. Fails when none could be opened.
    fn reopen_audio(&self, active: &mut ActiveRecording) -> Result<(), RecordingError> {
        let mut first_error: Option<CaptureError> = None;
        let mut reopened = 0_usize;
        for track in &mut active.tracks {
            let request = AudioStreamRequest {
                source: track.kind,
                device_id: track.device_id.clone(),
            };
            match self.audio.start(&request, track.sink.clone()) {
                Ok(stream) => {
                    track.stream = Some(stream);
                    reopened += 1;
                }
                Err(error) => {
                    active
                        .warnings
                        .push(format!("{} audio could not resume: {error}", track.kind));
                    first_error.get_or_insert(error);
                }
            }
        }
        if reopened == 0 {
            return Err(RecordingError::Capture(first_error.unwrap_or_else(|| {
                CaptureError::failed("no audio source is available")
            })));
        }
        Ok(())
    }

    /// Opens the screen stream of a resumed recording again (a failure is a warning).
    fn reopen_screen(&self, active: &mut ActiveRecording) {
        if let Some(screen) = &mut active.screen {
            match self.screen.start(&screen.request, screen.sink.clone()) {
                Ok(stream) => screen.stream = Some(stream),
                Err(error) => active
                    .warnings
                    .push(format!("the screen could not resume: {error}")),
            }
        }
    }

    /// Stops the running (or paused) recording and settles its session (`Recorded`, or
    /// `Failed` when nothing at all was captured). The session length is the active time.
    ///
    /// Blocks the calling thread while it waits for a pause or resume in progress and while
    /// the streams stop and the files are closed. From an async runtime, prefer
    /// [`Self::begin_stop`] on the blocking pool followed by [`Self::finish_stop`], which
    /// keeps the possibly long wait off the runtime threads.
    /// If the database fails at the very end the recording is over regardless; the session
    /// then stays `Recording` and [`Self::recover_interrupted`] settles it at next start.
    pub async fn stop(&self) -> Result<RecordingSummary, RecordingError> {
        let stopping = self.begin_stop()?;
        self.finish_stop(stopping).await
    }

    /// First half of [`Self::stop`]: takes the running or paused recording out of the
    /// service. Waits (blocking the calling thread, so call it from the blocking pool when
    /// on an async runtime) for a pause or resume in progress, which can take as long as the
    /// OS needs to open a stream. Fails with [`RecordingError::NotRecording`] when there is
    /// none.
    ///
    /// The service stays busy until the returned value is passed to [`Self::finish_stop`],
    /// which must therefore always follow.
    pub fn begin_stop(&self) -> Result<StoppingRecording, RecordingError> {
        // Waits for a pause or resume in progress instead of failing against it.
        let _transition = lock(&self.transition);
        let mut state = lock(&self.state);
        match std::mem::replace(&mut *state, State::Stopping) {
            State::Recording(active) | State::Paused(active) => Ok(StoppingRecording(active)),
            other => {
                *state = other;
                Err(RecordingError::NotRecording)
            }
        }
    }

    /// Second half of [`Self::stop`]: closes the streams and files of a recording taken by
    /// [`Self::begin_stop`], settles the session and publishes `Stopped`.
    pub async fn finish_stop(
        &self,
        stopping: StoppingRecording,
    ) -> Result<RecordingSummary, RecordingError> {
        let result = self.settle(*stopping.0).await;
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
            ledger,
            tracks,
            screen,
            frame_shared,
            ticker,
            levels: _,
            warnings,
        } = active;

        // One stop instant for every stream; tracks are padded up to it. It is active time:
        // a recording stopped while paused ends where the pause began.
        let stop_offset = ledger.active_now();
        let (ticker_stop, ticker_handle) = ticker;
        drop(ticker_stop);
        if let Some(handle) = ticker_handle {
            let _ = handle.join();
        }

        if let Some(ScreenHandle {
            stream,
            worker,
            queue,
            ..
        }) = screen
        {
            if let Some(stream) = stream {
                stream.stop();
            }
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
            ..
        } = track;
        if let Some(stream) = stream {
            stream.stop();
        }
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
                    dropouts: shared.dropouts.load(Ordering::Relaxed),
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

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn active_time_skips_the_paused_interval() {
        // 10 s recording, 10 s paused, 10 s recording.
        let t0 = Instant::now();
        let ledger = PauseLedger::new(t0);
        ledger.begin(t0 + secs(10));
        assert!(ledger.is_paused());
        ledger.end(t0 + secs(20));
        assert!(!ledger.is_paused());

        assert_eq!(ledger.active_offset(t0 + secs(5)), secs(5));
        assert_eq!(ledger.active_offset(t0 + secs(10)), secs(10));
        // Inside the pause the clock stands still.
        assert_eq!(ledger.active_offset(t0 + secs(15)), secs(10));
        assert_eq!(ledger.active_offset(t0 + secs(20)), secs(10));
        assert_eq!(ledger.active_offset(t0 + secs(30)), secs(20));
    }

    #[test]
    fn a_pause_in_progress_stops_the_clock_until_it_ends() {
        let t0 = Instant::now();
        let ledger = PauseLedger::new(t0);
        ledger.begin(t0 + secs(10));
        // Stopping while paused: the length is where the pause began.
        assert_eq!(ledger.active_offset(t0 + secs(25)), secs(10));
        // A second begin does not move the start of the pause.
        ledger.begin(t0 + secs(12));
        assert_eq!(ledger.active_offset(t0 + secs(25)), secs(10));
    }

    #[test]
    fn several_pauses_add_up_and_the_offset_never_goes_backwards() {
        let t0 = Instant::now();
        let ledger = PauseLedger::new(t0);
        ledger.begin(t0 + secs(2));
        ledger.end(t0 + secs(5));
        ledger.begin(t0 + secs(8));
        ledger.end(t0 + secs(9));

        let mut last = Duration::ZERO;
        for ms in (0..12_000).step_by(100) {
            let offset = ledger.active_offset(t0 + Duration::from_millis(ms));
            assert!(offset >= last, "went backwards at {ms} ms");
            last = offset;
        }
        // 12 s minus 3 s and 1 s of pause.
        assert_eq!(ledger.active_offset(t0 + secs(12)), secs(8));
    }

    #[test]
    fn instants_before_the_origin_have_no_active_time() {
        let before = Instant::now();
        let ledger = PauseLedger::new(before + secs(5));
        assert_eq!(ledger.active_offset(before), Duration::ZERO);
    }

    #[test]
    fn realigning_makes_the_next_block_exact_again() {
        let mut t = timeline();
        t.advance(u64::from(RATE));
        // A 100 ms lag is jitter normally...
        assert_eq!(t.silence_before(Duration::from_millis(1_100)), 0);
        // ...but the first block after a pause carries the reopened source's start-up delay.
        t.realign();
        assert_eq!(t.silence_before(Duration::from_millis(1_100)), 4_800);
    }

    #[test]
    fn dropouts_warn_once_on_the_threshold_within_the_window() {
        let t0 = Instant::now();
        let mut window = DropoutWindow::default();
        // Four within a second: below the threshold.
        for i in 0..4 {
            assert!(!window.record(t0 + Duration::from_millis(i * 250)));
        }
        // The fifth: the warning is due, exactly once.
        assert!(window.record(t0 + secs(1)));
        for i in 2..10 {
            assert!(!window.record(t0 + secs(i)));
        }
    }

    #[test]
    fn dropouts_spread_over_the_window_never_warn() {
        let t0 = Instant::now();
        let mut window = DropoutWindow::default();
        // One every 15 s: five are 60 s apart end to end, so the first has left the window
        // by the time the fifth arrives. Keep going for ten minutes.
        for i in 0..40 {
            assert!(!window.record(t0 + secs(i * 15)), "warned at dropout {i}");
        }
    }

    #[test]
    fn the_window_slides_and_keeps_only_recent_dropouts() {
        let t0 = Instant::now();
        let mut window = DropoutWindow::default();
        // Three early dropouts, then a long quiet spell: they no longer count.
        for i in 0..3 {
            assert!(!window.record(t0 + secs(i)));
        }
        assert!(!window.record(t0 + secs(100)));
        assert!(!window.record(t0 + secs(101)));
        assert!(!window.record(t0 + secs(102)));
        assert!(!window.record(t0 + secs(103)));
        // The fifth *recent* one is the 59.9 s mark of the first recent one: inside.
        assert!(window.record(t0 + Duration::from_millis(159_900)));
    }

    fn sink_with_events(kind: TrackKind) -> (TrackSink, Arc<WarningLog>, Arc<PauseLedger>) {
        let warnings = Arc::new(WarningLog {
            session_id: SessionId::new("s1"),
            events: Arc::new(super::super::NoRecordingEvents),
            items: Mutex::default(),
        });
        let ledger = Arc::new(PauseLedger::new(Instant::now()));
        let (queue, _receiver) = mpsc::sync_channel(1);
        let sink = TrackSink {
            kind,
            queue,
            shared: Arc::new(TrackShared::default()),
            warnings: warnings.clone(),
            ledger: ledger.clone(),
        };
        (sink, warnings, ledger)
    }

    #[test]
    fn the_sink_counts_every_dropout_but_warns_once_per_track() {
        let t0 = Instant::now();
        let (sink, warnings, _) = sink_with_events(TrackKind::System);
        // Sparse: 20 dropouts, 30 s apart, never five within 60 s.
        for i in 0..20 {
            sink.note_dropout(t0 + secs(i * 30));
        }
        assert_eq!(warnings.snapshot().len(), 0);
        // A burst afterwards warns, once.
        for i in 0..10 {
            sink.note_dropout(t0 + secs(1_000 + i));
        }
        assert_eq!(
            warnings.snapshot(),
            [RecordingWarning::audio_dropouts(TrackKind::System)]
        );
        assert_eq!(sink.shared.dropouts.load(Ordering::Relaxed), 30);
        assert_eq!(sink.shared.stream_issues.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_dropout_while_paused_is_ignored() {
        let (sink, warnings, ledger) = sink_with_events(TrackKind::Microphone);
        ledger.begin(Instant::now());
        for _ in 0..DROPOUT_WARN_COUNT * 2 {
            sink.on_dropout();
        }
        assert_eq!(sink.shared.dropouts.load(Ordering::Relaxed), 0);
        assert_eq!(warnings.snapshot().len(), 0);
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
