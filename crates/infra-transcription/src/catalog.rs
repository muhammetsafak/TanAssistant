//! The whisper models Tan knows how to download.
//!
//! Source: the official `ggerganov/whisper.cpp` repository on Hugging Face (the ggml
//! conversions the whisper.cpp project publishes). URLs are pinned to a repository revision so
//! the bytes behind a URL cannot change, and every entry carries the SHA-256 the repository's
//! LFS metadata lists for the file; a download is only accepted when its hash matches.
//!
//! Only multilingual models are listed (the product is Turkish first, ADR-005). The order is
//! the preference order: [`crate::ModelManager::default_installed`] picks the first installed
//! entry.

/// Repository revision the URLs are pinned to.
const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

macro_rules! model_url {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/",
            $file
        )
    };
}

/// One downloadable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// Stable identifier (used over IPC and for the file name on disk).
    pub id: &'static str,
    /// Name shown to the user.
    pub display_name: &'static str,
    /// File name inside the models directory.
    pub file_name: &'static str,
    /// Download URL.
    pub url: &'static str,
    /// Exact file size in bytes.
    pub size_bytes: u64,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: &'static str,
    /// Quantization of the weights (`q5_0`, `q8_0`, ... or `f16`).
    pub quantization: &'static str,
    /// The recommended default for Turkish.
    pub recommended: bool,
}

/// The built-in catalog, best choice first.
pub const BUILTIN_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "large-v3-turbo-q5_0",
        display_name: "Large v3 Turbo (q5_0)",
        file_name: "ggml-large-v3-turbo-q5_0.bin",
        url: model_url!("ggml-large-v3-turbo-q5_0.bin"),
        size_bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        quantization: "q5_0",
        recommended: true,
    },
    ModelSpec {
        id: "large-v3-turbo-q8_0",
        display_name: "Large v3 Turbo (q8_0)",
        file_name: "ggml-large-v3-turbo-q8_0.bin",
        url: model_url!("ggml-large-v3-turbo-q8_0.bin"),
        size_bytes: 874_188_075,
        sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1",
        quantization: "q8_0",
        recommended: false,
    },
    ModelSpec {
        id: "large-v3-q5_0",
        display_name: "Large v3 (q5_0)",
        file_name: "ggml-large-v3-q5_0.bin",
        url: model_url!("ggml-large-v3-q5_0.bin"),
        size_bytes: 1_081_140_203,
        sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
        quantization: "q5_0",
        recommended: false,
    },
    ModelSpec {
        id: "medium-q5_0",
        display_name: "Medium (q5_0)",
        file_name: "ggml-medium-q5_0.bin",
        url: model_url!("ggml-medium-q5_0.bin"),
        size_bytes: 539_212_467,
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        quantization: "q5_0",
        recommended: false,
    },
    ModelSpec {
        id: "small-q5_1",
        display_name: "Small (q5_1)",
        file_name: "ggml-small-q5_1.bin",
        url: model_url!("ggml-small-q5_1.bin"),
        size_bytes: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        quantization: "q5_1",
        recommended: false,
    },
    ModelSpec {
        id: "base-q5_1",
        display_name: "Base (q5_1)",
        file_name: "ggml-base-q5_1.bin",
        url: model_url!("ggml-base-q5_1.bin"),
        size_bytes: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
        quantization: "q5_1",
        recommended: false,
    },
    ModelSpec {
        id: "tiny-q5_1",
        display_name: "Tiny (q5_1)",
        file_name: "ggml-tiny-q5_1.bin",
        url: model_url!("ggml-tiny-q5_1.bin"),
        size_bytes: 32_152_673,
        sha256: "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7",
        quantization: "q5_1",
        recommended: false,
    },
];

/// The model recommended for Turkish meetings.
pub const RECOMMENDED_MODEL_ID: &str = "large-v3-turbo-q5_0";

/// The pinned repository revision (for documentation and tests).
#[must_use]
pub const fn pinned_revision() -> &'static str {
    REVISION
}

#[cfg(test)]
mod tests {
    use super::{pinned_revision, BUILTIN_MODELS, RECOMMENDED_MODEL_ID};
    use std::collections::HashSet;

    #[test]
    fn ids_and_file_names_are_unique_and_safe_path_segments() {
        let ids: HashSet<_> = BUILTIN_MODELS.iter().map(|m| m.id).collect();
        let files: HashSet<_> = BUILTIN_MODELS.iter().map(|m| m.file_name).collect();
        assert_eq!(ids.len(), BUILTIN_MODELS.len());
        assert_eq!(files.len(), BUILTIN_MODELS.len());
        for model in BUILTIN_MODELS {
            assert!(
                model
                    .file_name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{}",
                model.file_name
            );
            assert!(!model.file_name.contains(".."));
        }
    }

    #[test]
    fn every_entry_is_pinned_and_checksummed() {
        for model in BUILTIN_MODELS {
            assert!(model
                .url
                .starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/"));
            assert!(model.url.contains(pinned_revision()), "{}", model.id);
            assert!(model.url.ends_with(model.file_name), "{}", model.id);
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
            assert!(
                model
                    .sha256
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{}",
                model.id
            );
            assert!(model.size_bytes > 0);
        }
    }

    #[test]
    fn exactly_one_model_is_recommended_and_it_comes_first() {
        let recommended: Vec<_> = BUILTIN_MODELS.iter().filter(|m| m.recommended).collect();
        assert_eq!(recommended.len(), 1);
        assert_eq!(recommended[0].id, RECOMMENDED_MODEL_ID);
        assert_eq!(BUILTIN_MODELS[0].id, RECOMMENDED_MODEL_ID);
    }
}
