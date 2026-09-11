//! Daemon-owned Doctor signal-mapper and composition tests.
//!
//! Read mappers and [`DaemonRuntimeHealthSignalV1`] live in
//! `tracedecay-contracts::doctor`. Composition from resolved kernel reads is
//! owned here at the composition root.

use tracedecay_contracts::doctor::{
    DoctorCoverageCompletenessV1, HostConformanceV1, HostIntegrationReadV1, IngestRefusalCountV1,
    LanguageServerReadV1, LanguageServerStateV1, ObservabilityReadV1, ObservabilityStateV1,
};
use tracedecay_contracts::{
    ConfigurationAuthorityReadV1, storage::StorageTelemetryReadV1, storage::StoreKeyV1,
    storage::StoreSizeSampleV1,
};
use tracedecay_domain::UtcMicros;

use super::*;

#[test]
fn configuration_read_from_pin_absent_on_cold_cache() {
    let missing: Result<crate::config::PinnedRuntimeConfiguration, &str> = Err("cold cache");
    assert_eq!(
        configuration_read_from_pin(&missing),
        ConfigurationAuthorityReadV1::Absent
    );
}

/// The daemon-side Doctor reader must observe the exhaustive
/// observation-authority invariant pass itself. Without a producer the signal
/// is permanently not-run, which downgrades every `StorageRuntime` finding to
/// partial coverage and makes Doctor report unavailable audit data for a
/// perfectly healthy store.
#[tokio::test]
async fn observation_authority_audit_observes_the_real_invariant_pass() {
    let directory = tempfile::TempDir::new().expect("authority audit fixture root");
    let uninitialized_path = directory.path().join("uninitialized.db");
    let database_path = directory.path().join("registry.db");
    tracedecay_global_db::register_registered_schema_installer();
    let uninitialized_authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &uninitialized_path,
        "doctor uninitialized authority audit fixture",
    )
    .expect("doctor authority audit database authority");
    let (uninitialized, _) = tracedecay_runtime_core::db::Database::publish_test_runtime(
        &uninitialized_path,
        &uninitialized_authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("open uninitialized doctor audit fixture");

    assert!(
        !observation_authority_audit_passed(&uninitialized.read_connection()).await,
        "a store without the registered authority schema must fail the audit it ran"
    );
    drop(uninitialized);

    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &database_path,
        "doctor registered authority audit fixture",
    )
    .expect("doctor registered audit database authority");
    let (database, _) = tracedecay_runtime_core::db::Database::publish_registered_test_runtime(
        &database_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    .expect("install the registered authority schema");

    assert!(
        observation_authority_audit_passed(&database.read_connection()).await,
        "a converged registered authority must report a passing audit rather than \
         unavailable audit data"
    );
}

#[test]
fn receipt_and_checked_in_host_evidence_feed_canonical_host_truth() {
    let checked_in =
        tracedecay_agent_hosts::agents::host_bundle::HostBundleDoctorReportV1::default();
    assert_eq!(
        host_integration_read_from_report(&checked_in),
        HostIntegrationReadV1::Absent
    );

    let mut drifted = checked_in;
    drifted.components.push(
        tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentDoctorResultV1 {
            receipt_path: std::path::PathBuf::from("receipt.fixture.json"),
            host: Some(tracedecay_agent_hosts::agents::host_bundle::HostKindV1::Codex),
            component: Some(tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentV1::Core),
            state: tracedecay_agent_hosts::agents::host_bundle::HostBundleComponentDoctorStateV1::Repairable,
            registration: Some(
                tracedecay_agent_hosts::agents::host_bundle::HostBundleRegistrationStateV1::Repairable,
            ),
            artifacts: Vec::new(),
            repair_action: "repair fixture".to_owned(),
        },
    );
    assert_eq!(
        host_integration_read_from_report(&drifted),
        HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
}

#[test]
fn synchronous_table_growth_is_bounded_by_observed_store_size() {
    let sample = |bytes| StorageTelemetryReadV1::Observed {
        sample: StoreSizeSampleV1 {
            store: StoreKeyV1::new("sessions.db").unwrap(),
            page_size_bytes: 4096,
            page_count: bytes / 4096,
            freelist_pages: 0,
            observed_at: UtcMicros(1),
        },
    };

    assert!(permits_synchronous_table_growth(&sample(
        MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES
    )));
    assert!(!permits_synchronous_table_growth(&sample(
        MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES + 4096
    )));
    assert!(!permits_synchronous_table_growth(
        &StorageTelemetryReadV1::Unknown {
            store: StoreKeyV1::new("sessions.db").unwrap(),
        }
    ));
}

#[test]
fn language_server_engine_states_preserve_live_degradation() {
    use tracedecay_lsp::analyzer::broker::EngineState;

    assert_eq!(
        language_server_read_from_engine_states([]),
        LanguageServerReadV1::Absent
    );
    assert_eq!(
        language_server_read_from_engine_states([EngineState::Ready]),
        LanguageServerReadV1::Observed {
            state: LanguageServerStateV1::Ready,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    assert_eq!(
        language_server_read_from_engine_states([EngineState::Ready, EngineState::Crashed]),
        LanguageServerReadV1::Observed {
            state: LanguageServerStateV1::Crashed,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
}

#[test]
fn empty_observation_projection_is_absent() {
    let model =
        tracedecay_application::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("empty projection");
    assert_eq!(
        observability_read_from_model(Ok(model)),
        ObservabilityReadV1::Absent
    );
}

#[test]
fn retained_or_unreported_observation_history_is_not_absent() {
    let model =
        tracedecay_application::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
            &[],
            1,
            0,
        )
        .expect("retained projection");
    assert_eq!(
        observability_read_from_model(Ok(model)),
        ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 0,
            last_observed_at_micros: None,
            coverage: DoctorCoverageCompletenessV1::Partial,
        }
    );

    let unknown =
        tracedecay_application::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
            &[],
            0,
            1,
        )
        .expect("unknown projection");
    assert_eq!(
        observability_read_from_model(Ok(unknown)),
        ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 0,
            last_observed_at_micros: None,
            coverage: DoctorCoverageCompletenessV1::Unknown,
        }
    );

    let mut active =
        tracedecay_application::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("active empty projection");
    active.coverage = tracedecay_contracts::feedback::observations::FeedbackCoverageV1::Known;
    active.watermark.producer_boot_id =
        Some(tracedecay_domain::canonical_sha256(&"active-observation-boot").unwrap());
    assert_eq!(
        observability_read_from_model(Ok(active)),
        ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 0,
            last_observed_at_micros: None,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
}
#[tokio::test]
async fn composed_report_carries_real_states_and_enumerates_coverage() {
    use std::collections::BTreeSet;

    use tracedecay_contracts::doctor::{
        AdvisoryFeedbackReadV1, CodeIndexMountReadV1, CodeIndexMountStateV1,
        ConfigurationAuthorityReadV1, ConfigurationDriftV1, DoctorCoverageCompletenessV1,
        DoctorCoverageStatementV1, DoctorEvidenceRefV1, DoctorEvidenceReferenceV1,
        DoctorEvidenceStateV1, DoctorFamilyConsultationV1, DoctorFamilyCoverageV1,
        DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorFindingV1,
        DoctorKernelInputsV1, DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1,
        DoctorStorageFindingV1, OperationalAuditReadV1, ProfileAuthorityReadV1,
        RemoteOperationalReadV1, SemanticOwnerReadV1, SemanticOwnerStateV1,
    };
    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    let actor = ActorId::new("actor.doctor-kernel-compose-test").unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.doctor-kernel-compose-test").unwrap(),
        RepositoryId::new("repository.doctor-kernel-compose-test").unwrap(),
        WorktreeId::new("worktree.doctor-kernel-compose-test").unwrap(),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.doctor-kernel-compose-test").unwrap();
    let use_case = UseCaseId::new("use-case.doctor-kernel-compose-test").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.doctor-kernel-compose-test").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "11".repeat(32))).unwrap(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let ctx = RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.doctor-kernel-compose-test").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.doctor-kernel-compose-test").unwrap(),
    )
    .unwrap();

    let orphan_evidence = DoctorEvidenceRefV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceReferenceV1::new("storage.orphan_store.fixture.age-42d").unwrap(),
    );
    let orphan_coverage = DoctorCoverageStatementV1::new(
        DoctorCoverageCompletenessV1::Complete,
        "orphan store identity no longer resolves",
    )
    .unwrap();
    let orphan_finding = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceStateV1::Degraded,
        vec![orphan_evidence],
        orphan_coverage,
    )
    .unwrap();
    let orphan_storage =
        DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, orphan_finding)
            .unwrap();

    let inputs = DoctorKernelInputsV1 {
        configuration: ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: false,
            ..DaemonRuntimeHealthSignalV1::default()
        }),
        operational_audit: OperationalAuditReadV1 {
            remote: RemoteOperationalReadV1::Unconfigured,
            profile_authority: ProfileAuthorityReadV1::Unavailable,
        },
        host: HostIntegrationReadV1::Denied,
        advisory_feedback: AdvisoryFeedbackReadV1::Absent,
        language_server: LanguageServerReadV1::Observed {
            state: LanguageServerStateV1::Ready,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        code_index: CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Mounted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        semantic_owner: SemanticOwnerReadV1::Observed {
            state: SemanticOwnerStateV1::Ready,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        observability: ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 7,
            last_observed_at_micros: Some(42),
            coverage: DoctorCoverageCompletenessV1::Partial,
        },
        ingest_refusals: IngestRefusalCensusReadV1::Observed {
            refusals: vec![IngestRefusalCountV1 {
                provider: "cursor".to_owned(),
                reason: "admission_refused".to_owned(),
                count: 160,
            }],
        },
        storage: merge_storage_reads(
            storage_family_read(vec![orphan_storage]),
            DoctorStorageFamilyReadV1::Unknown,
        ),
    };

    let report = compose_doctor_report(&ctx, &inputs).await.expect("report");

    assert_eq!(report.coverage().families().len(), 7);

    let family_state = |family: DoctorFindingFamilyV1| {
        report
            .findings()
            .find(|finding| finding.family() == family)
            .map(DoctorFindingV1::state)
    };
    assert_eq!(
        family_state(DoctorFindingFamilyV1::Configuration),
        Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::StorageRuntime),
        Some(DoctorEvidenceStateV1::Degraded)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::Advisory),
        Some(DoctorEvidenceStateV1::Denied)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::SemanticIndex),
        Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::Storage),
        Some(DoctorEvidenceStateV1::Degraded)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::LanguageServer),
        Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
    );
    assert_eq!(
        family_state(DoctorFindingFamilyV1::Observability),
        Some(DoctorEvidenceStateV1::Partial)
    );

    assert!(!report.is_healthy_complete());
    assert_ne!(
        report.coverage().completeness(),
        DoctorCoverageCompletenessV1::Complete
    );

    let consultation = |family: DoctorFindingFamilyV1| {
        report
            .coverage()
            .families()
            .iter()
            .find(|record| record.family() == family)
            .map(DoctorFamilyCoverageV1::consultation)
    };
    assert_eq!(
        consultation(DoctorFindingFamilyV1::LanguageServer),
        Some(DoctorFamilyConsultationV1::Consulted)
    );
    assert_eq!(
        consultation(DoctorFindingFamilyV1::Observability),
        Some(DoctorFamilyConsultationV1::Consulted)
    );
    assert_eq!(
        consultation(DoctorFindingFamilyV1::Advisory),
        Some(DoctorFamilyConsultationV1::Unavailable {
            reason: DoctorFamilyUnavailableReasonV1::Denied,
        })
    );
    assert_eq!(
        consultation(DoctorFindingFamilyV1::Configuration),
        Some(DoctorFamilyConsultationV1::Consulted)
    );
    assert_eq!(
        consultation(DoctorFindingFamilyV1::Storage),
        Some(DoctorFamilyConsultationV1::Unavailable {
            reason: DoctorFamilyUnavailableReasonV1::Unknown,
        })
    );
}
