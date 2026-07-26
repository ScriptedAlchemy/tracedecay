use std::collections::BTreeMap;

use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    CheckSnapshotAnchorRefV1, CiFailureBranchEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRunIdentityV1,
    CoverageReportV1, EvidenceAvailabilityV1, EvidenceClass, FeedbackScopeV1, GitCommitIdentityV1,
    GitCoverageV1, GitHeadStateV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewReadOperationV1,
    GitIndexCommitIntentV1, GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexReceiptId, GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, GitTopologyAnchorTargetV1, GitTopologyGenerationRefV1,
    GitTopologySourceRoleV1, IntegrationReceiptAnchorRefV1, ManifestDigest,
    NativeGitObjectAnchorRefV1, NativeGitObjectKindV1, ObservationScopeV1, PayloadAccessState,
    PreflightPreviewAnchorRefV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId,
    ProjectionGenerationId, ProviderId, PullRequestSnapshotAnchorRefV1, RefId,
    RefSnapshotAnchorRefV1, RefSnapshotKindV1, RepositoryCaptureAnchorRefV1,
    RepositoryDirtyStateV1, RepositoryEvidenceV1, RepositoryId, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryProvenanceV1, RepositoryRemoteIdentityV1,
    RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
    ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorRecordV2,
    RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, ScopeResolutionId, ShardId, UtcMicros,
    VectorWatermark, WorktreeCaptureAnchorRefV1, WorktreeId, derive_git_topology_anchor_id,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture oid is canonical")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn snapshot(epoch: u64, head: char) -> RepositoryStateSnapshotV1 {
    RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        Some(id::<WorktreeId>("worktree.fixture")),
        epoch,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid(head),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::TrackedDirty,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        GitOperationStateV1::None,
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(i64::try_from(epoch).unwrap()),
        GitCoverageV1::complete(),
    )
    .unwrap()
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('7'),
    )
    .unwrap()
}

fn generation() -> tracedecay_domain::GenerationBoundRepositoryProvenanceV1 {
    let evidence = RepositoryEvidenceV1::new(
        EvidenceAvailabilityV1::Known(id::<RefId>("refs/heads/main")),
        EvidenceAvailabilityV1::Known(id(oid('a').as_str())),
        EvidenceAvailabilityV1::Known(id(oid('c').as_str())),
        EvidenceAvailabilityV1::Known(id(digest('b').as_str())),
        RepositoryRemoteIdentityV1::Known(id(digest('8').as_str())),
        EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty),
    )
    .unwrap();
    let capture = RepositoryProvenanceV1::new(
        id("repository.fixture"),
        Some(id("project.fixture")),
        Some(id("worktree.fixture")),
        id(digest('9').as_str()),
        evidence,
        UtcMicros(1),
    )
    .unwrap();
    tracedecay_domain::GenerationBoundRepositoryProvenanceV1::new(
        id("projection.repository.fixture"),
        capture,
        None,
    )
    .unwrap()
}

fn authorization() -> ResolutionAuthorizationV1 {
    ResolutionAuthorizationV1 {
        resolved_scope_id: id::<ScopeResolutionId>("scope.fixture"),
        privacy_domain_id: id::<PrivacyDomainId>("privacy.fixture"),
        access_policy_digest: id::<AccessPolicyDigest>(digest('a').as_str()),
        capability_id: id::<CapabilityId>("capability.fixture"),
        canonical_request_digest: id::<PrivacyDomainBoundLocatorDigest>(digest('b').as_str()),
    }
}

fn record(target: GitTopologyAnchorTargetV1) -> RetrievalAnchorRecordV2 {
    let owner = ObservationScopeV1::Project {
        project_id: id("project.fixture"),
    };
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        source_generation: AnchorSourceGenerationV2::GitTopology(target.generation()),
        target: RetrievalAnchorTargetV2::GitTopology(Box::new(target)),
        owner,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        projection_generation: id::<ProjectionGenerationId>("projection.anchor.fixture"),
        projection_watermark: VectorWatermark {
            components: BTreeMap::from([(id::<ShardId>("shard.fixture"), 1)]),
        },
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

#[test]
fn worktree_snapshot_anchor_rekeys_on_exact_generation_change() {
    let first_repository = RepositoryCaptureAnchorRefV1::new(&generation(), &snapshot(1, 'a'))
        .expect("first repository binding");
    let second_repository = RepositoryCaptureAnchorRefV1::new(&generation(), &snapshot(2, 'e'))
        .expect("second repository binding");
    let second_snapshot_id = second_repository.snapshot_id.clone();
    let first = GitTopologyAnchorTargetV1::WorktreeCapture(
        WorktreeCaptureAnchorRefV1::new(first_repository).unwrap(),
    );
    let second = GitTopologyAnchorTargetV1::WorktreeCapture(
        WorktreeCaptureAnchorRefV1::new(second_repository).unwrap(),
    );

    let first_record = record(first);
    let second_record = record(second);
    assert!(
        first_record
            .anchor_id()
            .as_str()
            .starts_with("retrieval.v3.")
    );
    assert_ne!(first_record.anchor_id(), second_record.anchor_id());

    let mut tampered = serde_json::to_value(&first_record).unwrap();
    tampered["source_generation"]["generation"]["binding"]["snapshot_id"] =
        serde_json::to_value(second_snapshot_id).unwrap();
    assert!(serde_json::from_value::<RetrievalAnchorRecordV2>(tampered).is_err());
}

#[test]
fn moving_ref_creates_a_new_target_without_retargeting_the_old_one() {
    let repository = RepositoryCaptureAnchorRefV1::new(&generation(), &snapshot(1, 'a')).unwrap();
    let commit_a = NativeGitObjectAnchorRefV1::new(
        repository.clone(),
        NativeGitObjectKindV1::Commit,
        oid('a'),
    )
    .unwrap();
    let commit_b = NativeGitObjectAnchorRefV1::new(
        repository.clone(),
        NativeGitObjectKindV1::Commit,
        oid('e'),
    )
    .unwrap();
    let first = GitTopologyAnchorTargetV1::RefSnapshot(
        RefSnapshotAnchorRefV1::new(
            repository.clone(),
            id("refs/heads/main"),
            RefSnapshotKindV1::Symbolic,
            Some(commit_a),
            digest('1'),
        )
        .unwrap(),
    );
    let moved = GitTopologyAnchorTargetV1::RefSnapshot(
        RefSnapshotAnchorRefV1::new(
            repository,
            id("refs/heads/main"),
            RefSnapshotKindV1::Symbolic,
            Some(commit_b),
            digest('2'),
        )
        .unwrap(),
    );
    let owner = ObservationScopeV1::Project {
        project_id: id("project.fixture"),
    };

    assert_ne!(
        derive_git_topology_anchor_id(&owner, &first).unwrap(),
        derive_git_topology_anchor_id(&owner, &moved).unwrap()
    );
}

#[test]
fn pr13_pull_request_and_ci_findings_keep_exact_commit_and_generation_identity() {
    let scope = FeedbackScopeV1 {
        project_id: id("project.fixture"),
        repository_id: id("repository.fixture"),
        worktree_id: id("worktree.fixture"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: id("commit.head"),
    };
    let ingress = GitHubReviewIngressResultV1 {
        provider: id::<ProviderId>("provider.github"),
        scope: scope.clone(),
        pull_request_id: GitHubPullRequestIdV1::new("pr.42").unwrap(),
        provider_base_commit_id: id("commit.base"),
        provider_head_commit_id: scope.head_commit_id.clone(),
        merge_base_commit_id: id("commit.merge-base"),
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
        coverage: GitHubReviewCoverageV1::Complete,
        items: vec![],
        fetched_at: UtcMicros(1),
    };
    let pull_request = PullRequestSnapshotAnchorRefV1::from_ingress(
        &ingress,
        id("anchor.pull-request.observation"),
    )
    .unwrap();
    assert_eq!(pull_request.head_commit_id, scope.head_commit_id);

    let generation = CiFailureGenerationEvidenceV1 {
        generation_id: id("code-generation.fixture"),
        retrieval_anchor_id: id("anchor.code-generation"),
    };
    let localization = CiFailureLocalizationResultV1 {
        provider: id("provider.ci"),
        run: CiFailureRunIdentityV1 {
            workflow_id: "workflow.1".to_owned(),
            job_id: "job.1".to_owned(),
            check_suite_id: "suite.1".to_owned(),
            check_run_id: "check.1".to_owned(),
            run_id: "run.1".to_owned(),
            attempt_id: "attempt.1".to_owned(),
        },
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.fixture".to_owned(),
            parser_version: "1".to_owned(),
        },
        state: CiFailureLocalizationStateV1::Complete,
        coverage: CiFailureCoverageV1::Complete,
        source_degradation: None,
        failure_kind: CiFailureKindV1::InfrastructureFailure,
        failure_anchor: id("anchor.ci.failure"),
        branch: CiFailureBranchEvidenceV1 {
            scope,
            provider_head_commit_id: id("commit.head"),
        },
        generation: Some(generation.clone()),
        symbol: None,
        callers: vec![],
        tests: vec![],
        rerun_hints: vec![],
        observed_at: UtcMicros(2),
    };
    let check = CheckSnapshotAnchorRefV1::from_localization(&localization).unwrap();
    assert_eq!(
        check.generation_ref(),
        GitTopologyGenerationRefV1::CodeGeneration {
            generation_id: generation.generation_id,
            retrieval_anchor_id: generation.retrieval_anchor_id,
            commit_id: id("commit.head"),
        }
    );
}

#[test]
fn pr11_preview_apply_and_integration_receipts_remain_exact_and_ordered() {
    let snapshot = snapshot(1, 'a');
    let repository = RepositoryCaptureAnchorRefV1::new(&generation(), &snapshot).unwrap();
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: UtcMicros(1),
    };
    let intent = GitIndexCommitIntentV1::new(
        "fixture commit".to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .unwrap();
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        GitIndexPreviewId::new("preview.fixture").unwrap(),
        GitIndexTransactionOperationV1::CommitIndex,
        snapshot,
        repository.snapshot_digest.clone(),
        vec![],
        Some(oid('c')),
        Some(&intent),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(2),
        UtcMicros(20),
    )
    .unwrap();
    let preflight = PreflightPreviewAnchorRefV1::new(repository, &preview).unwrap();
    let receipt = GitIndexTransactionReceiptV1::new(
        GitIndexReceiptId::new("receipt.fixture").unwrap(),
        GitIndexTransactionId::new("transaction.fixture").unwrap(),
        &preview,
        digest('f'),
        Some(oid('c')),
        Some(oid('e')),
        Some(oid('e')),
        GitIndexReceiptOutcomeV1::Committed,
        UtcMicros(3),
    )
    .unwrap();
    let apply = tracedecay_domain::ApplyReceiptAnchorRefV1::new(
        preflight,
        id("anchor.preflight"),
        &receipt,
    )
    .unwrap();
    let integration = IntegrationReceiptAnchorRefV1::new(
        apply,
        vec![(
            GitTopologySourceRoleV1::Decision,
            id("anchor.integration.decision"),
        )],
    )
    .unwrap();

    assert_eq!(
        integration.sources[0].role,
        GitTopologySourceRoleV1::Preflight
    );
    assert_eq!(integration.sources[1].source_ordinal, 1);
    assert_eq!(
        integration.apply.generation(),
        GitTopologyGenerationRefV1::GitReceipt {
            receipt_id: receipt.receipt_id,
            preview_id: receipt.preview_id,
            commit_id: receipt.created_commit,
        }
    );
}
