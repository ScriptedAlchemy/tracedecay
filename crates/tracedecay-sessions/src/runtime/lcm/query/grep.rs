use crate::compatibility::{
    RelatedMessageCopyIdentity, dedupe_related_message_copies, is_inventory_text,
};
use crate::runtime::SessionMessageType;

use super::*;

pub(super) fn contains_cjk(value: &str) -> bool {
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

pub async fn grep(
    conn: &(impl QueryExecutor + ?Sized),
    request: LcmGrepRequest,
    retrieval_filters: LcmGrepFilters,
) -> Result<LcmGrepOutcome, LcmError> {
    let query_plan = grep_query_plan(&request.query);
    if query_plan.is_empty() {
        return Ok(LcmGrepOutcome::default());
    }
    let limit = clamp_limit(request.limit);
    let session_filter = scoped_session_filter(request.scope, request.session_id.as_deref());
    if matches!(request.scope, LcmScope::Current | LcmScope::Session) && session_filter.is_none() {
        return Ok(LcmGrepOutcome::default());
    }
    // A git-scoped grep against a store predating the git-correlation schema
    // can never match; short-circuit rather than issue a `no such table`
    // EXISTS subquery.
    if !request.git_filter.is_empty() && !lcm_table_exists(conn, "session_git_spans").await? {
        return Ok(LcmGrepOutcome::default());
    }

    let raw_only_filters = request.role.is_some()
        || request.start_time.is_some()
        || request.end_time.is_some()
        || !matches!(
            retrieval_filters.message_type,
            crate::runtime::SessionMessageType::All
        );
    // Over-fetch so the deterministic re-rank below can promote substantive
    // hits above inventory/listing noise and still fill the caller's `limit`.
    let fetch_limit = rerank_fetch_limit(limit);
    let mut hits = raw_grep_hits(
        conn,
        &request,
        &retrieval_filters,
        session_filter,
        &query_plan,
        fetch_limit,
    )
    .await?;
    if request.include_summaries && !raw_only_filters && hits.len() < fetch_limit {
        let remaining = fetch_limit - hits.len();
        hits.extend(
            summary_grep_hits(
                conn,
                &request,
                &retrieval_filters,
                session_filter,
                &query_plan,
                remaining,
            )
            .await?,
        );
    }
    sort_hits(&mut hits, request.sort);
    let capped_sessions = rerank_grep_hits(&mut hits, request.sort, request.scope);
    hits.truncate(limit);
    Ok(LcmGrepOutcome {
        hits,
        capped_sessions,
    })
}

/// Deterministic post-fetch re-rank applied to every grep page:
///
/// 1. **Inventory downrank** — for the relevance-shaped sorts (the default
///    `relevance` and `hybrid`), messages that are themselves transcript
///    inventory/listing tool calls (or are dominated by file-path lists) are
///    stably moved below substantive hits. `recency` is left untouched so the
///    explicit "most recent first" contract still holds.
/// 2. **Per-session cap** — in cross-session (`scope: all`) pages no single
///    session may contribute more than [`PER_SESSION_HIT_CAP`] hits, so one
///    noisy session cannot flood the page. Single-session scopes are exempt.
///
/// Both stages are stable, preserving the relative order established by the
/// requested `sort` within each retained group.
fn rerank_grep_hits(
    hits: &mut Vec<LcmGrepHit>,
    sort: LcmGrepSort,
    scope: LcmScope,
) -> BTreeMap<String, usize> {
    if !matches!(sort, LcmGrepSort::Recency) {
        let mut substantive = Vec::with_capacity(hits.len());
        let mut inventory = Vec::new();
        for hit in hits.drain(..) {
            if hit_is_inventory(&hit) {
                inventory.push(hit);
            } else {
                substantive.push(hit);
            }
        }
        substantive.append(&mut inventory);
        *hits = substantive;
    }

    let mut capped: BTreeMap<String, usize> = BTreeMap::new();
    if matches!(scope, LcmScope::All) {
        // Per-session running aggregate, folded into one map so each hit touches
        // a single `entry`: `count` gates the cap, `has_tool` records whether a
        // tool-role hit was already kept, and `last_idx` is the session's
        // weakest kept slot (swapped for a reserved tool hit below).
        #[derive(Default)]
        struct SessionAgg {
            count: usize,
            last_idx: usize,
            has_tool: bool,
        }

        let mut kept: Vec<LcmGrepHit> = Vec::with_capacity(hits.len());
        let mut agg: BTreeMap<String, SessionAgg> = BTreeMap::new();
        let mut dropped_tool: BTreeMap<String, LcmGrepHit> = BTreeMap::new();
        for hit in hits.drain(..) {
            let is_tool = hit.role.as_deref() == Some("tool");
            let session = agg.entry(hit.session_id.clone()).or_default();
            if session.count >= PER_SESSION_HIT_CAP {
                *capped.entry(hit.session_id.clone()).or_insert(0) += 1;
                // Remember the best capped tool-role hit: narration routinely
                // outranks exact action rows (tool calls, file edits), and a
                // session capped to narration only cannot answer "what did it
                // actually do". One slot is reserved for it below.
                if is_tool && !session.has_tool {
                    dropped_tool.entry(hit.session_id.clone()).or_insert(hit);
                }
            } else {
                session.count += 1;
                if is_tool {
                    session.has_tool = true;
                }
                session.last_idx = kept.len();
                kept.push(hit);
            }
        }
        for (session_id, tool_hit) in dropped_tool {
            let Some(session) = agg.get(&session_id) else {
                continue;
            };
            if session.has_tool {
                continue;
            }
            // Swap the session's weakest kept hit for its top tool hit;
            // one hit still drops, so the capped count stays accurate.
            kept[session.last_idx] = tool_hit;
        }
        *hits = kept;
    }
    capped
}

/// Cheap, deterministic heuristic: is this hit a transcript inventory/listing
/// tool call, an otherwise path-list-dominated message, or a prose branch/
/// worktree roster rather than substantive conversation? Delegates to the
/// shared application classifier so the lcm/grep and global message-search
/// re-ranks agree. Summary nodes are curated prose, never raw inventory, so
/// they are exempt.
fn hit_is_inventory(hit: &LcmGrepHit) -> bool {
    if hit.kind != "raw_message" {
        return false;
    }
    is_inventory_text(&hit.snippet)
}

pub(super) async fn raw_grep_hits(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmGrepRequest,
    retrieval_filters: &LcmGrepFilters,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.requires_like_fallback {
        return raw_like_grep_hits(
            conn,
            request,
            retrieval_filters,
            session_id,
            query_plan,
            limit,
        )
        .await;
    }
    let mut values = vec![Value::Text(query_plan.fts_query.clone())];
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "r.provider", &mut filters, &mut values);
    push_raw_grep_filters(
        request,
        *retrieval_filters,
        session_id,
        &mut filters,
        &mut values,
    );
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
        "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text, r.role,
                COALESCE(NULLIF(s.parent_session_id, ''), r.session_id),
                COALESCE(s.is_subagent, 0)
         FROM lcm_raw_messages_fts
         JOIN lcm_raw_messages r ON r.store_id = lcm_raw_messages_fts.rowid
         LEFT JOIN sessions s ON s.provider = r.provider AND s.session_id = r.session_id
         WHERE lcm_raw_messages_fts MATCH ?
           {filter_sql}
         ORDER BY {order_by}
         LIMIT ?"
    );
    let mut rows = conn.query(&sql, values).await?;

    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(raw_hit_candidate_from_row(&row, &query_plan.like_terms)?);
    }
    Ok(dedupe_related_raw_hits(candidates))
}

pub(super) async fn summary_grep_hits(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmGrepRequest,
    retrieval_filters: &LcmGrepFilters,
    session_id: Option<&str>,
    query_plan: &GrepQueryPlan,
    limit: usize,
) -> Result<Vec<LcmGrepHit>, LcmError> {
    if query_plan.requires_like_fallback {
        return summary_like_grep_hits(
            conn,
            request,
            retrieval_filters,
            session_id,
            query_plan,
            limit,
        )
        .await;
    }
    let mut values = vec![Value::Text(query_plan.fts_query.clone())];
    let mut filters = Vec::new();
    push_grep_provider_filter(request, "n.provider", &mut filters, &mut values);
    push_summary_grep_filters(
        request,
        *retrieval_filters,
        session_id,
        &mut filters,
        &mut values,
    );
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
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmGrepRequest,
    retrieval_filters: &LcmGrepFilters,
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
    push_raw_grep_filters(
        request,
        *retrieval_filters,
        session_id,
        &mut filters,
        &mut values,
    );

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
        "SELECT r.provider, r.session_id, r.message_id, r.store_id, r.snippet_text, r.role,
                COALESCE(NULLIF(s.parent_session_id, ''), r.session_id),
                COALESCE(s.is_subagent, 0), 0.0 AS rank
         FROM lcm_raw_messages r
         LEFT JOIN sessions s ON s.provider = r.provider AND s.session_id = r.session_id
         WHERE {}
         ORDER BY {order_by}
         LIMIT ?",
        filters.join(" AND "),
    );
    let mut rows = conn.query(&sql, values).await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(raw_hit_candidate_from_row(&row, &query_plan.like_terms)?);
    }
    let mut hits = dedupe_related_raw_hits(candidates);
    if hits.len() > limit {
        hits.truncate(limit);
    }
    Ok(hits)
}

async fn summary_like_grep_hits(
    conn: &(impl QueryExecutor + ?Sized),
    request: &LcmGrepRequest,
    retrieval_filters: &LcmGrepFilters,
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
    push_summary_grep_filters(
        request,
        *retrieval_filters,
        session_id,
        &mut filters,
        &mut values,
    );

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
    retrieval_filters: LcmGrepFilters,
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
    if let Some(predicate) = message_type_predicate_sql("r", false, retrieval_filters.message_type)
    {
        filters.push(predicate);
    }
    push_grep_relationship_scope_filter(
        retrieval_filters.relationship_scope,
        "r.provider",
        "r.session_id",
        filters,
    );
    push_grep_git_scope_filter(request, "r.session_id", filters, values);
}

fn push_summary_grep_filters(
    request: &LcmGrepRequest,
    retrieval_filters: LcmGrepFilters,
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
    push_grep_relationship_scope_filter(
        retrieval_filters.relationship_scope,
        "n.provider",
        "n.session_id",
        filters,
    );
    push_grep_git_scope_filter(request, "n.session_id", filters, values);
}

fn message_type_predicate_sql(
    alias: &str,
    has_kind_column: bool,
    message_type: SessionMessageType,
) -> Option<String> {
    if matches!(message_type, SessionMessageType::All) {
        return None;
    }
    let kind_clause = if has_kind_column {
        format!(" OR lower(COALESCE({alias}.kind, '')) IN ('tool_result', 'tool_output')")
    } else {
        String::new()
    };
    let tool_result = format!(
        "({alias}.role = 'tool'{kind_clause} \
         OR CASE WHEN json_valid(COALESCE({alias}.metadata_json, '')) \
              THEN EXISTS (SELECT 1 FROM json_each({alias}.metadata_json, '$.tool_events') event \
                           WHERE json_extract(event.value, '$.type') = 'tool_result') \
              ELSE 0 END)"
    );
    Some(match message_type {
        SessionMessageType::All => unreachable!(),
        SessionMessageType::DirectUser => {
            format!("({alias}.role = 'user' AND NOT {tool_result})")
        }
        SessionMessageType::ToolResult => tool_result,
    })
}

fn push_grep_relationship_scope_filter(
    scope: crate::runtime::SessionSearchScope,
    provider_column: &str,
    session_column: &str,
    filters: &mut Vec<String>,
) {
    let is_subagent = match scope {
        crate::runtime::SessionSearchScope::All => return,
        crate::runtime::SessionSearchScope::ParentsOnly => 0,
        crate::runtime::SessionSearchScope::SubagentsOnly => 1,
    };
    filters.push(format!(
        "EXISTS (SELECT 1 FROM sessions scoped_session \
         WHERE scoped_session.provider = {provider_column} \
           AND scoped_session.session_id = {session_column} \
           AND scoped_session.is_subagent = {is_subagent})"
    ));
}

/// Appends the request's git-scope constraint (branch/worktree/commit) as an
/// EXISTS predicate correlated to the outer row via `session_column`. No-op
/// when the filter is empty.
fn push_grep_git_scope_filter(
    request: &LcmGrepRequest,
    session_column: &str,
    filters: &mut Vec<String>,
    values: &mut Vec<Value>,
) {
    if let Some((predicate, predicate_values)) =
        crate::runtime::git_correlation::git_scope_exists_predicate(
            &request.git_filter,
            session_column,
        )
    {
        filters.push(predicate);
        values.extend(predicate_values);
    }
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

struct RawGrepCandidate {
    hit: LcmGrepHit,
    family_session_id: String,
    is_subagent: bool,
    content: String,
}

fn raw_hit_candidate_from_row(
    row: &tracedecay_runtime_core::db::engine::Row,
    like_terms: &[String],
) -> Result<RawGrepCandidate, LcmError> {
    let snippet: String = row.get(4)?;
    let role: Option<String> = row.get::<Option<String>>(5).unwrap_or(None);
    Ok(RawGrepCandidate {
        hit: LcmGrepHit {
            kind: "raw_message".to_string(),
            provider: row.get(0)?,
            session_id: row.get(1)?,
            message_id: Some(row.get(2)?),
            node_id: None,
            store_id: Some(row.get(3)?),
            role: role.filter(|r| !r.is_empty()),
            snippet: match_centered_snippet(&snippet, like_terms),
        },
        family_session_id: row.get(6)?,
        is_subagent: row.get::<i64>(7).unwrap_or_default() != 0,
        content: snippet,
    })
}

fn dedupe_related_raw_hits(candidates: Vec<RawGrepCandidate>) -> Vec<LcmGrepHit> {
    dedupe_related_message_copies(candidates, |candidate| RelatedMessageCopyIdentity {
        provider: &candidate.hit.provider,
        family_session_id: &candidate.family_session_id,
        session_id: &candidate.hit.session_id,
        is_subagent: candidate.is_subagent,
        content: &candidate.content,
    })
    .into_iter()
    .map(|candidate| candidate.hit)
    .collect()
}

fn summary_hit_from_row(
    row: &tracedecay_runtime_core::db::engine::Row,
    like_terms: &[String],
) -> Result<LcmGrepHit, LcmError> {
    let summary_text: String = row.get(3)?;
    Ok(LcmGrepHit {
        kind: "summary_node".to_string(),
        provider: row.get(0)?,
        session_id: row.get(1)?,
        message_id: None,
        node_id: Some(row.get(2)?),
        store_id: None,
        role: None,
        snippet: match_centered_snippet(&summary_text, like_terms),
    })
}
