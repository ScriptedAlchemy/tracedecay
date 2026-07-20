//! PR10 quarantined preparation packet `pr10/prep-runtime-adapter`
//! (Plan 31, `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md`).
//!
//! Root-private embedding runtime port surface. This file owns the typed
//! `EmbeddingRuntime` / `EmbeddingSession` ports: load a verified artifact,
//! create bounded sessions, embed bounded sanitized text batches, and return
//! typed vectors whose dimensions, metric, and normalization are echoed from
//! the verified manifest descriptor. It also owns the deterministic
//! [`FakeEmbeddingRuntime`] used by every offline pool/session test.
//!
//! QUARANTINE STATUS: temporarily unlinked. Registration happens at
//! integration by the Sol coordinator; this file must not be referenced from
//! `src/lib.rs` or `src/semantic_code/mod.rs` in this packet. It depends only
//! on the quarantined artifact/manifest packet and domain projection types,
//! and compiles standalone through the PR10 `#[path]` integration test.
//!
//! The real FastEmbed-backed implementation is NOT part of this packet. The
//! port is shaped so that implementation drops in later as another
//! `EmbeddingRuntime` impl: at integration this becomes the only module that
//! imports `fastembed` (Plan 31: "Only one root-private adapter depends on
//! `fastembed`").
//!
//! Design decisions recorded for the coordinator (plan-level ambiguities,
//! flagged rather than guessed):
//!
//! - ESCALATION-1 (sync vs async): Plan 31 does not state whether the runtime
//!   port is async. FastEmbed/ORT inference is blocking CPU work, and PR9's
//!   `src/query/retrieval/ports.rs` sets the precedent that ports are
//!   synchronous contracts with scheduling/cancellation above them. This port
//!   is therefore synchronous; async wrapping is an integration concern.
//! - ESCALATION-2 (manifest/domain vocabulary): the signed artifact manifest
//!   and domain projection key remain separate authorities. Exhaustive bridge
//!   matches below admit them into one private projection-artifact authority;
//!   the runtime defines no duplicate metric/normalization/precision enums.
//! - ESCALATION-3 (projection identity): sessions and runtime descriptors are
//!   created only from the admitted projection-artifact authority. Callers
//!   cannot pair an independent projection identity with an artifact.
//! - ESCALATION-4 (budget type): Plan 31 says deadline/cancellation limits are
//!   fields of the shared PR9 `RetrievalBudget` and PR10 introduces no
//!   semantic-only budget type. That domain type is outside this quarantined
//!   packet, so deadlines are modelled here as a `Duration` against the
//!   injected pool clock and cancellation as the [`CancellationSignal`] trait;
//!   the integrator adapts `RetrievalBudget` onto both.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingTruncationSideV1,
    ManifestDigest,
};

use super::artifact_store::AdmittedArtifactV1;
use super::manifest::{
    ArtifactMemberRoleV1, ArtifactProfileKindV1, DeviceClassV1,
    EmbeddingNormalizationV1 as ManifestNormalizationV1, EmbeddingPoolingV1 as ManifestPoolingV1,
    EmbeddingPrecisionV1 as ManifestPrecisionV1, SemanticMetricV1, Sha256DigestHex,
    TruncationSideV1,
};

/// Typed failure of one embedding operation or runtime admission (Plan 31:
/// load failure, OOM, corruption, revocation, or incompatible pins disables
/// the affected semantic stage; nothing silently substitutes another model).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbedError {
    /// The caller's cancellation signal fired before or during the batch.
    Cancelled,
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

/// Exact signed-manifest pin that failed projection/artifact admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionArtifactPinV1 {
    ArtifactIdentity,
    ManifestIdentity,
    ProfileKind,
    ArtifactDigest,
    TokenizerDigest,
    ConfigDigest,
    QueryInstructionDigest,
    DocumentInstructionDigest,
    Pooling,
    TruncationSide,
    TruncationLength,
    RuntimeBackend,
    RuntimeBuildRevision,
    DeviceClass,
    Dimensions,
    Metric,
    Normalization,
    Precision,
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
    max_batch_texts: u32,
    max_batch_bytes: u32,
    resident_byte_ceiling: u64,
}

impl VerifiedEmbeddingArtifactV1 {
    fn embedding_key(&self) -> &tracedecay_domain::EmbeddingProjectionKeyV1 {
        self.projection.embedding_key()
    }

    fn artifact_digest(&self) -> &ManifestDigest {
        &self.embedding_key().model_artifact_digest
    }

    fn dimensions(&self) -> u32 {
        self.embedding_key().dimensions
    }

    fn metric(&self) -> EmbeddingMetricV1 {
        self.embedding_key().metric
    }

    fn normalization(&self) -> EmbeddingNormalizationV1 {
        self.embedding_key().normalization
    }

    fn max_batch_texts(&self) -> u32 {
        self.max_batch_texts
    }

    fn max_batch_bytes(&self) -> u32 {
        self.max_batch_bytes
    }

    pub(super) fn resident_byte_ceiling(&self) -> u64 {
        self.resident_byte_ceiling
    }
}

/// Single root-private authority pairing a store-admitted artifact with an
/// admitted domain projection. Construction exhaustively checks every pin the
/// signed manifest and projection share before compatibility or session open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdmittedProjectionArtifactV1 {
    runtime_artifact: VerifiedEmbeddingArtifactV1,
}

impl AdmittedProjectionArtifactV1 {
    pub(super) fn admit(
        artifact: &AdmittedArtifactV1,
        projection: &AdmittedEmbeddingProjectionKeyV1,
    ) -> Result<Self, ProjectionArtifactPinV1> {
        let manifest = artifact.manifest();
        let payload = &manifest.payload;
        let key = projection.embedding_key();

        require_pin(
            artifact.artifact_digest() == &manifest.signed_identity_digest(),
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
            key.pooling == bridge_pooling(payload.pooling),
            ProjectionArtifactPinV1::Pooling,
        )?;
        require_pin(
            key.truncation_side == bridge_truncation_side(payload.truncation.side),
            ProjectionArtifactPinV1::TruncationSide,
        )?;
        require_pin(
            key.truncation_length == payload.truncation.max_length
                && key.truncation_length == payload.resource_ceiling.max_sequence_length,
            ProjectionArtifactPinV1::TruncationLength,
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
            key.device_class == bridge_device(payload.device),
            ProjectionArtifactPinV1::DeviceClass,
        )?;
        require_pin(
            key.dimensions == payload.dimensions,
            ProjectionArtifactPinV1::Dimensions,
        )?;
        require_pin(
            key.metric == bridge_metric(payload.metric),
            ProjectionArtifactPinV1::Metric,
        )?;
        require_pin(
            key.normalization == bridge_normalization(payload.normalization),
            ProjectionArtifactPinV1::Normalization,
        )?;
        require_pin(
            key.precision == bridge_precision(payload.precision),
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
                max_batch_texts: payload.resource_ceiling.max_batch_size,
                max_batch_bytes: payload
                    .resource_ceiling
                    .max_batch_size
                    .saturating_mul(payload.resource_ceiling.max_sequence_length)
                    .saturating_mul(4),
                resident_byte_ceiling: payload.resource_ceiling.max_resident_bytes,
            },
        })
    }

    pub(super) fn projection(&self) -> &AdmittedEmbeddingProjectionKeyV1 {
        &self.runtime_artifact.projection
    }

    fn runtime_artifact(&self) -> &VerifiedEmbeddingArtifactV1 {
        &self.runtime_artifact
    }

    pub(super) fn resident_byte_ceiling(&self) -> u64 {
        self.runtime_artifact.resident_byte_ceiling()
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

fn bridge_metric(metric: SemanticMetricV1) -> EmbeddingMetricV1 {
    match metric {
        SemanticMetricV1::Cosine => EmbeddingMetricV1::Cosine,
        SemanticMetricV1::DotProduct => EmbeddingMetricV1::DotProduct,
        SemanticMetricV1::EuclideanL2 => EmbeddingMetricV1::EuclideanL2,
    }
}

fn bridge_normalization(normalization: ManifestNormalizationV1) -> EmbeddingNormalizationV1 {
    match normalization {
        ManifestNormalizationV1::L2 => EmbeddingNormalizationV1::L2,
        ManifestNormalizationV1::None => EmbeddingNormalizationV1::None,
    }
}

fn bridge_pooling(pooling: ManifestPoolingV1) -> EmbeddingPoolingV1 {
    match pooling {
        ManifestPoolingV1::Mean => EmbeddingPoolingV1::Mean,
        ManifestPoolingV1::Cls => EmbeddingPoolingV1::Cls,
        ManifestPoolingV1::LastToken => EmbeddingPoolingV1::LastToken,
        ManifestPoolingV1::MeanSqrtLength => EmbeddingPoolingV1::MeanSqrtLength,
    }
}

fn bridge_precision(precision: ManifestPrecisionV1) -> EmbeddingPrecisionV1 {
    match precision {
        ManifestPrecisionV1::Fp32 => EmbeddingPrecisionV1::Fp32,
        ManifestPrecisionV1::Fp16 => EmbeddingPrecisionV1::Fp16,
        ManifestPrecisionV1::Bf16 => EmbeddingPrecisionV1::Bf16,
        ManifestPrecisionV1::Int8 => EmbeddingPrecisionV1::Int8,
    }
}

fn bridge_device(device: DeviceClassV1) -> EmbeddingDeviceClassV1 {
    match device {
        DeviceClassV1::Cpu => EmbeddingDeviceClassV1::Cpu,
    }
}

fn bridge_truncation_side(side: TruncationSideV1) -> EmbeddingTruncationSideV1 {
    match side {
        TruncationSideV1::Left => EmbeddingTruncationSideV1::Left,
        TruncationSideV1::Right => EmbeddingTruncationSideV1::Right,
    }
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
        let total_bytes: usize = texts.iter().map(|t| t.len()).sum();
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

/// Cancellation surface shared by the runtime and the session pool (Plan 31:
/// sessions are pooled under a bounded cancellation policy; cancellation can
/// never broaden scope or leave a partial batch visible).
pub trait CancellationSignal: Send + Sync {
    fn cancelled(&self) -> bool;
}

/// A manually flipped cancellation flag.
#[derive(Debug, Default)]
pub struct ManualCancellation {
    flag: AtomicBool,
}

impl ManualCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

impl CancellationSignal for ManualCancellation {
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// A deterministic scripted signal that reports cancelled once it has been
/// polled more than `cancel_after` times. `cancel_after = 0` cancels on the
/// first poll. Used to test mid-batch cancellation deterministically.
#[derive(Debug)]
pub struct ScriptedCancellation {
    checks: AtomicUsize,
    cancel_after: usize,
}

impl ScriptedCancellation {
    pub fn new(cancel_after: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_after,
        }
    }
}

impl CancellationSignal for ScriptedCancellation {
    fn cancelled(&self) -> bool {
        let poll = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        poll > self.cancel_after
    }
}

/// One warmed embedding session bound to one verified artifact descriptor
/// (Plan 31: compatible warmed sessions are pooled under bounded memory,
/// concurrency, idle, and cancellation policy).
pub(super) trait EmbeddingSession: Send {
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
        cancel: &dyn CancellationSignal,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError>;
}

/// The root-private embedding runtime port (Plan 31: load verified artifact
/// → create session → embed bounded sanitized batches). The only production
/// implementation will be the FastEmbed adapter in this module; every other
/// crate depends on this trait surface, never on FastEmbed runtime types.
pub(super) trait EmbeddingRuntime {
    type Session: EmbeddingSession;

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

/// Test-observable counters for the deterministic fake runtime.
#[derive(Debug, Default)]
pub struct FakeRuntimeCounters {
    pub compatibility_checks: AtomicUsize,
    pub sessions_opened: AtomicUsize,
    pub sessions_closed: AtomicUsize,
    pub embed_calls: AtomicUsize,
    pub texts_embedded: AtomicUsize,
}

/// Deterministic offline implementation of [`EmbeddingRuntime`] (Plan 31:
/// "fake runtime ports" are part of this preparation packet). It loads no
/// model, performs no I/O and no network access, and produces hash-based
/// pseudo-embeddings with the descriptor's declared dimensions, metric, and
/// normalization. Same descriptor digest + same text always yields the same
/// vector, so all pool/session behavior is testable offline.
#[derive(Debug)]
pub(super) struct FakeEmbeddingRuntime {
    resident_bytes_per_session: u64,
    open_failure: Option<RuntimeFailureKindV1>,
    compatibility_failure: Option<RuntimeFailureKindV1>,
    counters: Arc<FakeRuntimeCounters>,
}

impl Default for FakeEmbeddingRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEmbeddingRuntime {
    pub(super) fn new() -> Self {
        Self {
            resident_bytes_per_session: 1024 * 1024,
            open_failure: None,
            compatibility_failure: None,
            counters: Arc::new(FakeRuntimeCounters::default()),
        }
    }

    pub(super) fn with_resident_bytes_per_session(mut self, bytes: u64) -> Self {
        self.resident_bytes_per_session = bytes;
        self
    }

    pub(super) fn with_open_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.open_failure = Some(kind);
        self
    }

    pub(super) fn with_compatibility_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.compatibility_failure = Some(kind);
        self
    }

    pub(super) fn counters(&self) -> Arc<FakeRuntimeCounters> {
        Arc::clone(&self.counters)
    }
}

impl EmbeddingRuntime for FakeEmbeddingRuntime {
    type Session = FakeEmbeddingSession;

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
        self.counters.sessions_opened.fetch_add(1, Ordering::SeqCst);
        Ok(FakeEmbeddingSession {
            authority: authority.clone(),
            vector_seed: fnv1a64(
                authority
                    .runtime_artifact()
                    .artifact_digest()
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
#[derive(Debug)]
pub struct FakeEmbeddingSession {
    authority: AdmittedProjectionArtifactV1,
    vector_seed: u64,
    resident_bytes: u64,
    counters: Arc<FakeRuntimeCounters>,
}

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
        cancel: &dyn CancellationSignal,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        if batch.is_empty() {
            return Err(EmbedError::EmptyBatch);
        }
        let artifact = self.authority.runtime_artifact();
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
        let mut out = Vec::with_capacity(batch.len());
        for text in batch.texts() {
            // Honor cancellation between texts; a cancelled batch returns no
            // partial results.
            if cancel.cancelled() {
                return Err(EmbedError::Cancelled);
            }
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

impl Drop for FakeEmbeddingSession {
    fn drop(&mut self) {
        self.counters.sessions_closed.fetch_add(1, Ordering::SeqCst);
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

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
/// no HashMap iteration, no clock, no randomness — so results are stable
/// across runs on one platform.
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

#[cfg(test)]
mod tests {
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
                max_batch_texts: 8,
                max_batch_bytes: 16 * 1024,
                resident_byte_ceiling: 64 * 1024 * 1024,
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
                assert_eq!(failure.kind, RuntimeFailureKindV1::IncompatibleRuntime)
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

    #[test]
    fn manifest_bridges_exhaustively_cover_domain_vector_types() {
        assert_eq!(
            [
                bridge_metric(SemanticMetricV1::Cosine),
                bridge_metric(SemanticMetricV1::DotProduct),
                bridge_metric(SemanticMetricV1::EuclideanL2),
            ],
            [
                EmbeddingMetricV1::Cosine,
                EmbeddingMetricV1::DotProduct,
                EmbeddingMetricV1::EuclideanL2,
            ]
        );
        assert_eq!(
            [
                bridge_normalization(ManifestNormalizationV1::L2),
                bridge_normalization(ManifestNormalizationV1::None),
            ],
            [EmbeddingNormalizationV1::L2, EmbeddingNormalizationV1::None,]
        );
        assert_eq!(
            [
                bridge_precision(ManifestPrecisionV1::Fp32),
                bridge_precision(ManifestPrecisionV1::Fp16),
                bridge_precision(ManifestPrecisionV1::Bf16),
                bridge_precision(ManifestPrecisionV1::Int8),
            ],
            [
                EmbeddingPrecisionV1::Fp32,
                EmbeddingPrecisionV1::Fp16,
                EmbeddingPrecisionV1::Bf16,
                EmbeddingPrecisionV1::Int8,
            ]
        );
        assert_eq!(
            [
                bridge_pooling(ManifestPoolingV1::Mean),
                bridge_pooling(ManifestPoolingV1::Cls),
                bridge_pooling(ManifestPoolingV1::LastToken),
                bridge_pooling(ManifestPoolingV1::MeanSqrtLength),
            ],
            [
                EmbeddingPoolingV1::Mean,
                EmbeddingPoolingV1::Cls,
                EmbeddingPoolingV1::LastToken,
                EmbeddingPoolingV1::MeanSqrtLength,
            ]
        );
        assert_eq!(
            [
                bridge_truncation_side(TruncationSideV1::Left),
                bridge_truncation_side(TruncationSideV1::Right),
            ],
            [
                EmbeddingTruncationSideV1::Left,
                EmbeddingTruncationSideV1::Right,
            ]
        );
        assert_eq!(
            bridge_device(DeviceClassV1::Cpu),
            EmbeddingDeviceClassV1::Cpu
        );
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
