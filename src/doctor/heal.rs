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
//!
//! Those auto-applied remedies are safe precisely because quarantine renames
//! instead of deleting and the GC removes only temp-rooted registry metadata
//! whose every known root has vanished — no user data is ever destroyed.
//!
//! Everything else (orphan store manifests, stale rows outside the temp
//! directory, registry/manifest identity drift) is only reported.

use std::path::{Path, PathBuf};

use crate::global_db::{CodeProjectRecord, GlobalDb};
use crate::migrate::registry::{code_project_root_exists, stale_code_projects, StaleRootScope};
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
    pub quarantined_branch_meta: Vec<BranchMetaQuarantine>,
    /// `None` when the global DB could not be opened, so the GC never ran.
    pub purged_temp_registry_rows: Option<usize>,
    pub remaining_findings: Vec<String>,
    pub warnings: Vec<String>,
}

/// Runs the full post-update health pass and prints a doctor-style summary.
///
/// Never fails: every error is collected as a warning so a broken store can
/// never abort `tracedecay update`.
pub async fn run_post_update_health_pass() -> HealthPassReport {
    eprintln!("\n\x1b[1mPost-update health pass\x1b[0m (skip with --no-heal)");

    let Some(profile_root) = crate::config::user_data_dir() else {
        let report = HealthPassReport {
            warnings: vec!["could not determine the profile data directory".to_string()],
            ..HealthPassReport::default()
        };
        render_warnings(&report.warnings);
        return report;
    };

    let report = compute_health_pass_report(&profile_root).await;
    render_health_pass_report(&report);
    report
}

/// Applies the safe remedies and gathers everything the pass has to say into
/// a [`HealthPassReport`], without printing anything.
async fn compute_health_pass_report(profile_root: &Path) -> HealthPassReport {
    let mut report = HealthPassReport::default();

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

    let (findings, warnings) =
        collect_remaining_findings(&global_db, profile_root, &projects, &purged_ids).await;
    report.remaining_findings = findings;
    report.warnings.extend(warnings);
    report
}

/// Prints the doctor-style summary for a computed report.
fn render_health_pass_report(report: &HealthPassReport) {
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
) -> (Vec<String>, Vec<String>) {
    let mut findings = Vec::new();
    let project_paths = global_db.list_project_paths().await;
    let (orphan_count, warnings) =
        super::orphan_store_manifest_report(profile_root, &project_paths);
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

    let drift = super::registry_drift::registry_drift_findings(global_db, profile_root).await;
    if !drift.is_empty() {
        findings.push(format!(
            "{} registry/store manifest identity drift finding(s)",
            drift.len()
        ));
    }
    (findings, warnings)
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
}
