//! Server lifecycle maintenance: startup catch-up, staleness-driven
//! sync-on-read, branch-drift reopen, and version-update checks.

use super::*;

/// Cache duration for version checks (15 minutes).
const VERSION_CHECK_INTERVAL: Duration = Duration::from_mins(15);

/// Why a detached branch reopen was kicked. The two triggers differ only in
/// whether the reopen re-checks for drift before running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchReopenTrigger {
    /// A request observed the served branch diverge from the live one.
    Drift,
    /// A branch was newly tracked, so the live branch now has a DB of its own.
    TrackingAdded,
}

impl BranchReopenTrigger {
    fn reason(self) -> &'static str {
        match self {
            Self::Drift => "branch_drift",
            Self::TrackingAdded => "branch_tracking_added",
        }
    }
}

/// Retained startup index-sync task, joined or aborted before the code graph
/// authority is released.
#[derive(Default)]
pub(crate) struct StartupCatchUpTasksV1 {
    sync: Option<tokio::task::JoinHandle<()>>,
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
    /// The index sync finished, including failure paths.
    Settled { tasks: StartupCatchUpTasksV1 },
    /// Shutdown tore the machine down.
    Cancelled,
}

impl StartupCatchUpStateV1 {
    const fn settled(&self) -> bool {
        !matches!(self, Self::Syncing { .. })
    }

    fn tasks_mut(&mut self) -> Option<&mut StartupCatchUpTasksV1> {
        match self {
            Self::Syncing { tasks } | Self::Settled { tasks } => Some(tasks),
            Self::NotStarted | Self::Cancelled => None,
        }
    }

    fn take_tasks(&mut self) -> StartupCatchUpTasksV1 {
        self.tasks_mut().map(std::mem::take).unwrap_or_default()
    }
}

/// Owns the startup index catch-up state.
///
/// Held behind an `Arc` on the server so the spawned sync task can signal
/// completion through the same lock the waiters read.
/// The lock is a `std::sync::Mutex` on purpose: every critical section is a
/// phase swap or a handle take, and joins always happen *outside* it, so the
/// sync readiness accessors stay callable from non-async code.
pub(crate) struct StartupCatchUpMachineV1 {
    state: std::sync::Mutex<StartupCatchUpStateV1>,
    /// Set once the first dispatch claims the machine. Kept distinct from
    /// the phase so a completed catch-up still refuses a second dispatch.
    dispatched: std::sync::atomic::AtomicBool,
}

impl Default for StartupCatchUpMachineV1 {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(StartupCatchUpStateV1::NotStarted),
            dispatched: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl StartupCatchUpMachineV1 {
    fn state(&self) -> std::sync::MutexGuard<'_, StartupCatchUpStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    /// The index-sync phase is done.
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

    fn take_sync_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.state().tasks_mut().and_then(|tasks| tasks.sync.take())
    }

    /// Terminal shutdown state.
    fn mark_cancelled(&self) {
        *self.state() = StartupCatchUpStateV1::Cancelled;
    }

    fn settled(&self) -> bool {
        self.state().settled()
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

    pub(super) fn settled_for_test(&self) -> bool {
        self.settled()
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
            crate::mcp::server::requests::recover_lock(self.dispatch_authority.cancellations());
        let now = crate::mcp::server::requests::mcp_now_micros();
        for cancellation in cancellations.values() {
            cancellation.cancel(now);
        }
    }

    /// Shutdown-side teardown of the startup index-sync phase.
    pub(super) async fn shutdown_startup_catch_up_sync(&self) {
        if let Some(task) = self.startup_catch_up.take_sync_task() {
            task.abort();
            let _ = task.await;
        }
        self.startup_catch_up.mark_cancelled();
    }

    /// Detects mid-session branch drift, kicks the reopen onto the live
    /// branch's DB in the background, and returns the instance the caller
    /// should use for this request.
    ///
    /// Fast path: one cheap `branch_drifted` check (gix HEAD read) on the
    /// current snapshot.
    ///
    /// **Serve old, await new.** On drift the caller does *not* wait for the
    /// reopen. `reopen_for_current_branch` is a full DB open plus a sealed
    /// restore — O(store), seconds to minutes on a large index — and it used to
    /// run inline on the request that happened to notice the checkout, with
    /// every other caller either blocked behind the reopen lock or (worse, in
    /// [`Self::reopen_after_branch_tracking_added`]) queued on it with no
    /// bound. Now the reopen is detached and single-flighted, and every caller
    /// — the one that noticed the drift included — serves the last complete
    /// snapshot until the swap lands.
    ///
    /// If reopening fails the previous instance is kept — the drift guards in
    /// [`TraceDecay::ensure_branch_writable`] and [`Self::maybe_sync_if_stale`]
    /// still protect writes, exactly as before this hot-swap existed.
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
        // One resolution serves the fast-path check and every later
        // live-branch read in this request.
        let live_branch = current.branch_memo();
        if !current.branch_drifted_with(&live_branch) {
            return (current, live_branch);
        }
        self.spawn_branch_reopen(BranchReopenTrigger::Drift);
        (current, live_branch)
    }

    /// Kicks a reopen after a branch was newly tracked.
    ///
    /// Never blocks: this used to take `branch_reopen.lock().await`, so every
    /// caller arriving during an in-flight reopen queued behind a full DB open.
    /// It now try-locks and detaches exactly like the drift path.
    pub(crate) async fn reopen_after_branch_tracking_added(&self) {
        self.spawn_branch_reopen(BranchReopenTrigger::TrackingAdded);
    }

    /// Single-flights and detaches one reopen onto the live branch.
    ///
    /// The `branch_reopen` guard is *moved into* the spawned task, so it is
    /// held for the reopen's real duration while no caller ever awaits it. A
    /// caller that finds the lane busy returns immediately: a reopen is already
    /// converging on the same live branch, and the next request observes the
    /// swap.
    fn spawn_branch_reopen(&self, trigger: BranchReopenTrigger) {
        let Ok(reopen_guard) = Arc::clone(&self.branch_reopen).try_lock_owned() else {
            return;
        };
        let cg_cell = Arc::clone(&self.cg);
        let token_map = Arc::clone(&self.file_token_map);
        let completions = Arc::clone(&self.branch_reopen_completions);
        let reconcile = self.database_owner_reconciler.clone();
        let reason = trigger.reason();
        tokio::spawn(async move {
            let _reopen_guard = reopen_guard;
            let current = cg_cell.read().await.clone();
            // Drift-triggered reopens re-check against a *fresh snapshot*: a
            // concurrent reopen may already have swapped the served instance
            // onto this same live branch. A tracking-added reopen has no drift
            // to re-check — the served branch is already the live one; what
            // changed is that it now has a DB of its own — so it always runs,
            // exactly as the blocking version did.
            if trigger == BranchReopenTrigger::Drift && !current.branch_drifted() {
                completions.fetch_add(1, Ordering::Release);
                return;
            }
            match current.reopen_for_current_branch().await {
                Ok(fresh) => {
                    let fresh = Arc::new(fresh);
                    tracing::info!(
                        branch = fresh.active_branch().unwrap_or("<detached>"),
                        reason,
                        "reopened index onto the live branch"
                    );
                    {
                        let mut guard = cg_cell.write().await;
                        *guard = Arc::clone(&fresh);
                    }
                    // The owner reconcile runs here, after the swap, rather
                    // than inside the request that noticed the drift: it takes
                    // the daemon's store writer lane, and a live `tools/call`
                    // must never park on it. That call has already answered on
                    // the snapshot it held.
                    if let Some(reconcile) = &reconcile {
                        reconcile(Arc::clone(&fresh)).await;
                    }
                    // New branch DB ⇒ new file set; refresh the token
                    // accounting map.
                    if let Ok(refreshed) = fresh.get_file_token_map().await {
                        *crate::mcp::server::requests::recover_lock(&token_map) = refreshed;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        serving_branch = current.serving_branch().unwrap_or("<none>"),
                        reason,
                        "index reopen onto the live branch failed"
                    );
                }
            }
            completions.fetch_add(1, Ordering::Release);
        });
    }

    /// Polls until at least one branch reopen has completed past `after`, or
    /// until `timeout` elapses. Returns `true` if one landed.
    ///
    /// Reopens are detached, so tests (and any caller that genuinely needs the
    /// post-swap state rather than an answer) observe completion here instead
    /// of blocking the request path.
    #[doc(hidden)]
    pub async fn wait_for_branch_reopen(&self, after: u64, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.branch_reopen_completions.load(Ordering::Acquire) <= after {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        true
    }

    /// Number of branch reopens that have completed so far.
    #[doc(hidden)]
    pub fn branch_reopens_completed(&self) -> u64 {
        self.branch_reopen_completions.load(Ordering::Acquire)
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

        self.startup_catch_up.settle();
    }

    /// Returns `true` once the startup file-tree walk and index sync finished.
    pub fn startup_catch_up_done(&self) -> bool {
        self.startup_catch_up.settled()
    }

    /// Polls until the startup index catch-up completes or `timeout` elapses.
    pub async fn wait_for_startup_catch_up(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.startup_catch_up_done() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        true
    }

    /// Claim the lazy-sync window for edit-shaped tools and kick the sync in
    /// the background — but only if at least 30 s have passed since the last
    /// successful sync. The cooldown is the gate: while it holds, this returns
    /// immediately, so dropping it into every `tools/call` handler is cheap.
    ///
    /// **Never blocks.** This used to run `find_stale_files` (a full project
    /// tree walk) and then reindex the entire stale set inline, on the request
    /// path, with no bound: one `git pull` ahead of an edit tool turned that
    /// call into an O(store) reindex the client waited on. The claim is still
    /// made here — so the cooldown and single-flight semantics are unchanged —
    /// but the work is detached through the same mechanism read tools already
    /// use ([`Self::spawn_read_refresh_task`]), and the caller serves
    /// immediately on the current snapshot. The *next* call observes the
    /// freshly synced index.
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

        // Reserve the single-flight slot shared with the read-refresh lane so a
        // lazy sync and a read refresh never stack on the same store. If a
        // refresh is already running, the cooldown claim above has done its job
        // and this call simply serves the current snapshot.
        if self
            .background_refresh_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // The detached task refreshes `file_token_map` from the synced graph on
        // every success — including the case where nothing was stale, because a
        // sibling MCP peer may have synced the DB between our cooldown windows.
        self.spawn_read_refresh_task(&cg, self.sync_config.full_sync_escalation_files);
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
