//! `GET /api/storage/findings` — compatibility projection of the canonical
//! Doctor storage family.
//!
//! The admitted daemon Doctor reader owns finding production and health
//! composition. This route selects the storage family from that report and
//! separately reads the dashboard's existing storage-telemetry authority to
//! describe whether owner budgets were evaluated, unset, or undetermined. It
//! never invokes a finding producer or derives a health verdict.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
    DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorStorageFindingKindV1,
};

use super::DashboardState;
use super::doctor_findings_api::DoctorFindingsPayloadV1;
use super::read_model::DashboardEnvelopeV1;
use super::storage_telemetry_api::{
    BUDGET_SETTING_KEY, StoreBudgetSourceSummaryV1, budget_source_summary,
};

const STORAGE_KINDS: [DoctorStorageFindingKindV1; 6] = [
    DoctorStorageFindingKindV1::OverBudgetStore,
    DoctorStorageFindingKindV1::OrphanStore,
    DoctorStorageFindingKindV1::StaleBranchDbs,
    DoctorStorageFindingKindV1::IncidentDebrisPresent,
    DoctorStorageFindingKindV1::RetentionBacklog,
    DoctorStorageFindingKindV1::TableGrowth,
];

/// Whether one Plan 38 producer had enough source evidence to report a real
/// result. This is source coverage, not a health grade: `Real` can describe a
/// clean observation or a problem finding.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageFindingSourceStateV1 {
    Real,
    Unset,
    Partial,
    Unsupported,
}

/// Source-coverage status for one typed storage finding producer.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct StorageFindingKindStatusV1 {
    pub kind: DoctorStorageFindingKindV1,
    pub state: StorageFindingSourceStateV1,
    pub observed_entries: usize,
    pub reason: String,
}

/// Route-specific payload for `/api/storage/findings`.
///
/// Storage producer coverage is required here rather than an optional field on
/// the general Doctor payload, so generated consumers cannot mistake one route
/// for the other.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct StorageFindingsPayloadV1 {
    #[serde(flatten)]
    pub findings: DoctorFindingsPayloadV1,
    pub kind_statuses: Vec<StorageFindingKindStatusV1>,
}

/// `GET /api/storage/findings`
pub async fn findings(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<StorageFindingsPayloadV1>> {
    let budget = budget_source_summary(&state).await;
    let envelope = super::doctor_findings_api::findings_for_family(
        state,
        Some(DoctorFindingFamilyV1::Storage),
    )
    .await;
    let kind_statuses = storage_kind_statuses(&envelope.payload, budget);
    Json(envelope.map_payload(|findings| StorageFindingsPayloadV1 {
        findings,
        kind_statuses,
    }))
}

fn storage_kind_statuses(
    payload: &DoctorFindingsPayloadV1,
    budget: StoreBudgetSourceSummaryV1,
) -> Vec<StorageFindingKindStatusV1> {
    STORAGE_KINDS
        .into_iter()
        .map(|kind| {
            if kind == DoctorStorageFindingKindV1::OverBudgetStore {
                budget_kind_status(budget)
            } else {
                canonical_kind_status(payload, kind)
            }
        })
        .collect()
}

fn budget_kind_status(summary: StoreBudgetSourceSummaryV1) -> StorageFindingKindStatusV1 {
    let (state, reason) = if summary.stores == 0 {
        (
            StorageFindingSourceStateV1::Unsupported,
            "no dashboard-held stores were available for budget evaluation".to_string(),
        )
    } else if summary.unknown > 0 || (summary.evaluated > 0 && summary.unset > 0) {
        (
            StorageFindingSourceStateV1::Partial,
            format!(
                "{} stores checked: {} budgets evaluated ({} over), {} unset, {} undetermined; unset or undetermined inputs cannot prove a clean result",
                summary.stores,
                summary.evaluated,
                summary.over_budget,
                summary.unset,
                summary.unknown,
            ),
        )
    } else if summary.evaluated > 0 {
        (
            StorageFindingSourceStateV1::Real,
            format!(
                "{} owner-configured budgets evaluated from {BUDGET_SETTING_KEY}; {} over budget",
                summary.evaluated, summary.over_budget
            ),
        )
    } else if summary.unset == summary.stores {
        (
            StorageFindingSourceStateV1::Unset,
            format!("No owner budget configured · {BUDGET_SETTING_KEY}"),
        )
    } else {
        (
            StorageFindingSourceStateV1::Partial,
            format!(
                "{} stores checked but budget coverage was incomplete; no clean result is asserted",
                summary.stores
            ),
        )
    };
    StorageFindingKindStatusV1 {
        kind: DoctorStorageFindingKindV1::OverBudgetStore,
        state,
        observed_entries: summary.evaluated,
        reason,
    }
}

fn canonical_kind_status(
    payload: &DoctorFindingsPayloadV1,
    kind: DoctorStorageFindingKindV1,
) -> StorageFindingKindStatusV1 {
    let matching = payload
        .entries
        .iter()
        .filter(|entry| entry.storage_kind() == Some(kind))
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        let complete_observations = matching.iter().all(|entry| {
            entry.finding().coverage().completeness() == DoctorCoverageCompletenessV1::Complete
                && matches!(
                    entry.finding().state(),
                    DoctorEvidenceStateV1::Stale
                        | DoctorEvidenceStateV1::Degraded
                        | DoctorEvidenceStateV1::HealthyCompleteCoverage
                )
        });
        let state = if complete_observations {
            StorageFindingSourceStateV1::Real
        } else {
            StorageFindingSourceStateV1::Partial
        };
        let reason = if complete_observations {
            format!(
                "canonical Doctor producer returned {} observed {}",
                matching.len(),
                if matching.len() == 1 {
                    "entry with complete coverage"
                } else {
                    "entries with complete coverage"
                }
            )
        } else {
            format!(
                "canonical Doctor producer returned {} entries, but coverage or evidence state was incomplete",
                matching.len()
            )
        };
        return StorageFindingKindStatusV1 {
            kind,
            state,
            observed_entries: matching.len(),
            reason,
        };
    }

    let consultation = payload.report_coverage.as_ref().and_then(|coverage| {
        coverage
            .families()
            .iter()
            .find(|family| family.family() == DoctorFindingFamilyV1::Storage)
            .map(tracedecay_application::DoctorFamilyCoverageV1::consultation)
    });
    let (state, reason) = match consultation {
        Some(DoctorFamilyConsultationV1::Consulted) => (
            StorageFindingSourceStateV1::Partial,
            "the storage family was consulted, but the canonical report returned no typed entry for this producer; absence does not prove clean per-producer coverage"
                .to_string(),
        ),
        Some(DoctorFamilyConsultationV1::Unavailable {
            reason:
                reason @ (DoctorFamilyUnavailableReasonV1::Unwired
                | DoctorFamilyUnavailableReasonV1::Unsupported),
        }) => (
            StorageFindingSourceStateV1::Unsupported,
            format!(
                "canonical Doctor storage source is unavailable ({})",
                unavailable_reason(reason)
            ),
        ),
        Some(DoctorFamilyConsultationV1::Unavailable { reason }) => (
            StorageFindingSourceStateV1::Partial,
            format!(
                "canonical Doctor storage source is unavailable ({}); no clean result is asserted",
                unavailable_reason(reason)
            ),
        ),
        None => (
            StorageFindingSourceStateV1::Unsupported,
            format!(
                "canonical Doctor storage source supplied no consultation record: {}",
                payload.note
            ),
        ),
    };
    StorageFindingKindStatusV1 {
        kind,
        state,
        observed_entries: 0,
        reason,
    }
}

const fn unavailable_reason(reason: DoctorFamilyUnavailableReasonV1) -> &'static str {
    match reason {
        DoctorFamilyUnavailableReasonV1::Unwired => "unwired",
        DoctorFamilyUnavailableReasonV1::Unsupported => "unsupported",
        DoctorFamilyUnavailableReasonV1::Absent => "absent",
        DoctorFamilyUnavailableReasonV1::Denied => "denied",
        DoctorFamilyUnavailableReasonV1::Unknown => "unknown",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn budget_source_reports_configured_over_clean_unset_and_partial_honestly() {
        let unset = budget_kind_status(StoreBudgetSourceSummaryV1 {
            stores: 2,
            unset: 2,
            ..StoreBudgetSourceSummaryV1::default()
        });
        assert_eq!(unset.state, StorageFindingSourceStateV1::Unset);
        assert!(unset.reason.contains(BUDGET_SETTING_KEY));

        let undetermined = budget_kind_status(StoreBudgetSourceSummaryV1 {
            stores: 2,
            evaluated: 1,
            unknown: 1,
            ..StoreBudgetSourceSummaryV1::default()
        });
        assert_eq!(undetermined.state, StorageFindingSourceStateV1::Partial);
        assert!(undetermined.reason.contains("cannot prove a clean result"));

        let configured_over = budget_kind_status(StoreBudgetSourceSummaryV1 {
            stores: 2,
            evaluated: 2,
            over_budget: 1,
            ..StoreBudgetSourceSummaryV1::default()
        });
        assert_eq!(configured_over.state, StorageFindingSourceStateV1::Real);
        assert_eq!(configured_over.observed_entries, 2);
        assert!(configured_over.reason.contains("1 over budget"));

        let configured_clean = budget_kind_status(StoreBudgetSourceSummaryV1 {
            stores: 2,
            evaluated: 2,
            ..StoreBudgetSourceSummaryV1::default()
        });
        assert_eq!(configured_clean.state, StorageFindingSourceStateV1::Real);
        assert_eq!(configured_clean.observed_entries, 2);
        assert!(configured_clean.reason.contains("0 over budget"));
    }

    #[tokio::test]
    async fn route_without_admitted_reader_is_typed_unsupported() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state) =
            crate::events_api::dashboard_state_fixture("project.dashboard-storage-findings").await;

        let Json(envelope) = findings(State(state)).await;

        assert_eq!(
            envelope.payload.findings.family_filter,
            Some(DoctorFindingFamilyV1::Storage)
        );
        assert_eq!(
            envelope.domain_state,
            super::super::read_model::DashboardDomainStateV1::Unsupported
        );
        assert!(envelope.payload.findings.entries.is_empty());
        let statuses = &envelope.payload.kind_statuses;
        assert_eq!(statuses.len(), STORAGE_KINDS.len());
        assert_eq!(
            statuses[0].state,
            StorageFindingSourceStateV1::Unset,
            "fresh owner config has no soft budgets"
        );
        assert!(
            statuses[1..]
                .iter()
                .all(|status| status.state == StorageFindingSourceStateV1::Unsupported)
        );
    }
}
