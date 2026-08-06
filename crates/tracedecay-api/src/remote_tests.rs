use std::convert::Infallible;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use axum::body::{Body, Bytes};
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use futures_util::stream;
use tower::ServiceExt;
use tracedecay_application::remote::auth::{
    RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentCommitReceiptV1,
};
use tracedecay_application::remote::composition::ExpectedRemoteShardV1;
use tracedecay_application::remote::credential_admission::{
    RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1,
    RemoteCredentialAdmissionPortV1, RemoteCredentialAdmissionServiceV1,
    RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1, RemoteCredentialLookupErrorV1,
    RemoteCredentialLookupPortV1, RemoteCredentialUseV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolExecutionControlV1,
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::query::{
    REMOTE_QUERY_SCHEMA_REVISION_V1, RemoteQueryOperationV1, RemoteQueryRequestV1,
    RemoteQueryResultV1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    RecoveryAuthorityExpectationV1, StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, OperationBudgetUsage,
    PolicyDecisionRef, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, AuthorityEpoch, BrainId, BrainNodeId, CanonicalObservationIdV1, ComponentVersion,
    CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, EnrollmentGrantV1, EntityId,
    ManifestDigest, ProjectId, ProjectionGenerationId, RefId, RemoteAuthorityUnavailableReasonV1,
    RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemotePlacementRevisionV1,
    RemoteRepositoryScopeV1, RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, ShardId,
    UtcMicros, WorktreeId, canonical_sha256,
};

use super::*;

struct RejectingCredentialAdmission {
    calls: Arc<AtomicUsize>,
    error: RemoteCredentialAdmissionErrorV1,
}

struct OneCredentialAuthority {
    fingerprint: RemoteCredentialFingerprintV1,
    record: RemoteCredentialAuthorityRecordV1,
}

impl RemoteCredentialLookupPortV1 for OneCredentialAuthority {
    fn credential_by_fingerprint(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1> {
        if class == RemoteCredentialClassV1::Enrollment && fingerprint == &self.fingerprint {
            return Ok(self.record.clone());
        }
        Err(RemoteCredentialLookupErrorV1::NotFound)
    }
}

impl RemoteCredentialAdmissionPortV1 for RejectingCredentialAdmission {
    fn admit_before_body(
        &self,
        _presented: &OpaqueRemoteCredential,
        _use_case: RemoteCredentialUseV1,
        _observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(self.error.clone())
    }

    fn reauthorize_publication(
        &self,
        _session: &RemoteAuthenticatedSessionV1,
        _observed_at: UtcMicros,
    ) -> Result<RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1> {
        Err(self.error.clone())
    }
}

struct UnreachedProtocolPort {
    calls: Arc<AtomicUsize>,
    controlled_deadline: Option<Arc<AtomicI64>>,
}

impl RemoteEnrollmentProtocolPortV1 for UnreachedProtocolPort {
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        _grant_credential: OpaqueRemoteCredential,
        _enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        unavailable_response(request)
    }
}

macro_rules! unreachable_protocol_port {
    ($request:ty, $output:ty) => {
        impl RemoteProtocolPortV1<$request> for UnreachedProtocolPort {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                _credential: OpaqueRemoteCredential,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                unavailable_response(request)
            }

            fn execute_controlled(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                _credential: OpaqueRemoteCredential,
                control: RemoteProtocolExecutionControlV1,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(deadline) = &self.controlled_deadline {
                    deadline.store(control.deadline.0, Ordering::SeqCst);
                }
                unavailable_response(request)
            }
        }
    };
}

unreachable_protocol_port!(RemoteReplayRequestV1, RemoteReplayOutcomeV1);
unreachable_protocol_port!(RemoteQueryRequestV1, RemoteQueryResultV1);
unreachable_protocol_port!(BackupRequestV1, BackupOperationStateV1);
unreachable_protocol_port!(StagedRestoreConfirmationV1, StagedRestoreProgressV1);
unreachable_protocol_port!(PromotionConfirmationV1, PromotionCasReceiptV1);

fn unavailable_response<Request, Output>(
    request: RemoteProtocolRequestV1<Request>,
) -> RemoteProtocolResponseV1<Output> {
    let request_id = request.request_id;
    RemoteProtocolResponseV1::new(
        request_id.clone(),
        CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
            observed_at: UtcMicros(20),
        },
        Err(remote_protocol_problem(
            remote_result_contract(),
            request_id,
            RemoteProtocolFailureV1::AuthorityUnavailable,
        )),
    )
    .unwrap()
}

const fn fixed_remote_clock() -> UtcMicros {
    UtcMicros(20)
}

const ACTIVE_CREDENTIAL: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn repository_scope(snapshot_id: &str) -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        project_id: id::<ProjectId>("project.remote"),
        repository_id: id::<RepositoryId>("repository.remote"),
        worktree_id: id::<WorktreeId>("worktree.remote"),
        reference: Some(id::<RefId>("refs/heads/main")),
        snapshot_id: RepositoryStateSnapshotId::new(snapshot_id).unwrap(),
    }
}

fn credential_authority() -> RemoteCredentialAdmissionServiceV1<OneCredentialAuthority> {
    let scope = repository_scope("snapshot.remote");
    let enrollment = EnrollmentCredentialRecordV1 {
        enrollment_id: id::<EntityId>("enrollment.remote"),
        brain_id: id::<BrainId>("brain.remote"),
        node_id: id::<BrainNodeId>("node.remote"),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(ACTIVE_CREDENTIAL).unwrap(),
        revision: 4,
        issued_at: UtcMicros(10),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: std::collections::BTreeSet::from([
            RemoteCapabilityV1::Query,
            RemoteCapabilityV1::CreateBackup,
        ]),
        scope: scope.clone(),
    };
    let grant = EnrollmentGrantV1 {
        grant_id: id::<EntityId>("grant.remote"),
        brain_id: enrollment.brain_id.clone(),
        node_id: enrollment.node_id.clone(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[3_u8; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: enrollment.capabilities.clone(),
        scope,
    };
    let resolved_scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .unwrap();
    let grant_digest = canonical_sha256(&grant).unwrap();
    let admission = RemoteEnrollmentAdmissionEvidenceV1::new(
        &grant,
        resolved_scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: resolved_scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote.enrollment",
                1,
                grant_digest.clone(),
                ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(9),
        },
        ActorId::new("actor.remote").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        Deadline::new(UtcMicros(100)).unwrap(),
    )
    .unwrap();
    let receipt = RemoteEnrollmentCommitReceiptV1 {
        admission,
        prior_grant_digest: grant_digest,
        input_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
        committed_state_digest: canonical_sha256(&enrollment).unwrap(),
        consumed_at: enrollment.issued_at,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: 1,
            elapsed_micros: 0,
        },
        enrollment,
    };
    receipt.validate().unwrap();
    let record = RemoteCredentialAuthorityRecordV1::Enrollment {
        enrollment: receipt.enrollment.clone(),
        receipt,
    };
    RemoteCredentialAdmissionServiceV1::new(OneCredentialAuthority {
        fingerprint: RemoteCredentialFingerprintV1::from_secret(ACTIVE_CREDENTIAL).unwrap(),
        record,
    })
}

fn expected_authority() -> RemoteWriterFenceV1 {
    RemoteWriterFenceV1 {
        brain_id: id::<BrainId>("brain.remote"),
        shard_id: ShardId::new("shard.remote").unwrap(),
        generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
        placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
        authority_epoch: AuthorityEpoch(1),
        authority_node_id: id::<BrainNodeId>("node.authority"),
    }
}

fn query_request(scope: RemoteRepositoryScopeV1) -> RemoteHttpRequestV1<RemoteQueryRequestV1> {
    let expected_authority = expected_authority();
    RemoteHttpRequestV1 {
        request: RemoteProtocolRequestV1::new(
            RequestId::new("request.remote.query").unwrap(),
            id::<BrainId>("brain.remote"),
            id::<BrainNodeId>("node.remote"),
            4,
            Some(expected_authority.clone()),
            UtcMicros(20),
            RemoteQueryRequestV1 {
                schema_revision: REMOTE_QUERY_SCHEMA_REVISION_V1,
                scope,
                expected_shards: vec![ExpectedRemoteShardV1 {
                    brain_id: "brain.remote".to_owned(),
                    shard_id: "shard.remote".to_owned(),
                    generation_id: "generation.remote".to_owned(),
                }],
                expected_authority,
                operation: RemoteQueryOperationV1::ExactObservation {
                    observation_id: CanonicalObservationIdV1::new(format!(
                        "sha256:{}",
                        "a".repeat(64)
                    ))
                    .unwrap(),
                },
            },
        )
        .unwrap(),
    }
}

fn authenticated_router(port_calls: Arc<AtomicUsize>) -> Router {
    remote_protocol_router(
        UnreachedProtocolPort {
            calls: port_calls,
            controlled_deadline: None,
        },
        Arc::new(credential_authority()),
        fixed_remote_clock,
    )
}

fn rejecting_router(
    error: RemoteCredentialAdmissionErrorV1,
    admission_calls: Arc<AtomicUsize>,
    port_calls: Arc<AtomicUsize>,
) -> Router {
    remote_protocol_router(
        UnreachedProtocolPort {
            calls: port_calls,
            controlled_deadline: None,
        },
        Arc::new(RejectingCredentialAdmission {
            calls: admission_calls,
            error,
        }),
        fixed_remote_clock,
    )
}

fn deadline_capturing_router(
    port_calls: Arc<AtomicUsize>,
    controlled_deadline: Arc<AtomicI64>,
) -> Router {
    remote_protocol_router(
        UnreachedProtocolPort {
            calls: port_calls,
            controlled_deadline: Some(controlled_deadline),
        },
        Arc::new(credential_authority()),
        fixed_remote_clock,
    )
}

fn query_http_request(body: Body) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/query")
        .header(AUTHORIZATION, "Bearer 0123456789abcdef0123456789abcdef")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

fn backup_http_request(body: Body) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/backup")
        .header(AUTHORIZATION, "Bearer 0123456789abcdef0123456789abcdef")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

fn unpolled_body(body_polls: &Arc<AtomicUsize>) -> Body {
    let observed_body_polls = Arc::clone(body_polls);
    Body::from_stream(stream::poll_fn(move |_| {
        observed_body_polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(None::<Result<Bytes, Infallible>>)
    }))
}

fn rejected_request(router: Router, body: Body) -> axum::response::Response {
    block_on(
        router.oneshot(
            Request::builder()
                .method("POST")
                .uri("/replay")
                .header(AUTHORIZATION, "Bearer 0123456789abcdef0123456789abcdef")
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        ),
    )
    .unwrap()
}

#[test]
fn authorization_header_is_always_redacted() {
    let header = RemoteAuthorizationHeader::from_owned_bytes(
        b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
    )
    .unwrap();
    assert_eq!(
        format!("{header:?}"),
        "RemoteAuthorizationHeader([REDACTED])"
    );
}

#[test]
fn malformed_authorization_fails_closed() {
    for authorization in [
        b"Basic 0123456789abcdef0123456789abcdef".as_slice(),
        b"Bearer short".as_slice(),
    ] {
        assert_eq!(
            RemoteAuthorizationHeader::from_owned_bytes(authorization.to_vec()).unwrap_err(),
            RemoteHttpBoundaryError::MissingOrInvalidAuthorization
        );
    }
}

#[test]
fn public_http_payload_never_contains_credentials() {
    let request: RemoteHttpRequestV1<()> = serde_json::from_value(serde_json::json!({
        "request": {
            "protocol_version": 1,
            "request_id": "request.remote",
            "brain_id": "brain.remote",
            "caller_node_id": "node.remote",
            "enrollment_revision": 1,
            "expected_authority": null,
            "sent_at": 10,
            "body": null
        }
    }))
    .unwrap();

    let json = serde_json::to_string(&request).unwrap();
    assert!(!json.contains("credential"));
    assert!(!json.contains("authorization"));
}

#[test]
fn credential_rejection_precedes_polling_the_json_body() {
    let admission_calls = Arc::new(AtomicUsize::new(0));
    let port_calls = Arc::new(AtomicUsize::new(0));
    let body_polls = Arc::new(AtomicUsize::new(0));
    let response = rejected_request(
        rejecting_router(
            RemoteCredentialAdmissionErrorV1::Rejected,
            Arc::clone(&admission_calls),
            Arc::clone(&port_calls),
        ),
        unpolled_body(&body_polls),
    );

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(admission_calls.load(Ordering::SeqCst), 1);
    assert_eq!(body_polls.load(Ordering::SeqCst), 0);
    assert_eq!(port_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn credential_authority_failures_share_one_concealed_response() {
    for error in [
        RemoteCredentialAdmissionErrorV1::Rejected,
        RemoteCredentialAdmissionErrorV1::Unavailable,
        RemoteCredentialAdmissionErrorV1::ResetRequired,
        RemoteCredentialAdmissionErrorV1::InsufficientCapability,
    ] {
        let response = rejected_request(
            rejecting_router(
                error,
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            ),
            Body::empty(),
        );
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[test]
fn typed_query_scope_must_match_the_pre_body_session() {
    let port_calls = Arc::new(AtomicUsize::new(0));
    let body = serde_json::to_vec(&query_request(repository_scope("snapshot.foreign"))).unwrap();
    let response = block_on(
        authenticated_router(Arc::clone(&port_calls)).oneshot(query_http_request(Body::from(body))),
    )
    .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(port_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn malformed_json_is_rejected_only_after_successful_pre_body_admission() {
    let port_calls = Arc::new(AtomicUsize::new(0));
    let response = block_on(
        authenticated_router(Arc::clone(&port_calls)).oneshot(query_http_request(Body::from("{"))),
    )
    .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(port_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exactly_bound_query_delegates_once() {
    let port_calls = Arc::new(AtomicUsize::new(0));
    let body = serde_json::to_vec(&query_request(repository_scope("snapshot.remote"))).unwrap();
    let response = authenticated_router(Arc::clone(&port_calls))
        .oneshot(query_http_request(Body::from(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(port_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn backup_http_route_binds_the_request_expiry_as_execution_deadline() {
    let port_calls = Arc::new(AtomicUsize::new(0));
    let controlled_deadline = Arc::new(AtomicI64::new(0));
    let writer = expected_authority();
    let body = RemoteHttpRequestV1 {
        request: RemoteProtocolRequestV1::new(
            RequestId::new("request.remote.backup").unwrap(),
            id::<BrainId>("brain.remote"),
            id::<BrainNodeId>("node.remote"),
            4,
            Some(writer.clone()),
            UtcMicros(20),
            BackupRequestV1 {
                operation_id: "backup.remote".to_owned(),
                expected: RecoveryAuthorityExpectationV1 {
                    brain_id: writer.brain_id.as_str().to_owned(),
                    shard_id: writer.shard_id.as_str().to_owned(),
                    generation_id: writer.generation_id.as_str().to_owned(),
                    authority_node_id: writer.authority_node_id.as_str().to_owned(),
                    placement_revision: writer.placement_revision.get(),
                    authority_epoch: writer.authority_epoch.0,
                },
                expires_at_micros: 40,
            },
        )
        .unwrap(),
    };
    let body = serde_json::to_vec(&body).unwrap();
    let response =
        deadline_capturing_router(Arc::clone(&port_calls), Arc::clone(&controlled_deadline))
            .oneshot(backup_http_request(Body::from(body)))
            .await
            .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(port_calls.load(Ordering::SeqCst), 1);
    assert_eq!(controlled_deadline.load(Ordering::SeqCst), 40);
}

#[test]
fn dropped_http_request_cancels_the_live_execution_signal() {
    let cancellation = CancellationSignal::active("cancel.remote.http.drop").unwrap();
    {
        let _cancel_on_drop = CancelRemoteRequestOnDropV1 {
            cancellation: cancellation.clone(),
            clock: fixed_remote_clock,
            armed: true,
        };
        assert!(!cancellation.is_cancelled());
    }
    assert_eq!(cancellation.cancelled_at(), Some(UtcMicros(20)));
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
