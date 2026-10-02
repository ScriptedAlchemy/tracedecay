//! Root adapters for native Hook daemon transport.
//!
//! These ports own the `daemon_hook_action` JSON. The native Hook dispatch core
//! consumes typed ports ([`AsyncHookAdmissionPortV1`],
//! [`AsyncHookFeedbackDeliveryPortV1`], and the `OpenCode` LSP submit port) and
//! never issues those action strings itself.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use tracedecay_contracts::context_scout::{ContextScoutAddressV1, ContextScoutDeliveryReceiptV1};
use tracedecay_contracts::retrieval::{
    ContextScoutStoreStatusV1, HookRuntimeDispositionV1, HookRuntimeResultV1,
    HookRuntimeSurfaceRequestV1, HookV2AdmissionResultV1, HookV2AdmitRequestV1,
    HookV2NoticeDeliveryResultV1,
};
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{
    AsyncHookAdmissionPortV1, AsyncHookFeedbackDeliveryPortV1, HookAdmissionFutureV1,
    HookDeliveryFutureV1, HookEventEnvelopeV2, HookFeedbackDeliveryOutcomeV1,
    HookImmediateAdmissionV1, HookReadyGuidanceV1, HookSynchronousDeadlineV1,
};

use crate::agents::context_scout::ContextScoutDeliveryReceiptHookV1;
use crate::ports::hook_runtime::HookRuntimeV1;

use super::analytics::HookTimingSpan;
use tracedecay_hooks::NativeContextScoutLifecycleV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DaemonAdmissionRetentionUnavailable;

pub(crate) struct DaemonAdmissionPort<'a> {
    runtime: &'a HookRuntimeV1,
    project_root: &'a Path,
    session_id: Option<&'a str>,
    lifecycle: Option<&'a NativeContextScoutLifecycleV1>,
    context_scout_address: Mutex<Option<ContextScoutAddressV1>>,
    feedback_notice: Mutex<Option<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>>,
    github_stack_signal_available: Mutex<bool>,
    /// The caller's hook span, so the admission round trip is attributed like
    /// every other hook/daemon call. Passing `None` here reported hosts that
    /// route through the native dispatcher as having done no daemon IPC at all.
    telemetry: Option<&'a HookTimingSpan>,
}

impl<'a> DaemonAdmissionPort<'a> {
    pub(crate) fn new(
        runtime: &'a HookRuntimeV1,
        project_root: &'a Path,
        session_id: Option<&'a str>,
        lifecycle: Option<&'a NativeContextScoutLifecycleV1>,
        telemetry: Option<&'a HookTimingSpan>,
    ) -> Self {
        Self {
            runtime,
            project_root,
            session_id,
            lifecycle,
            context_scout_address: Mutex::new(None),
            feedback_notice: Mutex::new(None),
            github_stack_signal_available: Mutex::new(false),
            telemetry,
        }
    }

    pub(crate) fn take_context_scout_address(
        &self,
    ) -> Result<Option<ContextScoutAddressV1>, DaemonAdmissionRetentionUnavailable> {
        self.context_scout_address
            .lock()
            .map_err(|_| DaemonAdmissionRetentionUnavailable)
            .map(|mut address| address.take())
    }

    pub(crate) fn take_feedback_notice(
        &self,
    ) -> Option<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1> {
        self.feedback_notice
            .lock()
            .ok()
            .and_then(|mut notice| notice.take())
    }

    /// An actor-less Hook V2 admission can carry only this opaque availability
    /// wakeup. It never constitutes recipient acknowledgement.
    pub(crate) fn take_github_stack_signal_available(&self) -> bool {
        self.github_stack_signal_available
            .lock()
            .is_ok_and(|mut available| std::mem::take(&mut *available))
    }
}

pub(crate) struct DaemonAdmissionResponseV1 {
    pub(crate) immediate: HookImmediateAdmissionV1,
    pub(crate) context_scout_address: Option<ContextScoutAddressV1>,
    pub(crate) feedback_notice:
        Option<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>,
    pub(crate) github_stack_signal_available: bool,
}

pub(crate) fn now_utc() -> UtcMicros {
    UtcMicros(
        tracedecay_runtime_core::tracedecay::saturating_utc_now()
            .0
            .max(1),
    )
}

#[hotpath::measure(label = "agent_hosts.hook_ports.admission_decode")]
pub(crate) fn daemon_admission_response(response: &serde_json::Value) -> DaemonAdmissionResponseV1 {
    let unavailable = || DaemonAdmissionResponseV1 {
        immediate: HookImmediateAdmissionV1::Unavailable,
        context_scout_address: None,
        feedback_notice: None,
        github_stack_signal_available: false,
    };
    let Ok(HookRuntimeResultV1::HookV2Admit(admission)) =
        HookRuntimeResultV1::deserialize(response)
    else {
        return unavailable();
    };
    let (ready_guidance, context_scout_address, feedback_notice, github_stack_signal_available) =
        match admission {
            HookV2AdmissionResultV1::Rejected {
                disposition: HookRuntimeDispositionV1::CatchupRequired,
                ..
            } => {
                return DaemonAdmissionResponseV1 {
                    immediate: HookImmediateAdmissionV1::CatchupRequired,
                    context_scout_address: None,
                    feedback_notice: None,
                    github_stack_signal_available: false,
                };
            }
            HookV2AdmissionResultV1::Backpressured {} => {
                return DaemonAdmissionResponseV1 {
                    immediate: HookImmediateAdmissionV1::Backpressured,
                    context_scout_address: None,
                    feedback_notice: None,
                    github_stack_signal_available: false,
                };
            }
            HookV2AdmissionResultV1::Accepted {
                disposition: HookRuntimeDispositionV1::Accepted,
                context_scout_address,
                ready_guidance,
                feedback_notice,
                github_stack_signal_available,
                ..
            } => (
                ready_guidance,
                context_scout_address,
                feedback_notice,
                github_stack_signal_available,
            ),
            HookV2AdmissionResultV1::ExactDuplicate {
                disposition: HookRuntimeDispositionV1::Accepted,
                context_scout_address,
                ready_guidance,
            } => (ready_guidance, context_scout_address, None, false),
            HookV2AdmissionResultV1::Accepted { .. }
            | HookV2AdmissionResultV1::ExactDuplicate { .. }
            | HookV2AdmissionResultV1::Rejected { .. }
            | HookV2AdmissionResultV1::Unavailable {} => return unavailable(),
        };
    let Ok(ready_guidance) = ready_guidance
        .map(serde_json::from_value::<HookReadyGuidanceV1>)
        .transpose()
    else {
        return unavailable();
    };
    let Ok(feedback_notice) = feedback_notice
        .map(serde_json::from_value::<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>)
        .transpose()
    else {
        return unavailable();
    };
    if feedback_notice
        .as_ref()
        .is_some_and(|notice| notice.validate().is_err())
    {
        return unavailable();
    }
    DaemonAdmissionResponseV1 {
        immediate: HookImmediateAdmissionV1::Accepted {
            admitted_at: now_utc(),
            ready_guidance,
        },
        context_scout_address,
        feedback_notice,
        github_stack_signal_available,
    }
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
            let Ok(native_lifecycle) = self.lifecycle.map(serde_json::to_value).transpose() else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            let response = tokio::time::timeout(
                Duration::from_micros(deadline.remaining_micros()),
                super::daemon_hook_action(
                    self.runtime,
                    Some(self.project_root),
                    HookRuntimeSurfaceRequestV1::HookV2Admit(HookV2AdmitRequestV1 {
                        envelope,
                        native_session_id: self.session_id.map(str::to_owned),
                        native_lifecycle,
                    }),
                    self.telemetry,
                ),
            )
            .await;
            let Ok(Ok(response)) = response else {
                return HookImmediateAdmissionV1::Unavailable;
            };
            let response = daemon_admission_response(&response);
            if let Some(address) = response.context_scout_address {
                let Ok(mut retained) = self.context_scout_address.lock() else {
                    return HookImmediateAdmissionV1::Unavailable;
                };
                *retained = Some(address);
            }
            if let Some(notice) = response.feedback_notice
                && let Ok(mut retained) = self.feedback_notice.lock()
            {
                *retained = Some(notice);
            }
            if response.github_stack_signal_available
                && let Ok(mut retained) = self.github_stack_signal_available.lock()
            {
                *retained = true;
            }
            response.immediate
        })
    }
}

fn delivery_outcome(response: &serde_json::Value) -> HookFeedbackDeliveryOutcomeV1 {
    match HookRuntimeResultV1::deserialize(response) {
        Ok(
            HookRuntimeResultV1::HookV2DeliveryReceipt {
                status: ContextScoutStoreStatusV1::Stored,
            }
            | HookRuntimeResultV1::HookV2FeedbackNoticeDelivery(
                HookV2NoticeDeliveryResultV1::Stored {},
            ),
        ) => HookFeedbackDeliveryOutcomeV1::Delivered,
        // Exact-address commits that lost a compare-and-swap still prove the
        // daemon retained an authoritative row for this receipt/feedback.
        Ok(HookRuntimeResultV1::HookV2DeliveryReceipt {
            status: ContextScoutStoreStatusV1::Duplicate | ContextScoutStoreStatusV1::Superseded,
        }) => HookFeedbackDeliveryOutcomeV1::Duplicate,
        _ => HookFeedbackDeliveryOutcomeV1::Unavailable,
    }
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_ports.timed_daemon_action")]
async fn timed_daemon_hook_action(
    runtime: &HookRuntimeV1,
    project_root: &Path,
    action: HookRuntimeSurfaceRequestV1,
    deadline: HookSynchronousDeadlineV1,
    telemetry: Option<&HookTimingSpan>,
) -> HookFeedbackDeliveryOutcomeV1 {
    let response = tokio::time::timeout(
        Duration::from_micros(deadline.remaining_micros()),
        super::daemon_hook_action(runtime, Some(project_root), action, telemetry),
    )
    .await;
    let Ok(Ok(response)) = response else {
        return HookFeedbackDeliveryOutcomeV1::Unavailable;
    };
    delivery_outcome(&response)
}

/// Daemon-backed Hook feedback-notice delivery. Acknowledgement crosses the
/// local daemon boundary; finding content stays in the feedback publication store.
pub(crate) struct DaemonFeedbackNoticeDeliveryPort<'a> {
    runtime: &'a HookRuntimeV1,
    project_root: &'a Path,
}

impl<'a> DaemonFeedbackNoticeDeliveryPort<'a> {
    pub(crate) fn new(runtime: &'a HookRuntimeV1, project_root: &'a Path) -> Self {
        Self {
            runtime,
            project_root,
        }
    }
}

impl AsyncHookFeedbackDeliveryPortV1<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>
    for DaemonFeedbackNoticeDeliveryPort<'_>
{
    fn deliver_hook_v2<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        feedback: &'a tracedecay_application::advisory::AdvisoryHookLookupNoticeV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move {
            let (Ok(envelope), Ok(feedback_notice)) = (
                serde_json::to_value(envelope),
                serde_json::to_value(feedback),
            ) else {
                return HookFeedbackDeliveryOutcomeV1::Unavailable;
            };
            timed_daemon_hook_action(
                self.runtime,
                self.project_root,
                HookRuntimeSurfaceRequestV1::HookV2FeedbackNoticeDelivery {
                    envelope,
                    feedback_notice,
                },
                deadline,
                None,
            )
            .await
        })
    }
}

/// Daemon-backed Context Scout delivery-receipt commit.
pub(crate) struct DaemonDeliveryReceiptPort<'a> {
    runtime: &'a HookRuntimeV1,
    project_root: &'a Path,
}

impl<'a> DaemonDeliveryReceiptPort<'a> {
    pub(crate) fn new(runtime: &'a HookRuntimeV1, project_root: &'a Path) -> Self {
        Self {
            runtime,
            project_root,
        }
    }

    #[hotpath::measure(future = true, label = "agent_hosts.hook_ports.post_receipt")]
    pub(crate) async fn post_receipt(
        &self,
        receipt: &ContextScoutDeliveryReceiptV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookFeedbackDeliveryOutcomeV1 {
        let Ok(receipt) = serde_json::to_value(receipt) else {
            return HookFeedbackDeliveryOutcomeV1::Unavailable;
        };
        timed_daemon_hook_action(
            self.runtime,
            self.project_root,
            HookRuntimeSurfaceRequestV1::HookV2DeliveryReceipt { receipt },
            deadline,
            None,
        )
        .await
    }
}

impl AsyncHookFeedbackDeliveryPortV1<ContextScoutDeliveryReceiptHookV1>
    for DaemonDeliveryReceiptPort<'_>
{
    fn deliver_hook_v2<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        feedback: &'a ContextScoutDeliveryReceiptHookV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move { self.post_receipt(&feedback.receipt, deadline).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_outcome_maps_superseded_as_duplicate() {
        for (response, outcome) in [
            (
                serde_json::json!({"action": "hook_v2_delivery_receipt", "status": "superseded"}),
                HookFeedbackDeliveryOutcomeV1::Duplicate,
            ),
            (
                serde_json::json!({"action": "hook_v2_delivery_receipt", "status": "stored"}),
                HookFeedbackDeliveryOutcomeV1::Delivered,
            ),
            (
                serde_json::json!({"action": "hook_v2_feedback_notice_delivery", "status": "stored"}),
                HookFeedbackDeliveryOutcomeV1::Delivered,
            ),
            (
                serde_json::json!({"action": "hook_v2_delivery_receipt", "status": "unavailable"}),
                HookFeedbackDeliveryOutcomeV1::Unavailable,
            ),
            (
                serde_json::json!({"status": "stored"}),
                HookFeedbackDeliveryOutcomeV1::Unavailable,
            ),
        ] {
            assert_eq!(delivery_outcome(&response), outcome, "{response}");
        }
    }
}
