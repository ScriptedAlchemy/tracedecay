use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexWorktreeSchedulerV1, PendingHintsV1,
    SharedCodeIndexBytePoolV1,
};

struct GitFixture {
    root: TempDir,
}

impl GitFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
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
    let status = Command::new(crate::git::git_program())
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

fn scheduler(fixture: &GitFixture, store: &TempDir) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.warm-delta-tests").expect("project id"),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler")
}

fn published(outcome: CodeIndexReconcileOutcomeV1) -> super::CodeIndexPublishEvidenceV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            panic!("expected a published generation, got {evidence:?}")
        }
    }
}

/// Removing exact-path reconciliation or falling back to a repository sweep
/// makes `source_files_read` exceed one and this journey fail.
#[test]
fn warm_hook_reconcile_reads_only_the_one_exact_changed_path() {
    let sources = (0..24)
        .map(|index| {
            (
                format!("src/file_{index:02}.rs"),
                format!("pub fn value_{index:02}() -> u32 {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let source_refs = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&source_refs);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/file_07.rs", "pub fn value_07() -> u32 { 700 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/file_07.rs")]);

    let update = published(scheduler.reconcile_now().expect("warm hook reconcile"));
    assert_eq!(update.source_files_read, 1);
    assert_eq!(update.full_status_scans, 0);
    assert_eq!(update.head_tree_diffs, 0);
    assert_eq!(update.reextracted_files, 1);
}

#[test]
fn exact_hook_path_treats_pathspec_metacharacters_as_literals() {
    let fixture = GitFixture::new(&[
        ("src/[literal].rs", "pub fn literal() -> u32 { 1 }\n"),
        ("src/l.rs", "pub fn decoy() -> u32 { 1 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/[literal].rs", "pub fn literal() -> u32 { 2 }\n");
    fixture.edit("src/l.rs", "pub fn decoy() -> u32 { 2 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/[literal].rs")]);

    let exact = published(scheduler.reconcile_now().expect("literal path reconcile"));
    assert_eq!(exact.source_files_read, 1);
    assert_eq!(exact.full_status_scans, 0);

    let backstop = published(scheduler.reconcile_now().expect("remaining dirty path"));
    assert_eq!(backstop.source_files_read, 2);
    assert_eq!(backstop.full_status_scans, 1);
}

#[test]
fn exact_hook_reconciles_a_dirty_file_reverted_to_clean_head_content() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn value() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/lib.rs", "pub fn value() -> u32 { 2 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
    let dirty = published(scheduler.reconcile_now().expect("dirty generation"));
    assert_ne!(
        dirty.snapshot_content_identity,
        baseline.snapshot_content_identity
    );

    fixture.edit("src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
    let reverted = published(scheduler.reconcile_now().expect("reverted generation"));
    assert_eq!(
        reverted.snapshot_content_identity,
        baseline.snapshot_content_identity
    );
    assert_eq!(reverted.source_files_read, 1);
    assert_eq!(reverted.full_status_scans, 0);
}

#[test]
fn full_status_backstop_reconciles_a_dropped_revert_event() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn value() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/lib.rs", "pub fn value() -> u32 { 2 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
    published(scheduler.reconcile_now().expect("dirty generation"));

    fixture.edit("src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    let reverted = published(scheduler.reconcile_now().expect("dropped-event backstop"));
    assert_eq!(
        reverted.snapshot_content_identity,
        baseline.snapshot_content_identity
    );
    assert_eq!(reverted.source_files_read, 1);
    assert_eq!(reverted.full_status_scans, 1);
}

/// A HEAD move has one exact old-tree -> new-tree frontier. The tree diff may
/// enumerate twenty changes, but unrelated source files must remain unread.
#[test]
fn warm_head_reconcile_diffs_once_and_reads_only_twenty_changed_paths() {
    let sources = (0..40)
        .map(|index| {
            (
                format!("src/file_{index:02}.rs"),
                format!("pub fn value_{index:02}() -> u32 {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let source_refs = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&source_refs);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    published(scheduler.reconcile_now().expect("baseline"));

    for index in 0..20 {
        fixture.edit(
            &format!("src/file_{index:02}.rs"),
            &format!("pub fn value_{index:02}() -> u32 {{ {} }}\n", index + 1_000),
        );
    }
    git(fixture.path(), &["commit", "-qam", "twenty-file update"]);

    let update = published(scheduler.reconcile_now().expect("warm HEAD reconcile"));
    assert_eq!(update.source_files_read, 20);
    assert_eq!(update.full_status_scans, 0);
    assert_eq!(update.head_tree_diffs, 1);
    assert_eq!(update.reextracted_files, 20);
}

/// The bounded full-status backstop is allowed to prove a quiet worktree, but
/// it must not turn that proof into an unchanged source-byte sweep.
#[test]
fn warm_noop_backstop_reads_zero_source_files() {
    let fixture = GitFixture::new(&[
        ("src/a.rs", "pub fn a() -> u32 { 1 }\n"),
        ("src/b.rs", "pub fn b() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    published(scheduler.reconcile_now().expect("baseline"));

    let noop = match scheduler.reconcile_now().expect("warm no-op reconcile") {
        CodeIndexReconcileOutcomeV1::Noop(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Published(evidence) => {
            panic!("unchanged worktree published {:?}", evidence.generation_id)
        }
    };
    assert_eq!(noop.source_files_read, 0);
    assert_eq!(noop.full_status_scans, 1);
    assert_eq!(noop.head_tree_diffs, 0);
}

#[test]
fn superseded_retry_backstop_recaptures_every_unpublished_dirty_path() {
    let fixture = GitFixture::new(&[
        ("src/a.rs", "pub fn a() -> u32 { 1 }\n"),
        ("src/b.rs", "pub fn b() -> u32 { 1 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(&fixture, &store);
    published(scheduler.reconcile_now().expect("baseline"));
    let identity = scheduler.identity().clone();

    fixture.edit("src/a.rs", "pub fn a() -> u32 { 2 }\n");
    let mut first_hints = PendingHintsV1::default();
    first_hints.path(PathBuf::from("src/a.rs"));
    let first = scheduler
        .capture_authoritative_snapshot(&identity, &first_hints, false)
        .expect("first attempt capture");
    assert_eq!(first.measurements.source_files_read, 1);

    fixture.edit("src/b.rs", "pub fn b() -> u32 { 2 }\n");
    let mut retry_hints = PendingHintsV1::default();
    retry_hints.path(PathBuf::from("src/b.rs"));
    let retry = scheduler
        .capture_authoritative_snapshot(&identity, &retry_hints, true)
        .expect("superseded retry capture");
    assert_eq!(retry.measurements.full_status_scans, 1);
    assert_eq!(
        retry.measurements.source_files_read, 2,
        "the retry must retain neither unpublished attempt as authority"
    );
}
