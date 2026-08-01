use std::collections::HashMap;

use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationIdV1, CanonicalWorkflowSemanticKindV1,
    DurableObservationV1, ObservationContractError, ObservationScopeV1,
};
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SessionMessageProjection, SessionMessageRecord, SessionRecord, WorkflowFactProjection,
    WorkflowFactRecord, derive_canonical_projection, workflow_semantic_kind,
};

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_sessions::compatibility::{
    derived_text_for_index, derived_text_for_snippet, projected_content_hash,
};
use tracedecay_sessions::runtime::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};

use super::state::{
    canonicalize_session_project_paths, read_message, read_output_state, read_session,
    reconcile_session_rows, storage, storage_message, verify_output_state,
};
use super::transition::{
    MessageTransition, MessageTransitionState, WorkflowFactTarget, WorkflowFactTransition,
    message_transition, write_workflow_fact_transition,
};

pub(in super::super) fn derive_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    match observation.source().provider().as_str() {
        "claude"
            if serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                observation.payload().clone(),
            )
            .is_ok() =>
        {
            derive_canonical_projection(observation)
        }
        "claude" => derive_claude_projection(observation),
        _ => derive_canonical_projection(observation),
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
    let source_path = (observation.source().source_key() != observation.source().session_id())
        .then(|| observation.source().source_key().as_str());
    let context = ClaudeRecordContext {
        session_id,
        project_key,
        project_path,
        file_generation: observation.identity().generation().file_id(),
        offset: observation.identity().position().start(),
        session_cwd: None,
        source_path,
        raw_message_id: durable_message_id,
        raw_tool_event_ids: &durable_tool_event_ids,
        raw_hook_tool_use_id: None,
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
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<Option<ProjectionOutputAlias>> {
    let mut rows = if let Some(generation) = rebuild_generation {
        conn.query(
            "SELECT output_provider, output_message_id
             FROM observation_projection_rebuild_aliases
             WHERE projector_version = ?1 AND generation = ?2 AND observation_id = ?3",
            (
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                observation_id.as_str(),
            ),
        )
        .await
    } else {
        conn.query(
            "SELECT output_provider, output_message_id
             FROM observation_projection_aliases
             WHERE projector_version = ?1 AND observation_id = ?2",
            (SESSION_MESSAGE_PROJECTOR_VERSION, observation_id.as_str()),
        )
        .await
    }
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
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    derive_projection_with_alias_from_generation(conn, observation, None).await
}

pub(super) async fn derive_projection_for_rebuild(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    generation: &str,
) -> ProjectionStoreResult<ObservationProjection> {
    derive_projection_with_alias_from_generation(conn, observation, Some(generation)).await
}

async fn derive_projection_with_alias_from_generation(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<ObservationProjection> {
    // Live derivation is disposition-aware: once an observation has converged
    // to a durable output-collision skip, its deterministic output identity
    // belongs to a different observation, so re-derivation must return that
    // skip rather than the message it would otherwise produce. A rebuild
    // (Some(generation)) re-derives from scratch by construction and ignores
    // the live disposition.
    if rebuild_generation.is_none()
        && output_collision_disposed(conn, observation.observation_id().as_str()).await?
    {
        return Ok(ObservationProjection::Skipped(
            ProjectionSkipReason::OutputCollision,
        ));
    }
    let projection = derive_projection(observation)?;
    // Collapse Codex-style token/time goal ticks at projection time so every
    // raw observation stays durable while current goal state follows the
    // established (thread, objective, status) transition semantics.
    let projection =
        collapse_consecutive_goal_ticks(conn, observation, projection, rebuild_generation).await?;
    let Some(alias) =
        read_projection_alias(conn, observation.observation_id(), rebuild_generation).await?
    else {
        return Ok(projection);
    };
    let (projection, derived_messages, workflow_facts) = match projection {
        ObservationProjection::Message(projection) => (projection, Vec::new(), Vec::new()),
        ObservationProjection::Composite {
            message: Some(projection),
            derived_messages,
            workflow_facts,
        } => (projection, derived_messages, workflow_facts),
        ObservationProjection::Composite { message: None, .. }
        | ObservationProjection::Skipped(_) => {
            return Err(ProjectionStoreError::ProvenanceCollision);
        }
    };
    let mut session = projection.session().clone();
    let mut message = projection.message().clone();
    session.provider.clone_from(&alias.provider);
    message.provider = alias.provider;
    message.message_id = alias.message_id;
    let mut messages = vec![(session, message)];
    messages.extend(
        derived_messages
            .into_iter()
            .map(|projection| (projection.session().clone(), projection.message().clone())),
    );
    ObservationProjection::for_outputs(
        observation,
        messages,
        workflow_facts
            .into_iter()
            .map(|projection| (projection.session().clone(), projection.fact().clone()))
            .collect(),
    )
}

/// Objective used for goal-state dedupe: prefer native `/objective` (Codex),
/// else the already-extracted `content_text`.
fn goal_dedupe_objective(fact: &WorkflowFactRecord) -> String {
    fact.content
        .as_ref()
        .and_then(|content| content.get("objective"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .map_or_else(|| fact.content_text.clone(), str::to_owned)
}

fn goal_dedupe_key(fact: &WorkflowFactRecord) -> (String, Option<String>) {
    (goal_dedupe_objective(fact), fact.status.clone())
}

async fn read_latest_goal_dedupe_key(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    provider_reference: Option<&str>,
    before_observation_id: &str,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<Option<(String, Option<String>)>> {
    let mut rows = if let Some(generation) = rebuild_generation {
        conn.query(
            "SELECT status, content_json, content_text
             FROM observation_projection_rebuild_workflow_facts
             WHERE projector_version = ?1 AND generation = ?2
               AND semantic_kind = 'goal'
               AND provider = ?3
               AND session_id = ?4
               AND (
                    (?5 IS NULL AND provider_reference IS NULL)
                    OR provider_reference = ?5
               )
               AND observation_sequence < (
                    SELECT sequence FROM observations WHERE observation_id = ?6
               )
             ORDER BY observation_sequence DESC, fact_ordinal DESC
             LIMIT 1",
            (
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provider,
                session_id,
                provider_reference,
                before_observation_id,
            ),
        )
        .await
    } else {
        conn.query(
            "SELECT status, content_json, content_text
             FROM observation_workflow_facts
             WHERE projector_version = ?1
               AND semantic_kind = 'goal'
               AND provider = ?2
               AND session_id = ?3
               AND (
                    (?4 IS NULL AND provider_reference IS NULL)
                    OR provider_reference = ?4
               )
               AND observation_sequence < (
                    SELECT sequence FROM observations WHERE observation_id = ?5
               )
             ORDER BY observation_sequence DESC, fact_ordinal DESC
             LIMIT 1",
            (
                SESSION_MESSAGE_PROJECTOR_VERSION,
                provider,
                session_id,
                provider_reference,
                before_observation_id,
            ),
        )
        .await
    }
    .map_err(|error| storage("read latest projected goal state", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read latest projected goal state", error))?
    else {
        return Ok(None);
    };
    let status: Option<String> = row
        .get(0)
        .map_err(|error| storage("read latest projected goal state", error))?;
    let content_json: Option<String> = row
        .get(1)
        .map_err(|error| storage("read latest projected goal state", error))?;
    let content_text: String = row
        .get(2)
        .map_err(|error| storage("read latest projected goal state", error))?;
    let objective = content_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .as_ref()
        .and_then(|content| content.get("objective"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .map_or(content_text, str::to_owned);
    Ok(Some((objective, status)))
}

/// Drop Codex Goal rows that only advance tokens/time while retaining the raw
/// observation. Meaningful objective or status transitions still project.
async fn collapse_consecutive_goal_ticks(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    projection: ObservationProjection,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<ObservationProjection> {
    type GoalPartition = (String, String, Option<String>);
    type GoalDedupeKey = (String, Option<String>);

    let ObservationProjection::Composite {
        message,
        derived_messages,
        workflow_facts,
    } = projection
    else {
        return Ok(projection);
    };
    if workflow_facts.is_empty() {
        return Ok(ObservationProjection::Composite {
            message,
            derived_messages,
            workflow_facts,
        });
    }

    let mut retained = Vec::with_capacity(workflow_facts.len());
    let mut last_in_batch: HashMap<GoalPartition, GoalDedupeKey> = HashMap::new();

    for fact_projection in workflow_facts {
        let fact = fact_projection.fact();
        let session = fact_projection.session();
        if session.provider != "codex"
            || fact.semantic_kind != CanonicalWorkflowSemanticKindV1::Goal
        {
            retained.push(fact_projection);
            continue;
        }
        let partition = (
            session.provider.clone(),
            session.session_id.clone(),
            fact.provider_reference.clone(),
        );
        let key = goal_dedupe_key(fact);
        let previous = if let Some(previous) = last_in_batch.get(&partition) {
            Some(previous.clone())
        } else {
            read_latest_goal_dedupe_key(
                conn,
                &partition.0,
                &partition.1,
                partition.2.as_deref(),
                observation.observation_id().as_str(),
                rebuild_generation,
            )
            .await?
        };
        if previous.as_ref() == Some(&key) {
            continue;
        }
        last_in_batch.insert(partition, key);
        retained.push(fact_projection);
    }

    let message = if message.as_ref().is_some_and(|projection| {
        projection.message().kind.as_deref() == Some("goal")
            && !retained
                .iter()
                .any(|fact| fact.fact().semantic_kind == CanonicalWorkflowSemanticKindV1::Goal)
    }) {
        None
    } else {
        message
    };
    if retained.is_empty() {
        return match message {
            Some(message) => Ok(ObservationProjection::Message(message)),
            None => ObservationProjection::for_skip(
                observation,
                ProjectionSkipReason::NonConversationalRecord,
            ),
        };
    }
    Ok(ObservationProjection::Composite {
        message,
        derived_messages,
        workflow_facts: retained,
    })
}

pub(super) async fn apply_session(
    conn: &impl Executor,
    session: &SessionRecord,
) -> ProjectionStoreResult<()> {
    // Resolve symlinked project-path family roots to their canonical on-disk
    // form once, here at the ingest boundary, and persist that canonical form.
    // Downstream reconciliation, verify/audit, and rebuild then operate on
    // stored strings alone without touching the filesystem.
    let session = &canonicalize_session_project_paths(session);
    match read_session(conn, &session.provider, &session.session_id).await? {
        Some(actual) => {
            let Some(merged) = reconcile_session_rows(&actual, session) else {
                return Err(ProjectionStoreError::OutputCollision {
                    provider: session.provider.clone(),
                    message_id: format!("session:{}", session.session_id),
                });
            };
            if merged == actual {
                return Ok(());
            }
            conn.execute(
                "UPDATE sessions
                 SET project_key = ?3, project_path = ?4, title = ?5, started_at = ?6,
                     ended_at = ?7, transcript_path = ?8, metadata_json = ?9,
                     parent_session_id = ?10, is_subagent = ?11, agent_id = ?12,
                     parent_tool_use_id = ?13
                 WHERE provider = ?1 AND session_id = ?2",
                params![
                    merged.provider.as_str(),
                    merged.session_id.as_str(),
                    merged.project_key.as_str(),
                    merged.project_path.as_str(),
                    merged.title.as_deref(),
                    merged.started_at,
                    merged.ended_at,
                    merged.transcript_path.as_deref(),
                    merged.metadata_json.as_deref(),
                    merged.parent_session_id.as_deref(),
                    i64::from(merged.is_subagent),
                    merged.agent_id.as_deref(),
                    merged.parent_tool_use_id.as_deref(),
                ],
            )
            .await
            .map(|_| ())
            .map_err(|error| storage("enrich projected session", error))
        }
        None => conn
            .execute(
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
                    session.title.as_deref(),
                    session.started_at,
                    session.ended_at,
                    session.transcript_path.as_deref(),
                    session.metadata_json.as_deref(),
                    session.parent_session_id.as_deref(),
                    i64::from(session.is_subagent),
                    session.agent_id.as_deref(),
                    session.parent_tool_use_id.as_deref(),
                ],
            )
            .await
            .map(|_| ())
            .map_err(|error| storage("insert projected session", error)),
    }
}

async fn upsert_projected_raw_message(
    conn: &impl Executor,
    message: &SessionMessageRecord,
) -> bool {
    let content_hash = projected_content_hash(&message.text);
    let snippet = derived_text_for_snippet(&message.text);
    let index = derived_text_for_index(&message.text);
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, legacy_source, legacy_truncated, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'inline', NULL, ?9, ?10, 0, 0, ?11)
         ON CONFLICT(provider, message_id) DO UPDATE SET
            session_id = excluded.session_id,
            role = excluded.role,
            ordinal = excluded.ordinal,
            timestamp = excluded.timestamp,
            content = excluded.content,
            content_hash = excluded.content_hash,
            storage_kind = excluded.storage_kind,
            payload_ref = excluded.payload_ref,
            snippet_text = excluded.snippet_text,
            index_text = excluded.index_text,
            legacy_source = 0,
            legacy_truncated = 0,
            metadata_json = excluded.metadata_json",
        params![
            message.provider.as_str(),
            message.message_id.as_str(),
            message.session_id.as_str(),
            message.role.as_str(),
            message.ordinal,
            message.timestamp,
            message.text.as_str(),
            content_hash.as_str(),
            snippet.as_str(),
            index.as_str(),
            message.metadata_json.as_deref(),
        ],
    )
    .await
    .is_ok()
}

async fn apply_rows(
    conn: &impl Executor,
    sequence: u64,
    observation: &DurableObservationV1,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<bool> {
    let session = projection.session();
    apply_session(conn, session).await?;

    let message = projection.message();
    let existing = read_message(conn, &message.provider, &message.message_id).await?;
    let state = read_output_state(conn, projection).await?;
    if existing.is_some()
        && let Some(state) = state.as_ref()
    {
        verify_output_state(conn, state, projection).await?;
    }
    let transition_state = state.as_ref().map(|state| {
        MessageTransitionState::new(
            observation,
            &state.latest.observation,
            state.latest.sequence,
            state.projector_owned,
        )
    });
    let (transition, preserve_protected_payload) = message_transition(
        conn,
        sequence,
        projection,
        existing.as_ref(),
        transition_state,
    )
    .await?;
    match transition {
        MessageTransition::Insert => {
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
                    message.timestamp,
                    message.ordinal,
                    message.text.as_str(),
                    message.kind.as_deref(),
                    message.model.as_deref(),
                    message.tool_names.as_deref(),
                    message.source_path.as_deref(),
                    message.source_offset,
                    message.metadata_json.as_deref(),
                ],
            )
            .await
            .map_err(|error| storage("insert projected message", error))?;
        }
        MessageTransition::Supersede => {
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
                    message.timestamp,
                    message.ordinal,
                    message.text.as_str(),
                    message.kind.as_deref(),
                    message.model.as_deref(),
                    message.tool_names.as_deref(),
                    message.source_path.as_deref(),
                    message.source_offset,
                    message.metadata_json.as_deref(),
                ],
            )
            .await
            .map_err(|error| storage("supersede projected message", error))?;
        }
        MessageTransition::Retain => {}
    }
    let projected_message = read_message(conn, &message.provider, &message.message_id)
        .await?
        .ok_or_else(|| ProjectionStoreError::OutputCollision {
            provider: message.provider.clone(),
            message_id: message.message_id.clone(),
        })?;
    if projected_message.provider != "hermes"
        && !preserve_protected_payload
        && !upsert_projected_raw_message(conn, &projected_message).await
    {
        return Err(storage_message(
            "upsert projected LCM raw message",
            "database write failed",
        ));
    }
    Ok(transition == MessageTransition::Insert)
}

fn workflow_content_json(
    projection: &WorkflowFactProjection,
) -> ProjectionStoreResult<Option<String>> {
    projection
        .fact()
        .content
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding))
}

#[derive(PartialEq, Eq)]
struct StoredWorkflowFact {
    retrieval_anchor_id: String,
    receipt_id: String,
    observation_sequence: i64,
    provider: String,
    session_id: String,
    semantic_kind: String,
    provider_reference: Option<String>,
    item_id: Option<String>,
    parent_reference: Option<String>,
    list_reference: Option<String>,
    state: Option<String>,
    status: Option<String>,
    item_order: Option<i64>,
    native_revision: Option<String>,
    event_sequence: Option<i64>,
    source_sequence: Option<i64>,
    native_timestamp: Option<i64>,
    ordering_domain: String,
    content_json: Option<String>,
    content_text: String,
    output_digest: String,
}

async fn verify_workflow_fact(
    conn: &impl QueryExecutor,
    projection: &WorkflowFactProjection,
) -> ProjectionStoreResult<()> {
    let fact = projection.fact();
    let provenance = projection.provenance();
    let session = projection.session();
    let mut sequence_rows = conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = ?1",
            (provenance.observation_id().as_str(),),
        )
        .await
        .map_err(|error| storage("read workflow observation sequence", error))?;
    let sequence = sequence_rows
        .next()
        .await
        .map_err(|error| storage("read workflow observation sequence", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?
        .get::<i64>(0)
        .map_err(|error| storage("read workflow observation sequence", error))?;
    drop(sequence_rows);
    let mut rows = conn
        .query(
            "SELECT retrieval_anchor_id, receipt_id, observation_sequence, provider, session_id,
                    semantic_kind,
                    provider_reference, item_id, parent_reference, list_reference, state, status,
                    item_order, native_revision, event_sequence, source_sequence,
                    native_timestamp, ordering_domain, content_json, content_text, output_digest
             FROM observation_workflow_facts
             WHERE projector_version = ?1 AND observation_id = ?2 AND fact_ordinal = ?3",
            (
                provenance.projector_version(),
                provenance.observation_id().as_str(),
                i64::from(fact.fact_ordinal),
            ),
        )
        .await
        .map_err(|error| storage("verify projected workflow fact", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("verify projected workflow fact", error))?
    else {
        return Err(ProjectionStoreError::ProvenanceCollision);
    };
    macro_rules! cell {
        ($index:literal, $ty:ty) => {
            row.get::<$ty>($index)
                .map_err(|error| storage("verify projected workflow fact", error))?
        };
    }
    let actual = StoredWorkflowFact {
        retrieval_anchor_id: cell!(0, String),
        receipt_id: cell!(1, String),
        observation_sequence: cell!(2, i64),
        provider: cell!(3, String),
        session_id: cell!(4, String),
        semantic_kind: cell!(5, String),
        provider_reference: cell!(6, Option<String>),
        item_id: cell!(7, Option<String>),
        parent_reference: cell!(8, Option<String>),
        list_reference: cell!(9, Option<String>),
        state: cell!(10, Option<String>),
        status: cell!(11, Option<String>),
        item_order: cell!(12, Option<i64>),
        native_revision: cell!(13, Option<String>),
        event_sequence: cell!(14, Option<i64>),
        source_sequence: cell!(15, Option<i64>),
        native_timestamp: cell!(16, Option<i64>),
        ordering_domain: cell!(17, String),
        content_json: cell!(18, Option<String>),
        content_text: cell!(19, String),
        output_digest: cell!(20, String),
    };
    let expected = StoredWorkflowFact {
        retrieval_anchor_id: provenance.retrieval_anchor_id().as_str().to_owned(),
        receipt_id: provenance.receipt_id().to_owned(),
        observation_sequence: sequence,
        provider: session.provider.clone(),
        session_id: session.session_id.clone(),
        semantic_kind: workflow_semantic_kind(fact.semantic_kind).to_owned(),
        provider_reference: fact.provider_reference.clone(),
        item_id: fact.item_id.clone(),
        parent_reference: fact.parent_reference.clone(),
        list_reference: fact.list_reference.clone(),
        state: fact.state.clone(),
        status: fact.status.clone(),
        item_order: fact
            .item_order
            .map(i64::try_from)
            .transpose()
            .map_err(|_| ProjectionStoreError::SequenceOverflow(u64::MAX))?,
        native_revision: fact.native_revision.clone(),
        event_sequence: fact
            .event_sequence
            .map(i64::try_from)
            .transpose()
            .map_err(|_| ProjectionStoreError::SequenceOverflow(u64::MAX))?,
        source_sequence: fact
            .source_sequence
            .map(i64::try_from)
            .transpose()
            .map_err(|_| ProjectionStoreError::SequenceOverflow(u64::MAX))?,
        native_timestamp: fact.native_timestamp,
        ordering_domain: fact.ordering_domain.clone(),
        content_json: workflow_content_json(projection)?,
        content_text: fact.content_text.clone(),
        output_digest: projection.output_digest()?.as_str().to_owned(),
    };
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn apply_workflow_fact(
    conn: &impl Executor,
    sequence: u64,
    projection: &WorkflowFactProjection,
) -> ProjectionStoreResult<()> {
    apply_session(conn, projection.session()).await?;
    let transition = WorkflowFactTransition::new(sequence, projection)?;
    let content_json = workflow_content_json(transition.projection())?;
    write_workflow_fact_transition(
        conn,
        WorkflowFactTarget::Live,
        &transition,
        workflow_semantic_kind(transition.fact().semantic_kind),
        content_json.as_deref(),
    )
    .await?;
    verify_workflow_fact(conn, projection).await
}

pub(super) async fn verify_provenance(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT retrieval_anchor_id, receipt_id, output_provider, output_message_id,
                    output_digest
             FROM observation_projection_provenance
             WHERE projector_version = ?1 AND observation_id = ?2
               AND output_ordinal = ?3",
            params![
                provenance.projector_version(),
                provenance.observation_id().as_str(),
                projection.output_ordinal(),
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
        row.get::<String>(4)
            .map_err(|error| storage("verify projection provenance", error))?,
    );
    let expected = (
        provenance.retrieval_anchor_id().as_str().to_string(),
        provenance.receipt_id().to_string(),
        message.provider.clone(),
        message.message_id.clone(),
        projection.output_digest()?.as_str().to_string(),
    );
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn apply_provenance(
    conn: &impl Executor,
    sequence: u64,
    projection: &SessionMessageProjection,
    message_created: bool,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let inserted = conn
        .execute(
            "INSERT INTO observation_projection_provenance
            (projector_version, observation_id, output_ordinal, retrieval_anchor_id, receipt_id,
             output_provider, output_message_id, output_digest, message_created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT DO NOTHING",
            params![
                provenance.projector_version(),
                provenance.observation_id().as_str(),
                projection.output_ordinal(),
                provenance.retrieval_anchor_id().as_str(),
                provenance.receipt_id(),
                message.provider.as_str(),
                message.message_id.as_str(),
                projection.output_digest()?.as_str(),
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

/// True when this observation already carries a durable `output_collision`
/// disposition: it must project as a skip everywhere (drain, audit, replay)
/// because its output identity is owned by a different observation.
pub async fn output_collision_disposed(
    conn: &impl QueryExecutor,
    observation_id: &str,
) -> ProjectionStoreResult<bool> {
    let mut rows = conn
        .query(
            "SELECT reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            (SESSION_MESSAGE_PROJECTOR_VERSION, observation_id),
        )
        .await
        .map_err(|error| storage("read projection disposition", error))?;
    let reason = rows
        .next()
        .await
        .map_err(|error| storage("read projection disposition", error))?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|error| storage("read projection disposition", error))?;
    Ok(reason.as_deref() == Some(ProjectionSkipReason::OutputCollision.as_str()))
}

async fn verify_skip_disposition(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT receipt_id, reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
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
    conn: &impl Executor,
    observation: &DurableObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO observation_projection_dispositions
            (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT DO NOTHING",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            observation.observation_id().as_str(),
            observation.receipt().receipt().receipt_id().as_str(),
            reason.as_str(),
        ],
    )
    .await
    .map_err(|error| storage("insert projection disposition", error))?;
    verify_skip_disposition(conn, observation, reason).await
}

async fn verify_message_effect(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    verify_provenance(conn, projection).await?;
    let state = read_output_state(conn, projection)
        .await?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    verify_output_state(conn, &state, projection).await
}

async fn apply_message_effect(
    conn: &impl Executor,
    sequence: u64,
    observation: &DurableObservationV1,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let message_created = apply_rows(conn, sequence, observation, projection).await?;
    apply_provenance(conn, sequence, projection, message_created).await
}

pub async fn verify_effect(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ObservationProjection::Message(projection) => verify_message_effect(conn, projection).await,
        ObservationProjection::Composite {
            message,
            derived_messages,
            workflow_facts,
        } => {
            if let Some(message) = message {
                verify_message_effect(conn, message).await?;
            }
            for message in derived_messages {
                verify_message_effect(conn, message).await?;
            }
            verify_workflow_effects(conn, workflow_facts).await?;
            Ok(())
        }
        ObservationProjection::Skipped(reason) => {
            verify_skip_disposition(conn, observation, *reason).await
        }
    }
}

pub async fn verify_workflow_effects(
    conn: &impl QueryExecutor,
    workflow_facts: &[WorkflowFactProjection],
) -> ProjectionStoreResult<()> {
    for projection in workflow_facts {
        verify_workflow_fact(conn, projection).await?;
    }
    Ok(())
}

pub(super) async fn apply_effect(
    conn: &impl Executor,
    sequence: u64,
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ObservationProjection::Message(projection) => {
            apply_message_effect(conn, sequence, observation, projection).await
        }
        ObservationProjection::Composite {
            message,
            derived_messages,
            workflow_facts,
        } => {
            if let Some(message) = message {
                apply_message_effect(conn, sequence, observation, message).await?;
            }
            for message in derived_messages {
                apply_message_effect(conn, sequence, observation, message).await?;
            }
            for projection in workflow_facts {
                apply_workflow_fact(conn, sequence, projection).await?;
            }
            Ok(())
        }
        ObservationProjection::Skipped(reason) => {
            apply_skip_disposition(conn, observation, *reason).await
        }
    }
}

pub(super) async fn seed_predecessor_message_lineage(
    conn: &impl Executor,
    sequence: u64,
    observation: &DurableObservationV1,
    predecessor_version: &str,
) -> ProjectionStoreResult<()> {
    let effect = derive_projection_with_alias(conn, observation).await?;
    for projection in effect.messages() {
        let message = projection.message();
        let mut rows = conn
            .query(
                "SELECT 1 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?2
                   AND output_provider = ?3 AND output_message_id = ?4
                 LIMIT 1",
                params![
                    predecessor_version,
                    observation.observation_id().as_str(),
                    message.provider.as_str(),
                    message.message_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage("read predecessor projection lineage", error))?;
        let inherited = rows
            .next()
            .await
            .map_err(|error| storage("read predecessor projection lineage", error))?
            .is_some();
        drop(rows);
        if !inherited {
            continue;
        }
        let mut actual = read_message(conn, &message.provider, &message.message_id)
            .await?
            .ok_or_else(|| ProjectionStoreError::OutputCollision {
                provider: message.provider.clone(),
                message_id: message.message_id.clone(),
            })?;
        if &actual != message
            && upgrade_v1_claude_source_path(
                conn,
                observation,
                predecessor_version,
                &actual,
                message,
            )
            .await?
        {
            actual = read_message(conn, &message.provider, &message.message_id)
                .await?
                .ok_or_else(|| ProjectionStoreError::OutputCollision {
                    provider: message.provider.clone(),
                    message_id: message.message_id.clone(),
                })?;
        }
        if &actual != message {
            return Err(ProjectionStoreError::OutputCollision {
                provider: message.provider.clone(),
                message_id: message.message_id.clone(),
            });
        }
        apply_provenance(conn, sequence, projection, false).await?;
    }
    Ok(())
}

async fn upgrade_v1_claude_source_path(
    conn: &impl Executor,
    observation: &DurableObservationV1,
    predecessor_version: &str,
    actual: &SessionMessageRecord,
    expected: &SessionMessageRecord,
) -> ProjectionStoreResult<bool> {
    const PROTECTED_SOURCE_PREFIX: &str = "tracedecay-claude-observation-source-v1-sha256-";

    let Some(expected_source_path) = expected.source_path.as_deref() else {
        return Ok(false);
    };
    if predecessor_version != SESSION_MESSAGE_PROJECTOR_VERSION_V1
        || observation.source().provider().as_str() != "claude"
        || expected.provider != "claude"
        || expected_source_path != observation.source().source_key().as_str()
        || !expected_source_path.starts_with(PROTECTED_SOURCE_PREFIX)
    {
        return Ok(false);
    }
    let legacy_source_path = format!("claude:{}", expected.session_id);
    let mut legacy = expected.clone();
    legacy.source_path = Some(legacy_source_path.clone());
    if actual != &legacy {
        return Ok(false);
    }
    let updated = conn
        .execute(
            "UPDATE session_messages SET source_path = ?3
             WHERE provider = ?1 AND message_id = ?2 AND source_path = ?4",
            params![
                expected.provider.as_str(),
                expected.message_id.as_str(),
                expected_source_path,
                legacy_source_path
            ],
        )
        .await
        .map_err(|error| storage("upgrade legacy Claude projection source path", error))?;
    if updated != 1 {
        return Err(ProjectionStoreError::OutputCollision {
            provider: expected.provider.clone(),
            message_id: expected.message_id.clone(),
        });
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_session_enrichment_is_commutative_and_rejects_real_path_collisions() {
        let sparse = SessionRecord {
            provider: "cursor".to_owned(),
            session_id: "session.fixture".to_owned(),
            project_key: "project.fixture".to_owned(),
            project_path: "project.fixture".to_owned(),
            title: None,
            started_at: None,
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let rich = SessionRecord {
            project_path: "/workspace/project".to_owned(),
            title: Some("Composer session".to_owned()),
            started_at: Some(10),
            ended_at: Some(20),
            metadata_json: Some(r#"{"source":"cursor_composer"}"#.to_owned()),
            ..sparse.clone()
        };

        let forward = reconcile_session_rows(&sparse, &rich).unwrap();
        let reverse = reconcile_session_rows(&rich, &sparse).unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(forward.project_path, "/workspace/project");
        assert_eq!(forward.title.as_deref(), Some("Composer session"));

        let legacy_path_key = SessionRecord {
            project_key: "/workspace/project".to_owned(),
            project_path: "/workspace/project".to_owned(),
            ..sparse.clone()
        };
        let typed_project = SessionRecord {
            project_key: "project.typed".to_owned(),
            ..legacy_path_key.clone()
        };
        let enriched = reconcile_session_rows(&legacy_path_key, &typed_project).unwrap();
        assert_eq!(enriched.project_key, "project.typed");
        assert_eq!(
            enriched,
            reconcile_session_rows(&typed_project, &legacy_path_key).unwrap()
        );

        let runtime_owned = SessionRecord {
            title: Some("Runtime-owned session".to_owned()),
            metadata_json: Some(r#"{"source":"runtime_preflight"}"#.to_owned()),
            ..forward.clone()
        };
        let projected = SessionRecord {
            title: Some("Projected session".to_owned()),
            metadata_json: Some(r#"{"source":"provider_projection"}"#.to_owned()),
            ..forward.clone()
        };
        let preserved = reconcile_session_rows(&runtime_owned, &projected).unwrap();
        assert_eq!(preserved.title.as_deref(), Some("Runtime-owned session"));
        assert_eq!(
            preserved.metadata_json.as_deref(),
            Some(r#"{"source":"runtime_preflight"}"#)
        );

        let conflicting = SessionRecord {
            project_path: "/workspace/other".to_owned(),
            ..rich
        };
        assert!(reconcile_session_rows(&forward, &conflicting).is_none());
    }
}
