use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::SystemTime;

use super::*;
use crate::global_db::StoreInstanceUpsert;
use crate::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreLayout, StoreManifest, profile_sharded_layout, write_enrollment_marker,
    write_repository_identity_marker, write_store_manifest,
};
use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition, LspInstallOption};
use tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings;

/// How the doctor "Current project" check sees the working directory's store.
///
/// Production resolves the current project through the daemon
/// ([`daemon_project_status`]); this models the registry/alias-aware
/// resolution the tools use so the tests below can assert it directly.
#[derive(Debug)]
enum CurrentProjectStore {
    /// A store resolved through the same registry/alias-aware path the tools
    /// use (enrollment marker, git-common-dir alias, profile shard, …).
    Resolved(Box<StoreLayout>),
    /// No resolvable store, but an old repo-local `.tracedecay/` database exists.
    LegacyRepoLocal,
    /// Resolution genuinely found nothing — `tracedecay init` is warranted.
    Uninitialized,
}

async fn resolve_current_project_store(
    project_path: &Path,
    open_options: &TraceDecayOpenOptions,
) -> crate::errors::Result<CurrentProjectStore> {
    if let Some(layout) =
        TraceDecay::try_initialized_store_layout_with_options(project_path, open_options).await?
    {
        return Ok(CurrentProjectStore::Resolved(Box::new(layout)));
    }
    if crate::config::has_project_database(project_path) {
        return Ok(CurrentProjectStore::LegacyRepoLocal);
    }
    Ok(CurrentProjectStore::Uninitialized)
}

fn describe_resolved_store(layout: &StoreLayout) -> String {
    let mode = match layout.storage_mode {
        StorageMode::ProjectLocal => "repo-local",
        StorageMode::ProfileSharded => "profile-sharded",
    };
    let store_id = layout
        .identity
        .project_id
        .as_deref()
        .map_or_else(String::new, |id| format!(", store {id}"));
    format!(
        "Index found: {}/ ({mode}{store_id})",
        layout.data_root.display()
    )
}

fn canonical_temp_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

#[test]
fn table_growth_lines_report_baseline_and_unknown_without_zero() {
    let status = serde_json::json!({
        "kind": "observed",
        "table_growth_evidence": [
            {
                "kind": "baseline_established",
                "store": "sessions.db",
                "observed_at": 2_000,
                "tables_observed": 3
            },
            {
                "kind": "unknown",
                "store": "graph.db"
            }
        ]
    });

    let lines = table_growth_doctor_lines(&status);

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].level, StorageGrowthDoctorLineLevel::Warning);
    assert!(lines[0].message.contains("no prior baseline"));
    assert!(lines[0].message.contains("sessions.db"));
    assert_eq!(lines[1].level, StorageGrowthDoctorLineLevel::Warning);
    assert!(lines[1].message.contains("unavailable"));
    assert!(lines[1].message.contains("graph.db"));
    assert!(!lines[1].message.contains("0 B"));
}

#[test]
fn significant_table_growth_line_is_informational() {
    let status = serde_json::json!({
        "kind": "observed",
        "table_growth_evidence": [{
            "kind": "significant_growth",
            "store": "sessions.db",
            "table": "messages",
            "previous_bytes": 10_485_760,
            "current_bytes": 11_534_336,
            "growth_bytes": 1_048_576,
            "previous_observed_at": 1_000,
            "current_observed_at": 2_000
        }]
    });

    let lines = table_growth_doctor_lines(&status);

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, StorageGrowthDoctorLineLevel::Information);
    assert!(lines[0].message.contains("sessions.db.messages"));
    assert!(lines[0].message.contains("1.0 MB"));
}

/// A directory guaranteed to sit outside `std::env::temp_dir()`, for fixtures
/// that must NOT be classified as "ephemeral" by
/// `migrate::registry::classify_project_root` (which rejects project roots
/// under the OS temp directory). `env!("CARGO_MANIFEST_DIR")).parent()` used
/// to serve this purpose, but that only holds when the checkout itself lives
/// outside the temp directory; a repo cloned under `/tmp` (as some sandboxed
/// CI/dev environments do) breaks that assumption. Deriving the base from the
/// running test binary's own on-disk location is robust regardless of where
/// the checkout lives, because cargo (or any build-cache shim in front of it)
/// never places build output inside the volatile system temp directory.
fn ephemeral_safe_fixture_base() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary has a current_exe path");
    let profile_dir = exe
        .parent() // .../target/<profile>/deps
        .and_then(Path::parent) // .../target/<profile>
        .expect("test binary sits under a cargo target profile directory")
        .to_path_buf();
    let base = profile_dir.join("clone-path-hermetic-fixtures");
    std::fs::create_dir_all(&base).unwrap();
    base
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

#[tokio::test]
async fn orphan_reporting_uses_complete_registry_rows_not_token_accounting() {
    let base = ephemeral_safe_fixture_base();
    let dir = tempfile::Builder::new()
        .prefix("doctor-orphans-")
        .tempdir_in(&base)
        .unwrap();
    let db_dir = tempfile::Builder::new()
        .prefix("doctor-orphans-db-")
        .tempdir()
        .unwrap();
    let profile_root = dir.path().join("profile");
    let eligible_root = dir.path().join("eligible-repo");
    let conflicting_root = dir.path().join("conflicting-repo");
    let conflicting_registered_root = dir.path().join("registered-elsewhere");
    let blocked_root = dir.path().join("blocked-repo");
    std::fs::create_dir_all(&eligible_root).unwrap();
    std::fs::create_dir_all(&conflicting_root).unwrap();
    std::fs::create_dir_all(&conflicting_registered_root).unwrap();
    std::fs::create_dir_all(&blocked_root).unwrap();
    for root in [&eligible_root, &conflicting_root, &blocked_root] {
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }
    write_enrollment_marker(
        &eligible_root,
        &EnrollmentMarker {
            project_id: "proj_eligible".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    write_repository_identity_marker(&eligible_root, "proj_eligible").unwrap();
    write_enrollment_marker(
        &conflicting_root,
        &EnrollmentMarker {
            project_id: "proj_conflict".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    write_repository_identity_marker(&conflicting_root, "proj_conflict").unwrap();
    for (project_id, project_root) in [
        ("proj_eligible", &eligible_root),
        ("proj_conflict", &conflicting_root),
        ("proj_blocked", &blocked_root),
    ] {
        let data_root = profile_root.join("projects").join(project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: data_root.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        };
        std::fs::write(
            data_root.join(STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    let runtime = DoctorTestRuntime::open(
        &db_dir.path().join("profile"),
        "doctor orphan reporting test",
    )
    .await;
    let db = runtime.database();
    db.upsert_code_project(
        "proj_conflict",
        &conflicting_registered_root,
        None,
        None,
        Some("main"),
    )
    .await
    .unwrap();
    let (count, warnings) = orphan_store_manifest_report(db, &profile_root).await;

    assert_eq!(count, 1, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("proj_conflict")),
        "{warnings:?}"
    );

    let scan = crate::migrate::registry::scan_profile_store_manifests(&profile_root, 1_800_000_000);
    let eligible = crate::migrate::registry::RegistryReconstructionReport {
        plans: scan
            .plans
            .into_iter()
            .filter(|plan| {
                plan.status == crate::migrate::registry::RegistryReconstructionStatus::Eligible
                    && plan.project.project_id == "proj_eligible"
            })
            .collect(),
        issues: Vec::new(),
    };
    let mut batch_left = eligible.plans[0].clone();
    batch_left.project.aliases = vec![dir.path().join("shared-alias")];
    let mut batch_right = batch_left.clone();
    batch_right.project.project_id = "proj_batch_other".to_string();
    batch_right.project.project_root = conflicting_root.clone();
    batch_right.store.project_id = batch_right.project.project_id.clone();
    batch_right.store.store_id = "store:proj_batch_other:profile_sharded".to_string();
    batch_right.store.store_relpath = "projects/proj_batch_other".to_string();
    batch_right.store.manifest_relpath =
        Some("projects/proj_batch_other/store_manifest.json".to_string());
    batch_right.graph_scopes.clear();
    batch_right.artifacts.clear();
    batch_left.graph_scopes.clear();
    batch_left.artifacts.clear();
    let batch_diff = crate::migrate::registry::diff_registry_reconstruction_report(
        db,
        &crate::migrate::registry::RegistryReconstructionReport {
            plans: vec![batch_left, batch_right],
            issues: Vec::new(),
        },
    )
    .await;
    assert_eq!(batch_diff.missing_plans, 0);
    assert!(
        batch_diff
            .issues
            .iter()
            .any(|issue| issue.contains("shared-alias")),
        "{:?}",
        batch_diff.issues
    );
    let applied = crate::migrate::registry::apply_registry_reconstruction_report(db, &eligible)
        .await
        .unwrap();
    assert_eq!(applied.projects, 1);
    assert_eq!(
        orphan_store_manifest_report(db, &profile_root).await.0,
        0,
        "a complete reconstruction registry is healthy without a legacy projects.path row"
    );
    assert_eq!(
        crate::migrate::registry::apply_registry_reconstruction_report(db, &eligible)
            .await
            .unwrap(),
        crate::migrate::registry::RegistryReconstructionApplyReport::default()
    );

    db.writer_connection()
        .unwrap()
        .execute(
            "DELETE FROM store_artifacts WHERE store_id=?1",
            crate::db::engine::params![eligible.plans[0].store.store_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(orphan_store_manifest_report(db, &profile_root).await.0, 1);
    crate::migrate::registry::apply_registry_reconstruction_report(db, &eligible)
        .await
        .unwrap();

    db.writer_connection()
        .unwrap()
        .execute(
            "DELETE FROM store_instances WHERE store_id=?1",
            crate::db::engine::params![eligible.plans[0].store.store_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(orphan_store_manifest_report(db, &profile_root).await.0, 1);
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
    assert!(guidance.contains("Derived branch indexes are preserved"));

    let branch_db = PathBuf::from("/profile/projects/proj_test/branches/feature.db");
    let branch_guidance = database_recovery_guidance(&branch_db);
    let branches_root = branch_db.parent().unwrap();
    let data_root = branches_root.parent().unwrap();
    assert!(branch_guidance.contains(&data_root.join("dirty").display().to_string()));
    assert!(branch_guidance.contains(&data_root.join("sessions.db").display().to_string()));
    assert!(!branch_guidance.contains(&branches_root.join("sessions.db").display().to_string()));
}

#[test]
fn nodes_fts_corruption_guidance_uses_derived_index_recovery() {
    let db_path = PathBuf::from("/profile/projects/proj_test/tracedecay.db");
    let guidance = database_recovery_guidance_for_problem(
        &db_path,
        "malformed inverted index for FTS5 table main.nodes_fts",
    );

    assert!(guidance.contains("derived `nodes_fts` index"));
    assert!(guidance.contains("run `tracedecay daemon restart`"));
    assert!(guidance.contains("retained or a newer compatible binary"));
    assert!(guidance.contains("rebuild it from the authoritative `nodes` table"));
    assert!(guidance.contains("Do not run `tracedecay init`"));
    assert!(!guidance.contains("automatic rebuild is intentionally blocked"));
}

#[tokio::test]
async fn database_check_preserves_corrupt_graph_and_adjacent_stores()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let layout = ts.store_layout().clone();
    ts.close();

    let corrupt_db = b"not-a-sqlite-database";
    let wal_path = layout.graph_db_path.with_extension("db-wal");
    let shm_path = layout.graph_db_path.with_extension("db-shm");
    std::fs::write(&layout.graph_db_path, corrupt_db)?;
    std::fs::write(&wal_path, b"preserve-wal")?;
    std::fs::write(&shm_path, b"preserve-shm")?;
    std::fs::write(&layout.dirty_path, b"preserve-dirty")?;
    std::fs::write(&layout.sessions_db_path, b"preserve-sessions")?;

    let mut counters = DoctorCounters::new();
    let health = check_database(
        &mut counters,
        &serde_json::json!({
            "storage_health": {
                "canonical_db_path": layout.graph_db_path,
                "db_size_bytes": corrupt_db.len(),
                "quick_check_ok": false,
                "authority_audit_ok": true,
                "authority_audit_error": null,
                "dirty_marker": { "exists": true, "state": "dirty" },
            }
        }),
    );

    assert!(matches!(health, DatabaseHealth::Failed { .. }));
    assert_eq!(counters.issues, 1);
    assert_eq!(std::fs::read(&layout.graph_db_path)?, corrupt_db);
    assert_eq!(std::fs::read(&wal_path)?, b"preserve-wal");
    assert_eq!(std::fs::read(&shm_path)?, b"preserve-shm");
    assert_eq!(std::fs::read(&layout.dirty_path)?, b"preserve-dirty");
    assert_eq!(
        std::fs::read(&layout.sessions_db_path)?,
        b"preserve-sessions"
    );
    Ok(())
}

#[tokio::test]
async fn database_check_is_read_only_while_a_writer_is_live()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let db_path = ts.db_path();
    let writer = ts.db();
    writer
        .execute_write_batch(
            "seed doctor freelist fixture",
            "CREATE TABLE doctor_probe (payload BLOB);\
             WITH RECURSIVE count(x) AS (\
                 VALUES(1) UNION ALL SELECT x + 1 FROM count WHERE x < 256\
             )\
             INSERT INTO doctor_probe SELECT zeroblob(8192) FROM count;\
             DELETE FROM doctor_probe;",
        )
        .await?;
    writer.checkpoint().await?;

    let freelist_before: i64 = {
        let mut rows = writer.conn().query("PRAGMA freelist_count", ()).await?;
        rows.next().await?.expect("freelist row").get(0)?
    };
    assert!(
        freelist_before > 0,
        "fixture must contain reclaimable pages"
    );

    let mut counters = DoctorCounters::new();
    let health = check_database(
        &mut counters,
        &serde_json::json!({
            "storage_health": {
                "canonical_db_path": db_path,
                "db_size_bytes": std::fs::metadata(&db_path)?.len(),
                "quick_check_ok": true,
                "authority_audit_ok": true,
                "authority_audit_error": null,
                "dirty_marker": { "exists": false },
                "daemon_owner_pid": std::process::id(),
                "daemon_generation": "test-generation",
            }
        }),
    );
    assert_eq!(health, DatabaseHealth::Healthy);

    let freelist_after: i64 = {
        let mut rows = writer.conn().query("PRAGMA freelist_count", ()).await?;
        rows.next().await?.expect("freelist row").get(0)?
    };
    assert_eq!(
        freelist_after, freelist_before,
        "doctor must not run VACUUM or otherwise compact a live database"
    );
    writer
        .execute_write(
            "verify doctor writer remains usable",
            "INSERT INTO doctor_probe(payload) VALUES (zeroblob(64))",
            (),
        )
        .await?;
    assert!(
        writer.quick_check().await?,
        "live writer must remain healthy"
    );
    Ok(())
}

#[test]
fn database_authority_failures_are_enforced_and_unavailable_is_degraded() {
    let healthy_status = serde_json::json!({
        "storage_health": {
            "quick_check_ok": true,
            "authority_audit_ok": true,
            "authority_audit_error": null,
        }
    });
    let mut healthy_counters = DoctorCounters::new();
    assert_eq!(
        check_database(&mut healthy_counters, &healthy_status),
        DatabaseHealth::Healthy
    );
    assert_eq!(healthy_counters.issues, 0);

    let failed_status = serde_json::json!({
        "storage_health": {
            "quick_check_ok": true,
            "authority_audit_ok": false,
            "authority_audit_error": "multiple writers detected",
        }
    });
    let mut failed_counters = DoctorCounters::new();
    assert!(matches!(
        check_database(&mut failed_counters, &failed_status),
        DatabaseHealth::Failed { .. }
    ));
    assert_eq!(failed_counters.issues, 1);

    let mut missing_counters = DoctorCounters::new();
    assert!(matches!(
        check_database(
            &mut missing_counters,
            &serde_json::json!({ "storage_health": { "quick_check_ok": true } }),
        ),
        DatabaseHealth::Unknown { .. }
    ));
    assert_eq!(missing_counters.issues, 0);
    assert_eq!(missing_counters.warnings, 1);

    let mut bounded_counters = DoctorCounters::new();
    assert!(
        matches!(
            check_database(
                &mut bounded_counters,
                &serde_json::json!({
                    "storage_health": {
                        "canonical_db_path": "/profile/project.db",
                        "quick_check_ok": null,
                        "quick_check_error": null,
                        "authority_audit_ok": null,
                        "authority_audit_reason": "authority_audit_not_run"
                    }
                }),
            ),
            DatabaseHealth::Unknown { .. }
        ),
        "an audit that did not run must stay distinguishable, not become healthy"
    );
    assert_eq!(bounded_counters.issues, 0);
    assert_eq!(bounded_counters.warnings, 2);

    // With integrity observed healthy, the authority reason is what survives.
    let mut not_run_counters = DoctorCounters::new();
    assert_eq!(
        check_database(
            &mut not_run_counters,
            &serde_json::json!({
                "storage_health": {
                    "quick_check_ok": true,
                    "authority_audit_ok": null,
                    "authority_audit_reason": "authority_audit_not_run"
                }
            }),
        ),
        DatabaseHealth::Unknown {
            reason: "authority_audit_not_run".to_string()
        }
    );
    assert_eq!(not_run_counters.issues, 0);
    assert_eq!(not_run_counters.warnings, 1);
}

#[test]
fn authority_audit_failure_preserves_the_observed_detail() {
    assert_eq!(
        authority_audit_failure_message("authority_invariant_failed", Some("2 orphaned rows")),
        "Observation database authority audit failed [authority_invariant_failed]: 2 orphaned rows"
    );
    assert_eq!(
        authority_audit_failure_message("authority_invariant_failed", None),
        "Observation database authority audit failed [authority_invariant_failed]"
    );
    // Older producers mirrored the reason into the detail key.
    assert_eq!(
        authority_audit_failure_message(
            "authority_invariant_failed",
            Some("authority_invariant_failed")
        ),
        "Observation database authority audit failed [authority_invariant_failed]"
    );
    assert_eq!(
        authority_audit_failure_message("something_else", Some("detail")),
        "Observation database authority audit failed [authority_audit_failed]: detail"
    );
}

#[test]
fn unavailable_reason_falls_back_to_the_legacy_error_key() {
    let mut counters = DoctorCounters::new();
    let health = check_database(
        &mut counters,
        &serde_json::json!({
            "storage_health": {
                "quick_check_ok": true,
                "authority_audit_ok": null,
                "authority_audit_error": "authority_store_missing"
            }
        }),
    );

    assert_eq!(
        health,
        DatabaseHealth::Unknown {
            reason: "authority_store_missing".to_string()
        },
        "a producer that only wrote the legacy error key must not be flattened to \
         authority_audit_unavailable"
    );
    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn unknown_storage_health_is_not_a_healthy_verdict_and_does_not_gate() {
    let unknown = DatabaseHealth::Unknown {
        reason: "authority_audit_not_run".to_string(),
    };
    assert_ne!(unknown, DatabaseHealth::Healthy);

    // Non-fatal for the exit code...
    let counters = DoctorCounters::new();
    super::doctor_result(&counters, Ok(serde_json::json!({})), &unknown).unwrap();

    // ...but never laundered into health, and still severity-ordered below a
    // real failure.
    assert_eq!(
        DatabaseHealth::Healthy.merge(unknown.clone()),
        unknown,
        "unknown must dominate healthy"
    );
    let failed = DatabaseHealth::Failed {
        reason: "integrity_check_failed".to_string(),
    };
    assert_eq!(
        unknown.clone().merge(failed.clone()),
        failed,
        "failed must dominate unknown"
    );
    assert_eq!(failed.clone().merge(unknown), failed);
}

#[tokio::test]
async fn current_project_store_resolves_profile_shard_via_registry_alias()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let project_root = canonical_temp_path(&project_root);
    let project_id = tracedecay_store::ProjectId::new("proj_doctor_current")?;
    let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        project_id,
    )
    .await?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: None,
    };
    let graph = runtime
        .initialize_project_graph_for_test(&project_root, open_options.clone())
        .await?;
    graph.index_all().await?;
    let shard_root = graph.store_layout().data_root.clone();

    // No repo-local `.tracedecay/` index exists, yet the project must not
    // be reported as uninitialized: resolution finds the profile shard.
    assert!(!crate::config::has_project_database(&project_root));
    match resolve_current_project_store(&project_root, &open_options).await? {
        CurrentProjectStore::Resolved(layout) => {
            assert_eq!(layout.data_root, shard_root);
            assert_eq!(
                layout.identity.project_id.as_deref(),
                Some("proj_doctor_current")
            );
            assert!(describe_resolved_store(&layout).contains("profile-sharded"));
        }
        other => panic!("expected resolved profile shard, got {other:?}"),
    }

    // A project the registry knows nothing about should still get the
    // `tracedecay init` advice.
    let unregistered = dir.path().join("unregistered");
    std::fs::create_dir_all(&unregistered)?;
    let unregistered = canonical_temp_path(&unregistered);
    assert!(matches!(
        resolve_current_project_store(&unregistered, &open_options).await?,
        CurrentProjectStore::Uninitialized
    ));
    Ok(())
}

#[tokio::test]
async fn current_project_store_resolves_moved_repository_identity_read_only()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let original = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    std::fs::create_dir_all(&original)?;
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&original)
        .status()?;
    assert!(status.success());

    let project_id = "proj_doctor_moved";
    let shard_root = crate::storage::profile_sharded_data_root(&profile_root, project_id);
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(
        shard_root.join(crate::config::db_filename(&shard_root)),
        b"graph",
    )?;
    crate::storage::write_repository_identity_marker(&original, project_id)?;
    std::fs::rename(&original, &moved)?;

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    match resolve_current_project_store(&moved, &open_options).await? {
        CurrentProjectStore::Resolved(layout) => {
            assert_eq!(layout.data_root, shard_root);
            assert_eq!(layout.identity.project_id.as_deref(), Some(project_id));
        }
        other => panic!("expected moved repository identity, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn current_project_store_surfaces_split_identity_conflict()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project_root)
        .status()?;
    assert!(status.success());

    let runtime =
        DoctorTestRuntime::open(&profile_root, "doctor split identity resolution test").await;
    for (project_id, node_id) in [
        ("proj_doctor_selected", "selected-node"),
        ("proj_doctor_legacy", "legacy-node"),
    ] {
        let layout = profile_sharded_layout(
            &project_root,
            &profile_root,
            &EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: StorageMode::ProfileSharded,
            },
        )?;
        let authority = crate::db::DatabaseAuthority::acquire_test(
            &layout.graph_db_path,
            "doctor identity test",
        )?;
        let db = runtime
            ._registry
            .code_graph_worktree(
                &project_root,
                tracedecay_store::ProjectId::new(project_id.to_string())?,
                layout.graph_db_path.clone(),
                authority,
                crate::db::DatabaseAccessMode::ReadWrite,
            )
            .await?;
        db.insert_node(&crate::types::Node {
            id: node_id.to_string(),
            kind: crate::types::NodeKind::Function,
            name: node_id.to_string(),
            qualified_name: node_id.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: crate::types::Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 1_800_000_000,
            parent_id: None,
        })
        .await?;
        db.checkpoint().await?;
        db.close();
        write_store_manifest(&layout)?;
    }
    write_repository_identity_marker(&project_root, "proj_doctor_selected")?;

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(runtime.database().db_path().to_path_buf()),
    };
    let selected_db = profile_root.join("projects/proj_doctor_selected/tracedecay.db");
    let legacy_db = profile_root.join("projects/proj_doctor_legacy/tracedecay.db");
    let selected_before = std::fs::read(&selected_db)?;
    let legacy_before = std::fs::read(&legacy_db)?;

    let resolution = resolve_current_project_store(&project_root, &open_options).await;
    let diagnostic = format!("{resolution:?}");
    assert!(
        diagnostic.contains("identity cutover conflict"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("proj_doctor_selected"), "{diagnostic}");
    assert!(diagnostic.contains("proj_doctor_legacy"), "{diagnostic}");
    assert!(
        diagnostic.contains("choose one shard and retire the other"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("no files changed"), "{diagnostic}");
    assert!(!diagnostic.contains("Uninitialized"), "{diagnostic}");
    assert_eq!(std::fs::read(selected_db)?, selected_before);
    assert_eq!(std::fs::read(legacy_db)?, legacy_before);
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_is_not_stale_without_marker()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let shard_relpath = Path::new("projects").join("proj_doctor");
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&shard_root)?;
    let project_root = canonical_temp_path(&project_root);
    std::fs::write(shard_root.join("tracedecay.db"), b"graph")?;
    let runtime =
        DoctorTestRuntime::open(&profile_root, "doctor registry storage classification test").await;
    let db = runtime.database();
    db.upsert(&project_root, 42).await;
    db.upsert_code_project("proj_doctor", &project_root, None, None, Some("main"))
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor:profile_sharded".to_string(),
        project_id: "proj_doctor".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage(&project_root),
        DoctorStorageStatus::Stale
    );
    assert_eq!(
        classify_project_storage_with_registry(&project_root, db, Some(&profile_root)).await,
        DoctorStorageStatus::ProfileSharded
    );
    #[cfg(unix)]
    {
        let symlinked_profile_root = dir.path().join("profile-link");
        symlink(&profile_root, &symlinked_profile_root)?;
        assert_eq!(
            classify_project_storage_with_registry(
                &project_root,
                db,
                Some(&symlinked_profile_root)
            )
            .await,
            DoctorStorageStatus::ProfileSharded
        );
    }
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_manifest_relpath_uses_profile_root()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let project_root = canonical_temp_path(&dir.path().join("repo"));
    let shard_relpath = Path::new("projects").join("proj_doctor_manifest");
    let manifest_relpath = shard_relpath.join(crate::storage::STORE_MANIFEST_FILENAME);
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(profile_root.join(&manifest_relpath), b"manifest")?;
    let runtime = DoctorTestRuntime::open(
        &profile_root,
        "doctor registry manifest classification test",
    )
    .await;
    let db = runtime.database();
    db.upsert(&project_root, 42).await;
    db.upsert_code_project(
        "proj_doctor_manifest",
        &project_root,
        None,
        None,
        Some("main"),
    )
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor_manifest:profile_sharded".to_string(),
        project_id: "proj_doctor_manifest".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(manifest_relpath.to_string_lossy().to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage_with_registry(&project_root, db, Some(&profile_root)).await,
        DoctorStorageStatus::ManifestReconstructable
    );
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_rejects_unsafe_store_relpath()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let outside_root = dir.path().join("outside");
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&outside_root)?;
    let project_root = canonical_temp_path(&project_root);
    std::fs::write(outside_root.join("tracedecay.db"), b"graph")?;
    let runtime = DoctorTestRuntime::open(
        &profile_root,
        "doctor unsafe registry storage classification test",
    )
    .await;
    let db = runtime.database();
    db.upsert(&project_root, 42).await;
    db.upsert_code_project(
        "proj_doctor_escape",
        &project_root,
        None,
        None,
        Some("main"),
    )
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor_escape:profile_sharded".to_string(),
        project_id: "proj_doctor_escape".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "../outside".to_string(),
        manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage_with_registry(&project_root, db, Some(&profile_root)).await,
        DoctorStorageStatus::Stale
    );
    Ok(())
}

#[tokio::test]
async fn registry_drift_findings_report_manifest_identity_mismatches()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let registry_root = canonical_temp_path(&dir.path().join("registry-repo"));
    let manifest_root = canonical_temp_path(&dir.path().join("manifest-repo"));
    let shard_relpath = Path::new("projects").join("proj_registry");
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&registry_root)?;
    std::fs::create_dir_all(&manifest_root)?;
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(shard_root.join("tracedecay.db"), b"graph")?;
    std::fs::write(shard_root.join("sessions.db"), b"sessions")?;
    std::fs::write(shard_root.join("branch-meta.json"), b"{}")?;
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_manifest".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: manifest_root.clone(),
        data_root: shard_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    std::fs::write(
        shard_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    let runtime =
        DoctorTestRuntime::open(&profile_root, "doctor registry drift findings test").await;
    let db = runtime.database();
    db.upsert_code_project("proj_registry", &registry_root, None, None, Some("main"))
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_registry:profile_sharded".to_string(),
        project_id: "proj_registry".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(
            shard_relpath
                .join(STORE_MANIFEST_FILENAME)
                .to_string_lossy()
                .to_string(),
        ),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    let findings = registry_drift::registry_drift_findings(db, &profile_root).await;
    let fields: Vec<_> = findings.iter().map(|finding| finding.field).collect();
    assert!(
        fields.contains(&"project_id"),
        "expected project_id drift finding, got {findings:#?}"
    );
    assert!(
        fields.contains(&"project_root"),
        "expected project_root drift finding, got {findings:#?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.registry_value == "proj_registry"
                && finding.manifest_value == "proj_manifest"),
        "project_id finding should include registry and manifest values: {findings:#?}"
    );

    Ok(())
}

/// A `ForeignOrphan` drift line must render as `Info` severity (no warning
/// count) and must never prescribe `tracedecay update` — the remediation the
/// remove path refuses to perform on a foreign package. Mirrors the pure
/// classifier pattern used for database-recovery guidance.
#[test]
fn foreign_orphan_renders_as_info_without_update_remediation() {
    use crate::automation::skill_materialization::SkillDrift;
    let finding = SkillDrift::ForeignOrphan {
        skill_id: "code-slop-cleanup".to_string(),
        path: std::path::PathBuf::from("/repo/.claude/skills/code-slop-cleanup/SKILL.md"),
    };
    let (level, msg) = super::skill_drift_report("claude/project", &finding);
    assert_eq!(level, super::DriftLevel::Info);
    assert!(
        msg.contains("another installation"),
        "message should explain the foreign origin: {msg}"
    );
    assert!(
        !msg.contains("tracedecay update"),
        "foreign orphan must not prescribe `tracedecay update`: {msg}"
    );
}

/// A self-authored `Orphan` still renders as `Warn` and keeps the update
/// remediation — the classifier must not blanket-downgrade every orphan.
#[test]
fn plain_orphan_still_warns_with_update_remediation() {
    use crate::automation::skill_materialization::SkillDrift;
    let finding = SkillDrift::Orphan {
        skill_id: "code-slop-cleanup".to_string(),
        path: std::path::PathBuf::from("/repo/.claude/skills/code-slop-cleanup/SKILL.md"),
    };
    let (level, msg) = super::skill_drift_report("claude/project", &finding);
    assert_eq!(level, super::DriftLevel::Warn);
    assert!(
        msg.contains("tracedecay update"),
        "plain orphan should still prescribe update: {msg}"
    );
}

#[test]
fn daemon_runtime_parser_extracts_storage_health_and_owner() {
    let parsed = super::daemon_runtime_status(&serde_json::json!({
        "content": [
            {"type": "text", "text": "daemon notice"},
            {
                "type": "text",
                "text": r#"{"tracedecay_version":"0.0.66","process":{"pid":1234},"database":{"canonical_db_path":"/tmp/project.db","quick_check_ok":true,"authority_audit_ok":true,"authority_audit_error":null,"dirty_marker":{"exists":false}},"doctor_report":{"kind":"unknown","table_growth_evidence":[]},"session_temporal_health":{"status":"complete","findings":[]},"cursor_session_ingest":{"tracked_transcripts":1,"pending_transcripts":0,"pending_bytes":0,"max_transcript_pending_bytes":0},"cursor_session_placeholder_paths":["${workspaceFolder}/cursor.jsonl"]}"#
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
        parsed.pointer("/cursor_session_ingest/tracked_transcripts"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        parsed.pointer("/cursor_session_placeholder_paths/0"),
        Some(&serde_json::json!("${workspaceFolder}/cursor.jsonl"))
    );
    assert_eq!(
        parsed.pointer("/session_temporal_health/status"),
        Some(&serde_json::json!("complete"))
    );
    assert_eq!(
        parsed.pointer("/doctor_report/kind"),
        Some(&serde_json::json!("unknown"))
    );
}

fn semantic_generation(byte: char) -> tracedecay_domain::VectorGenerationIdV1 {
    tracedecay_domain::VectorGenerationIdV1::new(
        tracedecay_domain::ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .unwrap(),
    )
}

fn semantic_status(
    state: crate::application::semantic_runtime::SemanticRuntimeStateV1,
) -> serde_json::Value {
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};

    let current = crate::application::configuration::ConfigurationCurrentStateV1 {
        revision_id: ConfigurationRevisionId::try_from("configuration.revision.doctor".to_owned())
            .unwrap(),
        snapshot: ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default()).unwrap(),
    };
    let pin =
        crate::application::semantic_runtime::SemanticConfigurationPinV1::from_current(&current)
            .unwrap();
    serde_json::to_value(
        crate::application::semantic_runtime::SemanticRuntimeStatusV1::new(Some(pin), state),
    )
    .unwrap()
}

#[test]
fn daemon_runtime_parser_preserves_semantic_status() {
    for state in [
        crate::application::semantic_runtime::SemanticRuntimeStateV1::Indexing {
            target_generation: semantic_generation('a'),
            completed_units: 1,
            total_units: 2,
        },
        crate::application::semantic_runtime::SemanticRuntimeStateV1::Degraded {
            active_generation: Some(semantic_generation('a')),
            reason: crate::application::semantic_runtime::SemanticFallbackReasonV1::RuntimeFailure,
        },
    ] {
        let semantic = semantic_status(state);
        let payload = serde_json::json!({
            "database": {
                "quick_check_ok": true,
                "authority_audit_ok": true
            },
            "semantic_runtime": semantic
        });
        let result = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&payload).unwrap()
            }]
        });

        let parsed = super::daemon_runtime_status(&result).unwrap();
        assert_eq!(
            parsed.get("semantic_runtime"),
            payload.get("semantic_runtime")
        );
    }
}

#[test]
fn semantic_indexing_is_healthy_non_blocking_fallback() {
    let status = semantic_status(
        crate::application::semantic_runtime::SemanticRuntimeStateV1::Indexing {
            target_generation: semantic_generation('a'),
            completed_units: 3,
            total_units: 10,
        },
    );
    let mut counters = DoctorCounters::new();
    super::check_semantic_runtime_health(
        &mut counters,
        Some(&serde_json::json!({ "semantic_runtime": status })),
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn semantic_degradation_warns_without_failing_baseline_search() {
    let status = semantic_status(
        crate::application::semantic_runtime::SemanticRuntimeStateV1::Degraded {
            active_generation: Some(semantic_generation('a')),
            reason: crate::application::semantic_runtime::SemanticFallbackReasonV1::RuntimeFailure,
        },
    );
    let mut counters = DoctorCounters::new();
    super::check_semantic_runtime_health(
        &mut counters,
        Some(&serde_json::json!({ "semantic_runtime": status })),
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn semantic_status_missing_from_daemon_keeps_offline_baseline_healthy() {
    let mut counters = DoctorCounters::new();
    super::check_semantic_runtime_health(&mut counters, Some(&serde_json::json!({})));
    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn selected_not_downloaded_is_offline_healthy_with_retry_guidance() {
    let status = semantic_status(
        crate::application::semantic_runtime::SemanticRuntimeStateV1::SelectedNotDownloaded {
            model_id: crate::config::DEFAULT_FASTEMBED_MODEL_ID.to_owned(),
            artifact_digest: "a".repeat(64),
        },
    );
    let mut counters = DoctorCounters::new();
    super::check_semantic_runtime_health(
        &mut counters,
        Some(&serde_json::json!({ "semantic_runtime": status })),
    );
    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 0);
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

#[test]
fn temporal_health_diagnosis_is_exhaustive_and_canonical() {
    let kinds = [
        "trigger_audit_drift",
        "occurrence_fts_corruption",
        "summary_fts_corruption",
        "summary_cycle",
        "stale_closure",
        "missing_anchor",
        "missing_receipt",
        "invalid_generation",
        "multi_active_generation",
        "cursor_chain_absent",
        "cursor_key_absent",
        "ownership_drift",
        "stuck_refresh",
        "stuck_binding",
        "stuck_progress",
        "stuck_receipt",
        "migration_gap",
        "compatibility_drift",
    ];
    let diagnosis = super::temporal_health::diagnose(Some(&serde_json::json!({
        "status": "complete",
        "findings": kinds
            .iter()
            .map(|kind| serde_json::json!({ "kind": kind, "count": 1 }))
            .collect::<Vec<_>>(),
    })));

    assert_eq!(diagnosis.finding_codes(), kinds);
    assert!(diagnosis.lines().iter().all(|line| {
        line.level == super::temporal_health::TemporalHealthLineLevel::Fail
            && line.text.contains("daemon-owned")
            && line.text.contains("no repair")
    }));
    let rendered = diagnosis
        .lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    assert!(rendered.contains("explicit derived-index repair"));
    assert!(rendered.contains("preserve the database"));
    assert!(rendered.contains("pause temporal refresh"));
    assert!(!rendered.contains("Plan 14"));
}

#[test]
fn temporal_health_diagnosis_distinguishes_non_clean_availability() {
    for status in ["unavailable", "partial", "locked"] {
        let diagnosis = super::temporal_health::diagnose(Some(&serde_json::json!({
            "status": status,
            "findings": [],
        })));
        assert!(!diagnosis.is_clean(), "{status}");
        assert!(
            diagnosis.lines().iter().any(|line| {
                line.level == super::temporal_health::TemporalHealthLineLevel::Warn
                    && line.text.contains(status)
            }),
            "{status}: {:?}",
            diagnosis.lines()
        );
    }

    let missing = super::temporal_health::diagnose(None);
    assert!(!missing.is_clean());
    assert!(
        missing
            .lines()
            .iter()
            .any(|line| line.text.contains("unavailable"))
    );
}

#[test]
fn cursor_key_recovery_is_advisory_only_while_the_report_owner_is_warming() {
    let diagnosis = super::temporal_health::diagnose_with_recovery(
        Some(&serde_json::json!({
            "status": "complete",
            "findings": [{
                "kind": "cursor_key_absent",
                "count": 8,
            }],
        })),
        true,
    );

    assert_eq!(
        diagnosis.lines()[0].level,
        super::temporal_health::TemporalHealthLineLevel::Warn
    );
    assert!(
        super::temporal_health::diagnose_with_recovery(
            Some(&serde_json::json!({
                "status": "complete",
                "findings": [{
                    "kind": "cursor_key_absent",
                    "count": 8,
                }],
            })),
            false,
        )
        .lines()
        .iter()
        .all(|line| line.level == super::temporal_health::TemporalHealthLineLevel::Fail)
    );
}

#[test]
fn temporal_health_diagnosis_is_bounded_and_redacts_payload_keys_and_text() {
    let canary = "sk-live-temporal-doctor-secret";
    let findings = (0..=super::temporal_health::MAX_DIAGNOSIS_FINDINGS)
        .map(|_| {
            serde_json::json!({
                "kind": "missing_anchor",
                "count": u64::MAX,
                "summary_text": canary,
                "key_material": canary,
                "payload": { "token": canary },
            })
        })
        .collect::<Vec<_>>();
    let diagnosis = super::temporal_health::diagnose(Some(&serde_json::json!({
        "status": "complete",
        "findings": findings,
        "message": canary,
        "database_key": canary,
    })));
    let rendered = diagnosis
        .lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!rendered.contains(canary));
    assert!(!rendered.contains("summary_text"));
    assert!(!rendered.contains("key_material"));
    assert!(!rendered.contains("payload"));
    assert!(rendered.contains("partial"));
    assert_eq!(diagnosis.finding_codes(), ["missing_anchor"]);
    assert!(rendered.contains("1000000"));
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
    let lock_root = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".tracedecay-database-locks");
    if let Ok(entries) = std::fs::read_dir(&lock_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if (name.ends_with(".access.lock")
                || name.ends_with(".writer.lock")
                || name.ends_with(".writer.owner")
                || name.ends_with(".bootstrap.lock"))
                && let Ok(metadata) = std::fs::metadata(&path)
            {
                manifest.insert(
                    format!("lock:{name}"),
                    (metadata.len(), metadata.modified().ok()),
                );
            }
        }
    }
    manifest
}

#[tokio::test]
async fn temporal_health_path_api_creates_no_authority_wal_shm_or_schema_artifacts() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal health family test",
    )
    .await;
    let db = runtime.database();
    let db_path = db.db_path().to_path_buf();
    let before_open = temporal_family_manifest(&db_path);
    assert!(
        before_open.keys().any(|name| name.ends_with("-wal")),
        "registered runtime should leave a WAL sidecar for this contract probe"
    );
    let live = db.session_temporal_doctor_health().await;
    assert_eq!(
        live.status(),
        crate::global_db::SessionTemporalHealthStatus::Complete
    );
    assert!(live.findings().is_empty());
    assert!(live.reason().is_none());
    assert_eq!(
        temporal_family_manifest(&db_path),
        before_open,
        "registered health snapshot must not mutate the live family"
    );
    db.checkpoint_result().await.unwrap();
    drop(runtime);

    // Drop the authority-held handle, then diagnose solely through the
    // immutable path API — the cold foreign Doctor/transport surface.
    let before_bytes = std::fs::read(&db_path).unwrap();
    let before_family = temporal_family_manifest(&db_path);
    let lock_root = db_path.parent().unwrap().join(".tracedecay-database-locks");
    let lock_names_before = if lock_root.is_dir() {
        std::fs::read_dir(&lock_root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };

    let report =
        crate::global_db::session_temporal::session_temporal_doctor_health_at(&db_path).await;
    assert_eq!(
        report.status(),
        crate::global_db::SessionTemporalHealthStatus::Complete
    );
    assert!(report.findings().is_empty());
    assert!(report.reason().is_none());
    assert_eq!(std::fs::read(&db_path).unwrap(), before_bytes);
    assert_eq!(temporal_family_manifest(&db_path), before_family);

    let lock_names_after = if lock_root.is_dir() {
        std::fs::read_dir(&lock_root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    assert_eq!(
        lock_names_after, lock_names_before,
        "immutable doctor health must not create authority/lock/owner files"
    );
    for suffix in ["-wal", "-shm"] {
        let mut path = db_path.as_os_str().to_os_string();
        path.push(suffix);
        let path = PathBuf::from(path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            path.exists(),
            before_family.contains_key(&name),
            "immutable doctor health must not create {suffix} sidecars"
        );
    }
}

#[tokio::test]
async fn temporal_health_missing_store_is_unavailable_without_artifacts() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("missing.db");
    let before = temporal_family_manifest(&missing);

    let report =
        crate::global_db::session_temporal::session_temporal_doctor_health_at(&missing).await;

    assert_eq!(
        report.status(),
        crate::global_db::SessionTemporalHealthStatus::Unavailable
    );
    assert!(report.findings().is_empty());
    assert!(report.reason().is_none());
    assert!(!missing.exists());
    assert_eq!(temporal_family_manifest(&missing), before);
    assert!(!dir.path().join(".tracedecay-database-locks").exists());
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

async fn temporal_health_test_count(db: &crate::global_db::RegisteredGlobalDb, sql: &str) -> i64 {
    let read = db.read_snapshot().await.unwrap();
    let mut rows = read.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn temporal_fts_health_and_repair_are_explicit_bounded_and_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal FTS repair test",
    )
    .await;
    let db = runtime.database();
    let db_path = db.db_path().to_path_buf();
    let writer = db.writer_connection().unwrap();
    writer
        .execute(
            "INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
             VALUES (9001, 'temporalorphan', 'temporalorphan')",
            (),
        )
        .await
        .unwrap();
    writer
        .execute(
            "INSERT INTO session_summary_nodes_fts(rowid, summary_text, index_text)
             VALUES (9002, 'temporalorphan', 'temporalorphan')",
            (),
        )
        .await
        .unwrap();
    db.checkpoint_result().await.unwrap();

    let before = std::fs::read(&db_path).unwrap();
    let report = serde_json::to_value(db.session_temporal_doctor_health().await).unwrap();
    assert_eq!(
        report["findings"],
        serde_json::json!([
            {"kind": "occurrence_fts_corruption", "count": 1},
            {"kind": "summary_fts_corruption", "count": 1},
        ])
    );
    assert_eq!(std::fs::read(&db_path).unwrap(), before);
    assert_eq!(
        temporal_health_test_count(
            db,
            "SELECT COUNT(*) FROM session_occurrences_fts
             WHERE session_occurrences_fts MATCH 'temporalorphan'",
        )
        .await,
        1
    );

    assert_eq!(db.repair_session_temporal_fts(false).await.unwrap(), (2, 0));
    assert_eq!(std::fs::read(&db_path).unwrap(), before);
    assert_eq!(
        temporal_health_test_count(
            db,
            "SELECT COUNT(*) FROM session_summary_nodes_fts
             WHERE session_summary_nodes_fts MATCH 'temporalorphan'",
        )
        .await,
        1
    );
    assert_eq!(
        temporal_health_test_count(db, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
    assert_eq!(
        temporal_health_test_count(db, "SELECT COUNT(*) FROM session_summary_nodes").await,
        0
    );
    assert_eq!(db.repair_session_temporal_fts(true).await.unwrap(), (2, 2));
    assert_eq!(
        temporal_health_test_count(
            db,
            "SELECT COUNT(*) FROM session_occurrences_fts
             WHERE session_occurrences_fts MATCH 'temporalorphan'",
        )
        .await,
        0
    );
    assert_eq!(
        temporal_health_test_count(
            db,
            "SELECT COUNT(*) FROM session_summary_nodes_fts
             WHERE session_summary_nodes_fts MATCH 'temporalorphan'",
        )
        .await,
        0
    );
    assert_eq!(
        temporal_health_test_count(db, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
    assert_eq!(
        temporal_health_test_count(db, "SELECT COUNT(*) FROM session_summary_nodes").await,
        0
    );
    assert_eq!(db.repair_session_temporal_fts(true).await.unwrap(), (0, 0));
}

#[tokio::test]
async fn temporal_fts_repair_accepts_exact_blob_index_damage() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor exact temporal FTS repair test",
    )
    .await;
    let db = runtime.database();
    let writer = db.writer_connection().unwrap();
    writer
        .execute(
            "INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
             VALUES (9003, 'malformedprobe', 'malformedprobe')",
            (),
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE session_occurrences_fts_data
             SET block = x'ff'
             WHERE id = (
                 SELECT MAX(id) FROM session_occurrences_fts_data WHERE id > 10
             )",
            (),
        )
        .await
        .unwrap();
    db.checkpoint_result().await.unwrap();

    let report = serde_json::to_value(db.session_temporal_doctor_health().await).unwrap();
    assert_eq!(
        report["findings"],
        serde_json::json!([
            {"kind": "occurrence_fts_corruption", "count": 1},
        ])
    );
    assert_eq!(db.repair_session_temporal_fts(true).await.unwrap(), (1, 1));
    db.checkpoint_result().await.unwrap();
    assert_eq!(
        serde_json::to_value(db.session_temporal_doctor_health().await).unwrap()["findings"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn temporal_health_detects_cross_session_ownership() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = DoctorTestRuntime::open(
        &dir.path().join("profile"),
        "doctor temporal ownership drift test",
    )
    .await;
    let db = runtime.database();
    db.checkpoint_result().await.unwrap();
    let writer = db.writer_connection().unwrap();
    writer
        .execute_batch(
            "DROP TRIGGER session_summary_sources_owner_guard_v1;
             INSERT INTO retrieval_anchors (
                 anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                 ('anchor-a', '{}', '{}', 'doctor-fixture'),
                 ('anchor-b', '{}', '{}', 'doctor-fixture');
             INSERT INTO session_summary_nodes (
                 summary_id, session_id, summary_anchor_id, summary_text, index_text,
                 source_horizon_json, publication_json, created_at
             ) VALUES
                 ('summary-a', 'session-a', 'anchor-a', 'a', 'a', '{}', NULL, 1),
                 ('summary-b', 'session-b', 'anchor-b', 'b', 'b', '{}', NULL, 2);
             INSERT INTO session_summary_sources (
                 summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-b', 0, 'summary', NULL, 'summary-a');",
        )
        .await
        .unwrap();
    db.checkpoint_result().await.unwrap();

    let report = serde_json::to_value(db.session_temporal_doctor_health().await).unwrap();
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["kind"] == "ownership_drift")
    );
}

#[test]
fn temporal_fts_classifier_rejects_whole_database_corruption() {
    assert!(
        crate::global_db::SessionTemporalHealthReport::is_fts_virtual_table_error_code_for_test(
            267
        )
    );
    assert!(
        !crate::global_db::SessionTemporalHealthReport::is_fts_virtual_table_error_code_for_test(
            11
        )
    );
    assert!(
        crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "malformed inverted index for FTS5 table main.session_occurrences_fts",
            true,
            false,
        )
    );
    assert!(
        crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "fts5: corruption found reading blob 137438953473 from table \"session_occurrences_fts\"",
            true,
            false,
        )
    );
    assert!(
        !crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "fts5: corruption found reading blob 137438953473 from table \"session_summary_nodes_fts\"",
            true,
            false,
        )
    );
    assert!(
        !crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "fts5: corruption found reading blob unknown from table \"session_occurrences_fts\"",
            true,
            false,
        )
    );
    assert!(
        !crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "database disk image is malformed",
            true,
            false,
        )
    );
    assert!(
        !crate::global_db::SessionTemporalHealthReport::is_allowed_fts_quick_check_for_test(
            "malformed inverted index for FTS5 table main.session_summary_nodes_fts",
            true,
            false,
        )
    );
}

#[test]
fn daemon_runtime_parser_rejects_missing_json_payload() {
    let error = super::daemon_runtime_status(&serde_json::json!({ "content": [] })).unwrap_err();
    assert!(error.to_string().contains("returned no JSON payload"));
}

#[test]
fn daemon_runtime_parser_rejects_missing_database_telemetry() {
    let error = super::daemon_runtime_status(&serde_json::json!({
        "content": [{"type": "text", "text": r#"{"process":{"pid":1234}}"#}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("omitted database telemetry"));
}

fn host_component_result(
    state: crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1,
    artifact_states: &[(
        &str,
        crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1,
    )],
) -> crate::agents::host_bundle_v2::HostBundleComponentDoctorResultV1 {
    use crate::agents::host_bundle_v2::{
        HostBundleArtifactDoctorResultV1, HostBundleComponentDoctorResultV1, HostBundleComponentV1,
        HostBundleRegistrationStateV1, HostKindV1,
    };

    HostBundleComponentDoctorResultV1 {
        receipt_path: PathBuf::from("/profile/host-components/receipt.cursor-desktop.core.v1.json"),
        host: Some(HostKindV1::CursorDesktop),
        component: Some(HostBundleComponentV1::Core),
        state,
        registration: Some(HostBundleRegistrationStateV1::Current),
        artifacts: artifact_states
            .iter()
            .map(|(relative_path, state)| HostBundleArtifactDoctorResultV1 {
                relative_path: (*relative_path).to_string(),
                expected_digest: [1; 32],
                observed_digest: Some([2; 32]),
                ownership_marker: "tracedecay.cursor-desktop.core".to_string(),
                state: *state,
            })
            .collect(),
        repair_action: "run `tracedecay reinstall --component core --yes` (backs up and re-owns)"
            .to_string(),
    }
}

/// Cursor Core's receipt-owned bundle drifts on every version bump (the plugin
/// manifest stamps the version, the hook config bakes the binary path). That is
/// repairable, so Doctor must warn and exit zero rather than fail.
#[test]
fn drifted_host_component_warns_and_keeps_a_clean_exit() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let mut counters = DoctorCounters::new();
    let component = host_component_result(
        State::Drifted,
        &[
            (
                ".cursor/plugins/local/tracedecay/hooks/hooks.json",
                State::Drifted,
            ),
            (".cursor/plugins/local/tracedecay/mcp.json", State::Current),
        ],
    );

    super::report_host_component_state(&mut counters, &component);

    assert_eq!(counters.issues, 0, "drift must not fail the doctor run");
    assert_eq!(counters.warnings, 1);
    super::doctor_result(
        &counters,
        Ok(serde_json::json!({})),
        &super::DatabaseHealth::Healthy,
    )
    .unwrap();
}

/// A host that activates only through its own UI leaves its staged components
/// unmaterialised until the operator clicks through. No unattended command can
/// converge that, so Doctor must report it as a pending user action and keep a
/// clean exit — otherwise every such machine fails the strict gate forever.
#[test]
fn activation_deferred_host_component_warns_without_blocking() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let mut counters = DoctorCounters::new();
    let component = host_component_result(
        State::ActivationDeferred,
        &[(".codex/plugins/tracedecay/.mcp.json", State::Missing)],
    );

    super::report_host_component_state(&mut counters, &component);

    assert_eq!(
        counters.issues, 0,
        "a pending interactive activation must not fail the doctor run"
    );
    assert_eq!(counters.warnings, 1);
    super::doctor_result(
        &counters,
        Ok(serde_json::json!({})),
        &super::DatabaseHealth::Healthy,
    )
    .unwrap();
}

/// The same absent artifact under the blocking classification still fails, so
/// the deferral above is a classification change, not a weaker doctor.
#[test]
fn missing_host_component_artifacts_still_fail_the_doctor_run() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let mut counters = DoctorCounters::new();
    let component = host_component_result(
        State::Missing,
        &[(".codex/plugins/tracedecay/.mcp.json", State::Missing)],
    );

    super::report_host_component_state(&mut counters, &component);

    assert_eq!(counters.issues, 1);
}

/// A contested path is not repairable without an operator decision, so it
/// keeps failing — the distinction the `Drifted` state exists to preserve.
#[test]
fn ownership_conflict_still_fails_the_doctor_run() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let mut counters = DoctorCounters::new();
    let component = host_component_result(
        State::OwnershipConflict,
        &[(
            ".cursor/plugins/local/tracedecay/mcp.json",
            State::OwnershipConflict,
        )],
    );

    super::report_host_component_state(&mut counters, &component);

    assert_eq!(counters.issues, 1);
    let error = super::doctor_result(
        &counters,
        Ok(serde_json::json!({})),
        &super::DatabaseHealth::Healthy,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "config error: doctor found 1 issue(s)");
}

/// An uninstall that left the host still advertising the component is
/// reported, not silently skipped, and is repairable rather than blocking.
#[test]
fn orphaned_registration_warns_without_blocking() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let mut counters = DoctorCounters::new();

    super::report_host_component_state(
        &mut counters,
        &host_component_result(State::OrphanedRegistration, &[]),
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

/// The warning has to name the drifted paths, so an operator can tell which
/// receipt-owned files moved without rerunning anything.
#[test]
fn drift_warning_names_only_the_drifted_paths() {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let component = host_component_result(
        State::Drifted,
        &[
            (
                ".cursor/plugins/local/tracedecay/hooks/hooks.json",
                State::Drifted,
            ),
            (
                ".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json",
                State::Drifted,
            ),
            (".cursor/plugins/local/tracedecay/mcp.json", State::Current),
        ],
    );

    let named = super::drifted_paths(&component);

    assert_eq!(
        named,
        ".cursor/plugins/local/tracedecay/hooks/hooks.json, \
         .cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json"
    );
}

#[test]
fn doctor_result_fails_when_checks_report_issues() {
    let mut counters = DoctorCounters::new();
    counters.fail("broken integration");

    let error = super::doctor_result(
        &counters,
        Ok(serde_json::json!({})),
        &DatabaseHealth::Healthy,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "config error: doctor found 1 issue(s)");
}

#[test]
fn doctor_result_allows_warnings_without_issues() {
    let mut counters = DoctorCounters::new();
    counters.warn("optional check unavailable");

    super::doctor_result(
        &counters,
        Ok(serde_json::json!({})),
        &DatabaseHealth::Healthy,
    )
    .unwrap();
}

#[test]
fn doctor_result_preserves_daemon_and_storage_errors() {
    let mut counters = DoctorCounters::new();
    counters.fail("broken integration");
    let daemon_error = crate::errors::TraceDecayError::Config {
        message: "daemon unavailable".to_string(),
    };

    let failed = DatabaseHealth::Failed {
        reason: "daemon_diagnostics_unavailable".to_string(),
    };

    let error = super::doctor_result(&counters, Err(daemon_error), &failed).unwrap_err();
    assert_eq!(error.to_string(), "config error: daemon unavailable");

    let error = super::doctor_result(&counters, Ok(serde_json::json!({})), &failed).unwrap_err();
    assert_eq!(
        error.to_string(),
        "config error: doctor storage health check failed [daemon_diagnostics_unavailable]"
    );
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
    .expect_err("terminal project-open error must fail the dogfood health wait");

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
        .expect_err("terminal corruption must fail dogfood health validation");
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

#[cfg(unix)]
fn write_executable_script(directory: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    let path = directory.join(name);
    std::fs::write(&path, body)?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions)?;
    Ok(path)
}

#[cfg(unix)]
#[tokio::test]
async fn language_analyzer_probe_reports_present_executable()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    write_executable_script(
        temp.path(),
        "typescript-language-server",
        "#!/bin/sh\necho 'typescript-language-server 4.3.3'\n",
    )?;

    let result = probe_language_analyzer_with_path(
        "typescript-language-server",
        "typescript-language-server",
        &["--version"],
        false,
        Some(temp.path().as_os_str()),
    )
    .await;

    assert!(matches!(
        result,
        LanguageAnalyzerProbe::Present { version }
            if version == "typescript-language-server 4.3.3"
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn language_analyzer_probe_reports_missing_executable() {
    let temp = tempfile::tempdir().expect("create empty PATH directory");

    let result = probe_language_analyzer_with_path(
        "pyright-langserver",
        "pyright",
        &["--version"],
        false,
        Some(temp.path().as_os_str()),
    )
    .await;

    assert!(matches!(result, LanguageAnalyzerProbe::Missing));
}

#[cfg(unix)]
#[tokio::test]
async fn language_analyzer_probe_reports_resolvable_but_broken_server()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    write_executable_script(
        temp.path(),
        "gopls",
        "#!/bin/sh\necho 'gopls startup configuration is invalid' >&2\nexit 7\n",
    )?;

    let result = probe_language_analyzer_with_path(
        "gopls",
        "gopls",
        &["version"],
        false,
        Some(temp.path().as_os_str()),
    )
    .await;

    assert!(matches!(
        result,
        LanguageAnalyzerProbe::Broken { detail }
            if detail.contains("gopls startup configuration is invalid")
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn language_analyzer_probe_detects_rustup_shim_without_component()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    write_executable_script(
        temp.path(),
        "rust-analyzer",
        "#!/bin/sh\necho \"error: 'rust-analyzer' is not installed for the toolchain 'stable'\" >&2\necho 'To install, run `rustup component add rust-analyzer`' >&2\nexit 1\n",
    )?;
    write_executable_script(
        temp.path(),
        "rustup",
        "#!/bin/sh\nif [ \"$1 $2 $3\" = \"component list --installed\" ]; then\n  echo 'rustfmt-x86_64-unknown-linux-gnu'\n  exit 0\nfi\nexit 2\n",
    )?;

    let result = probe_language_analyzer_with_path(
        "rust-analyzer",
        "rust-analyzer",
        &["--version"],
        true,
        Some(temp.path().as_os_str()),
    )
    .await;

    assert!(matches!(
        result,
        LanguageAnalyzerProbe::RustupComponentMissing
    ));
    Ok(())
}

/// `start_paused`: the analyzer that never answers sleeps in real time, so the
/// only timer the runtime can advance to is the probe deadline. That grades the
/// production `LANGUAGE_ANALYZER_PROBE_TIMEOUT` budget without spending it.
///
/// The probe pins the child's `PATH` to the fixture directory so it resolves the
/// stub, which also hides `sleep`; the script restores a system `PATH` so it
/// blocks instead of exiting 127 and grading as broken.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn language_analyzer_probe_reports_timed_out_version_probe()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    write_executable_script(
        temp.path(),
        "clangd",
        "#!/bin/sh\nPATH=/usr/bin:/bin\nexec sleep 30\n",
    )?;

    let result = probe_language_analyzer_with_path(
        "clangd",
        "clangd",
        &["--version"],
        false,
        Some(temp.path().as_os_str()),
    )
    .await;

    assert_eq!(result, LanguageAnalyzerProbe::TimedOut);
    Ok(())
}

#[test]
fn timed_out_analyzer_warning_is_distinct_from_missing_and_broken() {
    let analyzer = LanguageAnalyzerSpec {
        command: "gopls".to_string(),
        probe_command: "gopls".to_string(),
        version_args: &["version"],
        languages: vec!["go".to_string()],
        remedy: Some("go install golang.org/x/tools/gopls@latest".to_string()),
        rustup_component: false,
    };
    let timed_out_problem = format!(
        "resolved, but its version probe timed out after {}s",
        LANGUAGE_ANALYZER_PROBE_TIMEOUT.as_secs()
    );

    let timed_out = analyzer_warning(&analyzer, "go", &timed_out_problem);

    assert_eq!(
        timed_out,
        format!(
            "gopls resolved, but its version probe timed out after {}s; LSP context projection for go is unavailable. Install with `go install golang.org/x/tools/gopls@latest`.",
            LANGUAGE_ANALYZER_PROBE_TIMEOUT.as_secs()
        )
    );

    let missing = analyzer_warning(&analyzer, "go", "was not found on PATH");
    let broken = analyzer_warning(
        &analyzer,
        "go",
        "resolved, but its version probe failed: exit status: 7",
    );
    assert_ne!(
        timed_out, missing,
        "an unresponsive analyzer must not read as absent"
    );
    assert_ne!(
        timed_out, broken,
        "an unresponsive analyzer must not read as a failed probe"
    );
}

#[test]
fn timed_out_custom_analyzer_warning_reports_the_absent_remedy() {
    let analyzer = LanguageAnalyzerSpec {
        command: "example-language-server".to_string(),
        probe_command: "example-language-server".to_string(),
        version_args: &["--version"],
        languages: vec!["example".to_string()],
        remedy: None,
        rustup_component: false,
    };

    let warning = analyzer_warning(
        &analyzer,
        "example",
        &format!(
            "resolved, but its version probe timed out after {}s",
            LANGUAGE_ANALYZER_PROBE_TIMEOUT.as_secs()
        ),
    );

    assert_eq!(
        warning,
        format!(
            "example-language-server resolved, but its version probe timed out after {}s; LSP context projection for example is unavailable. No install remedy is configured for this custom adapter.",
            LANGUAGE_ANALYZER_PROBE_TIMEOUT.as_secs()
        )
    );
}

#[test]
fn configured_language_analyzers_cover_builtins_and_custom_adapters() {
    let mut settings = CodeDiagnosticsSettings::default();
    settings.custom_adapters.push(LspAdapterDefinition {
        language: "example".to_string(),
        language_id: "example".to_string(),
        command: "example-language-server".to_string(),
        args: vec!["--stdio".to_string()],
        extensions: vec!["example".to_string()],
        root_markers: Vec::new(),
        install_options: vec![LspInstallOption {
            label: "example package manager".to_string(),
            command: "example install example-language-server".to_string(),
            notes: None,
        }],
        diagnostics: DiagnosticMode::Push,
    });

    let analyzers = configured_language_analyzers(&settings);
    let commands = analyzers
        .iter()
        .map(|analyzer| analyzer.command.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        commands,
        BTreeSet::from([
            "clangd",
            "example-language-server",
            "gopls",
            "intelephense",
            "lua-language-server",
            "pyright-langserver",
            "rust-analyzer",
            "typescript-language-server",
            "zls",
        ])
    );
    assert_eq!(analyzers[0].command, "rust-analyzer");
    let custom = analyzers
        .iter()
        .find(|analyzer| analyzer.command == "example-language-server")
        .expect("custom analyzer is included");
    assert_eq!(
        custom.remedy.as_deref(),
        Some("example install example-language-server")
    );
}
