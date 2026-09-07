//! Daemon-owned Doctor signal-mapper tests.
//!
//! Adapter structs, [`DaemonRuntimeHealthSignalV1`], and
//! [`compose_doctor_report`] live in `tracedecay-application::doctor` and are
//! covered there. This module keeps the mappers that still read daemon,
//! global-db, LSP, and host-bundle types.

use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, HostConformanceV1, HostIntegrationReadV1,
    IngestRefusalCensusReadV1, IngestRefusalCountV1, LanguageServerReadV1, LanguageServerStateV1,
    ObservabilityReadV1, ObservabilityStateV1,
};
use tracedecay_application::{
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
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("empty projection");
    assert_eq!(
        observability_read_from_model(Ok(model)),
        ObservabilityReadV1::Absent
    );
}

#[test]
fn retained_or_unreported_observation_history_is_not_absent() {
    let model =
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
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
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1::project_with_accounting(
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
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1::project(&[])
            .expect("active empty projection");
    active.coverage = tracedecay_application::feedback::observations::FeedbackCoverageV1::Known;
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

#[test]
fn refusal_censuses_merge_by_provider_and_reason() {
    use tracedecay_global_db::observation::{
        ObservationRefusalCensusV1, ObservationRefusalCountV1,
    };

    let merged = ingest_refusal_read_from_censuses(&[
        ObservationRefusalCensusV1::Observed {
            refusals: vec![ObservationRefusalCountV1 {
                provider: "cursor".to_owned(),
                reason: "admission_refused".to_owned(),
                count: 100,
            }],
        },
        ObservationRefusalCensusV1::Observed {
            refusals: vec![
                ObservationRefusalCountV1 {
                    provider: "cursor".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 60,
                },
                ObservationRefusalCountV1 {
                    provider: "codex".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 27,
                },
            ],
        },
    ]);

    assert_eq!(
        merged,
        IngestRefusalCensusReadV1::Observed {
            refusals: vec![
                IngestRefusalCountV1 {
                    provider: "codex".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 27,
                },
                IngestRefusalCountV1 {
                    provider: "cursor".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 160,
                },
            ],
        }
    );
}

#[test]
fn one_unavailable_refusal_census_makes_the_merged_read_unknown() {
    use tracedecay_global_db::observation::{
        ObservationRefusalCensusV1, ObservationRefusalCountV1,
    };

    let merged = ingest_refusal_read_from_censuses(&[
        ObservationRefusalCensusV1::Observed {
            refusals: vec![ObservationRefusalCountV1 {
                provider: "cursor".to_owned(),
                reason: "admission_refused".to_owned(),
                count: 1,
            }],
        },
        ObservationRefusalCensusV1::Unavailable,
    ]);

    assert_eq!(merged, IngestRefusalCensusReadV1::Unknown);
}
