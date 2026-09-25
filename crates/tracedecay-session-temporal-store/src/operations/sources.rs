use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tracedecay_domain::{
    AnchorDurabilityClass, AnchorSourceGeneration, EntityId, EntityKind, EntityRef, EvidenceClass,
    PayloadAccessState, ProjectionGenerationId, RetentionClass, RetrievalAnchorRecord,
    RetrievalAnchorRecordParts, RetrievalAnchorTarget, UtcMicros,
};
use tracedecay_runtime_core::db::engine::params;

use tracedecay_lcm::retrieval_content::projected_content_hash;
use tracedecay_lcm::types::{
    LcmError, LcmImmutableSummaryPublication, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft,
};

use super::message_anchor::{ResolvedMessageAnchor, resolve_message_anchors};
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

struct LoadedSummarySource {
    session_id: String,
    source_horizon_json: String,
    publication_json: String,
    summary_anchor_id: String,
    anchor_json: String,
    anchor_owner_json: String,
}

/// A raw source that exists, is owned by the publishing session, and is
/// eligible; what the shared anchor pass and the final binding still need.
struct ValidatedRawSource {
    store_id: i64,
    provider: String,
    session_id: String,
    message_id: String,
    content_hash: String,
    storage_kind: String,
    payload_ref: Option<String>,
    timestamp: Option<i64>,
}

/// A child summary whose node row is session-owned and whose manifest decodes
/// and agrees with the node; the owner and availability checks need the
/// shared reads, so the manifest's owner is carried to them.
struct ValidatedSummarySource<'a> {
    node_id: &'a str,
    node: &'a LoadedSummarySource,
    manifest_owner_json: String,
}

enum ValidatedSource<'a> {
    Raw(ValidatedRawSource),
    Summary(ValidatedSummarySource<'a>),
}

/// Availability of the publication's child summaries in the active generation:
/// `summary_id -> (availability, reason)`.
type SummaryAvailabilityById = BTreeMap<String, (String, Option<String>)>;

/// Resolves every source of a publication against one set of shared reads.
///
/// Sources are validated in order first (existence, ownership, eligibility,
/// manifest agreement) so a per-source refusal surfaces exactly as before.
/// The shared authorities, active generation, session owner, canonical
/// message anchors, child-summary availability, are then each read once for
/// the whole publication instead of once per source, and the bindings are
/// assembled in source order from those results.
#[hotpath::measure(future = true, label = "session_temporal.sources.prepare")]
pub(super) async fn prepare_sources(
    conn: &impl crate::handle::SessionTemporalExec,
    publication: &LcmImmutableSummaryPublication,
) -> Result<Vec<PreparedSource>, LcmError> {
    let draft = &publication.draft;
    let now = unixepoch(conn).await?;
    let mut store_ids = Vec::new();
    let mut summary_ids = Vec::new();
    for source in &draft.source_refs {
        match source {
            LcmSourceRef::RawMessage { store_id } => store_ids.push(*store_id),
            LcmSourceRef::SummaryNode { node_id } => summary_ids.push(node_id.as_str()),
        }
    }
    let raw_by_store_id = raw_messages_by_store_id(conn, &store_ids).await?;
    let summary_by_id = summary_nodes_by_id(conn, &summary_ids).await?;

    let mut validated = Vec::with_capacity(draft.source_refs.len());
    let mut message_ids: Vec<String> = Vec::new();
    for source in &draft.source_refs {
        match source {
            LcmSourceRef::RawMessage { store_id } => {
                let Some(raw) = raw_by_store_id.get(store_id) else {
                    return Err(LcmError::SummarySourceNotOwnedBySession);
                };
                let raw = validate_raw_source(draft, *store_id, raw, now)?;
                if !message_ids.contains(&raw.message_id) {
                    message_ids.push(raw.message_id.clone());
                }
                validated.push(ValidatedSource::Raw(raw));
            }
            LcmSourceRef::SummaryNode { node_id } => {
                let Some(node) = summary_by_id.get(node_id.as_str()) else {
                    return Err(LcmError::SummaryNodeNotFound);
                };
                validated.push(ValidatedSource::Summary(validate_summary_source(
                    draft, node_id, node,
                )?));
            }
        }
    }

    let active_generation = super::generation::active_generation(conn, &draft.session_id).await?;
    let project_key = session_project_key(conn, &draft.provider, &draft.session_id).await?;
    let owner_json = owner_json_for(&draft.provider, &draft.session_id, &project_key);
    let anchors = resolve_message_anchors(
        conn,
        &draft.provider,
        &draft.session_id,
        &project_key,
        active_generation,
        &message_ids,
        now,
    )
    .await?;
    let availability =
        source_summary_availability(conn, &draft.session_id, active_generation, &summary_ids)
            .await?;

    let mut sources = Vec::with_capacity(validated.len());
    for source in validated {
        sources.push(match source {
            ValidatedSource::Raw(raw) => {
                let anchor = anchors.get(&raw.message_id);
                prepare_raw_source(conn, raw, anchor).await?
            }
            ValidatedSource::Summary(summary) => {
                prepare_summary_source(summary, &owner_json, availability.as_ref())?
            }
        });
    }
    Ok(sources)
}

async fn raw_messages_by_store_id(
    conn: &impl crate::handle::SessionTemporalExec,
    store_ids: &[i64],
) -> Result<BTreeMap<i64, Value>, LcmError> {
    if store_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let encoded_ids =
        serde_json::to_string(store_ids).map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT store_id, json_object(
                    'provider', provider,
                    'session_id', session_id,
                    'timestamp', timestamp,
                    'content_hash', content_hash,
                    'storage_kind', storage_kind,
                    'payload_ref', payload_ref,
                    'metadata', metadata_json,
                    'message_id', message_id
                )
             FROM lcm_raw_messages
             WHERE store_id IN (SELECT value FROM json_each(?1))",
            params![encoded_ids],
        )
        .await?;
    let mut messages = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let encoded = row.get::<String>(1)?;
        let raw: Value =
            serde_json::from_str(&encoded).map_err(|error| LcmError::Db(error.to_string()))?;
        messages.entry(store_id).or_insert(raw);
    }
    Ok(messages)
}

async fn summary_nodes_by_id(
    conn: &impl crate::handle::SessionTemporalExec,
    summary_ids: &[&str],
) -> Result<BTreeMap<String, LoadedSummarySource>, LcmError> {
    if summary_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let encoded_ids =
        serde_json::to_string(summary_ids).map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT node.summary_id, node.session_id, node.source_horizon_json,
                    node.publication_json, node.summary_anchor_id, anchor.anchor_json,
                    anchor.owner_json
             FROM session_summary_nodes node
             JOIN retrieval_anchors anchor ON anchor.anchor_id = node.summary_anchor_id
             WHERE node.summary_id IN (SELECT value FROM json_each(?1))",
            params![encoded_ids],
        )
        .await?;
    let mut nodes = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let summary_id: String = row.get(0)?;
        nodes.entry(summary_id).or_insert(LoadedSummarySource {
            session_id: row.get(1)?,
            source_horizon_json: row.get(2)?,
            publication_json: row.get(3)?,
            summary_anchor_id: row.get(4)?,
            anchor_json: row.get(5)?,
            anchor_owner_json: row.get(6)?,
        });
    }
    Ok(nodes)
}

fn validate_raw_source(
    draft: &LcmSummaryNodeDraft,
    store_id: i64,
    raw: &Value,
    now: i64,
) -> Result<ValidatedRawSource, LcmError> {
    let string = |field: &str| {
        raw[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LcmError::Db(format!("raw message {field} is unavailable")))
    };
    let provider = string("provider")?;
    let session_id = string("session_id")?;
    if provider != draft.provider || session_id != draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    validate_source_eligibility(&store_id.to_string(), raw["metadata"].as_str(), now)?;
    Ok(ValidatedRawSource {
        store_id,
        provider,
        session_id,
        message_id: string("message_id")?,
        storage_kind: string("storage_kind")?,
        payload_ref: raw["payload_ref"].as_str().map(str::to_owned),
        content_hash: string("content_hash")?,
        timestamp: raw["timestamp"].as_i64(),
    })
}

/// Binds a validated raw source to its canonical anchor; `None` is the typed
/// "no canonical anchor in this store" outcome of a raw row with no durable
/// observation behind it (an active replay message persisted by compression),
/// the only case that writes an unobserved raw-message anchor.
async fn prepare_raw_source(
    conn: &impl crate::handle::SessionTemporalExec,
    raw: ValidatedRawSource,
    canonical_anchor: Option<&ResolvedMessageAnchor>,
) -> Result<PreparedSource, LcmError> {
    let payload = if raw.storage_kind == LcmStorageKind::External.as_str() {
        Some(
            load_payload_manifest(
                conn,
                &raw.provider,
                &raw.session_id,
                raw.payload_ref.as_deref().ok_or(LcmError::PayloadMissing)?,
            )
            .await?,
        )
    } else {
        None
    };
    let (canonical_id, unobserved_raw_anchor, timestamp) = match canonical_anchor {
        Some(canonical) => canonical.clone(),
        None => {
            let source_timestamp = raw
                .timestamp
                .map(normalize_timestamp)
                .ok_or_else(|| unavailable(&raw.store_id.to_string(), "unverifiable_timestamp"))?;
            (
                unobserved_raw_anchor_id(
                    &raw.provider,
                    &raw.session_id,
                    raw.store_id,
                    &raw.content_hash,
                ),
                true,
                source_timestamp,
            )
        }
    };
    Ok(PreparedSource {
        canonical: CanonicalSourceBinding {
            kind: "anchor".to_string(),
            id: canonical_id,
        },
        unobserved_raw_anchor,
        timestamp,
        payload,
    })
}

fn validate_summary_source<'a>(
    draft: &LcmSummaryNodeDraft,
    node_id: &'a str,
    node: &'a LoadedSummarySource,
) -> Result<ValidatedSummarySource<'a>, LcmError> {
    if node.session_id != draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    let manifest: CanonicalPublicationManifest = serde_json::from_str(&node.publication_json)
        .map_err(|_| LcmError::ImmutableSummaryConflict {
            summary_id: node_id.to_string(),
        })?;
    // A typed child anchor is owned by its source observations; an untyped
    // one by the publishing session, as its manifest records.
    let anchor_owner_matches =
        match serde_json::from_str::<RetrievalAnchorRecord>(&node.anchor_json) {
            Ok(typed) => {
                typed.anchor_id().as_str() == node.summary_anchor_id
                    && typed
                        .owner_column_json()
                        .is_ok_and(|owner| owner == node.anchor_owner_json)
            }
            Err(_) => node.anchor_owner_json == manifest.owner_json,
        };
    if manifest.session_id != draft.session_id
        || manifest.provider != draft.provider
        || manifest.summary_anchor_id != node.summary_anchor_id
        || !anchor_owner_matches
        || manifest.depth >= draft.depth
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(ValidatedSummarySource {
        node_id,
        node,
        manifest_owner_json: manifest.owner_json,
    })
}

fn prepare_summary_source(
    summary: ValidatedSummarySource<'_>,
    expected_owner_json: &str,
    availability: Option<&SummaryAvailabilityById>,
) -> Result<PreparedSource, LcmError> {
    let ValidatedSummarySource {
        node_id,
        node,
        manifest_owner_json,
    } = summary;
    if manifest_owner_json != expected_owner_json {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    require_source_summary_available(availability, node_id)?;
    let timestamp = serde_json::from_str::<Value>(&node.source_horizon_json)
        .ok()
        .and_then(|value| value.get("knowledge_through").and_then(Value::as_i64))
        .ok_or_else(|| unavailable(node_id, "unverifiable_source_horizon"))?;
    Ok(PreparedSource {
        canonical: CanonicalSourceBinding {
            kind: "summary".to_string(),
            id: node_id.to_string(),
        },
        unobserved_raw_anchor: false,
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
    conn: &impl crate::handle::SessionTemporalExec,
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

/// The publishing session's project key; a session this store does not own is
/// a typed ownership refusal, never a fabricated owner.
async fn session_project_key(
    conn: &impl crate::handle::SessionTemporalExec,
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
    Ok(row.get::<String>(0)?)
}

fn owner_json_for(provider: &str, session_id: &str, project_key: &str) -> String {
    json!({
        "kind": "session",
        "provider": provider,
        "session_id": session_id,
        "project_key": project_key,
    })
    .to_string()
}

pub(super) async fn session_owner_json(
    conn: &impl crate::handle::SessionTemporalExec,
    provider: &str,
    session_id: &str,
) -> Result<String, LcmError> {
    let project_key = session_project_key(conn, provider, session_id).await?;
    Ok(owner_json_for(provider, session_id, &project_key))
}

fn unobserved_raw_anchor_id(
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

pub(super) async fn insert_unobserved_raw_anchors(
    conn: &impl crate::handle::SessionTemporalExec,
    sources: &[PreparedSource],
    owner_json: &str,
) -> Result<(), LcmError> {
    let mut seen = BTreeSet::new();
    for source in sources.iter().filter(|source| source.unobserved_raw_anchor) {
        if !seen.insert(source.canonical.id.as_str()) {
            continue;
        }
        let anchor = StoredAnchor {
            anchor_id: source.canonical.id.clone(),
            anchor_json: json!({
                "kind": "lcm_unobserved_raw_message",
                "anchor_id": source.canonical.id,
                "owner": serde_json::from_str::<Value>(owner_json).unwrap_or(Value::Null),
                "ingested_at": source.timestamp,
                "payload_access": "eligible",
                "retention_class": "retention.lcm-raw-message",
            })
            .to_string(),
            owner_json: owner_json.to_string(),
        };
        insert_anchor(conn, &anchor, &source.canonical.id).await?;
    }
    Ok(())
}

/// One `retrieval_anchors` row exactly as publication writes it.
pub(super) struct StoredAnchor {
    pub anchor_id: String,
    pub anchor_json: String,
    pub owner_json: String,
}

/// Derives a summary's retrieval anchor from its canonical sources.
///
/// The first source (in source order) with a typed observation-backed anchor
/// makes the summary anchor a typed [`RetrievalAnchorRecord`] inheriting its
/// owner, watermark, coverage, observations, and authorization. A child
/// summary source contributes its own summary anchor, so a summary of typed
/// summaries is typed too. Sources with no typed anchor (unobserved raw rows,
/// untyped child summaries) carry none of that authority, so such a summary
/// gets a session-owned anchor instead. Anchors are immutable, so publication
/// and exact replay derive the same row.
pub(super) async fn derive_summary_anchor(
    conn: &impl crate::handle::SessionTemporalExec,
    summary_id: &str,
    sources: &[CanonicalSourceBinding],
    owner_json: &str,
    source_horizon_json: &str,
    created_at: i64,
) -> Result<StoredAnchor, LcmError> {
    let Some(source) = first_typed_source_anchor(conn, sources).await? else {
        let anchor_id = format!("anchor_summary_{}", projected_content_hash(summary_id));
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
        return Ok(StoredAnchor {
            anchor_id,
            anchor_json,
            owner_json: owner_json.to_string(),
        });
    };
    let target = RetrievalAnchorTarget::Entity(EntityRef {
        id: EntityId::new(summary_id.to_string())
            .map_err(|error| LcmError::Db(error.to_string()))?,
        kind: EntityKind::SessionSummary,
    });
    let anchor = RetrievalAnchorRecord::new(RetrievalAnchorRecordParts {
        target,
        owner: source.owner().clone(),
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at: UtcMicros(created_at),
        evidence_class: EvidenceClass::DerivedExact,
        source_generation: AnchorSourceGeneration::Unknown,
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
    .map_err(|error| LcmError::Db(error.to_string()))?;
    Ok(StoredAnchor {
        anchor_id: anchor.anchor_id().as_str().to_string(),
        anchor_json: serde_json::to_string(&anchor)
            .map_err(|error| LcmError::Db(format!("encode summary anchor: {error}")))?,
        owner_json: anchor
            .owner_column_json()
            .map_err(|error| LcmError::Db(format!("encode summary anchor owner: {error}")))?,
    })
}

async fn first_typed_source_anchor(
    conn: &impl crate::handle::SessionTemporalExec,
    sources: &[CanonicalSourceBinding],
) -> Result<Option<RetrievalAnchorRecord>, LcmError> {
    let encoded_sources =
        serde_json::to_string(sources).map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT source.key, anchor.anchor_json
             FROM json_each(?1) AS source
             LEFT JOIN session_summary_nodes AS summary
               ON json_extract(source.value, '$.kind') = 'summary'
              AND summary.summary_id = json_extract(source.value, '$.id')
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = CASE json_extract(source.value, '$.kind')
                    WHEN 'summary' THEN summary.summary_anchor_id
                    ELSE json_extract(source.value, '$.id')
                  END
             ORDER BY source.key",
            params![encoded_sources],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        if let Ok(anchor) = serde_json::from_str::<RetrievalAnchorRecord>(&row.get::<String>(1)?) {
            return Ok(Some(anchor));
        }
    }
    Ok(None)
}

pub(super) async fn insert_anchor(
    conn: &impl crate::handle::SessionTemporalExec,
    anchor: &StoredAnchor,
    conflict_id: &str,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor.anchor_id.as_str(),
            anchor.anchor_json.as_str(),
            anchor.owner_json.as_str(),
            PUBLICATION_ROUTE
        ],
    )
    .await?;
    if !stored_anchor_matches(conn, anchor).await? {
        return Err(LcmError::ImmutableSummaryConflict {
            summary_id: conflict_id.to_string(),
        });
    }
    Ok(())
}

/// Whether the stored row for `anchor.anchor_id` is byte-identical to the
/// row publication writes; a missing row is a mismatch.
pub(super) async fn stored_anchor_matches(
    conn: &impl crate::handle::SessionTemporalExec,
    anchor: &StoredAnchor,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![anchor.anchor_id.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    Ok(row.get::<String>(0)? == anchor.anchor_json
        && row.get::<String>(1)? == anchor.owner_json
        && row.get::<String>(2)? == PUBLICATION_ROUTE)
}

pub(super) async fn insert_payload_manifests(
    conn: &impl crate::handle::SessionTemporalExec,
    manifest: &CanonicalPublicationManifest,
) -> Result<(), LcmError> {
    let created_at_by_ref =
        payload_authority_created_at_by_ref(conn, &manifest.payloads, &manifest.session_id).await?;
    for payload in &manifest.payloads {
        let created_at = *created_at_by_ref
            .get(&payload.payload_ref)
            .ok_or(LcmError::PayloadNotOwnedBySession)?;
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
    }
    verify_payload_bindings(
        conn,
        &manifest.payloads,
        &manifest.session_id,
        &created_at_by_ref,
    )
    .await
}

pub(super) async fn verify_payload_manifests(
    conn: &impl crate::handle::SessionTemporalExec,
    manifest: &CanonicalPublicationManifest,
) -> Result<(), LcmError> {
    let created_at_by_ref =
        payload_authority_created_at_by_ref(conn, &manifest.payloads, &manifest.session_id).await?;
    verify_payload_bindings(
        conn,
        &manifest.payloads,
        &manifest.session_id,
        &created_at_by_ref,
    )
    .await
}

async fn payload_authority_created_at_by_ref(
    conn: &impl crate::handle::SessionTemporalExec,
    payloads: &[PreparedPayload],
    session_id: &str,
) -> Result<BTreeMap<String, i64>, LcmError> {
    if payloads.is_empty() {
        return Ok(BTreeMap::new());
    }
    let encoded_refs = encoded_payload_refs(payloads)?;
    let mut rows = conn
        .query(
            "SELECT payload_ref, created_at FROM lcm_external_payloads
             WHERE session_id = ?1
               AND payload_ref IN (SELECT value FROM json_each(?2))",
            params![session_id, encoded_refs],
        )
        .await?;
    let mut created_at = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        let at: i64 = row.get(1)?;
        created_at.entry(payload_ref).or_insert(at);
    }
    for payload in payloads {
        if !created_at.contains_key(&payload.payload_ref) {
            return Err(LcmError::PayloadNotOwnedBySession);
        }
    }
    Ok(created_at)
}

async fn verify_payload_bindings(
    conn: &impl crate::handle::SessionTemporalExec,
    payloads: &[PreparedPayload],
    session_id: &str,
    created_at_by_ref: &BTreeMap<String, i64>,
) -> Result<(), LcmError> {
    if payloads.is_empty() {
        return Ok(());
    }
    let encoded_refs = encoded_payload_refs(payloads)?;
    let mut rows = conn
        .query(
            "SELECT payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
             FROM session_external_payload_manifests
             WHERE payload_ref IN (SELECT value FROM json_each(?1))",
            params![encoded_refs],
        )
        .await?;
    let mut bindings = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        bindings.entry(payload_ref).or_insert((
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<String>(3)?,
            row.get::<String>(4)?,
            row.get::<i64>(5)?,
        ));
    }
    for payload in payloads {
        let Some((bound_session, digest, manifest_json, receipt_id, created_at)) =
            bindings.get(&payload.payload_ref)
        else {
            return Err(LcmError::PayloadMissing);
        };
        let expected_created_at = created_at_by_ref
            .get(&payload.payload_ref)
            .copied()
            .ok_or(LcmError::PayloadNotOwnedBySession)?;
        if bound_session != session_id
            || digest != &payload.digest
            || manifest_json != &payload.manifest_json
            || *created_at != expected_created_at
            || !receipt_binds_payload(conn, payload, session_id, receipt_id).await?
        {
            return Err(LcmError::ImmutablePayloadConflict {
                payload_ref: payload.payload_ref.clone(),
            });
        }
    }
    Ok(())
}

fn encoded_payload_refs(payloads: &[PreparedPayload]) -> Result<String, LcmError> {
    serde_json::to_string(
        &payloads
            .iter()
            .map(|payload| payload.payload_ref.as_str())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| LcmError::Db(error.to_string()))
}

async fn receipt_binds_payload(
    conn: &impl crate::handle::SessionTemporalExec,
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

/// Reads the active-generation availability of every child summary at once.
/// `None` when the session has no active generation yet: availability is
/// generation-bound, so there is nothing to check.
async fn source_summary_availability(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    active_generation: Option<i64>,
    summary_ids: &[&str],
) -> Result<Option<SummaryAvailabilityById>, LcmError> {
    let Some(generation) = active_generation else {
        return Ok(None);
    };
    if summary_ids.is_empty() {
        return Ok(Some(BTreeMap::new()));
    }
    let encoded_ids =
        serde_json::to_string(summary_ids).map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT summary_id, availability, reason
             FROM session_summary_availability
             WHERE session_id = ?1 AND generation = ?2
               AND summary_id IN (SELECT value FROM json_each(?3))",
            params![session_id, generation, encoded_ids],
        )
        .await?;
    let mut availability = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let summary_id: String = row.get(0)?;
        availability
            .entry(summary_id)
            .or_insert((row.get::<String>(1)?, row.get::<Option<String>>(2)?));
    }
    Ok(Some(availability))
}

fn require_source_summary_available(
    availability: Option<&SummaryAvailabilityById>,
    summary_id: &str,
) -> Result<(), LcmError> {
    let Some(availability) = availability else {
        return Ok(());
    };
    let Some((state, reason)) = availability.get(summary_id) else {
        return Err(unavailable(summary_id, "missing_generation_availability"));
    };
    if state != "available" {
        return Err(unavailable(summary_id, reason.as_deref().unwrap_or(state)));
    }
    Ok(())
}
