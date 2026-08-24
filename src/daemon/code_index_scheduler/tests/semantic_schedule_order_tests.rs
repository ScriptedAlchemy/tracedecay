use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc::{Receiver, Sender, channel},
};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::CodeGenerationId;
use tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1;

use super::super::CodeIndexGenerationPublishedV1;
use super::{
    CodeIndexSchedulerRegistryV1, GitFixture, test_project_id, wait_for_generation_change,
    wait_for_initial_generation,
};

async fn published_generation_for_root(
    publications: &mut tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1>,
    project_root: &Path,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root.as_path() == project_root {
                break event.generation_id;
            }
        }
    })
    .await
    .expect("matching project generation publication")
}

#[tokio::test]
async fn remount_replaces_semantic_hook_and_replays_latest_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let first_calls = Arc::new(AtomicUsize::new(0));
    let first_hook = {
        let calls = Arc::clone(&first_calls);
        Arc::new(move |_: &CodeIndexPublishedGenerationV1| {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(first_hook),
            )
            .await
            .expect("mount scheduler")
    );
    let first_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while first_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial semantic schedule");

    let second_calls = Arc::new(AtomicUsize::new(0));
    let second_hook = {
        let calls = Arc::clone(&second_calls);
        Arc::new(move |_: &CodeIndexPublishedGenerationV1| {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(second_hook),
            )
            .await
            .expect("remount scheduler")
    );
    assert_eq!(
        second_calls.load(Ordering::SeqCst),
        1,
        "remount must replay the already-published generation"
    );
    let retired_calls = first_calls.load(Ordering::SeqCst);

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second_generation =
        wait_for_generation_change(&registry, fixture.path(), &first_generation).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while second_calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement hook scheduled edited generation");
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        retired_calls,
        "retired hook must not receive later generations"
    );
    let disabled_calls = second_calls.load(Ordering::SeqCst);
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("remount without semantics")
    );
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let _ = wait_for_generation_change(&registry, fixture.path(), &second_generation).await;
    assert_eq!(
        second_calls.load(Ordering::SeqCst),
        disabled_calls,
        "remount without a semantic runtime must clear the stale hook"
    );
    registry.shutdown().await;
}

struct BlockingSemanticScheduleProbeV1 {
    entered: Arc<Mutex<Receiver<CodeGenerationId>>>,
    release: Sender<()>,
    hook: SavedCodeGenerationScheduleHookV1,
}

impl BlockingSemanticScheduleProbeV1 {
    fn new() -> Self {
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let hook = Arc::new(move |generation: &CodeIndexPublishedGenerationV1| {
            entered_tx
                .send(generation.manifest().generation_id.clone())
                .expect("report scheduled semantic generation");
            release_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(3))
                .expect("release blocked semantic schedule");
            true
        }) as SavedCodeGenerationScheduleHookV1;
        Self {
            entered: Arc::new(Mutex::new(entered_rx)),
            release: release_tx,
            hook,
        }
    }

    async fn entered_generation(&self) -> CodeGenerationId {
        let entered = Arc::clone(&self.entered);
        tokio::task::spawn_blocking(move || {
            entered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(3))
                .expect("semantic schedule entry")
        })
        .await
        .expect("semantic schedule observation task")
    }

    fn release(&self) {
        self.release
            .send(())
            .expect("release semantic schedule hook");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_schedule_runs_after_exact_generation_becomes_servable() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let probe = BlockingSemanticScheduleProbeV1::new();
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(Arc::clone(&probe.hook)),
            )
            .await
            .expect("mount scheduler")
    );

    let first_scheduled = probe.entered_generation().await;
    let first_serving_while_hook_runs = registry.latest_generation_id(fixture.path()).await;
    probe.release();
    let first_published = wait_for_initial_generation(&registry, fixture.path()).await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second_scheduled = probe.entered_generation().await;
    let second_serving_while_hook_runs = registry.latest_generation_id(fixture.path()).await;
    probe.release();
    let second_published =
        wait_for_generation_change(&registry, fixture.path(), &first_published).await;
    registry.shutdown().await;

    assert_eq!(first_scheduled, first_published);
    assert_eq!(
        first_serving_while_hook_runs,
        Some(first_scheduled),
        "cold-mount semantics must not start before its exact code generation is serving"
    );
    assert_ne!(second_scheduled, first_published);
    assert_eq!(second_scheduled, second_published);
    assert_eq!(
        second_serving_while_hook_runs,
        Some(second_scheduled),
        "edited-generation semantics must not start while the prior code generation is serving"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicking_semantic_hook_does_not_retire_later_reconciliation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let project_root = fixture
        .path()
        .canonicalize()
        .expect("canonical project root");
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut publications = registry.subscribe_generation_publications();
    let panic_calls = Arc::new(AtomicUsize::new(0));
    let panicking_hook = {
        let calls = Arc::clone(&panic_calls);
        Arc::new(move |_: &CodeIndexPublishedGenerationV1| -> bool {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("semantic schedule panic fixture");
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(panicking_hook),
            )
            .await
            .expect("mount scheduler")
    );
    let first_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while panic_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panicking hook was called");
    let first_publication = published_generation_for_root(&mut publications, &project_root).await;
    assert_eq!(first_publication, first_generation);

    let replacement_calls = Arc::new(AtomicUsize::new(0));
    let replacement_hook = {
        let calls = Arc::clone(&replacement_calls);
        Arc::new(move |_: &CodeIndexPublishedGenerationV1| {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(replacement_hook),
            )
            .await
            .expect("replace panicking hook")
    );
    assert_eq!(
        replacement_calls.load(Ordering::SeqCst),
        1,
        "replacement hook replays the already-serving generation"
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second_publication = published_generation_for_root(&mut publications, &project_root).await;
    let second_generation = registry
        .latest_generation_id(fixture.path())
        .await
        .expect("edited serving generation");
    tokio::time::timeout(Duration::from_secs(3), async {
        while replacement_calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement hook received the edited generation");
    registry.shutdown().await;

    assert_ne!(first_generation, second_generation);
    assert_eq!(second_publication, second_generation);
}
