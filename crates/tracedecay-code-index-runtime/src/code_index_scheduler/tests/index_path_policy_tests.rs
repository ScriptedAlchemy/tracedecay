//! `index.exclude.v1` / `index.include.v1` as a restarted owner applies them:
//! each open below is a daemon start under the policy its configuration
//! resolves, over the same sealed store.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{IndexPathPolicyV1, SnapshotFileDispositionV1};

use super::{
    GitFixture, SharedCodeIndexBytePoolV1, published, test_project_id,
    wait_for_live_complete_generation,
};
use crate::code_index_scheduler::{
    CodeIndexDemandAdmissionV1, CodeIndexHintPolicyV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
};
use crate::config::registry_default_index_path_policy;

const SOURCES: &[(&str, &str)] = &[
    ("src/lib.rs", "pub fn kept() {}\n"),
    ("generated-fixtures/gen.rs", "pub fn generated_only() {}\n"),
    ("vendor/kept/lib.rs", "pub fn vendored_kept() {}\n"),
];

/// The shipped defaults plus `extra_exclude`, with `include` as given.
fn policy(extra_exclude: &[&str], include: &[&str]) -> IndexPathPolicyV1 {
    let defaults = registry_default_index_path_policy();
    IndexPathPolicyV1::new(
        defaults
            .exclude_patterns()
            .iter()
            .cloned()
            .chain(extra_exclude.iter().map(|pattern| (*pattern).to_owned()))
            .collect(),
        include
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
    )
    .expect("valid fixture patterns")
}

fn start(
    fixture: &GitFixture,
    store: &Path,
    path_policy: IndexPathPolicyV1,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open_with_policy(
        test_project_id(),
        fixture.path(),
        store.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        CodeIndexHintPolicyV1::default(),
        path_policy,
    )
    .expect("open code-index owner")
}

fn indexed_paths(scheduler: &CodeIndexWorktreeSchedulerV1) -> Vec<String> {
    scheduler
        .latest_complete()
        .expect("published generation")
        .generation()
        .snapshot()
        .files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .map(|file| file.logical_path.clone())
        .collect()
}

#[test]
fn a_restart_under_a_changed_path_policy_republishes_exactly_the_matching_files() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");

    let mut first = start(&fixture, store.path(), policy(&[], &[]));
    let first_generation = published(first.reconcile_now().expect("first index")).generation_id;
    assert_eq!(
        indexed_paths(&first),
        ["generated-fixtures/gen.rs", "src/lib.rs"]
    );
    drop(first);

    let mut unchanged = start(&fixture, store.path(), policy(&[], &[]));
    assert!(
        matches!(
            unchanged.reconcile_now().expect("unchanged restart"),
            CodeIndexReconcileOutcomeV1::Noop(_)
        ),
        "an unchanged policy must not republish"
    );
    assert_eq!(
        unchanged
            .latest_complete()
            .expect("retained generation")
            .generation()
            .manifest()
            .generation_id,
        first_generation
    );
    drop(unchanged);

    let mut excluded = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &[]),
    );
    published(excluded.reconcile_now().expect("exclude restart"));
    assert_eq!(indexed_paths(&excluded), ["src/lib.rs"]);
    drop(excluded);

    let mut included = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &["vendor/kept/**"]),
    );
    published(included.reconcile_now().expect("include restart"));
    assert_eq!(
        indexed_paths(&included),
        ["src/lib.rs", "vendor/kept/lib.rs"]
    );
    drop(included);

    let mut restored = start(&fixture, store.path(), policy(&[], &[]));
    published(restored.reconcile_now().expect("restored restart"));
    assert_eq!(
        indexed_paths(&restored),
        ["generated-fixtures/gen.rs", "src/lib.rs"]
    );
}

#[test]
fn a_sealed_roster_is_reusable_only_under_the_policy_it_was_captured_with() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");
    let mut first = start(&fixture, store.path(), policy(&[], &[]));
    published(first.reconcile_now().expect("first index"));
    let generation = first
        .latest_complete()
        .expect("published generation")
        .generation()
        .clone();
    drop(first);

    let same = start(&fixture, store.path(), policy(&[], &[]));
    let excluding = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &[]),
    );
    let including = start(&fixture, store.path(), policy(&[], &["vendor/kept/**"]));
    assert!(same.roster_follows_path_policy(&generation));
    assert!(!excluding.roster_follows_path_policy(&generation));
    assert!(!including.roster_follows_path_policy(&generation));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_hints_for_excluded_paths_queue_no_work() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    assert_eq!(
        registry
            .notify_hook_paths(fixture.path(), &["vendor/kept/lib.rs".to_owned()])
            .await,
        CodeIndexDemandAdmissionV1::NotApplicable,
        "a hint for a path the default policy excludes must not wake the owner"
    );
    assert_eq!(
        registry
            .notify_hook_paths(
                fixture.path(),
                &["vendor/kept/lib.rs".to_owned(), "src/lib.rs".to_owned()],
            )
            .await,
        CodeIndexDemandAdmissionV1::Queued,
        "an ordinary source path in the same batch is still delivered"
    );
    registry.shutdown().await;
}
