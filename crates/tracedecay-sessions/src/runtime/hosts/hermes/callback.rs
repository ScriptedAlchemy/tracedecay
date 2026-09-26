//! Hermes turns the host inlines in its turn-completed callback.
//!
//! Each message is admitted through the same observation authority as a
//! `state.db` sweep row, so the shared projection drain and the session
//! temporal refresh make it retrievable exactly like swept history. A callback
//! carries no `SQLite` row ids, so it writes its own daemon-sequenced source
//! stream of the session, and the host's stable message id is the projected
//! message identity.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_store::ObservationPersistOutcome;

use crate::admission::{HostAdmission, HostAdmissionOutcome};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::source::TranscriptIngestError;

use super::observation::stable_native_id;
use super::{OBSERVATION_RETENTION, PROVIDER};

const ORDERING: ObservationOrderingDomainV1 = ObservationOrderingDomainV1::DaemonSequence;
/// The daemon mints the callback stream, so it has exactly one generation.
const CALLBACK_GENERATION: u64 = 1;
const NOT_DURABLE: &str = "normalized observation record is not durable";

/// What one callback turn did to the observation authority.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HermesTurnCallbackOutcome {
    /// Messages durably admitted by this call.
    pub committed: u64,
    /// Messages already durable before this call.
    pub duplicates: u64,
}

/// Admits one inlined Hermes turn under `scope`. `project_path` is the admitted
/// project root on the project route. The caller drains the scope's projection
/// queue afterwards, as every capture route does.
pub async fn capture_turn_callback(
    facade: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    project_path: Option<&Path>,
    session_id: &str,
    messages: &[Value],
    cancellation: &ObservationCancellation,
) -> Result<HermesTurnCallbackOutcome, TranscriptIngestError> {
    let source = ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new(PROVIDER)?,
        SessionId::new(session_id)?,
        SessionId::new(format!("{session_id}:turn-callback"))?,
    )?;
    let generation = ObservationSourceGenerationV1::new(CALLBACK_GENERATION)?;
    let retention = RetentionClass::new(OBSERVATION_RETENTION)?;
    let project_path = project_path.map(|path| path.to_string_lossy().into_owned());
    let mut outcome = HermesTurnCallbackOutcome::default();
    for message in messages {
        let expected_cursor = facade
            .get_source_cursor(&source, scope)
            .await
            .map_err(admission_failure)?;
        let start = match &expected_cursor {
            None => 0,
            Some(cursor)
                if cursor.ordering_domain() == ORDERING && cursor.generation() == generation =>
            {
                cursor.position()
            }
            Some(_) => return Err(TranscriptIngestError::InvalidFrameState { provider: PROVIDER }),
        };
        let range = ObservationSourceRangeV1::new(start, start.saturating_add(1))?;
        let non_durable = || TranscriptIngestError::NonDurableRecord {
            provider: PROVIDER,
            offset: range.start(),
            end_offset: range.end(),
            reason: NOT_DURABLE,
        };
        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(non_durable)?;
        if facade
            .has_session_message(scope, PROVIDER, message_id)
            .await
            .map_err(admission_failure)?
        {
            outcome.duplicates += 1;
            continue;
        }
        let record_id = stable_native_id(
            "hermes.callback",
            &json!({ "session_id": session_id, "message_id": message_id }),
        )
        .map_err(|()| non_durable())?;
        let native = json!({
            "session_id": session_id,
            "message_id": message_id,
            "role": message.get("role").and_then(Value::as_str),
            "content": message.get("content").and_then(Value::as_str),
            "timestamp": message.get("timestamp").and_then(Value::as_f64),
            "project_path": project_path,
        });
        let encoded = serde_json::to_vec(&native).map_err(|_| non_durable())?;
        let envelope_record_id = record_id.clone();
        let parsed = parse_normalized_observation_record_v1(&encoded, range, ORDERING, |native| {
            normalize_callback_message(&native, range, envelope_record_id)
        })
        .map_err(|_| non_durable())?;
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ORDERING,
            record_id,
        )?;
        let request = CaptureObservationRequest::new(
            parsed,
            identity,
            expected_cursor,
            retention.clone(),
            cancellation.clone(),
        )
        .map_err(|_| non_durable())?;
        match facade
            .capture_observation(request)
            .await
            .map_err(admission_failure)?
        {
            CaptureObservationOutcome::Persisted {
                outcome: persisted, ..
            }
            | CaptureObservationOutcome::AcceptedForReplay {
                outcome: persisted, ..
            } => match *persisted {
                ObservationPersistOutcome::Committed(_) => outcome.committed += 1,
                ObservationPersistOutcome::ExactDuplicate(_)
                | ObservationPersistOutcome::CoveredDuplicate(_) => outcome.duplicates += 1,
            },
            CaptureObservationOutcome::Rejected { .. }
            | CaptureObservationOutcome::Quarantined { .. } => return Err(non_durable()),
        }
    }
    Ok(outcome)
}

fn admission_failure(outcome: HostAdmissionOutcome) -> TranscriptIngestError {
    TranscriptIngestError::HostAdmission {
        provider: PROVIDER,
        reason: outcome.reason_code.unwrap_or("host_admission_refused"),
        retryable: outcome.retryable,
        detail: outcome.cause,
    }
}

fn normalize_callback_message(
    native: &Value,
    range: ObservationSourceRangeV1,
    record_id: ObservationId,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    const INVALID: ObservationRecordParseErrorV1 =
        ObservationRecordParseErrorV1::InvalidCanonicalEnvelope;
    let text = |key: &str| native.get(key).and_then(Value::as_str);
    let session_id = SessionId::new(text("session_id").unwrap_or_default()).map_err(|_| INVALID)?;
    let message_id = text("message_id").unwrap_or_default();
    let role = text("role")
        .and_then(CanonicalMessageRoleV1::from_known_label)
        .ok_or(INVALID)?;
    let content = text("content")
        .filter(|content| !content.trim().is_empty())
        .ok_or(ObservationRecordParseErrorV1::Empty)?;
    let timestamp = native
        .get("timestamp")
        .and_then(Value::as_f64)
        .filter(|timestamp| timestamp.is_finite())
        .map(|timestamp| timestamp as i64);
    let project_path = text("project_path").map(str::to_owned);
    let agent_id =
        stable_native_id("hermes.session", &json!(session_id.as_str())).map_err(|()| INVALID)?;
    let relations = CanonicalObservationRelationsV1::new(session_id)
        .with_message_id(ObservationId::new(message_id.to_owned()).map_err(|_| INVALID)?)
        .with_agent_id(agent_id);
    let facts = vec![
        CanonicalObservationFactV1::Session {
            project_path: project_path.clone(),
            location_path: project_path,
            transcript_path: None,
            title: None,
            started_at: None,
            ended_at: None,
            source: Some("hermes_turn_callback".to_owned()),
            native_source: None,
            profile: None,
            location_provenance: None,
        },
        CanonicalObservationFactV1::Message {
            role,
            content: Value::String(content.to_owned()),
            model: None,
            timestamp,
        },
        CanonicalObservationFactV1::Reasoning {
            visibility: if role == CanonicalMessageRoleV1::Assistant {
                CanonicalReasoningVisibilityV1::Unavailable
            } else {
                CanonicalReasoningVisibilityV1::NotApplicable
            },
            content: None,
        },
    ];
    let mut evidence = CanonicalObservationEvidenceV1::new(ORDERING, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER).map_err(|_| INVALID)?,
        "message",
        record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| INVALID)
}
