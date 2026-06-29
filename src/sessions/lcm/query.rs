use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use libsql::{params, Connection, Value};

use crate::tracedecay::current_timestamp;

use super::types::{LcmLifecycleStatus, LcmPayloadGcStatus, LcmPayloadStatus, LcmRedactionStatus};
use super::{
    compression, dag, gc, payload, raw, schema, util, LcmConfigStatus, LcmContentRange,
    LcmContentSlice, LcmDagDepthStatus, LcmDagStatus, LcmDescribeExternalPayload,
    LcmDescribeRequest, LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode,
    LcmDescribeTarget, LcmError, LcmExpandQueryBudget, LcmExpandQueryContextBlock,
    LcmExpandQueryMatch, LcmExpandQueryPagination, LcmExpandQueryRequest, LcmExpandQueryResponse,
    LcmExpandQuerySynthesisPrompt, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination,
    LcmExpandTarget, LcmExpandedSummarySource, LcmGcConfig, LcmGrepHit, LcmGrepRequest,
    LcmGrepSort, LcmLoadSessionMessage, LcmLoadSessionPage, LcmLoadSessionRequest, LcmRawMessage,
    LcmRawMessageOverview, LcmScope, LcmSourceRef, LcmStatus, LcmStorageKind, LcmStoreStatus,
    LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeOverview,
    LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS, LCM_DEFAULT_FRESH_TAIL_COUNT,
    LCM_DEFAULT_SUMMARY_FAN_IN, LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LCM_SCHEMA_VERSION,
};

const MAX_PAGE_LIMIT: usize = 100;
const PLACEHOLDER_PREFIXES: [&str; 5] = [
    "[externalized payload:",
    "[gc'd externalized payload:",
    "[externalized lcm ingest payload:",
    "[externalized tool output:",
    "[gc'd externalized tool output:",
];
const PLACEHOLDER_TEXT_COLUMNS: [&str; 4] =
    ["content", "snippet_text", "index_text", "metadata_json"];
const TERM_SEPARATORS: [char; 4] = ['-', ':', '/', '#'];
const RAW_GREP_RECENCY_EXPR: &str = "COALESCE(r.timestamp, r.store_id)";
const SUMMARY_GREP_RECENCY_EXPR: &str =
    "COALESCE(n.source_time_end, n.source_time_start, n.created_at)";
const RAW_ROLE_PENALTY_CASE: &str =
    "CASE r.role WHEN 'user' THEN 0 WHEN 'assistant' THEN 1 WHEN 'tool' THEN 2 ELSE 1 END";

#[allow(clippy::struct_field_names)]
struct LcmLifecycleMetadata {
    current_session_id: Option<String>,
    current_frontier_store_id: Option<i64>,
    last_finalized_session_id: Option<String>,
    last_finalized_frontier_store_id: Option<i64>,
}

#[allow(clippy::struct_field_names)]
struct PlaceholderPayloadStatus {
    placeholder_ref_count: i64,
    missing_metadata_count: i64,
    missing_file_count: i64,
    missing_refs: Vec<PayloadRefLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PayloadRefLocation {
    pub(crate) payload_ref: String,
    pub(crate) session_id: String,
    pub(crate) message_id: String,
    pub(crate) store_id: i64,
    pub(crate) field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PayloadFileStatusSample {
    pub(crate) payload_ref: String,
    pub(crate) bytes: u64,
    pub(crate) age_seconds: i64,
    pub(crate) eligible_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PayloadRefStatusSample {
    pub(crate) payload_ref: String,
    pub(crate) bytes: u64,
    pub(crate) eligible_at: Option<i64>,
    pub(crate) grace_remaining_seconds: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PayloadHealthDetail {
    pub(crate) payload: LcmPayloadStatus,
    pub(crate) payload_gc: LcmPayloadGcStatus,
    pub(crate) missing_payload_refs: Vec<PayloadRefLocation>,
    pub(crate) orphan_files: Vec<PayloadFileStatusSample>,
    pub(crate) unreferenced_refs: Vec<PayloadRefStatusSample>,
    pub(crate) missing_placeholder_refs: Vec<PayloadRefLocation>,
    pub(crate) integrity_mismatch_refs: Vec<String>,
}

pub(crate) async fn load_session(
    conn: &Connection,
    request: LcmLoadSessionRequest,
) -> Result<LcmLoadSessionPage, LcmError> {
    let limit = clamp_limit(request.limit);
    let fetch_limit = limit.saturating_add(1);
    let mut values = vec![
        Value::Text(request.provider.clone()),
        Value::Text(request.session_id.clone()),
        Value::Integer(request.after_store_id.unwrap_or(0)),
    ];
    let mut role_clause = String::new();
    let roles = normalized_strings(&request.roles);
    if !roles.is_empty() {
        let placeholders = std::iter::repeat_n("?", roles.len())
            .collect::<Vec<_>>()
            .join(", ");
        role_clause = format!(" AND role IN ({placeholders})");
        values.extend(roles.into_iter().map(Value::Text));
    }
    let start_time = util::opt_i64(request.start_time);
    let end_time = util::opt_i64(request.end_time);
    values.push(start_time.clone());
    values.push(start_time);
    values.push(end_time.clone());
    values.push(end_time);
    values.push(Value::Integer(fetch_limit as i64));
    let sql = format!(
        "SELECT provider, message_id, session_id, store_id, role, ordinal,
                timestamp, content, content_hash, storage_kind, payload_ref,
                snippet_text, legacy_source, legacy_truncated, metadata_json
         FROM lcm_raw_messages
         WHERE provider = ?
           AND session_id = ?
           AND store_id > ?
           {role_clause}
           AND (? IS NULL OR timestamp >= ?)
           AND (? IS NULL OR timestamp <= ?)
         ORDER BY store_id
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;

    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        let raw = raw::raw_message_from_row(&row)?;
        messages.push(load_message_from_raw(raw, request.content_slice));
    }

    let has_more = messages.len() > limit;
    if has_more {
        messages.truncate(limit);
    }
    let next_cursor = if has_more {
        messages.last().map(|message| message.store_id.to_string())
    } else {
        None
    };

    Ok(LcmLoadSessionPage {
        messages,
        next_cursor,
    })
}

pub(crate) async fn grep(
    conn: &Connection,
    request: LcmGrepRequest,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let query_plan = grep_query_plan(&request.query);
    if query_plan.is_empty() {
        return Ok(Vec::new());
    }
    let limit = clamp_limit(request.limit);
    let session_filter = scoped_session_filter(request.scope, request.session_id.as_deref());
    if matches!(request.scope, LcmScope::Current | LcmScope::Session) && session_filter.is_none() {
        return Ok(Vec::new());
    }

    let raw_only_filters =
        request.role.is_some() || request.start_time.is_some() || request.end_time.is_some();
    let mut hits = raw_grep_hits(conn, &request, session_filter, &query_plan, limit).await?;
    if request.include_summaries && !raw_only_filters && hits.len() < limit {
        let remaining = limit - hits.len();
        hits.extend(
            summary_grep_hits(conn, &request, session_filter, &query_plan, remaining).await?,
        );
    }
    sort_hits(&mut hits, request.sort);
    Ok(hits)
}

pub(crate) async fn expand(
    conn: &Connection,
    storage_root: &Path,
    request: LcmExpandRequest,
) -> Result<LcmExpandResponse, LcmError> {
    match request.target {
        LcmExpandTarget::RawMessage { store_id } => {
            let raw = raw::load_raw_message_by_store_id(conn, store_id).await?;
            // Raw store_id expansion works across sessions like hermes-lcm
            // `lcm_expand` store_id mode (grep scope=all -> expand the hit),
            // but stays provider-scoped: providers are a TraceDecay concept
            // with no Hermes equivalent.
            if raw.provider != request.provider {
                return Err(LcmError::SummarySourceNotOwnedBySession);
            }
            let from_current_session = raw.session_id == request.session_id;
            let externalized_ref = raw.payload_ref.clone();
            let (raw, range) = raw_message_with_sliced_content(raw, request.content_slice);
            let content = raw.content.clone();
            let payload_ref = if from_current_session {
                None
            } else {
                externalized_ref.clone()
            };
            Ok(LcmExpandResponse {
                kind: "raw_message".to_string(),
                content,
                content_range: range,
                raw_message: Some(raw),
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref,
                from_current_session: Some(from_current_session),
                externalized_note: None,
                source_pagination: None,
            })
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            let expansion =
                dag::expand_summary_node(conn, &request.provider, &request.session_id, &node_id)
                    .await?;
            let (summary, range) =
                summary_node_with_sliced_text(expansion.summary, request.content_slice);
            let content = summary.summary_text.clone();
            let (sources, source_pagination) = paginate_summary_sources(
                expansion.sources,
                request.source_offset,
                request.source_limit,
            );
            let summary_sources = slice_summary_sources(sources, request.content_slice);
            Ok(LcmExpandResponse {
                kind: "summary_node".to_string(),
                content,
                content_range: range,
                raw_message: None,
                summary_node: Some(summary),
                summary_sources,
                payload_ref: None,
                from_current_session: None,
                externalized_note: None,
                source_pagination: Some(source_pagination),
            })
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            let slice = request.content_slice.unwrap_or(LcmContentSlice {
                offset: 0,
                limit: usize::MAX,
            });
            let store = payload::LcmStore::new(conn, storage_root.to_path_buf());
            let expansion = store
                .lcm_expand_payload(
                    &request.provider,
                    &request.session_id,
                    &payload_ref,
                    slice.offset,
                    slice.limit,
                )
                .await?;
            let range = LcmContentRange {
                offset: expansion.offset,
                limit: slice.limit as u64,
                returned_chars: expansion.char_count,
                total_chars: expansion.total_char_count,
                truncated: expansion.offset > 0
                    || expansion.offset.saturating_add(expansion.char_count)
                        < expansion.total_char_count,
            };
            Ok(LcmExpandResponse {
                kind: "external_payload".to_string(),
                content: expansion.content,
                content_range: range,
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(expansion.payload_ref),
                from_current_session: None,
                externalized_note: None,
                source_pagination: None,
            })
        }
    }
}

pub(crate) async fn expand_query(
    conn: &Connection,
    request: LcmExpandQueryRequest,
) -> Result<LcmExpandQueryResponse, LcmError> {
    let max_results = clamp_limit(request.max_results);
    let context_max_chars = request.context_max_tokens.max(1);
    let mut matches = Vec::new();
    let mut selected_summaries = Vec::new();
    let mut selected_raw_store_ids = Vec::new();

    if request.node_ids.is_empty() {
        if let Some(query) = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
        {
            let query_plan = grep_query_plan(query);
            if !query_plan.is_empty() {
                let grep_request = LcmGrepRequest {
                    provider: request.provider.clone(),
                    query: query.to_string(),
                    scope: LcmScope::Session,
                    session_id: Some(request.session_id.clone()),
                    include_summaries: true,
                    limit: max_results,
                    sort: LcmGrepSort::Recency,
                    source: None,
                    role: None,
                    start_time: None,
                    end_time: None,
                };
                let summary_hits = summary_grep_hits(
                    conn,
                    &grep_request,
                    Some(&request.session_id),
                    &query_plan,
                    max_results,
                )
                .await?;
                for hit in summary_hits {
                    if let Some(node_id) = hit.node_id.as_deref() {
                        let expansion = dag::expand_summary_node(
                            conn,
                            &request.provider,
                            &request.session_id,
                            node_id,
                        )
                        .await?;
                        matches.push(expand_query_match_from_hit(&hit));
                        selected_summaries.push(expansion);
                    }
                }

                if selected_summaries.len() < max_results {
                    let remaining = max_results - selected_summaries.len();
                    let raw_hits = raw_grep_hits(
                        conn,
                        &grep_request,
                        Some(&request.session_id),
                        &query_plan,
                        remaining,
                    )
                    .await?;
                    for hit in raw_hits {
                        if let Some(store_id) = hit.store_id {
                            matches.push(expand_query_match_from_hit(&hit));
                            selected_raw_store_ids.push(store_id);
                        }
                    }
                }
            }
        }
    } else {
        for node_id in request.node_ids.iter().take(max_results) {
            let expansion =
                dag::expand_summary_node(conn, &request.provider, &request.session_id, node_id)
                    .await?;
            matches.push(LcmExpandQueryMatch {
                kind: "summary_node".to_string(),
                node_id: Some(expansion.summary.node_id.clone()),
                store_id: None,
                snippet: raw::derived_text_for_snippet(&expansion.summary.summary_text),
            });
            selected_summaries.push(expansion);
        }
    }

    if selected_summaries.is_empty() && selected_raw_store_ids.is_empty() {
        return Ok(LcmExpandQueryResponse {
            prompt: request.prompt,
            query: request.query,
            answer: Some("No matching LCM context found in the current session.".to_string()),
            needs_synthesis: false,
            synthesis_prompt: None,
            max_tokens: request.max_tokens,
            context_max_tokens: request.context_max_tokens,
            context_budget: LcmExpandQueryBudget {
                requested_max_chars: context_max_chars,
                used_chars: 0,
            },
            context_truncated: false,
            context_pagination: Vec::new(),
            node_ids: Vec::new(),
            matches,
            context_blocks: Vec::new(),
        });
    }

    let mut assembler = ExpandQueryAssembler::new(context_max_chars);
    let mut node_ids = Vec::new();
    for expansion in selected_summaries {
        node_ids.push(expansion.summary.node_id.clone());
        assembler.add_summary_expansion(expansion);
    }
    for store_id in selected_raw_store_ids {
        let raw = raw::load_raw_message_by_store_id(conn, store_id).await?;
        if raw.provider == request.provider && raw.session_id == request.session_id {
            assembler.add_raw_message(raw, None);
        }
    }

    let context_blocks = assembler.context_blocks;
    let context_pagination = assembler.context_pagination;
    let context_truncated = !context_pagination.is_empty();
    let context_budget = LcmExpandQueryBudget {
        requested_max_chars: context_max_chars,
        used_chars: assembler.used_chars,
    };
    let synthesis_prompt =
        expand_query_synthesis_prompt(&request.prompt, &context_blocks, context_truncated);

    Ok(LcmExpandQueryResponse {
        prompt: request.prompt,
        query: request.query,
        answer: None,
        needs_synthesis: true,
        synthesis_prompt: Some(synthesis_prompt),
        max_tokens: request.max_tokens,
        context_max_tokens: request.context_max_tokens,
        context_budget,
        context_truncated,
        context_pagination,
        node_ids,
        matches,
        context_blocks,
    })
}

pub(crate) async fn describe(
    conn: &Connection,
    request: LcmDescribeRequest,
) -> Result<LcmDescribeResponse, LcmError> {
    let provider = request.provider.as_str();
    let session_id = request.session_id.as_str();
    let raw_message_count = count_raw_messages(conn, provider, Some(session_id)).await?;
    let summary_node_count = count_summary_nodes(conn, provider, Some(session_id)).await?;
    let external_payload_count = count_external_payloads(conn, provider, Some(session_id)).await?;
    let (first_store_id, last_store_id) = raw_store_bounds(conn, provider, session_id).await?;
    let (target, raw_messages, summary_nodes, summary_node, external_payload) = match request.target
    {
        LcmDescribeTarget::Session => (
            "session".to_string(),
            raw_message_overviews(conn, provider, session_id).await?,
            summary_overviews(conn, provider, session_id).await?,
            None,
            None,
        ),
        LcmDescribeTarget::SummaryNode { node_id } => (
            "summary_node".to_string(),
            Vec::new(),
            Vec::new(),
            Some(describe_summary_node(conn, provider, session_id, &node_id).await?),
            None,
        ),
        LcmDescribeTarget::ExternalPayload { payload_ref } => (
            "external_payload".to_string(),
            Vec::new(),
            Vec::new(),
            None,
            Some(describe_external_payload(conn, provider, session_id, &payload_ref).await?),
        ),
    };

    Ok(LcmDescribeResponse {
        target,
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        raw_message_count,
        summary_node_count,
        external_payload_count,
        first_store_id,
        last_store_id,
        raw_messages,
        summary_nodes,
        summary_node,
        external_payload,
    })
}

pub(crate) async fn status(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    if !lcm_table_exists(conn, "lcm_raw_messages").await? {
        return Ok(empty_status(schema_version, gc_config));
    }
    if provider == "all" {
        return aggregate_provider_status(conn, storage_root, session_id, deep, gc_config).await;
    }

    status_for_provider(conn, storage_root, provider, session_id, deep, gc_config).await
}

async fn status_for_provider(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    let payload_health = payload_health_detail(
        conn,
        storage_root,
        provider,
        session_id,
        deep,
        20,
        gc_config,
    )
    .await?;
    let external_payload_count = payload_health.payload.externalized_count;
    let missing_payload_count = payload_health.payload.missing_count;
    let unreferenced_payload_count = payload_health.payload.unreferenced_count;
    let maintenance_debt_count =
        compression::maintenance_debt_count(conn, provider, session_id).await?;
    let lifecycle_state_count =
        count_lifecycle_states_for_current_session(conn, provider, session_id).await?;
    let frontier_count = count_frontier_rows(conn, provider, session_id).await?;
    let lifecycle_metadata = load_lifecycle_metadata(conn, provider, session_id).await?;
    let legacy_truncated_count = count_legacy_truncated(conn, provider, session_id).await?;
    let lossy_ingest_records = count_lossy_ingest_records(conn, provider, session_id).await?;
    let lossy_records = legacy_truncated_count + lossy_ingest_records;
    let store = store_status(conn, provider, session_id).await?;
    let dag = dag_status(conn, provider, session_id).await?;

    Ok(LcmStatus {
        schema_version,
        storage_scope: Some("project_local".to_string()),
        raw_message_count: count_raw_messages(conn, provider, session_id).await?,
        summary_node_count: count_summary_nodes(conn, provider, session_id).await?,
        external_payload_count,
        missing_payload_count,
        unreferenced_payload_count,
        maintenance_debt_count,
        store,
        dag,
        config: LcmConfigStatus {
            fresh_tail_count: LCM_DEFAULT_FRESH_TAIL_COUNT,
            summary_fan_in: LCM_DEFAULT_SUMMARY_FAN_IN,
            compression_boundary_cooldown_seconds: LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
        },
        payload: payload_health.payload,
        payload_gc: payload_health.payload_gc,
        lifecycle: LcmLifecycleStatus {
            lifecycle_state_count,
            frontier_count,
            maintenance_debt_count,
            current_session_id: lifecycle_metadata.current_session_id,
            current_frontier_store_id: lifecycle_metadata.current_frontier_store_id,
            last_finalized_session_id: lifecycle_metadata.last_finalized_session_id,
            last_finalized_frontier_store_id: lifecycle_metadata.last_finalized_frontier_store_id,
        },
        redaction: LcmRedactionStatus {
            enabled: lossy_records > 0,
            lossy_records,
            legacy_truncated_count,
        },
    })
}

async fn aggregate_provider_status(
    conn: &Connection,
    storage_root: &Path,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    let providers = lcm_status_providers(conn, session_id).await?;
    if providers.is_empty() {
        return Ok(empty_status(schema_version, gc_config));
    }

    let mut aggregate = empty_status(schema_version, gc_config);
    for provider in providers {
        let status =
            status_for_provider(conn, storage_root, &provider, session_id, deep, gc_config).await?;
        merge_lcm_status(&mut aggregate, status);
    }
    let payload_health =
        payload_health_detail(conn, storage_root, "all", session_id, deep, 20, gc_config).await?;
    aggregate.external_payload_count = payload_health.payload.externalized_count;
    aggregate.missing_payload_count = payload_health.payload.missing_count;
    aggregate.unreferenced_payload_count = payload_health.payload.unreferenced_count;
    aggregate.payload = payload_health.payload;
    aggregate.payload_gc = payload_health.payload_gc;
    aggregate.storage_scope = Some("project_local".to_string());
    aggregate.dag.compression_ratio = python_round_ratio_to_tenths(
        aggregate.dag.total_source_tokens,
        aggregate.dag.total_tokens,
    );
    aggregate.redaction.enabled = aggregate.redaction.lossy_records > 0;
    Ok(aggregate)
}

async fn lcm_status_providers(
    conn: &Connection,
    session_id: Option<&str>,
) -> Result<Vec<String>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT provider
             FROM (
                 SELECT provider, session_id FROM lcm_raw_messages
                 UNION
                 SELECT provider, session_id FROM lcm_summary_nodes
                 UNION
                 SELECT provider, session_id FROM lcm_external_payloads
                 UNION
                 SELECT provider, current_session_id AS session_id FROM lcm_lifecycle_state
             )
             WHERE (?1 IS NULL OR session_id = ?1)
             ORDER BY provider",
            params![util::opt_text(session_id)],
        )
        .await?;
    let mut providers = Vec::new();
    while let Some(row) = rows.next().await? {
        providers.push(row.get(0)?);
    }
    Ok(providers)
}

fn merge_lcm_status(target: &mut LcmStatus, source: LcmStatus) {
    target.raw_message_count += source.raw_message_count;
    target.summary_node_count += source.summary_node_count;
    target.external_payload_count += source.external_payload_count;
    target.missing_payload_count += source.missing_payload_count;
    target.unreferenced_payload_count += source.unreferenced_payload_count;
    target.maintenance_debt_count += source.maintenance_debt_count;
    target.store.messages += source.store.messages;
    target.store.estimated_tokens += source.store.estimated_tokens;
    target.dag.total_nodes += source.dag.total_nodes;
    target.dag.total_tokens += source.dag.total_tokens;
    target.dag.total_source_tokens += source.dag.total_source_tokens;
    for (depth, source_depth) in source.dag.depths {
        let target_depth = target
            .dag
            .depths
            .entry(depth)
            .or_insert_with(|| LcmDagDepthStatus {
                count: 0,
                tokens: 0,
                source_tokens: 0,
            });
        target_depth.count += source_depth.count;
        target_depth.tokens += source_depth.tokens;
        target_depth.source_tokens += source_depth.source_tokens;
    }
    merge_payload_status(&mut target.payload, source.payload);
    merge_payload_gc_status(&mut target.payload_gc, source.payload_gc);
    target.lifecycle.lifecycle_state_count += source.lifecycle.lifecycle_state_count;
    target.lifecycle.frontier_count += source.lifecycle.frontier_count;
    target.lifecycle.maintenance_debt_count += source.lifecycle.maintenance_debt_count;
    target.redaction.lossy_records += source.redaction.lossy_records;
    target.redaction.legacy_truncated_count += source.redaction.legacy_truncated_count;
}

fn merge_payload_status(target: &mut LcmPayloadStatus, source: LcmPayloadStatus) {
    target.externalized_count += source.externalized_count;
    target.missing_count += source.missing_count;
    target.unreferenced_count += source.unreferenced_count;
    target.placeholder_ref_count += source.placeholder_ref_count;
    target.missing_placeholder_metadata_count += source.missing_placeholder_metadata_count;
    target.missing_placeholder_file_count += source.missing_placeholder_file_count;
    target.gc_candidate_count += source.gc_candidate_count;
    target.root_contained &= source.root_contained;
    target.orphan_file_count += source.orphan_file_count;
    target.tombstoned_count += source.tombstoned_count;
    target.referenced_count += source.referenced_count;
    target.total_bytes += source.total_bytes;
    target.referenced_bytes += source.referenced_bytes;
    target.orphan_file_bytes += source.orphan_file_bytes;
    target.reclaimable_bytes += source.reclaimable_bytes;
    target.reclaimable_bytes_after_grace += source.reclaimable_bytes_after_grace;
    target.integrity_mismatch_count = match (
        target.integrity_mismatch_count,
        source.integrity_mismatch_count,
    ) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
}

fn merge_payload_gc_status(target: &mut LcmPayloadGcStatus, source: LcmPayloadGcStatus) {
    target.last_gc_at = max_option_i64(target.last_gc_at, source.last_gc_at);
    target.last_gc_duration_ms =
        max_option_u64(target.last_gc_duration_ms, source.last_gc_duration_ms);
    if target.last_gc_status.as_deref() != Some("failed") {
        target.last_gc_status = source.last_gc_status.or(target.last_gc_status.take());
    }
    target.last_gc_error = source.last_gc_error.or(target.last_gc_error.take());
    target.last_reaped_refs = sum_option_i64(target.last_reaped_refs, source.last_reaped_refs);
    target.last_reaped_bytes = sum_option_u64(target.last_reaped_bytes, source.last_reaped_bytes);
    target.next_run_eligible_at =
        min_option_i64(target.next_run_eligible_at, source.next_run_eligible_at);
}

fn max_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn min_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn sum_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_option_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn sum_option_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

async fn lcm_table_exists(conn: &Connection, table_name: &str) -> Result<bool, LcmError> {
    Ok(util::fetch_i64(
        conn,
        "SELECT COUNT(*)
         FROM sqlite_master
         WHERE type = 'table' AND name = ?1",
        params![table_name],
        "LCM table existence query returned no rows",
    )
    .await?
        > 0)
}

fn empty_status(schema_version: i64, gc_config: &LcmGcConfig) -> LcmStatus {
    let gc_config = gc_config.clone().normalized();
    let grace_seconds = i64::try_from(gc_config.grace_seconds).unwrap_or(i64::MAX);
    let reap_missing_after_seconds =
        i64::try_from(gc_config.reap_missing_after).unwrap_or(i64::MAX);
    LcmStatus {
        schema_version,
        storage_scope: Some("project_local".to_string()),
        raw_message_count: 0,
        summary_node_count: 0,
        external_payload_count: 0,
        missing_payload_count: 0,
        unreferenced_payload_count: 0,
        maintenance_debt_count: 0,
        store: LcmStoreStatus {
            messages: 0,
            estimated_tokens: 0,
        },
        dag: LcmDagStatus {
            total_nodes: 0,
            total_tokens: 0,
            total_source_tokens: 0,
            compression_ratio: "0:1".to_string(),
            depths: BTreeMap::new(),
        },
        config: LcmConfigStatus {
            fresh_tail_count: LCM_DEFAULT_FRESH_TAIL_COUNT,
            summary_fan_in: LCM_DEFAULT_SUMMARY_FAN_IN,
            compression_boundary_cooldown_seconds: LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
        },
        payload: LcmPayloadStatus {
            externalized_count: 0,
            missing_count: 0,
            unreferenced_count: 0,
            placeholder_ref_count: 0,
            missing_placeholder_metadata_count: 0,
            missing_placeholder_file_count: 0,
            gc_candidate_count: 0,
            root_contained: true,
            orphan_file_count: 0,
            tombstoned_count: 0,
            referenced_count: 0,
            total_bytes: 0,
            referenced_bytes: 0,
            orphan_file_bytes: 0,
            reclaimable_bytes: 0,
            reclaimable_bytes_after_grace: 0,
            integrity_mismatch_count: None,
        },
        payload_gc: LcmPayloadGcStatus {
            last_gc_at: None,
            last_gc_duration_ms: None,
            last_gc_status: None,
            last_gc_error: None,
            last_reaped_refs: None,
            last_reaped_bytes: None,
            grace_seconds,
            reap_missing_metadata_after_seconds: reap_missing_after_seconds,
            next_run_eligible_at: None,
        },
        lifecycle: LcmLifecycleStatus {
            lifecycle_state_count: 0,
            frontier_count: 0,
            maintenance_debt_count: 0,
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        },
        redaction: LcmRedactionStatus {
            enabled: false,
            lossy_records: 0,
            legacy_truncated_count: 0,
        },
    }
}

fn load_message_from_raw(
    raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> LcmLoadSessionMessage {
    let LcmRawMessage {
        provider,
        message_id,
        session_id,
        store_id,
        role,
        ordinal,
        timestamp,
        content,
        content_hash,
        storage_kind,
        payload_ref,
        legacy_source,
        legacy_truncated,
        metadata_json,
    } = raw;
    let (content, content_range) = slice_content_owned(content, slice);
    LcmLoadSessionMessage {
        provider,
        message_id,
        session_id,
        store_id,
        role,
        ordinal,
        timestamp,
        content,
        content_range,
        content_hash,
        storage_kind,
        payload_ref,
        legacy_source,
        legacy_truncated,
        metadata_json,
    }
}

fn slice_content_owned(
    content: String,
    slice: Option<LcmContentSlice>,
) -> (String, LcmContentRange) {
    let total_chars = content.chars().count();
    let offset = slice.map_or(0, |slice| slice.offset).min(total_chars);
    let limit = slice.map_or(total_chars.saturating_sub(offset), |slice| slice.limit);
    if offset == 0 && limit >= total_chars {
        return (
            content,
            LcmContentRange {
                offset: 0,
                limit: limit as u64,
                returned_chars: total_chars as u64,
                total_chars: total_chars as u64,
                truncated: false,
            },
        );
    }
    let sliced = content.chars().skip(offset).take(limit).collect::<String>();
    let returned_chars = sliced.chars().count();
    let truncated = offset > 0 || offset.saturating_add(returned_chars) < total_chars;
    (
        sliced,
        LcmContentRange {
            offset: offset as u64,
            limit: limit as u64,
            returned_chars: returned_chars as u64,
            total_chars: total_chars as u64,
            truncated,
        },
    )
}

fn slice_content(content: &str, slice: Option<LcmContentSlice>) -> (String, LcmContentRange) {
    slice_content_owned(content.to_string(), slice)
}

fn raw_message_with_sliced_content(
    mut raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> (LcmRawMessage, LcmContentRange) {
    let (content, range) = slice_content_owned(std::mem::take(&mut raw.content), slice);
    raw.content = content;
    (raw, range)
}

fn summary_node_with_sliced_text(
    mut summary: LcmSummaryNode,
    slice: Option<LcmContentSlice>,
) -> (LcmSummaryNode, LcmContentRange) {
    let (summary_text, range) =
        slice_content_owned(std::mem::take(&mut summary.summary_text), slice);
    summary.summary_text = summary_text;
    (summary, range)
}

fn slice_summary_sources(
    sources: Vec<LcmExpandedSummarySource>,
    slice: Option<LcmContentSlice>,
) -> Vec<LcmExpandedSummarySource> {
    sources
        .into_iter()
        .map(|mut source| {
            let (content, range) = slice_content_owned(std::mem::take(&mut source.content), slice);
            source.content = content;
            source.content_truncated = range.truncated;
            source.content_range = Some(range);
            if let Some(raw_message) = source.raw_message.as_mut() {
                raw_message.content.clone_from(&source.content);
            }
            if let Some(summary_node) = source.summary_node.as_mut() {
                summary_node.summary_text.clone_from(&source.content);
            }
            source
        })
        .collect()
}

/// Pages a summary node's immediate source list with hermes-lcm `lcm_expand`
/// cursor semantics: the offset clamps to the source count, an omitted limit
/// returns all remaining sources, and `next_source_offset` is the resume
/// cursor while more sources remain.
fn paginate_summary_sources(
    sources: Vec<LcmExpandedSummarySource>,
    source_offset: usize,
    source_limit: Option<usize>,
) -> (Vec<LcmExpandedSummarySource>, LcmExpandSourcePagination) {
    let total_sources = sources.len();
    let source_offset = source_offset.min(total_sources);
    let remaining = total_sources - source_offset;
    let source_limit = source_limit.map_or(remaining, |limit| limit.min(remaining));
    let page: Vec<LcmExpandedSummarySource> = sources
        .into_iter()
        .skip(source_offset)
        .take(source_limit)
        .collect();
    let consumed = source_offset.saturating_add(source_limit);
    let has_more = consumed < total_sources;
    let pagination = LcmExpandSourcePagination {
        source_offset,
        source_limit,
        returned_sources: page.len(),
        total_sources,
        next_source_offset: has_more.then_some(consumed),
        has_more,
        remaining_sources: if has_more {
            total_sources - consumed
        } else {
            0
        },
    };
    (page, pagination)
}

/// Mirrors `compression::estimate_tokens`: deterministic whitespace-word
/// token estimate used for the `lcm_status` store size diagnostic.
fn estimate_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

async fn store_status(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmStoreStatus, LcmError> {
    let mut rows = conn
        .query(
            "SELECT content, snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut messages = 0_i64;
    let mut estimated_tokens = 0_i64;
    while let Some(row) = rows.next().await? {
        messages += 1;
        let content: Option<String> = row.get(0)?;
        let snippet: String = row.get(1)?;
        // Externalized rows count their inline placeholder, matching what the
        // engine replays into active context.
        let text = content.unwrap_or(snippet);
        estimated_tokens += estimate_tokens(&text);
    }
    Ok(LcmStoreStatus {
        messages,
        estimated_tokens,
    })
}

async fn dag_status(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmDagStatus, LcmError> {
    let mut rows = conn
        .query(
            "SELECT depth, COUNT(*), SUM(summary_token_count), SUM(source_token_count)
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             GROUP BY depth
             ORDER BY depth",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut depths = std::collections::BTreeMap::new();
    let mut total_nodes = 0_i64;
    let mut total_tokens = 0_i64;
    let mut total_source_tokens = 0_i64;
    while let Some(row) = rows.next().await? {
        let depth: i64 = row.get(0)?;
        let count: i64 = row.get(1)?;
        let tokens: i64 = row.get(2)?;
        let source_tokens: i64 = row.get(3)?;
        total_nodes += count;
        total_tokens += tokens;
        total_source_tokens += source_tokens;
        depths.insert(
            format!("d{depth}"),
            LcmDagDepthStatus {
                count,
                tokens,
                source_tokens,
            },
        );
    }
    // Hermes renders `round(source/summary, 1)` as "N.N:1" and "0:1" for an
    // empty DAG (`hermes-lcm/tools.py` lcm_status). Python `round` uses
    // bankers rounding (ties-to-even), so mirror it with integer math.
    let compression_ratio = python_round_ratio_to_tenths(total_source_tokens, total_tokens);
    Ok(LcmDagStatus {
        total_nodes,
        total_tokens,
        total_source_tokens,
        compression_ratio,
        depths,
    })
}

fn python_round_ratio_to_tenths(total_source_tokens: i64, total_tokens: i64) -> String {
    if total_tokens <= 0 {
        return "0:1".to_string();
    }
    let numerator = i128::from(total_source_tokens.max(0)) * 10;
    let denominator = i128::from(total_tokens);
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = match (remainder * 2).cmp(&denominator) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal => {
            if quotient % 2 == 0 {
                quotient
            } else {
                quotient + 1
            }
        }
    };
    let whole = rounded / 10;
    let fractional = (rounded % 10).abs();
    format!("{whole}.{fractional}:1")
}

struct ExpandQueryAssembler {
    context_blocks: Vec<LcmExpandQueryContextBlock>,
    context_pagination: Vec<LcmExpandQueryPagination>,
    max_chars: usize,
    used_chars: usize,
}

impl ExpandQueryAssembler {
    fn new(max_chars: usize) -> Self {
        Self {
            context_blocks: Vec::new(),
            context_pagination: Vec::new(),
            max_chars,
            used_chars: 0,
        }
    }

    fn remaining_chars(&self) -> usize {
        self.max_chars.saturating_sub(self.used_chars)
    }

    fn add_summary_expansion(&mut self, expansion: LcmSummaryExpansion) {
        let node_id = expansion.summary.node_id.clone();
        let summary_text = expansion.summary.summary_text.clone();
        if let Some((content, range)) =
            self.take_content("summary", Some(node_id.clone()), None, &summary_text)
        {
            let mut summary = expansion.summary.clone();
            summary.summary_text.clone_from(&content);
            self.context_blocks.push(LcmExpandQueryContextBlock {
                kind: "summary".to_string(),
                node_id: Some(node_id.clone()),
                source_ref: None,
                content,
                content_range: range,
                raw_message: None,
                summary_node: Some(summary),
            });
        }

        for source in expansion.sources {
            let source_ref = source.source_ref.clone();
            let kind = match source_ref {
                LcmSourceRef::RawMessage { .. } => "raw_message",
                LcmSourceRef::SummaryNode { .. } => "summary_source",
            };
            let Some((content, range)) = self.take_content(
                kind,
                Some(node_id.clone()),
                Some(source_ref.clone()),
                &source.content,
            ) else {
                continue;
            };
            let raw_message = source.raw_message.map(|mut raw| {
                raw.content.clone_from(&content);
                raw
            });
            let summary_node = source.summary_node.map(|summary| {
                let mut summary = *summary;
                summary.summary_text.clone_from(&content);
                summary
            });
            self.context_blocks.push(LcmExpandQueryContextBlock {
                kind: kind.to_string(),
                node_id: Some(node_id.clone()),
                source_ref: Some(source_ref),
                content,
                content_range: range,
                raw_message,
                summary_node,
            });
        }
    }

    fn add_raw_message(&mut self, raw: LcmRawMessage, node_id: Option<String>) {
        let source_ref = Some(LcmSourceRef::RawMessage {
            store_id: raw.store_id,
        });
        let Some((content, range)) = self.take_content(
            "raw_message",
            node_id.clone(),
            source_ref.clone(),
            &raw.content,
        ) else {
            return;
        };
        let mut raw_message = raw;
        raw_message.content.clone_from(&content);
        self.context_blocks.push(LcmExpandQueryContextBlock {
            kind: "raw_message".to_string(),
            node_id,
            source_ref,
            content,
            content_range: range,
            raw_message: Some(raw_message),
            summary_node: None,
        });
    }

    fn take_content(
        &mut self,
        kind: &str,
        node_id: Option<String>,
        source_ref: Option<LcmSourceRef>,
        content: &str,
    ) -> Option<(String, LcmContentRange)> {
        let remaining = self.remaining_chars();
        if remaining == 0 {
            self.context_pagination.push(LcmExpandQueryPagination {
                kind: kind.to_string(),
                node_id,
                source_ref,
                next_content_offset: Some(0),
                has_more: true,
            });
            return None;
        }

        let (sliced, range) = slice_content(
            content,
            Some(LcmContentSlice {
                offset: 0,
                limit: remaining,
            }),
        );
        self.used_chars = self
            .used_chars
            .saturating_add(sliced.chars().count())
            .min(self.max_chars);
        if range.truncated {
            self.context_pagination.push(LcmExpandQueryPagination {
                kind: kind.to_string(),
                node_id,
                source_ref,
                next_content_offset: Some(range.returned_chars),
                has_more: true,
            });
        }
        Some((sliced, range))
    }
}

fn expand_query_match_from_hit(hit: &LcmGrepHit) -> LcmExpandQueryMatch {
    LcmExpandQueryMatch {
        kind: hit.kind.clone(),
        node_id: hit.node_id.clone(),
        store_id: hit.store_id,
        snippet: hit.snippet.clone(),
    }
}

fn expand_query_synthesis_prompt(
    prompt: &str,
    context_blocks: &[LcmExpandQueryContextBlock],
    context_truncated: bool,
) -> LcmExpandQuerySynthesisPrompt {
    let system = LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT.to_string();
    let context_json = serde_json::to_string_pretty(context_blocks).unwrap_or_else(|_| "[]".into());
    let truncation_note = if context_truncated {
        "\n\nNOTE: Some LCM context was truncated; pagination metadata is included in the tool response."
    } else {
        ""
    };
    let user = format!("QUESTION:\n{prompt}\n\nEXPANDED CONTEXT:\n{context_json}{truncation_note}");
    LcmExpandQuerySynthesisPrompt { system, user }
}

fn grep_provider_filter(request: &LcmGrepRequest) -> Option<&str> {
    let provider = request.provider.trim();
    if provider.is_empty() || provider == "all" {
        None
    } else {
        Some(provider)
    }
}

fn push_grep_provider_filter(
    request: &LcmGrepRequest,
    column: &str,
    filters: &mut Vec<String>,
    values: &mut Vec<Value>,
) {
    if let Some(provider) = grep_provider_filter(request) {
        filters.push(format!("{column} = ?"));
        values.push(Value::Text(provider.to_string()));
    }
}

async fn raw_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.requires_like_fallback {
        return raw_like_grep_hits(conn, request, session_id, query_plan, limit).await;
    }
    let mut values = vec![Value::Text(query_plan.fts_query.clone())];
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "r.provider", &mut filters, &mut values);
    push_raw_grep_filters(request, session_id, &mut filters, &mut values);
    values.push(Value::Integer(limit as i64));
    let filter_sql = if filters.is_empty() {
        String::new()
    } else {
        format!(" AND {}", filters.join(" AND "))
    };
    let order_by = grep_order_by(
        request.sort,
        RAW_GREP_RECENCY_EXPR,
        Some(RAW_ROLE_PENALTY_CASE),
    );
    let sql = format!(
        "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text
         FROM lcm_raw_messages_fts
         JOIN lcm_raw_messages r ON r.store_id = lcm_raw_messages_fts.rowid
         WHERE lcm_raw_messages_fts MATCH ?
           {filter_sql}
         ORDER BY {order_by}
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;

    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        hits.push(raw_hit_from_row(&row, &query_plan.like_terms)?);
    }
    Ok(hits)
}

async fn summary_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.requires_like_fallback {
        return summary_like_grep_hits(conn, request, session_id, query_plan, limit).await;
    }
    let mut values = vec![Value::Text(query_plan.fts_query.clone())];
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "n.provider", &mut filters, &mut values);
    push_summary_grep_filters(request, session_id, &mut filters, &mut values);
    values.push(Value::Integer(limit as i64));
    let filter_sql = if filters.is_empty() {
        String::new()
    } else {
        format!(" AND {}", filters.join(" AND "))
    };
    let order_by = grep_order_by(request.sort, SUMMARY_GREP_RECENCY_EXPR, None);
    let sql = format!(
        "SELECT n.provider, n.session_id, n.node_id, n.summary_text
         FROM lcm_summary_nodes_fts
         JOIN lcm_summary_nodes n ON n.rowid = lcm_summary_nodes_fts.rowid
         WHERE lcm_summary_nodes_fts MATCH ?
           {filter_sql}
         ORDER BY {order_by}, n.node_id
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;

    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        hits.push(summary_hit_from_row(&row, &query_plan.like_terms)?);
    }
    Ok(hits)
}

async fn raw_like_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.like_terms.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_limit = compute_like_fallback_fetch_limit(limit, query_plan);

    let mut values = Vec::new();
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "r.provider", &mut filters, &mut values);
    push_raw_grep_filters(request, session_id, &mut filters, &mut values);

    let like_sql = like_predicate_sql(
        query_plan.like_terms.len(),
        &["r.index_text", "r.snippet_text", "COALESCE(r.content, '')"],
    );
    filters.push(like_sql);
    for term in &query_plan.like_terms {
        let escaped = escape_like(term);
        let pattern = format!("%{escaped}%");
        for _ in 0..3 {
            values.push(Value::Text(pattern.clone()));
        }
    }

    values.push(Value::Integer(fetch_limit as i64));
    let order_by = grep_order_by(
        request.sort,
        RAW_GREP_RECENCY_EXPR,
        Some(RAW_ROLE_PENALTY_CASE),
    );
    let sql = format!(
        "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text, 0.0 AS rank
         FROM lcm_raw_messages r
         WHERE {}
         ORDER BY {order_by}
         LIMIT ?",
        filters.join(" AND "),
    );
    let mut rows = conn.query(&sql, values).await?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        hits.push(raw_hit_from_row(&row, &query_plan.like_terms)?);
    }
    if hits.len() > limit {
        hits.truncate(limit);
    }
    Ok(hits)
}

async fn summary_like_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.like_terms.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_limit = compute_like_fallback_fetch_limit(limit, query_plan);

    let mut values = Vec::new();
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "n.provider", &mut filters, &mut values);
    push_summary_grep_filters(request, session_id, &mut filters, &mut values);

    filters.push(like_predicate_sql(
        query_plan.like_terms.len(),
        &["n.summary_text"],
    ));
    for term in &query_plan.like_terms {
        values.push(Value::Text(format!("%{}%", escape_like(term))));
    }

    values.push(Value::Integer(fetch_limit as i64));
    let order_by = grep_order_by(request.sort, SUMMARY_GREP_RECENCY_EXPR, None);
    let sql = format!(
        "SELECT n.provider, n.session_id, n.node_id, n.summary_text, 0.0 AS rank
         FROM lcm_summary_nodes n
         WHERE {}
         ORDER BY {order_by}, n.node_id
         LIMIT ?",
        filters.join(" AND "),
    );
    let mut rows = conn.query(&sql, values).await?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        hits.push(summary_hit_from_row(&row, &query_plan.like_terms)?);
    }
    if hits.len() > limit {
        hits.truncate(limit);
    }
    Ok(hits)
}

fn push_raw_grep_filters(
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    filters: &mut Vec<String>,
    values: &mut Vec<Value>,
) {
    if let Some(session_id) = session_id {
        filters.push("r.session_id = ?".to_string());
        values.push(Value::Text(session_id.to_string()));
    }
    if let Some(source) = request
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        filters.push(
            "(json_extract(r.metadata_json, '$.source') = ? OR r.metadata_json LIKE ?)".to_string(),
        );
        values.push(Value::Text(source.to_string()));
        values.push(Value::Text(format!("%\"source\":\"{source}\"%")));
    }
    if let Some(role) = request
        .role
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        filters.push("r.role = ?".to_string());
        values.push(Value::Text(role.to_string()));
    }
    if let Some(start_time) = request.start_time {
        filters.push("r.timestamp >= ?".to_string());
        values.push(Value::Integer(start_time));
    }
    if let Some(end_time) = request.end_time {
        filters.push("r.timestamp <= ?".to_string());
        values.push(Value::Integer(end_time));
    }
}

fn push_summary_grep_filters(
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    filters: &mut Vec<String>,
    values: &mut Vec<Value>,
) {
    if let Some(session_id) = session_id {
        filters.push("n.session_id = ?".to_string());
        values.push(Value::Text(session_id.to_string()));
    }
    if let Some(source) = request
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        filters.push(
            "EXISTS (
                SELECT 1
                FROM lcm_summary_sources ss
                JOIN lcm_raw_messages sr
                  ON ss.source_kind = 'raw_message'
                 AND sr.store_id = CAST(ss.source_id AS INTEGER)
                WHERE ss.node_id = n.node_id
                  AND (json_extract(sr.metadata_json, '$.source') = ? OR sr.metadata_json LIKE ?)
             )"
            .to_string(),
        );
        values.push(Value::Text(source.to_string()));
        values.push(Value::Text(format!("%\"source\":\"{source}\"%")));
    }
}

fn raw_hit_from_row(row: &libsql::Row, like_terms: &[String]) -> Result<LcmGrepHit, LcmError> {
    let snippet: String = row.get(4)?;
    Ok(LcmGrepHit {
        kind: "raw_message".to_string(),
        provider: row.get(0)?,
        session_id: row.get(1)?,
        message_id: Some(row.get(2)?),
        node_id: None,
        store_id: Some(row.get(3)?),
        snippet: match_centered_snippet(&snippet, like_terms),
    })
}

fn summary_hit_from_row(row: &libsql::Row, like_terms: &[String]) -> Result<LcmGrepHit, LcmError> {
    let summary_text: String = row.get(3)?;
    Ok(LcmGrepHit {
        kind: "summary_node".to_string(),
        provider: row.get(0)?,
        session_id: row.get(1)?,
        message_id: None,
        node_id: Some(row.get(2)?),
        store_id: None,
        snippet: match_centered_snippet(&summary_text, like_terms),
    })
}

async fn raw_message_overviews(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessageOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT message_id, store_id, role, storage_kind, payload_ref, snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY store_id
             LIMIT 20",
            params![provider, session_id],
        )
        .await?;

    let mut overviews = Vec::new();
    while let Some(row) = rows.next().await? {
        let storage_kind_text: String = row.get(3)?;
        let content_preview: String = row.get(5)?;
        let (_, content_range) = slice_content(&content_preview, None);
        overviews.push(LcmRawMessageOverview {
            message_id: row.get(0)?,
            store_id: row.get(1)?,
            role: row.get(2)?,
            storage_kind: LcmStorageKind::from_db(&storage_kind_text).ok_or_else(|| {
                LcmError::Db(format!("invalid storage_kind: {storage_kind_text}"))
            })?,
            payload_ref: row.get(4)?,
            content_preview,
            content_range,
        });
    }
    Ok(overviews)
}

async fn summary_overviews(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmSummaryNodeOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT n.node_id, n.conversation_id, n.depth, n.summary_text, n.created_at,
                    COUNT(s.source_id)
             FROM lcm_summary_nodes n
             LEFT JOIN lcm_summary_sources s ON s.node_id = n.node_id
             WHERE n.provider = ?1 AND n.session_id = ?2
             GROUP BY n.node_id, n.conversation_id, n.depth, n.summary_text, n.created_at
             ORDER BY n.depth, n.created_at, n.node_id
             LIMIT 20",
            params![provider, session_id],
        )
        .await?;

    let mut overviews = Vec::new();
    while let Some(row) = rows.next().await? {
        let summary_text: String = row.get(3)?;
        let source_count: i64 = row.get(5)?;
        overviews.push(LcmSummaryNodeOverview {
            node_id: row.get(0)?,
            conversation_id: row.get(1)?,
            depth: row.get(2)?,
            summary_preview: raw::derived_text_for_snippet(&summary_text),
            source_count: source_count.max(0) as usize,
            created_at: row.get(4)?,
        });
    }
    Ok(overviews)
}

async fn describe_summary_node(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmDescribeSummaryNode, LcmError> {
    let mut rows = conn
        .query(
            "SELECT node_id, conversation_id, depth, summary_token_count,
                    source_token_count, source_time_start, source_time_end,
                    expand_hint, metadata_json, created_at
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND session_id = ?2 AND node_id = ?3",
            params![provider, session_id, node_id],
        )
        .await?;
    let row = rows.next().await?.ok_or(LcmError::SummaryNodeNotFound)?;
    let children = describe_summary_sources(conn, provider, session_id, node_id).await?;
    Ok(LcmDescribeSummaryNode {
        node_id: row.get(0)?,
        conversation_id: row.get(1)?,
        depth: row.get(2)?,
        summary_token_count: row.get(3)?,
        source_token_count: row.get(4)?,
        source_time_start: row.get(5)?,
        source_time_end: row.get(6)?,
        expand_hint: row.get(7)?,
        metadata_json: row.get(8)?,
        created_at: row.get(9)?,
        source_count: children.len(),
        children,
    })
}

async fn describe_summary_sources(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<Vec<LcmDescribeSourceOverview>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT source_kind, source_id
             FROM lcm_summary_sources
             WHERE node_id = ?1
             ORDER BY ordinal",
            params![node_id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let source_kind: String = row.get(0)?;
        let source_id: String = row.get(1)?;
        match source_kind.as_str() {
            "raw_message" => {
                let store_id = source_id
                    .parse::<i64>()
                    .map_err(|err| LcmError::Db(format!("invalid raw source id: {err}")))?;
                let mut raw_rows = conn
                    .query(
                        "SELECT role, storage_kind
                         FROM lcm_raw_messages
                         WHERE provider = ?1 AND session_id = ?2 AND store_id = ?3",
                        params![provider, session_id, store_id],
                    )
                    .await?;
                let Some(raw_row) = raw_rows.next().await? else {
                    continue;
                };
                let storage_kind_text: String = raw_row.get(1)?;
                out.push(LcmDescribeSourceOverview {
                    source_kind,
                    source_ref: LcmSourceRef::RawMessage { store_id },
                    store_id: Some(store_id),
                    node_id: None,
                    role: Some(raw_row.get(0)?),
                    storage_kind: LcmStorageKind::from_db(&storage_kind_text),
                    summary_token_count: None,
                    source_token_count: None,
                    expand_hint: None,
                });
            }
            "summary_node" => {
                let mut summary_rows = conn
                    .query(
                        "SELECT summary_token_count, source_token_count, expand_hint
                         FROM lcm_summary_nodes
                         WHERE provider = ?1 AND session_id = ?2 AND node_id = ?3",
                        params![provider, session_id, source_id.as_str()],
                    )
                    .await?;
                let Some(summary_row) = summary_rows.next().await? else {
                    continue;
                };
                out.push(LcmDescribeSourceOverview {
                    source_kind,
                    source_ref: LcmSourceRef::SummaryNode {
                        node_id: source_id.clone(),
                    },
                    store_id: None,
                    node_id: Some(source_id),
                    role: None,
                    storage_kind: None,
                    summary_token_count: Some(summary_row.get(0)?),
                    source_token_count: Some(summary_row.get(1)?),
                    expand_hint: summary_row.get(2)?,
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

async fn describe_external_payload(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<LcmDescribeExternalPayload, LcmError> {
    payload::validate_payload_ref(payload_ref)?;
    let payload = payload::load_payload_metadata(conn, payload_ref).await?;
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotFound);
    }
    Ok(LcmDescribeExternalPayload {
        payload_ref: payload.payload_ref,
        provider: payload.provider,
        session_id: payload.session_id.clone(),
        message_id: payload.message_id.clone(),
        kind: payload.kind,
        content_hash: payload.content_hash,
        byte_count: payload.byte_count,
        char_count: payload.char_count,
        created_at: payload.created_at,
        metadata_json: payload.metadata_json,
        content_preview: external_payload_placeholder_preview(
            conn,
            provider,
            session_id,
            &payload.message_id,
            payload_ref,
        )
        .await?,
    })
}

async fn external_payload_placeholder_preview(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    message_id: &str,
    payload_ref: &str,
) -> Result<String, LcmError> {
    let mut rows = conn
        .query(
            "SELECT snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND session_id = ?2
               AND message_id = ?3
               AND payload_ref = ?4
             LIMIT 1",
            params![provider, session_id, message_id, payload_ref],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        return Ok(row.get(0)?);
    }
    Ok(format!("[externalized payload ref={payload_ref}]"))
}

async fn raw_store_bounds(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<(Option<i64>, Option<i64>), LcmError> {
    let mut rows = conn
        .query(
            "SELECT MIN(store_id), MAX(store_id)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok((None, None));
    };
    Ok((row.get(0)?, row.get(1)?))
}

async fn count_raw_messages(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await
}

async fn count_summary_nodes(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await
}

async fn count_external_payloads(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_external_payloads", provider, session_id).await
}

async fn count_lifecycle_states_for_current_session(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)",
        params![provider, util::opt_text(session_id)],
        "lifecycle count query returned no rows",
    )
    .await
}

pub(crate) async fn payload_health_detail(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    sample_limit: usize,
    gc_config: &LcmGcConfig,
) -> Result<PayloadHealthDetail, LcmError> {
    let gc_config = gc_config.clone().normalized();
    let now = current_timestamp();
    let sample_limit = sample_limit.max(1);
    let metadata_refs = metadata_refs_for_scope(conn, provider, session_id).await?;
    let metadata_bytes = payload_byte_counts_for_scope(conn, provider, session_id).await?;
    let payload_locations = payload_ref_locations_for_scope(conn, provider, session_id).await?;
    let referenced_refs = gc::referenced_payload_refs(conn, provider, session_id).await?;
    let placeholder_status = placeholder_payload_status(
        conn,
        storage_root,
        provider,
        session_id,
        &metadata_refs,
        &payload_locations,
        sample_limit,
    )
    .await?;
    let file_owner_refs = if session_id.is_some() {
        gc::all_payload_metadata_refs(conn).await?
    } else {
        metadata_refs.clone()
    };
    let payload_dir = payload::payload_dir(storage_root);
    let grace_seconds_i64 = i64::try_from(gc_config.grace_seconds).unwrap_or(i64::MAX);
    let reap_missing_after_seconds =
        i64::try_from(gc_config.reap_missing_after).unwrap_or(i64::MAX);

    let last_gc_at = gc_meta_i64(conn, "last_gc_at").await?;
    let last_gc_duration_ms = gc_meta_i64(conn, "last_gc_duration_ms")
        .await?
        .map(|value| value.max(0) as u64);
    let last_gc_error = schema::get_gc_meta(conn, "last_error").await?;
    let last_reaped_refs = gc_meta_i64(conn, "last_reaped_refs").await?;
    let last_reaped_bytes = gc_meta_i64(conn, "last_reaped_bytes")
        .await?
        .map(|value| value.max(0) as u64);
    let last_gc_status = schema::get_gc_meta(conn, "last_gc_status")
        .await?
        .or_else(|| match (last_gc_at, last_gc_error.as_deref()) {
            (None, _) => None,
            (Some(_), None | Some("")) => Some("ok".to_string()),
            (Some(_), Some("partial")) => Some("partial".to_string()),
            (Some(_), Some(_)) => Some("failed".to_string()),
        });

    let mut missing_count = 0_i64;
    let mut missing_payload_refs = Vec::new();
    let mut unreferenced_count = 0_i64;
    let mut total_bytes = 0_u64;
    let mut referenced_bytes = 0_u64;
    let mut reclaimable_unreferenced_bytes = 0_u64;
    let mut reclaimable_bytes_after_grace = 0_u64;
    let mut next_run_eligible_at: Option<i64> = None;
    let mut integrity_mismatch_count = 0_i64;
    let mut integrity_mismatch_refs = Vec::new();
    let root_contained = payload_root_contained(storage_root);

    for payload_ref in &metadata_refs {
        let bytes = metadata_bytes.get(payload_ref).copied().unwrap_or_default();
        total_bytes = total_bytes.saturating_add(bytes);
        let is_referenced = referenced_refs.contains(payload_ref);
        if is_referenced {
            referenced_bytes = referenced_bytes.saturating_add(bytes);
        } else {
            unreferenced_count += 1;
            reclaimable_bytes_after_grace = reclaimable_bytes_after_grace.saturating_add(bytes);
            let eligible_at =
                gc_eligible_at_for_unreferenced(conn, payload_ref, grace_seconds_i64).await?;
            if let Some(eligible_at) = eligible_at {
                next_run_eligible_at = Some(
                    next_run_eligible_at.map_or(eligible_at, |current| current.min(eligible_at)),
                );
                if last_gc_at.is_some() && eligible_at <= now {
                    reclaimable_unreferenced_bytes =
                        reclaimable_unreferenced_bytes.saturating_add(bytes);
                }
            }
        }

        let missing_file = payload::validate_payload_ref(payload_ref).is_err()
            || !payload_file_present_strict(&payload_dir, payload_ref)?;
        if missing_file {
            missing_count += 1;
            if missing_payload_refs.len() < sample_limit {
                missing_payload_refs.push(
                    payload_locations
                        .get(payload_ref)
                        .cloned()
                        .unwrap_or_else(|| {
                            payload_ref_location(payload_ref, session_id, "payload_ref")
                        }),
                );
            }
            continue;
        }

        if deep
            && payload_has_integrity_mismatch(
                storage_root,
                payload_ref,
                metadata_refs.contains(payload_ref),
                conn,
            )
            .await?
        {
            integrity_mismatch_count += 1;
            if integrity_mismatch_refs.len() < sample_limit {
                integrity_mismatch_refs.push(payload_ref.clone());
            }
        }
    }

    let mut orphan_file_count = 0_i64;
    let mut orphan_file_bytes = 0_u64;
    let mut reclaimable_orphan_bytes = 0_u64;
    let mut orphan_files = Vec::new();
    if let Ok(entries) = fs::read_dir(&payload_dir) {
        for entry in entries {
            let entry = entry.map_err(|err| LcmError::Io(err.to_string()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if payload::validate_payload_ref(&name).is_err() {
                continue;
            }
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|err| LcmError::Io(err.to_string()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            if file_owner_refs.contains(&name) {
                continue;
            }
            orphan_file_count += 1;
            orphan_file_bytes = orphan_file_bytes.saturating_add(metadata.len());
            reclaimable_bytes_after_grace =
                reclaimable_bytes_after_grace.saturating_add(metadata.len());
            let age_seconds = now.saturating_sub(file_mtime_seconds(&metadata));
            let eligible_at = file_mtime_seconds(&metadata).saturating_add(grace_seconds_i64);
            next_run_eligible_at =
                Some(next_run_eligible_at.map_or(eligible_at, |current| current.min(eligible_at)));
            if age_seconds >= grace_seconds_i64 && last_gc_at.is_some() {
                reclaimable_orphan_bytes = reclaimable_orphan_bytes.saturating_add(metadata.len());
            }
            if orphan_files.len() < sample_limit {
                orphan_files.push(PayloadFileStatusSample {
                    payload_ref: name,
                    bytes: metadata.len(),
                    age_seconds,
                    eligible_at,
                });
            }
        }
    }

    let tombstoned_count = tombstoned_count(conn, provider, session_id).await?;
    let externalized_count = metadata_refs.len() as i64;
    let referenced_count = externalized_count.saturating_sub(unreferenced_count);
    let reclaimable_bytes = if last_gc_at.is_some() {
        reclaimable_unreferenced_bytes.saturating_add(reclaimable_orphan_bytes)
    } else {
        0
    };
    let unreferenced_refs = payload_unreferenced_samples(
        conn,
        &metadata_refs,
        &referenced_refs,
        &metadata_bytes,
        last_gc_at,
        grace_seconds_i64,
        now,
        sample_limit,
    )
    .await?;

    Ok(PayloadHealthDetail {
        payload: LcmPayloadStatus {
            externalized_count,
            missing_count,
            unreferenced_count,
            placeholder_ref_count: placeholder_status.placeholder_ref_count,
            missing_placeholder_metadata_count: placeholder_status.missing_metadata_count,
            missing_placeholder_file_count: placeholder_status.missing_file_count,
            gc_candidate_count: unreferenced_count,
            root_contained,
            orphan_file_count,
            tombstoned_count,
            referenced_count,
            total_bytes,
            referenced_bytes,
            orphan_file_bytes,
            reclaimable_bytes,
            reclaimable_bytes_after_grace,
            integrity_mismatch_count: deep.then_some(integrity_mismatch_count),
        },
        payload_gc: LcmPayloadGcStatus {
            last_gc_at,
            last_gc_duration_ms,
            last_gc_status,
            last_gc_error,
            last_reaped_refs,
            last_reaped_bytes,
            grace_seconds: grace_seconds_i64,
            reap_missing_metadata_after_seconds: reap_missing_after_seconds,
            next_run_eligible_at,
        },
        missing_payload_refs,
        orphan_files,
        unreferenced_refs,
        missing_placeholder_refs: placeholder_status.missing_refs,
        integrity_mismatch_refs,
    })
}

pub(crate) fn payload_health_state(
    payload: &LcmPayloadStatus,
    payload_gc: &LcmPayloadGcStatus,
) -> &'static str {
    if payload.missing_count > 0
        || payload.missing_placeholder_file_count > 0
        || payload.integrity_mismatch_count.unwrap_or(0) > 0
        || payload_gc.last_gc_status.as_deref() == Some("failed")
        || !payload.root_contained
    {
        "error"
    } else if payload.orphan_file_count > 0
        || payload.unreferenced_count > 0
        || payload_gc.last_gc_at.is_none()
    {
        "warning"
    } else {
        "healthy"
    }
}

async fn payload_byte_counts_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, u64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT payload_ref, byte_count
             FROM lcm_external_payloads
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut bytes = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        let byte_count: i64 = row.get(1)?;
        bytes.insert(payload_ref, byte_count.max(0) as u64);
    }
    Ok(bytes)
}

async fn payload_ref_locations_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, PayloadRefLocation>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT store_id, message_id, session_id, storage_kind, payload_ref, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut refs = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let message_id: String = row.get(1)?;
        let owner_session_id: String = row.get(2)?;
        let storage_kind: String = row.get(3)?;
        let raw_payload_ref: Option<String> = row.get(4).unwrap_or(None);
        if storage_kind == "external" {
            if let Some(payload_ref) = raw_payload_ref {
                refs.entry(payload_ref.clone())
                    .or_insert_with(|| PayloadRefLocation {
                        payload_ref,
                        session_id: owner_session_id.clone(),
                        message_id: message_id.clone(),
                        store_id,
                        field: "payload_ref".to_string(),
                    });
            }
        }
        for index in 5..9 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            let field = match index {
                5 => "content",
                6 => "snippet_text",
                7 => "index_text",
                _ => "metadata_json",
            };
            for payload_ref in value
                .as_deref()
                .map(payload::extract_payload_refs_from_text)
                .unwrap_or_default()
            {
                refs.entry(payload_ref.clone())
                    .or_insert_with(|| PayloadRefLocation {
                        payload_ref,
                        session_id: owner_session_id.clone(),
                        message_id: message_id.clone(),
                        store_id,
                        field: field.to_string(),
                    });
            }
        }
    }
    Ok(refs)
}

fn payload_ref_location(
    payload_ref: &str,
    session_id: Option<&str>,
    field: &str,
) -> PayloadRefLocation {
    PayloadRefLocation {
        payload_ref: payload_ref.to_string(),
        session_id: session_id.unwrap_or_default().to_string(),
        message_id: String::new(),
        store_id: 0,
        field: field.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn payload_unreferenced_samples(
    conn: &Connection,
    metadata_refs: &BTreeSet<String>,
    referenced_refs: &BTreeSet<String>,
    metadata_bytes: &BTreeMap<String, u64>,
    last_gc_at: Option<i64>,
    grace_seconds: i64,
    now: i64,
    sample_limit: usize,
) -> Result<Vec<PayloadRefStatusSample>, LcmError> {
    let mut samples = Vec::new();
    for payload_ref in metadata_refs.difference(referenced_refs) {
        if samples.len() >= sample_limit {
            break;
        }
        let eligible_at = gc_eligible_at_for_unreferenced(conn, payload_ref, grace_seconds).await?;
        let grace_remaining_seconds = eligible_at.map(|ts| ts.saturating_sub(now).max(0));
        samples.push(PayloadRefStatusSample {
            payload_ref: payload_ref.clone(),
            bytes: metadata_bytes.get(payload_ref).copied().unwrap_or_default(),
            eligible_at: if last_gc_at.is_some() {
                eligible_at
            } else {
                None
            },
            grace_remaining_seconds: if last_gc_at.is_some() {
                grace_remaining_seconds
            } else {
                None
            },
        });
    }
    Ok(samples)
}

async fn gc_eligible_at_for_unreferenced(
    conn: &Connection,
    payload_ref: &str,
    grace_seconds: i64,
) -> Result<Option<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT first_seen_at
             FROM lcm_gc_marks
             WHERE payload_ref = ?1 AND state = 'unreferenced'",
            params![payload_ref],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let first_seen_at: i64 = row.get(0)?;
    Ok(Some(first_seen_at.saturating_add(grace_seconds)))
}

async fn gc_meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>, LcmError> {
    Ok(schema::get_gc_meta(conn, key)
        .await?
        .and_then(|value| value.parse::<i64>().ok()))
}

async fn tombstoned_count(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)
               AND (
                    content LIKE ?3 COLLATE NOCASE
                 OR content LIKE ?4 COLLATE NOCASE
                 OR snippet_text LIKE ?3 COLLATE NOCASE
                 OR snippet_text LIKE ?4 COLLATE NOCASE
                 OR index_text LIKE ?3 COLLATE NOCASE
                 OR index_text LIKE ?4 COLLATE NOCASE
                 OR metadata_json LIKE ?3 COLLATE NOCASE
                 OR metadata_json LIKE ?4 COLLATE NOCASE
               )",
            params![
                provider,
                util::opt_text(session_id),
                "%[gc'd externalized payload:%",
                "%[gc'd externalized tool output:%",
            ],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("tombstoned count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn placeholder_payload_status(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    metadata_refs: &BTreeSet<String>,
    payload_locations: &BTreeMap<String, PayloadRefLocation>,
    sample_limit: usize,
) -> Result<PlaceholderPayloadStatus, LcmError> {
    let placeholder_refs = placeholder_refs_for_scope(conn, provider, session_id).await?;
    let dir = payload::payload_dir(storage_root);
    let mut missing_metadata_count = 0_i64;
    let mut missing_file_count = 0_i64;
    let mut missing_refs = Vec::new();
    for payload_ref in &placeholder_refs {
        let missing_metadata = !metadata_refs.contains(payload_ref);
        let missing_file = payload::validate_payload_ref(payload_ref).is_err()
            || !payload_file_present_strict(&dir, payload_ref)?;
        if missing_metadata {
            missing_metadata_count += 1;
        }
        if missing_file {
            missing_file_count += 1;
        }
        if (missing_metadata || missing_file) && missing_refs.len() < sample_limit {
            missing_refs.push(
                payload_locations
                    .get(payload_ref)
                    .cloned()
                    .unwrap_or_else(|| {
                        payload_ref_location(payload_ref, session_id, "placeholder")
                    }),
            );
        }
    }
    Ok(PlaceholderPayloadStatus {
        placeholder_ref_count: placeholder_refs.len() as i64,
        missing_metadata_count,
        missing_file_count,
        missing_refs,
    })
}

async fn metadata_refs_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        refs.insert(row.get(0)?);
    }
    Ok(refs)
}

async fn placeholder_refs_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let placeholder_predicates = PLACEHOLDER_TEXT_COLUMNS
        .iter()
        .flat_map(|column| {
            PLACEHOLDER_PREFIXES
                .iter()
                .map(move |_| format!("{column} LIKE ? COLLATE NOCASE"))
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "SELECT content, snippet_text, index_text, metadata_json
         FROM lcm_raw_messages
         WHERE (? = 'all' OR provider = ?)
           AND (? IS NULL OR session_id = ?)
           AND ({placeholder_predicates})"
    );
    let session_value = util::opt_text(session_id);
    let mut values = vec![
        Value::Text(provider.to_string()),
        Value::Text(provider.to_string()),
        session_value.clone(),
        session_value,
    ];
    for _column in PLACEHOLDER_TEXT_COLUMNS {
        for prefix in PLACEHOLDER_PREFIXES {
            values.push(Value::Text(format!("%{prefix}%")));
        }
    }
    let mut refs = BTreeSet::new();
    let mut rows = conn.query(&sql, values).await?;
    while let Some(row) = rows.next().await? {
        for index in 0..4 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            if let Some(value) = value {
                refs.extend(payload::extract_payload_refs_from_text(&value));
            }
        }
    }
    Ok(refs)
}

fn payload_file_present_strict(dir: &Path, payload_ref: &str) -> Result<bool, LcmError> {
    let path = dir.join(payload_ref);
    payload::ensure_contained(dir, &path)?;
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    Ok(metadata.is_file() && !metadata.file_type().is_symlink())
}

async fn payload_has_integrity_mismatch(
    storage_root: &Path,
    payload_ref: &str,
    _exists_in_metadata: bool,
    conn: &Connection,
) -> Result<bool, LcmError> {
    let metadata = match payload::load_payload_metadata(conn, payload_ref).await {
        Ok(metadata) => metadata,
        Err(LcmError::PayloadNotFound) => return Ok(false),
        Err(err) => return Err(err),
    };
    let dir = payload::existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    payload::ensure_contained(&dir, &path)?;
    let Ok(fs_metadata) = fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    if fs_metadata.file_type().is_symlink() || !fs_metadata.is_file() {
        return Ok(true);
    }
    let bytes = fs::read(&path).map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(util::sha256_hex(&bytes) != metadata.content_hash)
}

fn payload_root_contained(storage_root: &Path) -> bool {
    let dir = payload::payload_dir(storage_root);
    if !dir.exists() {
        return true;
    }
    let Ok(root) = storage_root.canonicalize() else {
        return false;
    };
    let Ok(canonical_dir) = dir.canonicalize() else {
        return false;
    };
    canonical_dir.parent() == Some(root.as_path())
}

#[cfg(unix)]
fn file_mtime_seconds(metadata: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime()
}

#[cfg(not(unix))]
fn file_mtime_seconds(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

async fn load_lifecycle_metadata(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmLifecycleMetadata, LcmError> {
    let session_value = util::opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)
             ORDER BY updated_at DESC, conversation_id DESC
             LIMIT 1",
            params![provider, session_value],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(LcmLifecycleMetadata {
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        });
    };
    Ok(LcmLifecycleMetadata {
        current_session_id: row.get(0)?,
        current_frontier_store_id: row.get(1)?,
        last_finalized_session_id: row.get(2)?,
        last_finalized_frontier_store_id: row.get(3)?,
    })
}

async fn count_frontier_rows(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2)
               AND current_frontier_store_id IS NOT NULL",
        params![provider, util::opt_text(session_id)],
        "frontier count query returned no rows",
    )
    .await
}

async fn count_legacy_truncated(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND legacy_truncated != 0",
        params![provider, util::opt_text(session_id)],
        "legacy truncated count query returned no rows",
    )
    .await
}

/// SQL pushdown of the former Rust-side metadata scan. Semantics are pinned
/// to the old `serde_json` reader, which counted a row only when
/// `metadata_json.ingest_protection.lossy` was the JSON *boolean* `true`
/// (`Value::as_bool`): `json_type(...) = 'true'` matches exactly — a numeric
/// `1` reports `'integer'` and stays not-lossy (the Rust writer in
/// `raw::add_ingest_protection_metadata` only ever stores `json!(true)`),
/// invalid JSON is screened out by `json_valid` (`SQLite` `AND` short-circuits
/// left-to-right, so `json_type` never raises on malformed text), and a
/// missing key or non-object metadata yields `NULL`, which is not `'true'`.
async fn count_lossy_ingest_records(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND metadata_json IS NOT NULL
               AND json_valid(metadata_json)
               AND json_type(metadata_json, '$.ingest_protection.lossy') = 'true'",
        params![provider, util::opt_text(session_id)],
        "lossy ingest count query returned no rows",
    )
    .await
}

fn scoped_session_filter(scope: LcmScope, session_id: Option<&str>) -> Option<&str> {
    match scope {
        LcmScope::All => None,
        LcmScope::Current | LcmScope::Session => session_id,
    }
}

#[derive(Debug, Clone)]
struct GrepQueryPlan {
    fts_query: String,
    like_terms: Vec<String>,
    quoted_phrases: Vec<String>,
    requires_like_fallback: bool,
}

impl GrepQueryPlan {
    fn is_empty(&self) -> bool {
        self.fts_query.is_empty() && self.like_terms.is_empty()
    }
}

fn grep_query_plan(query: &str) -> GrepQueryPlan {
    let fts_query = sanitize_fts5_query(query);
    let terms = extract_search_terms(query);
    let quoted_phrases = extract_quoted_phrases(query);
    let mut like_terms = Vec::new();
    for term in terms {
        if !term.is_empty() && !like_terms.iter().any(|existing| existing == &term) {
            like_terms.push(term);
        }
    }
    if like_terms.is_empty() {
        let fallback = query.trim();
        if !fallback.is_empty() {
            like_terms.push(fallback.to_string());
        }
    }
    let requires_like_fallback = requires_like_fallback(query);
    GrepQueryPlan {
        fts_query,
        like_terms,
        quoted_phrases,
        requires_like_fallback,
    }
}

fn compute_like_fallback_fetch_limit(limit: usize, query_plan: &GrepQueryPlan) -> usize {
    compute_search_fetch_limit(limit, &query_plan.like_terms, &query_plan.quoted_phrases)
}

fn compute_search_fetch_limit(limit: usize, terms: &[String], phrases: &[String]) -> usize {
    let base = limit.saturating_mul(5).max(limit).max(20);
    if should_widen_candidate_fetch(terms, phrases) {
        return base.max(limit.saturating_mul(10)).max(50);
    }
    base
}

fn should_widen_candidate_fetch(terms: &[String], phrases: &[String]) -> bool {
    is_precise_query_shape(terms, phrases)
}

fn is_precise_query_shape(terms: &[String], phrases: &[String]) -> bool {
    terms.len() == 1 || (phrases.len() == 1 && terms.len() <= 2)
}

fn sanitize_fts5_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let mut quote_buffer = String::new();
    let mut in_quote = false;
    for ch in query.chars() {
        if ch == '"' {
            if in_quote {
                result.push('"');
                result.push_str(&quote_buffer);
                result.push('"');
                quote_buffer.clear();
                in_quote = false;
            } else {
                if result
                    .chars()
                    .last()
                    .is_some_and(|last| !last.is_whitespace())
                {
                    result.push(' ');
                }
                in_quote = true;
                quote_buffer.clear();
            }
            continue;
        }
        if in_quote {
            quote_buffer.push(ch);
            continue;
        }
        result.push(if is_fts5_special_char(ch) { ' ' } else { ch });
    }
    if in_quote && !quote_buffer.is_empty() {
        for ch in quote_buffer.chars() {
            result.push(if is_fts5_special_char(ch) { ' ' } else { ch });
        }
    }
    result.trim().to_string()
}

fn is_fts5_special_char(ch: char) -> bool {
    matches!(
        ch,
        '"' | '(' | ')' | '*' | '^' | '-' | ':' | '{' | '}' | '.' | '#'
    )
}

fn requires_like_fallback(query: &str) -> bool {
    contains_cjk(query) || contains_emoji(query) || contains_risky_fts_ascii(query)
}

fn contains_cjk(value: &str) -> bool {
    value.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0x3000..=0x303F
                | 0x3040..=0x30FF
                | 0xAC00..=0xD7AF
                | 0xFF00..=0xFFEF
        )
    })
}

fn contains_emoji(value: &str) -> bool {
    value.chars().any(|ch| {
        matches!(
            ch as u32,
            0x2600..=0x27BF | 0x1F300..=0x1FAFF
        )
    })
}

fn contains_risky_fts_ascii(value: &str) -> bool {
    let raw = value.trim();
    if raw.is_empty() {
        return false;
    }
    if raw.chars().filter(|ch| *ch == '"').count() % 2 != 0 {
        return true;
    }
    let (_, without_phrases) = split_quoted(raw);
    let chars = without_phrases.chars().collect::<Vec<_>>();
    for window in chars.windows(3) {
        let [left, mid, right] = [window[0], window[1], window[2]];
        if left.is_ascii_alphanumeric()
            && right.is_ascii_alphanumeric()
            && TERM_SEPARATORS.contains(&mid)
        {
            return true;
        }
    }
    false
}

fn extract_search_terms(query: &str) -> Vec<String> {
    let text = query.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let (mut terms, text_without_phrases) = split_quoted(text);
    for token in text_without_phrases.split_whitespace() {
        for variant in token_variants(token) {
            if !terms.iter().any(|existing| existing == &variant) {
                terms.push(variant);
            }
        }
    }
    if terms.is_empty() {
        let fallback = text.trim_matches(|ch: char| "\"'()[]{}.,;".contains(ch));
        if !fallback.is_empty() {
            terms.push(fallback.to_string());
        }
    }
    terms
}

fn extract_quoted_phrases(query: &str) -> Vec<String> {
    let text = query.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let (phrases, _) = split_quoted(text);
    let mut unique = Vec::new();
    for phrase in phrases {
        if !phrase.is_empty() && !unique.iter().any(|existing| existing == &phrase) {
            unique.push(phrase);
        }
    }
    unique
}

fn split_quoted(text: &str) -> (Vec<String>, String) {
    let mut phrases = Vec::new();
    let mut remainder = String::with_capacity(text.len());
    let mut in_quote = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '"' {
            if in_quote {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    phrases.push(trimmed.to_string());
                }
                current.clear();
                in_quote = false;
            } else {
                in_quote = true;
                current.clear();
            }
            remainder.push(' ');
            continue;
        }
        if in_quote {
            current.push(ch);
            remainder.push(' ');
        } else {
            remainder.push(ch);
        }
    }
    (phrases, remainder)
}

fn token_variants(token: &str) -> Vec<String> {
    let cleaned = token
        .trim()
        .trim_matches(|ch: char| "\"'()[]{}.,;".contains(ch));
    if cleaned.is_empty() {
        return Vec::new();
    }
    if matches!(
        cleaned.to_ascii_uppercase().as_str(),
        "AND" | "OR" | "NOT" | "NEAR"
    ) {
        return Vec::new();
    }
    let mut variants = vec![cleaned.to_string()];
    if cleaned.contains(TERM_SEPARATORS) {
        for part in cleaned.split(TERM_SEPARATORS) {
            if !part.is_empty() && !variants.iter().any(|existing| existing == part) {
                variants.push(part.to_string());
            }
        }
    }
    variants
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn like_predicate_sql(term_count: usize, columns: &[&str]) -> String {
    let mut parts = Vec::new();
    for _ in 0..term_count {
        let column_checks = columns
            .iter()
            .map(|column| format!("{column} LIKE ? ESCAPE '\\' COLLATE NOCASE"))
            .collect::<Vec<_>>()
            .join(" OR ");
        parts.push(format!("({column_checks})"));
    }
    format!("({})", parts.join(" OR "))
}

fn match_centered_snippet(text: &str, terms: &[String]) -> String {
    let mut best_match = None;
    for term in terms {
        if term.is_empty() {
            continue;
        }
        if let Some(byte_idx) = find_term(text, term) {
            best_match = Some((byte_idx, term.chars().count().max(1)));
            break;
        }
    }
    let Some((match_byte_idx, match_char_len)) = best_match else {
        return raw::derived_text_for_snippet(text);
    };

    let total_chars = text.chars().count();
    let match_char_idx = text[..match_byte_idx].chars().count();
    let window_chars = 160usize;
    let start_char = match_char_idx.saturating_sub(window_chars / 2);
    let end_char = (match_char_idx + match_char_len + (window_chars / 2)).min(total_chars);
    let start_byte = byte_offset_for_char_index(text, start_char);
    let end_byte = byte_offset_for_char_index(text, end_char);
    let mut snippet = String::new();
    if start_char > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&text[start_byte..end_byte]);
    if end_char < total_chars {
        snippet.push_str("...");
    }
    raw::derived_text_for_snippet(&snippet)
}

fn find_term(text: &str, term: &str) -> Option<usize> {
    if term.is_ascii() {
        let lower_text = text.to_ascii_lowercase();
        let lower_term = term.to_ascii_lowercase();
        return lower_text.find(&lower_term);
    }
    text.find(term)
}

fn byte_offset_for_char_index(text: &str, char_index: usize) -> usize {
    if char_index == 0 {
        return 0;
    }
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(idx, _)| idx)
}

fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_PAGE_LIMIT)
}

fn normalized_strings(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

const AGE_DECAY_RATE: f64 = 0.001;

fn grep_order_by(
    sort: LcmGrepSort,
    recency_column: &str,
    role_penalty_expr: Option<&str>,
) -> String {
    match sort {
        LcmGrepSort::Relevance => match role_penalty_expr {
            Some(role_penalty_expr) => {
                format!("rank ASC, {role_penalty_expr} ASC, {recency_column} DESC")
            }
            None => format!("rank ASC, {recency_column} DESC"),
        },
        LcmGrepSort::Hybrid => {
            let blended = format!(
                "(rank / (1 + (MAX(0.0, ((strftime('%s','now') - {recency_column}) / 3600.0)) * {AGE_DECAY_RATE})))"
            );
            match role_penalty_expr {
                Some(role_penalty_expr) => {
                    format!("{blended} ASC, {role_penalty_expr} ASC, {recency_column} DESC")
                }
                None => format!("{blended} ASC, {recency_column} DESC"),
            }
        }
        LcmGrepSort::Recency => match role_penalty_expr {
            Some(role_penalty_expr) => {
                format!("{recency_column} DESC, {role_penalty_expr} ASC, rank ASC")
            }
            None => format!("{recency_column} DESC, rank ASC"),
        },
    }
}

fn sort_hits(hits: &mut [LcmGrepHit], sort: LcmGrepSort) {
    if matches!(sort, LcmGrepSort::Recency) {
        hits.sort_by(|left, right| {
            right
                .store_id
                .unwrap_or(i64::MIN)
                .cmp(&left.store_id.unwrap_or(i64::MIN))
        });
    }
}
