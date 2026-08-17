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
fn daemon_runtime_request_keeps_startup_probe_bounded() {
    assert_eq!(
        super::daemon_startup_runtime_args(),
        serde_json::json!({
            "format": "json",
            "startup_health": true,
            "authority_audit": false,
            "doctor_report": false,
            "session_ingest_health": false,
        })
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

#[test]
fn daemon_startup_health_gates_only_current_project_storage() {
    let healthy = serde_json::json!({
        "storage_health": {
            "canonical_db_path": "/profile/project.db",
            "daemon_owner_pid": 1234,
            "daemon_version": "0.0.67+test",
            "quick_check_ok": true,
            "quick_check_error": null
        },
        "session_temporal_health": {
            "status": "unavailable",
            "reason": "compatibility_drift",
            "findings": [{
                "kind": "compatibility_drift",
                "count": 1
            }]
        }
    });
    assert!(
        super::daemon_startup_health_ready(&healthy),
        "unrelated session-temporal findings must remain Doctor findings, not block current-project admission"
    );
    assert_eq!(super::daemon_startup_health_detail(&healthy), "storage=ok");

    let bounded_probe = serde_json::json!({
        "storage_health": {
            "canonical_db_path": "/profile/project.db",
            "daemon_owner_pid": 1234,
            "daemon_version": "0.0.67+test",
            "quick_check_ok": null,
            "quick_check_error": null,
            "authority_audit_ok": null
        }
    });
    assert!(
        super::daemon_startup_health_ready(&bounded_probe),
        "mounted daemon telemetry is operationally ready while exhaustive integrity audits remain pending"
    );
    assert_eq!(
        super::daemon_startup_health_detail(&bounded_probe),
        "storage=quick_check_pending"
    );

    let migrating = serde_json::json!({
        "storage_health": {
            "canonical_db_path": "/profile/project.db",
            "daemon_owner_pid": 1234,
            "daemon_version": "0.0.67+test",
            "quick_check_error": "project_store_schema_unsupported"
        }
    });
    assert!(!super::daemon_startup_health_ready(&migrating));
}

#[test]
fn daemon_startup_health_requires_complete_mounted_daemon_identity() {
    let ready = serde_json::json!({
        "storage_health": {
            "canonical_db_path": "/profile/project.db",
            "daemon_owner_pid": 1234,
            "daemon_version": "0.0.67+test",
            "quick_check_ok": true
        }
    });
    assert!(super::daemon_startup_health_ready(&ready));

    for required_field in ["canonical_db_path", "daemon_owner_pid", "daemon_version"] {
        let mut incomplete = ready.clone();
        incomplete["storage_health"]
            .as_object_mut()
            .expect("storage health object")
            .remove(required_field);
        assert!(
            !super::daemon_startup_health_ready(&incomplete),
            "startup health must remain pending without {required_field}"
        );
    }
}

#[test]
fn daemon_startup_probe_skips_all_expensive_status_reads() {
    assert_eq!(
        super::daemon_admission_args(),
        serde_json::json!({
            "format": "json",
            "admission_only": true,
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        })
    );
}

#[test]
fn daemon_startup_pending_runtime_telemetry_is_retryable() {
    let error = super::daemon_runtime_status(&serde_json::json!({
        "content": [{"type": "text", "text": r#"{"process":{"pid":1234}}"#}]
    }))
    .unwrap_err();
    assert!(
        super::daemon_startup_error_is_retryable(&error),
        "an admitted project that has not published telemetry yet must be polled, not failed: {error}"
    );
    assert!(matches!(
        super::classify_daemon_startup_health_result(Err(error)),
        super::DaemonStartupHealthOutcome::Retryable { .. }
    ));
}

#[test]
fn daemon_startup_malformed_runtime_telemetry_stays_terminal() {
    let error = super::daemon_runtime_status(&serde_json::json!({
        "content": [{"type": "text", "text": r#"{"database":"not-an-object"}"#}]
    }))
    .unwrap_err();
    assert!(
        !super::daemon_startup_error_is_retryable(&error),
        "telemetry that is present but malformed is a contract violation: {error}"
    );
}

#[test]
fn daemon_startup_reset_requirement_is_terminal() {
    let error = crate::errors::TraceDecayError::reset_required(
        "session relation authority",
        "legacy session relation authority requires explicit reset",
    );

    assert!(!super::daemon_startup_error_is_retryable(&error));
}

#[test]
fn daemon_startup_host_cli_requirement_is_terminal() {
    let error = crate::errors::TraceDecayError::HostCliUnavailable {
        program: "kiro-cli".to_string(),
        lifecycle: "kiro MCP registry lifecycle".to_string(),
    };

    assert!(!super::daemon_startup_error_is_retryable(&error));
    assert!(matches!(
        super::classify_daemon_startup_health_result(Err(error)),
        super::DaemonStartupHealthOutcome::Terminal {
            error: crate::errors::TraceDecayError::HostCliUnavailable { program, lifecycle },
        } if program == "kiro-cli" && lifecycle == "kiro MCP registry lifecycle"
    ));
}

#[tokio::test]
async fn daemon_startup_health_converges_after_runtime_telemetry_appears() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe_attempts = std::sync::Arc::clone(&attempts);
    super::wait_for_daemon_startup_health_with(
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(1),
        move || {
            let attempt = probe_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                if attempt < 3 {
                    return super::daemon_runtime_status(&serde_json::json!({
                        "content": [{"type": "text", "text": r#"{"process":{"pid":1234}}"#}]
                    }));
                }
                Ok(serde_json::json!({
                    "storage_health": {
                        "canonical_db_path": "/profile/project.db",
                        "daemon_owner_pid": 1234,
                        "daemon_version": "0.0.67+test",
                        "quick_check_ok": true
                    }
                }))
            }
        },
        |_| {},
    )
    .await
    .expect("startup health must converge once the warming project publishes telemetry");
    assert!(
        attempts.load(std::sync::atomic::Ordering::Relaxed) >= 4,
        "the warming responses must have been polled before convergence"
    );
}

#[test]
fn daemon_startup_background_warmup_is_retryable() {
    let error = crate::errors::TraceDecayError::Config {
        message: "TraceDecay project '/fast/projects/tracedecay' is warming in the background; retry the same tool shortly".to_owned(),
    };
    assert!(super::daemon_startup_error_is_retryable(&error));
}

#[test]
fn daemon_startup_project_route_uses_typed_retryability() {
    let retryable = crate::errors::TraceDecayError::project_route(
        "project_route_unavailable",
        true,
        "project registry is warming",
    );
    assert!(super::daemon_startup_error_is_retryable(&retryable));
    assert!(matches!(
        super::classify_daemon_startup_health_result(Err(retryable)),
        super::DaemonStartupHealthOutcome::Retryable { detail }
            if detail.contains("project_route_unavailable")
                && detail.contains("project registry is warming")
    ));

    let terminal = crate::errors::TraceDecayError::project_route(
        "project_route_not_authorized",
        false,
        "project route is outside the admitted profile",
    );
    assert!(!super::daemon_startup_error_is_retryable(&terminal));
    assert!(matches!(
        super::classify_daemon_startup_health_result(Err(terminal)),
        super::DaemonStartupHealthOutcome::Terminal {
            error: crate::errors::TraceDecayError::ProjectRoute {
                reason_code,
                retryable: false,
                detail,
            },
        } if reason_code == "project_route_not_authorized"
            && detail == "project route is outside the admitted profile"
    ));
}

#[tokio::test]
async fn daemon_startup_health_surfaces_terminal_project_open_failure_immediately() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe_attempts = std::sync::Arc::clone(&attempts);
    let error = super::wait_for_daemon_startup_health_with(
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(1),
        move || {
            probe_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async {
                Err(crate::errors::TraceDecayError::Config {
                    message: "project-open source access denied: project-open source binding authority is inconsistent with the application contract".to_owned(),
                })
            }
        },
        |_| {},
    )
    .await
    .expect_err("terminal project-open error must fail the health wait");

    assert!(
        error
            .to_string()
            .contains("project-open source binding authority"),
        "underlying terminal error must be preserved: {error}"
    );
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "terminal failure must not be polled until the deadline"
    );
    assert!(super::daemon_startup_error_is_retryable(
        &crate::errors::TraceDecayError::Config {
            message: "daemon tracedecay_runtime timed out during read before deadline".to_owned(),
        }
    ));
}

#[tokio::test]
async fn daemon_startup_health_surfaces_terminal_corruption_immediately() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe_attempts = std::sync::Arc::clone(&attempts);
    let corrupt = serde_json::json!({
        "storage_health": {
            "quick_check_ok": false,
            "quick_check_error":
                "fts5: corruption found reading blob 412316860480 from table \"nodes_fts\"",
            "authority_audit_ok": true,
            "canonical_db_path": "/isolated/profile/projects/proj_test/tracedecay.db"
        }
    });
    let wait = super::wait_for_daemon_startup_health_with(
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(1),
        move || {
            probe_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let status = corrupt.clone();
            async move { Ok(status) }
        },
        |_| {},
    );
    let error = tokio::time::timeout(std::time::Duration::from_millis(100), wait)
        .await
        .expect("terminal corruption must not keep polling")
        .expect_err("terminal corruption must fail startup health validation");
    let message = error.to_string();

    assert!(message.contains("terminal daemon startup health failure"));
    assert!(
        message
            .contains("fts5: corruption found reading blob 412316860480 from table \"nodes_fts\"")
    );
    assert!(message.contains("tracedecay daemon restart"));
    assert!(message.contains("tracedecay tool runtime"));
    assert!(message.contains("tracedecay doctor"));
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "terminal corruption must not be retried"
    );
}

#[test]
fn daemon_startup_health_classifies_sqlite_corruption_spellings_as_terminal() {
    for problem in [
        "SQLITE_CORRUPT: database page failed validation",
        "database disk image is malformed",
        "malformed database image",
        "file is not a database",
    ] {
        let outcome = super::classify_daemon_startup_health_result(Ok(serde_json::json!({
            "storage_health": {
                "quick_check_error": problem,
                "authority_audit_reason": "authority_audit_not_run"
            }
        })));
        assert!(
            matches!(outcome, super::DaemonStartupHealthOutcome::Terminal { .. }),
            "{problem:?} must be terminal"
        );
    }
}

#[test]
fn startup_runtime_probe_defers_exhaustive_audits() {
    let args = super::daemon_startup_runtime_args();

    assert_eq!(args["startup_health"], serde_json::json!(true));
    assert_eq!(args["authority_audit"], serde_json::json!(false));
    assert_eq!(args["doctor_report"], serde_json::json!(false));
    assert_eq!(args["session_ingest_health"], serde_json::json!(false));
}

#[test]
fn daemon_startup_health_preserves_corruption_error_and_adds_remediation() {
    let problem = "fts5: corruption found reading blob 412316860480 from table \"nodes_fts\"";
    let outcome =
        super::classify_daemon_startup_health_result(Err(crate::errors::TraceDecayError::Config {
            message: problem.to_string(),
        }));
    let super::DaemonStartupHealthOutcome::Terminal { error } = outcome else {
        panic!("corruption error must be terminal");
    };
    let message = error.to_string();
    assert!(message.contains(problem));
    assert!(message.contains("terminal daemon startup health failure"));
    assert!(message.contains("tracedecay daemon restart"));
}

#[tokio::test]
async fn daemon_startup_health_retryable_progress_changes_then_converges() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe_attempts = std::sync::Arc::clone(&attempts);
    let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let progress_reports = std::sync::Arc::clone(&reports);
    super::wait_for_daemon_startup_health_with(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_millis(1),
        move || {
            let attempt = probe_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                Ok(match attempt {
                    0 => serde_json::json!({
                        "storage_health": {
                            "quick_check_error": "project_store_schema_unsupported",
                            "authority_audit_reason": "authority_audit_not_run"
                        }
                    }),
                    1 => serde_json::json!({
                        "storage_health": {
                            "quick_check_error": "project_store_migration_in_progress"
                        }
                    }),
                    _ => serde_json::json!({
                        "storage_health": {
                            "canonical_db_path": "/profile/project.db",
                            "daemon_owner_pid": 1234,
                            "daemon_version": "0.0.67+test",
                            "quick_check_ok": true
                        }
                    }),
                })
            }
        },
        move |progress| {
            progress_reports.lock().unwrap().push(progress);
        },
    )
    .await
    .expect("retryable startup health must converge");

    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "retryable health must continue polling until ready"
    );
    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].change, "initial observation");
    assert!(reports[1].change.starts_with("changed from "));
    assert!(
        reports[0]
            .detail
            .contains("project_store_schema_unsupported")
    );
    assert!(
        reports[1]
            .detail
            .contains("project_store_migration_in_progress")
    );
}

#[tokio::test]
async fn daemon_startup_health_deadline_is_distinct_from_terminal_failure() {
    let error = super::wait_for_daemon_startup_health_with(
        std::time::Duration::ZERO,
        std::time::Duration::from_millis(1),
        || async {
            Ok(serde_json::json!({
                "storage_health": {
                    "quick_check_error": "project_store_schema_unsupported",
                    "authority_audit_reason": "authority_audit_not_run"
                }
            }))
        },
        |_| {},
    )
    .await
    .expect_err("retryable health must fail when its deadline expires");

    let message = error.to_string();
    assert!(message.contains("deadline-exceeded"));
    assert!(message.contains("project_store_schema_unsupported"));
    assert!(!message.contains("terminal daemon startup health failure"));
}
