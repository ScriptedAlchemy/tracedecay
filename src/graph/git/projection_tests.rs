use std::fs;
use std::path::Path;

use tempfile::TempDir;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionCoverageV1, GitHealthProjectionSourceV1,
    ResolvedScope,
};
use tracedecay_domain::ProjectId;
use tracedecay_graph_db::GraphDbError;

use super::{
    CommitRecordV1, GitHealthProjectionError, GitHealthProjectionStoreV1, MAX_DURABLE_FRONTIER,
    MAX_UNIQUE_PATHS, MAX_WINDOW_COMMITS, ProjectionCountersV1, TokenCancellation, WorkingStateV1,
    capture_source,
};
use crate::application::context::CancellationToken;

const NOW_SECS: i64 = 2_000_000_000;

#[test]
fn non_final_graph_store_shape_requires_typed_reset() {
    let error = GitHealthProjectionError::from(GraphDbError::ResetRequired {
        message: "unsupported projection format".to_owned(),
    });

    assert!(matches!(
        error.unavailable_reason(),
        tracedecay_application::GitHealthProjectionUnavailableReasonV1::ResetRequired
    ));
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .env("GIT_AUTHOR_DATE", format!("@{} +0000", NOW_SECS - 60))
        .env("GIT_COMMITTER_DATE", format!("@{} +0000", NOW_SECS - 60))
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let root = TempDir::new().expect("temporary repository");
    git(root.path(), &["init", "--quiet", "-b", "main"]);
    root
}

fn commit_file(root: &Path, ordinal: usize, path: &str) {
    fs::write(root.join(path), format!("revision {ordinal}\n")).expect("write fixture");
    git(root, &["add", path]);
    git(
        root,
        &["commit", "--quiet", "-m", &format!("commit {ordinal}")],
    );
}

fn scope(root: &Path) -> ResolvedScope {
    let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(root)
        .expect("resolve fixture identity");
    ResolvedScope::new(
        ProjectId::new("project.git-health-test").expect("project id"),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope")
}

fn finish_projection(
    store: &GitHealthProjectionStoreV1,
    root: &Path,
    scope: &ResolvedScope,
    batch_limit: usize,
    cancellation: &CancellationToken,
) {
    for _ in 0..256 {
        let progress = store
            .advance(root, scope, NOW_SECS, batch_limit, cancellation)
            .expect("projection batch");
        assert!(progress.commits_examined <= batch_limit);
        if progress.complete {
            return;
        }
    }
    panic!("projection did not complete within the fixture bound");
}

#[test]
fn bounded_projection_preserves_exact_source_identity_and_does_not_rebuild_same_head() {
    let root = repository();
    for ordinal in 0..13 {
        commit_file(root.path(), ordinal, "src.rs");
    }
    let scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let cancellation = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(
        &store_dir.path().join("git-health.grafeo"),
        &cancellation,
    )
    .expect("open projection");

    let first = store
        .advance(root.path(), &scope, NOW_SECS, 3, &cancellation)
        .expect("first batch");
    assert_eq!(first.commits_examined, 3);
    assert!(!first.complete);
    assert!(matches!(
        store.read(&scope),
        GitHealthProjectionAvailabilityV1::Warming { .. }
    ));

    finish_projection(&store, root.path(), &scope, 3, &cancellation);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&scope) else {
        panic!("completed projection must be ready");
    };
    let expected = capture_source(root.path(), &scope, NOW_SECS).expect("source identity");
    assert_eq!(snapshot.source, expected);
    assert_eq!(snapshot.source.scope, scope);
    assert_eq!(snapshot.commits_projected, 13);
    assert_eq!(snapshot.file_churn.get("src.rs"), Some(&13));
    assert_eq!(snapshot.coverage, GitHealthProjectionCoverageV1::Complete);

    let no_op = store
        .advance(root.path(), &scope, NOW_SECS, 3, &cancellation)
        .expect("same-head no-op");
    assert_eq!(no_op.commits_examined, 0);
    assert!(no_op.complete);
    let GitHealthProjectionAvailabilityV1::Ready {
        snapshot: unchanged,
    } = store.read(&scope)
    else {
        panic!("same source remains ready");
    };
    assert_eq!(unchanged.batches_completed, snapshot.batches_completed);
    assert_eq!(
        unchanged.source.projection_generation,
        snapshot.source.projection_generation
    );
}

#[test]
fn same_head_advances_the_day_window_without_rewalking_history() {
    let root = repository();
    for ordinal in 0..5 {
        commit_file(root.path(), ordinal, "daily.rs");
    }
    let scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let cancellation = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(
        &store_dir.path().join("project-graph.grafeo"),
        &cancellation,
    )
    .expect("open projection");
    finish_projection(&store, root.path(), &scope, 2, &cancellation);

    let next_day = NOW_SECS + 24 * 60 * 60;
    let progress = store
        .advance(root.path(), &scope, next_day, 2, &cancellation)
        .expect("advance day boundary");
    assert_eq!(progress.commits_examined, 0);
    assert!(progress.complete);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&scope) else {
        panic!("same-head day-boundary advance must stay ready");
    };
    assert_eq!(snapshot.file_churn.get("daily.rs"), Some(&5));
    assert_eq!(snapshot.source.window_end_epoch_secs, {
        next_day - next_day.rem_euclid(24 * 60 * 60)
    });
}

#[test]
fn cancelled_refresh_never_promotes_partial_working_state_to_ready() {
    let root = repository();
    commit_file(root.path(), 0, "stable.rs");
    let scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let store_path = store_dir.path().join("git-health.grafeo");
    let active = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(&store_path, &active).expect("open projection");
    finish_projection(&store, root.path(), &scope, 8, &active);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot: before } = store.read(&scope) else {
        panic!("baseline projection must be ready");
    };

    commit_file(root.path(), 1, "new.rs");
    let refresh = store
        .advance(root.path(), &scope, NOW_SECS, 1, &active)
        .expect("start refresh");
    assert!(!refresh.complete);
    let GitHealthProjectionAvailabilityV1::Warming {
        target: Some(target),
    } = store.read(&scope)
    else {
        panic!("incomplete durable state must remain warming");
    };
    assert_ne!(target.commit, before.source.commit);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        store.advance(root.path(), &scope, NOW_SECS, 8, &cancelled),
        Err(GitHealthProjectionError::Cancelled)
    ));
    assert!(matches!(
        store.read(&scope),
        GitHealthProjectionAvailabilityV1::Warming { .. }
    ));
}

#[test]
fn ref_drift_is_rejected_before_projection_state_changes() {
    let root = repository();
    commit_file(root.path(), 0, "stable.rs");
    let scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let cancellation = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(
        &store_dir.path().join("git-health.grafeo"),
        &cancellation,
    )
    .expect("open projection");

    git(root.path(), &["switch", "--quiet", "-c", "other"]);
    assert!(matches!(
        store.advance(root.path(), &scope, NOW_SECS, 8, &cancellation),
        Err(GitHealthProjectionError::ScopeDrift)
    ));
    assert_eq!(
        store.read(&scope),
        GitHealthProjectionAvailabilityV1::Warming { target: None }
    );
}

#[test]
fn reopening_resumes_the_persisted_projection_frontier() {
    let root = repository();
    for ordinal in 0..11 {
        commit_file(root.path(), ordinal, "resume.rs");
    }
    let scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let store_path = store_dir.path().join("git-health.grafeo");
    let cancellation = CancellationToken::new();
    {
        let store =
            GitHealthProjectionStoreV1::open(&store_path, &cancellation).expect("open store");
        let first = store
            .advance(root.path(), &scope, NOW_SECS, 3, &cancellation)
            .expect("first batch");
        assert_eq!(first.commits_examined, 3);
        assert!(!first.complete);
    }

    let reopened =
        GitHealthProjectionStoreV1::open(&store_path, &cancellation).expect("reopen store");
    finish_projection(&reopened, root.path(), &scope, 3, &cancellation);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = reopened.read(&scope) else {
        panic!("resumed projection must complete");
    };
    assert_eq!(snapshot.commits_projected, 11);
    assert_eq!(snapshot.file_churn.get("resume.rs"), Some(&11));
}

#[test]
fn source_identity_changes_with_commit_tree_and_projection_generation() {
    let root = repository();
    commit_file(root.path(), 0, "identity.rs");
    let scope = scope(root.path());
    let before: GitHealthProjectionSourceV1 =
        capture_source(root.path(), &scope, NOW_SECS).expect("first source");

    commit_file(root.path(), 1, "identity.rs");
    let after = capture_source(root.path(), &scope, NOW_SECS).expect("second source");
    assert_eq!(after.scope, before.scope);
    assert_ne!(after.commit, before.commit);
    assert_ne!(after.tree, before.tree);
    assert_ne!(after.projection_generation, before.projection_generation);
}

#[test]
fn non_fast_forward_scope_switch_replaces_obsolete_branch_entities() {
    let root = repository();
    commit_file(root.path(), 0, "base.rs");
    commit_file(root.path(), 1, "old-only.rs");
    let main_scope = scope(root.path());
    let store_dir = TempDir::new().expect("temporary projection store");
    let cancellation = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(
        &store_dir.path().join("project-graph.grafeo"),
        &cancellation,
    )
    .expect("open projection");
    finish_projection(&store, root.path(), &main_scope, 4, &cancellation);

    git(root.path(), &["switch", "--quiet", "-c", "other", "HEAD~1"]);
    commit_file(root.path(), 2, "branch-only.rs");
    let branch_scope = scope(root.path());
    finish_projection(&store, root.path(), &branch_scope, 4, &cancellation);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&branch_scope) else {
        panic!("replacement branch projection must become ready");
    };
    assert_eq!(snapshot.file_churn.get("base.rs"), Some(&1));
    assert_eq!(snapshot.file_churn.get("branch-only.rs"), Some(&1));
    assert_eq!(snapshot.file_churn.get("old-only.rs"), None);

    let entities = store
        .projection_entities(
            &branch_scope,
            std::sync::Arc::new(TokenCancellation(CancellationToken::new())),
        )
        .expect("bounded retained projection");
    assert_eq!(entities.len(), 6);
}

#[test]
fn total_commit_path_and_frontier_bounds_produce_typed_partial_coverage() {
    let root = repository();
    commit_file(root.path(), 0, "base.rs");
    let scope = scope(root.path());
    let target = capture_source(root.path(), &scope, NOW_SECS).expect("source");
    let store_dir = TempDir::new().expect("temporary projection store");
    let cancellation = CancellationToken::new();
    let store = GitHealthProjectionStoreV1::open(
        &store_dir.path().join("project-graph.grafeo"),
        &cancellation,
    )
    .expect("open projection");
    let record = CommitRecordV1 {
        oid: target.commit.clone(),
        tree: target.tree.clone(),
        committed_at_epoch_secs: NOW_SECS,
        parents: Vec::new(),
        changed_files: vec!["new.rs".to_owned()],
    };
    let graph_cancellation = std::sync::Arc::new(TokenCancellation(cancellation.clone()));

    let mut commits = WorkingStateV1 {
        target: target.clone(),
        pending: Default::default(),
        counters: ProjectionCountersV1 {
            commits_projected: MAX_WINDOW_COMMITS,
            ..ProjectionCountersV1::default()
        },
        complete: false,
    };
    assert_eq!(
        store
            .admission_failure(
                &scope,
                &commits,
                &record,
                &Default::default(),
                graph_cancellation.clone(),
            )
            .expect("commit bound"),
        Some(tracedecay_application::GitHealthProjectionPartialReasonV1::CommitLimit)
    );

    commits.counters.commits_projected = 0;
    commits.counters.unique_paths = MAX_UNIQUE_PATHS;
    assert_eq!(
        store
            .admission_failure(
                &scope,
                &commits,
                &record,
                &Default::default(),
                graph_cancellation,
            )
            .expect("path bound"),
        Some(tracedecay_application::GitHealthProjectionPartialReasonV1::UniquePathLimit)
    );

    let parents = vec![target.commit; MAX_DURABLE_FRONTIER + 1];
    commits.admit_parents(&parents);
    assert_eq!(
        commits.counters.coverage,
        GitHealthProjectionCoverageV1::Partial {
            reason: tracedecay_application::GitHealthProjectionPartialReasonV1::FrontierLimit,
        }
    );
    assert!(commits.pending.is_empty());
}
