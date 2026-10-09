use std::collections::BTreeMap;
use std::time::SystemTime;

use super::*;
use tracedecay_session_temporal_store::{
    SessionTemporalAccess, SessionTemporalHealthFindingKind, SessionTemporalHealthStatus,
};

#[test]
fn domain_symbol_rules_warning_is_silent_without_the_file() {
    let project = tempfile::tempdir().expect("temp project root");
    assert_eq!(domain_symbol_rules_warning(project.path()), None);

    std::fs::create_dir_all(tracedecay_runtime_core::config::get_tracedecay_dir(
        project.path(),
    ))
    .expect("create project marker dir");
    assert_eq!(
        domain_symbol_rules_warning(project.path()),
        None,
        "an empty marker dir is not a rules file"
    );
}

#[test]
fn pr_autotrack_state_findings_name_stale_entries_and_blocking_state() {
    let data_root = tempfile::tempdir().expect("data root");
    assert_eq!(
        pr_autotrack_state_findings(data_root.path()),
        Ok(Vec::new())
    );

    let state_path = tracedecay_application::pr_tracking::state_path(data_root.path());
    std::fs::write(
        &state_path,
        r#"{"managed":{"tracedecay/autotrack/pr/8":{"pr":8,"head_branch":"legacy","worktree":"pr-worktrees/pr-8","tracking_ref":"refs/tracedecay/pr/8"}}}"#,
    )
    .expect("write stale state");
    let warnings = pr_autotrack_state_findings(data_root.path()).expect("stale entries warn");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("'tracedecay/autotrack/pr/8'"));
    assert!(warnings[0].contains("head_sha"));
    assert!(warnings[0].contains("drops this entry"));

    std::fs::write(&state_path, "{not json").expect("write malformed state");
    let failure =
        pr_autotrack_state_findings(data_root.path()).expect_err("malformed state is blocking");
    assert!(failure.contains(&state_path.display().to_string()));
    assert!(failure.contains("until that file is removed"));
}

#[test]
fn automation_effect_reset_findings_name_each_refused_journal_by_run_id() {
    let dashboard_root = tempfile::tempdir().expect("dashboard root");
    assert_eq!(
        automation_effect_reset_findings(dashboard_root.path()),
        Ok(Vec::new())
    );
    let journals = dashboard_root.path().join("automation_effects");
    std::fs::create_dir_all(&journals).expect("journal directory");
    let journal_file = |run_id: &str| {
        let run_id = tracedecay_domain::RunId::new(run_id).expect("run id");
        let key = tracedecay_domain::canonical_sha256(&(
            "tracedecay.automation-run.terminal-key.v1",
            &run_id,
        ))
        .expect("journal key");
        journals.join(format!(
            "{}.json",
            key.as_str().trim_start_matches("sha256:")
        ))
    };
    let legacy = br#"{"retirement":null,"admission":{"request":{"run_id":"run.legacy-memory"}}}"#;
    std::fs::write(journal_file("run.legacy-memory"), legacy).expect("legacy journal");
    // A journal whose admission names a run id that does not own its filename.
    let misfiled = journal_file("run.other");
    std::fs::write(&misfiled, legacy).expect("misfiled journal");
    std::fs::write(journals.join("pending-index.json"), b"{}").expect("index");

    let mut findings = automation_effect_reset_findings(dashboard_root.path()).expect("findings");
    findings.sort();
    assert_eq!(
        findings,
        vec![
            "Automation effect journal ".to_owned()
                + &misfiled.display().to_string()
                + " requires reset: its shape is refused (unknown field `retirement`, expected `admission` or `state` at line 1 column 13); project-open recovery removes it",
            "Automation run run.legacy-memory requires reset: its effect journal shape is refused (unknown field `retirement`, expected `admission` or `state` at line 1 column 13); project-open recovery removes the journal and the run id can run again".to_owned(),
        ]
    );
}

#[tokio::test]
async fn temporal_health_adapter_is_read_only_and_clean_on_canonical_schema() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal health adapter",
    )
    .await;
    let db = runtime.database();
    let db_path = db.db_path().to_path_buf();
    // Keep the byte-level assertion stable while diagnosis runs through the
    // retained registered reader pool.
    db.checkpoint_result().await.unwrap();
    let before = std::fs::read(&db_path).unwrap();
    let before_family = temporal_family_manifest(&db_path);

    let report = SessionTemporalAccess::new(db)
        .session_temporal_doctor_health()
        .await;

    let encoded = serde_json::to_value(report).unwrap();
    assert_eq!(encoded["status"], "complete");
    assert_eq!(encoded["findings"], serde_json::json!([]));
    assert!(encoded.get("reason").is_none());
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before,
        "temporal health diagnosis must not mutate the authoritative database"
    );
    assert_eq!(temporal_family_manifest(&db_path), before_family);
}

fn temporal_family_manifest(db_path: &Path) -> BTreeMap<String, (u64, Option<SystemTime>)> {
    let mut manifest = BTreeMap::new();
    for path in [
        db_path.to_path_buf(),
        {
            let mut wal = db_path.as_os_str().to_os_string();
            wal.push("-wal");
            PathBuf::from(wal)
        },
        {
            let mut shm = db_path.as_os_str().to_os_string();
            shm.push("-shm");
            PathBuf::from(shm)
        },
    ] {
        if let Ok(metadata) = std::fs::metadata(&path) {
            manifest.insert(
                path.file_name().unwrap().to_string_lossy().into_owned(),
                (metadata.len(), metadata.modified().ok()),
            );
        }
    }
    manifest
}

#[tokio::test]
async fn temporal_health_detects_index_and_column_migration_gaps() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal migration gap test",
    )
    .await;
    let db = runtime.database();
    let writer = db.writer_connection().unwrap();
    writer
        .execute(
            "DROP INDEX IF EXISTS idx_session_occurrences_generation_order",
            (),
        )
        .await
        .unwrap();
    writer
        .execute(
            "ALTER TABLE session_occurrences ADD COLUMN doctor_probe_column TEXT",
            (),
        )
        .await
        .unwrap();
    let report = serde_json::to_value(
        SessionTemporalAccess::new(db)
            .session_temporal_doctor_health()
            .await,
    )
    .unwrap();
    assert_eq!(report["status"], "partial");
    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|finding| {
            finding["kind"] == "migration_gap" && finding["count"].as_u64().unwrap_or(0) >= 2
        }),
        "{report}"
    );
}

fn storage_runtime_finding(
    state: tracedecay_contracts::doctor::DoctorEvidenceStateV1,
    reference: &str,
) -> tracedecay_contracts::doctor::DoctorFindingV1 {
    use tracedecay_contracts::doctor::{
        DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
        DoctorEvidenceReferenceV1, DoctorFindingFamilyV1, DoctorFindingV1,
    };

    DoctorFindingV1::new(
        DoctorFindingFamilyV1::StorageRuntime,
        state,
        vec![DoctorEvidenceRefV1::new(
            DoctorFindingFamilyV1::StorageRuntime,
            DoctorEvidenceReferenceV1::new(reference).unwrap(),
        )],
        DoctorCoverageStatementV1::new(
            DoctorCoverageCompletenessV1::Complete,
            "canonical storage runtime evidence",
        )
        .unwrap(),
    )
    .unwrap()
}

/// The rendered canonical findings are the only verdict: a degraded storage
/// runtime finding is one issue line, and the exit counts exactly that line.
#[test]
fn a_degraded_canonical_finding_is_the_one_issue_the_exit_counts() {
    use tracedecay_contracts::doctor::DoctorEvidenceStateV1 as State;

    let mut healthy = DoctorCounters::new();
    super::render_doctor_finding(
        &mut healthy,
        &storage_runtime_finding(State::HealthyCompleteCoverage, "runtime.healthy"),
    );
    assert_eq!(
        super::doctor_result(&healthy, false),
        super::DoctorCompletion::Healthy
    );

    let mut degraded = DoctorCounters::new();
    for (state, reference) in [
        (State::HealthyCompleteCoverage, "runtime.healthy"),
        (State::Degraded, "runtime.degraded"),
    ] {
        super::render_doctor_finding(&mut degraded, &storage_runtime_finding(state, reference));
    }
    assert_eq!(
        degraded.checks[1].message,
        "storage_runtime: canonical storage runtime evidence (runtime.degraded)"
    );
    assert_eq!(
        super::doctor_result(&degraded, false),
        super::DoctorCompletion::Issues(1)
    );
}

#[test]
fn denied_canonical_evidence_warns_instead_of_inventing_failure() {
    use tracedecay_contracts::doctor::DoctorEvidenceStateV1 as State;

    let mut counters = DoctorCounters::new();
    super::render_doctor_finding(
        &mut counters,
        &storage_runtime_finding(State::Denied, "runtime.denied"),
    );
    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn a_live_ingest_refusal_is_reported_informationally_and_never_fails_the_exit() {
    let finding = tracedecay_contracts::doctor::ingest_refusal_finding(
        &tracedecay_contracts::doctor::IngestRefusalCensusReadV1::Observed {
            refusals: vec![tracedecay_contracts::doctor::IngestRefusalV1 {
                provider: "cursor".to_owned(),
                session_id: "445777ad-0c9a-4c0e-bb98-7e8f7fb500ce".to_owned(),
                reason: "observation_identity_collision".to_owned(),
                start: 364_052,
                end: 364_900,
            }],
        },
    )
    .unwrap();

    let mut counters = DoctorCounters::new();
    super::render_doctor_finding(&mut counters, &finding);

    assert_eq!(
        (counters.issues, counters.warnings, counters.pending_actions),
        (0, 0, 0)
    );
    assert_eq!(
        counters.checks[0].message,
        "observability: durable ingest coverage converged past 1 refused source record(s), \
         informational, nothing needs doing: each was skipped by design and re-reading it would \
         refuse it again; cursor session 445777ad-0c9a-4c0e-bb98-7e8f7fb500ce range \
         364052..364900 observation_identity_collision (a different record with the same \
         identity is already retained) (observability.ingest-coverage.refused-informational)"
    );
    assert_eq!(
        super::doctor_result(&counters, false),
        super::DoctorCompletion::Healthy
    );
}

#[test]
fn canonical_doctor_unavailable_states_remain_typed_nonfatal_reads() {
    assert_eq!(
        super::canonical_daemon_doctor_report(&serde_json::json!({})).unwrap(),
        super::CanonicalDoctorReport::Unavailable
    );
    for kind in ["unknown", "unsupported"] {
        let status = serde_json::json!({
            "doctor_report": {
                "kind": kind,
                "table_growth_evidence": []
            }
        });
        assert_eq!(
            super::canonical_daemon_doctor_report(&status).unwrap(),
            super::CanonicalDoctorReport::Unavailable,
            "empty table-growth evidence is unavailable, never a pass"
        );
    }
}

/// Right after a daemon restart the project runtime that owns the report is
/// still mounting; the daemon says so, and Doctor names that state and its
/// wait remedy instead of an unexplained unknown.
#[test]
fn a_mounting_project_runtime_is_the_typed_mounting_state() {
    let status = serde_json::json!({
        "doctor_report": {
            "kind": "unknown",
            "reason": "doctor_report_owner_warming",
            "table_growth_evidence": []
        }
    });
    assert_eq!(
        super::canonical_daemon_doctor_report(&status).unwrap(),
        super::CanonicalDoctorReport::Mounting
    );
}

#[test]
fn canonical_doctor_rejects_unrecognized_wire_state() {
    let error = super::canonical_daemon_doctor_report(&serde_json::json!({
        "doctor_report": {
            "kind": "healthy",
            "table_growth_evidence": []
        }
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown typed state"));
}

#[test]
fn canonical_doctor_revalidates_observed_report_wire_contract() {
    let missing = super::canonical_daemon_doctor_report(&serde_json::json!({
        "doctor_report": { "kind": "observed" }
    }))
    .unwrap_err();
    assert!(missing.to_string().contains("omitted its report"));

    let invalid = super::canonical_daemon_doctor_report(&serde_json::json!({
        "doctor_report": {
            "kind": "observed",
            "report": {}
        }
    }))
    .unwrap_err();
    assert!(invalid.to_string().contains("violated its wire contract"));
}

/// A reachable daemon that has not admitted the project yet answers without a
/// `database` block. That is the warming state Doctor keeps polling, distinct
/// from an unreachable owner (an error) and from malformed telemetry (an error).
#[test]
fn daemon_runtime_parser_reports_missing_database_telemetry_as_pending() {
    let pending =
        super::daemon_runtime_status(serde_json::json!({"process": {"pid": 1234}})).unwrap();
    assert!(
        pending.is_none(),
        "absent telemetry is warming, not an error"
    );

    let published = serde_json::json!({
        "process": {"pid": 1234},
        "database": {"quick_check_ok": true},
        "doctor_report": {"kind": "unknown", "table_growth_evidence": []}
    });
    assert_eq!(
        super::daemon_runtime_status(published.clone()).unwrap(),
        Some(published)
    );

    let malformed =
        super::daemon_runtime_status(serde_json::json!({"process": {"pid": 1234}, "database": 7}))
            .unwrap_err();
    assert!(malformed.to_string().contains("was not an object"));
}

/// The sole daemon owner is the only authority that can observe storage
/// health, so Doctor losing it is an issue, not a warning: nothing else in the
/// run opened the store, and a zero exit reads as "checked and fine" to every
/// caller and CI gate. `doctor_result` already turns any issue into a non-zero
/// exit, so grading this `fail` is what makes an unavailable daemon fail closed.
#[test]
fn doctor_reports_a_discovery_blocked_daemon_without_recovery_guidance() {
    let path = std::path::Path::new("/Volumes/external/checkout");
    let blocked = tracedecay_domain::errors::TraceDecayError::project_route(
        crate::daemon::REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE,
        true,
        format!(
            "repository discovery for '{}' is deferred (DeadlineExceeded); repository discovery blocked on {}; retry after 2000ms",
            path.display(),
            path.display()
        ),
    );
    let message = super::daemon_warming_doctor_message(path, &blocked)
        .expect("discovery-blocked daemon is a warming report");
    assert!(
        message.contains(&format!(
            "daemon is still warming: repository discovery blocked on {}",
            path.display()
        )),
        "{message}"
    );
    assert!(
        !message.contains("daemon closed the connection")
            && !message.contains("Preserve this recovery set")
            && !message.contains("WAL:"),
        "{message}"
    );
    let mut counters = DoctorCounters::new();
    let findings = super::classify_daemon_status_error(
        &mut counters,
        std::path::Path::new("/Volumes/external/profile"),
        path,
        &blocked,
    );
    let super::DoctorDaemonFindingsV1::Unread { reason } = findings else {
        panic!("discovery-blocked warming must stay an unread report: {findings:?}");
    };
    assert_eq!(
        (reason, counters.issues, counters.warnings),
        ("daemon_warming", 0, 1)
    );

    let warming = tracedecay_domain::errors::TraceDecayError::project_route(
        crate::daemon::PROJECT_WARMING_REASON_CODE,
        true,
        "TraceDecay profile runtime is warming in the background; retry the same tool shortly",
    );
    let warming_message = super::daemon_warming_doctor_message(path, &warming)
        .expect("profile warming is a warming report");
    assert!(
        warming_message.contains("daemon is still warming: profile runtime is warming"),
        "{warming_message}"
    );
    assert!(
        !warming_message.contains("Preserve this recovery set")
            && !warming_message.contains("WAL:"),
        "{warming_message}"
    );

    let closed = tracedecay_domain::errors::TraceDecayError::Config {
        message: "daemon closed the connection after the tool request was sent but before returning a result; the outcome is unknown and the request was not retried".to_owned(),
    };
    assert!(
        super::daemon_warming_doctor_message(path, &closed).is_none(),
        "a closed connection is not a warming report"
    );
}

#[test]
fn unavailable_canonical_report_is_an_issue_that_fails_the_doctor_exit() {
    let mut counters = DoctorCounters::new();
    super::report_daemon_diagnostics_unavailable(
        &mut counters,
        None,
        &tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon socket is unavailable".to_string(),
        },
    );

    assert_eq!(counters.issues, 1, "an unobserved store is not a warning");
    assert_eq!(counters.warnings, 0);
    assert_eq!(
        super::doctor_result(&counters, true),
        super::DoctorCompletion::Issues(1)
    );
}

/// A store the daemon serves reset-required is the operator's pending
/// action: with no issue, Doctor completes pending rather than healthy.
#[test]
fn doctor_result_reports_a_pending_reset_without_issues_as_pending() {
    let counters = DoctorCounters::new();
    assert_eq!(
        super::doctor_result(&counters, true),
        super::DoctorCompletion::PendingOperatorAction
    );
}

#[test]
fn doctor_warns_for_intentionally_held_service_states_without_activation_advice() {
    use super::{DaemonServiceDoctorVerdict, daemon_service_doctor_verdict};
    use tracedecay_daemon_control::{DaemonProcessProofV1, DaemonServiceState};

    let unproven = DaemonProcessProofV1::Unproven {
        detail: "not probed".to_owned(),
    };
    for state in [
        DaemonServiceState::StoppedEnabled,
        DaemonServiceState::StoppedDisabled,
        DaemonServiceState::Masked,
    ] {
        assert_eq!(
            daemon_service_doctor_verdict(state, &unproven),
            DaemonServiceDoctorVerdict::Warn,
            "{state:?} may be an intentional hold and must be a Doctor warning"
        );
        let message = state.lifecycle_operator_advice();
        assert!(
            message.contains("intentional") && !message.contains("enable --now"),
            "{state:?} must preserve operator intent without enabling the service, got: {message}"
        );
    }

    let stopped_disabled = DaemonServiceState::StoppedDisabled.lifecycle_operator_advice();
    assert!(
        stopped_disabled.contains("stopped and disabled"),
        "stopped+disabled wording must be exact, got: {stopped_disabled}"
    );
    let stopped = DaemonServiceState::StoppedEnabled.lifecycle_operator_advice();
    assert!(
        stopped.contains("installed but stopped"),
        "stopped wording must be exact, got: {stopped}"
    );
    assert!(
        !stopped.contains("disabled"),
        "enabled-but-stopped must not claim disabled, got: {stopped}"
    );
}

#[test]
fn doctor_warns_on_missing_or_running_disabled_units() {
    use super::{
        DaemonServiceDoctorVerdict, daemon_service_doctor_message, daemon_service_doctor_verdict,
    };
    use tracedecay_daemon_control::{DaemonProcessProofV1, DaemonServiceState};

    let unproven = DaemonProcessProofV1::Unproven {
        detail: "initialize timed out".to_owned(),
    };
    let ready = DaemonProcessProofV1::Ready;
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::Missing, &unproven),
        DaemonServiceDoctorVerdict::Warn
    );
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::RunningDisabled, &ready),
        DaemonServiceDoctorVerdict::Warn
    );
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::RunningEnabled, &ready),
        DaemonServiceDoctorVerdict::Pass
    );
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::RunningEnabled, &unproven),
        DaemonServiceDoctorVerdict::Warn,
        "an active unit that did not answer initialize must not pass"
    );
    let active_without_process =
        daemon_service_doctor_message(DaemonServiceState::RunningEnabled, &unproven);
    assert!(
        active_without_process.contains("did not answer initialize"),
        "{active_without_process}"
    );
    assert!(
        !active_without_process.contains("enabled, and running"),
        "an unproven unit must not be reported as a running daemon: {active_without_process}"
    );
    let missing = DaemonServiceState::Missing.lifecycle_operator_advice();
    assert!(
        missing.contains("tracedecay daemon install-service"),
        "missing unit must name install-service, got: {missing}"
    );
    assert!(
        missing.contains("only if you want a managed daemon"),
        "missing-unit advice must make installation intentional, got: {missing}"
    );
}

#[test]
fn schema_convergence_severity_tracks_typed_state() {
    for (state, issues, warnings) in [
        (SchemaConvergenceStateV1::PendingSchemaMigration, 0, 1),
        (
            SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress,
            0,
            1,
        ),
        (SchemaConvergenceStateV1::Degraded, 1, 0),
        (SchemaConvergenceStateV1::Completed, 0, 0),
    ] {
        let status = serde_json::json!({"doctor_report": {"schema_convergences": [{
            "store": "profile-sessions", "stage": "registered_schema", "state": state,
            "progress": {"unit": "pages", "done": 4, "remaining": 7},
            "started_at_micros": 42, "degraded_row": "observation_id=obs-7"
        }]}});
        let mut counters = DoctorCounters::new();
        render_schema_convergences(&mut counters, &status).unwrap();
        assert_eq!((counters.issues, counters.warnings), (issues, warnings));
    }
    assert!(
        render_schema_convergences(
            &mut DoctorCounters::new(),
            &serde_json::json!({"doctor_report": {"schema_convergences": [{}]}})
        )
        .is_err()
    );
}

#[test]
fn project_open_severity_tracks_typed_state() {
    for (state, reason, issues, warnings) in [
        (
            ProjectOpenStatusStateV1::Completed,
            ProjectOpenStatusReasonV1::Ready,
            0,
            0,
        ),
        (
            ProjectOpenStatusStateV1::Converging,
            ProjectOpenStatusReasonV1::Converging,
            0,
            1,
        ),
        (
            ProjectOpenStatusStateV1::Stalled,
            ProjectOpenStatusReasonV1::UnrepairableVerdict,
            1,
            0,
        ),
    ] {
        let status = serde_json::json!({"project_open": {
            "state": state, "reason": reason, "retry_after_ms": null,
            "detail": "project authority verdict"
        }});
        let mut counters = DoctorCounters::new();
        render_project_open_status(&mut counters, &status).unwrap();
        assert_eq!((counters.issues, counters.warnings), (issues, warnings));
    }
}

#[tokio::test]
async fn temporal_health_does_not_block_on_a_saturated_general_reader_lane() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal health concurrent reader",
    )
    .await;
    let db = runtime.database();
    let occupancy = db
        .read_connection()
        .reader_pool_occupancy()
        .expect("reader pool occupancy");
    let mut held = Vec::new();
    for _ in 0..occupancy.available_general {
        held.push(
            db.read_snapshot()
                .await
                .expect("hold a general-lane snapshot"),
        );
    }
    assert_eq!(
        db.read_connection()
            .reader_pool_occupancy()
            .map(|snapshot| snapshot.available_general),
        Some(0),
        "the general lane must stay leased for the whole diagnosis"
    );

    let report = SessionTemporalAccess::new(db)
        .session_temporal_doctor_health()
        .await;

    assert_eq!(
        report.status(),
        SessionTemporalHealthStatus::Complete,
        "{report:?}"
    );
    assert!(
        !report.findings().iter().any(|finding| {
            finding.kind() == SessionTemporalHealthFindingKind::RelationGraphUnavailable
        }),
        "relation health must use the reserved health snapshot, not the general lane: {report:?}"
    );
    drop(held);
}

static NETWORK_PROBE_STARTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn rendezvous_network_probe() {
    use std::sync::atomic::Ordering;

    NETWORK_PROBE_STARTED.fetch_add(1, Ordering::SeqCst);
    while NETWORK_PROBE_STARTED.load(Ordering::SeqCst) < 2 {
        std::thread::yield_now();
    }
}

fn overlapping_worldwide_total() -> Option<u64> {
    rendezvous_network_probe();
    Some(7)
}

fn overlapping_latest_version()
-> Result<String, tracedecay_dashboard_api::cloud::ReleaseLookupError> {
    rendezvous_network_probe();
    Ok("1.2.3".to_owned())
}

#[test]
fn independent_network_probes_overlap() {
    use std::sync::atomic::Ordering;

    NETWORK_PROBE_STARTED.store(0, Ordering::SeqCst);
    let mut counters = DoctorCounters::quiet();
    check_network(
        &mut counters,
        Ok(&UploadSetting::Resolved(true)),
        AdmittedDoctorNetworkProbes {
            fetch_worldwide_total: overlapping_worldwide_total,
            fetch_latest_version: overlapping_latest_version,
        },
    );
    assert!(
        counters.checks.iter().any(|check| check
            .message
            .contains("Worldwide counter reachable (total: 7)")),
        "{:?}",
        counters.checks
    );
    assert!(
        counters.checks.iter().any(|check| check
            .message
            .contains("GitHub releases API reachable (latest v1.2.3)")),
        "{:?}",
        counters.checks
    );
}
