//! Embedding model configuration and pre-configured model specifications.

use std::path::PathBuf;

/// Known model specification for preset variants.
struct ModelSpec {
    repo_id: &'static str,
    model_file: &'static str,
    tokenizer_file: &'static str,
    expected_dimensions: usize,
    display_name: &'static str,
}

const MINILM_L6_V2: ModelSpec = ModelSpec {
    repo_id: "sentence-transformers/all-MiniLM-L6-v2",
    model_file: "onnx/model.onnx",
    tokenizer_file: "tokenizer.json",
    expected_dimensions: 384,
    display_name: "all-MiniLM-L6-v2",
};

const MINILM_L12_V2: ModelSpec = ModelSpec {
    repo_id: "sentence-transformers/all-MiniLM-L12-v2",
    model_file: "onnx/model.onnx",
    tokenizer_file: "tokenizer.json",
    expected_dimensions: 384,
    display_name: "all-MiniLM-L12-v2",
};

const BGE_SMALL_EN_V15: ModelSpec = ModelSpec {
    repo_id: "BAAI/bge-small-en-v1.5",
    model_file: "onnx/model.onnx",
    tokenizer_file: "tokenizer.json",
    expected_dimensions: 384,
    display_name: "bge-small-en-v1.5",
};

/// Pre-configured embedding model specifications.
///
/// Use the preset variants for common models that are automatically
/// downloaded from HuggingFace Hub on first use. Use [`Local`](Self::Local)
/// for your own ONNX models already on disk, or [`HuggingFace`](Self::HuggingFace)
/// for any HuggingFace-hosted model.
///
/// # Examples
///
/// ```no_run
/// use grafeo_engine::embedding::{EmbeddingModelConfig, OnnxEmbeddingModel};
///
/// # fn main() -> grafeo_common::utils::error::Result<()> {
/// // Preset: auto-downloads on first use
/// let model = OnnxEmbeddingModel::from_config(EmbeddingModelConfig::MiniLmL6v2)?;
///
/// // Local: use your own ONNX model files
/// let model = OnnxEmbeddingModel::from_config(EmbeddingModelConfig::Local {
///     model_path: "path/to/model.onnx".into(),
///     tokenizer_path: "path/to/tokenizer.json".into(),
/// })?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EmbeddingModelConfig {
    /// all-MiniLM-L6-v2 (384 dims, ~23MB, fast).
    MiniLmL6v2,
    /// all-MiniLM-L12-v2 (384 dims, ~33MB, better quality).
    MiniLmL12v2,
    /// BGE-small-en-v1.5 (384 dims, ~33MB, popular alternative).
    BgeSmallEnV15,
    /// Custom model from a HuggingFace repository.
    HuggingFace {
        /// Repository ID (e.g., `"sentence-transformers/all-MiniLM-L6-v2"`).
        repo_id: String,
        /// Path within the repo to the ONNX model file (e.g., `"onnx/model.onnx"`).
        model_file: String,
        /// Path within the repo to the tokenizer file (e.g., `"tokenizer.json"`).
        tokenizer_file: String,
    },
    /// Custom model from local file paths.
    Local {
        /// Path to the `.onnx` model file.
        model_path: PathBuf,
        /// Path to the `tokenizer.json` file.
        tokenizer_path: PathBuf,
    },
}

impl EmbeddingModelConfig {
    /// Returns the expected embedding dimensionality for preset models.
    ///
    /// Returns `None` for custom or HuggingFace models where dimensions
    /// are probed at load time.
    #[must_use]
    pub fn expected_dimensions(&self) -> Option<usize> {
        self.model_spec().map(|s| s.expected_dimensions)
    }

    /// Returns a human-readable name for this model configuration.
    ///
    /// For preset models, returns the standard model name (e.g., `"all-MiniLM-L6-v2"`).
    /// This name is used as the registry key in [`GrafeoDB::load_embedding_model()`](crate::GrafeoDB::load_embedding_model).
    #[must_use]
    pub fn display_name(&self) -> String {
        match self {
            Self::MiniLmL6v2 => MINILM_L6_V2.display_name.to_string(),
            Self::MiniLmL12v2 => MINILM_L12_V2.display_name.to_string(),
            Self::BgeSmallEnV15 => BGE_SMALL_EN_V15.display_name.to_string(),
            Self::HuggingFace { repo_id, .. } => repo_id.clone(),
            Self::Local {
                model_path: path, ..
            } => path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom-model")
                .to_string(),
        }
    }

    /// Returns the internal model spec for preset variants.
    fn model_spec(&self) -> Option<&'static ModelSpec> {
        match self {
            Self::MiniLmL6v2 => Some(&MINILM_L6_V2),
            Self::MiniLmL12v2 => Some(&MINILM_L12_V2),
            Self::BgeSmallEnV15 => Some(&BGE_SMALL_EN_V15),
            _ => None,
        }
    }

    /// Returns the HuggingFace repo ID and file paths for download resolution.
    pub(crate) fn resolve_info(&self) -> ResolveInfo<'_> {
        match self {
            Self::MiniLmL6v2 => {
                let spec = &MINILM_L6_V2;
                ResolveInfo::Hub {
                    repo_id: spec.repo_id,
                    model_file: spec.model_file,
                    tokenizer_file: spec.tokenizer_file,
                }
            }
            Self::MiniLmL12v2 => {
                let spec = &MINILM_L12_V2;
                ResolveInfo::Hub {
                    repo_id: spec.repo_id,
                    model_file: spec.model_file,
                    tokenizer_file: spec.tokenizer_file,
                }
            }
            Self::BgeSmallEnV15 => {
                let spec = &BGE_SMALL_EN_V15;
                ResolveInfo::Hub {
                    repo_id: spec.repo_id,
                    model_file: spec.model_file,
                    tokenizer_file: spec.tokenizer_file,
                }
            }
            Self::HuggingFace {
                repo_id,
                model_file,
                tokenizer_file,
            } => ResolveInfo::Hub {
                repo_id,
                model_file,
                tokenizer_file,
            },
            Self::Local {
                model_path,
                tokenizer_path,
            } => ResolveInfo::Local {
                model_path,
                tokenizer_path,
            },
        }
    }
}

/// Internal enum for download resolution dispatch.
pub(crate) enum ResolveInfo<'a> {
    Hub {
        repo_id: &'a str,
        model_file: &'a str,
        tokenizer_file: &'a str,
    },
    Local {
        model_path: &'a PathBuf,
        tokenizer_path: &'a PathBuf,
    },
}

/// Options for embedding model loading and inference.
///
/// Use the builder methods to customize batch size and thread configuration.
/// Defaults are tuned for single-user workloads on CPU.
///
/// # Examples
///
/// ```no_run
/// use grafeo_engine::embedding::EmbeddingOptions;
///
/// let options = EmbeddingOptions::new()
///     .with_batch_size(64)
///     .with_intra_threads(4);
/// ```
#[derive(Debug, Clone)]
pub struct EmbeddingOptions {
    /// Maximum batch size for embedding generation.
    pub batch_size: usize,
    /// Number of ONNX intra-op threads (parallelism within a single operation).
    pub intra_threads: usize,
    /// Number of ONNX inter-op threads (parallelism across operations).
    pub inter_threads: usize,
}

impl Default for EmbeddingOptions {
    fn default() -> Self {
        Self {
            batch_size: 32,
            intra_threads: 1,
            inter_threads: 1,
        }
    }
}

impl EmbeddingOptions {
    /// Creates default embedding options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum batch size for embedding generation.
    #[must_use]
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Sets the number of ONNX intra-op threads.
    ///
    /// Controls parallelism within individual ONNX operations (e.g., matrix
    /// multiplication). Higher values help with larger models or batch sizes.
    #[must_use]
    pub fn with_intra_threads(mut self, threads: usize) -> Self {
        self.intra_threads = threads;
        self
    }

    /// Sets the number of ONNX inter-op threads.
    ///
    /// Controls parallelism across independent ONNX operations. Usually
    /// less impactful than intra-op threads for embedding models.
    #[must_use]
    pub fn with_inter_threads(mut self, threads: usize) -> Self {
        self.inter_threads = threads;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_expected_dimensions() {
        assert_eq!(
            EmbeddingModelConfig::MiniLmL6v2.expected_dimensions(),
            Some(384)
        );
        assert_eq!(
            EmbeddingModelConfig::MiniLmL12v2.expected_dimensions(),
            Some(384)
        );
        assert_eq!(
            EmbeddingModelConfig::BgeSmallEnV15.expected_dimensions(),
            Some(384)
        );
    }

    #[test]
    fn custom_has_no_expected_dimensions() {
        let local = EmbeddingModelConfig::Local {
            model_path: "model.onnx".into(),
            tokenizer_path: "tokenizer.json".into(),
        };
        assert_eq!(local.expected_dimensions(), None);

        let hf = EmbeddingModelConfig::HuggingFace {
            repo_id: "org/model".into(),
            model_file: "model.onnx".into(),
            tokenizer_file: "tokenizer.json".into(),
        };
        assert_eq!(hf.expected_dimensions(), None);
    }

    #[test]
    fn preset_display_names() {
        assert_eq!(
            EmbeddingModelConfig::MiniLmL6v2.display_name(),
            "all-MiniLM-L6-v2"
        );
        assert_eq!(
            EmbeddingModelConfig::MiniLmL12v2.display_name(),
            "all-MiniLM-L12-v2"
        );
        assert_eq!(
            EmbeddingModelConfig::BgeSmallEnV15.display_name(),
            "bge-small-en-v1.5"
        );
    }

    #[test]
    fn huggingface_display_name_is_repo_id() {
        let config = EmbeddingModelConfig::HuggingFace {
            repo_id: "org/my-model".into(),
            model_file: "model.onnx".into(),
            tokenizer_file: "tokenizer.json".into(),
        };
        assert_eq!(config.display_name(), "org/my-model");
    }

    #[test]
    fn local_display_name_from_file_stem() {
        let config = EmbeddingModelConfig::Local {
            model_path: "/path/to/my-model.onnx".into(),
            tokenizer_path: "/path/to/tokenizer.json".into(),
        };
        assert_eq!(config.display_name(), "my-model");
    }

    #[test]
    fn options_default_values() {
        let opts = EmbeddingOptions::default();
        assert_eq!(opts.batch_size, 32);
        assert_eq!(opts.intra_threads, 1);
        assert_eq!(opts.inter_threads, 1);
    }

    #[test]
    fn options_builder_chaining() {
        let opts = EmbeddingOptions::new()
            .with_batch_size(64)
            .with_intra_threads(4)
            .with_inter_threads(2);
        assert_eq!(opts.batch_size, 64);
        assert_eq!(opts.intra_threads, 4);
        assert_eq!(opts.inter_threads, 2);
    }
}
