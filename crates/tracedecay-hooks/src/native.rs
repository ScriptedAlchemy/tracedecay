//! Provider-native Hook V2 decoding.
//!
//! These adapters only recognize checked-in native event names and preserve
//! their event-family provenance. They deliberately discard prompts, paths,
//! tool arguments, output, and provider identifiers; opaque IDs are supplied
//! later by the daemon-issued binding/material contract.

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{NativeHostIdentityV1, ObservationId, SessionId, UtcMicros};
use tracedecay_framing::MAX_WIRE_MESSAGE_BYTES;

use crate::{
    HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1, HookContractError, HookEventEnvelopeV2,
    HookEventFamily, HookEventSupportV1, HookEventV2, HookLifecyclePhaseV1, HookOrderingV1,
    HookScopeBindingV1, stock_event_support,
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

/// OpenCode's event bus and direct tool hook are distinct native plugin
/// surfaces. The caller selects the callback it received; no synthetic
/// discriminator is inserted into provider payload bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenCodePluginSurfaceV1 {
    Event,
    ToolExecuteAfter,
}

/// Provider-native identity retained with a replayable lifecycle event.
///
/// The session and call identifiers come directly from the host callback, and
/// `event_id` binds them to the exact content-free envelope stored beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContextScoutLifecycleV1 {
    pub session_id: SessionId,
    pub call_id: ObservationId,
    pub event_id: [u8; 16],
}

impl NativeContextScoutLifecycleV1 {
    pub fn new(session_id: &str, call_id: &str, event_id: [u8; 16]) -> Option<Self> {
        Some(Self {
            session_id: SessionId::new(session_id.to_owned()).ok()?,
            call_id: ObservationId::new(call_id.to_owned()).ok()?,
            event_id,
        })
    }

    pub fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        matches!(
            envelope.producer,
            tracedecay_domain::NativeHostIdentityV1::KimiCode
                | tracedecay_domain::NativeHostIdentityV1::OpenCode
        ) && <[u8; 32]>::from(Sha256::digest(self.session_id.as_str().as_bytes()))
            == envelope.protected_session_id
            && self.event_id == envelope.event_id
            && matches!(
                envelope.event,
                HookEventV2::SavedEdit { .. } | HookEventV2::ToolLifecycle { .. }
            )
    }
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
#[serde(deny_unknown_fields)]
pub struct DecodedNativeHookEventV1 {
    pub host: NativeHostIdentityV1,
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
            binding_token: binding.binding_token,
            ordering: self.ordering,
            observed_at: material.observed_at,
            event,
        };
        envelope
            .validate(binding)
            .map_err(NativeHookDecodeError::EnvelopeRejected)?;
        Ok(envelope)
    }
}

/// Opaque material that a binding-aware host adapter may attach after native
/// decoding. It never accepts a provider's raw ID, source, path, or payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEnvelopeMaterialV1 {
    pub event_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub observed_at: UtcMicros,
    pub tool_id: Option<[u8; 16]>,
    pub effect_receipt_id: Option<[u8; 16]>,
    pub file_id: Option<[u8; 16]>,
    pub changed_range_count: u8,
}

/// Content-free native material submitted by a projectless host hook.
///
/// The hook has no project route, so it cannot read a project binding or
/// decide a fallback action. The daemon reconstructs the profile-scoped V2
/// envelope from its authenticated profile identity before accepting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileScopedNativeHookAdmissionV1 {
    pub decoded: DecodedNativeHookEventV1,
    pub material: NativeEnvelopeMaterialV1,
}

impl ProfileScopedNativeHookAdmissionV1 {
    pub fn into_envelope(
        self,
        binding: &HookScopeBindingV1,
    ) -> Result<HookEventEnvelopeV2, NativeHookDecodeError> {
        self.decoded.into_envelope(binding, self.material)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeHookDecodeError {
    #[error("native hook payload exceeds the host wire bound")]
    PayloadTooLarge,
    #[error("native hook payload is malformed")]
    MalformedPayload,
    #[error("native hook payload exceeds structural limits")]
    StructureLimit,
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
/// Per-payload decode fan-out. Every native hook byte stream a host receives
/// passes through here once, so this is the boundary that reflects decode
/// cost across all provider shapes without measuring each `decode_*` helper.
#[tracing::instrument(name = "hooks.native.decode_event", level = "trace", skip_all)]
pub fn decode_native_hook_event(
    host: NativeHostIdentityV1,
    payload: &[u8],
) -> Result<DecodedNativeHookEventV1, NativeHookDecodeError> {
    let raw = parse_native_payload(payload)?;
    let signal = match host {
        NativeHostIdentityV1::ClaudeCode => decode_claude(&raw)?,
        NativeHostIdentityV1::Codex => decode_codex(&raw)?,
        NativeHostIdentityV1::CursorDesktop | NativeHostIdentityV1::CursorCloud => {
            decode_cursor(&raw)?
        }
        NativeHostIdentityV1::Hermes => decode_hermes(&raw)?,
        NativeHostIdentityV1::Kiro => decode_kiro(&raw)?,
        NativeHostIdentityV1::KimiCode => decode_kimi(&raw)?,
        NativeHostIdentityV1::OpenCode => decode_opencode_event(&raw)?,
        NativeHostIdentityV1::Pi => decode_pi(&raw)?,
        NativeHostIdentityV1::FactoryDroid => decode_droid(&raw)?,
        NativeHostIdentityV1::Cline
        | NativeHostIdentityV1::RooCode
        | NativeHostIdentityV1::Kilo => {
            return Err(NativeHookDecodeError::UnsupportedNativeEvent);
        }
    };
    finish_decoded_native_event(host, signal, &raw)
}

/// OpenCode plugin callbacks enter here directly instead of through
/// [`decode_native_hook_event`], so this surface needs its own decode
/// boundary to stay visible.
#[tracing::instrument(name = "hooks.native.decode_plugin_event", level = "trace", skip_all)]
pub fn decode_opencode_plugin_event(
    surface: OpenCodePluginSurfaceV1,
    payload: &[u8],
) -> Result<DecodedNativeHookEventV1, NativeHookDecodeError> {
    let raw = parse_native_payload(payload)?;
    let signal = match surface {
        OpenCodePluginSurfaceV1::Event => decode_opencode_event(&raw)?,
        OpenCodePluginSurfaceV1::ToolExecuteAfter => decode_opencode_tool_after(&raw)?,
    };
    finish_decoded_native_event(NativeHostIdentityV1::OpenCode, signal, &raw)
}

fn parse_native_payload(payload: &[u8]) -> Result<Value, NativeHookDecodeError> {
    const MAX_NATIVE_DEPTH: usize = 32;
    const MAX_NATIVE_VALUES: usize = 2_048;

    // Decoders keep only typed identity, never host content, so the raw
    // payload is bounded by the host wire authority rather than by the
    // content-free envelope a spool record holds.
    if payload.len() > MAX_WIRE_MESSAGE_BYTES {
        return Err(NativeHookDecodeError::PayloadTooLarge);
    }
    let raw: Value =
        serde_json::from_slice(payload).map_err(|_| NativeHookDecodeError::MalformedPayload)?;
    if !raw.is_object() {
        return Err(NativeHookDecodeError::MalformedPayload);
    }
    let mut values = 0usize;
    let mut pending = vec![(&raw, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        values = values.saturating_add(1);
        if values > MAX_NATIVE_VALUES || depth > MAX_NATIVE_DEPTH {
            return Err(NativeHookDecodeError::StructureLimit);
        }
        match value {
            Value::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth.saturating_add(1))));
            }
            Value::Object(fields) => {
                pending.extend(
                    fields
                        .values()
                        .map(|field| (field, depth.saturating_add(1))),
                );
            }
            _ => {}
        }
    }
    Ok(raw)
}

fn finish_decoded_native_event(
    host: NativeHostIdentityV1,
    signal: NativeHookSignalV1,
    raw: &Value,
) -> Result<DecodedNativeHookEventV1, NativeHookDecodeError> {
    if stock_event_support(host, signal.family()) != HookEventSupportV1::Native {
        return Err(NativeHookDecodeError::UnsupportedNativeFamily);
    }
    Ok(DecodedNativeHookEventV1 {
        host,
        signal,
        ordering: native_ordering(raw)?,
    })
}

/// Decode one checked-in provider-native event and immediately bind it to a
/// daemon-published exact scope. This is the only convenience path that turns
/// native bytes into a transport envelope; it still discards every raw host
/// field before binding and cannot infer a host/project/worktree identity.
pub fn decode_bound_native_hook_event(
    host: NativeHostIdentityV1,
    payload: &[u8],
    binding: &HookScopeBindingV1,
    material: NativeEnvelopeMaterialV1,
) -> Result<HookEventEnvelopeV2, NativeHookDecodeError> {
    decode_native_hook_event(host, payload)?.into_envelope(binding, material)
}

// Provider schemas intentionally allow unknown fields: documented hosts add
// forward-compatible metadata. Fields consumed for identity or routing are
// strongly typed below so wrong types fail; fields the decoders check only
// for documented presence deserialize as underscore-named [`IgnoredAny`] so
// a matched event keeps its checked-in shape without rematerializing the
// payload's prompts, paths, tool arguments, or output.
#[derive(Deserialize)]
struct ClaudePostToolUseEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "transcript_path")]
    _transcript_path: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "prompt_id")]
    _prompt_id: IgnoredAny,
    #[serde(rename = "permission_mode")]
    _permission_mode: IgnoredAny,
    tool_name: String,
    #[serde(rename = "tool_input")]
    _tool_input: IgnoredAny,
    #[serde(rename = "tool_response")]
    _tool_response: IgnoredAny,
    tool_use_id: String,
    #[serde(rename = "duration_ms")]
    _duration_ms: IgnoredAny,
}

#[derive(Deserialize)]
struct ClaudeStopEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "transcript_path")]
    _transcript_path: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "prompt_id")]
    _prompt_id: IgnoredAny,
    #[serde(rename = "permission_mode")]
    _permission_mode: IgnoredAny,
    #[serde(rename = "stop_hook_active")]
    _stop_hook_active: IgnoredAny,
    #[serde(rename = "last_assistant_message")]
    _last_assistant_message: IgnoredAny,
    #[serde(rename = "background_tasks")]
    _background_tasks: IgnoredAny,
    #[serde(rename = "session_crons")]
    _session_crons: IgnoredAny,
}

#[derive(Deserialize)]
struct CodexStopEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "turn_id")]
    _turn_id: IgnoredAny,
    #[serde(rename = "transcript_path")]
    _transcript_path: Option<IgnoredAny>,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "model")]
    _model: IgnoredAny,
    #[serde(rename = "permission_mode")]
    _permission_mode: IgnoredAny,
    #[serde(rename = "stop_hook_active")]
    _stop_hook_active: IgnoredAny,
    #[serde(rename = "last_assistant_message")]
    _last_assistant_message: IgnoredAny,
}

#[derive(Deserialize)]
struct CodexPostToolUseEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "turn_id")]
    _turn_id: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    tool_name: String,
    tool_use_id: String,
    #[serde(rename = "tool_input")]
    _tool_input: IgnoredAny,
    #[serde(rename = "tool_response")]
    _tool_response: IgnoredAny,
}

#[derive(Deserialize)]
struct CursorAfterFileEditEvent {
    #[serde(rename = "conversation_id")]
    _conversation_id: IgnoredAny,
    #[serde(rename = "generation_id")]
    _generation_id: IgnoredAny,
    #[serde(rename = "model")]
    _model: IgnoredAny,
    #[serde(rename = "file_path")]
    _file_path: IgnoredAny,
    edits: Vec<CursorEdit>,
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "cursor_version")]
    _cursor_version: IgnoredAny,
    workspace_roots: Vec<IgnoredAny>,
    #[serde(rename = "user_email")]
    _user_email: Option<IgnoredAny>,
    #[serde(rename = "transcript_path")]
    _transcript_path: IgnoredAny,
}

#[derive(Deserialize)]
struct CursorEdit {
    #[serde(rename = "old_string")]
    _old_string: IgnoredAny,
    #[serde(rename = "new_string")]
    _new_string: IgnoredAny,
}

#[derive(Deserialize)]
struct CursorStopEvent {
    #[serde(rename = "conversation_id")]
    _conversation_id: IgnoredAny,
    #[serde(rename = "generation_id")]
    _generation_id: IgnoredAny,
    #[serde(rename = "model")]
    _model: IgnoredAny,
    status: String,
    #[serde(rename = "loop_count")]
    _loop_count: IgnoredAny,
}

#[derive(Deserialize)]
struct HermesWriteEvent {
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    extra: HermesToolExtra,
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "tool_input")]
    _tool_input: IgnoredAny,
    tool_name: String,
}

#[derive(Deserialize)]
struct HermesToolExtra {
    status: String,
    tool_call_id: String,
}

#[derive(Deserialize)]
struct HermesTerminalReceiptEvent {
    agent: String,
    #[serde(rename = "event")]
    _event: IgnoredAny,
    route: HermesTerminalReceiptRoute,
    receipt: HermesTerminalReceipt,
}

#[derive(Deserialize)]
struct HermesTerminalReceiptRoute {
    session_id: String,
}

#[derive(Deserialize)]
struct HermesTerminalReceipt {
    tool_call_id: String,
    status: String,
}

#[derive(Deserialize)]
struct HermesSessionEndEvent {
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "extra")]
    _extra: HermesSessionEndExtra,
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    tool_input: Option<IgnoredAny>,
    tool_name: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct HermesSessionEndExtra {
    #[serde(rename = "completed")]
    _completed: IgnoredAny,
    #[serde(rename = "interrupted")]
    _interrupted: IgnoredAny,
    #[serde(rename = "model")]
    _model: IgnoredAny,
    #[serde(rename = "platform")]
    _platform: IgnoredAny,
    #[serde(rename = "task_id")]
    _task_id: IgnoredAny,
    #[serde(rename = "telemetry_schema_version")]
    _telemetry_schema_version: IgnoredAny,
    #[serde(rename = "turn_id")]
    _turn_id: IgnoredAny,
}

#[derive(Deserialize)]
struct KimiPostToolUseEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    tool_name: String,
    #[serde(rename = "tool_input")]
    _tool_input: IgnoredAny,
    #[serde(rename = "tool_call_id")]
    _tool_call_id: IgnoredAny,
    #[serde(rename = "tool_output")]
    _tool_output: IgnoredAny,
}

#[derive(Deserialize)]
struct KimiStopEvent {
    #[serde(rename = "session_id")]
    _session_id: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "stop_hook_active")]
    _stop_hook_active: IgnoredAny,
}

/// The lifecycle payload the TraceDecay Pi extension writes for Pi's
/// in-process `session_start` and `agent_end` events.
#[derive(Deserialize)]
struct PiLifecycleEvent {
    id: String,
    session_id: String,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
}

/// The lifecycle payload Factory Droid writes for its `SessionStart` and
/// `Stop` hooks (`~/.factory/hooks.json` commands receive one JSON object on
/// stdin; see fixtures/host_events/droid.json for the captured shape).
#[derive(Deserialize)]
struct DroidLifecycleEvent {
    session_id: String,
    #[serde(rename = "transcript_path")]
    _transcript_path: IgnoredAny,
    #[serde(rename = "cwd")]
    _cwd: IgnoredAny,
    #[serde(rename = "permission_mode")]
    _permission_mode: IgnoredAny,
}

/// One event from OpenCode's V2 public stream: a typed envelope whose
/// session-scoped payload lives under `data`.
#[derive(Deserialize)]
struct OpenCodeBusEvent {
    #[serde(rename = "id")]
    _id: IgnoredAny,
    #[serde(rename = "created")]
    _created: IgnoredAny,
    data: OpenCodeEventData,
}

#[derive(Deserialize)]
struct OpenCodeEventData {
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
}

/// The V2 `execute.after` tool hook event: one object carrying the call
/// identity, the tool input, and either a completed result or an error.
#[derive(Deserialize)]
struct OpenCodeToolAfterEvent {
    tool: String,
    #[serde(rename = "sessionID")]
    _session_id: IgnoredAny,
    #[serde(rename = "id")]
    _call_id: IgnoredAny,
    #[serde(rename = "input")]
    _input: IgnoredAny,
    status: String,
}

fn decode_shape<T: DeserializeOwned>(raw: &Value) -> Result<T, NativeHookDecodeError> {
    T::deserialize(raw).map_err(|_| NativeHookDecodeError::MalformedPayload)
}

fn decode_claude(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "SessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        "PostToolUse" => {
            let event = decode_shape::<ClaudePostToolUseEvent>(raw)?;
            if event.tool_name.is_empty() || event.tool_use_id.is_empty() {
                return Err(NativeHookDecodeError::MalformedPayload);
            }
            Ok(NativeHookSignalV1::ToolLifecycle(
                HookLifecyclePhaseV1::Completed,
            ))
        }
        "Stop" => {
            decode_shape::<ClaudeStopEvent>(raw)?;
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_codex(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "SessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        "PostToolUse" => {
            let event = decode_shape::<CodexPostToolUseEvent>(raw)?;
            if event.tool_name.is_empty() || event.tool_use_id.is_empty() {
                return Err(NativeHookDecodeError::MissingTypedIdentity);
            }
            Ok(NativeHookSignalV1::ToolLifecycle(
                HookLifecyclePhaseV1::Completed,
            ))
        }
        "Stop" => {
            decode_shape::<CodexStopEvent>(raw)?;
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_cursor(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "sessionStart" => Ok(NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start)),
        "afterFileEdit" => {
            let event = decode_shape::<CursorAfterFileEditEvent>(raw)?;
            if event.edits.is_empty() || event.workspace_roots.is_empty() {
                return Err(NativeHookDecodeError::MalformedPayload);
            }
            Ok(NativeHookSignalV1::SavedEdit)
        }
        "stop" => {
            let event = decode_shape::<CursorStopEvent>(raw)?;
            if !matches!(event.status.as_str(), "completed" | "aborted" | "error") {
                return Err(NativeHookDecodeError::MalformedPayload);
            }
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_hermes(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    if let Some(event_bus_name) = raw.get("event") {
        return match event_bus_name.as_str().filter(|value| !value.is_empty()) {
            Some("turnCompleted" | "turnIngested") => Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            )),
            Some("terminalReceipt") => {
                let event = decode_shape::<HermesTerminalReceiptEvent>(raw)?;
                if event.agent != "hermes"
                    || event.route.session_id.is_empty()
                    || event.receipt.tool_call_id.is_empty()
                {
                    return Err(NativeHookDecodeError::MissingTypedIdentity);
                }
                hermes_terminal_tool_signal(&event.receipt.status)
            }
            Some(_) => Err(NativeHookDecodeError::UnsupportedNativeEvent),
            None => Err(NativeHookDecodeError::MalformedPayload),
        };
    }

    match event_name(raw, "hook_event_name")? {
        "post_tool_call" => {
            let event = decode_shape::<HermesWriteEvent>(raw)?;
            if event.tool_name.is_empty() || event.extra.tool_call_id.is_empty() {
                return Err(NativeHookDecodeError::MalformedPayload);
            }
            hermes_terminal_tool_signal(&event.extra.status)
        }
        "on_session_end" => {
            let event = decode_shape::<HermesSessionEndEvent>(raw)?;
            if event.tool_name.is_some() || event.tool_input.is_some() {
                return Err(NativeHookDecodeError::MalformedPayload);
            }
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn hermes_terminal_tool_signal(status: &str) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match status {
        "ok" | "success" | "completed" => Ok(NativeHookSignalV1::ToolLifecycle(
            HookLifecyclePhaseV1::Completed,
        )),
        "error" | "failed" => Ok(NativeHookSignalV1::ToolLifecycle(
            HookLifecyclePhaseV1::Failed,
        )),
        _ => Err(NativeHookDecodeError::MalformedPayload),
    }
}

fn decode_kiro(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "userPromptSubmit" => Ok(NativeHookSignalV1::PromptBoundary),
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_kimi(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "hook_event_name")? {
        "PostToolUse" => {
            let event = decode_shape::<KimiPostToolUseEvent>(raw)?;
            Ok(if event.tool_name == "Edit" {
                NativeHookSignalV1::SavedEdit
            } else {
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed)
            })
        }
        "Stop" => {
            decode_shape::<KimiStopEvent>(raw)?;
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_pi(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    let boundary = match event_name(raw, "hook_event_name")? {
        "session_start" => HookBoundaryV1::Start,
        "agent_end" => HookBoundaryV1::TurnComplete,
        _ => return Err(NativeHookDecodeError::UnsupportedNativeEvent),
    };
    let event = decode_shape::<PiLifecycleEvent>(raw)?;
    if event.id.is_empty() || event.session_id.is_empty() {
        return Err(NativeHookDecodeError::MissingTypedIdentity);
    }
    Ok(NativeHookSignalV1::SessionBoundary(boundary))
}

fn decode_droid(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    let boundary = match event_name(raw, "hook_event_name")? {
        "SessionStart" => HookBoundaryV1::Start,
        "Stop" => HookBoundaryV1::TurnComplete,
        _ => return Err(NativeHookDecodeError::UnsupportedNativeEvent),
    };
    let event = decode_shape::<DroidLifecycleEvent>(raw)?;
    if event.session_id.is_empty() {
        return Err(NativeHookDecodeError::MissingTypedIdentity);
    }
    Ok(NativeHookSignalV1::SessionBoundary(boundary))
}

/// The V2 stream reports a turn's end only through the durable execution
/// terminal events; `session.idle` / `session.status` are not emitted for V2
/// executions and no stream event reports an edit, so edits arrive solely via
/// [`decode_opencode_tool_after`].
fn decode_opencode_event(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    match event_name(raw, "type")? {
        "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted" => {
            decode_shape::<OpenCodeBusEvent>(raw)?
                .data
                .session_id
                .filter(|session| !session.is_empty())
                .ok_or(NativeHookDecodeError::MalformedPayload)?;
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_opencode_tool_after(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    let event = decode_shape::<OpenCodeToolAfterEvent>(raw)?;
    Ok(match event.status.as_str() {
        "completed" if matches!(event.tool.as_str(), "edit" | "write" | "patch") => {
            NativeHookSignalV1::SavedEdit
        }
        "completed" => NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
        "error" => NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Failed),
        _ => return Err(NativeHookDecodeError::MalformedPayload),
    })
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

    fn fixture_request_value(document: &str, identity: &str) -> serde_json::Value {
        let document: serde_json::Value = serde_json::from_str(document).unwrap();
        document["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["identity"].as_str() == Some(identity))
            .unwrap()["request"]
            .clone()
    }

    fn fixture_request(document: &str, identity: &str) -> Vec<u8> {
        serde_json::to_vec(&fixture_request_value(document, identity)).unwrap()
    }

    #[test]
    fn decoded_event_serialization_is_structurally_content_free() {
        let value = serde_json::to_value(DecodedNativeHookEventV1 {
            host: NativeHostIdentityV1::CursorDesktop,
            signal: NativeHookSignalV1::SavedEdit,
            ordering: HookOrderingV1::Unknown,
        })
        .unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 3);
        assert!(object.contains_key("host"));
        assert!(object.contains_key("signal"));
        assert!(object.contains_key("ordering"));
    }

    #[test]
    fn checked_in_native_captures_decode_supported_host_families() {
        let captures: Vec<(NativeHostIdentityV1, &[u8], NativeHookSignalV1)> = vec![
            (
                NativeHostIdentityV1::ClaudeCode,
                include_bytes!("../fixtures/host_events/claude/post_tool_use_write.json"),
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
            ),
            (
                NativeHostIdentityV1::ClaudeCode,
                include_bytes!("../fixtures/host_events/claude/stop.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                NativeHostIdentityV1::Codex,
                include_bytes!("../fixtures/host_events/codex/stop.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                NativeHostIdentityV1::CursorDesktop,
                include_bytes!("../fixtures/host_events/cursor/after-file-edit.json"),
                NativeHookSignalV1::SavedEdit,
            ),
            (
                NativeHostIdentityV1::Hermes,
                include_bytes!("../fixtures/host_events/hermes/saved-edit.json"),
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
            ),
            (
                NativeHostIdentityV1::Hermes,
                include_bytes!("../fixtures/host_events/hermes/terminal-receipt.json"),
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
            ),
            (
                NativeHostIdentityV1::Hermes,
                include_bytes!("../fixtures/host_events/hermes/stop.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                NativeHostIdentityV1::KimiCode,
                include_bytes!("../fixtures/host_events/kimi/post-tool-use-edit.json"),
                NativeHookSignalV1::SavedEdit,
            ),
            (
                NativeHostIdentityV1::KimiCode,
                include_bytes!("../fixtures/host_events/kimi/stop.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                NativeHostIdentityV1::Pi,
                include_bytes!("../fixtures/host_events/pi/session-start.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start),
            ),
            (
                NativeHostIdentityV1::Pi,
                include_bytes!("../fixtures/host_events/pi/agent-end.json"),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
        ];

        for (host, payload, signal) in captures {
            assert_eq!(
                decode_native_hook_event(host, payload).unwrap().signal,
                signal
            );
        }

        let opencode = include_str!("../fixtures/host_events/opencode/baseline.json");
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::OpenCode,
                fixture_request(opencode, "stop").as_slice()
            )
            .unwrap()
            .signal,
            NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete)
        );
        for (identity, signal) in [
            ("post_tool_use", NativeHookSignalV1::SavedEdit),
            (
                "tool_completed",
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
            ),
        ] {
            assert_eq!(
                decode_opencode_plugin_event(
                    OpenCodePluginSurfaceV1::ToolExecuteAfter,
                    fixture_request(opencode, identity).as_slice(),
                )
                .unwrap()
                .signal,
                signal
            );
        }
    }

    /// The V2 stream's execution start and the deprecated idle events are not
    /// turn boundaries; a completed edit whose hook reports `error` is a
    /// failed tool, not a saved edit.
    #[test]
    fn opencode_v2_shapes_reject_non_boundary_and_failed_edits() {
        let opencode = include_str!("../fixtures/host_events/opencode/baseline.json");
        let mut started = fixture_request_value(opencode, "stop");
        started["type"] = serde_json::json!("session.execution.started");
        assert!(matches!(
            decode_native_hook_event(
                NativeHostIdentityV1::OpenCode,
                serde_json::to_vec(&started).unwrap().as_slice()
            ),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        ));
        let mut unsessioned = fixture_request_value(opencode, "stop");
        unsessioned["data"] = serde_json::json!({});
        assert!(matches!(
            decode_native_hook_event(
                NativeHostIdentityV1::OpenCode,
                serde_json::to_vec(&unsessioned).unwrap().as_slice()
            ),
            Err(NativeHookDecodeError::MalformedPayload)
        ));

        let mut failed_edit = fixture_request_value(opencode, "post_tool_use");
        failed_edit["status"] = serde_json::json!("error");
        assert_eq!(
            decode_opencode_plugin_event(
                OpenCodePluginSurfaceV1::ToolExecuteAfter,
                serde_json::to_vec(&failed_edit).unwrap().as_slice(),
            )
            .unwrap()
            .signal,
            NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Failed)
        );
    }

    #[test]
    fn pi_evidence_catalog_requests_are_the_checked_in_captures() {
        let catalog = include_str!("../fixtures/host_events/pi.json");
        for (identity, capture, signal) in [
            (
                "session_start",
                &include_bytes!("../fixtures/host_events/pi/session-start.json")[..],
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::Start),
            ),
            (
                "stop",
                &include_bytes!("../fixtures/host_events/pi/agent-end.json")[..],
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
        ] {
            let request = fixture_request(catalog, identity);
            assert_eq!(
                serde_json::from_slice::<Value>(&request).unwrap(),
                serde_json::from_slice::<Value>(capture).unwrap(),
                "{identity} catalog request drifted from its capture"
            );
            assert_eq!(
                decode_native_hook_event(NativeHostIdentityV1::Pi, &request)
                    .unwrap()
                    .signal,
                signal
            );
        }
    }

    #[test]
    fn pi_lifecycle_requires_its_own_event_and_session_identity() {
        let agent_end = include_bytes!("../fixtures/host_events/pi/agent-end.json");
        let mut payload = serde_json::from_slice::<Value>(agent_end).unwrap();
        payload["session_id"] = Value::String(String::new());
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Pi,
                &serde_json::to_vec(&payload).unwrap()
            ),
            Err(NativeHookDecodeError::MissingTypedIdentity)
        );
        payload["hook_event_name"] = Value::String("tool_result".to_owned());
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Pi,
                &serde_json::to_vec(&payload).unwrap()
            ),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        );
        // Another host's event shape never decodes as a Pi boundary.
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Pi,
                include_bytes!("../fixtures/host_events/codex/stop.json")
            ),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        );
    }

    #[test]
    fn droid_lifecycle_requires_its_own_event_and_session_identity() {
        let stop = include_bytes!("../fixtures/host_events/droid/stop.json");
        let mut payload = serde_json::from_slice::<Value>(stop).unwrap();
        payload["session_id"] = Value::String(String::new());
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::FactoryDroid,
                &serde_json::to_vec(&payload).unwrap()
            ),
            Err(NativeHookDecodeError::MissingTypedIdentity)
        );
        payload["hook_event_name"] = Value::String("PreToolUse".to_owned());
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::FactoryDroid,
                &serde_json::to_vec(&payload).unwrap()
            ),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        );
        // Another host's event shape never decodes as a Droid boundary.
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::FactoryDroid,
                include_bytes!("../fixtures/host_events/pi/agent-end.json")
            ),
            Err(NativeHookDecodeError::UnsupportedNativeEvent)
        );
    }

    #[test]
    fn kimi_and_opencode_reject_deep_or_oversized_payloads_before_typed_decode() {
        for (host, discriminator) in [
            (
                NativeHostIdentityV1::KimiCode,
                r#""hook_event_name":"Stop""#,
            ),
            (
                NativeHostIdentityV1::OpenCode,
                r#""type":"session.execution.succeeded""#,
            ),
        ] {
            let nested = format!("{}null{}", "[".repeat(33), "]".repeat(33));
            let deep = format!(r#"{{{discriminator},"nested":{nested}}}"#);
            assert_eq!(
                decode_native_hook_event(host, deep.as_bytes()),
                Err(NativeHookDecodeError::StructureLimit)
            );

            let oversized = vec![b' '; MAX_WIRE_MESSAGE_BYTES + 1];
            assert_eq!(
                decode_native_hook_event(host, &oversized),
                Err(NativeHookDecodeError::PayloadTooLarge)
            );
        }
    }

    #[test]
    fn native_events_carrying_large_host_content_still_decode() {
        let content = "fn f() {}\n".repeat(4_000);
        let enlarge = |fixture: &[u8], pointers: &[&str]| {
            let mut payload = serde_json::from_slice::<Value>(fixture).unwrap();
            for pointer in pointers {
                *payload.pointer_mut(pointer).unwrap() = Value::String(content.clone());
            }
            serde_json::to_vec(&payload).unwrap()
        };
        let cases = [
            (
                NativeHostIdentityV1::ClaudeCode,
                enlarge(
                    include_bytes!("../fixtures/host_events/claude/post_tool_use_write.json"),
                    &["/tool_input/content", "/tool_response/content"],
                ),
                NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed),
            ),
            (
                NativeHostIdentityV1::Codex,
                enlarge(
                    include_bytes!("../fixtures/host_events/codex/stop.json"),
                    &["/last_assistant_message"],
                ),
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                NativeHostIdentityV1::CursorDesktop,
                enlarge(
                    include_bytes!("../fixtures/host_events/cursor/after-file-edit.json"),
                    &["/edits/0/new_string"],
                ),
                NativeHookSignalV1::SavedEdit,
            ),
        ];
        for (host, payload, signal) in cases {
            assert!(payload.len() > 40_000, "{host:?} payload lost its content");
            assert_eq!(
                decode_native_hook_event(host, &payload).map(|decoded| decoded.signal),
                Ok(signal),
                "{host:?}"
            );
        }
    }

    #[test]
    fn hermes_hook_discriminators_do_not_alias_event_bus_variants() {
        for fixture in [
            include_bytes!("../fixtures/host_events/hermes/saved-edit.json").as_slice(),
            include_bytes!("../fixtures/host_events/hermes/stop.json").as_slice(),
        ] {
            let mut payload = serde_json::from_slice::<Value>(fixture).unwrap();
            let hook_event_name = payload
                .as_object_mut()
                .unwrap()
                .remove("hook_event_name")
                .unwrap();
            payload["event"] = hook_event_name;

            assert_eq!(
                decode_native_hook_event(
                    NativeHostIdentityV1::Hermes,
                    &serde_json::to_vec(&payload).unwrap()
                ),
                Err(NativeHookDecodeError::UnsupportedNativeEvent)
            );
        }
    }

    #[test]
    fn hermes_turn_completion_and_ingestion_are_truthful_native_boundaries() {
        for event in ["turnCompleted", "turnIngested"] {
            let payload = serde_json::json!({
                "agent": "hermes",
                "event": event,
                "route": {"session_id": "session.hermes"},
                "receipt": {
                    "status": "success",
                    "transcript_watermark": "message.hermes"
                }
            });
            assert_eq!(
                decode_native_hook_event(
                    NativeHostIdentityV1::Hermes,
                    &serde_json::to_vec(&payload).unwrap(),
                )
                .unwrap()
                .signal,
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete)
            );
        }
    }

    #[test]
    fn kiro_documented_unverified_events_are_rejected_instead_of_emulated() {
        let kiro = include_str!("../fixtures/host_events/kiro.json");
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Kiro,
                fixture_request(kiro, "prompt_boundary").as_slice()
            )
            .unwrap()
            .signal,
            NativeHookSignalV1::PromptBoundary
        );
        for identity in ["saved_edit", "stop"] {
            assert_eq!(
                decode_native_hook_event(
                    NativeHostIdentityV1::Kiro,
                    fixture_request(kiro, identity).as_slice()
                ),
                Err(NativeHookDecodeError::UnsupportedNativeEvent)
            );
        }
    }

    #[test]
    fn codex_documented_post_tool_use_preserves_native_tool_lifecycle() {
        let codex = include_str!("../fixtures/host_events/codex.json");
        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Codex,
                fixture_request(codex, "saved_edit").as_slice()
            )
            .unwrap()
            .signal,
            NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed)
        );
        assert_eq!(
            stock_event_support(NativeHostIdentityV1::Codex, HookEventFamily::ToolLifecycle),
            HookEventSupportV1::Native
        );
    }

    #[test]
    fn codex_native_event_without_event_identity_is_rejected() {
        let mut payload = serde_json::from_slice::<Value>(include_bytes!(
            "../fixtures/host_events/codex/stop.json"
        ))
        .unwrap();
        assert!(
            payload
                .as_object_mut()
                .and_then(|fields| fields.remove("hook_event_name"))
                .is_some()
        );

        assert_eq!(
            decode_native_hook_event(
                NativeHostIdentityV1::Codex,
                &serde_json::to_vec(&payload).unwrap(),
            ),
            Err(NativeHookDecodeError::MalformedPayload)
        );
    }

    #[test]
    fn authentic_cursor_saved_edit_preserves_scope_and_content_identity() {
        let binding = HookScopeBindingV1 {
            host: NativeHostIdentityV1::CursorDesktop,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [5; 32],
            capabilities: vec![crate::HookCapabilityV1 {
                family: HookEventFamily::SavedEdit,
                support: HookEventSupportV1::Native,
            }],
        };
        let envelope = decode_bound_native_hook_event(
            NativeHostIdentityV1::CursorDesktop,
            include_bytes!("../fixtures/host_events/cursor/after-file-edit.json"),
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [6; 16],
                protected_session_id: [7; 32],
                observed_at: UtcMicros(8),
                tool_id: None,
                effect_receipt_id: None,
                file_id: Some([9; 16]),
                changed_range_count: 1,
            },
        )
        .unwrap();

        assert_eq!(envelope.repository_id, binding.repository_id);
        assert_eq!(envelope.worktree_id, binding.worktree_id);
        assert_eq!(envelope.worktree_epoch, binding.worktree_epoch);
        assert_eq!(
            envelope.event,
            HookEventV2::SavedEdit {
                file_id: [9; 16],
                changed_range_count: 1,
            }
        );
        let mut conflicting_scope = binding;
        conflicting_scope.worktree_epoch += 1;
        assert_eq!(
            envelope.validate(&conflicting_scope),
            Err(HookContractError::BindingMismatch)
        );
    }

    #[test]
    fn bound_decoder_requires_exact_daemon_scope() {
        let binding = HookScopeBindingV1 {
            host: NativeHostIdentityV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 1,
            binding_token: [4; 32],
            capabilities: vec![crate::HookCapabilityV1 {
                family: HookEventFamily::SessionBoundary,
                support: HookEventSupportV1::Native,
            }],
        };
        let envelope = decode_bound_native_hook_event(
            NativeHostIdentityV1::ClaudeCode,
            include_bytes!("../fixtures/host_events/claude/stop.json"),
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [5; 16],
                protected_session_id: [6; 32],
                observed_at: UtcMicros(1),
                tool_id: None,
                effect_receipt_id: None,
                file_id: None,
                changed_range_count: 0,
            },
        )
        .unwrap();
        assert_eq!(envelope.producer, NativeHostIdentityV1::ClaudeCode);
        assert_eq!(envelope.project_id, binding.project_id);
        assert_eq!(envelope.worktree_id, binding.worktree_id);
        assert_eq!(envelope.binding_token, binding.binding_token);
    }
}
