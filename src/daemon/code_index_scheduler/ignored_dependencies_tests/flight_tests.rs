use std::sync::{Condvar, Mutex};

use crate::code_index::production::CodeIndexProductionErrorV1;

use super::*;

struct BlockingNthControl {
    checks: AtomicUsize,
    block_on: usize,
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    released: Mutex<bool>,
    release: Condvar,
}

impl BlockingNthControl {
    fn new(block_on: usize) -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
        let (entered, observed) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                block_on,
                entered: Mutex::new(Some(entered)),
                released: Mutex::new(false),
                release: Condvar::new(),
            }),
            observed,
        )
    }

    fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.release.notify_all();
    }
}

impl CodeIndexExecutionControlV1 for BlockingNthControl {
    fn is_cancelled(&self) -> bool {
        let check = self.checks.fetch_add(1, Ordering::AcqRel) + 1;
        if check != self.block_on {
            return false;
        }
        if let Some(entered) = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            entered.send(()).expect("signal blocked admission");
        }
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            released = self
                .release
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct ObservingControl(Mutex<Option<std::sync::mpsc::Sender<()>>>);

impl ObservingControl {
    fn new() -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
        let (entered, observed) = std::sync::mpsc::channel();
        (Arc::new(Self(Mutex::new(Some(entered)))), observed)
    }
}

impl CodeIndexExecutionControlV1 for ObservingControl {
    fn is_cancelled(&self) -> bool {
        if let Some(entered) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            entered.send(()).expect("signal follower flight wait");
        }
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct SchedulerHold {
    release: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SchedulerHold {
    async fn acquire(registry: &CodeIndexSchedulerRegistryV1, project_root: &Path) -> Self {
        let scheduler = registry
            .scheduler_handle(project_root)
            .await
            .expect("mounted scheduler");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held_tx.send(()).expect("signal scheduler hold");
            released.recv().expect("release scheduler hold");
        });
        held_rx.recv().expect("scheduler is held");
        Self {
            release,
            thread: Some(thread),
        }
    }

    fn release(mut self) {
        self.release.send(()).expect("release scheduler");
        self.thread
            .take()
            .expect("scheduler holder")
            .join()
            .expect("scheduler holder joins");
    }
}

fn fixture() -> GitFixture {
    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor(value: PublicWidget) { return value; }
"#,
    );
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface PublicWidget { value: string }\n",
    );
    fixture
}

fn assert_publication_error(error: CodeIndexSchedulerErrorV1) {
    assert!(
        matches!(
            error,
            CodeIndexSchedulerErrorV1::Production(CodeIndexProductionErrorV1::Publication(_))
        ),
        "coalesced failure must preserve the production publication error family"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_flight_owner_wakes_follower_and_allows_a_fresh_owner() {
    let fixture = fixture();
    let store = TempDir::new().expect("store root");
    let registry = Arc::new(mount(fixture.path(), &store, 1).await);
    let baseline = latest(&registry, fixture.path()).await;
    let request = request_for(&baseline, "pkg");
    let hold = SchedulerHold::acquire(&registry, fixture.path()).await;
    let (owner_control, owner_entered) = BlockingNthControl::new(4);

    let owner_registry = Arc::clone(&registry);
    let owner_root = fixture.path().to_path_buf();
    let owner_request = request.clone();
    let owner_control_for_task = Arc::clone(&owner_control);
    let owner = tokio::spawn(async move {
        index_dependency(
            &owner_registry,
            &owner_root,
            owner_request,
            owner_control_for_task,
        )
        .await
    });
    owner_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("owner inserts its flight and starts the blocked build");

    let follower_registry = Arc::clone(&registry);
    let follower_root = fixture.path().to_path_buf();
    let follower_request = request.clone();
    let (follower_control, follower_entered) = ObservingControl::new();
    let follower = tokio::spawn(async move {
        index_dependency(
            &follower_registry,
            &follower_root,
            follower_request,
            follower_control,
        )
        .await
    });
    follower_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("follower joins the existing flight");
    owner.abort();
    owner_control.release();
    assert!(
        owner
            .await
            .expect_err("owner task is aborted")
            .is_cancelled()
    );
    hold.release();

    let follower_error = tokio::time::timeout(Duration::from_secs(5), follower)
        .await
        .expect("orphaned flight cannot strand its follower")
        .expect("follower task joins")
        .expect_err("dropped owner completes its flight as a refusal");
    assert_refusal(
        follower_error,
        CodeIndexIgnoredDependencyRefusalV1::Cancelled,
    );

    index_dependency(&registry, fixture.path(), request, StaticControl::active())
        .await
        .expect("the cleaned flight admits a later fresh owner");
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coalesced_publication_failure_preserves_the_scheduler_error_family() {
    let fixture = fixture();
    let store = TempDir::new().expect("store root");
    let registry = Arc::new(mount(fixture.path(), &store, 1).await);
    let baseline = latest(&registry, fixture.path()).await;
    let request = request_for(&baseline, "pkg");
    let hold = SchedulerHold::acquire(&registry, fixture.path()).await;
    let (owner_control, owner_entered) = BlockingNthControl::new(4);

    let owner_registry = Arc::clone(&registry);
    let owner_root = fixture.path().to_path_buf();
    let owner_request = request.clone();
    let owner_control_for_task = Arc::clone(&owner_control);
    let owner = tokio::spawn(async move {
        index_dependency(
            &owner_registry,
            &owner_root,
            owner_request,
            owner_control_for_task,
        )
        .await
    });
    owner_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("owner inserts its flight and starts the blocked build");
    let follower_registry = Arc::clone(&registry);
    let follower_root = fixture.path().to_path_buf();
    let (follower_control, follower_entered) = ObservingControl::new();
    let follower = tokio::spawn(async move {
        index_dependency(
            &follower_registry,
            &follower_root,
            request,
            follower_control,
        )
        .await
    });
    follower_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("follower joins the existing flight");

    let scoped_store = super::super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let pointer_path = scoped_store.join("active-code-generation-v1.json");
    let pointer_bytes = std::fs::read(&pointer_path).expect("read active pointer");
    std::fs::write(&pointer_path, b"{").expect("corrupt active pointer");
    owner_control.release();
    hold.release();

    let owner_error = tokio::time::timeout(Duration::from_secs(5), owner)
        .await
        .expect("owner observes corrupt publication")
        .expect("owner task joins")
        .expect_err("owner publication fails closed");
    let follower_error = tokio::time::timeout(Duration::from_secs(5), follower)
        .await
        .expect("follower receives the shared failure")
        .expect("follower task joins")
        .expect_err("follower publication fails closed");
    assert_publication_error(owner_error);
    assert_publication_error(follower_error);

    std::fs::write(pointer_path, pointer_bytes).expect("restore active pointer");
    registry.shutdown().await;
}
