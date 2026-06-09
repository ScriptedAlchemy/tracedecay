use std::path::Path;

use libsql::{params, Connection};
use serde_json::{json, Map, Value};

use crate::sessions::SessionMessageRecord;

use super::{
    dag, payload, raw, security, util, LcmCompressionRequest, LcmCompressionResponse, LcmError,
    LcmLifecycleState, LcmLifecycleUpdate, LcmMaintenanceDebt, LcmPreflightRequest,
    LcmPreflightResponse, LcmRawMessage, LcmSessionBoundaryRequest, LcmSessionBoundaryResponse,
    LcmSourceRef, LcmStorageKind, LcmSummarizerMode, LcmSummaryNode, LcmSummaryNodeDraft,
    LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange,
    LCM_DEFAULT_FRESH_TAIL_COUNT, LCM_DEFAULT_SUMMARY_FAN_IN,
};

// Mirrors hermes-lcm `_compression_boundary_cooldown_active`: a boundary skip
// suppresses compression for 60 seconds.
const COMPRESSION_BOUNDARY_COOLDOWN_SECONDS: i64 = 60;
// Mirrors hermes-lcm `LCMConfig.incremental_max_depth` default: condensation
// only consumes nodes at depths strictly below this ceiling, so depth-1
// summaries are never re-condensed to depth 2+ at default settings. The
// configurable knob is plumbed by the config workstream; the engine enforces
// the Hermes default here.
const DEFAULT_INCREMENTAL_MAX_DEPTH: i64 = 1;
const MAX_FORCED_CATCHUP_PASSES: usize = 4;
const MIN_SUMMARY_RESCUE_SOURCE_TOKENS: i64 = 8;
const ACTIVE_REPLAY_METADATA_KEY: &str = "lcm_active_replay";
const ACTIVE_REPLAY_MESSAGE_KEY: &str = "active_replay";
const PRESERVED_TODO_CONTEXT_PREFIX: &str =
    "[Your active task list was preserved across context compression]";
const PRESERVED_OBJECTIVE_CONTEXT_PREFIX: &str =
    "[Current user objective preserved from compacted history]";

fn incremental_max_depth_limit(configured: Option<i64>) -> i64 {
    match configured {
        Some(value) if value < 0 => i64::MAX,
        Some(value) => value,
        None => DEFAULT_INCREMENTAL_MAX_DEPTH,
    }
}

struct IngestedActiveMessages {
    replay_messages: Vec<Value>,
    changed_replay: bool,
}

struct ExistingActiveMessageState {
    session_id: String,
    role: String,
    timestamp: Option<i64>,
    ordinal: i64,
    content_hash: String,
    metadata_json: Option<String>,
}

pub(crate) async fn update_lifecycle(
    conn: &Connection,
    update: LcmLifecycleUpdate,
) -> Result<LcmLifecycleState, LcmError> {
    util::with_immediate_tx(conn, async {
        upsert_lifecycle_state(conn, &update).await?;
        replace_maintenance_debt(
            conn,
            &update.provider,
            &update.conversation_id,
            &update.maintenance_debt,
        )
        .await?;
        Ok(())
    })
    .await?;
    lifecycle_state(conn, &update.provider, &update.conversation_id).await
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
        .await?;
    let row = rows.next().await?.ok_or(LcmError::LifecycleStateNotFound)?;
    let maintenance_debt = load_maintenance_debt(conn, provider, conversation_id).await?;
    Ok(LcmLifecycleState {
        provider: row.get(0)?,
        conversation_id: row.get(1)?,
        current_session_id: row.get(2)?,
        current_frontier_store_id: row.get(3)?,
        last_finalized_session_id: row.get(4)?,
        last_finalized_frontier_store_id: row.get(5)?,
        maintenance_debt,
    })
}

/// Records a compression-boundary session start, mirroring hermes-lcm
/// `_continue_compression_boundary`.
///
/// Hermes carries all LCM data over when the host's `old_session_id` matches
/// the bound session (finalize + reassign of messages, DAG nodes, and
/// externalized payloads, engine.py:1902-1923); when it does not, the boundary
/// skips carry-over and starts a short compression cooldown so the new session
/// does not cascade straight back into compression while pressure is still
/// unrelieved.
pub(crate) async fn record_session_boundary(
    conn: &Connection,
    request: LcmSessionBoundaryRequest,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    let old_session_id = request.old_session_id.as_deref().unwrap_or("");
    let is_compression_boundary = request.boundary_reason.as_deref() == Some("compression")
        && !old_session_id.is_empty()
        && old_session_id != request.session_id;
    if !is_compression_boundary {
        return Ok(session_boundary_response(false, "not_compression_boundary"));
    }
    if request.bound_session_id.as_deref() == Some(old_session_id) {
        return carry_over_session_boundary(conn, &request, old_session_id).await;
    }

    conn.execute(
        "INSERT INTO lcm_lifecycle_state (
            provider, conversation_id, current_session_id, boundary_skip_at, updated_at
         )
         VALUES (?1, ?2, ?2, COALESCE(?3, unixepoch()), unixepoch())
         ON CONFLICT(provider, conversation_id) DO UPDATE SET
            current_session_id = excluded.current_session_id,
            boundary_skip_at = excluded.boundary_skip_at,
            updated_at = unixepoch()",
        params![
            request.provider.as_str(),
            request.session_id.as_str(),
            util::opt_i64(request.boundary_skip_at),
        ],
    )
    .await?;
    Ok(session_boundary_response(
        true,
        "compression_boundary_skip_recorded",
    ))
}

/// Carries all LCM data forward across a matching-bound compression boundary,
/// mirroring the hermes-lcm happy path: finalize the old session, then
/// transactionally reassign raw messages, DAG nodes, and externalized payload
/// ownership to the new session id and rebind lifecycle state to it.
async fn carry_over_session_boundary(
    conn: &Connection,
    request: &LcmSessionBoundaryRequest,
    old_session_id: &str,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    util::with_immediate_tx(
        conn,
        carry_over_in_transaction(conn, request, old_session_id),
    )
    .await
}

async fn carry_over_in_transaction(
    conn: &Connection,
    request: &LcmSessionBoundaryRequest,
    old_session_id: &str,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    ensure_session(conn, &request.provider, &request.session_id).await?;
    let mut target_rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![request.provider.as_str(), request.session_id.as_str()],
        )
        .await?;
    let target_row = target_rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("carry-over guard query returned no rows".to_string()))?;
    let target_message_count: i64 = target_row.get(0)?;
    if target_message_count > 0 {
        return Err(LcmError::Db(format!(
            "compression boundary carry-over requires an empty target session; session {} already has {} raw message(s)",
            request.session_id, target_message_count
        )));
    }
    let old_state =
        lifecycle_state_or_default(conn, &request.provider, old_session_id, old_session_id).await?;
    // Mirrors hermes-lcm: the carried frontier is the strongest durable
    // marker recorded for the source session.
    let carried_frontier = [
        old_state.current_frontier_store_id,
        old_state.last_finalized_frontier_store_id,
    ]
    .into_iter()
    .flatten()
    .max();

    raw::reassign_session_messages(conn, &request.provider, old_session_id, &request.session_id)
        .await?;
    dag::reassign_session_nodes(conn, &request.provider, old_session_id, &request.session_id)
        .await?;
    payload::reassign_session_payloads(
        conn,
        &request.provider,
        old_session_id,
        &request.session_id,
    )
    .await?;

    let update = LcmLifecycleUpdate {
        provider: request.provider.clone(),
        conversation_id: request.session_id.clone(),
        current_session_id: request.session_id.clone(),
        current_frontier_store_id: carried_frontier,
        last_finalized_session_id: Some(old_session_id.to_string()),
        last_finalized_frontier_store_id: carried_frontier,
        maintenance_debt: old_state.maintenance_debt.clone(),
    };
    upsert_lifecycle_state(conn, &update).await?;
    replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await?;
    // Every LCM call keys conversation_id = session_id in this port, so the
    // old conversation row is fully superseded by the rebound one above.
    conn.execute(
        "DELETE FROM lcm_lifecycle_state WHERE provider = ?1 AND conversation_id = ?2",
        params![request.provider.as_str(), old_session_id],
    )
    .await?;

    Ok(session_boundary_response(
        true,
        "compression_boundary_carried_over",
    ))
}

fn session_boundary_response(recorded: bool, reason: &str) -> LcmSessionBoundaryResponse {
    LcmSessionBoundaryResponse {
        status: "ok".to_string(),
        recorded,
        reason: reason.to_string(),
    }
}

async fn boundary_cooldown_active(
    conn: &Connection,
    provider: &str,
    conversation_id: &str,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1
                FROM lcm_lifecycle_state
                WHERE provider = ?1 AND conversation_id = ?2
                  AND boundary_skip_at IS NOT NULL
                  AND unixepoch() - boundary_skip_at < ?3
             )",
            params![
                provider,
                conversation_id,
                COMPRESSION_BOUNDARY_COOLDOWN_SECONDS
            ],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("boundary cooldown query returned no rows".to_string()))?;
    let active: i64 = row.get(0)?;
    Ok(active != 0)
}

pub(crate) async fn preflight(
    conn: &Connection,
    storage_root: &Path,
    request: LcmPreflightRequest,
) -> Result<LcmPreflightResponse, LcmError> {
    let mut request = request;
    request.max_assembly_tokens = effective_assembly_token_cap(
        request.max_assembly_tokens,
        request.context_length,
        request.reserve_tokens_floor,
    );
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
    let ingested = ingest_active_messages_in_transaction(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
        &request.ignore_message_patterns,
    )
    .await?;
    let conversation_id = request.session_id.clone();
    // Mirrors hermes-lcm `should_compress_preflight`: the boundary-skip
    // cooldown is checked after ingest (preflight stays lossless) and blocks
    // every compression trigger, including changed-replay and forced overflow.
    if boundary_cooldown_active(conn, &request.provider, &conversation_id).await? {
        return Ok(LcmPreflightResponse {
            status: "ok".to_string(),
            should_compress: false,
            reason: "compression_boundary_cooldown".to_string(),
            replay_messages: ingested.replay_messages,
        });
    }
    let existing_frontier = lifecycle_state_or_default(
        conn,
        &request.provider,
        &conversation_id,
        &request.session_id,
    )
    .await?;
    let raw_messages =
        load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
    let window = compression_window(
        &raw_messages,
        existing_frontier.current_frontier_store_id,
        request.fresh_tail_count,
    );
    let (should_compress, reason) = preflight_decision(&request, &existing_frontier, &window);
    let should_compress = ingested.changed_replay || should_compress;
    let reason = if ingested.changed_replay {
        "ingest_protection_changed_replay"
    } else {
        reason
    };
    Ok(LcmPreflightResponse {
        status: "ok".to_string(),
        should_compress,
        reason: reason.to_string(),
        replay_messages: ingested.replay_messages,
    })
}

pub(crate) async fn compress(
    conn: &Connection,
    storage_root: &Path,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    let mut request = request;
    request.max_assembly_tokens = effective_assembly_token_cap(
        request.max_assembly_tokens,
        request.context_length,
        request.reserve_tokens_floor,
    );
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
    conn.execute("BEGIN IMMEDIATE", ()).await?;

    let ingested = match ingest_active_messages(
        conn,
        storage_root,
        &request.provider,
        &request.session_id,
        &request.messages,
        &request.ignore_message_patterns,
    )
    .await
    {
        Ok(ingested) => ingested,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
    };

    if matches!(request.summarizer, LcmSummarizerMode::Noop) {
        let frontier = lifecycle_state_or_default(
            conn,
            &request.provider,
            &request.session_id,
            &request.session_id,
        )
        .await?;
        let response = compression_response(
            "ok",
            "noop_summarizer",
            Vec::new(),
            ingested.replay_messages,
            frontier,
            None,
            request.max_assembly_tokens,
        );
        return match conn.execute("COMMIT", ()).await {
            Ok(_) => Ok(response),
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(LcmError::Db(err.to_string()))
            }
        };
    }

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

async fn ingest_active_messages_in_transaction(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    messages: &[Value],
    ignore_message_patterns: &[String],
) -> Result<IngestedActiveMessages, LcmError> {
    util::with_immediate_tx(
        conn,
        ingest_active_messages(
            conn,
            storage_root,
            provider,
            session_id,
            messages,
            ignore_message_patterns,
        ),
    )
    .await
}

async fn compress_in_transaction(
    conn: &Connection,
    request: LcmCompressionRequest,
) -> Result<LcmCompressionResponse, LcmError> {
    let overflow_assembly_cap = overflow_recovery_assembly_cap(
        request.current_tokens,
        request.max_assembly_tokens,
        &request.messages,
    );
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
            let window = compression_window(
                &raw_messages,
                existing_frontier.current_frontier_store_id,
                request.fresh_tail_count,
            );
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
    let window = compression_window(
        &raw_messages,
        existing_frontier.current_frontier_store_id,
        request.fresh_tail_count,
    );

    if window.backlog.is_empty() {
        if should_force_overflow_recovery(&request) {
            // Mirrors hermes-lcm `_assemble_overflow_recovery_context`:
            // without backlog to compact, recover by evicting droppable
            // active-context messages under the cap instead of returning the
            // overflowing context unchanged.
            let replay_messages = assemble_overflow_recovery_replay(
                conn,
                &request.provider,
                &request.session_id,
                ReplayWindowParts {
                    pinned_anchors: &window.pinned_anchors,
                    deferred_backlog: &[],
                    fresh_tail: &window.fresh_tail,
                },
                overflow_assembly_cap,
            )
            .await?;
            let over_budget = replay_exceeds_budget(
                replay_token_estimate(&replay_messages),
                overflow_assembly_cap,
            );
            let (status, reason) = if over_budget {
                ("best_effort", "irreducible_overflow_no_backlog")
            } else {
                ("ok", "overflow_recovery_no_backlog")
            };
            return Ok(compression_response(
                status,
                reason,
                Vec::new(),
                replay_messages,
                existing_frontier,
                None,
                overflow_assembly_cap,
            ));
        }
        if let Some(response) = condense_summary_nodes_if_ready(
            conn,
            &request,
            &conversation_id,
            &existing_frontier,
            &window,
        )
        .await?
        {
            return Ok(response);
        }
        let replay_messages = assemble_replay_context(
            conn,
            &request.provider,
            &request.session_id,
            ReplayWindowParts {
                pinned_anchors: &window.pinned_anchors,
                deferred_backlog: &[],
                fresh_tail: &window.fresh_tail,
            },
            request.max_assembly_tokens,
        )
        .await?;
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

    // Mirrors hermes-lcm `compress()`: a threshold-style request no-ops when
    // the raw backlog outside the fresh tail is strictly below the working
    // leaf chunk threshold. Forced overflow recovery and outstanding
    // maintenance debt bypass the guard, matching Hermes' `force_overflow`
    // and deferred-maintenance escape hatches.
    if !should_force_overflow_recovery(&request)
        && !frontier_has_maintenance_debt(&existing_frontier)
    {
        let leaf_chunk_tokens = effective_leaf_chunk_tokens(
            request.leaf_chunk_tokens,
            request.dynamic_leaf_chunk_enabled,
            request.dynamic_leaf_chunk_max,
            source_token_count(&window.backlog),
        );
        if !has_eligible_backlog(&window.backlog, leaf_chunk_tokens) {
            let replay_messages = assemble_replay_context(
                conn,
                &request.provider,
                &request.session_id,
                ReplayWindowParts {
                    pinned_anchors: &window.pinned_anchors,
                    deferred_backlog: &window.backlog,
                    fresh_tail: &window.fresh_tail,
                },
                request.max_assembly_tokens,
            )
            .await?;
            return Ok(compression_response(
                "ok",
                "backlog_below_leaf_chunk_threshold",
                Vec::new(),
                replay_messages,
                existing_frontier,
                None,
                request.max_assembly_tokens,
            ));
        }
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
        let leaf_chunk_tokens = effective_leaf_chunk_tokens(
            request.leaf_chunk_tokens,
            request.dynamic_leaf_chunk_enabled,
            request.dynamic_leaf_chunk_max,
            source_token_count(&remaining_backlog),
        );
        let selected_len = bounded_leaf_chunk_len(
            &remaining_backlog,
            leaf_chunk_tokens,
            request.max_source_messages,
        );
        let selected_backlog = remaining_backlog[..selected_len].to_vec();
        let source_tokens = source_token_count(&selected_backlog);
        let (pass_summary_text, pass_fallback_used) =
            rescuing_summary_text(summary_text.clone(), &selected_backlog, source_tokens);
        fallback_used |= pass_fallback_used;

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
        created_summaries.push(summary);
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
    // The summaries created above are already persisted in this transaction,
    // so the shared assembler replays them together with any earlier
    // uncondensed summary history (hermes-lcm `_assemble_context`).
    let replay_messages = assemble_replay_context(
        conn,
        &request.provider,
        &request.session_id,
        ReplayWindowParts {
            pinned_anchors: &window.pinned_anchors,
            deferred_backlog: &remaining_backlog,
            fresh_tail: &window.fresh_tail,
        },
        request.max_assembly_tokens,
    )
    .await?;
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
        && replay_exceeds_budget(replay_token_estimate, overflow_assembly_cap)
    {
        status = "best_effort";
        reason = "forced_overflow_recovery_replay_over_budget";
    }
    let compression_attempts = created_summaries.len();
    let summary_nodes = created_summaries;

    let retry_status = if plan.forced_overflow_recovery {
        Some("critical_pressure_catch_up")
    } else if fallback_used {
        Some("fallback_summary")
    } else {
        None
    };

    Ok(compression_response_with_attempt_state(
        CompressionResponseParts {
            status,
            reason,
            summary_nodes,
            replay_messages,
            frontier,
            summary_request: None,
            max_assembly_tokens: if plan.forced_overflow_recovery {
                overflow_assembly_cap
            } else {
                request.max_assembly_tokens
            },
        },
        CompressionAttemptState {
            compression_attempts,
            fallback_used,
            retry_status,
        },
    ))
}

pub(crate) async fn maintenance_debt_count(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = util::opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_maintenance_debt d
             JOIN lcm_lifecycle_state s
               ON s.provider = d.provider AND s.conversation_id = d.conversation_id
             WHERE d.provider = ?1 AND (?2 IS NULL OR s.current_session_id = ?2)",
            params![provider, session_value],
        )
        .await?;
    let row = rows
        .next()
        .await?
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
            util::opt_text(update.last_finalized_session_id.as_deref()),
            util::opt_i64(update.current_frontier_store_id),
            util::opt_i64(update.last_finalized_frontier_store_id),
        ],
    )
    .await?;
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
    .await?;

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
                util::opt_i64(from_store_id),
                util::opt_i64(to_store_id),
            ],
        )
        .await?;
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
        .await?;
    let mut debts = Vec::new();
    while let Some(row) = rows.next().await? {
        let debt_kind: String = row.get(0)?;
        debts.push(debt_from_db(&debt_kind, row.get(1)?, row.get(2)?)?);
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
        Err(LcmError::LifecycleStateNotFound) => Ok(LcmLifecycleState {
            provider: provider.to_string(),
            conversation_id: conversation_id.to_string(),
            current_session_id: session_id.to_string(),
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
            maintenance_debt: Vec::new(),
        }),
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

fn compression_window(
    raw_messages: &[LcmRawMessage],
    current_frontier_store_id: Option<i64>,
    fresh_tail_count: Option<usize>,
) -> CompressionWindow {
    let frontier_store_id = current_frontier_store_id.unwrap_or(0);
    let unsummarized = raw_messages
        .iter()
        .filter(|message| message.store_id > frontier_store_id)
        .cloned()
        .collect::<Vec<_>>();
    let backlog_len = unsummarized
        .len()
        .saturating_sub(fresh_tail_count.unwrap_or(LCM_DEFAULT_FRESH_TAIL_COUNT));
    let (older_unsummarized, fresh_tail) = unsummarized.split_at(backlog_len);
    let fresh_tail_start_store_id = fresh_tail
        .first()
        .map_or(i64::MAX, |message| message.store_id);
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
    let leaf_chunk_tokens = effective_leaf_chunk_tokens(
        request.leaf_chunk_tokens,
        request.dynamic_leaf_chunk_enabled,
        request.dynamic_leaf_chunk_max,
        source_token_count(&window.backlog),
    );
    let selected_len = bounded_leaf_chunk_len(
        &window.backlog,
        leaf_chunk_tokens,
        request.max_source_messages,
    );
    CompressionPlan {
        selected_backlog: window.backlog[..selected_len].to_vec(),
        forced_overflow_recovery,
    }
}

fn should_force_overflow_recovery(request: &LcmCompressionRequest) -> bool {
    forced_overflow_pressure(request.current_tokens, request.max_assembly_tokens)
}

fn preflight_decision(
    request: &LcmPreflightRequest,
    frontier: &LcmLifecycleState,
    window: &CompressionWindow,
) -> (bool, &'static str) {
    if forced_overflow_pressure(request.current_tokens, request.max_assembly_tokens) {
        return (true, "forced_overflow_pressure");
    }

    if frontier_has_maintenance_debt(frontier) {
        return (true, "maintenance_debt_ready");
    }

    if threshold_pressure(request.current_tokens, request.threshold_tokens) {
        if window.backlog.is_empty() {
            return (false, "threshold_no_eligible_backlog");
        }
        let leaf_chunk_tokens = effective_leaf_chunk_tokens(
            request.leaf_chunk_tokens,
            request.dynamic_leaf_chunk_enabled,
            request.dynamic_leaf_chunk_max,
            source_token_count(&window.backlog),
        );
        if has_eligible_backlog(&window.backlog, leaf_chunk_tokens) {
            return (true, "threshold_backlog_ready");
        }
        return (false, "threshold_no_eligible_backlog");
    }

    (false, "no_compression_needed")
}

fn threshold_pressure(current_tokens: Option<i64>, threshold_tokens: Option<i64>) -> bool {
    match (current_tokens, threshold_tokens) {
        (Some(current_tokens), Some(threshold_tokens)) if threshold_tokens > 0 => {
            current_tokens >= threshold_tokens
        }
        _ => false,
    }
}

// Mirrors hermes-lcm `_effective_assembly_token_cap`: the active assembly cap
// is the minimum of the explicit max_assembly_tokens knob and the
// reserve-derived `context_length - reserve_tokens_floor` headroom cap. A
// reserve floor that leaves no headroom disables the reserve-based cap
// (Hermes logs a warning and drops it) instead of clamping it to zero.
fn effective_assembly_token_cap(
    max_assembly_tokens: Option<i64>,
    context_length: Option<i64>,
    reserve_tokens_floor: Option<i64>,
) -> Option<i64> {
    let explicit_cap = max_assembly_tokens.filter(|cap| *cap > 0);
    let reserve_cap = match (
        context_length.filter(|length| *length > 0),
        reserve_tokens_floor.filter(|floor| *floor > 0),
    ) {
        (Some(context_length), Some(reserve_tokens_floor)) => {
            Some(context_length - reserve_tokens_floor).filter(|cap| *cap > 0)
        }
        _ => None,
    };
    [explicit_cap, reserve_cap]
        .into_iter()
        .flatten()
        .min()
        .map(|cap| cap.max(1))
}

fn overflow_recovery_assembly_cap(
    current_tokens: Option<i64>,
    max_assembly_tokens: Option<i64>,
    messages: &[Value],
) -> Option<i64> {
    let assembly_cap = max_assembly_tokens?;
    let Some(current_tokens) = current_tokens.filter(|tokens| *tokens > 0) else {
        return Some(assembly_cap);
    };
    if messages.is_empty() {
        return Some(assembly_cap);
    }
    let message_tokens = messages
        .iter()
        .map(|message| estimate_tokens(&message_content(message)))
        .sum::<i64>();
    let overhead_tokens = (current_tokens - message_tokens).max(0);
    Some((assembly_cap - overhead_tokens).max(1))
}

fn forced_overflow_pressure(current_tokens: Option<i64>, max_assembly_tokens: Option<i64>) -> bool {
    match (current_tokens, max_assembly_tokens) {
        (Some(current_tokens), Some(max_assembly_tokens)) if max_assembly_tokens > 0 => {
            current_tokens >= max_assembly_tokens
        }
        _ => false,
    }
}

fn frontier_has_maintenance_debt(frontier: &LcmLifecycleState) -> bool {
    frontier
        .maintenance_debt
        .iter()
        .any(|debt| matches!(debt, LcmMaintenanceDebt::RawBacklog { .. }))
}

// Mirrors hermes-lcm `_leaf_compaction_candidate_status`: the full raw backlog
// outside the fresh tail must reach the working leaf chunk threshold.
// `max_source_messages` is intentionally ignored here; it only bounds how many
// messages a single compression pass consumes, and truncating the eligibility
// sum with it would permanently stall threshold compression whenever the
// oldest capped slice stays below the leaf chunk threshold.
fn has_eligible_backlog(backlog: &[LcmRawMessage], leaf_chunk_tokens: Option<i64>) -> bool {
    if backlog.is_empty() {
        return false;
    }

    match leaf_chunk_tokens.filter(|limit| *limit > 0) {
        Some(token_limit) => source_token_count(backlog) >= token_limit,
        None => true,
    }
}

fn effective_leaf_chunk_tokens(
    leaf_chunk_tokens: Option<i64>,
    dynamic_leaf_chunk_enabled: Option<bool>,
    dynamic_leaf_chunk_max: Option<i64>,
    raw_tokens_outside_tail: i64,
) -> Option<i64> {
    if !dynamic_leaf_chunk_enabled.unwrap_or(false) {
        return leaf_chunk_tokens;
    }
    let base = leaf_chunk_tokens.unwrap_or(1).max(1);
    let ceiling = dynamic_leaf_chunk_max.unwrap_or(base).max(base);
    let mut working = base;
    while working < ceiling && raw_tokens_outside_tail > working.saturating_mul(2) {
        working = ceiling.min(working.saturating_mul(2));
    }
    Some(working)
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

const SUMMARY_REPLAY_PRIORITY: u8 = 0;
const RAW_REPLAY_PRIORITY: u8 = 1;

struct ReplayWindowParts<'a> {
    pinned_anchors: &'a [LcmRawMessage],
    deferred_backlog: &'a [LcmRawMessage],
    fresh_tail: &'a [LcmRawMessage],
}

/// Assembles the active replay context, mirroring hermes-lcm
/// `_assemble_context`: policy anchors are always kept, every uncondensed DAG
/// summary node is replayed (budgeted highest depth first), and the raw tail
/// is trimmed under the effective assembly cap.
async fn assemble_replay_context(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    parts: ReplayWindowParts<'_>,
    max_assembly_tokens: Option<i64>,
) -> Result<Vec<Value>, LcmError> {
    let summaries = dag::load_uncondensed_summary_nodes(conn, provider, session_id).await?;
    let anchor_source = load_raw_messages_for_session(conn, provider, session_id).await?;
    let anchor_source = anchor_source.iter().collect::<Vec<_>>();
    let (anchors, raws) = split_leading_anchors(&parts);
    Ok(assemble_replay_messages(
        &anchors,
        &summaries,
        &raws,
        &anchor_source,
        max_assembly_tokens,
    ))
}

/// Mirrors hermes-lcm `_assemble_overflow_recovery_context`: assemble under
/// the cap; when nothing beyond the anchors fits, fall back to anchors plus
/// the most recent message even if that stays over budget.
async fn assemble_overflow_recovery_replay(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    parts: ReplayWindowParts<'_>,
    max_assembly_tokens: Option<i64>,
) -> Result<Vec<Value>, LcmError> {
    let summaries = dag::load_uncondensed_summary_nodes(conn, provider, session_id).await?;
    let anchor_source = load_raw_messages_for_session(conn, provider, session_id).await?;
    let anchor_source = anchor_source.iter().collect::<Vec<_>>();
    let (anchors, raws) = split_leading_anchors(&parts);
    let candidate = assemble_replay_messages(
        &anchors,
        &summaries,
        &raws,
        &anchor_source,
        max_assembly_tokens,
    );
    if candidate.len() == anchors.len() {
        if let Some(last) = raws.last() {
            let mut replay = anchors
                .iter()
                .map(|message| raw_replay_message(message))
                .collect::<Vec<_>>();
            replay.push(raw_replay_message(last));
            return Ok(replay);
        }
    }
    Ok(candidate)
}

/// Mirrors hermes-lcm `_leading_anchor_count`: policy anchors at the very
/// start of the remaining context behave like the leading system message and
/// are never budget-dropped.
fn split_leading_anchors<'a>(
    parts: &ReplayWindowParts<'a>,
) -> (Vec<&'a LcmRawMessage>, Vec<&'a LcmRawMessage>) {
    let mut anchors = parts.pinned_anchors.iter().collect::<Vec<_>>();
    let mut raws = parts
        .deferred_backlog
        .iter()
        .chain(parts.fresh_tail.iter())
        .collect::<Vec<_>>();
    let promoted = raws
        .iter()
        .take_while(|message| is_policy_anchor_role(&message.role))
        .count();
    anchors.extend(raws.drain(..promoted));
    (anchors, raws)
}

fn assemble_replay_messages(
    anchors: &[&LcmRawMessage],
    summaries: &[dag::LcmUncondensedSummaryNode],
    raws: &[&LcmRawMessage],
    anchor_source: &[&LcmRawMessage],
    max_assembly_tokens: Option<i64>,
) -> Vec<Value> {
    let (selected_raws, selected_summaries, preserved_objective_anchor) = match max_assembly_tokens
    {
        None => (
            raws.to_vec(),
            summaries.iter().collect::<Vec<_>>(),
            latest_user_context_anchor(anchor_source, raws),
        ),
        Some(cap) => {
            let used = anchors
                .iter()
                .map(|message| estimate_tokens(&message.content))
                .sum::<i64>();
            let (selected_raws, tail_tokens) = select_budget_tail(raws, used, cap);
            let mut summary_budget = (cap - used - tail_tokens).max(0);
            let preserved_objective_anchor =
                latest_user_context_anchor(anchor_source, &selected_raws).and_then(
                    |(store_id, part, already_preserved)| {
                        if already_preserved {
                            return Some((store_id, part, already_preserved));
                        }
                        let part_tokens = estimate_tokens(&part);
                        if part_tokens <= summary_budget {
                            summary_budget -= part_tokens;
                            Some((store_id, part, already_preserved))
                        } else {
                            None
                        }
                    },
                );
            (
                selected_raws,
                select_budget_summaries(summaries, summary_budget),
                preserved_objective_anchor,
            )
        }
    };

    let mut replay_items = Vec::with_capacity(
        anchors.len()
            + selected_summaries.len()
            + selected_raws.len()
            + usize::from(preserved_objective_anchor.is_some()),
    );
    replay_items.extend(anchors.iter().map(|message| {
        (
            message.store_id,
            RAW_REPLAY_PRIORITY,
            raw_replay_message(message),
        )
    }));
    replay_items.extend(selected_summaries.iter().map(|summary| {
        (
            summary.first_source_store_id.unwrap_or(i64::MAX),
            SUMMARY_REPLAY_PRIORITY,
            summary_replay_message(&summary.node),
        )
    }));
    if let Some((store_id, preserved_objective_anchor, _already_preserved)) =
        preserved_objective_anchor
    {
        replay_items.push((
            store_id,
            SUMMARY_REPLAY_PRIORITY,
            json!({
                "role": "system",
                "content": preserved_objective_anchor,
            }),
        ));
    }
    replay_items.extend(selected_raws.iter().map(|message| {
        (
            message.store_id,
            RAW_REPLAY_PRIORITY,
            raw_replay_message(message),
        )
    }));
    replay_items.sort_by_key(|(store_id, priority, _)| (*store_id, *priority));
    replay_items
        .into_iter()
        .map(|(_, _, message)| message)
        .collect()
}

/// Mirrors hermes-lcm `_assemble_context` tail selection: keep the newest
/// contiguous run of messages that fits under the cap; a non-fitting
/// assistant/tool turn is skipped (evicted), a non-fitting prompt-bearing
/// turn stops selection, and nothing older is kept once a gap was skipped.
fn select_budget_tail<'a>(
    raws: &[&'a LcmRawMessage],
    used: i64,
    cap: i64,
) -> (Vec<&'a LcmRawMessage>, i64) {
    let mut kept_reversed = Vec::new();
    let mut tail_tokens = 0i64;
    let mut skipped_gap = false;
    for message in raws.iter().rev() {
        let message_tokens = estimate_tokens(&message.content);
        if used + tail_tokens + message_tokens > cap {
            if is_budget_droppable_tail_message(message) {
                skipped_gap = true;
                continue;
            }
            break;
        }
        if skipped_gap {
            break;
        }
        kept_reversed.push(*message);
        tail_tokens += message_tokens;
    }
    kept_reversed.reverse();
    (kept_reversed, tail_tokens)
}

/// Mirrors hermes-lcm `_is_budget_droppable_tail_message`: assistant/tool
/// turns are derived context and may be evicted under budget pressure;
/// user/system turns are prompt-bearing and stop tail selection.
fn is_budget_droppable_tail_message(message: &LcmRawMessage) -> bool {
    if !matches!(message.role.as_str(), "assistant" | "tool") {
        return false;
    }
    let content = &message.content;
    !content.contains(PRESERVED_TODO_CONTEXT_PREFIX)
        && !content.contains(PRESERVED_OBJECTIVE_CONTEXT_PREFIX)
}

fn latest_user_context_anchor(
    raws: &[&LcmRawMessage],
    selected_tail: &[&LcmRawMessage],
) -> Option<(i64, String, bool)> {
    for message in raws.iter().rev() {
        if let Some(preserved) = preserved_objective_context_content(&message.content) {
            if selected_tail.iter().any(|selected| {
                preserved_objective_context_content(&selected.content) == Some(preserved)
            }) {
                return None;
            }
            return Some((message.store_id, preserved.to_string(), true));
        }
        if message.role != "user" {
            continue;
        }
        if is_preserved_todo_context_message(&message.content) {
            continue;
        }
        if selected_tail
            .iter()
            .any(|selected| selected.store_id == message.store_id)
        {
            return None;
        }
        return Some((
            message.store_id,
            format!("{PRESERVED_OBJECTIVE_CONTEXT_PREFIX}\n{}", message.content),
            false,
        ));
    }
    None
}

fn is_preserved_todo_context_message(content: &str) -> bool {
    content
        .trim_start()
        .starts_with(PRESERVED_TODO_CONTEXT_PREFIX)
}

fn preserved_objective_context_content(content: &str) -> Option<&str> {
    content
        .trim_start()
        .starts_with(PRESERVED_OBJECTIVE_CONTEXT_PREFIX)
        .then_some(content)
}

/// Mirrors hermes-lcm summary-block budgeting: highest-depth summaries claim
/// the budget first; parts that do not fit are skipped without ending the
/// scan, so smaller lower-depth summaries can still land.
fn select_budget_summaries(
    summaries: &[dag::LcmUncondensedSummaryNode],
    summary_budget: i64,
) -> Vec<&dag::LcmUncondensedSummaryNode> {
    let mut by_depth = (0..summaries.len()).collect::<Vec<_>>();
    by_depth.sort_by_key(|&idx| std::cmp::Reverse(summaries[idx].node.depth));
    let mut selected = vec![false; summaries.len()];
    let mut used = 0i64;
    for idx in by_depth {
        let summary_tokens = estimate_tokens(&summaries[idx].node.summary_text);
        if used + summary_tokens > summary_budget {
            continue;
        }
        used += summary_tokens;
        selected[idx] = true;
    }
    summaries
        .iter()
        .enumerate()
        .filter(|(idx, _)| selected[*idx])
        .map(|(_, summary)| summary)
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
    compression_response_with_attempt_state(
        CompressionResponseParts {
            status,
            reason,
            summary_nodes,
            replay_messages,
            frontier,
            summary_request,
            max_assembly_tokens,
        },
        CompressionAttemptState {
            compression_attempts: 0,
            fallback_used: false,
            retry_status: None,
        },
    )
}

struct CompressionResponseParts<'a> {
    status: &'a str,
    reason: &'a str,
    summary_nodes: Vec<LcmSummaryNode>,
    replay_messages: Vec<Value>,
    frontier: LcmLifecycleState,
    summary_request: Option<LcmSummaryRequest>,
    max_assembly_tokens: Option<i64>,
}

#[derive(Clone, Copy)]
struct CompressionAttemptState<'a> {
    compression_attempts: usize,
    fallback_used: bool,
    retry_status: Option<&'a str>,
}

fn compression_response_with_attempt_state(
    parts: CompressionResponseParts<'_>,
    attempt_state: CompressionAttemptState<'_>,
) -> LcmCompressionResponse {
    let CompressionResponseParts {
        status,
        reason,
        summary_nodes,
        replay_messages,
        frontier,
        summary_request,
        max_assembly_tokens,
    } = parts;
    let CompressionAttemptState {
        compression_attempts,
        fallback_used,
        retry_status,
    } = attempt_state;
    let replay_token_estimate = replay_token_estimate(&replay_messages);
    LcmCompressionResponse {
        status: status.to_string(),
        reason: reason.to_string(),
        summary_nodes_created: summary_nodes.len(),
        summary_nodes,
        replay_messages,
        replay_token_estimate,
        replay_over_budget: replay_exceeds_budget(replay_token_estimate, max_assembly_tokens),
        compression_attempts,
        fallback_used,
        retry_status: retry_status.map(str::to_string),
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
    window: &CompressionWindow,
) -> Result<Option<LcmCompressionResponse>, LcmError> {
    let fan_in = request
        .summary_fan_in
        .filter(|fan_in| *fan_in > 1)
        .unwrap_or(LCM_DEFAULT_SUMMARY_FAN_IN);
    let children = load_condensation_candidates(
        conn,
        &request.provider,
        &request.session_id,
        fan_in,
        incremental_max_depth_limit(request.incremental_max_depth),
    )
    .await?;
    if children.len() < fan_in || matches!(request.summarizer, LcmSummarizerMode::HermesAuxiliary) {
        return Ok(None);
    }

    let summary_text = match &request.summarizer {
        LcmSummarizerMode::Fake { summary_text }
        | LcmSummarizerMode::Provided { summary_text, .. } => summary_text.clone(),
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
        rescuing_summary_text_from_texts(summary_text, &source_texts, source_tokens);
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
    // Mirrors hermes-lcm: `_assemble_context` always follows
    // `_maybe_condense`, so a condensation-only pass still returns the
    // assembled active context instead of an empty replay.
    let replay_messages = assemble_replay_context(
        conn,
        &request.provider,
        &request.session_id,
        ReplayWindowParts {
            pinned_anchors: &window.pinned_anchors,
            deferred_backlog: &[],
            fresh_tail: &window.fresh_tail,
        },
        request.max_assembly_tokens,
    )
    .await?;
    Ok(Some(compression_response(
        "ok",
        reason,
        vec![summary],
        replay_messages,
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
    incremental_max_depth: i64,
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
               WHERE depth < ?4
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
            params![
                provider,
                session_id,
                fan_in as i64,
                incremental_max_depth
            ],
        )
        .await
        ?;
    let mut nodes = Vec::new();
    while let Some(row) = rows.next().await? {
        nodes.push(LcmSummaryNode {
            node_id: row.get(0)?,
            provider: row.get(1)?,
            conversation_id: row.get(2)?,
            session_id: row.get(3)?,
            depth: row.get(4)?,
            summary_text: row.get(5)?,
            summary_hash: row.get(6)?,
            summary_token_count: row.get(7)?,
            source_token_count: row.get(8)?,
            source_time_start: row.get(9)?,
            source_time_end: row.get(10)?,
            expand_hint: row.get(11)?,
            metadata_json: row.get(12)?,
            created_at: row.get(13)?,
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
    let first_store_id = backlog.first().map_or(0, |message| message.store_id);
    let last_store_id = backlog.last().map_or(0, |message| message.store_id);
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
    let compiled_ignore_patterns = security::compile_message_patterns(ignore_message_patterns);

    for (idx, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let original_content = message_content_value(message);
        let storage_text = message_storage_text(&original_content);
        let search_text = message_content(message);
        if security::ignore_message_reason_with_compiled(&search_text, &compiled_ignore_patterns)
            .is_some()
        {
            let mut replay = message.clone();
            replay["role"] = Value::String(role);
            replay_messages.push(replay);
            continue;
        }
        let message_id = message
            .get("id")
            .or_else(|| message.get("message_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || deterministic_message_id(provider, session_id, idx, &role, &storage_text),
                str::to_string,
            );
        let existing_state = existing_active_message_state(conn, provider, &message_id).await?;
        let ordinal = if let Some(existing) = existing_state.as_ref() {
            existing.ordinal
        } else {
            next_available_ordinal += 1;
            next_available_ordinal
        };
        let message_timestamp = message.get("timestamp").and_then(Value::as_i64);
        let mut replay = message.clone();
        replay["role"] = Value::String(role.clone());
        replay["content"] = original_content.clone();
        let initial_metadata_json = active_message_metadata(message, &replay);
        let expected_content_hash = raw::sha256_hex(&storage_text);
        if let Some(existing) = existing_state.as_ref() {
            let matches_stored_row = existing.ordinal == ordinal
                && existing.content_hash == expected_content_hash
                && existing.metadata_json.as_deref() == Some(initial_metadata_json.as_str())
                && existing.session_id == session_id
                && existing.role == role
                && existing.timestamp == message_timestamp;
            if matches_stored_row {
                replay_messages.push(replay);
                continue;
            }
        }
        let kind = message
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(default_message_kind(&role)));
        let record = SessionMessageRecord {
            provider: provider.to_string(),
            message_id: message_id.clone(),
            session_id: session_id.to_string(),
            role: role.clone(),
            timestamp: message_timestamp,
            ordinal,
            text: storage_text.clone(),
            kind,
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: Some(initial_metadata_json.clone()),
        };
        let upsert = raw::upsert_raw_message_with_payload(conn, storage_root, &record).await?;
        let raw = super::schema::load_raw_message(conn, provider, &message_id)
            .await
            .ok_or_else(|| LcmError::Db("active message did not persist".to_string()))?;
        let replay_content =
            replay_content_value(&original_content, &raw, upsert.projection_text.as_str());
        if replay_content != original_content || raw.storage_kind == LcmStorageKind::External {
            changed_replay = true;
        }
        replay["content"] = replay_content;
        if let Some(tool_calls) = replay.get("tool_calls").cloned() {
            let protected_tool_calls = raw::protect_replay_field_value(
                conn,
                storage_root,
                &record,
                "tool_calls",
                &tool_calls,
            )
            .await?;
            if protected_tool_calls != tool_calls {
                changed_replay = true;
                replay["tool_calls"] = protected_tool_calls;
            }
        }
        let metadata_json =
            active_replay_metadata_json(upsert.projection_metadata_json.as_deref(), &replay);
        if metadata_json != initial_metadata_json {
            update_active_replay_metadata(conn, provider, &message_id, &metadata_json).await?;
        }
        replay_messages.push(replay);
    }

    Ok(IngestedActiveMessages {
        replay_messages,
        changed_replay,
    })
}

fn message_content_value(message: &Value) -> Value {
    message
        .get("content")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()))
}

fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

fn default_message_kind(role: &str) -> String {
    if role.eq_ignore_ascii_case("tool") {
        "tool_result".to_string()
    } else {
        "message".to_string()
    }
}

fn active_message_metadata(message: &Value, replay: &Value) -> String {
    let mut metadata = Map::new();
    metadata.insert(ACTIVE_REPLAY_METADATA_KEY.to_string(), Value::Bool(true));
    metadata.insert(ACTIVE_REPLAY_MESSAGE_KEY.to_string(), replay.clone());
    if let Some(lcm_ingest) = message.get("lcm_ingest") {
        metadata.insert("lcm_ingest".to_string(), lcm_ingest.clone());
    }
    Value::Object(metadata).to_string()
}

fn replay_content_value(
    original_content: &Value,
    raw: &LcmRawMessage,
    external_projection_text: &str,
) -> Value {
    if raw.storage_kind == LcmStorageKind::External {
        return Value::String(external_projection_text.to_string());
    }
    if original_content.is_string() {
        return Value::String(raw.content.clone());
    }
    serde_json::from_str(&raw.content).unwrap_or_else(|_| Value::String(raw.content.clone()))
}

fn active_replay_metadata_json(existing_metadata_json: Option<&str>, replay: &Value) -> String {
    let mut metadata = existing_metadata_json
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    metadata.insert(ACTIVE_REPLAY_METADATA_KEY.to_string(), Value::Bool(true));
    metadata.insert(ACTIVE_REPLAY_MESSAGE_KEY.to_string(), replay.clone());
    Value::Object(metadata).to_string()
}

async fn update_active_replay_metadata(
    conn: &Connection,
    provider: &str,
    message_id: &str,
    metadata_json: &str,
) -> Result<(), LcmError> {
    conn.execute(
        "UPDATE lcm_raw_messages
         SET metadata_json = ?3
         WHERE provider = ?1 AND message_id = ?2",
        params![provider, message_id, metadata_json],
    )
    .await?;
    Ok(())
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
    .await?;
    Ok(())
}

async fn existing_active_message_state(
    conn: &Connection,
    provider: &str,
    message_id: &str,
) -> Result<Option<ExistingActiveMessageState>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT session_id, role, timestamp, ordinal, content_hash, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            params![provider, message_id],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| {
            Ok(ExistingActiveMessageState {
                session_id: row.get(0)?,
                role: row.get(1)?,
                timestamp: row.get(2)?,
                ordinal: row.get(3)?,
                content_hash: row.get(4)?,
                metadata_json: row.get(5)?,
            })
        })
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
        .await?;
    let row = rows
        .next()
        .await?
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
        .await?;
    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        messages.push(raw::raw_message_from_row(&row)?);
    }
    Ok(messages)
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
    if let Some(mut replay) = active_replay_message_from_metadata(message) {
        replay["role"] = Value::String(message.role.clone());
        replay["store_id"] = Value::from(message.store_id);
        return replay;
    }
    json!({
        "role": message.role,
        "content": message.content,
        "store_id": message.store_id,
    })
}

fn active_replay_message_from_metadata(message: &LcmRawMessage) -> Option<Value> {
    let metadata: Value = serde_json::from_str(message.metadata_json.as_deref()?).ok()?;
    if metadata
        .get(ACTIVE_REPLAY_METADATA_KEY)
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let mut replay = metadata
        .get(ACTIVE_REPLAY_MESSAGE_KEY)
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| legacy_active_replay_message_from_metadata(&metadata))?;
    if !replay.contains_key("content") {
        replay.insert(
            "content".to_string(),
            Value::String(message.content.clone()),
        );
    }
    Some(Value::Object(replay))
}

fn legacy_active_replay_message_from_metadata(metadata: &Value) -> Option<Map<String, Value>> {
    let mut replay = metadata.as_object()?.clone();
    replay.remove(ACTIVE_REPLAY_METADATA_KEY);
    replay.remove(ACTIVE_REPLAY_MESSAGE_KEY);
    replay.remove("ingest_protection");
    replay.remove("external_payload");
    replay.remove("payload_ref");
    replay.remove("byte_count");
    replay.remove("char_count");
    replay.remove("sha256");
    Some(replay)
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
    rescuing_summary_text_from_texts(summary_text, &source_texts, source_token_count)
}

fn rescuing_summary_text_from_texts(
    summary_text: String,
    source_texts: &[String],
    source_token_count: i64,
) -> (String, bool) {
    if source_token_count < MIN_SUMMARY_RESCUE_SOURCE_TOKENS
        || estimate_tokens(&summary_text) < source_token_count
    {
        return (summary_text, false);
    }
    (
        deterministic_fallback_summary(source_texts, source_token_count),
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
