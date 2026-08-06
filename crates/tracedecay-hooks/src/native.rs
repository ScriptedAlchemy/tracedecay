//! Provider-native Hook V2 decoding.
//!
//! These adapters only recognize checked-in native event names and preserve
//! their event-family provenance. They deliberately discard prompts, paths,
//! tool arguments, output, and provider identifiers; opaque IDs are supplied
//! later by the daemon-issued binding/material contract.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::{NativeHostIdentityV1, UtcMicros};

use crate::{
    HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1, HookContractError, HookEventEnvelopeV2,
    HookEventFamily, HookEventSupportV1, HookEventV2, HookLifecyclePhaseV1, HookOrderingV1,
    HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES, stock_event_support,
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

/// Content-free result of decoding OpenCode's native project-scoped LSP event.
///
/// `lsp.updated` has no session identity and therefore must not be coerced into
/// the session-scoped Hook V2 envelope. The daemon ingests it through the
/// project-scoped host-event path instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecodedOpenCodeLspEventV1 {
    pub ordering: HookOrderingV1,
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
    #[error("native hook payload exceeds the Hook V2 bound")]
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
        NativeHostIdentityV1::Cline
        | NativeHostIdentityV1::RooCode
        | NativeHostIdentityV1::Kilo => {
            return Err(NativeHookDecodeError::UnsupportedNativeEvent);
        }
    };
    finish_decoded_native_event(host, signal, &raw)
}

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

pub fn decode_opencode_lsp_event(
    payload: &[u8],
) -> Result<DecodedOpenCodeLspEventV1, NativeHookDecodeError> {
    let raw = parse_native_payload(payload)?;
    if event_name(&raw, "type")? != "lsp.updated" {
        return Err(NativeHookDecodeError::UnsupportedNativeEvent);
    }
    let event = decode_shape::<OpenCodeLspUpdatedEvent>(&raw)?;
    if event.id.is_empty() {
        return Err(NativeHookDecodeError::MissingTypedIdentity);
    }
    Ok(DecodedOpenCodeLspEventV1 {
        ordering: native_ordering(&raw)?,
    })
}

fn parse_native_payload(payload: &[u8]) -> Result<Value, NativeHookDecodeError> {
    const MAX_NATIVE_DEPTH: usize = 32;
    const MAX_NATIVE_VALUES: usize = 2_048;

    if payload.len() > MAX_HOOK_PAYLOAD_BYTES {
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
// forward-compatible metadata. Every field consumed for identity or routing is
// strongly typed below, so wrong types fail without retaining raw payloads.
#[allow(dead_code)]
#[derive(Deserialize)]
struct ClaudePostToolUseEvent {
    session_id: String,
    transcript_path: String,
    cwd: String,
    prompt_id: String,
    permission_mode: String,
    tool_name: String,
    tool_input: Value,
    tool_response: Value,
    tool_use_id: String,
    duration_ms: u64,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct ClaudeStopEvent {
    session_id: String,
    transcript_path: String,
    cwd: String,
    prompt_id: String,
    permission_mode: String,
    stop_hook_active: bool,
    last_assistant_message: String,
    background_tasks: Vec<Value>,
    session_crons: Vec<Value>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CodexStopEvent {
    session_id: String,
    turn_id: String,
    transcript_path: Option<String>,
    cwd: String,
    model: String,
    permission_mode: String,
    stop_hook_active: bool,
    last_assistant_message: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CodexPostToolUseEvent {
    session_id: String,
    turn_id: String,
    cwd: String,
    tool_name: String,
    tool_use_id: String,
    tool_input: Value,
    tool_response: Value,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CursorAfterFileEditEvent {
    conversation_id: String,
    generation_id: String,
    model: String,
    file_path: String,
    edits: Vec<CursorEdit>,
    session_id: String,
    cursor_version: String,
    workspace_roots: Vec<String>,
    user_email: Option<String>,
    transcript_path: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CursorEdit {
    old_string: String,
    new_string: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CursorStopEvent {
    conversation_id: String,
    generation_id: String,
    model: String,
    status: String,
    loop_count: u64,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesWriteEvent {
    cwd: String,
    extra: HermesToolExtra,
    session_id: String,
    tool_input: Value,
    tool_name: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesToolExtra {
    status: String,
    tool_call_id: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesTerminalReceiptEvent {
    agent: String,
    event: String,
    route: HermesTerminalReceiptRoute,
    receipt: HermesTerminalReceipt,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesTerminalReceiptRoute {
    session_id: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesTerminalReceipt {
    tool_call_id: String,
    status: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesSessionEndEvent {
    cwd: String,
    extra: HermesSessionEndExtra,
    session_id: String,
    tool_input: Option<Value>,
    tool_name: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct HermesSessionEndExtra {
    completed: bool,
    interrupted: bool,
    model: String,
    platform: String,
    task_id: String,
    telemetry_schema_version: String,
    turn_id: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct KimiPostToolUseEvent {
    session_id: String,
    cwd: String,
    tool_name: String,
    tool_input: serde_json::Map<String, Value>,
    tool_call_id: String,
    tool_output: Value,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct KimiStopEvent {
    session_id: String,
    cwd: String,
    stop_hook_active: bool,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeEventProperties {
    file: Option<String>,
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    status: Option<OpenCodeSessionStatus>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeSessionStatus {
    #[serde(rename = "type")]
    kind: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeBusEvent {
    id: String,
    properties: OpenCodeEventProperties,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeLspUpdatedEvent {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    properties: OpenCodeLspUpdatedProperties,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeLspUpdatedProperties {}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeToolAfterEvent {
    input: OpenCodeToolAfterInput,
    output: OpenCodeToolAfterOutput,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeToolAfterInput {
    tool: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "callID")]
    call_id: String,
    args: Value,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct OpenCodeToolAfterOutput {
    title: String,
    output: String,
    metadata: Value,
}

fn decode_shape<T: DeserializeOwned>(raw: &Value) -> Result<T, NativeHookDecodeError> {
    serde_json::from_value(raw.clone()).map_err(|_| NativeHookDecodeError::MalformedPayload)
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

fn decode_opencode_event(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    let event = decode_shape::<OpenCodeBusEvent>(raw)?;
    match event_name(raw, "type")? {
        "file.edited" => {
            event
                .properties
                .file
                .filter(|file| !file.is_empty())
                .ok_or(NativeHookDecodeError::MalformedPayload)?;
            Ok(NativeHookSignalV1::SavedEdit)
        }
        "session.idle" => {
            event
                .properties
                .session_id
                .filter(|session| !session.is_empty())
                .ok_or(NativeHookDecodeError::MalformedPayload)?;
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        "session.status" => {
            event
                .properties
                .session_id
                .filter(|session| !session.is_empty())
                .ok_or(NativeHookDecodeError::MalformedPayload)?;
            if event.properties.status.map(|status| status.kind).as_deref() != Some("idle") {
                return Err(NativeHookDecodeError::UnsupportedNativeEvent);
            }
            Ok(NativeHookSignalV1::SessionBoundary(
                HookBoundaryV1::TurnComplete,
            ))
        }
        _ => Err(NativeHookDecodeError::UnsupportedNativeEvent),
    }
}

fn decode_opencode_tool_after(raw: &Value) -> Result<NativeHookSignalV1, NativeHookDecodeError> {
    let event = decode_shape::<OpenCodeToolAfterEvent>(raw)?;
    Ok(
        if matches!(event.input.tool.as_str(), "apply_patch" | "edit" | "write") {
            NativeHookSignalV1::SavedEdit
        } else {
            NativeHookSignalV1::ToolLifecycle(HookLifecyclePhaseV1::Completed)
        },
    )
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

    fn fixture_request(document: &str, identity: &str) -> Vec<u8> {
        let document: serde_json::Value = serde_json::from_str(document).unwrap();
        let event = document["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["identity"].as_str() == Some(identity))
            .unwrap();
        serde_json::to_vec(&event["request"]).unwrap()
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
        ];

        for (host, payload, signal) in captures {
            assert_eq!(
                decode_native_hook_event(host, payload).unwrap().signal,
                signal
            );
        }

        let opencode = include_str!("../fixtures/host_events/opencode/baseline.json");
        for (identity, signal) in [
            ("saved_edit", NativeHookSignalV1::SavedEdit),
            (
                "idle_status",
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
            (
                "stop",
                NativeHookSignalV1::SessionBoundary(HookBoundaryV1::TurnComplete),
            ),
        ] {
            assert_eq!(
                decode_native_hook_event(
                    NativeHostIdentityV1::OpenCode,
                    fixture_request(opencode, identity).as_slice()
                )
                .unwrap()
                .signal,
                signal
            );
        }
        assert_eq!(
            decode_opencode_plugin_event(
                OpenCodePluginSurfaceV1::ToolExecuteAfter,
                fixture_request(opencode, "post_tool_use").as_slice(),
            )
            .unwrap()
            .signal,
            NativeHookSignalV1::SavedEdit
        );
        assert_eq!(
            decode_opencode_lsp_event(fixture_request(opencode, "lsp_updated").as_slice(),)
                .unwrap()
                .ordering,
            HookOrderingV1::Unknown
        );
    }

    #[test]
    fn kimi_and_opencode_reject_deep_or_oversized_payloads_before_typed_decode() {
        for (host, discriminator) in [
            (
                NativeHostIdentityV1::KimiCode,
                r#""hook_event_name":"Stop""#,
            ),
            (NativeHostIdentityV1::OpenCode, r#""type":"session.idle""#),
        ] {
            let nested = format!("{}null{}", "[".repeat(33), "]".repeat(33));
            let deep = format!(r#"{{{discriminator},"nested":{nested}}}"#);
            assert_eq!(
                decode_native_hook_event(host, deep.as_bytes()),
                Err(NativeHookDecodeError::StructureLimit)
            );

            let oversized = vec![b' '; MAX_HOOK_PAYLOAD_BYTES + 1];
            assert_eq!(
                decode_native_hook_event(host, &oversized),
                Err(NativeHookDecodeError::PayloadTooLarge)
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
            ),
            Err(NativeHookDecodeError::UnsupportedNativeFamily)
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
    fn authentic_cursor_saved_edit_capture_is_typed() {
        assert!(matches!(
            decode_native_hook_event(
                NativeHostIdentityV1::CursorDesktop,
                include_bytes!("../fixtures/host_events/cursor/after-file-edit.json"),
            ),
            Ok(DecodedNativeHookEventV1 {
                signal: NativeHookSignalV1::SavedEdit,
                ..
            })
        ));
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
