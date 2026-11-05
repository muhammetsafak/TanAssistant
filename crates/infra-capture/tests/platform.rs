//! Tests of the real platform adapters. They need a desktop session (a display, an audio
//! device), so they are `#[ignore]`d by default; run them on a development machine with
//!
//! ```text
//! cargo test -p tan-infra-capture --test platform -- --ignored --nocapture
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tan_application::ports::{
    AudioCapture, AudioSink, AudioStreamRequest, CaptureErrorKind, FrameHasher, FrameSink,
    PcmChunk, RawFrame, ScreenCapture, ScreenStreamRequest,
};
use tan_domain::TrackKind;
use tan_infra_capture::{DHasher, PlatformAudioCapture, PlatformScreenCapture};

#[derive(Default)]
struct CountingAudio {
    chunks: AtomicU64,
    frames: AtomicU64,
    errors: Mutex<Vec<String>>,
    peak_bits: AtomicU64,
}

impl AudioSink for CountingAudio {
    fn on_chunk(&self, chunk: PcmChunk) {
        self.chunks.fetch_add(1, Ordering::Relaxed);
        self.frames
            .fetch_add(chunk.frame_count(), Ordering::Relaxed);
        let peak = chunk.samples.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        let _ = self
            .peak_bits
            .fetch_max(u64::from(peak.to_bits()), Ordering::Relaxed);
    }

    fn on_error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
}

#[derive(Default)]
struct CollectingFrames(Mutex<Vec<RawFrame>>);

impl FrameSink for CollectingFrames {
    fn on_frame(&self, frame: RawFrame) {
        self.0.lock().unwrap().push(frame);
    }

    fn on_error(&self, message: String) {
        panic!("screen capture reported an error: {message}");
    }
}

#[test]
#[ignore = "needs a desktop session with a display"]
fn displays_are_listed_and_a_frame_can_be_captured() {
    let screen = PlatformScreenCapture;
    let displays = screen.list_displays().unwrap();
    assert!(!displays.is_empty(), "no display found");
    assert!(displays.iter().any(|d| d.is_primary), "{displays:?}");
    println!("displays: {displays:?}");

    let frame = screen.capture_once(None).unwrap();
    assert!(frame.width > 0 && frame.height > 0);
    assert_eq!(
        frame.rgba.len() as u64,
        u64::from(frame.width) * u64::from(frame.height) * 4
    );
    println!(
        "frame {}x{}, hash {:016x}",
        frame.width,
        frame.height,
        DHasher.hash(&frame)
    );

    let bad = screen.capture_once(Some("999999999")).unwrap_err();
    assert_eq!(bad.kind, CaptureErrorKind::DeviceNotFound);
}

#[test]
#[ignore = "needs a desktop session with a display"]
fn the_screen_stream_ticks_on_schedule_and_stops() {
    let screen = PlatformScreenCapture;
    let sink = Arc::new(CollectingFrames::default());
    let stream = screen
        .start(
            &ScreenStreamRequest {
                display_id: None,
                interval: Duration::from_millis(500),
            },
            sink.clone(),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(2_300));
    stream.stop();

    let frames = sink.0.lock().unwrap();
    // Immediate capture plus ticks at 0.5, 1.0, 1.5, 2.0 s: 5 frames, allow one either way.
    assert!((4..=6).contains(&frames.len()), "{} frames", frames.len());
    let spacing: Vec<_> = frames
        .windows(2)
        .map(|w| {
            w[1].captured_at
                .duration_since(w[0].captured_at)
                .as_millis()
        })
        .collect();
    println!("spacing between frames (ms): {spacing:?}");
    let hashes: Vec<_> = frames.iter().map(|f| DHasher.hash(f)).collect();
    let distances: Vec<_> = hashes
        .windows(2)
        .map(|w| (w[0] ^ w[1]).count_ones())
        .collect();
    println!("hash distance between consecutive frames: {distances:?}");
    drop(frames);
    let after = Instant::now();
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        sink.0.lock().unwrap().iter().all(|f| f.captured_at < after),
        "frames kept arriving after stop()"
    );
}

#[test]
#[ignore = "needs an audio output (and ideally an input) device"]
fn system_audio_and_microphone_deliver_pcm() {
    let audio = PlatformAudioCapture;
    println!("input devices: {:?}", audio.list_input_devices());

    let system = Arc::new(CountingAudio::default());
    let system_stream = audio
        .start(
            &AudioStreamRequest {
                source: TrackKind::System,
                device_id: None,
            },
            system.clone(),
        )
        .unwrap();
    println!(
        "system audio: {} {:?}",
        system_stream.device_name(),
        system_stream.format()
    );

    let mic = Arc::new(CountingAudio::default());
    let mic_stream = audio.start(
        &AudioStreamRequest {
            source: TrackKind::Microphone,
            device_id: None,
        },
        mic.clone(),
    );
    match &mic_stream {
        Ok(s) => println!("microphone: {} {:?}", s.device_name(), s.format()),
        Err(e) => println!("microphone unavailable: {e}"),
    }

    std::thread::sleep(Duration::from_secs(2));
    let system_format = system_stream.format();
    system_stream.stop();
    if let Ok(stream) = mic_stream {
        let format = stream.format();
        stream.stop();
        let frames = mic.frames.load(Ordering::Relaxed);
        println!("microphone: {frames} frames in about 2 s");
        assert!(
            frames > u64::from(format.sample_rate),
            "microphone too quiet to be alive"
        );
    }

    // The silent render stream (Windows) keeps loopback packets flowing even when nothing
    // plays, so about two seconds of audio must arrive.
    let frames = system.frames.load(Ordering::Relaxed);
    println!(
        "system: {frames} frames in about 2 s, errors {:?}",
        system.errors.lock().unwrap()
    );
    assert!(
        frames > u64::from(system_format.sample_rate),
        "system audio delivered {frames} frames"
    );
}
