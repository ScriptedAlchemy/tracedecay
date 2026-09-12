//! Connection lifecycle: the JSON-RPC read/write loop, shutdown
//! policy, and daemon-owned host-admission replay driving.

use super::*;

pub(super) const MAX_CONCURRENT_CONNECTION_READS: usize =
    crate::daemon::MAX_CONCURRENT_REQUESTS_PER_DAEMON_CLIENT;

/// The single production context the portable MCP connection scheduler uses.
///
/// It borrows the already-constructed server authorities; it does not own a
/// registry, route cache, or cancellation table of its own.
pub(crate) struct ProductionMcpConnectionContext {
    server: Arc<McpServer>,
    admission: Option<Arc<crate::daemon::ParkableConnectionAdmission>>,
}

impl ProductionMcpConnectionContext {
    pub(crate) fn new(server: Arc<McpServer>) -> Arc<Self> {
        Arc::new(Self {
            server,
            admission: crate::daemon::current_connection_admission(),
        })
    }
}

impl tracedecay_mcp::server::McpConnectionContext for ProductionMcpConnectionContext {
    type Connection = ConnectionRouteState;

    fn new_connection(&self) -> Result<Self::Connection> {
        self.server.new_connection_route_state()
    }

    fn timings_enabled(&self) -> bool {
        self.server.timings_enabled()
    }

    fn build_version(&self) -> Result<&'static str> {
        crate::version::build_version().map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })
    }

    fn max_concurrent_reads(&self) -> usize {
        MAX_CONCURRENT_CONNECTION_READS
    }

    fn tool_is_read_only(&self, tool_name: &str) -> bool {
        tracedecay_mcp::tools::binding::mcp_dispatch_contract(tool_name)
            .is_ok_and(tracedecay_tool_catalog::McpDispatchContractV1::read_only)
    }

    fn tool_supports_live_cancellation(&self, tool_name: &str) -> bool {
        super::requests::tool_supports_live_cancellation(tool_name)
    }

    fn dispatch<'a>(
        &'a self,
        request: tracedecay_mcp::server::McpDispatchRequest<'a>,
        timings_enabled: bool,
        connection: &'a mut Self::Connection,
        pre_cancelled: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<JsonRpcResponse>> + Send + 'a>>
    {
        Box::pin(
            self.server
                .dispatch_envelope(request, timings_enabled, connection, pre_cancelled),
        )
    }

    fn cancel_request(&self, id: &Value, connection_scope: &str) -> bool {
        self.server
            .cancel_application_surface_request(id, connection_scope)
    }

    fn cancellation_registered(&self) -> &tokio::sync::Notify {
        self.server.dispatch_authority.cancellation_registered()
    }

    fn take_pending_notifications(&self) -> Vec<Value> {
        super::requests::recover_lock(&self.server.pending_notifications)
            .drain(..)
            .collect()
    }

    fn run_in_connection_admission<'a, T, F>(
        &'a self,
        future: F,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
    where
        T: Send + 'a,
        F: std::future::Future<Output = T> + Send + 'a,
    {
        Box::pin(crate::daemon::in_connection_admission(
            self.admission.clone(),
            future,
        ))
    }

    fn shutdown(
        self: Arc<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move { McpServer::shutdown(&self.server).await })
    }
}

impl McpServer {
    fn connection_server(
        self: &Arc<Self>,
    ) -> Arc<tracedecay_mcp::server::McpConnectionServer<ProductionMcpConnectionContext>> {
        tracedecay_mcp::server::McpConnectionServer::new(ProductionMcpConnectionContext::new(
            Arc::clone(self),
        ))
    }

    #[hotpath::skip]
    pub async fn run(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
    ) -> Result<()> {
        self.connection_server().run(transport).await
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[hotpath::skip]
    pub async fn run_connection(
        &self,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
    ) -> Result<()> {
        let server = self.dispatch_authority.server().upgrade().ok_or_else(|| {
            TraceDecayError::project_route(
                "tool_dispatch_shutdown",
                true,
                "MCP server was released before connection dispatch",
            )
        })?;
        server.connection_server().run_connection(transport).await
    }

    #[hotpath::skip]
    pub(crate) async fn run_daemon_connection_with_timings(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        timings_enabled: bool,
        lifecycle: &dyn tracedecay_mcp::McpConnectionLifecyclePort,
    ) -> Result<()> {
        self.connection_server()
            .run_daemon_connection_with_timings(transport, timings_enabled, lifecycle)
            .await
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn run_with_shutdown_policy(
        self: &Arc<Self>,
        transport: &mut impl tracedecay_mcp::transport::McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&dyn tracedecay_mcp::McpConnectionLifecyclePort>,
    ) -> Result<()> {
        self.connection_server()
            .run_with_shutdown_policy(
                transport,
                shutdown_on_exit,
                listen_for_process_signals,
                timings_override,
                request_lifecycle,
            )
            .await
    }

    /// Persists the tokens-saved counter, flushes pending tokens to the
    /// worldwide counter, checkpoints the WAL, and logs a session summary.
    ///
    /// Idempotent — safe to call multiple times. `run` invokes it once when
    /// its main loop exits; callers (e.g. `main.rs`, tests) may invoke it
    /// explicitly afterwards without re-running the persistence logic.
    #[hotpath::skip]
    pub async fn shutdown(self: &Arc<Self>) {
        let deadline =
            tokio::time::Instant::now() + tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;
        let status = self.shutdown_until(deadline).await;
        if !status.is_clean() {
            tracing::warn!(?status, "MCP server shutdown did not complete cleanly");
        }
    }

    #[hotpath::measure(label = "mcp.server.shutdown", future = true)]
    pub(crate) async fn shutdown_until(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        self.shutdown
            .coordinate_until(deadline, Arc::clone(self).run_shutdown(deadline))
            .await
    }

    #[hotpath::skip]
    async fn run_shutdown(
        self: Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        let mut failures = self.shutdown_background_tasks_until(deadline).await;

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self
            .tokens_saved
            .as_ref()
            .map(|tokens| tokens.load(Ordering::Relaxed));

        let cg = self.cg_snapshot().await;
        if let Some(tokens_saved) = tokens_saved {
            if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
                tracing::warn!(error = %e, "failed to persist tokens saved during shutdown");
                failures.push(format!("persist tokens saved: {e}"));
            }

            // A failed global-ledger flush joins the shutdown failure report
            // beside the local persistence failures instead of vanishing.
            if let Some(gdb) = self.accounting_db.as_ref().or(self.global_db.as_ref()) {
                if let Err(error) = gdb
                    .try_upsert_project_tokens(cg.project_root(), tokens_saved)
                    .await
                {
                    tracing::warn!(error = %error, "failed to flush tokens saved to the global ledger during shutdown");
                    failures.push(format!("flush global ledger tokens saved: {error}"));
                }
                gdb.checkpoint().await;
            }

            // Flush remaining delta to worldwide counter (what periodic flushes missed).
            if let Some(last_flushed_tokens) = self.last_flushed_tokens.as_ref() {
                let last_flushed = last_flushed_tokens.load(Ordering::Relaxed);
                if (self.accounting_db.is_some() || self.global_db.is_some())
                    && tokens_saved > last_flushed
                {
                    let delta = tokens_saved - last_flushed;
                    match self.canonical_upload_enabled().await {
                        Ok(upload_enabled) => {
                            let mut config =
                                tracedecay_session_memory::user_config::UserConfig::load();
                            config.pending_upload += delta;
                            if upload_enabled
                                && let Some(_total) = tracedecay_dashboard_api::cloud::flush_pending(
                                    config.pending_upload,
                                )
                            {
                                config.pending_upload = 0;
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64;
                                config.last_upload_at = now;
                            }
                            if let Err(err) = config.save() {
                                tracing::warn!(error = %err, "could not save upload config during shutdown");
                            }
                        }
                        Err(error) => failures.push(format!(
                            "worldwide counter upload configuration unavailable: {error}"
                        )),
                    }
                }
            }
        }

        // Checkpoint WAL to merge it into the main database file
        if let Err(e) = cg.checkpoint().await {
            tracing::warn!(error = %e, "failed to checkpoint WAL during shutdown");
            failures.push(format!("code graph checkpoint: {e}"));
        }

        if failures.is_empty() {
            tracing::info!(
                tool_calls,
                ?tokens_saved,
                uptime_secs = uptime.as_secs(),
                "MCP server shutdown complete"
            );
            crate::daemon::ShutdownStatus::Clean
        } else {
            crate::daemon::ShutdownStatus::Failed(failures.join("; "))
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[hotpath::skip]
    pub(crate) async fn shutdown_background_tasks(&self) {
        let failures = self
            .shutdown_background_tasks_until(
                tokio::time::Instant::now() + tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE,
            )
            .await;
        if !failures.is_empty() {
            tracing::warn!(
                failures = failures.join("; "),
                "MCP background shutdown did not complete cleanly"
            );
        }
    }

    #[hotpath::skip]
    async fn shutdown_background_tasks_until(
        &self,
        _deadline: tokio::time::Instant,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        // The hosted dashboard is daemon-process state (`DASHBOARD_MANAGER`),
        // not project-server state. `tracedecay_dashboard` starts against the
        // Core server; the later Core→Full remount retires that server. Tearing
        // the listener down here made a fresh bind report a URL that died as
        // soon as full capability published.
        failures.extend(self.background_tasks.shutdown().await);
        self.dispatch_authority.shutdown().await;
        if let Some(worker) = self.project_host_admission_replay.lock().await.take() {
            worker.shutdown().await;
        }
        self.shutdown_startup_catch_up_sync().await;
        failures
    }

    #[hotpath::skip]
    #[expect(
        clippy::too_many_lines,
        reason = "Host-admission replay is one identity-bind and session-catch-up for the connection."
    )]
    pub(crate) async fn replay_host_admission(
        &self,
        target_seq: Option<u64>,
    ) -> HostAdmissionOutcome {
        const MAX_RECORDS_PER_PASS: usize = 64;

        let Some(broker) = self.host_admission_broker.as_ref() else {
            return HostAdmissionOutcome::retained_unavailable("spool_unavailable");
        };
        let replay = match broker.begin_replay().await {
            Ok(replay) => replay,
            Err(outcome) => return outcome,
        };
        let mut attempted = HashSet::new();
        let mut blocked_sources = HashSet::new();
        let mut retained_leases = Vec::new();
        let mut non_committed_outcome = None;
        let mut target_outcome = None;
        let mut terminal_outcome = None;
        for _ in 0..MAX_RECORDS_PER_PASS {
            let record = match replay.lease_next().await {
                Ok(Some(record)) => record,
                Ok(None) => break,
                Err(outcome) => {
                    terminal_outcome = Some(outcome);
                    break;
                }
            };
            if blocked_sources.contains(&record.source) {
                retained_leases.push(record.seq);
                continue;
            }
            if !attempted.insert(record.seq) {
                let outcome = HostAdmissionOutcome::spool_ack_conflict();
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                non_committed_outcome.get_or_insert(outcome.clone());
                if target_seq == Some(record.seq) {
                    target_outcome = Some(outcome);
                }
                continue;
            }
            let plan = match hook_events::decode_durable_hook_event_plan(&record.payload) {
                Ok(plan) => plan,
                Err(hook_events::DurableHookEventDecodeError::UnsupportedVersion) => {
                    let outcome = HostAdmissionOutcome::durable_payload_unsupported_version();
                    blocked_sources.insert(record.source);
                    retained_leases.push(record.seq);
                    non_committed_outcome.get_or_insert(outcome.clone());
                    if target_seq == Some(record.seq) {
                        target_outcome = Some(outcome);
                    }
                    continue;
                }
                Err(hook_events::DurableHookEventDecodeError::Malformed) => {
                    let outcome = HostAdmissionOutcome::durable_payload_malformed();
                    match replay
                        .quarantine(record.seq, TerminalReason::MalformedPayload)
                        .await
                    {
                        Ok(_) => {
                            non_committed_outcome.get_or_insert(outcome.clone());
                            if target_seq == Some(record.seq) {
                                target_outcome = Some(outcome);
                            }
                        }
                        Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                            blocked_sources.insert(record.source);
                            retained_leases.push(record.seq);
                            non_committed_outcome.get_or_insert(failure.clone());
                            if target_seq == Some(record.seq) {
                                target_outcome = Some(failure);
                            }
                        }
                        Err(failure) => {
                            terminal_outcome = Some(failure);
                            break;
                        }
                    }
                    continue;
                }
            };
            let cg = self.reopen_if_branch_drifted().await;
            let root = cg.project_root().to_path_buf();
            let canonical_outcome = Box::pin(self.run_hook_event_plan(cg, &root, plan)).await;
            let outcome = if canonical_outcome.reason_code == Some("stale_branch_authorization")
                && !canonical_outcome.retryable
            {
                match replay
                    .quarantine(record.seq, TerminalReason::StaleBranchAuthorization)
                    .await
                {
                    Ok(_) => {
                        non_committed_outcome.get_or_insert(canonical_outcome.clone());
                        canonical_outcome
                    }
                    Err(failure) if failure == HostAdmissionOutcome::quarantine_full() => {
                        blocked_sources.insert(record.source);
                        retained_leases.push(record.seq);
                        non_committed_outcome.get_or_insert(failure.clone());
                        failure
                    }
                    Err(failure) => {
                        terminal_outcome = Some(failure);
                        break;
                    }
                }
            } else if matches!(
                canonical_outcome.status,
                HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
            ) {
                match replay.commit(record.seq).await {
                    Ok(_) => canonical_outcome,
                    Err(outcome) => {
                        terminal_outcome = Some(outcome);
                        break;
                    }
                }
            } else {
                blocked_sources.insert(record.source);
                retained_leases.push(record.seq);
                non_committed_outcome.get_or_insert(canonical_outcome.clone());
                canonical_outcome
            };
            if target_seq == Some(record.seq) {
                target_outcome = Some(outcome);
            }
        }
        for seq in retained_leases.into_iter().rev() {
            if let Err(outcome) = replay.defer(seq).await {
                return outcome;
            }
        }
        terminal_outcome
            .or(target_outcome)
            .or(non_committed_outcome)
            .unwrap_or_else(HostAdmissionOutcome::accepted_for_replay)
    }

    pub(crate) fn report_host_admission_outcome(outcome: &HostAdmissionOutcome) {
        if outcome.status.is_replay_progress() {
            return;
        }
        tracing::warn!(
            reason_code = outcome.reason_code.unwrap_or("host_admission_unavailable"),
            "host admission did not make replay progress"
        );
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn wait_project_host_admission_replay_idle(&self, timeout: Duration) -> bool {
        let worker = self
            .project_host_admission_replay
            .lock()
            .await
            .as_ref()
            .map(|task| Arc::clone(task.worker()));
        match worker {
            Some(worker) => worker.wait_idle(timeout).await,
            None => true,
        }
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn project_host_admission_replay_pass_count(&self) -> usize {
        let guard = self.project_host_admission_replay.lock().await;
        guard.as_ref().map_or(
            0,
            tracedecay_mcp::server::ProjectHostAdmissionReplayTask::pass_count,
        )
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn project_host_admission_replay_backoff_count(&self) -> usize {
        let guard = self.project_host_admission_replay.lock().await;
        guard.as_ref().map_or(
            0,
            tracedecay_mcp::server::ProjectHostAdmissionReplayTask::backoff_count,
        )
    }
}

#[cfg(test)]
mod cancellable_queue_tests {
    use super::*;

    static DELAYED_ROUTE_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct DelayedRouteFixture {
        _fixture_guard: tokio::sync::MutexGuard<'static, ()>,
        _isolation: tempfile::TempDir,
        harness: crate::daemon::ProductionProjectCompositionHarnessV1,
        caller: Arc<McpServer>,
        target_project_id: String,
        route_started: Arc<std::sync::atomic::AtomicUsize>,
        route_release: Arc<tokio::sync::Semaphore>,
    }

    impl DelayedRouteFixture {
        async fn new() -> Self {
            let fixture_guard = DELAYED_ROUTE_FIXTURE_LOCK.lock().await;
            crate::product_runtime::register_fixture_product_runtime();
            let isolation = tempfile::TempDir::new().expect("route concurrency isolation");
            let active_root = isolation.path().join("active");
            let target_root = isolation.path().join("target");
            for root in [&active_root, &target_root] {
                std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
                std::fs::write(root.join("src/lib.rs"), "pub fn route_fixture() {}\n")
                    .expect("fixture source");
                super::super::writer_test_support::git(root, &["init", "-q", "-b", "main"]);
                super::super::writer_test_support::git(
                    root,
                    &["config", "user.email", "route@test.invalid"],
                );
                super::super::writer_test_support::git(
                    root,
                    &["config", "user.name", "Route Test"],
                );
                super::super::writer_test_support::git(root, &["add", "."]);
                super::super::writer_test_support::git(root, &["commit", "-q", "-m", "fixture"]);
            }
            let harness = crate::daemon::ProductionProjectCompositionHarnessV1::open(
                isolation.path(),
                [active_root.clone(), target_root.clone()],
            )
            .await
            .expect("production route composition");
            let mounted_active = harness.server(&active_root).expect("mounted active server");
            let target = harness.server(&target_root).expect("mounted target server");
            let target_project_id = target
                .cg_snapshot()
                .await
                .store_layout()
                .identity
                .project_id
                .clone()
                .expect("target project identity");
            let route_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let route_release = Arc::new(tokio::sync::Semaphore::new(0));
            let resolver_target = Arc::clone(&target);
            let resolver_started = Arc::clone(&route_started);
            let resolver_release = Arc::clone(&route_release);
            let resolver: super::super::RetainedProjectServerResolver =
                super::super::install_retained_project_server_resolver(move |_request| {
                    let target = Arc::clone(&resolver_target);
                    let started = Arc::clone(&resolver_started);
                    let release = Arc::clone(&resolver_release);
                    Box::pin(async move {
                        started.fetch_add(1, Ordering::AcqRel);
                        let permit = release.acquire().await.map_err(|error| {
                            tracedecay_domain::errors::TraceDecayError::Config {
                                message: format!("route concurrency gate closed: {error}"),
                            }
                        })?;
                        permit.forget();
                        Ok(Some(target))
                    })
                });
            let context = super::super::McpServerConstructionContext::direct(
                mounted_active.cg_snapshot().await,
                None,
            )
            .with_direct_databases(
                mounted_active.global_db.clone(),
                mounted_active.registry_db.clone(),
                mounted_active.project_session_db.clone(),
                mounted_active.profile_session_db.clone(),
            )
            .with_retained_project_server_resolver(resolver);
            let caller = super::super::McpServer::new_with_context(context).await;
            Self {
                _fixture_guard: fixture_guard,
                _isolation: isolation,
                harness,
                caller,
                target_project_id,
                route_started,
                route_release,
            }
        }

        async fn wait_for_routes(&self, expected: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.route_started.load(Ordering::Acquire) < expected {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("selected requests did not enter route resolution");
        }
    }

    struct ObservedTransport {
        inner: tracedecay_mcp::transport::ChannelTransport,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Clone, Default)]
    struct TestConnectionLifecycle {
        accepting: Arc<AtomicBool>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        draining: Arc<tokio::sync::Notify>,
    }

    struct TestRequestActivity(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for TestRequestActivity {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    impl TestConnectionLifecycle {
        fn accepting() -> Self {
            Self {
                accepting: Arc::new(AtomicBool::new(true)),
                ..Self::default()
            }
        }

        fn begin_draining(&self) {
            self.accepting.store(false, Ordering::Release);
            self.draining.notify_waiters();
        }
    }

    impl tracedecay_mcp::McpConnectionLifecyclePort for TestConnectionLifecycle {
        fn accepting(&self) -> bool {
            self.accepting.load(Ordering::Acquire)
        }

        fn try_enter(&self) -> Option<tracedecay_mcp::McpRequestActivity> {
            if !self.accepting() {
                return None;
            }
            self.active.fetch_add(1, Ordering::AcqRel);
            if self.accepting() {
                return Some(tracedecay_mcp::McpRequestActivity::retain(
                    TestRequestActivity(Arc::clone(&self.active)),
                ));
            }
            self.active.fetch_sub(1, Ordering::AcqRel);
            None
        }

        fn wait_for_draining(&self) -> tracedecay_mcp::McpLifecycleDrainFuture<'_> {
            Box::pin(async move {
                while self.accepting() {
                    self.draining.notified().await;
                }
            })
        }
    }

    impl tracedecay_mcp::transport::McpTransport for ObservedTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            let line = self.inner.read_line().await?;
            if line.is_some() {
                self.reads.fetch_add(1, Ordering::Release);
            }
            Ok(line)
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.inner.write_line(line).await
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush().await
        }

        fn peer_fully_closed_after_eof(
            &self,
        ) -> impl std::future::Future<Output = ()> + Send + 'static {
            self.inner.peer_fully_closed_after_eof()
        }
    }

    async fn wait_for_transport_reads(reads: &std::sync::atomic::AtomicUsize, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while reads.load(Ordering::Acquire) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport did not consume the expected request lines");
    }

    async fn receive_response(
        responses: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = responses.recv().await.expect("connection response");
                let value: Value =
                    serde_json::from_str(line.trim()).expect("connection response JSON");
                if value.get("id").is_some() {
                    return value;
                }
            }
        })
        .await
        .expect("connection response timeout")
    }

    #[tokio::test]
    async fn independent_reads_complete_out_of_order_with_exact_ids() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "slow-read",
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send delayed selected read");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send independent status read");

        let fast = receive_response(&mut responses).await;
        assert_eq!(
            fast["id"],
            serde_json::json!(2),
            "the independent status read must not wait behind route resolution: {fast}"
        );

        fixture.route_release.add_permits(1);
        let slow = receive_response(&mut responses).await;
        assert_eq!(
            slow["id"],
            serde_json::json!("slow-read"),
            "the delayed response must preserve the string request id: {slow}"
        );

        drop(sender);
        serving
            .await
            .expect("join concurrent connection")
            .expect("serve concurrent connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn ordinary_connection_read_has_one_connection_task_owner() {
        let fixture = DelayedRouteFixture::new().await;
        let registry = fixture.caller.dispatch_authority.registry();
        let retained_before = registry.retained_spawn_count_for_test();
        let connection_owned_before = registry.connection_owned_count_for_test();
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send ordinary connection read");
        assert_eq!(receive_response(&mut responses).await["id"], json!(3));

        assert_eq!(
            registry.retained_spawn_count_for_test(),
            retained_before,
            "the connection's active-read task must be the sole task owner"
        );
        assert_eq!(
            registry.connection_owned_count_for_test(),
            connection_owned_before + 1,
            "one inline registry lease must cover the ordinary read"
        );

        drop(sender);
        serving
            .await
            .expect("join ordinary read connection")
            .expect("serve ordinary read connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn effect_request_is_a_barrier_for_reads_on_both_sides() {
        let fixture = DelayedRouteFixture::new().await;
        let (inner_transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let transport_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut transport = ObservedTransport {
            inner: inner_transport,
            reads: Arc::clone(&transport_reads),
        };
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read before effect");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_fact_store_add",
                        "arguments": {
                            "content": "effect barrier fixture",
                            "category": "project",
                            "trust": 0.9,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("send effect barrier");
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send read after effect");
        wait_for_transport_reads(&transport_reads, 3).await;
        assert_eq!(
            fixture.route_started.load(Ordering::Acquire),
            1,
            "the effect must not begin before the preceding read settles"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), responses.recv())
                .await
                .is_err(),
            "no request behind the blocked read/effect barrier may answer"
        );

        fixture.route_release.add_permits(1);
        assert_eq!(receive_response(&mut responses).await["id"], json!(10));
        assert_eq!(
            receive_response(&mut responses).await["id"],
            json!(11),
            "the effect must settle after the preceding read"
        );
        assert_eq!(
            receive_response(&mut responses).await["id"],
            json!(12),
            "the later read must not overtake the effect"
        );

        drop(sender);
        serving
            .await
            .expect("join effect barrier connection")
            .expect("serve effect barrier connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn independent_read_work_is_bounded_by_daemon_per_client_admission() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        for id in 1..=MAX_CONCURRENT_CONNECTION_READS + 1 {
            sender
                .send(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/call",
                        "params": {
                            "name": "tracedecay_grep",
                            "arguments": {
                                "pattern": "route_fixture",
                                "fixed_strings": true,
                                "project_selector": {
                                    "project_id": fixture.target_project_id.clone()
                                },
                                "format": "json"
                            }
                        }
                    })
                    .to_string(),
                )
                .expect("send bounded read");
        }

        fixture
            .wait_for_routes(MAX_CONCURRENT_CONNECTION_READS)
            .await;
        tokio::task::yield_now().await;
        assert_eq!(
            fixture.route_started.load(Ordering::Acquire),
            MAX_CONCURRENT_CONNECTION_READS,
            "one connection must derive its active-read cap from daemon per-client admission"
        );

        fixture.route_release.add_permits(1);
        let _ = receive_response(&mut responses).await;
        fixture
            .wait_for_routes(MAX_CONCURRENT_CONNECTION_READS + 1)
            .await;
        fixture
            .route_release
            .add_permits(MAX_CONCURRENT_CONNECTION_READS);

        let mut response_ids = HashSet::new();
        while response_ids.len() < MAX_CONCURRENT_CONNECTION_READS {
            response_ids.insert(receive_response(&mut responses).await["id"].clone());
        }
        assert_eq!(
            response_ids.len(),
            MAX_CONCURRENT_CONNECTION_READS,
            "every admitted and backpressured read must receive one response"
        );

        drop(sender);
        serving
            .await
            .expect("join bounded connection")
            .expect("serve bounded connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn notification_is_an_ordering_barrier_for_later_reads() {
        let fixture = DelayedRouteFixture::new().await;
        let (mut transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read before notification");
        fixture.wait_for_routes(1).await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                })
                .to_string(),
            )
            .expect("send ordering notification");
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": {"admission_only": true}
                    }
                })
                .to_string(),
            )
            .expect("send read after notification");

        assert!(
            tokio::time::timeout(Duration::from_millis(50), responses.recv())
                .await
                .is_err(),
            "the later read must not overtake an ordered notification"
        );
        fixture.route_release.add_permits(1);
        assert_eq!(receive_response(&mut responses).await["id"], json!(20));
        assert_eq!(receive_response(&mut responses).await["id"], json!(21));

        drop(sender);
        serving
            .await
            .expect("join notification barrier connection")
            .expect("serve notification barrier connection");
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn daemon_drain_cancels_and_joins_concurrent_reads() {
        let fixture = DelayedRouteFixture::new().await;
        let lifecycle = TestConnectionLifecycle::accepting();
        let (mut transport, sender, _responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let serving = tokio::spawn({
            let caller = Arc::clone(&fixture.caller);
            let lifecycle = lifecycle.clone();
            async move {
                caller
                    .run_with_shutdown_policy(&mut transport, false, false, None, Some(&lifecycle))
                    .await
            }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 30,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {
                                "project_id": fixture.target_project_id.clone()
                            },
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send read held across drain");
        fixture.wait_for_routes(1).await;
        assert_eq!(lifecycle.active.load(Ordering::Acquire), 1);

        lifecycle.begin_draining();
        tokio::time::timeout(Duration::from_secs(5), serving)
            .await
            .expect("draining connection did not join active reads")
            .expect("join draining connection")
            .expect("serve draining connection");
        assert_eq!(
            lifecycle.active.load(Ordering::Acquire),
            0,
            "shutdown drain must release every admitted request activity"
        );

        drop(sender);
        fixture.harness.shutdown().await;
    }

    #[tokio::test]
    async fn cancellation_during_route_resolution_reaches_selected_live_target() {
        let _fixture_guard = DELAYED_ROUTE_FIXTURE_LOCK.lock().await;
        let isolation = tempfile::TempDir::new().expect("route cancellation isolation");
        let active_root = isolation.path().join("active");
        let target_root = isolation.path().join("target");
        for root in [&active_root, &target_root] {
            std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
            std::fs::write(root.join("src/lib.rs"), "pub fn route_fixture() {}\n")
                .expect("fixture source");
            super::super::writer_test_support::git(root, &["init", "-q", "-b", "main"]);
            super::super::writer_test_support::git(
                root,
                &["config", "user.email", "route@test.invalid"],
            );
            super::super::writer_test_support::git(root, &["config", "user.name", "Route Test"]);
            super::super::writer_test_support::git(root, &["add", "."]);
            super::super::writer_test_support::git(root, &["commit", "-q", "-m", "fixture"]);
        }
        let harness = crate::daemon::ProductionProjectCompositionHarnessV1::open(
            isolation.path(),
            [active_root.clone(), target_root.clone()],
        )
        .await
        .expect("production route composition");
        let mounted_active = harness.server(&active_root).expect("mounted active server");
        let target = harness.server(&target_root).expect("mounted target server");
        let target_project_id = target
            .cg_snapshot()
            .await
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("target project identity");

        let route_entered = Arc::new(tokio::sync::Notify::new());
        let release_route = Arc::new(tokio::sync::Notify::new());
        let resolver_target = Arc::clone(&target);
        let resolver_entered = Arc::clone(&route_entered);
        let resolver_release = Arc::clone(&release_route);
        let resolver: super::super::RetainedProjectServerResolver =
            super::super::install_retained_project_server_resolver(move |_request| {
                let target = Arc::clone(&resolver_target);
                let entered = Arc::clone(&resolver_entered);
                let release = Arc::clone(&resolver_release);
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(Some(target))
                })
            });
        let context = super::super::McpServerConstructionContext::direct(
            mounted_active.cg_snapshot().await,
            None,
        )
        .with_direct_databases(
            mounted_active.global_db.clone(),
            mounted_active.registry_db.clone(),
            mounted_active.project_session_db.clone(),
            mounted_active.profile_session_db.clone(),
        )
        .with_retained_project_server_resolver(resolver);
        let caller = super::super::McpServer::new_with_context(context).await;
        let (inner_transport, sender, mut responses) =
            tracedecay_mcp::transport::ChannelTransport::new();
        let transport_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut transport = ObservedTransport {
            inner: inner_transport,
            reads: Arc::clone(&transport_reads),
        };
        let serving = tokio::spawn({
            let caller = Arc::clone(&caller);
            async move { caller.run_connection(&mut transport).await }
        });

        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 41,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_grep",
                        "arguments": {
                            "pattern": "route_fixture",
                            "fixed_strings": true,
                            "project_selector": {"project_id": target_project_id},
                            "format": "json"
                        }
                    }
                })
                .to_string(),
            )
            .expect("send selected request");
        route_entered.notified().await;
        sender
            .send(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": 41, "reason": "route still resolving"}
                })
                .to_string(),
            )
            .expect("cancel selected request during route resolution");
        wait_for_transport_reads(&transport_reads, 2).await;

        // The connection's original server may retire while the route is
        // unresolved. It must not reject or authorize the selected target.
        caller.project_server_response_lifecycle().revoke();
        release_route.notify_one();

        let response_line =
            tokio::time::timeout(std::time::Duration::from_secs(5), responses.recv())
                .await
                .expect("selected cancellation response timeout")
                .expect("selected cancellation response");
        let response: Value =
            serde_json::from_str(response_line.trim()).expect("selected cancellation JSON");
        assert_eq!(response["id"], serde_json::json!(41));
        assert_eq!(
            response["error"]["data"]["reason_code"],
            serde_json::json!("tool_dispatch_cancelled"),
            "cancellation captured before registration must reach selected target: {response}"
        );
        assert_eq!(
            caller.stats.total_requests.load(Ordering::Relaxed),
            0,
            "caller must not account a request owned by the selected target"
        );
        assert_eq!(
            target.stats.total_requests.load(Ordering::Relaxed),
            1,
            "selected target must own request/error accounting"
        );
        assert_eq!(target.stats.errors.load(Ordering::Relaxed), 1);

        drop(sender);
        tokio::time::timeout(std::time::Duration::from_secs(5), serving)
            .await
            .expect("selected cancellation connection close timeout")
            .expect("join selected cancellation connection")
            .expect("serve selected cancellation connection");
        harness.shutdown().await;
    }
}
