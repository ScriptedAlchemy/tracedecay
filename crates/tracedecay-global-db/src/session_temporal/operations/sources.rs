use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{
    AnchorDurabilityClass, AnchorSourceGenerationV2, EntityId, EntityKind, EntityRef,
    EvidenceClass, PayloadAccessState, ProjectionGenerationId, RetentionClass,
    RetrievalAnchorRecord, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, UtcMicros,
};
use tracedecay_runtime_core::db::engine::{Executor, params};

use tracedecay_sessions::compatibility::projected_content_hash;
use tracedecay_sessions::runtime::lcm::types::{
    LcmError, LcmImmutableSummaryPublication, LcmSourceRef, LcmStorageKind,
};

use super::message_anchor::resolve_message_anchor;
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
    conn: &impl Executor,
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
    conn: &impl Executor,
    publication: &LcmImmutableSummaryPublication,
    store_id: i64,
    now: i64,
) -> Result<PreparedSource, LcmError> {
    let mut rows = conn
        .query(
            "SELECT json_object(
                    'provider', provider,
                    'session_id', session_id,
                    'timestamp', timestamp,
                    'content_hash', content_hash,
                    'storage_kind', storage_kind,
                    'payload_ref', payload_ref,
                    'metadata', metadata_json,
                    'message_id', message_id
                )
             FROM lcm_raw_messages WHERE store_id = ?1",
            params![store_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    let encoded = row.get::<String>(0)?;
    let raw: serde_json::Value =
        serde_json::from_str(&encoded).map_err(|error| LcmError::Db(error.to_string()))?;
    let string = |field: &str| {
        raw[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LcmError::Db(format!("raw message {field} is unavailable")))
    };
    let provider = string("provider")?;
    let session_id = string("session_id")?;
    if provider != publication.draft.provider || session_id != publication.draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    validate_source_eligibility(&store_id.to_string(), raw["metadata"].as_str(), now)?;
    let message_id = string("message_id")?;
    let canonical_anchor =
        resolve_message_anchor(conn, &provider, &session_id, &message_id, now).await?;
    let storage_kind = string("storage_kind")?;
    let payload = if storage_kind == LcmStorageKind::External.as_str() {
        Some(
            load_payload_manifest(
                conn,
                &provider,
                &session_id,
                raw["payload_ref"]
                    .as_str()
                    .ok_or(LcmError::PayloadMissing)?,
            )
            .await?,
        )
    } else {
        None
    };
    let content_hash = string("content_hash")?;
    let source_timestamp = normalize_timestamp(raw["timestamp"].as_i64().unwrap_or(0));
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
    conn: &impl Executor,
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

pub(super) fn unavailable(source_id: &str, reason: &str) -> LcmError {
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
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Result<PreparedPayload, LcmError> {
    let mut rows = conn
        .query(
            "SELECT json_object(
                    'content_hash', content_hash,
                    'message_id', message_id,
                    'kind', kind,
                    'byte_count', byte_count,
                    'char_count', char_count,
                    'metadata', metadata_json
                )
             FROM lcm_external_payloads
             WHERE payload_ref = ?1 AND provider = ?2 AND session_id = ?3",
            params![payload_ref, provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::PayloadNotOwnedBySession);
    };
    let encoded = row.get::<String>(0)?;
    let manifest: serde_json::Value =
        serde_json::from_str(&encoded).map_err(|error| LcmError::Db(error.to_string()))?;
    let string = |field: &str| {
        manifest[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LcmError::Db(format!("external payload {field} is unavailable")))
    };
    let number = |field: &str| {
        manifest[field]
            .as_i64()
            .ok_or_else(|| LcmError::Db(format!("external payload {field} is unavailable")))
    };
    Ok(PreparedPayload {
        payload_ref: payload_ref.to_string(),
        digest: string("content_hash")?,
        manifest_json: json!({
            "provider": provider,
            "session_id": session_id,
            "message_id": string("message_id")?,
            "kind": string("kind")?,
            "byte_count": number("byte_count")?,
            "char_count": number("char_count")?,
            "metadata": manifest["metadata"],
        })
        .to_string(),
    })
}

pub(super) async fn session_owner_json(
    conn: &impl Executor,
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
        projected_content_hash(&format!(
            "{provider}\0{session_id}\0{store_id}\0{content_hash}"
        ))
    )
}

pub(super) fn source_horizon_json(
    sources: &[PreparedSource],
    declared_source_time_end: Option<i64>,
) -> String {
    let knowledge_through = sources
        .iter()
        .map(|source| source.timestamp)
        .max()
        .unwrap_or_default();
    // `knowledge_through` lives in the ingest-time domain, but lineage
    // eligibility compares source occurrences' VALID (event) times against
    // `valid_through`. A summary that declares the event-time range it
    // covers must keep sources inside that range eligible even when their
    // event times exceed the ingest clock, so the declared end extends the
    // valid horizon; without a declaration the ingest bound is the only
    // truthful upper bound available.
    let valid_through = declared_source_time_end
        .map(normalize_timestamp)
        .map_or(knowledge_through, |declared| {
            declared.max(knowledge_through)
        });
    json!({
        "knowledge_through": knowledge_through,
        "valid_through": valid_through,
    })
    .to_string()
}

pub(super) async fn insert_compatibility_source_anchors(
    conn: &impl Executor,
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

pub(super) async fn build_summary_anchor(
    conn: &impl Executor,
    summary_id: &str,
    sources: &[PreparedSource],
    created_at: i64,
) -> Result<Option<RetrievalAnchorRecord>, LcmError> {
    let mut retained_source = None;
    for source in sources {
        let mut rows = conn
            .query(
                "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1",
                params![source.canonical.id.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            continue;
        };
        let encoded = row.get::<String>(0)?;
        if let Ok(anchor) = serde_json::from_str::<RetrievalAnchorRecord>(&encoded) {
            retained_source = Some(anchor);
            break;
        }
    }
    let Some(source) = retained_source else {
        return Ok(None);
    };
    let target = RetrievalAnchorTargetV2::Entity(EntityRef {
        id: EntityId::new(summary_id.to_string())
            .map_err(|error| LcmError::Db(error.to_string()))?,
        kind: EntityKind::SessionSummary,
    });
    RetrievalAnchorRecord::new(RetrievalAnchorRecordV2Parts {
        target,
        owner: source.owner().clone(),
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at: UtcMicros(created_at),
        evidence_class: EvidenceClass::DerivedExact,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new(PUBLICATION_ROUTE)
            .map_err(|error| LcmError::Db(error.to_string()))?,
        projection_watermark: source.projection_watermark().clone(),
        coverage: source.coverage().clone(),
        source_observations: source.source_observations().to_vec(),
        source_anchors: Vec::new(),
        authorization: source.authorization().clone(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.session-summary")
            .map_err(|error| LcmError::Db(error.to_string()))?,
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .map(Some)
    .map_err(|error| LcmError::Db(error.to_string()))
}

pub(super) async fn insert_summary_anchor(
    conn: &impl Executor,
    anchor_id: &str,
    summary_id: &str,
    owner_json: &str,
    source_horizon_json: &str,
    created_at: i64,
    typed_anchor: Option<&RetrievalAnchorRecord>,
) -> Result<(), LcmError> {
    let stored_owner_json = match typed_anchor {
        Some(anchor) => serde_json::to_string(anchor.owner())
            .map_err(|error| LcmError::Db(format!("encode summary anchor owner: {error}")))?,
        None => owner_json.to_string(),
    };
    let anchor_json = match typed_anchor {
        Some(anchor) => serde_json::to_string(anchor)
            .map_err(|error| LcmError::Db(format!("encode summary anchor: {error}")))?,
        None => json!({
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
        .to_string(),
    };
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor_id,
            anchor_json.as_str(),
            stored_owner_json.as_str(),
            PUBLICATION_ROUTE
        ],
    )
    .await?;
    verify_anchor(
        conn,
        anchor_id,
        &anchor_json,
        &stored_owner_json,
        summary_id,
    )
    .await
}

async fn verify_anchor(
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
