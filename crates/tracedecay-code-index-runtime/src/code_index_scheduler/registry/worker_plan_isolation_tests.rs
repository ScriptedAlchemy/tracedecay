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

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index::parallelism::{
    INDEX_WORKER_RESIDENT_BUDGET_BYTES_V1, install_worker_plan, worker_reservation_bytes,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::CodeIndexWorkerSelectionV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;

use super::super::memory_tests::ledger_memory;
use super::super::tests::commit_fixture_checkout;
use super::super::{CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1};
use super::CodeIndexSchedulerRegistryV1;

#[derive(Default)]
struct CpuPermitHolders {
    gate: Arc<(Mutex<bool>, Condvar)>,
    holders: Vec<JoinHandle<()>>,
}

impl CpuPermitHolders {
    fn hold_all(background_cpu: &Arc<ProcessBackgroundCpuV1>) -> Self {
        let mut holders = Self::default();
        let mut ready = Vec::with_capacity(background_cpu.width().get());
        for _ in 0..background_cpu.width().get() {
            ready.push(
                holders
                    .spawn(Arc::clone(background_cpu))
                    .expect("spawn CPU permit holder"),
            );
        }
        for acquired in ready {
            acquired
                .recv_timeout(Duration::from_secs(5))
                .expect("CPU permit holder acquires its unit");
        }
        holders
    }

    fn spawn(
        &mut self,
        background_cpu: Arc<ProcessBackgroundCpuV1>,
    ) -> std::io::Result<mpsc::Receiver<()>> {
        // Allocate before spawning so every created holder is immediately owned.
        self.holders.reserve(1);
        let gate = Arc::clone(&self.gate);
        let (ready, acquired) = mpsc::channel();
        let holder = thread::Builder::new().spawn(move || {
            let (released, wake) = &*gate;
            let Some(_permit) = background_cpu.acquire_cancellable(|| {
                *released
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }) else {
                return;
            };
            if ready.send(()).is_err() {
                return;
            }
            let mut released = released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = wake
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        })?;
        self.holders.push(holder);
        Ok(acquired)
    }

    fn release_and_join(&mut self) -> bool {
        let (released, wake) = &*self.gate;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        wake.notify_all();
        let mut clean = true;
        for holder in self.holders.drain(..) {
            clean &= holder.join().is_ok();
        }
        clean
    }
}

impl Drop for CpuPermitHolders {
    fn drop(&mut self) {
        // Join every holder even if one panics; never panic during unwinding.
        self.release_and_join();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fixture_scheduler_publishes_while_the_process_worker_plan_is_fully_held() {
    let root = TempDir::new().expect("fixture root");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("create source root");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
    commit_fixture_checkout(&project);

    // The same plan every fixture that installs one requests, so this test and
    // its sibling share one process plan instead of racing two configurations.
    let installed = install_worker_plan(
        CodeIndexWorkerSelectionV1::Automatic {},
        16 * 1024 * 1024 * 1024,
    )
    .expect("process worker plan installs");
    let mut holders = CpuPermitHolders::hold_all(&installed.background_cpu);

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

    assert!(
        holders.release_and_join(),
        "CPU permit holders join cleanly"
    );

    published.expect(
        "a fixture scheduler's publication must not queue behind another owner's process budget",
    );
    registry.shutdown().await;
}

#[test]
fn cpu_permit_holders_release_and_join_on_unwind() {
    let background_cpu = Arc::new(ProcessBackgroundCpuV1::new(
        NonZeroUsize::new(2).expect("positive width"),
    ));
    let unwound = std::panic::catch_unwind(|| {
        let holders = CpuPermitHolders::hold_all(&background_cpu);
        assert!(background_cpu.try_acquire().is_none(), "every unit is held");
        // Also exercise poisoned-gate recovery while Drop is already unwinding.
        let _locked = holders.gate.0.lock().expect("unpoisoned gate");
        panic!("deliberate holder unwind");
    });
    let failure = unwound.expect_err("the deliberate unwind occurs after acquisition");
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"deliberate holder unwind")
    );
    assert_eq!(Arc::strong_count(&background_cpu), 1, "all holders exited");
    assert_eq!(background_cpu.active_units(), 0);
    let admitted: Option<Vec<_>> = (0..background_cpu.width().get())
        .map(|_| background_cpu.try_acquire())
        .collect();
    assert!(
        admitted.is_some(),
        "the entire CPU budget is immediately reusable"
    );
}

#[tokio::test]
async fn cpu_permit_holders_cancel_queued_acquisition_on_drop() {
    let background_cpu = Arc::new(ProcessBackgroundCpuV1::new(NonZeroUsize::MIN));
    let occupied = background_cpu.acquire();
    let mut holders = CpuPermitHolders::default();
    let acquired = holders
        .spawn(Arc::clone(&background_cpu))
        .expect("spawn queued holder");
    let queued = tokio::time::timeout(Duration::from_secs(5), async {
        while background_cpu.waiting_work_units() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(holders);

    queued.expect("holder queues behind the occupied unit");
    assert_eq!(acquired.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(background_cpu.waiting_work_units(), 0);
    assert_eq!(
        Arc::strong_count(&background_cpu),
        2,
        "queued holder exited"
    );
    drop(occupied);
    assert!(background_cpu.try_acquire().is_some());
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
