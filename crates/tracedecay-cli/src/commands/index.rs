use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_project::project::TraceDecay;

use tracedecay_contracts::code_index_freshness::{
    CODE_INDEX_READINESS_WAIT_TIMED_OUT, CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
    CodeIndexBuildBlockedReasonV1, CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1,
    CodeIndexNativeGraphPhaseV1, CodeIndexPresealAttemptProgressV1,
    CodeIndexReadinessWaitOutcomeV1, CodeIndexStalenessStateV1, CodeIndexWorktreeFreshnessV1,
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
    let adoption = moved_store_adoption_request(adopt_project, assume_yes)?;
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
    tracedecay_daemon_control::daemon_socket_connectable(
        profile,
        crate::product_runtime::PRODUCT_BUILD_VERSION,
    )
}

/// Only a named project authorizes adoption. Refuse the former `init --yes`
/// selection so an old command cannot silently mint a different identity.
fn moved_store_adoption_request(
    adopt_project: Option<String>,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<tracedecay_project::project::MovedStoreAdoption> {
    if assume_yes && adopt_project.is_none() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "init --yes no longer selects a moved project; use --adopt-project <id> \
                      to adopt a named project, or omit --yes to initialize a fresh identity"
                .to_owned(),
        });
    }
    Ok(match adopt_project {
        Some(project_id) => tracedecay_project::project::MovedStoreAdoption::AdoptNamed(project_id),
        None => tracedecay_project::project::MovedStoreAdoption::Never,
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

/// Maximum silence for an observed capture, parse, or committed text
/// boundary, or for an unavailable readiness owner. Graph publication has no
/// native write authority on the freshness wire yet, so its phase alone must
/// not become a false no-progress timeout.
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
    let mut idle_window = FirstGenerationIdleWindow::new(stall_window);
    let mut progress_window = FirstGenerationProgressWindow::new(stall_window);
    loop {
        let payload = read_slice().await?;
        let last_state = match first_generation_wait_outcome(&payload)? {
            CodeIndexReadinessWaitOutcomeV1::Reached => return Ok(()),
            CodeIndexReadinessWaitOutcomeV1::TimedOut { last_state } => last_state,
            CodeIndexReadinessWaitOutcomeV1::Unavailable { reason } => {
                // The wait carries an availability code. Preserve the actual
                // daemon park's cause and remedy when its typed status exists.
                let park_detail = payload
                    .get("code_index_freshness")
                    .cloned()
                    .map(serde_json::from_value::<StatusCodeIndexFreshnessV1>)
                    .transpose()?
                    .and_then(|freshness| match freshness {
                        StatusCodeIndexFreshnessV1::Parked { worktree } => worktree.parked,
                        _ => None,
                    })
                    .map(|parked| {
                        format!(
                            "; parked cause: {}; remedy: {}; retries on wake: {}",
                            parked.reason, parked.remediation, parked.retries_on_wake
                        )
                    })
                    .unwrap_or_default();
                return Err(first_generation_wait_refusal(
                    CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
                    format!(
                        "init --wait cannot reach a ready first generation: {reason}{park_detail}"
                    ),
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
        let progress = worktree.as_ref().ok().and_then(|worktree| {
            worktree
                .native_graph_progress
                .as_ref()
                .filter(|graph| graph.active && graph.last_progress_micros.is_some())
                .map(native_graph_progress_summary)
                .or_else(|| {
                    worktree
                        .preseal_attempt
                        .as_ref()
                        .filter(|attempt| attempt.active)
                        .map(preseal_progress_summary)
                })
                .or_else(|| worktree.progress.as_ref().map(build_progress_summary))
        });
        let progress_suffix = progress
            .as_deref()
            .map(|progress| format!(" ({progress})"))
            .unwrap_or_default();
        if let Some(stall) = idle_window.observe(
            FirstGenerationStall::of(worktree.as_deref().map_err(String::as_str)),
            Instant::now(),
        ) {
            return Err(first_generation_wait_refusal(
                CODE_INDEX_READINESS_WAIT_TIMED_OUT,
                format!(
                    "init --wait stopped waiting for the first generation: {stall} for \
                     {}s; last state: {last_state}{progress_suffix}",
                    stall_window.as_secs()
                ),
            ));
        }
        let worktree_read = worktree.as_deref().ok();
        let marker = FirstGenerationWorkMarker::of(worktree_read);
        // A retained text bulk commit and a successor's live capture can
        // advance independently. A frozen text cursor cannot hide real
        // preseal work, nor can a quiet preseal hide actual durable commits.
        let concurrent_preseal = if matches!(
            marker.as_ref(),
            Some(
                FirstGenerationWorkMarker::CommittedText { .. }
                    | FirstGenerationWorkMarker::NativeGraph { .. }
            )
        ) {
            worktree_read
                .and_then(|worktree| worktree.preseal_attempt.as_ref())
                .filter(|attempt| attempt.active)
                .map(FirstGenerationWorkMarker::preseal)
        } else {
            None
        };
        // A separately owned retained lexical writer can commit pages while
        // native graph encoding or successor source capture is quiet. Read
        // its actual bulk receipts alongside the primary writer instead of
        // letting a frozen native/preseal marker hide completed text work.
        let concurrent_text = if matches!(
            marker.as_ref(),
            Some(
                FirstGenerationWorkMarker::Preseal { .. }
                    | FirstGenerationWorkMarker::NativeGraph { .. }
            )
        ) {
            worktree_read
                .and_then(|worktree| worktree.progress.as_ref())
                .filter(|progress| progress.phase == CodeIndexBuildPhaseV1::BulkCommit)
                .map(FirstGenerationWorkMarker::text)
        } else {
            None
        };
        let now = Instant::now();
        let unknown_native_only = worktree_read.is_some_and(|worktree| {
            FirstGenerationWorkMarker::has_uninstrumented_native_write(worktree)
                && !worktree
                    .preseal_attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.active)
        });
        let stalled = if unknown_native_only {
            // A retained text snapshot has no live-owner flag. Its frozen
            // receipts cannot prove silence in the final native write. Keep
            // real text watermarks, but pause this unobservable interval;
            // neither polling nor this coverage change is producer work.
            if let Some(marker) = marker {
                progress_window.record(marker);
            }
            progress_window.observe(None, now)
        } else if concurrent_preseal.is_some() || concurrent_text.is_some() {
            progress_window.observe_with_concurrent_writers(
                marker,
                concurrent_preseal,
                concurrent_text,
                now,
            )
        } else {
            progress_window.observe(marker, now)
        };
        if stalled {
            return Err(first_generation_wait_refusal(
                CODE_INDEX_READINESS_WAIT_TIMED_OUT,
                format!(
                    "init --wait stopped waiting for the first generation: no observed \
                     capture, parse, text, or native graph work for {}s; last state: \
                     {last_state}{progress_suffix}",
                    stall_window.as_secs(),
                ),
            ));
        }
        eprintln!("first generation not ready yet: {last_state}{progress_suffix}");
    }
}

/// Why the daemon's reading shows no work toward the first generation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FirstGenerationStall {
    /// No code-index scheduler serves the project, for the given reason.
    Unavailable(String),
    /// No reconcile pass owns the route and no wake is pending.
    NoPassInFlight,
    /// The active build reports it cannot advance.
    Blocked(CodeIndexBuildBlockedReasonV1),
    /// A terminal convergence park names the operator action required.
    Parked { cause: String, remedy: String },
}

impl FirstGenerationStall {
    fn of(worktree: Result<&CodeIndexWorktreeFreshnessV1, &str>) -> Option<Self> {
        let worktree = match worktree {
            Ok(worktree) => worktree,
            Err(reason) => return Some(Self::Unavailable(reason.to_owned())),
        };
        let active_preseal = worktree
            .preseal_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.active);
        let active_graph = worktree
            .native_graph_progress
            .as_ref()
            .filter(|graph| graph.active);
        // An old terminal park cannot be renewed by the rebuild flag.
        // An independently active successor still owns its own work.
        if let Some(parked) = worktree
            .parked
            .as_ref()
            .filter(|parked| !parked.retries_on_wake && !active_preseal && active_graph.is_none())
        {
            return Some(Self::Parked {
                cause: parked.reason.clone(),
                remedy: parked.remediation.clone(),
            });
        }
        if let Some(progress) = worktree.progress.as_ref() {
            if let Some(reason) = progress.blocked_reason {
                // The previous sealed generation can remain visible while a
                // successor captures or publishes native graph rows. Its old
                // refusal is not a refusal by the new, active writer.
                let retained_text = active_preseal
                    && worktree.latest_generation_id.as_deref()
                        == Some(progress.generation_id.as_str())
                    && progress.phase != CodeIndexBuildPhaseV1::SourceScan;
                let different_graph =
                    active_graph.is_some_and(|graph| graph.generation_id != progress.generation_id);
                if !retained_text && !different_graph {
                    return Some(Self::Blocked(reason));
                }
            }
        }
        // Active attempts remain readiness work even if the freshness ladder
        // does not classify a source verification or restore as a rebuild.
        let readiness_in_flight = worktree.rebuild_in_flight
            || active_preseal
            || active_graph.is_some()
            || matches!(
                worktree.staleness_state,
                Some(CodeIndexStalenessStateV1::Restoring | CodeIndexStalenessStateV1::Verifying)
            );
        (!readiness_in_flight).then_some(Self::NoPassInFlight)
    }
}

/// Observed work under one producer. Epochs order reports; they are not work.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FirstGenerationWorkMarker {
    Preseal {
        owner: (u64, u64, u64),
        epoch: u64,
        phase: tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1,
        completed: u64,
        last_work_micros: Option<i64>,
    },
    NativeGraph {
        generation: String,
        sealed_graph_digest: String,
        owner: (u64, u64, u64),
        epoch: u64,
        completed: [u64; 6],
    },
    CommittedText {
        generation: String,
        sealed_source_digest: String,
        owner: (u64, u64),
        epoch: u64,
        completed: [u64; 6],
    },
}

impl FirstGenerationWorkMarker {
    fn has_uninstrumented_native_write(worktree: &CodeIndexWorktreeFreshnessV1) -> bool {
        worktree
            .native_graph_progress
            .as_ref()
            .filter(|graph| graph.active)
            .is_some_and(|graph| {
                let encoding_finished = graph.phase
                    == Some(CodeIndexNativeGraphPhaseV1::CompactEncoding)
                    && graph.total_entities == Some(graph.encoded_entities)
                    && graph
                        .total_relations
                        .is_some_and(|total| total == graph.encoded_relations);
                encoding_finished
                    || graph.phase == Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified)
            })
    }

    fn of(worktree: Option<&CodeIndexWorktreeFreshnessV1>) -> Option<Self> {
        let worktree = worktree?;
        // Restoring and verifying are readiness work even when the canonical
        // freshness ladder deliberately reports rebuild_in_flight=false.
        // A real active attempt remains authoritative in either state.
        if !worktree.rebuild_in_flight
            && !worktree.preseal_attempt.as_ref().is_some_and(|a| a.active)
            && !worktree
                .native_graph_progress
                .as_ref()
                .is_some_and(|g| g.active)
        {
            return None;
        }
        if let Some(graph) = worktree.native_graph_progress.as_ref().filter(|g| g.active) {
            // Direct seal has one final Grafeo compact-container write and
            // digest verification AFTER all accepted row windows. That work
            // has no intermediate native milestone. Never call a healthy
            // final write a stall merely because the last encode clock froze.
            if Self::has_uninstrumented_native_write(worktree) {
                // The final Grafeo container write has no row callbacks.
                // Independently owned source or lexical writers may still
                // report real work; never hide their observations behind it.
                return worktree
                    .preseal_attempt
                    .as_ref()
                    .filter(|attempt| attempt.active)
                    .map(Self::preseal)
                    .or_else(|| {
                        worktree
                            .progress
                            .as_ref()
                            .filter(|progress| progress.phase == CodeIndexBuildPhaseV1::BulkCommit)
                            .map(Self::text)
                    });
            }
            // Starting the native attempt proves only that there is an owner,
            // not that it has written any rows. A silent initial window must
            // be timed from the first status read, not treated as unknown
            // forever or credited with an invented progress timestamp.
            // Continue using real text commits if they are advancing
            // concurrently before the graph reports its first boundary.
            if graph.last_progress_micros.is_none()
                && let Some(progress) = worktree
                    .progress
                    .as_ref()
                    .filter(|progress| progress.phase == CodeIndexBuildPhaseV1::BulkCommit)
            {
                return Some(Self::text(progress));
            }
            return Some(Self::native_graph(graph));
        }
        // A real text bulk commit may be concurrent with the scoped preseal
        // guard and remains the authority while its committed cursor moves.
        if let Some(progress) = worktree
            .progress
            .as_ref()
            .filter(|progress| progress.phase == CodeIndexBuildPhaseV1::BulkCommit)
        {
            return Some(Self::text(progress));
        }
        // A retained generation may still report Ready or a post-text phase
        // while the successor actually captures or parses. Those old phase
        // projections must not hide its independently owned attempt work.
        if let Some(attempt) = worktree.preseal_attempt.as_ref().filter(|a| a.active) {
            return Some(Self::preseal(attempt));
        }
        // Absent a live attempt, uninstrumented post-text phases remain
        // unknown, not evidence that a writer has stalled.
        worktree
            .progress
            .as_ref()
            .filter(|progress| progress.phase == CodeIndexBuildPhaseV1::SourceScan)
            .map(Self::text)
    }

    fn preseal(attempt: &CodeIndexPresealAttemptProgressV1) -> Self {
        Self::Preseal {
            owner: (
                attempt.daemon_incarnation,
                attempt.producer_incarnation,
                attempt.attempt_sequence,
            ),
            epoch: attempt.progress_epoch,
            phase: attempt.phase,
            completed: attempt.completed_items,
            last_work_micros: attempt.last_progress_micros,
        }
    }

    fn native_graph(
        graph: &tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1,
    ) -> Self {
        Self::NativeGraph {
            generation: graph.generation_id.clone(),
            sealed_graph_digest: graph.sealed_graph_digest.clone(),
            owner: (
                graph.daemon_incarnation,
                graph.producer_incarnation,
                graph.attempt_sequence,
            ),
            epoch: graph.progress_epoch,
            completed: [
                graph.encoded_entities,
                graph.encoded_relations,
                graph.committed_stage_pages,
                graph.committed_stage_mutations,
                graph.committed_stage_bytes,
                graph.committed_compact_segments,
            ],
        }
    }

    fn text(progress: &CodeIndexBuildProgressV1) -> Self {
        Self::CommittedText {
            generation: progress.generation_id.clone(),
            sealed_source_digest: progress.sealed_source_digest.clone(),
            owner: (progress.daemon_incarnation, progress.producer_incarnation),
            epoch: progress.progress_epoch,
            completed: [
                progress.completed_files,
                progress.completed_lexical_units,
                progress.committed_pages,
                progress.committed_chunks,
                progress.committed_imports,
                progress.committed_payload_bytes,
            ],
        }
    }

    fn owner(&self) -> (u64, u64) {
        match self {
            Self::Preseal { owner, .. } => (owner.0, owner.1),
            Self::NativeGraph { owner, .. } => (owner.0, owner.1),
            Self::CommittedText { owner, .. } => *owner,
        }
    }

    /// Accept a newer report and say whether actual work advanced. A newer
    /// refusal, retry, timestamp or daemon incarnation is not itself work.
    /// `None` rejects a duplicate or stale report without lowering the fence.
    fn advance_from(&self, previous: &Self) -> Option<bool> {
        match (self, previous) {
            (
                Self::Preseal {
                    owner,
                    epoch,
                    phase,
                    completed,
                    last_work_micros,
                },
                Self::Preseal {
                    owner: prior_owner,
                    epoch: prior_epoch,
                    phase: prior_phase,
                    completed: prior_completed,
                    ..
                },
            ) => {
                if (owner, epoch) <= (prior_owner, prior_epoch) {
                    return None;
                }
                if owner == prior_owner {
                    if phase < prior_phase || (phase == prior_phase && completed < prior_completed)
                    {
                        return None;
                    }
                    if phase == prior_phase {
                        return Some(completed > prior_completed && last_work_micros.is_some());
                    }
                }
                Some(*completed > 0 && last_work_micros.is_some())
            }
            (
                Self::NativeGraph {
                    generation,
                    sealed_graph_digest,
                    owner,
                    epoch,
                    completed,
                },
                Self::NativeGraph {
                    generation: prior_generation,
                    sealed_graph_digest: prior_digest,
                    owner: prior_owner,
                    epoch: prior_epoch,
                    completed: prior_completed,
                },
            ) => {
                if (owner, epoch) <= (prior_owner, prior_epoch) {
                    return None;
                }
                if owner == prior_owner
                    && (generation != prior_generation || sealed_graph_digest != prior_digest)
                {
                    return None;
                }
                if owner != prior_owner {
                    return Some(completed.iter().any(|count| *count > 0));
                }
                Some(
                    completed != prior_completed
                        && completed
                            .iter()
                            .zip(prior_completed)
                            .all(|(now, prior)| now >= prior),
                )
            }
            (
                Self::CommittedText {
                    generation,
                    sealed_source_digest,
                    owner,
                    epoch,
                    completed,
                },
                Self::CommittedText {
                    generation: prior_generation,
                    sealed_source_digest: prior_digest,
                    owner: prior_owner,
                    epoch: prior_epoch,
                    completed: prior_completed,
                },
            ) => {
                if (owner, epoch) <= (prior_owner, prior_epoch) {
                    return None;
                }
                if generation != prior_generation {
                    return Some(completed.iter().any(|count| *count > 0));
                }
                if sealed_source_digest != prior_digest {
                    return None;
                }
                // A verified staging restart may reset counters. Adopt that
                // newer baseline, but only subsequent work renews the wait.
                Some(
                    completed != prior_completed
                        && completed
                            .iter()
                            .zip(prior_completed)
                            .all(|(now, prior)| now >= prior),
                )
            }
            _ => None,
        }
    }
}

/// Maintain independent preseal, native-graph and text watermarks. Polling,
/// status changes and empty retries cannot renew an observed silence window.
struct FirstGenerationProgressWindow {
    preseal: Option<FirstGenerationWorkMarker>,
    text: Option<FirstGenerationWorkMarker>,
    native_graph: Option<FirstGenerationWorkMarker>,
    owner: Option<(u64, u64)>,
    unchanged_since: Option<Instant>,
    stall_window: Duration,
}

impl FirstGenerationProgressWindow {
    fn new(stall_window: Duration) -> Self {
        Self {
            preseal: None,
            text: None,
            native_graph: None,
            owner: None,
            unchanged_since: None,
            stall_window,
        }
    }

    fn observe(&mut self, marker: Option<FirstGenerationWorkMarker>, now: Instant) -> bool {
        self.observe_with_concurrent_writers(marker, None, None, now)
    }

    fn observe_with_concurrent_writers(
        &mut self,
        marker: Option<FirstGenerationWorkMarker>,
        concurrent_preseal: Option<FirstGenerationWorkMarker>,
        concurrent_text: Option<FirstGenerationWorkMarker>,
        now: Instant,
    ) -> bool {
        if marker.is_none() && concurrent_preseal.is_none() && concurrent_text.is_none() {
            // Uninstrumented work is unknown, not stalled. Keep watermarks
            // so returning to an old report cannot fabricate new work.
            self.unchanged_since = None;
            return false;
        }
        let mut advanced = false;
        if let Some(marker) = marker {
            advanced |= self.record(marker);
        }
        if let Some(preseal) = concurrent_preseal {
            advanced |= self.record(preseal);
        }
        if let Some(text) = concurrent_text {
            advanced |= self.record(text);
        }
        if advanced || self.unchanged_since.is_none() {
            self.unchanged_since = Some(now);
            return false;
        }
        self.unchanged_since
            .is_some_and(|since| now.duration_since(since) >= self.stall_window)
    }

    fn record(&mut self, marker: FirstGenerationWorkMarker) -> bool {
        let owner = marker.owner();
        // Independently owned writers on one daemon may overlap. Fence an
        // older daemon globally; each marker kind fences its own producer.
        if self.owner.is_some_and(|current| owner.0 < current.0) {
            return false;
        }
        self.owner = Some(self.owner.map_or(owner, |current| current.max(owner)));
        let previous = match &marker {
            FirstGenerationWorkMarker::Preseal { .. } => &mut self.preseal,
            FirstGenerationWorkMarker::NativeGraph { .. } => &mut self.native_graph,
            FirstGenerationWorkMarker::CommittedText { .. } => &mut self.text,
        };
        let advanced = match previous.as_ref() {
            Some(prior) => match marker.advance_from(prior) {
                Some(advanced) => advanced,
                None => return false,
            },
            None => match &marker {
                FirstGenerationWorkMarker::Preseal {
                    completed,
                    last_work_micros,
                    ..
                } => *completed > 0 && last_work_micros.is_some(),
                FirstGenerationWorkMarker::NativeGraph { completed, .. }
                | FirstGenerationWorkMarker::CommittedText { completed, .. } => {
                    completed.iter().any(|count| *count > 0)
                }
            },
        };
        *previous = Some(marker);
        advanced
    }
}

/// Only continuous idle observations spend the stall window. An active
/// readiness owner clears the window even if it publishes no new progress.
struct FirstGenerationIdleWindow {
    stalled_since: Option<Instant>,
    stall_window: Duration,
}

impl FirstGenerationIdleWindow {
    fn new(stall_window: Duration) -> Self {
        Self {
            stalled_since: None,
            stall_window,
        }
    }

    fn observe(
        &mut self,
        stall: Option<FirstGenerationStall>,
        now: Instant,
    ) -> Option<FirstGenerationStall> {
        let Some(stall) = stall else {
            self.stalled_since = None;
            return None;
        };
        let since = *self.stalled_since.get_or_insert(now);
        (now.duration_since(since) >= self.stall_window).then_some(stall)
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
            Self::Parked { cause, remedy } => {
                write!(
                    formatter,
                    "convergence is parked ({cause}); remedy: {remedy}"
                )
            }
        }
    }
}

fn preseal_progress_summary(
    progress: &tracedecay_contracts::code_index_freshness::CodeIndexPresealAttemptProgressV1,
) -> String {
    let phase = match progress.phase {
        tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::Capture => {
            "capturing"
        }
        tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::ParseEvidence => {
            "parsing"
        }
        tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::Resolution => {
            "resolving references"
        }
    };
    let total = progress
        .total_items
        .map_or_else(|| "unknown".to_owned(), |total| total.to_string());
    format!(
        "{phase} {}/{} items, attempt={}:{}:{} epoch={} last_work_micros={}",
        progress.completed_items,
        total,
        progress.daemon_incarnation,
        progress.producer_incarnation,
        progress.attempt_sequence,
        progress.progress_epoch,
        progress
            .last_progress_micros
            .map_or_else(|| "unreported".to_owned(), |micros| micros.to_string()),
    )
}

fn native_graph_progress_summary(
    graph: &tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1,
) -> String {
    format!(
        "native graph {:?}: encoded={}/{} entities and {}/{} relations, \
         durable_pages={} durable_mutations={} verified_segments={}, \
         attempt={}:{}:{} epoch={} last_work_micros={}",
        graph.phase,
        graph.encoded_entities,
        graph
            .total_entities
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string()),
        graph.encoded_relations,
        graph
            .total_relations
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string()),
        graph.committed_stage_pages,
        graph.committed_stage_mutations,
        graph.committed_compact_segments,
        graph.daemon_incarnation,
        graph.producer_incarnation,
        graph.attempt_sequence,
        graph.progress_epoch,
        graph
            .last_progress_micros
            .map_or_else(|| "unreported".to_owned(), |micros| micros.to_string(),)
    )
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
    // Only the owner's exact typed refusal permits a non-Git enrollment to
    // treat reconciliation as inapplicable. A transport or storage failure
    // mentioning the scheduler in its diagnostic is still a real failure.
    let scheduler_unavailable = match error {
        tracedecay_domain::errors::TraceDecayError::ProjectRoute { reason_code, .. } => {
            reason_code == "code_index_scheduler_unavailable"
        }
        tracedecay_domain::errors::TraceDecayError::ToolRefused(refusal) => {
            refusal.code.as_deref() == Some("code_index_scheduler_unavailable")
        }
        _ => false,
    };
    if !scheduler_unavailable {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod daemon_precondition_tests {
    use std::path::Path;
    use std::time::{Duration, Instant};
    use tracedecay_contracts::code_index_freshness::{
        CODE_INDEX_READINESS_WAIT_TIMED_OUT, CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
        CodeGraphServingReadinessV1, CodeIndexBuildBlockedReasonV1, CodeIndexBuildPhaseV1,
        CodeIndexBuildProgressV1, CodeIndexConvergenceParkedV1, CodeIndexFreshnessLadderInputsV1,
        CodeIndexFreshnessLadderV1, CodeIndexNativeGraphPhaseV1, CodeIndexPresealWorkPhaseV1,
        CodeIndexReadinessTargetV1, CodeIndexReadinessV1, CodeIndexReadinessWaitOutcomeV1,
        CodeIndexReadinessWaitReadV1, CodeIndexStalenessStateV1, CodeIndexWorktreeFreshnessV1,
    };
    use tracedecay_contracts::retrieval::StatusCodeIndexFreshnessV1;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_mcp::handlers::info::readiness_wait_outcome;
    use tracedecay_project::project::MovedStoreAdoption;

    #[test]
    fn init_adoption_requires_a_named_identity() {
        assert_eq!(
            super::moved_store_adoption_request(None, false).unwrap(),
            MovedStoreAdoption::Never
        );
        for assume_yes in [false, true] {
            assert_eq!(
                super::moved_store_adoption_request(Some("proj_selected".to_owned()), assume_yes,)
                    .unwrap(),
                MovedStoreAdoption::AdoptNamed("proj_selected".to_owned())
            );
        }
        let error = super::moved_store_adoption_request(None, true).unwrap_err();
        assert!(error.to_string().contains("--adopt-project <id>"));
    }

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

    fn terminal_parked_worktree() -> CodeIndexWorktreeFreshnessV1 {
        let mut worktree = building(false);
        worktree.staleness_state = Some(CodeIndexStalenessStateV1::Parked);
        worktree.parked = Some(CodeIndexConvergenceParkedV1 {
            reason: "corrupt durable publication authority".to_owned(),
            blocked_reason: Some(CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt),
            remediation: "repair the owner-private graph store".to_owned(),
            parked_at_micros: 123,
            observed_passes: 2,
            retries_on_wake: false,
        });
        worktree
    }

    fn readiness_worktree(state: CodeIndexStalenessStateV1) -> CodeIndexWorktreeFreshnessV1 {
        let restoring = state == CodeIndexStalenessStateV1::Restoring;
        let ladder = CodeIndexFreshnessLadderV1::project(CodeIndexFreshnessLadderInputsV1 {
            ready: !restoring,
            refresh_in_flight: state != CodeIndexStalenessStateV1::Fresh,
            restore_in_flight: restoring,
            source_change_pending: false,
            parked: None,
            source_verified: Some(true),
            hook_hint_count: Some(0),
            sources_omitted: false,
        });
        assert!(!ladder.rebuild_in_flight);
        assert_eq!(ladder.staleness_state, state);
        CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/repo".to_owned(),
            latest_generation_id: Some("generation.v1.first".to_owned()),
            code_graph_serving: Some(if restoring {
                CodeGraphServingReadinessV1::Pending
            } else {
                CodeGraphServingReadinessV1::Ready
            }),
            staleness_state: Some(ladder.staleness_state),
            rebuild_in_flight: ladder.rebuild_in_flight,
            coverage: ladder.coverage,
            ..CodeIndexWorktreeFreshnessV1::default()
        }
    }

    #[test]
    fn init_wait_preseal_summary_does_not_report_sealed_work() {
        let attempt =
            tracedecay_contracts::code_index_freshness::CodeIndexPresealAttemptProgressV1 {
                daemon_incarnation: 2,
                producer_incarnation: 5,
                attempt_sequence: 6,
                progress_epoch: 11,
                phase:
                    tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::Capture,
                completed_items: 16,
                total_items: None,
                last_progress_micros: Some(123),
                active: true,
            };
        assert_eq!(
            super::preseal_progress_summary(&attempt),
            "capturing 16/unknown items, attempt=2:5:6 epoch=11 last_work_micros=123"
        );
    }

    fn attempt(
        phase: tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1,
        epoch: u64,
        completed: u64,
        sequence: u64,
    ) -> CodeIndexWorktreeFreshnessV1 {
        let mut result = building(true);
        result.preseal_attempt = Some(
            tracedecay_contracts::code_index_freshness::CodeIndexPresealAttemptProgressV1 {
                daemon_incarnation: 3,
                producer_incarnation: 5,
                attempt_sequence: sequence,
                progress_epoch: epoch,
                phase,
                completed_items: completed,
                total_items: Some(50),
                last_progress_micros: (completed > 0).then_some((completed * 100) as i64),
                active: true,
            },
        );
        result
    }

    #[test]
    fn retained_text_phases_cannot_hide_active_successor_capture() {
        for phase in [
            CodeIndexBuildPhaseV1::Ready,
            CodeIndexBuildPhaseV1::GraphPublication,
            CodeIndexBuildPhaseV1::IndexBuild,
            CodeIndexBuildPhaseV1::Verification,
            CodeIndexBuildPhaseV1::RelationalPreparation,
        ] {
            let mut worktree = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
            worktree.latest_generation_id = Some("generation.prior.sealed".to_owned());
            let mut retained = progress(6_136, None);
            retained.phase = phase;
            worktree.progress = Some(retained);
            assert!(
                matches!(
                    super::FirstGenerationWorkMarker::of(Some(&worktree)),
                    Some(super::FirstGenerationWorkMarker::Preseal {
                        owner: (3, 5, 2),
                        epoch: 4,
                        completed: 7,
                        last_work_micros: Some(700),
                        ..
                    })
                ),
                "successor capture must outrank prior text phase {phase:?}"
            );
        }

        // A real bulk commit can overlap the scoped capture guard. Its
        // advancing durable cursor must still take precedence.
        let mut writing = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        let mut committed = progress(24, None);
        committed.phase = CodeIndexBuildPhaseV1::BulkCommit;
        writing.progress = Some(committed);
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&writing)),
            Some(super::FirstGenerationWorkMarker::CommittedText { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_capture_and_text_receipts_each_renew_real_wait_progress() {
        let mut capturing = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capturing.latest_generation_id = Some("generation.prior.sealed".to_owned());
        let mut retained = progress(40, None);
        retained.daemon_incarnation = 3;
        retained.producer_incarnation = 4;
        retained.phase = CodeIndexBuildPhaseV1::BulkCommit;
        capturing.progress = Some(retained);

        let mut parsing = capturing.clone();
        parsing.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;

        let mut committed = parsing.clone();
        let text = committed.progress.as_mut().unwrap();
        text.progress_epoch += 1;
        text.committed_pages += 1;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capturing)),
                status(timed_out(), warming(parsing)),
                status(timed_out(), warming(committed)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "a frozen text cursor must not hide new parsing, or vice versa: {result:?}"
        );
        assert_eq!(reads, 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_concurrent_writer_reports_still_refuse_before_ready() {
        let mut capturing = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capturing.latest_generation_id = Some("generation.prior.sealed".to_owned());
        let mut retained = progress(40, None);
        retained.daemon_incarnation = 3;
        retained.producer_incarnation = 4;
        retained.phase = CodeIndexBuildPhaseV1::BulkCommit;
        capturing.progress = Some(retained);
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capturing.clone())),
                status(timed_out(), warming(capturing)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "neither producer advanced before the stall");
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(reason.contains("no observed capture, parse, text, or native graph work"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successor_capture_advances_while_prior_native_writer_is_quiet() {
        // The previously sealed graph may still be encoding when a new
        // source pass begins. Both are independently owned, and an unchanged
        // native counter must not conceal genuine successor parse work.
        let mut capturing = native_graph_without_writes();
        capturing.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        let mut parsing = capturing.clone();
        parsing.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capturing)),
                status(timed_out(), warming(parsing)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "real successor parsing must renew the wait despite a silent native writer: {result:?}"
        );
        assert_eq!(reads, 3, "only the reached receipt ends a progressing wait");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_graph_and_successor_attempts_cannot_mask_a_stall() {
        let mut stalled = native_graph_without_writes();
        stalled.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(stalled.clone())),
                status(timed_out(), warming(stalled)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "neither independent writer advanced");
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(reason.contains("no observed capture, parse, text, or native graph work"));
    }

    fn overlapping_native_and_text() -> CodeIndexWorktreeFreshnessV1 {
        let mut worktree = native_graph_without_writes();
        let graph = worktree.native_graph_progress.as_mut().unwrap();
        graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactEncoding);
        graph.encoded_entities = 512;
        graph.total_entities = Some(1_024);
        graph.total_relations = Some(8);
        graph.progress_epoch = 4;
        graph.last_progress_micros = Some(400);

        let mut lexical = progress(35, None);
        lexical.phase = CodeIndexBuildPhaseV1::BulkCommit;
        lexical.daemon_incarnation = 3;
        lexical.producer_incarnation = 4;
        worktree.progress = Some(lexical);
        worktree
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authentic_text_commits_advance_during_quiet_native_encoding() {
        let initial = overlapping_native_and_text();
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&initial)),
            Some(super::FirstGenerationWorkMarker::NativeGraph { .. })
        ));
        let mut next = initial.clone();
        let text = next.progress.as_mut().unwrap();
        text.progress_epoch += 1;
        text.committed_pages += 1;
        text.last_progress_micros += 100;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(initial)),
                status(timed_out(), warming(next)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "a real lexical page receipt must not disappear behind a quiet native writer: {result:?}"
        );
        assert_eq!(reads, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_native_write_and_quiet_preseal_keep_real_text_receipts() {
        let mut initial = native_graph_after_all_encoding();
        initial.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        let mut lexical = progress(35, None);
        lexical.phase = CodeIndexBuildPhaseV1::BulkCommit;
        lexical.daemon_incarnation = 3;
        lexical.producer_incarnation = 4;
        initial.progress = Some(lexical);
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&initial)),
            Some(super::FirstGenerationWorkMarker::Preseal { .. })
        ));
        let mut next = initial.clone();
        let text = next.progress.as_mut().unwrap();
        text.progress_epoch += 1;
        text.committed_pages += 1;
        text.last_progress_micros += 100;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(initial)),
                status(timed_out(), warming(next)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "committed lexical pages must remain observable beside a final native write and successor capture: {result:?}"
        );
        assert_eq!(reads, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_native_write_does_not_hide_independent_text_receipts() {
        for verified in [false, true] {
            let mut first = native_graph_after_all_encoding();
            if verified {
                let graph = first.native_graph_progress.as_mut().unwrap();
                graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified);
                graph.committed_compact_segments = 1;
                graph.committed_compact_bytes = 60_000;
                graph.progress_epoch += 1;
            }
            let mut lexical = progress(35, None);
            lexical.phase = CodeIndexBuildPhaseV1::BulkCommit;
            lexical.daemon_incarnation = 3;
            lexical.producer_incarnation = 4;
            first.progress = Some(lexical);
            assert!(matches!(
                super::FirstGenerationWorkMarker::of(Some(&first)),
                Some(super::FirstGenerationWorkMarker::CommittedText { .. })
            ));

            let mut advanced = first.clone();
            let text = advanced.progress.as_mut().unwrap();
            text.progress_epoch += 1;
            text.committed_pages += 1;
            text.committed_payload_bytes += 1_024;
            let (result, reads) = hold_over(
                vec![
                    status(timed_out(), warming(first.clone())),
                    status(timed_out(), warming(advanced)),
                    ready_status(),
                ],
                Duration::ZERO,
            )
            .await;
            assert!(
                result.is_ok(),
                "real lexical page commits during final native work must reach Ready (verified={verified}): {result:?}"
            );
            assert_eq!(reads, 3);

            // When the independent text writer is no longer committing, the
            // final native container write stays unknown, not a fake stall.
            first.progress.as_mut().unwrap().phase = CodeIndexBuildPhaseV1::Ready;
            assert_eq!(super::FirstGenerationWorkMarker::of(Some(&first)), None);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retained_text_cannot_time_out_an_uninstrumented_native_write() {
        for verified in [false, true] {
            let mut initial = native_graph_after_all_encoding();
            if verified {
                let graph = initial.native_graph_progress.as_mut().unwrap();
                graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified);
                graph.committed_compact_segments = 1;
                graph.committed_compact_bytes = 60_000;
            }
            let mut lexical = progress(35, None);
            lexical.phase = CodeIndexBuildPhaseV1::BulkCommit;
            lexical.daemon_incarnation = 3;
            lexical.producer_incarnation = 4;
            initial.progress = Some(lexical);
            let mut repeated = initial.clone();
            let text = repeated.progress.as_mut().unwrap();
            text.progress_epoch += 1;
            text.last_progress_micros += 1_000;

            let (result, reads) = hold_over(
                vec![
                    status(timed_out(), warming(initial)),
                    status(timed_out(), warming(repeated.clone())),
                    status(timed_out(), warming(repeated)),
                    ready_status(),
                ],
                Duration::ZERO,
            )
            .await;
            assert!(
                result.is_ok(),
                "frozen lexical receipts do not prove a native stall: {result:?}"
            );
            assert_eq!(reads, 4, "only the actual readiness proof ends the wait");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_native_write_does_not_credit_text_epoch_only_replays() {
        let mut initial = native_graph_after_all_encoding();
        let mut lexical = progress(35, None);
        lexical.phase = CodeIndexBuildPhaseV1::BulkCommit;
        lexical.daemon_incarnation = 3;
        lexical.producer_incarnation = 4;
        initial.progress = Some(lexical);
        let mut repeated = initial.clone();
        let text = repeated.progress.as_mut().unwrap();
        text.progress_epoch += 1;
        text.last_progress_micros += 1_000;
        repeated
            .native_graph_progress
            .as_mut()
            .unwrap()
            .progress_epoch += 1;

        // Final native work has no bounded silence evidence. Once that
        // owner retires, the same frozen text receipts must still stall;
        // observing them during the unknown interval cannot credit work.
        let mut retired = repeated.clone();
        retired.native_graph_progress.as_mut().unwrap().active = false;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(initial)),
                status(timed_out(), warming(repeated)),
                status(timed_out(), warming(retired.clone())),
                status(timed_out(), warming(retired)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            reads, 4,
            "frozen text must refuse after native work retires"
        );
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(
            reason.contains("no observed capture, parse, text, or native graph work"),
            "{reason}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn epoch_only_changes_in_all_overlapping_writers_still_stall() {
        let mut initial = overlapping_native_and_text();
        initial.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        let mut repeated = initial.clone();
        repeated
            .native_graph_progress
            .as_mut()
            .unwrap()
            .progress_epoch += 1;
        repeated.progress.as_mut().unwrap().progress_epoch += 1;
        repeated.progress.as_mut().unwrap().last_progress_micros += 100;
        repeated.preseal_attempt.as_mut().unwrap().progress_epoch += 1;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(initial)),
                status(timed_out(), warming(repeated)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "no owner supplied any new completed work");
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(reason.contains("no observed capture, parse, text, or native graph work"));
    }

    fn native_graph_after_all_encoding() -> CodeIndexWorktreeFreshnessV1 {
        let mut worktree = native_graph_without_writes();
        let graph = worktree.native_graph_progress.as_mut().unwrap();
        graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactEncoding);
        graph.encoded_entities = 3;
        graph.total_entities = Some(3);
        graph.encoded_relations = 2;
        graph.total_relations = Some(2);
        graph.progress_epoch = 5;
        graph.last_progress_micros = Some(500);
        worktree
    }

    #[test]
    fn final_graph_write_does_not_hide_an_independent_source_attempt() {
        let finished = native_graph_after_all_encoding();
        // A final compact write has no observable per-row milestones.
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&finished)), None);

        let mut successor = finished.clone();
        successor.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&successor)),
            Some(super::FirstGenerationWorkMarker::Preseal {
                owner: (3, 5, 2),
                epoch: 4,
                completed: 7,
                last_work_micros: Some(700),
                ..
            })
        ));

        // A digest-verified native completion also must not hide the separate
        // source owner. Retiring that owner restores the unknown final phase.
        successor.native_graph_progress.as_mut().unwrap().phase =
            Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified);
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&successor)),
            Some(super::FirstGenerationWorkMarker::Preseal { .. })
        ));
        successor.preseal_attempt.as_mut().unwrap().active = false;
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&successor)), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_native_encoding_cannot_hide_advancing_successor_capture() {
        let mut capture = native_graph_after_all_encoding();
        capture.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;
        let mut parse = capture.clone();
        parse.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture)),
                status(timed_out(), warming(parse)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "real capture and parse must outlive the old final graph write: {result:?}"
        );
        assert_eq!(reads, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_native_encoding_cannot_hide_a_frozen_successor() {
        let mut capture = native_graph_after_all_encoding();
        capture.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2).preseal_attempt;

        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture.clone())),
                status(timed_out(), warming(capture)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "a frozen source must refuse before later Ready");
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(reason.contains("no observed capture, parse, text, or native graph work"));
    }

    #[test]
    fn retired_preseal_does_not_stall_slow_durable_publication() {
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let mut source = attempt(CodeIndexPresealWorkPhaseV1::Resolution, 6, 19, 2);
        source.latest_generation_id = Some("generation.prior.sealed".to_owned());
        let mut retained = progress(6_136, None);
        retained.phase = CodeIndexBuildPhaseV1::Ready;
        source.progress = Some(retained);
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(marker.is_some());
        assert!(!window.observe(marker, now));

        // The scoped attempt was retired at the real publication handoff.
        // An old Ready report is not successor work, even after 120 seconds.
        source.preseal_attempt.as_mut().unwrap().active = false;
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&source)), None);
        assert!(!window.observe(None, now + Duration::from_secs(130)));
        assert!(!window.observe(None, now + Duration::from_secs(260)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_refuses_frozen_capture_even_when_old_generation_is_ready() {
        let mut capture = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capture.latest_generation_id = Some("generation.prior.sealed".to_owned());
        let mut retained = progress(6_136, None);
        retained.phase = CodeIndexBuildPhaseV1::Ready;
        capture.progress = Some(retained);
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture.clone())),
                status(timed_out(), warming(capture)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "frozen capture must refuse before a later ready");
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(
            reason.contains("no observed capture, parse, text, or native graph work"),
            "{reason}"
        );
        assert!(reason.contains("capturing 7/50 items"), "{reason}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successor_parse_progress_survives_old_ready_generation() {
        let mut first = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        first.latest_generation_id = Some("generation.prior.sealed".to_owned());
        let mut retained = progress(6_136, None);
        retained.phase = CodeIndexBuildPhaseV1::Ready;
        first.progress = Some(retained);
        let mut parsed = first.clone();
        parsed.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(first)),
                status(timed_out(), warming(parsed)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "new parse evidence is real work: {result:?}"
        );
        assert_eq!(reads, 3);
    }

    #[test]
    fn active_capture_without_work_is_not_an_infinite_wait() {
        use tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::Capture;
        let mut source = attempt(Capture, 1, 0, 1);
        // Restore/verification may own real pre-seal work without counting
        // as a rebuild in the canonical freshness ladder.
        source.rebuild_in_flight = false;
        let now = Instant::now();
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(marker.is_some());
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        assert!(!window.observe(marker.clone(), now));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(119)));
        assert!(window.observe(marker, now + Duration::from_secs(120)));
    }

    #[test]
    fn real_capture_parse_and_resumed_work_reset_the_silence_window() {
        use tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::{
            Capture, ParseEvidence,
        };
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let capture = attempt(Capture, 2, 3, 1);
        let marker = super::FirstGenerationWorkMarker::of(Some(&capture));
        assert!(!window.observe(marker.clone(), now));
        assert!(!window.observe(marker, now + Duration::from_secs(119)));
        let parsed = attempt(ParseEvidence, 3, 1, 1);
        let marker = super::FirstGenerationWorkMarker::of(Some(&parsed));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(119)));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(238)));
        assert!(window.observe(marker, now + Duration::from_secs(239)));
        let retry = attempt(Capture, 4, 0, 2);
        let marker = super::FirstGenerationWorkMarker::of(Some(&retry));
        assert!(window.observe(marker, now + Duration::from_secs(239)));
        let resumed = attempt(Capture, 5, 1, 2);
        let marker = super::FirstGenerationWorkMarker::of(Some(&resumed));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(240)));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(359)));
        assert!(window.observe(marker, now + Duration::from_secs(360)));
    }

    #[test]
    fn committed_text_can_stall_but_unreported_graph_writes_cannot_fake_a_stall() {
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let mut source = building(true);
        source.progress = Some(progress(10, None));
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(!window.observe(marker.clone(), now));
        assert!(!window.observe(marker, now + Duration::from_secs(119)));
        source.progress = Some(progress(12, None));
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(119)));
        assert!(window.observe(marker, now + Duration::from_secs(239)));
        for phase in [
            CodeIndexBuildPhaseV1::RelationalPreparation,
            CodeIndexBuildPhaseV1::IndexBuild,
            CodeIndexBuildPhaseV1::Verification,
            CodeIndexBuildPhaseV1::GraphPublication,
        ] {
            source.progress.as_mut().unwrap().phase = phase;
            assert_eq!(super::FirstGenerationWorkMarker::of(Some(&source)), None);
        }
        assert!(!window.observe(None, now + Duration::from_secs(1000)));
        source.progress.as_mut().unwrap().phase = CodeIndexBuildPhaseV1::Ready;
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&source)), None);
    }

    #[test]
    fn status_epochs_timestamps_and_recovery_do_not_renew_work() {
        let now = Instant::now();
        let mut source = building(true);
        let mut report = progress(40, None);
        report.phase = CodeIndexBuildPhaseV1::BulkCommit;
        source.progress = Some(report.clone());
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        assert!(!window.observe(super::FirstGenerationWorkMarker::of(Some(&source)), now,));
        for (seconds, blocked) in [
            (40, Some(CodeIndexBuildBlockedReasonV1::ResidentMemory)),
            (80, None),
            (120, Some(CodeIndexBuildBlockedReasonV1::RetryBackoff)),
        ] {
            report.progress_epoch += 1;
            report.last_progress_micros += 100;
            report.elapsed_micros += 100;
            report.current_batch_pages += 1;
            report.current_batch_payload_bytes += 4096;
            report.files_per_second = Some(42.0);
            report.blocked_reason = blocked;
            source.progress = Some(report.clone());
            assert_eq!(
                window.observe(
                    super::FirstGenerationWorkMarker::of(Some(&source)),
                    now + Duration::from_secs(seconds),
                ),
                seconds == 120,
                "metadata and blocked/recovered state are not completed work",
            );
        }
        report.blocked_reason = None;
        report.progress_epoch += 1;
        report.committed_pages += 1;
        report.committed_payload_bytes += 1024;
        source.progress = Some(report);
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(121)));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(240)));
        assert!(window.observe(marker, now + Duration::from_secs(241)));
    }

    #[test]
    fn preseal_phase_only_changes_and_delayed_callbacks_do_not_renew_work() {
        use tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::{
            Capture, ParseEvidence,
        };
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let capture = attempt(Capture, 2, 3, 1);
        assert!(!window.observe(super::FirstGenerationWorkMarker::of(Some(&capture)), now,));
        let entered_parse = attempt(ParseEvidence, 3, 0, 1);
        assert!(!window.observe(
            super::FirstGenerationWorkMarker::of(Some(&entered_parse)),
            now + Duration::from_secs(119),
        ));
        let stale_capture = attempt(Capture, 99, 50, 1);
        assert!(window.observe(
            super::FirstGenerationWorkMarker::of(Some(&stale_capture)),
            now + Duration::from_secs(120),
        ));
        let parsed = attempt(ParseEvidence, 4, 1, 1);
        let marker = super::FirstGenerationWorkMarker::of(Some(&parsed));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(121)));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(240)));
        assert!(window.observe(marker, now + Duration::from_secs(241)));
    }

    #[test]
    fn restart_adoption_is_not_work_and_old_daemon_reports_stay_fenced() {
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let mut source = building(true);
        source.progress = Some(progress(40, None));
        assert!(!window.observe(super::FirstGenerationWorkMarker::of(Some(&source)), now,));
        let mut adopted = progress(40, None);
        adopted.daemon_incarnation = 2;
        adopted.progress_epoch = 1;
        source.progress = Some(adopted.clone());
        assert!(!window.observe(
            super::FirstGenerationWorkMarker::of(Some(&source)),
            now + Duration::from_secs(119),
        ));
        source.progress = Some(progress(500, None));
        assert!(window.observe(
            super::FirstGenerationWorkMarker::of(Some(&source)),
            now + Duration::from_secs(120),
        ));
        adopted.completed_files += 1;
        adopted.committed_pages += 1;
        adopted.progress_epoch += 1;
        source.progress = Some(adopted);
        let marker = super::FirstGenerationWorkMarker::of(Some(&source));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(121)));
        assert!(window.observe(marker, now + Duration::from_secs(241)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_loop_refuses_epoch_only_changes_before_a_later_ready() {
        let mut first = building(true);
        let mut report = progress(40, None);
        report.phase = CodeIndexBuildPhaseV1::BulkCommit;
        first.progress = Some(report.clone());
        let mut unchanged = first.clone();
        report.progress_epoch += 1;
        report.last_progress_micros += 1000;
        report.elapsed_micros += 1000;
        unchanged.progress = Some(report);
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(first)),
                status(timed_out(), warming(unchanged)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "do not consume a later ready after a real stall");
        let (code, message) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(
            message.contains("no observed capture, parse, text, or native graph work"),
            "{message}"
        );
        assert!(message.contains("40/6136 files"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_loop_empty_retries_cannot_hide_frozen_preseal_work() {
        use tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::Capture;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(attempt(Capture, 2, 3, 1))),
                status(timed_out(), warming(attempt(Capture, 3, 0, 2))),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2);
        let (code, message) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(message.contains("attempt=3:5:2"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_loop_requires_work_across_capture_parse_and_text_handoff() {
        use tracedecay_contracts::code_index_freshness::CodeIndexPresealWorkPhaseV1::{
            Capture, ParseEvidence,
        };
        let mut first_text = building(true);
        let mut report = progress(2, None);
        report.daemon_incarnation = 3;
        report.producer_incarnation = 5;
        report.phase = CodeIndexBuildPhaseV1::BulkCommit;
        first_text.progress = Some(report.clone());
        let mut next_text = first_text.clone();
        report.progress_epoch += 1;
        report.committed_chunks = 7;
        report.committed_payload_bytes = 1024;
        next_text.progress = Some(report);
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(attempt(Capture, 2, 3, 1))),
                status(timed_out(), warming(attempt(ParseEvidence, 3, 1, 1))),
                status(timed_out(), warming(first_text)),
                status(timed_out(), warming(next_text)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "real work must keep the wait alive: {result:?}"
        );
        assert_eq!(reads, 5, "only the reached proof ends a progressing wait");
    }

    fn native_graph_without_writes() -> CodeIndexWorktreeFreshnessV1 {
        let mut worktree = building(true);
        worktree.progress = Some(CodeIndexBuildProgressV1 {
            phase: CodeIndexBuildPhaseV1::GraphPublication,
            ..progress(6_136, None)
        });
        worktree.native_graph_progress = Some(
            tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1 {
                generation_id: "generation.v1.first".to_owned(),
                sealed_graph_digest: "sha256:sealed".to_owned(),
                daemon_incarnation: 3,
                producer_incarnation: 5,
                attempt_sequence: 1,
                progress_epoch: 1,
                phase: None,
                encoded_entities: 0,
                total_entities: None,
                encoded_relations: 0,
                total_relations: None,
                committed_stage_pages: 0,
                committed_stage_mutations: 0,
                committed_stage_bytes: 0,
                committed_compact_segments: 0,
                committed_compact_bytes: 0,
                last_progress_micros: None,
                active: true,
            },
        );
        worktree
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_loop_refuses_a_silent_native_writer_before_later_ready() {
        let cold = native_graph_without_writes();
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&cold)),
            Some(super::FirstGenerationWorkMarker::NativeGraph {
                completed: [0, 0, 0, 0, 0, 0],
                ..
            })
        ));
        let mut repeated = cold.clone();
        repeated
            .native_graph_progress
            .as_mut()
            .unwrap()
            .progress_epoch += 1;
        // A changed status epoch without a physical row or durable page
        // must not turn the initial unreported native attempt into progress.
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(cold)),
                status(timed_out(), warming(repeated)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            reads, 2,
            "the frozen writer must refuse before a later ready"
        );
        let (code, reason) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(
            reason.contains("no observed capture, parse, text, or native graph work"),
            "{reason}"
        );
    }

    #[test]
    fn first_native_write_advances_silence_but_parallel_text_commits_are_not_hidden() {
        let mut cold = native_graph_without_writes();
        let mut text = progress(30, None);
        text.phase = CodeIndexBuildPhaseV1::BulkCommit;
        cold.progress = Some(text);
        assert!(matches!(
            super::FirstGenerationWorkMarker::of(Some(&cold)),
            Some(super::FirstGenerationWorkMarker::CommittedText { .. })
        ));

        cold.progress.as_mut().unwrap().phase = CodeIndexBuildPhaseV1::GraphPublication;
        let start = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        let before = super::FirstGenerationWorkMarker::of(Some(&cold));
        assert!(!window.observe(before.clone(), start));
        assert!(!window.observe(before, start + Duration::from_secs(119)));
        let graph = cold.native_graph_progress.as_mut().unwrap();
        graph.encoded_entities = 512;
        graph.total_entities = Some(1024);
        graph.total_relations = Some(4096);
        graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactEncoding);
        graph.progress_epoch += 1;
        graph.last_progress_micros = Some(10);
        let progressed = super::FirstGenerationWorkMarker::of(Some(&cold));
        assert!(!window.observe(progressed.clone(), start + Duration::from_secs(119)));
        assert!(!window.observe(progressed.clone(), start + Duration::from_secs(238)));
        assert!(window.observe(progressed, start + Duration::from_secs(239)));
    }

    #[test]
    fn native_graph_real_counts_refresh_wait_but_duplicate_epochs_and_stale_owners_do_not() {
        let now = Instant::now();
        let mut worktree = building(true);
        worktree.progress = Some(CodeIndexBuildProgressV1 {
            phase: CodeIndexBuildPhaseV1::GraphPublication,
            ..progress(6_136, None)
        });
        let mut graph =
            tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1 {
                generation_id: "generation.v1.first".to_owned(),
                sealed_graph_digest: "sha256:sealed".to_owned(),
                daemon_incarnation: 3,
                producer_incarnation: 4,
                attempt_sequence: 1,
                progress_epoch: 1,
                phase: None,
                encoded_entities: 0,
                total_entities: None,
                encoded_relations: 0,
                total_relations: None,
                committed_stage_pages: 0,
                committed_stage_mutations: 0,
                committed_stage_bytes: 0,
                committed_compact_segments: 0,
                committed_compact_bytes: 0,
                last_progress_micros: None,
                active: true,
            };
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        worktree.native_graph_progress = Some(graph.clone());
        let first = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(!window.observe(first, now));
        graph.encoded_entities = 512;
        graph.total_entities = Some(1_100);
        graph.progress_epoch += 1;
        graph.last_progress_micros = Some(100);
        worktree.native_graph_progress = Some(graph.clone());
        let advanced = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(!window.observe(advanced, now + Duration::from_secs(119)));
        graph.progress_epoch += 1;
        // A phase or progress-epoch update without accepted rows is not work.
        worktree.native_graph_progress = Some(graph.clone());
        let unchanged = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(window.observe(unchanged, now + Duration::from_secs(239)));
        graph.committed_stage_pages = 1;
        graph.committed_stage_mutations = 100;
        graph.progress_epoch += 1;
        worktree.native_graph_progress = Some(graph.clone());
        let committed = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(!window.observe(committed, now + Duration::from_secs(239)));
        graph.daemon_incarnation = 2;
        graph.progress_epoch += 1;
        graph.committed_stage_pages = 100;
        worktree.native_graph_progress = Some(graph);
        let stale = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(window.observe(stale, now + Duration::from_secs(359)));
    }

    #[test]
    fn final_compact_write_is_uninstrumented_not_a_false_stall() {
        let mut worktree = building(true);
        worktree.progress = Some(CodeIndexBuildProgressV1 {
            phase: CodeIndexBuildPhaseV1::GraphPublication,
            ..progress(6_136, None)
        });
        let mut graph =
            tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1 {
                generation_id: "generation.v1.first".to_owned(),
                sealed_graph_digest: "sha256:sealed".to_owned(),
                daemon_incarnation: 3,
                producer_incarnation: 5,
                attempt_sequence: 2,
                progress_epoch: 7,
                phase: Some(CodeIndexNativeGraphPhaseV1::CompactEncoding),
                encoded_entities: 512,
                total_entities: Some(1024),
                encoded_relations: 0,
                total_relations: None,
                committed_stage_pages: 0,
                committed_stage_mutations: 0,
                committed_stage_bytes: 0,
                committed_compact_segments: 0,
                committed_compact_bytes: 0,
                last_progress_micros: Some(20),
                active: true,
            };
        worktree.native_graph_progress = Some(graph.clone());
        assert!(super::FirstGenerationWorkMarker::of(Some(&worktree)).is_some());
        graph.encoded_entities = 1024;
        graph.total_relations = Some(0);
        // The manifest declared zero relations, so completed entity encoding
        // really does precede the uninstrumented final container write.
        worktree.native_graph_progress = Some(graph.clone());
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&worktree)), None);
        graph.phase = Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified);
        graph.committed_compact_segments = 1;
        graph.committed_compact_bytes = 50_000;
        worktree.native_graph_progress = Some(graph);
        assert_eq!(super::FirstGenerationWorkMarker::of(Some(&worktree)), None);
    }

    /// A completed entity stream is not an encoding-complete proof when the
    /// immutable manifest still has relations. Waiting must expose a stalled
    /// first relation window instead of silently dropping its work marker.
    #[test]
    fn completed_entities_cannot_hide_a_stalled_relation_encoder() {
        let mut worktree = building(true);
        worktree.progress = Some(CodeIndexBuildProgressV1 {
            phase: CodeIndexBuildPhaseV1::GraphPublication,
            ..progress(6_136, None)
        });
        let mut graph =
            tracedecay_contracts::code_index_freshness::CodeIndexNativeGraphProgressV1 {
                generation_id: "generation.v1.first".to_owned(),
                sealed_graph_digest: "sha256:sealed".to_owned(),
                daemon_incarnation: 3,
                producer_incarnation: 5,
                attempt_sequence: 3,
                progress_epoch: 4,
                phase: Some(CodeIndexNativeGraphPhaseV1::CompactEncoding),
                encoded_entities: 1_024,
                total_entities: Some(1_024),
                encoded_relations: 0,
                total_relations: Some(10_000),
                committed_stage_pages: 0,
                committed_stage_mutations: 0,
                committed_stage_bytes: 0,
                committed_compact_segments: 0,
                committed_compact_bytes: 0,
                last_progress_micros: Some(1_000),
                active: true,
            };
        worktree.native_graph_progress = Some(graph.clone());
        let marker = super::FirstGenerationWorkMarker::of(Some(&worktree));
        assert!(
            marker.is_some(),
            "a pending relation stream is still observable work"
        );
        let now = Instant::now();
        let mut window = super::FirstGenerationProgressWindow::new(Duration::from_secs(120));
        assert!(!window.observe(marker.clone(), now));
        assert!(!window.observe(marker.clone(), now + Duration::from_secs(119)));
        assert!(window.observe(marker, now + Duration::from_secs(120)));

        graph.encoded_relations = 10_000;
        graph.progress_epoch += 1;
        worktree.native_graph_progress = Some(graph);
        assert_eq!(
            super::FirstGenerationWorkMarker::of(Some(&worktree)),
            None,
            "only both completed streams may enter the uninstrumented final write"
        );
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

    fn pending_status(worktree: CodeIndexWorktreeFreshnessV1) -> serde_json::Value {
        // `ready` in the ladder means a serving generation exists. Ready
        // waits also require authoritative source freshness and a settled
        // owner, so even verification with an activated graph is pending.
        assert_eq!(
            worktree.readiness(CodeIndexReadinessTargetV1::Ready),
            CodeIndexReadinessV1::Pending
        );
        let outcome = readiness_wait_outcome(CodeIndexReadinessWaitReadV1::TimedOut {
            last: Some(Box::new(worktree.clone())),
        });
        let CodeIndexReadinessWaitOutcomeV1::TimedOut { ref last_state } = outcome else {
            panic!("a timed-out wait retains its last state");
        };
        serde_json::json!({
            "code_index_freshness": {"status": last_state, "worktree": worktree},
            "wait": outcome,
        })
    }

    fn ready_status() -> serde_json::Value {
        let worktree = readiness_worktree(CodeIndexStalenessStateV1::Fresh);
        assert_eq!(
            worktree.readiness(CodeIndexReadinessTargetV1::Ready),
            CodeIndexReadinessV1::Reached
        );
        status(
            readiness_wait_outcome(CodeIndexReadinessWaitReadV1::Reached {
                reading: Box::new(worktree.clone()),
            }),
            Some(StatusCodeIndexFreshnessV1::Current {
                worktree: Box::new(worktree),
            }),
        )
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
    async fn first_generation_wait_holds_while_restoring_without_build_progress() {
        let restoring = pending_status(readiness_worktree(CodeIndexStalenessStateV1::Restoring));
        let (result, reads) = hold_over(
            vec![restoring.clone(), restoring, ready_status()],
            Duration::ZERO,
        )
        .await;
        assert!(result.is_ok(), "restore owns readiness work: {result:?}");
        assert_eq!(reads, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_generation_wait_holds_while_verifying_without_build_progress() {
        let verifying = pending_status(readiness_worktree(CodeIndexStalenessStateV1::Verifying));
        let (result, reads) = hold_over(
            vec![verifying.clone(), verifying, ready_status()],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "verification owns readiness work: {result:?}"
        );
        assert_eq!(reads, 3);
    }

    #[test]
    fn first_generation_idle_window_restarts_after_readiness_work() {
        let started = Instant::now();
        let idle = building(false);
        let mut uninterrupted =
            super::FirstGenerationIdleWindow::new(super::INIT_WAIT_STALL_WINDOW);
        for seconds in [0, 119] {
            assert_eq!(
                uninterrupted.observe(
                    super::FirstGenerationStall::of(Ok(&idle)),
                    started + Duration::from_secs(seconds),
                ),
                None
            );
        }
        assert_eq!(
            uninterrupted.observe(
                super::FirstGenerationStall::of(Ok(&idle)),
                started + Duration::from_secs(120),
            ),
            Some(super::FirstGenerationStall::NoPassInFlight)
        );

        for active in [
            readiness_worktree(CodeIndexStalenessStateV1::Restoring),
            readiness_worktree(CodeIndexStalenessStateV1::Verifying),
            building(true),
        ] {
            let mut window = super::FirstGenerationIdleWindow::new(super::INIT_WAIT_STALL_WINDOW);
            let mut observe = |worktree, seconds| {
                window.observe(
                    super::FirstGenerationStall::of(Ok(worktree)),
                    started + Duration::from_secs(seconds),
                )
            };
            assert_eq!(observe(&idle, 0), None);
            assert_eq!(observe(&idle, 119), None);
            assert_eq!(observe(&active, 120), None);
            // Capture or authentication may legitimately report no progress
            // for longer than a status slice or the idle window.
            assert_eq!(observe(&active, 1_515), None);
            assert_eq!(observe(&idle, 1_545), None);
            assert_eq!(observe(&idle, 1_664), None);
            assert_eq!(
                observe(&idle, 1_665),
                Some(super::FirstGenerationStall::NoPassInFlight)
            );
        }
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

    #[tokio::test(flavor = "current_thread")]
    async fn unavailable_parked_wait_preserves_the_daemon_operator_remedy() {
        let parked = terminal_parked_worktree();
        let (result, reads) = hold_over(
            vec![
                status(
                    CodeIndexReadinessWaitOutcomeV1::Unavailable {
                        reason: "code_index_convergence_parked".to_owned(),
                    },
                    Some(StatusCodeIndexFreshnessV1::Parked {
                        worktree: Box::new(parked),
                    }),
                ),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 1, "unavailable cannot claim a later Ready");
        let (code, message) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_UNAVAILABLE);
        assert!(
            message.contains("code_index_convergence_parked"),
            "{message}"
        );
        assert!(
            message.contains("corrupt durable publication authority"),
            "{message}"
        );
        assert!(
            message.contains("repair the owner-private graph store"),
            "{message}"
        );
        assert!(message.contains("retries on wake: false"), "{message}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timed_out_terminal_park_reports_cause_even_with_stale_rebuild_flag() {
        let mut parked = terminal_parked_worktree();
        parked.rebuild_in_flight = true;
        let (result, reads) = hold_over(
            vec![
                status(
                    timed_out(),
                    Some(StatusCodeIndexFreshnessV1::Parked {
                        worktree: Box::new(parked),
                    }),
                ),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 1);
        let (code, message) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(message.contains("convergence is parked"), "{message}");
        assert!(
            message.contains("corrupt durable publication authority"),
            "{message}"
        );
        assert!(
            message.contains("repair the owner-private graph store"),
            "{message}"
        );
    }

    #[test]
    fn terminal_park_is_fenced_from_a_live_successor_and_recovery() {
        let mut parked = terminal_parked_worktree();
        parked.rebuild_in_flight = true;
        let expected = super::FirstGenerationStall::Parked {
            cause: "corrupt durable publication authority".to_owned(),
            remedy: "repair the owner-private graph store".to_owned(),
        };
        assert_eq!(
            super::FirstGenerationStall::of(Ok(&parked)),
            Some(expected.clone())
        );

        // A stale park cannot block a separately admitted live successor.
        let mut recovered = parked.clone();
        recovered.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::Capture, 2, 1, 5).preseal_attempt;
        assert_eq!(super::FirstGenerationStall::of(Ok(&recovered)), None);

        let started = Instant::now();
        let mut window = super::FirstGenerationIdleWindow::new(Duration::from_secs(120));
        assert_eq!(window.observe(Some(expected.clone()), started), None);
        assert_eq!(
            window.observe(Some(expected.clone()), started + Duration::from_secs(119)),
            None
        );
        assert_eq!(
            window.observe(
                super::FirstGenerationStall::of(Ok(&recovered)),
                started + Duration::from_secs(120),
            ),
            None
        );
        assert_eq!(
            window.observe(Some(expected.clone()), started + Duration::from_secs(240)),
            None
        );
        assert_eq!(
            window.observe(Some(expected.clone()), started + Duration::from_secs(360)),
            Some(expected)
        );
    }

    #[test]
    fn active_readiness_owner_does_not_mask_blocked_progress() {
        for mut worktree in [
            readiness_worktree(CodeIndexStalenessStateV1::Restoring),
            readiness_worktree(CodeIndexStalenessStateV1::Verifying),
        ] {
            worktree.progress = Some(progress(
                40,
                Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
            ));
            assert_eq!(
                super::FirstGenerationStall::of(Ok(&worktree)),
                Some(super::FirstGenerationStall::Blocked(
                    CodeIndexBuildBlockedReasonV1::ResidentMemory
                ))
            );
        }
    }

    #[test]
    fn retained_text_refusal_cannot_block_a_different_active_writer() {
        let mut successor = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        successor.latest_generation_id = Some("generation.v1.first".to_owned());
        let mut retained = progress(6_136, Some(CodeIndexBuildBlockedReasonV1::ResidentMemory));
        retained.phase = CodeIndexBuildPhaseV1::Ready;
        successor.progress = Some(retained);
        assert_eq!(
            super::FirstGenerationStall::of(Ok(&successor)),
            None,
            "a blocked, retained Ready report must not refuse active successor capture"
        );

        let mut graph = native_graph_without_writes();
        let mut old_text = progress(6_136, Some(CodeIndexBuildBlockedReasonV1::RetryBackoff));
        old_text.phase = CodeIndexBuildPhaseV1::GraphPublication;
        graph.progress = Some(old_text);
        graph.native_graph_progress.as_mut().unwrap().generation_id =
            "generation.successor".to_owned();
        assert_eq!(
            super::FirstGenerationStall::of(Ok(&graph)),
            None,
            "another generation's blocked text cannot refuse a live native writer"
        );
        graph.native_graph_progress.as_mut().unwrap().generation_id =
            "generation.v1.first".to_owned();
        assert_eq!(
            super::FirstGenerationStall::of(Ok(&graph)),
            Some(super::FirstGenerationStall::Blocked(
                CodeIndexBuildBlockedReasonV1::RetryBackoff
            )),
            "a genuine same-generation refusal must still be observable"
        );
    }

    #[test]
    fn live_writer_owns_readiness_even_without_a_rebuild_flag() {
        let now = Instant::now();
        let mut capture = attempt(CodeIndexPresealWorkPhaseV1::Capture, 3, 1, 1);
        capture.rebuild_in_flight = false;
        let mut graph = native_graph_without_writes();
        graph.rebuild_in_flight = false;
        for active in [&capture, &graph] {
            assert_eq!(super::FirstGenerationStall::of(Ok(active)), None);
            let mut window = super::FirstGenerationIdleWindow::new(Duration::from_secs(120));
            assert_eq!(
                window.observe(super::FirstGenerationStall::of(Ok(active)), now),
                None
            );
            assert_eq!(
                window.observe(
                    super::FirstGenerationStall::of(Ok(active)),
                    now + Duration::from_secs(240)
                ),
                None,
                "a real active writer is not an absent reconcile pass"
            );
        }
        capture.preseal_attempt.as_mut().unwrap().active = false;
        assert_eq!(
            super::FirstGenerationStall::of(Ok(&capture)),
            Some(super::FirstGenerationStall::NoPassInFlight),
            "retiring the attempt restores no-pass stall detection"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_survives_stale_blocked_text_while_successor_parses() {
        let mut capture = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capture.latest_generation_id = Some("generation.v1.first".to_owned());
        let mut prior = progress(6_136, Some(CodeIndexBuildBlockedReasonV1::ResidentMemory));
        prior.phase = CodeIndexBuildPhaseV1::Ready;
        capture.progress = Some(prior);
        let mut parsed = capture.clone();
        parsed.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture)),
                status(timed_out(), warming(parsed)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "old blocked generation must not refuse new work: {result:?}"
        );
        assert_eq!(reads, 3, "only the actual ready receipt ends a moving wait");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_retained_bulk_commit_cannot_hide_successor_parse_work() {
        // A new preseal attempt has not sealed an immutable generation yet.
        // An older generation's blocked bulk writer must not suppress this
        // separately owned successor's real capture and parse observations.
        let mut capture = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capture.latest_generation_id = Some("generation.v1.first".to_owned());
        let mut old_text = progress(40, Some(CodeIndexBuildBlockedReasonV1::ResidentMemory));
        old_text.phase = CodeIndexBuildPhaseV1::BulkCommit;
        capture.progress = Some(old_text);
        assert_eq!(super::FirstGenerationStall::of(Ok(&capture)), None);

        let mut parsed = capture.clone();
        parsed.preseal_attempt =
            attempt(CodeIndexPresealWorkPhaseV1::ParseEvidence, 5, 1, 2).preseal_attempt;
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture)),
                status(timed_out(), warming(parsed)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(
            result.is_ok(),
            "old blocked bulk commits must not refuse advancing successor work: {result:?}"
        );
        assert_eq!(reads, 3, "only a real ready receipt ends the wait");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_retained_bulk_commit_does_not_hide_a_frozen_successor() {
        let mut capture = attempt(CodeIndexPresealWorkPhaseV1::Capture, 4, 7, 2);
        capture.latest_generation_id = Some("generation.v1.first".to_owned());
        let mut old_text = progress(40, Some(CodeIndexBuildBlockedReasonV1::ResidentMemory));
        old_text.phase = CodeIndexBuildPhaseV1::BulkCommit;
        capture.progress = Some(old_text);
        let (result, reads) = hold_over(
            vec![
                status(timed_out(), warming(capture.clone())),
                status(timed_out(), warming(capture)),
                ready_status(),
            ],
            Duration::ZERO,
        )
        .await;
        assert_eq!(reads, 2, "neither retained text nor new capture advanced");
        let (code, message) = refusal(result);
        assert_eq!(code, CODE_INDEX_READINESS_WAIT_TIMED_OUT);
        assert!(
            message.contains("no observed capture, parse, text, or native graph work"),
            "{message}"
        );
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
        let mut refused = readiness_worktree(CodeIndexStalenessStateV1::Verifying);
        refused.code_graph_serving = Some(CodeGraphServingReadinessV1::Refused {
            reason: "graph activation failed".to_owned(),
        });
        let CodeIndexReadinessV1::Unreachable { reason } =
            refused.readiness(CodeIndexReadinessTargetV1::Ready)
        else {
            panic!("a graph refusal must make a ready wait unreachable");
        };
        for payload in [
            status(
                readiness_wait_outcome(CodeIndexReadinessWaitReadV1::Unreachable { reason }),
                Some(StatusCodeIndexFreshnessV1::Stale {
                    worktree: Box::new(refused),
                }),
            ),
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
            client_instance_id: "0000000000000000000000000000c002".to_string(),
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
        let unavailable = tracedecay_domain::errors::TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "admin sync was not accepted",
        );
        let tool_unavailable = tracedecay_domain::errors::TraceDecayError::tool_refused(
            "tracedecay_admin_sync",
            Some("code_index_scheduler_unavailable".to_owned()),
            Some("scheduler is not mounted".to_owned()),
        );
        let unrelated = tracedecay_domain::errors::TraceDecayError::project_route(
            "storage_unavailable",
            true,
            "storage has not mounted",
        );

        assert!(code_index_reconciliation_is_optional(&non_git, &unavailable).await);
        assert!(code_index_reconciliation_is_optional(&non_git, &tool_unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&nested_git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&non_git, &unrelated).await);
    }

    #[tokio::test]
    async fn scheduler_words_in_other_errors_cannot_make_init_succeed() {
        let temp = tempfile::TempDir::new().unwrap();
        let non_git = temp.path().join("non-git");
        std::fs::create_dir_all(&non_git).unwrap();
        let incidental = "code_index_scheduler_unavailable appeared in the diagnostic";
        for error in [
            tracedecay_domain::errors::TraceDecayError::Config {
                message: incidental.to_owned(),
            },
            tracedecay_domain::errors::TraceDecayError::project_route(
                "storage_unavailable",
                true,
                incidental,
            ),
            tracedecay_domain::errors::TraceDecayError::tool_refused(
                "tracedecay_admin_sync",
                None,
                Some(incidental.to_owned()),
            ),
            tracedecay_domain::errors::TraceDecayError::tool_refused(
                "tracedecay_admin_sync",
                Some("storage_unavailable".to_owned()),
                Some(incidental.to_owned()),
            ),
        ] {
            assert!(
                !code_index_reconciliation_is_optional(&non_git, &error).await,
                "an unrelated failure must not become successful init: {error}"
            );
        }
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
