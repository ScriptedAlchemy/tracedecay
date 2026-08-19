//! Transport-neutral Doctor kernel contract types.
//!
//! The Doctor application use case composes typed inputs from the advisory,
//! configuration, storage-runtime, language server, semantic index, and
//! observability authorities into stable finding families. It never evaluates a
//! generic health score or collapses unknown/partial evidence into a healthy or
//! clean result. Findings contain diagnostic evidence only.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ApplicationContractError;
use crate::identity::application_identifier;

/// Stable Doctor finding families.
///
/// The initial list covers advisory findings from
/// Brain, Explorer, Loom, Code, and Observatory, plus the legacy
/// `core_doctor` checks (graph quick-check, temporal/migration health,
/// configuration compatibility drift, semantic runtime, session ingest).
/// Each family maps to one audited typed input surface. The set is kept small
/// and honest; new families are added through a future versioned enum rather
/// than by widening the meaning of an existing variant.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorFindingFamilyV1 {
    /// advisory/scout findings (GitHub review, CI localization,
    /// proximity, context scout) — `crate::advisory` / domain feedback.
    Advisory,
    /// Desired-versus-effective configuration and compatibility drift from
    /// `ProjectConfigurationRuntime` / `ConfigurationControlPlane`.
    Configuration,
    /// Store, graph, and temporal runtime health plus migration coverage
    /// (`RuntimeReadOperationV1` health family, `StoreRuntimeClientLease`).
    StorageRuntime,
    /// Storage retention, size, and efficiency over canonical observability
    /// read models. Distinct from [`Self::StorageRuntime`] health: this
    /// family surfaces over-budget stores, identity-drift orphans, quarantined
    /// incident debris, and retention backlog. The typed
    /// subclass vocabulary is [`DoctorStorageFindingKindV1`].
    Storage,
    /// Language-server / analyzer engine status from the LSP gateway's
    /// `AnalyzerState`.
    LanguageServer,
    /// Semantic search / index runtime state (indexing, stale, unavailable).
    SemanticIndex,
    /// Denominator-safe measurement and telemetry health from analytics,
    /// accounting read models, and session ingest.
    Observability,
}

/// Typed subclasses of the [`DoctorFindingFamilyV1::Storage`] finding family.
///
/// The storage family never reports a silent overage: each subclass names one
/// observable retention/size condition Doctor surfaces over canonical size
/// observability read models. The set is closed and grows only through a future
/// versioned enum, never by widening an existing subclass.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStorageFindingKindV1 {
    /// A store exceeds its owner-configured soft size budget.
    OverBudgetStore,
    /// A store whose project identity no longer resolves to a live repository
    /// root (identity-drift orphan), reported with age and size.
    OrphanStore,
    /// Quarantined recovery/corruption artifacts are present and awaiting
    /// collection.
    IncidentDebrisPresent,
    /// Retention-eligible rows or stores are past their window and awaiting
    /// offload/collection.
    RetentionBacklog,
    /// Per-table SQLite payload growth observed between two retained
    /// watermarks, including baseline and unavailable measurement states.
    TableGrowth,
}

/// Exact Doctor evidence states.
///
/// Missing, partial, or unknown truth never becomes healthy or clean. Only
/// [`DoctorEvidenceStateV1::HealthyCompleteCoverage`] asserts a healthy result,
/// and only when its finding carries complete coverage (see
/// [`DoctorFindingV1::new`]).
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorEvidenceStateV1 {
    /// The owning authority does not support this evidence on this platform.
    Unsupported,
    /// The evidence source is supported but produced nothing.
    Absent,
    /// The evidence exists but is behind the current generation/watermark.
    Stale,
    /// The evidence proves a degraded but observed condition.
    Degraded,
    /// Only part of the evidence was observed.
    Partial,
    /// The evidence state could not be determined.
    Unknown,
    /// Authorization to read the evidence was denied.
    Denied,
    /// The evidence proves a healthy condition with complete coverage.
    HealthyCompleteCoverage,
}

impl DoctorEvidenceStateV1 {
    /// True only for the single state that asserts complete healthy coverage.
    #[must_use]
    pub const fn is_healthy_complete(self) -> bool {
        matches!(self, Self::HealthyCompleteCoverage)
    }
}

/// Whether Doctor observed all of a family's evidence sources.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCoverageCompletenessV1 {
    /// Every relevant evidence source for the family was observed.
    Complete,
    /// Some evidence sources were observed; others were omitted.
    Partial,
    /// Whether coverage is complete could not be determined.
    Unknown,
}

impl DoctorCoverageCompletenessV1 {
    #[must_use]
    const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

application_identifier!(
    @no_conversions
    /// Durable, non-disclosing reference to one owning-authority evidence
    /// record (for example a `FeedbackFindingId`, configuration revision, or
    /// runtime read coverage anchor). Doctor stores the reference only; the
    /// owning authority remains the single source of the record.
    DoctorEvidenceReferenceV1 => ("doctor evidence reference", 1024),
);

/// A typed reference to one piece of evidence Doctor composed into a finding.
///
/// The `family` records which audited input surface produced the evidence, so
/// a finding may cross-cite evidence from more than one family without losing
/// provenance.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct DoctorEvidenceRefV1 {
    family: DoctorFindingFamilyV1,
    reference: DoctorEvidenceReferenceV1,
}

impl DoctorEvidenceRefV1 {
    /// Construct an evidence reference. The identity is already validated by
    /// [`DoctorEvidenceReferenceV1::new`], so this constructor is infallible.
    #[must_use]
    pub fn new(family: DoctorFindingFamilyV1, reference: DoctorEvidenceReferenceV1) -> Self {
        Self { family, reference }
    }

    #[must_use]
    pub fn family(&self) -> DoctorFindingFamilyV1 {
        self.family
    }

    #[must_use]
    pub fn reference(&self) -> &DoctorEvidenceReferenceV1 {
        &self.reference
    }
}

/// A bounded, human-readable coverage statement plus its completeness.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorCoverageStatementV1 {
    completeness: DoctorCoverageCompletenessV1,
    statement: String,
}

impl<'de> Deserialize<'de> for DoctorCoverageStatementV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            completeness: DoctorCoverageCompletenessV1,
            statement: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.completeness, wire.statement).map_err(serde::de::Error::custom)
    }
}

impl DoctorCoverageStatementV1 {
    /// Validate and construct a coverage statement. The statement text must be
    /// non-empty, trimmed, bounded, and free of control characters.
    pub fn new(
        completeness: DoctorCoverageCompletenessV1,
        statement: impl Into<String>,
    ) -> Result<Self, ApplicationContractError> {
        let statement = statement.into();
        if statement.is_empty()
            || statement.trim() != statement
            || statement.len() > 512
            || statement.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "doctor coverage statement",
            });
        }
        Ok(Self {
            completeness,
            statement,
        })
    }

    #[must_use]
    pub fn completeness(&self) -> DoctorCoverageCompletenessV1 {
        self.completeness
    }

    #[must_use]
    pub fn statement(&self) -> &str {
        &self.statement
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.completeness.is_complete()
    }
}

/// One canonical Doctor finding.
///
/// A finding pins its diagnosis `family`, its evidence `state`, the typed
/// evidence it composed, and a coverage statement. Construction enforces the
/// invariants that keep unknown/partial evidence from collapsing into a
/// healthy or clean result.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorFindingV1 {
    family: DoctorFindingFamilyV1,
    state: DoctorEvidenceStateV1,
    evidence: Vec<DoctorEvidenceRefV1>,
    coverage: DoctorCoverageStatementV1,
}

impl DoctorFindingV1 {
    /// Validate and construct a Doctor finding.
    ///
    /// Invariants:
    /// 1. Every finding cites at least one typed evidence reference.
    /// 2. Evidence references are unique (no duplicates).
    /// 3. A [`DoctorEvidenceStateV1::HealthyCompleteCoverage`] finding requires
    ///    [`DoctorCoverageCompletenessV1::Complete`] coverage — partial or
    ///    unknown coverage never collapses into a healthy claim.
    pub fn new(
        family: DoctorFindingFamilyV1,
        state: DoctorEvidenceStateV1,
        evidence: Vec<DoctorEvidenceRefV1>,
        coverage: DoctorCoverageStatementV1,
    ) -> Result<Self, ApplicationContractError> {
        if evidence.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "doctor finding evidence",
            });
        }
        if evidence.iter().enumerate().any(|(index, current)| {
            evidence[index.saturating_add(1)..]
                .iter()
                .any(|other| other == current)
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "doctor finding evidence",
            });
        }
        if state.is_healthy_complete() && !coverage.is_complete() {
            return Err(ApplicationContractError::Inconsistent {
                field: "doctor healthy coverage",
            });
        }
        Ok(Self {
            family,
            state,
            evidence,
            coverage,
        })
    }

    #[must_use]
    pub fn family(&self) -> DoctorFindingFamilyV1 {
        self.family
    }

    #[must_use]
    pub fn state(&self) -> DoctorEvidenceStateV1 {
        self.state
    }

    #[must_use]
    pub fn evidence(&self) -> &[DoctorEvidenceRefV1] {
        &self.evidence
    }

    #[must_use]
    pub fn coverage(&self) -> &DoctorCoverageStatementV1 {
        &self.coverage
    }
}

impl<'de> Deserialize<'de> for DoctorFindingV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            family: DoctorFindingFamilyV1,
            state: DoctorEvidenceStateV1,
            evidence: Vec<DoctorEvidenceRefV1>,
            coverage: DoctorCoverageStatementV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.family, wire.state, wire.evidence, wire.coverage)
            .map_err(serde::de::Error::custom)
    }
}

/// A [`DoctorFindingFamilyV1::Storage`] finding paired with its typed subclass.
///
/// The storage subclass ([`DoctorStorageFindingKindV1`]) must be *attached* to
/// the finding it classifies, not smuggled into an
/// evidence-reference string that a consumer has to parse back out. This wrapper
/// is the typed carrier. Its constructor enforces that the wrapped finding is the
/// `Storage` family, so a non-Storage finding can never be mislabeled with a
/// storage subclass, and the kind is recovered by value rather than by string
/// prefix. The kernel owns the wrapper; storage producers emit it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorStorageFindingV1 {
    kind: DoctorStorageFindingKindV1,
    finding: DoctorFindingV1,
}

impl DoctorStorageFindingV1 {
    /// Validate and construct a typed storage finding.
    ///
    /// Invariant: the wrapped finding must be the [`DoctorFindingFamilyV1::Storage`]
    /// family. Pairing a storage subclass with any other family is a contract
    /// error, not a silently accepted mislabel.
    pub fn new(
        kind: DoctorStorageFindingKindV1,
        finding: DoctorFindingV1,
    ) -> Result<Self, ApplicationContractError> {
        if finding.family() != DoctorFindingFamilyV1::Storage {
            return Err(ApplicationContractError::Inconsistent {
                field: "doctor storage finding family",
            });
        }
        Ok(Self { kind, finding })
    }

    /// The typed subclass this finding belongs to.
    #[must_use]
    pub fn kind(&self) -> DoctorStorageFindingKindV1 {
        self.kind
    }

    /// The underlying canonical finding.
    #[must_use]
    pub fn finding(&self) -> &DoctorFindingV1 {
        &self.finding
    }

    /// Consume the wrapper, yielding the canonical finding.
    #[must_use]
    pub fn into_finding(self) -> DoctorFindingV1 {
        self.finding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence_reference(value: &str) -> DoctorEvidenceReferenceV1 {
        DoctorEvidenceReferenceV1::new(value).expect("valid evidence reference")
    }

    fn evidence(family: DoctorFindingFamilyV1, value: &str) -> DoctorEvidenceRefV1 {
        DoctorEvidenceRefV1::new(family, evidence_reference(value))
    }

    fn complete_coverage() -> DoctorCoverageStatementV1 {
        DoctorCoverageStatementV1::new(
            DoctorCoverageCompletenessV1::Complete,
            "all sources observed",
        )
        .expect("valid coverage")
    }

    fn partial_coverage() -> DoctorCoverageStatementV1 {
        DoctorCoverageStatementV1::new(DoctorCoverageCompletenessV1::Partial, "one source omitted")
            .expect("valid coverage")
    }

    #[test]
    fn doctor_healthy_finding_with_complete_coverage_constructs() {
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::StorageRuntime,
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            vec![evidence(
                DoctorFindingFamilyV1::StorageRuntime,
                "runtime.graph-quick-check",
            )],
            complete_coverage(),
        )
        .expect("healthy finding");
        assert!(finding.state().is_healthy_complete());
        assert_eq!(finding.evidence().len(), 1);
        assert!(finding.coverage().is_complete());
    }

    #[test]
    fn doctor_finding_wire_contract_contains_diagnostics_only() {
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Configuration,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence(
                DoctorFindingFamilyV1::Configuration,
                "config.revision.42",
            )],
            complete_coverage(),
        )
        .expect("diagnostic finding");

        let wire = serde_json::to_value(finding).expect("serialize finding");
        assert!(
            wire.get("remediation").is_none(),
            "Doctor findings must not expose action references: {wire}"
        );
    }

    #[test]
    fn doctor_finding_requires_at_least_one_evidence_reference() {
        let error = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Observability,
            DoctorEvidenceStateV1::Unknown,
            Vec::new(),
            partial_coverage(),
        )
        .expect_err("empty evidence rejected");
        assert_eq!(
            error,
            ApplicationContractError::Inconsistent {
                field: "doctor finding evidence"
            }
        );
    }

    #[test]
    fn doctor_finding_rejects_duplicate_evidence_references() {
        let error = DoctorFindingV1::new(
            DoctorFindingFamilyV1::SemanticIndex,
            DoctorEvidenceStateV1::Stale,
            vec![
                evidence(
                    DoctorFindingFamilyV1::SemanticIndex,
                    "semantic.generation.7",
                ),
                evidence(
                    DoctorFindingFamilyV1::SemanticIndex,
                    "semantic.generation.7",
                ),
            ],
            partial_coverage(),
        )
        .expect_err("duplicate evidence rejected");
        assert_eq!(
            error,
            ApplicationContractError::Duplicate {
                field: "doctor finding evidence"
            }
        );
    }

    #[test]
    fn doctor_healthy_finding_rejects_partial_coverage() {
        let error = DoctorFindingV1::new(
            DoctorFindingFamilyV1::LanguageServer,
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            vec![evidence(
                DoctorFindingFamilyV1::LanguageServer,
                "lsp.analyzer.ready",
            )],
            partial_coverage(),
        )
        .expect_err("partial coverage cannot be healthy");
        assert_eq!(
            error,
            ApplicationContractError::Inconsistent {
                field: "doctor healthy coverage"
            }
        );
    }

    #[test]
    fn doctor_healthy_finding_rejects_unknown_coverage() {
        let coverage = DoctorCoverageStatementV1::new(
            DoctorCoverageCompletenessV1::Unknown,
            "coverage unknown",
        )
        .expect("valid coverage");
        let error = DoctorFindingV1::new(
            DoctorFindingFamilyV1::StorageRuntime,
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            vec![evidence(
                DoctorFindingFamilyV1::StorageRuntime,
                "runtime.temporal-health",
            )],
            coverage,
        )
        .expect_err("unknown coverage cannot be healthy");
        assert_eq!(
            error,
            ApplicationContractError::Inconsistent {
                field: "doctor healthy coverage"
            }
        );
    }

    #[test]
    fn doctor_evidence_reference_rejects_empty_trimmed_and_control_input() {
        assert_eq!(
            DoctorEvidenceReferenceV1::new("").expect_err("empty rejected"),
            ApplicationContractError::InvalidIdentifier {
                field: "doctor evidence reference"
            }
        );
        assert_eq!(
            DoctorEvidenceReferenceV1::new(" leading").expect_err("untrimmed rejected"),
            ApplicationContractError::InvalidIdentifier {
                field: "doctor evidence reference"
            }
        );
        assert_eq!(
            DoctorEvidenceReferenceV1::new("ctrl\u{0}char").expect_err("control rejected"),
            ApplicationContractError::InvalidIdentifier {
                field: "doctor evidence reference"
            }
        );
    }

    #[test]
    fn doctor_coverage_statement_rejects_empty_text() {
        assert_eq!(
            DoctorCoverageStatementV1::new(DoctorCoverageCompletenessV1::Complete, "")
                .expect_err("empty statement rejected"),
            ApplicationContractError::InvalidIdentifier {
                field: "doctor coverage statement"
            }
        );
    }

    #[test]
    fn doctor_evidence_state_is_healthy_complete_only_for_one_variant() {
        for state in [
            DoctorEvidenceStateV1::Unsupported,
            DoctorEvidenceStateV1::Absent,
            DoctorEvidenceStateV1::Stale,
            DoctorEvidenceStateV1::Degraded,
            DoctorEvidenceStateV1::Partial,
            DoctorEvidenceStateV1::Unknown,
            DoctorEvidenceStateV1::Denied,
        ] {
            assert!(
                !state.is_healthy_complete(),
                "{state:?} must not be healthy"
            );
        }
        assert!(DoctorEvidenceStateV1::HealthyCompleteCoverage.is_healthy_complete());
    }

    #[test]
    fn doctor_storage_family_finding_constructs() {
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence(
                DoctorFindingFamilyV1::Storage,
                "storage.orphan-store.age-42d",
            )],
            complete_coverage(),
        )
        .expect("storage finding");
        assert_eq!(finding.family(), DoctorFindingFamilyV1::Storage);
        assert!(!finding.state().is_healthy_complete());
    }

    #[test]
    fn doctor_storage_family_healthy_finding_still_rejects_partial_coverage() {
        let error = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            vec![evidence(
                DoctorFindingFamilyV1::Storage,
                "storage.size.within-budget",
            )],
            partial_coverage(),
        )
        .expect_err("partial coverage cannot be healthy");
        assert_eq!(
            error,
            ApplicationContractError::Inconsistent {
                field: "doctor healthy coverage"
            }
        );
    }

    #[test]
    fn doctor_storage_finding_wrapper_attaches_kind_and_requires_storage_family() {
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence(
                DoctorFindingFamilyV1::Storage,
                "storage.orphan-store.age-42d",
            )],
            complete_coverage(),
        )
        .expect("storage finding");
        let typed =
            DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, finding.clone())
                .expect("typed storage finding");
        assert_eq!(typed.kind(), DoctorStorageFindingKindV1::OrphanStore);
        assert_eq!(typed.finding(), &finding);
        assert_eq!(typed.into_finding(), finding);
    }

    #[test]
    fn doctor_storage_finding_wrapper_rejects_non_storage_family() {
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::StorageRuntime,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence(
                DoctorFindingFamilyV1::StorageRuntime,
                "runtime.reader-lease",
            )],
            complete_coverage(),
        )
        .expect("runtime finding");
        assert_eq!(
            DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OverBudgetStore, finding)
                .expect_err("non-storage family rejected"),
            ApplicationContractError::Inconsistent {
                field: "doctor storage finding family"
            }
        );
    }

    #[test]
    fn doctor_storage_finding_kinds_serialize_to_stable_snake_case() {
        for (kind, expected) in [
            (
                DoctorStorageFindingKindV1::OverBudgetStore,
                "over_budget_store",
            ),
            (DoctorStorageFindingKindV1::OrphanStore, "orphan_store"),
            (
                DoctorStorageFindingKindV1::IncidentDebrisPresent,
                "incident_debris_present",
            ),
            (
                DoctorStorageFindingKindV1::RetentionBacklog,
                "retention_backlog",
            ),
            (DoctorStorageFindingKindV1::TableGrowth, "table_growth"),
        ] {
            let encoded = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(encoded, format!("\"{expected}\""), "{kind:?}");
            let decoded: DoctorStorageFindingKindV1 =
                serde_json::from_str(&encoded).expect("deserialize");
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn doctor_storage_finding_kinds_are_distinct_from_storage_runtime_family() {
        assert_ne!(
            DoctorFindingFamilyV1::Storage,
            DoctorFindingFamilyV1::StorageRuntime
        );
        assert_eq!(
            serde_json::to_string(&DoctorFindingFamilyV1::Storage).expect("serialize"),
            "\"storage\""
        );
    }

    #[test]
    fn doctor_enums_serialize_to_stable_snake_case() {
        assert_eq!(
            serde_json::to_string(&DoctorFindingFamilyV1::StorageRuntime).expect("serialize"),
            "\"storage_runtime\""
        );
        assert_eq!(
            serde_json::to_string(&DoctorEvidenceStateV1::HealthyCompleteCoverage)
                .expect("serialize"),
            "\"healthy_complete_coverage\""
        );
        let decoded: DoctorEvidenceStateV1 =
            serde_json::from_str("\"denied\"").expect("deserialize");
        assert_eq!(decoded, DoctorEvidenceStateV1::Denied);
    }
}
