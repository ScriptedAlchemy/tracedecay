//! Native-Git journey proof for the mounted Plan 36 daemon authority.
//!
//! The test drives the retained daemon owner, its canonical SQLite-backed
//! store actor, the exact-pair resolver, and the real `gix` adapter against
//! temporary repositories. It deliberately does not substitute a mock
//! transaction port or a test-only Git implementation.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use super::registry::DaemonNativeIntegrationServiceRegistry;
use tracedecay_application::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, NativeIntegrationApplyRequestV1, NativeIntegrationEvidenceRevisionsV1,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionRequestV1, RequestContext, RequestId, ResolvedScope,
    native_integration_surface_operation,
};
use tracedecay_domain::{
    ActorId, CapabilityId, ManifestDigest, MechanicalIntegrationModeV1,
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationPreviewId,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationTransactionId, ProjectId, RefId,
    RepositoryId, UtcMicros, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
    canonical_sha256,
};

const OBSERVED_AT: UtcMicros = UtcMicros(100);
const EXPIRES_AT: UtcMicros = UtcMicros(10_000);

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn write_and_commit(root: &Path, path: &str, contents: &str, message: &str) {
    std::fs::write(root.join(path), contents).expect("write fixture content");
    git(root, &["add", path]);
    git(root, &["commit", "-m", message]);
}

fn initialized_repository(root: &Path) {
    for arguments in [
        ["init", "--initial-branch=main"].as_slice(),
        ["config", "user.email", "fixture@example.com"].as_slice(),
        ["config", "user.name", "Fixture"].as_slice(),
    ] {
        git(root, arguments);
    }
    write_and_commit(root, "seed.txt", "seed\n", "seed");
}

fn prepare_pair(root: &Path, mode: MechanicalIntegrationModeV1) {
    initialized_repository(root);
    git(root, &["checkout", "-b", "destination"]);
    if mode != MechanicalIntegrationModeV1::FastForward {
        write_and_commit(root, "destination.txt", "destination\n", "destination");
    }
    git(root, &["checkout", "main"]);
    git(root, &["checkout", "-b", "source"]);
    write_and_commit(root, "source-1.txt", "source one\n", "source one");
    if mode == MechanicalIntegrationModeV1::CherryPickExactCommits {
        write_and_commit(root, "source-2.txt", "source two\n", "source two");
    }
    // Neither selected branch is checked out, so the production adapter can
    // prove that this journey does not materialize a selected worktree.
    git(root, &["checkout", "main"]);
}

fn exact_pair_scopes() -> (ResolvedScope, ResolvedScope) {
    let project = ProjectId::new("project.native.journey").expect("project id");
    let repository = RepositoryId::new("repository.native.journey").expect("repository id");
    let source = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        WorktreeId::new("worktree.native.source").expect("source worktree id"),
        Some(RefId::new("refs/heads/source").expect("source ref")),
    )
    .expect("source scope");
    let destination = ResolvedScope::new(
        project,
        repository,
        WorktreeId::new("worktree.native.destination").expect("destination worktree id"),
        Some(RefId::new("refs/heads/destination").expect("destination ref")),
    )
    .expect("destination scope");
    (source, destination)
}

fn operation_authority(
    operation: &str,
) -> (
    tracedecay_tool_catalog::CapabilityId,
    tracedecay_tool_catalog::UseCaseId,
) {
    let operation = native_integration_surface_operation(operation)
        .expect("canonical operation")
        .expect("declared operation");
    (
        operation.capability_id().clone(),
        operation.use_case_id().clone(),
    )
}

fn context(destination: ResolvedScope, request_id: &str) -> RequestContext {
    let (preflight_capability, preflight_use_case) =
        operation_authority(tracedecay_application::NATIVE_INTEGRATION_PREFLIGHT_OPERATION);
    let (apply_capability, apply_use_case) =
        operation_authority(tracedecay_application::NATIVE_INTEGRATION_APPLY_OPERATION);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.native.journey").expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.native.issuer").expect("issuer"),
        UtcMicros(1),
        EXPIRES_AT,
        destination.clone(),
        BTreeSet::from([preflight_capability, apply_capability]),
        BTreeSet::from([preflight_use_case, apply_use_case]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.native.requester").expect("requester"),
        destination,
        grant,
        RequestId::new(request_id).expect("request id"),
        Deadline::new(EXPIRES_AT).expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
    .expect("request context")
}

fn preflight_request(
    mode: MechanicalIntegrationModeV1,
    request_id: &str,
) -> NativeIntegrationPreflightRequestV1 {
    let (source, destination) = exact_pair_scopes();
    let context = context(destination.clone(), request_id);
    NativeIntegrationPreflightRequestV1 {
        context,
        topology: NativeIntegrationStackResolutionRequestV1 {
            source,
            destination,
            authorized_scope_set_digest: digest('b'),
            inventory_snapshot_id: WorktreeInventorySnapshotId::new("inventory.native.journey")
                .expect("inventory snapshot"),
            inventory_epoch: WorktreeInventoryEpoch::new(1).expect("inventory epoch"),
            selection: NativeIntegrationSelectionBindingV1::IndependentBranch {
                proposal_digest: digest('c'),
            },
            grant_digest: digest('a'),
            policy_digest: digest('d'),
            observed_at: OBSERVED_AT,
        },
        evidence: NativeIntegrationEvidenceRevisionsV1 {
            graph_revision_digest: digest('e'),
            test_revision_digest: digest('f'),
            schema_revision_digest: digest('1'),
            migration_revision_digest: digest('2'),
        },
        preview_id: NativeIntegrationPreviewId::new(format!("preview.native.{request_id}"))
            .expect("preview id"),
        preferred_mode: Some(mode),
        preview_expires_at: EXPIRES_AT,
        observed_at: OBSERVED_AT,
    }
}

fn approval_for(
    context: &RequestContext,
    preview: &tracedecay_domain::NativeIntegrationPreviewV1,
    request_id: &str,
) -> NativeIntegrationApprovalV1 {
    let (capability, _) =
        operation_authority(tracedecay_application::NATIVE_INTEGRATION_APPLY_OPERATION);
    NativeIntegrationApprovalV1 {
        approval_id: NativeIntegrationApprovalId::new(format!("approval.native.{request_id}"))
            .expect("approval id"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        principal: context.actor().clone(),
        delegated_agent: None,
        capability: CapabilityId::new(capability.as_str().to_owned()).expect("domain capability"),
        grant_digest: context.grant().digest.clone(),
        issued_at: OBSERVED_AT,
        expires_at: preview.expires_at,
        approval_digest: canonical_sha256(&"pending native journey approval").expect("digest"),
    }
    .seal()
    .expect("approval")
}

async fn mount(
    database_path: std::path::PathBuf,
    repository_root: std::path::PathBuf,
) -> (
    DaemonNativeIntegrationServiceRegistry,
    super::registry::DaemonNativeIntegrationOwner,
) {
    let registry = DaemonNativeIntegrationServiceRegistry::default();
    let owner = registry
        .ensure_engine_test(
            database_path,
            repository_root,
            ProjectId::new("project.native.journey").expect("project id"),
            RepositoryId::new("repository.native.journey").expect("repository id"),
            digest('d'),
            OBSERVED_AT,
        )
        .await
        .expect("mount native integration owner");
    (registry, owner)
}

async fn preflight(
    owner: super::registry::DaemonNativeIntegrationOwner,
    request: NativeIntegrationPreflightRequestV1,
) -> tracedecay_domain::NativeIntegrationPreviewV1 {
    let signal = CancellationSignal::active("cancel.native.journey.preflight").expect("signal");
    let outcome = tokio::task::spawn_blocking(move || owner.service().preflight(request, &signal))
        .await
        .expect("preflight join")
        .expect("preflight result");
    let NativeIntegrationPreflightOutcomeV1::Preview(preview) = outcome else {
        panic!("fixture must produce an eligible native preview: {outcome:?}");
    };
    preview.as_ref().clone()
}

async fn stack_snapshot(
    owner: super::registry::DaemonNativeIntegrationOwner,
    request: NativeIntegrationStackResolutionRequestV1,
) -> tracedecay_domain::NativeIntegrationSelectionV1 {
    let signal = CancellationSignal::active("cancel.native.journey.snapshot").expect("signal");
    let outcome = tokio::task::spawn_blocking(move || owner.stack_snapshot(request, &signal))
        .await
        .expect("stack snapshot join")
        .expect("stack snapshot result");
    let NativeIntegrationStackResolutionOutcomeV1::Complete(selection) = outcome else {
        panic!("fixture must freeze the exact independent pair: {outcome:?}");
    };
    selection.as_ref().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_pair_applies_supported_modes_and_survives_daemon_restart() {
    for (index, mode) in [
        MechanicalIntegrationModeV1::FastForward,
        MechanicalIntegrationModeV1::TwoParentMerge,
        MechanicalIntegrationModeV1::CherryPickExactCommits,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let repository_root = directory.path().join("repo");
        std::fs::create_dir_all(&repository_root).expect("repository root");
        prepare_pair(&repository_root, mode);
        let database_path = directory.path().join("project-sessions.db");
        drop(std::fs::File::create(&database_path).expect("database file"));

        let (registry, owner) = mount(database_path.clone(), repository_root.clone()).await;
        let request_id = format!("request.native.journey.{index}");
        let request = preflight_request(mode, &request_id);
        let context = request.context.clone();
        let frozen_selection = stack_snapshot(owner.clone(), request.topology.clone()).await;
        let preview = preflight(owner.clone(), request).await;
        assert_eq!(preview.selection, frozen_selection);
        assert_eq!(
            preview.disposition,
            tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                mode
            )
        );
        assert!(
            preview
                .repository_snapshot
                .destination_worktree_id
                .is_none()
        );
        let candidate_tree = preview
            .candidate_tree
            .clone()
            .expect("eligible candidate tree");
        let source_tip = preview.repository_snapshot.source_tip.clone();
        let destination_tip = preview.repository_snapshot.destination_tip.clone();
        let ordered_commit_count = preview.ordered_commits.len();
        let approval = approval_for(&context, &preview, &request_id);
        owner
            .store()
            .save_approval(approval.clone())
            .expect("durably issue approval");
        let transaction_id =
            NativeIntegrationTransactionId::new(format!("transaction.native.journey.{index}"))
                .expect("transaction id");
        let status_transaction_id = transaction_id.clone();
        let apply_owner = owner.clone();
        let apply_signal =
            CancellationSignal::active("cancel.native.journey.apply").expect("signal");
        let receipt = tokio::task::spawn_blocking(move || {
            apply_owner.service().apply(
                NativeIntegrationApplyRequestV1 {
                    context,
                    transaction_id: transaction_id.clone(),
                    preview,
                    approval,
                    observed_at: OBSERVED_AT,
                },
                &apply_signal,
            )
        })
        .await
        .expect("apply join")
        .expect("apply result");
        assert_eq!(
            receipt.status.terminal_outcome,
            Some(NativeIntegrationTerminalOutcomeV1::Committed)
        );
        assert_eq!(receipt.final_tree, candidate_tree);
        assert_eq!(
            git(&repository_root, &["rev-parse", "refs/heads/destination"]),
            receipt.final_ref_tip.as_str()
        );
        match mode {
            MechanicalIntegrationModeV1::FastForward => {
                assert_eq!(receipt.final_ref_tip, source_tip);
            }
            MechanicalIntegrationModeV1::TwoParentMerge => {
                let parent_line = git(
                    &repository_root,
                    &["rev-list", "--parents", "-n", "1", "refs/heads/destination"],
                );
                let parents = parent_line.split_whitespace().collect::<Vec<_>>();
                assert_eq!(parents.len(), 3, "merge result must have two parents");
                assert_eq!(parents[1], destination_tip.as_str());
                assert_eq!(parents[2], source_tip.as_str());
            }
            MechanicalIntegrationModeV1::CherryPickExactCommits => {
                let revision_range =
                    format!("{}..refs/heads/destination", destination_tip.as_str());
                let materialized_count = git(
                    &repository_root,
                    &["rev-list", "--count", revision_range.as_str()],
                )
                .parse::<usize>()
                .expect("cherry-pick result count");
                assert_eq!(materialized_count, ordered_commit_count);
                assert_ne!(receipt.final_ref_tip, source_tip);
            }
        }

        registry.shutdown().await.expect("shutdown owner registry");
        let (restarted_registry, restarted_owner) = mount(database_path, repository_root).await;
        let durable = tokio::task::spawn_blocking(move || {
            restarted_owner.service().status(
                tracedecay_application::NativeIntegrationStatusRequestV1 {
                    transaction_id: status_transaction_id,
                },
            )
        })
        .await
        .expect("status join")
        .expect("durable status")
        .expect("durable transaction status");
        assert_eq!(
            durable.terminal_outcome,
            Some(NativeIntegrationTerminalOutcomeV1::Committed)
        );
        restarted_registry
            .shutdown()
            .await
            .expect("shutdown restarted owner registry");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_destination_ref_drift_terminates_without_mutating_the_foreign_tip() {
    let directory = tempfile::tempdir().expect("temporary project directory");
    let repository_root = directory.path().join("repo");
    std::fs::create_dir_all(&repository_root).expect("repository root");
    prepare_pair(&repository_root, MechanicalIntegrationModeV1::FastForward);
    let database_path = directory.path().join("project-sessions.db");
    drop(std::fs::File::create(&database_path).expect("database file"));
    let (registry, owner) = mount(database_path, repository_root.clone()).await;
    let request = preflight_request(
        MechanicalIntegrationModeV1::FastForward,
        "request.native.journey.drift",
    );
    let context = request.context.clone();
    let preview = preflight(owner.clone(), request).await;
    let approval = approval_for(&context, &preview, "request.native.journey.drift");
    owner
        .store()
        .save_approval(approval.clone())
        .expect("durably issue approval");

    git(&repository_root, &["checkout", "-b", "foreign"]);
    write_and_commit(&repository_root, "foreign.txt", "foreign\n", "foreign");
    let foreign_tip = git(&repository_root, &["rev-parse", "HEAD"]);
    git(&repository_root, &["checkout", "main"]);
    git(
        &repository_root,
        &["update-ref", "refs/heads/destination", foreign_tip.as_str()],
    );

    let apply_owner = owner.clone();
    let signal = CancellationSignal::active("cancel.native.journey.drift").expect("signal");
    let receipt = tokio::task::spawn_blocking(move || {
        apply_owner.service().apply(
            NativeIntegrationApplyRequestV1 {
                context,
                transaction_id: NativeIntegrationTransactionId::new(
                    "transaction.native.journey.drift",
                )
                .expect("transaction id"),
                preview,
                approval,
                observed_at: OBSERVED_AT,
            },
            &signal,
        )
    })
    .await
    .expect("apply join")
    .expect("foreign drift must terminate without inspection quarantine");
    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::AbortedNoChange)
    );
    assert_eq!(receipt.final_ref_tip.as_str(), foreign_tip);
    assert_eq!(
        git(&repository_root, &["rev-parse", "refs/heads/destination"]),
        foreign_tip
    );
    // The known pre-commit drift did not poison the repository: a freshly
    // frozen selection can produce a new, mechanically eligible preview.
    let fresh_preview = preflight(
        owner,
        preflight_request(
            MechanicalIntegrationModeV1::TwoParentMerge,
            "request.native.journey.after-drift",
        ),
    )
    .await;
    assert_eq!(
        fresh_preview.disposition,
        tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
            MechanicalIntegrationModeV1::TwoParentMerge
        )
    );
    registry.shutdown().await.expect("shutdown owner registry");
}
