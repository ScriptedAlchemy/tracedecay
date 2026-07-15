use libsql::{Connection, params};
use tracedecay_domain::{CanonicalObservationIdV1, DurableClaudeObservationV1};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeSessionMessageProjection, ProjectionCheckpoint,
    ProjectionPersistOutcome, ProjectionRebuildOutcome, ProjectionStoreError,
    ProjectionStoreResult, project_claude_observation,
};

use super::GlobalDb;

pub(super) async fn ensure_observation_projection_schema(
    conn: &Connection,
) -> Result<(), libsql::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS observation_projection_provenance (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            PRIMARY KEY(projector_version, observation_id),
            UNIQUE(projector_version, output_provider, output_message_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_checkpoints (
            projector_version TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0)
        );",
    )
    .await
    .map(|_| ())
}

fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ProjectionStoreError {
    ProjectionStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn storage_message(operation: &'static str, message: impl Into<String>) -> ProjectionStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

fn decode_sequence(value: i64, operation: &'static str) -> ProjectionStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}

async fn read_observation(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<(u64, DurableClaudeObservationV1)>> {
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
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read queued observation", error))?,
        "read queued observation",
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage("read queued observation", error))?;
    let observation = serde_json::from_str(&observation_json)
        .map_err(|error| storage("decode queued observation", error))?;
    Ok(Some((sequence, observation)))
}

async fn read_checkpoint(conn: &Connection) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
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

async fn write_checkpoint(
    conn: &Connection,
    sequence: u64,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let sequence_i64 =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, ?2)
         ON CONFLICT(projector_version) DO UPDATE SET last_sequence = excluded.last_sequence",
        params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, sequence_i64],
    )
    .await
    .map_err(|error| storage("write projector checkpoint", error))?;
    Ok(ProjectionCheckpoint::new(sequence))
}

async fn queued_sequence(
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

async fn read_session(
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
    super::row_to_session_result(&row)
        .map(Some)
        .map_err(|error| storage("decode projected session", error))
}

async fn read_message(
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
    super::row_to_message(&row, 0)
        .map(Some)
        .ok_or_else(|| storage_message("decode projected message", "invalid row"))
}

async fn verify_rows(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    if read_session(conn, &session.provider, &session.session_id)
        .await?
        .as_ref()
        != Some(session)
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: session.provider.clone(),
            message_id: format!("session:{}", session.session_id),
        });
    }
    let message = projection.message();
    if read_message(conn, &message.provider, &message.message_id)
        .await?
        .as_ref()
        != Some(message)
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: message.provider.clone(),
            message_id: message.message_id.clone(),
        });
    }
    Ok(())
}

async fn apply_rows(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    conn.execute(
        "INSERT INTO sessions
            (provider, session_id, project_key, project_path, title, started_at, ended_at,
             transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
             parent_tool_use_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(provider, session_id) DO NOTHING",
        params![
            session.provider.as_str(),
            session.session_id.as_str(),
            session.project_key.as_str(),
            session.project_path.as_str(),
            super::opt_text(session.title.as_deref()),
            super::opt_i64(session.started_at),
            super::opt_i64(session.ended_at),
            super::opt_text(session.transcript_path.as_deref()),
            super::opt_text(session.metadata_json.as_deref()),
            super::opt_text(session.parent_session_id.as_deref()),
            i64::from(session.is_subagent),
            super::opt_text(session.agent_id.as_deref()),
            super::opt_text(session.parent_tool_use_id.as_deref()),
        ],
    )
    .await
    .map_err(|error| storage("insert projected session", error))?;

    let message = projection.message();
    conn.execute(
        "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(provider, message_id) DO NOTHING",
        params![
            message.provider.as_str(),
            message.message_id.as_str(),
            message.session_id.as_str(),
            message.role.as_str(),
            super::opt_i64(message.timestamp),
            message.ordinal,
            message.text.as_str(),
            super::opt_text(message.kind.as_deref()),
            super::opt_text(message.model.as_deref()),
            super::opt_text(message.tool_names.as_deref()),
            super::opt_text(message.source_path.as_deref()),
            super::opt_i64(message.source_offset),
            super::opt_text(message.metadata_json.as_deref()),
        ],
    )
    .await
    .map_err(|error| storage("insert projected message", error))?;
    verify_rows(conn, projection).await
}

async fn verify_provenance(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT receipt_id, output_provider, output_message_id, output_digest
             FROM observation_projection_provenance
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                provenance.projector_version(),
                provenance.observation_id().as_str()
            ],
        )
        .await
        .map_err(|error| storage("verify projection provenance", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("verify projection provenance", error))?
    else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };
    let actual = (
        row.get::<String>(0)
            .map_err(|error| storage("verify projection provenance", error))?,
        row.get::<String>(1)
            .map_err(|error| storage("verify projection provenance", error))?,
        row.get::<String>(2)
            .map_err(|error| storage("verify projection provenance", error))?,
        row.get::<String>(3)
            .map_err(|error| storage("verify projection provenance", error))?,
    );
    let expected = (
        provenance.receipt_id().to_string(),
        message.provider.clone(),
        message.message_id.clone(),
        projection.output_digest().as_str().to_string(),
    );
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn apply_provenance(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    conn.execute(
        "INSERT INTO observation_projection_provenance
            (projector_version, observation_id, receipt_id, output_provider,
             output_message_id, output_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(projector_version, observation_id) DO NOTHING",
        params![
            provenance.projector_version(),
            provenance.observation_id().as_str(),
            provenance.receipt_id(),
            message.provider.as_str(),
            message.message_id.as_str(),
            projection.output_digest().as_str(),
        ],
    )
    .await
    .map_err(|error| storage("insert projection provenance", error))?;
    verify_provenance(conn, projection).await
}

impl GlobalDb {
    pub(crate) async fn project_observation_result(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ProjectionStoreResult<ProjectionPersistOutcome> {
        let _writer = self.transaction.lock().await;
        let transaction = self
            .begin_authoritative_transaction()
            .await
            .map_err(|error| storage("begin projection transaction", error))?;
        let checkpoint = read_checkpoint(&transaction).await?;
        let Some((sequence, observation)) = read_observation(&transaction, observation_id).await?
        else {
            return Err(ProjectionStoreError::ObservationNotFound);
        };
        let projection = project_claude_observation(&observation)?;
        if sequence <= checkpoint.last_sequence() {
            verify_rows(&transaction, &projection).await?;
            verify_provenance(&transaction, &projection).await?;
            return Ok(ProjectionPersistOutcome::ExactDuplicate(checkpoint));
        }
        let expected = checkpoint.last_sequence().saturating_add(1);
        if sequence != expected {
            return Err(ProjectionStoreError::Gap {
                expected,
                actual: sequence,
            });
        }
        if queued_sequence(&transaction, observation_id).await? != Some(sequence) {
            return Err(ProjectionStoreError::NotQueued);
        }

        apply_rows(&transaction, &projection).await?;
        apply_provenance(&transaction, &projection).await?;
        transaction
            .execute(
                "DELETE FROM projection_queue WHERE observation_id = ?1",
                params![observation_id.as_str()],
            )
            .await
            .map_err(|error| storage("consume projection queue item", error))?;
        let checkpoint = write_checkpoint(&transaction, sequence).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection transaction", error))?;
        Ok(ProjectionPersistOutcome::Projected(checkpoint))
    }

    pub(crate) async fn projection_checkpoint_result(
        &self,
    ) -> ProjectionStoreResult<ProjectionCheckpoint> {
        let _reader = self.transaction.lock().await;
        read_checkpoint(&self.conn).await
    }

    pub(crate) async fn rebuild_projection_result(
        &self,
        frontier_sequence: u64,
    ) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
        let _writer = self.transaction.lock().await;
        let transaction = self
            .begin_authoritative_transaction()
            .await
            .map_err(|error| storage("begin projection rebuild", error))?;
        let mut max_rows = transaction
            .query("SELECT COALESCE(MAX(sequence), 0) FROM observations", ())
            .await
            .map_err(|error| storage("read projection rebuild frontier", error))?;
        let committed = max_rows
            .next()
            .await
            .map_err(|error| storage("read projection rebuild frontier", error))?
            .ok_or_else(|| storage_message("read projection rebuild frontier", "no row"))?
            .get::<i64>(0)
            .map_err(|error| storage("read projection rebuild frontier", error))?;
        drop(max_rows);
        let committed = decode_sequence(committed, "read projection rebuild frontier")?;
        if frontier_sequence > committed {
            return Err(ProjectionStoreError::InvalidRebuildFrontier {
                frontier: frontier_sequence,
                committed,
            });
        }
        let frontier_i64 = i64::try_from(frontier_sequence)
            .map_err(|_| ProjectionStoreError::SequenceOverflow(frontier_sequence))?;
        let mut rows = transaction
            .query(
                "SELECT sequence, observation_json FROM observations
                 WHERE sequence <= ?1 ORDER BY sequence ASC",
                params![frontier_i64],
            )
            .await
            .map_err(|error| storage("read projection rebuild observations", error))?;
        let mut projections = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projection rebuild observations", error))?
        {
            let sequence = decode_sequence(
                row.get::<i64>(0)
                    .map_err(|error| storage("read projection rebuild observations", error))?,
                "read projection rebuild observations",
            )?;
            let observation_json = row
                .get::<String>(1)
                .map_err(|error| storage("read projection rebuild observations", error))?;
            let observation: DurableClaudeObservationV1 =
                serde_json::from_str(&observation_json)
                    .map_err(|error| storage("decode projection rebuild observation", error))?;
            projections.push((sequence, project_claude_observation(&observation)?));
        }
        drop(rows);

        transaction
            .execute(
                "DELETE FROM session_messages
                 WHERE EXISTS (
                    SELECT 1 FROM observation_projection_provenance AS provenance
                    WHERE provenance.projector_version = ?1
                      AND provenance.output_provider = session_messages.provider
                      AND provenance.output_message_id = session_messages.message_id
                 )",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection rows for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM observation_projection_provenance WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection provenance for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM observation_projection_checkpoints WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection checkpoint for rebuild", error))?;

        for (_, projection) in &projections {
            apply_rows(&transaction, projection).await?;
            apply_provenance(&transaction, projection).await?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO projection_queue (observation_id, observation_sequence)
                 SELECT observation_id, sequence FROM observations WHERE sequence > ?1",
                params![frontier_i64],
            )
            .await
            .map_err(|error| storage("requeue observations past rebuild frontier", error))?;
        transaction
            .execute(
                "DELETE FROM projection_queue WHERE observation_sequence <= ?1",
                params![frontier_i64],
            )
            .await
            .map_err(|error| storage("consume rebuilt projection queue", error))?;
        let checkpoint = write_checkpoint(&transaction, frontier_sequence).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection rebuild", error))?;
        Ok(ProjectionRebuildOutcome::new(checkpoint, projections.len()))
    }
}
