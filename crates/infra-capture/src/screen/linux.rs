//! Linux: frames through `xcap`.
//!
//! STATUS: compiled by CI only; it has not been run on a Linux desktop yet.
//!
//! * **X11:** xcap reads the root window through xcb; no permission is involved.
//! * **Wayland: screen capture is off by default** (user decision, Phase 08): the recording
//!   service skips the screen when `availability()` reports `WaylandUnsupported` and records
//!   audio only. What follows describes why, and what the real fix needs.
//!   xcap asks GNOME Shell's screenshot D-Bus API, then the
//!   `org.freedesktop.portal.Screenshot` portal, then (wlroots compositors) `libwayshot`.
//!   Newer GNOME versions restrict the Shell call, and the portal route may show a permission
//!   dialog or a flash on every capture. The proper long-term path for periodic frames on
//!   Wayland is a one-time `ScreenCast` portal session with a `PipeWire` stream (`ashpd`); the
//!   spike recommended it and it is **not** implemented here, because it cannot be built or
//!   tried without a Wayland machine. Failures are reported through the sink as warnings and
//!   the recording continues with audio only.

use std::sync::Arc;

use tan_application::ports::{
    CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
    ScreenStreamRequest,
};

use super::xcap_backend;

fn map_error(context: &str, error: &xcap::XCapError) -> CaptureError {
    let wayland =
        std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"));
    let hint = if wayland {
        " (Wayland session: the desktop must allow screenshots through the portal; an X11 \
         session needs no permission)"
    } else {
        ""
    };
    CaptureError::failed(format!("{context}: {error}{hint}"))
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
