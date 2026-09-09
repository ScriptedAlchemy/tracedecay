//! Daemon-owned Doctor signal-mapper tests.
//!
//! Adapter structs, [`DaemonRuntimeHealthSignalV1`], and
//! [`compose_doctor_report`] live in `tracedecay-contracts::doctor` and are
//! covered there. This module keeps the mappers that still read daemon,
//! global-db, LSP, and host-bundle types.

use tracedecay_contracts::doctor::{
    DoctorCoverageCompletenessV1, HostConformanceV1, HostIntegrationReadV1, LanguageServerReadV1,
    LanguageServerStateV1, ObservabilityReadV1, ObservabilityStateV1,
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
    tracedecay_store_runtime::register_registered_schema_installer();
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
        tracedecay_agent_hosts::agents::host_bundle_v2::HostBundleDoctorReportV1::default();
    assert_eq!(
        host_integration_read_from_report(&checked_in),
        HostIntegrationReadV1::Absent
    );

    let mut drifted = checked_in;
    drifted.components.push(
        tracedecay_agent_hosts::agents::host_bundle_v2::HostBundleComponentDoctorResultV1 {
            receipt_path: std::path::PathBuf::from("receipt.fixture.json"),
            host: Some(tracedecay_agent_hosts::agents::host_bundle_v2::HostKindV1::Codex),
            component: Some(tracedecay_agent_hosts::agents::host_bundle_v2::HostBundleComponentV1::Core),
            state: tracedecay_agent_hosts::agents::host_bundle_v2::HostBundleComponentDoctorStateV1::Repairable,
            registration: Some(
                tracedecay_agent_hosts::agents::host_bundle_v2::HostBundleRegistrationStateV1::Repairable,
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
