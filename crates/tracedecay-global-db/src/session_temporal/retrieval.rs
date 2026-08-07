use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::{Value, params};

use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationFactV1,
    CanonicalWorkflowSemanticKindV1, DurableObservationV1, RetrievalAnchorId, SessionId,
};
use tracedecay_runtime_core::db::engine;
use tracedecay_temporal_query::candidates::{CandidateChannel, CandidatePlan};
use tracedecay_temporal_query::ports::{
    CandidatePageSink, MeasuredTemporalValue, PageRequest, PageStatus, PortFuture,
    TemporalCandidateFilterV1, TemporalExecutionSnapshot, TemporalMessageTypeFilterV1,
    TemporalPortError, TemporalReadPort, TemporalRecordPageSink, TemporalRetrievalScope,
    TemporalSessionScopeFilterV1,
};
use tracedecay_temporal_query::ranking::RankingCandidate;

mod candidates;
mod cursors;
#[cfg(test)]
#[path = "retrieval/graph_relation_tests.rs"]
mod graph_relation_tests;
mod queries;
mod records;
mod rows;
#[cfg(test)]
mod semantic_filter_tests;
#[cfg(test)]
mod tests;

use super::relations::{
    SessionRelationError, SessionRelationGraphStore, SessionRelationScope, SummarySourceVisitKind,
};
use super::sql::{TemporalSqlRead, TemporalSqlRows};
use super::store::execution_control_graph_cancellation;
use candidates::*;
use cursors::*;
use records::*;
use rows::*;

pub const CANDIDATE_OPERATION: &str = "read temporal candidates";
pub const RECORD_OPERATION: &str = "read temporal records";
pub const SNAPSHOT_OPERATION: &str = "validate temporal read snapshot";
pub const MIN_CURSOR_CAPACITY: usize = 96;
pub const MAX_SUMMARY_SOURCES_PER_RECORD: usize = 256;
const MAX_SESSION_CONTEXT_RELATIONS: usize = 256;
const FILTER_SCAN_PAGE_ITEMS: usize = 64;
const MAX_RECORD_QUERY_CANDIDATES: usize = 8;

fn temporal_relation_error(
    error: SessionRelationError,
    control: &tracedecay_temporal_query::ports::ExecutionControl,
    resource: &'static str,
) -> TemporalPortError {
    if error == SessionRelationError::Cancelled
        && let Err(control_error) = control.checkpoint()
    {
        return control_error;
    }
    match error {
        SessionRelationError::BudgetExhausted => TemporalPortError::BudgetExceeded { resource },
        SessionRelationError::Cancelled => TemporalPortError::Cancelled,
        SessionRelationError::DeadlineExceeded => TemporalPortError::DeadlineExceeded,
        SessionRelationError::Invalid
        | SessionRelationError::Cycle
        | SessionRelationError::Conflict
        | SessionRelationError::Corrupt => read_message(
            CANDIDATE_OPERATION,
            "session relation projection is invalid",
        ),
        SessionRelationError::ResetRequired => read_message(
            CANDIDATE_OPERATION,
            "session relation projection requires reset",
        ),
        SessionRelationError::DurabilityUncertain => read_message(
            CANDIDATE_OPERATION,
            "session relation projection durability is uncertain",
        ),
        SessionRelationError::NotFound
        | SessionRelationError::Unavailable
        | SessionRelationError::Storage(_) => read_message(
            CANDIDATE_OPERATION,
            "session relation projection is unavailable",
        ),
    }
}

fn observation_matches_filter(
    encoded: &str,
    occurrence_role: &str,
    filter: &TemporalCandidateFilterV1,
) -> Result<bool, TemporalPortError> {
    if !filter.roles.is_empty()
        && filter
            .roles
            .binary_search_by(|role| role.as_str().cmp(occurrence_role))
            .is_err()
    {
        return Ok(false);
    }
    let observation: DurableObservationV1 =
        serde_json::from_str(encoded).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    if let Some(source) = filter.source.as_deref()
        && observation.source().provider().as_str() != source
        && observation.source().source_key().as_str() != source
    {
        return Ok(false);
    }
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
    let is_goal = envelope.facts().iter().any(|fact| {
        matches!(
            fact,
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
                ..
            }
        )
    });
    if filter.goals && !is_goal {
        return Ok(false);
    }
    let has_tool_result = envelope
        .facts()
        .iter()
        .any(|fact| matches!(fact, CanonicalObservationFactV1::ToolResult { .. }));
    let message = envelope.facts().iter().find_map(|fact| match fact {
        CanonicalObservationFactV1::Message {
            role, timestamp, ..
        } => Some((*role, *timestamp)),
        _ => None,
    });
    match filter.message_type {
        TemporalMessageTypeFilterV1::All => {}
        TemporalMessageTypeFilterV1::DirectUser
            if has_tool_result
                || !message.is_some_and(|(role, _)| role == CanonicalMessageRoleV1::User) =>
        {
            return Ok(false);
        }
        TemporalMessageTypeFilterV1::ToolResult
            if !has_tool_result
                && !message.is_some_and(|(role, _)| role == CanonicalMessageRoleV1::Tool) =>
        {
            return Ok(false);
        }
        TemporalMessageTypeFilterV1::DirectUser | TemporalMessageTypeFilterV1::ToolResult => {}
    }
    let timestamp = message.and_then(|(_, timestamp)| timestamp).or_else(|| {
        is_goal
            .then(|| envelope.evidence().native_timestamp())
            .flatten()
    });
    Ok(filter
        .start_time
        .is_none_or(|start| timestamp.is_some_and(|value| value >= start))
        && filter
            .end_time
            .is_none_or(|end| timestamp.is_some_and(|value| value <= end)))
}

fn participant_generation(
    snapshot: &TemporalExecutionSnapshot,
    session_id: &SessionId,
    source: &str,
) -> Result<u64, TemporalPortError> {
    if !snapshot.has_authoritative_participant_manifest() {
        return Ok(snapshot.watermarks().generation);
    }
    snapshot
        .participant_manifest()
        .entries()
        .iter()
        .find(|participant| {
            participant.session_id() == session_id && participant.source_id() == source
        })
        .map(tracedecay_temporal_query::ports::TemporalParticipantGeneration::generation)
        .ok_or_else(|| {
            read_message(
                CANDIDATE_OPERATION,
                "candidate is absent from the frozen participant manifest",
            )
        })
}

/// Borrowed read-only adapter over one authoritative database snapshot.
pub struct GlobalDbTemporalReadPort<'a> {
    read: TemporalSqlRead<'a>,
    relation_authority: Option<SessionReadRelationAuthority<'a>>,
    git_scope_session_ids: Option<&'a BTreeSet<(String, String)>>,
}

struct SessionReadRelationAuthority<'a> {
    scope: &'a SessionRelationScope,
    store: SessionRelationGraphStore,
}

impl<'a> GlobalDbTemporalReadPort<'a> {
    #[cfg(test)]
    pub const fn new(read: &'a engine::Connection) -> Self {
        Self {
            read: TemporalSqlRead::engine_connection(read),
            relation_authority: None,
            git_scope_session_ids: None,
        }
    }

    #[cfg(test)]
    pub const fn new_registered(read: &'a engine::ReadSnapshot) -> Self {
        Self {
            read: TemporalSqlRead::registered(read),
            relation_authority: None,
            git_scope_session_ids: None,
        }
    }

    #[cfg(test)]
    pub const fn new_with_relations(
        read: &'a engine::Connection,
        scope: &'a SessionRelationScope,
        store: SessionRelationGraphStore,
    ) -> Self {
        Self {
            read: TemporalSqlRead::engine_connection(read),
            relation_authority: Some(SessionReadRelationAuthority { scope, store }),
            git_scope_session_ids: None,
        }
    }

    pub const fn new_registered_with_relations(
        read: &'a engine::ReadSnapshot,
        scope: &'a SessionRelationScope,
        store: SessionRelationGraphStore,
    ) -> Self {
        Self {
            read: TemporalSqlRead::registered(read),
            relation_authority: Some(SessionReadRelationAuthority { scope, store }),
            git_scope_session_ids: None,
        }
    }

    async fn candidate_matches_filter(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        candidate: &RankingCandidate,
        filter: &TemporalCandidateFilterV1,
    ) -> Result<bool, TemporalPortError> {
        if candidate.channel == CandidateChannel::Summary
            && (!filter.include_summaries
                || !filter.roles.is_empty()
                || filter.message_type != TemporalMessageTypeFilterV1::All
                || filter.start_time.is_some()
                || filter.end_time.is_some()
                || filter.goals)
        {
            return Ok(false);
        }
        if filter.is_empty() {
            return Ok(true);
        }
        let session_id = candidate
            .session
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "candidate session is missing"))?;
        let provider = candidate
            .source
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "candidate provider is missing"))?;
        if !self
            .session_matches_filter(snapshot, session_id, provider, filter)
            .await?
        {
            return Ok(false);
        }
        if !filter.goals
            && filter.source.is_none()
            && filter.roles.is_empty()
            && filter.message_type == TemporalMessageTypeFilterV1::All
            && filter.start_time.is_none()
            && filter.end_time.is_none()
        {
            return Ok(true);
        }
        self.candidate_observations_match(candidate, filter, snapshot)
            .await
    }

    async fn session_matches_filter(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        session_id: &str,
        provider: &str,
        filter: &TemporalCandidateFilterV1,
    ) -> Result<bool, TemporalPortError> {
        if filter.git_branch.is_some()
            || filter.git_worktree.is_some()
            || filter.git_commit.is_some()
        {
            let selected = self.git_scope_session_ids.ok_or_else(|| {
                read_message(
                    CANDIDATE_OPERATION,
                    "Git scope session authority is unavailable",
                )
            })?;
            if !selected
                .iter()
                .any(|(selected_provider, selected_session_id)| {
                    selected_provider == provider && selected_session_id == session_id
                })
            {
                return Ok(false);
            }
        }
        let mut sql = "SELECT EXISTS (
            SELECT 1 FROM sessions AS s
            WHERE s.session_id = ?1 AND s.provider = ?2"
            .to_string();
        let mut values = vec![
            Value::Text(session_id.to_string()),
            Value::Text(provider.to_string()),
        ];
        let mut bind = |value: String| {
            values.push(Value::Text(value));
            values.len()
        };
        if let Some(project_key) = &filter.project_key {
            let index = bind(project_key.clone());
            let _ = std::fmt::Write::write_fmt(
                &mut sql,
                format_args!(" AND (s.project_key = ?{index} OR s.project_path = ?{index})"),
            );
        }
        if let Some(parent) = &filter.parent_session_id {
            let index = bind(parent.clone());
            let _ = std::fmt::Write::write_fmt(
                &mut sql,
                format_args!(" AND s.parent_session_id = ?{index}"),
            );
        }
        match filter.session_scope {
            TemporalSessionScopeFilterV1::All => {}
            TemporalSessionScopeFilterV1::ParentsOnly => sql.push_str(" AND s.is_subagent = 0"),
            TemporalSessionScopeFilterV1::SubagentsOnly => sql.push_str(" AND s.is_subagent <> 0"),
        }
        if let Some(run_id) = &filter.workflow_run {
            let run = bind(run_id.clone());
            let _ = std::fmt::Write::write_fmt(
                &mut sql,
                format_args!(
                    " AND EXISTS (SELECT 1 FROM workflow_agents wa \
                     WHERE wa.run_id = ?{run} \
                       AND (wa.agent_session_id = s.session_id \
                            OR wa.transcript_path = s.transcript_path)"
                ),
            );
            if let Some(agent) = &filter.workflow_agent {
                let agent = bind(agent.clone());
                let _ = std::fmt::Write::write_fmt(
                    &mut sql,
                    format_args!(" AND wa.agent_label = ?{agent}"),
                );
            }
            sql.push(')');
        }
        sql.push_str(" LIMIT 1)");
        let mut rows = self
            .read
            .query(&sql, values)
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let matched = rows
            .next()
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "filter query returned no row"))?
            .get::<i64>(0)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        if matched != 1 {
            return Ok(false);
        }
        let requires_context = filter.parent_session_id.is_some()
            || filter.session_scope != TemporalSessionScopeFilterV1::All
            || filter.workflow_run.is_some();
        if !requires_context {
            return Ok(true);
        }
        let authority = self.relation_authority.as_ref().ok_or_else(|| {
            read_message(
                CANDIDATE_OPERATION,
                "mounted session relation graph is unavailable",
            )
        })?;
        let session_id =
            SessionId::new(session_id).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let generation = participant_generation(snapshot, &session_id, provider)?;
        let control = snapshot.request().execution_control();
        control.checkpoint()?;
        let context = authority
            .store
            .session_context(
                authority.scope,
                &session_id,
                generation,
                MAX_SESSION_CONTEXT_RELATIONS,
                execution_control_graph_cancellation(control),
            )
            .map_err(|error| {
                temporal_relation_error(error, control, "session context relations")
            })?;
        control.checkpoint()?;
        if filter.parent_session_id.as_deref().is_some_and(|parent| {
            context
                .parent_session_id
                .as_ref()
                .is_none_or(|actual| actual.as_str() != parent)
        }) {
            return Ok(false);
        }
        match filter.session_scope {
            TemporalSessionScopeFilterV1::All => {}
            TemporalSessionScopeFilterV1::ParentsOnly if context.parent_session_id.is_some() => {
                return Ok(false);
            }
            TemporalSessionScopeFilterV1::SubagentsOnly if context.parent_session_id.is_none() => {
                return Ok(false);
            }
            TemporalSessionScopeFilterV1::ParentsOnly
            | TemporalSessionScopeFilterV1::SubagentsOnly => {}
        }
        if let Some(run_id) = filter.workflow_run.as_deref()
            && !context.workflow_agents.iter().any(|membership| {
                membership.run_id == run_id
                    && filter
                        .workflow_agent
                        .as_deref()
                        .is_none_or(|agent| membership.agent_label == agent)
            })
        {
            return Ok(false);
        }
        Ok(true)
    }

    async fn candidate_observations_match(
        &self,
        candidate: &RankingCandidate,
        filter: &TemporalCandidateFilterV1,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<bool, TemporalPortError> {
        let session_id = candidate
            .session
            .as_deref()
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "candidate session is missing"))?;
        let source = candidate
            .source
            .as_deref()
            .filter(|source| !source.is_empty())
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "candidate provider is missing"))?;
        let typed_session_id =
            SessionId::new(session_id).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let generation =
            i64::try_from(participant_generation(snapshot, &typed_session_id, source)?)
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let common_values = || {
            vec![
                Value::Text(session_id.to_string()),
                Value::Text(candidate.retriever_record_id.clone()),
                Value::Text(candidate.anchor_id.to_string()),
                Value::Integer(generation),
            ]
        };
        let (sql, values) = match candidate.channel {
            CandidateChannel::Span | CandidateChannel::Burst => (
                "SELECT observation.observation_json, occurrence.role
                 FROM session_derived_evidence evidence
                 JOIN session_derived_evidence_members member
                   ON member.session_id = evidence.session_id
                  AND member.generation = evidence.generation
                  AND member.evidence_kind = evidence.evidence_kind
                  AND member.evidence_id = evidence.evidence_id
                 JOIN session_occurrences occurrence
                   ON occurrence.session_id = member.session_id
                  AND occurrence.generation = member.generation
                  AND occurrence.occurrence_id = member.occurrence_id
                 JOIN observations observation
                   ON observation.observation_id = occurrence.source_observation_id
                 WHERE evidence.session_id = ?1
                   AND evidence.evidence_id = ?2
                   AND evidence.retrieval_anchor_id = ?3
                   AND evidence.generation = ?4
                 ORDER BY member.ordinal
                 LIMIT 257",
                common_values(),
            ),
            CandidateChannel::Summary => {
                return self
                    .summary_observations_match(candidate, filter, snapshot)
                    .await;
            }
            CandidateChannel::Anchor
            | CandidateChannel::Scope
            | CandidateChannel::ExactMessage
            | CandidateChannel::Phrase
            | CandidateChannel::Entity
            | CandidateChannel::Time
            | CandidateChannel::Lexical => (
                "SELECT observation.observation_json, occurrence.role
                 FROM session_occurrences occurrence
                 JOIN observations observation
                   ON observation.observation_id = occurrence.source_observation_id
                 WHERE occurrence.session_id = ?1
                   AND occurrence.occurrence_id = ?2
                   AND occurrence.retrieval_anchor_id = ?3
                   AND occurrence.generation = ?4
                 LIMIT 2",
                common_values(),
            ),
        };
        let mut rows = self
            .read
            .query(sql, values)
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let mut count = 0usize;
        let mut matched = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
        {
            count += 1;
            if count > MAX_SUMMARY_SOURCES_PER_RECORD {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "semantic filter source count",
                });
            }
            let encoded: String = row
                .get(0)
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
            let role: String = row
                .get(1)
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
            if observation_matches_filter(&encoded, &role, filter)? {
                matched = true;
            }
        }
        Ok(matched)
    }

    async fn summary_observations_match(
        &self,
        candidate: &RankingCandidate,
        filter: &TemporalCandidateFilterV1,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<bool, TemporalPortError> {
        let authority = self.relation_authority.as_ref().ok_or_else(|| {
            read_message(
                CANDIDATE_OPERATION,
                "mounted session relation graph is unavailable",
            )
        })?;
        let session_id =
            SessionId::new(candidate.session.as_deref().ok_or_else(|| {
                read_message(CANDIDATE_OPERATION, "candidate session is missing")
            })?)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let source = candidate
            .source
            .as_deref()
            .filter(|source| !source.is_empty())
            .ok_or_else(|| read_message(CANDIDATE_OPERATION, "candidate provider is missing"))?;
        let generation = participant_generation(snapshot, &session_id, source)?;
        let control = snapshot.request().execution_control();
        control.checkpoint()?;
        let visits = authority
            .store
            .summary_sources(
                authority.scope,
                &session_id,
                generation,
                &candidate.retriever_record_id,
                MAX_SUMMARY_SOURCES_PER_RECORD,
                execution_control_graph_cancellation(control),
            )
            .map_err(|error| temporal_relation_error(error, control, "summary source relations"))?;
        control.checkpoint()?;
        let source_anchors = visits
            .into_iter()
            .filter_map(|visit| match visit.source {
                SummarySourceVisitKind::Anchor { anchor_id } => Some(anchor_id),
                SummarySourceVisitKind::Summary { .. } => None,
            })
            .collect::<Vec<RetrievalAnchorId>>();
        if source_anchors.is_empty() {
            return Ok(false);
        }
        let encoded_anchors = serde_json::to_string(&source_anchors)
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let generation =
            i64::try_from(generation).map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let mut rows = self
            .read
            .query(
                "SELECT observation.observation_json, occurrence.role
                 FROM json_each(?2) AS retained
                 JOIN session_occurrences occurrence
                   ON occurrence.retrieval_anchor_id = CAST(retained.value AS TEXT)
                  AND occurrence.session_id = ?1
                  AND occurrence.generation = ?3
                 JOIN observations observation
                   ON observation.observation_id = occurrence.source_observation_id
                 ORDER BY CAST(retained.key AS INTEGER)
                 LIMIT 257",
                params![session_id.as_str(), encoded_anchors, generation],
            )
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
        let mut count = 0usize;
        let mut matched = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
        {
            count += 1;
            if count > MAX_SUMMARY_SOURCES_PER_RECORD {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "semantic filter source count",
                });
            }
            let encoded = row
                .get::<String>(0)
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
            let role = row
                .get::<String>(1)
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?;
            if observation_matches_filter(&encoded, &role, filter)? {
                matched = true;
            }
        }
        control.checkpoint()?;
        Ok(matched)
    }

    #[allow(clippy::too_many_arguments)]
    async fn query_root_scope_candidates(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        cursor: &CandidateCursor,
        limit: usize,
        request: &PageRequest,
        project_key: &str,
    ) -> Result<TemporalSqlRows, TemporalPortError> {
        let caps = request.candidate_field_caps();
        let metadata_cap = caps.map_or(
            request.max_item_bytes(),
            tracedecay_temporal_query::ports::CandidateFieldCaps::metadata_field_bytes,
        );
        let stable_cap = caps.map_or(
            request.max_item_bytes(),
            tracedecay_temporal_query::ports::CandidateFieldCaps::stable_id_bytes,
        );
        let anchor_cap = caps.map_or(
            request.max_item_bytes(),
            tracedecay_temporal_query::ports::CandidateFieldCaps::anchor_id_bytes,
        );
        let provider = snapshot
            .provider_scope()
            .map_or(Value::Null, |value| Value::Text(value.to_string()));
        self.read
            .query(
                "SELECT occurrence.occurrence_id, occurrence.retrieval_anchor_id,
                        occurrence.knowledge_at, occurrence.message_id, occurrence.turn_id,
                        occurrence.session_id, occurrence.role, authority_session.provider
                 FROM session_temporal_generations frozen
                 JOIN session_occurrences occurrence
                   ON occurrence.session_id = frozen.session_id
                  AND occurrence.generation = frozen.generation
                 JOIN observations provider_observation
                   ON provider_observation.observation_id = occurrence.source_observation_id
                 JOIN retrieval_anchors authority_anchor
                   ON authority_anchor.anchor_id = occurrence.retrieval_anchor_id
                 JOIN sessions authority_session
                   ON authority_session.session_id = occurrence.session_id
                  AND authority_session.provider = COALESCE(json_extract(
                      provider_observation.observation_json,
                      '$.identity.source.provider'
                  ), 'claude')
                  AND authority_session.project_key = ?1
                 WHERE frozen.state = 'active'
                   AND (?2 IS NULL OR authority_session.provider = ?2)
                   AND (
                       (authority_session.project_key = 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'profile')
                       OR
                       (authority_session.project_key <> 'user'
                        AND json_extract(authority_anchor.owner_json, '$.kind') = 'project'
                        AND json_extract(authority_anchor.owner_json, '$.project_id')
                            = authority_session.project_key)
                   )
                   AND (
                       occurrence.knowledge_at < ?3
                       OR (
                           occurrence.knowledge_at = ?3
                           AND (
                               occurrence.session_id > ?4
                               OR (
                                   occurrence.session_id = ?4
                                   AND occurrence.occurrence_id > ?5
                               )
                           )
                       )
                   )
                   AND length(CAST(occurrence.occurrence_id AS BLOB)) <= ?6
                   AND length(CAST(occurrence.retrieval_anchor_id AS BLOB)) <= ?7
                   AND length(CAST(COALESCE(occurrence.message_id, '') AS BLOB)) <= ?8
                   AND length(CAST(COALESCE(occurrence.turn_id, '') AS BLOB)) <= ?8
                   AND length(CAST(occurrence.session_id AS BLOB)) <= ?8
                   AND length(CAST(occurrence.role AS BLOB)) <= ?8
                   AND length(CAST(authority_session.provider AS BLOB)) <= ?8
                   AND length(CAST(occurrence.occurrence_id AS BLOB))
                       + length(CAST(occurrence.retrieval_anchor_id AS BLOB))
                       + length(CAST(COALESCE(occurrence.message_id, '') AS BLOB))
                       + length(CAST(COALESCE(occurrence.turn_id, '') AS BLOB))
                       + length(CAST(occurrence.session_id AS BLOB))
                       + length(CAST(occurrence.role AS BLOB))
                       + length(CAST(authority_session.provider AS BLOB)) <= ?9
                   AND length(CAST(occurrence.occurrence_id AS BLOB))
                       + length(CAST(occurrence.session_id AS BLOB)) + 9 <= ?10
                 ORDER BY occurrence.knowledge_at DESC, occurrence.session_id,
                          occurrence.occurrence_id
                 LIMIT ?11",
                vec![
                    Value::Text(project_key.to_string()),
                    provider,
                    Value::Integer(cursor.knowledge_at),
                    Value::Text(cursor.session_id.clone()),
                    Value::Text(cursor.stable_id.clone()),
                    Value::Integer(i64::try_from(stable_cap.min(metadata_cap)).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(anchor_cap).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(metadata_cap).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(request.max_item_bytes()).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(stable_cap).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)),
                ],
            )
            .await
            .map_err(|error| read_error(CANDIDATE_OPERATION, error))
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
            if !participant.is_authorized_for_snapshot()
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
            let clause = &plan.clauses()[cursor.clause];
            validate_clause(clause, request)?;
            let mut extra = false;
            let mut last_emitted = None;
            let mut scan_cursor = cursor.clone();
            loop {
                clause_queries += 1;
                if clause_queries
                    > snapshot
                        .request()
                        .limits()
                        .candidate_limit
                        .saturating_add(plan.clauses().len())
                {
                    return Err(TemporalPortError::BudgetExceeded {
                        resource: "candidate filter scans",
                    });
                }
                let query_limit = bounds
                    .items
                    .saturating_sub(sink.len())
                    .saturating_add(1)
                    .max(FILTER_SCAN_PAGE_ITEMS);
                let mut rows = if matches!(
                    (scope, clause.channel),
                    (
                        TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
                        CandidateChannel::Scope
                    )
                ) {
                    self.query_root_scope_candidates(
                        snapshot,
                        &scan_cursor,
                        query_limit,
                        request,
                        root_project_key.ok_or(TemporalPortError::UnauthorizedSnapshot)?,
                    )
                    .await?
                } else {
                    query_candidate_clause(
                        &self.read,
                        scope,
                        snapshot,
                        clause,
                        &scan_cursor,
                        query_limit,
                        request,
                        root_project_key,
                    )
                    .await?
                };
                let mut scanned = 0usize;
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
                {
                    control.checkpoint()?;
                    scanned += 1;
                    let candidate = candidate_from_row(&row, clause.channel, scope)?;
                    require_candidate_scope(scope, &candidate)?;
                    scan_cursor = CandidateCursor {
                        clause: cursor.clause,
                        knowledge_at: candidate.knowledge_at_micros,
                        session_id: candidate.session.clone().unwrap_or_default(),
                        stable_id: candidate.retriever_record_id.clone(),
                    };
                    if !self
                        .candidate_matches_filter(
                            snapshot,
                            &candidate,
                            snapshot.request().semantic_filter(),
                        )
                        .await?
                    {
                        continue;
                    }
                    if sink.len() == bounds.items {
                        extra = true;
                        break;
                    }
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
                    last_emitted = Some(scan_cursor.clone());
                    sink.push(candidate)?;
                }
                if extra || scanned < query_limit {
                    break;
                }
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
        let window_size = bounds
            .items
            .saturating_add(1)
            .min(MAX_RECORD_QUERY_CANDIDATES);
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
            // The leading candidate's scope still gates the authority read, so a
            // scope violation fails before any window authorization runs.
            let root_authority = match (root_project_key, window.first()) {
                (Some(project_key), Some(first)) => {
                    require_candidate_scope(scope, first)?;
                    Some(
                        resolve_root_authority(
                            &self.read,
                            window,
                            project_key,
                            snapshot.provider_scope(),
                        )
                        .await?,
                    )
                }
                _ => None,
            };
            for (local, candidate) in window.iter().enumerate() {
                require_candidate_scope(scope, candidate)?;
                if let Some(authority) = root_authority.as_ref() {
                    authority.require(local)?;
                }
                if candidate.anchor_id.to_string().len() > request.max_key_bytes() {
                    return Err(TemporalPortError::BudgetExceeded {
                        resource: "record candidate anchor bytes",
                    });
                }
            }
            let query_limit = bounds.items.saturating_sub(sink.len()).saturating_add(1);
            let relation_authority = self.relation_authority.as_ref().ok_or_else(|| {
                read_message(
                    RECORD_OPERATION,
                    "mounted session relation graph is unavailable",
                )
            })?;
            let relations = load_record_relations(
                &relation_authority.store,
                relation_authority.scope,
                scope,
                snapshot,
                window,
                cursor.candidate,
                request,
            )?;
            let query = build_record_query_with_relations(
                scope,
                snapshot,
                window,
                cursor.candidate,
                &cursor,
                query_limit,
                request,
                &relations,
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
