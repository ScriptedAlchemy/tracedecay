//! Pure, versioned federated-retrieval kernel contracts for TraceDecay V2.
//!
//! Owning plans:
//! [Plan 15](../../../../docs/plans/tracedecay-v2/15-search-quality-evaluation-and-retrieval-research.md)
//! is the quality and composition authority for these types;
//! [Plan 05](../../../../docs/plans/tracedecay-v2/05-query-crate.md) owns the
//! query execution that composes them;
//! [Plan 25](../../../../docs/plans/tracedecay-v2/25-code-intelligence-indexing-crate.md)
//! owns the query code-generation evidence that code adapters carry.
//!
//! This module contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async
//! work. Field names may change only together with the Plan 15 contract tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::canonical_text::{
    CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within, validated_string_newtype,
};
use crate::code_intelligence::{
    CodeGenerationId, ProjectionKeyV1, SemanticSearchIndexKeyV1, VectorGenerationIdV1,
};
use crate::research::id::{ManifestDigest, PrivacyDomainId, RetrievalAnchorId, digest_id};
use crate::research::time::UtcMicros;
use crate::research::watermark::VectorWatermark;
use crate::research::{DomainError, SessionId, canonical_sha256};
use crate::session::TemporalModeV1;

/// Schema/domain separator for the independently hashed query fallback
/// subpayload (Plan 15, "typed retrieval contract"). The digest field itself
/// is excluded from the hashed bytes.
pub const QUERY_FALLBACK_SUBPAYLOAD_DIGEST_DOMAIN: &str = "tracedecay.query-fallback.v1";
const RETRIEVAL_SCOPE_DIGEST_DOMAIN: &str = "tracedecay.retrieval-scope.v1";
const RETRIEVAL_SNAPSHOT_DIGEST_DOMAIN: &str = "tracedecay.retrieval-snapshot.v1";

/// Reject retrieval identities that are empty, untrimmed, over 512 bytes, or
/// carry control characters.
fn validate_retrieval_identity(
    value: &str,
    field: &'static str,
) -> Result<(), RetrievalContractError> {
    if is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        Ok(())
    } else {
        Err(RetrievalContractError::InvalidIdentity { field })
    }
}

/// Restate a shared integrity-digest rejection as a retrieval-contract error.
///
/// The retrieval kernel makes no distinction between an empty digest and a
/// malformed one; both are simply a non-canonical identity.
fn retrieval_digest_error(error: DomainError) -> RetrievalContractError {
    let field = match error {
        DomainError::Empty { field } | DomainError::NonCanonical { field } => field,
        _ => "retrieval integrity digest",
    };
    RetrievalContractError::InvalidIdentity { field }
}

validated_string_newtype!(
    plain,
    RetrievalContractError,
    validate_retrieval_identity;
    PrincipalId,
    SourceOccurrenceId,
    LogicalEvidenceId,
    SessionOrThreadId,
    LogicalCopyClusterId,
    SourceNamespace,
    SourceInstanceKey,
    ScoreDomainId,
    CalibrationProfileId,
    FusionProfileId,
    DiversityPolicyId,
    RerankPolicyId,
    ComponentRevision,
    ExactAdmissionRuleRevision,
    AuthorizationRevision,
    RankingRevision,
    HydrationRevision,
    RetrievalCursorKeyId,
    EvaluationDecisionId,
);

digest_id!(
    RetrievalContractError, retrieval_digest_error;
    FallbackSubpayloadDigest,
    CandidateSetDigest,
    FreshnessVectorDigest,
    CursorPayloadDigest,
);

/// Opaque HMAC output that identifies one request-local query view without
/// exposing its sanitized bytes.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct QueryMac(String);

fn validate_query_mac(value: &str) -> Result<(), RetrievalContractError> {
    let valid = crate::canonical_text::is_tagged_lowercase_hex(value, "hmac-sha256:", 64);
    if !valid {
        return Err(RetrievalContractError::InvalidIdentity { field: "QueryMac" });
    }
    Ok(())
}

impl QueryMac {
    pub fn new(value: impl Into<String>) -> Result<Self, RetrievalContractError> {
        let value = value.into();
        validate_query_mac(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        validate_query_mac(&self.0)
    }
}

impl<'de> Deserialize<'de> for QueryMac {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for QueryMac {
    type Error = RetrievalContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for QueryMac {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Privacy- and key-epoch-bound identity for an ephemeral sanitized query
/// view. The value is opaque and safe to place only in in-process request
/// state, authenticated cursor identity, and privacy-separated cache keys.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryDigest {
    pub privacy_domain: PrivacyDomainId,
    pub key_epoch: u64,
    pub mac: QueryMac,
}

impl QueryDigest {
    pub fn new(privacy_domain: PrivacyDomainId, key_epoch: u64, mac: QueryMac) -> Self {
        Self {
            privacy_domain,
            key_epoch,
            mac,
        }
    }

    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        self.mac.validate()
    }
}

/// Validation failures for pure retrieval-kernel values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RetrievalContractError {
    #[error("{field} is not a canonical identity")]
    InvalidIdentity { field: &'static str },
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} contains a duplicate identity")]
    Duplicate { field: &'static str },
    #[error("fixed-point arithmetic overflowed in {operation}")]
    FixedPointOverflow { operation: &'static str },
    #[error("a score-domain calibration must have a positive raw score span")]
    InvalidCalibrationRange,
    #[error("the query fallback subpayload may only cover ExactLiteral, Lexical, and Graph lanes")]
    FallbackLaneViolation,
    #[error(
        "the query fallback subpayload must report all three query fallback lanes exactly once"
    )]
    IncompleteFallbackLaneCoverage,
    #[error("{field} is not in canonical order")]
    NonCanonicalOrder { field: &'static str },
    #[error("a retriever batch may contain candidates from only one lane")]
    MixedRetrieverBatch,
    #[error("exact-class candidates require a validated exact admission proof")]
    ExactClassWithoutProof,
    #[error("only the independent exact lane may attach an exact admission proof")]
    ExactProofOutsideExactLane,
    #[error("exact admission proof is not bound to the request {field}")]
    InvalidExactAdmissionBinding { field: &'static str },
    #[error("approximate candidates cannot carry an exact-tier admission decision")]
    UnexpectedExactTierAdmission,
    #[error("batch evidence is missing for a returned occurrence: {field}")]
    MissingOccurrenceEvidence { field: &'static str },
    #[error("batch evidence has no returned occurrence: {field}")]
    UnexpectedOccurrenceEvidence { field: &'static str },
    #[error("cursor binding is inconsistent: {field}")]
    InvalidCursorBinding { field: &'static str },
    #[error("digest does not match the canonical domain-separated payload")]
    DigestMismatch,
    #[error("canonical serialization failed: {0}")]
    CanonicalSerialization(String),
}

impl From<DomainError> for RetrievalContractError {
    fn from(error: DomainError) -> Self {
        Self::CanonicalSerialization(error.to_string())
    }
}

/// Runtime-backed retrieval lanes. Each lane is independently testable,
/// disableable, budgeted, and attributable; one lane is never an alias over
/// another.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverKind {
    ExactLiteral,
    Lexical,
    Semantic,
    Graph,
    Temporal,
    TaskSession,
    Diagnostic,
}

impl RetrieverKind {
    pub const ALL_LANES: [Self; 7] = [
        Self::ExactLiteral,
        Self::Lexical,
        Self::Semantic,
        Self::Graph,
        Self::Temporal,
        Self::TaskSession,
        Self::Diagnostic,
    ];

    /// The lanes admitted to the query fallback subpayload.
    pub const QUERY_FALLBACK_LANES: [Self; 3] = [Self::ExactLiteral, Self::Lexical, Self::Graph];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactLiteral => "exact_literal",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Graph => "graph",
            Self::Temporal => "temporal",
            Self::TaskSession => "task_session",
            Self::Diagnostic => "diagnostic",
        }
    }

    pub const fn is_query_fallback_lane(self) -> bool {
        matches!(self, Self::ExactLiteral | Self::Lexical | Self::Graph)
    }
}

/// Temporal sub-channel retained in evidence explanations.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TemporalCandidateChannelV1 {
    Scope,
    Anchor,
    ExactMessage,
    Phrase,
    Entity,
    Time,
    Lexical,
    Summary,
    Span,
    Burst,
}

/// One temporal ranking contribution retained for explanation and replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalCandidateContributionV1 {
    pub channel: TemporalCandidateChannelV1,
    pub source_occurrence: SourceOccurrenceId,
    pub source_id: Option<String>,
    pub retriever_ordinal: u64,
    pub raw_score: i64,
    pub calibrated_score_micros: u64,
    pub exact_ranges: Vec<crate::session::ByteRangeV1>,
}

/// Compact temporal evidence. It contains no message or summary payload bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalLaneEvidenceV1 {
    pub candidate_anchor: RetrievalAnchorId,
    pub source_occurrence: SourceOccurrenceId,
    pub authorization_revision: AuthorizationRevision,
    pub participant_epoch: ManifestDigest,
    pub session_id: SessionId,
    pub source_id: String,
    pub hydration_anchor: RetrievalAnchorId,
    pub contributions: Vec<TemporalCandidateContributionV1>,
}

/// Deterministic fixed-point score in millionths (Plan 15: "deterministic
/// fixed-point weighted fusion"). No floating point crosses this boundary.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct FixedPointScore(pub u64);

impl FixedPointScore {
    pub const ZERO: Self = Self(0);

    pub const fn micros(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, RetrievalContractError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(RetrievalContractError::FixedPointOverflow { operation: "add" })
    }

    /// `self * weight_micros / 1_000_000` with checked arithmetic.
    pub fn checked_weight(self, weight_micros: u32) -> Result<u64, RetrievalContractError> {
        self.0
            .checked_mul(u64::from(weight_micros))
            .map(|product| product / 1_000_000)
            .ok_or(RetrievalContractError::FixedPointOverflow {
                operation: "weight",
            })
    }
}

/// Versioned calibration curve for one declared raw-score domain. The
/// calibrated feature is always in `[0, 1_000_000]`, while the raw score
/// remains intact in every contribution for audit and replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScoreDomainCalibrationV1 {
    pub calibration_profile_id: CalibrationProfileId,
    pub score_domain: ScoreDomainId,
    pub raw_min_micros: u64,
    pub raw_max_micros: u64,
}

impl ScoreDomainCalibrationV1 {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        if self.raw_max_micros <= self.raw_min_micros {
            return Err(RetrievalContractError::InvalidCalibrationRange);
        }
        Ok(())
    }

    pub fn calibrate(&self, raw_score: FixedPointScore) -> Result<u32, RetrievalContractError> {
        self.validate()?;
        if raw_score.micros() <= self.raw_min_micros {
            return Ok(0);
        }
        if raw_score.micros() >= self.raw_max_micros {
            return Ok(1_000_000);
        }
        let offset = raw_score.micros().checked_sub(self.raw_min_micros).ok_or(
            RetrievalContractError::FixedPointOverflow {
                operation: "calibration offset",
            },
        )?;
        let span = self
            .raw_max_micros
            .checked_sub(self.raw_min_micros)
            .ok_or(RetrievalContractError::InvalidCalibrationRange)?;
        let feature = u128::from(offset) * 1_000_000_u128 / u128::from(span);
        u32::try_from(feature).map_err(|_| RetrievalContractError::FixedPointOverflow {
            operation: "calibration result",
        })
    }
}

/// query scope is explicitly single-root (Plan 25: federation means composing
/// independent evidence lanes within one authorized root; Plan 16 multi-root
/// execution remains future work).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalScope {
    pub privacy_domain: PrivacyDomainId,
    pub root: SingleRootScopeV1,
}

#[derive(Serialize)]
struct RetrievalScopeDigestInput<'a> {
    domain: &'static str,
    scope: &'a RetrievalScope,
}

impl RetrievalScope {
    pub fn compute_digest(&self) -> Result<CandidateSetDigest, RetrievalContractError> {
        let input = RetrievalScopeDigestInput {
            domain: RETRIEVAL_SCOPE_DIGEST_DOMAIN,
            scope: self,
        };
        let digest = canonical_sha256(&input)
            .map_err(|error| RetrievalContractError::CanonicalSerialization(error.to_string()))?;
        CandidateSetDigest::new(digest.as_str())
    }
}

/// One authorized root: the current-project repository/worktree/ref scope
/// resolved by the application layer before any lane executes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SingleRootScopeV1 {
    pub repository: crate::research::id::RepositoryId,
    pub worktree: Option<crate::research::id::WorktreeId>,
    pub reference: Option<crate::research::id::RefId>,
}

/// Frozen execution snapshot: watermarks, index generations, and authorization
/// revision captured once and shared by every lane (Plan 15 pipeline step 1).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalSnapshot {
    pub watermarks: VectorWatermark,
    pub freshness_digest: FreshnessVectorDigest,
    pub authorization_revision: AuthorizationRevision,
    pub captured_at: UtcMicros,
}

#[derive(Serialize)]
struct RetrievalSnapshotDigestInput<'a> {
    domain: &'static str,
    snapshot: &'a RetrievalSnapshot,
}

impl RetrievalSnapshot {
    pub fn compute_digest(&self) -> Result<CandidateSetDigest, RetrievalContractError> {
        let input = RetrievalSnapshotDigestInput {
            domain: RETRIEVAL_SNAPSHOT_DIGEST_DOMAIN,
            snapshot: self,
        };
        let digest = canonical_sha256(&input)
            .map_err(|error| RetrievalContractError::CanonicalSerialization(error.to_string()))?;
        CandidateSetDigest::new(digest.as_str())
    }
}

/// Per-request bounded work budget (Plan 15: deterministic per-lane work
/// budgets/checkpoints plus global resource ceilings).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalBudget {
    pub max_candidates_per_lane: u32,
    pub max_fused_candidates: u32,
    pub max_hydrated_results: u32,
    pub max_hydration_bytes: u64,
    pub deadline_micros: Option<u64>,
}

impl RetrievalBudget {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        if self.max_candidates_per_lane == 0
            || self.max_fused_candidates == 0
            || self.max_hydrated_results == 0
        {
            return Err(RetrievalContractError::Empty {
                field: "retrieval budget",
            });
        }
        Ok(())
    }
}

/// Observed budget consumption, sealed server-side.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalBudgetUsage {
    pub candidates_examined: u64,
    pub candidates_returned: u64,
    pub hydrated_results: u64,
    pub hydration_bytes: u64,
    pub elapsed_micros: u64,
}

/// Public, sanitized budget usage: no lane-identifying counts (Plan 15:
/// public bytes must not distinguish denied from absent evidence).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SanitizedBudgetUsage {
    pub elapsed_micros: u64,
    pub truncated: bool,
}

/// Typed lane failure (Plan 15 `RetrieverOutcome`). Denial is never surfaced
/// as a distinct public state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "failure", content = "detail", rename_all = "snake_case")]
pub enum RetrievalFailure {
    AuthorityUnavailable { detail: String },
    IncompatibleProjection { detail: String },
    StaleSource,
    InvalidRequest { detail: String },
    Internal { detail: String },
}

/// Fatal request-level error (distinct from per-lane typed outcomes).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RetrievalError {
    #[error("a required exact or lexical lane is unavailable")]
    RequiredLaneUnavailable,
    #[error("cursor replay cannot recompute a differently completed candidate set")]
    CursorSetMismatch,
    #[error("cursor authentication failed")]
    CursorAuthenticationFailed,
    #[error("cursor authentication key is unavailable")]
    CursorKeyUnavailable,
    #[error("cursor authentication key was revoked")]
    CursorKeyRevoked,
    #[error("cursor is expired")]
    CursorExpired,
    #[error("request rejected: {0}")]
    InvalidRequest(String),
    #[error("authorization denied the request")]
    Denied,
    #[error("contract violation: {0}")]
    Contract(#[from] RetrievalContractError),
}

/// The typed query request shared by all lanes (Plan 15).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalRequest {
    pub principal: PrincipalId,
    pub scope: RetrievalScope,
    pub temporal_mode: TemporalModeV1,
    pub snapshot: RetrievalSnapshot,
    pub profile_id: FusionProfileId,
    pub budget: RetrievalBudget,
}

/// Source freshness is source- and retriever-specific (Plan 15: there is no
/// global age-decay multiplier). Missing, stale, incompatible, and current
/// are distinct states.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceFreshness {
    pub source_namespace: SourceNamespace,
    pub source_instance: SourceInstanceKey,
    pub source_watermark: Option<u64>,
    pub projection_watermark: Option<u64>,
    pub observed_at: UtcMicros,
    pub source_generation: Option<u64>,
    pub generation_lag: Option<u64>,
    pub compatibility: FreshnessCompatibilityV1,
    pub policy_revision: ComponentRevision,
}

/// Compatibility state of one source/projection pair.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessCompatibilityV1 {
    Current,
    Stale,
    Incompatible,
    Missing,
    Unknown,
}

/// Evidence role used by dedupe/diversity caps (Plan 15: independent
/// corroboration and contradictions are preserved).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    Primary,
    Corroboration,
    Contradiction,
    Context,
}

/// Proof that a typed field admitted a candidate to the exact tier (Plan 15:
/// only the central exact-admission validator can mint this proof; retrievers
/// cannot assign an exact tier). Construct it only through
/// [`ExactAdmissionValidator`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactAdmissionProof {
    pub rule_revision: ExactAdmissionRuleRevision,
    pub field: ExactFieldV1,
    pub original_bytes: Vec<u8>,
    pub canonical_bytes: Vec<u8>,
    pub normalization_steps: Vec<String>,
    pub scope_digest: CandidateSetDigest,
    pub authorization_revision: AuthorizationRevision,
    pub snapshot_digest: CandidateSetDigest,
}

impl ExactAdmissionProof {
    /// Validate the pure proof shape before a lane may attach it to a
    /// candidate. Request-specific scope and snapshot binding is checked by
    /// the central admission authority before minting the proof.
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        self.rule_revision.validate()?;
        self.scope_digest.validate()?;
        self.authorization_revision.validate()?;
        self.snapshot_digest.validate()?;
        if self.original_bytes.is_empty() {
            return Err(RetrievalContractError::Empty {
                field: "exact admission original bytes",
            });
        }
        if self.canonical_bytes.is_empty() {
            return Err(RetrievalContractError::Empty {
                field: "exact admission canonical bytes",
            });
        }
        if self.normalization_steps.iter().any(|step| {
            !crate::canonical_text::is_canonical_text_within(
                step,
                crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
            )
        }) {
            return Err(RetrievalContractError::InvalidIdentity {
                field: "exact admission normalization step",
            });
        }
        Ok(())
    }

    /// Confirm that this proof is bound to the authoritative scope,
    /// authorization revision, and frozen snapshot of `request`.
    pub fn validate_for_request(
        &self,
        request: &RetrievalRequest,
    ) -> Result<(), RetrievalContractError> {
        self.validate()?;
        if self.scope_digest != request.scope.compute_digest()? {
            return Err(RetrievalContractError::InvalidExactAdmissionBinding { field: "scope" });
        }
        if self.authorization_revision != request.snapshot.authorization_revision {
            return Err(RetrievalContractError::InvalidExactAdmissionBinding {
                field: "authorization revision",
            });
        }
        if self.snapshot_digest != request.snapshot.compute_digest()? {
            return Err(RetrievalContractError::InvalidExactAdmissionBinding { field: "snapshot" });
        }
        Ok(())
    }
}

/// The typed fields eligible for exact admission (Plan 15: exact IDs,
/// diagnostic codes and text, symbols, CLI flags, quoted literals, paths,
/// config keys, tool names, commit identifiers, task/session IDs, protocol
/// fields).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExactFieldV1 {
    Identifier,
    QualifiedName,
    Path,
    QuotedPhrase,
    DiagnosticCode,
    DiagnosticText,
    CompilerOrRuntimeError,
    CliFlag,
    ToolName,
    ConfigurationKey,
    CommitIdentifier,
    TaskOrSessionId,
    ProtocolField,
}

/// The exact tiers, lexicographically ordered above all approximate
/// candidates (Plan 15 pipeline step 6). Fusion derives this only from a
/// validated [`ExactAdmissionProof`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ExactClass {
    ExactMessage,
    ExactLiteralPhrase,
    #[default]
    Approximate,
}

/// A compact pre-hydration candidate (Plan 15). Retrieval, fusion, dedupe,
/// and diversity operate on these anchors; payloads hydrate only for the
/// selected result set.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactCandidate {
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub source_occurrence_id: SourceOccurrenceId,
    /// Stable file occurrence when the owning lane is file-backed. `None`
    /// for non-file authorities; never inferred from a source-instance label.
    pub file_occurrence_id: Option<crate::code_intelligence::FileOccurrenceId>,
    pub source_namespace: SourceNamespace,
    pub repository_id: Option<crate::research::id::RepositoryId>,
    pub session_or_thread_id: Option<SessionOrThreadId>,
    pub logical_copy_cluster_id: Option<LogicalCopyClusterId>,
    pub logical_copy_evidence_anchor: Option<RetrievalAnchorId>,
    pub evidence_role: EvidenceRole,
    pub retriever: RetrieverKind,
    pub retriever_revision: ComponentRevision,
    pub score_domain: ScoreDomainId,
    pub raw_score: FixedPointScore,
    pub ordinal_rank: u32,
    pub exact_admission_proof: Option<ExactAdmissionProof>,
    pub retriever_evidence_anchor: RetrievalAnchorId,
    pub freshness: SourceFreshness,
}

impl CompactCandidate {
    pub fn exact_class(&self) -> ExactClass {
        match &self.exact_admission_proof {
            Some(proof) if proof.field == ExactFieldV1::QuotedPhrase => {
                ExactClass::ExactLiteralPhrase
            }
            Some(_) => ExactClass::ExactMessage,
            None => ExactClass::Approximate,
        }
    }
}

/// One lane's committed candidate prefix plus its typed evidence (Plan 15:
/// exactly one typed evidence value per returned `source_occurrence_id`;
/// missing, extra, or duplicate evidence rejects the batch).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverBatch<E> {
    pub candidates: Vec<CompactCandidate>,
    pub evidence_by_occurrence: BTreeMap<SourceOccurrenceId, E>,
    pub coverage: RetrieverCoverage,
    pub continuation: Option<RetrieverContinuation>,
}

impl<E> RetrieverBatch<E> {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        let mut returned_occurrences = BTreeSet::new();
        let lane = self
            .candidates
            .first()
            .map(|candidate| candidate.retriever)
            .or_else(|| {
                self.continuation
                    .as_ref()
                    .map(|continuation| continuation.lane)
            });
        for (expected_ordinal, candidate) in self.candidates.iter().enumerate() {
            if Some(candidate.retriever) != lane {
                return Err(RetrievalContractError::MixedRetrieverBatch);
            }
            match (candidate.retriever, &candidate.exact_admission_proof) {
                (RetrieverKind::ExactLiteral, Some(proof)) => proof.validate()?,
                (RetrieverKind::ExactLiteral, None) => {
                    return Err(RetrievalContractError::ExactClassWithoutProof);
                }
                (_, Some(_)) => {
                    return Err(RetrievalContractError::ExactProofOutsideExactLane);
                }
                (_, None) => {}
            }
            if candidate.ordinal_rank != expected_ordinal as u32 {
                return Err(RetrievalContractError::NonCanonicalOrder {
                    field: "retriever batch candidate ordinals",
                });
            }
            if !returned_occurrences.insert(&candidate.source_occurrence_id) {
                return Err(RetrievalContractError::Duplicate {
                    field: "retriever batch source occurrences",
                });
            }
            if !self
                .evidence_by_occurrence
                .contains_key(&candidate.source_occurrence_id)
            {
                return Err(RetrievalContractError::MissingOccurrenceEvidence {
                    field: "retriever batch evidence",
                });
            }
        }
        if self
            .continuation
            .as_ref()
            .map(|continuation| continuation.lane)
            != lane
            && self.continuation.is_some()
        {
            return Err(RetrievalContractError::MixedRetrieverBatch);
        }
        if self
            .evidence_by_occurrence
            .keys()
            .any(|occurrence| !returned_occurrences.contains(occurrence))
        {
            return Err(RetrievalContractError::UnexpectedOccurrenceEvidence {
                field: "retriever batch evidence",
            });
        }
        Ok(())
    }
}

/// Per-lane coverage counters (Plan 15: every lane reports examined,
/// eligible, excluded, capped, and unknown coverage independently).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverCoverage {
    pub examined: u64,
    pub eligible: u64,
    pub excluded: u64,
    pub capped: u64,
    pub unknown: u64,
}

/// Deterministic per-lane continuation checkpoint. A lane contributes its
/// entire admitted prefix only when the checkpoint completes; scheduler
/// interleaving or timing jitter cannot select a different prefix.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverContinuation {
    pub lane: RetrieverKind,
    pub checkpoint_digest: CursorPayloadDigest,
    pub exhausted: bool,
}

/// Per-lane typed outcome (Plan 15). `Denied` exists only in sealed internal
/// outcomes; public statuses coalesce denied and absent evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", content = "value", rename_all = "snake_case")]
pub enum RetrieverOutcome<T> {
    Complete(T),
    Partial { value: T, reason: RetrievalFailure },
    Unavailable(RetrievalFailure),
    Denied,
    Stale(SourceFreshness),
    BudgetExceeded(RetrievalBudgetUsage),
    TimedOut(RetrievalBudgetUsage),
    Cancelled,
}

/// The single generic retriever port (Plan 15: `src/query/retrieval/ports.rs`
/// owns the composition; this crate owns the pure contract). `R` is the
/// lane's typed request; `E` is the lane's typed per-occurrence evidence.
pub trait Retriever<R, E> {
    /// Retrieve one committed candidate prefix against the pinned snapshot.
    ///
    /// Implementations are provided by root query adapters (Plan 05/Plan 15),
    /// never by this crate.
    fn retrieve(&self, request: &R) -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalError>;
}

/// The sole authority that may mint an [`ExactAdmissionProof`] (Plan 15).
/// Implemented once, centrally; lane adapters consume proofs, they never
/// construct them.
pub trait ExactAdmissionValidator {
    /// Admit `candidate_bytes` for `field` under the pinned scope, snapshot,
    /// and authorization revision, or reject admission.
    fn admit(
        &self,
        field: ExactFieldV1,
        candidate_bytes: &[u8],
        request: &RetrievalRequest,
    ) -> Result<Option<ExactAdmissionProof>, RetrievalError>;
}

/// One retriever's scored contribution to a fused candidate (Plan 15: every
/// ranked candidate retains every retriever's raw score domain, ordinal rank,
/// calibrated feature, weight, and weighted contribution).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateContribution {
    pub retriever: RetrieverKind,
    pub retriever_revision: ComponentRevision,
    pub source_occurrence_id: SourceOccurrenceId,
    pub ordinal_rank: u32,
    pub raw_score: FixedPointScore,
    pub score_domain: ScoreDomainId,
    pub calibration_profile_id: CalibrationProfileId,
    pub calibrated_feature_micros: u32,
    pub weight_micros: u32,
    pub weighted_contribution_micros: u64,
}

/// Structured occurrence provenance retained through fusion (Plan 15: fusion
/// preserves each exact `(source_occurrence_id, retriever_evidence_anchor)`
/// pair; parallel unassociated provenance vectors are forbidden).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceProvenance {
    pub source_occurrence_id: SourceOccurrenceId,
    pub file_occurrence_id: Option<crate::code_intelligence::FileOccurrenceId>,
    pub retriever_evidence_anchor: RetrievalAnchorId,
    pub source_namespace: SourceNamespace,
    pub repository_id: Option<crate::research::id::RepositoryId>,
    pub session_or_thread_id: Option<SessionOrThreadId>,
    pub logical_copy_cluster_id: Option<LogicalCopyClusterId>,
    pub logical_copy_evidence_anchor: Option<RetrievalAnchorId>,
    pub evidence_role: EvidenceRole,
    pub freshness: SourceFreshness,
}

/// A candidate after contribution grouping and fixed-point fusion.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FusedCandidate {
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub occurrences: Vec<OccurrenceProvenance>,
    pub exact_class: ExactClass,
    pub utility_micros: u64,
    pub contributions: Vec<CandidateContribution>,
    pub freshness: Vec<SourceFreshness>,
    pub decisions: Vec<RankingDecision>,
}

impl FusedCandidate {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        let exact_decisions = self
            .decisions
            .iter()
            .filter(|decision| decision.kind == RankingDecisionKind::ExactTierAdmission);
        if self.exact_class == ExactClass::Approximate {
            if exact_decisions.count() != 0 {
                return Err(RetrievalContractError::UnexpectedExactTierAdmission);
            }
            return Ok(());
        }

        let mut found_admission = false;
        for decision in exact_decisions {
            found_admission = true;
            let evidence_is_bound = decision.evidence_anchor.as_ref().is_some_and(|anchor| {
                self.occurrences
                    .iter()
                    .any(|occurrence| occurrence.retriever_evidence_anchor == *anchor)
            });
            if decision.retriever != Some(RetrieverKind::ExactLiteral)
                || decision.policy_anchor.is_none()
                || !evidence_is_bound
            {
                return Err(RetrievalContractError::ExactClassWithoutProof);
            }
        }
        if !found_admission {
            return Err(RetrievalContractError::ExactClassWithoutProof);
        }
        Ok(())
    }
}

/// A fused candidate with its final deterministic ordinal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RankedCandidate {
    pub candidate: FusedCandidate,
    pub final_ordinal: u32,
}

/// One recorded ranking decision (Plan 15: explanations are rendered from
/// this provenance, never reconstructed from a final scalar score).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RankingDecision {
    pub kind: RankingDecisionKind,
    pub retriever: Option<RetrieverKind>,
    pub policy_anchor: Option<RetrievalAnchorId>,
    pub evidence_anchor: Option<RetrievalAnchorId>,
    pub detail: String,
}

/// The decision kinds the pipeline must record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RankingDecisionKind {
    ExactTierAdmission,
    SameSourceDuplicateCollapse,
    LogicalCopyRepresentativeSelection,
    ContradictionPreservation,
    DiversityCap,
    ComparatorProvenance,
    RerankAdmission,
    Fallback,
}

/// A versioned fusion profile backed by an immutable locked evaluation
/// result (Plan 15: no constant or weight is production authority before
/// Plan 15 accepts it).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FusionProfile {
    pub profile_id: FusionProfileId,
    pub evaluation_result_anchor: RetrievalAnchorId,
    pub calibrations: BTreeMap<RetrieverKind, CalibrationProfileId>,
    pub score_domain_calibrations: BTreeMap<ScoreDomainId, ScoreDomainCalibrationV1>,
    pub weights_micros: BTreeMap<RetrieverKind, u32>,
    pub diversity_policy_id: DiversityPolicyId,
    pub rerank_policy_id: Option<RerankPolicyId>,
    pub retrieval_budget: RetrievalBudget,
}

/// Profile-owned deterministic caps applied after fusion (Plan 15 pipeline
/// step 9). A cap must carry its locked evaluation anchor; absent evidence
/// leaves the cap disabled except resource-safety ceilings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiversityPolicy {
    pub policy_id: DiversityPolicyId,
    pub evaluation_result_anchor: Option<RetrievalAnchorId>,
    pub per_source_namespace: Option<u32>,
    pub per_source_instance: Option<u32>,
    pub per_repository: Option<u32>,
    pub per_file: Option<u32>,
    pub per_session_or_thread: Option<u32>,
    pub per_copy_cluster: Option<u32>,
    pub per_evidence_role: Option<u32>,
}

/// Optional bounded rerank contract (Plan 15: exact tiers bypass the
/// reranker; failure returns the exact pre-rerank order with a typed reason).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RerankPolicy {
    pub policy_id: RerankPolicyId,
    pub evaluation_result_anchor: RetrievalAnchorId,
    pub max_candidates: u32,
    pub max_input_bytes: u64,
    pub max_input_tokens: u64,
    pub max_work_units: u64,
    pub max_model_invocations: u32,
    pub deadline_micros: Option<u64>,
}

/// Ephemeral authorized rerank view (Plan 15 pipeline step 10): only approved
/// source-local text or token features, never cached or persisted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedRerankView {
    pub anchor_id: RetrievalAnchorId,
    pub snapshot_digest: CandidateSetDigest,
    pub privacy_domain: PrivacyDomainId,
    pub compatibility: FreshnessCompatibilityV1,
    pub approved_features: Vec<u8>,
}

/// Per-anchor hydration receipt (Plan 15: every contribution and hydration
/// receipt keys back to one `OccurrenceProvenance`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HydrationReceipt {
    pub anchor_id: RetrievalAnchorId,
    pub source_occurrence_id: SourceOccurrenceId,
    pub hydration_revision: HydrationRevision,
    pub bytes_hydrated: u64,
    pub authorized: bool,
    pub freshness: SourceFreshness,
}

/// Authenticated retrieval cursor (Plan 15: binds the query snapshot, profile
/// ID, authorized freshness digest, authorization revision, ordered candidate
/// set digest, sanitized lane statuses, and lane checkpoints; resume uses the
/// bound set or rejects, it never recomputes).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRetrievalContinuationV1 {
    pub profile_id: FusionProfileId,
    pub profile_digest: ManifestDigest,
    pub code_generation: CodeGenerationId,
    pub vector_generation: VectorGenerationIdV1,
    pub projection_key: ProjectionKeyV1,
    pub search_index_key: SemanticSearchIndexKeyV1,
    pub candidate_set_digest: CandidateSetDigest,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub lane_checkpoints: Vec<RetrieverContinuation>,
    pub ranking_revision: RankingRevision,
    pub rerank: OptionalStagePublicStatus,
    pub ordered_candidate_anchors: Vec<RetrievalAnchorId>,
    pub next_ordinal: u32,
}

impl SemanticRetrievalContinuationV1 {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        self.search_index_key.validate().map_err(|_| {
            RetrievalContractError::InvalidCursorBinding {
                field: "semantic search index key",
            }
        })?;
        if !self
            .public_lane_statuses
            .contains_key(&RetrieverKind::Semantic)
        {
            return Err(RetrievalContractError::InvalidCursorBinding {
                field: "semantic lane status",
            });
        }
        if self
            .lane_checkpoints
            .iter()
            .any(|checkpoint| !self.public_lane_statuses.contains_key(&checkpoint.lane))
        {
            return Err(RetrievalContractError::InvalidCursorBinding {
                field: "semantic lane checkpoint without admitted lane status",
            });
        }
        let unique_anchors = self
            .ordered_candidate_anchors
            .iter()
            .collect::<BTreeSet<_>>();
        if unique_anchors.len() != self.ordered_candidate_anchors.len()
            || usize::try_from(self.next_ordinal)
                .ok()
                .is_none_or(|next| next > self.ordered_candidate_anchors.len())
        {
            return Err(RetrievalContractError::InvalidCursorBinding {
                field: "semantic frozen candidate order",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSourceCursorBindingV1 {
    pub reference: crate::research::id::RefId,
    pub commit: crate::GitOidV1,
    pub tree: crate::GitOidV1,
    pub generation: crate::code_intelligence::CodeGenerationId,
}

impl CodeSourceCursorBindingV1 {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        self.reference.validate()?;
        self.commit.validate()?;
        self.tree.validate()?;
        self.generation.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalCursor {
    pub key_id: RetrievalCursorKeyId,
    pub key_epoch: u64,
    pub privacy_domain: PrivacyDomainId,
    pub query_digest: QueryDigest,
    pub profile_id: FusionProfileId,
    pub snapshot_digest: CandidateSetDigest,
    pub freshness_digest: FreshnessVectorDigest,
    pub authorization_revision: AuthorizationRevision,
    pub candidate_set_digest: CandidateSetDigest,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub lane_checkpoints: Vec<RetrieverContinuation>,
    pub ranking_revision: RankingRevision,
    /// First final ordinal in the next page of the frozen candidate set.
    pub next_ordinal: u32,
    /// Optional semantic continuation authenticated by the same query cursor key.
    /// Its absence preserves the canonical query cursor bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticRetrievalContinuationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_source: Option<CodeSourceCursorBindingV1>,
    pub expiry: UtcMicros,
    pub signature: QueryMac,
}

impl RetrievalCursor {
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        self.key_id.validate()?;
        self.query_digest.validate()?;
        self.signature.validate()?;
        if let Some(binding) = &self.code_source {
            binding.validate()?;
        }
        if self.query_digest.key_epoch != self.key_epoch
            || self.query_digest.privacy_domain != self.privacy_domain
        {
            return Err(RetrievalContractError::InvalidCursorBinding {
                field: "query privacy/key binding",
            });
        }
        if self
            .lane_checkpoints
            .iter()
            .any(|checkpoint| !self.public_lane_statuses.contains_key(&checkpoint.lane))
        {
            return Err(RetrievalContractError::InvalidCursorBinding {
                field: "lane checkpoint without admitted lane status",
            });
        }
        if let Some(semantic) = &self.semantic {
            semantic.validate()?;
        }
        Ok(())
    }
}

/// Public per-lane status (Plan 15: coalesces denied and nonexistent
/// evidence; omits unauthorized freshness, counts, timing, cap effects, and
/// failure details).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PublicRetrieverStatus {
    Complete,
    Partial,
    Unavailable,
    Stale,
}

/// Public status of an optional stage (Plan 15: deliberately no denied
/// variant — denied and absent coalesce through the same sanitized
/// unavailable shape).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum OptionalStagePublicStatus {
    NotRequested,
    Complete,
    Unavailable(SanitizedStageFailure),
    Rejected(SanitizedStageFailure),
    Cancelled,
    BudgetExceeded(SanitizedBudgetUsage),
}

/// Sanitized optional-stage failure: class only, no internal detail.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SanitizedStageFailure {
    AuthorityUnavailable,
    Incompatible,
    Stale,
    Invalid,
    Internal,
}

/// Semantic/rerank outcome reported outside the query fallback subpayload
/// (Plan 15). It may never change the subpayload, its digest, or cursor
/// identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRerankOutcome {
    pub semantic: OptionalStagePublicStatus,
    pub rerank: OptionalStagePublicStatus,
}

/// The typed, independently hashed query fallback subpayload (Plan 15/SEMANTIC
/// boundary). Canonical-encoded and hashed with
/// [`QUERY_FALLBACK_SUBPAYLOAD_DIGEST_DOMAIN`]; the `digest` field is excluded
/// from the hashed bytes. It contains the complete accepted
/// exact+lexical+graph result — IDs, order, contributions, explanations,
/// coverage, and cursor bytes. semantic must preserve it byte-for-byte whenever
/// the semantic or rerank stage is disabled, unavailable, rejected, or
/// cancelled.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryFallbackSubpayload {
    pub profile_id: FusionProfileId,
    pub ordered_candidates: Vec<RankedCandidate>,
    pub public_fallback_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub cursor: Option<RetrievalCursor>,
    pub digest: FallbackSubpayloadDigest,
}

#[derive(Serialize)]
struct QueryFallbackSubpayloadDigestInput<'a> {
    domain: &'static str,
    profile_id: &'a FusionProfileId,
    ordered_candidates: &'a [RankedCandidate],
    public_fallback_lane_coverage: &'a BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    freshness: &'a [SourceFreshness],
    cursor: &'a Option<RetrievalCursor>,
}

impl QueryFallbackSubpayload {
    /// Construct one canonical query fallback payload and compute its
    /// domain-separated digest without exposing any placeholder identity.
    pub fn new(
        profile_id: FusionProfileId,
        ordered_candidates: Vec<RankedCandidate>,
        public_fallback_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
        freshness: Vec<SourceFreshness>,
        cursor: Option<RetrievalCursor>,
    ) -> Result<Self, RetrievalContractError> {
        let digest = compute_query_fallback_subpayload_digest(
            &profile_id,
            &ordered_candidates,
            &public_fallback_lane_coverage,
            &freshness,
            &cursor,
        )?;
        let payload = Self {
            profile_id,
            ordered_candidates,
            public_fallback_lane_coverage,
            freshness,
            cursor,
            digest,
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Validate the query lane invariant: the subpayload covers only
    /// `ExactLiteral`, `Lexical`, and `Graph` (Plan 15).
    pub fn validate(&self) -> Result<(), RetrievalContractError> {
        let actual_lanes: BTreeSet<_> =
            self.public_fallback_lane_coverage.keys().copied().collect();
        let expected_lanes: BTreeSet<_> = RetrieverKind::QUERY_FALLBACK_LANES.into_iter().collect();
        if actual_lanes
            .iter()
            .any(|lane| !lane.is_query_fallback_lane())
        {
            return Err(RetrievalContractError::FallbackLaneViolation);
        }
        if actual_lanes != expected_lanes {
            return Err(RetrievalContractError::IncompleteFallbackLaneCoverage);
        }
        if let Some(cursor) = &self.cursor {
            cursor.validate()?;
            if cursor.profile_id != self.profile_id {
                return Err(RetrievalContractError::InvalidCursorBinding {
                    field: "fallback cursor profile",
                });
            }
            if cursor.public_lane_statuses != self.public_fallback_lane_coverage
                || cursor
                    .public_lane_statuses
                    .keys()
                    .any(|lane| !lane.is_query_fallback_lane())
            {
                return Err(RetrievalContractError::InvalidCursorBinding {
                    field: "fallback cursor lane statuses",
                });
            }
        }
        for (expected_ordinal, ranked) in self.ordered_candidates.iter().enumerate() {
            if ranked.final_ordinal != expected_ordinal as u32 {
                return Err(RetrievalContractError::NonCanonicalOrder {
                    field: "fallback candidate ordinals",
                });
            }
            ranked.candidate.validate()?;
            if ranked
                .candidate
                .contributions
                .iter()
                .any(|contribution| !contribution.retriever.is_query_fallback_lane())
                || ranked.candidate.decisions.iter().any(|decision| {
                    decision
                        .retriever
                        .is_some_and(|retriever| !retriever.is_query_fallback_lane())
                })
            {
                return Err(RetrievalContractError::FallbackLaneViolation);
            }
        }
        self.verify_digest()
    }

    /// Compute the canonical domain-separated digest of this subpayload,
    /// excluding the `digest` field itself.
    pub fn compute_digest(&self) -> Result<FallbackSubpayloadDigest, RetrievalContractError> {
        compute_query_fallback_subpayload_digest(
            &self.profile_id,
            &self.ordered_candidates,
            &self.public_fallback_lane_coverage,
            &self.freshness,
            &self.cursor,
        )
    }

    /// Verify the stored digest against the canonical payload.
    pub fn verify_digest(&self) -> Result<(), RetrievalContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(RetrievalContractError::DigestMismatch)
        }
    }
}

fn compute_query_fallback_subpayload_digest(
    profile_id: &FusionProfileId,
    ordered_candidates: &[RankedCandidate],
    public_fallback_lane_coverage: &BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    freshness: &[SourceFreshness],
    cursor: &Option<RetrievalCursor>,
) -> Result<FallbackSubpayloadDigest, RetrievalContractError> {
    let input = QueryFallbackSubpayloadDigestInput {
        domain: QUERY_FALLBACK_SUBPAYLOAD_DIGEST_DOMAIN,
        profile_id,
        ordered_candidates,
        public_fallback_lane_coverage,
        freshness,
        cursor,
    };
    let digest = canonical_sha256(&input)
        .map_err(|error| RetrievalContractError::CanonicalSerialization(error.to_string()))?;
    FallbackSubpayloadDigest::new(digest.as_str())
}

/// The assembled retrieval result (Plan 15 pipeline step 12).
/// `internal_lane_outcomes` is sealed server-side audit data: excluded from
/// fallback bytes/digest, cursors, public coverage, and cache keys.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalResult {
    pub snapshot: RetrievalSnapshot,
    pub profile_id: FusionProfileId,
    pub query_fallback: QueryFallbackSubpayload,
    pub ordered_candidates: Vec<RankedCandidate>,
    #[serde(skip)]
    pub internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    pub public_lane_coverage: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub semantic_rerank_outcome: SemanticRerankOutcome,
    pub hydration_receipts: Vec<HydrationReceipt>,
    pub cursor: Option<RetrievalCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    const ONE_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn freshness() -> SourceFreshness {
        SourceFreshness {
            source_namespace: id("ns.fixture"),
            source_instance: id("instance.fixture"),
            source_watermark: Some(7),
            projection_watermark: Some(7),
            observed_at: UtcMicros(1),
            source_generation: Some(3),
            generation_lag: Some(0),
            compatibility: FreshnessCompatibilityV1::Current,
            policy_revision: id("policy.fixture.v1"),
        }
    }

    fn subpayload(lanes: &[RetrieverKind]) -> QueryFallbackSubpayload {
        let mut payload = QueryFallbackSubpayload {
            profile_id: id("profile.fixture.v1"),
            ordered_candidates: vec![],
            public_fallback_lane_coverage: lanes
                .iter()
                .map(|lane| (*lane, PublicRetrieverStatus::Complete))
                .collect(),
            freshness: vec![freshness()],
            cursor: None,
            digest: id(ZERO_DIGEST),
        };
        payload.digest = payload.compute_digest().expect("digest computable");
        payload
    }

    fn candidate(
        occurrence: &str,
        retriever: RetrieverKind,
        ordinal_rank: u32,
    ) -> CompactCandidate {
        CompactCandidate {
            anchor_id: crate::research::id::RetrievalAnchorId::new(format!("anchor.{occurrence}"))
                .unwrap(),
            logical_evidence_id: id(&format!("evidence.{occurrence}")),
            source_occurrence_id: id(occurrence),
            file_occurrence_id: None,
            source_namespace: id("ns.fixture"),
            repository_id: None,
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever,
            retriever_revision: id("retriever.fixture.v1"),
            score_domain: id("score.fixture.v1"),
            raw_score: FixedPointScore(1),
            ordinal_rank,
            exact_admission_proof: None,
            retriever_evidence_anchor: crate::research::id::RetrievalAnchorId::new(format!(
                "evidence-anchor.{occurrence}"
            ))
            .unwrap(),
            freshness: freshness(),
        }
    }

    fn provenance(candidate: &CompactCandidate) -> OccurrenceProvenance {
        OccurrenceProvenance {
            source_occurrence_id: candidate.source_occurrence_id.clone(),
            file_occurrence_id: candidate.file_occurrence_id.clone(),
            retriever_evidence_anchor: candidate.retriever_evidence_anchor.clone(),
            source_namespace: candidate.source_namespace.clone(),
            repository_id: candidate.repository_id.clone(),
            session_or_thread_id: candidate.session_or_thread_id.clone(),
            logical_copy_cluster_id: candidate.logical_copy_cluster_id.clone(),
            logical_copy_evidence_anchor: candidate.logical_copy_evidence_anchor.clone(),
            evidence_role: candidate.evidence_role,
            freshness: candidate.freshness.clone(),
        }
    }

    #[test]
    fn fallback_subpayload_admits_only_fallback_lanes() {
        let accepted = subpayload(&[
            RetrieverKind::ExactLiteral,
            RetrieverKind::Lexical,
            RetrieverKind::Graph,
        ]);
        accepted
            .validate()
            .expect("query fallback lanes are admissible");

        let lane = RetrieverKind::Semantic;
        let rejected = subpayload(&[lane]);
        assert_eq!(
            rejected.validate(),
            Err(RetrievalContractError::FallbackLaneViolation),
            "lane {lane:?} must not enter the query fallback subpayload"
        );
    }

    #[test]
    fn retriever_contract_names_every_runtime_lane() {
        assert_eq!(
            RetrieverKind::ALL_LANES,
            [
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Semantic,
                RetrieverKind::Graph,
                RetrieverKind::Temporal,
                RetrieverKind::TaskSession,
                RetrieverKind::Diagnostic,
            ],
        );
        for (wire, expected) in [
            ("exact_literal", RetrieverKind::ExactLiteral),
            ("lexical", RetrieverKind::Lexical),
            ("semantic", RetrieverKind::Semantic),
            ("graph", RetrieverKind::Graph),
            ("temporal", RetrieverKind::Temporal),
            ("task_session", RetrieverKind::TaskSession),
            ("diagnostic", RetrieverKind::Diagnostic),
        ] {
            assert_eq!(
                serde_json::from_str::<RetrieverKind>(&format!("\"{wire}\""))
                    .expect("canonical runtime lane"),
                expected,
            );
            assert_eq!(
                serde_json::to_string(&expected).expect("serialize runtime lane"),
                format!("\"{wire}\""),
            );
        }
        assert_eq!(
            RetrieverKind::QUERY_FALLBACK_LANES,
            [
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
            ],
            "task/session evidence must never broaden query fallback",
        );
    }

    #[test]
    fn retriever_outcome_keeps_deadlines_distinct_from_cancellation() {
        let usage = RetrievalBudgetUsage {
            elapsed_micros: 10_000,
            ..RetrievalBudgetUsage::default()
        };
        let timed_out = RetrieverOutcome::<()>::TimedOut(usage);
        let cancelled = RetrieverOutcome::<()>::Cancelled;

        assert_ne!(timed_out, cancelled);
        assert_eq!(
            serde_json::to_value(timed_out).expect("serialize timeout")["outcome"],
            "timed_out",
        );
    }

    #[test]
    fn fallback_subpayload_requires_all_fallback_lanes_and_a_matching_digest() {
        let incomplete = subpayload(&[RetrieverKind::ExactLiteral, RetrieverKind::Lexical]);
        assert!(incomplete.validate().is_err());

        let mut stale_digest = subpayload(&RetrieverKind::QUERY_FALLBACK_LANES);
        stale_digest.profile_id = id("profile.changed.v1");
        assert_eq!(
            stale_digest.validate(),
            Err(RetrievalContractError::DigestMismatch)
        );
    }

    #[test]
    fn fallback_subpayload_rejects_noncanonical_ordinals_and_non_query_contributions() {
        let mut payload = subpayload(&RetrieverKind::QUERY_FALLBACK_LANES);
        payload.ordered_candidates = vec![RankedCandidate {
            candidate: FusedCandidate {
                anchor_id: crate::research::id::RetrievalAnchorId::new("anchor.fused").unwrap(),
                logical_evidence_id: id("evidence.fused"),
                occurrences: vec![],
                exact_class: ExactClass::Approximate,
                utility_micros: 1,
                contributions: vec![CandidateContribution {
                    retriever: RetrieverKind::Semantic,
                    retriever_revision: id("retriever.semantic.v1"),
                    source_occurrence_id: id("occurrence.semantic"),
                    ordinal_rank: 0,
                    raw_score: FixedPointScore(1),
                    score_domain: id("score.semantic.v1"),
                    calibration_profile_id: id("calibration.semantic.v1"),
                    calibrated_feature_micros: 1,
                    weight_micros: 1,
                    weighted_contribution_micros: 1,
                }],
                freshness: vec![freshness()],
                decisions: vec![],
            },
            final_ordinal: 1,
        }];
        payload.digest = payload.compute_digest().unwrap();
        assert!(payload.validate().is_err());
    }

    #[test]
    fn fallback_subpayload_digest_is_domain_separated_and_self_verifying() {
        let mut payload = subpayload(&[RetrieverKind::ExactLiteral]);
        payload.digest = payload.compute_digest().expect("digest computable");
        payload.verify_digest().expect("digest verifies");
        assert_eq!(payload.compute_digest().unwrap(), payload.digest);

        payload.profile_id = id("profile.other.v1");
        assert_eq!(
            payload.verify_digest(),
            Err(RetrievalContractError::DigestMismatch)
        );
    }

    #[test]
    fn fallback_subpayload_digest_excludes_the_digest_field() {
        let mut payload = subpayload(&[RetrieverKind::Lexical]);
        let first = payload.compute_digest().unwrap();
        payload.digest = id(ONE_DIGEST);
        assert_eq!(payload.compute_digest().unwrap(), first);
    }

    #[test]
    fn fixed_point_score_uses_checked_arithmetic() {
        let score = FixedPointScore(u64::MAX);
        assert_eq!(
            score.checked_add(FixedPointScore(1)),
            Err(RetrievalContractError::FixedPointOverflow { operation: "add" })
        );
        assert_eq!(
            score.checked_weight(2),
            Err(RetrievalContractError::FixedPointOverflow {
                operation: "weight"
            })
        );
        assert_eq!(
            FixedPointScore(2_000_000).checked_weight(500_000),
            Ok(1_000_000)
        );
    }

    #[test]
    fn score_calibration_handles_the_full_u64_domain_without_intermediate_overflow() {
        let calibration = ScoreDomainCalibrationV1 {
            calibration_profile_id: id("calibration.fixture.v1"),
            score_domain: id("score.fixture.v1"),
            raw_min_micros: 0,
            raw_max_micros: u64::MAX,
        };

        assert_eq!(
            calibration.calibrate(FixedPointScore(u64::MAX / 2)),
            Ok(499_999)
        );
    }

    #[test]
    fn retriever_batch_rejects_missing_or_extra_evidence() {
        let candidate = candidate("occurrence.fixture", RetrieverKind::Lexical, 0);
        let mut batch: RetrieverBatch<OccurrenceProvenance> = RetrieverBatch {
            candidates: vec![candidate],
            evidence_by_occurrence: BTreeMap::new(),
            coverage: RetrieverCoverage::default(),
            continuation: None,
        };
        assert!(batch.validate().is_err());

        let candidate = &batch.candidates[0];
        let provenance = provenance(candidate);
        batch
            .evidence_by_occurrence
            .insert(candidate.source_occurrence_id.clone(), provenance.clone());
        batch
            .evidence_by_occurrence
            .insert(id("occurrence.extra"), provenance);
        assert!(batch.validate().is_err());
    }

    #[test]
    fn retriever_batch_rejects_duplicate_occurrences_and_mixed_lanes() {
        let first = candidate("occurrence.shared", RetrieverKind::Lexical, 0);
        let duplicate = candidate("occurrence.shared", RetrieverKind::Lexical, 1);
        let mut evidence = BTreeMap::new();
        evidence.insert(first.source_occurrence_id.clone(), provenance(&first));
        let duplicate_batch = RetrieverBatch {
            candidates: vec![first, duplicate],
            evidence_by_occurrence: evidence,
            coverage: RetrieverCoverage::default(),
            continuation: None,
        };
        assert!(duplicate_batch.validate().is_err());

        let lexical = candidate("occurrence.lexical", RetrieverKind::Lexical, 0);
        let graph = candidate("occurrence.graph", RetrieverKind::Graph, 1);
        let mut evidence = BTreeMap::new();
        evidence.insert(lexical.source_occurrence_id.clone(), provenance(&lexical));
        evidence.insert(graph.source_occurrence_id.clone(), provenance(&graph));
        let mixed_batch = RetrieverBatch {
            candidates: vec![lexical, graph],
            evidence_by_occurrence: evidence,
            coverage: RetrieverCoverage::default(),
            continuation: None,
        };
        assert!(mixed_batch.validate().is_err());
    }
}
