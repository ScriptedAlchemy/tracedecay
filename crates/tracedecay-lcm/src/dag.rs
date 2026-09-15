use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

#[cfg(test)]
use serde_json::Value as JsonValue;
use serde_json::{Map, json};
use tracedecay_domain::HydrationStateV1;

use crate::retrieval_content::projected_content_hash;
use tracedecay_privacy::{
    bind_sanitized_lcm_payload_text, sanitize_lcm_payload_text, sanitize_provider_metadata_json,
};
use tracedecay_runtime_core::db::engine::{QueryExecutor, Value, params};

use super::types::{LcmImmutableSummaryPublication, LcmSummaryPublicationReceipt};
use super::{
    LcmError, LcmExpandedSummarySource, LcmRawMessage, LcmRawMessageMetadata, LcmSourceRef,
    LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft, raw, util,
};

#[derive(Clone)]
enum RawMessageRow {
    Hydrated(LcmRawMessage),
    Metadata(LcmRawMessageMetadata),
}

impl RawMessageRow {
    fn provider(&self) -> &str {
        match self {
            Self::Hydrated(raw) => &raw.provider,
            Self::Metadata(raw) => &raw.provider,
        }
    }

    fn session_id(&self) -> &str {
        match self {
            Self::Hydrated(raw) => &raw.session_id,
            Self::Metadata(raw) => &raw.session_id,
        }
    }
}

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
    let draft = sanitize_summary_draft(draft)?;
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

pub fn sanitize_summary_draft(
    mut draft: LcmSummaryNodeDraft,
) -> Result<LcmSummaryNodeDraft, LcmError> {
    const MAX_SUMMARY_METADATA_BYTES: u64 = 1_048_576;

    let summary = sanitize_lcm_payload_text(&draft.summary_text)
        .map_err(|error| LcmError::Db(format!("summary privacy sanitization failed: {error}")))?;
    draft.summary_text = summary.sanitized_text().to_owned();

    let hint = draft
        .expand_hint
        .as_deref()
        .map(sanitize_lcm_payload_text)
        .transpose()
        .map_err(|error| LcmError::Db(format!("summary hint sanitization failed: {error}")))?;
    draft.expand_hint = hint
        .as_ref()
        .map(|sanitization| sanitization.sanitized_text().to_owned());

    let raw_metadata = draft.metadata_json.as_deref().unwrap_or("{}");
    let mut metadata = sanitize_provider_metadata_json(raw_metadata, MAX_SUMMARY_METADATA_BYTES)
        .ok_or_else(|| LcmError::Db("summary metadata sanitization failed".to_owned()))?;
    if !metadata.is_object() {
        return Err(LcmError::Db(
            "summary metadata sanitization failed: metadata must be a JSON object".to_owned(),
        ));
    }
    let sanitized_metadata = serde_json::to_string(&metadata)
        .map_err(|error| LcmError::Db(format!("summary metadata encoding failed: {error}")))?;
    let metadata_receipt = bind_sanitized_lcm_payload_text(raw_metadata, &sanitized_metadata)
        .map_err(|error| LcmError::Db(format!("summary metadata receipt failed: {error}")))?;
    let mut receipts = Map::new();
    receipts.insert(
        "summary_text".to_owned(),
        serde_json::to_value(summary.receipt())
            .map_err(|error| LcmError::Db(format!("summary receipt encoding failed: {error}")))?,
    );
    if let Some(hint) = hint {
        receipts.insert(
            "expand_hint".to_owned(),
            serde_json::to_value(hint.receipt()).map_err(|error| {
                LcmError::Db(format!("summary hint receipt encoding failed: {error}"))
            })?,
        );
    }
    receipts.insert(
        "metadata".to_owned(),
        serde_json::to_value(metadata_receipt.receipt()).map_err(|error| {
            LcmError::Db(format!("summary metadata receipt encoding failed: {error}"))
        })?,
    );
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "tracedecay_privacy".to_owned(),
            json!({"sanitization_receipts": receipts}),
        );
    }
    draft.metadata_json = Some(metadata.to_string());
    Ok(draft)
}

pub async fn expand_summary_node(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<LcmSummaryExpansion, LcmError> {
    let mut expansions =
        expand_summary_nodes_with_content(conn, provider, session_id, &[node_id.to_string()], true)
            .await?;
    expansions.pop().ok_or(LcmError::SummaryNodeNotFound)
}

/// Expands every requested node against **one** hydration pass.
///
/// The node rows, their lineage rows, the raw sources of the whole set, and the
/// child summary nodes of the whole set are each loaded once, so a page of `N`
/// explicitly requested nodes costs a fixed number of round trips instead of
/// `N` independent expansions. Per-node semantics are unchanged: nodes are
/// assembled in request order and the first ownership or integrity failure
/// still aborts the whole call with the same error it raised before.
pub async fn expand_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_ids: &[String],
) -> Result<Vec<LcmSummaryExpansion>, LcmError> {
    expand_summary_nodes_with_content(conn, provider, session_id, node_ids, true).await
}

// The one hydration pass behind every summary expansion (node rows, lineage,
// raw + child source closure). Measured here rather than on the two public
// wrappers so single-node and batched expansions share one label.
#[hotpath::measure(label = "sessions.lcm.dag.hydrate", future = true)]
async fn expand_summary_nodes_with_content(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    node_ids: &[String],
    include_content: bool,
) -> Result<Vec<LcmSummaryExpansion>, LcmError> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let requested = hotpath::future!(
        load_summary_nodes_by_ids(conn, node_ids, include_content),
        label = "sessions.lcm.expand.summary.fetch"
    )
    .await?;

    // Resolve every requested node up front so the source closure below is the
    // union of the whole page, then hydrate that union once.
    let mut summaries = Vec::with_capacity(node_ids.len());
    let mut raw_store_ids = Vec::new();
    let mut child_node_ids = Vec::new();
    for node_id in node_ids {
        let summary = requested
            .get(node_id)
            .cloned()
            .ok_or(LcmError::SummaryNodeNotFound)?;
        if summary.provider != provider || summary.session_id != session_id {
            return Err(LcmError::SummaryNodeNotFound);
        }
        for source_ref in &summary.source_refs {
            match source_ref {
                LcmSourceRef::RawMessage { store_id } => raw_store_ids.push(*store_id),
                LcmSourceRef::SummaryNode { node_id } => child_node_ids.push(node_id.clone()),
            }
        }
        summaries.push(summary);
    }

    let raw_sources = hotpath::future!(
        load_raw_messages_by_store_ids(conn, &raw_store_ids, include_content),
        label = "sessions.lcm.expand.summary.hydrate"
    )
    .await?;
    let child_sources = hotpath::future!(
        load_summary_nodes_by_ids(conn, &child_node_ids, include_content),
        label = "sessions.lcm.expand.summary.fetch"
    )
    .await?;

    hotpath::measure_block!("sessions.lcm.expand.summary.assemble", {
        let mut expansions = Vec::with_capacity(summaries.len());
        for summary in summaries {
            expansions.push(assemble_summary_expansion(
                summary,
                provider,
                session_id,
                include_content,
                &raw_sources,
                &child_sources,
            )?);
        }
        Ok::<_, LcmError>(expansions)
    })
}

/// Assembles one expansion from an already-hydrated source closure. Pure: it
/// issues no queries, so the round-trip cost of a page lives entirely in
/// [`expand_summary_nodes_with_content`].
fn assemble_summary_expansion(
    summary: LcmSummaryNode,
    provider: &str,
    session_id: &str,
    include_content: bool,
    raw_sources: &BTreeMap<i64, RawMessageRow>,
    child_sources: &BTreeMap<String, LcmSummaryNode>,
) -> Result<LcmSummaryExpansion, LcmError> {
    let mut sources = Vec::with_capacity(summary.source_refs.len());

    for source_ref in &summary.source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                // An *absent* raw row is not an ownership violation: publication
                // (`session_temporal::operations::sources::prepare_raw_source`)
                // proves every raw source exists and is session-owned before the
                // lineage row is written, so a row that is missing at read time
                // was removed afterwards — by the projection-durability retention
                // drop pass (plan 38 §3), whose whole premise is that the summary
                // is the durable survivor. Report the source as retention-expired
                // (plan 23 hydration state) and keep expanding; aborting the whole
                // expansion would make every summary older than the drop window
                // unreadable, and would do so under a misleading ownership error.
                let Some(raw) = raw_sources.get(store_id).cloned() else {
                    sources.push(LcmExpandedSummarySource {
                        source_ref: source_ref.clone(),
                        state: HydrationStateV1::RetentionExpired,
                        content: String::new(),
                        content_range: None,
                        content_truncated: false,
                        raw_message: None,
                        raw_message_metadata: None,
                        summary_node: None,
                    });
                    continue;
                };
                // A row that is present but foreign is still a hard ownership
                // violation and must never be disclosed.
                if raw.provider() != provider || raw.session_id() != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                let (content, raw_message, raw_message_metadata) = match raw {
                    RawMessageRow::Hydrated(raw) => (raw.content.clone(), Some(raw), None),
                    RawMessageRow::Metadata(raw) => (String::new(), None, Some(raw)),
                };
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    state: if include_content {
                        HydrationStateV1::Available
                    } else {
                        HydrationStateV1::RetainedButUnavailable
                    },
                    content,
                    content_range: None,
                    content_truncated: false,
                    raw_message,
                    raw_message_metadata,
                    summary_node: None,
                });
            }
            LcmSourceRef::SummaryNode {
                node_id: child_node_id,
            } => {
                let child = child_sources
                    .get(child_node_id)
                    .cloned()
                    .ok_or(LcmError::SummaryNodeNotFound)?;
                if child.provider != provider || child.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                sources.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    state: if include_content {
                        HydrationStateV1::Available
                    } else {
                        HydrationStateV1::RetainedButUnavailable
                    },
                    content: child.summary_text.clone(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: None,
                    raw_message_metadata: None,
                    summary_node: Some(Box::new(child)),
                });
            }
        }
    }

    Ok(LcmSummaryExpansion { summary, sources })
}

/// One uncondensed summary node plus the earliest raw-message store id in its
/// descendant lineage, used to position the node inside interleaved replay.
#[derive(Debug, Clone)]
pub struct LcmUncondensedSummaryNode {
    pub node: LcmSummaryNode,
    pub first_source_store_id: Option<i64>,
}

/// CTE prefix shared by [`load_uncondensed_summary_nodes`] and its work
/// measurement: the available unparented roots of one session and, per root,
/// the descendants reachable through `lcm_summary_sources`.
///
/// `lineage` holds one `(root_id, source_kind, source_id)` row per *distinct*
/// descendant. The recursive `UNION` (not `UNION ALL`) makes SQLite's queue a
/// visited set, so a descendant shared by many routes is expanded once per
/// root and the walk costs O(reachable nodes + edges), never O(paths). The
/// replay consumer only needs `MIN(raw store id)` per root, which the distinct
/// set answers exactly. The visited set is also the termination bound: a
/// corrupted cyclic lineage revisits nothing and still yields the complete
/// reachable minimum instead of a depth-truncated one.
const UNCONDENSED_LINEAGE_CTE: &str = "WITH RECURSIVE unparented AS (
       SELECT n.node_id, n.provider, n.conversation_id, n.session_id, n.depth,
              n.summary_text, n.summary_hash, n.summary_token_count,
              n.source_token_count, n.source_time_start, n.source_time_end,
              n.expand_hint, n.metadata_json, n.created_at
       FROM lcm_summary_nodes n
       JOIN session_temporal_generations generation
         ON generation.session_id = n.session_id
        AND generation.state = 'active'
       JOIN session_summary_availability availability
         ON availability.session_id = generation.session_id
        AND availability.generation = generation.generation
        AND availability.summary_id = n.node_id
        AND availability.availability = 'available'
       WHERE n.provider = ?1 AND n.session_id = ?2
         -- Fail closed only while a raw revision's invalidation
         -- closure is partially applied: the walk enqueues
         -- `summary_node` work exactly when it has already staled at
         -- least one summary and has more to visit. Queued-but-
         -- unstarted work (only `raw_message` rows, which every
         -- protection revision seeds) leaves a consistent view.
         AND NOT EXISTS (
           SELECT 1
           FROM lcm_summary_convergence_invalidation_work partial
           WHERE partial.provider = n.provider
             AND partial.session_id = n.session_id
             AND partial.source_kind = 'summary_node'
             AND partial.state = 'pending'
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
     lineage(root_id, source_kind, source_id) AS (
       SELECT s.node_id, s.source_kind, s.source_id
       FROM lcm_summary_sources s
       JOIN unparented u ON u.node_id = s.node_id
       UNION
       SELECT l.root_id, s.source_kind, s.source_id
       FROM lineage l
       JOIN lcm_summary_sources s
         ON l.source_kind = 'summary_node' AND s.node_id = l.source_id
     ),
     first_raw AS (
       SELECT root_id, MIN(CAST(source_id AS INTEGER)) AS first_source_store_id
       FROM lineage
       WHERE source_kind = 'raw_message'
       GROUP BY root_id
     )";

/// The replay query: every unparented root with its earliest reachable raw
/// store id, in replay order. Parameters: `?1` provider, `?2` session id.
fn uncondensed_summary_nodes_sql() -> String {
    format!(
        "{UNCONDENSED_LINEAGE_CTE}
         SELECT u.node_id, u.provider, u.conversation_id, u.session_id, u.depth,
                u.summary_text, u.summary_hash, u.summary_token_count,
                u.source_token_count, u.source_time_start, u.source_time_end,
                u.expand_hint, u.metadata_json, u.created_at,
                first_raw.first_source_store_id
         FROM unparented u
         LEFT JOIN first_raw ON first_raw.root_id = u.node_id
         ORDER BY first_raw.first_source_store_id IS NULL,
                  first_raw.first_source_store_id,
                  u.depth DESC,
                  u.source_time_start IS NULL, u.source_time_start,
                  u.created_at, u.node_id"
    )
}

/// Loads every summary node for the session that has not been condensed into
/// a higher-depth node. Mirrors hermes-lcm `SummaryDAG.get_uncondensed_at_depth`
/// collapsed across all depths in one query; replay assembly consumes the
/// result ordered by lineage position (then depth, highest first).
// The recursive-CTE summary-DAG build: the dominant candidate when replay
// assembly is slow on deep DAGs, so it gets its own label under the
// assembly span.
#[hotpath::measure(label = "sessions.lcm.dag.load_uncondensed", future = true)]
pub async fn load_uncondensed_summary_nodes(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmUncondensedSummaryNode>, LcmError> {
    let mut rows = conn
        .query(
            &uncondensed_summary_nodes_sql(),
            params![provider, session_id],
        )
        .await?;
    let mut nodes = Vec::new();
    while let Some(row) = rows.next().await? {
        let node = LcmSummaryNode {
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
        };
        verify_summary_content(&node.summary_text, &node.summary_hash)?;
        nodes.push(LcmUncondensedSummaryNode {
            node,
            first_source_store_id: row.get(14)?,
        });
    }
    Ok(nodes)
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

async fn load_raw_messages_by_store_ids(
    conn: &(impl QueryExecutor + ?Sized),
    store_ids: &[i64],
    include_content: bool,
) -> Result<BTreeMap<i64, RawMessageRow>, LcmError> {
    let unique_store_ids = store_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_store_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let select_columns = if include_content {
        raw::RAW_MESSAGE_SELECT_COLUMNS
    } else {
        raw::RAW_MESSAGE_METADATA_SELECT_COLUMNS
    };
    let fetched = hotpath::future!(
        async {
            let mut fetched = Vec::new();
            for chunk in unique_store_ids.chunks(util::SQLITE_IN_BATCH_SIZE) {
                if chunk.is_empty() {
                    continue;
                }
                let sql = format!(
                    "SELECT {select_columns}
                     FROM lcm_raw_messages
                     WHERE store_id IN ({})",
                    util::sql_in_placeholders(chunk.len())
                );
                let mut rows = conn
                    .query(
                        &sql,
                        chunk
                            .iter()
                            .map(|store_id| Value::Integer(*store_id))
                            .collect::<Vec<_>>(),
                    )
                    .await?;
                while let Some(row) = rows.next().await? {
                    fetched.push(row);
                }
            }
            Ok::<_, LcmError>(fetched)
        },
        label = "sessions.lcm.hydrate.fetch"
    )
    .await?;

    if include_content {
        hotpath::measure_block!("sessions.lcm.hydrate.redact", {
            let mut out = BTreeMap::new();
            for row in fetched {
                let raw = raw::verified_raw_message_from_row(&row)?;
                out.insert(raw.store_id, RawMessageRow::Hydrated(raw));
            }
            Ok::<_, LcmError>(out)
        })
    } else {
        let mut out = BTreeMap::new();
        for row in fetched {
            let raw = raw::raw_message_metadata_from_row(&row)?;
            out.insert(raw.store_id, RawMessageRow::Metadata(raw));
        }
        Ok(out)
    }
}

async fn load_summary_nodes_by_ids(
    conn: &(impl QueryExecutor + ?Sized),
    node_ids: &[String],
    include_content: bool,
) -> Result<BTreeMap<String, LcmSummaryNode>, LcmError> {
    let unique_node_ids = node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique_node_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let summary_text = if include_content {
        "summary_text"
    } else {
        "'' AS summary_text"
    };
    let mut nodes = BTreeMap::new();
    for chunk in unique_node_ids.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = util::sql_in_placeholders(chunk.len());
        let node_sql = format!(
            "SELECT node_id, provider, conversation_id, session_id, depth, {summary_text},
                    summary_hash, summary_token_count, source_token_count, source_time_start,
                    source_time_end, expand_hint, metadata_json, created_at
             FROM lcm_summary_nodes
             WHERE node_id IN ({placeholders})"
        );
        let values = chunk
            .iter()
            .map(|node_id| Value::Text(node_id.clone()))
            .collect::<Vec<_>>();
        let mut node_rows = conn.query(&node_sql, values.clone()).await?;
        while let Some(row) = node_rows.next().await? {
            let node_id: String = row.get(0)?;
            let node = LcmSummaryNode {
                node_id: node_id.clone(),
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
            };
            if include_content {
                verify_summary_content(&node.summary_text, &node.summary_hash)?;
            }
            nodes.insert(node_id, node);
        }
        let source_sql = format!(
            "SELECT node_id, source_kind, source_id
             FROM lcm_summary_sources
             WHERE node_id IN ({placeholders})
             ORDER BY node_id, ordinal"
        );
        let mut source_rows = conn.query(&source_sql, values).await?;
        while let Some(row) = source_rows.next().await? {
            let node_id: String = row.get(0)?;
            let source_kind: String = row.get(1)?;
            let source_id: String = row.get(2)?;
            if let Some(node) = nodes.get_mut(&node_id) {
                node.source_refs
                    .push(source_ref_from_db(&source_kind, &source_id)?);
            }
        }
    }
    Ok(nodes)
}

pub(crate) fn verify_summary_content(
    summary_text: &str,
    summary_hash: &str,
) -> Result<(), LcmError> {
    if projected_content_hash(summary_text) != summary_hash {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    Ok(())
}

fn source_ref_from_db(source_kind: &str, source_id: &str) -> Result<LcmSourceRef, LcmError> {
    match source_kind {
        "raw_message" => source_id
            .parse::<i64>()
            .map(|store_id| LcmSourceRef::RawMessage { store_id })
            .map_err(|err| LcmError::Db(err.to_string())),
        "summary_node" => Ok(LcmSourceRef::SummaryNode {
            node_id: source_id.to_string(),
        }),
        _ => Err(LcmError::Db(format!(
            "invalid summary source_kind: {source_kind}"
        ))),
    }
}

#[cfg(test)]
mod lineage_tests {
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};

    use super::*;
    use crate::schema;
    use crate::test_support::{self, sqlite_vm_steps};

    const PROVIDER: &str = "cursor";

    /// One summary node of a lineage fixture.
    struct FixtureNode {
        node_id: String,
        depth: i64,
        created_at: i64,
        available: bool,
        sources: Vec<LcmSourceRef>,
    }

    fn node(
        node_id: &str,
        depth: i64,
        created_at: i64,
        available: bool,
        sources: Vec<LcmSourceRef>,
    ) -> FixtureNode {
        FixtureNode {
            node_id: node_id.to_string(),
            depth,
            created_at,
            available,
            sources,
        }
    }

    fn raw(store_id: i64) -> LcmSourceRef {
        LcmSourceRef::RawMessage { store_id }
    }

    fn child(node_id: &str) -> LcmSourceRef {
        LcmSourceRef::SummaryNode {
            node_id: node_id.to_string(),
        }
    }

    /// One admitted root over `layers` layers of two nodes each, every node
    /// sourcing both nodes of the next layer and the bottom layer sourcing one
    /// shared raw message: the shared-descendant lattice from the issue, with
    /// 2^layers routes to that raw message but only 2·layers + 1 descendants.
    fn lattice(layers: usize, raw_store_id: i64) -> Vec<FixtureNode> {
        let layer_pair =
            |layer: usize| vec![child(&format!("l{layer}a")), child(&format!("l{layer}b"))];
        let mut nodes = vec![node("root", layers as i64 + 1, 1, true, layer_pair(1))];
        for layer in 1..=layers {
            let sources = if layer == layers {
                vec![raw(raw_store_id)]
            } else {
                layer_pair(layer + 1)
            };
            for side in ["a", "b"] {
                nodes.push(node(
                    &format!("l{layer}{side}"),
                    (layers - layer) as i64 + 1,
                    layer as i64 + 1,
                    true,
                    sources.clone(),
                ));
            }
        }
        nodes
    }

    async fn lineage_store(session_id: &str) -> (tempfile::TempDir, TestConnection) {
        let temp = tempfile::tempdir().expect("lineage tempdir");
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );",
        )
        .await
        .expect("session schema");
        conn.execute_batch(test_support::SESSION_GENERATION_SCHEMA)
            .await
            .expect("session generation schema");
        schema::ensure_lcm_schema(&conn).await.expect("lcm schema");
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES (?1, ?2, 'fixture', 'fixture')",
            params![PROVIDER, session_id],
        )
        .await
        .expect("session row");
        test_support::seed_active_generation(&conn, session_id).await;
        (temp, conn)
    }

    async fn seed(conn: &TestConnection, session_id: &str, nodes: &[FixtureNode]) {
        for fixture in nodes {
            conn.execute(
                "INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count, created_at
                 ) VALUES (?1, ?2, ?3, ?3, ?4, ?1, ?5, 1, 1, ?6)",
                params![
                    fixture.node_id.as_str(),
                    PROVIDER,
                    session_id,
                    fixture.depth,
                    projected_content_hash(&fixture.node_id).as_str(),
                    fixture.created_at,
                ],
            )
            .await
            .expect("summary node");
            conn.execute(
                "INSERT INTO session_summary_availability(
                    session_id, generation, summary_id, availability
                 ) VALUES (?1, 1, ?2, ?3)",
                params![
                    session_id,
                    fixture.node_id.as_str(),
                    if fixture.available {
                        "available"
                    } else {
                        "stale"
                    },
                ],
            )
            .await
            .expect("availability row");
            for (ordinal, source) in fixture.sources.iter().enumerate() {
                let (kind, source_id) = match source {
                    LcmSourceRef::RawMessage { store_id } => ("raw_message", store_id.to_string()),
                    LcmSourceRef::SummaryNode { node_id } => ("summary_node", node_id.clone()),
                };
                conn.execute(
                    "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        fixture.node_id.as_str(),
                        kind,
                        source_id.as_str(),
                        ordinal as i64
                    ],
                )
                .await
                .expect("summary source");
            }
        }
    }

    /// Independent reference for the query: available nodes without an
    /// available parent are roots; each root's first raw source is the
    /// minimum over a visited-set BFS of its descendants; replay order is the
    /// query's `ORDER BY` (no fixture sets `source_time_start`).
    fn expected_replay(nodes: &[FixtureNode]) -> Vec<(String, Option<i64>)> {
        let by_id = nodes
            .iter()
            .map(|node| (node.node_id.as_str(), node))
            .collect::<BTreeMap<_, _>>();
        let available = nodes
            .iter()
            .filter(|node| node.available)
            .collect::<Vec<_>>();
        let parented = available
            .iter()
            .flat_map(|node| node.sources.iter())
            .filter_map(|source| match source {
                LcmSourceRef::SummaryNode { node_id } => Some(node_id.as_str()),
                LcmSourceRef::RawMessage { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        let mut roots = available
            .into_iter()
            .filter(|node| !parented.contains(node.node_id.as_str()))
            .map(|root| {
                let mut visited = BTreeSet::new();
                let mut queue = VecDeque::from([root.node_id.as_str()]);
                let mut first_raw = None;
                while let Some(node_id) = queue.pop_front() {
                    if !visited.insert(node_id) {
                        continue;
                    }
                    let Some(current) = by_id.get(node_id) else {
                        continue;
                    };
                    for source in &current.sources {
                        match source {
                            LcmSourceRef::RawMessage { store_id } => {
                                first_raw = Some(
                                    first_raw.map_or(*store_id, |seen: i64| seen.min(*store_id)),
                                );
                            }
                            LcmSourceRef::SummaryNode { node_id } => queue.push_back(node_id),
                        }
                    }
                }
                (root, first_raw)
            })
            .collect::<Vec<_>>();
        roots.sort_by_key(|(root, first_raw)| {
            (
                first_raw.is_none(),
                *first_raw,
                Reverse(root.depth),
                root.created_at,
                root.node_id.clone(),
            )
        });
        roots
            .into_iter()
            .map(|(root, first_raw)| (root.node_id.clone(), first_raw))
            .collect()
    }

    async fn replay(conn: &TestConnection, session_id: &str) -> Vec<(String, Option<i64>)> {
        load_uncondensed_summary_nodes(conn, PROVIDER, session_id)
            .await
            .expect("uncondensed summary nodes")
            .into_iter()
            .map(|entry| (entry.node.node_id, entry.first_source_store_id))
            .collect()
    }

    /// Rows the recursive lineage walk materializes: the work SQLite performs
    /// to answer the query, measured separately from the final result.
    async fn lineage_rows(conn: &TestConnection, session_id: &str) -> i64 {
        let mut rows = conn
            .query(
                &format!("{UNCONDENSED_LINEAGE_CTE} SELECT COUNT(*) FROM lineage"),
                params![PROVIDER, session_id],
            )
            .await
            .expect("lineage row count");
        rows.next()
            .await
            .expect("lineage count row")
            .expect("lineage count present")
            .get(0)
            .expect("lineage count value")
    }

    #[tokio::test]
    async fn shared_descendants_are_visited_once_per_root() {
        let mut vm_steps = BTreeMap::new();
        for layers in [4_usize, 12] {
            let session_id = format!("lattice-{layers}");
            let (temp, conn) = lineage_store(&session_id).await;
            let nodes = lattice(layers, 7);
            seed(&conn, &session_id, &nodes).await;

            let expected = expected_replay(&nodes);
            assert_eq!(expected, vec![("root".to_string(), Some(7))]);
            assert_eq!(replay(&conn, &session_id).await, expected);

            let distinct_descendants = (2 * layers + 1) as i64;
            let rows = lineage_rows(&conn, &session_id).await;
            assert_eq!(
                rows, distinct_descendants,
                "{layers} layers: the lineage walk must materialize one row per distinct descendant, not one per route"
            );
            let steps = sqlite_vm_steps(
                &temp.path().join("sessions.db"),
                &uncondensed_summary_nodes_sql(),
                &[
                    Value::Text(PROVIDER.to_string()),
                    Value::Text(session_id.clone()),
                ],
            );
            println!(
                "lineage lattice: {layers} layers, {} summary nodes, {rows} lineage rows, {steps} SQLite VM steps",
                nodes.len()
            );
            vm_steps.insert(layers, steps);
        }
        // Tripling the reachable graph (9 -> 25 nodes) must cost roughly
        // three times the VM work; path enumeration would cost ~250x.
        assert!(
            vm_steps[&12] <= vm_steps[&4] * 8,
            "lineage VM work grew super-linearly: {vm_steps:?}"
        );
    }

    #[tokio::test]
    async fn replay_covers_multiple_roots_stale_parents_and_cyclic_lineage() {
        let session_id = "mixed-lineage";
        let (_temp, conn) = lineage_store(session_id).await;
        let nodes = vec![
            // A stale parent does not condense its children: both become
            // roots, and `shared` is reached by both through a diamond.
            node(
                "stale-top",
                3,
                1,
                false,
                vec![child("left"), child("right")],
            ),
            node("left", 2, 2, true, vec![child("shared"), raw(40)]),
            node("right", 2, 3, true, vec![child("shared"), raw(30)]),
            node("shared", 1, 4, true, vec![raw(20), raw(25)]),
            // A root whose descendants were corrupted into a cycle still
            // yields the complete reachable minimum and terminates.
            node("cycle-root", 3, 5, true, vec![child("cycle-a")]),
            node("cycle-a", 2, 6, true, vec![child("cycle-b"), raw(90)]),
            node("cycle-b", 1, 7, true, vec![child("cycle-a"), raw(80)]),
            // A root with no raw sources sorts last; a stale root is hidden.
            node("rawless", 1, 8, true, vec![child("missing-child")]),
            node("stale-root", 1, 9, false, vec![raw(1)]),
        ];
        seed(&conn, session_id, &nodes).await;

        let expected = expected_replay(&nodes);
        assert_eq!(
            expected,
            vec![
                ("left".to_string(), Some(20)),
                ("right".to_string(), Some(20)),
                ("cycle-root".to_string(), Some(80)),
                ("rawless".to_string(), None),
            ]
        );
        assert_eq!(replay(&conn, session_id).await, expected);
        // left/right each reach {shared, 40|30, 20, 25}; cycle-root reaches
        // {cycle-a, cycle-b, 90, 80}; rawless reaches {missing-child}.
        assert_eq!(lineage_rows(&conn, session_id).await, 4 + 4 + 4 + 1);
    }
}

#[cfg(test)]
mod privacy_tests {
    use super::*;

    #[test]
    fn summary_text_hint_and_metadata_are_sanitized_before_publication() {
        let secret = "sk-summary-canary-1234567890abcdef";
        let draft = LcmSummaryNodeDraft {
            provider: "codex".to_owned(),
            conversation_id: "conversation-1".to_owned(),
            session_id: "session-1".to_owned(),
            depth: 0,
            summary_text: format!("api_key={secret}"),
            source_refs: vec![LcmSourceRef::RawMessage { store_id: 1 }],
            source_token_count: 1,
            summary_token_count: 1,
            source_time_start: None,
            source_time_end: None,
            expand_hint: Some(format!("Bearer {secret}")),
            metadata_json: Some(json!({"authorization": secret}).to_string()),
        };

        let sanitized = sanitize_summary_draft(draft).expect("sanitize summary draft");
        let durable = serde_json::to_string(&sanitized).expect("serialize sanitized draft");
        assert!(!durable.contains(secret));
        let metadata: JsonValue =
            serde_json::from_str(sanitized.metadata_json.as_deref().expect("metadata"))
                .expect("decode sanitized metadata");
        assert_eq!(
            metadata["tracedecay_privacy"]["sanitization_receipts"]["summary_text"]["disposition"],
            "redacted"
        );
    }
}
