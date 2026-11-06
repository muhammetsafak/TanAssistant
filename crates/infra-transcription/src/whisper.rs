//! Local speech-to-text with whisper.cpp (`whisper-rs`), the default transcription engine
//! (ADR-003, ADR-011).
//!
//! `WhisperTranscriber` implements the [`TranscriptionModel`] port, so the use case treats it
//! like any cloud adapter; the extra [`LocalTranscriber`] port lets the use case fail early
//! with a typed error when no model is installed, forecast the running time and measure speech.
//!
//! - **Silence is skipped** ([`crate::vad`]) before whisper sees the audio; segment times are
//!   mapped back onto the recording's own timeline.
//! - **GPU** (Vulkan, Metal, CUDA) when the build has the backend and the machine has a device;
//!   otherwise, or when the GPU fails to initialise, the CPU.
//! - **Calibration**: the first forecast times one short sample on the chosen model and backend
//!   and remembers the result (`calibration.json` next to the models).
//!
//! Long audio is processed in pieces of [`LOCAL_CHUNK_MS`] (cut at quiet moments). Inference is
//! blocking, so it runs on tokio's blocking pool, one transcription at a time. The model is
//! loaded for the duration of a transcription and released afterwards.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tan_application::ports::{
    AiError, AiResult, AudioSource, TranscriptSegmentDto, TranscriptionModel, TranscriptionRequest,
};
use tan_application::transcription::{
    AudioPrepError, AudioSpan, LocalEngineStatus, LocalSpeed, LocalTranscriber,
};
use tan_domain::LanguageCode;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::audio_prep::{plan_file, WavFile};
use crate::models::ModelManager;
use crate::vad::{self, Compacted};

/// Longest piece handed to whisper in one call (10 minutes keeps memory flat for hours-long
/// recordings).
pub const LOCAL_CHUNK_MS: u64 = 10 * 60 * 1_000;

/// Pieces shorter than this (100 ms at 16 kHz) hold nothing whisper can use.
const MIN_SAMPLES: usize = 1_600;

/// Segments whisper itself is this sure contain no speech are dropped.
const NO_SPEECH_DROP: f32 = 0.85;

/// Inference does not scale past this many threads.
const MAX_THREADS: usize = 8;

/// The sample the first forecast is calibrated on: 6.6 s of Turkish speech, spoken by a
/// text-to-speech voice, produced for this repository.
const CALIBRATION_WAV: &[u8] = include_bytes!("calibration.wav");

/// Name of the file (next to the models) that remembers the calibration.
const CALIBRATION_FILE: &str = "calibration.json";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Worker threads for inference. whisper.cpp's threads spin while waiting for each other, so
/// using every logical core (hyper-threads included) or competing with other busy processes
/// makes it much slower, not faster: use about one thread per physical core.
fn inference_threads() -> usize {
    let logical = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    (logical / 2).clamp(2, MAX_THREADS)
}

/// What does the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Whether whisper is asked to use a GPU.
    pub use_gpu: bool,
    /// `cpu`, `vulkan (<device>)`, `metal` or `cuda`.
    pub label: String,
}

impl Backend {
    fn cpu() -> Self {
        Self {
            use_gpu: false,
            label: "cpu".to_owned(),
        }
    }
}

/// Set once a GPU backend that looked usable failed to start a model: from then on this process
/// uses the CPU, and says so everywhere (labels, calibration keys).
static GPU_FAILED: AtomicBool = AtomicBool::new(false);

/// The best backend this build and machine offer: the GPU backend the build was compiled with
/// when a device is there and has worked, else the CPU. Probed once per process.
#[must_use]
pub fn detect_backend() -> Backend {
    static PROBED: OnceLock<Backend> = OnceLock::new();
    let probed = PROBED.get_or_init(probe_backend);
    if probed.use_gpu && GPU_FAILED.load(Ordering::Relaxed) {
        Backend::cpu()
    } else {
        probed.clone()
    }
}

fn probe_backend() -> Backend {
    #[allow(unused_mut)] // only the GPU features reassign it
    let mut backend = Backend::cpu();
    #[cfg(feature = "vulkan")]
    {
        if let Some(device) = first_vulkan_device() {
            backend = Backend {
                use_gpu: true,
                label: format!("vulkan ({device})"),
            };
        }
    }
    #[cfg(feature = "cuda")]
    {
        backend = Backend {
            use_gpu: true,
            label: "cuda".to_owned(),
        };
    }
    #[cfg(feature = "metal")]
    {
        backend = Backend {
            use_gpu: true,
            label: "metal".to_owned(),
        };
    }
    backend
}

/// The name of the first Vulkan device ggml can use, `None` when the machine has no usable
/// Vulkan (no driver, Vulkan older than 1.2, no device).
///
/// ggml initialises Vulkan with C++ code that throws when there is no driver. Only
/// `ggml_backend_vk_reg` catches that (and returns null); `ggml_backend_vk_get_device_count`,
/// which `whisper_rs::vulkan::list_devices` calls, does not, and an exception that reaches Rust
/// aborts the process ("Rust cannot catch foreign exceptions"). So the registry is asked first:
/// it initialises the instance exactly once, and the enumeration only runs after it succeeded.
#[cfg(feature = "vulkan")]
#[allow(unsafe_code)] // SAFETY: argument-less C call that is safe to make at any time
fn first_vulkan_device() -> Option<String> {
    let registry = unsafe { whisper_rs::whisper_rs_sys::ggml_backend_vk_reg() };
    if registry.is_null() {
        tracing::info!("no usable Vulkan driver; whisper runs on the CPU");
        return None;
    }
    whisper_rs::vulkan::list_devices()
        .into_iter()
        .next()
        .map(|device| device.name)
}

enum ModelSource {
    /// The chosen (or most preferred) model installed in the manager's directory, resolved per
    /// call.
    Installed(Arc<ModelManager>),
    /// One fixed model file (tests, advanced use).
    File(PathBuf),
}

struct LoadedModel {
    context: WhisperContext,
    backend: Backend,
}

/// What a calibration run measured, as remembered on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredCalibration {
    /// Model file, backend and thread count it was measured with.
    key: String,
    /// Seconds one 30-second window took.
    seconds_per_window: f64,
}

struct Inner {
    source: ModelSource,
    threads: usize,
    vad: AtomicBool,
    /// Serialises transcriptions and calibrations: two at once would only fight over the CPU.
    run: Mutex<()>,
    calibration: Mutex<Option<StoredCalibration>>,
}

/// The local whisper engine. Cheap to clone; clones share their state.
#[derive(Clone)]
pub struct WhisperTranscriber {
    inner: Arc<Inner>,
}

impl WhisperTranscriber {
    /// Uses the model chosen in `models`, or the most preferred installed one.
    #[must_use]
    pub fn new(models: Arc<ModelManager>) -> Self {
        Self::with_source(ModelSource::Installed(models))
    }

    /// Uses one specific ggml model file.
    #[must_use]
    pub fn with_model_file(path: impl Into<PathBuf>) -> Self {
        Self::with_source(ModelSource::File(path.into()))
    }

    fn with_source(source: ModelSource) -> Self {
        static HOOKS: Once = Once::new();
        // Route whisper.cpp's console output into `tracing`.
        HOOKS.call_once(whisper_rs::install_logging_hooks);
        Self {
            inner: Arc::new(Inner {
                source,
                threads: inference_threads(),
                vad: AtomicBool::new(true),
                run: Mutex::new(()),
                calibration: Mutex::new(None),
            }),
        }
    }

    /// Turns silence skipping on (the default) or off.
    pub fn set_vad(&self, enabled: bool) {
        self.inner.vad.store(enabled, Ordering::Relaxed);
    }

    /// Whether silence is skipped.
    #[must_use]
    pub fn vad_enabled(&self) -> bool {
        self.inner.vad.load(Ordering::Relaxed)
    }

    /// The model file that would be used right now, if any.
    #[must_use]
    pub fn model_path(&self) -> Option<PathBuf> {
        self.inner.model_path()
    }

    /// Forgets the remembered calibration (in memory and on disk) so the next forecast
    /// measures again.
    pub fn reset_calibration(&self) {
        *lock(&self.inner.calibration) = None;
        if let Some(path) = self.inner.calibration_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Inner {
    fn model_path(&self) -> Option<PathBuf> {
        match &self.source {
            ModelSource::Installed(models) => models.default_installed().map(|m| m.path),
            ModelSource::File(path) => path.is_file().then(|| path.clone()),
        }
    }

    fn calibration_path(&self) -> Option<PathBuf> {
        match &self.source {
            ModelSource::Installed(models) => Some(models.dir().join(CALIBRATION_FILE)),
            ModelSource::File(_) => None,
        }
    }

    /// Loads the model (seconds for the large ones), on the GPU when there is one and falling
    /// back to the CPU when the GPU cannot be set up.
    fn load(path: &Path) -> AiResult<LoadedModel> {
        let backend = detect_backend();
        let mut params = WhisperContextParameters::default();
        params.use_gpu(backend.use_gpu);
        match WhisperContext::new_with_params(path, params) {
            Ok(context) => Ok(LoadedModel { context, backend }),
            Err(error) if backend.use_gpu => {
                tracing::warn!(%error, backend = %backend.label, "GPU initialisation failed; using the CPU");
                GPU_FAILED.store(true, Ordering::Relaxed);
                let mut params = WhisperContextParameters::default();
                params.use_gpu(false);
                let context = WhisperContext::new_with_params(path, params)
                    .map_err(|e| engine_error(format!("cannot load the whisper model: {e}")))?;
                Ok(LoadedModel {
                    context,
                    backend: Backend {
                        use_gpu: false,
                        label: "cpu".to_owned(),
                    },
                })
            }
            Err(error) => Err(engine_error(format!(
                "cannot load the whisper model: {error}"
            ))),
        }
    }

    fn transcribe(&self, request: TranscriptionRequest) -> AiResult<Vec<TranscriptSegmentDto>> {
        let model_path = self
            .model_path()
            .ok_or_else(|| AiError::Unsupported("no whisper model is installed".to_owned()))?;
        let language = whisper_language(request.language.as_ref())?;

        let mut wav = match request.audio {
            AudioSource::Path(path) => WavFile::open(&path),
            AudioSource::Bytes { data, .. } => WavFile::from_bytes(data),
        }
        .map_err(|e| AiError::InvalidRequest(e.0))?;
        let spans =
            plan_file(&mut wav, LOCAL_CHUNK_MS).map_err(|e| AiError::InvalidRequest(e.0))?;
        let skip_silence = self.vad.load(Ordering::Relaxed);

        let _running = lock(&self.run);
        // Loaded for this transcription only: a large model would otherwise stay in memory for
        // as long as the tray application lives.
        let loaded = Self::load(&model_path)?;
        let mut state = loaded
            .context
            .create_state()
            .map_err(|e| engine_error(format!("cannot start whisper: {e}")))?;

        let mut segments = Vec::new();
        for span in spans {
            let samples = wav
                .load_16k_mono(span)
                .map_err(|e| AiError::InvalidRequest(e.0))?;
            if samples.len() < MIN_SAMPLES {
                continue;
            }
            let compact = if skip_silence {
                let regions = vad::regions_from_energies(&vad::frame_energies(&samples));
                match Compacted::new(&samples, &regions) {
                    Some(compact) => Some(compact),
                    None => continue, // nothing but silence
                }
            } else {
                None
            };
            let input = compact.as_ref().map_or(&samples[..], |c| &c.samples[..]);
            if input.len() < MIN_SAMPLES {
                continue;
            }
            for piece in self.run_piece(
                &mut state,
                input,
                language.as_deref(),
                request.prompt.as_deref(),
            )? {
                let (start, end) = match &compact {
                    Some(c) => (
                        c.original_ms(piece.start_ms, false),
                        c.original_ms(piece.end_ms, true),
                    ),
                    None => (piece.start_ms, piece.end_ms),
                };
                segments.push(TranscriptSegmentDto {
                    start_ms: span.start_ms + start,
                    end_ms: span.start_ms + end.max(start),
                    ..piece
                });
            }
        }
        Ok(segments)
    }

    /// Runs whisper on one piece (16 kHz mono) and returns its segments with times relative to
    /// the piece.
    fn run_piece(
        &self,
        state: &mut WhisperState,
        samples: &[f32],
        language: Option<&str>,
        prompt: Option<&str>,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // "auto" makes whisper detect the language (`detect_language` would stop after the
        // detection without transcribing).
        params.set_language(Some(language.unwrap_or("auto")));
        params.set_n_threads(i32::try_from(self.threads).unwrap_or(4));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
            params.set_initial_prompt(prompt);
        }

        state
            .full(params, samples)
            .map_err(|e| engine_error(format!("whisper failed: {e}")))?;

        let detected = match language {
            Some(_) => None,
            None => whisper_rs::get_lang_str(state.full_lang_id_from_state())
                .and_then(|code| LanguageCode::new(code).ok()),
        };
        let forced = language.and_then(|code| LanguageCode::new(code).ok());

        let mut out = Vec::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|e| engine_error(format!("whisper returned unreadable text: {e}")))?;
            let text = text.trim();
            if is_non_speech(text) || segment.no_speech_probability() > NO_SPEECH_DROP {
                continue;
            }
            // whisper reports centiseconds.
            let start = u64::try_from(segment.start_timestamp()).unwrap_or(0) * 10;
            let end = u64::try_from(segment.end_timestamp()).unwrap_or(0) * 10;
            out.push(TranscriptSegmentDto {
                start_ms: start,
                end_ms: end.max(start),
                text: text.to_owned(),
                language: forced.clone().or_else(|| detected.clone()),
            });
        }
        Ok(out)
    }

    /// The remembered calibration for `key`, from memory or from disk.
    fn stored_calibration(&self, key: &str) -> Option<f64> {
        let mut memory = lock(&self.calibration);
        if memory.is_none() {
            let text = std::fs::read_to_string(self.calibration_path()?).ok()?;
            *memory = serde_json::from_str(&text).ok();
        }
        memory
            .as_ref()
            .filter(|stored| stored.key == key)
            .map(|stored| stored.seconds_per_window)
    }

    fn remember_calibration(&self, key: String, seconds_per_window: f64) {
        let stored = StoredCalibration {
            key,
            seconds_per_window,
        };
        if let (Some(path), Ok(text)) = (self.calibration_path(), serde_json::to_string(&stored)) {
            if let Err(error) = std::fs::write(path, text) {
                tracing::debug!(%error, "cannot remember the whisper calibration");
            }
        }
        *lock(&self.calibration) = Some(stored);
    }

    /// Key of a calibration: the model file, the backend and the thread count it was measured
    /// with.
    fn calibration_key(&self, model_path: &Path, backend: &Backend) -> String {
        format!(
            "{}|{}|{}",
            model_path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            backend.label,
            self.threads
        )
    }

    /// The engine's speed, calibrating once per model, backend and thread count.
    fn speed(&self) -> AiResult<LocalSpeed> {
        let model_path = self
            .model_path()
            .ok_or_else(|| AiError::Unsupported("no whisper model is installed".to_owned()))?;
        // A remembered answer must not wait for a running transcription (the lock is held for
        // all of it), so the cache is looked at before the lock is taken.
        let remembered = |this: &Self| {
            let backend = detect_backend();
            this.stored_calibration(&this.calibration_key(&model_path, &backend))
                .map(|seconds_per_window| LocalSpeed {
                    seconds_per_window,
                    backend: backend.label,
                })
        };
        if let Some(speed) = remembered(self) {
            return Ok(speed);
        }
        let _running = lock(&self.run);
        // Another call may have calibrated while this one waited for the lock.
        if let Some(speed) = remembered(self) {
            return Ok(speed);
        }

        let loaded = Self::load(&model_path)?;
        // The label of the backend that really loaded (the GPU may have fallen back).
        let backend = loaded.backend.clone();
        let key = self.calibration_key(&model_path, &backend);
        let samples = WavFile::from_bytes(CALIBRATION_WAV.to_vec())
            .and_then(|mut wav| {
                let duration_ms = wav.info().duration_ms();
                wav.load_16k_mono(AudioSpan {
                    start_ms: 0,
                    end_ms: duration_ms,
                })
            })
            .map_err(|e| engine_error(e.0))?;
        let mut state = loaded
            .context
            .create_state()
            .map_err(|e| engine_error(format!("cannot start whisper: {e}")))?;
        if backend.use_gpu {
            // The first GPU run compiles shaders and warms caches: not what a transcription
            // costs.
            self.run_piece(&mut state, &samples, Some("tr"), None)?;
        }
        let started = Instant::now();
        self.run_piece(&mut state, &samples, Some("tr"), None)?;
        let seconds_per_window = started.elapsed().as_secs_f64();

        self.remember_calibration(key, seconds_per_window);
        Ok(LocalSpeed {
            seconds_per_window,
            backend: backend.label,
        })
    }

    /// Milliseconds of speech in the file, by the same detector the transcription uses.
    fn speech_ms(&self, audio: &Path) -> Result<u64, AudioPrepError> {
        let mut wav = WavFile::open(audio)?;
        if !self.vad.load(Ordering::Relaxed) {
            return Ok(wav.info().duration_ms());
        }
        let energies = wav.window_energies(vad::FRAME_MS)?;
        Ok(vad::speech_ms(&vad::regions_from_energies(&energies)))
    }
}

/// An engine-side failure. The port has no dedicated variant, so it travels as a provider
/// error (status 0 = local, no HTTP involved); the message names the cause.
fn engine_error(message: String) -> AiError {
    AiError::Provider { status: 0, message }
}

/// The whisper language code for a requested language: the primary subtag (`pt-br` becomes
/// `pt`), checked against what whisper supports. `None` means detect.
fn whisper_language(requested: Option<&LanguageCode>) -> AiResult<Option<String>> {
    let Some(code) = requested else {
        return Ok(None);
    };
    let primary = code.as_str().split('-').next().unwrap_or_default();
    if whisper_rs::get_lang_id(primary).is_none() {
        return Err(AiError::InvalidRequest(format!(
            "whisper does not support the language {code}"
        )));
    }
    Ok(Some(primary.to_owned()))
}

/// Whether the text is whisper's marker for silence or noise (`[BLANK_AUDIO]`, `(music)`,
/// `[Music]`...) rather than speech.
fn is_non_speech(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return true;
    }
    let bracketed = |open: char, close: char| text.starts_with(open) && text.ends_with(close);
    bracketed('[', ']')
        || bracketed('(', ')')
        || bracketed('*', '*')
        || text.chars().all(|c| c == '♪' || c == ' ')
}

fn join_error(e: &tokio::task::JoinError) -> AiError {
    engine_error(format!("the transcription task failed: {e}"))
}

impl TranscriptionModel for WhisperTranscriber {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> AiResult<Vec<TranscriptSegmentDto>> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.transcribe(request))
            .await
            .map_err(|e| join_error(&e))?
    }
}

impl LocalTranscriber for WhisperTranscriber {
    // The port is async-shaped; checking the models directory needs no `.await`.
    #[allow(clippy::unused_async_trait_impl)]
    async fn status(&self) -> LocalEngineStatus {
        if self.inner.model_path().is_some() {
            LocalEngineStatus::Ready
        } else {
            LocalEngineStatus::ModelMissing
        }
    }

    async fn speed(&self) -> AiResult<LocalSpeed> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.speed())
            .await
            .map_err(|e| join_error(&e))?
    }

    async fn speech_ms(&self, audio: &Path) -> Result<u64, AudioPrepError> {
        let inner = self.inner.clone();
        let audio = audio.to_path_buf();
        tokio::task::spawn_blocking(move || inner.speech_ms(&audio))
            .await
            .map_err(|e| AudioPrepError(format!("audio task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{detect_backend, is_non_speech, lock, whisper_language, WhisperTranscriber};
    use tan_domain::LanguageCode;

    #[test]
    fn a_remembered_forecast_does_not_wait_for_a_running_transcription() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("ggml-fake.bin");
        std::fs::write(&model, b"not a model").unwrap();
        let engine = WhisperTranscriber::with_model_file(&model);
        let inner = engine.inner.clone();
        let key = inner.calibration_key(&model, &detect_backend());
        inner.remember_calibration(key, 2.5);

        // A transcription is running: it holds the run lock for its whole duration.
        let _running = lock(&inner.run);
        let (sender, receiver) = mpsc::channel();
        let asker = inner.clone();
        std::thread::spawn(move || sender.send(asker.speed()).unwrap());

        let speed = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the forecast waited for the running transcription")
            .unwrap();
        assert!((speed.seconds_per_window - 2.5).abs() < f64::EPSILON);
        assert_eq!(speed.backend, detect_backend().label);
    }

    #[test]
    fn silence_markers_are_not_speech() {
        for marker in [
            "",
            "  ",
            "[BLANK_AUDIO]",
            "[Music]",
            "(music)",
            "*laughs*",
            "♪ ♪",
        ] {
            assert!(is_non_speech(marker), "{marker:?}");
        }
        for speech in [
            "Merhaba",
            "Toplantıyı başlatalım.",
            "[1] first item and more",
        ] {
            assert!(!is_non_speech(speech), "{speech:?}");
        }
    }

    #[test]
    fn language_codes_map_to_whisper_codes() {
        assert_eq!(whisper_language(None).unwrap(), None);
        assert_eq!(
            whisper_language(Some(&LanguageCode::turkish()))
                .unwrap()
                .as_deref(),
            Some("tr")
        );
        let pt_br = LanguageCode::new("pt-BR").unwrap();
        assert_eq!(
            whisper_language(Some(&pt_br)).unwrap().as_deref(),
            Some("pt")
        );
        let unknown = LanguageCode::new("xx").unwrap();
        assert!(whisper_language(Some(&unknown)).is_err());
    }

    #[test]
    fn a_build_without_gpu_features_uses_the_cpu() {
        let backend = detect_backend();
        if cfg!(not(any(
            feature = "vulkan",
            feature = "metal",
            feature = "cuda"
        ))) {
            assert!(!backend.use_gpu);
            assert_eq!(backend.label, "cpu");
        }
    }
}
