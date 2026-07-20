//! Pure, versioned search-quality evaluation fixture and evidence contracts
//! for TraceDecay V2.
//!
//! Owning plan:
//! [Plan 15](../../../../docs/plans/tracedecay-v2/15-search-quality-evaluation-and-retrieval-research.md)
//! ("Evaluation artifacts and fixtures", "Decision policy and terminal
//! outcomes"); delivered by the pr9/13-search-eval-fixtures packet under the
//! [PR9 contract spine](../../../../docs/plans/tracedecay-v2/pr9/00-contract-spine.md).
//!
//! This module contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async
//! work. It freezes the typed shapes of the query workload, the relevance
//! label set, the sealed-holdout locator, the run manifest, the candidate
//! lists, the emitted evidence batches, and the terminal decision record.
//!
//! Self-containment note (interim, pr9/13): this module deliberately depends
//! only on `std`, `serde`, `serde_json`, `sha2`, and `thiserror` — not on
//! `crate::research` or `crate::retrieval` — so the unregistered module can be
//! included by path from `tests/search_quality_suite/` until the crate-root
//! registration (`pub mod evaluation;`) lands with the coordinator's compose
//! step. Its canonical-digest helper therefore mirrors, but does not reuse,
//! `crate::research::canonical_sha256`; unifying the two is a follow-up
//! decision that must happen before any cross-module digest comparisons are
//! introduced.
//!
//! Determinism: no wall-clock timestamps and no randomness appear in these
//! types. Committed fixtures carry content digests; runtime artifacts
//! (evidence batches, run manifests, decision records) carry canonical
//! domain-separated digests computed from their typed payloads.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Schema/domain separator for [`QueryWorkloadV1`] digests.
pub const WORKLOAD_DIGEST_DOMAIN: &str = "tracedecay.eval-workload.v1";
/// Schema/domain separator for [`LabelSetV1`] digests.
pub const LABEL_SET_DIGEST_DOMAIN: &str = "tracedecay.eval-label-set.v1";
/// Schema/domain separator for [`EvidenceBatchV1`] digests.
pub const EVIDENCE_BATCH_DIGEST_DOMAIN: &str = "tracedecay.eval-evidence-batch.v1";
/// Schema/domain separator for [`RunManifestV1`] digests.
pub const RUN_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.eval-run-manifest.v1";
/// Schema/domain separator for [`DecisionRecordV1`] digests.
pub const DECISION_RECORD_DIGEST_DOMAIN: &str = "tracedecay.eval-decision-record.v1";
/// Schema/domain separator for [`EvidenceIndexV1`] digests.
pub const EVIDENCE_INDEX_DIGEST_DOMAIN: &str = "tracedecay.eval-evidence-index.v1";
/// Schema/domain separator for immutable pre-reveal saved candidate sets.
pub const SAVED_CANDIDATE_SET_DIGEST_DOMAIN: &str = "tracedecay.eval-saved-candidate-set.v1";

/// Validation failures for pure search-quality evaluation values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvaluationContractError {
    #[error("{field} is not a canonical identity")]
    InvalidIdentity { field: &'static str },
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} contains a duplicate identity")]
    Duplicate { field: &'static str },
    #[error("{field} digest does not match the canonical domain-separated payload")]
    DigestMismatch { field: &'static str },
    #[error("partition violation: {0}")]
    PartitionViolation(String),
    #[error("sealed holdout access violation: {0}")]
    HoldoutAccessViolation(String),
    #[error("evidence coverage violation: {0}")]
    CoverageViolation(String),
    #[error("canonical serialization failed: {0}")]
    CanonicalSerialization(String),
}

fn validate_evaluation_identity(
    value: &str,
    field: &'static str,
) -> Result<(), EvaluationContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(EvaluationContractError::InvalidIdentity { field });
    }
    Ok(())
}

macro_rules! evaluation_string_id {
    ($($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed evaluation identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, EvaluationContractError> {
                let value = value.into();
                validate_evaluation_identity(&value, stringify!($name))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), EvaluationContractError> {
                Self::new(self.0.clone()).map(|_| ())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = EvaluationContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

fn validate_evaluation_digest(
    value: &str,
    field: &'static str,
) -> Result<(), EvaluationContractError> {
    let valid = value
        .strip_prefix("sha256:")
        .map(|encoded| {
            encoded.len() == 64
                && encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .unwrap_or(false);
    if !valid {
        return Err(EvaluationContractError::InvalidIdentity { field });
    }
    Ok(())
}

macro_rules! evaluation_digest_id {
    ($($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed sha256 integrity digest: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, EvaluationContractError> {
                let value = value.into();
                validate_evaluation_digest(&value, stringify!($name))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), EvaluationContractError> {
                validate_evaluation_digest(&self.0, stringify!($name))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = EvaluationContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

evaluation_string_id!(
    EvalQueryId,
    CorpusDocumentId,
    LabelSetId,
    RunId,
    FixtureManifestId,
    ContaminationGroupId,
    DecisionOwnerId,
    EvidenceBatchId,
    MetricDefinitionId,
    RetrieverLaneId,
    SnapshotId,
    TemporalEventId,
    ContextSpanId,
    EvaluationTaskId,
    AuthorizationCanaryId,
    ExactAdmissionOracleId,
    EvidenceIndexId,
    EvidenceClaimId,
    JudgmentId,
    LogicalCopyGroupId,
    ContradictionGroupId,
);

evaluation_digest_id!(
    FixtureContentDigest,
    FixtureManifestDigest,
    WorkloadDigest,
    LabelSetDigest,
    HoldoutSealDigest,
    EvidenceBatchDigest,
    RunManifestDigest,
    DecisionRecordDigest,
    EvidenceIndexDigest,
    SavedCandidateSetDigest,
);

/// Compute the canonical sha256 digest of a typed digest-input payload.
///
/// Canonicalization is `serde_json::to_vec` over the digest-input struct:
/// struct fields serialize in declaration order and every map in these
/// contracts is a `BTreeMap`/`BTreeSet`, so the bytes are deterministic. The
/// returned string is algorithm-tagged (`sha256:<64 lowercase hex>`).
fn canonical_json_sha256<T: Serialize>(value: &T) -> Result<String, EvaluationContractError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| EvaluationContractError::CanonicalSerialization(error.to_string()))?;
    let digest = Sha256::digest(&bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .map_err(|error| EvaluationContractError::CanonicalSerialization(error.to_string()))?;
    }
    Ok(encoded)
}

/// Contamination partitions (Plan 15: development labels are tunable; the
/// locked holdout and forward-confirmation labels are sealed and never
/// readable during tuning).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvalPartitionV1 {
    Development,
    SealedHoldout,
    ForwardConfirmation,
}

impl EvalPartitionV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::SealedHoldout => "sealed_holdout",
            Self::ForwardConfirmation => "forward_confirmation",
        }
    }

    /// Labels for this partition may be checked in and read during tuning.
    pub const fn labels_are_tunable(self) -> bool {
        matches!(self, Self::Development)
    }
}

/// Query families frozen by the fixture corpus (Plan 15: exact errors,
/// symbols, flags, paths, IDs, false-exact hard negatives, paraphrases,
/// typos, multi-hop graph questions, wrong-scope near matches, authorization
/// canaries, hard negatives, contradictions, copies/echoes, and expected
/// no-result cases).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QueryFamilyV1 {
    ExactError,
    ExactSymbol,
    ExactPath,
    ExactIdentifier,
    ExactCliFlag,
    ExactConfigKey,
    FalseExactHardNegative,
    Paraphrase,
    Typo,
    MultiHopGraph,
    TemporalAsOf,
    WrongScopeNearMatch,
    AuthorizationCanary,
    HardNegative,
    Contradiction,
    CopyOrEcho,
    ExpectedNoResult,
}

/// Graded relevance of one judged document for one query.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceGradeV1 {
    HighlyRelevant,
    Relevant,
    Marginal,
    NotRelevant,
}

impl RelevanceGradeV1 {
    /// Whether the grade counts as relevant for recall-style evidence joins.
    pub const fn is_relevant(self) -> bool {
        matches!(self, Self::HighlyRelevant | Self::Relevant)
    }
}

/// Evidence role of a judged document (mirrors the retrieval kernel's
/// `EvidenceRole`; kept self-contained per the module note).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LabelEvidenceRoleV1 {
    Primary,
    Corroboration,
    Contradiction,
    Context,
}

/// Metric direction frozen in the fixture manifest (Plan 15: every metric
/// direction is frozen before candidate tuning).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirectionV1 {
    HigherIsBetter,
    LowerIsBetter,
}

/// The only holdout access policy at this revision: the sealed label bytes
/// may be revealed only by an audited locked run that records a
/// [`HoldoutAccessReceiptV1`]. Development runs never open the locator.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutAccessPolicyV1 {
    SealedRevealRequiresReceipt,
}

/// Scope of one evaluation run (Plan 15: the harness signs and freezes the
/// run manifest before the locked-label access capability is granted).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvalRunScopeV1 {
    Development,
    Locked,
}

/// Whether an artifact is contract-only or may participate in a locked
/// quality decision. PR9 fixture construction uses `ContractOnly`; only a
/// separately frozen locked run may carry `LockedQuality`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FixtureAuthorityV1 {
    ContractOnly,
    LockedQuality,
}

/// Terminal outcomes of an evaluation run (Plan 15, "Decision policy and
/// terminal outcomes"). Only `Accepted` may create a promotion record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvalOutcomeV1 {
    InvalidRun,
    Blocked,
    Rejected,
    Inconclusive,
    RuntimeFallbackObserved,
    Accepted,
}

impl EvalOutcomeV1 {
    /// Exact stable wire/terminal spelling required by Plan 15.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRun => "invalid_run",
            Self::Blocked => "blocked",
            Self::Rejected => "rejected",
            Self::Inconclusive => "inconclusive",
            Self::RuntimeFallbackObserved => "runtime_fallback_observed",
            Self::Accepted => "accepted",
        }
    }

    /// Only an `accepted` run is promotion authority (Plan 15).
    pub const fn permits_promotion(self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// `invalid_run` permits no quality conclusion (Plan 15).
    pub const fn permits_quality_conclusion(self) -> bool {
        !matches!(self, Self::InvalidRun | Self::Blocked)
    }
}

impl FromStr for EvalOutcomeV1 {
    type Err = EvaluationContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "invalid_run" => Ok(Self::InvalidRun),
            "blocked" => Ok(Self::Blocked),
            "rejected" => Ok(Self::Rejected),
            "inconclusive" => Ok(Self::Inconclusive),
            "runtime_fallback_observed" => Ok(Self::RuntimeFallbackObserved),
            "accepted" => Ok(Self::Accepted),
            _ => Err(EvaluationContractError::InvalidIdentity {
                field: "evaluation outcome",
            }),
        }
    }
}

/// One source-local immutable generation pinned by an evaluation snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotGenerationV1 {
    pub source: String,
    pub generation: String,
}

/// One source/projection watermark pinned by an evaluation snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotWatermarkV1 {
    pub source: String,
    pub watermark: String,
}

/// Plan 15 `snapshots-v1.jsonl` row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvalSnapshotV1 {
    pub snapshot_id: SnapshotId,
    pub repository_commit: String,
    pub branch_identity: String,
    pub worktree_identity: String,
    pub canonical_store_generations: Vec<SnapshotGenerationV1>,
    pub source_watermarks: Vec<SnapshotWatermarkV1>,
    pub projection_watermarks: Vec<SnapshotWatermarkV1>,
    pub authorization_policy_revision: String,
    pub wall_clock_cutoff_unix_micros: i64,
}

impl EvalSnapshotV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        validate_git_commit(&self.repository_commit, "snapshot repository commit")?;
        for (field, value) in [
            ("snapshot branch identity", &self.branch_identity),
            ("snapshot worktree identity", &self.worktree_identity),
            (
                "snapshot authorization policy revision",
                &self.authorization_policy_revision,
            ),
        ] {
            if value.is_empty() {
                return Err(EvaluationContractError::Empty { field });
            }
        }
        validate_named_values(
            &self.canonical_store_generations,
            |entry| (&entry.source, &entry.generation),
            "snapshot canonical-store generations",
        )?;
        validate_named_values(
            &self.source_watermarks,
            |entry| (&entry.source, &entry.watermark),
            "snapshot source watermarks",
        )?;
        validate_named_values(
            &self.projection_watermarks,
            |entry| (&entry.source, &entry.watermark),
            "snapshot projection watermarks",
        )
    }
}

fn validate_git_commit(value: &str, field: &'static str) -> Result<(), EvaluationContractError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(EvaluationContractError::InvalidIdentity { field });
    }
    Ok(())
}

fn validate_named_values<T, F>(
    values: &[T],
    mut fields: F,
    field: &'static str,
) -> Result<(), EvaluationContractError>
where
    F: FnMut(&T) -> (&str, &str),
{
    if values.is_empty() {
        return Err(EvaluationContractError::Empty { field });
    }
    let mut names = BTreeSet::new();
    for value in values {
        let (name, value) = fields(value);
        if name.is_empty() || value.is_empty() {
            return Err(EvaluationContractError::Empty { field });
        }
        if !names.insert(name) {
            return Err(EvaluationContractError::Duplicate { field });
        }
    }
    Ok(())
}

/// Eligibility oracle for one temporal event at one frozen snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotEligibilityV1 {
    pub snapshot_id: SnapshotId,
    pub eligible: bool,
}

/// Plan 15 `temporal-events-v1.jsonl` row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalEventV1 {
    pub event_id: TemporalEventId,
    pub document_id: CorpusDocumentId,
    pub valid_from_unix_micros: i64,
    #[serde(default)]
    pub valid_until_unix_micros: Option<i64>,
    pub observed_at_unix_micros: i64,
    pub ingested_at_unix_micros: i64,
    pub arrival_sequence: u64,
    pub source_generation: String,
    pub source_watermark: String,
    pub projection_watermark: String,
    #[serde(default)]
    pub supersedes_event_id: Option<TemporalEventId>,
    pub expected_eligibility: Vec<SnapshotEligibilityV1>,
}

impl TemporalEventV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self
            .valid_until_unix_micros
            .is_some_and(|until| until < self.valid_from_unix_micros)
        {
            return Err(EvaluationContractError::PartitionViolation(
                "temporal event validity interval is reversed".to_string(),
            ));
        }
        if self.ingested_at_unix_micros < self.observed_at_unix_micros {
            return Err(EvaluationContractError::PartitionViolation(
                "temporal event ingestion precedes observation".to_string(),
            ));
        }
        if self.source_generation.is_empty()
            || self.source_watermark.is_empty()
            || self.projection_watermark.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "temporal event generation/watermark",
            });
        }
        if self.expected_eligibility.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "temporal event snapshot eligibility",
            });
        }
        let mut snapshots = BTreeSet::new();
        for eligibility in &self.expected_eligibility {
            if !snapshots.insert(&eligibility.snapshot_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "temporal event snapshot eligibility",
                });
            }
        }
        Ok(())
    }
}

/// Classification of a judged context span.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContextSpanDispositionV1 {
    Relevant,
    Stale,
    Forbidden,
}

/// Plan 15 `context-spans-v1.jsonl` row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextSpanV1 {
    pub context_span_id: ContextSpanId,
    pub query_id: EvalQueryId,
    pub document_id: CorpusDocumentId,
    pub payload_revision: String,
    pub tokenizer_revision: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub token_start: u64,
    pub token_end: u64,
    pub disposition: ContextSpanDispositionV1,
    pub citation_support: bool,
    #[serde(default)]
    pub contradiction_group: Option<ContradictionGroupId>,
}

impl ContextSpanV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.payload_revision.is_empty() || self.tokenizer_revision.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "context span revision",
            });
        }
        if self.byte_start >= self.byte_end || self.token_start >= self.token_end {
            return Err(EvaluationContractError::CoverageViolation(
                "context span ranges must be non-empty and ordered".to_string(),
            ));
        }
        Ok(())
    }
}

/// Frozen resource budget for one task attempt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationTaskBudgetV1 {
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
}

/// Plan 15 `tasks-v1.jsonl` row. Exactly one prompt source and at least one
/// verifier source are required.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationTaskV1 {
    pub task_id: EvaluationTaskId,
    pub snapshot_id: SnapshotId,
    pub initial_repository_commit: String,
    #[serde(default)]
    pub sanitized_prompt: Option<String>,
    #[serde(default)]
    pub authorized_prompt_locator_digest: Option<FixtureContentDigest>,
    pub prompt_content_digest: FixtureContentDigest,
    #[serde(default)]
    pub deterministic_verifier: Option<String>,
    #[serde(default)]
    pub blinded_rubric: Option<String>,
    pub agent_revision: String,
    pub model_revision: String,
    pub tool_revision: String,
    pub decoding_parameters: String,
    pub attempt_seeds: Vec<u64>,
    pub budget: EvaluationTaskBudgetV1,
    pub timeout_millis: u64,
    pub workspace_reset_procedure: String,
    pub blinded_assignment: String,
}

impl EvaluationTaskV1 {
    /// Compute the tagged sha256 of the sanitized prompt bytes. Returns
    /// `None` for authorized-locator prompts, whose bytes are verified by the
    /// authorized store at reveal time, never in Git.
    pub fn compute_prompt_digest(
        &self,
    ) -> Result<Option<FixtureContentDigest>, EvaluationContractError> {
        match (
            &self.sanitized_prompt,
            &self.authorized_prompt_locator_digest,
        ) {
            (Some(prompt), None) => {
                let digest = Sha256::digest(prompt.as_bytes());
                let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
                encoded.push_str("sha256:");
                for byte in digest {
                    use std::fmt::Write as _;
                    write!(&mut encoded, "{byte:02x}").map_err(|error| {
                        EvaluationContractError::CanonicalSerialization(error.to_string())
                    })?;
                }
                Ok(Some(FixtureContentDigest::new(encoded)?))
            }
            (None, Some(_)) => Ok(None),
            _ => Err(EvaluationContractError::CoverageViolation(
                "task must declare exactly one prompt source".to_string(),
            )),
        }
    }

    /// Verify the frozen prompt content digest against the sanitized prompt
    /// bytes. Locator-based prompts have no locally verifiable bytes and pass
    /// here; the authorized store verifies them at reveal time.
    pub fn verify_prompt_digest(&self) -> Result<(), EvaluationContractError> {
        match self.compute_prompt_digest()? {
            Some(computed) if computed != self.prompt_content_digest => {
                Err(EvaluationContractError::DigestMismatch {
                    field: "task prompt content",
                })
            }
            _ => Ok(()),
        }
    }

    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        validate_git_commit(
            &self.initial_repository_commit,
            "task initial repository commit",
        )?;
        if self.sanitized_prompt.is_some() == self.authorized_prompt_locator_digest.is_some() {
            return Err(EvaluationContractError::CoverageViolation(
                "task must declare exactly one sanitized or authorized prompt source".to_string(),
            ));
        }
        if self
            .sanitized_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.is_empty())
            || (self.deterministic_verifier.is_none() && self.blinded_rubric.is_none())
        {
            return Err(EvaluationContractError::Empty {
                field: "task prompt/verifier",
            });
        }
        if self.agent_revision.is_empty()
            || self.model_revision.is_empty()
            || self.tool_revision.is_empty()
            || self.decoding_parameters.is_empty()
            || self.attempt_seeds.is_empty()
            || self.timeout_millis == 0
            || self.workspace_reset_procedure.is_empty()
            || self.blinded_assignment.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "task frozen execution settings",
            });
        }
        let mut seeds = BTreeSet::new();
        if self.attempt_seeds.iter().any(|seed| !seeds.insert(seed)) {
            return Err(EvaluationContractError::Duplicate {
                field: "task attempt seeds",
            });
        }
        Ok(())
    }
}

/// One explicit denied-anchor authorization canary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationCanaryV1 {
    pub canary_id: AuthorizationCanaryId,
    pub query_id: EvalQueryId,
    pub denied_document_id: CorpusDocumentId,
    #[serde(default)]
    pub denied_symbol: Option<String>,
    pub denied_scope_id: String,
    pub control_snapshot_id: SnapshotId,
    pub injected_snapshot_id: SnapshotId,
}

impl AuthorizationCanaryV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.denied_scope_id.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "authorization canary denied scope",
            });
        }
        Ok(())
    }
}

/// Exact-admission oracle outcome. It protects eligible exact anchors and
/// explicitly marks tempting false-exact candidates as ineligible.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExactAdmissionExpectationV1 {
    Eligible,
    Ineligible,
    NoExactCandidate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactAdmissionOracleV1 {
    pub oracle_id: ExactAdmissionOracleId,
    pub query_id: EvalQueryId,
    pub exact_text: String,
    pub expectation: ExactAdmissionExpectationV1,
    #[serde(default)]
    pub document_id: Option<CorpusDocumentId>,
    #[serde(default)]
    pub symbol: Option<String>,
    pub protected_from_approximate_demotion: bool,
    pub rationale: String,
}

impl ExactAdmissionOracleV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.exact_text.is_empty() || self.rationale.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "exact-admission oracle",
            });
        }
        if self.expectation == ExactAdmissionExpectationV1::NoExactCandidate
            && self.document_id.is_some()
        {
            return Err(EvaluationContractError::CoverageViolation(
                "no-exact-candidate oracle cannot name a document".to_string(),
            ));
        }
        Ok(())
    }
}

/// One contamination group belongs to exactly one partition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContaminationGroupV1 {
    pub group_id: ContaminationGroupId,
    pub partition: EvalPartitionV1,
    pub query_ids: Vec<EvalQueryId>,
    pub repository_family_clusters: Vec<String>,
    pub rationale: String,
}

/// Explicit contamination partition artifact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContaminationPartitionManifestV1 {
    pub revision: u32,
    pub groups: Vec<ContaminationGroupV1>,
}

impl ContaminationPartitionManifestV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.groups.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "contamination groups",
            });
        }
        let mut group_ids = BTreeSet::new();
        let mut query_ids = BTreeSet::new();
        for group in &self.groups {
            if !group_ids.insert(&group.group_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "contamination group id",
                });
            }
            if group.query_ids.is_empty()
                || group.repository_family_clusters.is_empty()
                || group.rationale.is_empty()
            {
                return Err(EvaluationContractError::Empty {
                    field: "contamination group members",
                });
            }
            for query_id in &group.query_ids {
                if !query_ids.insert(query_id) {
                    return Err(EvaluationContractError::PartitionViolation(format!(
                        "query {query_id} appears in more than one contamination group"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// One sanitized corpus document: a verbatim source snapshot drawn from the
/// tracedecay repository, frozen by content digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusDocumentV1 {
    pub document_id: CorpusDocumentId,
    /// Repo-relative path the snapshot was drawn from (provenance only).
    pub source_repository_path: String,
    /// Fixture-relative path of the committed snapshot bytes.
    pub snapshot_path: String,
    pub language: String,
    pub scope_ids: Vec<String>,
    pub privacy_domain_class: String,
    pub byte_len: u64,
    pub content_digest: FixtureContentDigest,
}

impl CorpusDocumentV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.source_repository_path.is_empty()
            || self.snapshot_path.is_empty()
            || self.language.is_empty()
            || self.scope_ids.is_empty()
            || self.privacy_domain_class.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "corpus document path/classification",
            });
        }
        let mut scopes = BTreeSet::new();
        if self
            .scope_ids
            .iter()
            .any(|scope| scope.is_empty() || !scopes.insert(scope))
        {
            return Err(EvaluationContractError::Duplicate {
                field: "corpus document scope",
            });
        }
        if self.byte_len == 0 {
            return Err(EvaluationContractError::Empty {
                field: "corpus document byte_len",
            });
        }
        Ok(())
    }
}

/// Digest of one committed fixture artifact file (workload, development
/// labels). The sealed holdout file is never listed here; only its
/// [`HoldoutSealV1`] digest is committed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FixtureFileDigestV1 {
    pub path: String,
    pub byte_len: u64,
    pub digest: FixtureContentDigest,
}

/// One partition's frozen query count.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartitionSpecV1 {
    pub partition: EvalPartitionV1,
    pub query_count: u32,
}

/// One frozen metric definition (Plan 15: name, direction, and unit are
/// frozen before tuning; no numeric cutoffs are invented here).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetricDefinitionV1 {
    pub metric_id: MetricDefinitionId,
    pub name: String,
    pub direction: MetricDirectionV1,
    pub unit: String,
}

/// One frozen support floor: a stratum with fewer than `min_queries`
/// queries is `inconclusive` and is never pooled away (Plan 15).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SupportFloorV1 {
    pub stratum: String,
    pub min_queries: u32,
}

/// The sealed holdout locator (Plan 15 `locked-judgments-v1.json`): only the
/// seal digest, the authorized-store locator, the access policy, and the
/// reveal audit contract are committed. Locked labels are not checked in and
/// are never read during tuning; a locked run proves non-access by verifying
/// this seal over the sealed bytes without parsing them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HoldoutSealV1 {
    /// Opaque locator resolved only by the authorized holdout store.
    pub locator: String,
    pub seal_digest: HoldoutSealDigest,
    pub signed_envelope_digest: FixtureContentDigest,
    pub signature_locator: String,
    pub access_policy: HoldoutAccessPolicyV1,
    /// Human-readable reveal audit contract: who may reveal, what receipt is
    /// recorded, and what invalidates the run on pre-freeze access.
    pub reveal_contract: String,
    pub schema_revision: u32,
}

impl HoldoutSealV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if !self.locator.starts_with("authorized-store://") {
            return Err(EvaluationContractError::InvalidIdentity {
                field: "holdout seal locator",
            });
        }
        if !self.signature_locator.starts_with("authorized-store://")
            || self.reveal_contract.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "holdout seal signature/reveal contract",
            });
        }
        Ok(())
    }
}

/// The frozen fixture manifest (Plan 15 `fixture-manifest-v1.json`): corpus
/// and artifact digests, partitions, contamination groups, metric
/// definitions, support floors, and decision owners, frozen before tuning.
/// The manifest carries no self-digest; run manifests pin the manifest by
/// the sha256 of its committed file bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FixtureManifestV1 {
    pub manifest_id: FixtureManifestId,
    pub revision: u32,
    pub authority: FixtureAuthorityV1,
    /// Free-form baseline provenance (e.g. the repository revision the
    /// corpus snapshots were drawn from).
    pub baseline_revision: String,
    pub checkpoint_commit: String,
    pub corpus: Vec<CorpusDocumentV1>,
    pub artifact_files: Vec<FixtureFileDigestV1>,
    pub partitions: Vec<PartitionSpecV1>,
    pub contamination_groups: Vec<ContaminationGroupId>,
    pub metric_definitions: Vec<MetricDefinitionV1>,
    pub support_floors: Vec<SupportFloorV1>,
    pub deterministic_seeds: Vec<u64>,
    pub exact_admission_rules: Vec<String>,
    pub adjudication_policy: String,
    pub stopping_rules: Vec<String>,
    pub decision_owners: Vec<DecisionOwnerId>,
    pub holdout_seal: HoldoutSealV1,
}

impl FixtureManifestV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        validate_git_commit(&self.checkpoint_commit, "manifest checkpoint commit")?;
        if self.baseline_revision.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "manifest baseline revision",
            });
        }
        if self.corpus.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "manifest corpus",
            });
        }
        let mut documents = BTreeSet::new();
        for document in &self.corpus {
            document.validate()?;
            if !documents.insert(&document.document_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "manifest corpus document_id",
                });
            }
        }
        if self.artifact_files.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "manifest artifact files",
            });
        }
        let mut artifact_paths = BTreeSet::new();
        for artifact in &self.artifact_files {
            if artifact.path.is_empty() || artifact.byte_len == 0 {
                return Err(EvaluationContractError::Empty {
                    field: "manifest artifact path/length",
                });
            }
            if artifact.path.starts_with('/')
                || artifact.path.split('/').any(|component| component == "..")
            {
                return Err(EvaluationContractError::InvalidIdentity {
                    field: "manifest artifact path",
                });
            }
            if !artifact_paths.insert(&artifact.path) {
                return Err(EvaluationContractError::Duplicate {
                    field: "manifest artifact path",
                });
            }
        }
        let mut groups = BTreeSet::new();
        for group in &self.contamination_groups {
            if !groups.insert(group) {
                return Err(EvaluationContractError::Duplicate {
                    field: "manifest contamination group",
                });
            }
        }
        let mut partition_ids = BTreeSet::new();
        for partition in &self.partitions {
            if partition.query_count == 0 {
                return Err(EvaluationContractError::Empty {
                    field: "manifest partition query count",
                });
            }
            if !partition_ids.insert(partition.partition) {
                return Err(EvaluationContractError::Duplicate {
                    field: "manifest partition",
                });
            }
        }
        let development_partitions = self
            .partitions
            .iter()
            .filter(|spec| spec.partition == EvalPartitionV1::Development)
            .count();
        if development_partitions != 1 {
            return Err(EvaluationContractError::PartitionViolation(
                "the manifest must declare exactly one development partition".to_string(),
            ));
        }
        if self.decision_owners.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "manifest decision owners",
            });
        }
        if self.deterministic_seeds.is_empty()
            || self.exact_admission_rules.is_empty()
            || self.adjudication_policy.is_empty()
            || self.stopping_rules.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "manifest frozen evaluation policy",
            });
        }
        let mut seeds = BTreeSet::new();
        if self
            .deterministic_seeds
            .iter()
            .any(|seed| !seeds.insert(seed))
        {
            return Err(EvaluationContractError::Duplicate {
                field: "manifest deterministic seed",
            });
        }
        self.holdout_seal.validate()?;
        Ok(())
    }

    /// Look up a corpus document by identity.
    pub fn document(&self, document_id: &CorpusDocumentId) -> Option<&CorpusDocumentV1> {
        self.corpus
            .iter()
            .find(|doc| &doc.document_id == document_id)
    }
}

/// One workload query (Plan 15 `queries-v1.jsonl`). Query text is sanitized;
/// labels live in the partition's label set, never here.
/// `forbidden_document_ids` declares authorization canaries: documents that
/// must never surface for this query in an authorized world.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvalQueryV1 {
    pub query_id: EvalQueryId,
    pub partition: EvalPartitionV1,
    pub family: QueryFamilyV1,
    pub provider: String,
    pub language: String,
    pub repository_family_cluster: String,
    pub snapshot_id: SnapshotId,
    pub snapshot_commit: String,
    pub as_of_unix_micros: i64,
    pub principal_class: String,
    pub privacy_domain_class: String,
    pub allowed_scope_ids: Vec<String>,
    pub query_text: String,
    #[serde(default)]
    pub authorized_private_query_locator_digest: Option<FixtureContentDigest>,
    #[serde(default)]
    pub contamination_groups: Vec<ContaminationGroupId>,
    #[serde(default)]
    pub forbidden_document_ids: Vec<CorpusDocumentId>,
}

impl EvalQueryV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        validate_git_commit(&self.snapshot_commit, "query snapshot commit")?;
        if self.query_text.is_empty() == self.authorized_private_query_locator_digest.is_none() {
            return Err(EvaluationContractError::CoverageViolation(
                "query must carry exactly one sanitized text or authorized private locator digest"
                    .to_string(),
            ));
        }
        if self.provider.is_empty()
            || self.language.is_empty()
            || self.repository_family_cluster.is_empty()
            || self.principal_class.is_empty()
            || self.privacy_domain_class.is_empty()
            || self.allowed_scope_ids.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "query classification",
            });
        }
        let mut scopes = BTreeSet::new();
        if self
            .allowed_scope_ids
            .iter()
            .any(|scope| scope.is_empty() || !scopes.insert(scope))
        {
            return Err(EvaluationContractError::Duplicate {
                field: "query allowed scope",
            });
        }
        Ok(())
    }
}

/// The frozen query workload (Plan 15 `queries-v1.jsonl`, parsed).
/// Canonically digested with [`WORKLOAD_DIGEST_DOMAIN`]; the digest field is
/// excluded from the hashed bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryWorkloadV1 {
    pub revision: u32,
    pub queries: Vec<EvalQueryV1>,
    pub digest: WorkloadDigest,
}

#[derive(Serialize)]
struct QueryWorkloadDigestInput<'a> {
    domain: &'static str,
    revision: u32,
    queries: &'a [EvalQueryV1],
}

impl QueryWorkloadV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.queries.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "workload queries",
            });
        }
        let mut ids = BTreeSet::new();
        for query in &self.queries {
            query.validate()?;
            if !ids.insert(&query.query_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "workload query_id",
                });
            }
        }
        if self.development_queries().next().is_none() {
            return Err(EvaluationContractError::PartitionViolation(
                "the workload must contain at least one development query".to_string(),
            ));
        }
        Ok(())
    }

    pub fn development_queries(&self) -> impl Iterator<Item = &EvalQueryV1> {
        self.queries
            .iter()
            .filter(|query| query.partition == EvalPartitionV1::Development)
    }

    pub fn sealed_holdout_queries(&self) -> impl Iterator<Item = &EvalQueryV1> {
        self.queries
            .iter()
            .filter(|query| query.partition == EvalPartitionV1::SealedHoldout)
    }

    pub fn query(&self, query_id: &EvalQueryId) -> Option<&EvalQueryV1> {
        self.queries
            .iter()
            .find(|query| &query.query_id == query_id)
    }

    pub fn compute_digest(&self) -> Result<WorkloadDigest, EvaluationContractError> {
        let input = QueryWorkloadDigestInput {
            domain: WORKLOAD_DIGEST_DOMAIN,
            revision: self.revision,
            queries: &self.queries,
        };
        WorkloadDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "query workload",
            })
        }
    }
}

/// One relevance judgment (Plan 15 `judgments-development-v1.jsonl` row).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelevanceJudgmentV1 {
    pub judgment_id: JudgmentId,
    pub query_id: EvalQueryId,
    pub document_id: CorpusDocumentId,
    #[serde(default)]
    pub symbol: Option<String>,
    pub grade: RelevanceGradeV1,
    pub evidence_role: LabelEvidenceRoleV1,
    pub valid_from_unix_micros: i64,
    #[serde(default)]
    pub valid_until_unix_micros: Option<i64>,
    #[serde(default)]
    pub supersedes_judgment_id: Option<JudgmentId>,
    #[serde(default)]
    pub logical_copy_group: Option<LogicalCopyGroupId>,
    #[serde(default)]
    pub forbidden_anchor_ids: Vec<CorpusDocumentId>,
    pub abstention_oracle: bool,
    #[serde(default)]
    pub task_oracle: Option<EvaluationTaskId>,
    pub labeler: String,
    pub labeler_provenance: String,
    pub adjudication: String,
    pub correction_revision: u32,
    #[serde(default)]
    pub note: Option<String>,
}

/// The development label set. Sealed holdout labels must never flow through
/// this typed set (Plan 15: locked labels are not checked in or readable
/// during tuning), so [`LabelSetV1::validate`] rejects any partition whose
/// labels are not tunable. Canonically digested with
/// [`LABEL_SET_DIGEST_DOMAIN`]; the digest field is excluded from the hashed
/// bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabelSetV1 {
    pub label_set_id: LabelSetId,
    pub revision: u32,
    pub partition: EvalPartitionV1,
    pub judgments: Vec<RelevanceJudgmentV1>,
    pub digest: LabelSetDigest,
}

#[derive(Serialize)]
struct LabelSetDigestInput<'a> {
    domain: &'static str,
    label_set_id: &'a LabelSetId,
    revision: u32,
    partition: EvalPartitionV1,
    judgments: &'a [RelevanceJudgmentV1],
}

impl LabelSetV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if !self.partition.labels_are_tunable() {
            return Err(EvaluationContractError::PartitionViolation(format!(
                "labels for partition {} are sealed and must not enter the typed development label set",
                self.partition.as_str()
            )));
        }
        if self.judgments.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "label set judgments",
            });
        }
        let mut pairs = BTreeSet::new();
        let mut judgment_ids = BTreeSet::new();
        for judgment in &self.judgments {
            if judgment.labeler.is_empty()
                || judgment.labeler_provenance.is_empty()
                || judgment.adjudication.is_empty()
            {
                return Err(EvaluationContractError::Empty {
                    field: "judgment provenance/adjudication",
                });
            }
            if judgment
                .valid_until_unix_micros
                .is_some_and(|until| until < judgment.valid_from_unix_micros)
            {
                return Err(EvaluationContractError::CoverageViolation(
                    "judgment validity interval is reversed".to_string(),
                ));
            }
            if !judgment_ids.insert(&judgment.judgment_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "judgment id",
                });
            }
            if !pairs.insert((&judgment.query_id, &judgment.document_id)) {
                return Err(EvaluationContractError::Duplicate {
                    field: "judgment (query_id, document_id)",
                });
            }
        }
        Ok(())
    }

    pub fn judgments_for(
        &self,
        query_id: &EvalQueryId,
    ) -> impl Iterator<Item = &RelevanceJudgmentV1> {
        self.judgments
            .iter()
            .filter(move |judgment| &judgment.query_id == query_id)
    }

    pub fn compute_digest(&self) -> Result<LabelSetDigest, EvaluationContractError> {
        let input = LabelSetDigestInput {
            domain: LABEL_SET_DIGEST_DOMAIN,
            label_set_id: &self.label_set_id,
            revision: self.revision,
            partition: self.partition,
            judgments: &self.judgments,
        };
        LabelSetDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch { field: "label set" })
        }
    }
}

/// Audited receipt recorded when a locked run reveals the sealed holdout
/// labels. Its presence in an [`EvidenceBatchV1`] is the only lawful evidence
/// that holdout bytes were opened (Plan 15: the reveal produces an audited
/// access receipt bound to the run; any unrecorded reveal invalidates the
/// run).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HoldoutAccessReceiptV1 {
    pub run_id: RunId,
    pub run_manifest_digest: RunManifestDigest,
    pub seal_digest: HoldoutSealDigest,
    pub revealed_by: DecisionOwnerId,
    pub rationale: String,
}

/// A capability supplied by the authorized holdout store after the locked run
/// manifest is frozen. Merely parsing fixture metadata never constructs this
/// value; the I/O layer may resolve its paths only after
/// [`RunManifestV1::validate_pre_reveal`] succeeds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HoldoutRevealCapabilityV1 {
    pub schema_revision: u32,
    pub locator: String,
    pub signature_locator: String,
    pub seal_digest: HoldoutSealDigest,
    pub run_id: RunId,
    pub run_manifest_digest: RunManifestDigest,
    pub revealed_by: DecisionOwnerId,
    pub sealed_labels_path: String,
    pub signed_envelope_path: String,
}

impl HoldoutRevealCapabilityV1 {
    pub fn issue_receipt(
        &self,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
    ) -> Result<HoldoutAccessReceiptV1, EvaluationContractError> {
        run.validate()?;
        run.verify_digest()?;
        if run.scope != EvalRunScopeV1::Locked
            || run.authority != FixtureAuthorityV1::LockedQuality
            || run.locked_outcomes_accessed
        {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "a reveal capability requires a frozen locked-quality run".to_string(),
            ));
        }
        if self.schema_revision == 0
            || self.sealed_labels_path.is_empty()
            || self.signed_envelope_path.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "holdout reveal capability",
            });
        }
        if self.run_id != run.run_id || self.run_manifest_digest != run.digest {
            return Err(EvaluationContractError::DigestMismatch {
                field: "holdout capability run manifest digest",
            });
        }
        if self.locator != seal.locator
            || self.signature_locator != seal.signature_locator
            || self.seal_digest != seal.seal_digest
        {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "reveal capability does not bind the committed holdout locator and seal"
                    .to_string(),
            ));
        }
        if !decision_owners.contains(&self.revealed_by) {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "reveal capability owner is not a frozen decision owner".to_string(),
            ));
        }
        Ok(HoldoutAccessReceiptV1 {
            run_id: run.run_id.clone(),
            run_manifest_digest: run.digest.clone(),
            seal_digest: seal.seal_digest.clone(),
            revealed_by: self.revealed_by.clone(),
            rationale: "sealed holdout revealed after frozen run-manifest verification".to_string(),
        })
    }
}

/// A candidate anchor at corpus-document granularity, with an optional
/// symbol pinpoint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EvalCandidateAnchorV1 {
    pub document_id: CorpusDocumentId,
    #[serde(default)]
    pub symbol: Option<String>,
}

/// One ranked candidate emitted by one retrieval lane for one query.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvalCandidateV1 {
    pub anchor: EvalCandidateAnchorV1,
    pub ordinal_rank: u32,
}

/// One lane's ranked candidate list for one query. The lane identity is a
/// typed free-form id (e.g. `tracedecay_search`, `tracedecay_grep`) so the
/// harness can record the existing tool lanes today and the PR9 federated
/// lanes later without re-versioning the schema.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateListV1 {
    pub query_id: EvalQueryId,
    pub lane: RetrieverLaneId,
    pub candidates: Vec<EvalCandidateV1>,
}

impl CandidateListV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        let mut ranks = BTreeSet::new();
        let mut anchors = BTreeSet::new();
        for candidate in &self.candidates {
            if !ranks.insert(candidate.ordinal_rank) {
                return Err(EvaluationContractError::Duplicate {
                    field: "candidate ordinal rank",
                });
            }
            if !anchors.insert(&candidate.anchor) {
                return Err(EvaluationContractError::Duplicate {
                    field: "candidate anchor",
                });
            }
        }
        Ok(())
    }

    /// Anchors that appear in the query's forbidden canary set.
    pub fn forbidden_hits<'a>(
        &'a self,
        forbidden: &'a [CorpusDocumentId],
    ) -> impl Iterator<Item = &'a EvalCandidateAnchorV1> {
        self.candidates.iter().filter_map(move |candidate| {
            forbidden
                .contains(&candidate.anchor.document_id)
                .then_some(&candidate.anchor)
        })
    }
}

/// Candidate lists saved before holdout reveal. Ablations filter these bytes;
/// they never rerun candidate generation after labels become visible.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedCandidateSetV1 {
    pub schema_revision: u32,
    pub run_id: RunId,
    pub run_manifest_digest: RunManifestDigest,
    pub scope: EvalRunScopeV1,
    pub workload_digest: WorkloadDigest,
    pub candidate_lists: Vec<CandidateListV1>,
    pub digest: SavedCandidateSetDigest,
}

#[derive(Serialize)]
struct SavedCandidateSetDigestInput<'a> {
    domain: &'static str,
    schema_revision: u32,
    run_id: &'a RunId,
    run_manifest_digest: &'a RunManifestDigest,
    scope: EvalRunScopeV1,
    workload_digest: &'a WorkloadDigest,
    candidate_lists: &'a [CandidateListV1],
}

impl SavedCandidateSetV1 {
    pub fn validate_for_run(
        &self,
        run: &RunManifestV1,
        workload: &QueryWorkloadV1,
    ) -> Result<(), EvaluationContractError> {
        if self.schema_revision == 0 || self.candidate_lists.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "saved candidate set",
            });
        }
        run.validate_against_workload(workload)?;
        run.verify_digest()?;
        if self.run_id != run.run_id
            || self.run_manifest_digest != run.digest
            || self.scope != run.scope
            || self.workload_digest != workload.compute_digest()?
        {
            return Err(EvaluationContractError::DigestMismatch {
                field: "saved candidate run/workload binding",
            });
        }
        self.verify_digest()?;

        let expected_partition = match self.scope {
            EvalRunScopeV1::Development => EvalPartitionV1::Development,
            EvalRunScopeV1::Locked => EvalPartitionV1::SealedHoldout,
        };
        let execution_order: BTreeSet<_> = run.execution_order.iter().collect();
        let mut covered = BTreeSet::new();
        let mut query_lanes = BTreeSet::new();
        for list in &self.candidate_lists {
            list.validate()?;
            let query = workload.query(&list.query_id).ok_or_else(|| {
                EvaluationContractError::CoverageViolation(format!(
                    "saved candidates reference unknown query {}",
                    list.query_id
                ))
            })?;
            if query.partition != expected_partition || !execution_order.contains(&list.query_id) {
                return Err(EvaluationContractError::PartitionViolation(format!(
                    "saved candidates for query {} are outside the frozen run partition",
                    list.query_id
                )));
            }
            if !query_lanes.insert((&list.query_id, &list.lane)) {
                return Err(EvaluationContractError::Duplicate {
                    field: "saved candidate query/lane",
                });
            }
            covered.insert(&list.query_id);
        }
        if covered != execution_order {
            return Err(EvaluationContractError::CoverageViolation(
                "saved candidates must cover every query in the frozen execution order".to_string(),
            ));
        }
        Ok(())
    }

    pub fn ablate_lanes(
        &self,
        disabled_lanes: &[RetrieverLaneId],
    ) -> Result<Vec<CandidateListV1>, EvaluationContractError> {
        let available: BTreeSet<_> = self.candidate_lists.iter().map(|list| &list.lane).collect();
        if disabled_lanes.iter().any(|lane| !available.contains(lane)) {
            return Err(EvaluationContractError::CoverageViolation(
                "unknown saved-candidate lane requested for ablation".to_string(),
            ));
        }
        Ok(self
            .candidate_lists
            .iter()
            .filter(|list| !disabled_lanes.contains(&list.lane))
            .cloned()
            .collect())
    }

    pub fn compute_digest(&self) -> Result<SavedCandidateSetDigest, EvaluationContractError> {
        let input = SavedCandidateSetDigestInput {
            domain: SAVED_CANDIDATE_SET_DIGEST_DOMAIN,
            schema_revision: self.schema_revision,
            run_id: &self.run_id,
            run_manifest_digest: &self.run_manifest_digest,
            scope: self.scope,
            workload_digest: &self.workload_digest,
            candidate_lists: &self.candidate_lists,
        };
        SavedCandidateSetDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "saved candidate set",
            })
        }
    }
}

/// The typed evidence batch emitted by the harness for one run (Plan 15:
/// runs emit raw samples and aggregates whose digests validate). Scope rules
/// enforce the contamination contract: a development-scope batch contains no
/// holdout receipts and no candidate lists for sealed queries; a
/// locked-scope batch must carry at least one audited reveal receipt.
/// Canonically digested with [`EVIDENCE_BATCH_DIGEST_DOMAIN`]; the digest
/// field is excluded from the hashed bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBatchV1 {
    pub batch_id: EvidenceBatchId,
    pub run_id: RunId,
    pub scope: EvalRunScopeV1,
    pub workload_digest: WorkloadDigest,
    pub candidate_lists: Vec<CandidateListV1>,
    pub holdout_receipts: Vec<HoldoutAccessReceiptV1>,
    pub digest: EvidenceBatchDigest,
}

#[derive(Serialize)]
struct EvidenceBatchDigestInput<'a> {
    domain: &'static str,
    batch_id: &'a EvidenceBatchId,
    run_id: &'a RunId,
    scope: EvalRunScopeV1,
    workload_digest: &'a WorkloadDigest,
    candidate_lists: &'a [CandidateListV1],
    holdout_receipts: &'a [HoldoutAccessReceiptV1],
}

impl EvidenceBatchV1 {
    /// Scope-independent structural validation. Use
    /// [`EvidenceBatchV1::validate_against_workload`] for the contamination
    /// and coverage rules.
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        for list in &self.candidate_lists {
            list.validate()?;
        }
        match self.scope {
            EvalRunScopeV1::Development => {
                if !self.holdout_receipts.is_empty() {
                    return Err(EvaluationContractError::HoldoutAccessViolation(
                        "a development-scope evidence batch must not carry holdout access receipts"
                            .to_string(),
                    ));
                }
            }
            EvalRunScopeV1::Locked => {
                if self.holdout_receipts.is_empty() {
                    return Err(EvaluationContractError::HoldoutAccessViolation(
                        "a locked-scope evidence batch must carry the audited holdout reveal receipt"
                            .to_string(),
                    ));
                }
                if self
                    .holdout_receipts
                    .iter()
                    .any(|receipt| receipt.run_id != self.run_id)
                {
                    return Err(EvaluationContractError::HoldoutAccessViolation(
                        "locked evidence contains a receipt for another run".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Full validation against the executed workload: digest binding, scope
    /// rules, and — for development scope — proof that every candidate list
    /// addresses a development query and every development query received at
    /// least one candidate list.
    pub fn validate_against_workload(
        &self,
        workload: &QueryWorkloadV1,
    ) -> Result<(), EvaluationContractError> {
        if self.workload_digest != workload.compute_digest()? {
            return Err(EvaluationContractError::DigestMismatch {
                field: "evidence batch workload binding",
            });
        }
        self.validate()?;
        let mut covered: BTreeSet<&EvalQueryId> = BTreeSet::new();
        for list in &self.candidate_lists {
            let query = workload.query(&list.query_id).ok_or_else(|| {
                EvaluationContractError::CoverageViolation(format!(
                    "candidate list references unknown query {}",
                    list.query_id
                ))
            })?;
            let expected_partition = match self.scope {
                EvalRunScopeV1::Development => EvalPartitionV1::Development,
                EvalRunScopeV1::Locked => EvalPartitionV1::SealedHoldout,
            };
            if query.partition != expected_partition {
                return Err(EvaluationContractError::HoldoutAccessViolation(format!(
                    "{} evidence batch executed {} query {}",
                    match self.scope {
                        EvalRunScopeV1::Development => "development-scope",
                        EvalRunScopeV1::Locked => "locked-scope",
                    },
                    query.partition.as_str(),
                    query.query_id
                )));
            }
            covered.insert(&list.query_id);
        }
        let expected_queries: Box<dyn Iterator<Item = &EvalQueryV1> + '_> = match self.scope {
            EvalRunScopeV1::Development => Box::new(workload.development_queries()),
            EvalRunScopeV1::Locked => Box::new(workload.sealed_holdout_queries()),
        };
        for query in expected_queries {
            if !covered.contains(&query.query_id) {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "{} query {} received no candidate list",
                    query.partition.as_str(),
                    query.query_id
                )));
            }
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<EvidenceBatchDigest, EvaluationContractError> {
        let input = EvidenceBatchDigestInput {
            domain: EVIDENCE_BATCH_DIGEST_DOMAIN,
            batch_id: &self.batch_id,
            run_id: &self.run_id,
            scope: self.scope,
            workload_digest: &self.workload_digest,
            candidate_lists: &self.candidate_lists,
            holdout_receipts: &self.holdout_receipts,
        };
        EvidenceBatchDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "evidence batch",
            })
        }
    }
}

/// Frozen candidate/context limits. These are reproducibility limits only;
/// they are not promoted quality constants.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRunBudgetV1 {
    pub candidate_limit_per_lane: u32,
    pub context_byte_limit: u64,
    pub context_token_limit: u64,
    pub deadline_millis: u64,
}

/// The frozen run manifest (Plan 15 `run-v1.json`): fixture, workload, label,
/// and seal digests; the candidate revision; the frozen execution order; the
/// decision owners; and the (unevaluated at this revision) executable
/// terminal decision expression. Canonically digested with
/// [`RUN_MANIFEST_DIGEST_DOMAIN`]; the digest field is excluded from the
/// hashed bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunManifestV1 {
    pub run_id: RunId,
    pub revision: u32,
    pub scope: EvalRunScopeV1,
    pub authority: FixtureAuthorityV1,
    /// sha256 of the committed `fixture-manifest-v1.json` file bytes.
    pub fixture_manifest_digest: FixtureManifestDigest,
    /// sha256 of the committed workload file bytes.
    pub workload_file_digest: FixtureContentDigest,
    /// sha256 of the committed development label file bytes.
    pub development_label_file_digest: FixtureContentDigest,
    pub holdout_seal_digest: HoldoutSealDigest,
    pub artifact_files: Vec<FixtureFileDigestV1>,
    /// Free-form candidate revision (tool/profile revision under test).
    pub candidate_revision: String,
    pub profile_matrix: Vec<String>,
    pub model_revision: String,
    pub tokenizer_revision: String,
    pub runtime_revision: String,
    pub command_revision: String,
    pub budget: EvaluationRunBudgetV1,
    pub cache_states: Vec<String>,
    pub execution_order: Vec<EvalQueryId>,
    pub sample_size_rationale: String,
    pub measurement_tools: Vec<String>,
    pub statistical_procedures: Vec<String>,
    pub output_schema: String,
    pub locked_outcomes_accessed: bool,
    /// The frozen executable terminal decision expression. This packet
    /// freezes the field and its validation; no expression is evaluated and
    /// no outcome is claimed at this revision.
    pub decision_expression: String,
    pub decision_owners: Vec<DecisionOwnerId>,
    pub digest: RunManifestDigest,
}

#[derive(Serialize)]
struct RunManifestDigestInput<'a> {
    domain: &'static str,
    run_id: &'a RunId,
    revision: u32,
    scope: EvalRunScopeV1,
    authority: FixtureAuthorityV1,
    fixture_manifest_digest: &'a FixtureManifestDigest,
    workload_file_digest: &'a FixtureContentDigest,
    development_label_file_digest: &'a FixtureContentDigest,
    holdout_seal_digest: &'a HoldoutSealDigest,
    artifact_files: &'a [FixtureFileDigestV1],
    candidate_revision: &'a str,
    profile_matrix: &'a [String],
    model_revision: &'a str,
    tokenizer_revision: &'a str,
    runtime_revision: &'a str,
    command_revision: &'a str,
    budget: &'a EvaluationRunBudgetV1,
    cache_states: &'a [String],
    execution_order: &'a [EvalQueryId],
    sample_size_rationale: &'a str,
    measurement_tools: &'a [String],
    statistical_procedures: &'a [String],
    output_schema: &'a str,
    locked_outcomes_accessed: bool,
    decision_expression: &'a str,
    decision_owners: &'a [DecisionOwnerId],
}

impl RunManifestV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.authority == FixtureAuthorityV1::ContractOnly
            && (self.scope != EvalRunScopeV1::Development || self.locked_outcomes_accessed)
        {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "contract-only runs must be development scope and cannot access locked outcomes"
                    .to_string(),
            ));
        }
        if self.execution_order.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "run execution order",
            });
        }
        if self.artifact_files.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "run artifact files",
            });
        }
        if self.artifact_files.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "run fixture artifacts",
            });
        }
        if self.budget.candidate_limit_per_lane == 0
            || self.budget.context_byte_limit == 0
            || self.budget.context_token_limit == 0
            || self.budget.deadline_millis == 0
        {
            return Err(EvaluationContractError::Empty {
                field: "run budget",
            });
        }
        let mut order = BTreeSet::new();
        for query_id in &self.execution_order {
            if !order.insert(query_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "run execution order query_id",
                });
            }
        }
        if self.candidate_revision.is_empty()
            || self.profile_matrix.is_empty()
            || self.model_revision.is_empty()
            || self.tokenizer_revision.is_empty()
            || self.runtime_revision.is_empty()
            || self.command_revision.is_empty()
            || self.cache_states.is_empty()
            || self.sample_size_rationale.is_empty()
            || self.measurement_tools.is_empty()
            || self.statistical_procedures.is_empty()
            || self.output_schema.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "run frozen revisions/procedure",
            });
        }
        if self.decision_expression.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "run decision expression",
            });
        }
        if self.decision_owners.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "run decision owners",
            });
        }
        Ok(())
    }

    /// The execution order must cover exactly the workload's development
    /// queries for a development-scope run.
    pub fn validate_against_workload(
        &self,
        workload: &QueryWorkloadV1,
    ) -> Result<(), EvaluationContractError> {
        self.validate()?;
        let expected: BTreeSet<&EvalQueryId> = match self.scope {
            EvalRunScopeV1::Development => workload
                .development_queries()
                .map(|query| &query.query_id)
                .collect(),
            EvalRunScopeV1::Locked => workload
                .sealed_holdout_queries()
                .map(|query| &query.query_id)
                .collect(),
        };
        let actual: BTreeSet<&EvalQueryId> = self.execution_order.iter().collect();
        if expected != actual {
            return Err(EvaluationContractError::CoverageViolation(format!(
                "{} run execution order must cover exactly its partition queries",
                match self.scope {
                    EvalRunScopeV1::Development => "development",
                    EvalRunScopeV1::Locked => "locked",
                }
            )));
        }
        Ok(())
    }

    /// Gate that must succeed before the I/O layer reads a reveal capability
    /// or resolves an authorized-store locator.
    pub fn validate_pre_reveal(
        &self,
        fixture_manifest: &FixtureManifestV1,
        workload: &QueryWorkloadV1,
    ) -> Result<(), EvaluationContractError> {
        self.validate_against_workload(workload)?;
        self.verify_digest()?;
        fixture_manifest.validate()?;
        if self.scope != EvalRunScopeV1::Locked
            || self.authority != FixtureAuthorityV1::LockedQuality
            || fixture_manifest.authority != FixtureAuthorityV1::LockedQuality
        {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "holdout reveal requires matching locked-quality fixture and run authority"
                    .to_string(),
            ));
        }
        if self.locked_outcomes_accessed {
            return Err(EvaluationContractError::HoldoutAccessViolation(
                "run manifest already records locked outcome access".to_string(),
            ));
        }
        if self.holdout_seal_digest != fixture_manifest.holdout_seal.seal_digest {
            return Err(EvaluationContractError::DigestMismatch {
                field: "pre-reveal holdout seal binding",
            });
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<RunManifestDigest, EvaluationContractError> {
        let input = RunManifestDigestInput {
            domain: RUN_MANIFEST_DIGEST_DOMAIN,
            run_id: &self.run_id,
            revision: self.revision,
            scope: self.scope,
            authority: self.authority,
            fixture_manifest_digest: &self.fixture_manifest_digest,
            workload_file_digest: &self.workload_file_digest,
            development_label_file_digest: &self.development_label_file_digest,
            holdout_seal_digest: &self.holdout_seal_digest,
            artifact_files: &self.artifact_files,
            candidate_revision: &self.candidate_revision,
            profile_matrix: &self.profile_matrix,
            model_revision: &self.model_revision,
            tokenizer_revision: &self.tokenizer_revision,
            runtime_revision: &self.runtime_revision,
            command_revision: &self.command_revision,
            budget: &self.budget,
            cache_states: &self.cache_states,
            execution_order: &self.execution_order,
            sample_size_rationale: &self.sample_size_rationale,
            measurement_tools: &self.measurement_tools,
            statistical_procedures: &self.statistical_procedures,
            output_schema: &self.output_schema,
            locked_outcomes_accessed: self.locked_outcomes_accessed,
            decision_expression: &self.decision_expression,
            decision_owners: &self.decision_owners,
        };
        RunManifestDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "run manifest",
            })
        }
    }
}

/// One evidence-index claim. Contract-only fixture claims deliberately carry
/// `acceptance_authority = false`; no profile, promotion, or locked quality
/// conclusion may cite them as acceptance evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceIndexEntryV1 {
    pub claim_id: EvidenceClaimId,
    pub claim: String,
    pub fixture_artifacts: Vec<String>,
    pub run_manifest_digest: RunManifestDigest,
    #[serde(default)]
    pub aggregate_artifacts: Vec<FixtureFileDigestV1>,
    pub immutable_result_anchors: Vec<String>,
    pub acceptance_authority: bool,
}

/// Plan 15 `evidence-index.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceIndexV1 {
    pub index_id: EvidenceIndexId,
    pub revision: u32,
    pub authority: FixtureAuthorityV1,
    pub entries: Vec<EvidenceIndexEntryV1>,
    pub digest: EvidenceIndexDigest,
}

#[derive(Serialize)]
struct EvidenceIndexDigestInput<'a> {
    domain: &'static str,
    index_id: &'a EvidenceIndexId,
    revision: u32,
    authority: FixtureAuthorityV1,
    entries: &'a [EvidenceIndexEntryV1],
}

impl EvidenceIndexV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.entries.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "evidence index entries",
            });
        }
        let mut claim_ids = BTreeSet::new();
        for entry in &self.entries {
            if !claim_ids.insert(&entry.claim_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "evidence claim id",
                });
            }
            if entry.claim.is_empty()
                || entry.fixture_artifacts.is_empty()
                || entry.immutable_result_anchors.is_empty()
            {
                return Err(EvaluationContractError::Empty {
                    field: "evidence claim support",
                });
            }
            if self.authority == FixtureAuthorityV1::ContractOnly && entry.acceptance_authority {
                return Err(EvaluationContractError::CoverageViolation(
                    "contract-only evidence cannot claim acceptance authority".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<EvidenceIndexDigest, EvaluationContractError> {
        EvidenceIndexDigest::new(canonical_json_sha256(&EvidenceIndexDigestInput {
            domain: EVIDENCE_INDEX_DIGEST_DOMAIN,
            index_id: &self.index_id,
            revision: self.revision,
            authority: self.authority,
            entries: &self.entries,
        })?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "evidence index",
            })
        }
    }
}

/// Fully loaded Plan 15 fixture packet. This pure value lets the harness
/// cross-check every foreign key and partition boundary without performing
/// retrieval or evaluating a quality outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationFixtureBundleV1 {
    pub manifest: FixtureManifestV1,
    pub workload: QueryWorkloadV1,
    pub snapshots: Vec<EvalSnapshotV1>,
    pub temporal_events: Vec<TemporalEventV1>,
    pub context_spans: Vec<ContextSpanV1>,
    pub tasks: Vec<EvaluationTaskV1>,
    pub authorization_canaries: Vec<AuthorizationCanaryV1>,
    pub exact_admission_oracles: Vec<ExactAdmissionOracleV1>,
    pub contamination_partitions: ContaminationPartitionManifestV1,
    pub development_labels: LabelSetV1,
    pub run: RunManifestV1,
    pub evidence_index: EvidenceIndexV1,
}

impl EvaluationFixtureBundleV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        self.manifest.validate()?;
        self.workload.validate()?;
        self.workload.verify_digest()?;
        self.contamination_partitions.validate()?;
        self.development_labels.validate()?;
        self.development_labels.verify_digest()?;
        self.run.validate_against_workload(&self.workload)?;
        self.run.verify_digest()?;
        self.evidence_index.validate()?;
        self.evidence_index.verify_digest()?;

        if self.manifest.authority != self.run.authority
            || self.manifest.authority != self.evidence_index.authority
        {
            return Err(EvaluationContractError::CoverageViolation(
                "manifest, run, and evidence-index authority differ".to_string(),
            ));
        }
        if self.run.holdout_seal_digest != self.manifest.holdout_seal.seal_digest {
            return Err(EvaluationContractError::DigestMismatch {
                field: "run holdout seal binding",
            });
        }

        let mut snapshot_ids = BTreeSet::new();
        for snapshot in &self.snapshots {
            snapshot.validate()?;
            if snapshot.repository_commit != self.manifest.checkpoint_commit {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "snapshot {} is not pinned to the manifest checkpoint",
                    snapshot.snapshot_id
                )));
            }
            if !snapshot_ids.insert(&snapshot.snapshot_id) {
                return Err(EvaluationContractError::Duplicate {
                    field: "snapshot id",
                });
            }
        }
        if snapshot_ids.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "fixture snapshots",
            });
        }

        let mut grouped_queries = BTreeSet::new();
        for group in &self.contamination_partitions.groups {
            if !self.manifest.contamination_groups.contains(&group.group_id) {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "contamination group {} is absent from the fixture manifest",
                    group.group_id
                )));
            }
            for query_id in &group.query_ids {
                let query = self.workload.query(query_id).ok_or_else(|| {
                    EvaluationContractError::CoverageViolation(format!(
                        "contamination group references unknown query {query_id}"
                    ))
                })?;
                if query.partition != group.partition {
                    return Err(EvaluationContractError::PartitionViolation(format!(
                        "query {query_id} appears in a {} contamination group",
                        group.partition.as_str()
                    )));
                }
                if !query.contamination_groups.contains(&group.group_id) {
                    return Err(EvaluationContractError::PartitionViolation(format!(
                        "query {query_id} contamination membership is not bidirectional for {}",
                        group.group_id
                    )));
                }
                grouped_queries.insert(query_id);
            }
        }

        for query in &self.workload.queries {
            let snapshot = self
                .snapshots
                .iter()
                .find(|snapshot| snapshot.snapshot_id == query.snapshot_id)
                .ok_or_else(|| {
                    EvaluationContractError::CoverageViolation(format!(
                        "query {} references unknown snapshot {}",
                        query.query_id, query.snapshot_id
                    ))
                })?;
            if query.snapshot_commit != snapshot.repository_commit {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "query {} snapshot commit does not match its snapshot",
                    query.query_id
                )));
            }
            if !grouped_queries.contains(&query.query_id) {
                return Err(EvaluationContractError::PartitionViolation(format!(
                    "query {} is absent from contamination partitions",
                    query.query_id
                )));
            }
            for group_id in &query.contamination_groups {
                let group = self
                    .contamination_partitions
                    .groups
                    .iter()
                    .find(|group| &group.group_id == group_id)
                    .ok_or_else(|| {
                        EvaluationContractError::CoverageViolation(format!(
                            "query {} references unknown contamination group {group_id}",
                            query.query_id
                        ))
                    })?;
                if group.partition != query.partition || !group.query_ids.contains(&query.query_id)
                {
                    return Err(EvaluationContractError::PartitionViolation(format!(
                        "query {} contamination membership crosses partitions",
                        query.query_id
                    )));
                }
            }
        }

        let mut event_ids = BTreeSet::new();
        for event in &self.temporal_events {
            event.validate()?;
            if !event_ids.insert(&event.event_id)
                || self.manifest.document(&event.document_id).is_none()
                || event
                    .expected_eligibility
                    .iter()
                    .any(|eligibility| !snapshot_ids.contains(&eligibility.snapshot_id))
            {
                return Err(EvaluationContractError::CoverageViolation(
                    "temporal event references an unknown or duplicate fixture anchor".to_string(),
                ));
            }
        }
        for event in &self.temporal_events {
            if event
                .supersedes_event_id
                .as_ref()
                .is_some_and(|superseded| !event_ids.contains(superseded))
            {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "temporal event {} supersedes an unknown event",
                    event.event_id
                )));
            }
        }

        let task_ids: BTreeSet<_> = self.tasks.iter().map(|task| &task.task_id).collect();
        for task in &self.tasks {
            task.validate()?;
            if !snapshot_ids.contains(&task.snapshot_id) {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "task {} references an unknown snapshot",
                    task.task_id
                )));
            }
        }

        for span in &self.context_spans {
            span.validate()?;
            if self.workload.query(&span.query_id).is_none() {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "context span {} references an unknown query",
                    span.context_span_id
                )));
            }
            let document = self.manifest.document(&span.document_id).ok_or_else(|| {
                EvaluationContractError::CoverageViolation(format!(
                    "context span {} references an unknown document",
                    span.context_span_id
                ))
            })?;
            if span.byte_end > document.byte_len {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "context span {} exceeds document byte length",
                    span.context_span_id
                )));
            }
        }

        for judgment in &self.development_labels.judgments {
            let query = self.workload.query(&judgment.query_id).ok_or_else(|| {
                EvaluationContractError::CoverageViolation(format!(
                    "judgment {} references an unknown query",
                    judgment.judgment_id
                ))
            })?;
            if query.partition != EvalPartitionV1::Development
                || self.manifest.document(&judgment.document_id).is_none()
                || judgment
                    .task_oracle
                    .as_ref()
                    .is_some_and(|task_id| !task_ids.contains(task_id))
            {
                return Err(EvaluationContractError::PartitionViolation(format!(
                    "judgment {} crosses a fixture boundary",
                    judgment.judgment_id
                )));
            }
        }

        for canary in &self.authorization_canaries {
            canary.validate()?;
            let query = self.workload.query(&canary.query_id).ok_or_else(|| {
                EvaluationContractError::CoverageViolation(format!(
                    "canary {} references an unknown query",
                    canary.canary_id
                ))
            })?;
            if query.family != QueryFamilyV1::AuthorizationCanary
                || !query
                    .forbidden_document_ids
                    .contains(&canary.denied_document_id)
                || query.allowed_scope_ids.contains(&canary.denied_scope_id)
                || !snapshot_ids.contains(&canary.control_snapshot_id)
                || !snapshot_ids.contains(&canary.injected_snapshot_id)
            {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "authorization canary {} is not isolated",
                    canary.canary_id
                )));
            }
        }

        for oracle in &self.exact_admission_oracles {
            oracle.validate()?;
            if self.workload.query(&oracle.query_id).is_none()
                || oracle
                    .document_id
                    .as_ref()
                    .is_some_and(|document_id| self.manifest.document(document_id).is_none())
            {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "exact-admission oracle {} references an unknown anchor",
                    oracle.oracle_id
                )));
            }
        }

        let run_digest = self.run.compute_digest()?;
        if self
            .evidence_index
            .entries
            .iter()
            .any(|entry| entry.run_manifest_digest != run_digest)
        {
            return Err(EvaluationContractError::DigestMismatch {
                field: "evidence-index run binding",
            });
        }
        Ok(())
    }
}

/// The terminal decision record (Plan 15: the harness returns exactly one
/// typed outcome). Canonically digested with
/// [`DECISION_RECORD_DIGEST_DOMAIN`]; the digest field is excluded from the
/// hashed bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecordV1 {
    pub run_id: RunId,
    pub outcome: EvalOutcomeV1,
    pub rationale: String,
    pub decided_by: DecisionOwnerId,
    pub evidence_batches: Vec<EvidenceBatchDigest>,
    pub digest: DecisionRecordDigest,
}

#[derive(Serialize)]
struct DecisionRecordDigestInput<'a> {
    domain: &'static str,
    run_id: &'a RunId,
    outcome: EvalOutcomeV1,
    rationale: &'a str,
    decided_by: &'a DecisionOwnerId,
    evidence_batches: &'a [EvidenceBatchDigest],
}

impl DecisionRecordV1 {
    pub fn validate(&self) -> Result<(), EvaluationContractError> {
        if self.rationale.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "decision rationale",
            });
        }
        if self.outcome.permits_promotion() && self.evidence_batches.is_empty() {
            return Err(EvaluationContractError::Empty {
                field: "accepted decision evidence batches",
            });
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<DecisionRecordDigest, EvaluationContractError> {
        let input = DecisionRecordDigestInput {
            domain: DECISION_RECORD_DIGEST_DOMAIN,
            run_id: &self.run_id,
            outcome: self.outcome,
            rationale: &self.rationale,
            decided_by: &self.decided_by,
            evidence_batches: &self.evidence_batches,
        };
        DecisionRecordDigest::new(canonical_json_sha256(&input)?)
    }

    pub fn verify_digest(&self) -> Result<(), EvaluationContractError> {
        if self.compute_digest()? == self.digest {
            Ok(())
        } else {
            Err(EvaluationContractError::DigestMismatch {
                field: "decision record",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn query(query_id: &str, partition: EvalPartitionV1) -> EvalQueryV1 {
        EvalQueryV1 {
            query_id: id(query_id),
            partition,
            family: QueryFamilyV1::ExactSymbol,
            provider: "cursor".to_string(),
            language: "rust".to_string(),
            repository_family_cluster: "tracedecay-domain".to_string(),
            snapshot_id: id("snapshot.fixture.v1"),
            snapshot_commit: "eda50f53000ab4f96ef30e1f3a46b748b3fea6e0".to_string(),
            as_of_unix_micros: 1_784_499_129_000_000,
            principal_class: "fixture-principal".to_string(),
            privacy_domain_class: "fixture-public".to_string(),
            allowed_scope_ids: vec!["scope.fixture".to_string()],
            query_text: "UtcMicros".to_string(),
            authorized_private_query_locator_digest: None,
            contamination_groups: vec![id("cg.fixture")],
            forbidden_document_ids: Vec::new(),
        }
    }

    fn workload() -> QueryWorkloadV1 {
        let mut workload = QueryWorkloadV1 {
            revision: 1,
            queries: vec![
                query("q-dev-001", EvalPartitionV1::Development),
                query("q-hold-001", EvalPartitionV1::SealedHoldout),
            ],
            digest: id(ZERO_DIGEST),
        };
        workload.digest = workload.compute_digest().expect("digest computable");
        workload
    }

    fn label_set(partition: EvalPartitionV1) -> LabelSetV1 {
        LabelSetV1 {
            label_set_id: id("labels.fixture.v1"),
            revision: 1,
            partition,
            judgments: vec![RelevanceJudgmentV1 {
                judgment_id: id("judgment.fixture.001"),
                query_id: id("q-dev-001"),
                document_id: id("doc.fixture"),
                symbol: Some("UtcMicros".to_string()),
                grade: RelevanceGradeV1::HighlyRelevant,
                evidence_role: LabelEvidenceRoleV1::Primary,
                valid_from_unix_micros: 1_784_499_129_000_000,
                valid_until_unix_micros: None,
                supersedes_judgment_id: None,
                logical_copy_group: None,
                forbidden_anchor_ids: Vec::new(),
                abstention_oracle: false,
                task_oracle: None,
                labeler: "fixture-author".to_string(),
                labeler_provenance: "human:pr9-13".to_string(),
                adjudication: "single_label_contract_fixture".to_string(),
                correction_revision: 1,
                note: None,
            }],
            digest: id(ZERO_DIGEST),
        }
    }

    fn batch(scope: EvalRunScopeV1, workload: &QueryWorkloadV1) -> EvidenceBatchV1 {
        EvidenceBatchV1 {
            batch_id: id("batch.fixture.001"),
            run_id: id("run.fixture.001"),
            scope,
            workload_digest: workload.compute_digest().unwrap(),
            candidate_lists: vec![CandidateListV1 {
                query_id: id("q-dev-001"),
                lane: id("tracedecay_search"),
                candidates: vec![EvalCandidateV1 {
                    anchor: EvalCandidateAnchorV1 {
                        document_id: id("doc.fixture"),
                        symbol: None,
                    },
                    ordinal_rank: 0,
                }],
            }],
            holdout_receipts: Vec::new(),
            digest: id(ZERO_DIGEST),
        }
    }

    #[test]
    fn string_ids_reject_noncanonical_identities() {
        assert!(EvalQueryId::new("").is_err());
        assert!(EvalQueryId::new(" padded").is_err());
        assert!(EvalQueryId::new("control\nchar").is_err());
        assert!(EvalQueryId::new("q-dev-001").is_ok());
    }

    #[test]
    fn digest_ids_require_tagged_sha256() {
        assert!(FixtureContentDigest::new("deadbeef").is_err());
        assert!(FixtureContentDigest::new("sha256:XYZ").is_err());
        assert!(FixtureContentDigest::new(ZERO_DIGEST).is_ok());
    }

    #[test]
    fn workload_digest_is_self_verifying_and_tamper_evident() {
        let mut workload = workload();
        workload.verify_digest().expect("digest verifies");
        workload.queries[0].query_text = "tampered".to_string();
        assert!(matches!(
            workload.verify_digest(),
            Err(EvaluationContractError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn development_label_set_rejects_sealed_partitions() {
        for partition in [
            EvalPartitionV1::SealedHoldout,
            EvalPartitionV1::ForwardConfirmation,
        ] {
            assert!(matches!(
                label_set(partition).validate(),
                Err(EvaluationContractError::PartitionViolation(_))
            ));
        }
        let mut development = label_set(EvalPartitionV1::Development);
        development
            .validate()
            .expect("development labels are tunable");
        development.digest = development.compute_digest().unwrap();
        development.verify_digest().expect("label digest verifies");
    }

    #[test]
    fn development_batch_proves_no_holdout_access() {
        let workload = workload();
        let mut dev = batch(EvalRunScopeV1::Development, &workload);
        dev.validate_against_workload(&workload)
            .expect("development batch covers exactly the development queries");
        dev.digest = dev.compute_digest().unwrap();
        dev.verify_digest().expect("batch digest verifies");

        // A receipt in a development batch is an access violation.
        let mut leaked = batch(EvalRunScopeV1::Development, &workload);
        leaked.holdout_receipts.push(HoldoutAccessReceiptV1 {
            run_id: id("run.fixture.001"),
            run_manifest_digest: id(ZERO_DIGEST),
            seal_digest: id(ZERO_DIGEST),
            revealed_by: id("owner.fixture"),
            rationale: "unauthorized".to_string(),
        });
        assert!(matches!(
            leaked.validate_against_workload(&workload),
            Err(EvaluationContractError::HoldoutAccessViolation(_))
        ));

        // A candidate list for a sealed query is an access violation.
        let mut executed_holdout = batch(EvalRunScopeV1::Development, &workload);
        executed_holdout.candidate_lists.push(CandidateListV1 {
            query_id: id("q-hold-001"),
            lane: id("tracedecay_grep"),
            candidates: Vec::new(),
        });
        assert!(matches!(
            executed_holdout.validate_against_workload(&workload),
            Err(EvaluationContractError::HoldoutAccessViolation(_))
        ));

        // A locked batch without a receipt is invalid.
        let locked = batch(EvalRunScopeV1::Locked, &workload);
        assert!(matches!(
            locked.validate(),
            Err(EvaluationContractError::HoldoutAccessViolation(_))
        ));
    }

    #[test]
    fn development_batch_requires_full_query_coverage() {
        let workload = workload();
        let mut incomplete = batch(EvalRunScopeV1::Development, &workload);
        incomplete.candidate_lists.clear();
        assert!(matches!(
            incomplete.validate_against_workload(&workload),
            Err(EvaluationContractError::CoverageViolation(_))
        ));
    }

    #[test]
    fn candidate_list_rejects_duplicate_ranks_and_flags_canaries() {
        let anchor = || EvalCandidateAnchorV1 {
            document_id: id("doc.fixture"),
            symbol: None,
        };
        let duplicate = CandidateListV1 {
            query_id: id("q-dev-001"),
            lane: id("tracedecay_search"),
            candidates: vec![
                EvalCandidateV1 {
                    anchor: anchor(),
                    ordinal_rank: 0,
                },
                EvalCandidateV1 {
                    anchor: anchor(),
                    ordinal_rank: 0,
                },
            ],
        };
        assert!(duplicate.validate().is_err());

        let list = CandidateListV1 {
            query_id: id("q-dev-001"),
            lane: id("tracedecay_search"),
            candidates: vec![EvalCandidateV1 {
                anchor: anchor(),
                ordinal_rank: 0,
            }],
        };
        let forbidden: Vec<CorpusDocumentId> = vec![id("doc.fixture")];
        assert_eq!(list.forbidden_hits(&forbidden).count(), 1);
    }

    #[test]
    fn run_manifest_binds_workload_and_freezes_order() {
        let workload = workload();
        let mut manifest = RunManifestV1 {
            run_id: id("run.fixture.001"),
            revision: 1,
            scope: EvalRunScopeV1::Development,
            authority: FixtureAuthorityV1::ContractOnly,
            fixture_manifest_digest: id(ZERO_DIGEST),
            workload_file_digest: id(ZERO_DIGEST),
            development_label_file_digest: id(ZERO_DIGEST),
            holdout_seal_digest: id(ZERO_DIGEST),
            artifact_files: vec![FixtureFileDigestV1 {
                path: "queries-v1.jsonl".to_string(),
                byte_len: 1,
                digest: id(ZERO_DIGEST),
            }],
            candidate_revision: "fixture-candidate.v1".to_string(),
            profile_matrix: vec!["contract-only-no-profile".to_string()],
            model_revision: "not-applicable".to_string(),
            tokenizer_revision: "tokenizer.fixture.v1".to_string(),
            runtime_revision: "runtime.fixture.v1".to_string(),
            command_revision: "command.fixture.v1".to_string(),
            budget: EvaluationRunBudgetV1 {
                candidate_limit_per_lane: 10,
                context_byte_limit: 4096,
                context_token_limit: 1024,
                deadline_millis: 1_000,
            },
            cache_states: vec!["cold".to_string()],
            execution_order: vec![id("q-dev-001")],
            sample_size_rationale: "contract validation only".to_string(),
            measurement_tools: vec!["none-contract-only".to_string()],
            statistical_procedures: vec!["none-contract-only".to_string()],
            output_schema: "tracedecay.eval-output.v1".to_string(),
            locked_outcomes_accessed: false,
            decision_expression: "undecided_at_this_revision".to_string(),
            decision_owners: vec![id("owner.fixture")],
            digest: id(ZERO_DIGEST),
        };
        manifest
            .validate_against_workload(&workload)
            .expect("order covers the development queries");
        manifest.digest = manifest.compute_digest().unwrap();
        manifest.verify_digest().expect("run digest verifies");

        manifest.execution_order.push(id("q-hold-001"));
        assert!(matches!(
            manifest.validate_against_workload(&workload),
            Err(EvaluationContractError::CoverageViolation(_))
        ));
    }

    #[test]
    fn accepted_decision_requires_evidence() {
        let record = |outcome, evidence: Vec<EvidenceBatchDigest>| DecisionRecordV1 {
            run_id: id("run.fixture.001"),
            outcome,
            rationale: "fixture rationale".to_string(),
            decided_by: id("owner.fixture"),
            evidence_batches: evidence,
            digest: id(ZERO_DIGEST),
        };
        assert!(
            record(EvalOutcomeV1::Accepted, Vec::new())
                .validate()
                .is_err()
        );
        let mut accepted = record(
            EvalOutcomeV1::Accepted,
            vec![EvidenceBatchDigest::new(ZERO_DIGEST).unwrap()],
        );
        accepted.validate().expect("accepted with evidence");
        accepted.digest = accepted.compute_digest().unwrap();
        accepted.verify_digest().expect("decision digest verifies");
        assert!(EvalOutcomeV1::Accepted.permits_promotion());
        assert!(!EvalOutcomeV1::InvalidRun.permits_quality_conclusion());
    }
}
