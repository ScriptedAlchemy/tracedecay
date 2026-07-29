//! Versioned, transport-neutral remote Brain protocol envelopes.
//!
//! Credentials are carried by the authenticated transport boundary and are
//! deliberately absent from these serializable payloads.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, EntityId,
    EntityVersionId, ProjectionGenerationId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, ShardId, UtcMicros,
};

use crate::{
    ApplicationContractError, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    LegalAction, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};

pub const REMOTE_PROTOCOL_VERSION_V1: u16 = 1;

/// Versioned request metadata common to enrollment, authority discovery,
/// rotation, revocation, and subsequent remote operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteProtocolRequestV1<T> {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub brain_id: BrainId,
    pub caller_node_id: BrainNodeId,
    pub enrollment_revision: u64,
    /// `None` is legal only while discovering or enrolling with authority.
    pub expected_authority: Option<RemoteWriterFenceV1>,
    pub sent_at: UtcMicros,
    pub body: T,
}

impl<T> RemoteProtocolRequestV1<T> {
    pub fn new(
        request_id: RequestId,
        brain_id: BrainId,
        caller_node_id: BrainNodeId,
        enrollment_revision: u64,
        expected_authority: Option<RemoteWriterFenceV1>,
        sent_at: UtcMicros,
        body: T,
    ) -> Result<Self, ApplicationContractError> {
        let request = Self {
            protocol_version: REMOTE_PROTOCOL_VERSION_V1,
            request_id,
            brain_id,
            caller_node_id,
            enrollment_revision,
            expected_authority,
            sent_at,
            body,
        };
        request.validate_metadata()?;
        Ok(request)
    }

    pub fn validate_metadata(&self) -> Result<(), ApplicationContractError> {
        if self.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote protocol version",
            });
        }
        self.brain_id.validate()?;
        self.caller_node_id.validate()?;
        if self.enrollment_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote enrollment revision",
            });
        }
        if let Some(authority) = &self.expected_authority {
            authority.validate()?;
            if authority.brain_id != self.brain_id {
                return Err(ApplicationContractError::Inconsistent {
                    field: "remote request authority Brain identity",
                });
            }
        }
        Ok(())
    }
}

/// Server response preserves the canonical application result and separately
/// states whether current authority identity was available, partial, or
/// unavailable.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProtocolResponseV1<T> {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub result: ApplicationResult<T>,
}

impl<T> RemoteProtocolResponseV1<T> {
    pub fn new(
        request_id: RequestId,
        authority: CurrentRemoteAuthorityStateV1,
        result: ApplicationResult<T>,
    ) -> Result<Self, ApplicationContractError> {
        authority.validate()?;
        let result_request_id = match &result {
            Ok(envelope) => &envelope.request_id,
            Err(problem) => &problem.request_id,
        };
        if result_request_id != &request_id {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote response request identity",
            });
        }
        Ok(Self {
            protocol_version: REMOTE_PROTOCOL_VERSION_V1,
            request_id,
            authority,
            result,
        })
    }
}

/// Exact shard placement requested during current-authority discovery.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentAuthorityRequestV1 {
    pub brain_id: BrainId,
    pub shard_id: ShardId,
    pub generation_id: ProjectionGenerationId,
    pub placement_revision: EntityVersionId,
}

impl CurrentAuthorityRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.brain_id.validate()?;
        self.shard_id.validate()?;
        self.generation_id.validate()?;
        self.placement_revision.validate()?;
        Ok(())
    }
}

/// Ensure discovered authority evidence addresses exactly the requested
/// Brain/shard/generation/placement. A response for a nearby shard or stale
/// placement is never accepted as current.
pub fn validate_current_authority_state(
    request: &CurrentAuthorityRequestV1,
    state: &CurrentRemoteAuthorityStateV1,
) -> Result<(), ApplicationContractError> {
    request.validate()?;
    state.validate()?;
    let fence = match state {
        CurrentRemoteAuthorityStateV1::Available(authority) => Some(&authority.fence),
        CurrentRemoteAuthorityStateV1::Partial { known_fence, .. } => known_fence.as_ref(),
        CurrentRemoteAuthorityStateV1::Unavailable { .. } => None,
    };
    if fence.is_some_and(|fence| {
        fence.brain_id != request.brain_id
            || fence.shard_id != request.shard_id
            || fence.generation_id != request.generation_id
            || fence.placement_revision != request.placement_revision
    }) {
        return Err(ApplicationContractError::Inconsistent {
            field: "current remote authority identity",
        });
    }
    Ok(())
}

/// Public enrollment metadata. The opaque enrollment credential is accepted
/// through `OpaqueRemoteCredential`, never this payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequestV1 {
    pub grant_id: EntityId,
    pub grant_revision: u64,
    pub enrollment_id: EntityId,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub expires_at: UtcMicros,
    pub capabilities: BTreeSet<RemoteCapabilityV1>,
    pub scope: RemoteRepositoryScopeV1,
}

impl EnrollmentRequestV1 {
    pub fn validate(&self, observed_at: UtcMicros) -> Result<(), ApplicationContractError> {
        self.grant_id.validate()?;
        self.enrollment_id.validate()?;
        self.brain_id.validate()?;
        self.node_id.validate()?;
        self.scope.validate()?;
        if self.grant_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote enrollment grant revision",
            });
        }
        if self.expires_at <= observed_at {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote enrollment validity",
            });
        }
        if self.capabilities.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote enrollment capabilities",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRotationRequestV1 {
    pub enrollment_id: EntityId,
    pub expected_revision: u64,
    pub expires_at: UtcMicros,
}

impl CredentialRotationRequestV1 {
    pub fn validate(&self, observed_at: UtcMicros) -> Result<(), ApplicationContractError> {
        self.enrollment_id.validate()?;
        if self.expected_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote rotation expected revision",
            });
        }
        if self.expires_at <= observed_at {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote rotated credential validity",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRevocationRequestV1 {
    pub enrollment_id: EntityId,
    pub expected_revision: u64,
    pub revoked_at: UtcMicros,
}

impl CredentialRevocationRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.enrollment_id.validate()?;
        if self.expected_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote revocation expected revision",
            });
        }
        Ok(())
    }
}

/// Stable, secret-free classification used to construct canonical application
/// problems for protocol admission failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteProtocolFailureV1 {
    UnsupportedVersion,
    CallerAuthenticationFailed,
    AuthorityAuthenticationFailed,
    EnrollmentExpired,
    EnrollmentRevoked,
    InsufficientCapability,
    ScopeMismatch,
    StaleCredentialRevision,
    StaleAuthorityFence,
    AuthorityUnavailable,
}

pub fn remote_protocol_problem(
    contract: ResultContractRef,
    request_id: RequestId,
    failure: RemoteProtocolFailureV1,
) -> ApplicationProblemEnvelope {
    let problem = match failure {
        RemoteProtocolFailureV1::CallerAuthenticationFailed
        | RemoteProtocolFailureV1::AuthorityAuthenticationFailed => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        RemoteProtocolFailureV1::EnrollmentExpired | RemoteProtocolFailureV1::EnrollmentRevoked => {
            ApplicationProblem::NotFoundOrNotAuthorized {
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Reauthorize],
            }
        }
        RemoteProtocolFailureV1::UnsupportedVersion => ApplicationProblem::Unsupported {
            diagnostic: safe_diagnostic(
                "remote.protocol_incompatible",
                "The remote protocol version is not supported",
            ),
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        RemoteProtocolFailureV1::InsufficientCapability
        | RemoteProtocolFailureV1::ScopeMismatch => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        RemoteProtocolFailureV1::StaleCredentialRevision
        | RemoteProtocolFailureV1::StaleAuthorityFence => ApplicationProblem::Stale {
            diagnostic: safe_diagnostic(
                "remote.authority_stale",
                "Remote authority or credential identity is stale",
            ),
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        RemoteProtocolFailureV1::AuthorityUnavailable => ApplicationProblem::Unavailable {
            diagnostic: safe_diagnostic(
                "remote.authority_unavailable",
                "The authenticated remote authority is unavailable",
            ),
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
    };
    ApplicationProblemEnvelope::new(contract, request_id, problem)
}

fn safe_diagnostic(code: &str, message: &str) -> SafeDiagnostic {
    SafeDiagnostic::new(code, message).expect("static remote diagnostic is canonical")
}

pub type EnrollmentProtocolResultV1 = ApplicationResult<EnrollmentCredentialRecordV1>;
pub type CurrentAuthorityProtocolResultV1 = ApplicationResult<CurrentRemoteAuthorityStateV1>;

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{AuthorityEpoch, RepositoryId, RepositoryStateSnapshotId, WorktreeId};
    use tracedecay_tool_catalog::SchemaId;

    fn fence() -> RemoteWriterFenceV1 {
        RemoteWriterFenceV1 {
            brain_id: BrainId::new("brain.remote").unwrap(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: EntityVersionId::new("placement.remote").unwrap(),
            authority_epoch: AuthorityEpoch(4),
            authority_node_id: BrainNodeId::new("node.authority").unwrap(),
        }
    }

    #[test]
    fn request_rejects_authority_from_another_brain() {
        let mut authority = fence();
        authority.brain_id = BrainId::new("brain.other").unwrap();
        assert!(
            RemoteProtocolRequestV1::new(
                RequestId::new("request.remote").unwrap(),
                BrainId::new("brain.remote").unwrap(),
                BrainNodeId::new("node.caller").unwrap(),
                1,
                Some(authority),
                UtcMicros(10),
                (),
            )
            .is_err()
        );
    }

    #[test]
    fn wire_request_contains_exact_identity_and_no_transport_or_secret_fields() {
        let body = EnrollmentRequestV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.caller").unwrap(),
            expires_at: UtcMicros(100),
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: RemoteRepositoryScopeV1 {
                repository_id: RepositoryId::new("repository.remote").unwrap(),
                worktree_id: WorktreeId::new("worktree.remote").unwrap(),
                reference: None,
                snapshot_id: RepositoryStateSnapshotId::new("repository.state.remote").unwrap(),
            },
        };
        let request = RemoteProtocolRequestV1::new(
            RequestId::new("request.remote").unwrap(),
            body.brain_id.clone(),
            body.node_id.clone(),
            1,
            None,
            UtcMicros(10),
            body,
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["protocol_version"], REMOTE_PROTOCOL_VERSION_V1);
        assert_eq!(value["body"]["scope"]["repository_id"], "repository.remote");
        assert!(value.get("credential").is_none());
        assert!(value.get("url").is_none());
        assert!(value.get("path").is_none());
    }

    #[test]
    fn authentication_failures_use_concealed_application_problem() {
        let problem = remote_protocol_problem(
            ResultContractRef::new(SchemaId::new("remote.result").unwrap(), 1).unwrap(),
            RequestId::new("request.remote").unwrap(),
            RemoteProtocolFailureV1::CallerAuthenticationFailed,
        );
        assert_eq!(
            problem.problem.source(),
            &ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        );
    }
}
