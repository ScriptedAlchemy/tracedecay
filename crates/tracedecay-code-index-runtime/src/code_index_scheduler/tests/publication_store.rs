use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1, DurableGenerationIndexEntryV1,
    DurablePublicationPointerV1, MAX_CODE_GENERATION_RETENTION_BATCH_V1,
    acquire_code_generation_store_lock, code_generation_segments_root,
    code_text_artifact_staging_root, code_text_artifacts_root,
    durable_generation_index_digest, execute_code_generation_retention_cancellable,
    prepare_next_code_generation_retention_cancellable, run_code_generation_retention,
    try_acquire_code_generation_store_read_lock, withdraw_verified_text_artifact_under_lock,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, ComponentRevision, EphemeralSanitizedQueryViewV1,
    FreshnessVectorDigest, ManifestDigest, PrincipalId, QueryNormalizationRevision,
    RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverOutcome,
    SanitizerRevision, ScoreDomainId, SingleRootScopeV1, TemporalModeV1, UtcMicros,
    VectorWatermark, encode_lowercase_hex, sha256_hex_suffix,
};
use tracedecay_query::retrieval::lexical::LexicalLaneRequest;
use tracedecay_query::retrieval::ports::RetrievalPortError;

use super::{
    EIGHT_DAYS_SECS, GitFixture, RETAINED_REVISION_0,
    execute_scope_retention_with_test_binding_cleanup, published,
    remove_historical_pointer_entries, retention_generations, scheduler, seeded_scope,
    test_project_id, unix_now_secs,
};
use crate::{
    code_index::production::{
        CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexInterruptionV1,
        CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, SEALED_GENERATION_FORMAT_REVISION_V1,
        SealedGenerationSegmentReadV1, UninterruptibleCodeIndexControlV1,
        VerifiedSealedLexicalPageReadV1,
    },
    code_index_scheduler::{
        CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
        scoped_code_index_store_root,
    },
};

struct CancelledCodeIndexControlV1;

impl CodeIndexExecutionControlV1 for CancelledCodeIndexControlV1 {
    fn is_cancelled(&self) -> bool {
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct ExpiredCodeIndexControlV1;

impl CodeIndexExecutionControlV1 for ExpiredCodeIndexControlV1 {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        true
    }
}

#[test]
fn partitioned_reclamation_is_bounded_and_preserves_retained_segments() {
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
    assert_eq!(
        first_manifest["generation"]["format_revision"],
        SEALED_GENERATION_FORMAT_REVISION_V1
    );
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
    let full_second_bytes = std::fs::metadata(&second_manifest_path)
        .expect("second manifest metadata")
        .len()
        .saturating_add(second_components.values().sum::<u64>());
    assert!(
        second_generation_growth.saturating_mul(2) < full_second_bytes,
        "one-line edit added {second_generation_growth} physical bytes versus a \
         {full_second_bytes}-byte full rewrite"
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

    let segment_path = |digest: &str| {
        segment_root.join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest).expect("tagged segment digest")
        ))
    };
    let retained_segment_bytes = first_components
        .keys()
        .chain(second_components.keys())
        .map(|digest| {
            (
                digest.clone(),
                std::fs::read(segment_path(digest)).expect("read retained segment"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let orphan_paths = (0..=(MAX_CODE_GENERATION_RETENTION_BATCH_V1 * 2))
        .map(|index| {
            let bytes = format!("unreferenced segment {index}");
            let digest = encode_lowercase_hex(&Sha256::digest(bytes.as_bytes()));
            let path = segment_root.join(format!("segment-{digest}.json"));
            std::fs::write(&path, bytes).expect("write unreferenced segment");
            path
        })
        .collect::<Vec<_>>();
    let first_generation = CodeGenerationId::new(
        first_pointer["generation_id"]
            .as_str()
            .expect("first generation id"),
    )
    .expect("valid first generation id");
    let second_generation = CodeGenerationId::new(
        second_pointer["generation_id"]
            .as_str()
            .expect("second generation id"),
    )
    .expect("valid second generation id");
    let retained_generations = BTreeSet::from([first_generation.clone()]);
    let interrupted_plan = prepare_next_code_generation_retention_cancellable(
        store.path(),
        &retained_generations,
        &|| false,
        None,
    )
    .expect("plan interrupted segment sweep");
    let error = execute_code_generation_retention_cancellable(
        store.path(),
        interrupted_plan,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(8_000_000),
        None,
        &|| orphan_paths.iter().any(|path| !path.exists()),
    )
    .expect_err("interrupt segment sweep after its first unlink");
    assert!(
        matches!(error, CodeGenerationRetentionErrorV1::Cancelled),
        "the interrupted sweep must preserve cancellation: {error}"
    );
    assert_eq!(
        orphan_paths.iter().filter(|path| !path.exists()).count(),
        1,
        "the injected interruption must land between segment unlinks"
    );

    drop(scheduler);
    let reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("reopen after interrupted segment sweep");
    for generation in [&first_generation, &second_generation] {
        assert!(
            reopened
                .load_generation(generation)
                .expect("load retained generation after interrupted sweep")
                .is_some(),
            "retained generation {generation} must remain restart-readable"
        );
    }
    drop(reopened);
    for (digest, expected) in &retained_segment_bytes {
        assert_eq!(
            std::fs::read(segment_path(digest)).expect("read retained segment after interruption"),
            *expected,
            "retained segment {digest} must remain byte-identical after interruption"
        );
    }

    let mut completed_sweeps = 0;
    while orphan_paths.iter().any(|path| path.exists()) {
        assert!(completed_sweeps < 3, "segment reclamation must converge");
        let before = orphan_paths.iter().filter(|path| path.exists()).count();
        let report = run_code_generation_retention(
            store.path(),
            &retained_generations,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(8_100_000 + completed_sweeps),
            None,
        )
        .expect("resume bounded segment reclamation");
        assert!(
            report.deleted_generations.is_empty(),
            "the retained superseded generation must not be collected"
        );
        let after = orphan_paths.iter().filter(|path| path.exists()).count();
        assert!(
            before - after <= MAX_CODE_GENERATION_RETENTION_BATCH_V1,
            "one maintenance unit reclaimed more than its segment batch"
        );
        completed_sweeps += 1;
    }
    assert_eq!(
        completed_sweeps, 2,
        "the interrupted sweep must resume as two bounded maintenance units"
    );

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(9_000_000),
        None,
    )
    .expect("collect retired partitioned generation");
    assert_eq!(report.deleted_generations.len(), 1);
    for (digest, expected) in retained_segment_bytes {
        let path = segment_path(&digest);
        if second_components.contains_key(&digest) {
            assert_eq!(
                std::fs::read(path).expect("read active retained segment"),
                expected,
                "active segment {digest} must remain byte-identical"
            );
        } else {
            assert!(
                !path.exists(),
                "segment {digest} referenced only by the retired generation must be reclaimed"
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(&segment_root)
            .expect("read reclaimed segment directory")
            .count(),
        second_components.len(),
        "only segments referenced by the active manifest may remain"
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
    assert!(matches!(
        source.next_page(&CancelledCodeIndexControlV1),
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(source.cursor(), &initial_cursor);
    assert!(matches!(
        source.next_page(&ExpiredCodeIndexControlV1),
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::DeadlineExceeded
        ))
    ));
    assert_eq!(source.cursor(), &initial_cursor);
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let result = source.next_page(&UninterruptibleCodeIndexControlV1);
        sent.send((source, result)).expect("return lexical source");
    });
    // An immutable segment read holds the store as a shared reader: while a
    // publication or retention writer owns the exclusive lock the reader
    // waits (bounded by that hold) instead of failing typed and abandoning
    // the projection pass; once the writer releases, the same page is served
    // from the unchanged cursor.
    let held = received.recv_timeout(Duration::from_millis(500));
    assert!(
        matches!(held, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "an exclusive publication hold must park the lexical reader, not fail it"
    );
    drop(lock);
    reader.join().expect("lexical reader exits");
    let (mut source, result) = received
        .recv_timeout(Duration::from_secs(2))
        .expect("the lexical reader resumes once the publication hold is released");
    assert!(matches!(
        result.expect("read after publication unlock"),
        VerifiedSealedLexicalPageReadV1::Page(_)
    ));
    assert_ne!(source.cursor(), &initial_cursor);
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
fn generation_decode_shares_store_and_refuses_exclusive_writer_contention() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn ready() -> usize { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish generation"));
    drop(scheduler);

    let open_cold = |root: &Path, project: &Path| {
        super::super::DaemonCodeIndexPublicationStoreV1::new(
            root,
            project,
            SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
                .expect("sanitizer revision"),
        )
        .expect("open cold publication store")
    };

    // `new` takes the exclusive store lock while it records the scope root, so
    // the store must be open before either probe hold or this test deadlocks.
    let publication = open_cold(store.path(), fixture.path());
    let shared = try_acquire_code_generation_store_read_lock(store.path())
        .expect("shared hold")
        .expect("shared hold not contended");
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        sent.send(publication.load_active_shared())
            .expect("return shared decode");
    });
    let decoded = received
        .recv_timeout(Duration::from_secs(5))
        .expect("a shared generation decode must finish while another shared hold is still taken");
    assert!(
        decoded.expect("shared decode").is_some(),
        "published generation must decode under a shared store hold"
    );
    drop(shared);
    reader.join().expect("shared reader exits");

    let publication = open_cold(store.path(), fixture.path());
    let exclusive = acquire_code_generation_store_lock(store.path()).expect("exclusive hold");
    assert!(
        matches!(
            publication.load_active_shared(),
            Err(CodeIndexPublicationStoreErrorV1::Unavailable(message))
                if message.contains("contended")
        ),
        "an unscoped generation decode must fail retryably instead of blocking indefinitely"
    );
    drop(exclusive);
    assert!(
        publication
            .load_active_shared()
            .expect("decode after unlock")
            .is_some()
    );
}

/// 1,600 functions named `{prefix}_{index}` whose bodies apply `operator`.
/// A clean generation's evidence is implied by its own symbols and chunks
/// and fits one page; a successor that changes every body keeps one whole
/// lineage row per function, which spans several.
fn evidence_fixture_source(prefix: &str, operator: char) -> String {
    (0..1_600).fold(String::new(), |mut source, index| {
        writeln!(
            source,
            "pub fn {prefix}_{index}(value: usize) -> usize {{ value {operator} {index} }}"
        )
        .expect("write generated fixture source");
        source
    })
}

/// Publish the fixture, then a successor that changes every function body.
fn publish_multi_page_evidence(
    fixture: &GitFixture,
    scheduler: &mut CodeIndexWorktreeSchedulerV1,
    prefix: &str,
) {
    published(
        scheduler
            .reconcile_now()
            .expect("publish the clean generation"),
    );
    fixture.edit("src/evidence.rs", &evidence_fixture_source(prefix, '*'));
    fixture.commit_all("change every evidence body");
    published(
        scheduler
            .reconcile_now()
            .expect("publish multi-page generation"),
    );
}

#[test]
fn multi_page_evidence_uses_one_durable_pack_and_survives_restart() {
    let source = evidence_fixture_source("evidence", '+');
    let fixture = GitFixture::new(&[("src/evidence.rs", source.as_str())]);
    let store = TempDir::new().expect("store root");
    let (generation_id, evidence_pack_path) = {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        publish_multi_page_evidence(&fixture, &mut scheduler, "evidence");
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
        for descriptor in manifest["generation"]["file_segments"]
            .as_array()
            .expect("file segment descriptors")
            .iter()
            .map(|descriptor| &descriptor["segment_digest"])
            .chain([&manifest["generation"]["generation_evidence"]["segment_digest"]])
        {
            let digest = sha256_hex_suffix(descriptor.as_str().expect("segment digest"))
                .expect("tagged segment digest");
            assert!(
                segments_root
                    .join(format!("segment-{digest}.json"))
                    .is_file(),
                "every file segment and the one evidence pack are durable objects"
            );
        }
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

    let scope = store
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("store scope name");
    let orphan_path = segments_root.join(format!(".evidence-pack-publication.{scope}.4242.tmp"));
    std::fs::write(&orphan_path, b"crash orphan").expect("write crash orphan");
    // Linked worktrees share this directory; a sibling's pack may be in flight.
    let sibling_path = segments_root.join(".evidence-pack-publication.sibling.4242.tmp");
    std::fs::write(&sibling_path, b"sibling in flight").expect("write sibling pack");
    let _reopened = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("restart publication store");
    assert!(
        !orphan_path.exists(),
        "restart must durably clean an abandoned evidence pack"
    );
    assert!(
        sibling_path.exists(),
        "restart must not remove another worktree scope's evidence pack"
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
fn publishing_many_new_segments_syncs_the_segments_directory_once() {
    // Eight distinct files seal to eight distinct new segment files. POSIX
    // durability only requires the containing directory to be fsynced once
    // after all of those segments are renamed into place, so a healthy
    // publish must not pay for one directory fsync per segment.
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
    let published_segments = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let segment_observer = Arc::clone(&published_segments);
    let directory_syncs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sync_observer = Arc::clone(&directory_syncs);
    let mut publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        target_store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open target publication store")
    .with_seal_segment_observer_for_test(Arc::new(move || {
        segment_observer.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }))
    .with_segments_dir_sync_observer_for_test(Arc::new(move || {
        sync_observer.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }));

    publication
        .publish_atomically(&generation.sealed_scope(), None, Arc::clone(&generation))
        .expect("publish a fresh multi-segment generation");

    let segment_count = published_segments.load(std::sync::atomic::Ordering::Acquire);
    assert!(
        segment_count >= 8,
        "expected at least 8 newly durable segments, saw {segment_count}"
    );
    assert_eq!(
        directory_syncs.load(std::sync::atomic::Ordering::Acquire),
        1,
        "one publish writing {segment_count} new segments must sync the segments \
         directory exactly once, not once per segment"
    );
}

#[test]
fn evidence_pack_failure_after_pages_never_publishes_manifest_or_pointer() {
    let source = evidence_fixture_source("failed_evidence", '+');
    let fixture = GitFixture::new(&[("src/evidence.rs", source.as_str())]);
    let source_store = TempDir::new().expect("source store root");
    let generation = {
        let mut scheduler = scheduler(
            &fixture,
            source_store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        publish_multi_page_evidence(&fixture, &mut scheduler, "failed_evidence");
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
fn code_generation_retention_never_sweeps_retained_readable_source() {
    use tracedecay_code_index_retention::code_index_generations::{
        CodeGenerationRetentionModeV1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 6);
    remove_historical_pointer_entries(store.path());
    let retained_readable = BTreeSet::from([generations[0].clone()]);

    let report = run_code_generation_retention(
        store.path(),
        &retained_readable,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(60),
        None,
    )
    .expect("apply retention");

    let retained_generation = report
        .plan
        .superseded_generations
        .iter()
        .find(|generation| generation.generation_id == generations[0])
        .expect("retained-readable generation was inventoried");
    assert!(
        store
            .path()
            .join("code-generations-v1")
            .join(&retained_generation.generation_file)
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

#[derive(Debug, PartialEq, Eq)]
enum RestartDecodeStatusV1 {
    Abstained,
    Decoded,
    SourceCommitmentRefused,
}

#[derive(Debug, PartialEq, Eq)]
enum RestartIdentityStatusV1 {
    NotReached,
    Matched,
    Mismatched,
}

#[derive(Debug, PartialEq, Eq)]
struct RestartDecodeCensusV1 {
    partitioned: RestartDecodeStatusV1,
    sanitizer: RestartIdentityStatusV1,
    pointer: RestartIdentityStatusV1,
}

fn restart_decode_census(
    store: &Path,
    pointer: &DurablePublicationPointerV1,
) -> RestartDecodeCensusV1 {
    let generation_path = store
        .join("code-generations-v1")
        .join(&pointer.generation_file);
    let generation_bytes = std::fs::read(&generation_path).expect("read generation manifest");
    let segments = code_generation_segments_root(store);
    let partitioned = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        &generation_bytes,
        |request, buffer| {
            let (digest, size, offset, length) = match request {
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                    (digest, size_bytes, 0, size_bytes)
                }
                SealedGenerationSegmentReadV1::Range {
                    digest,
                    size_bytes,
                    offset,
                    length,
                } => (digest, size_bytes, offset, length),
            };
            let digest = sha256_hex_suffix(digest.as_str()).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "restart census segment digest is not canonical".to_owned(),
                )
            })?;
            let path = segments.join(format!("segment-{digest}.json"));
            let mut file = File::open(path).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "restart census segment open failed: {error}"
                ))
            })?;
            if file
                .metadata()
                .map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "restart census segment metadata failed: {error}"
                    ))
                })?
                .len()
                != size
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "restart census segment size does not match its manifest".to_owned(),
                ));
            }
            let length = usize::try_from(length).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "restart census segment length is invalid: {error}"
                ))
            })?;
            buffer.resize(length, 0);
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(buffer))
                .map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "restart census segment read failed: {error}"
                    ))
                })
        },
    );
    let generation = match partitioned {
        Ok(generation) => generation,
        Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_)) => {
            return RestartDecodeCensusV1 {
                partitioned: RestartDecodeStatusV1::Abstained,
                sanitizer: RestartIdentityStatusV1::NotReached,
                pointer: RestartIdentityStatusV1::NotReached,
            };
        }
        Err(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable) => {
            return RestartDecodeCensusV1 {
                partitioned: RestartDecodeStatusV1::SourceCommitmentRefused,
                sanitizer: RestartIdentityStatusV1::NotReached,
                pointer: RestartIdentityStatusV1::NotReached,
            };
        }
        Err(error) => panic!("partitioned restart census failed: {error}"),
    };
    let sanitizer = if generation.manifest().sanitizer_revision.as_str()
        == tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1
    {
        RestartIdentityStatusV1::Matched
    } else {
        RestartIdentityStatusV1::Mismatched
    };
    let pointer_matches = generation.manifest().generation_id.as_str() == pointer.generation_id
        && generation.snapshot().content_identity.as_str() == pointer.snapshot_content_identity
        && generation.projection().publication_digest().as_str() == pointer.publication_digest
        && generation.manifest().seal.sealed_at.0 == pointer.sealed_at_micros;
    RestartDecodeCensusV1 {
        partitioned: RestartDecodeStatusV1::Decoded,
        sanitizer,
        pointer: if pointer_matches {
            RestartIdentityStatusV1::Matched
        } else {
            RestartIdentityStatusV1::Mismatched
        },
    }
}

#[test]
fn restart_decode_census_reaches_partitioned_decode_and_matches_durable_identity() {
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

    assert_eq!(
        restart_decode_census(store.path(), &pointer),
        RestartDecodeCensusV1 {
            partitioned: RestartDecodeStatusV1::Decoded,
            sanitizer: RestartIdentityStatusV1::Matched,
            pointer: RestartIdentityStatusV1::Matched,
        },
        "shared premise: a current partitioned generation must decode and reach exact sanitizer and pointer checks"
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
        manifest["generation"]["format_revision"], SEALED_GENERATION_FORMAT_REVISION_V1,
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

#[derive(Clone)]
struct CapturedLogWriter {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl std::io::Write for CapturedLogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

fn captured_tracing<T>(scope: impl FnOnce() -> T) -> (T, String) {
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .with_ansi(false)
        .with_writer(CapturedLogWriter {
            bytes: Arc::clone(&bytes),
        })
        .finish();
    let value = tracing::subscriber::with_default(subscriber, scope);
    let bytes = bytes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (
        value,
        String::from_utf8(bytes).expect("captured tracing is UTF-8"),
    )
}

fn rewrite_active_generation_as_revision_seven(
    store: &Path,
    keeps_census: bool,
) -> DurablePublicationPointerV1 {
    let pointer_path = store.join("active-code-generation-v1.json");
    let mut pointer: DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
            .expect("decode active pointer");
    let generations = store.join("code-generations-v1");
    let superseded = generations.join(&pointer.generation_file);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&superseded).expect("read generation manifest"))
            .expect("decode generation manifest");
    let payload = manifest["generation"]
        .as_object_mut()
        .expect("generation payload");
    payload.insert("format_revision".to_owned(), serde_json::json!(7));
    if !keeps_census {
        payload
            .remove("statistics")
            .expect("a published manifest carries a census to remove");
    }
    manifest["state_digest"] = serde_json::json!(format!(
        "sha256:{}",
        encode_lowercase_hex(&Sha256::digest(
            serde_json::to_vec(&manifest["generation"]).expect("retired payload bytes")
        ))
    ));

    let bytes = serde_json::to_vec(&manifest).expect("retired manifest bytes");
    let file_digest = format!("sha256:{}", encode_lowercase_hex(&Sha256::digest(&bytes)));
    let generation_file = format!(
        "generation-{}.json",
        file_digest
            .strip_prefix("sha256:")
            .expect("prefixed file digest")
    );
    std::fs::write(generations.join(&generation_file), &bytes).expect("write retired manifest");
    std::fs::remove_file(&superseded).expect("remove superseded manifest");

    for entry in &mut pointer.generation_index {
        if entry.generation_id == pointer.generation_id {
            entry.generation_file = generation_file.clone();
            entry.state_digest = file_digest.clone();
            entry.size_bytes = bytes.len() as u64;
        }
    }
    pointer.generation_file = generation_file;
    pointer.state_digest = file_digest;
    write_repaired_pointer(&pointer_path, &mut pointer);
    pointer
}

fn write_repaired_pointer(pointer_path: &Path, pointer: &mut DurablePublicationPointerV1) {
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("generation index digest"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(pointer).expect("encode pointer"),
    )
    .expect("write pointer");
}

#[test]
fn retired_sealed_manifest_revision_is_rebuilt_and_logged() {
    for keeps_census in [true, false] {
        let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
        let store = TempDir::new().expect("store root");
        let retired_generation_id = {
            let mut scheduler = scheduler(
                &fixture,
                store.path().to_path_buf(),
                Arc::new(SharedCodeIndexBytePoolV1::default()),
            );
            published(scheduler.reconcile_now().expect("initial publish"));
            scheduler
                .latest_complete_already_decoded()
                .expect("published generation remains decoded")
                .generation
                .manifest()
                .generation_id
                .clone()
        };
        rewrite_active_generation_as_revision_seven(store.path(), keeps_census);

        let mut reopened = CodeIndexWorktreeSchedulerV1::open(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("foreground open defers sealed validation");
        let (outcome, log) = captured_tracing(|| reopened.activate_or_reconcile());
        published(outcome.expect("a retired revision must rebuild, not fail activation"));
        assert!(
            log.contains("sealed_format_revision=7"),
            "the rebuild must name the retired revision it refused (census={keeps_census}): {log}"
        );

        let rebuilt = reopened
            .latest_complete_already_decoded()
            .expect("the rebuilt generation serves");
        assert_ne!(
            rebuilt.generation.manifest().generation_id,
            retired_generation_id,
            "a retired generation is replaced, never re-served"
        );
        drop(rebuilt);

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
                    .join(&pointer.generation_file),
            )
            .expect("read rebuilt manifest"),
        )
        .expect("decode rebuilt manifest");
        assert_eq!(
            manifest["generation"]["format_revision"],
            serde_json::json!(
                tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1
            ),
            "the rebuild must converge on the revision this build writes"
        );
        assert!(
            manifest["generation"]["statistics"].is_object(),
            "the current revision carries its census as a required field"
        );
    }
}

#[test]
fn publication_over_an_undecodable_active_generation_refuses_a_moved_pointer() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    let seeded = scheduler
        .latest_complete_already_decoded()
        .expect("published generation remains decoded")
        .generation;
    let scope = seeded.sealed_scope();
    drop(scheduler);
    let observed = rewrite_active_generation_as_revision_seven(store.path(), true);

    let publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open publication store over a retired generation");

    let pointer_path = store.path().join("active-code-generation-v1.json");
    type PointerMutation = fn(&mut DurablePublicationPointerV1);
    let mutations: [(&str, PointerMutation); 3] = [
        ("generation id", |pointer| {
            let moved = "generation.v1.moved-under-the-writer".to_owned();
            for entry in &mut pointer.generation_index {
                if entry.generation_id == pointer.generation_id {
                    entry.generation_id = moved.clone();
                }
            }
            pointer.generation_id = moved;
        }),
        ("state digest", |pointer| {
            let moved = format!("sha256:{}", "5".repeat(64));
            for entry in &mut pointer.generation_index {
                if entry.state_digest == pointer.state_digest {
                    entry.state_digest = moved.clone();
                }
            }
            pointer.state_digest = moved;
        }),
        ("generation file", |pointer| {
            let moved = format!("generation-{}.json", "6".repeat(64));
            for entry in &mut pointer.generation_index {
                if entry.generation_file == pointer.generation_file {
                    entry.generation_file = moved.clone();
                }
            }
            pointer.generation_file = moved;
        }),
    ];
    for (term, mutate) in mutations {
        let mut moved = observed.clone();
        mutate(&mut moved);
        assert_ne!(moved, observed, "the {term} mutation must move the pointer");
        write_repaired_pointer(&pointer_path, &mut moved);

        let mut refusing = publication.for_undecoded_active_rebuild(&observed);
        let error = refusing
            .publish_atomically(&scope, None, Arc::clone(&seeded))
            .expect_err("a pointer that moved under the writer must refuse the publication");
        assert!(
            matches!(error, CodeIndexPublicationStoreErrorV1::CompareAndSwap),
            "a moved {term} reached the wrong refusal: {error}"
        );
        assert_eq!(
            serde_json::from_slice::<DurablePublicationPointerV1>(
                &std::fs::read(&pointer_path).expect("read active pointer")
            )
            .expect("decode active pointer"),
            moved,
            "a refused publication must leave the {term} it did not expect untouched"
        );
    }

    let mut restored = observed.clone();
    write_repaired_pointer(&pointer_path, &mut restored);
    let mut admitting = publication.for_undecoded_active_rebuild(&observed);
    admitting
        .publish_atomically(&scope, None, seeded)
        .expect("the observed identity still admits the rebuild");
}

fn same_length_publication_pointer(
    generation_id: &str,
    digest_byte: u8,
) -> DurablePublicationPointerV1 {
    let digest = format!("sha256:{}", hex_byte(digest_byte));
    let entry = DurableGenerationIndexEntryV1 {
        generation_id: generation_id.to_owned(),
        snapshot_content_identity: digest.clone(),
        sealed_at_micros: 1,
        size_bytes: 1,
        segment_bytes: 0,
        generation_file: format!("generation-{}.json", hex_byte(digest_byte)),
        state_digest: digest.clone(),
        source_reference: None,
        source_revision: None,
        source_tree: None,
        cardinality: None,
        text_artifact: None,
    };
    let generation_index = vec![entry];
    DurablePublicationPointerV1 {
        generation_id: generation_id.to_owned(),
        snapshot_content_identity: digest.clone(),
        publication_digest: digest.clone(),
        sealed_at_micros: 1,
        generation_file: generation_index[0].generation_file.clone(),
        state_digest: digest,
        generation_index_truncated: false,
        generation_index_digest: Some(
            durable_generation_index_digest(&generation_index, false).expect("index digest"),
        ),
        generation_index,
    }
}

fn hex_byte(byte: u8) -> String {
    format!("{byte:02x}{}", "ab".repeat(31))
}

#[test]
fn publication_pointer_memo_follows_bytes_when_size_and_mtime_stay_put() {
    let store = TempDir::new().expect("store root");
    let project = TempDir::new().expect("project root");
    let publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        project.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open publication store");
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let first = same_length_publication_pointer("generation.memo-aaaa", 0x11);
    let second = same_length_publication_pointer("generation.memo-bbbb", 0x22);
    let first_bytes = serde_json::to_vec(&first).expect("encode first pointer");
    let second_bytes = serde_json::to_vec(&second).expect("encode second pointer");
    assert_eq!(
        first_bytes.len(),
        second_bytes.len(),
        "the replacement must not be distinguishable by size"
    );
    assert_ne!(first_bytes, second_bytes);
    std::fs::write(&pointer_path, &first_bytes).expect("write first pointer");
    let loaded = publication
        .read_publication_pointer()
        .expect("read first pointer")
        .expect("first pointer exists");
    assert_eq!(loaded.generation_id, first.generation_id);

    let mtime = std::fs::metadata(&pointer_path)
        .expect("pointer metadata")
        .modified()
        .expect("pointer mtime");
    std::fs::write(&pointer_path, &second_bytes).expect("replace pointer bytes");
    let file = std::fs::File::options()
        .write(true)
        .open(&pointer_path)
        .expect("reopen pointer");
    file.set_modified(mtime).expect("restore pointer mtime");
    drop(file);
    let replaced = std::fs::metadata(&pointer_path).expect("replaced metadata");
    assert_eq!(replaced.len(), first_bytes.len() as u64);
    assert_eq!(replaced.modified().ok(), Some(mtime));

    let reread = publication
        .read_publication_pointer()
        .expect("reread pointer by content")
        .expect("replaced pointer exists");
    assert_eq!(
        reread.generation_id, second.generation_id,
        "equal size and mtime must not reuse the previous pointer"
    );
}

#[test]
fn stale_pointer_commit_does_not_replace_a_changed_active_pointer() {
    let store = TempDir::new().expect("store root");
    let project = TempDir::new().expect("project root");
    let publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        store.path(),
        project.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open publication store");
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let observed = same_length_publication_pointer("generation.observed", 0x31);
    let observed_bytes = serde_json::to_vec(&observed).expect("encode observed pointer");
    let replacement = same_length_publication_pointer("generation.replacement", 0x32);
    let replacement_bytes = serde_json::to_vec(&replacement).expect("encode replacement pointer");
    std::fs::write(&pointer_path, &observed_bytes).expect("write observed pointer");
    let store_lock = acquire_code_generation_store_lock(store.path()).expect("store lock");

    publication
        .commit_observed_pointer(
            &store_lock,
            Some(&observed_bytes),
            &replacement,
            &replacement_bytes,
        )
        .expect("matching observation publishes");
    assert_eq!(
        std::fs::read(&pointer_path).expect("published pointer"),
        replacement_bytes
    );

    std::fs::write(&pointer_path, b"{").expect("truncate active pointer");
    let error = publication
        .commit_observed_pointer(
            &store_lock,
            Some(&replacement_bytes),
            &observed,
            &observed_bytes,
        )
        .expect_err("a stale observation must not publish");
    assert!(
        matches!(
            error,
            CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_)
        ),
        "corrupt pointer is a closed publication failure, not a rewrite: {error:?}"
    );
    assert_eq!(
        std::fs::read(&pointer_path).expect("faulted pointer remains"),
        b"{",
        "the truncated pointer must still be the file"
    );
}

/// The segment digests one scope's active manifest names: its file segments
/// and, last, its evidence pack.
fn active_segment_digests(scope: &Path) -> Vec<String> {
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scope.join("active-code-generation-v1.json")).expect("active pointer"),
    )
    .expect("decode active pointer");
    let manifest = std::fs::read(
        scope
            .join("code-generations-v1")
            .join(&pointer.generation_file),
    )
    .expect("active manifest");
    CodeIndexPublishedGenerationV1::partitioned_segment_identities(&manifest)
        .expect("segment identities")
        .into_iter()
        .map(|identity| {
            sha256_hex_suffix(identity.digest.as_str())
                .expect("sha256 segment digest")
                .to_owned()
        })
        .collect()
}

fn segment_files(segments_root: &Path) -> BTreeSet<String> {
    std::fs::read_dir(segments_root)
        .expect("read shared segments")
        .map(|entry| {
            entry
                .expect("segment entry")
                .file_name()
                .into_string()
                .expect("UTF-8 segment name")
        })
        .filter_map(|name| {
            name.strip_prefix("segment-")
                .and_then(|name| name.strip_suffix(".json"))
                .map(str::to_owned)
        })
        .collect()
}

/// A primary checkout and a linked worktree of it, mounted as two scopes of
/// one project's `code-index-v1/`, both published at the same tree.
struct LinkedWorktreeScopesV1 {
    _first: GitFixture,
    _linked_root: TempDir,
    linked: PathBuf,
    _store: TempDir,
    code_index_root: PathBuf,
    first_scope: PathBuf,
    linked_scope: PathBuf,
}

fn publish_linked_worktree_scopes(files: &[(&str, &str)]) -> LinkedWorktreeScopesV1 {
    let first = GitFixture::new(files);
    let linked_root = TempDir::new_in(super::canonical_temp_root()).expect("linked root");
    let linked = linked_root.path().join("linked");
    super::git(
        first.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().expect("linked path"),
            "main",
        ],
    );
    let store = TempDir::new().expect("project store");
    let code_index_root = store.path().join("code-index-v1");
    std::fs::create_dir_all(&code_index_root).expect("project code-index root");
    let first_scope = scoped_code_index_store_root(&code_index_root, first.path());
    let linked_scope = scoped_code_index_store_root(&code_index_root, &linked);
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for (root, scope) in [
        (first.path(), &first_scope),
        (linked.as_path(), &linked_scope),
    ] {
        let mut scheduler = registry
            .open_worktree(test_project_id(), root, scope.clone())
            .expect("open worktree scheduler");
        published(scheduler.reconcile_now().expect("publish worktree"));
    }
    LinkedWorktreeScopesV1 {
        _first: first,
        _linked_root: linked_root,
        linked,
        _store: store,
        code_index_root,
        first_scope,
        linked_scope,
    }
}

#[test]
fn linked_worktrees_that_seal_identical_files_share_one_segment_per_file() {
    // Several bodies per file: in memory they sort by worktree-specific
    // symbol occurrences, so the segment must not persist that order.
    let scopes = publish_linked_worktree_scopes(&[
        (
            "src/lib.rs",
            "pub fn alpha(value: u32) -> u32 { value + 1 }\n\
             pub fn beta(value: u32) -> u32 { value * 2 }\n\
             pub fn gamma(value: u32) -> u32 { value - 3 }\n\
             pub fn delta(value: u32) -> u32 { value / 4 }\n\
             pub fn epsilon(value: u32) -> u32 { value % 5 }\n\
             pub fn zeta(value: u32) -> u32 { value ^ 6 }\n\
             pub fn eta(value: u32) -> u32 { value | 7 }\n\
             pub fn theta(value: u32) -> u32 { value & 8 }\n",
        ),
        (
            "src/other.rs",
            "pub fn other(value: u32) -> u32 { value + 1 }\n",
        ),
    ]);
    let segments_root = scopes.code_index_root.join("code-generation-segments-v1");
    assert_eq!(
        code_generation_segments_root(&scopes.first_scope),
        segments_root
    );
    assert_eq!(
        code_generation_segments_root(&scopes.linked_scope),
        segments_root
    );
    for scope in [&scopes.first_scope, &scopes.linked_scope] {
        assert!(
            !scope.join("code-generation-segments-v1").exists(),
            "a worktree scope holds no segments of its own"
        );
    }

    let mut first = active_segment_digests(&scopes.first_scope);
    let mut linked = active_segment_digests(&scopes.linked_scope);
    let first_evidence = first.pop().expect("first evidence pack");
    let linked_evidence = linked.pop().expect("linked evidence pack");
    assert_eq!(first.len(), 2, "one file segment per source file");
    assert_eq!(
        first, linked,
        "identical files seal to identical, worktree-independent segments"
    );
    assert_ne!(first_evidence, linked_evidence);
    let mut expected = first.into_iter().collect::<BTreeSet<_>>();
    expected.extend([first_evidence, linked_evidence]);
    assert_eq!(
        segment_files(&segments_root),
        expected,
        "the project stores each shared file segment exactly once"
    );
}

#[test]
fn retiring_one_worktree_keeps_the_segments_its_sibling_still_names() {
    let scopes =
        publish_linked_worktree_scopes(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    // The linked worktree also seals a file the primary checkout does not.
    super::write(
        &scopes.linked,
        "src/only_linked.rs",
        "pub fn only_linked() {}\n",
    );
    super::git(&scopes.linked, &["add", "-A"]);
    super::git(&scopes.linked, &["commit", "-qm", "linked-only file"]);
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut linked_scheduler = registry
        .open_worktree(
            test_project_id(),
            &scopes.linked,
            scopes.linked_scope.clone(),
        )
        .expect("reopen linked scheduler");
    linked_scheduler.notify_path(scopes.linked.join("src/only_linked.rs"));
    published(
        linked_scheduler
            .reconcile_now()
            .expect("publish linked-only file"),
    );
    drop(linked_scheduler);

    let segments_root = code_generation_segments_root(&scopes.first_scope);
    let first = active_segment_digests(&scopes.first_scope);
    let linked = active_segment_digests(&scopes.linked_scope);
    let retain = |scope: &Path| {
        run_code_generation_retention(
            scope,
            &BTreeSet::new(),
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(unix_now_secs() * 1_000_000),
            None,
        )
        .expect("retention over shared segments")
    };
    // The linked scope's superseded generation retires, and a sweep from
    // the primary scope must still mark everything the linked manifest names.
    retain(&scopes.linked_scope);
    retain(&scopes.first_scope);
    let present = segment_files(&segments_root);
    for digest in first.iter().chain(&linked) {
        assert!(
            present.contains(digest),
            "a sweep from one scope must keep what a sibling scope names"
        );
    }

    // Collecting the linked scope strands only what it alone named.
    std::fs::remove_dir_all(&scopes.linked_scope).expect("collect linked scope");
    retain(&scopes.first_scope);
    let present = segment_files(&segments_root);
    for digest in &first {
        assert!(
            present.contains(digest),
            "the primary scope keeps its segments"
        );
    }
    let linked_only = linked
        .iter()
        .filter(|digest| !first.contains(digest))
        .collect::<Vec<_>>();
    assert!(
        linked_only.len() >= 2,
        "the linked scope named at least its own file segment and evidence pack"
    );
    for digest in linked_only {
        assert!(
            !present.contains(digest),
            "a segment only the collected scope named is swept"
        );
    }
}

/// Serve one scope's text artifact to completion and return the descriptor
/// its active generation names.
fn serve_scope_text(
    worktree: &Path,
    scope: &Path,
) -> tracedecay_code_index_retention::code_index_generations::DurableCodeTextArtifactDescriptorV1
{
    serve_scope_text_with_hits(worktree, scope, "alpha").0
}

/// [`serve_scope_text`], plus the route-independent identity of every
/// lexical hit the served artifact returns for `term`.
fn serve_scope_text_with_hits(
    worktree: &Path,
    scope: &Path,
    term: &str,
) -> (
    tracedecay_code_index_retention::code_index_generations::DurableCodeTextArtifactDescriptorV1,
    Vec<String>,
) {
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut scheduler = registry
        .open_worktree(test_project_id(), worktree, scope.to_path_buf())
        .expect("open worktree scheduler");
    scheduler.reconcile_now().expect("adopt published generation");
    let latest = scheduler.latest_complete().expect("published generation");
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("advance text-artifact build")
    {
        passes += 1;
        assert!(passes < 10_000, "the text-artifact build never completed");
    }
    let generation = latest.generation().manifest().generation_id.clone();
    let owners = latest.production_query_owners().expect("text query owners");
    let base = RetrievalRequest {
        principal: PrincipalId::new("principal.shared-artifact").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: latest.generation().manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: latest.generation().snapshot().repository.clone(),
                worktree: latest.generation().snapshot().worktree.clone(),
                reference: latest.generation().snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("freshness digest"),
            authorization_revision: AuthorizationRevision::new("authorization.shared.v1")
                .expect("authorization revision"),
            captured_at: UtcMicros(1),
        },
        profile_id: "profile.shared-artifact.v1"
            .to_owned()
            .try_into()
            .expect("profile"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 16,
            max_fused_candidates: 16,
            max_hydrated_results: 16,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    };
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        term,
        SanitizerRevision::new("sanitizer.shared-artifact.v1").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.shared-artifact.v1")
            .expect("normalization"),
    )
    .expect("query view");
    let RetrieverOutcome::Complete(batch) = owners
        .retrieve_lexical(&LexicalLaneRequest {
            query_view: &query_view,
            generation,
            whole_terms: std::borrow::Cow::Owned(vec![term.to_owned()]),
            subtokens: std::borrow::Cow::Owned(vec![term.to_owned()]),
            phrases: std::borrow::Cow::Owned(Vec::new()),
            proximities: std::borrow::Cow::Owned(Vec::new()),
            field_filters: std::borrow::Cow::Owned(Vec::new()),
            fuzzy_budget: 0,
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile revision"),
            score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            budget: base.budget,
            base,
            control: &super::ReadyRetrievalControlV1,
        })
        .expect("lexical retrieval")
    else {
        panic!("the served artifact must complete the lexical retrieval");
    };
    let mut hits = batch
        .evidence_by_occurrence
        .values()
        .map(|evidence| {
            format!(
                "{} {:?} {:?} {:?} {:?}",
                evidence.binding.occurrence.file,
                evidence.binding.occurrence.symbol,
                evidence.binding.occurrence.chunk,
                evidence.field_scores_micros,
                evidence.matched_whole_terms,
            )
        })
        .collect::<Vec<_>>();
    hits.sort();
    (active_text_descriptor(scope), hits)
}

fn active_pointer(scope: &Path) -> DurablePublicationPointerV1 {
    serde_json::from_slice(
        &std::fs::read(scope.join("active-code-generation-v1.json")).expect("read pointer"),
    )
    .expect("decode pointer")
}

fn active_text_descriptor(
    scope: &Path,
) -> tracedecay_code_index_retention::code_index_generations::DurableCodeTextArtifactDescriptorV1
{
    let pointer = active_pointer(scope);
    pointer
        .generation_index
        .iter()
        .find(|entry| entry.generation_id == pointer.generation_id)
        .and_then(|entry| entry.text_artifact().cloned())
        .expect("active generation names a text artifact")
}

fn completed_text_artifacts(root: &Path) -> BTreeSet<String> {
    std::fs::read_dir(root)
        .expect("read text artifact root")
        .map(|entry| entry.expect("artifact entry").file_name().into_string().expect("utf-8"))
        .filter(|name| name.starts_with("text-artifact-") && name.ends_with(".bin"))
        .collect()
}

/// The content metadata an artifact stores, which lists its logical paths.
fn artifact_content_metadata(path: &Path) -> String {
    let metadata: Vec<u8> = rusqlite::Connection::open(path)
        .expect("open artifact")
        .query_row(
            "SELECT metadata FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read artifact metadata");
    String::from_utf8(metadata).expect("utf-8 metadata")
}

const CLONE_FIXTURE_SOURCE: &str = "pub fn alpha(value: u32) -> u32 { let a = value + 1; let b = a * 2; let c = b - 3; let d = c / 4; a + b + c + d }\n\
     pub fn beta(input: u32) -> u32 { let a = input + 1; let b = a * 2; let c = b - 3; let d = c / 4; a + b + c + d }\n";

#[test]
fn linked_worktrees_that_index_identical_trees_share_one_text_artifact() {
    let scopes = publish_linked_worktree_scopes(&[
        ("src/lib.rs", CLONE_FIXTURE_SOURCE),
        ("src/other.rs", "pub fn other(value: u32) -> u32 { value + 1 }\n"),
    ]);
    let (first, first_hits) =
        serve_scope_text_with_hits(scopes._first.path(), &scopes.first_scope, "alpha");
    assert!(
        code_text_artifact_staging_root(&scopes.first_scope).is_dir(),
        "the first worktree builds the artifact"
    );
    let (linked, linked_hits) =
        serve_scope_text_with_hits(&scopes.linked, &scopes.linked_scope, "alpha");
    assert!(
        !code_text_artifact_staging_root(&scopes.linked_scope).exists(),
        "a worktree sealing content a sibling already published adopts it without building"
    );
    assert_eq!(first.content_key, linked.content_key);
    assert!(!first_hits.is_empty());
    assert_eq!(first_hits, linked_hits, "the adopted artifact serves identical results");
    let shared_root = code_text_artifacts_root(&scopes.first_scope);
    assert_eq!(shared_root, scopes.code_index_root.join("code-text-artifacts-v1"));
    assert_eq!(code_text_artifacts_root(&scopes.linked_scope), shared_root);
    assert_ne!(
        first.generation_id, linked.generation_id,
        "each worktree seals its own generation"
    );
    assert_eq!(
        (&first.artifact_file, &first.artifact_digest, first.artifact_size_bytes),
        (&linked.artifact_file, &linked.artifact_digest, linked.artifact_size_bytes),
        "identical trees seal byte-identical text artifacts"
    );
    assert_eq!(
        completed_text_artifacts(&shared_root),
        BTreeSet::from([first.artifact_file.clone()]),
        "the project stores the shared artifact exactly once"
    );
    for scope in [&scopes.first_scope, &scopes.linked_scope] {
        assert!(
            !scope.join("code-text-artifacts-v1").exists(),
            "a worktree scope holds no completed artifact of its own"
        );
    }
}

#[test]
fn a_file_that_diverges_in_one_worktree_is_never_served_to_its_sibling() {
    let scopes = publish_linked_worktree_scopes(&[("src/lib.rs", CLONE_FIXTURE_SOURCE)]);
    super::write(
        &scopes.linked,
        "src/only_linked.rs",
        "pub fn only_linked() -> u32 { 7 }\n",
    );
    super::git(&scopes.linked, &["add", "-A"]);
    super::git(&scopes.linked, &["commit", "-qm", "linked-only file"]);
    {
        let registry = CodeIndexSchedulerRegistryV1::new(1);
        let mut linked_scheduler = registry
            .open_worktree(test_project_id(), &scopes.linked, scopes.linked_scope.clone())
            .expect("reopen linked scheduler");
        linked_scheduler.notify_path(scopes.linked.join("src/only_linked.rs"));
        published(
            linked_scheduler
                .reconcile_now()
                .expect("publish linked-only file"),
        );
    }
    let first = serve_scope_text(scopes._first.path(), &scopes.first_scope);
    let linked = serve_scope_text(&scopes.linked, &scopes.linked_scope);
    assert_ne!(first.content_key, linked.content_key);
    assert!(
        code_text_artifact_staging_root(&scopes.linked_scope).is_dir(),
        "one differing file forces the worktree to build its own artifact"
    );
    assert_ne!(
        first.artifact_digest, linked.artifact_digest,
        "diverged trees seal different artifacts"
    );
    let shared_root = code_text_artifacts_root(&scopes.first_scope);
    assert!(
        artifact_content_metadata(&shared_root.join(&linked.artifact_file))
            .contains("src/only_linked.rs")
    );
    assert!(
        !artifact_content_metadata(&shared_root.join(&first.artifact_file))
            .contains("src/only_linked.rs"),
        "the primary worktree's artifact carries none of its sibling's divergent file"
    );
}

#[test]
fn a_shared_artifact_that_fails_verification_is_rebuilt_not_adopted() {
    let scopes = publish_linked_worktree_scopes(&[("src/lib.rs", CLONE_FIXTURE_SOURCE)]);
    let first = serve_scope_text(scopes._first.path(), &scopes.first_scope);
    // The shared file keeps its name and size but no longer holds the bytes
    // its content address names.
    let shared = code_text_artifacts_root(&scopes.first_scope).join(&first.artifact_file);
    let mut bytes = std::fs::read(&shared).expect("read shared artifact");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&shared, &bytes).expect("damage shared artifact");
    let linked = serve_scope_text(&scopes.linked, &scopes.linked_scope);
    assert!(
        code_text_artifact_staging_root(&scopes.linked_scope).is_dir(),
        "a key match that fails verification builds instead of adopting"
    );
    assert_eq!(linked.content_key, first.content_key);
    assert_eq!(linked.artifact_file, first.artifact_file);
    assert_eq!(
        Some(encode_lowercase_hex(&Sha256::digest(
            std::fs::read(&shared).expect("read rebuilt artifact")
        )))
        .as_deref(),
        sha256_hex_suffix(first.artifact_digest.as_str()),
        "the rebuild restores the bytes the content address names"
    );
}

#[test]
fn retiring_one_worktree_keeps_the_text_artifact_its_sibling_references() {
    let scopes = publish_linked_worktree_scopes(&[("src/lib.rs", CLONE_FIXTURE_SOURCE)]);
    let first = serve_scope_text(scopes._first.path(), &scopes.first_scope);
    let linked = serve_scope_text(&scopes.linked, &scopes.linked_scope);
    assert_eq!(first.artifact_file, linked.artifact_file);
    let shared = code_text_artifacts_root(&scopes.first_scope).join(&first.artifact_file);
    let retain = |scope: &Path| {
        run_code_generation_retention(
            scope,
            &BTreeSet::new(),
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(unix_now_secs() * 1_000_000),
            None,
        )
        .expect("retention over shared text artifacts")
    };
    let withdraw = |scope: &Path| {
        let lock = acquire_code_generation_store_lock(scope).expect("scope store lock");
        let pointer = active_pointer(scope);
        let descriptor = active_text_descriptor(scope);
        withdraw_verified_text_artifact_under_lock(&lock, &pointer, &descriptor)
            .expect("withdraw text artifact");
    };
    // A scope that stops naming the shared artifact must not collect it
    // while its sibling still does.
    withdraw(&scopes.first_scope);
    retain(&scopes.first_scope);
    assert!(
        shared.exists(),
        "retention from one scope keeps an artifact a sibling scope names"
    );
    // Collecting the linked scope leaves the artifact unnamed.
    std::fs::remove_dir_all(&scopes.linked_scope).expect("collect linked scope");
    retain(&scopes.first_scope);
    assert!(
        !shared.exists(),
        "an artifact no scope names is collected"
    );
}

#[test]
fn a_sweep_never_collects_segments_a_publication_has_not_yet_named() {
    let fixture = GitFixture::new(&[
        ("src/a.rs", "pub fn a() -> u32 { 1 }\n"),
        ("src/b.rs", "pub fn b() -> u32 { 2 }\n"),
        ("src/c.rs", "pub fn c() -> u32 { 3 }\n"),
    ]);
    let source_store = TempDir::new().expect("source store root");
    let generation = {
        let mut scheduler = scheduler(
            &fixture,
            source_store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("build generation"));
        Arc::clone(
            &scheduler
                .latest_complete_already_decoded()
                .expect("generation remains decoded")
                .generation,
        )
    };
    let project_store = TempDir::new().expect("project store");
    let code_index_root = project_store.path().join("code-index-v1");
    let target_scope = scoped_code_index_store_root(&code_index_root, fixture.path());
    // A sibling worktree scope of the same project whose maintenance pass
    // runs while the target publication is between segments and manifest.
    let sibling_scope = code_index_root.join("a".repeat(64));
    std::fs::create_dir_all(sibling_scope.join("code-generations-v1")).expect("sibling scope");
    std::fs::create_dir_all(&target_scope).expect("target scope");
    let sweeps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_sweeps = Arc::clone(&sweeps);
    let observed_sibling = sibling_scope.clone();
    let mut publication = super::super::DaemonCodeIndexPublicationStoreV1::new(
        &target_scope,
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("open target publication store")
    .with_seal_segment_observer_for_test(Arc::new(move || {
        let outcome = run_code_generation_retention(
            &observed_sibling,
            &BTreeSet::new(),
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(1),
            None,
        )
        .map(|_| ());
        observed_sweeps
            .lock()
            .expect("sweep observations")
            .push(outcome);
    }));

    publication
        .publish_atomically(&generation.sealed_scope(), None, Arc::clone(&generation))
        .expect("publish while sibling sweeps run");

    let sweeps = sweeps.lock().expect("sweep observations");
    assert_eq!(
        sweeps.len(),
        3,
        "one sibling sweep per written file segment"
    );
    assert!(
        sweeps.iter().all(|sweep| matches!(
            sweep,
            Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy)
        )),
        "a sweep that saw unnamed segments must wait out the publication: {sweeps:?}"
    );
    let segments_root = code_generation_segments_root(&target_scope);
    let present = segment_files(&segments_root);
    for digest in active_segment_digests(&target_scope) {
        assert!(present.contains(&digest), "every named segment survived");
    }
    // With the manifest durable the same sweep runs and keeps everything.
    run_code_generation_retention(
        &sibling_scope,
        &BTreeSet::new(),
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(1),
        None,
    )
    .expect("sweep after publication");
    assert_eq!(segment_files(&segments_root), present);
}
