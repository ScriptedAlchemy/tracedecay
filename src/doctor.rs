//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::path::{Path, PathBuf};

use tracedecay_application::{ApplicationOutcome, ResolvedSetting};
use tracedecay_domain::configuration::{
    ConfigurationValueV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY,
};
use tracedecay_tool_catalog::BindingSurface;

use crate::agents::{self, DoctorCounters, HealthcheckContext};
use crate::application_surface::{
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, ConfigurationKeySurfaceRequest,
    ConfigurationSurfaceRequest, execute_application_surface, resolve_application_surface_dispatch,
};
use crate::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use crate::display::format_token_count;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

// Consumed by the unix-only daemon git-watch maintenance path; on other
// targets only the module's tests reference it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) mod registry_drift;

/// Opens an isolated daemon-registered profile database so Doctor tests can
/// exercise the read-only session-temporal health adapter against the real
/// registered reader pool instead of an ad-hoc connection.
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
        // Mount the profile SESSIONS store: every production caller of
        // `session_temporal_doctor_health` diagnoses a sessions store, which
        // is the mount that binds the session relation graph the doctor's
        // relation-health stage requires.
        let database = registry
            .profile_sessions()
            .await
            .expect("mount Doctor test profile session store");
        Self {
            database,
            _registry: registry,
            _scope: scope,
        }
    }

    pub(crate) fn database(&self) -> &crate::global_db::RegisteredGlobalDb {
        self.database.as_ref()
    }
}

/// Runs a comprehensive health check of the tracedecay installation.
pub async fn run_doctor() -> crate::errors::Result<()> {
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
        Ok(status) => match canonical_daemon_doctor_report(status)? {
            Some(report) => {
                let storage_health = database_health_from_canonical_report(&report);
                render_canonical_doctor_report(&mut dc, &report);
                storage_health
            }
            None => {
                dc.warn("Canonical Doctor report is unavailable; health remains unknown");
                DatabaseHealth::unknown("canonical_doctor_report_unavailable")
            }
        },
        Err(error) => {
            report_daemon_diagnostics_unavailable(
                &mut dc,
                fallback_database_path(&project_path).as_deref(),
                error,
            );
            DatabaseHealth::unknown("canonical_doctor_report_unavailable")
        }
    };
    check_watcher(&mut dc);
    let upload_enabled = configured_upload_enabled(&project_path).await;
    check_user_config(&mut dc, upload_enabled.as_ref());
    check_external_tools(&mut dc);

    if let Some(ref home) = agents::home_dir() {
        // Host integration health is read-only: every `healthcheck` only reads
        // the host's own on-disk registration and reports findings. Doctor
        // never repairs them — remediation stays with `tracedecay install`.
        let hctx = HealthcheckContext {
            home: home.clone(),
            project_path: project_path.clone(),
        };
        for agent in agents::all_integrations() {
            if agent.has_tracedecay(home) {
                agent.healthcheck_with_daemon_status(&mut dc, &hctx, daemon_status.as_ref().ok());
            }
        }
        let materialization_root =
            crate::automation::skill_materialization::resolve_project_root(&project_path);
        check_managed_skill_materialization(&mut dc, home, &materialization_root);
    } else {
        dc.fail("Could not determine home directory");
    }

    check_network(&mut dc, upload_enabled.as_ref());
    print_summary(&dc);

    doctor_result(&dc, &storage_health)
}

fn render_canonical_doctor_report(
    dc: &mut DoctorCounters,
    report: &tracedecay_application::doctor::DoctorReportV1,
) {
    eprintln!("\n\x1b[1mCanonical Doctor findings\x1b[0m");
    for finding in report.findings() {
        render_doctor_finding(dc, finding);
    }
    dc.info(report.coverage().statement().statement());
}

fn render_doctor_finding(
    dc: &mut DoctorCounters,
    finding: &tracedecay_application::doctor::DoctorFindingV1,
) {
    use tracedecay_application::doctor::DoctorEvidenceStateV1 as State;

    let evidence = finding
        .evidence()
        .first()
        .map_or("doctor.evidence.unavailable", |evidence| {
            evidence.reference().as_str()
        });
    let message = format!(
        "{:?}: {} ({evidence})",
        finding.family(),
        finding.coverage().statement()
    );
    match finding.state() {
        State::HealthyCompleteCoverage => dc.pass(&message),
        State::Degraded => dc.fail(&message),
        State::Unsupported
        | State::Absent
        | State::Stale
        | State::Partial
        | State::Unknown
        | State::Denied => dc.warn(&message),
    }
}

fn canonical_daemon_doctor_report(
    status: &serde_json::Value,
) -> crate::errors::Result<Option<tracedecay_application::doctor::DoctorReportV1>> {
    let Some(doctor_report) = status.get("doctor_report") else {
        return Ok(None);
    };
    match doctor_report
        .get("kind")
        .and_then(serde_json::Value::as_str)
    {
        Some("observed") => {}
        Some("unknown" | "unsupported") => return Ok(None),
        Some(kind) => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!("daemon canonical Doctor report has unknown typed state: {kind}"),
            });
        }
        None => {
            return Err(crate::errors::TraceDecayError::Config {
                message: "daemon canonical Doctor report omitted its typed state".to_string(),
            });
        }
    }
    let report = doctor_report.get("report").cloned().ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "observed daemon Doctor response omitted its report".to_string(),
        }
    })?;
    serde_json::from_value(report).map(Some).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("daemon canonical Doctor report violated its wire contract: {error}"),
        }
    })
}

/// Derive the exit-gating storage verdict from the canonical kernel findings.
///
/// A degraded `StorageRuntime` finding is an observed failure. Every other
/// non-healthy state is unknown evidence, and missing family evidence is also
/// unknown. Multiple findings retain the strongest state:
/// `Failed` > `Unknown` > `Healthy`.
fn database_health_from_canonical_report(
    report: &tracedecay_application::doctor::DoctorReportV1,
) -> DatabaseHealth {
    use tracedecay_application::doctor::DoctorFindingFamilyV1 as Family;

    database_health_from_storage_runtime_findings(
        report
            .findings()
            .filter(|finding| finding.family() == Family::StorageRuntime),
    )
}

fn database_health_from_storage_runtime_findings<'a>(
    findings: impl IntoIterator<Item = &'a tracedecay_application::doctor::DoctorFindingV1>,
) -> DatabaseHealth {
    use tracedecay_application::doctor::DoctorEvidenceStateV1 as State;

    let mut findings = findings.into_iter();
    let Some(first) = findings.next() else {
        return DatabaseHealth::unknown("canonical_storage_runtime_missing");
    };
    let health = |finding: &tracedecay_application::doctor::DoctorFindingV1| {
        let evidence = finding
            .evidence()
            .first()
            .map_or("canonical_storage_runtime_evidence_missing", |evidence| {
                evidence.reference().as_str()
            });
        match finding.state() {
            State::Degraded => DatabaseHealth::failed(evidence),
            State::HealthyCompleteCoverage => DatabaseHealth::Healthy,
            State::Unsupported
            | State::Absent
            | State::Stale
            | State::Partial
            | State::Unknown
            | State::Denied => DatabaseHealth::unknown(evidence),
        }
    };
    findings.fold(health(first), |combined, finding| {
        combined.merge(health(finding))
    })
}

/// Gates the doctor exit code.
///
/// Only an observed storage *failure* is fatal. `DatabaseHealth::Unknown` — a
/// diagnostic that could not run — is reported to the user but never laundered
/// into a healthy verdict nor turned into a hard failure.
fn doctor_result(
    dc: &DoctorCounters,
    storage_health: &DatabaseHealth,
) -> crate::errors::Result<()> {
    match storage_health {
        DatabaseHealth::Failed { reason } => Err(crate::errors::TraceDecayError::Config {
            message: format!("doctor storage health check failed [{reason}]"),
        }),
        DatabaseHealth::Healthy | DatabaseHealth::Unknown { .. } if dc.issues > 0 => {
            Err(crate::errors::TraceDecayError::Config {
                message: format!("doctor found {} issue(s)", dc.issues),
            })
        }
        DatabaseHealth::Healthy | DatabaseHealth::Unknown { .. } => Ok(()),
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
        crate::errors::TraceDecayError::ResetRequired { .. }
        | crate::errors::TraceDecayError::File { .. }
        | crate::errors::TraceDecayError::Parse { .. }
        | crate::errors::TraceDecayError::Database { .. }
        | crate::errors::TraceDecayError::Search { .. }
        | crate::errors::TraceDecayError::ProfileResetRequired { .. }
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
    if let Some(value) = runtime.get("doctor_report").cloned() {
        status["doctor_report"] = value;
    }
    Ok(status)
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

fn report_daemon_diagnostics_unavailable(
    dc: &mut DoctorCounters,
    db_path: Option<&Path>,
    error: &crate::errors::TraceDecayError,
) {
    dc.warn(&format!(
        "Canonical Doctor report unavailable from the sole daemon owner: {error}. Health remains unknown; Doctor did not open SQLite."
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
         Facts are stored in the graph database; automatic default-store rebuild is intentionally blocked because it cannot preserve them generically.\n\
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

/// Reports git-metadata watcher health (design D3/D5).
///
/// The watcher lives in the daemon; its per-project state is only in-process, so
/// this section sources telemetry the read-only way: recent `git_watch_*` events
/// from the daemon log (systemd journal on Linux, launchd err-log on macOS). It
/// reports whether an explicitly enabled project watcher is active or using
/// bounded scheduler reconciliation. Absent telemetry is reported as info, not
/// a failure — activation comes from each project's pinned configuration.
fn check_watcher(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mWatcher\x1b[0m");

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
                        "{project}: degraded (bounded scheduler-reconciliation fallback){}",
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

async fn configured_upload_enabled(project_path: &Path) -> crate::errors::Result<bool> {
    let operation = ApplicationSurfaceOperation::ConfigurationGet;
    let key = SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: error.to_string(),
        }
    })?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::DaemonDoctor).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("could not create Doctor configuration request: {error}"),
            }
        })?;
    let handshake = crate::daemon::DaemonHandshake::for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let client = DaemonInvocationClient::for_current(handshake)?;
    let dispatched = resolve_application_surface_dispatch(
        BindingSurface::Cli,
        operation,
        request_id.clone(),
        ApplicationSurfaceRequest::Configuration(ConfigurationSurfaceRequest::Get(
            ConfigurationKeySurfaceRequest { key: key.clone() },
        )),
        RequestedOutputFormat::Json,
    )
    .map_err(|error| crate::errors::TraceDecayError::Config {
        message: error.to_string(),
    })?;
    let result = execute_application_surface(operation, dispatched, Some(&client))
        .await
        .map_err(|error| crate::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let envelope = result
        .result
        .map_err(|problem| crate::errors::TraceDecayError::Config {
            message: format!("{}: {}", problem.problem.code, problem.problem.message),
        })?;
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        return Err(crate::errors::TraceDecayError::Config {
            message: "configuration get returned a non-evidence outcome".to_owned(),
        });
    };
    let setting: ResolvedSetting = serde_json::from_value(evidence.payload.ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "configuration get omitted its payload".to_owned(),
        }
    })?)
    .map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("configuration get returned an invalid setting: {error}"),
    })?;
    if setting.key != key {
        return Err(crate::errors::TraceDecayError::Config {
            message: "configuration get returned the wrong setting".to_owned(),
        });
    }
    match setting.effective_value {
        ConfigurationValueV1::Boolean(enabled) => Ok(enabled),
        _ => Err(crate::errors::TraceDecayError::Config {
            message: "worldwide counter upload setting is not boolean".to_owned(),
        }),
    }
}

/// Check canonical user configuration and pending upload state.
fn check_user_config(
    dc: &mut DoctorCounters,
    upload_enabled: Result<&bool, &crate::errors::TraceDecayError>,
) {
    eprintln!("\n\x1b[1mUser config\x1b[0m");
    match upload_enabled {
        Ok(true) => dc.pass("Worldwide counter upload enabled"),
        Ok(false) => dc.info("Worldwide counter upload disabled (default)"),
        Err(error) => dc.warn(&format!(
            "Worldwide counter upload setting unavailable from canonical configuration: {error}"
        )),
    }
    if let Some(config_path) = crate::user_config::config_path()
        && config_path.exists()
    {
        let config = crate::user_config::UserConfig::load();
        if config.pending_upload > 0 {
            dc.info(&format!("Pending upload: {} tokens", config.pending_upload));
        }
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
fn check_network(
    dc: &mut DoctorCounters,
    upload_enabled: Result<&bool, &crate::errors::TraceDecayError>,
) {
    eprintln!("\n\x1b[1mNetwork\x1b[0m");
    match upload_enabled {
        Ok(true) => {
            if let Some(total) = crate::cloud::fetch_worldwide_total() {
                dc.pass(&format!(
                    "Worldwide counter reachable (total: {})",
                    format_token_count(total)
                ));
            } else {
                dc.warn("Worldwide counter unreachable (offline or timeout)");
            }
        }
        Ok(false) => dc.info("Worldwide counter skipped (upload disabled)"),
        Err(error) => dc.warn(&format!(
            "Worldwide counter check skipped because canonical configuration is unavailable: {error}"
        )),
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
