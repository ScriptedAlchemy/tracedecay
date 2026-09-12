use crate::runtime_ports::compose_application_catalog_snapshot;
use crate::test_support::git::GIT_FIXTURE_CONFIG;
use tracedecay_application::git_intelligence::NativeGitIntelligence;
use tracedecay_code_index_runtime::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use tracedecay_contracts::git::GitIndexTransactionPortError;
use tracedecay_contracts::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, GitIndexOperationBindingV1, GitIndexPreviewRequestV1, GitIndexTransactionPort,
    IdempotencyKey, OperationTermination, PolicyDecisionRef, RequestContext, RequestId,
};
use tracedecay_daemon_service::{GRANT_HORIZON, daemon_owned_project_source_access_at};
use tracedecay_domain::git::{
    GitDiffScopeV1, GitIndexPreviewDispositionV1, GitIndexPreviewV1, GitIndexReceiptOutcomeV1,
    GitIndexTransactionOperationV1, GitIndexUnsupportedStateV1,
    MAX_GIT_INDEX_PREVIEW_INPUT_LIFETIME_MICROS,
};
use tracedecay_domain::{
    ComponentVersion, GitCommitIdentityV1, GitIndexCommitIntentV1, GitIndexPreviewId,
    GitIndexPreviewInputV1, GitIndexSigningPolicyV1, canonical_sha256,
};
use tracedecay_domain::{ProjectId, UtcMicros};

fn unavailable_catalog() -> Result<
    tracedecay_tool_catalog::CatalogSnapshotV1,
    tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1,
> {
    Err(
        tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1::new(
            "catalog unavailable for this independently constructed owner",
        ),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_owner_uses_explicit_canonical_catalog_and_rechecks_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = directory.path().join("project");
    let profile_root = directory.path().join("profile");
    std::fs::create_dir_all(&project_root).unwrap();
    git(&project_root, &["init", "-b", "main"]);
    std::fs::write(project_root.join("file.txt"), "initial\n").unwrap();
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-m", "fixture"]);
    let project_id = ProjectId::new("project.git-catalog").unwrap();
    let fixture = crate::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let graph = fixture
        .initialize_project_graph_for_test(
            &project_root,
            crate::project::TraceDecayOpenOptions {
                profile_root: Some(profile_root),
                global_db_path: None,
            },
        )
        .await
        .unwrap();
    let configuration = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .unwrap();
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&project_root, &project_id)
            .unwrap();
    let access = daemon_owned_project_source_access_at(
        &scope,
        &project_root,
        &configuration,
        tracedecay_contracts::now_micros(),
    )
    .unwrap();
    let database = graph
        .store_runtime_registry()
        .mount_registered_project_sessions(project_id.clone())
        .await
        .unwrap();
    let registry =
        DaemonGitIndexTransactionServiceRegistry::new(compose_application_catalog_snapshot);
    registry
        .ensure(
            database.clone(),
            project_root.clone(),
            project_id.clone(),
            tracedecay_contracts::now_micros(),
        )
        .await
        .unwrap();
    registry
        .install_authority(
            &project_root,
            access.clone(),
            database.clone(),
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
    let owner = registry
        .for_repository_root(&project_root)
        .await
        .unwrap()
        .unwrap();
    let operation = GitIndexTransactionOperationV1::StageHunks;
    let initial = owner.current_authority(operation).unwrap();
    assert_eq!(
        initial.catalog_digest.as_str(),
        compose_application_catalog_snapshot()
            .unwrap()
            .digest()
            .to_string()
    );
    assert_eq!(initial.scope, scope);

    // A separate owner can refuse its own provider without replacing the
    // canonical dependency already retained by the first owner.
    let independent = DaemonGitIndexTransactionServiceRegistry::new(unavailable_catalog);
    independent
        .ensure(
            database.clone(),
            project_root.clone(),
            project_id,
            tracedecay_contracts::now_micros(),
        )
        .await
        .unwrap();
    independent
        .install_authority(
            &project_root,
            access.clone(),
            database.clone(),
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
    let unavailable = independent
        .for_repository_root(&project_root)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        unavailable.current_authority(operation),
        Err(GitIndexTransactionPortError::DaemonUnavailable)
    ));
    assert_eq!(
        owner.current_authority(operation).unwrap().catalog_digest,
        initial.catalog_digest
    );

    std::fs::write(project_root.join("file.txt"), "commit unavailable\n").unwrap();
    git(&project_root, &["add", "."]);
    let unavailable_commit = transaction_preview(
        &owner,
        "commit-unavailable",
        GitIndexTransactionOperationV1::CommitIndex,
    );
    let before = git(&project_root, &["rev-parse", "HEAD"]);
    let index_before = std::fs::read(project_root.join(".git/index")).unwrap();
    let unavailable_result = owner.service.apply(&unavailable_commit).unwrap();
    assert_eq!(
        unavailable_result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(git(&project_root, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        std::fs::read(project_root.join(".git/index")).unwrap(),
        index_before
    );

    std::fs::write(project_root.join("file.txt"), "accepted\n").unwrap();
    let index_tree_before = git(&project_root, &["write-tree"]);
    let accepted = transaction_preview(&owner, "accepted", operation);
    let accepted_result = owner.service.apply(&accepted).unwrap();
    assert_eq!(
        accepted_result.receipt.outcome,
        GitIndexReceiptOutcomeV1::Committed
    );
    assert_ne!(git(&project_root, &["write-tree"]), index_tree_before);
    assert_eq!(git(&project_root, &["show", ":file.txt"]), b"accepted\n");
    assert_eq!(git(&project_root, &["rev-parse", "HEAD"]), before);

    std::fs::write(project_root.join("file.txt"), "previewed\n").unwrap();
    let stale = transaction_preview(&owner, "stale", operation);
    std::fs::write(project_root.join("file.txt"), "changed\n").unwrap();
    let head_before_stale = git(&project_root, &["rev-parse", "HEAD"]);
    let index_before_stale = std::fs::read(project_root.join(".git/index")).unwrap();
    // Repository revalidation happens after durable admission, so refusal
    // must return a terminal no-change receipt.
    let stale_result = owner.service.apply(&stale).unwrap();
    stale_result.validate_for(&stale).unwrap();
    assert_eq!(
        stale_result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(
        stale_result.execution.termination,
        OperationTermination::Failed
    );
    assert_eq!(
        git(&project_root, &["rev-parse", "HEAD"]),
        head_before_stale
    );
    assert_eq!(
        std::fs::read(project_root.join(".git/index")).unwrap(),
        index_before_stale
    );
    assert_eq!(
        std::fs::read(project_root.join("file.txt")).unwrap(),
        b"changed\n"
    );
    let denied = transaction_preview(&owner, "revoked", operation);
    let before_denial = git(&project_root, &["rev-parse", "HEAD"]);
    let index_before_denial = std::fs::read(project_root.join(".git/index")).unwrap();
    let mut revoked = access.clone();
    revoked.effective_capabilities.clear();
    registry
        .install_authority(
            &project_root,
            revoked,
            database.clone(),
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
    assert!(matches!(
        owner.current_authority(operation),
        Err(GitIndexTransactionPortError::PolicyDenied)
    ));
    let denied_result = owner.service.apply(&denied).unwrap();
    denied_result.validate_for(&denied).unwrap();
    assert_eq!(
        denied_result.receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(
        denied_result.execution.termination,
        OperationTermination::Failed
    );
    assert_eq!(git(&project_root, &["rev-parse", "HEAD"]), before_denial);
    assert_eq!(
        std::fs::read(project_root.join(".git/index")).unwrap(),
        index_before_denial
    );
    let observed_at = tracedecay_contracts::now_micros();
    let issued_at =
        UtcMicros(observed_at.0 - i64::try_from(GRANT_HORIZON.as_micros()).unwrap() - 1);
    let expired =
        daemon_owned_project_source_access_at(&scope, &project_root, &configuration, issued_at)
            .unwrap();
    assert!(issued_at < expired.grant_expires_at);
    assert!(expired.grant_expires_at < observed_at);
    registry
        .install_authority(
            &project_root,
            expired,
            database,
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
    assert!(matches!(
        owner.current_authority(operation),
        Err(GitIndexTransactionPortError::PolicyDenied)
    ));
}

fn git(root: &std::path::Path, arguments: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .args(GIT_FIXTURE_CONFIG)
        .args(arguments)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "git failed: {output:?}");
    output.stdout
}

fn transaction_preview(
    owner: &tracedecay_code_index_runtime::git_transactions::DaemonGitInvocationOwner,
    suffix: &str,
    operation: GitIndexTransactionOperationV1,
) -> tracedecay_contracts::GitIndexApplyRequestV1 {
    let current = owner.current_authority(operation).unwrap();
    let binding = GitIndexOperationBindingV1::for_operation(operation).unwrap();
    let snapshot = tracedecay_code_index_runtime::git_transactions::capture_exact_snapshot(
        &owner.repository_root,
        current.scope.project_id.clone(),
        current.scope.repository_id.clone(),
        current.scope.worktree_id.clone(),
        current.evaluated_at,
    )
    .unwrap();
    let identity = GitCommitIdentityV1 {
        name: "Fixture".into(),
        email: "fixture@example.com".into(),
        at: current.evaluated_at,
    };
    let intent = (operation == GitIndexTransactionOperationV1::CommitIndex).then(|| {
        GitIndexCommitIntentV1::new(
            format!("catalog {suffix}"),
            identity.clone(),
            identity,
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .unwrap()
    });
    let preview_id = GitIndexPreviewId::new(format!("preview.catalog.{suffix}")).unwrap();
    let hunks = if operation == GitIndexTransactionOperationV1::StageHunks {
        let hunks = NativeGitIntelligence::new(
            owner.repository_root.clone(),
            current.scope.repository_id.clone(),
            current.scope.worktree_id.clone(),
        )
        .hunk_refs(
            &GitDiffScopeV1::WorkingTree,
            preview_id.as_str(),
            &GitIndexPreviewV1::repository_snapshot_digest(&snapshot).unwrap(),
        )
        .unwrap();
        assert!(!hunks.is_empty());
        hunks
    } else {
        Vec::new()
    };
    let expires_at =
        UtcMicros(current.evaluated_at.0 + MAX_GIT_INDEX_PREVIEW_INPUT_LIFETIME_MICROS);
    let input = if let Some(intent) = &intent {
        GitIndexPreviewInputV1::new_commit(
            preview_id.clone(),
            snapshot.clone(),
            intent.clone(),
            current.evaluated_at,
            expires_at,
        )
    } else {
        GitIndexPreviewInputV1::new_hunk_selection(
            preview_id.clone(),
            operation,
            snapshot.clone(),
            hunks.clone(),
            current.evaluated_at,
            expires_at,
        )
    }
    .unwrap();
    owner.service.save_preview_input(input).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.catalog.{suffix}")).unwrap(),
        current.policy_revision,
        canonical_sha256(&current.policy_digest).unwrap(),
        current.requester.clone(),
        current.evaluated_at,
        current.grant_expires_at,
        current.scope.clone(),
        [binding.capability_id.clone()].into_iter().collect(),
        [binding.use_case_id.clone()].into_iter().collect(),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let context = RequestContext::new(
        current.requester.clone(),
        current.scope.clone(),
        grant,
        RequestId::new(format!("request.catalog.{suffix}")).unwrap(),
        Deadline::new(current.grant_expires_at).unwrap(),
        CancellationContext::active(format!("cancel.catalog.{suffix}")).unwrap(),
    )
    .unwrap();
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.catalog.fixture",
            current.policy_revision,
            current.policy_digest.clone(),
            ComponentVersion::new("policy.catalog.fixture").unwrap(),
        )
        .unwrap(),
        current.evaluated_at,
    )
    .unwrap();
    let preview = owner
        .service
        .preview(&GitIndexPreviewRequestV1 {
            context: context.clone(),
            authority: authority.clone(),
            binding: binding.clone(),
            preview_id,
            repository_snapshot: snapshot,
            selected_hunks: hunks,
            commit_intent: intent,
            observed_at: current.evaluated_at,
        })
        .unwrap()
        .preview;
    assert_eq!(
        preview.disposition,
        if operation == GitIndexTransactionOperationV1::CommitIndex {
            GitIndexPreviewDispositionV1::Unsupported(
                GitIndexUnsupportedStateV1::AtomicRefNamespaceUnavailable,
            )
        } else {
            GitIndexPreviewDispositionV1::Applicable
        },
    );
    tracedecay_contracts::GitIndexApplyRequestV1 {
        context,
        authority,
        binding,
        preview_id: preview.preview_id,
        preview_digest: preview.preview_digest,
        idempotency_key: IdempotencyKey::new(format!("apply.catalog.{suffix}")).unwrap(),
        proof: tracedecay_contracts::GitIndexEffectProofV1 {
            policy_digest: current.policy_digest,
            configuration_digest: current.configuration_digest,
            catalog_digest: current.catalog_digest,
            privacy_digest: current.privacy_digest,
            external_proof: None,
        },
        observed_at: tracedecay_contracts::now_micros(),
    }
}
