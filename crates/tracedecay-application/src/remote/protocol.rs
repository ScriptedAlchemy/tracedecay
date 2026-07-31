//! Versioned, transport-neutral remote Brain protocol envelopes.
//!
//! Credentials are carried by the authenticated transport boundary and are
//! deliberately absent from these serializable payloads.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, EntityId,
    ProjectionGenerationId, RemoteCapabilityV1, RemotePlacementRevisionV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, ShardId, UtcMicros,
};
use tracedecay_tool_catalog::SchemaId;

use crate::remote::auth::OpaqueRemoteCredential;
use crate::{
    ApplicationContractError, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    LegalAction, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};

pub const REMOTE_PROTOCOL_VERSION_V1: u16 = 1;
pub const REMOTE_ENROLLMENT_USE_CASE_ID_V1: &str = "use-case.remote.enrollment";
pub const REMOTE_REPLAY_USE_CASE_ID_V1: &str = "use-case.remote.replay";

pub fn remote_enrollment_result_contract_v1() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("remote.result").expect("static remote result schema id is canonical"),
        1,
    )
    .expect("static remote result contract is canonical")
}

pub fn remote_replay_result_contract_v1() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("remote.replay.result")
            .expect("static remote replay result schema id is canonical"),
        1,
    )
    .expect("static remote replay result contract is canonical")
}

/// Canonical semantic validation required before any authenticated remote
/// request reaches a production port.
pub trait RemoteProtocolBodyV1 {
    fn validate_remote_protocol_body(
        &self,
        sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError>;
}

/// Authenticated transport boundary for one versioned remote operation.
///
/// Concrete HTTP/SSE and persistence adapters remain outside the application
/// crate; this port receives the opaque credential without serializing it.
pub trait RemoteProtocolPortV1<Request> {
    type Output;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<Request>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output>;
}

/// Enrollment requires both the one-time grant credential and the replacement
/// enrollment credential. Neither secret is serializable or retained by the
/// protocol request body.
pub trait RemoteEnrollmentProtocolPortV1: Send + Sync {
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1>;
}

/// Validates canonical protocol metadata before delegating exactly once to the
/// authenticated transport-neutral remote port.
pub struct RemoteProtocolServiceV1<Port> {
    port: Port,
}

impl<Port> RemoteProtocolServiceV1<Port> {
    pub const fn new(port: Port) -> Self {
        Self { port }
    }

    pub fn execute<Request>(
        &self,
        request: RemoteProtocolRequestV1<Request>,
        credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<Port::Output>, ApplicationContractError>
    where
        Port: RemoteProtocolPortV1<Request>,
        Request: RemoteProtocolBodyV1,
    {
        request.validate_metadata()?;
        request
            .body
            .validate_remote_protocol_body(request.sent_at)?;
        Ok(self.port.execute(request, credential))
    }

    pub fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<EnrollmentCredentialRecordV1>, ApplicationContractError>
    where
        Port: RemoteEnrollmentProtocolPortV1,
    {
        request.validate_initial_enrollment_metadata()?;
        request
            .body
            .validate_remote_protocol_body(request.sent_at)?;
        Ok(self
            .port
            .execute_enrollment(request, grant_credential, enrollment_credential))
    }
}

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
        self.validate_common_metadata()?;
        if self.enrollment_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote enrollment revision",
            });
        }
        Ok(())
    }

    fn validate_common_metadata(&self) -> Result<(), ApplicationContractError> {
        if self.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote protocol version",
            });
        }
        self.brain_id.validate()?;
        self.caller_node_id.validate()?;
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

impl RemoteProtocolRequestV1<EnrollmentRequestV1> {
    pub fn new_initial_enrollment(
        request_id: RequestId,
        brain_id: BrainId,
        caller_node_id: BrainNodeId,
        sent_at: UtcMicros,
        body: EnrollmentRequestV1,
    ) -> Result<Self, ApplicationContractError> {
        let request = Self {
            protocol_version: REMOTE_PROTOCOL_VERSION_V1,
            request_id,
            brain_id,
            caller_node_id,
            enrollment_revision: 0,
            expected_authority: None,
            sent_at,
            body,
        };
        request.validate_initial_enrollment_metadata()?;
        Ok(request)
    }

    pub fn validate_initial_enrollment_metadata(&self) -> Result<(), ApplicationContractError> {
        self.validate_common_metadata()?;
        if self.enrollment_revision != 0 || self.expected_authority.is_some() {
            return Err(ApplicationContractError::Inconsistent {
                field: "initial remote enrollment metadata",
            });
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
    pub placement_revision: RemotePlacementRevisionV1,
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

impl RemoteProtocolBodyV1 for CurrentAuthorityRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
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

impl RemoteProtocolBodyV1 for EnrollmentRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate(sent_at)
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

impl RemoteProtocolBodyV1 for CredentialRotationRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate(sent_at)
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

impl RemoteProtocolBodyV1 for CredentialRevocationRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use tracedecay_domain::{
        AuthorityEpoch, CurrentRemoteAuthorityV1, ObservabilityTerminalResultV1,
        OperationActivationOutcomeV1, OperationAvailabilityV1, OperationPhaseTimingV1,
        OperationPhaseV1, OperationReadinessV1, OperationResourceObservedV1,
        OperationStageTimingV1, OperationStageV1, ProjectId, RemotePlacementRevisionV1,
        RepositoryId, RepositoryStateSnapshotId, WorktreeId,
    };
    use tracedecay_tool_catalog::SchemaId;

    fn fence() -> RemoteWriterFenceV1 {
        RemoteWriterFenceV1 {
            brain_id: BrainId::new("brain.remote").unwrap(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            authority_epoch: AuthorityEpoch(4),
            authority_node_id: BrainNodeId::new("node.authority").unwrap(),
        }
    }

    struct FakeProtocolPort {
        calls: Arc<AtomicUsize>,
    }

    struct EmptyTestBody;

    impl RemoteProtocolBodyV1 for EmptyTestBody {
        fn validate_remote_protocol_body(
            &self,
            _sent_at: UtcMicros,
        ) -> Result<(), ApplicationContractError> {
            Ok(())
        }
    }

    impl RemoteProtocolPortV1<EmptyTestBody> for FakeProtocolPort {
        type Output = ();

        fn execute(
            &self,
            request: RemoteProtocolRequestV1<EmptyTestBody>,
            _credential: OpaqueRemoteCredential,
        ) -> RemoteProtocolResponseV1<Self::Output> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let request_id = request.request_id;
            RemoteProtocolResponseV1::new(
                request_id.clone(),
                CurrentRemoteAuthorityStateV1::Unavailable {
                    reason:
                        tracedecay_domain::RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    observed_at: UtcMicros(20),
                },
                Err(remote_protocol_problem(
                    ResultContractRef::new(SchemaId::new("remote.result").unwrap(), 1).unwrap(),
                    request_id,
                    RemoteProtocolFailureV1::AuthorityUnavailable,
                )),
            )
            .unwrap()
        }
    }

    #[test]
    fn generic_protocol_service_delegates_once_to_application_port() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = RemoteProtocolServiceV1::new(FakeProtocolPort {
            calls: Arc::clone(&calls),
        });
        let request = RemoteProtocolRequestV1::new(
            RequestId::new("request.remote").unwrap(),
            BrainId::new("brain.remote").unwrap(),
            BrainNodeId::new("node.caller").unwrap(),
            1,
            None,
            UtcMicros(10),
            EmptyTestBody,
        )
        .unwrap();

        let response = service
            .execute(
                request,
                OpaqueRemoteCredential::new(
                    b"0123456789abcdef0123456789abcdef"
                        .to_vec()
                        .into_boxed_slice(),
                )
                .unwrap(),
            )
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.request_id.as_str(), "request.remote");
    }

    #[test]
    fn protocol_round_trip_preserves_observability_contracts() {
        let resource = OperationResourceObservedV1 {
            scheduled_latency_micros: 5,
            service_latency_micros: 34,
            process_rss_bytes: None,
            process_pss_bytes: None,
            cpu_user_micros: None,
            cpu_system_micros: None,
            read_bytes: None,
            write_bytes: None,
            input_tokens: None,
            output_tokens: None,
            cost_amount: None,
            cost_currency: None,
            pricing_revision: None,
            stage_timings: vec![
                OperationStageTimingV1 {
                    stage: OperationStageV1::Scheduled,
                    elapsed_micros: 0,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Admitted,
                    elapsed_micros: 5,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Started,
                    elapsed_micros: 8,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::FirstUsefulResult,
                    elapsed_micros: 21,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Terminal,
                    elapsed_micros: 34,
                },
            ],
            phase_timings: vec![
                OperationPhaseTimingV1 {
                    phase: OperationPhaseV1::ProcessSpawn,
                    duration_micros: 3,
                },
                OperationPhaseTimingV1 {
                    phase: OperationPhaseV1::ProcessReady,
                    duration_micros: 4,
                },
                OperationPhaseTimingV1 {
                    phase: OperationPhaseV1::Dispatch,
                    duration_micros: 8,
                },
                OperationPhaseTimingV1 {
                    phase: OperationPhaseV1::OutputWrite,
                    duration_micros: 1,
                },
            ],
            absolute_deadline_micros: Some(50),
            availability: OperationAvailabilityV1::Available,
            activation_outcome: Some(OperationActivationOutcomeV1::Committed),
            process_count: Some(2),
            input_bytes: Some(128),
            output_bytes: Some(64),
        };
        let request = RemoteProtocolRequestV1::new(
            RequestId::new("request.remote.observability").unwrap(),
            BrainId::new("brain.remote").unwrap(),
            BrainNodeId::new("node.caller").unwrap(),
            1,
            None,
            UtcMicros(10),
            resource,
        )
        .unwrap();

        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: RemoteProtocolRequestV1<OperationResourceObservedV1> =
            serde_json::from_slice(&encoded).unwrap();

        assert_eq!(
            decoded.body.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: Some(21),
                background_complete_micros: Some(34),
            }
        );
        assert_eq!(decoded.body.phase_timings, request.body.phase_timings);
        assert_eq!(
            decoded.body.absolute_deadline_micros,
            request.body.absolute_deadline_micros
        );
        assert_eq!(decoded.body.availability, request.body.availability);
        assert_eq!(
            decoded
                .body
                .validate(Some(ObservabilityTerminalResultV1::Succeeded)),
            Ok(())
        );
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
    fn authority_discovery_requires_the_exact_typed_placement_revision() {
        let request = CurrentAuthorityRequestV1 {
            brain_id: BrainId::new("brain.remote").unwrap(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(2).unwrap(),
        };
        let state = CurrentRemoteAuthorityStateV1::Available(CurrentRemoteAuthorityV1 {
            fence: fence(),
            credential_revision: 1,
            observed_at: UtcMicros(10),
        });
        assert!(validate_current_authority_state(&request, &state).is_err());

        let exact = CurrentAuthorityRequestV1 {
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            ..request
        };
        assert!(validate_current_authority_state(&exact, &state).is_ok());
    }

    #[test]
    fn authority_discovery_rejects_zero_placement_on_the_wire() {
        let invalid = serde_json::json!({
            "brain_id": "brain.remote",
            "shard_id": "shard.remote",
            "generation_id": "generation.remote",
            "placement_revision": 0,
        });
        assert!(serde_json::from_value::<CurrentAuthorityRequestV1>(invalid).is_err());

        let valid = CurrentAuthorityRequestV1 {
            brain_id: BrainId::new("brain.remote").unwrap(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(9).unwrap(),
        };
        let encoded = serde_json::to_value(&valid).unwrap();
        assert_eq!(encoded["placement_revision"], 9);
        assert_eq!(
            serde_json::from_value::<CurrentAuthorityRequestV1>(encoded).unwrap(),
            valid
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
                project_id: ProjectId::new("project.remote").unwrap(),
                repository_id: RepositoryId::new("repository.remote").unwrap(),
                worktree_id: WorktreeId::new("worktree.remote").unwrap(),
                reference: None,
                snapshot_id: RepositoryStateSnapshotId::new("repository.state.remote").unwrap(),
            },
        };
        let request = RemoteProtocolRequestV1::new_initial_enrollment(
            RequestId::new("request.remote").unwrap(),
            body.brain_id.clone(),
            body.node_id.clone(),
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
