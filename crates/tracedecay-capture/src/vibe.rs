use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceRangeV1, PayloadReferenceV1, ProviderId,
    ProviderUsageContractDimensionV1, SessionId,
};

use crate::ObservationRecordParseErrorV1;

const PROVIDER: &str = "vibe";

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
    model: Option<&str>,
    stable_record_id: ObservationId,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let provider = ProviderId::new(PROVIDER)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let session_id = SessionId::new(session_id)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let role = canonical_role(native)?;
    let message = native.get("message").unwrap_or(native);
    let content = message
        .get("content")
        .or_else(|| native.get("content"))
        .filter(|content| !content_is_empty(content))
        .cloned()
        .ok_or(ObservationRecordParseErrorV1::Empty)?;
    let timestamp = native
        .get("timestamp")
        .or_else(|| native.get("created_at"))
        .or_else(|| message.get("timestamp"))
        .or_else(|| message.get("created_at"))
        .and_then(timestamp_secs);

    let relations =
        CanonicalObservationRelationsV1::new(session_id).with_message_id(stable_record_id.clone());
    let mut facts = vec![CanonicalObservationFactV1::Message {
        role,
        content,
        model: model.map(str::to_owned),
        timestamp,
    }];
    append_tool_invocations(&mut facts, native, &stable_record_id)?;
    append_usage(&mut facts, native)?;

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}

fn canonical_role(native: &Value) -> Result<CanonicalMessageRoleV1, ObservationRecordParseErrorV1> {
    let role = native
        .get("role")
        .or_else(|| native.pointer("/message/role"))
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    match role {
        "user" => Ok(CanonicalMessageRoleV1::User),
        "assistant" | "model" => Ok(CanonicalMessageRoleV1::Assistant),
        _ => Err(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope),
    }
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

fn timestamp_secs(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .map(|timestamp| {
            if timestamp >= 1_000_000_000_000 {
                timestamp / 1000
            } else {
                timestamp
            }
        })
}

fn append_tool_invocations(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let mut calls = Vec::new();
    if let Some(values) = native
        .get("tool_calls")
        .or_else(|| native.pointer("/message/tool_calls"))
        .and_then(Value::as_array)
    {
        calls.extend(values);
    }
    if let Some(values) = native
        .get("content")
        .or_else(|| native.pointer("/message/content"))
        .and_then(Value::as_array)
    {
        calls.extend(values.iter().filter(|value| {
            matches!(
                value.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_call" | "function_call")
            )
        }));
    }
    for call in calls {
        let Some(name) = call
            .pointer("/function/name")
            .or_else(|| call.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"))
            .or_else(|| call.get("input"))
            .cloned()
            .unwrap_or(Value::Null);
        let arguments = match arguments {
            Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
            value => value,
        };
        let invocation_evidence = json!({
            "message_id": message_id.as_str(),
            "native_tool_id": call.get("id"),
            "name": name,
            "arguments": arguments,
        });
        let digest = PayloadReferenceV1::for_payload(&invocation_evidence)
            .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        let invocation_id = ObservationId::new(format!("vibe.tool.{}", digest.digest().as_str()))
            .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name: name.to_owned(),
            arguments,
        });
    }
    Ok(())
}

fn append_usage(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
) -> Result<(), ObservationRecordParseErrorV1> {
    let Some(usage) = native
        .get("usage")
        .or_else(|| native.pointer("/message/usage"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    let input_tokens = token_count(usage, &["input_tokens", "prompt_tokens"])?;
    let output_tokens = token_count(usage, &["output_tokens", "completion_tokens"])?;
    let cache_read_tokens =
        token_count(usage, &["cache_read_input_tokens", "cached_input_tokens"])?;
    let cache_write_tokens = token_count(usage, &["cache_creation_input_tokens"])?;
    let reasoning_tokens = token_count(usage, &["reasoning_tokens", "reasoning_output_tokens"])?;
    let total_tokens = token_count(usage, &["total_tokens"])?;
    if input_tokens.is_some()
        || output_tokens.is_some()
        || cache_read_tokens.is_some()
        || cache_write_tokens.is_some()
        || reasoning_tokens.is_some()
        || total_tokens.is_some()
    {
        facts.push(CanonicalObservationFactV1::UncorrelatedUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            total_tokens,
            native_kind: "message".to_owned(),
            native_field: if native.get("usage").is_some() {
                "usage"
            } else {
                "message.usage"
            }
            .to_owned(),
            missing_dimensions: BTreeSet::from([
                ProviderUsageContractDimensionV1::Model,
                ProviderUsageContractDimensionV1::Scope,
                ProviderUsageContractDimensionV1::CounterSemantics,
            ]),
        });
    }
    Ok(())
}

fn token_count(
    usage: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Result<Option<u64>, ObservationRecordParseErrorV1> {
    let Some(value) = keys.iter().find_map(|key| usage.get(*key)) else {
        return Ok(None);
    };
    let value = value
        .as_i64()
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    u64::try_from(value)
        .map(Some)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}
