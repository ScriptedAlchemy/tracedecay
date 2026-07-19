use std::cmp;

use libsql::{Connection, Row, Value as SqlValue, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracedecay_domain::{
    LogicalCopyRecordV1, RetrievalAnchorId, SessionEvidenceMetadataV1, SessionId,
    SessionSummaryIdV1, SessionSummaryRecordV1, SignedCursorKeyRefV1, SummaryPublicationMetadataV1,
    SummarySourceHorizonV1, TemporalModeV1, TemporalValidityV1, UtcMicros,
};

use crate::global_db::GlobalDbReadSnapshot;
use crate::query::temporal::candidates::{CandidateChannel, CandidateClause, CandidatePlan};
use crate::query::temporal::ports::{
    CandidatePageSink, MeasuredTemporalValue, PageKey, PageRequest, PageStatus, PortFuture,
    SummarySourceRecord, TemporalExecutionSnapshot, TemporalPortError, TemporalReadPort,
    TemporalRecord, TemporalRecordPageSink, TemporalRetrievalScope,
};
use crate::query::temporal::ranking::RankingCandidate;
use crate::query::temporal::resolution::{
    ResolutionAssertion, ResolutionEvidence, ResolutionOccurrence, SummarySourceState,
    ValidatedAuthorization,
};
use crate::timeutil::parse_rfc3339_timestamp;

const CANDIDATE_OPERATION: &str = "read temporal candidates";
const RECORD_OPERATION: &str = "read temporal records";
const SNAPSHOT_OPERATION: &str = "validate temporal read snapshot";
const MIN_CURSOR_CAPACITY: usize = 96;
const MAX_SUMMARY_SOURCES_PER_RECORD: usize = 256;
const EXACT_CANDIDATE_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           COALESCE(json_extract(
               provider_observation.observation_json, '$.identity.source.provider'
           ), 'claude')
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?3)
      AND session_occurrences_fts MATCH ?4
      AND o.snippet_text = ?5
      AND (o.knowledge_at < ?6 OR (o.knowledge_at = ?6 AND o.occurrence_id > ?7))
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?8
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?10
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?10
      AND length(CAST(o.session_id AS BLOB)) <= ?10
      AND length(CAST(o.role AS BLOB)) <= ?10
      AND length(CAST(COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') AS BLOB)) <= ?10
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(COALESCE(json_extract(
              provider_observation.observation_json, '$.identity.source.provider'
          ), 'claude') AS BLOB)) <= ?11
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?12";
const SCOPE_CANDIDATE_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           COALESCE(json_extract(
               provider_observation.observation_json, '$.identity.source.provider'
           ), 'claude')
    FROM session_occurrences AS o
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?3)
      AND (o.knowledge_at < ?4 OR (o.knowledge_at = ?4 AND o.occurrence_id > ?5))
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?6
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?7
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?8
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?8
      AND length(CAST(o.session_id AS BLOB)) <= ?8
      AND length(CAST(o.role AS BLOB)) <= ?8
      AND length(CAST(COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') AS BLOB)) <= ?8
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(COALESCE(json_extract(
              provider_observation.observation_json, '$.identity.source.provider'
          ), 'claude') AS BLOB)) <= ?9
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?10";
const OCCURRENCE_FTS_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           COALESCE(json_extract(
               provider_observation.observation_json, '$.identity.source.provider'
           ), 'claude')
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?3)
      AND session_occurrences_fts MATCH ?4
      AND (o.knowledge_at < ?5 OR (o.knowledge_at = ?5 AND o.occurrence_id > ?6))
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?7
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?8
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?9
      AND length(CAST(o.session_id AS BLOB)) <= ?9
      AND length(CAST(o.role AS BLOB)) <= ?9
      AND length(CAST(COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') AS BLOB)) <= ?9
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(COALESCE(json_extract(
              provider_observation.observation_json, '$.identity.source.provider'
          ), 'claude') AS BLOB)) <= ?10
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?11";
const TIME_CANDIDATE_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           COALESCE(json_extract(
               provider_observation.observation_json, '$.identity.source.provider'
           ), 'claude')
    FROM session_occurrences AS o INDEXED BY idx_session_occurrences_generation_order
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?3)
      AND o.knowledge_at >= ?4 AND o.knowledge_at < ?5
      AND (o.knowledge_at < ?6 OR (o.knowledge_at = ?6 AND o.occurrence_id > ?7))
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?8
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?10
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?10
      AND length(CAST(o.session_id AS BLOB)) <= ?10
      AND length(CAST(o.role AS BLOB)) <= ?10
      AND length(CAST(COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') AS BLOB)) <= ?10
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(COALESCE(json_extract(
              provider_observation.observation_json, '$.identity.source.provider'
          ), 'claude') AS BLOB)) <= ?11
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?12";
const SUMMARY_CANDIDATE_QUERY: &str = "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           json_extract(n.publication_json, '$.provider')
    FROM session_summary_nodes_fts
    JOIN session_summary_nodes AS n ON n.rowid = session_summary_nodes_fts.rowid
    JOIN session_summary_availability AS a
      ON a.summary_id = n.summary_id
     AND a.session_id = ?1
     AND a.generation = ?2
    WHERE n.session_id = ?1
      AND (?3 IS NULL OR EXISTS (
          WITH RECURSIVE retained_sources(source_anchor_id, source_summary_id, depth) AS (
              SELECT source_anchor_id, source_summary_id, 0
              FROM session_summary_sources
              WHERE summary_id = n.summary_id
              UNION ALL
              SELECT nested.source_anchor_id, nested.source_summary_id, retained.depth + 1
              FROM retained_sources AS retained
              JOIN session_summary_nodes AS retained_summary
                ON retained_summary.summary_id = retained.source_summary_id
               AND retained_summary.session_id = n.session_id
              JOIN session_summary_sources AS nested
                ON nested.summary_id = retained_summary.summary_id
              WHERE retained.depth < 63
              LIMIT 257
          )
          SELECT 1
          FROM retained_sources AS retained
          JOIN session_occurrences AS source_occurrence
            ON source_occurrence.retrieval_anchor_id = retained.source_anchor_id
           AND source_occurrence.session_id = n.session_id
           AND source_occurrence.generation = ?2
          JOIN observations AS source_observation
            ON source_observation.observation_id = source_occurrence.source_observation_id
          WHERE COALESCE(json_extract(
              source_observation.observation_json, '$.identity.source.provider'
          ), 'claude') = ?3
          LIMIT 1
      ))
      AND session_summary_nodes_fts MATCH ?4
      AND a.availability <> 'unavailable'
      AND (n.created_at < ?5 OR (n.created_at = ?5 AND n.summary_id > ?6))
      AND length(CAST(n.summary_id AS BLOB)) <= ?7
      AND length(CAST(n.summary_anchor_id AS BLOB)) <= ?8
      AND length(CAST(n.session_id AS BLOB)) <= ?9
      AND length(CAST(COALESCE(
          json_extract(n.publication_json, '$.provider'), ''
      ) AS BLOB)) <= ?9
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.summary_anchor_id AS BLOB))
          + length(CAST(n.session_id AS BLOB))
          + length(CAST(COALESCE(
              json_extract(n.publication_json, '$.provider'), ''
          ) AS BLOB)) <= ?10
    ORDER BY n.created_at DESC, n.summary_id
    LIMIT ?11";
const ROOT_EXACT_CANDIDATE_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    JOIN session_temporal_generations AS frozen
      ON frozen.session_id = o.session_id
     AND frozen.generation = o.generation
     AND frozen.state = 'active'
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = COALESCE(json_extract(
         provider_observation.observation_json, '$.identity.source.provider'
     ), 'claude')
     AND authority_session.project_key = ?1
    WHERE (
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
      )
      AND (?2 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?2)
      AND session_occurrences_fts MATCH ?3
      AND o.snippet_text = ?4
      AND (
          o.knowledge_at < ?5
          OR (
              o.knowledge_at = ?5
              AND (
                  o.session_id > ?6
                  OR (o.session_id = ?6 AND o.occurrence_id > ?7)
              )
          )
      )
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?8
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?10
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?10
      AND length(CAST(o.session_id AS BLOB)) <= ?10
      AND length(CAST(o.role AS BLOB)) <= ?10
      AND length(CAST(authority_session.provider AS BLOB)) <= ?10
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(authority_session.provider AS BLOB)) <= ?11
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.session_id AS BLOB)) + 9 <= ?12
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?13";
const ROOT_OCCURRENCE_FTS_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    JOIN session_temporal_generations AS frozen
      ON frozen.session_id = o.session_id
     AND frozen.generation = o.generation
     AND frozen.state = 'active'
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = COALESCE(json_extract(
         provider_observation.observation_json, '$.identity.source.provider'
     ), 'claude')
     AND authority_session.project_key = ?1
    WHERE (
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
      )
      AND (?2 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?2)
      AND session_occurrences_fts MATCH ?3
      AND (
          o.knowledge_at < ?4
          OR (
              o.knowledge_at = ?4
              AND (
                  o.session_id > ?5
                  OR (o.session_id = ?5 AND o.occurrence_id > ?6)
              )
          )
      )
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?7
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?8
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?9
      AND length(CAST(o.session_id AS BLOB)) <= ?9
      AND length(CAST(o.role AS BLOB)) <= ?9
      AND length(CAST(authority_session.provider AS BLOB)) <= ?9
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(authority_session.provider AS BLOB)) <= ?10
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.session_id AS BLOB)) + 9 <= ?11
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?12";
const ROOT_TIME_CANDIDATE_QUERY: &str = "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider
    FROM session_temporal_generations AS frozen
    JOIN session_occurrences AS o
      ON o.session_id = frozen.session_id
     AND o.generation = frozen.generation
    JOIN observations AS provider_observation
      ON provider_observation.observation_id = o.source_observation_id
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = COALESCE(json_extract(
         provider_observation.observation_json, '$.identity.source.provider'
     ), 'claude')
     AND authority_session.project_key = ?1
    WHERE frozen.state = 'active'
      AND (
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
      )
      AND (?2 IS NULL OR COALESCE(json_extract(
          provider_observation.observation_json, '$.identity.source.provider'
      ), 'claude') = ?2)
      AND o.knowledge_at >= ?3 AND o.knowledge_at < ?4
      AND (
          o.knowledge_at < ?5
          OR (
              o.knowledge_at = ?5
              AND (
                  o.session_id > ?6
                  OR (o.session_id = ?6 AND o.occurrence_id > ?7)
              )
          )
      )
      AND length(CAST(o.occurrence_id AS BLOB)) <= ?8
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?9
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ?10
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ?10
      AND length(CAST(o.session_id AS BLOB)) <= ?10
      AND length(CAST(o.role AS BLOB)) <= ?10
      AND length(CAST(authority_session.provider AS BLOB)) <= ?10
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(authority_session.provider AS BLOB)) <= ?11
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.session_id AS BLOB)) + 9 <= ?12
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?13";
const ROOT_SUMMARY_CANDIDATE_QUERY: &str = "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           authority_session.provider
    FROM session_summary_nodes_fts
    JOIN session_summary_nodes AS n ON n.rowid = session_summary_nodes_fts.rowid
    JOIN session_summary_availability AS a
      ON a.summary_id = n.summary_id
     AND a.session_id = n.session_id
    JOIN session_temporal_generations AS frozen
      ON frozen.session_id = a.session_id
     AND frozen.generation = a.generation
     AND frozen.state = 'active'
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = n.summary_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = n.session_id
     AND authority_session.provider = json_extract(n.publication_json, '$.provider')
     AND authority_session.project_key = ?1
    WHERE (
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
      )
      AND (?2 IS NULL OR EXISTS (
          WITH RECURSIVE retained_sources(source_anchor_id, source_summary_id, depth) AS (
              SELECT source_anchor_id, source_summary_id, 0
              FROM session_summary_sources
              WHERE summary_id = n.summary_id
              UNION ALL
              SELECT nested.source_anchor_id, nested.source_summary_id, retained.depth + 1
              FROM retained_sources AS retained
              JOIN session_summary_nodes AS retained_summary
                ON retained_summary.summary_id = retained.source_summary_id
               AND retained_summary.session_id = n.session_id
              JOIN session_summary_sources AS nested
                ON nested.summary_id = retained_summary.summary_id
              WHERE retained.depth < 63
              LIMIT 257
          )
          SELECT 1
          FROM retained_sources AS retained
          JOIN session_occurrences AS source_occurrence
            ON source_occurrence.retrieval_anchor_id = retained.source_anchor_id
           AND source_occurrence.session_id = n.session_id
           AND source_occurrence.generation = frozen.generation
          JOIN observations AS source_observation
            ON source_observation.observation_id = source_occurrence.source_observation_id
          WHERE COALESCE(json_extract(
              source_observation.observation_json, '$.identity.source.provider'
          ), 'claude') = ?2
          LIMIT 1
      ))
      AND session_summary_nodes_fts MATCH ?3
      AND a.availability <> 'unavailable'
      AND (
          n.created_at < ?4
          OR (
              n.created_at = ?4
              AND (
                  n.session_id > ?5
                  OR (n.session_id = ?5 AND n.summary_id > ?6)
              )
          )
      )
      AND length(CAST(n.summary_id AS BLOB)) <= ?7
      AND length(CAST(n.summary_anchor_id AS BLOB)) <= ?8
      AND length(CAST(n.session_id AS BLOB)) <= ?9
      AND length(CAST(authority_session.provider AS BLOB)) <= ?9
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.summary_anchor_id AS BLOB))
          + length(CAST(n.session_id AS BLOB))
          + length(CAST(authority_session.provider AS BLOB)) <= ?10
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.session_id AS BLOB)) + 9 <= ?11
    ORDER BY n.created_at DESC, n.session_id, n.summary_id
    LIMIT ?12";

/// Borrowed read-only adapter over one authoritative database snapshot.
pub struct GlobalDbTemporalReadPort<'a> {
    read: &'a GlobalDbReadSnapshot,
}

impl<'a> GlobalDbTemporalReadPort<'a> {
    pub const fn new(read: &'a GlobalDbReadSnapshot) -> Self {
        Self { read }
    }

    async fn validate_snapshot(
        &self,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<(), TemporalPortError> {
        let control = snapshot.request().execution_control();
        control.checkpoint()?;
        if !snapshot.has_authoritative_participant_manifest() {
            if matches!(
                snapshot.retrieval_scope(),
                TemporalRetrievalScope::AllSessionsInAuthorizedRoot
            ) {
                return Err(TemporalPortError::UnauthorizedSnapshot);
            }
            let generation = i64::try_from(snapshot.watermarks().generation)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let mut rows = self
                .read
                .query(
                    "SELECT state, frozen_watermarks_json
                     FROM session_temporal_generations
                     WHERE session_id = ?1 AND generation = ?2
                     LIMIT 2",
                    (snapshot.request().session_id().as_str(), generation),
                )
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .ok_or_else(|| read_message(SNAPSHOT_OPERATION, "frozen generation is missing"))?;
            let state: String = row
                .get(0)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let encoded: String = row
                .get(1)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            if rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .is_some()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "frozen generation is not unique",
                ));
            }
            let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let watermarks = snapshot.watermarks();
            if state != "active"
                || frozen.active_generation > watermarks.generation
                || frozen.source_frontier != watermarks.source
                || frozen.projection_frontier != watermarks.projection
                || frozen.summary_frontier != watermarks.summary
                || frozen.cursor_key.as_ref() != snapshot.cursor_key()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "snapshot does not match the active frozen generation",
                ));
            }
            return control.checkpoint();
        }
        let project_key = snapshot
            .request()
            .authorized_root()
            .ok_or(TemporalPortError::UnauthorizedSnapshot)?
            .project_key();
        for participant in snapshot.participant_manifest().entries() {
            control.checkpoint()?;
            if participant.access()
                != crate::query::temporal::ports::TemporalSourceAccess::Authorized
                || participant.configuration_digest()
                    != snapshot.versions().configuration_digest.as_str()
                || participant.authorization_digest() != snapshot.access_digest().as_str()
            {
                return Err(TemporalPortError::UnauthorizedSnapshot);
            }
            let generation = i64::try_from(participant.generation())
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let mut rows = self
                .read
                .query(
                    "SELECT generation.state, generation.frozen_watermarks_json
                     FROM session_temporal_generations AS generation
                     JOIN sessions AS source
                       ON source.session_id = generation.session_id
                      AND source.provider = ?3
                      AND source.project_key = ?4
                     WHERE generation.session_id = ?1
                       AND generation.generation = ?2
                     LIMIT 2",
                    params![
                        participant.session_id().as_str(),
                        generation,
                        participant.source_id(),
                        project_key
                    ],
                )
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .ok_or_else(|| {
                    read_message(
                        SNAPSHOT_OPERATION,
                        "frozen participant generation is missing",
                    )
                })?;
            let state: String = row
                .get(0)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let encoded: String = row
                .get(1)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            if rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .is_some()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "frozen participant generation is not unique",
                ));
            }
            let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let watermarks = participant.watermarks();
            if state != "active"
                || frozen.active_generation > watermarks.generation
                || frozen.source_frontier != watermarks.source
                || frozen.projection_frontier != watermarks.projection
                || frozen.summary_frontier != watermarks.summary
                || frozen.cursor_key.as_ref() != snapshot.cursor_key()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "snapshot does not match the active participant generation",
                ));
            }
        }
        control.checkpoint()
    }

    async fn produce_candidates(
        &self,
        scope: &TemporalRetrievalScope,
        snapshot: &TemporalExecutionSnapshot,
        plan: &CandidatePlan,
        request: &PageRequest,
        sink: &mut CandidatePageSink<'_>,
    ) -> Result<PageStatus, TemporalPortError> {
        require_snapshot_scope(scope, snapshot)?;
        let bounds = PageBounds::from_request(request)?;
        if bounds.items == 0 || bounds.bytes == 0 {
            return Ok(PageStatus::Complete);
        }
        self.validate_snapshot(snapshot).await?;
        let root_project_key = authorized_root_project_key(scope, snapshot)?;
        let mut cursor = CandidateCursor::decode(request.keyset())?;
        if cursor.clause >= plan.clauses().len() {
            return Ok(PageStatus::Complete);
        }
        let control = snapshot.request().execution_control();
        let mut page_bytes = 0usize;
        let mut clause_queries = 0usize;
        while cursor.clause < plan.clauses().len() {
            control.checkpoint()?;
            clause_queries += 1;
            if clause_queries > bounds.items.saturating_add(1) {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate clause scans",
                });
            }
            let clause = &plan.clauses()[cursor.clause];
            validate_clause(clause, request)?;
            let query_limit = bounds.items.saturating_sub(sink.len()).saturating_add(1);
            let mut rows = query_candidate_clause(
                self.read,
                scope,
                snapshot,
                clause,
                &cursor,
                query_limit,
                request,
                root_project_key.as_deref(),
            )
            .await?;
            let mut extra = false;
            let mut last_emitted = None;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
            {
                control.checkpoint()?;
                if sink.len() == bounds.items {
                    extra = true;
                    break;
                }
                let candidate = candidate_from_row(&row, clause.channel, scope)?;
                require_candidate_scope(scope, &candidate)?;
                let encoded = candidate.measured_encoded_bytes()?;
                if !fits_bytes(page_bytes, encoded, bounds, request.max_item_bytes()) {
                    if sink.is_empty() {
                        return Err(TemporalPortError::BudgetExceeded {
                            resource: "candidate bytes",
                        });
                    }
                    extra = true;
                    break;
                }
                page_bytes += encoded;
                last_emitted = Some(CandidateCursor {
                    clause: cursor.clause,
                    knowledge_at: candidate.knowledge_at_micros,
                    session_id: candidate.session.clone().unwrap_or_default(),
                    stable_id: candidate.retriever_record_id.clone(),
                });
                sink.push(candidate)?;
            }
            if extra {
                let continuation = last_emitted.unwrap_or(cursor);
                sink.set_continuation_key(continuation.encode(request.max_key_bytes())?)?;
                return Ok(PageStatus::More);
            }
            cursor = CandidateCursor {
                clause: cursor.clause + 1,
                knowledge_at: i64::MAX,
                session_id: String::new(),
                stable_id: String::new(),
            };
            if sink.len() == bounds.items {
                if cursor.clause < plan.clauses().len() {
                    sink.set_continuation_key(cursor.encode(request.max_key_bytes())?)?;
                    return Ok(PageStatus::More);
                }
                return Ok(PageStatus::Complete);
            }
        }
        Ok(PageStatus::Complete)
    }

    async fn produce_records(
        &self,
        scope: &TemporalRetrievalScope,
        snapshot: &TemporalExecutionSnapshot,
        candidates: &[RankingCandidate],
        request: &PageRequest,
        sink: &mut TemporalRecordPageSink<'_>,
    ) -> Result<PageStatus, TemporalPortError> {
        require_snapshot_scope(scope, snapshot)?;
        let bounds = PageBounds::from_request(request)?;
        if bounds.items == 0 || bounds.bytes == 0 || candidates.is_empty() {
            return Ok(PageStatus::Complete);
        }
        self.validate_snapshot(snapshot).await?;
        let root_project_key = authorized_root_project_key(scope, snapshot)?;
        let control = snapshot.request().execution_control();
        let mut cursor = RecordCursor::decode(request.keyset())?;
        if cursor.candidate >= candidates.len() {
            return Ok(PageStatus::Complete);
        }
        let mut page_bytes = 0usize;
        let window_size = bounds.items.saturating_add(1);
        let mut window_queries = 0usize;
        while cursor.candidate < candidates.len() {
            control.checkpoint()?;
            window_queries += 1;
            if window_queries > bounds.items.saturating_add(1) {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "record candidate window scans",
                });
            }
            let window_end = bounded_window_end(candidates.len(), cursor.candidate, window_size);
            let window = &candidates[cursor.candidate..window_end];
            for candidate in window {
                require_candidate_scope(scope, candidate)?;
                if let Some(project_key) = root_project_key.as_deref() {
                    require_candidate_root_authority(
                        self.read,
                        candidate,
                        project_key,
                        snapshot.provider_scope(),
                    )
                    .await?;
                }
                if candidate.anchor_id.to_string().len() > request.max_key_bytes() {
                    return Err(TemporalPortError::BudgetExceeded {
                        resource: "record candidate anchor bytes",
                    });
                }
            }
            let query_limit = bounds.items.saturating_sub(sink.len()).saturating_add(1);
            let query = build_record_query(
                scope,
                snapshot,
                window,
                cursor.candidate,
                &cursor,
                query_limit,
                request,
            )?;
            let mut rows = self
                .read
                .query(&query.sql, query.params)
                .await
                .map_err(|error| read_error(RECORD_OPERATION, error))?;
            control.checkpoint()?;
            let mut extra = false;
            let mut last_emitted = None;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| read_error(RECORD_OPERATION, error))?
            {
                control.checkpoint()?;
                let row_cursor = RecordCursor::from_row(&row)?;
                if sink.len() == bounds.items {
                    extra = true;
                    break;
                }
                let record = temporal_record_from_row(&row)?;
                let encoded = record.measured_encoded_bytes()?;
                if !fits_bytes(page_bytes, encoded, bounds, request.max_item_bytes()) {
                    if sink.is_empty() {
                        return Err(TemporalPortError::BudgetExceeded {
                            resource: "record bytes",
                        });
                    }
                    extra = true;
                    break;
                }
                page_bytes += encoded;
                last_emitted = Some(row_cursor);
                sink.push(record)?;
            }
            if extra {
                let continuation = last_emitted.unwrap_or(cursor);
                sink.set_continuation_key(continuation.encode(request.max_key_bytes())?)?;
                return Ok(PageStatus::More);
            }
            cursor = RecordCursor {
                candidate: window_end,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            };
            if sink.len() == bounds.items {
                if cursor.candidate < candidates.len() {
                    sink.set_continuation_key(cursor.encode(request.max_key_bytes())?)?;
                    return Ok(PageStatus::More);
                }
                return Ok(PageStatus::Complete);
            }
        }
        Ok(PageStatus::Complete)
    }
}

impl TemporalReadPort for GlobalDbTemporalReadPort<'_> {
    fn produce_candidate_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_candidates(snapshot.retrieval_scope(), snapshot, plan, &request, sink)
                .await
        })
    }

    fn produce_candidate_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_candidates(scope, snapshot, plan, &request, sink)
                .await
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_records(
                snapshot.retrieval_scope(),
                snapshot,
                candidates,
                &request,
                sink,
            )
            .await
        })
    }

    fn produce_temporal_record_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_records(scope, snapshot, candidates, &request, sink)
                .await
        })
    }
}

#[derive(Clone, Copy)]
struct PageBounds {
    items: usize,
    bytes: usize,
}

#[derive(Deserialize)]
struct FrozenWatermarksWire {
    active_generation: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
    projection_frontier: u64,
    source_frontier: u64,
    summary_frontier: u64,
}

impl PageBounds {
    fn from_request(request: &PageRequest) -> Result<Self, TemporalPortError> {
        if request
            .keyset()
            .is_some_and(|key| key.as_str().len() > request.max_key_bytes())
        {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key bytes",
            });
        }
        if request.max_key_bytes() < MIN_CURSOR_CAPACITY {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key capacity",
            });
        }
        Ok(Self {
            items: cmp::min(request.remaining_items(), request.page_item_limit()),
            bytes: cmp::min(
                request.remaining_total_bytes(),
                request.page_total_byte_limit(),
            ),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct CandidateCursor {
    clause: usize,
    knowledge_at: i64,
    #[serde(default)]
    session_id: String,
    stable_id: String,
}

impl CandidateCursor {
    fn decode(key: Option<&PageKey>) -> Result<Self, TemporalPortError> {
        key.map_or(
            Ok(Self {
                clause: 0,
                knowledge_at: i64::MAX,
                session_id: String::new(),
                stable_id: String::new(),
            }),
            |key| decode_cursor(key, CANDIDATE_OPERATION),
        )
    }

    fn encode(&self, cap: usize) -> Result<PageKey, TemporalPortError> {
        encode_cursor(self, cap, CANDIDATE_OPERATION)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RecordCursor {
    candidate: usize,
    kind: i64,
    #[serde(default)]
    session_id: String,
    stable_id: String,
}

impl RecordCursor {
    fn decode(key: Option<&PageKey>) -> Result<Self, TemporalPortError> {
        key.map_or(
            Ok(Self {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            }),
            |key| decode_cursor(key, RECORD_OPERATION),
        )
    }

    fn encode(&self, cap: usize) -> Result<PageKey, TemporalPortError> {
        encode_cursor(self, cap, RECORD_OPERATION)
    }

    fn from_row(row: &Row) -> Result<Self, TemporalPortError> {
        let candidate: i64 = row
            .get(0)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        Ok(Self {
            candidate: usize::try_from(candidate)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            kind: row
                .get(1)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            session_id: row
                .get(15)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            stable_id: row
                .get(2)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
        })
    }
}

fn encode_cursor(
    cursor: &impl Serialize,
    cap: usize,
    operation: &'static str,
) -> Result<PageKey, TemporalPortError> {
    let encoded = serde_json::to_string(cursor).map_err(|error| read_error(operation, error))?;
    if encoded.len() > cap {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "continuation key bytes",
        });
    }
    Ok(PageKey::new(encoded))
}

fn decode_cursor<T: DeserializeOwned>(
    key: &PageKey,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_str(key.as_str()).map_err(|error| read_error(operation, error))
}

fn validate_clause(
    clause: &CandidateClause,
    request: &PageRequest,
) -> Result<(), TemporalPortError> {
    let metadata_cap = request
        .candidate_field_caps()
        .map_or(request.max_item_bytes(), |caps| caps.metadata_field_bytes());
    if clause.value.len() > request.max_item_bytes() || clause.value.len() > metadata_cap {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "candidate clause bytes",
        });
    }
    Ok(())
}

fn fits_bytes(page_bytes: usize, item_bytes: usize, bounds: PageBounds, item_cap: usize) -> bool {
    item_bytes <= item_cap
        && page_bytes
            .checked_add(item_bytes)
            .is_some_and(|total| total <= bounds.bytes)
}

fn bounded_window_end(total: usize, start: usize, capacity: usize) -> usize {
    cmp::min(total, start.saturating_add(capacity))
}

fn authorized_root_project_key<'a>(
    scope: &TemporalRetrievalScope,
    snapshot: &'a TemporalExecutionSnapshot,
) -> Result<Option<&'a str>, TemporalPortError> {
    if !matches!(scope, TemporalRetrievalScope::AllSessionsInAuthorizedRoot) {
        return Ok(None);
    }
    snapshot
        .request()
        .authorized_root()
        .map(|root| Some(root.project_key()))
        .ok_or(TemporalPortError::UnauthorizedSnapshot)
}

async fn require_candidate_root_authority(
    conn: &Connection,
    candidate: &RankingCandidate,
    project_key: &str,
    provider: Option<&str>,
) -> Result<(), TemporalPortError> {
    let session_id = candidate
        .session
        .as_deref()
        .filter(|session| !session.is_empty())
        .ok_or_else(|| {
            read_message(
                RECORD_OPERATION,
                "root-wide candidate is missing session identity",
            )
        })?;
    let source_id = candidate.retriever_record_id.as_str();
    if source_id.is_empty() {
        return Err(read_message(
            RECORD_OPERATION,
            "root-wide candidate is missing retriever record identity",
        ));
    }
    let provider = provider.map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string()));
    let params = vec![
        SqlValue::Text(candidate.anchor_id.to_string()),
        SqlValue::Text(session_id.to_string()),
        SqlValue::Text(project_key.to_string()),
        SqlValue::Text(source_id.to_string()),
        provider,
    ];
    let sql = match candidate.channel {
        CandidateChannel::Summary => {
            "SELECT EXISTS (
                 SELECT 1
                 FROM retrieval_anchors AS authority_anchor
                 JOIN session_summary_nodes AS summary
                   ON summary.summary_anchor_id = authority_anchor.anchor_id
                  AND summary.session_id = ?2
                  AND summary.summary_id = ?4
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = summary.session_id
                  AND generation.state = 'active'
                 JOIN sessions AS authority_session
                   ON authority_session.session_id = summary.session_id
                  AND authority_session.provider =
                      json_extract(summary.publication_json, '$.provider')
                  AND authority_session.project_key = ?3
                 WHERE authority_anchor.anchor_id = ?1
                   AND (
                       (authority_session.project_key = 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
                       OR
                       (authority_session.project_key <> 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
                        AND json_extract(authority_anchor.owner_json, '$.project_id')
                            = authority_session.project_key)
                   )
                   AND (?5 IS NULL OR EXISTS (
                       WITH RECURSIVE retained_sources(
                           source_anchor_id, source_summary_id, depth
                       ) AS (
                           SELECT source_anchor_id, source_summary_id, 0
                           FROM session_summary_sources
                           WHERE summary_id = summary.summary_id
                           UNION ALL
                           SELECT nested.source_anchor_id, nested.source_summary_id,
                                  retained.depth + 1
                           FROM retained_sources AS retained
                           JOIN session_summary_nodes AS retained_summary
                             ON retained_summary.summary_id = retained.source_summary_id
                            AND retained_summary.session_id = summary.session_id
                           JOIN session_summary_sources AS nested
                             ON nested.summary_id = retained_summary.summary_id
                           WHERE retained.depth < 63
                           LIMIT 257
                       )
                       SELECT 1
                       FROM retained_sources AS retained
                       JOIN session_occurrences AS source_occurrence
                         ON source_occurrence.retrieval_anchor_id =
                            retained.source_anchor_id
                        AND source_occurrence.session_id = summary.session_id
                        AND source_occurrence.generation = generation.generation
                       JOIN observations AS source_observation
                         ON source_observation.observation_id =
                            source_occurrence.source_observation_id
                       WHERE json_extract(
                           source_observation.observation_json,
                           '$.identity.source.provider'
                       ) = ?5
                       LIMIT 1
                   ))
                 LIMIT 1
             )"
        }
        CandidateChannel::Scope
        | CandidateChannel::ExactMessage
        | CandidateChannel::Phrase
        | CandidateChannel::Entity
        | CandidateChannel::Time
        | CandidateChannel::Lexical => {
            "SELECT EXISTS (
                 SELECT 1
                 FROM retrieval_anchors AS authority_anchor
                 JOIN session_occurrences AS occurrence
                   ON occurrence.retrieval_anchor_id = authority_anchor.anchor_id
                  AND occurrence.session_id = ?2
                  AND occurrence.occurrence_id = ?4
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                  AND generation.state = 'active'
                 JOIN observations AS source_observation
                   ON source_observation.observation_id =
                      occurrence.source_observation_id
                 JOIN sessions AS authority_session
                   ON authority_session.session_id = occurrence.session_id
                  AND authority_session.provider = json_extract(
                      source_observation.observation_json,
                      '$.identity.source.provider'
                  )
                  AND authority_session.project_key = ?3
                 WHERE authority_anchor.anchor_id = ?1
                   AND (?5 IS NULL OR authority_session.provider = ?5)
                   AND (
                       (authority_session.project_key = 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
                       OR
                       (authority_session.project_key <> 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
                        AND json_extract(authority_anchor.owner_json, '$.project_id')
                            = authority_session.project_key)
                   )
                 LIMIT 1
             )"
        }
    };
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| read_error(RECORD_OPERATION, error))?;
    let authorized: i64 = rows
        .next()
        .await
        .map_err(|error| read_error(RECORD_OPERATION, error))?
        .ok_or_else(|| read_message(RECORD_OPERATION, "root authority query returned no row"))?
        .get(0)
        .map_err(|error| read_error(RECORD_OPERATION, error))?;
    if authorized != 1 {
        return Err(read_message(
            RECORD_OPERATION,
            "candidate is outside the authorized root",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn query_candidate_clause(
    conn: &Connection,
    scope: &TemporalRetrievalScope,
    snapshot: &TemporalExecutionSnapshot,
    clause: &CandidateClause,
    cursor: &CandidateCursor,
    limit: usize,
    request: &PageRequest,
    root_project_key: Option<&str>,
) -> Result<libsql::Rows, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let generation = i64::try_from(snapshot.watermarks().generation)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let limit = i64::try_from(limit).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let caps = request.candidate_field_caps();
    let metadata_cap = caps.map_or(request.max_item_bytes(), |value| {
        value.metadata_field_bytes()
    });
    let stable_cap = caps.map_or(request.max_item_bytes(), |value| value.stable_id_bytes());
    let source_stable_cap = stable_cap.min(metadata_cap);
    let source_stable_cap =
        i64::try_from(source_stable_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let stable_cap =
        i64::try_from(stable_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let anchor_cap =
        i64::try_from(caps.map_or(request.max_item_bytes(), |value| value.anchor_id_bytes()))
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let metadata_cap =
        i64::try_from(metadata_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let item_cap = i64::try_from(request.max_item_bytes())
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let provider = snapshot
        .provider_scope()
        .map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string()));
    let root_project_key =
        root_project_key.map(|project_key| SqlValue::Text(project_key.to_string()));
    let (sql, params) = match (scope, clause.channel) {
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::Scope) => {
            return Err(read_message(
                CANDIDATE_OPERATION,
                "scope scans require an exact session",
            ));
        }
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::ExactMessage) => (
            ROOT_EXACT_CANDIDATE_QUERY,
            vec![
                root_project_key.clone().ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(stable_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
            CandidateChannel::Phrase | CandidateChannel::Entity | CandidateChannel::Lexical,
        ) => (
            ROOT_OCCURRENCE_FTS_QUERY,
            vec![
                root_project_key.clone().ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(stable_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::Time) => {
            let (start, end) = iso_day_bounds(&clause.value)?;
            (
                ROOT_TIME_CANDIDATE_QUERY,
                vec![
                    root_project_key.clone().ok_or_else(|| {
                        read_message(CANDIDATE_OPERATION, "authorized root is missing")
                    })?,
                    provider,
                    SqlValue::Integer(start),
                    SqlValue::Integer(end),
                    SqlValue::Integer(cursor.knowledge_at),
                    SqlValue::Text(cursor.session_id.clone()),
                    SqlValue::Text(cursor.stable_id.clone()),
                    SqlValue::Integer(source_stable_cap),
                    SqlValue::Integer(anchor_cap),
                    SqlValue::Integer(metadata_cap),
                    SqlValue::Integer(item_cap),
                    SqlValue::Integer(stable_cap),
                    SqlValue::Integer(limit),
                ],
            )
        }
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::Summary) => (
            ROOT_SUMMARY_CANDIDATE_QUERY,
            vec![
                root_project_key.ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(stable_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::ExactMessage) => (
            EXACT_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::Scope) => (
            SCOPE_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (
            TemporalRetrievalScope::Session(session_id),
            CandidateChannel::Phrase | CandidateChannel::Entity | CandidateChannel::Lexical,
        ) => (
            OCCURRENCE_FTS_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::Time) => {
            let (start, end) = iso_day_bounds(&clause.value)?;
            (
                TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text(session_id.as_str().to_string()),
                    SqlValue::Integer(generation),
                    provider,
                    SqlValue::Integer(start),
                    SqlValue::Integer(end),
                    SqlValue::Integer(cursor.knowledge_at),
                    SqlValue::Text(cursor.stable_id.clone()),
                    SqlValue::Integer(source_stable_cap),
                    SqlValue::Integer(anchor_cap),
                    SqlValue::Integer(metadata_cap),
                    SqlValue::Integer(item_cap),
                    SqlValue::Integer(limit),
                ],
            )
        }
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::Summary) => (
            SUMMARY_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(limit),
            ],
        ),
    };
    conn.query(sql, params)
        .await
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))
}

fn candidate_from_row(
    row: &Row,
    channel: CandidateChannel,
    _scope: &TemporalRetrievalScope,
) -> Result<RankingCandidate, TemporalPortError> {
    let source_id: String = row
        .get(0)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let anchor: String = row
        .get(1)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let session: String = row
        .get(5)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let source_partition: String = row
        .get(7)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    Ok(RankingCandidate {
        stable_id: anchor.clone(),
        anchor_id: parse_text(anchor, CANDIDATE_OPERATION)?,
        retriever_record_id: source_id,
        channel,
        raw_score: candidate_score(channel),
        knowledge_at_micros: row
            .get(2)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?,
        logical_message: row
            .get(3)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?,
        turn: row
            .get(4)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?,
        session: Some(session),
        source: Some(source_partition),
        evidence_role: row
            .get(6)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?,
    })
}

const fn candidate_score(channel: CandidateChannel) -> i64 {
    match channel {
        CandidateChannel::Scope => 100,
        CandidateChannel::ExactMessage => 1_000,
        CandidateChannel::Phrase => 800,
        CandidateChannel::Entity => 700,
        CandidateChannel::Time => 600,
        CandidateChannel::Summary => 500,
        CandidateChannel::Lexical => 400,
    }
}

fn fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn iso_day_bounds(value: &str) -> Result<(i64, i64), TemporalPortError> {
    let start_seconds = parse_rfc3339_timestamp(&format!("{value}T00:00:00Z"))
        .ok_or_else(|| read_message(CANDIDATE_OPERATION, "invalid ISO date candidate"))?;
    let start = start_seconds
        .checked_mul(1_000_000)
        .ok_or(TemporalPortError::BudgetExceeded {
            resource: "time range",
        })?;
    let end = start
        .checked_add(86_400_000_000)
        .ok_or(TemporalPortError::BudgetExceeded {
            resource: "time range",
        })?;
    Ok((start, end))
}

fn require_snapshot_scope(
    scope: &TemporalRetrievalScope,
    snapshot: &TemporalExecutionSnapshot,
) -> Result<(), TemporalPortError> {
    if scope != snapshot.retrieval_scope() {
        return Err(read_message(
            SNAPSHOT_OPERATION,
            "retrieval scope does not match the frozen snapshot",
        ));
    }
    Ok(())
}

fn require_candidate_scope(
    scope: &TemporalRetrievalScope,
    candidate: &RankingCandidate,
) -> Result<(), TemporalPortError> {
    match scope {
        TemporalRetrievalScope::Session(session_id) => {
            if candidate
                .session
                .as_deref()
                .is_some_and(|session| session != session_id.as_str())
            {
                return Err(read_message(
                    RECORD_OPERATION,
                    "candidate is outside the frozen session scope",
                ));
            }
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            if candidate.session.as_deref().is_none_or(str::is_empty) {
                return Err(read_message(
                    RECORD_OPERATION,
                    "root-wide candidate is missing session identity",
                ));
            }
        }
    }
    Ok(())
}

struct RecordQuery {
    sql: String,
    params: Vec<SqlValue>,
}

struct RecordScopeSql {
    occurrence_condition: String,
    occurrence_generation_join: String,
    assertion_condition: String,
    assertion_generation_join: String,
    target_condition: String,
    target_generation_join: String,
    summary_condition: String,
    summary_generation_join: String,
    availability_condition: String,
    summary_generation: String,
    source_current_session: String,
}

impl RecordScopeSql {
    fn new(scope: &TemporalRetrievalScope, scope_param: usize, generation_param: usize) -> Self {
        match scope {
            TemporalRetrievalScope::Session(_) => Self {
                occurrence_condition: format!(
                    "AND o.session_id = ?{scope_param}
                     AND o.generation = ?{generation_param}"
                ),
                occurrence_generation_join: String::new(),
                assertion_condition: format!(
                    "AND a.session_id = ?{scope_param}
                     AND a.generation = ?{generation_param}"
                ),
                assertion_generation_join: String::new(),
                target_condition: format!(
                    "AND target.session_id = ?{scope_param}
                     AND target.generation = ?{generation_param}"
                ),
                target_generation_join: String::new(),
                summary_condition: format!("AND n.session_id = ?{scope_param}"),
                summary_generation_join: String::new(),
                availability_condition: format!(
                    "availability.session_id = ?{scope_param}
                     AND availability.generation = ?{generation_param}"
                ),
                summary_generation: format!("?{generation_param}"),
                source_current_session: format!("?{scope_param}"),
            },
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => Self {
                occurrence_condition: "AND o.session_id = c.session_id".to_string(),
                occurrence_generation_join:
                    "JOIN session_temporal_generations AS occurrence_generation
                       ON occurrence_generation.session_id = o.session_id
                      AND occurrence_generation.generation = o.generation
                      AND occurrence_generation.state = 'active'"
                        .to_string(),
                assertion_condition: "AND a.session_id = c.session_id".to_string(),
                assertion_generation_join:
                    "JOIN session_temporal_generations AS assertion_generation
                       ON assertion_generation.session_id = a.session_id
                      AND assertion_generation.generation = a.generation
                      AND assertion_generation.state = 'active'"
                        .to_string(),
                target_condition: "AND target.session_id = c.session_id".to_string(),
                target_generation_join: "JOIN session_temporal_generations AS copy_generation
                       ON copy_generation.session_id = target.session_id
                      AND copy_generation.generation = target.generation
                      AND copy_generation.state = 'active'"
                    .to_string(),
                summary_condition: "AND n.session_id = c.session_id".to_string(),
                summary_generation_join: "JOIN session_temporal_generations AS summary_generation
                       ON summary_generation.session_id = n.session_id
                      AND summary_generation.state = 'active'"
                    .to_string(),
                availability_condition: "availability.session_id = n.session_id
                     AND availability.generation = summary_generation.generation"
                    .to_string(),
                summary_generation: "summary_generation.generation".to_string(),
                source_current_session: "n.session_id".to_string(),
            },
        }
    }
}

fn build_record_query(
    scope: &TemporalRetrievalScope,
    snapshot: &TemporalExecutionSnapshot,
    candidates: &[RankingCandidate],
    candidate_offset: usize,
    cursor: &RecordCursor,
    limit: usize,
    request: &PageRequest,
) -> Result<RecordQuery, TemporalPortError> {
    if candidates.len() > request.page_item_limit().saturating_add(1) {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "record candidate window",
        });
    }
    let mut params = Vec::with_capacity(candidates.len().saturating_mul(3).saturating_add(14));
    let mut values = String::new();
    for (local, candidate) in candidates.iter().enumerate() {
        if local != 0 {
            values.push(',');
        }
        values.push_str("(?, ?, ?)");
        params.push(SqlValue::Integer(
            i64::try_from(candidate_offset.saturating_add(local))
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
        ));
        let session_id = match scope {
            TemporalRetrievalScope::Session(session_id) => session_id.as_str(),
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => candidate
                .session
                .as_deref()
                .filter(|session| !session.is_empty())
                .ok_or_else(|| {
                    read_message(
                        RECORD_OPERATION,
                        "root-wide candidate is missing session identity",
                    )
                })?,
        };
        params.push(SqlValue::Text(session_id.to_string()));
        params.push(SqlValue::Text(candidate.anchor_id.to_string()));
    }
    let scope_param = params.len() + 1;
    params.push(SqlValue::Text(
        snapshot.request().session_id().as_str().to_string(),
    ));
    let generation_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(snapshot.watermarks().generation)
            .map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let provider_param = params.len() + 1;
    params.push(
        snapshot
            .provider_scope()
            .map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string())),
    );
    let root_param = params.len() + 1;
    params.push(match scope {
        TemporalRetrievalScope::Session(_) => SqlValue::Null,
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => SqlValue::Text(
            snapshot
                .request()
                .authorized_root()
                .ok_or(TemporalPortError::UnauthorizedSnapshot)?
                .project_key()
                .to_string(),
        ),
    });
    let cutoff_param = params.len() + 1;
    params.push(SqlValue::Integer(match snapshot.temporal_mode() {
        TemporalModeV1::AsOf { cutoff } => cutoff.0,
        _ => i64::MAX,
    }));
    let cursor_candidate_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(cursor.candidate).map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let cursor_kind_param = params.len() + 1;
    params.push(SqlValue::Integer(cursor.kind));
    let cursor_session_param = params.len() + 1;
    params.push(SqlValue::Text(cursor.session_id.clone()));
    let cursor_stable_param = params.len() + 1;
    params.push(SqlValue::Text(cursor.stable_id.clone()));
    let item_cap_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(request.max_item_bytes())
            .map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let source_byte_cap_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(request.max_item_bytes().max(1))
            .map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let source_count_cap_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(MAX_SUMMARY_SOURCES_PER_RECORD)
            .map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let source_probe_cap_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(MAX_SUMMARY_SOURCES_PER_RECORD.saturating_add(1))
            .map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let limit_param = params.len() + 1;
    params.push(SqlValue::Integer(
        i64::try_from(limit).map_err(|error| read_error(RECORD_OPERATION, error))?,
    ));
    let mode = RecordModeSql::new(snapshot.temporal_mode(), cutoff_param);
    let record_scope = RecordScopeSql::new(scope, scope_param, generation_param);
    let sql = format!(
        "WITH candidate_input(ordinal, session_id, anchor_id) AS (VALUES {values}),
         candidate(ordinal, session_id, anchor_id) AS (
             SELECT MIN(input.ordinal), input.session_id, input.anchor_id
             FROM candidate_input AS input
             WHERE ?{root_param} IS NULL
                OR EXISTS (
                    SELECT 1
                    FROM sessions AS root_session
                    WHERE root_session.session_id = input.session_id
                      AND root_session.project_key = ?{root_param}
                      AND (
                          ?{provider_param} IS NULL
                          OR root_session.provider = ?{provider_param}
                      )
                    LIMIT 1
                )
             GROUP BY input.session_id, input.anchor_id
         ),
         records AS (
             SELECT c.ordinal, 0 AS kind_rank, o.occurrence_id AS stable_id,
                    'occurrence' AS record_kind,
                    o.occurrence_id AS a, o.retrieval_anchor_id AS b, NULL AS c,
                    o.knowledge_at, o.valid_time_json, o.evidence_json,
                    NULL AS extra_json, NULL AS source_json, NULL AS predecessor,
                    NULL AS publication_json, NULL AS state, o.session_id AS scope_session
             FROM candidate AS c
             JOIN session_occurrences AS o
               ON o.retrieval_anchor_id = c.anchor_id
              {occurrence_condition}
             {occurrence_generation_join}
             JOIN observations AS occurrence_provider
               ON occurrence_provider.observation_id = o.source_observation_id
             {occurrence_join}
             WHERE {occurrence_predicate}
               AND (?{provider_param} IS NULL OR COALESCE(json_extract(
                   occurrence_provider.observation_json, '$.identity.source.provider'
               ), 'claude') = ?{provider_param})
               AND length(CAST(o.occurrence_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.valid_time_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.evidence_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.occurrence_id AS BLOB))
                   + length(CAST(o.retrieval_anchor_id AS BLOB))
                   + length(CAST(o.valid_time_json AS BLOB))
                   + length(CAST(o.evidence_json AS BLOB)) <= ?{item_cap_param}
             UNION ALL
             SELECT c.ordinal, 1, a.assertion_id, 'assertion',
                    a.assertion_kind, a.subject_anchor_id, a.object_anchor_id,
                    a.knowledge_at, a.valid_time_json, a.evidence_json,
                    NULL, NULL, NULL, NULL, NULL, a.session_id
             FROM candidate AS c
             JOIN session_assertions AS a
               ON (a.subject_anchor_id = c.anchor_id OR a.object_anchor_id = c.anchor_id)
              {assertion_condition}
             {assertion_generation_join}
             {assertion_join}
             WHERE {assertion_predicate}
               AND (?{provider_param} IS NULL OR EXISTS (
                   SELECT 1
                   FROM session_occurrences AS assertion_source
                   JOIN observations AS assertion_provider
                     ON assertion_provider.observation_id =
                        assertion_source.source_observation_id
                   WHERE assertion_source.session_id = a.session_id
                     AND assertion_source.generation = a.generation
                     AND assertion_source.retrieval_anchor_id =
                         json_extract(a.evidence_json, '$.source_anchor_id')
                     AND COALESCE(json_extract(
                         assertion_provider.observation_json,
                         '$.identity.source.provider'
                     ), 'claude') = ?{provider_param}
                   LIMIT 1
               ))
               AND length(CAST(a.assertion_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.assertion_kind AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.subject_anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.object_anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.valid_time_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.evidence_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(a.assertion_id AS BLOB))
                   + length(CAST(a.assertion_kind AS BLOB))
                   + length(CAST(a.subject_anchor_id AS BLOB))
                   + length(CAST(a.object_anchor_id AS BLOB))
                   + length(CAST(a.valid_time_json AS BLOB))
                   + length(CAST(a.evidence_json AS BLOB)) <= ?{item_cap_param}
             UNION ALL
             SELECT c.ordinal, 2,
                    e.occurrence_id || ':' || e.copied_from_occurrence_id,
                    'copy', e.occurrence_id, e.copied_from_occurrence_id, NULL,
                    e.knowledge_at, e.valid_time_json, NULL, e.proof_json, NULL, NULL, NULL, NULL,
                    e.session_id
             FROM candidate AS c
             JOIN session_occurrences AS target
               ON target.retrieval_anchor_id = c.anchor_id
              {target_condition}
             {target_generation_join}
             JOIN observations AS copy_provider
               ON copy_provider.observation_id = target.source_observation_id
             JOIN session_logical_copy_edges AS e
               ON e.session_id = target.session_id
              AND e.generation = target.generation
              AND e.occurrence_id = target.occurrence_id
             {copy_join}
             WHERE {copy_predicate}
               AND (?{provider_param} IS NULL OR COALESCE(json_extract(
                   copy_provider.observation_json, '$.identity.source.provider'
               ), 'claude') = ?{provider_param})
               AND length(CAST(e.occurrence_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(e.copied_from_occurrence_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(e.proof_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(e.occurrence_id AS BLOB))
                   + length(CAST(e.copied_from_occurrence_id AS BLOB))
                   + length(CAST(e.proof_json AS BLOB)) <= ?{item_cap_param}
             UNION ALL
             SELECT c.ordinal, 3, n.summary_id, 'summary',
                    n.summary_id, n.summary_anchor_id, NULL,
                    n.created_at, NULL, NULL, n.source_horizon_json,
                    (
                        SELECT json_group_array(source_anchor_id)
                        FROM (
                            SELECT COALESCE(ss.source_anchor_id, sn.summary_anchor_id)
                                   AS source_anchor_id
                            FROM session_summary_sources AS ss
                            LEFT JOIN session_summary_nodes AS sn
                              ON sn.summary_id = ss.source_summary_id
                             AND sn.session_id = n.session_id
                            WHERE ss.summary_id = n.summary_id
                              AND (
                                  ss.source_anchor_id IS NOT NULL
                                  OR sn.summary_anchor_id IS NOT NULL
                              )
                              AND length(CAST(COALESCE(
                                  ss.source_anchor_id, sn.summary_anchor_id
                              ) AS BLOB)) <= ?{source_byte_cap_param}
                            ORDER BY ss.source_ordinal
                            LIMIT ?{source_count_cap_param}
                        )
                    ),
                    (
                        SELECT successor.predecessor_summary_id
                        FROM session_summary_successors AS successor
                        JOIN session_summary_nodes AS predecessor
                          ON predecessor.summary_id = successor.predecessor_summary_id
                         AND predecessor.session_id = n.session_id
                        WHERE successor.successor_summary_id = n.summary_id
                        ORDER BY successor.created_at DESC,
                                 successor.predecessor_summary_id
                        LIMIT 1
                    ),
                    n.publication_json, availability.availability, n.session_id
             FROM candidate AS c
             JOIN session_summary_nodes AS n
               ON n.summary_anchor_id = c.anchor_id
              {summary_condition}
             {summary_generation_join}
             LEFT JOIN session_summary_availability AS availability
               ON availability.summary_id = n.summary_id
              AND {availability_condition}
             WHERE {summary_predicate}
               AND (?{provider_param} IS NULL OR EXISTS (
                   WITH RECURSIVE retained_sources(
                       source_anchor_id, source_summary_id, depth
                   ) AS (
                       SELECT source_anchor_id, source_summary_id, 0
                       FROM session_summary_sources
                       WHERE summary_id = n.summary_id
                       UNION ALL
                       SELECT nested.source_anchor_id, nested.source_summary_id,
                              retained.depth + 1
                       FROM retained_sources AS retained
                           JOIN session_summary_nodes AS retained_summary
                             ON retained_summary.summary_id = retained.source_summary_id
                            AND retained_summary.session_id = n.session_id
                       JOIN session_summary_sources AS nested
                             ON nested.summary_id = retained_summary.summary_id
                       WHERE retained.depth < 63
                       LIMIT 257
                   )
                   SELECT 1
                   FROM retained_sources AS retained
                   JOIN session_occurrences AS summary_source_occurrence
                     ON summary_source_occurrence.retrieval_anchor_id =
                        retained.source_anchor_id
                    AND summary_source_occurrence.session_id = n.session_id
                    AND summary_source_occurrence.generation = {summary_generation}
                   JOIN observations AS summary_source_provider
                     ON summary_source_provider.observation_id =
                        summary_source_occurrence.source_observation_id
                   WHERE COALESCE(json_extract(
                       summary_source_provider.observation_json,
                       '$.identity.source.provider'
                   ), 'claude') = ?{provider_param}
                   LIMIT 1
               ))
               AND length(CAST(n.summary_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(n.summary_anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(n.source_horizon_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(COALESCE(n.publication_json, '') AS BLOB))
                   <= ?{item_cap_param}
               AND (
                   SELECT COUNT(*)
                   FROM (
                       SELECT 1
                       FROM session_summary_sources AS count_source
                       WHERE count_source.summary_id = n.summary_id
                       ORDER BY count_source.source_ordinal
                       LIMIT ?{source_probe_cap_param}
                   )
               ) <= ?{source_count_cap_param}
               AND (
                   SELECT 2
                       + COALESCE(SUM(2 + 6 * source_bytes), 0)
                       + CASE WHEN COUNT(*) > 0 THEN COUNT(*) - 1 ELSE 0 END
                   FROM (
                       SELECT length(CAST(COALESCE(
                                  byte_source.source_anchor_id,
                                  byte_summary.summary_anchor_id
                              ) AS BLOB)) AS source_bytes
                       FROM session_summary_sources AS byte_source
                       LEFT JOIN session_summary_nodes AS byte_summary
                         ON byte_summary.summary_id = byte_source.source_summary_id
                        AND byte_summary.session_id = n.session_id
                       WHERE byte_source.summary_id = n.summary_id
                         AND (
                             byte_source.source_anchor_id IS NOT NULL
                             OR byte_summary.summary_anchor_id IS NOT NULL
                         )
                       ORDER BY byte_source.source_ordinal
                       LIMIT ?{source_probe_cap_param}
                   )
               ) <= ?{source_byte_cap_param}
               AND length(CAST(n.summary_id AS BLOB))
                   + length(CAST(n.summary_anchor_id AS BLOB))
                   + length(CAST(n.source_horizon_json AS BLOB))
                   + length(CAST(COALESCE(n.publication_json, '') AS BLOB))
                   + (
                       SELECT 2
                           + COALESCE(SUM(2 + 6 * source_bytes), 0)
                           + CASE WHEN COUNT(*) > 0 THEN COUNT(*) - 1 ELSE 0 END
                       FROM (
                           SELECT length(CAST(COALESCE(
                                      item_source.source_anchor_id,
                                      item_summary.summary_anchor_id
                                  ) AS BLOB)) AS source_bytes
                           FROM session_summary_sources AS item_source
                           LEFT JOIN session_summary_nodes AS item_summary
                             ON item_summary.summary_id = item_source.source_summary_id
                                AND item_summary.session_id = n.session_id
                           WHERE item_source.summary_id = n.summary_id
                                 AND (
                                     item_source.source_anchor_id IS NOT NULL
                                     OR item_summary.summary_anchor_id IS NOT NULL
                                 )
                           ORDER BY item_source.source_ordinal
                           LIMIT ?{source_probe_cap_param}
                       )
                   ) <= ?{item_cap_param}
             UNION ALL
             SELECT c.ordinal, 4,
                    n.summary_id || ':' || printf('%020d', ss.source_ordinal),
                    'summary_source',
                    COALESCE(ss.source_anchor_id, source_summary.summary_anchor_id),
                    NULL, NULL,
                    COALESCE(source_occurrence.knowledge_at, source_summary.created_at),
                    COALESCE(source_occurrence.valid_time_json, '{{\"kind\":\"unknown\"}}'),
                    NULL, NULL, NULL, NULL, NULL,
                    CASE
                        WHEN availability.availability = 'stale' THEN 'stale'
                        WHEN availability.availability = 'unavailable' THEN 'unavailable'
                        WHEN COALESCE(
                            ss.source_anchor_id, source_summary.summary_anchor_id
                        ) IS NULL THEN 'missing'
                        ELSE 'covered'
                    END, n.session_id
             FROM candidate AS c
             JOIN session_summary_nodes AS n
               ON n.summary_anchor_id = c.anchor_id
              {summary_condition}
             {summary_generation_join}
             JOIN session_summary_sources AS ss ON ss.summary_id = n.summary_id
             LEFT JOIN session_summary_nodes AS source_summary
               ON source_summary.summary_id = ss.source_summary_id
              AND source_summary.session_id = n.session_id
             LEFT JOIN session_current_entities AS source_current
               ON source_current.session_id = {source_current_session}
              AND source_current.generation = {summary_generation}
              AND source_current.entity_kind = 'occurrence_anchor'
              AND source_current.entity_id = COALESCE(
                  ss.source_anchor_id, source_summary.summary_anchor_id
              )
             LEFT JOIN session_occurrences AS source_occurrence
               ON source_occurrence.session_id = source_current.session_id
              AND source_occurrence.generation = source_current.generation
              AND source_occurrence.occurrence_id = source_current.current_occurrence_id
             LEFT JOIN session_summary_availability AS availability
               ON availability.summary_id = n.summary_id
              AND {availability_condition}
             WHERE {summary_predicate}
               AND ss.source_ordinal < ?{source_count_cap_param}
               AND (?{provider_param} IS NULL OR EXISTS (
                   WITH RECURSIVE retained_sources(
                       source_anchor_id, source_summary_id, depth
                   ) AS (
                       SELECT ss.source_anchor_id, ss.source_summary_id, 0
                       UNION ALL
                       SELECT nested.source_anchor_id, nested.source_summary_id,
                              retained.depth + 1
                       FROM retained_sources AS retained
                           JOIN session_summary_nodes AS retained_summary
                             ON retained_summary.summary_id = retained.source_summary_id
                            AND retained_summary.session_id = n.session_id
                       JOIN session_summary_sources AS nested
                             ON nested.summary_id = retained_summary.summary_id
                       WHERE retained.depth < 63
                       LIMIT 257
                   )
                   SELECT 1
                   FROM retained_sources AS retained
                   JOIN session_occurrences AS retained_occurrence
                     ON retained_occurrence.retrieval_anchor_id =
                        retained.source_anchor_id
                    AND retained_occurrence.session_id = n.session_id
                    AND retained_occurrence.generation = {summary_generation}
                   JOIN observations AS retained_provider
                     ON retained_provider.observation_id =
                        retained_occurrence.source_observation_id
                   WHERE COALESCE(json_extract(
                       retained_provider.observation_json,
                       '$.identity.source.provider'
                   ), 'claude') = ?{provider_param}
                   LIMIT 1
               ))
               AND length(CAST(n.summary_id AS BLOB))
                   + length(CAST(COALESCE(
                       ss.source_anchor_id, source_summary.summary_anchor_id
                   ) AS BLOB))
                   + length(CAST(COALESCE(
                       source_occurrence.valid_time_json, '{{\"kind\":\"unknown\"}}'
                   ) AS BLOB)) <= ?{item_cap_param}
         )
         SELECT ordinal, kind_rank, stable_id, record_kind,
                a, b, c, knowledge_at, valid_time_json, evidence_json,
                extra_json, source_json, predecessor, publication_json, state,
                scope_session
         FROM records
         WHERE (
             ordinal > ?{cursor_candidate_param}
             OR (
                 ordinal = ?{cursor_candidate_param}
                 AND (
                     kind_rank > ?{cursor_kind_param}
                     OR (
                         kind_rank = ?{cursor_kind_param}
                         AND (
                             scope_session > ?{cursor_session_param}
                             OR (
                                 scope_session = ?{cursor_session_param}
                                 AND stable_id > ?{cursor_stable_param}
                             )
                         )
                     )
                 )
             )
         )
           AND length(CAST(stable_id AS BLOB))
               + length(CAST(COALESCE(a, '') AS BLOB))
               + length(CAST(COALESCE(b, '') AS BLOB))
               + length(CAST(COALESCE(c, '') AS BLOB))
               + length(CAST(COALESCE(valid_time_json, '') AS BLOB))
               + length(CAST(COALESCE(evidence_json, '') AS BLOB))
               + length(CAST(COALESCE(extra_json, '') AS BLOB))
               + length(CAST(COALESCE(source_json, '') AS BLOB))
               + length(CAST(COALESCE(publication_json, '') AS BLOB))
               <= ?{item_cap_param}
         ORDER BY ordinal, kind_rank, scope_session, stable_id
         LIMIT ?{limit_param}",
        occurrence_condition = record_scope.occurrence_condition,
        occurrence_generation_join = record_scope.occurrence_generation_join,
        assertion_condition = record_scope.assertion_condition,
        assertion_generation_join = record_scope.assertion_generation_join,
        target_condition = record_scope.target_condition,
        target_generation_join = record_scope.target_generation_join,
        summary_condition = record_scope.summary_condition,
        summary_generation_join = record_scope.summary_generation_join,
        availability_condition = record_scope.availability_condition,
        summary_generation = record_scope.summary_generation,
        source_current_session = record_scope.source_current_session,
        occurrence_join = mode.occurrence_join,
        occurrence_predicate = mode.occurrence_predicate,
        assertion_join = mode.assertion_join,
        assertion_predicate = mode.assertion_predicate,
        copy_join = mode.copy_join,
        copy_predicate = mode.copy_predicate,
        summary_predicate = mode.summary_predicate,
        root_param = root_param,
    );
    Ok(RecordQuery { sql, params })
}

struct RecordModeSql {
    occurrence_join: String,
    occurrence_predicate: String,
    assertion_join: String,
    assertion_predicate: String,
    copy_join: String,
    copy_predicate: String,
    summary_predicate: String,
}

impl RecordModeSql {
    fn new(mode: TemporalModeV1, cutoff_param: usize) -> Self {
        match mode {
            TemporalModeV1::Current => Self {
                occurrence_join: "JOIN session_current_entities AS occurrence_current
                    ON occurrence_current.session_id = o.session_id
                   AND occurrence_current.generation = o.generation
                   AND occurrence_current.entity_kind = 'occurrence_anchor'
                   AND occurrence_current.entity_id = o.retrieval_anchor_id
                   AND occurrence_current.current_occurrence_id = o.occurrence_id"
                    .to_string(),
                occurrence_predicate: "1 = 1".to_string(),
                assertion_join: "JOIN session_current_entities AS assertion_current
                    ON assertion_current.session_id = a.session_id
                   AND assertion_current.generation = a.generation
                   AND assertion_current.entity_kind = 'assertion_anchor'
                   AND assertion_current.current_assertion_id = a.assertion_id"
                    .to_string(),
                assertion_predicate: "1 = 1".to_string(),
                copy_join: "JOIN session_current_entities AS copy_current
                    ON copy_current.session_id = target.session_id
                   AND copy_current.generation = target.generation
                   AND copy_current.entity_kind = 'occurrence_anchor'
                   AND copy_current.entity_id = target.retrieval_anchor_id
                   AND copy_current.current_occurrence_id = target.occurrence_id"
                    .to_string(),
                copy_predicate: "1 = 1".to_string(),
                summary_predicate: "availability.availability = 'available'".to_string(),
            },
            TemporalModeV1::AsOf { .. } => Self {
                occurrence_join: String::new(),
                occurrence_predicate: format!(
                    "o.knowledge_at <= ?{cutoff_param}
                     AND json_extract(o.valid_time_json, '$.kind') = 'known'
                     AND json_extract(o.valid_time_json, '$.valid_at') <= ?{cutoff_param}"
                ),
                assertion_join: String::new(),
                assertion_predicate: format!(
                    "a.knowledge_at <= ?{cutoff_param}
                     AND json_extract(a.valid_time_json, '$.kind') = 'known'
                     AND json_extract(a.valid_time_json, '$.valid_at') <= ?{cutoff_param}"
                ),
                copy_join: String::new(),
                copy_predicate: format!("e.created_at <= ?{cutoff_param}"),
                summary_predicate: format!(
                    "n.created_at <= ?{cutoff_param}
                     AND COALESCE(availability.availability, 'unavailable') <> 'unavailable'"
                ),
            },
            TemporalModeV1::Evolution => Self {
                occurrence_join: String::new(),
                occurrence_predicate: "1 = 1".to_string(),
                assertion_join: String::new(),
                assertion_predicate: "1 = 1".to_string(),
                copy_join: String::new(),
                copy_predicate: "1 = 1".to_string(),
                summary_predicate:
                    "COALESCE(availability.availability, 'unavailable') <> 'unavailable'"
                        .to_string(),
            },
            TemporalModeV1::Forensic => Self {
                occurrence_join: String::new(),
                occurrence_predicate: "1 = 1".to_string(),
                assertion_join: String::new(),
                assertion_predicate: "1 = 1".to_string(),
                copy_join: String::new(),
                copy_predicate: "1 = 1".to_string(),
                summary_predicate: "1 = 1".to_string(),
            },
        }
    }
}

fn temporal_record_from_row(row: &Row) -> Result<TemporalRecord, TemporalPortError> {
    let kind: String = row
        .get(3)
        .map_err(|error| read_error(RECORD_OPERATION, error))?;
    match kind.as_str() {
        "occurrence" => {
            let occurrence_id = required_string(row, 4)?;
            let anchor_id = required_string(row, 5)?;
            let knowledge_at = required_i64(row, 7)?;
            let valid_time = required_string(row, 8)?;
            let evidence = required_string(row, 9)?;
            let evidence: SessionEvidenceMetadataV1 = parse_json(&evidence, RECORD_OPERATION)?;
            Ok(TemporalRecord::Occurrence(ResolutionOccurrence {
                occurrence_id: parse_text(occurrence_id, RECORD_OPERATION)?,
                anchor_id: parse_text(anchor_id, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(knowledge_at),
                valid_time: parse_json(&valid_time, RECORD_OPERATION)?,
                evidence: authorized_evidence(evidence),
            }))
        }
        "assertion" => {
            let assertion_kind = required_string(row, 4)?;
            let subject = required_string(row, 5)?;
            let object = required_string(row, 6)?;
            let evidence: SessionEvidenceMetadataV1 =
                parse_json(&required_string(row, 9)?, RECORD_OPERATION)?;
            Ok(TemporalRecord::Assertion(ResolutionAssertion {
                kind: parse_text(assertion_kind, RECORD_OPERATION)?,
                subject_anchor_id: parse_text(subject, RECORD_OPERATION)?,
                object_anchor_id: parse_text(object, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(required_i64(row, 7)?),
                valid_time: parse_json(&required_string(row, 8)?, RECORD_OPERATION)?,
                evidence: authorized_evidence(evidence),
            }))
        }
        "copy" => {
            let valid_time = match required_string(row, 8) {
                Ok(encoded) => parse_json(&encoded, RECORD_OPERATION)?,
                Err(_) => TemporalValidityV1::Unknown,
            };
            Ok(TemporalRecord::Copy(LogicalCopyRecordV1 {
                occurrence_id: parse_text(required_string(row, 4)?, RECORD_OPERATION)?,
                copied_from_occurrence_id: parse_text(required_string(row, 5)?, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(required_i64(row, 7)?),
                valid_time,
                proof: parse_json(&required_string(row, 10)?, RECORD_OPERATION)?,
            }))
        }
        "summary" => summary_from_row(row).map(TemporalRecord::Summary),
        "summary_source" => summary_source_from_row(row).map(TemporalRecord::SummarySource),
        _ => Err(read_message(
            RECORD_OPERATION,
            "unknown temporal record kind",
        )),
    }
}

fn summary_from_row(row: &Row) -> Result<SessionSummaryRecordV1, TemporalPortError> {
    let summary_id: SessionSummaryIdV1 = parse_text(required_string(row, 4)?, RECORD_OPERATION)?;
    let summary_anchor: RetrievalAnchorId = parse_text(required_string(row, 5)?, RECORD_OPERATION)?;
    let source_values: Vec<String> = parse_json(&required_string(row, 11)?, RECORD_OPERATION)?;
    let mut source_anchors = Vec::with_capacity(source_values.len());
    for value in source_values {
        source_anchors.push(parse_text(value, RECORD_OPERATION)?);
    }
    let session_id: SessionId = parse_text(required_string(row, 15)?, RECORD_OPERATION)?;
    let horizon: SummarySourceHorizonV1 = parse_json(&required_string(row, 10)?, RECORD_OPERATION)?;
    let mut summary = SessionSummaryRecordV1::new(
        summary_id,
        session_id,
        summary_anchor,
        source_anchors,
        horizon,
        UtcMicros(required_i64(row, 7)?),
    )
    .map_err(|error| read_error(RECORD_OPERATION, error))?;
    if let Some(predecessor) = optional_string(row, 12)? {
        summary = summary
            .with_predecessor(parse_text(predecessor, RECORD_OPERATION)?)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
    }
    if let Some(publication) = optional_string(row, 13)? {
        let value: serde_json::Value = parse_json(&publication, RECORD_OPERATION)?;
        let publication = if value.get("version").is_some() {
            let configuration_digest = value["configuration_digest"]
                .as_str()
                .map(|digest| {
                    if digest.starts_with("sha256:") {
                        digest.to_owned()
                    } else {
                        format!("sha256:{digest}")
                    }
                })
                .ok_or_else(|| {
                    read_message(
                        RECORD_OPERATION,
                        "summary publication configuration digest is unavailable",
                    )
                })?;
            serde_json::json!({
                "model_route": value["model_route"],
                "configuration_digest": configuration_digest,
                "sanitization_receipt": {
                    "receipt_id": value["sanitization_receipt"],
                    "sanitizer_version": super::operations::SANITIZER_VERSION,
                },
            })
        } else {
            value
        };
        let publication: SummaryPublicationMetadataV1 = serde_json::from_value(publication)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        summary = summary
            .with_publication(publication)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
    }
    Ok(summary)
}

fn summary_source_from_row(row: &Row) -> Result<SummarySourceRecord, TemporalPortError> {
    let anchor_id = parse_text(required_string(row, 4)?, RECORD_OPERATION)?;
    let state = match required_string(row, 14)?.as_str() {
        "covered" => SummarySourceState::Covered {
            knowledge_at: UtcMicros(required_i64(row, 7)?),
            valid_time: parse_json(&required_string(row, 8)?, RECORD_OPERATION)?,
        },
        "stale" => SummarySourceState::Stale,
        "unavailable" => SummarySourceState::Unavailable,
        "missing" => SummarySourceState::Missing,
        _ => {
            return Err(read_message(
                RECORD_OPERATION,
                "unknown summary source state",
            ));
        }
    };
    Ok(SummarySourceRecord { anchor_id, state })
}

fn authorized_evidence(evidence: SessionEvidenceMetadataV1) -> ResolutionEvidence {
    ResolutionEvidence::new(evidence.authority, ValidatedAuthorization::Authorized)
        .with_supporting_anchor(evidence.source_anchor_id)
}

fn required_string(row: &Row, column: i32) -> Result<String, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

fn optional_string(row: &Row, column: i32) -> Result<Option<String>, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

fn required_i64(row: &Row, column: i32) -> Result<i64, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

fn parse_json<T: DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_str(value).map_err(|error| read_error(operation, error))
}

fn parse_text<T: DeserializeOwned>(
    value: String,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| read_error(operation, error))
}

fn read_error(operation: &'static str, error: impl std::fmt::Display) -> TemporalPortError {
    TemporalPortError::Read {
        operation,
        message: error.to_string(),
    }
}

fn read_message(operation: &'static str, message: impl Into<String>) -> TemporalPortError {
    TemporalPortError::Read {
        operation,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use libsql::Builder;
    use tempfile::tempdir;
    use tracedecay_domain::RetrievalGrainV1;

    use super::*;
    use crate::global_db::GlobalDb;
    use crate::query::temporal::ports::{
        BindingDigest, KernelVersions, TemporalAuthorizedRoot, TemporalSnapshotRequest,
        TemporalWatermarks,
    };
    use crate::query::temporal::resolution::ValidatedAuthorization;

    const REQUIRED_SCHEMA_INDEXES: &[&str] = &[
        "idx_session_temporal_generations_session_state",
        "idx_session_occurrences_generation_order",
        "idx_session_current_entities_primary_key",
        "idx_session_assertions_subject",
        "idx_session_summary_availability_generation",
        "idx_session_summary_nodes_session_created",
        "session_occurrences_fts",
        "session_summary_nodes_fts",
    ];
    const FOLLOW_UP_SCHEMA_INDEXES: &[&str] = &[
        "session_occurrences(session_id, generation, retrieval_anchor_id, knowledge_at, occurrence_id)",
        "session_assertions(session_id, generation, object_anchor_id, knowledge_at, assertion_id)",
        "session_summary_successors(successor_summary_id, created_at, predecessor_summary_id)",
        "session_occurrences(knowledge_at DESC, session_id, occurrence_id, generation)",
        "session_summary_nodes(created_at DESC, session_id, summary_id)",
    ];

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot(generation: u64) -> TemporalExecutionSnapshot {
        TemporalExecutionSnapshot::new_authorized(
            TemporalSnapshotRequest::new(
                SessionId::new("session-snapshot").expect("session"),
                digest('1'),
                digest('2'),
                digest('3'),
                TemporalModeV1::Current,
                RetrievalGrainV1::Session,
            )
            .expect("request"),
            TemporalWatermarks {
                generation,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('4'))
                    .expect("configuration"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("snapshot")
    }

    fn scoped_snapshot(generation: u64, provider: Option<&str>) -> TemporalExecutionSnapshot {
        scoped_snapshot_with_mode(generation, provider, TemporalModeV1::Current)
    }

    fn scoped_snapshot_with_mode(
        generation: u64,
        provider: Option<&str>,
        mode: TemporalModeV1,
    ) -> TemporalExecutionSnapshot {
        TemporalExecutionSnapshot::new_authorized(
            TemporalSnapshotRequest::new(
                SessionId::new("session-snapshot").expect("session"),
                digest('1'),
                digest('2'),
                digest('3'),
                mode,
                RetrievalGrainV1::Session,
            )
            .expect("request")
            .with_provider_scope(provider.map(str::to_string))
            .expect("provider scope"),
            TemporalWatermarks {
                generation,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('4'))
                    .expect("configuration"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("snapshot")
    }

    fn root_snapshot_with_mode(
        generation: u64,
        provider: Option<&str>,
        mode: TemporalModeV1,
    ) -> TemporalExecutionSnapshot {
        let request = TemporalSnapshotRequest::new(
            SessionId::new("session-snapshot").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            mode,
            RetrievalGrainV1::Session,
        )
        .expect("request")
        .with_authorized_root(
            TemporalAuthorizedRoot::profile("profile-1", "store-1", "root-1")
                .expect("profile root"),
        )
        .expect("authorized root")
        .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot)
        .with_provider_scope(provider.map(str::to_string))
        .expect("provider scope");
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('4'))
                    .expect("configuration"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("snapshot")
    }

    fn record_request() -> PageRequest {
        PageRequest::for_test(32, 64 * 1024, 8 * 1024, 32, 512)
    }

    fn record_candidate() -> RankingCandidate {
        candidate_for_anchor("anchor-1")
    }

    fn candidate_for_anchor(anchor_id: &str) -> RankingCandidate {
        RankingCandidate {
            stable_id: "exact:occurrence-1".to_string(),
            anchor_id: RetrievalAnchorId::new(anchor_id).expect("anchor"),
            retriever_record_id: "occurrence-1".to_string(),
            channel: CandidateChannel::ExactMessage,
            raw_score: 1_000,
            knowledge_at_micros: 1,
            logical_message: Some("message-1".to_string()),
            turn: None,
            session: Some("session-snapshot".to_string()),
            source: Some("claude".to_string()),
            evidence_role: Some("user".to_string()),
        }
    }

    async fn record_kinds(
        db: &GlobalDb,
        snapshot: &TemporalExecutionSnapshot,
        candidate: RankingCandidate,
        request: &PageRequest,
    ) -> Vec<String> {
        let query = build_record_query(
            snapshot.retrieval_scope(),
            snapshot,
            &[candidate],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            request.page_item_limit().saturating_add(1),
            request,
        )
        .expect("record query");
        let mut rows = db
            .read_connection()
            .query(&query.sql, query.params)
            .await
            .expect("record rows");
        let mut kinds = Vec::new();
        while let Some(row) = rows.next().await.expect("record row") {
            kinds.push(row.get(3).expect("record kind"));
        }
        kinds
    }

    async fn insert_generation(db: &GlobalDb, generation: u64) {
        insert_generation_for_session(db, "session-snapshot", generation).await;
    }

    async fn insert_generation_for_session(db: &GlobalDb, session_id: &str, generation: u64) {
        let frozen = serde_json::json!({
            "active_generation": generation,
            "cursor_key": null,
            "projection_frontier": 0,
            "source_frontier": 0,
            "summary_frontier": 0
        })
        .to_string();
        let generation = i64::try_from(generation).expect("generation");
        // frozen_watermarks_json is immutable after insert; seed it on building.
        db.read_connection()
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, ?2, 'building', ?3, ?2, NULL, NULL, NULL)",
                (session_id, generation, frozen.as_str()),
            )
            .await
            .expect("building generation");
        db.read_connection()
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = generation
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
                (session_id, generation),
            )
            .await
            .expect("ready generation");
        db.read_connection()
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = ?1
                 WHERE session_id = ?2
                   AND generation <> ?1
                   AND state = 'active'",
                (generation, session_id),
            )
            .await
            .expect("supersede prior active generation");
        db.read_connection()
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = generation
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
                (session_id, generation),
            )
            .await
            .expect("activate generation");

        let mut rows = db
            .read_connection()
            .query(
                "SELECT frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND generation = ?2
                 LIMIT 1",
                (session_id, generation),
            )
            .await
            .expect("query frozen watermarks");
        let encoded: String = rows
            .next()
            .await
            .expect("row")
            .expect("generation row")
            .get(0)
            .expect("frozen_watermarks_json");
        assert_eq!(
            encoded, frozen,
            "legal building→ready→active transitions must not mutate frozen_watermarks_json"
        );
    }

    #[test]
    fn adapter_contains_only_the_borrowed_global_db_handle() {
        fn assert_exact_fields(adapter: &GlobalDbTemporalReadPort<'_>) {
            let GlobalDbTemporalReadPort { read: _ } = adapter;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbTemporalReadPort<'static>>(),
            std::mem::size_of::<&'static GlobalDbReadSnapshot>()
        );
    }

    #[tokio::test]
    async fn frozen_generation_survives_rotation_while_a_new_snapshot_observes_drift() {
        let dir = tempdir().expect("temporary directory");
        let db = GlobalDb::try_open_at(&dir.path().join("global.db"))
            .await
            .expect("open database")
            .expect("database");
        insert_generation(&db, 1).await;
        let frozen_snapshot = snapshot(1);
        let frozen_read = db.read_snapshot().await.expect("read snapshot");
        let frozen_adapter = GlobalDbTemporalReadPort::new(&frozen_read);
        frozen_adapter
            .validate_snapshot(&frozen_snapshot)
            .await
            .expect("generation one is frozen active");

        insert_generation(&db, 2).await;

        frozen_adapter
            .validate_snapshot(&frozen_snapshot)
            .await
            .expect("same read snapshot retains generation one");
        let fresh_read = db.read_snapshot().await.expect("fresh read snapshot");
        let fresh_adapter = GlobalDbTemporalReadPort::new(&fresh_read);
        assert!(
            fresh_adapter
                .validate_snapshot(&frozen_snapshot)
                .await
                .is_err()
        );
        fresh_adapter
            .validate_snapshot(&snapshot(2))
            .await
            .expect("new read snapshot sees generation two");
    }

    #[test]
    fn candidate_and_record_cursors_are_stable_and_bounded() {
        let candidate = CandidateCursor {
            clause: 42,
            knowledge_at: 1_234_567,
            session_id: "session-b".to_string(),
            stable_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
        };
        let encoded = candidate.encode(256).unwrap();
        assert_eq!(CandidateCursor::decode(Some(&encoded)).unwrap(), candidate);

        let record = RecordCursor {
            candidate: 99_999,
            kind: 4,
            session_id: "session-b".to_string(),
            stable_id: "summary:17".to_string(),
        };
        let encoded = record.encode(256).unwrap();
        assert_eq!(RecordCursor::decode(Some(&encoded)).unwrap(), record);
        assert!(record.encode(8).is_err());
    }

    #[test]
    fn snapshot_uniqueness_probe_reads_at_most_two_rows() {
        let source = include_str!("retrieval.rs");
        let start = source
            .find("async fn validate_snapshot(")
            .expect("validator");
        let end = source[start..]
            .find("async fn produce_candidates(")
            .map(|offset| start + offset)
            .expect("validator end");
        let validator = &source[start..end];
        assert!(validator.contains("LIMIT 2"));
        assert!(validator.contains("frozen generation is not unique"));
    }

    #[test]
    fn one_hundred_thousand_candidates_are_windowed_before_sql_allocation() {
        let total = 100_000usize;
        let page_items = 37usize;
        let start = 71_111usize;
        let end = bounded_window_end(total, start, page_items.saturating_add(1));
        assert_eq!(end - start, 38);
        assert!(end < total);
    }

    #[test]
    fn mode_sql_is_shaped_without_optional_or_fallback_predicates() {
        let current = RecordModeSql::new(TemporalModeV1::Current, 9);
        assert!(current.occurrence_join.contains("session_current_entities"));
        assert!(!current.occurrence_join.contains(" OR "));

        let as_of = RecordModeSql::new(
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(10),
            },
            9,
        );
        assert!(as_of.occurrence_predicate.contains("o.knowledge_at <= ?9"));
        assert!(as_of.assertion_predicate.contains("a.knowledge_at <= ?9"));

        let evolution = RecordModeSql::new(TemporalModeV1::Evolution, 9);
        assert!(evolution.summary_predicate.contains("availability"));
        let forensic = RecordModeSql::new(TemporalModeV1::Forensic, 9);
        assert_eq!(forensic.summary_predicate, "1 = 1");
    }

    #[test]
    fn candidate_queries_use_keysets_limits_and_mode_indexes() {
        for sql in [
            EXACT_CANDIDATE_QUERY,
            OCCURRENCE_FTS_QUERY,
            TIME_CANDIDATE_QUERY,
            SUMMARY_CANDIDATE_QUERY,
        ] {
            assert!(sql.contains("LIMIT ?"));
            assert!(!sql.to_ascii_uppercase().contains("OFFSET"));
        }
        assert!(TIME_CANDIDATE_QUERY.contains("idx_session_occurrences_generation_order"));
        assert!(OCCURRENCE_FTS_QUERY.contains("session_occurrences_fts MATCH"));
        assert!(SUMMARY_CANDIDATE_QUERY.contains("session_summary_nodes_fts MATCH"));
    }

    #[test]
    fn authorized_root_candidate_queries_use_composite_session_keysets() {
        for sql in [
            ROOT_EXACT_CANDIDATE_QUERY,
            ROOT_OCCURRENCE_FTS_QUERY,
            ROOT_TIME_CANDIDATE_QUERY,
            ROOT_SUMMARY_CANDIDATE_QUERY,
        ] {
            assert!(sql.contains("session_id"));
            assert!(sql.contains("LIMIT ?"));
            assert!(!sql.to_ascii_uppercase().contains("OFFSET"));
        }
        assert!(
            ROOT_EXACT_CANDIDATE_QUERY
                .contains("ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id")
        );
        assert!(
            ROOT_SUMMARY_CANDIDATE_QUERY
                .contains("ORDER BY n.created_at DESC, n.session_id, n.summary_id")
        );
        assert!(ROOT_OCCURRENCE_FTS_QUERY.contains("session_occurrences_fts MATCH"));
        assert!(ROOT_SUMMARY_CANDIDATE_QUERY.contains("session_summary_nodes_fts MATCH"));
        assert_eq!(
            ROOT_OCCURRENCE_FTS_QUERY
                .matches("session_occurrences_fts MATCH")
                .count(),
            1,
            "root-wide FTS must be one calibrated store query, not per-session fan-out"
        );
    }

    #[test]
    fn authorized_root_candidate_queries_bind_durable_anchor_owner_before_materialization() {
        for sql in [
            ROOT_EXACT_CANDIDATE_QUERY,
            ROOT_OCCURRENCE_FTS_QUERY,
            ROOT_TIME_CANDIDATE_QUERY,
            ROOT_SUMMARY_CANDIDATE_QUERY,
        ] {
            assert!(sql.contains("JOIN retrieval_anchors AS authority_anchor"));
            assert!(sql.contains("JOIN sessions AS authority_session"));
            assert!(sql.contains("authority_session.project_key = ?1"));
            assert!(sql.contains("json_extract(authority_anchor.owner_json, '$.kind')"));
        }
    }

    #[tokio::test]
    async fn root_record_authority_binds_the_candidate_source_provider() {
        let dir = tempdir().unwrap();
        let database = Builder::new_local(dir.path().join("root-record-authority.db"))
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL
             );
             INSERT INTO sessions VALUES
                ('provider-good', 'shared-session', 'user'),
                ('provider-bad', 'shared-session', 'different-project');
             INSERT INTO retrieval_anchors VALUES
                ('anchor-1', '{\"kind\":\"profile\"}');
             INSERT INTO session_temporal_generations VALUES
                ('shared-session', 1, 'active');
             INSERT INTO observations VALUES (
                'observation-bad',
                '{\"identity\":{\"source\":{\"provider\":\"provider-bad\"}}}'
             );
             INSERT INTO session_occurrences VALUES (
                'shared-session', 1, 'occurrence-1', 'observation-bad', 'anchor-1'
             );",
        )
        .await
        .unwrap();
        let mut candidate = candidate_for_anchor("anchor-1");
        candidate.session = Some("shared-session".to_string());
        candidate.source = Some("occurrence-1".to_string());
        assert!(
            require_candidate_root_authority(&conn, &candidate, "user", None)
                .await
                .is_err()
        );
    }

    #[test]
    fn provider_scope_is_applied_at_every_candidate_authority_join() {
        for sql in [
            EXACT_CANDIDATE_QUERY,
            OCCURRENCE_FTS_QUERY,
            TIME_CANDIDATE_QUERY,
        ] {
            assert!(sql.contains("JOIN observations AS provider_observation"));
            assert!(sql.contains("$.identity.source.provider"));
            assert!(sql.contains("COALESCE(json_extract"));
            assert!(sql.contains("'claude'"));
        }
        assert!(SUMMARY_CANDIDATE_QUERY.contains("session_summary_sources"));
        assert!(SUMMARY_CANDIDATE_QUERY.contains("JOIN observations AS source_observation"));
        assert!(SUMMARY_CANDIDATE_QUERY.contains("$.identity.source.provider"));
        assert!(SUMMARY_CANDIDATE_QUERY.contains("COALESCE(json_extract"));
        assert!(SUMMARY_CANDIDATE_QUERY.contains("'claude'"));
    }

    #[test]
    fn record_union_filters_provider_and_large_fields_before_materialization() {
        let query = build_record_query(
            &TemporalRetrievalScope::Session(SessionId::new("session-snapshot").expect("session")),
            &scoped_snapshot(1, Some("claude")),
            &[record_candidate()],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            33,
            &record_request(),
        )
        .expect("record query");
        let records_end = query
            .sql
            .find("SELECT ordinal, kind_rank, stable_id, record_kind")
            .expect("outer records select");
        let records = &query.sql[..records_end];

        assert!(
            records.matches("JOIN observations AS").count() >= 4,
            "occurrence, assertion, copy, and summary-source arms need canonical provider joins"
        );
        assert!(records.matches("$.identity.source.provider").count() >= 5);
        assert!(records.matches("COALESCE(json_extract").count() >= 5);
        assert!(records.matches("'claude'").count() >= 5);
        for field in [
            "evidence_json",
            "proof_json",
            "source_horizon_json",
            "publication_json",
        ] {
            assert!(
                records.contains("length(CAST("),
                "{field} must be byte-bounded in its UNION arm"
            );
            assert!(records.contains(field));
        }
        assert!(records.contains("json_group_array"));
        let source = include_str!("retrieval.rs");
        let builder_start = source.find("fn build_record_query(").expect("builder");
        let builder_end = source[builder_start..]
            .find("struct RecordModeSql")
            .map(|offset| builder_start + offset)
            .expect("builder end");
        let builder = &source[builder_start..builder_end];
        assert!(builder.contains("source_count_cap_param"));
        assert!(builder.contains("source_byte_cap_param"));
        assert!(!builder.contains("LIMIT ?{source_byte_cap_param}"));
    }

    #[tokio::test]
    async fn explain_time_query_uses_generation_order_index() {
        let dir = tempdir().unwrap();
        let database = Builder::new_local(dir.path().join("query-plan.db"))
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );",
        )
        .await
        .unwrap();
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {TIME_CANDIDATE_QUERY}"),
                vec![
                    SqlValue::Text("session".to_string()),
                    SqlValue::Integer(1),
                    SqlValue::Null,
                    SqlValue::Integer(0),
                    SqlValue::Integer(1),
                    SqlValue::Integer(i64::MAX),
                    SqlValue::Text(String::new()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(10),
                ],
            )
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_session_occurrences_generation_order"))
        );
        assert!(details.iter().all(|detail| !detail.contains("SCAN o")));
    }

    #[tokio::test]
    async fn provider_filter_separates_same_session_and_none_reads_all_providers() {
        let dir = tempdir().unwrap();
        let database = Builder::new_local(dir.path().join("provider-scope.db"))
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO session_occurrences VALUES
                ('shared-session', 1, 'occurrence-claude', 'observation-claude',
                 'anchor-claude', 'message-claude', NULL, 'user', 2),
                ('shared-session', 1, 'occurrence-codex', 'observation-codex',
                 'anchor-codex', 'message-codex', NULL, 'user', 1);",
        )
        .await
        .unwrap();

        async fn occurrence_ids(
            conn: &Connection,
            provider: SqlValue,
        ) -> Result<Vec<String>, libsql::Error> {
            let mut rows = conn
                .query(
                    TIME_CANDIDATE_QUERY,
                    vec![
                        SqlValue::Text("shared-session".to_string()),
                        SqlValue::Integer(1),
                        provider,
                        SqlValue::Integer(0),
                        SqlValue::Integer(10),
                        SqlValue::Integer(i64::MAX),
                        SqlValue::Text(String::new()),
                        SqlValue::Integer(128),
                        SqlValue::Integer(128),
                        SqlValue::Integer(128),
                        SqlValue::Integer(1024),
                        SqlValue::Integer(10),
                    ],
                )
                .await?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await? {
                ids.push(row.get(0)?);
            }
            Ok(ids)
        }

        assert_eq!(
            occurrence_ids(&conn, SqlValue::Text("claude".to_string()))
                .await
                .unwrap(),
            ["occurrence-claude"]
        );
        assert_eq!(
            occurrence_ids(&conn, SqlValue::Null).await.unwrap(),
            ["occurrence-claude", "occurrence-codex"]
        );
    }

    #[tokio::test]
    async fn root_pagination_restart_provider_filter_and_session_parity_are_stable() {
        let dir = tempdir().unwrap();
        let database = Builder::new_local(dir.path().join("root-pagination.db"))
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             CREATE INDEX idx_session_occurrences_root_generation_order
                ON session_occurrences(
                    knowledge_at DESC, session_id, occurrence_id, generation
                );
             INSERT INTO session_temporal_generations VALUES
                ('session-a', 1, 'active'),
                ('session-b', 1, 'active'),
                ('session-c', 1, 'active');
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO retrieval_anchors VALUES
                ('same-anchor', '{\"kind\":\"profile\"}');
             INSERT INTO sessions VALUES
                ('claude', 'session-a', 'user'),
                ('claude', 'session-b', 'user'),
                ('codex', 'session-c', 'user');
             INSERT INTO session_occurrences VALUES
                ('session-a', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-b', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-c', 1, 'same-id', 'observation-codex',
                 'same-anchor', 'same-message', NULL, 'user', 5);",
        )
        .await
        .unwrap();

        async fn root_rows(
            conn: &Connection,
            provider: SqlValue,
            cursor: (i64, &str, &str),
            limit: i64,
        ) -> Vec<(String, String)> {
            let mut rows = conn
                .query(
                    ROOT_TIME_CANDIDATE_QUERY,
                    vec![
                        SqlValue::Text("user".to_string()),
                        provider,
                        SqlValue::Integer(0),
                        SqlValue::Integer(10),
                        SqlValue::Integer(cursor.0),
                        SqlValue::Text(cursor.1.to_string()),
                        SqlValue::Text(cursor.2.to_string()),
                        SqlValue::Integer(128),
                        SqlValue::Integer(128),
                        SqlValue::Integer(128),
                        SqlValue::Integer(1_024),
                        SqlValue::Integer(1_024),
                        SqlValue::Integer(limit),
                    ],
                )
                .await
                .unwrap();
            let mut values = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                values.push((row.get(5).unwrap(), row.get(0).unwrap()));
            }
            values
        }

        let first = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 1).await;
        assert_eq!(first, [("session-a".to_string(), "same-id".to_string())]);
        let continuation = (5, first[0].0.as_str(), first[0].1.as_str());
        let second = root_rows(&conn, SqlValue::Null, continuation, 1).await;
        let restarted = root_rows(&conn, SqlValue::Null, continuation, 1).await;
        assert_eq!(second, [("session-b".to_string(), "same-id".to_string())]);
        assert_eq!(restarted, second);
        assert_eq!(
            root_rows(
                &conn,
                SqlValue::Text("claude".to_string()),
                (i64::MAX, "", ""),
                10,
            )
            .await,
            [
                ("session-a".to_string(), "same-id".to_string()),
                ("session-b".to_string(), "same-id".to_string()),
            ]
        );

        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'superseded'
             WHERE session_id <> 'session-a'",
            (),
        )
        .await
        .unwrap();
        let root = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 10).await;
        let mut session_rows = conn
            .query(
                TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text("session-a".to_string()),
                    SqlValue::Integer(1),
                    SqlValue::Null,
                    SqlValue::Integer(0),
                    SqlValue::Integer(10),
                    SqlValue::Integer(i64::MAX),
                    SqlValue::Text(String::new()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(10),
                ],
            )
            .await
            .unwrap();
        let mut session = Vec::new();
        while let Some(row) = session_rows.next().await.unwrap() {
            session.push((row.get(5).unwrap(), row.get(0).unwrap()));
        }
        assert_eq!(
            root, session,
            "single-session root scope must preserve session semantics"
        );
    }

    #[tokio::test]
    async fn root_record_hydration_rejects_cross_session_copy_and_assertion_traps() {
        let dir = tempdir().unwrap();
        let db = GlobalDb::try_open_at(&dir.path().join("root-record-isolation.db"))
            .await
            .expect("open database")
            .expect("database");
        insert_generation_for_session(&db, "session-a", 1).await;
        insert_generation_for_session(&db, "session-b", 1).await;
        let conn = db.read_connection();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO sessions (
                provider, session_id, project_key, project_path
             ) VALUES
                ('claude', 'session-a', 'user', '/root-record-test'),
                ('claude', 'session-b', 'user', '/root-record-test');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-shared', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                (
                    'session-a', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'source-b', 'observation-shared', 1,
                    'source-anchor-b', 'user', 4, '{\"kind\":\"unknown\"}', '{}',
                    'source', 'source'
                );
             INSERT INTO session_logical_copy_edges (
                session_id, generation, occurrence_id, copied_from_occurrence_id,
                proof_json, knowledge_at, valid_time_json, created_at
             ) VALUES (
                'session-b', 1, 'same-id', 'source-b', '{}', 5,
                '{\"kind\":\"unknown\"}', 5
             );
             INSERT INTO session_assertions (
                session_id, generation, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
             ) VALUES (
                'session-b', 1, 'assertion-b', 'supports',
                'same-anchor', 'other-anchor', 5, '{\"kind\":\"unknown\"}', '{}'
             );",
        )
        .await
        .unwrap();
        let snapshot = root_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
        let mut candidate_a = candidate_for_anchor("same-anchor");
        candidate_a.session = Some("session-a".to_string());
        let kinds_a = record_kinds(&db, &snapshot, candidate_a, &record_request()).await;
        assert_eq!(kinds_a, ["occurrence"]);

        let mut candidate_b = candidate_for_anchor("same-anchor");
        candidate_b.session = Some("session-b".to_string());
        let kinds_b = record_kinds(&db, &snapshot, candidate_b, &record_request()).await;
        assert!(kinds_b.contains(&"occurrence".to_string()));
        assert!(kinds_b.contains(&"assertion".to_string()));
        assert!(kinds_b.contains(&"copy".to_string()));
    }

    #[tokio::test]
    async fn oversized_evidence_publication_and_source_json_never_reach_record_rows() {
        let dir = tempdir().unwrap();
        let db = GlobalDb::try_open_at(&dir.path().join("oversized-records.db"))
            .await
            .expect("open database")
            .expect("database");
        let conn = db.read_connection();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-1', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );",
        )
        .await
        .unwrap();
        let oversized_json = serde_json::to_string(&"x".repeat(16 * 1024)).unwrap();
        conn.execute(
            "INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-oversized', 'observation-1',
                0, 'anchor-evidence', 'user', 1,
                '{\"kind\":\"unknown\"}', ?1, 'snippet', 'index'
             )",
            [oversized_json.clone()],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-publication', 'session-snapshot', 'anchor-publication',
                'summary', 'summary', '{}', ?1, 1
             )",
            [oversized_json],
        )
        .await
        .unwrap();
        conn.execute_batch(
            "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-publication', 0, 'anchor', 'source-short', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-publication', 'available',
                '{}', NULL, 1
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-source', 'session-snapshot', 'anchor-source',
                'summary', 'summary', '{}', NULL, 1
             );",
        )
        .await
        .unwrap();
        let oversized_anchor = format!("source-{}", "y".repeat(512));
        conn.execute(
            "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-source', 0, 'anchor', ?1, NULL)",
            [oversized_anchor],
        )
        .await
        .unwrap();
        conn.execute_batch(
            "INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-source', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .unwrap();

        let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
        let request = PageRequest::for_test(32, 4096, 128, 32, 512);
        assert!(
            !record_kinds(
                &db,
                &snapshot,
                candidate_for_anchor("anchor-evidence"),
                &request,
            )
            .await
            .contains(&"occurrence".to_string())
        );
        for anchor in ["anchor-publication", "anchor-source"] {
            assert!(
                !record_kinds(&db, &snapshot, candidate_for_anchor(anchor), &request)
                    .await
                    .contains(&"summary".to_string()),
                "oversized summary JSON for {anchor} must be rejected in its UNION arm"
            );
        }
    }

    #[tokio::test]
    async fn summary_source_count_cap_rejects_before_group_array() {
        let dir = tempdir().unwrap();
        let db = GlobalDb::try_open_at(&dir.path().join("source-count-cap.db"))
            .await
            .expect("open database")
            .expect("database");
        let conn = db.read_connection();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-many-sources', 'session-snapshot', 'anchor-many-sources',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-many-sources', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .unwrap();
        for ordinal in 0..=MAX_SUMMARY_SOURCES_PER_RECORD {
            conn.execute(
                "INSERT INTO session_summary_sources (
                    summary_id, source_ordinal, source_kind,
                    source_anchor_id, source_summary_id
                 ) VALUES ('summary-many-sources', ?1, 'anchor', ?2, NULL)",
                (
                    i64::try_from(ordinal).unwrap(),
                    format!("source-{ordinal:03}"),
                ),
            )
            .await
            .unwrap();
        }

        let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
        let request = PageRequest::for_test(32, 2 * 1024 * 1024, 1024 * 1024, 32, 512);
        let kinds = record_kinds(
            &db,
            &snapshot,
            candidate_for_anchor("anchor-many-sources"),
            &request,
        )
        .await;
        assert!(
            !kinds.contains(&"summary".to_string()),
            "257 sources must not be truncated into a 256-source summary JSON array"
        );
        let query = build_record_query(
            snapshot.retrieval_scope(),
            &snapshot,
            &[candidate_for_anchor("anchor-many-sources")],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            33,
            &request,
        )
        .unwrap();
        assert!(query.sql.contains("ss.source_ordinal < ?"));
        assert!(query.sql.contains("LIMIT 257"));
    }

    #[tokio::test]
    async fn provider_specific_summary_requires_retained_provider_evidence() {
        let dir = tempdir().unwrap();
        let db = GlobalDb::try_open_at(&dir.path().join("summary-provider.db"))
            .await
            .expect("open database")
            .expect("database");
        let conn = db.read_connection();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-claude', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-claude', 'observation-claude',
                0, 'source-claude', 'user', 1, '{\"kind\":\"unknown\"}',
                '{\"authority\":\"canonical\",\"evidence_class\":\"observed\",
                  \"source_anchor_id\":\"source-claude\",
                  \"sanitization_receipt\":{\"receipt_id\":\"receipt-1\"}}',
                'snippet', 'index'
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-provider', 'session-snapshot', 'anchor-summary-provider',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-provider', 0, 'anchor', 'source-claude', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-provider', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .unwrap();
        let request = record_request();
        let candidate = || candidate_for_anchor("anchor-summary-provider");

        let claude = record_kinds(
            &db,
            &scoped_snapshot(1, Some("claude")),
            candidate(),
            &request,
        )
        .await;
        assert!(claude.contains(&"summary".to_string()));

        let codex = record_kinds(
            &db,
            &scoped_snapshot(1, Some("codex")),
            candidate(),
            &request,
        )
        .await;
        assert!(!codex.contains(&"summary".to_string()));

        let all = record_kinds(&db, &scoped_snapshot(1, None), candidate(), &request).await;
        assert!(all.contains(&"summary".to_string()));
    }

    #[tokio::test]
    async fn explain_record_query_stays_bounded_after_hundred_thousand_candidates() {
        let total = 100_000usize;
        let start = 71_111usize;
        let end = bounded_window_end(total, start, 38);
        let candidates = (start..end)
            .map(|ordinal| RankingCandidate {
                stable_id: format!("exact:occurrence-{ordinal}"),
                anchor_id: RetrievalAnchorId::new(format!("anchor-{ordinal}")).expect("anchor"),
                retriever_record_id: format!("occurrence-{ordinal}"),
                channel: CandidateChannel::ExactMessage,
                raw_score: 1_000,
                knowledge_at_micros: 1,
                logical_message: None,
                turn: None,
                session: Some("session-snapshot".to_string()),
                source: Some("claude".to_string()),
                evidence_role: Some("user".to_string()),
            })
            .collect::<Vec<_>>();
        let request = PageRequest::for_test(37, 64 * 1024, 8 * 1024, 37, 512);
        let query = build_record_query(
            &TemporalRetrievalScope::Session(SessionId::new("session-snapshot").expect("session")),
            &scoped_snapshot(1, Some("claude")),
            &candidates,
            start,
            &RecordCursor {
                candidate: start,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            38,
            &request,
        )
        .expect("bounded record query");
        assert_eq!(candidates.len(), 38);
        assert!(query.params.len() <= candidates.len() * 3 + 14);

        let dir = tempdir().unwrap();
        let db = GlobalDb::try_open_at(&dir.path().join("record-plan.db"))
            .await
            .expect("open database")
            .expect("database");
        let explain = format!("EXPLAIN QUERY PLAN {}", query.sql);
        let mut rows = db
            .read_connection()
            .query(&explain, query.params)
            .await
            .expect("record query must parse and plan");
        let mut detail_count = 0usize;
        while rows.next().await.expect("plan row").is_some() {
            detail_count += 1;
            assert!(detail_count < 512, "record plan must remain finite");
        }
        assert!(detail_count > 0);
    }

    #[tokio::test]
    async fn explain_root_candidate_query_stays_keyset_bounded_at_hundred_thousand_rows() {
        let dir = tempdir().unwrap();
        let database = Builder::new_local(dir.path().join("root-plan.db"))
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE INDEX idx_session_temporal_generations_session_state
                ON session_temporal_generations(session_id, state);
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_root_generation_order
                ON session_occurrences(
                    knowledge_at DESC, session_id, occurrence_id, generation
                );
             INSERT INTO session_temporal_generations VALUES ('session-bulk', 1, 'active');
             INSERT INTO observations VALUES (
                'observation-bulk',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'
             );
             INSERT INTO retrieval_anchors VALUES (
                'anchor-bulk',
                '{\"kind\":\"profile\"}'
             );
             INSERT INTO sessions VALUES ('claude', 'session-bulk', 'user');
             WITH RECURSIVE sequence(value) AS (
                 VALUES(0)
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 99999
             )
             INSERT INTO session_occurrences
             SELECT 'session-bulk', 1, printf('occurrence-%06d', value),
                    'observation-bulk', 'anchor-bulk',
                    NULL, NULL, 'user', value
             FROM sequence;",
        )
        .await
        .unwrap();
        let count: i64 = conn
            .query("SELECT COUNT(*) FROM session_occurrences", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(count, 100_000);

        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {ROOT_TIME_CANDIDATE_QUERY}"),
                vec![
                    SqlValue::Text("user".to_string()),
                    SqlValue::Null,
                    SqlValue::Integer(0),
                    SqlValue::Integer(100_001),
                    SqlValue::Integer(71_111),
                    SqlValue::Text("session-bulk".to_string()),
                    SqlValue::Text("occurrence-071111".to_string()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(38),
                ],
            )
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_session_occurrences_root_generation_order"))
        );
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY"))
        );
    }

    #[test]
    fn schema_index_dependencies_are_explicit_and_follow_ups_are_not_hidden() {
        assert!(REQUIRED_SCHEMA_INDEXES.len() >= 8);
        assert_eq!(FOLLOW_UP_SCHEMA_INDEXES.len(), 5);
        assert!(
            FOLLOW_UP_SCHEMA_INDEXES
                .iter()
                .all(|index| index.contains('('))
        );
    }

    #[test]
    fn fts_values_are_bound_as_literal_phrases() {
        assert_eq!(fts_phrase("hello world"), "\"hello world\"");
        assert_eq!(fts_phrase("say \"hello\""), "\"say \"\"hello\"\"\"");
    }

    #[test]
    fn iso_day_bounds_are_micros_and_half_open() {
        let (start, end) = iso_day_bounds("2026-07-18").unwrap();
        assert_eq!(end - start, 86_400_000_000);
        assert!(iso_day_bounds("not-a-date").is_err());
    }

    #[test]
    fn record_query_has_no_offset_or_per_candidate_subqueries() {
        let source = include_str!("retrieval.rs");
        let start = source.find("fn build_record_query(").unwrap();
        let end = source[start..]
            .find("struct RecordModeSql")
            .map(|offset| start + offset)
            .unwrap();
        let builder = &source[start..end];
        assert!(!builder.to_ascii_uppercase().contains(" OFFSET "));
        assert!(!builder.contains("for candidate in candidates {\n        conn.query"));
        assert!(builder.contains("candidate_input(ordinal, session_id, anchor_id)"));
        assert!(builder.contains("ORDER BY ordinal, kind_rank, scope_session, stable_id"));
    }

    #[test]
    fn root_record_query_carries_session_identity_through_hydration() {
        let scope =
            crate::query::temporal::ports::TemporalRetrievalScope::AllSessionsInAuthorizedRoot;
        let mut candidate = record_candidate();
        candidate.session = Some("session-b".to_string());
        let query = build_record_query(
            &scope,
            &scoped_snapshot(1, None),
            &[candidate],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            33,
            &record_request(),
        )
        .expect("root record query");
        assert!(
            query
                .sql
                .contains("candidate_input(ordinal, session_id, anchor_id)")
        );
        assert!(query.sql.contains("o.session_id = c.session_id"));
        assert!(query.sql.contains("a.session_id = c.session_id"));
        assert!(query.sql.contains("target.session_id = c.session_id"));
        assert!(query.sql.contains("n.session_id = c.session_id"));
        assert!(
            query
                .sql
                .contains("source_summary.session_id = n.session_id")
        );
        assert!(
            query
                .sql
                .contains("retained_summary.session_id = n.session_id")
        );
        assert!(
            query
                .sql
                .contains("ORDER BY ordinal, kind_rank, scope_session, stable_id")
        );
        let adapter = include_str!("retrieval.rs");
        assert!(adapter.contains("fn produce_candidate_page_for_scope<'a>("));
        assert!(adapter.contains("fn produce_temporal_record_page_for_scope<'a>("));
    }
}
