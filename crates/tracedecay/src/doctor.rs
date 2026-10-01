//! Doctor command: comprehensive health check of the tracedecay installation.
//!
//! Checks the binary, project index, global DB, user config, agent
//! integrations, and network connectivity.

use std::path::{Path, PathBuf};

use tracedecay_contracts::project_open::{
    ProjectOpenStatusReasonV1, ProjectOpenStatusStateV1, ProjectOpenStatusV1,
};
use tracedecay_contracts::storage::{
    SchemaConvergenceFindingV1, SchemaConvergenceProgressV1, SchemaConvergenceStateV1,
};
use tracedecay_contracts::{ApplicationOutcome, ConfigurationSettingFindingV1, ResolvedSetting};
use tracedecay_domain::configuration::{
    ConfigurationValueV1, INDEX_EXCLUDE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY, SettingKey,
    USER_UPLOAD_ENABLED_SETTING_KEY,
};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use tracedecay_agent_hosts::agents::{self, DoctorCounters, HealthcheckContext};
use tracedecay_application::advisory::github_runtime;
use tracedecay_automation_runtime::automation::effect_runtime::pending_automation_effect_resets_blocking;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{ConfigurationGetRequestV1, ConfigurationWireRequestV1};
use tracedecay_daemon_protocol::ApplicationSurfaceRequest;
use tracedecay_daemon_protocol::RequestedOutputFormat;
use tracedecay_daemon_service::application_surface::{
    execute_application_surface, resolve_application_surface_dispatch,
};
#[cfg(unix)]
use tracedecay_daemon_service::logging::recent_watcher_events;
use tracedecay_runtime_core::text::format_token_count;

/// Opens an isolated daemon-registered profile database so Doctor tests can
/// exercise the read-only session-temporal health adapter against the real
/// registered reader pool instead of an ad-hoc connection.
#[cfg(test)]
pub(crate) struct DoctorTestRuntime {
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    _registry: tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

#[cfg(test)]
impl DoctorTestRuntime {
    #[hotpath::skip]
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
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(profile_root)
            .expect("load Doctor test profile identity");
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(profile_root, nonce, label)
                .expect("enter Doctor test database scope");
        let registry = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
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

    pub(crate) fn database(&self) -> &tracedecay_global_db::RegisteredGlobalDb {
        self.database.as_ref()
    }
}

/// Sync cloud probes admitted by the CLI binary. Doctor never opens ureq
/// itself, the composition root cannot depend on the CLI crate.
#[derive(Clone, Copy)]
pub struct AdmittedDoctorNetworkProbes {
    pub fetch_worldwide_total: fn() -> Option<u64>,
    pub fetch_latest_version:
        fn() -> Result<String, tracedecay_dashboard_api::cloud::ReleaseLookupError>,
}

/// What a completed doctor run concluded; each variant has its own exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorCompletion {
    Healthy,
    /// The operator owes a step Doctor named: the reset of a store the daemon
    /// serves in its typed reset-required state, or a host's interactive
    /// activation.
    PendingOperatorAction,
    /// Doctor found this many issues; each issue line names its own remedy.
    Issues(u32),
}

/// Runs a comprehensive health check of the tracedecay installation.
///
/// The human report goes to stderr. With `emit_json`, stdout also carries one
/// JSON document with the same check lines and the daemon's canonical findings
/// in the `/api/doctor/findings` payload shape.
#[hotpath::measure(label = "doctor.run", future = true)]
pub async fn run_doctor(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    network: AdmittedDoctorNetworkProbes,
    emit_json: bool,
) -> tracedecay_domain::errors::Result<DoctorCompletion> {
    let _lifecycle_lease =
        match tracedecay_runtime_core::lifecycle_lease::acquire_shared_or_inherited(
            profile.data_dir(),
            "doctor",
        ) {
            Ok(lease) => lease,
            Err(error) => {
                eprintln!("tracedecay doctor could not start: {error}");
                return Err(error);
            }
        };
    let build_version = tracedecay_project::version::build_version()?;
    let mut dc = DoctorCounters::new();

    eprintln!("\n\x1b[1mtracedecay doctor v{build_version}\x1b[0m\n");

    check_binary(&mut dc, build_version);
    check_daemon_service(&mut dc, profile, build_version);
    let daemon_listening = tracedecay_daemon_control::daemon_socket_connectable(profile);
    let mut pending_reset = check_reset_required_stores(&mut dc, profile, build_version);

    eprintln!("\n\x1b[1mCurrent project\x1b[0m");
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    check_inert_project_config(&mut dc, &project_path);
    check_pr_autotrack_state(&mut dc, profile.data_dir(), &project_path);
    check_automation_effect_resets(&mut dc, profile.data_dir(), &project_path);
    let daemon_status = if daemon_listening {
        Some(daemon_project_status(profile, &project_path).await)
    } else {
        None
    };
    let daemon_findings = render_current_project_daemon_status(
        &mut dc,
        profile.data_dir(),
        &project_path,
        daemon_status.as_ref(),
        &mut pending_reset,
    )?;
    if matches!(daemon_status, Some(Ok(Some(_)))) {
        check_project_index_paths(&mut dc, profile, &project_path).await;
    }
    check_watcher(&mut dc, profile);
    let upload_enabled = if daemon_listening {
        configured_upload_enabled(profile)
            .await
            .map(UploadSetting::Resolved)
    } else {
        Ok(UploadSetting::DaemonUnavailable)
    };
    check_user_config(&mut dc, profile.data_dir(), upload_enabled.as_ref());
    check_external_tools(&mut dc);

    check_host_integrations(&mut dc, profile, &project_path);

    check_network(&mut dc, upload_enabled.as_ref(), network);
    print_summary(&dc);

    let completion = doctor_result(&dc, pending_reset);
    if emit_json {
        print_doctor_json(build_version, completion, &dc, daemon_findings)?;
    }
    Ok(completion)
}

/// The current project's section as the daemon answered it; `None` is the
/// typed `daemon_unavailable` state.
fn render_current_project_daemon_status(
    dc: &mut DoctorCounters,
    profile_root: &Path,
    project_path: &Path,
    daemon_status: Option<&tracedecay_domain::errors::Result<Option<serde_json::Value>>>,
    pending_reset: &mut bool,
) -> tracedecay_domain::errors::Result<DoctorDaemonFindingsV1> {
    Ok(match daemon_status {
        None => {
            dc.pending(DAEMON_UNAVAILABLE_STATEMENT);
            DoctorDaemonFindingsV1::DaemonUnavailable
        }
        Some(Ok(None)) => {
            // The daemon answered, so the sole owner is reachable; it simply
            // has not admitted this project far enough to publish storage
            // telemetry. That is a warming state, not a lost authority.
            dc.warn(&format!("{RUNTIME_TELEMETRY_PENDING} within {RUNTIME_TELEMETRY_WARMUP:?}; health remains unknown until the project is admitted"));
            DoctorDaemonFindingsV1::unread("daemon_storage_telemetry_pending")
        }
        Some(Ok(Some(status))) => render_daemon_status(dc, status)?,
        Some(Err(error)) => {
            if let Some((authority, reason)) = tracedecay_mcp::reset_required_context(error) {
                *pending_reset = true;
                dc.pending(&format!(
                    "Current project is not served: {authority} requires reset ({reason}). \
                     Pending operator action: run `{}`",
                    tracedecay_mcp::reset_required_command(&authority, Some(project_path))
                ));
                DoctorDaemonFindingsV1::unread("reset_required")
            } else {
                classify_daemon_status_error(dc, profile_root, project_path, error)
            }
        }
    })
}

/// Renders a daemon runtime answer: project open, schema convergence, and the
/// canonical findings projected by the same authority `/api/doctor/findings`
/// uses.
fn render_daemon_status(
    dc: &mut DoctorCounters,
    status: &serde_json::Value,
) -> tracedecay_domain::errors::Result<DoctorDaemonFindingsV1> {
    render_project_open_status(dc, status)?;
    let schema_convergences = render_schema_convergences(dc, status)?;
    Ok(match canonical_daemon_doctor_report(status)? {
        CanonicalDoctorReport::Observed(report) => {
            let read =
                tracedecay_dashboard_api::doctor_findings(&report, schema_convergences, None);
            render_doctor_findings(dc, &read.payload);
            DoctorDaemonFindingsV1::Observed(Box::new(ObservedDoctorFindingsV1 {
                domain_state: read.presentation.domain_state,
                coverage: read.presentation.coverage,
                freshness: read.presentation.freshness,
                payload: read.payload,
            }))
        }
        CanonicalDoctorReport::Mounting => {
            dc.warn(&format!(
                "Canonical Doctor report is pending: {PROJECT_RUNTIME_MOUNTING}"
            ));
            DoctorDaemonFindingsV1::Mounting
        }
        CanonicalDoctorReport::Unavailable => {
            dc.warn("Canonical Doctor report is unavailable; health remains unknown");
            DoctorDaemonFindingsV1::unread(CANONICAL_DOCTOR_REPORT_UNAVAILABLE)
        }
    })
}

fn print_doctor_json(
    build_version: &str,
    completion: DoctorCompletion,
    dc: &DoctorCounters,
    daemon_findings: DoctorDaemonFindingsV1,
) -> tracedecay_domain::errors::Result<()> {
    let document = DoctorJsonReportV1 {
        version: build_version,
        outcome: match completion {
            DoctorCompletion::Healthy => DoctorOutcomeV1::Healthy,
            DoctorCompletion::PendingOperatorAction => DoctorOutcomeV1::PendingOperatorAction,
            DoctorCompletion::Issues(_) => DoctorOutcomeV1::Issue,
        },
        issues: dc.issues,
        warnings: dc.warnings,
        pending_actions: dc.pending_actions,
        daemon_findings,
        checks: &dc.checks,
    };
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}

/// The machine-readable `tracedecay doctor --json` document.
#[derive(serde::Serialize)]
struct DoctorJsonReportV1<'a> {
    version: &'a str,
    outcome: DoctorOutcomeV1,
    issues: u32,
    warnings: u32,
    pending_actions: u32,
    daemon_findings: DoctorDaemonFindingsV1,
    checks: &'a [agents::DoctorCheckV1],
}

/// The exit-code table: `healthy` exits 0, `issue` 1, and
/// `pending_operator_action` 75.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorOutcomeV1 {
    Healthy,
    Issue,
    PendingOperatorAction,
}

/// What Doctor read from the daemon's canonical Doctor authority. Only the
/// rendered findings grade the exit code; this state is reported, never
/// counted again.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum DoctorDaemonFindingsV1 {
    /// The daemon's canonical report, projected exactly as
    /// `/api/doctor/findings` projects it.
    Observed(Box<ObservedDoctorFindingsV1>),
    /// No daemon listens for this profile; only binary-local checks ran.
    DaemonUnavailable,
    /// The project runtime that owns the report is still mounting
    /// (`application.runtime.mounting`); wait, then re-run.
    Mounting,
    /// A daemon answered, but not with an observed report.
    Unread { reason: &'static str },
}

impl DoctorDaemonFindingsV1 {
    fn unread(reason: &'static str) -> Self {
        Self::Unread { reason }
    }
}

const CANONICAL_DOCTOR_REPORT_UNAVAILABLE: &str = "canonical_doctor_report_unavailable";

#[derive(Debug, serde::Serialize)]
struct ObservedDoctorFindingsV1 {
    domain_state: tracedecay_api::read_model::DashboardDomainStateV1,
    coverage: tracedecay_api::read_model::DashboardCoverageV1,
    freshness: tracedecay_api::read_model::DashboardFreshnessV1,
    payload: tracedecay_dashboard_api::DoctorFindingsPayloadV1,
}

/// Typed state for a profile with no listening daemon.
const DAEMON_UNAVAILABLE: &str = "daemon_unavailable";
const DAEMON_UNAVAILABLE_STATEMENT: &str = "daemon_unavailable: no TraceDecay daemon is listening \
     for this profile, so the daemon's canonical Doctor findings were not read and only \
     binary-local checks ran. Pending operator action: start the daemon (`tracedecay daemon \
     start` for the managed service, or `tracedecay daemon run`), then re-run `tracedecay doctor`";

/// Lists every registered store the daemon serves in its typed
/// reset-required state, each with the exact reset command. Returns whether
/// any reset is pending.
fn check_reset_required_stores(
    dc: &mut DoctorCounters,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    build_version: &str,
) -> bool {
    if !tracedecay_daemon_control::daemon_reachable(profile) {
        return false;
    }
    match tracedecay_daemon_control::daemon_reset_required_stores(profile, build_version) {
        Ok(stores) => {
            for store in &stores {
                dc.pending(&format!(
                    "Store {} requires reset ({}). Pending operator action: run `{}`",
                    store.store, store.reason, store.remedy
                ));
            }
            !stores.is_empty()
        }
        Err(error) => {
            dc.warn(&format!(
                "Daemon reset-required stores could not be read: {error}"
            ));
            false
        }
    }
}

fn render_project_open_status(
    dc: &mut DoctorCounters,
    status: &serde_json::Value,
) -> tracedecay_domain::errors::Result<()> {
    let Some(value) = status.get("project_open").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let project_open: ProjectOpenStatusV1 = serde_json::from_value(value.clone())?;
    let message = format!(
        "Project open: {:?} ({:?}){}{}",
        project_open.state,
        project_open.reason,
        project_open
            .retry_after_ms
            .map_or_else(String::new, |delay| format!(", retry after {delay} ms")),
        project_open
            .detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(": {detail}")),
    );
    if project_open.state == ProjectOpenStatusStateV1::Completed {
        dc.pass(&message);
    } else if project_open.reason == ProjectOpenStatusReasonV1::UnrepairableVerdict {
        dc.fail(&message);
    } else {
        dc.warn(&message);
    }
    Ok(())
}

fn render_schema_convergences(
    dc: &mut DoctorCounters,
    status: &serde_json::Value,
) -> tracedecay_domain::errors::Result<Vec<SchemaConvergenceFindingV1>> {
    let Some(value) = status.pointer("/doctor_report/schema_convergences") else {
        return Ok(Vec::new());
    };
    let findings: Vec<SchemaConvergenceFindingV1> = serde_json::from_value(value.clone())?;
    for finding in &findings {
        let progress = match finding.progress {
            Some(SchemaConvergenceProgressV1::Rows { done, remaining }) => {
                format!(", rows {done} done / {remaining} remaining")
            }
            Some(SchemaConvergenceProgressV1::Pages { done, remaining }) => {
                format!(", pages {done} done / {remaining} remaining")
            }
            None => String::new(),
        };
        let message = format!(
            "Schema convergence: {} {} {}{progress}, started at {}",
            finding.store.as_str(),
            finding.stage.as_str(),
            finding.state.as_str(),
            finding.started_at_micros
        );
        match finding.state {
            SchemaConvergenceStateV1::Completed => dc.pass(&message),
            SchemaConvergenceStateV1::Degraded => match &finding.degraded_row {
                Some(row) => dc.fail(&format!("{message}: {row}")),
                None => dc.fail(&message),
            },
            SchemaConvergenceStateV1::PendingSchemaMigration
            | SchemaConvergenceStateV1::ReleasedShapeConvergenceInProgress => dc.warn(&message),
        }
    }
    Ok(findings)
}

/// Renders the same projected findings `/api/doctor/findings` serves.
fn render_doctor_findings(
    dc: &mut DoctorCounters,
    payload: &tracedecay_dashboard_api::DoctorFindingsPayloadV1,
) {
    eprintln!("\n\x1b[1mCanonical Doctor findings\x1b[0m");
    for entry in &payload.entries {
        render_doctor_finding(dc, entry.finding());
    }
    dc.info(&payload.note);
}

fn render_doctor_finding(
    dc: &mut DoctorCounters,
    finding: &tracedecay_contracts::doctor::DoctorFindingV1,
) {
    use tracedecay_contracts::doctor::DoctorEvidenceStateV1 as State;

    let evidence = finding
        .evidence()
        .first()
        .map_or("doctor.evidence.unavailable", |evidence| {
            evidence.reference().as_str()
        });
    let message = format!(
        "{}: {} ({evidence})",
        tracedecay_contracts::doctor::doctor_finding_family_label(finding.family()),
        finding.coverage().statement()
    );
    match finding.state() {
        State::HealthyCompleteCoverage => dc.pass(&message),
        State::Degraded => dc.fail(&message),
        // Nothing there to grade (an optional capability, host, or analyzer
        // that is not installed or configured): reported, counted nowhere.
        State::Absent => dc.info(&message),
        State::Unsupported | State::Stale | State::Partial | State::Unknown | State::Denied => {
            dc.warn(&message);
        }
    }
}

/// The wait remedy for a check whose answer is owned by a project runtime that
/// has not finished mounting since the daemon started.
const PROJECT_RUNTIME_MOUNTING: &str = "the project runtime is still mounting \
     (application.runtime.mounting); wait for it to finish, then re-run `tracedecay doctor`";

/// The daemon's canonical Doctor report for the current project.
#[derive(Debug, PartialEq)]
enum CanonicalDoctorReport {
    Observed(Box<tracedecay_contracts::doctor::DoctorReportV1>),
    /// The project runtime that owns the report is still mounting.
    Mounting,
    Unavailable,
}

fn canonical_daemon_doctor_report(
    status: &serde_json::Value,
) -> tracedecay_domain::errors::Result<CanonicalDoctorReport> {
    let Some(doctor_report) = status.get("doctor_report") else {
        return Ok(CanonicalDoctorReport::Unavailable);
    };
    match doctor_report
        .get("kind")
        .and_then(serde_json::Value::as_str)
    {
        Some("observed") => {}
        Some("unknown")
            if doctor_report
                .get("reason")
                .and_then(serde_json::Value::as_str)
                == Some(crate::daemon::DOCTOR_REPORT_OWNER_WARMING_REASON) =>
        {
            return Ok(CanonicalDoctorReport::Mounting);
        }
        Some("unknown" | "unsupported") => return Ok(CanonicalDoctorReport::Unavailable),
        Some(kind) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("daemon canonical Doctor report has unknown typed state: {kind}"),
            });
        }
        None => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon canonical Doctor report omitted its typed state".to_string(),
            });
        }
    }
    let report = doctor_report.get("report").cloned().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "observed daemon Doctor response omitted its report".to_string(),
        }
    })?;
    serde_json::from_value(report)
        .map(|report| CanonicalDoctorReport::Observed(Box::new(report)))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon canonical Doctor report violated its wire contract: {error}"),
        })
}

/// Gates the doctor exit code on the counted check lines, the canonical
/// findings among them.
///
/// An issue, including a degraded canonical finding, is something the
/// operator must fix. A diagnostic that could not run is a warning, never
/// laundered into an issue. With no issue, a pending reset or any other
/// operator step is the operator's action, not health.
fn doctor_result(dc: &DoctorCounters, pending_reset: bool) -> DoctorCompletion {
    if dc.issues > 0 {
        DoctorCompletion::Issues(dc.issues)
    } else if pending_reset || dc.pending_actions > 0 {
        DoctorCompletion::PendingOperatorAction
    } else {
        DoctorCompletion::Healthy
    }
}

#[hotpath::measure(label = "doctor.daemon_status", future = true)]
async fn daemon_project_status(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
) -> tracedecay_domain::errors::Result<Option<serde_json::Value>> {
    let handshake = crate::daemon::handshake_for_current_client(
        profile,
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let warmup_deadline = tokio::time::Instant::now() + RUNTIME_TELEMETRY_WARMUP;
    loop {
        // Diagnostic probe, not a liveness gate. A multi-gigabyte store
        // cold-opening while agents saturate the daemon can take well over 10s
        // for its first integrity read; a warm steady-state read returns in well
        // under a second. Give it headroom so a contended read reports real
        // status instead of failing the post-update with a spurious timeout.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        let result = crate::daemon::call_default_tool_within(
            profile,
            &handshake,
            "tracedecay_runtime",
            daemon_doctor_runtime_args(),
            deadline,
        )
        .await?;
        // The runtime reply carrying the full report exceeds one response
        // frame, so it arrives as a truncation envelope to reassemble.
        let runtime = crate::daemon::recover_truncated_tool_payload(
            profile,
            &handshake,
            "tracedecay_runtime",
            result,
            Some(deadline),
        )
        .await?;
        let warmed_up = tokio::time::Instant::now() >= warmup_deadline;
        match daemon_runtime_status(runtime)? {
            // The first Doctor call starts the project open, so a warming
            // owner is polled like missing telemetry until the report lands.
            Some(status)
                if warmed_up
                    || canonical_daemon_doctor_report(&status)?
                        != CanonicalDoctorReport::Mounting =>
            {
                return Ok(Some(status));
            }
            None if warmed_up => return Ok(None),
            Some(_) | None => tokio::time::sleep(RUNTIME_TELEMETRY_POLL).await,
        }
    }
}

/// The daemon owner's per-analyzer language-server read for the project at
/// `project_path`: the same read Doctor grades, resolved on the daemon's PATH.
/// `Ok(None)` is the warming state where the daemon answered but has not
/// published this project's telemetry yet.
pub async fn daemon_language_server_read(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
) -> tracedecay_domain::errors::Result<Option<tracedecay_contracts::doctor::LanguageServerReadV1>> {
    let Some(status) = daemon_project_status(profile, project_path).await? else {
        return Ok(None);
    };
    let read = status
        .pointer("/doctor_report/language_servers")
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon runtime response omitted the language-server read".to_string(),
        })?;
    serde_json::from_value(read.clone())
        .map(Some)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon language-server read violated its wire contract: {error}"),
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
/// How long Doctor keeps re-asking a reachable daemon for this project's
/// storage telemetry before reporting the warming state as unknown health.
const RUNTIME_TELEMETRY_WARMUP: std::time::Duration = std::time::Duration::from_secs(15);
const RUNTIME_TELEMETRY_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// The runtime reply once the daemon published this project's `database`
/// block; `Ok(None)` is the warming state before it has.
fn daemon_runtime_status(
    runtime: serde_json::Value,
) -> tracedecay_domain::errors::Result<Option<serde_json::Value>> {
    match runtime.get("database") {
        None => Ok(None),
        Some(database) if database.is_object() => Ok(Some(runtime)),
        Some(_) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon runtime database telemetry was not an object".to_string(),
        }),
    }
}

/// Warming and discovery-blocked refusals are not lost database authority.
///
/// The closed-connection / WAL recovery text belongs to a daemon that
/// disappeared while owning the store. A profile that is still warming, or a
/// repository walk blocked on one path, is a retryable state.
fn classify_daemon_status_error(
    dc: &mut DoctorCounters,
    profile_root: &Path,
    project_path: &Path,
    error: &tracedecay_domain::errors::TraceDecayError,
) -> DoctorDaemonFindingsV1 {
    if let Some(message) = daemon_warming_doctor_message(project_path, error) {
        dc.warn(&message);
        return DoctorDaemonFindingsV1::unread("daemon_warming");
    }
    if crate::daemon::error_is_project_not_enrolled(error) {
        report_project_not_enrolled(dc, project_path);
        return DoctorDaemonFindingsV1::unread("project_not_enrolled");
    }
    report_daemon_diagnostics_unavailable(
        dc,
        fallback_database_path(profile_root, project_path).as_deref(),
        error,
    );
    DoctorDaemonFindingsV1::unread(CANONICAL_DOCTOR_REPORT_UNAVAILABLE)
}

fn daemon_warming_doctor_message(
    project_path: &Path,
    error: &tracedecay_domain::errors::TraceDecayError,
) -> Option<String> {
    if crate::daemon::error_is_repository_discovery_deferred(error) {
        return Some(format!(
            "daemon is still warming: repository discovery blocked on {}",
            repository_discovery_block_path(error, project_path)
        ));
    }
    if crate::daemon::error_is_project_warming(error) {
        return Some("daemon is still warming: profile runtime is warming".to_owned());
    }
    None
}

fn repository_discovery_block_path(
    error: &tracedecay_domain::errors::TraceDecayError,
    project_path: &Path,
) -> String {
    let Some((_, _, detail)) = error.project_route_context() else {
        return project_path.display().to_string();
    };
    detail
        .split("repository discovery blocked on ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map_or_else(|| project_path.display().to_string(), str::to_owned)
}

fn report_project_not_enrolled(dc: &mut DoctorCounters, project_path: &Path) {
    dc.warn(&format!(
        "Current project is not enrolled in this profile ({}). Run `tracedecay init` to enroll it.",
        project_path.display()
    ));
}

fn report_daemon_diagnostics_unavailable(
    dc: &mut DoctorCounters,
    db_path: Option<&Path>,
    error: &tracedecay_domain::errors::TraceDecayError,
) {
    // The sole daemon owner is the only authority that can report storage
    // health, so losing it is an issue Doctor must exit non-zero on, not a
    // warning: nothing else in this run observed the store, and a zero exit
    // reads as "checked and fine" to every caller and CI gate.
    dc.fail(&format!(
        "Canonical Doctor report unavailable from the sole daemon owner: {error}. Health remains unknown; Doctor did not open SQLite."
    ));
    if let Some(path) = db_path {
        print_database_recovery_guidance(dc, path);
    } else {
        dc.info("The database path could not be resolved without opening registry SQLite; stop all TraceDecay processes and preserve the project store before repair.");
    }
}

fn fallback_database_path(profile_root: &Path, project_path: &Path) -> Option<PathBuf> {
    tracedecay_runtime_core::storage::resolve_persisted_layout(project_path, profile_root)
        .ok()
        .flatten()
        .map(|layout| layout.graph_db_path)
}

fn database_recovery_guidance(db_path: &Path) -> String {
    let wal_path = db_path.with_extension("db-wal");
    let shm_path = db_path.with_extension("db-shm");
    let data_root = db_path.parent().unwrap_or_else(|| Path::new("."));
    let mut graph_dirty = db_path.as_os_str().to_os_string();
    graph_dirty.push(".dirty");
    let graph_dirty = PathBuf::from(graph_dirty);
    let sessions_path = data_root.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);

    format!(
        "First stop all TraceDecay daemon and MCP processes. No files were changed.\n\
         Preserve this recovery set together before any repair:\n\
         DB: {}\n\
         WAL: {}\n\
         SHM: {}\n\
         graph dirty sentinel: {}\n\
         `sessions.db` is separate and must not be removed: {}\n\
         Facts are stored in the graph database; automatic default-store rebuild is intentionally blocked because it cannot preserve them generically.\n\
         Do not run `tracedecay init`, `tracedecay sync`, or `tracedecay wipe` until that recovery set is safely copied.\n\
         Report the preserved set at https://github.com/ScriptedAlchemy/tracedecay/issues for offline recovery.",
        db_path.display(),
        wal_path.display(),
        shm_path.display(),
        graph_dirty.display(),
        sessions_path.display(),
    )
}

fn print_database_recovery_guidance(dc: &mut DoctorCounters, db_path: &Path) {
    for line in database_recovery_guidance(db_path).lines() {
        dc.info(line);
    }
}

/// Diagnose the managed user-service unit, then prove a running unit by
/// initialize, not by `systemctl is-active` or a connectable socket.
///
/// A stopped or disabled unit is visible without treating it as permission to
/// activate the service; it may be an intentional operator hold.
fn check_daemon_service(
    dc: &mut DoctorCounters,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    build_version: &str,
) {
    eprintln!("\n\x1b[1mDaemon service\x1b[0m");
    let state = match tracedecay_daemon_control::installed_service_state(profile) {
        Ok(state) => state,
        Err(error) => {
            dc.warn(&format!("Daemon service state could not be read: {error}"));
            return;
        }
    };
    let proof =
        match tracedecay_daemon_control::installed_service_process_proof(profile, build_version) {
            Ok(proof) => proof,
            Err(error) => {
                dc.warn(&format!("Daemon process proof could not be read: {error}"));
                return;
            }
        };
    let message = daemon_service_doctor_message(state, &proof);
    match daemon_service_doctor_verdict(state, &proof) {
        DaemonServiceDoctorVerdict::Pass => dc.pass(&message),
        DaemonServiceDoctorVerdict::Warn => dc.warn(&message),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonServiceDoctorVerdict {
    Pass,
    Warn,
}

fn daemon_service_doctor_verdict(
    state: tracedecay_daemon_control::DaemonServiceState,
    proof: &tracedecay_daemon_control::DaemonProcessProofV1,
) -> DaemonServiceDoctorVerdict {
    match state {
        tracedecay_daemon_control::DaemonServiceState::RunningEnabled
            if proof.version_matches() =>
        {
            DaemonServiceDoctorVerdict::Pass
        }
        tracedecay_daemon_control::DaemonServiceState::Missing
        | tracedecay_daemon_control::DaemonServiceState::RunningEnabled
        | tracedecay_daemon_control::DaemonServiceState::RunningDisabled
        | tracedecay_daemon_control::DaemonServiceState::StoppedEnabled
        | tracedecay_daemon_control::DaemonServiceState::StoppedDisabled
        | tracedecay_daemon_control::DaemonServiceState::Masked => DaemonServiceDoctorVerdict::Warn,
    }
}

fn daemon_service_doctor_message(
    state: tracedecay_daemon_control::DaemonServiceState,
    proof: &tracedecay_daemon_control::DaemonProcessProofV1,
) -> String {
    if !matches!(
        state,
        tracedecay_daemon_control::DaemonServiceState::RunningEnabled
            | tracedecay_daemon_control::DaemonServiceState::RunningDisabled
    ) {
        return state.lifecycle_operator_advice();
    }
    match proof {
        tracedecay_daemon_control::DaemonProcessProofV1::Ready => state.lifecycle_operator_advice(),
        tracedecay_daemon_control::DaemonProcessProofV1::VersionMismatch { observed, expected } => {
            format!(
                "TraceDecay daemon unit is active, but initialize reported version {} not {expected}. Unit state is not proof this binary is serving.",
                observed.as_deref().unwrap_or("missing")
            )
        }
        tracedecay_daemon_control::DaemonProcessProofV1::Unproven { detail } => format!(
            "TraceDecay daemon unit is active, but the process did not answer initialize ({detail}). Unit state is not proof the daemon is serving."
        ),
    }
}

/// Check binary location and version.
fn check_binary(dc: &mut DoctorCounters, build_version: &str) {
    eprintln!("\x1b[1mBinary\x1b[0m");
    if let Ok(exe) = std::env::current_exe() {
        dc.pass(&format!("Binary: {}", exe.display()));
    } else {
        dc.fail("Could not determine binary path");
    }
    dc.pass(&format!("Version: {build_version}"));
}

/// Reports git-metadata watcher health (design D3/D5).
///
/// The watcher lives in the daemon; its per-project state is only in-process, so
/// this section sources telemetry the read-only way: recent `git_watch_*` events
/// from the daemon log (systemd journal on Linux, launchd err-log on macOS). It
/// reports whether an explicitly enabled project watcher is active or using
/// bounded scheduler reconciliation. Absent telemetry is reported as info, not
/// a failure, activation comes from each project's pinned configuration.
#[hotpath::measure(label = "doctor.check.watcher")]
fn check_watcher(dc: &mut DoctorCounters, profile: &tracedecay_runtime_core::config::ProfileRoot) {
    eprintln!("\n\x1b[1mWatcher\x1b[0m");

    if !tracedecay_daemon_control::daemon_reachable(profile) {
        dc.info("Daemon not running, watcher inactive; sync happens on hook/read events");
        return;
    }

    #[cfg(unix)]
    {
        let events = recent_watcher_events(profile.data_dir(), 2000);
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
                        ev.detail.map(|d| format!(", {d}")).unwrap_or_default()
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
/// check, authoring one is a silent no-op, no error, no warning, and no domain
/// nodes, so Doctor is where the author finds out. `None` (the normal case)
/// keeps Doctor silent about a file that is not there.
fn domain_symbol_rules_warning(project_path: &Path) -> Option<String> {
    let rules = tracedecay_runtime_core::config::get_tracedecay_dir(project_path)
        .join(DOMAIN_SYMBOL_RULES_FILENAME);
    rules.is_file().then(|| {
        format!(
            "Domain symbol extraction is unavailable: no extractor reads {}, \
             so this file contributes no graph nodes. \
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

/// Warnings name entries PR reconciliation will reset; the error names a
/// state file that blocks reconciliation outright.
fn pr_autotrack_state_findings(data_root: &Path) -> std::result::Result<Vec<String>, String> {
    match tracedecay_application::pr_tracking::load_state(data_root) {
        Ok(state) => Ok(state
            .stale
            .iter()
            .map(|stale| {
                format!(
                    "{stale}; PR auto-tracking reconciliation drops this entry and re-tracks the PR if it is still open"
                )
            })
            .collect()),
        Err(error) => Err(format!(
            "PR auto-tracking state {} is unreadable ({error}); reconciliation stays blocked until that file is removed, after which open PRs are re-tracked",
            tracedecay_application::pr_tracking::state_path(data_root).display()
        )),
    }
}

fn check_pr_autotrack_state(dc: &mut DoctorCounters, profile_root: &Path, project_path: &Path) {
    let Ok(Some(layout)) =
        tracedecay_runtime_core::storage::resolve_persisted_layout(project_path, profile_root)
    else {
        return;
    };
    match pr_autotrack_state_findings(&layout.data_root) {
        Ok(warnings) => {
            for warning in warnings {
                dc.warn(&warning);
            }
        }
        Err(failure) => dc.fail(&failure),
    }
}

/// One warning per refused automation effect journal, named by its run id,
/// that project-open recovery resets; the error names an unreadable journal
/// directory.
fn automation_effect_reset_findings(
    dashboard_root: &Path,
) -> std::result::Result<Vec<String>, String> {
    let resets = pending_automation_effect_resets_blocking(dashboard_root)
        .map_err(|error| format!("Automation effect journals could not be read: {error}"))?;
    Ok(resets
        .into_iter()
        .map(|reset| match reset.run_id {
            Some(run_id) => format!(
                "Automation run {} requires reset: its effect journal shape is refused ({}); project-open recovery removes the journal and the run id can run again",
                run_id.as_str(),
                reset.reason
            ),
            None => format!(
                "Automation effect journal {} requires reset: its shape is refused ({}); project-open recovery removes it",
                reset.journal.display(),
                reset.reason
            ),
        })
        .collect())
}

fn check_automation_effect_resets(
    dc: &mut DoctorCounters,
    profile_root: &Path,
    project_path: &Path,
) {
    let Ok(Some(layout)) =
        tracedecay_runtime_core::storage::resolve_persisted_layout(project_path, profile_root)
    else {
        return;
    };
    match automation_effect_reset_findings(&layout.dashboard_root) {
        Ok(warnings) => {
            for warning in warnings {
                dc.warn(&warning);
            }
        }
        Err(failure) => dc.fail(&failure),
    }
}

#[hotpath::measure(label = "doctor.config.upload", future = true)]
async fn configured_upload_enabled(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
) -> tracedecay_domain::errors::Result<bool> {
    // The setting belongs to the profile: the request names no project.
    let setting = configured_setting(profile, None, USER_UPLOAD_ENABLED_SETTING_KEY).await?;
    match setting.effective_value {
        ConfigurationValueV1::Boolean(enabled) => Ok(enabled),
        _ => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "worldwide counter upload setting is not boolean".to_owned(),
        }),
    }
}

/// Reports every stored index path pattern the project's runtime
/// configuration leaves unapplied, with the command that repairs it.
#[hotpath::measure(label = "doctor.config.index_paths", future = true)]
async fn check_project_index_paths(
    dc: &mut DoctorCounters,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
) {
    for key in [INDEX_EXCLUDE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY] {
        match configured_setting(profile, Some(project_path), key).await {
            Ok(setting) => {
                for finding in setting.findings {
                    match finding {
                        ConfigurationSettingFindingV1::InvalidIndexPathPattern {
                            pattern,
                            message,
                            ..
                        } => dc.fail(&format!(
                            "{key} stores pattern {pattern:?} that does not compile ({message}); \
                             indexing runs without it. Set {key} to patterns that compile or \
                             unset it (tracedecay_configuration_set / tracedecay_configuration_unset)"
                        )),
                    }
                }
            }
            // A reset-required or still-mounting store is already reported
            // as its own state; a read that could not run is not an issue.
            Err(error) => dc.warn(&format!("{key} could not be read: {error}")),
        }
    }
}

async fn configured_setting(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: Option<&Path>,
    key: &str,
) -> tracedecay_domain::errors::Result<ResolvedSetting> {
    let operation = ApplicationSurfaceOperation::ConfigurationGet;
    let key = SettingKey::new(key).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        }
    })?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::DaemonDoctor).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not create Doctor configuration request: {error}"),
            }
        })?;
    let handshake = crate::daemon::handshake_for_current_client(
        profile,
        project_path.map(Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let client = crate::daemon::invocation_client_for_current(profile, handshake)?;
    let dispatched = resolve_application_surface_dispatch(
        BindingSurface::Cli,
        operation,
        request_id.clone(),
        ApplicationSurfaceRequest::Configuration(ConfigurationWireRequestV1::Get(
            ConfigurationGetRequestV1 { key: key.clone() },
        )),
        RequestedOutputFormat::Json,
    )
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: error.to_string(),
    })?;
    let result = execute_application_surface(operation, dispatched, Some(&client))
        .await
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let envelope = match result.result {
        Ok(envelope) => envelope,
        Err(problem) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: problem.problem.summary(),
            });
        }
    };
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "configuration get returned a non-evidence outcome".to_owned(),
        });
    };
    let setting: ResolvedSetting = serde_json::from_value(evidence.payload.ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "configuration get omitted its payload".to_owned(),
        }
    })?)
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("configuration get returned an invalid setting: {error}"),
    })?;
    if setting.key != key {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "configuration get returned the wrong setting".to_owned(),
        });
    }
    Ok(setting)
}

/// The worldwide-counter upload setting as the profile's configuration owner
/// answered it.
#[derive(Debug, PartialEq, Eq)]
enum UploadSetting {
    Resolved(bool),
    /// No daemon listens for this profile, so no configuration owner answered.
    DaemonUnavailable,
}

/// Check canonical user configuration and pending upload state.
fn check_user_config(
    dc: &mut DoctorCounters,
    profile_root: &Path,
    upload_enabled: Result<&UploadSetting, &tracedecay_domain::errors::TraceDecayError>,
) {
    eprintln!("\n\x1b[1mUser config\x1b[0m");
    match upload_enabled {
        Ok(UploadSetting::Resolved(true)) => dc.pass("Worldwide counter upload enabled"),
        Ok(UploadSetting::Resolved(false)) => {
            dc.info("Worldwide counter upload disabled (default)");
        }
        Ok(UploadSetting::DaemonUnavailable) => {
            dc.info(&format!(
                "Worldwide counter upload setting unread: {DAEMON_UNAVAILABLE}"
            ));
        }
        Err(error) => dc.warn(&format!(
            "Worldwide counter upload setting unavailable from canonical configuration: {error}"
        )),
    }
    match tracedecay_session_memory::user_config::UserConfig::load(profile_root) {
        Ok(config) => {
            if config.pending_upload > 0 {
                dc.info(&format!("Pending upload: {} tokens", config.pending_upload));
            }
            if let Err(error) =
                github_runtime::check_configured_github_review_sources_v1(profile_root)
            {
                dc.fail(&format!(
                    "GitHub review sources are unusable, none is registered: {error}"
                ));
            }
        }
        Err(error) => dc.fail(&format!("Profile config is unusable: {error}")),
    }
}

/// Reports every host the profile tracks or finds integrated. A host that is
/// not installed is one skipped line, counted nowhere.
fn check_host_integrations(
    dc: &mut DoctorCounters,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
) {
    let Some(home) = profile.home() else {
        dc.fail("Could not determine home directory");
        return;
    };
    // Host integration health is read-only: every `healthcheck` only reads
    // the host's own on-disk registration and reports findings. Doctor
    // never repairs them, remediation stays with `tracedecay install`.
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        profile: profile.clone(),
        project_path: project_path.to_path_buf(),
    };
    let tracked = tracked_hosts(dc, profile.data_dir());
    for agent in agents::all_integrations() {
        let integrated = agent.has_tracedecay(home, profile);
        if !integrated && !tracked.iter().any(|id| id == agent.id()) {
            warn_detected_unintegrated_host(dc, agent.as_ref(), home, profile);
            continue;
        }
        let absence = match agent.require_host(home) {
            Ok(agents::HostPresence::HostCli) => None,
            Ok(agents::HostPresence::NoHostCli) if integrated || agent.is_detected(home) => None,
            Ok(agents::HostPresence::NoHostCli) => Some((
                tracedecay_domain::errors::HostAbsence::NotInstalled,
                format!("{} is not detected under {}", agent.name(), home.display()),
            )),
            Err(error) => match error.host_absence() {
                Some(absence) => Some((absence, error.to_string())),
                None => {
                    eprintln!("\n\x1b[1m{} integration\x1b[0m", agent.name());
                    dc.fail(&format!("{}: host CLI is unusable: {error}", agent.id()));
                    continue;
                }
            },
        };
        match absence {
            None => agent.healthcheck(dc, &hctx),
            Some((absence, detail)) => {
                eprintln!("\n\x1b[1m{} integration\x1b[0m", agent.name());
                dc.skipped(&format!(
                    "{}: skipped, {} ({detail})",
                    agent.id(),
                    absence.reason()
                ));
            }
        }
    }
}

/// A host that is on this machine but carries no tracedecay integration,
/// which an operator most likely wants wired up; an absent one says nothing.
fn warn_detected_unintegrated_host(
    dc: &mut DoctorCounters,
    agent: &dyn agents::AgentIntegration,
    home: &Path,
    profile: &tracedecay_runtime_core::config::ProfileRoot,
) {
    let surface = agent.detected_host_surface(home, profile);
    if surface.is_none() && !agent.is_detected(home) {
        return;
    }
    eprintln!("\n\x1b[1m{} integration\x1b[0m", agent.name());
    dc.warn(&format!(
        "{} detected{} but tracedecay is not integrated, run `tracedecay install --agent {}`",
        agent.name(),
        surface
            .map(|surface| format!(" ({})", surface.display()))
            .unwrap_or_default(),
        agent.id()
    ));
}

/// The hosts the profile tracks; an unreadable profile config is a warning,
/// with no host treated as tracked.
fn tracked_hosts(dc: &mut DoctorCounters, profile_root: &Path) -> Vec<String> {
    match tracedecay_session_memory::user_config::UserConfig::load(profile_root) {
        Ok(config) => config.installed_agents,
        Err(error) => {
            dc.warn(&format!("Tracked hosts are unknown: {error}"));
            Vec::new()
        }
    }
}

/// Check optional external tools that gate optional MCP capabilities.
#[hotpath::measure(label = "doctor.check.external_tools")]
fn check_external_tools(dc: &mut DoctorCounters) {
    eprintln!("\n\x1b[1mExternal tools\x1b[0m");
    let diagnostics = tracedecay_mcp::ast_grep_diagnostics_json();
    let installed = json_bool(&diagnostics, "installed");
    let rewrite_available = json_bool(&diagnostics, "rewrite_available");
    let version = diagnostics
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let message = diagnostics
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ast-grep status unavailable");

    if rewrite_available {
        dc.pass(&format!("ast-grep {version}: rewrite support available"));
        return;
    }

    if installed {
        dc.warn(&format!(
            "ast-grep {version}: optional ast-grep-backed tools are unavailable"
        ));
    } else {
        dc.warn("ast-grep not found on PATH; optional ast-grep-backed tools are hidden");
    }
    dc.info(message);
    dc.info("Install or update ast-grep, then rerun `tracedecay install` or `tracedecay update-plugin` if your agent integration caches tool metadata.");
}

fn json_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Check network connectivity.
#[hotpath::measure(label = "doctor.check.network")]
fn check_network(
    dc: &mut DoctorCounters,
    upload_enabled: Result<&UploadSetting, &tracedecay_domain::errors::TraceDecayError>,
    network: AdmittedDoctorNetworkProbes,
) {
    eprintln!("\n\x1b[1mNetwork\x1b[0m");
    match upload_enabled {
        Ok(UploadSetting::Resolved(true)) => {
            if let Some(total) = (network.fetch_worldwide_total)() {
                dc.pass(&format!(
                    "Worldwide counter reachable (total: {})",
                    format_token_count(total)
                ));
            } else {
                dc.warn("Worldwide counter unreachable (offline or timeout)");
            }
        }
        Ok(UploadSetting::Resolved(false)) => {
            dc.info("Worldwide counter skipped (upload disabled)");
        }
        Ok(UploadSetting::DaemonUnavailable) => {
            dc.info(&format!("Worldwide counter check skipped: {DAEMON_UNAVAILABLE}"));
        }
        Err(error) => dc.warn(&format!(
            "Worldwide counter check skipped because canonical configuration is unavailable: {error}"
        )),
    }
    match (network.fetch_latest_version)() {
        Ok(latest) => dc.pass(&format!("GitHub releases API reachable (latest v{latest})")),
        Err(error) => dc.warn(&format!("GitHub release lookup failed, {error}")),
    }
}

/// Print final summary.
fn print_summary(dc: &DoctorCounters) {
    eprintln!();
    if dc.issues == 0 && dc.warnings == 0 && dc.pending_actions == 0 {
        eprintln!("\x1b[32mAll checks passed.\x1b[0m");
    } else if dc.issues == 0 && dc.pending_actions > 0 {
        eprintln!(
            "\x1b[33m{} pending operator action(s), {} warning(s), no issues.\x1b[0m",
            dc.pending_actions, dc.warnings
        );
    } else if dc.issues == 0 {
        eprintln!("\x1b[33m{} warning(s), no issues.\x1b[0m", dc.warnings);
    } else {
        eprintln!(
            "\x1b[31m{} issue(s), {} warning(s).\x1b[0m",
            dc.issues, dc.warnings
        );
    }
    eprintln!();
}

#[cfg(test)]
mod tests;
