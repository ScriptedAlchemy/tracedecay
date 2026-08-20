//! semantic `FastEmbed` semantic adapter
//! (Plan 31, `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md`).
//!
//! Root-private embedding runtime port surface. This file owns the typed
//! `EmbeddingRuntime` / `EmbeddingSession` ports: load a verified artifact,
//! create bounded sessions, embed bounded sanitized text batches, and return
//! typed vectors whose dimensions, metric, and normalization are echoed from
//! the verified manifest descriptor.
//!
//! The production adapter is feature-gated so dependency activation remains a
//! deliberate package decision. It initializes `FastEmbed` only from bytes
//! opened through the digest-addressed artifact-store capability; it never
//! selects a catalog model, imports an artifact, or discovers a cache.
//!
//! Design decisions for this port, where Plan 31 leaves the choice open:
//!
//! - Sync vs async: Plan 31 does not state whether the runtime port is async.
//!   FastEmbed/ORT inference is blocking CPU work, and query fallback's
//!   `src/query/retrieval/ports.rs` sets the precedent that ports are
//!   synchronous contracts with scheduling/cancellation above them. This port
//!   is therefore synchronous; async wrapping is an integration concern.
//! - Manifest/domain vocabulary: the artifact manifest and domain projection
//!   key remain separate authorities. Exhaustive bridge matches below admit
//!   them into one private projection-artifact authority; the runtime defines
//!   no duplicate metric/normalization/precision enums.
//! - Projection identity: sessions and runtime descriptors are created only
//!   from the admitted projection-artifact authority. Callers cannot pair an
//!   independent projection identity with an artifact.
//! - Budget type: Plan 31 says deadline/cancellation limits are fields of the
//!   shared query `RetrievalBudget` and semantic introduces no semantic-only budget
//!   type. That domain type is outside this root-private module, so deadlines
//!   are modelled here as a `Duration` against the injected pool clock and
//!   interruption as the [`SemanticExecutionAuthority`] trait; the integrator
//!   adapts `RetrievalBudget` onto it.
#[cfg(feature = "semantic-fastembed")]
use fastembed::{
    InitOptionsUserDefined, Pooling as FastEmbedPooling, QuantizationMode, TextEmbedding,
    TokenizerFiles, UserDefinedEmbeddingModel,
};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(test, feature = "semantic-fastembed"))]
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChunkerRevision, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, ManifestDigest, PrivacyDomainId,
};

use super::artifact_store::{
    AdmittedArtifactV1, FASTEMBED_RUNTIME_BUILD_REVISION_V1, FASTEMBED_RUNTIME_FAMILY_V1,
};
use super::manifest::{ArtifactMemberRoleV1, ArtifactProfileKindV1, Sha256DigestHex};
use super::model_catalog::{CatalogMemberPinV1, CatalogedFastEmbedModelV1, catalog_package_digest};
use crate::SemanticResourceCeilings;

mod pins;
pub use pins::ProjectionArtifactPinV1;

fn inference_batch_byte_ceiling(max_batch_size: u32, max_sequence_length: u32) -> u32 {
    max_batch_size
        .saturating_mul(max_sequence_length)
        .saturating_mul(4)
}

/// Typed failure of one embedding operation or runtime admission (Plan 31:
/// load failure, OOM, corruption, revocation, or incompatible pins disables
/// the affected semantic stage; nothing silently substitutes another model).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbedError {
    /// The caller's cancellation signal fired before or during the batch.
    Cancelled,
    /// The caller's authoritative deadline expired before or during the batch.
    DeadlineExceeded,
    /// A batch with zero texts invokes no inference.
    EmptyBatch,
    /// The batch exceeded the manifest's bounded text count.
    TooManyTexts { presented: usize, max: usize },
    /// The batch exceeded the manifest's bounded total sanitized bytes.
    BatchBytesExceeded { presented: usize, max: usize },
    /// A produced vector does not match the manifest's declared dimension.
    DimensionMismatch { expected: u32, actual: usize },
    /// A produced vector contains NaN or infinite values.
    NonFiniteVectorValue,
    /// Runtime-level failure (load, OOM, corruption, revocation,
    /// incompatibility, inference failure).
    Runtime(RuntimeFailureV1),
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "the embedding operation was cancelled"),
            Self::DeadlineExceeded => write!(f, "the embedding operation deadline expired"),
            Self::EmptyBatch => write!(f, "an empty batch invokes no inference"),
            Self::TooManyTexts { presented, max } => write!(
                f,
                "batch of {presented} texts exceeds the manifest bound of {max}"
            ),
            Self::BatchBytesExceeded { presented, max } => write!(
                f,
                "batch of {presented} bytes exceeds the manifest bound of {max}"
            ),
            Self::DimensionMismatch { expected, actual } => write!(
                f,
                "vector dimension {actual} does not match the manifest dimension {expected}"
            ),
            Self::NonFiniteVectorValue => {
                write!(f, "vector contains NaN or infinite values")
            }
            Self::Runtime(failure) => write!(f, "{failure}"),
        }
    }
}

impl Error for EmbedError {}

/// Kind of runtime-level failure (Plan 31: each disables the affected
/// semantic stage with a typed reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeFailureKindV1 {
    LoadFailed,
    OutOfMemory,
    CorruptArtifact,
    RevokedArtifact,
    IncompatibleRuntime,
    EmbedFailed,
}

impl fmt::Display for RuntimeFailureKindV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::LoadFailed => "load_failed",
            Self::OutOfMemory => "out_of_memory",
            Self::CorruptArtifact => "corrupt_artifact",
            Self::RevokedArtifact => "revoked_artifact",
            Self::IncompatibleRuntime => "incompatible_runtime",
            Self::EmbedFailed => "embed_failed",
        };
        f.write_str(s)
    }
}

/// One typed runtime failure with an operator-facing detail string (no raw
/// model bytes, query text, or private paths — Plan 31 privacy boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFailureV1 {
    pub kind: RuntimeFailureKindV1,
    pub detail: String,
}

impl fmt::Display for RuntimeFailureV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime failure ({}): {}", self.kind, self.detail)
    }
}

/// Private runtime descriptor created only by successful projection/artifact
/// admission. It carries the admitted domain projection directly rather than
/// re-declaring vector-affecting pins in adapter-local types.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedEmbeddingArtifactV1 {
    projection: AdmittedEmbeddingProjectionKeyV1,
    model_file: String,
    tokenizer_file: String,
    config_file: String,
    artifact: Option<AdmittedArtifactV1>,
    lifecycle_install: Option<LifecycleInstallArtifactV1>,
    max_batch_texts: u32,
    max_batch_bytes: u32,
    max_threads: u32,
    resident_byte_ceiling: u64,
    load_deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LifecycleInstallArtifactV1 {
    root: PathBuf,
    members: std::collections::BTreeMap<String, CatalogMemberPinV1>,
}

impl VerifiedEmbeddingArtifactV1 {
    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn embedding_key(&self) -> &tracedecay_domain::EmbeddingProjectionKeyV1 {
        self.projection.embedding_key()
    }

    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn dimensions(&self) -> u32 {
        self.embedding_key().dimensions
    }

    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn metric(&self) -> EmbeddingMetricV1 {
        self.embedding_key().metric
    }

    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn normalization(&self) -> EmbeddingNormalizationV1 {
        self.embedding_key().normalization
    }

    #[cfg(feature = "semantic-fastembed")]
    fn pooling(&self) -> EmbeddingPoolingV1 {
        self.embedding_key().pooling
    }

    #[cfg(feature = "semantic-fastembed")]
    fn precision(&self) -> EmbeddingPrecisionV1 {
        self.embedding_key().precision
    }

    #[cfg(feature = "semantic-fastembed")]
    fn truncation_length(&self) -> u32 {
        self.embedding_key().truncation_length
    }

    fn max_batch_texts(&self) -> u32 {
        self.max_batch_texts
    }

    fn max_batch_bytes(&self) -> u32 {
        self.max_batch_bytes
    }

    #[cfg(feature = "semantic-fastembed")]
    fn declares_member(&self, role: ArtifactMemberRoleV1) -> bool {
        self.artifact
            .as_ref()
            .is_some_and(|artifact| artifact.manifest().package_member(role).is_some())
            || self
                .lifecycle_install
                .as_ref()
                .is_some_and(|install| install.declares_member(role))
    }

    fn max_threads(&self) -> u32 {
        self.max_threads
    }

    pub fn resident_byte_ceiling(&self) -> u64 {
        self.resident_byte_ceiling
    }

    pub fn load_deadline_ms(&self) -> u64 {
        self.load_deadline_ms
    }

    // Feature-independent implementation, but only the compiled FastEmbed
    // runtime and descriptor tests need direct member bytes.
    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn required_member_bytes(&self, role: ArtifactMemberRoleV1) -> Result<Vec<u8>, EmbedError> {
        if let Some(artifact) = self.artifact.as_ref() {
            return artifact.read_member_bytes(role).map_err(|_| {
                fastembed_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "verified artifact member no longer matches its signed pin",
                )
            });
        }
        let lifecycle = self.lifecycle_install.as_ref().ok_or_else(|| {
            fastembed_failure(
                RuntimeFailureKindV1::LoadFailed,
                "verified artifact bytes are unavailable",
            )
        })?;
        lifecycle.read_member_bytes(role)
    }
}

/// Single root-private authority pairing a store-admitted artifact with an
/// admitted domain projection. Construction exhaustively checks every pin the
/// manifest and projection share before compatibility or session open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedProjectionArtifactV1 {
    runtime_artifact: VerifiedEmbeddingArtifactV1,
}

impl AdmittedProjectionArtifactV1 {
    pub fn admit(
        artifact: &AdmittedArtifactV1,
        projection: &AdmittedEmbeddingProjectionKeyV1,
    ) -> Result<Self, ProjectionArtifactPinV1> {
        let manifest = artifact.manifest();
        let payload = &manifest.payload;
        let key = projection.embedding_key();

        require_pin(
            artifact.artifact_digest() == &manifest.artifact_identity_digest(),
            ProjectionArtifactPinV1::ArtifactIdentity,
        )?;
        require_pin(
            artifact.manifest_digest() == &manifest.canonical_digest(),
            ProjectionArtifactPinV1::ManifestIdentity,
        )?;
        require_pin(
            payload.profile_kind == ArtifactProfileKindV1::Embedding,
            ProjectionArtifactPinV1::ProfileKind,
        )?;
        require_pin(
            key.model_artifact_digest
                == domain_digest(
                    artifact.artifact_digest(),
                    ProjectionArtifactPinV1::ArtifactDigest,
                )?,
            ProjectionArtifactPinV1::ArtifactDigest,
        )?;
        require_pin(
            key.tokenizer_digest
                == domain_digest(
                    &payload.tokenizer_digest,
                    ProjectionArtifactPinV1::TokenizerDigest,
                )?,
            ProjectionArtifactPinV1::TokenizerDigest,
        )?;
        require_pin(
            key.config_digest
                == domain_digest(
                    &payload.config_digest,
                    ProjectionArtifactPinV1::ConfigDigest,
                )?,
            ProjectionArtifactPinV1::ConfigDigest,
        )?;
        let query_instruction_digest = payload
            .query_instruction_digest
            .as_ref()
            .map(|digest| domain_digest(digest, ProjectionArtifactPinV1::QueryInstructionDigest))
            .transpose()?;
        require_pin(
            key.query_instruction_digest == query_instruction_digest,
            ProjectionArtifactPinV1::QueryInstructionDigest,
        )?;
        let document_instruction_digest = payload
            .document_instruction_digest
            .as_ref()
            .map(|digest| domain_digest(digest, ProjectionArtifactPinV1::DocumentInstructionDigest))
            .transpose()?;
        require_pin(
            key.document_instruction_digest == document_instruction_digest,
            ProjectionArtifactPinV1::DocumentInstructionDigest,
        )?;
        require_pin(
            key.pooling == payload.pooling,
            ProjectionArtifactPinV1::Pooling,
        )?;
        require_pin(
            key.truncation_side == payload.truncation.side,
            ProjectionArtifactPinV1::TruncationSide,
        )?;
        require_pin(
            key.truncation_length == payload.truncation.max_length
                && key.truncation_length == payload.resource_ceiling.max_sequence_length,
            ProjectionArtifactPinV1::TruncationLength,
        )?;
        require_pin(
            key.inference_batch_size == payload.resource_ceiling.max_batch_size,
            ProjectionArtifactPinV1::InferenceBatchSize,
        )?;
        require_pin(
            key.inference_batch_bytes
                == inference_batch_byte_ceiling(
                    payload.resource_ceiling.max_batch_size,
                    payload.resource_ceiling.max_sequence_length,
                ),
            ProjectionArtifactPinV1::InferenceBatchBytes,
        )?;
        require_pin(
            key.runtime_backend == payload.runtime.runtime,
            ProjectionArtifactPinV1::RuntimeBackend,
        )?;
        require_pin(
            key.runtime_build_revision == payload.runtime.build_revision,
            ProjectionArtifactPinV1::RuntimeBuildRevision,
        )?;
        require_pin(
            key.device_class == payload.device,
            ProjectionArtifactPinV1::DeviceClass,
        )?;
        require_pin(
            key.dimensions == payload.dimensions,
            ProjectionArtifactPinV1::Dimensions,
        )?;
        require_pin(
            key.metric == payload.metric,
            ProjectionArtifactPinV1::Metric,
        )?;
        require_pin(
            key.normalization == payload.normalization,
            ProjectionArtifactPinV1::Normalization,
        )?;
        require_pin(
            key.precision == payload.precision,
            ProjectionArtifactPinV1::Precision,
        )?;

        let model_file = member_path(artifact, ArtifactMemberRoleV1::Model)?;
        let tokenizer_file = member_path(artifact, ArtifactMemberRoleV1::Tokenizer)?;
        let config_file = member_path(artifact, ArtifactMemberRoleV1::Config)?;
        Ok(Self {
            runtime_artifact: VerifiedEmbeddingArtifactV1 {
                projection: projection.clone(),
                model_file,
                tokenizer_file,
                config_file,
                artifact: Some(artifact.clone()),
                lifecycle_install: None,
                max_batch_texts: payload.resource_ceiling.max_batch_size,
                max_batch_bytes: inference_batch_byte_ceiling(
                    payload.resource_ceiling.max_batch_size,
                    payload.resource_ceiling.max_sequence_length,
                ),
                max_threads: payload.resource_ceiling.max_threads,
                resident_byte_ceiling: payload.resource_ceiling.max_resident_bytes,
                load_deadline_ms: payload.resource_ceiling.load_deadline_ms,
            },
        })
    }

    pub fn from_lifecycle_install(
        model: &CatalogedFastEmbedModelV1,
        install_path: &Path,
        chunker_revision: ChunkerRevision,
        privacy_domain: PrivacyDomainId,
        privacy_key_epoch: u64,
        resources: SemanticResourceCeilings,
    ) -> Result<Self, EmbedError> {
        let projection = Self::lifecycle_projection(
            model,
            chunker_revision,
            privacy_domain,
            privacy_key_epoch,
            resources,
        )?;
        let member = |role: &str| {
            model.members.get(role).ok_or_else(|| {
                fastembed_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "cataloged lifecycle install is missing a required member",
                )
            })
        };
        let model_member = member("model")?;
        let tokenizer = member("tokenizer")?;
        let config = member("config")?;
        member("special_tokens_map")?;
        member("tokenizer_config")?;
        let lifecycle_install = LifecycleInstallArtifactV1 {
            root: install_path.to_path_buf(),
            members: model.members.clone(),
        };
        // Check every required member's structural pin (declared entry,
        // normalized path, regular non-symlink file, exact length) without
        // reading its bytes. Byte digests are verified by
        // `read_member_bytes` at every session open — the only place member
        // bytes are consumed — matching the artifact-store authority, whose
        // admission also defers digest checks to reads. Reading and hashing
        // the whole model file here charged every authority construction
        // (each scheduled projection's artifact load and each serving
        // restore attempt) a full model read that session open then
        // repeated.
        for role in [
            ArtifactMemberRoleV1::Model,
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
            ArtifactMemberRoleV1::SpecialTokensMap,
            ArtifactMemberRoleV1::TokenizerConfig,
        ] {
            lifecycle_install.member_pin_path(role)?;
        }
        Ok(Self {
            runtime_artifact: VerifiedEmbeddingArtifactV1 {
                projection,
                model_file: model_member.path.clone(),
                tokenizer_file: tokenizer.path.clone(),
                config_file: config.path.clone(),
                artifact: None,
                lifecycle_install: Some(lifecycle_install),
                max_batch_texts: resources.max_batch_size,
                max_batch_bytes: inference_batch_byte_ceiling(
                    resources.max_batch_size,
                    resources.max_sequence_length,
                ),
                max_threads: resources.max_threads,
                resident_byte_ceiling: resources.max_resident_bytes,
                load_deadline_ms: resources.load_deadline_ms,
            },
        })
    }

    pub fn lifecycle_projection(
        model: &CatalogedFastEmbedModelV1,
        chunker_revision: ChunkerRevision,
        privacy_domain: PrivacyDomainId,
        privacy_key_epoch: u64,
        resources: SemanticResourceCeilings,
    ) -> Result<AdmittedEmbeddingProjectionKeyV1, EmbedError> {
        let member = |role: &str| {
            model.members.get(role).ok_or_else(|| {
                fastembed_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "cataloged lifecycle install is missing a required member",
                )
            })
        };
        let model_member = member("model")?;
        let tokenizer = member("tokenizer")?;
        let config = member("config")?;
        if model_member.length > resources.max_model_bytes
            || tokenizer.length > resources.max_tokenizer_bytes
            || model_member.length > resources.max_resident_bytes
        {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::OutOfMemory,
                "cataloged lifecycle model exceeds configured resource ceilings",
            ));
        }
        let manifest_digest = |value: &str| {
            ManifestDigest::new(format!("sha256:{value}")).map_err(|_| {
                fastembed_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "cataloged lifecycle member digest is invalid",
                )
            })
        };
        let projection = EmbeddingProjectionKeyV1 {
            model_artifact_digest: manifest_digest(&catalog_package_digest(model))?,
            tokenizer_digest: manifest_digest(&tokenizer.sha256)?,
            config_digest: manifest_digest(&config.sha256)?,
            query_instruction_digest: None,
            document_instruction_digest: None,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: model.max_length.min(resources.max_sequence_length),
            inference_batch_size: resources.max_batch_size,
            inference_batch_bytes: inference_batch_byte_ceiling(
                resources.max_batch_size,
                resources.max_sequence_length,
            ),
            runtime_backend: FASTEMBED_RUNTIME_FAMILY_V1.to_owned(),
            runtime_build_revision: FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: model.expected_dimensions,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision,
            privacy_domain,
            privacy_key_epoch,
        }
        .admit()
        .map_err(|_| {
            fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle projection is invalid",
            )
        })?;
        Ok(projection)
    }

    pub fn projection(&self) -> &AdmittedEmbeddingProjectionKeyV1 {
        &self.runtime_artifact.projection
    }

    /// Intra-op width is admitted with the artifact and is part of the exact
    /// FastEmbed execution identity, even though it is not a projection-key
    /// field. Keep it crate-private so callers cannot label a cache entry
    /// with an independently supplied width.
    pub(crate) fn execution_max_threads(&self) -> u32 {
        self.runtime_artifact.max_threads()
    }

    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn runtime_artifact(&self) -> &VerifiedEmbeddingArtifactV1 {
        &self.runtime_artifact
    }

    pub fn resident_byte_ceiling(&self) -> u64 {
        self.runtime_artifact.resident_byte_ceiling()
    }

    pub fn load_deadline_ms(&self) -> u64 {
        self.runtime_artifact.load_deadline_ms()
    }

    pub fn max_batch_bytes(&self) -> u32 {
        self.runtime_artifact.max_batch_bytes()
    }

    pub fn max_batch_texts(&self) -> u32 {
        self.runtime_artifact.max_batch_texts()
    }

    #[cfg(test)]
    pub fn with_test_max_batch_bytes(mut self, max_batch_bytes: u32) -> Self {
        self.runtime_artifact.max_batch_bytes = max_batch_bytes;
        self
    }
}

impl LifecycleInstallArtifactV1 {
    #[cfg(feature = "semantic-fastembed")]
    fn declares_member(&self, role: ArtifactMemberRoleV1) -> bool {
        let key = match role {
            ArtifactMemberRoleV1::Model => "model",
            ArtifactMemberRoleV1::Tokenizer => "tokenizer",
            ArtifactMemberRoleV1::Config => "config",
            ArtifactMemberRoleV1::SpecialTokensMap => "special_tokens_map",
            ArtifactMemberRoleV1::TokenizerConfig => "tokenizer_config",
            ArtifactMemberRoleV1::QueryInstruction | ArtifactMemberRoleV1::DocumentInstruction => {
                return false;
            }
        };
        self.members.contains_key(key)
    }

    /// Resolve one member pin to its on-disk path after the byte-free
    /// structural checks: the pin exists, its path is a normalized relative
    /// path, and the target is a regular non-symlink file matching the
    /// length pin exactly. Digest verification deliberately stays in
    /// [`Self::read_member_bytes`], where the bytes are consumed.
    fn member_pin_path(
        &self,
        role: ArtifactMemberRoleV1,
    ) -> Result<(PathBuf, &CatalogMemberPinV1), EmbedError> {
        let key = match role {
            ArtifactMemberRoleV1::Model => "model",
            ArtifactMemberRoleV1::Tokenizer => "tokenizer",
            ArtifactMemberRoleV1::Config => "config",
            ArtifactMemberRoleV1::SpecialTokensMap => "special_tokens_map",
            ArtifactMemberRoleV1::TokenizerConfig => "tokenizer_config",
            ArtifactMemberRoleV1::QueryInstruction | ArtifactMemberRoleV1::DocumentInstruction => {
                return Err(fastembed_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "cataloged lifecycle install does not declare instruction members",
                ));
            }
        };
        let pin = self.members.get(key).ok_or_else(|| {
            fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle install is missing a required member",
            )
        })?;
        let relative = Path::new(&pin.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle member path is not normalized",
            ));
        }
        let path = self.root.join(relative);
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
            fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle member is unavailable",
            )
        })?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != pin.length
        {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle member no longer matches its length pin",
            ));
        }
        Ok((path, pin))
    }

    // Byte reads exist only where a runtime consumes member bytes, matching
    // the [`VerifiedEmbeddingArtifactV1::required_member_bytes`] gate.
    #[cfg(any(test, feature = "semantic-fastembed"))]
    fn read_member_bytes(&self, role: ArtifactMemberRoleV1) -> Result<Vec<u8>, EmbedError> {
        let (path, pin) = self.member_pin_path(role)?;
        let bytes = std::fs::read(path).map_err(|_| {
            fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle member cannot be read",
            )
        })?;
        if hex::encode(Sha256::digest(&bytes)) != pin.sha256 {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "cataloged lifecycle member no longer matches its digest pin",
            ));
        }
        Ok(bytes)
    }
}

fn require_pin(matches: bool, pin: ProjectionArtifactPinV1) -> Result<(), ProjectionArtifactPinV1> {
    matches.then_some(()).ok_or(pin)
}

fn domain_digest(
    digest: &Sha256DigestHex,
    pin: ProjectionArtifactPinV1,
) -> Result<ManifestDigest, ProjectionArtifactPinV1> {
    ManifestDigest::new(format!("sha256:{}", digest.as_str())).map_err(|_| pin)
}

fn member_path(
    artifact: &AdmittedArtifactV1,
    role: ArtifactMemberRoleV1,
) -> Result<String, ProjectionArtifactPinV1> {
    artifact
        .manifest()
        .package_member(role)
        .map(|member| member.path.clone())
        .ok_or(ProjectionArtifactPinV1::ManifestIdentity)
}

/// One typed embedding vector. Dimensions, metric, and normalization are
/// echoed from the verified manifest descriptor, so a consumer can prove the
/// vector belongs to the declared projection without trusting the runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingVectorV1 {
    pub values: Vec<f32>,
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
}

impl EmbeddingVectorV1 {
    /// Echo validation: declared dimension matches the payload and every
    /// value is finite (Plan 31: publication verifies dimensions and finite
    /// values; the runtime port fails fast on the same invariants).
    pub fn validate(&self) -> Result<(), EmbedError> {
        if self.values.len() != self.dimensions as usize {
            return Err(EmbedError::DimensionMismatch {
                expected: self.dimensions,
                actual: self.values.len(),
            });
        }
        if self.values.iter().any(|v| !v.is_finite()) {
            return Err(EmbedError::NonFiniteVectorValue);
        }
        Ok(())
    }

    /// Squared L2 norm of the payload (test and diagnostic aid).
    pub fn squared_l2_norm(&self) -> f32 {
        self.values.iter().map(|v| v * v).sum()
    }
}

/// One bounded batch of sanitized text ready for inference (Plan 31: a raw
/// query/source is sanitized into a bounded ephemeral view before model
/// inference; the batch bound comes from the manifest's resource ceiling).
///
/// Construction enforces the bound, so a value of this type is proof the
/// batch is within *some* declared ceiling. A session re-checks the batch
/// against its own descriptor's ceiling, which may be tighter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedSanitizedTextBatchV1 {
    texts: Vec<String>,
    total_bytes: usize,
}

impl BoundedSanitizedTextBatchV1 {
    pub fn try_new(
        texts: Vec<String>,
        max_texts: usize,
        max_bytes: usize,
    ) -> Result<Self, EmbedError> {
        if texts.is_empty() {
            return Err(EmbedError::EmptyBatch);
        }
        if texts.len() > max_texts {
            return Err(EmbedError::TooManyTexts {
                presented: texts.len(),
                max: max_texts,
            });
        }
        let total_bytes: usize = texts.iter().map(std::string::String::len).sum();
        if total_bytes > max_bytes {
            return Err(EmbedError::BatchBytesExceeded {
                presented: total_bytes,
                max: max_bytes,
            });
        }
        Ok(Self { texts, total_bytes })
    }

    pub fn texts(&self) -> &[String] {
        &self.texts
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn len(&self) -> usize {
        self.texts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }
}

/// One request-owned interruption authority shared by every semantic stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticExecutionInterruptionV1 {
    Cancelled,
    DeadlineExceeded,
}

pub trait SemanticExecutionAuthority: Send + Sync {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1>;
}

#[cfg(any(test, feature = "semantic-fastembed"))]
fn check_execution_authority(authority: &dyn SemanticExecutionAuthority) -> Result<(), EmbedError> {
    match authority.interruption() {
        None => Ok(()),
        Some(SemanticExecutionInterruptionV1::Cancelled) => Err(EmbedError::Cancelled),
        Some(SemanticExecutionInterruptionV1::DeadlineExceeded) => {
            Err(EmbedError::DeadlineExceeded)
        }
    }
}

/// A manually flipped cancellation flag.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ManualCancellation {
    flag: AtomicBool,
}

#[cfg(test)]
impl ManualCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl SemanticExecutionAuthority for ManualCancellation {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        self.flag
            .load(Ordering::SeqCst)
            .then_some(SemanticExecutionInterruptionV1::Cancelled)
    }
}

/// A deterministic scripted signal that reports cancelled once it has been
/// polled more than `cancel_after` times. `cancel_after = 0` cancels on the
/// first poll. Used to test mid-batch cancellation deterministically.
#[cfg(test)]
#[derive(Debug)]
pub struct ScriptedCancellation {
    checks: AtomicUsize,
    cancel_after: usize,
}

#[cfg(test)]
impl ScriptedCancellation {
    pub fn new(cancel_after: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_after,
        }
    }
}

#[cfg(test)]
impl SemanticExecutionAuthority for ScriptedCancellation {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        let poll = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        (poll > self.cancel_after).then_some(SemanticExecutionInterruptionV1::Cancelled)
    }
}

/// One warmed embedding session bound to one verified artifact descriptor
/// (Plan 31: compatible warmed sessions are pooled under bounded memory,
/// concurrency, idle, and cancellation policy).
pub trait EmbeddingSession: Send {
    /// The authority this session was opened from (echo surface).
    fn authority(&self) -> &AdmittedProjectionArtifactV1;
    /// Estimated resident bytes, used by the pool's memory-ceiling
    /// enforcement. Must be <= the descriptor's `resident_byte_ceiling`.
    fn resident_bytes_estimate(&self) -> u64;
    /// Embed one bounded sanitized batch. Returns one typed vector per input
    /// text, in input order. Implementations must honor the cancellation
    /// signal between texts and must return an error rather than a partial
    /// batch.
    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError>;
}

/// The root-private embedding runtime port (Plan 31: load verified artifact
/// → create session → embed bounded sanitized batches). The only production
/// implementation will be the `FastEmbed` adapter in this module; every other
/// crate depends on this trait surface, never on `FastEmbed` runtime types.
pub trait EmbeddingRuntime {
    type Session: EmbeddingSession;

    /// Conservative resident-byte reservation made before session loading.
    /// Production runtimes must return an upper bound so concurrent opens
    /// cannot transiently exceed the pool's memory ceiling.
    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64;

    /// Cheap admission-time compatibility check (Plan 31: activation verifies
    /// runtime/platform compatibility before publishing). Performs no model
    /// load and no I/O beyond descriptor/platform inspection.
    fn verify_artifact_compatibility(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError>;

    /// Load the verified artifact and create one warmed session. The
    /// artifact bytes are already installed and verified by the artifact
    /// packet; this performs no download, import, extraction, cache
    /// discovery, or trust decision.
    fn open_session(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<Self::Session, EmbedError>;
}

#[cfg(any(test, feature = "semantic-fastembed"))]
fn validate_batch_limits(
    batch: &BoundedSanitizedTextBatchV1,
    artifact: &VerifiedEmbeddingArtifactV1,
) -> Result<(), EmbedError> {
    if batch.is_empty() {
        return Err(EmbedError::EmptyBatch);
    }
    if batch.len() > artifact.max_batch_texts() as usize {
        return Err(EmbedError::TooManyTexts {
            presented: batch.len(),
            max: artifact.max_batch_texts() as usize,
        });
    }
    if batch.total_bytes() > artifact.max_batch_bytes() as usize {
        return Err(EmbedError::BatchBytesExceeded {
            presented: batch.total_bytes(),
            max: artifact.max_batch_bytes() as usize,
        });
    }
    Ok(())
}

/// The production `FastEmbed` runtime. Its dependency feature disables model-hub
/// support, and this adapter uses only `FastEmbed`'s local-byte constructor.
#[cfg(feature = "semantic-fastembed")]
#[derive(Default)]
pub struct FastEmbedEmbeddingRuntime;

/// Feature-disabled stand-in: keeps every consumer type-compatible while the
/// `semantic-fastembed` dependency is compiled out. Every operation fails
/// with a typed runtime failure, so semantic retrieval degrades to its
/// documented fallback states instead of the crate failing to build.
#[cfg(not(feature = "semantic-fastembed"))]
#[derive(Default)]
pub struct FastEmbedEmbeddingRuntime;

/// Uninhabited session type for the feature-disabled runtime: `open_session`
/// always fails, so no session value can ever exist.
#[cfg(not(feature = "semantic-fastembed"))]
pub enum UnavailableEmbeddingSession {}

#[cfg(not(feature = "semantic-fastembed"))]
impl EmbeddingSession for UnavailableEmbeddingSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        match *self {}
    }

    fn resident_bytes_estimate(&self) -> u64 {
        match *self {}
    }

    fn embed_batch(
        &mut self,
        _batch: &BoundedSanitizedTextBatchV1,
        _authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        match *self {}
    }
}

#[cfg(not(feature = "semantic-fastembed"))]
impl EmbeddingRuntime for FastEmbedEmbeddingRuntime {
    type Session = UnavailableEmbeddingSession;

    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
        authority.resident_byte_ceiling()
    }

    fn verify_artifact_compatibility(
        &self,
        _authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        Err(fastembed_failure(
            RuntimeFailureKindV1::IncompatibleRuntime,
            "the semantic-fastembed feature is compiled out of this build",
        ))
    }

    fn open_session(
        &self,
        _authority: &AdmittedProjectionArtifactV1,
    ) -> Result<Self::Session, EmbedError> {
        Err(fastembed_failure(
            RuntimeFailureKindV1::IncompatibleRuntime,
            "the semantic-fastembed feature is compiled out of this build",
        ))
    }
}

#[cfg(feature = "semantic-fastembed")]
impl EmbeddingRuntime for FastEmbedEmbeddingRuntime {
    type Session = FastEmbedEmbeddingSession;

    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
        authority.resident_byte_ceiling()
    }

    fn verify_artifact_compatibility(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        let artifact = authority.runtime_artifact();
        if artifact.embedding_key().runtime_backend != "fastembed-ort" {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the projection does not select the FastEmbed ORT backend",
            ));
        }
        if artifact.normalization() != EmbeddingNormalizationV1::L2 {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "FastEmbed always returns L2-normalized text embeddings",
            ));
        }
        let _ = fastembed_pooling(artifact.pooling())?;
        for role in [
            ArtifactMemberRoleV1::Model,
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
            ArtifactMemberRoleV1::SpecialTokensMap,
            ArtifactMemberRoleV1::TokenizerConfig,
        ] {
            if !artifact.declares_member(role) {
                return Err(fastembed_failure(
                    RuntimeFailureKindV1::IncompatibleRuntime,
                    "the artifact lacks a FastEmbed-required tokenizer member",
                ));
            }
        }
        if artifact.max_threads() == 0 {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the artifact has no permitted FastEmbed inference threads",
            ));
        }
        Ok(())
    }

    fn open_session(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<Self::Session, EmbedError> {
        self.verify_artifact_compatibility(authority)?;
        let artifact = authority.runtime_artifact();
        let model = fastembed_model(artifact)?;
        let options = InitOptionsUserDefined::new()
            .with_max_length(artifact.truncation_length() as usize)
            .with_intra_threads(artifact.max_threads() as usize);
        let embedding =
            TextEmbedding::try_new_from_user_defined(model, options).map_err(|error| {
                fastembed_error(
                    RuntimeFailureKindV1::LoadFailed,
                    "FastEmbed could not initialize the verified artifact",
                    &error,
                )
            })?;
        Ok(FastEmbedEmbeddingSession {
            authority: authority.clone(),
            embedding,
        })
    }
}

#[cfg(feature = "semantic-fastembed")]
pub struct FastEmbedEmbeddingSession {
    authority: AdmittedProjectionArtifactV1,
    embedding: TextEmbedding,
}

#[cfg(feature = "semantic-fastembed")]
impl EmbeddingSession for FastEmbedEmbeddingSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        &self.authority
    }

    fn resident_bytes_estimate(&self) -> u64 {
        self.authority.resident_byte_ceiling()
    }

    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        let artifact = self.authority.runtime_artifact();
        validate_batch_limits(batch, artifact)?;

        check_execution_authority(authority)?;
        // FastEmbed/ORT inference is synchronous. Keep batches small at the
        // projector boundary, then perform one tensor invocation per admitted
        // batch instead of one invocation per text.
        let embedded = self
            .embedding
            .embed(batch.texts(), Some(batch.len()))
            .map_err(|error| {
                fastembed_error(
                    RuntimeFailureKindV1::EmbedFailed,
                    "FastEmbed inference failed for the verified artifact",
                    &error,
                )
            })?;
        check_execution_authority(authority)?;
        if embedded.len() != batch.len() {
            return Err(fastembed_failure(
                RuntimeFailureKindV1::EmbedFailed,
                "FastEmbed returned an unexpected embedding count",
            ));
        }
        let mut vectors = Vec::with_capacity(batch.len());
        for mut values in embedded {
            // Canonicalize IEEE negative zero before vector hashing and
            // exact-flat comparison. FastEmbed remains the sole producer;
            // this changes no distance while removing signed-zero drift.
            for value in &mut values {
                if *value == 0.0 {
                    *value = 0.0;
                }
            }
            let vector = EmbeddingVectorV1 {
                values,
                dimensions: artifact.dimensions(),
                metric: artifact.metric(),
                normalization: artifact.normalization(),
            };
            vector.validate()?;
            vectors.push(vector);
        }
        Ok(vectors)
    }
}

#[cfg(feature = "semantic-fastembed")]
fn fastembed_model(
    artifact: &VerifiedEmbeddingArtifactV1,
) -> Result<UserDefinedEmbeddingModel, EmbedError> {
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: artifact.required_member_bytes(ArtifactMemberRoleV1::Tokenizer)?,
        config_file: artifact.required_member_bytes(ArtifactMemberRoleV1::Config)?,
        special_tokens_map_file: artifact
            .required_member_bytes(ArtifactMemberRoleV1::SpecialTokensMap)?,
        tokenizer_config_file: artifact
            .required_member_bytes(ArtifactMemberRoleV1::TokenizerConfig)?,
    };
    Ok(UserDefinedEmbeddingModel::new(
        artifact.required_member_bytes(ArtifactMemberRoleV1::Model)?,
        tokenizer_files,
    )
    .with_pooling(fastembed_pooling(artifact.pooling())?)
    .with_quantization(fastembed_quantization(artifact.precision())))
}

#[cfg(feature = "semantic-fastembed")]
fn fastembed_pooling(pooling: EmbeddingPoolingV1) -> Result<FastEmbedPooling, EmbedError> {
    match pooling {
        EmbeddingPoolingV1::Mean => Ok(FastEmbedPooling::Mean),
        EmbeddingPoolingV1::Cls => Ok(FastEmbedPooling::Cls),
        EmbeddingPoolingV1::LastToken | EmbeddingPoolingV1::MeanSqrtLength => {
            Err(fastembed_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the projection pooling mode is unsupported by FastEmbed",
            ))
        }
    }
}

#[cfg(feature = "semantic-fastembed")]
fn fastembed_quantization(precision: EmbeddingPrecisionV1) -> QuantizationMode {
    match precision {
        EmbeddingPrecisionV1::Int8 => QuantizationMode::Static,
        EmbeddingPrecisionV1::Fp32 | EmbeddingPrecisionV1::Fp16 | EmbeddingPrecisionV1::Bf16 => {
            QuantizationMode::None
        }
    }
}

fn fastembed_failure(kind: RuntimeFailureKindV1, detail: &str) -> EmbedError {
    EmbedError::Runtime(RuntimeFailureV1 {
        kind,
        detail: detail.to_owned(),
    })
}

#[cfg(feature = "semantic-fastembed")]
fn fastembed_error(
    fallback_kind: RuntimeFailureKindV1,
    detail: &str,
    error: &impl fmt::Display,
) -> EmbedError {
    let message = error.to_string().to_ascii_lowercase();
    let kind = if message.contains("out of memory") || message.contains("allocation") {
        RuntimeFailureKindV1::OutOfMemory
    } else {
        fallback_kind
    };
    fastembed_failure(kind, detail)
}

/// Test-observable counters for the deterministic fake runtime.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeRuntimeCounters {
    pub compatibility_checks: AtomicUsize,
    pub sessions_opened: AtomicUsize,
    pub sessions_closed: AtomicUsize,
    pub embed_calls: AtomicUsize,
    pub texts_embedded: AtomicUsize,
}

/// Deterministic offline implementation of [`EmbeddingRuntime`]. It loads no
/// model, performs no I/O and no network access, and produces hash-based
/// pseudo-embeddings with the descriptor's declared dimensions, metric, and
/// normalization. Same descriptor digest + same text always yields the same
/// vector, so all pool/session behavior is testable offline.
#[cfg(test)]
#[derive(Debug)]
pub struct FakeEmbeddingRuntime {
    resident_bytes_per_session: u64,
    open_failure: Option<RuntimeFailureKindV1>,
    compatibility_failure: Option<RuntimeFailureKindV1>,
    counters: Arc<FakeRuntimeCounters>,
}

#[cfg(test)]
impl Default for FakeEmbeddingRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl FakeEmbeddingRuntime {
    pub fn new() -> Self {
        Self {
            resident_bytes_per_session: 1024 * 1024,
            open_failure: None,
            compatibility_failure: None,
            counters: Arc::new(FakeRuntimeCounters::default()),
        }
    }

    pub fn with_resident_bytes_per_session(mut self, bytes: u64) -> Self {
        self.resident_bytes_per_session = bytes;
        self
    }

    pub fn with_open_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.open_failure = Some(kind);
        self
    }

    pub fn with_compatibility_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.compatibility_failure = Some(kind);
        self
    }

    pub fn counters(&self) -> Arc<FakeRuntimeCounters> {
        Arc::clone(&self.counters)
    }
}

#[cfg(test)]
impl EmbeddingRuntime for FakeEmbeddingRuntime {
    type Session = FakeEmbeddingSession;

    fn resident_bytes_reservation(&self, _authority: &AdmittedProjectionArtifactV1) -> u64 {
        self.resident_bytes_per_session
    }

    fn verify_artifact_compatibility(
        &self,
        _authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        self.counters
            .compatibility_checks
            .fetch_add(1, Ordering::SeqCst);
        if let Some(kind) = self.compatibility_failure {
            return Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind,
                detail: "scripted fake compatibility failure".to_string(),
            }));
        }
        Ok(())
    }

    fn open_session(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<Self::Session, EmbedError> {
        if let Some(kind) = self.open_failure {
            return Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind,
                detail: "scripted fake open failure".to_string(),
            }));
        }
        // Production parity: a session open consumes member bytes, so a
        // lifecycle-backed authority is length- and digest-verified here
        // exactly as the FastEmbed runtime is when it reads the bytes.
        if let Some(lifecycle) = authority.runtime_artifact().lifecycle_install.as_ref() {
            for role in [
                ArtifactMemberRoleV1::Model,
                ArtifactMemberRoleV1::Tokenizer,
                ArtifactMemberRoleV1::Config,
                ArtifactMemberRoleV1::SpecialTokensMap,
                ArtifactMemberRoleV1::TokenizerConfig,
            ] {
                lifecycle.read_member_bytes(role)?;
            }
        }
        self.counters.sessions_opened.fetch_add(1, Ordering::SeqCst);
        Ok(FakeEmbeddingSession {
            authority: authority.clone(),
            vector_seed: fnv1a64(
                authority
                    .runtime_artifact()
                    .embedding_key()
                    .model_artifact_digest
                    .as_str()
                    .as_bytes(),
                FNV_OFFSET_BASIS,
            ),
            resident_bytes: self.resident_bytes_per_session,
            counters: Arc::clone(&self.counters),
        })
    }
}

/// One deterministic fake warmed session.
#[cfg(test)]
#[derive(Debug)]
pub struct FakeEmbeddingSession {
    authority: AdmittedProjectionArtifactV1,
    vector_seed: u64,
    resident_bytes: u64,
    counters: Arc<FakeRuntimeCounters>,
}

#[cfg(test)]
impl EmbeddingSession for FakeEmbeddingSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        &self.authority
    }

    fn resident_bytes_estimate(&self) -> u64 {
        self.resident_bytes
    }

    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        let artifact = self.authority.runtime_artifact();
        validate_batch_limits(batch, artifact)?;
        let mut out = Vec::with_capacity(batch.len());
        for text in batch.texts() {
            // Honor cancellation between texts; a cancelled batch returns no
            // partial results.
            check_execution_authority(authority)?;
            let vector = EmbeddingVectorV1 {
                values: pseudo_embedding(
                    self.vector_seed,
                    text,
                    artifact.dimensions(),
                    artifact.normalization(),
                ),
                dimensions: artifact.dimensions(),
                metric: artifact.metric(),
                normalization: artifact.normalization(),
            };
            vector.validate()?;
            self.counters.texts_embedded.fetch_add(1, Ordering::SeqCst);
            out.push(vector);
        }
        self.counters.embed_calls.fetch_add(1, Ordering::SeqCst);
        Ok(out)
    }
}

#[cfg(test)]
impl Drop for FakeEmbeddingSession {
    fn drop(&mut self) {
        self.counters.sessions_closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
#[cfg(test)]
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[cfg(test)]
fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(test)]
fn xorshift64star(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Deterministic hash-based pseudo-embedding: FNV-1a seeds a xorshift64*
/// stream from (model identity, text); each output maps into [-1, 1). L2
/// normalization is applied when the descriptor pins it. Pure arithmetic —
/// no `HashMap` iteration, no clock, no randomness — so results are stable
/// across runs on one platform.
#[cfg(test)]
fn pseudo_embedding(
    seed: u64,
    text: &str,
    dimensions: u32,
    normalization: EmbeddingNormalizationV1,
) -> Vec<f32> {
    let mut state = fnv1a64(text.as_bytes(), seed);
    if state == 0 {
        state = 0x9e37_79b9_7f4a_7c15;
    }
    let mut values: Vec<f32> = (0..dimensions)
        .map(|_| {
            let bits = xorshift64star(&mut state);
            // 24 high bits -> [0, 1) -> [-1, 1).
            ((bits >> 40) as f32 / 16_777_216.0_f32) * 2.0 - 1.0
        })
        .collect();
    if normalization == EmbeddingNormalizationV1::L2 {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for value in &mut values {
                *value /= norm;
            }
        }
    }
    values
}

/// Lifecycle-install fixtures shared by this module's tests and the crate
/// root's scheduling tests. Everything stays inside a tempdir; nothing here
/// reads a live profile or downloads a model.
#[cfg(test)]
pub(crate) mod lifecycle_test_support {
    use std::collections::BTreeMap;

    use sha2::{Digest, Sha256};
    use tracedecay_domain::{ChunkerRevision, PrivacyDomainId};

    use super::super::model_catalog::{
        CatalogMemberPinV1, CatalogSourceV1, CatalogedFastEmbedModelV1,
    };
    use super::AdmittedProjectionArtifactV1;
    use super::EmbedError;
    use crate::SemanticResourceCeilings;

    pub(crate) struct LifecycleInstallFixtureV1 {
        pub(crate) install: tempfile::TempDir,
        pub(crate) model: CatalogedFastEmbedModelV1,
    }

    pub(crate) fn lifecycle_install_fixture(model_bytes: &[u8]) -> LifecycleInstallFixtureV1 {
        let install = tempfile::tempdir().expect("lifecycle install");
        let members = [
            ("model", "model.onnx", model_bytes),
            ("tokenizer", "tokenizer.json", b"tokenizer".as_slice()),
            ("config", "config.json", b"config".as_slice()),
            (
                "special_tokens_map",
                "special_tokens_map.json",
                b"special".as_slice(),
            ),
            (
                "tokenizer_config",
                "tokenizer_config.json",
                b"tokenizer-config".as_slice(),
            ),
        ];
        let mut pins = BTreeMap::new();
        for (role, path, bytes) in members {
            std::fs::write(install.path().join(path), bytes).expect("fixture member");
            pins.insert(
                role.to_owned(),
                CatalogMemberPinV1 {
                    path: path.to_owned(),
                    upstream_path: path.to_owned(),
                    length: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            );
        }
        let model = CatalogedFastEmbedModelV1 {
            model_id: "jina-embeddings-v2-base-code".to_owned(),
            fastembed_enum: "JinaEmbeddingsV2BaseCode".to_owned(),
            model_code: "jinaai/jina-embeddings-v2-base-code".to_owned(),
            source: CatalogSourceV1 {
                upstream: "https://example.invalid".to_owned(),
                revision: "fixture-revision".to_owned(),
                license: "Apache-2.0".to_owned(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".to_owned(),
                provenance: "fixture".to_owned(),
            },
            expected_dimensions: 768,
            max_length: 8192,
            members: pins,
        };
        LifecycleInstallFixtureV1 { install, model }
    }

    /// A constructed lifecycle authority holding its tempdir install alive.
    pub(crate) struct LifecycleAuthorityV1 {
        pub(crate) authority: AdmittedProjectionArtifactV1,
        pub(crate) _install: tempfile::TempDir,
    }

    /// An authority whose model member matches its length pin but not its
    /// digest pin. Structural construction succeeds by design; any session
    /// open (read + digest verification) over it must fail typed.
    pub(crate) fn digest_mismatched_lifecycle_authority() -> LifecycleAuthorityV1 {
        let fixture = lifecycle_install_fixture(b"model");
        std::fs::write(fixture.install.path().join("model.onnx"), b"lodem")
            .expect("same-length digest-mismatched model member");
        let authority = lifecycle_authority_from(&fixture, 1024)
            .expect("structural construction succeeds without reading member bytes");
        LifecycleAuthorityV1 {
            authority,
            _install: fixture.install,
        }
    }

    pub(crate) fn lifecycle_authority_from(
        fixture: &LifecycleInstallFixtureV1,
        max_model_bytes: u64,
    ) -> Result<AdmittedProjectionArtifactV1, EmbedError> {
        AdmittedProjectionArtifactV1::from_lifecycle_install(
            &fixture.model,
            fixture.install.path(),
            ChunkerRevision::new("chunker.v1").expect("chunker fixture"),
            PrivacyDomainId::new("privacy.project-a".to_owned()).expect("privacy fixture"),
            7,
            SemanticResourceCeilings {
                max_model_bytes,
                max_tokenizer_bytes: 1024,
                max_resident_bytes: max_model_bytes.max(4096),
                max_threads: 1,
                max_concurrent_sessions: 1,
                max_batch_size: 4,
                max_sequence_length: 128,
                load_deadline_ms: 1_000,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::lifecycle_test_support::{
        digest_mismatched_lifecycle_authority, lifecycle_authority_from, lifecycle_install_fixture,
    };
    use super::*;
    use tracedecay_domain::{ChunkerRevision, EmbeddingProjectionKeyV1, PrivacyDomainId};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn digest(byte: char) -> ManifestDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn authority(dimensions: u32) -> AdmittedProjectionArtifactV1 {
        authority_with(
            dimensions,
            'a',
            EmbeddingMetricV1::Cosine,
            EmbeddingNormalizationV1::L2,
        )
    }

    fn authority_with(
        dimensions: u32,
        artifact_digest: char,
        metric: EmbeddingMetricV1,
        normalization: EmbeddingNormalizationV1,
    ) -> AdmittedProjectionArtifactV1 {
        let projection = EmbeddingProjectionKeyV1 {
            model_artifact_digest: digest(artifact_digest),
            tokenizer_digest: digest('b'),
            config_digest: digest('c'),
            query_instruction_digest: Some(digest('d')),
            document_instruction_digest: Some(digest('e')),
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: 8,
            inference_batch_bytes: 16 * 1024,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-test-rev-1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions,
            metric,
            normalization,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: id::<PrivacyDomainId>("privacy.test"),
            privacy_key_epoch: 7,
        }
        .admit()
        .expect("valid test projection");
        AdmittedProjectionArtifactV1 {
            runtime_artifact: VerifiedEmbeddingArtifactV1 {
                projection,
                model_file: "model.onnx".to_string(),
                tokenizer_file: "tokenizer.json".to_string(),
                config_file: "config.json".to_string(),
                artifact: None,
                lifecycle_install: None,
                max_batch_texts: 8,
                max_batch_bytes: 16 * 1024,
                max_threads: 4,
                resident_byte_ceiling: 64 * 1024 * 1024,
                load_deadline_ms: 30_000,
            },
        }
    }

    fn descriptor(authority: &AdmittedProjectionArtifactV1) -> &VerifiedEmbeddingArtifactV1 {
        authority.runtime_artifact()
    }

    fn descriptor_paths(authority: &AdmittedProjectionArtifactV1) -> (&str, &str, &str) {
        let descriptor = descriptor(authority);
        (
            descriptor.model_file.as_str(),
            descriptor.tokenizer_file.as_str(),
            descriptor.config_file.as_str(),
        )
    }

    fn batch(texts: &[&str]) -> BoundedSanitizedTextBatchV1 {
        BoundedSanitizedTextBatchV1::try_new(
            texts.iter().map(|t| (*t).to_string()).collect(),
            64,
            1 << 20,
        )
        .expect("batch within bounds")
    }

    fn never_cancelled() -> ManualCancellation {
        ManualCancellation::new()
    }

    #[test]
    fn private_runtime_descriptor_uses_domain_projection_types() {
        let authority = authority(384);
        let descriptor = descriptor(&authority);
        assert_eq!(descriptor.dimensions(), 384);
        assert_eq!(descriptor.metric(), EmbeddingMetricV1::Cosine);
        assert_eq!(descriptor.normalization(), EmbeddingNormalizationV1::L2);
        assert_eq!(
            descriptor_paths(&authority),
            ("model.onnx", "tokenizer.json", "config.json")
        );
    }

    #[test]
    fn lifecycle_install_authority_verifies_member_bytes_at_read() {
        let fixture = lifecycle_install_fixture(b"model");
        let authority =
            lifecycle_authority_from(&fixture, 1024).expect("verified lifecycle authority");

        assert_eq!(
            authority
                .runtime_artifact()
                .required_member_bytes(ArtifactMemberRoleV1::Model)
                .expect("model bytes"),
            b"model"
        );
        #[cfg(feature = "semantic-fastembed")]
        {
            let runtime = FastEmbedEmbeddingRuntime;
            runtime
                .verify_artifact_compatibility(&authority)
                .expect("production runtime admits the verified lifecycle install");
            assert!(matches!(
                runtime.open_session(&authority),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::LoadFailed,
                    ..
                }))
            ));
        }
        std::fs::write(fixture.install.path().join("tokenizer.json"), b"mutated")
            .expect("corrupt tokenizer");
        assert!(matches!(
            authority
                .runtime_artifact()
                .required_member_bytes(ArtifactMemberRoleV1::Tokenizer),
            Err(EmbedError::Runtime(_))
        ));
    }

    #[test]
    fn lifecycle_authority_construction_reads_no_member_bytes() {
        // The fixture's model member has the pinned length but not the
        // pinned digest. Only reading and hashing the file could detect the
        // mismatch, so the successful construction inside the fixture
        // helper proves zero member byte reads at construction.
        let mismatched = digest_mismatched_lifecycle_authority();
        let authority = mismatched.authority;

        assert!(
            matches!(
                authority
                    .runtime_artifact()
                    .required_member_bytes(ArtifactMemberRoleV1::Model),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "every byte consumption still verifies the digest pin"
        );
        assert!(
            matches!(
                FakeEmbeddingRuntime::new().open_session(&authority),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "the default-runtime session open also rejects digest-mismatched bytes"
        );
        #[cfg(feature = "semantic-fastembed")]
        assert!(
            matches!(
                FastEmbedEmbeddingRuntime.open_session(&authority),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "no session can open over digest-mismatched member bytes"
        );
    }

    #[test]
    fn lifecycle_authority_construction_rejects_structural_pin_violations() {
        let fixture = lifecycle_install_fixture(b"model");
        let model_path = fixture.install.path().join("model.onnx");

        std::fs::write(&model_path, b"model-longer-than-pin").expect("length-mismatched member");
        assert!(
            matches!(
                lifecycle_authority_from(&fixture, 1024),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "a length-pin mismatch fails construction eagerly"
        );

        std::fs::remove_file(&model_path).expect("remove model member");
        assert!(
            matches!(
                lifecycle_authority_from(&fixture, 1024),
                Err(EmbedError::Runtime(RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::CorruptArtifact,
                    ..
                }))
            ),
            "a missing member fails construction eagerly"
        );

        #[cfg(unix)]
        {
            std::fs::write(fixture.install.path().join("model.real"), b"model")
                .expect("symlink target");
            std::os::unix::fs::symlink(fixture.install.path().join("model.real"), &model_path)
                .expect("symlinked member");
            assert!(
                matches!(
                    lifecycle_authority_from(&fixture, 1024),
                    Err(EmbedError::Runtime(RuntimeFailureV1 {
                        kind: RuntimeFailureKindV1::CorruptArtifact,
                        ..
                    }))
                ),
                "a symlinked member fails construction eagerly"
            );
        }
    }

    /// The removed construction work is exactly one [`read_member_bytes`]
    /// pass over every member — still the per-session-open verification —
    /// so construction must do strictly less: it may stat member pins but
    /// never open one for reading. Proven by operations, not wall clocks:
    /// the verification pass must return every member's exact pinned bytes
    /// (impossible without reading all of them), while construction still
    /// succeeds after member read permission is revoked (any
    /// construction-time byte read would fail the constructor and this
    /// test).
    #[test]
    fn lifecycle_authority_construction_is_cheaper_than_member_byte_verification() {
        let fixture = lifecycle_install_fixture(b"model");
        let authority =
            lifecycle_authority_from(&fixture, 1024).expect("verified lifecycle authority");
        for (role, pinned) in [
            (ArtifactMemberRoleV1::Model, b"model".as_slice()),
            (ArtifactMemberRoleV1::Tokenizer, b"tokenizer".as_slice()),
            (ArtifactMemberRoleV1::Config, b"config".as_slice()),
            (
                ArtifactMemberRoleV1::SpecialTokensMap,
                b"special".as_slice(),
            ),
            (
                ArtifactMemberRoleV1::TokenizerConfig,
                b"tokenizer-config".as_slice(),
            ),
        ] {
            assert_eq!(
                authority
                    .runtime_artifact()
                    .required_member_bytes(role)
                    .expect("baseline member byte verification"),
                pinned,
                "one verification pass must consume every member's bytes"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for member in [
                "model.onnx",
                "tokenizer.json",
                "config.json",
                "special_tokens_map.json",
                "tokenizer_config.json",
            ] {
                std::fs::set_permissions(
                    fixture.install.path().join(member),
                    std::fs::Permissions::from_mode(0o000),
                )
                .expect("revoke member read permission");
            }
            assert!(
                std::fs::read(fixture.install.path().join("model.onnx")).is_err(),
                "the fixture requires a test runner whose member reads are deniable"
            );
            let unreadable = lifecycle_authority_from(&fixture, 1024).expect(
                "construction checks structural pins without opening member bytes for reading",
            );
            assert!(
                matches!(
                    unreadable
                        .runtime_artifact()
                        .required_member_bytes(ArtifactMemberRoleV1::Model),
                    Err(EmbedError::Runtime(RuntimeFailureV1 {
                        kind: RuntimeFailureKindV1::CorruptArtifact,
                        ..
                    }))
                ),
                "the byte-verification pass cannot succeed without reading, so the revocation \
                 that construction tolerates provably blocks the read path"
            );
        }
    }

    #[test]
    fn batch_constructor_enforces_bounds() {
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec![], 4, 16),
            Err(EmbedError::EmptyBatch)
        ));
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec!["a".to_string(), "b".to_string()], 1, 16),
            Err(EmbedError::TooManyTexts {
                presented: 2,
                max: 1
            })
        ));
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec!["abcdef".to_string()], 4, 3),
            Err(EmbedError::BatchBytesExceeded {
                presented: 6,
                max: 3
            })
        ));
    }

    #[test]
    fn fake_embed_is_deterministic_across_sessions() {
        let runtime = FakeEmbeddingRuntime::new();
        let authority = authority(16);
        let mut s1 = runtime.open_session(&authority).expect("session 1");
        let mut s2 = runtime.open_session(&authority).expect("session 2");
        let texts = batch(&["fn reserve_stock()", "impl Display for Error"]);
        let cancel = never_cancelled();
        let v1 = s1.embed_batch(&texts, &cancel).expect("embed 1");
        let v2 = s2.embed_batch(&texts, &cancel).expect("embed 2");
        assert_eq!(v1, v2, "same model identity + same text => same vector");
    }

    #[test]
    fn fake_embed_distinguishes_inputs_and_model_identities() {
        let runtime = FakeEmbeddingRuntime::new();
        let authority = authority(16);
        let mut session = runtime.open_session(&authority).expect("session");
        let cancel = never_cancelled();
        let pair = session
            .embed_batch(&batch(&["alpha", "beta"]), &cancel)
            .expect("embed");
        assert_ne!(pair[0].values, pair[1].values, "distinct texts differ");

        let other = authority_with(
            16,
            'f',
            EmbeddingMetricV1::Cosine,
            EmbeddingNormalizationV1::L2,
        );
        let mut other_session = runtime.open_session(&other).expect("other session");
        let other_vec = other_session
            .embed_batch(&batch(&["alpha"]), &cancel)
            .expect("embed other");
        assert_ne!(
            pair[0].values, other_vec[0].values,
            "distinct model identities differ"
        );
    }

    #[test]
    fn echo_dimensions_metric_and_normalization_are_exact() {
        let runtime = FakeEmbeddingRuntime::new();
        let authority = authority_with(
            24,
            'a',
            EmbeddingMetricV1::DotProduct,
            EmbeddingNormalizationV1::L2,
        );
        let mut session = runtime.open_session(&authority).expect("session");
        let vectors = session
            .embed_batch(&batch(&["echo me"]), &never_cancelled())
            .expect("embed");
        assert_eq!(vectors.len(), 1);
        let v = &vectors[0];
        assert_eq!(v.values.len(), 24);
        assert_eq!(v.dimensions, 24);
        assert_eq!(v.metric, EmbeddingMetricV1::DotProduct);
        assert_eq!(v.normalization, EmbeddingNormalizationV1::L2);
        let norm = v.squared_l2_norm().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "L2-normalized vector has unit norm, got {norm}"
        );
    }

    #[test]
    fn unnormalized_echo_stays_raw() {
        let runtime = FakeEmbeddingRuntime::new();
        let authority = authority_with(
            24,
            'a',
            EmbeddingMetricV1::Cosine,
            EmbeddingNormalizationV1::None,
        );
        let mut session = runtime.open_session(&authority).expect("session");
        let vectors = session
            .embed_batch(&batch(&["raw values"]), &never_cancelled())
            .expect("embed");
        assert_eq!(vectors[0].normalization, EmbeddingNormalizationV1::None);
        assert!(
            vectors[0].values.iter().all(|v| (-1.0..1.0).contains(v)),
            "fake raw values stay in [-1, 1)"
        );
    }

    #[test]
    fn cancellation_before_embed_aborts() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut session = runtime.open_session(&authority(8)).expect("session");
        let cancel = ManualCancellation::new();
        cancel.cancel();
        let result = session.embed_batch(&batch(&["a", "b"]), &cancel);
        assert!(matches!(result, Err(EmbedError::Cancelled)));
        assert_eq!(
            runtime.counters().texts_embedded.load(Ordering::SeqCst),
            0,
            "no text embedded after pre-cancel"
        );
    }

    #[test]
    fn deadline_before_embed_surfaces_typed_expiry_without_inference() {
        struct ExpiredAuthority;

        impl SemanticExecutionAuthority for ExpiredAuthority {
            fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
                Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
            }
        }

        let runtime = FakeEmbeddingRuntime::new();
        let mut session = runtime.open_session(&authority(8)).expect("session");
        let result = session.embed_batch(&batch(&["a", "b"]), &ExpiredAuthority);

        assert_eq!(result, Err(EmbedError::DeadlineExceeded));
        assert_eq!(
            runtime.counters().texts_embedded.load(Ordering::SeqCst),
            0,
            "expired work must not enter inference"
        );
    }

    #[test]
    fn cancellation_mid_embed_discards_partial_batch() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut session = runtime.open_session(&authority(8)).expect("session");
        // First poll (before text 1) passes, second poll cancels.
        let cancel = ScriptedCancellation::new(1);
        let result = session.embed_batch(&batch(&["a", "b", "c", "d"]), &cancel);
        assert!(matches!(result, Err(EmbedError::Cancelled)));
        assert_eq!(
            runtime.counters().texts_embedded.load(Ordering::SeqCst),
            1,
            "exactly one text embedded before cancellation; no partial batch returned"
        );
    }

    #[test]
    fn session_enforces_its_own_manifest_batch_ceiling() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut authority = authority(8);
        authority.runtime_artifact.max_batch_texts = 1;
        let mut session = runtime.open_session(&authority).expect("session");
        let result = session.embed_batch(&batch(&["a", "b"]), &never_cancelled());
        assert!(matches!(
            result,
            Err(EmbedError::TooManyTexts {
                presented: 2,
                max: 1
            })
        ));
    }

    #[test]
    fn vector_validation_rejects_bad_shape_and_nonfinite_values() {
        let mut v = EmbeddingVectorV1 {
            values: vec![0.0; 3],
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
        };
        assert!(matches!(
            v.validate(),
            Err(EmbedError::DimensionMismatch {
                expected: 4,
                actual: 3
            })
        ));
        v.dimensions = 3;
        v.values[1] = f32::NAN;
        assert!(matches!(
            v.validate(),
            Err(EmbedError::NonFiniteVectorValue)
        ));
        v.values[1] = f32::INFINITY;
        assert!(matches!(
            v.validate(),
            Err(EmbedError::NonFiniteVectorValue)
        ));
    }

    #[test]
    fn open_failure_is_typed_and_disables_nothing_silently() {
        for kind in [
            RuntimeFailureKindV1::OutOfMemory,
            RuntimeFailureKindV1::CorruptArtifact,
            RuntimeFailureKindV1::RevokedArtifact,
            RuntimeFailureKindV1::IncompatibleRuntime,
            RuntimeFailureKindV1::LoadFailed,
            RuntimeFailureKindV1::EmbedFailed,
        ] {
            let runtime = FakeEmbeddingRuntime::new().with_open_failure(kind);
            let result = runtime.open_session(&authority(8));
            match result {
                Err(EmbedError::Runtime(failure)) => assert_eq!(failure.kind, kind),
                other => panic!("expected typed runtime failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn compatibility_failure_is_typed() {
        let runtime = FakeEmbeddingRuntime::new()
            .with_compatibility_failure(RuntimeFailureKindV1::IncompatibleRuntime);
        let result = runtime.verify_artifact_compatibility(&authority(8));
        match result {
            Err(EmbedError::Runtime(failure)) => {
                assert_eq!(failure.kind, RuntimeFailureKindV1::IncompatibleRuntime);
            }
            other => panic!("expected typed compatibility failure, got {other:?}"),
        }
    }

    #[test]
    fn compatibility_check_consumes_admitted_authority() {
        let runtime = FakeEmbeddingRuntime::new();
        runtime
            .verify_artifact_compatibility(&authority(8))
            .expect("admitted authority is compatible");
        assert_eq!(
            runtime
                .counters()
                .compatibility_checks
                .load(Ordering::SeqCst),
            1
        );
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn real_fastembed_runtime_rejects_unnormalized_projection_before_loading() {
        let runtime = FastEmbedEmbeddingRuntime;
        let result = runtime.verify_artifact_compatibility(&authority_with(
            8,
            'a',
            EmbeddingMetricV1::Cosine,
            EmbeddingNormalizationV1::None,
        ));
        assert!(matches!(
            result,
            Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::IncompatibleRuntime,
                ..
            }))
        ));
    }

    #[test]
    fn fake_reports_resident_bytes_and_close_counters() {
        let runtime = FakeEmbeddingRuntime::new().with_resident_bytes_per_session(4096);
        let counters = runtime.counters();
        {
            let session = runtime.open_session(&authority(8)).expect("session");
            assert_eq!(session.resident_bytes_estimate(), 4096);
            assert_eq!(counters.sessions_opened.load(Ordering::SeqCst), 1);
            assert_eq!(counters.sessions_closed.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            counters.sessions_closed.load(Ordering::SeqCst),
            1,
            "closing a session is observable"
        );
    }
}
