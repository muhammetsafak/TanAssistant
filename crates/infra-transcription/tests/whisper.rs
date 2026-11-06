//! The local whisper engine.
//!
//! Without a model these tests cover the "model missing" behaviour. With
//! `--features model-tests` and `TAN_WHISPER_MODEL=<path to a ggml model>` they run real
//! speech through whisper (a Turkish and an English fixture, spoken by Windows text-to-speech
//! voices, generated for this repository).

use std::sync::Arc;

use tan_application::ports::{AiError, AudioSource, TranscriptionModel, TranscriptionRequest};
use tan_application::transcription::{LocalEngineStatus, LocalTranscriber};
use tan_infra_transcription::{ModelManager, WhisperTranscriber};

fn request(path: &std::path::Path) -> TranscriptionRequest {
    TranscriptionRequest {
        audio: AudioSource::Path(path.to_path_buf()),
        language: None,
        duration_ms: None,
        prompt: None,
    }
}

#[tokio::test]
async fn without_a_model_the_engine_reports_it_and_refuses_to_transcribe() {
    let dir = tempfile::tempdir().unwrap();
    let models = Arc::new(ModelManager::new(dir.path()).unwrap());
    let engine = WhisperTranscriber::new(models);

    assert_eq!(engine.status().await, LocalEngineStatus::ModelMissing);
    assert!(engine.model_path().is_none());
    let error = engine
        .transcribe(request(&dir.path().join("whatever.wav")))
        .await
        .unwrap_err();
    assert!(matches!(error, AiError::Unsupported(_)), "{error:?}");
}

#[tokio::test]
async fn a_model_path_that_does_not_exist_counts_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = WhisperTranscriber::with_model_file(dir.path().join("ggml-nope.bin"));
    assert_eq!(engine.status().await, LocalEngineStatus::ModelMissing);
}

#[cfg(feature = "model-tests")]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test audio maths
mod with_model {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
    use tan_application::ports::{AudioSource, TranscriptionModel, TranscriptionRequest};
    use tan_application::transcription::{LocalEngineStatus, LocalTranscriber};
    use tan_domain::LanguageCode;
    use tan_infra_transcription::WhisperTranscriber;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// The model to test with, or `None` (the test then does nothing) when none is configured.
    ///
    /// One engine is shared by all tests: the model is loaded once, and its internal lock
    /// keeps the tests from running inference concurrently (whisper's worker threads spin, so
    /// parallel runs slow each other down badly).
    fn engine() -> Option<WhisperTranscriber> {
        static ENGINE: OnceLock<Option<WhisperTranscriber>> = OnceLock::new();
        ENGINE
            .get_or_init(|| {
                let Some(path) = std::env::var_os("TAN_WHISPER_MODEL") else {
                    eprintln!("TAN_WHISPER_MODEL is not set: skipping the model tests");
                    return None;
                };
                Some(WhisperTranscriber::with_model_file(path))
            })
            .clone()
    }

    async fn text_of(
        engine: &WhisperTranscriber,
        audio: AudioSource,
        language: Option<LanguageCode>,
    ) -> (String, Vec<tan_application::ports::TranscriptSegmentDto>) {
        let segments = engine
            .transcribe(TranscriptionRequest {
                audio,
                language,
                duration_ms: None,
                prompt: None,
            })
            .await
            .unwrap();
        let text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        (text, segments)
    }

    #[tokio::test]
    async fn turkish_speech_contains_the_expected_keyword() {
        let Some(engine) = engine() else { return };
        assert_eq!(engine.status().await, LocalEngineStatus::Ready);

        let (text, segments) = text_of(
            &engine,
            AudioSource::Path(fixture("speech_tr.wav")),
            Some(LanguageCode::turkish()),
        )
        .await;

        println!("turkish: {text:?}");
        assert!(text.contains("toplant"), "{text}");
        assert_ne!(segments.len(), 0);
        for segment in &segments {
            assert!(segment.end_ms >= segment.start_ms);
            assert!(segment.end_ms <= 8_000, "fixture is 6.6 s: {segment:?}");
            assert_eq!(segment.language, Some(LanguageCode::turkish()));
        }
    }

    #[tokio::test]
    async fn english_speech_contains_the_expected_keywords() {
        let Some(engine) = engine() else { return };

        let (text, _) = text_of(
            &engine,
            AudioSource::Path(fixture("speech_en.wav")),
            Some(LanguageCode::english()),
        )
        .await;

        println!("english: {text:?}");
        assert!(text.contains("budget"), "{text}");
        assert!(text.contains("meeting"), "{text}");
    }

    #[tokio::test]
    async fn auto_detection_finds_the_spoken_language() {
        let Some(engine) = engine() else { return };

        let (_, tr) = text_of(&engine, AudioSource::Path(fixture("speech_tr.wav")), None).await;
        let (_, en) = text_of(&engine, AudioSource::Path(fixture("speech_en.wav")), None).await;

        assert_eq!(tr[0].language, Some(LanguageCode::turkish()), "{tr:?}");
        assert_eq!(en[0].language, Some(LanguageCode::english()), "{en:?}");
    }

    /// Recordings are stored at the device rate, usually 48 kHz stereo: the whole path
    /// (decode, mix down, resample) must still recognise the speech.
    #[tokio::test]
    async fn native_48khz_stereo_audio_is_transcribed_through_the_resampler() {
        let Some(engine) = engine() else { return };
        let mut reader = WavReader::open(fixture("speech_tr.wav")).unwrap();
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        let spec = WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            // Linear 3x upsampling, same signal on both channels.
            for pair in samples.windows(2) {
                let (a, b) = (i32::from(pair[0]), i32::from(pair[1]));
                for step in 0..3 {
                    let value = (a + (b - a) * step / 3) as i16;
                    writer.write_sample(value).unwrap();
                    writer.write_sample(value).unwrap();
                }
            }
            writer.finalize().unwrap();
        }

        let (text, _) = text_of(
            &engine,
            AudioSource::Bytes {
                file_name: "native.wav".to_owned(),
                mime_type: "audio/wav".to_owned(),
                data: cursor.into_inner(),
            },
            Some(LanguageCode::turkish()),
        )
        .await;

        println!("turkish 48k stereo: {text:?}");
        assert!(text.contains("toplant"), "{text}");
    }

    /// Not a regression test: prints how fast this machine transcribes (`--release` and
    /// `-- --ignored --nocapture`). A two-minute track of speech and silence in equal parts,
    /// with silence skipping on and off, and the calibrated speed the forecast uses.
    #[tokio::test]
    #[ignore = "a measurement, not a test"]
    async fn measure_speed() {
        let Some(engine) = engine() else { return };
        let mut reader = WavReader::open(fixture("speech_tr.wav")).unwrap();
        let speech: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        let mut audio = Vec::new();
        for _ in 0..9 {
            audio.extend_from_slice(&speech);
            audio.extend(std::iter::repeat_n(0_i16, speech.len()));
        }
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            for sample in &audio {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let wav = cursor.into_inner();
        let audio_seconds = audio.len() as f64 / 16_000.0;
        let speed = engine.speed().await.unwrap();
        println!(
            "backend: {}; calibrated {:.2} s per 30 s window",
            speed.backend, speed.seconds_per_window
        );
        for vad in [false, true] {
            engine.set_vad(vad);
            let started = std::time::Instant::now();
            let (text, _) = text_of(
                &engine,
                AudioSource::Bytes {
                    file_name: "measure.wav".to_owned(),
                    mime_type: "audio/wav".to_owned(),
                    data: wav.clone(),
                },
                Some(LanguageCode::turkish()),
            )
            .await;
            let took = started.elapsed().as_secs_f64();
            println!(
                "vad {}: {audio_seconds:.0} s of audio in {took:.1} s -> RTF {:.2} ({} words)",
                if vad { "on " } else { "off" },
                took / audio_seconds,
                text.split_whitespace().count()
            );
        }
    }
}
