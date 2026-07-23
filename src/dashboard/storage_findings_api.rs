//! `GET /api/storage/findings` — the HTTP surface for the five plan-38 §7
//! storage Doctor findings (`DoctorStorageFindingKindV1`).
//!
//! The five pure producers (`over_budget_finding`, `orphan_store_finding`,
//! `stale_branch_dbs_finding`, `incident_debris_finding`,
//! `retention_backlog_finding`) are landed, but each consumes an input the
//! daemon does not yet expose as a consumable read source:
//! - `over_budget_store` needs an owner-configured `StoreSizeBudgetV1`;
//! - `orphan_store` needs an `OrphanStoreRecordV1` identity-resolution inventory;
//! - `stale_branch_dbs` needs a `StaleBranchDbRecordV1` git-ref-liveness record
//!   (owned by the retention wiring in `src/daemon/git_watch`, not yet exposed);
//! - `incident_debris_present` needs an `IncidentDebrisScanV1` quarantine scan;
//! - `retention_backlog` needs a `RetentionBacklogRecordV1`.
//!
//! None of those inputs is wired as a source the dashboard can consume today, so
//! every kind is rendered typed `unsupported` naming its required source (plan
//! §"Known backend gaps"), never a fabricated clean finding. When a producer's
//! input source is wired daemon-side, its per-kind status flips to a real
//! producer result here without changing the envelope shape.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use tracedecay_application::doctor::{DoctorEvidenceStateV1, DoctorStorageFindingKindV1};

use super::DashboardState;
use super::read_model::{
    DashboardDomainStateV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, scope_from_state,
};

/// Per-kind status for one storage finding subclass.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageFindingKindStatusV1 {
    pub kind: DoctorStorageFindingKindV1,
    /// The honest evidence state for the kind. `unsupported` while the producer
    /// input source is unwired.
    pub state: DoctorEvidenceStateV1,
    /// The typed input record the producer requires before it can emit a real
    /// finding.
    pub required_source: String,
    /// Human-readable reason the kind is currently unsupported.
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageFindingsPayloadV1 {
    pub kinds: Vec<StorageFindingKindStatusV1>,
    pub note: String,
}

const NOTE: &str =
    "the five plan-38 storage finding producers are landed, but their input read \
     sources are not yet wired daemon-side; each kind is typed unsupported until \
     its source is available";

fn kind_status(
    kind: DoctorStorageFindingKindV1,
    required_source: &str,
    reason: &str,
) -> StorageFindingKindStatusV1 {
    StorageFindingKindStatusV1 {
        kind,
        state: DoctorEvidenceStateV1::Unsupported,
        required_source: required_source.to_string(),
        reason: reason.to_string(),
    }
}

/// `GET /api/storage/findings`
pub(crate) async fn findings(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<StorageFindingsPayloadV1>> {
    let kinds = vec![
        kind_status(
            DoctorStorageFindingKindV1::OverBudgetStore,
            "StoreSizeBudgetV1",
            "no owner-configured soft size budget source is wired; a budget is \
             required to evaluate an over-budget store",
        ),
        kind_status(
            DoctorStorageFindingKindV1::OrphanStore,
            "OrphanStoreRecordV1",
            "no store identity-resolution inventory is wired to detect orphan stores",
        ),
        kind_status(
            DoctorStorageFindingKindV1::StaleBranchDbs,
            "StaleBranchDbRecordV1",
            "the git-ref-liveness inventory owned by src/daemon/git_watch retention \
             is not yet exposed as a consumable read source",
        ),
        kind_status(
            DoctorStorageFindingKindV1::IncidentDebrisPresent,
            "IncidentDebrisScanV1",
            "no quarantine debris scan is wired as a consumable read source",
        ),
        kind_status(
            DoctorStorageFindingKindV1::RetentionBacklog,
            "RetentionBacklogRecordV1",
            "no retention-backlog inventory is wired as a consumable read source",
        ),
    ];

    let payload = StorageFindingsPayloadV1 {
        kinds,
        note: NOTE.to_string(),
    };

    // No producer input source is wired, so the whole read model is unsupported.
    let mut envelope = DashboardEnvelopeV1::unsupported(scope_from_state(&state), payload);
    envelope = envelope.with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        "use-case.dashboard.storage.findings.refresh",
    )]);
    debug_assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
    Json(envelope)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tracedecay::TraceDecay;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path()).await.expect("project init");
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, state)
    }

    #[tokio::test]
    async fn all_five_kinds_are_typed_unsupported_with_named_sources() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = findings(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert_eq!(envelope.payload.kinds.len(), 5);

        for status in &envelope.payload.kinds {
            assert_eq!(status.state, DoctorEvidenceStateV1::Unsupported);
            assert!(
                !status.required_source.is_empty(),
                "kind {:?} must name its required source",
                status.kind
            );
        }

        // Each of the five closed subclasses is represented exactly once.
        let kinds: Vec<_> = envelope.payload.kinds.iter().map(|s| s.kind).collect();
        for expected in [
            DoctorStorageFindingKindV1::OverBudgetStore,
            DoctorStorageFindingKindV1::OrphanStore,
            DoctorStorageFindingKindV1::StaleBranchDbs,
            DoctorStorageFindingKindV1::IncidentDebrisPresent,
            DoctorStorageFindingKindV1::RetentionBacklog,
        ] {
            assert!(kinds.contains(&expected), "missing kind {expected:?}");
        }
    }
}
