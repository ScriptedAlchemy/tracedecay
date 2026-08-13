//! Released LCM response shaping over one canonical frozen-store read snapshot.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::HydrationStateV1;

use super::relations::{SummaryRelationRead, SummarySourceRef as GraphSummarySourceRef};
use super::render::apply_canonical_content;
use tracedecay_runtime_core::db::build_qmark_placeholders;
use tracedecay_runtime_core::db::engine::{ReadSnapshot, Row, Value, params, params_from_iter};
use tracedecay_sessions::lcm::contracts::{
    LcmContentRange, LcmContentSlice, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmPayloadRef, LcmRawMessageMetadata, LcmRawMessageOverview,
    LcmSourceRef, LcmStorageKind, LcmSummaryNode, LcmSummaryNodeOverview, validate_payload_ref,
};

macro_rules! field {
    ($row:expr, $column:expr) => {
        $row.get($column)
            .map_err(|error| LcmError::Db(error.to_string()))
    };
    ($row:expr, $column:expr, $type:ty) => {
        $row.get::<$type>($column)
            .map_err(|error| LcmError::Db(error.to_string()))
    };
}

pub(super) async fn describe_relation_summary_ids(
    snapshot: &ReadSnapshot,
    request: &LcmDescribeRequest,
) -> Result<Vec<String>, LcmError> {
    match &request.target {
        LcmDescribeTarget::Session => {
            session_summary_ids(snapshot, &request.provider, &request.session_id).await
        }
        LcmDescribeTarget::SummaryNode { node_id } => Ok(vec![node_id.clone()]),
        LcmDescribeTarget::ExternalPayload { .. } => Ok(Vec::new()),
    }
}

pub(super) fn expand_relation_summary_ids(request: &LcmExpandRequest) -> Vec<String> {
    match &request.target {
        LcmExpandTarget::SummaryNode { node_id } => vec![node_id.clone()],
        LcmExpandTarget::RawMessage { .. } | LcmExpandTarget::ExternalPayload { .. } => Vec::new(),
    }
}

async fn session_summary_ids(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
) -> Result<Vec<String>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT node_id
         FROM lcm_summary_nodes
         WHERE provider = ?1 AND session_id = ?2
         ORDER BY depth, created_at, node_id
         LIMIT 20",
        params![provider, session_id],
    )
    .await?;
    let mut ids = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        ids.push(field!(&row, 0)?);
    }
    Ok(ids)
}

pub(super) async fn describe(
    snapshot: &ReadSnapshot,
    request: LcmDescribeRequest,
    relations: &[SummaryRelationRead],
) -> Result<LcmDescribeResponse, LcmError> {
    let provider = request.provider.as_str();
    let session_id = request.session_id.as_str();
    let counts = describe_counts(snapshot, provider, session_id).await?;
    let (target, raw_messages, summary_nodes, summary_node, external_payload) = match request.target
    {
        LcmDescribeTarget::Session => (
            "session".to_string(),
            raw_message_overviews(snapshot, provider, session_id).await?,
            summary_overviews(snapshot, provider, session_id, relations).await?,
            None,
            None,
        ),
        LcmDescribeTarget::SummaryNode { node_id } => (
            "summary_node".to_string(),
            Vec::new(),
            Vec::new(),
            Some(describe_summary_node(snapshot, provider, session_id, &node_id, relations).await?),
            None,
        ),
        LcmDescribeTarget::ExternalPayload { payload_ref } => (
            "external_payload".to_string(),
            Vec::new(),
            Vec::new(),
            None,
            Some(describe_external_payload(snapshot, provider, session_id, &payload_ref).await?),
        ),
    };

    let session_token_estimate = if target == "session" {
        let store = tracedecay_sessions::runtime::lcm::query::store_status(
            snapshot,
            provider,
            Some(session_id),
        )
        .await?;
        store
            .token_estimate
            .complete
            .then_some(store.estimated_tokens)
    } else {
        None
    };
    Ok(LcmDescribeResponse {
        target,
        provider: request.provider,
        session_id: request.session_id,
        raw_message_count: counts.raw_messages,
        summary_node_count: counts.summary_nodes,
        external_payload_count: counts.external_payloads,
        first_store_id: counts.first_store_id,
        last_store_id: counts.last_store_id,
        raw_messages,
        summary_nodes,
        summary_node,
        external_payload,
        session_token_estimate,
    })
}

pub(super) async fn expand(
    snapshot: &ReadSnapshot,
    request: LcmExpandRequest,
    canonical_content: &str,
    relations: &[SummaryRelationRead],
) -> Result<LcmExpandResponse, LcmError> {
    let slice = request.content_slice.unwrap_or(LcmContentSlice {
        offset: 0,
        limit: usize::MAX,
    });
    let expansion = match request.target {
        LcmExpandTarget::RawMessage { store_id } => {
            let raw = load_raw_message(snapshot, store_id).await?;
            if raw.provider != request.provider {
                return Err(LcmError::SummarySourceNotOwnedBySession);
            }
            let from_current_session = raw.session_id == request.session_id;
            let payload_ref = (!from_current_session)
                .then(|| raw.payload_ref.clone())
                .flatten();
            LcmExpandResponse {
                kind: "raw_message".to_string(),
                content: String::new(),
                content_range: empty_content_range(slice),
                raw_message: None,
                raw_message_metadata: Some(raw),
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref,
                from_current_session: Some(from_current_session),
                externalized_note: None,
                source_pagination: None,
            }
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            let summary = load_summary_node(
                snapshot,
                &request.provider,
                &request.session_id,
                &node_id,
                relations,
            )
            .await?;
            let total_sources = summary.source_refs.len();
            let source_pagination =
                source_pagination(total_sources, request.source_offset, request.source_limit);
            let page_refs = summary
                .source_refs
                .iter()
                .skip(source_pagination.source_offset)
                .take(source_pagination.source_limit)
                .cloned()
                .collect::<Vec<_>>();
            let summary_sources = load_summary_sources(
                snapshot,
                &request.provider,
                &request.session_id,
                &page_refs,
                relations,
            )
            .await?;
            LcmExpandResponse {
                kind: "summary_node".to_string(),
                content: String::new(),
                content_range: empty_content_range(slice),
                raw_message: None,
                raw_message_metadata: None,
                summary_node: Some(summary),
                summary_sources,
                payload_ref: None,
                from_current_session: None,
                externalized_note: None,
                source_pagination: Some(source_pagination),
            }
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            validate_expand_payload(
                snapshot,
                &request.provider,
                &request.session_id,
                &payload_ref,
            )
            .await?;
            LcmExpandResponse {
                kind: "external_payload".to_string(),
                content: String::new(),
                content_range: empty_content_range(slice),
                raw_message: None,
                raw_message_metadata: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(payload_ref),
                from_current_session: None,
                externalized_note: None,
                source_pagination: None,
            }
        }
    };

    apply_canonical_content(expansion, slice, canonical_content)
}

struct DescribeCounts {
    raw_messages: i64,
    summary_nodes: i64,
    external_payloads: i64,
    first_store_id: Option<i64>,
    last_store_id: Option<i64>,
}

async fn describe_counts(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
) -> Result<DescribeCounts, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT
             (SELECT COUNT(*) FROM lcm_raw_messages
              WHERE provider = ?1 AND session_id = ?2),
             (SELECT COUNT(*) FROM lcm_summary_nodes
              WHERE provider = ?1 AND session_id = ?2),
             (SELECT COUNT(*) FROM lcm_external_payloads
              WHERE provider = ?1 AND session_id = ?2),
             (SELECT MIN(store_id) FROM lcm_raw_messages
              WHERE provider = ?1 AND session_id = ?2),
             (SELECT MAX(store_id) FROM lcm_raw_messages
              WHERE provider = ?1 AND session_id = ?2)",
        params![provider, session_id],
    )
    .await?;
    let row = next_row(&mut rows)
        .await?
        .ok_or_else(|| LcmError::Db("LCM describe counts returned no rows".to_string()))?;
    Ok(DescribeCounts {
        raw_messages: field!(&row, 0)?,
        summary_nodes: field!(&row, 1)?,
        external_payloads: field!(&row, 2)?,
        first_store_id: field!(&row, 3)?,
        last_store_id: field!(&row, 4)?,
    })
}

async fn raw_message_overviews(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
) -> Result<Vec<LcmRawMessageOverview>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT message_id, store_id, role, storage_kind, payload_ref,
                LENGTH(snippet_text)
         FROM lcm_raw_messages
         WHERE provider = ?1 AND session_id = ?2
         ORDER BY store_id
         LIMIT 20",
        params![provider, session_id],
    )
    .await?;
    let mut out = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        let storage_kind_text: String = field!(&row, 3)?;
        let total_chars = field!(&row, 5, i64)?.max(0) as u64;
        out.push(LcmRawMessageOverview {
            message_id: field!(&row, 0)?,
            store_id: field!(&row, 1)?,
            role: field!(&row, 2)?,
            storage_kind: storage_kind(&storage_kind_text)?,
            payload_ref: field!(&row, 4)?,
            content_preview: String::new(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 0,
                returned_chars: 0,
                total_chars,
                truncated: total_chars > 0,
            },
        });
    }
    Ok(out)
}

async fn summary_overviews(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    relations: &[SummaryRelationRead],
) -> Result<Vec<LcmSummaryNodeOverview>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT node_id, conversation_id, depth, created_at
         FROM lcm_summary_nodes
         WHERE provider = ?1 AND session_id = ?2
         ORDER BY depth, created_at, node_id
         LIMIT 20",
        params![provider, session_id],
    )
    .await?;
    let mut out = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        let node_id: String = field!(&row, 0)?;
        let source_count = relation(relations, &node_id)?.sources.len();
        out.push(LcmSummaryNodeOverview {
            node_id,
            conversation_id: field!(&row, 1)?,
            depth: field!(&row, 2)?,
            summary_preview: String::new(),
            source_count,
            created_at: field!(&row, 3)?,
        });
    }
    Ok(out)
}

async fn describe_summary_node(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    node_id: &str,
    relations: &[SummaryRelationRead],
) -> Result<LcmDescribeSummaryNode, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT node_id, conversation_id, depth, summary_token_count,
                source_token_count, source_time_start, source_time_end,
                expand_hint, metadata_json, created_at
         FROM lcm_summary_nodes
         WHERE provider = ?1 AND session_id = ?2 AND node_id = ?3",
        params![provider, session_id, node_id],
    )
    .await?;
    let row = next_row(&mut rows)
        .await?
        .ok_or(LcmError::SummaryNodeNotFound)?;
    let children =
        describe_summary_sources(snapshot, provider, session_id, node_id, relations).await?;
    Ok(LcmDescribeSummaryNode {
        node_id: field!(&row, 0)?,
        conversation_id: field!(&row, 1)?,
        depth: field!(&row, 2)?,
        summary_token_count: field!(&row, 3)?,
        source_token_count: field!(&row, 4)?,
        source_time_start: field!(&row, 5)?,
        source_time_end: field!(&row, 6)?,
        expand_hint: field!(&row, 7)?,
        metadata_json: field!(&row, 8)?,
        created_at: field!(&row, 9)?,
        source_count: children.len(),
        children,
    })
}

async fn describe_summary_sources(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    node_id: &str,
    relations: &[SummaryRelationRead],
) -> Result<Vec<LcmDescribeSourceOverview>, LcmError> {
    let source_refs = relation_source_refs(
        snapshot,
        provider,
        session_id,
        relation(relations, node_id)?,
    )
    .await?;
    let mut out = Vec::new();
    for source_ref in source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                // An *absent* raw row is not an ownership violation: the
                // projection-durability retention drop pass (plan 38 §3)
                // deletes raw rows precisely because the summary is the durable
                // survivor, so the lineage outlives the row it names. Describe
                // still reports the source — eliding it would understate the
                // summary's lineage — but carries no raw metadata for it, which
                // is how this overview already spells "no raw row backs this
                // ref" (`role`/`storage_kind` are read straight off that row).
                // `tracedecay_lcm_expand` on the same node reports the typed
                // `HydrationStateV1::RetentionExpired` state.
                let raw = find_raw_message(snapshot, store_id).await?;
                // A row that is present but foreign is still a hard ownership
                // violation and must never be disclosed.
                if let Some(raw) = raw.as_ref()
                    && (raw.provider != provider || raw.session_id != session_id)
                {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                out.push(LcmDescribeSourceOverview {
                    source_kind: "raw_message".to_owned(),
                    source_ref: LcmSourceRef::RawMessage { store_id },
                    store_id: Some(store_id),
                    node_id: None,
                    role: raw.as_ref().map(|raw| raw.role.clone()),
                    storage_kind: raw.as_ref().map(|raw| raw.storage_kind),
                    summary_token_count: None,
                    source_token_count: None,
                    expand_hint: None,
                });
            }
            LcmSourceRef::SummaryNode { node_id: child_id } => {
                let child =
                    load_summary_node(snapshot, provider, session_id, &child_id, relations).await?;
                out.push(LcmDescribeSourceOverview {
                    source_kind: "summary_node".to_owned(),
                    source_ref: LcmSourceRef::SummaryNode {
                        node_id: child_id.clone(),
                    },
                    store_id: None,
                    node_id: Some(child_id),
                    role: None,
                    storage_kind: None,
                    summary_token_count: Some(child.summary_token_count),
                    source_token_count: Some(child.source_token_count),
                    expand_hint: child.expand_hint,
                });
            }
        }
    }
    Ok(out)
}

async fn describe_external_payload(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<LcmDescribeExternalPayload, LcmError> {
    validate_payload_ref(payload_ref)?;
    let payload = load_payload(snapshot, payload_ref).await?;
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotFound);
    }
    Ok(LcmDescribeExternalPayload {
        payload_ref: payload.payload_ref,
        provider: payload.provider,
        session_id: payload.session_id,
        message_id: payload.message_id,
        kind: payload.kind,
        content_hash: payload.content_hash,
        byte_count: payload.byte_count,
        char_count: payload.char_count,
        created_at: payload.created_at,
        metadata_json: payload.metadata_json,
        content_preview: String::new(),
    })
}

/// Loads the raw row a directly requested `store_id` names, refusing when it is
/// gone. Summary *lineage* reads must use [`find_raw_message`] instead: an
/// absent row there is retention, not a missing target.
async fn load_raw_message(
    snapshot: &ReadSnapshot,
    store_id: i64,
) -> Result<LcmRawMessageMetadata, LcmError> {
    find_raw_message(snapshot, store_id)
        .await?
        .ok_or(LcmError::SummarySourceNotOwnedBySession)
}

/// Reads one raw row by `store_id` alone, so a row belonging to another session
/// is *present* (and rejected by the caller's ownership check) rather than
/// indistinguishable from a row retention already removed.
async fn find_raw_message(
    snapshot: &ReadSnapshot,
    store_id: i64,
) -> Result<Option<LcmRawMessageMetadata>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT provider, message_id, session_id, store_id, role, ordinal,
                timestamp, NULL AS content, content_hash, storage_kind, payload_ref,
                '' AS snippet_text, legacy_source, legacy_truncated, metadata_json
         FROM lcm_raw_messages
         WHERE store_id = ?1",
        params![store_id],
    )
    .await?;
    let Some(row) = next_row(&mut rows).await? else {
        return Ok(None);
    };
    raw_message_metadata_from_row(&row).map(Some)
}

fn raw_message_metadata_from_row(row: &Row) -> Result<LcmRawMessageMetadata, LcmError> {
    let storage_kind_text: String = field!(row, 9)?;
    let storage_kind = storage_kind(&storage_kind_text)?;
    Ok(LcmRawMessageMetadata {
        provider: field!(row, 0)?,
        message_id: field!(row, 1)?,
        session_id: field!(row, 2)?,
        store_id: field!(row, 3)?,
        role: field!(row, 4)?,
        ordinal: field!(row, 5)?,
        timestamp: field!(row, 6)?,
        content_hash: field!(row, 8)?,
        storage_kind,
        payload_ref: field!(row, 10)?,
        legacy_source: field!(row, 12, i64).unwrap_or(0) != 0,
        legacy_truncated: field!(row, 13, i64).unwrap_or(0) != 0,
        metadata_json: field!(row, 14)?,
    })
}

async fn load_summary_node(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    node_id: &str,
    relations: &[SummaryRelationRead],
) -> Result<LcmSummaryNode, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT node_id, provider, conversation_id, session_id, depth,
                '' AS summary_text, summary_hash, summary_token_count,
                source_token_count, source_time_start, source_time_end,
                expand_hint, metadata_json, created_at
         FROM lcm_summary_nodes
         WHERE node_id = ?1",
        params![node_id],
    )
    .await?;
    let row = next_row(&mut rows)
        .await?
        .ok_or(LcmError::SummaryNodeNotFound)?;
    let node_provider: String = field!(&row, 1)?;
    let node_session_id: String = field!(&row, 3)?;
    if node_provider != provider || node_session_id != session_id {
        return Err(LcmError::SummaryNodeNotFound);
    }
    let source_refs = relation_source_refs(
        snapshot,
        provider,
        session_id,
        relation(relations, node_id)?,
    )
    .await?;
    Ok(LcmSummaryNode {
        node_id: field!(&row, 0)?,
        provider: node_provider,
        conversation_id: field!(&row, 2)?,
        session_id: node_session_id,
        depth: field!(&row, 4)?,
        summary_text: field!(&row, 5)?,
        summary_hash: field!(&row, 6)?,
        source_refs,
        summary_token_count: field!(&row, 7)?,
        source_token_count: field!(&row, 8)?,
        source_time_start: field!(&row, 9)?,
        source_time_end: field!(&row, 10)?,
        expand_hint: field!(&row, 11)?,
        metadata_json: field!(&row, 12)?,
        created_at: field!(&row, 13)?,
    })
}

async fn relation_source_refs(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    relation: &SummaryRelationRead,
) -> Result<Vec<LcmSourceRef>, LcmError> {
    let mut out = Vec::with_capacity(relation.sources.len());
    for (ordinal, source) in relation.sources.iter().enumerate() {
        match source {
            GraphSummarySourceRef::Anchor { anchor_id } => {
                out.push(LcmSourceRef::RawMessage {
                    store_id: anchor_store_id(
                        snapshot,
                        provider,
                        session_id,
                        &relation.summary_id,
                        ordinal,
                        anchor_id.as_str(),
                    )
                    .await?,
                });
            }
            GraphSummarySourceRef::Summary { summary_id } => {
                out.push(LcmSourceRef::SummaryNode {
                    node_id: summary_id.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// Resolves the `store_id` an anchored summary source names.
///
/// The anchor is bound to a message occurrence, and the occurrence reaches the
/// locator only through the raw row, so the retention drop pass (plan 38 §3)
/// takes the mapping down with the row it deletes. That must not make the
/// summary unreadable, so a resolution that finds no raw row falls back to the
/// projected lineage, which retains the locator; see
/// [`retention_dropped_store_id`].
async fn anchor_store_id(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    summary_id: &str,
    ordinal: usize,
    anchor_id: &str,
) -> Result<i64, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT raw.store_id
         FROM session_occurrences AS occurrence
         JOIN session_temporal_generations AS generation
           ON generation.session_id = occurrence.session_id
          AND generation.generation = occurrence.generation
          AND generation.state = 'active'
         JOIN lcm_raw_messages AS raw
           ON raw.message_id = occurrence.message_id
          AND raw.provider = ?1
          AND raw.session_id = ?2
         WHERE occurrence.session_id = ?2
           AND occurrence.retrieval_anchor_id = ?3
         ORDER BY raw.store_id",
        params![provider, session_id, anchor_id],
    )
    .await?;
    let Some(store_id) = next_row(&mut rows)
        .await?
        .map(|row| field!(&row, 0, i64))
        .transpose()?
    else {
        return retention_dropped_store_id(snapshot, summary_id, ordinal).await;
    };
    if next_row(&mut rows).await?.is_some() {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(store_id)
}

/// Recovers the locator of a raw source whose row retention already dropped.
///
/// Publication writes both lineage records from the same manifest source list:
/// the projected `lcm_summary_sources` row carries the `store_id` as text at the
/// source's ordinal (`operations::summary_projection`), and the relation graph
/// carries the anchor at that same ordinal (`relations::build_graph` enumerates
/// the same sequence). Retention drops the raw row but never the lineage, so the
/// projected record still names the locator the anchor can no longer reach.
///
/// The recovered locator only ever *names* a source that the caller then reports
/// as retention-expired — no content or metadata is disclosed. It is refused
/// unless the raw row is genuinely absent: a present row that the anchor failed
/// to reach is an identity or ownership problem, not retention, and must keep
/// failing closed.
async fn retention_dropped_store_id(
    snapshot: &ReadSnapshot,
    summary_id: &str,
    ordinal: usize,
) -> Result<i64, LcmError> {
    let ordinal = i64::try_from(ordinal).map_err(|_| LcmError::SummarySourceNotOwnedBySession)?;
    let mut rows = query(
        snapshot,
        "SELECT source_id
         FROM lcm_summary_sources
         WHERE node_id = ?1 AND ordinal = ?2 AND source_kind = 'raw_message'",
        params![summary_id, ordinal],
    )
    .await?;
    let Some(row) = next_row(&mut rows).await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    let store_id = field!(&row, 0, String)?
        .parse::<i64>()
        .map_err(|_| LcmError::SummarySourceNotOwnedBySession)?;
    if find_raw_message(snapshot, store_id).await?.is_some() {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(store_id)
}

fn relation<'a>(
    relations: &'a [SummaryRelationRead],
    summary_id: &str,
) -> Result<&'a SummaryRelationRead, LcmError> {
    relations
        .iter()
        .find(|relation| relation.summary_id == summary_id)
        .ok_or(LcmError::SummaryNodeNotFound)
}

async fn load_summary_sources(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    source_refs: &[LcmSourceRef],
    relations: &[SummaryRelationRead],
) -> Result<Vec<LcmExpandedSummarySource>, LcmError> {
    let raw_ids = source_refs
        .iter()
        .filter_map(|source| match source {
            LcmSourceRef::RawMessage { store_id } => Some(*store_id),
            LcmSourceRef::SummaryNode { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let child_ids = source_refs
        .iter()
        .filter_map(|source| match source {
            LcmSourceRef::SummaryNode { node_id } => Some(node_id.clone()),
            LcmSourceRef::RawMessage { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let raw = load_raw_messages(snapshot, &raw_ids).await?;
    let children =
        load_summary_nodes(snapshot, provider, session_id, &child_ids, relations).await?;
    let mut out = Vec::with_capacity(source_refs.len());
    for source_ref in source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                // An *absent* raw row is not an ownership violation: publication
                // proves every raw source exists and is session-owned before the
                // lineage row is written (`operations::sources::prepare_raw_source`),
                // so a row missing at read time was removed afterwards — by the
                // projection-durability retention drop pass (plan 38 §3), whose
                // whole premise is that the summary is the durable survivor.
                // Report the source as retention-expired (plan 23 hydration
                // state) and keep rendering; aborting would make every summary
                // older than the drop window unreadable, and would do so under a
                // misleading ownership error.
                let Some(metadata) = raw.get(store_id).cloned() else {
                    out.push(LcmExpandedSummarySource {
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
                if metadata.provider != provider || metadata.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                out.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    state: HydrationStateV1::RetainedButUnavailable,
                    content: String::new(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: None,
                    raw_message_metadata: Some(metadata),
                    summary_node: None,
                });
            }
            LcmSourceRef::SummaryNode { node_id } => {
                let child = children
                    .get(node_id)
                    .cloned()
                    .ok_or(LcmError::SummaryNodeNotFound)?;
                if child.provider != provider || child.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                out.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    state: HydrationStateV1::RetainedButUnavailable,
                    content: String::new(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: None,
                    raw_message_metadata: None,
                    summary_node: Some(Box::new(child)),
                });
            }
        }
    }
    Ok(out)
}

async fn load_raw_messages(
    snapshot: &ReadSnapshot,
    store_ids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, LcmRawMessageMetadata>, LcmError> {
    if store_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = build_qmark_placeholders(store_ids.len());
    let sql = format!(
        "SELECT provider, message_id, session_id, store_id, role, ordinal,
                timestamp, NULL AS content, content_hash, storage_kind, payload_ref,
                '' AS snippet_text, legacy_source, legacy_truncated, metadata_json
         FROM lcm_raw_messages
         WHERE store_id IN ({placeholders})"
    );
    let values = store_ids
        .iter()
        .copied()
        .map(Value::Integer)
        .collect::<Vec<_>>();
    let mut rows = query(snapshot, &sql, params_from_iter(values)).await?;
    let mut out = BTreeMap::new();
    while let Some(row) = next_row(&mut rows).await? {
        let raw = raw_message_metadata_from_row(&row)?;
        out.insert(raw.store_id, raw);
    }
    Ok(out)
}

async fn load_summary_nodes(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    node_ids: &BTreeSet<String>,
    relations: &[SummaryRelationRead],
) -> Result<BTreeMap<String, LcmSummaryNode>, LcmError> {
    if node_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = build_qmark_placeholders(node_ids.len());
    let values = node_ids
        .iter()
        .cloned()
        .map(Value::Text)
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT node_id, provider, conversation_id, session_id, depth,
                '' AS summary_text, summary_hash, summary_token_count,
                source_token_count, source_time_start, source_time_end,
                expand_hint, metadata_json, created_at
         FROM lcm_summary_nodes
         WHERE node_id IN ({placeholders})"
    );
    let mut rows = query(snapshot, &sql, params_from_iter(values)).await?;
    let mut out = BTreeMap::new();
    while let Some(row) = next_row(&mut rows).await? {
        let node_id: String = field!(&row, 0)?;
        out.insert(
            node_id.clone(),
            LcmSummaryNode {
                node_id,
                provider: field!(&row, 1)?,
                conversation_id: field!(&row, 2)?,
                session_id: field!(&row, 3)?,
                depth: field!(&row, 4)?,
                summary_text: field!(&row, 5)?,
                summary_hash: field!(&row, 6)?,
                source_refs: Vec::new(),
                summary_token_count: field!(&row, 7)?,
                source_token_count: field!(&row, 8)?,
                source_time_start: field!(&row, 9)?,
                source_time_end: field!(&row, 10)?,
                expand_hint: field!(&row, 11)?,
                metadata_json: field!(&row, 12)?,
                created_at: field!(&row, 13)?,
            },
        );
    }
    for node_id in node_ids {
        let source_refs = relation_source_refs(
            snapshot,
            provider,
            session_id,
            relation(relations, node_id)?,
        )
        .await?;
        let node = out.get_mut(node_id).ok_or(LcmError::SummaryNodeNotFound)?;
        if node.provider != provider || node.session_id != session_id {
            return Err(LcmError::SummarySourceNotOwnedBySession);
        }
        node.source_refs = source_refs;
    }
    Ok(out)
}

async fn validate_expand_payload(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<(), LcmError> {
    validate_payload_ref(payload_ref)?;
    let payload = load_payload(snapshot, payload_ref).await?;
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotOwnedBySession);
    }
    let mut rows = query(
        snapshot,
        "SELECT 1
         FROM lcm_raw_messages
         WHERE provider = ?1
           AND session_id = ?2
           AND message_id = ?3
           AND storage_kind = 'external'
           AND payload_ref = ?4
         LIMIT 1",
        params![
            payload.provider.as_str(),
            payload.session_id.as_str(),
            payload.message_id.as_str(),
            payload.payload_ref.as_str(),
        ],
    )
    .await?;
    if next_row(&mut rows).await?.is_none() {
        return Err(LcmError::PayloadNotFound);
    }
    Ok(())
}

async fn load_payload(
    snapshot: &ReadSnapshot,
    payload_ref: &str,
) -> Result<LcmPayloadRef, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT payload_ref, provider, session_id, message_id, kind, content_hash,
                byte_count, char_count, created_at, metadata_json
         FROM lcm_external_payloads
         WHERE payload_ref = ?1",
        params![payload_ref],
    )
    .await?;
    let row = next_row(&mut rows)
        .await?
        .ok_or(LcmError::PayloadNotFound)?;
    Ok(LcmPayloadRef {
        payload_ref: field!(&row, 0)?,
        provider: field!(&row, 1)?,
        session_id: field!(&row, 2)?,
        message_id: field!(&row, 3)?,
        kind: field!(&row, 4)?,
        content_hash: field!(&row, 5)?,
        byte_count: field!(&row, 6, i64)?.max(0) as u64,
        char_count: field!(&row, 7, i64)?.max(0) as u64,
        created_at: field!(&row, 8)?,
        metadata_json: field!(&row, 9)?,
    })
}

fn source_pagination(
    total_sources: usize,
    source_offset: usize,
    source_limit: Option<usize>,
) -> LcmExpandSourcePagination {
    let source_offset = source_offset.min(total_sources);
    let remaining = total_sources - source_offset;
    let source_limit = source_limit.map_or(remaining, |limit| limit.min(remaining));
    let consumed = source_offset.saturating_add(source_limit);
    let has_more = consumed < total_sources;
    LcmExpandSourcePagination {
        source_offset,
        source_limit,
        returned_sources: source_limit,
        total_sources,
        next_source_offset: has_more.then_some(consumed),
        has_more,
        remaining_sources: if has_more {
            total_sources - consumed
        } else {
            0
        },
    }
}

fn empty_content_range(slice: LcmContentSlice) -> LcmContentRange {
    LcmContentRange {
        offset: slice.offset as u64,
        limit: slice.limit as u64,
        returned_chars: 0,
        total_chars: 0,
        truncated: false,
    }
}

fn storage_kind(value: &str) -> Result<LcmStorageKind, LcmError> {
    LcmStorageKind::from_db(value)
        .ok_or_else(|| LcmError::Db(format!("invalid storage_kind: {value}")))
}

async fn query<P>(
    snapshot: &ReadSnapshot,
    sql: &str,
    params: P,
) -> Result<tracedecay_runtime_core::db::engine::Rows, LcmError>
where
    P: tracedecay_runtime_core::db::engine::IntoParams,
{
    snapshot
        .query(sql, params)
        .await
        .map_err(|error| LcmError::Db(error.to_string()))
}

async fn next_row(
    rows: &mut tracedecay_runtime_core::db::engine::Rows,
) -> Result<Option<Row>, LcmError> {
    rows.next()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))
}

#[cfg(test)]
#[path = "registered_lcm_render/tests.rs"]
mod tests;
