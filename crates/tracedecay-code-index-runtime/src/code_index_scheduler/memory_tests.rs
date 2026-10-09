use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tracedecay_code_index::parallelism::CodeIndexWorkerRuntimeV1;
use tracedecay_domain::{ProjectId, configuration::CodeIndexWorkerSelectionV1};
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1, ProcessResidentSampleV1,
    ResidentMemoryComponentIdV1, ResidentMemoryPressureV1,
};

use crate::code_index::production::{CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1};

use super::publication_store::ActiveGenerationDecodeChargeV1;
use super::tests::OwnerSignals;
use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
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

fn expected_worker_reservation_on(runtime: &CodeIndexWorkerRuntimeV1, remaining_bytes: u64) -> u64 {
    let planned_workers = usize::from(runtime.status().effective_workers);
    let affordable = tracedecay_code_index::parallelism::memory_safe_worker_count(remaining_bytes);
    tracedecay_code_index::parallelism::worker_reservation_bytes(
        planned_workers.min(affordable).max(1),
    )
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
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
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
    let authority = Arc::new(ProcessResidentMemoryV1::new(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    let reservation = scheduler
        .reserve_worker_memory()
        .expect("reserve worker memory");
    assert_eq!(
        authority.snapshot().used_bytes,
        expected_worker_reservation_on(&runtime, DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get())
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
    let runtime = bind_automatic_worker_runtime(&scheduler, 128, 128 * 1024 * 1024 * 1024);
    // This case isolates the ledger's worker/snapshot split. Live process
    // pressure can narrow the slab and is exercised with explicit samples below.
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        Arc::new(|| None),
    ));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        pressure,
    ));
    scheduler.bind_resident_memory(Arc::clone(&authority));

    let _worker = scheduler
        .reserve_worker_memory()
        .expect("6 GiB authority admits a memory-safe worker slab");
    let used = authority.snapshot().used_bytes;
    let limit = DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get();
    assert_eq!(used, expected_worker_reservation_on(&runtime, limit));
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
                    cgroup_committed_bytes: None,
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

/// Live heap no resident owner charges still fills the process, so the ledger
/// claims room for the whole worker plan while measured RSS does not. A
/// refresh after the cold index sizes its worker slab to the measured headroom
/// and publishes, instead of asking for the full plan and being refused on
/// every retry.
#[test]
fn a_refresh_after_cold_index_sizes_its_worker_slab_to_measured_headroom() {
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
    let planned = u64::from(runtime.status().effective_workers);
    assert!(
        planned >= 2,
        "the default plan must span more than one worker for the slab to narrow"
    );
    // Measured headroom fits half the planned slab. The limit keeps the
    // observed bytes under the high watermark and leaves the ledger room for
    // the whole plan.
    let headroom =
        (planned / 2) * tracedecay_code_index::parallelism::INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1;
    let limit = NonZeroU64::new(8 * headroom).expect("limit");
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
                cgroup_committed_bytes: None,
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

    observed.store(limit.get() - headroom, Ordering::Release);
    pressure.sample_and_publish();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\npub fn refreshed_generation() -> u32 { 2 }\n",
    )
    .expect("write source");
    git(project.path(), &["commit", "-q", "-am", "refresh"]);
    let refreshed = scheduler
        .reconcile_now()
        .expect("the refresh is admitted within measured headroom");
    assert!(matches!(
        refreshed,
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let latest = scheduler.latest_complete().expect("refreshed generation");
    let mut symbols = latest
        .generation
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
            "src/lib.rs::retained_generation"
        ]
    );
}

/// A refresh decodes its active parent generation and builds under one worker
/// slab. Here another holder leaves the parent's measured decode plus 64 MiB
/// below the admission watermark: room for the parent, not for the parent
/// beside a 128 MiB worker. The refresh still publishes, with its slab
/// planned against what the resident parent leaves.
#[test]
fn a_refresh_decodes_its_parent_before_planning_its_worker_slab() {
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
    bind_automatic_worker_runtime(
        &scheduler,
        8,
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
    );
    let limit = NonZeroU64::new(4 * GIB).expect("limit");
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(|| {
            Some(ProcessResidentSampleV1 {
                resident_bytes: 0,
                unreclaimable_bytes: 0,
                swapped_bytes: 0,
                cgroup_committed_bytes: None,
            })
        }),
    ));
    let high_watermark = pressure.high_watermark_bytes();
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure));
    scheduler.bind_resident_memory(Arc::clone(&authority));
    assert!(matches!(
        scheduler.reconcile_now().expect("cold index publishes"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    scheduler
        .publication
        .release_decoded_active_after_seal()
        .expect("release the sealed decode");
    let ActiveGenerationDecodeChargeV1::Measured {
        bytes: decode_bytes,
        ..
    } = scheduler
        .publication
        .active_generation_charge()
        .expect("decode charge")
    else {
        panic!("the cold index measured the generation it published");
    };
    let holder_bytes = high_watermark - authority.snapshot().used_bytes - decode_bytes - 64 * MIB;
    let _holder = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-text-build").expect("component"),
            NonZeroU64::new(holder_bytes).expect("holder bytes"),
        )
        .expect("the holder fits the ledger");

    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\npub fn refreshed_generation() -> u32 { 2 }\n",
    )
    .expect("write source");
    git(project.path(), &["commit", "-q", "-am", "refresh"]);
    let refreshed = scheduler
        .reconcile_now()
        .expect("the refresh decodes its parent and builds");
    assert!(matches!(
        refreshed,
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let latest = scheduler.latest_complete().expect("refreshed generation");
    let mut symbols = latest
        .generation
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
            "src/lib.rs::retained_generation"
        ]
    );
}

/// The seal hands the decoded generation back so the text build has the
/// memory; decoding it again must fit the process budget. Here another holder
/// keeps all but a sliver below the admission watermark, so the decode waits
/// (typed refusal, no decode, the ledger untouched) instead of materializing
/// the generation into the kill line, and runs once that holder lets go.
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
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
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

    let holder = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-text-build").expect("component"),
            NonZeroU64::new(high_watermark - 1024).expect("holder bytes"),
        )
        .expect("the holder fits the ledger");
    let used_while_held = authority.snapshot().used_bytes;

    assert!(
        scheduler.latest_complete().is_none(),
        "a decode that does not fit must not materialize the generation"
    );
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
        .latest_complete()
        .expect("the decode runs once the memory is given back");
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
    assert_eq!(
        authority.snapshot().used_bytes,
        0,
        "the decode's charge is released once it completes"
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
        cgroup_committed_bytes: None,
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
        cgroup_committed_bytes: None,
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
        cgroup_committed_bytes: None,
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

/// A linked-worktree reconcile is admitted while RSS is still under the
/// watermark and then keeps allocating. The next checkpoint has to read the
/// process again: once the sample crosses the watermark the pass stops as a
/// capacity refusal and the worktree stays unpublished. Treating that stop as
/// a superseded epoch would start another capture immediately.
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
    let limit = NonZeroU64::new(16 * 1024 * 1024 * 1024).expect("limit");
    let sampled = Arc::new(AtomicU64::new(0));
    let ceiling = limit.get();
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(move || {
            let read = sampled.fetch_add(1, Ordering::AcqRel);
            let unreclaimable_bytes = if read == 0 { 1 } else { ceiling };
            Some(ProcessResidentSampleV1 {
                resident_bytes: unreclaimable_bytes,
                unreclaimable_bytes,
                swapped_bytes: 0,
                cgroup_committed_bytes: None,
            })
        }),
    ));
    scheduler.bind_resident_memory(Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit, pressure,
    )));

    let error = scheduler
        .reconcile_now()
        .expect_err("a reconcile whose live RSS crosses the watermark must not publish");
    assert!(
        error.is_transient_capacity_failure(),
        "the watermark stop must stay retryable after RSS falls: {error}"
    );
    assert!(
        matches!(
            error,
            CodeIndexSchedulerErrorV1::Production(CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
            ))
        ),
        "the stop must be a resident-memory refusal, not a superseded retry: {error}"
    );
    assert!(
        matches!(scheduler.publication.load_active_shared(), Ok(None)),
        "the refused reconcile must leave the worktree unpublished"
    );
}
