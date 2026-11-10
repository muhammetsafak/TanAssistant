//! Capture ports: audio and screen capture, perceptual hashing and media files (ADR-009).
//!
//! These ports are **synchronous and callback based**, unlike the repository ports: capture
//! runs on OS audio/screen threads that must never wait for a runtime. An adapter delivers
//! data to a sink from its own thread; the recording use case moves the data to workers that
//! do the slow part (file I/O, hashing, database writes).
//!
//! Timestamps are [`Instant`]s taken from the monotonic clock when data is delivered, so the
//! microphone, the system audio and the frames all share one timeline (the use case keeps a
//! single origin `Instant`). The wall clock ([`crate::ports::Clock`]) is only used for the
//! human-facing session start time.

use std::error::Error;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tan_domain::{SessionId, TrackKind};

/// Why a capture operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureErrorKind {
    /// The OS denied access (microphone privacy switch, macOS screen recording permission...).
    PermissionDenied,
    /// The requested or default device does not exist.
    DeviceNotFound,
    /// This platform or configuration cannot capture that source.
    Unsupported,
    /// Anything else the capture backend reported.
    Failed,
}

/// A capture backend failure. The message is safe to show to the user (it never carries
/// audio or screen content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    /// Failure class.
    pub kind: CaptureErrorKind,
    /// Human-readable description, in English.
    pub message: String,
}

impl CaptureError {
    /// Builds an error from its parts.
    #[must_use]
    pub fn new(kind: CaptureErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// [`CaptureErrorKind::PermissionDenied`].
    #[must_use]
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(CaptureErrorKind::PermissionDenied, message)
    }

    /// [`CaptureErrorKind::DeviceNotFound`].
    #[must_use]
    pub fn device_not_found(message: impl Into<String>) -> Self {
        Self::new(CaptureErrorKind::DeviceNotFound, message)
    }

    /// [`CaptureErrorKind::Unsupported`].
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(CaptureErrorKind::Unsupported, message)
    }

    /// [`CaptureErrorKind::Failed`].
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(CaptureErrorKind::Failed, message)
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CaptureError {}

/// Result alias for capture operations.
pub type CaptureResult<T> = Result<T, CaptureError>;

// ---------------------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------------------

/// A selectable audio input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Opaque identifier to pass back in [`AudioStreamRequest::device_id`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether the OS reports it as the default input device.
    pub is_default: bool,
}

/// Layout of the interleaved PCM a stream delivers (the device's native format; resampling
/// is the transcription step's job).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
}

/// A block of interleaved `f32` samples (range `-1.0..=1.0`).
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Format of `samples`.
    pub format: AudioFormat,
    /// When the adapter received the block, i.e. the *end* of the audio it covers.
    pub captured_at: Instant,
    /// Interleaved samples; the length is a multiple of `format.channels`.
    pub samples: Vec<f32>,
}

impl PcmChunk {
    /// Number of frames (one sample per channel) in the chunk.
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        match usize::from(self.format.channels) {
            0 => 0,
            channels => u64::try_from(self.samples.len() / channels).unwrap_or(u64::MAX),
        }
    }

    /// Playback length of the chunk.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // a block holds far fewer than 2^52 frames
    pub fn duration(&self) -> Duration {
        if self.format.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.frame_count() as f64 / f64::from(self.format.sample_rate))
    }
}

/// Receives audio from a running stream. Called from the adapter's audio thread: it must not
/// block and must not do I/O.
pub trait AudioSink: Send + Sync {
    /// A block of audio arrived.
    fn on_chunk(&self, chunk: PcmChunk);

    /// The backend reported a problem (device change, dropped connection...). The stream may
    /// keep running.
    fn on_error(&self, message: String);

    /// The backend reported a short dropout in the audio data (an xrun or a data
    /// discontinuity). The stream keeps running; the recording fills the lost span with
    /// silence.
    fn on_dropout(&self);
}

/// Which audio to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStreamRequest {
    /// [`TrackKind::Microphone`] or [`TrackKind::System`] (loopback of what the machine plays).
    pub source: TrackKind,
    /// Input device to use for the microphone; `None` selects the default. Ignored for system
    /// audio, which always follows the default output.
    pub device_id: Option<String>,
}

/// A running audio capture. Dropping it without calling [`AudioStream::stop`] must stop it too.
pub trait AudioStream: Send {
    /// Format the stream delivers.
    fn format(&self) -> AudioFormat;

    /// Name of the device being captured.
    fn device_name(&self) -> &str;

    /// Stops the capture. When this returns the sink is not called any more.
    fn stop(self: Box<Self>);
}

/// Audio capture port (ADR-009: microphone and system audio on separate tracks).
pub trait AudioCapture: Send + Sync {
    /// The selectable microphone devices.
    fn list_input_devices(&self) -> CaptureResult<Vec<AudioDevice>>;

    /// Starts capturing `request.source` and delivers it to `sink`.
    fn start(
        &self,
        request: &AudioStreamRequest,
        sink: Arc<dyn AudioSink>,
    ) -> CaptureResult<Box<dyn AudioStream>>;
}

// ---------------------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------------------

/// A selectable display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Opaque identifier to pass back in [`ScreenStreamRequest::display_id`].
    pub id: String,
    /// Human-readable name (backends often cannot report a real monitor name).
    pub name: String,
    /// Width in pixels (macOS: points).
    pub width: u32,
    /// Height in pixels (macOS: points).
    pub height: u32,
    /// Whether it is the primary display.
    pub is_primary: bool,
}

/// One captured screen image, 8-bit RGBA, row-major, no padding.
#[derive(Clone)]
pub struct RawFrame {
    /// When the adapter captured it.
    pub captured_at: Instant,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl fmt::Debug for RawFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print pixel data.
        f.debug_struct("RawFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish_non_exhaustive()
    }
}

/// Receives frames from a running screen stream. Must not block.
pub trait FrameSink: Send + Sync {
    /// A frame was captured.
    fn on_frame(&self, frame: RawFrame);

    /// The backend reported a problem; the stream may keep running.
    fn on_error(&self, message: String);
}

/// Which screen to capture and how often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenStreamRequest {
    /// Display to capture; `None` selects the primary display.
    pub display_id: Option<String>,
    /// Sampling interval. Adapters that poll tick against an absolute schedule; push-based
    /// backends (macOS) use it as the minimum interval and only deliver on change.
    pub interval: Duration,
}

/// A running screen capture. Dropping it without calling [`ScreenStream::stop`] must stop it too.
pub trait ScreenStream: Send {
    /// Stops the capture. When this returns the sink is not called any more.
    fn stop(self: Box<Self>);
}

/// Why screen capture cannot be used in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenUnavailableReason {
    /// A Wayland session: periodic capture would need a `ScreenCast` portal session with a
    /// `PipeWire` stream, which is not implemented yet (the screenshot portal asks for
    /// permission on every capture). Audio is still recorded.
    WaylandUnsupported,
}

/// Whether the screen can be sampled in the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAvailability {
    /// Frames can be captured.
    Available,
    /// Frames are not captured; recording continues with audio only.
    Unavailable(ScreenUnavailableReason),
}

/// Longest edge, in pixels, of a display preview thumbnail ([`ScreenCapture::display_thumbnail`]).
pub const THUMBNAIL_MAX_EDGE: u32 = 320;

/// A small encoded preview image of one display.
///
/// The picture shows what is on the screen, so it is sensitive: it is never written to disk
/// or logged ([`fmt::Debug`] hides the bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct DisplayThumbnail {
    /// Media type of `bytes` (e.g. `image/png`).
    pub mime_type: &'static str,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The encoded image.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for DisplayThumbnail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print image data.
        f.debug_struct("DisplayThumbnail")
            .field("mime_type", &self.mime_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Why no thumbnail could be produced. This is an expected outcome (the UI falls back to the
/// display name), not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailUnavailableReason {
    /// The OS denied screen access (macOS "Screen & System Audio Recording").
    PermissionDenied,
    /// A Wayland session: a preview would need a portal dialog, which is never opened for a
    /// thumbnail.
    WaylandUnsupported,
    /// The display id does not match any display (it may have been unplugged).
    DisplayNotFound,
    /// This platform or adapter has no thumbnail support.
    Unsupported,
    /// The capture or the encoding failed.
    Failed,
}

/// Result of [`ScreenCapture::display_thumbnail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailOutcome {
    /// The preview image.
    Image(DisplayThumbnail),
    /// There is no preview, for the given reason.
    Unavailable(ThumbnailUnavailableReason),
}

/// Screen capture port.
pub trait ScreenCapture: Send + Sync {
    /// Whether periodic capture is usable in this session. Recording skips the screen
    /// silently (no error, no frames) when it is not.
    fn availability(&self) -> ScreenAvailability {
        ScreenAvailability::Available
    }

    /// The capturable displays.
    fn list_displays(&self) -> CaptureResult<Vec<DisplayInfo>>;

    /// Captures a single frame (`display_id` `None` = primary display).
    fn capture_once(&self, display_id: Option<&str>) -> CaptureResult<RawFrame>;

    /// Starts periodic capture and delivers frames to `sink`.
    fn start(
        &self,
        request: &ScreenStreamRequest,
        sink: Arc<dyn FrameSink>,
    ) -> CaptureResult<Box<dyn ScreenStream>>;

    /// Captures one frame of the display `display_id` and returns it scaled down to at most
    /// [`THUMBNAIL_MAX_EDGE`] pixels on its longest edge.
    ///
    /// It works while a recording runs and must not disturb it. It never opens a dialog of its
    /// own (no `ScreenCast` portal dialog): a missing permission or a Wayland session yields
    /// [`ThumbnailOutcome::Unavailable`] instead. On macOS the OS may still show its one-time
    /// "Screen & System Audio Recording" prompt if the permission was never decided, exactly
    /// as it does for [`ScreenCapture::list_displays`]. The image is not stored anywhere.
    ///
    /// The default has no thumbnail support.
    fn display_thumbnail(&self, display_id: &str) -> ThumbnailOutcome {
        let _ = display_id;
        ThumbnailOutcome::Unavailable(ThumbnailUnavailableReason::Unsupported)
    }
}

/// Computes the 64-bit perceptual hash stored with each frame (`Frame::perceptual_hash`).
///
/// The hash is an adapter concern (it needs pixel access); the *decision* what to keep is
/// the pure `recording::FrameSelector`.
pub trait FrameHasher: Send + Sync {
    /// Hash of `frame`: visually similar images differ in few bits (Hamming distance).
    fn hash(&self, frame: &RawFrame) -> u64;
}

// ---------------------------------------------------------------------------------------
// Media files
// ---------------------------------------------------------------------------------------

/// What an audio file on disk holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFileInfo {
    /// Path relative to the session directory (e.g. `audio/microphone.wav`).
    pub relative_path: String,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Number of frames (one sample per channel) in the file.
    pub frames: u64,
}

impl AudioFileInfo {
    /// Duration in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.frames.saturating_mul(1_000) / u64::from(self.sample_rate)
    }
}

/// Appends PCM to one audio file. The file must stay readable up to the last
/// [`AudioWriter::checkpoint`] even if the process dies without calling
/// [`AudioWriter::finish`].
pub trait AudioWriter: Send {
    /// Appends interleaved samples.
    fn write(&mut self, samples: &[f32]) -> io::Result<()>;

    /// Appends `frames` frames of silence.
    fn write_silence(&mut self, frames: u64) -> io::Result<()>;

    /// Makes everything written so far durable and readable.
    fn checkpoint(&mut self) -> io::Result<()>;

    /// Finalises the file.
    fn finish(self: Box<Self>) -> io::Result<AudioFileInfo>;
}

/// The files of a session on disk (the `<data>/sessions/<id>/{audio,frames}` layout).
pub trait MediaStore: Send + Sync {
    /// Creates the session's folders.
    fn prepare_session(&self, session_id: &SessionId) -> io::Result<()>;

    /// Removes the session's folder and everything in it (a missing folder is fine).
    fn remove_session(&self, session_id: &SessionId) -> io::Result<()>;

    /// Opens the audio file of `kind`, replacing any previous one.
    fn open_audio_writer(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        format: AudioFormat,
    ) -> io::Result<Box<dyn AudioWriter>>;

    /// Reads what the audio file of `kind` holds; `None` when there is no (readable) file.
    fn probe_audio(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
    ) -> io::Result<Option<AudioFileInfo>>;

    /// Stores a frame image `timestamp_ms` after the session start and returns its path
    /// relative to the session directory.
    fn save_frame(
        &self,
        session_id: &SessionId,
        timestamp_ms: u64,
        frame: &RawFrame,
    ) -> io::Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_reports_frames_and_duration() {
        let chunk = PcmChunk {
            format: AudioFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            captured_at: Instant::now(),
            samples: vec![0.0; 9_600],
        };
        assert_eq!(chunk.frame_count(), 4_800);
        assert_eq!(chunk.duration(), Duration::from_millis(100));
    }

    #[test]
    fn degenerate_formats_do_not_divide_by_zero() {
        let chunk = PcmChunk {
            format: AudioFormat {
                sample_rate: 0,
                channels: 0,
            },
            captured_at: Instant::now(),
            samples: vec![0.0; 8],
        };
        assert_eq!(chunk.frame_count(), 0);
        assert_eq!(chunk.duration(), Duration::ZERO);
    }

    #[test]
    fn file_info_duration_is_in_milliseconds() {
        let info = AudioFileInfo {
            relative_path: "audio/microphone.wav".to_owned(),
            sample_rate: 16_000,
            channels: 1,
            frames: 24_000,
        };
        assert_eq!(info.duration_ms(), 1_500);
    }
}
