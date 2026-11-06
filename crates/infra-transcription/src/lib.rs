//! Transcription adapter: local whisper (whisper.cpp) and the model manager.
//!
//! - [`WhisperTranscriber`] implements the `TranscriptionModel` and `LocalTranscriber` ports.
//! - [`ModelManager`] lists, downloads (checksum-verified, resumable) and deletes models.
//! - [`audio_prep`] reads WAV files, resamples to 16 kHz mono and cuts long recordings.
//! - [`vad`] finds the speech so silence never reaches whisper (ADR-011).
//!
//! GPU backends are features: `vulkan` (Windows, Linux), `metal` (macOS) and `cuda`. A build
//! with one uses the GPU when the machine has one and falls back to the CPU otherwise; a build
//! without any is CPU only.

pub mod audio_prep;
pub mod catalog;
pub mod models;
pub mod vad;
pub mod whisper;

pub use audio_prep::{SessionFiles, WavAudioChunker};
pub use catalog::{ModelSpec, BUILTIN_MODELS, RECOMMENDED_MODEL_ID};
pub use models::{DownloadProgress, InstalledModel, ModelError, ModelInfo, ModelManager};
pub use whisper::{detect_backend, Backend, WhisperTranscriber};

/// Name of the architectural layer this crate implements.
pub const LAYER: &str = "infra-transcription";

#[cfg(test)]
mod tests {
    use super::LAYER;

    #[test]
    fn layer_name_is_set() {
        assert_eq!(LAYER, "infra-transcription");
    }
}
