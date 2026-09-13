use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index_retention::code_index_generations::{
    DurablePublicationPointerV1, acquire_code_generation_store_lock,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, SanitizerRevision, UtcMicros, encode_lowercase_hex,
    sha256_hex_suffix,
};
use tracedecay_query::retrieval::ports::RetrievalPortError;

use super::{
    EIGHT_DAYS_SECS, GitFixture, RETAINED_REVISION_0,
    execute_scope_retention_with_test_binding_cleanup, published,
    remove_historical_pointer_entries, retention_generations, scheduler, seeded_scope,
    test_project_id, unix_now_secs,
};
use crate::{
    code_index::production::{
        CodeIndexAtomicPublicationPort, CodeIndexInterruptionV1, CodeIndexProductionErrorV1,
        CodeIndexPublicationStoreErrorV1, UninterruptibleCodeIndexControlV1,
        VerifiedSealedLexicalPageReadV1,
    },
    code_index_scheduler::{CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1},
};

#[test]
fn partitioned_publication_reuses_unchanged_file_segments() {
    let unchanged = (0..256).fold(String::new(), |mut source, index| {
        writeln!(
            source,
            "// stable unchanged body padding xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        )
        .expect("write generated fixture padding");
        writeln!(source, "pub fn unchanged_{index}() -> usize {{ {index} }}")
            .expect("write generated fixture source");
        source
    });
    let fixture = GitFixture::new(&[
        ("src/large.rs", unchanged.as_str()),
        ("src/edited.rs", "pub fn edited() -> usize { 1 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );

    published(
        scheduler
            .reconcile_now()
            .expect("publish first segmented generation"),
    );
    let first_encoded_segment_bytes = scheduler
        .publication
        .seal_encoded_segment_bytes
        .load(std::sync::atomic::Ordering::Relaxed);
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let first_pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read first pointer"))
            .expect("decode first pointer");
    let first_manifest_path = store.path().join("code-generations-v1").join(
        first_pointer["generation_file"]
            .as_str()
            .expect("first generation manifest"),
    );
    let first_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&first_manifest_path).expect("read first manifest"))
            .expect("decode first manifest");
    assert_eq!(first_manifest["generation"]["format_revision"], 7);
    let first_segments = first_manifest["generation"]["file_segments"]
        .as_array()
        .expect("first generation file segments");
    assert_eq!(first_segments.len(), 2);

    fixture.edit("src/edited.rs", "pub fn edited() -> usize { 2 }\n");
    scheduler.notify_hook_paths([PathBuf::from("src/edited.rs")]);
    published(
        scheduler
            .reconcile_now()
            .expect("publish one-line-edit generation"),
    );
    let second_encoded_segment_bytes = scheduler
        .publication
        .seal_encoded_segment_bytes
        .load(std::sync::atomic::Ordering::Relaxed);
    let second_existing_segment_bytes_read = scheduler
        .publication
        .seal_existing_segment_bytes_read
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        second_encoded_segment_bytes.saturating_mul(4) < first_encoded_segment_bytes,
        "one-file increment encoded {second_encoded_segment_bytes} segment bytes after the cold generation encoded {first_encoded_segment_bytes}"
    );
    assert_eq!(
        second_existing_segment_bytes_read, 0,
        "unchanged content-addressed segments must not be reopened during incremental seal"
    );
    let second_pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read second pointer"))
            .expect("decode second pointer");
    let second_manifest_path = store.path().join("code-generations-v1").join(
        second_pointer["generation_file"]
            .as_str()
            .expect("second generation manifest"),
    );
    let second_manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&second_manifest_path).expect("read second manifest"),
    )
    .expect("decode second manifest");
    let second_segments = second_manifest["generation"]["file_segments"]
        .as_array()
        .expect("second generation file segments");
    assert_eq!(second_segments.len(), 2);

    let segment_digest = |segments: &[serde_json::Value], file_key: u64| {
        segments
            .iter()
            .find(|segment| segment["file_key"].as_u64() == Some(file_key))
            .and_then(|segment| segment["segment_digest"].as_str())
            .expect("segment descriptor")
            .to_owned()
    };
    let file_key = |manifest: &serde_json::Value, logical_path: &str| {
        manifest["generation"]["snapshot"]["files"]
            .as_array()
            .expect("snapshot files")
            .iter()
            .position(|file| file["logical_path"].as_str() == Some(logical_path))
            .and_then(|key| u64::try_from(key).ok())
            .expect("snapshot file key")
    };
    let large_key = file_key(&first_manifest, "src/large.rs");
    let edited_key = file_key(&first_manifest, "src/edited.rs");
    let shared_segment = segment_digest(first_segments, large_key);
    let retired_edited_segment = segment_digest(first_segments, edited_key);
    let second_shared_segment = segment_digest(second_segments, large_key);
    assert_eq!(
        shared_segment, second_shared_segment,
        "the unchanged large file must reuse the exact segment content address"
    );
    assert_ne!(
        retired_edited_segment,
        segment_digest(second_segments, edited_key),
        "the one-line edit must publish exactly one replacement segment"
    );

    let segment_root = store.path().join("code-generation-segments-v1");
    let component_sizes = |manifest: &serde_json::Value| {
        let mut components = manifest["generation"]["file_segments"]
            .as_array()
            .expect("file segment descriptors")
            .iter()
            .map(|segment| {
                (
                    segment["segment_digest"]
                        .as_str()
                        .expect("file segment digest")
                        .to_owned(),
                    segment["segment_size_bytes"]
                        .as_u64()
                        .expect("file segment size"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        components.insert(
            manifest["generation"]["generation_evidence"]["segment_digest"]
                .as_str()
                .expect("evidence segment digest")
                .to_owned(),
            manifest["generation"]["generation_evidence"]["segment_size_bytes"]
                .as_u64()
                .expect("evidence segment size"),
        );
        components
    };
    let first_components = component_sizes(&first_manifest);
    let second_components = component_sizes(&second_manifest);
    let first_evidence_pack = first_manifest["generation"]["generation_evidence"]["segment_digest"]
        .as_str()
        .expect("first evidence pack digest")
        .to_owned();
    let second_evidence_pack =
        second_manifest["generation"]["generation_evidence"]["segment_digest"]
            .as_str()
            .expect("second evidence pack digest")
            .to_owned();
    assert_ne!(first_evidence_pack, second_evidence_pack);
    let second_generation_growth = std::fs::metadata(&second_manifest_path)
        .expect("second manifest metadata")
        .len()
        .saturating_add(
            second_components
                .iter()
                .filter(|(digest, _)| !first_components.contains_key(*digest))
                .map(|(_, size)| *size)
                .sum::<u64>(),
        );
    let monolithic_second_bytes = scheduler
        .latest_complete_already_decoded()
        .expect("second generation remains decoded")
        .generation
        .encode_sealed()
        .expect("encode monolithic comparison")
        .len() as u64;
    assert!(
        second_generation_growth.saturating_mul(2) < monolithic_second_bytes,
        "one-line edit added {second_generation_growth} physical bytes versus a \
         {monolithic_second_bytes}-byte monolithic rewrite"
    );
    let segment_sizes = std::fs::read_dir(&segment_root)
        .expect("list content-addressed segments")
        .map(|entry| {
            entry
                .expect("segment entry")
                .metadata()
                .expect("segment metadata")
                .len()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        segment_sizes.len(),
        5,
        "three shared/edited file segments plus one evidence segment per generation"
    );
    let first_generation_segment_bytes = first_segments
        .iter()
        .map(|segment| {
            segment["segment_size_bytes"]
                .as_u64()
                .expect("segment size")
        })
        .sum::<u64>();
    let second_generation_new_bytes = second_segments
        .iter()
        .find(|segment| segment["file_key"].as_u64() == Some(edited_key))
        .and_then(|segment| segment["segment_size_bytes"].as_u64())
        .expect("edited segment size");
    assert_eq!(
        first_encoded_segment_bytes, first_generation_segment_bytes,
        "cold seal counts every encoded file segment byte"
    );
    assert_eq!(
        second_encoded_segment_bytes, second_generation_new_bytes,
        "incremental seal counts only the edited file segment bytes"
    );
    assert!(
        second_generation_new_bytes.saturating_mul(8) < first_generation_segment_bytes,
        "one-line edit rewrote {second_generation_new_bytes} bytes from a \
         {first_generation_segment_bytes}-byte first generation"
    );

    let orphan_bytes = b"evidence pack committed before its manifest";
    let orphan_digest = encode_lowercase_hex(&Sha256::digest(orphan_bytes));
    let orphan_pack = segment_root.join(format!("segment-{orphan_digest}.json"));
    std::fs::write(&orphan_pack, orphan_bytes).expect("write committed orphan evidence pack");
    // A vector-readable mark holds the single superseded generation; the
    // pointer index it is still named by would not.
    let first_generation = CodeGenerationId::new(
        first_pointer["generation_id"]
            .as_str()
            .expect("first generation id"),
    )
    .expect("valid first generation id");
    let orphan_report = tracedecay_code_index_retention::code_index_generations::run_code_generation_retention(
        store.path(),
        &BTreeSet::from([first_generation]),
        tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionModeV1::Apply,
        UtcMicros(8_000_000),
        None,
    )
    .expect("sweep committed orphan without collecting a generation");
    assert!(
        orphan_report.deleted_generations.is_empty(),
        "the vector-readable mark must retain the superseded generation"
    );
    assert!(
        !orphan_pack.exists(),
        "retention must sweep an unreferenced final pack even without a generation deletion"
    );
    for live_segment in [&shared_segment, &first_evidence_pack, &second_evidence_pack] {
        assert!(
            segment_root
                .join(format!(
                    "segment-{}.json",
                    sha256_hex_suffix(live_segment).expect("tagged live segment digest")
                ))
                .is_file(),
            "active, retained, and parent-reused segment {live_segment} must remain marked"
        );
    }

    // Without that reserve the same generation is collectable while the
    // pointer still names it, and its segments part by whether the active
    // generation reuses them.
    let report = tracedecay_code_index_retention::code_index_generations::run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionModeV1::Apply,
        UtcMicros(9_000_000),
        None,
    )
    .expect("collect retired partitioned generation");
    assert_eq!(report.deleted_generations.len(), 1);
    let segment_path = |digest: &str| {
        segment_root.join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest).expect("tagged segment digest")
        ))
    };
    assert!(
        segment_path(&shared_segment).is_file(),
        "retention must preserve a segment referenced by the active generation"
    );
    assert!(
        !segment_path(&retired_edited_segment).exists(),
        "retention must collect a segment referenced only by the retired generation"
    );
    assert!(
        segment_path(&second_evidence_pack).is_file(),
        "retention must preserve the active generation's packed evidence"
    );
    assert!(
        !segment_path(&first_evidence_pack).exists(),
        "retention must collect packed evidence referenced only by the retired generation"
    );
}

#[test]
fn lazy_lexical_source_cancels_when_retention_retires_its_unread_segments() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn before_retirement() -> usize { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish first generation"));
    let latest = scheduler
        .latest_complete_already_decoded()
        .expect("first generation");
    let generation_id = latest.generation.manifest().generation_id.clone();
    let text_store = &latest.text.text_artifact_store;
    let identity = text_store
        .sealed_identity(&generation_id)
        .expect("retained seal identity");
    let mut source = text_store
        .open_sealed_source(&identity, &UninterruptibleCodeIndexControlV1)
        .expect("open lazy source without retaining every file");
    let initial_cursor = source.cursor().clone();
    let lock = acquire_code_generation_store_lock(store.path()).expect("hold publication lock");
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let result = source.next_page(&UninterruptibleCodeIndexControlV1);
        sent.send((source, result)).expect("return lexical source");
    });
    let response = received.recv_timeout(Duration::from_secs(2));
    drop(lock);
    reader.join().expect("lexical reader exits");
    let (mut source, result) =
        response.expect("busy publication must not block the lexical reader");
    assert!(matches!(
        result,
        Err(CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::Unavailable(_)
        ))
    ));
    assert_eq!(source.cursor(), &initial_cursor);
    assert!(matches!(
        source
            .next_page(&UninterruptibleCodeIndexControlV1)
            .expect("retry after publication unlock"),
        VerifiedSealedLexicalPageReadV1::Page(_)
    ));
    source
        .rewind()
        .expect("rewind before retiring unread source");

    fixture.edit("src/lib.rs", "pub fn after_retirement() -> usize { 2 }\n");
    published(scheduler.reconcile_now().expect("publish successor"));
    remove_historical_pointer_entries(store.path());
    let report = tracedecay_code_index_retention::code_index_generations::run_code_generation_retention(
        store.path(), &BTreeSet::new(),
        tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionModeV1::Apply,
        UtcMicros(9_000_000), None,
    ).expect("collect retired generation");
    assert_eq!(report.deleted_generations.len(), 1);
    assert!(
        matches!(
            source.next_page(&UninterruptibleCodeIndexControlV1),
            Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::Cancelled
            ))
        ),
        "retired lazy sources cancel before reading collected segment paths"
    );
    assert_eq!(source.cursor(), &initial_cursor);

    let latest = scheduler
        .latest_complete_already_decoded()
        .expect("successor generation");
    let text_store = &latest.text.text_artifact_store;
    let identity = text_store
        .sealed_identity(&latest.generation.manifest().generation_id)
        .expect("successor seal identity");
    let mut corrupt_source = text_store
        .open_sealed_source(&identity, &UninterruptibleCodeIndexControlV1)
        .expect("open source before pointer corruption");
    let cursor = corrupt_source.cursor().clone();
    std::fs::write(
        store.path().join("active-code-generation-v1.json"),
        b"corrupt",
    )
    .expect("corrupt hermetic publication pointer");
    let error = corrupt_source
        .next_page(&UninterruptibleCodeIndexControlV1)
        .expect_err("corrupt publication pointer must refuse source read");
    assert!(
        matches!(
            super::super::map_sealed_page_source_error(error),
            RetrievalPortError::Contract(_)
        ),
        "corrupt authority is terminal, never transient store contention"
    );
    assert_eq!(corrupt_source.cursor(), &cursor);
}

#[test]
fn multi_page_evidence_uses_one_durable_pack_and_survives_restart() {
    let source = (0..1_600).fold(String::new(), |mut source, index| {
        writeln!(
            source,
            "pub fn evidence_{index}(value: usize) -> usize {{ value + {index} }}"
        )
        .expect("write generated fixture source");
        source
    });
    let fixture = GitFixture::new(&[("src/evidence.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let (generation_id, evidence_pack_path) = {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(
            scheduler
                .reconcile_now()
                .expect("publish multi-page generation"),
        );
        let latest = scheduler
            .latest_complete_already_decoded()
            .expect("multi-page generation remains decoded");
        let generation_id = latest.generation.manifest().generation_id.clone();
        let pointer: serde_json::Value = serde_json::from_slice(
            &std::fs::read(store.path().join("active-code-generation-v1.json"))
                .expect("read active pointer"),
        )
        .expect("decode active pointer");
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                store.path().join("code-generations-v1").join(
                    pointer["generation_file"]
                        .as_str()
                        .expect("generation manifest path"),
                ),
            )
            .expect("read partitioned manifest"),
        )
        .expect("decode partitioned manifest");
        let pages = manifest["generation"]["generation_evidence"]["pages"]
            .as_array()
            .expect("evidence page descriptors");
        assert!(pages.len() > 1, "the production fixture must span pages");
        let segments_root = store.path().join("code-generation-segments-v1");
        let file_segment_count = manifest["generation"]["file_segments"]
            .as_array()
            .expect("file segment descriptors")
            .len();
        assert_eq!(
            std::fs::read_dir(&segments_root)
                .expect("read segment objects")
                .count(),
            file_segment_count + 1,
            "pages must be ranges in one pack, never separate filesystem objects"
        );
        for page in pages {
            let page_digest = sha256_hex_suffix(page["page_digest"].as_str().expect("page digest"))
                .expect("tagged page digest");
            assert!(
                !segments_root
                    .join(format!("segment-{page_digest}.json"))
                    .exists(),
                "an evidence page must not receive its own durable transaction"
            );
        }
        assert_eq!(
            scheduler
                .publication
                .seal_evidence_page_count
                .load(std::sync::atomic::Ordering::Relaxed),
            pages.len() as u64,
            "the scale counter must report every streamed page"
        );
        assert_eq!(
            scheduler
                .publication
                .seal_evidence_durable_transaction_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "all evidence pages must use one fsync/rename transaction"
        );
        let evidence_digest = sha256_hex_suffix(
            manifest["generation"]["generation_evidence"]["segment_digest"]
                .as_str()
                .expect("evidence pack digest"),
        )
        .expect("tagged evidence pack digest");
        (
            generation_id,
            store
                .path()
                .join("code-generation-segments-v1")
                .join(format!("segment-{evidence_digest}.json")),
        )
    };

    let reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("reopen publication store");
    assert!(
        reopened
            .load_generation(&generation_id)
            .expect("decode multi-page evidence after restart")
            .is_some(),
        "the one-pack generation must remain restart-readable"
    );
    drop(reopened);

    let evidence_len = std::fs::metadata(&evidence_pack_path)
        .expect("evidence pack metadata")
        .len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&evidence_pack_path)
        .expect("open evidence pack for truncation")
        .set_len(evidence_len - 1)
        .expect("truncate one evidence byte");
    let corrupted = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("reopen corrupted publication store");
    assert!(
        corrupted.load_generation(&generation_id).is_err(),
        "a pack missing any page byte must fail closed after restart"
    );
}

#[test]
fn failed_and_crashed_evidence_pack_temporaries_are_removed() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() {}\n")]);
    let store = TempDir::new().expect("store root");
    let segments_root = store.path().join("code-generation-segments-v1");
    std::fs::create_dir_all(&segments_root).expect("create segment root");
    let temporary_path = segments_root.join(".evidence-pack-publication.injected.tmp");
    {
        let mut pack = super::super::TemporaryEvidencePackV1::create(temporary_path.clone())
            .expect("create temporary evidence pack");
        for ordinal in 0..3 {
            let bytes = format!("page-{ordinal}");
            let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes.as_bytes()))
                .expect("page digest");
            pack.append_page(ordinal, &digest, bytes.as_bytes())
                .expect("append page before injected failure");
        }
    }
    assert!(
        !temporary_path.exists(),
        "a failure after N pages must remove the incomplete pack"
    );

    std::fs::write(&temporary_path, b"crash orphan").expect("write crash orphan");
    let _reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("restart publication store");
    assert!(
        !temporary_path.exists(),
        "restart must durably clean an abandoned evidence pack"
    );
}

#[test]
fn retired_fence_cancels_a_generation_seal_between_segments() {
    // One segment per file: shutdown is signalled after the first durable
    // segment, exactly where a TERM lands on a large worktree's first build.
    let sources = (0..8)
        .map(|file| {
            (
                format!("src/module_{file}.rs"),
                format!("pub fn sealed_{file}() -> u32 {{ {file} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(
        &sources
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str()))
            .collect::<Vec<_>>(),
    );
    let source_store = TempDir::new().expect("source store root");
    let generation = {
        let mut scheduler = scheduler(
            &fixture,
            source_store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(
            scheduler
                .reconcile_now()
                .expect("build multi-file generation"),
        );
        Arc::clone(
            &scheduler
                .latest_complete_already_decoded()
                .expect("multi-file generation remains decoded")
                .generation,
        )
    };
    assert!(
        generation.snapshot().files.len() >= 8,
        "fixture must seal one segment per file"
    );

    let target_store = TempDir::new().expect("target publication store root");
    let shutting_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let published_segments = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observer_segments = Arc::clone(&published_segments);
    let observer_shutting_down = Arc::clone(&shutting_down);
    let mut publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        target_store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open target publication store")
    .with_shutdown_signal(Arc::clone(&shutting_down))
    .with_seal_segment_observer_for_test(Arc::new(move || {
        observer_segments.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        observer_shutting_down.store(true, std::sync::atomic::Ordering::Release);
    }));

    let error = publication
        .publish_atomically(&generation.sealed_scope(), None, Arc::clone(&generation))
        .expect_err("shutdown signalled mid-seal must stop the publication");
    assert!(
        matches!(error, CodeIndexPublicationStoreErrorV1::CompareAndSwap),
        "a cancelled seal is the same typed outcome as a retired fence: {error}"
    );
    assert_eq!(
        published_segments.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the seal must stop at the first checkpoint after shutdown was signalled"
    );
    assert!(
        !target_store
            .path()
            .join("active-code-generation-v1.json")
            .exists(),
        "a cancelled seal must not publish a pointer"
    );
    let generations_root = target_store.path().join("code-generations-v1");
    let leftover = std::fs::read_dir(&generations_root)
        .map_or(0, |entries| entries.filter_map(Result::ok).count());
    assert_eq!(
        leftover, 0,
        "a cancelled seal must leave no manifest behind"
    );
}

#[test]
fn evidence_pack_failure_after_pages_never_publishes_manifest_or_pointer() {
    let source = (0..1_600).fold(String::new(), |mut source, index| {
        writeln!(
            source,
            "pub fn failed_evidence_{index}(value: usize) -> usize {{ value + {index} }}"
        )
        .expect("write generated fixture source");
        source
    });
    let fixture = GitFixture::new(&[("src/evidence.rs", source.as_str())]);
    let source_store = TempDir::new().expect("source store root");
    let generation = {
        let mut scheduler = scheduler(
            &fixture,
            source_store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(
            scheduler
                .reconcile_now()
                .expect("build multi-page generation"),
        );
        Arc::clone(
            &scheduler
                .latest_complete_already_decoded()
                .expect("multi-page generation remains decoded")
                .generation,
        )
    };
    let source_pointer: serde_json::Value = serde_json::from_slice(
        &std::fs::read(source_store.path().join("active-code-generation-v1.json"))
            .expect("read source pointer"),
    )
    .expect("decode source pointer");
    let source_generation_file = source_pointer["generation_file"]
        .as_str()
        .expect("source generation manifest path")
        .to_owned();
    let source_manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            source_store
                .path()
                .join("code-generations-v1")
                .join(&source_generation_file),
        )
        .expect("read source manifest"),
    )
    .expect("decode source manifest");
    let evidence = &source_manifest["generation"]["generation_evidence"];
    let evidence_page_count = evidence["pages"]
        .as_array()
        .expect("evidence page descriptors")
        .len();
    assert!(
        evidence_page_count > 1,
        "the failure fixture must append multiple pages before commit"
    );
    let evidence_digest = sha256_hex_suffix(
        evidence["segment_digest"]
            .as_str()
            .expect("evidence pack digest"),
    )
    .expect("tagged evidence pack digest");
    let source_pack = source_store
        .path()
        .join("code-generation-segments-v1")
        .join(format!("segment-{evidence_digest}.json"));

    let failed_store = TempDir::new().expect("failed publication store root");
    let mut publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        failed_store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open failed publication store");
    let segments_root = failed_store.path().join("code-generation-segments-v1");
    let colliding_pack = segments_root.join(format!("segment-{evidence_digest}.json"));
    std::fs::write(&colliding_pack, b"wrong immutable evidence pack")
        .expect("seed corrupt immutable evidence target");

    let error = publication
        .publish_atomically(&generation.sealed_scope(), None, Arc::clone(&generation))
        .expect_err("a conflicting aggregate pack must fail publication");
    assert!(
        error
            .to_string()
            .contains("existing sealed evidence pack does not match its content address"),
        "the real pack commit must reject the conflicting aggregate: {error}"
    );
    assert_eq!(
        publication
            .seal_evidence_page_count
            .load(std::sync::atomic::Ordering::Relaxed),
        evidence_page_count as u64,
        "publication must fail only after every evidence page was appended"
    );
    assert_eq!(
        publication
            .seal_evidence_durable_transaction_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a rejected pack must not claim a completed durability transaction"
    );
    assert!(
        !failed_store
            .path()
            .join("active-code-generation-v1.json")
            .exists(),
        "a failed aggregate commit must not publish a pointer"
    );
    assert_eq!(
        std::fs::read_dir(failed_store.path().join("code-generations-v1"))
            .expect("read failed generation directory")
            .count(),
        0,
        "a failed aggregate commit must not publish a manifest or leave its temporary"
    );
    assert!(
        std::fs::read_dir(&segments_root)
            .expect("read failed segment directory")
            .all(|entry| {
                !entry
                    .expect("failed segment directory entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".evidence-pack-publication.")
            }),
        "a failed aggregate commit must remove its incomplete evidence temporary"
    );

    std::fs::remove_file(&colliding_pack).expect("remove injected collision");
    let colliding_manifest = failed_store
        .path()
        .join("code-generations-v1")
        .join(&source_generation_file);
    std::fs::write(&colliding_manifest, b"wrong immutable generation manifest")
        .expect("seed corrupt immutable generation target");
    let error = publication
        .publish_atomically(&generation.sealed_scope(), None, Arc::clone(&generation))
        .expect_err("a post-pack manifest collision must fail publication");
    assert!(
        error
            .to_string()
            .contains("immutable code-generation path contains different bytes"),
        "the real manifest commit must reject the conflicting generation: {error}"
    );
    assert_eq!(
        publication
            .seal_evidence_durable_transaction_count
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the failure must occur after the final evidence-pack durability transaction"
    );
    assert!(
        !colliding_pack.exists(),
        "a pre-manifest publication failure must immediately roll back its final pack"
    );
    assert!(
        !failed_store
            .path()
            .join("active-code-generation-v1.json")
            .exists(),
        "a post-pack manifest failure must not publish a pointer"
    );
    std::fs::remove_file(colliding_manifest).expect("remove injected manifest collision");

    std::fs::copy(&source_pack, &colliding_pack)
        .expect("simulate a crash after the final evidence-pack rename");
    drop(publication);
    let _reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        failed_store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("reopen after committed-pack crash");
    assert!(
        colliding_pack.exists(),
        "publication-store construction lacks graph replay liveness and must not sweep final packs"
    );

    let graph_replay_pool = failed_store.path().join("graph-replay-pool");
    tracedecay_private_fs::create_private_directory(&graph_replay_pool)
        .expect("create graph replay pool");
    let report = tracedecay_code_index_retention::code_index_generations::run_code_generation_retention(
        failed_store.path(),
        &BTreeSet::new(),
        tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionModeV1::Apply,
        UtcMicros(8_100_000),
        Some(&graph_replay_pool),
    )
    .expect("maintenance sweeps committed-pack crash debris");
    assert!(report.deleted_generations.is_empty());
    assert!(
        !colliding_pack.exists(),
        "canonical locked maintenance must remove a final pack with no durable manifest"
    );
}

/// Membership of the durable `generation_index` is a referential-integrity
/// check, not a liveness mark: a published store keeps the active generation
/// and the vector-readable sources, and everything older is collectable while
/// the pointer still names it.
#[test]
fn code_generation_retention_keeps_only_the_active_and_marked_generations() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, run_code_generation_retention,
    };

    const MARKED_SUPERSEDED: usize = 2;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 5);
    let (collected, reserved) = generations.split_at(generations.len() - MARKED_SUPERSEDED - 1);
    let vector_readable_sources = reserved[..MARKED_SUPERSEDED]
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    let report = run_code_generation_retention(
        store.path(),
        &vector_readable_sources,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(49),
        None,
    )
    .expect("apply retention");

    assert_eq!(
        report
            .deleted_generations
            .iter()
            .map(|generation| generation.generation_id.as_str())
            .collect::<BTreeSet<_>>(),
        collected
            .iter()
            .map(CodeGenerationId::as_str)
            .collect::<BTreeSet<_>>(),
        "everything older than the active generation and the marked sources is collectable"
    );
    let reserved = reserved
        .iter()
        .map(CodeGenerationId::as_str)
        .collect::<BTreeSet<_>>();
    let reopened = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    for generation in &generations {
        assert_eq!(
            reopened
                .publication
                .load_generation(generation)
                .expect("read retained generation")
                .is_some(),
            reserved.contains(generation.as_str()),
            "only the active generation and the marked sources survive collection"
        );
    }
}

#[test]
fn sealed_replay_binding_resolves_an_exact_superseded_generation() {
    let fixture = GitFixture::new(RETAINED_REVISION_0);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 2);
    let scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let pointer = scheduler
        .publication
        .read_publication_pointer()
        .expect("read publication pointer")
        .expect("published generation pointer");
    let superseded = &generations[0];
    assert_ne!(pointer.generation_id, superseded.as_str());
    let entry = pointer
        .generation_index
        .iter()
        .find(|entry| entry.generation_id == superseded.as_str())
        .expect("superseded generation remains pointer-addressable");

    let binding = scheduler
        .publication
        .sealed_replay_binding(superseded)
        .expect("bind the exact superseded sealed replay");

    assert_eq!(
        binding.sealed_state_digest.as_str(),
        entry.state_digest,
        "the replay binding must retain the requested generation's sealed identity",
    );

    let unknown = CodeGenerationId::new("generation.not-retained").expect("unknown generation");
    assert!(matches!(
        scheduler.publication.sealed_replay_binding(&unknown),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(message))
            if message.contains(unknown.as_str())
                && message.contains("not retained in the publication index")
    ));
}

/// The durable index bounds the pointer's own history, and a collection pass
/// bounds its batch; neither bound is the other's. One pass takes its whole
/// batch from the oldest end no matter which of those generations the index
/// still names, and every collected id leaves the index with them.
#[test]
fn one_bounded_pass_collects_clean_and_dirty_superseded_generations() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DurablePublicationPointerV1,
        MAX_CODE_GENERATION_RETENTION_BATCH_V1, MAX_DURABLE_GENERATION_INDEX_BYTES_V1,
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(
        &fixture,
        store.path(),
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 + 3,
    );
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read bounded publication pointer"),
    )
    .expect("decode bounded publication pointer");

    assert!(pointer.generation_index_truncated);
    assert_eq!(
        pointer.generation_index.len(),
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1
    );
    assert!(
        pointer
            .generation_index
            .iter()
            .map(|entry| entry.size_bytes)
            .sum::<u64>()
            <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
    );
    assert!(
        pointer
            .generation_index
            .iter()
            .any(|entry| entry.source_reference.is_none()),
        "dirty snapshots must consume the same retained-history budget"
    );

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(50),
        None,
    )
    .expect("collect superseded generations in one bounded pass");
    let collected = report
        .deleted_generations
        .iter()
        .map(|generation| generation.generation_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(collected.len(), MAX_CODE_GENERATION_RETENTION_BATCH_V1);

    let reopened = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let retained = generations
        .iter()
        .map(CodeGenerationId::as_str)
        .filter(|generation| !collected.contains(generation))
        .collect::<BTreeSet<_>>();
    for generation in &generations {
        assert_eq!(
            reopened
                .publication
                .load_generation(generation)
                .expect("read bounded generation")
                .is_some(),
            retained.contains(generation.as_str()),
            "one bounded pass collects its whole batch and nothing else"
        );
    }
    assert_eq!(
        std::fs::read_dir(store.path().join("code-generations-v1"))
            .expect("list retained generations")
            .count(),
        retained.len()
    );
    let rewritten: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read rewritten publication pointer"),
    )
    .expect("decode rewritten publication pointer");
    assert!(
        rewritten
            .generation_index
            .iter()
            .all(|entry| retained.contains(entry.generation_id.as_str())),
        "the durable index may never name a generation this pass unlinked"
    );
}

#[test]
fn code_generation_retention_dry_run_reports_without_deleting() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 5);
    remove_historical_pointer_entries(store.path());

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        CodeGenerationRetentionModeV1::DryRun,
        UtcMicros(50),
        None,
    )
    .expect("plan retention");

    assert_eq!(report.plan.superseded_generations.len(), 4);
    assert_eq!(report.plan.collectable_generations.len(), 4);
    assert_eq!(
        report
            .plan
            .collectable_generations
            .iter()
            .map(|generation| generation.generation_id.clone())
            .collect::<BTreeSet<_>>(),
        generations[..4].iter().cloned().collect()
    );
    println!(
        "dry_run superseded_count={} superseded_bytes={} collectable_count={} collectable_bytes={} deleted_count={}",
        report.plan.superseded_generations.len(),
        report.plan.superseded_generation_bytes(),
        report.plan.collectable_generations.len(),
        report.plan.collectable_generation_bytes(),
        report.deleted_generations.len(),
    );
    assert_eq!(report.deleted_generations.len(), 0);
    assert!(report.receipt.is_none());
    assert!(
        store
            .path()
            .join("code-generations-v1")
            .join(&report.plan.collectable_generations[0].generation_file)
            .is_file()
    );
}

#[test]
fn code_generation_retention_never_sweeps_vector_readable_source() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 6);
    remove_historical_pointer_entries(store.path());
    let vector_readable = BTreeSet::from([generations[0].clone()]);

    let report = run_code_generation_retention(
        store.path(),
        &vector_readable,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(60),
        None,
    )
    .expect("apply retention");

    let vector_generation = report
        .plan
        .superseded_generations
        .iter()
        .find(|generation| generation.generation_id == generations[0])
        .expect("vector-readable generation was inventoried");
    assert!(
        store
            .path()
            .join("code-generations-v1")
            .join(&vector_generation.generation_file)
            .is_file(),
        "a generation named by retained_readable_sources must survive the sweep"
    );
    assert!(
        report
            .deleted_generations
            .iter()
            .all(|generation| generation.generation_id != generations[0])
    );
    assert_eq!(report.deleted_generations.len(), 4);
    assert_eq!(
        report
            .deleted_generations
            .iter()
            .map(|generation| generation.generation_id.clone())
            .collect::<BTreeSet<_>>(),
        generations[1..generations.len() - 1]
            .iter()
            .cloned()
            .collect()
    );
}

#[test]
fn code_generation_retention_emits_durable_reclaim_receipt() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    retention_generations(&fixture, store.path(), 5);
    remove_historical_pointer_entries(store.path());

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(70),
        None,
    )
    .expect("apply retention");

    let receipt = report.receipt.expect("applied reclaim receipt");
    assert_eq!(receipt.deleted_generations.len(), 4);
    assert_eq!(
        receipt.reclaimed_bytes,
        receipt
            .deleted_generations
            .iter()
            .map(|generation| generation.size_bytes)
            .sum::<u64>()
    );
    assert!(
        store
            .path()
            .join("code-generation-retention-receipts-v1")
            .join(format!("receipt-{}.json", receipt.receipt_digest))
            .is_file()
    );
    let remaining_generation_files = std::fs::read_dir(store.path().join("code-generations-v1"))
        .expect("generation directory")
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        })
        .count();
    assert_eq!(
        remaining_generation_files, 1,
        "only the active generation survives the reclaim"
    );
}

// --- Code-index scope-root reconciliation ----------------------------------
//
// Generation retention above operates inside one
// `code-index-v1/<sha256(canonical_project_root)>/` scope. These tests cover the
// pass that reconciles the *scopes themselves* against the live project roots,
// which is the only thing that can reach a scope whose root no longer exists.

#[test]
fn stranded_code_index_scope_is_collected_while_its_live_sibling_is_untouched() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let deleted_worktree = fixture.path().join(".claude/worktrees/agent-deadbeef");

    let live_scope = seeded_scope(&fixture, code_index.path(), &live_root, 2);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &deleted_worktree, 2);
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(90),
    )
    .expect("reconcile code-index scope roots");

    assert_eq!(report.collected_scopes.len(), 1);
    assert_eq!(report.plan.live_scope_count, 1);
    assert!(
        !stranded_scope.exists(),
        "a scope whose canonical project root is gone must be collected"
    );
    assert!(
        live_scope.join("active-code-generation-v1.json").is_file(),
        "reconciliation must never touch a scope a live root names"
    );
    let receipt = report.receipt.expect("durable reconciliation receipt");
    assert!(receipt.reclaimed_bytes > 0);
    assert_eq!(
        receipt.reclaimed_bytes,
        report.plan.collectable_scope_bytes()
    );
    assert!(
        code_index
            .path()
            .join("code-index-scope-retention-receipts-v1")
            .join(format!("receipt-{}.json", receipt.receipt_digest))
            .is_file(),
        "collection must leave a durable receipt outside the collected scope"
    );
}

#[test]
fn code_index_scope_matching_a_live_worktree_is_never_collected() {
    use tracedecay_code_index_retention::code_index_generations::{
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, plan_scope_root_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let primary = fixture.path().to_path_buf();
    let linked_worktree = fixture.path().join(".claude/worktrees/agent-live");

    seeded_scope(&fixture, code_index.path(), &primary, 1);
    seeded_scope(&fixture, code_index.path(), &linked_worktree, 1);
    let live_roots = BTreeSet::from([primary, linked_worktree]);

    let plan = plan_scope_root_retention(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        unix_now_secs() + EIGHT_DAYS_SECS,
    )
    .expect("plan scope reconciliation");

    assert_eq!(plan.live_scope_count, 2);
    assert_eq!(
        plan.stranded_scope_count(),
        0,
        "a linked worktree is a live canonical root, not a stranded scope"
    );
    assert_eq!(plan.stranded_scope_bytes(), 0);
}

#[test]
fn code_index_scope_with_a_pending_generation_journal_is_refused() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        StrandedScopeRefusalV1,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let deleted_worktree = fixture.path().join(".claude/worktrees/agent-interrupted");

    seeded_scope(&fixture, code_index.path(), &live_root, 1);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &deleted_worktree, 2);
    // An unfinished generation-retention journal inside the scope. Recovering it
    // belongs to that scope's own owner; collecting the scope would destroy the
    // evidence recovery needs.
    std::fs::write(
        stranded_scope.join(".code-generation-retention-transaction-v1.json"),
        b"{}",
    )
    .expect("seed pending generation journal");
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(91),
    )
    .expect("reconcile code-index scope roots");

    assert!(report.collected_scopes.is_empty());
    assert!(report.receipt.is_none());
    assert!(report.plan.collectable_scopes.is_empty());
    assert_eq!(report.plan.refused_scopes.len(), 1);
    assert_eq!(
        report.plan.refused_scopes[0].refusal,
        StrandedScopeRefusalV1::PendingGenerationRetention
    );
    assert!(
        stranded_scope
            .join("active-code-generation-v1.json")
            .is_file(),
        "a scope mid-transaction must survive reconciliation untouched"
    );
    assert!(
        report.plan.stranded_scope_bytes() > 0,
        "a refused scope is still unreachable storage and must be reported"
    );
}

#[test]
fn freshly_stranded_code_index_scope_is_retained_until_the_age_gate_passes() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let just_removed = fixture.path().join(".claude/worktrees/agent-just-removed");

    seeded_scope(&fixture, code_index.path(), &live_root, 1);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &just_removed, 1);
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs(),
        UtcMicros(92),
    )
    .expect("reconcile code-index scope roots");

    assert!(report.collected_scopes.is_empty());
    assert_eq!(report.plan.retained_immature_scopes.len(), 1);
    assert!(
        stranded_scope.exists(),
        "a scope stranded moments ago may still belong to a worktree being moved"
    );
    assert_eq!(
        report.plan.stranded_scope_count(),
        1,
        "immaturity delays collection; it never hides the bytes"
    );
}

#[test]
fn scope_reconciliation_refuses_to_collect_without_a_proven_live_root_set() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        plan_scope_root_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let scope = seeded_scope(&fixture, code_index.path(), fixture.path(), 2);
    let unproven = BTreeSet::new();

    // "The registry could not be read" and "this profile has no live roots" are
    // indistinguishable at this layer, and one of those readings deletes the
    // whole store. The planner refuses rather than choosing.
    let planned = plan_scope_root_retention(
        code_index.path(),
        &unproven,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        unix_now_secs() + EIGHT_DAYS_SECS,
    );
    let applied = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &unproven,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(93),
    );

    assert!(planned.is_err());
    assert!(applied.is_err());
    assert!(
        scope.join("active-code-generation-v1.json").is_file(),
        "an empty live-root set must never mean every scope is stranded"
    );
}

/// The retention census must stay reachable on stores whose generations are
/// individually larger than any byte budget worth calling cheap. Directory
/// entries are what the census costs; bytes are not.
#[test]
fn oversized_generations_still_produce_a_complete_retention_finding() {
    use tracedecay_code_index_retention::code_index_generations::{
        GenerationDigestVerificationV1, plan_code_generation_retention_with_verification,
    };
    use tracedecay_contracts::doctor::DoctorCoverageCompletenessV1;
    use tracedecay_contracts::storage::{
        CodeGenerationRetentionRecordV1, StorageByteSizeV1, StoreKeyV1,
        code_generation_retention_finding,
    };

    const ONE_GIB: u64 = 1024 * 1024 * 1024;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    retention_generations(&fixture, store.path(), 4);
    // Sparse growth: the manifest prefix each generation is read through is
    // untouched, only the on-disk size a byte budget would have measured.
    for entry in std::fs::read_dir(store.path().join("code-generations-v1"))
        .expect("list sealed generations")
    {
        let entry = entry.expect("sealed generation entry");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(entry.path())
            .expect("open sealed generation");
        file.set_len(ONE_GIB).expect("grow sealed generation");
    }
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let mut pointer: tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read publication pointer"))
            .expect("decode publication pointer");
    // The pointer's index is a referential-integrity check the census still
    // validates, so each entry's recorded size must match its sparsely grown
    // file even though membership no longer holds a generation live.
    for entry in &mut pointer.generation_index {
        entry.size_bytes = ONE_GIB;
    }
    pointer.generation_index_digest = Some(
        tracedecay_code_index_retention::code_index_generations::durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("digest sparse publication index"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(&pointer).expect("encode sparse publication pointer"),
    )
    .expect("write sparse publication pointer");

    let plan = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("metadata-only census must not depend on re-hashing gigabytes");

    assert_eq!(plan.superseded_generations.len(), 3);
    assert_eq!(plan.collectable_generations.len(), 3);
    assert!(
        plan.superseded_generation_bytes() >= 3 * ONE_GIB,
        "the census must report the real footprint, not a budgeted subset"
    );
    assert!(
        plan.collectable_generation_bytes() >= 3 * ONE_GIB,
        "the backlog the finding reports is the real footprint too"
    );

    let record = CodeGenerationRetentionRecordV1 {
        store: StoreKeyV1::new("code-index-v1").expect("valid store key"),
        superseded_generation_count: plan.superseded_generations.len() as u64,
        superseded_generation_bytes: StorageByteSizeV1(plan.superseded_generation_bytes()),
        collectable_generation_count: plan.collectable_generations.len() as u64,
        collectable_generation_bytes: StorageByteSizeV1(plan.collectable_generation_bytes()),
        stranded_scope_count: 0,
        stranded_scope_bytes: StorageByteSizeV1(0),
        superseded_sealed_generation_count: 0,
        superseded_sealed_generation_bytes: StorageByteSizeV1::ZERO,
        abandoned_sealed_staging_count: 0,
        abandoned_sealed_staging_bytes: StorageByteSizeV1::ZERO,
        sealed_head_generation_bytes: StorageByteSizeV1::ZERO,
        live_graph_container_bytes: StorageByteSizeV1::ZERO,
        deferred_native_retirement_count: 0,
    };
    let finding =
        code_generation_retention_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("the retention finding must be producible at this size");

    assert!(
        finding.finding().coverage().is_complete(),
        "a byte budget must not downgrade coverage the census actually achieved"
    );
    // Complete coverage of a real backlog, not health: pointer membership no
    // longer holds these three superseded generations live, so the finding a
    // complete census produces at this size is the backlog itself.
    assert_eq!(
        finding.finding().state(),
        tracedecay_contracts::doctor::DoctorEvidenceStateV1::Stale
    );
}

#[test]
fn restart_rejects_corrupt_sealed_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("initial publish"));
    }
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read active pointer"),
    )
    .expect("decode active pointer");
    let generation_path = store
        .path()
        .join("code-generations-v1")
        .join(pointer.generation_file);
    let mut bytes = std::fs::read(&generation_path).expect("read sealed generation");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&generation_path, bytes).expect("corrupt sealed generation");

    let mut reopened = CodeIndexWorktreeSchedulerV1::open(
        test_project_id(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed validation");
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "foreground open must not inspect corrupt sealed bytes"
    );
    assert!(
        reopened.activate_or_reconcile().is_err(),
        "retained activation must reject corrupt sealed state"
    );
    assert!(
        reopened.latest_complete_already_decoded().is_none(),
        "corrupt sealed state never becomes serving state"
    );
}

#[test]
fn restart_rejects_corrupt_partitioned_file_segment() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("initial publish"));
    }
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read active pointer"),
    )
    .expect("decode active pointer");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            store
                .path()
                .join("code-generations-v1")
                .join(pointer.generation_file),
        )
        .expect("read generation manifest"),
    )
    .expect("decode generation manifest");
    let segment_digest = manifest["generation"]["file_segments"][0]["segment_digest"]
        .as_str()
        .and_then(|digest| sha256_hex_suffix(digest))
        .expect("file segment digest");
    let segment_path = store
        .path()
        .join("code-generation-segments-v1")
        .join(format!("segment-{segment_digest}.json"));
    let mut bytes = std::fs::read(&segment_path).expect("read file segment");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&segment_path, bytes).expect("corrupt file segment");

    let mut reopened = CodeIndexWorktreeSchedulerV1::open(
        test_project_id(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers segment validation");
    assert!(
        reopened.activate_or_reconcile().is_err(),
        "retained activation must reject a corrupt file segment"
    );
    assert!(
        reopened.latest_complete_already_decoded().is_none(),
        "a generation with a corrupt segment never becomes serving state"
    );
}

#[test]
fn durable_publication_writes_partitioned_manifest_and_reuses_immutable_targets() {
    let fixture =
        GitFixture::new(&[("src/lib.rs", "pub fn streamed_publication() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    let latest = scheduler
        .latest_complete_already_decoded()
        .expect("published generation remains decoded");
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let pointer: DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
            .expect("decode active pointer");
    let generation_path = store
        .path()
        .join("code-generations-v1")
        .join(&pointer.generation_file);
    let canonical = std::fs::read(&generation_path).expect("read generation manifest");
    let manifest: serde_json::Value =
        serde_json::from_slice(&canonical).expect("decode generation manifest");
    assert_eq!(
        manifest["generation"]["format_revision"], 7,
        "durable publication must emit the partitioned format"
    );
    assert_eq!(
        manifest["generation"]["file_segments"]
            .as_array()
            .expect("partitioned file segments")
            .len(),
        1
    );
    let segment_count = std::fs::read_dir(store.path().join("code-generation-segments-v1"))
        .expect("read segment directory")
        .count();

    let active_generation = latest.generation.manifest().generation_id.clone();
    let mut reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("reopen publication store");
    reopened
        .publish_atomically(
            &latest.generation.sealed_scope(),
            Some(&active_generation),
            Arc::clone(&latest.generation),
        )
        .expect("identical immutable generation republishes");
    assert_eq!(
        reopened
            .seal_evidence_durable_transaction_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an identical evidence pack retry must not repeat a durability transaction"
    );

    assert_eq!(
        std::fs::read(&generation_path).expect("read republished generation"),
        canonical,
        "an existing content-addressed manifest must remain byte exact"
    );
    assert_eq!(
        std::fs::read_dir(store.path().join("code-generation-segments-v1"))
            .expect("read segment directory")
            .count(),
        segment_count,
        "identical republication must not duplicate content-addressed segments"
    );
    assert!(
        std::fs::read_dir(store.path().join("code-generations-v1"))
            .expect("read generation directory")
            .all(|entry| {
                !entry
                    .expect("generation directory entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".generation-publication.")
            }),
        "successful publication must not retain a streamed temporary file"
    );
}

#[test]
fn restart_rejects_pointer_generation_mismatch() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("initial publish"));
    }
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let mut pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
            .expect("decode active pointer");
    pointer["generation_id"] = serde_json::Value::String("generation.mismatched".to_owned());
    std::fs::write(
        &pointer_path,
        serde_json::to_vec(&pointer).expect("encode mismatched pointer"),
    )
    .expect("write mismatched pointer");

    let mut reopened = CodeIndexWorktreeSchedulerV1::open(
        test_project_id(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed validation");
    assert!(
        reopened.activate_or_reconcile().is_err(),
        "pointer/generation mismatch must fail retained activation"
    );
    assert!(
        reopened.latest_complete_already_decoded().is_none(),
        "a mismatched pointer never becomes serving state"
    );
}
