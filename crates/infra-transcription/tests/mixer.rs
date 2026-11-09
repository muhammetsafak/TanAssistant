//! Mixing the tracks of a session into one WAV file.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use tan_application::ports::{AudioMixError, AudioMixer};
use tan_infra_transcription::WavMixer;

fn spec(channels: u16, rate: u32) -> WavSpec {
    WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    }
}

/// Writes `seconds` of audio; `f(time_in_seconds, channel)` gives each sample in `-1.0..=1.0`.
fn write_track(
    dir: &Path,
    name: &str,
    channels: u16,
    rate: u32,
    seconds: f32,
    f: impl Fn(f32, u16) -> f32,
) -> PathBuf {
    let path = dir.join(name);
    let mut writer = WavWriter::create(&path, spec(channels, rate)).unwrap();
    let frames = (seconds * rate as f32) as usize;
    for frame in 0..frames {
        for channel in 0..channels {
            let value = f(frame as f32 / rate as f32, channel);
            writer.write_sample((value * 32_767.0) as i16).unwrap();
        }
    }
    writer.finalize().unwrap();
    path
}

fn sine(hz: f32, amplitude: f32) -> impl Fn(f32, u16) -> f32 {
    move |time, _| (2.0 * PI * hz * time).sin() * amplitude
}

fn read(path: &Path) -> (WavSpec, Vec<f32>) {
    let mut reader = WavReader::open(path).unwrap();
    let spec = reader.spec();
    let samples = reader
        .samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32_767.0)
        .collect();
    (spec, samples)
}

fn peak(samples: &[f32], rate: u32, from_s: f32, to_s: f32) -> f32 {
    let from = (from_s * rate as f32) as usize;
    let to = ((to_s * rate as f32) as usize).min(samples.len());
    samples[from..to]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn two_tracks_of_different_rate_and_channels_become_one_mono_file_at_the_higher_rate() {
    let dir = tempfile::tempdir().unwrap();
    let system = write_track(dir.path(), "system.wav", 2, 48_000, 3.0, sine(440.0, 0.4));
    let microphone = write_track(
        dir.path(),
        "microphone.wav",
        1,
        44_100,
        2.0,
        sine(1_000.0, 0.2),
    );
    let out = dir.path().join("mixed.wav");

    WavMixer.mix_to_wav(&[microphone, system], &out).unwrap();

    let (spec, samples) = read(&out);
    assert_eq!((spec.channels, spec.sample_rate), (1, 48_000));
    assert_eq!(spec.bits_per_sample, 16);
    // As long as the longer track: 3 s at 48 kHz.
    assert!(
        (samples.len() as i64 - 144_000).abs() <= 2,
        "{} frames",
        samples.len()
    );
    // Both tracks are heard while both play, only the longer one afterwards.
    let both = peak(&samples, 48_000, 0.5, 1.9);
    let one = peak(&samples, 48_000, 2.2, 2.9);
    assert!(both > 0.5, "both: {both}");
    assert!((0.35..0.45).contains(&one), "one: {one}");
    // Nothing is left beside the result.
    assert_eq!(
        files_in(dir.path()),
        ["microphone.wav", "mixed.wav", "system.wav"]
    );
}

#[test]
fn a_single_track_keeps_its_own_rate_and_is_downmixed_to_mono() {
    let dir = tempfile::tempdir().unwrap();
    let track = write_track(dir.path(), "mic.wav", 2, 44_100, 1.5, |time, channel| {
        // Left carries a tone, right is its opposite: the mono average cancels.
        let tone = (2.0 * PI * 300.0 * time).sin() * 0.5;
        if channel == 0 {
            tone
        } else {
            -tone
        }
    });
    let out = dir.path().join("out.wav");

    WavMixer.mix_to_wav(&[track], &out).unwrap();

    let (spec, samples) = read(&out);
    assert_eq!((spec.channels, spec.sample_rate), (1, 44_100));
    assert_eq!(samples.len(), 66_150);
    assert!(samples.iter().all(|s| s.abs() < 1e-3));
}

#[test]
fn the_shorter_track_is_completed_with_silence() {
    let dir = tempfile::tempdir().unwrap();
    let short = write_track(dir.path(), "short.wav", 1, 16_000, 1.0, |_, _| 0.2);
    let long = write_track(dir.path(), "long.wav", 1, 16_000, 3.0, |_, _| 0.1);
    let out = dir.path().join("out.wav");

    WavMixer.mix_to_wav(&[short, long], &out).unwrap();

    let (spec, samples) = read(&out);
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(samples.len(), 48_000);
    assert!((samples[8_000] - 0.3).abs() < 1e-3, "{}", samples[8_000]);
    assert!((samples[32_000] - 0.1).abs() < 1e-3, "{}", samples[32_000]);
    assert!((samples[47_999] - 0.1).abs() < 1e-3);
}

#[test]
fn a_sum_beyond_full_scale_is_clamped_not_wrapped() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_track(dir.path(), "a.wav", 1, 16_000, 0.5, |_, _| 0.9);
    let b = write_track(dir.path(), "b.wav", 1, 16_000, 0.5, |_, _| 0.9);
    let out = dir.path().join("loud.wav");
    WavMixer.mix_to_wav(&[a, b], &out).unwrap();
    let mut reader = WavReader::open(&out).unwrap();
    assert!(reader.samples::<i16>().all(|s| s.unwrap() == i16::MAX));

    let a = write_track(dir.path(), "c.wav", 1, 16_000, 0.5, |_, _| -0.9);
    let b = write_track(dir.path(), "d.wav", 1, 16_000, 0.5, |_, _| -0.9);
    let out = dir.path().join("loud-negative.wav");
    WavMixer.mix_to_wav(&[a, b], &out).unwrap();
    let mut reader = WavReader::open(&out).unwrap();
    assert!(reader.samples::<i16>().all(|s| s.unwrap() == -i16::MAX));
}

#[test]
fn a_long_resampled_track_has_no_seams_between_windows() {
    // 12 s is three read windows and many mixing chunks; a resampler that lost its state at a
    // boundary would leave a jump there.
    let dir = tempfile::tempdir().unwrap();
    let track = write_track(dir.path(), "long.wav", 1, 44_100, 12.0, sine(440.0, 0.5));
    let other = write_track(dir.path(), "other.wav", 1, 48_000, 1.0, |_, _| 0.0);
    let out = dir.path().join("out.wav");

    WavMixer.mix_to_wav(&[track, other], &out).unwrap();

    let (spec, samples) = read(&out);
    assert_eq!(spec.sample_rate, 48_000);
    assert!(
        (samples.len() as i64 - 576_000).abs() <= 2,
        "{} frames",
        samples.len()
    );
    // 440 Hz at 48 kHz and amplitude 0.5 moves by at most 0.029 per sample.
    let largest_step = samples
        .windows(2)
        .skip(2_000) // the filter's own start-up
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(largest_step < 0.04, "largest step {largest_step}");
    let crossings = samples
        .windows(2)
        .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
        .count();
    assert!(
        (5_270..=5_290).contains(&crossings),
        "{crossings} crossings"
    );
}

#[test]
fn an_existing_destination_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let track = write_track(dir.path(), "t.wav", 1, 16_000, 0.5, |_, _| 0.1);
    let out = dir.path().join("out.wav");
    std::fs::write(&out, b"old").unwrap();

    WavMixer.mix_to_wav(&[track], &out).unwrap();

    assert_eq!(read(&out).1.len(), 8_000);
}

#[test]
fn failures_are_typed_and_leave_no_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let good = write_track(dir.path(), "good.wav", 1, 16_000, 0.5, |_, _| 0.1);
    let out = dir.path().join("out.wav");

    // No track at all.
    assert!(matches!(
        WavMixer.mix_to_wav(&[], &out),
        Err(AudioMixError::NoTracks)
    ));

    // A track that is missing, and one that is not a WAV file.
    let missing = dir.path().join("missing.wav");
    assert!(matches!(
        WavMixer.mix_to_wav(&[good.clone(), missing], &out),
        Err(AudioMixError::Source(_))
    ));
    let garbage = dir.path().join("garbage.wav");
    std::fs::write(&garbage, b"this is not a wav file").unwrap();
    assert!(matches!(
        WavMixer.mix_to_wav(&[good.clone(), garbage], &out),
        Err(AudioMixError::Source(_))
    ));

    // A track without any audio.
    let empty = write_track(dir.path(), "empty.wav", 1, 16_000, 0.0, |_, _| 0.0);
    assert!(matches!(
        WavMixer.mix_to_wav(&[empty], &out),
        Err(AudioMixError::Empty)
    ));

    // A destination whose folder does not exist.
    let nowhere = dir.path().join("no-such-folder").join("out.wav");
    assert!(matches!(
        WavMixer.mix_to_wav(&[good], &nowhere),
        Err(AudioMixError::Destination(_))
    ));

    assert_eq!(
        files_in(dir.path()),
        ["empty.wav", "garbage.wav", "good.wav"]
    );
}
