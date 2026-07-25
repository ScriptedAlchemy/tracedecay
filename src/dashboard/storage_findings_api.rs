//! `GET /api/storage/findings` — compatibility projection of the canonical
//! Doctor storage family.
//!
//! The admitted daemon Doctor reader owns source observation and health
//! composition. This route only selects the storage family from that report;
//! it never re-reads stores or invokes finding producers itself.

use axum::Json;
use axum::extract::State;
use tracedecay_application::doctor::DoctorFindingFamilyV1;

use super::DashboardState;
use super::doctor_findings_api::DoctorFindingsPayloadV1;
use super::read_model::DashboardEnvelopeV1;

/// `GET /api/storage/findings`
pub(crate) async fn findings(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<DoctorFindingsPayloadV1>> {
    Json(
        super::doctor_findings_api::findings_for_family(
            state,
            Some(DoctorFindingFamilyV1::Storage),
        )
        .await,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tracedecay::TraceDecay;

    #[tokio::test]
    async fn route_without_admitted_reader_is_typed_unsupported() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");

        let Json(envelope) = findings(State(state)).await;

        assert_eq!(
            envelope.payload.family_filter,
            Some(DoctorFindingFamilyV1::Storage)
        );
        assert_eq!(
            envelope.domain_state,
            super::super::read_model::DashboardDomainStateV1::Unsupported
        );
        assert!(envelope.payload.entries.is_empty());
    }
}
