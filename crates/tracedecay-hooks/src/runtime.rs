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
pub const MAX_GUIDANCE_LOOKUP_ITEMS: u8 = 1;

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

/// The host transport receives the hard deadline and must return by it. The
/// port cannot return future work; model, query, cycle, and network follow-up
/// run outside the synchronous hook process.
pub trait HookAdmissionPortV1 {
    fn try_admit(
        &self,
        envelope: &HookEventEnvelopeV2,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookImmediateAdmissionV1;
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

/// Adapter over the existing bounded replay spool. Implementations append the
/// exact validated envelope and never open an application/query store.
pub trait HookReplaySpoolPortV1 {
    fn append_for_replay(
        &mut self,
        envelope: &HookEventEnvelopeV2,
        deadline: HookSynchronousDeadlineV1,
    ) -> SpoolAppendOutcomeV1;
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

/// A later host lookup is exact-event addressed and can return at most one
/// already-ready suggestion. It cannot request search, model, cycle, or task
/// execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookGuidanceLookupRequestV1 {
    pub event_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub configuration_revision: u64,
    pub max_items: u8,
}

impl HookGuidanceLookupRequestV1 {
    pub fn validate(self) -> Result<(), HookRuntimeErrorV1> {
        if self.event_id == [0; 16]
            || self.protected_session_id == [0; 32]
            || self.configuration_revision == 0
            || self.max_items != MAX_GUIDANCE_LOOKUP_ITEMS
        {
            return Err(HookRuntimeErrorV1::InvalidLookup);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookGuidanceLookupOutcomeV1 {
    Ready(HookReadyGuidanceV1),
    NotReady,
    Paused,
    Unavailable,
}

pub trait HookGuidanceLookupPortV1 {
    fn lookup_ready(&self, request: HookGuidanceLookupRequestV1) -> HookGuidanceLookupOutcomeV1;
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

pub fn deliver_feedback_with_rollback<T, P>(
    rollback: HookFeedbackRollbackSwitchV1,
    feedback: &T,
    port: &P,
) -> Result<HookFeedbackDeliveryOutcomeV1, HookRuntimeErrorV1>
where
    P: HookFeedbackDeliveryPortV1<T> + ?Sized,
{
    if rollback.configuration_revision == 0 {
        return Err(HookRuntimeErrorV1::InvalidControl);
    }
    Ok(match rollback.route {
        HookFeedbackDeliveryRouteV1::HookV2 => port.deliver_hook_v2(feedback),
        HookFeedbackDeliveryRouteV1::Legacy => port.deliver_legacy(feedback),
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
    #[error("hook guidance lookup is not exact and bounded")]
    InvalidLookup,
    #[error("hook admission receipt timing is invalid")]
    InvalidAdmission,
    #[error("hook envelope does not satisfy the daemon-issued binding")]
    EnvelopeRejected(HookContractError),
}
