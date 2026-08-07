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

    assert_eq!(released.len(), 67);
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
