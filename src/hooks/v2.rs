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
    edits: Option<Vec<serde_json::Value>>,
    properties: Option<NativeIdentityProperties>,
    input: Option<NativeIdentityInput>,
    extra: Option<NativeIdentityExtra>,
    route: Option<NativeIdentityRoute>,
    receipt: Option<NativeIdentityReceipt>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityProperties {
    #[serde(alias = "sessionID")]
    session_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct NativeIdentityInput {
    #[serde(alias = "sessionID")]
    session_id: Option<String>,
    #[serde(alias = "callID")]
    call_id: Option<String>,
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
    let material = native_material(&native_fields, decoded.family(), now)?;
    let envelope = decoded.into_envelope(binding, material).ok()?;
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

/// Builds the envelope material from the identity fields the caller already
/// decoded. `prepare_bound_hook` decodes them once for the native session id and
/// the context-scout lifecycle; re-decoding the same payload here was a second
/// full deserialization of every hook event.
fn native_material(
    fields: &NativeIdentityFields,
    family: tracedecay_hooks::HookEventFamily,
    observed_at: UtcMicros,
) -> Option<NativeEnvelopeMaterialV1> {
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
mod tests;
