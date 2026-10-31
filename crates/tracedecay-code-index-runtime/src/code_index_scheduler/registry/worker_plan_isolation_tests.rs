//! A fixture scheduler meters against its own worker plan, not a plan some
//! other in-process fixture installed.
//!
//! One test process hosts many fixture daemons. The composition root's process
//! plan (installed by host-admission and daemon-bootstrap fixtures) is a single
//! indexing pool and background-CPU FIFO. A scheduler that deferred to it
//! queued its index pass behind unrelated tests' passes, so a publication that
//! needs milliseconds of CPU could miss a caller's latency bound. Production
//! runs one owner per process, so this test pins the fixture equivalent:
//! a mounted scheduler publishes while every unit of the process plan's budget
//! is held by someone else.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index::parallelism::{
    INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1, install_worker_plan, worker_reservation_bytes,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::CodeIndexWorkerSelectionV1;

use super::super::memory_tests::ledger_memory;
use super::super::tests::commit_fixture_checkout;
use super::super::{CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1};
use super::CodeIndexSchedulerRegistryV1;

/// A holder thread reports that its permit is held once it has acquired it.
fn acquire_until_released(
    background_cpu: Arc<tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    acquired: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _permit = background_cpu.acquire();
        acquired.fetch_add(1, Ordering::AcqRel);
        let (released, wake) = &*gate;
        let mut released = released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            released = wake
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fixture_scheduler_publishes_while_the_process_worker_plan_is_fully_held() {
    // The same plan every fixture that installs one requests, so this test and
    // its sibling share one process plan instead of racing two configurations.
    let installed = install_worker_plan(
        CodeIndexWorkerSelectionV1::Automatic {},
        16 * 1024 * 1024 * 1024,
    )
    .expect("process worker plan installs");
    let width = installed.background_cpu.width().get();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let acquired = Arc::new(AtomicUsize::new(0));
    let holders = (0..width)
        .map(|_| {
            acquire_until_released(
                Arc::clone(&installed.background_cpu),
                Arc::clone(&gate),
                Arc::clone(&acquired),
            )
        })
        .collect::<Vec<_>>();
    while acquired.load(Ordering::Acquire) < width {
        tokio::task::yield_now().await;
    }

    let root = TempDir::new().expect("fixture root");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("create source root");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
    commit_fixture_checkout(&project);

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            ProjectId::new("project.worker-plan-isolation").expect("project identity"),
            &project,
            root.path().join("store"),
        )
        .await
        .expect("mount fixture scheduler");
    let mut serving_changes = registry
        .subscribe_serving_generation_changes(&project)
        .await
        .expect("mounted fixture scheduler");
    let published = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if registry.latest_generation_id(&project).await.is_some() {
                return;
            }
            serving_changes
                .changed()
                .await
                .expect("the fixture registry outlives the wait");
        }
    })
    .await;

    {
        let (released, wake) = &*gate;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        wake.notify_all();
    }
    for holder in holders {
        holder.join().expect("the budget holder releases cleanly");
    }

    published.expect(
        "a fixture scheduler's publication must not queue behind another owner's process budget",
    );
    registry.shutdown().await;
}

/// The plan a fixture scheduler runs must be sized against the resident-memory
/// ledger the same pass charges, not against an installed process plan.
///
/// `CodeIndexSchedulerRegistryV1::new` gives a fixture a fixed 6 GiB ledger,
/// while a process plan is sized from the host's detected authority. On a host
/// where that plan's worker scratch reservation reaches the fixture ledger
/// (96 logical CPUs: 48 workers x 128 MiB = 6 GiB), a pass admits its workers
/// and then cannot charge even one source-snapshot byte. The refusal parks the
/// worktree for resident memory, and the ledger can never release room because
/// nothing published: the generation stays unpublished for as long as the
/// caller waits. That is the CI failure this pins, at the plan boundary that
/// made it host-dependent.
#[test]
fn a_fixture_scheduler_sizes_its_plan_from_the_ledger_it_meters() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let installed = install_worker_plan(CodeIndexWorkerSelectionV1::Automatic {}, 16 * GIB)
        .expect("process worker plan installs");
    let process_width = installed.background_cpu.width().get();
    assert_eq!(
        worker_reservation_bytes(process_width),
        u64::try_from(process_width).expect("width fits u64")
            * INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1,
        "the process plan reserves its whole worker scratch width"
    );

    let root = TempDir::new().expect("fixture root");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("create source root");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
    commit_fixture_checkout(&project);

    let ledger_bytes = 6 * GIB;
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.worker-plan-ledger").expect("project identity"),
        &project,
        root.path().join("store"),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open fixture scheduler");
    scheduler.bind_resident_memory(ledger_memory(
        NonZeroU64::new(ledger_bytes).expect("positive ledger"),
    ));
    let entered = scheduler
        .ensure_worker_plan()
        .expect("fixture worker runtime")
        .expect("a fixture scheduler enters its own runtime even when a process plan is installed");
    let fixture_width = tracedecay_code_index::parallelism::indexing_workers();
    drop(entered);

    // The fixture plan is sized from the authority the pass meters, so its
    // worker reservation cannot consume the ledger the source snapshot charges.
    assert_eq!(fixture_width, 2, "fixture owners run at the fixture width");
    assert!(
        worker_reservation_bytes(fixture_width) <= ledger_bytes / 2,
        "the fixture plan must leave the snapshot its non-worker headroom: \
         reservation={} ledger={ledger_bytes}",
        worker_reservation_bytes(fixture_width)
    );
}
