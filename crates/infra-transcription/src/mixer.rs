//! Mixes the tracks of a session into one mono WAV file.
//!
//! A session keeps its microphone and its system audio as separate files, each at the rate and
//! channel count of its device. [`WavMixer`] brings them to the higher of the two rates, mixes
//! them down to mono, adds them sample by sample (clamped to the valid range) and writes one
//! 16-bit PCM WAV. The shorter track is completed with silence.
//!
//! The work is streamed in windows of a few seconds, so memory use does not depend on the
//! length of the recording. The result goes to a temporary file next to the destination and is
//! renamed into place at the end; a failure leaves nothing behind.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hound::{SampleFormat, WavSpec, WavWriter};
use tan_application::ports::{AudioMixError, AudioMixer};
use tan_application::transcription::AudioPrepError;

use crate::audio_prep::{StreamResampler, WavFile};

/// Length of the stretch of a track read (and resampled) at a time, in milliseconds.
const READ_WINDOW_MS: u64 = 4_000;

/// Frames mixed and written at a time.
const MIX_CHUNK_FRAMES: usize = 16_384;

/// Distinguishes temporary files made by the same process in the same instant.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn source_error(error: AudioPrepError) -> AudioMixError {
    AudioMixError::Source(error.0)
}

/// [`AudioMixer`] for WAV files.
#[derive(Debug, Clone, Copy, Default)]
pub struct WavMixer;

impl AudioMixer for WavMixer {
    fn mix_to_wav(&self, tracks: &[PathBuf], destination: &Path) -> Result<(), AudioMixError> {
        if tracks.is_empty() {
            return Err(AudioMixError::NoTracks);
        }
        // Open everything first: an unreadable track fails before any file is created.
        let mut files = Vec::with_capacity(tracks.len());
        for path in tracks {
            files.push(WavFile::open(path).map_err(source_error)?);
        }
        let target_rate = files
            .iter()
            .map(|file| file.info().sample_rate)
            .max()
            .unwrap_or(1);
        let mut streams = files
            .into_iter()
            .map(|file| TrackStream::new(file, target_rate))
            .collect::<Result<Vec<_>, _>>()?;
        let total_frames = streams
            .iter()
            .map(|stream| stream.output_frames)
            .max()
            .unwrap_or(0);
        if total_frames == 0 {
            return Err(AudioMixError::Empty);
        }

        let temp = temp_path_for(destination)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(AudioMixError::Destination)?;
        let written = write_mix(file, &mut streams, target_rate, total_frames);
        let result = written
            .and_then(|()| fs::rename(&temp, destination).map_err(AudioMixError::Destination));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

/// A name beside `destination` that no other file has: the hidden `.<name>.<unique>.partial`.
fn temp_path_for(destination: &Path) -> Result<PathBuf, AudioMixError> {
    let name = destination.file_name().ok_or_else(|| {
        AudioMixError::Destination(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the destination has no file name",
        ))
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".{}-{nanos}-{}.partial",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(destination.with_file_name(temp_name))
}

/// Mixes `streams` and writes `total_frames` frames to `file` as a mono 16-bit WAV.
fn write_mix(
    file: File,
    streams: &mut [TrackStream],
    sample_rate: u32,
    total_frames: u64,
) -> Result<(), AudioMixError> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::new(BufWriter::new(file), spec).map_err(wav_write_error)?;
    let mut written = 0_u64;
    while written < total_frames {
        let wanted = usize::try_from(total_frames - written)
            .map_or(MIX_CHUNK_FRAMES, |left| left.min(MIX_CHUNK_FRAMES));
        let mut mix = vec![0.0_f32; wanted];
        for stream in streams.iter_mut() {
            stream.fill(wanted)?;
            // A track that has ended contributes silence for the rest.
            let take = wanted.min(stream.queue.len());
            for (slot, sample) in mix.iter_mut().zip(stream.queue.drain(..take)) {
                *slot += sample;
            }
        }
        for sample in mix {
            writer
                .write_sample(sample_to_i16(sample))
                .map_err(wav_write_error)?;
        }
        written += wanted as u64;
    }
    writer.finalize().map_err(wav_write_error)
}

fn wav_write_error(error: hound::Error) -> AudioMixError {
    match error {
        hound::Error::IoError(error) => AudioMixError::Destination(error),
        other => AudioMixError::Destination(std::io::Error::other(other)),
    }
}

/// A mono sample as 16-bit PCM, clamped to the valid range.
fn sample_to_i16(sample: f32) -> i16 {
    #[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
    let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
    value
}

/// One track on its way to the target rate: read in windows, resampled, queued.
struct TrackStream {
    file: WavFile,
    resampler: StreamResampler,
    window_frames: usize,
    /// Resampled mono samples at the target rate that are not mixed yet.
    queue: VecDeque<f32>,
    source_done: bool,
    /// How long the track is at the target rate.
    output_frames: u64,
}

impl TrackStream {
    fn new(file: WavFile, target_rate: u32) -> Result<Self, AudioMixError> {
        let info = file.info();
        let window_frames = usize::try_from(u64::from(info.sample_rate) * READ_WINDOW_MS / 1_000)
            .map_err(|e| AudioMixError::Source(e.to_string()))?
            .max(1);
        let output_frames = (u128::from(info.frames) * u128::from(target_rate))
            .div_ceil(u128::from(info.sample_rate));
        Ok(Self {
            resampler: StreamResampler::new(info.sample_rate, target_rate).map_err(source_error)?,
            file,
            window_frames,
            queue: VecDeque::new(),
            source_done: false,
            output_frames: u64::try_from(output_frames)
                .map_err(|e| AudioMixError::Source(e.to_string()))?,
        })
    }

    /// Reads on until at least `wanted` samples are queued or the track is over.
    fn fill(&mut self, wanted: usize) -> Result<(), AudioMixError> {
        while self.queue.len() < wanted && !self.source_done {
            let block = self
                .file
                .read_mono(self.window_frames)
                .map_err(source_error)?;
            // A short read is the end of the file.
            self.source_done = block.len() < self.window_frames;
            self.queue
                .extend(self.resampler.push(&block).map_err(source_error)?);
            if self.source_done {
                self.queue
                    .extend(self.resampler.finish().map_err(source_error)?);
            }
        }
        Ok(())
    }
}
