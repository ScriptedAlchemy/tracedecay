//! Provider-shaped decisions taken by the canonical observation projection.
//!
//! The projection reducer itself is provider-neutral: it turns canonical facts
//! into session, message, and workflow records the same way for every host.
//! Two decisions are not neutral, because the provider, or the capture source
//! its records were read through, changes the answer:
//!
//! * whether a record may omit its native record id, because the provider
//!   synthesizes a stable one instead;
//! * whether a provider's tool invocations are normalized into the
//!   cross-provider `tool_calls` / `tool_events` message-metadata shape;
//! * whether provider-authored context has a typed session-message rendering.
//!
//! Session-location metadata keys are deliberately absent from that list: every
//! provider writes them under `{provider}_session`, so the reducer formats the
//! namespace itself rather than asking a descriptor.
//!
//! Spreading those literals through the reducer made the reducer read as if it
//! were provider-aware everywhere, and left no single place to answer "what is
//! provider-specific about the projection?". Each decision lives here, named,
//! so adding or retiring a provider is a change to this descriptor rather than
//! a search for string comparisons inside the reducer.

use tracedecay_domain::{CanonicalObservationFactV1, ObservationContractError, ObservationId};

use crate::{
    ProjectionStoreError, ProjectionStoreResult, codex_goal_context_from_text,
    codex_message_visible_text,
};

/// Provider that derives a stable record id from record content rather than
/// carrying a provider-native one, so its identity material legitimately omits
/// `native_record_id`.
const SYNTHESIZED_RECORD_ID_PROVIDER: &str = "claude";

/// Capture source naming Cursor records read from its transcript.
const CURSOR_TRANSCRIPT_SOURCE: &str = "cursor_transcript";

const CODEX_PROVIDER: &str = "codex";

pub(crate) struct ProviderMessageSemantics {
    pub role: &'static str,
    pub text: String,
    pub kind: &'static str,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

pub(crate) fn provider_message_semantics(
    provider: &str,
    native_record_kind: &str,
    role: &str,
    content: &serde_json::Value,
    has_native_item_identity: bool,
) -> Option<ProviderMessageSemantics> {
    if provider != CODEX_PROVIDER || role != "user" {
        return None;
    }
    let visible_text = codex_message_visible_text(content);
    let goal = codex_goal_context_from_text(&visible_text)?;
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_owned(),
        serde_json::Value::String("codex_rollout".to_owned()),
    );
    metadata.insert(
        "codex_internal_context".to_owned(),
        serde_json::Value::String("goal".to_owned()),
    );
    let source_event = match native_record_kind {
        "event_msg" if has_native_item_identity => Some("item_completed"),
        "response_item" => Some("response_item"),
        _ => None,
    };
    if let Some(source_event) = source_event {
        metadata.insert(
            "source_event".to_owned(),
            serde_json::Value::String(source_event.to_owned()),
        );
    }
    if native_record_kind == "response_item" {
        metadata.insert(
            "source_role".to_owned(),
            serde_json::Value::String("user".to_owned()),
        );
    }
    metadata.insert("codex_goal".to_owned(), goal.metadata());
    Some(ProviderMessageSemantics {
        role: "system",
        text: goal.storage_text(),
        kind: "goal_context",
        metadata,
    })
}

/// Normalizes a provider's tool invocations into the cross-provider message
/// metadata shape. Selected by capture source, then applied to the merged
/// metadata map and the record's canonical facts. `serves_id` answers whether
/// an invocation id may be served as the host's own.
pub type ToolMetadataNormalizer = fn(
    &mut serde_json::Map<String, serde_json::Value>,
    &[CanonicalObservationFactV1],
    &dyn Fn(&ObservationId) -> bool,
) -> ProjectionStoreResult<()>;

/// Message-metadata key naming why a record's tool calls carry no ids.
pub const TOOL_CALL_ID_COVERAGE_KEY: &str = "tool_call_id_coverage";

/// [`TOOL_CALL_ID_COVERAGE_KEY`] value: the host wrote no id for at least one
/// of the record's tool calls, so that call is served without one.
pub const TOOL_CALL_IDS_HOST_UNRECORDED: &str = "host_unrecorded";

/// Whether `provider` synthesizes its own stable record id.
///
/// A record from such a provider is allowed to carry no native record id: the
/// envelope's `stable_record_id` is the identity. Every other provider must
/// carry one, and the projection rejects the record when it does not match.
pub fn synthesizes_native_record_id(provider: &str) -> bool {
    provider == SYNTHESIZED_RECORD_ID_PROVIDER
}

/// Tool-metadata normalizer for a record's capture `source`, if any.
///
/// Keyed by source rather than provider because the source is what records the
/// captured shape: it is the shape, not the host, that decides which
/// normalization the canonical message metadata still needs.
pub fn tool_metadata_normalizer(source: Option<&str>) -> Option<ToolMetadataNormalizer> {
    if source == Some(CURSOR_TRANSCRIPT_SOURCE) {
        Some(normalize_cursor_tool_metadata)
    } else {
        None
    }
}

/// Restates a Cursor transcript record's tool invocations as the canonical
/// cross-provider `tool_calls` and `tool_events` fields. The record's
/// `tool_use_id` is provider-neutral and written by the reducer itself.
///
/// Cursor's transcript JSONL writes no tool-call ids, so its calls are served
/// without `id`/`call_id` and the record says so rather than carrying the
/// capture's record-rooted fallback, which every call on the record shares.
fn normalize_cursor_tool_metadata(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    facts: &[CanonicalObservationFactV1],
    serves_id: &dyn Fn(&ObservationId) -> bool,
) -> ProjectionStoreResult<()> {
    let mut tool_calls = Vec::new();
    let mut tool_events = Vec::new();
    let mut host_unrecorded = false;
    for fact in facts {
        let CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name,
            arguments,
        } = fact
        else {
            continue;
        };
        let mut tool_call = serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments,
            },
        });
        let input_bytes = serde_json::to_vec(arguments)
            .map_err(|_| {
                ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
            })?
            .len();
        let mut tool_event = serde_json::json!({
            "type": "tool_use",
            "tool_name": name,
            "input_bytes": input_bytes,
        });
        if serves_id(invocation_id) {
            tool_call["id"] = invocation_id.as_str().into();
            tool_event["call_id"] = invocation_id.as_str().into();
        } else {
            host_unrecorded = true;
        }
        tool_calls.push(tool_call);
        tool_events.push(tool_event);
    }
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".to_owned(), tool_calls.into());
        metadata.insert("tool_events".to_owned(), tool_events.into());
    }
    if host_unrecorded {
        metadata.insert(
            TOOL_CALL_ID_COVERAGE_KEY.to_owned(),
            TOOL_CALL_IDS_HOST_UNRECORDED.into(),
        );
    }
    Ok(())
}
