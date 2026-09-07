use tracedecay_runtime_core::db::{DatabaseEngineReadSnapshot, engine};

/// Copies every session-temporal projection table's rows for the session bound
/// as `?1` from source generation `?3` into target generation `?2`.
pub(super) const GENERATION_COPY_STATEMENTS: &[&str] = &[
    "INSERT INTO session_turns (
        session_id, generation, turn_id, ordinal, grouping_provenance, created_at
     )
     SELECT session_id, ?2, turn_id, ordinal, grouping_provenance, created_at
     FROM session_turns WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_threads (
        session_id, generation, thread_id, grouping_provenance, created_at
     )
     SELECT session_id, ?2, thread_id, grouping_provenance, created_at
     FROM session_threads WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_agents (
        session_id, generation, agent_id, agent_json, created_at
     )
     SELECT session_id, ?2, agent_id, agent_json, created_at
     FROM session_agents WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_occurrences (
        session_id, generation, occurrence_id, source_observation_id,
        source_provider, projection_output_ordinal, retrieval_anchor_id, thread_id,
        thread_grouping_json, turn_id, turn_grouping_json, message_id,
        agent_id, role, knowledge_at, valid_time_json, evidence_json,
        sanitized_content_digest, sanitized_content_bytes,
        snippet_text, index_text
     )
     SELECT session_id, ?2, occurrence_id, source_observation_id,
            source_provider, projection_output_ordinal, retrieval_anchor_id, thread_id,
            thread_grouping_json, turn_id, turn_grouping_json, message_id,
            agent_id, role, knowledge_at, valid_time_json, evidence_json,
            sanitized_content_digest, sanitized_content_bytes,
            snippet_text, index_text
     FROM session_occurrences WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_turn_members (
        session_id, generation, turn_id, occurrence_id, ordinal
     )
     SELECT session_id, ?2, turn_id, occurrence_id, ordinal
     FROM session_turn_members WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_assertions (
        session_id, generation, assertion_id, assertion_kind,
        subject_anchor_id, object_anchor_id, knowledge_at,
        valid_time_json, evidence_json
     )
     SELECT session_id, ?2, assertion_id, assertion_kind,
            subject_anchor_id, object_anchor_id, knowledge_at,
            valid_time_json, evidence_json
     FROM session_assertions WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_assertion_supersession (
        session_id, generation, superseded_assertion_id,
        superseding_assertion_id, created_at
     )
     SELECT session_id, ?2, superseded_assertion_id,
            superseding_assertion_id, created_at
     FROM session_assertion_supersession
     WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_current_entities (
        session_id, generation, entity_kind, entity_id,
        current_assertion_id, current_occurrence_id, coverage_json
     )
     SELECT session_id, ?2, entity_kind, entity_id,
            current_assertion_id, current_occurrence_id, coverage_json
     FROM session_current_entities WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_derived_evidence (
        session_id, generation, evidence_kind, evidence_id,
        retrieval_anchor_id, thread_id,
        first_occurrence_id, last_occurrence_id,
        algorithm_version, configuration_digest,
        member_count, member_digest, evidence_json
     )
     SELECT session_id, ?2, evidence_kind, evidence_id,
            retrieval_anchor_id, thread_id,
            first_occurrence_id, last_occurrence_id,
            algorithm_version, configuration_digest,
            member_count, member_digest, evidence_json
     FROM session_derived_evidence WHERE session_id = ?1 AND generation = ?3",
    "INSERT INTO session_derived_evidence_members (
        session_id, generation, evidence_kind, evidence_id,
        ordinal, occurrence_id, member_role
     )
     SELECT session_id, ?2, evidence_kind, evidence_id,
            ordinal, occurrence_id, member_role
     FROM session_derived_evidence_members
     WHERE session_id = ?1 AND generation = ?3",
];

#[derive(Clone, Copy)]
pub(super) enum TemporalSqlRead<'a> {
    #[cfg(test)]
    EngineConnection(&'a engine::Connection),
    Registered(&'a DatabaseEngineReadSnapshot),
}

impl<'a> TemporalSqlRead<'a> {
    #[cfg(test)]
    #[hotpath::skip]
    pub(super) const fn engine_connection(read: &'a engine::Connection) -> Self {
        Self::EngineConnection(read)
    }

    #[hotpath::skip]
    pub(super) const fn registered(read: &'a DatabaseEngineReadSnapshot) -> Self {
        Self::Registered(read)
    }

    pub(super) async fn query<P>(&self, sql: &str, params: P) -> engine::Result<TemporalSqlRows>
    where
        P: engine::IntoParams,
    {
        match self {
            #[cfg(test)]
            Self::EngineConnection(read) => read.query(sql, params).await,
            Self::Registered(read) => read.query(sql, params).await,
        }
    }
}

impl engine::QueryExecutor for TemporalSqlRead<'_> {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> engine::Result<engine::Rows>
    where
        P: engine::IntoParams,
    {
        TemporalSqlRead::query(self, sql, params).await
    }
}

impl crate::handle::SessionTemporalQuery for TemporalSqlRead<'_> {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl std::future::Future<Output = engine::Result<engine::Rows>> + Send
    where
        P: engine::IntoParams + Send,
    {
        TemporalSqlRead::query(self, sql, params)
    }
}

pub(super) type TemporalSqlRows = engine::Rows;
pub(super) type TemporalSqlRow = engine::Row;
