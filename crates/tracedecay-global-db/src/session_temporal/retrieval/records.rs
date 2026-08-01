use tracedecay_runtime_core::db::engine::Value as SqlValue;
use tracedecay_domain::TemporalModeV1;

use tracedecay_temporal_query::ports::{
    PageRequest, TemporalExecutionSnapshot, TemporalPortError, TemporalRetrievalScope,
};
use tracedecay_temporal_query::ranking::RankingCandidate;

use super::cursors::*;
use super::rows::*;
use super::{MAX_SUMMARY_SOURCES_PER_RECORD, RECORD_OPERATION};

pub(super) struct RecordQuery {
    pub(super) sql: String,
    pub(super) params: Vec<SqlValue>,
}

pub(super) fn build_record_query(
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
    let mut params = Vec::with_capacity(candidates.len().saturating_mul(5).saturating_add(14));
    let mut values = String::new();
    for (local, candidate) in candidates.iter().enumerate() {
        if local != 0 {
            values.push(',');
        }
        values.push_str("(?, ?, ?, ?, ?)");
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
        params.push(match candidate.channel {
            tracedecay_temporal_query::candidates::CandidateChannel::Span => {
                SqlValue::Text("span".to_string())
            }
            tracedecay_temporal_query::candidates::CandidateChannel::Burst => {
                SqlValue::Text("burst".to_string())
            }
            _ => SqlValue::Null,
        });
        params.push(SqlValue::Text(candidate.retriever_record_id.clone()));
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
        "WITH candidate_input(
             ordinal, session_id, anchor_id, derived_kind, retriever_record_id
         ) AS (VALUES {values}),
         candidate(
             ordinal, session_id, anchor_id, derived_kind, retriever_record_id
         ) AS (
             SELECT MIN(input.ordinal), input.session_id, input.anchor_id,
                    input.derived_kind, input.retriever_record_id
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
             GROUP BY input.session_id, input.anchor_id, input.derived_kind,
                      input.retriever_record_id
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
             WHERE c.derived_kind IS NULL
               AND {occurrence_predicate}
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
             SELECT c.ordinal, 0, o.occurrence_id, 'occurrence',
                    o.occurrence_id, o.retrieval_anchor_id, c.anchor_id,
                    o.knowledge_at, o.valid_time_json, o.evidence_json,
                    NULL, NULL, NULL, NULL, NULL, o.session_id
             FROM candidate AS c
             JOIN session_derived_evidence AS derived
               ON derived.session_id = c.session_id
              AND derived.retrieval_anchor_id = c.anchor_id
              AND derived.evidence_kind = c.derived_kind
              AND derived.evidence_id = c.retriever_record_id
             JOIN session_derived_evidence_members AS member
               ON member.session_id = derived.session_id
              AND member.generation = derived.generation
              AND member.evidence_kind = derived.evidence_kind
              AND member.evidence_id = derived.evidence_id
             JOIN session_occurrences AS o
               ON o.session_id = member.session_id
              AND o.generation = member.generation
              AND o.occurrence_id = member.occurrence_id
              {occurrence_condition}
             {occurrence_generation_join}
             JOIN observations AS occurrence_provider
               ON occurrence_provider.observation_id = o.source_observation_id
             {occurrence_join}
             WHERE c.derived_kind IS NOT NULL
               AND {occurrence_predicate}
               AND (?{provider_param} IS NULL OR COALESCE(json_extract(
                   occurrence_provider.observation_json, '$.identity.source.provider'
               ), 'claude') = ?{provider_param})
               AND length(CAST(o.occurrence_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(c.anchor_id AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.valid_time_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.evidence_json AS BLOB)) <= ?{item_cap_param}
               AND length(CAST(o.occurrence_id AS BLOB))
                   + length(CAST(o.retrieval_anchor_id AS BLOB))
                   + length(CAST(c.anchor_id AS BLOB))
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
                        WHEN ss.source_anchor_id IS NOT NULL
                             AND source_occurrence.occurrence_id IS NULL THEN 'missing'
                        WHEN ss.source_summary_id IS NOT NULL
                             AND source_summary.summary_id IS NULL THEN 'missing'
                        WHEN COALESCE(ss.source_anchor_id,
                                      source_summary.summary_anchor_id) IS NULL THEN 'missing'
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
             LEFT JOIN session_occurrences AS source_occurrence
               ON source_occurrence.session_id = n.session_id
              AND source_occurrence.generation = {summary_generation}
              AND source_occurrence.retrieval_anchor_id = ss.source_anchor_id
              AND source_occurrence.occurrence_id = (
                  SELECT historical_source.occurrence_id
                  FROM session_occurrences AS historical_source
                  WHERE historical_source.session_id = n.session_id
                    AND historical_source.generation = {summary_generation}
                    AND historical_source.retrieval_anchor_id = ss.source_anchor_id
                    AND historical_source.knowledge_at <= json_extract(
                        n.source_horizon_json, '$.knowledge_through'
                    )
                  ORDER BY historical_source.knowledge_at DESC,
                           historical_source.occurrence_id DESC
                  LIMIT 1
              )
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

impl RecordScopeSql {
    pub(super) fn new(
        scope: &TemporalRetrievalScope,
        scope_param: usize,
        generation_param: usize,
    ) -> Self {
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
            },
        }
    }
}

impl RecordModeSql {
    pub(super) fn new(mode: TemporalModeV1, cutoff_param: usize) -> Self {
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
                assertion_join: String::new(),
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
                copy_predicate: format!(
                    "e.knowledge_at <= ?{cutoff_param}
                     AND json_extract(e.valid_time_json, '$.kind') = 'known'
                     AND json_extract(e.valid_time_json, '$.valid_at') <= ?{cutoff_param}"
                ),
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

pub(super) struct RecordScopeSql {
    pub(super) occurrence_condition: String,
    pub(super) occurrence_generation_join: String,
    pub(super) assertion_condition: String,
    pub(super) assertion_generation_join: String,
    pub(super) target_condition: String,
    pub(super) target_generation_join: String,
    pub(super) summary_condition: String,
    pub(super) summary_generation_join: String,
    pub(super) availability_condition: String,
    pub(super) summary_generation: String,
}

pub(super) struct RecordModeSql {
    pub(super) occurrence_join: String,
    pub(super) occurrence_predicate: String,
    pub(super) assertion_join: String,
    pub(super) assertion_predicate: String,
    pub(super) copy_join: String,
    pub(super) copy_predicate: String,
    pub(super) summary_predicate: String,
}
