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
fn segment_only_sweep_defers_while_the_replay_pool_is_held() {
    let store = tempfile::TempDir::new().expect("store root");
    std::fs::create_dir_all(store.path().join(GENERATIONS_DIRECTORY))
        .expect("create generation root");
    let segments_root = store.path().join(GENERATION_SEGMENTS_DIRECTORY);
    std::fs::create_dir_all(&segments_root).expect("create segment root");
    let orphan_bytes = b"unreferenced final evidence pack";
    let orphan_digest = encode_lowercase_hex(&Sha256::digest(orphan_bytes));
    let orphan_path = segments_root.join(format!("segment-{orphan_digest}.json"));
    std::fs::write(&orphan_path, orphan_bytes).expect("write orphan segment");
    let pool_root = store.path().join("graph-replay-pool");
    ensure_replay_pool(&pool_root);
    let plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        Some(&pool_root),
    )
    .expect("plan segment-only sweep");
    assert!(plan.collectable_generations.is_empty());
    assert!(plan.has_collectable_work());

    let publisher = hold_replay_pool(&pool_root);
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(122),
        Some(&pool_root),
    )
    .expect_err("a segment-only sweep must respect the replay-pool lock");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::GraphReplayPoolBusy
    ));
    assert!(
        orphan_path.is_file(),
        "a deferred segment-only sweep must not unlink any segment"
    );
    drop(publisher);
}

#[test]
fn segment_sweep_marks_a_replay_unlink_while_staged() {
    let store = tempfile::TempDir::new().expect("store root");
    std::fs::create_dir_all(store.path().join(GENERATIONS_DIRECTORY))
        .expect("create generation root");
    let segments_root = store.path().join(GENERATION_SEGMENTS_DIRECTORY);
    std::fs::create_dir_all(&segments_root).expect("create segment root");
    let orphan_bytes = b"unreferenced final evidence pack";
    let orphan_digest = encode_lowercase_hex(&Sha256::digest(orphan_bytes));
    let orphan_path = segments_root.join(format!("segment-{orphan_digest}.json"));
    std::fs::write(&orphan_path, orphan_bytes).expect("write orphan segment");

    let pool_root = store.path().join("graph-replay-pool");
    ensure_replay_pool(&pool_root);
    let replay_manifest = serde_json::to_vec(&serde_json::json!({
        "state_digest": format!("sha256:{}", "0".repeat(64)),
        "generation": {
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "snapshot": { "files": [] },
            "file_segments": [],
            "generation_evidence": {
                "segment_digest": format!("sha256:{orphan_digest}"),
                "segment_size_bytes": orphan_bytes.len(),
                "pages": [{
                    "page_ordinal": 0,
                    "page_digest": format!("sha256:{orphan_digest}"),
                    "page_size_bytes": orphan_bytes.len()
                }]
            }
        }
    }))
    .expect("encode staged replay manifest");
    let replay_digest = encode_lowercase_hex(&Sha256::digest(&replay_manifest));
    let staged_unlink = pool_root.join(format!(".generation-{replay_digest}.unlink-123-456-1"));
    let mut corrupted_replay_manifest = replay_manifest.clone();
    corrupted_replay_manifest.push(b' ');
    std::fs::write(&staged_unlink, corrupted_replay_manifest).expect("stage corrupt replay unlink");
    let error = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        Some(&pool_root),
    )
    .expect_err("a replay entry that changed under its content address must fail closed");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(orphan_path.is_file());
    std::fs::write(&staged_unlink, replay_manifest).expect("stage replay unlink");

    let plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        Some(&pool_root),
    )
    .expect("mark while replay unlink is staged");
    assert!(
        !plan.has_collectable_work(),
        "the hidden replay manifest must mark its evidence pack live"
    );
    assert!(
        orphan_path.is_file(),
        "planning must preserve the segment while replay liveness is ambiguous"
    );

    std::fs::remove_file(&staged_unlink).expect("finish staged replay unlink");
    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(123),
        Some(&pool_root),
    )
    .expect("collect after replay pool becomes empty");
    assert!(report.deleted_generations.is_empty());
    assert!(
        !orphan_path.exists(),
        "maintenance must reclaim the orphan after replay ambiguity clears"
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

/// An already-expired caller deadline is a typed busy result even when the
/// pool is free. Checking after `try_lock` would take the lock and lose the
/// deadline, or — on Windows — turn a native lock-conflict into Storage.
#[test]
fn checked_acquire_returns_busy_for_an_elapsed_deadline_without_taking_a_free_pool() {
    let (_root, pool) = isolated_pool();
    let started = Instant::now();
    let error = match acquire_graph_replay_pool_lock_checked(&pool, Instant::now(), &|| false) {
        Ok(_) => panic!("an elapsed deadline must not take a free pool"),
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
    assert!(
        try_acquire_code_generation_store_lock(&pool)
            .expect("probe after expired-deadline refuse")
            .is_some(),
        "an expired deadline must leave a free pool untouched"
    );
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
