//! macOS 13+: frames from a video-only `ScreenCaptureKit` stream (`screencapturekit` crate).
//!
//! STATUS: compiled by CI only; it has not been run on a Mac yet.
//!
//! `ScreenCaptureKit` is push based and only delivers a frame when the screen content changed,
//! at most once per `minimum_frame_interval`. A static screen therefore yields *no* frames
//! (the recording service's keep-alive does not apply there; transcript matching uses the
//! frame timestamps, so this is fine). The stream captures at the display's size in points
//! (half the physical pixels on Retina).
//!
//! Permission: "Screen & System Audio Recording", granted to the calling app (the same
//! permission as the system audio stream in `audio::macos`).

// Reading the pixel memory of a locked CVPixelBuffer is `unsafe` in the binding. It is kept
// in the one small function `read_rgba` below.
#![allow(unsafe_code)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use screencapturekit::prelude::*;
use tan_application::ports::{
    CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
    ScreenStreamRequest,
};

/// How long [`capture_once`] waits for the first frame.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

fn shareable_content() -> CaptureResult<SCShareableContent> {
    SCShareableContent::get().map_err(|e| {
        CaptureError::permission_denied(format!(
            "ScreenCaptureKit denied or failed: {e}. Allow 'Screen & System Audio Recording' \
             for this app in System Settings > Privacy & Security and restart it"
        ))
    })
}

pub(super) fn list_displays() -> CaptureResult<Vec<DisplayInfo>> {
    let displays = shareable_content()?.displays();
    Ok(displays
        .iter()
        .enumerate()
        .map(|(index, display)| DisplayInfo {
            id: display.display_id().to_string(),
            name: format!(
                "Display {} ({}x{})",
                index + 1,
                display.width(),
                display.height()
            ),
            width: display.width(),
            height: display.height(),
            // ScreenCaptureKit does not flag the main display; it lists it first.
            is_primary: index == 0,
        })
        .collect())
}

/// A single frame: `ScreenCaptureKit` (macOS 13) has no screenshot call, so a stream is opened
/// and closed around the first frame it delivers.
pub(super) fn capture_once(display_id: Option<&str>) -> CaptureResult<RawFrame> {
    struct FirstFrame(Mutex<Option<mpsc::Sender<RawFrame>>>);

    impl FrameSink for FirstFrame {
        fn on_frame(&self, frame: RawFrame) {
            if let Ok(mut slot) = self.0.lock() {
                if let Some(sender) = slot.take() {
                    let _ = sender.send(frame);
                }
            }
        }

        fn on_error(&self, _message: String) {}
    }

    let (sender, receiver) = mpsc::channel();
    let request = ScreenStreamRequest {
        display_id: display_id.map(str::to_owned),
        interval: Duration::from_secs(1),
    };
    let stream = start(&request, Arc::new(FirstFrame(Mutex::new(Some(sender)))))?;
    let frame = receiver.recv_timeout(FIRST_FRAME_TIMEOUT);
    stream.stop();
    frame.map_err(|_| CaptureError::failed("the screen delivered no frame in time"))
}

pub(super) fn start(
    request: &ScreenStreamRequest,
    sink: Arc<dyn FrameSink>,
) -> CaptureResult<Box<dyn ScreenStream>> {
    let displays = shareable_content()?.displays();
    let display = match request.display_id.as_deref() {
        Some(id) => displays
            .iter()
            .find(|d| {
                id.parse::<u32>()
                    .is_ok_and(|wanted| d.display_id() == wanted)
            })
            .ok_or_else(|| {
                CaptureError::device_not_found("the selected display is not available")
            })?,
        None => displays.first().ok_or_else(|| {
            CaptureError::device_not_found("ScreenCaptureKit reported no display")
        })?,
    };

    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build()
        .map_err(|e| CaptureError::failed(format!("cannot build the content filter: {e}")))?;

    let interval_ms = i64::try_from(request.interval.as_millis()).unwrap_or(i64::MAX);
    let config = SCStreamConfiguration::new()
        .with_width(display.width())
        .with_height(display.height())
        .with_pixel_format(PixelFormat::BGRA)
        // Mouse movement alone must not count as a screen change.
        .with_shows_cursor(false)
        // Only emitted when the screen changed, at most once per interval.
        .with_minimum_frame_interval(&CMTime::new(interval_ms, 1000));

    let mut stream = SCStream::new(&filter, &config)
        .map_err(|e| CaptureError::failed(format!("cannot create the screen stream: {e}")))?;
    stream
        .add_output_handler(ScreenHandler { sink }, SCStreamOutputType::Screen)
        .map_err(|e| CaptureError::failed(format!("cannot register the screen handler: {e}")))?;
    stream
        .start_capture()
        .map_err(|e| CaptureError::failed(format!("cannot start the screen capture: {e}")))?;
    Ok(Box::new(SckScreenStream {
        stream: Some(stream),
    }))
}

struct SckScreenStream {
    stream: Option<SCStream>,
}

impl SckScreenStream {
    fn shutdown(&mut self) {
        if let Some(stream) = self.stream.take() {
            // Stopping can fail if the stream already died; nothing is left to clean up then.
            let _ = stream.stop_capture();
        }
    }
}

impl ScreenStream for SckScreenStream {
    fn stop(mut self: Box<Self>) {
        self.shutdown();
    }
}

impl Drop for SckScreenStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ScreenHandler {
    sink: Arc<dyn FrameSink>,
}

impl SCStreamOutputTrait for ScreenHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Screen) {
            return;
        }
        let Some((width, height, rgba)) = read_rgba(&sample) else {
            return;
        };
        self.sink.on_frame(RawFrame {
            captured_at: Instant::now(),
            width,
            height,
            rgba,
        });
    }
}

/// Copies the pixels of a sample into a tightly packed RGBA buffer. `None` for status-only
/// buffers (idle/blank notifications carry no pixels) and for malformed ones.
fn read_rgba(sample: &CMSampleBuffer) -> Option<(u32, u32, Vec<u8>)> {
    let pixel_buffer = sample.pixel_buffer()?;
    let guard = pixel_buffer.lock_read_only().ok()?;
    let (width, height, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
    // SAFETY: the buffer stays locked (read-only) while `guard` lives and the slice is only
    // used inside this function.
    let bytes = unsafe { guard.as_slice() }?;
    if height == 0 || bytes.len() < (height - 1) * stride + width * 4 {
        return None;
    }
    // BGRA (pinned in the configuration) -> RGBA, honouring the row stride.
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let line = &bytes[row * stride..row * stride + width * 4];
        for pixel in line.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
    }
    Some((
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
        rgba,
    ))
}
