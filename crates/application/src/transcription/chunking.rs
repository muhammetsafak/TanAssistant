//! Cloud transcription in pieces: planning, bounded concurrency, per-piece retry.
//!
//! A cloud provider caps the upload (`OpenAI` 25 MB, Gemini about 14 MB inline) and the newer
//! `OpenAI` models return no segment timestamps, so a recording is cut into short pieces that are
//! transcribed one request each. [`plan_chunks`] finds the pieces from the speech map; the rest
//! of this module sends them a few at a time and puts the answers back on the recording's
//! timeline. Nothing here knows a provider: it works against [`TranscriptionModel`].

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use tan_domain::LanguageCode;

use super::{
    AudioChunker, AudioPrepError, AudioSpan, SpeechMap, UploadFormat, CLOUD_CHUNK_MS,
    CLOUD_UPLOAD_LIMIT_BYTES,
};
use crate::ports::{
    AiError, AudioSource, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
};

/// Pieces are never planned shorter than this limit allows.
const MIN_CHUNK_LIMIT_MS: u64 = 1_000;

/// A cut inside a stretch of speech goes to the quietest stretch of audio this long, not to the
/// quietest single window: a stop consonant leaves a gap of 30-100 ms inside a word, a breath
/// or a pause lasts longer.
const QUIET_SPAN_MS: u64 = 300;

/// How a failed piece is retried. Only transient failures (see [`is_transient`]) are retried; a
/// refused key or request fails at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per piece, the first one included. `1` disables retrying.
    pub max_attempts: u32,
    /// Wait before the second attempt; doubles with each further one.
    pub base_delay: Duration,
    /// Upper bound of a backoff wait.
    pub max_delay: Duration,
    /// Longest `Retry-After` the piece waits for. A provider that asks for more (a spent
    /// per-minute quota can ask for minutes) is not asked again: the error is returned, since
    /// retrying early only gets another refusal.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    /// Three attempts, 2 s then 4 s apart. The HTTP client underneath has already retried
    /// throttling and server errors with its own short backoff, so this is the second line.
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_mins(5),
        }
    }
}

impl RetryPolicy {
    /// How long to wait after failed attempt number `attempt` (1-based), or `None` when the
    /// provider asked for a wait longer than [`Self::max_retry_after`] and the error is final.
    fn delay(&self, attempt: u32, error: &AiError) -> Option<Duration> {
        match error {
            AiError::RateLimited {
                retry_after: Some(wait),
            } => (*wait <= self.max_retry_after).then_some(*wait),
            _ => Some(
                self.base_delay
                    .saturating_mul(2_u32.saturating_pow(attempt.saturating_sub(1)))
                    .min(self.max_delay),
            ),
        }
    }
}

/// Whether trying the same request again can help: throttling, timeouts, connection failures,
/// server errors, and a provider that answered `200` with an unusable reply (a model that
/// returned broken JSON or cut its transcript off; generation is not deterministic, so the next
/// try usually works). A refused key, a malformed request or a missing capability will not
/// change.
#[must_use]
pub fn is_transient(error: &AiError) -> bool {
    match error {
        AiError::RateLimited { .. } | AiError::Timeout | AiError::Network(_) => true,
        AiError::Provider { status, .. } => *status >= 500 || *status == 200,
        AiError::Auth
        | AiError::CliUnavailable(_)
        | AiError::InvalidRequest(_)
        | AiError::Unsupported(_)
        | AiError::SecretUnavailable(_) => false,
    }
}

/// How cloud transcription cuts and sends a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSettings {
    /// Longest piece, in milliseconds.
    pub max_chunk_ms: u64,
    /// Pieces in flight at once.
    pub concurrency: usize,
    /// Retrying of a failed piece.
    pub retry: RetryPolicy,
}

impl Default for ChunkSettings {
    /// 30-second pieces, three at a time.
    fn default() -> Self {
        Self {
            max_chunk_ms: CLOUD_CHUNK_MS,
            concurrency: 3,
            retry: RetryPolicy::default(),
        }
    }
}

/// Why a track could not be transcribed in pieces.
#[derive(Debug)]
pub(super) enum ChunkError {
    /// The audio could not be read, analysed or encoded.
    Audio(AudioPrepError),
    /// The provider failed on a piece for good.
    Engine(AiError),
}

/// Plans the pieces of a recording: spans of at most `max_chunk_ms` that together cover all the
/// speech and nothing else but the pauses between it.
///
/// Whole stretches of speech are packed into a piece while they fit, so a cut always falls in a
/// pause. A stretch longer than a piece has no pause to cut at; it is cut where the recording is
/// quietest in the second half of the piece. Silence between pieces is not sent.
#[must_use]
pub fn plan_chunks(map: &SpeechMap, max_chunk_ms: u64) -> Vec<AudioSpan> {
    let max = max_chunk_ms.max(MIN_CHUNK_LIMIT_MS);
    let mut chunks: Vec<AudioSpan> = Vec::new();
    let mut open: Option<AudioSpan> = None;
    for region in normalised_regions(map) {
        for piece in split_region(map, region, max) {
            open = match open {
                Some(current) if piece.end_ms - current.start_ms <= max => Some(AudioSpan {
                    start_ms: current.start_ms,
                    end_ms: piece.end_ms,
                }),
                Some(current) => {
                    chunks.push(current);
                    Some(piece)
                }
                None => Some(piece),
            };
        }
    }
    chunks.extend(open);
    chunks
}

/// The speech regions in time order, clipped to the recording, without overlaps or empties.
fn normalised_regions(map: &SpeechMap) -> Vec<AudioSpan> {
    let mut regions: Vec<AudioSpan> = map
        .regions
        .iter()
        .map(|r| AudioSpan {
            start_ms: r.start_ms,
            end_ms: r.end_ms.min(map.duration_ms),
        })
        .filter(|r| r.end_ms > r.start_ms)
        .collect();
    regions.sort_by_key(|r| (r.start_ms, r.end_ms));
    let mut merged: Vec<AudioSpan> = Vec::with_capacity(regions.len());
    for region in regions {
        match merged.last_mut() {
            Some(last) if region.start_ms <= last.end_ms => {
                last.end_ms = last.end_ms.max(region.end_ms);
            }
            _ => merged.push(region),
        }
    }
    merged
}

/// Cuts a region that is longer than `max` into pieces of at most `max`.
fn split_region(map: &SpeechMap, region: AudioSpan, max: u64) -> Vec<AudioSpan> {
    let mut pieces = Vec::new();
    let mut start = region.start_ms;
    while region.end_ms - start > max {
        let target = start + max;
        let cut = quietest_point(map, start + max / 2, target).unwrap_or(target);
        pieces.push(AudioSpan {
            start_ms: start,
            end_ms: cut,
        });
        start = cut;
    }
    pieces.push(AudioSpan {
        start_ms: start,
        end_ms: region.end_ms,
    });
    pieces
}

/// The middle of the quietest stretch of [`QUIET_SPAN_MS`] that lies within `from_ms..=to_ms`
/// (quietest by mean energy, so one silent window between loud ones does not count); on a tie
/// the latest one, which keeps the piece as long as possible.
fn quietest_point(map: &SpeechMap, from_ms: u64, to_ms: u64) -> Option<u64> {
    let window = map.energy_window_ms.max(1);
    let span = usize::try_from(QUIET_SPAN_MS.div_ceil(window)).ok()?.max(1);
    let first = usize::try_from(from_ms.div_ceil(window)).ok()?;
    let last = usize::try_from(to_ms / window)
        .ok()?
        .min(map.energies.len());
    if last < first + span {
        return None;
    }
    // Prefix sums: the mean of any stretch is two lookups.
    let mut sums = Vec::with_capacity(last - first + 1);
    let mut total = 0.0_f64;
    sums.push(total);
    for &energy in &map.energies[first..last] {
        total += f64::from(energy);
        sums.push(total);
    }
    (0..=last - first - span)
        .min_by(|&a, &b| {
            (sums[a + span] - sums[a])
                .total_cmp(&(sums[b + span] - sums[b]))
                .then_with(|| b.cmp(&a))
        })
        .map(|start| (first + start) as u64 * window + span as u64 * window / 2)
}

/// Transcribes one track in pieces and returns its segments on the recording's timeline, in
/// time order.
///
/// `on_chunk(done, total)` is called when the pieces are planned (`done == 0`) and after each
/// piece finishes. A piece that fails for good ends the track: the pieces still in flight are
/// dropped and the error is returned.
pub(super) async fn transcribe_track<M, C>(
    model: &M,
    chunker: &C,
    settings: &ChunkSettings,
    audio: &Path,
    language: Option<&LanguageCode>,
    on_chunk: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<Vec<TranscriptSegmentDto>, ChunkError>
where
    M: TranscriptionModel,
    C: AudioChunker,
{
    let map = chunker.speech_map(audio).await.map_err(ChunkError::Audio)?;
    let spans = plan_chunks(&map, settings.max_chunk_ms);
    let total = spans.len();
    on_chunk(0, total);

    let done = AtomicUsize::new(0);
    let done = &done;
    // Set when a provider refuses the compressed format: the rest of the track goes as WAV.
    let wav_only = AtomicBool::new(false);
    let wav_only = &wav_only;
    let mut results = stream::iter(spans)
        .map(|span| async move {
            let segments =
                run_chunk(model, chunker, settings, audio, language, span, wav_only).await?;
            on_chunk(done.fetch_add(1, Ordering::SeqCst) + 1, total);
            Ok::<_, ChunkError>(segments)
        })
        // In order: the answers come out in time order whichever piece finishes first.
        .buffered(settings.concurrency.max(1));

    let mut all = Vec::new();
    while let Some(segments) = results.next().await {
        all.extend(segments?);
    }
    Ok(all)
}

/// Encodes and transcribes one piece, retrying transient failures.
async fn run_chunk<M, C>(
    model: &M,
    chunker: &C,
    settings: &ChunkSettings,
    audio: &Path,
    language: Option<&LanguageCode>,
    span: AudioSpan,
    wav_only: &AtomicBool,
) -> Result<Vec<TranscriptSegmentDto>, ChunkError>
where
    M: TranscriptionModel,
    C: AudioChunker,
{
    let mut format = if wav_only.load(Ordering::SeqCst) {
        UploadFormat::Wav
    } else {
        UploadFormat::Compressed
    };
    let mut encoded = encode_checked(chunker, audio, span, format).await?;
    let mut attempt = 1_u32;
    loop {
        let request = TranscriptionRequest {
            audio: AudioSource::Bytes {
                file_name: encoded.file_name.clone(),
                mime_type: encoded.mime_type.clone(),
                data: encoded.data.clone(),
            },
            language: language.cloned(),
            duration_ms: Some(span.end_ms.saturating_sub(span.start_ms)),
            prompt: None,
        };
        let error = match model.transcribe(request).await {
            Ok(segments) => return Ok(place(segments, span)),
            Err(error) => error,
        };
        // A refused request may be a refused format (the compressed one is the less common):
        // send this piece, and the rest of the track, as WAV. This is not a retry, so it does
        // not use up an attempt; a refusal of WAV itself is final.
        if matches!(error, AiError::InvalidRequest(_)) && format == UploadFormat::Compressed {
            wav_only.store(true, Ordering::SeqCst);
            format = UploadFormat::Wav;
            encoded = encode_checked(chunker, audio, span, format).await?;
            continue;
        }
        let wait = if attempt < settings.retry.max_attempts && is_transient(&error) {
            settings.retry.delay(attempt, &error)
        } else {
            None
        };
        let Some(wait) = wait else {
            return Err(ChunkError::Engine(error));
        };
        pause(wait).await;
        attempt += 1;
    }
}

/// Encodes a piece and refuses one that is over the upload limit.
async fn encode_checked<C: AudioChunker>(
    chunker: &C,
    audio: &Path,
    span: AudioSpan,
    format: UploadFormat,
) -> Result<super::EncodedAudio, ChunkError> {
    let encoded = chunker
        .encode(audio, span, format)
        .await
        .map_err(ChunkError::Audio)?;
    if encoded.data.len() > CLOUD_UPLOAD_LIMIT_BYTES {
        return Err(ChunkError::Audio(AudioPrepError(format!(
            "an encoded piece is {} bytes, more than the {CLOUD_UPLOAD_LIMIT_BYTES} byte upload limit",
            encoded.data.len()
        ))));
    }
    Ok(encoded)
}

/// Moves a piece's segments from piece-relative to file-relative time and keeps them inside the
/// piece, so pieces never overlap and the result is monotonic whatever a model reports.
fn place(segments: Vec<TranscriptSegmentDto>, span: AudioSpan) -> Vec<TranscriptSegmentDto> {
    let mut placed: Vec<TranscriptSegmentDto> = segments
        .into_iter()
        .map(|mut segment| {
            let end = segment
                .end_ms
                .saturating_add(span.start_ms)
                .min(span.end_ms);
            let start = segment.start_ms.saturating_add(span.start_ms).min(end);
            segment.start_ms = start;
            segment.end_ms = end;
            segment
        })
        .collect();
    placed.sort_by_key(|s| (s.start_ms, s.end_ms));
    placed
}

/// Waits without a runtime: this crate has no timer, so the wait is a thread that sleeps and
/// wakes the task. Retries are rare, so a thread per wait is fine. Dropping the wait (the
/// transcription was cancelled, or another piece failed for good) wakes the thread at once so it
/// ends instead of sleeping out the delay.
async fn pause(delay: Duration) {
    if !delay.is_zero() {
        Pause {
            delay,
            shared: None,
        }
        .await;
    }
}

#[derive(Default)]
struct PauseState {
    over: bool,
    cancelled: bool,
    waker: Option<Waker>,
}

#[derive(Default)]
struct PauseShared {
    state: Mutex<PauseState>,
    cancel: Condvar,
}

struct Pause {
    delay: Duration,
    shared: Option<Arc<PauseShared>>,
}

impl Future for Pause {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(shared) = &self.shared {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.over {
                return Poll::Ready(());
            }
            state.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let shared = Arc::new(PauseShared::default());
        shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waker = Some(cx.waker().clone());
        let sleeper = shared.clone();
        let delay = self.delay;
        let spawned = std::thread::Builder::new()
            .name("tan-retry-pause".to_owned())
            .spawn(move || {
                let state = sleeper.state.lock().unwrap_or_else(PoisonError::into_inner);
                let (mut state, _) = sleeper
                    .cancel
                    .wait_timeout_while(state, delay, |state| !state.cancelled)
                    .unwrap_or_else(PoisonError::into_inner);
                state.over = true;
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            });
        if spawned.is_err() {
            // No thread to wait on: retry at once rather than not at all.
            return Poll::Ready(());
        }
        self.shared = Some(shared);
        Poll::Pending
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .cancelled = true;
            shared.cancel.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(duration_ms: u64, regions: &[(u64, u64)], energies: Vec<f32>) -> SpeechMap {
        SpeechMap {
            duration_ms,
            regions: regions
                .iter()
                .map(|&(start_ms, end_ms)| AudioSpan { start_ms, end_ms })
                .collect(),
            energies,
            energy_window_ms: 30,
        }
    }

    #[test]
    fn a_short_recording_is_one_piece_and_silence_is_none() {
        let speech = map(10_000, &[(1_000, 4_000), (5_000, 8_000)], Vec::new());
        assert_eq!(
            plan_chunks(&speech, 30_000),
            [AudioSpan {
                start_ms: 1_000,
                end_ms: 8_000
            }]
        );
        assert!(
            plan_chunks(&map(10_000, &[], Vec::new()), 30_000).is_empty(),
            "no speech, nothing to send"
        );
    }

    #[test]
    fn a_stretch_without_a_pause_is_cut_in_the_middle_of_its_quietest_stretch() {
        // One 100 s stretch, loud except for a 300 ms hush from 30.0 s; a 40 s piece looks for
        // its cut in its second half (20..40 s).
        let mut energies = vec![0.5_f32; 3_400];
        energies[1_000..1_010].fill(0.0); // 30 000..30 300 ms
        let speech = map(100_000, &[(0, 100_000)], energies);
        let chunks = plan_chunks(&speech, 40_000);
        assert_eq!(chunks[0].end_ms, 30_150, "middle of the hush");
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms);
        }
        assert_eq!(chunks.first().unwrap().start_ms, 0);
        assert_eq!(chunks.last().unwrap().end_ms, 100_000);
        assert!(chunks.iter().all(|c| c.end_ms - c.start_ms <= 40_000));
    }

    #[test]
    fn a_single_silent_window_inside_a_word_loses_to_a_real_pause() {
        // A stop consonant leaves one near-silent window (22.0 s, level 0.0) between loud ones;
        // a breath at 33.0 s lasts 400 ms at a slightly higher level (0.02). The cut belongs in
        // the breath even though the dip is quieter window for window.
        let mut energies = vec![0.5_f32; 3_400];
        energies[733] = 0.0;
        energies[1_100..1_113].fill(0.02); // 33 000..33 390 ms
        let speech = map(100_000, &[(0, 100_000)], energies);
        let chunks = plan_chunks(&speech, 40_000);
        assert!(
            (33_000..=33_400).contains(&chunks[0].end_ms),
            "cut at {} ms",
            chunks[0].end_ms
        );
    }

    #[test]
    fn overlapping_and_out_of_order_regions_are_tidied() {
        let speech = map(
            20_000,
            &[(9_000, 25_000), (0, 5_000), (4_000, 6_000)],
            Vec::new(),
        );
        assert_eq!(
            plan_chunks(&speech, 30_000),
            [AudioSpan {
                start_ms: 0,
                end_ms: 20_000
            }]
        );
    }

    #[test]
    fn retry_waits_double_and_honour_retry_after_up_to_the_cap() {
        let policy = RetryPolicy::default();
        let timeout = AiError::Timeout;
        assert_eq!(policy.delay(1, &timeout), Some(Duration::from_secs(2)));
        assert_eq!(policy.delay(2, &timeout), Some(Duration::from_secs(4)));
        assert_eq!(policy.delay(30, &timeout), Some(Duration::from_secs(30)));
        let limited = |secs| AiError::RateLimited {
            retry_after: Some(Duration::from_secs(secs)),
        };
        // The provider's own wait is honoured, even beyond the backoff cap...
        assert_eq!(policy.delay(1, &limited(10)), Some(Duration::from_secs(10)));
        assert_eq!(
            policy.delay(1, &limited(120)),
            Some(Duration::from_secs(120))
        );
        // ...but a wait longer than the policy tolerates ends the retrying: asking again early
        // only earns another refusal.
        assert_eq!(policy.delay(1, &limited(3_600)), None);
    }

    #[test]
    fn only_transient_failures_are_retried() {
        assert!(is_transient(&AiError::Timeout));
        assert!(is_transient(&AiError::Network("x".to_owned())));
        assert!(is_transient(&AiError::RateLimited { retry_after: None }));
        assert!(is_transient(&AiError::Provider {
            status: 503,
            message: String::new()
        }));
        // A model that answered 200 with broken JSON or a cut-off transcript: worth another try.
        assert!(is_transient(&AiError::Provider {
            status: 200,
            message: "the transcript could not be understood".to_owned()
        }));
        assert!(!is_transient(&AiError::Provider {
            status: 404,
            message: String::new()
        }));
        assert!(!is_transient(&AiError::Auth));
        assert!(!is_transient(&AiError::InvalidRequest(String::new())));
        assert!(!is_transient(&AiError::Unsupported(String::new())));
    }

    #[test]
    fn a_dropped_pause_ends_its_thread_at_once() {
        let mut waiting = Box::pin(Pause {
            delay: Duration::from_secs(600),
            shared: None,
        });
        let waker = futures_util::task::noop_waker();
        assert!(waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let shared = waiting.shared.clone().unwrap();
        assert_eq!(Arc::strong_count(&shared), 3, "us, the pause, the thread");
        drop(waiting);
        let started = std::time::Instant::now();
        while Arc::strong_count(&shared) > 1 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the thread kept sleeping after the pause was dropped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_real_pause_wakes_the_task() {
        futures_executor::block_on(pause(Duration::ZERO));
        let started = std::time::Instant::now();
        // `futures-executor` is a dev-dependency of the crate, available to unit tests too.
        futures_executor::block_on(pause(Duration::from_millis(30)));
        assert!(started.elapsed() >= Duration::from_millis(30));
    }
}
