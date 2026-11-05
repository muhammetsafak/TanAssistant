//! Windows: WASAPI loopback of the default output device through cpal, screen frames
//! through xcap (Windows Graphics Capture, `wgc` feature).
//!
//! cpal 0.18 turns an *output* device opened with `build_input_stream` into a WASAPI
//! loopback capture (`AUDCLNT_STREAMFLAGS_LOOPBACK`), so no Windows-specific capture code
//! is needed for the audio itself.
//!
//! Learned the hard way: while *nothing* renders on the output device WASAPI delivers no
//! loopback packets at all (0 frames in the first experiment). The standard fix is to keep
//! a silent render stream alive on the same device, which is what `SilentRender` does.

use std::path::Path;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample, StreamConfig};

use super::{Combined, CpalInput, Recording, Report, Result};

/// The microphone goes through the default (WASAPI) host.
pub fn microphone_host() -> Result<cpal::Host> {
    Ok(cpal::default_host())
}

pub fn start_system(out_dir: &Path, clock: Instant) -> Result<Box<dyn Recording>> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device to loop back")?;
    // Keep-alive first, so the loopback capture sees a running render engine.
    let keep_alive = SilentRender::start(&device)?;
    let audio = CpalInput::open("system", &device, &out_dir.join("system.wav"), clock)?;
    Ok(Box::new(
        Combined::new(vec![Box::new(audio), Box::new(keep_alive)])
            .with_note(
                "system audio source: WASAPI loopback of the default output device".to_owned(),
            )
            .with_frames(out_dir, clock),
    ))
}

/// Plays silence on the default output device so WASAPI keeps producing loopback packets.
struct SilentRender {
    stream: cpal::Stream,
}

impl SilentRender {
    fn start(device: &cpal::Device) -> Result<Self> {
        let config = device.default_output_config()?;
        let stream_config: StreamConfig = config.into();
        let stream = match config.sample_format() {
            SampleFormat::F32 => silent_stream::<f32>(device, stream_config)?,
            SampleFormat::I16 => silent_stream::<i16>(device, stream_config)?,
            SampleFormat::I32 => silent_stream::<i32>(device, stream_config)?,
            other => return Err(format!("unsupported output sample format {other:?}").into()),
        };
        stream.play()?;
        Ok(Self { stream })
    }
}

fn silent_stream<T: SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: StreamConfig,
) -> Result<cpal::Stream> {
    Ok(device.build_output_stream(
        config,
        |data: &mut [T], _info: &cpal::OutputCallbackInfo| data.fill(T::EQUILIBRIUM),
        |err| eprintln!("  [cpal output error] {err}"),
        None,
    )?)
}

impl Recording for SilentRender {
    fn stop(self: Box<Self>) -> Result<Vec<Report>> {
        drop(self.stream);
        Ok(vec![Report::Note(
            "WASAPI loopback only delivers data while something renders on the device; a silent render stream was kept alive".to_owned(),
        )])
    }
}
