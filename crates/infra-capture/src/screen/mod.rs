//! Screen capture adapters.
//!
//! | Platform | Backend |
//! |---|---|
//! | Windows | `xcap` with Windows Graphics Capture, polled on an absolute schedule |
//! | macOS 13+ | `ScreenCaptureKit` video-only stream (pushes a frame when the screen changes) |
//! | Linux, X11 | `xcap` through xcb, polled on an absolute schedule |
//! | Linux, Wayland | `ScreenCast` portal session + `PipeWire` stream (the user picks the source once; the portal remembers it) |
//!
//! Polling backends tick against an absolute schedule (`start + n * interval`): "capture,
//! sleep one interval" would stretch the cadence by the capture time (about a second for a
//! 1080p frame including PNG encoding, which the spike measured), and the PNG encoding is not
//! done here at all but only for the frames the selector keeps.

use std::sync::Arc;

use tan_application::ports::{
    CaptureErrorKind, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenAvailability,
    ScreenCapture, ScreenStream, ScreenStreamRequest, ScreenUnavailableReason, ThumbnailOutcome,
    ThumbnailUnavailableReason, THUMBNAIL_MAX_EDGE,
};

// Pure helpers of the Wayland adapter (pixel conversion, restore token file, probe gate). They need no
// Linux libraries, so they are also built for the tests of the other platforms.
#[cfg(any(target_os = "linux", test))]
mod frame_convert;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_portal;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", test))]
mod probe_gate;
#[cfg(any(target_os = "linux", test))]
mod restore_token;
mod thumbnail;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("tan-infra-capture supports Windows, macOS and Linux only");

/// Screen capture for the platform this crate is compiled for.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformScreenCapture;

/// Whether this is a Wayland session: `XDG_SESSION_TYPE` says `wayland`, or a
/// `WAYLAND_DISPLAY` is set (either is enough). Pure so it can be tested on any OS.
#[must_use]
pub fn is_wayland_session(session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    session_type.is_some_and(|t| t.trim().eq_ignore_ascii_case("wayland"))
        || wayland_display.is_some_and(|d| !d.trim().is_empty())
}

/// Screen availability from the platform and the session environment.
///
/// On Linux/Wayland the screen is captured through a `ScreenCast` portal session (Phase 13;
/// before it, Wayland recorded audio only by user decision, Phase 08). That works when the
/// portal can serve the session, which `screencast_usable` reports; it is only called for a
/// Wayland session on Linux, because asking the portal costs a D-Bus round trip. When it says
/// no (no portal, no monitor or window source), the screen is off and only audio is recorded
/// ([`ScreenUnavailableReason::WaylandUnsupported`]). The user declining the source dialog is
/// not known here (the dialog appears when the recording starts); it surfaces as a
/// `PermissionDenied` error from `start`, and the recording carries on with audio only.
///
/// X11, Windows and macOS are unaffected.
#[must_use]
pub fn availability_for(
    is_linux: bool,
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    screencast_usable: impl FnOnce() -> bool,
) -> ScreenAvailability {
    if is_linux && is_wayland_session(session_type, wayland_display) && !screencast_usable() {
        ScreenAvailability::Unavailable(ScreenUnavailableReason::WaylandUnsupported)
    } else {
        ScreenAvailability::Available
    }
}

/// Whether the `ScreenCast` portal can serve a session. Only ever asked on Linux.
fn screencast_usable() -> bool {
    #[cfg(target_os = "linux")]
    {
        platform::screencast_usable()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Why a display preview must not even be attempted in this session, if any.
///
/// A Wayland session on Linux is always refused, whether or not the `ScreenCast` portal could
/// serve it: the only way to a frame there is the portal's source dialog, and a preview in a
/// selection list must never open one. Pure so it can be tested on any OS.
#[must_use]
fn thumbnail_gate(
    is_linux: bool,
    session_type: Option<&str>,
    wayland_display: Option<&str>,
) -> Option<ThumbnailUnavailableReason> {
    (is_linux && is_wayland_session(session_type, wayland_display))
        .then_some(ThumbnailUnavailableReason::WaylandUnsupported)
}

/// Maps a failed capture to the reason the UI gets.
fn thumbnail_reason(kind: CaptureErrorKind) -> ThumbnailUnavailableReason {
    match kind {
        CaptureErrorKind::PermissionDenied => ThumbnailUnavailableReason::PermissionDenied,
        CaptureErrorKind::DeviceNotFound => ThumbnailUnavailableReason::DisplayNotFound,
        CaptureErrorKind::Unsupported => ThumbnailUnavailableReason::Unsupported,
        CaptureErrorKind::Failed => ThumbnailUnavailableReason::Failed,
    }
}

/// Turns the outcome of the platform capture into the thumbnail result.
fn thumbnail_outcome(captured: CaptureResult<RawFrame>, max_edge: u32) -> ThumbnailOutcome {
    // Only error texts are logged; they never carry screen content.
    match captured {
        Ok(frame) => thumbnail::encode(frame, max_edge).map_or_else(
            || {
                tracing::warn!("a display preview could not be scaled or encoded");
                ThumbnailOutcome::Unavailable(ThumbnailUnavailableReason::Failed)
            },
            ThumbnailOutcome::Image,
        ),
        Err(error) => {
            if matches!(
                error.kind,
                CaptureErrorKind::Failed | CaptureErrorKind::Unsupported
            ) {
                tracing::warn!(kind = ?error.kind, message = %error.message, "display preview capture failed");
            }
            ThumbnailOutcome::Unavailable(thumbnail_reason(error.kind))
        }
    }
}

impl ScreenCapture for PlatformScreenCapture {
    fn availability(&self) -> ScreenAvailability {
        availability_for(
            cfg!(target_os = "linux"),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
            screencast_usable,
        )
    }

    fn list_displays(&self) -> CaptureResult<Vec<DisplayInfo>> {
        platform::list_displays()
    }

    fn capture_once(&self, display_id: Option<&str>) -> CaptureResult<RawFrame> {
        platform::capture_once(display_id)
    }

    fn start(
        &self,
        request: &ScreenStreamRequest,
        sink: Arc<dyn FrameSink>,
    ) -> CaptureResult<Box<dyn ScreenStream>> {
        platform::start(request, sink)
    }

    fn display_thumbnail(&self, display_id: &str) -> ThumbnailOutcome {
        if let Some(reason) = thumbnail_gate(
            cfg!(target_os = "linux"),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        ) {
            return ThumbnailOutcome::Unavailable(reason);
        }
        thumbnail_outcome(
            platform::capture_for_thumbnail(display_id, THUMBNAIL_MAX_EDGE),
            THUMBNAIL_MAX_EDGE,
        )
    }
}

/// The `xcap` based backend shared by Windows and Linux. Each platform supplies only an
/// error mapper that adds platform specific hints.
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod xcap_backend {
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use tan_application::ports::{
        CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
        ScreenStreamRequest,
    };

    /// Turns an `xcap` failure into a [`CaptureError`].
    pub(super) type ErrorMapper = fn(&str, &xcap::XCapError) -> CaptureError;

    /// Shortest interval the poller accepts (the settings enforce 500 ms already).
    const MIN_INTERVAL: Duration = Duration::from_millis(100);

    fn all_monitors(map: ErrorMapper) -> CaptureResult<Vec<xcap::Monitor>> {
        xcap::Monitor::all().map_err(|e| map("cannot list the displays", &e))
    }

    /// The monitor with the given id, or the primary one (first one as a last resort).
    ///
    /// `xcap::Monitor` is not `Send` on Windows (it wraps an `HMONITOR`), so this is called on
    /// the thread that uses the monitor.
    fn pick_monitor(display_id: Option<&str>, map: ErrorMapper) -> CaptureResult<xcap::Monitor> {
        let wanted = match display_id {
            Some(text) => Some(text.parse::<u32>().map_err(|_| {
                CaptureError::device_not_found("the selected display id is not valid")
            })?),
            None => None,
        };
        let mut first = None;
        for monitor in all_monitors(map)? {
            match wanted {
                Some(id) if monitor.id().ok() == Some(id) => return Ok(monitor),
                Some(_) => {}
                None if monitor.is_primary().unwrap_or(false) => return Ok(monitor),
                None => {
                    first.get_or_insert(monitor);
                }
            }
        }
        match (wanted, first) {
            (None, Some(monitor)) => Ok(monitor),
            (Some(_), _) => Err(CaptureError::device_not_found(
                "the selected display is not available",
            )),
            (None, None) => Err(CaptureError::device_not_found("no display was found")),
        }
    }

    pub(super) fn list_displays(map: ErrorMapper) -> CaptureResult<Vec<DisplayInfo>> {
        let mut displays = Vec::new();
        for (index, monitor) in all_monitors(map)?.into_iter().enumerate() {
            let (Ok(id), Ok(width), Ok(height)) = (monitor.id(), monitor.width(), monitor.height())
            else {
                continue;
            };
            let is_primary = monitor.is_primary().unwrap_or(false);
            // `friendly_name` is unreliable ("Unknown Monitor ..." in the spike), so the
            // list shows a number and the resolution instead.
            let name = format!(
                "Display {} ({width}x{height}){}",
                index + 1,
                if is_primary { ", primary" } else { "" }
            );
            displays.push(DisplayInfo {
                id: id.to_string(),
                name,
                width,
                height,
                is_primary,
            });
        }
        Ok(displays)
    }

    fn grab(monitor: &xcap::Monitor, map: ErrorMapper) -> CaptureResult<RawFrame> {
        let image = monitor
            .capture_image()
            .map_err(|e| map("cannot capture the screen", &e))?;
        let (width, height) = (image.width(), image.height());
        Ok(RawFrame {
            captured_at: Instant::now(),
            width,
            height,
            rgba: image.into_raw(),
        })
    }

    pub(super) fn capture_once(
        display_id: Option<&str>,
        map: ErrorMapper,
    ) -> CaptureResult<RawFrame> {
        grab(&pick_monitor(display_id, map)?, map)
    }

    /// A polling thread that is stopped by dropping `stop`.
    struct PollingStream {
        stop: Option<Sender<()>>,
        handle: Option<JoinHandle<()>>,
    }

    impl PollingStream {
        fn shutdown(&mut self) {
            drop(self.stop.take());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    impl ScreenStream for PollingStream {
        fn stop(mut self: Box<Self>) {
            self.shutdown();
        }
    }

    impl Drop for PollingStream {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    pub(super) fn start(
        request: &ScreenStreamRequest,
        sink: Arc<dyn FrameSink>,
        map: ErrorMapper,
    ) -> CaptureResult<Box<dyn ScreenStream>> {
        let display_id = request.display_id.clone();
        let interval = request.interval.max(MIN_INTERVAL);
        let (stop, stopped) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<CaptureResult<()>>();

        let handle = std::thread::Builder::new()
            .name("tan-screen".to_owned())
            .spawn(move || {
                let monitor = match pick_monitor(display_id.as_deref(), map) {
                    Ok(monitor) => monitor,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                poll(&monitor, &*sink, interval, &stopped, map);
            })
            .map_err(|e| CaptureError::failed(format!("cannot start the screen thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(PollingStream {
                stop: Some(stop),
                handle: Some(handle),
            })),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => Err(CaptureError::failed(
                "the screen thread ended before it was ready",
            )),
        }
    }

    /// Captures immediately, then once per `interval` on an absolute schedule until `stopped`
    /// is disconnected or signalled.
    fn poll(
        monitor: &xcap::Monitor,
        sink: &dyn FrameSink,
        interval: Duration,
        stopped: &mpsc::Receiver<()>,
        map: ErrorMapper,
    ) {
        let origin = Instant::now();
        let mut tick = 0_u32;
        loop {
            match grab(monitor, map) {
                Ok(frame) => sink.on_frame(frame),
                Err(error) => sink.on_error(error.to_string()),
            }
            // Next tick strictly in the future: a capture that overran its slot skips the
            // missed ticks instead of firing them back to back.
            let now = Instant::now();
            let next = loop {
                tick = tick.saturating_add(1);
                let Some(next) = origin.checked_add(interval.saturating_mul(tick)) else {
                    return;
                };
                if next > now {
                    break next;
                }
            };
            if !matches!(
                stopped.recv_timeout(next.saturating_duration_since(Instant::now())),
                Err(RecvTimeoutError::Timeout)
            ) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tan_application::ports::CaptureError;

    use super::*;

    const WAYLAND_OFF: ScreenAvailability =
        ScreenAvailability::Unavailable(ScreenUnavailableReason::WaylandUnsupported);

    #[test]
    fn wayland_is_detected_from_either_variable() {
        assert!(is_wayland_session(Some("wayland"), None));
        assert!(is_wayland_session(Some("Wayland"), None));
        assert!(is_wayland_session(None, Some("wayland-0")));
        assert!(is_wayland_session(Some("tty"), Some("wayland-1")));
    }

    #[test]
    fn x11_and_unknown_sessions_are_not_wayland() {
        assert!(!is_wayland_session(Some("x11"), None));
        assert!(!is_wayland_session(None, None));
        assert!(!is_wayland_session(Some(""), Some("")));
        assert!(!is_wayland_session(Some("x11"), Some("  ")));
    }

    #[test]
    fn linux_wayland_without_a_usable_portal_turns_the_screen_off() {
        assert_eq!(
            availability_for(true, Some("wayland"), None, || false),
            WAYLAND_OFF
        );
        assert_eq!(
            availability_for(true, None, Some("wayland-0"), || false),
            WAYLAND_OFF
        );
    }

    #[test]
    fn linux_wayland_with_a_usable_portal_keeps_the_screen() {
        assert_eq!(
            availability_for(true, Some("wayland"), None, || true),
            ScreenAvailability::Available
        );
        assert_eq!(
            availability_for(true, None, Some("wayland-0"), || true),
            ScreenAvailability::Available
        );
    }

    #[test]
    fn linux_x11_keeps_the_screen_without_asking_the_portal() {
        let never = || -> bool { panic!("the portal must not be asked outside Wayland") };
        assert_eq!(
            availability_for(true, Some("x11"), None, never),
            ScreenAvailability::Available
        );
        assert_eq!(
            availability_for(true, None, None, never),
            ScreenAvailability::Available
        );
    }

    #[test]
    fn other_platforms_ignore_the_wayland_variables() {
        let never = || -> bool { panic!("the portal must not be asked outside Linux") };
        assert_eq!(
            availability_for(false, Some("wayland"), Some("wayland-0"), never),
            ScreenAvailability::Available
        );
    }

    #[test]
    fn thumbnails_are_refused_on_wayland_even_with_a_usable_portal() {
        let denied = Some(ThumbnailUnavailableReason::WaylandUnsupported);
        assert_eq!(thumbnail_gate(true, Some("wayland"), None), denied);
        assert_eq!(thumbnail_gate(true, None, Some("wayland-0")), denied);
    }

    #[test]
    fn thumbnails_are_attempted_on_x11_and_other_platforms() {
        assert_eq!(thumbnail_gate(true, Some("x11"), None), None);
        assert_eq!(thumbnail_gate(true, None, None), None);
        assert_eq!(
            thumbnail_gate(false, Some("wayland"), Some("wayland-0")),
            None
        );
    }

    #[test]
    fn capture_failures_map_to_thumbnail_reasons() {
        for (error, reason) in [
            (
                CaptureError::permission_denied("no access"),
                ThumbnailUnavailableReason::PermissionDenied,
            ),
            (
                CaptureError::device_not_found("gone"),
                ThumbnailUnavailableReason::DisplayNotFound,
            ),
            (
                CaptureError::unsupported("no"),
                ThumbnailUnavailableReason::Unsupported,
            ),
            (
                CaptureError::failed("boom"),
                ThumbnailUnavailableReason::Failed,
            ),
        ] {
            assert_eq!(
                thumbnail_outcome(Err(error), 320),
                ThumbnailOutcome::Unavailable(reason)
            );
        }
    }

    #[test]
    fn a_captured_frame_becomes_a_scaled_image() {
        let frame = RawFrame {
            captured_at: Instant::now(),
            width: 640,
            height: 360,
            rgba: vec![255; 640 * 360 * 4],
        };
        match thumbnail_outcome(Ok(frame), 320) {
            ThumbnailOutcome::Image(image) => {
                assert_eq!((image.width, image.height), (320, 180));
                assert_eq!(image.mime_type, "image/png");
            }
            other @ ThumbnailOutcome::Unavailable(_) => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_frame_is_a_failure_not_a_panic() {
        let frame = RawFrame {
            captured_at: Instant::now(),
            width: 640,
            height: 360,
            rgba: vec![0; 12],
        };
        assert_eq!(
            thumbnail_outcome(Ok(frame), 320),
            ThumbnailOutcome::Unavailable(ThumbnailUnavailableReason::Failed)
        );
    }
}
