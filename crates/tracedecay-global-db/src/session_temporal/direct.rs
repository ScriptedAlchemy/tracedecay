use tracedecay_domain::{RetrievalAnchorId, SessionId};
use tracedecay_runtime_core::db::engine::params;

use super::execution::SessionTemporalExecutionError;
use tracedecay_sessions::lcm::contracts::{LcmDescribeTarget, LcmExpandTarget};

use super::sql::TemporalSqlRead;

#[derive(Clone, Debug)]
pub struct ResolvedDirectAnchor {
    pub anchor_id: RetrievalAnchorId,
    pub owner_session_id: SessionId,
}

pub async fn resolve_describe_target(
    read: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &SessionId,
    target: &LcmDescribeTarget,
) -> Result<Option<ResolvedDirectAnchor>, SessionTemporalExecutionError> {
    match target {
        LcmDescribeTarget::Session => Ok(None),
        LcmDescribeTarget::SummaryNode { node_id } => {
            resolve_summary_anchor(read, provider, session_id, node_id)
                .await
                .map(Some)
        }
        LcmDescribeTarget::ExternalPayload { payload_ref } => {
            resolve_external_anchor(read, provider, session_id, payload_ref)
                .await
                .map(Some)
        }
    }
}

pub async fn resolve_expand_target(
    read: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &SessionId,
    target: &LcmExpandTarget,
) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
    match target {
        LcmExpandTarget::RawMessage { store_id } => {
            resolve_occurrence_anchor(read, provider, *store_id).await
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            resolve_summary_anchor(read, provider, session_id, node_id).await
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            resolve_external_anchor(read, provider, session_id, payload_ref).await
        }
    }
}

/// `ObservationSourceIdentityV1` omits `provider` on the wire whenever it is the
/// default (`claude`), so every SQL comparison against `observation_json` must
/// restore that default before matching — exactly as the candidate, hydration,
/// and derived-evidence queries do. Matching the bare `json_extract` misses
/// every Claude observation, which reports a live message as deleted.
async fn resolve_occurrence_anchor(
    read: &TemporalSqlRead<'_>,
    provider: &str,
    store_id: i64,
) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
    let mut rows = read
        .query(
            "SELECT raw.session_id, occurrence.retrieval_anchor_id
             FROM lcm_raw_messages AS raw
             JOIN session_temporal_generations AS generation
               ON generation.session_id = raw.session_id
              AND generation.state = 'active'
             JOIN session_occurrences AS occurrence
               ON occurrence.session_id = raw.session_id
              AND occurrence.generation = generation.generation
              AND occurrence.message_id = raw.message_id
             JOIN observations AS observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE raw.provider = ?1
               AND raw.store_id = ?2
               AND COALESCE(
                   json_extract(
                       observation.observation_json,
                       '$.identity.source.provider'
                   ),
                   'claude'
               ) = ?1
             ORDER BY occurrence.occurrence_id
             LIMIT 2",
            params![provider, store_id],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .ok_or(SessionTemporalExecutionError::Deleted)?;
    let owner_session_id = SessionId::new(
        row.get::<String>(0)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
    )
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let anchor_id = RetrievalAnchorId::new(
        row.get::<String>(1)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
    )
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .is_some()
    {
        return Err(SessionTemporalExecutionError::Unavailable);
    }
    Ok(ResolvedDirectAnchor {
        anchor_id,
        owner_session_id,
    })
}

async fn resolve_summary_anchor(
    read: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &SessionId,
    summary_id: &str,
) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
    let mut rows = read
        .query(
            "SELECT summary_anchor_id
             FROM session_summary_nodes
             WHERE session_id = ?1
               AND summary_id = ?2
               AND json_extract(publication_json, '$.provider') = ?3
             LIMIT 2",
            params![session_id.as_str(), summary_id, provider],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .ok_or(SessionTemporalExecutionError::Deleted)?;
    let anchor_id = RetrievalAnchorId::new(
        row.get::<String>(0)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
    )
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .is_some()
    {
        return Err(SessionTemporalExecutionError::Unavailable);
    }
    Ok(ResolvedDirectAnchor {
        anchor_id,
        owner_session_id: session_id.clone(),
    })
}

async fn resolve_external_anchor(
    read: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &SessionId,
    payload_ref: &str,
) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
    let mut rows = read
        .query(
            "SELECT occurrence.retrieval_anchor_id
             FROM lcm_raw_messages AS raw
             JOIN session_temporal_generations AS generation
               ON generation.session_id = raw.session_id
              AND generation.state = 'active'
             JOIN session_occurrences AS occurrence
               ON occurrence.session_id = raw.session_id
              AND occurrence.generation = generation.generation
              AND occurrence.message_id = raw.message_id
             JOIN observations AS observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE raw.provider = ?1
               AND raw.session_id = ?2
               AND raw.payload_ref = ?3
               AND COALESCE(
                   json_extract(
                       observation.observation_json,
                       '$.identity.source.provider'
                   ),
                   'claude'
               ) = ?1
             ORDER BY occurrence.occurrence_id
             LIMIT 2",
            params![provider, session_id.as_str(), payload_ref],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .ok_or(SessionTemporalExecutionError::Deleted)?;
    let anchor_id = RetrievalAnchorId::new(
        row.get::<String>(0)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
    )
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .is_some()
    {
        return Err(SessionTemporalExecutionError::Unavailable);
    }
    Ok(ResolvedDirectAnchor {
        anchor_id,
        owner_session_id: session_id.clone(),
    })
}
