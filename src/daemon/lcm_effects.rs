use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{CancellationSignal, Deadline};

use crate::global_db::RegisteredGlobalDb;
use crate::sessions::lcm::{
    LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmSessionBoundaryRequest,
    LcmSessionBoundaryResponse,
};

const LCM_EFFECT_CEILING: Duration = Duration::from_secs(30);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Daemon-owned execution boundary for retained LCM mutations.
///
/// The database authority remains lower-level storage. Host and MCP adapters
/// call this service so a disconnect or deadline can still roll back the open
/// transaction before its commit checkpoint.
#[derive(Clone)]
pub(crate) struct DaemonLcmEffectService {
    db: Arc<RegisteredGlobalDb>,
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
        mutation: impl Future<Output = Result<T, LcmError>>,
    ) -> Result<T, LcmError> {
        self.checkpoint()?;
        tokio::pin!(mutation);
        loop {
            tokio::select! {
                result = &mut mutation => return result,
                () = tokio::time::sleep(CANCELLATION_POLL_INTERVAL) => self.checkpoint()?,
            }
        }
    }
}

impl DaemonLcmEffectService {
    pub(crate) fn new(
        db: Arc<RegisteredGlobalDb>,
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
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let before_commit = self.control.clone();
        self.control
            .execute(
                self.db
                    .lcm_compress_guarded(request, move || before_commit.checkpoint()),
            )
            .await
    }

    pub(crate) async fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let before_commit = self.control.clone();
        self.control
            .execute(
                self.db
                    .lcm_session_boundary_guarded(request, move || before_commit.checkpoint()),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_db::tests::harness::RegisteredGlobalDbHarness;
    use crate::sessions::lcm::{LcmSourceRef, LcmSummarizerMode};
    use crate::sessions::{SessionMessageRecord, SessionRecord};

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
            summarizer: LcmSummarizerMode::Provided {
                summary_text: String::new(),
                route: Some("daemon_deterministic".to_string()),
            },
        }
    }

    #[tokio::test]
    async fn compression_producer_apply_read_and_rollback_stay_one_authority() {
        let harness = RegisteredGlobalDbHarness::open("lcm-compress-effect-journey").await;
        let db = Arc::clone(&harness.registered);
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
        let service_cancelled =
            DaemonLcmEffectService::new(Arc::clone(&db), None, Some(&cancellation))
                .compress(compression_request("compress-session"))
                .await;
        assert_eq!(service_cancelled.unwrap_err(), LcmError::Cancelled);

        let cancelled = db
            .lcm_compress_guarded(compression_request("compress-session"), || {
                Err(LcmError::Cancelled)
            })
            .await;
        assert_eq!(cancelled.unwrap_err(), LcmError::Cancelled);
        let rolled_back = db
            .lcm_status("cursor", Some("compress-session"))
            .await
            .unwrap();
        assert_eq!(rolled_back.raw_message_count, 8);
        assert_eq!(rolled_back.summary_node_count, 0);

        let response = DaemonLcmEffectService::new(Arc::clone(&db), None, None)
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
        let raw = db
            .lcm_load_raw_message("cursor", "message-1")
            .await
            .unwrap();
        assert_eq!(raw.store_id, source_store_id);
        assert_eq!(
            raw.content,
            "canonical historical message 1 with durable context"
        );
        assert!(!summary.summary_text.is_empty());
    }

    #[tokio::test]
    async fn boundary_apply_and_cancelled_rollback_are_observable() {
        let harness = RegisteredGlobalDbHarness::open("lcm-boundary-effect-journey").await;
        let db = Arc::clone(&harness.registered);
        for session_id in ["old-session", "new-session", "cancelled-session"] {
            assert!(db.upsert_session(&session("cursor", session_id)).await);
        }
        let service = DaemonLcmEffectService::new(Arc::clone(&db), None, None);
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
