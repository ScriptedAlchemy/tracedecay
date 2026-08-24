use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{ProjectId, configuration::CodeIndexWorkerSelectionV1};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
    SharedCodeIndexBytePoolV1,
};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "memory@test.invalid"],
    );
    git(root.path(), &["config", "user.name", "Memory Test"]);
    fs::create_dir_all(root.path().join("src")).expect("create source directory");
    fs::write(
        root.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\n",
    )
    .expect("write source");
    git(root.path(), &["add", "src/lib.rs"]);
    git(root.path(), &["commit", "-q", "-m", "fixture"]);
    root
}

fn worker_reservation_bytes() -> u64 {
    let status = tracedecay_code_index::parallelism::install_worker_plan(
        CodeIndexWorkerSelectionV1::Automatic,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    )
    .expect("install automatic worker plan");
    tracedecay_code_index::parallelism::worker_reservation_bytes(usize::from(
        status.effective_workers,
    ))
}

#[test]
fn captured_source_bytes_are_charged_until_the_snapshot_drops() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-source-memory").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(1024 * 1024).expect("source memory limit"),
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));

    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture source snapshot");
    let retained_bytes = captured
        .retained_bytes
        .iter()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    assert!(retained_bytes > 0);
    assert_eq!(authority.snapshot().used_bytes, retained_bytes * 2);
    drop(captured);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn completed_reconcile_releases_the_captured_file_copy_charge() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-source-build-copy").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));

    scheduler.reconcile_now().expect("publish generation");
    let retained_bytes = scheduler
        .retained_snapshot_bytes
        .iter()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    assert!(retained_bytes > 0);
    assert_eq!(authority.snapshot().used_bytes, retained_bytes);

    assert!(matches!(
        scheduler
            .reconcile_now()
            .expect("reconcile unchanged source"),
        CodeIndexReconcileOutcomeV1::Noop(_)
    ));
    assert_eq!(
        authority.snapshot().used_bytes,
        retained_bytes,
        "the no-build path must drop captured Vec copies before retaining only the Arc charge"
    );
}

#[test]
fn source_capture_refuses_before_build_when_retention_cannot_be_charged() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-source-refusal").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let authority = Arc::new(ProcessResidentMemoryV1::new(NonZeroU64::MIN));
    scheduler.bind_resident_memory(Arc::clone(&authority));

    assert!(matches!(
        scheduler.capture_authoritative_snapshot(None),
        Err(super::CodeIndexSchedulerErrorV1::SnapshotMemoryCapacityUnavailable)
    ));
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn latest_complete_reuses_the_immutable_generation_allocation() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-memory").expect("valid project");
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    assert!(matches!(
        scheduler.reconcile_now().expect("publish generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));

    let first = scheduler.latest_complete().expect("first generation read");
    let second = scheduler.latest_complete().expect("second generation read");

    assert!(
        std::ptr::eq(first.generation(), second.generation()),
        "readers must share the sealed generation instead of deep-cloning it"
    );
    assert!(!first.exact().expect("exact chunks").is_empty());
    let generation_id = first.generation().manifest().generation_id.clone();
    drop(first);
    drop(second);
    drop(scheduler);

    let reopened = CodeIndexWorktreeSchedulerV1::open(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("reopen scheduler");
    assert_eq!(
        reopened
            .latest_complete()
            .expect("restored generation")
            .generation()
            .manifest()
            .generation_id,
        generation_id,
        "borrowed publication encoding must remain restart-compatible"
    );
}

#[test]
fn worker_memory_reservation_is_charged_and_released_by_raii() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-worker-memory").expect("valid project");
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let reservation_bytes = worker_reservation_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(reservation_bytes).expect("worker reservation is nonzero"),
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    let reservation = scheduler
        .reserve_worker_memory()
        .expect("reserve worker memory");
    assert_eq!(authority.snapshot().used_bytes, reservation_bytes);
    drop(reservation);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn worker_memory_reservation_refusal_is_typed() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-worker-refusal").expect("valid project");
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let reservation_bytes = worker_reservation_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(reservation_bytes - 1).expect("positive test limit"),
    ));
    scheduler.bind_resident_memory(authority);
    assert!(matches!(
        scheduler.reserve_worker_memory(),
        Err(super::CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(_))
    ));
}

#[tokio::test]
async fn registry_reports_retained_generation_bytes_without_scheduler_locks() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            ProjectId::new("project.code-index-memory").expect("valid project"),
            project.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");

    // Mount restores whatever the store retained and hands the first build to
    // the background worker, so an empty store reports no retained bytes until
    // that reconcile lands. Settle on the post-reconcile state instead of
    // racing it; the assertions below are unchanged and must all hold at once.
    let stats = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let stats = registry.memory_stats().await;
            if stats.reconciling_worktrees == 0 && stats.retained_generation_encoded_bytes > 0 {
                break stats;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mount-time reconcile publishes a retained generation");

    assert_eq!(stats.mounted_worktrees, 1);
    assert_eq!(stats.reconciling_worktrees, 0);
    assert!(stats.retained_generation_encoded_bytes > 0);
    registry.shutdown().await;
}
