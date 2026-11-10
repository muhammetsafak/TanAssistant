//! Audio capture adapters: the microphone and the system audio as two independent streams.
//!
//! | Platform | Microphone | System audio |
//! |---|---|---|
//! | Windows | cpal (WASAPI) | cpal: the default *output* device opened as input (WASAPI loopback) plus a silent render stream |
//! | macOS 13+ | cpal (`CoreAudio`) | `ScreenCaptureKit` audio-only stream |
//! | Linux | cpal (`PulseAudio` protocol, also served by `PipeWire`) | cpal: `<default sink>.monitor` source |
//!
//! Every cpal stream lives on a dedicated thread that builds it, owns it and drops it: not
//! every backend's `Stream` is `Send`, and this keeps the [`AudioStream`] handles `Send`
//! without any `unsafe`.

use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use tan_application::ports::{
    AudioCapture, AudioDevice, AudioFormat, AudioSink, AudioStream, AudioStreamRequest,
    CaptureError, CaptureErrorKind, CaptureResult, PcmChunk,
};
use tan_domain::TrackKind;

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
compile_error!("tan-infra-capture supports Windows, macOS and Linux only");

/// Microphone and system audio for the platform this crate is compiled for.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformAudioCapture;

impl AudioCapture for PlatformAudioCapture {
    fn list_input_devices(&self) -> CaptureResult<Vec<AudioDevice>> {
        let host = platform::microphone_host()?;
        let default_id = host
            .default_input_device()
            .and_then(|d| d.id().ok())
            .map(|id| id.to_string());
        let devices = host
            .input_devices()
            .map_err(|e| map_cpal_error("cannot list the input devices", &e))?;

        let mut listed = Vec::new();
        for device in devices {
            let Ok(id) = device.id() else { continue };
            // PulseAudio lists the loopback of every output as an input source; those are
            // "system audio", not microphones.
            if id.id().ends_with(".monitor") {
                continue;
            }
            let id = id.to_string();
            let name = device
                .description()
                .map_or_else(|_| id.clone(), |d| d.name().to_owned());
            listed.push(AudioDevice {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
            });
        }
        Ok(listed)
    }

    fn start(
        &self,
        request: &AudioStreamRequest,
        sink: Arc<dyn AudioSink>,
    ) -> CaptureResult<Box<dyn AudioStream>> {
        match request.source {
            TrackKind::Microphone => start_microphone(request.device_id.clone(), sink),
            TrackKind::System => platform::start_system(sink),
        }
    }
}

fn start_microphone(
    device_id: Option<String>,
    sink: Arc<dyn AudioSink>,
) -> CaptureResult<Box<dyn AudioStream>> {
    let stream = ThreadedStream::spawn("tan-microphone", move || {
        let host = platform::microphone_host()?;
        let device = match device_id {
            Some(id) => {
                let parsed: cpal::DeviceId = id.parse().map_err(|_| {
                    CaptureError::device_not_found("the selected microphone id is not valid")
                })?;
                host.device_by_id(&parsed).ok_or_else(|| {
                    CaptureError::device_not_found("the selected microphone is not available")
                })?
            }
            None => host
                .default_input_device()
                .ok_or_else(|| CaptureError::device_not_found("no microphone was found"))?,
        };
        let (stream, info) = build_input(&device, sink)?;
        Ok((vec![stream], info))
    })?;
    Ok(Box::new(stream))
}

// ---------------------------------------------------------------------------------------
// Stream thread
// ---------------------------------------------------------------------------------------

/// What a started stream reports about itself.
#[derive(Debug, Clone)]
struct StreamInfo {
    format: AudioFormat,
    device_name: String,
}

/// A set of cpal streams owned by one thread; dropping or stopping it ends the thread and
/// with it the streams.
struct ThreadedStream {
    info: StreamInfo,
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl ThreadedStream {
    /// Runs `build` on a new thread. `build` returns the streams to keep alive (they are
    /// already playing) and the format of the main one.
    fn spawn<B>(name: &str, build: B) -> CaptureResult<Self>
    where
        B: FnOnce() -> CaptureResult<(Vec<cpal::Stream>, StreamInfo)> + Send + 'static,
    {
        let (stop, stopped) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<CaptureResult<StreamInfo>>();
        let handle = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || match build() {
                Ok((streams, info)) => {
                    let _ = ready_tx.send(Ok(info));
                    // Park until stop() (or the handle being dropped), then drop the streams.
                    let _ = stopped.recv();
                    drop(streams);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| CaptureError::failed(format!("cannot start the audio thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(info)) => Ok(Self {
                info,
                stop: Some(stop),
                handle: Some(handle),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => Err(CaptureError::failed(
                "the audio thread ended before it was ready",
            )),
        }
    }

    fn shutdown(&mut self) {
        drop(self.stop.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl AudioStream for ThreadedStream {
    fn format(&self) -> AudioFormat {
        self.info.format
    }

    fn device_name(&self) -> &str {
        &self.info.device_name
    }

    fn stop(mut self: Box<Self>) {
        self.shutdown();
    }
}

impl Drop for ThreadedStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------------------
// cpal input
// ---------------------------------------------------------------------------------------

/// Opens `device` as an input stream and starts it. On WASAPI an *output* device opened this
/// way becomes a loopback capture of what that device plays.
fn build_input(
    device: &cpal::Device,
    sink: Arc<dyn AudioSink>,
) -> CaptureResult<(cpal::Stream, StreamInfo)> {
    let config = if device.supports_input() {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| map_cpal_error("cannot read the audio device configuration", &e))?;

    let format = AudioFormat {
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    };
    let device_name = device
        .description()
        .map_or_else(|_| "unknown device".to_owned(), |d| d.name().to_owned());
    let stream_config: StreamConfig = config.into();

    let stream = match config.sample_format() {
        SampleFormat::F32 => build_typed::<f32>(device, stream_config, format, sink),
        SampleFormat::I16 => build_typed::<i16>(device, stream_config, format, sink),
        SampleFormat::I32 => build_typed::<i32>(device, stream_config, format, sink),
        SampleFormat::U16 => build_typed::<u16>(device, stream_config, format, sink),
        other => Err(CaptureError::unsupported(format!(
            "the audio device uses the unsupported sample format {other:?}"
        ))),
    }?;
    stream
        .play()
        .map_err(|e| map_cpal_error("cannot start the audio stream", &e))?;
    Ok((
        stream,
        StreamInfo {
            format,
            device_name,
        },
    ))
}

fn build_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    format: AudioFormat,
    sink: Arc<dyn AudioSink>,
) -> CaptureResult<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let error_sink = sink.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _info: &cpal::InputCallbackInfo| {
                // Convert and hand over; all I/O happens on the writer thread.
                let samples = data.iter().map(|s| f32::from_sample(*s)).collect();
                sink.on_chunk(PcmChunk {
                    format,
                    captured_at: Instant::now(),
                    samples,
                });
            },
            move |error| {
                if error.kind() == cpal::ErrorKind::Xrun {
                    error_sink.on_dropout();
                } else {
                    error_sink.on_error(error.to_string());
                }
            },
            None,
        )
        .map_err(|e| map_cpal_error("cannot open the audio stream", &e))
}

/// Maps a cpal error to a [`CaptureError`], with a hint for the common permission case.
fn map_cpal_error(context: &str, error: &cpal::Error) -> CaptureError {
    let kind = match error.kind() {
        cpal::ErrorKind::PermissionDenied => CaptureErrorKind::PermissionDenied,
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
            CaptureErrorKind::DeviceNotFound
        }
        cpal::ErrorKind::UnsupportedConfig | cpal::ErrorKind::UnsupportedOperation => {
            CaptureErrorKind::Unsupported
        }
        _ => CaptureErrorKind::Failed,
    };
    let hint = if kind == CaptureErrorKind::PermissionDenied {
        " (allow microphone access for this app in the system privacy settings)"
    } else {
        ""
    };
    CaptureError::new(kind, format!("{context}: {error}{hint}"))
}
