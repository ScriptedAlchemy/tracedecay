//! Provider-shaped decisions taken by the canonical observation projection.
//!
//! The projection reducer itself is provider-neutral: it turns canonical facts
//! into session, message, and workflow records the same way for every host.
//! Two decisions are not neutral, because the provider — or the capture source
//! its records were read through — changes the answer:
//!
//! * whether a record may omit its native record id, because the provider
//!   synthesizes a stable one instead;
//! * whether a provider's tool invocations are normalized into the
//!   cross-provider `tool_calls` / `tool_events` message-metadata shape.
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

use tracedecay_domain::{CanonicalObservationFactV1, ObservationContractError};

use crate::cursor_dispatch::is_subagent_dispatch_tool;
use crate::{ProjectionStoreError, ProjectionStoreResult};

/// Provider that derives a stable record id from record content rather than
/// carrying a provider-native one, so its identity material legitimately omits
/// `native_record_id`.
const SYNTHESIZED_RECORD_ID_PROVIDER: &str = "claude";

/// Capture source naming Cursor records read from its transcript.
const CURSOR_TRANSCRIPT_SOURCE: &str = "cursor_transcript";

/// Normalizes a provider's tool invocations into the cross-provider message
/// metadata shape. Selected by capture source, then applied to the merged
/// metadata map and the record's canonical facts.
pub type ToolMetadataNormalizer = fn(
    &mut serde_json::Map<String, serde_json::Value>,
    &[CanonicalObservationFactV1],
) -> ProjectionStoreResult<()>;

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
/// cross-provider `tool_calls`, `tool_events`, and `tool_use_id` fields.
fn normalize_cursor_tool_metadata(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    facts: &[CanonicalObservationFactV1],
) -> ProjectionStoreResult<()> {
    let mut tool_calls = Vec::new();
    let mut tool_events = Vec::new();
    let mut first_dispatch_id = None;
    for fact in facts {
        let CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name,
            arguments,
        } = fact
        else {
            continue;
        };
        tool_calls.push(serde_json::json!({
            "id": invocation_id.as_str(),
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments,
            },
        }));
        let input_bytes = serde_json::to_vec(arguments)
            .map_err(|_| {
                ProjectionStoreError::Contract(ObservationContractError::CanonicalEncoding)
            })?
            .len();
        tool_events.push(serde_json::json!({
            "type": "tool_use",
            "tool_name": name,
            "call_id": invocation_id.as_str(),
            "input_bytes": input_bytes,
        }));
        if first_dispatch_id.is_none() && is_subagent_dispatch_tool(name) {
            first_dispatch_id = Some(invocation_id.as_str());
        }
    }
    if !tool_calls.is_empty() {
        metadata.insert("tool_calls".to_owned(), tool_calls.into());
        metadata.insert("tool_events".to_owned(), tool_events.into());
    }
    if let Some(tool_use_id) = first_dispatch_id {
        metadata.insert("tool_use_id".to_owned(), tool_use_id.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_synthesizing_provider_may_omit_a_native_record_id() {
        assert!(synthesizes_native_record_id("claude"));
        for provider in ["codex", "cursor", "hermes", ""] {
            assert!(
                !synthesizes_native_record_id(provider),
                "{provider} must carry a native record id"
            );
        }
    }

    #[test]
    fn the_tool_metadata_normalizer_is_selected_by_the_transcript_source_alone() {
        assert!(tool_metadata_normalizer(Some("cursor_transcript")).is_some());
        assert!(tool_metadata_normalizer(Some("cursor_composer")).is_none());
        assert!(tool_metadata_normalizer(Some("provider_store")).is_none());
        assert!(tool_metadata_normalizer(None).is_none());
    }
}
