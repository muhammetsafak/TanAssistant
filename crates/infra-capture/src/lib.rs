//! Capture adapters: audio and screen capture per platform, perceptual hashing and the
//! session media files (ADR-009).
//!
//! Wire-up: hand `PlatformAudioCapture`, `PlatformScreenCapture`, `DHasher` and a
//! `FileMediaStore` rooted at the sessions directory to `tan_application::recording`.
//!
//! The platform modules are selected with `cfg(target_os)`. Only Windows is exercised on the
//! development machine; the macOS and Linux modules are built by CI and run by hand (see
//! `.ssot/tasarim/yakalama-spike.md`).

use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use tan_application::ports::{AudioFileInfo, AudioFormat, AudioWriter, MediaStore, RawFrame};
use tan_domain::{SessionId, TrackKind};

pub mod audio;
pub mod phash;
pub mod screen;
pub mod wav_writer;

pub use audio::PlatformAudioCapture;
pub use phash::DHasher;
pub use screen::PlatformScreenCapture;
pub use wav_writer::WavTrackWriter;

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-capture";

const AUDIO_DIR: &str = "audio";
const FRAMES_DIR: &str = "frames";

/// Stores session media under `<sessions dir>/<session id>/{audio,frames}`.
///
/// This is the same layout `tan_infra_storage::DataLayout` describes (infra crates must not
/// depend on each other, so the sessions directory is passed in by the composition root).
/// Paths reported back are relative to the session directory, as the database stores them.
#[derive(Debug, Clone)]
pub struct FileMediaStore {
    sessions_dir: PathBuf,
}

impl FileMediaStore {
    /// A store whose session folders live in `sessions_dir`.
    #[must_use]
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// The session's folder. The id becomes a path segment, so anything other than ASCII
    /// letters, digits, `-` and `_` is rejected (no separators, no `..`).
    fn session_dir(&self, session_id: &SessionId) -> io::Result<PathBuf> {
        let name = session_id.as_str();
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session id is not usable as a directory name",
            ));
        }
        Ok(self.sessions_dir.join(name))
    }

    fn audio_relative_path(kind: TrackKind) -> String {
        format!("{AUDIO_DIR}/{kind}.wav")
    }
}

impl MediaStore for FileMediaStore {
    fn prepare_session(&self, session_id: &SessionId) -> io::Result<()> {
        let dir = self.session_dir(session_id)?;
        fs::create_dir_all(dir.join(AUDIO_DIR))?;
        fs::create_dir_all(dir.join(FRAMES_DIR))
    }

    fn remove_session(&self, session_id: &SessionId) -> io::Result<()> {
        match fs::remove_dir_all(self.session_dir(session_id)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn open_audio_writer(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
        format: AudioFormat,
    ) -> io::Result<Box<dyn AudioWriter>> {
        let dir = self.session_dir(session_id)?.join(AUDIO_DIR);
        fs::create_dir_all(&dir)?;
        let relative = Self::audio_relative_path(kind);
        let path = dir.join(format!("{kind}.wav"));
        Ok(Box::new(WavTrackWriter::create(&path, relative, format)?))
    }

    fn probe_audio(
        &self,
        session_id: &SessionId,
        kind: TrackKind,
    ) -> io::Result<Option<AudioFileInfo>> {
        let path = self
            .session_dir(session_id)?
            .join(AUDIO_DIR)
            .join(format!("{kind}.wav"));
        wav_writer::probe_wav(&path, &Self::audio_relative_path(kind))
    }

    fn save_frame(
        &self,
        session_id: &SessionId,
        timestamp_ms: u64,
        frame: &RawFrame,
    ) -> io::Result<String> {
        let dir = self.session_dir(session_id)?.join(FRAMES_DIR);
        fs::create_dir_all(&dir)?;
        let file_name = format!("frame-{timestamp_ms:09}.png");
        write_png(&dir.join(&file_name), frame)?;
        Ok(format!("{FRAMES_DIR}/{file_name}"))
    }
}

/// Encodes `frame` as PNG (RGB: screens are opaque) and moves it into place atomically, so a
/// crash never leaves a half-written frame under its final name.
fn write_png(path: &Path, frame: &RawFrame) -> io::Result<()> {
    let expected = u64::from(frame.width) * u64::from(frame.height) * 4;
    if frame.width == 0 || frame.height == 0 || frame.rgba.len() as u64 != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame buffer does not match its dimensions",
        ));
    }
    let mut rgb = Vec::with_capacity(frame.rgba.len() / 4 * 3);
    for pixel in frame.rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }

    let temp = path.with_extension("png.tmp");
    let encoded = (|| -> io::Result<()> {
        let file = BufWriter::new(File::create(&temp)?);
        // Fast compression: a 1080p frame encodes in a fraction of a second and the files
        // are short-lived working data, not an archive.
        PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Adaptive)
            .write_image(&rgb, frame.width, frame.height, ExtendedColorType::Rgb8)
            .map_err(io::Error::other)
    })();
    match encoded.and_then(|()| fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn store() -> (tempfile::TempDir, FileMediaStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileMediaStore::new(tmp.path().join("sessions"));
        (tmp, store)
    }

    fn gradient(width: u32, height: u32) -> RawFrame {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let (r, g) = (
                    u8::try_from(x % 256).unwrap(),
                    u8::try_from(y % 256).unwrap(),
                );
                rgba.extend_from_slice(&[r, g, 128, 255]);
            }
        }
        RawFrame {
            captured_at: Instant::now(),
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-capture");
    }

    #[test]
    fn prepare_and_remove_manage_the_session_folders() {
        let (tmp, store) = store();
        let id = SessionId::new("s1");
        store.prepare_session(&id).unwrap();
        assert!(tmp.path().join("sessions/s1/audio").is_dir());
        assert!(tmp.path().join("sessions/s1/frames").is_dir());
        store.remove_session(&id).unwrap();
        assert!(!tmp.path().join("sessions/s1").exists());
        store.remove_session(&id).unwrap(); // idempotent
    }

    #[test]
    fn unsafe_session_ids_are_rejected_everywhere() {
        let (_tmp, store) = store();
        for bad in ["", "..", "../x", "a/b", "a\\b", "c:evil"] {
            let id = SessionId::new(bad);
            assert!(store.prepare_session(&id).is_err(), "{bad:?}");
            assert!(store.remove_session(&id).is_err(), "{bad:?}");
            assert!(store.probe_audio(&id, TrackKind::Microphone).is_err());
            assert!(store.save_frame(&id, 0, &gradient(2, 2)).is_err());
        }
    }

    #[test]
    fn audio_is_written_probed_and_reported_relative_to_the_session() {
        let (tmp, store) = store();
        let id = SessionId::new("s1");
        store.prepare_session(&id).unwrap();
        assert_eq!(
            store.probe_audio(&id, TrackKind::Microphone).unwrap(),
            None,
            "no file yet"
        );

        let format = AudioFormat {
            sample_rate: 16_000,
            channels: 1,
        };
        let mut writer = store
            .open_audio_writer(&id, TrackKind::Microphone, format)
            .unwrap();
        writer.write(&vec![0.1; 16_000]).unwrap();
        let info = writer.finish().unwrap();
        assert_eq!(info.relative_path, "audio/microphone.wav");
        assert_eq!(info.frames, 16_000);
        assert!(tmp
            .path()
            .join("sessions/s1/audio/microphone.wav")
            .is_file());

        let probed = store
            .probe_audio(&id, TrackKind::Microphone)
            .unwrap()
            .unwrap();
        assert_eq!(probed, info);
        assert_eq!(probed.duration_ms(), 1_000);
        assert_eq!(store.probe_audio(&id, TrackKind::System).unwrap(), None);
    }

    #[test]
    fn frames_are_stored_as_png_with_a_relative_path() {
        let (tmp, store) = store();
        let id = SessionId::new("s1");
        store.prepare_session(&id).unwrap();

        let path = store.save_frame(&id, 3_000, &gradient(64, 48)).unwrap();
        assert_eq!(path, "frames/frame-000003000.png");

        let file = tmp.path().join("sessions/s1").join(&path);
        let decoded = image::open(&file).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 48));
        assert_eq!(decoded.get_pixel(10, 20).0, [10, 20, 128]);
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["frame-000003000.png"], "no temp file is left");
    }

    #[test]
    fn a_frame_whose_buffer_does_not_match_its_size_is_rejected() {
        let (_tmp, store) = store();
        let id = SessionId::new("s1");
        store.prepare_session(&id).unwrap();
        let mut frame = gradient(4, 4);
        frame.rgba.truncate(10);
        let err = store.save_frame(&id, 0, &frame).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
