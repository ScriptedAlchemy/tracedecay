//! `GET /api/doctor/findings` — the HTTP surface binding for the Doctor finding
//! family (plan 09 §PR14 / plan 11 §"Typed presentation contracts").
//!
//! The Doctor kernel contract types (`DoctorFindingV1`, `DoctorStorageFindingV1`,
//! `DoctorEvidenceStateV1`, coverage statements, remediation references) are
//! landed and stable, but the one composing use case that would *produce* live
//! findings from the advisory/configuration/storage-runtime/language-server/
//! semantic-index/observability authorities has not landed yet — the module doc
//! on `tracedecay_application::doctor` states it is "intentionally not wired into
//! any surface binding yet".
//!
//! This route is that binding. Because no live finding producer is wired, the
//! honest envelope is `unsupported` (plan §"Known backend gaps"): it exhaustively
//! models the contract — echoing the validated per-family filter and the full
//! finding-family vocabulary — and returns an empty findings list that is typed
//! unsupported, never a fabricated healthy/clean result and never a default
//! `complete_zero_findings`.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{DoctorFindingFamilyV1, DoctorFindingV1};

use super::DashboardState;
use super::read_model::{
    DashboardDomainStateV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, scope_from_state,
};

#[derive(Debug, Deserialize)]
pub(crate) struct FindingsParams {
    /// Optional per-family filter (`advisory`, `configuration`,
    /// `storage_runtime`, `storage`, `language_server`, `semantic_index`,
    /// `observability`).
    #[serde(default)]
    family: Option<String>,
}

/// The doctor-findings payload. `findings` is empty and typed-unsupported until
/// the composing use case lands; `known_families` is the full stable vocabulary
/// so the frontend can render the per-family selector without inventing labels.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorFindingsPayloadV1 {
    pub family_filter: Option<DoctorFindingFamilyV1>,
    pub findings: Vec<DoctorFindingV1>,
    pub known_families: Vec<DoctorFindingFamilyV1>,
    pub note: String,
}

const KNOWN_FAMILIES: [DoctorFindingFamilyV1; 7] = [
    DoctorFindingFamilyV1::Advisory,
    DoctorFindingFamilyV1::Configuration,
    DoctorFindingFamilyV1::StorageRuntime,
    DoctorFindingFamilyV1::Storage,
    DoctorFindingFamilyV1::LanguageServer,
    DoctorFindingFamilyV1::SemanticIndex,
    DoctorFindingFamilyV1::Observability,
];

const UNSUPPORTED_NOTE: &str =
    "the Doctor composing use case that produces live findings has not landed yet; \
     the finding contract is bound but no producer source is wired server-side";

/// `GET /api/doctor/findings`
pub(crate) async fn findings(
    State(state): State<DashboardState>,
    Query(params): Query<FindingsParams>,
) -> Json<DashboardEnvelopeV1<DoctorFindingsPayloadV1>> {
    let scope = scope_from_state(&state);

    // Validate the optional per-family filter against the closed vocabulary. An
    // unknown family is a typed `error` envelope, not a silent all-families read.
    let family_filter = match parse_family(params.family.as_deref()) {
        Ok(family) => family,
        Err(invalid) => {
            let payload = DoctorFindingsPayloadV1 {
                family_filter: None,
                findings: Vec::new(),
                known_families: KNOWN_FAMILIES.to_vec(),
                note: format!("unknown doctor finding family: {invalid}"),
            };
            let mut envelope = DashboardEnvelopeV1::unsupported(scope, payload);
            envelope.domain_state = DashboardDomainStateV1::Error;
            return Json(envelope);
        }
    };

    let payload = DoctorFindingsPayloadV1 {
        family_filter,
        findings: Vec::new(),
        known_families: KNOWN_FAMILIES.to_vec(),
        note: UNSUPPORTED_NOTE.to_string(),
    };

    let envelope = DashboardEnvelopeV1::unsupported(scope, payload).with_legal_actions(vec![
        DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "use-case.dashboard.doctor.findings.refresh",
        ),
    ]);
    Json(envelope)
}

/// Parse a `snake_case` family label against the closed vocabulary. `Ok(None)`
/// means no filter was supplied; `Err` carries the invalid label.
fn parse_family(family: Option<&str>) -> Result<Option<DoctorFindingFamilyV1>, String> {
    let Some(raw) = family else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let quoted = format!("\"{trimmed}\"");
    serde_json::from_str::<DoctorFindingFamilyV1>(&quoted)
        .map(Some)
        .map_err(|_| trimmed.to_string())
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

    #[test]
    fn family_filter_parses_closed_vocabulary_and_rejects_unknown() {
        assert_eq!(parse_family(None).unwrap(), None);
        assert_eq!(parse_family(Some("")).unwrap(), None);
        assert_eq!(
            parse_family(Some("storage")).unwrap(),
            Some(DoctorFindingFamilyV1::Storage)
        );
        assert_eq!(
            parse_family(Some("storage_runtime")).unwrap(),
            Some(DoctorFindingFamilyV1::StorageRuntime)
        );
        assert_eq!(parse_family(Some("nonsense")).unwrap_err(), "nonsense");
    }

    #[tokio::test]
    async fn findings_route_is_typed_unsupported_not_empty_or_healthy() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = findings(
            State(state),
            Query(FindingsParams { family: None }),
        )
        .await;

        assert_eq!(envelope.schema_revision, 1);
        // Absent producer -> unsupported, never complete_zero_findings/ready.
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.findings.is_empty());
        assert_eq!(envelope.payload.known_families.len(), 7);
        assert_eq!(envelope.payload.family_filter, None);
    }

    #[tokio::test]
    async fn findings_route_echoes_valid_family_filter() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = findings(
            State(state),
            Query(FindingsParams {
                family: Some("configuration".to_string()),
            }),
        )
        .await;
        assert_eq!(
            envelope.payload.family_filter,
            Some(DoctorFindingFamilyV1::Configuration)
        );
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
    }

    #[tokio::test]
    async fn findings_route_rejects_unknown_family_with_error_state() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = findings(
            State(state),
            Query(FindingsParams {
                family: Some("not_a_family".to_string()),
            }),
        )
        .await;
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Error);
        assert_eq!(envelope.payload.family_filter, None);
    }
}
