use tracedecay_runtime_core::db::{DatabaseEngineReadSnapshot, engine};

/// One generation-shared projection table.
///
/// Generation `G` reads every row with `generation <= G`. A candidate adds
/// rows under its own generation; `key` names the columns a row is
/// identified by. An append-only table holds one row per key, while a
/// versioned table holds an older and a newer version of one key while the
/// candidate builds.
pub(crate) struct SharedGenerationTable {
    pub(crate) name: &'static str,
    pub(crate) key: &'static str,
    pub(crate) versioned: bool,
}

pub(crate) const SHARED_GENERATION_TABLES: &[SharedGenerationTable] = &[
    SharedGenerationTable {
        name: "session_turns",
        key: "turn_id",
        versioned: true,
    },
    SharedGenerationTable {
        name: "session_threads",
        key: "thread_id",
        versioned: true,
    },
    SharedGenerationTable {
        name: "session_agents",
        key: "agent_id",
        versioned: true,
    },
    SharedGenerationTable {
        name: "session_occurrences",
        key: "occurrence_id",
        versioned: false,
    },
    SharedGenerationTable {
        name: "session_turn_members",
        key: "turn_id, occurrence_id",
        versioned: false,
    },
    SharedGenerationTable {
        name: "session_assertions",
        key: "assertion_id",
        versioned: false,
    },
    SharedGenerationTable {
        name: "session_assertion_supersession",
        key: "superseded_assertion_id, superseding_assertion_id",
        versioned: false,
    },
    SharedGenerationTable {
        name: "session_current_entities",
        key: "entity_kind, entity_id",
        versioned: true,
    },
    SharedGenerationTable {
        name: "session_derived_evidence",
        key: "evidence_kind, first_occurrence_id",
        versioned: true,
    },
    SharedGenerationTable {
        name: "session_derived_evidence_members",
        key: "evidence_kind, first_occurrence_id, ordinal",
        versioned: true,
    },
];

/// Deletes every row a terminated candidate introduced, bound as
/// `(?1 session, ?2 generation)`. Later candidates read `generation <= G`, so
/// a failed candidate's rows must not outlive it.
pub(crate) fn discard_candidate_rows_sql(table: &SharedGenerationTable) -> String {
    format!(
        "DELETE FROM {} WHERE session_id = ?1 AND generation = ?2",
        table.name
    )
}

/// Deletes every row the generations through `?2` introduced for session `?1`.
pub(crate) fn discard_session_rows_sql(table: &SharedGenerationTable) -> String {
    format!(
        "DELETE FROM {} WHERE session_id = ?1 AND generation <= ?2",
        table.name
    )
}

/// Deletes the older version of every key the activating generation
/// re-versioned, bound as `(?1 session, ?2 generation)`. Only the active
/// generation is read, so a superseded version has no reader once its
/// successor activates.
pub(crate) fn retire_superseded_versions_sql(table: &SharedGenerationTable) -> String {
    let matches = table
        .key
        .split(", ")
        .map(|column| format!("older.{column} = successor.{column}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "DELETE FROM {name}
         WHERE rowid IN (
             SELECT older.rowid
             FROM {name} AS successor INDEXED BY idx_{name}_introduced
                  CROSS JOIN {name} AS older
             WHERE successor.session_id = ?1 AND successor.generation = ?2
               AND older.session_id = ?1 AND {matches} AND older.generation < ?2
         )",
        name = table.name,
    )
}

/// Predicate over a `session_temporal_observation_effects` row aliased
/// `effect`: whether its output is still canonical through the source
/// frontier bound at `frontier`. A source rewrite may have retired the
/// observation, or a later observation through the frontier may own its
/// message. The owner follows the projection's rule: the newest owner when
/// the projector created the message, the oldest otherwise. An effect without
/// projection provenance is canonical.
pub(crate) fn live_effect_predicate(frontier: &str) -> String {
    let version = tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;
    let retired_reason = tracedecay_store::ProjectionSkipReason::SourceRecordRetired.as_str();
    format!(
        "NOT EXISTS (
             SELECT 1 FROM observation_projection_dispositions AS retired
             WHERE retired.projector_version = '{version}'
               AND retired.observation_id = effect.observation_id
               AND retired.reason = '{retired_reason}'
         )
         AND NOT EXISTS (
             SELECT 1 FROM observation_projection_provenance AS own
             WHERE own.projector_version = '{version}'
               AND own.observation_id = effect.observation_id
               AND own.observation_id IS NOT (
                   SELECT owner.observation_id
                   FROM observation_projection_provenance AS owner
                   JOIN observations AS owner_observation
                     ON owner_observation.observation_id = owner.observation_id
                   WHERE owner.projector_version = own.projector_version
                     AND owner.output_provider = own.output_provider
                     AND owner.output_message_id = own.output_message_id
                     AND owner_observation.sequence <= {frontier}
                   ORDER BY CASE WHEN EXISTS (
                                SELECT 1
                                FROM observation_projection_provenance AS created
                                JOIN observations AS created_observation
                                  ON created_observation.observation_id = created.observation_id
                                WHERE created.projector_version = own.projector_version
                                  AND created.output_provider = own.output_provider
                                  AND created.output_message_id = own.output_message_id
                                  AND created.message_created = 1
                                  AND created_observation.sequence <= {frontier}
                            )
                            THEN -owner_observation.sequence
                            ELSE owner_observation.sequence END
                   LIMIT 1
               )
         )"
    )
}

#[derive(Clone, Copy)]
pub(super) enum TemporalSqlRead<'a> {
    #[cfg(test)]
    EngineConnection(&'a engine::Connection),
    Registered(&'a DatabaseEngineReadSnapshot),
}

impl<'a> TemporalSqlRead<'a> {
    pub(super) fn backend_kind(&self) -> engine::BackendKind {
        match self {
            #[cfg(test)]
            Self::EngineConnection(read) => read.backend_kind(),
            Self::Registered(read) => engine::QueryExecutor::backend_kind(*read),
        }
    }

    #[cfg(test)]
    pub(super) const fn engine_connection(read: &'a engine::Connection) -> Self {
        Self::EngineConnection(read)
    }

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
    fn backend_kind(&self) -> engine::BackendKind {
        TemporalSqlRead::backend_kind(self)
    }

    async fn query<P>(&self, sql: &str, params: P) -> engine::Result<engine::Rows>
    where
        P: engine::IntoParams,
    {
        TemporalSqlRead::query(self, sql, params).await
    }
}

impl crate::handle::SessionTemporalQuery for TemporalSqlRead<'_> {
    fn backend_kind(&self) -> engine::BackendKind {
        TemporalSqlRead::backend_kind(self)
    }

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
