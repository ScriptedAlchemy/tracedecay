//! `GET /api/doctor/findings`, root composition for the Doctor finding family.
//!
//! An admitted daemon owner injects the canonical composed report reader into
//! [`DashboardState`]. This module only resolves scope and invokes that reader.
//! Every presentation decision, family vocabulary, coverage, freshness,
//! domain state, notes, and refresh action, belongs to
//! [`tracedecay_api::doctor`]. A dashboard opened without an admitted reader
//! remains explicitly unsupported.

use axum::Json;
use axum::extract::{Query, State};
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_api::doctor::{
    DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE, DoctorFindingsQueryV1, DoctorReadPresentationV1,
    doctor_report_failure_note, parse_doctor_finding_family, project_doctor_report,
};
use tracedecay_contracts::doctor::{
    DOCTOR_FINDING_FAMILIES, DoctorCoverageCompletenessV1, DoctorEvidenceStateV1,
    DoctorFamilyConsultationV1, DoctorFamilyCoverageV1, DoctorFamilyUnavailableReasonV1,
    DoctorFindingFamilyV1, DoctorReportCoverageV1, DoctorReportEntryV1, DoctorStorageFindingKindV1,
};
use tracedecay_contracts::storage::SchemaConvergenceFindingV1;

use super::DashboardState;
use super::read_model::{
    DashboardDomainStateV1, DashboardEnvelopeV1, DashboardScopeV1, scope_from_state,
};

const STORAGE_KINDS: [DoctorStorageFindingKindV1; 6] = [
    DoctorStorageFindingKindV1::OverBudgetStore,
    DoctorStorageFindingKindV1::OrphanStore,
    DoctorStorageFindingKindV1::IncidentDebrisPresent,
    DoctorStorageFindingKindV1::RetentionBacklog,
    DoctorStorageFindingKindV1::TableGrowth,
    DoctorStorageFindingKindV1::PendingSchemaMigration,
];

/// The canonical Doctor report projection for the read-only dashboard.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DoctorFindingsPayloadV1 {
    pub family_filter: Option<DoctorFindingFamilyV1>,
    pub entries: Vec<DoctorReportEntryV1>,
    pub report_coverage: Option<DoctorReportCoverageV1>,
    pub known_families: Vec<DoctorFindingFamilyV1>,
    pub schema_convergences: Vec<SchemaConvergenceFindingV1>,
    /// Source coverage for each typed storage finding producer. Empty when the
    /// family filter excludes storage or the family filter was rejected.
    pub storage_kind_statuses: Vec<StorageFindingKindStatusV1>,
    pub note: String,
}

/// Whether one storage finding producer had enough source evidence to report
/// a real result. This is source coverage, not a health grade: `Real` can
/// describe a clean observation or a problem finding.
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

/// `GET /api/doctor/findings`
#[hotpath::measure(label = "dashboard_api.doctor.findings", future = true)]
pub async fn findings(
    State(state): State<DashboardState>,
    Query(params): Query<DoctorFindingsQueryV1>,
) -> Json<DashboardEnvelopeV1<DoctorFindingsPayloadV1>> {
    let scope = scope_from_state(&state);
    Json(findings_with_authorities(scope, params, state.doctor_report_reader.clone()).await)
}

async fn findings_with_authorities(
    scope: DashboardScopeV1,
    params: DoctorFindingsQueryV1,
    doctor_report_reader: Option<crate::DoctorReportReader>,
) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
    // Validate the optional per-family filter against the closed vocabulary. An
    // unknown family is a typed `error` envelope, not a silent all-families read.
    let family_filter = match parse_doctor_finding_family(params.family.as_deref()) {
        Ok(family) => family,
        Err(invalid) => {
            let payload =
                unavailable_payload(None, format!("unknown doctor finding family: {invalid}"));
            let mut envelope = DashboardEnvelopeV1::unsupported(scope, payload);
            envelope.domain_state = DashboardDomainStateV1::Error;
            return envelope;
        }
    };
    let Some(reader) = doctor_report_reader.as_ref() else {
        return envelope(
            scope,
            DoctorReadPresentationV1::source_unsupported(),
            unavailable_payload(family_filter, DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE),
        );
    };

    // The admitted daemon composes the report across every finding producer;
    // this single await is the expensive phase behind `/api/doctor/*`, and
    // the span records failed reads too.
    let admitted =
        match hotpath::future!(reader(), label = "dashboard_api.doctor.report_read").await {
            Ok(admitted) => admitted,
            Err(error) => {
                return envelope(
                    scope,
                    DoctorReadPresentationV1::source_failed(),
                    unavailable_payload(family_filter, doctor_report_failure_note(&error)),
                );
            }
        };

    let projection = match project_doctor_report(&admitted.report, family_filter) {
        Ok(projection) => projection,
        Err(rejection) => {
            return envelope(
                scope,
                DoctorReadPresentationV1::source_failed(),
                unavailable_payload(family_filter, rejection.note()),
            );
        }
    };

    envelope(
        scope,
        projection.presentation,
        DoctorFindingsPayloadV1 {
            family_filter,
            entries: projection.entries,
            report_coverage: Some(projection.report_coverage),
            known_families: DOCTOR_FINDING_FAMILIES.to_vec(),
            schema_convergences: admitted.schema_convergences,
            storage_kind_statuses: Vec::new(),
            note: projection.note,
        },
    )
}

fn envelope(
    scope: DashboardScopeV1,
    presentation: DoctorReadPresentationV1,
    mut payload: DoctorFindingsPayloadV1,
) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
    if payload
        .family_filter
        .is_none_or(|family| family == DoctorFindingFamilyV1::Storage)
    {
        payload.storage_kind_statuses = STORAGE_KINDS
            .into_iter()
            .map(|kind| storage_kind_status(&payload, kind))
            .collect();
    }
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
        known_families: DOCTOR_FINDING_FAMILIES.to_vec(),
        schema_convergences: Vec::new(),
        storage_kind_statuses: Vec::new(),
        note: note.into(),
    }
}

fn storage_kind_status(
    payload: &DoctorFindingsPayloadV1,
    kind: DoctorStorageFindingKindV1,
) -> StorageFindingKindStatusV1 {
    let consultation = payload.report_coverage.as_ref().and_then(|coverage| {
        coverage
            .families()
            .iter()
            .find(|family| family.family() == DoctorFindingFamilyV1::Storage)
            .map(DoctorFamilyCoverageV1::consultation)
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
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
    use tracedecay_api::doctor::doctor_findings_refresh_action;
    use tracedecay_contracts::doctor::{
        AdvisoryFeedbackDoctorPort, AdvisoryFeedbackReadV1, CodeIndexMountDoctorPort,
        CodeIndexMountReadV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
        DoctorReportComposerV1, DoctorReportV1, DoctorSourceFuture, DoctorStorageFamilyReadV1,
        HostIntegrationDoctorPort, HostIntegrationReadV1, IngestRefusalCensusReadV1,
        LanguageServerDoctorPort, LanguageServerReadV1, ObservabilityDoctorPort,
        ObservabilityReadV1, OperationalAuditDoctorPort, OperationalAuditReadV1,
        ProfileAuthorityReadV1, RemoteOperationalReadV1, ResidentMemoryDoctorPort,
        ResidentMemoryReadV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1, StorageDoctorPort,
    };
    use tracedecay_contracts::storage::{
        SchemaConvergenceFindingV1, SchemaConvergenceProgressV1, SchemaConvergenceStageV1,
        SchemaConvergenceStateV1,
    };
    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    struct DoctorTestSourcesV1 {
        configuration: ConfigurationAuthorityReadV1,
    }

    impl DoctorTestSourcesV1 {
        fn all_unknown() -> Self {
            Self {
                configuration: ConfigurationAuthorityReadV1::Unknown,
            }
        }
    }

    impl ConfigurationAuthorityDoctorPort for DoctorTestSourcesV1 {
        fn configuration_health<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, ConfigurationAuthorityReadV1> {
            let read = self.configuration.clone();
            Box::pin(async move { read })
        }
    }

    impl RuntimeHealthDoctorPort for DoctorTestSourcesV1 {
        fn runtime_health<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, RuntimeHealthReadV1> {
            Box::pin(async { RuntimeHealthReadV1::Unknown })
        }
    }

    impl OperationalAuditDoctorPort for DoctorTestSourcesV1 {
        fn operational_audit<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, OperationalAuditReadV1> {
            Box::pin(async {
                OperationalAuditReadV1 {
                    remote: RemoteOperationalReadV1::Unavailable,
                    profile_authority: ProfileAuthorityReadV1::Unavailable,
                }
            })
        }
    }

    impl HostIntegrationDoctorPort for DoctorTestSourcesV1 {
        fn host_conformance<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, HostIntegrationReadV1> {
            Box::pin(async { HostIntegrationReadV1::Unknown })
        }
    }

    impl AdvisoryFeedbackDoctorPort for DoctorTestSourcesV1 {
        fn advisory_feedback<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, AdvisoryFeedbackReadV1> {
            Box::pin(async { AdvisoryFeedbackReadV1::Unknown })
        }
    }

    impl LanguageServerDoctorPort for DoctorTestSourcesV1 {
        fn language_server_health<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, LanguageServerReadV1> {
            Box::pin(async { LanguageServerReadV1::Unknown })
        }
    }

    impl CodeIndexMountDoctorPort for DoctorTestSourcesV1 {
        fn code_index_mount<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, CodeIndexMountReadV1> {
            Box::pin(async { CodeIndexMountReadV1::Unknown })
        }
    }

    impl ObservabilityDoctorPort for DoctorTestSourcesV1 {
        fn observability_health<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, ObservabilityReadV1> {
            Box::pin(async { ObservabilityReadV1::Unknown })
        }

        fn ingest_refusal_census<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, IngestRefusalCensusReadV1> {
            Box::pin(async { IngestRefusalCensusReadV1::Unknown })
        }
    }

    impl ResidentMemoryDoctorPort for DoctorTestSourcesV1 {
        fn resident_memory<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, ResidentMemoryReadV1> {
            Box::pin(async { ResidentMemoryReadV1::Unobserved })
        }
    }

    impl StorageDoctorPort for DoctorTestSourcesV1 {
        fn storage_findings<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> DoctorSourceFuture<'a, DoctorStorageFamilyReadV1> {
            Box::pin(async { DoctorStorageFamilyReadV1::Unknown })
        }
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

    fn dashboard_scope() -> DashboardScopeV1 {
        DashboardScopeV1 {
            project_id: Some("project.dashboard-doctor-findings".to_owned()),
            storage_mode: "project_local".to_owned(),
            store_root: "fixture".to_owned(),
        }
    }

    async fn compose_report(inputs: &DoctorTestSourcesV1) -> DoctorReportV1 {
        let context = context();
        DoctorReportComposerV1::new()
            .with_configuration(inputs)
            .with_runtime(inputs)
            .with_operational_audit(inputs)
            .with_host(inputs)
            .with_advisory_feedback(inputs)
            .with_language_server(inputs)
            .with_code_index(inputs)
            .with_observability(inputs)
            .with_storage(inputs)
            .with_memory(inputs)
            .compose(&context)
            .await
            .expect("canonical Doctor report")
    }

    fn reader_for(report: DoctorReportV1) -> crate::DoctorReportReader {
        Arc::new(move || {
            let report = report.clone();
            Box::pin(async move { Ok(crate::AdmittedDoctorReportV1::new(report)) })
        })
    }

    async fn findings_for_test(
        params: DoctorFindingsQueryV1,
        report: Option<DoctorReportV1>,
    ) -> DashboardEnvelopeV1<DoctorFindingsPayloadV1> {
        findings_with_authorities(dashboard_scope(), params, report.map(reader_for)).await
    }

    #[tokio::test]
    async fn findings_route_is_typed_unsupported_not_empty_or_healthy() {
        let envelope = findings_for_test(DoctorFindingsQueryV1 { family: None }, None).await;

        assert_eq!(envelope.schema_revision, 1);
        // Absent producer -> unsupported, never complete_zero_findings/ready.
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert!(envelope.payload.entries.is_empty());
        assert_eq!(
            envelope.payload.known_families.len(),
            DOCTOR_FINDING_FAMILIES.len()
        );
        assert_eq!(envelope.payload.family_filter, None);
        assert_eq!(envelope.payload.note, DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE);
        assert_eq!(
            envelope.legal_actions,
            vec![doctor_findings_refresh_action()]
        );
    }

    #[tokio::test]
    async fn storage_family_without_admitted_reader_projects_every_producer_as_unsupported() {
        let envelope = findings_for_test(
            DoctorFindingsQueryV1 {
                family: Some("storage".to_string()),
            },
            None,
        )
        .await;

        assert_eq!(
            envelope.payload.family_filter,
            Some(DoctorFindingFamilyV1::Storage)
        );
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        let statuses = &envelope.payload.storage_kind_statuses;
        assert_eq!(
            statuses
                .iter()
                .map(|status| status.kind)
                .collect::<Vec<_>>(),
            STORAGE_KINDS
        );
        assert!(
            statuses.iter().all(
                |status| status.state == StorageFindingSourceStateV1::Unsupported
                    && !status.reason.is_empty()
            ),
            "an unadmitted canonical source must not report any producer as real: {statuses:?}"
        );

        let other = findings_for_test(
            DoctorFindingsQueryV1 {
                family: Some("advisory".to_string()),
            },
            None,
        )
        .await;
        assert!(other.payload.storage_kind_statuses.is_empty());
    }

    #[tokio::test]
    async fn storage_family_consulted_without_entries_is_partial_not_clean() {
        let report = compose_report(&DoctorTestSourcesV1::all_unknown()).await;
        let envelope = findings_for_test(
            DoctorFindingsQueryV1 {
                family: Some("storage".to_string()),
            },
            Some(report),
        )
        .await;

        let statuses = &envelope.payload.storage_kind_statuses;
        assert_eq!(statuses.len(), STORAGE_KINDS.len());
        assert!(
            statuses
                .iter()
                .all(|status| status.state != StorageFindingSourceStateV1::Real),
            "unknown storage evidence must never read as a real producer result: {statuses:?}"
        );
    }

    #[tokio::test]
    async fn findings_route_rejects_unknown_family_with_error_state() {
        let envelope = findings_for_test(
            DoctorFindingsQueryV1 {
                family: Some("not_a_family".to_string()),
            },
            None,
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
        let report = compose_report(&DoctorTestSourcesV1::all_unknown()).await;
        // Compare against the composed report rather than a fixed catalog size,
        // so adding or retiring a finding kind does not break this route test.
        let canonical_entries = report.entries().len();
        assert!(
            canonical_entries > 0,
            "all-unknown report must carry entries"
        );
        let envelope =
            findings_for_test(DoctorFindingsQueryV1 { family: None }, Some(report)).await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Partial);
        assert_eq!(envelope.payload.entries.len(), canonical_entries);
        assert!(envelope.payload.entries.iter().all(|entry| {
            entry.finding().state() == tracedecay_contracts::doctor::DoctorEvidenceStateV1::Unknown
        }));
        assert_eq!(
            envelope
                .payload
                .report_coverage
                .as_ref()
                .unwrap()
                .families()
                .len(),
            DOCTOR_FINDING_FAMILIES.len()
        );
    }

    #[tokio::test]
    async fn findings_route_preserves_typed_schema_convergence() {
        let report = compose_report(&DoctorTestSourcesV1::all_unknown()).await;
        let admitted = crate::AdmittedDoctorReportV1::new(report).with_schema_convergences(vec![
            SchemaConvergenceFindingV1 {
                store: "profile-sessions".to_owned(),
                stage: SchemaConvergenceStageV1::RegisteredSchema,
                state: SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress,
                progress: Some(SchemaConvergenceProgressV1::Pages {
                    done: 4,
                    remaining: 7,
                }),
                started_at_micros: 42,
                degraded_row: None,
            },
        ]);
        let reader: crate::DoctorReportReader = Arc::new(move || {
            let admitted = admitted.clone();
            Box::pin(async move { Ok(admitted) })
        });

        let envelope = findings_with_authorities(
            dashboard_scope(),
            DoctorFindingsQueryV1 { family: None },
            Some(reader),
        )
        .await;

        assert_eq!(
            envelope.payload.schema_convergences[0].state,
            SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress
        );
        assert_eq!(
            envelope.payload.schema_convergences[0].progress,
            Some(SchemaConvergenceProgressV1::Pages {
                done: 4,
                remaining: 7,
            })
        );
    }
}
