//! Guided replay: every frame a node's drain left at the authority, replayed
//! in sequence order through the single-frame replay service.
//!
//! The answer is always projected from the durable spool rows, so a resend
//! after any partial or complete step reports the same frames and commits
//! nothing twice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, UtcMicros,
};
use tracedecay_tool_catalog::SchemaId;

use super::{
    OpaqueRemoteCredential, REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolBodyV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1, RemoteReplayApplicationErrorV1,
    RemoteReplayCommitReceiptV1, RemoteReplayFindingV1, RemoteReplayOperationReceiptV1,
    RemoteReplayOutcomeV1, RemoteReplayProtocolAdapterV1, RemoteReplayRequestV1,
    RemoteReplayServiceErrorV1, RemoteReplayServiceV1, RemoteReplayStateV1,
    replay_failure_response,
};
use crate::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, RequestId, ResolvedScope,
    ResultContractRef,
};

/// Replay every frame of the caller's enrollment that the authority holds,
/// lowest sequence first. The caller is the request's session-bound
/// `caller_node_id` and `enrollment_revision`; `expected_authority` is
/// required and fences the whole step.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayPendingRequestV1 {}

impl RemoteProtocolBodyV1 for RemoteReplayPendingRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayPendingReceiptV1 {
    /// Acknowledged frames, in sequence order, up to the first frame that is not.
    pub acknowledged: Vec<RemoteReplayPendingFrameV1>,
    /// The first frame the authority refused terminally. Later frames stay pending.
    pub stopped_at: Option<RemoteReplayPendingStopV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayPendingFrameV1 {
    pub sequence: u64,
    pub receipt: RemoteReplayCommitReceiptV1,
    /// This request's replay receipt; `None` when an earlier replay acknowledged the frame.
    pub replayed: Option<RemoteReplayOperationReceiptV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayPendingStopV1 {
    pub event_id: String,
    pub sequence: u64,
    pub refusal: RemoteReplayRefusalV1,
    pub finding: RemoteReplayFindingV1,
    /// This request's replay receipt; `None` when an earlier replay refused the frame.
    pub replayed: Option<RemoteReplayOperationReceiptV1>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReplayRefusalV1 {
    Rejected,
    Quarantined,
}

impl RemoteReplayRefusalV1 {
    const fn of(state: RemoteReplayStateV1) -> Option<Self> {
        match state {
            RemoteReplayStateV1::Rejected => Some(Self::Rejected),
            RemoteReplayStateV1::Quarantined => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// One durable spool row as the guided replay reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayBacklogEntryV1 {
    pub event_id: String,
    pub sequence: u64,
    pub state: RemoteReplayStateV1,
    pub receipt: Option<RemoteReplayCommitReceiptV1>,
    pub finding: Option<RemoteReplayFindingV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayPendingServiceOutcomeV1 {
    pub receipt: RemoteReplayPendingReceiptV1,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub scope: ResolvedScope,
}

pub fn remote_replay_pending_result_contract_v1()
-> Result<ResultContractRef, ApplicationContractError> {
    let schema = SchemaId::new("remote.replay-pending.result").map_err(|_| {
        ApplicationContractError::InvalidIdentifier {
            field: "remote replay pending result schema",
        }
    })?;
    ResultContractRef::new(schema, 1)
}

impl RemoteReplayServiceV1 {
    /// Replays the caller's unsettled frames in sequence order through
    /// [`Self::replay`], stopping at the first refusal. A protocol failure
    /// returns as that frame's problem; a terminal frame outcome is reported
    /// in `stopped_at`.
    pub fn replay_pending(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayPendingRequestV1>,
        presented_credential: &OpaqueRemoteCredential,
    ) -> Result<RemoteReplayPendingServiceOutcomeV1, RemoteReplayServiceErrorV1> {
        if request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteReplayServiceErrorV1::UnsupportedVersion);
        }
        request
            .validate_metadata()
            .map_err(|_| RemoteReplayServiceErrorV1::InvalidRequest)?;
        let Some(expected) = request.expected_authority.as_ref() else {
            return Err(RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(
                Box::new(CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                    observed_at: request.sent_at,
                }),
            ));
        };
        let caller = self
            .credentials
            .authority_enrollment(
                &request.brain_id,
                &request.caller_node_id,
                request.enrollment_revision,
            )
            .map_err(RemoteReplayServiceErrorV1::Credential)?;
        let caller_admission = self.caller_admission(&caller)?;
        // Checked once up front so a stale fence refuses even when every
        // frame is already settled and no single replay would run.
        let (_, authority) = self.expected_writer(Some(expected), expected)?;
        let backlog = |service: &Self| {
            service
                .spool
                .replay_backlog(&caller.enrollment_id)
                .map_err(RemoteReplayServiceErrorV1::Persistence)
        };
        let mut replayed = BTreeMap::new();
        for entry in backlog(self)? {
            if RemoteReplayRefusalV1::of(entry.state).is_some() {
                break;
            }
            if entry.state.is_settled() {
                continue;
            }
            let outcome = self
                .replay(&frame_request(request, &entry)?, presented_credential)?
                .outcome;
            let acknowledged = matches!(
                outcome,
                RemoteReplayOutcomeV1::Acknowledged { .. }
                    | RemoteReplayOutcomeV1::AlreadyAcknowledged { .. }
            );
            if let Some(receipt) = outcome.into_operation_receipt() {
                replayed.insert(entry.event_id, receipt);
            }
            if !acknowledged {
                break;
            }
        }
        Ok(RemoteReplayPendingServiceOutcomeV1 {
            receipt: settled_receipt(backlog(self)?, replayed)?,
            authority,
            scope: caller_admission.admission.scope().clone(),
        })
    }
}

fn frame_request(
    request: &RemoteProtocolRequestV1<RemoteReplayPendingRequestV1>,
    entry: &RemoteReplayBacklogEntryV1,
) -> Result<RemoteProtocolRequestV1<RemoteReplayRequestV1>, RemoteReplayServiceErrorV1> {
    let request_id = RequestId::new(format!(
        "{}.{}",
        request.request_id.as_str(),
        entry.sequence
    ))
    .map_err(|_| RemoteReplayServiceErrorV1::InvalidRequest)?;
    RemoteProtocolRequestV1::new(
        request_id,
        request.brain_id.clone(),
        request.caller_node_id.clone(),
        request.enrollment_revision,
        request.expected_authority.clone(),
        request.sent_at,
        RemoteReplayRequestV1 {
            event_id: entry.event_id.clone(),
        },
    )
    .map_err(|_| RemoteReplayServiceErrorV1::InvalidRequest)
}

/// Projects the step's answer from the durable rows read after the walk.
fn settled_receipt(
    backlog: Vec<RemoteReplayBacklogEntryV1>,
    mut replayed: BTreeMap<String, RemoteReplayOperationReceiptV1>,
) -> Result<RemoteReplayPendingReceiptV1, RemoteReplayApplicationErrorV1> {
    let mut acknowledged = Vec::new();
    for entry in backlog {
        let replayed = replayed.remove(&entry.event_id);
        if let Some(refusal) = RemoteReplayRefusalV1::of(entry.state) {
            return Ok(RemoteReplayPendingReceiptV1 {
                acknowledged,
                stopped_at: Some(RemoteReplayPendingStopV1 {
                    finding: entry
                        .finding
                        .ok_or(RemoteReplayApplicationErrorV1::InvalidSpoolState)?,
                    event_id: entry.event_id,
                    sequence: entry.sequence,
                    refusal,
                    replayed,
                }),
            });
        }
        if !entry.state.is_settled() {
            return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
        }
        acknowledged.push(RemoteReplayPendingFrameV1 {
            sequence: entry.sequence,
            receipt: entry
                .receipt
                .ok_or(RemoteReplayApplicationErrorV1::ReceiptMissing)?,
            replayed,
        });
    }
    Ok(RemoteReplayPendingReceiptV1 {
        acknowledged,
        stopped_at: None,
    })
}

impl RemoteProtocolPortV1<RemoteReplayPendingRequestV1> for RemoteReplayProtocolAdapterV1 {
    type Output = RemoteReplayPendingReceiptV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayPendingRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let contract = remote_replay_pending_result_contract_v1()?;
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        match self.service.replay_pending(&request, &credential) {
            // A resend commits nothing, so the step answers with a plain
            // result rather than an effect whose commit postdates the request.
            Ok(outcome) => RemoteProtocolResponseV1::new_or_unavailable(
                request_id.clone(),
                outcome.authority,
                Ok(ApplicationEnvelope {
                    contract: contract.clone(),
                    request_id,
                    scope: outcome.scope,
                    outcome: ApplicationOutcome::Result(outcome.receipt),
                    touched_files: Vec::new(),
                    code_graph: None,
                    analytics: None,
                    cost: None,
                }),
                contract,
                observed_at,
            ),
            Err(error) => replay_failure_response(
                request_id,
                observed_at,
                request.expected_authority,
                contract,
                error,
            ),
        }
    }
}

#[cfg(test)]
mod tests;
