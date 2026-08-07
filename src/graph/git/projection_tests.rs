use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionBindingV1, GitHealthProjectionCoverageV1,
    GitHealthProjectionPartialReasonV1,
};
use tracedecay_domain::{ProjectId, SourceStoreId, UserProfileId};
use tracedecay_graph_db::{
    GraphCancellation, GraphMutation, GraphProperty, GraphPropertyName, GraphWatermark,
    GraphWriteBatch, SourceGeneration,
};

use super::native::{
    AncestorCheckV1, CollectCommitError, collect_root_tree_paths, is_ancestor_bounded,
};
use super::{GitHealthProjectionError, GitHealthProjectionStoreV1, MAX_CHANGED_FILES_PER_COMMIT};
use crate::application::context::CancellationToken;

const NOW_SECS: i64 = 2_000_000_000;
const WINDOW_END: i64 = 1_999_987_200;
const WINDOW_START: i64 = 1_992_211_200;

fn git(root: &Path, args: &[&str]) -> String {
    git_at(root, args, NOW_SECS - 60)
}

fn git_at(root: &Path, args: &[&str], committed_at: i64) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .env("GIT_AUTHOR_DATE", format!("@{committed_at} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{committed_at} +0000"))
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git test output is UTF-8")
        .trim()
        .to_owned()
}

fn git_with_input(root: &Path, args: &[&str], input: &[u8], committed_at: i64) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .env("GIT_AUTHOR_DATE", format!("@{committed_at} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{committed_at} +0000"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git command should start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write Git input");
    let output = child.wait_with_output().expect("wait for Git command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git test output is UTF-8")
        .trim()
        .to_owned()
}

fn repository() -> TempDir {
    let root = TempDir::new().expect("temporary repository");
    git(root.path(), &["init", "--quiet", "-b", "main"]);
    root
}

fn commit_file_at(root: &Path, ordinal: usize, path: &str, committed_at: i64) {
    fs::write(root.join(path), format!("revision {ordinal}\n")).expect("write fixture");
    git_at(root, &["add", path], committed_at);
    git_at(
        root,
        &["commit", "--quiet", "-m", &format!("commit {ordinal}")],
        committed_at,
    );
}

fn commit_file(root: &Path, ordinal: usize, path: &str) {
    commit_file_at(root, ordinal, path, WINDOW_END - 60);
}

fn commit_flat_root(root: &Path, file_count: usize) {
    let blob = git_with_input(
        root,
        &["hash-object", "-w", "--stdin"],
        b"fixture\n",
        WINDOW_END - 60,
    );
    let mut tree_input = Vec::with_capacity(file_count.saturating_mul(80));
    for ordinal in 0..file_count {
        writeln!(tree_input, "100644 blob {blob}\troot-{ordinal:05}.rs")
            .expect("build tree fixture");
    }
    let tree = git_with_input(root, &["mktree"], &tree_input, WINDOW_END - 60);
    let commit = git_at(
        root,
        &["commit-tree", &tree, "-m", "oversized root"],
        WINDOW_END - 60,
    );
    git(root, &["update-ref", "refs/heads/main", &commit]);
}

fn binding(root: &Path) -> GitHealthProjectionBindingV1 {
    let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(root)
        .expect("resolve fixture identity");
    let scope = tracedecay_application::ResolvedScope::new(
        ProjectId::new("project.git-health-correction").expect("project id"),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("resolved scope");
    GitHealthProjectionBindingV1::new(
        scope,
        UserProfileId::new("profile.git-health-correction").expect("profile id"),
        SourceStoreId::new("store.git-health-correction").expect("store id"),
    )
    .expect("projection binding")
}

fn store(root: &TempDir) -> GitHealthProjectionStoreV1 {
    GitHealthProjectionStoreV1::open(
        &root.path().join("project-graph.grafeo"),
        &CancellationToken::new(),
    )
    .expect("open projection")
}

fn finish_projection(
    store: &GitHealthProjectionStoreV1,
    root: &Path,
    binding: &GitHealthProjectionBindingV1,
    now: i64,
) -> usize {
    let mut examined = 0usize;
    for _ in 0..512 {
        let progress = store
            .advance(root, binding, now, 3, &CancellationToken::new())
            .expect("projection batch");
        examined += progress.commits_examined;
        if progress.complete {
            return examined;
        }
    }
    panic!("projection did not complete within fixture bound");
}

fn live_graph_cancellation() -> std::sync::Arc<dyn GraphCancellation> {
    std::sync::Arc::new(super::TokenCancellation(CancellationToken::new()))
}

fn read_churn(
    store: &GitHealthProjectionStoreV1,
    binding: &GitHealthProjectionBindingV1,
    snapshot: &tracedecay_application::GitHealthProjectionSnapshotV1,
) -> BTreeMap<String, usize> {
    let mut churn = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = store
            .read_churn_page(binding, snapshot, cursor.as_deref(), 256)
            .expect("churn page");
        churn.extend(
            page.entries
                .into_iter()
                .map(|entry| (entry.path, entry.churn)),
        );
        let Some(next) = page.next_cursor else {
            return churn;
        };
        cursor = Some(next);
    }
}

fn apply_corruption(
    store: &GitHealthProjectionStoreV1,
    binding: &GitHealthProjectionBindingV1,
    generation: &str,
    watermark: &str,
    mutation: GraphMutation,
) {
    store
        .database()
        .apply(
            GraphWriteBatch::new(
                super::persistence::namespace(binding).expect("namespace"),
                super::persistence::projection().expect("projection"),
                SourceGeneration::new(generation).expect("generation"),
                GraphWatermark::new(watermark).expect("watermark"),
                vec![mutation],
                live_graph_cancellation(),
            )
            .expect("corruption batch"),
        )
        .expect("publish corruption fixture");
}

#[test]
fn a_second_head_projects_only_the_new_commit() {
    let repository = repository();
    for ordinal in 0..12 {
        commit_file(repository.path(), ordinal, "history.rs");
    }
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    assert_eq!(
        finish_projection(&store, repository.path(), &binding, NOW_SECS),
        12
    );
    let GitHealthProjectionAvailabilityV1::Ready { snapshot: before } = store.read(&binding) else {
        panic!("baseline projection must be ready");
    };
    assert_eq!(before.coverage, GitHealthProjectionCoverageV1::Complete);
    commit_file(repository.path(), 12, "history.rs");
    let after = super::capture_source(repository.path(), &binding, NOW_SECS)
        .expect("capture fast-forward source");
    let repository_handle = gix::open(repository.path()).expect("open fixture repository");
    assert_eq!(
        is_ancestor_bounded(
            &repository_handle,
            &before.source.commit,
            &after.commit,
            64,
            || false,
        )
        .expect("bounded ancestry"),
        AncestorCheckV1::Ancestor,
        "the prior ready commit must be recognized as the new HEAD ancestor"
    );
    assert_eq!(
        finish_projection(&store, repository.path(), &binding, NOW_SECS),
        1,
        "a fast-forward must resume from the durable prior frontier"
    );
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("incremental projection must be ready");
    };
    assert_eq!(snapshot.commits_projected, 13);
    assert_eq!(
        read_churn(&store, &binding, &snapshot).get("history.rs"),
        Some(&13)
    );
}

#[test]
fn ancestry_walk_observes_cancellation_before_its_bound() {
    let repository = repository();
    for ordinal in 0..32 {
        commit_file(repository.path(), ordinal, "history.rs");
    }
    let repository_handle = gix::open(repository.path()).expect("open fixture repository");
    let head = super::capture_source(repository.path(), &binding(repository.path()), NOW_SECS)
        .expect("capture head");
    let absent = tracedecay_domain::GitOidV1::new("1111111111111111111111111111111111111111")
        .expect("absent oid");
    let mut checkpoints = 0usize;

    let result = is_ancestor_bounded(&repository_handle, &absent, &head.commit, 64, || {
        checkpoints += 1;
        checkpoints > 5
    });

    assert!(matches!(result, Err(GitHealthProjectionError::Cancelled)));
    assert!(checkpoints <= 6);
}

#[test]
fn ancestry_walk_reports_its_bound_without_claiming_a_negative() {
    let repository = repository();
    for ordinal in 0..8 {
        commit_file(repository.path(), ordinal, "history.rs");
    }
    let repository_handle = gix::open(repository.path()).expect("open fixture repository");
    let head = super::capture_source(repository.path(), &binding(repository.path()), NOW_SECS)
        .expect("capture head");
    let absent = tracedecay_domain::GitOidV1::new("1111111111111111111111111111111111111111")
        .expect("absent oid");

    assert_eq!(
        is_ancestor_bounded(&repository_handle, &absent, &head.commit, 2, || false)
            .expect("bounded ancestry"),
        AncestorCheckV1::TraversalLimit
    );
}

#[test]
fn non_monotonic_ancestor_inside_window_is_not_omitted() {
    let repository = repository();
    commit_file_at(repository.path(), 0, "retained.rs", WINDOW_START + 60);
    commit_file_at(repository.path(), 1, "older-child.rs", WINDOW_START - 60);
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);

    finish_projection(&store, repository.path(), &binding, NOW_SECS);

    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("projection must be ready");
    };
    assert_eq!(snapshot.coverage, GitHealthProjectionCoverageV1::Complete);
    assert_eq!(snapshot.commits_projected, 1);
    assert_eq!(
        read_churn(&store, &binding, &snapshot).get("retained.rs"),
        Some(&1)
    );
}

#[test]
fn exhausted_history_bound_is_persisted_as_partial_not_complete() {
    let repository = repository();
    for ordinal in 0..4 {
        commit_file_at(
            repository.path(),
            ordinal,
            "old.rs",
            WINDOW_START - 100 + ordinal as i64,
        );
    }
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);

    let progress = store
        .advance_with_history_limit(
            repository.path(),
            &binding,
            NOW_SECS,
            16,
            2,
            &CancellationToken::new(),
        )
        .expect("bounded projection");
    assert!(progress.complete);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("bounded projection must publish a typed result");
    };
    assert_eq!(
        snapshot.coverage,
        GitHealthProjectionCoverageV1::Partial {
            reason: GitHealthProjectionPartialReasonV1::HistoryTraversalLimit,
        }
    );
}

#[test]
fn day_rollover_prunes_expired_commits_and_paths_without_rewalking_head() {
    let repository = repository();
    commit_file_at(repository.path(), 0, "expired.rs", WINDOW_START + 1);
    commit_file_at(repository.path(), 1, "retained.rs", WINDOW_START + 86_401);
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);

    let examined = finish_projection(&store, repository.path(), &binding, NOW_SECS + 86_400);
    assert_eq!(examined, 0, "window expiry must not rewalk unchanged HEAD");
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("rolled projection must be ready");
    };
    assert_eq!(snapshot.commits_projected, 1);
    let churn = read_churn(&store, &binding, &snapshot);
    assert!(!churn.contains_key("expired.rs"));
    assert_eq!(churn.get("retained.rs"), Some(&1));
}

#[test]
fn commits_at_or_after_window_end_are_not_projected() {
    let repository = repository();
    commit_file_at(repository.path(), 0, "present.rs", WINDOW_END - 60);
    commit_file_at(repository.path(), 1, "future.rs", WINDOW_END + 1);
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);

    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("projection must be ready");
    };
    assert_eq!(snapshot.commits_projected, 1);
    let churn = read_churn(&store, &binding, &snapshot);
    assert_eq!(churn.get("present.rs"), Some(&1));
    assert!(!churn.contains_key("future.rs"));
}

#[test]
fn oversized_root_tree_stops_at_the_path_bound_with_partial_coverage() {
    let repository = repository();
    commit_flat_root(repository.path(), MAX_CHANGED_FILES_PER_COMMIT + 1);
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);

    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("bounded projection must publish typed partial coverage");
    };
    assert_eq!(
        snapshot.coverage,
        GitHealthProjectionCoverageV1::Partial {
            reason: GitHealthProjectionPartialReasonV1::CommitPathLimit,
        }
    );
    assert_eq!(snapshot.churn_entries, 0);
}

#[test]
fn root_tree_walk_observes_cancellation_before_the_path_bound() {
    let repository = repository();
    commit_flat_root(repository.path(), MAX_CHANGED_FILES_PER_COMMIT + 1);
    let repository_handle = gix::open(repository.path()).expect("open fixture repository");
    let tree = repository_handle
        .head_commit()
        .expect("HEAD commit")
        .tree()
        .expect("HEAD tree");
    let mut visits = 0usize;

    let result = collect_root_tree_paths(&tree, || {
        visits += 1;
        visits > 10
    });

    assert!(matches!(
        result,
        Err(CollectCommitError::Projection(
            GitHealthProjectionError::Cancelled
        ))
    ));
    assert!(visits <= 11);
}

#[test]
fn reopen_resumes_the_persisted_frontier() {
    let repository = repository();
    for ordinal in 0..11 {
        commit_file(repository.path(), ordinal, "resume.rs");
    }
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store_path = store_root.path().join("project-graph.grafeo");
    let cancellation = CancellationToken::new();
    {
        let store =
            GitHealthProjectionStoreV1::open(&store_path, &cancellation).expect("open store");
        let first = store
            .advance(repository.path(), &binding, NOW_SECS, 3, &cancellation)
            .expect("first batch");
        assert_eq!(first.commits_examined, 3);
        assert!(!first.complete);
    }

    let reopened =
        GitHealthProjectionStoreV1::open(&store_path, &cancellation).expect("reopen store");
    assert_eq!(
        finish_projection(&reopened, repository.path(), &binding, NOW_SECS),
        8
    );
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = reopened.read(&binding) else {
        panic!("resumed projection must become ready");
    };
    assert_eq!(snapshot.commits_projected, 11);
}

#[test]
fn persisted_state_with_a_foreign_project_profile_and_store_is_typed_as_corrupt() {
    let repository = repository();
    commit_file(repository.path(), 0, "history.rs");
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);
    let mut ready = store
        .read_state::<super::ReadyStateV1>(&binding, super::READY_ENTITY, live_graph_cancellation())
        .expect("ready state")
        .expect("ready state exists");
    let generation = ready.source.projection_generation.as_str().to_owned();
    let watermark = format!("{generation}:{}", ready.counters.batches_completed);
    let foreign_scope = tracedecay_application::ResolvedScope::new(
        ProjectId::new("project.foreign").expect("foreign project"),
        binding.scope.repository_id.clone(),
        binding.scope.worktree_id.clone(),
        binding.scope.reference.clone(),
    )
    .expect("foreign scope");
    ready.source.binding = GitHealthProjectionBindingV1::new(
        foreign_scope,
        UserProfileId::new("profile.foreign").expect("foreign profile"),
        SourceStoreId::new("store.foreign").expect("foreign store"),
    )
    .expect("foreign binding");
    apply_corruption(
        &store,
        &binding,
        &generation,
        &watermark,
        GraphMutation::UpsertEntity(
            super::persistence::state_entity(super::READY_ENTITY, &ready)
                .expect("corrupt state entity"),
        ),
    );
    assert_eq!(
        store.read(&binding),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason:
                tracedecay_application::GitHealthProjectionUnavailableReasonV1::CorruptProjection,
        }
    );
}

#[test]
fn persisted_source_identity_must_match_its_authenticated_generation() {
    let repository = repository();
    commit_file(repository.path(), 0, "history.rs");
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);
    let mut ready = store
        .read_state::<super::ReadyStateV1>(&binding, super::READY_ENTITY, live_graph_cancellation())
        .expect("ready state")
        .expect("ready state exists");
    let generation = ready.source.projection_generation.as_str().to_owned();
    let watermark = format!("{generation}:{}", ready.counters.batches_completed);
    ready.source.tree =
        tracedecay_domain::GitOidV1::new("2222222222222222222222222222222222222222")
            .expect("foreign tree");
    apply_corruption(
        &store,
        &binding,
        &generation,
        &watermark,
        GraphMutation::UpsertEntity(
            super::persistence::state_entity(super::READY_ENTITY, &ready)
                .expect("corrupt state entity"),
        ),
    );
    assert_eq!(
        store.read(&binding),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason:
                tracedecay_application::GitHealthProjectionUnavailableReasonV1::CorruptProjection,
        }
    );
}

#[test]
fn persisted_commit_payload_must_authenticate_its_entity_identity() {
    let repository = repository();
    commit_file(repository.path(), 0, "history.rs");
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);
    let GitHealthProjectionAvailabilityV1::Ready { snapshot } = store.read(&binding) else {
        panic!("projection ready");
    };
    let generation = snapshot.source.projection_generation.as_str().to_owned();
    let watermark = format!("{generation}:{}", snapshot.batches_completed);
    let mut entity = store
        .projection_entities(&binding, live_graph_cancellation())
        .expect("projection entities")
        .into_iter()
        .find(|entity| {
            entity.labels.contains(
                &tracedecay_graph_db::GraphLabel::new(super::COMMIT_LABEL).expect("label"),
            )
        })
        .expect("commit entity");
    let mut record =
        super::persistence::commit_record_from_entity(&entity, None).expect("commit record");
    record.oid = tracedecay_domain::GitOidV1::new("1111111111111111111111111111111111111111")
        .expect("foreign oid");
    entity.properties.insert(
        GraphPropertyName::new(super::COMMIT_PROPERTY).expect("property"),
        GraphProperty::Bytes(serde_json::to_vec(&record).expect("record JSON")),
    );
    apply_corruption(
        &store,
        &binding,
        &generation,
        &watermark,
        GraphMutation::UpsertEntity(entity),
    );
    assert_eq!(
        store.read(&binding),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason:
                tracedecay_application::GitHealthProjectionUnavailableReasonV1::CorruptProjection,
        }
    );
}

#[test]
fn persisted_projection_commit_metadata_must_match_ready_generation_and_watermark() {
    let repository = repository();
    commit_file(repository.path(), 0, "history.rs");
    let binding = binding(repository.path());
    let store_root = TempDir::new().expect("projection root");
    let store = store(&store_root);
    finish_projection(&store, repository.path(), &binding, NOW_SECS);
    let ready = store
        .read_state::<super::ReadyStateV1>(&binding, super::READY_ENTITY, live_graph_cancellation())
        .expect("ready state")
        .expect("ready state exists");
    apply_corruption(
        &store,
        &binding,
        "foreign-generation",
        "foreign-generation:foreign-watermark",
        GraphMutation::UpsertEntity(
            super::persistence::state_entity(super::READY_ENTITY, &ready)
                .expect("ready state entity"),
        ),
    );

    assert_eq!(
        store.read(&binding),
        GitHealthProjectionAvailabilityV1::Unavailable {
            reason:
                tracedecay_application::GitHealthProjectionUnavailableReasonV1::CorruptProjection,
        }
    );
}
