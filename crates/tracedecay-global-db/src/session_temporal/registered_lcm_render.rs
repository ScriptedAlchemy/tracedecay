//! LCM compatibility shaping over one frozen registered-store read snapshot.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::HydrationStateV1;

use super::render::apply_canonical_content;
use tracedecay_runtime_core::db::build_qmark_placeholders;
use tracedecay_runtime_core::db::engine::{ReadSnapshot, Row, Value, params, params_from_iter};
use tracedecay_sessions::lcm::contracts::{
    LcmContentRange, LcmContentSlice, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmPayloadRef, LcmRawMessage, LcmRawMessageOverview, LcmSourceRef,
    LcmStorageKind, LcmSummaryNode, LcmSummaryNodeOverview, validate_payload_ref,
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

pub(super) async fn describe(
    snapshot: &ReadSnapshot,
    request: LcmDescribeRequest,
) -> Result<LcmDescribeResponse, LcmError> {
    let provider = request.provider.as_str();
    let session_id = request.session_id.as_str();
    let counts = describe_counts(snapshot, provider, session_id).await?;
    let (target, raw_messages, summary_nodes, summary_node, external_payload) = match request.target
    {
        LcmDescribeTarget::Session => (
            "session".to_string(),
            raw_message_overviews(snapshot, provider, session_id).await?,
            summary_overviews(snapshot, provider, session_id).await?,
            None,
            None,
        ),
        LcmDescribeTarget::SummaryNode { node_id } => (
            "summary_node".to_string(),
            Vec::new(),
            Vec::new(),
            Some(describe_summary_node(snapshot, provider, session_id, &node_id).await?),
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
    })
}

pub(super) async fn expand(
    snapshot: &ReadSnapshot,
    request: LcmExpandRequest,
    canonical_content: &str,
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
                raw_message: Some(raw),
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref,
                from_current_session: Some(from_current_session),
                externalized_note: None,
                source_pagination: None,
            }
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            let summary =
                load_summary_node(snapshot, &request.provider, &request.session_id, &node_id)
                    .await?;
            validate_summary_source_ownership(
                snapshot,
                &request.provider,
                &request.session_id,
                &node_id,
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
            let summary_sources =
                load_summary_sources(snapshot, &request.provider, &request.session_id, &page_refs)
                    .await?;
            LcmExpandResponse {
                kind: "summary_node".to_string(),
                content: String::new(),
                content_range: empty_content_range(slice),
                raw_message: None,
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
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(payload_ref),
                from_current_session: None,
                externalized_note: None,
                source_pagination: None,
            }
        }
    };

    Ok(apply_canonical_content(expansion, slice, canonical_content))
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
) -> Result<Vec<LcmSummaryNodeOverview>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT n.node_id, n.conversation_id, n.depth, n.created_at,
                COUNT(s.source_id)
         FROM lcm_summary_nodes AS n
         LEFT JOIN lcm_summary_sources AS s ON s.node_id = n.node_id
         WHERE n.provider = ?1 AND n.session_id = ?2
         GROUP BY n.node_id, n.conversation_id, n.depth, n.created_at
         ORDER BY n.depth, n.created_at, n.node_id
         LIMIT 20",
        params![provider, session_id],
    )
    .await?;
    let mut out = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        out.push(LcmSummaryNodeOverview {
            node_id: field!(&row, 0)?,
            conversation_id: field!(&row, 1)?,
            depth: field!(&row, 2)?,
            summary_preview: String::new(),
            source_count: field!(&row, 4, i64)?.max(0) as usize,
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
    let children = describe_summary_sources(snapshot, provider, session_id, node_id).await?;
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
) -> Result<Vec<LcmDescribeSourceOverview>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT source.source_kind, source.source_id,
                raw.role, raw.storage_kind,
                child.summary_token_count, child.source_token_count, child.expand_hint
         FROM lcm_summary_sources AS source
         LEFT JOIN lcm_raw_messages AS raw
           ON source.source_kind = 'raw_message'
          AND raw.store_id = CAST(source.source_id AS INTEGER)
          AND raw.provider = ?2
          AND raw.session_id = ?3
         LEFT JOIN lcm_summary_nodes AS child
           ON source.source_kind = 'summary_node'
          AND child.node_id = source.source_id
          AND child.provider = ?2
          AND child.session_id = ?3
         WHERE source.node_id = ?1
         ORDER BY source.ordinal",
        params![node_id, provider, session_id],
    )
    .await?;
    let mut out = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        let source_kind: String = field!(&row, 0)?;
        let source_id: String = field!(&row, 1)?;
        match source_kind.as_str() {
            "raw_message" => {
                let store_id = parse_store_id(&source_id)?;
                let Some(role) = field!(&row, 2, Option<String>)? else {
                    continue;
                };
                let Some(storage_kind_text) = field!(&row, 3, Option<String>)? else {
                    continue;
                };
                out.push(LcmDescribeSourceOverview {
                    source_kind,
                    source_ref: LcmSourceRef::RawMessage { store_id },
                    store_id: Some(store_id),
                    node_id: None,
                    role: Some(role),
                    storage_kind: LcmStorageKind::from_db(&storage_kind_text),
                    summary_token_count: None,
                    source_token_count: None,
                    expand_hint: None,
                });
            }
            "summary_node" => {
                let Some(summary_token_count) = field!(&row, 4, Option<i64>)? else {
                    continue;
                };
                let Some(source_token_count) = field!(&row, 5, Option<i64>)? else {
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
                    summary_token_count: Some(summary_token_count),
                    source_token_count: Some(source_token_count),
                    expand_hint: field!(&row, 6)?,
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

async fn validate_summary_source_ownership(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    node_id: &str,
) -> Result<(), LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT source.source_kind
         FROM lcm_summary_sources AS source
         LEFT JOIN lcm_raw_messages AS raw
           ON source.source_kind = 'raw_message'
          AND raw.store_id = CAST(source.source_id AS INTEGER)
         LEFT JOIN lcm_summary_nodes AS child
          ON source.source_kind = 'summary_node'
          AND child.node_id = source.source_id
         WHERE source.node_id = ?1
           AND (
                (
                    source.source_kind = 'raw_message'
                    AND (
                         raw.store_id IS NULL
                      OR raw.provider != ?2
                      OR raw.session_id != ?3
                    )
                )
             OR (
                    source.source_kind = 'summary_node'
                    AND (
                         child.node_id IS NULL
                      OR child.provider != ?2
                      OR child.session_id != ?3
                    )
                )
           )
         ORDER BY source.ordinal
         LIMIT 1",
        params![node_id, provider, session_id],
    )
    .await?;
    let Some(row) = next_row(&mut rows).await? else {
        return Ok(());
    };
    match field!(&row, 0, String)?.as_str() {
        "raw_message" => Err(LcmError::SummarySourceNotOwnedBySession),
        "summary_node" => Err(LcmError::SummaryNodeNotFound),
        _ => Ok(()),
    }
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

async fn load_raw_message(
    snapshot: &ReadSnapshot,
    store_id: i64,
) -> Result<LcmRawMessage, LcmError> {
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
    let row = next_row(&mut rows)
        .await?
        .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
    raw_message_from_row(&row)
}

fn raw_message_from_row(row: &Row) -> Result<LcmRawMessage, LcmError> {
    let storage_kind_text: String = field!(row, 9)?;
    let storage_kind = storage_kind(&storage_kind_text)?;
    let content: Option<String> = field!(row, 7)?;
    let snippet: String = field!(row, 11)?;
    Ok(LcmRawMessage {
        provider: field!(row, 0)?,
        message_id: field!(row, 1)?,
        session_id: field!(row, 2)?,
        store_id: field!(row, 3)?,
        role: field!(row, 4)?,
        ordinal: field!(row, 5)?,
        timestamp: field!(row, 6)?,
        content: match storage_kind {
            LcmStorageKind::Inline => content.unwrap_or_default(),
            LcmStorageKind::External => content.unwrap_or(snippet),
        },
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
    Ok(LcmSummaryNode {
        node_id: field!(&row, 0)?,
        provider: node_provider,
        conversation_id: field!(&row, 2)?,
        session_id: node_session_id,
        depth: field!(&row, 4)?,
        summary_text: field!(&row, 5)?,
        summary_hash: field!(&row, 6)?,
        source_refs: load_source_refs(snapshot, node_id).await?,
        summary_token_count: field!(&row, 7)?,
        source_token_count: field!(&row, 8)?,
        source_time_start: field!(&row, 9)?,
        source_time_end: field!(&row, 10)?,
        expand_hint: field!(&row, 11)?,
        metadata_json: field!(&row, 12)?,
        created_at: field!(&row, 13)?,
    })
}

async fn load_source_refs(
    snapshot: &ReadSnapshot,
    node_id: &str,
) -> Result<Vec<LcmSourceRef>, LcmError> {
    let mut rows = query(
        snapshot,
        "SELECT source_kind, source_id
         FROM lcm_summary_sources
         WHERE node_id = ?1
         ORDER BY ordinal",
        params![node_id],
    )
    .await?;
    let mut out = Vec::new();
    while let Some(row) = next_row(&mut rows).await? {
        let source_kind: String = field!(&row, 0)?;
        let source_id: String = field!(&row, 1)?;
        out.push(source_ref(&source_kind, &source_id)?);
    }
    Ok(out)
}

async fn load_summary_sources(
    snapshot: &ReadSnapshot,
    provider: &str,
    session_id: &str,
    source_refs: &[LcmSourceRef],
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
    let children = load_summary_nodes(snapshot, &child_ids).await?;
    let mut out = Vec::with_capacity(source_refs.len());
    for source_ref in source_refs {
        match source_ref {
            LcmSourceRef::RawMessage { store_id } => {
                let raw = raw
                    .get(store_id)
                    .cloned()
                    .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
                if raw.provider != provider || raw.session_id != session_id {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                }
                out.push(LcmExpandedSummarySource {
                    source_ref: source_ref.clone(),
                    state: HydrationStateV1::RetainedButUnavailable,
                    content: String::new(),
                    content_range: None,
                    content_truncated: false,
                    raw_message: Some(raw),
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
) -> Result<BTreeMap<i64, LcmRawMessage>, LcmError> {
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
        let raw = raw_message_from_row(&row)?;
        out.insert(raw.store_id, raw);
    }
    Ok(out)
}

async fn load_summary_nodes(
    snapshot: &ReadSnapshot,
    node_ids: &BTreeSet<String>,
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
    let mut rows = query(snapshot, &sql, params_from_iter(values.clone())).await?;
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
    let sql = format!(
        "SELECT node_id, source_kind, source_id
         FROM lcm_summary_sources
         WHERE node_id IN ({placeholders})
         ORDER BY node_id, ordinal"
    );
    let mut rows = query(snapshot, &sql, params_from_iter(values)).await?;
    while let Some(row) = next_row(&mut rows).await? {
        let node_id: String = field!(&row, 0)?;
        if let Some(node) = out.get_mut(&node_id) {
            let source_kind: String = field!(&row, 1)?;
            let source_id: String = field!(&row, 2)?;
            node.source_refs.push(source_ref(&source_kind, &source_id)?);
        }
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

fn source_ref(source_kind: &str, source_id: &str) -> Result<LcmSourceRef, LcmError> {
    match source_kind {
        "raw_message" => Ok(LcmSourceRef::RawMessage {
            store_id: parse_store_id(source_id)?,
        }),
        "summary_node" => Ok(LcmSourceRef::SummaryNode {
            node_id: source_id.to_string(),
        }),
        _ => Err(LcmError::Db(format!(
            "invalid summary source_kind: {source_kind}"
        ))),
    }
}

fn parse_store_id(source_id: &str) -> Result<i64, LcmError> {
    source_id
        .parse::<i64>()
        .map_err(|error| LcmError::Db(format!("invalid raw source id: {error}")))
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
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    #[tokio::test]
    async fn registered_metadata_rendering_matches_the_canonical_fixture() {
        let directory = tempdir().expect("temporary session store");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        runtime
            .seed_lcm_render_fixture_for_test(HostAdmissionScope::Profile)
            .await
            .expect("canonical LCM render fixture");

        let describe_requests = [
            LcmDescribeRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmDescribeTarget::Session,
            },
            LcmDescribeRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmDescribeTarget::SummaryNode {
                    node_id: "summary-parent".to_string(),
                },
            },
            LcmDescribeRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmDescribeTarget::ExternalPayload {
                    payload_ref: "payload-a".to_string(),
                },
            },
        ];
        let expand_requests = [
            LcmExpandRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmExpandTarget::RawMessage { store_id: 11 },
                content_slice: Some(LcmContentSlice {
                    offset: 2,
                    limit: 7,
                }),
                source_offset: 0,
                source_limit: None,
            },
            LcmExpandRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmExpandTarget::SummaryNode {
                    node_id: "summary-parent".to_string(),
                },
                content_slice: Some(LcmContentSlice {
                    offset: 1,
                    limit: 9,
                }),
                source_offset: 0,
                source_limit: Some(2),
            },
            LcmExpandRequest {
                provider: "codex".to_string(),
                session_id: "session-a".to_string(),
                target: LcmExpandTarget::ExternalPayload {
                    payload_ref: "payload-a".to_string(),
                },
                content_slice: Some(LcmContentSlice {
                    offset: 3,
                    limit: 8,
                }),
                source_offset: 0,
                source_limit: None,
            },
        ];

        let session = runtime
            .lcm_describe_for_test(describe_requests[0].clone())
            .await
            .expect("registered session describe");
        assert_eq!(session.target, "session");
        assert_eq!(session.raw_message_count, 2);
        assert_eq!(session.summary_node_count, 2);
        assert_eq!(session.external_payload_count, 1);
        assert_eq!(
            (session.first_store_id, session.last_store_id),
            (Some(11), Some(12))
        );

        let summary = runtime
            .lcm_describe_for_test(describe_requests[1].clone())
            .await
            .expect("registered summary describe");
        let summary_node = summary.summary_node.expect("summary metadata");
        assert_eq!(summary_node.node_id, "summary-parent");
        assert_eq!(summary_node.children.len(), 2);

        let payload = runtime
            .lcm_describe_for_test(describe_requests[2].clone())
            .await
            .expect("registered payload describe");
        let external = payload.external_payload.expect("external payload metadata");
        assert_eq!(external.payload_ref, "payload-a");
        assert_eq!(external.content_preview, "canonical external payload");

        let expected = [
            ("raw_message", "nonical", 0usize),
            ("summary_node", "anonical ", 2usize),
            ("external_payload", "onical e", 0usize),
        ];
        for (request, (kind, content, source_count)) in expand_requests.into_iter().zip(expected) {
            let expansion = runtime
                .lcm_expand_for_test(request)
                .await
                .expect("registered expansion");
            assert_eq!(expansion.kind, kind);
            assert_eq!(expansion.content, content);
            assert_eq!(expansion.summary_sources.len(), source_count);
        }
    }
}
