use std::collections::BTreeSet;

use libsql::{Connection, params};
use serde_json::{Value, json};
use tracedecay_domain::{
    AnchorDurabilityClass, DurableObservationV1, ObservationScopeV1, PayloadAccessState, ProjectId,
    RetrievalAnchorRecord,
};

use crate::sessions::lcm::{
    raw,
    types::{LcmError, LcmImmutableSummaryPublication, LcmSourceRef, LcmStorageKind},
};

use super::{
    CanonicalPublicationManifest, CanonicalSourceBinding, PUBLICATION_ROUTE, PreparedPayload,
    PreparedSource, normalize_timestamp, unixepoch,
};

const SOURCE_UNAVAILABLE_STATES: &[&str] = &[
    "redacted",
    "deleted",
    "retention_expired",
    "quarantined",
    "unavailable",
];

pub(super) async fn prepare_sources(
    conn: &Connection,
    publication: &LcmImmutableSummaryPublication,
) -> Result<Vec<PreparedSource>, LcmError> {
    let mut sources = Vec::with_capacity(publication.draft.source_refs.len());
    let now = unixepoch(conn).await?;
    for source in &publication.draft.source_refs {
        match source {
            LcmSourceRef::RawMessage { store_id } => {
                sources.push(prepare_raw_source(conn, publication, *store_id, now).await?);
            }
            LcmSourceRef::SummaryNode { node_id } => {
                sources.push(prepare_summary_source(conn, publication, node_id).await?);
            }
        }
    }
    Ok(sources)
}

async fn prepare_raw_source(
    conn: &Connection,
    publication: &LcmImmutableSummaryPublication,
    store_id: i64,
    now: i64,
) -> Result<PreparedSource, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, session_id, COALESCE(timestamp, 0), content_hash,
                    storage_kind, payload_ref, metadata_json, message_id
             FROM lcm_raw_messages WHERE store_id = ?1",
            params![store_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    let provider: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    if provider != publication.draft.provider || session_id != publication.draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    let metadata: Option<String> = row.get(6)?;
    validate_source_eligibility(&store_id.to_string(), metadata.as_deref(), now)?;
    let message_id: String = row.get(7)?;
    let canonical_anchor =
        resolve_message_anchor(conn, &provider, &session_id, &message_id, now).await?;
    let storage_kind: String = row.get(4)?;
    let payload_ref: Option<String> = row.get(5)?;
    let payload = if storage_kind == LcmStorageKind::External.as_str() {
        Some(
            load_payload_manifest(
                conn,
                &provider,
                &session_id,
                payload_ref.as_deref().ok_or(LcmError::PayloadMissing)?,
            )
            .await?,
        )
    } else {
        None
    };
    let content_hash: String = row.get(3)?;
    let source_timestamp = normalize_timestamp(row.get(2)?);
    let (canonical_id, compatibility_anchor, timestamp) = canonical_anchor.unwrap_or((
        compatibility_anchor_id(&provider, &session_id, store_id, &content_hash),
        true,
        source_timestamp,
    ));
    Ok(PreparedSource {
        canonical: CanonicalSourceBinding {
            kind: "anchor".to_string(),
            id: canonical_id,
        },
        compatibility_anchor,
        timestamp,
        payload,
    })
}

async fn prepare_summary_source(
    conn: &Connection,
    publication: &LcmImmutableSummaryPublication,
    node_id: &str,
) -> Result<PreparedSource, LcmError> {
    let mut rows = conn
        .query(
            "SELECT node.session_id, node.source_horizon_json, node.publication_json,
                    node.summary_anchor_id, anchor.owner_json
             FROM session_summary_nodes node
             JOIN retrieval_anchors anchor ON anchor.anchor_id = node.summary_anchor_id
             WHERE node.summary_id = ?1",
            params![node_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummaryNodeNotFound);
    };
    if row.get::<String>(0)? != publication.draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    let manifest_raw: String = row.get(2)?;
    let manifest: CanonicalPublicationManifest =
        serde_json::from_str(&manifest_raw).map_err(|_| LcmError::ImmutableSummaryConflict {
            summary_id: node_id.to_string(),
        })?;
    let summary_anchor_id: String = row.get(3)?;
    let owner_json: String = row.get(4)?;
    let expected_owner_json = session_owner_json(
        conn,
        &publication.draft.provider,
        &publication.draft.session_id,
    )
    .await?;
    if manifest.session_id != publication.draft.session_id
        || manifest.provider != publication.draft.provider
        || manifest.summary_anchor_id != summary_anchor_id
        || manifest.owner_json != expected_owner_json
        || manifest.owner_json != owner_json
        || manifest.depth >= publication.draft.depth
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    ensure_source_summary_available(conn, &publication.draft.session_id, node_id).await?;
    let horizon: String = row.get(1)?;
    let timestamp = serde_json::from_str::<Value>(&horizon)
        .ok()
        .and_then(|value| value.get("knowledge_through").and_then(Value::as_i64))
        .unwrap_or_default();
    Ok(PreparedSource {
        canonical: CanonicalSourceBinding {
            kind: "summary".to_string(),
            id: node_id.to_string(),
        },
        compatibility_anchor: false,
        timestamp,
        payload: None,
    })
}

async fn resolve_message_anchor(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<(String, bool, i64)>, LcmError> {
    let Some(generation) = super::generation::active_generation(conn, session_id).await? else {
        return Ok(None);
    };
    let mut rows = conn
        .query(
            "SELECT DISTINCT occurrence.retrieval_anchor_id,
                    anchor.anchor_json, anchor.owner_json, occurrence.knowledge_at,
                    observation.observation_json, observation.receipt_id
             FROM session_occurrences occurrence
             JOIN retrieval_anchors anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             JOIN observations observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE occurrence.session_id = ?1
               AND occurrence.generation = ?2
               AND occurrence.message_id = ?3
             ORDER BY occurrence.retrieval_anchor_id",
            params![session_id, generation, message_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let anchor_id: String = row.get(0)?;
    let anchor_json: String = row.get(1)?;
    let owner_json: String = row.get(2)?;
    let knowledge_at: i64 = row.get(3)?;
    if rows.next().await?.is_some() {
        return Err(LcmError::SummarySourceUnavailable {
            source_id: message_id.to_string(),
            reason: "ambiguous_anchor".to_string(),
        });
    }
    let anchor: RetrievalAnchorRecord = serde_json::from_str(&anchor_json)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_anchor"))?;
    let observation_raw: String = row.get(4)?;
    let observation: DurableObservationV1 = serde_json::from_str(&observation_raw)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_observation"))?;
    let expected_scope = publishing_scope(conn, provider, session_id).await?;
    if observation.source().provider().as_str() != provider
        || observation.source().session_id().as_str() != session_id
        || observation.scope() != &expected_scope
        || anchor.owner() != observation.scope()
        || serde_json::to_string(anchor.owner()).ok().as_deref() != Some(owner_json.as_str())
        || row.get::<String>(5)? != observation.receipt().receipt().receipt_id().as_str()
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    match anchor.payload_access() {
        PayloadAccessState::Eligible => {}
        state => {
            return Err(unavailable(
                &anchor_id,
                &format!("{state:?}").to_ascii_lowercase(),
            ));
        }
    }
    if let AnchorDurabilityClass::RetentionBound { expires_at } = anchor.durability()
        && expires_at.0 <= now
    {
        return Err(unavailable(&anchor_id, "retention_expired"));
    }
    Ok(Some((anchor_id, false, knowledge_at)))
}

async fn publishing_scope(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<ObservationScopeV1, LcmError> {
    let mut rows = conn
        .query(
            "SELECT project_key FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    let project_key: String = row.get(0)?;
    if project_key == "user" {
        return Ok(ObservationScopeV1::Profile);
    }
    ProjectId::new(project_key)
        .map(|project_id| ObservationScopeV1::Project { project_id })
        .map_err(|_| LcmError::SummarySourceNotOwnedBySession)
}

fn unavailable(source_id: &str, reason: &str) -> LcmError {
    LcmError::SummarySourceUnavailable {
        source_id: source_id.to_string(),
        reason: reason.to_string(),
    }
}

fn validate_source_eligibility(
    source_id: &str,
    metadata_json: Option<&str>,
    now: i64,
) -> Result<(), LcmError> {
    let metadata = metadata_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    let state = ["payload_access", "hydration_state", "availability"]
        .iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_str));
    if let Some(state) = state.filter(|state| SOURCE_UNAVAILABLE_STATES.contains(state)) {
        return Err(unavailable(source_id, state));
    }
    let expired = metadata
        .get("retention_expires_at")
        .and_then(Value::as_i64)
        .or_else(|| {
            metadata
                .pointer("/durability/retention_bound/expires_at")
                .and_then(Value::as_i64)
        })
        .is_some_and(|expires_at| expires_at <= now);
    if expired {
        return Err(unavailable(source_id, "retention_expired"));
    }
    Ok(())
}

async fn load_payload_manifest(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<PreparedPayload, LcmError> {
    let mut rows = conn
        .query(
            "SELECT content_hash, message_id, kind, byte_count, char_count, metadata_json
             FROM lcm_external_payloads
             WHERE payload_ref = ?1 AND provider = ?2 AND session_id = ?3",
            params![payload_ref, provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::PayloadNotOwnedBySession);
    };
    Ok(PreparedPayload {
        payload_ref: payload_ref.to_string(),
        digest: row.get(0)?,
        manifest_json: json!({
            "provider": provider,
            "session_id": session_id,
            "message_id": row.get::<String>(1)?,
            "kind": row.get::<String>(2)?,
            "byte_count": row.get::<i64>(3)?,
            "char_count": row.get::<i64>(4)?,
            "metadata": row.get::<Option<String>>(5)?,
        })
        .to_string(),
    })
}

pub(super) async fn session_owner_json(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<String, LcmError> {
    let mut rows = conn
        .query(
            "SELECT project_key FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    Ok(json!({
        "kind": "session",
        "provider": provider,
        "session_id": session_id,
        "project_key": row.get::<String>(0)?,
    })
    .to_string())
}

fn compatibility_anchor_id(
    provider: &str,
    session_id: &str,
    store_id: i64,
    content_hash: &str,
) -> String {
    format!(
        "anchor_lcm_{}",
        raw::sha256_hex(&format!(
            "{provider}\0{session_id}\0{store_id}\0{content_hash}"
        ))
    )
}

pub(super) fn source_horizon_json(sources: &[PreparedSource]) -> String {
    let knowledge_through = sources
        .iter()
        .map(|source| source.timestamp)
        .max()
        .unwrap_or_default();
    json!({
        "knowledge_through": knowledge_through,
        "valid_through": knowledge_through,
    })
    .to_string()
}

pub(super) async fn insert_compatibility_source_anchors(
    conn: &Connection,
    sources: &[PreparedSource],
    owner_json: &str,
) -> Result<(), LcmError> {
    let mut seen = BTreeSet::new();
    for source in sources.iter().filter(|source| source.compatibility_anchor) {
        if !seen.insert(source.canonical.id.as_str()) {
            continue;
        }
        let anchor_json = json!({
            "kind": "legacy_lcm_raw_message",
            "anchor_id": source.canonical.id,
            "owner": serde_json::from_str::<Value>(owner_json).unwrap_or(Value::Null),
            "ingested_at": source.timestamp,
            "payload_access": "eligible",
            "retention_class": "retention.legacy-lcm",
        })
        .to_string();
        conn.execute(
            "INSERT OR IGNORE INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source.canonical.id.as_str(),
                anchor_json.as_str(),
                owner_json,
                PUBLICATION_ROUTE,
            ],
        )
        .await?;
        verify_anchor(
            conn,
            &source.canonical.id,
            &anchor_json,
            owner_json,
            &source.canonical.id,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn insert_summary_anchor(
    conn: &Connection,
    anchor_id: &str,
    summary_id: &str,
    owner_json: &str,
    source_horizon_json: &str,
    created_at: i64,
) -> Result<(), LcmError> {
    let anchor_json = json!({
        "kind": "immutable_session_summary",
        "anchor_id": anchor_id,
        "summary_id": summary_id,
        "owner": serde_json::from_str::<Value>(owner_json).unwrap_or(Value::Null),
        "source_horizon": serde_json::from_str::<Value>(source_horizon_json)
            .unwrap_or(Value::Null),
        "ingested_at": created_at,
        "payload_access": "eligible",
        "retention_class": "retention.session-summary",
    })
    .to_string();
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor_id,
            anchor_json.as_str(),
            owner_json,
            PUBLICATION_ROUTE
        ],
    )
    .await?;
    verify_anchor(conn, anchor_id, &anchor_json, owner_json, summary_id).await
}

async fn verify_anchor(
    conn: &Connection,
    anchor_id: &str,
    anchor_json: &str,
    owner_json: &str,
    conflict_id: &str,
) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![anchor_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    if row.get::<String>(0)? != anchor_json
        || row.get::<String>(1)? != owner_json
        || row.get::<String>(2)? != PUBLICATION_ROUTE
    {
        return Err(LcmError::ImmutableSummaryConflict {
            summary_id: conflict_id.to_string(),
        });
    }
    Ok(())
}

pub(super) async fn insert_payload_manifests(
    conn: &Connection,
    _summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    _created_at: i64,
) -> Result<(), LcmError> {
    for payload in &manifest.payloads {
        let created_at =
            payload_authority_created_at(conn, &payload.payload_ref, &manifest.session_id).await?;
        conn.execute(
            "INSERT OR IGNORE INTO session_external_payload_manifests (
                payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                payload.payload_ref.as_str(),
                manifest.session_id.as_str(),
                payload.digest.as_str(),
                payload.manifest_json.as_str(),
                manifest.receipt_id.as_str(),
                created_at,
            ],
        )
        .await?;
        verify_payload_binding(conn, payload, &manifest.session_id, created_at).await?;
    }
    Ok(())
}

pub(super) async fn verify_payload_manifests(
    conn: &Connection,
    _summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    _created_at: i64,
) -> Result<(), LcmError> {
    for payload in &manifest.payloads {
        let created_at =
            payload_authority_created_at(conn, &payload.payload_ref, &manifest.session_id).await?;
        verify_payload_binding(conn, payload, &manifest.session_id, created_at).await?;
    }
    Ok(())
}

async fn payload_authority_created_at(
    conn: &Connection,
    payload_ref: &str,
    session_id: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT created_at FROM lcm_external_payloads
             WHERE payload_ref = ?1 AND session_id = ?2",
            params![payload_ref, session_id],
        )
        .await?;
    rows.next()
        .await?
        .ok_or(LcmError::PayloadNotOwnedBySession)?
        .get(0)
        .map_err(Into::into)
}

async fn verify_payload_binding(
    conn: &Connection,
    payload: &PreparedPayload,
    session_id: &str,
    created_at: i64,
) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT session_id, payload_digest, manifest_json, receipt_id, created_at
             FROM session_external_payload_manifests WHERE payload_ref = ?1",
            params![payload.payload_ref.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::PayloadMissing);
    };
    let receipt_id: String = row.get(3)?;
    if row.get::<String>(0)? != session_id
        || row.get::<String>(1)? != payload.digest
        || row.get::<String>(2)? != payload.manifest_json
        || row.get::<i64>(4)? != created_at
        || !receipt_binds_payload(conn, payload, session_id, &receipt_id).await?
    {
        return Err(LcmError::ImmutablePayloadConflict {
            payload_ref: payload.payload_ref.clone(),
        });
    }
    Ok(())
}

async fn receipt_binds_payload(
    conn: &Connection,
    payload: &PreparedPayload,
    session_id: &str,
    receipt_id: &str,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT node.session_id,
                    json_extract(node.publication_json, '$.receipt_id'),
                    json_extract(source.value, '$.digest'),
                    json_extract(source.value, '$.manifest_json')
             FROM session_summary_nodes AS node
             JOIN json_each(node.publication_json, '$.payloads') AS source ON TRUE
             JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = json_extract(node.publication_json, '$.receipt_id')
             WHERE json_extract(source.value, '$.payload_ref') = ?1
             ORDER BY node.rowid
             LIMIT 1",
            params![payload.payload_ref.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    Ok(row.get::<String>(0)? == session_id
        && row.get::<String>(1)? == receipt_id
        && row.get::<String>(2)? == payload.digest
        && row.get::<String>(3)? == payload.manifest_json)
}

async fn ensure_source_summary_available(
    conn: &Connection,
    session_id: &str,
    summary_id: &str,
) -> Result<(), LcmError> {
    let Some(generation) = super::generation::active_generation(conn, session_id).await? else {
        return Ok(());
    };
    let mut rows = conn
        .query(
            "SELECT availability, reason
             FROM session_summary_availability
             WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
            params![session_id, generation, summary_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(unavailable(summary_id, "missing_generation_availability"));
    };
    let availability: String = row.get(0)?;
    if availability != "available" {
        return Err(unavailable(
            summary_id,
            &row.get::<Option<String>>(1)?.unwrap_or(availability),
        ));
    }
    Ok(())
}
