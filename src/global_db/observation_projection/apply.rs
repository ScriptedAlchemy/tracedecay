use libsql::{Connection, params};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationFactV1, CanonicalObservationIdV1, CanonicalReasoningVisibilityV1,
    CanonicalWorkflowEvidenceKindV1, DurableObservationV1, ObservationContractError,
    ObservationScopeV1,
};
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION_V1, SessionMessageProjection,
};

use crate::sessions::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};
use crate::sessions::{SessionMessageRecord, SessionRecord};

use super::state::{
    has_other_projector_output_owner, message_rows_compatible, read_message, read_output_state,
    read_session, same_projection_lineage, session_rows_compatible, storage, verify_output_state,
};

pub(in super::super) fn derive_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    match observation.source().provider().as_str() {
        "claude" => derive_claude_projection(observation),
        "codex" | "cursor" | "hermes" | "kiro" | "cline" | "roo-code" | "kilo" => {
            derive_canonical_projection(observation)
        }
        provider => Err(ProjectionStoreError::UnsupportedProvider(
            provider.to_string(),
        )),
    }
}

fn derive_canonical_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone()).map_err(|_| {
            ProjectionStoreError::Contract(ObservationContractError::InvalidCanonicalPayload)
        })?;
    envelope
        .validate()
        .map_err(ProjectionStoreError::Contract)?;
    if envelope.provider() != observation.source().provider()
        || envelope.stable_record_id()
            != observation.identity().native_record_id().ok_or_else(|| {
                ProjectionStoreError::Contract(ObservationContractError::InvalidCanonicalPayload)
            })?
        || envelope.evidence().ordering_domain() != observation.identity().ordering_domain()
        || envelope.evidence().range() != observation.identity().position()
    {
        return Err(ProjectionStoreError::Contract(
            ObservationContractError::InvalidCanonicalPayload,
        ));
    }

    let Some(projected) = canonical_message_fields(&envelope)? else {
        return ObservationProjection::for_skip(
            observation,
            ProjectionSkipReason::NonConversationalRecord,
        );
    };
    let provider = envelope.provider().as_str().to_owned();
    let session_id = envelope.relations().session_id().as_str().to_owned();
    let (project_key, project_path) = match observation.scope() {
        ObservationScopeV1::Profile => ("user".to_owned(), "user".to_owned()),
        ObservationScopeV1::Project { project_id } => (
            project_id.as_str().to_owned(),
            project_id.as_str().to_owned(),
        ),
    };
    let timestamp = projected
        .timestamp
        .or_else(|| envelope.evidence().native_timestamp());
    let is_subagent = envelope.relations().parent_agent_id().is_some();
    let session = SessionRecord {
        provider: provider.clone(),
        session_id: session_id.clone(),
        project_key,
        project_path,
        title: None,
        started_at: timestamp,
        ended_at: timestamp,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent,
        agent_id: envelope
            .relations()
            .agent_id()
            .map(|id| id.as_str().to_owned()),
        parent_tool_use_id: None,
    };
    let ordinal = envelope
        .evidence()
        .native_sequence()
        .unwrap_or_else(|| envelope.evidence().range().start());
    let ordinal = i64::try_from(ordinal).map_err(|_| {
        ProjectionStoreError::Contract(ObservationContractError::InvalidCanonicalPayload)
    })?;
    let metadata_json = serde_json::to_string(&envelope)
        .map_err(|_| ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding))?;
    let message = SessionMessageRecord {
        provider,
        message_id: envelope
            .relations()
            .message_id()
            .unwrap_or_else(|| envelope.stable_record_id())
            .as_str()
            .to_owned(),
        session_id,
        role: projected.role,
        timestamp,
        ordinal,
        text: projected.text,
        kind: Some(projected.kind),
        model: projected.model,
        tool_names: projected.tool_names,
        source_path: None,
        source_offset: i64::try_from(envelope.evidence().range().start()).ok(),
        metadata_json: Some(metadata_json),
    };
    ObservationProjection::for_message(observation, session, message)
}

struct CanonicalMessageFields {
    role: String,
    text: String,
    kind: String,
    model: Option<String>,
    timestamp: Option<i64>,
    tool_names: Option<String>,
}

fn canonical_message_fields(
    envelope: &CanonicalObservationEnvelopeV1,
) -> ProjectionStoreResult<Option<CanonicalMessageFields>> {
    let facts = envelope.facts();
    let tool_names = facts
        .iter()
        .filter_map(|fact| match fact {
            CanonicalObservationFactV1::ToolInvocation { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let tool_names = (!tool_names.is_empty()).then(|| tool_names.join(","));

    if let Some(CanonicalObservationFactV1::Message {
        role,
        content,
        model,
        timestamp,
    }) = facts
        .iter()
        .find(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }))
    {
        return Ok(Some(CanonicalMessageFields {
            role: canonical_role(*role).to_owned(),
            text: canonical_fact_text(content)?,
            kind: "message".to_owned(),
            model: model.clone(),
            timestamp: *timestamp,
            tool_names,
        }));
    }

    for fact in facts {
        let fields = match fact {
            CanonicalObservationFactV1::ToolInvocation {
                name, arguments, ..
            } => CanonicalMessageFields {
                role: "assistant".to_owned(),
                text: canonical_fact_text(arguments)?,
                kind: "tool_invocation".to_owned(),
                model: None,
                timestamp: None,
                tool_names: Some(name.clone()),
            },
            CanonicalObservationFactV1::ToolResult { content, .. } => CanonicalMessageFields {
                role: "tool".to_owned(),
                text: canonical_fact_text(content)?,
                kind: "tool_result".to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Compaction { summary, .. } => CanonicalMessageFields {
                role: "system".to_owned(),
                text: summary
                    .as_ref()
                    .map(canonical_fact_text)
                    .transpose()?
                    .unwrap_or_default(),
                kind: "compaction".to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Reasoning {
                visibility,
                content: Some(content),
            } => CanonicalMessageFields {
                role: "assistant".to_owned(),
                text: canonical_fact_text(content)?,
                kind: reasoning_kind(*visibility).to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Git {
                evidence_kind,
                content,
                ..
            } => CanonicalMessageFields {
                role: "system".to_owned(),
                text: content
                    .as_ref()
                    .map(canonical_fact_text)
                    .transpose()?
                    .unwrap_or_default(),
                kind: git_kind(*evidence_kind).to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Workflow {
                evidence_kind,
                content,
                ..
            } => CanonicalMessageFields {
                role: "system".to_owned(),
                text: content
                    .as_ref()
                    .map(canonical_fact_text)
                    .transpose()?
                    .unwrap_or_default(),
                kind: workflow_kind(*evidence_kind).to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Usage { .. } => CanonicalMessageFields {
                role: "system".to_owned(),
                text: String::new(),
                kind: "usage".to_owned(),
                model: None,
                timestamp: None,
                tool_names: None,
            },
            CanonicalObservationFactV1::Message { .. }
            | CanonicalObservationFactV1::Reasoning { content: None, .. }
            | CanonicalObservationFactV1::Boundary { .. }
            | CanonicalObservationFactV1::Unknown { .. } => continue,
        };
        return Ok(Some(fields));
    }
    Ok(None)
}

fn canonical_fact_text(value: &serde_json::Value) -> ProjectionStoreResult<String> {
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    for pointer in ["/text", "/content", "/message"] {
        if let Some(text) = value.pointer(pointer).and_then(serde_json::Value::as_str) {
            return Ok(text.to_owned());
        }
    }
    serde_json::to_string(value)
        .map_err(|_| ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding))
}

fn canonical_role(role: CanonicalMessageRoleV1) -> &'static str {
    match role {
        CanonicalMessageRoleV1::User => "user",
        CanonicalMessageRoleV1::Assistant => "assistant",
        CanonicalMessageRoleV1::System => "system",
        CanonicalMessageRoleV1::Tool => "tool",
        CanonicalMessageRoleV1::Unknown => "unknown",
    }
}

fn reasoning_kind(visibility: CanonicalReasoningVisibilityV1) -> &'static str {
    match visibility {
        CanonicalReasoningVisibilityV1::Visible => "reasoning_visible",
        CanonicalReasoningVisibilityV1::Redacted => "reasoning_redacted",
        CanonicalReasoningVisibilityV1::Unavailable => "reasoning_unavailable",
        CanonicalReasoningVisibilityV1::NotApplicable => "reasoning_not_applicable",
    }
}

fn git_kind(kind: CanonicalGitEvidenceKindV1) -> &'static str {
    match kind {
        CanonicalGitEvidenceKindV1::Diff => "git_diff",
        CanonicalGitEvidenceKindV1::FileEdit => "git_file_edit",
        CanonicalGitEvidenceKindV1::Commit => "git_commit",
        CanonicalGitEvidenceKindV1::Branch => "git_branch",
        CanonicalGitEvidenceKindV1::PullRequest => "git_pull_request",
        CanonicalGitEvidenceKindV1::Unknown => "git_unknown",
    }
}

fn workflow_kind(kind: CanonicalWorkflowEvidenceKindV1) -> &'static str {
    match kind {
        CanonicalWorkflowEvidenceKindV1::Plan => "workflow_plan",
        CanonicalWorkflowEvidenceKindV1::Task => "workflow_task",
        CanonicalWorkflowEvidenceKindV1::Subagent => "workflow_subagent",
        CanonicalWorkflowEvidenceKindV1::ModelFallback => "workflow_model_fallback",
        CanonicalWorkflowEvidenceKindV1::Attribution => "workflow_attribution",
        CanonicalWorkflowEvidenceKindV1::PullRequest => "workflow_pull_request",
        CanonicalWorkflowEvidenceKindV1::Unknown => "workflow_unknown",
    }
}

fn derive_claude_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
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
            ObservationProjection::for_message(observation, session, message)
        }
        ClaudeRecordDisposition::NonConversational => ObservationProjection::for_skip(
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    let projection = derive_projection(observation)?;
    let Some(alias) = read_projection_alias(conn, observation.observation_id()).await? else {
        return Ok(projection);
    };
    let ObservationProjection::Message(projection) = projection else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };
    let mut session = projection.session().clone();
    let mut message = projection.message().clone();
    session.provider.clone_from(&alias.provider);
    message.provider = alias.provider;
    message.message_id = alias.message_id;
    ObservationProjection::for_message(observation, session, message)
}

async fn apply_rows(
    conn: &Connection,
    sequence: u64,
    observation: &DurableObservationV1,
    projection: &SessionMessageProjection,
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
    projection: &SessionMessageProjection,
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
    projection: &SessionMessageProjection,
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
    observation: &DurableObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT receipt_id, reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    observation: &DurableObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO observation_projection_dispositions
            (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO NOTHING",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION_V1,
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
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ObservationProjection::Message(projection) => {
            verify_provenance(conn, projection).await?;
            let state = read_output_state(conn, projection)
                .await?
                .ok_or(ProjectionStoreError::ProvenanceCollision)?;
            verify_output_state(conn, &state).await
        }
        ObservationProjection::Skipped(reason) => {
            verify_skip_disposition(conn, observation, *reason).await
        }
    }
}

pub(super) async fn apply_effect(
    conn: &Connection,
    sequence: u64,
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ObservationProjection::Message(projection) => {
            let message_created = apply_rows(conn, sequence, observation, projection).await?;
            apply_provenance(conn, sequence, projection, message_created).await
        }
        ObservationProjection::Skipped(reason) => {
            apply_skip_disposition(conn, observation, *reason).await
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{
        CanonicalBoundaryKindV1, CanonicalObservationEvidenceV1, CanonicalObservationRelationsV1,
        ObservationId, ObservationOrderingDomainV1, ObservationSourceRangeV1, ProviderId,
        SessionId,
    };

    use super::*;

    fn envelope(facts: Vec<CanonicalObservationFactV1>) -> CanonicalObservationEnvelopeV1 {
        CanonicalObservationEnvelopeV1::new(
            ProviderId::new("codex").unwrap(),
            "fixture",
            ObservationId::new("record.fixture").unwrap(),
            CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
            facts,
            CanonicalObservationEvidenceV1::new(
                ObservationOrderingDomainV1::SnapshotOrder,
                ObservationSourceRangeV1::new(1, 2).unwrap(),
            ),
        )
        .unwrap()
    }

    #[test]
    fn canonical_projection_prefers_authored_message_over_supporting_facts() {
        let envelope = envelope(vec![
            CanonicalObservationFactV1::Usage {
                input_tokens: Some(10),
                output_tokens: Some(4),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
            CanonicalObservationFactV1::ToolInvocation {
                invocation_id: ObservationId::new("tool.fixture").unwrap(),
                name: "Read".to_owned(),
                arguments: json!({"path": "redacted"}),
            },
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": "safe"}),
                model: Some("model.fixture".to_owned()),
                timestamp: Some(42),
            },
        ]);

        let fields = canonical_message_fields(&envelope).unwrap().unwrap();
        assert_eq!(fields.role, "assistant");
        assert_eq!(fields.text, "safe");
        assert_eq!(fields.kind, "message");
        assert_eq!(fields.model.as_deref(), Some("model.fixture"));
        assert_eq!(fields.timestamp, Some(42));
        assert_eq!(fields.tool_names.as_deref(), Some("Read"));
    }

    #[test]
    fn canonical_projection_skips_boundary_only_records() {
        let envelope = envelope(vec![CanonicalObservationFactV1::Boundary {
            boundary_kind: CanonicalBoundaryKindV1::TurnEnd,
        }]);

        assert!(canonical_message_fields(&envelope).unwrap().is_none());
    }

    #[test]
    fn canonical_projection_kind_names_are_stable() {
        assert_eq!(
            reasoning_kind(CanonicalReasoningVisibilityV1::Visible),
            "reasoning_visible"
        );
        assert_eq!(
            git_kind(CanonicalGitEvidenceKindV1::PullRequest),
            "git_pull_request"
        );
        assert_eq!(
            workflow_kind(CanonicalWorkflowEvidenceKindV1::ModelFallback),
            "workflow_model_fallback"
        );
    }
}
