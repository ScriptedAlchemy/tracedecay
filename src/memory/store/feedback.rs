//! Feedback, trust-history, and oplog operations for `MemoryStore`.

use crate::db::engine::params;

use crate::errors::Result;
use crate::memory::trust::apply_feedback;
use crate::memory::types::{FeedbackAction, FeedbackRequest, FeedbackResult, TrustHistoryEntry};
use crate::tracedecay::current_timestamp;

use super::{MemoryStore, db_error, db_message, feedback_action_str, parse_feedback_action};

impl MemoryStore<'_> {
    /// Public oplog hook for mutation flows that live outside this store
    /// (e.g. dashboard curation apply).
    pub async fn record_oplog(
        &self,
        op: &str,
        fact_id: Option<i64>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        self.log_oplog(op, fact_id, detail).await
    }

    pub async fn record_feedback_event(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        self.with_immediate_tx("record_feedback_event", move |store| {
            Box::pin(store.record_feedback_event_inner(request))
        })
        .await
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        const PAGE_SIZE: i64 = 512;
        let mut history = Vec::new();
        let mut cursor: Option<(i64, i64)> = None;
        loop {
            let mut rows = if let Some((created_at, event_id)) = cursor {
                self.conn
                    .query(
                        "SELECT created_at, event_id, action, old_trust, new_trust,
                                trust_delta, source, note
                         FROM memory_feedback_events
                         WHERE fact_id = ?1
                           AND (
                               created_at > ?2
                               OR (created_at = ?2 AND event_id > ?3)
                           )
                         ORDER BY created_at ASC, event_id ASC
                         LIMIT ?4",
                        params![fact_id, created_at, event_id, PAGE_SIZE],
                    )
                    .await
            } else {
                self.conn
                    .query(
                        "SELECT created_at, event_id, action, old_trust, new_trust,
                                trust_delta, source, note
                         FROM memory_feedback_events
                         WHERE fact_id = ?1
                         ORDER BY created_at ASC, event_id ASC
                         LIMIT ?2",
                        params![fact_id, PAGE_SIZE],
                    )
                    .await
            }
            .map_err(|e| db_error("fact_trust_history", e))?;
            let mut page_count = 0;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|e| db_error("fact_trust_history", e))?
            {
                let timestamp = row
                    .get::<i64>(0)
                    .map_err(|e| db_error("fact_trust_history", e))?;
                let event_id = row
                    .get::<i64>(1)
                    .map_err(|e| db_error("fact_trust_history", e))?;
                let action = parse_feedback_action(
                    &row.get::<String>(2)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                    "fact_trust_history",
                )?;
                history.push(TrustHistoryEntry {
                    timestamp,
                    action,
                    old_trust: row
                        .get::<f64>(3)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                    new_trust: row
                        .get::<f64>(4)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                    delta: row
                        .get::<f64>(5)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                    source: row
                        .get::<String>(6)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                    note: row
                        .get::<Option<String>>(7)
                        .map_err(|e| db_error("fact_trust_history", e))?,
                });
                cursor = Some((timestamp, event_id));
                page_count += 1;
            }
            if page_count < PAGE_SIZE {
                break;
            }
        }
        Ok(history)
    }

    async fn record_feedback_event_inner(
        &self,
        request: FeedbackRequest,
    ) -> Result<FeedbackResult> {
        let existing = self.get_fact(request.fact_id).await?.ok_or_else(|| {
            db_message(
                "record_feedback_event",
                format!("fact {} does not exist", request.fact_id),
            )
        })?;
        let old_trust = existing.trust_score;
        let new_trust = apply_feedback(old_trust, request.action);
        let delta = new_trust - old_trust;
        let now = current_timestamp();
        let action = feedback_action_str(request.action);
        let source = request
            .source
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "mcp".to_string());

        self.conn
            .execute(
                "UPDATE memory_facts
                 SET trust_score = ?1,
                     helpful_count = helpful_count + ?2,
                     unhelpful_count = unhelpful_count + ?3,
                     last_feedback_at = ?4,
                     updated_at = ?4
                 WHERE fact_id = ?5",
                params![
                    new_trust,
                    i64::from(request.action == FeedbackAction::Helpful),
                    i64::from(request.action == FeedbackAction::Unhelpful),
                    now,
                    request.fact_id,
                ],
            )
            .await
            .map_err(|e| db_error("record_feedback_event", e))?;

        self.conn
            .execute(
                "INSERT INTO memory_feedback_events (
                    fact_id, action, trust_delta, old_trust, new_trust,
                    created_at, source, note
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    request.fact_id,
                    action,
                    delta,
                    old_trust,
                    new_trust,
                    now,
                    source,
                    request.note,
                ],
            )
            .await
            .map_err(|e| db_error("record_feedback_event", e))?;

        let event_id = self.last_insert_rowid("record_feedback_event").await?;
        self.log_oplog(
            "feedback",
            Some(request.fact_id),
            &serde_json::json!({ "action": action, "trust_delta": delta }),
        )
        .await?;
        Ok(FeedbackResult {
            event_id,
            fact_id: request.fact_id,
            action: request.action,
            old_trust,
            new_trust,
            trust_delta: delta,
            helpful_count: existing.helpful_count
                + i64::from(request.action == FeedbackAction::Helpful),
            unhelpful_count: existing.unhelpful_count
                + i64::from(request.action == FeedbackAction::Unhelpful),
        })
    }
}
