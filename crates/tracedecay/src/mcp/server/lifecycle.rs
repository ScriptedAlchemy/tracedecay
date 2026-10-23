//! Server lifecycle maintenance: startup catch-up, staleness-driven
//! sync-on-read, and branch-drift reopen.

use super::*;
use tracedecay_domain::ApplicationProblemDetailV1;

/// A parked index logs its cause and remedy as fields; the bounded route
/// message would cut the cause.
fn warn_reconcile_not_admitted(error: &TraceDecayError, message: &str) {
    match (
        error.project_route_context(),
        error.project_route_typed_detail(),
    ) {
        (
            Some((reason_code, _, _)),
            Some(ApplicationProblemDetailV1::Parked {
                cause,
                remedy,
                retries_on_wake,
            }),
        ) => tracing::warn!(reason_code, cause, remedy, retries_on_wake, "{message}"),
        _ => tracing::warn!(error = %error, "{message}"),
    }
}

struct ReadRefreshRunningGuard(Arc<AtomicBool>);

impl Drop for ReadRefreshRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct BranchReopenCompletion(Arc<tokio::sync::watch::Sender<u64>>);

impl Drop for BranchReopenCompletion {
    fn drop(&mut self) {
        self.0.send_modify(|completed| *completed += 1);
    }
}

/// Compare-and-swap cooldown gate for the lazy staleness check and background
/// read refresh. Returns `true` iff at least `window_secs` have elapsed since
/// `atomic`'s last stamp and this call won the race to advance it to `now`, so
/// at most one caller per window proceeds.
fn try_claim_cooldown(atomic: &AtomicI64, now: i64, window_secs: i64) -> bool {
    let previous = atomic.load(Ordering::Acquire);
    if now.saturating_sub(previous) < window_secs {
        return false;
    }
    atomic
        .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

impl McpServer {
    pub(crate) fn spawn_background_task<Task>(&self, task: Task) -> bool
    where
        Task: std::future::Future<Output = ()> + Send + 'static,
    {
        self.background_tasks.spawn(task)
    }

    pub(crate) fn refuse_background_work(&self) {
        self.background_tasks.close_admission();
    }

    pub(crate) fn project_server_response_lifecycle(&self) -> ProjectServerResponseLifecycle {
        self.project_server_lifecycle.clone()
    }

    pub(crate) fn revoke_project_server_responses(&self) {
        self.project_server_lifecycle.revoke();
    }

    pub(crate) async fn revoke_project_server_responses_after_drain(&self) {
        self.project_server_lifecycle
            .revoke_after_request_drain()
            .await;
    }

    pub(crate) async fn wait_for_project_server_request_drain(&self) {
        self.project_server_lifecycle.wait_for_request_drain().await;
    }

    pub(crate) fn abort_project_server_requests(&self) {
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
    #[tracing::instrument(
        name = "mcp.server.startup_catch_up.shutdown",
        level = "trace",
        skip_all
    )]
    pub(super) async fn shutdown_startup_catch_up_sync(&self) {
        if let Some(task) = self.startup_catch_up.take_sync_task() {
            task.abort();
            let _ = task.wait().await;
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
    /// restore, O(store), seconds to minutes on a large index, and it used to
    /// run inline on the request that happened to notice the checkout, with
    /// every other caller blocked behind the reopen lock. Now the reopen is
    /// retained and single-flighted, and every caller
    ///, the one that noticed the drift included, serves the last complete
    /// snapshot until the swap lands.
    ///
    /// If reopening fails the previous instance is kept, the effect-time
    /// branch identity check in the hook writer and
    /// [`Self::maybe_sync_if_stale`] still protect writes.
    pub(crate) async fn reopen_if_branch_drifted(&self) -> Arc<TraceDecay> {
        self.reopen_if_branch_drifted_memoized().await.0
    }

    /// [`reopen_if_branch_drifted`](Self::reopen_if_branch_drifted) that also
    /// hands back this request's single branch resolution, so the rest of the
    /// request reads the live branch from the memo instead of re-opening the
    /// repository. The memo is request-scoped and never retained.
    #[tracing::instrument(
        name = "mcp.server.branch_drift_reopen_memoized",
        level = "trace",
        skip_all
    )]
    pub(crate) async fn reopen_if_branch_drifted_memoized(
        &self,
    ) -> (Arc<TraceDecay>, tracedecay_runtime_core::branch::BranchMemo) {
        let current = self.cg_snapshot().await;
        // One resolution serves the fast-path check and every later
        // live-branch read in this request.
        let live_branch = current.branch_memo();
        if !current.branch_drifted_with(&live_branch) {
            return (current, live_branch);
        }
        self.spawn_branch_reopen();
        (current, live_branch)
    }

    /// Single-flights and retains one reopen onto the live branch.
    ///
    /// The `branch_reopen` guard is *moved into* the spawned task, so it is
    /// held for the reopen's real duration while no caller ever awaits it. A
    /// caller that finds the lane busy returns immediately: a reopen is already
    /// converging on the same live branch, and the next request observes the
    /// swap.
    fn spawn_branch_reopen(&self) {
        let Ok(reopen_guard) = Arc::clone(&self.branch_reopen).try_lock_owned() else {
            return;
        };
        let cg_cell = Arc::clone(&self.cg);
        let completion = BranchReopenCompletion(Arc::clone(&self.branch_reopen_completions));
        let reconcile = self.database_owner_reconciler.clone();
        // A drift observation is the only trigger for a retained reopen.
        let reason = "branch_drift";
        let _admitted = self.background_tasks.spawn(async move {
            let _completion = completion;
            let _reopen_guard = reopen_guard;
            let current = cg_cell.read().await.clone();
            // Re-check against a *fresh snapshot*: a concurrent reopen may
            // already have swapped the served instance onto this same live
            // branch.
            if !current.branch_drifted() {
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
        });
    }

    /// Waits until at least one branch reopen has completed past `after`, or
    /// until `timeout` elapses. Returns `true` if one landed. Reopens do not
    /// block requests, so tests observe completion here.
    #[cfg(test)]
    pub(crate) async fn wait_for_branch_reopen(
        &self,
        after: u64,
        timeout: std::time::Duration,
    ) -> bool {
        let mut completions = self.branch_reopen_completions.subscribe();
        matches!(
            tracedecay_runtime_core::native_runtime::timeout(
                timeout,
                completions.wait_for(|completed| *completed > after),
            )
            .await,
            Ok(Ok(_))
        )
    }

    /// Number of branch reopens that have completed so far.
    #[cfg(test)]
    pub(crate) fn branch_reopens_completed(&self) -> u64 {
        *self.branch_reopen_completions.borrow()
    }

    /// Catch-up helper for tests and explicit callers. Bypasses the 30 s
    /// cooldown in [`Self::maybe_sync_if_stale`] so changes made while the
    /// server was down, a terminal `git pull`, IDE edits before the agent
    /// launched, files touched by another tool, are admitted for authoritative
    /// reconciliation. This method waits only for scheduler admission, never
    /// for indexing. The staleness-check stamp is updated on the way out so the
    /// next lazy request does not immediately enqueue duplicate work.
    ///
    /// The machine is advanced on every exit path (including errors) so
    /// [`Self::wait_for_startup_catch_up`] never hangs.
    #[tracing::instrument(name = "mcp.server.startup_catch_up", level = "trace", skip_all)]
    pub async fn run_startup_catch_up_sync(&self) {
        self.startup_catch_up.begin_sync();

        let cg = self.cg_snapshot().await;
        let refresh = Arc::clone(&self.background_refresh_writer);
        let request = BackgroundRefreshRequest {
            graph: Arc::clone(&cg),
            project_root: cg.project_root().to_path_buf(),
            mode: super::BackgroundRefreshModeV1::ForceReconcile,
            reconcile_sink: self.code_index_reconcile_sink.clone(),
            freshness_probe_sink: self.code_index_freshness_probe_sink.clone(),
        };
        match refresh(request).await {
            Ok(super::hook_writes::BackgroundRefreshOutcome::Admitted(Some(fresh))) => {
                *crate::mcp::server::requests::recover_lock(&self.file_token_map) = fresh;
            }
            Ok(super::hook_writes::BackgroundRefreshOutcome::Admitted(None)) => {}
            Ok(super::hook_writes::BackgroundRefreshOutcome::NotApplicable) => {
                tracing::debug!("startup code-index catch-up does not apply without Git identity");
            }
            Ok(super::hook_writes::BackgroundRefreshOutcome::LinkedWorktreeDisabled) => {
                tracing::info!(
                    reason = "linked_worktree_disabled",
                    "automatic code-index refresh disabled by watch policy"
                );
            }
            Err(e) => {
                warn_reconcile_not_admitted(&e, "startup catch-up admission failed");
                self.startup_catch_up.settle();
                return;
            }
        }
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        self.last_staleness_check_at.store(now, Ordering::Release);

        self.startup_catch_up.settle();
    }

    /// Returns `true` once startup reconciliation admission has settled.
    pub fn startup_catch_up_done(&self) -> bool {
        self.startup_catch_up.settled()
    }

    /// Polls until startup reconciliation admission settles or `timeout` elapses.
    pub async fn wait_for_startup_catch_up(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !self.startup_catch_up_done() {
            let now = std::time::Instant::now();
            if now >= deadline
                || tracedecay_runtime_core::native_runtime::sleep_until(
                    now + std::time::Duration::from_millis(25),
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    /// Claim the lazy-reconciliation window for edit-shaped tools and enqueue it
    /// in the background, but only if at least 30 s have passed since the last
    /// successful admission. The cooldown is the gate: while it holds, this returns
    /// immediately, so dropping it into every `tools/call` handler is cheap.
    ///
    /// **Never blocks.** This used to perform a full project tree walk and then
    /// reindex the entire stale set inline, on the request
    /// path, with no bound: one `git pull` ahead of an edit tool turned that
    /// call into an O(store) reindex the client waited on. The claim is still
    /// made here, so the cooldown and single-flight semantics are unchanged,
    /// but the bounded request is retained through the same mechanism read tools
    /// already use (`Self::spawn_read_refresh_task`), and the caller serves
    /// immediately on the current snapshot. Freshness is reported separately by
    /// the code-index authority after reconciliation completes.
    ///
    /// Concurrent callers are serialized via
    /// `Self::last_staleness_check_at`: the first caller stamps `now`
    /// into the field with `compare_exchange`; later callers within the
    /// same window see the stamp and bail. If admission fails, the stamp still
    /// advances so every subsequent tool call does not retry immediately.
    #[tracing::instrument(name = "mcp.server.sync_if_stale", level = "trace", skip_all)]
    pub async fn maybe_sync_if_stale(&self) {
        if !self.background_tasks.admits() {
            return;
        }
        let cg = self.cg_snapshot().await;
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        if !try_claim_cooldown(&self.last_staleness_check_at, now, 30) {
            return;
        }

        // Branch-drift guard (#2): if the working tree switched branches since
        // this snapshot opened, the cached DB belongs to the old branch. Skip
        // lazy reconciliation: a tree diff would compare the new branch's files
        // against the old branch's DB, and the writer fence would
        // reject the write anyway. `tools/call` reopens onto the live branch
        // via [`Self::reopen_if_branch_drifted`] *before* invoking this, so
        // the guard only fires on a checkout racing the current call.
        //
        // R4: deliberately resolves its own branch rather than taking the
        // request memo. The cooldown claim above rate-limits this path
        // to once per 30s, so it is not a per-request cost, and re-reading
        // HEAD here keeps the racing-checkout guard genuine.
        if cg.branch_drifted() {
            return;
        }

        // Reserve the single-flight slot shared with the read-refresh lane so a
        // lazy sync and a read refresh never stack on the same store. If a
        // refresh is already running, the cooldown claim above has done its job
        // and this call serves the current snapshot.
        if self
            .background_refresh_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // The retained task submits one bounded authoritative reconcile request.
        self.spawn_read_refresh_task(&cg);
    }

    /// D4: reconciliation-on-read entry point for read (non-edit) tools. NEVER blocks.
    ///
    /// If read-refresh is enabled and the read cooldown has elapsed since the
    /// last background spawn, this `compare_exchange`s
    /// [`background_refresh_running`](Self::background_refresh_running) to
    /// `true` and spawns a retained refresh, then returns immediately so the
    /// caller serves the current answer with zero added latency. Completion and
    /// freshness remain owned by the code-index scheduler.
    ///
    /// Single-flighted by the `read_cooldown_secs` stamp and the
    /// `background_refresh_running` flag. At most one admission runs at a time.
    ///
    /// R4: this runs before any cooldown claim, so it is on the hot path of
    /// every read tool call. It takes the caller's request-scoped branch memo
    ///, the same resolution `reopen_if_branch_drifted` already made for this
    /// request, instead of re-opening the repository.
    pub(crate) fn maybe_spawn_read_refresh(
        &self,
        cg: &Arc<TraceDecay>,
        live_branch: &tracedecay_runtime_core::branch::BranchMemo,
    ) {
        if !self.sync_config.read_refresh || !self.background_tasks.admits() {
            return;
        }
        // A checkout racing this call would diff the new branch against the
        // old branch's DB; `tools/call` reopens onto the live branch before
        // dispatch, so this only fires on an in-flight race. Skip it, the
        // next call runs on the reopened snapshot.
        if cg.branch_drifted_with(live_branch) {
            return;
        }

        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        let cooldown = self.sync_config.read_cooldown_secs as i64;
        // Reserve the cooldown slot. If another read call won the race, bail.
        if !try_claim_cooldown(&self.last_background_refresh_at, now, cooldown) {
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

        self.spawn_read_refresh_task(cg);
    }

    /// Spawns the retained D4 refresh task. The task owns cheap `Arc` clones
    /// of the background-refresh flag and the admission-completion stamp, so no
    /// `Arc<Self>` receiver is needed. The scheduler owns worktree traversal,
    /// coalescing, and publication after accepting the request.
    ///
    /// The caller MUST have already set `background_refresh_running` to
    /// `true`; this task clears it on completion.
    pub(crate) fn spawn_read_refresh_task(&self, cg: &Arc<TraceDecay>) {
        let running = Arc::clone(&self.background_refresh_running);
        let running_guard = ReadRefreshRunningGuard(Arc::clone(&running));
        let done_at = Arc::clone(&self.last_background_refresh_done_at);
        let token_map = Arc::clone(&self.file_token_map);
        let refresh = Arc::clone(&self.background_refresh_writer);
        let request = BackgroundRefreshRequest {
            graph: Arc::clone(cg),
            project_root: cg.project_root().to_path_buf(),
            mode: super::BackgroundRefreshModeV1::FreshnessProbe,
            reconcile_sink: self.code_index_reconcile_sink.clone(),
            freshness_probe_sink: self.code_index_freshness_probe_sink.clone(),
        };
        let _admitted = self.background_tasks.spawn(async move {
            let _running = running_guard;
            match refresh(request).await {
                Ok(super::hook_writes::BackgroundRefreshOutcome::Admitted(Some(fresh))) => {
                    if let Ok(mut guard) = token_map.lock() {
                        *guard = fresh;
                    }
                }
                Ok(super::hook_writes::BackgroundRefreshOutcome::Admitted(None)) => {}
                Ok(super::hook_writes::BackgroundRefreshOutcome::NotApplicable) => {
                    tracing::debug!(
                        "background code-index reconciliation does not apply without Git identity"
                    );
                }
                Ok(super::hook_writes::BackgroundRefreshOutcome::LinkedWorktreeDisabled) => {
                    tracing::info!(
                        reason = "linked_worktree_disabled",
                        "automatic code-index refresh disabled by watch policy"
                    );
                }
                Err(e) => {
                    warn_reconcile_not_admitted(
                        &e,
                        "background read reconciliation was not admitted",
                    );
                }
            }
            done_at.store(
                tracedecay_runtime_core::tracedecay::current_timestamp(),
                Ordering::Release,
            );
        });
    }
}
