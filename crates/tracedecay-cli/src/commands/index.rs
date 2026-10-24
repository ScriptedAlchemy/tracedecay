use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_project::project::TraceDecay;

use tracedecay_contracts::code_index_freshness::{
    CODE_INDEX_READINESS_WAIT_TIMED_OUT, CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
    CodeIndexBuildBlockedReasonV1, CodeIndexBuildProgressV1, CodeIndexReadinessWaitOutcomeV1,
    CodeIndexWorktreeFreshnessV1,
};
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retrieval::{
    AdminCliRegistryEmptyV1, AdminCliResultV1, AdminCliSurfaceRequestV1, AdminSyncAdmissionV1,
    AdminSyncResultV1, StatusCodeIndexFreshnessV1,
};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::daemon::admin_cli_result;

/// True when the profile registry has zero registered projects, i.e. the user
/// has not run `tracedecay init` anywhere yet. An unreadable registry is not
/// a fresh install: the offer to initialize is only made on a known-empty one.
async fn is_fresh_install(profile: &ProfileRoot) -> bool {
    matches!(
        admin_cli_result(profile, None, AdminCliSurfaceRequestV1::RegistryEmpty {}).await,
        Ok(AdminCliResultV1::RegistryEmpty(AdminCliRegistryEmptyV1 {
            empty: true
        }))
    )
}

/// When invoked with no subcommand, offer to create the index if none exists.
pub(crate) async fn handle_no_command(
    profile: &ProfileRoot,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = tracedecay_configuration::resolve_path(None);
    if TraceDecay::has_initialized_store_with_options(
        &project_path,
        &tracedecay_project::project::TraceDecayOpenOptions::for_profile(profile),
    )
    .await
    {
        // Already initialized, show help via clap
        let _ = <crate::cli::Cli as clap::CommandFactory>::command().print_help();
        eprintln!();
        return Ok(());
    }
    if is_fresh_install(profile).await {
        eprintln!("\x1b[1;36mWelcome to tracedecay!\x1b[0m");
        eprintln!(
            "Looks like a new installation. To get started, run \x1b[1mtracedecay init\x1b[0m \
             in your project root."
        );
        eprintln!();
    }
    // Bare `tracedecay` is ambiguous (help vs init) and creating a store here
    // is how phantom indexes used to appear. Present the next command and
    // return; do not read stdin on a terminal or a pipe.
    eprintln!(
        "No TraceDecay index found at '{}'. Skipping index creation (run `tracedecay init`).",
        project_path.display()
    );
    Ok(())
}

#[tracing::instrument(name = "cli.init.run", level = "trace", skip_all)]
pub(crate) async fn handle_init(
    profile: &ProfileRoot,
    path: Option<String>,
    adopt_project: Option<String>,
    fresh: bool,
    assume_yes: bool,
    wait: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = tracedecay_configuration::resolve_path(path);
    let profile_root = profile.data_dir().to_path_buf();
    if let Some(message) =
        tracedecay_global_db::ephemeral_root_rejection(&project_path, &profile_root)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config { message });
    }
    let adoption = moved_store_adoption_request(adopt_project, fresh, assume_yes)?;
    let mut handshake = tracedecay::daemon::handshake_for_current_client(
        profile,
        Some(project_path.clone()),
        None,
        false,
        true,
    )?;
    handshake.moved_store_adoption = adoption;
    let daemon_available = init_daemon_available(profile);

    let project_path_for_remedy = project_path.clone();
    handle_init_with_daemon_availability(profile, project_path, handshake, daemon_available, wait)
        .await
        .map_err(|error| annotate_reset_required_init_error(error, &project_path_for_remedy))
}

/// Whether a daemon is accepting connections for this profile.
///
/// A connectable endpoint is the whole precondition: `brokered_init` carries
/// its own 120 s bootstrap deadline, so a daemon that has not finished
/// answering initialize within the one-second reachability probe is still the
/// daemon this init must broker through. Requiring the identity proof here
/// refused cold starts on CPU-constrained hosts and told the operator to start
/// a daemon that was already running.
///
/// This resolves through the daemon-control authority on every platform rather
/// than a `cfg` split. The unix socket and the Windows loopback authority are
/// both behind `daemon_socket_connectable`, so assuming availability wherever
/// the transport differs would let init proceed on Windows without the
/// scheduler it then requires.
fn init_daemon_available(profile: &ProfileRoot) -> bool {
    tracedecay_daemon_control::daemon_socket_connectable(profile)
}

/// Maps explicit `tracedecay init` flags to the adoption request the daemon
/// honors. Only init escalates past `Never`: `--adopt-project` names the
/// project, bare `--yes` confirms a unique candidate, `--fresh` opts out of
/// adoption entirely, and no flags means candidates are offered in a typed
/// refusal instead of silently remapped.
fn moved_store_adoption_request(
    adopt_project: Option<String>,
    fresh: bool,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<tracedecay_project::project::MovedStoreAdoption> {
    if fresh && adopt_project.is_some() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "--fresh mints a new project identity and contradicts --adopt-project; \
                      pass exactly one"
                .to_owned(),
        });
    }
    Ok(match (adopt_project, fresh, assume_yes) {
        (Some(project_id), _, _) => {
            tracedecay_project::project::MovedStoreAdoption::AdoptNamed(project_id)
        }
        (None, true, _) => tracedecay_project::project::MovedStoreAdoption::Never,
        (None, false, true) => tracedecay_project::project::MovedStoreAdoption::AdoptUnique,
        (None, false, false) => tracedecay_project::project::MovedStoreAdoption::OfferCandidates,
    })
}

/// A refused store surfaces from init as the typed ResetRequired state. The
/// remedy is the scoped operator reset, not manual directory removal, so init
/// names the exact command for this project instead of leaving the raw
/// refusal as the last word.
fn annotate_reset_required_init_error(
    error: tracedecay_domain::errors::TraceDecayError,
    project_path: &Path,
) -> tracedecay_domain::errors::TraceDecayError {
    if error.reset_required_context().is_none() {
        return error;
    }
    let project_path = project_path.to_string_lossy();
    let reset_command = shell_words::join([
        "tracedecay",
        "storage",
        "reset-project-store",
        "--project-root",
        project_path.as_ref(),
        "--yes",
    ]);
    let init_command = shell_words::join(["tracedecay", "init", project_path.as_ref()]);
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!(
            "{error}\n\nthis store cannot be opened until it is reset; run:\n  \
             {reset_command}\n\
             then re-run `{init_command}`, sessions re-ingest from the \
             preserved transcripts"
        ),
    }
}

async fn handle_init_with_daemon_availability(
    profile: &ProfileRoot,
    project_path: PathBuf,
    handshake: tracedecay_daemon_protocol::DaemonHandshake,
    daemon_available: bool,
    wait: bool,
) -> tracedecay_domain::errors::Result<()> {
    if daemon_available {
        return brokered_init(profile, &project_path, &handshake, wait).await;
    }
    Err(tracedecay_domain::errors::TraceDecayError::project_route(
        "code_index_scheduler_unavailable",
        true,
        "project initialization requires the daemon-owned code-index scheduler; start the daemon and retry",
    ))
}

async fn brokered_init(
    profile: &ProfileRoot,
    project_path: &Path,
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    wait: bool,
) -> tracedecay_domain::errors::Result<()> {
    // Init deliberately triggers a cold project open behind this single
    // status call. The default warming-retry grace is far tighter than a cold
    // open can take on a debug build or slow shared runner, which surfaced as
    // "daemon tracedecay_status timed out during read before deadline" failures
    // in CI. Give the bootstrap a generous budget so the client waits out the
    // background open instead of abandoning it just before it completes.
    let init_deadline = Instant::now() + Duration::from_secs(120);
    tracedecay::daemon::call_default_tool_awaiting_project_open(
        profile,
        handshake,
        "tracedecay_status",
        serde_json::json!({"format": "json", "admission_only": true}),
        init_deadline,
    )
    .await?;
    // Admission alone does not start indexing: the code-index scheduler mounts
    // on first demand, and the background full-server upgrade that would
    // eventually demand it does not survive a daemon restart. Request the
    // reconciliation explicitly so the receipt below reports something that
    // actually happened.
    let reconcile = admin_sync(profile, handshake.clone(), init_deadline).await;
    let reconcile = match reconcile {
        Err(error) => {
            if !code_index_reconciliation_is_optional(project_path, &error).await {
                return Err(error);
            }
            eprintln!("{}", init_unavailable_receipt(project_path));
            return Ok(());
        }
        Ok(reconcile) => reconcile,
    };
    match reconcile.status {
        // `queued` means the daemon accepted the reconcile demand into its
        // pre-mount queue. Default init names that request as a typed
        // not-ready receipt; `--wait` holds on the existing status
        // `wait_for` ready gate until the first generation serves.
        AdminSyncAdmissionV1::Queued => {
            if wait {
                return wait_for_first_generation(profile, handshake, project_path).await;
            }
            eprintln!("{}", init_queued_not_ready_receipt(project_path));
        }
        AdminSyncAdmissionV1::NotApplicable => {
            eprintln!("{}", init_not_applicable_receipt(project_path));
        }
    }
    Ok(())
}

/// One `init --wait` status read holds the `wait_for` gate this long. A first
/// generation can take many slices; each slice stays well inside the status
/// dispatch ceiling.
const INIT_WAIT_SLICE_MS: u64 = 30_000;
const INIT_WAIT_SLICE: Duration = Duration::from_millis(INIT_WAIT_SLICE_MS);

/// How long `init --wait` keeps waiting once the daemon stops reporting work
/// toward the first generation. Capture, extraction, and graph publication
/// publish no build progress for minutes on a large repository, so liveness
/// is the scheduler's own in-flight claim, not a progress counter.
const INIT_WAIT_STALL_WINDOW: Duration = Duration::from_secs(120);

fn init_queued_not_ready_receipt(project_path: &Path) -> String {
    format!(
        "enrolled {}; first generation not ready (code_index_reconciliation_requested)\n\
         poll `tracedecay status` until graph_statistics.state is available, or rerun `tracedecay init --wait`",
        project_path.display()
    )
}

fn init_ready_receipt(project_path: &Path) -> String {
    format!(
        "enrolled {}; first generation ready",
        project_path.display()
    )
}

fn init_not_applicable_receipt(project_path: &Path) -> String {
    format!(
        "enrolled {}; code indexing does not apply to this non-Git project",
        project_path.display()
    )
}

fn init_unavailable_receipt(project_path: &Path) -> String {
    format!(
        "enrolled {}; code indexing is unavailable for this non-Git project",
        project_path.display()
    )
}

async fn wait_for_first_generation(
    profile: &ProfileRoot,
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    project_path: &Path,
) -> tracedecay_domain::errors::Result<()> {
    hold_until_first_generation(
        || async move {
            // The transport outlives the slice so its typed outcome arrives.
            let deadline = Instant::now()
                + INIT_WAIT_SLICE
                + tracedecay_daemon_protocol::DAEMON_TOOL_RESPONSE_GRACE;
            let result = tracedecay::daemon::call_default_tool_awaiting_project_open(
                profile,
                handshake,
                "tracedecay_status",
                serde_json::json!({
                    "format": "json",
                    "wait_for": {"state": "ready", "timeout_ms": INIT_WAIT_SLICE_MS}
                }),
                deadline,
            )
            .await?;
            let payload = tracedecay::daemon::recover_truncated_tool_payload(
                profile,
                handshake,
                "tracedecay_status",
                result,
                Some(deadline),
            )
            .await?;
            crate::commands::reject_problem_envelope(&payload, "tracedecay_status")?;
            Ok(payload)
        },
        INIT_WAIT_STALL_WINDOW,
    )
    .await?;
    eprintln!("{}", init_ready_receipt(project_path));
    Ok(())
}

/// Repeats bounded `wait_for` status reads until the first generation is
/// ready. It stops early on a terminal wait outcome, and once the daemon has
/// reported no work toward the generation for a whole `stall_window`.
async fn hold_until_first_generation<F, Fut>(
    mut read_slice: F,
    stall_window: Duration,
) -> tracedecay_domain::errors::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = tracedecay_domain::errors::Result<serde_json::Value>>,
{
    let mut stalled_since: Option<Instant> = None;
    loop {
        let payload = read_slice().await?;
        let last_state = match first_generation_wait_outcome(&payload)? {
            CodeIndexReadinessWaitOutcomeV1::Reached => return Ok(()),
            CodeIndexReadinessWaitOutcomeV1::TimedOut { last_state } => last_state,
            CodeIndexReadinessWaitOutcomeV1::Unavailable { reason } => {
                return Err(first_generation_wait_refusal(
                    CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
                    format!("init --wait cannot reach a ready first generation: {reason}"),
                ));
            }
        };
        let freshness = payload
            .get("code_index_freshness")
            .cloned()
            .map(serde_json::from_value::<StatusCodeIndexFreshnessV1>)
            .transpose()?;
        let worktree = match freshness {
            Some(StatusCodeIndexFreshnessV1::MountFailed {
                message,
                remediation,
            }) => {
                return Err(first_generation_wait_refusal(
                    CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
                    format!(
                        "init --wait cannot reach a ready first generation: {message}; {remediation}"
                    ),
                ));
            }
            Some(
                StatusCodeIndexFreshnessV1::Current { worktree }
                | StatusCodeIndexFreshnessV1::Stale { worktree }
                | StatusCodeIndexFreshnessV1::Restoring { worktree }
                | StatusCodeIndexFreshnessV1::Warming { worktree }
                | StatusCodeIndexFreshnessV1::Parked { worktree },
            ) => Ok(worktree),
            Some(StatusCodeIndexFreshnessV1::Unavailable { reason }) => Err(reason),
            None => Err("status omitted code_index_freshness".to_owned()),
        };
        let progress = worktree
            .as_ref()
            .ok()
            .and_then(|worktree| worktree.progress.as_ref())
            .map(build_progress_summary);
        let progress_suffix = progress
            .as_deref()
            .map(|progress| format!(" ({progress})"))
            .unwrap_or_default();
        match FirstGenerationStall::of(worktree.as_deref().map_err(String::as_str)) {
            None => stalled_since = None,
            Some(stall) => {
                let since = *stalled_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= stall_window {
                    return Err(first_generation_wait_refusal(
                        CODE_INDEX_READINESS_WAIT_TIMED_OUT,
                        format!(
                            "init --wait stopped waiting for the first generation: {stall} for \
                             {}s; last state: {last_state}{progress_suffix}",
                            stall_window.as_secs()
                        ),
                    ));
                }
            }
        }
        eprintln!("first generation not ready yet: {last_state}{progress_suffix}");
    }
}

/// Why the daemon's reading shows no work toward the first generation.
#[derive(Debug, PartialEq, Eq)]
enum FirstGenerationStall {
    /// No code-index scheduler serves the project, for the given reason.
    Unavailable(String),
    /// No reconcile pass owns the route and no wake is pending.
    NoPassInFlight,
    /// The active build reports it cannot advance.
    Blocked(CodeIndexBuildBlockedReasonV1),
}

impl FirstGenerationStall {
    fn of(worktree: Result<&CodeIndexWorktreeFreshnessV1, &str>) -> Option<Self> {
        let worktree = match worktree {
            Ok(worktree) => worktree,
            Err(reason) => return Some(Self::Unavailable(reason.to_owned())),
        };
        if let Some(reason) = worktree
            .progress
            .as_ref()
            .and_then(|progress| progress.blocked_reason)
        {
            return Some(Self::Blocked(reason));
        }
        (!worktree.rebuild_in_flight).then_some(Self::NoPassInFlight)
    }
}

impl std::fmt::Display for FirstGenerationStall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => {
                write!(
                    formatter,
                    "no code-index scheduler serves the project ({reason})"
                )
            }
            Self::NoPassInFlight => formatter.write_str("no reconcile pass is in flight"),
            Self::Blocked(reason) => write!(formatter, "the build is blocked ({reason:?})"),
        }
    }
}

fn build_progress_summary(progress: &CodeIndexBuildProgressV1) -> String {
    format!(
        "{:?} {}/{} files",
        progress.phase, progress.completed_files, progress.total_files
    )
}

fn first_generation_wait_outcome(
    payload: &serde_json::Value,
) -> tracedecay_domain::errors::Result<CodeIndexReadinessWaitOutcomeV1> {
    let wait = payload.get("wait").ok_or_else(|| {
        first_generation_wait_refusal(
            CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
            "status omitted the requested first-generation readiness proof".to_owned(),
        )
    })?;
    Ok(serde_json::from_value(wait.clone())?)
}

fn first_generation_wait_refusal(
    code: &str,
    message: String,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::tool_refused(
        "tracedecay_status",
        Some(code.to_owned()),
        Some(message),
    )
}

/// Asks the project's owner for the operator's code-index reconcile.
async fn admin_sync(
    profile: &ProfileRoot,
    handshake: tracedecay_daemon_protocol::DaemonHandshake,
    deadline: Instant,
) -> tracedecay_domain::errors::Result<AdminSyncResultV1> {
    match crate::tool_command::owner_operation_result(
        profile,
        handshake,
        ApplicationSurfaceOperation::AdminSync,
        serde_json::json!({}),
        deadline,
    )
    .await?
    {
        GraphToolResultV1::AdminSync(result) => Ok(result),
        _ => Err(tracedecay_domain::errors::TraceDecayError::project_route(
            "owner_result_mismatch",
            false,
            "the project owner answered tracedecay_admin_sync with another operation's result",
        )),
    }
}

async fn code_index_reconciliation_is_optional(
    project_path: &Path,
    error: &tracedecay_domain::errors::TraceDecayError,
) -> bool {
    if !error
        .to_string()
        .contains("code_index_scheduler_unavailable")
    {
        return false;
    }
    let deadline = tracedecay_runtime_core::cancellation::MonotonicDeadline::at(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    matches!(
        tracedecay_runtime_core::git_discovery::discover_repository_identity(
            project_path,
            deadline,
            &tracedecay_runtime_core::cancellation::CancellationToken::new(),
        )
        .await,
        tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::NotRepository
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod daemon_precondition_tests {
    use std::path::Path;
    use std::time::Duration;
    use tracedecay_contracts::code_index_freshness::{
        CODE_INDEX_READINESS_WAIT_TIMED_OUT, CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
        CodeIndexBuildBlockedReasonV1, CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1,
        CodeIndexReadinessWaitOutcomeV1, CodeIndexStalenessStateV1, CodeIndexWorktreeFreshnessV1,
    };
    use tracedecay_contracts::retrieval::StatusCodeIndexFreshnessV1;
    use tracedecay_domain::errors::TraceDecayError;

    pub(super) struct SocketEnvGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl SocketEnvGuard {
        pub(super) fn set(value: &Path) -> Self {
            let previous = std::env::var_os(tracedecay_daemon_protocol::SOCKET_ENV);
            unsafe {
                std::env::set_var(tracedecay_daemon_protocol::SOCKET_ENV, value);
            }
            Self { previous }
        }
    }

    impl Drop for SocketEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(previous) => {
                        std::env::set_var(tracedecay_daemon_protocol::SOCKET_ENV, previous);
                    }
                    None => std::env::remove_var(tracedecay_daemon_protocol::SOCKET_ENV),
                }
            }
        }
    }

    /// Init's daemon precondition is a probe on every platform, not a `cfg`.
    ///
    /// This test is deliberately not gated to unix. On Windows the endpoint is
    /// the loopback authority rather than a socket file, and a profile that
    /// has no authority record has no daemon to broker through, so the answer
    /// must be `false` there exactly as it is on unix. Hardcoding availability
    /// off-unix let init run past the scheduler it then requires, and that
    /// regression is only observable from a test the Windows shard compiles.
    #[test]
    fn init_daemon_availability_is_probed_on_every_platform() {
        let profile = tempfile::TempDir::new().expect("temp profile");
        let _socket = SocketEnvGuard::set(&profile.path().join("absent.sock"));

        assert!(
            !super::init_daemon_available(&tracedecay_runtime_core::config::ProfileRoot::new(
                profile.path()
            )),
            "an endpoint with no listener must not count as an available daemon"
        );
    }

    #[test]
    fn queued_init_receipt_is_typed_not_ready() {
        let text = super::init_queued_not_ready_receipt(Path::new("/repo"));
        assert!(
            text.contains("enrolled /repo"),
            "receipt must name enrollment, not a finished index: {text}"
        );
        assert!(
            text.contains("first generation not ready"),
            "receipt must say the first generation is not ready: {text}"
        );
        assert!(
            text.contains("code_index_reconciliation_requested"),
            "receipt must carry the typed not-ready reason: {text}"
        );
        assert!(
            !text.contains("initialized "),
            "receipt must not look like a finished index: {text}"
        );
    }

    fn timed_out() -> CodeIndexReadinessWaitOutcomeV1 {
        CodeIndexReadinessWaitOutcomeV1::TimedOut {
            last_state: "warming".to_owned(),
        }
    }

    fn building(rebuild_in_flight: bool) -> CodeIndexWorktreeFreshnessV1 {
        CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/repo".to_owned(),
            staleness_state: Some(CodeIndexStalenessStateV1::Indexing),
            rebuild_in_flight,
            ..CodeIndexWorktreeFreshnessV1::default()
        }
    }

    fn progress(
        completed_files: u64,
        blocked_reason: Option<CodeIndexBuildBlockedReasonV1>,
    ) -> CodeIndexBuildProgressV1 {
        CodeIndexBuildProgressV1 {
            generation_id: "generation.v1.first".to_owned(),
            daemon_incarnation: 1,
            producer_incarnation: 1,
            progress_epoch: completed_files + 1,
            sealed_source_digest: "digest".to_owned(),
            phase: CodeIndexBuildPhaseV1::SourceScan,
            committed_pages: completed_files,
            committed_chunks: 0,
            committed_imports: 0,
            committed_payload_bytes: 0,
            completed_files,
            total_files: 6_136,
            completed_lexical_units: 0,
            total_lexical_units: 0,
            current_batch_pages: 0,
            current_batch_payload_bytes: 0,
            elapsed_micros: 0,
            last_commit_latency_micros: None,
            files_per_second: None,
            lexical_units_per_second: None,
            estimated_remaining_seconds: None,
            last_progress_micros: 1,
            blocked_reason,
        }
    }

    fn status(
        outcome: CodeIndexReadinessWaitOutcomeV1,
        freshness: Option<StatusCodeIndexFreshnessV1>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({ "wait": outcome });
        if let Some(freshness) = freshness {
            payload["code_index_freshness"] = serde_json::to_value(freshness).unwrap();
        }
        payload
    }

    fn warming(worktree: CodeIndexWorktreeFreshnessV1) -> Option<StatusCodeIndexFreshnessV1> {
        Some(StatusCodeIndexFreshnessV1::Warming {
            worktree: Box::new(worktree),
        })
    }

    /// Runs the `init --wait` loop over scripted status reads and reports
    /// its result with how many reads it consumed.
    async fn hold_over(
        script: Vec<serde_json::Value>,
        stall_window: Duration,
    ) -> (tracedecay_domain::errors::Result<()>, usize) {
        let total = script.len();
        let mut reads = script.into_iter();
        let result = super::hold_until_first_generation(
            || {
                let next = reads
                    .next()
                    .expect("the wait read past the scripted status");
                async move { Ok(next) }
            },
            stall_window,
        )
        .await;
        (result, total - reads.len())
    }

    fn refusal(result: tracedecay_domain::errors::Result<()>) -> (String, String) {
        let TraceDecayError::ToolRefused(refusal) = result.expect_err("the wait must refuse")
        else {
            panic!("a wait refusal must stay a typed tool refusal");
        };
        (
            refusal.code.unwrap_or_default(),
            refusal.reason.unwrap_or_default(),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_requires_positive_wait_proof() {
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"wait": null}),
            serde_json::json!({"wait": {"outcome": "unknown"}}),
        ] {
            let (result, _) = hold_over(vec![payload], Duration::ZERO).await;
            assert!(result.is_err(), "only a reached outcome proves readiness");
        }
        let (result, reads) = hold_over(
            vec![status(CodeIndexReadinessWaitOutcomeV1::Reached, None)],
            Duration::ZERO,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(reads, 1);
    }

    /// A first generation outlives any one `wait_for` budget. The wait holds
    /// through every timed-out slice in which the daemon reports a pass in
    /// flight, whether or not the build has published progress yet, and even
    /// with no stall tolerance at all.
    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_holds_while_the_build_is_in_flight() {
        let mut capturing = building(true);
        capturing.progress = None;
        let mut scanning = building(true);
        scanning.progress = Some(progress(1_112, None));
        let mut publishing_graph = building(true);
        publishing_graph.progress = Some(CodeIndexBuildProgressV1 {
            phase: CodeIndexBuildPhaseV1::GraphPublication,
            ..progress(6_136, None)
        });
        let script = vec![
            status(timed_out(), warming(capturing.clone())),
            status(timed_out(), warming(capturing)),
            status(timed_out(), warming(scanning)),
            status(timed_out(), warming(publishing_graph.clone())),
            status(timed_out(), warming(publishing_graph)),
            status(CodeIndexReadinessWaitOutcomeV1::Reached, None),
        ];
        let (result, reads) = hold_over(script, Duration::ZERO).await;
        assert!(
            result.is_ok(),
            "an in-flight build must not end the wait: {result:?}"
        );
        assert_eq!(reads, 6, "the wait must read until the generation is ready");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_names_the_stall_that_ended_it() {
        let mut blocked = building(true);
        blocked.progress = Some(progress(
            40,
            Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
        ));
        for (freshness, stall) in [
            (warming(building(false)), "no reconcile pass is in flight"),
            (warming(blocked), "the build is blocked (ResidentMemory)"),
            (
                Some(StatusCodeIndexFreshnessV1::Unavailable {
                    reason: "code_index_scheduler_not_mounted".to_owned(),
                }),
                "no code-index scheduler serves the project (code_index_scheduler_not_mounted)",
            ),
        ] {
            let (result, reads) =
                hold_over(vec![status(timed_out(), freshness)], Duration::ZERO).await;
            assert_eq!(reads, 1);
            let (code, message) = refusal(result);
            assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT, "{message}");
            assert!(message.contains(stall), "stall must be named: {message}");
            assert!(message.contains("last state: warming"), "{message}");
        }
    }

    /// A stall shorter than the window is ridden out: the window restarts
    /// once the daemon reports work again.
    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_rides_out_a_stall_inside_the_window() {
        let script = vec![
            status(timed_out(), warming(building(false))),
            status(timed_out(), None),
            status(timed_out(), warming(building(true))),
            status(CodeIndexReadinessWaitOutcomeV1::Reached, None),
        ];
        let (result, reads) = hold_over(script, Duration::from_secs(3_600)).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(reads, 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_ends_at_once_on_a_terminal_state() {
        for payload in [
            status(
                CodeIndexReadinessWaitOutcomeV1::Unavailable {
                    reason: "code_index_convergence_parked".to_owned(),
                },
                None,
            ),
            status(
                timed_out(),
                Some(StatusCodeIndexFreshnessV1::MountFailed {
                    message: "the code-index scheduler could not mount".to_owned(),
                    remediation: "run `tracedecay sync`".to_owned(),
                }),
            ),
        ] {
            let (result, reads) = hold_over(vec![payload], Duration::from_secs(3_600)).await;
            assert_eq!(reads, 1, "a terminal state must not be retried");
            let (code, message) = refusal(result);
            assert_eq!(code, CODE_INDEX_READINESS_WAIT_UNAVAILABLE, "{message}");
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod init_bootstrap_tests {
    use super::*;

    fn test_handshake(
        project_path: &Path,
        profile_root: &Path,
    ) -> tracedecay_daemon_protocol::DaemonHandshake {
        tracedecay_daemon_protocol::DaemonHandshake {
            project_path: Some(project_path.to_path_buf()),
            scope_prefix: None,
            timings: false,
            allow_init: true,
            allow_initialize_root_routing: false,
            client_identity: tracedecay_daemon_protocol::DaemonClientIdentity {
                profile_root: profile_root.to_path_buf(),
                global_db_path: profile_root.join("global.db"),
            },
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_instance_id: "commands-init-test".to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: tracedecay_project::project::MovedStoreAdoption::Never,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn daemonless_init_refuses_without_the_code_index_scheduler() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        let handshake = test_handshake(&project, &profile);

        let error = handle_init_with_daemon_availability(
            &ProfileRoot::new(&profile),
            project.clone(),
            handshake,
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.project_route_context(),
                Some(("code_index_scheduler_unavailable", true, _))
            ),
            "unexpected daemonless init error: {error}"
        );
        assert!(
            !profile.exists(),
            "scheduler refusal must not initialize a local store"
        );
    }

    /// A worktree parked on a corrupt publication authority refuses the
    /// reconcile; the operator reads its cause and remedy as whole fields,
    /// not folded into one sentence cut at the diagnostic bound.
    #[test]
    fn a_parked_admin_sync_refusal_prints_as_fields() {
        let parked = tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1 {
            reason: format!("the publication authority is corrupt: {}", "x".repeat(600)),
            blocked_reason: None,
            remediation: "run `tracedecay daemon restart`".to_owned(),
            parked_at_micros: 1,
            observed_passes: 1,
            retries_on_wake: false,
        };
        let detail = parked
            .publication_authority_corrupt_error()
            .project_route_typed_detail()
            .cloned()
            .expect("a parked refusal carries its typed detail");
        let refusal = tracedecay_mcp::application_output::tool_result::ApplicationRefusal {
            operation: ApplicationSurfaceOperation::AdminSync,
            binding_id: tracedecay_tool_catalog::BindingId::new("binding.cli.admin_sync.v1")
                .unwrap(),
            problem: tracedecay_contracts::ApplicationProblemEnvelope::new(
                tracedecay_contracts::ResultContractRef::new(
                    tracedecay_tool_catalog::SchemaId::new(
                        "schema.application.primitive.admin-sync.result",
                    )
                    .unwrap(),
                    1,
                )
                .unwrap(),
                tracedecay_contracts::RequestId::new("request.cli.admin-sync").unwrap(),
                tracedecay_contracts::ApplicationProblem::from_detail(detail),
            )
            .unwrap(),
        };

        assert_eq!(
            crate::commands::process_error_text(refusal.into_error()),
            format!(
                "project route error (application.code-index.parked)\n\
                 Parked cause: the publication authority is corrupt: {}\n\
                 Parked remedy: run `tracedecay daemon restart`\n\
                 Retries on wake: false",
                "x".repeat(600)
            )
        );
    }

    #[tokio::test]
    async fn only_non_git_scheduler_unavailability_is_optional_during_init() {
        let temp = tempfile::TempDir::new().unwrap();
        let non_git = temp.path().join("non-git");
        let git = temp.path().join("git");
        let nested_git = git.join("nested");
        std::fs::create_dir_all(&non_git).unwrap();
        std::fs::create_dir_all(&nested_git).unwrap();
        let initialized = std::process::Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .args(["init", "-q"])
        .current_dir(&git)
        .status()
        .unwrap();
        assert!(initialized.success());
        let unavailable = tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon tool call failed: code_index_scheduler_unavailable: admin sync was not accepted"
                .to_string(),
        };
        let unrelated = tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon tool call failed: storage unavailable".to_string(),
        };

        assert!(code_index_reconciliation_is_optional(&non_git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&nested_git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&non_git, &unrelated).await);
    }

    #[test]
    fn reset_remedy_commands_survive_paths_with_spaces_and_apostrophes() {
        let error = annotate_reset_required_init_error(
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "project-fixture",
                "incompatible persisted shape",
            ),
            Path::new("/repo/it's an example"),
        );

        let text = error.to_string();
        assert!(text.contains("this store cannot be opened until it is reset"));
        let reset_command = text
            .split_once("run:\n  ")
            .expect("reset command must be named")
            .1;
        let (reset_command, then_part) = reset_command
            .split_once("\n")
            .expect("reset command ends before the init remedy");
        assert_eq!(
            shell_words::split(reset_command).unwrap(),
            [
                "tracedecay",
                "storage",
                "reset-project-store",
                "--project-root",
                "/repo/it's an example",
                "--yes",
            ]
        );
        let init_command = then_part
            .split_once("`tracedecay init ")
            .map(|(_, tail)| tail)
            .and_then(|tail| tail.split_once('`').map(|(command, _)| command))
            .expect("init remedy must be named");
        assert_eq!(
            shell_words::split(&format!("tracedecay init {init_command}")).unwrap(),
            ["tracedecay", "init", "/repo/it's an example"]
        );
    }
}

#[tracing::instrument(name = "cli.sync.run", level = "trace", skip_all)]
pub(crate) async fn handle_sync(
    profile: &ProfileRoot,
    path: Option<String>,
    verbose: bool,
) -> tracedecay_domain::errors::Result<()> {
    let resolved = super::scope::resolve_project_scope(
        profile,
        tracedecay_configuration::resolve_path_with_discovery(profile, path),
    )
    .await?;
    let handshake = super::daemon::client_handshake(profile, Some(&resolved.project_path))?;
    let deadline = crate::tool_command::tool_command_deadline()?;
    let result = admin_sync(profile, handshake, deadline).await?;
    if verbose {
        eprintln!("{}", serde_json::to_string_pretty(&result)?);
    }
    match result.status {
        AdminSyncAdmissionV1::Queued => eprintln!(
            "code-index reconciliation queued via daemon for {}",
            resolved.project_path.display()
        ),
        AdminSyncAdmissionV1::NotApplicable => eprintln!(
            "code indexing does not apply to {}",
            resolved.project_path.display()
        ),
    }
    Ok(())
}
