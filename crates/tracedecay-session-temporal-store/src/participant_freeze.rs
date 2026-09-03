use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;
use tracedecay_domain::{SessionId, SignedCursorKeyRefV1};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_temporal_query::ports::{
    BindingDigest, MAX_TEMPORAL_PARTICIPANTS, TemporalAuthorizedRoot,
    TemporalParticipantAuthorization, TemporalParticipantGeneration, TemporalParticipantManifest,
    TemporalPreparedCandidateCohort, TemporalRetrievalScope, TemporalSourceAccess,
    TemporalWatermarks,
};

use super::execution::{
    AuthorizedTemporalExecutionRequest, SessionDataFreshness, SessionTemporalExecutionError,
};
use super::map_control_error;
use super::sql::TemporalSqlRead;
use super::sql::TemporalSqlRows;

/// Decides what a request is actually allowed to see of one participant source.
///
/// The session-scoped query does not filter on `project_key`, so a session
/// belonging to another project reaches this point and must be denied here.
/// An absent authorized root is missing authority, not a permissive one.
fn participant_authorization(
    authorized_root: Option<&TemporalAuthorizedRoot>,
    participant_project_key: &str,
) -> TemporalParticipantAuthorization {
    match authorized_root {
        Some(root) if root.project_key() == participant_project_key => {
            TemporalParticipantAuthorization::Authorized
        }
        _ => TemporalParticipantAuthorization::Denied,
    }
}

fn participant_source_access(
    metadata_json: Option<&str>,
    now: i64,
) -> Option<TemporalSourceAccess> {
    let metadata = match metadata_json {
        Some(encoded) => serde_json::from_str::<Value>(encoded).ok()?,
        None => Value::Null,
    };
    if metadata
        .get("retention_expires_at")
        .and_then(Value::as_i64)
        .is_some_and(|expires_at| expires_at <= now)
    {
        return Some(TemporalSourceAccess::RetentionWithheld);
    }
    let state = [
        "source_access",
        "payload_access",
        "hydration_state",
        "availability",
    ]
    .iter()
    .find_map(|key| metadata.get(*key).and_then(Value::as_str));
    match state {
        None | Some("authorized" | "available" | "eligible") => {
            Some(TemporalSourceAccess::Available)
        }
        Some("locked" | "quarantined") => Some(TemporalSourceAccess::Locked),
        Some("retention_withheld" | "retention_expired") => {
            Some(TemporalSourceAccess::RetentionWithheld)
        }
        Some("deleted") => Some(TemporalSourceAccess::Deleted),
        Some("redacted") => Some(TemporalSourceAccess::Redacted),
        Some("unavailable") => Some(TemporalSourceAccess::Unavailable),
        Some(_) => None,
    }
}

pub(super) async fn freeze_participants(
    read: &TemporalSqlRead<'_>,
    request: &AuthorizedTemporalExecutionRequest,
) -> Result<
    (
        TemporalParticipantManifest,
        TemporalWatermarks,
        Option<SignedCursorKeyRefV1>,
    ),
    SessionTemporalExecutionError,
> {
    let snapshot_request = request.snapshot_request();
    let TemporalRetrievalScope::Session(session_id) = snapshot_request.retrieval_scope() else {
        return Err(SessionTemporalExecutionError::WrongScope);
    };
    let rows = read
        .query(
            "SELECT generation.session_id, source.provider, generation.generation,
                    generation.frozen_watermarks_json, source.project_key,
                    source.metadata_json, unixepoch(), relation.generation
             FROM session_temporal_generations AS generation
             JOIN sessions AS source ON source.session_id = generation.session_id
             JOIN session_relation_receipts AS relation
               ON relation.session_id = generation.session_id
              AND relation.generation = generation.generation
              AND relation.state = 'applied'
              AND relation.graph_watermark = relation.expected_graph_watermark
             WHERE generation.session_id = ?1
               AND generation.state = 'active'
               AND (?2 IS NULL OR source.provider = ?2)
             ORDER BY generation.session_id, source.provider
             LIMIT ?3",
            params![
                session_id.as_str(),
                snapshot_request.provider_scope(),
                i64::try_from(MAX_TEMPORAL_PARTICIPANTS + 1).unwrap_or(i64::MAX)
            ],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;

    collect_participant_rows(read, rows, request, None).await
}

#[hotpath::measure(future = true, label = "session_temporal.freeze.prepared_candidates")]
pub(super) async fn freeze_prepared_candidate_participants(
    read: &TemporalSqlRead<'_>,
    request: &AuthorizedTemporalExecutionRequest,
    cohort: &TemporalPreparedCandidateCohort,
) -> Result<
    (
        TemporalParticipantManifest,
        TemporalWatermarks,
        Option<SignedCursorKeyRefV1>,
    ),
    SessionTemporalExecutionError,
> {
    let snapshot_request = request.snapshot_request();
    let root = snapshot_request
        .authorized_root()
        .ok_or(SessionTemporalExecutionError::WrongScope)?;
    let mut keys = BTreeSet::new();
    for candidate in cohort.candidates() {
        snapshot_request
            .execution_control()
            .checkpoint()
            .map_err(map_control_error)?;
        let session_id = candidate
            .session
            .as_deref()
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let provider = candidate
            .source
            .as_deref()
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        keys.insert((
            session_id.to_string(),
            provider.to_string(),
            candidate.participant_generation,
        ));
    }
    if keys.is_empty() {
        return Err(SessionTemporalExecutionError::Empty {
            freshness: root_readiness(read, request).await?,
        });
    }
    if keys.len() > MAX_TEMPORAL_PARTICIPANTS {
        return Err(SessionTemporalExecutionError::BudgetExhausted);
    }
    let encoded_keys = serde_json::to_string(
        &keys
            .iter()
            .map(|(session_id, provider, generation)| {
                serde_json::json!({
                    "session_id": session_id,
                    "provider": provider,
                    "generation": generation,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let rows = read
        .query(
            "WITH requested AS (
                 SELECT json_extract(value, '$.session_id') AS session_id,
                        json_extract(value, '$.provider') AS provider,
                        CAST(json_extract(value, '$.generation') AS INTEGER) AS generation
                 FROM json_each(?1)
             )
             SELECT generation.session_id, source.provider, generation.generation,
                    generation.frozen_watermarks_json, source.project_key,
                    source.metadata_json, unixepoch(), relation.generation
             FROM requested
             JOIN sessions AS source
               ON source.session_id = requested.session_id
              AND source.provider = requested.provider
              AND source.project_key = ?2
             JOIN session_temporal_generations AS generation
               ON generation.session_id = requested.session_id
              AND generation.generation = requested.generation
              AND generation.state = 'active'
             JOIN session_relation_receipts AS relation
               ON relation.session_id = generation.session_id
              AND relation.generation = generation.generation
              AND relation.state = 'applied'
              AND relation.graph_watermark = relation.expected_graph_watermark
             WHERE (?3 IS NULL OR source.provider = ?3)
             ORDER BY generation.session_id, source.provider
             LIMIT 257",
            params![
                encoded_keys,
                root.project_key(),
                snapshot_request.provider_scope()
            ],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    collect_participant_rows(read, rows, request, Some(keys.len())).await
}

async fn collect_participant_rows(
    read: &TemporalSqlRead<'_>,
    mut rows: TemporalSqlRows,
    request: &AuthorizedTemporalExecutionRequest,
    expected_count: Option<usize>,
) -> Result<
    (
        TemporalParticipantManifest,
        TemporalWatermarks,
        Option<SignedCursorKeyRefV1>,
    ),
    SessionTemporalExecutionError,
> {
    let snapshot_request = request.snapshot_request();
    let configuration_digest =
        BindingDigest::new("configuration_digest", request.configuration_digest())
            .map_err(map_control_error)?;
    let mut entries = Vec::new();
    let mut aggregate = TemporalWatermarks {
        generation: 0,
        source: 0,
        projection: 0,
        index: 0,
        summary: 0,
    };
    let mut shared_cursor_key = None::<Option<SignedCursorKeyRefV1>>;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
    {
        snapshot_request
            .execution_control()
            .checkpoint()
            .map_err(map_control_error)?;
        let session_id = row
            .get::<String>(0)
            .ok()
            .and_then(|value| SessionId::new(value).ok())
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let source_id = row
            .get::<String>(1)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let generation = row
            .get::<i64>(2)
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let encoded = row
            .get::<String>(3)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let participant_project_key = row
            .get::<String>(4)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let participant_metadata = row
            .get::<Option<String>>(5)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let snapshot_time = row
            .get::<i64>(6)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let graph_generation = row
            .get::<i64>(7)
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let mut authorization =
            participant_authorization(snapshot_request.authorized_root(), &participant_project_key);
        let access = participant_source_access(participant_metadata.as_deref(), snapshot_time)
            .unwrap_or_else(|| {
                authorization = TemporalParticipantAuthorization::Denied;
                TemporalSourceAccess::Available
            });
        let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        if frozen.active_generation > generation {
            return Err(SessionTemporalExecutionError::Unavailable);
        }
        let watermarks = TemporalWatermarks {
            generation,
            source: frozen.source_frontier,
            projection: frozen.projection_frontier,
            index: frozen.projection_frontier,
            summary: frozen.summary_frontier,
        };
        aggregate.generation = aggregate.generation.max(watermarks.generation);
        aggregate.source = aggregate.source.max(watermarks.source);
        aggregate.projection = aggregate.projection.max(watermarks.projection);
        aggregate.index = aggregate.index.max(watermarks.index);
        aggregate.summary = aggregate.summary.max(watermarks.summary);
        match &shared_cursor_key {
            Some(expected) if expected != &frozen.cursor_key => {
                return Err(SessionTemporalExecutionError::Unavailable);
            }
            None => shared_cursor_key = Some(frozen.cursor_key.clone()),
            Some(_) => {}
        }
        entries.push(
            TemporalParticipantGeneration::new(
                session_id,
                source_id,
                watermarks,
                graph_generation,
                &configuration_digest,
                snapshot_request.access_digest(),
                authorization,
                access,
            )
            .map_err(map_control_error)?,
        );
    }
    drop(rows);
    if expected_count.is_some_and(|expected| entries.len() != expected) {
        return Err(SessionTemporalExecutionError::Stale { generation_lag: 1 });
    }
    if entries.is_empty() {
        return if authorized_scope_has_sources(read, request).await? {
            Err(SessionTemporalExecutionError::Unavailable)
        } else {
            Err(SessionTemporalExecutionError::Empty {
                freshness: SessionDataFreshness::Fresh,
            })
        };
    }
    let participants = TemporalParticipantManifest::new(entries).map_err(map_control_error)?;
    Ok((participants, aggregate, shared_cursor_key.flatten()))
}

#[hotpath::measure(future = true, label = "session_temporal.freeze.root_readiness")]
pub(super) async fn root_readiness(
    read: &TemporalSqlRead<'_>,
    request: &AuthorizedTemporalExecutionRequest,
) -> Result<SessionDataFreshness, SessionTemporalExecutionError> {
    let snapshot_request = request.snapshot_request();
    let root = snapshot_request
        .authorized_root()
        .ok_or(SessionTemporalExecutionError::WrongScope)?;
    let mut rows = read
        .query(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN generation.generation IS NOT NULL
                                           AND relation.generation IS NOT NULL
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(MAX(CASE WHEN generation.generation IS NULL THEN 1
                                      ELSE MAX(
                                          CAST(json_extract(generation.frozen_watermarks_json,
                                              '$.source_frontier') AS INTEGER)
                                          - CAST(json_extract(generation.frozen_watermarks_json,
                                              '$.projection_frontier') AS INTEGER),
                                          0)
                                 END), 0)
             FROM sessions AS source
             LEFT JOIN session_temporal_generations AS generation
               ON generation.session_id = source.session_id
              AND generation.state = 'active'
             LEFT JOIN session_relation_receipts AS relation
               ON relation.session_id = generation.session_id
              AND relation.generation = generation.generation
              AND relation.state = 'applied'
              AND relation.graph_watermark = relation.expected_graph_watermark
             WHERE source.project_key = ?1
               AND (?2 IS NULL OR source.provider = ?2)",
            params![root.project_key(), snapshot_request.provider_scope()],
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
        .ok_or(SessionTemporalExecutionError::Unavailable)?;
    let total = row
        .get::<i64>(0)
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(SessionTemporalExecutionError::Unavailable)?;
    let ready = row
        .get::<i64>(1)
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(SessionTemporalExecutionError::Unavailable)?;
    let lag = row
        .get::<i64>(2)
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(SessionTemporalExecutionError::Unavailable)?;
    snapshot_request
        .execution_control()
        .checkpoint()
        .map_err(map_control_error)?;
    Ok(if ready < total {
        SessionDataFreshness::Partial {
            generation_lag: lag.max(1),
        }
    } else if lag > 0 {
        SessionDataFreshness::Stored {
            generation_lag: lag,
        }
    } else {
        SessionDataFreshness::Fresh
    })
}

async fn authorized_scope_has_sources(
    read: &TemporalSqlRead<'_>,
    request: &AuthorizedTemporalExecutionRequest,
) -> Result<bool, SessionTemporalExecutionError> {
    let snapshot_request = request.snapshot_request();
    let provider = snapshot_request.provider_scope();
    let project_key = snapshot_request
        .authorized_root()
        .ok_or(SessionTemporalExecutionError::WrongScope)?
        .project_key();
    let mut rows = match snapshot_request.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            read.query(
                "SELECT 1
                 FROM sessions
                 WHERE session_id = ?1
                   AND project_key = ?2
                   AND (?3 IS NULL OR provider = ?3)
                 LIMIT 1",
                params![session_id.as_str(), project_key, provider],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            read.query(
                "SELECT 1
                 FROM sessions
                 WHERE project_key = ?1
                   AND (?2 IS NULL OR provider = ?2)
                 LIMIT 1",
                params![project_key, provider],
            )
            .await
        }
    }
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|_| SessionTemporalExecutionError::Unavailable)
}

#[derive(Deserialize)]
struct FrozenWatermarksWire {
    active_generation: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
    projection_frontier: u64,
    source_frontier: u64,
    summary_frontier: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tracedecay_domain::{RetrievalGrainV1, TemporalModeV1};
    use tracedecay_global_db::tests::harness::open_registered_test_database_fixture;
    use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
    use tracedecay_runtime_core::db::engine::{Executor, TestConnection};
    use tracedecay_temporal_query::context::ContextBudget;
    use tracedecay_temporal_query::ports::TemporalSnapshotRequest;
    use tracedecay_temporal_query::ranking::DiversityLimits;

    fn root(project_id: Option<&str>) -> TemporalAuthorizedRoot {
        match project_id {
            Some(project_id) => {
                TemporalAuthorizedRoot::project("profile", project_id, "store", "root")
            }
            None => TemporalAuthorizedRoot::profile("profile", "store", "root"),
        }
        .expect("valid authorized root")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn execution_request() -> AuthorizedTemporalExecutionRequest {
        let snapshot = TemporalSnapshotRequest::new(
            SessionId::new("session.graph-stale").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            TemporalModeV1::Current,
            RetrievalGrainV1::Session,
        )
        .expect("snapshot request")
        .with_authorized_root(root(None))
        .expect("authorized root")
        .with_provider_scope(Some("codex".to_string()))
        .expect("provider scope");
        AuthorizedTemporalExecutionRequest::new(
            snapshot,
            "graph stale".to_string(),
            None,
            10,
            DiversityLimits::default(),
            ContextBudget {
                max_bytes: 64 * 1024,
                max_tokens: 4_096,
                estimator_version: "test-estimator.v1".to_string(),
            },
            1,
            1,
            digest('4'),
        )
    }

    fn root_execution_request(query: &str) -> AuthorizedTemporalExecutionRequest {
        let snapshot = TemporalSnapshotRequest::new(
            SessionId::new("root-anchor").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            TemporalModeV1::Current,
            RetrievalGrainV1::Session,
        )
        .expect("snapshot request")
        .with_authorized_root(root(None))
        .expect("authorized root")
        .with_provider_scope(Some("codex".to_string()))
        .expect("provider scope")
        .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
        AuthorizedTemporalExecutionRequest::new(
            snapshot,
            query.to_string(),
            None,
            10,
            DiversityLimits::unbounded(),
            ContextBudget {
                max_bytes: 64 * 1024,
                max_tokens: 4_096,
                estimator_version: "test-estimator.v1".to_string(),
            },
            1,
            1,
            digest('4'),
        )
    }

    async fn seed_root_sessions(
        connection: &TestConnection,
        count: usize,
        hit_count: usize,
        source_frontier: u64,
    ) {
        for index in 0..count {
            let session_id = format!("session.{index:03}");
            connection
                .execute(
                    "INSERT INTO sessions (provider, session_id, project_key, project_path)
                     VALUES ('codex', ?1, 'user', '/fixture')",
                    params![session_id.as_str()],
                )
                .await
                .expect("session");
            connection
                .execute(
                    "INSERT INTO session_temporal_generations (
                         session_id, generation, state, frozen_watermarks_json, created_at,
                         ready_at, activated_at, completed_at
                     ) VALUES (
                         ?1, 1, 'building', ?2,
                         1, NULL, NULL, NULL
                     )",
                    params![
                        session_id.as_str(),
                        format!(
                            "{{\"active_generation\":1,\"cursor_key\":null,\
                             \"projection_frontier\":1,\"source_frontier\":{source_frontier},\
                             \"summary_frontier\":1}}"
                        )
                    ],
                )
                .await
                .expect("building generation");
            connection
                .execute(
                    "UPDATE session_temporal_generations
                        SET state = 'ready', ready_at = 1
                      WHERE session_id = ?1 AND generation = 1",
                    params![session_id.as_str()],
                )
                .await
                .expect("ready generation");
            connection
                .execute(
                    "UPDATE session_temporal_generations
                        SET state = 'active', activated_at = 1
                      WHERE session_id = ?1 AND generation = 1",
                    params![session_id.as_str()],
                )
                .await
                .expect("active generation");
            connection
                .execute(
                    "INSERT INTO session_relation_receipts (
                         session_id, generation, scope_kind, scope_id,
                         expected_graph_watermark, state, graph_watermark,
                         created_at, applied_at
                     ) VALUES (?1, 1, 'profile_sessions', 'profile.fixture',
                               'graph.1', 'applied', 'graph.1', 1, 1)",
                    params![session_id.as_str()],
                )
                .await
                .expect("relation receipt");
            if index >= hit_count {
                continue;
            }
            let receipt_id = format!("receipt.{index:03}");
            let observation_id = format!("observation.{index:03}");
            let anchor_id = format!("anchor.{index:03}");
            let turn_id = format!("turn.{index:03}");
            let occurrence_id = format!("occurrence.{index:03}");
            let message_id = format!("message.{index:03}");
            connection
                .execute(
                    "INSERT INTO sanitization_receipts (
                         receipt_id, sanitizer_version, payload_digest, receipt_json
                     ) VALUES (?1, 'fixture', ?2, '{}')",
                    params![receipt_id.as_str(), format!("sha256:payload.{index:03}")],
                )
                .await
                .expect("sanitization receipt");
            connection
                .execute(
                    "INSERT INTO observations (
                         observation_id, payload_digest, receipt_id, observation_json,
                         committed_cursor_json
                     ) VALUES (?1, ?2, ?3, '{\"identity\":{\"source\":{
                         \"provider\":\"codex\"}}}', '{}')",
                    params![
                        observation_id.as_str(),
                        format!("sha256:payload.{index:03}"),
                        receipt_id.as_str()
                    ],
                )
                .await
                .expect("observation");
            connection
                .execute(
                    "INSERT INTO retrieval_anchors (
                         anchor_id, anchor_json, owner_json, projection_generation
                     ) VALUES (?1, '{}', '{\"kind\":\"profile\"}', 'fixture')",
                    params![anchor_id.as_str()],
                )
                .await
                .expect("retrieval anchor");
            connection
                .execute(
                    "INSERT INTO session_turns (
                         session_id, generation, turn_id, ordinal,
                         grouping_provenance, created_at
                     ) VALUES (?1, 1, ?2, 0, '{\"kind\":\"provider_native\"}', 1)",
                    params![session_id.as_str(), turn_id.as_str()],
                )
                .await
                .expect("turn");
            connection
                .execute(
                    "INSERT INTO session_occurrences (
                         session_id, generation, occurrence_id, source_observation_id,
                         source_provider, projection_output_ordinal, retrieval_anchor_id,
                         message_id, turn_id, role, knowledge_at, valid_time_json,
                         evidence_json, sanitized_content_digest, sanitized_content_bytes,
                         snippet_text, index_text
                     ) VALUES (?1, 1, ?2, ?3, 'codex', 0, ?4, ?5, ?6, 'user', ?7,
                               '{\"kind\":\"unknown\"}', '{}',
                               '0000000000000000000000000000000000000000000000000000000000000000',
                               14, 'needle cohort', 'needle cohort')",
                    params![
                        session_id.as_str(),
                        occurrence_id.as_str(),
                        observation_id.as_str(),
                        anchor_id.as_str(),
                        message_id.as_str(),
                        turn_id.as_str(),
                        i64::try_from(index + 1).expect("knowledge at")
                    ],
                )
                .await
                .expect("occurrence");
        }
    }

    #[tokio::test]
    async fn root_no_hit_over_256_sessions_is_truthful_zero_not_manifest_limit() {
        let directory = tempdir().expect("temporary directory");
        let database_path = directory.path().join("sessions.db");
        let (database, _owner) = open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("registered schema");
        let connection = TestConnection::open(&database_path);
        seed_root_sessions(&connection, 300, 0, 1).await;

        let execution = super::super::RegisteredGlobalDbSessionTemporalExecution::new(&database);
        let result = execution
            .freeze(&root_execution_request("no-such-message"))
            .await;

        match result {
            Err(SessionTemporalExecutionError::Empty { freshness }) => {
                assert_eq!(freshness, SessionDataFreshness::Fresh)
            }
            Err(error) => panic!("unexpected root freeze refusal: {error:?}"),
            Ok(_) => panic!("no-hit root freeze must return a truthful zero outcome"),
        }
    }

    #[tokio::test]
    async fn root_no_hit_reports_aggregate_projection_staleness() {
        let directory = tempdir().expect("temporary directory");
        let database_path = directory.path().join("sessions.db");
        let (database, _owner) = open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("registered schema");
        let connection = TestConnection::open(&database_path);
        seed_root_sessions(&connection, 3, 0, 2).await;

        let execution = super::super::RegisteredGlobalDbSessionTemporalExecution::new(&database);
        let result = execution
            .freeze(&root_execution_request("no-such-message"))
            .await;

        assert!(matches!(
            result,
            Err(SessionTemporalExecutionError::Empty {
                freshness: SessionDataFreshness::Stored { generation_lag: 1 }
            })
        ));
    }

    #[tokio::test]
    async fn root_rare_hit_over_256_sessions_freezes_only_the_admitted_participant() {
        let directory = tempdir().expect("temporary directory");
        let database_path = directory.path().join("sessions.db");
        let (database, _owner) = open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("registered schema");
        let connection = TestConnection::open(&database_path);
        seed_root_sessions(&connection, 300, 1, 1).await;

        let execution = super::super::RegisteredGlobalDbSessionTemporalExecution::new(&database);
        let (_, snapshot, readiness) = execution
            .freeze(&root_execution_request("needle cohort"))
            .await
            .expect("rare root hit");

        assert_eq!(snapshot.participant_manifest().entries().len(), 1);
        let candidates = snapshot
            .prepared_candidate_cohort()
            .expect("prepared cohort")
            .candidates();
        assert!(!candidates.is_empty());
        assert!(candidates.iter().all(|candidate| {
            candidate.session.as_deref() == Some("session.000")
                && candidate.participant_generation == 1
        }));
        assert_eq!(readiness, Some(SessionDataFreshness::Fresh));
    }

    #[tokio::test]
    async fn root_common_hit_over_256_sessions_reports_candidate_budget_not_manifest_limit() {
        let directory = tempdir().expect("temporary directory");
        let database_path = directory.path().join("sessions.db");
        let (database, _owner) = open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("registered schema");
        let connection = TestConnection::open(&database_path);
        seed_root_sessions(&connection, 300, 300, 1).await;

        let execution = super::super::RegisteredGlobalDbSessionTemporalExecution::new(&database);
        let result = execution
            .freeze(&root_execution_request("needle cohort"))
            .await;

        assert!(matches!(
            result,
            Err(SessionTemporalExecutionError::BudgetExhausted)
        ));
    }

    #[tokio::test]
    async fn participant_freeze_rejects_an_active_generation_without_its_applied_graph_receipt() {
        let directory = tempdir().expect("temporary directory");
        let database_path = directory.path().join("sessions.db");
        drop(
            open_registered_test_database_fixture(
                &database_path,
                TestDatabaseRuntimeScope::ProfileSessions,
            )
            .await
            .expect("registered schema"),
        );
        let connection = TestConnection::open(&database_path);
        connection
            .execute_batch(
                // Generations are walked through their real lifecycle rather
                // than inserted in a terminal state: the schema triggers admit
                // only `building` on insert and only the declared transitions
                // after it, so a fixture that writes `active` directly is
                // rejected and would test nothing.
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('codex', 'session.graph-stale', 'user', '/fixture');
                 INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES
                    (
                        'session.graph-stale', 1, 'building',
                        '{\"active_generation\":1,\"cursor_key\":null,
                          \"projection_frontier\":11,\"source_frontier\":11,
                          \"summary_frontier\":11}',
                        1, NULL, NULL, NULL
                    ),
                    (
                        'session.graph-stale', 2, 'building',
                        '{\"active_generation\":2,\"cursor_key\":null,
                          \"projection_frontier\":22,\"source_frontier\":22,
                          \"summary_frontier\":22}',
                        2, NULL, NULL, NULL
                    );
                 UPDATE session_temporal_generations
                    SET state = 'ready', ready_at = 1
                  WHERE session_id = 'session.graph-stale' AND generation = 1;
                 UPDATE session_temporal_generations
                    SET state = 'active', activated_at = 1
                  WHERE session_id = 'session.graph-stale' AND generation = 1;
                 UPDATE session_temporal_generations
                    SET state = 'superseded', completed_at = 2
                  WHERE session_id = 'session.graph-stale' AND generation = 1;
                 UPDATE session_temporal_generations
                    SET state = 'ready', ready_at = 2
                  WHERE session_id = 'session.graph-stale' AND generation = 2;
                 UPDATE session_temporal_generations
                    SET state = 'active', activated_at = 2
                  WHERE session_id = 'session.graph-stale' AND generation = 2;
                 INSERT INTO session_relation_receipts (
                    session_id, generation, scope_kind, scope_id,
                    expected_graph_watermark, state, graph_watermark,
                    created_at, applied_at
                 ) VALUES (
                    'session.graph-stale', 1, 'profile_sessions', 'profile.fixture',
                    'graph.old', 'applied', 'graph.old', 1, 1
                 );",
            )
            .await
            .expect("stale graph fixture");

        let result = freeze_participants(
            &TemporalSqlRead::engine_connection(&connection),
            &execution_request(),
        )
        .await;

        assert!(matches!(
            result,
            Err(SessionTemporalExecutionError::Unavailable)
        ));

        connection
            .execute(
                "INSERT INTO session_relation_receipts (
                    session_id, generation, scope_kind, scope_id,
                    expected_graph_watermark, state, graph_watermark,
                    created_at, applied_at
                 ) VALUES (
                    'session.graph-stale', 2, 'profile_sessions', 'profile.fixture',
                    'graph.current', 'applied', 'graph.current', 2, 2
                 )",
                (),
            )
            .await
            .expect("current graph receipt");
        let (participants, _, _) = freeze_participants(
            &TemporalSqlRead::engine_connection(&connection),
            &execution_request(),
        )
        .await
        .expect("current graph participant");
        let participant = participants.entries().first().expect("frozen participant");
        assert_eq!(participant.generation(), 2);
        assert_eq!(participant.graph_watermark(), 2);
    }

    #[test]
    fn a_source_owned_by_the_authorized_project_is_authorized() {
        assert_eq!(
            participant_authorization(Some(&root(Some("proj_a"))), "proj_a"),
            TemporalParticipantAuthorization::Authorized
        );
    }

    #[test]
    fn a_source_owned_by_another_project_is_denied() {
        assert_eq!(
            participant_authorization(Some(&root(Some("proj_a"))), "proj_b"),
            TemporalParticipantAuthorization::Denied
        );
    }

    #[test]
    fn a_profile_root_does_not_authorize_project_owned_sources() {
        assert_eq!(
            participant_authorization(Some(&root(None)), "proj_a"),
            TemporalParticipantAuthorization::Denied
        );
        assert_eq!(
            participant_authorization(Some(&root(None)), "user"),
            TemporalParticipantAuthorization::Authorized
        );
    }

    #[test]
    fn a_missing_authorized_root_denies_rather_than_permits() {
        assert_eq!(
            participant_authorization(None, "proj_a"),
            TemporalParticipantAuthorization::Denied
        );
    }

    #[test]
    fn persisted_source_lifecycle_states_are_preserved() {
        for (metadata, expected) in [
            (
                r#"{"payload_access":"quarantined"}"#,
                TemporalSourceAccess::Locked,
            ),
            (
                r#"{"payload_access":"retention_expired"}"#,
                TemporalSourceAccess::RetentionWithheld,
            ),
            (
                r#"{"payload_access":"deleted"}"#,
                TemporalSourceAccess::Deleted,
            ),
            (
                r#"{"payload_access":"redacted"}"#,
                TemporalSourceAccess::Redacted,
            ),
            (
                r#"{"payload_access":"unavailable"}"#,
                TemporalSourceAccess::Unavailable,
            ),
        ] {
            assert_eq!(
                participant_source_access(Some(metadata), 100),
                Some(expected)
            );
        }
    }

    #[test]
    fn expired_source_retention_is_withheld_at_snapshot_time() {
        assert_eq!(
            participant_source_access(Some(r#"{"retention_expires_at":99}"#), 100),
            Some(TemporalSourceAccess::RetentionWithheld)
        );
    }

    #[test]
    fn invalid_or_ambiguous_source_access_never_becomes_unavailable() {
        assert_eq!(
            participant_source_access(Some(r#"{"payload_access":"ambiguous"}"#), 100),
            None
        );
        assert_eq!(participant_source_access(Some("{"), 100), None);
    }
}
