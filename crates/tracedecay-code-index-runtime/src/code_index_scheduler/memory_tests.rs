use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{ProjectId, configuration::CodeIndexWorkerSelectionV1};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1, ResidentMemoryComponentIdV1,
    ResidentMemoryPressureV1, process_resident_memory_limit_for_system_v1,
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
        CodeIndexWorkerSelectionV1::Automatic {},
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    )
    .expect("install automatic worker plan");
    tracedecay_code_index::parallelism::worker_reservation_bytes(usize::from(
        status.effective_workers,
    ))
}

fn expected_worker_reservation_on(remaining_bytes: u64) -> u64 {
    let planned_workers = tracedecay_code_index::parallelism::indexing_workers();
    let affordable = tracedecay_code_index::parallelism::memory_safe_worker_count(remaining_bytes);
    tracedecay_code_index::parallelism::worker_reservation_bytes(
        planned_workers.min(affordable).max(1),
    )
}

#[test]
fn large_host_authority_admits_worker_scratch_lexical_build_and_snapshot() {
    let host_memory_bytes = 88 * 1024 * 1024 * 1024;
    let limit = process_resident_memory_limit_for_system_v1(host_memory_bytes).get();
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(limit).expect("derived host authority is nonzero"),
    ));
    let worker_scratch_bytes = limit.saturating_sub(limit / 4);
    let lexical_build_bytes =
        u64::try_from(super::CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1)
            .expect("lexical build budget fits u64");
    let retained_snapshot_bytes = 64 * 1024 * 1024;

    let _workers = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test.code-index.worker-scratch")
                .expect("valid component"),
            NonZeroU64::new(worker_scratch_bytes).expect("worker scratch is nonzero"),
        )
        .expect("host authority admits the maximum automatic worker scratch");
    let _lexical = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test.query.lexical-build").expect("valid component"),
            NonZeroU64::new(lexical_build_bytes).expect("lexical build is nonzero"),
        )
        .expect("worker scratch and lexical build can coexist");
    let _snapshot = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test.code-index.retained-snapshot")
                .expect("valid component"),
            NonZeroU64::new(retained_snapshot_bytes).expect("snapshot charge is nonzero"),
        )
        .expect("bounded source retention cannot be crowded out by admitted work");

    assert_eq!(
        authority.snapshot().used_bytes,
        worker_scratch_bytes + lexical_build_bytes + retained_snapshot_bytes
    );
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
    assert_eq!(authority.snapshot().used_bytes, retained_bytes);
    drop(captured);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

/// Capture became proportional to the change set: a reconcile over an
/// unchanged checkout reuses the active generation's rows instead of
/// re-reading them, so it retains no source Arcs and holds no charge for
/// bytes it is not keeping resident. This pins the invariant that survived
/// that change — the charge always equals what the scheduler still retains,
/// on the build path and on the no-build path alike — rather than the
/// pre-proportional behaviour where every reconcile re-captured, and so
/// re-charged, the whole snapshot.
#[test]
fn reconcile_charges_exactly_the_snapshot_sources_it_retains() {
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
    let retained_after_no_build = scheduler
        .retained_snapshot_bytes
        .iter()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    assert!(
        retained_after_no_build <= retained_bytes,
        "a reuse-only capture never retains more source than the snapshot it reused"
    );
    assert_eq!(
        authority.snapshot().used_bytes,
        retained_after_no_build,
        "the no-build path charges exactly the Arc sources it still retains, so a \
         reuse-only pass neither strands the previous charge nor holds one for bytes \
         it released"
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
    let _installed = worker_reservation_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    let reservation = scheduler
        .reserve_worker_memory()
        .expect("reserve worker memory");
    assert_eq!(
        authority.snapshot().used_bytes,
        expected_worker_reservation_on(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get())
    );
    drop(reservation);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

/// A host-sized process-global plan must not spend the 6 GiB default
/// authority's typed snapshot headroom. The live failure was remount seating
/// gen 00000001, then refusing a 31-byte successor snapshot because
/// `reserve_worker_memory` had reserved `remaining / 128MiB` and used==limit.
#[test]
fn default_authority_worker_reserve_leaves_typed_snapshot_headroom() {
    let _installed = worker_reservation_bytes();
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-worker-headroom").expect("valid project");
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));

    let _worker = scheduler
        .reserve_worker_memory()
        .expect("6 GiB authority admits a memory-safe worker slab");
    let used = authority.snapshot().used_bytes;
    let limit = DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get();
    assert_eq!(used, expected_worker_reservation_on(limit));
    assert!(
        used < limit,
        "worker reserve must leave the typed non-worker headroom: used={used} limit={limit}"
    );

    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("31-byte-class snapshot must admit beside the worker slab");
    assert!(
        !captured.retained_bytes.is_empty(),
        "the fixture source must charge snapshot bytes"
    );
    assert!(
        authority.snapshot().used_bytes > used,
        "snapshot charge is a separate ledger entry, not a silent borrow of worker scratch"
    );
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
    let _installed = worker_reservation_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(
            tracedecay_code_index::parallelism::INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1 - 1,
        )
        .expect("positive test limit"),
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

/// Measured RSS, not the reservation ledger, decides worker admission once a
/// sample says the process is over budget — and the refusal names the observed
/// and configured bytes so it is never a silent stall.
#[test]
fn measured_rss_pressure_refuses_worker_admission_and_readmits_as_it_falls() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let project_id = ProjectId::new("project.code-index-rss-pressure").expect("valid project");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");

    // A limit with ample room for the worker plan, so the only thing that can
    // refuse admission below is the injected measurement.
    let limit =
        NonZeroU64::new(worker_reservation_bytes().saturating_mul(4)).expect("positive test limit");
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
    scheduler.bind_resident_memory(Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::clone(&pressure),
    )));

    // Nothing sampled yet: admission falls back to the reservation ceiling.
    drop(
        scheduler
            .reserve_worker_memory()
            .expect("an unobserved process admits on the reservation ceiling alone"),
    );

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes() + 1);
    let failure = scheduler
        .reserve_worker_memory()
        .expect_err("measured RSS over the high watermark refuses new worker admission");
    let super::CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(admission) = &failure else {
        panic!("expected a worker resident-memory admission failure, got {failure:?}");
    };
    assert!(
        admission.is_observed_over_budget(),
        "the refusal must name measured pressure, not a full reservation ledger"
    );
    let rendered = failure.to_string();
    assert!(
        rendered.contains(&(pressure.high_watermark_bytes() + 1).to_string()),
        "the refusal names observed bytes: {rendered}"
    );
    assert!(
        rendered.contains(&limit.get().to_string()),
        "the refusal names configured bytes: {rendered}"
    );
    assert!(
        failure.is_transient_capacity_failure(),
        "an over-budget refusal is retryable as pressure falls"
    );

    // Hysteresis: between the watermarks the refusal stands rather than flapping.
    let between = u64::midpoint(
        pressure.low_watermark_bytes(),
        pressure.high_watermark_bytes(),
    );
    for _ in 0..3 {
        pressure.publish_observed_resident_bytes(between);
        assert!(
            scheduler.reserve_worker_memory().is_err(),
            "admission must not flap between the watermarks"
        );
    }

    pressure.publish_observed_resident_bytes(pressure.low_watermark_bytes());
    drop(
        scheduler
            .reserve_worker_memory()
            .expect("admission is retryable once measured pressure falls to the low watermark"),
    );
}
