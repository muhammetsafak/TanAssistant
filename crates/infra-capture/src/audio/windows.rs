//! Windows: WASAPI through cpal.
//!
//! cpal turns an *output* device opened with `build_input_stream` into a WASAPI loopback
//! capture (`AUDCLNT_STREAMFLAGS_LOOPBACK`), so system audio needs no Windows-specific
//! capture code. One thing it does need: while *nothing* renders on the device WASAPI
//! delivers no loopback packets at all (the spike measured 0 frames in 8 s of silence), which
//! would make `system.wav` lose time during every quiet moment of a meeting. The standard fix
//! is a silent render stream kept open on the same device.
//!
//! Known limit: the loopback follows the default output device at the time recording starts;
//! switching the default device mid-recording is not followed.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample, StreamConfig};
use tan_application::ports::{AudioSink, AudioStream, CaptureError, CaptureResult};

use super::{build_input, map_cpal_error, ThreadedStream};

/// The microphone goes through the default (WASAPI) host.
#[allow(clippy::unnecessary_wraps)] // same signature as the other platforms
pub(super) fn microphone_host() -> CaptureResult<cpal::Host> {
    Ok(cpal::default_host())
}

pub(super) fn start_system(sink: Arc<dyn AudioSink>) -> CaptureResult<Box<dyn AudioStream>> {
    let stream = ThreadedStream::spawn("tan-system-audio", move || {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| CaptureError::device_not_found("no default output device to capture"))?;
        // Keep-alive first, so the loopback sees a running render engine.
        let keep_alive = silent_render(&device)?;
        let (loopback, info) = build_input(&device, sink)?;
        Ok((vec![keep_alive, loopback], info))
    })?;
    Ok(Box::new(stream))
}

/// Plays silence on `device` so WASAPI keeps producing loopback packets.
fn silent_render(device: &cpal::Device) -> CaptureResult<cpal::Stream> {
    let config = device
        .default_output_config()
        .map_err(|e| map_cpal_error("cannot read the output device configuration", &e))?;
    let stream_config: StreamConfig = config.into();
    let stream = match config.sample_format() {
        SampleFormat::F32 => silent_stream::<f32>(device, stream_config),
        SampleFormat::I16 => silent_stream::<i16>(device, stream_config),
        SampleFormat::I32 => silent_stream::<i32>(device, stream_config),
        other => Err(CaptureError::unsupported(format!(
            "the output device uses the unsupported sample format {other:?}"
        ))),
    }?;
    stream
        .play()
        .map_err(|e| map_cpal_error("cannot start the silent render stream", &e))?;
    Ok(stream)
}

fn silent_stream<T: SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: StreamConfig,
) -> CaptureResult<cpal::Stream> {
    device
        .build_output_stream(
            config,
            |data: &mut [T], _info: &cpal::OutputCallbackInfo| data.fill(T::EQUILIBRIUM),
            // A failing keep-alive only degrades the loopback; the loopback stream reports
            // its own errors.
            |_error| {},
            None,
        )
        .map_err(|e| map_cpal_error("cannot open the silent render stream", &e))
}
