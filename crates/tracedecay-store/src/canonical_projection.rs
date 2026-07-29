//! Pure deterministic reducers for canonical observation projections.

use serde_json::Value;
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationFactV1, CanonicalReasoningVisibilityV1, CanonicalWorkflowEvidenceKindV1,
    CanonicalWorkflowSemanticKindV1, DurableObservationV1, ObservationContractError,
    ObservationScopeV1,
};

use crate::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SessionMessageRecord, SessionRecord, WorkflowFactRecord,
};

fn cursor_model_string(value: &Value) -> Option<String> {
    [
        "model",
        "model_id",
        "modelId",
        "model_name",
        "modelName",
        "model_slug",
        "modelSlug",
        "model_display_name",
        "modelDisplayName",
        "display_model",
        "displayModel",
        "display_model_name",
        "displayModelName",
    ]
    .into_iter()
    .find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
    })
}

fn cursor_dispatch_model(item: &Value) -> Option<String> {
    item.get("input")
        .and_then(cursor_model_string)
        .or_else(|| cursor_model_string(item))
}

fn is_subagent_dispatch_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "task" | "subagent")
}

fn dispatch_text(item: &Value) -> Option<String> {
    let input = item.get("input").unwrap_or(item);
    let mut parts = Vec::new();
    for key in ["description", "prompt", "subagent_type"] {
        if let Some(value) = input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(value.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub fn derive_canonical_projection(
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

    if let Some(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        content: Some(content),
        ..
    }) = facts.iter().find(|fact| {
        matches!(
            fact,
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
                content: Some(_),
                ..
            }
        )
    }) {
        let text = content
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or(canonical_fact_text(content)?);
        return Ok(Some(CanonicalMessageFields {
            role: "system".to_owned(),
            text,
            kind: "goal".to_owned(),
            model: None,
            timestamp: envelope.evidence().native_timestamp(),
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

pub fn canonical_fact_text(value: &serde_json::Value) -> ProjectionStoreResult<String> {
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

pub fn workflow_semantic_kind(kind: CanonicalWorkflowSemanticKindV1) -> &'static str {
    match kind {
        CanonicalWorkflowSemanticKindV1::Goal => "goal",
        CanonicalWorkflowSemanticKindV1::Plan => "plan",
        CanonicalWorkflowSemanticKindV1::TodoList => "todo_list",
        CanonicalWorkflowSemanticKindV1::TodoItem => "todo_item",
        CanonicalWorkflowSemanticKindV1::Task => "task",
    }
}
