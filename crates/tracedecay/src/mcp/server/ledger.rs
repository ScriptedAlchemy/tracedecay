//! Token-savings ledger and tool/hook analytics recording.

use super::*;
use tracedecay_global_db::RegisteredGlobalDb;

/// Upper bound for [`McpServer::ledger_writes_settled`]. Savings-ledger writes
/// are fire-and-forget `SQLite` appends that finish in well under a second on a
/// healthy machine, so 10 s is generous headroom; the point is that the wait
/// is *finite*, a wedged recorder task can never hang the caller (tests,
/// shutdown drains) indefinitely as the previous unbounded loop allowed.
const LEDGER_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SPAN_IDENTIFIER_BYTES: usize = 256;

fn bounded_span_identifier(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_SPAN_IDENTIFIER_BYTES
                && !value.chars().any(char::is_control)
        })
        .map(str::to_string)
}

fn derive_hook_span_git_context(
    cwd: &Path,
    project_root: PathBuf,
    active_project_root: &Path,
) -> Option<(String, Option<String>)> {
    let deadline = tracedecay_runtime_core::cancellation::MonotonicDeadline::at(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
    let worktree_raw =
        match tracedecay_runtime_core::git_discovery::discover_repository_identity_with_control(
            cwd,
            deadline,
            &cancellation,
        ) {
            tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Resolved(
                identity,
            ) => identity.worktree_root,
            tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::NotRepository => {
                project_root
            }
            tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Unknown(_) => {
                return None;
            }
        };
    let worktree_raw =
        hook_events::authorize_add_branch_at_root(&worktree_raw, active_project_root).ok()?;
    let worktree = git_correlation::normalize_worktree(&worktree_raw.to_string_lossy());
    let branch = bounded_span_identifier(
        tracedecay_runtime_core::branch::current_branch(&worktree_raw).as_deref(),
    );
    Some((worktree, branch))
}

// Global accounting (the savings ledger) is enabled by default; see
// `tracedecay_global_db::global_accounting_mode` for the env override
// precedence.

/// Where this server's savings-ledger and analytics writes land.
///
/// A server built without an accounting database is legitimate (a direct
/// `McpServer::new` has no profile to account against), but it used to be
/// *silent*: each recorder independently checked both handles and returned
/// early, so a fixture that forgot to mount a database failed later as a
/// missing row in an assertion far from the construction that caused it.
/// Naming the state makes the absence observable at construction, see
/// [`McpServer::ledger_sink_is_mounted`], and collapses three copies of
/// the same fallback into one resolution.
pub(crate) enum LedgerSink {
    Mounted(tracedecay_global_db::RegisteredGlobalDbLeaseV1),
    NotMounted,
}

pub(crate) struct McpToolErrorAnalyticsRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) session_id: Option<String>,
    pub(crate) tool_name: &'a str,
    pub(crate) request_id: &'a Value,
    pub(crate) arguments: &'a Value,
    pub(crate) duration_us: Option<u64>,
    pub(crate) error: &'a TraceDecayError,
    pub(crate) connection_client_name: Option<&'a str>,
    pub(crate) connection_instance_id: Option<&'a str>,
}

impl McpServer {
    /// Resolves the ledger sink: the dedicated accounting database when the
    /// daemon mounted one, otherwise the registry handle it shares with, and
    /// [`LedgerSink::NotMounted`] when this server accounts against nothing.
    pub(crate) fn ledger_sink(&self) -> LedgerSink {
        self.accounting_db
            .as_ref()
            .or(self.global_db.as_ref())
            .map_or(LedgerSink::NotMounted, |db| LedgerSink::Mounted(db.clone()))
    }

    /// Whether any ledger write from this server can reach a database.
    ///
    /// Lets a caller assert its accounting wiring where it is built rather
    /// than discovering the gap as an absent row much later.
    pub fn ledger_sink_is_mounted(&self) -> bool {
        matches!(self.ledger_sink(), LedgerSink::Mounted(_))
    }

    /// Estimates the raw-file token cost ("before") for the given file
    /// paths from the cached file-token map (indexed file bytes / 4).
    /// Pure lookup, persists nothing.
    #[tracing::instrument(name = "mcp.ledger.estimate_raw_tokens", level = "trace", skip_all)]
    pub(crate) fn estimate_raw_file_tokens(&self, file_paths: &[String]) -> u64 {
        if file_paths.is_empty() {
            return 0;
        }
        // Paths arrive from indexed node rows on every tool response. A blank
        // one is bad index data, not a broken invariant, so it is skipped
        // rather than asserted on this shared response path.
        let map = crate::mcp::server::requests::recover_lock(&self.file_token_map);
        file_paths
            .iter()
            .filter(|path| !path.is_empty())
            .filter_map(|path| map.get(path.as_str()))
            .sum()
    }

    /// Fire-and-forget persistence for one accounted tool response: the
    /// tokens-saved counters (project DB, resettable local counter, ledger
    /// sink) plus the live monitor ring-buffer entry.
    ///
    /// `net_saved_tokens` must already be the *net* saving for one call
    /// (`before.saturating_sub(after)`), not the gross raw-file estimate:
    /// crediting the full "before" would count a full-file read whose
    /// response contains the entire file as 100% saved.
    ///
    /// The in-memory counter advances synchronously so concurrent calls
    /// account exact totals; the `SQLite` and mmap writes ride the observed
    /// ledger-write path so the response never waits on them and tests can
    /// still await durability via [`Self::ledger_writes_settled`]. Shutdown
    /// persists the final counter independently.
    #[tracing::instrument(
        name = "mcp.ledger.persist_token_accounting",
        level = "trace",
        skip_all
    )]
    pub(crate) fn spawn_token_accounting_persist(
        &self,
        monitor_project_root: &Path,
        tool_name: &str,
        net_saved_tokens: u64,
        raw_file_tokens: u64,
    ) {
        let persist = self
            .tokens_saved
            .as_ref()
            .filter(|_| net_saved_tokens != 0)
            .map(|tokens_saved| {
                let new_total =
                    tokens_saved.fetch_add(net_saved_tokens, Ordering::Relaxed) + net_saved_tokens;
                (
                    std::sync::Arc::clone(&self.cg),
                    self.ledger_sink(),
                    new_total,
                )
            });
        let monitor_project_root = monitor_project_root.to_path_buf();
        let monitor_dir = self
            .owner_profile()
            .map(|profile| profile.data_dir().to_path_buf());
        let tool_name = tool_name.to_owned();
        self.spawn_observed_ledger_write(async move {
            if let Some((cg_lock, sink, new_total)) = persist {
                let cg = cg_lock.read().await.clone();
                let _ = cg.set_tokens_saved(new_total).await;
                let _ = cg.add_local_counter(net_saved_tokens).await;
                if let LedgerSink::Mounted(gdb) = sink
                    && let Err(error) = gdb
                        .try_upsert_project_tokens(cg.project_root(), new_total)
                        .await
                {
                    // Background persist: the response already went out, so
                    // the failed ledger write degrades to a named warning.
                    tracing::warn!(error = %error, "background token-accounting persist failed");
                }
            }
            // The monitor entry opens, locks, and mmaps a file; keep that
            // off the async workers.
            // A server without an owning profile has no monitor ring.
            let Some(monitor_dir) = monitor_dir else {
                return;
            };
            let monitor_write =
                tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
                    tracedecay_session_memory::monitor_ring::write_entry_to(
                        &monitor_dir,
                        &monitor_project_root,
                        "tracedecay",
                        &tool_name,
                        net_saved_tokens,
                        raw_file_tokens,
                    );
                });
            if let Err(error) = monitor_write.await {
                tracing::warn!(error = %error, "live monitor entry write failed");
            }
        });
    }

    /// Resolves once every savings-ledger write spawned so far has
    /// completed (immediately when none are pending, including when global
    /// accounting is disabled and no writes are ever spawned).
    ///
    /// Test-only observability for the fire-and-forget ledger recorder:
    /// production code never calls this, so the request path stays
    /// non-blocking, while tests can await durability deterministically
    /// instead of polling the DB against a wall-clock deadline.
    ///
    /// Bounded by `LEDGER_SETTLE_TIMEOUT` so a spawned write that wedges
    /// (a stuck DB handle, a task that never resolves) can never hang the
    /// caller forever, the earlier unbounded loop made a wedged write
    /// manifest as an un-observable, indefinitely-hung integration test.
    pub async fn ledger_writes_settled(&self) {
        self.ledger_writes_settled_within(LEDGER_SETTLE_TIMEOUT)
            .await;
    }

    /// Like [`Self::ledger_writes_settled`] but bounded by an explicit
    /// `timeout`. Returns `true` when every spawned ledger write settled
    /// within the bound, `false` when the bound elapsed with writes still
    /// pending. A timeout is never silent: it logs a warning naming how many
    /// writes were still outstanding so a wedged recorder is diagnosable.
    pub async fn ledger_writes_settled_within(&self, timeout: std::time::Duration) -> bool {
        let wait = async {
            loop {
                // Register interest *before* re-checking so a completion
                // between the check and the await cannot be missed.
                let notified = self.ledger_write_notify.notified();
                let started = self.ledger_writes_started.load(Ordering::SeqCst);
                let finished = self.ledger_writes_finished.load(Ordering::SeqCst);
                if finished >= started {
                    return;
                }
                notified.await;
            }
        };
        if tracedecay_runtime_core::native_runtime::timeout(timeout, wait)
            .await
            .is_ok()
        {
            return true;
        }
        let started = self.ledger_writes_started.load(Ordering::SeqCst);
        let finished = self.ledger_writes_finished.load(Ordering::SeqCst);
        let pending = started.saturating_sub(finished);
        tracing::warn!(
            ?timeout,
            pending,
            "timed out waiting for savings-ledger writes"
        );
        false
    }

    /// Test-only hook: spawn an observed ledger write that never completes, so
    /// tests can prove [`Self::ledger_writes_settled`] stays bounded when a
    /// recorder task wedges. Uses the same [`Self::spawn_observed_ledger_write`]
    /// accounting the production path uses, so the counters advance identically.
    #[cfg(test)]
    pub(crate) fn spawn_wedged_ledger_write_for_test(&self) {
        self.spawn_observed_ledger_write(std::future::pending::<()>());
    }

    #[cfg(test)]
    pub(crate) fn file_token_map_snapshot(&self) -> HashMap<String, u64> {
        crate::mcp::server::requests::recover_lock(&self.file_token_map).clone()
    }

    #[tracing::instrument(name = "mcp.ledger.record_error_analytics", level = "trace", skip_all)]
    pub(crate) fn record_mcp_tool_error_analytics(
        &self,
        request: McpToolErrorAnalyticsRequest<'_>,
    ) {
        let McpToolErrorAnalyticsRequest {
            project_root,
            session_id,
            tool_name,
            request_id,
            arguments,
            duration_us,
            error,
            connection_client_name,
            connection_instance_id,
        } = request;
        let LedgerSink::Mounted(gdb) = self.ledger_sink() else {
            return;
        };
        // `TraceDecayError`'s `Display` (via `thiserror`) already carries a
        // variant-classified, human-readable message (e.g. "config error:
        // missing required parameter: handle"); bounded truncation happens
        // in `mcp_tool_analytics_event`.
        let failure_reason = error.to_string();
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root,
            session_id,
            tool_name,
            outcome: "error",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us,
            timestamp: tracedecay_runtime_core::tracedecay::current_timestamp(),
            request_id,
            arguments,
            internal_analytics: None,
            client_name: connection_client_name,
            mcp_instance_id: connection_instance_id,
            failure_reason: Some(&failure_reason),
            cost: None,
        });
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = gdb.append_analytics_event(&event).await {
                tracing::warn!(error = %e, "MCP error analytics event insert failed");
            }
        });
    }

    /// Best-effort hook-route analytics after authoritative admission commit.
    ///
    /// Insert failures are logged only, they never alter
    /// [`HostAdmissionOutcome`]. The durable admission sequence is carried as
    /// the event idempotency identity, so identical but distinct admissions
    /// remain distinct analytics rows.
    #[tracing::instrument(name = "mcp.ledger.record_route_analytics", level = "trace", skip_all)]
    pub(crate) fn record_hook_route_analytics(
        &self,
        project_root: &std::path::Path,
        event: &hook_events::HookEvent,
        current_branch: Option<&str>,
        admission_seq: u64,
    ) {
        let Some(event) = hook_route_analytics_event(
            project_root,
            event,
            current_branch,
            tracedecay_runtime_core::tracedecay::current_timestamp(),
            admission_seq,
        ) else {
            return;
        };
        let LedgerSink::Mounted(gdb) = self.ledger_sink() else {
            return;
        };
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = gdb.append_analytics_event(&event).await {
                // Deliberate fail-open: admission already committed; telemetry
                // loss is preferred over blocking or rewriting host outcomes.
                tracing::warn!(error = %e, "hook route analytics insert failed");
            }
        });
    }

    /// Records a live session↔git span from one hook route notification.
    ///
    /// Route metadata carries `(session_id, thread_id, cwd, worktree,
    /// branch)`; when the route names a session and resolves to a registered
    /// project, this folds one [`SpanObservation`] into that project's
    /// `sessions.db` span table (see [`tracedecay_sessions::runtime::git_correlation`]).
    /// Mid-session branch/worktree switches are handled by the span table
    /// itself, the observation always carries the *current* branch.
    ///
    /// This analytics side write is intentionally fail-open: any resolution
    /// or DB error is dropped. Graph snapshots, git derivation, debounce, and
    /// the span write all run on the observed ledger path so the notification
    /// never waits on them. An in-process debounce keyed by
    /// `(provider, session, branch, worktree)` collapses a burst of tool-use
    /// events to one write per
    /// [`DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS`](tracedecay_sessions::runtime::git_correlation::DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
    /// (spans merge regardless, so a dropped observation only widens a span
    /// slightly less).
    #[tracing::instrument(name = "mcp.ledger.record_span_observation", level = "trace", skip_all)]
    pub(crate) fn record_hook_span_observation(
        self: &Arc<Self>,
        event: &hook_events::HookEvent,
        selected: &crate::mcp::project_route::ResolvedProjectRoute,
    ) {
        let Some(route) = event.route.as_ref() else {
            return;
        };

        let Some(session_id) = bounded_span_identifier(route.session_id.as_deref())
            .and_then(|value| tracedecay_privacy::protect_sensitive_structural_id(&value).ok())
        else {
            return;
        };
        let route_cwd = route.cwd.as_deref().or(event.cwd.as_deref());
        let Some(cwd) = route_cwd else {
            return;
        };
        let Ok(selected_server) = selected.retained_server() else {
            return;
        };
        let Some(db) = self.project_session_db.clone() else {
            return;
        };
        let thread_id = bounded_span_identifier(route.thread_id.as_deref())
            .and_then(|value| tracedecay_privacy::protect_sensitive_structural_id(&value).ok());
        let ts = tracedecay_runtime_core::tracedecay::current_timestamp();
        // Session-only pre-debounce: the full key needs branch/worktree, which
        // cost gix/git discovery. A burst for one session almost always shares
        // those, so reject here before paying for derivation. Mid-session
        // branch switches inside the window are dropped; spans merge anyway.
        let session_pre_key = format!("hook-span-session:{session_id}");
        let should_derive = self
            .span_observation_debounce
            .lock()
            .map_or(true, |mut debounce| {
                debounce.should_record(&session_pre_key, ts, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
            });
        if !should_derive {
            return;
        }
        let cwd = cwd.to_path_buf();
        let server = Arc::clone(self);
        self.spawn_observed_ledger_write(async move {
            let project_root = selected_server
                .cg_snapshot()
                .await
                .project_root()
                .to_path_buf();
            let active_project_root = server.cg_snapshot().await.project_root().to_path_buf();
            if RegisteredGlobalDb::canonical_project_key(&project_root)
                != RegisteredGlobalDb::canonical_project_key(&active_project_root)
            {
                return;
            }
            // Derive the worktree and branch from the freshly authorized cwd.
            // Route-provided worktree/branch strings are hints, not authority.
            // The derivation walks the filesystem (gix discovery) and may
            // spawn git, so it runs on the blocking pool, off the
            // notification hot path.
            let derived = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
                derive_hook_span_git_context(&cwd, project_root, &active_project_root)
            })
            .await;
            let Ok(Some((worktree, branch))) = derived else {
                return;
            };

            // Hook routes are provider-agnostic: leave provider empty.
            let key =
                git_correlation::span_debounce_key("", &session_id, branch.as_deref(), &worktree);
            let should_record =
                server
                    .span_observation_debounce
                    .lock()
                    .map_or(true, |mut debounce| {
                        debounce.should_record(&key, ts, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
                    });
            if !should_record {
                return;
            }

            let observation = SpanObservation {
                provider: String::new(),
                session_id,
                thread_id,
                branch,
                worktree,
                ts,
                source: SpanSource::HookRoute,
            };
            if let Err(e) = tracedecay_global_db::GlobalDbGitCorrelationStore::new(db)
                .record_span_observation(&observation, DEFAULT_SPAN_MERGE_GAP_SECS)
                .await
            {
                tracing::warn!(error = %e, "hook route span record failed");
            }
        });
    }

    pub(crate) fn spawn_observed_ledger_write<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.ledger_writes_started.fetch_add(1, Ordering::SeqCst);
        let finished = self.ledger_writes_finished.clone();
        let notify = self.ledger_write_notify.clone();
        let spawned = tracedecay_runtime_core::native_runtime::NativeTask::spawn(async move {
            future.await;
            finished.fetch_add(1, Ordering::SeqCst);
            notify.notify_waiters();
        });
        if let Err(error) = spawned {
            tracing::error!(%error, "savings-ledger write could not start");
            self.ledger_writes_finished.fetch_add(1, Ordering::SeqCst);
            self.ledger_write_notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_mcp::transport::JsonRpcRequest;

    use super::*;

    /// A completed tool response persists its tokens-saved accounting locally.
    #[tokio::test]
    async fn completed_response_persists_local_accounting() {
        let (cg, _project, authority) =
            super::super::writer_test_support::init_indexed_repo().await;
        let project_root = cg.project_root().to_path_buf();
        let context = super::super::writer_test_support::registered_context(cg, &authority);
        let server = super::super::McpServer::new_with_context(context).await;
        let tokens_saved = server.tokens_saved.as_ref().expect("fixture token counter");
        let baseline = tokens_saved.load(Ordering::Acquire);
        tokens_saved.store(baseline + 41, Ordering::Release);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "tracedecay_search",
                "arguments": { "query": "main" }
            })),
        };
        server
            .handle_request(&request)
            .await
            .expect("tool call should produce a response");
        server.ledger_writes_settled().await;
        let accounted = tokens_saved.load(Ordering::Acquire);
        server.spawn_token_accounting_persist(&project_root, "tracedecay_search", 41, 100);
        server.ledger_writes_settled().await;
        assert_eq!(
            server.cg_snapshot().await.get_tokens_saved().await.unwrap(),
            accounted + 41,
            "local tokens-saved accounting must stay persisted"
        );
        server.shutdown_background_tasks().await;
    }
}
