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

#[test]
fn identical_pool_and_staged_bytes_must_still_match_the_generation_digest() {
    let (store, _generations) = fixture_store(5);
    let plan =
        plan_code_generation_retention(store.path(), &BTreeSet::new(), 3).expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let canonical = store
        .path()
        .join(GENERATIONS_DIRECTORY)
        .join(&collectable.generation_file);
    let canonical_bytes = std::fs::read(&canonical).expect("read canonical generation");
    let mut wrong_bytes = canonical_bytes.clone();
    wrong_bytes[0] ^= 0x2a;
    let staged = store.path().join("digest-mismatched-staged.json");
    let pool_entry = store.path().join("digest-mismatched-pool.json");
    std::fs::write(&staged, &wrong_bytes).expect("write digest-mismatched staged bytes");
    std::fs::write(&pool_entry, &wrong_bytes).expect("write identical pool bytes");

    let error = verify_existing_graph_replay_pool_entry(&pool_entry, &staged, &collectable)
        .expect_err("matching copies with the wrong content digest must be refused");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert_eq!(
        std::fs::read(canonical).expect("canonical generation remains intact"),
        canonical_bytes,
    );
}

#[cfg(unix)]
#[test]
fn replaced_pool_path_is_not_the_opened_stable_destination() {
    let (store, _generations) = fixture_store(5);
    let plan =
        plan_code_generation_retention(store.path(), &BTreeSet::new(), 3).expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let canonical = store
        .path()
        .join(GENERATIONS_DIRECTORY)
        .join(&collectable.generation_file);
    let pool_entry = store.path().join("replaceable-pool-entry.json");
    std::fs::copy(&canonical, &pool_entry).expect("create exact pool entry");
    let opened = File::open(&pool_entry).expect("open admitted pool entry");
    let admitted = pool_entry
        .symlink_metadata()
        .expect("snapshot admitted pool path");
    let displaced = store.path().join("displaced-pool-entry.json");
    std::fs::rename(&pool_entry, &displaced).expect("replace admitted pool path");
    std::fs::copy(&canonical, &pool_entry).expect("install same-size replacement");

    assert!(
        !path_still_names_open_file(&pool_entry, &opened, &admitted)
            .expect("compare current path to opened identity"),
        "a replacement path must not be certified through the old open file"
    );
}

#[test]
fn missing_staged_generation_blocks_pool_exposure_before_receipt() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let plan =
        plan_code_generation_retention(store.path(), &BTreeSet::new(), 3).expect("plan retention");
    let receipt = build_receipt(&plan, plan.collectable_generations.clone(), UtcMicros(109))
        .expect("build retention receipt");
    let transaction = CodeGenerationRetentionTransactionV1 {
        schema: TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };
    persist_transaction(store.path(), &transaction).expect("persist transaction journal");
    stage_collectable_generations(store.path(), &transaction).expect("stage generation");
    let missing = transaction_stage_root(store.path(), &receipt)
        .join(&receipt.deleted_generations[0].generation_file);
    std::fs::remove_file(&missing).expect("remove staged source before exposure");
    let pool_lock =
        acquire_graph_replay_pool_lock(&pool_root).expect("acquire retention pool lock");

    let error = expose_staged_generations_under_graph_replay_pool_lock(
        store.path(),
        &transaction,
        &pool_lock,
    )
    .expect_err("missing staged source must block release publication");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(!store.path().join(RECEIPTS_DIRECTORY).exists());
    assert_eq!(queued_release_count(store.path()), 0);
    assert!(transaction_path(store.path()).is_file());
}

/// Deterministic retention-versus-reconciler interleaving at the staged
/// unlink boundary. The reconciler moves the canonical pool entry aside under
/// the canonical lock, then releases that lock while it verifies the staged
/// inode. Cleanup must fail closed while the release event is still live;
/// after the reconciler removes its staged inode and completes the event, a
/// retry may finish without resurrecting an orphan.
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
    {
        let pool_lock =
            acquire_graph_replay_pool_lock(&pool_root).expect("acquire retention pool lock");
        expose_staged_generations_under_graph_replay_pool_lock(
            store.path(),
            &transaction,
            &pool_lock,
        )
        .expect("expose staged generation to the pool");
    }
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

    // Reconciler phase one: move the canonical name aside while holding the
    // pool lock, exactly as stage_project_graph_replay_unlink does.
    let staged_unlink = pool_root.join(format!(
        ".{}.deterministic-staged-unlink",
        collectable.generation_file
    ));
    {
        let _pool_lock =
            acquire_code_generation_store_lock(&pool_root).expect("reconciler pool lock");
        assert!(
            pool_root.join(&collectable.generation_file).is_file(),
            "the replay never disappears while its release event is outstanding"
        );
        std::fs::rename(pool_root.join(&collectable.generation_file), &staged_unlink)
            .expect("reconciler stages the retired pool copy for unlink");
    }
    assert!(
        staged_unlink.is_file(),
        "the staged replay bytes remain available"
    );

    // Retention cleanup reaches the exact unlocked verification boundary. It
    // must preserve both staged copies and the transaction rather than
    // recreating a canonical pool name that the reconciler will not collect.
    let error = cleanup_committed_transaction(
        store.path(),
        &transaction,
        &BTreeSet::new(),
        Some(&pool_root),
    )
    .expect_err("cleanup must wait for the in-flight release to become terminal");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        transaction_stage_root(store.path(), &receipt)
            .join(&collectable.generation_file)
            .is_file(),
        "retention keeps its staged canonical bytes while release is in flight"
    );
    assert!(
        graph_replay_release::release_event_exists(store.path(), &receipt, &collectable)
            .expect("release event remains readable")
    );

    // Reconciler phase two: verification succeeded, so it reacquires the
    // canonical lock, removes only its staged inode, and checkpoints release.
    {
        let _pool_lock =
            acquire_code_generation_store_lock(&pool_root).expect("reconciler finalizer lock");
        std::fs::remove_file(&staged_unlink).expect("finalize staged replay unlink");
    }
    complete_code_generation_graph_replay_release(store.path(), &release)
        .expect("reconciler completes the release event");

    cleanup_committed_transaction(
        store.path(),
        &transaction,
        &BTreeSet::new(),
        Some(&pool_root),
    )
    .expect("cleanup retries after release completion");
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
    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(10),
        None,
    )
    .expect("apply retention");
    let receipt = report.receipt.expect("durable retention receipt");
    let released_generation = receipt.deleted_generations[0].clone();
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
    assert!(matches!(
        graph_replay_release::release_event_exists(store.path(), &receipt, &released_generation,),
        Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
    ));
}
