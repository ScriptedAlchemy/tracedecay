use serde_json::Value;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1,
    CanonicalWorkflowEvidenceKindV1, CanonicalWorkflowSemanticKindV1, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceRangeV1, ProviderId,
    ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1, ProviderUsageModelV1,
    ProviderUsageScopeV1, SessionId,
};

use crate::{ObservationRecordParseErrorV1, parse_rfc3339_timestamp};

const PROVIDER: &str = "claude";

pub fn stable_record_id(
    native: &Value,
    session_id: &str,
    offset: u64,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    let candidate = native
        .pointer("/message/id")
        .and_then(Value::as_str)
        .or_else(|| native.get("uuid").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), str::to_owned);
    provider_observation_id(&candidate).ok_or(ObservationRecordParseErrorV1::NormalizationFailed)
}

pub fn normalize(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let invalid = || ObservationRecordParseErrorV1::NormalizationFailed;
    let record_kind = native
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| is_canonical_label(kind))
        .unwrap_or("unknown");
    let timestamp = native
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_timestamp);
    let mut facts = Vec::new();

    append_session_location_fact(&mut facts, native);
    if matches!(record_kind, "user" | "assistant") {
        let message = native.get("message").unwrap_or(native);
        // Message facts carry only provider-authored visible text. Thinking,
        // redacted thinking, tool_use, and tool_result stay typed facts so they
        // never leak as ordinary searchable JSON (Cursor/Hermes parity).
        let authored_message = authored_claude_message_content(message)
            .or_else(|| tool_result_only_message_content(message))
            .map(|content| CanonicalObservationFactV1::Message {
                role: canonical_role(message, record_kind),
                content,
                model: message
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                timestamp,
            });
        append_message_facts(
            &mut facts,
            message,
            stable_record_id.as_str(),
            record_kind == "assistant" && !is_api_error_placeholder(native, message),
            authored_message,
        );
        append_assistant_attribution_fact(&mut facts, native, record_kind);
        append_tool_use_result_facts(&mut facts, native, record_kind);
        // Preserve Claude's native compact-summary flags as typed compaction
        // metadata without introducing a cross-provider trust contract.
        if record_kind == "user"
            && native.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            && native
                .get("isVisibleInTranscriptOnly")
                .and_then(Value::as_bool)
                == Some(true)
        {
            facts.push(CanonicalObservationFactV1::Compaction {
                summary: Some(Value::Object({
                    let mut marker = serde_json::Map::new();
                    marker.insert("isCompactSummary".to_owned(), Value::Bool(true));
                    marker.insert("isVisibleInTranscriptOnly".to_owned(), Value::Bool(true));
                    marker
                })),
                input_tokens: None,
                output_tokens: None,
            });
        }
    } else {
        append_non_message_fact(&mut facts, native, record_kind);
    }

    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: record_kind.to_owned(),
            state: CanonicalUnknownStateV1::Unsupported,
        });
    }

    let mut relations =
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?);
    if matches!(record_kind, "user" | "assistant") {
        relations = relations.with_message_id(stable_record_id.clone());
    }
    if let Some(parent) = optional_id(native, &["parentUuid", "parent_uuid", "logicalParentUuid"]) {
        relations = relations.with_parent_message_id(parent);
    }
    if let Some(agent) = optional_id(native, &["agentId", "agent_id"]) {
        relations = relations.with_agent_id(agent);
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER).map_err(|_| invalid())?,
        record_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

/// Claude writes API-error placeholders as assistant records carrying
/// `isApiErrorMessage: true`, model `"<synthetic>"`, and all-zero
/// `message.usage`. No provider request was billed for them, so their zero
/// counters must never become provider-usage rows — recording them attributes
/// zero-token usage to the non-model `"<synthetic>"` and pollutes billing.
/// Either native signal alone marks the placeholder (older records may carry
/// one without the other).
fn is_api_error_placeholder(native: &Value, message: &Value) -> bool {
    native.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        || message.get("model").and_then(Value::as_str) == Some("<synthetic>")
}

/// Provider-authored visible message content only: plain strings, or `text`
/// blocks in provider order. Skips thinking / `redacted_thinking` / `tool_use` /
/// `tool_result` (those become typed facts via [`append_message_facts`]).
fn authored_claude_message_content(message: &Value) -> Option<Value> {
    let content = message.get("content")?;
    match content {
        Value::String(text) if !text.trim().is_empty() => Some(Value::String(text.clone())),
        Value::Array(blocks) => {
            let parts = blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(Value::as_str) != Some("text") {
                        return None;
                    }
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| Value::String(parts.join("\n\n")))
        }
        _ => None,
    }
}

/// Tool-result-only user turns (edits, git ops) have no authored text blocks.
/// Keep a searchable Message fact so V1 projection still lands a row.
fn tool_result_only_message_content(message: &Value) -> Option<Value> {
    let blocks = message.get("content").and_then(Value::as_array)?;
    if blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("text"))
    {
        return None;
    }
    let parts = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|block| {
            let content = block.get("content")?;
            match content {
                Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
                other => {
                    let rendered = other.to_string();
                    (!rendered.trim().is_empty()).then_some(rendered)
                }
            }
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| Value::String(parts.join("\n\n")))
}

fn append_session_location_fact(facts: &mut Vec<CanonicalObservationFactV1>, native: &Value) {
    let Some(cwd) = native
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_owned)
    else {
        return;
    };
    facts.push(CanonicalObservationFactV1::Session {
        project_path: Some(cwd.clone()),
        location_path: Some(cwd),
        transcript_path: None,
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("claude_transcript".to_owned()),
        native_source: None,
        profile: None,
        location_provenance: Some("transcript_record".to_owned()),
    });
}

fn append_assistant_attribution_fact(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    record_kind: &str,
) {
    if record_kind != "assistant" {
        return;
    }
    let mut attribution = serde_json::Map::new();
    for (source_key, dest_key) in [
        ("attributionMcpServer", "attribution_mcp_server"),
        ("attributionMcpTool", "attribution_mcp_tool"),
        ("attributionSkill", "attribution_skill"),
        ("promptSource", "prompt_source"),
        ("origin", "origin"),
    ] {
        if let Some(value) = native
            .get(source_key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            attribution.insert(dest_key.to_owned(), Value::String(value.to_owned()));
        }
    }
    if attribution.is_empty() {
        return;
    }
    facts.push(CanonicalObservationFactV1::Workflow {
        evidence_kind: CanonicalWorkflowEvidenceKindV1::Attribution,
        reference: attribution
            .get("attribution_mcp_tool")
            .and_then(Value::as_str)
            .map(str::to_owned),
        content: Some(Value::Object(attribution)),
    });
}

fn append_tool_use_result_facts(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    record_kind: &str,
) {
    if record_kind != "user" {
        return;
    }
    let Some(tool_use_result) = native
        .get("toolUseResult")
        .filter(|value| value.is_object())
        .cloned()
    else {
        return;
    };
    let mut content = serde_json::Map::new();
    content.insert("type".to_owned(), Value::String("user".to_owned()));
    content.insert("toolUseResult".to_owned(), tool_use_result.clone());
    if let Some(branch) = native.get("gitBranch").cloned() {
        content.insert("gitBranch".to_owned(), branch);
    }
    if tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::FileEdit,
            reference: tool_use_result
                .get("filePath")
                .and_then(Value::as_str)
                .map(str::to_owned),
            content: Some(Value::Object(content.clone())),
        });
    }
    if tool_use_result
        .pointer("/gitOperation/commit")
        .and_then(Value::as_object)
        .is_some()
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: tool_use_result
                .pointer("/gitOperation/commit/sha")
                .and_then(Value::as_str)
                .map(str::to_owned),
            content: Some(Value::Object(content)),
        });
    }
}

fn append_message_facts(
    facts: &mut Vec<CanonicalObservationFactV1>,
    message: &Value,
    message_id: &str,
    provider_usage_allowed: bool,
    mut authored_message: Option<CanonicalObservationFactV1>,
) {
    if let Some(usage) = provider_usage_allowed
        .then(|| message.get("usage").filter(|value| value.is_object()))
        .flatten()
    {
        let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
        let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
        let cache_read_tokens = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
        let cache_write_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64);
        let reasoning_tokens = usage.get("reasoning_tokens").and_then(Value::as_u64);
        let total_tokens = usage.get("total_tokens").and_then(Value::as_u64);
        let optional_counters_are_valid = [
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "reasoning_tokens",
            "total_tokens",
        ]
        .into_iter()
        .all(|field| {
            usage
                .get(field)
                .is_none_or(|value| value.as_u64().is_some())
        });
        let counters =
            if input_tokens.is_some() && output_tokens.is_some() && optional_counters_are_valid {
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
            model: message.get("model").and_then(Value::as_str).map_or(
                ProviderUsageModelV1::Unknown {
                    reason: CanonicalUnknownStateV1::Absent,
                },
                |model| ProviderUsageModelV1::Known {
                    model: model.to_owned(),
                },
            ),
            native_scope: ProviderUsageScopeV1::Message,
            counter_semantics: ProviderUsageCounterSemanticsV1::Delta,
            counters,
            request_id: None,
            native_kind: "assistant".to_owned(),
            native_field: "message.usage".to_owned(),
        });
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        if let Some(authored_message) = authored_message {
            facts.push(authored_message);
        }
        return;
    };
    for (index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                // Visible text is aggregated into one Message for compatibility,
                // but its fact occupies the first native text-block position.
                if let Some(authored_message) = authored_message.take() {
                    facts.push(authored_message);
                }
            }
            Some("tool_use") => {
                let Some(name) = block
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| is_canonical_label(name))
                else {
                    continue;
                };
                let invocation_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(provider_observation_id)
                    .or_else(|| ObservationId::new(format!("{message_id}:tool:{index}")).ok());
                if let Some(invocation_id) = invocation_id {
                    facts.push(CanonicalObservationFactV1::ToolInvocation {
                        invocation_id,
                        name: name.to_owned(),
                        arguments: block.get("input").cloned().unwrap_or(Value::Null),
                    });
                }
                append_task_lifecycle_fact(name, block.get("input"), facts);
            }
            Some("tool_result") => facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .and_then(provider_observation_id),
                content: block.get("content").cloned().unwrap_or(Value::Null),
                success: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .map(|is_error| !is_error),
            }),
            Some("thinking") => {
                let content = block.get("thinking").cloned();
                if content.is_some() {
                    facts.push(CanonicalObservationFactV1::Reasoning {
                        visibility: CanonicalReasoningVisibilityV1::Visible,
                        content,
                    });
                }
            }
            Some("redacted_thinking") => facts.push(CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Redacted,
                content: None,
            }),
            _ => {}
        }
    }
    if let Some(authored_message) = authored_message {
        facts.push(authored_message);
    }
}

/// Claude Code tracks a session's work as a task list (`TaskCreate` /
/// `TaskUpdate` tool calls; per-session state mirrored under
/// `~/.claude/tasks/<session>/`). Each call is one lifecycle event on one task
/// item. The native input rides in `content` verbatim; ids and statuses are
/// never synthesized (`TaskCreate` carries no task id or status in its input —
/// the id only arrives later via `toolUseResult`).
fn append_task_lifecycle_fact(
    name: &str,
    input: Option<&Value>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let event = match name {
        "TaskCreate" => "TaskCreate",
        "TaskUpdate" => "TaskUpdate",
        _ => return,
    };
    let task_id = input
        .and_then(|input| input.get("taskId"))
        .and_then(Value::as_str)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_string);
    let status = input
        .and_then(|input| input.get("status"))
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
        provider_reference: task_id.clone(),
        item_id: task_id,
        parent_reference: None,
        list_reference: None,
        state: Some(event.to_string()),
        status,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: input.cloned(),
    });
}
fn append_non_message_fact(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    record_kind: &str,
) {
    match record_kind {
        "pr-link" => facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
            reference: native
                .get("prNumber")
                .or_else(|| native.get("url"))
                .and_then(value_label),
            content: Some(native.clone()),
        }),
        "system" if native.get("subtype").and_then(Value::as_str) == Some("compact_boundary") => {
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
            });
            facts.push(CanonicalObservationFactV1::Compaction {
                summary: native.get("compactMetadata").cloned(),
                input_tokens: native
                    .pointer("/compactMetadata/preTokens")
                    .and_then(Value::as_u64),
                output_tokens: None,
            });
        }
        "system"
            if native
                .get("subtype")
                .and_then(Value::as_str)
                .is_some_and(|subtype| subtype.contains("fallback")) =>
        {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::ModelFallback,
                reference: native
                    .get("fallbackModel")
                    .or_else(|| native.get("model"))
                    .and_then(value_label),
                content: Some(native.clone()),
            });
        }
        "system" if system_hook_has_signal(native) => {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Unknown,
                reference: native.get("subtype").and_then(value_label),
                content: Some(native.clone()),
            });
        }
        _ => {}
    }
}

fn system_hook_has_signal(native: &Value) -> bool {
    let hook_errors = native
        .get("hookErrors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty());
    let stop_reason = native
        .get("stopReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty());
    let prevented = native
        .get("preventedContinuation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    hook_errors || stop_reason || prevented
}

fn canonical_role(message: &Value, record_kind: &str) -> CanonicalMessageRoleV1 {
    match message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(record_kind)
    {
        "user" => CanonicalMessageRoleV1::User,
        "assistant" => CanonicalMessageRoleV1::Assistant,
        "system" => CanonicalMessageRoleV1::System,
        "tool" => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    }
}

fn optional_id(native: &Value, keys: &[&str]) -> Option<ObservationId> {
    keys.iter()
        .find_map(|key| native.get(*key).and_then(Value::as_str))
        .and_then(provider_observation_id)
}

fn provider_observation_id(value: &str) -> Option<ObservationId> {
    ObservationId::new(value).ok()
}

fn value_label(value: &Value) -> Option<String> {
    let value = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    is_canonical_label(&value).then_some(value)
}

fn is_canonical_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod provider_usage_tests {
    use serde_json::json;
    use tracedecay_domain::{
        CanonicalObservationFactV1, CanonicalUnknownStateV1, ObservationId,
        ObservationSourceRangeV1, ProviderUsageCountersV1,
    };

    use super::normalize;

    #[test]
    fn malformed_required_usage_counter_is_unknown_not_zero() {
        let native = json!({
            "type": "assistant",
            "message": {
                "id": "message.fixture",
                "model": "claude-sonnet-4",
                "role": "assistant",
                "content": [{"type": "text", "text": "done"}],
                "usage": {
                    "input_tokens": "100",
                    "output_tokens": 20
                }
            }
        });
        let envelope = normalize(
            &native,
            "session.fixture",
            ObservationId::new("message.fixture").unwrap(),
            ObservationSourceRangeV1::new(10, 20).unwrap(),
        )
        .unwrap();

        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ProviderUsage {
                counters: ProviderUsageCountersV1::Unknown {
                    reason: CanonicalUnknownStateV1::Malformed
                },
                ..
            }
        )));
    }

    #[test]
    fn user_record_usage_lookalike_is_not_provider_billing_evidence() {
        let native = json!({
            "type": "user",
            "message": {
                "id": "message.user-fixture",
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
                "model": "claude-sonnet-4",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20
                }
            }
        });
        let envelope = normalize(
            &native,
            "session.fixture",
            ObservationId::new("message.user-fixture").unwrap(),
            ObservationSourceRangeV1::new(20, 30).unwrap(),
        )
        .unwrap();

        assert!(
            !envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::ProviderUsage { .. }))
        );
    }
}
