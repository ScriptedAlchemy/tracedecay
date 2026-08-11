use std::sync::Arc;

use tracedecay_application::retrieval::{
    GitTopologyAnchorAuthorityV2, GitTopologyAnchorPublicationOutcomeV2,
    GitTopologyAnchorPublicationV2, GitTopologyAnchorResolutionOutcomeV2,
    GitTopologyAnchorResolutionV2,
};
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV2, AnchorProvenanceRelationV2,
    AnchorSourceGenerationV2, CapabilityId, CommitId, CoverageReportV1, EvidenceClass,
    GitHubStackCapabilitySnapshotV1, GitHubStackCapabilityStateV1, GitTopologyAnchorTargetV1,
    ObservationScopeV1, PayloadAccessState, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectId, ProjectionGenerationId, ProviderId, RepositoryId, ResolutionAuthorizationV1,
    RetentionClass, RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    ScopeResolutionId, UtcMicros, VectorWatermark, WorktreeId,
};
use tracedecay_global_db::{
    RegisteredGitTopologyAnchorAuthorityV2, tests::harness::RegisteredGlobalDbTestRuntime,
};

const SHA: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn authorization() -> ResolutionAuthorizationV1 {
    ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new("scope.github-stack-anchor").unwrap(),
        privacy_domain_id: PrivacyDomainId::new("privacy.github-stack-anchor").unwrap(),
        access_policy_digest: AccessPolicyDigest::new(SHA).unwrap(),
        capability_id: CapabilityId::new("capability.github-stack-anchor").unwrap(),
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(SHA).unwrap(),
    }
}

fn record(
    owner: ObservationScopeV1,
    target: RetrievalAnchorTargetV2,
    source_generation: AnchorSourceGenerationV2,
    projection_generation: ProjectionGenerationId,
    source_anchors: Vec<AnchorLineageRefV2>,
) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target,
        owner,
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at: UtcMicros(200),
        evidence_class: EvidenceClass::ProviderDeclared,
        source_generation,
        projection_generation,
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: Vec::new(),
        source_anchors,
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.github-stack.provider-evidence.v1")
            .unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

#[tokio::test]
async fn degraded_capability_persists_through_the_v2_git_topology_authority() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let profile = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.github-stack-anchor.degraded").unwrap();
    let repository_id = RepositoryId::new("repository.github-stack-anchor.degraded").unwrap();
    let owner = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let generation_id = ProjectionGenerationId::new("generation.github-stack.degraded").unwrap();
    let source = record(
        owner.clone(),
        RetrievalAnchorTargetV2::ExactRepositoryCommit {
            repository_id: repository_id.clone(),
            commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        },
        AnchorSourceGenerationV2::Unknown,
        ProjectionGenerationId::new("generation.github-stack.source.degraded").unwrap(),
        Vec::new(),
    );
    let capability = GitHubStackCapabilitySnapshotV1::new(
        ProviderId::new("provider.github").unwrap(),
        project_id.clone(),
        repository_id,
        WorktreeId::new("worktree.github-stack-anchor.degraded").unwrap(),
        GitHubStackCapabilityStateV1::Degraded,
        generation_id.clone(),
        source.anchor_id().clone(),
    )
    .unwrap();
    let capability_generation = capability.generation();
    let capability_record = record(
        owner.clone(),
        RetrievalAnchorTargetV2::GitTopology(Box::new(
            GitTopologyAnchorTargetV1::GitHubStackCapability(capability),
        )),
        AnchorSourceGenerationV2::GitTopology(capability_generation),
        generation_id,
        vec![
            AnchorLineageRefV2::new(
                AnchorProvenanceRelationV2::Observed,
                source.anchor_id().clone(),
                owner.clone(),
            )
            .unwrap(),
        ],
    );
    let capability_anchor_id = capability_record.anchor_id().clone();
    let runtime =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id.clone())
            .await
            .unwrap();
    let database = runtime.project_database_arc().unwrap();
    let authority = RegisteredGitTopologyAnchorAuthorityV2::new(Arc::clone(&database));
    assert_eq!(
        authority
            .publish(
                GitTopologyAnchorPublicationV2::new(
                    owner.clone(),
                    vec![source, capability_record],
                )
                .unwrap(),
            )
            .await,
        Ok(GitTopologyAnchorPublicationOutcomeV2::Published)
    );
    drop(authority);
    drop(database);
    drop(runtime);

    let restarted =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id)
            .await
            .unwrap();
    let authority =
        RegisteredGitTopologyAnchorAuthorityV2::new(restarted.project_database_arc().unwrap());
    let resolved = authority
        .resolve(GitTopologyAnchorResolutionV2::new(owner, capability_anchor_id).unwrap())
        .await;
    assert!(matches!(
        resolved,
        Ok(GitTopologyAnchorResolutionOutcomeV2::Resolved(record))
            if matches!(record.target(), RetrievalAnchorTargetV2::GitTopology(target)
                if matches!(target.as_ref(), GitTopologyAnchorTargetV1::GitHubStackCapability(capability)
                    if capability.state == GitHubStackCapabilityStateV1::Degraded))
    ));
}
