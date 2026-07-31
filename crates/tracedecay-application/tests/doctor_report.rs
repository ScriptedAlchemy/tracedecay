//! Doctor kernel composition, coverage, remediation resolution, and regression behavior.
//!
//! These drive the real composition entry point over seeded source ports with
//! mixed healthy/degraded/unavailable families, assert the coverage statement is
//! truthful, resolve remediation references (including unknown-ref rejection),
//! and exercise every finding family supported by the kernel.

mod common;

use std::future::Future;
use std::task::{Context, Poll, Waker};

use tracedecay_application::doctor::operations;
use tracedecay_application::{
    CodeIndexMountDoctorPort, CodeIndexMountReadV1, CodeIndexMountStateV1,
    ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1, ConfigurationDriftV1,
    DoctorCoverageCompletenessV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
    DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorRemediationKindV1,
    DoctorRemediationRefV1, DoctorRemediationRegistryV1, DoctorRemediationResolutionErrorV1,
    DoctorReportComposerV1, DoctorSourceFuture, DoctorStorageFamilyReadV1,
    DoctorStorageFindingKindV1, HostConformanceV1, HostIntegrationDoctorPort,
    HostIntegrationReadV1, OperationalAuditDoctorPort, OperationalAuditReadV1, OrphanStoreRecordV1,
    ProfileAuthorityReadV1, RemoteAuthorityReadV1, RemoteListenerReadV1, RemoteOperationalReadV1,
    RequestContext, RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1,
    StorageByteSizeV1, StorageDoctorPort, StoreKeyV1, orphan_store_finding,
};
use tracedecay_domain::UtcMicros;

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("doctor fixture futures must complete immediately"),
    }
}

// --- Seeded static source ports ---------------------------------------------

struct StaticConfiguration(ConfigurationAuthorityReadV1);
impl ConfigurationAuthorityDoctorPort for StaticConfiguration {
    fn configuration_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, ConfigurationAuthorityReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

struct StaticRuntime(RuntimeHealthReadV1);
impl RuntimeHealthDoctorPort for StaticRuntime {
    fn runtime_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, RuntimeHealthReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

struct StaticHost(HostIntegrationReadV1);
impl HostIntegrationDoctorPort for StaticHost {
    fn host_conformance<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, HostIntegrationReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

struct StaticCodeIndex(CodeIndexMountReadV1);
impl CodeIndexMountDoctorPort for StaticCodeIndex {
    fn code_index_mount<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, CodeIndexMountReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

struct StaticStorage(DoctorStorageFamilyReadV1);
impl StorageDoctorPort for StaticStorage {
    fn storage_findings<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, DoctorStorageFamilyReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

struct StaticOperationalAudit(OperationalAuditReadV1);
impl OperationalAuditDoctorPort for StaticOperationalAudit {
    fn operational_audit<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, OperationalAuditReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

fn context() -> RequestContext {
    common::context(&common::operation())
}

fn orphan_storage_read() -> DoctorStorageFamilyReadV1 {
    let record = OrphanStoreRecordV1 {
        store: StoreKeyV1::new("sessions.db").expect("store"),
        identity_resolves: false,
        size_bytes: StorageByteSizeV1(41_000_000_000),
        first_unresolved_at: UtcMicros(100),
        observed_at: UtcMicros(1_000),
    };
    let finding =
        orphan_store_finding(&record, DoctorCoverageCompletenessV1::Complete).expect("finding");
    DoctorStorageFamilyReadV1::Observed {
        findings: vec![finding],
    }
}

// --- Composition -------------------------------------------------------------

#[test]
fn doctor_report_composes_all_families_from_mixed_sources() {
    let ctx = context();
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::Drifted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let runtime = StaticRuntime(RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let host = StaticHost(HostIntegrationReadV1::Unsupported);
    let code_index = StaticCodeIndex(CodeIndexMountReadV1::Observed {
        state: CodeIndexMountStateV1::Stale,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let storage = StaticStorage(orphan_storage_read());

    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .with_runtime(&runtime)
            .with_host(&host)
            .with_code_index(&code_index)
            .with_storage(&storage)
            .compose(&ctx),
    )
    .expect("compose");

    // Every required family is represented; nothing is silently omitted, and
    // findings are never merged (each family contributes at least one).
    let families: Vec<DoctorFindingFamilyV1> = report
        .coverage()
        .families()
        .iter()
        .map(|c| c.family())
        .collect();
    for family in [
        DoctorFindingFamilyV1::Advisory,
        DoctorFindingFamilyV1::Configuration,
        DoctorFindingFamilyV1::StorageRuntime,
        DoctorFindingFamilyV1::Storage,
        DoctorFindingFamilyV1::LanguageServer,
        DoctorFindingFamilyV1::SemanticIndex,
        DoctorFindingFamilyV1::Observability,
    ] {
        assert!(families.contains(&family), "family {family:?} missing");
        assert!(
            report.findings().any(|f| f.family() == family),
            "no finding for {family:?}"
        );
    }

    // The storage entry preserves its typed subclass by value.
    let storage_entry = report
        .entries()
        .iter()
        .find(|e| e.finding().family() == DoctorFindingFamilyV1::Storage)
        .expect("storage entry");
    assert_eq!(
        storage_entry.storage_kind(),
        Some(DoctorStorageFindingKindV1::OrphanStore)
    );

    // A mixed report with an unsupported host and two unwired families is not
    // healthy and not complete.
    assert!(!report.is_healthy_complete());
    assert_eq!(
        report.coverage().completeness(),
        DoctorCoverageCompletenessV1::Partial
    );
}

#[test]
fn doctor_report_coverage_statement_is_truthful_about_unavailable_families() {
    let ctx = context();
    // Only configuration is wired; every other family is unwired or unavailable.
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::InSync,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .compose(&ctx),
    )
    .expect("compose");

    // Configuration consulted; all others unavailable (unwired).
    let consulted: Vec<DoctorFindingFamilyV1> = report
        .coverage()
        .families()
        .iter()
        .filter(|c| matches!(c.consultation(), DoctorFamilyConsultationV1::Consulted))
        .map(|c| c.family())
        .collect();
    assert_eq!(consulted, vec![DoctorFindingFamilyV1::Configuration]);

    for record in report.coverage().families() {
        if record.family() != DoctorFindingFamilyV1::Configuration {
            assert_eq!(
                record.consultation(),
                DoctorFamilyConsultationV1::Unavailable {
                    reason: DoctorFamilyUnavailableReasonV1::Unwired
                },
                "family {:?} should be unwired",
                record.family()
            );
        }
    }

    let statement = report.coverage().statement().statement();
    assert!(
        statement.contains("consulted 1/7"),
        "statement: {statement}"
    );
    assert!(statement.contains("unavailable"), "statement: {statement}");
    assert!(
        statement.contains("language_server(unwired)"),
        "statement: {statement}"
    );
    // The unwired families carry a truthful non-healthy evidence state.
    let advisory = report
        .findings()
        .find(|f| f.family() == DoctorFindingFamilyV1::Advisory)
        .expect("advisory finding");
    assert_eq!(advisory.state(), DoctorEvidenceStateV1::Unsupported);
    assert!(!advisory.state().is_healthy_complete());
}

#[test]
fn doctor_report_exposes_remote_and_profile_authority_truth_without_replacing_runtime_health() {
    let ctx = context();
    let runtime = StaticRuntime(RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let audit = StaticOperationalAudit(OperationalAuditReadV1 {
        remote: RemoteOperationalReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            authority: RemoteAuthorityReadV1::Available,
            pending_spool_items: 2,
            quarantined_spool_items: 1,
            replay_coverage_complete: false,
            backup_verified: true,
            failover_in_progress: false,
            recovery_required: true,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        profile_authority: ProfileAuthorityReadV1::Observed {
            registry_attached: true,
            profile_sessions_attached: true,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
    });

    let report = block_on(
        DoctorReportComposerV1::new()
            .with_runtime(&runtime)
            .with_operational_audit(&audit)
            .compose(&ctx),
    )
    .expect("compose");
    let runtime_codes = report
        .findings()
        .filter(|finding| finding.family() == DoctorFindingFamilyV1::StorageRuntime)
        .map(|finding| finding.evidence()[0].reference().as_str())
        .collect::<Vec<_>>();

    assert!(runtime_codes.contains(&"runtime.health.healthy"));
    assert!(runtime_codes.contains(&"remote.operational.recovery-required"));
    assert!(runtime_codes.contains(&"profile.authority.registered"));
    assert!(
        report
            .findings()
            .find(|finding| {
                finding.evidence()[0].reference().as_str() == "remote.operational.recovery-required"
            })
            .is_some_and(|finding| finding.state() == DoctorEvidenceStateV1::Degraded)
    );
}

#[test]
fn optional_remote_capability_preserves_unconfigured_and_unsupported_truth() {
    let ctx = context();
    for (read, expected_code) in [
        (
            RemoteOperationalReadV1::Unconfigured,
            "remote.operational.unconfigured",
        ),
        (
            RemoteOperationalReadV1::Unsupported,
            "remote.operational.unsupported",
        ),
    ] {
        let audit = StaticOperationalAudit(OperationalAuditReadV1 {
            remote: read,
            profile_authority: ProfileAuthorityReadV1::Unavailable,
        });
        let report = block_on(
            DoctorReportComposerV1::new()
                .with_operational_audit(&audit)
                .compose(&ctx),
        )
        .expect("compose");
        assert!(
            report
                .findings()
                .any(|finding| finding.evidence()[0].reference().as_str() == expected_code),
            "{expected_code} must remain explicit"
        );
        assert!(report.findings().any(|finding| {
            finding.evidence()[0].reference().as_str() == "profile.authority.unavailable"
        }));
    }
}

#[test]
fn doctor_report_healthy_only_under_genuinely_complete_coverage() {
    let ctx = context();
    // Every family wired and observed healthy with complete coverage.
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::InSync,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let runtime = StaticRuntime(RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let host = StaticHost(HostIntegrationReadV1::Observed {
        conformance: HostConformanceV1::Conformant,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let code_index = StaticCodeIndex(CodeIndexMountReadV1::Observed {
        state: CodeIndexMountStateV1::Mounted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    // Storage: a clean, within-budget observation is healthy.
    let clean_record = OrphanStoreRecordV1 {
        store: StoreKeyV1::new("sessions.db").expect("store"),
        identity_resolves: true,
        size_bytes: StorageByteSizeV1(1_000),
        first_unresolved_at: UtcMicros(100),
        observed_at: UtcMicros(1_000),
    };
    let storage = StaticStorage(DoctorStorageFamilyReadV1::Observed {
        findings: vec![
            orphan_store_finding(&clean_record, DoctorCoverageCompletenessV1::Complete)
                .expect("finding"),
        ],
    });

    // LanguageServer and Observability have no wired ports, so the report cannot
    // be complete: an unwired family keeps the report honest.
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .with_runtime(&runtime)
            .with_host(&host)
            .with_code_index(&code_index)
            .with_storage(&storage)
            .compose(&ctx),
    )
    .expect("compose");
    assert!(
        !report.is_healthy_complete(),
        "unwired families must prevent a healthy-complete report"
    );
    assert_eq!(
        report.coverage().completeness(),
        DoctorCoverageCompletenessV1::Partial
    );

    // Each individually consulted family, however, is healthy-complete.
    for family in [
        DoctorFindingFamilyV1::Configuration,
        DoctorFindingFamilyV1::StorageRuntime,
        DoctorFindingFamilyV1::Advisory,
        DoctorFindingFamilyV1::SemanticIndex,
        DoctorFindingFamilyV1::Storage,
    ] {
        let finding = report
            .findings()
            .find(|f| f.family() == family)
            .expect("finding");
        assert!(
            finding.state().is_healthy_complete(),
            "family {family:?} should be healthy-complete"
        );
    }
}

// --- Remediation resolution --------------------------------------------------

#[test]
fn doctor_report_remediation_references_resolve_against_default_registry() {
    let ctx = context();
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::Drifted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let storage = StaticStorage(orphan_storage_read());
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .with_storage(&storage)
            .compose(&ctx),
    )
    .expect("compose");

    let registry = DoctorRemediationRegistryV1::default_registry();
    // Every remediation reference the composed report carries resolves to a
    // typed descriptor without executing anything.
    let mut resolved = 0usize;
    for finding in report.findings() {
        if let Some(reference) = finding.remediation() {
            let descriptor = registry.resolve(reference).expect("remediation resolves");
            assert!(!descriptor.summary().is_empty());
            assert_eq!(descriptor.operation(), reference.owning_operation());
            resolved += 1;
        }
    }
    assert!(resolved >= 2, "expected drift + orphan remediation refs");
}

#[test]
fn doctor_remediation_registry_rejects_unknown_reference() {
    let registry = DoctorRemediationRegistryV1::default_registry();
    let unknown = DoctorRemediationRefV1::new(
        tracedecay_application::DoctorOwningOperationRefV1::new(
            "use-case.application.unknown.made-up",
        )
        .expect("operation"),
        DoctorRemediationKindV1::Action,
    );
    let error = registry
        .resolve(&unknown)
        .expect_err("unknown reference rejected");
    assert_eq!(
        error,
        DoctorRemediationResolutionErrorV1::UnknownOperation {
            operation: "use-case.application.unknown.made-up".to_string(),
        }
    );
}

// --- Plan 14 PR14 regression families ---------------------------------------

// Plan 14 "finding to verified remediation" enumerates observable classes the
// Doctor kernel must represent. Each case below maps one class onto the landed
// contract. Classes owned by transport/dashboard (deep-link scope, SSE churn,
// renderer fallback) or by not-yet-wired advisory sub-sources (GitHub item
// lifecycle, CI provenance, proximity) are noted in the crate report as
// unmapped-in-this-slice rather than asserted here.

#[test]
fn doctor_regression_unavailable_and_drift_families_are_distinct_states() {
    let ctx = context();
    // Executable absent, protocol drift, configuration drift, stuck runtime,
    // unmounted index, denied storage — each a distinct visible state.
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Resolved {
        drift: ConfigurationDriftV1::Drifted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let runtime = StaticRuntime(RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Stuck,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let host = StaticHost(HostIntegrationReadV1::Observed {
        conformance: HostConformanceV1::ExecutableAbsent,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let code_index = StaticCodeIndex(CodeIndexMountReadV1::Observed {
        state: CodeIndexMountStateV1::Unmounted,
        coverage: DoctorCoverageCompletenessV1::Complete,
    });
    let storage = StaticStorage(DoctorStorageFamilyReadV1::Denied);

    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .with_runtime(&runtime)
            .with_host(&host)
            .with_code_index(&code_index)
            .with_storage(&storage)
            .compose(&ctx),
    )
    .expect("compose");

    let state = |family: DoctorFindingFamilyV1| {
        report
            .findings()
            .find(|f| f.family() == family)
            .expect("finding")
            .state()
    };
    // Configuration drift and executable-absent are degraded-but-observed;
    // stuck runtime degraded; unmounted index degraded; denied storage denied.
    assert_eq!(
        state(DoctorFindingFamilyV1::Configuration),
        DoctorEvidenceStateV1::Degraded
    );
    assert_eq!(
        state(DoctorFindingFamilyV1::Advisory),
        DoctorEvidenceStateV1::Degraded
    );
    assert_eq!(
        state(DoctorFindingFamilyV1::StorageRuntime),
        DoctorEvidenceStateV1::Degraded
    );
    assert_eq!(
        state(DoctorFindingFamilyV1::SemanticIndex),
        DoctorEvidenceStateV1::Degraded
    );
    assert_eq!(
        state(DoctorFindingFamilyV1::Storage),
        DoctorEvidenceStateV1::Denied
    );
    // A denied storage family is unavailable in coverage, never a clean zero.
    let storage_cov = report
        .coverage()
        .families()
        .iter()
        .find(|c| c.family() == DoctorFindingFamilyV1::Storage)
        .expect("storage coverage");
    assert_eq!(
        storage_cov.consultation(),
        DoctorFamilyConsultationV1::Unavailable {
            reason: DoctorFamilyUnavailableReasonV1::Denied
        }
    );
    assert!(!report.is_healthy_complete());
}

#[test]
fn doctor_regression_incomplete_telemetry_never_becomes_healthy() {
    let ctx = context();
    // Partial coverage on an otherwise-healthy runtime must not read healthy.
    let runtime = StaticRuntime(RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage: DoctorCoverageCompletenessV1::Partial,
    });
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_runtime(&runtime)
            .compose(&ctx),
    )
    .expect("compose");
    let runtime_finding = report
        .findings()
        .find(|f| f.family() == DoctorFindingFamilyV1::StorageRuntime)
        .expect("runtime finding");
    assert_eq!(runtime_finding.state(), DoctorEvidenceStateV1::Partial);
    assert!(!runtime_finding.state().is_healthy_complete());
}

#[test]
fn doctor_regression_unauthorized_read_maps_to_denied_not_absent() {
    let ctx = context();
    let configuration = StaticConfiguration(ConfigurationAuthorityReadV1::Denied);
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_configuration(&configuration)
            .compose(&ctx),
    )
    .expect("compose");
    let finding = report
        .findings()
        .find(|f| f.family() == DoctorFindingFamilyV1::Configuration)
        .expect("configuration finding");
    assert_eq!(finding.state(), DoctorEvidenceStateV1::Denied);
    assert!(finding.remediation().is_none());
}

#[test]
fn doctor_regression_preview_reference_resolution_is_kind_checked() {
    // A remediation reference offered as a preview must resolve only when the
    // owning operation actually offers one; daemon recovery offers none.
    let registry = DoctorRemediationRegistryV1::default_registry();
    let preview = DoctorRemediationRefV1::new(
        tracedecay_application::DoctorOwningOperationRefV1::new(operations::RUNTIME_RECOVER_DAEMON)
            .expect("operation"),
        DoctorRemediationKindV1::Preview,
    );
    assert!(matches!(
        registry.resolve(&preview),
        Err(DoctorRemediationResolutionErrorV1::PreviewUnavailable { .. })
    ));
    // Configuration apply offers a preview.
    let config_preview = DoctorRemediationRefV1::new(
        tracedecay_application::DoctorOwningOperationRefV1::new(
            operations::CONFIGURATION_PROTECTED_APPLY,
        )
        .expect("operation"),
        DoctorRemediationKindV1::Preview,
    );
    assert!(registry.resolve(&config_preview).is_ok());
}
