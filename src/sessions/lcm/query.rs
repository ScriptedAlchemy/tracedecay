use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use libsql::{params, Connection, Value};

use super::types::{LcmLifecycleStatus, LcmPayloadStatus, LcmRedactionStatus};
use super::{
    compression, dag, payload, raw, schema, LcmConfigStatus, LcmContentRange, LcmContentSlice,
    LcmDagDepthStatus, LcmDagStatus, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryMatch,
    LcmExpandQueryPagination, LcmExpandQueryRequest, LcmExpandQueryResponse,
    LcmExpandQuerySynthesisPrompt, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination,
    LcmExpandTarget, LcmExpandedSummarySource, LcmGrepHit, LcmGrepRequest, LcmGrepSort,
    LcmLoadSessionMessage, LcmLoadSessionPage, LcmLoadSessionRequest, LcmRawMessage,
    LcmRawMessageOverview, LcmScope, LcmSourceRef, LcmStatus, LcmStorageKind, LcmStoreStatus,
    LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeOverview,
    LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS, LCM_DEFAULT_FRESH_TAIL_COUNT,
    LCM_DEFAULT_SUMMARY_FAN_IN, LCM_SCHEMA_VERSION,
};

const MAX_PAGE_LIMIT: usize = 100;

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
    let start_time = opt_i64(request.start_time);
    let end_time = opt_i64(request.end_time);
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
    let mut rows = conn
        .query(&sql, values)
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut messages = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let raw = raw_message_from_row(&row)?;
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
        request,
        messages,
        next_cursor,
    })
}

pub(crate) async fn grep(
    conn: &Connection,
    request: LcmGrepRequest,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let query = fts_query(&request.query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let limit = clamp_limit(request.limit);
    let session_filter = scoped_session_filter(request.scope, request.session_id.as_deref());
    if matches!(request.scope, LcmScope::Current | LcmScope::Session) && session_filter.is_none() {
        return Ok(Vec::new());
    }

    let raw_only_filters =
        request.role.is_some() || request.start_time.is_some() || request.end_time.is_some();
    let mut hits = raw_grep_hits(conn, &request, session_filter, &query, limit).await?;
    if request.include_summaries && !raw_only_filters && hits.len() < limit {
        let remaining = limit - hits.len();
        hits.extend(summary_grep_hits(conn, &request, session_filter, &query, remaining).await?);
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
            let raw = load_raw_message_by_store_id(conn, store_id).await?;
            // Raw store_id expansion works across sessions like hermes-lcm
            // `lcm_expand` store_id mode (grep scope=all -> expand the hit),
            // but stays provider-scoped: providers are a TokenSave concept
            // with no Hermes equivalent.
            if raw.provider != request.provider {
                return Err(LcmError::SummarySourceNotOwnedBySession);
            }
            let from_current_session = raw.session_id == request.session_id;
            let externalized_ref = raw.payload_ref.clone();
            let (raw, range) = raw_message_with_sliced_content(raw, request.content_slice);
            let content = raw.content.clone();
            let (payload_ref, externalized_note) = if from_current_session {
                (None, None)
            } else {
                (
                    externalized_ref.clone(),
                    externalized_ref.map(|_| {
                        "Externalized payload metadata is session-scoped; cross-session ref is \
                         surfaced for traceability only and cannot be expanded in this version."
                            .to_string()
                    }),
                )
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
                externalized_note,
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
    _storage_root: &Path,
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
            let fts = fts_query(query);
            if !fts.is_empty() {
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
                    &fts,
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
                        &fts,
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
        let raw = load_raw_message_by_store_id(conn, store_id).await?;
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
) -> Result<LcmStatus, LcmError> {
    let external_payload_count = count_external_payloads(conn, provider, session_id).await?;
    let missing_payload_count =
        count_missing_payloads(conn, storage_root, provider, session_id).await?;
    let unreferenced_payload_count = count_unreferenced_payloads(conn, storage_root).await?;
    let placeholder_status =
        placeholder_payload_status(conn, storage_root, provider, session_id).await?;
    let maintenance_debt_count =
        compression::maintenance_debt_count(conn, provider, session_id).await?;
    let lifecycle_state_count = count_lifecycle_states(conn, provider, session_id).await?;
    let frontier_count = count_frontier_rows(conn, provider, session_id).await?;
    let lifecycle_metadata = load_lifecycle_metadata(conn, provider, session_id).await?;
    let legacy_truncated_count = count_legacy_truncated(conn, provider, session_id).await?;
    let lossy_ingest_records = count_lossy_ingest_records(conn, provider, session_id).await?;
    let lossy_records = legacy_truncated_count + lossy_ingest_records;
    let store = store_status(conn, provider, session_id).await?;
    let dag = dag_status(conn, provider, session_id).await?;

    Ok(LcmStatus {
        schema_version: schema::schema_version(conn)
            .await
            .unwrap_or(LCM_SCHEMA_VERSION),
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
        payload: LcmPayloadStatus {
            externalized_count: external_payload_count,
            missing_count: missing_payload_count,
            unreferenced_count: unreferenced_payload_count,
            placeholder_ref_count: placeholder_status.placeholder_ref_count,
            missing_placeholder_metadata_count: placeholder_status.missing_metadata_count,
            missing_placeholder_file_count: placeholder_status.missing_file_count,
            gc_candidate_count: unreferenced_payload_count,
            root_contained: payload_root_contained(storage_root),
        },
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

fn load_message_from_raw(
    raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> LcmLoadSessionMessage {
    let (content, content_range) = slice_content(&raw.content, slice);
    LcmLoadSessionMessage {
        provider: raw.provider,
        message_id: raw.message_id,
        session_id: raw.session_id,
        store_id: raw.store_id,
        role: raw.role,
        ordinal: raw.ordinal,
        timestamp: raw.timestamp,
        content,
        content_range,
        content_hash: raw.content_hash,
        storage_kind: raw.storage_kind,
        payload_ref: raw.payload_ref,
        legacy_source: raw.legacy_source,
        legacy_truncated: raw.legacy_truncated,
        metadata_json: raw.metadata_json,
    }
}

fn slice_content(content: &str, slice: Option<LcmContentSlice>) -> (String, LcmContentRange) {
    let total_chars = content.chars().count();
    let offset = slice.map_or(0, |slice| slice.offset).min(total_chars);
    let limit = slice.map_or(total_chars.saturating_sub(offset), |slice| slice.limit);
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

fn raw_message_with_sliced_content(
    mut raw: LcmRawMessage,
    slice: Option<LcmContentSlice>,
) -> (LcmRawMessage, LcmContentRange) {
    let (content, range) = slice_content(&raw.content, slice);
    raw.content = content;
    (raw, range)
}

fn summary_node_with_sliced_text(
    mut summary: LcmSummaryNode,
    slice: Option<LcmContentSlice>,
) -> (LcmSummaryNode, LcmContentRange) {
    let (summary_text, range) = slice_content(&summary.summary_text, slice);
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
            let (content, range) = slice_content(&source.content, slice);
            source.content.clone_from(&content);
            source.content_range = Some(range);
            if let Some(raw_message) = source.raw_message.as_mut() {
                raw_message.content.clone_from(&content);
            }
            if let Some(summary_node) = source.summary_node.as_mut() {
                summary_node.summary_text = content;
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
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut messages = 0_i64;
    let mut estimated_tokens = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        messages += 1;
        let content: Option<String> = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let snippet: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
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
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut depths = std::collections::BTreeMap::new();
    let mut total_nodes = 0_i64;
    let mut total_tokens = 0_i64;
    let mut total_source_tokens = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let depth: i64 = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let count: i64 = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        let tokens: i64 = row.get(2).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_tokens: i64 = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
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
    // empty DAG (`hermes-lcm/tools.py` lcm_status).
    let compression_ratio = if total_tokens > 0 {
        format!("{:.1}:1", total_source_tokens as f64 / total_tokens as f64)
    } else {
        "0:1".to_string()
    };
    Ok(LcmDagStatus {
        total_nodes,
        total_tokens,
        total_source_tokens,
        compression_ratio,
        depths,
    })
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
    let system = "You answer questions using expanded LCM retrieval context. Be concise, factual, and grounded in the provided context. If the context is insufficient, say so plainly.".to_string();
    let context_json = serde_json::to_string_pretty(context_blocks).unwrap_or_else(|_| "[]".into());
    let truncation_note = if context_truncated {
        "\n\nNOTE: Some LCM context was truncated; pagination metadata is included in the tool response."
    } else {
        ""
    };
    let user = format!("QUESTION:\n{prompt}\n\nEXPANDED CONTEXT:\n{context_json}{truncation_note}");
    LcmExpandQuerySynthesisPrompt { system, user }
}

async fn raw_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let mut values = vec![
        Value::Text(query.to_string()),
        Value::Text(request.provider.clone()),
    ];
    let mut filters = Vec::new();
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
    values.push(Value::Integer(limit as i64));
    let filter_sql = if filters.is_empty() {
        String::new()
    } else {
        format!(" AND {}", filters.join(" AND "))
    };
    let order_by = grep_order_by(request.sort, "r.store_id");
    let sql = format!(
        "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text
         FROM lcm_raw_messages_fts
         JOIN lcm_raw_messages r ON r.store_id = lcm_raw_messages_fts.rowid
         WHERE lcm_raw_messages_fts MATCH ?
           AND r.provider = ?
           {filter_sql}
         ORDER BY {order_by}
         LIMIT ?"
    );
    let mut rows = conn
        .query(&sql, values)
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut hits = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let snippet: String = row.get(4).map_err(|err| LcmError::Db(err.to_string()))?;
        hits.push(LcmGrepHit {
            kind: "raw_message".to_string(),
            provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            session_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            message_id: Some(row.get(2).map_err(|err| LcmError::Db(err.to_string()))?),
            node_id: None,
            store_id: Some(row.get(3).map_err(|err| LcmError::Db(err.to_string()))?),
            snippet: raw::derived_text_for_snippet(&snippet),
        });
    }
    Ok(hits)
}

async fn summary_grep_hits(
    conn: &Connection,
    request: &LcmGrepRequest,
    session_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    let mut values = vec![
        Value::Text(query.to_string()),
        Value::Text(request.provider.clone()),
    ];
    let mut filters = Vec::new();
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
    values.push(Value::Integer(limit as i64));
    let filter_sql = if filters.is_empty() {
        String::new()
    } else {
        format!(" AND {}", filters.join(" AND "))
    };
    let order_by = grep_order_by(request.sort, "n.created_at");
    let sql = format!(
        "SELECT n.provider, n.session_id, n.node_id, n.summary_text
         FROM lcm_summary_nodes_fts
         JOIN lcm_summary_nodes n ON n.rowid = lcm_summary_nodes_fts.rowid
         WHERE lcm_summary_nodes_fts MATCH ?
           AND n.provider = ?
           {filter_sql}
         ORDER BY {order_by}, n.node_id
         LIMIT ?"
    );
    let mut rows = conn
        .query(&sql, values)
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut hits = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let summary_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        hits.push(LcmGrepHit {
            kind: "summary_node".to_string(),
            provider: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            session_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            message_id: None,
            node_id: Some(row.get(2).map_err(|err| LcmError::Db(err.to_string()))?),
            store_id: None,
            snippet: raw::derived_text_for_snippet(&summary_text),
        });
    }
    Ok(hits)
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut overviews = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let storage_kind_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        let content_preview: String = row.get(5).map_err(|err| LcmError::Db(err.to_string()))?;
        let (_, content_range) = slice_content(&content_preview, None);
        overviews.push(LcmRawMessageOverview {
            message_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            store_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            role: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
            storage_kind: LcmStorageKind::from_db(&storage_kind_text).ok_or_else(|| {
                LcmError::Db(format!("invalid storage_kind: {storage_kind_text}"))
            })?,
            payload_ref: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;

    let mut overviews = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let summary_text: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_count: i64 = row.get(5).map_err(|err| LcmError::Db(err.to_string()))?;
        overviews.push(LcmSummaryNodeOverview {
            node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
            conversation_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
            depth: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
            summary_preview: raw::derived_text_for_snippet(&summary_text),
            source_count: source_count.max(0) as usize,
            created_at: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::SummaryNodeNotFound)?;
    let children = describe_summary_sources(conn, provider, session_id, node_id).await?;
    Ok(LcmDescribeSummaryNode {
        node_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        conversation_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        depth: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        summary_token_count: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        source_token_count: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        source_time_start: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        source_time_end: row.get(6).map_err(|err| LcmError::Db(err.to_string()))?,
        expand_hint: row.get(7).map_err(|err| LcmError::Db(err.to_string()))?,
        metadata_json: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        created_at: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let source_kind: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let source_id: String = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
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
                    .await
                    .map_err(|err| LcmError::Db(err.to_string()))?;
                let Some(raw_row) = raw_rows
                    .next()
                    .await
                    .map_err(|err| LcmError::Db(err.to_string()))?
                else {
                    continue;
                };
                let storage_kind_text: String = raw_row
                    .get(1)
                    .map_err(|err| LcmError::Db(err.to_string()))?;
                out.push(LcmDescribeSourceOverview {
                    source_kind,
                    source_ref: LcmSourceRef::RawMessage { store_id },
                    store_id: Some(store_id),
                    node_id: None,
                    role: Some(
                        raw_row
                            .get(0)
                            .map_err(|err| LcmError::Db(err.to_string()))?,
                    ),
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
                    .await
                    .map_err(|err| LcmError::Db(err.to_string()))?;
                let Some(summary_row) = summary_rows
                    .next()
                    .await
                    .map_err(|err| LcmError::Db(err.to_string()))?
                else {
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
                    summary_token_count: Some(
                        summary_row
                            .get(0)
                            .map_err(|err| LcmError::Db(err.to_string()))?,
                    ),
                    source_token_count: Some(
                        summary_row
                            .get(1)
                            .map_err(|err| LcmError::Db(err.to_string()))?,
                    ),
                    expand_hint: summary_row
                        .get(2)
                        .map_err(|err| LcmError::Db(err.to_string()))?,
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
    let mut rows = conn
        .query(
            "SELECT payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
             FROM lcm_external_payloads
             WHERE payload_ref = ?1 AND provider = ?2 AND session_id = ?3",
            params![payload_ref, provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::PayloadNotFound)?;
    let byte_count: i64 = row.get(6).map_err(|err| LcmError::Db(err.to_string()))?;
    let char_count: i64 = row.get(7).map_err(|err| LcmError::Db(err.to_string()))?;
    let message_id: String = row.get(3).map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(LcmDescribeExternalPayload {
        payload_ref: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        message_id: message_id.clone(),
        kind: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        content_hash: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        byte_count: byte_count.max(0) as u64,
        char_count: char_count.max(0) as u64,
        created_at: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        metadata_json: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
        content_preview: external_payload_placeholder_preview(
            conn,
            provider,
            session_id,
            &message_id,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        return row.get(0).map_err(|err| LcmError::Db(err.to_string()));
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    else {
        return Ok((None, None));
    };
    Ok((
        row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
    ))
}

async fn count_raw_messages(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await
}

async fn count_summary_nodes(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await
}

async fn count_external_payloads(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    count_by_provider_session(conn, "lcm_external_payloads", provider, session_id).await
}

async fn count_by_provider_session(
    conn: &Connection,
    table: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let sql = format!(
        "SELECT COUNT(*) FROM {table} WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)"
    );
    let mut rows = conn
        .query(&sql, params![provider, session_value])
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_missing_payloads(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let dir = payload::payload_dir(storage_root);
    let mut missing = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        if payload::validate_payload_ref(&payload_ref).is_err() || !dir.join(&payload_ref).is_file()
        {
            missing += 1;
        }
    }
    Ok(missing)
}

async fn count_unreferenced_payloads(
    conn: &Connection,
    storage_root: &Path,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut referenced = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        referenced.insert(payload_ref);
    }

    let dir = payload::payload_dir(storage_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(0);
    };

    let mut unreferenced = 0_i64;
    for entry in entries {
        let entry = entry.map_err(|err| LcmError::Io(err.to_string()))?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|err| LcmError::Io(err.to_string()))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            unreferenced += 1;
            continue;
        };
        if payload::validate_payload_ref(&file_name).is_err() || !referenced.contains(&file_name) {
            unreferenced += 1;
        }
    }
    Ok(unreferenced)
}

async fn placeholder_payload_status(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<PlaceholderPayloadStatus, LcmError> {
    let metadata_refs = metadata_refs_for_scope(conn, provider, session_id).await?;
    let placeholder_refs = placeholder_refs_for_scope(conn, provider, session_id).await?;
    let dir = payload::payload_dir(storage_root);
    let mut missing_metadata_count = 0_i64;
    let mut missing_file_count = 0_i64;
    for payload_ref in &placeholder_refs {
        if !metadata_refs.contains(payload_ref) {
            missing_metadata_count += 1;
        }
        if payload::validate_payload_ref(payload_ref).is_err() || !dir.join(payload_ref).is_file() {
            missing_file_count += 1;
        }
    }
    Ok(PlaceholderPayloadStatus {
        placeholder_ref_count: placeholder_refs.len() as i64,
        missing_metadata_count,
        missing_file_count,
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
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        refs.insert(row.get(0).map_err(|err| LcmError::Db(err.to_string()))?);
    }
    Ok(refs)
}

async fn placeholder_refs_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        for index in 0..4 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            if let Some(value) = value {
                refs.extend(payload::extract_payload_refs_from_text(&value));
            }
        }
    }
    Ok(refs)
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

async fn count_lifecycle_states(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("lifecycle count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn load_lifecycle_metadata(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmLifecycleMetadata, LcmError> {
    let session_value = opt_text(session_id);
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    else {
        return Ok(LcmLifecycleMetadata {
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        });
    };
    Ok(LcmLifecycleMetadata {
        current_session_id: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        current_frontier_store_id: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        last_finalized_frontier_store_id: row
            .get(3)
            .map_err(|err| LcmError::Db(err.to_string()))?,
    })
}

async fn count_frontier_rows(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2)
               AND current_frontier_store_id IS NOT NULL",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("frontier count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_legacy_truncated(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let session_value = opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND legacy_truncated != 0",
            params![provider, session_value],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("legacy truncated count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_lossy_ingest_records(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND metadata_json IS NOT NULL",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut count = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let metadata: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        if serde_json::from_str::<serde_json::Value>(&metadata)
            .ok()
            .and_then(|value| value.get("ingest_protection").cloned())
            .and_then(|value| value.get("lossy").and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
        {
            count += 1;
        }
    }
    Ok(count)
}

async fn load_raw_message_by_store_id(
    conn: &Connection,
    store_id: i64,
) -> Result<LcmRawMessage, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE store_id = ?1",
            params![store_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
    raw_message_from_row(&row)
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

fn scoped_session_filter(scope: LcmScope, session_id: Option<&str>) -> Option<&str> {
    match scope {
        LcmScope::All => None,
        LcmScope::Current | LcmScope::Session => session_id,
    }
}

fn fts_query(query: &str) -> String {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }

    terms
        .into_iter()
        .map(|term| quote_fts_term(&term))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_fts_term(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
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

fn grep_order_by(sort: LcmGrepSort, recency_column: &str) -> String {
    match sort {
        LcmGrepSort::Recency => format!("{recency_column} DESC"),
        LcmGrepSort::Relevance | LcmGrepSort::Hybrid => {
            format!("rank, {recency_column} DESC")
        }
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

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}
