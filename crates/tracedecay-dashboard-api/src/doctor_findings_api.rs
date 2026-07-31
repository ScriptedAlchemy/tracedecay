//! `GET /api/doctor/findings` — root composition for the Doctor finding family
//! (plan 09 §PR14 / plan 11 §"Typed presentation contracts").
//!
//! An admitted daemon owner injects the canonical composed report reader into
//! [`DashboardState`]. This module only resolves scope, invokes that reader,
//! consults the owner remediation dispatcher, and attaches the dispatch targets
//! it can build from a finding. Every presentation decision — family
//! vocabulary, coverage, freshness, domain state, notes, and legal-action
//! references — belongs to [`tracedecay_api::doctor`]. A dashboard opened
//! without an admitted reader remains explicitly unsupported.

use axum::Json;
use axum::extract::{Query, State};
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_api::doctor::{
    DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE, DoctorFindingsQueryV1, DoctorReadPresentationV1,
    KNOWN_DOCTOR_FINDING_FAMILIES, doctor_report_failure_note, parse_doctor_finding_family,
    project_doctor_report,
};
use tracedecay_application::doctor::{
    DoctorFindingFamilyV1, DoctorRemediationDescriptorV1, DoctorReportCoverageV1,
    DoctorReportEntryV1,
};

use super::DashboardState;
use super::doctor_remediation_api::DoctorRemediationTargetV1;
use super::read_model::{
    DashboardDomainStateV1, DashboardEnvelopeV1, DashboardLegalActionKindV1, DashboardScopeV1,
    scope_from_state,
};

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DashboardDoctorRemediationDescriptorV1 {
    #[serde(flatten)]
    descriptor: DoctorRemediationDescriptorV1,
    /// The dispatch target this route can construct from the finding alone.
    /// `None` means the owning surface must supply the target (a protected
    /// configuration apply needs the concrete key, value, and base revision),
    /// not that the owner withholds the action — that is `legal_actions`.
    pub target: Option<DoctorRemediationTargetV1>,
}

impl DashboardDoctorRemediationDescriptorV1 {
    #[cfg(test)]
    fn operation(&self) -> &tracedecay_application::doctor::DoctorOwningOperationRefV1 {
        self.descriptor.operation()
    }
}

/// The canonical Doctor report projection. `entries` retains the storage
/// subclass attached by the kernel; remediation descriptors are emitted only
/// after the kernel registry resolves the finding's owner-supplied reference.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DoctorFindingsPayloadV1 {
    pub family_filter: Option<DoctorFindingFamilyV1>,
    pub entries: Vec<DoctorReportEntryV1>,
    pub report_coverage: Option<DoctorReportCoverageV1>,
    pub remediations: Vec<DashboardDoctorRemediationDescriptorV1>,
    pub known_families: Vec<DoctorFindingFamilyV1>,
    pub note: String,
}

/// `GET /api/doctor/findings`
pub async fn findings(
    State(state): State<DashboardState>,
    Query(params): Query<DoctorFindingsQueryV1>,
) -> Json<DashboardEnvelopeV1<DoctorFindingsPayloadV1>> {
    // Validate the optional per-family filter against the closed vocabulary. An
    // unknown family is a typed `error` envelope, not a silent all-families read.
    let family_filter = match parse_doctor_finding_family(params.family.as_deref()) {
        Ok(family) => family,
        Err(invalid) => {
            let scope = scope_from_state(&state);
            let payload =
                unavailable_payload(None, format!("unknown doctor finding family: {invalid}"));
            let mut envelope = DashboardEnvelopeV1::unsupported(scope, payload);
            envelope.domain_state = DashboardDomainStateV1::Error;
            return Json(envelope);
        }
    };
    Json(findings_for_family(state, family_filter).await)
}

/// Project the admitted canonical Doctor report for one closed finding family.
///
/// Compatibility routes such as `/api/storage/findings` call this seam instead
/// of evaluating health from dashboard-held database handles.
pub async fn findings_for_family(
    state: DashboardState,
    family_filter: Option<DoctorFindingFamilyV1>,
) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
    let scope = scope_from_state(&state);

    let Some(reader) = state.doctor_report_reader.as_ref() else {
        return envelope(
            scope,
            DoctorReadPresentationV1::source_unsupported(),
            unavailable_payload(family_filter, DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE),
        );
    };

    let report = match reader().await {
        Ok(admitted) => admitted.report,
        Err(error) => {
            return envelope(
                scope,
                DoctorReadPresentationV1::source_failed(),
                unavailable_payload(family_filter, doctor_report_failure_note(&error)),
            );
        }
    };

    let projection = match project_doctor_report(&report, family_filter) {
        Ok(projection) => projection,
        Err(rejection) => {
            return envelope(
                scope,
                DoctorReadPresentationV1::source_failed(),
                unavailable_payload(family_filter, rejection.note()),
            );
        }
    };

    let mut presentation = projection.presentation;
    if let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() {
        for reference in &projection.remediation_references {
            for kind in dispatcher.legal_actions(reference).await {
                let kind = match kind {
                    super::doctor_remediation_api::DoctorRemediationLegalActionV1::RequestPreview => {
                        DashboardLegalActionKindV1::RequestDryRun
                    }
                    super::doctor_remediation_api::DoctorRemediationLegalActionV1::RequestApply => {
                        DashboardLegalActionKindV1::RequestApply
                    }
                };
                presentation.merge_owner_legal_action(kind, reference);
            }
        }
    }

    envelope(
        scope,
        presentation,
        DoctorFindingsPayloadV1 {
            family_filter,
            entries: projection.entries,
            report_coverage: Some(projection.report_coverage),
            remediations: projection
                .remediations
                .into_iter()
                .map(|descriptor| DashboardDoctorRemediationDescriptorV1 {
                    target: DoctorRemediationTargetV1::for_operation(descriptor.operation()),
                    descriptor,
                })
                .collect(),
            known_families: KNOWN_DOCTOR_FINDING_FAMILIES.to_vec(),
            note: projection.note,
        },
    )
}

fn envelope(
    scope: DashboardScopeV1,
    presentation: DoctorReadPresentationV1,
    payload: DoctorFindingsPayloadV1,
) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
    DashboardEnvelopeV1::new(
        scope,
        presentation.domain_state,
        presentation.coverage,
        presentation.freshness,
        payload,
    )
    .with_legal_actions(presentation.legal_actions)
}

fn unavailable_payload(
    family_filter: Option<DoctorFindingFamilyV1>,
    note: impl Into<String>,
) -> DoctorFindingsPayloadV1 {
    DoctorFindingsPayloadV1 {
        family_filter,
        entries: Vec::new(),
        report_coverage: None,
        remediations: Vec::new(),
        known_families: KNOWN_DOCTOR_FINDING_FAMILIES.to_vec(),
        note: note.into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
    use tracedecay_api::doctor::doctor_findings_refresh_action;
    use tracedecay_application::doctor::{
        ConfigurationAuthorityReadV1, ConfigurationDriftV1, DoctorCoverageCompletenessV1,
    };
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::super::read_model::DashboardLegalActionRefV1;
    use crate::tracedecay::TraceDecay;

    async fn state_for_test() -> (
        tempfile::TempDir,
        Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
        DashboardState,
    ) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            project.path(),
            "project.dashboard-doctor-findings",
        )
        .await
        .expect("project init");
        let state = crate::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, runtime, state)
    }

    fn context() -> RequestContext {
        let actor = ActorId::new("actor.dashboard-doctor-test").unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.dashboard-doctor-test").unwrap(),
            RepositoryId::new("repository.dashboard-doctor-test").unwrap(),
            WorktreeId::new("worktree.dashboard-doctor-test").unwrap(),
            None,
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.dashboard-doctor-test").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "22".repeat(32))).unwrap(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.dashboard-doctor-test").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.dashboard-doctor-test").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            actor,
            scope,
            grant,
            RequestId::new("request.dashboard-doctor-test").unwrap(),
            Deadline::new(UtcMicros(9_000)).unwrap(),
            CancellationContext::active("cancel.dashboard-doctor-test").unwrap(),
        )
        .unwrap()
    }

    async fn state_with_inputs(
        inputs: crate::daemon::doctor_kernel::DoctorKernelInputsV1,
    ) -> (
        tempfile::TempDir,
        Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
        DashboardState,
    ) {
        let (project, runtime, mut state) = state_for_test().await;
        let report = crate::daemon::doctor_kernel::compose_doctor_report(&context(), &inputs)
            .await
            .unwrap();
        state.doctor_report_reader = Some(Arc::new(move || {
            let report = report.clone();
            Box::pin(async move { Ok(crate::AdmittedDoctorReportV1::new(report)) })
        }));
        (project, runtime, state)
    }

    #[tokio::test]
    async fn findings_route_is_typed_unsupported_not_empty_or_healthy() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, _runtime, state) = state_for_test().await;
        let Json(envelope) =
            findings(State(state), Query(DoctorFindingsQueryV1 { family: None })).await;

        assert_eq!(envelope.schema_revision, 1);
        // Absent producer -> unsupported, never complete_zero_findings/ready.
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.entries.is_empty());
        assert_eq!(envelope.payload.known_families.len(), 7);
        assert_eq!(envelope.payload.family_filter, None);
        assert_eq!(envelope.payload.note, DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE);
        assert_eq!(
            envelope.legal_actions,
            vec![doctor_findings_refresh_action()]
        );
    }

    #[tokio::test]
    async fn findings_route_echoes_valid_family_filter() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, _runtime, state) = state_for_test().await;
        let Json(envelope) = findings(
            State(state),
            Query(DoctorFindingsQueryV1 {
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
        let (_project, _runtime, state) = state_for_test().await;
        let Json(envelope) = findings(
            State(state),
            Query(DoctorFindingsQueryV1 {
                family: Some("not_a_family".to_string()),
            }),
        )
        .await;
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Error);
        assert_eq!(envelope.payload.family_filter, None);
        assert_eq!(
            envelope.payload.note,
            "unknown doctor finding family: not_a_family"
        );
    }

    #[tokio::test]
    async fn findings_route_preserves_canonical_unknown_entries() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, _runtime, state) =
            state_with_inputs(crate::daemon::doctor_kernel::DoctorKernelInputsV1::all_unknown())
                .await;

        let Json(envelope) =
            findings(State(state), Query(DoctorFindingsQueryV1 { family: None })).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Partial);
        // Advisory has both host-integration and feedback-owner findings.
        assert_eq!(envelope.payload.entries.len(), 8);
        assert!(envelope.payload.entries.iter().all(|entry| {
            entry.finding().state()
                == tracedecay_application::doctor::DoctorEvidenceStateV1::Unknown
        }));
        assert_eq!(
            envelope
                .payload
                .report_coverage
                .as_ref()
                .unwrap()
                .families()
                .len(),
            KNOWN_DOCTOR_FINDING_FAMILIES.len()
        );
    }

    /// The route must add no presentation of its own: every envelope axis has to
    /// equal the API-owned projection of the very same report.
    #[tokio::test]
    async fn route_envelope_equals_the_api_owned_projection() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let inputs = crate::daemon::doctor_kernel::DoctorKernelInputsV1::all_unknown();
        let report = crate::daemon::doctor_kernel::compose_doctor_report(&context(), &inputs)
            .await
            .unwrap();
        let (_project, _runtime, state) = state_with_inputs(inputs).await;

        for family in [None, Some(DoctorFindingFamilyV1::Storage)] {
            let expected = project_doctor_report(&report, family).expect("canonical projection");
            let envelope = findings_for_family(state.clone(), family).await;

            assert_eq!(envelope.domain_state, expected.presentation.domain_state);
            assert_eq!(envelope.coverage, expected.presentation.coverage);
            assert_eq!(
                envelope.freshness.state,
                expected.presentation.freshness.state
            );
            assert_eq!(envelope.legal_actions, expected.presentation.legal_actions);
            assert_eq!(envelope.payload.entries, expected.entries);
            assert_eq!(
                envelope.payload.report_coverage.as_ref(),
                Some(&expected.report_coverage)
            );
            assert_eq!(envelope.payload.note, expected.note);
            assert_eq!(envelope.payload.family_filter, family);
        }
    }

    #[tokio::test]
    async fn findings_route_does_not_invent_an_action_from_a_descriptor() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let mut inputs = crate::daemon::doctor_kernel::DoctorKernelInputsV1::all_unknown();
        inputs.configuration = ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
        let (_project, _runtime, state) = state_with_inputs(inputs).await;

        let Json(envelope) = findings(
            State(state),
            Query(DoctorFindingsQueryV1 {
                family: Some("configuration".to_string()),
            }),
        )
        .await;

        assert_eq!(envelope.payload.entries.len(), 1);
        assert_eq!(envelope.payload.remediations.len(), 1);
        assert_eq!(
            envelope.payload.remediations[0].operation().as_str(),
            tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY
        );
        assert_eq!(
            envelope.legal_actions,
            vec![DashboardLegalActionRefV1::new(
                DashboardLegalActionKindV1::Refresh,
                "use-case.dashboard.doctor.findings.refresh",
            )]
        );
    }

    #[tokio::test]
    async fn findings_route_exposes_an_action_only_from_an_injected_owner_dispatcher() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let mut inputs = crate::daemon::doctor_kernel::DoctorKernelInputsV1::all_unknown();
        inputs.configuration = ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
        let (_project, _runtime, mut state) = state_with_inputs(inputs).await;
        state.doctor_remediation_dispatcher = Some(
            super::super::doctor_remediation_api::DoctorRemediationDispatcherV1::new(
                Arc::new(|_| {
                    Box::pin(async {
                        vec![
                            super::super::doctor_remediation_api::DoctorRemediationLegalActionV1::RequestApply,
                        ]
                    })
                }),
                Arc::new(|_| {
                    Box::pin(async {
                        Err(
                            super::super::doctor_remediation_api::DoctorRemediationDispatchErrorV1::OwnerUnavailable,
                        )
                    })
                }),
                Arc::new(|_| panic!("finding projection never observes remediation")),
            ),
        );

        let Json(envelope) = findings(
            State(state),
            Query(DoctorFindingsQueryV1 {
                family: Some("configuration".to_string()),
            }),
        )
        .await;

        assert!(
            envelope
                .legal_actions
                .contains(&DashboardLegalActionRefV1::new(
                    DashboardLegalActionKindV1::RequestApply,
                    tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY,
                ))
        );
        // The owner authorizes the action even though this route cannot build
        // the protected-apply target from the finding. Suppressing the action
        // because the target is absent would report "no authorized action" for
        // an operation the owner does authorize.
        assert_eq!(envelope.payload.remediations.len(), 1);
        assert!(envelope.payload.remediations[0].target.is_none());
    }
}
