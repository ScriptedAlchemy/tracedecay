//! Per-adapter and composed-report tests for the daemon Doctor kernel wiring.

use std::collections::BTreeSet;

use tracedecay_application::doctor::{
    CodeIndexMountReadV1, CodeIndexMountStateV1, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DoctorCoverageCompletenessV1, DoctorCoverageStatementV1,
    DoctorEvidenceRefV1, DoctorEvidenceReferenceV1, DoctorEvidenceStateV1,
    DoctorFamilyConsultationV1, DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1,
    DoctorFindingV1, DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1, DoctorStorageFindingV1,
    HostConformanceV1, HostIntegrationReadV1, LanguageServerReadV1, LanguageServerStateV1,
    ObservabilityReadV1, ObservabilityStateV1, RuntimeHealthReadV1, RuntimeLivenessV1,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;

// --- Fixtures ---------------------------------------------------------------

fn context() -> RequestContext {
    let actor = ActorId::new("actor.doctor-kernel-test").unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.doctor-kernel-test").unwrap(),
        RepositoryId::new("repository.doctor-kernel-test").unwrap(),
        WorktreeId::new("worktree.doctor-kernel-test").unwrap(),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.doctor-kernel-test").unwrap();
    let use_case = UseCaseId::new("use-case.doctor-kernel-test").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.doctor-kernel-test").unwrap(),
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
    RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.doctor-kernel-test").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.doctor-kernel-test").unwrap(),
    )
    .unwrap()
}

fn orphan_storage_finding() -> DoctorStorageFindingV1 {
    let evidence = DoctorEvidenceRefV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceReferenceV1::new("storage.orphan_store.fixture.age-42d").unwrap(),
    );
    let coverage = DoctorCoverageStatementV1::new(
        DoctorCoverageCompletenessV1::Complete,
        "orphan store identity no longer resolves",
    )
    .unwrap();
    let remediation = DoctorRemediationRefV1::new(
        DoctorOwningOperationRefV1::new("use-case.application.storage.collect-orphan-store")
            .unwrap(),
        DoctorRemediationKindV1::Action,
    );
    let finding = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceStateV1::Degraded,
        vec![evidence],
        coverage,
        Some(remediation),
    )
    .unwrap();
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, finding).unwrap()
}

// --- Configuration authority mapper -----------------------------------------

#[test]
fn configuration_signal_maps_each_variant_honestly() {
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::Pinned),
        ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::Drifted),
        ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::PinUnavailable),
        ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::PinUnavailable,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::Missing),
        ConfigurationAuthorityReadV1::Absent
    );
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::Denied),
        ConfigurationAuthorityReadV1::Denied
    );
    assert_eq!(
        configuration_read(ConfigurationAuthoritySignalV1::Unknown),
        ConfigurationAuthorityReadV1::Unknown
    );
}

#[test]
fn configuration_read_from_pin_absent_on_cold_cache() {
    let missing: Result<crate::config::PinnedRuntimeConfiguration, &str> = Err("cold cache");
    assert_eq!(
        configuration_read_from_pin(&missing),
        ConfigurationAuthorityReadV1::Absent
    );
}

#[tokio::test]
async fn configuration_adapter_returns_seeded_read() {
    let ctx = context();
    for read in [
        ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        ConfigurationAuthorityReadV1::Absent,
        ConfigurationAuthorityReadV1::Denied,
        ConfigurationAuthorityReadV1::Unknown,
    ] {
        let adapter = ConfigurationAuthorityDoctorAdapterV1::from_read(read.clone());
        assert_eq!(adapter.configuration_health(&ctx).await, read);
    }
}

// --- Runtime health mapper --------------------------------------------------

#[test]
fn runtime_healthy_requires_all_signals_observed_for_complete_coverage() {
    let healthy = DaemonRuntimeHealthSignalV1 {
        serving: true,
        startup_converged: true,
        quick_check_ok: Some(true),
        authority_audit_ok: Some(true),
        temporal_ok: Some(true),
    };
    assert_eq!(
        runtime_health_read(&healthy),
        RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Healthy,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    // A serving, converged daemon with an unobserved signal is healthy so far
    // as observed but never healthy-complete.
    let partial = DaemonRuntimeHealthSignalV1 {
        temporal_ok: None,
        ..healthy
    };
    assert_eq!(
        runtime_health_read(&partial),
        RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Healthy,
            coverage: DoctorCoverageCompletenessV1::Partial,
        }
    );
}

#[test]
fn runtime_degraded_stuck_and_unreachable_are_honest() {
    let degraded = DaemonRuntimeHealthSignalV1 {
        serving: true,
        startup_converged: false,
        ..DaemonRuntimeHealthSignalV1::default()
    };
    assert_eq!(
        runtime_health_read(&degraded),
        RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Degraded,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    let stuck = DaemonRuntimeHealthSignalV1 {
        serving: true,
        startup_converged: true,
        quick_check_ok: Some(false),
        ..DaemonRuntimeHealthSignalV1::default()
    };
    assert_eq!(
        runtime_health_read(&stuck),
        RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Stuck,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    let unreachable = DaemonRuntimeHealthSignalV1::default();
    assert_eq!(
        runtime_health_read(&unreachable),
        RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Unreachable,
            coverage: DoctorCoverageCompletenessV1::Unknown,
        }
    );
}

#[tokio::test]
async fn runtime_adapter_returns_seeded_read() {
    let ctx = context();
    let read = RuntimeHealthReadV1::Denied;
    let adapter = RuntimeHealthDoctorAdapterV1::from_read(read.clone());
    assert_eq!(adapter.runtime_health(&ctx).await, read);
}

// --- Host integration mapper ------------------------------------------------

#[test]
fn host_conformance_maps_conformant_drift_and_absent() {
    let conformant = HostConformanceSummaryV1 {
        probed: 3,
        accepted: 3,
        executable_present: true,
    };
    assert_eq!(
        host_conformance_read(&conformant),
        HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::Conformant,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    let mixed = HostConformanceSummaryV1 {
        probed: 3,
        accepted: 1,
        executable_present: true,
    };
    assert!(matches!(
        host_conformance_read(&mixed),
        HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::Drifted,
            ..
        }
    ));
    let all_rejected = HostConformanceSummaryV1 {
        probed: 2,
        accepted: 0,
        executable_present: true,
    };
    assert!(matches!(
        host_conformance_read(&all_rejected),
        HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::ProtocolDrift,
            ..
        }
    ));
    let no_exe = HostConformanceSummaryV1 {
        probed: 1,
        accepted: 0,
        executable_present: false,
    };
    assert!(matches!(
        host_conformance_read(&no_exe),
        HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::ExecutableAbsent,
            ..
        }
    ));
    let none = HostConformanceSummaryV1::default();
    assert_eq!(host_conformance_read(&none), HostIntegrationReadV1::Absent);
}

#[test]
fn host_conformance_summary_counts_real_verifier_outcomes() {
    let installed = ["good-a", "bad", "good-b"];
    let summary =
        host_conformance_summary(&installed, true, |manifest| manifest.starts_with("good"));
    assert_eq!(summary.probed, 3);
    assert_eq!(summary.accepted, 2);
    assert!(summary.executable_present);
}

#[test]
fn receipt_and_checked_in_host_evidence_feed_canonical_host_truth() {
    let checked_in = crate::agents::host_bundle_v2::HostBundleDoctorReportV1::default();
    assert_eq!(
        host_integration_read_from_report(&checked_in),
        HostIntegrationReadV1::Absent
    );

    let mut drifted = checked_in;
    drifted.components.push(
        crate::agents::host_bundle_v2::HostBundleComponentDoctorResultV1 {
            receipt_path: std::path::PathBuf::from("receipt.fixture.json"),
            host: Some(crate::agents::host_bundle_v2::HostKindV1::Codex),
            component: Some(crate::agents::host_bundle_v2::HostBundleComponentV1::Core),
            state: crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1::Repairable,
            registration: Some(
                crate::agents::host_bundle_v2::HostBundleRegistrationStateV1::Repairable,
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

#[tokio::test]
async fn host_adapter_returns_seeded_read() {
    let ctx = context();
    let read = HostIntegrationReadV1::Observed {
        conformance: HostConformanceV1::ProtocolDrift,
        coverage: DoctorCoverageCompletenessV1::Complete,
    };
    let adapter = HostIntegrationDoctorAdapterV1::from_read(read.clone());
    assert_eq!(adapter.host_conformance(&ctx).await, read);
}

// --- Code-index mount mapper ------------------------------------------------

#[test]
fn code_index_signal_maps_each_state() {
    assert_eq!(
        code_index_read(CodeIndexMountSignalV1::MountedFresh),
        CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Mounted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    );
    assert!(matches!(
        code_index_read(CodeIndexMountSignalV1::Indexing),
        CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Indexing,
            ..
        }
    ));
    assert!(matches!(
        code_index_read(CodeIndexMountSignalV1::Unmounted),
        CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Unmounted,
            ..
        }
    ));
    assert!(matches!(
        code_index_read(CodeIndexMountSignalV1::Incompatible),
        CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Incompatible,
            ..
        }
    ));
}

#[test]
fn synchronous_table_growth_is_bounded_by_observed_store_size() {
    use tracedecay_application::storage::{StorageTelemetryReadV1, StoreKeyV1, StoreSizeSampleV1};

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
fn synchronous_exhaustive_scans_are_bounded_before_work_starts() {
    let tmp = tempfile::TempDir::new().unwrap();
    let small = tmp.path().join("small");
    std::fs::create_dir(&small).unwrap();
    std::fs::write(small.join("payload"), b"small").unwrap();
    assert!(permits_synchronous_exhaustive_scan(&small));

    let large = tmp.path().join("large");
    std::fs::create_dir(&large).unwrap();
    std::fs::File::create(large.join("payload"))
        .unwrap()
        .set_len(MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES + 1)
        .unwrap();
    assert!(!permits_synchronous_exhaustive_scan(&large));
}

#[test]
fn synchronous_session_retention_includes_sqlite_sidecars() {
    let tmp = tempfile::TempDir::new().unwrap();
    let database = tmp.path().join("sessions.db");
    std::fs::write(&database, b"small").unwrap();
    assert!(permits_synchronous_session_retention_backlog(&database));

    std::fs::File::create(tmp.path().join("sessions.db-wal"))
        .unwrap()
        .set_len(MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES + 1)
        .unwrap();
    assert!(!permits_synchronous_session_retention_backlog(&database));
}

#[tokio::test]
async fn code_index_adapter_returns_seeded_read() {
    let ctx = context();
    let read = CodeIndexMountReadV1::Absent;
    let adapter = CodeIndexMountDoctorAdapterV1::from_read(read.clone());
    assert_eq!(adapter.code_index_mount(&ctx).await, read);
}

// --- Language-server and observability mappers ------------------------------

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
        crate::application::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("empty projection");
    assert_eq!(
        observability_read_from_model(Ok(model)),
        ObservabilityReadV1::Absent
    );
}

#[test]
fn retained_or_unreported_observation_history_is_not_absent() {
    let model =
        crate::application::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
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
        crate::application::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
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
        crate::application::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("active empty projection");
    active.coverage = crate::application::feedback::observations::Plan26CoverageV1::Known;
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

// --- Storage mapper ---------------------------------------------------------

#[test]
fn storage_family_read_absent_when_empty() {
    assert_eq!(
        storage_family_read(Vec::new()),
        DoctorStorageFamilyReadV1::Absent
    );
}

#[test]
fn storage_family_read_observed_when_findings_present() {
    let read = storage_family_read(vec![orphan_storage_finding()]);
    match read {
        DoctorStorageFamilyReadV1::Observed { findings } => {
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind(), DoctorStorageFindingKindV1::OrphanStore);
        }
        other => panic!("expected observed, got {other:?}"),
    }
}

#[tokio::test]
async fn storage_adapter_returns_seeded_read() {
    let ctx = context();
    let adapter = StorageDoctorAdapterV1::from_findings(vec![orphan_storage_finding()]);
    match adapter.storage_findings(&ctx).await {
        DoctorStorageFamilyReadV1::Observed { findings } => assert_eq!(findings.len(), 1),
        other => panic!("expected observed, got {other:?}"),
    }
}

// --- Composed report integration through the factory ------------------------

#[tokio::test]
async fn composed_report_carries_real_states_and_enumerates_coverage() {
    let ctx = context();
    let inputs = DoctorKernelInputsV1 {
        // Healthy configuration, degraded runtime, denied host, mounted index,
        // observed storage problem: a genuinely mixed report.
        configuration: configuration_read(ConfigurationAuthoritySignalV1::Pinned),
        runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: false,
            ..DaemonRuntimeHealthSignalV1::default()
        }),
        operational_audit: OperationalAuditReadV1 {
            profile_authority: ProfileAuthorityReadV1::Unavailable,
        },
        host: HostIntegrationReadV1::Denied,
        advisory_feedback: AdvisoryFeedbackReadV1::Absent,
        language_server: LanguageServerReadV1::Observed {
            state: LanguageServerStateV1::Ready,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        code_index: code_index_read(CodeIndexMountSignalV1::MountedFresh),
        observability: ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 7,
            last_observed_at_micros: Some(42),
            coverage: DoctorCoverageCompletenessV1::Partial,
        },
        storage: storage_family_read(vec![orphan_storage_finding()]),
    };

    let report = compose_doctor_report(&ctx, &inputs).await.expect("report");

    // Every one of the seven families appears in coverage, in stable order.
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

    // The storage entry carries its typed subclass; the report is not healthy
    // (denied host + degraded runtime), and bounded observability coverage
    // prevents a complete-coverage claim.
    assert!(!report.is_healthy_complete());
    assert_ne!(
        report.coverage().completeness(),
        DoctorCoverageCompletenessV1::Complete
    );

    // Both newly wired families were consulted; the denied host remains
    // explicitly unavailable.
    let consultation = |family: DoctorFindingFamilyV1| {
        report
            .coverage()
            .families()
            .iter()
            .find(|record| record.family() == family)
            .map(tracedecay_application::DoctorFamilyCoverageV1::consultation)
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
}

#[tokio::test]
async fn all_unknown_inputs_compose_a_truthful_unavailable_report() {
    let ctx = context();
    let report = compose_doctor_report(&ctx, &DoctorKernelInputsV1::all_unknown())
        .await
        .expect("report");
    assert!(!report.is_healthy_complete());
    // No family asserts health when every source is undetermined.
    assert!(
        report
            .findings()
            .all(|finding| !finding.state().is_healthy_complete())
    );
}

fn remediation_operation() -> crate::dashboard::DoctorRemediationOperationV1 {
    crate::dashboard::DoctorRemediationOperationV1 {
        operation_id: crate::application::operation_stream::OperationId::from_request(
            RequestId::new("request.doctor-remediation-observation").unwrap(),
        ),
        owning_operation: DoctorOwningOperationRefV1::new(
            tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY,
        )
        .unwrap(),
        phase: crate::dashboard::DoctorRemediationOperationPhaseV1::Partial,
        preview_id: None,
        execution: None,
        effect_receipt: None,
        owner_effect_receipt: None,
        owner_result_digest: None,
        verification: crate::dashboard::DoctorRemediationVerificationV1::Pending,
    }
}

#[tokio::test]
async fn remediation_reobservation_uses_fresh_doctor_evidence_not_dispatch_success() {
    let ctx = context();
    let mut before = DoctorKernelInputsV1::all_unknown();
    before.configuration = ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::Drifted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    };
    let finding_report = compose_doctor_report(&ctx, &before).await.unwrap();
    let finding = finding_report
        .entries()
        .iter()
        .find(|entry| entry.finding().family() == DoctorFindingFamilyV1::Configuration)
        .unwrap()
        .finding();
    assert_eq!(finding.state(), DoctorEvidenceStateV1::Degraded);
    assert_eq!(
        finding.remediation().unwrap().owning_operation().as_str(),
        tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY
    );
    assert!(matches!(
        verify_doctor_remediation_observation(&finding_report, &remediation_operation()).unwrap(),
        crate::dashboard::DoctorRemediationVerificationV1::Failed { .. }
    ));

    let mut after = DoctorKernelInputsV1::all_unknown();
    after.configuration = ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::InSync,
        coverage: DoctorCoverageCompletenessV1::Complete,
    };
    let recovered_report = compose_doctor_report(&ctx, &after).await.unwrap();
    assert!(matches!(
        verify_doctor_remediation_observation(&recovered_report, &remediation_operation()).unwrap(),
        crate::dashboard::DoctorRemediationVerificationV1::Verified { .. }
    ));
}

#[tokio::test]
async fn remediation_reobservation_preserves_partial_denied_and_unavailable_truth() {
    let ctx = context();
    let operation = remediation_operation();

    let mut partial = DoctorKernelInputsV1::all_unknown();
    partial.configuration = ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::InSync,
        coverage: DoctorCoverageCompletenessV1::Partial,
    };
    assert!(matches!(
        verify_doctor_remediation_observation(
            &compose_doctor_report(&ctx, &partial).await.unwrap(),
            &operation,
        )
        .unwrap(),
        crate::dashboard::DoctorRemediationVerificationV1::Partial { .. }
    ));

    let mut denied = DoctorKernelInputsV1::all_unknown();
    denied.configuration = ConfigurationAuthorityReadV1::Denied;
    assert_eq!(
        verify_doctor_remediation_observation(
            &compose_doctor_report(&ctx, &denied).await.unwrap(),
            &operation,
        )
        .unwrap(),
        crate::dashboard::DoctorRemediationVerificationV1::Denied
    );

    let unavailable = compose_doctor_report(&ctx, &DoctorKernelInputsV1::all_unknown())
        .await
        .unwrap();
    assert_eq!(
        verify_doctor_remediation_observation(&unavailable, &operation).unwrap(),
        crate::dashboard::DoctorRemediationVerificationV1::Unavailable
    );
}
