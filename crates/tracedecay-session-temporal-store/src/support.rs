//! Crate-local helpers that previously lived as `pub(crate)` global-db internals.

use tracedecay_domain::{CanonicalObservationEnvelopeV1, DurableObservationV1, ObservationScopeV1};
use tracedecay_sessions::runtime::claude::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record,
};
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreResult, SessionRecord,
    derive_canonical_projection,
};

/// Millisecond-scale Unix timestamps are at least 13 digits.
pub(crate) const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

#[inline(always)]
pub(crate) fn record_snapshot_admissions(count: u64) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.observe.snapshot_admissions").inc(count);
    #[cfg(not(feature = "hotpath"))]
    let _ = count;
}

#[inline(always)]
pub(crate) fn record_output_sessions(count: u64) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.observe.output_sessions").inc(count);
    #[cfg(not(feature = "hotpath"))]
    let _ = count;
}

/// Same composition as the observation-projection derive path: canonical
/// envelopes go through store authority; legacy Claude records use the public
/// sessions mapper. Kept here so this crate does not depend on global-db.
pub(crate) fn derive_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    match observation.source().provider().as_str() {
        "claude" if decode_canonical_envelope(observation.payload()).is_ok() => {
            derive_canonical_projection(observation)
        }
        "claude" => derive_claude_projection(observation),
        _ => derive_canonical_projection(observation),
    }
}

fn decode_canonical_envelope(
    payload: &serde_json::Value,
) -> Result<CanonicalObservationEnvelopeV1, serde_json::Error> {
    serde::Deserialize::deserialize(payload)
}

fn derive_claude_projection(
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<ObservationProjection> {
    let session_id = observation.source().session_id().as_str();
    let payload = observation.payload();
    let durable_message_id = payload
        .pointer("/message/id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| payload.get("uuid").and_then(serde_json::Value::as_str))
        .filter(|id| !id.is_empty());
    let durable_tool_event_ids = payload
        .pointer("/message/content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("tool_use_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let (project_key, project_path) = match observation.scope() {
        ObservationScopeV1::Profile => ("user", "user"),
        ObservationScopeV1::Project { project_id } => (project_id.as_str(), project_id.as_str()),
    };
    let source_path = (observation.source().source_key() != observation.source().session_id())
        .then(|| observation.source().source_key().as_str());
    let context = ClaudeRecordContext {
        session_id,
        project_key,
        project_path,
        file_generation: observation.identity().generation().file_id(),
        offset: observation.identity().position().start(),
        session_cwd: None,
        source_path,
        raw_message_id: durable_message_id,
        raw_tool_event_ids: &durable_tool_event_ids,
        raw_hook_tool_use_id: None,
    };

    match map_sanitized_claude_record(payload, &context) {
        ClaudeRecordDisposition::Message { draft, message } => {
            let draft = *draft;
            let message = *message;
            let timestamp = message.timestamp;
            let session = SessionRecord {
                provider: "claude".to_string(),
                session_id: draft.session_id,
                project_key: draft.project_key,
                project_path: draft.project_path,
                title: draft.title,
                started_at: timestamp,
                ended_at: timestamp,
                transcript_path: None,
                metadata_json: draft.metadata_json,
                parent_session_id: draft.parent_session_id,
                is_subagent: draft.is_subagent,
                agent_id: draft.agent_id,
                parent_tool_use_id: draft.parent_tool_use_id,
            };
            ObservationProjection::for_message(observation, session, message)
        }
        ClaudeRecordDisposition::NonConversational => ObservationProjection::for_skip(
            observation,
            ProjectionSkipReason::NonConversationalRecord,
        ),
    }
}
