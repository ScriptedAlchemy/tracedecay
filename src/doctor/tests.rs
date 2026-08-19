use std::collections::BTreeMap;
use std::time::SystemTime;

use super::*;
use crate::agents::AgentIntegration;
use crate::display::format_bytes;

#[test]
fn supported_kimi_and_kiro_absence_reaches_doctor_without_host_directories() {
    let home = tempfile::tempdir().expect("isolated home");
    let reported = agents::all_integrations()
        .into_iter()
        .filter(|agent| should_run_host_healthcheck(agent.as_ref(), home.path()))
        .map(|agent| agent.id())
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        reported,
        std::collections::BTreeSet::from(["kimi", "kiro"]),
        "supported Kimi and Kiro absences must remain visible while unrelated absent hosts stay quiet"
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
    assert_eq!(counters.warnings, 2);
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
