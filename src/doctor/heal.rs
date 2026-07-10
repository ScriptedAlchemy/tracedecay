//! Post-update health pass: safe, automatic repairs plus a concise summary
//! of the doctor findings that still need a human decision.
//!
//! Runs at the end of `tracedecay update` / `tracedecay post-update`. Running
//! by default (opt-out via `--no-heal`) is intentional product policy: the
//! hidden `post-update` subcommand fires from the self-update re-exec path,
//! so every successful `tracedecay update` heals the store unless the user
//! explicitly skips it. Every step is failure-tolerant: a failing check
//! prints a warning but never fails the update itself. Only remedies that
//! are safe to automate are applied:
//!
//! - corrupt `branch-meta.json` files (anything [`crate::branch_meta::parse`]
//!   rejects) are quarantined — renamed to
//!   `branch-meta.json.corrupt-<timestamp>`, never deleted — preserving the
//!   evidence while restoring the silent single-DB fallback,
//! - registry rows whose project root no longer exists AND lives under the
//!   system temp directory are purged (the automated equivalent of
//!   `tracedecay migrate registry-gc --prefix <tmp> --apply`), and only when
//!   BOTH the canonical and display roots are gone.
//! - input store manifests from completed schema-2 consolidations are renamed
//!   out of the canonical discovery path after the applied ledger and both
//!   repository markers prove the destination identity.
//!
//! Those auto-applied remedies are safe precisely because quarantine renames
//! instead of deleting and the GC removes only temp-rooted registry metadata
//! whose every known root has vanished — no user data is ever destroyed.
//!
//! Everything else (orphan store manifests, stale rows outside the temp
//! directory, registry/manifest identity drift) is only reported.

use std::path::{Path, PathBuf};

use crate::global_db::{CodeProjectRecord, GlobalDb};
use crate::migrate::registry::{StaleRootScope, code_project_root_exists, stale_code_projects};
use crate::storage::{BRANCH_META_FILENAME, BRANCH_META_QUARANTINE_PREFIX};

/// A corrupt `branch-meta.json` that was renamed out of the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMetaQuarantine {
    pub original: PathBuf,
    pub quarantined: PathBuf,
}

/// Outcome of one post-update health pass.
#[derive(Debug, Default)]
pub struct HealthPassReport {
    /// Input manifests retired from already-applied consolidations.
    pub retired_consolidation_manifests: Vec<PathBuf>,
    /// Superseded source/target registry projects removed after validation.
    pub retired_consolidation_registry_projects: usize,
    pub quarantined_branch_meta: Vec<BranchMetaQuarantine>,
    /// `None` when the global DB could not be opened, so the GC never ran.
    pub purged_temp_registry_rows: Option<usize>,
    /// Stale store manifests reconciled to the registry canonical path.
    pub reconciled_store_roots: Vec<super::registry_drift::ReconciledStoreRoot>,
    pub remaining_findings: Vec<String>,
    pub warnings: Vec<String>,
}

#[doc(hidden)]
pub async fn run_post_update_health_pass_under_lease(
    lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
) -> HealthPassReport {
    let Some(profile_root) = crate::config::user_data_dir() else {
        return render_missing_profile_report();
    };
    if let Some(warning) = health_pass_lease_error(
        lifecycle_lease,
        &profile_root,
        crate::daemon::daemon_reachable(),
    ) {
        let report = HealthPassReport {
            warnings: vec![warning],
            ..HealthPassReport::default()
        };
        render_warnings(&report.warnings);
        return report;
    }
    run_post_update_health_pass_for_profile(&profile_root).await
}

fn health_pass_lease_error(
    lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    profile_root: &Path,
    daemon_reachable: bool,
) -> Option<String> {
    if !lifecycle_lease.is_exclusive() {
        return Some("post-update health pass requires an exclusive lifecycle lease".to_string());
    }
    if !lifecycle_lease.guards_profile(profile_root) {
        return Some(format!(
            "post-update health pass lifecycle lease does not guard profile '{}'",
            profile_root.display()
        ));
    }
    daemon_reachable.then(|| {
        "post-update health pass requires the TraceDecay daemon to be unreachable".to_string()
    })
}

async fn run_post_update_health_pass_for_profile(profile_root: &Path) -> HealthPassReport {
    eprintln!("\n\x1b[1mPost-update health pass\x1b[0m (skip with --no-heal)");
    let report = compute_health_pass_report(profile_root).await;
    render_health_pass_report(&report);
    report
}

fn render_missing_profile_report() -> HealthPassReport {
    let report = HealthPassReport {
        warnings: vec!["could not determine the profile data directory".to_string()],
        ..HealthPassReport::default()
    };
    render_warnings(&report.warnings);
    report
}

/// Applies the safe remedies and gathers everything the pass has to say into
/// a [`HealthPassReport`], without printing anything.
async fn compute_health_pass_report(profile_root: &Path) -> HealthPassReport {
    let mut report = HealthPassReport::default();

    // The post-update command holds the exclusive lifecycle lease around this
    // entire pass, so manifest retirement cannot race daemon or hook opens.
    retire_completed_consolidation_manifests(profile_root, &mut report).await;

    let (quarantined, warnings) = quarantine_corrupt_branch_meta(profile_root);
    report.quarantined_branch_meta = quarantined;
    report.warnings.extend(warnings);

    // Opening the global DB applies its idempotent schema migrations — the
    // same lazy upgrade every normal open path performs.
    let Some(global_db) = GlobalDb::open().await else {
        report
            .warnings
            .push("could not open the global DB for the health pass".to_string());
        return report;
    };

    // One registry snapshot for the whole pass: the GC and the remaining
    // findings below both work from this list.
    let projects = global_db.list_code_projects(usize::MAX).await;
    let (purged, purged_ids) = gc_stale_temp_registry_rows(&global_db, &projects).await;
    report.purged_temp_registry_rows = Some(purged);

    let registry_drift =
        super::registry_drift::registry_drift_findings(&global_db, profile_root).await;
    let (reconciled, reconcile_warnings) =
        super::registry_drift::reconcile_drifted_store_roots_from_findings(&registry_drift);
    let remaining_registry_drift_count =
        count_remaining_registry_drift(&registry_drift, &reconciled);
    report.reconciled_store_roots = reconciled;
    report.warnings.extend(reconcile_warnings);

    let (findings, warnings) = collect_remaining_findings(
        &global_db,
        profile_root,
        &projects,
        &purged_ids,
        remaining_registry_drift_count,
    )
    .await;
    report.remaining_findings = findings;
    report.warnings.extend(warnings);
    report
}

async fn retire_completed_consolidation_manifests(
    profile_root: &Path,
    report: &mut HealthPassReport,
) {
    let retirement =
        crate::migrate::consolidate::retire_applied_input_manifests(profile_root).await;
    report.retired_consolidation_manifests = retirement.retired;
    report.retired_consolidation_registry_projects = retirement.retired_registry_projects;
    report.warnings.extend(retirement.warnings);
}

/// Prints the doctor-style summary for a computed report.
fn render_health_pass_report(report: &HealthPassReport) {
    if report.retired_consolidation_manifests.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No completed consolidation manifests to retire");
    } else {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Retired {} completed consolidation input manifest(s):",
            report.retired_consolidation_manifests.len()
        );
        for path in &report.retired_consolidation_manifests {
            eprintln!("      • {}", path.display());
        }
    }
    if report.retired_consolidation_registry_projects > 0 {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Retired {} superseded consolidation registry project(s)",
            report.retired_consolidation_registry_projects
        );
    }

    if report.quarantined_branch_meta.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No corrupt branch metadata files");
    } else {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Quarantined {} corrupt branch metadata file(s):",
            report.quarantined_branch_meta.len()
        );
        for quarantine in &report.quarantined_branch_meta {
            eprintln!("      • {}", quarantine.quarantined.display());
        }
    }

    match report.purged_temp_registry_rows {
        Some(0) => eprintln!("  \x1b[32m✔\x1b[0m No stale temp-root registry rows"),
        Some(purged) => {
            eprintln!("  \x1b[32m✔\x1b[0m Purged {purged} stale temp-root registry row(s)");
        }
        None => {}
    }

    if report.reconciled_store_roots.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No stale store manifest roots to reconcile");
    } else {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Reconciled {} stale store manifest root(s):",
            report.reconciled_store_roots.len()
        );
        for reconciled in &report.reconciled_store_roots {
            eprintln!("      • {}", reconciled.manifest_path.display());
            if let Some(config_path) = &reconciled.config_path {
                eprintln!("        (config: {})", config_path.display());
            }
        }
    }

    if report.remaining_findings.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No remaining doctor findings");
    } else {
        eprintln!("  Remaining findings (not auto-fixed — run `tracedecay doctor` for details):");
        for finding in &report.remaining_findings {
            eprintln!("      • {finding}");
        }
    }
    render_warnings(&report.warnings);
}

fn render_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("  \x1b[33mwarning:\x1b[0m health pass: {warning}");
    }
}

/// Renames every `branch-meta.json` under `<profile_root>/projects/*` that
/// [`crate::branch_meta::parse`] rejects — the runtime's own definition of
/// corrupt, covering both invalid JSON and schema mismatches — to
/// `branch-meta.json.corrupt-<timestamp>`, preserving the corrupt content as
/// evidence while restoring the single-DB fallback.
///
/// Returns the performed quarantines and any warnings.
fn quarantine_corrupt_branch_meta(profile_root: &Path) -> (Vec<BranchMetaQuarantine>, Vec<String>) {
    let mut quarantines = Vec::new();
    let mut warnings = Vec::new();
    let projects_root = profile_root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return (quarantines, warnings);
    };
    let mut meta_paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path().join(BRANCH_META_FILENAME))
        .filter(|path| path.is_file())
        .collect();
    meta_paths.sort();

    let now = crate::tracedecay::current_timestamp();
    for path in meta_paths {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                warnings.push(format!("could not read '{}': {err}", path.display()));
                continue;
            }
        };
        if crate::branch_meta::parse(&content).is_ok() {
            continue;
        }
        let quarantined = path.with_file_name(format!("{BRANCH_META_QUARANTINE_PREFIX}{now}"));
        match std::fs::rename(&path, &quarantined) {
            Ok(()) => quarantines.push(BranchMetaQuarantine {
                original: path,
                quarantined,
            }),
            Err(err) => warnings.push(format!(
                "could not quarantine corrupt '{}': {err}",
                path.display()
            )),
        }
    }
    (quarantines, warnings)
}

/// Purges registry rows in the auto-GC scope: canonical root under the
/// system temp directory AND every known root gone
/// ([`StaleRootScope::AllRootsMissing`]) — the only registry GC scope that is
/// safe to run without review.
///
/// Returns the purged row count plus the candidate ids, so the remaining
/// findings can exclude them from the shared pre-purge registry snapshot.
async fn gc_stale_temp_registry_rows(
    global_db: &GlobalDb,
    projects: &[CodeProjectRecord],
) -> (usize, Vec<String>) {
    let stale_ids: Vec<String> = stale_code_projects(
        projects,
        &temp_dir_prefixes(),
        StaleRootScope::AllRootsMissing,
    )
    .into_iter()
    .map(|project| project.project_id.clone())
    .collect();
    if stale_ids.is_empty() {
        return (0, stale_ids);
    }
    let purged = global_db.delete_code_projects(&stale_ids).await;
    (purged, stale_ids)
}

/// The system temp directory in both its literal and canonicalized spellings,
/// so registry rows recorded through a symlinked temp path still match.
fn temp_dir_prefixes() -> Vec<PathBuf> {
    let temp_dir = std::env::temp_dir();
    let mut prefixes = vec![temp_dir.clone()];
    if let Ok(canonical) = temp_dir.canonicalize() {
        if !prefixes.contains(&canonical) {
            prefixes.push(canonical);
        }
    }
    prefixes
}

/// Summarizes the doctor findings that are NOT safe to auto-apply so the user
/// sees them at the end of `tracedecay update` output. `projects` is the
/// pre-purge registry snapshot; rows in `purged_ids` are skipped.
///
/// Returns the findings and any warnings.
async fn collect_remaining_findings(
    global_db: &GlobalDb,
    profile_root: &Path,
    projects: &[CodeProjectRecord],
    purged_ids: &[String],
    remaining_registry_drift_count: usize,
) -> (Vec<String>, Vec<String>) {
    let mut findings = Vec::new();
    let (orphan_count, warnings) =
        super::orphan_store_manifest_report(global_db, profile_root).await;
    if orphan_count > 0 {
        findings.push(format!(
            "{orphan_count} orphan profile store manifest(s) can reconstruct registry rows"
        ));
    }

    let stale_rows = projects
        .iter()
        .filter(|project| !purged_ids.contains(&project.project_id))
        .filter(|project| !code_project_root_exists(project))
        .count();
    if stale_rows > 0 {
        findings.push(format!(
            "{stale_rows} stale code project registry row(s) outside the temp directory"
        ));
    }

    if remaining_registry_drift_count > 0 {
        findings.push(format!(
            "{remaining_registry_drift_count} registry/store manifest identity drift finding(s)"
        ));
    }
    (findings, warnings)
}

fn count_remaining_registry_drift(
    drift: &[super::registry_drift::RegistryDriftFinding],
    reconciled: &[super::registry_drift::ReconciledStoreRoot],
) -> usize {
    drift
        .iter()
        .filter(|finding| {
            finding.field != "project_root"
                || !reconciled.iter().any(|entry| {
                    entry.store_id == finding.store_id
                        && entry.manifest_path == finding.manifest_path
                })
        })
        .count()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn write_branch_meta(projects_root: &Path, project_id: &str, content: &str) -> PathBuf {
        let shard = projects_root.join(project_id);
        std::fs::create_dir_all(&shard).unwrap();
        let path = shard.join(BRANCH_META_FILENAME);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn quarantine_renames_only_corrupt_branch_meta() {
        let dir = tempfile::TempDir::new().unwrap();
        let projects_root = dir.path().join("projects");
        let corrupt = write_branch_meta(&projects_root, "proj_corrupt", "{not valid json");
        let valid = write_branch_meta(
            &projects_root,
            "proj_valid",
            r#"{"default_branch":"main","branches":{}}"#,
        );

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert_eq!(quarantines.len(), 1);
        assert!(warnings.is_empty());
        let quarantine = &quarantines[0];
        assert_eq!(quarantine.original, corrupt);
        assert!(!corrupt.exists(), "corrupt file should be renamed away");
        assert_eq!(
            std::fs::read_to_string(&quarantine.quarantined).unwrap(),
            "{not valid json",
            "quarantined file must preserve the corrupt content as evidence"
        );
        assert!(
            quarantine
                .quarantined
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(BRANCH_META_QUARANTINE_PREFIX),
            "quarantine name should be branch-meta.json.corrupt-<timestamp>: {quarantine:?}"
        );
        assert!(valid.exists(), "valid branch-meta must be left untouched");
    }

    #[test]
    fn quarantine_treats_schema_mismatch_as_corrupt() {
        let dir = tempfile::TempDir::new().unwrap();
        let projects_root = dir.path().join("projects");
        // Valid JSON, but not a valid BranchMeta — the runtime warns
        // "corrupt" on every open, so the health pass must agree.
        let schema_corrupt =
            write_branch_meta(&projects_root, "proj_schema", r#"{"default_branch": 5}"#);

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert!(warnings.is_empty());
        assert_eq!(
            quarantines.len(),
            1,
            "schema-corrupt branch-meta must be quarantined: {quarantines:?}"
        );
        assert_eq!(quarantines[0].original, schema_corrupt);
        assert!(!schema_corrupt.exists());
        assert_eq!(
            std::fs::read_to_string(&quarantines[0].quarantined).unwrap(),
            r#"{"default_branch": 5}"#,
            "quarantined file must preserve the corrupt content as evidence"
        );
    }

    #[test]
    fn quarantine_is_a_no_op_without_a_projects_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());
        assert!(quarantines.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn under_lease_health_pass_rejects_shared_wrong_profile_and_reachable_daemon() {
        let dir = tempfile::TempDir::new().unwrap();
        let shared = match crate::lifecycle_lease::try_acquire_shared_for_profile(
            dir.path(),
            "shared healer test",
        )
        .unwrap()
        {
            crate::lifecycle_lease::SharedLeaseAttempt::Acquired(lease) => lease,
            crate::lifecycle_lease::SharedLeaseAttempt::Busy => panic!("unexpected busy lease"),
        };
        assert!(
            health_pass_lease_error(&shared, dir.path(), false)
                .unwrap()
                .contains("requires an exclusive lifecycle lease")
        );
        drop(shared);

        let guarded_profile = dir.path().join("guarded");
        let other_profile = dir.path().join("other");
        let exclusive = crate::lifecycle_lease::acquire_exclusive_for_profile(
            &guarded_profile,
            "exclusive healer test",
        )
        .unwrap();
        assert!(
            health_pass_lease_error(&exclusive, &other_profile, false)
                .unwrap()
                .contains("does not guard profile")
        );
        assert!(
            health_pass_lease_error(&exclusive, &guarded_profile, true)
                .unwrap()
                .contains("daemon to be unreachable")
        );
        assert!(health_pass_lease_error(&exclusive, &guarded_profile, false).is_none());
    }

    #[tokio::test]
    async fn post_update_retires_applied_consolidation_manifests_idempotently() {
        let dir = tempfile::TempDir::new().unwrap();
        let project = dir.path().join("repo");
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project)
            .status()
            .unwrap();
        assert!(status.success());
        let project = project.canonicalize().unwrap();
        let git_common_dir = crate::worktree::git_common_dir(&project).unwrap();
        let source_id = "proj_heal_source";
        let target_id = "proj_heal_target";
        let destination_id = crate::migrate::consolidate::destination_project_id(
            &git_common_dir,
            source_id,
            target_id,
        );
        for project_id in [source_id, target_id, destination_id.as_str()] {
            let layout = crate::storage::profile_sharded_layout(
                &project,
                &profile,
                &crate::storage::EnrollmentMarker {
                    project_id: project_id.to_string(),
                    storage_mode: crate::storage::StorageMode::ProfileSharded,
                },
            )
            .unwrap();
            std::fs::create_dir_all(&layout.data_root).unwrap();
            crate::storage::write_store_manifest(&layout).unwrap();
        }
        crate::storage::write_repository_identity_marker(&project, &destination_id).unwrap();
        crate::storage::write_enrollment_marker(
            &project,
            &crate::storage::EnrollmentMarker {
                project_id: destination_id.clone(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();

        let migration_id = format!("consolidate_{}", &destination_id[5..]);
        let ledger_root = profile.join("migration-inventory");
        std::fs::create_dir_all(&ledger_root).unwrap();
        std::fs::write(
            ledger_root.join(format!("{migration_id}.json")),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 2,
                "migration_id": migration_id,
                "confirmation_token": "confirm-healer",
                "input_fingerprint": "healer-fixture",
                "source_project_id": source_id,
                "target_project_id": target_id,
                "destination_project_id": destination_id.clone(),
                "project_root": project,
                "git_common_dir": git_common_dir,
                "state": "applied",
                "graph_offsets": [],
                "session_offsets": null,
                "preserved_collisions": []
            }))
            .unwrap(),
        )
        .unwrap();

        let global = crate::global_db::GlobalDb::open_at(&profile.join("global.db"))
            .await
            .unwrap();
        for project_id in [source_id, target_id, destination_id.as_str()] {
            global
                .upsert_code_project(
                    project_id,
                    &project,
                    Some(&git_common_dir),
                    None,
                    Some("main"),
                )
                .await
                .unwrap();
            global
                .upsert_store_instance(crate::global_db::StoreInstanceUpsert {
                    store_id: format!("store:{project_id}:profile_sharded"),
                    project_id: project_id.to_string(),
                    store_kind: "code_project".to_string(),
                    storage_mode: "profile_sharded".to_string(),
                    store_relpath: format!("projects/{project_id}"),
                    manifest_relpath: Some(format!(
                        "projects/{project_id}/{}",
                        crate::storage::STORE_MANIFEST_FILENAME
                    )),
                    last_verified_at: Some(1_800_000_000),
                    last_write_at: Some(1_800_000_000),
                })
                .await
                .unwrap();
        }
        global
            .upsert_project_alias(&project, &destination_id)
            .await
            .unwrap();
        global.checkpoint().await;
        global.close();

        let _lease = crate::lifecycle_lease::acquire_exclusive_for_profile(
            &profile,
            "post-update healer test",
        )
        .unwrap();
        std::fs::write(
            profile
                .join("migration-inventory")
                .join(".fail-registry-retirement-once"),
            b"fail once",
        )
        .unwrap();
        let mut interrupted = HealthPassReport::default();
        retire_completed_consolidation_manifests(&profile, &mut interrupted).await;
        assert_eq!(interrupted.warnings.len(), 1);
        assert!(interrupted.warnings[0].contains("synthetic registry retirement failure"));
        for project_id in [source_id, target_id] {
            let root = profile.join("projects").join(project_id);
            assert!(
                !root.join(crate::storage::STORE_MANIFEST_FILENAME).exists()
                    && root
                        .join(format!(
                            "store_manifest.consolidated-into-{destination_id}.json"
                        ))
                        .is_file()
            );
        }
        let global = crate::global_db::GlobalDb::open_at(&profile.join("global.db"))
            .await
            .unwrap();
        assert_eq!(global.list_code_projects(usize::MAX).await.len(), 3);
        global
            .conn()
            .execute(
                "UPDATE code_projects SET canonical_root=?1 WHERE project_id=?2",
                libsql::params!["/moved/elsewhere", source_id],
            )
            .await
            .unwrap();
        global.close();
        let mut moved = HealthPassReport::default();
        retire_completed_consolidation_manifests(&profile, &mut moved).await;
        assert!(moved.warnings.is_empty(), "{:?}", moved.warnings);
        assert!(moved.retired_consolidation_manifests.is_empty());
        assert_eq!(moved.retired_consolidation_registry_projects, 1);
        for project_id in [source_id, target_id] {
            let root = profile.join("projects").join(project_id);
            assert!(!root.join(crate::storage::STORE_MANIFEST_FILENAME).exists());
            assert!(
                root.join(format!(
                    "store_manifest.consolidated-into-{destination_id}.json"
                ))
                .is_file()
            );
        }

        let mut retried = HealthPassReport::default();
        retire_completed_consolidation_manifests(&profile, &mut retried).await;
        assert!(retried.warnings.is_empty(), "{:?}", retried.warnings);
        assert!(retried.retired_consolidation_manifests.is_empty());
        assert_eq!(retried.retired_consolidation_registry_projects, 0);

        let global = crate::global_db::GlobalDb::open_at(&profile.join("global.db"))
            .await
            .unwrap();
        let owners = global.list_code_projects(usize::MAX).await;
        assert_eq!(owners.len(), 2);
        assert!(owners.iter().any(|project| {
            project.project_id == source_id && project.canonical_root == "/moved/elsewhere"
        }));
        assert!(
            owners
                .iter()
                .any(|project| project.project_id == destination_id)
        );
    }
}
