use std::sync::Arc;

use tracedecay_contracts::remote::auth::OpaqueRemoteCredential;
use tracedecay_contracts::remote::credential_admission::{
    RemoteCredentialAdmissionErrorV1, RemoteCredentialAdmissionPortV1,
    RemoteCredentialAdmissionServiceV1, RemoteCredentialUseV1,
};
use tracedecay_contracts::remote::query::{
    RemoteQueryAuthorizationDecisionV1, RemoteQueryPolicyRecordV1,
};
use tracedecay_contracts::remote::replay::{
    RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidenceV1,
};
use tracedecay_contracts::remote::status::{
    RemoteOperationalReadinessV1, RemoteOperationalStatusReadV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, DisclosureClass, DoctorCoverageCompletenessV1,
    PolicyDecisionRef, RemoteListenerReadV1, RemoteOperationalReadV1, ResolvedScope,
};
use tracedecay_domain::{
    BrainNodeId, ComponentVersion, CurrentRemoteAuthorityStateV1, EntityId, ManifestDigest,
    RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1, UtcMicros,
    canonical_sha256,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};

use super::DaemonSessionRuntimeRegistryV1;
use super::remote_fixtures::{
    remote_enrollment_admission_fixture, remote_enrollment_grant_fixture,
};

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

pub(crate) fn replay_policy(scope: &RemoteRepositoryScopeV1) -> RemoteReplayPolicyEvidenceV1 {
    let digest = ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("policy digest");
    RemoteReplayPolicyEvidenceV1 {
        scope: ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            scope.reference.clone(),
        )
        .expect("resolved scope"),
        repository_scope: scope.clone(),
        policy_revision: 1,
        decision: RemoteReplayPolicyDecisionV1::Admit,
        policy: PolicyDecisionRef::new(
            "policy.remote-registry.replay",
            1,
            digest.clone(),
            ComponentVersion::new("policy.remote-registry.replay.v1").expect("policy component"),
        )
        .expect("policy decision"),
        configuration_digest: digest.clone(),
        catalog_digest: digest.clone(),
        privacy_digest: digest,
        revalidated_at: UtcMicros(1),
    }
}

pub(crate) fn query_policy(scope: &RemoteRepositoryScopeV1) -> RemoteQueryPolicyRecordV1 {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        scope.reference.clone(),
    )
    .expect("resolved scope");
    let digest = ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest");
    RemoteQueryPolicyRecordV1 {
        repository_scope: scope.clone(),
        authority: AuthorityReceipt {
            grant_id: CapabilityGrantId::new("grant.remote-registry.query")
                .expect("capability grant identity"),
            grant_revision: 1,
            grant_digest: digest.clone(),
            authorized_scope_digest: resolved.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote-registry.query",
                1,
                digest,
                ComponentVersion::new("policy.remote-registry.query.v1").expect("policy component"),
            )
            .expect("policy decision"),
            revalidated_at: UtcMicros(1),
        },
        scope: resolved,
        policy_revision: 1,
        decision: RemoteQueryAuthorizationDecisionV1::Allow,
        revalidated_at: UtcMicros(1),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_operational_read_observes_mounted_authorities_and_fails_typed_when_unmounted() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(
        profile_root.join("remote-operational.sock"),
    );
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote operational read",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let credentials = registry.remote_credential_authority();

    // No listener and no registered node: the optional remote plane is
    // truthfully unconfigured, not unavailable and not an empty success.
    assert_eq!(
        registry.remote_operational_status(),
        RemoteOperationalStatusReadV1::Unconfigured
    );
    assert_eq!(
        credentials.operational_status().doctor_read(),
        RemoteOperationalReadV1::Unconfigured
    );

    // Mount a real node store and serve the listener: the read must observe
    // the mounted enrollment, spool, and recovery-journal authorities.
    let node_id = BrainNodeId::new("node.remote-operational").expect("node identity");
    let secret = [11_u8; 32];
    let grant =
        remote_enrollment_grant_fixture(identity.brain_id().clone(), node_id.clone(), &secret);
    registry
        .provision_remote_node(grant.clone(), remote_enrollment_admission_fixture(&grant))
        .await
        .expect("authenticated RemoteNode provisioning");
    credentials.publish_listener_serving();
    let RemoteOperationalStatusReadV1::Observed {
        listener,
        status,
        coverage,
    } = registry.remote_operational_status()
    else {
        panic!("mounted remote authorities must produce an observed read");
    };
    assert_eq!(listener, RemoteListenerReadV1::Serving);
    assert_eq!(coverage, DoctorCoverageCompletenessV1::Complete);
    assert!(
        !status.enrollment_configured,
        "an enrollment grant alone is not a completed enrollment"
    );
    assert_eq!(status.readiness, RemoteOperationalReadinessV1::Unconfigured);
    assert_eq!(status.spool.pending_count, 0);
    assert_eq!(status.spool.quarantined_count, 0);
    assert!(!status.spool.has_sequence_gap);
    assert!(!status.failover_in_progress);
    assert!(!status.recovery_required);
    assert!(
        matches!(
            status.authority,
            CurrentRemoteAuthorityStateV1::Unavailable { .. }
        ),
        "a never-published authority is a typed unavailable state"
    );
    assert!(matches!(
        credentials.operational_status().doctor_read(),
        RemoteOperationalReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            ..
        }
    ));

    // A degraded listener task stays visible without erasing the observation.
    credentials.publish_listener_degraded();
    assert!(matches!(
        registry.remote_operational_status(),
        RemoteOperationalStatusReadV1::Observed {
            listener: RemoteListenerReadV1::Degraded,
            ..
        }
    ));

    // Unmounting the remote plane is a typed unavailable read, never an
    // empty or unconfigured success.
    credentials.cancel();
    assert_eq!(
        registry.remote_operational_status(),
        RemoteOperationalStatusReadV1::Unavailable
    );
    assert_eq!(
        credentials.operational_status().doctor_read(),
        RemoteOperationalReadV1::Unavailable
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_node_populates_exact_prebody_credential_route_and_shutdown_cancels_it() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(
        profile_root.join("remote-credential.sock"),
    );
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
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
    let grant =
        remote_enrollment_grant_fixture(identity.brain_id().clone(), node_id.clone(), &secret);
    registry
        .provision_remote_node(grant.clone(), remote_enrollment_admission_fixture(&grant))
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
        Err(crate::DaemonRemoteCredentialRegistryErrorV1::IdentityConflict)
    );
    drop(keyring);
    let service = RemoteCredentialAdmissionServiceV1::new(
        crate::DaemonRemoteCredentialLookupV1::new(Arc::clone(&credentials)),
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

fn joined_discovery(
    brain_id: tracedecay_domain::BrainId,
    node_id: BrainNodeId,
    secret: &[u8],
) -> tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1 {
    let mut grant = remote_enrollment_grant_fixture(brain_id, node_id, &[3_u8; 32]);
    grant.capabilities = [
        RemoteCapabilityV1::CaptureOffline,
        RemoteCapabilityV1::DiscoverAuthority,
    ]
    .into_iter()
    .collect();
    let enrollment = tracedecay_domain::EnrollmentCredentialRecordV1 {
        enrollment_id: EntityId::new("enrollment.remote-joined").expect("enrollment identity"),
        brain_id: grant.brain_id.clone(),
        node_id: grant.node_id.clone(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret)
            .expect("credential fingerprint"),
        revision: 1,
        issued_at: UtcMicros(10),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: grant.capabilities.clone(),
        scope: grant.scope.clone(),
    };
    let receipt = tracedecay_contracts::remote::auth::RemoteEnrollmentCommitReceiptV1 {
        admission: remote_enrollment_admission_fixture(&grant),
        prior_grant_digest: canonical_sha256(&grant).expect("grant digest"),
        input_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
            .expect("input digest"),
        committed_state_digest: canonical_sha256(&enrollment).expect("enrollment digest"),
        consumed_at: enrollment.issued_at,
        budget: tracedecay_contracts::OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: 1,
            elapsed_micros: 0,
        },
        enrollment,
    };
    let writer: tracedecay_contracts::remote::capture::RemoteWriterAuthorityV1 =
        serde_json::from_value(serde_json::json!({
            "project_id": grant.scope.project_id,
            "scope": grant.scope,
            "authority": {
                "fence": {
                    "brain_id": grant.brain_id,
                    "shard_id": "shard.remote-joined",
                    "generation_id": "generation.remote-joined",
                    "placement_revision": 1,
                    "authority_epoch": 1,
                    "authority_node_id": grant.node_id,
                },
                "credential_revision": 1,
                "observed_at": 10,
            },
        }))
        .expect("writer authority");
    tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1 {
        policy: replay_policy(&grant.scope),
        writer,
        enrollment: receipt,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joined_node_store_serves_only_offline_capture_for_its_member_brain() {
    let temporary = tempfile::tempdir().expect("temporary profile parent");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        tracedecay_daemon_protocol::DaemonEndpoint::Unix(profile_root.join("remote-joined.sock"));
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote node join",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("session runtime registry");
    let credentials = registry.remote_credential_authority();
    let authority_brain =
        tracedecay_domain::BrainId::new("brain.remote-joined-authority").expect("brain identity");
    let node_id = BrainNodeId::new("node.remote-joined").expect("node identity");
    let secret = [13_u8; 32];
    let presented = || OpaqueRemoteCredential::new(secret).expect("opaque credential");

    assert_eq!(
        registry
            .join_remote_node(&joined_discovery(
                identity.brain_id().clone(),
                node_id.clone(),
                &secret,
            ))
            .await,
        Err(crate::RemoteNodeOperatorErrorV1::BrainIdentityConflict),
        "joining the profile's own Brain is provisioning's job, never a join"
    );

    let discovery = joined_discovery(authority_brain.clone(), node_id.clone(), &secret);
    for attempt in ["join", "rerun"] {
        registry
            .join_remote_node(&discovery)
            .await
            .unwrap_or_else(|error| panic!("{attempt}: {error}"));
    }
    let joined = credentials
        .storage_for_presented(RemoteCredentialUseV1::CaptureOffline, &presented())
        .expect("the joined store serves node-local offline capture");
    assert_eq!(joined.storage.member_brain(), &authority_brain);
    assert_eq!(&joined.binding.shard_id.brain_id, identity.brain_id());
    for use_case in [
        RemoteCredentialUseV1::TransferFrame,
        RemoteCredentialUseV1::Replay,
        RemoteCredentialUseV1::Query,
        RemoteCredentialUseV1::Promote,
        RemoteCredentialUseV1::DiscoverAuthority,
    ] {
        assert!(
            credentials
                .storage_for_presented(use_case, &presented())
                .is_err(),
            "a joined store must never answer {use_case:?} as its Brain's authority"
        );
    }
    let session = RemoteCredentialAdmissionServiceV1::new(
        crate::DaemonRemoteCredentialLookupV1::new(Arc::clone(&credentials)),
    )
    .admit_before_body(
        &presented(),
        RemoteCredentialUseV1::CaptureOffline,
        UtcMicros(20),
    )
    .expect("the adopted enrollment admits the node's own credential");
    assert_eq!(
        (session.brain_id(), session.node_id()),
        (&authority_brain, &node_id)
    );

    let mut rival = discovery.clone();
    rival.enrollment.enrollment.expires_at = UtcMicros(90);
    assert_eq!(
        registry.join_remote_node(&rival).await,
        Err(crate::RemoteNodeOperatorErrorV1::Conflict),
        "a different record for the adopted enrollment never replaces it"
    );
    let own = remote_enrollment_grant_fixture(identity.brain_id().clone(), node_id, &[14_u8; 32]);
    assert_eq!(
        registry
            .provision_remote_node(own.clone(), remote_enrollment_admission_fixture(&own))
            .await,
        Err(crate::RemoteNodeOperatorErrorV1::BrainIdentityConflict),
        "a joined node store never becomes a hosted one"
    );
}
