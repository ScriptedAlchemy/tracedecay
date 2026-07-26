use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ObservationId, ProjectId, SessionId, UtcMicros};
use tracedecay_hooks::{
    AsyncHookAdmissionPortV1, HookAdmissionFutureV1, HookConfigurationFileReaderV1,
    HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1, HookEventEnvelopeV2,
    HookGuidanceStateV1, HookHostV1, HookImmediateAdmissionStateV1, HookImmediateAdmissionV1,
    HookReadyGuidanceV1, HookRuntimeControlV1, HookScopeBindingV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolV1, HookSynchronousDeadlineV1, HookTransportDispositionV1,
    NativeEnvelopeMaterialV1, NativeHookDecodeError, SpoolAppendOutcomeV1, admit_async_exact_scope,
    decode_bound_native_hook_event, finish_synchronous_hook,
};

pub(crate) enum HookV2Dispatch {
    NotApplicable,
    Handled {
        guidance: Option<String>,
        #[allow(dead_code)] // Plan 07 hook transport disposition — reserved
        disposition: HookTransportDispositionV1,
    },
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
                binding_token: domain_hash32(project_key, host.as_key()),
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
                    host.as_key()
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

struct DaemonAdmissionPort<'a> {
    project_root: &'a Path,
    session_id: Option<&'a str>,
    lifecycle: Option<&'a NativeContextScoutLifecycleV1>,
    feedback_notice: Mutex<Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>>,
}

struct DaemonAdmissionResponseV1 {
    immediate: HookImmediateAdmissionV1,
    feedback_notice: Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DaemonAdmissionStatusV1 {
    Accepted,
    Committed,
    ExactDuplicate,
    Backpressured,
    Rejected,
    Unavailable,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonAdmissionResponseWireV1 {
    action: String,
    status: DaemonAdmissionStatusV1,
    disposition: Option<HookTransportDispositionV1>,
    orchestration: Option<serde_json::Value>,
    ready_guidance: Option<HookReadyGuidanceV1>,
    feedback_notice: Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>,
    reason: Option<String>,
}

fn daemon_admission_response(response: &serde_json::Value) -> DaemonAdmissionResponseV1 {
    let unavailable = || DaemonAdmissionResponseV1 {
        immediate: HookImmediateAdmissionV1::Unavailable,
        feedback_notice: None,
    };
    let Ok(wire) = serde_json::from_value::<DaemonAdmissionResponseWireV1>(response.clone()) else {
        return unavailable();
    };
    if wire.action != "hook_v2_admit" {
        return unavailable();
    }
    let _ = (&wire.orchestration, &wire.reason);
    match (wire.status, wire.disposition) {
        (DaemonAdmissionStatusV1::Rejected, Some(HookTransportDispositionV1::CatchupRequired)) => {
            DaemonAdmissionResponseV1 {
                immediate: HookImmediateAdmissionV1::CatchupRequired,
                feedback_notice: None,
            }
        }
        (
            DaemonAdmissionStatusV1::Accepted
            | DaemonAdmissionStatusV1::Committed
            | DaemonAdmissionStatusV1::ExactDuplicate,
            Some(HookTransportDispositionV1::Accepted),
        ) => {
            if wire
                .feedback_notice
                .as_ref()
                .is_some_and(|notice| notice.validate().is_err())
            {
                return unavailable();
            }
            DaemonAdmissionResponseV1 {
                immediate: HookImmediateAdmissionV1::Accepted {
                    admitted_at: now_utc(),
                    ready_guidance: wire.ready_guidance,
                },
                feedback_notice: wire.feedback_notice,
            }
        }
        (DaemonAdmissionStatusV1::Backpressured, None) => DaemonAdmissionResponseV1 {
            immediate: HookImmediateAdmissionV1::Backpressured,
            feedback_notice: None,
        },
        (DaemonAdmissionStatusV1::Unavailable, None) => unavailable(),
        _ => unavailable(),
    }
}

const HOOK_ADMISSION_ACK_BUDGET_MICROS: u64 = 25_000;

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn admission_window(started: Instant) -> Option<(HookSynchronousDeadlineV1, Duration)> {
    admission_window_after_elapsed(elapsed_micros(started))
}

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

impl AsyncHookAdmissionPortV1 for DaemonAdmissionPort<'_> {
    fn try_admit_async<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookAdmissionFutureV1<'a> {
        Box::pin(async move {
            let Ok(envelope) = serde_json::to_value(envelope) else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            let response = tokio::time::timeout(
                Duration::from_micros(deadline.remaining_micros()),
                super::daemon_hook_action(
                    Some(self.project_root),
                    serde_json::json!({
                        "action": "hook_v2_admit",
                        "envelope": envelope,
                        "native_session_id": self.session_id,
                        "native_lifecycle": self.lifecycle,
                    }),
                    None,
                ),
            )
            .await;
            let Ok(Ok(response)) = response else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            let response = daemon_admission_response(&response);
            if let Some(notice) = response.feedback_notice
                && let Ok(mut retained) = self.feedback_notice.lock()
            {
                *retained = Some(notice);
            }
            response.immediate
        })
    }
}

pub(crate) async fn dispatch(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
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
        Err(_) => {
            return HookV2Dispatch::Handled {
                guidance: None,
                disposition: HookTransportDispositionV1::CatchupRequired,
            };
        }
    };
    dispatch_decoded(host, event_json, project_root, decoded, started).await
}

pub(crate) async fn dispatch_opencode_tool_after(
    event_json: &str,
    project_root: &Path,
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
    dispatch_decoded(
        HookHostV1::OpenCode,
        event_json,
        project_root,
        decoded,
        started,
    )
    .await
}

async fn dispatch_decoded(
    host: HookHostV1,
    event_json: &str,
    project_root: &Path,
    decoded: tracedecay_hooks::DecodedNativeHookEventV1,
    started: Instant,
) -> HookV2Dispatch {
    let Ok(layout) = crate::storage::resolve_layout_for_current_profile(project_root) else {
        return unavailable();
    };
    let config_path = tracedecay_hooks::hook_configuration_path(&layout.data_root, host);
    let subscriber =
        HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(config_path));
    let now = now_utc();
    let HookConfigurationReadOutcomeV1::Bound(snapshot) = subscriber.load_current(host, now) else {
        return unavailable();
    };
    let binding = &snapshot.binding;
    let native_fields =
        serde_json::from_str::<NativeIdentityFields>(event_json).unwrap_or_default();
    let native_session_id = native_fields.session_id().map(str::to_owned);
    let native_lifecycle = native_context_scout_lifecycle(host, &native_fields);
    let Some(material) = native_material(event_json, decoded.family(), now) else {
        return unavailable();
    };
    let Ok(envelope) =
        decode_bound_native_hook_event(host, event_json.as_bytes(), binding, material)
    else {
        return unavailable();
    };

    let port = DaemonAdmissionPort {
        project_root,
        session_id: native_session_id.as_deref(),
        lifecycle: native_lifecycle.as_ref(),
        feedback_notice: Mutex::new(None),
    };
    let immediate = match admission_window(started) {
        Some((deadline, timeout)) => match tokio::time::timeout(
            timeout,
            admit_async_exact_scope(&envelope, binding, deadline, &port),
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
            now,
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
    let elapsed_before_finish_micros = elapsed_micros(started);
    let completed = finish_synchronous_hook(
        &envelope,
        binding,
        control,
        immediate,
        replay,
        now_utc(),
        elapsed_before_finish_micros,
    );
    let feedback_notice = port
        .feedback_notice
        .lock()
        .ok()
        .and_then(|mut notice| notice.take());
    match completed {
        Ok(result) => {
            if result.rendered_guidance.is_some()
                && let Some(envelope_id) = guidance_envelope_id
                && let Some(deadline) =
                    HookSynchronousDeadlineV1::after_elapsed(elapsed_before_finish_micros)
            {
                let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
                    receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
                    envelope_id,
                    delivered_at: now_utc(),
                    outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Attempted,
                };
                let _ = tokio::time::timeout(
                    Duration::from_micros(deadline.remaining_micros()),
                    record_context_scout_delivery(project_root, &receipt),
                )
                .await;
            }
            let feedback_notice = if may_render_feedback_notice(
                result.receipt.immediate,
                result.receipt.deadline_exceeded,
            ) && feedback_notice
                .as_ref()
                .is_some_and(|notice| feedback_notice_matches_envelope(notice, &envelope))
            {
                feedback_notice
            } else {
                None
            };
            if let (Some(notice), Some(deadline)) = (
                feedback_notice.as_ref(),
                HookSynchronousDeadlineV1::after_elapsed(elapsed_micros(started)),
            ) {
                let _ = tokio::time::timeout(
                    Duration::from_micros(deadline.remaining_micros()),
                    acknowledge_advisory_feedback_notice(project_root, &envelope, notice),
                )
                .await;
            }
            HookV2Dispatch::Handled {
                guidance: render_host_delivery(result.rendered_guidance, feedback_notice.as_ref()),
                disposition: result.receipt.disposition,
            }
        }
        Err(_) => unavailable(),
    }
}

fn may_render_feedback_notice(
    immediate: HookImmediateAdmissionStateV1,
    deadline_exceeded: bool,
) -> bool {
    immediate == HookImmediateAdmissionStateV1::Accepted && !deadline_exceeded
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
    super::daemon_hook_action(
        Some(project_root),
        serde_json::json!({
            "action": "hook_v2_delivery_receipt",
            "receipt": receipt,
        }),
        None,
    )
    .await
    .ok()
    .and_then(|response| {
        response
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .is_some_and(|status| matches!(status.as_str(), "stored" | "duplicate"))
}

pub(crate) async fn commit_context_scout_feedback(
    project_root: &Path,
    receipt: &crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    feedback: crate::agents::context_scout_v2::ContextScoutFeedbackV1,
) -> bool {
    super::daemon_hook_action(
        Some(project_root),
        serde_json::json!({
            "action": "hook_v2_feedback",
            "receipt": receipt,
            "feedback": feedback,
        }),
        None,
    )
    .await
    .ok()
    .and_then(|response| {
        response
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .is_some_and(|status| matches!(status.as_str(), "stored" | "duplicate"))
}

async fn acknowledge_advisory_feedback_notice(
    project_root: &Path,
    envelope: &HookEventEnvelopeV2,
    notice: &crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
) -> bool {
    super::daemon_hook_action(
        Some(project_root),
        serde_json::json!({
            "action": "hook_v2_feedback_notice_delivery",
            "envelope": envelope,
            "feedback_notice": notice,
        }),
        None,
    )
    .await
    .ok()
    .and_then(|response| {
        response
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .is_some_and(|status| matches!(status.as_str(), "stored" | "duplicate"))
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
    let root = data_root.join("hook-v2-spool").join(host.as_key());
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

fn now_utc() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros.max(1))
}

fn unavailable() -> HookV2Dispatch {
    HookV2Dispatch::Handled {
        guidance: None,
        disposition: HookTransportDispositionV1::CatchupRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::feedback::{FeedbackCycleId, FeedbackResultId, FeedbackScopeV1};
    use tracedecay_domain::{CodeGenerationId, CommitId, ManifestDigest, RepositoryId, WorktreeId};

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

    #[test]
    fn feedback_notice_never_renders_after_deadline_or_failed_admission() {
        assert!(may_render_feedback_notice(
            HookImmediateAdmissionStateV1::Accepted,
            false
        ));
        assert!(!may_render_feedback_notice(
            HookImmediateAdmissionStateV1::Accepted,
            true
        ));
        assert!(!may_render_feedback_notice(
            HookImmediateAdmissionStateV1::Backpressured,
            false
        ));
    }

    #[tokio::test]
    async fn host_delivery_and_explicit_feedback_use_typed_daemon_commits() {
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
            feedback
        );
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
        let fixture = include_str!(
            "../../crates/tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
        )
        .replace("<SESSION_ID>", "session.kimi.native")
        .replace("<TOOL_CALL_ID>", "call.kimi.native");
        let fields = serde_json::from_str::<NativeIdentityFields>(&fixture).unwrap();

        let lifecycle = native_context_scout_lifecycle(HookHostV1::KimiCode, &fields).unwrap();

        assert_eq!(lifecycle.session_id.as_str(), "session.kimi.native");
        assert_eq!(lifecycle.call_id.as_str(), "call.kimi.native");
    }

    #[test]
    fn hermes_real_tool_fixture_uses_terminal_receipt_identity() {
        let fixture = include_str!(
            "../../crates/tracedecay-hooks/fixtures/host_events/hermes/saved-edit.json"
        );
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
        let fixture = include_str!(
            "../../crates/tracedecay-hooks/fixtures/host_events/hermes/terminal-receipt.json"
        );
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
            "../../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json"
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
