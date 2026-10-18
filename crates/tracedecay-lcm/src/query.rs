use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_runtime_core::db::engine::{QueryExecutor, Value, params};
use tracedecay_runtime_core::tracedecay::current_timestamp;

mod describe;
mod expand;
mod payload_health;
mod scope;
mod session;
mod status;

use describe::*;
pub use expand::expand;
pub use payload_health::*;
pub use session::*;
pub use status::store_status;
use status::*;

use super::types::LcmStoreTokenCoverage;
use super::types::{
    LcmLifecycleStatus, LcmPayloadCoverage, LcmPayloadCoverageState, LcmPayloadGcStatus,
    LcmPayloadStatus, LcmRedactionStatus, LcmSessionRawStats,
};
use super::{
    LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS, LCM_DEFAULT_FRESH_TAIL_COUNT,
    LCM_DEFAULT_SUMMARY_FAN_IN, LcmConfigStatus, LcmContentRange, LcmContentSlice,
    LcmDagDepthStatus, LcmDagStatus, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmGcConfig, LcmLoadSessionMessage, LcmLoadSessionPage,
    LcmLoadSessionRequest, LcmRawMessage, LcmRawMessageOverview, LcmRecentSession,
    LcmReplayMessage, LcmReplaySummaryNode, LcmSessionReplayRequest, LcmSessionReplaySlice,
    LcmSourceRef, LcmStatus, LcmStorageKind, LcmStoreStatus, LcmSummaryConvergenceStatus,
    LcmSummaryNode, LcmSummaryNodeOverview, dag, gc, maintenance, payload, raw, schema, util,
};

const MAX_PAGE_LIMIT: usize = 100;

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
    let unpublished_message_count =
        count_unpublished_raw_messages(conn, provider, session_id).await?;
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
        unpublished_message_count,
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
    let schema_version = schema::schema_version(conn).await?;
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

async fn count_unpublished_raw_messages(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        &format!(
            "SELECT COUNT(*) FROM lcm_raw_messages raw
             WHERE raw.provider = ?1 AND raw.session_id = ?2 AND {}",
            schema::RAW_MESSAGE_UNPUBLISHED_SQL
        ),
        params![provider, session_id],
        "unpublished raw message count query returned no rows",
    )
    .await
}

async fn count_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        &format!(
            "SELECT COUNT(*) FROM session_summary_nodes n
             WHERE n.provider = ?1 AND (?2 IS NULL OR n.session_id = ?2) AND {}",
            schema::SUMMARY_VISIBLE_SQL
        ),
        params![provider, util::opt_text(session_id)],
        "summary count query returned no rows",
    )
    .await
}

async fn count_external_payloads(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::count_by_provider_session(conn, "lcm_external_payloads", provider, session_id).await
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
