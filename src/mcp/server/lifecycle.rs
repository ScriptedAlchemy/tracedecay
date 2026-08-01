//! Server lifecycle maintenance: startup catch-up, staleness-driven
//! sync-on-read, branch-drift reopen, and version-update checks.

use super::*;

/// Cache duration for version checks (15 minutes).
const VERSION_CHECK_INTERVAL: Duration = Duration::from_mins(15);
const STARTUP_TRANSCRIPT_INGEST_ABORT_DEADLINE: Duration = Duration::from_secs(2);

async fn join_or_abort_startup_ingest(
    mut task: tokio::task::JoinHandle<()>,
    deadline: Duration,
) -> bool {
    if tokio::time::timeout(deadline, &mut task).await.is_ok() {
        return true;
    }
    task.abort();
    let _ = task.await;
    false
}

/// Retained startup task handles, carried by the phases that can still own
/// one. Both are joined (or aborted) by shutdown before database authorities
/// are released.
#[derive(Default)]
pub(crate) struct StartupCatchUpTasksV1 {
    sync: Option<tokio::task::JoinHandle<()>>,
    ingest: Option<tokio::task::JoinHandle<()>>,
}

/// The startup catch-up lifecycle as one linear machine.
///
/// This replaces six independently mutable fields (two completion
/// `AtomicBool`s, a dispatch `AtomicBool`, two task-handle mutexes, and the
/// ingest cancellation) whose only valid combinations were these phases.
/// The hazard that motivated the change: the completion flags defaulted to
/// `true` so a server with no catch-up reported "settled", which forced the
/// dispatch site to pre-clear them in a separate store *before* spawning —
/// an ordering that was documented rather than enforced. Here, dispatch
/// *is* the transition into [`Self::Syncing`], so no window exists in which
/// a dispatched catch-up still reads as settled.
pub(crate) enum StartupCatchUpStateV1 {
    /// No catch-up was ever dispatched (session-start sync disabled, or a
    /// construction path that opts out). Terminal, and *ready*: waiters must
    /// not block on work that will never run.
    NotStarted,
    /// The synchronous index sync is running.
    Syncing { tasks: StartupCatchUpTasksV1 },
    /// The index sync finished; the detached transcript ingest is in flight.
    Ingesting { tasks: StartupCatchUpTasksV1 },
    /// Both phases finished — including the failure paths, which settle
    /// rather than stranding waiters.
    Settled { tasks: StartupCatchUpTasksV1 },
    /// Shutdown tore the machine down. Ready, so a shutdown can never leave
    /// a waiter blocked on a task that was just aborted.
    Cancelled,
}

impl StartupCatchUpStateV1 {
    /// True once the *synchronous* index-sync phase can no longer be
    /// pending — the old `startup_catch_up_done` flag.
    const fn sync_phase_settled(&self) -> bool {
        !matches!(self, Self::Syncing { .. })
    }

    /// True once the detached transcript ingest can no longer be pending —
    /// the old `transcript_ingest_done` flag.
    const fn ingest_phase_settled(&self) -> bool {
        matches!(
            self,
            Self::NotStarted | Self::Settled { .. } | Self::Cancelled
        )
    }

    fn tasks_mut(&mut self) -> Option<&mut StartupCatchUpTasksV1> {
        match self {
            Self::Syncing { tasks } | Self::Ingesting { tasks } | Self::Settled { tasks } => {
                Some(tasks)
            }
            Self::NotStarted | Self::Cancelled => None,
        }
    }

    fn take_tasks(&mut self) -> StartupCatchUpTasksV1 {
        self.tasks_mut().map(std::mem::take).unwrap_or_default()
    }
}

/// Owns the startup catch-up state plus the ingest cancellation that the
/// detached task honours.
///
/// Held behind an `Arc` on the server so the spawned ingest task can signal
/// completion through the same lock the waiters read, instead of through a
/// separate `Arc<AtomicBool>` that could disagree with the retained handle.
/// The lock is a `std::sync::Mutex` on purpose: every critical section is a
/// phase swap or a handle take, and joins always happen *outside* it, so the
/// sync readiness accessors stay callable from non-async code.
pub(crate) struct StartupCatchUpMachineV1 {
    state: std::sync::Mutex<StartupCatchUpStateV1>,
    /// Set once the first dispatch claims the machine. Kept distinct from
    /// the phase so a completed catch-up still refuses a second dispatch.
    dispatched: std::sync::atomic::AtomicBool,
    cancellation: crate::application::observation::ObservationCancellation,
}

impl Default for StartupCatchUpMachineV1 {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(StartupCatchUpStateV1::NotStarted),
            dispatched: std::sync::atomic::AtomicBool::new(false),
            cancellation: crate::application::observation::ObservationCancellation::default(),
        }
    }
}

impl StartupCatchUpMachineV1 {
    fn state(&self) -> std::sync::MutexGuard<'_, StartupCatchUpStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn cancellation(&self) -> &crate::application::observation::ObservationCancellation {
        &self.cancellation
    }

    /// One-shot dispatch claim. The first caller wins and the machine enters
    /// [`StartupCatchUpStateV1::Syncing`] in the same critical section, so
    /// there is no interval in which a dispatched catch-up reads as settled.
    pub(crate) fn try_claim_dispatch(&self) -> bool {
        if self
            .dispatched
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return false;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Syncing { tasks };
        true
    }

    /// Enters the synchronous phase for a direct
    /// [`McpServer::run_startup_catch_up_sync`] call. Idempotent for the
    /// dispatched path, which is already `Syncing`. A cancelled machine
    /// stays cancelled: shutdown has already released what this phase needs.
    fn begin_sync(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Syncing { tasks };
    }

    /// Index sync finished; the detached ingest is about to be spawned.
    /// Called *before* the spawn so the ingest task can never settle a
    /// machine that still claims to be syncing.
    fn enter_ingesting(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Ingesting { tasks };
    }

    /// Both phases are done. Used by the ingest task on every exit path and
    /// by the index-sync failure path, so a failure never strands waiters.
    fn settle(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Settled { tasks };
    }

    pub(super) fn install_sync_task(&self, task: tokio::task::JoinHandle<()>) {
        let mut state = self.state();
        match state.tasks_mut() {
            Some(tasks) => tasks.sync = Some(task),
            // Shutdown won the race; nothing will ever join this handle.
            None => task.abort(),
        }
    }

    fn install_ingest_task(&self, task: tokio::task::JoinHandle<()>) {
        let mut state = self.state();
        match state.tasks_mut() {
            Some(tasks) => tasks.ingest = Some(task),
            None => task.abort(),
        }
    }

    fn take_sync_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.state().tasks_mut().and_then(|tasks| tasks.sync.take())
    }

    fn take_ingest_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.state()
            .tasks_mut()
            .and_then(|tasks| tasks.ingest.take())
    }

    /// Shutdown abandoned the index-sync phase: it is no longer pending,
    /// but the ingest teardown below still has to run.
    fn abandon_sync_phase(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Syncing { .. }) {
            let tasks = state.take_tasks();
            *state = StartupCatchUpStateV1::Ingesting { tasks };
        }
    }

    /// Terminal shutdown state. Both phases read as settled so no waiter
    /// blocks on work that was just aborted.
    fn mark_cancelled(&self) {
        *self.state() = StartupCatchUpStateV1::Cancelled;
    }

    fn sync_phase_settled(&self) -> bool {
        self.state().sync_phase_settled()
    }

    fn ingest_phase_settled(&self) -> bool {
        self.state().ingest_phase_settled()
    }
}

/// Phase transitions exposed to the sibling test module, which asserts the
/// machine's invariants directly rather than by racing a live server.
#[cfg(test)]
impl StartupCatchUpMachineV1 {
    /// True once dispatch has been claimed — the old
    /// `startup_catch_up_started` flag.
    pub(super) fn dispatch_claimed(&self) -> bool {
        self.dispatched.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn sync_phase_settled_for_test(&self) -> bool {
        self.sync_phase_settled()
    }

    pub(super) fn ingest_phase_settled_for_test(&self) -> bool {
        self.ingest_phase_settled()
    }

    pub(super) fn enter_ingesting_for_test(&self) {
        self.enter_ingesting();
    }

    pub(super) fn settle_for_test(&self) {
        self.settle();
    }

    pub(super) fn mark_cancelled_for_test(&self) {
        self.mark_cancelled();
    }
}

/// Cached result of a latest-version check against GitHub releases.
pub(crate) struct VersionCheckState {
    pub(crate) latest: Option<String>,
    pub(crate) checked_at: Option<Instant>,
}

/// Owns response admission, revocation, and forced cancellation for one
/// daemon-retained project server.
#[derive(Clone)]
pub(crate) struct ProjectServerResponseLifecycle {
    response_gate: Arc<tokio::sync::RwLock<()>>,
    response_revoked: crate::application::context::CancellationToken,
    request_abort: crate::application::context::CancellationToken,
}

impl Default for ProjectServerResponseLifecycle {
    fn default() -> Self {
        Self {
            response_gate: Arc::new(tokio::sync::RwLock::new(())),
            response_revoked: crate::application::context::CancellationToken::new(),
            request_abort: crate::application::context::CancellationToken::new(),
        }
    }
}

impl ProjectServerResponseLifecycle {
    pub(crate) fn revoke(&self) {
        self.response_revoked.cancel();
    }

    pub(crate) async fn wait_for_request_drain(&self) {
        let _guard = self.response_gate.write().await;
    }

    pub(crate) fn abort_requests(&self) {
        self.request_abort.cancel();
    }

    pub(crate) fn response_gate(&self) -> &Arc<tokio::sync::RwLock<()>> {
        &self.response_gate
    }

    pub(crate) fn response_revoked(&self) -> &crate::application::context::CancellationToken {
        &self.response_revoked
    }

    pub(crate) fn request_abort(&self) -> &crate::application::context::CancellationToken {
        &self.request_abort
    }
}

/// Runs the project and user transcript portions of startup recovery against
/// daemon-retained authorities. Project recovery is independent: a missing
/// user or registry authority skips only the user sweep.
fn log_startup_transcript_ingest_failure(
    scope: &str,
    failure: &crate::sessions::TranscriptCatchUpFailure,
) {
    tracing::warn!(
        scope,
        provider = failure.provider,
        source = failure.source,
        reason_code = failure.reason_code,
        retryable = failure.retryable,
        source_offset = ?failure.source_locator.map(tracedecay_domain::ObservationSourceRangeV1::start),
        source_end_offset = ?failure.source_locator.map(tracedecay_domain::ObservationSourceRangeV1::end),
        "startup transcript ingest incomplete"
    );
}

/// What one startup catch-up pass actually did, per scope.
///
/// Both fields report observed outcomes, never intent: the user scope in
/// particular is skipped by several paths (missing authority, an early
/// return before the sweep, cancellation, or session storage with no profile
/// root), and callers that wake the temporal refresh scheduler must not fire
/// on a sweep that never ran.
#[derive(Default)]
pub(super) struct StartupSessionCatchUpOutcome {
    /// The project session authority, present only when the project sweep
    /// completed successfully.
    pub(super) project_sessions: Option<Arc<RegisteredGlobalDb>>,
    /// True only when the user transcript sweep actually ran to completion.
    pub(super) user_sweep_completed: bool,
}

pub(super) async fn run_startup_session_catch_up(
    sessions: Option<Arc<RegisteredGlobalDb>>,
    user_sessions: Option<Arc<RegisteredGlobalDb>>,
    registry_db: Option<Arc<RegisteredGlobalDb>>,
    profile_identity: Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    project_root: &Path,
    project_id: Option<&str>,
    cancellation: &crate::application::observation::ObservationCancellation,
) -> StartupSessionCatchUpOutcome {
    let Some(sessions) = sessions else {
        tracing::warn!(
            project_root = %project_root.display(),
            "startup project transcript ingest skipped because authoritative session storage is unavailable"
        );
        return StartupSessionCatchUpOutcome::default();
    };
    let Some(profile_identity) = profile_identity else {
        tracing::warn!(
            project_root = %project_root.display(),
            "startup transcript ingest skipped because durable profile identity is unavailable"
        );
        return StartupSessionCatchUpOutcome::default();
    };
    let project_id = project_id.and_then(|id| tracedecay_domain::ProjectId::new(id).ok());
    // Build the authority over an owned `Arc` rather than a borrow: a
    // lifetime-free authority type keeps the downstream auto-trait obligation
    // first-order, which is what lets the spawned startup future prove `Send`.
    let project_authority =
        crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(&sessions));
    let project_outcome = crate::sessions::ingest_project_sources_for_provider_with_cancellation(
        profile_identity.brain_id(),
        profile_identity.profile_id(),
        &project_authority,
        project_root,
        project_id,
        None,
        true,
        cancellation,
    )
    .await;
    for failure in &project_outcome.failures {
        log_startup_transcript_ingest_failure("project", failure);
    }
    if cancellation.is_cancelled() {
        return StartupSessionCatchUpOutcome::default();
    }
    let mut user_sweep_completed = false;
    if let (Some(user_sessions), Some(registry_db)) = (user_sessions, registry_db) {
        if let Some(profile_root) = user_sessions.db_path().parent() {
            let user_authority =
                crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(&user_sessions));
            let registry_authority =
                crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(&registry_db));
            let outcome = crate::sessions::ingest_user_global_sources_for_startup_with_db(
                profile_identity.brain_id(),
                profile_identity.profile_id(),
                &user_authority,
                &registry_authority,
                profile_root,
                cancellation,
            )
            .await;
            for failure in &outcome.failures {
                log_startup_transcript_ingest_failure("user", failure);
            }
            user_sweep_completed = true;
        } else {
            tracing::warn!(
                "startup user transcript ingest skipped because session storage has no profile root"
            );
        }
    } else {
        tracing::warn!(
            "startup user transcript ingest skipped because session or registry storage is unavailable"
        );
    }
    StartupSessionCatchUpOutcome {
        project_sessions: project_outcome.is_success().then_some(sessions),
        user_sweep_completed,
    }
}

async fn run_startup_session_catch_up_with_home(
    transcript_source_home: Option<PathBuf>,
    sessions: Option<Arc<RegisteredGlobalDb>>,
    user_sessions: Option<Arc<RegisteredGlobalDb>>,
    registry_db: Option<Arc<RegisteredGlobalDb>>,
    profile_identity: Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    project_root: PathBuf,
    project_id: Option<String>,
    cancellation: crate::application::observation::ObservationCancellation,
) -> StartupSessionCatchUpOutcome {
    // Own every capture inside the future passed to
    // `with_transcript_source_home`: `task_local::scope` returns
    // `impl Future + Send`, and the auto-trait leak check cannot prove Send
    // "general enough" while the wrapped future's type borrows these locals
    // (E0477 notes on `&Path` / `&RegisteredGlobalDb`).
    let catch_up = async move {
        run_startup_session_catch_up(
            sessions,
            user_sessions,
            registry_db,
            profile_identity,
            project_root.as_path(),
            project_id.as_deref(),
            &cancellation,
        )
        .await
    };
    match transcript_source_home {
        Some(home) => crate::sessions::with_transcript_source_home(home, catch_up).await,
        None => catch_up.await,
    }
}

async fn run_startup_session_post_ingest(
    db: Arc<RegisteredGlobalDb>,
    analytics_db: Option<Arc<RegisteredGlobalDb>>,
    project_root: PathBuf,
    cancellation: crate::application::observation::ObservationCancellation,
) -> bool {
    let git = crate::sessions::git_correlation::SystemGit;
    let _ = crate::store::GlobalDbGitCorrelationStore::new(Arc::clone(&db))
        .run_incremental_backfill(
            &git,
            crate::sessions::git_correlation::DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS,
        )
        .await;
    if cancellation.is_cancelled() {
        return false;
    }
    if let Some(analytics_db) = analytics_db {
        let sources = crate::analytics_bridge::hook_import_sources(Some(&project_root));
        let _ =
            crate::analytics_bridge::import_hook_analytics(analytics_db.as_ref(), sources).await;
        let project_id = RegisteredGlobalDb::canonical_project_key(&project_root);
        let now = crate::tracedecay::current_timestamp();
        let _ = crate::hooks::hint_outcomes::correlate_hint_outcomes(
            analytics_db.as_ref(),
            db.as_ref(),
            project_id.as_str(),
            now,
        )
        .await;
    }
    true
}

/// Shared compare-and-swap cooldown gate for the lazy staleness check,
/// background read refresh, and automation-notice check below. Each
/// wraps one `AtomicI64` timestamp field on [`McpServer`]; `try_claim`
/// single-flights concurrent callers off that stamp so at most one
/// caller per window proceeds.
///
/// Note: call sites are inconsistent about additionally special-casing
/// a `0` (never-checked) stamp before calling `try_claim` — some do,
/// some don't. That inconsistency predates this extraction and is
/// preserved as-is here rather than harmonized.
struct CooldownGate;

impl CooldownGate {
    /// Returns `true` iff at least `window_secs` have elapsed since
    /// `atomic`'s last stamp and this call won the race to advance it
    /// to `now`. The loser of a race bails so at most one caller
    /// within each window proceeds.
    fn try_claim(&self, atomic: &AtomicI64, now: i64, window_secs: i64) -> bool {
        let previous = atomic.load(Ordering::Acquire);
        if now.saturating_sub(previous) < window_secs {
            return false;
        }
        atomic
            .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl McpServer {
    pub(crate) fn project_server_response_lifecycle(&self) -> ProjectServerResponseLifecycle {
        self.project_server_lifecycle.clone()
    }

    pub(crate) fn revoke_project_server_responses(&self) {
        self.project_server_lifecycle.revoke();
    }

    pub(crate) async fn wait_for_project_server_request_drain(&self) {
        self.project_server_lifecycle.wait_for_request_drain().await;
    }

    pub(crate) fn abort_project_server_requests(&self) {
        self.project_server_lifecycle.abort_requests();
        // Poison recovery matters most here: skipping this on a poisoned
        // mutex leaves every in-flight request uncancelled and the shutdown
        // drain waits forever.
        let cancellations =
            crate::mcp::server::requests::recover_lock(&self.application_surface_cancellations);
        let now = crate::mcp::server::requests::mcp_now_micros();
        for cancellation in cancellations.values() {
            cancellation.cancel(now);
        }
    }

    pub(crate) fn cancel_startup_transcript_ingest(&self) {
        self.startup_catch_up.cancellation().cancel();
    }

    pub(super) async fn shutdown_startup_transcript_ingest(&self) {
        self.cancel_startup_transcript_ingest();
        if let Some(task) = self.startup_catch_up.take_ingest_task()
            && !join_or_abort_startup_ingest(task, STARTUP_TRANSCRIPT_INGEST_ABORT_DEADLINE).await
        {
            tracing::warn!(
                deadline_secs = STARTUP_TRANSCRIPT_INGEST_ABORT_DEADLINE.as_secs(),
                "startup transcript ingest shutdown backstop aborted and joined the task"
            );
        }
        self.startup_catch_up.mark_cancelled();
    }

    /// Shutdown-side teardown of the index-sync phase, in the order
    /// [`Self::shutdown_background_tasks`] requires: abort and join the
    /// retained handle first, then record that the phase is no longer
    /// pending, and only afterwards tear down the ingest.
    pub(super) async fn shutdown_startup_catch_up_sync(&self) {
        if let Some(task) = self.startup_catch_up.take_sync_task() {
            task.abort();
            let _ = task.await;
            self.startup_catch_up.abandon_sync_phase();
        }
    }

    /// Detects mid-session branch drift and reopens the served instance
    /// onto the live branch's DB, returning the instance the caller should
    /// use for this request.
    ///
    /// Fast path: one cheap `branch_drifted` check (gix HEAD read) on the
    /// current snapshot. On drift, the write lock serializes the swap and
    /// the drift check is repeated under it so concurrent calls reopen at
    /// most once. If reopening fails the previous instance is kept — the
    /// drift guards in [`TraceDecay::ensure_branch_writable`] and
    /// [`Self::maybe_sync_if_stale`] still protect writes, exactly as
    /// before this hot-swap existed.
    pub(crate) async fn reopen_if_branch_drifted(&self) -> Arc<TraceDecay> {
        self.reopen_if_branch_drifted_memoized().await.0
    }

    /// [`reopen_if_branch_drifted`](Self::reopen_if_branch_drifted) that also
    /// hands back this request's single branch resolution, so the rest of the
    /// request reads the live branch from the memo instead of re-opening the
    /// repository. The memo is request-scoped and never retained.
    pub(crate) async fn reopen_if_branch_drifted_memoized(
        &self,
    ) -> (Arc<TraceDecay>, crate::branch::BranchMemo) {
        let current = self.cg_snapshot().await;
        // One resolution serves the fast-path check, the re-check under the
        // reopen lock, and every later live-branch read in this request.
        let live_branch = current.branch_memo();
        if !current.branch_drifted_with(&live_branch) {
            return (current, live_branch);
        }
        let Ok(_reopen_guard) = self.branch_reopen.try_lock() else {
            return (current, live_branch);
        };
        // Re-check against a *fresh snapshot*: a concurrent request may have
        // already swapped the served instance onto this same live branch.
        let current = self.cg_snapshot().await;
        if !current.branch_drifted_with(&live_branch) {
            return (current, live_branch);
        }
        let fresh = match current.reopen_for_current_branch().await {
            Ok(fresh) => Arc::new(fresh),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    serving_branch = current.serving_branch().unwrap_or("<none>"),
                    "branch drift detected but index reopen failed"
                );
                return (current, live_branch);
            }
        };
        tracing::info!(
            branch = fresh.active_branch().unwrap_or("<detached>"),
            "reopened index after branch change"
        );
        {
            let mut guard = self.cg.write().await;
            *guard = fresh.clone();
        }
        if let Some(reconcile) = &self.database_owner_reconciler {
            reconcile(fresh.clone()).await;
        }
        // New branch DB ⇒ new file set; refresh the token accounting map.
        self.refresh_file_token_map().await;
        (fresh, live_branch)
    }

    pub(crate) async fn reopen_after_branch_tracking_added(&self) {
        let _reopen_guard = self.branch_reopen.lock().await;
        let current = self.cg_snapshot().await;
        let reopened = match current.reopen_for_current_branch().await {
            Ok(fresh) => {
                let fresh = Arc::new(fresh);
                tracing::info!(
                    branch = fresh.active_branch().unwrap_or("<detached>"),
                    "reopened index after branch tracking was added"
                );
                {
                    let mut guard = self.cg.write().await;
                    *guard = fresh.clone();
                }
                if let Some(reconcile) = &self.database_owner_reconciler {
                    reconcile(fresh.clone()).await;
                }
                true
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    serving_branch = current.serving_branch().unwrap_or("<none>"),
                    "index reopen failed after branch tracking was added"
                );
                false
            }
        };
        if reopened {
            self.refresh_file_token_map().await;
        }
    }

    /// Catch-up sync helper for tests and explicit callers. Bypasses the 30 s
    /// cooldown in [`Self::maybe_sync_if_stale`] so changes made while the
    /// server was down — a terminal `git pull`, IDE edits before the agent
    /// launched, files touched by another tool — can be reconciled before
    /// assertions or source-editing work. The staleness-check stamp is updated
    /// on the way out so the next lazy sync doesn't immediately re-walk the
    /// tree.
    ///
    /// The machine is advanced on every exit path (including errors) so
    /// [`Self::wait_for_startup_catch_up`] never hangs.
    pub async fn run_startup_catch_up_sync(&self) {
        self.startup_catch_up.begin_sync();

        let cg = self.cg_snapshot().await;
        let refresh = Arc::clone(&self.background_refresh_writer);
        let request = BackgroundRefreshRequest {
            graph: Arc::clone(&cg),
            project_root: cg.project_root().to_path_buf(),
            full_sync_escalation_files: self.sync_config.full_sync_escalation_files,
        };
        match refresh(request).await {
            Ok(Some(fresh)) => {
                *crate::mcp::server::requests::recover_lock(&self.file_token_map) = fresh;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "startup catch-up sync failed");
                self.startup_catch_up.settle();
                return;
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.last_staleness_check_at.store(now, Ordering::Release);

        // Best-effort transcript ingestion sweep for hookless agents (Claude,
        // Codex, Gemini). Cursor ingests via its own end-of-turn hook; these
        // agents register no hook, so their transcripts are reconciled here.
        // Detached so it never delays MCP readiness. Do not wrap the database
        // work in a timeout: cancelling it after BEGIN could leave the shared
        // connection inside an open transaction. Callers that need a bounded
        // readiness wait use `wait_for_startup_catch_up` instead.
        // The machine is moved to `Ingesting` *before* the spawn and settled
        // from inside it (via an `Arc` clone), so tests that assert on LCM
        // store content can wait for both phases via
        // `wait_for_startup_catch_up`.
        {
            self.startup_catch_up.enter_ingesting();
            let project_root = cg.project_root().to_path_buf();
            let project_id = cg.store_layout().identity.project_id.clone();
            // `session_db`/`registered_session_db` (and the user pair) are set
            // from the same `Arc` by every construction site, so startup
            // catch-up takes one authority per scope rather than two.
            let sessions = self.session_db.clone();
            let user_sessions = self.user_session_db.clone();
            let registry_db = self.registry_db.clone();
            let profile_identity = self.profile_identity.clone();
            let project_session_refresh_wake = self.project_session_refresh_wake.clone();
            let user_session_refresh_wake = self.user_session_refresh_wake.clone();
            let machine = Arc::clone(&self.startup_catch_up);
            let cancellation = self.startup_catch_up.cancellation().clone();
            let analytics_db = self.accounting_db.clone();
            let transcript_source_home = self.transcript_source_home.clone();
            let task = tokio::spawn(async move {
                let catch_up = run_startup_session_catch_up_with_home(
                    transcript_source_home,
                    sessions,
                    user_sessions,
                    registry_db,
                    profile_identity,
                    project_root.clone(),
                    project_id,
                    cancellation.clone(),
                )
                .await;
                if let Some(db) = catch_up.project_sessions {
                    if cancellation.is_cancelled() {
                        machine.settle();
                        return;
                    }
                    if let Some(wake) = &project_session_refresh_wake {
                        wake.wake();
                    }
                    // Historical git-span correlation is only ever written by
                    // live hook events (which never fire for stdio/daemonless
                    // deployments) or a manual CLI backfill. Neither runs for
                    // most projects, leaving `session_git_spans` empty so
                    // `sessions_for` silently returns nothing. Drain that
                    // history here — one bounded, watermarked pass per startup
                    // — so correlation self-heals without a manual invocation.
                    // With transcripts freshly ingested into `db`'s
                    // session_messages, close the hint-efficacy loop: import
                    // any new hook telemetry into the durable analytics store
                    // and correlate emitted hints against the tool activity
                    // that followed them. Best-effort and idempotent (own
                    // parse cursors + hint_outcome watermark), so it never
                    // blocks readiness and re-runs safely each startup.
                    // Boxed with an explicit `Send` bound: proving the
                    // post-ingest future Send inside this spawned block trips
                    // rustc's higher-ranked leak check; at this narrower scope
                    // the same proof discharges first-order.
                    let post_ingest: std::pin::Pin<
                        Box<dyn std::future::Future<Output = bool> + Send>,
                    > = Box::pin(run_startup_session_post_ingest(
                        db,
                        analytics_db,
                        project_root,
                        cancellation.clone(),
                    ));
                    if !post_ingest.await {
                        machine.settle();
                        return;
                    }
                }
                // Wake on the observed sweep, not on the authorities being
                // present: every skip path above (missing authority, early
                // return, cancellation, absent profile root) leaves nothing
                // new for the temporal refresh scheduler to pick up.
                if catch_up.user_sweep_completed
                    && let Some(wake) = &user_session_refresh_wake
                {
                    wake.wake();
                }
                machine.settle();
            });
            self.startup_catch_up.install_ingest_task(task);
        }
    }

    /// Returns `true` once the *synchronous* portion of
    /// [`Self::run_startup_catch_up_sync`] has finished (the file-tree walk
    /// and index sync). See [`Self::transcript_ingest_done`] for the
    /// detached ingest task.
    pub fn startup_catch_up_done(&self) -> bool {
        self.startup_catch_up.sync_phase_settled()
    }

    /// Returns `true` once the detached transcript-ingest task spawned by
    /// [`Self::run_startup_catch_up_sync`] has completed (success or error).
    pub fn transcript_ingest_done(&self) -> bool {
        self.startup_catch_up.ingest_phase_settled()
    }

    /// Polls until both the synchronous catch-up sync *and* the detached
    /// transcript-ingest task have completed, or until `timeout` elapses.
    /// Returns `true` if both completed within the budget.
    ///
    /// Tests use this so neither the index walk nor the transcript ingest
    /// races against later DB assertions.
    pub async fn wait_for_startup_catch_up(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.startup_catch_up_done() || !self.transcript_ingest_done() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        true
    }

    /// Walk the project tree, sync any stale files, and refresh the
    /// file-to-token-count map — but only if at least 30 s have passed
    /// since the last successful sync. The cooldown is the gate: while
    /// it holds, this returns immediately, so dropping it into every
    /// `tools/call` handler is cheap.
    ///
    /// Concurrent callers are serialized via
    /// [`Self::last_staleness_check_at`]: the first caller stamps `now`
    /// into the field with `compare_exchange`; later callers within the
    /// same window see the stamp and bail. If the actual sync work
    /// fails, the stamp still advances — failure to walk the tree
    /// should not cause every subsequent tool call to retry.
    pub async fn maybe_sync_if_stale(&self) {
        let cg = self.cg_snapshot().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let previous = self.last_staleness_check_at.load(Ordering::Acquire);
        let last_sync = cg.last_sync_timestamp().await;
        if previous != 0 && now.saturating_sub(last_sync) < 30 {
            return;
        }

        if !CooldownGate.try_claim(&self.last_staleness_check_at, now, 30) {
            return;
        }

        // Branch-drift guard (#2): if the working tree switched branches since
        // this snapshot opened, the cached DB belongs to the old branch. Skip
        // the lazy sync — `find_stale_files` would diff the new branch's files
        // against the old branch's DB, and `ensure_branch_writable` would
        // reject the write anyway. `tools/call` reopens onto the live branch
        // via [`Self::reopen_if_branch_drifted`] *before* invoking this, so
        // the guard only fires on a checkout racing the current call.
        //
        // R4: deliberately resolves its own branch rather than taking the
        // request memo. The `CooldownGate` claim above rate-limits this path
        // to once per 30s, so it is not a per-request cost, and re-reading
        // HEAD here keeps the racing-checkout guard genuine.
        if cg.branch_drifted() {
            return;
        }

        let stale = cg.find_stale_files().await;
        if !stale.is_empty()
            && let Err(e) = cg.sync_if_stale_silent(&stale).await
        {
            tracing::warn!(error = %e, "lazy sync failed");
            return;
        }
        // Always refresh: a sibling MCP peer may have synced the DB
        // between our cooldown windows, in which case `stale` is empty
        // here but our in-memory `file_token_map` is still pre-sync.
        self.refresh_file_token_map().await;
    }

    /// D4: sync-on-read entry point for read (non-edit) tools. NEVER blocks.
    ///
    /// If read-refresh is enabled and the read cooldown has elapsed since the
    /// last background spawn, this `compare_exchange`s
    /// [`background_refresh_running`](Self::background_refresh_running) to
    /// `true` and spawns a detached refresh, then returns immediately so the
    /// caller serves the current answer with zero added latency. The *next*
    /// read observes the freshly synced index.
    ///
    /// Single-flighted three ways: the `read_cooldown_secs` stamp, the
    /// `background_refresh_running` flag, and the underlying cross-process
    /// sync lock. At most one refresh runs at a time.
    ///
    /// R4: this runs before any cooldown claim, so it is on the hot path of
    /// every read tool call. It takes the caller's request-scoped branch memo
    /// — the same resolution `reopen_if_branch_drifted` already made for this
    /// request — instead of re-opening the repository.
    pub(crate) fn maybe_spawn_read_refresh(
        &self,
        cg: &Arc<TraceDecay>,
        live_branch: &crate::branch::BranchMemo,
    ) {
        if !self.sync_config.read_refresh {
            return;
        }
        // A checkout racing this call would diff the new branch against the
        // old branch's DB; `tools/call` reopens onto the live branch before
        // dispatch, so this only fires on an in-flight race. Skip it — the
        // next call runs on the reopened snapshot.
        if cg.branch_drifted_with(live_branch) {
            return;
        }

        let now = crate::tracedecay::current_timestamp();
        let cooldown = self.sync_config.read_cooldown_secs as i64;
        let previous = self.last_background_refresh_at.load(Ordering::Acquire);
        if previous != 0 && now.saturating_sub(previous) < cooldown {
            return;
        }
        // Reserve the cooldown slot. If another read call won the race, bail.
        if !CooldownGate.try_claim(&self.last_background_refresh_at, now, cooldown) {
            return;
        }
        // Reserve the single-flight slot. If a refresh is already running
        // (e.g. a slow prior spawn that outlived its cooldown), don't stack.
        if self
            .background_refresh_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        self.spawn_read_refresh_task(cg, self.sync_config.full_sync_escalation_files);
    }

    /// Spawns the detached D4 refresh task. The task owns cheap `Arc` clones
    /// of the background-refresh flag, the completion stamp, and the shared
    /// file-token map, so no `Arc<Self>` receiver is needed. Prefers diff-
    /// scoping off `last_synced_commit`; falls back to the full tree walk
    /// when no base commit is stamped or the diff escalates past the limit.
    ///
    /// The caller MUST have already set `background_refresh_running` to
    /// `true`; this task clears it on completion.
    pub(crate) fn spawn_read_refresh_task(&self, cg: &Arc<TraceDecay>, escalation: usize) {
        let running = Arc::clone(&self.background_refresh_running);
        let done_at = Arc::clone(&self.last_background_refresh_done_at);
        let token_map = Arc::clone(&self.file_token_map);
        let refresh = Arc::clone(&self.background_refresh_writer);
        let request = BackgroundRefreshRequest {
            graph: Arc::clone(cg),
            project_root: cg.project_root().to_path_buf(),
            full_sync_escalation_files: escalation,
        };
        tokio::spawn(async move {
            match refresh(request).await {
                Ok(Some(fresh)) => {
                    if let Ok(mut guard) = token_map.lock() {
                        *guard = fresh;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "background read refresh could not reopen project"
                    );
                }
            }
            done_at.store(crate::tracedecay::current_timestamp(), Ordering::Release);
            running.store(false, Ordering::Release);
        });
    }

    /// Returns a compact one-line notice when automation runs have staged
    /// managed-skill output awaiting review that the user hasn't been told
    /// about yet. Fact proposal counts remain telemetry-only.
    ///
    /// Cheap by construction: a 60 s `compare_exchange` cooldown gates the
    /// check, and the underlying dedupe state
    /// ([`crate::automation::staged_notice`]) fires at most once per new
    /// batch (latest run id or pending-count change), so dropping this into
    /// every `tools/call` response is safe.
    pub(crate) async fn maybe_automation_staged_notice(&self, cg: &TraceDecay) -> Option<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if !CooldownGate.try_claim(&self.last_automation_notice_check_at, now, 60) {
            return None;
        }
        let profile_root = crate::storage::default_profile_root().ok()?;
        let owner = cg.project_memory_owner().ok()?;
        let memory = crate::tracedecay::facts::memory_application_for_db(owner, cg.db()).ok()?;
        crate::automation::staged_notice::maybe_automation_staged_notice(
            &memory,
            &cg.store_layout().dashboard_root,
            &profile_root,
        )
        .await
    }

    /// Returns a version-update warning if a newer release is available.
    /// Results are cached for `VERSION_CHECK_INTERVAL` (15 minutes).
    pub(crate) async fn check_version_update(&self) -> Option<String> {
        let current = env!("CARGO_PKG_VERSION");

        // Fast path: serve from cache if still fresh.
        {
            let cache = self.version_cache.lock().ok()?;
            if let Some(checked_at) = cache.checked_at
                && checked_at.elapsed() < VERSION_CHECK_INTERVAL
            {
                let latest = cache.latest.as_deref()?;
                return if crate::cloud::is_newer_minor_version(current, latest) {
                    Some(format!(
                        "⚠️ tracedecay v{current} is installed, but v{latest} is available. \
                             Run `tracedecay upgrade` to update."
                    ))
                } else {
                    None
                };
            }
        }

        // Cache miss or expired – fetch from GitHub (best-effort, 1 s timeout).
        let latest = tokio::task::spawn_blocking(crate::cloud::fetch_latest_version)
            .await
            .ok()
            .flatten();

        // Update cache regardless of fetch outcome so we don't retry immediately.
        if let Ok(mut cache) = self.version_cache.lock() {
            cache.latest.clone_from(&latest);
            cache.checked_at = Some(Instant::now());
        }

        let latest = latest?;
        if crate::cloud::is_newer_minor_version(current, &latest) {
            Some(format!(
                "⚠️ tracedecay v{current} is installed, but v{latest} is available. \
                 Run `tracedecay upgrade` to update."
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::join_or_abort_startup_ingest;

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn startup_ingest_timeout_aborts_and_joins_instead_of_detaching() {
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _dropped = Dropped(task_dropped);
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        entered_rx.await.expect("startup ingest task entered");

        assert!(
            !join_or_abort_startup_ingest(task, Duration::from_millis(5)).await,
            "a stuck startup ingest must use the abort backstop"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "the aborted task must be joined before shutdown continues"
        );
    }
}
