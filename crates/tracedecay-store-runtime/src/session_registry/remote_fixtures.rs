//! Remote enrollment fixtures shared by store-runtime and composition-root tests.

use tracedecay_contracts::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, ManifestDigest, ProjectId,
    RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId, canonical_sha256,
};

pub fn remote_enrollment_grant_fixture(
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

pub fn remote_enrollment_admission_fixture(
    grant: &EnrollmentGrantV1,
) -> RemoteEnrollmentAdmissionEvidenceV1 {
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
