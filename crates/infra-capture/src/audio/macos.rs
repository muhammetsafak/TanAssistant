//! macOS 13+: the microphone through cpal (`CoreAudio`), system audio through an audio-only
//! `ScreenCaptureKit` stream (`screencapturekit` crate).
//!
//! cpal 0.18's `CoreAudio` loopback needs macOS 14.6+ and `ScreenCaptureKit`'s own microphone
//! output needs macOS 15; `ScreenCaptureKit` *system audio* works from macOS 13, which is the
//! floor in ADR-009, so that is what is used. The screen frames come from a second stream in
//! `screen::macos`.
//!
//! STATUS: compiled by CI only; it has not been run on a Mac yet. See
//! `.ssot/tasarim/yakalama-spike.md` for the permissions (TCC): "Screen & System Audio
//! Recording" is granted to the app that calls this code (Terminal during `cargo run`, Tan.app
//! in production); the microphone needs `NSMicrophoneUsageDescription` in `Info.plist`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use screencapturekit::prelude::*;
use tan_application::ports::{
    AudioFormat, AudioSink, AudioStream, CaptureError, CaptureResult, PcmChunk,
};

/// `ScreenCaptureKit` rate and layout we ask for; the real format of every buffer is read from
/// its description and passed on, so a deviation is harmless.
const REQUESTED_SAMPLE_RATE: i32 = 48_000;
const REQUESTED_CHANNELS: i32 = 2;

/// The microphone goes through the default (`CoreAudio`) host.
#[allow(clippy::unnecessary_wraps)] // same signature as the other platforms
pub(super) fn microphone_host() -> CaptureResult<cpal::Host> {
    Ok(cpal::default_host())
}

pub(super) fn start_system(sink: Arc<dyn AudioSink>) -> CaptureResult<Box<dyn AudioStream>> {
    // First call that needs the Screen & System Audio Recording permission.
    let content = SCShareableContent::get().map_err(|e| {
        CaptureError::permission_denied(format!(
            "ScreenCaptureKit denied or failed: {e}. Allow 'Screen & System Audio Recording' \
             for this app in System Settings > Privacy & Security and restart it"
        ))
    })?;
    let display =
        content.displays().into_iter().next().ok_or_else(|| {
            CaptureError::device_not_found("ScreenCaptureKit reported no display")
        })?;

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()
        .map_err(|e| CaptureError::failed(format!("cannot build the content filter: {e}")))?;

    // The stream needs a video configuration even though only audio is used: keep it tiny and
    // slow so it costs next to nothing.
    let config = SCStreamConfiguration::new()
        .with_width(128)
        .with_height(128)
        .with_minimum_frame_interval(&CMTime::new(1, 1))
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(REQUESTED_SAMPLE_RATE)
        .with_channel_count(REQUESTED_CHANNELS);

    let mut stream = SCStream::new(&filter, &config)
        .map_err(|e| CaptureError::failed(format!("cannot create the audio stream: {e}")))?;
    stream
        .add_output_handler(
            AudioHandler {
                sink,
                reported_format: AtomicBool::new(false),
            },
            SCStreamOutputType::Audio,
        )
        .map_err(|e| CaptureError::failed(format!("cannot register the audio handler: {e}")))?;
    stream
        .start_capture()
        .map_err(|e| CaptureError::failed(format!("cannot start the audio capture: {e}")))?;

    Ok(Box::new(SckAudioStream {
        stream: Some(stream),
        format: AudioFormat {
            sample_rate: REQUESTED_SAMPLE_RATE.unsigned_abs(),
            channels: u16::try_from(REQUESTED_CHANNELS).unwrap_or(2),
        },
    }))
}

struct SckAudioStream {
    stream: Option<SCStream>,
    format: AudioFormat,
}

impl SckAudioStream {
    fn shutdown(&mut self) {
        if let Some(stream) = self.stream.take() {
            // Stopping can fail if the stream already died; nothing is left to clean up then.
            let _ = stream.stop_capture();
        }
    }
}

impl AudioStream for SckAudioStream {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn device_name(&self) -> &'static str {
        "System audio (ScreenCaptureKit)"
    }

    fn stop(mut self: Box<Self>) {
        self.shutdown();
    }
}

impl Drop for SckAudioStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct AudioHandler {
    sink: Arc<dyn AudioSink>,
    /// Set after the first buffer of an unusable format was reported, to report it once.
    reported_format: AtomicBool,
}

impl AudioHandler {
    fn report_once(&self, message: &str) {
        if !self.reported_format.swap(true, Ordering::Relaxed) {
            self.sink.on_error(message.to_owned());
        }
    }
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Audio) {
            return;
        }
        let Some(description) = sample.format_description() else {
            return;
        };
        let channels = description.audio_channel_count().unwrap_or(0);
        let float32 =
            description.audio_is_float() && description.audio_bits_per_channel() == Some(32);
        let sample_rate = rate_to_u32(description.audio_sample_rate().unwrap_or(0.0));
        let Ok(channels) = u16::try_from(channels) else {
            return;
        };
        if !float32 || channels == 0 || sample_rate == 0 {
            self.report_once(&format!(
                "unexpected ScreenCaptureKit audio format ({sample_rate} Hz, {channels} ch, \
                 float32={float32}); this audio is skipped"
            ));
            return;
        }

        let Ok(list) = sample.audio_buffer_list() else {
            return;
        };
        let planes: Vec<&[u8]> = list
            .iter()
            .map(screencapturekit::AudioBuffer::data)
            .collect();
        let pcm = if planes.len() == usize::from(channels) && planes.len() > 1 {
            interleave_planar(&planes)
        } else if planes.len() == 1 {
            // Already interleaved.
            planes[0]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect()
        } else {
            self.report_once("unexpected ScreenCaptureKit audio buffer layout; audio is skipped");
            return;
        };

        self.sink.on_chunk(PcmChunk {
            format: AudioFormat {
                sample_rate,
                channels,
            },
            captured_at: Instant::now(),
            samples: pcm,
        });
    }
}

/// `ScreenCaptureKit` usually delivers one buffer per channel (non-interleaved): weave them
/// into interleaved frames.
fn interleave_planar(planes: &[&[u8]]) -> Vec<f32> {
    let frames = planes
        .iter()
        .map(|channel| channel.len() / 4)
        .min()
        .unwrap_or(0);
    let mut out = Vec::with_capacity(frames * planes.len());
    for frame in 0..frames {
        for channel in planes {
            let bytes = &channel[frame * 4..frame * 4 + 4];
            out.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
    }
    out
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rounded and range-checked
fn rate_to_u32(rate: f64) -> u32 {
    if (1.0..=f64::from(u32::MAX)).contains(&rate) {
        rate.round() as u32
    } else {
        0
    }
}
