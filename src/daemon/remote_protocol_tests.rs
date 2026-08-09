use std::sync::Arc;

use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentAdmissionEvidenceV1,
};
use tracedecay_application::remote::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalEvidenceV1,
    PendingLocalObservationsV1, QueryManifestBindingV1, RemoteCompletenessV1, RemoteFreshnessV1,
    RemoteQueryCompositionV1, ShardCoverageStateV1, ShardQueryContributionV1,
};
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAdmissionErrorV1, RemoteCredentialAdmissionPortV1,
    RemoteCredentialAdmissionServiceV1, RemoteCredentialUseV1,
};
use tracedecay_application::remote::query::{RemoteExactObservationResultV1, RemoteQueryResultV1};
use tracedecay_application::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainNodeId, ComponentVersion, CoverageStateV1, EnrollmentGrantV1, EntityId,
    ManifestDigest, ObservedTernaryV1, ProjectId, RefId, RemoteCapabilityV1,
    RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1, RepositoryId,
    RepositoryStateSnapshotId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};

use super::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;

struct TestRemoteKeyring(Arc<RemoteSpoolKeyV1>);

impl RemoteSpoolKeyringV1 for TestRemoteKeyring {
    fn active_key(&self) -> Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1> {
        Ok(Arc::clone(&self.0))
    }

    fn key(
        &self,
        revision: u64,
    ) -> Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1> {
        Ok((revision == self.0.revision()).then(|| Arc::clone(&self.0)))
    }
}

fn remote_query_result(
    coverage: ShardCoverageStateV1,
    pending_local: PendingLocalEvidenceV1,
) -> RemoteQueryResultV1 {
    RemoteQueryResultV1 {
        composition: RemoteQueryCompositionV1 {
            contributions: vec![ShardQueryContributionV1 {
                manifest: QueryManifestBindingV1 {
                    brain_id: "brain.remote-coverage".to_owned(),
                    shard_id: "shard.remote-coverage".to_owned(),
                    generation_id: "generation.remote-coverage".to_owned(),
                    schema_digest: [1; 32],
                    watermark_sequence: 1,
                    placement_revision: 1,
                    authority_epoch: 1,
                    cache_age_millis: 0,
                    cache_lag_commits: 0,
                },
                integrity: IntegrityClaimV1::Verified,
                authenticity: AuthenticityClaimV1::Authenticated,
                freshness: RemoteFreshnessV1::Current,
                completeness: RemoteCompletenessV1::Complete,
                authorization: AuthorizationClaimV1::Authorized,
                coverage,
                authority_receipt: None,
                value: None,
                reason_code: (coverage != ShardCoverageStateV1::Complete)
                    .then(|| "remote_shard_degraded".to_owned()),
            }],
            pending_local,
            coverage,
        },
        observation: RemoteExactObservationResultV1::NotFound,
    }
}

#[test]
fn remote_query_coverage_preserves_real_shard_and_pending_counts() {
    let result = remote_query_result(
        ShardCoverageStateV1::Stale,
        PendingLocalObservationsV1 {
            count: 3,
            oldest_age_millis: Some(9),
            has_sequence_gap: false,
            has_quarantined: false,
        }
        .into(),
    );
    result.validate().expect("valid stale remote query result");

    let observation = super::remote_protocol::remote_query_result_observation(
        "request.remote-coverage",
        1,
        &result,
        ObservedTernaryV1::Yes,
    );

    assert_eq!(observation.expected_shards, Some(1));
    assert_eq!(observation.observed_shards, Some(1));
    assert_eq!(observation.pending_local_evidence, Some(3));
    assert_eq!(observation.terminal_succeeded, ObservedTernaryV1::Yes);
    assert_eq!(observation.coverage, CoverageStateV1::Stale);
    assert_eq!(
        observation.unavailable_reason.as_deref(),
        Some("pending_local_evidence")
    );
}

#[test]
fn remote_query_coverage_does_not_fabricate_unavailable_pending_count() {
    let result = remote_query_result(
        ShardCoverageStateV1::Unknown,
        PendingLocalEvidenceV1::Unavailable {
            reason: tracedecay_application::remote::composition::PendingLocalUnavailableReasonV1::AuthorityUnavailable,
        },
    );
    result
        .validate()
        .expect("valid unavailable remote query result");

    let observation = super::remote_protocol::remote_query_result_observation(
        "request.remote-coverage-unavailable",
        1,
        &result,
        ObservedTernaryV1::Unknown,
    );

    assert_eq!(observation.pending_local_evidence, None);
    assert_eq!(observation.terminal_succeeded, ObservedTernaryV1::Unknown);
    assert_eq!(observation.coverage, CoverageStateV1::Unknown);
    assert_eq!(
        observation.unavailable_reason.as_deref(),
        Some("pending_local_authority_unavailable")
    );
}

pub(crate) fn grant(
    brain_id: tracedecay_domain::BrainId,
    node_id: BrainNodeId,
    secret: &[u8],
) -> EnrollmentGrantV1 {
    EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote-registry").expect("grant identity"),
        brain_id,
        node_id,
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret)
            .expect("credential fingerprint"),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: [RemoteCapabilityV1::Query].into_iter().collect(),
        scope: RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote-registry").expect("project identity"),
            repository_id: RepositoryId::new("repository.remote-registry")
                .expect("repository identity"),
            worktree_id: WorktreeId::new("worktree.remote-registry").expect("worktree identity"),
            reference: Some(RefId::new("refs/heads/main").expect("reference identity")),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-registry")
                .expect("snapshot identity"),
        },
    }
}

pub(crate) fn admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .expect("resolved scope");
    let grant_digest = canonical_sha256(grant).expect("grant digest");
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str())
                .expect("capability grant identity"),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote-registry",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote-registry.v1").expect("policy component"),
            )
            .expect("policy decision"),
            revalidated_at: UtcMicros(2),
        },
        ActorId::new("actor.remote-registry").expect("actor identity"),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("configuration digest"),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("catalog digest"),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).expect("privacy digest"),
        Deadline::new(UtcMicros(100)).expect("deadline"),
    )
    .expect("enrollment admission")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_node_populates_exact_prebody_credential_route_and_shutdown_cancels_it() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        super::transport::DaemonEndpoint::Unix(profile_root.join("remote-credential.sock"));
    #[cfg(not(unix))]
    let endpoint = super::transport::default_loopback_endpoint();
    let daemon_authority =
        super::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("daemon authority");
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote credential registry",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let node_id = BrainNodeId::new("node.remote-registry").expect("node identity");
    let secret = [9_u8; 32];
    let grant = grant(identity.brain_id().clone(), node_id.clone(), &secret);
    registry
        .provision_remote_node(grant.clone(), admission(&grant))
        .await
        .expect("authenticated first RemoteNode provisioning");
    let keyring = Arc::new(TestRemoteKeyring(Arc::new(
        RemoteSpoolKeyV1::from_secret_bytes(1, vec![7; 32]).expect("remote spool key"),
    )));
    let storage = registry
        .remote_node_storage(node_id.clone(), keyring.clone())
        .await
        .expect("remote node storage");
    let credentials = registry.remote_credential_authority();
    assert_eq!(
        credentials.register_storage(
            BrainNodeId::new("node.remote-registry.other").expect("other node identity"),
            storage.clone(),
        ),
        Err(super::remote_protocol::DaemonRemoteCredentialRegistryErrorV1::IdentityConflict)
    );
    drop(keyring);
    let service = RemoteCredentialAdmissionServiceV1::new(
        super::remote_protocol::DaemonRemoteCredentialLookupV1::new(Arc::clone(&credentials)),
    );
    let session = service
        .admit_before_body(
            &OpaqueRemoteCredential::new(secret).expect("opaque credential"),
            RemoteCredentialUseV1::InitialEnrollment,
            UtcMicros(10),
        )
        .expect("pre-body credential admission");
    assert_eq!(session.brain_id(), identity.brain_id());
    assert_eq!(session.node_id(), &node_id);

    credentials.cancel();
    assert_eq!(
        service.admit_before_body(
            &OpaqueRemoteCredential::new(secret).expect("opaque credential"),
            RemoteCredentialUseV1::InitialEnrollment,
            UtcMicros(11),
        ),
        Err(RemoteCredentialAdmissionErrorV1::Unavailable)
    );
}
