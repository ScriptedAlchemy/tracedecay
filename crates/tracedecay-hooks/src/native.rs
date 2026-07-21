//! Provider-native Hook V2 decoding.
//!
//! These adapters only recognize checked-in native event names and preserve
//! their event-family provenance. They deliberately discard prompts, paths,
//! tool arguments, output, and provider identifiers; opaque IDs are supplied
//! later by the daemon-issued binding/material contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::UtcMicros;

use crate::{
    HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1, HookContractError, HookEventEnvelopeV2,
    HookEventFamily, HookEventSupportV1, HookEventV2, HookHostV1, HookLifecyclePhaseV1,
    HookOrderingV1, HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES, stock_event_support,
};

/// The bounded, content-free signal yielded from one native host event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHookSignalV1 {
    SessionBoundary(HookBoundaryV1),
    PromptBoundary,
    ToolLifecycle(HookLifecyclePhaseV1),
    SavedEdit,
}

impl NativeHookSignalV1 {
    pub const fn family(self) -> HookEventFamily {
        match self {
            Self::SessionBoundary(_) => HookEventFamily::SessionBoundary,
            Self::PromptBoundary => HookEventFamily::PromptBoundary,
            Self::ToolLifecycle(_) => HookEventFamily::ToolLifecycle,
            Self::SavedEdit => HookEventFamily::SavedEdit,
        }
    }
}

/// A successfully decoded provider-native event. This type intentionally has
/// no field capable of retaining a host payload or workspace path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedNativeHookEventV1 {
    pub host: HookHostV1,
    pub signal: NativeHookSignalV1,
    pub ordering: HookOrderingV1,
}

impl DecodedNativeHookEventV1 {
    pub const fn family(self) -> HookEventFamily {
        self.signal.family()
    }

    /// Convert a decoded native signal into the closed Hook V2 envelope using
    /// only opaque material furnished by the binding/admission path.
    pub fn into_envelope(
        self,
        binding: &HookScopeBindingV1,
        material: NativeEnvelopeMaterialV1,
    ) -> Result<HookEventEnvelopeV2, NativeHookDecodeError> {
        if binding.host != self.host {
            return Err(NativeHookDecodeError::BindingHostMismatch);
        }
        let event = match self.signal {
            NativeHookSignalV1::SessionBoundary(boundary) => {
                HookEventV2::SessionBoundary { boundary }
            }
            NativeHookSignalV1::PromptBoundary => HookEventV2::PromptBoundary,
            NativeHookSignalV1::ToolLifecycle(phase) => HookEventV2::ToolLifecycle {
                tool_id: material
                    .tool_id
                    .ok_or(NativeHookDecodeError::MissingOpaqueMaterial)?,
                phase,
                effect_receipt_id: material.effect_receipt_id,
            },
            NativeHookSignalV1::SavedEdit => HookEventV2::SavedEdit {
                file_id: material
                    .file_id
                    .ok_or(NativeHookDecodeError::MissingOpaqueMaterial)?,
                content_digest: material
                    .content_digest
                    .ok_or(NativeHookDecodeError::MissingOpaqueMaterial)?,
                changed_range_count: material.changed_range_count,
            },
        };
        let envelope = HookEventEnvelopeV2 {
            schema_version: HOOK_EVENT_SCHEMA_VERSION,
            event_id: material.event_id,
            producer: self.host,
            protected_session_id: material.protected_session_id,
            project_id: binding.project_id,
            repository_id: binding.repository_id,
            worktree_id: binding.worktree_id,
            worktree_epoch: binding.worktree_epoch,
            authorization_epoch: binding.authorization_epoch,
            capability_revision: binding.capability_revision,
            binding_token: binding.binding_token,
            ordering: self.ordering,
            observed_at: material.observed_at,
            event,
            payload_digest: material.payload_digest,
        };
        envelope
            .validate(binding)
            .map_err(NativeHookDecodeError::EnvelopeRejected)?;
        Ok(envelope)
    }
}

/// Opaque material that a binding-aware host adapter may attach after native
/// decoding. It never accepts a provider's raw ID, source, path, or payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeEnvelopeMaterialV1 {
    pub event_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub payload_digest: [u8; 32],
    pub observed_at: UtcMicros,
    pub tool_id: Option<[u8; 16]>,
    pub effect_receipt_id: Option<[u8; 16]>,
    pub file_id: Option<[u8; 16]>,
    pub content_digest: Option<[u8; 32]>,
    pub changed_range_count: u8,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeHookDecodeError {
    #[error("native hook payload exceeds the Hook V2 bound")]
    PayloadTooLarge,
    #[error("native hook payload is malformed")]
    MalformedPayload,
    #[error("native hook event is not a checked-in supported event")]
    UnsupportedNativeEvent,
    #[error("native hook event is missing a required typed identity")]
    MissingTypedIdentity,
    #[error("native hook family is not supported natively by this host")]
    UnsupportedNativeFamily,
    #[error("decoded event host does not match the daemon binding")]
    BindingHostMismatch,
    #[error("opaque admission material is missing for the decoded event")]
    MissingOpaqueMaterial,
    #[error("the completed envelope does not satisfy the Hook V2 contract")]
    EnvelopeRejected(HookContractError),
}

/// Decode one provider-native checked-in event shape. Unsupported names are
/// rejected rather than inferred from command text or another provider.
pub fn decode_native_hook_event(
    host: HookHostV1,
    payload: &[u8],
) -> Result<DecodedNativeHookEventV1, NativeHookDecodeError> {
    if payload.len() > MAX_HOOK_PAYLOAD_BYTES {
        return Err(NativeHookDecodeError::PayloadTooLarge);
    }
    let raw: Value =
        serde_json::from_slice(payload).map_err(|_| NativeHookDecodeError::MalformedPayload)?;
    if !raw.is_object() {
        return Err(NativeHookDecodeError::MalformedPayload);
    }
    let signal = match host {
        HookHostV1::ClaudeCode => decode_claude(&raw)?,
        HookHostV1::Codex => decode_codex(&raw)?,
        HookHostV1::CursorDesktop | HookHostV1::CursorCloud => decode_cursor(&raw)?,
        HookHostV1::Hermes => decode_hermes(&raw)?,
        HookHostV1::Kiro => decode_kiro(&raw)?,
    };
    if stock_event_support(host, signal.family()) != HookEventSupportV1::Native {
        return Err(NativeHookDecodeError::UnsupportedNativeFamily);
    }
    Ok(DecodedNativeHookEventV1 {
        host,
        signal,
        ordering: native_ordering(&raw)?,
    })
}

/// Decode one checked-in provider-native event and immediately bind it to a
/// daemon-published exact scope. This is the only convenience path that turns
/// native bytes into a transport envelope; it still discards every raw host
/// field before binding and cannot infer a host/project/worktree identity.
pub fn decode_bound_native_hook_event(
    host: HookHostV1,
    payload: &[u8],
    binding: &HookScopeBindingV1,
    material: NativeEnvelopeMaterialV1,
) -> Result<HookEventEnvelopeV2, NativeHookDecodeError> {
    decode_native_hook_event(host, payload)?.into_envelope(binding, material)
}

fn decode_claude(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "SessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_codex(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "SessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_cursor(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "sessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_hermes(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "event")? {
        "turnIngested" => Ok(NativeHookSignalV1::SessionBoundary(
            HookBoundaryV1::TurnComplete,
        )),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_kiro(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "userPromptSubmit" => Ok(NativeHookSignalV1::PromptBoundary),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn event_name<'a>(raw: &'a Value, key: &str) -> Result<&'a str, NativeHookDecodeError> {
    raw.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(NativeHookDecodeError::MalformedPayload)
}

fn native_ordering(raw: &Value) -> Result<HookOrderingV1, NativeHookDecodeError> {
    let sequence = raw.get("event_sequence").or_else(|| raw.get("sequence"));
    match sequence {
        None | Some(Value::Null) => Ok(HookOrderingV1::Unknown),
        Some(value) => value
            .as_u64()
            .filter(|sequence| *sequence > 0)
            .map(HookOrderingV1::ProviderSequence)
            .ok_or(NativeHookDecodeError::MalformedPayload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Fixture {
        schema_version: u16,
        provider: String,
        cases: Vec<FixtureCase>,
    }

    #[derive(Deserialize)]
    struct FixtureCase {
        state: String,
        request: Value,
    }

    const FIXTURES: &[(HookHostV1, HookEventFamily, &str, &str)] = &[
        (
            HookHostV1::ClaudeCode,
            HookEventFamily::SessionBoundary,
            "claude",
            include_str!("../../../tests/fixtures/host_events/claude/baseline.json"),
        ),
        (
            HookHostV1::Codex,
            HookEventFamily::SessionBoundary,
            "codex",
            include_str!("../../../tests/fixtures/host_events/codex/baseline.json"),
        ),
        (
            HookHostV1::CursorDesktop,
            HookEventFamily::SessionBoundary,
            "cursor",
            include_str!("../../../tests/fixtures/host_events/cursor/baseline.json"),
        ),
        (
            HookHostV1::Hermes,
            HookEventFamily::SessionBoundary,
            "hermes",
            include_str!("../../../tests/fixtures/host_events/hermes/baseline.json"),
        ),
        (
            HookHostV1::Kiro,
            HookEventFamily::PromptBoundary,
            "kiro",
            include_str!("../../../tests/fixtures/host_events/kiro/baseline.json"),
        ),
    ];

    #[test]
    fn checked_in_native_fixtures_have_family_shadow_parity_without_payload_leakage() {
        for (host, expected_family, expected_provider, bytes) in FIXTURES {
            let fixture: Fixture = serde_json::from_str(bytes).unwrap();
            assert_eq!(fixture.schema_version, 1);
            assert_eq!(&fixture.provider, expected_provider);
            let mut request = fixture
                .cases
                .into_iter()
                .find(|case| case.state == "supported")
                .expect("authoritative fixture has a supported native case")
                .request;
            request["privacy_canary"] =
                Value::String("TOP_SECRET /private/workspace raw-tool-argument".to_owned());
            let decoded = decode_native_hook_event(*host, request.to_string().as_bytes()).unwrap();
            assert_eq!(decoded.family(), *expected_family);
            assert_eq!(
                stock_event_support(*host, decoded.family()),
                HookEventSupportV1::Native
            );
            assert_eq!(decoded.ordering, HookOrderingV1::Unknown);
            let rendered = serde_json::to_string(&decoded).unwrap();
            for private in ["TOP_SECRET", "/private/workspace", "raw-tool-argument"] {
                assert!(
                    !rendered.contains(private),
                    "fixture leaked {private}: {rendered}"
                );
            }
        }
    }

    #[test]
    fn kiro_tool_events_are_rejected_instead_of_emulated() {
        let raw = br#"{"hook_event_name":"preToolUse","tool_name":"fsWrite"}"#;
        assert_eq!(
            decode_native_hook_event(HookHostV1::Kiro, raw),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        );
    }

    #[test]
    fn bound_decoder_requires_daemon_identity_and_drops_native_payload_fields() {
        let binding = HookScopeBindingV1 {
            host: HookHostV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 1,
            authorization_epoch: 1,
            capability_revision: 1,
            binding_token: [4; 32],
            capabilities: vec![crate::HookCapabilityV1 {
                family: HookEventFamily::SessionBoundary,
                support: HookEventSupportV1::Native,
            }],
        };
        let envelope = decode_bound_native_hook_event(
            HookHostV1::ClaudeCode,
            br#"{"hook_event_name":"SessionStart","cwd":"/private/path","secret":"do-not-retain"}"#,
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [5; 16],
                protected_session_id: [6; 32],
                payload_digest: [7; 32],
                observed_at: UtcMicros(1),
                tool_id: None,
                effect_receipt_id: None,
                file_id: None,
                content_digest: None,
                changed_range_count: 0,
            },
        )
        .unwrap();
        let rendered = serde_json::to_string(&envelope).unwrap();
        assert!(!rendered.contains("/private/path"));
        assert!(!rendered.contains("do-not-retain"));
        assert_eq!(envelope.producer, HookHostV1::ClaudeCode);
    }
}
