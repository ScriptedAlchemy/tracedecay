//! Token-savings ledger and tool/hook analytics recording.

use super::*;

/// Upper bound for [`McpServer::ledger_writes_settled`]. Savings-ledger writes
/// are fire-and-forget `SQLite` appends that finish in well under a second on a
/// healthy machine, so 10 s is generous headroom; the point is that the wait
/// is *finite* — a wedged recorder task can never hang the caller (tests,
/// shutdown drains) indefinitely as the previous unbounded loop allowed.
const LEDGER_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

fn configuration_authority_unavailable(detail: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("configuration authority unavailable: {detail}"),
    }
}

fn upload_enabled_from_desired_configuration(
    desired: &tracedecay_domain::configuration::ConfigurationSnapshotV1,
) -> Result<bool> {
    use tracedecay_domain::configuration::{
        ConfigurationValueV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY,
    };

    let key = SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY)
        .map_err(configuration_authority_unavailable)?;
    match desired.effective_values.get(&key) {
        Some(ConfigurationValueV1::Boolean(enabled)) => Ok(*enabled),
        Some(_) => Err(configuration_authority_unavailable(
            "desired upload setting is not boolean",
        )),
        None => Err(configuration_authority_unavailable(
            "desired upload setting is missing",
        )),
    }
}

// Global accounting (savings ledger + worldwide-counter flushes) is enabled
// by default; see `crate::global_db::global_accounting_mode` for the env
// override precedence.

/// Where this server's savings-ledger and analytics writes land.
///
/// A server built without an accounting database is legitimate (a direct
/// `McpServer::new` has no profile to account against), but it used to be
/// *silent*: each recorder independently checked both handles and returned
/// early, so a fixture that forgot to mount a database failed later as a
/// missing row in an assertion far from the construction that caused it.
/// Naming the state makes the absence observable at construction — see
/// [`McpServer::ledger_sink_is_mounted`] — and collapses three copies of
/// the same fallback into one resolution.
pub(crate) enum LedgerSink {
    Mounted(crate::global_db::RegisteredGlobalDbLeaseV1),
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

    /// Reads the upload policy from the daemon-retained desired configuration
    /// snapshot. There is deliberately no `config.toml` fallback: without the
    /// canonical authority, the upload decision is unavailable.
    pub(super) async fn canonical_upload_enabled(&self) -> Result<bool> {
        let cg = self.cg_snapshot().await;
        let desired = cg
            .configuration_runtime()
            .client()
            .current()
            .await
            .map_err(|error| {
                configuration_authority_unavailable(format!(
                    "cannot read desired upload setting: {error}"
                ))
            })?;
        upload_enabled_from_desired_configuration(&desired.snapshot)
    }

    /// Estimates the raw-file token cost ("before") for the given file
    /// paths from the cached file-token map (indexed file bytes / 4).
    /// Pure lookup — persists nothing.
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

    /// Adds `delta` saved tokens to the running counter and persists it.
    ///
    /// `delta` must already be the *net* saving for one call
    /// (`before.saturating_sub(after)`), not the gross raw-file estimate:
    /// crediting the full "before" would count a full-file read whose
    /// response contains the entire file as 100% saved.
    pub(crate) async fn persist_saved_tokens(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        let new_total = self.tokens_saved.fetch_add(delta, Ordering::Relaxed) + delta;
        let cg = self.cg_snapshot().await;
        // Persist to DB (best-effort, don't block on failure)
        let _ = cg.set_tokens_saved(new_total).await;
        // Also increment the resettable local counter
        let _ = cg.add_local_counter(delta).await;
        // Best-effort update to global DB
        if let LedgerSink::Mounted(gdb) = self.ledger_sink() {
            gdb.upsert(cg.project_root(), new_total).await;
        }
    }

    /// Resolves once every savings-ledger write spawned so far has
    /// completed (immediately when none are pending — including when global
    /// accounting is disabled and no writes are ever spawned).
    ///
    /// Test-only observability for the fire-and-forget ledger recorder:
    /// production code never calls this, so the request path stays
    /// non-blocking, while tests can await durability deterministically
    /// instead of polling the DB against a wall-clock deadline.
    ///
    /// Bounded by [`LEDGER_SETTLE_TIMEOUT`] so a spawned write that wedges
    /// (a stuck DB handle, a task that never resolves) can never hang the
    /// caller forever — the earlier unbounded loop made a wedged write
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
        if tokio::time::timeout(timeout, wait).await.is_ok() {
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

    /// Internal: snapshot of the current `file_token_map`. Exposed for
    /// integration tests only; not part of the stable public API.
    #[doc(hidden)]
    pub fn file_token_map_snapshot(&self) -> HashMap<String, u64> {
        crate::mcp::server::requests::recover_lock(&self.file_token_map).clone()
    }

    /// Flushes pending tokens to the worldwide counter if at least 30 seconds
    /// have elapsed since the last flush. Best-effort, never blocks for long.
    pub(crate) async fn maybe_flush_worldwide(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let last = self.last_flush_at.load(Ordering::Relaxed);
        if now - last < 30 {
            return;
        }
        // Mark as attempted immediately to prevent re-entry.
        self.last_flush_at.store(now, Ordering::Relaxed);

        let current = self.tokens_saved.load(Ordering::Relaxed);
        let last_flushed = self.last_flushed_tokens.load(Ordering::Relaxed);
        if current <= last_flushed {
            return;
        }
        let delta = current - last_flushed;

        if !self.ledger_sink_is_mounted() {
            return;
        }

        let upload_enabled = match self.canonical_upload_enabled().await {
            Ok(enabled) => enabled,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "worldwide counter upload skipped because configuration authority is unavailable"
                );
                return;
            }
        };

        let success = tokio::task::spawn_blocking(move || {
            let mut config = crate::user_config::UserConfig::load();
            config.pending_upload += delta;
            if upload_enabled && crate::cloud::flush_pending(config.pending_upload).is_some() {
                config.pending_upload = 0;
                config.last_upload_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if let Err(err) = config.save() {
                    tracing::warn!(error = %err, "could not save upload config");
                }
                return true;
            }
            if let Err(err) = config.save() {
                tracing::warn!(error = %err, "could not save upload config");
            }
            false
        })
        .await
        .unwrap_or(false);

        if success {
            self.last_flushed_tokens.store(current, Ordering::Relaxed);
        }
    }

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
            timestamp: crate::tracedecay::current_timestamp(),
            request_id,
            arguments,
            internal_analytics: None,
            client_name: connection_client_name,
            mcp_instance_id: connection_instance_id,
            failure_reason: Some(&failure_reason),
        });
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = gdb.append_analytics_event(&event).await {
                tracing::warn!(error = %e, "MCP error analytics event insert failed");
            }
        });
    }

    /// Best-effort hook-route analytics after authoritative admission commit.
    ///
    /// Insert failures are logged only — they never alter
    /// [`HostAdmissionOutcome`]. The durable admission sequence is carried as
    /// the event idempotency identity, so identical but distinct admissions
    /// remain distinct analytics rows.
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
            crate::tracedecay::current_timestamp(),
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
    /// itself — the observation always carries the *current* branch.
    ///
    /// This analytics side write is intentionally fail-open: any resolution
    /// or DB error is dropped. An in-process debounce keyed by
    /// `(provider, session, branch, worktree)` collapses a burst of tool-use
    /// events to one write per
    /// [`DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS`](tracedecay_sessions::runtime::git_correlation::DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
    /// so the notification hot path never blocks on repeated writes (spans
    /// merge regardless, so a dropped observation only widens a span slightly
    /// less).
    pub(crate) async fn record_hook_span_observation(
        &self,
        event: &hook_events::HookEvent,
        selected: &crate::mcp::project_route::ResolvedProjectRoute,
    ) {
        const MAX_SPAN_IDENTIFIER_BYTES: usize = 256;

        let Some(route) = event.route.as_ref() else {
            return;
        };

        let bounded_identifier = |value: Option<&str>| {
            value
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= MAX_SPAN_IDENTIFIER_BYTES
                        && !value.chars().any(char::is_control)
                })
                .map(str::to_string)
        };
        let Some(session_id) = bounded_identifier(route.session_id.as_deref())
            .and_then(|value| crate::privacy::protect_sensitive_structural_id(&value).ok())
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
        let project_root = selected_server
            .cg_snapshot()
            .await
            .project_root()
            .to_path_buf();
        let active_project_root = self.cg_snapshot().await.project_root().to_path_buf();
        if RegisteredGlobalDb::canonical_project_key(&project_root)
            != RegisteredGlobalDb::canonical_project_key(&active_project_root)
        {
            return;
        }
        let Some(db) = self.session_db.clone() else {
            return;
        };

        // Derive the worktree and branch from the freshly authorized cwd.
        // Route-provided worktree/branch strings are hints, not authority.
        let worktree_raw =
            crate::worktree::git_worktree_root(cwd).unwrap_or_else(|| project_root.clone());
        let Ok(worktree_raw) =
            hook_events::authorize_add_branch_at_root(&worktree_raw, &active_project_root)
        else {
            return;
        };
        let worktree = git_correlation::normalize_worktree(&worktree_raw.to_string_lossy());
        let branch = bounded_identifier(crate::branch::current_branch(&worktree_raw).as_deref());
        let thread_id = bounded_identifier(route.thread_id.as_deref())
            .and_then(|value| crate::privacy::protect_sensitive_structural_id(&value).ok());
        let ts = crate::tracedecay::current_timestamp();

        // Hook routes are provider-agnostic: leave provider empty.
        let key = git_correlation::span_debounce_key("", &session_id, branch.as_deref(), &worktree);
        let should_record = self
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
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = crate::store::GlobalDbGitCorrelationStore::new(db)
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
        tokio::spawn(async move {
            future.await;
            finished.fetch_add(1, Ordering::SeqCst);
            notify.notify_waiters();
        });
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::configuration::{
        ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY,
    };

    use super::*;

    fn desired_configuration() -> ConfigurationSnapshotV1 {
        let registry =
            crate::config::registry::ConfigurationRegistry::core().expect("configuration registry");
        crate::config::resolver::resolve_configuration(&registry, &[])
            .expect("default desired configuration")
            .snapshot
    }

    #[test]
    fn upload_setting_comes_from_the_desired_configuration_snapshot() {
        let mut desired = desired_configuration();
        let key = SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).expect("canonical setting key");
        desired
            .effective_values
            .insert(key, ConfigurationValueV1::Boolean(true));

        assert!(
            upload_enabled_from_desired_configuration(&desired)
                .expect("desired boolean setting must be readable")
        );
    }

    #[test]
    fn missing_desired_upload_setting_is_typed_unavailable() {
        let mut desired = desired_configuration();
        let key = SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).expect("canonical setting key");
        desired.effective_values.remove(&key);

        assert!(matches!(
            upload_enabled_from_desired_configuration(&desired),
            Err(TraceDecayError::Config { message })
                if message.starts_with("configuration authority unavailable")
        ));
    }

    #[test]
    fn non_boolean_desired_upload_setting_is_typed_unavailable() {
        let mut desired = desired_configuration();
        let key = SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).expect("canonical setting key");
        desired
            .effective_values
            .insert(key, ConfigurationValueV1::Unsigned(1));

        assert!(matches!(
            upload_enabled_from_desired_configuration(&desired),
            Err(TraceDecayError::Config { message })
                if message.starts_with("configuration authority unavailable")
        ));
    }
}
