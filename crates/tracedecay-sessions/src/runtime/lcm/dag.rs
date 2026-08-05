use std::future::Future;

use crate::compatibility::projected_content_hash;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::types::{LcmImmutableSummaryPublication, LcmSummaryPublicationReceipt};
use super::{LcmError, LcmSourceRef, LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft};

pub trait LcmSummaryPublicationPort {
    fn publish_immutable_summary(
        &self,
        publication: LcmImmutableSummaryPublication,
    ) -> impl Future<Output = Result<LcmSummaryPublicationReceipt, LcmError>>;
}

pub async fn insert_summary_node(
    publisher: &impl LcmSummaryPublicationPort,
    draft: LcmSummaryNodeDraft,
) -> Result<LcmSummaryNode, LcmError> {
    let summary_hash = projected_content_hash(&draft.summary_text);
    let node_id = summary_node_id(
        &draft.provider,
        &draft.session_id,
        draft.depth,
        &draft.source_refs,
        &summary_hash,
    );

    publisher
        .publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: node_id,
            predecessor_summary_id: None,
            draft,
        })
        .await
        .map(|receipt| receipt.summary)
}

pub async fn expand_summary_node(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmSummaryExpansion, LcmError> {
    expand_summary_node_with_content(conn, provider, session_id, node_id, true).await
}

async fn expand_summary_node_with_content(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
    include_content: bool,
) -> Result<LcmSummaryExpansion, LcmError> {
    let _summary =
        load_summary_node_with_content(conn, provider, session_id, node_id, include_content)
            .await?;
    Err(LcmError::SummarySourceUnavailable {
        source_id: node_id.to_string(),
        reason: "summary topology is owned by the native session relation graph".to_string(),
    })
}

/// One uncondensed summary node plus the earliest raw-message store id in its
/// descendant lineage, used to position the node inside interleaved replay.
#[derive(Debug, Clone)]
pub struct LcmUncondensedSummaryNode {
    pub node: LcmSummaryNode,
    pub first_source_store_id: Option<i64>,
}

/// Loads every summary node for the session that has not been condensed into
/// a higher-depth node. Mirrors hermes-lcm `SummaryDAG.get_uncondensed_at_depth`
/// collapsed across all depths in one query; replay assembly consumes the
/// result ordered by lineage position (then depth, highest first).
pub async fn load_uncondensed_summary_nodes(
    _conn: &(impl QueryExecutor + ?Sized),
    _provider: &str,
    _session_id: &str,
) -> Result<Vec<LcmUncondensedSummaryNode>, LcmError> {
    Ok(Vec::new())
}

pub fn summary_node_id(
    provider: &str,
    session_id: &str,
    depth: i64,
    source_refs: &[LcmSourceRef],
    summary_hash: &str,
) -> String {
    let input = serde_json::json!({
        "provider": provider,
        "session_id": session_id,
        "depth": depth,
        "source_refs": source_refs,
        "summary_hash": summary_hash,
    });
    format!("sum_{}", projected_content_hash(&input.to_string()))
}

async fn load_summary_node_with_content(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
    include_content: bool,
) -> Result<LcmSummaryNode, LcmError> {
    let node = load_summary_node_by_id(conn, node_id, include_content).await?;
    if node.provider == provider && node.session_id == session_id {
        Ok(node)
    } else {
        Err(LcmError::SummaryNodeNotFound)
    }
}

async fn load_summary_node_by_id(
    conn: &(impl QueryExecutor + ?Sized),
    node_id: &str,
    include_content: bool,
) -> Result<LcmSummaryNode, LcmError> {
    let summary_text = if include_content {
        "summary_text"
    } else {
        "'' AS summary_text"
    };
    let sql = format!(
        "SELECT node_id, provider, conversation_id, session_id, depth, {summary_text},
                summary_hash, summary_token_count, source_token_count, source_time_start,
                source_time_end, expand_hint, metadata_json, created_at
         FROM lcm_summary_nodes
         WHERE node_id = ?1"
    );
    let mut rows = conn.query(&sql, params![node_id]).await?;
    let row = rows.next().await?.ok_or(LcmError::SummaryNodeNotFound)?;
    Ok(LcmSummaryNode {
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
    })
}
