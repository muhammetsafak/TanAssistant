//! Hand-run test of the Wayland screen capture (`ScreenCast` portal + `PipeWire`).
//!
//! It needs a Wayland desktop session and a person at the keyboard: the first session shows the
//! portal's source dialog (pick a screen or a window and share it), the second must not show
//! any dialog because the portal restores the choice from the saved restore token. Run it with
//!
//! ```text
//! TAN_PORTAL_SECONDS=30 cargo test -p tan-infra-capture --test wayland_portal -- --ignored --nocapture
//! ```
//!
//! and change what is on the shared screen (switch windows, scroll, play a video) while it
//! records. The test keeps its data directory (and so the restore token) in a temporary folder,
//! so it never touches the real application's saved permission.
#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tan_application::ports::{
    FrameHasher, FrameSink, RawFrame, ScreenAvailability, ScreenCapture, ScreenStreamRequest,
};
use tan_infra_capture::{DHasher, PlatformScreenCapture};

/// Name of the file the adapter keeps the restore token in, inside the data directory.
const TOKEN_FILE: &str = "screencast-restore-token";

#[derive(Default)]
struct Collector {
    frames: Mutex<Vec<RawFrame>>,
    problems: Mutex<Vec<String>>,
}

impl FrameSink for Collector {
    fn on_frame(&self, frame: RawFrame) {
        self.frames.lock().unwrap().push(frame);
    }

    fn on_error(&self, message: String) {
        println!("  sink problem: {message}");
        self.problems.lock().unwrap().push(message);
    }
}

/// Records for `seconds` with a 2 s interval and returns what arrived; prints a summary.
fn record(screen: PlatformScreenCapture, label: &str, seconds: u64) -> Vec<RawFrame> {
    let sink = Arc::new(Collector::default());
    let asked_at = Instant::now();
    let stream = screen
        .start(
            &ScreenStreamRequest {
                display_id: None,
                interval: Duration::from_secs(2),
            },
            sink.clone(),
        )
        .unwrap_or_else(|e| panic!("{label}: the stream did not start: {e}"));
    println!(
        "{label}: stream started after {} ms (a dialog the user has to answer takes seconds)",
        asked_at.elapsed().as_millis()
    );
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(seconds));
    stream.stop();

    let frames = std::mem::take(&mut *sink.frames.lock().unwrap());
    println!("{label}: {} frames in {seconds} s", frames.len());
    let spacing: Vec<_> = frames
        .windows(2)
        .map(|pair| {
            pair[1]
                .captured_at
                .duration_since(pair[0].captured_at)
                .as_millis()
        })
        .collect();
    println!("{label}: spacing between frames (ms): {spacing:?}");
    let hashes: Vec<_> = frames.iter().map(|f| DHasher.hash(f)).collect();
    let distances: Vec<_> = hashes
        .windows(2)
        .map(|pair| (pair[0] ^ pair[1]).count_ones())
        .collect();
    println!("{label}: hash distance between consecutive frames: {distances:?}");
    if let Some(first) = frames.first() {
        println!(
            "{label}: first frame {}x{} after {} ms",
            first.width,
            first.height,
            first.captured_at.duration_since(started).as_millis()
        );
    }
    frames
}

fn assert_frames_are_sound(label: &str, frames: &[RawFrame]) {
    assert!(!frames.is_empty(), "{label}: no frame arrived");
    for frame in frames {
        assert!(frame.width > 0 && frame.height > 0, "{label}: empty frame");
        assert_eq!(
            frame.rgba.len() as u64,
            u64::from(frame.width) * u64::from(frame.height) * 4,
            "{label}: buffer does not match the size"
        );
    }
    assert!(
        frames.iter().any(|f| f
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[..3] != [0, 0, 0])),
        "{label}: every frame is black"
    );
}

#[test]
#[ignore = "needs a Wayland desktop session and a person to answer the source dialog"]
fn a_wayland_session_delivers_frames_and_remembers_the_choice() {
    let seconds = std::env::var("TAN_PORTAL_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let data_dir = tempfile::tempdir().unwrap();
    std::env::set_var("TAN_DATA_DIR", data_dir.path());

    let screen = PlatformScreenCapture;
    assert_eq!(
        screen.availability(),
        ScreenAvailability::Available,
        "no usable ScreenCast portal (is xdg-desktop-portal and your desktop's backend running?)"
    );
    let displays = screen.list_displays().unwrap();
    println!("displays offered: {displays:?}");
    assert_eq!(
        displays.len(),
        1,
        "Wayland lists the portal as the one display"
    );

    println!("\n>>> SESSION 1: the source dialog must appear now. Pick a screen and share it.");
    let first = record(screen, "session 1", seconds);
    assert_frames_are_sound("session 1", &first);
    let token = data_dir.path().join(TOKEN_FILE);
    assert!(
        token.is_file(),
        "the portal issued no restore token (portal older than interface version 4?)"
    );
    println!("restore token saved: {}", token.display());

    println!("\n>>> SESSION 2: NO dialog may appear; frames must start on their own.");
    let second = record(screen, "session 2", seconds);
    assert_frames_are_sound("session 2", &second);

    println!("\n>>> a single frame (capture_once), also without a dialog:");
    let single = screen.capture_once(None).unwrap();
    println!("capture_once: {}x{}", single.width, single.height);
    assert!(single.width > 0 && single.height > 0);
}
