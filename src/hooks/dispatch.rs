use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ObservationId, ProjectId, SessionId, UtcMicros};
use tracedecay_hooks::{
    AsyncHookFeedbackDeliveryPortV1, HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1,
    HookConfigurationSnapshotV1, HookConfigurationSubscriberV1, HookEventEnvelopeV2,
    HookFeedbackDeliveryRouteV1, HookFeedbackDeliveryV1, HookFeedbackRollbackSwitchV1,
    HookGuidanceStateV1, HookHostV1, HookImmediateAdmissionV1, HookRuntimeControlV1,
    HookScopeBindingV1, HookSpoolConfigV1, HookSpoolError, HookSpoolV1, HookSynchronousDeadlineV1,
    HookTransportDispositionV1, NativeEnvelopeMaterialV1, NativeHookDecodeError,
    SpoolAppendOutcomeV1, admit_async_exact_scope, deliver_hook_feedback, finish_synchronous_hook,
};
#[cfg(test)]
use tracedecay_hooks::{HookImmediateAdmissionStateV1, HookScopedFeedbackV1};

use crate::agents::context_scout_v2::context_scout_delivery_receipt_id;

use super::analytics::{HookTimingSpan, elapsed_us};
#[cfg(test)]
use super::daemon_ports::{
    ContextScoutFeedbackCommitV1, DaemonContextScoutFeedbackPort, outcome_is_committed,
};
use super::daemon_ports::{
    DaemonAdmissionPort, DaemonDeliveryReceiptPort, DaemonFeedbackNoticeDeliveryPort,
    DaemonOpenCodeLspUpdatePort, now_utc,
};

pub(crate) enum HookDispatch {
    NotApplicable,
    /// The native dispatcher recognised the event but could not take ownership of it — no
    /// published binding, an unreadable store layout, or an envelope it could
    /// not decode. The disposition is still worth recording, but the event has
    /// not been admitted anywhere, so callers must fall back to their ordinary
    /// daemon notification rather than treat the event as delivered.
    Unavailable(HookTransportDispositionV1),
    Handled {
        guidance: Option<String>,
        disposition: HookTransportDispositionV1,
    },
}

impl HookDispatch {
    pub(crate) fn into_recorded_guidance(
        self,
        telemetry: &HookTimingSpan,
    ) -> Option<Option<String>> {
        match self {
            Self::NotApplicable => None,
            Self::Unavailable(disposition) => {
                telemetry.note_native_dispatch_disposition(disposition);
                None
            }
            Self::Handled {
                guidance,
                disposition,
            } => {
                telemetry.note_native_dispatch_disposition(disposition);
                Some(guidance)
            }
        }
    }
}

pub(crate) const NATIVE_HOOK_HOSTS: &[HookHostV1] = &[
    HookHostV1::ClaudeCode,
    HookHostV1::Codex,
    HookHostV1::CursorDesktop,
    HookHostV1::Hermes,
    HookHostV1::Kiro,
    HookHostV1::KimiCode,
    HookHostV1::OpenCode,
];

pub(crate) fn project_id_for_layout(layout: &crate::storage::StoreLayout) -> Option<[u8; 16]> {
    layout
        .identity
        .project_id
        .as_deref()
        .map(|project_id| domain_hash16(project_id, "project"))
}

pub(crate) fn publish_daemon_bindings(
    layout: &crate::storage::StoreLayout,
) -> crate::errors::Result<()> {
    let project_key = layout.identity.project_id.as_deref().ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "cannot publish Hook binding without typed project identity".to_owned(),
        }
    })?;
    let typed_project_id = ProjectId::new(project_key.to_owned()).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("cannot validate Hook project identity: {error}"),
        }
    })?;
    let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        &layout.project_root,
        &typed_project_id,
    )
    .map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("cannot resolve Hook repository/worktree scope: {error}"),
    })?;
    let now = now_utc();
    let revision = now.0.max(1) as u64;
    let (project_id, repository_id, worktree_id, worktree_epoch) =
        binding_identity_from_scope(&scope, revision);
    for host in NATIVE_HOOK_HOSTS {
        let capabilities = [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(*host, family),
        })
        .collect();
        let snapshot = tracedecay_hooks::HookConfigurationSnapshotV1 {
            schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision,
            published_at: now,
            expires_at: UtcMicros(now.0.saturating_add(24 * 60 * 60 * 1_000_000)),
            binding: HookScopeBindingV1 {
                host: *host,
                project_id,
                repository_id,
                worktree_id,
                worktree_epoch,
                binding_token: domain_hash32(project_key, host.hook_key()),
                capabilities,
            },
        };
        let writer = tracedecay_hooks::HookConfigurationFileWriterV1::new(
            tracedecay_hooks::hook_configuration_path(&layout.data_root, *host),
        );
        tracedecay_hooks::HookConfigurationPublisherV1::new(writer)
            .publish(snapshot)
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!(
                    "failed to publish {} Hook binding: {error}",
                    host.hook_key()
                ),
            })?;
    }
    Ok(())
}

fn binding_identity_from_scope(
    scope: &ResolvedScope,
    binding_revision: u64,
) -> ([u8; 16], [u8; 16], [u8; 16], u64) {
    (
        domain_hash16(scope.project_id.as_str(), "project"),
        domain_hash16(scope.repository_id.as_str(), "repository"),
        domain_hash16(scope.worktree_id.as_str(), "worktree"),
        binding_revision.max(1),
    )
}

pub(crate) fn project_and_worktree_locators_for_scope(
    scope: &ResolvedScope,
) -> ([u8; 16], [u8; 16]) {
    (
        domain_hash16(scope.project_id.as_str(), "project"),
        domain_hash16(scope.worktree_id.as_str(), "worktree"),
    )
}

#[derive(Default, Deserialize)]
struct NativeIdentityFields {
    id: Option<String>,
    #[serde(alias = "sessionID")]
    session_id: Option<String>,
    conversation_id: Option<String>,
    generation_id: Option<String>,
    #[serde(alias = "filePath")]
    file_path: Option<String>,
    prompt_id: Option<String>,
    turn_id: Option<String>,
    #[serde(alias = "toolUseID")]
    tool_use_id: Option<String>,
    #[serde(alias = "toolCallID")]
    tool_call_id: Option<String>,
    #[serde(alias = "callID")]
    call_id: Option<String>,
    edits: Option<Vec<serde_json::Value>>,
    properties: Option<NativeIdentityProperties>,
    input: Option<NativeIdentityInput>,
    tool_input: Option<NativeIdentityToolInput>,
    output: Option<NativeIdentityOutput>,
    extra: Option<NativeIdentityExtra>,
    route: Option<NativeIdentityRoute>,
    receipt: Option<NativeIdentityReceipt>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityProperties {
    #[serde(alias = "sessionID")]
    session_id: Option<String>,
    file: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityInput {
    #[serde(alias = "sessionID")]
    session_id: Option<String>,
    #[serde(alias = "callID")]
    call_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityToolInput {
    #[serde(alias = "filePath")]
    file_path: Option<String>,
    path: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityOutput {
    metadata: Option<NativeIdentityOutputMetadata>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityOutputMetadata {
    files: Option<Vec<NativeIdentityOutputFile>>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityOutputFile {
    #[serde(alias = "filePath")]
    file_path: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityExtra {
    tool_call_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityRoute {
    session_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityReceipt {
    tool_call_id: Option<String>,
}

/// Provider-native lifecycle identity that may cross the local hook/daemon
/// boundary. Session and call values come from checked-in host fields; the
/// event ID binds them to the exact content-free envelope admitted alongside
/// them. Paths and payloads remain unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeContextScoutLifecycleV1 {
    pub(crate) session_id: SessionId,
    pub(crate) call_id: ObservationId,
    pub(crate) event_id: [u8; 16],
}

impl NativeContextScoutLifecycleV1 {
    pub(crate) fn new(session_id: &str, call_id: &str, event_id: [u8; 16]) -> Option<Self> {
        Some(Self {
            session_id: SessionId::new(session_id.to_owned()).ok()?,
            call_id: ObservationId::new(call_id.to_owned()).ok()?,
            event_id,
        })
    }

    pub(crate) fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        matches!(
            envelope.producer,
            HookHostV1::KimiCode | HookHostV1::OpenCode
        ) && protected_session_id_for_native(self.session_id.as_str())
            == envelope.protected_session_id
            && self.event_id == envelope.event_id
            && matches!(
                envelope.event,
                tracedecay_hooks::HookEventV2::SavedEdit { .. }
                    | tracedecay_hooks::HookEventV2::ToolLifecycle { .. }
            )
    }
}

impl NativeIdentityFields {
    fn session_id(&self) -> Option<&str> {
        self.session_id
            .as_deref()
            .or(self.conversation_id.as_deref())
            .or_else(|| {
                self.properties
                    .as_ref()
                    .and_then(|properties| properties.session_id.as_deref())
            })
            .or_else(|| {
                self.input
                    .as_ref()
                    .and_then(|input| input.session_id.as_deref())
            })
            .or_else(|| {
                self.route
                    .as_ref()
                    .and_then(|route| route.session_id.as_deref())
            })
    }

    fn event_key(&self) -> Option<&str> {
        self.tool_use_id
            .as_deref()
            .or(self.tool_call_id.as_deref())
            .or(self.call_id.as_deref())
            .or_else(|| {
                self.input
                    .as_ref()
                    .and_then(|input| input.call_id.as_deref())
            })
            .or_else(|| {
                self.extra
                    .as_ref()
                    .and_then(|extra| extra.tool_call_id.as_deref())
            })
            .or_else(|| {
                self.receipt
                    .as_ref()
                    .and_then(|receipt| receipt.tool_call_id.as_deref())
            })
            .or(self.generation_id.as_deref())
            .or(self.prompt_id.as_deref())
            .or(self.turn_id.as_deref())
            .or(self.id.as_deref())
    }

    fn call_id(&self) -> Option<&str> {
        self.tool_use_id
            .as_deref()
            .or(self.tool_call_id.as_deref())
            .or(self.call_id.as_deref())
            .or_else(|| {
                self.input
                    .as_ref()
                    .and_then(|input| input.call_id.as_deref())
            })
            .or_else(|| {
                self.extra
                    .as_ref()
                    .and_then(|extra| extra.tool_call_id.as_deref())
            })
            .or_else(|| {
                self.receipt
                    .as_ref()
                    .and_then(|receipt| receipt.tool_call_id.as_deref())
            })
    }

    fn file_path(&self) -> Option<&str> {
        self.file_path
            .as_deref()
            .or_else(|| self.properties.as_ref()?.file.as_deref())
            .or_else(|| {
                let tool_input = self.tool_input.as_ref()?;
                tool_input
                    .file_path
                    .as_deref()
                    .or(tool_input.path.as_deref())
            })
            .or_else(|| {
                let files = self.output.as_ref()?.metadata.as_ref()?.files.as_ref()?;
                (files.len() == 1)
                    .then(|| files[0].file_path.as_deref())
                    .flatten()
            })
            .filter(|path| !path.is_empty())
    }
}

fn native_context_scout_lifecycle(
    host: HookHostV1,
    fields: &NativeIdentityFields,
    event_id: [u8; 16],
) -> Option<NativeContextScoutLifecycleV1> {
    matches!(host, HookHostV1::KimiCode | HookHostV1::OpenCode)
        .then(|| {
            NativeContextScoutLifecycleV1::new(fields.session_id()?, fields.call_id()?, event_id)
        })
        .flatten()
}

const HOOK_ADMISSION_ACK_BUDGET_MICROS: u64 = 25_000;

fn admission_window_after_elapsed(elapsed: u64) -> Option<(HookSynchronousDeadlineV1, Duration)> {
    let deadline = HookSynchronousDeadlineV1::after_elapsed(elapsed)?;
    let admission_remaining = HOOK_ADMISSION_ACK_BUDGET_MICROS.checked_sub(elapsed)?;
    if admission_remaining == 0 || deadline.remaining_micros() == 0 {
        return None;
    }
    Some((
        deadline,
        Duration::from_micros(admission_remaining.min(deadline.remaining_micros())),
    ))
}

pub(crate) async fn dispatch(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
    telemetry: Option<&HookTimingSpan>,
) -> HookDispatch {
    let started = Instant::now();
    let decoded = match tracedecay_hooks::decode_native_hook_event(host, event_json.as_bytes()) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => {
            return HookDispatch::NotApplicable;
        }
        Err(_) => return unavailable(),
    };
    let Some(prepared) = prepare_bound_hook(host, event_json, project_root, decoded) else {
        return unavailable();
    };
    let native_session_id = prepared.native_session_id.clone();
    let native_lifecycle = prepared.native_lifecycle.clone();
    let admission = DaemonAdmissionPort::new(
        project_root,
        native_session_id.as_deref(),
        native_lifecycle.as_ref(),
        telemetry,
    );
    let delivery = DaemonFeedbackNoticeDeliveryPort::new(project_root);
    dispatch_decoded(prepared, project_root, started, &admission, &delivery).await
}

/// Dispatches a native event through its exact project binding when one is
/// known, or through the authenticated daemon profile when the host has no
/// project identity. Both paths send only the closed event material.
pub(crate) async fn dispatch_for_scope(
    host: HookHostV1,
    event_json: &str,
    project_root: Option<&Path>,
    telemetry: Option<&HookTimingSpan>,
) -> HookDispatch {
    match project_root {
        Some(project_root) => dispatch(host, event_json, project_root, telemetry).await,
        None => dispatch_profile_scoped(host, event_json, telemetry).await,
    }
}

async fn dispatch_profile_scoped(
    host: HookHostV1,
    event_json: &str,
    telemetry: Option<&HookTimingSpan>,
) -> HookDispatch {
    let started = Instant::now();
    let decoded = match tracedecay_hooks::decode_native_hook_event(host, event_json.as_bytes()) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => return HookDispatch::NotApplicable,
        Err(_) => return unavailable(),
    };
    let Ok(fields) = serde_json::from_str::<NativeIdentityFields>(event_json) else {
        return unavailable();
    };
    let Some(material) =
        native_material(&fields, decoded.family(), event_json.as_bytes(), now_utc())
    else {
        return unavailable();
    };
    let Some((_, timeout)) = admission_window_after_elapsed(elapsed_us(started)) else {
        return unavailable();
    };
    let response = tokio::time::timeout(
        timeout,
        super::daemon_hook_action(
            None,
            serde_json::json!({
                "action": "hook_v2_profile_admit",
                "admission": tracedecay_hooks::ProfileScopedNativeHookAdmissionV1 {
                    decoded,
                    material,
                },
            }),
            telemetry,
        ),
    )
    .await;
    let Ok(Ok(response)) = response else {
        return unavailable();
    };
    let accepted = response.get("action").and_then(serde_json::Value::as_str)
        == Some("hook_v2_profile_admit")
        && matches!(
            response.get("status").and_then(serde_json::Value::as_str),
            Some("accepted" | "exact_duplicate")
        )
        && response
            .get("disposition")
            .and_then(serde_json::Value::as_str)
            == Some("accepted");
    if accepted {
        HookDispatch::Handled {
            guidance: None,
            disposition: HookTransportDispositionV1::Accepted,
        }
    } else {
        unavailable()
    }
}

pub(crate) async fn dispatch_opencode_tool_after(
    event_json: &str,
    project_root: &Path,
    telemetry: Option<&HookTimingSpan>,
) -> HookDispatch {
    let started = Instant::now();
    let decoded = match tracedecay_hooks::decode_opencode_plugin_event(
        tracedecay_hooks::OpenCodePluginSurfaceV1::ToolExecuteAfter,
        event_json.as_bytes(),
    ) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => {
            return HookDispatch::NotApplicable;
        }
        Err(_) => return unavailable(),
    };
    let Some(prepared) =
        prepare_bound_hook(HookHostV1::OpenCode, event_json, project_root, decoded)
    else {
        return unavailable();
    };
    let native_session_id = prepared.native_session_id.clone();
    let native_lifecycle = prepared.native_lifecycle.clone();
    let admission = DaemonAdmissionPort::new(
        project_root,
        native_session_id.as_deref(),
        native_lifecycle.as_ref(),
        telemetry,
    );
    let delivery = DaemonFeedbackNoticeDeliveryPort::new(project_root);
    dispatch_decoded(prepared, project_root, started, &admission, &delivery).await
}

pub(crate) async fn dispatch_opencode_lsp_updated(
    event_json: &str,
    project_root: &Path,
    telemetry: Option<&HookTimingSpan>,
) -> HookDispatch {
    if tracedecay_hooks::decode_opencode_lsp_event(event_json.as_bytes()).is_err() {
        return unavailable();
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(event_json) else {
        return unavailable();
    };
    let port = DaemonOpenCodeLspUpdatePort::new(project_root, telemetry);
    if port.submit_updated_event(&event).await {
        HookDispatch::Handled {
            guidance: None,
            disposition: HookTransportDispositionV1::Accepted,
        }
    } else {
        unavailable()
    }
}

struct PreparedBoundHook {
    host: HookHostV1,
    layout: crate::storage::StoreLayout,
    snapshot: HookConfigurationSnapshotV1,
    envelope: HookEventEnvelopeV2,
    native_session_id: Option<String>,
    native_lifecycle: Option<NativeContextScoutLifecycleV1>,
    prepared_at: UtcMicros,
}

fn prepare_bound_hook(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
    decoded: tracedecay_hooks::DecodedNativeHookEventV1,
) -> Option<PreparedBoundHook> {
    let layout = super::store_layout::layout(project_root)?;
    let config_path = tracedecay_hooks::hook_configuration_path(&layout.data_root, host);
    let subscriber =
        HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(config_path));
    let now = now_utc();
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return None;
    };
    let binding = &snapshot.binding;
    let native_fields = serde_json::from_str::<NativeIdentityFields>(event_json).ok()?;
    let native_session_id = native_fields.session_id().map(str::to_owned);
    let material = native_material(&native_fields, decoded.family(), event_json.as_bytes(), now)?;
    let native_lifecycle = native_context_scout_lifecycle(host, &native_fields, material.event_id);
    let envelope = decoded.into_envelope(binding, material).ok()?;
    let envelope =
        match replay_envelope_if_pending(&layout.data_root, host, binding, &envelope, now) {
            PendingEnvelopeV1::Missing => envelope,
            PendingEnvelopeV1::Exact(queued) => queued,
            PendingEnvelopeV1::Unavailable => return None,
        };
    Some(PreparedBoundHook {
        host,
        layout,
        snapshot,
        envelope,
        native_session_id,
        native_lifecycle,
        prepared_at: now,
    })
}

async fn dispatch_decoded(
    prepared: PreparedBoundHook,
    project_root: &Path,
    started: Instant,
    admission: &DaemonAdmissionPort<'_>,
    delivery: &impl AsyncHookFeedbackDeliveryPortV1<
        tracedecay_usecases::advisory::AdvisoryHookLookupNoticeV1,
    >,
) -> HookDispatch {
    let PreparedBoundHook {
        host,
        layout,
        snapshot,
        envelope,
        prepared_at,
        ..
    } = prepared;
    let binding = &snapshot.binding;
    let immediate = match admission_window_after_elapsed(elapsed_us(started)) {
        Some((deadline, timeout)) => match tokio::time::timeout(
            timeout,
            admit_async_exact_scope(&envelope, binding, deadline, admission),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => HookImmediateAdmissionV1::Unavailable,
            Err(_) => HookImmediateAdmissionV1::TimedOut,
        },
        None => HookImmediateAdmissionV1::TimedOut,
    };
    let replay = match immediate {
        HookImmediateAdmissionV1::Accepted { .. } | HookImmediateAdmissionV1::CatchupRequired => {
            None
        }
        HookImmediateAdmissionV1::Unavailable
        | HookImmediateAdmissionV1::TimedOut
        | HookImmediateAdmissionV1::Backpressured => Some(append_for_replay(
            &layout.data_root,
            host,
            &envelope,
            binding,
            prepared_at,
        )),
    };
    let guidance_envelope_id = match &immediate {
        HookImmediateAdmissionV1::Accepted {
            ready_guidance: Some(guidance),
            ..
        } => Some(guidance.guidance_id),
        _ => None,
    };
    let control = HookRuntimeControlV1::from_configuration(&snapshot, HookGuidanceStateV1::Active);
    let completed = finish_synchronous_hook(
        &envelope,
        binding,
        control,
        immediate,
        replay,
        now_utc(),
        elapsed_us(started),
    );
    let feedback_notice = admission.take_feedback_notice();
    let github_stack_signal_available = admission.take_github_stack_signal_available();
    match completed {
        Ok(result) => {
            let rollback = HookFeedbackRollbackSwitchV1 {
                configuration_revision: snapshot.revision,
                route: HookFeedbackDeliveryRouteV1::HookV2,
            };
            let deadline = HookSynchronousDeadlineV1::after_elapsed(elapsed_us(started));
            let scout_receipt = match (result.rendered_guidance.as_ref(), guidance_envelope_id) {
                (Some(_), Some(envelope_id)) => Some(
                    crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
                        receipt_id: context_scout_delivery_receipt_id(
                            envelope.event_id,
                            envelope_id,
                        ),
                        envelope_id,
                        delivered_at: now_utc(),
                        outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Attempted,
                    },
                ),
                _ => None,
            };
            let receipts = DaemonDeliveryReceiptPort::new(project_root);
            let _ = deliver_hook_feedback(
                &envelope,
                &result.receipt,
                rollback,
                scout_receipt,
                deadline,
                &receipts,
            )
            .await;
            let delivered = deliver_hook_feedback(
                &envelope,
                &result.receipt,
                rollback,
                feedback_notice,
                deadline,
                delivery,
            )
            .await
            .unwrap_or(HookFeedbackDeliveryV1 {
                feedback: None,
                outcome: None,
            });
            HookDispatch::Handled {
                guidance: render_host_delivery(
                    result.rendered_guidance,
                    delivered.feedback.as_ref(),
                    github_stack_signal_available,
                ),
                disposition: result.receipt.disposition,
            }
        }
        Err(_) => unavailable(),
    }
}

#[cfg(test)]
impl HookScopedFeedbackV1 for ContextScoutFeedbackCommitV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        self.feedback.receipt_id == self.receipt.receipt_id
            && self.receipt.matches_envelope(envelope)
    }
}

#[cfg(test)]
pub(crate) async fn record_context_scout_delivery(
    project_root: &Path,
    receipt: &crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
) -> bool {
    let Some(deadline) = HookSynchronousDeadlineV1::after_elapsed(0) else {
        return false;
    };
    outcome_is_committed(
        DaemonDeliveryReceiptPort::new(project_root)
            .post_receipt(receipt, deadline)
            .await,
    )
}

#[cfg(test)]
pub(crate) async fn commit_context_scout_feedback(
    project_root: &Path,
    receipt: &crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    feedback: crate::agents::context_scout_v2::ContextScoutFeedbackV1,
) -> bool {
    let Some(deadline) = HookSynchronousDeadlineV1::after_elapsed(0) else {
        return false;
    };
    outcome_is_committed(
        DaemonContextScoutFeedbackPort::new(project_root)
            .post_feedback(receipt, &feedback, deadline)
            .await,
    )
}

fn render_host_delivery(
    guidance: Option<String>,
    feedback_notice: Option<&tracedecay_usecases::advisory::AdvisoryHookLookupNoticeV1>,
    github_stack_signal_available: bool,
) -> Option<String> {
    let notice = feedback_notice
        .and_then(|notice| serde_json::to_string(notice).ok())
        .map(|notice| format!("TraceDecay feedback ready for authorized lookup: {notice}"));
    let stack_wakeup = github_stack_signal_available
        .then_some("TraceDecay GitHub stack update available for authenticated expansion.");
    [guidance, notice, stack_wakeup.map(str::to_owned)]
        .into_iter()
        .flatten()
        .reduce(|mut rendered, next| {
            rendered.push_str("\n\n");
            rendered.push_str(&next);
            rendered
        })
}

fn append_for_replay(
    data_root: &Path,
    host: HookHostV1,
    envelope: &HookEventEnvelopeV2,
    binding: &HookScopeBindingV1,
    now: UtcMicros,
) -> SpoolAppendOutcomeV1 {
    let root = data_root.join("hook-v2-spool").join(host.hook_key());
    if std::fs::create_dir_all(&root).is_err() {
        return SpoolAppendOutcomeV1::Unavailable;
    }
    let Ok((mut spool, _)) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(host), now) else {
        return SpoolAppendOutcomeV1::Unavailable;
    };
    match spool.append(envelope.clone(), binding, now) {
        Ok(_) => SpoolAppendOutcomeV1::Accepted,
        Err(HookSpoolError::SpoolFull) => SpoolAppendOutcomeV1::Full,
        Err(_) => SpoolAppendOutcomeV1::Unavailable,
    }
}

/// Reuse a timed-out attempt's exact envelope before retrying daemon admission.
/// The spool contains only typed, protected material; provider paths and
/// payloads are neither retained nor compared here.
#[derive(Debug, PartialEq, Eq)]
enum PendingEnvelopeV1 {
    Missing,
    Exact(HookEventEnvelopeV2),
    Unavailable,
}

fn replay_envelope_if_pending(
    data_root: &Path,
    host: HookHostV1,
    binding: &HookScopeBindingV1,
    retry: &HookEventEnvelopeV2,
    now: UtcMicros,
) -> PendingEnvelopeV1 {
    let root = data_root.join("hook-v2-spool").join(host.hook_key());
    let Ok((spool, _)) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(host), now) else {
        return PendingEnvelopeV1::Unavailable;
    };
    let Some(queued) = spool.pending_envelope(retry.event_id) else {
        return PendingEnvelopeV1::Missing;
    };
    if queued.validate(binding).is_err() {
        return PendingEnvelopeV1::Unavailable;
    }
    let mut retry_identity = retry.clone();
    retry_identity.observed_at = queued.observed_at;
    if queued == retry_identity {
        PendingEnvelopeV1::Exact(queued)
    } else {
        PendingEnvelopeV1::Unavailable
    }
}

pub fn native_capture_material(
    source: tracedecay_hooks::NativeHookCaptureSourceV1,
    payload: &[u8],
    observed_at: UtcMicros,
) -> Result<NativeEnvelopeMaterialV1, NativeHookDecodeError> {
    let decoded = match source {
        tracedecay_hooks::NativeHookCaptureSourceV1::Host(host) => {
            tracedecay_hooks::decode_native_hook_event(host, payload)?
        }
        tracedecay_hooks::NativeHookCaptureSourceV1::OpenCodeToolExecuteAfter => {
            tracedecay_hooks::decode_opencode_plugin_event(
                tracedecay_hooks::OpenCodePluginSurfaceV1::ToolExecuteAfter,
                payload,
            )?
        }
    };
    let fields = serde_json::from_slice::<NativeIdentityFields>(payload)
        .map_err(|_| NativeHookDecodeError::MalformedPayload)?;
    native_material(&fields, decoded.family(), payload, observed_at)
        .ok_or(NativeHookDecodeError::MissingOpaqueMaterial)
}

/// Builds the envelope material from the identity fields the caller already
/// decoded. `prepare_bound_hook` decodes them once for the native session id and
/// the context-scout lifecycle; re-decoding the same payload here was a second
/// full deserialization of every hook event.
fn native_material(
    fields: &NativeIdentityFields,
    family: tracedecay_hooks::HookEventFamily,
    event_material: &[u8],
    observed_at: UtcMicros,
) -> Option<NativeEnvelopeMaterialV1> {
    let session = fields.session_id()?;
    let event_id = fields.event_key().map_or_else(
        || native_event_digest(family, b"exact-event-material", event_material),
        |event_key| {
            if family == tracedecay_hooks::HookEventFamily::SavedEdit {
                fields.file_path().map_or_else(
                    || native_event_digest(family, b"provider-event-key", event_key.as_bytes()),
                    |file_path| saved_edit_event_id(event_key, file_path),
                )
            } else {
                native_event_digest(family, b"provider-event-key", event_key.as_bytes())
            }
        },
    );
    Some(NativeEnvelopeMaterialV1 {
        event_id,
        protected_session_id: protected_session_id_for_native(session),
        observed_at,
        tool_id: (family == tracedecay_hooks::HookEventFamily::ToolLifecycle).then_some(event_id),
        effect_receipt_id: fields.call_id().map(|value| hash16(value.as_bytes())),
        file_id: (family == tracedecay_hooks::HookEventFamily::SavedEdit).then(|| {
            fields
                .file_path()
                .map_or(event_id, |file_path| hash16(file_path.as_bytes()))
        }),
        changed_range_count: fields
            .edits
            .as_ref()
            .map_or(1, |edits| edits.len().min(64) as u8),
    })
}

fn saved_edit_event_id(event_key: &str, file_path: &str) -> [u8; 16] {
    let mut material = Vec::with_capacity(event_key.len() + file_path.len() + 16);
    material.extend_from_slice(&(event_key.len() as u64).to_le_bytes());
    material.extend_from_slice(event_key.as_bytes());
    material.extend_from_slice(&(file_path.len() as u64).to_le_bytes());
    material.extend_from_slice(file_path.as_bytes());
    native_event_digest(
        tracedecay_hooks::HookEventFamily::SavedEdit,
        b"provider-event-key-and-file",
        &material,
    )
}

/// Hash native event identity in the event-family domain. Provider hook
/// surfaces legitimately reuse IDs across lifecycle families, while the spool
/// keys pending records only by this ID.
fn native_event_digest(
    family: tracedecay_hooks::HookEventFamily,
    source: &[u8],
    material: &[u8],
) -> [u8; 16] {
    let family_tag = match family {
        tracedecay_hooks::HookEventFamily::SessionBoundary => 1u8,
        tracedecay_hooks::HookEventFamily::PromptBoundary => 2,
        tracedecay_hooks::HookEventFamily::ToolLifecycle => 3,
        tracedecay_hooks::HookEventFamily::SavedEdit => 4,
        tracedecay_hooks::HookEventFamily::TestLifecycle => 5,
    };
    let mut digest_material = Vec::with_capacity(source.len() + material.len() + 49);
    digest_material.extend_from_slice(b"tracedecay.hook-v2.native-event.v2");
    digest_material.push(family_tag);
    digest_material.extend_from_slice(&(source.len() as u64).to_le_bytes());
    digest_material.extend_from_slice(source);
    digest_material.extend_from_slice(&(material.len() as u64).to_le_bytes());
    digest_material.extend_from_slice(material);
    hash16(&digest_material)
}

fn hash16(bytes: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(bytes);
    let mut output = [0; 16];
    output.copy_from_slice(&digest[..16]);
    output
}

/// Native saved-edit file identity for one exact provider-reported path.
/// Producers reverse-map an admitted `SavedEdit` hook to its indexed document
/// by minting candidate identities through this same function.
pub(crate) fn native_file_id_for_path(file_path: &str) -> [u8; 16] {
    hash16(file_path.as_bytes())
}

fn hash32(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn protected_session_id_for_native(session_id: &str) -> [u8; 32] {
    hash32(session_id.as_bytes())
}

fn domain_hash16(value: &str, domain: &str) -> [u8; 16] {
    hash16(format!("{domain}:{value}").as_bytes())
}

fn domain_hash32(value: &str, domain: &str) -> [u8; 32] {
    hash32(format!("{domain}:{value}").as_bytes())
}

fn unavailable() -> HookDispatch {
    HookDispatch::Unavailable(HookTransportDispositionV1::CatchupRequired)
}

#[cfg(test)]
mod tests;
