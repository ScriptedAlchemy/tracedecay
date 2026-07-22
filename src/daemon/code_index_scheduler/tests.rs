use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
    SharedCodeIndexBytePoolV1,
};

struct GitFixture {
    root: TempDir,
}

impl GitFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        for (path, source) in files {
            write(root.path(), path, source);
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn edit(&self, path: &str, source: &str) {
        write(self.path(), path, source);
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("source parent")).expect("create source parent");
    std::fs::write(path, source).expect("write fixture source");
}

fn scheduler(
    fixture: &GitFixture,
    store_root: PathBuf,
    bytes: Arc<SharedCodeIndexBytePoolV1>,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(fixture.path(), store_root, bytes)
        .expect("open worktree scheduler")
}

fn published(outcome: CodeIndexReconcileOutcomeV1) -> super::CodeIndexPublishEvidenceV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Noop(_) => panic!("expected a published generation"),
    }
}

#[test]
fn saved_edit_incremental_publish() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);

    let first = published(scheduler.reconcile_now().expect("initial publish"));
    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 2 }\n",
    );
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let second = published(scheduler.reconcile_now().expect("incremental publish"));

    assert_ne!(first.generation_id, second.generation_id);
    assert_eq!(second.incremental_parse_files, 1);
    assert!(second.changed_ranges > 0);
    let latest = scheduler.latest_complete().expect("latest generation");
    assert!(!latest.exact().expect("exact lane").is_empty());
    assert!(!latest.lexical().is_empty());
    assert!(
        !latest.graph_edges().is_empty() || !latest.graph_abstentions().is_empty(),
        "graph lane must remain explicitly queryable"
    );
    let owners = latest
        .production_query_owners()
        .expect("production exact/lexical/graph owners connect");
    let _ = owners.exact;
    let _ = owners.lexical;
    let _ = owners.graph;
}

#[test]
fn duplicate_save_and_overflow_equals_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n"),
        ("src/other.rs", "pub fn other() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut hinted = scheduler(&fixture, store.path().join("hinted"), Arc::clone(&bytes));
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    published(hinted.reconcile_now().expect("hinted baseline"));
    published(clean.reconcile_now().expect("clean baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    let path = fixture.path().join("src/lib.rs");
    hinted.notify_path(path.clone());
    hinted.notify_path(path);
    hinted.notify_overflow();

    let hinted_publish = published(hinted.reconcile_now().expect("hinted reconcile"));
    let clean_publish = published(clean.reconcile_now().expect("clean reconcile"));
    assert_eq!(
        hinted_publish.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(hinted_publish.lane_digest, clean_publish.lane_digest);
    assert!(hinted_publish.overflow_reconciled);
}

#[test]
fn cross_worktree_byte_reuse_without_identity_alias() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);

    let mut first_scheduler = registry
        .open_worktree(first.path(), store.path().join("first"))
        .expect("first scheduler");
    let mut second_scheduler = registry
        .open_worktree(second.path(), store.path().join("second"))
        .expect("second scheduler");
    let first_publish = published(first_scheduler.reconcile_now().expect("first publish"));
    let second_publish = published(second_scheduler.reconcile_now().expect("second publish"));

    assert!(registry.byte_pool_stats().reused >= 1);
    assert_ne!(first_publish.repository_id, second_publish.repository_id);
    assert_ne!(
        first_publish.file_occurrence_ids, second_publish.file_occurrence_ids,
        "shared bytes must never alias repository/worktree occurrence identity"
    );
}

#[test]
fn one_symbol_unrelated_work_skip() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\n\npub fn unrelated() -> u32 { 99 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 2 }\n\npub fn unrelated() -> u32 { 99 }\n",
    );
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let changed = published(scheduler.reconcile_now().expect("one-symbol publish"));

    assert_eq!(changed.reextracted_files, 1);
    assert!(changed.changed_chunks > 0);
    assert!(
        changed.reused_chunks > 0,
        "unrelated symbol chunks must skip projection work"
    );
}

#[test]
fn content_noop_suppresses_publication() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let first = published(scheduler.reconcile_now().expect("baseline publish"));

    match scheduler.reconcile_now().expect("content noop") {
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            assert_eq!(
                evidence.snapshot_content_identity, first.snapshot_content_identity,
                "unchanged content must reuse the sealed snapshot identity"
            );
        }
        CodeIndexReconcileOutcomeV1::Published(_) => {
            panic!("identical content must not publish a new generation")
        }
    }
    let _owners = scheduler
        .latest_complete()
        .expect("active generation")
        .production_query_owners()
        .expect("owners remain connected after content no-op");
}

#[test]
fn superseding_notifies_publish_only_latest_content() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    published(live.reconcile_now().expect("live baseline"));
    published(clean.reconcile_now().expect("clean baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    live.notify_overflow();

    let superseded = published(live.reconcile_now().expect("superseded reconcile"));
    let expected = published(clean.reconcile_now().expect("clean latest reconcile"));
    assert_eq!(
        superseded.snapshot_content_identity, expected.snapshot_content_identity,
        "fair supersession must publish only the latest reconciled content"
    );
    assert_eq!(superseded.lane_digest, expected.lane_digest);
    assert!(superseded.overflow_reconciled);
}

#[test]
fn production_query_owners_bind_exact_lexical_and_graph_lanes() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let owners = scheduler
        .latest_complete()
        .expect("latest generation")
        .production_query_owners()
        .expect("connect production query owners");
    assert!(
        std::mem::size_of_val(&owners.exact) > 0
            && std::mem::size_of_val(&owners.lexical) > 0
            && std::mem::size_of_val(&owners.graph) > 0,
        "exact/lexical/graph production owners must be concrete lane values"
    );
}

#[tokio::test]
async fn daemon_owned_per_worktree_scheduler_reconciles_saved_edits() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(fixture.path(), store.path().to_path_buf(), None)
            .await
            .expect("mount daemon-owned scheduler")
    );

    let first = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(fixture.path()).await {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial generation published");

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(fixture.path()).await
                && generation != first
            {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("saved edit generation published");

    assert_ne!(first, second);
    registry.shutdown().await;
}
