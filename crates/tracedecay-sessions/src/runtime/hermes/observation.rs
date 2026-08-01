//! Native Hermes observation model, canonical normalization, and admission
//! preparation for the shared observation authority.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::observation::{CaptureObservationRequest, ObservationCancellation};
use crate::runtime::shared::path_belongs_to_project;
use tracedecay_runtime_core::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    parse_normalized_observation_record_v1,
};

use super::ingest::HermesProfileSource;
use super::rows::{HermesRow, hermes_native_payload_bytes};
use super::{OBSERVATION_RETENTION, PROVIDER};

pub struct HermesObservationRecord {
    pub native: Value,
    pub native_record_id: ObservationId,
    pub source: ObservationSourceIdentityV1,
    pub range: ObservationSourceRangeV1,
}

#[derive(Clone)]
pub struct HermesProjectionMetadata {
    pub project_path: Option<String>,
    pub location_path: Option<String>,
    pub profile: Option<String>,
    pub location_provenance: Option<&'static str>,
}

pub(super) fn project_projection_metadata(
    row: &HermesRow,
    source: &HermesProfileSource,
    authority_project_root: &Path,
    location_provenance: &'static str,
) -> HermesProjectionMetadata {
    let presentation_path = match location_provenance {
        "profile_pin" => source.legacy_project_pin.as_deref(),
        "session_cwd" => row.session_cwd.as_deref().map(Path::new),
        _ => None,
    }
    .filter(|path| path.is_absolute() && path_belongs_to_project(path, authority_project_root))
    .unwrap_or(authority_project_root);
    HermesProjectionMetadata {
        project_path: Some(authority_project_root.to_string_lossy().into_owned()),
        location_path: Some(presentation_path.to_string_lossy().into_owned()),
        profile: source.profile.clone(),
        location_provenance: Some(location_provenance),
    }
}

pub enum HermesAdmissionAction {
    Capture(Box<CaptureObservationRequest>),
    Cover(ObservationCoverageReason),
}

pub(super) struct HermesAdmission {
    pub source: ObservationSourceIdentityV1,
    pub range: ObservationSourceRangeV1,
    pub expected_cursor: Option<ObservationSourceCursorV1>,
    pub action: HermesAdmissionAction,
}

pub fn observation_source(row: &HermesRow) -> Result<ObservationSourceIdentityV1, String> {
    let provider = ProviderId::new(PROVIDER).map_err(|_| "invalid Hermes provider".to_string())?;
    let session_id =
        SessionId::new(&row.session_id).map_err(|_| "invalid Hermes session id".to_string())?;
    ObservationSourceIdentityV1::for_provider(provider, session_id)
        .map_err(|_| "invalid Hermes observation source".to_string())
}

#[derive(serde::Deserialize)]
struct HermesNativeObservation {
    session_id: String,
    parent_session_id: Option<String>,
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    model: Option<String>,
    tool_name: Option<String>,
    tool_calls: Option<Value>,
    timestamp: Option<f64>,
    usage: HermesNativeUsage,
    project_path: Option<String>,
    location_path: Option<String>,
    title: Option<String>,
    started_at: Option<f64>,
    ended_at: Option<f64>,
    source: Option<String>,
    profile: Option<String>,
    location_provenance: Option<String>,
}

#[derive(serde::Deserialize)]
struct HermesNativeUsage {
    #[serde(rename = "input_tokens")]
    input: Option<i64>,
    #[serde(rename = "output_tokens")]
    output: Option<i64>,
    #[serde(rename = "cache_read_tokens")]
    cache_read: Option<i64>,
    #[serde(rename = "cache_write_tokens")]
    cache_write: Option<i64>,
    #[serde(rename = "reasoning_tokens")]
    reasoning: Option<i64>,
}

pub fn native_observation_record(
    row: &HermesRow,
    projection: &HermesProjectionMetadata,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
) -> Result<HermesObservationRecord, ObservationCoverageReason> {
    if [row.timestamp, row.session_started_at, row.session_ended_at]
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(ObservationCoverageReason::MalformedFrame);
    }
    let tool_calls = match row
        .tool_calls
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => Some(
            serde_json::from_str::<Value>(value)
                .map_err(|_| ObservationCoverageReason::MalformedFrame)?,
        ),
        None => None,
    };
    let native = json!({
        "session_id": row.session_id,
        "parent_session_id": row.parent_session_id,
        "role": row.role,
        "content": row.content,
        "reasoning": row.reasoning,
        "model": row.session_model,
        "tool_name": row.tool_name,
        "tool_calls": tool_calls,
        "timestamp": row.timestamp,
        "project_path": projection.project_path,
        "location_path": projection.location_path,
        "title": row.session_title,
        "started_at": row.session_started_at,
        "ended_at": row.session_ended_at,
        "source": row.session_source,
        "profile": projection.profile,
        "location_provenance": projection.location_provenance,
        "usage": {
            "input_tokens": row.session_input_tokens,
            "output_tokens": row.session_output_tokens,
            "cache_read_tokens": row.session_cache_read_tokens,
            "cache_write_tokens": row.session_cache_write_tokens,
            "reasoning_tokens": row.session_reasoning_tokens,
        },
    });
    let native_record_id = stable_native_id("hermes.native", &immutable_message_evidence(&native))
        .map_err(|()| ObservationCoverageReason::MalformedFrame)?;
    Ok(HermesObservationRecord {
        native,
        native_record_id,
        source,
        range,
    })
}

fn immutable_message_evidence(native: &Value) -> Value {
    json!({
        "session_id": native.get("session_id"),
        "role": native.get("role"),
        "content": native.get("content"),
        "reasoning": native.get("reasoning"),
        "tool_name": native.get("tool_name"),
        "tool_calls": native.get("tool_calls"),
        "timestamp_bits": native
            .get("timestamp")
            .and_then(Value::as_f64)
            .map(f64::to_bits),
    })
}

pub(super) fn stable_native_id(prefix: &str, evidence: &Value) -> Result<ObservationId, ()> {
    let digest = PayloadReferenceV1::for_payload(evidence).map_err(|_| ())?;
    ObservationId::new(format!("{prefix}.{}", digest.digest().as_str())).map_err(|_| ())
}

pub fn normalize_native_observation(
    native: Value,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native: HermesNativeObservation =
        serde_json::from_value(native).map_err(|_| ObservationRecordParseErrorV1::Malformed)?;
    let provider = ProviderId::new(PROVIDER)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let session_id = SessionId::new(&native.session_id)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    // Preserve Hermes' public V1 message identity while the observation keeps
    // its content-derived idempotency key. SQLite row IDs are native ordering
    // evidence and remain stable for the lifetime of one state database.
    let message_id = ObservationId::new(format!("{}:{}", native.session_id, range.end()))
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let identity_evidence = json!({
        "session_id": native.session_id,
        "role": native.role,
        "content": native.content,
        "reasoning": native.reasoning,
        "tool_name": native.tool_name,
        "tool_calls": native.tool_calls,
        "timestamp_bits": native.timestamp.map(f64::to_bits),
    });
    let stable_record_id = stable_native_id("hermes.native", &identity_evidence)
        .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let agent_id = stable_native_id("hermes.session", &json!(native.session_id))
        .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let mut relations = CanonicalObservationRelationsV1::new(session_id)
        .with_message_id(message_id)
        .with_agent_id(agent_id);
    if let Some(parent_session_id) = native.parent_session_id.as_deref() {
        let parent_session_id = SessionId::new(parent_session_id)
            .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        let parent_agent_id = stable_native_id("hermes.session", &json!(parent_session_id))
            .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        relations = relations
            .with_parent_session_id(parent_session_id)
            .with_parent_agent_id(parent_agent_id);
    }

    let role = canonical_message_role(&native.role)?;
    let mut facts = vec![CanonicalObservationFactV1::Session {
        project_path: native.project_path,
        location_path: native.location_path,
        transcript_path: None,
        title: native.title,
        started_at: native.started_at.map(|value| value as i64),
        ended_at: native.ended_at.map(|value| value as i64),
        source: Some("hermes_state_db".to_string()),
        native_source: native.source,
        profile: native.profile,
        location_provenance: native.location_provenance,
    }];
    // Message carries provider-authored content only. Empty assistant rows keep
    // typed Reasoning / ToolInvocation facts projectable instead of synthesizing
    // reasoning text or tool_calls JSON into Message.content.
    if let Some(content) = native
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(|content| Value::String(content.to_string()))
    {
        facts.push(CanonicalObservationFactV1::Message {
            role,
            content,
            model: native.model.clone(),
            timestamp: native.timestamp.map(|value| value as i64),
        });
    }
    if role == CanonicalMessageRoleV1::Tool {
        facts.push(CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: native.content.clone().map_or(Value::Null, Value::String),
            success: None,
        });
    }
    append_tool_invocations(&mut facts, native.tool_calls.as_ref(), &stable_record_id)?;

    let (visibility, content) = match native.reasoning {
        Some(content) => (
            CanonicalReasoningVisibilityV1::Visible,
            Some(Value::String(content)),
        ),
        None if role == CanonicalMessageRoleV1::Assistant => {
            (CanonicalReasoningVisibilityV1::Unavailable, None)
        }
        None => (CanonicalReasoningVisibilityV1::NotApplicable, None),
    };
    facts.push(CanonicalObservationFactV1::Reasoning {
        visibility,
        content,
    });
    // Reasoning before Usage so reasoning-only rows project as reasoning_visible
    // instead of an empty usage fallback when Message is absent.
    if let Some((
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    )) = canonical_usage(&native.usage)?
    {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range)
            .with_native_sequence(range.end());
    if let Some(timestamp) = native.timestamp {
        evidence = evidence.with_native_timestamp(timestamp as i64);
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

fn canonical_message_role(
    role: &str,
) -> Result<CanonicalMessageRoleV1, ObservationRecordParseErrorV1> {
    match role {
        "user" => Ok(CanonicalMessageRoleV1::User),
        "assistant" => Ok(CanonicalMessageRoleV1::Assistant),
        "system" => Ok(CanonicalMessageRoleV1::System),
        "tool" => Ok(CanonicalMessageRoleV1::Tool),
        _ => Err(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope),
    }
}

fn append_tool_invocations(
    facts: &mut Vec<CanonicalObservationFactV1>,
    tool_calls: Option<&Value>,
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let Some(tool_calls) = tool_calls else {
        return Ok(());
    };
    let calls = tool_calls
        .as_array()
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    for call in calls {
        let name = call
            .pointer("/function/name")
            .or_else(|| call.get("name"))
            .and_then(Value::as_str)
            .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"))
            .cloned()
            .unwrap_or(Value::Null);
        let arguments = match arguments {
            Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
            value => value,
        };
        let invocation_evidence = match call.get("id") {
            Some(native_id) => json!({
                "message_id": message_id.as_str(),
                "native_tool_id": native_id,
            }),
            None => json!({
                "message_id": message_id.as_str(),
                "tool_call": call,
            }),
        };
        let invocation_id = stable_native_id("hermes.tool", &invocation_evidence)
            .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name: name.to_owned(),
            arguments,
        });
    }
    Ok(())
}

type CanonicalUsage = (
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
);

fn canonical_usage(
    usage: &HermesNativeUsage,
) -> Result<Option<CanonicalUsage>, ObservationRecordParseErrorV1> {
    let usage = (
        nonnegative_token_count(usage.input)?,
        nonnegative_token_count(usage.output)?,
        nonnegative_token_count(usage.cache_read)?,
        nonnegative_token_count(usage.cache_write)?,
        nonnegative_token_count(usage.reasoning)?,
    );
    Ok((usage.0.is_some()
        || usage.1.is_some()
        || usage.2.is_some()
        || usage.3.is_some()
        || usage.4.is_some())
    .then_some(usage))
}

fn nonnegative_token_count(
    value: Option<i64>,
) -> Result<Option<u64>, ObservationRecordParseErrorV1> {
    value
        .map(u64::try_from)
        .transpose()
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_observation_row(
    row: &HermesRow,
    projection: Option<&HermesProjectionMetadata>,
    scope: &ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    file_identity: u64,
    resume_fingerprint: u64,
) -> Result<HermesAdmission, String> {
    prepare_observation_row_with_cancellation(
        row,
        projection,
        scope,
        generation,
        expected_cursor,
        file_identity,
        resume_fingerprint,
        &ObservationCancellation::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_observation_row_with_cancellation(
    row: &HermesRow,
    projection: Option<&HermesProjectionMetadata>,
    scope: &ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    file_identity: u64,
    resume_fingerprint: u64,
    cancellation: &ObservationCancellation,
) -> Result<HermesAdmission, String> {
    let source = observation_source(row)?;
    let start = expected_cursor.as_ref().map_or(0, |cursor| {
        if cursor.generation() == generation {
            cursor.position()
        } else {
            0
        }
    });
    let end = u64::try_from(row.id).map_err(|_| "invalid Hermes SQLite row id".to_string())?;
    let range = ObservationSourceRangeV1::new(start, end)
        .map_err(|_| "invalid Hermes SQLite row range".to_string())?;
    let coverage = if row.sql_value_oversized
        || hermes_native_payload_bytes(row)
            > u64::try_from(MAX_OBSERVATION_RECORD_BYTES).unwrap_or(u64::MAX)
    {
        Some(ObservationCoverageReason::OversizedFrame)
    } else if projection.is_none() || row.active == 0 {
        Some(ObservationCoverageReason::OutOfScope)
    } else if !matches!(row.role.as_str(), "user" | "assistant" | "tool" | "system") {
        Some(ObservationCoverageReason::UnsupportedFact)
    } else if row
        .content
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && row
            .reasoning
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && row
            .tool_calls
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && row
            .tool_name
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        Some(ObservationCoverageReason::BlankFrame)
    } else {
        None
    };
    let action = if let Some(reason) = coverage {
        HermesAdmissionAction::Cover(reason)
    } else {
        let projection = projection
            .ok_or_else(|| "admitted Hermes row has no projection metadata".to_string())?;
        match native_observation_record(row, projection, source.clone(), range) {
            Err(reason) => HermesAdmissionAction::Cover(reason),
            Ok(normalized) => {
                let encoded = serde_json::to_vec(&normalized.native)
                    .map_err(|_| "could not encode Hermes observation".to_string())?;
                if encoded.len() > MAX_OBSERVATION_RECORD_BYTES {
                    HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
                } else {
                    match parse_normalized_observation_record_v1(
                        &encoded,
                        normalized.range,
                        ObservationOrderingDomainV1::SqliteRowId,
                        |native| normalize_native_observation(native, normalized.range),
                    ) {
                        Err(
                            ObservationRecordParseErrorV1::TooLarge
                            | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge,
                        ) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
                        }
                        Err(ObservationRecordParseErrorV1::Empty) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::BlankFrame)
                        }
                        Err(_) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
                        }
                        Ok(parsed) => {
                            let identity = ObservationIdentityMaterialV1::for_native_record(
                                normalized.source,
                                scope.clone(),
                                generation,
                                normalized.range,
                                ObservationOrderingDomainV1::SqliteRowId,
                                normalized.native_record_id,
                            )
                            .map_err(|_| "invalid Hermes observation identity".to_string())?;
                            let retention = RetentionClass::new(OBSERVATION_RETENTION)
                                .map_err(|_| "invalid Hermes retention class".to_string())?;
                            let request = CaptureObservationRequest::new(
                                parsed,
                                identity,
                                expected_cursor.clone(),
                                retention,
                                cancellation.clone(),
                            )
                            .map_err(|_| "invalid Hermes capture request".to_string())?
                            .with_resume_checkpoint(file_identity, resume_fingerprint);
                            HermesAdmissionAction::Capture(Box::new(request))
                        }
                    }
                }
            }
        }
    };
    Ok(HermesAdmission {
        source,
        range,
        expected_cursor,
        action,
    })
}
