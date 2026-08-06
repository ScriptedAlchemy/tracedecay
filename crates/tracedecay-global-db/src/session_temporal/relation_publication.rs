use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ProjectId, SessionId, SessionProjectionGenerationV1,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{SessionStoreResult, SessionTemporalProjectionBatchV1};

use super::query::{generation_i64, storage, storage_message};
use super::relations::{
    AgentHierarchyRelation, LogicalCopyRelation, SessionRelationGraphStore,
    SessionRelationProjection, validate_projection,
};
use crate::RegisteredGlobalDb;

const STAGE_OPERATION: &str = "stage session relation graph publication";
const APPLY_OPERATION: &str = "apply session relation graph publication";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionRelationPublicationIntent {
    pub projection: SessionRelationProjection,
    pub expected_active_generation: Option<u64>,
    pub digest: String,
    pub state: SessionRelationPublicationState,
    pub graph_watermark: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRelationPublicationState {
    Pending,
    Applied,
    Active,
}

pub async fn load_intent(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<Option<SessionRelationPublicationIntent>> {
    let mut rows = conn
        .query(
            "SELECT projection_json, projection_digest, expected_active_generation,
                    state, graph_watermark
             FROM session_relation_publications
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                generation_i64(generation, APPLY_OPERATION)?
            ],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let encoded: String = row
        .get(0)
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let digest: String = row
        .get(1)
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    if projection_digest(encoded.as_bytes()) != digest {
        return Err(storage_message(
            APPLY_OPERATION,
            "session relation publication intent digest does not match its payload",
        ));
    }
    let projection =
        serde_json::from_str(&encoded).map_err(|error| storage(APPLY_OPERATION, error))?;
    let expected_active_generation = row
        .get::<Option<i64>>(2)
        .map_err(|error| storage(APPLY_OPERATION, error))?
        .map(|generation| {
            u64::try_from(generation).map_err(|error| storage(APPLY_OPERATION, error))
        })
        .transpose()?;
    let state = match row
        .get::<String>(3)
        .map_err(|error| storage(APPLY_OPERATION, error))?
        .as_str()
    {
        "pending" => SessionRelationPublicationState::Pending,
        "applied" => SessionRelationPublicationState::Applied,
        "active" => SessionRelationPublicationState::Active,
        _ => {
            return Err(storage_message(
                APPLY_OPERATION,
                "session relation publication intent has an invalid state",
            ));
        }
    };
    let graph_watermark = row
        .get(4)
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    Ok(Some(SessionRelationPublicationIntent {
        projection,
        expected_active_generation,
        digest,
        state,
        graph_watermark,
    }))
}

pub async fn stage_projection(
    conn: &impl Executor,
    projection: &SessionRelationProjection,
    expected_active_generation: Option<u64>,
    now: i64,
) -> SessionStoreResult<()> {
    validate_projection(projection).map_err(|error| storage(STAGE_OPERATION, error))?;
    let encoded =
        serde_json::to_string(projection).map_err(|error| storage(STAGE_OPERATION, error))?;
    let digest = projection_digest(encoded.as_bytes());
    let expected = expected_active_generation
        .map(|generation| {
            i64::try_from(generation).map_err(|error| storage(STAGE_OPERATION, error))
        })
        .transpose()?;
    let changed = conn
        .execute(
            "INSERT INTO session_relation_publications (
            session_id, generation, project_id, expected_active_generation,
            projection_json, projection_digest, state, graph_watermark,
            created_at, applied_at, activated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', NULL, ?7, NULL, NULL)
         ON CONFLICT(session_id, generation) DO UPDATE SET
            project_id = excluded.project_id,
            expected_active_generation = excluded.expected_active_generation,
            projection_json = excluded.projection_json,
            projection_digest = excluded.projection_digest,
            state = CASE
                WHEN session_relation_publications.projection_digest = excluded.projection_digest
                  THEN session_relation_publications.state
                ELSE 'pending'
            END,
            graph_watermark = CASE
                WHEN session_relation_publications.projection_digest = excluded.projection_digest
                  THEN session_relation_publications.graph_watermark
                ELSE NULL
            END,
            applied_at = CASE
                WHEN session_relation_publications.projection_digest = excluded.projection_digest
                  THEN session_relation_publications.applied_at
                ELSE NULL
            END,
            activated_at = CASE
                WHEN session_relation_publications.projection_digest = excluded.projection_digest
                  THEN session_relation_publications.activated_at
                ELSE NULL
            END
         WHERE session_relation_publications.project_id = excluded.project_id
           AND session_relation_publications.expected_active_generation
               IS excluded.expected_active_generation
           AND (
               session_relation_publications.state = 'pending'
               OR session_relation_publications.projection_digest = excluded.projection_digest
           )",
            params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| storage(STAGE_OPERATION, error))?,
                projection.project_id.as_str(),
                expected,
                encoded,
                digest,
                now,
            ],
        )
        .await
        .map_err(|error| storage(STAGE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            STAGE_OPERATION,
            "an immutable session relation publication rejected different identity or topology",
        ));
    }
    Ok(())
}

pub async fn merge_refresh_batch(
    conn: &impl Executor,
    mut projection: SessionRelationProjection,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<SessionRelationProjection> {
    let mut copies = projection
        .logical_copies
        .into_iter()
        .map(|copy| {
            (
                (
                    copy.occurrence_id.as_str().to_owned(),
                    copy.copied_from_occurrence_id.as_str().to_owned(),
                ),
                copy,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for copy in batch.copies() {
        copies.insert(
            (
                copy.occurrence_id.as_str().to_owned(),
                copy.copied_from_occurrence_id.as_str().to_owned(),
            ),
            LogicalCopyRelation::from(copy),
        );
    }
    projection.logical_copies = copies.into_values().collect();

    let mut agents = projection
        .agent_hierarchy
        .into_iter()
        .map(|edge| {
            (
                (
                    edge.parent_agent_id.as_str().to_owned(),
                    edge.child_agent_id.as_str().to_owned(),
                ),
                edge,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for occurrence in batch.occurrences() {
        let mut rows = conn
            .query(
                "SELECT observation_json FROM observations WHERE observation_id = ?1",
                params![occurrence.source_observation_id.as_str()],
            )
            .await
            .map_err(|error| storage(STAGE_OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(STAGE_OPERATION, error))?
        else {
            return Err(storage_message(
                STAGE_OPERATION,
                "session relation publication lost its source observation",
            ));
        };
        let observation: tracedecay_domain::DurableObservationV1 = serde_json::from_str(
            &row.get::<String>(0)
                .map_err(|error| storage(STAGE_OPERATION, error))?,
        )
        .map_err(|error| storage(STAGE_OPERATION, error))?;
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone())
                .map_err(|error| storage(STAGE_OPERATION, error))?;
        if let (Some(parent), Some(child)) = (
            envelope.relations().parent_agent_id(),
            occurrence.agent_id.as_ref(),
        ) {
            let parent = tracedecay_domain::AgentInstanceId::new(parent.as_str())
                .map_err(|error| storage(STAGE_OPERATION, error))?;
            let edge = AgentHierarchyRelation {
                parent_agent_id: parent,
                child_agent_id: child.clone(),
                ordinal: occurrence.projection_output_ordinal.value(),
            };
            agents.insert(
                (
                    edge.parent_agent_id.as_str().to_owned(),
                    edge.child_agent_id.as_str().to_owned(),
                ),
                edge,
            );
        }
    }
    projection.agent_hierarchy = agents.into_values().collect();
    Ok(projection)
}

pub async fn apply_intent(
    database: &RegisteredGlobalDb,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<SessionRelationPublicationIntent> {
    let (project_id, graph) = database
        .session_relation_graph()
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let intent = load_intent(&snapshot, session_id, generation)
        .await?
        .ok_or_else(|| {
            storage_message(
                APPLY_OPERATION,
                "session relation publication intent is pending",
            )
        })?;
    if &intent.projection.project_id != project_id {
        return Err(storage_message(
            APPLY_OPERATION,
            "session relation publication project identity does not match the mounted graph",
        ));
    }
    if intent.state == SessionRelationPublicationState::Active {
        return Ok(intent);
    }
    let watermark = SessionRelationGraphStore::new(Arc::clone(graph))
        .replace(&intent.projection)
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE session_relation_publications
             SET state = 'applied', graph_watermark = ?4, applied_at = ?5
             WHERE session_id = ?1 AND generation = ?2
               AND projection_digest = ?3 AND state IN ('pending', 'applied')",
            params![
                session_id.as_str(),
                generation_i64(generation, APPLY_OPERATION)?,
                intent.digest.as_str(),
                watermark.as_str(),
                now_micros()?,
            ],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            APPLY_OPERATION,
            "session relation publication intent changed during graph apply",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    Ok(SessionRelationPublicationIntent {
        state: SessionRelationPublicationState::Applied,
        graph_watermark: Some(watermark.as_str().to_owned()),
        ..intent
    })
}

pub async fn apply_and_activate_latest_lcm_intent(
    database: &RegisteredGlobalDb,
    session_id: &SessionId,
) -> SessionStoreResult<()> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let mut rows = snapshot
        .query(
            "SELECT publication.generation
             FROM session_relation_publications AS publication
             JOIN session_temporal_generations AS generation
               ON generation.session_id = publication.session_id
              AND generation.generation = publication.generation
             WHERE publication.session_id = ?1
               AND publication.state IN ('pending', 'applied')
               AND generation.state = 'ready'
               AND json_extract(generation.frozen_watermarks_json, '$.route')
                   = 'lcm_summary_lineage_v1'
             ORDER BY publication.generation",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let mut generations = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?
    {
        let generation = row
            .get::<i64>(0)
            .map_err(|error| storage(APPLY_OPERATION, error))?;
        generations.push(
            SessionProjectionGenerationV1::new(
                u64::try_from(generation).map_err(|error| storage(APPLY_OPERATION, error))?,
            )
            .map_err(|error| storage(APPLY_OPERATION, error))?,
        );
    }
    drop(rows);
    drop(snapshot);
    for generation in &generations {
        apply_intent(database, session_id, *generation).await?;
    }
    let Some(candidate_generation) = generations.last().copied() else {
        return Ok(());
    };
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let intent = load_intent(&transaction, session_id, candidate_generation)
        .await?
        .ok_or_else(|| {
            storage_message(
                APPLY_OPERATION,
                "latest LCM relation publication intent disappeared",
            )
        })?;
    if intent.state != SessionRelationPublicationState::Applied {
        return Err(storage_message(
            APPLY_OPERATION,
            "latest LCM relation publication is not durably applied",
        ));
    }
    let candidate = generation_i64(candidate_generation, APPLY_OPERATION)?;
    let mut active_rows = transaction
        .query(
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active' ORDER BY generation",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let active = active_rows
        .next()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?
        .map(|row| {
            row.get::<i64>(0)
                .map_err(|error| storage(APPLY_OPERATION, error))
        })
        .transpose()?
        .map(|generation| {
            u64::try_from(generation).map_err(|error| storage(APPLY_OPERATION, error))
        })
        .transpose()?;
    if active_rows
        .next()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?
        .is_some()
        || active != intent.expected_active_generation
    {
        return Err(storage_message(
            APPLY_OPERATION,
            "LCM relation activation compare-and-swap failed",
        ));
    }
    drop(active_rows);
    let now = now_micros()?;
    if let Some(expected) = active {
        let superseded = transaction
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = ?3
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                params![
                    session_id.as_str(),
                    i64::try_from(expected).map_err(|error| storage(APPLY_OPERATION, error))?,
                    now,
                ],
            )
            .await
            .map_err(|error| storage(APPLY_OPERATION, error))?;
        if superseded != 1 {
            return Err(storage_message(
                APPLY_OPERATION,
                "LCM relation activation lost the expected active generation",
            ));
        }
    }
    transaction
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'cancelled', completed_at = ?3
             WHERE session_id = ?1 AND generation <> ?2 AND state = 'ready'",
            params![session_id.as_str(), candidate, now],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let activated = transaction
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = ?3
             WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'
               AND NOT EXISTS (
                   SELECT 1 FROM session_temporal_generations
                   WHERE session_id = ?1 AND state = 'active'
               )",
            params![session_id.as_str(), candidate, now],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let publication_activated = transaction
        .execute(
            "UPDATE session_relation_publications
             SET state = 'active', activated_at = ?4
             WHERE session_id = ?1 AND generation = ?2
               AND projection_digest = ?3 AND state = 'applied'",
            params![session_id.as_str(), candidate, intent.digest.as_str(), now,],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    if activated != 1 || publication_activated != 1 {
        return Err(storage_message(
            APPLY_OPERATION,
            "LCM relation activation did not publish one exact generation",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))
}

pub async fn recover_ready_lcm_intents(
    database: &RegisteredGlobalDb,
    limit: usize,
) -> SessionStoreResult<usize> {
    if limit == 0 {
        return Ok(0);
    }
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let mut rows = snapshot
        .query(
            "SELECT DISTINCT publication.session_id
             FROM session_relation_publications AS publication
             JOIN session_temporal_generations AS generation
               ON generation.session_id = publication.session_id
              AND generation.generation = publication.generation
             WHERE publication.state IN ('pending', 'applied')
               AND generation.state = 'ready'
               AND json_extract(generation.frozen_watermarks_json, '$.route')
                   = 'lcm_summary_lineage_v1'
             ORDER BY publication.session_id
             LIMIT ?1",
            params![i64::try_from(limit).map_err(|error| storage(APPLY_OPERATION, error))?],
        )
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?;
    let mut sessions = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(APPLY_OPERATION, error))?
    {
        sessions.push(
            SessionId::new(
                row.get::<String>(0)
                    .map_err(|error| storage(APPLY_OPERATION, error))?,
            )
            .map_err(|error| storage(APPLY_OPERATION, error))?,
        );
    }
    drop(rows);
    drop(snapshot);
    for session_id in &sessions {
        apply_and_activate_latest_lcm_intent(database, session_id).await?;
    }
    Ok(sessions.len())
}

pub fn empty_projection(
    project_id: ProjectId,
    session_id: SessionId,
    generation: u64,
) -> SessionRelationProjection {
    SessionRelationProjection {
        project_id,
        session_id,
        generation,
        summaries: Vec::new(),
        logical_copies: Vec::new(),
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
    }
}

fn projection_digest(encoded: &[u8]) -> String {
    hex::encode(Sha256::digest(encoded))
}

fn now_micros() -> SessionStoreResult<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| storage(APPLY_OPERATION, error))
        .and_then(|duration| {
            i64::try_from(duration.as_micros()).map_err(|error| storage(APPLY_OPERATION, error))
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_domain::{ProjectId, SessionId, SessionProjectionGenerationV1};
    use tracedecay_graph_db::{
        GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
        NeverCancelled,
    };
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

    use super::{
        SessionRelationPublicationState, apply_intent, empty_projection, load_intent,
        recover_ready_lcm_intents, stage_projection,
    };
    use crate::tests::harness::RegisteredGlobalDbTestRuntime;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid test identity")
    }

    #[tokio::test]
    async fn committed_intent_retries_graph_apply_and_rejects_generation_mutation() {
        let profile = tempfile::tempdir().expect("profile directory");
        let project_root = tempfile::tempdir().expect("project directory");
        let project_id = id::<ProjectId>("project.session-relation-publication");
        let runtime = RegisteredGlobalDbTestRuntime::project(
            profile.path(),
            project_root.path(),
            project_id.clone(),
        )
        .await
        .expect("project database");
        let database = runtime.project_database().expect("project shard");
        let graph = Arc::new(
            GraphDb::open(GraphDbOpenOptions {
                location: GraphDbLocation::Memory,
                expected_format: GraphFormatVersion::new(2).expect("format"),
                durability: GraphDurability::Memory,
                cancellation: Arc::new(NeverCancelled),
            })
            .expect("graph database"),
        );
        database
            .bind_session_relation_graph(project_id.clone(), Arc::clone(&graph))
            .expect("exact graph binding");
        let session_id = id::<SessionId>("session.relation-publication");
        let generation = SessionProjectionGenerationV1::new(1).expect("generation");
        let projection = empty_projection(project_id, session_id.clone(), generation.value());

        let transaction = database
            .begin_write_transaction()
            .await
            .expect("publication transaction");
        transaction
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json,
                    created_at, ready_at
                 ) VALUES (?1, ?2, 'ready', ?3, 1, 1)",
                params![
                    session_id.as_str(),
                    i64::try_from(generation.value()).expect("stored generation"),
                    serde_json::json!({
                        "active_generation": 1,
                        "cursor_key": null,
                        "source_frontier": 0,
                        "projection_frontier": 0,
                        "summary_frontier": 1,
                        "route": "lcm_summary_lineage_v1",
                    })
                    .to_string(),
                ],
            )
            .await
            .expect("ready generation");
        stage_projection(&transaction, &projection, None, 1)
            .await
            .expect("durable intent");
        transaction.commit().await.expect("intent commit");

        let first = apply_intent(database, &session_id, generation)
            .await
            .expect("first graph apply");
        let replay = apply_intent(database, &session_id, generation)
            .await
            .expect("idempotent graph replay");
        assert_eq!(first.digest, replay.digest);
        assert_eq!(replay.state, SessionRelationPublicationState::Applied);
        assert!(replay.graph_watermark.is_some());
        let transaction = database
            .begin_write_transaction()
            .await
            .expect("activation conflict transaction");
        stage_projection(&transaction, &projection, Some(999), 2)
            .await
            .expect_err("applied intent keeps its activation expectation immutable");
        transaction
            .rollback()
            .await
            .expect("activation conflict rollback");
        assert_eq!(
            recover_ready_lcm_intents(database, 1)
                .await
                .expect("restart recovery"),
            1
        );

        let mut conflicting = projection;
        conflicting
            .agent_hierarchy
            .push(super::AgentHierarchyRelation {
                parent_agent_id: id("agent.parent"),
                child_agent_id: id("agent.child"),
                ordinal: 0,
            });
        let transaction = database
            .begin_write_transaction()
            .await
            .expect("conflict transaction");
        let error = stage_projection(&transaction, &conflicting, None, 2)
            .await
            .expect_err("applied generation is immutable");
        assert!(format!("{error:?}").contains("rejected different identity or topology"));
        transaction.rollback().await.expect("conflict rollback");

        let snapshot = database.read_snapshot().await.expect("snapshot");
        let durable = load_intent(&snapshot, &session_id, generation)
            .await
            .expect("read durable intent")
            .expect("publication exists");
        assert_eq!(durable.digest, first.digest);
        assert_eq!(durable.state, SessionRelationPublicationState::Active);
        let mut active = snapshot
            .query(
                "SELECT generation FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = 'active'",
                params![session_id.as_str()],
            )
            .await
            .expect("active generation query");
        assert_eq!(
            active
                .next()
                .await
                .expect("active row")
                .expect("active generation")
                .get::<i64>(0)
                .expect("generation"),
            1
        );
    }
}
