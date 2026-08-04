use serde_json::Value;
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, ObservationId, ObservationOrderingDomainV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProviderId, SessionId,
};

use crate::ObservationRecordParseErrorV1;

const PROVIDER: &str = "opencode";

pub fn normalize_observation(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let message = native.get("message").unwrap_or(native);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let parts = native
        .get("parts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut facts = Vec::new();
    if let Some(content) = message_content(parts) {
        facts.push(CanonicalObservationFactV1::Message {
            role: canonical_role(role),
            content,
            model: message
                .pointer("/model/modelID")
                .or_else(|| message.pointer("/model/model_id"))
                .or_else(|| message.get("modelID"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            timestamp: timestamp_secs(
                message
                    .pointer("/time/created")
                    .or_else(|| message.get("time_created")),
            ),
        });
    }
    append_parts(&mut facts, parts, &stable_record_id)?;
    if facts.is_empty() {
        return Err(ObservationRecordParseErrorV1::Empty);
    }

    let timestamp = timestamp_secs(
        message
            .pointer("/time/created")
            .or_else(|| message.get("time_created")),
    );
    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER).map_err(|_| invalid())?,
        "message",
        stable_record_id.clone(),
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?)
            .with_message_id(stable_record_id),
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

fn append_parts(
    facts: &mut Vec<CanonicalObservationFactV1>,
    parts: &[Value],
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("reasoning") => facts.push(CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: part.get("text").or_else(|| part.get("content")).cloned(),
            }),
            Some("compaction") => {
                facts.push(CanonicalObservationFactV1::Compaction {
                    summary: part
                        .get("summary")
                        .or_else(|| part.get("text"))
                        .or_else(|| part.get("content"))
                        .cloned(),
                    input_tokens: None,
                    output_tokens: None,
                });
                facts.push(CanonicalObservationFactV1::Boundary {
                    boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
                });
            }
            Some("tool") => append_tool_fact(facts, part, message_id)?,
            _ => {}
        }
    }
    Ok(())
}

fn append_tool_fact(
    facts: &mut Vec<CanonicalObservationFactV1>,
    part: &Value,
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let name = part
        .get("tool")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty());
    let state = part.get("state").unwrap_or(part);
    let input = state
        .get("input")
        .or_else(|| part.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    let tool_id = stable_tool_id(message_id, part)?;
    if let Some(name) = name {
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: tool_id.clone(),
            name: name.to_owned(),
            arguments: input,
        });
    }
    if let Some(output) = state
        .get("output")
        .or_else(|| state.get("error"))
        .or_else(|| part.get("output"))
        .or_else(|| part.get("error"))
    {
        facts.push(CanonicalObservationFactV1::ToolResult {
            invocation_id: Some(tool_id),
            content: output.clone(),
            success: state
                .get("status")
                .and_then(Value::as_str)
                .and_then(|status| match status {
                    "completed" => Some(true),
                    "error" => Some(false),
                    _ => None,
                }),
        });
    }
    Ok(())
}

fn stable_tool_id(
    message_id: &ObservationId,
    part: &Value,
) -> Result<ObservationId, ObservationRecordParseErrorV1> {
    if let Some(id) = part
        .get("callID")
        .or_else(|| part.get("call_id"))
        .or_else(|| part.get("id"))
        .and_then(Value::as_str)
        .and_then(|id| ObservationId::new(id).ok())
    {
        return Ok(id);
    }
    let evidence = serde_json::json!({"message_id": message_id.as_str(), "part": part});
    let digest = PayloadReferenceV1::for_payload(&evidence).map_err(|_| invalid())?;
    ObservationId::new(format!("opencode.tool.{}", digest.digest().as_str())).map_err(|_| invalid())
}

fn message_content(parts: &[Value]) -> Option<Value> {
    let content = parts
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("text" | "file")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    (!content.is_empty()).then_some(Value::Array(content))
}

fn canonical_role(role: &str) -> CanonicalMessageRoleV1 {
    match role {
        "user" => CanonicalMessageRoleV1::User,
        "assistant" => CanonicalMessageRoleV1::Assistant,
        "system" => CanonicalMessageRoleV1::System,
        "tool" => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    }
}

fn timestamp_secs(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).map(|timestamp| {
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
    use serde_json::json;
    use tracedecay_domain::{CanonicalObservationFactV1, ObservationId, ObservationSourceRangeV1};

    use super::normalize_observation;

    #[test]
    fn preserves_text_reasoning_tool_and_compaction_as_distinct_facts() {
        let envelope = normalize_observation(
            &json!({
                "message": {"id": "msg_1", "role": "assistant", "time": {"created": 1_700_000_000_000_i64}},
                "parts": [
                    {"id": "p1", "type": "text", "text": "answer"},
                    {"id": "p2", "type": "reasoning", "text": "reason"},
                    {"id": "p3", "type": "tool", "tool": "read", "callID": "call_1",
                     "state": {"status": "completed", "input": {"path": "x"}, "output": "ok"}},
                    {"id": "p4", "type": "compaction", "summary": "summary"}
                ]
            }),
            "session",
            ObservationId::new("msg_1").unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
        )
        .unwrap();

        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Compaction {
                summary: Some(_),
                ..
            }
        )));
        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Reasoning { .. }))
        );
        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::ToolInvocation { .. }))
        );
        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::ToolResult { .. }))
        );
    }
}
