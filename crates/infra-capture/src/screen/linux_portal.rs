//! Linux Wayland: screen frames from an xdg-desktop-portal `ScreenCast` session, read through
//! `PipeWire` (ADR-009).
//!
//! STATUS: compiled by CI only; it has not been run on a Wayland desktop yet. The run
//! instructions are in the Phase 13 report.
//!
//! A Wayland compositor does not let applications read the screen. The portal is the sanctioned
//! way: the application opens a `ScreenCast` session, the desktop shows a dialog where the user
//! picks a monitor or a window, and the portal then hands over a `PipeWire` connection that
//! streams that source. Unlike the screenshot portal, which asks (or flashes) on every capture,
//! one session serves a whole recording.
//!
//! * **One dialog, once.** The session asks for `persist_mode` "until revoked" and keeps the
//!   restore token the portal returns in the data directory ([`RestoreTokenStore`]). The next
//!   recording passes it back and the portal restores the choice without a dialog. A token the
//!   portal no longer knows just brings the dialog back.
//! * **Frames on a schedule.** A compositor pushes a buffer whenever the screen changes, up to
//!   its refresh rate. Each buffer is copied raw into a pending slot (cheap); a timer on the
//!   `PipeWire` loop turns the pending slot into an RGBA [`RawFrame`] once per interval. So the
//!   cadence is steady, the last change before the screen goes still is never lost (it is
//!   delivered at the next tick) and the colour conversion runs once per interval, not once
//!   per compositor frame. The very first buffer is delivered at once.
//! * **Threads.** Everything portal- and `PipeWire`-related lives on one worker thread
//!   (`tan-screencast`): it negotiates the session (blocking on the dialog), then runs the
//!   `PipeWire` main loop until it is told to quit, then closes the session. The thread's
//!   `ready` channel reports the outcome of the start, so [`start`] returns only once frames
//!   can flow (or the user declined).
//! * **No tokio.** `ashpd` runs on zbus' own executor (a helper thread of its own); the futures
//!   are driven with `futures_executor::block_on`.
//!
//! The user can end the sharing at any time from the desktop's indicator; the stream then ends
//! and the sink is told once ([`FrameSink::on_error`]); the recording carries on with audio.

use std::cell::RefCell;
use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{
    OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, ResponseError, Session};
use futures_executor::block_on;
use pipewire as pw;
use pw::spa;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::utils::{Fraction, Rectangle};
use pw::stream::StreamState;
use tan_application::ports::{
    CaptureError, CaptureResult, DisplayInfo, FrameSink, RawFrame, ScreenStream,
    ScreenStreamRequest,
};

use super::frame_convert::{self, PixelFormat};
use super::probe_gate::{Claim, ProbeGate};
use super::restore_token::{self, RestoreTokenStore};

/// Id of the one display the Wayland adapter lists: the source is chosen in the system dialog,
/// not by the application.
const PORTAL_DISPLAY_ID: &str = "portal";

/// Shortest sampling interval accepted (the settings enforce 500 ms already).
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long `start` waits for the user to answer the source dialog. `start` blocks the
/// caller (and the recording service reports no recording yet) for that long, so it stays well
/// below the 60 s the application grants a recording to settle when it is closed.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(45);

/// How long the availability probe waits for the portal to answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a negative probe result is trusted before the portal is asked again (a portal that
/// is not up yet at login may be a moment later; a positive result is kept).
const NEGATIVE_PROBE_TTL: Duration = Duration::from_secs(30);

/// How long `capture_once` waits for its single frame.
const SINGLE_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// The one display listed on Wayland.
pub(super) fn list_displays() -> Vec<DisplayInfo> {
    vec![DisplayInfo {
        id: PORTAL_DISPLAY_ID.to_owned(),
        name: "Chosen in the system dialog".to_owned(),
        // The size is only known once the stream runs.
        width: 0,
        height: 0,
        is_primary: true,
    }]
}

// ---------------------------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------------------------

/// Remembers the probe's answer and makes concurrent askers share one probe.
static PROBE_GATE: ProbeGate = ProbeGate::new();

/// Whether a `ScreenCast` session can be set up here: the portal frontend is on the bus and
/// offers a monitor or window source.
///
/// A positive answer is cached for the life of the process; a negative one for
/// [`NEGATIVE_PROBE_TTL`]. The probe never shows a dialog. A caller that arrives while a probe
/// is running waits for its answer (at most [`PROBE_TIMEOUT`] from the probe's start) rather
/// than guessing; only a portal that does not answer within that time counts as unusable, and
/// while such a probe is still stuck no second one is started.
pub(super) fn screencast_usable() -> bool {
    match PROBE_GATE.claim(NEGATIVE_PROBE_TTL, PROBE_TIMEOUT) {
        Claim::Answer(usable) => usable,
        Claim::Run => {
            start_probe();
            PROBE_GATE.wait(NEGATIVE_PROBE_TTL, PROBE_TIMEOUT)
        }
    }
}

/// Asks the portal on a helper thread so a stuck portal cannot hang the caller for more than
/// [`PROBE_TIMEOUT`]. The thread records its own answer, so one that arrives late is kept too.
fn start_probe() {
    let spawned = std::thread::Builder::new()
        .name("tan-portal-probe".to_owned())
        .spawn(|| PROBE_GATE.finish(block_on(portal_offers_sources())));
    if spawned.is_err() {
        PROBE_GATE.finish(false);
    }
}

async fn portal_offers_sources() -> bool {
    let Ok(screencast) = Screencast::new().await else {
        return false;
    };
    screencast
        .available_source_types()
        .await
        .is_ok_and(|types| types.intersects(SourceType::MONITOR | SourceType::WINDOW))
}

// ---------------------------------------------------------------------------------------
// Starting a stream
// ---------------------------------------------------------------------------------------

/// Starts a `ScreenCast` stream and delivers a frame to `sink` every `request.interval`
/// (on change; see the module docs). `request.display_id` is ignored: the user picks the source
/// in the portal dialog.
///
/// Blocks until the user has answered the dialog (at most [`DIALOG_TIMEOUT`], 45 s) and the stream is
/// set up. A declined dialog is [`CaptureError::permission_denied`].
pub(super) fn start(
    request: &ScreenStreamRequest,
    sink: Arc<dyn FrameSink>,
) -> CaptureResult<Box<dyn ScreenStream>> {
    let interval = request.interval.max(MIN_INTERVAL);
    let abandoned = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();

    let worker_abandoned = abandoned.clone();
    let handle = std::thread::Builder::new()
        .name("tan-screencast".to_owned())
        .spawn(move || run_worker(interval, &sink, &worker_abandoned, &ready_tx))
        .map_err(|e| {
            CaptureError::failed(format!("cannot start the screen capture thread: {e}"))
        })?;

    match ready_rx.recv_timeout(DIALOG_TIMEOUT) {
        Ok(Ok(quit)) => Ok(Box::new(PortalStream {
            quit: Some(quit),
            handle: Some(handle),
        })),
        Ok(Err(error)) => {
            let _ = handle.join();
            Err(error)
        }
        Err(RecvTimeoutError::Timeout) => {
            // The worker is still waiting for the dialog; when the user finally answers, it
            // sees this flag, closes the session and exits on its own. The flag is set
            // before the channel is drained and the worker re-checks it after sending, so
            // whichever happens last, a stream that became ready in this window is stopped.
            abandoned.store(true, Ordering::SeqCst);
            if let Ok(Ok(quit)) = ready_rx.try_recv() {
                let _ = quit.send(());
                let _ = handle.join();
            }
            Err(CaptureError::failed(
                "the screen sharing dialog was not answered in time",
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err(CaptureError::failed(
                "the screen capture thread ended unexpectedly",
            ))
        }
    }
}

/// One frame, for [`ScreenCapture::capture_once`](tan_application::ports::ScreenCapture): a
/// short session that stops after its first frame. With a saved restore token there is no
/// dialog.
pub(super) fn capture_once() -> CaptureResult<RawFrame> {
    let (frame_tx, frame_rx) = mpsc::channel();
    let sink = Arc::new(FirstFrameSink {
        frame: Mutex::new(Some(frame_tx)),
        problems: Mutex::new(Vec::new()),
    });
    let stream = start(
        &ScreenStreamRequest {
            display_id: None,
            interval: Duration::from_millis(500),
        },
        sink.clone(),
    )?;
    let outcome = frame_rx.recv_timeout(SINGLE_FRAME_TIMEOUT);
    stream.stop();
    outcome.map_err(|_| {
        let problems = sink.problems.lock().unwrap_or_else(PoisonError::into_inner);
        CaptureError::failed(
            problems
                .last()
                .cloned()
                .unwrap_or_else(|| "no frame arrived from the screen sharing stream".to_owned()),
        )
    })
}

/// Hands the first frame to `capture_once` and remembers problems for its error message.
struct FirstFrameSink {
    frame: Mutex<Option<mpsc::Sender<RawFrame>>>,
    problems: Mutex<Vec<String>>,
}

impl FrameSink for FirstFrameSink {
    fn on_frame(&self, frame: RawFrame) {
        let sender = self
            .frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(frame);
        }
    }

    fn on_error(&self, message: String) {
        self.problems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message);
    }
}

/// A running portal stream. Stopping (or dropping) it ends the `PipeWire` loop, which closes
/// the portal session.
struct PortalStream {
    quit: Option<pw::channel::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl PortalStream {
    fn shutdown(&mut self) {
        if let Some(quit) = self.quit.take() {
            // A closed channel means the loop already ended on its own.
            let _ = quit.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl ScreenStream for PortalStream {
    fn stop(mut self: Box<Self>) {
        self.shutdown();
    }
}

impl Drop for PortalStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Outcome of the start, sent once by the worker.
type ReadyMessage = CaptureResult<pw::channel::Sender<()>>;

/// Body of the `tan-screencast` thread: portal negotiation, `PipeWire` loop, session close.
fn run_worker(
    interval: Duration,
    sink: &Arc<dyn FrameSink>,
    abandoned: &AtomicBool,
    ready: &mpsc::Sender<ReadyMessage>,
) {
    let store = default_data_dir().map(|dir| RestoreTokenStore::in_data_dir(&dir));
    let negotiated = match block_on(negotiate(store.as_ref())) {
        Ok(negotiated) => negotiated,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let Negotiated {
        session,
        node_id,
        fd,
        notes,
    } = negotiated;
    if abandoned.load(Ordering::SeqCst) {
        block_on(close_session(&session));
        return;
    }
    for note in notes {
        sink.on_error(note);
    }

    let mut ready_slot = Some(ready);
    let outcome = run_pipewire(fd, node_id, interval, sink, |quit| {
        if let Some(ready) = ready_slot.take() {
            // `start` may have given up waiting (the dialog timed out): nobody wants this
            // stream then. Either the message is refused (receiver gone) or `start` set
            // `abandoned` before it looked at the channel; both are handled here or there.
            let stop = quit.clone();
            match ready.send(Ok(quit)) {
                Err(mpsc::SendError(Ok(quit))) => {
                    let _ = quit.send(());
                }
                Ok(()) if abandoned.load(Ordering::SeqCst) => {
                    let _ = stop.send(());
                }
                _ => {}
            }
        }
    });
    if let Err(error) = outcome {
        match ready_slot.take() {
            Some(ready) => {
                let _ = ready.send(Err(error));
            }
            None => sink.on_error(error.to_string()),
        }
    }
    block_on(close_session(&session));
}

async fn close_session(session: &Session<Screencast>) {
    // The portal drops a session whose connection goes away anyway; a failure here is moot.
    let _ = session.close().await;
}

// ---------------------------------------------------------------------------------------
// Portal negotiation
// ---------------------------------------------------------------------------------------

/// What the portal handed over.
struct Negotiated {
    session: Session<Screencast>,
    node_id: u32,
    fd: OwnedFd,
    /// Non-fatal problems to report to the sink once the stream runs.
    notes: Vec<String>,
}

/// The directory the restore token lives in: the same one the application uses for its data
/// (`TAN_DATA_DIR`, else the per-user data directory; kept in step with
/// `tan_infra_storage::DataLayout`, which infra crates cannot depend on).
fn default_data_dir() -> Option<std::path::PathBuf> {
    restore_token::resolve_data_dir(std::env::var_os(restore_token::DATA_DIR_ENV), || {
        directories::ProjectDirs::from("dev", "muhammetsafak", "Tan")
            .map(|dirs| dirs.data_dir().to_path_buf())
    })
}

fn portal_error(context: &str, error: &ashpd::Error) -> CaptureError {
    match error {
        ashpd::Error::Response(ResponseError::Cancelled)
        | ashpd::Error::Portal(
            ashpd::PortalError::Cancelled(_) | ashpd::PortalError::NotAllowed(_),
        ) => CaptureError::permission_denied("the screen sharing request was declined"),
        ashpd::Error::PortalNotFound(_) => CaptureError::unsupported(
            "the desktop offers no ScreenCast portal (install xdg-desktop-portal and the \
             backend for your desktop)",
        ),
        other => CaptureError::failed(format!("{context}: {other}")),
    }
}

/// Opens a session, lets the user pick (or restores) a source and opens the `PipeWire` remote.
async fn negotiate(store: Option<&RestoreTokenStore>) -> CaptureResult<Negotiated> {
    let screencast = Screencast::new()
        .await
        .map_err(|e| portal_error("the ScreenCast portal is not reachable", &e))?;
    let session = screencast
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| portal_error("cannot create the ScreenCast session", &e))?;
    match configure(&screencast, &session, store).await {
        Ok((node_id, fd, notes)) => Ok(Negotiated {
            session,
            node_id,
            fd,
            notes,
        }),
        Err(error) => {
            close_session(&session).await;
            Err(error)
        }
    }
}

async fn configure(
    screencast: &Screencast,
    session: &Session<Screencast>,
    store: Option<&RestoreTokenStore>,
) -> CaptureResult<(u32, OwnedFd, Vec<String>)> {
    let saved = store.and_then(RestoreTokenStore::load);
    // Restore tokens exist from version 4 of the interface on; older portals ask every time.
    let persist = if screencast.version() >= 4 {
        PersistMode::ExplicitlyRevoked
    } else {
        PersistMode::DoNot
    };
    screencast
        .select_sources(
            session,
            SelectSourcesOptions::default()
                .set_sources(SourceType::MONITOR | SourceType::WINDOW)
                .set_multiple(false)
                .set_restore_token(saved.as_deref())
                .set_persist_mode(persist),
        )
        .await
        .and_then(|request| request.response())
        .map_err(|e| portal_error("cannot select the screen sources", &e))?;

    // This is where the dialog appears (unless the saved token restores the choice).
    let started = screencast
        .start(session, None, StartCastOptions::default())
        .await
        .and_then(|request| request.response())
        .map_err(|e| portal_error("cannot start the ScreenCast session", &e))?;
    let node_id = started
        .streams()
        .first()
        .map(ashpd::desktop::screencast::Stream::pipe_wire_node_id)
        .ok_or_else(|| CaptureError::failed("the portal returned no screen stream"))?;

    let mut notes = Vec::new();
    match (store, started.restore_token()) {
        (Some(store), Some(token)) => {
            if let Err(error) = store.save(token) {
                notes.push(format!(
                    "the screen sharing permission cannot be remembered, the dialog will \
                     appear again: {error}"
                ));
            }
        }
        (None, _) => notes.push(
            "the data directory is unknown, so the screen sharing permission is not remembered"
                .to_owned(),
        ),
        // The portal issued no token (an old portal, or persistence was refused).
        (Some(_), None) => {}
    }

    let fd = screencast
        .open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default())
        .await
        .map_err(|e| portal_error("cannot open the PipeWire connection", &e))?;
    Ok((node_id, fd, notes))
}

// ---------------------------------------------------------------------------------------
// PipeWire stream
// ---------------------------------------------------------------------------------------

fn pw_error(context: &str, error: &dyn std::fmt::Display) -> CaptureError {
    CaptureError::failed(format!("{context}: {error}"))
}

/// The newest buffer content, copied raw until the next tick turns it into a frame.
struct Pending {
    captured_at: Instant,
    format: PixelFormat,
    width: u32,
    height: u32,
    stride: usize,
    data: Vec<u8>,
}

/// State shared by the `PipeWire` callbacks (they all run on the loop's thread).
struct CastState {
    sink: Arc<dyn FrameSink>,
    /// Format the stream negotiated; `None` until the first `param_changed`.
    info: Option<VideoInfoRaw>,
    pending: Pending,
    /// Whether `pending` holds a buffer that has not been delivered yet.
    dirty: bool,
    first_delivered: bool,
    /// Set when the loop is being stopped on purpose, so the teardown is not reported.
    stopping: bool,
}

impl CastState {
    fn new(sink: Arc<dyn FrameSink>) -> Self {
        Self {
            sink,
            info: None,
            pending: Pending {
                captured_at: Instant::now(),
                format: PixelFormat::Bgrx,
                width: 0,
                height: 0,
                stride: 0,
                data: Vec::new(),
            },
            dirty: false,
            first_delivered: false,
            stopping: false,
        }
    }

    /// Copies a buffer's pixels into the pending slot. `offset` and `size` locate the valid
    /// bytes inside `bytes` (the buffer's chunk); `stride` is the row pitch it reports.
    /// Buffers without pixels (cursor-only updates) and buffers that do not fit the
    /// negotiated format are skipped.
    fn ingest(&mut self, bytes: &[u8], offset: u32, size: u32, stride: i32) {
        let Some(info) = self.info else {
            return;
        };
        let Some(format) = pixel_format(info.format()) else {
            return;
        };
        let (width, height) = (info.size().width, info.size().height);
        let start = to_usize(offset);
        let Some(region) = start
            .checked_add(to_usize(size))
            .and_then(|end| bytes.get(start..end))
            .filter(|region| !region.is_empty())
        else {
            return;
        };
        let stride =
            frame_convert::effective_stride(stride, to_usize(width), format.bytes_per_pixel());
        let fits = frame_convert::min_buffer_len(width, height, stride, format.bytes_per_pixel())
            .is_some_and(|needed| needed <= region.len());
        if !fits {
            return;
        }
        let pending = &mut self.pending;
        pending.data.clear();
        pending.data.extend_from_slice(region);
        pending.format = format;
        pending.width = width;
        pending.height = height;
        pending.stride = stride;
        pending.captured_at = Instant::now();
        self.dirty = true;
    }

    /// Delivers the pending buffer to the sink as an RGBA frame.
    fn deliver(&mut self) {
        self.dirty = false;
        self.first_delivered = true;
        let pending = &self.pending;
        if let Some(rgba) = frame_convert::to_rgba(
            pending.format,
            pending.width,
            pending.height,
            pending.stride,
            &pending.data,
        ) {
            self.sink.on_frame(RawFrame {
                captured_at: pending.captured_at,
                width: pending.width,
                height: pending.height,
                rgba,
            });
        }
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The pixel layouts the stream is asked for, and nothing else.
fn pixel_format(format: VideoFormat) -> Option<PixelFormat> {
    match format {
        VideoFormat::BGRx => Some(PixelFormat::Bgrx),
        VideoFormat::BGRA => Some(PixelFormat::Bgra),
        VideoFormat::RGBx => Some(PixelFormat::Rgbx),
        VideoFormat::RGBA => Some(PixelFormat::Rgba),
        VideoFormat::RGB => Some(PixelFormat::Rgb),
        _ => None,
    }
}

/// The format request for the stream: raw video in one of the layouts [`pixel_format`] knows,
/// any size up to 16384 x 16384 (several monitors side by side exceed 4096), any frame rate.
fn format_request() -> CaptureResult<Vec<u8>> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::RGB
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: 16_384,
                height: 16_384
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );
    let (bytes, _) = spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|e| {
        pw_error(
            "cannot build the PipeWire format request",
            &format!("{e:?}"),
        )
    })?;
    Ok(bytes.into_inner())
}

/// Registers the callbacks of the stream: format negotiation, buffers, state changes.
fn register_listener(
    stream: &pw::stream::StreamRc,
    state: &Rc<RefCell<CastState>>,
    main_loop: &pw::main_loop::MainLoopRc,
) -> CaptureResult<pw::stream::StreamListener<()>> {
    stream
        .add_local_listener::<()>()
        .state_changed({
            let main_loop = main_loop.clone();
            let state = state.clone();
            move |_stream, _data, old, new| {
                let ended = match new {
                    StreamState::Error(message) => {
                        Some(format!("the screen stream failed: {message}"))
                    }
                    StreamState::Unconnected if old != StreamState::Unconnected => {
                        Some("the screen sharing ended".to_owned())
                    }
                    _ => None,
                };
                if let Some(message) = ended {
                    let cast = state.borrow();
                    if !cast.stopping {
                        cast.sink.on_error(message);
                    }
                    drop(cast);
                    main_loop.quit();
                }
            }
        })
        .param_changed({
            let state = state.clone();
            move |_stream, _data, id, param| {
                let Some(param) = param else {
                    return;
                };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param)
                else {
                    return;
                };
                if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                    return;
                }
                let mut info = VideoInfoRaw::new();
                if info.parse(param).is_ok() {
                    state.borrow_mut().info = Some(info);
                }
            }
        })
        .process({
            let state = state.clone();
            move |stream, _data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let mut cast = state.borrow_mut();
                if let Some(data) = buffer.datas_mut().first_mut() {
                    let chunk = data.chunk();
                    let (offset, size, stride) = (chunk.offset(), chunk.size(), chunk.stride());
                    if let Some(bytes) = data.data() {
                        cast.ingest(bytes, offset, size, stride);
                    }
                }
                if cast.dirty && !cast.first_delivered {
                    cast.deliver();
                }
            }
        })
        .register()
        .map_err(|e| pw_error("cannot listen to the PipeWire stream", &e))
}

/// Connects to the `PipeWire` remote the portal opened, streams the node `node_id` and runs
/// the loop until it is told to quit (or the stream ends).
///
/// `on_ready` is called with the sender that stops the loop, right before the loop starts; an
/// `Err` return means `on_ready` was not called.
fn run_pipewire(
    fd: OwnedFd,
    node_id: u32,
    interval: Duration,
    sink: &Arc<dyn FrameSink>,
    on_ready: impl FnOnce(pw::channel::Sender<()>),
) -> CaptureResult<()> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| pw_error("cannot create the PipeWire loop", &e))?;
    let context = pw::context::ContextRc::new(&main_loop, None)
        .map_err(|e| pw_error("cannot create the PipeWire context", &e))?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|e| pw_error("cannot connect to PipeWire", &e))?;
    let stream = pw::stream::StreamRc::new(
        core,
        "tan-screen",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| pw_error("cannot create the PipeWire stream", &e))?;

    let state = Rc::new(RefCell::new(CastState::new(sink.clone())));

    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let _quit_receiver = quit_rx.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        let state = state.clone();
        move |()| {
            state.borrow_mut().stopping = true;
            main_loop.quit();
        }
    });

    let listener = register_listener(&stream, &state, &main_loop)?;

    let request = format_request()?;
    let request_pod = spa::pod::Pod::from_bytes(&request)
        .ok_or_else(|| CaptureError::failed("the PipeWire format request is malformed"))?;
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [request_pod],
        )
        .map_err(|e| pw_error("cannot connect the PipeWire stream", &e))?;

    // One frame per interval after the first one, on the timer's own steady schedule.
    let timer = main_loop.loop_().add_timer({
        let state = state.clone();
        move |_expirations| {
            let mut cast = state.borrow_mut();
            if cast.dirty {
                cast.deliver();
            }
        }
    });
    timer
        .update_timer(Some(interval), Some(interval))
        .into_sync_result()
        .map_err(|e| pw_error("cannot arm the frame timer", &e))?;

    on_ready(quit_tx);
    main_loop.run();

    drop(timer);
    listener.unregister();
    let _ = stream.disconnect();
    Ok(())
}
