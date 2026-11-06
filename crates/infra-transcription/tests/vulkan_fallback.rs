//! A Vulkan build on a machine without a Vulkan driver must fall back to the CPU, not abort.
//!
//! ggml initialises Vulkan with C++ code that throws when there is no driver; an exception that
//! reaches Rust aborts the whole process ("Rust cannot catch foreign exceptions"). The driver
//! is hidden from the Vulkan loader with an empty driver list, which needs no GPU-less machine:
//! the test runs wherever the Vulkan loader library exists. Everything lives in one test
//! because the loader reads the environment once, when Vulkan is first initialised.
#![cfg(feature = "vulkan")]

use tan_application::transcription::LocalTranscriber;
use tan_infra_transcription::{detect_backend, WhisperTranscriber};

#[tokio::test]
async fn without_a_vulkan_driver_everything_runs_on_the_cpu() {
    // `VK_DRIVER_FILES` replaced `VK_ICD_FILENAMES` in newer loaders; set both.
    let nowhere = std::env::temp_dir().join("tan-no-such-vulkan-driver.json");
    std::env::set_var("VK_DRIVER_FILES", &nowhere);
    std::env::set_var("VK_ICD_FILENAMES", &nowhere);

    let backend = detect_backend();
    assert!(!backend.use_gpu, "{backend:?}");
    assert_eq!(backend.label, "cpu");
    // Asking again must not touch Vulkan again.
    assert_eq!(detect_backend(), backend);

    // With a real model (`TAN_WHISPER_MODEL`): loading and calibrating also work, and report
    // the CPU.
    if let Some(model) = std::env::var_os("TAN_WHISPER_MODEL") {
        let engine = WhisperTranscriber::with_model_file(model);
        let speed = engine.speed().await.unwrap();
        assert_eq!(speed.backend, "cpu");
        assert!(speed.seconds_per_window > 0.0);
    }
}
