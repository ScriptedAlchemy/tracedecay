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
use crate::global_db::{StaleRootScope, code_project_root_exists, stale_project_contexts};
mod report;

use report::{render_health_pass_report, render_missing_profile_report, render_warnings};

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

    let (findings, warnings) =
        collect_remaining_findings(&projects, &purged_ids, remaining_registry_drift_count).await;
    report.remaining_findings = findings;
    report
        .warnings
        .extend(warnings.into_iter().map(HealthPassWarning::new));
    report
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
    projects: &[CodeProjectRecord],
    purged_ids: &[String],
    remaining_registry_drift_count: usize,
) -> (Vec<String>, Vec<String>) {
    let mut findings = Vec::new();
    let warnings = Vec::new();

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
