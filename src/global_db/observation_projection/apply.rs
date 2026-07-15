use libsql::{Connection, params};
use tracedecay_domain::{CanonicalObservationIdV1, DurableClaudeObservationV1, ObservationScopeV1};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection,
    ClaudeSessionMessageProjection, ProjectionSkipReason, ProjectionStoreError,
    ProjectionStoreResult,
};

use crate::sessions::SessionRecord;
use crate::sessions::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};

use super::state::{
    has_other_projector_output_owner, message_rows_compatible, read_message, read_output_state,
    read_session, same_projection_lineage, session_rows_compatible, storage, verify_output_state,
};

pub(in super::super) fn derive_projection(
    observation: &DurableClaudeObservationV1,
) -> ProjectionStoreResult<ClaudeObservationProjection> {
    let session_id = observation.source().session_id().as_str();
    let payload = observation.payload();
    let durable_message_id = payload
        .pointer("/message/id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| payload.get("uuid").and_then(serde_json::Value::as_str))
        .filter(|id| !id.is_empty());
    let durable_tool_event_ids = payload
        .pointer("/message/content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("tool_use_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
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
        raw_message_id: durable_message_id,
        raw_tool_event_ids: &durable_tool_event_ids,
    };

    match map_sanitized_claude_record(payload, &context) {
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
        ClaudeRecordDisposition::NonConversational => ClaudeObservationProjection::for_skip(
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

pub(in super::super) async fn derive_projection_with_alias(
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
    let state = read_output_state(conn, projection).await?;
    let message_created = match (existing, state) {
        (Some(actual), Some(state)) => {
            verify_output_state(conn, &state).await?;
            if !same_projection_lineage(observation, &state.latest.observation) {
                return Err(ProjectionStoreError::OutputCollision {
                    provider: message.provider.clone(),
                    message_id: message.message_id.clone(),
                });
            }

            if sequence < state.latest.sequence {
                false
            } else if observation.identity().generation()
                == state.latest.observation.identity().generation()
            {
                if !message_rows_compatible(&actual, message) {
                    return Err(ProjectionStoreError::OutputCollision {
                        provider: message.provider.clone(),
                        message_id: message.message_id.clone(),
                    });
                }
                false
            } else if !state.projector_owned || message_rows_compatible(&actual, message) {
                false
            } else {
                if has_other_projector_output_owner(conn, projection).await? {
                    return Err(ProjectionStoreError::OutputCollision {
                        provider: message.provider.clone(),
                        message_id: message.message_id.clone(),
                    });
                }
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
        (Some(actual), None) if message_rows_compatible(&actual, message) => false,
        (Some(_), None) | (None, Some(_)) => {
            return Err(ProjectionStoreError::OutputCollision {
                provider: message.provider.clone(),
                message_id: message.message_id.clone(),
            });
        }
        (None, None) => {
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

pub(super) async fn verify_provenance(
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
    sequence: u64,
    projection: &ClaudeSessionMessageProjection,
    message_created: bool,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let inserted = conn
        .execute(
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
    verify_provenance(conn, projection).await?;
    if inserted == 0 {
        return Ok(());
    }

    let sequence_i64 =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    conn.execute(
        "INSERT INTO temp.observation_projection_output_state (
            projector_version, output_provider, output_message_id,
            canonical_observation_id, latest_observation_id, latest_sequence,
            projector_owned, owner_count
         ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, 1)
         ON CONFLICT(projector_version, output_provider, output_message_id) DO UPDATE SET
            canonical_observation_id = CASE
                WHEN observation_projection_output_state.projector_owned = 1
                 AND excluded.latest_sequence >= observation_projection_output_state.latest_sequence
                THEN excluded.latest_observation_id
                ELSE observation_projection_output_state.canonical_observation_id
            END,
            latest_observation_id = CASE
                WHEN excluded.latest_sequence >= observation_projection_output_state.latest_sequence
                THEN excluded.latest_observation_id
                ELSE observation_projection_output_state.latest_observation_id
            END,
            latest_sequence = MAX(
                observation_projection_output_state.latest_sequence,
                excluded.latest_sequence
            ),
            projector_owned = MAX(
                observation_projection_output_state.projector_owned,
                excluded.projector_owned
            ),
            owner_count = observation_projection_output_state.owner_count + 1",
        params![
            provenance.projector_version(),
            message.provider.as_str(),
            message.message_id.as_str(),
            provenance.observation_id().as_str(),
            sequence_i64,
            i64::from(message_created),
        ],
    )
    .await
    .map_err(|error| storage("update projection output state", error))?;
    Ok(())
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

pub(super) async fn verify_effect(
    conn: &Connection,
    observation: &DurableClaudeObservationV1,
    effect: &ClaudeObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ClaudeObservationProjection::Message(projection) => {
            verify_provenance(conn, projection).await?;
            let state = read_output_state(conn, projection)
                .await?
                .ok_or(ProjectionStoreError::ProvenanceCollision)?;
            verify_output_state(conn, &state).await
        }
        ClaudeObservationProjection::Skipped(reason) => {
            verify_skip_disposition(conn, observation, *reason).await
        }
    }
}

pub(super) async fn apply_effect(
    conn: &Connection,
    sequence: u64,
    observation: &DurableClaudeObservationV1,
    effect: &ClaudeObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ClaudeObservationProjection::Message(projection) => {
            let message_created = apply_rows(conn, sequence, observation, projection).await?;
            apply_provenance(conn, sequence, projection, message_created).await
        }
        ClaudeObservationProjection::Skipped(reason) => {
            apply_skip_disposition(conn, observation, *reason).await
        }
    }
}
