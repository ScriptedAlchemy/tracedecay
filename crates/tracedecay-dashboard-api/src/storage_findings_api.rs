//! `GET /api/storage/findings` — compatibility projection of the canonical
//! Doctor storage family.
//!
//! The admitted daemon Doctor reader owns finding production and health
//! composition. This route only selects the storage family and projects typed
//! producer status from that same report. It never invokes a finding producer,
//! consults a dashboard-held telemetry authority, or derives a health verdict.

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

const STORAGE_KINDS: [DoctorStorageFindingKindV1; 5] = [
    DoctorStorageFindingKindV1::OverBudgetStore,
    DoctorStorageFindingKindV1::OrphanStore,
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
    let envelope = super::doctor_findings_api::findings_for_family(
        state,
        Some(DoctorFindingFamilyV1::Storage),
    )
    .await;
    let kind_statuses = storage_kind_statuses(&envelope.payload);
    Json(envelope.map_payload(|findings| StorageFindingsPayloadV1 {
        findings,
        kind_statuses,
    }))
}

fn storage_kind_statuses(payload: &DoctorFindingsPayloadV1) -> Vec<StorageFindingKindStatusV1> {
    STORAGE_KINDS
        .into_iter()
        .map(|kind| canonical_kind_status(payload, kind))
        .collect()
}

fn canonical_kind_status(
    payload: &DoctorFindingsPayloadV1,
    kind: DoctorStorageFindingKindV1,
) -> StorageFindingKindStatusV1 {
    let consultation = payload.report_coverage.as_ref().and_then(|coverage| {
        coverage
            .families()
            .iter()
            .find(|family| family.family() == DoctorFindingFamilyV1::Storage)
            .map(tracedecay_application::DoctorFamilyCoverageV1::consultation)
    });
    let matching = payload
        .entries
        .iter()
        .filter(|entry| entry.storage_kind() == Some(kind))
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        let complete_observations = consultation == Some(DoctorFamilyConsultationV1::Consulted)
            && matching.iter().all(|entry| {
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
        } else if let Some(DoctorFamilyConsultationV1::Unavailable { reason }) = consultation {
            format!(
                "canonical Doctor producer returned {} observed entries, but storage family coverage is incomplete ({})",
                matching.len(),
                unavailable_reason(reason)
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
        DoctorFamilyUnavailableReasonV1::Unavailable => "unavailable",
        DoctorFamilyUnavailableReasonV1::ResetRequired => "reset_required",
        DoctorFamilyUnavailableReasonV1::Corrupt => "corrupt",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn route_without_admitted_reader_projects_all_kinds_as_unsupported() {
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
        assert!(
            statuses
                .iter()
                .all(|status| status.state == StorageFindingSourceStateV1::Unsupported),
            "dashboard-held telemetry must not override the canonical unavailable report"
        );
    }
}
