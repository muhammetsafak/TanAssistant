//! Phase 02 spike: record the microphone (`mic.wav`), the system audio (`system.wav`) and a
//! screen frame every 3 s (`frame-<ms>.png`) on Windows, macOS and Linux.
//!
//! ```text
//! cargo run -p tan-infra-capture --example capture_spike -- --seconds 20 --out <dir>
//! ```
//!
//! Defaults: 60 seconds, output into `./capture-spike-out`. `<ms>` in the frame names is the
//! number of milliseconds since the recording started. Findings live in
//! `.ssot/tasarim/yakalama-spike.md`; this code is not reused in production.

// Spike code: `unwrap`-free but deliberately simple argument parsing and printing.
#![allow(clippy::pedantic)]

mod spike;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: capture_spike [--seconds <n>] [--out <dir>]";

fn parse_args() -> Result<spike::Config, String> {
    let mut config = spike::Config {
        seconds: 60,
        out_dir: PathBuf::from("capture-spike-out"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => {
                let value = args.next().ok_or("--seconds needs a value")?;
                config.seconds = value
                    .parse()
                    .map_err(|_| format!("invalid --seconds value: {value}"))?;
                if config.seconds == 0 {
                    return Err("--seconds must be at least 1".to_owned());
                }
            }
            "--out" => {
                config.out_dir = PathBuf::from(args.next().ok_or("--out needs a value")?);
            }
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    Ok(config)
}

fn main() -> ExitCode {
    let config = match parse_args() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match spike::run(&config) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("capture spike failed: {err}");
            ExitCode::FAILURE
        }
    }
}
