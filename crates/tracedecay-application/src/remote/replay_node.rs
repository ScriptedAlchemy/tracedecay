//! Node-owned replay orchestration over an authenticated remote transport.
//!
//! Only this side opens and mutates the encrypted offline spool. The authority
//! receives the sanitized frame value and never receives a local store handle.

use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, RemoteWriterFenceV1,
    UtcMicros,
};

use super::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, RemoteClockPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_replay_result_contract_v1,
};
use super::replay::{
    RemoteReplayCommitReceiptV1, RemoteReplayFindingV1, RemoteReplayFrameLookupPortV1,
    RemoteReplayFrameV1, RemoteReplayOutcomeV1, RemoteReplayRequestV1, RemoteReplaySpoolPortV1,
    RemoteReplayStateV1, RemoteReplayTransitionV1,
};
use crate::{ApplicationOutcome, RequestId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteNodeReplayCommandV1 {
    pub request_id: RequestId,
    pub event_id: String,
    pub expected_authority: RemoteWriterFenceV1,
}

pub trait RemoteReplayTransportPortV1: Send + Sync {
    fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteReplayOutcomeV1>, RemoteReplayTransportErrorV1>;
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayTransportErrorV1 {
    #[error("current remote authority is unreachable")]
    AuthorityUnreachable,
    #[error("remote replay transport is unavailable")]
    Unavailable,
    #[error("remote replay transport response is invalid")]
    InvalidResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteNodeReplayReceiptV1 {
    pub event_id: String,
    pub replay_attempt: u64,
    pub disposition: RemoteReplayStateV1,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub receipt: Option<RemoteReplayCommitReceiptV1>,
}

pub struct RemoteNodeReplayServiceV1 {
    frames: Arc<dyn RemoteReplayFrameLookupPortV1>,
    spool: Arc<dyn RemoteReplaySpoolPortV1>,
    transport: Arc<dyn RemoteReplayTransportPortV1>,
    clock: Arc<dyn RemoteClockPortV1>,
}

impl RemoteNodeReplayServiceV1 {
    pub fn new(
        frames: Arc<dyn RemoteReplayFrameLookupPortV1>,
        spool: Arc<dyn RemoteReplaySpoolPortV1>,
        transport: Arc<dyn RemoteReplayTransportPortV1>,
        clock: Arc<dyn RemoteClockPortV1>,
    ) -> Self {
        Self {
            frames,
            spool,
            transport,
            clock,
        }
    }

    pub fn replay(
        &self,
        command: RemoteNodeReplayCommandV1,
    ) -> Result<RemoteNodeReplayReceiptV1, RemoteNodeReplayErrorV1> {
        let frame = self
            .frames
            .load_replay_frame(&command.event_id)
            .map_err(RemoteNodeReplayErrorV1::Persistence)?;
        validate_command(&command, &frame)?;
        validate_predecessor(self.spool.as_ref(), &frame)?;

        let state = self
            .spool
            .state(&frame.event_id)
            .map_err(RemoteNodeReplayErrorV1::Persistence)?;
        if matches!(
            state.state,
            RemoteReplayStateV1::Admitted | RemoteReplayStateV1::Duplicate
        ) {
            let receipt = state
                .receipt
                .ok_or(RemoteNodeReplayErrorV1::InvalidSpoolState)?;
            validate_receipt(&receipt, &frame, &command.expected_authority)?;
            let replay_attempt = self.begin_attempt(&frame.event_id)?;
            let acknowledged_at = match self.now() {
                Ok(observed_at) => observed_at,
                Err(error) => {
                    self.abandon(&frame.event_id, replay_attempt)?;
                    return Err(error);
                }
            };
            let transition = local_transition(
                &frame,
                state.state,
                RemoteReplayStateV1::Acknowledged,
                replay_attempt,
                acknowledged_at,
                None,
                Some(receipt.clone()),
            );
            if let Err(error) = self.spool.transition(transition) {
                self.abandon(&frame.event_id, replay_attempt)?;
                return Err(RemoteNodeReplayErrorV1::Persistence(error));
            }
            return Ok(RemoteNodeReplayReceiptV1 {
                event_id: frame.event_id,
                replay_attempt,
                disposition: state.state,
                authority: CurrentRemoteAuthorityStateV1::Partial {
                    known_fence: Some(command.expected_authority),
                    missing: BTreeSet::from([
                        RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    ]),
                    observed_at: acknowledged_at,
                },
                receipt: Some(receipt),
            });
        }
        if state.state != RemoteReplayStateV1::Pending {
            return Err(RemoteNodeReplayErrorV1::InvalidSpoolState);
        }

        let sent_at = self.now()?;
        let replay_attempt = self
            .spool
            .begin_replay_attempt(&frame.event_id, sent_at)
            .map_err(RemoteNodeReplayErrorV1::Persistence)?;
        let request = RemoteProtocolRequestV1::new(
            command.request_id.clone(),
            frame.capture.writer.authority.fence.brain_id.clone(),
            frame.capture.node_id.clone(),
            frame.capture.enrollment_revision,
            command.expected_authority.clone(),
            sent_at,
            RemoteReplayRequestV1 {
                frame: frame.clone(),
                replay_attempt,
            },
        );
        let request = match request {
            Ok(request) => request,
            Err(_) => {
                self.abandon(&frame.event_id, replay_attempt)?;
                return Err(RemoteNodeReplayErrorV1::InvalidCommand);
            }
        };
        let response = match self.transport.replay(&request) {
            Ok(response) => response,
            Err(error) => {
                self.abandon(&frame.event_id, replay_attempt)?;
                return Err(RemoteNodeReplayErrorV1::Transport(error));
            }
        };
        let outcome = match validated_response(&request, response) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.abandon(&frame.event_id, replay_attempt)?;
                return Err(error);
            }
        };
        match self.apply_outcome(&frame, &command.expected_authority, replay_attempt, outcome) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.abandon(&frame.event_id, replay_attempt)?;
                Err(error)
            }
        }
    }

    fn apply_outcome(
        &self,
        frame: &RemoteReplayFrameV1,
        expected_authority: &RemoteWriterFenceV1,
        replay_attempt: u64,
        outcome: ValidatedRemoteReplayOutcomeV1,
    ) -> Result<RemoteNodeReplayReceiptV1, RemoteNodeReplayErrorV1> {
        let (disposition, receipt) = match outcome.outcome {
            RemoteReplayOutcomeV1::Acknowledged {
                disposition,
                receipt,
                operation_receipt,
            } => {
                operation_receipt
                    .validate()
                    .map_err(|_| RemoteNodeReplayErrorV1::InvalidResponse)?;
                if operation_receipt.replay_attempt != replay_attempt
                    || operation_receipt.transaction.as_ref() != Some(&receipt)
                    || !matches!(
                        disposition,
                        RemoteReplayStateV1::Admitted | RemoteReplayStateV1::Duplicate
                    )
                {
                    return Err(RemoteNodeReplayErrorV1::InvalidResponse);
                }
                validate_receipt(&receipt, frame, expected_authority)?;
                self.spool
                    .transition(local_transition(
                        frame,
                        RemoteReplayStateV1::Pending,
                        disposition,
                        replay_attempt,
                        self.now()?,
                        (disposition == RemoteReplayStateV1::Duplicate)
                            .then_some(RemoteReplayFindingV1::LostAcknowledgement),
                        Some(receipt.clone()),
                    ))
                    .map_err(RemoteNodeReplayErrorV1::Persistence)?;
                self.spool
                    .transition(local_transition(
                        frame,
                        disposition,
                        RemoteReplayStateV1::Acknowledged,
                        replay_attempt,
                        self.now()?,
                        None,
                        Some(receipt.clone()),
                    ))
                    .map_err(RemoteNodeReplayErrorV1::Persistence)?;
                (disposition, Some(receipt))
            }
            RemoteReplayOutcomeV1::Rejected { operation_receipt } => {
                validate_terminal_operation(&operation_receipt, replay_attempt)?;
                self.spool
                    .transition(local_transition(
                        frame,
                        RemoteReplayStateV1::Pending,
                        RemoteReplayStateV1::Rejected,
                        replay_attempt,
                        self.now()?,
                        Some(RemoteReplayFindingV1::PolicyChanged),
                        None,
                    ))
                    .map_err(RemoteNodeReplayErrorV1::Persistence)?;
                (RemoteReplayStateV1::Rejected, None)
            }
            RemoteReplayOutcomeV1::Quarantined { operation_receipt } => {
                validate_terminal_operation(&operation_receipt, replay_attempt)?;
                self.spool
                    .transition(local_transition(
                        frame,
                        RemoteReplayStateV1::Pending,
                        RemoteReplayStateV1::Quarantined,
                        replay_attempt,
                        self.now()?,
                        Some(RemoteReplayFindingV1::PolicyChanged),
                        None,
                    ))
                    .map_err(RemoteNodeReplayErrorV1::Persistence)?;
                (RemoteReplayStateV1::Quarantined, None)
            }
        };
        Ok(RemoteNodeReplayReceiptV1 {
            event_id: frame.event_id.clone(),
            replay_attempt,
            disposition,
            authority: outcome.authority,
            receipt,
        })
    }

    fn begin_attempt(&self, event_id: &str) -> Result<u64, RemoteNodeReplayErrorV1> {
        let observed_at = self.now()?;
        self.spool
            .begin_replay_attempt(event_id, observed_at)
            .map_err(RemoteNodeReplayErrorV1::Persistence)
    }

    fn abandon(&self, event_id: &str, replay_attempt: u64) -> Result<(), RemoteNodeReplayErrorV1> {
        self.spool
            .abandon_replay_attempt(event_id, replay_attempt)
            .map_err(RemoteNodeReplayErrorV1::Persistence)
    }

    fn now(&self) -> Result<UtcMicros, RemoteNodeReplayErrorV1> {
        self.clock
            .now()
            .map_err(|_| RemoteNodeReplayErrorV1::ClockUnavailable)
    }
}

struct ValidatedRemoteReplayOutcomeV1 {
    authority: CurrentRemoteAuthorityStateV1,
    outcome: RemoteReplayOutcomeV1,
}

fn validated_response(
    request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    response: RemoteProtocolResponseV1<RemoteReplayOutcomeV1>,
) -> Result<ValidatedRemoteReplayOutcomeV1, RemoteNodeReplayErrorV1> {
    if response.protocol_version != REMOTE_PROTOCOL_VERSION_V1
        || response.request_id != request.request_id
    {
        return Err(RemoteNodeReplayErrorV1::InvalidResponse);
    }
    let CurrentRemoteAuthorityStateV1::Available(authority) = &response.authority else {
        return Err(RemoteNodeReplayErrorV1::AuthorityUnavailable);
    };
    if authority.fence != request.expected_authority {
        return Err(RemoteNodeReplayErrorV1::AuthorityUnavailable);
    }
    let envelope = response
        .result
        .map_err(|_| RemoteNodeReplayErrorV1::RemoteProblem)?;
    if envelope.contract != remote_replay_result_contract_v1()
        || envelope.request_id != request.request_id
        || envelope.scope.project_id != request.body.frame.capture.writer.scope.project_id
        || envelope.scope.repository_id != request.body.frame.capture.writer.scope.repository_id
        || envelope.scope.worktree_id != request.body.frame.capture.writer.scope.worktree_id
        || envelope.scope.reference != request.body.frame.capture.writer.scope.reference
    {
        return Err(RemoteNodeReplayErrorV1::InvalidResponse);
    }
    let ApplicationOutcome::Effect(effect) = envelope.outcome else {
        return Err(RemoteNodeReplayErrorV1::InvalidResponse);
    };
    let outcome = effect
        .payload
        .ok_or(RemoteNodeReplayErrorV1::InvalidResponse)?;
    Ok(ValidatedRemoteReplayOutcomeV1 {
        authority: response.authority,
        outcome,
    })
}

fn validate_command(
    command: &RemoteNodeReplayCommandV1,
    frame: &RemoteReplayFrameV1,
) -> Result<(), RemoteNodeReplayErrorV1> {
    frame
        .validate()
        .map_err(|_| RemoteNodeReplayErrorV1::InvalidCommand)?;
    command
        .expected_authority
        .validate()
        .map_err(|_| RemoteNodeReplayErrorV1::InvalidCommand)?;
    if command.event_id != frame.event_id
        || !(command.expected_authority == frame.capture.writer.authority.fence
            || command
                .expected_authority
                .fences(&frame.capture.writer.authority.fence))
    {
        return Err(RemoteNodeReplayErrorV1::InvalidCommand);
    }
    Ok(())
}

fn validate_predecessor(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
) -> Result<(), RemoteNodeReplayErrorV1> {
    let Some(previous_event_id) = frame.capture.sequence.previous_event_id.as_deref() else {
        return Ok(());
    };
    let predecessor = spool
        .state(previous_event_id)
        .map_err(RemoteNodeReplayErrorV1::Persistence)?;
    if !matches!(
        predecessor.state,
        RemoteReplayStateV1::Acknowledged | RemoteReplayStateV1::GarbageCollectionEligible
    ) {
        return Err(RemoteNodeReplayErrorV1::InvalidSpoolState);
    }
    Ok(())
}

fn validate_receipt(
    receipt: &RemoteReplayCommitReceiptV1,
    frame: &RemoteReplayFrameV1,
    expected_authority: &RemoteWriterFenceV1,
) -> Result<(), RemoteNodeReplayErrorV1> {
    if receipt.event_id != frame.event_id
        || &receipt.writer_fence != expected_authority
        || receipt.commit_sequence == 0
        || receipt.committed_at < frame.capture.captured_at
    {
        return Err(RemoteNodeReplayErrorV1::InvalidResponse);
    }
    Ok(())
}

fn validate_terminal_operation(
    receipt: &super::replay::RemoteReplayOperationReceiptV1,
    replay_attempt: u64,
) -> Result<(), RemoteNodeReplayErrorV1> {
    receipt
        .validate()
        .map_err(|_| RemoteNodeReplayErrorV1::InvalidResponse)?;
    if receipt.replay_attempt != replay_attempt || receipt.transaction.is_some() {
        return Err(RemoteNodeReplayErrorV1::InvalidResponse);
    }
    Ok(())
}

fn local_transition(
    frame: &RemoteReplayFrameV1,
    from: RemoteReplayStateV1,
    to: RemoteReplayStateV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
    finding: Option<RemoteReplayFindingV1>,
    receipt: Option<RemoteReplayCommitReceiptV1>,
) -> RemoteReplayTransitionV1 {
    RemoteReplayTransitionV1 {
        event_id: frame.event_id.clone(),
        from,
        to,
        replay_attempt,
        observed_at,
        finding,
        receipt,
    }
}

pub fn mark_remote_capture_gc_eligible(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
    receipt: RemoteReplayCommitReceiptV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteNodeReplayErrorV1> {
    let state = spool
        .state(&frame.event_id)
        .map_err(RemoteNodeReplayErrorV1::Persistence)?;
    if state.state != RemoteReplayStateV1::Acknowledged || state.receipt.as_ref() != Some(&receipt)
    {
        return Err(RemoteNodeReplayErrorV1::InvalidSpoolState);
    }
    let replay_attempt = spool
        .begin_replay_attempt(&frame.event_id, observed_at)
        .map_err(RemoteNodeReplayErrorV1::Persistence)?;
    let transition = local_transition(
        frame,
        RemoteReplayStateV1::Acknowledged,
        RemoteReplayStateV1::GarbageCollectionEligible,
        replay_attempt,
        observed_at,
        None,
        Some(receipt),
    );
    if let Err(error) = spool.transition(transition) {
        spool
            .abandon_replay_attempt(&frame.event_id, replay_attempt)
            .map_err(RemoteNodeReplayErrorV1::Persistence)?;
        return Err(RemoteNodeReplayErrorV1::Persistence(error));
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteNodeReplayErrorV1 {
    #[error("remote node replay command is invalid")]
    InvalidCommand,
    #[error("remote node replay clock is unavailable")]
    ClockUnavailable,
    #[error("remote node spool state is invalid")]
    InvalidSpoolState,
    #[error("remote authority is unavailable")]
    AuthorityUnavailable,
    #[error("remote authority returned an application problem")]
    RemoteProblem,
    #[error("remote replay response is invalid")]
    InvalidResponse,
    #[error(transparent)]
    Persistence(super::capture::RemoteCapturePersistenceErrorV1),
    #[error(transparent)]
    Transport(RemoteReplayTransportErrorV1),
}
