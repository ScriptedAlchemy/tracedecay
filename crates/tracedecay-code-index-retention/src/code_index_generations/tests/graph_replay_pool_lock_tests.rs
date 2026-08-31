use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

fn ensure_replay_pool(pool_root: &std::path::Path) {
    match tracedecay_private_fs::create_private_directory(pool_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tracedecay_private_fs::validate_private_directory(pool_root)
                .expect("existing replay pool must be owner-private");
        }
        Err(error) => panic!("create replay pool: {error}"),
    }
}

fn hold_replay_pool(pool_root: &std::path::Path) -> CodeGenerationStoreLockV1 {
    ensure_replay_pool(pool_root);
    try_acquire_code_generation_store_lock(pool_root)
        .expect("probe replay pool")
        .expect("publisher acquires the replay pool")
}

fn isolated_pool() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::TempDir::new().expect("pool parent");
    let pool = root.path().join("graph-replay");
    ensure_replay_pool(&pool);
    (root, pool)
}

/// The remaining TOCTOU after the outer non-blocking probe: the probe
/// succeeds, the publisher takes the pool, and execute must defer with a
/// typed busy result instead of a deadline-free flock. Nothing is exposed.
#[test]
fn publisher_between_probe_and_execute_defers_without_exposure() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    ensure_replay_pool(&pool_root);

    let probe = try_acquire_code_generation_store_lock(&pool_root)
        .expect("outer probe")
        .expect("probe finds a free pool");
    drop(probe);

    let publisher = hold_replay_pool(&pool_root);
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    assert!(
        !plan.collectable_generations.is_empty(),
        "the race is only live when execute would acquire the pool"
    );
    let collectable = plan.collectable_generations[0].clone();
    let started = Instant::now();
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(120),
        Some(&pool_root),
    )
    .expect_err("a held pool after a successful probe must defer");
    let elapsed = started.elapsed();

    assert!(
        matches!(error, CodeGenerationRetentionErrorV1::GraphReplayPoolBusy),
        "executor must return the typed busy result, got {error:?}"
    );
    assert!(
        elapsed < GRAPH_REPLAY_POOL_ACQUIRE_BUDGET + Duration::from_millis(50),
        "bounded acquire must finish inside the budget, took {elapsed:?}"
    );
    assert!(
        !transaction_path(store.path()).exists(),
        "a deferred acquire must not persist a retention journal"
    );
    assert!(
        store
            .path()
            .join(GENERATIONS_DIRECTORY)
            .join(&collectable.generation_file)
            .is_file(),
        "a deferred acquire must leave the canonical generation in place"
    );
    assert!(
        !pool_root.join(&collectable.generation_file).exists(),
        "a deferred acquire must not expose a pool entry"
    );
    assert_eq!(
        queued_release_count(store.path()),
        0,
        "a deferred acquire must not publish release evidence"
    );

    drop(publisher);
    assert!(
        try_acquire_code_generation_store_lock(&pool_root)
            .expect("re-probe after publisher drop")
            .is_some(),
        "execute must not leak a pool lock after a busy deferral"
    );
}

#[test]
fn checked_acquire_returns_cancelled_without_waiting_out_the_budget() {
    let (_root, pool) = isolated_pool();
    let publisher = hold_replay_pool(&pool);
    let started = Instant::now();
    let error = match acquire_graph_replay_pool_lock_checked(
        &pool,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        &|| true,
    ) {
        Ok(_) => panic!("cancellation must win a held-pool wait"),
        Err(error) => error,
    };
    let elapsed = started.elapsed();

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(
        elapsed < Duration::from_millis(20),
        "cancelled acquire must not poll the budget, took {elapsed:?}"
    );
    drop(publisher);
}

#[test]
fn execute_cancels_held_pool_acquire_before_any_exposure() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let publisher = hold_replay_pool(&pool_root);
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let checks = AtomicUsize::new(0);
    let started = Instant::now();
    let error = execute_code_generation_retention_cancellable(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(121),
        Some(&pool_root),
        &|| checks.fetch_add(1, Ordering::SeqCst) >= 2,
    )
    .expect_err("cancellation during pool wait must abort execute");
    let elapsed = started.elapsed();

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(
        elapsed < Duration::from_millis(20),
        "cancelled execute must not wait out the acquire budget, took {elapsed:?}"
    );
    assert!(
        !transaction_path(store.path()).exists(),
        "cancelled acquire must not persist a retention journal"
    );
    assert!(
        store
            .path()
            .join(GENERATIONS_DIRECTORY)
            .join(&collectable.generation_file)
            .is_file()
    );
    assert!(!pool_root.join(&collectable.generation_file).exists());
    drop(publisher);
}

#[test]
fn checked_acquire_returns_busy_when_the_carried_deadline_has_elapsed() {
    let (_root, pool) = isolated_pool();
    let publisher = hold_replay_pool(&pool);
    let started = Instant::now();
    let error = match acquire_graph_replay_pool_lock_checked(&pool, Instant::now(), &|| false) {
        Ok(_) => panic!("an elapsed deadline must defer a held pool"),
        Err(error) => error,
    };
    let elapsed = started.elapsed();

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::GraphReplayPoolBusy
    ));
    assert!(
        elapsed < Duration::from_millis(20),
        "an expired deadline must not poll, took {elapsed:?}"
    );
    drop(publisher);
}

#[test]
fn checked_acquire_takes_a_free_pool_and_releases_without_leak() {
    let (_root, pool) = isolated_pool();
    let lock = acquire_graph_replay_pool_lock_checked(
        &pool,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        &|| false,
    )
    .expect("acquire a free replay pool");
    assert!(
        try_acquire_code_generation_store_lock(&pool)
            .expect("probe while held")
            .is_none(),
        "the acquired guard must actually hold the pool"
    );
    drop(lock);
    assert!(
        try_acquire_code_generation_store_lock(&pool)
            .expect("probe after release")
            .is_some(),
        "drop must release the pool lock"
    );
}
