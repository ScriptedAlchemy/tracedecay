use std::collections::BTreeMap;

use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV2, AnchorProvenanceRelationV2,
    AnchorSourceGenerationV2, CapabilityId, CheckSnapshotAnchorRefV1, CiFailureBranchEvidenceV1,
    CiFailureCoverageV1, CiFailureGenerationEvidenceV1, CiFailureKindV1,
    CiFailureLocalizationResultV1, CiFailureLocalizationStateV1, CiFailureParserIdentityV1,
    CiFailureRunIdentityV1, CommitId, CoverageReportV1, EvidenceAvailabilityV1, EvidenceClass,
    FeedbackScopeV1, GitCommitIdentityV1, GitCoverageV1, GitHeadStateV1, GitHubPullRequestIdV1,
    GitHubPullRequestSnapshotV1, GitHubPullRequestStateV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewReadOperationV1,
    GitHubStackCapabilitySnapshotV1, GitHubStackCapabilityStateV1, GitHubStackLayerSnapshotV1,
    GitHubStackSnapshotV1, GitIndexCommitIntentV1, GitIndexPreviewDispositionV1, GitIndexPreviewId,
    GitIndexPreviewV1, GitIndexReceiptId, GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1,
    GitIndexTransactionId, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitObjectFormatV1, GitOidV1, GitOperationStateV1, GitTopologyAnchorTargetV1,
    GitTopologyGenerationRefV1, GitTopologySourceRoleV1, IntegrationReceiptAnchorRefV1,
    ManifestDigest, NativeGitObjectAnchorRefV1, NativeGitObjectKindV1, ObservationScopeV1,
    PayloadAccessState, PreflightPreviewAnchorRefV1, PrivacyDomainBoundLocatorDigest,
    PrivacyDomainId, ProjectId, ProjectionGenerationId, PullRequestSnapshotAnchorRefV1, RefId,
    RefSnapshotAnchorRefV1, RefSnapshotKindV1, RepositoryCaptureAnchorRefV1,
    RepositoryDirtyStateV1, RepositoryEvidenceV1, RepositoryId, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryProvenanceV1, RepositoryRemoteIdentityV1,
    RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
    ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorRecordV2,
    RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, ScopeResolutionId, ShardId, UtcMicros,
    VectorWatermark, WorktreeCaptureAnchorRefV1, WorktreeId, canonical_sha256,
    derive_git_topology_anchor_id,
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
        Some(digest('0')),
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
    // A retained record must carry lineage to every ordered source the target
    // declares, so the helper derives it rather than letting callers drift.
    let source_anchors = target
        .ordered_sources()
        .iter()
        .map(|source| {
            AnchorLineageRefV2::new(
                AnchorProvenanceRelationV2::Observed,
                source.anchor_id.clone(),
                owner.clone(),
            )
            .expect("ordered source lineage is canonical")
        })
        .collect();
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
        source_anchors,
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

/// A code-generation finding is only actionable against the commit it was
/// observed on, so the derived generation ref must carry the branch's head
/// commit rather than only the generation evidence it was handed.
#[test]
fn generation_ref_binds_the_observed_head_commit() {
    let head_commit = id::<CommitId>("commit.head");
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
            scope: FeedbackScopeV1 {
                project_id: id("project.fixture"),
                repository_id: id("repository.fixture"),
                worktree_id: id("worktree.fixture"),
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: head_commit.clone(),
            },
            provider_head_commit_id: head_commit.clone(),
        },
        generation: Some(generation),
        symbol: None,
        callers: vec![],
        tests: vec![],
        rerun_hints: vec![],
        observed_at: UtcMicros(2),
    };

    let check = CheckSnapshotAnchorRefV1::from_localization(&localization).unwrap();
    let GitTopologyGenerationRefV1::CodeGeneration { commit_id, .. } = check.generation_ref()
    else {
        panic!("a code-generation finding must derive a code-generation ref");
    };
    assert_eq!(commit_id, head_commit);
}

/// Receipt sources are replayed in order, so the derived source list must be
/// ordered by role and ordinal regardless of the order the caller supplied.
#[test]
fn integration_receipt_sources_stay_ordered() {
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
}

fn github_stack_capability(state: GitHubStackCapabilityStateV1) -> GitHubStackCapabilitySnapshotV1 {
    GitHubStackCapabilitySnapshotV1::new(
        id("provider.github"),
        id("project.fixture"),
        id("repository.fixture"),
        id("worktree.fixture"),
        state,
        id("projection.github-stack-capability.1"),
        id("anchor.github-stack-capability.1"),
    )
    .expect("capability snapshot is canonical")
}

fn github_stack_layer(
    provider_position: u32,
    repository: &str,
    pull_request: &str,
    refs: (&str, &str),
    commits: (&str, &str),
    source_anchor: &str,
) -> GitHubStackLayerSnapshotV1 {
    let (base_ref, head_ref) = refs;
    let (base_commit, head_commit) = commits;
    let result = GitHubReviewIngressResultV1 {
        provider: id("provider.github"),
        scope: FeedbackScopeV1 {
            project_id: id("project.fixture"),
            repository_id: id(repository),
            worktree_id: id("worktree.fixture"),
            branch_ref: head_ref.to_owned(),
            head_commit_id: id(head_commit),
        },
        pull_request_id: GitHubPullRequestIdV1::new(pull_request).unwrap(),
        provider_base_commit_id: id(base_commit),
        provider_head_commit_id: id(head_commit),
        merge_base_commit_id: id(base_commit),
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
        coverage: GitHubReviewCoverageV1::Complete,
        items: vec![],
        pull_request: Some(GitHubPullRequestSnapshotV1 {
            title: "stack layer fixture".to_owned(),
            state: GitHubPullRequestStateV1::Open,
            draft: false,
            additions: 1,
            deletions: 1,
            changed_files: 1,
        }),
        fetched_at: UtcMicros(1),
    };
    GitHubStackLayerSnapshotV1 {
        provider_position,
        pull_request: PullRequestSnapshotAnchorRefV1::from_ingress(&result, id(source_anchor))
            .expect("pull request anchor is canonical"),
        base_ref_id: id(base_ref),
        head_ref_id: id(head_ref),
        protection_digest: digest('a'),
        ci_digest: digest('b'),
        merge_queue_digest: digest('c'),
    }
}

/// `main -> lower -> upper`: the strictly linear two-layer stack that an
/// enabled provider capability is allowed to publish.
fn linear_github_stack_layers() -> Vec<GitHubStackLayerSnapshotV1> {
    vec![
        github_stack_layer(
            0,
            "repository.fixture",
            "pr.41",
            ("refs/heads/main", "refs/heads/lower"),
            ("commit.main", "commit.lower"),
            "anchor.pr.41",
        ),
        github_stack_layer(
            1,
            "repository.fixture",
            "pr.42",
            ("refs/heads/lower", "refs/heads/upper"),
            ("commit.lower", "commit.upper"),
            "anchor.pr.42",
        ),
    ]
}

fn github_stack_snapshot(
    capability: GitHubStackCapabilitySnapshotV1,
    layers: Vec<GitHubStackLayerSnapshotV1>,
) -> Result<GitHubStackSnapshotV1, tracedecay_domain::DomainError> {
    GitHubStackSnapshotV1::new(
        capability,
        id::<PrivacyDomainBoundLocatorDigest>(digest('d').as_str()),
        id("projection.github-stack.1"),
        id("refs/heads/main"),
        id("commit.main"),
        layers,
        id("anchor.github-stack.1"),
    )
}

#[test]
fn github_stack_targets_bind_exact_capability_generation_and_linear_snapshot_content() {
    let capability = github_stack_capability(GitHubStackCapabilityStateV1::Enabled);
    assert_eq!(
        capability.generation(),
        GitTopologyGenerationRefV1::GitHubStackCapability {
            generation_id: id("projection.github-stack-capability.1"),
            source_anchor_id: id("anchor.github-stack-capability.1"),
            content_digest: capability.content_digest.clone(),
        }
    );

    let snapshot = github_stack_snapshot(capability, linear_github_stack_layers()).unwrap();
    let target = GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone());
    let encoded = serde_json::to_value(&target).unwrap();
    assert_eq!(encoded["kind"], "github_stack_snapshot");
    assert_eq!(
        serde_json::from_value::<GitTopologyAnchorTargetV1>(encoded).unwrap(),
        target
    );
    assert_eq!(
        target.generation(),
        GitTopologyGenerationRefV1::GitHubStackSnapshot {
            generation_id: id("projection.github-stack.1"),
            source_anchor_id: id("anchor.github-stack.1"),
            content_digest: snapshot.content_digest.clone(),
            final_target_commit_id: id("commit.main"),
        }
    );
    assert_eq!(
        target
            .ordered_sources()
            .iter()
            .map(|source| source.role)
            .collect::<Vec<_>>(),
        vec![
            GitTopologySourceRoleV1::GitHubStackCapability,
            GitTopologySourceRoleV1::GitHubStackSnapshot,
            GitTopologySourceRoleV1::PullRequestObservation,
            GitTopologySourceRoleV1::PullRequestObservation,
        ],
        "every stack layer keeps its own ordered pull-request observation"
    );

    let owner = ObservationScopeV1::Project {
        project_id: id("project.fixture"),
    };
    let anchored = derive_git_topology_anchor_id(&owner, &target).unwrap();
    let mut changed = snapshot.clone();
    changed.generation_id = id("projection.github-stack.2");
    changed.content_digest = canonical_sha256(&(
        "tracedecay.github-stack.snapshot.v1",
        &changed.capability,
        &changed.provider_stack_id_digest,
        &changed.generation_id,
        &changed.final_target_ref_id,
        &changed.final_target_commit_id,
        &changed.layers,
        &changed.source_anchor_id,
    ))
    .unwrap();
    changed.validate().unwrap();
    assert_ne!(
        anchored,
        derive_git_topology_anchor_id(
            &owner,
            &GitTopologyAnchorTargetV1::GitHubStackSnapshot(changed),
        )
        .unwrap(),
        "a later provider observation is a new target, never a retarget"
    );

    let capability_target = GitTopologyAnchorTargetV1::GitHubStackCapability(
        github_stack_capability(GitHubStackCapabilityStateV1::Enabled),
    );
    let snapshot_record = record(target);
    let capability_record = record(capability_target);
    assert!(
        snapshot_record
            .anchor_id()
            .as_str()
            .starts_with("retrieval.v3.")
    );
    assert_ne!(
        snapshot_record.anchor_id(),
        capability_record.anchor_id(),
        "the capability observation and the stack snapshot are separate targets"
    );

    // What the store persists is this canonical record encoding, so it is the
    // exact place to prove the anchor stayed payload-free.
    let persisted = serde_json::to_string(&snapshot_record).unwrap();
    for payload in [
        "bodyText",
        "body_digest",
        "annotation",
        "patch",
        "diff_hunk",
        "cursor",
        "task_id",
    ] {
        assert!(
            !persisted.contains(payload),
            "a GitHub stack anchor never copies {payload} into the retained record"
        );
    }

    let mut tampered = snapshot;
    tampered.layers[1].pull_request.head_commit_id = id("commit.tampered");
    assert!(tampered.validate().is_err());
}

#[test]
fn github_stack_snapshot_rejects_non_enabled_capability_and_broken_topology() {
    for state in [
        GitHubStackCapabilityStateV1::Unavailable,
        GitHubStackCapabilityStateV1::PrivatePreviewDisabled,
        GitHubStackCapabilityStateV1::Degraded,
    ] {
        assert!(
            github_stack_snapshot(github_stack_capability(state), linear_github_stack_layers())
                .is_err(),
            "only an enabled capability may publish a stack snapshot: {state:?}"
        );
    }

    let enabled = || github_stack_capability(GitHubStackCapabilityStateV1::Enabled);
    assert!(
        github_stack_snapshot(enabled(), Vec::new()).is_err(),
        "an enabled capability still needs at least one observed layer"
    );

    let mut detached_base = linear_github_stack_layers();
    detached_base[1] = github_stack_layer(
        1,
        "repository.fixture",
        "pr.42",
        ("refs/heads/other", "refs/heads/upper"),
        ("commit.other", "commit.upper"),
        "anchor.pr.42",
    );
    assert!(
        github_stack_snapshot(enabled(), detached_base).is_err(),
        "a layer whose base leaves the stack breaks strict linearity"
    );

    let mut swapped_positions = linear_github_stack_layers();
    swapped_positions[0].provider_position = 1;
    swapped_positions[1].provider_position = 0;
    assert!(
        github_stack_snapshot(enabled(), swapped_positions).is_err(),
        "provider position must equal the observed stack ordinal"
    );

    let mut foreign_repository = linear_github_stack_layers();
    foreign_repository[1] = github_stack_layer(
        1,
        "repository.other",
        "pr.42",
        ("refs/heads/lower", "refs/heads/upper"),
        ("commit.lower", "commit.upper"),
        "anchor.pr.42",
    );
    assert!(
        github_stack_snapshot(enabled(), foreign_repository).is_err(),
        "an enabled stack stays inside one repository"
    );

    let mut foreign_final_target = linear_github_stack_layers();
    foreign_final_target[0].base_ref_id = id("refs/heads/release");
    assert!(
        github_stack_snapshot(enabled(), foreign_final_target).is_err(),
        "the lowest layer must sit on the declared final target"
    );
}
