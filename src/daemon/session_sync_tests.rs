use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionSyncCommandV1, SessionSyncCompletionReceiptV1, SessionSyncCoverageV1,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionSyncSourceCoverageV1, SessionSyncStatsV1,
    SessionTranscriptImportV1,
};
use tracedecay_application::{
    AuthorizedRootAdmission, AuthorizedScopeSetAuthority, CancellationContext, CancellationSignal,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, IdempotencyKey, OperationTermination,
    RegisteredRootLocatorV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitBranchStackBindingV1, GitTopologyProjectionStore,
    GitWorktreeOccupancyV1, build_git_topology_manifest_checked, git_topology_idempotency_key,
    git_topology_namespace, git_topology_projection_identity,
};
use tracedecay_domain::{
    ActorId, BrainId, BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CommitId, ManifestDigest, ProjectId, RefId,
    RepositoryId, ScopeSetId, ScopeSetRevision, StackNodeId, UserProfileId, UtcMicros, WorktreeId,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_global_db::VerifiedGraphRuntimePortV1;
use tracedecay_graph_db::{GraphCancellation, GraphProjectorRevision};
use tracedecay_store::{FactReadControl, runtime::ScopeSetCasOutcomeV1};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::git_topology::GitTopologySyncFailure;
use super::work::{
    SessionSyncInterruption, coalesced_alias_local_interruption, git_history_frontier_from_meta,
    git_history_source_frontier, git_sync_with_topology_result, git_sync_work_result,
};
use super::{
    DaemonSessionSyncConfig, DaemonSessionSyncService, SessionSyncWorkResult,
    completed_profile_sweep_covers, completion_termination, decode_matching_journal, journal_key,
};

struct NeverCancelled;

impl GraphCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn cancel_after_first_git_commit_preserves_progress_and_cancelled_termination() {
    let result = git_sync_work_result(
        &ProjectId::new("project.cancel-after-commit").unwrap(),
        tracedecay_sessions::runtime::git_correlation::BoundedBackfillOutcome {
            stats: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                sessions_scanned: 1,
                spans_written: 2,
                commits_attributed: 3,
                ..tracedecay_sessions::runtime::git_correlation::BackfillStats::default()
            },
            committed: true,
            frontier: tracedecay_sessions::runtime::git_correlation::GitHistoryIndexFrontier {
                activity_timestamp: 1_723_456_789,
                source_rowid: 417,
            },
            remaining_sessions: 1,
            unresolved_failures: 0,
            interruption: Some(
                tracedecay_sessions::runtime::git_correlation::BoundedBackfillInterruption::Cancelled,
            ),
        },
        Some(SessionSyncInterruption::Cancelled),
    );
    let SessionSyncWorkResult::Finished {
        interruption,
        committed,
        stats,
        coverage,
        source_frontiers,
        failure_codes,
    } = result
    else {
        panic!("committed Git progress must produce durable terminal evidence");
    };

    assert!(committed);
    assert_eq!(stats.sessions_scanned, 1);
    assert_eq!(stats.spans_written, 2);
    assert_eq!(stats.commits_attributed, 3);
    assert_eq!(
        coverage,
        vec![SessionSyncSourceCoverageV1 {
            store_scope: "git".to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
        }]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&source_frontiers[0].committed_cursor_json)
            .unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_789,
            "source_rowid": 417,
        })
    );
    assert_eq!(
        failure_codes,
        vec!["git_sync_cancelled_after_commit".to_owned()]
    );
    assert_eq!(
        completion_termination(
            interruption.and_then(SessionSyncInterruption::termination),
            committed,
            &stats,
            false,
            failure_codes.is_empty(),
        ),
        OperationTermination::Cancelled
    );
}

#[test]
fn deadline_after_first_git_commit_preserves_progress_and_timed_out_termination() {
    let result = git_sync_work_result(
        &ProjectId::new("project.deadline-after-commit").unwrap(),
        tracedecay_sessions::runtime::git_correlation::BoundedBackfillOutcome {
            stats: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                sessions_scanned: 1,
                spans_written: 2,
                commits_attributed: 3,
                ..tracedecay_sessions::runtime::git_correlation::BackfillStats::default()
            },
            committed: true,
            frontier: tracedecay_sessions::runtime::git_correlation::GitHistoryIndexFrontier {
                activity_timestamp: 1_723_456_790,
                source_rowid: 418,
            },
            remaining_sessions: 0,
            unresolved_failures: 0,
            interruption: Some(
                tracedecay_sessions::runtime::git_correlation::BoundedBackfillInterruption::Cancelled,
            ),
        },
        Some(SessionSyncInterruption::TimedOut),
    );
    let SessionSyncWorkResult::Finished {
        interruption,
        committed,
        stats,
        coverage,
        source_frontiers,
        failure_codes,
    } = result
    else {
        panic!("committed Git progress must produce durable terminal evidence");
    };

    assert!(committed);
    assert_eq!(stats.sessions_scanned, 1);
    assert_eq!(stats.spans_written, 2);
    assert_eq!(stats.commits_attributed, 3);
    assert_eq!(
        coverage,
        vec![SessionSyncSourceCoverageV1 {
            store_scope: "git".to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
        }]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&source_frontiers[0].committed_cursor_json)
            .unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_790,
            "source_rowid": 418,
        })
    );
    assert_eq!(
        failure_codes,
        vec!["git_sync_timed_out_after_commit".to_owned()]
    );
    assert_eq!(
        completion_termination(
            interruption.and_then(SessionSyncInterruption::termination),
            committed,
            &stats,
            false,
            failure_codes.is_empty(),
        ),
        OperationTermination::TimedOut
    );
}

#[test]
fn committed_result_stays_terminal_when_cancel_or_deadline_arrives_late() {
    let stats = SessionSyncStatsV1 {
        sessions_imported: 1,
        messages_imported: 3,
        ..SessionSyncStatsV1::default()
    };

    assert_eq!(
        completion_termination(None, true, &stats, true, true),
        OperationTermination::Completed
    );
    assert_eq!(
        completion_termination(None, true, &stats, true, false),
        OperationTermination::Partial
    );
}

#[test]
fn uncommitted_failure_is_not_reported_as_success() {
    assert_eq!(
        completion_termination(None, false, &SessionSyncStatsV1::default(), true, false,),
        OperationTermination::Failed
    );
}

#[test]
fn partial_coverage_is_never_completed_without_failures() {
    assert_eq!(
        completion_termination(None, false, &SessionSyncStatsV1::default(), false, true,),
        OperationTermination::Failed
    );
    let stats = SessionSyncStatsV1 {
        messages_imported: 1,
        ..SessionSyncStatsV1::default()
    };
    assert_eq!(
        completion_termination(None, true, &stats, false, true),
        OperationTermination::Partial
    );
}

#[test]
fn declared_git_topology_failures_keep_their_typed_failure_code() {
    for (failure, expected) in [
        (
            GitTopologySyncFailure::Stale,
            "git_topology_declared_state_stale",
        ),
        (
            GitTopologySyncFailure::Denied,
            "git_topology_declared_authority_denied",
        ),
        (
            GitTopologySyncFailure::Unavailable,
            "git_topology_declared_authority_unavailable",
        ),
    ] {
        let result = git_sync_with_topology_result(
            SessionSyncWorkResult::Finished {
                interruption: None,
                committed: true,
                stats: SessionSyncStatsV1::default(),
                coverage: Vec::new(),
                source_frontiers: Vec::new(),
                failure_codes: Vec::new(),
            },
            Err(failure),
        );
        let SessionSyncWorkResult::Finished { failure_codes, .. } = result else {
            panic!("declared topology failure must preserve completed Git sync evidence");
        };
        assert_eq!(failure_codes, vec![expected.to_owned()]);
    }
}

fn native_topology_digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn run_native_topology_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("start Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn native_topology_context(
    scope: ResolvedScope,
    suffix: &str,
    capability: &CapabilityId,
    use_case: &UseCaseId,
) -> RequestContext {
    let grant = CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!("grant.session-sync.{suffix}"))
            .expect("grant"),
        1,
        native_topology_digest('c'),
        ActorId::new("actor.session-sync.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(100),
        scope.clone(),
        BTreeSet::from([capability.clone()]),
        BTreeSet::from([use_case.clone()]),
        DisclosureClass::Evidence,
    )
    .expect("capability grant");
    RequestContext::new(
        ActorId::new("actor.session-sync.requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.session-sync.{suffix}")).expect("request"),
        Deadline::new(UtcMicros(90)).expect("deadline"),
        CancellationContext::active(format!("cancel.session-sync.{suffix}")).expect("cancellation"),
    )
    .expect("request context")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_declared_topology_survives_registry_restart_and_session_sync_revalidation() {
    let temporary = tempfile::tempdir().expect("native topology fixture");
    let profile_root = temporary.path().join("profile");
    let repository_root = temporary.path().join("repository");
    std::fs::create_dir_all(&repository_root).expect("repository root");
    run_native_topology_git(&repository_root, &["init", "--quiet", "-b", "main"]);
    run_native_topology_git(
        &repository_root,
        &["config", "user.name", "TraceDecay Fixture"],
    );
    run_native_topology_git(
        &repository_root,
        &["config", "user.email", "fixture@tracedecay.invalid"],
    );
    std::fs::write(repository_root.join("base.txt"), "base\n").expect("base file");
    run_native_topology_git(&repository_root, &["add", "base.txt"]);
    run_native_topology_git(&repository_root, &["commit", "--quiet", "-m", "base"]);
    run_native_topology_git(&repository_root, &["branch", "feature"]);
    let linked_root = temporary.path().join("feature");
    run_native_topology_git(
        &repository_root,
        &[
            "worktree",
            "add",
            "--quiet",
            linked_root.to_str().expect("UTF-8 linked root"),
            "feature",
        ],
    );

    let project = ProjectId::new("project.session-sync.native-topology").expect("project");
    let repository =
        RepositoryId::new("repository.session-sync.native-topology").expect("repository");
    let main_worktree = WorktreeId::new("worktree.session-sync.main").expect("main worktree");
    let feature_worktree =
        WorktreeId::new("worktree.session-sync.feature").expect("feature worktree");
    for root in [&repository_root, &linked_root] {
        crate::storage::pin_fixture_repository_identity(root, project.as_str())
            .expect("project enrollment");
    }
    let roots = vec![
        repository_root
            .canonicalize()
            .expect("canonical repository"),
        linked_root.canonicalize().expect("canonical linked root"),
    ];
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 47, "native topology restart")
            .expect("daemon database scope");
    let first_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity.clone(),
        )
        .await
        .expect("first session registry");
    let first_project_database = first_registry
        .project_memory(project.clone(), roots.clone())
        .await
        .expect("first project graph database");
    let first_sessions = first_registry
        .project_sessions(project.clone(), roots.clone())
        .await
        .expect("first project sessions database");
    let first_runtime = first_project_database
        .memory_graph_runtime()
        .expect("first project graph runtime");
    assert!(
        first_sessions
            .bind_project_graph_runtime(first_runtime.clone())
            .is_ok(),
        "bind first graph runtime"
    );

    let main_ref = RefId::new("refs/heads/main").expect("main ref");
    let feature_ref = RefId::new("refs/heads/feature").expect("feature ref");
    let main_scope = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        main_worktree.clone(),
        Some(main_ref.clone()),
    )
    .expect("main scope");
    let feature_scope = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        feature_worktree.clone(),
        Some(feature_ref.clone()),
    )
    .expect("feature scope");
    let capability =
        CapabilityId::new("capability.session-sync.native-topology").expect("topology capability");
    let use_case =
        UseCaseId::new("use-case.session-sync.native-topology").expect("topology use case");
    let scope_set_id =
        ScopeSetId::new("scope-set.session-sync.native-topology").expect("scope set");
    let scope_set = AuthorizedScopeSetAuthority::authorize_registered(
        scope_set_id.clone(),
        ScopeSetRevision::new(1).expect("scope revision"),
        vec![
            AuthorizedRootAdmission::new(
                native_topology_context(main_scope.clone(), "main.1", &capability, &use_case),
                RegisteredRootLocatorV1::new(
                    project.clone(),
                    identity.profile_id().clone(),
                    "store.session-sync.native-topology",
                    roots[0].clone(),
                )
                .expect("main locator"),
            )
            .expect("main admission"),
            AuthorizedRootAdmission::new(
                native_topology_context(feature_scope.clone(), "feature.1", &capability, &use_case),
                RegisteredRootLocatorV1::new(
                    project.clone(),
                    identity.profile_id().clone(),
                    "store.session-sync.native-topology",
                    roots[1].clone(),
                )
                .expect("feature locator"),
            )
            .expect("feature admission"),
        ],
        &capability,
        &use_case,
        UtcMicros(10),
    )
    .expect("authorized registered roots");
    let scope_storage = first_sessions
        .authorized_scope_set_storage()
        .expect("scope-set storage");
    assert!(matches!(
        scope_storage.compare_and_swap(None, &scope_set),
        Ok(ScopeSetCasOutcomeV1::Applied(_))
    ));

    let inventory_snapshot =
        WorktreeInventorySnapshotId::new("worktree-inventory.session-sync.native-topology")
            .expect("inventory snapshot");
    let inventory_epoch = WorktreeInventoryEpoch::new(1).expect("inventory epoch");
    let main_node = StackNodeId::new("stack-node.session-sync.main").expect("main node");
    let feature_node = StackNodeId::new("stack-node.session-sync.feature").expect("feature node");
    let revision = BranchStackRevisionV1::new(
        BranchStackId::new("branch-stack.session-sync").expect("stack"),
        BranchStackRevisionId::new("branch-stack-revision.session-sync.1").expect("revision"),
        inventory_snapshot,
        inventory_epoch,
        BranchStackSourceV1::ExplicitDeclaration,
        vec![
            BranchStackNodeV1 {
                node_id: main_node.clone(),
                project_id: project.clone(),
                repository_id: repository.clone(),
                reference: main_ref.clone(),
                tip: CommitId::new(run_native_topology_git(
                    &repository_root,
                    &["rev-parse", "refs/heads/main"],
                ))
                .expect("main tip"),
                worktree_id: Some(main_worktree.clone()),
            },
            BranchStackNodeV1 {
                node_id: feature_node.clone(),
                project_id: project.clone(),
                repository_id: repository.clone(),
                reference: feature_ref.clone(),
                tip: CommitId::new(run_native_topology_git(
                    &linked_root,
                    &["rev-parse", "refs/heads/feature"],
                ))
                .expect("feature tip"),
                worktree_id: Some(feature_worktree.clone()),
            },
        ],
        vec![BranchStackEdgeV1 {
            dependency: main_node,
            dependent: feature_node,
        }],
    )
    .expect("branch-stack revision");
    let branch_binding = GitBranchStackBindingV1 {
        project_id: project.clone(),
        repository_id: repository.clone(),
        scope_set_id: scope_set_id.clone(),
        scope_set_revision: scope_set.revision(),
        scope_set_digest: scope_set.digest().clone(),
        revision,
    };
    let occupancies = vec![
        GitWorktreeOccupancyV1 {
            project_id: project.clone(),
            repository_id: repository.clone(),
            scope_set_id: scope_set_id.clone(),
            scope_set_revision: scope_set.revision(),
            scope_set_digest: scope_set.digest().clone(),
            worktree_id: main_worktree.clone(),
            reference: Some(main_ref),
        },
        GitWorktreeOccupancyV1 {
            project_id: project.clone(),
            repository_id: repository.clone(),
            scope_set_id: scope_set_id.clone(),
            scope_set_revision: scope_set.revision(),
            scope_set_digest: scope_set.digest().clone(),
            worktree_id: feature_worktree.clone(),
            reference: Some(feature_ref),
        },
    ];
    let projection = crate::git_intelligence::NativeGitIntelligence::new(
        roots[0].clone(),
        repository.clone(),
        main_worktree.clone(),
    )
    .topology_projection(crate::git_intelligence::GIT_HISTORY_MAX_COUNT_LIMIT)
    .expect("native topology projection")
    .with_declared_topology(vec![branch_binding], occupancies)
    .expect("declared topology projection");
    let projection_identity = git_topology_projection_identity(
        git_topology_namespace(&repository).expect("topology namespace"),
    )
    .expect("topology identity");
    let projector_revision =
        GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
            .expect("projector revision");
    let manifest = build_git_topology_manifest_checked(
        projection_identity.clone(),
        &projection,
        &projector_revision,
        &|| Ok(()),
    )
    .expect("topology manifest");
    first_runtime
        .publish_verified_manifest(
            &manifest,
            git_topology_idempotency_key(&projection, &projector_revision)
                .expect("topology idempotency"),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("persist declared topology");

    drop(scope_storage);
    drop(first_runtime);
    drop(first_sessions);
    drop(first_project_database);
    drop(first_registry);

    let restarted_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity.clone(),
        )
        .await
        .expect("restarted session registry");
    let restarted_project_database = restarted_registry
        .project_memory(project.clone(), roots.clone())
        .await
        .expect("restarted project graph database");
    let restarted_sessions = restarted_registry
        .project_sessions(project.clone(), roots.clone())
        .await
        .expect("restarted project sessions database");
    let restarted_runtime = restarted_project_database
        .memory_graph_runtime()
        .expect("restarted project graph runtime");
    assert!(
        restarted_sessions
            .bind_project_graph_runtime(restarted_runtime.clone())
            .is_ok(),
        "bind restarted graph runtime"
    );
    let restarted_scope_storage = restarted_sessions
        .authorized_scope_set_storage()
        .expect("restarted scope-set storage");
    super::git_topology::publish_native_topology(
        Arc::new(restarted_runtime.clone()),
        roots[0].clone(),
        repository.clone(),
        main_worktree,
        restarted_scope_storage.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("session sync must revalidate and republish retained topology after restart");

    let snapshot = restarted_runtime
        .verified_snapshot(
            &projection_identity,
            FactReadControl::new(Arc::new(|| false)),
        )
        .expect("read restarted topology")
        .expect("persisted topology head");
    let store = GitTopologyProjectionStore::from_verified_snapshot_verified(
        snapshot,
        Arc::new(NeverCancelled),
    )
    .expect("verified restarted topology");
    assert_eq!(store.branch_stacks().len(), 1);
    assert_eq!(store.worktree_occupancies().len(), 2);

    let replacement_scope_set = AuthorizedScopeSetAuthority::authorize_registered(
        scope_set_id,
        ScopeSetRevision::new(2).expect("replacement scope revision"),
        vec![
            AuthorizedRootAdmission::new(
                native_topology_context(main_scope, "main.2", &capability, &use_case),
                RegisteredRootLocatorV1::new(
                    project.clone(),
                    identity.profile_id().clone(),
                    "store.session-sync.native-topology",
                    roots[0].clone(),
                )
                .expect("replacement main locator"),
            )
            .expect("replacement main admission"),
            AuthorizedRootAdmission::new(
                native_topology_context(feature_scope, "feature.2", &capability, &use_case),
                RegisteredRootLocatorV1::new(
                    project,
                    identity.profile_id().clone(),
                    "store.session-sync.native-topology",
                    roots[1].clone(),
                )
                .expect("replacement feature locator"),
            )
            .expect("replacement feature admission"),
        ],
        &capability,
        &use_case,
        UtcMicros(20),
    )
    .expect("replacement authorized roots");
    assert!(matches!(
        restarted_scope_storage.compare_and_swap(
            Some(ScopeSetRevision::new(1).expect("prior scope revision")),
            &replacement_scope_set,
        ),
        Ok(ScopeSetCasOutcomeV1::Applied(_))
    ));
    assert_eq!(
        super::git_topology::publish_native_topology(
            Arc::new(restarted_runtime.clone()),
            roots[0].clone(),
            repository,
            feature_worktree,
            restarted_scope_storage,
            Arc::new(AtomicBool::new(false)),
        ),
        Err(GitTopologySyncFailure::Stale)
    );
}

#[test]
fn completed_profile_sweep_only_covers_already_admitted_work() {
    assert!(completed_profile_sweep_covers(
        Some(&UtcMicros(20)),
        UtcMicros(19)
    ));
    assert!(!completed_profile_sweep_covers(
        Some(&UtcMicros(20)),
        UtcMicros(21)
    ));
    assert!(!completed_profile_sweep_covers(None, UtcMicros(19)));
}

#[test]
fn completed_alias_replay_survives_its_original_deadline() {
    let request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.alias").unwrap(),
        IdempotencyKey::new("session-sync.alias").unwrap(),
        SessionSyncScopeV1::new(
            ProjectId::new("project.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        ),
        Deadline::new(UtcMicros(20)).unwrap(),
        CancellationSignal::active("session-sync.alias").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let primary = IdempotencyKey::new("session-sync.primary").unwrap();
    let mut journal = SessionSyncJournalV1::coalesced(&request, UtcMicros(10), primary.clone());
    let pending_alias = journal.clone();
    journal.status = SessionSyncJournalStatusV1::Complete;
    journal.completion = Some(SessionSyncCompletionReceiptV1 {
        admission: journal.admission.clone(),
        coalesced_primary: Some(primary),
        completed_at: UtcMicros(15),
        termination: OperationTermination::Completed,
        stats: SessionSyncStatsV1::default(),
        coverage: vec![SessionSyncSourceCoverageV1 {
            store_scope: "profile".to_owned(),
            coverage: SessionSyncCoverageV1::Complete,
        }],
        source_frontiers: Vec::new(),
        failure_codes: Vec::new(),
    });

    assert!(request.admit_at(UtcMicros(30)).is_err());
    let encoded = serde_json::to_string(&journal).unwrap();
    assert!(matches!(
        decode_matching_journal(&encoded, &request)
            .unwrap()
            .outcome(),
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.admission.idempotency_key == *request.idempotency_key()
                && receipt.coalesced_primary.is_some()
    ));
    assert_eq!(
        coalesced_alias_local_interruption(&journal, &pending_alias, true, UtcMicros(30)),
        None,
        "the primary terminal receipt wins over alias-local timeout and cancellation"
    );
}

#[test]
fn git_recovery_frontier_preserves_the_exact_committed_tuple() {
    let frontier = git_history_frontier_from_meta(Some(1_723_456_789), Some(417)).unwrap();

    assert_eq!(frontier.activity_timestamp, 1_723_456_789);
    assert_eq!(frontier.source_rowid, 417);
    let receipt_frontier =
        git_history_source_frontier(&ProjectId::new("project.fixture").unwrap(), frontier);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&receipt_frontier.committed_cursor_json).unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_789,
            "source_rowid": 417,
        })
    );
    assert!(git_history_frontier_from_meta(None, Some(999)).is_none());
}

#[tokio::test]
async fn cancel_in_alias_activation_gap_mirrors_primary_terminal_receipt() {
    let profile_root = tempfile::tempdir().unwrap();
    let project_root = profile_root.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let project_id = ProjectId::new("project.cancel-alias-race").unwrap();
    let profile_id = UserProfileId::new("profile.cancel-alias-race").unwrap();
    let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        profile_root.path(),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let project_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Profile)
        .unwrap();
    let service = DaemonSessionSyncService::default();
    service
        .register_project(DaemonSessionSyncConfig {
            brain_id: BrainId::new("brain.cancel-alias-race").unwrap(),
            profile_id: profile_id.clone(),
            project_id: project_id.clone(),
            profile_root: profile_root.path().to_path_buf(),
            project_root,
            transcript_source_home: None,
            project_sessions,
            user_sessions: profile_sessions.clone(),
            registry: profile_sessions.clone(),
            startup_import: false,
            project_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
            user_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();
    let scope = SessionSyncScopeV1::new(project_id, profile_id);
    let primary_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.cancel-primary").unwrap(),
        IdempotencyKey::new("session-sync.cancel-primary").unwrap(),
        scope.clone(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.cancel-primary").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let mut primary = SessionSyncJournalV1::queued(&primary_request, UtcMicros(10));
    primary.status = SessionSyncJournalStatusV1::Complete;
    primary.completion = Some(SessionSyncCompletionReceiptV1 {
        admission: primary.admission.clone(),
        coalesced_primary: None,
        completed_at: UtcMicros(20),
        termination: OperationTermination::Completed,
        stats: SessionSyncStatsV1 {
            sessions_imported: 1,
            ..SessionSyncStatsV1::default()
        },
        coverage: vec![SessionSyncSourceCoverageV1 {
            store_scope: "project".to_owned(),
            coverage: SessionSyncCoverageV1::Complete,
        }],
        source_frontiers: Vec::new(),
        failure_codes: Vec::new(),
    });
    let alias_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.cancel-alias").unwrap(),
        IdempotencyKey::new("session-sync.cancel-alias").unwrap(),
        scope.clone(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.cancel-alias").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let alias = SessionSyncJournalV1::coalesced(
        &alias_request,
        UtcMicros(11),
        primary_request.idempotency_key().clone(),
    );
    let primary_key = journal_key(&scope, primary_request.idempotency_key());
    let alias_key = journal_key(&scope, alias_request.idempotency_key());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let cancel_service = service.clone();
    let cancel_barrier = Arc::clone(&barrier);
    let control = tracedecay_application::session_sync::SessionSyncControlV1::new(
        scope,
        alias_request.idempotency_key().clone(),
    );
    let cancel = tokio::spawn(async move {
        cancel_barrier.wait().await;
        SessionSyncServicePort::cancel(&cancel_service, control).await
    });

    assert!(
        profile_sessions
            .insert_session_sync_journal(&primary_key, &serde_json::to_string(&primary).unwrap(),)
            .await
            .unwrap()
    );
    assert!(
        profile_sessions
            .insert_session_sync_journal(&alias_key, &serde_json::to_string(&alias).unwrap())
            .await
            .unwrap()
    );
    assert!(!service.active_contains(&alias_key));
    barrier.wait().await;

    assert!(matches!(
        cancel.await.unwrap(),
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.termination == OperationTermination::Completed
                && receipt.admission.idempotency_key == *alias_request.idempotency_key()
                && receipt.coalesced_primary
                    == Some(primary_request.idempotency_key().clone())
    ));
    let persisted: SessionSyncJournalV1 = serde_json::from_str(
        &profile_sessions
            .read_session_sync_journal(&alias_key)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(persisted.cancel_requested_at.is_none());
    assert_eq!(
        persisted.completion.unwrap().termination,
        OperationTermination::Completed
    );
    drop(runtime);
}

#[tokio::test]
async fn daemon_wide_scan_slot_serializes_concurrent_acquisition() {
    let service = DaemonSessionSyncService::default();
    let first = service.scan_slots.clone().acquire_owned().await.unwrap();
    let second_slots = Arc::clone(&service.scan_slots);
    let mut second = tokio::spawn(async move { second_slots.acquire_owned().await.unwrap() });

    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut second)
            .await
            .is_err(),
        "a second native scan must remain queued while the daemon-wide slot is held"
    );
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut second)
            .await
            .unwrap()
            .is_ok()
    );
}
