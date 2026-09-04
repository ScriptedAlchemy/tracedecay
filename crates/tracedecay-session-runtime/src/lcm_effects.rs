use std::time::Duration;

use tracedecay_application::{CancellationSignal, Deadline};
use tracedecay_temporal_query::ports::ExecutionControl;

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::{LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmSummarizerMode};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_lcm::{LcmSessionBoundaryRequest, LcmSessionBoundaryResponse};

pub(super) const LCM_EFFECT_CEILING: Duration =
    tracedecay_daemon_protocol::DEFAULT_DAEMON_OPERATION_DEADLINE;
const LCM_EFFECT_WORK_LIMIT: usize = 4_096;

/// Daemon-owned execution boundary for retained LCM mutations.
///
/// The database authority remains lower-level storage. Host and MCP adapters
/// call this service so a disconnect or deadline can still roll back the open
/// transaction before its commit checkpoint.
#[derive(Clone)]
pub(super) struct DaemonLcmEffectService {
    db: RegisteredGlobalDbLeaseV1,
    control: LcmEffectControl,
}

#[derive(Clone)]
struct LcmEffectControl {
    cancellation: Option<CancellationSignal>,
    expires_at: tokio::time::Instant,
}

impl LcmEffectControl {
    fn new(deadline: Option<&Deadline>, cancellation: Option<&CancellationSignal>) -> Self {
        let budget = deadline
            .and_then(tracedecay_daemon_protocol::deadline_remaining)
            .map_or(LCM_EFFECT_CEILING, |remaining| {
                remaining.min(LCM_EFFECT_CEILING)
            });
        Self {
            cancellation: cancellation.cloned(),
            expires_at: tokio::time::Instant::now() + budget,
        }
    }

    fn checkpoint(&self) -> Result<(), LcmError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationSignal::is_cancelled)
        {
            return Err(LcmError::Cancelled);
        }
        if tokio::time::Instant::now() >= self.expires_at {
            return Err(LcmError::DeadlineExceeded);
        }
        Ok(())
    }

    #[hotpath::skip]
    async fn execute<T>(
        &self,
        execution: &ExecutionControl,
        mutation: impl std::future::Future<Output = Result<T, LcmError>>,
    ) -> Result<T, LcmError> {
        self.checkpoint()?;
        let result = mutation.await;
        if result.is_err() {
            execution.cancel();
        }
        result
    }

    fn execution_control(&self) -> ExecutionControl {
        let control = ExecutionControl::new(Some(self.expires_at.into_std()))
            .with_work_limit(LCM_EFFECT_WORK_LIMIT);
        if self.checkpoint().is_err() {
            control.cancel();
        }
        control
    }

    fn remaining(&self) -> Result<Duration, LcmError> {
        self.checkpoint()?;
        Ok(self
            .expires_at
            .saturating_duration_since(tokio::time::Instant::now()))
    }
}

impl DaemonLcmEffectService {
    pub(super) fn new(
        db: RegisteredGlobalDbLeaseV1,
        deadline: Option<&Deadline>,
        cancellation: Option<&CancellationSignal>,
    ) -> Self {
        Self {
            db,
            control: LcmEffectControl::new(deadline, cancellation),
        }
    }

    #[hotpath::skip]
    pub(super) async fn compress(
        &self,
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let result = self.compress_phases(request).await;
        observe_compression_outcome(&result);
        result
    }

    #[hotpath::skip]
    pub(super) async fn compress_retained_page(
        &self,
        request: LcmCompressionRequest,
        convergence_candidate: &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        let result = self
            .compress_retained_phases(request, convergence_candidate)
            .await;
        observe_compression_outcome(
            &result
                .as_ref()
                .map(|bounded| bounded.response.clone())
                .map_err(|error| (*error).clone()),
        );
        result
    }

    #[hotpath::skip]
    async fn compress_phases(
        &self,
        mut request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        // Observation-projected sessions land raw rows without ingest
        // protection; the verified raw loads inside compression reject them.
        // Hydrate the canonical sanitization receipts first so the daemon
        // journey consumes the same protected shape as transcript ingest.
        let execution = self.control.execution_control();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db
                        .lcm_protect_session_raw_messages(&request.provider, &request.session_id),
                    label = "daemon.lcm.hydrate"
                ),
            )
            .await?;
        if matches!(
            &request.summarizer,
            LcmSummarizerMode::Provided { summary_text, .. } if !summary_text.trim().is_empty()
        ) || matches!(&request.summarizer, LcmSummarizerMode::Fake { .. })
        {
            return self.commit_compression(request).await;
        }

        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let pending = self.commit_compression(request.clone()).await?;
        if pending.status != "needs_summary" {
            return Ok(pending);
        }
        let Some(summary_request) = pending.summary_request.clone() else {
            return Ok(pending);
        };
        let summary = match super::lcm_summarization::resolve_authoritative_summary(
            &self.db,
            &request.provider,
            &request.session_id,
            summary_request,
            self.control.remaining()?,
            None,
        )
        .await
        {
            Ok(summary) => summary,
            Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                return Err(error);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                self.control.checkpoint()?;
                return Ok(summary_unavailable(pending, reason));
            }
        };
        self.control.checkpoint()?;
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary.text,
            route: Some(summary.route),
        };
        self.commit_compression(request).await
    }

    async fn compress_retained_phases(
        &self,
        mut request: LcmCompressionRequest,
        convergence_candidate: &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        if matches!(
            &request.summarizer,
            LcmSummarizerMode::Provided { summary_text, .. } if !summary_text.trim().is_empty()
        ) || matches!(&request.summarizer, LcmSummarizerMode::Fake { .. })
        {
            return self
                .commit_retained_compression(request, Some(convergence_candidate), None)
                .await;
        }

        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        let pending = self
            .commit_retained_compression(request.clone(), Some(convergence_candidate), None)
            .await?;
        if pending.response.status != "needs_summary" {
            return Ok(pending);
        }
        let Some(summary_request) = pending.response.summary_request.clone() else {
            return Ok(pending);
        };
        // Host-native compaction text is usable for retained convergence only
        // when its evidence binds the exact raw source range selected by this
        // page. Otherwise the provider authority summarizes source_messages.
        let required_native_source_range = summary_request.source_range.clone();
        let summary = match super::lcm_summarization::resolve_authoritative_summary(
            &self.db,
            &request.provider,
            &request.session_id,
            summary_request,
            self.control.remaining()?,
            Some(&required_native_source_range),
        )
        .await
        {
            Ok(summary) if summary.source_range.as_ref() == Some(&required_native_source_range) => {
                summary
            }
            Ok(_) => {
                self.control.checkpoint()?;
                let mut pending = pending;
                pending.response =
                    summary_unavailable(pending.response, "authoritative_summary_source_mismatch");
                return Ok(pending);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Storage(error)) => {
                return Err(error);
            }
            Err(super::lcm_summarization::SummaryResolutionError::Unavailable(reason)) => {
                self.control.checkpoint()?;
                let mut pending = pending;
                pending.response = summary_unavailable(pending.response, reason);
                return Ok(pending);
            }
        };
        self.control.checkpoint()?;
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary.text,
            route: Some(summary.route),
        };
        let mut committed = self
            .commit_retained_compression(
                request,
                Some(convergence_candidate),
                Some(&required_native_source_range),
            )
            .await?;
        committed.rows_scanned = committed.rows_scanned.saturating_add(pending.rows_scanned);
        committed.bytes_scanned = committed
            .bytes_scanned
            .saturating_add(pending.bytes_scanned);
        committed.has_more |= pending.has_more;
        Ok(committed)
    }

    #[hotpath::skip]
    async fn commit_compression(
        &self,
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_compress_guarded(request, &execution, move || {
                        before_commit.checkpoint()
                    }),
                    label = "daemon.lcm.commit"
                ),
            )
            .await
    }

    async fn commit_retained_compression(
        &self,
        request: LcmCompressionRequest,
        convergence_candidate: Option<
            &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
        >,
        expected_summary_source_range: Option<&tracedecay_lcm::LcmSummarySourceRange>,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
        // A retained pass can perform one planning scan and one commit scan.
        // Split the existing page budgets between those phases so the whole
        // service call, rather than each internal phase, remains bounded.
        const RETAINED_PHASE_ROWS: usize = tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize / 2;
        const RETAINED_PHASE_BYTES: u64 = tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64 / 2;
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_compress_retained_page_guarded(
                        request,
                        &execution,
                        move || before_commit.checkpoint(),
                        tracedecay_lcm::compression::RetainedCompressionGuard {
                            row_limit: RETAINED_PHASE_ROWS,
                            byte_limit: RETAINED_PHASE_BYTES,
                            expected_summary_source_range: expected_summary_source_range.cloned(),
                        },
                        convergence_candidate,
                    ),
                    label = "daemon.lcm.retained_commit"
                ),
            )
            .await
    }

    pub(super) async fn recover_retained_relation_projection_page(
        &self,
    ) -> Result<tracedecay_session_temporal_store::SessionRelationRecoveryPage, LcmError> {
        const RELATION_PAGE_LIMIT: usize = 1;
        let execution = self.control.execution_control();
        let recovered = self
            .control
            .execute(&execution, async {
                self.db
                    .recover_pending_session_relation_projection_page(
                        RELATION_PAGE_LIMIT,
                        tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                            &execution,
                        ),
                    )
                    .await
                    .map_err(|error| match error {
                        tracedecay_store::SessionStoreError::Cancelled => LcmError::Cancelled,
                        tracedecay_store::SessionStoreError::DeadlineExceeded => {
                            LcmError::DeadlineExceeded
                        }
                        error => LcmError::Db(format!(
                            "recover bounded retained LCM relation projection: {error}"
                        )),
                    })
            })
            .await?;
        self.control.checkpoint()?;
        Ok(recovered)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub(super) async fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                hotpath::future!(
                    self.db.lcm_session_boundary_guarded(request, move || {
                        before_commit.checkpoint()
                    }),
                    label = "daemon.lcm.boundary"
                ),
            )
            .await
    }
}

#[cfg(feature = "test-helpers")]
#[doc(hidden)]
pub async fn lcm_compress_for_test(
    db: RegisteredGlobalDbLeaseV1,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    DaemonLcmEffectService::new(db, None, None)
        .compress(request)
        .await
}

#[cfg(feature = "test-helpers")]
#[doc(hidden)]
pub async fn lcm_session_boundary_for_test(
    db: RegisteredGlobalDbLeaseV1,
    request: LcmSessionBoundaryRequest,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    DaemonLcmEffectService::new(db, None, None)
        .session_boundary(request)
        .await
}

/// Terminal compression outcomes for profiling, including deferrals and
/// failures: a lane that only counts commits hides exactly the retried and
/// cancelled work a compaction investigation needs to see.
fn observe_compression_outcome(result: &Result<LcmCompressionResponse, LcmError>) {
    match result {
        Ok(response) if response.retry_status.is_some() => {
            hotpath::gauge!("daemon.lcm.compress.deferred").inc(1.0);
        }
        Ok(response) if response.status == "needs_summary" => {
            hotpath::gauge!("daemon.lcm.compress.needs_summary").inc(1.0);
        }
        Ok(response) if response.summary_nodes_created > 0 => {
            hotpath::gauge!("daemon.lcm.compress.committed").inc(1.0);
        }
        Ok(_) => {
            hotpath::gauge!("daemon.lcm.compress.noop").inc(1.0);
        }
        Err(LcmError::Cancelled) => {
            hotpath::gauge!("daemon.lcm.compress.cancelled").inc(1.0);
        }
        Err(LcmError::DeadlineExceeded) => {
            hotpath::gauge!("daemon.lcm.compress.deadline").inc(1.0);
        }
        Err(_) => {
            hotpath::gauge!("daemon.lcm.compress.failed").inc(1.0);
        }
    }
}

fn summary_unavailable(
    mut response: LcmCompressionResponse,
    reason: &'static str,
) -> LcmCompressionResponse {
    response.reason = reason.to_string();
    response.retry_status = Some("needs_authoritative_summary".to_string());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tracedecay_domain::SessionId;
    use tracedecay_global_db::RegisteredGlobalDb;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_lcm::{LcmRelationProjectionStatus, LcmSourceRef, LcmSummarizerMode};
    use tracedecay_runtime_core::db::engine::params;
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
    use tracedecay_store::ParseOffset;

    fn session(provider: &str, session_id: &str) -> SessionRecord {
        SessionRecord {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            project_key: "project.lcm-effects".to_string(),
            project_path: "/tmp/lcm-effects".to_string(),
            title: Some("LCM effects journey".to_string()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    fn message(session_id: &str, ordinal: i64) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: format!("message-{ordinal}"),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            timestamp: Some(ordinal),
            ordinal,
            text: format!("canonical historical message {ordinal} with durable context"),
            kind: Some("message".to_string()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }
    }

    fn compression_request(session_id: &str) -> LcmCompressionRequest {
        LcmCompressionRequest {
            provider: "cursor".to_string(),
            session_id: session_id.to_string(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: Some(1),
            max_source_messages: Some(8),
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: Some(1),
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "fixture summary preserving canonical historical context".to_string(),
            },
        }
    }

    fn daemon_summary_request(provider: &str, session_id: &str) -> LcmCompressionRequest {
        let mut request = compression_request(session_id);
        request.provider = provider.to_string();
        request.summarizer = LcmSummarizerMode::HermesAuxiliary;
        request
    }

    fn execution_control() -> ExecutionControl {
        ExecutionControl::new(Some(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ))
        .with_work_limit(4_096)
    }

    fn retained_guard(
        expected_summary_source_range: Option<tracedecay_lcm::LcmSummarySourceRange>,
    ) -> tracedecay_lcm::compression::RetainedCompressionGuard {
        tracedecay_lcm::compression::RetainedCompressionGuard {
            row_limit: tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize / 2,
            byte_limit: tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64 / 2,
            expected_summary_source_range,
        }
    }

    /// Runs a future under the canonical user-data-dir env lock so provider
    /// binary env overrides cannot race parallel tests.
    fn run_with_test_env_lock<T>(future: impl std::future::Future<Output = T>) -> T {
        let _lock = tracedecay_runtime_core::config::lock_user_data_dir_test_env();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build lcm effects test runtime")
            .block_on(future)
    }

    #[tokio::test]
    async fn cancellation_observed_after_a_committed_effect_does_not_replace_its_result() {
        let cancellation = CancellationSignal::active("cancellation.lcm-settlement").unwrap();
        let control = LcmEffectControl::new(None, Some(&cancellation));
        let execution = control.execution_control();
        let result = control
            .execute(&execution, async {
                assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
                Ok::<_, LcmError>("committed")
            })
            .await;

        assert_eq!(result.unwrap(), "committed");
    }

    #[tokio::test]
    async fn retained_relation_recovery_preserves_typed_cancellation() {
        let harness = RegisteredGlobalDbHarness::open("lcm-relation-recovery-cancelled").await;
        let cancellation =
            CancellationSignal::active("cancellation.lcm-relation-recovery").unwrap();
        assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
        let control = LcmEffectControl::new(None, Some(&cancellation));
        let execution = control.execution_control();

        let error = harness
            .registered
            .recover_pending_session_relation_projection_page(
                1,
                tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                    &execution,
                ),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            tracedecay_store::SessionStoreError::Cancelled
        ));
    }

    #[tokio::test]
    async fn compression_producer_apply_read_and_rollback_stay_one_authority() {
        let harness = RegisteredGlobalDbHarness::open("lcm-compress-effect-journey").await;
        let db = harness.registered.clone();
        assert!(
            db.upsert_session(&session("cursor", "compress-session"))
                .await
        );
        let storage_root = db.db_path().parent().unwrap();
        for ordinal in 1..=8 {
            db.lcm_ingest_raw_message(storage_root, &message("compress-session", ordinal))
                .await
                .unwrap();
        }

        let cancellation = CancellationSignal::active("cancellation.lcm-compress-journey").unwrap();
        assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
        let service_cancelled = DaemonLcmEffectService::new(db.clone(), None, Some(&cancellation))
            .compress(compression_request("compress-session"))
            .await;
        assert_eq!(service_cancelled.unwrap_err(), LcmError::Cancelled);

        let cancellation_control = execution_control();
        let cancelled = db
            .lcm_compress_guarded(
                compression_request("compress-session"),
                &cancellation_control,
                || Err(LcmError::Cancelled),
            )
            .await;
        assert_eq!(cancelled.unwrap_err(), LcmError::Cancelled);
        let rolled_back = db
            .lcm_status("cursor", Some("compress-session"))
            .await
            .unwrap();
        assert_eq!(rolled_back.raw_message_count, 8);
        assert_eq!(rolled_back.summary_node_count, 0);

        let response = DaemonLcmEffectService::new(db.clone(), None, None)
            .compress(compression_request("compress-session"))
            .await
            .unwrap();
        let summary = response.summary_nodes.first().unwrap();
        let source_store_id = summary
            .source_refs
            .iter()
            .find_map(|source| match source {
                LcmSourceRef::RawMessage { store_id } => Some(*store_id),
                LcmSourceRef::SummaryNode { .. } => None,
            })
            .unwrap();
        let store_id = db
            .lcm_raw_message_store_id("cursor", "message-1")
            .await
            .unwrap()
            .expect("durable raw message");
        assert_eq!(store_id, source_store_id);
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT content FROM lcm_raw_messages WHERE store_id = ?1",
                params![store_id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("durable raw message row");
        let content: String = row.get(0).unwrap();
        assert_eq!(
            content,
            "canonical historical message 1 with durable context"
        );
        assert!(!summary.summary_text.is_empty());
        assert_eq!(
            response.relation_projection_status,
            LcmRelationProjectionStatus::Applied
        );

        let session_id = SessionId::new("compress-session").unwrap();
        let relation_ids = [summary.node_id.clone()];
        let read_control = execution_control();
        let (_, relations) = db
            .active_session_summary_relations(
                &session_id,
                &relation_ids,
                4_096,
                tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                    &read_control,
                ),
            )
            .await
            .unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].sources.len(), summary.source_refs.len());
        assert_eq!(
            db.recover_pending_session_relation_projections(
                1,
                tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                    &read_control,
                ),
            )
            .await
            .unwrap(),
            0,
            "compress applies the graph projection in the same journey"
        );

        let expected_relations = relations.clone();
        drop(response);
        drop(db);
        let harness = harness.restart().await;
        let restarted = harness.registered.clone();
        let restart_control = execution_control();
        let (_, restarted_relations) = restarted
            .active_session_summary_relations(
                &session_id,
                &relation_ids,
                4_096,
                tracedecay_session_temporal_store::store::execution_control_graph_cancellation(
                    &restart_control,
                ),
            )
            .await
            .unwrap();
        assert_eq!(restarted_relations, expected_relations);
    }

    #[tokio::test]
    async fn preflight_reads_canonical_state_without_creating_or_ingesting_a_session() {
        let harness = RegisteredGlobalDbHarness::open("lcm-preflight-read-only").await;
        let db = harness.registered.clone();
        let response = db
            .lcm_preflight(tracedecay_lcm::LcmPreflightRequest {
                provider: "cursor".to_string(),
                session_id: "missing-session".to_string(),
                messages: Vec::new(),
                current_tokens: Some(1_000),
                threshold_tokens: None,
                max_assembly_tokens: None,
                leaf_chunk_tokens: None,
                max_source_messages: None,
                summary_fan_in: None,
                incremental_max_depth: None,
                fresh_tail_count: None,
                dynamic_leaf_chunk_enabled: None,
                dynamic_leaf_chunk_max: None,
                context_length: None,
                reserve_tokens_floor: None,
                ignore_session_patterns: Vec::new(),
                stateless_session_patterns: Vec::new(),
            })
            .await
            .unwrap();
        assert!(response.replay_messages.is_empty());

        let snapshot = db.read_snapshot().await.unwrap();
        for table in ["sessions", "session_messages", "lcm_raw_messages"] {
            let mut rows = snapshot
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                0
            );
        }
    }

    #[tokio::test]
    async fn native_summary_evidence_requires_exact_cursor_text_and_claude_pair_identity() {
        let harness = RegisteredGlobalDbHarness::open("lcm-native-summary-evidence").await;
        let db = harness.registered.clone();
        for (provider, session_id) in [
            ("cursor", "cursor-native-session"),
            ("claude", "claude-native-session"),
            ("codex", "codex-native-session"),
        ] {
            assert!(db.upsert_session(&session(provider, session_id)).await);
        }
        let cursor_text = "exact Cursor Composer compacted text";
        let cursor_metadata = canonical_envelope(
            "cursor",
            "cursor-native-session",
            "cursor-summary",
            None,
            vec![
                serde_json::json!({
                    "kind": "message",
                    "role": "assistant",
                    "content": cursor_text
                }),
                serde_json::json!({
                    "kind": "compaction",
                    "summary": cursor_text
                }),
            ],
        );
        insert_summary_evidence(
            (&db, "cursor"),
            "cursor-native-session",
            "cursor-summary",
            10,
            cursor_text,
            "message",
            &cursor_metadata,
        )
        .await;
        insert_summary_evidence(
            (&db, "codex"),
            "codex-native-session",
            "codex-encrypted-summary",
            10,
            "Codex context compaction; encrypted body unavailable",
            "summary",
            &serde_json::json!({
                "source": "codex_context_compacted",
                "summary_body": "encrypted"
            }),
        )
        .await;
        insert_summary_evidence(
            (&db, "codex"),
            "codex-native-session",
            "codex-unrelated-plaintext",
            12,
            "unrelated plaintext summary",
            "summary",
            &serde_json::json!({
                "source": "unrelated_source",
                "summary_body": "plaintext"
            }),
        )
        .await;

        let claude_text = "exact Claude compact summary wrapper and body";
        let claude_summary_metadata = canonical_envelope(
            "claude",
            "claude-native-session",
            "claude-summary",
            Some("claude-boundary"),
            vec![
                serde_json::json!({
                    "kind": "message",
                    "role": "user",
                    "content": claude_text
                }),
                serde_json::json!({
                    "kind": "compaction",
                    "summary": {
                        "isCompactSummary": true,
                        "isVisibleInTranscriptOnly": true
                    }
                }),
            ],
        );
        insert_summary_evidence(
            (&db, "claude"),
            "claude-native-session",
            "claude-summary",
            11,
            claude_text,
            "message",
            &claude_summary_metadata,
        )
        .await;

        let cursor = super::super::lcm_summarization::native_summary_evidence(
            &db,
            "cursor",
            "cursor-native-session",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(cursor.text, cursor_text);
        assert_eq!(cursor.route, "cursor_native_compaction");
        assert!(
            super::super::lcm_summarization::native_summary_evidence(
                &db,
                "claude",
                "claude-native-session",
                None,
            )
            .await
            .unwrap()
            .is_none(),
            "an unpaired Claude summary must remain non-authoritative"
        );
        assert!(
            super::super::lcm_summarization::native_summary_evidence(
                &db,
                "codex",
                "codex-native-session",
                None,
            )
            .await
            .unwrap()
            .is_none(),
            "encrypted Codex compaction must remain non-authoritative"
        );
        insert_summary_evidence(
            (&db, "codex"),
            "codex-native-session",
            "codex-plaintext-summary",
            11,
            "exact Codex plaintext summary",
            "summary",
            &serde_json::json!({
                "source": "codex_context_compacted",
                "summary_body": "plaintext"
            }),
        )
        .await;
        let codex = super::super::lcm_summarization::native_summary_evidence(
            &db,
            "codex",
            "codex-native-session",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(codex.text, "exact Codex plaintext summary");
        assert_eq!(codex.route, "codex_native_compaction");

        let boundary_metadata = canonical_envelope(
            "claude",
            "claude-native-session",
            "claude-boundary",
            None,
            vec![
                serde_json::json!({
                    "kind": "boundary",
                    "boundary_kind": "compaction_boundary"
                }),
                serde_json::json!({
                    "kind": "compaction",
                    "summary": {
                        "preservedSegment": {
                            "anchorUuid": "claude-summary"
                        }
                    }
                }),
            ],
        );
        insert_summary_evidence(
            (&db, "claude"),
            "claude-native-session",
            "claude-boundary",
            10,
            "Claude compaction boundary",
            "compaction",
            &boundary_metadata,
        )
        .await;
        let claude = super::super::lcm_summarization::native_summary_evidence(
            &db,
            "claude",
            "claude-native-session",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claude.text, claude_text);
        assert_eq!(claude.route, "claude_native_compaction");
    }

    #[tokio::test]
    async fn transcript_ingest_persists_native_compaction_raw_range() {
        let harness = RegisteredGlobalDbHarness::open("lcm-native-summary-range").await;
        let db = harness.registered.clone();
        let session_id = "codex-native-range-session";
        let mut messages = Vec::new();
        for ordinal in 1..=4 {
            let mut record = message(session_id, ordinal);
            record.provider = "codex".to_string();
            record.message_id = format!("native-range-message-{ordinal}");
            messages.push(record);
        }
        let mut compaction = message(session_id, 5);
        compaction.provider = "codex".to_string();
        compaction.message_id = "native-range-compaction".to_string();
        compaction.kind = Some("summary".to_string());
        compaction.text = "production Codex compaction text".to_string();
        compaction.metadata_json = Some(
            serde_json::json!({
                "source": "codex_context_compacted",
                "source_event": "compacted",
                "summary_body": "plaintext",
                "replacement_history_count": 1,
                "codex_compaction_depth": 1,
                "source_offset": 5,
                "encrypted": false
            })
            .to_string(),
        );
        messages.push(compaction);
        let mut post_compaction = message(session_id, 6);
        post_compaction.provider = "codex".to_string();
        post_compaction.message_id = "native-range-post-compaction".to_string();
        messages.push(post_compaction);

        assert!(
            db.upsert_transcript_batch(
                &session("codex", session_id),
                &messages,
                "/tmp/codex-native-range-session.jsonl",
                ParseOffset::default(),
            )
            .await
        );
        let first = db
            .lcm_raw_message_store_id("codex", "native-range-message-1")
            .await
            .unwrap()
            .unwrap();
        let last = db
            .lcm_raw_message_store_id("codex", "native-range-message-4")
            .await
            .unwrap()
            .unwrap();
        let evidence = super::super::lcm_summarization::native_summary_evidence(
            &db, "codex", session_id, None,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(evidence.text, "production Codex compaction text");
        assert_eq!(
            evidence.source_range,
            Some(tracedecay_lcm::LcmSummarySourceRange {
                from_store_id: first,
                to_store_id: last,
            })
        );
        let converged =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(converged.sessions[0].summary_nodes_created, 1);
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT summary_text, json_extract(metadata_json, '$.summary_route')
                 FROM lcm_summary_nodes
                 WHERE provider = 'codex' AND session_id = ?1",
                params![session_id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(
            row.get::<String>(0).unwrap(),
            "production Codex compaction text"
        );
        assert_eq!(
            row.get::<Option<String>>(1).unwrap().as_deref(),
            Some("codex_native_compaction")
        );
    }

    #[tokio::test]
    async fn successive_claude_compactions_bind_to_the_previous_native_boundary_after_restart() {
        let harness = RegisteredGlobalDbHarness::open("lcm-claude-successive-native-ranges").await;
        let db = harness.registered.clone();
        let session_id = "claude-successive-native-ranges";
        let mut messages = Vec::new();
        for ordinal in 1..=3 {
            let mut record = message(session_id, ordinal);
            record.provider = "claude".to_string();
            record.message_id = format!("claude-before-first-{ordinal}");
            messages.push(record);
        }
        let first_summary_id = "claude-first-summary";
        let first_boundary_id = "claude-first-boundary";
        let mut first_boundary = message(session_id, 4);
        first_boundary.provider = "claude".to_string();
        first_boundary.message_id = first_boundary_id.to_string();
        first_boundary.kind = Some("compaction".to_string());
        first_boundary.metadata_json = Some(
            canonical_envelope(
                "claude",
                session_id,
                first_boundary_id,
                None,
                vec![
                    serde_json::json!({
                        "kind": "boundary",
                        "boundary_kind": "compaction_boundary"
                    }),
                    serde_json::json!({
                        "kind": "compaction",
                        "summary": {"preservedSegment": {"anchorUuid": first_summary_id}}
                    }),
                ],
            )
            .to_string(),
        );
        messages.push(first_boundary);
        let mut first_summary = message(session_id, 5);
        first_summary.provider = "claude".to_string();
        first_summary.message_id = first_summary_id.to_string();
        first_summary.text = "first authoritative Claude compaction".to_string();
        first_summary.metadata_json = Some(
            canonical_envelope(
                "claude",
                session_id,
                first_summary_id,
                Some(first_boundary_id),
                vec![serde_json::json!({
                    "kind": "compaction",
                    "summary": {
                        "isCompactSummary": true,
                        "isVisibleInTranscriptOnly": true
                    }
                })],
            )
            .to_string(),
        );
        messages.push(first_summary);
        for ordinal in 6..=520 {
            let mut record = message(session_id, ordinal);
            record.provider = "claude".to_string();
            record.message_id = format!("claude-between-{ordinal}");
            messages.push(record);
        }
        let second_summary_id = "claude-second-summary";
        let second_boundary_id = "claude-second-boundary";
        let mut second_boundary = message(session_id, 521);
        second_boundary.provider = "claude".to_string();
        second_boundary.message_id = second_boundary_id.to_string();
        second_boundary.kind = Some("compaction".to_string());
        second_boundary.metadata_json = Some(
            canonical_envelope(
                "claude",
                session_id,
                second_boundary_id,
                None,
                vec![
                    serde_json::json!({
                        "kind": "boundary",
                        "boundary_kind": "compaction_boundary"
                    }),
                    serde_json::json!({
                        "kind": "compaction",
                        "summary": {"preservedSegment": {"anchorUuid": second_summary_id}}
                    }),
                ],
            )
            .to_string(),
        );
        messages.push(second_boundary);
        let mut second_summary = message(session_id, 522);
        second_summary.provider = "claude".to_string();
        second_summary.message_id = second_summary_id.to_string();
        second_summary.text = "second authoritative Claude compaction".to_string();
        second_summary.metadata_json = Some(
            canonical_envelope(
                "claude",
                session_id,
                second_summary_id,
                Some(second_boundary_id),
                vec![serde_json::json!({
                    "kind": "compaction",
                    "summary": {
                        "isCompactSummary": true,
                        "isVisibleInTranscriptOnly": true
                    }
                })],
            )
            .to_string(),
        );
        messages.push(second_summary);
        assert!(
            db.upsert_transcript_batch(
                &session("claude", session_id),
                &messages,
                "/tmp/claude-successive-native-ranges.jsonl",
                ParseOffset::default(),
            )
            .await
        );

        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT store_id, message_id, role
                 FROM lcm_raw_messages
                 WHERE provider = 'claude' AND session_id = ?1
                 ORDER BY store_id",
                params![session_id],
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            raw.push((
                row.get::<i64>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<String>(2).unwrap(),
            ));
        }
        drop(rows);
        drop(snapshot);
        let first_summary_store_id = raw
            .iter()
            .find(|(_, message_id, _)| message_id == first_summary_id)
            .unwrap()
            .0;
        let second_summary_store_id = raw
            .iter()
            .find(|(_, message_id, _)| message_id == second_summary_id)
            .unwrap()
            .0;
        let first_sources = raw
            .iter()
            .filter(|(store_id, _, _)| *store_id < first_summary_store_id)
            .map(
                |(store_id, _, role)| tracedecay_lcm::LcmSummarySourceMessage {
                    store_id: *store_id,
                    role: role.clone(),
                    content: String::new(),
                },
            )
            .collect::<Vec<_>>();
        let first_request = tracedecay_lcm::LcmSummaryRequest {
            provider: "claude".to_string(),
            session_id: session_id.to_string(),
            focus_topic: None,
            prompt: String::new(),
            source_range: tracedecay_lcm::LcmSummarySourceRange {
                from_store_id: first_sources.first().unwrap().store_id,
                to_store_id: first_sources.last().unwrap().store_id,
            },
            source_messages: first_sources,
            extraction_request: None,
        };
        let native_without_range = super::super::lcm_summarization::native_summary_evidence(
            &db, "claude", session_id, None,
        )
        .await
        .unwrap();
        assert!(
            native_without_range.is_some(),
            "production transcript rows must retain recognizable Claude compaction pairs"
        );
        let first = super::super::lcm_summarization::native_summary_evidence(
            &db,
            "claude",
            session_id,
            Some(&first_request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(first.text, "first authoritative Claude compaction");

        drop(db);
        let harness = harness.restart().await;
        let restarted = harness.registered.clone();
        let second_sources = raw
            .iter()
            .filter(|(store_id, _, _)| {
                *store_id >= first_summary_store_id && *store_id < second_summary_store_id
            })
            .map(
                |(store_id, _, role)| tracedecay_lcm::LcmSummarySourceMessage {
                    store_id: *store_id,
                    role: role.clone(),
                    content: String::new(),
                },
            )
            .collect::<Vec<_>>();
        let second_request = tracedecay_lcm::LcmSummaryRequest {
            provider: "claude".to_string(),
            session_id: session_id.to_string(),
            focus_topic: None,
            prompt: String::new(),
            source_range: tracedecay_lcm::LcmSummarySourceRange {
                from_store_id: second_sources.first().unwrap().store_id,
                to_store_id: second_sources.last().unwrap().store_id,
            },
            source_messages: second_sources,
            extraction_request: None,
        };
        let second = super::super::lcm_summarization::native_summary_evidence(
            &restarted,
            "claude",
            session_id,
            Some(&second_request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(second.text, "second authoritative Claude compaction");
        assert_eq!(
            second.source_range.as_ref().unwrap().from_store_id,
            first_summary_store_id
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_compaction_requires_exact_selected_raw_membership() {
        run_with_test_env_lock(async {
            let harness = RegisteredGlobalDbHarness::open("lcm-native-membership").await;
            let db = harness.registered.clone();
            let session_id = "codex-native-membership-session";
            let mut messages = Vec::new();
            for ordinal in 1..=4 {
                let mut record = message(session_id, ordinal);
                record.provider = "codex".to_string();
                record.message_id = format!("native-membership-message-{ordinal}");
                if ordinal == 2 {
                    record.role = "system".to_string();
                }
                messages.push(record);
            }
            let mut compaction = message(session_id, 5);
            compaction.provider = "codex".to_string();
            compaction.message_id = "native-membership-compaction".to_string();
            compaction.kind = Some("summary".to_string());
            compaction.text = "native text covering the full predecessor interval".to_string();
            compaction.metadata_json = Some(
                serde_json::json!({
                    "source": "codex_context_compacted",
                    "summary_body": "plaintext"
                })
                .to_string(),
            );
            messages.push(compaction);
            let mut tail = message(session_id, 6);
            tail.provider = "codex".to_string();
            tail.message_id = "native-membership-tail".to_string();
            messages.push(tail);
            assert!(
                db.upsert_transcript_batch(
                    &session("codex", session_id),
                    &messages,
                    "/tmp/codex-native-membership-session.jsonl",
                    ParseOffset::default(),
                )
                .await
            );
            let policy_anchor_store_id = db
                .lcm_raw_message_store_id("codex", "native-membership-message-2")
                .await
                .unwrap()
                .unwrap();

            let temporary = tempfile::tempdir().unwrap();
            let codex_bin = temporary.path().join("codex");
            std::fs::write(
                &codex_bin,
                r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
    *'"id":1'*) printf '%s\n' '{"id":1,"result":{"thread":{"id":"thread-1","model":"codex-membership-model"}}}' ;;
    *'"id":2'*)
      printf '%s\n' '{"method":"item/completed","params":{"model":"codex-membership-model","item":{"content":[{"type":"output_text","text":"auxiliary membership-bound summary"}]}}}'
      printf '%s\n' '{"method":"turn/completed"}'
      ;;
  esac
done
"#,
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let codex_bin_env = codex_bin.to_string_lossy().into_owned();
            let _env = TestEnvironment::set([
                ("TRACEDECAY_CODEX_BIN", codex_bin_env.as_str()),
                ("TRACEDECAY_CODEX_SUMMARY_TIMEOUT_SECS", "5"),
            ]);

            let converged =
                super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                    .await
                    .unwrap();
            assert_eq!(converged.sessions[0].summary_nodes_created, 1);
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT node_id, summary_text
                     FROM lcm_summary_nodes
                     WHERE provider = 'codex' AND session_id = ?1",
                    params![session_id],
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            let node_id = row.get::<String>(0).unwrap();
            assert_eq!(
                row.get::<String>(1).unwrap(),
                "auxiliary membership-bound summary"
            );
            drop(rows);
            let mut sources = snapshot
                .query(
                    "SELECT source_id FROM lcm_summary_sources
                     WHERE node_id = ?1 AND source_kind = 'raw_message'
                     ORDER BY ordinal",
                    params![node_id],
                )
                .await
                .unwrap();
            let mut source_ids = Vec::new();
            while let Some(row) = sources.next().await.unwrap() {
                source_ids.push(row.get::<String>(0).unwrap());
            }
            assert_eq!(source_ids.len(), 3);
            assert!(!source_ids.contains(&policy_anchor_store_id.to_string()));
        });
    }

    #[tokio::test]
    async fn providers_without_authoritative_summarizers_keep_frontiers_pending() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-unavailable").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        for provider in [
            "claude", "hermes", "kiro", "kimi", "opencode", "cline", "roo", "kilo",
        ] {
            let session_id = format!("{provider}-session");
            assert!(db.upsert_session(&session(provider, &session_id)).await);
            for ordinal in 1..=8 {
                let mut record = message(&session_id, ordinal);
                record.provider = provider.to_string();
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }

            let response = DaemonLcmEffectService::new(db.clone(), None, None)
                .compress(daemon_summary_request(provider, &session_id))
                .await
                .unwrap();
            assert_eq!(response.status, "needs_summary", "{provider}");
            assert_eq!(
                response.reason, "authoritative_summarizer_unavailable",
                "{provider}"
            );
            assert_eq!(
                response.retry_status.as_deref(),
                Some("needs_authoritative_summary"),
                "{provider}"
            );
            assert!(response.summary_nodes.is_empty(), "{provider}");
            assert_eq!(
                response.relation_projection_status,
                LcmRelationProjectionStatus::NotApplicable,
                "{provider}"
            );
            assert_eq!(
                response.frontier.current_frontier_store_id, None,
                "{provider}"
            );
            let status = db.lcm_status(provider, Some(&session_id)).await.unwrap();
            assert_eq!(status.summary_node_count, 0, "{provider}");
        }
    }

    #[tokio::test]
    async fn summary_convergence_resumes_after_restart_without_duplicate_summaries() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-restart").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        for session_id in ["convergence-session-a", "convergence-session-b"] {
            assert!(db.upsert_session(&session("codex", session_id)).await);
            for ordinal in 1..=4 {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                record.provider = "codex".to_string();
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }
            let summary_text = format!("native summary for {session_id}");
            ingest_codex_compaction_evidence(
                &db,
                session_id,
                &format!("{session_id}-summary"),
                5,
                &summary_text,
            )
            .await;
        }

        let first =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(first.sessions.len(), 1);
        assert!(first.has_more);
        assert_eq!(first.sessions[0].session_id, "convergence-session-a");
        assert_eq!(first.sessions[0].summary_nodes_created, 1);
        drop(db);

        let harness = harness.restart().await;
        let restarted = harness.registered.clone();
        let second = super::super::lcm_summary_convergence::run_summary_convergence_page(
            restarted.clone(),
            1,
        )
        .await
        .unwrap();
        assert_eq!(second.sessions.len(), 1);
        assert_eq!(second.sessions[0].session_id, "convergence-session-b");
        assert_eq!(second.sessions[0].summary_nodes_created, 1);

        let third = super::super::lcm_summary_convergence::run_summary_convergence_page(
            restarted.clone(),
            2,
        )
        .await
        .unwrap();
        assert!(
            third
                .sessions
                .iter()
                .all(|session| session.summary_nodes_created == 0)
        );
        for session_id in ["convergence-session-a", "convergence-session-b"] {
            let status = restarted
                .lcm_status("codex", Some(session_id))
                .await
                .unwrap();
            assert_eq!(status.raw_message_count, 6, "{session_id}");
            assert_eq!(status.summary_node_count, 1, "{session_id}");
        }
    }

    #[tokio::test]
    async fn interrupted_atomic_summary_settlement_resumes_without_duplicate() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-crash-window").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let session_id = "convergence-crash-window-session";
        assert!(db.upsert_session(&session("codex", session_id)).await);
        for ordinal in 1..=4 {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            record.provider = "codex".to_string();
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let summary_text = "native summary committed with queue settlement";
        ingest_codex_compaction_evidence(&db, session_id, "crash-window-summary", 5, summary_text)
            .await;

        let snapshot = db.read_snapshot().await.unwrap();
        let candidate = tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
            .await
            .unwrap()
            .unwrap();
        drop(snapshot);
        let mut interrupted_request = daemon_summary_request("codex", session_id);
        interrupted_request.summarizer = LcmSummarizerMode::Provided {
            summary_text: summary_text.to_string(),
            route: Some("codex_native_compaction".to_string()),
        };
        let interrupted = db
            .lcm_compress_retained_page_guarded(
                interrupted_request,
                &execution_control(),
                || Err(LcmError::Cancelled),
                retained_guard(None),
                Some(&candidate),
            )
            .await;
        assert_eq!(interrupted.unwrap_err(), LcmError::Cancelled);
        let interrupted_status = db.lcm_status("codex", Some(session_id)).await.unwrap();
        assert_eq!(interrupted_status.summary_node_count, 0);
        assert_eq!(
            interrupted_status.summary_convergence.pending_session_count, 1,
            "summary and queue settlement must roll back together"
        );
        drop(db);

        let harness = harness.restart().await;
        let restarted = harness.registered.clone();
        let replay = super::super::lcm_summary_convergence::run_summary_convergence_page(
            restarted.clone(),
            1,
        )
        .await
        .unwrap();
        assert_eq!(replay.sessions.len(), 1);
        assert_eq!(replay.sessions[0].summary_nodes_created, 1);
        let status = restarted
            .lcm_status("codex", Some(session_id))
            .await
            .unwrap();
        assert_eq!(status.summary_node_count, 1);
        assert_eq!(status.summary_convergence.current_session_count, 1);
    }

    #[tokio::test]
    async fn retained_commit_defers_whole_session_replay_and_relation_application() {
        let harness = RegisteredGlobalDbHarness::open("lcm-retained-deferred-maintenance").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let session_id = "retained-deferred-maintenance-session";
        assert!(db.upsert_session(&session("cursor", session_id)).await);
        for ordinal in 1..=8 {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let mut initial = compression_request(session_id);
        initial.summarizer = LcmSummarizerMode::Fake {
            summary_text: "old whole-session replay sentinel".to_string(),
        };
        DaemonLcmEffectService::new(db.clone(), None, None)
            .compress(initial)
            .await
            .unwrap();
        for ordinal in 9..=16 {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let snapshot = db.read_snapshot().await.unwrap();
        let candidate = tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
            .await
            .unwrap()
            .unwrap();
        drop(snapshot);
        let mut retained = compression_request(session_id);
        retained.summarizer = LcmSummarizerMode::Fake {
            summary_text: "new bounded-page summary".to_string(),
        };
        let bounded = db
            .lcm_compress_retained_page_guarded(
                retained,
                &execution_control(),
                || Ok(()),
                retained_guard(None),
                Some(&candidate),
            )
            .await
            .unwrap();

        assert!(
            bounded
                .response
                .replay_messages
                .iter()
                .all(|message| !message
                    .to_string()
                    .contains("old whole-session replay sentinel")),
            "retained convergence must not hydrate whole-session summary history"
        );
        assert_eq!(
            bounded.response.relation_projection_status,
            LcmRelationProjectionStatus::Pending,
            "relation application belongs to its canonical bounded recovery lane"
        );
    }

    #[tokio::test]
    async fn summary_convergence_keeps_unsupported_provider_typed_pending() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-pending").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        assert!(
            db.upsert_session(&session("claude", "unsupported-convergence-session"))
                .await
        );
        for ordinal in 1..=8 {
            let mut record = message("unsupported-convergence-session", ordinal);
            record.provider = "claude".to_string();
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }

        let page =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(page.sessions.len(), 1);
        let result = &page.sessions[0];
        assert_eq!(result.provider, "claude");
        assert_eq!(
            result.disposition,
            super::super::lcm_summary_convergence::LcmSummaryConvergenceDisposition::Pending {
                reason: "authoritative_summarizer_unavailable".to_string(),
            }
        );
        assert_eq!(result.summary_nodes_created, 0);

        let status = db
            .lcm_status("claude", Some("unsupported-convergence-session"))
            .await
            .unwrap();
        assert_eq!(status.raw_message_count, 8);
        assert_eq!(status.summary_node_count, 0);
        assert_eq!(status.lifecycle.current_frontier_store_id, None);
        assert_eq!(status.summary_convergence.unavailable_session_count, 1);
    }

    #[tokio::test]
    async fn mounted_profile_scheduler_runs_summary_convergence_on_startup() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-scheduler").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let session_id = "mounted-convergence-session";
        assert!(db.upsert_session(&session("codex", session_id)).await);
        for ordinal in 1..=4 {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            record.provider = "codex".to_string();
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let summary_text = "native summary consumed by the mounted scheduler";
        ingest_codex_compaction_evidence(
            &db,
            session_id,
            "mounted-convergence-summary",
            5,
            summary_text,
        )
        .await;

        let registry =
            super::super::session_temporal_refresh_scheduler::registry::SessionTemporalRefreshSchedulerRegistry::default();
        let database_path = db.db_path().to_path_buf();
        registry
            .ensure_profile(database_path.clone(), db.clone())
            .await;
        assert!(
            registry
                .wait_profile_idle(&database_path, Duration::from_secs(10))
                .await
        );
        let status = db.lcm_status("codex", Some(session_id)).await.unwrap();
        registry.shutdown().await;

        assert_eq!(status.summary_node_count, 1);
        assert_eq!(status.raw_message_count, 6);
    }

    #[tokio::test]
    async fn mounted_schedulers_share_historical_work_admission() {
        let mut stores = Vec::new();
        for index in 0..3 {
            let harness = RegisteredGlobalDbHarness::open(&format!(
                "lcm-summary-convergence-admission-{index}"
            ))
            .await;
            let db = harness.registered.clone();
            let session_id = format!("admission-session-{index}");
            let mut messages = Vec::new();
            for ordinal in 1..=4 {
                let mut record = message(&session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                record.provider = "codex".to_string();
                messages.push(record);
            }
            let summary_text = format!("native admission summary {index}");
            let mut compaction = message(&session_id, 5);
            compaction.provider = "codex".to_string();
            compaction.message_id = format!("{session_id}-summary");
            compaction.kind = Some("summary".to_string());
            compaction.text = summary_text;
            compaction.metadata_json = Some(
                serde_json::json!({
                    "source": "codex_context_compacted",
                    "summary_body": "plaintext"
                })
                .to_string(),
            );
            messages.push(compaction);
            let mut tail = message(&session_id, 6);
            tail.provider = "codex".to_string();
            tail.message_id = format!("{session_id}-tail");
            messages.push(tail);
            assert!(
                db.upsert_transcript_batch(
                    &session("codex", &session_id),
                    &messages,
                    &format!("/tmp/{session_id}.jsonl"),
                    ParseOffset::default(),
                )
                .await
            );
            stores.push((harness, db, session_id));
        }

        let registry = super::super::session_temporal_refresh_scheduler::registry::SessionTemporalRefreshSchedulerRegistry::default();
        let admission = registry.historical_ingest_admission();
        let permit_count = admission.available_permits();
        let held = std::sync::Arc::clone(&admission)
            .acquire_many_owned(u32::try_from(permit_count).unwrap())
            .await
            .unwrap();
        for (_, db, _) in &stores {
            registry
                .ensure_profile(db.db_path().to_path_buf(), db.clone())
                .await;
        }
        for (_, db, session_id) in &stores {
            assert!(
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if registry.profile_pass_count(db.db_path()).await >= 1
                            && registry
                                .wait_profile_idle(db.db_path(), Duration::from_millis(25))
                                .await
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .is_ok(),
                "worker must reach the occupied shared admission before it is released"
            );
            assert_eq!(
                db.lcm_status("codex", Some(session_id))
                    .await
                    .unwrap()
                    .summary_node_count,
                0,
                "convergence bypassed the occupied daemon-wide admission"
            );
        }

        drop(held);
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut complete = true;
                for (_, db, session_id) in &stores {
                    complete &= db
                        .lcm_status("codex", Some(session_id))
                        .await
                        .unwrap()
                        .summary_node_count
                        == 1;
                }
                if complete {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        if completed.is_err() {
            let mut states = Vec::new();
            for (_, db, session_id) in &stores {
                let status = db.lcm_status("codex", Some(session_id)).await.unwrap();
                states.push((
                    session_id.clone(),
                    status.summary_node_count,
                    status.summary_convergence,
                    registry.profile_pass_count(db.db_path()).await,
                ));
            }
            panic!(
                "admission release should wake every bounded worker (available={}): {states:?}",
                admission.available_permits()
            );
        }
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn permanent_session_failure_does_not_starve_later_sessions_after_restart() {
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-permanent").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        for session_id in ["permanent-session-a", "healthy-session-b"] {
            assert!(db.upsert_session(&session("codex", session_id)).await);
            for ordinal in 1..=4 {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                record.provider = "codex".to_string();
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }
            let summary_text = format!("native summary for {session_id}");
            ingest_codex_compaction_evidence(
                &db,
                session_id,
                &format!("{session_id}-summary"),
                5,
                &summary_text,
            )
            .await;
        }
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE lcm_raw_messages
                 SET metadata_json = '{\"ingest_protection\":{\"sanitization_receipt\":\"invalid\"}}'
                 WHERE provider = 'codex' AND session_id = 'permanent-session-a'",
                (),
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let first =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert!(matches!(
            first.sessions[0].disposition,
            super::super::lcm_summary_convergence::LcmSummaryConvergenceDisposition::Permanent { .. }
        ));
        assert_eq!(
            db.lcm_status("codex", Some("permanent-session-a"))
                .await
                .unwrap()
                .summary_convergence
                .permanent_session_count,
            1
        );
        drop(db);

        let harness = harness.restart().await;
        let restarted = harness.registered.clone();
        let second = super::super::lcm_summary_convergence::run_summary_convergence_page(
            restarted.clone(),
            1,
        )
        .await
        .unwrap();
        assert_eq!(second.sessions[0].session_id, "healthy-session-b");
        assert_eq!(second.sessions[0].summary_nodes_created, 1);
        let third = super::super::lcm_summary_convergence::run_summary_convergence_page(
            restarted.clone(),
            1,
        )
        .await
        .unwrap();
        assert!(third.sessions.is_empty(), "permanent work was reselected");
    }

    #[tokio::test]
    async fn malformed_relation_receipt_is_permanent_without_starving_summary_work() {
        let harness = RegisteredGlobalDbHarness::open("lcm-relation-recovery-poison").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let poison_session = "relation-poison-session";
        assert!(db.upsert_session(&session("cursor", poison_session)).await);
        for ordinal in 1..=4 {
            let mut record = message(poison_session, ordinal);
            record.message_id = format!("{poison_session}-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let snapshot = db.read_snapshot().await.unwrap();
        let poison_candidate =
            tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
                .await
                .unwrap()
                .unwrap();
        drop(snapshot);
        let mut poison_request = daemon_summary_request("cursor", poison_session);
        poison_request.summarizer = LcmSummarizerMode::Provided {
            summary_text: "summary with a poisoned relation receipt".to_string(),
            route: Some("test_relation_poison".to_string()),
        };
        let poison = db
            .lcm_compress_retained_page_guarded(
                poison_request,
                &execution_control(),
                || Ok(()),
                retained_guard(None),
                Some(&poison_candidate),
            )
            .await
            .unwrap();
        assert_eq!(poison.response.summary_nodes_created, 1);
        let snapshot = db.read_snapshot().await.unwrap();
        let mut projection_rows = snapshot
            .query(
                "SELECT projection_json FROM session_relation_effect_journal
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        let valid_projection_json = projection_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        drop(projection_rows);
        drop(snapshot);
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE session_relation_effect_journal
                 SET projection_json = '{}'
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let healthy_session = "relation-poison-healthy-session";
        assert!(db.upsert_session(&session("codex", healthy_session)).await);
        for ordinal in 1..=4 {
            let mut record = message(healthy_session, ordinal);
            record.provider = "codex".to_string();
            record.message_id = format!("{healthy_session}-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        ingest_codex_compaction_evidence(
            &db,
            healthy_session,
            "relation-poison-healthy-summary",
            5,
            "healthy summary after poisoned relation receipt",
        )
        .await;

        let converged =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(converged.sessions[0].session_id, healthy_session);
        assert_eq!(converged.sessions[0].summary_nodes_created, 1);
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT recovery_state, recovery_failure_code
                 FROM session_relation_receipts
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "permanent");
        assert_eq!(
            row.get::<Option<String>>(1).unwrap().as_deref(),
            Some("journal_malformed")
        );
        drop(rows);
        drop(snapshot);
        let mut cyclic_projection =
            serde_json::from_str::<serde_json::Value>(&valid_projection_json).unwrap();
        cyclic_projection["parent_session_id"] = serde_json::json!(poison_session);
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE session_relation_effect_journal
                 SET projection_json = ?2
                 WHERE session_id = ?1",
                params![poison_session, cyclic_projection.to_string()],
            )
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE session_relation_receipts
                 SET recovery_state = 'pending', recovery_failure_code = NULL,
                     recovery_failure_count = 0, recovery_next_attempt_at = 0
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        db.recover_pending_session_relation_projection_page(
            16,
            std::sync::Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .await
        .unwrap();
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT recovery_state, recovery_failure_code
                 FROM session_relation_receipts
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "permanent");
        assert_eq!(
            row.get::<Option<String>>(1).unwrap().as_deref(),
            Some("relation_receipt_mismatch")
        );
        drop(rows);
        drop(snapshot);
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE session_relation_receipts
                 SET recovery_state = 'retryable', recovery_next_attempt_at = unixepoch() + 60
                 WHERE session_id = ?1",
                params![poison_session],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let sleeping =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert!(sleeping.sessions.is_empty());
        assert!(
            sleeping.next_retry_delay.is_some_and(
                |delay| !delay.is_zero() && delay <= std::time::Duration::from_secs(60)
            ),
            "the scheduler must retain the isolated relation retry deadline"
        );
    }

    #[tokio::test]
    async fn mega_session_convergence_bounds_protection_and_compression_pages() {
        const RAW_ROWS: i64 = tracedecay_lcm::LCM_SCAN_PAGE_ROWS + 1;
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-mega").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let session_id = "mega-convergence-session";
        assert!(db.upsert_session(&session("cursor", session_id)).await);
        for ordinal in 1..=RAW_ROWS {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            record.text = format!("{ordinal:04}:{}", "bounded retained context ".repeat(32));
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }

        let first =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(first.sessions.len(), 1);
        assert_eq!(
            first.sessions[0].disposition,
            super::super::lcm_summary_convergence::LcmSummaryConvergenceDisposition::Preparing
        );
        assert_eq!(
            first.sessions[0].protection_rows_scanned,
            tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize
        );
        assert_eq!(first.sessions[0].compression_rows_scanned, 0);
        assert!(
            first.sessions[0].protection_bytes_scanned
                <= tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64
        );

        let second =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert!(
            second.sessions[0].compression_rows_scanned
                <= tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize
        );
        assert!(
            second.sessions[0].compression_bytes_scanned
                <= tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64
        );
        assert_eq!(
            db.lcm_status("cursor", Some(session_id))
                .await
                .unwrap()
                .raw_message_count,
            RAW_ROWS
        );
    }

    #[cfg(unix)]
    #[test]
    fn retained_pages_never_reuse_unbound_session_wide_native_text() {
        run_with_test_env_lock(async {
            const RAW_ROWS: i64 = tracedecay_lcm::LCM_SCAN_PAGE_ROWS + 1;
            let harness = RegisteredGlobalDbHarness::open("lcm-page-bound-summary").await;
            let db = harness.registered.clone();
            let storage_root = db.db_path().parent().unwrap();
            let session_id = "page-bound-summary-session";
            assert!(db.upsert_session(&session("cursor", session_id)).await);
            for ordinal in 1..=RAW_ROWS {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }
            let native_text = "one native summary for the entire retained session";
            let metadata = canonical_envelope(
                "cursor",
                session_id,
                "session-wide-native-summary",
                None,
                vec![serde_json::json!({
                    "kind": "compaction",
                    "summary": native_text
                })],
            );
            insert_summary_evidence(
                (&db, "cursor"),
                session_id,
                "session-wide-native-summary",
                RAW_ROWS + 1,
                native_text,
                "message",
                &metadata,
            )
            .await;

            let temporary = tempfile::tempdir().unwrap();
            let cursor_bin = temporary.path().join("cursor-agent");
            let counter = temporary.path().join("calls");
            std::fs::write(
                &cursor_bin,
                format!(
                    "#!/bin/sh\ncount=$(cat '{}' 2>/dev/null || printf 0)\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nprintf 'page-bound summary %s\\n' \"$count\"\n",
                    counter.display(),
                    counter.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cursor_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cursor_bin_env = cursor_bin.to_string_lossy().into_owned();
            let workspace_env = temporary.path().to_string_lossy().into_owned();
            let _env = TestEnvironment::set([
                ("TRACEDECAY_CURSOR_AGENT_BIN", cursor_bin_env.as_str()),
                (
                    "TRACEDECAY_CURSOR_SUMMARY_WORKSPACE",
                    workspace_env.as_str(),
                ),
                ("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS", "5"),
            ]);

            for _ in 0..8 {
                super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                    .await
                    .unwrap();
                if db
                    .lcm_status("cursor", Some(session_id))
                    .await
                    .unwrap()
                    .summary_node_count
                    >= 2
                {
                    break;
                }
            }
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT summary_text
                     FROM lcm_summary_nodes
                     WHERE provider = 'cursor' AND session_id = ?1 AND depth = 0
                     ORDER BY created_at, node_id
                     LIMIT 2",
                    params![session_id],
                )
                .await
                .unwrap();
            let first = rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap();
            let second = rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap();
            assert_ne!(
                first, second,
                "each raw page requires page-bound summary text"
            );
            assert!(
                first != native_text || second != native_text,
                "one range-bound native result cannot be reused for another raw page"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn protected_in_place_revision_stales_old_summary_before_reconvergence() {
        run_with_test_env_lock(async {
            let harness = RegisteredGlobalDbHarness::open("lcm-revised-raw-summary").await;
            let db = harness.registered.clone();
            let storage_root = db.db_path().parent().unwrap();
            let session_id = "revised-raw-summary-session";
            assert!(db.upsert_session(&session("cursor", session_id)).await);
            for ordinal in 1..=4 {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }
            let old_text = "native summary of the original protected content";
            let snapshot = db.read_snapshot().await.unwrap();
            let initial_candidate =
                tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
                    .await
                    .unwrap()
                    .unwrap();
            drop(snapshot);
            let mut initial_request = daemon_summary_request("cursor", session_id);
            initial_request.summarizer = LcmSummarizerMode::Provided {
                summary_text: old_text.to_string(),
                route: Some("test_exact_source_summary".to_string()),
            };
            let initial = db
                .lcm_compress_retained_page_guarded(
                    initial_request,
                    &execution_control(),
                    || Ok(()),
                    retained_guard(None),
                    Some(&initial_candidate),
                )
                .await
                .unwrap();
            assert_eq!(initial.response.summary_nodes_created, 1);
            let old_summary_id = {
                let snapshot = db.read_snapshot().await.unwrap();
                let mut rows = snapshot
                    .query(
                        "SELECT node_id FROM lcm_summary_nodes
                         WHERE provider = 'cursor' AND session_id = ?1",
                        params![session_id],
                    )
                    .await
                    .unwrap();
                rows.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap()
            };

            let temporary = tempfile::tempdir().unwrap();
            let cursor_bin = temporary.path().join("cursor-agent");
            std::fs::write(
                &cursor_bin,
                "#!/bin/sh\nprintf '%s\\n' 'summary of the revised protected content'\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cursor_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cursor_bin_env = cursor_bin.to_string_lossy().into_owned();
            let workspace_env = temporary.path().to_string_lossy().into_owned();
            let _env = TestEnvironment::set([
                ("TRACEDECAY_CURSOR_AGENT_BIN", cursor_bin_env.as_str()),
                (
                    "TRACEDECAY_CURSOR_SUMMARY_WORKSPACE",
                    workspace_env.as_str(),
                ),
                ("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS", "5"),
            ]);
            let mut revised = message(session_id, 1);
            revised.message_id = format!("{session_id}-message-1");
            revised.text = "authoritative revised protected content".to_string();
            db.lcm_ingest_raw_message(storage_root, &revised)
                .await
                .unwrap();
            assert_eq!(
                db.lcm_status("cursor", Some(session_id))
                    .await
                    .unwrap()
                    .summary_convergence
                    .pending_session_count,
                1
            );

            let reconverged =
                super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                    .await
                    .unwrap();
            assert_eq!(reconverged.sessions[0].summary_nodes_created, 1);
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT availability.summary_id, availability.availability,
                            availability.reason, node.summary_text
                     FROM session_temporal_generations AS generation
                     JOIN session_summary_availability AS availability
                       ON availability.session_id = generation.session_id
                      AND availability.generation = generation.generation
                     JOIN session_summary_nodes AS node
                       ON node.summary_id = availability.summary_id
                     WHERE generation.session_id = ?1 AND generation.state = 'active'
                     ORDER BY availability.summary_id",
                    params![session_id],
                )
                .await
                .unwrap();
            let mut active = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                active.push((
                    row.get::<String>(0).unwrap(),
                    row.get::<String>(1).unwrap(),
                    row.get::<Option<String>>(2).unwrap(),
                    row.get::<String>(3).unwrap(),
                ));
            }
            assert!(active.iter().any(|(id, state, reason, _)| {
                id == &old_summary_id
                    && state == "stale"
                    && reason.as_deref() == Some("raw_source_revised")
            }));
            let current = active
                .iter()
                .filter(|(_, state, _, _)| state == "available")
                .collect::<Vec<_>>();
            assert_eq!(current.len(), 1);
            assert_ne!(current[0].0, old_summary_id);
            assert_eq!(current[0].3, "summary of the revised protected content");
        });
    }

    #[cfg(unix)]
    #[test]
    fn disjoint_published_summary_revisions_both_reconverge_across_restart() {
        run_with_test_env_lock(async {
            let harness = RegisteredGlobalDbHarness::open("lcm-disjoint-summary-revisions").await;
            let db = harness.registered.clone();
            let storage_root = db.db_path().parent().unwrap();
            let session_id = "disjoint-summary-revisions";
            assert!(db.upsert_session(&session("cursor", session_id)).await);
            for ordinal in 1..=6 {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                db.lcm_ingest_raw_message(storage_root, &record)
                    .await
                    .unwrap();
            }
            for (summary_index, summary_text) in ["old disjoint leaf one", "old disjoint leaf two"]
                .into_iter()
                .enumerate()
            {
                if summary_index == 1 {
                    for ordinal in 7..=12 {
                        let mut record = message(session_id, ordinal);
                        record.message_id = format!("{session_id}-message-{ordinal}");
                        db.lcm_ingest_raw_message(storage_root, &record)
                            .await
                            .unwrap();
                    }
                }
                let snapshot = db.read_snapshot().await.unwrap();
                let candidate =
                    tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
                        .await
                        .unwrap()
                        .unwrap();
                drop(snapshot);
                let mut request = daemon_summary_request("cursor", session_id);
                request.max_source_messages = Some(4);
                request.fresh_tail_count = Some(0);
                request.summarizer = LcmSummarizerMode::Provided {
                    summary_text: summary_text.to_string(),
                    route: Some("test_disjoint_revision".to_string()),
                };
                let compressed = db
                    .lcm_compress_retained_page_guarded(
                        request,
                        &execution_control(),
                        || Ok(()),
                        retained_guard(None),
                        Some(&candidate),
                    )
                    .await
                    .unwrap();
                assert_eq!(compressed.response.summary_nodes_created, 1);
            }
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT node.node_id, MIN(CAST(source.source_id AS INTEGER))
                     FROM lcm_summary_nodes AS node
                     JOIN lcm_summary_sources AS source ON source.node_id = node.node_id
                     WHERE node.provider = 'cursor' AND node.session_id = ?1
                       AND node.depth = 0 AND source.source_kind = 'raw_message'
                     GROUP BY node.node_id
                     ORDER BY MIN(CAST(source.source_id AS INTEGER))
                     LIMIT 2",
                    params![session_id],
                )
                .await
                .unwrap();
            let mut old = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                old.push((row.get::<String>(0).unwrap(), row.get::<i64>(1).unwrap()));
            }
            drop(rows);
            drop(snapshot);
            assert_eq!(old.len(), 2);
            assert_ne!(old[0].1, old[1].1);
            for (revision, (_, store_id)) in old.iter().enumerate() {
                let snapshot = db.read_snapshot().await.unwrap();
                let mut rows = snapshot
                    .query(
                        "SELECT message_id, ordinal FROM lcm_raw_messages WHERE store_id = ?1",
                        params![*store_id],
                    )
                    .await
                    .unwrap();
                let row = rows.next().await.unwrap().unwrap();
                let message_id = row.get::<String>(0).unwrap();
                let ordinal = row.get::<i64>(1).unwrap();
                drop(rows);
                drop(snapshot);
                let mut revised = message(session_id, ordinal);
                revised.message_id = message_id;
                revised.text = format!("revised disjoint source {revision}");
                db.lcm_ingest_raw_message(storage_root, &revised)
                    .await
                    .unwrap();
            }

            let temporary = tempfile::tempdir().unwrap();
            let cursor_bin = temporary.path().join("cursor-agent");
            let counter = temporary.path().join("calls");
            std::fs::write(
                &cursor_bin,
                format!(
                    "#!/bin/sh\ncount=$(cat '{}' 2>/dev/null || printf 0)\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nprintf 'replacement disjoint leaf %s\\n' \"$count\"\n",
                    counter.display(),
                    counter.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cursor_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cursor_bin_env = cursor_bin.to_string_lossy().into_owned();
            let workspace_env = temporary.path().to_string_lossy().into_owned();
            let _env = TestEnvironment::set([
                ("TRACEDECAY_CURSOR_AGENT_BIN", cursor_bin_env.as_str()),
                (
                    "TRACEDECAY_CURSOR_SUMMARY_WORKSPACE",
                    workspace_env.as_str(),
                ),
                ("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS", "5"),
            ]);

            let first =
                super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                    .await
                    .unwrap();
            assert_eq!(first.sessions[0].summary_nodes_created, 0);
            assert!(matches!(
                first.sessions[0].disposition,
                super::super::lcm_summary_convergence::LcmSummaryConvergenceDisposition::Preparing
            ));
            drop(db);

            let harness = harness.restart().await;
            let restarted = harness.registered.clone();
            for _ in 0..8 {
                super::super::lcm_summary_convergence::run_summary_convergence_page(
                    restarted.clone(),
                    1,
                )
                .await
                .unwrap();
                let status = restarted
                    .lcm_status("cursor", Some(session_id))
                    .await
                    .unwrap();
                if status.summary_convergence.current_session_count == 1 {
                    break;
                }
            }
            let snapshot = restarted.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT availability.summary_id, availability.availability
                     FROM session_temporal_generations AS generation
                     JOIN session_summary_availability AS availability
                       ON availability.session_id = generation.session_id
                      AND availability.generation = generation.generation
                     WHERE generation.session_id = ?1 AND generation.state = 'active'",
                    params![session_id],
                )
                .await
                .unwrap();
            let mut stale_old = 0;
            let mut available_replacements = 0;
            while let Some(row) = rows.next().await.unwrap() {
                let summary_id = row.get::<String>(0).unwrap();
                let availability = row.get::<String>(1).unwrap();
                if old.iter().any(|(old_id, _)| old_id == &summary_id) {
                    stale_old += usize::from(availability == "stale");
                } else {
                    available_replacements += usize::from(availability == "available");
                }
            }
            assert_eq!(stale_old, 2);
            assert!(available_replacements > 0);
            drop(rows);
            for (_, revised_store_id) in &old {
                let mut sources = snapshot
                    .query(
                        "SELECT COUNT(*)
                         FROM session_temporal_generations AS generation
                         JOIN session_summary_availability AS availability
                           ON availability.session_id = generation.session_id
                          AND availability.generation = generation.generation
                          AND availability.availability = 'available'
                         JOIN lcm_summary_sources AS source
                           ON source.node_id = availability.summary_id
                          AND source.source_kind = 'raw_message'
                         WHERE generation.session_id = ?1
                           AND generation.state = 'active'
                           AND source.source_id = ?2",
                        params![session_id, revised_store_id.to_string()],
                    )
                    .await
                    .unwrap();
                assert!(
                    sources
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .get::<i64>(0)
                        .unwrap()
                        > 0,
                    "each revised closure must have an available replacement source"
                );
            }
            let status = restarted
                .lcm_status("cursor", Some(session_id))
                .await
                .unwrap();
            assert_eq!(status.summary_convergence.current_session_count, 1);
            assert_eq!(status.summary_convergence.permanent_session_count, 0);
        });
    }

    #[cfg(unix)]
    #[test]
    fn retained_summary_rejects_a_role_revision_during_model_generation() {
        run_with_test_env_lock(async {
            let harness = RegisteredGlobalDbHarness::open("lcm-summary-revision-barrier").await;
            let db = harness.registered.clone();
            let storage_root = db.db_path().parent().unwrap().to_path_buf();
            let session_id = "summary-revision-barrier-session";
            assert!(db.upsert_session(&session("cursor", session_id)).await);
            for ordinal in 1..=4 {
                let mut record = message(session_id, ordinal);
                record.message_id = format!("{session_id}-message-{ordinal}");
                db.lcm_ingest_raw_message(&storage_root, &record)
                    .await
                    .unwrap();
            }

            let temporary = tempfile::tempdir().unwrap();
            let cursor_bin = temporary.path().join("cursor-agent");
            let started = temporary.path().join("started");
            let release = temporary.path().join("release");
            std::fs::write(
                &cursor_bin,
                format!(
                    "#!/bin/sh\nprintf started > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s\\n' 'stale summary from before the revision'\n",
                    started.display(),
                    release.display(),
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cursor_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cursor_bin_env = cursor_bin.to_string_lossy().into_owned();
            let workspace_env = temporary.path().to_string_lossy().into_owned();
            let _env = TestEnvironment::set([
                ("TRACEDECAY_CURSOR_AGENT_BIN", cursor_bin_env.as_str()),
                (
                    "TRACEDECAY_CURSOR_SUMMARY_WORKSPACE",
                    workspace_env.as_str(),
                ),
                ("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS", "5"),
            ]);

            let convergence_db = db.clone();
            let convergence = tokio::spawn(async move {
                super::super::lcm_summary_convergence::run_summary_convergence_page(
                    convergence_db,
                    1,
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !started.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("provider invocation did not reach the revision barrier");

            let mut revised = message(session_id, 1);
            revised.message_id = format!("{session_id}-message-1");
            revised.role = "system".to_string();
            db.lcm_ingest_raw_message(&storage_root, &revised)
                .await
                .unwrap();
            std::fs::write(&release, "release").unwrap();

            let first = convergence.await.unwrap().unwrap();
            assert_eq!(first.sessions[0].summary_nodes_created, 0);
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT summary_text FROM lcm_summary_nodes
                     WHERE provider = 'cursor' AND session_id = ?1",
                    params![session_id],
                )
                .await
                .unwrap();
            assert!(
                rows.next().await.unwrap().is_none(),
                "text generated before a role-only raw revision must never be published"
            );
            drop(rows);
            drop(snapshot);
            assert_eq!(
                db.lcm_status("cursor", Some(session_id))
                    .await
                    .unwrap()
                    .summary_convergence
                    .pending_session_count,
                1
            );
        });
    }

    #[tokio::test]
    async fn raw_message_identity_cannot_move_between_sessions_after_summary_publication() {
        let harness = RegisteredGlobalDbHarness::open("lcm-raw-session-ownership").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let old_session = "raw-owner-old-session";
        let new_session = "raw-owner-new-session";
        assert!(db.upsert_session(&session("cursor", old_session)).await);
        assert!(db.upsert_session(&session("cursor", new_session)).await);
        for ordinal in 1..=4 {
            let mut record = message(old_session, ordinal);
            record.message_id = format!("owned-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let snapshot = db.read_snapshot().await.unwrap();
        let candidate = tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
            .await
            .unwrap()
            .unwrap();
        drop(snapshot);
        let mut request = daemon_summary_request("cursor", old_session);
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: "summary owned by the original session".to_string(),
            route: Some("test_exact_source_summary".to_string()),
        };
        let initial = db
            .lcm_compress_retained_page_guarded(
                request,
                &execution_control(),
                || Ok(()),
                retained_guard(None),
                Some(&candidate),
            )
            .await
            .unwrap();
        assert_eq!(initial.response.summary_nodes_created, 1);

        let mut moved = message(new_session, 1);
        moved.message_id = "owned-message-1".to_string();
        let error = db
            .lcm_ingest_raw_message(storage_root, &moved)
            .await
            .unwrap_err();
        assert_eq!(error, LcmError::SummarySourceNotOwnedBySession);

        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT session_id FROM lcm_raw_messages
                 WHERE provider = 'cursor' AND message_id = 'owned-message-1'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            old_session
        );
        assert_eq!(
            db.lcm_status("cursor", Some(old_session))
                .await
                .unwrap()
                .summary_node_count,
            1
        );
    }

    #[tokio::test]
    async fn retained_summary_publication_requires_the_exact_planned_source_range() {
        let harness = RegisteredGlobalDbHarness::open("lcm-retained-source-range-cas").await;
        let db = harness.registered.clone();
        let storage_root = db.db_path().parent().unwrap();
        let session_id = "retained-source-range-cas-session";
        assert!(db.upsert_session(&session("cursor", session_id)).await);
        for ordinal in 1..=4 {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("range-cas-message-{ordinal}");
            db.lcm_ingest_raw_message(storage_root, &record)
                .await
                .unwrap();
        }
        let snapshot = db.read_snapshot().await.unwrap();
        let candidate = tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
            .await
            .unwrap()
            .unwrap();
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = 'cursor' AND session_id = ?1
                 ORDER BY store_id LIMIT 2",
                params![session_id],
            )
            .await
            .unwrap();
        let first = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        let second = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        drop(rows);
        drop(snapshot);
        let mut request = daemon_summary_request("cursor", session_id);
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: "summary generated for a different page".to_string(),
            route: Some("test_exact_source_summary".to_string()),
        };
        let error = db
            .lcm_compress_retained_page_guarded(
                request,
                &execution_control(),
                || Ok(()),
                retained_guard(Some(tracedecay_lcm::LcmSummarySourceRange {
                    from_store_id: first,
                    to_store_id: second,
                })),
                Some(&candidate),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            LcmError::StaleSummarySourceRange {
                expected_from: first,
                expected_to: second,
                actual_from: Some(first),
                actual_to: Some(first),
            }
        );
        assert_eq!(
            db.lcm_status("cursor", Some(session_id))
                .await
                .unwrap()
                .summary_node_count,
            0
        );
    }

    #[tokio::test]
    async fn large_byte_session_stops_each_retained_pass_at_the_existing_budget() {
        const RAW_ROWS: i64 = 65;
        let harness = RegisteredGlobalDbHarness::open("lcm-summary-convergence-large-bytes").await;
        let db = harness.registered.clone();
        let session_id = "large-byte-convergence-session";
        assert!(db.upsert_session(&session("cursor", session_id)).await);
        let body = "bounded retained byte context ".repeat(20_000);
        assert!(body.len() < tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as usize);
        let transaction = db.begin_write_transaction().await.unwrap();
        for ordinal in 1..=RAW_ROWS {
            let mut record = message(session_id, ordinal);
            record.message_id = format!("{session_id}-message-{ordinal}");
            record.text = format!("{ordinal:04}:{body}");
            transaction
                .execute(
                    "INSERT INTO session_messages (
                        provider, message_id, session_id, role, timestamp, ordinal, text
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        record.provider.as_str(),
                        record.message_id.as_str(),
                        record.session_id.as_str(),
                        record.role.as_str(),
                        record.timestamp,
                        record.ordinal,
                        record.text.as_str(),
                    ],
                )
                .await
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO lcm_raw_messages (
                        provider, message_id, session_id, role, ordinal, timestamp,
                        content, content_hash, storage_kind, snippet_text, index_text,
                        metadata_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?2, 'inline', '', '', '{}')",
                    params![
                        record.provider.as_str(),
                        record.message_id.as_str(),
                        record.session_id.as_str(),
                        record.role.as_str(),
                        record.ordinal,
                        record.timestamp,
                    ],
                )
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();

        let first =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert_eq!(
            first.sessions[0].disposition,
            super::super::lcm_summary_convergence::LcmSummaryConvergenceDisposition::Preparing
        );
        assert!(first.sessions[0].protection_rows_scanned < RAW_ROWS as usize);
        assert!(
            first.sessions[0].protection_bytes_scanned
                <= tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64
        );

        let second =
            super::super::lcm_summary_convergence::run_summary_convergence_page(db.clone(), 1)
                .await
                .unwrap();
        assert!(second.sessions[0].compression_rows_scanned > 0);
        assert!(
            second.sessions[0].compression_bytes_scanned
                <= tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64
        );
        assert_eq!(
            db.lcm_status("cursor", Some(session_id))
                .await
                .unwrap()
                .raw_message_count,
            RAW_ROWS
        );
    }

    #[test]
    fn concurrent_raw_revision_cannot_be_overwritten_by_staged_protection() {
        run_with_test_env_lock(async {
            const RAW_ROWS: i64 = 32;
            let harness = RegisteredGlobalDbHarness::open("lcm-protection-revision-barrier").await;
            let db = harness.registered.clone();
            let storage_root = db.db_path().parent().unwrap().to_path_buf();
            let session_id = "protection-revision-barrier-session";
            assert!(db.upsert_session(&session("cursor", session_id)).await);
            let transaction = db.begin_write_transaction().await.unwrap();
            for ordinal in 1..=RAW_ROWS {
                let text = format!("source-a-{ordinal}:{}", "x".repeat(768 * 1024));
                transaction
                    .execute(
                        "INSERT INTO session_messages (
                            provider, message_id, session_id, role, timestamp, ordinal, text, kind
                         ) VALUES ('cursor', ?1, ?2, 'tool', ?3, ?3, ?4, 'tool_result')",
                        params![
                            format!("barrier-message-{ordinal}"),
                            session_id,
                            ordinal,
                            text
                        ],
                    )
                    .await
                    .unwrap();
                transaction
                    .execute(
                        "INSERT INTO lcm_raw_messages (
                            provider, message_id, session_id, role, ordinal, timestamp,
                            content, content_hash, storage_kind, snippet_text, index_text,
                            metadata_json
                         ) VALUES ('cursor', ?1, ?2, 'tool', ?3, ?3, '', ?1,
                                   'inline', '', '', '{}')",
                        params![format!("barrier-message-{ordinal}"), session_id, ordinal],
                    )
                    .await
                    .unwrap();
            }
            transaction.commit().await.unwrap();

            let protection_db = db.clone();
            let protection = tokio::spawn(async move {
                protection_db
                    .lcm_protect_session_raw_messages_page(
                        "cursor",
                        session_id,
                        0,
                        RAW_ROWS as usize,
                        tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64,
                    )
                    .await
            });
            let payload_dir = tracedecay_lcm::payload::payload_dir(&storage_root);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if payload_dir
                        .read_dir()
                        .is_ok_and(|mut entries| entries.next().is_some())
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("protection did not reach staged payload persistence");

            let unrelated_db = db.clone();
            tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                assert!(
                    unrelated_db
                        .upsert_session(&session("cursor", "unrelated-during-protection"))
                        .await
                );
            })
            .await
            .expect("payload staging must not hold an Immediate writer transaction");

            let mut revised = message(session_id, 1);
            revised.message_id = "barrier-message-1".to_string();
            revised.role = "tool".to_string();
            revised.kind = Some("tool_result".to_string());
            revised.text = format!("source-b: {}", "y".repeat(768 * 1024));
            let expected_hash =
                tracedecay_lcm::retrieval_content::projected_content_hash(&revised.text);
            let revision_db = db.clone();
            let revision_storage = storage_root.clone();
            let revision = tokio::spawn(async move {
                revision_db
                    .lcm_ingest_raw_message(&revision_storage, &revised)
                    .await
            });

            revision.await.unwrap().unwrap();
            assert!(matches!(
                protection.await.unwrap(),
                Err(LcmError::StaleRawProtectionSource { .. })
            ));
            let protected = db
                .lcm_protect_session_raw_messages_page(
                    "cursor",
                    session_id,
                    0,
                    RAW_ROWS as usize,
                    tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64,
                )
                .await
                .unwrap();
            assert_eq!(protected.rows_protected, RAW_ROWS as usize - 1);
            let snapshot = db.read_snapshot().await.unwrap();
            let mut rows = snapshot
                .query(
                    "SELECT store_id, content_hash FROM lcm_raw_messages
                     WHERE provider = 'cursor' AND message_id = 'barrier-message-1'",
                    (),
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            let revised_store_id = row.get::<i64>(0).unwrap();
            assert_eq!(row.get::<String>(1).unwrap(), expected_hash);
            drop(rows);
            let candidate =
                tracedecay_lcm::summary_convergence::next_candidate(&snapshot, i64::MAX)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(candidate.stale_from_store_id, Some(revised_store_id));
        });
    }

    fn canonical_envelope(
        provider: &str,
        session_id: &str,
        message_id: &str,
        parent_message_id: Option<&str>,
        facts: Vec<Value>,
    ) -> Value {
        let mut relations = serde_json::json!({
            "session_id": session_id,
            "message_id": message_id,
        });
        if let Some(parent_message_id) = parent_message_id {
            relations["parent_message_id"] = Value::String(parent_message_id.to_string());
        }
        serde_json::json!({
            "version": 1,
            "provider": provider,
            "native_record_kind": "compaction",
            "stable_record_id": message_id,
            "relations": relations,
            "facts": facts,
            "evidence": {
                "ordering_domain": "file_bytes",
                "range": {"start": 1, "end": 2}
            }
        })
    }

    async fn insert_summary_evidence(
        source: (&RegisteredGlobalDb, &str),
        session_id: &str,
        message_id: &str,
        ordinal: i64,
        text: &str,
        kind: &str,
        metadata: &Value,
    ) {
        let (db, provider) = source;
        let mut metadata = metadata.clone();
        let snapshot = db.read_snapshot().await.unwrap();
        let mut source_rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND session_id = ?2
                 ORDER BY store_id
                 LIMIT 9",
                params![provider, session_id],
            )
            .await
            .unwrap();
        let mut source_ids = Vec::new();
        while let Some(row) = source_rows.next().await.unwrap() {
            source_ids.push(row.get::<i64>(0).unwrap());
        }
        drop(source_rows);
        drop(snapshot);
        let summarized_source_count = source_ids
            .len()
            .saturating_sub(tracedecay_lcm::LCM_DEFAULT_FRESH_TAIL_COUNT);
        if provider == "codex"
            && let (Some(first_source_id), Some(last_source_id)) = (
                source_ids.first().copied(),
                source_ids
                    .get(summarized_source_count.saturating_sub(1))
                    .copied(),
            )
        {
            metadata["tracedecay_lcm_source_range"] = serde_json::json!({
                "from_store_id": first_source_id,
                "to_store_id": last_source_id,
            });
        }
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO session_messages (
                     provider, message_id, session_id, role, ordinal, text, kind, metadata_json
                 ) VALUES (?1, ?2, ?3, 'system', ?4, ?5, ?6, ?7)",
                tracedecay_runtime_core::db::engine::params![
                    provider,
                    message_id,
                    session_id,
                    ordinal,
                    text,
                    kind,
                    metadata.to_string(),
                ],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    async fn ingest_codex_compaction_evidence(
        db: &RegisteredGlobalDb,
        session_id: &str,
        message_id: &str,
        ordinal: i64,
        text: &str,
    ) {
        let mut compaction = message(session_id, ordinal);
        compaction.provider = "codex".to_string();
        compaction.message_id = message_id.to_string();
        compaction.kind = Some("summary".to_string());
        compaction.text = text.to_string();
        compaction.metadata_json = Some(
            serde_json::json!({
                "source": "codex_context_compacted",
                "summary_body": "plaintext"
            })
            .to_string(),
        );
        let mut tail = message(session_id, ordinal.saturating_add(1));
        tail.provider = "codex".to_string();
        tail.message_id = format!("{message_id}-tail");
        assert!(
            db.upsert_transcript_batch(
                &session("codex", session_id),
                &[compaction, tail],
                &format!("/tmp/{session_id}.jsonl"),
                ParseOffset::default(),
            )
            .await
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_and_cursor_daemon_adapters_commit_exact_authoritative_summaries() {
        run_with_test_env_lock(async {
            let harness = RegisteredGlobalDbHarness::open("lcm-provider-summary-adapters").await;
            let db = harness.registered.clone();
            let temporary = tempfile::tempdir().unwrap();
            let cursor_bin = temporary.path().join("cursor-agent");
            let codex_bin = temporary.path().join("codex");
            std::fs::write(
                &cursor_bin,
                "#!/bin/sh\nprintf '%s\\n' 'cursor authoritative summary'\n",
            )
            .unwrap();
            std::fs::write(
                &codex_bin,
                r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
    *'"id":1'*) printf '%s\n' '{"id":1,"result":{"thread":{"id":"thread-1","model":"codex-test-model"}}}' ;;
    *'"id":2'*)
      printf '%s\n' '{"method":"item/completed","params":{"model":"codex-test-model","item":{"content":[{"type":"output_text","text":"codex authoritative summary"}]}}}'
      printf '%s\n' '{"method":"turn/completed"}'
      ;;
  esac
done
"#,
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            for path in [&cursor_bin, &codex_bin] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let cursor_bin_env = cursor_bin.to_string_lossy().into_owned();
            let codex_bin_env = codex_bin.to_string_lossy().into_owned();
            let workspace_env = temporary.path().to_string_lossy().into_owned();
            let env = TestEnvironment::set([
                ("TRACEDECAY_CURSOR_AGENT_BIN", cursor_bin_env.as_str()),
                (
                    "TRACEDECAY_CURSOR_SUMMARY_WORKSPACE",
                    workspace_env.as_str(),
                ),
                ("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS", "5"),
                ("TRACEDECAY_CODEX_BIN", codex_bin_env.as_str()),
                ("TRACEDECAY_CODEX_SUMMARY_TIMEOUT_SECS", "5"),
            ]);

            for (provider, expected) in [
                ("cursor", "cursor authoritative summary"),
                ("codex", "codex authoritative summary"),
            ] {
                let session_id = format!("{provider}-adapter-session");
                assert!(db.upsert_session(&session(provider, &session_id)).await);
                let storage_root = db.db_path().parent().unwrap();
                for ordinal in 1..=8 {
                    let mut record = message(&session_id, ordinal);
                    record.provider = provider.to_string();
                    db.lcm_ingest_raw_message(storage_root, &record)
                        .await
                        .unwrap();
                }
                let response = DaemonLcmEffectService::new(db.clone(), None, None)
                    .compress(daemon_summary_request(provider, &session_id))
                    .await
                    .unwrap();
                assert_eq!(response.status, "ok");
                assert_eq!(response.summary_nodes_created, 1);
                assert_eq!(response.summary_nodes[0].summary_text, expected);
                assert!(!response.fallback_used);
                assert_eq!(
                    response.relation_projection_status,
                    LcmRelationProjectionStatus::Applied
                );
            }
            drop(env);
        });
    }

    struct TestEnvironment {
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl TestEnvironment {
        fn set<const N: usize>(values: [(&'static str, &str); N]) -> Self {
            let mut previous = Vec::with_capacity(N);
            for (name, value) in values {
                previous.push((name, std::env::var_os(name)));
                // SAFETY: tests serialize process-environment access through
                // the shared TraceDecay environment lock.
                unsafe { std::env::set_var(name, value) };
            }
            Self { previous }
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            for (name, value) in self.previous.drain(..).rev() {
                // SAFETY: the shared test environment lock remains held until
                // this guard restores every value.
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn boundary_apply_and_cancelled_rollback_are_observable() {
        let harness = RegisteredGlobalDbHarness::open("lcm-boundary-effect-journey").await;
        let db = harness.registered.clone();
        for session_id in ["old-session", "new-session", "cancelled-session"] {
            assert!(db.upsert_session(&session("cursor", session_id)).await);
        }
        let service = DaemonLcmEffectService::new(db.clone(), None, None);
        let response = service
            .session_boundary(LcmSessionBoundaryRequest {
                provider: "cursor".to_string(),
                session_id: "new-session".to_string(),
                old_session_id: Some("old-session".to_string()),
                boundary_reason: Some("compression".to_string()),
                bound_session_id: Some("old-session".to_string()),
                boundary_skip_at: None,
            })
            .await
            .unwrap();
        assert!(response.recorded);

        let cancelled = db
            .lcm_session_boundary_guarded(
                LcmSessionBoundaryRequest {
                    provider: "cursor".to_string(),
                    session_id: "cancelled-session".to_string(),
                    old_session_id: Some("old-session".to_string()),
                    boundary_reason: Some("compression".to_string()),
                    bound_session_id: Some("old-session".to_string()),
                    boundary_skip_at: None,
                },
                || Err(LcmError::Cancelled),
            )
            .await;
        assert_eq!(cancelled.unwrap_err(), LcmError::Cancelled);

        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT conversation_id FROM lcm_lifecycle_state
                 WHERE provider = 'cursor'
                 ORDER BY conversation_id",
                (),
            )
            .await
            .unwrap();
        let mut conversation_ids = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            conversation_ids.push(row.get::<String>(0).unwrap());
        }
        assert_eq!(conversation_ids, vec!["new-session"]);
    }
}
