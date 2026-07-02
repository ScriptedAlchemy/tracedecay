//! Post-update health pass: safe, automatic repairs plus a concise summary
//! of the doctor findings that still need a human decision.
//!
//! Runs at the end of `tracedecay update` / `tracedecay post-update` (skip
//! with `--no-heal`). Every step is failure-tolerant: a failing check prints
//! a warning but never fails the update itself. Only remedies that are safe
//! to automate are applied:
//!
//! - corrupt `branch-meta.json` files are quarantined (renamed to
//!   `branch-meta.json.corrupt-<timestamp>`), which preserves evidence while
//!   restoring the silent single-DB fallback,
//! - registry rows whose project root no longer exists AND lives under the
//!   system temp directory are purged (the automated equivalent of
//!   `tracedecay migrate registry-gc --prefix <tmp> --apply`).
//!
//! Everything else (orphan store manifests, stale rows outside the temp
//! directory, registry/manifest identity drift) is only reported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::migrate::registry::stale_code_projects;
use crate::storage::BRANCH_META_FILENAME;

/// A corrupt `branch-meta.json` that was renamed out of the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMetaQuarantine {
    pub original: PathBuf,
    pub quarantined: PathBuf,
}

/// Outcome of one post-update health pass.
#[derive(Debug, Default)]
pub struct HealthPassReport {
    pub quarantined_branch_meta: Vec<BranchMetaQuarantine>,
    pub purged_temp_registry_rows: usize,
    pub remaining_findings: Vec<String>,
    pub warnings: Vec<String>,
}

/// Runs the full post-update health pass and prints a doctor-style summary.
///
/// Never fails: every error is collected as a warning so a broken store can
/// never abort `tracedecay update`.
pub async fn run_post_update_health_pass() -> HealthPassReport {
    let mut report = HealthPassReport::default();
    eprintln!("\n\x1b[1mPost-update health pass\x1b[0m (skip with --no-heal)");

    let Some(profile_root) = crate::config::user_data_dir() else {
        report
            .warnings
            .push("could not determine the profile data directory".to_string());
        print_warnings(&report.warnings);
        return report;
    };

    quarantine_corrupt_branch_meta(&profile_root, &mut report);
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

    // Opening the global DB applies its idempotent schema migrations — the
    // same lazy upgrade every normal open path performs.
    match GlobalDb::open().await {
        Some(global_db) => {
            gc_stale_temp_registry_rows(&global_db, &mut report).await;
            if report.purged_temp_registry_rows > 0 {
                eprintln!(
                    "  \x1b[32m✔\x1b[0m Purged {} stale temp-root registry row(s)",
                    report.purged_temp_registry_rows
                );
            } else {
                eprintln!("  \x1b[32m✔\x1b[0m No stale temp-root registry rows");
            }
            collect_remaining_findings(&global_db, &profile_root, &mut report).await;
        }
        None => {
            report
                .warnings
                .push("could not open the global DB for the health pass".to_string());
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
    print_warnings(&report.warnings);
    report
}

fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("  \x1b[33mwarning:\x1b[0m health pass: {warning}");
    }
}

/// Renames every `branch-meta.json` under `<profile_root>/projects/*` that is
/// not valid JSON to `branch-meta.json.corrupt-<timestamp>`, preserving the
/// corrupt content as evidence while restoring the single-DB fallback.
fn quarantine_corrupt_branch_meta(profile_root: &Path, report: &mut HealthPassReport) {
    let projects_root = profile_root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return;
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
                report
                    .warnings
                    .push(format!("could not read '{}': {err}", path.display()));
                continue;
            }
        };
        if serde_json::from_str::<serde_json::Value>(&content).is_ok() {
            continue;
        }
        let quarantined = path.with_file_name(format!("{BRANCH_META_FILENAME}.corrupt-{now}"));
        match std::fs::rename(&path, &quarantined) {
            Ok(()) => report.quarantined_branch_meta.push(BranchMetaQuarantine {
                original: path,
                quarantined,
            }),
            Err(err) => report.warnings.push(format!(
                "could not quarantine corrupt '{}': {err}",
                path.display()
            )),
        }
    }
}

/// Purges registry rows whose canonical project root is gone AND lives under
/// the system temp directory. This is the only registry GC scope that is safe
/// to run without review.
async fn gc_stale_temp_registry_rows(global_db: &GlobalDb, report: &mut HealthPassReport) {
    let projects = global_db.list_code_projects(usize::MAX).await;
    let mut seen: HashSet<String> = HashSet::new();
    let mut stale_ids: Vec<String> = Vec::new();
    for prefix in temp_dir_prefixes() {
        for project in stale_code_projects(projects.clone(), Some(&prefix)) {
            // Extra safety over the manual `migrate registry-gc`: also
            // require the display root to be gone before auto-deleting.
            if super::code_project_root_exists(&project) {
                continue;
            }
            if seen.insert(project.project_id.clone()) {
                stale_ids.push(project.project_id);
            }
        }
    }
    if stale_ids.is_empty() {
        return;
    }
    report.purged_temp_registry_rows = global_db.delete_code_projects(&stale_ids).await;
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
/// sees them at the end of `tracedecay update` output.
async fn collect_remaining_findings(
    global_db: &GlobalDb,
    profile_root: &Path,
    report: &mut HealthPassReport,
) {
    let project_paths = global_db.list_project_paths().await;
    let (orphan_count, issues) = super::orphan_store_manifest_report(profile_root, &project_paths);
    report.warnings.extend(issues);
    if orphan_count > 0 {
        report.remaining_findings.push(format!(
            "{orphan_count} orphan profile store manifest(s) can reconstruct registry rows"
        ));
    }

    let stale_rows = global_db
        .list_code_projects(usize::MAX)
        .await
        .into_iter()
        .filter(|project| !super::code_project_root_exists(project))
        .count();
    if stale_rows > 0 {
        report.remaining_findings.push(format!(
            "{stale_rows} stale code project registry row(s) outside the temp directory"
        ));
    }

    let drift = super::registry_drift::registry_drift_findings(global_db, profile_root).await;
    if !drift.is_empty() {
        report.remaining_findings.push(format!(
            "{} registry/store manifest identity drift finding(s)",
            drift.len()
        ));
    }
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

        let mut report = HealthPassReport::default();
        quarantine_corrupt_branch_meta(dir.path(), &mut report);

        assert_eq!(report.quarantined_branch_meta.len(), 1);
        assert!(report.warnings.is_empty());
        let quarantine = &report.quarantined_branch_meta[0];
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
                .starts_with("branch-meta.json.corrupt-"),
            "quarantine name should be branch-meta.json.corrupt-<timestamp>: {quarantine:?}"
        );
        assert!(valid.exists(), "valid branch-meta must be left untouched");
    }

    #[test]
    fn quarantine_is_a_no_op_without_a_projects_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut report = HealthPassReport::default();
        quarantine_corrupt_branch_meta(dir.path(), &mut report);
        assert!(report.quarantined_branch_meta.is_empty());
        assert!(report.warnings.is_empty());
    }
}
