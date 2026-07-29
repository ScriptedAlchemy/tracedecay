//! Bounded Hook V2 admission and guidance completion contracts.
//!
//! Native decoding, daemon transport, and durable replay remain separate
//! authorities. This module only closes a completed synchronous admission
//! attempt into a receipt and optionally renders guidance that the daemon had
//! already prepared before the hook invocation.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::{
    HookConfigurationSnapshotV1, HookContractError, HookEventEnvelopeV2, HookScopeBindingV1,
    HookTransportDispositionV1, SpoolAppendOutcomeV1, render_approved_guidance,
};

pub const HOOK_SYNCHRONOUS_BUDGET_MICROS: u64 = 100_000;

/// Non-widenable deadline token furnished to admission and replay ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookSynchronousDeadlineV1 {
    remaining_micros: u64,
}

impl HookSynchronousDeadlineV1 {
    pub const fn start() -> Self {
        Self {
            remaining_micros: HOOK_SYNCHRONOUS_BUDGET_MICROS,
        }
    }

    pub const fn after_elapsed(elapsed_micros: u64) -> Option<Self> {
        match HOOK_SYNCHRONOUS_BUDGET_MICROS.checked_sub(elapsed_micros) {
            Some(remaining_micros) => Some(Self { remaining_micros }),
            None => None,
        }
    }

    pub const fn remaining_micros(self) -> u64 {
        self.remaining_micros
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookGuidanceStateV1 {
    Active,
    Paused,
    Disabled,
}

/// Daemon-published runtime controls. Pausing guidance never pauses event
/// capture or replay, and a hook cannot update this state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRuntimeControlV1 {
    pub configuration_revision: u64,
    pub published_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub guidance: HookGuidanceStateV1,
}

impl HookRuntimeControlV1 {
    pub const fn from_configuration(
        configuration: &HookConfigurationSnapshotV1,
        guidance: HookGuidanceStateV1,
    ) -> Self {
        Self {
            configuration_revision: configuration.revision,
            published_at: configuration.published_at,
            expires_at: configuration.expires_at,
            guidance,
        }
    }

    pub fn validate(self, now: UtcMicros) -> Result<(), HookRuntimeErrorV1> {
        if self.configuration_revision == 0
            || self.published_at.0 <= 0
            || self.expires_at.0 <= self.published_at.0
            || now.0 >= self.expires_at.0
        {
            return Err(HookRuntimeErrorV1::InvalidControl);
        }
        Ok(())
    }
}

/// Guidance returned by admission was already approved and materialized by
/// the daemon. It carries no deferred query, model, command, or task handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookReadyGuidanceV1 {
    pub guidance_id: [u8; 16],
    pub event_id: [u8; 16],
    pub configuration_revision: u64,
    pub expires_at: UtcMicros,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookImmediateAdmissionV1 {
    Accepted {
        admitted_at: UtcMicros,
        ready_guidance: Option<HookReadyGuidanceV1>,
    },
    CatchupRequired,
    Unavailable,
    TimedOut,
    Backpressured,
}

impl HookImmediateAdmissionV1 {
    const fn state(&self) -> HookImmediateAdmissionStateV1 {
        match self {
            Self::Accepted { .. } => HookImmediateAdmissionStateV1::Accepted,
            Self::CatchupRequired => HookImmediateAdmissionStateV1::CatchupRequired,
            Self::Unavailable => HookImmediateAdmissionStateV1::Unavailable,
            Self::TimedOut => HookImmediateAdmissionStateV1::TimedOut,
            Self::Backpressured => HookImmediateAdmissionStateV1::Backpressured,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookImmediateAdmissionStateV1 {
    Accepted,
    CatchupRequired,
    Unavailable,
    TimedOut,
    Backpressured,
}

pub type HookAdmissionFutureV1<'a> =
    Pin<Box<dyn Future<Output = HookImmediateAdmissionV1> + Send + 'a>>;

/// Non-blocking local-daemon admission seam for hosts whose native callback
/// is asynchronous (for example OpenCode plugins). Implementations receive
/// only the validated content-free envelope and bounded deadline; they cannot
/// expose model, search, command, or external-network capabilities.
pub trait AsyncHookAdmissionPortV1 {
    fn try_admit_async<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookAdmissionFutureV1<'a>;
}

/// Validate exact daemon-issued scope before yielding to asynchronous local
/// admission. This function performs no search, model, command, store-open, or
/// external-network work.
pub async fn admit_async_exact_scope(
    envelope: &HookEventEnvelopeV2,
    binding: &HookScopeBindingV1,
    deadline: HookSynchronousDeadlineV1,
    port: &impl AsyncHookAdmissionPortV1,
) -> Result<HookImmediateAdmissionV1, HookRuntimeErrorV1> {
    envelope
        .validate(binding)
        .map_err(HookRuntimeErrorV1::EnvelopeRejected)?;
    Ok(port.try_admit_async(envelope, deadline).await)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookGuidanceDispositionV1 {
    Rendered,
    NotReady,
    Paused,
    Disabled,
    Expired,
    Invalid,
    DeadlineExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookAdmissionReceiptV1 {
    pub event_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub configuration_revision: u64,
    pub completed_at: UtcMicros,
    pub elapsed_micros: u64,
    pub deadline_exceeded: bool,
    pub immediate: HookImmediateAdmissionStateV1,
    pub disposition: HookTransportDispositionV1,
    pub guidance: HookGuidanceDispositionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookSynchronousResultV1 {
    pub receipt: HookAdmissionReceiptV1,
    pub rendered_guidance: Option<String>,
}

/// Finish one synchronous hook invocation without performing I/O. The caller
/// supplies the already-completed admission and spool outcomes. Over-budget
/// work still returns a receipt, but it can never render guidance.
pub fn finish_synchronous_hook(
    envelope: &HookEventEnvelopeV2,
    binding: &HookScopeBindingV1,
    control: HookRuntimeControlV1,
    immediate: HookImmediateAdmissionV1,
    replay_append: Option<SpoolAppendOutcomeV1>,
    completed_at: UtcMicros,
    elapsed_micros: u64,
) -> Result<HookSynchronousResultV1, HookRuntimeErrorV1> {
    envelope
        .validate(binding)
        .map_err(HookRuntimeErrorV1::EnvelopeRejected)?;
    control.validate(completed_at)?;
    if let HookImmediateAdmissionV1::Accepted { admitted_at, .. } = &immediate
        && (admitted_at.0 <= 0 || admitted_at.0 > completed_at.0)
    {
        return Err(HookRuntimeErrorV1::InvalidAdmission);
    }

    let immediate_state = immediate.state();
    let disposition = match immediate_state {
        HookImmediateAdmissionStateV1::Accepted => HookTransportDispositionV1::Accepted,
        HookImmediateAdmissionStateV1::CatchupRequired => {
            HookTransportDispositionV1::CatchupRequired
        }
        HookImmediateAdmissionStateV1::Unavailable
        | HookImmediateAdmissionStateV1::TimedOut
        | HookImmediateAdmissionStateV1::Backpressured => match replay_append {
            Some(SpoolAppendOutcomeV1::Accepted) => HookTransportDispositionV1::AcceptedForReplay,
            Some(SpoolAppendOutcomeV1::Full | SpoolAppendOutcomeV1::Unavailable) | None => {
                HookTransportDispositionV1::CatchupRequired
            }
        },
    };
    let deadline_exceeded = elapsed_micros > HOOK_SYNCHRONOUS_BUDGET_MICROS;
    let (guidance, rendered_guidance) = guidance_result(
        envelope,
        control,
        &immediate,
        completed_at,
        deadline_exceeded,
    );

    Ok(HookSynchronousResultV1 {
        receipt: HookAdmissionReceiptV1 {
            event_id: envelope.event_id,
            protected_session_id: envelope.protected_session_id,
            configuration_revision: control.configuration_revision,
            completed_at,
            elapsed_micros,
            deadline_exceeded,
            immediate: immediate_state,
            disposition,
            guidance,
        },
        rendered_guidance,
    })
}

fn guidance_result(
    envelope: &HookEventEnvelopeV2,
    control: HookRuntimeControlV1,
    immediate: &HookImmediateAdmissionV1,
    now: UtcMicros,
    deadline_exceeded: bool,
) -> (HookGuidanceDispositionV1, Option<String>) {
    if deadline_exceeded {
        return (HookGuidanceDispositionV1::DeadlineExceeded, None);
    }
    match control.guidance {
        HookGuidanceStateV1::Paused => return (HookGuidanceDispositionV1::Paused, None),
        HookGuidanceStateV1::Disabled => return (HookGuidanceDispositionV1::Disabled, None),
        HookGuidanceStateV1::Active => {}
    }
    let HookImmediateAdmissionV1::Accepted {
        ready_guidance: Some(guidance),
        ..
    } = immediate
    else {
        return (HookGuidanceDispositionV1::NotReady, None);
    };
    if guidance.expires_at.0 <= now.0 {
        return (HookGuidanceDispositionV1::Expired, None);
    }
    if guidance.guidance_id == [0; 16]
        || guidance.event_id != envelope.event_id
        || guidance.configuration_revision != control.configuration_revision
    {
        return (HookGuidanceDispositionV1::Invalid, None);
    }
    match render_approved_guidance(true, &guidance.text) {
        Ok(text) => (HookGuidanceDispositionV1::Rendered, Some(text)),
        Err(_) => (HookGuidanceDispositionV1::Invalid, None),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFeedbackDeliveryRouteV1 {
    HookV2,
    Legacy,
}

/// Daemon configuration owns this rollback switch. Host lifecycle code may
/// publish a new revision, while hook code can only dispatch through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookFeedbackRollbackSwitchV1 {
    pub configuration_revision: u64,
    pub route: HookFeedbackDeliveryRouteV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookFeedbackDeliveryOutcomeV1 {
    Delivered,
    Duplicate,
    Unavailable,
}

/// Delivery-only seam over the existing feedback authority. The generic
/// payload remains the owning application's typed feedback value.
pub trait HookFeedbackDeliveryPortV1<T> {
    fn deliver_hook_v2(&self, feedback: &T) -> HookFeedbackDeliveryOutcomeV1;
    fn deliver_legacy(&self, feedback: &T) -> HookFeedbackDeliveryOutcomeV1;
}

const fn route_for_rollback(
    rollback: HookFeedbackRollbackSwitchV1,
) -> Result<HookFeedbackDeliveryRouteV1, HookRuntimeErrorV1> {
    if rollback.configuration_revision == 0 {
        return Err(HookRuntimeErrorV1::InvalidControl);
    }
    Ok(rollback.route)
}

pub fn deliver_feedback_with_rollback<T, P>(
    rollback: HookFeedbackRollbackSwitchV1,
    feedback: &T,
    port: &P,
) -> Result<HookFeedbackDeliveryOutcomeV1, HookRuntimeErrorV1>
where
    P: HookFeedbackDeliveryPortV1<T> + ?Sized,
{
    Ok(match route_for_rollback(rollback)? {
        HookFeedbackDeliveryRouteV1::HookV2 => port.deliver_hook_v2(feedback),
        HookFeedbackDeliveryRouteV1::Legacy => port.deliver_legacy(feedback),
    })
}

pub type HookDeliveryFutureV1<'a> =
    Pin<Box<dyn Future<Output = HookFeedbackDeliveryOutcomeV1> + Send + 'a>>;

/// Envelope-bound counterpart of [`HookFeedbackDeliveryPortV1`] for hook
/// dispatch, where delivery crosses the local daemon boundary. Routes,
/// outcomes, and the rollback switch are the synchronous port's; only the
/// completion is deferred. Implementations own the transport and must finish
/// inside `deadline`; they receive the validated content-free envelope and the
/// owning application's typed payload, never a hook-authored command.
pub trait AsyncHookFeedbackDeliveryPortV1<T> {
    fn deliver_hook_v2<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        feedback: &'a T,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a>;

    fn deliver_legacy<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        feedback: &'a T,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a>;
}

pub async fn deliver_feedback_with_rollback_async<T, P>(
    envelope: &HookEventEnvelopeV2,
    rollback: HookFeedbackRollbackSwitchV1,
    feedback: &T,
    deadline: HookSynchronousDeadlineV1,
    port: &P,
) -> Result<HookFeedbackDeliveryOutcomeV1, HookRuntimeErrorV1>
where
    P: AsyncHookFeedbackDeliveryPortV1<T> + ?Sized,
{
    Ok(match route_for_rollback(rollback)? {
        HookFeedbackDeliveryRouteV1::HookV2 => {
            port.deliver_hook_v2(envelope, feedback, deadline).await
        }
        HookFeedbackDeliveryRouteV1::Legacy => {
            port.deliver_legacy(envelope, feedback, deadline).await
        }
    })
}

/// Typed hook feedback that can prove it was minted for the exact admitted
/// envelope. The owning application keeps its identity derivation private, so
/// this runtime never learns how a project, repository, or worktree is hashed.
pub trait HookScopedFeedbackV1 {
    fn matches_envelope(&self, envelope: &HookEventEnvelopeV2) -> bool;
}

/// What a completed synchronous hook may hand back to its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookFeedbackDeliveryV1<T> {
    pub feedback: Option<T>,
    /// Present only when the port was actually asked to deliver.
    pub outcome: Option<HookFeedbackDeliveryOutcomeV1>,
}

impl<T> HookFeedbackDeliveryV1<T> {
    const fn withheld() -> Self {
        Self {
            feedback: None,
            outcome: None,
        }
    }
}

const fn feedback_is_eligible(receipt: &HookAdmissionReceiptV1) -> bool {
    matches!(receipt.immediate, HookImmediateAdmissionStateV1::Accepted)
        && !receipt.deadline_exceeded
}

/// Close a completed synchronous hook by acknowledging its typed feedback
/// through `port`. Feedback is withheld unless admission was accepted inside
/// the synchronous budget, the payload proves it belongs to this envelope, and
/// budget remains, so an over-budget or foreign-scope hook can never surface
/// another scope's feedback. Acknowledgement failure withholds nothing already
/// earned: the outcome is reported so callers can record it truthfully.
pub async fn deliver_hook_feedback<T, P>(
    envelope: &HookEventEnvelopeV2,
    receipt: &HookAdmissionReceiptV1,
    rollback: HookFeedbackRollbackSwitchV1,
    feedback: Option<T>,
    deadline: Option<HookSynchronousDeadlineV1>,
    port: &P,
) -> Result<HookFeedbackDeliveryV1<T>, HookRuntimeErrorV1>
where
    T: HookScopedFeedbackV1,
    P: AsyncHookFeedbackDeliveryPortV1<T> + ?Sized,
{
    let Some(feedback) = feedback
        .filter(|feedback| feedback_is_eligible(receipt) && feedback.matches_envelope(envelope))
    else {
        return Ok(HookFeedbackDeliveryV1::withheld());
    };
    let Some(deadline) = deadline else {
        return Ok(HookFeedbackDeliveryV1::withheld());
    };
    let outcome =
        deliver_feedback_with_rollback_async(envelope, rollback, &feedback, deadline, port).await?;
    Ok(HookFeedbackDeliveryV1 {
        feedback: Some(feedback),
        outcome: Some(outcome),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRuntimeStatusV1 {
    pub configuration_revision: u64,
    pub guidance: HookGuidanceStateV1,
    pub pending_replay_records: u32,
    pub catchup_required: bool,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum HookRuntimeErrorV1 {
    #[error("hook runtime control is invalid or stale")]
    InvalidControl,
    #[error("hook admission receipt timing is invalid")]
    InvalidAdmission,
    #[error("hook envelope does not satisfy the daemon-issued binding")]
    EnvelopeRejected(HookContractError),
}
