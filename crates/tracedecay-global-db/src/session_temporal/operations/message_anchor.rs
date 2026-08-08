//! Canonical retrieval-anchor resolution for one raw-message summary source.
//!
//! A published summary's source lineage must name the same retrieval anchor the
//! temporal projection binds to that message. Anything else is a second anchor
//! identity space: every generation-bound read resolves such a source to no
//! occurrence, reports it missing, and drops the whole summary from the page.
//!
//! The temporal occurrence is generation-bound, so it only exists once a refresh
//! has materialized the message. A summary published while a refresh is still
//! pending must therefore resolve through the durable observation authority
//! instead — the exact-observation anchor identity is retained when the
//! observation is persisted and does not change when the refresh later
//! materializes the occurrence, so both routes agree on the anchor.

use tracedecay_domain::{
    AnchorDurabilityClass, DurableObservationV1, ObservationScopeV1, PayloadAccessState, ProjectId,
    RetrievalAnchorRecord, derive_exact_observation_anchor_id,
};
use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_sessions::runtime::lcm::types::LcmError;
use tracedecay_store::derive_canonical_projection;

use super::sources::unavailable;

/// Resolved canonical source binding: anchor id, whether the publication still
/// has to write a compatibility anchor row, and the source's knowledge time.
pub(super) type ResolvedMessageAnchor = (String, bool, i64);

/// Resolves the canonical retrieval anchor for one raw LCM message.
///
/// `Ok(None)` means the message has no canonical anchor in this store at all —
/// the only case in which the publication falls back to a legacy compatibility
/// anchor.
pub(super) async fn resolve_message_anchor(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    if let Some(resolved) =
        resolve_materialized_occurrence(conn, provider, session_id, message_id, now).await?
    {
        return Ok(Some(resolved));
    }
    resolve_canonical_observation(conn, provider, session_id, message_id, now).await
}

/// Resolves through the message's occurrence in the active temporal generation.
async fn resolve_materialized_occurrence(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    let Some(generation) = super::generation::active_generation(conn, session_id).await? else {
        return Ok(None);
    };
    let mut rows = conn
        .query(
            "SELECT DISTINCT json_object(
                    'anchor_id', occurrence.retrieval_anchor_id,
                    'anchor_json', anchor.anchor_json,
                    'owner_json', anchor.owner_json,
                    'knowledge_at', occurrence.knowledge_at,
                    'observation_json', observation.observation_json,
                    'receipt_id', observation.receipt_id
                )
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
    let encoded = row.get::<String>(0)?;
    let retained: serde_json::Value =
        serde_json::from_str(&encoded).map_err(|error| LcmError::Db(error.to_string()))?;
    let string = |field: &str| {
        retained[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LcmError::Db(format!("retained source {field} is unavailable")))
    };
    let anchor_id = string("anchor_id")?;
    let anchor_json = string("anchor_json")?;
    let owner_json = string("owner_json")?;
    let knowledge_at = retained["knowledge_at"]
        .as_i64()
        .ok_or_else(|| LcmError::Db("retained source knowledge_at is unavailable".to_string()))?;
    if rows.next().await?.is_some() {
        return Err(LcmError::SummarySourceUnavailable {
            source_id: message_id.to_string(),
            reason: "ambiguous_anchor".to_string(),
        });
    }
    let anchor: RetrievalAnchorRecord = serde_json::from_str(&anchor_json)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_anchor"))?;
    let observation_raw = string("observation_json")?;
    let observation: DurableObservationV1 = serde_json::from_str(&observation_raw)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_observation"))?;
    let expected_scope = publishing_scope(conn, provider, session_id).await?;
    require_session_owned_observation(
        &observation,
        &anchor,
        &owner_json,
        &string("receipt_id")?,
        provider,
        session_id,
        &expected_scope,
    )?;
    require_readable_anchor(&anchor, &anchor_id, now)?;
    Ok(Some((anchor_id, false, knowledge_at)))
}

/// Resolves through the durable observation authority, which retains the
/// exact-observation anchor before any generation materializes the occurrence.
async fn resolve_canonical_observation(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    let expected_scope = publishing_scope(conn, provider, session_id).await?;
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, observation.receipt_id,
                    anchor.anchor_json, anchor.owner_json
             FROM session_temporal_observation_effects AS effect
             JOIN observations AS observation
               ON observation.observation_id = effect.observation_id
             JOIN observation_retrieval_anchors AS link
               ON link.observation_id = observation.observation_id
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = link.anchor_id
             WHERE effect.session_id = ?1
               AND effect.output_count > 0
               AND json_extract(
                       observation.observation_json, '$.identity.source.provider'
                   ) = ?2
               AND json_extract(
                       observation.observation_json, '$.identity.source.session_id'
                   ) = ?1
               AND COALESCE(
                       json_extract(
                           observation.observation_json, '$.payload.relations.message_id'
                       ),
                       json_extract(
                           observation.observation_json, '$.payload.stable_record_id'
                       )
                   ) = ?3
             ORDER BY effect.observation_sequence, link.anchor_id",
            params![session_id, provider, message_id],
        )
        .await?;
    let mut resolved: Option<ResolvedMessageAnchor> = None;
    while let Some(row) = rows.next().await? {
        let observation_raw = row.get::<String>(0)?;
        let receipt_id = row.get::<String>(1)?;
        let anchor_json = row.get::<String>(2)?;
        let owner_json = row.get::<String>(3)?;
        // Every step up to identification decides whether this row IS the
        // message's canonical anchor: a row that does not verify simply is not
        // that anchor and leaves the legacy binding intact. Payload access and
        // retention are decided once the anchor is identified and stay typed
        // source-unavailable states.
        let Ok(observation) = serde_json::from_str::<DurableObservationV1>(&observation_raw) else {
            continue;
        };
        // The envelope's own message identity is only a prefilter: the
        // projection reducer decides which message ids an observation produces.
        if !projects_message(&observation, message_id) {
            continue;
        }
        let Ok(anchor) = serde_json::from_str::<RetrievalAnchorRecord>(&anchor_json) else {
            continue;
        };
        if require_session_owned_observation(
            &observation,
            &anchor,
            &owner_json,
            &receipt_id,
            provider,
            session_id,
            &expected_scope,
        )
        .is_err()
            || require_exact_observation_anchor(&observation, &anchor).is_err()
        {
            continue;
        }
        let anchor_id = anchor.anchor_id().as_str().to_owned();
        require_readable_anchor(&anchor, &anchor_id, now)?;
        let candidate = (anchor_id, false, anchor.ingested_at().0);
        match &resolved {
            Some(existing) if existing.0 != candidate.0 => {
                return Err(LcmError::SummarySourceUnavailable {
                    source_id: message_id.to_string(),
                    reason: "ambiguous_anchor".to_string(),
                });
            }
            Some(_) => {}
            None => resolved = Some(candidate),
        }
    }
    Ok(resolved)
}

fn projects_message(observation: &DurableObservationV1, message_id: &str) -> bool {
    derive_canonical_projection(observation).is_ok_and(|projection| {
        projection
            .messages()
            .any(|output| output.message().message_id == message_id)
    })
}

fn require_session_owned_observation(
    observation: &DurableObservationV1,
    anchor: &RetrievalAnchorRecord,
    owner_json: &str,
    retained_receipt_id: &str,
    provider: &str,
    session_id: &str,
    expected_scope: &ObservationScopeV1,
) -> Result<(), LcmError> {
    if observation.source().provider().as_str() != provider
        || observation.source().session_id().as_str() != session_id
        || observation.scope() != expected_scope
        || anchor.owner() != observation.scope()
        || serde_json::to_string(anchor.owner()).ok().as_deref() != Some(owner_json)
        || retained_receipt_id != observation.receipt().receipt().receipt_id().as_str()
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(())
}

/// The observation route finds the anchor by derivation, so the retained row has
/// to be exactly the canonical exact-observation anchor for that observation.
fn require_exact_observation_anchor(
    observation: &DurableObservationV1,
    anchor: &RetrievalAnchorRecord,
) -> Result<(), LcmError> {
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .map_err(|error| LcmError::Db(error.to_string()))?;
    if anchor.anchor_id() != &expected_anchor
        || !anchor
            .source_observations()
            .contains(observation.observation_id())
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(())
}

fn require_readable_anchor(
    anchor: &RetrievalAnchorRecord,
    anchor_id: &str,
    now: i64,
) -> Result<(), LcmError> {
    match anchor.payload_access() {
        PayloadAccessState::Eligible => {}
        state => {
            return Err(unavailable(
                anchor_id,
                &format!("{state:?}").to_ascii_lowercase(),
            ));
        }
    }
    if let AnchorDurabilityClass::RetentionBound { expires_at } = anchor.durability()
        && expires_at.0 <= now
    {
        return Err(unavailable(anchor_id, "retention_expired"));
    }
    Ok(())
}

async fn publishing_scope(
    conn: &impl Executor,
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
