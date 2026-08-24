use std::collections::BTreeSet;

use tracedecay_domain::{
    ActorId, ComponentVersion, GitCommitIdentityV1, GitCoverageV1, GitHeadStateV1,
    GitIndexCommitIntentV1, GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexSigningPolicyV1, GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, ManifestDigest, ProjectId, RefId, RepositoryId, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1,
    RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, EffectClass, UseCaseId};

use super::transactions::scope_reference_matches_snapshot;
use super::{
    GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1, git_index_effect_class,
};
use crate::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, IdempotencyKey, OperationBudgetUsage, OperationReceipt, PolicyDecisionRef,
    RequestContext, RequestId, ResolvedScope,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture oid")
}

fn snapshot(repository: &str) -> RepositoryStateSnapshotV1 {
    RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>(repository),
        Some(id::<WorktreeId>("worktree.fixture")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
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
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("snapshot")
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('5'),
    )
    .expect("native snapshot")
}

fn commit_intent(message: &str) -> GitIndexCommitIntentV1 {
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: UtcMicros(1_000_000),
    };
    GitIndexCommitIntentV1::new(
        message.to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .expect("commit intent")
}

fn request_for_repository(
    intent: GitIndexCommitIntentV1,
    repository: &str,
) -> GitIndexPreviewRequestV1 {
    let capability_id = CapabilityId::new("capability.git.commit-index").expect("capability");
    let use_case_id = UseCaseId::new("use-case.git.commit-index").expect("use case");
    let scope = ResolvedScope::new(
        id("project.fixture"),
        id(repository),
        id("worktree.fixture"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .expect("scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.fixture").expect("grant id"),
        1,
        digest('6'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([capability_id.clone()]),
        BTreeSet::from([use_case_id.clone()]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    let context = RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new("request.fixture").expect("request id"),
        Deadline::new(UtcMicros(500)).expect("deadline"),
        CancellationContext::active("cancel.fixture").expect("cancellation"),
    )
    .expect("context");
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.fixture",
            1,
            digest('7'),
            ComponentVersion::new("policy.evaluator.v1").expect("policy version"),
        )
        .expect("policy"),
        UtcMicros(2),
    )
    .expect("authority");
    GitIndexPreviewRequestV1 {
        context,
        authority,
        binding: GitIndexOperationBindingV1 {
            capability_id,
            use_case_id,
            operation: GitIndexTransactionOperationV1::CommitIndex,
        },
        preview_id: GitIndexPreviewId::new("preview.fixture").expect("preview id"),
        repository_snapshot: snapshot(repository),
        selected_hunks: Vec::new(),
        commit_intent: Some(intent),
        observed_at: UtcMicros(10),
    }
}

fn request(intent: GitIndexCommitIntentV1) -> GitIndexPreviewRequestV1 {
    request_for_repository(intent, "repository.fixture")
}

fn apply_request(
    preview_request: &GitIndexPreviewRequestV1,
    preview: &GitIndexPreviewV1,
) -> GitIndexApplyRequestV1 {
    GitIndexApplyRequestV1 {
        context: preview_request.context.clone(),
        authority: preview_request.authority.clone(),
        binding: preview_request.binding.clone(),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        idempotency_key: IdempotencyKey::new("idempotency.fixture").expect("idempotency key"),
        proof: GitIndexEffectProofV1 {
            policy_digest: preview_request.authority.policy.digest.clone(),
            configuration_digest: digest('8'),
            catalog_digest: digest('9'),
            privacy_digest: digest('a'),
            external_proof: None,
        },
        observed_at: UtcMicros(15),
    }
}

#[test]
fn each_index_mutation_keeps_its_own_effect_class() {
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::StageHunks),
        EffectClass::GitIndexStage
    );
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::UnstageHunks),
        EffectClass::GitIndexUnstage
    );
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::CommitIndex),
        EffectClass::GitIndexCommit
    );
}

#[test]
fn preview_validation_rejects_a_different_commit_intent_than_requested() {
    let request = request(commit_intent("requested message\n"));
    request.validate().expect("request");
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&request.repository_snapshot)
            .expect("snapshot digest");
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        request.preview_id.clone(),
        GitIndexTransactionOperationV1::CommitIndex,
        request.repository_snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        request.repository_snapshot.index.tree_id.clone(),
        Some(&commit_intent("different message\n")),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview");
    let result = GitIndexPreviewPortResultV1 {
        preview,
        execution: OperationReceipt::completed(
            UtcMicros(10),
            UtcMicros(11),
            Deadline::new(UtcMicros(500)).expect("deadline"),
            OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: 1,
                elapsed_micros: 1,
            },
        )
        .expect("execution"),
    };

    assert!(matches!(
        result.validate_for(&request),
        Err(crate::ApplicationContractError::Inconsistent {
            field: "git index preview commit intent binding"
        })
    ));
}

#[test]
fn operation_binding_must_match_the_native_operation() {
    let mut wrong_operation = request(commit_intent("requested message\n"));
    wrong_operation.binding.operation = GitIndexTransactionOperationV1::StageHunks;
    assert!(matches!(
        wrong_operation.validate(),
        Err(crate::ApplicationContractError::Inconsistent {
            field: "git index transaction operation binding"
        })
    ));
}

#[test]
fn repository_reference_binding_is_exact_and_never_implicit() {
    let attached = snapshot("repository.fixture");
    let matching = RefId::new("refs/heads/main").expect("matching ref");
    let different = RefId::new("refs/heads/other").expect("different ref");

    assert!(scope_reference_matches_snapshot(Some(&matching), &attached));
    assert!(!scope_reference_matches_snapshot(None, &attached));
    assert!(!scope_reference_matches_snapshot(
        Some(&different),
        &attached
    ));
}

#[test]
fn apply_request_must_bind_the_exact_preview_before_native_mutation() {
    let preview_request = request(commit_intent("requested message\n"));
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&preview_request.repository_snapshot)
            .expect("snapshot digest");
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        preview_request.preview_id.clone(),
        GitIndexTransactionOperationV1::CommitIndex,
        preview_request.repository_snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        preview_request.repository_snapshot.index.tree_id.clone(),
        preview_request.commit_intent.as_ref(),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview");
    let request = apply_request(&preview_request, &preview);
    request
        .validate_for_preview(&preview)
        .expect("exact apply binding");

    let mut wrong_operation = request.clone();
    wrong_operation.binding.operation = GitIndexTransactionOperationV1::StageHunks;
    assert!(matches!(
        wrong_operation.validate_for_preview(&preview),
        Err(crate::ApplicationContractError::Inconsistent {
            field: "git index transaction operation binding"
        })
    ));

    let mut wrong_digest = request;
    wrong_digest.preview_digest = digest('f');
    assert!(matches!(
        wrong_digest.validate_for_preview(&preview),
        Err(crate::ApplicationContractError::Inconsistent {
            field: "git index apply preview binding"
        })
    ));

    let wrong_scope_source = request_for_repository(
        commit_intent("other repository message\n"),
        "repository.other",
    );
    let wrong_scope = apply_request(&wrong_scope_source, &preview);
    assert!(matches!(
        wrong_scope.validate_for_preview(&preview),
        Err(crate::ApplicationContractError::Inconsistent {
            field: "git index apply preview binding"
        })
    ));
}

#[test]
fn apply_idempotency_digest_excludes_volatile_revalidation_evidence() {
    let preview_request = request(commit_intent("requested message\n"));
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&preview_request.repository_snapshot)
            .expect("snapshot digest");
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        preview_request.preview_id.clone(),
        GitIndexTransactionOperationV1::CommitIndex,
        preview_request.repository_snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        preview_request.repository_snapshot.index.tree_id.clone(),
        preview_request.commit_intent.as_ref(),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(20),
    )
    .expect("preview");
    let request = apply_request(&preview_request, &preview);
    let expected = request.input_digest().expect("semantic apply digest");

    let mut revalidated = request.clone();
    revalidated.observed_at = UtcMicros(16);
    revalidated.authority.revalidated_at = UtcMicros(3);
    revalidated.proof.configuration_digest = digest('b');
    revalidated.proof.catalog_digest = digest('c');
    revalidated.proof.privacy_digest = digest('d');
    assert_eq!(
        revalidated.input_digest().expect("revalidated digest"),
        expected
    );

    revalidated.preview_digest = digest('e');
    assert_ne!(
        revalidated
            .input_digest()
            .expect("different preview digest"),
        expected
    );
}
