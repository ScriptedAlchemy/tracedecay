use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1};
use tracedecay_store::{
    ProjectionCheckpoint, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SessionMessageProjection, SessionMessageRecord,
    SessionRecord,
};

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, Value, params};
use tracedecay_sessions::compatibility::projected_content_hash;

use super::apply::{derive_projection_with_alias, verify_provenance};

pub(super) fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ProjectionStoreError {
    ProjectionStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn storage_message(
    operation: &'static str,
    message: impl Into<String>,
) -> ProjectionStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

pub(super) fn decode_sequence(value: i64, operation: &'static str) -> ProjectionStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}

pub(super) fn decode_observation_row(
    row: &Row,
    operation: &'static str,
) -> ProjectionStoreResult<(u64, DurableObservationV1)> {
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage(operation, error))?;
    let observation = serde_json::from_str(&observation_json)
        .map_err(|error| storage("decode queued observation", error))?;
    Ok((sequence, observation))
}

pub(super) async fn read_observation(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<(u64, DurableObservationV1)>> {
    let mut rows = conn
        .query(
            "SELECT sequence, observation_json FROM observations WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read queued observation", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read queued observation", error))?
    else {
        return Ok(None);
    };
    decode_observation_row(&row, "read queued observation").map(Some)
}

pub(super) async fn read_checkpoint(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage("read projector checkpoint", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projector checkpoint", error))?
    else {
        return Ok(ProjectionCheckpoint::new(0));
    };
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projector checkpoint", error))?,
        "read projector checkpoint",
    )?;
    Ok(ProjectionCheckpoint::new(sequence))
}

pub(super) async fn write_checkpoint(
    conn: &impl Executor,
    sequence: u64,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let sequence_i64 =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, ?2)
         ON CONFLICT(projector_version) DO UPDATE SET last_sequence = excluded.last_sequence",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, sequence_i64],
    )
    .await
    .map_err(|error| storage("write projector checkpoint", error))?;
    Ok(ProjectionCheckpoint::new(sequence))
}

pub(super) async fn queued_sequence(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<u64>> {
    let mut rows = conn
        .query(
            "SELECT observation_sequence FROM projection_queue WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read projection queue", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection queue", error))?
    else {
        return Ok(None);
    };
    decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projection queue", error))?,
        "read projection queue",
    )
    .map(Some)
}

pub(super) async fn consume_projection_queue_item(
    conn: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "DELETE FROM projection_queue WHERE observation_id = ?1",
        params![observation_id.as_str()],
    )
    .await
    .map_err(|error| storage("consume projection queue item", error))?;
    Ok(())
}

pub(super) async fn read_session(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
) -> ProjectionStoreResult<Option<SessionRecord>> {
    let mut rows = conn
        .query(
            "SELECT provider, session_id, project_key, project_path, title, started_at, ended_at,
                    transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                    parent_tool_use_id
             FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|error| storage("read projected session", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projected session", error))?
    else {
        return Ok(None);
    };
    Ok(Some(SessionRecord {
        provider: row
            .get(0)
            .map_err(|error| storage("decode projected session", error))?,
        session_id: row
            .get(1)
            .map_err(|error| storage("decode projected session", error))?,
        project_key: row
            .get(2)
            .map_err(|error| storage("decode projected session", error))?,
        project_path: row
            .get(3)
            .map_err(|error| storage("decode projected session", error))?,
        title: row
            .get(4)
            .map_err(|error| storage("decode projected session", error))?,
        started_at: row
            .get(5)
            .map_err(|error| storage("decode projected session", error))?,
        ended_at: row
            .get(6)
            .map_err(|error| storage("decode projected session", error))?,
        transcript_path: row
            .get(7)
            .map_err(|error| storage("decode projected session", error))?,
        metadata_json: row
            .get(8)
            .map_err(|error| storage("decode projected session", error))?,
        parent_session_id: row
            .get(9)
            .map_err(|error| storage("decode projected session", error))?,
        is_subagent: row
            .get::<i64>(10)
            .map_err(|error| storage("decode projected session", error))?
            != 0,
        agent_id: row
            .get(11)
            .map_err(|error| storage("decode projected session", error))?,
        parent_tool_use_id: row
            .get(12)
            .map_err(|error| storage("decode projected session", error))?,
    }))
}

pub(super) async fn read_message(
    conn: &impl QueryExecutor,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<SessionMessageRecord>> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                    model, tool_names, source_path, source_offset, metadata_json
             FROM session_messages WHERE provider = ?1 AND message_id = ?2",
            params![provider, message_id],
        )
        .await
        .map_err(|error| storage("read projected message", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projected message", error))?
    else {
        return Ok(None);
    };
    macro_rules! cell {
        ($index:literal) => {
            row.get($index)
                .map_err(|error| storage("decode projected message", error))?
        };
    }
    Ok(Some(SessionMessageRecord {
        provider: cell!(0),
        message_id: cell!(1),
        session_id: cell!(2),
        role: cell!(3),
        timestamp: cell!(4),
        ordinal: cell!(5),
        text: cell!(6),
        kind: cell!(7),
        model: cell!(8),
        tool_names: cell!(9),
        source_path: cell!(10),
        source_offset: cell!(11),
        metadata_json: cell!(12),
    }))
}

pub(super) struct ProjectionOutputOwner {
    pub(super) sequence: u64,
    pub(super) observation: DurableObservationV1,
}

pub(super) struct ProjectionOutputState {
    pub(super) latest: ProjectionOutputOwner,
    pub(super) canonical: DurableObservationV1,
    pub(super) projector_owned: bool,
    pub(super) owner_count: u64,
}

pub(super) async fn ensure_projection_output_state_cache(
    conn: &impl Executor,
) -> ProjectionStoreResult<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state (
            projector_version TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            canonical_observation_id TEXT NOT NULL,
            latest_observation_id TEXT NOT NULL,
            latest_sequence INTEGER NOT NULL CHECK(latest_sequence >= 0),
            projector_owned INTEGER NOT NULL CHECK(projector_owned IN (0, 1)),
            owner_count INTEGER NOT NULL CHECK(owner_count > 0),
            PRIMARY KEY(projector_version, output_provider, output_message_id)
        ) WITHOUT ROWID;
        CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state_meta (
            initialized INTEGER PRIMARY KEY CHECK(initialized = 1),
            data_version INTEGER NOT NULL CHECK(data_version >= 0)
        ) WITHOUT ROWID;",
    )
    .await
    .map_err(|error| storage("create projection output state cache", error))?;
    let mut version_rows = conn
        .query("PRAGMA data_version", ())
        .await
        .map_err(|error| storage("read projection cache data version", error))?;
    let data_version = version_rows
        .next()
        .await
        .map_err(|error| storage("read projection cache data version", error))?
        .ok_or_else(|| storage_message("read projection cache data version", "no row"))?
        .get::<i64>(0)
        .map_err(|error| storage("read projection cache data version", error))?;
    drop(version_rows);

    let mut rows = conn
        .query(
            "SELECT 1 FROM temp.observation_projection_output_state_meta
             WHERE initialized = 1 AND data_version = ?1",
            params![data_version],
        )
        .await
        .map_err(|error| storage("read projection output state cache", error))?;
    let initialized = rows
        .next()
        .await
        .map_err(|error| storage("read projection output state cache", error))?
        .is_some();
    drop(rows);
    if initialized {
        return Ok(());
    }

    conn.execute_batch(
        "DELETE FROM temp.observation_projection_output_state;
         DELETE FROM temp.observation_projection_output_state_meta;
         WITH owner_groups AS (
            SELECT projector_version, output_provider, output_message_id,
                   MAX(message_created) AS projector_owned,
                   COUNT(*) AS owner_count
            FROM observation_projection_provenance
            GROUP BY projector_version, output_provider, output_message_id
         )
         INSERT INTO temp.observation_projection_output_state (
            projector_version, output_provider, output_message_id,
            canonical_observation_id, latest_observation_id, latest_sequence,
            projector_owned, owner_count
         )
         SELECT groups.projector_version, groups.output_provider, groups.output_message_id,
                CASE WHEN groups.projector_owned = 1 THEN (
                    SELECT provenance.observation_id
                    FROM observation_projection_provenance AS provenance
                    JOIN observations AS observation
                      ON observation.observation_id = provenance.observation_id
                    WHERE provenance.projector_version = groups.projector_version
                      AND provenance.output_provider = groups.output_provider
                      AND provenance.output_message_id = groups.output_message_id
                    ORDER BY observation.sequence DESC, provenance.observation_id DESC
                    LIMIT 1
                ) ELSE (
                    SELECT provenance.observation_id
                    FROM observation_projection_provenance AS provenance
                    JOIN observations AS observation
                      ON observation.observation_id = provenance.observation_id
                    WHERE provenance.projector_version = groups.projector_version
                      AND provenance.output_provider = groups.output_provider
                      AND provenance.output_message_id = groups.output_message_id
                    ORDER BY observation.sequence ASC, provenance.observation_id ASC
                    LIMIT 1
                ) END,
                (
                    SELECT provenance.observation_id
                    FROM observation_projection_provenance AS provenance
                    JOIN observations AS observation
                      ON observation.observation_id = provenance.observation_id
                    WHERE provenance.projector_version = groups.projector_version
                      AND provenance.output_provider = groups.output_provider
                      AND provenance.output_message_id = groups.output_message_id
                    ORDER BY observation.sequence DESC, provenance.observation_id DESC
                    LIMIT 1
                ),
                (
                    SELECT observation.sequence
                    FROM observation_projection_provenance AS provenance
                    JOIN observations AS observation
                      ON observation.observation_id = provenance.observation_id
                    WHERE provenance.projector_version = groups.projector_version
                      AND provenance.output_provider = groups.output_provider
                      AND provenance.output_message_id = groups.output_message_id
                    ORDER BY observation.sequence DESC, provenance.observation_id DESC
                    LIMIT 1
                ),
                groups.projector_owned, groups.owner_count
         FROM owner_groups AS groups;",
    )
    .await
    .map_err(|error| storage("initialize projection output state cache", error))?;
    conn.execute(
        "UPDATE temp.observation_projection_output_state
         SET projector_owned = 1,
             canonical_observation_id = latest_observation_id
         WHERE projector_version = ?1
           AND EXISTS (
                SELECT 1 FROM observation_projection_provenance AS predecessor
                WHERE predecessor.projector_version IN (?2, ?3)
                  AND predecessor.output_provider =
                      observation_projection_output_state.output_provider
                  AND predecessor.output_message_id =
                      observation_projection_output_state.output_message_id
                  AND predecessor.message_created = 1
           )",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            SESSION_MESSAGE_PROJECTOR_VERSION_V2,
            SESSION_MESSAGE_PROJECTOR_VERSION_V1
        ],
    )
    .await
    .map_err(|error| storage("inherit predecessor projection ownership", error))?;
    conn.execute(
        "INSERT INTO temp.observation_projection_output_state_meta(initialized, data_version)
         VALUES (1, ?1)",
        params![data_version],
    )
    .await
    .map_err(|error| storage("record projection cache data version", error))?;

    let mut rows = conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM observation_projection_provenance),
                (SELECT COALESCE(SUM(owner_count), 0)
                 FROM temp.observation_projection_output_state)",
            (),
        )
        .await
        .map_err(|error| storage("verify projection output state cache", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("verify projection output state cache", error))?
        .ok_or_else(|| storage_message("verify projection output state cache", "no row"))?;
    let provenance_count = row
        .get::<i64>(0)
        .map_err(|error| storage("verify projection output state cache", error))?;
    let cached_count = row
        .get::<i64>(1)
        .map_err(|error| storage("verify projection output state cache", error))?;
    if provenance_count != cached_count {
        return Err(storage_message(
            "verify projection output state cache",
            "provenance aggregate mismatch",
        ));
    }
    Ok(())
}

pub(super) async fn inherit_predecessor_output_state(
    conn: &impl Executor,
    observation_id: &str,
    predecessor_version: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "UPDATE temp.observation_projection_output_state AS state
         SET projector_owned = 1,
             canonical_observation_id = latest_observation_id
         WHERE state.projector_version = ?1
           AND EXISTS (
                SELECT 1 FROM observation_projection_provenance AS current
                WHERE current.projector_version = ?1
                  AND current.observation_id = ?2
                  AND current.output_provider = state.output_provider
                  AND current.output_message_id = state.output_message_id
           )
           AND EXISTS (
                SELECT 1 FROM observation_projection_provenance AS predecessor
                WHERE predecessor.projector_version = ?3
                  AND predecessor.output_provider = state.output_provider
                  AND predecessor.output_message_id = state.output_message_id
                  AND predecessor.message_created = 1
           )",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            observation_id,
            predecessor_version
        ],
    )
    .await
    .map_err(|error| storage("inherit predecessor projection output state", error))?;
    Ok(())
}

pub(super) async fn read_output_state(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<Option<ProjectionOutputState>> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT state.latest_sequence, latest.observation_json,
                    canonical.observation_json, state.projector_owned, state.owner_count
             FROM temp.observation_projection_output_state AS state
             JOIN observations AS latest
               ON latest.observation_id = state.latest_observation_id
             JOIN observations AS canonical
               ON canonical.observation_id = state.canonical_observation_id
             WHERE state.projector_version = ?1
               AND state.output_provider = ?2
               AND state.output_message_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage("read projection output state", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection output state", error))?
    else {
        return Ok(None);
    };
    let latest_sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projection output state", error))?,
        "read projection output state",
    )?;
    let latest = serde_json::from_str(
        &row.get::<String>(1)
            .map_err(|error| storage("read projection output state", error))?,
    )
    .map_err(|error| storage("decode latest projection output owner", error))?;
    let canonical = serde_json::from_str(
        &row.get::<String>(2)
            .map_err(|error| storage("read projection output state", error))?,
    )
    .map_err(|error| storage("decode canonical projection output owner", error))?;
    let projector_owned = row
        .get::<i64>(3)
        .map_err(|error| storage("read projection output state", error))?
        != 0;
    let owner_count = decode_sequence(
        row.get::<i64>(4)
            .map_err(|error| storage("read projection output state", error))?,
        "read projection output state",
    )?;
    if owner_count == 0 {
        return Err(storage_message(
            "read projection output state",
            "empty ownership aggregate",
        ));
    }
    Ok(Some(ProjectionOutputState {
        latest: ProjectionOutputOwner {
            sequence: latest_sequence,
            observation: latest,
        },
        canonical,
        projector_owned,
        owner_count,
    }))
}

pub(super) async fn has_other_projector_output_owner(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<bool> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT 1 FROM observation_projection_provenance
             WHERE output_provider = ?1 AND output_message_id = ?2
               AND projector_version <> ?3
               AND NOT EXISTS (
                    SELECT 1 FROM observation_projection_migrations AS migration
                    WHERE migration.source_projector_version =
                          observation_projection_provenance.projector_version
                      AND migration.target_projector_version = ?3
                      AND migration.completed = 1
                      AND EXISTS (
                        SELECT 1 FROM observation_projection_provenance AS inherited
                        WHERE inherited.projector_version = ?3
                          AND inherited.observation_id =
                              observation_projection_provenance.observation_id
                          AND inherited.receipt_id =
                              observation_projection_provenance.receipt_id
                          AND inherited.output_provider =
                              observation_projection_provenance.output_provider
                          AND inherited.output_message_id =
                              observation_projection_provenance.output_message_id
                      )
               )
             LIMIT 1",
            params![
                message.provider.as_str(),
                message.message_id.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
            ],
        )
        .await
        .map_err(|error| storage("read cross-projector output owners", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage("read cross-projector output owners", error))?
        .is_some())
}

async fn message_projection(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<SessionMessageProjection> {
    derive_projection_with_alias(conn, observation)
        .await?
        .messages()
        .find(|projection| {
            projection.message().provider == provider
                && projection.message().message_id == message_id
        })
        .cloned()
        .ok_or(ProjectionStoreError::ProvenanceCollision)
}

async fn verify_rows(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    // Re-derived sessions still carry the observation's host spelling. Apply the
    // same ingest-boundary normalization used by apply_session so macOS firmlink
    // expansions (/var -> /private/var) and user symlink families compare equal
    // to the persisted canonical row without putting FS probing into reconcile.
    let expected = canonicalize_session_project_paths(session);
    if !read_session(conn, &session.provider, &session.session_id)
        .await?
        .as_ref()
        .is_some_and(|actual| session_rows_compatible(actual, &expected))
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: session.provider.clone(),
            message_id: format!("session:{}", session.session_id),
        });
    }
    let message = projection.message();
    let actual = read_message(conn, &message.provider, &message.message_id).await?;
    let compatible = match actual.as_ref() {
        Some(actual) => {
            actual == message || protected_message_rows_compatible(conn, actual, message).await?
        }
        None => false,
    };
    if !compatible {
        return Err(ProjectionStoreError::OutputCollision {
            provider: message.provider.clone(),
            message_id: message.message_id.clone(),
        });
    }
    Ok(())
}

pub(super) fn same_projection_lineage(
    candidate: &DurableObservationV1,
    owner: &DurableObservationV1,
) -> bool {
    candidate.source() == owner.source() && candidate.scope() == owner.scope()
}

pub(super) async fn verify_output_state(
    conn: &impl QueryExecutor,
    state: &ProjectionOutputState,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    if state.owner_count == 0 {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    let message = projection.message();
    let owner_projection = message_projection(
        conn,
        &state.canonical,
        &message.provider,
        &message.message_id,
    )
    .await?;
    verify_provenance(conn, &owner_projection).await?;
    verify_rows(conn, &owner_projection).await
}

pub(in super::super) async fn verify_output_authority(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT MAX(message_created), COUNT(*), EXISTS (
                SELECT 1
                FROM observation_projection_migrations AS migration
                JOIN observation_projection_provenance AS predecessor
                  ON predecessor.projector_version = migration.source_projector_version
                JOIN observation_projection_provenance AS current
                  ON current.projector_version = migration.target_projector_version
                 AND current.observation_id = predecessor.observation_id
                 AND current.receipt_id = predecessor.receipt_id
                 AND current.output_provider = predecessor.output_provider
                 AND current.output_message_id = predecessor.output_message_id
                WHERE migration.target_projector_version = ?1
                  AND migration.completed = 1
                  AND predecessor.output_provider = ?2
                  AND predecessor.output_message_id = ?3
                  AND predecessor.message_created = 1
             )
             FROM observation_projection_provenance
                  INDEXED BY idx_observation_projection_provenance_global_output
             WHERE projector_version = ?1
               AND output_provider = ?2 AND output_message_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("read projection output authority", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read projection output authority", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let projector_owned = row
        .get::<Option<i64>>(0)
        .map_err(|error| storage("read projection output authority", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?
        != 0
        || row
            .get::<i64>(2)
            .map_err(|error| storage("read projection output authority", error))?
            != 0;
    let owner_count = row
        .get::<i64>(1)
        .map_err(|error| storage("read projection output authority", error))?;
    drop(rows);
    if owner_count <= 0 {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }

    let ordering = if projector_owned { "DESC" } else { "ASC" };
    let mut rows = conn
        .query(
            &format!(
                "SELECT observation.observation_json
                 FROM observation_projection_provenance AS provenance
                      INDEXED BY idx_observation_projection_provenance_global_output
                 JOIN observations AS observation
                   ON observation.observation_id = provenance.observation_id
                 WHERE provenance.projector_version = ?1
                   AND provenance.output_provider = ?2
                   AND provenance.output_message_id = ?3
                 ORDER BY observation.sequence {ordering}, provenance.observation_id {ordering}
                 LIMIT 1"
            ),
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("read canonical projection output authority", error))?;
    let observation_json = rows
        .next()
        .await
        .map_err(|error| storage("read canonical projection output authority", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?
        .get::<String>(0)
        .map_err(|error| storage("read canonical projection output authority", error))?;
    let observation = serde_json::from_str(&observation_json)
        .map_err(|error| storage("decode canonical projection output authority", error))?;
    let owner_projection =
        message_projection(conn, &observation, &message.provider, &message.message_id).await?;
    verify_provenance(conn, &owner_projection).await?;
    verify_rows(conn, &owner_projection).await
}

pub(super) fn session_rows_compatible(actual: &SessionRecord, expected: &SessionRecord) -> bool {
    reconcile_session_rows(actual, expected).is_some()
}

/// Session project-path normalization boundary. Resolves a session row's
/// project path to its canonical on-disk form so that symlinked family roots
/// (e.g. `/home/zack/projects` vs `/fast/projects`) converge to a single
/// spelling before they are persisted or compared.
///
/// Called from [`apply_session`](super::apply) at ingest, and from verify/rebuild
/// on the re-derived expected row so host spellings match the persisted form.
/// [`reconcile_session_rows`] itself stays pure string/shape logic with no
/// filesystem access. macOS `/var` firmlink expansions are collapsed back to the
/// public `/var/...` spelling inside [`canonical_project_path`].
pub(super) fn canonicalize_session_project_paths(session: &SessionRecord) -> SessionRecord {
    let Some(canonical) = canonical_project_path(&session.project_path) else {
        return session.clone();
    };
    let mut normalized = session.clone();
    // A path-shaped key must track the canonical path so downstream pure
    // reconciliation keeps recognizing the two spellings as one family root.
    if session.project_key == session.project_path {
        normalized.project_key.clone_from(&canonical);
    }
    normalized.project_path = canonical;
    normalized
}

/// Converge path spellings persisted before ingest-side canonicalization.
///
/// Distinct paths are resolved before any write, then each verified alias is
/// updated idempotently. Updating `project_key` only when it matched the old
/// path preserves provider-native keys while keeping path-shaped keys aligned.
pub async fn converge_session_project_paths(conn: &impl Executor) -> ProjectionStoreResult<()> {
    let mut rows = conn
        .query("SELECT DISTINCT project_path FROM sessions", ())
        .await
        .map_err(|error| storage("list session project paths", error))?;
    let mut repairs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read session project paths", error))?
    {
        let project_path = row
            .get::<String>(0)
            .map_err(|error| storage("decode session project path", error))?;
        if let Some(canonical) = canonical_project_path(&project_path) {
            repairs.push((project_path, canonical));
        }
    }
    drop(rows);

    // Batch every distinct drifted path into one statement per chunk via a
    // `VALUES` CTE, rather than one `UPDATE ... WHERE project_path = ?1` per
    // path. The old_path -> canonical mapping still varies per row (unlike
    // the retention passes' shared marker), so each chunk binds the mapping
    // as a small `repair` table and resolves it with a correlated subquery
    // keyed on `sessions.project_path`, which is exactly the old loop's
    // per-path `WHERE`/`SET` pairing collapsed into one pass. Chunked at 400
    // paths (2 params each) to stay under SQLite's default bound-parameter
    // limit (999).
    const CONVERGE_CHUNK: usize = 400;
    for chunk in repairs.chunks(CONVERGE_CHUNK) {
        let values_clause = vec!["(?, ?)"; chunk.len()].join(",");
        let sql = format!(
            "WITH repair(old_path, canonical_path) AS (VALUES {values_clause})
             UPDATE sessions
             SET project_key = CASE
                     WHEN project_key = (
                         SELECT old_path FROM repair WHERE old_path = sessions.project_path
                     )
                     THEN (
                         SELECT canonical_path FROM repair WHERE old_path = sessions.project_path
                     )
                     ELSE project_key
                 END,
                 project_path = (
                     SELECT canonical_path FROM repair WHERE old_path = sessions.project_path
                 )
             WHERE project_path IN (SELECT old_path FROM repair)"
        );
        let mut values = Vec::with_capacity(chunk.len() * 2);
        for (project_path, canonical) in chunk {
            values.push(Value::Text(project_path.clone()));
            values.push(Value::Text(canonical.clone()));
        }
        conn.execute(&sql, values)
            .await
            .map_err(|error| storage("converge session project path", error))?;
    }
    Ok(())
}

/// Resolve a project-path string to its canonical on-disk form, returning
/// `Some` only when the path exists and its canonical spelling differs. Non
/// paths and vanished paths yield `None`, so identity is only widened by
/// verifiable filesystem evidence.
///
/// On macOS, `Path::canonicalize` expands firmlinks such as `/var` ->
/// `/private/var`. Prefer the stable public `/var/...` spelling (same policy as
/// [`tracedecay_sessions::runtime::git_correlation::normalize_worktree`]) so host-reported
/// temp/project roots are not rewritten into a form that breaks search keys and
/// authority verify against the original observation path.
fn canonical_project_path(path: &str) -> Option<String> {
    let canonical = std::path::Path::new(path).canonicalize().ok()?;
    let mut canonical = canonical.to_string_lossy().into_owned();
    if let Some(stripped) = canonical.strip_prefix("/private/var/") {
        canonical = format!("/var/{stripped}");
    }
    (canonical != path).then_some(canonical)
}

/// Reconcile two stored session rows into one merged row using pure
/// string/shape logic only. Project-path family identity is resolved earlier,
/// at the apply-side ingest boundary ([`canonicalize_session_project_paths`]),
/// so this function — reached from the verify/audit and rebuild paths as well
/// as apply — never touches the filesystem and stays reproducible from stored
/// evidence.
pub(super) fn reconcile_session_rows(
    actual: &SessionRecord,
    expected: &SessionRecord,
) -> Option<SessionRecord> {
    if actual.provider != expected.provider || actual.session_id != expected.session_id {
        return None;
    }
    let project_key = if actual.project_key == expected.project_key {
        actual.project_key.clone()
    } else if actual.project_key == "user" {
        expected.project_key.clone()
    } else if expected.project_key == "user" {
        actual.project_key.clone()
    } else if actual.project_key == actual.project_path
        && actual.project_path == expected.project_path
    {
        expected.project_key.clone()
    } else if expected.project_key == expected.project_path
        && expected.project_path == actual.project_path
    {
        actual.project_key.clone()
    } else {
        return None;
    };
    let project_path = if actual.project_path == expected.project_path {
        actual.project_path.clone()
    } else if actual.project_path == actual.project_key {
        expected.project_path.clone()
    } else if expected.project_path == expected.project_key {
        actual.project_path.clone()
    } else {
        return None;
    };
    Some(SessionRecord {
        provider: actual.provider.clone(),
        session_id: actual.session_id.clone(),
        project_key,
        project_path,
        title: actual.title.clone().or_else(|| expected.title.clone()),
        started_at: actual
            .started_at
            .into_iter()
            .chain(expected.started_at)
            .min(),
        ended_at: actual.ended_at.into_iter().chain(expected.ended_at).max(),
        transcript_path: reconcile_optional(
            actual.transcript_path.as_ref(),
            expected.transcript_path.as_ref(),
        )
        .ok()?,
        metadata_json: reconcile_metadata(
            actual.metadata_json.as_ref(),
            expected.metadata_json.as_ref(),
        )
        .ok()?,
        parent_session_id: reconcile_optional(
            actual.parent_session_id.as_ref(),
            expected.parent_session_id.as_ref(),
        )
        .ok()?,
        is_subagent: actual.is_subagent || expected.is_subagent,
        agent_id: reconcile_optional(actual.agent_id.as_ref(), expected.agent_id.as_ref()).ok()?,
        parent_tool_use_id: reconcile_optional(
            actual.parent_tool_use_id.as_ref(),
            expected.parent_tool_use_id.as_ref(),
        )
        .ok()?,
    })
}

#[derive(Debug)]
struct ReconcileConflict;

fn reconcile_optional<T: Clone + Eq>(
    actual: Option<&T>,
    expected: Option<&T>,
) -> Result<Option<T>, ReconcileConflict> {
    match (actual, expected) {
        (Some(actual), Some(expected)) if actual != expected => Err(ReconcileConflict),
        (Some(actual), _) => Ok(Some(actual.clone())),
        (_, Some(expected)) => Ok(Some(expected.clone())),
        (None, None) => Ok(None),
    }
}

fn reconcile_metadata(
    actual: Option<&String>,
    expected: Option<&String>,
) -> Result<Option<String>, ReconcileConflict> {
    let (Some(actual), Some(expected)) = (actual, expected) else {
        return reconcile_optional(actual, expected);
    };
    if actual == expected {
        return Ok(Some(actual.clone()));
    }
    let mut actual: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(actual).map_err(|_| ReconcileConflict)?;
    let expected: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(expected).map_err(|_| ReconcileConflict)?;
    for (key, expected_value) in expected {
        match actual.get_mut(&key) {
            None => {
                actual.insert(key, expected_value);
            }
            Some(actual_value) if *actual_value == expected_value => {}
            Some(actual_value) if key == "usage" => {
                *actual_value =
                    reconcile_usage(actual_value, &expected_value).ok_or(ReconcileConflict)?;
            }
            Some(_) if key == "source" => {}
            Some(_) => return Err(ReconcileConflict),
        }
    }
    serde_json::to_string(&actual)
        .map(Some)
        .map_err(|_| ReconcileConflict)
}

fn reconcile_usage(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> Option<serde_json::Value> {
    if actual == expected {
        return Some(actual.clone());
    }
    match (actual, expected) {
        (serde_json::Value::Number(actual), serde_json::Value::Number(expected)) => Some(
            serde_json::Value::from(actual.as_u64()?.max(expected.as_u64()?)),
        ),
        (serde_json::Value::Object(actual), serde_json::Value::Object(expected)) => {
            let mut merged = actual.clone();
            for (key, expected_value) in expected {
                match merged.get_mut(key) {
                    None => {
                        merged.insert(key.clone(), expected_value.clone());
                    }
                    Some(actual_value) => {
                        *actual_value = reconcile_usage(actual_value, expected_value)?;
                    }
                }
            }
            Some(serde_json::Value::Object(merged))
        }
        _ => None,
    }
}

pub(super) async fn protected_message_rows_compatible(
    conn: &impl QueryExecutor,
    actual: &SessionMessageRecord,
    expected: &SessionMessageRecord,
) -> ProjectionStoreResult<bool> {
    if actual.provider != expected.provider
        || actual.message_id != expected.message_id
        || actual.session_id != expected.session_id
        || actual.role != expected.role
        || actual.timestamp != expected.timestamp
        || actual.ordinal != expected.ordinal
        || actual.kind != expected.kind
        || actual.model != expected.model
        || actual.tool_names != expected.tool_names
        || actual.source_path != expected.source_path
        || actual.source_offset != expected.source_offset
    {
        return Ok(false);
    }
    let Some(metadata) = actual
        .metadata_json
        .as_deref()
        .and_then(|encoded| serde_json::from_str::<serde_json::Value>(encoded).ok())
    else {
        return Ok(false);
    };
    let Some(payload_ref) = metadata
        .get("payload_ref")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(false);
    };
    let expected_hash = projected_content_hash(&expected.text);
    if metadata
        .get("external_payload")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || metadata.get("sha256").and_then(serde_json::Value::as_str)
            != Some(expected_hash.as_str())
        || !actual.text.contains(payload_ref)
    {
        return Ok(false);
    }
    let mut rows = conn
        .query(
            "SELECT CAST(COALESCE(content, '') AS TEXT),
                    CAST(COALESCE(content_hash, '') AS TEXT),
                    CAST(COALESCE(storage_kind, '') AS TEXT),
                    CAST(COALESCE(payload_ref, '') AS TEXT)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2
             LIMIT 2",
            params![actual.provider.as_str(), actual.message_id.as_str()],
        )
        .await
        .map_err(|error| storage("read protected projection output", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read protected projection output", error))?
    else {
        return Ok(false);
    };
    let compatible = row
        .get::<String>(0)
        .map_err(|error| storage("read protected projection output", error))?
        .is_empty()
        && row
            .get::<String>(1)
            .map_err(|error| storage("read protected projection output", error))?
            == expected_hash
        && row
            .get::<String>(2)
            .map_err(|error| storage("read protected projection output", error))?
            == "external"
        && row
            .get::<String>(3)
            .map_err(|error| storage("read protected projection output", error))?
            == payload_ref;
    if rows
        .next()
        .await
        .map_err(|error| storage("read protected projection output", error))?
        .is_some()
    {
        return Ok(false);
    }
    Ok(compatible)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod reconcile_tests {
    use tracedecay_store::SessionRecord;

    use super::reconcile_session_rows;
    #[cfg(unix)]
    use super::{canonicalize_session_project_paths, converge_session_project_paths};
    #[cfg(unix)]
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection};

    fn record(project_path: &str) -> SessionRecord {
        SessionRecord {
            provider: "codex".to_owned(),
            session_id: "session-family".to_owned(),
            project_key: project_path.to_owned(),
            project_path: project_path.to_owned(),
            title: None,
            started_at: Some(1),
            ended_at: Some(2),
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_family_roots_reconcile_after_ingest_normalization() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("fast-projects").join("repo");
        std::fs::create_dir_all(&real).unwrap();
        let alias_parent = tmp.path().join("home-projects");
        std::os::unix::fs::symlink(tmp.path().join("fast-projects"), &alias_parent).unwrap();
        let aliased = alias_parent.join("repo");

        // The apply-side ingest boundary resolves each family spelling to the
        // canonical on-disk form; reconciliation itself is pure string logic.
        let normalized_alias =
            canonicalize_session_project_paths(&record(&aliased.to_string_lossy()));
        let normalized_real = canonicalize_session_project_paths(&record(&real.to_string_lossy()));
        assert_eq!(normalized_alias.project_path, normalized_real.project_path);
        assert_eq!(normalized_alias.project_key, normalized_real.project_key);
        assert_ne!(
            normalized_alias.project_path,
            aliased.to_string_lossy(),
            "user symlink families must converge away from the alias spelling"
        );

        let merged = reconcile_session_rows(&normalized_alias, &normalized_real)
            .expect("normalized symlink families naming one directory must reconcile");
        assert_eq!(merged.project_path, normalized_real.project_path);
        assert_eq!(merged.project_key, normalized_real.project_key);

        // Symmetric: order must not change the merged identity.
        let merged_reversed = reconcile_session_rows(&normalized_real, &normalized_alias).unwrap();
        assert_eq!(merged_reversed.project_path, merged.project_path);

        // The audit path stays pure: two live family spellings that were never
        // normalized at ingest do not silently merge via filesystem probing.
        assert!(
            reconcile_session_rows(
                &record(&aliased.to_string_lossy()),
                &record(&real.to_string_lossy()),
            )
            .is_none(),
            "reconcile must not canonicalize; family identity is an ingest concern"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_session_paths_converge_without_rewriting_native_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("fast-projects").join("repo");
        std::fs::create_dir_all(&real).unwrap();
        let alias_parent = tmp.path().join("home-projects");
        std::os::unix::fs::symlink(tmp.path().join("fast-projects"), &alias_parent).unwrap();
        let aliased = alias_parent.join("repo");
        let connection = TestConnection::open(&tmp.path().join("sessions.db"));
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    project_key TEXT NOT NULL,
                    project_path TEXT NOT NULL
                 );",
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('codex', 'path-key', ?1, ?1),
                        ('codex', 'native-key', 'project-id', ?1)",
                (aliased.to_string_lossy().as_ref(),),
            )
            .await
            .unwrap();

        converge_session_project_paths(&connection).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT session_id, project_key, project_path
                 FROM sessions ORDER BY session_id",
                (),
            )
            .await
            .unwrap();
        let native = rows.next().await.unwrap().unwrap();
        assert_eq!(native.get::<String>(0).unwrap(), "native-key");
        assert_eq!(native.get::<String>(1).unwrap(), "project-id");
        assert_eq!(
            native.get::<String>(2).unwrap(),
            real.to_string_lossy().as_ref()
        );
        let path = rows.next().await.unwrap().unwrap();
        assert_eq!(path.get::<String>(0).unwrap(), "path-key");
        assert_eq!(
            path.get::<String>(1).unwrap(),
            real.to_string_lossy().as_ref()
        );
        assert_eq!(
            path.get::<String>(2).unwrap(),
            real.to_string_lossy().as_ref()
        );
    }

    /// The batched `WITH repair(...) AS (VALUES ...)` statement maps each
    /// distinct drifted `project_path` to its own canonical target within one
    /// chunked pass. Two independent families in the same batch must each
    /// resolve to their own canonical spelling, not cross-contaminate.
    #[cfg(unix)]
    #[tokio::test]
    async fn independent_families_converge_to_distinct_canonical_paths_in_one_batch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_a = tmp.path().join("fast-projects").join("repo-a");
        let real_b = tmp.path().join("fast-projects").join("repo-b");
        std::fs::create_dir_all(&real_a).unwrap();
        std::fs::create_dir_all(&real_b).unwrap();
        let alias_parent = tmp.path().join("home-projects");
        std::os::unix::fs::symlink(tmp.path().join("fast-projects"), &alias_parent).unwrap();
        let aliased_a = alias_parent.join("repo-a");
        let aliased_b = alias_parent.join("repo-b");

        let connection = TestConnection::open(&tmp.path().join("sessions.db"));
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    project_key TEXT NOT NULL,
                    project_path TEXT NOT NULL
                 );",
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('codex', 'session-a', ?1, ?1),
                        ('codex', 'session-b', ?2, ?2)",
                (
                    aliased_a.to_string_lossy().as_ref(),
                    aliased_b.to_string_lossy().as_ref(),
                ),
            )
            .await
            .unwrap();

        converge_session_project_paths(&connection).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT session_id, project_key, project_path
                 FROM sessions ORDER BY session_id",
                (),
            )
            .await
            .unwrap();
        let session_a = rows.next().await.unwrap().unwrap();
        assert_eq!(session_a.get::<String>(0).unwrap(), "session-a");
        assert_eq!(
            session_a.get::<String>(1).unwrap(),
            real_a.to_string_lossy().as_ref()
        );
        assert_eq!(
            session_a.get::<String>(2).unwrap(),
            real_a.to_string_lossy().as_ref()
        );
        let session_b = rows.next().await.unwrap().unwrap();
        assert_eq!(session_b.get::<String>(0).unwrap(), "session-b");
        assert_eq!(
            session_b.get::<String>(1).unwrap(),
            real_b.to_string_lossy().as_ref()
        );
        assert_eq!(
            session_b.get::<String>(2).unwrap(),
            real_b.to_string_lossy().as_ref()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_temp_firmlink_spelling_is_preserved_at_ingest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let host = project.to_string_lossy().into_owned();
        assert!(
            host.starts_with("/var/"),
            "expected macOS temp host spelling under /var, got {host}"
        );
        let normalized = canonicalize_session_project_paths(&record(&host));
        assert_eq!(
            normalized.project_path, host,
            "macOS /var firmlink expansion must not rewrite host-facing project paths"
        );
        assert_eq!(normalized.project_key, host);
    }

    #[test]
    fn genuinely_different_roots_still_refuse_to_reconcile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        assert!(
            reconcile_session_rows(
                &record(&first.to_string_lossy()),
                &record(&second.to_string_lossy()),
            )
            .is_none(),
            "distinct directories must never merge"
        );
    }
}
