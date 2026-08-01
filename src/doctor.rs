//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt};

use crate::agents::{self, DoctorCounters, HealthcheckContext};
use crate::application::semantic_runtime::{SemanticRuntimeStateV1, SemanticRuntimeStatusV1};
use crate::display::{format_bytes, format_token_count};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_lsp::analyzer::adapters::builtin_adapters;
use tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings;

#[cfg(test)]
pub(crate) struct DoctorTestRuntime {
    database: std::sync::Arc<crate::global_db::RegisteredGlobalDb>,
    _registry: crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    _scope: crate::db::DaemonDatabaseScope,
}

#[cfg(test)]
impl DoctorTestRuntime {
    pub(crate) async fn open(profile_root: &Path, label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NONCE: AtomicU64 = AtomicU64::new(1);

        std::fs::create_dir_all(profile_root).expect("create Doctor test profile root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700))
                .expect("secure Doctor test profile root");
        }
        let identity = crate::daemon::profile_identity::load_or_create(profile_root)
            .expect("load Doctor test profile identity");
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let scope = crate::db::enter_daemon_database_scope(profile_root, nonce, label)
            .expect("enter Doctor test database scope");
        let registry =
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await
            .expect("open Doctor test runtime registry");
        let database = registry
            .profile_database()
            .await
            .expect("mount Doctor test profile database");
        Self {
            database,
            _registry: registry,
            _scope: scope,
        }
    }

    pub(crate) fn database(&self) -> &crate::global_db::RegisteredGlobalDb {
        self.database.as_ref()
    }

    pub(crate) fn database_arc(&self) -> std::sync::Arc<crate::global_db::RegisteredGlobalDb> {
        std::sync::Arc::clone(&self.database)
    }
}

pub mod heal;
// Consumed by the unix-only daemon git-watch maintenance path; on other
// targets only the module's tests reference it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) mod registry_drift;
mod temporal_health;

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
        !crate::version::build_version().is_empty(),
        "the reported build version must not be empty"
    );
    let mut dc = DoctorCounters::new();

    eprintln!(
        "\n\x1b[1mtracedecay doctor v{}\x1b[0m\n",
        crate::version::build_version()
    );

    check_binary(&mut dc);

    eprintln!("\n\x1b[1mCurrent project\x1b[0m");
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    check_inert_project_config(&mut dc, &project_path);
    let daemon_status = daemon_project_status(&project_path).await;
    let storage_health = match daemon_status.as_ref() {
        Ok(status) => {
            let health = check_database(&mut dc, status);
            check_daemon_doctor_report(&mut dc, status);
            health
        }
        Err(error) => {
            report_daemon_diagnostics_unavailable(
                &mut dc,
                fallback_database_path(&project_path).as_deref(),
                error,
            );
            DatabaseHealth::Failed {
                reason: "daemon_diagnostics_unavailable".to_string(),
            }
        }
    };
    if let DatabaseHealth::Unknown { reason } = &storage_health {
        dc.info(&format!(
            "Storage health is unknown [{reason}]; doctor observed no healthy store and is not claiming one"
        ));
    }
    check_session_temporal_health(&mut dc, daemon_status.as_ref().ok());
    check_semantic_runtime_health(&mut dc, daemon_status.as_ref().ok());

    check_global_db(&mut dc);
    check_stale_stores(&mut dc, daemon_status.as_ref().ok());
    check_watcher(&mut dc);
    check_user_config(&mut dc);
    check_external_tools(&mut dc);
    check_language_analyzers(&mut dc, &project_path).await;

    // Agent-specific health checks
    if let Some(ref home) = agents::home_dir() {
        let hctx = HealthcheckContext {
            home: home.clone(),
            project_path: project_path.clone(),
        };
        let host_components = check_host_component_receipts(&mut dc, &hctx, agent_filter);
        let receipt_agents = host_components
            .components
            .iter()
            .filter_map(|component| component.host)
            .map(agents::integration_id_for_host)
            .collect::<BTreeSet<_>>();
        let legacy_agents = match agent_filter {
            Some(id) => match agents::get_integration(id) {
                Ok(agent) => vec![agent],
                Err(error) => {
                    dc.fail(&error.to_string());
                    Vec::new()
                }
            },
            None => agents::all_integrations(),
        };
        for agent in legacy_agents {
            if !receipt_agents.contains(agent.id()) && agent.has_tracedecay(home) {
                agent.healthcheck_with_daemon_status(&mut dc, &hctx, daemon_status.as_ref().ok());
            }
        }
        let materialization_root =
            crate::automation::skill_materialization::resolve_project_root(&project_path);
        check_managed_skill_materialization(&mut dc, home, &materialization_root);
    } else {
        dc.fail("Could not determine home directory");
    }

    check_network(&mut dc);
    print_summary(&dc);

    doctor_result(&dc, daemon_status, &storage_health)
}

fn check_host_component_receipts(
    dc: &mut DoctorCounters,
    context: &HealthcheckContext,
    agent_filter: Option<&str>,
) -> agents::host_bundle_v2::HostBundleDoctorReportV1 {
    eprintln!("\n\x1b[1mReceipt-backed host components\x1b[0m");
    let lifecycle_root = match agents::host_bundle_v2::resolved_host_bundle_lifecycle_root() {
        Ok(root) => root,
        Err(error) => {
            dc.fail(&format!("Could not resolve host lifecycle root: {error}"));
            return agents::host_bundle_v2::HostBundleDoctorReportV1::default();
        }
    };
    let report = match agents::inspect_installed_host_components(context) {
        Ok(report) => report,
        Err(error) => {
            dc.fail(&format!(
                "Could not inspect host component receipts in {}: {error}",
                lifecycle_root.display()
            ));
            return agents::host_bundle_v2::HostBundleDoctorReportV1::default();
        }
    };
    if report.components.is_empty() {
        dc.info(&format!(
            "No installed host component receipts in {}",
            lifecycle_root.display()
        ));
        report_native_edit_stop_conformance(dc, &report, agent_filter);
        return report;
    }
    for component in &report.components {
        if agent_filter.is_some_and(|filter| {
            component
                .host
                .is_some_and(|host| agents::integration_id_for_host(host) != filter)
        }) {
            continue;
        }
        report_host_component_state(dc, component);
    }
    report_native_edit_stop_conformance(dc, &report, agent_filter);
    report
}

/// Report one installed component. Doctor is read-only: it never repairs, so
/// each state only decides whether the operator is told this is blocking.
///
/// Content drift on a path the component still owns, and a leftover
/// registration with no owning receipt, both converge under the ordinary
/// reinstall, so they warn and leave the exit code clean. A component staged
/// for a host that only activates through its own UI warns for the same reason
/// in reverse: no unattended command can converge it, so failing the run would
/// block every machine whose operator has not clicked through the host yet. A
/// contested path, missing artifacts, and corrupt state each need an operator
/// decision, so they keep failing.
fn report_host_component_state(
    dc: &mut DoctorCounters,
    component: &agents::host_bundle_v2::HostBundleComponentDoctorResultV1,
) {
    use agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let label = match (component.host, component.component) {
        (Some(host), Some(inner)) => format!("{host:?}/{inner:?}"),
        _ => component.receipt_path.display().to_string(),
    };
    match component.state {
        State::Current => dc.pass(&format!("{label} is current")),
        State::Repairable => dc.warn(&format!(
            "{label} native registration is repairable; {}",
            component.repair_action
        )),
        State::Drifted => dc.warn(&format!(
            "{label} has drifted from its receipt ({}); {}",
            drifted_paths(component),
            component.repair_action
        )),
        State::OwnershipConflict => dc.fail(&format!(
            "{label} has an ownership conflict; {}",
            component.repair_action
        )),
        State::OrphanedRegistration => dc.warn(&format!(
            "{label} is still registered with no owning receipt; {}",
            component.repair_action
        )),
        State::ActivationDeferred => dc.warn(&format!(
            "{label} is staged and waiting on interactive host activation; {}",
            component.repair_action
        )),
        State::Missing => dc.fail(&format!(
            "{label} is missing receipt-owned artifacts; {}",
            component.repair_action
        )),
        State::Corrupt => dc.fail(&format!(
            "{label} receipt or deployed state is corrupt; {}",
            component.repair_action
        )),
    }
    for artifact in component
        .artifacts
        .iter()
        .filter(|artifact| artifact.state != State::Current)
    {
        dc.info(&format!("{}: {:?}", artifact.relative_path, artifact.state));
    }
}

/// Name the exact receipt-owned paths whose bytes moved, so the warning points
/// at files rather than at a component label.
fn drifted_paths(component: &agents::host_bundle_v2::HostBundleComponentDoctorResultV1) -> String {
    use agents::host_bundle_v2::HostBundleComponentDoctorStateV1 as State;

    let paths = component
        .artifacts
        .iter()
        .filter(|artifact| artifact.state == State::Drifted)
        .map(|artifact| artifact.relative_path.as_str())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        "no receipt-owned path reported drift".to_string()
    } else {
        paths.join(", ")
    }
}

/// Surface the checked-in native edit/stop fixture behind each packaged host so
/// a host whose fixture proves neither boundary is visible here rather than
/// silently relying on a documented-but-uncaptured protocol.
fn report_native_edit_stop_conformance(
    dc: &mut DoctorCounters,
    report: &agents::host_bundle_v2::HostBundleDoctorReportV1,
    agent_filter: Option<&str>,
) {
    use agents::host_bundle_v2::HostCapabilityStateV1 as State;

    for evidence in &report.native_edit_stop_conformance {
        if agent_filter
            .is_some_and(|filter| agents::integration_id_for_host(evidence.host) != filter)
        {
            continue;
        }
        let label = format!(
            "{:?} native {} fixture ({})",
            evidence.host, evidence.evidenced_event, evidence.source_path
        );
        match (evidence.edit, evidence.stop) {
            (State::Supported, State::Supported) => {
                dc.pass(&format!("{label} proves saved-edit and stop"));
            }
            (edit, stop) => dc.info(&format!(
                "{label} proves edit={edit:?}, stop={stop:?}; unproven boundaries stay unavailable"
            )),
        }
    }
}

/// Gates the doctor exit code.
///
/// Only an observed storage *failure* is fatal. `DatabaseHealth::Unknown` — a
/// diagnostic that could not run — is reported to the user but never laundered
/// into a healthy verdict nor turned into a hard failure.
fn doctor_result(
    dc: &DoctorCounters,
    daemon_status: crate::errors::Result<serde_json::Value>,
    storage_health: &DatabaseHealth,
) -> crate::errors::Result<()> {
    match daemon_status {
        Err(error) => Err(error),
        Ok(_) => match storage_health {
            DatabaseHealth::Failed { reason } => Err(crate::errors::TraceDecayError::Config {
                message: format!("doctor storage health check failed [{reason}]"),
            }),
            DatabaseHealth::Healthy | DatabaseHealth::Unknown { .. } if dc.issues > 0 => {
                Err(crate::errors::TraceDecayError::Config {
                    message: format!("doctor found {} issue(s)", dc.issues),
                })
            }
            DatabaseHealth::Healthy | DatabaseHealth::Unknown { .. } => Ok(()),
        },
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

async fn daemon_project_status(project_path: &Path) -> crate::errors::Result<serde_json::Value> {
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let result = crate::daemon::call_default_tool_within(
        &handshake,
        "tracedecay_runtime",
        daemon_doctor_runtime_args(),
        // Diagnostic probe, not a liveness gate. A multi-gigabyte store
        // cold-opening while agents saturate the daemon can take well over 10s
        // for its first integrity read; a warm steady-state read returns in well
        // under a second. Give it headroom so a contended read reports real
        // status instead of failing the post-update with a spurious timeout.
        tokio::time::Instant::now() + std::time::Duration::from_secs(90),
    )
    .await?;
    daemon_runtime_status(&result)
}

async fn daemon_project_status_with_deadline(
    project_path: &Path,
    startup_deadline: tokio::time::Instant,
    report_admission: bool,
    startup_health_only: bool,
) -> crate::errors::Result<serde_json::Value> {
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    // Startup validation must observe the routed project's terminal open
    // failure. The ordinary Doctor helper intentionally falls back to a cold
    // snapshot on daemon errors, which is useful for diagnostics but would
    // conceal a cached non-retryable warm-up failure here.
    // Cold-open admission under heavy load can exceed a tight 10s bound; keep it
    // generous (still capped by the outer startup deadline) so warm-up isn't
    // misreported as a terminal admission failure.
    let admission_deadline =
        (tokio::time::Instant::now() + std::time::Duration::from_secs(90)).min(startup_deadline);
    let admission = crate::daemon::call_default_tool_within(
        &handshake,
        "tracedecay_status",
        daemon_admission_args(),
        admission_deadline,
    )
    .await;
    let admitted = match admission {
        Ok(_) => true,
        Err(error) if crate::daemon::error_message_is_project_warming(&error.to_string()) => false,
        Err(error) => return Err(error),
    };
    if report_admission && admitted {
        eprintln!(
            "Daemon project admitted; waiting for runtime integrity telemetry within the startup deadline."
        );
    }
    let result = crate::daemon::call_default_tool_within(
        &handshake,
        "tracedecay_runtime",
        if startup_health_only {
            daemon_startup_runtime_args()
        } else {
            daemon_doctor_runtime_args()
        },
        startup_deadline,
    )
    .await?;
    daemon_runtime_status(&result)
}

pub async fn wait_for_daemon_startup_health(
    timeout: std::time::Duration,
) -> crate::errors::Result<()> {
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let startup_deadline = tokio::time::Instant::now() + timeout;
    wait_for_daemon_startup_health_with(
        timeout,
        std::time::Duration::from_millis(500),
        || daemon_project_status_with_deadline(&project_path, startup_deadline, true, true),
        |progress| {
            eprintln!(
                "Waiting for daemon startup health convergence: elapsed={}s waiting_on={} change={}",
                progress.elapsed.as_secs(),
                progress.detail,
                progress.change,
            );
        },
    )
    .await
}

#[derive(Debug)]
struct DaemonStartupHealthProgress {
    elapsed: std::time::Duration,
    detail: String,
    change: String,
}

#[derive(Debug)]
enum DaemonStartupHealthOutcome {
    Ready,
    Retryable {
        detail: String,
    },
    Terminal {
        error: crate::errors::TraceDecayError,
    },
    DeadlineExceeded {
        timeout: std::time::Duration,
        last_detail: String,
    },
}

async fn wait_for_daemon_startup_health_with<Probe, ProbeFuture, Progress>(
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
    mut probe: Probe,
    mut progress: Progress,
) -> crate::errors::Result<()>
where
    Probe: FnMut() -> ProbeFuture,
    ProbeFuture: std::future::Future<Output = crate::errors::Result<serde_json::Value>>,
    Progress: FnMut(DaemonStartupHealthProgress),
{
    let started = std::time::Instant::now();
    let deadline = started + timeout;
    let mut last_detail = None;
    let mut last_report = started
        .checked_sub(std::time::Duration::from_secs(20))
        .unwrap_or(started);
    loop {
        let detail = match classify_daemon_startup_health_result(probe().await) {
            DaemonStartupHealthOutcome::Ready => return Ok(()),
            DaemonStartupHealthOutcome::Retryable { detail } => detail,
            DaemonStartupHealthOutcome::Terminal { error } => return Err(error),
            deadline @ DaemonStartupHealthOutcome::DeadlineExceeded { .. } => {
                return Err(daemon_startup_health_failure(deadline));
            }
        };
        let now = std::time::Instant::now();
        let changed = last_detail.as_deref() != Some(detail.as_str());
        if changed || now.duration_since(last_report) >= std::time::Duration::from_secs(20) {
            let change = match last_detail.as_deref() {
                None => "initial observation".to_string(),
                Some(previous) if previous != detail => format!("changed from {previous}"),
                Some(_) => "no change since previous poll".to_string(),
            };
            progress(DaemonStartupHealthProgress {
                elapsed: now.duration_since(started),
                detail: detail.clone(),
                change,
            });
            last_report = now;
        }
        last_detail = Some(detail);
        if now >= deadline {
            let outcome = DaemonStartupHealthOutcome::DeadlineExceeded {
                timeout,
                last_detail: last_detail.unwrap_or_else(|| "no health response".to_string()),
            };
            return Err(daemon_startup_health_failure(outcome));
        }
        tokio::time::sleep(poll_interval.min(deadline.saturating_duration_since(now))).await;
    }
}

fn classify_daemon_startup_health_result(
    result: crate::errors::Result<serde_json::Value>,
) -> DaemonStartupHealthOutcome {
    match result {
        Ok(status) if daemon_startup_health_ready(&status) => DaemonStartupHealthOutcome::Ready,
        Ok(status) => match daemon_startup_terminal_status_error(&status) {
            Some(error) => DaemonStartupHealthOutcome::Terminal { error },
            None => DaemonStartupHealthOutcome::Retryable {
                detail: daemon_startup_health_detail(&status),
            },
        },
        Err(error) if daemon_startup_error_is_retryable(&error) => {
            DaemonStartupHealthOutcome::Retryable {
                detail: error.to_string(),
            }
        }
        Err(error) => {
            let detail = error.to_string();
            let error = if daemon_health_reports_sqlite_corruption(&detail) {
                daemon_startup_corruption_error(&detail, None)
            } else {
                error
            };
            DaemonStartupHealthOutcome::Terminal { error }
        }
    }
}

fn daemon_startup_health_failure(
    outcome: DaemonStartupHealthOutcome,
) -> crate::errors::TraceDecayError {
    match outcome {
        DaemonStartupHealthOutcome::Terminal { error } => error,
        DaemonStartupHealthOutcome::DeadlineExceeded {
            timeout,
            last_detail,
        } => crate::errors::TraceDecayError::Config {
            message: format!(
                "daemon startup health deadline-exceeded after {}s before Doctor validation; last retryable state: {last_detail}",
                timeout.as_secs(),
            ),
        },
        DaemonStartupHealthOutcome::Ready | DaemonStartupHealthOutcome::Retryable { .. } => {
            crate::errors::TraceDecayError::Config {
                message: "daemon startup health failure was not terminal".to_string(),
            }
        }
    }
}

fn daemon_startup_terminal_status_error(
    status: &serde_json::Value,
) -> Option<crate::errors::TraceDecayError> {
    let storage = status.get("storage_health")?;
    let quick_check_ok = storage
        .get("quick_check_ok")
        .and_then(serde_json::Value::as_bool);
    let quick_check_error = storage
        .get("quick_check_error")
        .and_then(serde_json::Value::as_str);
    if quick_check_ok == Some(false)
        || quick_check_error.is_some_and(daemon_health_reports_sqlite_corruption)
    {
        let problem = quick_check_error.unwrap_or("SQLite quick_check failed without detail");
        let db_path = storage
            .get("canonical_db_path")
            .or_else(|| storage.get("db_path"))
            .and_then(serde_json::Value::as_str)
            .map(Path::new);
        return Some(daemon_startup_corruption_error(problem, db_path));
    }

    if storage
        .get("authority_audit_ok")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        let reason = storage
            .get("authority_audit_reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("authority invariant failed without detail");
        return Some(crate::errors::TraceDecayError::Config {
            message: format!(
                "terminal daemon startup health failure: observation database authority audit failed: {reason}. Preserve daemon logs and run `tracedecay doctor` with the retained or a newer compatible binary before retrying; do not run an older binary."
            ),
        });
    }

    None
}

fn daemon_startup_corruption_error(
    problem: &str,
    db_path: Option<&Path>,
) -> crate::errors::TraceDecayError {
    let remediation = match db_path {
        Some(db_path) => database_recovery_guidance_for_problem(db_path, problem),
        None if crate::tracedecay::is_fts_only_corruption(problem) => {
            "Run `tracedecay daemon restart` with the retained or a newer compatible binary so the sole-writer open path can rebuild `nodes_fts`; then run `tracedecay tool runtime` and `tracedecay doctor`. Do not run an older binary or delete the database.".to_string()
        }
        None => "Stop all TraceDecay processes and preserve the database, WAL, and SHM together before attempting repair. Do not run an older binary, `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe`.".to_string(),
    };
    crate::errors::TraceDecayError::Config {
        message: format!(
            "terminal daemon startup health failure: {problem}\nRemediation: {remediation}"
        ),
    }
}

fn daemon_health_reports_sqlite_corruption(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("sqlite_corrupt")
        || detail.contains("database disk image is malformed")
        || detail.contains("malformed database image")
        || detail.contains("file is not a database")
        || detail.contains("fts5: corruption found")
        || detail.contains("malformed inverted index for fts5")
        || detail.contains("database corruption")
        || detail.contains("database is corrupt")
}

#[allow(deprecated)]
fn daemon_startup_error_is_retryable(error: &crate::errors::TraceDecayError) -> bool {
    match error {
        crate::errors::TraceDecayError::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
        ),
        crate::errors::TraceDecayError::Config { message } => {
            (message.contains("daemon socket") && message.contains("not available"))
                || message.contains("still warming up")
                || crate::daemon::error_message_is_project_warming(message)
                || message.contains("restart grace")
                || crate::daemon::error_message_is_read_deadline(message)
                || message.contains(RUNTIME_TELEMETRY_PENDING)
        }
        crate::errors::TraceDecayError::ProjectRoute { retryable, .. } => *retryable,
        crate::errors::TraceDecayError::Automation(error) => {
            tracedecay_automation::backend::classify_agent_task_error_message(&error.to_string())
                .is_retryable()
        }
        crate::errors::TraceDecayError::File { .. }
        | crate::errors::TraceDecayError::Parse { .. }
        | crate::errors::TraceDecayError::Database { .. }
        | crate::errors::TraceDecayError::DatabaseOperation { .. }
        | crate::errors::TraceDecayError::Search { .. }
        | crate::errors::TraceDecayError::SyncLock { .. }
        | crate::errors::TraceDecayError::Sqlite(_)
        | crate::errors::TraceDecayError::Json(_) => false,
    }
}

fn daemon_startup_health_detail(status: &serde_json::Value) -> String {
    let storage = status.get("storage_health");
    let quick = if storage
        .and_then(|storage| storage.get("quick_check_ok"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        "ok"
    } else {
        storage
            .and_then(|storage| storage.get("quick_check_error"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("quick_check_pending")
    };
    format!("storage={quick}")
}

fn daemon_startup_health_ready(status: &serde_json::Value) -> bool {
    let Some(storage) = status
        .get("storage_health")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let mounted = storage
        .get("canonical_db_path")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && storage
            .get("daemon_owner_pid")
            .and_then(serde_json::Value::as_u64)
            .is_some()
        && storage
            .get("daemon_version")
            .and_then(serde_json::Value::as_str)
            .is_some();
    let integrity_failed = storage
        .get("quick_check_ok")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        || storage
            .get("quick_check_error")
            .and_then(serde_json::Value::as_str)
            .is_some()
        || storage
            .get("authority_audit_ok")
            .and_then(serde_json::Value::as_bool)
            == Some(false);
    mounted && !integrity_failed
}

fn daemon_admission_args() -> serde_json::Value {
    serde_json::json!({
        "format": "json",
        "admission_only": true,
        "include_branch_diagnostics": false,
        "include_storage_health": false,
        "include_session_ingest": false,
        "include_staleness": false,
    })
}

fn daemon_startup_runtime_args() -> serde_json::Value {
    serde_json::json!({
        "format": "json",
        "startup_health": true,
        "authority_audit": false,
        "doctor_report": false,
        "session_ingest_health": false,
    })
}

fn daemon_doctor_runtime_args() -> serde_json::Value {
    serde_json::json!({
        "format": "json",
        "startup_health": false,
        "authority_audit": true,
        "doctor_report": true,
        // `authority_audit` already requests session-temporal health. Keeping
        // ingest health false avoids the core startup-only interception and
        // routes comprehensive Doctor through the ready project owner, where
        // the composed Doctor report reader is mounted.
        "session_ingest_health": false,
    })
}

/// A routed project publishes database telemetry only after it is mounted and
/// admitted. During startup, an absent `database` block means "not published
/// yet" and remains a warming state to poll, while telemetry that is present
/// but malformed remains a terminal contract violation.
const RUNTIME_TELEMETRY_PENDING: &str = "daemon runtime response omitted database telemetry";

fn daemon_runtime_status(result: &serde_json::Value) -> crate::errors::Result<serde_json::Value> {
    let runtime = crate::daemon::tool_json_payload(result, "tracedecay_runtime")?;
    let mut storage =
        runtime
            .get("database")
            .cloned()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: RUNTIME_TELEMETRY_PENDING.to_string(),
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
    for key in [
        "cursor_session_ingest",
        "cursor_session_placeholder_paths",
        "doctor_report",
        "session_temporal_health",
        "semantic_runtime",
    ] {
        if let Some(value) = runtime.get(key).cloned() {
            status[key] = value;
        }
    }
    Ok(status)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageGrowthDoctorLineLevel {
    Information,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StorageGrowthDoctorLine {
    level: StorageGrowthDoctorLineLevel,
    message: String,
}

fn table_growth_doctor_lines(doctor_report: &serde_json::Value) -> Vec<StorageGrowthDoctorLine> {
    let kind = doctor_report
        .get("kind")
        .and_then(serde_json::Value::as_str);
    if kind != Some("observed") {
        let state = kind.unwrap_or("unavailable");
        return vec![StorageGrowthDoctorLine {
            level: StorageGrowthDoctorLineLevel::Warning,
            message: format!("Daemon Doctor report is {state}; per-table growth is unavailable"),
        }];
    }

    let Some(evidence) = doctor_report
        .get("table_growth_evidence")
        .and_then(serde_json::Value::as_array)
    else {
        return vec![StorageGrowthDoctorLine {
            level: StorageGrowthDoctorLineLevel::Warning,
            message: "Daemon Doctor report omitted typed per-table growth evidence".to_string(),
        }];
    };

    evidence
        .iter()
        .map(|item| {
            let item_kind = item
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let store = item
                .get("store")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unnamed store");
            match item_kind {
                "significant_growth" => {
                    let table = item
                        .get("table")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unnamed table");
                    match item
                        .get("growth_bytes")
                        .and_then(serde_json::Value::as_u64)
                    {
                        Some(growth) => StorageGrowthDoctorLine {
                            level: StorageGrowthDoctorLineLevel::Information,
                            message: format!(
                                "{store}.{table} grew by {} since the prior baseline",
                                format_bytes(growth)
                            ),
                        },
                        None => StorageGrowthDoctorLine {
                            level: StorageGrowthDoctorLineLevel::Warning,
                            message: format!(
                                "{store}.{table} reported significant growth without a byte measurement"
                            ),
                        },
                    }
                }
                "baseline_established" => {
                    let tables = item
                        .get("tables_observed")
                        .and_then(serde_json::Value::as_u64);
                    let detail = tables.map_or_else(
                        || "an unknown number of tables".to_string(),
                        |count| format!("{count} tables"),
                    );
                    StorageGrowthDoctorLine {
                        level: StorageGrowthDoctorLineLevel::Warning,
                        message: format!(
                            "{store} had no prior baseline; established one across {detail}, so growth is not yet measurable"
                        ),
                    }
                }
                "unsupported" | "denied" | "unknown" => StorageGrowthDoctorLine {
                    level: StorageGrowthDoctorLineLevel::Warning,
                    message: format!(
                        "{store} per-table growth measurement is unavailable ({item_kind})"
                    ),
                },
                other => StorageGrowthDoctorLine {
                    level: StorageGrowthDoctorLineLevel::Warning,
                    message: format!(
                        "{store} returned an unrecognized per-table growth state ({other})"
                    ),
                },
            }
        })
        .collect()
}

fn check_daemon_doctor_report(dc: &mut DoctorCounters, status: &serde_json::Value) {
    eprintln!("\n\x1b[1mStorage table growth\x1b[0m");
    let Some(report) = status.get("doctor_report") else {
        dc.warn("Daemon did not expose its Doctor report; per-table growth is unavailable");
        return;
    };
    let lines = table_growth_doctor_lines(report);
    if lines.is_empty() {
        dc.pass("No significant table payload growth observed since the prior baseline");
        return;
    }
    for line in lines {
        match line.level {
            StorageGrowthDoctorLineLevel::Information => dc.info(&line.message),
            StorageGrowthDoctorLineLevel::Warning => dc.warn(&line.message),
        }
    }
}

/// What Doctor actually observed about the current project's storage.
///
/// Deliberately three-state: a diagnostic that could not run (`Unknown`) is not
/// evidence of a sound store, so it must never collapse into `Healthy`. Only
/// `Failed` is an observed failure, and only `Failed` gates the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DatabaseHealth {
    Healthy,
    Unknown { reason: String },
    Failed { reason: String },
}

impl DatabaseHealth {
    fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }

    /// Combines two independent observations, keeping the most severe:
    /// `Failed` > `Unknown` > `Healthy`.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (failed @ Self::Failed { .. }, _) | (_, failed @ Self::Failed { .. }) => failed,
            (unknown @ Self::Unknown { .. }, _) | (_, unknown @ Self::Unknown { .. }) => unknown,
            (Self::Healthy, Self::Healthy) => Self::Healthy,
        }
    }
}

fn check_database(dc: &mut DoctorCounters, status: &serde_json::Value) -> DatabaseHealth {
    let Some(storage) = status.get("storage_health") else {
        dc.fail("Daemon status omitted storage health; doctor did not open SQLite");
        return DatabaseHealth::failed("storage_health_missing");
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
    let integrity_health = match storage
        .get("quick_check_ok")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {
            dc.pass("DB integrity: ok (checked by daemon owner)");
            DatabaseHealth::Healthy
        }
        Some(false) => {
            let detail = storage
                .get("quick_check_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no problem detail reported");
            let recovery = if crate::tracedecay::is_fts_only_corruption(detail) {
                "derived FTS recovery is required"
            } else {
                "offline recovery is required"
            };
            dc.fail(&format!(
                "Database integrity check failed ({detail}); {recovery}"
            ));
            if let Some(path) = db_path.as_deref() {
                print_database_recovery_guidance_for_problem(dc, path, detail);
            }
            DatabaseHealth::failed(format!("integrity_check_failed: {detail}"))
        }
        None => {
            let detail = storage
                .get("quick_check_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("daemon did not return a quick_check result");
            dc.warn(&format!(
                "Database integrity diagnostics unavailable: {detail}; no clean result was inferred"
            ));
            DatabaseHealth::unknown(format!("integrity_diagnostics_unavailable: {detail}"))
        }
    };
    let authority_health = match storage
        .get("authority_audit_ok")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {
            dc.pass("Observation database authority: ok (checked by daemon owner)");
            DatabaseHealth::Healthy
        }
        Some(false) => {
            let reason = storage
                .get("authority_audit_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("authority_invariant_failed");
            let detail = storage
                .get("authority_audit_error")
                .and_then(serde_json::Value::as_str);
            dc.fail(&authority_audit_failure_message(reason, detail));
            DatabaseHealth::failed(reason)
        }
        None => {
            // Producers before the typed-reason key wrote the vocabulary into
            // `authority_audit_error`; read it before falling back to the bare
            // literal so a known reason is never flattened into "unavailable".
            let reason = storage
                .get("authority_audit_reason")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    storage
                        .get("authority_audit_error")
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("authority_audit_unavailable");
            dc.warn(authority_audit_unavailable_message(reason));
            DatabaseHealth::unknown(reason)
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
    integrity_health.merge(authority_health)
}

/// Builds the operator-facing failure line, preserving the observed detail the
/// producer reported. Dropping that detail leaves the user with a bare
/// classification and nothing to act on.
fn authority_audit_failure_message(reason: &str, detail: Option<&str>) -> String {
    let classification = match reason {
        "authority_invariant_failed" => {
            "Observation database authority audit failed [authority_invariant_failed]"
        }
        _ => "Observation database authority audit failed [authority_audit_failed]",
    };
    match detail {
        // Older producers mirrored the reason into the detail key; do not echo
        // the same token twice.
        Some(detail) if !detail.is_empty() && detail != reason => {
            format!("{classification}: {detail}")
        }
        _ => classification.to_string(),
    }
}

fn authority_audit_unavailable_message(reason: &str) -> &'static str {
    match reason {
        "authority_store_missing" => {
            "Observation database authority diagnostics unavailable [authority_store_missing]"
        }
        "authority_store_unavailable" => {
            "Observation database authority diagnostics unavailable [authority_store_unavailable]"
        }
        "authority_audit_not_run" => {
            "Observation database authority diagnostics unavailable [authority_audit_not_run]"
        }
        _ => "Observation database authority diagnostics unavailable [authority_audit_unavailable]",
    }
}

fn check_session_temporal_health(dc: &mut DoctorCounters, status: Option<&serde_json::Value>) {
    eprintln!("\n\x1b[1mSession temporal health\x1b[0m");
    let recovery_pending = status.is_some_and(|status| {
        status
            .pointer("/doctor_report/reason")
            .and_then(serde_json::Value::as_str)
            == Some("doctor_report_owner_warming")
    });
    let diagnosis = temporal_health::diagnose_with_recovery(
        status.and_then(|status| status.get("session_temporal_health")),
        recovery_pending,
    );
    for line in diagnosis.lines() {
        match line.level {
            temporal_health::TemporalHealthLineLevel::Pass => dc.pass(&line.text),
            temporal_health::TemporalHealthLineLevel::Warn => dc.warn(&line.text),
            temporal_health::TemporalHealthLineLevel::Fail => dc.fail(&line.text),
        }
    }
}

fn check_semantic_runtime_health(dc: &mut DoctorCounters, status: Option<&serde_json::Value>) {
    eprintln!("\n\x1b[1mSemantic runtime\x1b[0m");
    let Some(raw) = status.and_then(|status| status.get("semantic_runtime")) else {
        dc.info(
            "Semantic runtime unavailable; exact, lexical, and graph search remain healthy offline",
        );
        return;
    };
    let semantic: SemanticRuntimeStatusV1 = match serde_json::from_value(raw.clone()) {
        Ok(status) => status,
        Err(error) => {
            dc.warn(&format!(
                "Semantic runtime status is invalid ({error}); exact, lexical, and graph search remain healthy"
            ));
            return;
        }
    };
    if let Err(error) = semantic.validate() {
        dc.warn(&format!(
            "Semantic runtime status failed validation ({error}); semantic influence is disabled while exact, lexical, and graph search remain healthy"
        ));
        return;
    }
    match &semantic.state {
        SemanticRuntimeStateV1::Unavailable { reason } => dc.info(&format!(
            "Semantic runtime unavailable ({reason:?}); exact, lexical, and graph search remain healthy offline"
        )),
        SemanticRuntimeStateV1::SelectedNotDownloaded {
            model_id,
            artifact_digest,
        } => {
            dc.info(&format!(
                "SelectedNotDownloaded: model {model_id} digest {artifact_digest}; exact/lexical/graph remain available (retry to download)"
            ));
        }
        SemanticRuntimeStateV1::Downloading {
            model_id,
            artifact_digest,
            bytes_received,
            bytes_total,
        } => dc.info(&format!(
            "Downloading: model {model_id} digest {artifact_digest} ({bytes_received}/{bytes_total}); semantics omitted; exact/lexical/graph remain available"
        )),
        SemanticRuntimeStateV1::Verifying {
            model_id,
            artifact_digest,
        } => dc.info(&format!(
            "Verifying: model {model_id} digest {artifact_digest}; semantics omitted; exact/lexical/graph remain available"
        )),
        SemanticRuntimeStateV1::Installed {
            model_id,
            artifact_digest,
        } => dc.pass(&format!(
            "Installed: model {model_id} digest {artifact_digest}; load/index pending; exact/lexical/graph remain available"
        )),
        SemanticRuntimeStateV1::Loading {
            model_id,
            artifact_digest,
        } => dc.info(&format!(
            "Loading: model {model_id} digest {artifact_digest}; semantics omitted; exact/lexical/graph remain available"
        )),
        SemanticRuntimeStateV1::Indexing {
            target_generation,
            completed_units,
            total_units,
        } => dc.pass(&format!(
            "Indexing: generation {target_generation:?} ({completed_units}/{total_units}); exact/lexical/graph remain available"
        )),
        SemanticRuntimeStateV1::Current { receipt } => dc.pass(&format!(
            "Ready: semantic generation {:?} is atomically current and may influence search",
            receipt.activated_generation
        )),
        SemanticRuntimeStateV1::Degraded {
            active_generation,
            reason,
        } => dc.warn(&format!(
            "Semantic runtime degraded ({reason:?}, prior generation {active_generation:?} omitted); exact, lexical, and graph search remain healthy"
        )),
        SemanticRuntimeStateV1::Rollback {
            from_generation,
            target_generation,
        } => dc.info(&format!(
            "Semantic rollback in progress ({from_generation:?} -> {target_generation:?}); semantic influence is omitted while exact, lexical, and graph search remain healthy"
        )),
        SemanticRuntimeStateV1::Failed {
            model_id,
            artifact_digest,
            detail,
            retryable,
        } => {
            dc.warn(&format!(
                "Failed: model {model_id} digest {artifact_digest} ({detail}); exact/lexical/graph remain available"
            ));
            if *retryable {
                dc.info("Remediation: retry download, or remove/rollback the installed semantic model");
            } else {
                dc.info("Remediation: remove or rollback the installed semantic model");
            }
        }
    }
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
    let graph_parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let data_root = if graph_parent.file_name() == Some(std::ffi::OsStr::new("branches")) {
        graph_parent.parent().unwrap_or(graph_parent)
    } else {
        graph_parent
    };
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
         Facts are stored in the graph database; automatic default-store rebuild is intentionally blocked because it cannot preserve them generically.\n\
         Derived branch indexes are preserved under `recovery/` and rebuilt automatically from a healthy tracked ancestor.\n\
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

fn database_recovery_guidance_for_problem(db_path: &Path, problem: &str) -> String {
    if !crate::tracedecay::is_fts_only_corruption(problem) {
        return database_recovery_guidance(db_path);
    }

    format!(
        "The failure is confined to the derived `nodes_fts` index at {}; the authoritative `nodes` table and graph-resident facts must be preserved.\n\
         Do not run `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe`, and do not delete the database.\n\
         Once no sync is active, run `tracedecay daemon restart` with the retained or a newer compatible binary. Its sole-writer open path will rebuild it from the authoritative `nodes` table before serving requests.\n\
         Then rerun `tracedecay tool runtime` and `tracedecay doctor`; if quick_check still fails, preserve the DB/WAL/SHM/dirty recovery set and follow the offline recovery guidance.",
        db_path.display(),
    )
}

fn print_database_recovery_guidance(dc: &DoctorCounters, db_path: &Path) {
    for line in database_recovery_guidance(db_path).lines() {
        dc.info(line);
    }
}

fn print_database_recovery_guidance_for_problem(
    dc: &DoctorCounters,
    db_path: &Path,
    problem: &str,
) {
    for line in database_recovery_guidance_for_problem(db_path, problem).lines() {
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
    dc.pass(&format!("Version: {}", crate::version::build_version()));
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
    global_db: &crate::global_db::RegisteredGlobalDb,
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
    global_db: &crate::global_db::RegisteredGlobalDb,
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

/// Project-local domain symbol rules file described by
/// `docs/DOMAIN-EXTRACTORS.md`.
const DOMAIN_SYMBOL_RULES_FILENAME: &str = "domain-symbols.toml";

/// Builds the warning for a domain symbol rules file that nothing reads.
///
/// `docs/DOMAIN-EXTRACTORS.md` documents `.tracedecay/domain-symbols.toml` as a
/// design rather than a shipped feature: no extractor parses it. Without this
/// check, authoring one is a silent no-op — no error, no warning, and no domain
/// nodes — so Doctor is where the author finds out. `None` (the normal case)
/// keeps Doctor silent about a file that is not there.
fn domain_symbol_rules_warning(project_path: &Path) -> Option<String> {
    let rules = crate::config::get_tracedecay_dir(project_path).join(DOMAIN_SYMBOL_RULES_FILENAME);
    rules.is_file().then(|| {
        format!(
            "Domain symbol rules at {} are not read: domain symbol extraction is \
             unimplemented, so this file contributes no graph nodes. \
             See docs/DOMAIN-EXTRACTORS.md, which describes the design only.",
            rules.display()
        )
    })
}

/// Check for project configuration that `TraceDecay` does not act on.
fn check_inert_project_config(dc: &mut DoctorCounters, project_path: &Path) {
    if let Some(warning) = domain_symbol_rules_warning(project_path) {
        dc.warn(&warning);
    }
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

const LANGUAGE_ANALYZER_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageAnalyzerSpec {
    command: String,
    probe_command: String,
    version_args: &'static [&'static str],
    languages: Vec<String>,
    remedy: Option<String>,
    rustup_component: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LanguageAnalyzerProbe {
    Present { version: String },
    Missing,
    RustupComponentMissing,
    Broken { detail: String },
    TimedOut,
}

fn configured_language_analyzers(settings: &CodeDiagnosticsSettings) -> Vec<LanguageAnalyzerSpec> {
    let mut adapters = builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    let mut analyzers = Vec::<LanguageAnalyzerSpec>::new();

    for adapter in adapters {
        if !settings.language_enabled(&adapter.language) {
            continue;
        }
        let command = settings.command_for(&adapter.language, &adapter.command);
        if let Some(existing) = analyzers
            .iter_mut()
            .find(|analyzer| analyzer.command == command)
        {
            if !existing.languages.contains(&adapter.language) {
                existing.languages.push(adapter.language);
            }
            continue;
        }
        let remedy = adapter
            .install_options
            .first()
            .map(|option| option.command.clone());
        let (probe_command, version_args) = analyzer_version_probe(&adapter.command);
        analyzers.push(LanguageAnalyzerSpec {
            probe_command: probe_command.unwrap_or(&command).to_string(),
            version_args,
            rustup_component: adapter.command == "rust-analyzer",
            command,
            languages: vec![adapter.language],
            remedy,
        });
    }

    analyzers
}

fn analyzer_version_probe(command: &str) -> (Option<&'static str>, &'static [&'static str]) {
    match command {
        "gopls" => (None, &["version"]),
        "intelephense" => (
            Some("npm"),
            &["list", "--global", "--depth=0", "intelephense"],
        ),
        "pyright-langserver" => (Some("pyright"), &["--version"]),
        _ => (None, &["--version"]),
    }
}

async fn check_language_analyzers(dc: &mut DoctorCounters, project_path: &Path) {
    eprintln!("\n\x1b[1mLanguage analyzers\x1b[0m");
    let settings = match language_analyzer_settings(project_path).await {
        Ok(settings) => settings,
        Err(error) => {
            dc.warn(&format!(
                "Code diagnostics settings could not be loaded: {error}; probing built-in analyzers only"
            ));
            CodeDiagnosticsSettings::default()
        }
    };
    dc.info(
        "Project-active language evidence is not available to this Doctor path; enabled analyzers are graded as warnings when unavailable.",
    );

    for analyzer in configured_language_analyzers(&settings) {
        let languages = analyzer.languages.join(", ");
        match probe_language_analyzer_with_path(
            &analyzer.command,
            &analyzer.probe_command,
            analyzer.version_args,
            analyzer.rustup_component,
            None,
        )
        .await
        {
            LanguageAnalyzerProbe::Present { version } => {
                dc.pass(&format!(
                    "{} ({languages}) is executable: {version}",
                    analyzer.command
                ));
            }
            LanguageAnalyzerProbe::Missing => {
                dc.warn(&analyzer_warning(
                    &analyzer,
                    &languages,
                    "was not found on PATH",
                ));
            }
            LanguageAnalyzerProbe::RustupComponentMissing => {
                dc.warn(
                    "rust-analyzer resolves to a rustup shim, but the active toolchain does not have the rust-analyzer component installed; LSP context projection for rust is unavailable. Run `rustup component add rust-analyzer`.",
                );
            }
            LanguageAnalyzerProbe::Broken { detail } => {
                dc.warn(&analyzer_warning(
                    &analyzer,
                    &languages,
                    &format!("resolved, but its version probe failed: {detail}"),
                ));
            }
            LanguageAnalyzerProbe::TimedOut => {
                dc.warn(&analyzer_warning(
                    &analyzer,
                    &languages,
                    &format!(
                        "resolved, but its version probe timed out after {}s",
                        LANGUAGE_ANALYZER_PROBE_TIMEOUT.as_secs()
                    ),
                ));
            }
        }
    }
}

async fn language_analyzer_settings(
    project_path: &Path,
) -> crate::errors::Result<CodeDiagnosticsSettings> {
    let Some(layout) = TraceDecay::try_initialized_store_layout_with_options(
        project_path,
        &TraceDecayOpenOptions::default(),
    )
    .await?
    else {
        return Ok(CodeDiagnosticsSettings::default());
    };
    tracedecay_lsp::analyzer::settings::load_settings(&layout.dashboard_root)
        .await
        .map_err(Into::into)
}

fn analyzer_warning(analyzer: &LanguageAnalyzerSpec, languages: &str, problem: &str) -> String {
    let mut message = format!(
        "{} {problem}; LSP context projection for {languages} is unavailable.",
        analyzer.command
    );
    if let Some(remedy) = &analyzer.remedy {
        message.push_str(" Install with `");
        message.push_str(remedy);
        message.push_str("`.");
    } else {
        message.push_str(" No install remedy is configured for this custom adapter.");
    }
    message
}

async fn probe_language_analyzer_with_path(
    command: &str,
    probe_command: &str,
    version_args: &[&str],
    rustup_component: bool,
    path: Option<&OsStr>,
) -> LanguageAnalyzerProbe {
    match resolve_executable(command, path) {
        ExecutableResolution::Missing => return LanguageAnalyzerProbe::Missing,
        ExecutableResolution::NotExecutable(path) => {
            return LanguageAnalyzerProbe::Broken {
                detail: format!("{} is not executable", path.display()),
            };
        }
        ExecutableResolution::Executable => {}
    }

    let mut process = tokio::process::Command::new(probe_command);
    process.args(version_args).kill_on_drop(true);
    if let Some(path) = path {
        process.env("PATH", path);
    }
    let output = match tokio::time::timeout(LANGUAGE_ANALYZER_PROBE_TIMEOUT, process.output()).await
    {
        Err(_) => return LanguageAnalyzerProbe::TimedOut,
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => {
            return LanguageAnalyzerProbe::Broken {
                detail: format!("equivalent version probe `{probe_command}` was not found"),
            };
        }
        Ok(Err(error)) => {
            return LanguageAnalyzerProbe::Broken {
                detail: format!("could not execute: {error}"),
            };
        }
        Ok(Ok(output)) => output,
    };

    if output.status.success() {
        let version = command_output_detail(&output);
        return LanguageAnalyzerProbe::Present {
            version: if version.is_empty() {
                format!("version probe exited successfully ({})", output.status)
            } else {
                version
            },
        };
    }

    let detail = command_output_detail(&output);
    if rustup_component
        && rustup_missing_component_message(&detail)
        && !rustup_component_is_installed(path).await
    {
        return LanguageAnalyzerProbe::RustupComponentMissing;
    }
    LanguageAnalyzerProbe::Broken {
        detail: if detail.is_empty() {
            format!("version probe exited with {}", output.status)
        } else {
            format!("{}: {detail}", output.status)
        },
    }
}

enum ExecutableResolution {
    Executable,
    Missing,
    NotExecutable(PathBuf),
}

fn resolve_executable(command: &str, path: Option<&OsStr>) -> ExecutableResolution {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return classify_executable_path(command_path);
    }
    let paths = path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    let Some(paths) = paths else {
        return ExecutableResolution::Missing;
    };
    let mut not_executable = None;
    for directory in std::env::split_paths(&paths) {
        for candidate in executable_candidates(command) {
            match classify_executable_path(&directory.join(candidate)) {
                ExecutableResolution::Executable => {
                    return ExecutableResolution::Executable;
                }
                ExecutableResolution::NotExecutable(path) => {
                    not_executable.get_or_insert(path);
                }
                ExecutableResolution::Missing => {}
            }
        }
    }
    not_executable.map_or(
        ExecutableResolution::Missing,
        ExecutableResolution::NotExecutable,
    )
}

fn classify_executable_path(path: &Path) -> ExecutableResolution {
    let Ok(metadata) = std::fs::metadata(path) else {
        return ExecutableResolution::Missing;
    };
    if !metadata.is_file() || !permissions_are_executable(metadata.permissions()) {
        return ExecutableResolution::NotExecutable(path.to_path_buf());
    }
    ExecutableResolution::Executable
}

#[cfg(unix)]
fn permissions_are_executable(permissions: Permissions) -> bool {
    permissions.mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn permissions_are_executable(_permissions: std::fs::Permissions) -> bool {
    true
}

#[cfg(windows)]
fn executable_candidates(command: &str) -> Vec<String> {
    if Path::new(command).extension().is_some() {
        return vec![command.to_string()];
    }
    ["", ".exe", ".cmd", ".bat", ".com"]
        .into_iter()
        .map(|extension| format!("{command}{extension}"))
        .collect()
}

#[cfg(not(windows))]
fn executable_candidates(command: &str) -> Vec<String> {
    vec![command.to_string()]
}

fn rustup_missing_component_message(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("rust-analyzer")
        && (detail.contains("rustup component add rust-analyzer")
            || detail.contains("not installed for the toolchain"))
}

async fn rustup_component_is_installed(path: Option<&OsStr>) -> bool {
    let mut process = tokio::process::Command::new("rustup");
    process
        .args(["component", "list", "--installed"])
        .kill_on_drop(true);
    if let Some(path) = path {
        process.env("PATH", path);
    }
    let Ok(Ok(output)) =
        tokio::time::timeout(LANGUAGE_ANALYZER_PROBE_TIMEOUT, process.output()).await
    else {
        return true;
    };
    if !output.status.success() {
        return true;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim_start().starts_with("rust-analyzer"))
}

fn command_output_detail(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    detail.chars().take(500).collect()
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
