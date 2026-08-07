use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceRangeV1, PayloadReferenceV1, ProviderId,
    ProviderUsageContractDimensionV1, SessionId,
};

use crate::ObservationRecordParseErrorV1;

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
    let role = native
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let mut facts = Vec::new();
    let mut relations =
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?);

    match role {
        "user" | "assistant" | "system" | "tool" | "_system_prompt" => {
            let content = native
                .get("content")
                .filter(|value| !content_is_empty(value))
                .cloned();
            if let Some(content) = &content {
                append_reasoning(&mut facts, content);
                if let Some(message_content) = message_content(content) {
                    facts.push(CanonicalObservationFactV1::Message {
                        role: canonical_role(role)?,
                        content: message_content.clone(),
                        model: native
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        timestamp: timestamp_secs(native.get("timestamp")),
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
            append_tool_calls(&mut facts, native, &stable_record_id)?;
            append_tool_result(&mut facts, native)?;
            if facts.is_empty() {
                return Err(ObservationRecordParseErrorV1::Empty);
            }
            relations = relations.with_message_id(stable_record_id.clone());
        }
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

    let timestamp = timestamp_secs(native.get("timestamp"));
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
    let input_tokens = canonical_u64(
        usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens")),
    );
    let output_tokens = canonical_u64(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens")),
    );
    let cache_read_tokens = canonical_u64(
        usage
            .get("cache_read_input_tokens")
            .or_else(|| usage.get("cached_input_tokens"))
            .or_else(|| usage.get("cache_read_tokens")),
    );
    let cache_write_tokens = canonical_u64(
        usage
            .get("cache_creation_input_tokens")
            .or_else(|| usage.get("cache_write_input_tokens"))
            .or_else(|| usage.get("cache_write_tokens")),
    );
    let reasoning_tokens = canonical_u64(
        usage
            .get("reasoning_tokens")
            .or_else(|| usage.get("reasoning_output_tokens")),
    );
    let total_tokens = canonical_u64(usage.get("total_tokens"));
    if [
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
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

fn canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn append_tool_calls(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let Some(calls) = native.get("tool_calls").and_then(Value::as_array) else {
        return Ok(());
    };
    for call in calls {
        let function = match call.get("function") {
            Some(function @ Value::Object(_)) => function,
            _ => call,
        };
        let Some(name) = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let arguments = match function.get("arguments") {
            Some(Value::String(raw)) => match serde_json::from_str(raw) {
                Ok(value) => value,
                Err(_) => Value::String(raw.clone()),
            },
            Some(value) => value.clone(),
            None => Value::Null,
        };
        let invocation_id = if let Some(id) = call.get("id").and_then(Value::as_str) {
            ObservationId::new(id).map_err(|_| invalid())?
        } else {
            let evidence = serde_json::json!({
                "message_id": message_id.as_str(),
                "name": name,
                "arguments": arguments,
            });
            let digest = PayloadReferenceV1::for_payload(&evidence).map_err(|_| invalid())?;
            ObservationId::new(format!("kimi.tool.{}", digest.digest().as_str()))
                .map_err(|_| invalid())?
        };
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name: name.to_owned(),
            arguments,
        });
    }
    Ok(())
}

fn append_tool_result(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
) -> Result<(), ObservationRecordParseErrorV1> {
    if native.get("role").and_then(Value::as_str) != Some("tool") {
        return Ok(());
    }
    let Some(content) = native
        .get("content")
        .filter(|content| !content_is_empty(content))
        .cloned()
    else {
        return Ok(());
    };
    let invocation_id = native
        .get("tool_call_id")
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

fn content_is_empty(content: &Value) -> bool {
    match content {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn timestamp_secs(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .map(|timestamp| {
            if timestamp >= 1_000_000_000_000 {
                timestamp / 1000
            } else {
                timestamp
            }
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
        CanonicalBoundaryKindV1, CanonicalObservationFactV1, ObservationSourceRangeV1,
        ProviderUsageContractDimensionV1,
    };

    use super::{native_record_id, normalize_observation};

    #[test]
    fn compaction_summary_remains_typed_and_message_backed() {
        let range = ObservationSourceRangeV1::new(10, 20).unwrap();
        let id = native_record_id("session", range).unwrap();
        let envelope = normalize_observation(
            &json!({
                "role": "assistant",
                "content": "Previous context has been compacted. Here is the compaction output: summary"
            }),
            "session",
            id,
            range,
        )
        .unwrap();

        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }))
        );
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Compaction {
                summary: Some(_),
                ..
            }
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary
            }
        )));
    }

    #[test]
    fn tool_only_assistant_turn_remains_durable() {
        let range = ObservationSourceRangeV1::new(20, 30).unwrap();
        let id = native_record_id("session", range).unwrap();
        let envelope = normalize_observation(
            &json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "read", "arguments": "{\"path\":\"x\"}"}
                }]
            }),
            "session",
            id,
            range,
        )
        .unwrap();

        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::ToolInvocation { .. }))
        );
    }

    #[test]
    fn reasoning_is_not_duplicated_into_message_content() {
        let range = ObservationSourceRangeV1::new(30, 40).unwrap();
        let envelope = normalize_observation(
            &json!({
                "role": "assistant",
                "content": [
                    {"type": "think", "think": "private chain"},
                    {"type": "text", "text": "public answer"}
                ]
            }),
            "session",
            native_record_id("session", range).unwrap(),
            range,
        )
        .unwrap();

        let message = envelope.facts().iter().find_map(|fact| match fact {
            CanonicalObservationFactV1::Message { content, .. } => Some(content),
            _ => None,
        });
        assert_eq!(
            message,
            Some(&json!([{"type": "text", "text": "public answer"}]))
        );
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                content: Some(content),
                ..
            } if content == "private chain"
        )));
    }

    #[test]
    fn native_system_and_usage_records_keep_supported_semantics() {
        let system_range = ObservationSourceRangeV1::new(40, 50).unwrap();
        let system = normalize_observation(
            &json!({"role": "system", "content": "instructions"}),
            "session",
            native_record_id("session", system_range).unwrap(),
            system_range,
        )
        .unwrap();
        assert!(system.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: tracedecay_domain::CanonicalMessageRoleV1::System,
                ..
            }
        )));

        let usage_range = ObservationSourceRangeV1::new(50, 60).unwrap();
        let usage = normalize_observation(
            &json!({
                "role": "_usage",
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 7,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "reasoning_tokens": 5,
                    "total_tokens": 18
                }
            }),
            "session",
            native_record_id("session", usage_range).unwrap(),
            usage_range,
        )
        .unwrap();
        assert!(usage.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::UncorrelatedUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                cache_read_tokens: Some(3),
                cache_write_tokens: Some(2),
                reasoning_tokens: Some(5),
                total_tokens: Some(18),
                native_kind,
                native_field,
                missing_dimensions,
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
