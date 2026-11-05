//! Linux: `PulseAudio` protocol through cpal's `pulseaudio` backend.
//!
//! The backend speaks the `PulseAudio` native protocol in pure Rust, so it works with
//! `PulseAudio` and with `PipeWire`'s `pipewire-pulse` server alike and needs no `libpulse`
//! headers. System audio is the monitor source `PulseAudio` creates for every sink
//! (`<sink name>.monitor`); the microphone is the host's default source.
//!
//! STATUS: compiled by CI only; it has not been run on a Linux desktop yet. Pure-`PipeWire`
//! systems without `pipewire-pulse` fail with a "not reachable" error.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};
use tan_application::ports::{AudioSink, AudioStream, CaptureError, CaptureResult};

use super::{build_input, ThreadedStream};

fn pulse_host() -> CaptureResult<cpal::Host> {
    cpal::host_from_id(cpal::HostId::PulseAudio).map_err(|e| {
        CaptureError::device_not_found(format!(
            "PulseAudio (or pipewire-pulse) is not reachable: {e}"
        ))
    })
}

/// The microphone goes through the same `PulseAudio` endpoint as the monitor.
pub(super) fn microphone_host() -> CaptureResult<cpal::Host> {
    pulse_host()
}

pub(super) fn start_system(sink: Arc<dyn AudioSink>) -> CaptureResult<Box<dyn AudioStream>> {
    let stream = ThreadedStream::spawn("tan-system-audio", move || {
        let host = pulse_host()?;
        let monitor = find_default_monitor(&host)?;
        let (stream, info) = build_input(&monitor, sink)?;
        Ok((vec![stream], info))
    })?;
    Ok(Box::new(stream))
}

/// The monitor of the default sink; any other monitor source when that one is missing.
fn find_default_monitor(host: &cpal::Host) -> CaptureResult<cpal::Device> {
    let sink = host
        .default_output_device()
        .ok_or_else(|| CaptureError::device_not_found("PulseAudio reports no default sink"))?;
    let sink_id = sink
        .id()
        .map_err(|e| CaptureError::failed(format!("cannot read the default sink id: {e}")))?;
    let wanted = format!("{}.monitor", sink_id.id());

    let devices = host
        .input_devices()
        .map_err(|e| CaptureError::failed(format!("cannot list the PulseAudio sources: {e}")))?;
    let mut fallback = None;
    for device in devices {
        let Ok(id) = device.id() else { continue };
        if id.id() == wanted {
            return Ok(device);
        }
        if id.id().ends_with(".monitor") {
            fallback.get_or_insert(device);
        }
    }
    fallback.ok_or_else(|| CaptureError::device_not_found("no PulseAudio monitor source found"))
}
