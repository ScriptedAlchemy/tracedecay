//! Exercise the production preparation, filesystem writer, and reader together.
use std::{
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexPreparationPort, CodeIndexProductionErrorV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
};
use tracedecay_domain::{ManifestDigest, ProjectId, SanitizerRevision, sha256_hex_suffix};

use super::{
    DaemonCodeIndexPreparationStateV1, DaemonCodeIndexPreparationV1,
    DaemonCodeIndexPublicationStoreV1, MAX_STAGED_SEGMENT_READ_HANDLES, SharedCodeIndexBytePoolV1,
};
use crate::code_index_scheduler::tests::GitFixture;
use crate::code_index_scheduler::{CodeIndexReconcileOutcomeV1, CodeIndexWorktreeSchedulerV1};

fn source_generation(
    fixture: &GitFixture,
    store_root: &Path,
) -> Arc<CodeIndexPublishedGenerationV1> {
    let mut source = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-tests").expect("project identity"),
        fixture.path(),
        store_root.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("source scheduler");
    assert!(matches!(
        source.reconcile_now().expect("source publication"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    Arc::clone(&source.latest_complete().expect("decode source").generation)
}

fn target_store(root: &Path, fixture: &GitFixture) -> DaemonCodeIndexPublicationStoreV1 {
    DaemonCodeIndexPublicationStoreV1::new(
        root,
        fixture.path(),
        SanitizerRevision::new(tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1)
            .expect("sanitizer revision"),
    )
    .expect("target publication store")
}

fn segment_digest(bytes: &[u8]) -> ManifestDigest {
    ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).expect("canonical digest")
}

fn staged_path(preparation: &DaemonCodeIndexPreparationV1, digest: &ManifestDigest) -> PathBuf {
    let state = preparation.state.lock().expect("preparation state");
    let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
        panic!("preparation must still be open");
    };
    let suffix = sha256_hex_suffix(digest.as_str()).expect("digest suffix");
    let canonical = preparation
        .segments_root
        .join(format!("segment-{suffix}.json"));
    files
        .segments
        .pending
        .get(&canonical)
        .expect("segment was staged, not published")
        .clone()
}

fn pinned_handle_count(preparation: &DaemonCodeIndexPreparationV1) -> usize {
    let state = preparation.state.lock().expect("preparation state");
    let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
        panic!("preparation must still be open");
    };
    files.pinned_segment_reads.len()
}

#[test]
fn staged_segment_range_reads_open_once_per_cached_file_and_revalidate_replacements() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() -> u32 { 7 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let contents = (0..9)
        .map(|index| format!("sealed-staged-segment-{index}-bytes").into_bytes())
        .collect::<Vec<_>>();
    let digests = contents
        .iter()
        .map(|bytes| segment_digest(bytes))
        .collect::<Vec<_>>();
    for (digest, bytes) in digests.iter().zip(&contents) {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
            .expect("write exact staged segment");
    }
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    for offset in 0..16 {
        reader(
            SealedGenerationSegmentReadV1::Range {
                digest: &digests[0],
                size_bytes: contents[0].len() as u64,
                offset,
                length: 4,
            },
            &mut read,
        )
        .expect("read staged segment range");
        assert_eq!(&read, &contents[0][offset as usize..offset as usize + 4]);
    }
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        1,
        "16 overlapping production reads must open the first file once",
    );
    let invalid_range = reader(
        SealedGenerationSegmentReadV1::Range {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
            offset: contents[0].len() as u64 - 1,
            length: 2,
        },
        &mut read,
    );
    assert!(matches!(
        invalid_range,
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert!(read.is_empty(), "a bad range must not expose stale bytes");
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 1);

    // Alternating ranges exercise LRU promotion, not just consecutive reads.
    for offset in 0..16 {
        for index in [0, 1] {
            reader(
                SealedGenerationSegmentReadV1::Range {
                    digest: &digests[index],
                    size_bytes: contents[index].len() as u64,
                    offset,
                    length: 4,
                },
                &mut read,
            )
            .expect("read interleaved staged ranges");
            assert_eq!(
                &read,
                &contents[index][offset as usize..offset as usize + 4]
            );
        }
    }
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 2);
    for (digest, bytes) in digests.iter().zip(&contents).skip(1) {
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest,
                size_bytes: bytes.len() as u64,
            },
            &mut read,
        )
        .expect("read distinct staged segment");
        assert_eq!(&read, bytes);
    }
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        9,
        "nine distinct segments must each need a real file open",
    );
    assert_eq!(
        pinned_handle_count(&preparation),
        MAX_STAGED_SEGMENT_READ_HANDLES
    );
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
        },
        &mut read,
    )
    .expect("reopen the evicted first segment");
    assert_eq!(&read, &contents[0]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        10,
        "an eight-handle bound must evict and reopen the ninth distinct file",
    );

    let path = staged_path(&preparation, &digests[0]);
    let replacement_path = path.with_extension("replacement.tmp");
    std::fs::write(&replacement_path, vec![b'Z'; contents[0].len()])
        .expect("write replacement bytes");
    std::fs::rename(&replacement_path, &path).expect("replace the pinned path");
    for _ in 0..2 {
        read.extend_from_slice(b"stale buffer sentinel");
        let result = reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &digests[0],
                size_bytes: contents[0].len() as u64,
            },
            &mut read,
        );
        assert!(matches!(
            result,
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(read.is_empty(), "replacement and retry must clear output");
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 10);
    }
    assert_eq!(pinned_handle_count(&preparation), 0);
    assert!(matches!(
        preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[1],
            bytes: &contents[1],
        }),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
    ));
    preparation.abort().expect("abort and clean staged files");
    assert!(!path.exists(), "abort removes replaced staged files");
    // An escaped reader must not keep the preparation or its staged bytes live.
    read.extend_from_slice(b"stale after abort");
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &digests[0],
                size_bytes: contents[0].len() as u64,
            },
            &mut read,
        ),
        Err(CodeIndexProductionErrorV1::Publication(_))
    ));
    assert_eq!(read, [] as [u8; 0]);
}

#[test]
fn staged_segment_in_place_mutation_poison_survives_restoration_and_refuses_commit() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation)
        .expect("stage a real valid publication");

    // This auxiliary segment is deliberately outside the valid manifest.
    // Commit must refuse the poisoned transaction, not just a digest mismatch
    // discovered while verifying a segment the manifest happens to reference.
    let bytes = b"original-staged-segment";
    let digest = segment_digest(bytes);
    preparation
        .write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digest,
            bytes,
        })
        .expect("write auxiliary staged segment");
    let opens_before = preparation.staged_segment_read_open_attempts_for_test();
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        },
        &mut read,
    )
    .expect("pin original staged bytes");
    assert_eq!(read.as_slice(), bytes);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        opens_before + 1
    );
    let path = staged_path(&preparation, &digest);
    let before = path.symlink_metadata().expect("original metadata");
    let mut writer = File::options()
        .write(true)
        .open(&path)
        .expect("same inode writer");
    writer
        .write_all(&vec![b'X'; bytes.len()])
        .expect("same-length overwrite");
    // Force a visible timestamp change even on coarse-resolution filesystems.
    writer
        .set_modified(before.modified().expect("mtime") + Duration::from_secs(2))
        .expect("change mtime");
    drop(writer);
    let after = path.symlink_metadata().expect("mutated metadata");
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (after.dev(), after.ino(), after.len())
    );
    assert_ne!(
        before.modified().expect("old mtime"),
        after.modified().expect("new mtime")
    );
    let result = reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        },
        &mut read,
    );
    assert!(matches!(
        result,
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert!(read.is_empty(), "mutated bytes must never escape");
    assert_eq!(pinned_handle_count(&preparation), 0);

    // Restoration cannot unpoison an already-invalidated transaction.
    std::fs::write(&path, bytes).expect("restore original bytes");
    read.extend_from_slice(b"stale after mutation");
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &digest,
                size_bytes: bytes.len() as u64,
            },
            &mut read,
        ),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(read, [] as [u8; 0]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        opens_before + 1
    );
    assert!(matches!(
        preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digest,
            bytes,
        }),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(detail))
            if detail.contains("identity was invalidated")
    ));
    assert!(matches!(
        target.commit(&mut preparation, &publication),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(detail))
            if detail.contains("identity was invalidated")
    ));
    assert!(
        target
            .load_active_manifest()
            .expect("active manifest probe")
            .is_none()
    );
    preparation.abort().expect("abort poisoned preparation");
    assert!(!path.exists());
}

#[test]
fn staged_segment_cold_refusals_clear_output_without_poisoning_valid_reads() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let first = b"first staged bytes";
    let cold = b"cold staged bytes";
    let first_digest = segment_digest(first);
    let cold_digest = segment_digest(cold);
    for (digest, bytes) in [
        (&first_digest, first.as_slice()),
        (&cold_digest, cold.as_slice()),
    ] {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
            .expect("stage segment");
    }
    let missing_digest = segment_digest(b"never staged");
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &first_digest,
            size_bytes: first.len() as u64,
        },
        &mut read,
    )
    .expect("prime buffer and first handle");
    assert_eq!(read.as_slice(), first);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 1);
    for request in [
        SealedGenerationSegmentReadV1::Range {
            digest: &cold_digest,
            size_bytes: cold.len() as u64,
            offset: cold.len() as u64 - 1,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &cold_digest,
            size_bytes: cold.len() as u64 + 1,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &missing_digest,
            size_bytes: 12,
        },
    ] {
        read.extend_from_slice(b"stale cold-refusal sentinel");
        assert!(matches!(
            reader(request, &mut read),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(
            read.is_empty(),
            "even a refusal before File::open must clear output"
        );
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 1);
    }
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &first_digest,
            size_bytes: first.len() as u64,
        },
        &mut read,
    )
    .expect("invalid requests must not poison an unchanged preparation");
    assert_eq!(read.as_slice(), first);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 1);
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &cold_digest,
            size_bytes: cold.len() as u64,
        },
        &mut read,
    )
    .expect("valid cold request still opens its file");
    assert_eq!(read.as_slice(), cold);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 2);
    preparation.abort().expect("clean up preparation");
}

#[test]
fn staged_segment_evicted_path_replacement_poison_without_reopen() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let contents = (0..9)
        .map(|index| format!("eviction-identity-segment-{index}").into_bytes())
        .collect::<Vec<_>>();
    let digests = contents
        .iter()
        .map(|bytes| segment_digest(bytes))
        .collect::<Vec<_>>();
    for (digest, bytes) in digests.iter().zip(&contents) {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
            .expect("stage sealed segment");
    }
    let first_path = staged_path(&preparation, &digests[0]);
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    for (digest, bytes) in digests.iter().zip(&contents) {
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest,
                size_bytes: bytes.len() as u64,
            },
            &mut read,
        )
        .expect("pin distinct segment");
        assert_eq!(&read, bytes);
    }
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 9);
    assert_eq!(
        pinned_handle_count(&preparation),
        MAX_STAGED_SEGMENT_READ_HANDLES
    );

    let replacement_path = first_path.with_extension("eviction-replacement.tmp");
    std::fs::write(&replacement_path, vec![b'Z'; contents[0].len()])
        .expect("same-length replacement");
    std::fs::rename(&replacement_path, &first_path).expect("replace evicted segment pathname");
    read.extend_from_slice(b"old data");
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &digests[0],
                size_bytes: contents[0].len() as u64,
            },
            &mut read,
        ),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert!(
        read.is_empty(),
        "replacement cannot expose stale or new bytes"
    );
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        9,
        "replaced evicted path must be rejected before another File::open"
    );
    assert_eq!(pinned_handle_count(&preparation), 0);
    assert!(matches!(
        preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[1],
            bytes: &contents[1],
        }),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
    ));
    preparation.abort().expect("remove poisoned staging");
    assert!(!first_path.exists());
}

#[test]
fn staged_preparation_exact_segment_bytes_commit_and_reload() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation)
        .expect("stage real generation");
    let identities = CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse staged segment identities")
    .expect("current partitioned generation revision");
    let identity = identities
        .into_iter()
        .find(|identity| identity.size_bytes > 4)
        .expect("real generation must have a nonempty segment");

    let reader = preparation.segment_reader();
    let initial_opens = preparation.staged_segment_read_open_attempts_for_test();
    let mut exact = Vec::new();
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &identity.digest,
            size_bytes: identity.size_bytes,
        },
        &mut exact,
    )
    .expect("read staged sealed whole segment");
    assert_eq!(
        segment_digest(&exact),
        identity.digest,
        "production reader must return the exact addressed segment"
    );
    let mut range = Vec::new();
    for _ in 0..16 {
        reader(
            SealedGenerationSegmentReadV1::Range {
                digest: &identity.digest,
                size_bytes: identity.size_bytes,
                offset: 1,
                length: 3,
            },
            &mut range,
        )
        .expect("read repeated byte-exact range");
        assert_eq!(range.as_slice(), &exact[1..4]);
    }
    assert!(
        preparation.staged_segment_read_open_attempts_for_test() - initial_opens <= 1,
        "whole and repeated range reads may open the segment only once"
    );
    let committed = target
        .commit(&mut preparation, &publication)
        .expect("publish the real staged generation");
    assert_eq!(
        committed.as_ref(),
        publication.manifest_bytes().as_ref(),
        "commit must preserve the staged manifest bytes"
    );
    drop(reader);
    drop(preparation);
    let reloaded = target
        .load_active_shared()
        .expect("decode committed generation")
        .expect("active durable generation");
    assert_eq!(
        reloaded.manifest().generation_id,
        generation.manifest().generation_id
    );
    assert_eq!(
        reloaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "a cold durable read must recover the exact published snapshot identity"
    );
}

#[test]
fn staged_segment_rejected_reads_preserve_full_cache_and_recency() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");
    let contents = (0..9)
        .map(|index| format!("admission-staged-segment-{index}-bytes").into_bytes())
        .collect::<Vec<_>>();
    let digests = contents
        .iter()
        .map(|bytes| segment_digest(bytes))
        .collect::<Vec<_>>();
    for (digest, bytes) in digests.iter().zip(&contents) {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
            .expect("stage real segment");
    }
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    for (digest, bytes) in digests.iter().zip(&contents).take(8) {
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest,
                size_bytes: bytes.len() as u64,
            },
            &mut read,
        )
        .expect("fill the handle cache");
        assert_eq!(&read, bytes);
    }
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 8);
    assert_eq!(pinned_handle_count(&preparation), 8);
    let missing_digest = segment_digest(b"never staged admission");
    for request in [
        SealedGenerationSegmentReadV1::Range {
            digest: &digests[8],
            size_bytes: contents[8].len() as u64,
            offset: contents[8].len() as u64 - 1,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Range {
            digest: &digests[8],
            size_bytes: contents[8].len() as u64,
            offset: u64::MAX,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[8],
            size_bytes: contents[8].len() as u64 + 1,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &missing_digest,
            size_bytes: b"never staged admission".len() as u64,
        },
        SealedGenerationSegmentReadV1::Range {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
            offset: contents[0].len() as u64 - 1,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64 + 1,
        },
    ] {
        read.extend_from_slice(b"stale rejected-read sentinel");
        assert!(matches!(
            reader(request, &mut read),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert_eq!(read, [] as [u8; 0]);
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 8);
        assert_eq!(
            pinned_handle_count(&preparation),
            8,
            "a rejected request must not evict a valid handle"
        );
    }

    // The next valid cold read must evict zero, not one: invalid accesses
    // to zero above must not make it more recent than the other seven files.
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[8],
            size_bytes: contents[8].len() as u64,
        },
        &mut read,
    )
    .expect("admit valid cold read after refusals");
    assert_eq!(read, contents[8]);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 9);
    assert_eq!(pinned_handle_count(&preparation), 8);
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[1],
            size_bytes: contents[1].len() as u64,
        },
        &mut read,
    )
    .expect("second-oldest handle must still be cached");
    assert_eq!(read, contents[1]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        9,
        "invalid cache hits must not change the eviction victim"
    );
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
        },
        &mut read,
    )
    .expect("reopen the actual least-recently-used segment");
    assert_eq!(read, contents[0]);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 10);
    preparation.abort().expect("clean up staged cache fixture");
}

#[test]
fn staged_segment_publish_never_overwrites_a_concurrent_canonical_name() {
    let root = TempDir::new().expect("sealed segment race fixture");
    let canonical = root.path().join("segment-content-address.json");
    let temporary = root.path().join("staged-segment.json");
    let original = b"sealed-original";
    let changed = b"sealed-modified";
    assert_eq!(original.len(), changed.len());

    // Exercise the actual production helper after the missing-path probe,
    // with a conflicting canonical name created in the race window.
    std::fs::write(&canonical, original).expect("competing canonical bytes");
    std::fs::write(&temporary, changed).expect("staged bytes");
    assert!(
        super::StagedGenerationSegmentsV1::publish_missing_segment(&temporary, &canonical)
            .is_err(),
        "a race must refuse, not replace a different immutable segment"
    );
    assert_eq!(
        std::fs::read(&canonical).expect("preserved competing segment"),
        original
    );
    assert_eq!(
        std::fs::read(&temporary).expect("staged bytes retained on refusal"),
        changed
    );

    // When the competing publisher installed identical bytes, deduplication
    // may finish without another publication or destroying canonical bytes.
    std::fs::write(&temporary, original).expect("identical staged bytes");
    super::StagedGenerationSegmentsV1::publish_missing_segment(&temporary, &canonical)
        .expect("identical concurrent publication is safe to deduplicate");
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(&canonical).expect("canonical name retained"),
        original
    );

    std::fs::remove_file(&canonical).expect("vacate canonical name");
    std::fs::write(&temporary, changed).expect("new staged segment");
    super::StagedGenerationSegmentsV1::publish_missing_segment(&temporary, &canonical)
        .expect("missing canonical name accepts one atomic publication");
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(&canonical).expect("new published bytes"),
        changed
    );
}
