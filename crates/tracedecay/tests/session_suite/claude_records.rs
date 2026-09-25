//! Claude transcript fixtures shaped by the production normalizer.
//!
//! Projection accepts only canonical observation envelopes, so a fixture that
//! persists a raw JSONL record never reaches the projector. These helpers run
//! the same `stable_record_id` + `normalize` path the Claude host applies to
//! every transcript record, so the suites persist and project what ships.

use serde_json::{Value, json};
use tracedecay_capture::claude::{normalize, stable_record_id};
use tracedecay_domain::{ObservationId, ObservationSourceRangeV1};

/// One native assistant record carrying only visible text.
pub(crate) fn assistant_record(text: &str) -> Value {
    json!({
        "type": "assistant",
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "model": "claude-sonnet-4"
        }
    })
}

/// The record identity the Claude host derives for `native` read at byte
/// `offset` of `session_id`'s transcript: its row `uuid`, else `message.id`,
/// else the positional `session:offset` fallback.
pub(crate) fn record_id(native: &Value, session_id: &str, offset: u64) -> ObservationId {
    stable_record_id(native, session_id, offset).unwrap()
}

/// The canonical envelope the Claude host persists for `native` read at
/// `start..end` of `session_id`'s transcript.
pub(crate) fn canonical_envelope(native: &Value, session_id: &str, start: u64, end: u64) -> Value {
    let range = ObservationSourceRangeV1::new(start, end).unwrap();
    let envelope = normalize(
        native,
        session_id,
        record_id(native, session_id, start),
        range,
    )
    .unwrap();
    serde_json::to_value(envelope).unwrap()
}
