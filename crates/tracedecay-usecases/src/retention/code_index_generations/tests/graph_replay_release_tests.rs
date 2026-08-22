use super::*;

#[test]
fn durable_deletion_receipt_enqueues_restart_safe_graph_release() {
    let (store, generations) = fixture_store(3);
    let plan = plan_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
    )
    .expect("plan retention");
    let deleted = plan.collectable_generations[0].clone();
    execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(10),
        None,
    )
    .expect("apply retention");

    let page = code_generation_graph_replay_release_page(store.path(), None)
        .expect("read durable graph replay release");
    assert_eq!(page.releases.len(), 1);
    assert_eq!(page.releases[0].generation, deleted);
    assert_ne!(page.releases[0].generation.generation_id, generations[2].id);

    complete_code_generation_graph_replay_release(store.path(), &page.releases[0])
        .expect("checkpoint graph replay release");
    assert!(
        code_generation_graph_replay_release_page(store.path(), None)
            .expect("read empty graph replay release queue")
            .releases
            .is_empty()
    );
}

#[test]
fn graph_release_queue_pages_more_than_one_retention_batch() {
    let (store, _) = fixture_store(70);
    loop {
        let plan = plan_next_code_generation_retention_cancellable(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            &|| false,
        )
        .expect("plan retention");
        if plan.collectable_generations.is_empty() {
            break;
        }
        execute_code_generation_retention(
            store.path(),
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(10),
            None,
        )
        .expect("apply retention");
    }

    let mut after = None;
    let mut released = BTreeSet::new();
    loop {
        let page = code_generation_graph_replay_release_page(store.path(), after.as_deref())
            .expect("read graph replay release page");
        assert!(page.releases.len() <= MAX_CODE_GENERATION_RETENTION_BATCH_V1);
        for release in page.releases {
            assert!(released.insert(release.generation.generation_id));
        }
        let Some(continuation) = page.continuation else {
            break;
        };
        after = Some(continuation);
    }

    assert_eq!(released.len(), 69);
}

#[test]
fn graph_release_queue_rejects_corrupt_and_oversize_evidence() {
    let (store, _) = fixture_store(4);
    let plan = plan_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
    )
    .expect("plan retention");
    execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(10),
        None,
    )
    .expect("apply retention");
    let release_path = std::fs::read_dir(store.path().join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY))
        .expect("read release queue")
        .next()
        .expect("release entry")
        .expect("release entry")
        .path();

    std::fs::write(&release_path, b"{").expect("corrupt release evidence");
    assert!(matches!(
        code_generation_graph_replay_release_page(store.path(), None),
        Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
    ));

    let file = std::fs::File::create(&release_path).expect("replace release evidence");
    file.set_len(MAX_TRANSACTION_BYTES + 1)
        .expect("oversize release evidence");
    assert!(matches!(
        code_generation_graph_replay_release_page(store.path(), None),
        Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
    ));
}

/// Deterministic retention-versus-reconciler interleaving at the staged
/// unlink boundary. Retention exposes the retired generation and makes its
/// receipt durable; before retention's cleanup phase runs, an old in-process
/// reconciler — holding the same canonical pool lock the daemon's replay
/// reconciler uses — consumes the queued release event as its typed
/// retirement authority, unlinks the pool copy, and completes the event.
/// Cleanup must then not resurrect the retired pool entry (an orphan no
/// authority would ever collect), and no required replay may disappear: the
/// pool copy stays present the whole time the release event is outstanding,
/// and every retained generation keeps its canonical file.
#[test]
fn stale_reconciler_retirement_interleaves_with_retention_without_orphan_or_missing_replay() {
    let (store, generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let plan =
        plan_code_generation_retention(store.path(), &BTreeSet::new(), 3).expect("plan retention");
    assert_eq!(plan.collectable_generations.len(), 1);
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let receipt = build_receipt(&plan, plan.collectable_generations.clone(), UtcMicros(108))
        .expect("build retention receipt");
    let transaction = CodeGenerationRetentionTransactionV1 {
        schema: TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };

    // Retention: journal, quarantine, and expose before the receipt.
    persist_transaction(store.path(), &transaction).expect("persist transaction journal");
    stage_collectable_generations(store.path(), &transaction).expect("stage generation");
    expose_staged_generations_to_graph_replay_pool(
        store.path(),
        &transaction,
        &pool_root,
        GraphReplayPoolExposureV1::BeforeReceipt,
    )
    .expect("expose staged generation to the pool");
    assert!(
        pool_root.join(&collectable.generation_file).is_file(),
        "the pool copy must exist before the release event can become durable"
    );

    // Retention: the receipt and its release event become durable.
    write_receipt(store.path(), &receipt).expect("write durable receipt");
    let page = code_generation_graph_replay_release_page(store.path(), None)
        .expect("read durable release event");
    assert_eq!(page.releases.len(), 1);
    let release = page.releases[0].clone();
    assert_eq!(release.generation, collectable);

    // Old reconciler at the staged unlink boundary: under the canonical pool
    // lock it retires the pool copy with the release event as its typed
    // authority, then completes the event. Retention's cleanup has not run.
    {
        let _pool_lock =
            acquire_code_generation_store_lock(&pool_root).expect("reconciler pool lock");
        assert!(
            pool_root.join(&collectable.generation_file).is_file(),
            "the replay never disappears while its release event is outstanding"
        );
        std::fs::remove_file(pool_root.join(&collectable.generation_file))
            .expect("reconciler unlinks the retired pool copy");
        complete_code_generation_graph_replay_release(store.path(), &release)
            .expect("reconciler completes the release event");
    }

    // Retention: cleanup replays exposure after the durable receipt.
    cleanup_committed_transaction(
        store.path(),
        &transaction,
        &BTreeSet::new(),
        Some(&pool_root),
    )
    .expect("cleanup committed transaction");
    clear_transaction(store.path()).expect("clear transaction journal");

    // No orphan: the consumed release's pool copy must not be resurrected,
    // and no release event survives without its pool copy.
    assert!(
        !pool_root.join(&collectable.generation_file).exists(),
        "cleanup must not resurrect a pool entry the graph already retired"
    );
    assert!(
        code_generation_graph_replay_release_page(store.path(), None)
            .expect("read release queue after cleanup")
            .releases
            .is_empty(),
        "no release event may remain without a matching pool copy"
    );
    // No missing replay: the retired generation was fully released under
    // typed authority, and every retained generation keeps its canonical
    // file and never entered the pool.
    assert!(
        !generations_root.join(&collectable.generation_file).exists(),
        "the retired generation's canonical file was collected"
    );
    for generation in &generations {
        if generation.file == collectable.generation_file {
            continue;
        }
        assert!(
            generations_root.join(&generation.file).is_file(),
            "retained generation '{}' must keep its canonical file",
            generation.file
        );
        assert!(
            !pool_root.join(&generation.file).exists(),
            "retained generation '{}' must not leak into the pool",
            generation.file
        );
    }
    assert!(!transaction_path(store.path()).exists());
}

#[cfg(unix)]
#[test]
fn graph_release_queue_rejects_symlink_evidence() {
    use std::os::unix::fs::symlink;

    let (store, _) = fixture_store(4);
    let plan = plan_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
    )
    .expect("plan retention");
    execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(10),
        None,
    )
    .expect("apply retention");
    let release_path = std::fs::read_dir(store.path().join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY))
        .expect("read release queue")
        .next()
        .expect("release entry")
        .expect("release entry")
        .path();
    let receipt_path = std::fs::read_dir(store.path().join(RECEIPTS_DIRECTORY))
        .expect("read receipts")
        .next()
        .expect("receipt entry")
        .expect("receipt entry")
        .path();
    std::fs::remove_file(&release_path).expect("remove release");
    symlink(receipt_path, &release_path).expect("symlink release");

    assert!(matches!(
        code_generation_graph_replay_release_page(store.path(), None),
        Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
    ));
}
