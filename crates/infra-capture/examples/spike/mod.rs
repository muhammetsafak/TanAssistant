//! Shared building blocks of the Phase 02 capture spike.
//!
//! Everything here is throw-away spike code (see `.ssot/tasarim/yakalama-spike.md`): it is
//! not copied into `src/`. What matters is what it taught us about each platform.
//!
//! Data flow, identical on every platform:
//!
//! ```text
//! audio callback (OS thread) --Chunk--> mpsc --> writer thread --> float32 WAV
//! frame thread (every 3 s)   --------------------------------> frame-<ms>.png
//! ```
//!
//! The audio callbacks only convert to `f32` and send; all file I/O happens on a writer
//! thread. Allocating a `Vec` per callback is acceptable for a spike but is not what the
//! production adapter should do (use a pre-sized ring buffer there).

// Spike code: pedantic lints add noise without value here. `clippy::all` still applies.
#![allow(clippy::pedantic)]

use std::error::Error;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfig};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("the capture spike only supports Windows, macOS and Linux");

pub type BoxError = Box<dyn Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, BoxError>;

/// Interval between two screen captures (ADR-009 says 2-5 s).
pub const FRAME_INTERVAL: Duration = Duration::from_secs(3);

/// Spike run configuration, filled from the command line.
pub struct Config {
    pub seconds: u64,
    pub out_dir: PathBuf,
}

/// Result of one finished audio stream.
pub struct AudioReport {
    pub label: &'static str,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames (one sample per channel) actually written to the WAV file.
    pub frames: u64,
    /// Offset of the first callback from the shared start instant.
    pub first_chunk_ms: Option<u128>,
    /// Offset of the end of the last chunk from the shared start instant.
    pub last_chunk_end_ms: Option<u128>,
    /// Number of callback-to-callback gaps longer than 250 ms (silence is not delivered by
    /// some backends, so the file is shorter than wall-clock time).
    pub gaps_over_250ms: u64,
    /// Peak absolute sample value, to tell a silent-but-valid file from a live one.
    pub peak: f32,
    pub path: PathBuf,
}

/// Result of the screen frame capture.
pub struct FramesReport {
    pub source: String,
    pub frames: u64,
    pub first_ms: Option<u128>,
    pub last_ms: Option<u128>,
    pub slowest_capture_ms: u128,
    pub total_capture_ms: u128,
    pub width: u32,
    pub height: u32,
}

pub enum Report {
    Audio(AudioReport),
    Frames(FramesReport),
    /// Free-form note a platform wants to surface (permission hints, fallbacks...).
    Note(String),
}

/// A running capture that can be stopped, yielding its reports.
pub trait Recording {
    fn stop(self: Box<Self>) -> Result<Vec<Report>>;
}

// ---------------------------------------------------------------------------------------
// WAV writer thread
// ---------------------------------------------------------------------------------------

pub struct Chunk {
    pub at: Instant,
    pub samples: Vec<f32>,
}

/// Owns the writer thread of one WAV file; audio callbacks feed it through [`Self::sender`].
pub struct WavRecorder {
    tx: Sender<Option<Chunk>>,
    handle: JoinHandle<Result<AudioReport>>,
}

impl WavRecorder {
    pub fn start(
        label: &'static str,
        path: &Path,
        device: String,
        sample_rate: u32,
        channels: u16,
        clock: Instant,
    ) -> Result<Self> {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::new(BufWriter::new(File::create(path)?), spec)?;
        let (tx, rx): (Sender<Option<Chunk>>, Receiver<Option<Chunk>>) = mpsc::channel();
        let path = path.to_path_buf();

        let handle = std::thread::Builder::new()
            .name(format!("wav-{label}"))
            .spawn(move || -> Result<AudioReport> {
                let mut frames = 0_u64;
                let mut first: Option<Instant> = None;
                let mut last_end: Option<Instant> = None;
                let mut gaps = 0_u64;
                let mut peak = 0_f32;
                let mut prev_end: Option<Instant> = None;

                while let Ok(Some(chunk)) = rx.recv() {
                    let chunk_frames = (chunk.samples.len() / usize::from(channels)) as u64;
                    let chunk_len =
                        Duration::from_secs_f64(chunk_frames as f64 / f64::from(sample_rate));
                    // `at` is taken when the callback runs, i.e. when the data was
                    // delivered; the data covers the chunk_len before that instant.
                    let start = chunk.at.checked_sub(chunk_len).unwrap_or(chunk.at);
                    first.get_or_insert(start);
                    if let Some(prev) = prev_end {
                        if start.saturating_duration_since(prev) > Duration::from_millis(250) {
                            gaps += 1;
                        }
                    }
                    prev_end = Some(chunk.at);
                    last_end = Some(chunk.at);
                    for sample in &chunk.samples {
                        peak = peak.max(sample.abs());
                        writer.write_sample(*sample)?;
                    }
                    frames += chunk_frames;
                }
                writer.finalize()?;

                let offset = |at: Instant| at.saturating_duration_since(clock).as_millis();
                Ok(AudioReport {
                    label,
                    device,
                    sample_rate,
                    channels,
                    frames,
                    first_chunk_ms: first.map(offset),
                    last_chunk_end_ms: last_end.map(offset),
                    gaps_over_250ms: gaps,
                    peak,
                    path,
                })
            })?;

        Ok(Self { tx, handle })
    }

    pub fn sender(&self) -> Sender<Option<Chunk>> {
        self.tx.clone()
    }

    /// Flushes and closes the file. Safe to call while callbacks are still alive: late
    /// chunks are simply dropped by the closed channel.
    pub fn finish(self) -> Result<AudioReport> {
        let _ = self.tx.send(None);
        self.handle
            .join()
            .map_err(|_| "wav writer thread panicked")?
    }
}

/// Prefix of a [`Report::Note`] that records a failure (the run then exits non-zero).
pub const FAILURE_PREFIX: &str = "FAILED: ";

/// Several recordings stopped together. A part that fails to start or to stop never
/// discards the others: its error becomes a `FAILED:` note next to the reports that did
/// succeed, so e.g. a broken screen capture cannot hide whether the system audio worked.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub struct Combined {
    recordings: Vec<Box<dyn Recording>>,
    notes: Vec<String>,
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl Combined {
    pub fn new(recordings: Vec<Box<dyn Recording>>) -> Self {
        Self {
            recordings,
            notes: Vec::new(),
        }
    }

    /// Adds an informational note that is printed in the summary.
    pub fn with_note(mut self, note: String) -> Self {
        self.notes.push(note);
        self
    }

    /// Starts the xcap frame capture; a start-up failure is kept as a failure note instead
    /// of tearing down the audio that is already running.
    pub fn with_frames(mut self, out_dir: &Path, clock: Instant) -> Self {
        match XcapFrames::start(out_dir, clock) {
            Ok(frames) => self.recordings.push(Box::new(frames)),
            Err(err) => self.notes.push(format!(
                "{FAILURE_PREFIX}screen capture could not start: {err}"
            )),
        }
        self
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl Recording for Combined {
    fn stop(self: Box<Self>) -> Result<Vec<Report>> {
        let Self { recordings, notes } = *self;
        let mut reports: Vec<Report> = notes.into_iter().map(Report::Note).collect();
        for recording in recordings {
            match recording.stop() {
                Ok(mut r) => reports.append(&mut r),
                Err(err) => reports.push(Report::Note(format!(
                    "{FAILURE_PREFIX}a capture failed while stopping: {err}"
                ))),
            }
        }
        Ok(reports)
    }
}

// ---------------------------------------------------------------------------------------
// cpal input helper (microphone on every OS, loopback on Windows, monitor on Linux)
// ---------------------------------------------------------------------------------------

pub struct CpalInput {
    stream: cpal::Stream,
    recorder: WavRecorder,
}

impl CpalInput {
    /// Opens `device` as an *input* stream. On WASAPI an output device opened this way
    /// transparently becomes a loopback capture of that output.
    pub fn open(
        label: &'static str,
        device: &cpal::Device,
        path: &Path,
        clock: Instant,
    ) -> Result<Self> {
        let config = if device.supports_input() {
            device.default_input_config()?
        } else {
            device.default_output_config()?
        };
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "<unknown device>".to_owned());
        let recorder = WavRecorder::start(
            label,
            path,
            format!("{name} [{:?}]", config.sample_format()),
            config.sample_rate(),
            config.channels(),
            clock,
        )?;
        let stream = build_stream(device, &config, recorder.sender())?;
        stream.play()?;
        Ok(Self { stream, recorder })
    }
}

impl Recording for CpalInput {
    fn stop(self: Box<Self>) -> Result<Vec<Report>> {
        let Self { stream, recorder } = *self;
        drop(stream);
        Ok(vec![Report::Audio(recorder.finish()?)])
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    tx: Sender<Option<Chunk>>,
) -> Result<cpal::Stream> {
    let stream_config: StreamConfig = (*config).into();
    match config.sample_format() {
        SampleFormat::F32 => build_typed::<f32>(device, stream_config, tx),
        SampleFormat::I16 => build_typed::<i16>(device, stream_config, tx),
        SampleFormat::I32 => build_typed::<i32>(device, stream_config, tx),
        other => Err(format!("unsupported sample format {other:?}").into()),
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    tx: Sender<Option<Chunk>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            let samples = data.iter().map(|s| f32::from_sample(*s)).collect();
            let _ = tx.send(Some(Chunk {
                at: Instant::now(),
                samples,
            }));
        },
        |err| eprintln!("  [cpal stream error] {err}"),
        None,
    )?;
    Ok(stream)
}

// ---------------------------------------------------------------------------------------
// Screen frames through xcap (Windows, Linux X11/Wayland)
// ---------------------------------------------------------------------------------------

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub struct XcapFrames {
    stop: Sender<()>,
    handle: JoinHandle<Result<FramesReport>>,
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl XcapFrames {
    pub fn start(out_dir: &Path, clock: Instant) -> Result<Self> {
        let out_dir = out_dir.to_path_buf();
        let (stop, stop_rx) = mpsc::channel::<()>();
        // `xcap::Monitor` is not `Send` on Windows, so it is created on the capture thread
        // and the start-up result is handed back through this channel.
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("frames".to_owned())
            .spawn(move || -> Result<FramesReport> {
                let monitor = match select_monitor() {
                    Ok(monitor) => monitor,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err.to_string()));
                        return Err(err);
                    }
                };
                let _ = ready_tx.send(Ok(()));
                let mut report = FramesReport {
                    source: format!(
                        "xcap monitor '{}' {}x{} @ scale {}",
                        monitor.friendly_name().unwrap_or_default(),
                        monitor.width().unwrap_or(0),
                        monitor.height().unwrap_or(0),
                        monitor.scale_factor().unwrap_or(1.0)
                    ),
                    frames: 0,
                    first_ms: None,
                    last_ms: None,
                    slowest_capture_ms: 0,
                    total_capture_ms: 0,
                    width: 0,
                    height: 0,
                };
                let mut tick = 0_u32;
                loop {
                    let t0 = Instant::now();
                    let image = monitor.capture_image()?;
                    let took = t0.elapsed().as_millis();
                    let ms = clock.elapsed().as_millis();
                    image.save(out_dir.join(format!("frame-{ms}.png")))?;
                    report.frames += 1;
                    report.first_ms.get_or_insert(ms);
                    report.last_ms = Some(ms);
                    report.slowest_capture_ms = report.slowest_capture_ms.max(took);
                    report.total_capture_ms += took;
                    report.width = image.width();
                    report.height = image.height();
                    // Wait for the next absolute tick (capture + PNG encoding take ~1 s on
                    // 1080p and must not stretch the interval), or leave on a stop request.
                    tick += 1;
                    let next = clock + FRAME_INTERVAL * tick;
                    if !matches!(
                        stop_rx.recv_timeout(next.saturating_duration_since(Instant::now())),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        break;
                    }
                }
                Ok(report)
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle }),
            Ok(Err(message)) => Err(message.into()),
            Err(_) => Err("frame thread exited before it was ready".into()),
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn select_monitor() -> Result<xcap::Monitor> {
    let mut chosen = None;
    for monitor in xcap::Monitor::all()? {
        if monitor.is_primary().unwrap_or(false) {
            return Ok(monitor);
        }
        chosen.get_or_insert(monitor);
    }
    chosen.ok_or_else(|| "xcap found no monitor".into())
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl Recording for XcapFrames {
    fn stop(self: Box<Self>) -> Result<Vec<Report>> {
        let _ = self.stop.send(());
        let report = self.handle.join().map_err(|_| "frame thread panicked")??;
        Ok(vec![Report::Frames(report)])
    }
}

// ---------------------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------------------

/// Runs the whole spike: system audio + screen (platform specific), microphone (cpal),
/// then prints a summary. Returns `Ok(false)` if any stream failed to start or finish.
pub fn run(config: &Config) -> Result<bool> {
    std::fs::create_dir_all(&config.out_dir)?;
    let clock = Instant::now();
    let mut ok = true;
    let mut running: Vec<(&str, Box<dyn Recording>)> = Vec::new();

    println!("platform: {}", std::env::consts::OS);
    println!("output:   {}", config.out_dir.display());
    println!("duration: {} s", config.seconds);

    match platform::start_system(&config.out_dir, clock) {
        Ok(recording) => running.push(("system audio + screen", recording)),
        Err(err) => {
            ok = false;
            eprintln!("FAILED system audio + screen: {err}");
        }
    }

    match start_microphone(&config.out_dir, clock) {
        Ok(recording) => running.push(("microphone", recording)),
        Err(err) => {
            ok = false;
            eprintln!("FAILED microphone: {err}");
        }
    }

    if running.is_empty() {
        return Ok(false);
    }

    println!("recording...");
    std::thread::sleep(Duration::from_secs(config.seconds));

    let mut reports = Vec::new();
    for (name, recording) in running {
        match recording.stop() {
            Ok(mut r) => reports.append(&mut r),
            Err(err) => {
                ok = false;
                eprintln!("FAILED stopping {name}: {err}");
            }
        }
    }
    let wall_ms = clock.elapsed().as_millis();
    print_summary(&reports, wall_ms);
    if reports
        .iter()
        .any(|r| matches!(r, Report::Note(n) if n.starts_with(FAILURE_PREFIX)))
    {
        ok = false;
    }
    Ok(ok)
}

fn start_microphone(out_dir: &Path, clock: Instant) -> Result<Box<dyn Recording>> {
    use cpal::traits::HostTrait;
    let host = platform::microphone_host()?;
    let device = host
        .default_input_device()
        .ok_or("no default input device (microphone)")?;
    let input = CpalInput::open("mic", &device, &out_dir.join("mic.wav"), clock)?;
    Ok(Box::new(input))
}

fn print_summary(reports: &[Report], wall_ms: u128) {
    println!("\n=== summary (wall clock since start: {wall_ms} ms) ===");
    for report in reports {
        match report {
            Report::Audio(a) => {
                let secs = a.frames as f64 / f64::from(a.sample_rate);
                println!(
                    "[{}] {}\n    {} Hz, {} ch, float32 | {} frames = {:.3} s | first chunk @ {} ms, last chunk end @ {} ms | gaps>250ms: {} | peak {:.4}\n    file: {}",
                    a.label,
                    a.device,
                    a.sample_rate,
                    a.channels,
                    a.frames,
                    secs,
                    a.first_chunk_ms.map_or("-".to_owned(), |v| v.to_string()),
                    a.last_chunk_end_ms.map_or("-".to_owned(), |v| v.to_string()),
                    a.gaps_over_250ms,
                    a.peak,
                    a.path.display()
                );
            }
            Report::Frames(f) => println!(
                "[frames] {}\n    {} frames {}x{} | first @ {} ms, last @ {} ms | capture time: avg {} ms, slowest {} ms",
                f.source,
                f.frames,
                f.width,
                f.height,
                f.first_ms.map_or("-".to_owned(), |v| v.to_string()),
                f.last_ms.map_or("-".to_owned(), |v| v.to_string()),
                f.total_capture_ms / u128::from(f.frames.max(1)),
                f.slowest_capture_ms
            ),
            Report::Note(n) => println!("[note] {n}"),
        }
    }
}
