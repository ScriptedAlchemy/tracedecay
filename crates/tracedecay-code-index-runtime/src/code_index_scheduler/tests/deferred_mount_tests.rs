use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_domain::{CodeGenerationId, ContentDigest, RepositoryId};

use super::{ALPHA_LIB_V1, GitFixture, wait_for_initial_generation};
use crate::code_index_scheduler::query_runtime::{
    DeferredMountAttemptV1, retry_deferred_query_authority_until_serving,
};
use crate::code_index_scheduler::{CodeIndexGenerationPublishedV1, CodeIndexSchedulerRegistryV1};

const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;

fn synthetic_publication(project_root: &Path, index: usize) -> CodeIndexGenerationPublishedV1 {
    CodeIndexGenerationPublishedV1 {
        project_root: project_root.to_path_buf(),
        repository_id: RepositoryId::new("repository.deferred-mount.test").expect("repository id"),
        generation_id: CodeGenerationId::new(format!("generation.deferred-mount.{index}"))
            .expect("generation id"),
        snapshot_content_identity: ContentDigest::new(format!("sha256:{index:064x}"))
            .expect("content digest"),
        observation_time_micros: i64::try_from(index).expect("fixture index fits i64"),
    }
}

fn spawn_deferred_mount_waiter(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: std::path::PathBuf,
    attempts: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    let registry = registry.clone();
    tokio::spawn(async move {
        retry_deferred_query_authority_until_serving(&registry, project_root, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { DeferredMountAttemptV1::Terminal }
        })
        .await;
    })
}

/// Cold deferred mount must wake on publication / serving watches, not a
/// standing 1 Hz `ready_poll`. After the first generation seats, the waiter
/// finishes well under one second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_query_authority_wakes_without_ready_poll() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let project_root = fixture.path().to_path_buf();
    let attempts = Arc::new(AtomicUsize::new(0));

    let waiter =
        spawn_deferred_mount_waiter(&registry, project_root.clone(), Arc::clone(&attempts));

    // Let the waiter miss the empty slot before mount publishes.
    tokio::task::yield_now().await;

    registry
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    let _ = wait_for_initial_generation(&registry, fixture.path()).await;

    let started = Instant::now();
    tokio::time::timeout(Duration::from_millis(750), waiter)
        .await
        .expect("deferred mount woke without a 1s ready_poll")
        .expect("deferred mount task");
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "event-driven wake must not wait out a 1 Hz poll"
    );
    assert!(
        attempts.load(Ordering::SeqCst) >= 1,
        "mount attempt must run once a retained text owner is seated"
    );
    registry.shutdown().await;
}

/// A lagged publication receiver must settle once and retry; the mount still
/// completes without installing a standing interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_query_authority_recovers_from_lagged_publications() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let project_root = fixture.path().to_path_buf();
    let attempts = Arc::new(AtomicUsize::new(0));
    let noise_root = TempDir::new().expect("noise root");

    let waiter =
        spawn_deferred_mount_waiter(&registry, project_root.clone(), Arc::clone(&attempts));

    tokio::task::yield_now().await;

    let flood_registry = registry.clone();
    let noise_root = noise_root.path().to_path_buf();
    let flood = tokio::spawn(async move {
        for index in 0..=GENERATION_PUBLICATION_CHANNEL_CAPACITY {
            flood_registry
                .push_generation_publication_for_test(synthetic_publication(&noise_root, index));
        }
    });

    registry
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    let _ = wait_for_initial_generation(&registry, fixture.path()).await;
    flood.await.expect("publication flood");

    let started = Instant::now();
    tokio::time::timeout(Duration::from_millis(750), waiter)
        .await
        .expect("lagged receiver must still wake event-driven")
        .expect("deferred mount task");
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "Lagged settle must not degrade into a 1 Hz poll"
    );
    assert!(
        attempts.load(Ordering::SeqCst) >= 1,
        "mount attempt must run after lagged settle"
    );
    registry.shutdown().await;
}

/// A retained restart that seats silently (`Noop`, no republication) must still
/// wake a waiter subscribed before the remount probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_query_authority_subscribe_race_survives_silent_restore() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("initial mount");
    let sealed = wait_for_initial_generation(&first, fixture.path()).await;
    first.shutdown().await;

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    let attempts = Arc::new(AtomicUsize::new(0));
    let project_root = fixture.path().to_path_buf();
    let waiter = spawn_deferred_mount_waiter(&restarted, project_root, Arc::clone(&attempts));
    tokio::task::yield_now().await;

    restarted
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("remount worktree");
    let seated = wait_for_initial_generation(&restarted, fixture.path()).await;
    // Only the wake belongs on this clock. Charging the remount's own restore
    // work to it measured how fast this machine reopens an index, not whether
    // the watch is event-driven: a standing poll still takes its full period
    // from here.
    let wake_started = Instant::now();

    tokio::time::timeout(Duration::from_millis(750), waiter)
        .await
        .expect("silent restore must wake the pre-subscribe waiter")
        .expect("deferred mount task");
    assert!(
        wake_started.elapsed() < Duration::from_millis(750),
        "serving watches must wake without waiting out a 1 Hz poll"
    );
    assert_eq!(seated, sealed, "restore must seat the retained generation");
    assert!(
        attempts.load(Ordering::SeqCst) >= 1,
        "pre-remount subscribe must observe the seated owner on retry"
    );
    restarted.shutdown().await;
}

/// Pre-activation subscribe returns `None`. Partitioned verified-head Noop
/// recovery then emits neither publication nor registry-wide serving-seat,
/// only the per-worktree watch created at mount. The waiter must wake on
/// registry-wide root-mounted so it can re-subscribe; otherwise it parks
/// forever and query authority stays unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_query_authority_wakes_after_pre_mount_subscribe_on_partitioned_noop() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("initial mount");
    let sealed = wait_for_initial_generation(&first, fixture.path()).await;
    let partitioned = first
        .retained_text_owner_for_root(fixture.path())
        .await
        .expect("retained text owner")
        .uses_partitioned_manifest();
    assert!(
        partitioned,
        "fixture must retain a partitioned generation so remount takes the Noop path"
    );
    first.shutdown().await;

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    let project_root = fixture.path().to_path_buf();
    assert!(
        restarted
            .subscribe_serving_generation_changes(&project_root)
            .await
            .is_none(),
        "pre-activation serving-generation subscribe must miss until mount"
    );

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut root_mounted = restarted.subscribe_root_mounted();
    let initial_mount = *root_mounted.borrow();
    let waiter = spawn_deferred_mount_waiter(&restarted, project_root, Arc::clone(&attempts));

    // Park the waiter on the pre-mount select arms before remount commits.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    restarted
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("remount worktree");

    tokio::time::timeout(Duration::from_millis(750), root_mounted.changed())
        .await
        .expect("mount must advance the registry-wide root-mounted watch")
        .expect("root-mounted channel stays open");
    assert_ne!(
        *root_mounted.borrow(),
        initial_mount,
        "root-mounted must advance on insert"
    );
    let _ = wait_for_initial_generation(&restarted, fixture.path()).await;
    // Same reason as the silent-restore sibling: the Noop recovery itself is
    // real work, and only the wake that follows it is what this test is about.
    let wake_started = Instant::now();

    tokio::time::timeout(Duration::from_millis(750), waiter)
        .await
        .expect("pre-mount waiter must wake via root-mounted then per-worktree Noop")
        .expect("deferred mount task");
    assert!(
        wake_started.elapsed() < Duration::from_millis(750),
        "partitioned Noop wake must not wait out a standing poll"
    );
    assert_eq!(
        restarted.latest_generation_id(fixture.path()).await,
        Some(sealed),
        "restore must seat the retained generation identity"
    );
    assert!(
        attempts.load(Ordering::SeqCst) >= 1,
        "mount attempt must run after subscribe-after-mount"
    );
    restarted.shutdown().await;
}
