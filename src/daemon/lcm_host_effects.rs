//! Durable admission and retained execution for host compaction effects.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracedecay_application::CancellationSignal;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::automation::config_error;
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::compatibility::projected_content_hash;
use crate::sessions::lcm::{LcmCompressionRequest, LcmError, LcmSummarizerMode};

#[derive(Clone, Debug)]
pub(crate) struct LcmHostEffectAdmission<'a> {
    pub(crate) provider: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) event_json: &'a str,
    pub(crate) compact_summary: Option<&'a str>,
    pub(crate) current_tokens: Option<i64>,
    pub(crate) context_length: Option<i64>,
    pub(crate) max_source_messages: Option<usize>,
    pub(crate) fresh_tail_count: Option<usize>,
    pub(crate) source_ready: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LcmHostEffectReceipt {
    pub(crate) event_id: String,
    pub(crate) event_digest: String,
    pub(crate) status: String,
    pub(crate) reason: Option<String>,
    pub(crate) summary_node_ids: Vec<String>,
    pub(crate) retryable: bool,
}

#[derive(Clone, Debug)]
struct PendingLcmHostEffect {
    event_id: String,
    provider: String,
    session_id: String,
    scope_kind: String,
    scope_id: String,
    compact_summary_digest: Option<String>,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
}

pub(crate) async fn enqueue_lcm_host_effect(
    database: &RegisteredGlobalDb,
    admission: LcmHostEffectAdmission<'_>,
) -> Result<LcmHostEffectReceipt> {
    if !matches!(admission.provider, "claude" | "codex" | "cursor") {
        return Err(config_error("host compaction provider is unsupported"));
    }
    let (scope, _) = database
        .session_relation_store()
        .map_err(|error| config_error(format!("host compaction scope is unavailable: {error}")))?;
    let scope_kind = match scope {
        tracedecay_global_db::session_temporal::relations::SessionRelationScope::Project {
            ..
        } => "project",
        tracedecay_global_db::session_temporal::relations::SessionRelationScope::Profile {
            ..
        } => "profile",
    };
    let event_digest = projected_content_hash(admission.event_json);
    let identity_material = format!(
        "{}\0{}\0{}\0{}\0{}",
        admission.provider,
        admission.session_id,
        scope_kind,
        scope.identity(),
        event_digest
    );
    let identity_digest = projected_content_hash(&identity_material);
    let identity_body = identity_digest
        .strip_prefix("sha256:")
        .ok_or_else(|| config_error("host compaction identity digest is invalid"))?;
    let event_id = format!("host-lcm-{identity_body}");
    let compact_summary_digest = admission.compact_summary.map(projected_content_hash);
    let max_source_messages = admission
        .max_source_messages
        .map(i64::try_from)
        .transpose()
        .map_err(|error| config_error(format!("host source bound is invalid: {error}")))?;
    let fresh_tail_count = admission
        .fresh_tail_count
        .map(i64::try_from)
        .transpose()
        .map_err(|error| config_error(format!("host fresh tail is invalid: {error}")))?;
    let created_at = now_micros()?;
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| config_error(format!("open host compaction journal: {error}")))?;
    let changed = transaction
        .execute(
            "INSERT INTO session_lcm_effect_journal (
                 effect_id, event_digest, provider, session_id, scope_kind, scope_id,
                 compact_summary_digest, current_tokens, context_length,
                 max_source_messages, fresh_tail_count, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(effect_id) DO UPDATE SET event_digest = excluded.event_digest
             WHERE session_lcm_effect_journal.event_digest = excluded.event_digest
               AND session_lcm_effect_journal.provider = excluded.provider
               AND session_lcm_effect_journal.session_id = excluded.session_id
               AND session_lcm_effect_journal.scope_kind = excluded.scope_kind
               AND session_lcm_effect_journal.scope_id = excluded.scope_id
               AND session_lcm_effect_journal.compact_summary_digest
                   IS excluded.compact_summary_digest
               AND session_lcm_effect_journal.current_tokens IS excluded.current_tokens
               AND session_lcm_effect_journal.context_length IS excluded.context_length
               AND session_lcm_effect_journal.max_source_messages
                   IS excluded.max_source_messages
               AND session_lcm_effect_journal.fresh_tail_count IS excluded.fresh_tail_count",
            params![
                event_id.as_str(),
                event_digest.as_str(),
                admission.provider,
                admission.session_id,
                scope_kind,
                scope.identity(),
                compact_summary_digest.as_deref(),
                admission.current_tokens,
                admission.context_length,
                max_source_messages,
                fresh_tail_count,
                created_at,
            ],
        )
        .await
        .map_err(|error| config_error(format!("record host compaction event: {error}")))?;
    if changed != 1 {
        return Err(config_error(
            "host compaction event identity conflicts with its durable journal",
        ));
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO session_lcm_effect_receipts (
                 effect_id, state, reason, summary_node_ids_json, completed_at
             ) VALUES (?1, ?2, NULL, '[]', NULL)",
            params![
                event_id.as_str(),
                if admission.source_ready {
                    "pending"
                } else {
                    "awaiting_source"
                }
            ],
        )
        .await
        .map_err(|error| config_error(format!("record host compaction receipt: {error}")))?;
    if admission.source_ready {
        transaction
            .execute(
                "UPDATE session_lcm_effect_receipts
                 SET state = 'pending'
                 WHERE effect_id = ?1 AND state = 'awaiting_source'",
                params![event_id.as_str()],
            )
            .await
            .map_err(|error| config_error(format!("release host compaction work: {error}")))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| config_error(format!("commit host compaction admission: {error}")))?;
    database.notify_session_relation_effect_appended();
    load_receipt(database, &event_id, &event_digest).await
}

pub(crate) async fn process_one_pending_lcm_host_effect(
    database: Arc<RegisteredGlobalDb>,
    cancellation: &CancellationSignal,
) -> Result<bool> {
    if cancellation.is_cancelled() {
        return Ok(false);
    }
    let Some(effect) = load_pending_effect(&database).await? else {
        return Ok(false);
    };
    let (mounted_scope, _) = database.session_relation_store().map_err(|error| {
        config_error(format!(
            "host compaction relation authority is unavailable: {error}"
        ))
    })?;
    let mounted_kind = match mounted_scope {
        tracedecay_global_db::session_temporal::relations::SessionRelationScope::Project {
            ..
        } => "project",
        tracedecay_global_db::session_temporal::relations::SessionRelationScope::Profile {
            ..
        } => "profile",
    };
    if effect.scope_kind != mounted_kind || effect.scope_id != mounted_scope.identity() {
        return Err(config_error(
            "host compaction work does not belong to the mounted session scope",
        ));
    }
    let mut request = compression_request(&effect);
    if effect.provider == "claude" {
        let authoritative = super::lcm_summarization::native_summary_evidence(
            &database,
            "claude",
            &effect.session_id,
        )
        .await
        .map_err(|error| config_error(format!("read Claude compaction evidence: {error}")))?;
        let Some(authoritative) = authoritative else {
            finish_effect(
                &database,
                &effect.event_id,
                "needs_authoritative_summary",
                "canonical_claude_summary_unavailable",
                &[],
            )
            .await?;
            return Ok(true);
        };
        if effect
            .compact_summary_digest
            .as_deref()
            .is_none_or(|digest| digest != projected_content_hash(&authoritative.text))
        {
            finish_effect(
                &database,
                &effect.event_id,
                "needs_authoritative_summary",
                "canonical_claude_summary_mismatch",
                &[],
            )
            .await?;
            return Ok(true);
        }
        request.summarizer = LcmSummarizerMode::Provided {
            summary_text: authoritative.text,
            route: Some(authoritative.route),
        };
    }
    let effects =
        super::lcm_effects::DaemonLcmEffectService::new(database.clone(), None, Some(cancellation));
    match effects
        .compress_host_effect(&effect.event_id, request)
        .await
    {
        Ok(response) => {
            if response.status == "needs_summary" {
                finish_effect(
                    &database,
                    &effect.event_id,
                    "needs_authoritative_summary",
                    &response.reason,
                    &[],
                )
                .await?;
            }
        }
        Err(LcmError::Cancelled | LcmError::DeadlineExceeded) => return Ok(false),
        Err(error) => {
            finish_effect(
                &database,
                &effect.event_id,
                "failed",
                &error.to_string(),
                &[],
            )
            .await?;
        }
    }
    Ok(true)
}

fn compression_request(effect: &PendingLcmHostEffect) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: effect.provider.clone(),
        session_id: effect.session_id.clone(),
        messages: Vec::new(),
        current_tokens: effect.current_tokens,
        focus_topic: Some(format!("{} context compaction", effect.provider)),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: effect.max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: effect.fresh_tail_count,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: effect.context_length,
        reserve_tokens_floor: None,
        summarizer: LcmSummarizerMode::HermesAuxiliary,
    }
}

async fn load_pending_effect(database: &RegisteredGlobalDb) -> Result<Option<PendingLcmHostEffect>> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| config_error(format!("read host compaction journal: {error}")))?;
    let mut rows = snapshot
        .query(
            "SELECT journal.effect_id, journal.provider, journal.session_id,
                    journal.scope_kind, journal.scope_id,
                    journal.compact_summary_digest, journal.current_tokens,
                    journal.context_length, journal.max_source_messages,
                    journal.fresh_tail_count
             FROM session_lcm_effect_journal AS journal
             JOIN session_lcm_effect_receipts AS receipt
               ON receipt.effect_id = journal.effect_id
             WHERE receipt.state = 'pending'
             ORDER BY journal.created_at, journal.effect_id
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| config_error(format!("query host compaction journal: {error}")))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| config_error(format!("read host compaction work item: {error}")))?
    else {
        return Ok(None);
    };
    let max_source_messages = stored_usize(
        row.get(8)
            .map_err(|error| config_error(format!("read host source bound: {error}")))?,
        "host source bound",
    )?;
    let fresh_tail_count = stored_usize(
        row.get(9)
            .map_err(|error| config_error(format!("read host fresh tail: {error}")))?,
        "host fresh tail",
    )?;
    Ok(Some(PendingLcmHostEffect {
        event_id: row
            .get(0)
            .map_err(|error| config_error(format!("read host compaction id: {error}")))?,
        provider: row
            .get(1)
            .map_err(|error| config_error(format!("read host compaction provider: {error}")))?,
        session_id: row
            .get(2)
            .map_err(|error| config_error(format!("read host compaction session: {error}")))?,
        scope_kind: row
            .get(3)
            .map_err(|error| config_error(format!("read host compaction scope: {error}")))?,
        scope_id: row
            .get(4)
            .map_err(|error| config_error(format!("read host compaction owner: {error}")))?,
        compact_summary_digest: row
            .get(5)
            .map_err(|error| config_error(format!("read host summary digest: {error}")))?,
        current_tokens: row
            .get(6)
            .map_err(|error| config_error(format!("read host token count: {error}")))?,
        context_length: row
            .get(7)
            .map_err(|error| config_error(format!("read host context length: {error}")))?,
        max_source_messages,
        fresh_tail_count,
    }))
}

fn stored_usize(value: Option<i64>, field: &str) -> Result<Option<usize>> {
    value
        .map(usize::try_from)
        .transpose()
        .map_err(|error| config_error(format!("{field} is invalid: {error}")))
}

async fn finish_effect(
    database: &RegisteredGlobalDb,
    event_id: &str,
    state: &str,
    reason: &str,
    summary_node_ids: &[String],
) -> Result<()> {
    let summary_node_ids_json = serde_json::to_string(summary_node_ids)
        .map_err(|error| config_error(format!("encode host compaction receipt: {error}")))?;
    let changed = database
        .writer_connection()
        .map_err(|error| config_error(format!("open host compaction receipt: {error}")))?
        .execute(
            "UPDATE session_lcm_effect_receipts
             SET state = ?2, reason = ?3, summary_node_ids_json = ?4, completed_at = ?5
             WHERE effect_id = ?1 AND state = 'pending'",
            params![
                event_id,
                state,
                reason,
                summary_node_ids_json,
                now_micros()?
            ],
        )
        .await
        .map_err(|error| config_error(format!("complete host compaction receipt: {error}")))?;
    if changed != 1 {
        return Err(config_error(
            "host compaction receipt changed before terminal acknowledgement",
        ));
    }
    Ok(())
}

async fn load_receipt(
    database: &RegisteredGlobalDb,
    event_id: &str,
    event_digest: &str,
) -> Result<LcmHostEffectReceipt> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| config_error(format!("read host compaction receipt: {error}")))?;
    let mut rows = snapshot
        .query(
            "SELECT state, reason, summary_node_ids_json
             FROM session_lcm_effect_receipts WHERE effect_id = ?1",
            params![event_id],
        )
        .await
        .map_err(|error| config_error(format!("query host compaction receipt: {error}")))?;
    let row = rows
        .next()
        .await
        .map_err(|error| config_error(format!("read host compaction receipt row: {error}")))?
        .ok_or_else(|| config_error("host compaction receipt is unavailable"))?;
    let status: String = row
        .get(0)
        .map_err(|error| config_error(format!("read host compaction status: {error}")))?;
    let reason = row
        .get(1)
        .map_err(|error| config_error(format!("read host compaction reason: {error}")))?;
    let encoded_ids: String = row
        .get(2)
        .map_err(|error| config_error(format!("read host compaction outputs: {error}")))?;
    let summary_node_ids = serde_json::from_str(&encoded_ids)
        .map_err(|error| config_error(format!("decode host compaction outputs: {error}")))?;
    Ok(LcmHostEffectReceipt {
        event_id: event_id.to_owned(),
        event_digest: event_digest.to_owned(),
        retryable: matches!(
            status.as_str(),
            "awaiting_source" | "pending" | "needs_authoritative_summary"
        ),
        status,
        reason,
        summary_node_ids,
    })
}

fn now_micros() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| config_error(format!("host compaction clock failed: {error}")))?;
    i64::try_from(duration.as_micros())
        .map_err(|error| config_error(format!("host compaction timestamp overflow: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_provider_requests_never_select_a_deterministic_fallback() {
        for provider in ["codex", "cursor"] {
            let request = compression_request(&PendingLcmHostEffect {
                event_id: "event".to_owned(),
                provider: provider.to_owned(),
                session_id: "session".to_owned(),
                scope_kind: "project".to_owned(),
                scope_id: "project".to_owned(),
                compact_summary_digest: None,
                current_tokens: None,
                context_length: None,
                max_source_messages: None,
                fresh_tail_count: None,
            });
            assert_eq!(request.summarizer, LcmSummarizerMode::HermesAuxiliary);
        }
    }
}
