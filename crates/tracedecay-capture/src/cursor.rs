use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, CanonicalWorkflowEvidenceKindV1,
    ObservationId, ObservationOrderingDomainV1, ProviderId, SessionId,
};

use crate::{ObservationRecordParseErrorV1, parse_cursor_human_timestamp};

pub fn normalize_cursor_observation(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    agent_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_cursor_observation_with_message_id(
        native,
        session_id,
        stable_record_id.clone(),
        stable_record_id,
        range,
        agent_id,
        parent_agent_id,
    )
}

pub fn normalize_cursor_observation_with_message_id(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    projected_message_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    agent_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native_kind = native
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or("message");
    let message = native.get("message").filter(|message| message.is_object());
    let content = message
        .and_then(|message| message.get("content"))
        .or_else(|| native.get("content"))
        .or_else(|| native.get("message").filter(|message| !message.is_object()));
    let timestamp = record_timestamp(native)
        .or_else(|| {
            native
                .get("tracedecayDerivedTimestamp")
                .and_then(Value::as_i64)
        })
        .or_else(|| timestamp_tag_from_record(native));
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(session_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    )
    .with_message_id(projected_message_id);
    if let Some(thread_id) = cursor_native_thread_id(native) {
        relations = relations.with_thread_id(thread_id);
    }
    if let Some(agent_id) = agent_id.and_then(|id| ObservationId::new(id).ok()) {
        relations = relations.with_agent_id(agent_id);
    }
    if let Some(parent_session_id) = parent_agent_id {
        if let Ok(parent_agent_id) = ObservationId::new(parent_session_id) {
            relations = relations.with_parent_agent_id(parent_agent_id);
        }
        if let Ok(parent_session_id) = SessionId::new(parent_session_id) {
            relations = relations.with_parent_session_id(parent_session_id);
        }
    }
    let mut facts = vec![CanonicalObservationFactV1::Session {
        project_path: native
            .get("tracedecayProjectPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        location_path: native
            .get("tracedecayLocationPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        transcript_path: native
            .get("tracedecayTranscriptPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("cursor_transcript".to_string()),
        native_source: Some("cursor".to_string()),
        profile: None,
        location_provenance: native
            .get("tracedecayLocationProvenance")
            .and_then(Value::as_str)
            .map(str::to_string),
    }];

    if let Some(content) = content {
        if let Some(message_content) = canonical_cursor_message_content(content) {
            facts.push(CanonicalObservationFactV1::Message {
                role: canonical_message_role(native.get("role").and_then(Value::as_str)),
                content: message_content,
                model: cursor_record_message_model(native, message.unwrap_or(native)).or_else(
                    || {
                        native
                            .get("tracedecayModel")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    },
                ),
                timestamp,
            });
        }
        append_cursor_content_facts(content, &stable_record_id, &mut facts);
    }
    append_cursor_tool_call_facts(
        message
            .and_then(|message| message.get("tool_calls"))
            .or_else(|| native.get("tool_calls")),
        &stable_record_id,
        &mut facts,
    );
    append_cursor_usage_fact(native, message, &mut facts);
    append_cursor_git_facts(native, &mut facts);

    // Compaction facts require an exact fixture-backed Cursor JSONL `type`
    // allowlist. No such native kinds are checked in; do not substring-match
    // "compact" (that promotes protocol-echo lookalikes). Composer bubbles use
    // the distinct provider bool `isCompacted` instead.
    if facts.len() == 1 {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("cursor")
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn cursor_native_thread_id(native: &Value) -> Option<ObservationId> {
    native
        .get("tracedecayThreadId")
        .or_else(|| native.get("conversation_id"))
        .or_else(|| native.get("session_id"))
        .or_else(|| native.get("chat_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .and_then(|id| ObservationId::new(id).ok())
}

fn canonical_cursor_message_content(content: &Value) -> Option<Value> {
    match content {
        Value::String(text) if !text.trim().is_empty() => Some(Value::String(text.clone())),
        Value::Array(items) if !items.is_empty() => Some(Value::Array(items.clone())),
        Value::Object(map) if !map.is_empty() => Some(Value::Object(map.clone())),
        _ => None,
    }
}

fn append_cursor_content_facts(
    content: &Value,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let invocation_id = canonical_native_observation_id(
                    item.get("id").and_then(Value::as_str),
                    stable_record_id,
                );
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("tool")
                    .to_string();
                facts.push(CanonicalObservationFactV1::ToolInvocation {
                    invocation_id,
                    name: name.clone(),
                    arguments: item
                        .get("input")
                        .or_else(|| item.get("arguments"))
                        .cloned()
                        .unwrap_or(Value::Null),
                });
                if is_subagent_dispatch_tool(&name) {
                    facts.push(CanonicalObservationFactV1::Workflow {
                        evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                        reference: item.get("id").and_then(Value::as_str).map(str::to_string),
                        content: None,
                    });
                }
            }
            Some("tool_result") => {
                facts.push(CanonicalObservationFactV1::ToolResult {
                    invocation_id: item
                        .get("tool_use_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(|id| canonical_native_observation_id(Some(id), stable_record_id)),
                    content: item
                        .get("content")
                        .or_else(|| item.get("result"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    success: item
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .map(|error| !error),
                });
            }
            Some("thinking" | "reasoning") => {
                let content = item
                    .get("text")
                    .or_else(|| item.get("thinking"))
                    .filter(|content| !content.is_null())
                    .cloned();
                facts.push(CanonicalObservationFactV1::Reasoning {
                    visibility: if content.is_some() {
                        CanonicalReasoningVisibilityV1::Visible
                    } else {
                        CanonicalReasoningVisibilityV1::Unavailable
                    },
                    content,
                });
            }
            _ => {}
        }
    }
}

fn append_cursor_tool_call_facts(
    tool_calls: Option<&Value>,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(tool_calls) = tool_calls.and_then(Value::as_array) else {
        return;
    };
    for tool_call in tool_calls {
        let function = tool_call.get("function").unwrap_or(tool_call);
        let name = function
            .get("name")
            .or_else(|| tool_call.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("tool")
            .to_string();
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: canonical_native_observation_id(
                tool_call.get("id").and_then(Value::as_str),
                stable_record_id,
            ),
            name,
            arguments: function
                .get("arguments")
                .or_else(|| tool_call.get("arguments"))
                .cloned()
                .unwrap_or(Value::Null),
        });
    }
}

fn append_cursor_usage_fact(
    native: &Value,
    message: Option<&Value>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let usage = native
        .get("usage")
        .or_else(|| native.get("tokenCount"))
        .or_else(|| message.and_then(|message| message.get("usage")))
        .or_else(|| message.and_then(|message| message.get("tokenCount")));
    let Some(usage) = usage else {
        return;
    };
    let input_tokens = canonical_u64(
        usage
            .get("input_tokens")
            .or_else(|| usage.get("inputTokens")),
    );
    let output_tokens = canonical_u64(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("outputTokens")),
    );
    let cache_read_tokens = canonical_u64(
        usage
            .get("cache_read_tokens")
            .or_else(|| usage.get("cacheReadTokens")),
    );
    let cache_write_tokens = canonical_u64(
        usage
            .get("cache_write_tokens")
            .or_else(|| usage.get("cacheWriteTokens")),
    );
    let reasoning_tokens = canonical_u64(
        usage
            .get("reasoning_tokens")
            .or_else(|| usage.get("reasoningTokens")),
    );
    if input_tokens.is_some()
        || output_tokens.is_some()
        || cache_read_tokens.is_some()
        || cache_write_tokens.is_some()
        || reasoning_tokens.is_some()
    {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        });
    }
}

fn append_cursor_git_facts(native: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    if let Some(branch) = native
        .get("branch")
        .or_else(|| native.pointer("/git/branch"))
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
            reference: Some(branch.to_string()),
            content: None,
        });
    }
    if let Some(commit) = native
        .get("commit")
        .or_else(|| native.get("commit_hash"))
        .or_else(|| native.pointer("/git/commit"))
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: Some(commit.to_string()),
            content: None,
        });
    }
    if native
        .get("gitDiffs")
        .and_then(Value::as_array)
        .is_some_and(|diffs| !diffs.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Diff,
            reference: None,
            content: None,
        });
    }
    if let Some(pull_requests) = native.get("pullRequests").and_then(Value::as_array) {
        for pull_request in pull_requests {
            let reference = ["url", "htmlUrl", "html_url", "id"]
                .into_iter()
                .find_map(|key| pull_request.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference: reference.clone(),
                content: None,
            });
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::PullRequest,
                reference,
                content: None,
            });
        }
    }
}

pub fn observation_native_record_id(
    provider: &str,
    session_id: &str,
    value: &Value,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0");
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    ObservationId::new(format!(
        "{provider}.native.sha256:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

pub fn cursor_projected_message_id(
    native: &Value,
    session_id: &str,
    source_offset: u64,
    generation: u64,
    namespace_replacement: bool,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    let base = native
        .get("id")
        .or_else(|| native.pointer("/message/id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{source_offset}"), str::to_string);
    // Truncate/rename replacements reuse byte offsets across file generations.
    // Namespace only on replacement rescans so first-generation ids stay stable.
    let message_id = if namespace_replacement {
        format!("{base}:generation:{generation}")
    } else {
        base
    };
    ObservationId::new(message_id).map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

const CURSOR_MODEL_KEYS: &[&str] = &[
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
];

fn cursor_model_string(value: &Value) -> Option<String> {
    CURSOR_MODEL_KEYS.iter().copied().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
    })
}

fn is_subagent_dispatch_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "task" | "subagent")
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
        .and_then(|id| ObservationId::new(id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn cursor_record_message_model(record: &Value, message: &Value) -> Option<String> {
    cursor_model_string(record).or_else(|| cursor_model_string(message))
}

fn record_timestamp(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(|timestamp| {
            timestamp
                .as_i64()
                .or_else(|| timestamp.as_str().and_then(|value| value.parse().ok()))
        })
}

/// Extracts and parses the first `<timestamp>…</timestamp>` tag carried by a
/// transcript line's text content.
///
/// Cursor emits no structured per-message time, so this tag is the whole
/// signal; every reader of a Cursor transcript has to agree on where it is
/// looked for (top-level, `message`, or a content array element) or two lanes
/// will date the same message differently.
pub fn timestamp_tag_from_record(record: &Value) -> Option<i64> {
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    match content {
        Value::String(text) => timestamp_tag_from_text(text),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .find_map(timestamp_tag_from_text),
        _ => None,
    }
}

fn timestamp_tag_from_text(text: &str) -> Option<i64> {
    let start = text.find("<timestamp>")? + "<timestamp>".len();
    let end = start + text[start..].find("</timestamp>")?;
    parse_cursor_human_timestamp(text[start..end].trim())
}
