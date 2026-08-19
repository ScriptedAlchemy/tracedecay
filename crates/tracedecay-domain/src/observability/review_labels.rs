//! The one canonical review and outcome label vocabulary for delivered work.
//!
//! The vocabulary, its legality rules, and the evidence gate come from the
//! "Canonical review and outcome labels" section of
//! `docs/plans/tracedecay-v2/26-observability-accounting-and-usage.md`:
//!
//! - One owned label schema. Every label records schema revision,
//!   work/acceptance/decomposition identity, attempt and evidence horizon,
//!   valid/observation time, source class, retrieval anchors,
//!   coverage/confidence, reviewer identity where permitted, and
//!   conflict/override provenance.
//! - The exhaustive lifecycle labels are `Pending`, `ObservedPartial`,
//!   `Reviewable`, `Accepted`, `Rejected`, `Censored`, and `Unknown`; review
//!   independence and review judgment are separate closed dimensions.
//! - `Accepted` and `Rejected` describe *independently evidenced* outcome
//!   judgment. Runtime terminal status, provider outcomes, and worker
//!   self-report remain evidence that may support — but never substitute for —
//!   these labels, so [`IndependentReviewEvidenceV1`] is the only value that
//!   can carry a label into `Accepted` or `Rejected`.
//! - `Censored` names a known observation cutoff and always carries one;
//!   `Unknown` means the available evidence cannot classify the outcome and
//!   never carries a cutoff. The two are structurally distinguishable.
//! - Late or corrected evidence appends a new label revision and leaves prior
//!   labels queryable; a correction never rewrites the revision it supersedes.
//!
//! The graph transition table that consumes these labels lives with the work
//! contracts. This module owns the vocabulary only: it neither mints a second
//! spelling of the same judgment nor coerces one label into another.

use serde::{Deserialize, Serialize};

use super::CoverageStateV1;
use crate::canonical_text::{CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within};

/// The only accepted schema revision of the label record.
///
/// Adding or reinterpreting a label increments this revision rather than
/// silently rewriting the meaning of already recorded history.
pub const REVIEW_OUTCOME_LABEL_SCHEMA_REVISION: u32 = 1;

/// Upper bound on authorized retrieval anchors retained per label.
pub const REVIEW_OUTCOME_ANCHOR_LIMIT: usize = 8;

/// Exhaustive task-outcome lifecycle labels.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcomeLabelV1 {
    /// No outcome evidence has closed yet.
    Pending,
    /// Some outcome evidence exists but the horizon is not complete.
    ObservedPartial,
    /// Evidence is complete enough to review; no judgment exists yet.
    Reviewable,
    /// Independently evidenced acceptance of the delivered work.
    Accepted,
    /// Independently evidenced rejection of the delivered work.
    Rejected,
    /// A known observation cutoff stopped the measurement.
    Censored,
    /// Available evidence cannot classify the outcome.
    Unknown,
}

impl TaskOutcomeLabelV1 {
    /// Every label, in lifecycle order, for exhaustive projection and fixtures.
    pub const ALL: [Self; 7] = [
        Self::Pending,
        Self::ObservedPartial,
        Self::Reviewable,
        Self::Accepted,
        Self::Rejected,
        Self::Censored,
        Self::Unknown,
    ];

    /// Whether the label may exist only on independently evidenced judgment.
    #[must_use]
    pub const fn requires_independent_review(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected)
    }

    /// Whether the label states a known observation cutoff rather than an
    /// unclassifiable one. `Censored` and `Unknown` are never interchangeable.
    #[must_use]
    pub const fn requires_observation_cutoff(self) -> bool {
        matches!(self, Self::Censored)
    }
}

/// Independence of the review that produced a judgment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewIndependenceV1 {
    /// Reviewed by an actor distinct from the one that produced the work.
    Independent,
    /// Reviewed by the producing actor, or by one acting on its behalf.
    NonIndependent,
    /// A declared conflict of interest applies to the reviewer.
    Conflicted,
    /// No review exists.
    Missing,
    /// Review independence cannot be established from available evidence.
    Unknown,
}

impl ReviewIndependenceV1 {
    /// Every independence value, for exhaustive projection and fixtures.
    pub const ALL: [Self; 5] = [
        Self::Independent,
        Self::NonIndependent,
        Self::Conflicted,
        Self::Missing,
        Self::Unknown,
    ];

    /// Only `Independent` satisfies the independent-evidence requirement.
    #[must_use]
    pub const fn is_independent(self) -> bool {
        matches!(self, Self::Independent)
    }
}

/// Judgment recorded by a review.
///
/// `Partial` review judgment does not imply an `ObservedPartial` task outcome;
/// the two dimensions are measured separately.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewJudgmentV1 {
    Accepted,
    Rejected,
    Partial,
    Unknown,
}

impl ReviewJudgmentV1 {
    /// Every judgment value, for exhaustive projection and fixtures.
    pub const ALL: [Self; 4] = [Self::Accepted, Self::Rejected, Self::Partial, Self::Unknown];
}

/// Source class of the evidence behind a label.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeEvidenceSourceV1 {
    /// A runtime terminal state such as completed, failed, cancelled, or
    /// timed out. Supporting evidence only.
    RuntimeTerminal,
    /// A provider-reported outcome. Supporting evidence only.
    ProviderOutcome,
    /// The executing worker's own report. Supporting evidence only.
    WorkerSelfReport,
    /// A review performed by an actor independent of the producing one.
    IndependentReview,
    /// The evidence source cannot be established.
    Unknown,
}

impl OutcomeEvidenceSourceV1 {
    /// Every source class, for exhaustive projection and fixtures.
    pub const ALL: [Self; 5] = [
        Self::RuntimeTerminal,
        Self::ProviderOutcome,
        Self::WorkerSelfReport,
        Self::IndependentReview,
        Self::Unknown,
    ];

    /// Whether the source can carry a label into `Accepted` or `Rejected`.
    #[must_use]
    pub const fn is_independent_review(self) -> bool {
        matches!(self, Self::IndependentReview)
    }
}

/// The known cutoff that censored an observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCutoffV1 {
    Cancelled,
    Superseded,
    LostAuthority,
    UnfinishedHorizon,
    Unknown,
}

impl ObservationCutoffV1 {
    /// Every cutoff reason, for exhaustive projection and fixtures.
    pub const ALL: [Self; 5] = [
        Self::Cancelled,
        Self::Superseded,
        Self::LostAuthority,
        Self::UnfinishedHorizon,
        Self::Unknown,
    ];
}

/// How a label revision resolved conflicting evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelConflictResolutionV1 {
    /// Independent review overrode a prior, less authoritative revision.
    IndependentReviewOverride,
    /// Late evidence corrected a prior revision without overriding authority.
    LateCorrection,
    /// The conflict is recorded and still open.
    Unresolved,
}

/// Provenance of a conflict or override between label revisions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LabelConflictProvenanceV1 {
    pub conflicting_label_revision: u64,
    pub conflicting_evidence_source: OutcomeEvidenceSourceV1,
    pub resolution: LabelConflictResolutionV1,
}

/// The evidence horizon a label was computed over.
///
/// An incomplete horizon can never be reported as a closed outcome; it is the
/// difference between "not yet observed" and "observed to be absent".
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceHorizonV1 {
    pub horizon_end_micros: i64,
    pub complete: bool,
}

impl EvidenceHorizonV1 {
    /// A horizon that has closed at `horizon_end_micros`.
    #[must_use]
    pub const fn complete(horizon_end_micros: i64) -> Self {
        Self {
            horizon_end_micros,
            complete: true,
        }
    }

    /// A horizon still open at `horizon_end_micros`.
    #[must_use]
    pub const fn open(horizon_end_micros: i64) -> Self {
        Self {
            horizon_end_micros,
            complete: false,
        }
    }
}

/// The work the label is about.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutcomeSubjectV1 {
    pub work_ref: String,
    pub attempt_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decomposition_ref: Option<String>,
}

impl ReviewOutcomeSubjectV1 {
    /// Rejects subjects whose identity cannot be projected without inventing
    /// a work, attempt, acceptance, or decomposition reference.
    pub fn validate(&self) -> Result<(), &'static str> {
        let required = [self.work_ref.as_str(), self.attempt_ref.as_str()];
        let optional = [
            self.acceptance_ref.as_deref(),
            self.decomposition_ref.as_deref(),
        ];
        if !required
            .into_iter()
            .chain(optional.into_iter().flatten())
            .all(|value| is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES))
        {
            return Err("review_outcome_subject");
        }
        Ok(())
    }
}

/// Revision and time identity of one label record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutcomeIdentityV1 {
    pub subject: ReviewOutcomeSubjectV1,
    pub label_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_label_revision: Option<u64>,
    pub valid_from_micros: i64,
    pub observation_time_micros: i64,
}

impl ReviewOutcomeIdentityV1 {
    /// Rejects identities that would rewrite rather than append history, or
    /// that observe a label before it becomes valid.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.subject.validate()?;
        if self.label_revision == 0
            || self
                .supersedes_label_revision
                .is_some_and(|prior| prior >= self.label_revision)
        {
            return Err("review_outcome_label_revision");
        }
        if self.observation_time_micros < self.valid_from_micros {
            return Err("review_outcome_temporal_range");
        }
        Ok(())
    }
}

/// The three closed label dimensions carried together.
///
/// Serde deserialization is deliberately unconditional: any combination
/// decodes, and [`ReviewOutcomeDispositionV1::validate`] is the single place
/// that states which combinations are legal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutcomeDispositionV1 {
    pub outcome: TaskOutcomeLabelV1,
    pub independence: ReviewIndependenceV1,
    pub judgment: ReviewJudgmentV1,
}

impl ReviewOutcomeDispositionV1 {
    #[must_use]
    pub const fn new(
        outcome: TaskOutcomeLabelV1,
        independence: ReviewIndependenceV1,
        judgment: ReviewJudgmentV1,
    ) -> Self {
        Self {
            outcome,
            independence,
            judgment,
        }
    }

    /// Whether the label, independence, and judgment agree.
    ///
    /// `Accepted` and `Rejected` require an independent judgment of the same
    /// name. `Pending` and `Reviewable` state that no judgment exists yet, so
    /// they cannot carry one. `ObservedPartial`, `Censored`, and `Unknown`
    /// describe measurement state rather than judgment and stay orthogonal to
    /// it, so every judgment remains representable alongside them.
    #[must_use]
    pub const fn is_legal(&self) -> bool {
        match self.outcome {
            TaskOutcomeLabelV1::Accepted => {
                self.independence.is_independent()
                    && matches!(self.judgment, ReviewJudgmentV1::Accepted)
            }
            TaskOutcomeLabelV1::Rejected => {
                self.independence.is_independent()
                    && matches!(self.judgment, ReviewJudgmentV1::Rejected)
            }
            TaskOutcomeLabelV1::Pending => {
                matches!(self.judgment, ReviewJudgmentV1::Unknown)
                    && matches!(
                        self.independence,
                        ReviewIndependenceV1::Missing | ReviewIndependenceV1::Unknown
                    )
            }
            TaskOutcomeLabelV1::Reviewable => matches!(self.judgment, ReviewJudgmentV1::Unknown),
            TaskOutcomeLabelV1::ObservedPartial
            | TaskOutcomeLabelV1::Censored
            | TaskOutcomeLabelV1::Unknown => true,
        }
    }

    /// [`Self::is_legal`] as a rejection.
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.is_legal() {
            Ok(())
        } else {
            Err("review_outcome_disposition")
        }
    }
}

/// Evidence that an actor independent of the producing one judged the work.
///
/// This type is the gate. It cannot be constructed from a runtime terminal
/// state, a provider outcome, or a worker self-report, and it is the only
/// input [`ReviewOutcomeLabelV1::from_independent_review`] accepts — so no
/// caller can reach `Accepted` or `Rejected` through self-reported evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndependentReviewEvidenceV1 {
    reviewer_ref: String,
    judgment: ReviewJudgmentV1,
    horizon: EvidenceHorizonV1,
    coverage: CoverageStateV1,
}

impl IndependentReviewEvidenceV1 {
    /// Rejects any evidence that is not an identified, independent review over
    /// a closed horizon with established coverage.
    pub fn new(
        reviewer_ref: impl Into<String>,
        independence: ReviewIndependenceV1,
        judgment: ReviewJudgmentV1,
        horizon: EvidenceHorizonV1,
        coverage: CoverageStateV1,
    ) -> Result<Self, &'static str> {
        let reviewer_ref = reviewer_ref.into();
        if !independence.is_independent() {
            return Err("review_evidence_independence");
        }
        if !is_canonical_text_within(&reviewer_ref, CANONICAL_TEXT_MAX_BYTES) {
            return Err("review_evidence_reviewer_ref");
        }
        if !horizon.complete {
            return Err("review_evidence_horizon");
        }
        if matches!(coverage, CoverageStateV1::Unknown) {
            return Err("review_evidence_coverage");
        }
        Ok(Self {
            reviewer_ref,
            judgment,
            horizon,
            coverage,
        })
    }

    #[must_use]
    pub fn reviewer_ref(&self) -> &str {
        &self.reviewer_ref
    }

    #[must_use]
    pub const fn judgment(&self) -> ReviewJudgmentV1 {
        self.judgment
    }

    #[must_use]
    pub const fn horizon(&self) -> EvidenceHorizonV1 {
        self.horizon
    }

    #[must_use]
    pub const fn coverage(&self) -> CoverageStateV1 {
        self.coverage
    }
}

/// Runtime terminal status, a provider outcome, or a worker self-report.
///
/// Supporting evidence only. A label built from this value can describe
/// measurement state, but the evidence-source rule in
/// [`ReviewOutcomeLabelV1::validate`] refuses to let it reach `Accepted` or
/// `Rejected`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeOutcomeEvidenceV1 {
    source: OutcomeEvidenceSourceV1,
    horizon: EvidenceHorizonV1,
    coverage: CoverageStateV1,
}

impl RuntimeOutcomeEvidenceV1 {
    /// Rejects an attempt to relabel independent review as runtime evidence.
    pub const fn new(
        source: OutcomeEvidenceSourceV1,
        horizon: EvidenceHorizonV1,
        coverage: CoverageStateV1,
    ) -> Result<Self, &'static str> {
        if source.is_independent_review() {
            return Err("runtime_evidence_source");
        }
        Ok(Self {
            source,
            horizon,
            coverage,
        })
    }

    #[must_use]
    pub const fn source(&self) -> OutcomeEvidenceSourceV1 {
        self.source
    }

    #[must_use]
    pub const fn horizon(&self) -> EvidenceHorizonV1 {
        self.horizon
    }

    #[must_use]
    pub const fn coverage(&self) -> CoverageStateV1 {
        self.coverage
    }
}

/// One immutable revision of the canonical review and outcome label.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutcomeLabelV1 {
    pub schema_revision: u32,
    pub identity: ReviewOutcomeIdentityV1,
    pub disposition: ReviewOutcomeDispositionV1,
    pub evidence_source: OutcomeEvidenceSourceV1,
    pub evidence_horizon: EvidenceHorizonV1,
    pub coverage: CoverageStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_ppm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_cutoff: Option<ObservationCutoffV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retrieval_anchor_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_provenance: Option<LabelConflictProvenanceV1>,
}

impl ReviewOutcomeLabelV1 {
    /// The only constructor that can reach `Accepted` or `Rejected`.
    ///
    /// The judgment, reviewer identity, horizon, and coverage all come from
    /// the independent-review evidence, so a caller cannot supply a judgment
    /// the review did not make.
    pub fn from_independent_review(
        identity: ReviewOutcomeIdentityV1,
        outcome: TaskOutcomeLabelV1,
        evidence: &IndependentReviewEvidenceV1,
    ) -> Result<Self, &'static str> {
        let label = Self {
            schema_revision: REVIEW_OUTCOME_LABEL_SCHEMA_REVISION,
            identity,
            disposition: ReviewOutcomeDispositionV1::new(
                outcome,
                ReviewIndependenceV1::Independent,
                evidence.judgment(),
            ),
            evidence_source: OutcomeEvidenceSourceV1::IndependentReview,
            evidence_horizon: evidence.horizon(),
            coverage: evidence.coverage(),
            confidence_ppm: None,
            observation_cutoff: None,
            reviewer_ref: Some(evidence.reviewer_ref().to_owned()),
            retrieval_anchor_refs: Vec::new(),
            conflict_provenance: None,
        };
        label.validate()?;
        Ok(label)
    }

    /// Builds a label from runtime, provider, or self-reported evidence.
    ///
    /// Such evidence can describe measurement state only: an `Accepted` or
    /// `Rejected` disposition is rejected here, never downgraded silently into
    /// a different label.
    pub fn from_runtime_evidence(
        identity: ReviewOutcomeIdentityV1,
        disposition: ReviewOutcomeDispositionV1,
        evidence: RuntimeOutcomeEvidenceV1,
        observation_cutoff: Option<ObservationCutoffV1>,
    ) -> Result<Self, &'static str> {
        let label = Self {
            schema_revision: REVIEW_OUTCOME_LABEL_SCHEMA_REVISION,
            identity,
            disposition,
            evidence_source: evidence.source(),
            evidence_horizon: evidence.horizon(),
            coverage: evidence.coverage(),
            confidence_ppm: None,
            observation_cutoff,
            reviewer_ref: None,
            retrieval_anchor_refs: Vec::new(),
            conflict_provenance: None,
        };
        label.validate()?;
        Ok(label)
    }

    /// Rejects records that would report an outcome the evidence cannot carry.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_revision != REVIEW_OUTCOME_LABEL_SCHEMA_REVISION {
            return Err("review_outcome_schema_revision");
        }
        self.identity.validate()?;
        self.disposition.validate()?;

        let outcome = self.disposition.outcome;
        if outcome.requires_independent_review()
            && (!self.evidence_source.is_independent_review()
                || !self.evidence_horizon.complete
                || self.reviewer_ref.is_none())
        {
            return Err("review_outcome_independent_evidence");
        }
        if outcome.requires_independent_review()
            && matches!(self.coverage, CoverageStateV1::Unknown)
        {
            return Err("review_outcome_coverage");
        }
        if outcome.requires_observation_cutoff() != self.observation_cutoff.is_some() {
            return Err("review_outcome_observation_cutoff");
        }
        if matches!(outcome, TaskOutcomeLabelV1::Pending) && self.evidence_horizon.complete {
            return Err("review_outcome_evidence_horizon");
        }
        if self.observation_cutoff == Some(ObservationCutoffV1::UnfinishedHorizon)
            && self.evidence_horizon.complete
        {
            return Err("review_outcome_evidence_horizon");
        }
        if self
            .reviewer_ref
            .as_deref()
            .is_some_and(|value| !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES))
        {
            return Err("review_outcome_reviewer_ref");
        }
        if self.confidence_ppm.is_some_and(|value| value > 1_000_000) {
            return Err("review_outcome_confidence");
        }
        if self.retrieval_anchor_refs.len() > REVIEW_OUTCOME_ANCHOR_LIMIT
            || self
                .retrieval_anchor_refs
                .iter()
                .enumerate()
                .any(|(index, anchor)| {
                    !is_canonical_text_within(anchor, CANONICAL_TEXT_MAX_BYTES)
                        || self.retrieval_anchor_refs[..index].contains(anchor)
                })
        {
            return Err("review_outcome_anchor_refs");
        }
        if let Some(provenance) = &self.conflict_provenance
            && (provenance.conflicting_label_revision >= self.identity.label_revision
                || (matches!(
                    provenance.resolution,
                    LabelConflictResolutionV1::IndependentReviewOverride
                ) && !self.evidence_source.is_independent_review()))
        {
            return Err("review_outcome_conflict_provenance");
        }
        Ok(())
    }

    /// Whether this revision appends to `prior` for the same subject rather
    /// than rewriting it. A correction never reuses the superseded revision.
    #[must_use]
    pub fn is_correction_of(&self, prior: &Self) -> bool {
        self.identity.subject == prior.identity.subject
            && self.identity.label_revision > prior.identity.label_revision
            && self.identity.supersedes_label_revision == Some(prior.identity.label_revision)
    }
}
