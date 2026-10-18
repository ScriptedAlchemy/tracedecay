use std::collections::BTreeSet;

use tracedecay_domain::{
    ActorId, ComponentVersion, GitBlobExpectationV1, GitCoverageV1, GitFileModeV1, GitHeadStateV1,
    GitIndexEntryExpectationV1, GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1,
    HunkDirectionV1, HunkRefV1, ManifestDigest, ProjectId, RefId, RepositoryId,
    RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::transactions::scope_reference_matches_snapshot;
use super::{
    GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
};
use crate::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, IdempotencyKey, OperationBudgetUsage, OperationReceipt, PolicyDecisionRef,
    RequestContext, RequestId, ResolvedScope,
};

use tracedecay_domain::test_fixtures::id;

use tracedecay_domain::test_fixtures::digest;

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

fn hunk(preview_id: &GitIndexPreviewId, snapshot_digest: ManifestDigest) -> HunkRefV1 {
    HunkRefV1 {
        repository: id("repository.fixture"),
        worktree: id("worktree.fixture"),
        direction: HunkDirectionV1::WorkingTreeToIndex,
        path: "src/lib.rs".to_owned(),
        original_path: None,
        expected_base_blob: GitBlobExpectationV1::Present(oid('c')),
        expected_index_entry: GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::Present(oid('c')),
            mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).expect("regular mode")),
            unmerged_stage: None,
        },
        expected_worktree_blob: Some(GitBlobExpectationV1::Present(oid('e'))),
        expected_worktree_mode: Some(
            GitFileModeV1::new(GitFileModeV1::REGULAR).expect("regular mode"),
        ),
        hunk_header: "@@ -1,1 +1,1 @@".to_owned(),
        context_digest: digest('f'),
        patch_digest: digest('0'),
        selected_line_bitmap: vec![1],
        attributes_digest: None,
        preview_id: preview_id.as_str().to_owned(),
        schema_version: "hunkref.v1".to_owned(),
        snapshot_digest,
    }
}

fn request_for_repository(repository: &str) -> GitIndexPreviewRequestV1 {
    let capability_id = CapabilityId::new("capability.git.stage-hunks").expect("capability");
    let use_case_id = UseCaseId::new("use-case.git.stage-hunks").expect("use case");
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
    let preview_id = GitIndexPreviewId::new("preview.fixture").expect("preview id");
    let repository_snapshot = snapshot(repository);
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&repository_snapshot).expect("digest");
    GitIndexPreviewRequestV1 {
        context,
        authority,
        binding: GitIndexOperationBindingV1 {
            capability_id,
            use_case_id,
            operation: GitIndexTransactionOperationV1::StageHunks,
        },
        preview_id: preview_id.clone(),
        repository_snapshot,
        selected_hunks: vec![hunk(&preview_id, snapshot_digest)],
        observed_at: UtcMicros(10),
    }
}

fn request() -> GitIndexPreviewRequestV1 {
    request_for_repository("repository.fixture")
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
fn preview_validation_rejects_unrequested_hunks() {
    let request = request();
    request.validate().expect("request");
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&request.repository_snapshot)
            .expect("snapshot digest");
    let mut extra = request.selected_hunks[0].clone();
    extra.path = "src/other.rs".to_owned();
    let preview = GitIndexPreviewV1::new(
        request.preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        request.repository_snapshot.clone(),
        snapshot_digest,
        vec![request.selected_hunks[0].clone(), extra],
        request.repository_snapshot.index.tree_id.clone(),
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
            field: "git index preview selected hunk binding"
        })
    ));
}

#[test]
fn operation_binding_must_match_the_native_operation() {
    let mut wrong_operation = request();
    wrong_operation.binding.operation = GitIndexTransactionOperationV1::UnstageHunks;
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
    let preview_request = request();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&preview_request.repository_snapshot)
            .expect("snapshot digest");
    let preview = GitIndexPreviewV1::new(
        preview_request.preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        preview_request.repository_snapshot.clone(),
        snapshot_digest,
        preview_request.selected_hunks.clone(),
        preview_request.repository_snapshot.index.tree_id.clone(),
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
    wrong_operation.binding.operation = GitIndexTransactionOperationV1::UnstageHunks;
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

    let wrong_scope_source = request_for_repository("repository.other");
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
    let preview_request = request();
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&preview_request.repository_snapshot)
            .expect("snapshot digest");
    let preview = GitIndexPreviewV1::new(
        preview_request.preview_id.clone(),
        GitIndexTransactionOperationV1::StageHunks,
        preview_request.repository_snapshot.clone(),
        snapshot_digest,
        preview_request.selected_hunks.clone(),
        preview_request.repository_snapshot.index.tree_id.clone(),
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
