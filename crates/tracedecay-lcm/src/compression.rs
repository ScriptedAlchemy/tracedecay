use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::message_storage_text;
use crate::retrieval_content::projected_content_hash;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Value as SqlValue, params};
use tracedecay_store::SessionMessageRecord;

use super::compression_decision::{
    self, AssemblyCapInput, CompressionPlanInput, CondensationCandidateDecision,
    CondensationDecision, CondensationDecisionInput, OverflowRecoveryCapInput,
    PreflightDecisionInput,
};
use super::extraction;
use super::summarizer::CompressionSummarizerAdapter;
use super::types::{LcmExtractionResult, LcmRelationProjectionStatus, LcmSummarySourceRange};
use super::{
    LCM_DEFAULT_FRESH_TAIL_COUNT, LcmCompressionRequest, LcmCompressionResponse, LcmError,
    LcmLifecycleState, LcmLifecycleUpdate, LcmMaintenanceDebt, LcmPreflightRequest,
    LcmPreflightResponse, LcmRawMessage, LcmSessionBoundaryRequest, LcmSessionBoundaryResponse,
    LcmSourceRef, LcmStorageKind, LcmSummaryNode, LcmSummaryNodeDraft, LcmSummaryRequest, dag,
    payload, raw, replay_transactions, security, util,
};
const MAX_FORCED_CATCHUP_PASSES: usize = 4;
const PRESERVED_TODO_CONTEXT_PREFIX: &str =
    "[Your active task list was preserved across context compression]";
const PRESERVED_OBJECTIVE_CONTEXT_PREFIX: &str =
    "[Current user objective preserved from compacted history]";
const CONTEXT_RECOVERY_HINT_SUFFIX: &str = "If the replay after compression is missing context, query TraceDecay LCM before assuming the compacted summary is complete. Start with tracedecay_message_search or tracedecay_lcm_expand_query; use tracedecay_lcm_describe and tracedecay_lcm_expand when you need summary DAG sources.";

struct IngestedActiveMessages {
    replay_messages: Vec<Value>,
}

/// Per-message state resolved before the ingest loop so the loop does not
/// issue one round trip per message.
enum PreparedActiveMessage {
    /// Message without a real role: never stored (a fabricated role would
    /// enter identity hashes) but still carried verbatim into the replay
    /// output — dropping it would silently lose conversation content.
    ReplayVerbatim { source_index: usize },
    Ingest {
        source_index: usize,
        role: String,
        original_content: Value,
        storage_text: String,
        /// `None` for messages replayed as-is (already summarized, or ignored
        /// by the configured message patterns).
        message_id: Option<String>,
    },
}

struct ExistingActiveMessageState {
    session_id: String,
    role: String,
    timestamp: Option<i64>,
    ordinal: i64,
    content_hash: String,
    metadata_json: Option<String>,
}

struct CompressionTransactionWriteRequest<'a> {
    request: &'a LcmCompressionRequest,
    conversation_id: &'a str,
    existing_frontier: &'a LcmLifecycleState,
    summary_text: &'a str,
    route: Option<String>,
    extraction_result: Option<LcmExtractionResult>,
    backlog: &'a [LcmRawMessage],
    forced_overflow_recovery: bool,
}

struct CompressionTransactionWriteResult {
    created_summaries: Vec<LcmSummaryNode>,
    frontier: LcmLifecycleState,
    remaining_backlog: Vec<LcmRawMessage>,
}

struct CompressionTransactionContext {
    conversation_id: String,
    existing_frontier: LcmLifecycleState,
    raw_messages: Vec<LcmRawMessage>,
    window: CompressionWindow,
    plan: compression_decision::CompressionPlan,
    overflow_assembly_cap: Option<i64>,
    raw_rows_scanned: usize,
    raw_bytes_scanned: u64,
    raw_has_more: bool,
    retained: bool,
}

#[derive(Clone)]
pub struct RetainedCompressionGuard {
    pub row_limit: usize,
    pub byte_limit: u64,
    pub expected_summary_source_range: Option<LcmSummarySourceRange>,
}

pub async fn update_lifecycle(
    conn: &impl Executor,
    update: LcmLifecycleUpdate,
) -> Result<LcmLifecycleState, LcmError> {
    upsert_lifecycle_state(conn, &update).await?;
    replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await?;
    lifecycle_state(conn, &update.provider, &update.conversation_id).await
}

pub async fn lifecycle_state(
    conn: &impl QueryExecutor,
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
/// When the host's `old_session_id` matches the bound session, `TraceDecay`
/// appends a lifecycle boundary link and leaves message, summary, and payload
/// ownership unchanged. A mismatched boundary starts a short compression
/// cooldown so the new session does not cascade straight back into compression
/// while pressure is still unrelieved.
pub async fn record_session_boundary(
    conn: &impl Executor,
    request: LcmSessionBoundaryRequest,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    match compression_decision::boundary_transition_decision(
        &request,
        current_unixepoch(conn).await?,
    ) {
        compression_decision::BoundaryTransitionDecision::Ignore => {
            Ok(session_boundary_response(false, "not_compression_boundary"))
        }
        compression_decision::BoundaryTransitionDecision::CarryOver { old_session_id } => {
            link_session_boundary(conn, &request, &old_session_id).await
        }
        compression_decision::BoundaryTransitionDecision::StartCooldown { boundary_skip_at } => {
            conn.execute(
                "INSERT INTO lcm_lifecycle_state (
                    provider, conversation_id, current_session_id, boundary_skip_at, updated_at
                 )
                 VALUES (?1, ?2, ?2, ?3, unixepoch())
                 ON CONFLICT(provider, conversation_id) DO UPDATE SET
                    current_session_id = excluded.current_session_id,
                    boundary_skip_at = excluded.boundary_skip_at,
                    updated_at = unixepoch()",
                params![
                    request.provider.as_str(),
                    request.session_id.as_str(),
                    boundary_skip_at,
                ],
            )
            .await?;
            Ok(session_boundary_response(
                true,
                "compression_boundary_skip_recorded",
            ))
        }
    }
}

/// Records an immutable boundary link across a matching-bound compression
/// transition. Historical messages, summary nodes, payloads, and source
/// lifecycle rows retain their original owner.
async fn link_session_boundary(
    conn: &impl Executor,
    request: &LcmSessionBoundaryRequest,
    old_session_id: &str,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    link_in_transaction(conn, request, old_session_id).await
}

async fn link_in_transaction(
    conn: &impl Executor,
    request: &LcmSessionBoundaryRequest,
    old_session_id: &str,
) -> Result<LcmSessionBoundaryResponse, LcmError> {
    ensure_session(conn, &request.provider, &request.session_id).await?;
    let old_state =
        lifecycle_state_or_default(conn, &request.provider, old_session_id, old_session_id).await?;
    // The link carries only frozen lifecycle coordinates. Authority rows keep
    // their source-session identity.
    let carried_frontier = [
        old_state.current_frontier_store_id,
        old_state.last_finalized_frontier_store_id,
    ]
    .into_iter()
    .flatten()
    .max();

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
    conn: &impl QueryExecutor,
    provider: &str,
    conversation_id: &str,
) -> Result<bool, LcmError> {
    Ok(compression_decision::cooldown_active(
        load_boundary_skip_at(conn, provider, conversation_id).await?,
        current_unixepoch(conn).await?,
    ))
}

async fn load_boundary_skip_at(
    conn: &impl QueryExecutor,
    provider: &str,
    conversation_id: &str,
) -> Result<Option<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT boundary_skip_at
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND conversation_id = ?2",
            params![provider, conversation_id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => None,
    })
}

async fn current_unixepoch(conn: &impl QueryExecutor) -> Result<i64, LcmError> {
    let mut rows = conn.query("SELECT unixepoch()", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("unixepoch query returned no rows".to_string()))?;
    Ok(row.get(0)?)
}

// Preflight loads the whole raw session to decide whether to compress, so a
// "compression feels slow" profile must see it separately from `compress`.
#[hotpath::measure(label = "sessions.lcm.preflight", future = true)]
pub async fn preflight(
    conn: &impl QueryExecutor,
    request: LcmPreflightRequest,
) -> Result<LcmPreflightResponse, LcmError> {
    let mut request = request;
    request.max_assembly_tokens =
        compression_decision::effective_assembly_token_cap(AssemblyCapInput {
            max_assembly_tokens: request.max_assembly_tokens,
            context_length: request.context_length,
            reserve_tokens_floor: request.reserve_tokens_floor,
        });
    if let Some(reason) = filtered_session_reason(
        &request.session_id,
        &request.ignore_session_patterns,
        &request.stateless_session_patterns,
    ) {
        return Ok(LcmPreflightResponse {
            status: "ok".to_string(),
            should_compress: false,
            reason: reason.to_string(),
            replay_messages: Vec::new(),
        });
    }

    let conversation_id = request.session_id.clone();
    if boundary_cooldown_active(conn, &request.provider, &conversation_id).await? {
        let raw_messages =
            load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
        return Ok(LcmPreflightResponse {
            status: "ok".to_string(),
            should_compress: false,
            reason: "compression_boundary_cooldown".to_string(),
            replay_messages: canonical_replay_messages(&raw_messages),
        });
    }
    let existing_frontier = lifecycle_state_or_default(
        conn,
        &request.provider,
        &conversation_id,
        &request.session_id,
    )
    .await?;
    let mut raw_messages =
        load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
    if raw_messages.is_empty()
        && let Some(bound_session_id) = existing_frontier.last_finalized_session_id.as_deref()
    {
        raw_messages =
            load_raw_messages_for_session(conn, &request.provider, bound_session_id).await?;
    }
    let window = compression_window(
        &raw_messages,
        existing_frontier.current_frontier_store_id,
        request.fresh_tail_count,
        request.current_tokens,
        request.threshold_tokens,
    );
    let decision = compression_decision::preflight_decision(PreflightDecisionInput {
        request: &request,
        frontier: &existing_frontier,
        backlog: &window.backlog,
    });
    Ok(LcmPreflightResponse {
        status: "ok".to_string(),
        should_compress: decision.should_compress,
        reason: decision.reason.to_string(),
        replay_messages: canonical_replay_messages(&raw_messages),
    })
}

fn canonical_replay_messages(raw_messages: &[LcmRawMessage]) -> Vec<Value> {
    let replay = raw_messages
        .iter()
        .map(replay_transactions::raw_replay_message)
        .collect::<Vec<_>>();
    replay_transactions::normalize_replay_tool_pairs(&replay)
}

#[hotpath::measure(label = "sessions.lcm.compress", future = true)]
pub async fn compress(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    storage_root: &Path,
    request: LcmCompressionRequest,
    payload_rollback: &mut payload::PayloadFileRollback,
) -> Result<LcmCompressionResponse, LcmError> {
    let response = compress_inner(
        conn,
        publisher,
        storage_root,
        request,
        payload_rollback,
        None,
    )
    .await
    .map(|bounded| bounded.response);
    // A failed compression discarded its ingest writes, assembled backlog,
    // and summary drafts; success-only gauges would hide exactly that waste.
    if response.is_err() {
        crate::metrics::record_lcm_compress_failed();
    }
    response
}

/// Runs canonical compression over one bounded retained-session raw page.
///
/// This is the background-convergence entry point. Interactive compression
/// keeps its complete replay contract, while retained convergence advances the
/// same lifecycle CAS without materializing a mega-session in one future.
pub async fn compress_retained_page(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    storage_root: &Path,
    request: LcmCompressionRequest,
    payload_rollback: &mut payload::PayloadFileRollback,
    guard: RetainedCompressionGuard,
) -> Result<super::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
    let response = compress_inner(
        conn,
        publisher,
        storage_root,
        request,
        payload_rollback,
        Some(guard),
    )
    .await;
    if response.is_err() {
        crate::metrics::record_lcm_compress_failed();
    }
    response
}

async fn compress_inner(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    storage_root: &Path,
    request: LcmCompressionRequest,
    payload_rollback: &mut payload::PayloadFileRollback,
    retained_scan: Option<RetainedCompressionGuard>,
) -> Result<super::summary_convergence::LcmBoundedCompressionResponse, LcmError> {
    let mut request = request;
    request.max_assembly_tokens =
        compression_decision::effective_assembly_token_cap(AssemblyCapInput {
            max_assembly_tokens: request.max_assembly_tokens,
            context_length: request.context_length,
            reserve_tokens_floor: request.reserve_tokens_floor,
        });
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
        return Ok(super::summary_convergence::LcmBoundedCompressionResponse {
            response: record_compression_gauges(compression_response(
                "ok",
                reason,
                Vec::new(),
                request.messages,
                frontier,
                None,
                request.max_assembly_tokens,
            )),
            rows_scanned: 0,
            bytes_scanned: 0,
            has_more: false,
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
        payload_rollback,
    )
    .await?;

    let summarizer = CompressionSummarizerAdapter::from_mode(request.summarizer.clone());

    if summarizer.is_noop() {
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
        return Ok(super::summary_convergence::LcmBoundedCompressionResponse {
            response: record_compression_gauges(response),
            rows_scanned: 0,
            bytes_scanned: 0,
            has_more: false,
        });
    }

    let (response, rows_scanned, bytes_scanned, has_more) =
        compress_in_transaction(conn, publisher, request, &summarizer, retained_scan).await?;
    Ok(super::summary_convergence::LcmBoundedCompressionResponse {
        response: record_compression_gauges(response),
        rows_scanned,
        bytes_scanned,
        has_more,
    })
}

fn record_compression_gauges(response: LcmCompressionResponse) -> LcmCompressionResponse {
    crate::metrics::record_lcm_compression(
        response.summary_nodes_created,
        response.compression_attempts,
        response.replay_token_estimate,
    );
    response
}

async fn compress_in_transaction(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    request: LcmCompressionRequest,
    summarizer: &CompressionSummarizerAdapter,
    retained_scan: Option<RetainedCompressionGuard>,
) -> Result<(LcmCompressionResponse, usize, u64, bool), LcmError> {
    let expected_summary_source_range = retained_scan
        .as_ref()
        .and_then(|guard| guard.expected_summary_source_range.as_ref())
        .cloned();
    let context = prepare_compression_context(conn, &request, retained_scan).await?;
    if let Some(expected) = &expected_summary_source_range {
        let actual_from = context
            .plan
            .selected_backlog
            .first()
            .map(|message| message.store_id);
        let actual_to = context
            .plan
            .selected_backlog
            .last()
            .map(|message| message.store_id);
        if actual_from != Some(expected.from_store_id) || actual_to != Some(expected.to_store_id) {
            return Err(LcmError::StaleSummarySourceRange {
                expected_from: expected.from_store_id,
                expected_to: expected.to_store_id,
                actual_from,
                actual_to,
            });
        }
    }
    let scan = (
        context.raw_rows_scanned,
        context.raw_bytes_scanned,
        context.raw_has_more,
    );
    if let Some(response) = frontier_changed_response(&request, &context) {
        return Ok((response, scan.0, scan.1, scan.2));
    }
    if let Some(response) =
        no_backlog_compression_response(conn, publisher, &request, summarizer, &context).await?
    {
        return Ok((response, scan.0, scan.1, scan.2));
    }
    if let Some(response) = backlog_below_threshold_response(conn, &request, &context).await? {
        return Ok((response, scan.0, scan.1, scan.2));
    }
    if let Some(response) = auxiliary_summary_response(&request, summarizer, &context) {
        return Ok((response, scan.0, scan.1, scan.2));
    }

    let response =
        persist_and_replay_backlog_compression(conn, publisher, request, summarizer, context)
            .await?;
    Ok((response, scan.0, scan.1, scan.2))
}

// The read phase: whole-session raw load plus window/plan derivation.
// Together with `ingest_active`, `persist`, and `assemble_replay` this splits
// the inclusive `compress` span into its sequential phases.
#[hotpath::measure(label = "sessions.lcm.compress.prepare", future = true)]
async fn prepare_compression_context(
    conn: &impl QueryExecutor,
    request: &LcmCompressionRequest,
    retained_scan: Option<RetainedCompressionGuard>,
) -> Result<CompressionTransactionContext, LcmError> {
    let conversation_id = request.session_id.clone();
    let retained = retained_scan.is_some();
    let existing_frontier = lifecycle_state_or_default(
        conn,
        &request.provider,
        &conversation_id,
        &request.session_id,
    )
    .await?;
    let (raw_messages, raw_rows_scanned, raw_bytes_scanned, raw_has_more) =
        if let Some(limit) = retained_scan {
            let page = load_raw_messages_for_session_page(
                conn,
                &request.provider,
                &request.session_id,
                existing_frontier.current_frontier_store_id.unwrap_or(0),
                limit,
            )
            .await?;
            (
                page.messages,
                page.rows_scanned,
                page.bytes_scanned,
                page.has_more,
            )
        } else {
            let messages =
                load_raw_messages_for_session(conn, &request.provider, &request.session_id).await?;
            let rows_scanned = messages.len();
            (messages, rows_scanned, 0, false)
        };
    let window = compression_window(
        &raw_messages,
        existing_frontier.current_frontier_store_id,
        request.fresh_tail_count,
        request.current_tokens,
        request.threshold_tokens,
    );
    let plan = compression_decision::compression_plan(CompressionPlanInput {
        request,
        backlog: &window.backlog,
    });
    let overflow_assembly_cap =
        compression_decision::overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
            current_tokens: request.current_tokens,
            max_assembly_tokens: request.max_assembly_tokens,
            messages: &request.messages,
        });

    Ok(CompressionTransactionContext {
        conversation_id,
        existing_frontier,
        raw_messages,
        window,
        plan,
        overflow_assembly_cap,
        raw_rows_scanned,
        raw_bytes_scanned,
        raw_has_more,
        retained,
    })
}

fn frontier_changed_response(
    request: &LcmCompressionRequest,
    context: &CompressionTransactionContext,
) -> Option<LcmCompressionResponse> {
    let expected_frontier = request.expected_current_frontier_store_id?;
    if context
        .existing_frontier
        .current_frontier_store_id
        .unwrap_or(0)
        == expected_frontier
    {
        return None;
    }

    let replay_messages =
        replay_without_summary(&context.window.pinned_anchors, &context.window.fresh_tail);
    Some(compression_response(
        "ok",
        "frontier_changed",
        Vec::new(),
        replay_messages,
        context.existing_frontier.clone(),
        None,
        request.max_assembly_tokens,
    ))
}

async fn no_backlog_compression_response(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    request: &LcmCompressionRequest,
    summarizer: &CompressionSummarizerAdapter,
    context: &CompressionTransactionContext,
) -> Result<Option<LcmCompressionResponse>, LcmError> {
    if !context.window.backlog.is_empty() {
        return Ok(None);
    }
    if context.retained {
        return Ok(Some(compression_response(
            "ok",
            "no_backlog_to_compress",
            Vec::new(),
            retained_replay_messages(
                &context.window.pinned_anchors,
                &[],
                &context.window.fresh_tail,
            ),
            context.existing_frontier.clone(),
            None,
            request.max_assembly_tokens,
        )));
    }
    if context.plan.forced_overflow_recovery {
        return Ok(Some(
            overflow_recovery_no_backlog_response(conn, request, context).await?,
        ));
    }
    if let Some(response) = condense_summary_nodes_if_ready(
        conn,
        publisher,
        request,
        summarizer,
        &context.conversation_id,
        &context.existing_frontier,
        &context.window,
        &context.raw_messages,
    )
    .await?
    {
        return Ok(Some(response));
    }

    let replay_messages = assemble_replay_context(
        conn,
        &request.provider,
        &request.session_id,
        &context.raw_messages,
        ReplayWindowParts {
            pinned_anchors: &context.window.pinned_anchors,
            deferred_backlog: &[],
            fresh_tail: &context.window.fresh_tail,
        },
        request.max_assembly_tokens,
    )
    .await?;
    Ok(Some(compression_response(
        "ok",
        "no_backlog_to_compress",
        Vec::new(),
        replay_messages,
        context.existing_frontier.clone(),
        None,
        request.max_assembly_tokens,
    )))
}

async fn overflow_recovery_no_backlog_response(
    conn: &impl QueryExecutor,
    request: &LcmCompressionRequest,
    context: &CompressionTransactionContext,
) -> Result<LcmCompressionResponse, LcmError> {
    // Mirrors hermes-lcm `_assemble_overflow_recovery_context`: without
    // backlog to compact, recover by evicting droppable active-context
    // messages under the cap instead of returning the overflowing context
    // unchanged.
    let replay_messages = assemble_overflow_recovery_replay(
        conn,
        &request.provider,
        &request.session_id,
        &context.raw_messages,
        ReplayWindowParts {
            pinned_anchors: &context.window.pinned_anchors,
            deferred_backlog: &[],
            fresh_tail: &context.window.fresh_tail,
        },
        context.overflow_assembly_cap,
    )
    .await?;
    let over_budget = replay_exceeds_budget(
        replay_token_estimate(&replay_messages),
        context.overflow_assembly_cap,
    );
    let (status, reason) = if over_budget {
        ("best_effort", "irreducible_overflow_no_backlog")
    } else {
        ("ok", "overflow_recovery_no_backlog")
    };

    Ok(compression_response(
        status,
        reason,
        Vec::new(),
        replay_messages,
        context.existing_frontier.clone(),
        None,
        context.overflow_assembly_cap,
    ))
}

async fn backlog_below_threshold_response(
    conn: &impl QueryExecutor,
    request: &LcmCompressionRequest,
    context: &CompressionTransactionContext,
) -> Result<Option<LcmCompressionResponse>, LcmError> {
    // Mirrors hermes-lcm `compress()`: a threshold-style request no-ops when
    // the raw backlog outside the fresh tail is strictly below the working
    // leaf chunk threshold. Forced overflow recovery and outstanding
    // maintenance debt bypass the guard, matching Hermes' `force_overflow`
    // and deferred-maintenance escape hatches.
    if context.plan.forced_overflow_recovery
        || compression_decision::frontier_has_maintenance_debt(&context.existing_frontier)
        || compression_decision::has_eligible_backlog(
            &context.window.backlog,
            context.plan.leaf_chunk_tokens,
        )
    {
        return Ok(None);
    }

    let replay_messages = if context.retained {
        retained_replay_messages(
            &context.window.pinned_anchors,
            &context.window.backlog,
            &context.window.fresh_tail,
        )
    } else {
        assemble_replay_context(
            conn,
            &request.provider,
            &request.session_id,
            &context.raw_messages,
            ReplayWindowParts {
                pinned_anchors: &context.window.pinned_anchors,
                deferred_backlog: &context.window.backlog,
                fresh_tail: &context.window.fresh_tail,
            },
            request.max_assembly_tokens,
        )
        .await?
    };
    Ok(Some(compression_response(
        "ok",
        "backlog_below_leaf_chunk_threshold",
        Vec::new(),
        replay_messages,
        context.existing_frontier.clone(),
        None,
        request.max_assembly_tokens,
    )))
}

fn auxiliary_summary_response(
    request: &LcmCompressionRequest,
    summarizer: &CompressionSummarizerAdapter,
    context: &CompressionTransactionContext,
) -> Option<LcmCompressionResponse> {
    let summary_request = summarizer.summary_request(
        &request.provider,
        &request.session_id,
        request.focus_topic.clone(),
        &context.plan.selected_backlog,
    )?;
    let replay_messages =
        replay_without_summary(&context.window.pinned_anchors, &context.window.fresh_tail);

    Some(compression_response(
        "needs_summary",
        "hermes_auxiliary_not_available",
        Vec::new(),
        replay_messages,
        context.existing_frontier.clone(),
        Some(summary_request),
        request.max_assembly_tokens,
    ))
}

async fn persist_and_replay_backlog_compression(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    request: LcmCompressionRequest,
    summarizer: &CompressionSummarizerAdapter,
    context: CompressionTransactionContext,
) -> Result<LcmCompressionResponse, LcmError> {
    let Some(summary_invocation) = summarizer.persisted_summary_invocation() else {
        return Err(LcmError::Db(
            "persisted summarizer required after noop/auxiliary short-circuits".to_string(),
        ));
    };
    let write_result = persist_compression_transaction_writes(
        conn,
        publisher,
        CompressionTransactionWriteRequest {
            request: &request,
            conversation_id: &context.conversation_id,
            existing_frontier: &context.existing_frontier,
            summary_text: &summary_invocation.summary_text,
            route: summary_invocation.route.clone(),
            extraction_result: summary_invocation.extraction_result.clone(),
            backlog: &context.window.backlog,
            forced_overflow_recovery: context.plan.forced_overflow_recovery,
        },
    )
    .await?;
    // The summaries created above are already persisted in this transaction,
    // so the shared assembler replays them together with any earlier
    // uncondensed summary history (hermes-lcm `_assemble_context`).
    let replay_parts = ReplayWindowParts {
        pinned_anchors: &context.window.pinned_anchors,
        deferred_backlog: &write_result.remaining_backlog,
        fresh_tail: &context.window.fresh_tail,
    };
    let replay_messages = if context.retained {
        retained_replay_messages(
            &context.window.pinned_anchors,
            &write_result.remaining_backlog,
            &context.window.fresh_tail,
        )
    } else if context.plan.forced_overflow_recovery {
        assemble_overflow_recovery_replay(
            conn,
            &request.provider,
            &request.session_id,
            &context.raw_messages,
            replay_parts,
            context.overflow_assembly_cap,
        )
        .await?
    } else {
        assemble_replay_context(
            conn,
            &request.provider,
            &request.session_id,
            &context.raw_messages,
            replay_parts,
            request.max_assembly_tokens,
        )
        .await?
    };
    let mut status = "ok";
    let mut reason = if context.plan.forced_overflow_recovery {
        "forced_overflow_recovery"
    } else {
        "compressed_backlog"
    };
    let replay_token_estimate = replay_token_estimate(&replay_messages);
    if context.plan.forced_overflow_recovery
        && replay_exceeds_budget(replay_token_estimate, context.overflow_assembly_cap)
    {
        status = "best_effort";
        reason = "forced_overflow_recovery_replay_over_budget";
    }
    let compression_attempts = write_result.created_summaries.len();
    let summary_nodes = write_result.created_summaries;

    let retry_status = context
        .plan
        .forced_overflow_recovery
        .then_some("critical_pressure_catch_up");

    Ok(compression_response_with_attempt_state(
        CompressionResponseParts {
            status,
            reason,
            summary_nodes,
            replay_messages,
            frontier: write_result.frontier,
            summary_request: None,
            max_assembly_tokens: if context.plan.forced_overflow_recovery {
                context.overflow_assembly_cap
            } else {
                request.max_assembly_tokens
            },
        },
        CompressionAttemptState {
            compression_attempts,
            retry_status,
        },
    ))
}

// The summary-publication transaction: chunk selection, immutable summary
// publication, and the lifecycle/debt writes that commit the new frontier.
#[hotpath::measure(label = "sessions.lcm.compress.persist", future = true)]
async fn persist_compression_transaction_writes(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    write: CompressionTransactionWriteRequest<'_>,
) -> Result<CompressionTransactionWriteResult, LcmError> {
    let pass_limit = if write.forced_overflow_recovery {
        MAX_FORCED_CATCHUP_PASSES
    } else {
        1
    };
    let mut remaining_backlog = write.backlog.to_vec();
    let mut created_summaries = Vec::new();
    let mut new_frontier = write.existing_frontier.current_frontier_store_id;

    while !remaining_backlog.is_empty() && created_summaries.len() < pass_limit {
        let leaf_chunk_tokens = compression_decision::effective_leaf_chunk_tokens(
            write.request.leaf_chunk_tokens,
            write.request.dynamic_leaf_chunk_enabled,
            write.request.dynamic_leaf_chunk_max,
            source_token_count(&remaining_backlog),
        );
        let selected_len = compression_decision::progress_leaf_chunk_len(
            &remaining_backlog,
            leaf_chunk_tokens,
            write.request.max_source_messages,
        );
        let selected_backlog = remaining_backlog[..selected_len].to_vec();

        let summary = dag::insert_summary_node(
            publisher,
            summary_draft(
                &write.request.provider,
                write.conversation_id,
                &write.request.session_id,
                write.summary_text,
                write.route.clone(),
                write.extraction_result.as_ref(),
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

        if !write.forced_overflow_recovery {
            break;
        }
    }

    let update = LcmLifecycleUpdate {
        provider: write.request.provider.clone(),
        conversation_id: write.conversation_id.to_string(),
        current_session_id: write.request.session_id.clone(),
        current_frontier_store_id: new_frontier,
        last_finalized_session_id: write.existing_frontier.last_finalized_session_id.clone(),
        last_finalized_frontier_store_id: write.existing_frontier.last_finalized_frontier_store_id,
        maintenance_debt: debt_for_deferred_backlog(&remaining_backlog),
    };
    upsert_lifecycle_state(conn, &update).await?;
    replace_maintenance_debt(
        conn,
        &update.provider,
        &update.conversation_id,
        &update.maintenance_debt,
    )
    .await?;

    Ok(CompressionTransactionWriteResult {
        created_summaries,
        frontier: lifecycle_state(conn, &update.provider, &update.conversation_id).await?,
        remaining_backlog,
    })
}

async fn upsert_lifecycle_state(
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl QueryExecutor,
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
    conn: &impl QueryExecutor,
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

fn compression_window(
    raw_messages: &[LcmRawMessage],
    current_frontier_store_id: Option<i64>,
    fresh_tail_count: Option<usize>,
    current_tokens: Option<i64>,
    threshold_tokens: Option<i64>,
) -> CompressionWindow {
    let frontier_store_id = current_frontier_store_id.unwrap_or(0);
    let unsummarized = raw_messages
        .iter()
        .filter(|message| message.store_id > frontier_store_id)
        .cloned()
        .collect::<Vec<_>>();
    let configured_fresh_tail_count = fresh_tail_count.unwrap_or(LCM_DEFAULT_FRESH_TAIL_COUNT);
    let effective_fresh_tail_count = if unsummarized.len() > 1
        && compression_decision::threshold_pressure(current_tokens, threshold_tokens)
    {
        configured_fresh_tail_count.min(unsummarized.len() - 1)
    } else {
        configured_fresh_tail_count
    };
    let backlog_len = unsummarized
        .len()
        .saturating_sub(effective_fresh_tail_count);
    let backlog_len = replay_transactions::atomic_tail_start(&unsummarized, backlog_len);
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

fn is_policy_anchor_role(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

fn replay_without_summary(
    pinned_anchors: &[LcmRawMessage],
    fresh_tail: &[LcmRawMessage],
) -> Vec<Value> {
    let mut replay_messages = Vec::with_capacity(pinned_anchors.len() + fresh_tail.len());
    replay_messages.extend(
        pinned_anchors
            .iter()
            .map(replay_transactions::raw_replay_message),
    );
    replay_messages.extend(
        fresh_tail
            .iter()
            .map(replay_transactions::raw_replay_message),
    );
    replay_transactions::normalize_replay_tool_pairs(&replay_messages)
}

fn retained_replay_messages(
    pinned_anchors: &[LcmRawMessage],
    deferred_backlog: &[LcmRawMessage],
    fresh_tail: &[LcmRawMessage],
) -> Vec<Value> {
    let replay = pinned_anchors
        .iter()
        .chain(deferred_backlog)
        .chain(fresh_tail)
        .map(replay_transactions::raw_replay_message)
        .collect::<Vec<_>>();
    replay_transactions::normalize_replay_tool_pairs(&replay)
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
// Replay assembly; its DAG-build share is the nested
// `sessions.lcm.dag.load_uncondensed` span.
#[hotpath::measure(label = "sessions.lcm.compress.assemble_replay", future = true)]
async fn assemble_replay_context(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    anchor_source: &[LcmRawMessage],
    parts: ReplayWindowParts<'_>,
    max_assembly_tokens: Option<i64>,
) -> Result<Vec<Value>, LcmError> {
    let summaries = hotpath::future!(
        dag::load_uncondensed_summary_nodes(conn, provider, session_id),
        label = "sessions.lcm.replay.fetch"
    )
    .await?;
    let (anchors, raws) = split_leading_anchors(&parts);
    Ok(hotpath::measure_block!("sessions.lcm.replay.compile", {
        assemble_replay_messages(
            &anchors,
            &summaries,
            &raws,
            anchor_source,
            max_assembly_tokens,
        )
    }))
}

/// Mirrors hermes-lcm `_assemble_overflow_recovery_context`: assemble under
/// the cap; when nothing beyond the anchors fits, fall back to anchors plus
/// the most recent message even if that stays over budget.
#[hotpath::measure(
    label = "sessions.lcm.compress.assemble_overflow_replay",
    future = true
)]
async fn assemble_overflow_recovery_replay(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    anchor_source: &[LcmRawMessage],
    parts: ReplayWindowParts<'_>,
    max_assembly_tokens: Option<i64>,
) -> Result<Vec<Value>, LcmError> {
    let summaries = hotpath::future!(
        dag::load_uncondensed_summary_nodes(conn, provider, session_id),
        label = "sessions.lcm.replay.fetch"
    )
    .await?;
    let (anchors, raws) = split_leading_anchors(&parts);
    let candidate = hotpath::measure_block!("sessions.lcm.replay.compile", {
        assemble_replay_messages(
            &anchors,
            &summaries,
            &raws,
            anchor_source,
            max_assembly_tokens,
        )
    });
    if candidate.len() == anchors.len()
        && let Some(last_unit) = replay_transactions::replay_units(&raws).last()
    {
        let mut replay = anchors
            .iter()
            .map(|message| replay_transactions::raw_replay_message(message))
            .collect::<Vec<_>>();
        replay.extend(
            last_unit
                .messages
                .iter()
                .map(|message| replay_transactions::raw_replay_message(message)),
        );
        return Ok(replay_transactions::normalize_replay_tool_pairs(&replay));
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
    anchor_source: &[LcmRawMessage],
    max_assembly_tokens: Option<i64>,
) -> Vec<Value> {
    let (selected_raws, selected_summaries, preserved_objective_anchor) = match max_assembly_tokens
    {
        None => (
            replay_transactions::replay_units(raws)
                .into_iter()
                .flat_map(|unit| unit.messages)
                .collect(),
            summaries.iter().collect::<Vec<_>>(),
            latest_user_context_anchor(anchor_source, raws),
        ),
        Some(cap) => {
            let used = anchors
                .iter()
                .map(|message| crate::lcm_budget_tokens(&message.content))
                .sum::<i64>();
            let (selected_raws, tail_tokens) = select_budget_tail(raws, used, cap);
            let mut summary_budget = (cap - used - tail_tokens).max(0);
            let preserved_objective_anchor =
                latest_user_context_anchor(anchor_source, &selected_raws).and_then(
                    |(store_id, part, already_preserved)| {
                        if already_preserved {
                            return Some((store_id, part, already_preserved));
                        }
                        let part_tokens = crate::lcm_budget_tokens(&part);
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
            replay_transactions::raw_replay_message(message),
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
            replay_transactions::raw_replay_message(message),
        )
    }));
    replay_items.sort_by_key(|(store_id, priority, _)| (*store_id, *priority));
    let replay = replay_items
        .into_iter()
        .map(|(_, _, message)| message)
        .collect::<Vec<_>>();
    replay_transactions::normalize_replay_tool_pairs(&replay)
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
    for unit in replay_transactions::replay_units(raws).iter().rev() {
        let unit_tokens = unit.token_count();
        if used + tail_tokens + unit_tokens > cap {
            if unit
                .messages
                .iter()
                .all(|message| is_budget_droppable_tail_message(message))
            {
                skipped_gap = true;
                continue;
            }
            break;
        }
        if skipped_gap {
            break;
        }
        kept_reversed.extend(unit.messages.iter().rev().copied());
        tail_tokens += unit_tokens;
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
    raws: &[LcmRawMessage],
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
        let summary_tokens = crate::lcm_budget_tokens(&summaries[idx].node.summary_text);
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
        retry_status,
    } = attempt_state;
    let replay_token_estimate = replay_token_estimate(&replay_messages);
    let context_recovery_hint = context_recovery_hint(&summary_nodes);
    // Persist has not applied the Grafeo projection yet. The guarded
    // commit settles `Pending` to `Applied` after apply succeeds.
    let relation_projection_status = if summary_nodes.is_empty() {
        LcmRelationProjectionStatus::NotApplicable
    } else {
        LcmRelationProjectionStatus::Pending
    };
    LcmCompressionResponse {
        status: status.to_string(),
        reason: reason.to_string(),
        summary_nodes_created: summary_nodes.len(),
        summary_nodes,
        replay_messages,
        replay_token_estimate,
        replay_over_budget: replay_exceeds_budget(replay_token_estimate, max_assembly_tokens),
        compression_attempts,
        fallback_used: false,
        context_recovery_hint,
        retry_status: retry_status.map(str::to_string),
        relation_projection_status,
        frontier,
        summary_request,
    }
}

fn context_recovery_hint(summary_nodes: &[LcmSummaryNode]) -> Option<String> {
    let summary = summary_nodes.first()?;
    Some(format!(
        "Compacted context is stored in TraceDecay LCM for provider '{}' session '{}'. {CONTEXT_RECOVERY_HINT_SUFFIX}",
        summary.provider, summary.session_id
    ))
}

fn replay_token_estimate(messages: &[Value]) -> i64 {
    messages.iter().map(crate::lcm_message_budget_tokens).sum()
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
    extraction_result: Option<&LcmExtractionResult>,
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
    let mut metadata = json!({
        "pre_compaction_extraction": extraction::summary_metadata_extraction(
            extraction_result,
            false,
        )
    });
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
        summary_token_count: crate::lcm_budget_tokens(summary_text),
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
        summary_token_count: crate::lcm_budget_tokens(summary_text),
        source_time_start,
        source_time_end,
        expand_hint: Some(format!("{} summary nodes", children.len())),
        metadata_json: Some(
            json!({
                "pre_compaction_extraction": extraction::summary_metadata_extraction(None, true)
            })
            .to_string(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn condense_summary_nodes_if_ready(
    conn: &impl Executor,
    publisher: &impl dag::LcmSummaryPublicationPort,
    request: &LcmCompressionRequest,
    summarizer: &CompressionSummarizerAdapter,
    conversation_id: &str,
    existing_frontier: &LcmLifecycleState,
    window: &CompressionWindow,
    raw_messages: &[LcmRawMessage],
) -> Result<Option<LcmCompressionResponse>, LcmError> {
    let CondensationDecision::QueryCandidates(policy) =
        compression_decision::condensation_policy_decision(CondensationDecisionInput {
            has_backlog: !window.backlog.is_empty(),
            summary_fan_in: request.summary_fan_in,
            incremental_max_depth: request.incremental_max_depth,
            summarizer,
        })
    else {
        return Ok(None);
    };
    let children = load_condensation_candidates(
        conn,
        &request.provider,
        &request.session_id,
        policy.fan_in,
        policy.incremental_max_depth,
    )
    .await?;
    let Some(summary_invocation) = summarizer.persisted_summary_invocation() else {
        return Err(LcmError::Db(
            "condensation policy only queries candidates for persisted summarizers".to_string(),
        ));
    };
    if matches!(
        compression_decision::condensation_candidate_decision(children.len(), policy.fan_in),
        CondensationCandidateDecision::SkipNotEnoughCandidates
    ) {
        return Ok(None);
    }

    let summary_text = summary_invocation.summary_text.clone();
    let summary = dag::insert_summary_node(
        publisher,
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
    // Mirrors hermes-lcm: `_assemble_context` always follows
    // `_maybe_condense`, so a condensation-only pass still returns the
    // assembled active context instead of an empty replay.
    let replay_messages = assemble_replay_context(
        conn,
        &request.provider,
        &request.session_id,
        raw_messages,
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
        "condensed_summary_nodes",
        vec![summary],
        replay_messages,
        frontier,
        None,
        request.max_assembly_tokens,
    )))
}

async fn load_condensation_candidates(
    conn: &impl QueryExecutor,
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
               JOIN session_temporal_generations generation
                 ON generation.session_id = n.session_id
                AND generation.state = 'active'
               JOIN session_summary_availability availability
                 ON availability.session_id = generation.session_id
                AND availability.generation = generation.generation
                AND availability.summary_id = n.node_id
                AND availability.availability = 'available'
               LEFT JOIN source_order ON source_order.node_id = n.node_id
               WHERE n.provider = ?1 AND n.session_id = ?2
                 AND NOT EXISTS (
                   SELECT 1
                   FROM lcm_summary_convergence_dirty_raw dirty
                   WHERE dirty.provider = n.provider
                     AND dirty.session_id = n.session_id
                 )
                 AND NOT EXISTS (
                   SELECT 1
                   FROM lcm_summary_sources s
                   JOIN session_summary_availability parent_availability
                     ON parent_availability.session_id = generation.session_id
                    AND parent_availability.generation = generation.generation
                    AND parent_availability.summary_id = s.node_id
                    AND parent_availability.availability = 'available'
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

// Active-context ingest: sanitization, payload externalization, and raw
// upserts. This is compression's write-side file I/O + CPU phase, distinct
// from the summary-publication transaction in `persist`.
#[hotpath::measure(label = "sessions.lcm.compress.ingest_active", future = true)]
async fn ingest_active_messages(
    conn: &impl Executor,
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    messages: &[Value],
    ignore_message_patterns: &[String],
    payload_rollback: &mut payload::PayloadFileRollback,
) -> Result<IngestedActiveMessages, LcmError> {
    let (mut next_available_ordinal, prepared, prefetched_states) = hotpath::future!(
        async {
            let next_available_ordinal = next_ordinal(conn, provider, session_id).await?;
            let compiled_ignore_patterns =
                security::compile_message_patterns(ignore_message_patterns);
            let prepared = prepare_active_messages(
                conn,
                provider,
                session_id,
                messages,
                &compiled_ignore_patterns,
            )
            .await?;
            let prefetched_message_ids = prepared
                .iter()
                .filter_map(|prepared| match prepared {
                    PreparedActiveMessage::ReplayVerbatim { .. } => None,
                    PreparedActiveMessage::Ingest { message_id, .. } => message_id.clone(),
                })
                .collect::<Vec<_>>();
            let prefetched_states =
                existing_active_message_states(conn, provider, &prefetched_message_ids).await?;
            Ok::<_, LcmError>((next_available_ordinal, prepared, prefetched_states))
        },
        label = "sessions.lcm.compress.ingest.fetch"
    )
    .await?;
    // Message ids written by an earlier iteration are re-read from the
    // database so a repeated id still sees the row this loop just wrote.
    let replay_messages = hotpath::future!(
        async {
            let mut replay_messages = Vec::with_capacity(messages.len());
            let mut rewritten_message_ids = HashSet::new();
            for prepared in prepared {
                let (source_index, role, original_content, storage_text, message_id) =
                    match prepared {
                        PreparedActiveMessage::ReplayVerbatim { source_index } => {
                            replay_messages.push(messages[source_index].clone());
                            continue;
                        }
                        PreparedActiveMessage::Ingest {
                            source_index,
                            role,
                            original_content,
                            storage_text,
                            message_id,
                        } => (
                            source_index,
                            role,
                            original_content,
                            storage_text,
                            message_id,
                        ),
                    };
                let message = &messages[source_index];
                let Some(message_id) = message_id else {
                    let mut replay = message.clone();
                    replay["role"] = Value::String(role);
                    replay_messages.push(replay);
                    continue;
                };
                let rewritten_state;
                let existing_state = if rewritten_message_ids.contains(&message_id) {
                    rewritten_state =
                        existing_active_message_state(conn, provider, &message_id).await?;
                    rewritten_state.as_ref()
                } else {
                    prefetched_states.get(&message_id)
                };
                let ordinal = if let Some(existing) = existing_state {
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
                let expected_content_hash = projected_content_hash(&storage_text);
                if let Some(existing) = existing_state {
                    let matches_stored_row = existing.ordinal == ordinal
                        && existing.content_hash == expected_content_hash
                        && existing.metadata_json.as_deref()
                            == Some(initial_metadata_json.as_str())
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
                let upsert = raw::upsert_raw_message_with_payload_tracked(
                    conn,
                    storage_root,
                    &record,
                    payload_rollback,
                )
                .await?;
                rewritten_message_ids.insert(message_id.clone());
                let raw = super::schema::load_raw_message(conn, provider, &message_id)
                    .await?
                    .ok_or_else(|| LcmError::Db("active message did not persist".to_string()))?;
                let replay_content =
                    replay_content_value(&original_content, &raw, upsert.projection_text.as_str());
                replay["content"] = replay_content;
                if let Some(tool_calls) = replay.get("tool_calls").cloned() {
                    let protected_tool_calls = raw::protect_replay_field_value_tracked(
                        conn,
                        storage_root,
                        &record,
                        "tool_calls",
                        &tool_calls,
                        payload_rollback,
                    )
                    .await?;
                    if protected_tool_calls != tool_calls {
                        replay["tool_calls"] = protected_tool_calls;
                    }
                }
                let metadata_json = active_replay_metadata_json(
                    upsert.projection_metadata_json.as_deref(),
                    &replay,
                );
                if metadata_json != initial_metadata_json {
                    update_active_replay_metadata(conn, provider, &message_id, &metadata_json)
                        .await?;
                }
                replay_messages.push(replay);
            }
            Ok::<_, LcmError>(replay_messages)
        },
        label = "sessions.lcm.compress.ingest.persist"
    )
    .await?;

    Ok(IngestedActiveMessages { replay_messages })
}

/// Resolves role, content, and message id for every ingest candidate up front
/// so the ingest loop performs in-memory lookups instead of one query per
/// message.
async fn prepare_active_messages(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    messages: &[Value],
    compiled_ignore_patterns: &security::CompiledPatternSet,
) -> Result<Vec<PreparedActiveMessage>, LcmError> {
    enum DraftActiveMessage {
        ReplayVerbatim {
            source_index: usize,
        },
        Ingest {
            source_index: usize,
            role: String,
            original_content: Value,
            storage_text: String,
            replay_as_is: bool,
            explicit_message_id: Option<String>,
            store_id: Option<i64>,
        },
    }

    let mut drafts = Vec::with_capacity(messages.len());
    let mut lookup_store_ids = Vec::new();
    for (source_index, message) in messages.iter().enumerate() {
        let Some(role) = active_message_role(message) else {
            drafts.push(DraftActiveMessage::ReplayVerbatim { source_index });
            continue;
        };
        let role = role.to_string();
        let original_content = message_content_value(message);
        let storage_text = message_storage_text(&original_content);
        let search_text = crate::lcm_message_visible_text(message);
        let replay_as_is = message
            .get("lcm_summary_node_id")
            .and_then(Value::as_str)
            .is_some_and(|node_id| !node_id.is_empty())
            || security::ignore_message_reason_with_compiled(
                &search_text,
                compiled_ignore_patterns,
            )
            .is_some();
        let explicit_message_id = (!replay_as_is)
            .then(|| {
                message
                    .get("id")
                    .or_else(|| message.get("message_id"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .flatten();
        let store_id = (!replay_as_is && explicit_message_id.is_none())
            .then(|| message.get("store_id").and_then(Value::as_i64))
            .flatten();
        if let Some(store_id) = store_id {
            lookup_store_ids.push(store_id);
        }
        drafts.push(DraftActiveMessage::Ingest {
            source_index,
            role,
            original_content,
            storage_text,
            replay_as_is,
            explicit_message_id,
            store_id,
        });
    }

    let stored_message_ids =
        message_ids_for_store_ids(conn, provider, session_id, &lookup_store_ids).await?;
    let mut prepared = Vec::with_capacity(drafts.len());
    for draft in drafts {
        match draft {
            DraftActiveMessage::ReplayVerbatim { source_index } => {
                prepared.push(PreparedActiveMessage::ReplayVerbatim { source_index });
            }
            DraftActiveMessage::Ingest {
                source_index,
                role,
                original_content,
                storage_text,
                replay_as_is,
                explicit_message_id,
                store_id,
            } => {
                let message_id = (!replay_as_is).then(|| {
                    explicit_message_id
                        .or_else(|| {
                            store_id.and_then(|store_id| stored_message_ids.get(&store_id).cloned())
                        })
                        .unwrap_or_else(|| {
                            deterministic_message_id(
                                provider,
                                session_id,
                                source_index,
                                &role,
                                &storage_text,
                            )
                        })
                });
                prepared.push(PreparedActiveMessage::Ingest {
                    source_index,
                    role,
                    original_content,
                    storage_text,
                    message_id,
                });
            }
        }
    }
    Ok(prepared)
}

async fn message_ids_for_store_ids(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    store_ids: &[i64],
) -> Result<HashMap<i64, String>, LcmError> {
    let mut message_ids = HashMap::new();
    for chunk in store_ids.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = util::sql_in_placeholders(chunk.len());
        let sql = format!(
            "SELECT store_id, message_id
             FROM lcm_raw_messages
             WHERE provider = ? AND session_id = ? AND store_id IN ({placeholders})"
        );
        let mut values = vec![
            SqlValue::Text(provider.to_string()),
            SqlValue::Text(session_id.to_string()),
        ];
        values.extend(chunk.iter().copied().map(SqlValue::Integer));
        let mut rows = conn.query(&sql, values).await?;
        while let Some(row) = rows.next().await? {
            message_ids.insert(row.get(0)?, row.get(1)?);
        }
    }
    Ok(message_ids)
}

/// Stored LCM rows require a real role. Missing or empty role is a typed skip
/// — never a fabricated `"user"` that would persist and enter identity hashes.
fn active_message_role(message: &Value) -> Option<&str> {
    message
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
}

fn message_content_value(message: &Value) -> Value {
    message
        .get("content")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()))
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
    metadata.insert(
        replay_transactions::ACTIVE_REPLAY_METADATA_KEY.to_string(),
        Value::Bool(true),
    );
    metadata.insert(
        replay_transactions::ACTIVE_REPLAY_MESSAGE_KEY.to_string(),
        active_replay_for_metadata(replay),
    );
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
    metadata.insert(
        replay_transactions::ACTIVE_REPLAY_METADATA_KEY.to_string(),
        Value::Bool(true),
    );
    metadata.insert(
        replay_transactions::ACTIVE_REPLAY_MESSAGE_KEY.to_string(),
        active_replay_for_metadata(replay),
    );
    Value::Object(metadata).to_string()
}

fn active_replay_for_metadata(replay: &Value) -> Value {
    let mut replay = replay.clone();
    if let Some(object) = replay.as_object_mut() {
        replay_transactions::strip_disposable_assistant_replay_sidecars(object, "");
    }
    replay
}

async fn update_active_replay_metadata(
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl QueryExecutor,
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

/// Batch form of [`existing_active_message_state`] for the ingest prefetch.
async fn existing_active_message_states(
    conn: &impl QueryExecutor,
    provider: &str,
    message_ids: &[String],
) -> Result<HashMap<String, ExistingActiveMessageState>, LcmError> {
    let mut states = HashMap::new();
    for chunk in message_ids.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = util::sql_in_placeholders(chunk.len());
        let sql = format!(
            "SELECT message_id, session_id, role, timestamp, ordinal, content_hash, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ? AND message_id IN ({placeholders})"
        );
        let mut values = vec![SqlValue::Text(provider.to_string())];
        values.extend(chunk.iter().cloned().map(SqlValue::Text));
        let mut rows = conn.query(&sql, values).await?;
        while let Some(row) = rows.next().await? {
            states.insert(
                row.get(0)?,
                ExistingActiveMessageState {
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    timestamp: row.get(3)?,
                    ordinal: row.get(4)?,
                    content_hash: row.get(5)?,
                    metadata_json: row.get(6)?,
                },
            );
        }
    }
    Ok(states)
}

async fn next_ordinal(
    conn: &impl QueryExecutor,
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
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessage>, LcmError> {
    let fetched = hotpath::future!(
        async {
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
            let mut fetched = Vec::new();
            while let Some(row) = rows.next().await? {
                fetched.push(row);
            }
            Ok::<_, LcmError>(fetched)
        },
        label = "sessions.lcm.hydrate.fetch"
    )
    .await?;
    hotpath::measure_block!("sessions.lcm.hydrate.redact", {
        fetched
            .iter()
            .map(raw::verified_raw_message_from_row)
            .collect::<Result<Vec<_>, _>>()
    })
}

struct RetainedRawMessagePage {
    messages: Vec<LcmRawMessage>,
    rows_scanned: usize,
    bytes_scanned: u64,
    has_more: bool,
}

async fn load_raw_messages_for_session_page(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    after_store_id: i64,
    limit: RetainedCompressionGuard,
) -> Result<RetainedRawMessagePage, LcmError> {
    let row_limit = i64::try_from(limit.row_limit.max(1))
        .map_err(|_| LcmError::Db("retained compression row limit overflow".to_string()))?;
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json,
                    length(CAST(COALESCE(content, '') AS BLOB))
                      + length(CAST(snippet_text AS BLOB))
                      + length(CAST(index_text AS BLOB))
                      + length(CAST(COALESCE(metadata_json, '') AS BLOB))
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2 AND store_id > ?3
             ORDER BY store_id
             LIMIT ?4",
            params![provider, session_id, after_store_id, row_limit],
        )
        .await?;
    let mut messages = Vec::new();
    let mut bytes_scanned = 0_u64;
    let mut byte_limited = false;
    while let Some(row) = rows.next().await? {
        let row_bytes = u64::try_from(row.get::<i64>(15)?).map_err(|error| {
            LcmError::Db(format!("invalid retained compression byte count: {error}"))
        })?;
        if bytes_scanned.saturating_add(row_bytes) > limit.byte_limit {
            if messages.is_empty() {
                return Err(LcmError::BudgetExhausted);
            }
            byte_limited = true;
            break;
        }
        bytes_scanned = bytes_scanned.saturating_add(row_bytes);
        messages.push(raw::verified_raw_message_from_row(&row)?);
    }
    let rows_scanned = messages.len();
    Ok(RetainedRawMessagePage {
        messages,
        rows_scanned,
        bytes_scanned,
        has_more: byte_limited || rows_scanned == limit.row_limit.max(1),
    })
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
        projected_content_hash(&format!(
            "{provider}\0{session_id}\0{idx}\0{role}\0{content}"
        ))
    )
}

fn summary_replay_message(summary: &LcmSummaryNode) -> Value {
    json!({
        "role": "system",
        "content": summary.summary_text,
        "lcm_summary_node_id": summary.node_id,
    })
}

fn source_token_count(backlog: &[LcmRawMessage]) -> i64 {
    backlog
        .iter()
        .map(|message| crate::lcm_budget_tokens(&message.content))
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

#[cfg(test)]
mod authority_tests {
    use super::*;
    use crate::{LcmSummarizerMode, schema};

    #[test]
    fn missing_or_empty_role_is_a_typed_skip() {
        assert_eq!(active_message_role(&json!({"content": "hi"})), None);
        assert_eq!(
            active_message_role(&json!({"role": "", "content": "hi"})),
            None
        );
        assert_eq!(
            active_message_role(&json!({"role": "assistant", "content": "hi"})),
            Some("assistant")
        );
    }

    /// A message without a real role is never stored (a fabricated role would
    /// enter identity hashes) but must not vanish from the replay output.
    #[tokio::test]
    async fn role_less_message_replays_verbatim_without_storage() {
        let temp = tempfile::TempDir::new().expect("create lcm tempdir");
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("sessions.db"),
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                PRIMARY KEY(provider, session_id)
            );",
        )
        .await
        .expect("create session table");
        schema::ensure_lcm_schema(&conn)
            .await
            .expect("ensure lcm schema");
        conn.execute_batch(
            "INSERT INTO sessions (provider, session_id, project_key, project_path, title, started_at)
             VALUES ('cursor', 'session-role-skip', 'fixture', 'fixture', 'fixture', 1);",
        )
        .await
        .expect("insert fixture session");
        let messages = vec![
            json!({"role": "user", "content": "first"}),
            json!({"content": "attachment notice without a role"}),
            json!({"role": "assistant", "content": "second"}),
        ];
        let mut rollback = payload::PayloadFileRollback::begin_cancellation_safe(temp.path());
        let ingested = ingest_active_messages(
            &conn,
            temp.path(),
            "cursor",
            "session-role-skip",
            &messages,
            &[],
            &mut rollback,
        )
        .await
        .expect("ingest active messages");
        assert_eq!(
            ingested.replay_messages.len(),
            3,
            "role-less message must stay in the replay output"
        );
        assert_eq!(
            ingested.replay_messages[1], messages[1],
            "role-less message must replay verbatim"
        );
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages WHERE provider = ? AND session_id = ?",
                params!["cursor", "session-role-skip"],
            )
            .await
            .expect("count stored raw rows");
        let row = rows
            .next()
            .await
            .expect("read count row")
            .expect("count row present");
        let stored: i64 = row.get(0).expect("count value");
        assert_eq!(stored, 2, "role-less message must not be stored");
    }

    #[tokio::test]
    async fn condensation_uses_only_available_nodes_from_the_active_generation() {
        let temp = tempfile::TempDir::new().expect("create lcm tempdir");
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("sessions.db"),
        );
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'active-condensation', 'fixture', 'fixture');",
        )
        .await
        .unwrap();
        schema::ensure_lcm_schema(&conn).await.unwrap();
        conn.execute_batch(
            "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL
             );
             CREATE TABLE session_summary_availability (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                summary_id TEXT NOT NULL,
                availability TEXT NOT NULL
             );
             INSERT INTO session_temporal_generations(session_id, generation, state)
             VALUES ('active-condensation', 2, 'active');
             INSERT INTO lcm_summary_nodes(
                node_id, provider, conversation_id, session_id, depth,
                summary_text, summary_hash, summary_token_count, source_token_count,
                created_at
             ) VALUES
                ('current', 'cursor', 'active-condensation', 'active-condensation', 0,
                 'current summary', 'current-hash', 2, 4, 1),
                ('stale-parent', 'cursor', 'active-condensation', 'active-condensation', 1,
                 'stale summary', 'stale-hash', 2, 4, 2);
             INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal) VALUES
                ('current', 'raw_message', '1', 0),
                ('stale-parent', 'summary_node', 'current', 0);
             INSERT INTO session_summary_availability(
                session_id, generation, summary_id, availability
             ) VALUES
                ('active-condensation', 2, 'current', 'available'),
                ('active-condensation', 2, 'stale-parent', 'stale');",
        )
        .await
        .unwrap();

        let candidates = load_condensation_candidates(&conn, "cursor", "active-condensation", 1, 8)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].node_id, "current");
        assert_eq!(candidates[0].summary_text, "current summary");
    }

    #[test]
    fn replay_budget_matches_policy_for_object_with_text() {
        let message = json!({
            "content": {
                "extra": "ignored key words",
                "text": "one",
            }
        });
        assert_eq!(replay_token_estimate(std::slice::from_ref(&message)), 1);
        assert_eq!(
            replay_token_estimate(std::slice::from_ref(&message)),
            crate::lcm_message_budget_tokens(&message)
        );
    }

    #[test]
    fn replay_budget_matches_policy_for_array_of_text_parts() {
        let message = json!({
            "content": [
                { "extra": "ignored key words", "text": "one" },
                { "text": "two three" },
            ]
        });
        assert_eq!(replay_token_estimate(std::slice::from_ref(&message)), 3);
        assert_eq!(
            replay_token_estimate(std::slice::from_ref(&message)),
            crate::lcm_message_budget_tokens(&message)
        );
    }

    #[test]
    fn authoritative_summary_text_is_never_replaced_by_an_extractive_fallback() {
        let summary = "Exact native host summary. ".repeat(400);
        let adapter = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Provided {
            summary_text: summary.clone(),
            route: Some("native_host".to_string()),
        });

        let invocation = adapter
            .persisted_summary_invocation()
            .expect("non-empty authoritative summary should persist");
        assert_eq!(invocation.summary_text, summary);
    }
}
