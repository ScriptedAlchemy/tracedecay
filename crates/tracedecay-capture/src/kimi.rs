use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceRangeV1, ProviderId,
    ProviderUsageContractDimensionV1, SessionId,
};

use crate::content::content_is_empty;
use crate::timestamp::timestamp_secs;
use crate::{ObservationRecordParseErrorV1, parse::canonical_u64_string};

const PROVIDER: &str = "kimi";
const COMPACTION_PREFIX: &str =
    "Previous context has been compacted. Here is the compaction output:";

pub fn native_record_id(
    session_id: &str,
    range: ObservationSourceRangeV1,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    ObservationId::new(format!("{session_id}:{}", range.start()))
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}

pub fn normalize_observation(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    // Kimi records order by file bytes, so the range length is the source
    // record's byte length. Failed normalizations are counted, never hidden.
    hotpath::gauge!("capture.kimi.record_bytes").inc(range.end() - range.start());
    let envelope = normalize_kimi_record(native, session_id, stable_record_id, range);
    if envelope.is_err() {
        hotpath::gauge!("capture.kimi.normalize_failures").inc(1u64);
    }
    envelope
}

/// One source-record canonicalization, not a per-item walk.
#[hotpath::measure(label = "capture.kimi.normalize")]
fn normalize_kimi_record(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let record = current_wire_record(native)?;
    let native = record.value;
    let role = record.role;
    let mut facts = Vec::new();
    let mut relations =
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?);

    match role {
        "user" | "assistant" | "system" | "tool" | "_system_prompt" => {
            let content = native
                .get("content")
                .or_else(|| {
                    matches!(
                        native.get("type").and_then(Value::as_str),
                        Some("text" | "think")
                    )
                    .then_some(native)
                })
                .filter(|value| !content_is_empty(value))
                .cloned();
            if let Some(content) = &content {
                let canonical_content = if native.get("content").is_some() {
                    content.clone()
                } else {
                    Value::Array(vec![content.clone()])
                };
                append_reasoning(&mut facts, &canonical_content);
                if let Some(message_content) = message_content(&canonical_content) {
                    facts.push(CanonicalObservationFactV1::Message {
                        role: canonical_role(role)?,
                        content: message_content.clone(),
                        model: native
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        timestamp: record.timestamp.and_then(timestamp_secs),
                    });
                    if content_text(&message_content)
                        .is_some_and(|text| text.starts_with(COMPACTION_PREFIX))
                    {
                        facts.push(CanonicalObservationFactV1::Compaction {
                            summary: Some(message_content),
                            input_tokens: None,
                            output_tokens: None,
                        });
                        facts.push(CanonicalObservationFactV1::Boundary {
                            boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
                        });
                    }
                }
            }
            append_tool_result(&mut facts, native)?;
            if facts.is_empty() {
                return Err(ObservationRecordParseErrorV1::Empty);
            }
            relations = relations.with_message_id(stable_record_id.clone());
        }
        "_tool_call" => append_current_tool_call(&mut facts, native)?,
        "_usage" => append_usage(&mut facts, native),
        "_checkpoint" => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: role.to_owned(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        native_kind => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_owned(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
    }

    let timestamp = record.timestamp.and_then(timestamp_secs);
    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER).map_err(|_| invalid())?,
        role.trim_start_matches('_'),
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

struct KimiRecord<'a> {
    role: &'a str,
    value: &'a Value,
    timestamp: Option<&'a Value>,
}

fn current_wire_record(native: &Value) -> Result<KimiRecord<'_>, ObservationRecordParseErrorV1> {
    match native.get("type").and_then(Value::as_str) {
        Some("context.append_message") => {
            let message = native
                .get("message")
                .filter(|message| message.is_object())
                .ok_or_else(invalid)?;
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            Ok(KimiRecord {
                role,
                value: message,
                timestamp: native.get("time"),
            })
        }
        Some("context.append_loop_event") => {
            let event = native
                .get("event")
                .filter(|event| event.is_object())
                .ok_or_else(invalid)?;
            match event.get("type").and_then(Value::as_str) {
                Some("content.part") => {
                    let part = event
                        .get("part")
                        .filter(|part| part.is_object())
                        .ok_or_else(invalid)?;
                    Ok(KimiRecord {
                        role: "assistant",
                        value: part,
                        timestamp: native.get("time"),
                    })
                }
                Some("tool.call") => Ok(KimiRecord {
                    role: "_tool_call",
                    value: event,
                    timestamp: native.get("time"),
                }),
                Some("tool.result") => Ok(KimiRecord {
                    role: "tool",
                    value: event,
                    timestamp: native.get("time"),
                }),
                Some("step.end") => Ok(KimiRecord {
                    role: "_usage",
                    value: event,
                    timestamp: native.get("time"),
                }),
                _ => Err(ObservationRecordParseErrorV1::Empty),
            }
        }
        Some(_) | None => Err(ObservationRecordParseErrorV1::Empty),
    }
}

fn append_current_tool_call(
    facts: &mut Vec<CanonicalObservationFactV1>,
    event: &Value,
) -> Result<(), ObservationRecordParseErrorV1> {
    let name = event
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(invalid)?;
    let invocation_id = event
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(ObservationId::new)
        .transpose()
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    facts.push(CanonicalObservationFactV1::ToolInvocation {
        invocation_id,
        name: name.to_owned(),
        arguments: event.get("args").cloned().unwrap_or(Value::Null),
    });
    Ok(())
}

fn append_reasoning(facts: &mut Vec<CanonicalObservationFactV1>, content: &Value) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("think") {
            continue;
        }
        facts.push(CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: item
                .get("think")
                .or_else(|| item.get("text"))
                .or_else(|| item.get("content"))
                .cloned(),
        });
    }
}

fn message_content(content: &Value) -> Option<Value> {
    let Value::Array(items) = content else {
        return (!content_is_empty(content)).then(|| content.clone());
    };
    let visible = items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) != Some("think"))
        .cloned()
        .collect::<Vec<_>>();
    (!visible.is_empty()).then_some(Value::Array(visible))
}

fn append_usage(facts: &mut Vec<CanonicalObservationFactV1>, native: &Value) {
    let usage = native
        .get("usage")
        .or_else(|| native.get("content"))
        .filter(|value| value.is_object())
        .unwrap_or(native);
    let input_tokens = usage_u64(usage, &["input_tokens", "prompt_tokens", "inputOther"]);
    let output_tokens = usage_u64(usage, &["output_tokens", "completion_tokens", "output"]);
    let cache_read_tokens = usage_u64(
        usage,
        &[
            "cache_read_input_tokens",
            "cached_input_tokens",
            "cache_read_tokens",
            "inputCacheRead",
        ],
    );
    let cache_write_tokens = usage_u64(
        usage,
        &[
            "cache_creation_input_tokens",
            "cache_write_input_tokens",
            "cache_write_tokens",
            "inputCacheCreation",
        ],
    );
    let reasoning_tokens = usage_u64(usage, &["reasoning_tokens", "reasoning_output_tokens"]);
    let total_tokens = usage_u64(usage, &["total_tokens"]);
    if [
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens,
    ]
    .iter()
    .all(Option::is_none)
    {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "_usage".to_owned(),
            state: CanonicalUnknownStateV1::Malformed,
        });
        return;
    }
    facts.push(CanonicalObservationFactV1::UncorrelatedUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
        native_kind: "_usage".to_owned(),
        native_field: if native.get("usage").is_some() {
            "usage"
        } else if native.get("content").is_some() {
            "content"
        } else {
            "record"
        }
        .to_owned(),
        missing_dimensions: BTreeSet::from([
            ProviderUsageContractDimensionV1::Model,
            ProviderUsageContractDimensionV1::Scope,
            ProviderUsageContractDimensionV1::CounterSemantics,
            ProviderUsageContractDimensionV1::Correlation,
        ]),
    });
}

fn usage_u64(usage: &Value, aliases: &[&str]) -> Option<u64> {
    aliases
        .iter()
        .find_map(|key| canonical_u64_string(usage.get(*key)))
}

fn append_tool_result(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
) -> Result<(), ObservationRecordParseErrorV1> {
    let current_wire = native.get("type").and_then(Value::as_str) == Some("tool.result");
    if !current_wire && native.get("role").and_then(Value::as_str) != Some("tool") {
        return Ok(());
    }
    let Some(content) = native
        .get("content")
        .or_else(|| native.get("result"))
        .filter(|content| !content_is_empty(content))
        .cloned()
    else {
        return Ok(());
    };
    let invocation_id = native
        .get("tool_call_id")
        .or_else(|| native.get("toolCallId"))
        .and_then(Value::as_str)
        .map(ObservationId::new)
        .transpose()
        .map_err(|_| invalid())?;
    facts.push(CanonicalObservationFactV1::ToolResult {
        invocation_id,
        content,
        success: None,
    });
    Ok(())
}

fn canonical_role(role: &str) -> Result<CanonicalMessageRoleV1, ObservationRecordParseErrorV1> {
    match role {
        "user" => Ok(CanonicalMessageRoleV1::User),
        "assistant" => Ok(CanonicalMessageRoleV1::Assistant),
        "system" | "_system_prompt" => Ok(CanonicalMessageRoleV1::System),
        "tool" => Ok(CanonicalMessageRoleV1::Tool),
        _ => Err(invalid()),
    }
}

fn content_text(content: &Value) -> Option<&str> {
    content.as_str().or_else(|| {
        content.as_array()?.iter().find_map(|item| {
            item.get("text")
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
        })
    })
}

const fn invalid() -> ObservationRecordParseErrorV1 {
    ObservationRecordParseErrorV1::InvalidCanonicalEnvelope
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use tracedecay_domain::{
        CanonicalBoundaryKindV1, CanonicalMessageRoleV1, CanonicalObservationFactV1,
        ObservationSourceRangeV1, ProviderUsageContractDimensionV1,
    };

    use super::{native_record_id, normalize_observation};

    fn normalize(native: serde_json::Value) -> tracedecay_domain::CanonicalObservationEnvelopeV1 {
        let range = ObservationSourceRangeV1::new(10, 20).unwrap();
        normalize_observation(
            &native,
            "session-current",
            native_record_id("session-current", range).unwrap(),
            range,
        )
        .unwrap()
    }

    #[test]
    fn current_wire_visible_and_reasoning_parts_keep_distinct_semantics() {
        let visible = normalize(json!({
            "type": "context.append_loop_event",
            "agentId": "main",
            "event": {
                "type": "content.part",
                "part": {"type": "text", "text": "public answer"}
            },
            "time": 1_789_228_156_434_u64
        }));
        assert!(visible.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content,
                timestamp: Some(1_789_228_156),
                ..
            } if content == &json!([{"type": "text", "text": "public answer"}])
        )));
        assert!(
            !visible
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Reasoning { .. }))
        );

        let reasoning = normalize(json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "content.part",
                "part": {"type": "think", "think": "visible reasoning"}
            },
            "time": 1_789_228_156_433_u64
        }));
        assert!(reasoning.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                content: Some(content),
                ..
            } if content == "visible reasoning"
        )));
        assert!(
            !reasoning
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }))
        );
    }

    #[test]
    fn current_wire_compaction_and_tool_events_remain_typed() {
        let compaction = normalize(json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "content.part",
                "part": {
                    "type": "text",
                    "text": "Previous context has been compacted. Here is the compaction output: summary"
                }
            },
            "time": 1_789_228_156_434_u64
        }));
        assert!(compaction.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Compaction {
                summary: Some(_),
                ..
            }
        )));
        assert!(compaction.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary
            }
        )));

        let call = normalize(json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.call",
                "name": "read",
                "args": {"path": "x"},
                "toolCallId": "call_1"
            },
            "time": 1_789_228_124_540_u64
        }));
        assert!(call.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation { name, arguments, .. }
                if name == "read" && arguments == &json!({"path": "x"})
        )));

        let result = normalize(json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolCallId": "call_1",
                "result": {"output": "done"}
            },
            "time": 1_789_228_124_654_u64
        }));
        assert!(result.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolResult { content, .. }
                if content == &json!({"output": "done"})
        )));
    }

    #[test]
    fn current_wire_message_and_usage_keep_supported_semantics() {
        let user = normalize(json!({
            "type": "context.append_message",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "request"}],
                "toolCalls": []
            },
            "time": 1_789_228_081_157_u64
        }));
        assert!(user.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::User,
                ..
            }
        )));

        let usage = normalize(json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "step.end",
                "usage": {
                    "inputOther": 11,
                    "output": 7,
                    "inputCacheRead": 3,
                    "inputCacheCreation": 2
                }
            },
            "time": 1_789_228_124_657_u64
        }));
        assert!(usage.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::UncorrelatedUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                cache_read_tokens: Some(3),
                cache_write_tokens: Some(2),
                native_kind,
                native_field,
                missing_dimensions,
                ..
            } if native_kind == "_usage"
                && native_field == "usage"
                && missing_dimensions == &BTreeSet::from([
                    ProviderUsageContractDimensionV1::Model,
                    ProviderUsageContractDimensionV1::Scope,
                    ProviderUsageContractDimensionV1::CounterSemantics,
                    ProviderUsageContractDimensionV1::Correlation,
                ])
        )));
    }
}
