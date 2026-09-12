use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_text::default_true;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    ComponentRevision, EmbeddingDocumentCompositionV1, ManifestDigest, host_cpu_target,
};

use crate::manifest::ResourceCeilingV1;

/// Catalog ids of the shipped embedding models. These name the catalog
/// entries; whether a `selected_model` is actually cataloged is decided by
/// the production catalog in `tracedecay-semantic`, not here — settings
/// validation is provider-free and structural only.
pub const DEFAULT_FASTEMBED_MODEL_ID: &str = "JinaEmbeddingsV2BaseCode";
/// Catalog id of the Model2Vec static code-embedding model
/// (`minishlab/potion-code-16M-v2`, 256 dimensions, no ONNX Runtime).
pub const MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID: &str = "PotionCode16MV2";

const MAX_SEMANTIC_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_TOKENIZER_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_SEMANTIC_RESIDENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const DEFAULT_SEMANTIC_RESIDENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_THREADS: u32 = 64;
const MAX_SEMANTIC_CONCURRENT_SESSIONS: u32 = 64;
const MAX_SEMANTIC_BATCH_SIZE: u32 = 4096;
const MAX_SEMANTIC_SEQUENCE_LENGTH: u32 = 32768;
const MAX_SEMANTIC_LOAD_DEADLINE_MS: u64 = 10 * 60 * 1000;
const DEFAULT_INTRA_THREADS: u32 = 12;
const BASELINE_INTRA_THREADS: usize = 4;

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticResourceCeilings {
    pub max_model_bytes: u64,
    pub max_tokenizer_bytes: u64,
    /// The resident-memory ceiling the operator pinned, if they pinned one.
    ///
    /// `None` is a value, not a missing field: it says the operator declined
    /// to choose, so the host's own admitted process memory decides
    /// (`embedding_parallelism::effective_resident_ceiling`). Composition
    /// resolves it to `Some` once, before any runtime reads it. A struct
    /// default here would be indistinguishable from a deliberate 2 GiB pin,
    /// which is exactly how the host-derived ceiling became unreachable.
    #[serde(default)]
    pub max_resident_bytes: Option<u64>,
    pub max_threads: u32,
    pub max_concurrent_sessions: u32,
    pub max_batch_size: u32,
    pub max_sequence_length: u32,
    pub load_deadline_ms: u64,
}

impl Default for SemanticResourceCeilings {
    fn default() -> Self {
        let total_cores = host_cpu_target(usize::MAX);
        let shared_cpu_budget = if total_cores <= 8 {
            total_cores
        } else {
            total_cores / 2
        };
        Self {
            max_model_bytes: 700 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: None,
            max_threads: u32::try_from(total_cores.max(1))
                .unwrap_or(u32::MAX)
                .min(DEFAULT_INTRA_THREADS),
            max_concurrent_sessions: u32::try_from(
                (shared_cpu_budget / BASELINE_INTRA_THREADS).max(1),
            )
            .unwrap_or(1),
            max_batch_size: 32,
            max_sequence_length: 4096,
            load_deadline_ms: 30_000,
        }
    }
}

impl SemanticResourceCeilings {
    /// The resident ceiling in force, for a configuration that has already
    /// been composed against its host.
    ///
    /// `None` here is not "the shipped default": it is a configuration that
    /// never passed through `effective_resident_ceiling`, so the only honest
    /// answer is a typed failure rather than an invented number.
    pub fn resolved_max_resident_bytes(&self) -> Result<u64> {
        self.max_resident_bytes.ok_or_else(|| {
            config_error(
                "semantic resident ceiling was read before composition resolved it against the host",
            )
        })
    }
}

/// A manifest ceiling needs every bound pinned, so this bridge carries the
/// same typed failure as [`SemanticResourceCeilings::resolved_max_resident_bytes`]
/// for a configuration that was never composed against its host.
impl TryFrom<SemanticResourceCeilings> for ResourceCeilingV1 {
    type Error = TraceDecayError;

    fn try_from(ceilings: SemanticResourceCeilings) -> Result<Self> {
        Ok(Self {
            max_model_bytes: ceilings.max_model_bytes,
            max_tokenizer_bytes: ceilings.max_tokenizer_bytes,
            max_resident_bytes: ceilings.resolved_max_resident_bytes()?,
            max_threads: ceilings.max_threads,
            max_batch_size: ceilings.max_batch_size,
            max_sequence_length: ceilings.max_sequence_length,
            load_deadline_ms: ceilings.load_deadline_ms,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankCompatibilityPinsV1 {
    pub implementation_revision: ComponentRevision,
    pub artifact_manifest_digest: ManifestDigest,
    pub runtime_compatibility_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticFallbackReasonV1 {
    ConfigurationUnavailable,
    RuntimeUnavailable,
    ArtifactUnavailable,
    IncompatibleRuntime,
    ResourceCeilingExceeded,
    CorruptArtifact,
    Indexing,
    RuntimeFailure,
    RollbackInProgress,
    /// A vector generation is current but no durable activation receipt has
    /// ever been issued for it. This is the ordinary pre-activation state of a
    /// fresh profile, not a malformed runtime status: the operator's next step
    /// is `tracedecay semantic activate`.
    NotActivated,
    InvalidRuntimeStatus,
    SelectedNotDownloaded,
    Downloading,
    Verifying,
    Loading,
    ModelFailed,
    Stale,
    ResetRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProfileSelection {
    pub profile_id: String,
    pub accepted_profile_digest: ManifestDigest,
    pub artifact_digest: String,
    pub artifact_path: PathBuf,
}

impl SemanticProfileSelection {
    fn validate(&self) -> Result<()> {
        self.accepted_profile_digest
            .validate()
            .map_err(|error| config_error(format!("semantic accepted profile digest: {error}")))?;
        if self.profile_id.trim().is_empty() || self.profile_id.len() > 128 {
            return Err(config_error(
                "semantic profile_id must be non-empty and at most 128 bytes",
            ));
        }
        if self.artifact_digest.len() != 64
            || !self
                .artifact_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(config_error(
                "semantic artifact_digest must be 64 lowercase hexadecimal characters",
            ));
        }
        if !self.artifact_path.is_absolute()
            || self
                .artifact_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(config_error(
                "semantic artifact_path must be an absolute normalized local path",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    #[serde(default = "default_selected_fastembed_model")]
    pub selected_model: Option<String>,
    #[serde(default = "default_true")]
    pub auto_download: bool,
    #[serde(default)]
    pub active_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub rollback_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub resources: SemanticResourceCeilings,
    /// How each chunk's embedding input is composed. Projection identity: a
    /// change re-projects every generation under a new projection key, so the
    /// header composition stays a measured candidate until the search-quality
    /// harness has compared it against `SanitizedText`.
    #[serde(default)]
    pub document_composition: EmbeddingDocumentCompositionV1,
}

fn default_selected_fastembed_model() -> Option<String> {
    Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned())
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            selected_model: default_selected_fastembed_model(),
            auto_download: true,
            active_profile: None,
            rollback_profile: None,
            resources: SemanticResourceCeilings::default(),
            document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
        }
    }
}

impl SemanticConfig {
    /// Structural validation only: model-id shape, resource ceilings, profile
    /// selection shape, and rollback invariants. Catalog membership of
    /// `selected_model` is admitted where the catalog is declared
    /// (`tracedecay-semantic`), so adding a model edits one declaration.
    pub fn validate(&self) -> Result<()> {
        validate_semantic_resource_ceilings(self.resources)?;
        if let Some(model_id) = self.selected_model.as_ref()
            && (model_id.trim().is_empty() || model_id.len() > 128)
        {
            return Err(config_error(
                "semantic selected_model must be a non-empty catalog id at most 128 bytes",
            ));
        }
        if let Some(active) = self.active_profile.as_ref() {
            active.validate()?;
        }
        if let Some(rollback) = self.rollback_profile.as_ref() {
            rollback.validate()?;
            if self.active_profile.is_none() {
                return Err(config_error(
                    "semantic rollback profile requires an active profile",
                ));
            }
        }
        if self.active_profile == self.rollback_profile && self.active_profile.is_some() {
            return Err(config_error(
                "semantic active and rollback profiles must be distinct",
            ));
        }
        Ok(())
    }
}

/// Whether one pinned resident ceiling is coherent with the model and
/// tokenizer ceilings it must hold at once.
///
/// Shared with the host derivation so a derived ceiling is measured against
/// exactly the invariant an operator's ceiling must satisfy, instead of
/// bypassing it.
#[must_use]
pub fn semantic_resident_ceiling_is_valid(
    ceilings: SemanticResourceCeilings,
    ceiling: u64,
) -> bool {
    ceiling > 0
        && ceiling <= MAX_SEMANTIC_RESIDENT_BYTES
        && ceilings.max_model_bytes <= ceiling
        && ceilings.max_tokenizer_bytes <= ceiling
}

fn validate_semantic_resource_ceilings(ceilings: SemanticResourceCeilings) -> Result<()> {
    let valid = ceilings.max_model_bytes > 0
        && ceilings.max_model_bytes <= MAX_SEMANTIC_MODEL_BYTES
        && ceilings.max_tokenizer_bytes > 0
        && ceilings.max_tokenizer_bytes <= MAX_SEMANTIC_TOKENIZER_BYTES
        // `None` is the operator declining to pin a ceiling, which composition
        // resolves against the host; only a pinned value is validated here.
        && ceilings
            .max_resident_bytes
            .is_none_or(|ceiling| semantic_resident_ceiling_is_valid(ceilings, ceiling))
        && (1..=MAX_SEMANTIC_THREADS).contains(&ceilings.max_threads)
        && (1..=MAX_SEMANTIC_CONCURRENT_SESSIONS).contains(&ceilings.max_concurrent_sessions)
        && (1..=MAX_SEMANTIC_BATCH_SIZE).contains(&ceilings.max_batch_size)
        && (1..=MAX_SEMANTIC_SEQUENCE_LENGTH).contains(&ceilings.max_sequence_length)
        && (1..=MAX_SEMANTIC_LOAD_DEADLINE_MS).contains(&ceilings.load_deadline_ms);
    if !valid {
        return Err(config_error(
            "semantic resource ceilings are zero, incoherent, or exceed supported maxima",
        ));
    }
    Ok(())
}
