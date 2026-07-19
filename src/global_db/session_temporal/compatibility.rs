use serde::Deserialize;
use thiserror::Error;
use tracedecay_domain::{
    CompactContextLineageEdgeV1, HydrationStateV1, RetrievalAnchorId, RetrievalAnchorRecord,
    RetrievalGrainV1, SessionAuthorityClassV1, SessionId, SignedCursorKeyRefV1,
    TemporalAssertionKindV1, TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};

use crate::global_db::{GlobalDb, GlobalDbReadSnapshot};
use crate::query::temporal::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use crate::query::temporal::resolution::ValidatedAuthorization;
use crate::sessions::lcm::{
    LcmDescribeRequest, LcmDescribeResponse, LcmDescribeTarget, LcmError, LcmExpandRequest,
    LcmExpandResponse, LcmExpandTarget, LcmStorageKind,
};

use super::hydration::hydrate_authorized_anchor_bytes;

#[derive(Clone, Debug, Deserialize)]
struct FrozenWatermarks {
    active_generation: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
    projection_frontier: u64,
    source_frontier: u64,
    summary_frontier: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthorizedSessionDescribeRequest {
    provider: String,
    session_id: String,
    target: LcmDescribeTarget,
    grain: RetrievalGrainV1,
    authorization_digest: String,
}

impl AuthorizedSessionDescribeRequest {
    pub(crate) fn new(
        provider: impl Into<String>,
        session_id: impl Into<String>,
        target: LcmDescribeTarget,
        grain: RetrievalGrainV1,
        authorization_digest: impl Into<String>,
    ) -> Result<Self, CompatibilityReadError> {
        let request = Self {
            provider: provider.into(),
            session_id: session_id.into(),
            target,
            grain,
            authorization_digest: authorization_digest.into(),
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), CompatibilityReadError> {
        if [&self.provider, &self.session_id].into_iter().any(|value| {
            value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
        }) || !is_digest(&self.authorization_digest)
        {
            return Err(CompatibilityReadError::Denied);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthorizedSessionExpandRequest {
    provider: String,
    session_id: String,
    target: LcmExpandTarget,
    grain: RetrievalGrainV1,
    content_slice: crate::sessions::lcm::LcmContentSlice,
    source_offset: usize,
    source_limit: Option<usize>,
    authorization_digest: String,
}

impl AuthorizedSessionExpandRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: impl Into<String>,
        session_id: impl Into<String>,
        target: LcmExpandTarget,
        grain: RetrievalGrainV1,
        content_slice: crate::sessions::lcm::LcmContentSlice,
        source_offset: usize,
        source_limit: Option<usize>,
        authorization_digest: impl Into<String>,
    ) -> Result<Self, CompatibilityReadError> {
        let request = Self {
            provider: provider.into(),
            session_id: session_id.into(),
            target,
            grain,
            content_slice,
            source_offset,
            source_limit,
            authorization_digest: authorization_digest.into(),
        };
        if [&request.provider, &request.session_id]
            .into_iter()
            .any(|value| {
                value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
            })
            || request.content_slice.limit == 0
            || request.source_limit == Some(0)
            || !is_digest(&request.authorization_digest)
        {
            return Err(CompatibilityReadError::Denied);
        }
        Ok(request)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompatibilityWatermarks {
    pub(crate) generation: u64,
    pub(crate) source: u64,
    pub(crate) projection: u64,
    pub(crate) index: u64,
    pub(crate) summary: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompatibilityTemporalMetadata {
    pub(crate) anchors: Vec<RetrievalAnchorId>,
    pub(crate) watermarks: CompatibilityWatermarks,
    pub(crate) coverage: TemporalCoverageCountsV1,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AuthorizedSessionDescribeResult {
    pub(crate) description: LcmDescribeResponse,
    pub(crate) temporal: CompatibilityTemporalMetadata,
    pub(crate) grain: RetrievalGrainV1,
    pub(crate) state: HydrationStateV1,
    pub(crate) lineage: Vec<CompactContextLineageEdgeV1>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AuthorizedSessionExpandResult {
    pub(crate) expansion: LcmExpandResponse,
    pub(crate) temporal: CompatibilityTemporalMetadata,
    pub(crate) grain: RetrievalGrainV1,
    pub(crate) state: HydrationStateV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum CompatibilityReadError {
    #[error("session compatibility target is locked")]
    Locked,
    #[error("session compatibility target is redacted")]
    Redacted,
    #[error("session compatibility target was deleted")]
    Deleted,
    #[error("session compatibility target was denied")]
    Denied,
    #[error("session compatibility target is unavailable")]
    Unavailable,
}

pub(crate) async fn describe_authorized(
    db: &GlobalDb,
    request: AuthorizedSessionDescribeRequest,
) -> Result<AuthorizedSessionDescribeResult, CompatibilityReadError> {
    request.validate()?;
    let anchors = match &request.target {
        LcmDescribeTarget::Session => {
            session_anchors(db, &request.provider, &request.session_id).await
        }
        LcmDescribeTarget::SummaryNode { node_id } => {
            summary_anchor(db, &request.provider, &request.session_id, node_id)
                .await
                .map(|anchor| vec![anchor])
        }
        LcmDescribeTarget::ExternalPayload { payload_ref } => {
            external_anchor(db, &request.provider, &request.session_id, payload_ref)
                .await
                .map(|anchor| vec![anchor])
        }
    }
    .ok_or(CompatibilityReadError::Deleted)?;
    let state = match anchors.first() {
        Some(anchor) => anchor_state(db, anchor)
            .await
            .unwrap_or(HydrationStateV1::RetainedButUnavailable),
        None => HydrationStateV1::Available,
    };
    let temporal = temporal_metadata(db, &request.session_id, anchors, state)
        .await
        .ok_or(CompatibilityReadError::Unavailable)?;
    let lineage = match &request.target {
        LcmDescribeTarget::SummaryNode { node_id } => {
            let summary_anchor = temporal
                .anchors
                .first()
                .ok_or(CompatibilityReadError::Deleted)?;
            summary_lineage(db, &request.session_id, node_id, summary_anchor)
                .await
                .ok_or(CompatibilityReadError::Unavailable)?
        }
        LcmDescribeTarget::Session | LcmDescribeTarget::ExternalPayload { .. } => Vec::new(),
    };
    let mut description = db
        .lcm_describe(LcmDescribeRequest {
            provider: request.provider,
            session_id: request.session_id,
            target: request.target,
        })
        .await
        .map_err(map_describe_error)?;
    for raw in &mut description.raw_messages {
        raw.content_preview.clear();
        raw.content_range.returned_chars = 0;
    }
    for summary in &mut description.summary_nodes {
        summary.summary_preview.clear();
    }
    if let Some(external) = description.external_payload.as_mut() {
        external.content_preview.clear();
    }
    Ok(AuthorizedSessionDescribeResult {
        description,
        temporal,
        grain: request.grain,
        state,
        lineage,
    })
}

pub(crate) async fn expand_authorized(
    db: &GlobalDb,
    request: AuthorizedSessionExpandRequest,
) -> Result<AuthorizedSessionExpandResult, CompatibilityReadError> {
    let target = request.target.clone();
    let authority = match &request.target {
        LcmExpandTarget::RawMessage { store_id } => {
            occurrence_anchor(db, &request.provider, *store_id).await
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            summary_anchor(db, &request.provider, &request.session_id, node_id)
                .await
                .map(|anchor| (request.session_id.clone(), anchor))
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            external_anchor(db, &request.provider, &request.session_id, payload_ref)
                .await
                .map(|anchor| (request.session_id.clone(), anchor))
        }
    }
    .ok_or(CompatibilityReadError::Deleted)?;
    let (owner_session, anchor) = authority;
    let state = anchor_state(db, &anchor)
        .await
        .ok_or(CompatibilityReadError::Unavailable)?;
    if state != HydrationStateV1::Available {
        return Err(map_hydration_state(state));
    }
    let canonical_content = hydrate_anchor_content(
        db,
        &request.session_id,
        &request.provider,
        request.grain,
        &request.authorization_digest,
        &anchor,
    )
    .await?;
    let mut expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: request.provider,
            session_id: request.session_id,
            target: request.target,
            content_slice: Some(request.content_slice),
            source_offset: request.source_offset,
            source_limit: request.source_limit,
        })
        .await
        .map_err(map_expand_error)?;
    apply_canonical_content(
        &mut expansion,
        &target,
        &canonical_content,
        request.content_slice,
    )?;
    if anchor_state(db, &anchor).await != Some(HydrationStateV1::Available) {
        return Err(CompatibilityReadError::Denied);
    }
    let temporal = temporal_metadata(
        db,
        &owner_session,
        vec![anchor],
        HydrationStateV1::Available,
    )
    .await
    .ok_or(CompatibilityReadError::Unavailable)?;
    Ok(AuthorizedSessionExpandResult {
        expansion,
        temporal,
        grain: request.grain,
        state: HydrationStateV1::Available,
    })
}

async fn hydrate_anchor_content(
    db: &GlobalDb,
    request_session_id: &str,
    provider: &str,
    grain: RetrievalGrainV1,
    authorization_digest: &str,
    anchor_id: &RetrievalAnchorId,
) -> Result<String, CompatibilityReadError> {
    let read = db
        .read_snapshot()
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    let frozen = load_frozen(&read, request_session_id)
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    let mut anchor_rows = read
        .query(
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1 LIMIT 2",
            [anchor_id.as_str()],
        )
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    let anchor_json: String = anchor_rows
        .next()
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?
        .ok_or(CompatibilityReadError::Denied)?
        .get(0)
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    if anchor_rows
        .next()
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?
        .is_some()
    {
        return Err(CompatibilityReadError::Denied);
    }
    let anchor: RetrievalAnchorRecord =
        serde_json::from_str(&anchor_json).map_err(|_| CompatibilityReadError::Denied)?;
    anchor
        .validate()
        .map_err(|_| CompatibilityReadError::Denied)?;
    let snapshot_request = TemporalSnapshotRequest::new(
        SessionId::new(request_session_id.to_string())
            .map_err(|_| CompatibilityReadError::Denied)?,
        authorization_digest,
        authorization_digest,
        anchor.authorization().access_policy_digest.as_str(),
        TemporalModeV1::Current,
        grain,
    )
    .map_err(|_| CompatibilityReadError::Denied)?
    .with_provider_scope(Some(provider.to_string()))
    .map_err(|_| CompatibilityReadError::Denied)?;
    let snapshot = TemporalExecutionSnapshot::new_authorized(
        snapshot_request,
        TemporalWatermarks {
            generation: frozen.active_generation,
            source: frozen.source_frontier,
            projection: frozen.projection_frontier,
            index: frozen.projection_frontier,
            summary: frozen.summary_frontier,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration_digest", authorization_digest)
                .map_err(|_| CompatibilityReadError::Denied)?,
        },
        frozen.cursor_key,
        ValidatedAuthorization::Authorized,
    )
    .map_err(|_| CompatibilityReadError::Denied)?;
    let bytes = hydrate_authorized_anchor_bytes(&read, &db.storage_root, &snapshot, anchor_id)
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| CompatibilityReadError::Unavailable)
}

fn apply_canonical_content(
    expansion: &mut LcmExpandResponse,
    target: &LcmExpandTarget,
    canonical: &str,
    slice: crate::sessions::lcm::LcmContentSlice,
) -> Result<(), CompatibilityReadError> {
    if matches!(target, LcmExpandTarget::RawMessage { .. })
        && expansion
            .raw_message
            .as_ref()
            .is_some_and(|message| message.storage_kind == LcmStorageKind::External)
    {
        return Ok(());
    }
    let total_chars = canonical.chars().count();
    let offset = slice.offset.min(total_chars);
    let content = canonical
        .chars()
        .skip(offset)
        .take(slice.limit)
        .collect::<String>();
    let returned_chars =
        u64::try_from(content.chars().count()).map_err(|_| CompatibilityReadError::Unavailable)?;
    let total_chars =
        u64::try_from(total_chars).map_err(|_| CompatibilityReadError::Unavailable)?;
    expansion.content.clone_from(&content);
    expansion.content_range.offset =
        u64::try_from(offset).map_err(|_| CompatibilityReadError::Unavailable)?;
    expansion.content_range.limit =
        u64::try_from(slice.limit).map_err(|_| CompatibilityReadError::Unavailable)?;
    expansion.content_range.returned_chars = returned_chars;
    expansion.content_range.total_chars = total_chars;
    expansion.content_range.truncated = expansion.content_range.offset > 0
        || expansion
            .content_range
            .offset
            .saturating_add(returned_chars)
            < total_chars;
    if let Some(raw) = expansion.raw_message.as_mut() {
        raw.content.clone_from(&content);
    }
    if let Some(summary) = expansion.summary_node.as_mut() {
        summary.summary_text = content;
    }
    Ok(())
}

async fn temporal_metadata(
    db: &GlobalDb,
    session_id: &str,
    anchors: Vec<RetrievalAnchorId>,
    state: HydrationStateV1,
) -> Option<CompatibilityTemporalMetadata> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT generation, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'
             ORDER BY generation DESC
             LIMIT 2",
            [session_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let generation = u64::try_from(row.get::<i64>(0).ok()?).ok()?;
    let encoded = row.get::<String>(1).ok()?;
    if rows.next().await.ok()?.is_some() {
        return None;
    }
    let frozen: FrozenWatermarks = serde_json::from_str(&encoded).ok()?;
    if frozen.active_generation != generation {
        return None;
    }
    let visible = u64::try_from(anchors.len())
        .ok()
        .filter(|_| state == HydrationStateV1::Available)
        .unwrap_or(0);
    let redacted = u64::try_from(anchors.len())
        .ok()
        .filter(|_| state == HydrationStateV1::Redacted)
        .unwrap_or(0);
    let hidden = u64::try_from(anchors.len())
        .ok()
        .filter(|_| {
            matches!(
                state,
                HydrationStateV1::Unauthorized
                    | HydrationStateV1::Locked
                    | HydrationStateV1::RetentionExpired
            )
        })
        .unwrap_or(0);
    let unknown = u64::try_from(anchors.len())
        .ok()
        .filter(|_| {
            matches!(
                state,
                HydrationStateV1::RetainedButUnavailable
                    | HydrationStateV1::Deleted
                    | HydrationStateV1::UnverifiableLegacy
            )
        })
        .unwrap_or(0);
    Some(CompatibilityTemporalMetadata {
        anchors,
        watermarks: CompatibilityWatermarks {
            generation,
            source: frozen.source_frontier,
            projection: frozen.projection_frontier,
            index: frozen.projection_frontier,
            summary: frozen.summary_frontier,
        },
        coverage: TemporalCoverageCountsV1 {
            visible,
            hidden,
            unknown,
            redacted,
        },
    })
}

async fn anchor_state(db: &GlobalDb, anchor: &RetrievalAnchorId) -> Option<HydrationStateV1> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT anchor_json, owner_json
             FROM retrieval_anchors
             WHERE anchor_id = ?1
             LIMIT 2",
            [anchor.as_str()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let anchor_json = row.get::<String>(0).ok()?;
    let owner_json = row.get::<String>(1).ok()?;
    if rows.next().await.ok()?.is_some() {
        return Some(HydrationStateV1::RetainedButUnavailable);
    }
    let anchor_value: serde_json::Value = serde_json::from_str(&anchor_json).ok()?;
    let owner_value: serde_json::Value = serde_json::from_str(&owner_json).ok()?;
    if anchor_value["anchor_id"].as_str() != Some(anchor.as_str())
        || anchor_value["owner"] != owner_value
    {
        return Some(HydrationStateV1::UnverifiableLegacy);
    }
    Some(match anchor_value["payload_access"].as_str()? {
        "eligible" => HydrationStateV1::Available,
        "redacted" => HydrationStateV1::Redacted,
        "quarantined" => HydrationStateV1::Locked,
        "retention_expired" => HydrationStateV1::RetentionExpired,
        "deleted" => HydrationStateV1::Deleted,
        "unavailable" | "ambiguous" => HydrationStateV1::RetainedButUnavailable,
        _ => HydrationStateV1::UnverifiableLegacy,
    })
}

async fn session_anchors(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
) -> Option<Vec<RetrievalAnchorId>> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT anchor_id FROM (
                SELECT occurrence.retrieval_anchor_id AS anchor_id
                FROM session_temporal_generations generation
                JOIN session_occurrences occurrence
                  ON occurrence.session_id = generation.session_id
                 AND occurrence.generation = generation.generation
                JOIN observations observation
                  ON observation.observation_id = occurrence.source_observation_id
                WHERE generation.session_id = ?1
                  AND generation.state = 'active'
                  AND json_extract(
                      observation.observation_json,
                      '$.identity.source.provider'
                  ) = ?2
                UNION
                SELECT summary.summary_anchor_id AS anchor_id
                FROM session_summary_nodes summary
                WHERE summary.session_id = ?1
                  AND json_extract(summary.publication_json, '$.provider') = ?2
             )
             ORDER BY anchor_id
             LIMIT 257",
            libsql::params![session_id, provider],
        )
        .await
        .ok()?;
    let mut anchors = Vec::new();
    while let Some(row) = rows.next().await.ok()? {
        anchors.push(RetrievalAnchorId::new(row.get::<String>(0).ok()?).ok()?);
    }
    (anchors.len() <= 256).then_some(anchors)
}

async fn summary_anchor(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    summary_id: &str,
) -> Option<RetrievalAnchorId> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT summary_anchor_id
             FROM session_summary_nodes
             WHERE session_id = ?1
               AND summary_id = ?2
               AND json_extract(publication_json, '$.provider') = ?3
             LIMIT 2",
            libsql::params![session_id, summary_id, provider],
        )
        .await
        .ok()?;
    let anchor = RetrievalAnchorId::new(rows.next().await.ok()??.get::<String>(0).ok()?).ok()?;
    if rows.next().await.ok()?.is_some() {
        return None;
    }
    Some(anchor)
}

async fn summary_lineage(
    db: &GlobalDb,
    session_id: &str,
    summary_id: &str,
    summary_anchor: &RetrievalAnchorId,
) -> Option<Vec<CompactContextLineageEdgeV1>> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT COALESCE(source.source_anchor_id, nested.summary_anchor_id),
                    summary.created_at
             FROM session_summary_sources source
             JOIN session_summary_nodes summary
               ON summary.summary_id = source.summary_id
              AND summary.session_id = ?1
             LEFT JOIN session_summary_nodes nested
               ON nested.summary_id = source.source_summary_id
              AND nested.session_id = summary.session_id
             WHERE source.summary_id = ?2
             ORDER BY source.source_ordinal
             LIMIT 257",
            libsql::params![session_id, summary_id],
        )
        .await
        .ok()?;
    let mut lineage = Vec::new();
    while let Some(row) = rows.next().await.ok()? {
        lineage.push(CompactContextLineageEdgeV1 {
            kind: TemporalAssertionKindV1::Supports,
            subject_anchor_id: summary_anchor.clone(),
            object_anchor_id: RetrievalAnchorId::new(row.get::<String>(0).ok()?).ok()?,
            knowledge_at: UtcMicros(row.get::<i64>(1).ok()?),
            authority: SessionAuthorityClassV1::ImmutableSummary,
            authorized: true,
            supporting_anchor_ids: Default::default(),
        });
    }
    (lineage.len() <= 256).then_some(lineage)
}

async fn occurrence_anchor(
    db: &GlobalDb,
    provider: &str,
    store_id: i64,
) -> Option<(String, RetrievalAnchorId)> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT raw.session_id, occurrence.retrieval_anchor_id
             FROM lcm_raw_messages raw
             JOIN session_temporal_generations generation
               ON generation.session_id = raw.session_id
              AND generation.state = 'active'
             JOIN session_occurrences occurrence
               ON occurrence.session_id = raw.session_id
              AND occurrence.generation = generation.generation
              AND occurrence.message_id = raw.message_id
             JOIN observations observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE raw.provider = ?1
               AND raw.store_id = ?2
               AND json_extract(
                   observation.observation_json,
                   '$.identity.source.provider'
               ) = ?1
             ORDER BY occurrence.occurrence_id
             LIMIT 2",
            libsql::params![provider, store_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let owner_session = row.get::<String>(0).ok()?;
    let anchor = RetrievalAnchorId::new(row.get::<String>(1).ok()?).ok()?;
    if rows.next().await.ok()?.is_some() {
        return None;
    }
    Some((owner_session, anchor))
}

async fn external_anchor(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
) -> Option<RetrievalAnchorId> {
    let read = db.read_snapshot().await.ok()?;
    let mut rows = read
        .query(
            "SELECT occurrence.retrieval_anchor_id
             FROM lcm_raw_messages raw
             JOIN session_temporal_generations generation
               ON generation.session_id = raw.session_id
              AND generation.state = 'active'
             JOIN session_occurrences occurrence
               ON occurrence.session_id = raw.session_id
              AND occurrence.generation = generation.generation
              AND occurrence.message_id = raw.message_id
             JOIN observations observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE raw.provider = ?1
               AND raw.session_id = ?2
               AND raw.payload_ref = ?3
               AND json_extract(
                   observation.observation_json,
                   '$.identity.source.provider'
               ) = ?1
             ORDER BY occurrence.occurrence_id
             LIMIT 2",
            libsql::params![provider, session_id, payload_ref],
        )
        .await
        .ok()?;
    let anchor = RetrievalAnchorId::new(rows.next().await.ok()??.get::<String>(0).ok()?).ok()?;
    if rows.next().await.ok()?.is_some() {
        return None;
    }
    Some(anchor)
}

fn map_hydration_state(state: HydrationStateV1) -> CompatibilityReadError {
    match state {
        HydrationStateV1::Redacted => CompatibilityReadError::Redacted,
        HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
            CompatibilityReadError::Deleted
        }
        HydrationStateV1::Locked => CompatibilityReadError::Locked,
        HydrationStateV1::Unauthorized => CompatibilityReadError::Denied,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => CompatibilityReadError::Unavailable,
    }
}

fn map_describe_error(error: LcmError) -> CompatibilityReadError {
    match error {
        LcmError::SummaryNodeNotFound
        | LcmError::PayloadNotFound
        | LcmError::PayloadMissing
        | LcmError::PayloadGcd => CompatibilityReadError::Deleted,
        LcmError::PayloadNotOwnedBySession | LcmError::SummarySourceNotOwnedBySession => {
            CompatibilityReadError::Denied
        }
        _ => CompatibilityReadError::Unavailable,
    }
}

fn map_expand_error(error: LcmError) -> CompatibilityReadError {
    map_describe_error(error)
}

async fn load_frozen(
    read: &GlobalDbReadSnapshot,
    session_id: &str,
) -> Result<FrozenWatermarks, CompatibilityReadError> {
    let mut rows = read
        .query(
            "SELECT generation, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'
             ORDER BY generation DESC
             LIMIT 2",
            [session_id],
        )
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?
        .ok_or(CompatibilityReadError::Unavailable)?;
    let generation = u64::try_from(
        row.get::<i64>(0)
            .map_err(|_| CompatibilityReadError::Unavailable)?,
    )
    .map_err(|_| CompatibilityReadError::Unavailable)?;
    let encoded: String = row
        .get(1)
        .map_err(|_| CompatibilityReadError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| CompatibilityReadError::Unavailable)?
        .is_some()
    {
        return Err(CompatibilityReadError::Unavailable);
    }
    let frozen: FrozenWatermarks =
        serde_json::from_str(&encoded).map_err(|_| CompatibilityReadError::Unavailable)?;
    if frozen.active_generation != generation {
        return Err(CompatibilityReadError::Unavailable);
    }
    Ok(frozen)
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
