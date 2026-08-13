//! Provider-shaped decisions taken by the canonical observation projection.
//!
//! The projection reducer itself is provider-neutral: it turns canonical facts
//! into session, message, and workflow records the same way for every host.
//! Three decisions are not neutral, because the provider — or the compatibility
//! source its records were captured through — changes the answer:
//!
//! * whether a record may omit its native record id, because the provider
//!   synthesizes a stable one instead;
//! * which namespace the session-location metadata keys are written under;
//! * whether provider-compatibility fields are appended to message metadata.
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

/// Provider whose transcript-sourced records use the event metadata namespace.
const CURSOR_PROVIDER: &str = "cursor";

/// Compatibility source naming Cursor records captured from its transcript.
const CURSOR_TRANSCRIPT_SOURCE: &str = "cursor_transcript";

/// Namespace those transcript records keep for their location metadata keys.
const CURSOR_EVENT_NAMESPACE: &str = "cursor_event";

/// Appends a provider's compatibility fields to already-projected message
/// metadata. Selected by compatibility source, then applied to the merged
/// metadata map and the record's canonical facts.
pub type CompatibilityMetadataHook = fn(
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

/// Namespace prefix for a session's location metadata keys.
///
/// The default is `{provider}_session`. Cursor's transcript-sourced records
/// keep the `cursor_event` namespace they were first written under, so readers
/// of already-persisted metadata continue to resolve the same keys. Both the
/// provider and the compatibility source must match: a cursor record from any
/// other source, and any other provider's `cursor_transcript` record, take the
/// default.
pub fn metadata_namespace(provider: &str, source: Option<&str>) -> String {
    if provider == CURSOR_PROVIDER && source == Some(CURSOR_TRANSCRIPT_SOURCE) {
        CURSOR_EVENT_NAMESPACE.to_owned()
    } else {
        format!("{provider}_session")
    }
}

/// Compatibility-metadata hook for a record's compatibility `source`, if any.
///
/// Keyed by source rather than provider because the source is what records the
/// captured shape: it is the shape, not the host, that decides which
/// compatibility fields downstream readers expect.
pub fn compatibility_metadata_hook(source: Option<&str>) -> Option<CompatibilityMetadataHook> {
    if source == Some(CURSOR_TRANSCRIPT_SOURCE) {
        Some(append_cursor_compatibility_metadata)
    } else {
        None
    }
}

/// Restates a record's tool invocations as the `tool_calls`, `tool_events`, and
/// `tool_use_id` fields Cursor transcript readers were built against.
fn append_cursor_compatibility_metadata(
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
    fn the_event_namespace_needs_the_cursor_provider_and_the_transcript_source() {
        assert_eq!(
            metadata_namespace("cursor", Some("cursor_transcript")),
            "cursor_event"
        );
        assert_eq!(
            metadata_namespace("cursor", Some("cursor_composer")),
            "cursor_session"
        );
        assert_eq!(metadata_namespace("cursor", None), "cursor_session");
        assert_eq!(
            metadata_namespace("codex", Some("cursor_transcript")),
            "codex_session"
        );
        assert_eq!(metadata_namespace("codex", None), "codex_session");
    }

    #[test]
    fn the_compatibility_hook_is_selected_by_the_transcript_source_alone() {
        assert!(compatibility_metadata_hook(Some("cursor_transcript")).is_some());
        assert!(compatibility_metadata_hook(Some("cursor_composer")).is_none());
        assert!(compatibility_metadata_hook(Some("provider_store")).is_none());
        assert!(compatibility_metadata_hook(None).is_none());
    }
}
