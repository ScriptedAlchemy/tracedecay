//! Root adapters for Hook V2 daemon transport.
//!
//! These ports own the `daemon_hook_action` JSON. The Hook V2 dispatch core
//! consumes typed ports ([`AsyncHookAdmissionPortV1`],
//! [`AsyncHookFeedbackDeliveryPortV1`], and the `OpenCode` LSP submit port) and
//! never issues those action strings itself.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{
    AsyncHookAdmissionPortV1, AsyncHookFeedbackDeliveryPortV1, HookAdmissionFutureV1,
    HookDeliveryFutureV1, HookEventEnvelopeV2, HookFeedbackDeliveryOutcomeV1,
    HookImmediateAdmissionV1, HookReadyGuidanceV1, HookSynchronousDeadlineV1,
    HookTransportDispositionV1,
};

use super::analytics::HookTimingSpan;
use super::v2::NativeContextScoutLifecycleV1;

pub(crate) struct DaemonAdmissionPort<'a> {
    project_root: &'a Path,
    session_id: Option<&'a str>,
    lifecycle: Option<&'a NativeContextScoutLifecycleV1>,
    feedback_notice: Mutex<Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>>,
    /// The caller's hook span, so the admission round trip is attributed like
    /// every other hook/daemon call. Passing `None` here reported hosts that
    /// route through V2 as having done no daemon IPC at all.
    telemetry: Option<&'a HookTimingSpan>,
}

impl<'a> DaemonAdmissionPort<'a> {
    pub(crate) fn new(
        project_root: &'a Path,
        session_id: Option<&'a str>,
        lifecycle: Option<&'a NativeContextScoutLifecycleV1>,
        telemetry: Option<&'a HookTimingSpan>,
    ) -> Self {
        Self {
            project_root,
            session_id,
            lifecycle,
            feedback_notice: Mutex::new(None),
            telemetry,
        }
    }

    pub(crate) fn take_feedback_notice(
        &self,
    ) -> Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1> {
        self.feedback_notice
            .lock()
            .ok()
            .and_then(|mut notice| notice.take())
    }
}

pub(crate) struct DaemonAdmissionResponseV1 {
    pub(crate) immediate: HookImmediateAdmissionV1,
    pub(crate) feedback_notice:
        Option<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>,
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

pub(crate) fn now_utc() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros.max(1))
}

pub(crate) fn daemon_admission_response(response: &serde_json::Value) -> DaemonAdmissionResponseV1 {
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
                    self.telemetry,
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

fn delivery_outcome_from_status(status: Option<&str>) -> HookFeedbackDeliveryOutcomeV1 {
    match status {
        Some("stored") => HookFeedbackDeliveryOutcomeV1::Delivered,
        // Exact-address commits that lost a compare-and-swap still prove the
        // daemon retained an authoritative row for this receipt/feedback.
        Some("duplicate" | "superseded") => HookFeedbackDeliveryOutcomeV1::Duplicate,
        _ => HookFeedbackDeliveryOutcomeV1::Unavailable,
    }
}

async fn timed_daemon_hook_action(
    project_root: &Path,
    action: serde_json::Value,
    deadline: HookSynchronousDeadlineV1,
    telemetry: Option<&HookTimingSpan>,
) -> HookFeedbackDeliveryOutcomeV1 {
    let response = tokio::time::timeout(
        Duration::from_micros(deadline.remaining_micros()),
        super::daemon_hook_action(Some(project_root), action, telemetry),
    )
    .await;
    let Ok(Ok(response)) = response else {
        return HookFeedbackDeliveryOutcomeV1::Unavailable;
    };
    delivery_outcome_from_status(response.get("status").and_then(|value| value.as_str()))
}

/// Daemon-backed Hook V2 feedback-notice delivery. Acknowledgement crosses the
/// local daemon boundary; finding content stays in the PR12 store.
pub(crate) struct DaemonFeedbackNoticeDeliveryPort<'a> {
    project_root: &'a Path,
}

impl<'a> DaemonFeedbackNoticeDeliveryPort<'a> {
    pub(crate) fn new(project_root: &'a Path) -> Self {
        Self { project_root }
    }
}

impl AsyncHookFeedbackDeliveryPortV1<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>
    for DaemonFeedbackNoticeDeliveryPort<'_>
{
    fn deliver_hook_v2<'a>(
        &'a self,
        envelope: &'a HookEventEnvelopeV2,
        feedback: &'a crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move {
            timed_daemon_hook_action(
                self.project_root,
                serde_json::json!({
                    "action": "hook_v2_feedback_notice_delivery",
                    "envelope": envelope,
                    "feedback_notice": feedback,
                }),
                deadline,
                None,
            )
            .await
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

/// Daemon-backed Context Scout delivery-receipt commit.
pub(crate) struct DaemonDeliveryReceiptPort<'a> {
    project_root: &'a Path,
}

impl<'a> DaemonDeliveryReceiptPort<'a> {
    pub(crate) fn new(project_root: &'a Path) -> Self {
        Self { project_root }
    }

    pub(crate) async fn post_receipt(
        &self,
        receipt: &crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookFeedbackDeliveryOutcomeV1 {
        timed_daemon_hook_action(
            self.project_root,
            serde_json::json!({
                "action": "hook_v2_delivery_receipt",
                "receipt": receipt,
            }),
            deadline,
            None,
        )
        .await
    }
}

impl AsyncHookFeedbackDeliveryPortV1<crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1>
    for DaemonDeliveryReceiptPort<'_>
{
    fn deliver_hook_v2<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        feedback: &'a crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move { self.post_receipt(feedback, deadline).await })
    }

    fn deliver_legacy<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        _feedback: &'a crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async { HookFeedbackDeliveryOutcomeV1::Unavailable })
    }
}

/// Typed Context Scout explicit-feedback payload for envelope-bound delivery.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextScoutFeedbackCommitV1 {
    pub receipt: crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    pub feedback: crate::agents::context_scout_v2::ContextScoutFeedbackV1,
}

/// Daemon-backed Context Scout explicit-feedback commit.
#[cfg(test)]
pub(crate) struct DaemonContextScoutFeedbackPort<'a> {
    project_root: &'a Path,
}

#[cfg(test)]
impl<'a> DaemonContextScoutFeedbackPort<'a> {
    pub(crate) fn new(project_root: &'a Path) -> Self {
        Self { project_root }
    }

    pub(crate) async fn post_feedback(
        &self,
        receipt: &crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
        feedback: &crate::agents::context_scout_v2::ContextScoutFeedbackV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookFeedbackDeliveryOutcomeV1 {
        timed_daemon_hook_action(
            self.project_root,
            serde_json::json!({
                "action": "hook_v2_feedback",
                "receipt": receipt,
                "feedback": feedback,
            }),
            deadline,
            None,
        )
        .await
    }
}

#[cfg(test)]
impl AsyncHookFeedbackDeliveryPortV1<ContextScoutFeedbackCommitV1>
    for DaemonContextScoutFeedbackPort<'_>
{
    fn deliver_hook_v2<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        feedback: &'a ContextScoutFeedbackCommitV1,
        deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move {
            self.post_feedback(&feedback.receipt, &feedback.feedback, deadline)
                .await
        })
    }

    fn deliver_legacy<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        _feedback: &'a ContextScoutFeedbackCommitV1,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async { HookFeedbackDeliveryOutcomeV1::Unavailable })
    }
}

/// Daemon-backed `OpenCode` LSP update submission.
pub(crate) struct DaemonOpenCodeLspUpdatePort<'a> {
    project_root: &'a Path,
    telemetry: Option<&'a HookTimingSpan>,
}

impl<'a> DaemonOpenCodeLspUpdatePort<'a> {
    pub(crate) fn new(project_root: &'a Path, telemetry: Option<&'a HookTimingSpan>) -> Self {
        Self {
            project_root,
            telemetry,
        }
    }

    pub(crate) async fn submit_updated_event(&self, event: &serde_json::Value) -> bool {
        let response = super::daemon_hook_action(
            Some(self.project_root),
            serde_json::json!({
                "action": "opencode_lsp_updated",
                "event": event,
            }),
            self.telemetry,
        )
        .await;
        response
            .ok()
            .and_then(|value| {
                value
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|status| status == "accepted")
    }
}

#[cfg(test)]
pub(crate) fn outcome_is_committed(outcome: HookFeedbackDeliveryOutcomeV1) -> bool {
    matches!(
        outcome,
        HookFeedbackDeliveryOutcomeV1::Delivered | HookFeedbackDeliveryOutcomeV1::Duplicate
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::feedback::{FeedbackCycleId, FeedbackResultId, FeedbackScopeV1};
    use tracedecay_domain::{
        CodeGenerationId, CommitId, ManifestDigest, ProjectId, RepositoryId, WorktreeId,
    };

    #[test]
    fn delivery_outcome_maps_superseded_as_duplicate() {
        assert_eq!(
            delivery_outcome_from_status(Some("superseded")),
            HookFeedbackDeliveryOutcomeV1::Duplicate
        );
        assert_eq!(
            delivery_outcome_from_status(Some("stored")),
            HookFeedbackDeliveryOutcomeV1::Delivered
        );
        assert_eq!(
            delivery_outcome_from_status(Some("unavailable")),
            HookFeedbackDeliveryOutcomeV1::Unavailable
        );
    }

    #[test]
    fn daemon_feedback_notice_survives_admission_decode() {
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
        assert_eq!(admitted.feedback_notice, Some(notice));
    }
}
