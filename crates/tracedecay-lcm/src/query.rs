use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_runtime_core::db::engine::{QueryExecutor, Value, params};
use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_temporal_query::context::OrderedTextContextAssembler;

mod describe;
mod expand;
mod grep;
mod payload_health;
mod scope;
mod session;
mod status;

use describe::*;
pub use expand::expand;
use expand::{expand_query_match_from_hit, expand_query_synthesis_prompt, is_noise_block_content};
pub use grep::grep;
use grep::{contains_cjk, raw_grep_hits, summary_grep_hits};
pub use payload_health::*;
pub use session::*;
pub use status::store_status;
use status::*;

use super::types::LcmStoreTokenCoverage;
use super::types::{
    LcmGrepOutcome, LcmLifecycleStatus, LcmPayloadCoverage, LcmPayloadCoverageState,
    LcmPayloadGcStatus, LcmPayloadStatus, LcmRedactionStatus,
};
use super::{
    LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS, LCM_DEFAULT_FRESH_TAIL_COUNT,
    LCM_DEFAULT_SUMMARY_FAN_IN, LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LCM_SCHEMA_VERSION,
    LcmConfigStatus, LcmContentRange, LcmContentSlice, LcmDagDepthStatus, LcmDagStatus,
    LcmDescribeExternalPayload, LcmDescribeRequest, LcmDescribeResponse, LcmDescribeSourceOverview,
    LcmDescribeSummaryNode, LcmDescribeTarget, LcmError, LcmExpandQueryBudget,
    LcmExpandQueryContextBlock, LcmExpandQueryMatch, LcmExpandQueryPagination,
    LcmExpandQueryRequest, LcmExpandQueryResponse, LcmExpandQuerySynthesisPrompt, LcmExpandRequest,
    LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget, LcmExpandedSummarySource,
    LcmGcConfig, LcmGrepFilters, LcmGrepHit, LcmGrepRequest, LcmGrepSort, LcmLoadSessionMessage,
    LcmLoadSessionPage, LcmLoadSessionRequest, LcmRawMessage, LcmRawMessageOverview,
    LcmRecentSession, LcmReplayMessage, LcmReplaySummaryNode, LcmScope, LcmSessionReplayRequest,
    LcmSessionReplaySlice, LcmSourceRef, LcmStatus, LcmStorageKind, LcmStoreStatus,
    LcmSummaryConvergenceStatus, LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeOverview, dag,
    gc, maintenance, payload, raw, schema, util,
};

const MAX_PAGE_LIMIT: usize = 100;
const TERM_SEPARATORS: [char; 4] = ['-', ':', '/', '#'];
const RAW_GREP_RECENCY_EXPR: &str = "COALESCE(r.timestamp, r.store_id)";
const SUMMARY_GREP_RECENCY_EXPR: &str =
    "COALESCE(n.source_time_end, n.source_time_start, n.created_at)";
const RAW_ROLE_PENALTY_CASE: &str =
    "CASE r.role WHEN 'user' THEN 0 WHEN 'assistant' THEN 1 WHEN 'tool' THEN 2 ELSE 1 END";
/// Maximum grep hits retained per session in a cross-session (`scope: all`)
/// page. Keeps one noisy session (e.g. a review session full of transcript
/// inventory tool calls) from flooding the page and crowding out distinct
/// sessions. Single-session scopes (`current`/`session`) are exempt — capping
/// there would silently drop legitimate same-session recall.
const PER_SESSION_HIT_CAP: usize = 3;

/// Fetch budget before the re-rank stage, bounded by [`MAX_PAGE_LIMIT`].
fn rerank_fetch_limit(limit: usize) -> usize {
    crate::retrieval_content::rerank_fetch_limit(limit, MAX_PAGE_LIMIT)
}

#[hotpath::measure(label = "sessions.lcm.expand_query", future = true)]
pub async fn expand_query(
    conn: &(impl QueryExecutor + ?Sized),
    request: LcmExpandQueryRequest,
) -> Result<LcmExpandQueryResponse, LcmError> {
    let max_results = clamp_limit(request.max_results);
    let context_max_chars = request.context_max_tokens.max(1);
    let mut matches = Vec::new();
    let mut selected_summaries = Vec::new();
    let mut selected_raw_identities = Vec::new();

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
                    git_filter: crate::GitScopeFilter::default(),
                };
                let summary_hits = hotpath::future!(
                    summary_grep_hits(
                        conn,
                        &grep_request,
                        &LcmGrepFilters::default(),
                        Some(&request.session_id),
                        grep::LcmGitScopeSessions::Unscoped,
                        &query_plan,
                        max_results,
                    ),
                    label = "sessions.lcm.expand_query.search"
                )
                .await?;
                for hit in summary_hits {
                    if let Some(node_id) = hit.node_id.as_deref() {
                        let expansion = hotpath::future!(
                            dag::expand_summary_node(
                                conn,
                                &request.provider,
                                &request.session_id,
                                node_id,
                            ),
                            label = "sessions.lcm.expand_query.hydrate"
                        )
                        .await?;
                        matches.push(expand_query_match_from_hit(&hit));
                        selected_summaries.push(expansion);
                    }
                }

                if selected_summaries.len() < max_results {
                    let remaining = max_results - selected_summaries.len();
                    let raw_hits = hotpath::future!(
                        raw_grep_hits(
                            conn,
                            &grep_request,
                            &LcmGrepFilters::default(),
                            Some(&request.session_id),
                            grep::LcmGitScopeSessions::Unscoped,
                            &query_plan,
                            remaining,
                        ),
                        label = "sessions.lcm.expand_query.search"
                    )
                    .await?;
                    for hit in raw_hits {
                        if hit.store_id.is_some()
                            && let Some(message_id) = hit.message_id.as_deref()
                        {
                            matches.push(expand_query_match_from_hit(&hit));
                            selected_raw_identities
                                .push((hit.provider.clone(), message_id.to_owned()));
                        }
                    }
                }
            }
        }
    } else {
        // Explicitly requested nodes are hydrated as one page: the whole set's
        // node rows, lineage rows, and source closure are each loaded once,
        // instead of one independent expansion per node id.
        let requested_node_ids = request
            .node_ids
            .iter()
            .take(max_results)
            .cloned()
            .collect::<Vec<_>>();
        let expansions = hotpath::future!(
            dag::expand_summary_nodes(
                conn,
                &request.provider,
                &request.session_id,
                &requested_node_ids,
            ),
            label = "sessions.lcm.expand_query.hydrate"
        )
        .await?;
        for expansion in expansions {
            matches.push(LcmExpandQueryMatch {
                kind: "summary_node".to_string(),
                node_id: Some(expansion.summary.node_id.clone()),
                store_id: None,
                snippet: raw::derived_text_for_snippet(&expansion.summary.summary_text),
            });
            selected_summaries.push(expansion);
        }
    }

    if selected_summaries.is_empty() && selected_raw_identities.is_empty() {
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

    let selected_raws = hotpath::future!(
        raw::load_raw_messages_by_identity(conn, &selected_raw_identities),
        label = "sessions.lcm.expand_query.hydrate"
    )
    .await?;
    let mut hydrated_raws = Vec::with_capacity(selected_raws.len());
    for raw in selected_raws {
        if raw.provider == request.provider && raw.session_id == request.session_id {
            hydrated_raws.push(raw);
        }
    }

    let (
        node_ids,
        used_chars,
        context_blocks,
        context_pagination,
        context_truncated,
        synthesis_prompt,
    ) = hotpath::measure_block!("sessions.lcm.expand_query.assemble", {
        let mut assembler = ExpandQueryAssembler::new(context_max_chars);
        let mut node_ids = Vec::new();
        for expansion in selected_summaries {
            node_ids.push(expansion.summary.node_id.clone());
            assembler.add_summary_expansion(expansion);
        }
        for raw in hydrated_raws {
            assembler.add_raw_message(raw, None);
        }
        let used_chars = assembler.used_chars();
        let context_blocks = assembler.context_blocks;
        let context_pagination = assembler.context_pagination;
        let context_truncated = !context_pagination.is_empty();
        let synthesis_prompt =
            expand_query_synthesis_prompt(&request.prompt, &context_blocks, context_truncated);
        (
            node_ids,
            used_chars,
            context_blocks,
            context_pagination,
            context_truncated,
            synthesis_prompt,
        )
    });

    let context_budget = LcmExpandQueryBudget {
        requested_max_chars: context_max_chars,
        used_chars,
    };

    crate::metrics::record_lcm_retrieval(matches.len());
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

pub async fn describe(
    conn: &(impl QueryExecutor + ?Sized),
    request: LcmDescribeRequest,
) -> Result<LcmDescribeResponse, LcmError> {
    let provider = request.provider.as_str();
    let session_id = request.session_id.as_str();
    let session_store = if matches!(request.target, LcmDescribeTarget::Session) {
        Some(store_status(conn, provider, Some(session_id)).await?)
    } else {
        None
    };
    let raw_message_count = match session_store.as_ref() {
        Some(store) => store.messages,
        None => count_raw_messages(conn, provider, Some(session_id)).await?,
    };
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

    let session_token_estimate = session_store.and_then(|store| {
        store
            .token_estimate
            .complete
            .then_some(store.estimated_tokens)
    });
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
        session_token_estimate,
    })
}

pub async fn status(
    conn: &(impl QueryExecutor + ?Sized),
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

async fn lcm_table_exists(
    conn: &(impl QueryExecutor + ?Sized),
    table_name: &str,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM sqlite_master
             WHERE type = 'table' AND name = ?1",
            params![table_name],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("LCM table existence query returned no rows".to_string()))?;
    Ok(row.get::<i64>(0)? > 0)
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

/// Pages a summary node's immediate source list. The internal offset clamps to
/// the source count and an omitted limit returns all remaining sources; the
/// temporal service authenticates the internal next boundary before exposing
/// an opaque cursor.
fn paginate_summary_sources(
    sources: Vec<LcmExpandedSummarySource>,
    source_offset: usize,
    source_limit: Option<usize>,
) -> (Vec<LcmExpandedSummarySource>, LcmExpandSourcePagination) {
    let total_sources = sources.len();
    let source_offset = source_offset.min(total_sources);
    let remaining = total_sources - source_offset;
    let source_limit = source_limit.map_or(remaining, |limit| limit.max(1).min(remaining));
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

struct ExpandQueryAssembler {
    context_blocks: Vec<LcmExpandQueryContextBlock>,
    context_pagination: Vec<LcmExpandQueryPagination>,
    context: OrderedTextContextAssembler,
}

impl ExpandQueryAssembler {
    fn new(max_chars: usize) -> Self {
        Self {
            context_blocks: Vec::new(),
            context_pagination: Vec::new(),
            context: OrderedTextContextAssembler::new(max_chars),
        }
    }

    fn used_chars(&self) -> usize {
        self.context.used_chars()
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
        // Drop pure machine-noise blocks (base64 thinking-signature blobs and
        // other binary-ish payloads) before they consume the context budget or
        // pollute the synthesized answer. Dropping is silent — no pagination
        // entry — because there is nothing meaningful to resume.
        if is_noise_block_content(content) {
            return None;
        }
        let admitted = self.context.admit(content);
        let Some(content) = admitted.content else {
            self.context_pagination.push(LcmExpandQueryPagination {
                kind: kind.to_string(),
                node_id,
                source_ref,
                state: None,
                next_content_offset: admitted.next_content_offset,
                has_more: admitted.truncated,
            });
            return None;
        };
        let range = LcmContentRange {
            offset: 0,
            limit: admitted.limit,
            returned_chars: admitted.returned_chars,
            total_chars: admitted.total_chars,
            truncated: admitted.truncated,
        };
        if admitted.truncated {
            self.context_pagination.push(LcmExpandQueryPagination {
                kind: kind.to_string(),
                node_id,
                source_ref,
                state: None,
                next_content_offset: admitted.next_content_offset,
                has_more: true,
            });
        }
        Some((content, range))
    }
}

async fn raw_store_bounds(
    conn: &(impl QueryExecutor + ?Sized),
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
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await
}

async fn count_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await
}

async fn count_external_payloads(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_external_payloads", provider, session_id).await
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
        requires_like_fallback,
    }
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
    let lower_text = text.to_ascii_lowercase();
    let mut best_match = None;
    for term in terms {
        if term.is_empty() {
            continue;
        }
        if let Some(byte_idx) = find_term(text, &lower_text, term) {
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

fn find_term(text: &str, lower_text: &str, term: &str) -> Option<usize> {
    if term.is_ascii() {
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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use tracedecay_runtime_core::db::engine::{
        Executor, IntoParams, QueryExecutor, Result as EngineResult, Row, Rows, TestConnection,
        Value, params, params_from_iter,
    };

    use super::*;

    struct CountingQuery<'a> {
        inner: &'a TestConnection,
        queries: Cell<usize>,
        executed_queries: RefCell<Vec<(String, Vec<Value>)>>,
    }

    impl<'a> CountingQuery<'a> {
        fn new(inner: &'a TestConnection) -> Self {
            Self {
                inner,
                queries: Cell::new(0),
                executed_queries: RefCell::new(Vec::new()),
            }
        }
    }

    impl QueryExecutor for CountingQuery<'_> {
        async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
        where
            P: IntoParams,
        {
            self.queries.set(self.queries.get() + 1);
            let values = params.into_params()?;
            self.executed_queries
                .borrow_mut()
                .push((sql.to_string(), values.clone()));
            let mut rows = self.inner.query(sql, params_from_iter(values)).await?;
            let columns = (0..rows.column_count())
                .map(|index| rows.column_name(index).unwrap_or_default().to_string())
                .collect::<Vec<_>>();
            let mut replay = Vec::new();
            while let Some(row) = rows.next().await? {
                let mut values = Vec::new();
                let mut column = 0_i32;
                while let Ok(value) = row.get::<Value>(column) {
                    values.push(value);
                    column += 1;
                }
                replay.push(Row::from_values(values));
            }
            Ok(Rows::from_parts(columns, replay))
        }
    }

    async fn query_test_store() -> (tempfile::TempDir, TestConnection) {
        let temp = tempfile::tempdir().expect("temporary query store");
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                parent_session_id TEXT,
                is_subagent INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(provider, session_id)
            );",
        )
        .await
        .expect("session schema");
        schema::ensure_lcm_schema(&conn).await.expect("LCM schema");
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'session-a', '/p', '/p')",
            (),
        )
        .await
        .expect("session fixture");
        (temp, conn)
    }

    async fn query_plan_lines(conn: &TestConnection, sql: &str, values: Vec<Value>) -> Vec<String> {
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {sql}"),
                params_from_iter(values),
            )
            .await
            .expect("explain query plan");
        let mut lines = Vec::new();
        while let Some(row) = rows.next().await.expect("read query plan row") {
            lines.push(row.get::<String>(3).expect("query plan detail"));
        }
        assert!(!lines.is_empty(), "query plan must not be empty");
        lines
    }

    fn raw_like_candidate_query(counted: &CountingQuery<'_>) -> (String, Vec<Value>) {
        counted
            .executed_queries
            .borrow()
            .iter()
            .find(|(sql, _)| sql.contains("SELECT r.store_id"))
            .cloned()
            .expect("unsafe LIKE grep must admit indexed raw candidates first")
    }

    fn sqlite_value(value: &Value) -> rusqlite::types::Value {
        match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Integer(value) => rusqlite::types::Value::Integer(*value),
            Value::Real(value) => rusqlite::types::Value::Real(*value),
            Value::Text(value) => rusqlite::types::Value::Text(value.clone()),
            Value::Blob(value) => rusqlite::types::Value::Blob(value.clone()),
        }
    }

    /// Counts actual SQLite virtual-machine steps for the exact candidate SQL,
    /// rather than the rows materialized by the engine test adapter.
    fn candidate_vm_steps(database_path: &Path, sql: &str, values: &[Value]) -> usize {
        let connection = rusqlite::Connection::open(database_path)
            .expect("open native SQLite connection for candidate measurement");
        let steps = Arc::new(AtomicUsize::new(0));
        let counted_steps = Arc::clone(&steps);
        connection
            .progress_handler(
                1,
                Some(move || {
                    counted_steps.fetch_add(1, Ordering::Relaxed);
                    false
                }),
            )
            .expect("install SQLite VM progress handler");
        {
            let mut statement = connection
                .prepare(sql)
                .expect("prepare candidate measurement statement");
            let native_values = values.iter().map(sqlite_value).collect::<Vec<_>>();
            let mut rows = statement
                .query(rusqlite::params_from_iter(native_values))
                .expect("execute candidate measurement statement");
            while rows
                .next()
                .expect("advance candidate measurement statement")
                .is_some()
            {}
        }
        connection
            .progress_handler(1, None::<fn() -> bool>)
            .expect("clear SQLite VM progress handler");
        steps.load(Ordering::Relaxed)
    }

    async fn insert_query_test_raw(
        temp: &tempfile::TempDir,
        conn: &TestConnection,
        provider: &str,
        message_id: &str,
        ordinal: i64,
        text: &str,
    ) {
        let message = tracedecay_store::SessionMessageRecord {
            provider: provider.to_owned(),
            message_id: message_id.to_owned(),
            session_id: "session-a".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(ordinal),
            ordinal,
            text: text.to_owned(),
            kind: Some("message".to_owned()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        };
        let mut rollback = payload::PayloadFileRollback::begin_cancellation_safe(temp.path());
        raw::upsert_raw_message_with_payload_tracked(conn, temp.path(), &message, &mut rollback)
            .await
            .expect("raw query fixture");
        rollback.disarm();
    }

    fn production_grep_fixture_rows() -> i64 {
        std::env::var("TRACEDECAY_LCM_GREP_FIXTURE_ROWS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|rows| *rows > 0)
            .unwrap_or(244_016)
    }

    async fn seed_production_grep_corpus(conn: &TestConnection, rows: i64) {
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES
                ('cursor', 'session-direct-user', '/p', '/p'),
                ('cursor', 'session-single', '/p', '/p')",
            (),
        )
        .await
        .expect("production grep sessions");
        let mut seeded = 0_i64;
        while seeded < rows {
            let batch = 25_000.min(rows - seeded);
            conn.execute(
                &format!(
                    "WITH RECURSIVE fixture(value) AS (
                         SELECT {start} UNION ALL
                         SELECT value + 1 FROM fixture WHERE value < {end}
                     )
                     INSERT INTO lcm_raw_messages (
                         provider, message_id, session_id, role, ordinal, timestamp,
                         content, content_hash, storage_kind, snippet_text, index_text
                     )
                     SELECT 'cursor',
                            printf('background-%09d', value),
                            'session-a',
                            'assistant',
                            value,
                            value,
                            'retained background history',
                            printf('hash-%09d', value),
                            'inline',
                            'retained background history',
                            'retained background history'
                     FROM fixture",
                    start = seeded + 1,
                    end = seeded + batch,
                ),
                (),
            )
            .await
            .expect("production grep raw batch");
            seeded += batch;
        }
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, snippet_text, index_text
             ) VALUES
                ('cursor', 'direct-user-match', 'session-direct-user', 'user', ?1, ?1,
                 'unique:needle direct user', 'direct-user-hash', 'inline',
                 'unique:needle direct user', 'unique:needle direct user'),
                ('cursor', 'single-session-match', 'session-single', 'assistant', ?2, ?2,
                 'unique:needle single session', 'single-session-hash', 'inline',
                 'unique:needle single session', 'unique:needle single session')",
            params![rows + 1, rows + 2],
        )
        .await
        .expect("production grep matches");
    }

    fn summary_source(store_id: i64) -> LcmExpandedSummarySource {
        LcmExpandedSummarySource {
            source_ref: LcmSourceRef::RawMessage { store_id },
            state: tracedecay_domain::HydrationStateV1::Available,
            content: format!("source-{store_id}"),
            content_range: None,
            content_truncated: false,
            raw_message: None,
            raw_message_metadata: None,
            summary_node: None,
        }
    }

    #[test]
    fn explicit_zero_summary_page_limit_still_advances() {
        let (page, pagination) =
            paginate_summary_sources(vec![summary_source(1), summary_source(2)], 0, Some(0));

        assert_eq!(page.len(), 1);
        assert_eq!(pagination.next_source_offset, Some(1));
        assert!(pagination.has_more);
    }

    #[tokio::test]
    async fn unsafe_like_fallback_reads_only_the_bounded_scope_candidate_set() {
        let (_temp, conn) = query_test_store().await;
        for ordinal in 0..8_i64 {
            let message_id = format!("message-{ordinal}");
            conn.execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, snippet_text, index_text
                 ) VALUES (
                    'cursor', ?1, 'session-a', 'assistant', ?2, ?2,
                    'alert:marker candidate', 'hash', 'inline',
                    'alert:marker candidate', 'alert:marker candidate'
                 )",
                params![message_id, ordinal],
            )
            .await
            .expect("raw candidate");
        }

        let counted = CountingQuery::new(&conn);
        let outcome = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::Session,
                session_id: Some("session-a".to_string()),
                include_summaries: false,
                limit: 2,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters::default(),
            None,
        )
        .await
        .expect("unsafe LIKE grep");

        assert_eq!(outcome.hits.len(), 2);
        assert_eq!(counted.queries.get(), 2);
        let (candidate_sql, candidate_values) = counted
            .executed_queries
            .borrow()
            .first()
            .cloned()
            .expect("scope candidate query");
        assert!(
            !candidate_sql.contains("LIKE"),
            "candidate admission must not read retained message text: {candidate_sql}"
        );
        let plan = query_plan_lines(&conn, &candidate_sql, candidate_values).await;
        assert!(
            plan.iter()
                .any(|line| line.contains("idx_lcm_raw_session_order")),
            "single-session candidate must use the maintained session index; plan: {plan:?}"
        );
    }

    #[tokio::test]
    async fn unsafe_direct_user_grep_uses_bounded_candidates_before_exact_matching() {
        let (_temp, conn) = query_test_store().await;
        for ordinal in 0..16_i64 {
            let message_id = format!("message-{ordinal}");
            let (role, text) = match ordinal {
                12 => ("user", "alert:marker exact user match"),
                13 => ("user", "prealertness keeps LIKE infix recall"),
                _ => ("assistant", "alert marker assistant context"),
            };
            conn.execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, snippet_text, index_text
                 ) VALUES (
                    'cursor', ?1, 'session-a', ?2, ?3, ?3,
                    ?4, 'hash', 'inline', ?4, ?4
                 )",
                params![message_id, role, ordinal, text],
            )
            .await
            .expect("raw candidate");
        }

        let counted = CountingQuery::new(&conn);
        let outcome = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::All,
                session_id: None,
                include_summaries: false,
                limit: 2,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters {
                relationship_scope: crate::SessionSearchScope::All,
                message_type: crate::SessionMessageType::DirectUser,
            },
            None,
        )
        .await
        .expect("unsafe direct-user grep");

        assert_eq!(
            outcome
                .hits
                .iter()
                .filter_map(|hit| hit.message_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["message-13", "message-12"],
            "candidate selection must preserve punctuation and infix LIKE recall"
        );
        let (candidate_sql, candidate_values) = raw_like_candidate_query(&counted);
        assert!(
            !candidate_sql.contains("LIKE"),
            "direct-user candidate admission must not read retained message text: {candidate_sql}"
        );
        assert!(
            !candidate_sql.contains("metadata_json")
                && !candidate_sql.contains("json_each")
                && !candidate_sql.contains("EXISTS"),
            "direct-user candidate admission must use only the indexed user-role superset: {candidate_sql}"
        );
        let plan = query_plan_lines(&conn, &candidate_sql, candidate_values).await;
        assert!(
            plan.iter()
                .any(|line| line.contains("idx_lcm_raw_direct_user_candidate")),
            "direct-user candidate must use the maintained user-role index; plan: {plan:?}"
        );
    }

    #[tokio::test]
    async fn unsafe_like_source_filter_refuses_before_unbounded_candidate_admission() {
        let (_temp, conn) = query_test_store().await;
        let counted = CountingQuery::new(&conn);

        let result = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::All,
                session_id: None,
                include_summaries: false,
                limit: 1,
                sort: LcmGrepSort::Recency,
                source: Some("cursor".to_string()),
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters::default(),
            None,
        )
        .await;

        assert!(matches!(result, Err(LcmError::BudgetExhausted)));
        assert_eq!(
            counted.queries.get(),
            0,
            "source metadata uses json_extract/LIKE and must refuse before any raw-corpus candidate scan"
        );
    }

    #[tokio::test]
    async fn unsafe_direct_user_grep_refuses_when_its_candidate_index_is_unavailable() {
        let (_temp, conn) = query_test_store().await;
        conn.execute_batch("DROP INDEX idx_lcm_raw_direct_user_candidate;")
            .await
            .expect("remove candidate index from fixture");
        let counted = CountingQuery::new(&conn);

        let result = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::All,
                session_id: None,
                include_summaries: false,
                limit: 1,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters {
                relationship_scope: crate::SessionSearchScope::All,
                message_type: crate::SessionMessageType::DirectUser,
            },
            None,
        )
        .await;

        assert!(matches!(result, Err(LcmError::BudgetExhausted)));
        assert_eq!(counted.queries.get(), 1);
        let (candidate_sql, _) = raw_like_candidate_query(&counted);
        assert!(
            candidate_sql.contains("INDEXED BY idx_lcm_raw_direct_user_candidate"),
            "candidate admission must fail closed rather than let SQLite choose a table scan: {candidate_sql}"
        );
    }

    #[tokio::test]
    async fn unsafe_like_relationship_scope_without_indexed_authority_refuses_before_query() {
        let (_temp, conn) = query_test_store().await;
        let counted = CountingQuery::new(&conn);

        let result = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::All,
                session_id: None,
                include_summaries: false,
                limit: 1,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters {
                relationship_scope: crate::SessionSearchScope::ParentsOnly,
                message_type: crate::SessionMessageType::All,
            },
            None,
        )
        .await;

        assert!(matches!(result, Err(LcmError::BudgetExhausted)));
        assert_eq!(
            counted.queries.get(),
            0,
            "relationship EXISTS must not become an all-corpus candidate admission scan"
        );
    }

    #[tokio::test]
    async fn unsafe_like_candidate_retains_a_lossless_content_tail_match() {
        let (_temp, conn) = query_test_store().await;
        let content = format!(
            "{}alert:marker lossless tail",
            "filler ".repeat(crate::MAX_DERIVED_TEXT_CHARS)
        );
        let index_text = crate::derived_text_for_index(&content);
        assert!(
            !index_text.contains("alert:marker"),
            "fixture must place the exact term beyond the FTS-derived text cap"
        );
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, snippet_text, index_text
             ) VALUES (
                'cursor', 'tail-match', 'session-a', 'assistant', 1, 1,
                ?1, 'hash', 'inline', ?2, ?3
             )",
            params![
                content,
                crate::retrieval_content::derived_text_for_snippet(&index_text),
                index_text
            ],
        )
        .await
        .expect("lossless raw fixture");

        let outcome = grep(
            &conn,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "alert:marker".to_string(),
                scope: LcmScope::Session,
                session_id: Some("session-a".to_string()),
                include_summaries: false,
                limit: 1,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters::default(),
            None,
        )
        .await
        .expect("unsafe lossless-tail grep");

        assert_eq!(
            outcome
                .hits
                .iter()
                .filter_map(|hit| hit.message_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["tail-match"],
            "a complete scope candidate set must preserve exact lossless-tail recall"
        );
    }

    #[tokio::test]
    async fn unsafe_like_scope_overflow_is_a_typed_budget_refusal() {
        let (_temp, conn) = query_test_store().await;
        for ordinal in 0..17_i64 {
            conn.execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, snippet_text, index_text
                 ) VALUES (
                    'cursor', ?1, 'session-a', 'assistant', ?2, ?2,
                    '雪 candidate', 'hash', 'inline', '雪 candidate', '雪 candidate'
                 )",
                params![format!("message-{ordinal}"), ordinal],
            )
            .await
            .expect("overflow candidate");
        }
        let counted = CountingQuery::new(&conn);
        let result = grep(
            &counted,
            LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "雪".to_string(),
                scope: LcmScope::Session,
                session_id: Some("session-a".to_string()),
                include_summaries: false,
                limit: 1,
                sort: LcmGrepSort::Recency,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            },
            LcmGrepFilters::default(),
            None,
        )
        .await;

        assert!(
            matches!(result, Err(LcmError::BudgetExhausted)),
            "an incomplete scope must refuse rather than scan the full corpus"
        );
        assert_eq!(
            counted.queries.get(),
            1,
            "overflow must stop after candidate admission, before exact LIKE verification"
        );
    }

    #[tokio::test]
    #[ignore = "production-sized retained corpus; run explicitly"]
    async fn production_sized_unsafe_grep_is_candidate_bounded() {
        let (temp, conn) = query_test_store().await;
        seed_production_grep_corpus(&conn, production_grep_fixture_rows()).await;

        let direct_user = CountingQuery::new(&conn);
        let direct_outcome = tokio::time::timeout(
            Duration::from_secs(5),
            grep(
                &direct_user,
                LcmGrepRequest {
                    provider: "cursor".to_string(),
                    query: "unique:needle".to_string(),
                    scope: LcmScope::All,
                    session_id: None,
                    include_summaries: false,
                    limit: 1,
                    sort: LcmGrepSort::Recency,
                    source: None,
                    role: None,
                    start_time: None,
                    end_time: None,
                    git_filter: Default::default(),
                },
                LcmGrepFilters {
                    relationship_scope: crate::SessionSearchScope::All,
                    message_type: crate::SessionMessageType::DirectUser,
                },
                None,
            ),
        )
        .await
        .expect("direct-user candidate grep must be cancellable within five seconds")
        .expect("direct-user candidate grep");
        assert_eq!(
            direct_outcome
                .hits
                .iter()
                .filter_map(|hit| hit.message_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["direct-user-match"]
        );
        assert_eq!(direct_user.queries.get(), 2);
        let (direct_candidate_sql, direct_candidate_values) =
            raw_like_candidate_query(&direct_user);
        let direct_candidate_plan = query_plan_lines(
            &conn,
            &direct_candidate_sql,
            direct_candidate_values.clone(),
        )
        .await;
        assert!(
            direct_candidate_plan
                .iter()
                .any(|line| line.contains("idx_lcm_raw_direct_user_candidate")),
            "direct-user candidate must seek the maintained user-role index: {direct_candidate_plan:?}"
        );
        let direct_candidate_steps = candidate_vm_steps(
            &temp.path().join("sessions.db"),
            &direct_candidate_sql,
            &direct_candidate_values,
        );
        let direct_candidate_limit = rerank_fetch_limit(rerank_fetch_limit(1)) + 1;
        assert!(
            direct_candidate_steps <= direct_candidate_limit * 32,
            "direct-user candidate used {direct_candidate_steps} SQLite VM steps for {direct_candidate_limit} admitted rows; it must be bounded by the indexed candidate set"
        );

        let single_session = CountingQuery::new(&conn);
        let session_outcome = tokio::time::timeout(
            Duration::from_secs(5),
            grep(
                &single_session,
                LcmGrepRequest {
                    provider: "cursor".to_string(),
                    query: "unique:needle".to_string(),
                    scope: LcmScope::Session,
                    session_id: Some("session-single".to_string()),
                    include_summaries: false,
                    limit: 1,
                    sort: LcmGrepSort::Recency,
                    source: None,
                    role: None,
                    start_time: None,
                    end_time: None,
                    git_filter: Default::default(),
                },
                LcmGrepFilters::default(),
                None,
            ),
        )
        .await
        .expect("single-session candidate grep must be cancellable within five seconds")
        .expect("single-session candidate grep");
        assert_eq!(
            session_outcome
                .hits
                .iter()
                .filter_map(|hit| hit.message_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["single-session-match"]
        );
        assert_eq!(single_session.queries.get(), 2);
        let (session_candidate_sql, session_candidate_values) =
            raw_like_candidate_query(&single_session);
        let session_candidate_plan = query_plan_lines(
            &conn,
            &session_candidate_sql,
            session_candidate_values.clone(),
        )
        .await;
        assert!(
            session_candidate_plan
                .iter()
                .any(|line| line.contains("idx_lcm_raw_session_order")),
            "single-session candidate must seek the maintained session index: {session_candidate_plan:?}"
        );
        let session_candidate_steps = candidate_vm_steps(
            &temp.path().join("sessions.db"),
            &session_candidate_sql,
            &session_candidate_values,
        );
        let session_candidate_limit = rerank_fetch_limit(rerank_fetch_limit(1)) + 1;
        assert!(
            session_candidate_steps <= session_candidate_limit * 32,
            "single-session candidate used {session_candidate_steps} SQLite VM steps for {session_candidate_limit} admitted rows; it must be bounded by the indexed candidate set"
        );
    }

    #[tokio::test]
    async fn expand_query_batches_ranked_raw_hydration_roundtrips() {
        let (temp, conn) = query_test_store().await;
        for ordinal in 0..8_i64 {
            insert_query_test_raw(
                &temp,
                &conn,
                "cursor",
                &format!("message-{ordinal}"),
                ordinal,
                &format!("orchard detail {ordinal}"),
            )
            .await;
        }

        let counted = CountingQuery::new(&conn);
        let response = expand_query(
            &counted,
            LcmExpandQueryRequest {
                provider: "cursor".to_string(),
                session_id: "session-a".to_string(),
                prompt: "summarize orchard".to_string(),
                query: Some("orchard".to_string()),
                node_ids: Vec::new(),
                max_results: 8,
                max_tokens: 100,
                context_max_tokens: 10_000,
            },
        )
        .await
        .expect("expand raw query");

        assert_eq!(response.context_blocks.len(), 8);
        assert_eq!(
            response
                .context_blocks
                .iter()
                .map(|block| block.content.as_str())
                .collect::<Vec<_>>(),
            (0..8)
                .rev()
                .map(|ordinal| format!("orchard detail {ordinal}"))
                .collect::<Vec<_>>()
        );
        assert!(
            counted.queries.get() <= 3,
            "eight ranked raw hits used {} DB roundtrips",
            counted.queries.get()
        );
        println!(
            "expand_query selected raw hydration: {} DB roundtrips for 8 hits",
            counted.queries.get()
        );
    }

    #[tokio::test]
    async fn batched_raw_hydration_matches_ordered_per_row_semantics() {
        let (temp, conn) = query_test_store().await;
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('claude', 'session-a', '/p', '/p')",
            (),
        )
        .await
        .expect("second provider session");
        for (provider, message_id, ordinal, text) in [
            ("cursor", "shared", 1, "cursor shared"),
            ("claude", "shared", 2, "claude shared"),
            ("cursor", "cursor-only", 3, "cursor only"),
        ] {
            insert_query_test_raw(&temp, &conn, provider, message_id, ordinal, text).await;
        }
        let identities = vec![
            ("claude".to_string(), "shared".to_string()),
            ("cursor".to_string(), "cursor-only".to_string()),
            ("cursor".to_string(), "shared".to_string()),
        ];
        let mut per_row = Vec::new();
        for (provider, message_id) in &identities {
            let identity =
                raw::load_raw_message_by_identity(&conn, provider, "session-a", message_id)
                    .await
                    .expect("identity lookup")
                    .expect("raw fixture");
            per_row.push(
                raw::load_raw_message_by_store_id(&conn, identity.store_id)
                    .await
                    .expect("legacy per-row hydration"),
            );
        }

        let batched = raw::load_raw_messages_by_identity(&conn, &identities)
            .await
            .expect("batch hydration");
        assert_eq!(
            serde_json::to_vec(&batched).expect("batch encoding"),
            serde_json::to_vec(&per_row).expect("per-row encoding"),
            "mixed-provider output must remain byte-for-byte ordered"
        );

        let per_row_missing = raw::load_raw_message_by_store_id(&conn, i64::MAX)
            .await
            .expect_err("missing legacy row");
        let batched_missing = raw::load_raw_messages_by_identity(
            &conn,
            &[("cursor".to_string(), "missing".to_string())],
        )
        .await
        .expect_err("missing batch row");
        assert_eq!(
            batched_missing.to_string(),
            per_row_missing.to_string(),
            "batching must retain the typed missing-row failure"
        );
    }

    #[tokio::test]
    async fn expand_query_batches_explicit_summary_hydration_roundtrips() {
        let (_temp, conn) = query_test_store().await;
        let mut node_ids = Vec::new();
        for ordinal in 0..8_i64 {
            let node_id = format!("node-{ordinal}");
            let summary_text = format!("summary {ordinal}");
            let summary_hash = crate::retrieval_content::projected_content_hash(&summary_text);
            conn.execute(
                "INSERT INTO lcm_summary_nodes (
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count
                 ) VALUES (?1, 'cursor', 'conversation-a', 'session-a', 0, ?2, ?3, 1, 1)",
                params![
                    node_id.as_str(),
                    summary_text.as_str(),
                    summary_hash.as_str()
                ],
            )
            .await
            .expect("summary node");
            node_ids.push(node_id);
        }

        let counted = CountingQuery::new(&conn);
        let response = expand_query(
            &counted,
            LcmExpandQueryRequest {
                provider: "cursor".to_string(),
                session_id: "session-a".to_string(),
                prompt: "summarize".to_string(),
                query: None,
                node_ids,
                max_results: 8,
                max_tokens: 100,
                context_max_tokens: 10_000,
            },
        )
        .await
        .expect("expand query");

        assert_eq!(response.node_ids.len(), 8);
        assert!(
            counted.queries.get() <= 3,
            "explicit summary hydration used {} DB roundtrips",
            counted.queries.get()
        );
    }
}
