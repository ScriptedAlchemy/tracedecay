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
    HookGuidanceStateV1, HookHostV1, HookImmediateAdmissionStateV1, HookImmediateAdmissionV1,
    HookRuntimeControlV1, HookScopeBindingV1, HookScopedFeedbackV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolV1, HookSynchronousDeadlineV1, HookTransportDispositionV1,
    NativeEnvelopeMaterialV1, NativeHookDecodeError, SpoolAppendOutcomeV1, admit_async_exact_scope,
    decode_bound_native_hook_event, deliver_hook_feedback, finish_synchronous_hook,
};

use super::analytics::{HookTimingSpan, elapsed_us};
use super::daemon_ports::{
    ContextScoutFeedbackCommitV1, DaemonAdmissionPort, DaemonContextScoutFeedbackPort,
    DaemonDeliveryReceiptPort, DaemonFeedbackNoticeDeliveryPort, DaemonOpenCodeLspUpdatePort,
    now_utc, outcome_is_committed,
};

pub(crate) enum HookV2Dispatch {
    NotApplicable,
    /// Hook V2 recognised the event but could not take ownership of it — no
    /// published binding, an unreadable store layout, or an envelope it could
    /// not decode. The disposition is still worth recording, but the event has
    /// not been admitted anywhere, so callers must fall back to their pre-V2
    /// daemon notification rather than treat the event as delivered.
    Unavailable(HookTransportDispositionV1),
    Handled {
        guidance: Option<String>,
        disposition: HookTransportDispositionV1,
    },
}

impl HookV2Dispatch {
    pub(crate) fn into_recorded_guidance(
        self,
        telemetry: &HookTimingSpan,
    ) -> Option<Option<String>> {
        match self {
            Self::NotApplicable => None,
            Self::Unavailable(disposition) => {
                telemetry.note_hook_v2_disposition(disposition);
                None
            }
            Self::Handled {
                guidance,
                disposition,
            } => {
                telemetry.note_hook_v2_disposition(disposition);
                Some(guidance)
            }
        }
    }
}

pub(crate) const HOOK_V2_BOUND_HOSTS: &[HookHostV1] = &[
    HookHostV1::ClaudeCode,
    HookHostV1::Codex,
    HookHostV1::CursorDesktop,
    HookHostV1::CursorCloud,
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
            message: "cannot publish Hook V2 binding without typed project identity".to_owned(),
        }
    })?;
    let typed_project_id = ProjectId::new(project_key.to_owned()).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("cannot validate Hook V2 project identity: {error}"),
        }
    })?;
    let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
        &layout.project_root,
        &typed_project_id,
    )
    .map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("cannot resolve Hook V2 repository/worktree scope: {error}"),
    })?;
    let now = now_utc();
    let revision = now.0.max(1) as u64;
    let (project_id, repository_id, worktree_id, worktree_epoch) =
        binding_identity_from_scope(&scope, revision);
    for host in HOOK_V2_BOUND_HOSTS {
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
                    "failed to publish {} Hook V2 binding: {error}",
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
    prompt_id: Option<String>,
    turn_id: Option<String>,
    #[serde(alias = "toolUseID")]
    tool_use_id: Option<String>,
    #[serde(alias = "toolCallID")]
    tool_call_id: Option<String>,
    #[serde(alias = "callID")]
    call_id: Option<String>,
    #[serde(alias = "filePath")]
    file_path: Option<String>,
    #[serde(alias = "toolName")]
    tool_name: Option<String>,
    edits: Option<Vec<serde_json::Value>>,
    properties: Option<NativeIdentityProperties>,
    input: Option<NativeIdentityInput>,
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
    tool: Option<String>,
    #[serde(alias = "filePath")]
    file_path: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityOutput {
    metadata: Option<NativeIdentityOutputMetadata>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityOutputMetadata {
    #[serde(default)]
    files: Vec<NativeIdentityFile>,
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
/// boundary. Both values come from checked-in host fields; paths, payloads,
/// and derived placeholder identities are deliberately unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeContextScoutLifecycleV1 {
    pub(crate) session_id: SessionId,
    pub(crate) call_id: ObservationId,
}

impl NativeContextScoutLifecycleV1 {
    pub(crate) fn new(session_id: &str, call_id: &str) -> Option<Self> {
        Some(Self {
            session_id: SessionId::new(session_id.to_owned()).ok()?,
            call_id: ObservationId::new(call_id.to_owned()).ok()?,
        })
    }

    pub(crate) fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        matches!(
            envelope.producer,
            HookHostV1::KimiCode | HookHostV1::OpenCode
        ) && protected_session_id_for_native(self.session_id.as_str())
            == envelope.protected_session_id
            && hash16(self.call_id.as_str().as_bytes()) == envelope.event_id
            && matches!(
                envelope.event,
                tracedecay_hooks::HookEventV2::SavedEdit { .. }
                    | tracedecay_hooks::HookEventV2::ToolLifecycle { .. }
            )
    }
}

#[derive(Deserialize)]
struct NativeIdentityFile {
    #[serde(alias = "filePath")]
    file_path: String,
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
            .or_else(|| {
                self.properties
                    .as_ref()
                    .and_then(|properties| properties.file.as_deref())
            })
            .or_else(|| {
                self.input
                    .as_ref()
                    .and_then(|input| input.file_path.as_deref())
            })
            .or_else(|| {
                self.output
                    .as_ref()
                    .and_then(|output| output.metadata.as_ref())
                    .and_then(|metadata| metadata.files.first())
                    .map(|file| file.file_path.as_str())
            })
    }

    fn tool_name(&self) -> Option<&str> {
        self.tool_name
            .as_deref()
            .or_else(|| self.input.as_ref().and_then(|input| input.tool.as_deref()))
    }
}

fn native_context_scout_lifecycle(
    host: HookHostV1,
    fields: &NativeIdentityFields,
) -> Option<NativeContextScoutLifecycleV1> {
    matches!(host, HookHostV1::KimiCode | HookHostV1::OpenCode)
        .then(|| NativeContextScoutLifecycleV1::new(fields.session_id()?, fields.call_id()?))
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
) -> HookV2Dispatch {
    let started = Instant::now();
    let decoded = match tracedecay_hooks::decode_native_hook_event(host, event_json.as_bytes()) {
        Ok(decoded) => decoded,
        Err(
            NativeHookDecodeError::UnsupportedNativeEvent
            | NativeHookDecodeError::UnsupportedNativeFamily,
        ) => {
            return HookV2Dispatch::NotApplicable;
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

pub(crate) async fn dispatch_opencode_tool_after(
    event_json: &str,
    project_root: &Path,
    telemetry: Option<&HookTimingSpan>,
) -> HookV2Dispatch {
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
            return HookV2Dispatch::NotApplicable;
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
) -> HookV2Dispatch {
    if tracedecay_hooks::decode_opencode_lsp_event(event_json.as_bytes()).is_err() {
        return unavailable();
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(event_json) else {
        return unavailable();
    };
    let port = DaemonOpenCodeLspUpdatePort::new(project_root, telemetry);
    if port.submit_updated_event(&event).await {
        HookV2Dispatch::Handled {
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
    let layout = crate::storage::resolve_layout_for_current_profile(project_root).ok()?;
    let config_path = tracedecay_hooks::hook_configuration_path(&layout.data_root, host);
    let subscriber =
        HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(config_path));
    let now = now_utc();
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return None;
    };
    let binding = &snapshot.binding;
    let native_fields =
        serde_json::from_str::<NativeIdentityFields>(event_json).unwrap_or_default();
    let native_session_id = native_fields.session_id().map(str::to_owned);
    let native_lifecycle = native_context_scout_lifecycle(host, &native_fields);
    let material = native_material(event_json, decoded.family(), now)?;
    let envelope =
        decode_bound_native_hook_event(host, event_json.as_bytes(), binding, material).ok()?;
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
        crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
    >,
) -> HookV2Dispatch {
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
            HookV2Dispatch::Handled {
                guidance: render_host_delivery(
                    result.rendered_guidance,
                    delivered.feedback.as_ref(),
                ),
                disposition: result.receipt.disposition,
            }
        }
        Err(_) => unavailable(),
    }
}

impl HookScopedFeedbackV1 for crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        feedback_notice_matches_envelope(self, envelope)
    }
}

impl HookScopedFeedbackV1 for crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        self.receipt_id != [0; 16]
            && self.envelope_id != [0; 16]
            && self.delivered_at.0 > 0
            && self.receipt_id
                == context_scout_delivery_receipt_id(envelope.event_id, self.envelope_id)
    }
}

impl HookScopedFeedbackV1 for ContextScoutFeedbackCommitV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool {
        self.feedback.receipt_id == self.receipt.receipt_id
            && self.receipt.matches_envelope(envelope)
    }
}

fn feedback_notice_matches_envelope(
    notice: &crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
    envelope: &HookEventEnvelopeV2,
) -> bool {
    notice.validate().is_ok()
        && domain_hash16(notice.scope.project_id.as_str(), "project") == envelope.project_id
        && domain_hash16(notice.scope.repository_id.as_str(), "repository")
            == envelope.repository_id
        && domain_hash16(notice.scope.worktree_id.as_str(), "worktree") == envelope.worktree_id
}

fn context_scout_delivery_receipt_id(event_id: [u8; 16], envelope_id: [u8; 16]) -> [u8; 16] {
    let mut material = [0; 32];
    material[..16].copy_from_slice(&event_id);
    material[16..].copy_from_slice(&envelope_id);
    hash16(&material)
}

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
    feedback_notice: Option<&crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>,
) -> Option<String> {
    let notice = feedback_notice
        .and_then(|notice| serde_json::to_string(notice).ok())
        .map(|notice| format!("TraceDecay feedback ready for authorized lookup: {notice}"));
    match (guidance, notice) {
        (Some(guidance), Some(notice)) => Some(format!("{guidance}\n\n{notice}")),
        (Some(guidance), None) => Some(guidance),
        (None, Some(notice)) => Some(notice),
        (None, None) => None,
    }
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

fn native_material(
    event_json: &str,
    family: tracedecay_hooks::HookEventFamily,
    observed_at: UtcMicros,
) -> Option<NativeEnvelopeMaterialV1> {
    let fields = serde_json::from_str::<NativeIdentityFields>(event_json).unwrap_or_default();
    let session = fields.session_id()?;
    let event_id = fields.event_key().map_or_else(
        || typed_native_event_fallback(session, family, observed_at),
        |event_key| hash16(event_key.as_bytes()),
    );
    Some(NativeEnvelopeMaterialV1 {
        event_id,
        protected_session_id: protected_session_id_for_native(session),
        observed_at,
        tool_id: (family == tracedecay_hooks::HookEventFamily::ToolLifecycle).then_some(event_id),
        effect_receipt_id: fields.call_id().map(|value| hash16(value.as_bytes())),
        file_id: (family == tracedecay_hooks::HookEventFamily::SavedEdit).then_some(event_id),
        changed_range_count: fields
            .edits
            .as_ref()
            .map_or(1, |edits| edits.len().min(64) as u8),
    })
}

fn typed_native_event_fallback(
    session_id: &str,
    family: tracedecay_hooks::HookEventFamily,
    observed_at: UtcMicros,
) -> [u8; 16] {
    let family = match family {
        tracedecay_hooks::HookEventFamily::SessionBoundary => 1u8,
        tracedecay_hooks::HookEventFamily::PromptBoundary => 2,
        tracedecay_hooks::HookEventFamily::ToolLifecycle => 3,
        tracedecay_hooks::HookEventFamily::SavedEdit => 4,
        tracedecay_hooks::HookEventFamily::TestLifecycle => 5,
    };
    let mut material = Vec::with_capacity(session_id.len() + 10);
    material.extend_from_slice(session_id.as_bytes());
    material.push(family);
    material.extend_from_slice(&observed_at.0.to_le_bytes());
    hash16(&material)
}

fn hash16(bytes: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(bytes);
    let mut output = [0; 16];
    output.copy_from_slice(&digest[..16]);
    output
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

fn unavailable() -> HookV2Dispatch {
    HookV2Dispatch::Unavailable(HookTransportDispositionV1::CatchupRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::daemon_ports::daemon_admission_response;
    use std::sync::Mutex;
    use tracedecay_domain::feedback::{FeedbackCycleId, FeedbackResultId, FeedbackScopeV1};
    use tracedecay_domain::{CodeGenerationId, CommitId, ManifestDigest, RepositoryId, WorktreeId};
    use tracedecay_hooks::{
        HookAdmissionReceiptV1, HookDeliveryFutureV1, HookFeedbackDeliveryOutcomeV1,
        HookGuidanceDispositionV1,
    };

    fn scope(worktree: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.hook-v2-test").unwrap(),
            RepositoryId::new("repository.hook-v2-test").unwrap(),
            WorktreeId::new(worktree).unwrap(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn hook_binding_uses_exact_resolved_worktree_and_revision_epoch() {
        let first = binding_identity_from_scope(&scope("worktree.first"), 17);
        let second = binding_identity_from_scope(&scope("worktree.second"), 19);

        assert_eq!(first.0, second.0);
        assert_eq!(first.1, second.1);
        assert_ne!(first.2, second.2);
        assert_eq!(first.3, 17);
        assert_eq!(second.3, 19);
    }

    #[test]
    fn every_host_with_a_native_pr13_event_receives_a_daemon_binding() {
        let hosts = [
            HookHostV1::ClaudeCode,
            HookHostV1::Codex,
            HookHostV1::CursorDesktop,
            HookHostV1::CursorCloud,
            HookHostV1::Hermes,
            HookHostV1::Kiro,
            HookHostV1::KimiCode,
            HookHostV1::OpenCode,
            HookHostV1::Cline,
            HookHostV1::RooCode,
            HookHostV1::Kilo,
        ];
        let families = [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ];

        for host in hosts {
            let has_native = families.into_iter().any(|family| {
                tracedecay_hooks::stock_event_support(host, family)
                    == tracedecay_hooks::HookEventSupportV1::Native
            });
            assert_eq!(HOOK_V2_BOUND_HOSTS.contains(&host), has_native, "{host:?}");
        }
    }

    #[test]
    fn daemon_catchup_disposition_is_not_reclassified_as_unavailable() {
        let response = serde_json::json!({
            "action": "hook_v2_admit",
            "status": "rejected",
            "disposition": HookTransportDispositionV1::CatchupRequired,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
        });

        assert!(matches!(
            daemon_admission_response(&response).immediate,
            HookImmediateAdmissionV1::CatchupRequired
        ));
    }

    #[test]
    fn admission_window_switches_to_replay_at_twenty_five_milliseconds() {
        let (_, initial) = admission_window_after_elapsed(0).unwrap();
        assert_eq!(
            initial,
            Duration::from_micros(HOOK_ADMISSION_ACK_BUDGET_MICROS)
        );
        let (_, last) =
            admission_window_after_elapsed(HOOK_ADMISSION_ACK_BUDGET_MICROS - 1).unwrap();
        assert_eq!(last, Duration::from_micros(1));
        assert!(admission_window_after_elapsed(HOOK_ADMISSION_ACK_BUDGET_MICROS).is_none());
    }

    #[test]
    fn daemon_admission_response_rejects_open_or_incoherent_actions() {
        let open = serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": HookTransportDispositionV1::Accepted,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
            "unexpected": true,
        });
        let incoherent = serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": HookTransportDispositionV1::CatchupRequired,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
        });

        for response in [&open, &incoherent] {
            assert!(matches!(
                daemon_admission_response(response).immediate,
                HookImmediateAdmissionV1::Unavailable
            ));
        }
    }

    #[test]
    fn daemon_feedback_notice_survives_into_host_delivery() {
        let notice = crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
            scope: FeedbackScopeV1 {
                project_id: ProjectId::new("project.hook-v2-test").unwrap(),
                repository_id: RepositoryId::new("repository.hook-v2-test").unwrap(),
                worktree_id: WorktreeId::new("worktree.hook-v2-test").unwrap(),
                branch_ref: "refs/heads/feature".to_owned(),
                head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
            },
            result_id: FeedbackResultId::new("result.hook-v2-test").unwrap(),
            cycle_id: FeedbackCycleId::new("cycle.hook-v2-test").unwrap(),
            generation_id: CodeGenerationId::new("generation.hook-v2-test").unwrap(),
            generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            returned_findings: 2,
            omitted_findings: 1,
        };
        let current_envelope = HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: [1; 16],
            producer: HookHostV1::ClaudeCode,
            protected_session_id: [2; 32],
            project_id: domain_hash16(notice.scope.project_id.as_str(), "project"),
            repository_id: domain_hash16(notice.scope.repository_id.as_str(), "repository"),
            worktree_id: domain_hash16(notice.scope.worktree_id.as_str(), "worktree"),
            worktree_epoch: 1,
            binding_token: [3; 32],
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            observed_at: UtcMicros(1),
            event: tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
            },
        };
        assert!(feedback_notice_matches_envelope(&notice, &current_envelope));
        let mut stale_envelope = current_envelope;
        stale_envelope.worktree_id = [9; 16];
        assert!(!feedback_notice_matches_envelope(&notice, &stale_envelope));
        let response = serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": HookTransportDispositionV1::Accepted,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": notice,
            "reason": null,
        });

        let admitted = daemon_admission_response(&response);
        assert!(matches!(
            admitted.immediate,
            HookImmediateAdmissionV1::Accepted {
                ready_guidance: None,
                ..
            }
        ));
        assert_eq!(admitted.feedback_notice, Some(notice.clone()));

        let rendered = render_host_delivery(None, Some(&notice)).unwrap();
        assert!(rendered.starts_with("TraceDecay feedback ready for authorized lookup: "));
        let encoded = rendered.split_once(": ").unwrap().1;
        assert_eq!(
            serde_json::from_str::<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>(
                encoded
            )
            .unwrap(),
            notice
        );
    }

    struct RecordingFeedbackDeliveryPort {
        calls: Mutex<usize>,
    }

    impl
        AsyncHookFeedbackDeliveryPortV1<
            crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
        > for RecordingFeedbackDeliveryPort
    {
        fn deliver_hook_v2<'a>(
            &'a self,
            _envelope: &'a HookEventEnvelopeV2,
            _feedback: &'a crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
            _deadline: HookSynchronousDeadlineV1,
        ) -> HookDeliveryFutureV1<'a> {
            Box::pin(async move {
                *self.calls.lock().unwrap() += 1;
                HookFeedbackDeliveryOutcomeV1::Delivered
            })
        }

        fn deliver_legacy<'a>(
            &'a self,
            _envelope: &'a HookEventEnvelopeV2,
            _feedback: &'a crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
            _deadline: HookSynchronousDeadlineV1,
        ) -> HookDeliveryFutureV1<'a> {
            Box::pin(async { HookFeedbackDeliveryOutcomeV1::Unavailable })
        }
    }

    fn sample_notice() -> crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
        crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
            scope: FeedbackScopeV1 {
                project_id: ProjectId::new("project.hook-v2-test").unwrap(),
                repository_id: RepositoryId::new("repository.hook-v2-test").unwrap(),
                worktree_id: WorktreeId::new("worktree.hook-v2-test").unwrap(),
                branch_ref: "refs/heads/feature".to_owned(),
                head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
            },
            result_id: FeedbackResultId::new("result.hook-v2-test").unwrap(),
            cycle_id: FeedbackCycleId::new("cycle.hook-v2-test").unwrap(),
            generation_id: CodeGenerationId::new("generation.hook-v2-test").unwrap(),
            generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            returned_findings: 2,
            omitted_findings: 1,
        }
    }

    fn sample_envelope(
        notice: &crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
    ) -> HookEventEnvelopeV2 {
        HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: [1; 16],
            producer: HookHostV1::ClaudeCode,
            protected_session_id: [2; 32],
            project_id: domain_hash16(notice.scope.project_id.as_str(), "project"),
            repository_id: domain_hash16(notice.scope.repository_id.as_str(), "repository"),
            worktree_id: domain_hash16(notice.scope.worktree_id.as_str(), "worktree"),
            worktree_epoch: 1,
            binding_token: [3; 32],
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            observed_at: UtcMicros(1),
            event: tracedecay_hooks::HookEventV2::SessionBoundary {
                boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
            },
        }
    }

    fn sample_receipt(
        immediate: HookImmediateAdmissionStateV1,
        deadline_exceeded: bool,
    ) -> HookAdmissionReceiptV1 {
        HookAdmissionReceiptV1 {
            event_id: [1; 16],
            protected_session_id: [2; 32],
            configuration_revision: 1,
            completed_at: UtcMicros(10),
            elapsed_micros: 1,
            deadline_exceeded,
            immediate,
            disposition: HookTransportDispositionV1::Accepted,
            guidance: HookGuidanceDispositionV1::NotReady,
        }
    }

    #[tokio::test]
    async fn feedback_notice_never_delivers_after_deadline_or_failed_admission() {
        let notice = sample_notice();
        let envelope = sample_envelope(&notice);
        let port = RecordingFeedbackDeliveryPort {
            calls: Mutex::new(0),
        };
        let rollback = HookFeedbackRollbackSwitchV1 {
            configuration_revision: 1,
            route: HookFeedbackDeliveryRouteV1::HookV2,
        };
        let deadline = HookSynchronousDeadlineV1::after_elapsed(0);

        let accepted = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
            rollback,
            Some(notice.clone()),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert!(accepted.feedback.is_some());
        assert_eq!(*port.calls.lock().unwrap(), 1);

        let after_deadline = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Accepted, true),
            rollback,
            Some(notice.clone()),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert!(after_deadline.feedback.is_none());

        let backpressured = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Backpressured, false),
            rollback,
            Some(notice),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert!(backpressured.feedback.is_none());
        assert_eq!(*port.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn host_delivery_and_explicit_feedback_use_typed_daemon_commits() {
        let project = tempfile::tempdir().unwrap();
        let notice = sample_notice();
        let mut envelope = sample_envelope(&notice);
        envelope.event_id = [7; 16];
        let envelope_id = [22; 16];
        let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
            receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
            envelope_id,
            delivered_at: UtcMicros(23),
            outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Displayed,
        };
        let feedback = crate::agents::context_scout_v2::ContextScoutFeedbackV1 {
            receipt_id: receipt.receipt_id,
            kind: crate::agents::context_scout_v2::ContextScoutFeedbackKindV1::ExplicitlyAccepted,
        };
        let commit = ContextScoutFeedbackCommitV1 {
            receipt: receipt.clone(),
            feedback,
        };
        let guard = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "status": "stored" }),
            serde_json::json!({ "status": "duplicate" }),
            serde_json::json!({ "status": "stored" }),
        ]);
        let admission = sample_receipt(HookImmediateAdmissionStateV1::Accepted, false);
        let rollback = HookFeedbackRollbackSwitchV1 {
            configuration_revision: 1,
            route: HookFeedbackDeliveryRouteV1::HookV2,
        };
        let deadline = HookSynchronousDeadlineV1::after_elapsed(0);

        let recorded = deliver_hook_feedback(
            &envelope,
            &admission,
            rollback,
            Some(receipt.clone()),
            deadline,
            &DaemonDeliveryReceiptPort::new(project.path()),
        )
        .await
        .unwrap();
        assert_eq!(
            recorded.outcome,
            Some(HookFeedbackDeliveryOutcomeV1::Delivered)
        );

        let committed = deliver_hook_feedback(
            &envelope,
            &admission,
            rollback,
            Some(commit.clone()),
            deadline,
            &DaemonContextScoutFeedbackPort::new(project.path()),
        )
        .await
        .unwrap();
        assert_eq!(
            committed.outcome,
            Some(HookFeedbackDeliveryOutcomeV1::Duplicate)
        );

        let delivered = deliver_hook_feedback(
            &envelope,
            &admission,
            rollback,
            Some(notice.clone()),
            deadline,
            &DaemonFeedbackNoticeDeliveryPort::new(project.path()),
        )
        .await
        .unwrap();
        assert_eq!(
            delivered.outcome,
            Some(HookFeedbackDeliveryOutcomeV1::Delivered)
        );
        assert_eq!(delivered.feedback.as_ref(), Some(&notice));

        let calls = guard.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0.as_deref(), Some(project.path()));
        assert_eq!(calls[0].1["action"], "hook_v2_delivery_receipt");
        assert_eq!(
            serde_json::from_value::<
                crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
            >(calls[0].1["receipt"].clone())
            .unwrap(),
            receipt
        );
        assert_eq!(calls[1].1["action"], "hook_v2_feedback");
        assert_eq!(
            serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutFeedbackV1>(
                calls[1].1["feedback"].clone()
            )
            .unwrap(),
            commit.feedback
        );
        assert_eq!(calls[2].1["action"], "hook_v2_feedback_notice_delivery");
        assert_eq!(
            serde_json::from_value::<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>(
                calls[2].1["feedback_notice"].clone()
            )
            .unwrap(),
            notice
        );
    }

    #[tokio::test]
    async fn scout_receipt_and_feedback_helpers_delegate_to_daemon_ports() {
        let project = tempfile::tempdir().unwrap();
        let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
            receipt_id: [21; 16],
            envelope_id: [22; 16],
            delivered_at: UtcMicros(23),
            outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Displayed,
        };
        let feedback = crate::agents::context_scout_v2::ContextScoutFeedbackV1 {
            receipt_id: receipt.receipt_id,
            kind: crate::agents::context_scout_v2::ContextScoutFeedbackKindV1::ExplicitlyAccepted,
        };
        let guard = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "status": "stored" }),
            serde_json::json!({ "status": "duplicate" }),
        ]);

        assert!(record_context_scout_delivery(project.path(), &receipt).await);
        assert!(commit_context_scout_feedback(project.path(), &receipt, feedback).await);

        let calls = guard.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1["action"], "hook_v2_delivery_receipt");
        assert_eq!(calls[1].1["action"], "hook_v2_feedback");
    }

    #[test]
    fn opencode_event_uses_nested_properties_identity() {
        let material = native_material(
            r#"{
                "id": "event-17",
                "properties": {
                    "sessionID": "session-23",
                    "file": "/project/src/lib.rs"
                }
            }"#,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            UtcMicros(41),
        )
        .unwrap();

        assert_eq!(material.event_id, hash16(b"event-17"));
        assert_eq!(material.protected_session_id, hash32(b"session-23"));
        assert_eq!(material.file_id, Some(hash16(b"event-17")));
    }

    fn opencode_lsp_fixture_event() -> (serde_json::Value, String) {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/packaged_host_events/opencode/baseline.json"
        ))
        .unwrap();
        let event = fixture["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["identity"] == "lsp_updated")
            .unwrap()["request"]
            .clone();
        let event_json = serde_json::to_string(&event).unwrap();
        (event, event_json)
    }

    #[tokio::test]
    async fn opencode_lsp_updated_uses_project_scoped_daemon_action() {
        let project = tempfile::tempdir().unwrap();
        let (event, event_json) = opencode_lsp_fixture_event();
        let guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "opencode_lsp_updated",
            "status": "accepted",
        })]);

        let dispatch = dispatch_opencode_lsp_updated(&event_json, project.path(), None).await;

        assert!(matches!(
            dispatch,
            HookV2Dispatch::Handled {
                guidance: None,
                disposition: HookTransportDispositionV1::Accepted,
            }
        ));
        let calls = guard.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_deref(), Some(project.path()));
        assert_eq!(calls[0].1["action"], "opencode_lsp_updated");
        assert_eq!(calls[0].1["event"], event);
    }

    #[tokio::test]
    async fn opencode_lsp_updated_rejects_non_accepted_daemon_status() {
        let project = tempfile::tempdir().unwrap();
        let (_event, event_json) = opencode_lsp_fixture_event();
        let _guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "opencode_lsp_updated",
            "status": "rejected",
        })]);

        let dispatch = dispatch_opencode_lsp_updated(&event_json, project.path(), None).await;
        assert!(matches!(
            dispatch,
            HookV2Dispatch::Unavailable(HookTransportDispositionV1::CatchupRequired)
        ));
    }

    #[tokio::test]
    async fn delivery_receipt_withheld_when_ineligible_or_foreign_envelope() {
        let project = tempfile::tempdir().unwrap();
        let notice = sample_notice();
        let mut envelope = sample_envelope(&notice);
        envelope.event_id = [9; 16];
        let envelope_id = [11; 16];
        let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
            receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
            envelope_id,
            delivered_at: UtcMicros(23),
            outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Attempted,
        };
        let foreign = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
            receipt_id: [3; 16],
            ..receipt.clone()
        };
        let port = DaemonDeliveryReceiptPort::new(project.path());
        let rollback = HookFeedbackRollbackSwitchV1 {
            configuration_revision: 1,
            route: HookFeedbackDeliveryRouteV1::HookV2,
        };
        let deadline = HookSynchronousDeadlineV1::after_elapsed(0);
        let guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "status": "stored"
        })]);

        let after_deadline = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Accepted, true),
            rollback,
            Some(receipt.clone()),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert!(after_deadline.feedback.is_none());

        let foreign_scope = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
            rollback,
            Some(foreign),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert!(foreign_scope.feedback.is_none());

        let accepted = deliver_hook_feedback(
            &envelope,
            &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
            rollback,
            Some(receipt),
            deadline,
            &port,
        )
        .await
        .unwrap();
        assert_eq!(
            accepted.outcome,
            Some(HookFeedbackDeliveryOutcomeV1::Delivered)
        );
        assert_eq!(guard.calls().len(), 1);
    }

    #[test]
    fn opencode_tool_event_uses_nested_input_and_output_identity() {
        let material = native_material(
            r#"{
                "input": {
                    "tool": "apply_patch",
                    "sessionID": "session-29",
                    "callID": "call-31"
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/project/src/main.rs"}]
                    }
                }
            }"#,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            UtcMicros(43),
        )
        .unwrap();

        assert_eq!(material.event_id, hash16(b"call-31"));
        assert_eq!(material.protected_session_id, hash32(b"session-29"));
        assert_eq!(material.effect_receipt_id, Some(hash16(b"call-31")));
        assert_eq!(material.file_id, Some(hash16(b"call-31")));
    }

    #[test]
    fn native_path_tool_and_payload_aliases_cannot_change_native_identity() {
        let first = native_material(
            r#"{
                "input": {
                    "tool": "apply_patch",
                    "sessionID": "session-29",
                    "callID": "call-31",
                    "args": {"patchText": "first payload"}
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/project/first.rs"}]
                    }
                }
            }"#,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            UtcMicros(43),
        )
        .unwrap();
        let aliases_changed = native_material(
            r#"{
                "input": {
                    "tool": "write",
                    "sessionID": "session-29",
                    "callID": "call-31",
                    "args": {"patchText": "unrelated payload"}
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/elsewhere/alias.rs"}]
                    }
                }
            }"#,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            UtcMicros(43),
        )
        .unwrap();
        let different_native_event = native_material(
            r#"{
                "input": {
                    "tool": "write",
                    "sessionID": "session-29",
                    "callID": "call-32"
                }
            }"#,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            UtcMicros(43),
        )
        .unwrap();

        assert_eq!(aliases_changed.event_id, first.event_id);
        assert_eq!(aliases_changed.file_id, first.file_id);
        assert_ne!(different_native_event.event_id, first.event_id);
        assert_ne!(different_native_event.file_id, first.file_id);
    }

    #[test]
    fn kimi_rendered_hook_fixture_queues_only_native_session_and_call_identity() {
        let fixture =
            include_str!("../../tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json")
                .replace("<SESSION_ID>", "session.kimi.native")
                .replace("<TOOL_CALL_ID>", "call.kimi.native");
        let fields = serde_json::from_str::<NativeIdentityFields>(&fixture).unwrap();

        let lifecycle = native_context_scout_lifecycle(HookHostV1::KimiCode, &fields).unwrap();

        assert_eq!(lifecycle.session_id.as_str(), "session.kimi.native");
        assert_eq!(lifecycle.call_id.as_str(), "call.kimi.native");
    }

    #[test]
    fn hermes_real_tool_fixture_uses_terminal_receipt_identity() {
        let fixture =
            include_str!("../../tests/fixtures/packaged_host_events/hermes/saved-edit.json");
        let material = native_material(
            fixture,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            UtcMicros(43),
        )
        .unwrap();

        assert_eq!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
        assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
        assert_eq!(material.tool_id, Some(hash16(b"<TOOL_CALL_ID>")));
        assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
        assert_eq!(material.file_id, None);
    }

    #[test]
    fn hermes_adapter_fixture_preserves_native_terminal_identity() {
        let fixture =
            include_str!("../../tests/fixtures/packaged_host_events/hermes/terminal-receipt.json");
        let material = native_material(
            fixture,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            UtcMicros(47),
        )
        .unwrap();

        assert_eq!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
        assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
        assert_eq!(material.tool_id, Some(hash16(b"<TOOL_CALL_ID>")));
        assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
        assert_eq!(material.file_id, None);
    }

    #[test]
    fn opencode_rendered_plugin_queues_only_tool_after_lifecycle_identity() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/packaged_host_events/opencode/baseline.json"
        ))
        .unwrap();
        let tool_after = fixture["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["identity"] == "post_tool_use")
            .unwrap()["request"]
            .to_string()
            .replace("<SESSION_ID>", "session.opencode.native")
            .replace("<CALL_ID>", "call.opencode.native");
        let fields = serde_json::from_str::<NativeIdentityFields>(&tool_after).unwrap();
        let lifecycle = native_context_scout_lifecycle(HookHostV1::OpenCode, &fields).unwrap();
        assert_eq!(lifecycle.session_id.as_str(), "session.opencode.native");
        assert_eq!(lifecycle.call_id.as_str(), "call.opencode.native");

        let file_edit = fixture["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["identity"] == "saved_edit")
            .unwrap()["request"]
            .to_string();
        let fields = serde_json::from_str::<NativeIdentityFields>(&file_edit).unwrap();
        assert!(native_context_scout_lifecycle(HookHostV1::OpenCode, &fields).is_none());
    }
}
