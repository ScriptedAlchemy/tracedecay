use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1,
    CanonicalWorkflowEvidenceKindV1, CanonicalWorkflowSemanticKindV1, ObservationId,
    ObservationOrderingDomainV1, ProviderId, ProviderUsageContractDimensionV1,
    ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1, ProviderUsageModelV1,
    ProviderUsageScopeV1, SessionId,
};

use crate::{ObservationRecordParseErrorV1, parse_rfc3339_timestamp};

const PROVIDER: &str = "codex";

pub fn codex_observation_record_supported(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "compacted"
                | "inter_agent_communication"
        )
    )
}

pub fn normalize_codex_observation(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_codex_observation_inner(
        native,
        session_id,
        native_thread_id,
        stable_record_id,
        range,
        None,
    )
}

pub struct CodexObservationLocation<'a> {
    pub project_path: Option<&'a Path>,
    pub location_path: Option<&'a Path>,
    pub transcript_path: &'a Path,
    pub turn_id: Option<&'a str>,
    pub model: Option<&'a str>,
}

pub fn normalize_codex_observation_with_location(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    location: CodexObservationLocation<'_>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_codex_observation_inner(
        native,
        session_id,
        native_thread_id,
        stable_record_id,
        range,
        Some(location),
    )
}

fn normalize_codex_observation_inner(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    location: Option<CodexObservationLocation<'_>>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native_kind = native
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::NormalizationFailed)?;
    let payload = native.get("payload").unwrap_or(native);
    let timestamp = timestamp_from_record(native);
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(session_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    // `session_meta.payload.id` is the native thread id. The session parser may
    // fall back to a filename for legacy source identity, which is not enough
    // evidence to populate this relation.
    if let Some(thread_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_thread_id(thread_id);
    }
    if let Some(turn_id) = codex_native_turn_id(payload).or_else(|| {
        location
            .as_ref()
            .and_then(|location| location.turn_id)
            .and_then(observation_id_from_native)
    }) {
        relations = relations.with_turn_id(turn_id);
    }
    if matches!(
        native_kind,
        "event_msg" | "response_item" | "compacted" | "inter_agent_communication"
    ) {
        relations = relations.with_message_id(stable_record_id.clone());
    }
    if native_kind == "session_meta" {
        relations = append_codex_session_meta_agent_relations(relations, payload, native_thread_id);
    }

    let mut facts = Vec::new();
    let context_model = location.as_ref().and_then(|location| location.model);
    if let Some(location) = location {
        facts.push(CanonicalObservationFactV1::Session {
            project_path: location
                .project_path
                .map(|path| path.to_string_lossy().into_owned()),
            location_path: location
                .location_path
                .map(|path| path.to_string_lossy().into_owned()),
            transcript_path: Some(location.transcript_path.to_string_lossy().into_owned()),
            title: None,
            started_at: None,
            ended_at: None,
            source: Some("codex_rollout".to_string()),
            native_source: Some("codex".to_string()),
            profile: None,
            location_provenance: Some("rollout_context".to_string()),
        });
    }
    match native_kind {
        "session_meta" => {
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::SessionStart,
            });
            append_codex_git_facts(payload, &mut facts);
            if payload.pointer("/source/subagent").is_some()
                || payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
            {
                facts.push(CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                    reference: None,
                    content: None,
                });
            }
        }
        "turn_context" => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "turn_context".to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        "event_msg" => append_codex_event_facts(payload, timestamp, context_model, &mut facts),
        "response_item" => {
            append_codex_response_item_facts(payload, timestamp, &stable_record_id, &mut facts);
        }
        "compacted" => {
            facts.push(CanonicalObservationFactV1::Compaction {
                summary: payload.get("message").cloned(),
                input_tokens: canonical_u64(payload.get("input_tokens")),
                output_tokens: canonical_u64(payload.get("output_tokens")),
            });
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
            });
        }
        "inter_agent_communication" => {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                reference: None,
                content: payload
                    .get("message")
                    .or_else(|| payload.get("content"))
                    .cloned(),
            });
        }
        _ => {}
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn observation_id_from_native(value: &str) -> Option<ObservationId> {
    ObservationId::new(value).ok()
}

fn codex_native_turn_id(payload: &Value) -> Option<ObservationId> {
    string_field(payload, "turn_id")
        .or_else(|| {
            nested_string_field(
                payload,
                "/internal_chat_message_metadata_passthrough/turn_id",
            )
        })
        .and_then(|turn_id| observation_id_from_native(&turn_id))
}

fn append_codex_session_meta_agent_relations(
    mut relations: CanonicalObservationRelationsV1,
    payload: &Value,
    native_thread_id: Option<&str>,
) -> CanonicalObservationRelationsV1 {
    let parent_session_id = string_field(payload, "forked_from_id")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id"));
    let thread_source = string_field(payload, "thread_source");
    let is_subagent = thread_source.as_deref() == Some("subagent")
        || parent_session_id.is_some()
        || payload.pointer("/source/subagent").is_some();
    if !is_subagent {
        return relations;
    }
    // Codex does not expose a separate stable agent id in session_meta. The
    // subagent's native session/thread id is stable; nickname and role are
    // mutable descriptive labels and must never participate in identity.
    if let Some(agent_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_agent_id(agent_id);
    }
    if let Some(parent_agent_id) = parent_session_id.and_then(|id| observation_id_from_native(&id))
    {
        relations = relations.with_parent_agent_id(parent_agent_id);
    }
    relations
}

fn append_codex_event_facts(
    payload: &Value,
    timestamp: Option<i64>,
    context_model: Option<&str>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message" | "agent_message") => {
            let role = if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                CanonicalMessageRoleV1::User
            } else {
                CanonicalMessageRoleV1::Assistant
            };
            if let Some(content) = payload.get("message").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role,
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
            }
        }
        Some("token_count") => {
            let info = payload.get("info").unwrap_or(payload);
            let mut emitted = false;
            for (usage, semantics, field) in [
                (
                    info.get("last_token_usage"),
                    ProviderUsageCounterSemanticsV1::Delta,
                    "payload.info.last_token_usage",
                ),
                (
                    info.get("total_token_usage"),
                    ProviderUsageCounterSemanticsV1::Cumulative,
                    "payload.info.total_token_usage",
                ),
            ] {
                if let Some(usage) = usage {
                    append_codex_usage_fact(payload, usage, context_model, semantics, field, facts);
                    emitted = true;
                }
            }
            if !emitted {
                append_uncorrelated_codex_usage_fact(payload, info, context_model, facts);
            }
        }
        Some("thread_goal_updated") => {
            if !append_codex_thread_goal_lifecycle_fact(payload, facts) {
                facts.push(CanonicalObservationFactV1::Unknown {
                    native_kind: "thread_goal_updated".to_string(),
                    state: CanonicalUnknownStateV1::Malformed,
                });
            }
        }
        Some(event @ ("task_started" | "task_complete" | "turn_aborted")) => {
            append_codex_turn_lifecycle_fact(payload, event, facts);
        }
        Some(kind) => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        None => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "event_msg".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        }),
    }
}

fn append_uncorrelated_codex_usage_fact(
    payload: &Value,
    usage: &Value,
    context_model: Option<&str>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let input_tokens = canonical_u64(usage.get("input_tokens"));
    let output_tokens = canonical_u64(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens")),
    );
    let cache_read_tokens = canonical_u64(
        usage
            .get("cached_input_tokens")
            .or_else(|| usage.get("cache_read_input_tokens")),
    );
    let cache_write_tokens = canonical_u64(usage.get("cache_write_input_tokens"));
    let reasoning_tokens = canonical_u64(
        usage
            .get("reasoning_output_tokens")
            .or_else(|| usage.get("reasoning_tokens")),
    );
    let total_tokens = canonical_u64(usage.get("total_tokens"));
    let mut missing_dimensions = BTreeSet::from([
        ProviderUsageContractDimensionV1::Scope,
        ProviderUsageContractDimensionV1::CounterSemantics,
        ProviderUsageContractDimensionV1::Correlation,
    ]);
    if context_model
        .or_else(|| payload.get("model").and_then(Value::as_str))
        .is_none()
    {
        missing_dimensions.insert(ProviderUsageContractDimensionV1::Model);
    }
    facts.push(CanonicalObservationFactV1::UncorrelatedUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
        native_kind: "token_count".to_owned(),
        native_field: "payload.info".to_owned(),
        missing_dimensions,
    });
}

fn append_codex_usage_fact(
    payload: &Value,
    usage: &Value,
    context_model: Option<&str>,
    counter_semantics: ProviderUsageCounterSemanticsV1,
    native_field: &str,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let input_tokens = canonical_u64(usage.get("input_tokens"));
    let output_tokens = canonical_u64(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens")),
    );
    let cache_read_tokens = canonical_u64(
        usage
            .get("cached_input_tokens")
            .or_else(|| usage.get("cache_read_input_tokens")),
    );
    let cache_write_tokens = canonical_u64(usage.get("cache_write_input_tokens"));
    let reasoning_tokens = canonical_u64(
        usage
            .get("reasoning_output_tokens")
            .or_else(|| usage.get("reasoning_tokens")),
    );
    let total_tokens = canonical_u64(usage.get("total_tokens"));
    let counters = if [
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
    ]
    .into_iter()
    .any(|value| value.is_some())
    {
        ProviderUsageCountersV1::Known {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            total_tokens,
        }
    } else {
        ProviderUsageCountersV1::Unknown {
            reason: CanonicalUnknownStateV1::Malformed,
        }
    };
    facts.push(CanonicalObservationFactV1::ProviderUsage {
        model: context_model
            .or_else(|| payload.get("model").and_then(Value::as_str))
            .map_or(
                ProviderUsageModelV1::Unknown {
                    reason: CanonicalUnknownStateV1::Absent,
                },
                |model| ProviderUsageModelV1::Known {
                    model: model.to_owned(),
                },
            ),
        native_scope: match counter_semantics {
            ProviderUsageCounterSemanticsV1::Delta => ProviderUsageScopeV1::Request,
            ProviderUsageCounterSemanticsV1::Cumulative => ProviderUsageScopeV1::Session,
            ProviderUsageCounterSemanticsV1::Unknown
            | ProviderUsageCounterSemanticsV1::Unavailable => ProviderUsageScopeV1::Unknown,
        },
        counter_semantics,
        counters,
        request_id: string_field(payload, "request_id")
            .and_then(|request_id| observation_id_from_native(&request_id)),
        native_kind: "token_count".to_owned(),
        native_field: native_field.to_owned(),
    });
}

fn append_codex_thread_goal_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape (see goal_event_line / write_codex_rollout_with_goal_events):
    // payload.goal.{objective,status,threadId,tokensUsed,timeUsedSeconds,createdAt,updatedAt}
    // plus optional payload.threadId. Never invent fields absent from that nest.
    let Some(goal) = payload.get("goal") else {
        return false;
    };
    let Some(objective) = goal
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
    else {
        return false;
    };
    let _ = objective;
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(str::to_string);
    let provider_reference = goal
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| payload.get("threadId").and_then(Value::as_str))
        .filter(|thread_id| !thread_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(goal.clone()),
    });
    true
}

fn append_codex_turn_lifecycle_fact(
    payload: &Value,
    event: &str,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    // Exact singular task_complete / task_started / turn_aborted only
    // (write_codex_rollout_with_structured_events, task_events_become_turn_boundary_rows).
    // Do not index last_agent_message as content — classic turn rows exclude it.
    let provider_reference = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string);
    // Keep native `reason` in content only — do not promote it to status
    // (no fixture evidence that abort reason is a workflow status vocabulary).
    let mut content = serde_json::Map::new();
    content.insert("type".to_string(), Value::String(event.to_string()));
    for key in [
        "turn_id",
        "started_at",
        "completed_at",
        "duration_ms",
        "time_to_first_token_ms",
        "model_context_window",
        "reason",
    ] {
        if let Some(value) = payload.get(key) {
            content.insert(key.to_string(), value.clone());
        }
    }
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: Some(event.to_string()),
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(Value::Object(content)),
    });
}

fn append_codex_update_plan_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape: response_item function_call name=update_plan with
    // arguments JSON string/object {explanation?, plan:[{step,status}]} + call_id.
    let Some(arguments) = parse_arguments(payload.get("arguments")) else {
        return false;
    };
    if arguments.get("plan").and_then(Value::as_array).is_none() {
        return false;
    }
    let provider_reference = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(arguments),
    });
    true
}

fn append_codex_response_item_facts(
    payload: &Value,
    timestamp: Option<i64>,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(item_kind) = payload.get("type").and_then(Value::as_str) else {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "response_item".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
        return;
    };
    match item_kind {
        "message" => {
            if let Some(content) = payload.get("content").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role: canonical_message_role(payload.get("role").and_then(Value::as_str)),
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
                // Goal-context response_item messages stay Message-only.
                // WorkflowLifecycle Goal is reserved for nested thread_goal_updated
                // (write_codex_rollout_with_goal_events), not synthetic goal-context text.
            }
        }
        "function_call" | "custom_tool_call" | "tool_search_call" | "web_search_call" => {
            let invocation_id = canonical_native_observation_id(
                payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str),
                stable_record_id,
            );
            let name = response_item_tool_name(payload, item_kind)
                .unwrap_or_else(|| item_kind.to_string());
            facts.push(CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name: name.clone(),
                arguments: Value::Null,
            });
            if item_kind == "function_call" && name == "update_plan" {
                let _ = append_codex_update_plan_lifecycle_fact(payload, facts);
            }
        }
        "function_call_output" | "custom_tool_call_output" => {
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(canonical_native_observation_id(
                    payload.get("call_id").and_then(Value::as_str),
                    stable_record_id,
                )),
                content: Value::Null,
                success: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| matches!(status, "completed" | "success" | "succeeded")),
            });
        }
        "reasoning" => {
            let summary = payload.get("summary").filter(|summary| !summary.is_null());
            let encrypted = payload
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty());
            let (visibility, content) = if let Some(summary) = summary {
                (
                    CanonicalReasoningVisibilityV1::Visible,
                    Some(summary.clone()),
                )
            } else if encrypted {
                (CanonicalReasoningVisibilityV1::Redacted, None)
            } else {
                (CanonicalReasoningVisibilityV1::Unavailable, None)
            };
            facts.push(CanonicalObservationFactV1::Reasoning {
                visibility,
                content,
            });
        }
        kind => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
    }
}

fn append_codex_git_facts(payload: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    let Some(git) = payload.get("git") else {
        return;
    };
    if let Some(branch) = git
        .get("branch")
        .or_else(|| git.get("current_branch"))
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
            reference: Some(branch.to_string()),
            content: None,
        });
    }
    if let Some(commit) = git
        .get("commit_hash")
        .or_else(|| git.get("commit"))
        .or_else(|| git.get("head"))
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: Some(commit.to_string()),
            content: None,
        });
    }
}

pub fn codex_native_record_id(
    session_id: &str,
    value: &Value,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0codex\0");
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    ObservationId::new(format!(
        "codex.native.sha256:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn parse_arguments(arguments: Option<&Value>) -> Option<Value> {
    match arguments {
        Some(Value::String(raw)) => serde_json::from_str(raw).ok(),
        Some(value @ Value::Object(_)) => Some(value.clone()),
        _ => None,
    }
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nested_string_field(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn timestamp_from_record(record: &Value) -> Option<i64> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_timestamp)
}

fn canonical_message_role(role: Option<&str>) -> CanonicalMessageRoleV1 {
    match role {
        Some("user") => CanonicalMessageRoleV1::User,
        Some("assistant") => CanonicalMessageRoleV1::Assistant,
        Some("system" | "developer") => CanonicalMessageRoleV1::System,
        Some("tool") => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    }
}

fn canonical_native_observation_id(
    native_id: Option<&str>,
    fallback: &ObservationId,
) -> ObservationId {
    native_id
        .and_then(|native_id| ObservationId::new(native_id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn response_item_tool_name(payload: &Value, response_item_type: &str) -> Option<String> {
    payload
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| match response_item_type {
            "tool_search_call" => Some("tool_search".to_string()),
            "web_search_call" => Some("web_search".to_string()),
            _ => None,
        })
}

#[cfg(test)]
mod provider_usage_tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_json::json;
    use tracedecay_domain::{
        CanonicalObservationFactV1, ObservationId, ObservationSourceRangeV1,
        ProviderUsageContractDimensionV1, ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1,
        ProviderUsageModelV1, ProviderUsageScopeV1,
    };

    use super::{CodexObservationLocation, normalize_codex_observation_with_location};

    #[test]
    fn cumulative_token_count_preserves_native_semantics_and_turn_context() {
        let native = json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 10,
                        "output_tokens": 2,
                        "cached_input_tokens": 3,
                        "reasoning_output_tokens": 1,
                        "total_tokens": 12
                    },
                    "total_token_usage": {
                        "input_tokens": 100,
                        "output_tokens": 20,
                        "cached_input_tokens": 30,
                        "reasoning_output_tokens": 5,
                        "total_tokens": 120
                    }
                }
            }
        });
        let envelope = normalize_codex_observation_with_location(
            &native,
            "session.fixture",
            Some("thread.fixture"),
            ObservationId::new("record.fixture").unwrap(),
            ObservationSourceRangeV1::new(10, 20).unwrap(),
            CodexObservationLocation {
                project_path: None,
                location_path: None,
                transcript_path: Path::new("/tmp/rollout.jsonl"),
                turn_id: Some("turn.fixture"),
                model: Some("gpt-5.6-codex"),
            },
        )
        .unwrap();

        assert_eq!(
            envelope.relations().turn_id().map(|id| id.as_str()),
            Some("turn.fixture")
        );
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ProviderUsage {
                counter_semantics: ProviderUsageCounterSemanticsV1::Delta,
                native_scope: ProviderUsageScopeV1::Request,
                counters: ProviderUsageCountersV1::Known {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    ..
                },
                native_field,
                ..
            } if native_field == "payload.info.last_token_usage"
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ProviderUsage {
                model: ProviderUsageModelV1::Known { model },
                counter_semantics: ProviderUsageCounterSemanticsV1::Cumulative,
                native_scope: ProviderUsageScopeV1::Session,
                counters: ProviderUsageCountersV1::Known {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cache_read_tokens: Some(30),
                    reasoning_tokens: Some(5),
                    total_tokens: Some(120),
                    ..
                },
                native_field,
                ..
            } if model == "gpt-5.6-codex"
                && native_field == "payload.info.total_token_usage"
        )));
    }

    #[test]
    fn bare_token_count_info_remains_typed_uncorrelated() {
        let native = json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "total_tokens": 12
                }
            }
        });
        let envelope = normalize_codex_observation_with_location(
            &native,
            "session.fixture",
            Some("thread.fixture"),
            ObservationId::new("record.fixture").unwrap(),
            ObservationSourceRangeV1::new(10, 20).unwrap(),
            CodexObservationLocation {
                project_path: None,
                location_path: None,
                transcript_path: Path::new("/tmp/rollout.jsonl"),
                turn_id: Some("turn.fixture"),
                model: Some("gpt-5.6-codex"),
            },
        )
        .unwrap();

        assert!(
            envelope
                .facts()
                .iter()
                .all(|fact| !matches!(fact, CanonicalObservationFactV1::ProviderUsage { .. }))
        );
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::UncorrelatedUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                total_tokens: Some(12),
                native_kind,
                native_field,
                missing_dimensions,
                ..
            } if native_kind == "token_count"
                && native_field == "payload.info"
                && missing_dimensions == &BTreeSet::from([
                    ProviderUsageContractDimensionV1::Scope,
                    ProviderUsageContractDimensionV1::CounterSemantics,
                    ProviderUsageContractDimensionV1::Correlation,
                ])
        )));
    }
}
