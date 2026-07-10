//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::path::{Component, Path, PathBuf};

use crate::agents::{self, DoctorCounters, HealthcheckContext};
use crate::db::Database;
use crate::display::{format_bytes, format_token_count};
use crate::migrate::registry::code_project_root_exists;
use crate::storage::StoreLayout;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

pub mod heal;
pub(crate) mod registry_drift;

/// Runs a comprehensive health check of the tracedecay installation.
pub async fn run_doctor(agent_filter: Option<&str>) {
    let _lifecycle_lease = match crate::lifecycle_lease::acquire_shared_or_inherited("doctor") {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("tracedecay doctor could not start: {error}");
            return;
        }
    };
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
    let open_options = TraceDecayOpenOptions::default();
    match resolve_current_project_store(&project_path, &open_options).await {
        Ok(CurrentProjectStore::Resolved(layout)) => {
            dc.pass(&describe_resolved_store(&layout));
            check_database(&mut dc, &project_path, open_options.clone()).await;
        }
        Ok(CurrentProjectStore::LegacyRepoLocal) => {
            dc.pass(&format!(
                "Index found: {}/ (legacy repo-local store)",
                crate::config::get_tracedecay_dir(&project_path).display()
            ));
            check_database(&mut dc, &project_path, open_options).await;
        }
        Ok(CurrentProjectStore::Uninitialized) => {
            dc.warn(&format!(
                "No index found for {} — run `tracedecay init`",
                project_path.display()
            ));
        }
        Err(error) => dc.fail(&format!("Project storage resolution failed: {error}")),
    }

    check_global_db(&mut dc);
    check_stale_stores(&mut dc).await;
    check_watcher(&mut dc);
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
        let materialization_root =
            crate::automation::skill_materialization::resolve_project_root(&project_path);
        check_managed_skill_materialization(&mut dc, home, &materialization_root);
    } else {
        dc.fail("Could not determine home directory");
    }

    check_network(&mut dc);
    print_summary(&dc);
}

/// Reports drift between the active managed-skill set and the host-loadable
/// `SKILL.md` files `TraceDecay` automation materializes into detected
/// `.claude`/`.codex` skills directories: missing (active but not on disk),
/// forked (user-edited a managed file — the reconciler will not clobber it),
/// conflict (a foreign file blocks the slot), or orphan (a managed file for a
/// no-longer-active skill). A clean scope passes silently-ish with an info line.
fn check_managed_skill_materialization(dc: &mut DoctorCounters, home: &Path, project_root: &Path) {
    use crate::automation::skill_materialization::doctor_detected_scopes;

    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return;
    };
    let scopes = match doctor_detected_scopes(&profile_root, home, project_root) {
        Ok(scopes) => scopes,
        Err(err) => {
            dc.warn(&format!(
                "Managed skill materialization check failed: {err}"
            ));
            return;
        }
    };
    if scopes.is_empty() {
        return;
    }
    eprintln!("\n\x1b[1mManaged skill materialization\x1b[0m");
    for (scope, drift) in scopes {
        if drift.is_empty() {
            dc.pass(&format!(
                "{}: materialized skills in sync",
                scope.describe()
            ));
            continue;
        }
        let scope_desc = scope.describe();
        for finding in drift {
            match skill_drift_report(&scope_desc, &finding) {
                (DriftLevel::Warn, msg) => dc.warn(&msg),
                (DriftLevel::Info, msg) => dc.info(&msg),
            }
        }
    }
}

/// Severity of a doctor materialization-drift line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftLevel {
    Warn,
    Info,
}

/// Pure classifier: maps a materialization drift finding to its doctor severity
/// and rendered line. Split out from emission so it can be unit-tested — in
/// particular that `ForeignOrphan` renders as `Info` and never prescribes
/// `tracedecay update`, a remediation `update` refuses to perform on a foreign
/// package.
fn skill_drift_report(
    scope_desc: &str,
    finding: &crate::automation::skill_materialization::SkillDrift,
) -> (DriftLevel, String) {
    use crate::automation::skill_materialization::SkillDrift;
    let path = finding.path().display();
    let skill_id = finding.skill_id();
    match finding {
        SkillDrift::Missing { .. } => (
            DriftLevel::Warn,
            format!(
                "{scope_desc}: '{skill_id}' active but not materialized ({path}); run `tracedecay update`"
            ),
        ),
        SkillDrift::Forked { .. } => (
            DriftLevel::Warn,
            format!(
                "{scope_desc}: '{skill_id}' materialized file was user-edited (forked); left untouched ({path})"
            ),
        ),
        SkillDrift::Conflict { .. } => (
            DriftLevel::Warn,
            format!(
                "{scope_desc}: '{skill_id}' cannot materialize — a non-managed file occupies {path}"
            ),
        ),
        SkillDrift::Orphan { .. } => (
            DriftLevel::Warn,
            format!(
                "{scope_desc}: stale materialized skill '{skill_id}' ({path}); run `tracedecay update` to remove"
            ),
        ),
        SkillDrift::ForeignOrphan { .. } => (
            DriftLevel::Info,
            format!(
                "{scope_desc}: '{skill_id}' project skill from another installation; leave in place, or delete the directory manually if unwanted ({path})"
            ),
        ),
        SkillDrift::Warning { message, .. } => (
            DriftLevel::Warn,
            format!("{scope_desc}: '{skill_id}' {message} ({path})"),
        ),
    }
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

/// Check database health without mutating a store that may be owned by the daemon.
///
/// The DB path is taken from the opened instance so the size measured is the
/// same file that the active branch reader serves.
async fn check_database(
    dc: &mut DoctorCounters,
    project_path: &Path,
    open_options: TraceDecayOpenOptions,
) {
    let db_path = active_database_path(project_path, &open_options).await;
    let ts = match TraceDecay::open_read_only_with_options(project_path, open_options).await {
        Ok(ts) => ts,
        Err(e) if Database::is_corruption_error(&e) => {
            dc.fail(&format!("Database recovery required: {e}"));
            if let Some(db_path) = db_path.as_deref() {
                print_database_recovery_guidance(dc, db_path);
            } else {
                dc.info("TraceDecay could not resolve the damaged database path; no files were changed.");
            }
            return;
        }
        Err(e) => {
            dc.fail(&format!("Could not open database read-only: {e}"));
            return;
        }
    };
    let db_path = ts.db_path();
    let size_before = std::fs::metadata(&db_path).map_or(0, |m| m.len());

    dc.pass(&format!("DB size: {}", format_bytes(size_before)));

    match ts.quick_check().await {
        Ok(true) => dc.pass("DB integrity: ok"),
        Ok(false) => {
            dc.fail("Database integrity check failed; offline recovery is required");
            print_database_recovery_guidance(dc, &db_path);
        }
        Err(e) if Database::is_corruption_error(&e) => {
            dc.fail(&format!("Database recovery required: {e}"));
            print_database_recovery_guidance(dc, &db_path);
        }
        Err(e) => dc.warn(&format!(
            "Could not complete read-only integrity check: {e}"
        )),
    }
}

async fn active_database_path(
    project_path: &Path,
    open_options: &TraceDecayOpenOptions,
) -> Option<PathBuf> {
    if let Some(layout) =
        TraceDecay::initialized_store_layout_with_options(project_path, open_options).await
    {
        let branch = crate::branch::current_branch(project_path);
        return Some(
            TraceDecay::resolve_db_for_branch(project_path, &layout.data_root, branch.as_deref()).0,
        );
    }

    let data_root = crate::config::get_tracedecay_dir(project_path);
    let db_path = data_root.join(crate::config::db_filename(&data_root));
    db_path.is_file().then_some(db_path)
}

fn database_recovery_guidance(db_path: &Path) -> String {
    let wal_path = db_path.with_extension("db-wal");
    let shm_path = db_path.with_extension("db-shm");
    let data_root = db_path.parent().unwrap_or_else(|| Path::new("."));
    let dirty_path = data_root.join("dirty");
    let sessions_path = data_root.join(crate::storage::SESSIONS_DB_FILENAME);

    format!(
        "First stop all TraceDecay daemon and MCP processes. No files were changed.\n\
         Preserve this recovery set together before any repair:\n\
         DB: {}\n\
         WAL: {}\n\
         SHM: {}\n\
         dirty sentinel: {}\n\
         `sessions.db` is separate and must not be removed: {}\n\
         Facts are stored in the graph database; automatic rebuild is intentionally blocked because it cannot preserve them generically.\n\
         Do not run `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe` until that recovery set is safely copied.\n\
         Report the preserved set at https://github.com/ScriptedAlchemy/tracedecay/issues for offline recovery.",
        db_path.display(),
        wal_path.display(),
        shm_path.display(),
        dirty_path.display(),
        sessions_path.display(),
    )
}

fn print_database_recovery_guidance(dc: &DoctorCounters, db_path: &Path) {
    for line in database_recovery_guidance(db_path).lines() {
        dc.info(line);
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
    let mut orphan_count = 0;
    let mut warnings = report.issues;
    for plan in report.plans {
        let key = crate::global_db::GlobalDb::canonical_project_key(&plan.project.project_root);
        if registered.contains(&key) {
            continue;
        }
        match plan.status {
            crate::migrate::registry::RegistryReconstructionStatus::Eligible => {
                orphan_count += 1;
            }
            crate::migrate::registry::RegistryReconstructionStatus::Blocked => {
                warnings.push(format!(
                    "blocked store manifest '{}': {}",
                    plan.manifest_path.display(),
                    plan.status_reason.as_deref().unwrap_or("not eligible")
                ));
            }
            crate::migrate::registry::RegistryReconstructionStatus::Stale
            | crate::migrate::registry::RegistryReconstructionStatus::Retired => {}
        }
    }
    (orphan_count, warnings)
}

/// Reports git-metadata watcher health (design D3/D5).
///
/// The watcher lives in the daemon; its per-project state is only in-process, so
/// this section sources telemetry the read-only way: recent `git_watch_*` events
/// from the daemon log (systemd journal on Linux, launchd err-log on macOS). It
/// reports whether the watcher is active vs degraded (mtime-poll fallback) per
/// project. Absent telemetry is reported as info, not a failure — the watcher is
/// a best-effort freshness aid backed by the on-read/hook sync paths.
fn check_watcher(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mWatcher\x1b[0m");

    let config = crate::config::SyncConfig::default().with_env_overrides();
    if !config.auto_watch {
        dc.info("Git-metadata watcher disabled (`sync.auto_watch = false`)");
        return;
    }

    if !crate::daemon::daemon_reachable() {
        dc.info("Daemon not running — watcher inactive; sync happens on hook/read events");
        return;
    }

    #[cfg(unix)]
    {
        let events = crate::daemon::recent_watcher_events(2000);
        if events.is_empty() {
            dc.info("Daemon running; no recent watcher telemetry in the log yet");
            return;
        }
        let mut degraded = 0usize;
        let mut active = 0usize;
        let mut projects: Vec<_> = events.into_iter().collect();
        projects.sort_by(|a, b| a.0.cmp(&b.0));
        for (project, ev) in projects {
            match ev.event.as_str() {
                "git_watch_degraded" => {
                    degraded += 1;
                    dc.warn(&format!(
                        "{project}: degraded (mtime-poll fallback){}",
                        ev.detail.map(|d| format!(" — {d}")).unwrap_or_default()
                    ));
                }
                "git_watch_restart" => {
                    dc.warn(&format!("{project}: watcher restarting after failure"));
                }
                _ => {
                    active += 1;
                    dc.pass(&format!(
                        "{project}: active ({})",
                        ev.detail.unwrap_or_else(|| ev.event.clone())
                    ));
                }
            }
        }
        if degraded == 0 && active > 0 {
            dc.info(&format!("{active} project(s) watched, none degraded"));
        }
    }

    #[cfg(not(unix))]
    dc.info("Git-metadata watcher is only available on Unix daemons");
}

/// Check user config file.
fn check_user_config(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mUser config\x1b[0m");
    if let Some(config_path) = crate::user_config::config_path() {
        if config_path.exists() {
            let config = crate::user_config::UserConfig::load();
            dc.pass(&format!("Config: {}", config_path.display()));
            if config.upload_enabled {
                dc.pass("Worldwide counter upload enabled");
            } else {
                dc.info("Worldwide counter upload disabled (default)");
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
    if crate::user_config::UserConfig::load().upload_enabled {
        if let Some(total) = crate::cloud::fetch_worldwide_total() {
            dc.pass(&format!(
                "Worldwide counter reachable (total: {})",
                format_token_count(total)
            ));
        } else {
            dc.warn("Worldwide counter unreachable (offline or timeout)");
        }
    } else {
        dc.info("Worldwide counter skipped (upload disabled)");
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
mod tests;
