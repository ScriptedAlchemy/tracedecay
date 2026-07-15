use libsql::{Connection, params};
use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1};
use tracedecay_store::{
    ObservationProjection, ProjectionCheckpoint, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION_V1, SessionMessageProjection,
};

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
    row: &libsql::Row,
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
    conn: &Connection,
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
    conn: &Connection,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V1],
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
    conn: &Connection,
    sequence: u64,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let sequence_i64 =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, ?2)
         ON CONFLICT(projector_version) DO UPDATE SET last_sequence = excluded.last_sequence",
        params![SESSION_MESSAGE_PROJECTOR_VERSION_V1, sequence_i64],
    )
    .await
    .map_err(|error| storage("write projector checkpoint", error))?;
    Ok(ProjectionCheckpoint::new(sequence))
}

pub(super) async fn queued_sequence(
    conn: &Connection,
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
    conn: &Connection,
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
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> ProjectionStoreResult<Option<crate::sessions::SessionRecord>> {
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
    super::super::row_to_session_result(&row)
        .map(Some)
        .map_err(|error| storage("decode projected session", error))
}

pub(super) async fn read_message(
    conn: &Connection,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<crate::sessions::SessionMessageRecord>> {
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
    super::super::row_to_message(&row, 0)
        .map(Some)
        .ok_or_else(|| storage_message("decode projected message", "invalid row"))
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
    conn: &Connection,
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

pub(super) async fn read_output_state(
    conn: &Connection,
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    conn: &Connection,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<bool> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT 1 FROM observation_projection_provenance
             WHERE output_provider = ?1 AND output_message_id = ?2
               AND projector_version <> ?3
             LIMIT 1",
            params![
                message.provider.as_str(),
                message.message_id.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    conn: &Connection,
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<SessionMessageProjection> {
    match derive_projection_with_alias(conn, observation).await? {
        ObservationProjection::Message(projection) => Ok(*projection),
        ObservationProjection::Skipped(_) => Err(ProjectionStoreError::ProvenanceCollision),
    }
}

async fn verify_rows(
    conn: &Connection,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    if !read_session(conn, &session.provider, &session.session_id)
        .await?
        .as_ref()
        .is_some_and(|actual| session_rows_compatible(actual, session))
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: session.provider.clone(),
            message_id: format!("session:{}", session.session_id),
        });
    }
    let message = projection.message();
    if !read_message(conn, &message.provider, &message.message_id)
        .await?
        .as_ref()
        .is_some_and(|actual| message_rows_compatible(actual, message))
    {
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
    conn: &Connection,
    state: &ProjectionOutputState,
) -> ProjectionStoreResult<()> {
    if state.owner_count == 0 {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    let owner_projection = message_projection(conn, &state.canonical).await?;
    verify_provenance(conn, &owner_projection).await?;
    verify_rows(conn, &owner_projection).await
}

pub(in super::super) async fn verify_output_authority(
    conn: &Connection,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT MAX(message_created), COUNT(*)
             FROM observation_projection_provenance
                  INDEXED BY idx_observation_projection_provenance_global_output
             WHERE projector_version = ?1
               AND output_provider = ?2 AND output_message_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    let owner_projection = message_projection(conn, &observation).await?;
    verify_provenance(conn, &owner_projection).await?;
    verify_rows(conn, &owner_projection).await
}

pub(super) fn session_rows_compatible(
    actual: &crate::sessions::SessionRecord,
    expected: &crate::sessions::SessionRecord,
) -> bool {
    actual.provider == expected.provider && actual.session_id == expected.session_id
}

pub(super) fn message_rows_compatible(
    actual: &crate::sessions::SessionMessageRecord,
    expected: &crate::sessions::SessionMessageRecord,
) -> bool {
    actual == expected
}
