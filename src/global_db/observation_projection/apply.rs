use std::collections::HashMap;

use libsql::{Connection, params};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationFactV1, CanonicalObservationIdV1, CanonicalReasoningVisibilityV1,
    CanonicalWorkflowEvidenceKindV1, CanonicalWorkflowSemanticKindV1, DurableObservationV1,
    ObservationContractError, ObservationScopeV1,
};
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SessionMessageProjection, WorkflowFactProjection, WorkflowFactRecord,
};

use crate::sessions::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};
use crate::sessions::cursor::{cursor_dispatch_model, dispatch_text, is_subagent_dispatch_tool};
use crate::sessions::{SessionMessageRecord, SessionRecord};

use super::state::{
    message_rows_compatible, read_message, read_output_state, read_session, reconcile_session_rows,
    storage, storage_message, verify_output_state,
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
    let native_record_matches = observation.identity().native_record_id().map_or_else(
        || envelope.provider().as_str() == "claude",
        |native_record_id| envelope.stable_record_id() == native_record_id,
    );
    if envelope.provider() != observation.source().provider()
        || !native_record_matches
        || envelope.evidence().ordering_domain() != observation.identity().ordering_domain()
        || envelope.evidence().range() != observation.identity().position()
    {
        return Err(ProjectionStoreError::Contract(
            ObservationContractError::InvalidCanonicalPayload,
        ));
    }

    let mut projected = canonical_message_fields(&envelope)?;
    let session_fields = canonical_session_fields(&envelope);
    let (primary_message_id, derived_messages) =
        canonical_compatibility_message_fields(&envelope, session_fields.as_ref(), &mut projected)?;
    let workflow_facts = canonical_workflow_facts(&envelope)?;
    if projected.is_none() && derived_messages.is_empty() && workflow_facts.is_empty() {
        return ObservationProjection::for_skip(
            observation,
            ProjectionSkipReason::NonConversationalRecord,
        );
    }
    let provider = envelope.provider().as_str().to_owned();
    let session_id = envelope.relations().session_id().as_str().to_owned();
    let (project_key, fallback_project_path) = match observation.scope() {
        ObservationScopeV1::Profile => ("user".to_owned(), "user".to_owned()),
        ObservationScopeV1::Project { project_id } => (
            project_id.as_str().to_owned(),
            project_id.as_str().to_owned(),
        ),
    };
    let timestamp = projected
        .as_ref()
        .and_then(|projected| projected.timestamp)
        .or_else(|| envelope.evidence().native_timestamp());
    let is_subagent = envelope.relations().parent_agent_id().is_some();
    let project_path = session_fields
        .as_ref()
        .and_then(|fields| fields.project_path.clone())
        .unwrap_or(fallback_project_path);
    let session_metadata_json =
        canonical_session_metadata(&provider, session_fields.as_ref(), envelope.facts())?;
    let session = SessionRecord {
        provider: provider.clone(),
        session_id: session_id.clone(),
        project_key,
        project_path,
        title: session_fields
            .as_ref()
            .and_then(|fields| fields.title.clone()),
        started_at: session_fields
            .as_ref()
            .and_then(|fields| fields.started_at)
            .or(timestamp),
        ended_at: session_fields
            .as_ref()
            .and_then(|fields| fields.ended_at)
            .or(timestamp),
        transcript_path: session_fields
            .as_ref()
            .and_then(|fields| fields.transcript_path.clone()),
        metadata_json: session_metadata_json.clone(),
        parent_session_id: envelope
            .relations()
            .parent_session_id()
            .map(|session_id| session_id.as_str().to_owned()),
        is_subagent: is_subagent || envelope.relations().parent_session_id().is_some(),
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
    let metadata_json = canonical_message_metadata(&envelope, session_metadata_json.as_deref())?;
    let base_message_id = primary_message_id.unwrap_or_else(|| {
        envelope
            .relations()
            .message_id()
            .unwrap_or_else(|| envelope.stable_record_id())
            .as_str()
            .to_owned()
    });
    let source_offset = i64::try_from(envelope.evidence().range().start()).ok();
    let mut messages =
        Vec::with_capacity(usize::from(projected.is_some()) + derived_messages.len());
    if let Some(projected) = projected {
        messages.push((
            session.clone(),
            canonical_session_message_record(
                &provider,
                &session_id,
                base_message_id.clone(),
                timestamp,
                ordinal,
                source_offset,
                &metadata_json,
                projected,
            ),
        ));
    }
    for derived in derived_messages {
        messages.push((
            session.clone(),
            canonical_session_message_record(
                &provider,
                &session_id,
                derived
                    .message_id
                    .unwrap_or_else(|| format!("{base_message_id}:{}", derived.suffix)),
                derived.fields.timestamp.or(timestamp),
                ordinal,
                source_offset,
                &metadata_json,
                derived.fields,
            ),
        ));
    }
    let workflow_facts: Vec<(SessionRecord, WorkflowFactRecord)> = workflow_facts
        .into_iter()
        .map(|fact| (session.clone(), fact))
        .collect();
    ObservationProjection::for_outputs(observation, messages, workflow_facts)
}

#[allow(clippy::too_many_arguments)]
fn canonical_session_message_record(
    provider: &str,
    session_id: &str,
    message_id: String,
    timestamp: Option<i64>,
    ordinal: i64,
    source_offset: Option<i64>,
    metadata_json: &str,
    fields: CanonicalMessageFields,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_owned(),
        message_id,
        session_id: session_id.to_owned(),
        role: fields.role,
        timestamp,
        ordinal,
        text: fields.text,
        kind: Some(fields.kind),
        model: fields.model,
        tool_names: fields.tool_names,
        source_path: None,
        source_offset,
        metadata_json: Some(metadata_json.to_owned()),
    }
}

struct CanonicalSessionFields {
    project_path: Option<String>,
    location_path: Option<String>,
    transcript_path: Option<String>,
    title: Option<String>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    source: Option<String>,
    native_source: Option<String>,
    profile: Option<String>,
    location_provenance: Option<String>,
}

fn canonical_session_fields(
    envelope: &CanonicalObservationEnvelopeV1,
) -> Option<CanonicalSessionFields> {
    envelope.facts().iter().find_map(|fact| match fact {
        CanonicalObservationFactV1::Session {
            project_path,
            location_path,
            transcript_path,
            title,
            started_at,
            ended_at,
            source,
            native_source,
            profile,
            location_provenance,
        } => Some(CanonicalSessionFields {
            project_path: project_path.clone(),
            location_path: location_path.clone(),
            transcript_path: transcript_path.clone(),
            title: title.clone(),
            started_at: *started_at,
            ended_at: *ended_at,
            source: source.clone(),
            native_source: native_source.clone(),
            profile: profile.clone(),
            location_provenance: location_provenance.clone(),
        }),
        _ => None,
    })
}

fn canonical_session_metadata(
    provider: &str,
    session: Option<&CanonicalSessionFields>,
    facts: &[CanonicalObservationFactV1],
) -> ProjectionStoreResult<Option<String>> {
    let mut metadata = serde_json::Map::new();
    if let Some(session) = session {
        if let Some(source) = &session.source {
            metadata.insert("source".to_owned(), source.clone().into());
        }
        if let Some(profile) = &session.profile {
            metadata.insert("profile".to_owned(), profile.clone().into());
        }
        if let Some(native_source) = &session.native_source {
            metadata.insert(format!("{provider}_source"), native_source.clone().into());
        }
        let location_namespace =
            if provider == "cursor" && session.source.as_deref() == Some("cursor_transcript") {
                "cursor_event".to_owned()
            } else {
                format!("{provider}_session")
            };
        if let Some(location_path) = session
            .location_path
            .as_ref()
            .or(session.project_path.as_ref())
        {
            metadata.insert(
                format!("{location_namespace}_cwd"),
                location_path.clone().into(),
            );
            metadata.insert(
                format!("{location_namespace}_worktree"),
                location_path.clone().into(),
            );
        }
        if let Some(provenance) = &session.location_provenance {
            metadata.insert(
                format!("{location_namespace}_location_provenance"),
                provenance.clone().into(),
            );
        }
    }
    if let Some(CanonicalObservationFactV1::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    }) = facts
        .iter()
        .find(|fact| matches!(fact, CanonicalObservationFactV1::Usage { .. }))
    {
        let mut usage = serde_json::Map::new();
        for (key, value) in [
            ("input_tokens", *input_tokens),
            ("output_tokens", *output_tokens),
            ("cache_read_input_tokens", *cache_read_tokens),
            ("cache_creation_input_tokens", *cache_write_tokens),
            ("reasoning_tokens", *reasoning_tokens),
        ] {
            if let Some(value) = value.filter(|value| *value != 0) {
                usage.insert(key.to_owned(), value.into());
            }
        }
        if !usage.is_empty() {
            metadata.insert("usage".to_owned(), usage.into());
        }
    }
    if metadata.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(&metadata).map(Some).map_err(|_| {
            ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
        })
    }
}

fn canonical_message_metadata(
    envelope: &CanonicalObservationEnvelopeV1,
    session_metadata_json: Option<&str>,
) -> ProjectionStoreResult<String> {
    let mut metadata = serde_json::to_value(envelope)
        .map_err(|_| ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding))?
        .as_object()
        .cloned()
        .ok_or_else(|| {
            ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
        })?;
    if let Some(session_metadata_json) = session_metadata_json {
        let session_metadata: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(session_metadata_json).map_err(|_| {
                ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
            })?;
        metadata.extend(session_metadata);
    }
    if metadata.get("source").and_then(serde_json::Value::as_str) == Some("cursor_transcript") {
        append_cursor_compatibility_metadata(&mut metadata, envelope.facts())?;
    }
    serde_json::to_string(&metadata)
        .map_err(|_| ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding))
}

fn append_cursor_compatibility_metadata(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    facts: &[CanonicalObservationFactV1],
) -> ProjectionStoreResult<()> {
    let mut tool_calls = Vec::new();
    let mut tool_events = Vec::new();
    let mut first_dispatch_id = None;
    for fact in facts {
        let CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name,
            arguments,
        } = fact
        else {
            continue;
        };
        tool_calls.push(serde_json::json!({
            "id": invocation_id.as_str(),
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments,
            },
        }));
        let input_bytes = serde_json::to_vec(arguments)
            .map_err(|_| {
                ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
            })?
            .len();
        tool_events.push(serde_json::json!({
            "type": "tool_use",
            "tool_name": name,
            "call_id": invocation_id.as_str(),
            "input_bytes": input_bytes,
        }));
        if first_dispatch_id.is_none() && is_subagent_dispatch_tool(name) {
            first_dispatch_id = Some(invocation_id.as_str());
        }
    }
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".to_owned(), tool_calls.into());
        metadata.insert("tool_events".to_owned(), tool_events.into());
    }
    if let Some(tool_use_id) = first_dispatch_id {
        metadata.insert("tool_use_id".to_owned(), tool_use_id.into());
    }
    Ok(())
}

fn canonical_workflow_facts(
    envelope: &CanonicalObservationEnvelopeV1,
) -> ProjectionStoreResult<Vec<WorkflowFactRecord>> {
    envelope
        .facts()
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| {
            let (
                semantic_kind,
                provider_reference,
                item_id,
                parent_reference,
                list_reference,
                state,
                status,
                item_order,
                revision,
                event_sequence,
                content,
            ) = match fact {
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind,
                    provider_reference,
                    item_id,
                    parent_reference,
                    list_reference,
                    state,
                    status,
                    item_order,
                    revision,
                    event_sequence,
                    content,
                } => (
                    *semantic_kind,
                    provider_reference.clone(),
                    item_id.clone(),
                    parent_reference.clone(),
                    list_reference.clone(),
                    state.clone(),
                    status.clone(),
                    *item_order,
                    revision
                        .clone()
                        .or_else(|| envelope.evidence().revision().map(str::to_owned)),
                    *event_sequence,
                    content.clone(),
                ),
                CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Plan,
                    reference,
                    content,
                } => (
                    CanonicalWorkflowSemanticKindV1::Plan,
                    reference.clone(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    envelope.evidence().revision().map(str::to_owned),
                    None,
                    content.clone(),
                ),
                CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Task,
                    reference,
                    content,
                } => (
                    CanonicalWorkflowSemanticKindV1::Task,
                    reference.clone(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    envelope.evidence().revision().map(str::to_owned),
                    None,
                    content.clone(),
                ),
                _ => return None,
            };
            Some((|| {
                let fact_ordinal = u32::try_from(index).map_err(|_| {
                    ProjectionStoreError::Contract(
                        ObservationContractError::InvalidCanonicalPayload,
                    )
                })?;
                let content_text = match (semantic_kind, content.as_ref()) {
                    (CanonicalWorkflowSemanticKindV1::Goal, Some(content)) => content
                        .get("objective")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or(canonical_fact_text(content)?),
                    (_, Some(content)) => canonical_fact_text(content)?,
                    (_, None) => String::new(),
                };
                Ok(WorkflowFactRecord {
                    fact_ordinal,
                    semantic_kind,
                    provider_reference,
                    item_id,
                    parent_reference,
                    list_reference,
                    state,
                    status,
                    item_order,
                    native_revision: revision,
                    event_sequence,
                    source_sequence: envelope.evidence().native_sequence(),
                    native_timestamp: envelope.evidence().native_timestamp(),
                    ordering_domain: envelope.evidence().ordering_domain().as_str().to_owned(),
                    content,
                    content_text,
                })
            })())
        })
        .collect()
}

struct CanonicalMessageFields {
    role: String,
    text: String,
    kind: String,
    model: Option<String>,
    timestamp: Option<i64>,
    tool_names: Option<String>,
}

struct CanonicalDerivedMessageFields {
    suffix: String,
    message_id: Option<String>,
    fields: CanonicalMessageFields,
}

fn canonical_compatibility_message_fields(
    envelope: &CanonicalObservationEnvelopeV1,
    session: Option<&CanonicalSessionFields>,
    primary: &mut Option<CanonicalMessageFields>,
) -> ProjectionStoreResult<(Option<String>, Vec<CanonicalDerivedMessageFields>)> {
    match session.and_then(|session| session.source.as_deref()) {
        Some("cursor_composer") => {
            canonical_composer_compatibility_message_fields(envelope).map(|derived| (None, derived))
        }
        Some("cursor_transcript") => {
            canonical_cursor_compatibility_message_fields(envelope, primary)
        }
        _ => Ok((None, Vec::new())),
    }
}

fn canonical_composer_compatibility_message_fields(
    envelope: &CanonicalObservationEnvelopeV1,
) -> ProjectionStoreResult<Vec<CanonicalDerivedMessageFields>> {
    let mut derived = Vec::new();
    let mut reasoning_index = 0usize;
    let mut tool_index = 0usize;
    let mut pull_request_index = 0usize;
    let has_tool_invocation = envelope
        .facts()
        .iter()
        .any(|fact| matches!(fact, CanonicalObservationFactV1::ToolInvocation { .. }));
    for fact in envelope.facts() {
        let (suffix, fields) = match fact {
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: Some(content),
            } => {
                let suffix = if reasoning_index == 0 {
                    "thinking".to_owned()
                } else {
                    format!("thinking:{reasoning_index}")
                };
                reasoning_index += 1;
                (
                    suffix,
                    CanonicalMessageFields {
                        role: "assistant".to_owned(),
                        text: canonical_fact_text(content)?,
                        kind: "reasoning".to_owned(),
                        model: None,
                        timestamp: None,
                        tool_names: None,
                    },
                )
            }
            CanonicalObservationFactV1::ToolInvocation {
                name, arguments, ..
            } => {
                let suffix = if tool_index == 0 {
                    "tool".to_owned()
                } else {
                    format!("tool:{tool_index}")
                };
                tool_index += 1;
                let normalized_name = name.to_ascii_lowercase();
                let kind = if ["edit", "write", "patch"]
                    .iter()
                    .any(|needle| normalized_name.contains(needle))
                {
                    "file_edit"
                } else {
                    "tool_call"
                };
                (
                    suffix,
                    CanonicalMessageFields {
                        role: "assistant".to_owned(),
                        text: canonical_fact_text(arguments)?,
                        kind: kind.to_owned(),
                        model: None,
                        timestamp: None,
                        tool_names: Some(name.clone()),
                    },
                )
            }
            CanonicalObservationFactV1::ToolResult { content, .. } if !has_tool_invocation => {
                let suffix = if tool_index == 0 {
                    "tool".to_owned()
                } else {
                    format!("tool:{tool_index}")
                };
                tool_index += 1;
                (
                    suffix,
                    CanonicalMessageFields {
                        role: "tool".to_owned(),
                        text: canonical_fact_text(content)?,
                        kind: "tool_result".to_owned(),
                        model: None,
                        timestamp: None,
                        tool_names: None,
                    },
                )
            }
            CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference,
                content,
            } => {
                let suffix = format!("pr:{pull_request_index}");
                pull_request_index += 1;
                let text = reference.clone().unwrap_or(
                    content
                        .as_ref()
                        .map(canonical_fact_text)
                        .transpose()?
                        .unwrap_or_default(),
                );
                (
                    suffix,
                    CanonicalMessageFields {
                        role: "system".to_owned(),
                        text,
                        kind: "pr_link".to_owned(),
                        model: None,
                        timestamp: None,
                        tool_names: None,
                    },
                )
            }
            _ => continue,
        };
        derived.push(CanonicalDerivedMessageFields {
            suffix,
            message_id: None,
            fields,
        });
    }
    Ok(derived)
}

fn canonical_cursor_compatibility_message_fields(
    envelope: &CanonicalObservationEnvelopeV1,
    primary: &mut Option<CanonicalMessageFields>,
) -> ProjectionStoreResult<(Option<String>, Vec<CanonicalDerivedMessageFields>)> {
    let dispatches = envelope
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name,
                arguments,
            } if is_subagent_dispatch_tool(name) => Some((invocation_id, name, arguments)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if dispatches.is_empty() {
        return Ok((None, Vec::new()));
    }

    let only_dispatches = envelope
        .facts()
        .iter()
        .find_map(|fact| match fact {
            CanonicalObservationFactV1::Message { content, .. } => {
                Some(content.as_array().is_some_and(|items| {
                    !items.is_empty()
                        && items.iter().all(|item| {
                            item.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                                && item
                                    .get("name")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(is_subagent_dispatch_tool)
                        })
                }))
            }
            _ => None,
        })
        .unwrap_or(true);
    let session_id = envelope.relations().session_id().as_str();
    let mut derived = Vec::new();
    let mut primary_message_id = None;
    for (index, (invocation_id, name, arguments)) in dispatches.into_iter().enumerate() {
        let fields = CanonicalMessageFields {
            role: "assistant".to_owned(),
            text: dispatch_text(arguments).map_or_else(|| canonical_fact_text(arguments), Ok)?,
            kind: "tool_dispatch".to_owned(),
            model: cursor_dispatch_model(arguments),
            timestamp: None,
            tool_names: Some(name.clone()),
        };
        let message_id = format!("{session_id}:tool_dispatch:{}", invocation_id.as_str());
        if only_dispatches && index == 0 {
            *primary = Some(fields);
            primary_message_id = Some(message_id);
        } else {
            derived.push(CanonicalDerivedMessageFields {
                suffix: format!("tool_dispatch:{index}"),
                message_id: Some(message_id),
                fields,
            });
        }
    }
    Ok((primary_message_id, derived))
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
        if matches!(
            fact,
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Plan
                    | CanonicalWorkflowEvidenceKindV1::Task,
                ..
            }
        ) {
            continue;
        }
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
            CanonicalObservationFactV1::Session { .. }
            | CanonicalObservationFactV1::Message { .. }
            | CanonicalObservationFactV1::WorkflowLifecycle { .. }
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

pub(super) fn workflow_semantic_kind(kind: CanonicalWorkflowSemanticKindV1) -> &'static str {
    match kind {
        CanonicalWorkflowSemanticKindV1::Goal => "goal",
        CanonicalWorkflowSemanticKindV1::Plan => "plan",
        CanonicalWorkflowSemanticKindV1::TodoList => "todo_list",
        CanonicalWorkflowSemanticKindV1::TodoItem => "todo_item",
        CanonicalWorkflowSemanticKindV1::Task => "task",
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
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<Option<ProjectionOutputAlias>> {
    let mut rows = if let Some(generation) = rebuild_generation {
        conn.query(
            "SELECT output_provider, output_message_id
             FROM observation_projection_rebuild_aliases
             WHERE projector_version = ?1 AND generation = ?2 AND observation_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                observation_id.as_str()
            ],
        )
        .await
    } else {
        conn.query(
            "SELECT output_provider, output_message_id
             FROM observation_projection_aliases
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id.as_str()],
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
    conn: &Connection,
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    derive_projection_with_alias_from_generation(conn, observation, None).await
}

pub(super) async fn derive_projection_for_rebuild(
    conn: &Connection,
    observation: &DurableObservationV1,
    generation: &str,
) -> ProjectionStoreResult<ObservationProjection> {
    derive_projection_with_alias_from_generation(conn, observation, Some(generation)).await
}

async fn derive_projection_with_alias_from_generation(
    conn: &Connection,
    observation: &DurableObservationV1,
    rebuild_generation: Option<&str>,
) -> ProjectionStoreResult<ObservationProjection> {
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
    conn: &Connection,
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
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provider,
                session_id,
                super::opt_text(provider_reference),
                before_observation_id,
            ],
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
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                provider,
                session_id,
                super::opt_text(provider_reference),
                before_observation_id,
            ],
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
    conn: &Connection,
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
    conn: &Connection,
    session: &SessionRecord,
) -> ProjectionStoreResult<()> {
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
                    super::opt_text(merged.title.as_deref()),
                    super::opt_i64(merged.started_at),
                    super::opt_i64(merged.ended_at),
                    super::opt_text(merged.transcript_path.as_deref()),
                    super::opt_text(merged.metadata_json.as_deref()),
                    super::opt_text(merged.parent_session_id.as_deref()),
                    i64::from(merged.is_subagent),
                    super::opt_text(merged.agent_id.as_deref()),
                    super::opt_text(merged.parent_tool_use_id.as_deref()),
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
            .map(|_| ())
            .map_err(|error| storage("insert projected session", error)),
    }
}

async fn apply_rows(
    conn: &Connection,
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
    let transition = message_transition(
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
        && !crate::sessions::lcm::raw::upsert_projected_raw_message(conn, &projected_message).await
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
    conn: &Connection,
    projection: &WorkflowFactProjection,
) -> ProjectionStoreResult<()> {
    let fact = projection.fact();
    let provenance = projection.provenance();
    let session = projection.session();
    let mut sequence_rows = conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = ?1",
            params![provenance.observation_id().as_str()],
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
            "SELECT receipt_id, observation_sequence, provider, session_id, semantic_kind,
                    provider_reference, item_id, parent_reference, list_reference, state, status,
                    item_order, native_revision, event_sequence, source_sequence,
                    native_timestamp, ordering_domain, content_json, content_text, output_digest
             FROM observation_workflow_facts
             WHERE projector_version = ?1 AND observation_id = ?2 AND fact_ordinal = ?3",
            params![
                provenance.projector_version(),
                provenance.observation_id().as_str(),
                i64::from(fact.fact_ordinal),
            ],
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
        receipt_id: cell!(0, String),
        observation_sequence: cell!(1, i64),
        provider: cell!(2, String),
        session_id: cell!(3, String),
        semantic_kind: cell!(4, String),
        provider_reference: cell!(5, Option<String>),
        item_id: cell!(6, Option<String>),
        parent_reference: cell!(7, Option<String>),
        list_reference: cell!(8, Option<String>),
        state: cell!(9, Option<String>),
        status: cell!(10, Option<String>),
        item_order: cell!(11, Option<i64>),
        native_revision: cell!(12, Option<String>),
        event_sequence: cell!(13, Option<i64>),
        source_sequence: cell!(14, Option<i64>),
        native_timestamp: cell!(15, Option<i64>),
        ordering_domain: cell!(16, String),
        content_json: cell!(17, Option<String>),
        content_text: cell!(18, String),
        output_digest: cell!(19, String),
    };
    let expected = StoredWorkflowFact {
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
        output_digest: projection.output_digest().as_str().to_owned(),
    };
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn apply_workflow_fact(
    conn: &Connection,
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
    conn: &Connection,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT receipt_id, output_provider, output_message_id, output_digest
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
            (projector_version, observation_id, output_ordinal, receipt_id, output_provider,
             output_message_id, output_digest, message_created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT DO NOTHING",
            params![
                provenance.projector_version(),
                provenance.observation_id().as_str(),
                projection.output_ordinal(),
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
    conn: &Connection,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    verify_provenance(conn, projection).await?;
    let state = read_output_state(conn, projection)
        .await?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    verify_output_state(conn, &state, projection).await
}

async fn apply_message_effect(
    conn: &Connection,
    sequence: u64,
    observation: &DurableObservationV1,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let message_created = apply_rows(conn, sequence, observation, projection).await?;
    apply_provenance(conn, sequence, projection, message_created).await
}

pub(crate) async fn verify_effect(
    conn: &Connection,
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

pub(crate) async fn verify_workflow_effects(
    conn: &Connection,
    workflow_facts: &[WorkflowFactProjection],
) -> ProjectionStoreResult<()> {
    for projection in workflow_facts {
        verify_workflow_fact(conn, projection).await?;
    }
    Ok(())
}

pub(super) async fn apply_effect(
    conn: &Connection,
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
    conn: &Connection,
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
        if !message_rows_compatible(&actual, message)
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
        if !message_rows_compatible(&actual, message) {
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
    conn: &Connection,
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
    fn canonical_session_fact_projects_typed_metadata_without_becoming_a_message() {
        let session_fact = CanonicalObservationFactV1::Session {
            project_path: Some("/workspace/project".to_owned()),
            location_path: Some("/workspace/project/.worktrees/feature".to_owned()),
            transcript_path: Some("/transcripts/session.jsonl".to_owned()),
            title: Some("Session title".to_owned()),
            started_at: Some(10),
            ended_at: Some(20),
            source: Some("provider_store".to_owned()),
            native_source: Some("tui".to_owned()),
            profile: Some("default".to_owned()),
            location_provenance: Some("profile_pin".to_owned()),
        };
        assert!(
            canonical_message_fields(&envelope(vec![session_fact.clone()]))
                .unwrap()
                .is_none()
        );
        let envelope = envelope(vec![
            session_fact,
            CanonicalObservationFactV1::Usage {
                input_tokens: Some(12),
                output_tokens: Some(3),
                cache_read_tokens: Some(7),
                cache_write_tokens: Some(0),
                reasoning_tokens: None,
            },
        ]);

        let fields = canonical_session_fields(&envelope).unwrap();
        assert_eq!(fields.project_path.as_deref(), Some("/workspace/project"));
        assert_eq!(
            fields.location_path.as_deref(),
            Some("/workspace/project/.worktrees/feature")
        );
        assert_eq!(
            fields.transcript_path.as_deref(),
            Some("/transcripts/session.jsonl")
        );
        let session_metadata =
            canonical_session_metadata("codex", Some(&fields), envelope.facts()).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(session_metadata.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["source"], "provider_store");
        assert_eq!(
            metadata["codex_session_cwd"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(
            metadata["codex_session_worktree"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(metadata["codex_session_location_provenance"], "profile_pin");
        assert_eq!(metadata["usage"]["input_tokens"], 12);
        assert!(
            metadata["usage"]
                .get("cache_creation_input_tokens")
                .is_none()
        );

        let message_metadata: serde_json::Value = serde_json::from_str(
            &canonical_message_metadata(&envelope, session_metadata.as_deref()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            message_metadata["codex_session_cwd"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(
            message_metadata["codex_session_worktree"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(
            message_metadata["codex_session_location_provenance"],
            "profile_pin"
        );
        assert_eq!(message_metadata["stable_record_id"], "record.fixture");
    }

    #[test]
    fn cursor_transcript_metadata_uses_event_compatibility_namespace() {
        let fields = CanonicalSessionFields {
            project_path: Some("/workspace/project".to_owned()),
            location_path: Some("/workspace/project/.worktrees/feature".to_owned()),
            transcript_path: Some("/transcripts/session.jsonl".to_owned()),
            title: None,
            started_at: None,
            ended_at: None,
            source: Some("cursor_transcript".to_owned()),
            native_source: Some("cursor".to_owned()),
            profile: None,
            location_provenance: Some("hook_event".to_owned()),
        };
        let metadata: serde_json::Value = serde_json::from_str(
            canonical_session_metadata("cursor", Some(&fields), &[])
                .unwrap()
                .as_deref()
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            metadata["cursor_event_cwd"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(
            metadata["cursor_event_worktree"],
            "/workspace/project/.worktrees/feature"
        );
        assert_eq!(metadata["cursor_event_location_provenance"], "hook_event");
        assert!(metadata.get("cursor_session_cwd").is_none());
    }

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
