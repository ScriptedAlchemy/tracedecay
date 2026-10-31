//! Durable delivery identity and terminalization for daemon Work responses.

use std::path::Path;
use std::sync::Arc;

use super::*;
use tracedecay_daemon_protocol::DaemonHandshake;

pub struct DaemonWorkDeliveryDescriptorV1 {
    owner_event_id: String,
    channel_ref: String,
    valid_at: tracedecay_domain::UtcMicros,
    event_class: tracedecay_domain::DeliveryEventClassV1,
    kind: DaemonWorkDeliveryKindV1,
    attempt_identity: Option<tracedecay_domain::WorkAttemptIdentityV1>,
}

#[derive(Clone, Copy)]
enum DaemonWorkDeliveryKindV1 {
    Attempt,
    ArtifactPage,
}

impl DaemonWorkDeliveryDescriptorV1 {
    pub fn from_request(
        request: &DaemonInvocationRequest,
        handshake: &DaemonHandshake,
    ) -> Option<Self> {
        let DaemonInvocationPayload::WorkApplication {
            request: work_request,
            observed_at,
            ..
        } = &request.payload
        else {
            return None;
        };
        let observed_at = *observed_at;
        let (operation, event_class, kind, attempt_identity) = match work_request.as_ref() {
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::StartAttempt(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::AttemptStatus(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::CancelAttempt(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::HydrateArtifacts(_) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::Activity,
                DaemonWorkDeliveryKindV1::ArtifactPage,
                None,
            ),
            _ => return None,
        };
        let project = handshake.project_path.as_ref()?.to_string_lossy();
        let owner = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon-work-delivery.v1",
            project.as_ref(),
            request.request_id.as_str(),
            operation,
            observed_at,
        ))
        .ok()?;
        let channel = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon-work-channel.v1",
            project.as_ref(),
            handshake.client_instance_id.as_str(),
        ))
        .ok()?;
        Some(Self {
            owner_event_id: format!(
                "work:daemon-response:{}",
                owner.as_str().trim_start_matches("sha256:")
            ),
            channel_ref: format!(
                "cli:daemon:{}",
                channel.as_str().trim_start_matches("sha256:")
            ),
            valid_at: observed_at,
            event_class,
            kind,
            attempt_identity,
        })
    }

    pub fn is_successful_delivery(&self, response: &DaemonInvocationResponse) -> bool {
        use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

        match (&self.kind, &response.outcome) {
            (
                DaemonWorkDeliveryKindV1::Attempt,
                DaemonInvocationOutcome::WorkApplication {
                    outcome:
                        WorkApplicationOutcomeV1::StartAttempt(outcome)
                        | WorkApplicationOutcomeV1::AttemptStatus(outcome)
                        | WorkApplicationOutcomeV1::CancelAttempt(outcome),
                    ..
                },
            ) => outcome.payload().is_some(),
            (
                DaemonWorkDeliveryKindV1::ArtifactPage,
                DaemonInvocationOutcome::WorkApplication {
                    outcome: WorkApplicationOutcomeV1::HydrateArtifacts(outcome),
                    ..
                },
            ) => outcome.payload().is_some_and(|hydration| {
                matches!(
                    hydration,
                    tracedecay_contracts::WorkArtifactHydrationV1::Hydrated { attempts, .. }
                        if !attempts.is_empty()
                )
            }),
            _ => false,
        }
    }

    pub async fn attempts(
        self,
        service: &DaemonInvocationService,
        project_root: Option<&Path>,
        response: &DaemonInvocationResponse,
    ) -> Vec<tracedecay_domain::DeliverySettlementAttemptV1> {
        let identities = self.attempt_identities(response);
        if identities.is_empty() {
            return vec![self.settlement_attempt(self.owner_event_id.clone(), None)];
        }
        let mut attempts = Vec::with_capacity(identities.len());
        for identity in identities {
            let binding = service.work_fan_out_binding(project_root, &identity).await;
            let Ok(owner) = tracedecay_domain::canonical_sha256(&(
                "tracedecay.daemon-work-fan-out-delivery.v1",
                self.owner_event_id.as_str(),
                &identity,
                binding.as_ref(),
            )) else {
                continue;
            };
            attempts.push(self.settlement_attempt(
                format!(
                    "work:fan-out-response:{}",
                    owner.as_str().trim_start_matches("sha256:")
                ),
                Some(identity),
            ));
        }
        attempts
    }

    fn settlement_attempt(
        &self,
        owner_event_id: String,
        work_attempt: Option<tracedecay_domain::WorkAttemptIdentityV1>,
    ) -> tracedecay_domain::DeliverySettlementAttemptV1 {
        let attempted_at = std::cmp::max(self.valid_at, tracedecay_contracts::clock::now_micros());
        tracedecay_domain::DeliverySettlementAttemptV1 {
            owner_event_id,
            event_class: self.event_class,
            channel: tracedecay_domain::DeliveryChannelIdentityV1 {
                surface: tracedecay_domain::DeliverySurfaceFamilyV1::Cli,
                channel_ref: self.channel_ref.clone(),
            },
            work_attempt,
            eligible: 1,
            valid_at: self.valid_at,
            attempted_at,
        }
    }

    fn attempt_identities(
        &self,
        response: &DaemonInvocationResponse,
    ) -> Vec<tracedecay_domain::WorkAttemptIdentityV1> {
        use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

        if let Some(identity) = self.attempt_identity.as_ref() {
            return vec![identity.clone()];
        }
        let DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::HydrateArtifacts(outcome),
            ..
        } = &response.outcome
        else {
            return Vec::new();
        };
        let Some(tracedecay_contracts::WorkArtifactHydrationV1::Hydrated { attempts, .. }) =
            outcome.payload()
        else {
            return Vec::new();
        };
        attempts
            .iter()
            .map(|attempt| attempt.identity.clone())
            .collect()
    }
}

fn offer_daemon_work_delivery(
    recorder: Option<
        &Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
    >,
    attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
    drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
) -> std::result::Result<(), tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason> {
    let (Some(recorder), Some(attempt)) = (recorder, attempt) else {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    };
    let settlement = tracedecay_domain::DeliverySettlementV1 {
        settled_at: std::cmp::max(
            attempt.attempted_at,
            tracedecay_contracts::clock::now_micros(),
        ),
        attempt,
        outcome,
        drop_reason,
    };
    match recorder.try_record(settlement) {
        Ok(tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {
            Ok(())
        }
        Ok(tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity) => {
            tracing::warn!("daemon Work delivery receipt was dropped at recorder capacity");
            Err(
                tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderAtCapacity,
            )
        }
        Err(error) => {
            tracing::warn!(%error, "daemon Work delivery receipt was refused");
            Err(
                tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
            )
        }
    }
}

/// Settle the exact attempts resolved immediately before the daemon response
/// was written.  In particular, do not resolve fan-out bindings again when a
/// client ACK arrives: the Work response and its receipt must share one
/// immutable identity even if the workflow owner changes in the meantime.
/// `attempted_at` is response-write-adjacent; `settled_at` is stamped when
/// this terminal ACK is observed by the daemon.
pub fn settle_daemon_work_delivery(
    attempts: Option<&[tracedecay_domain::DeliverySettlementAttemptV1]>,
    recorder: Option<
        &Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
    >,
    outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
    drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
) -> std::result::Result<(), tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason> {
    let Some(attempts) = attempts else {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    };
    if attempts.is_empty() {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    }
    let mut result = Ok(());
    for attempt in attempts {
        if let Err(error) =
            offer_daemon_work_delivery(recorder, Some(attempt.clone()), outcome, drop_reason)
        {
            result = Err(error);
        }
    }
    result
}
