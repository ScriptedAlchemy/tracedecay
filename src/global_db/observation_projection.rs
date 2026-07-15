use libsql::{Connection, TransactionBehavior, params};
use tracedecay_domain::{CanonicalObservationIdV1, DurableClaudeObservationV1, ObservationScopeV1};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection,
    ClaudeSessionMessageProjection, ProjectionCheckpoint, ProjectionPersistOutcome,
    ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
};

use crate::sessions::SessionRecord;
use crate::sessions::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
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
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            PRIMARY KEY(projector_version, observation_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_checkpoints (
            projector_version TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_aliases (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            PRIMARY KEY(projector_version, observation_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TABLE IF NOT EXISTS observation_projection_dispositions (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            PRIMARY KEY(projector_version, observation_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );",
    )
    .await?;
    migrate_legacy_projection_output_uniqueness(conn).await?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_observation_projection_provenance_output
         ON observation_projection_provenance
            (projector_version, output_provider, output_message_id);",
    )
    .await?;
    Ok(())
}

async fn has_legacy_projection_output_uniqueness(conn: &Connection) -> Result<bool, libsql::Error> {
    let mut rows = conn
        .query("PRAGMA index_list(observation_projection_provenance)", ())
        .await?;
    let mut unique_indexes = Vec::new();
    while let Some(row) = rows.next().await? {
        if row.get::<i64>(2)? != 0 {
            unique_indexes.push(row.get::<String>(1)?);
        }
    }
    drop(rows);

    for index_name in unique_indexes {
        let mut columns = conn
            .query(
                "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
                params![index_name],
            )
            .await?;
        let mut names = Vec::new();
        while let Some(row) = columns.next().await? {
            names.push(row.get::<String>(0)?);
        }
        if names == ["projector_version", "output_provider", "output_message_id"] {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn migrate_legacy_projection_output_uniqueness(
    conn: &Connection,
) -> Result<(), libsql::Error> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    if !has_legacy_projection_output_uniqueness(&transaction).await? {
        transaction.commit().await?;
        return Ok(());
    }

    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS observation_projection_provenance_without_output_unique;
             CREATE TABLE observation_projection_provenance_without_output_unique (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                output_digest TEXT NOT NULL,
                message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
                PRIMARY KEY(projector_version, observation_id),
                FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );
             INSERT INTO observation_projection_provenance_without_output_unique
                (projector_version, observation_id, receipt_id, output_provider,
                 output_message_id, output_digest, message_created)
             SELECT projector_version, observation_id, receipt_id, output_provider,
                    output_message_id, output_digest, message_created
             FROM observation_projection_provenance;
             DROP TABLE observation_projection_provenance;
             ALTER TABLE observation_projection_provenance_without_output_unique
                RENAME TO observation_projection_provenance;",
        )
        .await?;
    transaction.commit().await
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

fn decode_observation_row(
    row: &libsql::Row,
    operation: &'static str,
) -> ProjectionStoreResult<(u64, DurableClaudeObservationV1)> {
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
    decode_observation_row(&row, "read queued observation").map(Some)
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

async fn consume_projection_queue_item(
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

struct ProjectionOutputOwner {
    sequence: u64,
    observation: DurableClaudeObservationV1,
}

struct ProjectionOutputOwners {
    owners: Vec<ProjectionOutputOwner>,
    projector_owned: bool,
}

impl ProjectionOutputOwners {
    fn latest(&self) -> Option<&ProjectionOutputOwner> {
        self.owners.last()
    }

    fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }
}

async fn read_output_owners(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<ProjectionOutputOwners> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT observations.sequence, observations.observation_json,
                    EXISTS (
                        SELECT 1
                        FROM observation_projection_provenance AS owned
                        WHERE owned.projector_version = provenance.projector_version
                          AND owned.output_provider = provenance.output_provider
                          AND owned.output_message_id = provenance.output_message_id
                          AND owned.message_created = 1
                    )
             FROM observation_projection_provenance AS provenance
             JOIN observations
               ON observations.observation_id = provenance.observation_id
             WHERE provenance.projector_version = ?1
               AND provenance.output_provider = ?2
               AND provenance.output_message_id = ?3
             ORDER BY observations.sequence ASC",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage("read projection output owners", error))?;
    let mut owners = Vec::new();
    let mut projector_owned = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection output owners", error))?
    {
        let (sequence, observation) =
            decode_observation_row(&row, "read projection output owners")?;
        projector_owned = row
            .get::<i64>(2)
            .map_err(|error| storage("read projection output owners", error))?
            != 0;
        owners.push(ProjectionOutputOwner {
            sequence,
            observation,
        });
    }
    Ok(ProjectionOutputOwners {
        owners,
        projector_owned,
    })
}

async fn message_projection(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
) -> ProjectionStoreResult<ClaudeSessionMessageProjection> {
    match derive_projection_with_alias(conn, observation).await? {
        ClaudeObservationProjection::Message(projection) => Ok(*projection),
        ClaudeObservationProjection::Skipped(_) => Err(ProjectionStoreError::ProvenanceCollision),
    }
}

async fn verify_rows(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
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

fn same_projection_lineage(
    candidate: &DurableClaudeObservationV1,
    owner: &DurableClaudeObservationV1,
) -> bool {
    candidate.source() == owner.source() && candidate.scope() == owner.scope()
}

async fn verify_output_state(
    conn: &Connection,
    projection: &ClaudeSessionMessageProjection,
    owners: &ProjectionOutputOwners,
) -> ProjectionStoreResult<()> {
    let message = projection.message();
    let Some(latest) = owners.latest() else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };

    if owners.projector_owned {
        let owner_projection = message_projection(conn, &latest.observation).await?;
        verify_provenance(conn, &owner_projection).await?;
        return verify_rows(conn, &owner_projection).await;
    }

    let actual = read_message(conn, &message.provider, &message.message_id)
        .await?
        .ok_or_else(|| ProjectionStoreError::OutputCollision {
            provider: message.provider.clone(),
            message_id: message.message_id.clone(),
        })?;
    for owner in &owners.owners {
        let owner_projection = message_projection(conn, &owner.observation).await?;
        verify_provenance(conn, &owner_projection).await?;
        if message_rows_compatible(&actual, owner_projection.message()) {
            return verify_rows(conn, &owner_projection).await;
        }
    }
    Err(ProjectionStoreError::OutputCollision {
        provider: message.provider.clone(),
        message_id: message.message_id.clone(),
    })
}

fn session_rows_compatible(
    actual: &crate::sessions::SessionRecord,
    expected: &crate::sessions::SessionRecord,
) -> bool {
    actual.provider == expected.provider && actual.session_id == expected.session_id
}

fn message_rows_compatible(
    actual: &crate::sessions::SessionMessageRecord,
    expected: &crate::sessions::SessionMessageRecord,
) -> bool {
    actual == expected
}

fn derive_projection(
    observation: &DurableClaudeObservationV1,
) -> ProjectionStoreResult<ClaudeObservationProjection> {
    let session_id = observation.source().session_id().as_str();
    let (project_key, project_path) = match observation.scope() {
        ObservationScopeV1::Profile => ("user", "user"),
        ObservationScopeV1::Project { project_id } => (project_id.as_str(), project_id.as_str()),
    };
    let context = ClaudeRecordContext {
        session_id,
        project_key,
        project_path,
        file_generation: observation.identity().generation().file_id(),
        offset: observation.identity().position().start(),
        session_cwd: None,
    };

    match map_sanitized_claude_record(observation.payload(), &context) {
        ClaudeRecordDisposition::Message { draft, message } => {
            let draft = *draft;
            let message = *message;
            let timestamp = message.timestamp;
            let session = SessionRecord {
                provider: "claude".to_string(),
                session_id: draft.session_id,
                project_key: draft.project_key,
                project_path: draft.project_path,
                title: draft.title,
                started_at: timestamp,
                ended_at: timestamp,
                transcript_path: None,
                metadata_json: draft.metadata_json,
                parent_session_id: draft.parent_session_id,
                is_subagent: draft.is_subagent,
                agent_id: draft.agent_id,
                parent_tool_use_id: draft.parent_tool_use_id,
            };
            ClaudeObservationProjection::for_message(observation, session, message)
        }
        ClaudeRecordDisposition::NonConversational { .. } => ClaudeObservationProjection::for_skip(
            observation,
            ProjectionSkipReason::NonConversationalRecord,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionOutputAlias {
    provider: String,
    message_id: String,
}

async fn read_projection_alias(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<ProjectionOutputAlias>> {
    let mut rows = conn
        .query(
            "SELECT output_provider, output_message_id
             FROM observation_projection_aliases
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage("read projection output alias", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection output alias", error))?
    else {
        return Ok(None);
    };
    Ok(Some(ProjectionOutputAlias {
        provider: row
            .get(0)
            .map_err(|error| storage("read projection output alias", error))?,
        message_id: row
            .get(1)
            .map_err(|error| storage("read projection output alias", error))?,
    }))
}

async fn derive_projection_with_alias(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
) -> ProjectionStoreResult<ClaudeObservationProjection> {
    let projection = derive_projection(observation)?;
    let Some(alias) = read_projection_alias(conn, observation.observation_id()).await? else {
        return Ok(projection);
    };
    let ClaudeObservationProjection::Message(projection) = projection else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };
    let mut session = projection.session().clone();
    let mut message = projection.message().clone();
    session.provider.clone_from(&alias.provider);
    message.provider = alias.provider;
    message.message_id = alias.message_id;
    ClaudeObservationProjection::for_message(observation, session, message)
}

async fn apply_rows(
    conn: &Connection,
    sequence: u64,
    observation: &DurableClaudeObservationV1,
    projection: &ClaudeSessionMessageProjection,
) -> ProjectionStoreResult<bool> {
    let session = projection.session();
    match read_session(conn, &session.provider, &session.session_id).await? {
        Some(actual) if session_rows_compatible(&actual, session) => {}
        Some(_) => {
            return Err(ProjectionStoreError::OutputCollision {
                provider: session.provider.clone(),
                message_id: format!("session:{}", session.session_id),
            });
        }
        None => {
            conn.execute(
                "INSERT INTO sessions
            (provider, session_id, project_key, project_path, title, started_at, ended_at,
             transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
             parent_tool_use_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        }
    }

    let message = projection.message();
    let existing = read_message(conn, &message.provider, &message.message_id).await?;
    let owners = read_output_owners(conn, projection).await?;
    let message_created = match (existing, owners.is_empty()) {
        (Some(actual), false) => {
            verify_output_state(conn, projection, &owners).await?;
            let latest = owners
                .latest()
                .ok_or(ProjectionStoreError::ProvenanceCollision)?;
            if !same_projection_lineage(observation, &latest.observation) {
                return Err(ProjectionStoreError::OutputCollision {
                    provider: message.provider.clone(),
                    message_id: message.message_id.clone(),
                });
            }

            if sequence < latest.sequence {
                false
            } else if observation.identity().generation()
                == latest.observation.identity().generation()
            {
                if !message_rows_compatible(&actual, message) {
                    return Err(ProjectionStoreError::OutputCollision {
                        provider: message.provider.clone(),
                        message_id: message.message_id.clone(),
                    });
                }
                false
            } else if !owners.projector_owned || message_rows_compatible(&actual, message) {
                false
            } else {
                conn.execute(
                    "UPDATE session_messages
                     SET session_id = ?3, role = ?4, timestamp = ?5, ordinal = ?6,
                         text = ?7, kind = ?8, model = ?9, tool_names = ?10,
                         source_path = ?11, source_offset = ?12, metadata_json = ?13
                     WHERE provider = ?1 AND message_id = ?2",
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
                .map_err(|error| storage("supersede projected message", error))?;
                false
            }
        }
        (Some(actual), true) if message_rows_compatible(&actual, message) => false,
        (Some(_), true) | (None, false) => {
            return Err(ProjectionStoreError::OutputCollision {
                provider: message.provider.clone(),
                message_id: message.message_id.clone(),
            });
        }
        (None, true) => {
            conn.execute(
                "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
            true
        }
    };
    Ok(message_created)
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
    message_created: bool,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    conn.execute(
        "INSERT INTO observation_projection_provenance
            (projector_version, observation_id, receipt_id, output_provider,
             output_message_id, output_digest, message_created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT DO NOTHING",
        params![
            provenance.projector_version(),
            provenance.observation_id().as_str(),
            provenance.receipt_id(),
            message.provider.as_str(),
            message.message_id.as_str(),
            projection.output_digest().as_str(),
            i64::from(message_created),
        ],
    )
    .await
    .map_err(|error| storage("insert projection provenance", error))?;
    verify_provenance(conn, projection).await
}

async fn verify_skip_disposition(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT receipt_id, reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str()
            ],
        )
        .await
        .map_err(|error| storage("verify projection disposition", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("verify projection disposition", error))?
    else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };
    let receipt_id = row
        .get::<String>(0)
        .map_err(|error| storage("verify projection disposition", error))?;
    let actual_reason = row
        .get::<String>(1)
        .map_err(|error| storage("verify projection disposition", error))?;
    let expected_receipt_id = observation.receipt().receipt().receipt_id().as_str();
    if receipt_id == expected_receipt_id && actual_reason == reason.as_str() {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn apply_skip_disposition(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO observation_projection_dispositions
            (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO NOTHING",
        params![
            CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
            observation.observation_id().as_str(),
            observation.receipt().receipt().receipt_id().as_str(),
            reason.as_str(),
        ],
    )
    .await
    .map_err(|error| storage("insert projection disposition", error))?;
    verify_skip_disposition(conn, observation, reason).await
}

async fn verify_effect(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
    effect: &ClaudeObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ClaudeObservationProjection::Message(projection) => {
            verify_provenance(conn, projection).await?;
            let owners = read_output_owners(conn, projection).await?;
            verify_output_state(conn, projection, &owners).await
        }
        ClaudeObservationProjection::Skipped(reason) => {
            verify_skip_disposition(conn, observation, *reason).await
        }
    }
}

async fn apply_effect(
    conn: &Connection,
    sequence: u64,
    observation: &DurableClaudeObservationV1,
    effect: &ClaudeObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ClaudeObservationProjection::Message(projection) => {
            let message_created = apply_rows(conn, sequence, observation, projection).await?;
            apply_provenance(conn, projection, message_created).await
        }
        ClaudeObservationProjection::Skipped(reason) => {
            apply_skip_disposition(conn, observation, *reason).await
        }
    }
}

impl GlobalDb {
    pub(crate) async fn next_queued_observation_result(
        &self,
    ) -> ProjectionStoreResult<Option<CanonicalObservationIdV1>> {
        let _reader = self.transaction.lock().await;
        let mut rows = self
            .conn
            .query(
                "SELECT observation_id FROM projection_queue
                 ORDER BY observation_sequence ASC LIMIT 1",
                (),
            )
            .await
            .map_err(|error| storage("read next projection queue item", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read next projection queue item", error))?
        else {
            return Ok(None);
        };
        let observation_id = row
            .get::<String>(0)
            .map_err(|error| storage("read next projection queue item", error))?;
        CanonicalObservationIdV1::new(observation_id)
            .map(Some)
            .map_err(ProjectionStoreError::Contract)
    }

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
        let effect = derive_projection_with_alias(&transaction, &observation).await?;
        if sequence <= checkpoint.last_sequence() {
            verify_effect(&transaction, &observation, &effect).await?;
            consume_projection_queue_item(&transaction, observation_id).await?;
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit projection transaction", error))?;
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

        apply_effect(&transaction, sequence, &observation, &effect).await?;
        consume_projection_queue_item(&transaction, observation_id).await?;
        let checkpoint = write_checkpoint(&transaction, sequence).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection transaction", error))?;
        Ok(match effect {
            ClaudeObservationProjection::Message(_) => {
                ProjectionPersistOutcome::Projected(checkpoint)
            }
            ClaudeObservationProjection::Skipped(reason) => {
                ProjectionPersistOutcome::Skipped { checkpoint, reason }
            }
        })
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
        let mut effects = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projection rebuild observations", error))?
        {
            let (sequence, observation) =
                decode_observation_row(&row, "read projection rebuild observations")?;
            let effect = derive_projection_with_alias(&transaction, &observation).await?;
            effects.push((sequence, observation, effect));
        }
        drop(rows);

        transaction
            .execute(
                "DELETE FROM session_messages
                 WHERE EXISTS (
                    SELECT 1 FROM observation_projection_provenance AS provenance
                    WHERE provenance.projector_version = ?1
                      AND provenance.message_created = 1
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
                "DELETE FROM observation_projection_dispositions WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection dispositions for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM observation_projection_checkpoints WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection checkpoint for rebuild", error))?;

        for (sequence, observation, effect) in &effects {
            apply_effect(&transaction, *sequence, observation, effect).await?;
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
        let projected_rows = effects
            .iter()
            .filter(|(_, _, effect)| matches!(effect, ClaudeObservationProjection::Message(_)))
            .count();
        Ok(ProjectionRebuildOutcome::new(
            checkpoint,
            projected_rows,
            effects.len() - projected_rows,
        ))
    }
}
