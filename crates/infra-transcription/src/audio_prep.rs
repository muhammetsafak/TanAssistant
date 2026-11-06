//! Audio preparation: read a WAV file, mix it down to mono, resample to 16 kHz and cut long
//! recordings into pieces at quiet moments.
//!
//! Whisper needs 16 kHz mono `f32`; cloud providers cap the upload size, so they get 16 kHz
//! mono 16-bit WAV pieces. Tracks are stored at the device's native rate (usually 48 kHz
//! stereo), so every consumer goes through here.
//!
//! Reading is by span (`seek` + read), so an hour-long file is never held in memory whole:
//! only the planning pass scans it, keeping one number per 100 ms.

use std::io::{self, BufReader, Cursor, Read, Seek};
use std::path::{Component, Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use tan_application::transcription::{AudioChunker, AudioPrepError, AudioSpan, SessionAudioFiles};
use tan_domain::SessionId;

/// The sample rate whisper models expect.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Energy is measured per window of this length when looking for a quiet place to cut.
const ENERGY_WINDOW_MS: u64 = 100;

/// A cut is looked for in the last this-many milliseconds of a piece.
const CUT_SEARCH_MS: u64 = 20_000;

/// Pieces are never planned shorter than this.
const MIN_CHUNK_MS: u64 = 2_000;

/// Frames read per `read` call while scanning.
const SCAN_BLOCK_FRAMES: usize = 16_384;

fn prep_error(why: impl std::fmt::Display) -> AudioPrepError {
    AudioPrepError(why.to_string())
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// Format of a WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Interleaved channels.
    pub channels: u16,
    /// Frames (samples per channel).
    pub frames: u64,
}

impl WavInfo {
    /// Duration in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.frames.saturating_mul(1_000) / u64::from(self.sample_rate.max(1))
    }
}

/// An open WAV file.
pub struct WavFile {
    reader: WavReader<Box<dyn ReadSeek>>,
    info: WavInfo,
}

impl WavFile {
    /// Opens a WAV file from disk.
    pub fn open(path: &Path) -> Result<Self, AudioPrepError> {
        let file = std::fs::File::open(path)
            .map_err(|e| prep_error(format!("cannot open the audio file: {e}")))?;
        Self::from_reader(Box::new(BufReader::new(file)))
    }

    /// Opens a WAV file held in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AudioPrepError> {
        Self::from_reader(Box::new(Cursor::new(bytes)))
    }

    fn from_reader(reader: Box<dyn ReadSeek>) -> Result<Self, AudioPrepError> {
        let reader = WavReader::new(reader)
            .map_err(|e| prep_error(format!("not a readable WAV file: {e}")))?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 {
            return Err(prep_error("the WAV file has no channels or no sample rate"));
        }
        let supported = match spec.sample_format {
            SampleFormat::Int => (1..=32).contains(&spec.bits_per_sample),
            SampleFormat::Float => spec.bits_per_sample == 32,
        };
        if !supported {
            return Err(prep_error(format!(
                "unsupported WAV sample format ({} bit {:?})",
                spec.bits_per_sample, spec.sample_format
            )));
        }
        let info = WavInfo {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
            frames: u64::from(reader.duration()),
        };
        Ok(Self { reader, info })
    }

    /// The file's format and length.
    #[must_use]
    pub fn info(&self) -> WavInfo {
        self.info
    }

    /// Reads up to `frames` frames from the current position, mixed down to mono `f32` in
    /// `-1.0..=1.0`. Returns fewer at the end of the file.
    fn read_mono(&mut self, frames: usize) -> Result<Vec<f32>, AudioPrepError> {
        let spec = self.reader.spec();
        let channels = usize::from(spec.channels);
        let wanted = frames.saturating_mul(channels);
        let mut interleaved: Vec<f32> = Vec::with_capacity(wanted.min(1 << 22));
        match spec.sample_format {
            SampleFormat::Float => {
                for sample in self.reader.samples::<f32>().take(wanted) {
                    interleaved.push(sample.map_err(prep_error)?);
                }
            }
            SampleFormat::Int => {
                #[allow(clippy::cast_precision_loss)] // 2^31 is exactly representable
                let scale = 1.0 / (1_u64 << (spec.bits_per_sample - 1)) as f32;
                for sample in self.reader.samples::<i32>().take(wanted) {
                    #[allow(clippy::cast_precision_loss)] // audio precision is far below f32's
                    interleaved.push(sample.map_err(prep_error)? as f32 * scale);
                }
            }
        }
        if channels == 1 {
            return Ok(interleaved);
        }
        #[allow(clippy::cast_precision_loss)]
        let divisor = channels as f32;
        Ok(interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / divisor)
            .collect())
    }

    /// The span as mono samples at the file's own sample rate.
    fn read_span_mono(&mut self, span: AudioSpan) -> Result<Vec<f32>, AudioPrepError> {
        let rate = u64::from(self.info.sample_rate);
        let first = (span.start_ms.saturating_mul(rate) / 1_000).min(self.info.frames);
        let last = (span.end_ms.saturating_mul(rate) / 1_000).min(self.info.frames);
        if last <= first {
            return Ok(Vec::new());
        }
        let first_frame = u32::try_from(first).map_err(prep_error)?;
        self.reader
            .seek(first_frame)
            .map_err(|e| prep_error(format!("cannot seek in the audio file: {e}")))?;
        let frames = usize::try_from(last - first).map_err(prep_error)?;
        self.read_mono(frames)
    }

    /// The span as 16 kHz mono samples (what whisper consumes).
    pub fn load_16k_mono(&mut self, span: AudioSpan) -> Result<Vec<f32>, AudioPrepError> {
        let native = self.read_span_mono(span)?;
        resample_to_16k(native, self.info.sample_rate)
    }

    /// Energy (RMS) of every `window_ms` window of the whole file, mixed to mono. Used to find
    /// quiet places for cutting and to measure speech; one `f32` per window keeps memory flat.
    pub fn window_energies(&mut self, window_ms: u64) -> Result<Vec<f32>, AudioPrepError> {
        self.reader
            .seek(0)
            .map_err(|e| prep_error(format!("cannot seek in the audio file: {e}")))?;
        let window_frames = usize::try_from(u64::from(self.info.sample_rate) * window_ms / 1_000)
            .map_err(prep_error)?
            .max(1);
        let mut energies = Vec::new();
        let mut pending: Vec<f32> = Vec::new();
        loop {
            let block = self.read_mono(SCAN_BLOCK_FRAMES)?;
            let done = block.len() < SCAN_BLOCK_FRAMES;
            pending.extend(block);
            let whole = pending.len() / window_frames * window_frames;
            for window in pending[..whole].chunks_exact(window_frames) {
                energies.push(rms(window));
            }
            pending.drain(..whole);
            if done {
                if !pending.is_empty() {
                    energies.push(rms(&pending));
                }
                return Ok(energies);
            }
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn rms(window: &[f32]) -> f32 {
    if window.is_empty() {
        return 0.0;
    }
    (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt()
}

/// Resamples mono audio to 16 kHz. Audio already at 16 kHz is returned as is.
pub fn resample_to_16k(mono: Vec<f32>, from_rate: u32) -> Result<Vec<f32>, AudioPrepError> {
    if from_rate == WHISPER_SAMPLE_RATE || mono.is_empty() {
        return Ok(mono);
    }
    let frames = mono.len();
    let mut resampler = Fft::<f32>::new(
        from_rate as usize,
        WHISPER_SAMPLE_RATE as usize,
        1_024,
        1,
        FixedSync::Both,
    )
    .map_err(|e| prep_error(format!("cannot resample from {from_rate} Hz: {e}")))?;
    let input = InterleavedSlice::new(&mono, 1, frames).map_err(prep_error)?;
    let output = resampler
        .process_all(&input, frames, None)
        .map_err(|e| prep_error(format!("resampling failed: {e}")))?;
    Ok(output.take_data())
}

/// Encodes 16 kHz mono samples as a 16-bit PCM WAV file.
pub fn encode_wav_16k_mono(samples: &[f32]) -> Result<Vec<u8>, AudioPrepError> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: WHISPER_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));
    {
        let mut writer = WavWriter::new(&mut cursor, spec).map_err(prep_error)?;
        for &sample in samples {
            #[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            writer.write_sample(value).map_err(prep_error)?;
        }
        writer.finalize().map_err(prep_error)?;
    }
    Ok(cursor.into_inner())
}

/// Plans pieces of at most `max_chunk_ms` from the per-100-ms energies of a file that lasts
/// `duration_ms`. Every cut is placed in the quietest window of the last 20 s of a piece,
/// never earlier than half the piece length, so pieces stay large and words stay whole.
#[must_use]
pub fn plan_spans(duration_ms: u64, energies: &[f32], max_chunk_ms: u64) -> Vec<AudioSpan> {
    let max_chunk_ms = max_chunk_ms.max(MIN_CHUNK_MS);
    let mut spans = Vec::new();
    let mut start = 0_u64;
    while duration_ms - start > max_chunk_ms {
        let target = start + max_chunk_ms;
        let search_from = target.saturating_sub(CUT_SEARCH_MS.min(max_chunk_ms / 2));
        let first = usize::try_from(search_from.div_ceil(ENERGY_WINDOW_MS)).unwrap_or(usize::MAX);
        let last = usize::try_from(target / ENERGY_WINDOW_MS)
            .unwrap_or(usize::MAX)
            .min(energies.len());
        // Quietest window; on a tie the latest one, which keeps the piece largest.
        let cut = (first..last)
            .min_by(|&a, &b| energies[a].total_cmp(&energies[b]).then_with(|| b.cmp(&a)))
            .map_or(target, |window| {
                window as u64 * ENERGY_WINDOW_MS + ENERGY_WINDOW_MS / 2
            });
        let end = cut.clamp(start + 1, target);
        spans.push(AudioSpan {
            start_ms: start,
            end_ms: end,
        });
        start = end;
    }
    spans.push(AudioSpan {
        start_ms: start,
        end_ms: duration_ms,
    });
    spans
}

/// Plans the pieces of a WAV file (see [`plan_spans`]). A file that fits in one piece is not
/// scanned.
pub fn plan_file(wav: &mut WavFile, max_chunk_ms: u64) -> Result<Vec<AudioSpan>, AudioPrepError> {
    let duration_ms = wav.info().duration_ms();
    if duration_ms <= max_chunk_ms {
        return Ok(vec![AudioSpan {
            start_ms: 0,
            end_ms: duration_ms,
        }]);
    }
    let energies = wav.window_energies(ENERGY_WINDOW_MS)?;
    Ok(plan_spans(duration_ms, &energies, max_chunk_ms))
}

fn join_error(e: &tokio::task::JoinError) -> AudioPrepError {
    prep_error(format!("audio task failed: {e}"))
}

/// [`AudioChunker`] for WAV files. The work is blocking, so it runs on tokio's blocking pool.
#[derive(Debug, Clone, Copy, Default)]
pub struct WavAudioChunker;

impl AudioChunker for WavAudioChunker {
    async fn plan(
        &self,
        audio: &Path,
        max_chunk_ms: u64,
    ) -> Result<Vec<AudioSpan>, AudioPrepError> {
        let path = audio.to_path_buf();
        tokio::task::spawn_blocking(move || plan_file(&mut WavFile::open(&path)?, max_chunk_ms))
            .await
            .map_err(|e| join_error(&e))?
    }

    async fn load_wav(&self, audio: &Path, span: AudioSpan) -> Result<Vec<u8>, AudioPrepError> {
        let path = audio.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let samples = WavFile::open(&path)?.load_16k_mono(span)?;
            encode_wav_16k_mono(&samples)
        })
        .await
        .map_err(|e| join_error(&e))?
    }
}

/// [`SessionAudioFiles`] for the `<data>/sessions/<id>/...` layout.
#[derive(Debug, Clone)]
pub struct SessionFiles {
    sessions_dir: PathBuf,
}

impl SessionFiles {
    /// Files under `sessions_dir` (the `sessions` folder of the data directory).
    #[must_use]
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }
}

impl SessionAudioFiles for SessionFiles {
    fn audio_path(&self, session_id: &SessionId, relative_path: &str) -> io::Result<PathBuf> {
        let id = session_id.as_str();
        let id_ok = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        // The stored path comes from the database: only plain relative segments are allowed,
        // so it cannot point outside the session folder (no `..`, no drive, no root).
        let relative = Path::new(relative_path);
        let relative_ok = !relative_path.is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        if !id_ok || !relative_ok {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the audio path is not inside the session folder",
            ));
        }
        Ok(self.sessions_dir.join(id).join(relative))
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, spec: WavSpec, frames: usize, f: impl Fn(usize, u16) -> f32) {
        let mut writer = WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            for channel in 0..spec.channels {
                let value = f(frame, channel);
                match (spec.sample_format, spec.bits_per_sample) {
                    (SampleFormat::Float, _) => writer.write_sample(value).unwrap(),
                    (SampleFormat::Int, 8) => writer.write_sample((value * 127.0) as i8).unwrap(),
                    (SampleFormat::Int, 16) => {
                        writer.write_sample((value * 32_767.0) as i16).unwrap();
                    }
                    (SampleFormat::Int, 24) => {
                        writer.write_sample((value * 8_388_607.0) as i32).unwrap();
                    }
                    (SampleFormat::Int, _) => {
                        writer
                            .write_sample((value * 2_147_483_000.0) as i32)
                            .unwrap();
                    }
                }
            }
        }
        writer.finalize().unwrap();
    }

    fn spec(channels: u16, rate: u32, bits: u16, format: SampleFormat) -> WavSpec {
        WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: bits,
            sample_format: format,
        }
    }

    fn sine(rate: u32, hz: f32) -> impl Fn(usize, u16) -> f32 {
        move |frame, _| (2.0 * std::f32::consts::PI * hz * frame as f32 / rate as f32).sin() * 0.5
    }

    fn zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count()
    }

    #[test]
    fn resampled_output_is_16khz_mono_whatever_the_input() {
        let dir = tempfile::tempdir().unwrap();
        for (rate, channels) in [
            (48_000, 2),
            (44_100, 2),
            (48_000, 1),
            (22_050, 1),
            (8_000, 1),
        ] {
            let path = dir.path().join(format!("in-{rate}-{channels}.wav"));
            write_wav(
                &path,
                spec(channels, rate, 16, SampleFormat::Int),
                rate as usize * 2,
                sine(rate, 440.0),
            );
            let mut wav = WavFile::open(&path).unwrap();
            assert_eq!(wav.info().sample_rate, rate);
            assert_eq!(wav.info().channels, channels);

            let samples = wav
                .load_16k_mono(AudioSpan {
                    start_ms: 0,
                    end_ms: 2_000,
                })
                .unwrap();
            let expected = 32_000_i64;
            assert!(
                (samples.len() as i64 - expected).abs() < 64,
                "{rate} Hz x{channels}: got {} samples",
                samples.len()
            );
            // 440 Hz survives resampling: about 880 rising zero crossings in 2 s.
            let crossings = zero_crossings(&samples);
            assert!(
                (870..=890).contains(&crossings),
                "{rate}: {crossings} crossings"
            );

            // And the encoded piece is a 16 kHz mono 16-bit WAV.
            let encoded = encode_wav_16k_mono(&samples).unwrap();
            let reread = WavReader::new(Cursor::new(encoded)).unwrap();
            assert_eq!(reread.spec().sample_rate, 16_000);
            assert_eq!(reread.spec().channels, 1);
            assert_eq!(reread.spec().bits_per_sample, 16);
            assert_eq!(reread.duration() as usize, samples.len());
        }
    }

    #[test]
    fn audio_at_16khz_is_not_resampled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.wav");
        write_wav(
            &path,
            spec(1, 16_000, 16, SampleFormat::Int),
            16_000,
            sine(16_000, 300.0),
        );
        let samples = WavFile::open(&path)
            .unwrap()
            .load_16k_mono(AudioSpan {
                start_ms: 0,
                end_ms: 1_000,
            })
            .unwrap();
        assert_eq!(samples.len(), 16_000);
    }

    #[test]
    fn stereo_is_averaged_so_opposite_channels_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.wav");
        write_wav(
            &path,
            spec(2, 16_000, 16, SampleFormat::Int),
            1_600,
            |_, ch| {
                if ch == 0 {
                    0.5
                } else {
                    -0.5
                }
            },
        );
        let samples = WavFile::open(&path)
            .unwrap()
            .load_16k_mono(AudioSpan {
                start_ms: 0,
                end_ms: 100,
            })
            .unwrap();
        assert_eq!(samples.len(), 1_600);
        assert!(samples.iter().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn every_supported_sample_format_reads_back_at_the_same_level() {
        let dir = tempfile::tempdir().unwrap();
        for (bits, format) in [
            (8, SampleFormat::Int),
            (16, SampleFormat::Int),
            (24, SampleFormat::Int),
            (32, SampleFormat::Int),
            (32, SampleFormat::Float),
        ] {
            let path = dir.path().join(format!("in-{bits}-{format:?}.wav"));
            write_wav(&path, spec(1, 16_000, bits, format), 1_600, |_, _| 0.5);
            let samples = WavFile::open(&path)
                .unwrap()
                .load_16k_mono(AudioSpan {
                    start_ms: 0,
                    end_ms: 100,
                })
                .unwrap();
            assert_eq!(samples.len(), 1_600, "{bits} {format:?}");
            assert!(
                (samples[800] - 0.5).abs() < 0.02,
                "{bits} {format:?}: {}",
                samples[800]
            );
        }
    }

    #[test]
    fn a_span_reads_exactly_that_stretch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ramp.wav");
        // 16 kHz mono, sample value encodes the time: 0.1 in the first second, 0.2 in the
        // second, 0.3 in the third.
        write_wav(
            &path,
            spec(1, 16_000, 16, SampleFormat::Int),
            48_000,
            |frame, _| 0.1 * (frame / 16_000 + 1) as f32,
        );
        let mut wav = WavFile::open(&path).unwrap();
        let samples = wav
            .load_16k_mono(AudioSpan {
                start_ms: 1_000,
                end_ms: 2_000,
            })
            .unwrap();
        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|s| (s - 0.2).abs() < 0.01));
        // Past the end is clipped, not an error.
        let tail = wav
            .load_16k_mono(AudioSpan {
                start_ms: 2_500,
                end_ms: 9_000,
            })
            .unwrap();
        assert_eq!(tail.len(), 8_000);
        let beyond = wav
            .load_16k_mono(AudioSpan {
                start_ms: 9_000,
                end_ms: 9_500,
            })
            .unwrap();
        assert_eq!(beyond.len(), 0);
    }

    #[test]
    fn unreadable_audio_is_an_error_not_a_panic() {
        assert!(WavFile::from_bytes(b"definitely not a wav file".to_vec()).is_err());
        assert!(WavFile::open(Path::new("/no/such/file.wav")).is_err());
        let empty = encode_wav_16k_mono(&[]).unwrap();
        let wav = WavFile::from_bytes(empty).unwrap();
        assert_eq!(wav.info().frames, 0);
    }

    #[test]
    fn short_audio_is_one_span() {
        let spans = plan_spans(5_000, &[], 600_000);
        assert_eq!(
            spans,
            [AudioSpan {
                start_ms: 0,
                end_ms: 5_000
            }]
        );
    }

    #[test]
    fn cuts_land_in_the_quietest_window_and_spans_tile_the_file() {
        // 100 s of loud audio with one quiet window at 38.4 s; pieces of at most 40 s.
        let mut energies = vec![0.5_f32; 1_000];
        energies[384] = 0.0;
        let spans = plan_spans(100_000, &energies, 40_000);
        assert_eq!(
            spans[0],
            AudioSpan {
                start_ms: 0,
                end_ms: 38_450
            }
        );
        assert_eq!(spans.first().unwrap().start_ms, 0);
        assert_eq!(spans.last().unwrap().end_ms, 100_000);
        for pair in spans.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms, "no gap, no overlap");
        }
        for span in &spans {
            assert!(span.end_ms - span.start_ms <= 40_000);
        }
    }

    #[test]
    fn without_a_quiet_spot_the_cut_is_as_late_as_allowed() {
        let energies = vec![0.5_f32; 1_000];
        let spans = plan_spans(100_000, &energies, 40_000);
        assert_eq!(
            spans[0].end_ms, 39_950,
            "middle of the last window before the limit"
        );
        assert!(spans.iter().all(|s| s.end_ms - s.start_ms <= 40_000));
    }

    #[test]
    fn planning_a_real_file_scans_it_and_cuts_at_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        // 25 s, 8 kHz: a tone with a 300 ms pause from 8.0 s; pieces of at most 10 s.
        write_wav(
            &path,
            spec(1, 8_000, 16, SampleFormat::Int),
            25 * 8_000,
            |frame, _| {
                let t = frame as f32 / 8_000.0;
                if (8.0..8.3).contains(&t) {
                    0.0
                } else {
                    (t * 2_000.0).sin() * 0.4
                }
            },
        );
        let mut wav = WavFile::open(&path).unwrap();
        let spans = plan_file(&mut wav, 10_000).unwrap();
        assert!(spans.len() >= 3, "{spans:?}");
        assert!((8_000..=8_300).contains(&spans[0].end_ms), "{:?}", spans[0]);
        assert_eq!(spans.last().unwrap().end_ms, 25_000);
        for pair in spans.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms);
        }
        assert!(spans.iter().all(|s| s.end_ms - s.start_ms <= 10_000));
    }

    #[test]
    fn session_files_stay_inside_the_session_folder() {
        let files = SessionFiles::new("/data/sessions");
        let id = SessionId::new("0190-abc_1");
        let ok = files.audio_path(&id, "audio/microphone.wav").unwrap();
        assert!(
            ok.ends_with("sessions/0190-abc_1/audio/microphone.wav")
                || ok.ends_with("sessions\\0190-abc_1\\audio\\microphone.wav")
        );
        for bad in [
            "",
            "../x.wav",
            "audio/../../x.wav",
            "/etc/passwd",
            "C:\\x.wav",
            "C:/x.wav",
            "./a/../b",
        ] {
            assert!(files.audio_path(&id, bad).is_err(), "{bad:?}");
        }
        for bad_id in ["", "..", "a/b", "a b"] {
            assert!(files
                .audio_path(&SessionId::new(bad_id), "audio/x.wav")
                .is_err());
        }
    }
}
