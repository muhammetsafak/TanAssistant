//! macOS 13+: system audio and screen frames from ONE ScreenCaptureKit stream
//! (`screencapturekit` crate). The microphone is recorded by cpal (CoreAudio) in `mod.rs`,
//! because SCK's own microphone output needs macOS 15.
//!
//! STATUS: compiled by CI only. This code has never been run (no macOS machine in the
//! Phase 02 environment); see `.ssot/tasarim/yakalama-spike.md` for the run instructions.
//!
//! Permissions (TCC):
//! * `SCShareableContent::get()` is the first call that needs "Screen & System Audio
//!   Recording" for the *host application* (the terminal when run with `cargo run`).
//! * cpal's input stream triggers the Microphone prompt for the same host application.

// Reading the pixel memory of a locked CVPixelBuffer is `unsafe` in the binding. Spike only;
// the production adapter must keep this behind a small, reviewed module.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use screencapturekit::prelude::*;

use super::{Chunk, FramesReport, Recording, Report, Result, WavRecorder, FRAME_INTERVAL};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// The microphone goes through the default (CoreAudio) host.
pub fn microphone_host() -> Result<cpal::Host> {
    Ok(cpal::default_host())
}

pub fn start_system(out_dir: &Path, clock: Instant) -> Result<Box<dyn Recording>> {
    // First call that needs the Screen Recording permission.
    let content = SCShareableContent::get().map_err(|e| {
        format!(
            "ScreenCaptureKit denied or failed: {e}. Grant 'Screen & System Audio Recording' \
             to your terminal in System Settings > Privacy & Security and restart it"
        )
    })?;
    let display = content
        .displays()
        .into_iter()
        .next()
        .ok_or("ScreenCaptureKit reported no display")?;
    let (width, height) = (display.width(), display.height());

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()
        .map_err(|e| format!("cannot build content filter: {e}"))?;

    let config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        // SCK only emits a frame when the screen changed, at most once per interval.
        .with_minimum_frame_interval(&CMTime::new(FRAME_INTERVAL.as_secs() as i64, 1))
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(SAMPLE_RATE as i32)
        .with_channel_count(i32::from(CHANNELS));

    let recorder = WavRecorder::start(
        "system",
        &out_dir.join("system.wav"),
        format!("ScreenCaptureKit system audio [F32, {SAMPLE_RATE} Hz requested]"),
        SAMPLE_RATE,
        CHANNELS,
        clock,
    )?;

    let sink = Arc::new(FrameSink {
        out_dir: out_dir.to_path_buf(),
        clock,
        state: Mutex::new(FrameState {
            frames: 0,
            first_ms: None,
            last_ms: None,
            slowest_ms: 0,
            total_ms: 0,
            width: 0,
            height: 0,
        }),
    });
    let audio_note = Arc::new(Mutex::new(None::<String>));

    let mut stream =
        SCStream::new(&filter, &config).map_err(|e| format!("cannot create SCStream: {e}"))?;
    stream
        .add_output_handler(
            ScreenHandler {
                sink: Arc::clone(&sink),
            },
            SCStreamOutputType::Screen,
        )
        .map_err(|e| format!("cannot register screen handler: {e}"))?;
    stream
        .add_output_handler(
            AudioHandler {
                tx: recorder.sender(),
                note: Arc::clone(&audio_note),
            },
            SCStreamOutputType::Audio,
        )
        .map_err(|e| format!("cannot register audio handler: {e}"))?;
    stream
        .start_capture()
        .map_err(|e| format!("cannot start capture: {e}"))?;

    Ok(Box::new(SckRecording {
        stream,
        recorder,
        sink,
        audio_note,
        source: format!("ScreenCaptureKit display {width}x{height} (points)"),
    }))
}

struct SckRecording {
    stream: SCStream,
    recorder: WavRecorder,
    sink: Arc<FrameSink>,
    audio_note: Arc<Mutex<Option<String>>>,
    source: String,
}

impl Recording for SckRecording {
    fn stop(self: Box<Self>) -> Result<Vec<Report>> {
        let Self {
            stream,
            recorder,
            sink,
            audio_note,
            source,
        } = *self;
        stream
            .stop_capture()
            .map_err(|e| format!("cannot stop capture: {e}"))?;
        drop(stream);

        let mut reports = vec![Report::Audio(recorder.finish()?)];
        if let Some(note) = audio_note.lock().map_err(|_| "poisoned note")?.take() {
            reports.push(Report::Note(note));
        }
        let state = sink.state.lock().map_err(|_| "poisoned frame state")?;
        reports.push(Report::Frames(FramesReport {
            source,
            frames: state.frames,
            first_ms: state.first_ms,
            last_ms: state.last_ms,
            slowest_capture_ms: state.slowest_ms,
            total_capture_ms: state.total_ms,
            width: state.width,
            height: state.height,
        }));
        reports.push(Report::Note(
            "ScreenCaptureKit only delivers a frame when the screen content changed; a static \
             screen yields fewer than one frame per interval"
                .to_owned(),
        ));
        Ok(reports)
    }
}

// ---------------------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------------------

struct AudioHandler {
    tx: Sender<Option<Chunk>>,
    note: Arc<Mutex<Option<String>>>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Audio) {
            return;
        }
        let Some(format) = sample.format_description() else {
            return;
        };
        let rate = format.audio_sample_rate().unwrap_or(0.0);
        let channels = format.audio_channel_count().unwrap_or(0);
        let float32 = format.audio_is_float() && format.audio_bits_per_channel() == Some(32);
        if let Ok(mut note) = self.note.lock() {
            note.get_or_insert_with(|| {
                format!(
                    "first SCK audio buffer: {rate} Hz, {channels} ch, float32={float32} \
                     (WAV header assumes {SAMPLE_RATE} Hz, {CHANNELS} ch)"
                )
            });
        }
        if !float32 || channels != u32::from(CHANNELS) {
            return; // would corrupt the WAV; the note above tells why the file stays empty
        }
        let Ok(list) = sample.audio_buffer_list() else {
            return;
        };
        let planes: Vec<&[u8]> = list.iter().map(|buffer| buffer.data()).collect();
        let samples: Vec<f32> = if planes.len() == usize::from(CHANNELS) {
            // Non-interleaved: one buffer per channel; interleave for the WAV file.
            let frames = planes.iter().map(|p| p.len() / 4).min().unwrap_or(0);
            let mut out = Vec::with_capacity(frames * planes.len());
            for i in 0..frames {
                for plane in &planes {
                    let b = &plane[i * 4..i * 4 + 4];
                    out.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                }
            }
            out
        } else if planes.len() == 1 {
            // Already interleaved.
            planes[0]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        } else {
            return;
        };
        let _ = self.tx.send(Some(Chunk {
            at: Instant::now(),
            samples,
        }));
    }
}

// ---------------------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------------------

struct FrameState {
    frames: u64,
    first_ms: Option<u128>,
    last_ms: Option<u128>,
    slowest_ms: u128,
    total_ms: u128,
    width: u32,
    height: u32,
}

struct FrameSink {
    out_dir: PathBuf,
    clock: Instant,
    state: Mutex<FrameState>,
}

struct ScreenHandler {
    sink: Arc<FrameSink>,
}

impl SCStreamOutputTrait for ScreenHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Screen) {
            return;
        }
        // Idle / blank status buffers carry no pixel buffer.
        let Some(pixel_buffer) = sample.pixel_buffer() else {
            return;
        };
        let t0 = Instant::now();
        let Ok(guard) = pixel_buffer.lock_read_only() else {
            return;
        };
        let (width, height, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
        // SAFETY: the buffer stays locked (read-only) while `guard` lives and the slice is
        // only used inside this function.
        let Some(bytes) = (unsafe { guard.as_slice() }) else {
            return;
        };
        if height == 0 || bytes.len() < (height - 1) * stride + width * 4 {
            return;
        }
        // BGRA (pinned in the configuration) -> RGBA, honouring the row stride.
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            let line = &bytes[row * stride..row * stride + width * 4];
            for px in line.chunks_exact(4) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        drop(guard);
        let Some(image) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) else {
            return;
        };
        let took = t0.elapsed().as_millis();
        let ms = self.sink.clock.elapsed().as_millis();
        if image
            .save(self.sink.out_dir.join(format!("frame-{ms}.png")))
            .is_err()
        {
            return;
        }
        if let Ok(mut state) = self.sink.state.lock() {
            state.frames += 1;
            state.first_ms.get_or_insert(ms);
            state.last_ms = Some(ms);
            state.slowest_ms = state.slowest_ms.max(took);
            state.total_ms += took;
            state.width = width as u32;
            state.height = height as u32;
        }
    }
}
