#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

mod graph_replay_pool_lock_tests;
mod graph_replay_release_tests;

const TEST_ROLLBACK_FLOOR: usize = 3;

fn indexed_generation(
    sequence: usize,
    sealed_at_micros: i64,
    size_bytes: u64,
    exact: bool,
) -> DurableGenerationIndexEntryV1 {
    DurableGenerationIndexEntryV1 {
        generation_id: format!("generation.v1.retention.{sequence:08}"),
        snapshot_content_identity: format!("sha256:{sequence:064x}"),
        sealed_at_micros,
        size_bytes,
        segment_bytes: 0,
        generation_file: format!("generation-{sequence:064x}.json"),
        state_digest: format!("sha256:{sequence:064x}"),
        source_reference: exact.then(|| format!("refs/heads/branch-{sequence}")),
        source_revision: exact.then(|| format!("{sequence:040x}")),
        source_tree: exact.then(|| format!("{:040x}", sequence + 1)),
        cardinality: None,
        text_artifact: None,
    }
}

fn text_artifact(
    generation_id: &CodeGenerationId,
    sequence: usize,
    artifact_size_bytes: u64,
) -> DurableCodeTextArtifactDescriptorV1 {
    DurableCodeTextArtifactDescriptorV1 {
        generation_id: generation_id.clone(),
        artifact_file: format!("text-artifact-{sequence:064x}.bin"),
        artifact_digest: ManifestDigest::new(format!("sha256:{sequence:064x}"))
            .expect("artifact digest"),
        artifact_size_bytes,
    }
}

fn text_artifact_for_bytes(
    generation_id: &CodeGenerationId,
    bytes: &[u8],
) -> DurableCodeTextArtifactDescriptorV1 {
    let digest_bytes = Sha256::digest(bytes);
    let digest = encode_lowercase_hex(&digest_bytes);
    DurableCodeTextArtifactDescriptorV1 {
        generation_id: generation_id.clone(),
        artifact_file: format!("text-artifact-{digest}.bin"),
        artifact_digest: ManifestDigest::from_sha256_bytes(&digest_bytes).expect("artifact digest"),
        artifact_size_bytes: u64::try_from(bytes.len()).expect("artifact byte count"),
    }
}

fn write_text_artifact(
    store: &tempfile::TempDir,
    descriptor: &DurableCodeTextArtifactDescriptorV1,
    bytes: &[u8],
) -> PathBuf {
    let root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&root).expect("create artifact root");
    let path = root.join(&descriptor.artifact_file);
    std::fs::write(&path, bytes).expect("write artifact bytes");
    path
}

fn attach_fixture_text_artifact(
    store: &tempfile::TempDir,
    generation: &FixtureGeneration,
    bytes: &[u8],
) -> DurableCodeTextArtifactDescriptorV1 {
    let descriptor = text_artifact_for_bytes(&generation.id, bytes);
    write_text_artifact(store, &descriptor, bytes);
    let expected = read_active_pointer(store.path()).expect("read fixture pointer");
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: generation.file.clone(),
        digest: ManifestDigest::new(generation.state_digest.clone()).expect("sealed digest"),
        size_bytes: generation.size_bytes,
    };
    let lock = acquire_code_generation_store_lock(store.path()).expect("generation store lock");
    attach_verified_text_artifact_under_lock(
        &lock,
        &expected,
        &sealed_identity,
        descriptor.clone(),
    )
    .expect("attach fixture artifact");
    descriptor
}

#[test]
fn durable_index_bounds_clean_and_dirty_history_by_ttl_bytes_and_count() {
    let now = MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1 * 2;
    let active = indexed_generation(99, now, 32, true);
    let mut segment_heavy = indexed_generation(1, now - 3, 1, true);
    segment_heavy.segment_bytes = MAX_DURABLE_GENERATION_INDEX_BYTES_V1;
    let mut entries = vec![
        indexed_generation(
            0,
            now - MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1 - 1,
            1,
            false,
        ),
        segment_heavy,
        indexed_generation(2, now - 2, 32, false),
        active.clone(),
    ];
    entries.extend(
        (3..=MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 + 2)
            .map(|sequence| indexed_generation(sequence, now - 1, 1, sequence % 2 == 0)),
    );

    let removed = retain_bounded_generation_index(&mut entries, &active.generation_id);

    assert!(
        removed >= 3,
        "TTL, byte, and count pressure must evict history"
    );
    assert!(entries.iter().any(|entry| entry == &active));
    assert!(entries.len() <= MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1);
    assert!(
        entries
            .iter()
            .map(|entry| entry.size_bytes.saturating_add(entry.segment_bytes))
            .sum::<u64>()
            <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
    );
    assert!(
        entries.iter().all(|entry| {
            entry.generation_id == active.generation_id
                || entry.sealed_at_micros >= now - MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1
        }),
        "dirty generations are not exempt from the TTL"
    );
}

#[test]
fn durable_index_counts_text_bytes_and_never_evicts_the_active_text_head() {
    let now = MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1 * 2;
    let active = indexed_generation(99, now, 32, true);
    let mut text_head = indexed_generation(1, now - 3, 32, true);
    let text_head_id =
        CodeGenerationId::new(text_head.generation_id.clone()).expect("text-head generation id");
    text_head.text_artifact = Some(text_artifact(
        &text_head_id,
        1,
        MAX_DURABLE_GENERATION_INDEX_BYTES_V1,
    ));
    let mut entries = vec![
        indexed_generation(0, now - 4, 32, true),
        text_head.clone(),
        active.clone(),
    ];

    let removed = retain_bounded_generation_index_with_text_head(
        &mut entries,
        &active.generation_id,
        Some(&text_head.generation_id),
    );

    assert_eq!(removed, 1, "artifact bytes must participate in the bound");
    assert!(entries.contains(&active));
    assert!(entries.contains(&text_head));
}

#[derive(Clone)]
struct FixtureGeneration {
    id: CodeGenerationId,
    file: String,
    state_digest: String,
    size_bytes: u64,
}

fn fixture_store(count: usize) -> (tempfile::TempDir, Vec<FixtureGeneration>) {
    let store = tempfile::TempDir::new().expect("create generation store");
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    std::fs::create_dir_all(&generations_root).expect("create generation directory");
    let mut generations = Vec::with_capacity(count);

    for sequence in 0..count {
        let generation_id = CodeGenerationId::new(format!("generation.v1.fixture.{sequence:08}"))
            .expect("valid generation id");
        let sealed_at = i64::try_from(sequence).expect("fixture sequence fits i64");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": {
                "generation_id": generation_id.as_str(),
                "seal": { "sealed_at": sealed_at },
            },
            "chunks": [],
        }))
        .expect("serialize generation fixture");
        let state_digest = encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&bytes));
        let file = format!(
            "generation-{}.json",
            state_digest.strip_prefix("sha256:").expect("digest prefix")
        );
        let size_bytes = u64::try_from(bytes.len()).expect("fixture size fits u64");
        std::fs::write(generations_root.join(&file), bytes).expect("write generation fixture");
        generations.push(FixtureGeneration {
            id: generation_id,
            file,
            state_digest,
            size_bytes,
        });
    }

    let active = generations.last().expect("at least one generation");
    let active_entry = DurableGenerationIndexEntryV1 {
        generation_id: active.id.as_str().to_owned(),
        snapshot_content_identity: "snapshot.fixture".to_owned(),
        sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
        size_bytes: active.size_bytes,
        segment_bytes: 0,
        generation_file: active.file.clone(),
        state_digest: active.state_digest.clone(),
        source_reference: None,
        source_revision: None,
        source_tree: None,
        cardinality: None,
        text_artifact: None,
    };
    let generation_index = vec![active_entry];
    let generation_index_digest =
        durable_generation_index_digest(&generation_index, true).expect("index digest");
    let pointer = DurablePublicationPointerV1 {
        generation_id: active.id.as_str().to_owned(),
        snapshot_content_identity: "snapshot.fixture".to_owned(),
        publication_digest: "sha256:publication".to_owned(),
        sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
        generation_file: active.file.clone(),
        state_digest: active.state_digest.clone(),
        generation_index,
        generation_index_truncated: true,
        generation_index_digest: Some(generation_index_digest),
    };
    std::fs::write(
        store.path().join(ACTIVE_POINTER_FILE),
        serde_json::to_vec(&pointer).expect("serialize active pointer"),
    )
    .expect("write active pointer");

    (store, generations)
}

#[test]
fn verified_text_artifact_attachment_is_durable_and_idempotent_under_the_store_lock() {
    let (store, generations) = fixture_store(1);
    let expected = read_active_pointer(store.path()).expect("active pointer");
    let active = generations.last().expect("active generation");
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: active.file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("sealed digest"),
        size_bytes: active.size_bytes,
    };
    let descriptor = text_artifact(&active.id, 7, 4096);
    let lock = acquire_code_generation_store_lock(store.path()).expect("generation store lock");

    let updated = attach_verified_text_artifact_under_lock(
        &lock,
        &expected,
        &sealed_identity,
        descriptor.clone(),
    )
    .expect("attach verified artifact");
    let repeated = attach_verified_text_artifact_under_lock(
        &lock,
        &updated,
        &sealed_identity,
        descriptor.clone(),
    )
    .expect("repeat exact attachment");
    drop(lock);

    assert_eq!(repeated, updated);
    assert_eq!(
        read_active_pointer(store.path())
            .expect("durable active pointer")
            .generation_index[0]
            .text_artifact,
        Some(descriptor)
    );
}

#[test]
fn verified_text_artifact_attachment_retires_history_before_enforcing_byte_bound() {
    let (store, generations) = fixture_store(2);
    let prior = &generations[0];
    let active = &generations[1];
    let mut pointer = read_active_pointer(store.path()).expect("active pointer");
    let prior_artifact = text_artifact(&prior.id, 31, MAX_DURABLE_GENERATION_INDEX_BYTES_V1 / 2);
    pointer.generation_index.insert(
        0,
        DurableGenerationIndexEntryV1 {
            generation_id: prior.id.as_str().to_owned(),
            snapshot_content_identity: "snapshot.prior".to_owned(),
            sealed_at_micros: 0,
            size_bytes: prior.size_bytes,
            segment_bytes: 0,
            generation_file: prior.file.clone(),
            state_digest: prior.state_digest.clone(),
            source_reference: None,
            source_revision: None,
            source_tree: None,
            cardinality: None,
            text_artifact: Some(prior_artifact),
        },
    );
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("prior index digest"),
    );
    std::fs::write(
        store.path().join(ACTIVE_POINTER_FILE),
        serde_json::to_vec(&pointer).expect("serialize prior pointer"),
    )
    .expect("write prior pointer");

    let descriptor = text_artifact(&active.id, 32, MAX_DURABLE_GENERATION_INDEX_BYTES_V1 / 2);
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: active.file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("sealed digest"),
        size_bytes: active.size_bytes,
    };
    let lock = acquire_code_generation_store_lock(store.path()).expect("generation store lock");

    let updated = attach_verified_text_artifact_under_lock(
        &lock,
        &pointer,
        &sealed_identity,
        descriptor.clone(),
    )
    .expect("attach active text artifact under byte pressure");
    drop(lock);

    assert!(updated.generation_index_truncated);
    assert_eq!(updated.generation_index.len(), 1);
    assert_eq!(
        updated.generation_index[0].generation_id,
        active.id.as_str()
    );
    assert_eq!(updated.generation_index[0].text_artifact, Some(descriptor));
    assert_eq!(
        read_active_pointer(store.path()).expect("durable pointer"),
        updated
    );
}

#[test]
fn verified_text_artifact_withdrawal_is_exact_durable_and_idempotent() {
    let (store, generations) = fixture_store(1);
    let expected = read_active_pointer(store.path()).expect("active pointer");
    let active = generations.last().expect("active generation");
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: active.file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("sealed digest"),
        size_bytes: active.size_bytes,
    };
    let descriptor = text_artifact(&active.id, 11, 4096);
    let lock = acquire_code_generation_store_lock(store.path()).expect("generation store lock");
    let attached = attach_verified_text_artifact_under_lock(
        &lock,
        &expected,
        &sealed_identity,
        descriptor.clone(),
    )
    .expect("attach verified artifact");

    let withdrawn = withdraw_verified_text_artifact_under_lock(&lock, &attached, &descriptor)
        .expect("withdraw exact artifact");
    let repeated = withdraw_verified_text_artifact_under_lock(&lock, &withdrawn, &descriptor)
        .expect("repeat exact withdrawal");
    drop(lock);

    assert_eq!(repeated, withdrawn);
    assert_eq!(withdrawn.generation_index[0].text_artifact, None);
    assert_eq!(
        read_active_pointer(store.path()).expect("durable pointer"),
        withdrawn
    );
}

#[test]
fn text_artifact_attachment_refuses_a_stale_pointer_without_mutation() {
    let (store, generations) = fixture_store(1);
    let durable_before =
        std::fs::read(store.path().join(ACTIVE_POINTER_FILE)).expect("durable pointer bytes");
    let mut stale = read_active_pointer(store.path()).expect("active pointer");
    stale.publication_digest = "sha256:stale".to_owned();
    let active = generations.last().expect("active generation");
    let sealed_identity = DurableSealedCodeGenerationIdentityV1 {
        locator: active.file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("sealed digest"),
        size_bytes: active.size_bytes,
    };
    let lock = acquire_code_generation_store_lock(store.path()).expect("generation store lock");

    let error = attach_verified_text_artifact_under_lock(
        &lock,
        &stale,
        &sealed_identity,
        text_artifact(&active.id, 9, 4096),
    )
    .expect_err("stale pointer must lose the attachment CAS");
    drop(lock);

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Conflict(_)));
    assert_eq!(
        std::fs::read(store.path().join(ACTIVE_POINTER_FILE)).expect("unchanged durable pointer"),
        durable_before
    );
}

#[test]
fn text_artifact_retention_preserves_references_and_collects_orphans() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let referenced = attach_fixture_text_artifact(&store, active, b"durably referenced");
    let artifacts_root = code_text_artifacts_root(store.path());

    let orphan = text_artifact_for_bytes(&active.id, b"unreferenced completed bytes");
    let orphan_path = write_text_artifact(&store, &orphan, b"unreferenced completed bytes");
    let staging_name = format!(".text-artifact-{}.staging", "a".repeat(64));
    let staging_path = artifacts_root.join(&staging_name);
    std::fs::write(&staging_path, b"abandoned staging").expect("write stale staging");
    let active_staging_name = format!(
        ".text-artifact-{}.staging",
        active
            .state_digest
            .strip_prefix("sha256:")
            .expect("active sealed digest")
    );
    let active_staging_path = artifacts_root.join(&active_staging_name);
    std::fs::write(&active_staging_path, b"resumable active staging")
        .expect("write active staging");
    let corrupt_name = format!("text-artifact-{}.corrupt-incident", "b".repeat(64));
    let corrupt_path = artifacts_root.join(&corrupt_name);
    std::fs::write(&corrupt_path, b"corrupt backup").expect("write corrupt backup");

    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan artifact retention");
    assert!(plan.collectable_generations.is_empty());
    assert_eq!(plan.collectable_text_artifacts.len(), 3);
    assert!(plan.has_collectable_work());
    assert_eq!(
        total_text_artifact_bytes(&plan.collectable_text_artifacts),
        std::fs::metadata(&orphan_path)
            .expect("orphan metadata")
            .len()
            .saturating_add(
                std::fs::metadata(&staging_path)
                    .expect("staging metadata")
                    .len(),
            )
            .saturating_add(
                std::fs::metadata(&corrupt_path)
                    .expect("corrupt metadata")
                    .len(),
            )
    );
    assert_eq!(
        plan.text_artifact_inventory_bytes,
        std::fs::metadata(artifacts_root.join(&referenced.artifact_file),)
            .expect("referenced metadata")
            .len()
            .saturating_add(
                std::fs::metadata(&active_staging_path)
                    .expect("active staging metadata")
                    .len(),
            )
            .saturating_add(total_text_artifact_bytes(&plan.collectable_text_artifacts)),
        "the bounded inventory accounts descriptor bytes, resumable staging, and each candidate once"
    );

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(10),
        None,
    )
    .expect("collect artifact debris");
    assert_eq!(report.deleted_generations.len(), 0);
    assert_eq!(report.deleted_text_artifacts.len(), 3);
    let receipt = report
        .text_artifact_receipt
        .expect("durable artifact retention receipt");
    assert_eq!(receipt.deleted_artifacts.len(), 3);
    assert_eq!(
        receipt.reclaimed_bytes,
        total_text_artifact_bytes(&receipt.deleted_artifacts),
        "each receipt candidate contributes its exact bytes once"
    );
    assert_eq!(
        receipt.inventory_bytes_before_collection,
        std::fs::metadata(artifacts_root.join(&referenced.artifact_file),)
            .expect("referenced metadata after collection")
            .len()
            .saturating_add(
                std::fs::metadata(&active_staging_path)
                    .expect("active staging metadata after collection")
                    .len(),
            )
            .saturating_add(receipt.reclaimed_bytes),
        "the durable receipt binds the pre-collection unique inventory bytes"
    );
    assert!(
        artifacts_root.join(&referenced.artifact_file).is_file(),
        "durable descriptor target must survive retention"
    );
    assert!(!orphan_path.exists());
    assert!(!staging_path.exists());
    assert!(!corrupt_path.exists());
    assert!(
        active_staging_path.is_file(),
        "only the active generation's resumable staging evidence is preserved"
    );
}

#[test]
fn text_artifact_retention_collects_empty_publish_crash_placeholder() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let referenced = attach_fixture_text_artifact(&store, active, b"durably referenced");
    let artifacts_root = code_text_artifacts_root(store.path());
    let placeholder_name = format!("text-artifact-{}.bin", "5".repeat(64));
    let placeholder_path = artifacts_root.join(&placeholder_name);
    std::fs::write(&placeholder_path, []).expect("write interrupted publish placeholder");

    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("an empty daemon-owned publish placeholder is collectable crash debris");
    assert_eq!(plan.collectable_text_artifacts.len(), 1);
    assert_eq!(
        plan.collectable_text_artifacts[0].artifact_file,
        placeholder_name
    );
    assert_eq!(plan.collectable_text_artifacts[0].size_bytes, 0);

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(11),
        None,
    )
    .expect("collect interrupted publish placeholder");
    assert_eq!(report.deleted_text_artifacts.len(), 1);
    assert!(!placeholder_path.exists());
    assert!(
        artifacts_root.join(referenced.artifact_file).is_file(),
        "the exact referenced artifact must survive placeholder recovery"
    );
}

#[test]
fn text_artifact_retention_collects_staging_database_sidecars_with_their_owner() {
    let (store, _generations) = fixture_store(1);
    let artifacts_root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts_root).expect("create artifact root");
    let staging_name = format!(".text-artifact-{}.staging", "c".repeat(64));
    let paths = [
        artifacts_root.join(&staging_name),
        artifacts_root.join(format!("{staging_name}-journal")),
        artifacts_root.join(format!("{staging_name}-wal")),
        artifacts_root.join(format!("{staging_name}-shm")),
    ];
    for (path, bytes) in paths
        .iter()
        .zip([b"stage".as_slice(), b"journal", b"wal", b"shm"])
    {
        std::fs::write(path, bytes).expect("write staging database evidence");
    }

    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan staging database retention");
    assert_eq!(plan.collectable_text_artifacts.len(), 4);

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(11),
        None,
    )
    .expect("collect staging database and sidecars");

    assert_eq!(report.deleted_text_artifacts.len(), 4);
    assert!(
        paths.iter().all(|path| !path.exists()),
        "the staging database and every SQLite sidecar must be collected together"
    );
}

#[test]
fn cancellable_artifact_apply_stops_rehash_before_quarantine_and_retries() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let bytes = vec![b'x'; 3 * 64 * 1024];
    let orphan = text_artifact_for_bytes(&active.id, &bytes);
    let orphan_path = write_text_artifact(&store, &orphan, &bytes);
    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan fully verified artifact collection");
    assert_eq!(plan.collectable_text_artifacts.len(), 1);

    let checks = std::sync::atomic::AtomicUsize::new(0);
    let error = execute_code_generation_retention_cancellable(
        store.path(),
        plan.clone(),
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(15),
        None,
        &|| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 6,
    )
    .expect_err("cancellation must interrupt the under-lock artifact rehash");

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(
        checks.load(std::sync::atomic::Ordering::SeqCst) >= 7,
        "the cancellation fires after at least one rehash chunk"
    );
    assert_eq!(
        std::fs::read(&orphan_path).expect("cancelled candidate remains canonical"),
        bytes,
        "cancellation before rename must preserve the complete artifact"
    );
    assert!(
        !text_artifact_transaction_path(store.path()).exists(),
        "the rolled-back pre-receipt journal must not strand a retry"
    );
    assert!(
        !store.path().join(TEXT_ARTIFACT_RECEIPTS_DIRECTORY).exists(),
        "cancellation during rehash must not publish a deletion receipt"
    );

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(16),
        None,
    )
    .expect("the unchanged plan retries after cancellation");
    assert_eq!(report.deleted_text_artifacts.len(), 1);
    assert!(
        !orphan_path.exists(),
        "only the successful retry may remove the candidate"
    );
    assert!(report.text_artifact_receipt.is_some());
}

#[test]
fn text_artifact_retention_uses_bounded_restartable_batches() {
    let (store, _generations) = fixture_store(1);
    let artifacts_root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts_root).expect("create artifact root");
    for sequence in 0..(MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1 + 2) {
        let path = artifacts_root.join(format!("text-artifact-{sequence:064x}.corrupt-restart"));
        std::fs::write(path, [u8::try_from(sequence).expect("small sequence")])
            .expect("write corrupt backup");
    }

    let first = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("first bounded page");
    assert_eq!(
        first.collectable_text_artifacts.len(),
        MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1
    );
    execute_code_generation_retention(
        store.path(),
        first,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(11),
        None,
    )
    .expect("apply first bounded page");

    let second = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("restart from the next artifact page");
    assert_eq!(second.collectable_text_artifacts.len(), 2);
    execute_code_generation_retention(
        store.path(),
        second,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(12),
        None,
    )
    .expect("apply second bounded page");
    assert!(
        std::fs::read_dir(&artifacts_root)
            .expect("read empty artifact root")
            .next()
            .is_none(),
        "a resumed page must reach later orphan artifacts"
    );
}

#[test]
fn text_artifact_inventory_honors_cancellation_before_marking_or_mutation() {
    let (store, _generations) = fixture_store(1);
    let artifacts_root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts_root).expect("create artifact root");
    let orphan = artifacts_root.join(format!("text-artifact-{}.corrupt-cancel", "c".repeat(64)));
    std::fs::write(&orphan, b"uncollected").expect("write artifact debris");
    let pointer = read_active_pointer(store.path()).expect("pointer");
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let error = plan_collectable_text_artifacts_cancellable(
        store.path(),
        Some(&pointer),
        GenerationDigestVerificationV1::Full,
        &|| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0,
    )
    .expect_err("cancellation must stop the bounded inventory");
    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(
        orphan.is_file(),
        "planning cancellation must not mutate debris"
    );
}

#[test]
fn text_artifact_retention_refuses_tamper_and_publish_cas_movement() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let descriptor = attach_fixture_text_artifact(&store, active, b"expected artifact bytes");
    let artifact_path = code_text_artifact_path(store.path(), &descriptor).expect("artifact path");
    std::fs::write(&artifact_path, b"tampered artifact bytes").expect("tamper artifact");
    let error = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect_err("a referenced tampered artifact must fail closed");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));

    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let orphan = text_artifact_for_bytes(&active.id, b"candidate before publish");
    let orphan_path = write_text_artifact(&store, &orphan, b"candidate before publish");
    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan orphan before concurrent publish");
    let mut moved = read_active_pointer(store.path()).expect("pointer before movement");
    moved.publication_digest = "sha256:concurrent-publish".to_owned();
    std::fs::write(
        store.path().join(ACTIVE_POINTER_FILE),
        serde_json::to_vec(&moved).expect("serialize moved pointer"),
    )
    .expect("advance pointer");
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(13),
        None,
    )
    .expect_err("a moved publication pointer must lose the artifact sweep CAS");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        orphan_path.is_file(),
        "CAS refusal must preserve candidate bytes"
    );
}

#[cfg(unix)]
#[test]
fn text_artifact_retention_refuses_symlinked_inventory_entries() {
    let (store, _generations) = fixture_store(1);
    let artifacts_root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts_root).expect("create artifact root");
    let outside = tempfile::NamedTempFile::new().expect("outside artifact");
    let symlink = artifacts_root.join(format!("text-artifact-{}.bin", "d".repeat(64)));
    std::os::unix::fs::symlink(outside.path(), &symlink).expect("create symlink");
    let error = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect_err("symlinked artifact entry must fail closed");
    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        symlink.exists(),
        "refusal must preserve foreign link evidence"
    );
}

#[test]
fn text_artifact_recovery_rolls_back_before_receipt_and_commits_after_receipt() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let orphan = text_artifact_for_bytes(&active.id, b"recoverable orphan");
    let orphan_path = write_text_artifact(&store, &orphan, b"recoverable orphan");
    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan artifact transaction");
    let receipt = build_text_artifact_receipt(
        &plan,
        plan.collectable_text_artifacts.clone(),
        UtcMicros(14),
    )
    .expect("build artifact receipt");
    let transaction = CodeTextArtifactRetentionTransactionV1 {
        schema: TEXT_ARTIFACT_TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };
    persist_text_artifact_transaction(store.path(), &transaction)
        .expect("journal artifact retention");
    stage_collectable_text_artifacts(store.path(), &transaction).expect("quarantine artifact");
    assert!(!orphan_path.exists());
    recover_code_generation_retention(store.path(), &BTreeSet::new(), None)
        .expect("rollback an uncommitted artifact transaction");
    assert!(
        orphan_path.is_file(),
        "uncommitted artifact staging must roll back"
    );

    persist_text_artifact_transaction(store.path(), &transaction)
        .expect("journal second transaction");
    stage_collectable_text_artifacts(store.path(), &transaction)
        .expect("quarantine second artifact");
    write_text_artifact_receipt(store.path(), &receipt).expect("durably commit artifact receipt");
    recover_code_generation_retention(store.path(), &BTreeSet::new(), None)
        .expect("finish a committed artifact transaction");
    assert!(
        !orphan_path.exists(),
        "durable receipt recovery must finish deletion"
    );
    assert!(
        !text_artifact_transaction_path(store.path()).exists(),
        "recovery must clear the committed artifact journal"
    );
}

#[test]
fn cancellable_recovery_preserves_pending_artifact_journal_for_retry() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let orphan = text_artifact_for_bytes(&active.id, b"recover after cancellation");
    let orphan_path = write_text_artifact(&store, &orphan, b"recover after cancellation");
    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan artifact transaction");
    let receipt = build_text_artifact_receipt(
        &plan,
        plan.collectable_text_artifacts.clone(),
        UtcMicros(17),
    )
    .expect("build artifact receipt");
    let transaction = CodeTextArtifactRetentionTransactionV1 {
        schema: TEXT_ARTIFACT_TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt,
    };
    persist_text_artifact_transaction(store.path(), &transaction)
        .expect("journal artifact retention");
    stage_collectable_text_artifacts(store.path(), &transaction)
        .expect("quarantine uncommitted candidate");
    assert!(!orphan_path.exists());

    let error = recover_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        None,
        &|| true,
    )
    .expect_err("cancelled recovery must preserve its durable journal");
    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(
        text_artifact_transaction_path(store.path()).is_file(),
        "a cancelled recovery leaves the transaction resumable"
    );
    assert!(
        !orphan_path.exists(),
        "cancelled recovery performs no rollback"
    );

    recover_code_generation_retention(store.path(), &BTreeSet::new(), None)
        .expect("the next recovery resumes the pending rollback");
    assert!(orphan_path.is_file());
    assert!(!text_artifact_transaction_path(store.path()).exists());
}

fn pad_generation_file(
    store: &tempfile::TempDir,
    generation: &mut FixtureGeneration,
    padding_bytes: usize,
    active: bool,
) {
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let old_path = generations_root.join(&generation.file);
    let mut bytes = std::fs::read(&old_path).expect("read generation fixture");
    bytes.extend(std::iter::repeat_n(b' ', padding_bytes));
    let state_digest = encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&bytes));
    let file = format!(
        "generation-{}.json",
        state_digest.strip_prefix("sha256:").expect("digest prefix")
    );
    let size_bytes = u64::try_from(bytes.len()).expect("fixture size fits u64");
    std::fs::write(generations_root.join(&file), bytes).expect("write padded generation");
    std::fs::remove_file(old_path).expect("remove unpadded generation");
    generation.file = file.clone();
    generation.state_digest = state_digest.clone();
    generation.size_bytes = size_bytes;
    if active {
        let pointer_path = store.path().join(ACTIVE_POINTER_FILE);
        let mut pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
                .expect("decode active pointer");
        pointer.generation_file = file.clone();
        pointer.state_digest = state_digest.clone();
        for entry in &mut pointer.generation_index {
            if entry.generation_id == generation.id.as_str() {
                entry.generation_file = file.clone();
                entry.state_digest = state_digest.clone();
                entry.size_bytes = size_bytes;
            }
        }
        pointer.generation_index_digest = Some(
            durable_generation_index_digest(
                &pointer.generation_index,
                pointer.generation_index_truncated,
            )
            .expect("index digest"),
        );
        std::fs::write(
            pointer_path,
            serde_json::to_vec(&pointer).expect("serialize active pointer"),
        )
        .expect("write active pointer");
    }
}

#[test]
fn next_retention_plan_limits_collection_to_one_generation() {
    let (store, _generations) = fixture_store(8);

    let plan = plan_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
    )
    .expect("plan one retention unit");

    assert_eq!(plan.collectable_generations.len(), 1);
    assert_eq!(plan.superseded_generations.len(), 7);
}

#[test]
fn unpublished_store_collects_sealed_crash_debris_under_the_absent_pointer() {
    let (store, generations) = fixture_store(2);
    std::fs::remove_file(store.path().join(ACTIVE_POINTER_FILE))
        .expect("remove publication pointer to model an interrupted first publish");

    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), 0)
        .expect("plan unpublished crash-debris retention");
    assert_eq!(plan.collectable_generations.len(), 2);

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(98),
        None,
    )
    .expect("collect unpublished sealed generations");

    assert_eq!(report.deleted_generations.len(), 2);
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    assert!(
        generations
            .iter()
            .all(|generation| { !generations_root.join(&generation.file).exists() })
    );
    assert!(
        !store.path().join(ACTIVE_POINTER_FILE).exists(),
        "retention must not fabricate a publication pointer"
    );
}

#[test]
fn idle_maintenance_preparation_stays_metadata_only() {
    let (store, _generations) = fixture_store(1);

    let plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        None,
    )
    .expect("prepare idle retention census");

    assert!(!plan.has_collectable_work());
    assert_eq!(
        plan.verification,
        GenerationDigestVerificationV1::MetadataOnly,
        "an idle maintenance tick must not re-hash every retained generation"
    );
}

#[test]
fn metadata_only_segment_census_observes_at_most_one_directory_entry() {
    let store = tempfile::TempDir::new().expect("create unpublished store");
    std::fs::create_dir_all(store.path().join(GENERATIONS_DIRECTORY))
        .expect("create generation root");
    let segments_root = store.path().join(GENERATION_SEGMENTS_DIRECTORY);
    std::fs::create_dir_all(&segments_root).expect("create segment root");
    for index in 0..256 {
        std::fs::write(
            segments_root.join(format!("crash-debris-{index:04}")),
            b"debris",
        )
        .expect("write crash debris");
    }

    let cancellation_observations = std::cell::Cell::new(0_usize);
    let holds_segments = store_holds_generation_segments(store.path(), &|| {
        cancellation_observations.set(cancellation_observations.get() + 1);
        cancellation_observations.get() > 1
    })
    .expect("one observed entry conservatively proves possible segment work");

    assert!(holds_segments);
    assert_eq!(
        cancellation_observations.get(),
        1,
        "metadata-only diagnostics must not exhaust an arbitrarily large debris directory"
    );
    let plan = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("plan bounded metadata-only census");
    assert_eq!(
        plan.generation_segment_census(),
        GenerationSegmentCensusV1::Unknown,
        "any observed entry remains typed unknown until the full mark-and-sweep"
    );
}

#[test]
fn metadata_only_segment_census_distinguishes_empty_and_cancelled() {
    let store = tempfile::TempDir::new().expect("create unpublished store");
    let segments_root = store.path().join(GENERATION_SEGMENTS_DIRECTORY);
    std::fs::create_dir_all(&segments_root).expect("create segment root");

    assert!(
        !store_holds_generation_segments(store.path(), &|| false).expect("empty segment directory")
    );

    std::fs::write(segments_root.join("crash-debris"), b"debris").expect("write crash debris");
    let error = store_holds_generation_segments(store.path(), &|| true)
        .expect_err("cancellation wins before an observed entry is accepted");
    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
}

#[test]
fn maintenance_preparation_wakes_for_an_unreferenced_final_segment() {
    let store = tempfile::TempDir::new().expect("create unpublished store");
    std::fs::create_dir_all(store.path().join(GENERATIONS_DIRECTORY))
        .expect("create generation root");
    let segments_root = store.path().join(GENERATION_SEGMENTS_DIRECTORY);
    std::fs::create_dir_all(&segments_root).expect("create segment root");
    let orphan_bytes = b"pack committed before generation manifest";
    let orphan_digest = encode_lowercase_hex(&Sha256::digest(orphan_bytes));
    let orphan_path = segments_root.join(format!("segment-{orphan_digest}.json"));
    std::fs::write(&orphan_path, orphan_bytes).expect("write final orphan segment");

    let plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        None,
    )
    .expect("prepare segment-only retention unit");
    assert!(plan.has_collectable_work());
    assert!(plan.collectable_generations.is_empty());
    assert_eq!(
        plan.verification,
        GenerationDigestVerificationV1::Full,
        "segment unlinking must keep the full-verification execution fence"
    );

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(99),
        None,
    )
    .expect("execute segment-only retention unit");
    assert!(report.deleted_generations.is_empty());
    assert!(report.receipt.is_none());
    assert!(
        !orphan_path.exists(),
        "ordinary maintenance must remove the segment without waiting for a generation deletion"
    );
}

#[test]
fn collectable_maintenance_preparation_escalates_to_full_verification() {
    let (store, _generations) = fixture_store(8);

    let plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
        None,
    )
    .expect("prepare collectable retention unit");

    assert!(plan.has_collectable_work());
    assert_eq!(plan.collectable_generations.len(), 1);
    assert_eq!(plan.verification, GenerationDigestVerificationV1::Full);
}

#[test]
fn cancellable_maintenance_preparation_stops_during_generation_verification() {
    let (store, mut generations) = fixture_store(8);
    pad_generation_file(&store, &mut generations[7], 3 * 1024 * 1024, true);
    let checks = std::sync::atomic::AtomicUsize::new(0);

    let error = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2,
        None,
    )
    .expect_err("cancellation must interrupt full-file verification");

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
    assert!(checks.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    assert!(!transaction_path(store.path()).exists());
    assert!(!store.path().join(RECEIPTS_DIRECTORY).exists());
}

#[test]
fn executing_a_prevalidated_unit_collects_only_that_generation() {
    let (store, _generations) = fixture_store(8);
    let plan = plan_next_code_generation_retention_cancellable(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        &|| false,
    )
    .expect("plan one retention unit");

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(99),
        None,
    )
    .expect("execute one retention unit");

    assert_eq!(report.deleted_generations.len(), 1);
    assert_eq!(
        std::fs::read_dir(store.path().join(GENERATIONS_DIRECTORY))
            .expect("generation directory")
            .count(),
        7
    );
}

#[test]
fn apply_preserves_collectable_generations_when_receipt_commit_fails() {
    let (store, _generations) = fixture_store(5);
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    assert_eq!(plan.collectable_generations.len(), 1);
    let collectable = plan.collectable_generations[0].clone();
    let active_file = plan
        .active_generation_file()
        .expect("published fixture has an active generation")
        .to_owned();
    let rollback_files = plan
        .superseded_generations
        .iter()
        .take(plan.rollback_floor)
        .map(|generation| generation.generation_file.clone())
        .collect::<Vec<_>>();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);

    std::fs::write(store.path().join(RECEIPTS_DIRECTORY), b"not a directory")
        .expect("block receipt directory");
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(100),
        None,
    )
    .expect_err("receipt commit must fail");

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Storage(_)));
    assert!(
        generations_root
            .join(&collectable.generation_file)
            .is_file(),
        "a failed receipt commit must not unlink collectable evidence"
    );
    assert!(
        generations_root.join(active_file).is_file(),
        "retention must preserve the active generation"
    );
    for rollback_file in rollback_files {
        assert!(
            generations_root.join(rollback_file).is_file(),
            "retention must preserve the rollback floor"
        );
    }
}

#[test]
fn recovery_restores_quarantined_generations_without_a_durable_receipt() {
    let (store, _generations) = fixture_store(5);
    let vector_readable_sources = BTreeSet::new();
    let plan =
        plan_code_generation_retention(store.path(), &vector_readable_sources, TEST_ROLLBACK_FLOOR)
            .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let receipt = build_receipt(&plan, plan.collectable_generations.clone(), UtcMicros(101))
        .expect("build retention receipt");
    let transaction = CodeGenerationRetentionTransactionV1 {
        schema: TRANSACTION_SCHEMA.to_owned(),
        active_pointer: plan.active_pointer.clone(),
        receipt: receipt.clone(),
    };
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let staged_root = transaction_stage_root(store.path(), &receipt);

    persist_transaction(store.path(), &transaction).expect("persist transaction journal");
    stage_collectable_generations(store.path(), &transaction).expect("stage generation");
    assert!(!generations_root.join(&collectable.generation_file).exists());
    assert!(staged_root.join(&collectable.generation_file).is_file());

    recover_code_generation_retention(store.path(), &vector_readable_sources, None)
        .expect("recover uncommitted transaction");

    assert!(
        generations_root
            .join(&collectable.generation_file)
            .is_file()
    );
    assert!(!transaction_path(store.path()).exists());
    assert!(!staged_root.exists());
}

#[test]
fn apply_retires_collectable_generations_into_the_graph_replay_pool() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    assert_eq!(plan.collectable_generations.len(), 1);
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let source_bytes = std::fs::read(generations_root.join(&collectable.generation_file))
        .expect("read collectable bytes");

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(102),
        Some(&pool_root),
    )
    .expect("apply retention");

    assert_eq!(report.deleted_generations.len(), 1);
    assert!(!generations_root.join(&collectable.generation_file).exists());
    assert_eq!(
        std::fs::read(pool_root.join(&collectable.generation_file))
            .expect("retired generation survives in the graph replay pool"),
        source_bytes,
    );
    let queued_releases =
        std::fs::read_dir(store.path().join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY))
            .expect("release queue exists")
            .count();
    assert_eq!(
        queued_releases, 1,
        "the retired generation's release event is queued for the graph reconciler"
    );
}

#[test]
fn failed_receipt_commit_withdraws_the_graph_replay_pool_exposure() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);

    std::fs::write(store.path().join(RECEIPTS_DIRECTORY), b"not a directory")
        .expect("block receipt directory");
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(103),
        Some(&pool_root),
    )
    .expect_err("receipt commit must fail");

    assert!(matches!(error, CodeGenerationRetentionErrorV1::Storage(_)));
    assert!(
        generations_root
            .join(&collectable.generation_file)
            .is_file(),
        "rollback must restore the canonical generation"
    );
    assert!(
        !pool_root.join(&collectable.generation_file).exists(),
        "rollback must withdraw the graph replay pool exposure"
    );
    let queued_releases = store.path().join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY);
    let queued = std::fs::read_dir(&queued_releases)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(queued, 0, "rollback must remove the queued release events");
}

fn queued_release_count(store_root: &Path) -> usize {
    std::fs::read_dir(store_root.join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn retention_refuses_a_corrupt_same_name_graph_replay_pool_entry() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    tracedecay_private_fs::create_private_directory(&pool_root).expect("create pool root");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let canonical_bytes = std::fs::read(generations_root.join(&collectable.generation_file))
        .expect("read collectable bytes");
    let mut corrupt_bytes = canonical_bytes.clone();
    corrupt_bytes[0] ^= 0x2a;
    std::fs::write(pool_root.join(&collectable.generation_file), &corrupt_bytes)
        .expect("pre-create corrupt same-name pool entry");

    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(104),
        Some(&pool_root),
    )
    .expect_err("a corrupt same-name pool entry must fail retention closed");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        !store.path().join(RECEIPTS_DIRECTORY).exists(),
        "no deletion receipt may be published over unusable pool evidence"
    );
    assert_eq!(
        queued_release_count(store.path()),
        0,
        "no release event may be published over unusable pool evidence"
    );
    assert_eq!(
        std::fs::read(generations_root.join(&collectable.generation_file))
            .expect("canonical generation bytes survive the refused retention"),
        canonical_bytes,
    );
    assert_eq!(
        std::fs::read(pool_root.join(&collectable.generation_file))
            .expect("foreign pool entry is left in place"),
        corrupt_bytes,
    );
    assert!(!transaction_path(store.path()).exists());
}

#[test]
fn retention_refuses_a_directory_graph_replay_pool_entry() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let canonical_bytes = std::fs::read(generations_root.join(&collectable.generation_file))
        .expect("read collectable bytes");
    tracedecay_private_fs::create_private_directory(&pool_root).expect("create pool root");
    std::fs::create_dir(pool_root.join(&collectable.generation_file))
        .expect("pre-create directory at the pool path");

    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(105),
        Some(&pool_root),
    )
    .expect_err("a directory at the pool path must fail retention closed");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        !store.path().join(RECEIPTS_DIRECTORY).exists(),
        "no deletion receipt may be published over unusable pool evidence"
    );
    assert_eq!(queued_release_count(store.path()), 0);
    assert_eq!(
        std::fs::read(generations_root.join(&collectable.generation_file))
            .expect("canonical generation bytes survive the refused retention"),
        canonical_bytes,
    );
    assert!(
        pool_root.join(&collectable.generation_file).is_dir(),
        "the foreign directory is left in place"
    );
    assert!(!transaction_path(store.path()).exists());
}

#[cfg(unix)]
#[test]
fn retention_refuses_a_symlink_graph_replay_pool_entry() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    tracedecay_private_fs::create_private_directory(&pool_root).expect("create pool root");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let canonical_bytes = std::fs::read(generations_root.join(&collectable.generation_file))
        .expect("read collectable bytes");
    // The symlink resolves to the exact sealed bytes, so only the
    // non-regular identity check can refuse it.
    std::os::unix::fs::symlink(
        generations_root.join(&collectable.generation_file),
        pool_root.join(&collectable.generation_file),
    )
    .expect("pre-create symlink at the pool path");

    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(106),
        Some(&pool_root),
    )
    .expect_err("a symlink at the pool path must fail retention closed");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(
        !store.path().join(RECEIPTS_DIRECTORY).exists(),
        "no deletion receipt may be published over unusable pool evidence"
    );
    assert_eq!(queued_release_count(store.path()), 0);
    assert_eq!(
        std::fs::read(generations_root.join(&collectable.generation_file))
            .expect("canonical generation bytes survive the refused retention"),
        canonical_bytes,
    );
    assert!(
        pool_root
            .join(&collectable.generation_file)
            .symlink_metadata()
            .expect("foreign symlink is left in place")
            .file_type()
            .is_symlink()
    );
    assert!(!transaction_path(store.path()).exists());
}

#[test]
fn retention_accepts_an_identical_existing_graph_replay_pool_entry() {
    let (store, _generations) = fixture_store(5);
    let pool_root = store.path().join("graph-replay-pool");
    tracedecay_private_fs::create_private_directory(&pool_root).expect("create pool root");
    let plan = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("plan retention");
    let collectable = plan.collectable_generations[0].clone();
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    let canonical_bytes = std::fs::read(generations_root.join(&collectable.generation_file))
        .expect("read collectable bytes");
    // A distinct-inode copy with identical bytes is what the graph's
    // eager seal staging installs; it must be accepted, not refused.
    std::fs::write(
        pool_root.join(&collectable.generation_file),
        &canonical_bytes,
    )
    .expect("pre-create identical pool entry");

    let report = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(107),
        Some(&pool_root),
    )
    .expect("identical pool collision completes retention");

    assert_eq!(report.deleted_generations.len(), 1);
    assert!(!generations_root.join(&collectable.generation_file).exists());
    assert_eq!(
        std::fs::read(pool_root.join(&collectable.generation_file))
            .expect("pool entry survives retention"),
        canonical_bytes,
    );
    assert_eq!(queued_release_count(store.path()), 1);
}

#[test]
fn plan_keeps_active_vector_pinned_and_rollback_generations() {
    let (store, generations) = fixture_store(7);
    let vector_readable_sources = [generations[0].id.clone()].into_iter().collect();

    let plan =
        plan_code_generation_retention(store.path(), &vector_readable_sources, TEST_ROLLBACK_FLOOR)
            .expect("plan retention");

    assert_eq!(plan.active_generation_id, Some(generations[6].id.clone()));
    assert!(
        plan.collectable_generations
            .iter()
            .all(|generation| generation.generation_id != generations[0].id),
        "a vector-readable generation remains pinned even outside the rollback floor"
    );
    let collectable_ids = plan
        .collectable_generations
        .iter()
        .map(|generation| generation.generation_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        collectable_ids,
        [generations[1].id.clone(), generations[2].id.clone()]
            .into_iter()
            .collect()
    );
}

// --- Scope-root reconciliation -----------------------------------------

const LIVE_ROOT: &str = "/repos/live-checkout";
const STRANDED_ROOT: &str = "/repos/.claude/worktrees/agent-deleted";
const AGED_NOW_SECS: i64 = 4_000_000_000;

/// A `code-index-v1/` parent holding one live scope and one stranded scope,
/// each with a payload file so the census has bytes to measure.
fn fixture_scope_store() -> (tempfile::TempDir, String, String) {
    let store = tempfile::TempDir::new().expect("create code-index store");
    let mut hashes = Vec::new();
    for (root, payload) in [(LIVE_ROOT, "live"), (STRANDED_ROOT, "stranded")] {
        let hash = code_index_scope_hash(Path::new(root));
        let scope = store.path().join(&hash);
        std::fs::create_dir_all(scope.join(GENERATIONS_DIRECTORY))
            .expect("create scope generations directory");
        std::fs::write(
            scope.join(GENERATIONS_DIRECTORY).join("generation-fixture"),
            payload.as_bytes(),
        )
        .expect("write scope payload");
        hashes.push(hash);
    }
    let (live, stranded) = (hashes[0].clone(), hashes[1].clone());
    (store, live, stranded)
}

fn live_root_set() -> BTreeSet<PathBuf> {
    [PathBuf::from(LIVE_ROOT)].into_iter().collect()
}

fn authority_receipt(
    revision: &str,
    terminal_count: u64,
    digest_byte: char,
) -> ScopeRootAuthorityReceiptV1 {
    ScopeRootAuthorityReceiptV1 {
        revision: revision.to_owned(),
        terminal_count,
        digest: format!("sha256:{}", digest_byte.to_string().repeat(64)),
    }
}

fn fixture_scope_liveness_proof(
    live_scope_hash: String,
    candidate_scope_hash: String,
) -> ScopeRootLivenessProofV1 {
    let source_scope = tracedecay_store::StoreShardIdV1::project(
        tracedecay_domain::BrainId::new("brain.scope-retention").expect("fixture brain"),
        tracedecay_domain::UserProfileId::new("profile.scope-retention").expect("fixture profile"),
        tracedecay_domain::ProjectId::new("project.scope-retention").expect("fixture project"),
    );
    ScopeRootLivenessProofV1::new(
        [live_scope_hash].into_iter().collect(),
        authority_receipt("registry-r1", 1, '1'),
        authority_receipt("git-r1", 1, '2'),
        authority_receipt("mount-r1", 1, '3'),
        authority_receipt("config-r1", 1, '4'),
        authority_receipt("vector-r1", 2, '5'),
        authority_receipt("dependency-r1", 1, '6'),
        ScopeRootCandidateBindingV1 {
            scope_hash: candidate_scope_hash,
            source_scope,
            vector_census_revision: "vector-r1".to_owned(),
            live: false,
        },
    )
    .expect("valid fixture liveness proof")
}

#[test]
fn scope_apply_refuses_a_changed_terminal_authority_receipt() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live, stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof.clone(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan proof-bound scope reconciliation");
    let completed_at = UtcMicros(10);
    prepare_scope_root_binding_cleanup(
        store.path(),
        &plan,
        &stranded,
        &proof.candidate_binding.source_scope,
        &proof,
        completed_at,
    )
    .expect("persist exact proof-bound cleanup intent");
    let mut changed = proof;
    changed.mounted_leases.revision = "mount-r2".to_owned();
    changed
        .refresh_digest()
        .expect("refresh changed proof digest");

    let error = execute_scope_root_retention(
        store.path(),
        plan,
        &changed,
        CodeGenerationRetentionModeV1::Apply,
        AGED_NOW_SECS,
        completed_at,
    )
    .expect_err("pre-quarantine CAS must reject a changed root authority");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(store.path().join(stranded).is_dir());
    assert!(!scope_transaction_path(store.path()).exists());
}

#[test]
fn cleanup_replay_preserves_exact_source_shard_and_liveness_proof() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live, stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof.clone(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan proof-bound scope reconciliation");
    let completed_at = UtcMicros(11);
    prepare_scope_root_binding_cleanup(
        store.path(),
        &plan,
        &stranded,
        &proof.candidate_binding.source_scope,
        &proof,
        completed_at,
    )
    .expect("persist proof-bound cleanup intent");
    execute_scope_root_retention(
        store.path(),
        plan,
        &proof,
        CodeGenerationRetentionModeV1::Apply,
        AGED_NOW_SECS,
        completed_at,
    )
    .expect("collect proof-bound stranded scope");

    let replay = recover_scope_root_binding_cleanup(store.path())
        .expect("read cleanup replay")
        .expect("pending cleanup replay");
    assert_eq!(replay.scope_hash, stranded);
    assert_eq!(replay.source_scope, proof.candidate_binding.source_scope);
    assert_eq!(replay.liveness_proof, proof);
}

#[test]
fn scope_plan_refuses_an_unproven_live_root_set() {
    let (store, _live, stranded) = fixture_scope_store();

    let error = plan_scope_root_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect_err("an empty live-root set must never be interpreted");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(store.path().join(stranded).is_dir());
}

#[test]
fn scope_recovery_restores_quarantined_scopes_without_a_durable_receipt() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan scope reconciliation");
    assert_eq!(plan.collectable_scopes.len(), 1);
    assert_eq!(plan.collectable_scopes[0].scope_hash, stranded);

    let receipt = build_scope_receipt(&plan, plan.collectable_scopes.clone(), UtcMicros(11))
        .expect("build reconciliation receipt");
    let mut quarantine = ScopeQuarantineAuthority::prepare(
        store.path(),
        &receipt.receipt_digest,
        &receipt.collected_scopes,
    )
    .expect("open scope quarantine authority");
    let transaction = ScopeRootRetentionTransactionV1 {
        schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
        receipt: receipt.clone(),
        scope_identities: quarantine.scope_identities().clone(),
    };
    let staged_root = scope_stage_root(store.path(), &receipt);

    // Crash exactly between quarantine and the durable receipt.
    persist_scope_transaction(store.path(), &transaction).expect("persist journal");
    quarantine
        .stage(&transaction.receipt.collected_scopes)
        .expect("quarantine stranded scope");
    assert!(!store.path().join(&stranded).exists());
    assert!(staged_root.join(&stranded).is_dir());

    recover_scope_root_retention(store.path()).expect("recover uncommitted reconciliation");

    assert!(
        store.path().join(&stranded).is_dir(),
        "without a durable receipt the scope must come back intact"
    );
    assert!(store.path().join(&live).is_dir());
    assert!(!scope_transaction_path(store.path()).exists());
    assert!(!staged_root.exists());
}

#[test]
fn scope_recovery_completes_collection_once_the_receipt_is_durable() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan scope reconciliation");
    let receipt = build_scope_receipt(&plan, plan.collectable_scopes.clone(), UtcMicros(12))
        .expect("build reconciliation receipt");
    let mut quarantine = ScopeQuarantineAuthority::prepare(
        store.path(),
        &receipt.receipt_digest,
        &receipt.collected_scopes,
    )
    .expect("open scope quarantine authority");
    let transaction = ScopeRootRetentionTransactionV1 {
        schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
        receipt: receipt.clone(),
        scope_identities: quarantine.scope_identities().clone(),
    };
    let staged_root = scope_stage_root(store.path(), &receipt);

    // Crash after the receipt is durable but before the quarantine is
    // unlinked: the decision is committed, so recovery rolls forward.
    persist_scope_transaction(store.path(), &transaction).expect("persist journal");
    quarantine
        .stage(&transaction.receipt.collected_scopes)
        .expect("quarantine stranded scope");
    write_scope_receipt(store.path(), &receipt).expect("commit reconciliation receipt");

    recover_scope_root_retention(store.path()).expect("recover committed reconciliation");

    assert!(!store.path().join(&stranded).exists());
    assert!(!staged_root.exists());
    assert!(store.path().join(&live).is_dir());
    assert!(!scope_transaction_path(store.path()).exists());
    assert!(scope_receipt_path(store.path(), &receipt).is_file());
}

#[test]
fn scope_apply_refuses_collection_without_exact_binding_cleanup_intent() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live, stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof.clone(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan scope reconciliation");

    let error = execute_scope_root_retention(
        store.path(),
        plan,
        &proof,
        CodeGenerationRetentionModeV1::Apply,
        AGED_NOW_SECS,
        UtcMicros(13),
    )
    .expect_err("physical collection must require a durable relational cleanup intent");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    assert!(store.path().join(stranded).is_dir());
    assert!(!scope_transaction_path(store.path()).exists());
}

#[test]
fn scope_binding_cleanup_intent_replays_after_filesystem_collection_restart() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
    let plan = plan_scope_root_retention_with_liveness_proof(
        store.path(),
        proof.clone(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan scope reconciliation");
    let completed_at = UtcMicros(14);

    prepare_scope_root_binding_cleanup(
        store.path(),
        &plan,
        &stranded,
        &proof.candidate_binding.source_scope,
        &proof,
        completed_at,
    )
    .expect("journal relational cleanup before filesystem collection");
    let report = execute_scope_root_retention(
        store.path(),
        plan,
        &proof,
        CodeGenerationRetentionModeV1::Apply,
        AGED_NOW_SECS,
        completed_at,
    )
    .expect("complete filesystem collection");
    assert_eq!(report.collected_scopes[0].scope_hash, stranded);
    assert!(!store.path().join(&stranded).exists());
    assert!(store.path().join(&live).is_dir());

    // Simulate restart exactly after durable filesystem completion and
    // before the caller removes the semantic source-scope binding.
    recover_scope_root_retention(store.path()).expect("recover filesystem transaction");
    let replay = recover_scope_root_binding_cleanup(store.path())
        .expect("replay binding cleanup intent")
        .expect("pending replay");
    assert_eq!(replay.scope_hash, stranded);
    assert_eq!(replay.source_scope, proof.candidate_binding.source_scope);
    assert_eq!(replay.liveness_proof, proof);
    complete_scope_root_binding_cleanup(store.path(), &replay)
        .expect("complete exact binding cleanup intent");
    assert_eq!(
        recover_scope_root_binding_cleanup(store.path()).expect("completed cleanup stays complete"),
        None
    );
}

#[test]
fn scope_transaction_never_journals_a_live_scope() {
    let (store, live, stranded) = fixture_scope_store();
    let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
    let mut receipt = ScopeRootRetentionReceiptV1 {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
        receipt_digest: String::new(),
        live_scope_hashes: [live.clone()].into_iter().collect(),
        liveness_proof: proof,
        minimum_stranding_age_secs: DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        collected_scopes: vec![
            StrandedCodeIndexScopeV1 {
                scope_hash: stranded,
                size_bytes: 8,
                newest_mtime_secs: 1,
            },
            StrandedCodeIndexScopeV1 {
                scope_hash: live,
                size_bytes: 4,
                newest_mtime_secs: 1,
            },
        ],
        reclaimed_bytes: 12,
        completed_at_micros: 13,
    };
    receipt.receipt_digest =
        scope_receipt_digest(&receipt).expect("calculate malformed receipt digest");

    let quarantine = ScopeQuarantineAuthority::prepare(
        store.path(),
        &receipt.receipt_digest,
        &receipt.collected_scopes,
    )
    .expect("open scope quarantine authority");
    let error = validate_scope_transaction(&ScopeRootRetentionTransactionV1 {
        schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
        receipt,
        scope_identities: quarantine.scope_identities().clone(),
    })
    .expect_err("a live scope in the collected set must be rejected");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
}

#[test]
fn scope_reconciliation_never_treats_its_own_artifacts_as_scopes() {
    let (store, _live, _stranded) = fixture_scope_store();
    std::fs::create_dir_all(store.path().join(SCOPE_RETENTION_RECEIPTS_DIRECTORY))
        .expect("create receipts directory");
    std::fs::create_dir_all(store.path().join(SCOPE_RETENTION_QUARANTINE_DIRECTORY))
        .expect("create quarantine directory");

    let plan = plan_scope_root_retention(
        store.path(),
        &live_root_set(),
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        AGED_NOW_SECS,
    )
    .expect("plan scope reconciliation");

    assert_eq!(plan.collectable_scopes.len(), 1);
    assert_eq!(
        plan.unrecognized_entry_count, 2,
        "reconciliation's own directories are never candidates"
    );
}

#[test]
fn metadata_only_census_matches_full_verification() {
    let (store, generations) = fixture_store(5);
    let active = generations.last().expect("active generation");
    let referenced = attach_fixture_text_artifact(&store, active, b"metadata parity live");
    let orphan = text_artifact_for_bytes(&active.id, b"metadata parity orphan");
    write_text_artifact(&store, &orphan, b"metadata parity orphan");
    let stale_staging = code_text_artifacts_root(store.path())
        .join(format!(".text-artifact-{}.staging", "e".repeat(64)));
    std::fs::write(&stale_staging, b"metadata parity staging").expect("write stale staging");

    let full = plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
        .expect("full census");
    let metadata_only = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        TEST_ROLLBACK_FLOOR,
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("metadata-only census");

    assert_eq!(
        full.superseded_generations,
        metadata_only.superseded_generations
    );
    assert_eq!(
        full.collectable_generations,
        metadata_only.collectable_generations
    );
    assert_eq!(
        full.collectable_text_artifacts, metadata_only.collectable_text_artifacts,
        "metadata observation must identify the same bounded artifact debris"
    );
    assert_eq!(
        full.text_artifact_inventory_bytes, metadata_only.text_artifact_inventory_bytes,
        "both modes must account the same unique descriptor, staging, and candidate bytes"
    );
    assert!(
        code_text_artifacts_root(store.path())
            .join(&referenced.artifact_file)
            .is_file(),
        "planning in either mode preserves the descriptor target"
    );
}

#[test]
fn metadata_only_artifact_census_does_not_hash_unlink_evidence() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let orphan = text_artifact_for_bytes(&active.id, b"good");
    let orphan_path = write_text_artifact(&store, &orphan, b"good");
    std::fs::write(&orphan_path, b"evil").expect("same-size tamper");

    let metadata_only = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("metadata census uses bounded filename/type/size identity");
    assert_eq!(metadata_only.collectable_text_artifacts.len(), 1);
    let full = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    );
    assert!(
        matches!(full, Err(CodeGenerationRetentionErrorV1::UnsafeState(_))),
        "only full verification may trust the content address for unlinking"
    );
}

#[test]
fn applied_retention_refuses_a_metadata_only_plan() {
    let (store, _generations) = fixture_store(5);
    let plan = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        TEST_ROLLBACK_FLOOR,
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("metadata-only census");

    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(14),
        None,
    )
    .expect_err("unlinking evidence requires proven content digests");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
}

/// The OOM-crash debris shape: sealed generation files and derived artifacts
/// exist, but the publish never reached its pointer write, so no active
/// pointer file exists. The pass must reclaim everything through the ordinary
/// journal/receipt/release machinery — before this, such stores were
/// unreachable by every retention pass while their worktree root stayed live.
#[test]
fn unpublished_store_retention_reclaims_orphaned_partial_generations() {
    let (store, generations) = fixture_store(2);
    std::fs::remove_file(store.path().join(ACTIVE_POINTER_FILE)).expect("sever the active pointer");
    let artifacts_root = code_text_artifacts_root(store.path());
    let orphan = text_artifact_for_bytes(&generations[0].id, b"orphan completed bytes");
    let orphan_path = write_text_artifact(&store, &orphan, b"orphan completed bytes");
    let staging_name = format!(".text-artifact-{}.staging", "a".repeat(64));
    let staging_path = artifacts_root.join(&staging_name);
    std::fs::write(&staging_path, b"abandoned staging").expect("write orphan staging");
    let sidecar_name = format!(".text-artifact-{}.staging-journal", "a".repeat(64));
    let sidecar_path = artifacts_root.join(&sidecar_name);
    std::fs::write(&sidecar_path, b"abandoned staging journal").expect("write orphan sidecar");
    let pool_root = store.path().join("graph-replay-pool");

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(99),
        Some(&pool_root),
    )
    .expect("apply unpublished-store retention");

    assert_eq!(report.deleted_generations.len(), generations.len());
    let receipt = report.receipt.expect("durable retention receipt");
    assert_eq!(receipt.active_generation_id, None);
    assert_eq!(
        receipt.reclaimed_bytes,
        generations
            .iter()
            .map(|generation| generation.size_bytes)
            .sum::<u64>()
    );
    assert!(
        store
            .path()
            .join(RECEIPTS_DIRECTORY)
            .join(format!("receipt-{}.json", receipt.receipt_digest))
            .is_file(),
        "the deletion journal must record the unpublished-store sweep"
    );
    assert_eq!(
        queued_release_count(store.path()),
        generations.len(),
        "every reclaimed generation must queue its graph replay release"
    );
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    for generation in &generations {
        assert!(!generations_root.join(&generation.file).exists());
        assert!(
            pool_root.join(&generation.file).is_file(),
            "the replay pool must retain the sealed bytes until the graph confirms release"
        );
    }
    let text_receipt = report
        .text_artifact_receipt
        .expect("text artifact retention receipt");
    assert_eq!(text_receipt.active_generation_id, None);
    assert_eq!(report.deleted_text_artifacts.len(), 3);
    assert!(!orphan_path.exists());
    assert!(!staging_path.exists());
    assert!(!sidecar_path.exists());
    assert!(!transaction_path(store.path()).exists());
    assert!(!text_artifact_transaction_path(store.path()).exists());
}

#[test]
fn unpublished_store_execution_refuses_when_a_pointer_appears() {
    let (store, generations) = fixture_store(1);
    let pointer_path = store.path().join(ACTIVE_POINTER_FILE);
    let pointer_bytes = std::fs::read(&pointer_path).expect("read fixture pointer");
    std::fs::remove_file(&pointer_path).expect("sever the active pointer");

    let plan = plan_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    )
    .expect("plan unpublished-store retention");
    assert_eq!(plan.active_generation_id, None);
    assert_eq!(plan.collectable_generations.len(), 1);

    // A publisher lands the first pointer between the mark phase and apply:
    // the absent-pointer compare-and-swap must refuse under the store lock.
    std::fs::write(&pointer_path, &pointer_bytes).expect("republish the pointer");
    let error = execute_code_generation_retention(
        store.path(),
        plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(7),
        None,
    )
    .expect_err("a published pointer must fail the unpublished-store CAS");

    assert!(matches!(
        error,
        CodeGenerationRetentionErrorV1::UnsafeState(_)
    ));
    let generations_root = store.path().join(GENERATIONS_DIRECTORY);
    assert!(
        generations_root
            .join(&generations.last().expect("fixture generation").file)
            .is_file(),
        "no generation may be unlinked after the CAS refusal"
    );
    assert!(!transaction_path(store.path()).exists());
}

#[test]
fn staging_sidecars_share_their_staging_artifact_liveness() {
    let (store, generations) = fixture_store(1);
    let active = generations.last().expect("active generation");
    let artifacts_root = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts_root).expect("create artifact root");
    let active_digest = active
        .state_digest
        .strip_prefix("sha256:")
        .expect("active sealed digest");
    let active_staging = artifacts_root.join(format!(".text-artifact-{active_digest}.staging"));
    std::fs::write(&active_staging, b"resumable active staging").expect("write active staging");
    let active_sidecar =
        artifacts_root.join(format!(".text-artifact-{active_digest}.staging-journal"));
    std::fs::write(&active_sidecar, b"active staging journal").expect("write active sidecar");
    let orphan_staging = artifacts_root.join(format!(".text-artifact-{}.staging", "c".repeat(64)));
    std::fs::write(&orphan_staging, b"abandoned staging").expect("write orphan staging");
    let orphan_sidecar =
        artifacts_root.join(format!(".text-artifact-{}.staging-journal", "c".repeat(64)));
    std::fs::write(&orphan_sidecar, b"abandoned staging journal").expect("write orphan sidecar");

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(21),
        None,
    )
    .expect("apply sidecar-aware retention");

    assert_eq!(report.deleted_text_artifacts.len(), 2);
    assert!(
        active_staging.is_file() && active_sidecar.is_file(),
        "the active build's staging file and its sidecar must survive"
    );
    assert!(!orphan_staging.exists());
    assert!(!orphan_sidecar.exists());
}
