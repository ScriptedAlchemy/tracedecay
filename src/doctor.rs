//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::path::{Component, Path, PathBuf};

use crate::agents::{self, DoctorCounters, HealthcheckContext};
use crate::display::{format_bytes, format_token_count};
#[cfg(test)]
use crate::storage::StoreLayout;
#[cfg(test)]
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

pub mod heal;
pub(crate) mod registry_drift;

/// Runs a comprehensive health check of the tracedecay installation.
pub async fn run_doctor(agent_filter: Option<&str>) -> crate::errors::Result<()> {
    let _lifecycle_lease = match crate::lifecycle_lease::acquire_shared_or_inherited("doctor") {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("tracedecay doctor could not start: {error}");
            return Err(error);
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
    let daemon_status = daemon_project_status(&project_path).await;
    let storage_healthy = match daemon_status.as_ref() {
        Ok(status) => check_database(&mut dc, status),
        Err(error) => {
            report_daemon_diagnostics_unavailable(
                &mut dc,
                fallback_database_path(&project_path).as_deref(),
                error,
            );
            false
        }
    };

    check_global_db(&mut dc);
    check_stale_stores(&mut dc, daemon_status.as_ref().ok());
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
            ag.healthcheck_with_daemon_status(&mut dc, &hctx, daemon_status.as_ref().ok());
        }
        let materialization_root =
            crate::automation::skill_materialization::resolve_project_root(&project_path);
        check_managed_skill_materialization(&mut dc, home, &materialization_root);
    } else {
        dc.fail("Could not determine home directory");
    }

    check_network(&mut dc);
    print_summary(&dc);

    match daemon_status {
        Err(error) => Err(error),
        Ok(_) if !storage_healthy => Err(crate::errors::TraceDecayError::Config {
            message: "doctor storage health check failed".to_string(),
        }),
        Ok(_) => Ok(()),
    }
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
#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

async fn daemon_project_status(project_path: &Path) -> crate::errors::Result<serde_json::Value> {
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let result =
        crate::daemon::call_default_tool(&handshake, "tracedecay_runtime", daemon_runtime_args())
            .await?;
    daemon_runtime_status(&result)
}

fn daemon_runtime_args() -> serde_json::Value {
    serde_json::json!({
        "format": "json",
        "authority_audit": true,
        "session_ingest_health": true,
    })
}

fn daemon_runtime_status(result: &serde_json::Value) -> crate::errors::Result<serde_json::Value> {
    let runtime = crate::daemon::tool_json_payload(result, "tracedecay_runtime")?;
    let mut storage =
        runtime
            .get("database")
            .cloned()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "daemon runtime response omitted database telemetry".to_string(),
            })?;
    let storage =
        storage
            .as_object_mut()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "daemon runtime database telemetry was not an object".to_string(),
            })?;
    if let Some(pid) = runtime.pointer("/process/pid").cloned() {
        storage.insert("daemon_owner_pid".to_string(), pid);
    }
    if let Some(version) = runtime.get("tracedecay_version").cloned() {
        storage.insert("daemon_version".to_string(), version);
    }
    let mut status = serde_json::json!({ "storage_health": storage });
    for key in ["cursor_session_ingest", "cursor_session_placeholder_paths"] {
        if let Some(value) = runtime.get(key).cloned() {
            status[key] = value;
        }
    }
    Ok(status)
}

fn check_database(dc: &mut DoctorCounters, status: &serde_json::Value) -> bool {
    let Some(storage) = status.get("storage_health") else {
        dc.fail("Daemon status omitted storage health; doctor did not open SQLite");
        return false;
    };
    let db_path = storage
        .get("canonical_db_path")
        .or_else(|| storage.get("db_path"))
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from);
    if let Some(path) = db_path.as_deref() {
        dc.pass(&format!("Index found: {} (daemon-owned)", path.display()));
    }
    if let Some(size) = storage
        .get("db_size_bytes")
        .and_then(serde_json::Value::as_u64)
    {
        dc.pass(&format!("DB size: {}", format_bytes(size)));
    }
    let integrity_healthy = match storage
        .get("quick_check_ok")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {
            dc.pass("DB integrity: ok (checked by daemon owner)");
            true
        }
        Some(false) => {
            dc.fail("Database integrity check failed; offline recovery is required");
            if let Some(path) = db_path.as_deref() {
                print_database_recovery_guidance(dc, path);
            }
            false
        }
        None => {
            let detail = storage
                .get("quick_check_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("daemon did not return a quick_check result");
            dc.fail(&format!("Database diagnostics unavailable: {detail}"));
            if let Some(path) = db_path.as_deref() {
                print_database_recovery_guidance(dc, path);
            }
            false
        }
    };
    let authority_healthy = match storage
        .get("authority_audit_ok")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {
            dc.pass("Observation database authority: ok (checked by daemon owner)");
            true
        }
        Some(false) => {
            let detail = storage
                .get("authority_audit_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("daemon reported an authority audit failure without detail");
            dc.fail(&format!(
                "Observation database authority audit failed: {detail}"
            ));
            false
        }
        None => {
            let detail = storage
                .get("authority_audit_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("daemon did not return an authority audit result");
            dc.fail(&format!(
                "Observation database authority diagnostics unavailable: {detail}"
            ));
            false
        }
    };
    if storage
        .pointer("/dirty_marker/exists")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        let state = storage
            .pointer("/dirty_marker/state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unparsed");
        dc.warn(&format!("Graph dirty marker present (state={state})"));
    }
    integrity_healthy && authority_healthy
}

fn report_daemon_diagnostics_unavailable(
    dc: &mut DoctorCounters,
    db_path: Option<&Path>,
    error: &crate::errors::TraceDecayError,
) {
    dc.fail(&format!(
        "Database diagnostics unavailable from the sole daemon owner: {error}. Doctor did not open SQLite."
    ));
    if let Some(path) = db_path {
        print_database_recovery_guidance(dc, path);
    } else {
        dc.info("The database path could not be resolved without opening registry SQLite; stop all TraceDecay processes and preserve the project store before repair.");
    }
}

fn fallback_database_path(project_path: &Path) -> Option<PathBuf> {
    if let Ok(Some(marker)) = crate::storage::read_enrollment_marker(project_path)
        && let Ok(profile_root) = crate::storage::default_profile_root()
        && let Ok(layout) =
            crate::storage::profile_sharded_layout(project_path, &profile_root, &marker)
    {
        return Some(layout.graph_db_path);
    }
    let data_root = crate::config::get_tracedecay_dir(project_path);
    let db_path = data_root.join(crate::config::db_filename(&data_root));
    db_path.is_file().then_some(db_path)
}

fn database_recovery_guidance(db_path: &Path) -> String {
    let wal_path = db_path.with_extension("db-wal");
    let shm_path = db_path.with_extension("db-shm");
    let data_root = db_path.parent().unwrap_or_else(|| Path::new("."));
    let mut graph_dirty = db_path.as_os_str().to_os_string();
    graph_dirty.push(".dirty");
    let graph_dirty = PathBuf::from(graph_dirty);
    let legacy_dirty = data_root.join("dirty");
    let sessions_path = data_root.join(crate::storage::SESSIONS_DB_FILENAME);

    format!(
        "First stop all TraceDecay daemon and MCP processes. No files were changed.\n\
         Preserve this recovery set together before any repair:\n\
         DB: {}\n\
         WAL: {}\n\
         SHM: {}\n\
         graph dirty sentinel: {}\n\
         legacy dirty sentinel (if present): {}\n\
         `sessions.db` is separate and must not be removed: {}\n\
         Facts are stored in the graph database; automatic rebuild is intentionally blocked because it cannot preserve them generically.\n\
         Do not run `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe` until that recovery set is safely copied.\n\
         Report the preserved set at https://github.com/ScriptedAlchemy/tracedecay/issues for offline recovery.",
        db_path.display(),
        wal_path.display(),
        shm_path.display(),
        graph_dirty.display(),
        legacy_dirty.display(),
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

/// Registry `SQLite` is owned by the daemon. The external doctor reports that
/// ownership and leaves stale-row inspection/repair to daemon-backed tools.
fn check_stale_stores(dc: &mut DoctorCounters, status: Option<&serde_json::Value>) {
    eprintln!("\n\x1b[1mStorage registry\x1b[0m");
    if let Some(storage) = status.and_then(|value| value.get("storage_health")) {
        let owner = storage
            .get("daemon_owner_pid")
            .and_then(serde_json::Value::as_u64)
            .map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
        let identity = storage
            .get("daemon_generation")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || {
                    storage
                        .get("daemon_version")
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(
                            || "identity=unknown".to_string(),
                            |version| format!("version={version}"),
                        )
                },
                |generation| format!("generation={generation}"),
            );
        dc.pass(&format!(
            "Registry/database inspection delegated to daemon owner pid={owner}, {identity}"
        ));
    } else {
        dc.warn("Registry diagnostics unavailable because the daemon owner did not answer; doctor did not open the global DB");
    }
    dc.info("Use `tracedecay projects list` for daemon-backed registry inspection and `tracedecay migrate registry-gc --json` to preview explicit offline cleanup.");
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStorageStatus {
    RepoLocal,
    ProfileSharded,
    ManifestReconstructable,
    Stale,
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
#[derive(Debug, Clone)]
struct RegistryStoreArtifacts {
    graph_db_path: PathBuf,
    manifest_path: Option<PathBuf>,
}

#[cfg(test)]
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

#[cfg(test)]
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
    if let Ok(canonical) = profile_root.canonicalize()
        && !roots.iter().any(|root| root == &canonical)
    {
        roots.push(canonical);
    }
    roots
}

/// Counts profile store manifests with no matching registry row, plus any
/// manifest scan issues. Shared between `doctor` and the post-update health
/// pass.
pub(crate) async fn orphan_store_manifest_report(
    global_db: &crate::global_db::GlobalDb,
    profile_root: &Path,
) -> (usize, Vec<String>) {
    let report = crate::migrate::registry::scan_profile_store_manifests(
        profile_root,
        crate::tracedecay::current_timestamp(),
    );
    let mut warnings = report.issues.clone();
    for plan in &report.plans {
        match plan.status {
            crate::migrate::registry::RegistryReconstructionStatus::Blocked => {
                warnings.push(format!(
                    "blocked store manifest '{}': {}",
                    plan.manifest_path.display(),
                    plan.status_reason.as_deref().unwrap_or("not eligible")
                ));
            }
            crate::migrate::registry::RegistryReconstructionStatus::Eligible
            | crate::migrate::registry::RegistryReconstructionStatus::Stale
            | crate::migrate::registry::RegistryReconstructionStatus::Retired => {}
        }
    }
    let diff =
        crate::migrate::registry::diff_registry_reconstruction_report(global_db, &report).await;
    warnings.extend(diff.issues);
    (diff.missing_plans, warnings)
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
