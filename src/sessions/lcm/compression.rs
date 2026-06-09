use std::path::Path;

use libsql::{params, Connection, Value as DbValue};
use serde_json::{json, Value};

use crate::sessions::SessionMessageRecord;

use super::{
    dag, raw, security, LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmLifecycleState,
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmPreflightRequest, LcmPreflightResponse,
    LcmRawMessage, LcmSourceRef, LcmStorageKind, LcmSummarizerMode, LcmSummaryNode,
    LcmSummaryNodeDraft, LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange,
};

const DEFAULT_FRESH_TAIL_COUNT: usize = 2;
const DEFAULT_SUMMARY_FAN_IN: usize = 4;
const MAX_FORCED_CATCHUP_PASSES: usize = 4;
const MIN_SUMMARY_RESCUE_SOURCE_TOKENS: i64 = 8;

struct IngestedActiveMessages {
    replay_messages: Vec<Value>,
    changed_replay: bool,
}

pub(crate) async fn update_lifecycle(
    conn: &Connection,
    update: LcmLifecycleUpdate,
) -> Result<LcmLifecycleState, LcmError> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    if let Err(err) = upsert_lifecycle_state(conn, &update).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }
    if let Err(err) = replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await
    {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err);
    }

    match conn.execute("COMMIT", ()).await {
        Ok(_) => lifecycle_state(conn, &update.provider, &update.conversation_id).await,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(LcmError::Db(err.to_string()))
        }
    }
}

pub(crate) async fn lifecycle_state(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
) -> Result<LcmLifecycleState, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, conversation_id, current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND conversation_id = ?2",
            params![provider, conversation_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("lifecycle state not found".to_string()))?;
    let maintenance_debt = load_maintenance_debt(conn, provider, conversation_id).await?;
    Ok(LcmLifecycleState {
        provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        conversation_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        current_session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        current_frontier_store_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_session_id: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_frontier_store_id: row
            .get(5)
            .map_err(|err| LcmError::Db(err.to_string()))?,
        maintenance_debt,
    })
}

pub(crate) async fn preflight(
    conn: &Connection,
    storage_root: &Path,
    request: LcmPreflightRequest,
) -> Result<LcmPreflightResponse, LcmError> {
    if let Some(reason) = filtered_session_reason(
        &request.session_id,
        &request.ignore_session_patterns,
        &request.stateless_session_patterns,
    ) {
        return Ok(LcmPreflightResponse {
            status: "ok".to_string(),
            should_compress: false,
            reason: reason.to_string(),
            replay_messages: request.messages,
        });
    }

    ensure_session(conn, &request.provider, &request.session_id).await?;
    let ingested = ingest_active_messages(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
        &request.ignore_message_patterns,
    )
    .await?;
    let reason = if ingested.changed_replay {
        "ingest_protection_changed_replay"
    } else {
        "no_compression_needed"
    };
    Ok(LcmPreflightResponse {
        status: "ok".to_string(),
        should_compress: ingested.changed_replay,
        reason: reason.to_string(),
        replay_messages: ingested.replay_messages,
    })
}

pub(crate) async fn compress(
    conn: &Connection,
    storage_root: &Path,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    if let Some(reason) = filtered_session_reason(
        &request.session_id,
        &request.ignore_session_patterns,
        &request.stateless_session_patterns,
    ) {
        let frontier = lifecycle_state_or_default(
            conn,
            &request.provider,
            &request.session_id,
            &request.session_id,
        )
        .await?;
        return Ok(compression_response(
            "ok",
            reason,
            Vec::new(),
            request.messages,
            frontier,
            None,
            request.max_assembly_tokens,
        ));
    }

    ensure_session(conn, &request.provider, &request.session_id).await?;
    let ingested = ingest_active_messages(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
        &request.ignore_message_patterns,
    )
    .await?;

    if matches!(request.summarizer, LcmSummarizerMode::Noop) {
        let frontier = lifecycle_state_or_default(
            conn,
            &request.provider,
            &request.session_id,
            &request.session_id,
        )
        .await?;
        return Ok(compression_response(
            "ok",
            "noop_summarizer",
            Vec::new(),
            ingested.replay_messages,
            frontier,
            None,
            request.max_assembly_tokens,
        ));
    }

    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let response = match compress_in_transaction(conn, request).await {
        Ok(response) => response,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
    };

    match conn.execute("COMMIT", ()).await {
        Ok(_) => Ok(response),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(LcmError::Db(err.to_string()))
        }
    }
}

async fn compress_in_transaction(
    conn: &Connection,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    let conversation_id = request.session_id.clone();
    let existing_frontier = lifecycle_state_or_default(
        conn,
        &request.provider,
        &conversation_id,
        &request.session_id,
    )
    .await?;
    if let Some(expected_frontier) = request.expected_current_frontier_store_id {
        if existing_frontier.current_frontier_store_id.unwrap_or(0) != expected_frontier {
            let raw_messages =
                load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
            let window =
                compression_window(&raw_messages, existing_frontier.current_frontier_store_id);
            let replay_messages =
                replay_without_summary(&window.pinned_anchors, &window.fresh_tail);
            return Ok(compression_response(
                "ok",
                "frontier_changed",
                Vec::new(),
                replay_messages,
                existing_frontier,
                None,
                request.max_assembly_tokens,
            ));
        }
    }

    let raw_messages =
        load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
    let window = compression_window(&raw_messages, existing_frontier.current_frontier_store_id);

    if window.backlog.is_empty() {
        if should_force_overflow_recovery(&request) {
            let replay_messages =
                replay_without_summary(&window.pinned_anchors, &window.fresh_tail);
            return Ok(compression_response(
                "best_effort",
                "irreducible_overflow_no_backlog",
                Vec::new(),
                replay_messages,
                existing_frontier,
                None,
                request.max_assembly_tokens,
            ));
        }
        if let Some(response) =
            condense_summary_nodes_if_ready(conn, &request, &conversation_id, &existing_frontier)
                .await?
        {
            return Ok(response);
        }
        let replay_messages = replay_without_summary(&window.pinned_anchors, &window.fresh_tail);
        return Ok(compression_response(
            "ok",
            "no_backlog_to_compress",
            Vec::new(),
            replay_messages,
            existing_frontier,
            None,
            request.max_assembly_tokens,
        ));
    }

    let plan = compression_plan(&request, &window);

    if matches!(request.summarizer, LcmSummarizerMode::HermesAuxiliary) {
        let replay_messages = replay_without_summary(&window.pinned_anchors, &window.fresh_tail);
        return Ok(compression_response(
            "needs_summary",
            "hermes_auxiliary_not_available",
            Vec::new(),
            replay_messages,
            existing_frontier,
            Some(summary_request_for_backlog(
                &request.provider,
                &request.session_id,
                request.focus_topic,
                &plan.selected_backlog,
            )),
            request.max_assembly_tokens,
        ));
    }

    let (summary_text, route) = match request.summarizer {
        LcmSummarizerMode::Fake { summary_text } => (summary_text, None),
        LcmSummarizerMode::Provided {
            summary_text,
            route,
        } => (summary_text, route),
        LcmSummarizerMode::Noop | LcmSummarizerMode::HermesAuxiliary => unreachable!(),
    };
    let mut remaining_backlog = window.backlog.clone();
    let pass_limit = if plan.forced_overflow_recovery {
        MAX_FORCED_CATCHUP_PASSES
    } else {
        1
    };
    let mut created_summaries = Vec::new();
    let mut fallback_used = false;
    let mut new_frontier = existing_frontier.current_frontier_store_id;

    while !remaining_backlog.is_empty() && created_summaries.len() < pass_limit {
        let selected_len = bounded_leaf_chunk_len(
            &remaining_backlog,
            request.leaf_chunk_tokens,
            request.max_source_messages,
        );
        let selected_backlog = remaining_backlog[..selected_len].to_vec();
        let source_tokens = source_token_count(&selected_backlog);
        let (pass_summary_text, pass_fallback_used) =
            rescuing_summary_text(summary_text.clone(), &selected_backlog, source_tokens);
        fallback_used |= pass_fallback_used;

        let first_store_id = selected_backlog.first().map(|message| message.store_id);
        let summary = dag::insert_summary_node_in_transaction(
            conn,
            summary_draft(
                &request.provider,
                &conversation_id,
                &request.session_id,
                &pass_summary_text,
                route.clone(),
                &selected_backlog,
            ),
        )
        .await?;
        new_frontier = selected_backlog
            .last()
            .map(|message| message.store_id)
            .or(new_frontier);
        created_summaries.push(CreatedSummary {
            summary,
            first_store_id,
        });
        remaining_backlog = remaining_backlog[selected_len..].to_vec();

        if !plan.forced_overflow_recovery {
            break;
        }
    }

    let maintenance_debt = debt_for_deferred_backlog(&remaining_backlog);
    let update = LcmLifecycleUpdate {
        provider: request.provider.clone(),
        conversation_id,
        current_session_id: request.session_id.clone(),
        current_frontier_store_id: new_frontier,
        last_finalized_session_id: existing_frontier.last_finalized_session_id.clone(),
        last_finalized_frontier_store_id: existing_frontier.last_finalized_frontier_store_id,
        maintenance_debt,
    };
    upsert_lifecycle_state(conn, &update).await?;
    replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await?;
    let frontier = lifecycle_state(conn, &update.provider, &update.conversation_id).await?;
    let replay_messages = replay_with_summaries(
        &window.pinned_anchors,
        &created_summaries,
        &remaining_backlog,
        &window.fresh_tail,
    );
    let mut status = "ok";
    let mut reason = if plan.forced_overflow_recovery {
        "forced_overflow_recovery"
    } else if fallback_used {
        "compressed_backlog_with_fallback_summary"
    } else {
        "compressed_backlog"
    };
    let replay_token_estimate = replay_token_estimate(&replay_messages);
    if plan.forced_overflow_recovery
        && replay_exceeds_budget(replay_token_estimate, request.max_assembly_tokens)
    {
        status = "best_effort";
        reason = "forced_overflow_recovery_replay_over_budget";
    }
    let summary_nodes = created_summaries
        .into_iter()
        .map(|created| created.summary)
        .collect::<Vec<_>>();

    Ok(compression_response(
        status,
        reason,
        summary_nodes,
        replay_messages,
        frontier,
        None,
        request.max_assembly_tokens,
    ))
}

pub(crate) async fn maintenance_debt_count(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_maintenance_debt d
             JOIN lcm_lifecycle_state s
               ON s.provider = d.provider AND s.conversation_id = d.conversation_id
             WHERE d.provider = ?1 AND (?2 IS NULL OR s.current_session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("maintenance debt count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn upsert_lifecycle_state(
    conn: &Connection,
    update: &LcmLifecycleUpdate,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_lifecycle_state (
            provider, conversation_id, current_session_id, last_finalized_session_id,
            current_frontier_store_id, last_finalized_frontier_store_id, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
         ON CONFLICT(provider, conversation_id) DO UPDATE SET
            current_session_id = excluded.current_session_id,
            last_finalized_session_id = excluded.last_finalized_session_id,
            current_frontier_store_id = excluded.current_frontier_store_id,
            last_finalized_frontier_store_id = excluded.last_finalized_frontier_store_id,
            updated_at = unixepoch()",
        params![
            update.provider.as_str(),
            update.conversation_id.as_str(),
            update.current_session_id.as_str(),
            opt_text(update.last_finalized_session_id.as_deref()),
            opt_i64(update.current_frontier_store_id),
            opt_i64(update.last_finalized_frontier_store_id),
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn replace_maintenance_debt(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
    debts: &[LcmMaintenanceDebt],
) -> Result<(), LcmError> {
    conn.execute(
        "DELETE FROM lcm_maintenance_debt WHERE provider = ?1 AND conversation_id = ?2",
        params![provider, conversation_id],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;

    for debt in debts {
        let (debt_id, debt_kind, from_store_id, to_store_id) = debt_to_db(debt);
        conn.execute(
            "INSERT INTO lcm_maintenance_debt (
                provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                provider,
                conversation_id,
                debt_id.as_str(),
                debt_kind,
                opt_i64(from_store_id),
                opt_i64(to_store_id),
            ],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    }
    Ok(())
}

async fn load_maintenance_debt(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
) -> Result<Vec<LcmMaintenanceDebt>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT debt_kind, from_store_id, to_store_id
             FROM lcm_maintenance_debt
             WHERE provider = ?1 AND conversation_id = ?2
             ORDER BY created_at, debt_id",
            params![provider, conversation_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut debts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let debt_kind: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        debts.push(debt_from_db(
            &debt_kind,
            row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        )?);
    }
    Ok(debts)
}

async fn lifecycle_state_or_default(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
    session_id: &str,
) -> Result<LcmLifecycleState, LcmError> {
    match lifecycle_state(conn, provider, conversation_id).await {
        Ok(state) => Ok(state),
        Err(LcmError::Db(message)) if message == "lifecycle state not found" => {
            Ok(LcmLifecycleState {
                provider: provider.to_string(),
                conversation_id: conversation_id.to_string(),
                current_session_id: session_id.to_string(),
                current_frontier_store_id: None,
                last_finalized_session_id: None,
                last_finalized_frontier_store_id: None,
                maintenance_debt: Vec::new(),
            })
        }
        Err(err) => Err(err),
    }
}

struct CompressionWindow {
    pinned_anchors: Vec<LcmRawMessage>,
    backlog: Vec<LcmRawMessage>,
    fresh_tail: Vec<LcmRawMessage>,
}

struct CompressionPlan {
    selected_backlog: Vec<LcmRawMessage>,
    forced_overflow_recovery: bool,
}

struct CreatedSummary {
    summary: LcmSummaryNode,
    first_store_id: Option<i64>,
}

fn compression_window(
    raw_messages: &[LcmRawMessage],
    current_frontier_store_id: Option<i64>,
) -> CompressionWindow {
    let frontier_store_id = current_frontier_store_id.unwrap_or(0);
    let unsummarized = raw_messages
        .iter()
        .filter(|message| message.store_id > frontier_store_id)
        .cloned()
        .collect::<Vec<_>>();
    let backlog_len = unsummarized.len().saturating_sub(DEFAULT_FRESH_TAIL_COUNT);
    let (older_unsummarized, fresh_tail) = unsummarized.split_at(backlog_len);
    let fresh_tail_start_store_id = fresh_tail
        .first()
        .map(|message| message.store_id)
        .unwrap_or(i64::MAX);
    let pinned_anchors = raw_messages
        .iter()
        .filter(|message| {
            message.store_id < fresh_tail_start_store_id && is_policy_anchor_role(&message.role)
        })
        .cloned()
        .collect::<Vec<_>>();
    let backlog = older_unsummarized
        .iter()
        .filter(|message| !is_policy_anchor_role(&message.role))
        .cloned()
        .collect::<Vec<_>>();

    CompressionWindow {
        pinned_anchors,
        backlog,
        fresh_tail: fresh_tail.to_vec(),
    }
}

fn compression_plan(
    request: &LcmCompressionRequest,
    window: &CompressionWindow,
) -> CompressionPlan {
    let forced_overflow_recovery = should_force_overflow_recovery(request);
    let selected_len = bounded_leaf_chunk_len(
        &window.backlog,
        request.leaf_chunk_tokens,
        request.max_source_messages,
    );
    CompressionPlan {
        selected_backlog: window.backlog[..selected_len].to_vec(),
        forced_overflow_recovery,
    }
}

fn should_force_overflow_recovery(request: &LcmCompressionRequest) -> bool {
    match (request.current_tokens, request.max_assembly_tokens) {
        (Some(current_tokens), Some(max_assembly_tokens)) => current_tokens >= max_assembly_tokens,
        _ => false,
    }
}

fn filtered_session_reason(
    session_id: &str,
    ignore_session_patterns: &[String],
    stateless_session_patterns: &[String],
) -> Option<&'static str> {
    if security::matches_any_pattern(ignore_session_patterns, session_id) {
        Some("ignored_session")
    } else if security::matches_any_pattern(stateless_session_patterns, session_id) {
        Some("stateless_session")
    } else {
        None
    }
}

fn bounded_leaf_chunk_len(
    backlog: &[LcmRawMessage],
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
) -> usize {
    if backlog.is_empty() {
        return 0;
    }
    if leaf_chunk_tokens.is_none() && max_source_messages.is_none() {
        return backlog.len();
    }

    let max_messages = max_source_messages
        .filter(|limit| *limit > 0)
        .unwrap_or(backlog.len())
        .min(backlog.len());
    let token_limit = leaf_chunk_tokens.filter(|limit| *limit > 0);
    let mut selected_len = 0;
    let mut selected_tokens = 0;
    for message in backlog.iter().take(max_messages) {
        let message_tokens = estimate_tokens(&message.content);
        if selected_len > 0 {
            if let Some(token_limit) = token_limit {
                if selected_tokens + message_tokens > token_limit {
                    break;
                }
            }
        }
        selected_tokens += message_tokens;
        selected_len += 1;
    }
    selected_len.max(1)
}

fn is_policy_anchor_role(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

fn replay_without_summary(
    pinned_anchors: &[LcmRawMessage],
    fresh_tail: &[LcmRawMessage],
) -> Vec<Value> {
    let mut replay_messages = Vec::with_capacity(pinned_anchors.len() + fresh_tail.len());
    replay_messages.extend(pinned_anchors.iter().map(raw_replay_message));
    replay_messages.extend(fresh_tail.iter().map(raw_replay_message));
    replay_messages
}

fn replay_with_summaries(
    pinned_anchors: &[LcmRawMessage],
    summaries: &[CreatedSummary],
    deferred_backlog: &[LcmRawMessage],
    fresh_tail: &[LcmRawMessage],
) -> Vec<Value> {
    let mut replay_items = Vec::with_capacity(
        pinned_anchors.len() + summaries.len() + deferred_backlog.len() + fresh_tail.len(),
    );
    replay_items.extend(
        pinned_anchors
            .iter()
            .map(|message| (message.store_id, 1, raw_replay_message(message))),
    );
    replay_items.extend(summaries.iter().map(|created| {
        (
            created.first_store_id.unwrap_or(i64::MAX),
            0,
            summary_replay_message(&created.summary),
        )
    }));
    replay_items.extend(
        deferred_backlog
            .iter()
            .map(|message| (message.store_id, 1, raw_replay_message(message))),
    );
    replay_items.extend(
        fresh_tail
            .iter()
            .map(|message| (message.store_id, 1, raw_replay_message(message))),
    );
    replay_items.sort_by_key(|(store_id, priority, _)| (*store_id, *priority));
    replay_items
        .into_iter()
        .map(|(_, _, message)| message)
        .collect()
}

fn compression_response(
    status: &str,
    reason: &str,
    summary_nodes: Vec<LcmSummaryNode>,
    replay_messages: Vec<Value>,
    frontier: LcmLifecycleState,
    summary_request: Option<LcmSummaryRequest>,
    max_assembly_tokens: Option<i64>,
) -> LcmCompressionResponse {
    let replay_token_estimate = replay_token_estimate(&replay_messages);
    LcmCompressionResponse {
        status: status.to_string(),
        reason: reason.to_string(),
        summary_nodes_created: summary_nodes.len(),
        summary_nodes,
        replay_messages,
        replay_token_estimate,
        replay_over_budget: replay_exceeds_budget(replay_token_estimate, max_assembly_tokens),
        frontier,
        summary_request,
    }
}

fn replay_token_estimate(messages: &[Value]) -> i64 {
    messages
        .iter()
        .map(|message| estimate_tokens(&message_content(message)))
        .sum()
}

fn replay_exceeds_budget(replay_token_estimate: i64, max_assembly_tokens: Option<i64>) -> bool {
    max_assembly_tokens.is_some_and(|max_tokens| replay_token_estimate > max_tokens)
}

fn summary_draft(
    provider: &str,
    conversation_id: &str,
    session_id: &str,
    summary_text: &str,
    route: Option<String>,
    backlog: &[LcmRawMessage],
) -> LcmSummaryNodeDraft {
    let source_refs = backlog
        .iter()
        .map(|message| LcmSourceRef::RawMessage {
            store_id: message.store_id,
        })
        .collect::<Vec<_>>();
    let source_token_count = source_token_count(backlog);
    let source_time_start = backlog.iter().filter_map(|message| message.timestamp).min();
    let source_time_end = backlog.iter().filter_map(|message| message.timestamp).max();
    let mut metadata = json!({ "pre_compaction_extraction": "noop_contract" });
    if let Some(route) = route {
        metadata["summary_route"] = Value::String(route);
    }
    let metadata_json = Some(metadata.to_string());

    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: conversation_id.to_string(),
        session_id: session_id.to_string(),
        depth: 0,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count,
        summary_token_count: estimate_tokens(summary_text),
        source_time_start,
        source_time_end,
        expand_hint: Some(format!("{} raw messages", backlog.len())),
        metadata_json,
    }
}

fn condensation_draft(
    provider: &str,
    conversation_id: &str,
    session_id: &str,
    summary_text: &str,
    children: &[LcmSummaryNode],
) -> LcmSummaryNodeDraft {
    let source_refs = children
        .iter()
        .map(|node| LcmSourceRef::SummaryNode {
            node_id: node.node_id.clone(),
        })
        .collect::<Vec<_>>();
    let source_token_count = children
        .iter()
        .map(|node| node.summary_token_count)
        .sum::<i64>();
    let source_time_start = children
        .iter()
        .filter_map(|node| node.source_time_start)
        .min();
    let source_time_end = children
        .iter()
        .filter_map(|node| node.source_time_end)
        .max();
    let depth = children.iter().map(|node| node.depth).max().unwrap_or(0) + 1;

    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: conversation_id.to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count,
        summary_token_count: estimate_tokens(summary_text),
        source_time_start,
        source_time_end,
        expand_hint: Some(format!("{} summary nodes", children.len())),
        metadata_json: Some(json!({ "pre_compaction_extraction": "noop_contract" }).to_string()),
    }
}

async fn condense_summary_nodes_if_ready(
    conn: &Connection,
    request: &LcmCompressionRequest,
    conversation_id: &str,
    existing_frontier: &LcmLifecycleState,
) -> Result<Option<LcmCompressionResponse>, LcmError> {
    let fan_in = request
        .summary_fan_in
        .filter(|fan_in| *fan_in > 1)
        .unwrap_or(DEFAULT_SUMMARY_FAN_IN);
    let children =
        load_condensation_candidates(conn, &request.provider, &request.session_id, fan_in).await?;
    if children.len() < fan_in || matches!(request.summarizer, LcmSummarizerMode::HermesAuxiliary) {
        return Ok(None);
    }

    let summary_text = match &request.summarizer {
        LcmSummarizerMode::Fake { summary_text } => summary_text.clone(),
        LcmSummarizerMode::Provided { summary_text, .. } => summary_text.clone(),
        LcmSummarizerMode::Noop | LcmSummarizerMode::HermesAuxiliary => unreachable!(),
    };
    let source_tokens = children
        .iter()
        .map(|node| node.summary_token_count)
        .sum::<i64>();
    let source_texts = children
        .iter()
        .map(|node| node.summary_text.clone())
        .collect::<Vec<_>>();
    let (summary_text, fallback_used) =
        rescuing_summary_text_from_texts(summary_text, source_texts, source_tokens);
    let summary = dag::insert_summary_node_in_transaction(
        conn,
        condensation_draft(
            &request.provider,
            conversation_id,
            &request.session_id,
            &summary_text,
            &children,
        ),
    )
    .await?;
    let update = LcmLifecycleUpdate {
        provider: request.provider.clone(),
        conversation_id: conversation_id.to_string(),
        current_session_id: request.session_id.clone(),
        current_frontier_store_id: existing_frontier.current_frontier_store_id,
        last_finalized_session_id: existing_frontier.last_finalized_session_id.clone(),
        last_finalized_frontier_store_id: existing_frontier.last_finalized_frontier_store_id,
        maintenance_debt: existing_frontier.maintenance_debt.clone(),
    };
    upsert_lifecycle_state(conn, &update).await?;
    replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await?;
    let frontier = lifecycle_state(conn, &update.provider, &update.conversation_id).await?;
    let reason = if fallback_used {
        "condensed_summary_nodes_with_fallback_summary"
    } else {
        "condensed_summary_nodes"
    };
    Ok(Some(compression_response(
        "ok",
        reason,
        vec![summary],
        Vec::new(),
        frontier,
        None,
        request.max_assembly_tokens,
    )))
}

async fn load_condensation_candidates(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    fan_in: usize,
) -> Result<Vec<LcmSummaryNode>, LcmError> {
    let mut rows = conn
        .query(
            "WITH source_order AS (
               SELECT lcm_summary_sources.node_id, MIN(CAST(source_id AS INTEGER)) AS first_source_id
               FROM lcm_summary_sources
               WHERE source_kind = 'raw_message'
               GROUP BY lcm_summary_sources.node_id
             ),
             unparented AS (
               SELECT n.node_id, n.provider, n.conversation_id, n.session_id, n.depth, n.summary_text,
                      n.summary_hash, n.summary_token_count, n.source_token_count, n.source_time_start,
                      n.source_time_end, n.expand_hint, n.metadata_json, n.created_at,
                      source_order.first_source_id
               FROM lcm_summary_nodes n
               LEFT JOIN source_order ON source_order.node_id = n.node_id
               WHERE n.provider = ?1 AND n.session_id = ?2
                 AND NOT EXISTS (
                   SELECT 1
                   FROM lcm_summary_sources s
                   WHERE s.source_kind = 'summary_node'
                     AND s.source_id = n.node_id
                 )
             ),
             eligible_depth AS (
               SELECT depth
               FROM unparented
               GROUP BY depth
               HAVING COUNT(*) >= ?3
               ORDER BY depth
               LIMIT 1
             )
             SELECT node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count, source_time_start,
                    source_time_end, expand_hint, metadata_json, created_at
             FROM unparented
             WHERE depth = (SELECT depth FROM eligible_depth)
             ORDER BY source_time_start IS NULL, source_time_start,
                      first_source_id IS NULL, first_source_id,
                      created_at, node_id
             LIMIT ?3",
            params![provider, session_id, fan_in as i64],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        nodes.push(LcmSummaryNode {
            node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            conversation_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
            session_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
            depth: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
            summary_text: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
            summary_hash: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
            summary_token_count: row.get(7).map_err(|err| LcmError::Db(err.to_string()))?,
            source_token_count: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
            source_time_start: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
            source_time_end: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
            expand_hint: row.get(11).map_err(|err| LcmError::Db(err.to_string()))?,
            metadata_json: row.get(12).map_err(|err| LcmError::Db(err.to_string()))?,
            created_at: row.get(13).map_err(|err| LcmError::Db(err.to_string()))?,
            source_refs: Vec::new(),
        });
    }
    Ok(nodes)
}

fn summary_request_for_backlog(
    provider: &str,
    session_id: &str,
    focus_topic: Option<String>,
    backlog: &[LcmRawMessage],
) -> LcmSummaryRequest {
    let first_store_id = backlog.first().map(|message| message.store_id).unwrap_or(0);
    let last_store_id = backlog.last().map(|message| message.store_id).unwrap_or(0);
    let focus = focus_topic.as_deref().unwrap_or("the conversation so far");
    let prompt = format!(
        "Summarize LCM raw messages for provider '{provider}', session '{session_id}', \
         store_id range {first_store_id}..={last_store_id}. Focus on {focus}. \
         Preserve durable instructions, decisions, open tasks, and facts needed to continue."
    );

    LcmSummaryRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        focus_topic,
        prompt,
        source_range: LcmSummarySourceRange {
            from_store_id: first_store_id,
            to_store_id: last_store_id,
        },
        source_messages: backlog
            .iter()
            .map(|message| LcmSummarySourceMessage {
                store_id: message.store_id,
                role: message.role.clone(),
                content: message.content.clone(),
            })
            .collect(),
    }
}

async fn ingest_active_messages(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    messages: &[Value],
    ignore_message_patterns: &[String],
) -> Result<IngestedActiveMessages, LcmError> {
    let mut replay_messages = Vec::with_capacity(messages.len());
    let mut changed_replay = false;
    let mut next_available_ordinal = next_ordinal(conn, provider, session_id).await?;

    for (idx, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let content = message_content(message);
        if security::ignore_message_reason(&role, &content, ignore_message_patterns).is_some() {
            let mut replay = message.clone();
            replay["role"] = Value::String(role);
            replay["content"] = Value::String(content);
            replay_messages.push(replay);
            continue;
        }
        let message_id = message
            .get("id")
            .or_else(|| message.get("message_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                deterministic_message_id(provider, session_id, idx, &role, &content)
            });
        let ordinal = match existing_message_ordinal(conn, provider, &message_id).await? {
            Some(existing_ordinal) => existing_ordinal,
            None => {
                next_available_ordinal += 1;
                next_available_ordinal
            }
        };
        let kind = message
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some("message".to_string()));
        let record = SessionMessageRecord {
            provider: provider.to_string(),
            message_id: message_id.clone(),
            session_id: session_id.to_string(),
            role,
            timestamp: message.get("timestamp").and_then(Value::as_i64),
            ordinal,
            text: content.clone(),
            kind,
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: serde_json::to_string(message).ok(),
        };
        let upsert = raw::upsert_raw_message_with_payload(conn, storage_root, &record).await?;
        let raw = super::schema::load_raw_message(conn, provider, &message_id)
            .await
            .ok_or_else(|| LcmError::Db("active message did not persist".to_string()))?;
        let replay_content = if raw.storage_kind == LcmStorageKind::External {
            changed_replay = true;
            upsert.projection_text
        } else {
            content
        };
        let mut replay = message.clone();
        replay["role"] = Value::String(record.role.clone());
        replay["content"] = Value::String(replay_content);
        replay_messages.push(replay);
    }

    Ok(IngestedActiveMessages {
        replay_messages,
        changed_replay,
    })
}

async fn ensure_session(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT OR IGNORE INTO sessions (
            provider, session_id, project_key, project_path, title, started_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())",
        params![
            provider,
            session_id,
            "lcm-active-context",
            "lcm-active-context",
            "LCM active context",
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn existing_message_ordinal(
    conn: &Connection,
    provider: &str,
    message_id: &str,
) -> Result<Option<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT ordinal
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            params![provider, message_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    rows.next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .map(|row| row.get(0).map_err(|err| LcmError::Db(err.to_string())))
        .transpose()
}

async fn next_ordinal(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(ordinal), 0)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("ordinal query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn load_raw_messages_for_session(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessage>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY store_id",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut messages = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        messages.push(raw_message_from_row(&row)?);
    }
    Ok(messages)
}

fn raw_message_from_row(row: &libsql::Row) -> Result<LcmRawMessage, LcmError> {
    let storage_kind_text: String = row.get(9).map_err(|err| LcmError::Db(err.to_string()))?;
    let content: Option<String> = row.get(7).map_err(|err| LcmError::Db(err.to_string()))?;
    let snippet_text: String = row.get(11).map_err(|err| LcmError::Db(err.to_string()))?;
    let storage_kind = LcmStorageKind::from_db(&storage_kind_text)
        .ok_or_else(|| LcmError::Db(format!("invalid storage_kind: {storage_kind_text}")))?;
    let content = match storage_kind {
        LcmStorageKind::Inline => content.unwrap_or_default(),
        LcmStorageKind::External => content.unwrap_or(snippet_text),
    };
    Ok(LcmRawMessage {
        provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        message_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        store_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        role: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        ordinal: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        timestamp: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
        content,
        content_hash: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        storage_kind,
        payload_ref: row.get(10).map_err(|err| LcmError::Db(err.to_string()))?,
        legacy_source: row.get::<i64>(12).unwrap_or(0) != 0,
        legacy_truncated: row.get::<i64>(13).unwrap_or(0) != 0,
        metadata_json: row.get(14).map_err(|err| LcmError::Db(err.to_string()))?,
    })
}

fn message_content(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(items) = content.as_array() {
        let texts = items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !texts.is_empty() {
            return texts.join("\n\n");
        }
    }
    content.to_string()
}

fn deterministic_message_id(
    provider: &str,
    session_id: &str,
    idx: usize,
    role: &str,
    content: &str,
) -> String {
    format!(
        "active_{}",
        raw::sha256_hex(&format!(
            "{provider}\0{session_id}\0{idx}\0{role}\0{content}"
        ))
    )
}

fn raw_replay_message(message: &LcmRawMessage) -> Value {
    json!({
        "role": message.role,
        "content": message.content,
        "store_id": message.store_id,
    })
}

fn summary_replay_message(summary: &LcmSummaryNode) -> Value {
    json!({
        "role": "system",
        "content": summary.summary_text,
        "lcm_summary_node_id": summary.node_id,
    })
}

fn estimate_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

fn source_token_count(backlog: &[LcmRawMessage]) -> i64 {
    backlog
        .iter()
        .map(|message| estimate_tokens(&message.content))
        .sum::<i64>()
}

fn debt_for_deferred_backlog(deferred_backlog: &[LcmRawMessage]) -> Vec<LcmMaintenanceDebt> {
    match (deferred_backlog.first(), deferred_backlog.last()) {
        (Some(first), Some(last)) => vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: first.store_id,
            to_store_id: last.store_id,
        }],
        _ => Vec::new(),
    }
}

fn rescuing_summary_text(
    summary_text: String,
    backlog: &[LcmRawMessage],
    source_token_count: i64,
) -> (String, bool) {
    let source_texts = backlog
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>();
    rescuing_summary_text_from_texts(summary_text, source_texts, source_token_count)
}

fn rescuing_summary_text_from_texts(
    summary_text: String,
    source_texts: Vec<String>,
    source_token_count: i64,
) -> (String, bool) {
    if source_token_count < MIN_SUMMARY_RESCUE_SOURCE_TOKENS
        || estimate_tokens(&summary_text) < source_token_count
    {
        return (summary_text, false);
    }
    (
        deterministic_fallback_summary(&source_texts, source_token_count),
        true,
    )
}

fn deterministic_fallback_summary(source_texts: &[String], source_token_count: i64) -> String {
    if source_token_count <= 4 {
        return "summary".to_string();
    }
    let take_limit = ((source_token_count as usize) / 2).saturating_sub(4).max(1);
    let words = source_texts
        .iter()
        .flat_map(|text| text.split_whitespace())
        .take(take_limit)
        .collect::<Vec<_>>();
    format!("[deterministic LCM summary: {}]", words.join(" "))
}

fn debt_to_db(debt: &LcmMaintenanceDebt) -> (String, &'static str, Option<i64>, Option<i64>) {
    match debt {
        LcmMaintenanceDebt::RawBacklog {
            from_store_id,
            to_store_id,
        } => (
            format!("raw_backlog:{from_store_id}:{to_store_id}"),
            "raw_backlog",
            Some(*from_store_id),
            Some(*to_store_id),
        ),
    }
}

fn debt_from_db(
    debt_kind: &str,
    from_store_id: Option<i64>,
    to_store_id: Option<i64>,
) -> Result<LcmMaintenanceDebt, LcmError> {
    match debt_kind {
        "raw_backlog" => Ok(LcmMaintenanceDebt::RawBacklog {
            from_store_id: from_store_id.unwrap_or(0),
            to_store_id: to_store_id.unwrap_or(0),
        }),
        _ => Err(LcmError::Db(format!(
            "invalid maintenance debt kind: {debt_kind}"
        ))),
    }
}

fn opt_text(value: Option<&str>) -> DbValue {
    value.map_or(DbValue::Null, |s| DbValue::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> DbValue {
    value.map_or(DbValue::Null, DbValue::Integer)
}
