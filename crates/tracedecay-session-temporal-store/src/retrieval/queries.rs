// Candidate queries read the provider from `session_occurrences.source_provider`,
// the NOT NULL, CHECK-validated column the projection batch writes from the same
// canonical observation (and byte-verifies via `require_exact_occurrence`), so no
// query re-parses the observation JSON blob per scanned row.

// The anchor-owner authority predicate shared by every root-scope query: a
// participant is readable when its anchor owner matches the authorized root
// (profile owners under the 'user' root, project owners under project roots).
// `ROOT_SUMMARY_BROWSE_CANDIDATE_QUERY` alone additionally accepts
// session-owned anchors; that deliberate divergence is the `with_session_owner`
// arm and is documented at that query's definition.
macro_rules! anchor_owner_authority_predicate {
    () => {
        "(
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
      )"
    };
    (with_session_owner) => {
        anchor_owner_authority_predicate!(with_session_owner: "n.session_id")
    };
    (with_session_owner: $session_id:literal) => {
        concat!(
            "(
          (authority_session.project_key = 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
          OR
          (authority_session.project_key <> 'user'
           AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
           AND json_extract(authority_anchor.owner_json, '$.project_id')
               = authority_session.project_key)
          OR
          (json_extract(authority_anchor.owner_json, '$.kind') = 'session'
           AND json_extract(authority_anchor.owner_json, '$.project_key')
               = authority_session.project_key
           AND json_extract(authority_anchor.owner_json, '$.session_id') = ",
            $session_id,
            "
           AND json_extract(authority_anchor.owner_json, '$.provider')
               = authority_session.provider)
      )"
        )
    };
}

pub(super) use anchor_owner_authority_predicate;

// Summary nodes have no denormalized provider column. Publication identity
// stays in `publication_json.provider`; this is not an observations join.
macro_rules! summary_publication_provider {
    () => {
        "json_extract(n.publication_json, '$.provider')"
    };
}

// Shared per-row identity byte caps for occurrence candidate listing.
// `$provider` is `o.source_provider` on session scope and
// `authority_session.provider` on root queries (join-equality already holds).
macro_rules! occurrence_row_length_bounds {
    ($id:literal, $anchor:literal, $text:literal, $sum:literal) => {
        occurrence_row_length_bounds!($id, $anchor, $text, $sum, "o.source_provider")
    };
    ($id:literal, $anchor:literal, $text:literal, $sum:literal, $provider:literal) => {
        concat!(
            "AND length(CAST(o.occurrence_id AS BLOB)) <= ",
            $id,
            "
      AND length(CAST(o.retrieval_anchor_id AS BLOB)) <= ",
            $anchor,
            "
      AND length(CAST(COALESCE(o.message_id, '') AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(COALESCE(o.turn_id, '') AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(o.session_id AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(o.role AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(",
            $provider,
            " AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.retrieval_anchor_id AS BLOB))
          + length(CAST(COALESCE(o.message_id, '') AS BLOB))
          + length(CAST(COALESCE(o.turn_id, '') AS BLOB))
          + length(CAST(o.session_id AS BLOB))
          + length(CAST(o.role AS BLOB))
          + length(CAST(",
            $provider,
            " AS BLOB)) <= ",
            $sum
        )
    };
}

macro_rules! occurrence_keyset {
    ($time:literal, $id:literal) => {
        concat!(
            "AND (o.knowledge_at < ",
            $time,
            " OR (o.knowledge_at = ",
            $time,
            " AND o.occurrence_id > ",
            $id,
            "))"
        )
    };
}

macro_rules! occurrence_root_keyset {
    ($time:literal, $session:literal, $id:literal) => {
        concat!(
            "AND (
          o.knowledge_at < ",
            $time,
            "
          OR (
              o.knowledge_at = ",
            $time,
            "
              AND (
                  o.session_id > ",
            $session,
            "
                  OR (o.session_id = ",
            $session,
            " AND o.occurrence_id > ",
            $id,
            ")
              )
          )
      )"
        )
    };
}

macro_rules! root_occurrence_cursor_bound {
    ($cursor:literal) => {
        concat!(
            "AND length(CAST(o.occurrence_id AS BLOB))
          + length(CAST(o.session_id AS BLOB)) + 9 <= ",
            $cursor
        )
    };
}

macro_rules! summary_keyset {
    ($time:literal, $id:literal) => {
        concat!(
            "AND (n.created_at < ",
            $time,
            " OR (n.created_at = ",
            $time,
            " AND n.summary_id > ",
            $id,
            "))"
        )
    };
}

macro_rules! summary_root_keyset {
    ($time:literal, $session:literal, $id:literal) => {
        concat!(
            "AND (
          n.created_at < ",
            $time,
            "
          OR (
              n.created_at = ",
            $time,
            "
              AND (
                  n.session_id > ",
            $session,
            "
                  OR (n.session_id = ",
            $session,
            " AND n.summary_id > ",
            $id,
            ")
              )
          )
      )"
        )
    };
}

// Session-scope summary identity caps. Publication provider stays in JSON.
macro_rules! summary_row_length_bounds {
    ($id:literal, $anchor:literal, $text:literal, $sum:literal) => {
        concat!(
            "AND length(CAST(n.summary_id AS BLOB)) <= ",
            $id,
            "
      AND length(CAST(n.summary_anchor_id AS BLOB)) <= ",
            $anchor,
            "
      AND length(CAST(n.session_id AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(COALESCE(
          ",
            summary_publication_provider!(),
            ", ''
      ) AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.summary_anchor_id AS BLOB))
          + length(CAST(n.session_id AS BLOB))
          + length(CAST(COALESCE(
              ",
            summary_publication_provider!(),
            ", ''
          ) AS BLOB)) <= ",
            $sum
        )
    };
    (root: $id:literal, $anchor:literal, $text:literal, $sum:literal, $cursor:literal) => {
        concat!(
            "AND length(CAST(n.summary_id AS BLOB)) <= ",
            $id,
            "
      AND length(CAST(n.summary_anchor_id AS BLOB)) <= ",
            $anchor,
            "
      AND length(CAST(n.session_id AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(authority_session.provider AS BLOB)) <= ",
            $text,
            "
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.summary_anchor_id AS BLOB))
          + length(CAST(n.session_id AS BLOB))
          + length(CAST(authority_session.provider AS BLOB)) <= ",
            $sum,
            "
      AND length(CAST(n.summary_id AS BLOB))
          + length(CAST(n.session_id AS BLOB)) + 9 <= ",
            $cursor
        )
    };
}

macro_rules! derived_keyset {
    ($time:literal, $id:literal) => {
        concat!(
            "AND (
          first_occurrence.knowledge_at < ",
            $time,
            "
          OR (
              first_occurrence.knowledge_at = ",
            $time,
            "
              AND evidence.evidence_id > ",
            $id,
            "
          )
      )"
        )
    };
}

macro_rules! derived_root_keyset {
    ($time:literal, $session:literal, $id:literal) => {
        concat!(
            "AND (
          first_occurrence.knowledge_at < ",
            $time,
            "
          OR (
              first_occurrence.knowledge_at = ",
            $time,
            "
              AND (
                  evidence.session_id > ",
            $session,
            "
                  OR (
                      evidence.session_id = ",
            $session,
            "
                      AND evidence.evidence_id > ",
            $id,
            "
                  )
              )
          )
      )"
        )
    };
}

pub(super) const EXACT_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           o.source_provider,
           o.snippet_text, ?4
    FROM session_occurrences AS o
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR o.source_provider = ?3)
      AND instr(o.snippet_text, ?4) > 0
      ",
    occurrence_keyset!("?5", "?6"),
    "
      ",
    occurrence_row_length_bounds!("?7", "?8", "?9", "?10"),
    "
      AND length(CAST(o.snippet_text AS BLOB)) <= ?11
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?12"
);

pub(super) const SCOPE_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           o.source_provider
    FROM session_occurrences AS o
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR o.source_provider = ?3)
      ",
    occurrence_keyset!("?4", "?5"),
    "
      ",
    occurrence_row_length_bounds!("?6", "?7", "?8", "?9"),
    "
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?10"
);

// The scope browse's summary listing: the session's published summary nodes
// in creation order, carried on the Summary channel so hydration routes them
// through the summary reader. Without this clause an empty-query browse would
// be summary-blind while the participant's summary frontier says otherwise.
pub(super) const SUMMARY_BROWSE_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           ",
    summary_publication_provider!(),
    "
    FROM session_summary_nodes AS n
    JOIN session_summary_availability AS a
      ON a.summary_id = n.summary_id
     AND a.session_id = ?1
     AND a.generation = ?2
    WHERE n.session_id = ?1
      AND (?3 IS NULL OR ",
    summary_publication_provider!(),
    " = ?3)
      AND a.availability <> 'unavailable'
      AND (?11 <> 'current' OR NOT EXISTS (
          SELECT 1
          FROM lcm_summary_convergence_dirty_raw AS dirty
          WHERE dirty.provider = ",
    summary_publication_provider!(),
    "
            AND dirty.session_id = n.session_id
      ))
      ",
    summary_keyset!("?4", "?5"),
    "
      ",
    summary_row_length_bounds!("?6", "?7", "?8", "?9"),
    "
    ORDER BY n.created_at DESC, n.summary_id
    LIMIT ?10"
);

pub(super) const ANCHOR_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT stable_id, anchor_id, knowledge_at, logical_message, turn_id, session_id,
           evidence_role, provider
    FROM (
        SELECT o.occurrence_id AS stable_id, o.retrieval_anchor_id AS anchor_id,
               o.knowledge_at AS knowledge_at, o.message_id AS logical_message,
               o.turn_id AS turn_id, o.session_id AS session_id, o.role AS evidence_role,
               o.source_provider AS provider
        FROM session_occurrences AS o
        WHERE o.session_id = ?1 AND o.generation = ?2
          AND (?3 IS NULL OR o.source_provider = ?3)
          AND o.retrieval_anchor_id = ?4
        UNION ALL
        SELECT n.summary_id, n.summary_anchor_id, n.created_at, NULL, NULL, n.session_id,
               'summary', ",
    summary_publication_provider!(),
    "
        FROM session_summary_nodes AS n
        WHERE n.session_id = ?1
          AND (?3 IS NULL OR ",
    summary_publication_provider!(),
    " = ?3)
          AND n.summary_anchor_id = ?4
          AND (?8 <> 'current' OR NOT EXISTS (
              SELECT 1
              FROM lcm_summary_convergence_dirty_raw AS dirty
              WHERE dirty.provider = ",
    summary_publication_provider!(),
    "
                AND dirty.session_id = n.session_id
          ))
    )
    WHERE knowledge_at < ?5 OR (knowledge_at = ?5 AND stable_id > ?6)
    ORDER BY knowledge_at DESC, stable_id
    LIMIT ?7"
);

pub(super) const ROOT_ANCHOR_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role, authority_session.provider,
           frozen.generation
    FROM session_temporal_generations AS frozen
    JOIN session_occurrences AS o
      ON o.session_id = frozen.session_id
     AND o.generation = frozen.generation
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = o.source_provider
     AND authority_session.project_key = ?1
    WHERE frozen.state = 'active'
      AND (?2 IS NULL OR authority_session.provider = ?2)
      AND o.retrieval_anchor_id = ?3
      AND ",
    anchor_owner_authority_predicate!(),
    "
      ",
    occurrence_root_keyset!("?4", "?5", "?6"),
    "
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?7"
);

pub(super) const OCCURRENCE_FTS_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           o.source_provider
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR o.source_provider = ?3)
      AND session_occurrences_fts MATCH ?4
      ",
    occurrence_keyset!("?5", "?6"),
    "
      ",
    occurrence_row_length_bounds!("?7", "?8", "?9", "?10"),
    "
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?11"
);

pub(super) const TIME_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           o.source_provider
    FROM session_occurrences AS o INDEXED BY idx_session_occurrences_generation_order
    WHERE o.session_id = ?1 AND o.generation = ?2
      AND (?3 IS NULL OR o.source_provider = ?3)
      AND o.knowledge_at >= ?4 AND o.knowledge_at < ?5
      ",
    occurrence_keyset!("?6", "?7"),
    "
      ",
    occurrence_row_length_bounds!("?8", "?9", "?10", "?11"),
    "
    ORDER BY o.knowledge_at DESC, o.occurrence_id
    LIMIT ?12"
);

pub(super) const SUMMARY_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           ",
    summary_publication_provider!(),
    "
    FROM session_summary_nodes_fts
    JOIN session_summary_nodes AS n ON n.rowid = session_summary_nodes_fts.rowid
    JOIN session_summary_availability AS a
      ON a.summary_id = n.summary_id
     AND a.session_id = ?1
     AND a.generation = ?2
    WHERE n.session_id = ?1
      AND session_summary_nodes_fts MATCH ?3
      AND a.availability <> 'unavailable'
      AND (?11 <> 'current' OR NOT EXISTS (
          SELECT 1
          FROM lcm_summary_convergence_dirty_raw AS dirty
          WHERE dirty.provider = ",
    summary_publication_provider!(),
    "
            AND dirty.session_id = n.session_id
      ))
      ",
    summary_keyset!("?4", "?5"),
    "
      ",
    summary_row_length_bounds!("?6", "?7", "?8", "?9"),
    "
    ORDER BY n.created_at DESC, n.summary_id
    LIMIT ?10"
);

pub(super) const ROOT_EXACT_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider, o.snippet_text, ?3, frozen.generation
    FROM session_occurrences AS o
    JOIN session_temporal_generations AS frozen
      ON frozen.session_id = o.session_id
     AND frozen.generation = o.generation
     AND frozen.state = 'active'
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = o.source_provider
     AND authority_session.project_key = ?1
    WHERE ",
    anchor_owner_authority_predicate!(),
    "
      AND (?2 IS NULL OR o.source_provider = ?2)
      AND instr(o.snippet_text, ?3) > 0
      ",
    occurrence_root_keyset!("?4", "?5", "?6"),
    "
      ",
    occurrence_row_length_bounds!("?7", "?8", "?9", "?10", "authority_session.provider"),
    "
      AND length(CAST(o.snippet_text AS BLOB)) <= ?12
      ",
    root_occurrence_cursor_bound!("?11"),
    "
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?13"
);

pub(super) const ROOT_OCCURRENCE_FTS_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider, frozen.generation
    FROM session_occurrences_fts
    JOIN session_occurrences AS o ON o.rowid = session_occurrences_fts.rowid
    JOIN session_temporal_generations AS frozen
      ON frozen.session_id = o.session_id
     AND frozen.generation = o.generation
     AND frozen.state = 'active'
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = o.source_provider
     AND authority_session.project_key = ?1
    WHERE ",
    anchor_owner_authority_predicate!(),
    "
      AND (?2 IS NULL OR o.source_provider = ?2)
      AND session_occurrences_fts MATCH ?3
      ",
    occurrence_root_keyset!("?4", "?5", "?6"),
    "
      ",
    occurrence_row_length_bounds!("?7", "?8", "?9", "?10", "authority_session.provider"),
    "
      ",
    root_occurrence_cursor_bound!("?11"),
    "
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?12"
);

pub(super) const ROOT_TIME_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT o.occurrence_id, o.retrieval_anchor_id, o.knowledge_at,
           o.message_id, o.turn_id, o.session_id, o.role,
           authority_session.provider, frozen.generation
    FROM session_temporal_generations AS frozen
    JOIN session_occurrences AS o
      INDEXED BY idx_session_occurrences_root_generation_order
      ON o.session_id = frozen.session_id
     AND o.generation = frozen.generation
    JOIN retrieval_anchors AS authority_anchor
      ON authority_anchor.anchor_id = o.retrieval_anchor_id
    JOIN sessions AS authority_session
      ON authority_session.session_id = o.session_id
     AND authority_session.provider = o.source_provider
     AND authority_session.project_key = ?1
    WHERE frozen.state = 'active'
      AND ",
    anchor_owner_authority_predicate!(),
    "
      AND (?2 IS NULL OR o.source_provider = ?2)
      AND o.knowledge_at >= ?3 AND o.knowledge_at < ?4
      ",
    occurrence_root_keyset!("?5", "?6", "?7"),
    "
      ",
    occurrence_row_length_bounds!("?8", "?9", "?10", "?11", "authority_session.provider"),
    "
      ",
    root_occurrence_cursor_bound!("?12"),
    "
    ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
    LIMIT ?13"
);

pub(super) const ROOT_SUMMARY_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           authority_session.provider, frozen.generation
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
     AND authority_session.provider = ",
    summary_publication_provider!(),
    "
     AND authority_session.project_key = ?1
    WHERE ",
    anchor_owner_authority_predicate!(),
    "
      AND session_summary_nodes_fts MATCH ?2
      AND a.availability <> 'unavailable'
      AND (?12 <> 'current' OR NOT EXISTS (
          SELECT 1
          FROM lcm_summary_convergence_dirty_raw AS dirty
          WHERE dirty.provider = authority_session.provider
            AND dirty.session_id = n.session_id
      ))
      ",
    summary_root_keyset!("?3", "?4", "?5"),
    "
      ",
    summary_row_length_bounds!(root: "?6", "?7", "?8", "?9", "?10"),
    "
    ORDER BY n.created_at DESC, n.session_id, n.summary_id
    LIMIT ?11"
);

// The root browse's summary listing across every participant with an active
// generation, carried on the Summary channel behind the anchor-owner
// authority predicate (the same shape as `ROOT_SUMMARY_CANDIDATE_QUERY`
// without its full-text filter, plus a third session-owner arm this listing
// alone accepts).
pub(super) const ROOT_SUMMARY_BROWSE_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT n.summary_id, n.summary_anchor_id, n.created_at,
           NULL, NULL, n.session_id, 'summary',
           authority_session.provider, frozen.generation
    FROM session_summary_nodes AS n
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
     AND authority_session.provider = ",
    summary_publication_provider!(),
    "
     AND authority_session.project_key = ?1
    WHERE ",
    anchor_owner_authority_predicate!(with_session_owner),
    "
      AND (?2 IS NULL OR authority_session.provider = ?2)
      AND a.availability <> 'unavailable'
      AND (?12 <> 'current' OR NOT EXISTS (
          SELECT 1
          FROM lcm_summary_convergence_dirty_raw AS dirty
          WHERE dirty.provider = authority_session.provider
            AND dirty.session_id = n.session_id
      ))
      ",
    summary_root_keyset!("?3", "?4", "?5"),
    "
      ",
    summary_row_length_bounds!(root: "?6", "?7", "?8", "?9", "?10"),
    "
    ORDER BY n.created_at DESC, n.session_id, n.summary_id
    LIMIT ?11"
);

pub(super) const DERIVED_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT evidence.evidence_id, evidence.retrieval_anchor_id,
           first_occurrence.knowledge_at,
           CASE WHEN evidence.member_count = 1
                THEN first_occurrence.message_id ELSE NULL END,
           NULL, evidence.session_id, evidence.evidence_kind,
           first_occurrence.source_provider
    FROM session_derived_evidence AS evidence
    JOIN session_occurrences AS first_occurrence
      ON first_occurrence.session_id = evidence.session_id
     AND first_occurrence.generation = evidence.generation
     AND first_occurrence.occurrence_id = evidence.first_occurrence_id
    WHERE evidence.session_id = ?1 AND evidence.generation = ?2
      AND evidence.evidence_kind = ?3
      AND (?4 IS NULL OR first_occurrence.source_provider = ?4)
      AND EXISTS (
          SELECT 1
          FROM session_derived_evidence_members AS member
          JOIN session_occurrences AS member_occurrence
            ON member_occurrence.session_id = member.session_id
           AND member_occurrence.generation = member.generation
           AND member_occurrence.occurrence_id = member.occurrence_id
          JOIN session_occurrences_fts
            ON session_occurrences_fts.rowid = member_occurrence.rowid
          WHERE member.session_id = evidence.session_id
            AND member.generation = evidence.generation
            AND member.evidence_kind = evidence.evidence_kind
            AND member.evidence_id = evidence.evidence_id
            AND session_occurrences_fts MATCH ?5
      )
      ",
    derived_keyset!("?6", "?7"),
    "
    ORDER BY first_occurrence.knowledge_at DESC, evidence.evidence_id
    LIMIT ?8"
);

// Fresh stores have no planner statistics. CROSS JOIN pins authorized sessions
// as the outer loop so SQLite probes evidence by session/generation/kind instead
// of scanning every evidence row before applying the root boundary.
pub(super) const ROOT_DERIVED_CANDIDATE_QUERY: &str = concat!(
    "
    SELECT evidence.evidence_id, evidence.retrieval_anchor_id,
           first_occurrence.knowledge_at,
           CASE WHEN evidence.member_count = 1
                THEN first_occurrence.message_id ELSE NULL END,
           NULL, evidence.session_id, evidence.evidence_kind,
           authority_session.provider, frozen.generation
    FROM sessions AS authority_session
    CROSS JOIN session_temporal_generations AS frozen
    CROSS JOIN session_derived_evidence AS evidence
    CROSS JOIN session_occurrences AS first_occurrence
    CROSS JOIN retrieval_anchors AS authority_anchor
    WHERE authority_session.project_key = ?1
      AND (?3 IS NULL OR authority_session.provider = ?3)
      AND frozen.session_id = authority_session.session_id
      AND frozen.state = 'active'
      AND evidence.session_id = frozen.session_id
      AND evidence.generation = frozen.generation
      AND evidence.evidence_kind = ?2
      AND first_occurrence.session_id = evidence.session_id
      AND first_occurrence.generation = evidence.generation
      AND first_occurrence.occurrence_id = evidence.first_occurrence_id
      AND authority_anchor.anchor_id = evidence.retrieval_anchor_id
      AND authority_session.provider = first_occurrence.source_provider
      AND EXISTS (
          SELECT 1
          FROM session_derived_evidence_members AS member
          JOIN session_occurrences AS member_occurrence
            ON member_occurrence.session_id = member.session_id
           AND member_occurrence.generation = member.generation
           AND member_occurrence.occurrence_id = member.occurrence_id
          JOIN session_occurrences_fts
            ON session_occurrences_fts.rowid = member_occurrence.rowid
          WHERE member.session_id = evidence.session_id
            AND member.generation = evidence.generation
            AND member.evidence_kind = evidence.evidence_kind
            AND member.evidence_id = evidence.evidence_id
            AND session_occurrences_fts MATCH ?4
      )
      AND ",
    anchor_owner_authority_predicate!(),
    "
      ",
    derived_root_keyset!("?5", "?6", "?7"),
    "
    ORDER BY first_occurrence.knowledge_at DESC, evidence.session_id, evidence.evidence_id
    LIMIT ?8"
);
