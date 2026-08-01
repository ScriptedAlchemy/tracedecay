use std::cmp;

use tracedecay_domain::MAX_OBSERVATION_RECORD_BYTES;

use tracedecay_runtime_core::db::engine::Value as SqlValue;
use tracedecay_capture::parse_rfc3339_timestamp;
use tracedecay_temporal_query::candidates::{CandidateChannel, CandidateClause};
use tracedecay_temporal_query::ports::{
    CandidateFieldCaps, PageRequest, TemporalExecutionSnapshot, TemporalPortError,
    TemporalRetrievalScope,
};
use tracedecay_temporal_query::ranking::RankingCandidate;

use super::super::sql::{TemporalSqlRead, TemporalSqlRow, TemporalSqlRows};
use super::cursors::*;
use super::queries::*;
use super::rows::*;
use super::{CANDIDATE_OPERATION, RECORD_OPERATION, SNAPSHOT_OPERATION};

pub(super) fn validate_clause(
    clause: &CandidateClause,
    request: &PageRequest,
) -> Result<(), TemporalPortError> {
    let metadata_cap = request.candidate_field_caps().map_or(
        request.max_item_bytes(),
        CandidateFieldCaps::metadata_field_bytes,
    );
    if clause.value.len() > request.max_item_bytes() || clause.value.len() > metadata_cap {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "candidate clause bytes",
        });
    }
    Ok(())
}

pub(super) fn fits_bytes(
    page_bytes: usize,
    item_bytes: usize,
    bounds: PageBounds,
    item_cap: usize,
) -> bool {
    item_bytes <= item_cap
        && page_bytes
            .checked_add(item_bytes)
            .is_some_and(|total| total <= bounds.bytes)
}

pub(super) fn bounded_window_end(total: usize, start: usize, capacity: usize) -> usize {
    cmp::min(total, start.saturating_add(capacity))
}

pub(super) fn exact_match_ranges(text: &str, literal: &str) -> Vec<tracedecay_domain::ByteRangeV1> {
    if literal.is_empty() {
        return Vec::new();
    }
    text.char_indices()
        .filter(|&(start_byte, _)| text[start_byte..].starts_with(literal))
        .filter_map(|(start_byte, _)| {
            let end_byte = start_byte.checked_add(literal.len())?;
            let start = u64::try_from(start_byte).ok()?;
            let end = u64::try_from(end_byte).ok()?;
            tracedecay_domain::ByteRangeV1::new(start, end).ok()
        })
        .collect()
}

pub(super) fn authorized_root_project_key<'a>(
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

/// Root-authority shape a candidate channel is authorized through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootAuthorityChannel {
    Summary,
    Derived,
    Occurrence,
}

impl RootAuthorityChannel {
    const fn from_candidate(channel: CandidateChannel) -> Self {
        match channel {
            CandidateChannel::Summary => Self::Summary,
            CandidateChannel::Span | CandidateChannel::Burst => Self::Derived,
            CandidateChannel::Anchor
            | CandidateChannel::Scope
            | CandidateChannel::ExactMessage
            | CandidateChannel::Phrase
            | CandidateChannel::Entity
            | CandidateChannel::Time
            | CandidateChannel::Lexical => Self::Occurrence,
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Derived => "derived",
            Self::Occurrence => "occurrence",
        }
    }

    fn authorized_predicate(self, project_param: usize, provider_param: usize) -> String {
        match self {
            Self::Summary => format!(
                "EXISTS (
                     SELECT 1
                     FROM retrieval_anchors AS authority_anchor
                     JOIN session_summary_nodes AS summary
                       ON summary.summary_anchor_id = authority_anchor.anchor_id
                      AND summary.session_id = input.session_id
                      AND summary.summary_id = input.source_id
                     JOIN session_temporal_generations AS generation
                       ON generation.session_id = summary.session_id
                      AND generation.state = 'active'
                     JOIN sessions AS authority_session
                       ON authority_session.session_id = summary.session_id
                      AND authority_session.provider =
                          json_extract(summary.publication_json, '$.provider')
                      AND authority_session.project_key = ?{project_param}
                     WHERE authority_anchor.anchor_id = input.anchor_id
                       AND (
                           (authority_session.project_key = 'user'
                            AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
                           OR
                           (authority_session.project_key <> 'user'
                            AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
                            AND json_extract(authority_anchor.owner_json, '$.project_id')
                                = authority_session.project_key)
                       )
                       AND (?{provider_param} IS NULL OR EXISTS (
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
                           ) = ?{provider_param}
                           LIMIT 1
                       ))
                     LIMIT 1
                 )"
            ),
            Self::Derived => format!(
                "EXISTS (
                     SELECT 1
                     FROM retrieval_anchors AS authority_anchor
                     JOIN session_derived_evidence AS evidence
                       ON evidence.retrieval_anchor_id = authority_anchor.anchor_id
                      AND evidence.session_id = input.session_id
                      AND evidence.evidence_id = input.source_id
                     JOIN session_temporal_generations AS generation
                       ON generation.session_id = evidence.session_id
                      AND generation.generation = evidence.generation
                      AND generation.state = 'active'
                     JOIN session_occurrences AS first_occurrence
                       ON first_occurrence.session_id = evidence.session_id
                      AND first_occurrence.generation = evidence.generation
                      AND first_occurrence.occurrence_id = evidence.first_occurrence_id
                     JOIN observations AS source_observation
                       ON source_observation.observation_id =
                          first_occurrence.source_observation_id
                     JOIN sessions AS authority_session
                       ON authority_session.session_id = evidence.session_id
                      AND authority_session.provider = COALESCE(json_extract(
                          source_observation.observation_json,
                          '$.identity.source.provider'
                      ), 'claude')
                      AND authority_session.project_key = ?{project_param}
                     WHERE authority_anchor.anchor_id = input.anchor_id
                       AND (?{provider_param} IS NULL
                            OR authority_session.provider = ?{provider_param})
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
            ),
            Self::Occurrence => format!(
                "EXISTS (
                     SELECT 1
                     FROM retrieval_anchors AS authority_anchor
                     JOIN session_occurrences AS occurrence
                       ON occurrence.retrieval_anchor_id = authority_anchor.anchor_id
                      AND occurrence.session_id = input.session_id
                      AND occurrence.occurrence_id = input.source_id
                     JOIN session_temporal_generations AS generation
                       ON generation.session_id = occurrence.session_id
                      AND generation.generation = occurrence.generation
                      AND generation.state = 'active'
                     JOIN observations AS source_observation
                       ON source_observation.observation_id =
                          occurrence.source_observation_id
                     JOIN sessions AS authority_session
                       ON authority_session.session_id = occurrence.session_id
                      AND authority_session.provider = COALESCE(json_extract(
                          source_observation.observation_json,
                          '$.identity.source.provider'
                      ), 'claude')
                      AND authority_session.project_key = ?{project_param}
                     WHERE authority_anchor.anchor_id = input.anchor_id
                       AND (?{provider_param} IS NULL
                            OR authority_session.provider = ?{provider_param})
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
            ),
        }
    }
}

/// Authorization verdict for one root-wide candidate.
#[derive(Clone, Copy)]
enum RootAuthorityDecision {
    Authorized,
    OutsideRoot,
    MissingSession,
    MissingRecordIdentity,
}

/// Per-candidate root authorization for one record window, resolved together.
pub(super) struct RootAuthorityDecisions {
    decisions: Vec<RootAuthorityDecision>,
}

impl RootAuthorityDecisions {
    pub(super) fn require(&self, candidate: usize) -> Result<(), TemporalPortError> {
        match self.decisions.get(candidate) {
            Some(RootAuthorityDecision::Authorized) => Ok(()),
            Some(RootAuthorityDecision::MissingSession) => Err(read_message(
                RECORD_OPERATION,
                "root-wide candidate is missing session identity",
            )),
            Some(RootAuthorityDecision::MissingRecordIdentity) => Err(read_message(
                RECORD_OPERATION,
                "root-wide candidate is missing retriever record identity",
            )),
            Some(RootAuthorityDecision::OutsideRoot) => Err(read_message(
                RECORD_OPERATION,
                "candidate is outside the authorized root",
            )),
            None => Err(read_message(
                RECORD_OPERATION,
                "root authority did not decide the candidate",
            )),
        }
    }
}

/// Authorizes an entire root-wide candidate window in one read.
///
/// Candidates are denied unless the window query names their ordinal, so a
/// query that cannot see a candidate never widens the authorized root.
pub(super) async fn resolve_root_authority(
    conn: &TemporalSqlRead<'_>,
    candidates: &[RankingCandidate],
    project_key: &str,
    provider: Option<&str>,
) -> Result<RootAuthorityDecisions, TemporalPortError> {
    let mut decisions = vec![RootAuthorityDecision::OutsideRoot; candidates.len()];
    let mut channels: Vec<RootAuthorityChannel> = Vec::new();
    let mut values = String::new();
    let mut params = Vec::with_capacity(candidates.len().saturating_mul(5).saturating_add(2));
    for (ordinal, candidate) in candidates.iter().enumerate() {
        let Some(session_id) = candidate
            .session
            .as_deref()
            .filter(|session| !session.is_empty())
        else {
            decisions[ordinal] = RootAuthorityDecision::MissingSession;
            continue;
        };
        let source_id = candidate.retriever_record_id.as_str();
        if source_id.is_empty() {
            decisions[ordinal] = RootAuthorityDecision::MissingRecordIdentity;
            continue;
        }
        let channel = RootAuthorityChannel::from_candidate(candidate.channel);
        if !channels.contains(&channel) {
            channels.push(channel);
        }
        if !values.is_empty() {
            values.push(',');
        }
        values.push_str("(?, ?, ?, ?, ?)");
        params.push(SqlValue::Integer(
            i64::try_from(ordinal).map_err(|error| read_error(RECORD_OPERATION, error))?,
        ));
        params.push(SqlValue::Text(candidate.anchor_id.to_string()));
        params.push(SqlValue::Text(session_id.to_string()));
        params.push(SqlValue::Text(source_id.to_string()));
        params.push(SqlValue::Text(channel.tag().to_string()));
    }
    if values.is_empty() {
        return Ok(RootAuthorityDecisions { decisions });
    }
    let project_param = params.len() + 1;
    params.push(SqlValue::Text(project_key.to_string()));
    let provider_param = params.len() + 1;
    params.push(provider.map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string())));
    let authorized = channels
        .iter()
        .map(|channel| {
            format!(
                "(input.channel_kind = '{tag}' AND {predicate})",
                tag = channel.tag(),
                predicate = channel.authorized_predicate(project_param, provider_param)
            )
        })
        .collect::<Vec<_>>()
        .join("\n              OR ");
    let sql = format!(
        "WITH candidate_input(
             ordinal, anchor_id, session_id, source_id, channel_kind
         ) AS (VALUES {values})
         SELECT input.ordinal
         FROM candidate_input AS input
         WHERE {authorized}"
    );
    let mut rows = conn
        .query(&sql, params)
        .await
        .map_err(|error| read_error(RECORD_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| read_error(RECORD_OPERATION, error))?
    {
        let ordinal: i64 = row
            .get(0)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        let ordinal =
            usize::try_from(ordinal).map_err(|error| read_error(RECORD_OPERATION, error))?;
        let Some(decision) = decisions.get_mut(ordinal) else {
            return Err(read_message(
                RECORD_OPERATION,
                "root authority returned an unknown candidate ordinal",
            ));
        };
        *decision = RootAuthorityDecision::Authorized;
    }
    Ok(RootAuthorityDecisions { decisions })
}

#[cfg(test)]
pub(super) async fn require_candidate_root_authority(
    conn: &TemporalSqlRead<'_>,
    candidate: &RankingCandidate,
    project_key: &str,
    provider: Option<&str>,
) -> Result<(), TemporalPortError> {
    resolve_root_authority(conn, std::slice::from_ref(candidate), project_key, provider)
        .await?
        .require(0)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn query_candidate_clause(
    conn: &TemporalSqlRead<'_>,
    scope: &TemporalRetrievalScope,
    snapshot: &TemporalExecutionSnapshot,
    clause: &CandidateClause,
    cursor: &CandidateCursor,
    limit: usize,
    request: &PageRequest,
    root_project_key: Option<&str>,
) -> Result<TemporalSqlRows, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let generation = i64::try_from(snapshot.watermarks().generation)
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let limit = i64::try_from(limit).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let caps = request.candidate_field_caps();
    let metadata_cap = caps.map_or(
        request.max_item_bytes(),
        CandidateFieldCaps::metadata_field_bytes,
    );
    let stable_cap = caps.map_or(
        request.max_item_bytes(),
        CandidateFieldCaps::stable_id_bytes,
    );
    let source_stable_cap = stable_cap.min(metadata_cap);
    let source_stable_cap =
        i64::try_from(source_stable_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let stable_cap =
        i64::try_from(stable_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let anchor_cap = i64::try_from(caps.map_or(
        request.max_item_bytes(),
        CandidateFieldCaps::anchor_id_bytes,
    ))
    .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let metadata_cap =
        i64::try_from(metadata_cap).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let item_cap = i64::try_from(request.max_item_bytes())
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let exact_source_cap = i64::try_from(MAX_OBSERVATION_RECORD_BYTES)
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
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::Anchor) => (
            ROOT_ANCHOR_CANDIDATE_QUERY,
            vec![
                root_project_key.ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                provider,
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::AllSessionsInAuthorizedRoot, CandidateChannel::ExactMessage) => (
            ROOT_EXACT_CANDIDATE_QUERY,
            vec![
                root_project_key.clone().ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                provider,
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(stable_cap),
                SqlValue::Integer(exact_source_cap),
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
        (
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
            CandidateChannel::Span | CandidateChannel::Burst,
        ) => (
            ROOT_DERIVED_CANDIDATE_QUERY,
            vec![
                root_project_key.clone().ok_or_else(|| {
                    read_message(CANDIDATE_OPERATION, "authorized root is missing")
                })?,
                SqlValue::Text(match clause.channel {
                    CandidateChannel::Span => "span".to_string(),
                    CandidateChannel::Burst => "burst".to_string(),
                    _ => unreachable!("derived candidate channel"),
                }),
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.session_id.clone()),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::ExactMessage) => (
            EXACT_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(source_stable_cap),
                SqlValue::Integer(anchor_cap),
                SqlValue::Integer(metadata_cap),
                SqlValue::Integer(item_cap),
                SqlValue::Integer(exact_source_cap),
                SqlValue::Integer(limit),
            ],
        ),
        (TemporalRetrievalScope::Session(session_id), CandidateChannel::Anchor) => (
            ANCHOR_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                provider,
                SqlValue::Text(clause.value.clone()),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
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
        (
            TemporalRetrievalScope::Session(session_id),
            CandidateChannel::Span | CandidateChannel::Burst,
        ) => (
            DERIVED_CANDIDATE_QUERY,
            vec![
                SqlValue::Text(session_id.as_str().to_string()),
                SqlValue::Integer(generation),
                SqlValue::Text(match clause.channel {
                    CandidateChannel::Span => "span".to_string(),
                    CandidateChannel::Burst => "burst".to_string(),
                    _ => unreachable!("derived candidate channel"),
                }),
                provider,
                SqlValue::Text(fts_phrase(&clause.value)),
                SqlValue::Integer(cursor.knowledge_at),
                SqlValue::Text(cursor.stable_id.clone()),
                SqlValue::Integer(limit),
            ],
        ),
    };
    conn.query(sql, params)
        .await
        .map_err(|error| read_error(CANDIDATE_OPERATION, error))
}

pub(super) fn candidate_from_row(
    row: &TemporalSqlRow,
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
    let exact_ranges = if channel == CandidateChannel::ExactMessage {
        let snippet: String = row
            .get(8)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let literal: String = row
            .get(9)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let ranges = exact_match_ranges(&snippet, &literal);
        if ranges.is_empty() {
            return Err(read_message(
                CANDIDATE_OPERATION,
                "exact candidate row contains no exact byte range",
            ));
        }
        ranges
    } else {
        Vec::new()
    };
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
        exact_ranges,
    })
}

pub(super) const fn candidate_score(channel: CandidateChannel) -> i64 {
    match channel {
        CandidateChannel::Scope => 100,
        CandidateChannel::Anchor => 1_100,
        CandidateChannel::ExactMessage => 1_000,
        CandidateChannel::Phrase => 800,
        CandidateChannel::Span => 780,
        CandidateChannel::Burst => 760,
        CandidateChannel::Entity => 700,
        CandidateChannel::Time => 600,
        CandidateChannel::Summary => 500,
        CandidateChannel::Lexical => 400,
    }
}

pub(super) fn fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn iso_day_bounds(value: &str) -> Result<(i64, i64), TemporalPortError> {
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

pub(super) fn require_snapshot_scope(
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

pub(super) fn require_candidate_scope(
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
