use std::time::Duration;

use tracedecay_application::{CancellationSignal, Deadline};
use tracedecay_temporal_query::ports::ExecutionControl;

use crate::global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_sessions::runtime::lcm::{
    LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmSessionBoundaryRequest,
    LcmSessionBoundaryResponse, LcmSummarizerMode,
};

const LCM_EFFECT_CEILING: Duration = Duration::from_secs(30);
const LCM_EFFECT_WORK_LIMIT: usize = 4_096;

/// Daemon-owned execution boundary for retained LCM mutations.
///
/// The database authority remains lower-level storage. Host and MCP adapters
/// call this service so a disconnect or deadline can still roll back the open
/// transaction before its commit checkpoint.
#[derive(Clone)]
pub(crate) struct DaemonLcmEffectService {
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
            .and_then(crate::daemon_client::deadline_remaining)
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
    pub(crate) fn new(
        db: RegisteredGlobalDbLeaseV1,
        deadline: Option<&Deadline>,
        cancellation: Option<&CancellationSignal>,
    ) -> Self {
        Self {
            db,
            control: LcmEffectControl::new(deadline, cancellation),
        }
    }

    pub(crate) async fn compress(
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
                self.db
                    .lcm_protect_session_raw_messages(&request.provider, &request.session_id),
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

    async fn commit_compression(
        &self,
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                self.db
                    .lcm_compress_guarded(request, &execution, move || before_commit.checkpoint()),
            )
            .await
    }

    pub(crate) async fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let execution = self.control.execution_control();
        let before_commit = self.control.clone();
        self.control
            .execute(
                &execution,
                self.db
                    .lcm_session_boundary_guarded(request, move || before_commit.checkpoint()),
            )
            .await
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
    use crate::global_db::RegisteredGlobalDb;
    use crate::global_db::tests::harness::RegisteredGlobalDbHarness;
    use serde_json::Value;
    use tracedecay_domain::SessionId;
    use tracedecay_runtime_core::db::engine::params;
    use tracedecay_sessions::runtime::lcm::{
        LcmRelationProjectionStatus, LcmSourceRef, LcmSummarizerMode,
    };
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

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
            LcmRelationProjectionStatus::Pending
        );

        let session_id = SessionId::new("compress-session").unwrap();
        let relation_ids = [summary.node_id.clone()];
        let read_control = execution_control();
        let unavailable = db
            .active_session_summary_relations(
                &session_id,
                &relation_ids,
                4_096,
                crate::global_db::session_temporal::store::execution_control_graph_cancellation(
                    &read_control,
                ),
            )
            .await;
        assert!(
            unavailable.is_err(),
            "relation reads must not reconstruct a pending projection"
        );
        assert_eq!(
            db.recover_pending_session_relation_projections(
                1,
                crate::global_db::session_temporal::store::execution_control_graph_cancellation(
                    &read_control,
                ),
            )
            .await
            .unwrap(),
            1
        );
        let (_, relations) = db
            .active_session_summary_relations(
                &session_id,
                &relation_ids,
                4_096,
                crate::global_db::session_temporal::store::execution_control_graph_cancellation(
                    &read_control,
                ),
            )
            .await
            .unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].sources.len(), summary.source_refs.len());

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
                crate::global_db::session_temporal::store::execution_control_graph_cancellation(
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
            .lcm_preflight(tracedecay_sessions::runtime::lcm::LcmPreflightRequest {
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
            &db,
            "cursor",
            "cursor-native-session",
            "cursor-summary",
            10,
            cursor_text,
            "message",
            &cursor_metadata,
        )
        .await;
        insert_summary_evidence(
            &db,
            "codex",
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
            &db,
            "codex",
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
            &db,
            "claude",
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
            )
            .await
            .unwrap()
            .is_none(),
            "encrypted Codex compaction must remain non-authoritative"
        );
        insert_summary_evidence(
            &db,
            "codex",
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
            &db,
            "claude",
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
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claude.text, claude_text);
        assert_eq!(claude.route, "claude_native_compaction");
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
        db: &RegisteredGlobalDb,
        provider: &str,
        session_id: &str,
        message_id: &str,
        ordinal: i64,
        text: &str,
        kind: &str,
        metadata: &Value,
    ) {
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO session_messages (
                     provider, message_id, session_id, role, ordinal, text, kind, metadata_json
                 ) VALUES (?1, ?2, ?3, 'system', ?4, ?5, ?6, ?7)",
                crate::db::engine::params![
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

    #[cfg(unix)]
    #[test]
    fn codex_and_cursor_daemon_adapters_commit_exact_authoritative_summaries() {
        crate::hooks::run_with_test_env_lock(async {
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
                    LcmRelationProjectionStatus::Pending
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
