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
//! - corrupt `branch-meta.json` paths (non-regular files or anything
//!   [`crate::branch_meta::parse`] rejects) are quarantined — renamed to
//!   `branch-meta.json.corrupt-<timestamp>`, never deleted — preserving the
//!   evidence while restoring the silent single-DB fallback,
//! - registry rows whose project root no longer exists AND lives under the
//!   system temp directory are purged (the automated equivalent of the
//!   daemon's `registry_gc` admin action), and only when BOTH the canonical
//!   and display roots are gone.
//!
//! Those auto-applied remedies are safe precisely because quarantine renames
//! instead of deleting and the GC removes only temp-rooted registry metadata
//! whose every known root has vanished — no user data is ever destroyed.
//!
//! Everything else (orphan store manifests, stale rows outside the temp
//! directory, registry/manifest identity drift) is only reported.
//!
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::global_db::{CodeProjectRecord, RegisteredGlobalDb};
use crate::migrate::registry::{StaleRootScope, code_project_root_exists, stale_project_contexts};
use crate::storage::{BRANCH_META_FILENAME, BRANCH_META_QUARANTINE_PREFIX};

mod report;

use report::{render_health_pass_report, render_missing_profile_report, render_warnings};

/// A corrupt `branch-meta.json` that was renamed out of the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMetaQuarantine {
    pub original: PathBuf,
    pub quarantined: PathBuf,
}

/// One warning surfaced by the post-update health pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthPassWarning {
    pub message: String,
}

impl HealthPassWarning {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HealthPassWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

/// Outcome of one post-update health pass.
#[derive(Debug, Default)]
pub struct HealthPassReport {
    pub quarantined_branch_meta: Vec<BranchMetaQuarantine>,
    /// `None` when the global DB could not be opened, so the GC never ran.
    pub purged_temp_registry_rows: Option<usize>,
    /// Stale store manifests reconciled to the registry canonical path.
    pub reconciled_store_roots: Vec<super::registry_drift::ReconciledStoreRoot>,
    pub remaining_findings: Vec<String>,
    pub warnings: Vec<HealthPassWarning>,
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
            warnings: vec![HealthPassWarning::new(warning)],
            ..HealthPassReport::default()
        };
        render_warnings(&report.warnings);
        return report;
    }
    let _database_scope = match crate::db::enter_maintenance_database_scope(
        lifecycle_lease,
        &profile_root,
        "post-update health pass",
    ) {
        Ok(scope) => scope,
        Err(error) => {
            let report = HealthPassReport {
                warnings: vec![HealthPassWarning::new(format!(
                    "could not enter maintenance database scope for the post-update health pass: {error}"
                ))],
                ..HealthPassReport::default()
            };
            render_warnings(&report.warnings);
            return report;
        }
    };
    let profile_identity = match crate::daemon::profile_identity::load_or_create(&profile_root) {
        Ok(identity) => identity,
        Err(error) => {
            let report = HealthPassReport {
                warnings: vec![HealthPassWarning::new(format!(
                    "could not load the profile identity for the post-update health pass: {error}"
                ))],
                ..HealthPassReport::default()
            };
            render_warnings(&report.warnings);
            return report;
        }
    };
    // Post-update needs admission-critical schema and a durable repair
    // checkpoint, not inline historical convergence over multi-gigabyte
    // session stores. Use the daemon admission path; the restarted daemon
    // resumes the checkpointed maintenance after service restoration.
    crate::daemon::mark_process_long_lived_for_session_maintenance();
    let runtime_registry =
        match crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            profile_identity,
        )
        .await
        {
            Ok(registry) => registry,
            Err(error) => {
                let report = HealthPassReport {
                    warnings: vec![HealthPassWarning::new(format!(
                        "could not mount the profile runtime for the post-update health pass: {error}"
                    ))],
                    ..HealthPassReport::default()
                };
                render_warnings(&report.warnings);
                return report;
            }
        };
    run_post_update_health_pass_for_profile(&profile_root, &runtime_registry).await
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

async fn run_post_update_health_pass_for_profile(
    profile_root: &Path,
    runtime_registry: &crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
) -> HealthPassReport {
    eprintln!("\n\x1b[1mPost-update health pass\x1b[0m (skip with --no-heal)");
    let report = compute_health_pass_report(profile_root, runtime_registry).await;
    render_health_pass_report(&report);
    report
}

/// Applies the safe remedies and gathers everything the pass has to say into
/// a [`HealthPassReport`], without printing anything.
async fn compute_health_pass_report(
    profile_root: &Path,
    runtime_registry: &crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
) -> HealthPassReport {
    let mut report = HealthPassReport::default();

    let (quarantined, warnings) = quarantine_corrupt_branch_meta(profile_root);
    report.quarantined_branch_meta = quarantined;
    report
        .warnings
        .extend(warnings.into_iter().map(HealthPassWarning::new));

    let global_db = match runtime_registry.profile_database().await {
        Ok(global_db) => global_db,
        Err(error) => {
            report.warnings.push(HealthPassWarning::new(format!(
                "could not mount the global DB for the health pass: {error}"
            )));
            return report;
        }
    };

    // One registry snapshot for the whole pass: the GC and the remaining
    // findings below both work from this list.
    let projects = match global_db.list_code_projects(usize::MAX).await {
        Ok(projects) => projects,
        Err(error) => {
            report.warnings.push(HealthPassWarning::new(format!(
                "could not read the global project registry: {error}"
            )));
            return report;
        }
    };
    let purged_ids = match gc_stale_temp_registry_rows(&global_db, &projects).await {
        Ok((purged, purged_ids)) => {
            report.purged_temp_registry_rows = Some(purged);
            purged_ids
        }
        Err(error) => {
            report.warnings.push(HealthPassWarning::new(format!(
                "could not purge stale temp registry rows: {error}"
            )));
            Vec::new()
        }
    };

    let registry_drift =
        super::registry_drift::registry_drift_findings(&global_db, profile_root).await;
    let (reconciled, reconcile_warnings) =
        super::registry_drift::reconcile_drifted_store_roots_from_findings(&registry_drift);
    let remaining_registry_drift_count =
        count_remaining_registry_drift(&registry_drift, &reconciled);
    report.reconciled_store_roots = reconciled;
    report
        .warnings
        .extend(reconcile_warnings.into_iter().map(HealthPassWarning::new));

    let (findings, warnings) = collect_remaining_findings(
        &global_db,
        profile_root,
        &projects,
        &purged_ids,
        remaining_registry_drift_count,
    )
    .await;
    report.remaining_findings = findings;
    report
        .warnings
        .extend(warnings.into_iter().map(HealthPassWarning::new));
    report
}

/// Renames every `branch-meta.json` under `<profile_root>/projects/*` that is
/// not a regular file or that [`crate::branch_meta::parse`] rejects. This is
/// the runtime's own definition of corrupt, covering invalid JSON, schema
/// mismatches, and non-regular paths. Quarantine preserves the original path
/// as evidence while restoring the single-DB fallback.
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
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path().join(BRANCH_META_FILENAME))
        .collect();
    meta_paths.sort();

    let now = crate::tracedecay::current_timestamp();
    for path in meta_paths {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!("could not inspect '{}': {error}", path.display()));
                continue;
            }
        };
        let corrupt = if metadata.file_type().is_file() {
            match std::fs::read_to_string(&path) {
                Ok(content) => crate::branch_meta::parse(&content).is_err(),
                Err(error) => {
                    warnings.push(format!("could not read '{}': {error}", path.display()));
                    continue;
                }
            }
        } else {
            true
        };
        if !corrupt {
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
    global_db: &Arc<RegisteredGlobalDb>,
    projects: &[CodeProjectRecord],
) -> crate::errors::Result<(usize, Vec<String>)> {
    // Resolve aliases and store instances before retiring anything. Deleting a
    // `code_projects` row cascades its aliases and store instances away, so a
    // roots-only check would silently retire a project another checkout — or a
    // registered store — is still using, and the daemon's orphan sweep would
    // then collect the now-unregistered store.
    let contexts = global_db
        .project_registry_contexts_for_projects(projects)
        .await?;
    let stale_ids: Vec<String> = stale_project_contexts(
        &contexts,
        &temp_dir_prefixes(),
        StaleRootScope::AllRootsMissing,
    )
    .into_iter()
    .map(|context| context.project.project_id.clone())
    .collect();
    if stale_ids.is_empty() {
        return Ok((0, stale_ids));
    }
    let transaction = global_db.begin_write_transaction().await?;
    let mut purged = 0_usize;
    for project_id in &stale_ids {
        let deleted = transaction
            .execute(
                "DELETE FROM code_projects WHERE project_id=?1",
                crate::db::engine::params![project_id],
            )
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "purge stale temporary project registry row".to_string(),
                message: error.to_string(),
            })?;
        purged = purged.saturating_add(usize::try_from(deleted).unwrap_or(usize::MAX));
    }
    transaction
        .commit()
        .await
        .map_err(|error| crate::errors::TraceDecayError::Database {
            operation: "commit stale temporary project registry purge".to_string(),
            message: error.to_string(),
        })?;
    Ok((purged, stale_ids))
}

/// The system temp directory in both its literal and canonicalized spellings,
/// so registry rows recorded through a symlinked temp path still match.
fn temp_dir_prefixes() -> Vec<PathBuf> {
    let temp_dir = std::env::temp_dir();
    let mut prefixes = vec![temp_dir.clone()];
    if let Ok(canonical) = temp_dir.canonicalize()
        && !prefixes.contains(&canonical)
    {
        prefixes.push(canonical);
    }
    prefixes
}

/// Summarizes the doctor findings that are NOT safe to auto-apply so the user
/// sees them at the end of `tracedecay update` output. `projects` is the
/// pre-purge registry snapshot; rows in `purged_ids` are skipped.
///
/// Returns the findings and any warnings.
async fn collect_remaining_findings(
    global_db: &Arc<RegisteredGlobalDb>,
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

    struct ClearedUserDataDir {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl ClearedUserDataDir {
        fn new() -> Self {
            let lock = crate::config::lock_user_data_dir_test_env();
            let previous = std::env::var_os(crate::config::USER_DATA_DIR_ENV);
            unsafe { std::env::remove_var(crate::config::USER_DATA_DIR_ENV) };
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for ClearedUserDataDir {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe { std::env::set_var(crate::config::USER_DATA_DIR_ENV, previous) };
            } else {
                unsafe { std::env::remove_var(crate::config::USER_DATA_DIR_ENV) };
            }
        }
    }

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
        let syntax_corrupt =
            write_branch_meta(&projects_root, "proj_syntax_corrupt", "{not valid json");
        let semantic_corrupt = write_branch_meta(
            &projects_root,
            "proj_semantic_corrupt",
            r#"{"default_branch":"main","branches":{}}"#,
        );
        let valid_content =
            serde_json::to_string(&crate::branch_meta::BranchMeta::new("main")).unwrap();
        let valid = write_branch_meta(&projects_root, "proj_valid", &valid_content);

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert_eq!(quarantines.len(), 2);
        assert!(warnings.is_empty());
        for (original, content) in [
            (&syntax_corrupt, "{not valid json"),
            (
                &semantic_corrupt,
                r#"{"default_branch":"main","branches":{}}"#,
            ),
        ] {
            let quarantine = quarantines
                .iter()
                .find(|quarantine| &quarantine.original == original)
                .unwrap();
            assert!(!original.exists(), "corrupt file should be renamed away");
            assert_eq!(
                std::fs::read_to_string(&quarantine.quarantined).unwrap(),
                content,
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
        }
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
    fn quarantine_ignores_non_directory_project_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let projects_root = dir.path().join("projects");
        std::fs::create_dir_all(&projects_root).unwrap();
        let shared_store = projects_root.join("sessions.db");
        std::fs::write(&shared_store, b"not a project shard").unwrap();

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert!(quarantines.is_empty());
        assert!(warnings.is_empty());
        assert_eq!(
            std::fs::read(&shared_store).unwrap(),
            b"not a project shard"
        );
    }

    #[test]
    fn quarantine_renames_non_regular_branch_meta() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("projects")
            .join("proj_directory")
            .join(BRANCH_META_FILENAME);
        std::fs::create_dir_all(&path).unwrap();

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert!(warnings.is_empty());
        assert_eq!(quarantines.len(), 1);
        assert_eq!(quarantines[0].original, path);
        assert!(!path.exists());
        assert!(quarantines[0].quarantined.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_renames_symlinked_valid_branch_meta() {
        let dir = tempfile::TempDir::new().unwrap();
        let outside = dir.path().join("outside.json");
        std::fs::write(
            &outside,
            serde_json::to_vec_pretty(&crate::branch_meta::BranchMeta::new("main")).unwrap(),
        )
        .unwrap();
        let shard = dir.path().join("projects").join("proj_symlink");
        std::fs::create_dir_all(&shard).unwrap();
        let path = shard.join(BRANCH_META_FILENAME);
        std::os::unix::fs::symlink(&outside, &path).unwrap();

        let (quarantines, warnings) = quarantine_corrupt_branch_meta(dir.path());

        assert!(warnings.is_empty());
        assert_eq!(quarantines.len(), 1);
        assert_eq!(quarantines[0].original, path);
        assert!(!path.exists());
        assert!(outside.exists());
        assert!(
            std::fs::symlink_metadata(&quarantines[0].quarantined)
                .unwrap()
                .file_type()
                .is_symlink()
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

    #[test]
    fn maintenance_scope_authorizes_health_pass_global_db_open() {
        let _data_dir = ClearedUserDataDir::new();
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let dir = tempfile::Builder::new()
            .prefix("doctor-heal-scope-")
            .tempdir_in(base)
            .unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        unsafe { std::env::set_var(crate::config::USER_DATA_DIR_ENV, &profile) };
        let profile = crate::config::user_data_dir().unwrap();
        let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
            &profile,
            "maintenance scope healer test",
        )
        .unwrap();
        let _database_scope = crate::db::enter_maintenance_database_scope(
            &lifecycle,
            &profile,
            "post-update health pass",
        )
        .unwrap();
        let db_path = profile.join("global.db");
        let authority = crate::db::DatabaseAuthority::for_runtime(
            &db_path,
            "open global database for health pass",
        )
        .unwrap();

        assert_eq!(
            authority.role(),
            crate::db::DatabaseAuthorityRole::Maintenance
        );
    }
}
