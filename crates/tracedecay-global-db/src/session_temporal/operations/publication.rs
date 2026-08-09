use std::sync::Mutex;

use serde_json::{Value, json};
use tracedecay_domain::{
    EntityKind, RetrievalAnchorId, RetrievalAnchorRecord, RetrievalAnchorTargetV2,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use tracedecay_sessions::retrieval_content::projected_content_hash;
use tracedecay_sessions::runtime::lcm::{
    dag::LcmSummaryPublicationPort,
    types::{
        LcmError, LcmImmutableSummaryPublication, LcmSummaryNode, LcmSummaryPublicationDisposition,
        LcmSummaryPublicationReceipt,
    },
};

use super::{
    CanonicalPublicationManifest, FrozenPublicationReceipt, PUBLICATION_ROUTE, SANITIZER_VERSION,
    generation, load_manifest, logical_identity_digest, receipt_id, sources, summary_projection,
    unixepoch,
};
use crate::session_temporal::relations::{
    SessionRelationProjection, SummaryRelationNode, SummarySourceRef,
};

pub struct GlobalDbLcmSummaryPublication<'a, E> {
    conn: &'a E,
    relation_projection: Mutex<SessionRelationProjection>,
}

impl<'a, E> GlobalDbLcmSummaryPublication<'a, E> {
    pub fn for_scope(conn: &'a E, relation_projection: SessionRelationProjection) -> Self {
        Self {
            conn,
            relation_projection: Mutex::new(relation_projection),
        }
    }
}

impl<E> LcmSummaryPublicationPort for GlobalDbLcmSummaryPublication<'_, E>
where
    E: Executor,
{
    async fn publish_immutable_summary(
        &self,
        publication: LcmImmutableSummaryPublication,
    ) -> Result<LcmSummaryPublicationReceipt, LcmError> {
        let mut projection = self
            .relation_projection
            .lock()
            .map_err(|_| LcmError::Db("session relation publication lock poisoned".to_owned()))?
            .clone();
        let receipt =
            publish_immutable_summary(self.conn, publication.clone(), &projection).await?;
        if receipt.disposition == LcmSummaryPublicationDisposition::ExactReplay {
            let (manifest, _) = load_manifest(self.conn, &publication.summary_id)
                .await?
                .ok_or(LcmError::SummaryNodeNotFound)?;
            verify_projection_summary(&projection, &publication, &manifest)?;
            return Ok(receipt);
        }
        append_summary_relation(
            self.conn,
            &mut projection,
            &publication,
            receipt.generation,
            receipt.published_at,
        )
        .await?;
        *self
            .relation_projection
            .lock()
            .map_err(|_| LcmError::Db("session relation publication lock poisoned".to_owned()))? =
            projection;
        Ok(receipt)
    }
}

fn relation_node(
    publication: &LcmImmutableSummaryPublication,
    manifest: &CanonicalPublicationManifest,
) -> Result<SummaryRelationNode, LcmError> {
    let sources = manifest
        .canonical_sources
        .iter()
        .map(|source| match source.kind.as_str() {
            "summary" => Ok(SummarySourceRef::Summary {
                summary_id: source.id.clone(),
            }),
            "anchor" => RetrievalAnchorId::new(source.id.clone())
                .map(|anchor_id| SummarySourceRef::Anchor { anchor_id })
                .map_err(|error| LcmError::Db(error.to_string())),
            _ => Err(LcmError::ImmutableSummaryConflict {
                summary_id: publication.summary_id.clone(),
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SummaryRelationNode {
        summary_id: publication.summary_id.clone(),
        sources,
        predecessor_summary_id: publication.predecessor_summary_id.clone(),
    })
}

fn verify_projection_summary(
    projection: &SessionRelationProjection,
    publication: &LcmImmutableSummaryPublication,
    manifest: &CanonicalPublicationManifest,
) -> Result<(), LcmError> {
    let expected = relation_node(publication, manifest)?;
    if projection
        .summaries
        .iter()
        .any(|summary| summary == &expected)
    {
        Ok(())
    } else {
        Err(LcmError::ImmutableSummaryConflict {
            summary_id: publication.summary_id.clone(),
        })
    }
}

async fn append_summary_relation(
    conn: &impl Executor,
    projection: &mut SessionRelationProjection,
    publication: &LcmImmutableSummaryPublication,
    generation: i64,
    published_at: i64,
) -> Result<(), LcmError> {
    if projection.session_id.as_str() != publication.draft.session_id {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    projection.generation = u64::try_from(generation)
        .map_err(|error| LcmError::Db(format!("invalid relation generation: {error}")))?;
    let (manifest, _) = load_manifest(conn, &publication.summary_id)
        .await?
        .ok_or(LcmError::SummaryNodeNotFound)?;
    projection
        .summaries
        .push(relation_node(publication, &manifest)?);
    crate::session_temporal::relations::validate_projection(projection)
        .map_err(|error| LcmError::Db(error.to_string()))?;
    crate::session_temporal::relation_receipts::record_relation_receipt(
        conn,
        projection,
        published_at,
    )
    .await
    .map(|_| ())
    .map_err(|error| LcmError::Db(error.to_string()))
}

pub async fn publish_immutable_summary(
    conn: &impl Executor,
    publication: LcmImmutableSummaryPublication,
    relation_projection: &SessionRelationProjection,
) -> Result<LcmSummaryPublicationReceipt, LcmError> {
    validate_publication_shape(&publication)?;
    if let Some((manifest, created_at)) = load_manifest(conn, &publication.summary_id).await? {
        return exact_replay_receipt(conn, &publication, manifest, created_at).await;
    }

    // Cycle checks must precede source materialization: a self-source is not a
    // missing node, and loading first would misreport SummaryNodeNotFound.
    generation::validate_lineage_projection(relation_projection, &publication)?;
    let sources = sources::prepare_sources(conn, &publication).await?;
    let logical_identity = logical_identity_digest(&publication.draft)?;
    generation::validate_current_predecessor(
        conn,
        relation_projection,
        &publication,
        &logical_identity,
    )
    .await?;

    let summary_id = publication.summary_id.as_str();
    let draft = &publication.draft;
    let summary_hash = projected_content_hash(&draft.summary_text);
    let created_at = unixepoch(conn).await?.max(
        sources
            .iter()
            .map(|source| source.timestamp)
            .max()
            .unwrap_or_default(),
    );
    let source_horizon = sources::source_horizon_json(&sources, draft.source_time_end);
    let owner_json = sources::session_owner_json(conn, &draft.provider, &draft.session_id).await?;
    sources::insert_compatibility_source_anchors(conn, &sources, &owner_json).await?;
    let typed_summary_anchor =
        sources::build_summary_anchor(conn, summary_id, &sources, created_at).await?;
    let summary_anchor_id = typed_summary_anchor.as_ref().map_or_else(
        || format!("anchor_summary_{}", projected_content_hash(summary_id)),
        |anchor| anchor.anchor_id().as_str().to_string(),
    );
    let receipt_id = receipt_id(summary_id, &summary_hash);
    let manifest = CanonicalPublicationManifest::from_publication(
        draft,
        summary_hash.clone(),
        &sources,
        source_horizon.clone(),
        owner_json.clone(),
        summary_anchor_id.clone(),
        receipt_id.clone(),
        publication.predecessor_summary_id.clone(),
        logical_identity,
    );
    let publication_json = serde_json::to_string(&manifest)
        .map_err(|error| LcmError::Db(format!("encode summary publication manifest: {error}")))?;

    sources::insert_summary_anchor(
        conn,
        &summary_anchor_id,
        summary_id,
        &owner_json,
        &source_horizon,
        created_at,
        typed_summary_anchor.as_ref(),
    )
    .await?;
    insert_canonical_node(
        conn,
        summary_id,
        draft.session_id.as_str(),
        &summary_anchor_id,
        draft.summary_text.as_str(),
        &source_horizon,
        &publication_json,
        created_at,
    )
    .await?;
    let generation = generation::publish_candidate_generation(
        conn,
        &draft.session_id,
        summary_id,
        publication.predecessor_summary_id.as_deref(),
        &source_horizon,
        created_at,
        relation_projection,
    )
    .await?;
    let frozen_watermarks_json =
        generation::generation_watermarks(conn, &draft.session_id, generation).await?;
    let frozen_receipt = FrozenPublicationReceipt {
        summary_id: summary_id.to_string(),
        disposition: "accepted".to_string(),
        published_at: created_at,
        generation,
        frozen_watermarks_json: frozen_watermarks_json.clone(),
        source_horizon_json: source_horizon,
        publication_manifest_digest: projected_content_hash(&publication_json),
    };
    insert_sanitization_receipt(conn, &receipt_id, &summary_hash, &frozen_receipt).await?;
    sources::insert_payload_manifests(conn, summary_id, &manifest, created_at).await?;

    // The durable summary projection is deliberately last: a projection
    // failure rolls back all canonical rows and lets the outer payload
    // rollback guard remove files.
    summary_projection::project_canonical_summary(conn, summary_id, &manifest, created_at).await?;

    Ok(LcmSummaryPublicationReceipt {
        summary: summary_node(summary_id, &manifest, created_at),
        disposition: LcmSummaryPublicationDisposition::Published,
        generation,
        frozen_watermarks_json,
        published_at: created_at,
    })
}

fn validate_publication_shape(
    publication: &LcmImmutableSummaryPublication,
) -> Result<(), LcmError> {
    let draft = &publication.draft;
    if publication.summary_id.trim().is_empty()
        || draft.provider.trim().is_empty()
        || draft.session_id.trim().is_empty()
        || draft.conversation_id.trim().is_empty()
        || draft.summary_text.trim().is_empty()
        || draft.source_refs.is_empty()
        || draft.depth < 0
    {
        return Err(LcmError::Db(
            "immutable summary publication requires identity, owner, content, and sources"
                .to_string(),
        ));
    }
    match publication.predecessor_summary_id.as_deref() {
        Some(predecessor) if predecessor == publication.summary_id => Err(LcmError::SummaryCycle {
            summary_id: publication.summary_id.clone(),
        }),
        Some(_) | None => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_canonical_node(
    conn: &impl Executor,
    summary_id: &str,
    session_id: &str,
    summary_anchor_id: &str,
    summary_text: &str,
    source_horizon_json: &str,
    publication_json: &str,
    created_at: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, publication_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
        params![
            summary_id,
            session_id,
            summary_anchor_id,
            summary_text,
            source_horizon_json,
            publication_json,
            created_at,
        ],
    )
    .await?;
    Ok(())
}

async fn exact_replay_receipt(
    conn: &impl Executor,
    publication: &LcmImmutableSummaryPublication,
    manifest: CanonicalPublicationManifest,
    created_at: i64,
) -> Result<LcmSummaryPublicationReceipt, LcmError> {
    let summary_id = publication.summary_id.as_str();
    let expected_identity = logical_identity_digest(&publication.draft)?;
    let expected_receipt = receipt_id(
        summary_id,
        &projected_content_hash(&publication.draft.summary_text),
    );
    if !manifest.matches_draft(&publication.draft)
        || manifest.predecessor_summary_id != publication.predecessor_summary_id
        || manifest.logical_identity_digest != expected_identity
        || manifest.receipt_id != expected_receipt
    {
        return Err(conflict(summary_id));
    }
    verify_canonical_node(conn, summary_id, &manifest, created_at).await?;
    verify_summary_anchor(conn, summary_id, &manifest, created_at).await?;
    let receipt = load_and_verify_receipt(conn, summary_id, &manifest, created_at).await?;
    sources::verify_payload_manifests(conn, summary_id, &manifest, created_at).await?;
    Ok(LcmSummaryPublicationReceipt {
        summary: summary_node(summary_id, &manifest, created_at),
        disposition: LcmSummaryPublicationDisposition::ExactReplay,
        generation: receipt.generation,
        frozen_watermarks_json: receipt.frozen_watermarks_json,
        published_at: receipt.published_at,
    })
}

async fn verify_canonical_node(
    conn: &impl Executor,
    summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    created_at: i64,
) -> Result<(), LcmError> {
    let expected_json = serde_json::to_string(manifest)
        .map_err(|error| LcmError::Db(format!("encode summary manifest: {error}")))?;
    let mut rows = conn
        .query(
            "SELECT session_id, summary_anchor_id, summary_text, index_text,
                    source_horizon_json, publication_json, created_at
             FROM session_summary_nodes WHERE summary_id = ?1",
            params![summary_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(conflict(summary_id));
    };
    if row.get::<String>(0)? != manifest.session_id
        || row.get::<String>(1)? != manifest.summary_anchor_id
        || row.get::<String>(2)? != manifest.summary_text
        || row.get::<String>(3)? != manifest.summary_text
        || row.get::<String>(4)? != manifest.source_horizon_json
        || row.get::<String>(5)? != expected_json
        || row.get::<i64>(6)? != created_at
    {
        return Err(conflict(summary_id));
    }
    Ok(())
}

async fn verify_summary_anchor(
    conn: &impl Executor,
    summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    created_at: i64,
) -> Result<(), LcmError> {
    let expected_anchor_json = json!({
        "kind": "immutable_session_summary",
        "anchor_id": manifest.summary_anchor_id,
        "summary_id": summary_id,
        "owner": serde_json::from_str::<Value>(&manifest.owner_json).unwrap_or(Value::Null),
        "source_horizon": serde_json::from_str::<Value>(&manifest.source_horizon_json)
            .unwrap_or(Value::Null),
        "ingested_at": created_at,
        "payload_access": "eligible",
        "retention_class": "retention.session-summary",
    })
    .to_string();
    let mut rows = conn
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![manifest.summary_anchor_id.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(conflict(summary_id));
    };
    let actual_anchor_json = row.get::<String>(0)?;
    let actual_owner_json = row.get::<String>(1)?;
    let typed_match = serde_json::from_str::<RetrievalAnchorRecord>(&actual_anchor_json)
        .ok()
        .is_some_and(|anchor| {
            anchor.anchor_id().as_str() == manifest.summary_anchor_id
                && serde_json::to_string(anchor.owner()).ok().as_deref()
                    == Some(actual_owner_json.as_str())
                && matches!(
                    anchor.target(),
                    RetrievalAnchorTargetV2::Entity(entity)
                        if entity.kind == EntityKind::SessionSummary
                            && entity.id.as_str() == summary_id
                )
        });
    let legacy_match =
        actual_anchor_json == expected_anchor_json && actual_owner_json == manifest.owner_json;
    if (!legacy_match && !typed_match) || row.get::<String>(2)? != PUBLICATION_ROUTE {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(())
}

async fn insert_sanitization_receipt(
    conn: &impl Executor,
    receipt_id: &str,
    payload_digest: &str,
    receipt: &FrozenPublicationReceipt,
) -> Result<(), LcmError> {
    let receipt_json = serde_json::to_string(receipt)
        .map_err(|error| LcmError::Db(format!("encode summary receipt: {error}")))?;
    conn.execute(
        "INSERT OR IGNORE INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt_id,
            SANITIZER_VERSION,
            payload_digest,
            receipt_json.as_str()
        ],
    )
    .await?;
    verify_receipt_row(
        conn,
        receipt_id,
        payload_digest,
        &receipt_json,
        &receipt.summary_id,
    )
    .await
}

async fn verify_receipt_row(
    conn: &impl Executor,
    receipt_id: &str,
    payload_digest: &str,
    receipt_json: &str,
    summary_id: &str,
) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT sanitizer_version, payload_digest, receipt_json
             FROM sanitization_receipts WHERE receipt_id = ?1",
            params![receipt_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(conflict(summary_id));
    };
    if row.get::<String>(0)? != SANITIZER_VERSION
        || row.get::<String>(1)? != payload_digest
        || row.get::<String>(2)? != receipt_json
    {
        return Err(conflict(summary_id));
    }
    Ok(())
}

pub(in crate::session_temporal) async fn load_and_verify_receipt(
    conn: &(impl QueryExecutor + ?Sized),
    summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    created_at: i64,
) -> Result<FrozenPublicationReceipt, LcmError> {
    let mut rows = conn
        .query(
            "SELECT sanitizer_version, payload_digest, receipt_json
             FROM sanitization_receipts WHERE receipt_id = ?1",
            params![manifest.receipt_id.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(conflict(summary_id));
    };
    let receipt_raw: String = row.get(2)?;
    let receipt: FrozenPublicationReceipt =
        serde_json::from_str(&receipt_raw).map_err(|_| conflict(summary_id))?;
    let manifest_json = serde_json::to_string(manifest)
        .map_err(|error| LcmError::Db(format!("encode summary manifest: {error}")))?;
    if row.get::<String>(0)? != SANITIZER_VERSION
        || row.get::<String>(1)? != manifest.summary_hash
        || receipt.summary_id != summary_id
        || receipt.disposition != "accepted"
        || receipt.published_at != created_at
        || receipt.source_horizon_json != manifest.source_horizon_json
        || receipt.publication_manifest_digest != projected_content_hash(&manifest_json)
        || receipt.generation <= 0
        || serde_json::from_str::<Value>(&receipt.frozen_watermarks_json).is_err()
    {
        return Err(conflict(summary_id));
    }

    let mut generation_rows = conn
        .query(
            "SELECT frozen_watermarks_json, state
             FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            params![manifest.session_id.as_str(), receipt.generation],
        )
        .await?;
    let Some(generation_row) = generation_rows.next().await? else {
        return Err(conflict(summary_id));
    };
    let generation_watermarks: String = generation_row.get(0)?;
    let generation_state: String = generation_row.get(1)?;
    if generation_watermarks != receipt.frozen_watermarks_json
        || !matches!(generation_state.as_str(), "active" | "superseded")
    {
        return Err(conflict(summary_id));
    }

    let mut availability_rows = conn
        .query(
            "SELECT availability, source_horizon_json
             FROM session_summary_availability
             WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
            params![manifest.session_id.as_str(), receipt.generation, summary_id],
        )
        .await?;
    let Some(availability_row) = availability_rows.next().await? else {
        return Err(conflict(summary_id));
    };
    if availability_row.get::<String>(0)? != "available"
        || availability_row.get::<String>(1)? != receipt.source_horizon_json
    {
        return Err(conflict(summary_id));
    }

    Ok(receipt)
}

fn summary_node(
    summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    created_at: i64,
) -> LcmSummaryNode {
    LcmSummaryNode {
        node_id: summary_id.to_string(),
        provider: manifest.provider.clone(),
        conversation_id: manifest.conversation_id.clone(),
        session_id: manifest.session_id.clone(),
        depth: manifest.depth,
        summary_text: manifest.summary_text.clone(),
        summary_hash: manifest.summary_hash.clone(),
        source_refs: manifest.source_refs.clone(),
        summary_token_count: manifest.summary_token_count,
        source_token_count: manifest.source_token_count,
        source_time_start: manifest.source_time_start,
        source_time_end: manifest.source_time_end,
        expand_hint: manifest.expand_hint.clone(),
        metadata_json: manifest.metadata_json.clone(),
        created_at,
    }
}

fn conflict(summary_id: &str) -> LcmError {
    LcmError::ImmutableSummaryConflict {
        summary_id: summary_id.to_string(),
    }
}
