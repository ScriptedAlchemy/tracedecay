use std::collections::BTreeMap;
use std::time::SystemTime;

use super::*;
use crate::agents::AgentIntegration;
use tracedecay_runtime_core::text::format_bytes;

#[test]
fn supported_optional_host_absences_reach_doctor_without_host_directories() {
    let home = tempfile::tempdir().expect("isolated home");
    let reported = agents::all_integrations()
        .into_iter()
        .filter(|agent| should_run_host_healthcheck(agent.as_ref(), home.path()))
        .map(|agent| agent.id())
        .collect::<std::collections::BTreeSet<_>>();

    // Every host whose integration sets `reports_absence_to_doctor()`. Adding a
    // host here is a deliberate product decision: an absent optional host stays
    // an informational Doctor warning, and every other absent host stays quiet.
    assert_eq!(
        reported,
        std::collections::BTreeSet::from(["antigravity", "devin", "kimi", "kiro", "vibe", "zed"]),
        "supported optional-host absences must remain visible while unrelated absent hosts stay quiet"
    );

    let context = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: home.path().to_path_buf(),
    };
    let mut counters = DoctorCounters::new();
    for agent in agents::all_integrations()
        .into_iter()
        .filter(|agent| should_run_host_healthcheck(agent.as_ref(), home.path()))
    {
        agent.healthcheck(&mut counters, &context);
    }
    assert_eq!(
        counters.issues, 0,
        "an absent optional host is a truthful Doctor warning, not a broken installation"
    );
    // One warning per absent host, plus one extra each for the two hosts that
    // register two documents: Antigravity (IDE config and CLI plugin) and Vibe
    // (MCP config and prompt rules).
    assert_eq!(counters.warnings, 8);
}

#[test]
fn detected_kiro_without_a_tracedecay_registration_is_optional_absence() {
    let home = tempfile::tempdir().expect("isolated Kiro home");
    let mcp_config = home.path().join(".kiro/settings/mcp.json");
    std::fs::create_dir_all(mcp_config.parent().expect("Kiro settings parent"))
        .expect("create Kiro settings");
    std::fs::write(
        &mcp_config,
        br#"{"mcpServers":{"operator":{"command":"other"}}}"#,
    )
    .expect("write operator-owned Kiro config");

    let kiro = agents::KiroIntegration;
    assert!(
        should_run_host_healthcheck(&kiro, home.path()),
        "Kiro remains a visible optional host"
    );

    let mut counters = DoctorCounters::new();
    kiro.healthcheck(
        &mut counters,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn domain_symbol_rules_warning_is_silent_without_the_file() {
    let project = tempfile::tempdir().expect("temp project root");
    assert_eq!(domain_symbol_rules_warning(project.path()), None);

    std::fs::create_dir_all(crate::config::get_tracedecay_dir(project.path()))
        .expect("create project marker dir");
    assert_eq!(
        domain_symbol_rules_warning(project.path()),
        None,
        "an empty marker dir is not a rules file"
    );
}

#[test]
fn domain_symbol_rules_warning_names_the_unread_file() {
    let project = tempfile::tempdir().expect("temp project root");
    let marker_dir = crate::config::get_tracedecay_dir(project.path());
    std::fs::create_dir_all(&marker_dir).expect("create project marker dir");
    let rules = marker_dir.join(DOMAIN_SYMBOL_RULES_FILENAME);
    std::fs::write(&rules, "[[rule]]\nname = \"elisp\"\n").expect("write rules file");

    let warning = domain_symbol_rules_warning(project.path()).expect("rules file must be reported");
    assert!(
        warning.contains(&rules.display().to_string()),
        "warning must name the file: {warning}"
    );
    assert!(
        warning.contains("docs/DOMAIN-EXTRACTORS.md"),
        "warning must point at the doc: {warning}"
    );
}

#[test]
fn format_bytes_boundaries() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1023), "1023 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1536), "1.5 KB");
    assert_eq!(format_bytes(1024 * 1024 - 1), "1024.0 KB");
    assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 512), "512.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    assert_eq!(format_bytes(1024 * 1024 * 1024 * 2), "2.0 GB");
}

#[test]
fn format_bytes_fractional_kb() {
    // 2048 bytes = 2.0 KB
    assert_eq!(format_bytes(2048), "2.0 KB");
    // 1536 = 1.5 KB
    assert_eq!(format_bytes(1536), "1.5 KB");
}

#[test]
fn database_recovery_guidance_names_the_preserved_recovery_set() {
    let db_path = PathBuf::from("/profile/projects/proj_test/tracedecay.db");
    let guidance = database_recovery_guidance(&db_path);

    for path in [
        db_path.clone(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
        PathBuf::from(format!("{}.dirty", db_path.display())),
        db_path.parent().unwrap().join("dirty"),
    ] {
        assert!(guidance.contains(&path.display().to_string()));
    }
    assert!(guidance.contains("stop all TraceDecay daemon and MCP processes"));
    assert!(
        guidance.contains(
            "Do not run `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe`"
        )
    );
    assert!(guidance.contains("`sessions.db` is separate and must not be removed"));
    assert!(guidance.contains("Facts are stored in the graph database"));
    assert!(guidance.contains("automatic default-store rebuild is intentionally blocked"));
}

#[test]
fn daemon_runtime_parser_extracts_storage_health_and_owner() {
    let parsed = super::daemon_runtime_status(&serde_json::json!({
        "content": [
            {"type": "text", "text": "daemon notice"},
            {
                "type": "text",
                "text": r#"{"tracedecay_version":"0.0.66","process":{"pid":1234},"database":{"canonical_db_path":"/tmp/project.db","quick_check_ok":true,"authority_audit_ok":true,"authority_audit_error":null,"dirty_marker":{"exists":false}},"doctor_report":{"kind":"unknown","table_growth_evidence":[]}}"#
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        parsed.pointer("/storage_health/quick_check_ok"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        parsed.pointer("/storage_health/daemon_owner_pid"),
        Some(&serde_json::json!(1234))
    );
    assert_eq!(
        parsed.pointer("/storage_health/daemon_version"),
        Some(&serde_json::json!("0.0.66"))
    );
    assert_eq!(
        parsed.pointer("/storage_health/authority_audit_ok"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        parsed.pointer("/storage_health/authority_audit_error"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        parsed.pointer("/doctor_report/kind"),
        Some(&serde_json::json!("unknown"))
    );
}

#[test]
fn daemon_doctor_request_uses_comprehensive_ready_owner() {
    assert_eq!(
        super::daemon_doctor_runtime_args(),
        serde_json::json!({
            "format": "json",
            "startup_health": false,
            "authority_audit": true,
            "doctor_report": true,
            "session_ingest_health": false,
        })
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

    let report = db.session_temporal_doctor_health().await;

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
    let report = serde_json::to_value(db.session_temporal_doctor_health().await).unwrap();
    assert_eq!(report["status"], "partial");
    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|finding| {
            finding["kind"] == "migration_gap" && finding["count"].as_u64().unwrap_or(0) >= 2
        }),
        "{report}"
    );
}

#[test]
fn daemon_runtime_parser_rejects_missing_json_payload() {
    let error = super::daemon_runtime_status(&serde_json::json!({ "content": [] })).unwrap_err();
    assert!(error.to_string().contains("returned no JSON payload"));
}

fn storage_runtime_finding(
    state: tracedecay_application::doctor::DoctorEvidenceStateV1,
    reference: &str,
) -> tracedecay_application::doctor::DoctorFindingV1 {
    use tracedecay_application::doctor::{
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

#[test]
fn canonical_storage_runtime_findings_are_the_only_storage_verdict() {
    use tracedecay_application::doctor::DoctorEvidenceStateV1 as State;

    let healthy = storage_runtime_finding(State::HealthyCompleteCoverage, "runtime.healthy");
    let unknown = storage_runtime_finding(State::Denied, "runtime.denied");
    let failed = storage_runtime_finding(State::Degraded, "runtime.degraded");

    assert_eq!(
        super::database_health_from_storage_runtime_findings([&healthy]),
        DatabaseHealth::Healthy
    );
    assert!(matches!(
        super::database_health_from_storage_runtime_findings([&healthy, &unknown]),
        DatabaseHealth::Unknown { .. }
    ));
    assert_eq!(
        super::database_health_from_storage_runtime_findings([&healthy, &unknown, &failed]),
        DatabaseHealth::Failed {
            reason: "runtime.degraded".to_string()
        }
    );
    assert!(matches!(
        super::database_health_from_storage_runtime_findings(std::iter::empty()),
        DatabaseHealth::Unknown { .. }
    ));
}

#[test]
fn denied_canonical_evidence_warns_instead_of_inventing_failure() {
    use tracedecay_application::doctor::DoctorEvidenceStateV1 as State;

    let mut counters = DoctorCounters::new();
    super::render_doctor_finding(
        &mut counters,
        &storage_runtime_finding(State::Denied, "runtime.denied"),
    );
    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn canonical_doctor_unavailable_states_remain_typed_nonfatal_reads() {
    assert_eq!(
        super::canonical_daemon_doctor_report(&serde_json::json!({})).unwrap(),
        None
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
            None
        );
    }
    assert_eq!(
        super::canonical_daemon_doctor_report(&serde_json::json!({
            "doctor_report": {
                "kind": "unknown",
                "table_growth_evidence": []
            }
        }))
        .unwrap(),
        None,
        "empty legacy table-growth evidence is unavailable, never a pass"
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

#[test]
fn daemon_runtime_parser_rejects_missing_database_telemetry() {
    let error = super::daemon_runtime_status(&serde_json::json!({
        "content": [{"type": "text", "text": r#"{"process":{"pid":1234}}"#}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("omitted database telemetry"));
}

/// The sole daemon owner is the only authority that can observe storage
/// health, so Doctor losing it is an issue, not a warning: nothing else in the
/// run opened the store, and a zero exit reads as "checked and fine" to every
/// caller and CI gate. `doctor_result` already turns any issue into a non-zero
/// exit, so grading this `fail` is what makes an unavailable daemon fail closed.
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
    let error = super::doctor_result(
        &counters,
        &DatabaseHealth::unknown("canonical_doctor_report_unavailable"),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "config error: doctor found 1 issue(s)");
}

#[test]
fn doctor_result_fails_when_checks_report_issues() {
    let mut counters = DoctorCounters::new();
    counters.fail("broken integration");

    let error = super::doctor_result(&counters, &DatabaseHealth::Healthy).unwrap_err();
    assert_eq!(error.to_string(), "config error: doctor found 1 issue(s)");
}

#[test]
fn doctor_result_allows_warnings_without_issues() {
    let mut counters = DoctorCounters::new();
    counters.warn("optional check unavailable");

    super::doctor_result(&counters, &DatabaseHealth::Healthy).unwrap();
}

#[test]
fn doctor_result_preserves_canonical_storage_failures() {
    let counters = DoctorCounters::new();
    let failed = DatabaseHealth::Failed {
        reason: "runtime.health.stuck".to_string(),
    };

    let error = super::doctor_result(&counters, &failed).unwrap_err();
    assert_eq!(
        error.to_string(),
        "config error: doctor storage health check failed [runtime.health.stuck]"
    );
}

#[test]
fn doctor_result_treats_unavailable_canonical_report_as_unknown() {
    let counters = DoctorCounters::new();
    super::doctor_result(
        &counters,
        &DatabaseHealth::Unknown {
            reason: "canonical_doctor_report_unavailable".to_string(),
        },
    )
    .unwrap();
}

fn canonical_temp_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

#[tokio::test]
async fn store_layout_resolution_surfaces_split_identity_conflict()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let project_root = canonical_temp_path(&project_root);
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project_root)
        .status()?;
    assert!(status.success());

    for project_id in ["proj_doctor_selected", "proj_doctor_legacy"] {
        let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
            &project_root,
            &profile_root,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )?;
        std::fs::create_dir_all(&layout.data_root)?;
        std::fs::write(&layout.graph_db_path, b"graph")?;
        tracedecay_runtime_core::storage::write_store_manifest(&layout)?;
    }
    tracedecay_runtime_core::storage::write_repository_identity_marker(
        &project_root,
        "proj_doctor_selected",
    )?;

    let open_options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let selected_db = profile_root.join("projects/proj_doctor_selected/tracedecay.db");
    let legacy_db = profile_root.join("projects/proj_doctor_legacy/tracedecay.db");
    let selected_before = std::fs::read(&selected_db)?;
    let legacy_before = std::fs::read(&legacy_db)?;

    let resolution = crate::tracedecay::TraceDecay::try_initialized_store_layout_with_options(
        &project_root,
        &open_options,
    )
    .await;
    let diagnostic = format!("{resolution:?}");
    assert!(
        diagnostic.contains("identity cutover conflict"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("proj_doctor_selected"), "{diagnostic}");
    assert!(diagnostic.contains("proj_doctor_legacy"), "{diagnostic}");
    assert!(
        diagnostic.contains("tracedecay migrate consolidate"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("--source-project-id proj_doctor_legacy"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("--target-project-id proj_doctor_selected"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("no files changed"), "{diagnostic}");
    assert_eq!(std::fs::read(selected_db)?, selected_before);
    assert_eq!(std::fs::read(legacy_db)?, legacy_before);
    Ok(())
}

#[test]
fn doctor_warns_for_intentionally_held_service_states_without_activation_advice() {
    use super::{DaemonServiceDoctorVerdict, daemon_service_doctor_verdict};
    use tracedecay_daemon_control::DaemonServiceState;

    for state in [
        DaemonServiceState::StoppedEnabled,
        DaemonServiceState::StoppedDisabled,
        DaemonServiceState::Masked,
    ] {
        assert_eq!(
            daemon_service_doctor_verdict(state),
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
    use super::{DaemonServiceDoctorVerdict, daemon_service_doctor_verdict};
    use tracedecay_daemon_control::DaemonServiceState;

    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::Missing),
        DaemonServiceDoctorVerdict::Warn
    );
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::RunningDisabled),
        DaemonServiceDoctorVerdict::Warn
    );
    assert_eq!(
        daemon_service_doctor_verdict(DaemonServiceState::RunningEnabled),
        DaemonServiceDoctorVerdict::Pass
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
