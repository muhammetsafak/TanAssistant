//! 16-bit PCM WAV files that stay readable if the process dies.
//!
//! Audio is stored at the device's native sample rate and channel count. The header of a
//! WAV file holds the data length, and a plain writer only fixes it when the file is closed,
//! so a crash would leave a file most players refuse to open. [`WavTrackWriter::checkpoint`]
//! rewrites the header and flushes; the recording service calls it every few seconds, which
//! bounds what a crash can lose.
//!
//! Samples are stored as 16-bit integers (half the size of float32; the speech content does
//! not need more): about 11.5 MB per minute for 48 kHz stereo.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use tan_application::ports::{AudioFileInfo, AudioFormat, AudioWriter};

/// Size of the biggest data chunk a WAV header can describe, minus headroom for the header.
const MAX_DATA_BYTES: u64 = u32::MAX as u64 - 1_024;
const BYTES_PER_SAMPLE: u64 = 2;
/// Silence is written in blocks of this many samples.
const SILENCE_BLOCK: usize = 4_096;

fn to_io(error: hound::Error) -> io::Error {
    match error {
        hound::Error::IoError(e) => e,
        other => io::Error::other(other),
    }
}

/// Converts a float sample to 16-bit PCM, clamping out-of-range values.
#[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

/// Appends audio to one WAV file.
pub struct WavTrackWriter {
    writer: WavWriter<BufWriter<File>>,
    relative_path: String,
    format: AudioFormat,
    frames: u64,
}

impl WavTrackWriter {
    /// Creates (truncating) the file at `path`. `relative_path` is only what
    /// [`AudioWriter::finish`] reports back (the path relative to the session directory).
    pub fn create(
        path: &Path,
        relative_path: impl Into<String>,
        format: AudioFormat,
    ) -> io::Result<Self> {
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio format has no channels or no sample rate",
            ));
        }
        let spec = WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let writer = WavWriter::new(BufWriter::new(File::create(path)?), spec).map_err(to_io)?;
        Ok(Self {
            writer,
            relative_path: relative_path.into(),
            format,
            frames: 0,
        })
    }

    /// Fails when `extra_frames` more frames would not fit the 4 GiB WAV data limit.
    fn ensure_room(&self, extra_frames: u64) -> io::Result<()> {
        let bytes = self
            .frames
            .saturating_add(extra_frames)
            .saturating_mul(u64::from(self.format.channels))
            .saturating_mul(BYTES_PER_SAMPLE);
        if bytes > MAX_DATA_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "the audio file reached the 4 GiB WAV size limit",
            ));
        }
        Ok(())
    }
}

impl AudioWriter for WavTrackWriter {
    fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        let channels = usize::from(self.format.channels);
        let whole_frames = samples.len() / channels;
        let frames = whole_frames as u64;
        self.ensure_room(frames)?;
        // A trailing partial frame (never produced by the adapters) is ignored so the file
        // always holds whole frames.
        for sample in &samples[..whole_frames * channels] {
            self.writer.write_sample(to_i16(*sample)).map_err(to_io)?;
        }
        self.frames += frames;
        Ok(())
    }

    fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        self.ensure_room(frames)?;
        let channels = u64::from(self.format.channels);
        let mut remaining = frames * channels;
        while remaining > 0 {
            let block = remaining.min(SILENCE_BLOCK as u64);
            for _ in 0..block {
                self.writer.write_sample(0_i16).map_err(to_io)?;
            }
            remaining -= block;
        }
        self.frames += frames;
        Ok(())
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        self.writer.flush().map_err(to_io)
    }

    fn finish(self: Box<Self>) -> io::Result<AudioFileInfo> {
        let Self {
            writer,
            relative_path,
            format,
            frames,
        } = *self;
        writer.finalize().map_err(to_io)?;
        Ok(AudioFileInfo {
            relative_path,
            sample_rate: format.sample_rate,
            channels: format.channels,
            frames,
        })
    }
}

/// Reads what a WAV file holds, based on its header (so a crashed recording reports the
/// state of its last checkpoint). `Ok(None)` when the file does not exist.
pub fn probe_wav(path: &Path, relative_path: &str) -> io::Result<Option<AudioFileInfo>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let reader = WavReader::open(path).map_err(to_io)?;
    let spec = reader.spec();
    Ok(Some(AudioFileInfo {
        relative_path: relative_path.to_owned(),
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        frames: u64::from(reader.duration()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO_48K: AudioFormat = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };

    fn writer(dir: &Path, name: &str, format: AudioFormat) -> WavTrackWriter {
        WavTrackWriter::create(&dir.join(name), format!("audio/{name}"), format).unwrap()
    }

    #[test]
    fn samples_round_trip_as_16_bit_pcm() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = writer(tmp.path(), "a.wav", STEREO_48K);
        w.write(&[0.0, 0.5, -0.5, 1.0, -1.0, 2.0]).unwrap(); // 3 frames, last clamped
        let info = Box::new(w).finish().unwrap();
        assert_eq!(info.frames, 3);
        assert_eq!(info.relative_path, "audio/a.wav");

        let mut reader = WavReader::open(tmp.path().join("a.wav")).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 16);
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 48_000);
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples, [0, 16_384, -16_384, 32_767, -32_767, 32_767]);
    }

    #[test]
    fn silence_is_whole_frames_of_zeros() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = writer(tmp.path(), "s.wav", STEREO_48K);
        w.write_silence(10_000).unwrap(); // spans several silence blocks
        w.write(&[0.5, 0.5]).unwrap();
        let info = Box::new(w).finish().unwrap();
        assert_eq!(info.frames, 10_001);
        assert_eq!(info.duration_ms(), 208);

        let reader = WavReader::open(tmp.path().join("s.wav")).unwrap();
        assert_eq!(reader.duration(), 10_001);
        let samples: Vec<i16> = reader.into_samples::<i16>().map(Result::unwrap).collect();
        assert!(samples[..20_000].iter().all(|s| *s == 0));
        assert_eq!(samples[20_000], 16_384);
    }

    #[test]
    fn a_trailing_partial_frame_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = writer(tmp.path(), "p.wav", STEREO_48K);
        w.write(&[0.1, 0.1, 0.1]).unwrap(); // 1.5 frames
        assert_eq!(Box::new(w).finish().unwrap().frames, 1);
        assert!(WavReader::open(tmp.path().join("p.wav")).is_ok());
    }

    #[test]
    fn a_checkpoint_keeps_the_file_readable_without_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let mono = AudioFormat {
            sample_rate: 16_000,
            channels: 1,
        };
        let mut w = writer(tmp.path(), "crash.wav", mono);
        w.write(&vec![0.25; 16_000]).unwrap(); // 1 s
        w.checkpoint().unwrap();
        w.write(&vec![0.25; 8_000]).unwrap(); // 0.5 s after the checkpoint

        // What a crash would leave: copy the file while the writer is still open and never
        // finalised.
        let crashed = tmp.path().join("crashed-copy.wav");
        std::fs::copy(tmp.path().join("crash.wav"), &crashed).unwrap();
        let reader = WavReader::open(&crashed).unwrap();
        assert_eq!(reader.duration(), 16_000, "everything up to the checkpoint");
        let info = probe_wav(&crashed, "audio/crash.wav").unwrap().unwrap();
        assert_eq!(info.frames, 16_000);
        assert_eq!(info.sample_rate, 16_000);

        let finished = Box::new(w).finish().unwrap();
        assert_eq!(finished.frames, 24_000);
    }

    #[test]
    fn probing_a_missing_file_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(probe_wav(&tmp.path().join("nope.wav"), "audio/nope.wav")
            .unwrap()
            .is_none());
    }

    #[test]
    fn probing_garbage_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.wav");
        std::fs::write(&path, b"not a wav file at all").unwrap();
        assert!(probe_wav(&path, "audio/bad.wav").is_err());
    }

    #[test]
    fn a_degenerate_format_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = AudioFormat {
            sample_rate: 48_000,
            channels: 0,
        };
        assert!(WavTrackWriter::create(&tmp.path().join("x.wav"), "x", bad).is_err());
    }

    #[test]
    fn writes_past_the_wav_size_limit_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = writer(tmp.path(), "big.wav", STEREO_48K);
        // 2^31 frames of stereo 16-bit would be 8 GiB: refused before anything is written.
        let err = w.write_silence(1 << 31).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert_eq!(Box::new(w).finish().unwrap().frames, 0);
    }
}
