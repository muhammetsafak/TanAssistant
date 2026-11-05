//! Windows: frames through `xcap` with Windows Graphics Capture (the `wgc` feature, ADR-009).
//!
//! Measured in the spike on a 1080p display: about 170-230 ms per capture. Known limits:
//! protected (DRM) content can come out black, and `xcap::Monitor` is not `Send`, which the
//! shared polling backend handles by creating the monitor on the capture thread.

use std::sync::Arc;

use tan_application::ports::{
    CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
    ScreenStreamRequest,
};

use super::xcap_backend;

fn map_error(context: &str, error: &xcap::XCapError) -> CaptureError {
    CaptureError::failed(format!(
        "{context}: {error} (protected content and the secure desktop cannot be captured)"
    ))
}

pub(super) fn list_displays() -> CaptureResult<Vec<DisplayInfo>> {
    xcap_backend::list_displays(map_error)
}

pub(super) fn capture_once(display_id: Option<&str>) -> CaptureResult<RawFrame> {
    xcap_backend::capture_once(display_id, map_error)
}

pub(super) fn start(
    request: &ScreenStreamRequest,
    sink: Arc<dyn FrameSink>,
) -> CaptureResult<Box<dyn ScreenStream>> {
    xcap_backend::start(request, sink, map_error)
}
