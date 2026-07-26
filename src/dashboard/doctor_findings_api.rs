//! `GET /api/doctor/findings` — the HTTP surface binding for the Doctor finding
//! family (plan 09 §PR14 / plan 11 §"Typed presentation contracts").
//!
//! An admitted daemon owner injects the canonical composed report reader into
//! [`DashboardState`]. The route only filters and projects that report; it never
//! evaluates Doctor health or invents remediation. A dashboard opened without
//! an admitted reader remains explicitly unsupported.

use axum::Json;
use axum::extract::{Query, State};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
    DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorRemediationDescriptorV1,
    DoctorRemediationRegistryV1, DoctorReportCoverageV1, DoctorReportEntryV1, DoctorReportV1,
};

use super::DashboardState;
use super::doctor_remediation_api::DoctorRemediationTargetV1;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, scope_from_state,
};

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct DashboardDoctorRemediationDescriptorV1 {
    #[serde(flatten)]
    descriptor: DoctorRemediationDescriptorV1,
    pub target: Option<DoctorRemediationTargetV1>,
}

impl DashboardDoctorRemediationDescriptorV1 {
    fn operation(&self) -> &tracedecay_application::doctor::DoctorOwningOperationRefV1 {
        self.descriptor.operation()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct FindingsParams {
    /// Optional per-family filter (`advisory`, `configuration`,
    /// `storage_runtime`, `storage`, `language_server`, `semantic_index`,
    /// `observability`).
    #[serde(default)]
    family: Option<String>,
}

/// The canonical Doctor report projection. `entries` retains the storage
/// subclass attached by the kernel; remediation descriptors are emitted only
/// after the kernel registry resolves the finding's owner-supplied reference.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct DoctorFindingsPayloadV1 {
    pub family_filter: Option<DoctorFindingFamilyV1>,
    pub entries: Vec<DoctorReportEntryV1>,
    pub report_coverage: Option<DoctorReportCoverageV1>,
    pub remediations: Vec<DashboardDoctorRemediationDescriptorV1>,
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
    "no admitted Doctor report source is available for this dashboard scope";
const REFRESH_OPERATION: &str = "use-case.dashboard.doctor.findings.refresh";

/// `GET /api/doctor/findings`
pub(crate) async fn findings(
    State(state): State<DashboardState>,
    Query(params): Query<FindingsParams>,
) -> Json<DashboardEnvelopeV1<DoctorFindingsPayloadV1>> {
    // Validate the optional per-family filter against the closed vocabulary. An
    // unknown family is a typed `error` envelope, not a silent all-families read.
    let family_filter = match parse_family(params.family.as_deref()) {
        Ok(family) => family,
        Err(invalid) => {
            let scope = scope_from_state(&state);
            let payload = DoctorFindingsPayloadV1 {
                family_filter: None,
                entries: Vec::new(),
                report_coverage: None,
                remediations: Vec::new(),
                known_families: KNOWN_FAMILIES.to_vec(),
                note: format!("unknown doctor finding family: {invalid}"),
            };
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
pub(crate) async fn findings_for_family(
    state: DashboardState,
    family_filter: Option<DoctorFindingFamilyV1>,
) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
    let scope = scope_from_state(&state);

    let Some(reader) = state.doctor_report_reader.as_ref() else {
        let payload = unavailable_payload(family_filter, UNSUPPORTED_NOTE);
        return DashboardEnvelopeV1::unsupported(scope, payload)
            .with_legal_actions(vec![refresh_action()]);
    };

    let report = match reader().await {
        Ok(admitted) => admitted.report,
        Err(error) => {
            let payload = unavailable_payload(
                family_filter,
                format!("Doctor report composition failed: {error}"),
            );
            return DashboardEnvelopeV1::new(
                scope,
                DashboardDomainStateV1::Error,
                DashboardCoverageV1::unknown(),
                DashboardFreshnessV1::unknown(),
                payload,
            )
            .with_legal_actions(vec![refresh_action()]);
        }
    };

    match project_report(
        report,
        family_filter,
        state.doctor_remediation_dispatcher.as_ref(),
    )
    .await
    {
        Ok(projected) => DashboardEnvelopeV1::new(
            scope,
            projected.domain_state,
            projected.coverage,
            projected.freshness,
            projected.payload,
        )
        .with_legal_actions(projected.legal_actions),
        Err(note) => DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::Error,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            unavailable_payload(family_filter, note),
        )
        .with_legal_actions(vec![refresh_action()]),
    }
}

struct ProjectedDoctorReportV1 {
    payload: DoctorFindingsPayloadV1,
    coverage: DashboardCoverageV1,
    freshness: DashboardFreshnessV1,
    domain_state: DashboardDomainStateV1,
    legal_actions: Vec<DashboardLegalActionRefV1>,
}

async fn project_report(
    report: DoctorReportV1,
    family_filter: Option<DoctorFindingFamilyV1>,
    dispatcher: Option<&super::doctor_remediation_api::DoctorRemediationDispatcherV1>,
) -> Result<ProjectedDoctorReportV1, String> {
    let entries = report
        .entries()
        .iter()
        .filter(|entry| family_filter.is_none_or(|family| entry.finding().family() == family))
        .cloned()
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err("canonical Doctor report omitted the requested finding family".to_string());
    }

    let registry = DoctorRemediationRegistryV1::default_registry();
    let mut remediations = Vec::new();
    let mut legal_actions = vec![refresh_action()];
    for reference in entries
        .iter()
        .filter_map(|entry| entry.finding().remediation())
    {
        let descriptor = registry.resolve(reference).map_err(|error| {
            format!("Doctor remediation reference was rejected by the canonical registry: {error}")
        })?;
        let target = DoctorRemediationTargetV1::for_operation(descriptor.operation());
        if !remediations
            .iter()
            .any(|current: &DashboardDoctorRemediationDescriptorV1| {
                current.operation() == descriptor.operation()
            })
        {
            remediations.push(DashboardDoctorRemediationDescriptorV1 {
                descriptor: descriptor.clone(),
                target: target.clone(),
            });
        }
        if target.is_some()
            && let Some(dispatcher) = dispatcher
        {
            for kind in dispatcher.legal_actions(reference).await {
                let kind = match kind {
                    super::doctor_remediation_api::DoctorRemediationLegalActionV1::RequestPreview => {
                        DashboardLegalActionKindV1::RequestDryRun
                    }
                    super::doctor_remediation_api::DoctorRemediationLegalActionV1::RequestApply => {
                        DashboardLegalActionKindV1::RequestApply
                    }
                };
                let action = DashboardLegalActionRefV1::new(
                    kind,
                    reference.owning_operation().as_str().to_string(),
                );
                if !legal_actions.contains(&action) {
                    legal_actions.push(action);
                }
            }
        }
    }

    let coverage = dashboard_coverage(&report, family_filter, &entries);
    let domain_state = dashboard_domain_state(&entries, &coverage);
    let freshness = dashboard_freshness(&entries, domain_state);
    let note = report.coverage().statement().statement().to_string();
    let report_coverage = report.coverage().clone();
    Ok(ProjectedDoctorReportV1 {
        payload: DoctorFindingsPayloadV1 {
            family_filter,
            entries,
            report_coverage: Some(report_coverage),
            remediations,
            known_families: KNOWN_FAMILIES.to_vec(),
            note,
        },
        coverage,
        freshness,
        domain_state,
        legal_actions,
    })
}

fn dashboard_coverage(
    report: &DoctorReportV1,
    family_filter: Option<DoctorFindingFamilyV1>,
    entries: &[DoctorReportEntryV1],
) -> DashboardCoverageV1 {
    if let Some(family) = family_filter {
        let Some(family_coverage) = report
            .coverage()
            .families()
            .iter()
            .find(|coverage| coverage.family() == family)
        else {
            return DashboardCoverageV1::unknown();
        };
        return match family_coverage.consultation() {
            DoctorFamilyConsultationV1::Unavailable {
                reason:
                    DoctorFamilyUnavailableReasonV1::Unwired
                    | DoctorFamilyUnavailableReasonV1::Unsupported,
            } => DashboardCoverageV1::unsupported(),
            DoctorFamilyConsultationV1::Unavailable { .. } => DashboardCoverageV1::unknown(),
            DoctorFamilyConsultationV1::Consulted => {
                dashboard_finding_coverage(entries, family_label(family))
            }
        };
    }

    match report.coverage().completeness() {
        DoctorCoverageCompletenessV1::Complete => {
            DashboardCoverageV1::complete(KNOWN_FAMILIES.len() as u64, "doctor_families")
        }
        DoctorCoverageCompletenessV1::Partial => {
            let consulted = report
                .coverage()
                .families()
                .iter()
                .filter(|coverage| {
                    matches!(
                        coverage.consultation(),
                        DoctorFamilyConsultationV1::Consulted
                    )
                })
                .count() as u64;
            let omissions = report
                .coverage()
                .families()
                .iter()
                .filter_map(|coverage| match coverage.consultation() {
                    DoctorFamilyConsultationV1::Consulted => None,
                    DoctorFamilyConsultationV1::Unavailable { reason } => {
                        Some(format!("{}:{reason:?}", family_label(coverage.family())))
                    }
                })
                .collect();
            DashboardCoverageV1::partial(
                KNOWN_FAMILIES.len() as u64,
                consulted,
                "doctor_families",
                omissions,
            )
        }
        DoctorCoverageCompletenessV1::Unknown => {
            let unsupported = report.coverage().families().iter().all(|coverage| {
                matches!(
                    coverage.consultation(),
                    DoctorFamilyConsultationV1::Unavailable {
                        reason: DoctorFamilyUnavailableReasonV1::Unwired
                            | DoctorFamilyUnavailableReasonV1::Unsupported
                    }
                )
            });
            if unsupported {
                DashboardCoverageV1::unsupported()
            } else {
                DashboardCoverageV1::unknown()
            }
        }
    }
}

fn dashboard_finding_coverage(
    entries: &[DoctorReportEntryV1],
    family: &'static str,
) -> DashboardCoverageV1 {
    if entries.iter().all(|entry| {
        entry.finding().coverage().completeness() == DoctorCoverageCompletenessV1::Complete
    }) {
        return DashboardCoverageV1::complete(entries.len() as u64, "doctor_findings");
    }
    if entries.iter().any(|entry| {
        entry.finding().coverage().completeness() == DoctorCoverageCompletenessV1::Partial
    }) {
        return DashboardCoverageV1::partial(
            entries.len() as u64,
            entries
                .iter()
                .filter(|entry| {
                    entry.finding().coverage().completeness()
                        == DoctorCoverageCompletenessV1::Complete
                })
                .count() as u64,
            "doctor_findings",
            vec![format!("{family}:partial")],
        );
    }
    DashboardCoverageV1::unknown()
}

fn dashboard_domain_state(
    entries: &[DoctorReportEntryV1],
    coverage: &DashboardCoverageV1,
) -> DashboardDomainStateV1 {
    if coverage.is_complete()
        && entries
            .iter()
            .all(|entry| entry.finding().state().is_healthy_complete())
    {
        return DashboardDomainStateV1::Ready;
    }
    if entries
        .iter()
        .all(|entry| entry.finding().state() == DoctorEvidenceStateV1::Unsupported)
    {
        return DashboardDomainStateV1::Unsupported;
    }
    if entries
        .iter()
        .all(|entry| entry.finding().state() == DoctorEvidenceStateV1::Denied)
    {
        return DashboardDomainStateV1::Denied;
    }
    if entries
        .iter()
        .any(|entry| entry.finding().state() == DoctorEvidenceStateV1::Stale)
    {
        return DashboardDomainStateV1::Stale;
    }
    DashboardDomainStateV1::Partial
}

fn dashboard_freshness(
    entries: &[DoctorReportEntryV1],
    domain_state: DashboardDomainStateV1,
) -> DashboardFreshnessV1 {
    if domain_state == DashboardDomainStateV1::Unsupported {
        return DashboardFreshnessV1::unsupported();
    }
    if entries
        .iter()
        .any(|entry| entry.finding().state() == DoctorEvidenceStateV1::Stale)
    {
        return DashboardFreshnessV1 {
            state: super::read_model::DashboardFreshnessStateV1::Stale,
            observed_at_micros: Some(super::read_model::now_micros()),
            watermark: None,
        };
    }
    DashboardFreshnessV1::fresh_now()
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
        known_families: KNOWN_FAMILIES.to_vec(),
        note: note.into(),
    }
}

fn refresh_action() -> DashboardLegalActionRefV1 {
    DashboardLegalActionRefV1::new(DashboardLegalActionKindV1::Refresh, REFRESH_OPERATION)
}

const fn family_label(family: DoctorFindingFamilyV1) -> &'static str {
    match family {
        DoctorFindingFamilyV1::Advisory => "advisory",
        DoctorFindingFamilyV1::Configuration => "configuration",
        DoctorFindingFamilyV1::StorageRuntime => "storage_runtime",
        DoctorFindingFamilyV1::Storage => "storage",
        DoctorFindingFamilyV1::LanguageServer => "language_server",
        DoctorFindingFamilyV1::SemanticIndex => "semantic_index",
        DoctorFindingFamilyV1::Observability => "observability",
    }
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
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
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
        let state = crate::dashboard::build_state(&cg)
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
            Box::pin(async move { Ok(crate::dashboard::AdmittedDoctorReportV1::new(report)) })
        }));
        (project, runtime, state)
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
        let (_project, _runtime, state) = state_for_test().await;
        let Json(envelope) = findings(State(state), Query(FindingsParams { family: None })).await;

        assert_eq!(envelope.schema_revision, 1);
        // Absent producer -> unsupported, never complete_zero_findings/ready.
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.entries.is_empty());
        assert_eq!(envelope.payload.known_families.len(), 7);
        assert_eq!(envelope.payload.family_filter, None);
    }

    #[tokio::test]
    async fn findings_route_echoes_valid_family_filter() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, _runtime, state) = state_for_test().await;
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
        let (_project, _runtime, state) = state_for_test().await;
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

    #[tokio::test]
    async fn findings_route_preserves_canonical_unknown_entries() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, _runtime, state) =
            state_with_inputs(crate::daemon::doctor_kernel::DoctorKernelInputsV1::all_unknown())
                .await;

        let Json(envelope) = findings(State(state), Query(FindingsParams { family: None })).await;

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
            KNOWN_FAMILIES.len()
        );
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
            Query(FindingsParams {
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
            Query(FindingsParams {
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
    }
}
