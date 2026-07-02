//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::path::{Component, Path, PathBuf};

use crate::agents::{self, DoctorCounters, HealthcheckContext};
use crate::display::{format_bytes, format_token_count};
use crate::migrate::registry::code_project_root_exists;
use crate::storage::StoreLayout;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

pub mod heal;
mod registry_drift;

/// Runs a comprehensive health check of the tracedecay installation.
pub async fn run_doctor(agent_filter: Option<&str>) {
    debug_assert!(
        !env!("CARGO_PKG_VERSION").is_empty(),
        "CARGO_PKG_VERSION must not be empty"
    );
    let mut dc = DoctorCounters::new();

    eprintln!(
        "\n\x1b[1mtracedecay doctor v{}\x1b[0m\n",
        env!("CARGO_PKG_VERSION")
    );

    check_binary(&mut dc);

    eprintln!("\n\x1b[1mCurrent project\x1b[0m");
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match resolve_current_project_store(&project_path, &TraceDecayOpenOptions::default()).await {
        CurrentProjectStore::Resolved(layout) => {
            dc.pass(&describe_resolved_store(&layout));
            check_database(&mut dc, &project_path).await;
        }
        CurrentProjectStore::LegacyRepoLocal => {
            dc.pass(&format!(
                "Index found: {}/ (legacy repo-local store)",
                crate::config::get_tracedecay_dir(&project_path).display()
            ));
            check_database(&mut dc, &project_path).await;
        }
        CurrentProjectStore::Uninitialized => {
            dc.warn(&format!(
                "No index found for {} — run `tracedecay init`",
                project_path.display()
            ));
        }
    }

    check_global_db(&mut dc);
    check_stale_stores(&mut dc).await;
    check_user_config(&mut dc);
    check_external_tools(&mut dc);

    // Agent-specific health checks
    if let Some(ref home) = agents::home_dir() {
        let hctx = HealthcheckContext {
            home: home.clone(),
            project_path: project_path.clone(),
        };
        let agents_to_check: Vec<Box<dyn agents::AgentIntegration>> = match agent_filter {
            Some(id) => match agents::get_integration(id) {
                Ok(ag) => vec![ag],
                Err(e) => {
                    dc.fail(&format!("{e}"));
                    vec![]
                }
            },
            None => agents::all_integrations(),
        };
        for ag in &agents_to_check {
            ag.healthcheck(&mut dc, &hctx);
        }
    } else {
        dc.fail("Could not determine home directory");
    }

    check_network(&mut dc);
    print_summary(&dc);
}

/// How the doctor "Current project" check sees the working directory's store.
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
) -> CurrentProjectStore {
    if let Some(layout) =
        TraceDecay::initialized_store_layout_with_options(project_path, open_options).await
    {
        return CurrentProjectStore::Resolved(Box::new(layout));
    }
    if crate::config::has_project_database(project_path) {
        return CurrentProjectStore::LegacyRepoLocal;
    }
    CurrentProjectStore::Uninitialized
}

fn describe_resolved_store(layout: &StoreLayout) -> String {
    let mode = match layout.storage_mode {
        crate::storage::StorageMode::ProjectLocal => "repo-local",
        crate::storage::StorageMode::ProfileSharded => "profile-sharded",
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

/// Check database health: report size and run VACUUM to reclaim space.
///
/// The DB path is taken from the opened instance so the size measured is the
/// same file (possibly a branch-specific DB) that VACUUM actually compacts.
async fn check_database(dc: &mut DoctorCounters, project_path: &Path) {
    let ts = match TraceDecay::open(project_path).await {
        Ok(ts) => ts,
        Err(e) => {
            dc.fail(&format!("Could not open database: {e}"));
            return;
        }
    };
    let db_path = ts.db_path();
    let size_before = std::fs::metadata(&db_path).map_or(0, |m| m.len());

    dc.pass(&format!("DB size: {}", format_bytes(size_before)));

    eprintln!("    Compacting database (VACUUM)…");
    match ts.optimize().await {
        Ok(()) => {
            let size_after = std::fs::metadata(&db_path).map_or(size_before, |m| m.len());
            if size_before > size_after {
                let reclaimed = size_before - size_after;
                dc.pass(&format!(
                    "Compacted: {} → {} (reclaimed {})",
                    format_bytes(size_before),
                    format_bytes(size_after),
                    format_bytes(reclaimed),
                ));
            } else {
                dc.pass("Database already compact");
            }
        }
        Err(e) => {
            dc.warn(&format!("VACUUM failed: {e}"));
        }
    }
}

/// Check binary location and version.
fn check_binary(dc: &mut DoctorCounters) {
    eprintln!("\x1b[1mBinary\x1b[0m");
    if let Ok(exe) = std::env::current_exe() {
        dc.pass(&format!("Binary: {}", exe.display()));
    } else {
        dc.fail("Could not determine binary path");
    }
    dc.pass(&format!("Version: {}", env!("CARGO_PKG_VERSION")));
}

/// Check global database exists.
fn check_global_db(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mGlobal database\x1b[0m");
    if let Some(db_path) = crate::global_db::global_db_path() {
        if db_path.exists() {
            dc.pass(&format!("Global DB: {}", db_path.display()));
        } else {
            dc.warn("Global DB not yet created (created on first sync)");
        }
    } else {
        dc.fail("Could not determine home directory for global DB");
    }
}

/// Lists projects registered in the global DB whose resolved data directory
/// is gone, and offers to purge them. Stale rows are harmless but show up in
/// `tracedecay list --all` and inflate the global tokens-saved count.
async fn check_stale_stores(dc: &mut DoctorCounters) {
    use std::io::{IsTerminal, Write};

    let Some(gdb) = crate::global_db::GlobalDb::open().await else {
        return;
    };
    let project_paths = gdb.list_project_paths().await;
    let mut repo_local = 0usize;
    let mut profile_sharded = 0usize;
    let mut reconstructable = Vec::new();
    let mut stale = Vec::new();

    let profile_root = crate::config::user_data_dir();
    for project_path in &project_paths {
        match classify_project_storage_with_registry(
            Path::new(project_path),
            &gdb,
            profile_root.as_deref(),
        )
        .await
        {
            DoctorStorageStatus::RepoLocal => repo_local += 1,
            DoctorStorageStatus::ProfileSharded => profile_sharded += 1,
            DoctorStorageStatus::ManifestReconstructable => {
                reconstructable.push(project_path.clone());
            }
            DoctorStorageStatus::Stale => stale.push(project_path.clone()),
        }
    }

    dc.pass(&format!(
        "Storage registry: {repo_local} repo-local, {profile_sharded} profile-sharded"
    ));
    if !reconstructable.is_empty() {
        dc.warn(&format!(
            "{} manifest-reconstructable project(s) need registry repair",
            reconstructable.len()
        ));
        for p in reconstructable.iter().take(10) {
            dc.info(&format!("  • {p}"));
        }
    }

    check_orphan_store_manifests(dc, &project_paths);
    check_stale_code_projects(dc, &gdb).await;
    if let Some(profile_root) = profile_root.as_deref() {
        let drift = registry_drift::registry_drift_findings(&gdb, profile_root).await;
        if drift.is_empty() {
            dc.pass("No registry/store manifest identity drift");
        } else {
            dc.warn(&format!(
                "{} registry/store manifest identity drift finding(s):",
                drift.len()
            ));
            for finding in drift.iter().take(10) {
                dc.info(&format!(
                    "  • {} {} {}: registry={} manifest={} ({})",
                    finding.project_id,
                    finding.store_id,
                    finding.field,
                    finding.registry_value,
                    finding.manifest_value,
                    finding.manifest_path.display()
                ));
            }
            if drift.len() > 10 {
                dc.info(&format!("  … and {} more", drift.len() - 10));
            }
        }
    }
    if stale.is_empty() {
        dc.pass("No stale projects in global DB");
        return;
    }

    eprintln!(
        "  \x1b[33m!\x1b[0m {} stale project(s) in global DB (registered but the data dir is gone):",
        stale.len()
    );
    let preview = stale.len().min(10);
    for p in &stale[..preview] {
        dc.info(&format!("  • {p}"));
    }
    if stale.len() > preview {
        dc.info(&format!("  … and {} more", stale.len() - preview));
    }

    if !std::io::stdin().is_terminal() {
        dc.warnings += 1;
        dc.info("    Re-run `tracedecay doctor` interactively to purge them.");
        return;
    }

    eprint!(
        "  Purge {} stale row(s) from the global DB? [Y/n] ",
        stale.len()
    );
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        dc.warnings += 1;
        return;
    }
    let answer = answer.trim();
    if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
        dc.warnings += 1;
        dc.info("Skipped — run again later to purge.");
        return;
    }

    let purged = gdb.delete_projects(&stale).await;
    dc.pass(&format!("Purged {purged} stale project(s)"));
}

async fn check_stale_code_projects(dc: &mut DoctorCounters, gdb: &crate::global_db::GlobalDb) {
    use std::io::{IsTerminal, Write};

    let stale: Vec<_> = gdb
        .list_code_projects(usize::MAX)
        .await
        .into_iter()
        .filter(|project| !code_project_root_exists(project))
        .collect();

    if stale.is_empty() {
        dc.pass("No stale code project registry rows");
        return;
    }

    dc.warn(&format!(
        "{} stale code project registry row(s) (registered but project root is gone):",
        stale.len()
    ));
    let preview = stale.len().min(10);
    for project in &stale[..preview] {
        dc.info(&format!(
            "  • {} ({})",
            project.project_id, project.display_root
        ));
    }
    if stale.len() > preview {
        dc.info(&format!("  … and {} more", stale.len() - preview));
    }

    if !std::io::stdin().is_terminal() {
        dc.info("    Re-run `tracedecay doctor` interactively to purge registry rows.");
        return;
    }

    eprint!(
        "  Purge {} stale code project registry row(s)? [Y/n] ",
        stale.len()
    );
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return;
    }
    let answer = answer.trim();
    if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
        dc.info("Skipped code project registry purge.");
        return;
    }

    let project_ids: Vec<String> = stale
        .into_iter()
        .map(|project| project.project_id)
        .collect();
    let purged = gdb.delete_code_projects(&project_ids).await;
    dc.pass(&format!(
        "Purged {purged} stale code project registry row(s)"
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStorageStatus {
    RepoLocal,
    ProfileSharded,
    ManifestReconstructable,
    Stale,
}

fn classify_project_storage(project_root: &Path) -> DoctorStorageStatus {
    let Ok(layout) = crate::storage::resolve_layout_for_current_profile(project_root) else {
        return DoctorStorageStatus::Stale;
    };
    let graph_exists = layout.graph_db_path.exists();
    let manifest_exists = layout
        .manifest_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    match layout.storage_mode {
        crate::storage::StorageMode::ProjectLocal if graph_exists => DoctorStorageStatus::RepoLocal,
        crate::storage::StorageMode::ProfileSharded if graph_exists => {
            DoctorStorageStatus::ProfileSharded
        }
        crate::storage::StorageMode::ProfileSharded if manifest_exists => {
            DoctorStorageStatus::ManifestReconstructable
        }
        _ => DoctorStorageStatus::Stale,
    }
}

async fn classify_project_storage_with_registry(
    project_root: &Path,
    global_db: &crate::global_db::GlobalDb,
    profile_root: Option<&Path>,
) -> DoctorStorageStatus {
    let status = classify_project_storage(project_root);
    if status != DoctorStorageStatus::Stale {
        return status;
    }
    let Some(profile_root) = profile_root else {
        return status;
    };
    let Some(resolution) = global_db.resolve_project_store_by_alias(project_root).await else {
        return status;
    };
    classify_registry_storage(profile_root, &resolution.store).unwrap_or(status)
}

fn classify_registry_storage(
    profile_root: &Path,
    store: &crate::global_db::StoreInstanceRecord,
) -> Option<DoctorStorageStatus> {
    if store.storage_mode != "profile_sharded" {
        return None;
    }
    let artifacts = registry_store_artifacts(profile_root, store);
    if artifacts
        .iter()
        .any(|artifacts| artifacts.graph_db_path.exists())
    {
        Some(DoctorStorageStatus::ProfileSharded)
    } else if artifacts
        .iter()
        .any(|artifacts| artifacts.manifest_path.is_some())
    {
        Some(DoctorStorageStatus::ManifestReconstructable)
    } else if artifacts.is_empty() {
        None
    } else {
        Some(DoctorStorageStatus::Stale)
    }
}

#[derive(Debug, Clone)]
struct RegistryStoreArtifacts {
    graph_db_path: PathBuf,
    manifest_path: Option<PathBuf>,
}

fn registry_store_artifacts(
    profile_root: &Path,
    store: &crate::global_db::StoreInstanceRecord,
) -> Vec<RegistryStoreArtifacts> {
    if store.storage_mode != "profile_sharded" {
        return Vec::new();
    }
    let store_relpath = registry_relpath(&store.store_relpath);
    let manifest_relpath = store
        .manifest_relpath
        .as_ref()
        .map(|relpath| registry_relpath(relpath));
    let mut artifacts = Vec::new();
    for profile_root in registry_profile_roots(profile_root) {
        let Ok(data_root) =
            crate::storage::StoreArtifactPath::resolve(&profile_root, &store_relpath)
        else {
            continue;
        };
        let data_root = data_root.absolute_path();
        artifacts.push(RegistryStoreArtifacts {
            graph_db_path: data_root.join(crate::config::db_filename(&data_root)),
            manifest_path: registry_manifest_path(
                &profile_root,
                &data_root,
                manifest_relpath.as_deref(),
            ),
        });
    }
    artifacts
}

fn registry_manifest_path(
    profile_root: &Path,
    data_root: &Path,
    manifest_relpath: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(relpath) = manifest_relpath {
        return [profile_root, data_root].iter().find_map(|root| {
            crate::storage::StoreArtifactPath::resolve(root, relpath)
                .ok()
                .map(|path| path.absolute_path())
                .filter(|path| path.is_file())
        });
    }
    let path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
    path.is_file().then_some(path)
}

fn registry_relpath(value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return path.to_path_buf();
    }
    value
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect()
}

fn registry_profile_roots(profile_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![profile_root.to_path_buf()];
    if let Ok(canonical) = profile_root.canonicalize() {
        if !roots.iter().any(|root| root == &canonical) {
            roots.push(canonical);
        }
    }
    roots
}

fn check_orphan_store_manifests(dc: &mut DoctorCounters, project_paths: &[String]) {
    let Some(profile_root) = crate::config::user_data_dir() else {
        return;
    };
    let (orphan_count, issues) = orphan_store_manifest_report(&profile_root, project_paths);
    for issue in issues.iter().take(10) {
        dc.warn(&format!("Store manifest issue: {issue}"));
    }
    if orphan_count > 0 {
        dc.warn(&format!(
            "{orphan_count} orphan profile store manifest(s) can reconstruct registry rows"
        ));
        dc.info("    Run `tracedecay migrate reconstruct --profile-root <profile> --apply` after review.");
    }
}

/// Counts profile store manifests with no matching registry row, plus any
/// manifest scan issues. Shared between `doctor` and the post-update health
/// pass.
pub(crate) fn orphan_store_manifest_report(
    profile_root: &Path,
    project_paths: &[String],
) -> (usize, Vec<String>) {
    let registered: std::collections::HashSet<String> = project_paths
        .iter()
        .map(|path| crate::global_db::GlobalDb::canonical_project_key(std::path::Path::new(path)))
        .collect();
    let report = crate::migrate::registry::scan_profile_store_manifests(
        profile_root,
        crate::tracedecay::current_timestamp(),
    );
    let orphan_count = report
        .plans
        .iter()
        .filter(|plan| {
            let key = crate::global_db::GlobalDb::canonical_project_key(&plan.project.project_root);
            !registered.contains(&key)
        })
        .count();
    (orphan_count, report.issues)
}

/// Check user config file.
fn check_user_config(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mUser config\x1b[0m");
    if let Some(config_path) = crate::user_config::config_path() {
        if config_path.exists() {
            let config = crate::user_config::UserConfig::load();
            dc.pass(&format!("Config: {}", config_path.display()));
            if config.upload_enabled {
                dc.pass("Upload enabled");
            } else {
                dc.info("Upload disabled (opt-out)");
            }
            if config.pending_upload > 0 {
                dc.info(&format!("Pending upload: {} tokens", config.pending_upload));
            }
        } else {
            dc.warn("Config not yet created (created on first sync)");
        }
    } else {
        dc.fail("Could not determine home directory for config");
    }
}

/// Check optional external tools that gate optional MCP capabilities.
fn check_external_tools(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mExternal tools\x1b[0m");
    let diagnostics = crate::mcp::tools::ast_grep_diagnostics_json();
    let installed = json_bool(&diagnostics, "installed");
    let rewrite_available = json_bool(&diagnostics, "rewrite_available");
    let outline_available = json_bool(&diagnostics, "outline_available");
    let version = diagnostics
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let message = diagnostics
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ast-grep status unavailable");

    if outline_available {
        dc.pass(&format!(
            "ast-grep {version}: rewrite and outline support available"
        ));
        return;
    }

    if rewrite_available {
        dc.warn(&format!(
            "ast-grep {version}: rewrite support available, but outline support is missing"
        ));
    } else if installed {
        dc.warn(&format!(
            "ast-grep {version}: optional ast-grep-backed tools are unavailable"
        ));
    } else {
        dc.warn("ast-grep not found on PATH; optional ast-grep-backed tools are hidden");
    }
    dc.info(message);
    dc.info("Install or update ast-grep to >= 0.44, then rerun `tracedecay install` or `tracedecay update-plugin` if your agent integration caches tool metadata.");
}

fn json_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Check network connectivity.
fn check_network(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mNetwork\x1b[0m");
    if let Some(total) = crate::cloud::fetch_worldwide_total() {
        dc.pass(&format!(
            "Worldwide counter reachable (total: {})",
            format_token_count(total)
        ));
    } else {
        dc.warn("Worldwide counter unreachable (offline or timeout)");
    }
    if crate::cloud::fetch_latest_version().is_some() {
        dc.pass("GitHub releases API reachable");
    } else {
        dc.warn("GitHub releases API unreachable (offline or timeout)");
    }
}

/// Print final summary.
fn print_summary(dc: &DoctorCounters) {
    eprintln!();
    if dc.issues == 0 && dc.warnings == 0 {
        eprintln!("\x1b[32mAll checks passed.\x1b[0m");
    } else if dc.issues == 0 {
        eprintln!("\x1b[33m{} warning(s), no issues.\x1b[0m", dc.warnings);
    } else {
        eprintln!(
            "\x1b[31m{} issue(s), {} warning(s).\x1b[0m",
            dc.issues, dc.warnings
        );
        eprintln!("Run \x1b[1mtracedecay install\x1b[0m to fix most issues.");
    }
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_db::StoreInstanceUpsert;
    use crate::storage::{
        StorageMode, StoreKind, StoreManifest, STORE_MANIFEST_FILENAME,
        STORE_MANIFEST_SCHEMA_VERSION,
    };
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

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

    #[tokio::test]
    async fn current_project_store_resolves_profile_shard_via_registry_alias(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let profile_root = dir.path().join("profile");
        let project_root = dir.path().join("repo");
        std::fs::create_dir_all(&project_root)?;
        let project_root = canonical_temp_path(&project_root);
        let shard_root =
            crate::storage::profile_sharded_data_root(&profile_root, "proj_doctor_current");
        std::fs::create_dir_all(&shard_root)?;
        std::fs::write(
            shard_root.join(crate::config::db_filename(&shard_root)),
            b"graph",
        )?;

        let global_db_path = dir.path().join("global.db");
        let db = crate::global_db::GlobalDb::open_at(&global_db_path)
            .await
            .ok_or_else(|| std::io::Error::other("could not open global db"))?;
        db.upsert_code_project(
            "proj_doctor_current",
            &project_root,
            None,
            None,
            Some("main"),
        )
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
        db.upsert_store_instance(StoreInstanceUpsert {
            store_id: "store:proj_doctor_current:profile_sharded".to_string(),
            project_id: "proj_doctor_current".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: Path::new("projects")
                .join("proj_doctor_current")
                .to_string_lossy()
                .to_string(),
            manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
            last_verified_at: Some(1_800_000_000),
            last_write_at: Some(1_800_000_000),
        })
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

        let open_options = TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(global_db_path),
        };

        // No repo-local `.tracedecay/` index exists, yet the project must not
        // be reported as uninitialized: resolution finds the profile shard.
        assert!(!crate::config::has_project_database(&project_root));
        match resolve_current_project_store(&project_root, &open_options).await {
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
            resolve_current_project_store(&unregistered, &open_options).await,
            CurrentProjectStore::Uninitialized
        ));
        Ok(())
    }

    #[tokio::test]
    async fn registry_backed_profile_shard_is_not_stale_without_marker(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let profile_root = dir.path().join("profile");
        let project_root = dir.path().join("repo");
        let shard_relpath = Path::new("projects").join("proj_doctor");
        let shard_root = profile_root.join(&shard_relpath);
        std::fs::create_dir_all(&project_root)?;
        std::fs::create_dir_all(&shard_root)?;
        let project_root = canonical_temp_path(&project_root);
        std::fs::write(shard_root.join("tracedecay.db"), b"graph")?;
        let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("could not open global db"))?;
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
            classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
            DoctorStorageStatus::ProfileSharded
        );
        #[cfg(unix)]
        {
            let symlinked_profile_root = dir.path().join("profile-link");
            symlink(&profile_root, &symlinked_profile_root)?;
            assert_eq!(
                classify_project_storage_with_registry(
                    &project_root,
                    &db,
                    Some(&symlinked_profile_root)
                )
                .await,
                DoctorStorageStatus::ProfileSharded
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn registry_backed_profile_shard_manifest_relpath_uses_profile_root(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let profile_root = canonical_temp_path(&dir.path().join("profile"));
        let project_root = canonical_temp_path(&dir.path().join("repo"));
        let shard_relpath = Path::new("projects").join("proj_doctor_manifest");
        let manifest_relpath = shard_relpath.join(crate::storage::STORE_MANIFEST_FILENAME);
        let shard_root = profile_root.join(&shard_relpath);
        std::fs::create_dir_all(&project_root)?;
        std::fs::create_dir_all(&shard_root)?;
        std::fs::write(profile_root.join(&manifest_relpath), b"manifest")?;
        let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("could not open global db"))?;
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
            classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
            DoctorStorageStatus::ManifestReconstructable
        );
        Ok(())
    }

    #[tokio::test]
    async fn registry_backed_profile_shard_rejects_unsafe_store_relpath(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let profile_root = dir.path().join("profile");
        let project_root = dir.path().join("repo");
        let outside_root = dir.path().join("outside");
        std::fs::create_dir_all(&project_root)?;
        std::fs::create_dir_all(&outside_root)?;
        let project_root = canonical_temp_path(&project_root);
        std::fs::write(outside_root.join("tracedecay.db"), b"graph")?;
        let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("could not open global db"))?;
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
            classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
            DoctorStorageStatus::Stale
        );
        Ok(())
    }

    #[tokio::test]
    async fn registry_drift_findings_report_manifest_identity_mismatches(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
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

        let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("could not open global db"))?;
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

        let findings = registry_drift::registry_drift_findings(&db, &profile_root).await;
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
}
