//! Linux: the monitor source of the default sink through cpal's PulseAudio backend (works
//! with PulseAudio and with PipeWire's `pipewire-pulse` server alike), screen frames through
//! xcap (X11 via xcb; Wayland via the desktop portal / wlroots screencopy).
//!
//! STATUS: compiled by CI only. This code has never been run (no Linux desktop in the
//! Phase 02 environment); see `.ssot/tasarim/yakalama-spike.md` for the run instructions.
//!
//! cpal's PulseAudio backend speaks the PulseAudio native protocol in pure Rust, so no
//! `libpulse` headers are needed. cpal's ALSA backend still needs `libasound2-dev` to build.

use std::path::Path;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait};

use super::{Combined, CpalInput, Recording, Result};

fn pulse_host() -> Result<cpal::Host> {
    cpal::host_from_id(cpal::HostId::PulseAudio)
        .map_err(|e| format!("PulseAudio (or pipewire-pulse) is not reachable: {e}").into())
}

/// The microphone goes through the same PulseAudio protocol endpoint as the monitor.
pub fn microphone_host() -> Result<cpal::Host> {
    pulse_host()
}

pub fn start_system(out_dir: &Path, clock: Instant) -> Result<Box<dyn Recording>> {
    let host = pulse_host()?;
    let (monitor, source_note) = find_default_monitor(&host)?;
    let audio = CpalInput::open("system", &monitor, &out_dir.join("system.wav"), clock)?;
    Ok(Box::new(
        Combined::new(vec![Box::new(audio)])
            .with_note(source_note)
            .with_frames(out_dir, clock),
    ))
}

/// PulseAudio exposes every sink's loopback as a source named `<sink name>.monitor`.
/// Returns the device plus a note naming the source that was picked (and why).
fn find_default_monitor(host: &cpal::Host) -> Result<(cpal::Device, String)> {
    let sink = host
        .default_output_device()
        .ok_or("PulseAudio reports no default sink")?;
    let wanted = format!("{}.monitor", sink.id()?.id());
    let mut fallback = None;
    for device in host.input_devices()? {
        let name = device.id()?.id().to_owned();
        if name == wanted {
            let note = format!("system audio source: {name} (monitor of the default sink)");
            return Ok((device, note));
        }
        if name.ends_with(".monitor") {
            fallback.get_or_insert((device, name));
        }
    }
    match fallback {
        Some((device, name)) => {
            let note = format!("system audio source: {name} (FALLBACK: {wanted} not found)");
            Ok((device, note))
        }
        None => Err("no PulseAudio monitor source found".into()),
    }
}
