pub(super) const EXACT_CANDIDATE_QUERY: &str = "
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

pub(super) const SCOPE_CANDIDATE_QUERY: &str = "
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

pub(super) const OCCURRENCE_FTS_QUERY: &str = "
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

pub(super) const TIME_CANDIDATE_QUERY: &str = "
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

pub(super) const SUMMARY_CANDIDATE_QUERY: &str = "
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

pub(super) const ROOT_EXACT_CANDIDATE_QUERY: &str = "
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

pub(super) const ROOT_OCCURRENCE_FTS_QUERY: &str = "
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

pub(super) const ROOT_TIME_CANDIDATE_QUERY: &str = "
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

pub(super) const ROOT_SUMMARY_CANDIDATE_QUERY: &str = "
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
