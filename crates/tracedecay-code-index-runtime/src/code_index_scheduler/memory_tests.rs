use std::fs;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tempfile::TempDir;
use tracedecay_code_index::parallelism::CodeIndexWorkerRuntimeV1;
use tracedecay_domain::{ProjectId, configuration::CodeIndexWorkerSelectionV1};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1, ProcessResidentSampleV1,
    ResidentMemoryComponentIdV1, ResidentMemoryPressureV1,
};

use tracedecay_code_index::production::CodeIndexPublicationStoreErrorV1;

use super::publication_store::ActiveGenerationDecodeChargeV1;
use super::tests::OwnerSignals;
use super::tests::git;
use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
};

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

/// The automatic plan for `logical_cpus` against `available_memory_bytes`,
/// bound to `scheduler` so its reservations use exactly this plan.
fn bind_automatic_worker_runtime(
    scheduler: &CodeIndexWorktreeSchedulerV1,
    logical_cpus: usize,
    available_memory_bytes: u64,
) -> CodeIndexWorkerRuntimeV1 {
    let runtime = CodeIndexWorkerRuntimeV1::build(
        CodeIndexWorkerSelectionV1::Automatic {},
        logical_cpus,
        available_memory_bytes,
    )
    .expect("build automatic worker runtime");
    scheduler.bind_worker_runtime(runtime.clone());
    runtime
}

fn worker_reservation_bytes(runtime: &CodeIndexWorkerRuntimeV1) -> u64 {
    tracedecay_code_index::parallelism::worker_reservation_bytes(usize::from(
        runtime.status().effective_workers,
    ))
}

/// Ledger scenarios own their measured input independently of sibling tests.
pub(super) fn ledger_memory(limit: NonZeroU64) -> Arc<ProcessResidentMemoryV1> {
    Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(|| None),
        )),
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
    let authority = ledger_memory(NonZeroU64::new(1024 * 1024).expect("source memory limit"));
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

/// Captured source lives for the build that reads it. Once the generation is
/// sealed nothing reads those bytes (the sealed artifacts serve every later
/// read), so neither a publishing reconcile nor a no-build reconcile over the
/// unchanged checkout leaves source resident or charged: holding them had
/// kept about 16 KB of anonymous heap per indexed file after every build.
#[test]
fn reconcile_leaves_no_captured_source_charged_after_the_build() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-source-build-copy").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let authority = ledger_memory(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1);
    scheduler.bind_resident_memory(Arc::clone(&authority));
    let captured = scheduler
        .capture_authoritative_snapshot(None)
        .expect("capture source snapshot");
    assert_eq!(
        authority.snapshot().used_bytes,
        u64::try_from("pub fn retained_generation() -> u32 { 1 }\n".len()).expect("fits"),
        "captured source is charged while a build holds it"
    );
    drop(captured);

    assert!(matches!(
        scheduler.reconcile_now().expect("publish generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    assert_eq!(authority.snapshot().used_bytes, 0);
    assert_eq!(
        scheduler
            .latest_complete()
            .expect("published generation")
            .generation
            .symbols()
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["src/lib.rs::retained_generation"]
    );

    assert!(matches!(
        scheduler
            .reconcile_now()
            .expect("reconcile unchanged source"),
        CodeIndexReconcileOutcomeV1::Noop(_)
    ));
    assert_eq!(authority.snapshot().used_bytes, 0);
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
    let authority = ledger_memory(NonZeroU64::MIN);
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
    let exact = first.exact().expect("exact chunks");
    let files = &first.generation().snapshot().files;
    for admitted in exact.iter() {
        let path = files
            .iter()
            .find(|file| file.file_occurrence_id == admitted.chunk().anchor.file_occurrence_id)
            .map(|file| file.logical_path.as_str());
        assert_eq!(path, Some("src/lib.rs"));
    }
    assert!(
        exact.iter().any(|admitted| admitted
            .chunk()
            .sanitized_text
            .as_str()
            .contains("pub fn retained_generation() -> u32 { 1 }")),
        "exact chunks must carry the committed fixture source"
    );
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

/// A fixture scheduler's own runtime has the fixed fixture width on any host,
/// so concurrent fixture owners never size their pools from the machine.
#[test]
fn fixture_scheduler_builds_on_the_fixture_width_not_the_host() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-fixture-width").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let _entered = scheduler
        .ensure_worker_plan()
        .expect("fixture worker runtime")
        .expect("a fixture scheduler enters its own runtime");
    let pool_threads =
        tracedecay_code_index::parallelism::install(rayon::current_num_threads).expect("pool");
    assert_eq!(
        (
            tracedecay_code_index::parallelism::indexing_workers(),
            pool_threads
        ),
        (2, 2)
    );
}

#[test]
fn worker_memory_reservation_is_charged_and_released_by_raii() {
    const MIB: u64 = 1024 * 1024;
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
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        2,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    assert_eq!(runtime.status().effective_workers, 2);
    let authority = ledger_memory(NonZeroU64::new(400 * MIB).expect("limit"));
    let standing = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-standing-memory").unwrap(),
            NonZeroU64::new(100 * MIB).unwrap(),
        )
        .expect("standing charge fits");
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(
        tracedecay_code_index::parallelism::memory_safe_worker_count(300 * MIB).max(1) == 1,
        "the old admission arithmetic would charge only one worker"
    );
    let _entered = runtime.enter();
    assert_eq!(
        tracedecay_code_index::parallelism::install(rayon::current_num_threads)
            .expect("installed executor width"),
        2
    );
    let reservation = scheduler
        .reserve_worker_memory()
        .expect("both installed workers fit beside the standing charge");
    assert_eq!(reservation.reserved_bytes(), 2 * 128 * MIB);
    assert_eq!(authority.snapshot().used_bytes, 100 * MIB + 2 * 128 * MIB);
    assert_eq!(runtime.status().effective_workers, 2);
    drop(reservation);
    assert_eq!(authority.snapshot().used_bytes, standing.reserved_bytes());
}

/// Planning against the default authority leaves snapshot headroom while
/// admission charges the complete installed executor width.
#[test]
fn default_authority_worker_reserve_leaves_typed_snapshot_headroom() {
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
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        128,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    // This case isolates the installed worker/snapshot split. Measured process
    // pressure is exercised with explicit samples below.
    let authority = ledger_memory(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1);
    scheduler.bind_resident_memory(Arc::clone(&authority));

    let _worker = scheduler
        .reserve_worker_memory()
        .expect("6 GiB authority admits a memory-safe worker slab");
    let used = authority.snapshot().used_bytes;
    let limit = DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get();
    assert_eq!(used, worker_reservation_bytes(&runtime));
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
    bind_automatic_worker_runtime(
        &scheduler,
        2,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    let authority = ledger_memory(
        NonZeroU64::new(
            tracedecay_code_index::parallelism::INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1 - 1,
        )
        .expect("positive test limit"),
    );
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
        )
        .await
        .expect("mount worktree");

    // Mount restores whatever the store retained and hands the first build to
    // the background worker, so an empty store reports no retained bytes until
    // that reconcile lands. Settle on the post-reconcile state instead of
    // racing it; the assertions below are unchanged and must all hold at once.
    let mut signals = OwnerSignals::subscribe(&registry, project.path()).await;
    let stats = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let stats = registry.memory_stats().await;
            if stats.reconciling_worktrees == 0 && stats.retained_generation_encoded_bytes > 0 {
                break stats;
            }
            signals.changed().await;
        }
    })
    .await
    .expect("the mount-time reconcile publishes a retained generation");

    assert_eq!(stats.mounted_worktrees, 1);
    assert_eq!(stats.reconciling_worktrees, 0);
    assert!(stats.retained_generation_encoded_bytes > 0);
    registry.shutdown().await;
}

fn assert_observed_worker_memory_refusal(
    failure: &CodeIndexSchedulerErrorV1,
    observed_bytes: u64,
    configured_limit: u64,
) {
    let CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(admission) = failure else {
        panic!("expected a worker resident-memory admission failure, got {failure:?}");
    };
    assert!(
        admission.is_observed_over_budget(),
        "the refusal must name measured pressure, not a full reservation ledger"
    );
    let rendered = failure.to_string();
    assert!(
        rendered.contains(&observed_bytes.to_string()),
        "the refusal names observed bytes: {rendered}"
    );
    assert!(
        rendered.contains(&configured_limit.to_string()),
        "the refusal names configured bytes: {rendered}"
    );
    assert!(
        failure.is_transient_capacity_failure(),
        "an over-budget refusal is retryable as pressure falls"
    );
}

/// Measured RSS, not the reservation ledger, decides worker admission once a
/// sample says the process is over budget, and the refusal names the observed
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
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        2,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    let limit = NonZeroU64::new(worker_reservation_bytes(&runtime).saturating_mul(4))
        .expect("positive test limit");
    let measured = Arc::new(Mutex::new(None));
    let sampled = Arc::clone(&measured);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(move || {
            sampled
                .lock()
                .expect("measurement")
                .map(|unreclaimable_bytes| ProcessResidentSampleV1 {
                    resident_bytes: unreclaimable_bytes,
                    unreclaimable_bytes,
                    swapped_bytes: 0,
                    cgroup_headroom_bytes: None,
                })
        }),
    ));
    let measure = |bytes: u64| *measured.lock().expect("measurement") = Some(bytes);
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

    let over_high = pressure.high_watermark_bytes() + 1;
    measure(over_high);
    let failure = scheduler
        .reserve_worker_memory()
        .expect_err("measured RSS over the high watermark refuses new worker admission");
    assert_observed_worker_memory_refusal(&failure, over_high, limit.get());

    // Hysteresis: between the watermarks the refusal stands rather than flapping.
    let between = u64::midpoint(
        pressure.low_watermark_bytes(),
        pressure.high_watermark_bytes(),
    );
    for _ in 0..3 {
        measure(between);
        let failure = scheduler
            .reserve_worker_memory()
            .expect_err("admission must not flap between the watermarks");
        assert_observed_worker_memory_refusal(&failure, between, limit.get());
    }

    measure(pressure.low_watermark_bytes());
    drop(
        scheduler
            .reserve_worker_memory()
            .expect("admission is retryable once measured pressure falls to the low watermark"),
    );
}

/// Measured process headroom must fit the complete installed executor.
/// External memory release admits the same width without changing its pool.
#[test]
fn a_refresh_with_installed_workers_waits_for_enough_measured_headroom() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-refresh-headroom").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        8,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    assert_eq!(runtime.status().effective_workers, 8);
    let worker_bytes = worker_reservation_bytes(&runtime);
    assert_eq!(worker_bytes, GIB);
    let limit = NonZeroU64::new(4 * GIB).expect("limit");
    let observed = Arc::new(AtomicU64::new(0));
    let sampled = Arc::clone(&observed);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(move || {
            let unreclaimable_bytes = sampled.load(Ordering::Acquire);
            Some(ProcessResidentSampleV1 {
                resident_bytes: unreclaimable_bytes,
                unreclaimable_bytes,
                swapped_bytes: 0,
                cgroup_headroom_bytes: None,
            })
        }),
    ));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::clone(&pressure),
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(matches!(
        scheduler.reconcile_now().expect("cold index publishes"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let predecessor = scheduler.active_publication_generation_id_for_test();
    assert!(predecessor.is_some());
    let decodes_before = scheduler.sealed_decode_count();
    let used_before = authority.snapshot().used_bytes;

    let refused_observed = 3 * GIB + GIB / 2;
    observed.store(refused_observed, Ordering::Release);
    pressure.sample_and_publish();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\npub fn refreshed_generation() -> u32 { 2 }\n",
    )
    .expect("write source");
    git(project.path(), &["commit", "-q", "-am", "refresh"]);
    let failure = scheduler
        .reconcile_now()
        .expect_err("the installed executor does not fit");
    assert_observed_worker_memory_refusal(&failure, refused_observed, limit.get());
    let CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(admission) = &failure else {
        panic!("expected installed-worker admission refusal");
    };
    assert_eq!(admission.requested_bytes(), worker_bytes);
    assert_eq!(
        scheduler.active_publication_generation_id_for_test(),
        predecessor
    );
    assert_eq!(scheduler.sealed_decode_count(), decodes_before);
    assert_eq!(authority.snapshot().used_bytes, used_before);

    observed.store(2 * GIB + GIB / 2, Ordering::Release);
    pressure.sample_and_publish();
    assert!(matches!(
        scheduler
            .reconcile_now()
            .expect("external release admits the installed executor"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    assert_eq!(runtime.status().effective_workers, 8);
    assert_ne!(
        scheduler.active_publication_generation_id_for_test(),
        predecessor
    );
    let latest = scheduler
        .publication
        .load_active_shared()
        .expect("explicit decode admission")
        .expect("refreshed generation");
    let mut symbols = latest
        .symbols()
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    assert_eq!(
        symbols,
        [
            "src/lib.rs::refreshed_generation",
            "src/lib.rs::retained_generation",
        ]
    );
}

/// A graph-disabled retained-text refresh builds from sealed metadata without
/// materializing its parent or reserving the full executor's scratch again.
#[test]
fn a_retained_text_refresh_does_not_decode_its_parent_under_memory_pressure() {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-refresh-parent-decode").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        8,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    let limit = NonZeroU64::new(4 * GIB).expect("limit");
    let authority = ledger_memory(limit);
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(matches!(
        scheduler.reconcile_now().expect("cold index publishes"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let metadata = scheduler
        .publication
        .load_active_manifest()
        .expect("read active sealed metadata")
        .expect("sealed predecessor")
        .metadata;
    let predecessor = scheduler.active_publication_generation_id_for_test();
    let decodes_before = scheduler.sealed_decode_count();
    assert_eq!(
        decodes_before, 0,
        "publishing sealed text does not decode its parent"
    );
    assert_eq!(
        scheduler
            .publication
            .active_generation_charge()
            .expect("decode state"),
        ActiveGenerationDecodeChargeV1::Unmeasured
    );
    let holder = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-standing-memory").expect("component"),
            NonZeroU64::new(limit.get() - authority.snapshot().used_bytes - 64 * MIB)
                .expect("standing bytes"),
        )
        .expect("standing memory leaves the retained refresh room");
    assert!(worker_reservation_bytes(&runtime) > 64 * MIB);

    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\npub fn refreshed_generation() -> u32 { 2 }\n",
    )
    .expect("write source");
    git(project.path(), &["commit", "-q", "-am", "refresh"]);
    assert!(matches!(
        scheduler
            .reconcile_retained_text_generation_with(&metadata, true)
            .expect("retained-text refresh builds within its real charge"),
        Some(CodeIndexReconcileOutcomeV1::Published(_))
    ));
    assert_ne!(
        scheduler.active_publication_generation_id_for_test(),
        predecessor
    );
    assert_eq!(
        scheduler.sealed_decode_count(),
        decodes_before,
        "the retained-text successor must not decode the predecessor"
    );
    drop(holder);
    let latest = scheduler
        .publication
        .load_active_shared()
        .expect("explicit decode admission")
        .expect("refreshed generation");
    assert_eq!(scheduler.sealed_decode_count(), decodes_before + 1);
    let mut symbols = latest
        .symbols()
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    assert_eq!(
        symbols,
        [
            "src/lib.rs::refreshed_generation",
            "src/lib.rs::retained_generation",
        ]
    );
}

/// A consumer explicitly decodes and releases a generation. Its next decode
/// uses that measured cost and must wait until the full cost fits again.
#[test]
fn a_released_generation_decodes_again_only_once_its_bytes_fit_the_budget() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-decode-admission").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let limit = NonZeroU64::new(16 * GIB).expect("limit");
    let authority = ledger_memory(limit);
    let high_watermark = authority.admission_watermark_bytes();
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(matches!(
        scheduler.reconcile_now().expect("publish generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let measured = scheduler
        .publication
        .load_active_shared()
        .expect("first explicit decode admission")
        .expect("published generation");
    drop(measured);
    scheduler
        .publication
        .release_decoded_active_after_seal()
        .expect("release the explicitly decoded generation");
    let ActiveGenerationDecodeChargeV1::Measured {
        bytes: decode_bytes,
        ..
    } = scheduler
        .publication
        .active_generation_charge()
        .expect("measured decode charge")
    else {
        panic!("an explicit decode must measure the next decode's charge");
    };
    assert!(decode_bytes > 0);
    let decodes_before = scheduler.sealed_decode_count();
    let used_before = authority.snapshot().used_bytes;
    let holder = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-standing-memory").expect("component"),
            NonZeroU64::new(high_watermark - used_before - (decode_bytes - 1))
                .expect("holder bytes"),
        )
        .expect("standing memory leaves one byte less than the measured decode needs");
    let used_while_held = authority.snapshot().used_bytes;
    assert!(matches!(
        scheduler.publication.load_active_shared(),
        Err(CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_))
    ));
    assert_eq!(
        scheduler.sealed_decode_count(),
        decodes_before,
        "nothing was decoded"
    );
    assert_eq!(authority.snapshot().used_bytes, used_while_held);

    drop(holder);
    let decoded = scheduler
        .publication
        .load_active_shared()
        .expect("decode admission after external release")
        .expect("published generation");
    assert_eq!(scheduler.sealed_decode_count(), decodes_before + 1);
    assert_eq!(
        decoded
            .symbols()
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["src/lib.rs::retained_generation"]
    );
    let repeated = scheduler
        .publication
        .load_active_shared()
        .expect("repeat explicit decode admission")
        .expect("published generation");
    assert!(Arc::ptr_eq(&decoded, &repeated));
    assert_eq!(scheduler.sealed_decode_count(), decodes_before + 1);
    assert_eq!(
        authority.snapshot().used_bytes,
        used_before,
        "the transient decode reservation releases after the decode completes"
    );
}

/// Admission counts only what the kernel cannot take back without swapping.
/// Here clean file pages (the mapped sealed container) fill the resident set
/// to one byte under the admission watermark while anonymous memory leaves
/// room, so the decode runs. Once the unreclaimable bytes themselves fill the
/// headroom, the same decode is refused and nothing is decoded.
#[test]
fn a_decode_is_admitted_against_unreclaimable_bytes_not_clean_file_pages() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-decode-unreclaimable").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let limit = NonZeroU64::new(16 * GIB).expect("limit");
    let view = Arc::new(Mutex::new(ProcessResidentSampleV1 {
        resident_bytes: GIB,
        unreclaimable_bytes: GIB,
        swapped_bytes: 0,
        cgroup_headroom_bytes: None,
    }));
    let sampled = Arc::clone(&view);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(move || Some(*sampled.lock().expect("view"))),
    ));
    let high_watermark = pressure.high_watermark_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(matches!(
        scheduler.reconcile_now().expect("publish generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    scheduler
        .publication
        .release_decoded_active_after_seal()
        .expect("release the sealed decode");
    let decodes_before = scheduler.sealed_decode_count();

    *view.lock().expect("view") = ProcessResidentSampleV1 {
        resident_bytes: high_watermark - 1,
        unreclaimable_bytes: 2 * GIB,
        swapped_bytes: 0,
        cgroup_headroom_bytes: None,
    };
    let decoded = scheduler
        .latest_complete()
        .expect("clean file pages do not refuse a decode that fits in anonymous headroom");
    assert_eq!(scheduler.sealed_decode_count(), decodes_before + 1);
    assert_eq!(
        decoded
            .generation
            .symbols()
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["src/lib.rs::retained_generation"]
    );
    drop(decoded);
    scheduler
        .publication
        .release_decoded_active_after_seal()
        .expect("release the decode again");

    *view.lock().expect("view") = ProcessResidentSampleV1 {
        resident_bytes: high_watermark - 1,
        unreclaimable_bytes: high_watermark - 1,
        swapped_bytes: 0,
        cgroup_headroom_bytes: None,
    };
    assert!(matches!(
        scheduler.publication.load_active_shared(),
        Err(CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_))
    ));
    assert_eq!(
        scheduler.sealed_decode_count(),
        decodes_before + 1,
        "a decode refused on unreclaimable bytes decodes nothing"
    );
}

/// Live pressure crosses the watermark after real construction starts. The
/// refusal retains the full witnessed reconstruction demand through unwind.
#[test]
fn reconcile_stops_when_resident_memory_crosses_the_watermark() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-watermark-stop").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    let runtime = bind_automatic_worker_runtime(
        &scheduler,
        2,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    let limit = NonZeroU64::new(16 * 1024 * 1024 * 1024).expect("limit");
    let ledger = Arc::new(OnceLock::<Weak<ProcessResidentMemoryV1>>::new());
    let external_burst = Arc::new(AtomicBool::new(false));
    let construction_peak = Arc::new(AtomicU64::new(0));
    let construction = ResidentMemoryComponentIdV1::new("code_index.construction.v1").unwrap();
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(limit, {
        let ledger = Arc::clone(&ledger);
        let external_burst = Arc::clone(&external_burst);
        let construction_peak = Arc::clone(&construction_peak);
        Arc::new(move || {
            if let Some(memory) = ledger.get().and_then(Weak::upgrade) {
                let bytes = memory.snapshot().process_shared_charge_for(construction);
                construction_peak.fetch_max(bytes, Ordering::AcqRel);
                if bytes > 0 {
                    external_burst.store(true, Ordering::Release);
                }
            }
            // The external occupancy persists after the build releases itself.
            let bytes = if external_burst.load(Ordering::Acquire) {
                limit.get()
            } else {
                1
            };
            Some(ProcessResidentSampleV1 {
                resident_bytes: bytes,
                unreclaimable_bytes: bytes,
                swapped_bytes: 0,
                cgroup_headroom_bytes: None,
            })
        })
    }));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure));
    ledger.set(Arc::downgrade(&authority)).unwrap();
    scheduler.bind_resident_memory(Arc::clone(&authority));
    let error = scheduler
        .reconcile_now()
        .expect_err("a reconcile whose live pressure crosses the watermark must not publish");
    assert!(external_burst.load(Ordering::Acquire));
    assert!(construction_peak.load(Ordering::Acquire) > 0);
    assert!(error.is_transient_capacity_failure());
    assert!(error.is_resident_memory_refusal());
    let CodeIndexSchedulerErrorV1::ReconstructionMemoryRefused { demand, .. } = &error else {
        panic!("expected a witnessed reconstruction refusal, got {error:?}");
    };
    assert!(demand.required_bytes() >= u128::from(worker_reservation_bytes(&runtime)));
    assert!(!demand.can_retry());
    assert_eq!(
        authority.snapshot().used_bytes,
        0,
        "discarded build charges unwind"
    );
    assert!(
        matches!(scheduler.publication.load_active_shared(), Ok(None)),
        "the refused reconcile leaves the worktree unpublished"
    );
}
