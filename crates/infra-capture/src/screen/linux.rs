//! Linux: frames through `xcap` on X11 and through a `ScreenCast` portal session on Wayland.
//!
//! STATUS: compiled by CI only; it has not been run on a Linux desktop yet.
//!
//! * **X11:** xcap reads the root window through xcb; no permission is involved.
//! * **Wayland:** a compositor does not let applications read the screen, and xcap's only
//!   route there (the screenshot portal) asks or flashes on every capture. So Wayland sessions
//!   use the `linux_portal` module: one `ScreenCast` portal session per recording, where the user
//!   picks the source in a system dialog once (the portal remembers it through a restore
//!   token) and frames come from a `PipeWire` stream. When the portal is missing or the user
//!   declines, the recording carries on with audio only (see `availability` in the parent
//!   module and the error kinds returned by `start`).

use std::sync::Arc;

use tan_application::ports::{
    CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
    ScreenStreamRequest,
};

use super::{linux_portal, xcap_backend};

/// Whether this process runs in a Wayland session.
fn on_wayland() -> bool {
    super::is_wayland_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

fn map_error(context: &str, error: &xcap::XCapError) -> CaptureError {
    CaptureError::failed(format!("{context}: {error}"))
}

/// Whether the `ScreenCast` portal can serve this session (asked without any dialog).
pub(super) fn screencast_usable() -> bool {
    linux_portal::screencast_usable()
}

pub(super) fn list_displays() -> CaptureResult<Vec<DisplayInfo>> {
    if on_wayland() {
        return Ok(linux_portal::list_displays());
    }
    xcap_backend::list_displays(map_error)
}

pub(super) fn capture_once(display_id: Option<&str>) -> CaptureResult<RawFrame> {
    if on_wayland() {
        return linux_portal::capture_once();
    }
    xcap_backend::capture_once(display_id, map_error)
}

pub(super) fn start(
    request: &ScreenStreamRequest,
    sink: Arc<dyn FrameSink>,
) -> CaptureResult<Box<dyn ScreenStream>> {
    if on_wayland() {
        return linux_portal::start(request, sink);
    }
    xcap_backend::start(request, sink, map_error)
}
