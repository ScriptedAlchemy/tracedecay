//! Exercise the production preparation, filesystem writer, and reader together.
use std::{
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexInterruptionV1,
    CodeIndexPreparationPort, CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
    CodeIndexPublishedGenerationV1, SealedGenerationSegmentPublicationV1,
    SealedGenerationSegmentReadV1,
};
use tracedecay_code_index_retention::code_index_generations::DurableSealedCodeGenerationIdentityV1;
use tracedecay_domain::{ManifestDigest, ProjectId, SanitizerRevision, sha256_hex_suffix};

use super::{
    DaemonCodeIndexPreparationStateV1, DaemonCodeIndexPreparationV1,
    DaemonCodeIndexPublicationStoreV1, MAX_STAGED_SEGMENT_READ_HANDLES,
    PREPARED_SEGMENT_DIGEST_OPENS, SharedCodeIndexBytePoolV1,
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
    // A new store has no decoded-generation or active-manifest memo from the
    // publisher; this must really reconstruct the snapshot from durable bytes.
    drop(target);
    let cold_store = target_store(target_root.path(), &fixture);
    let reloaded = cold_store
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
fn staged_writer_authentication_avoids_commit_reopens_and_rejects_replaced_inode() {
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
    .expect("partitioned generation");
    let identity = identities
        .into_iter()
        .find(|identity| identity.size_bytes > 0)
        .expect("nonempty generation segment");
    let path = staged_path(&preparation, &identity.digest);
    let bytes = std::fs::read(&path).expect("real staged segment");
    assert!(
        !bytes.is_empty(),
        "the test must authenticate real segment bytes"
    );
    assert_eq!(segment_digest(&bytes), identity.digest);
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert!(
            !files.segments.pending.is_empty(),
            "real files must be staged"
        );
        assert!(
            files.segments.authenticated_segments.contains_key(&path),
            "new staged bytes must be verified through their original writer"
        );
        target
            .verify_prepared_segments(files, publication.manifest_bytes().as_ref())
            .expect("authenticate exact pending generation without reopening");
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before,
        "commit verification must not reopen authenticated staged segments"
    );

    // Digest equality alone cannot authorize a replacement of the original
    // writer's inode, even when the replacement has byte-identical content.
    let replacement = path.with_extension("replacement.tmp");
    std::fs::write(&replacement, &bytes).expect("make equal-byte replacement");
    std::fs::rename(&replacement, &path).expect("replace staged inode");
    let before_refusal = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert!(
            target
                .verify_prepared_segments(files, publication.manifest_bytes().as_ref())
                .is_err(),
            "replacing a staged inode must fail even if its bytes are identical"
        );
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before_refusal,
        "identity rejection must not reopen a substituted staged inode"
    );
    preparation
        .abort()
        .expect("abort replaced staged generation");
}

#[test]
fn committed_whole_segment_read_refuses_same_length_corruption() {
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
    target
        .commit(&mut preparation, &publication)
        .expect("commit real generation");
    drop(preparation);
    drop(target);

    let cold = target_store(target_root.path(), &fixture);
    let identity = CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse real manifest")
    .expect("partitioned generation")
    .into_iter()
    .find(|segment| segment.size_bytes > 4)
    .expect("nonempty addressed segment");
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &identity.digest,
        size_bytes: identity.size_bytes,
    };
    let mut bytes = Vec::new();
    cold.read_partitioned_segment(request, &mut bytes)
        .expect("cold valid whole-segment read");
    assert_eq!(segment_digest(&bytes), identity.digest);
    let mut range = Vec::new();
    cold.read_partitioned_segment(
        SealedGenerationSegmentReadV1::Range {
            digest: &identity.digest,
            size_bytes: identity.size_bytes,
            offset: 1,
            length: 3,
        },
        &mut range,
    )
    .expect("valid range still returns exact bytes");
    assert_eq!(range.as_slice(), &bytes[1..4]);

    // Keep the inode and extent: size/path checks alone accept this corruption.
    let segment_path = cold.segments_root.join(format!(
        "segment-{}.json",
        sha256_hex_suffix(identity.digest.as_str()).expect("digest suffix")
    ));
    let original = segment_path.symlink_metadata().expect("original segment");
    let mut corrupt = vec![b'X'; bytes.len()];
    if corrupt == bytes {
        corrupt[0] ^= 1;
    }
    std::fs::write(&segment_path, &corrupt).expect("mutate same inode");
    let changed = segment_path.symlink_metadata().expect("mutated segment");
    assert_eq!(
        (original.dev(), original.ino(), original.len()),
        (changed.dev(), changed.ino(), changed.len())
    );
    let mut reused = b"stale output".to_vec();
    assert!(matches!(
        cold.read_partitioned_segment(request, &mut reused),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(reused, [] as [u8; 0], "corrupt bytes must not escape");
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
        super::StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical)
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
    super::StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical)
        .expect("identical concurrent publication is safe to deduplicate");
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(&canonical).expect("canonical name retained"),
        original
    );

    std::fs::remove_file(&canonical).expect("vacate canonical name");
    std::fs::write(&temporary, changed).expect("new staged segment");
    super::StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical)
        .expect("missing canonical name accepts one atomic publication");
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(&canonical).expect("new published bytes"),
        changed
    );
}

#[test]
fn repeated_pending_segment_accepts_identical_concurrent_canonical_publication() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() -> u32 { 7 }\n")]);
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

    let (canonical, temporary) = {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        files
            .segments
            .pending
            .iter()
            .next()
            .map(|(name, temporary)| (name.clone(), temporary.clone()))
            .expect("a real generation must stage at least one segment")
    };
    let bytes = std::fs::read(&temporary).expect("staged segment bytes");
    let digest = segment_digest(&bytes);
    assert!(
        !canonical.exists(),
        "the first stage has not named its segment"
    );

    // A separate publisher wins the canonical name after this preparation
    // writes its own temporary file. It has the same authenticated bytes but
    // a different inode. Repeating the write must not confuse their identities.
    let mut foreign = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&canonical)
        .expect("concurrent canonical publication");
    foreign.write_all(&bytes).expect("canonical bytes");
    foreign.sync_all().expect("durable competing segment");
    let foreign_identity = foreign.metadata().expect("canonical inode");
    drop(foreign);
    preparation
        .write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digest,
            bytes: &bytes,
        })
        .expect("repeat an already-pending segment after concurrent naming");
    {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert_eq!(
            files.segments.pending.get(&canonical),
            Some(&temporary),
            "this preparation still owns its original temporary bytes"
        );
        assert!(
            !files
                .segments
                .authenticated_segments
                .contains_key(&canonical),
            "the concurrently named inode must not masquerade as our pending inode"
        );
        target
            .verify_prepared_segments(files, publication.manifest_bytes().as_ref())
            .expect("verification authenticates pending bytes instead of another inode");
    }

    target
        .commit(&mut preparation, &publication)
        .expect("identical canonical bytes deduplicate at commit");
    assert!(!temporary.exists(), "deduplicated temporary is discharged");
    assert_eq!(
        std::fs::read(&canonical).expect("canonical bytes after commit"),
        bytes
    );
    let after = canonical.metadata().expect("canonical inode after commit");
    assert_eq!(after.dev(), foreign_identity.dev());
    assert_eq!(after.ino(), foreign_identity.ino());
}

#[cfg(unix)]
#[test]
fn prepared_generation_rechecks_directory_identity_without_reopening_paths() {
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
        .expect("stage generation");

    target
        .validate_preparation_store(&preparation, &publication)
        .expect("unchanged directories must retain preparation identity");
    let canonical = target.segments_root.clone();
    let moved = target_root.path().join("detached-segments");
    std::fs::rename(&canonical, &moved).expect("move original directory inode");
    std::fs::create_dir(&canonical).expect("substitute another directory");
    assert!(matches!(
        target.validate_preparation_store(&preparation, &publication),
        Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap)
    ));
    std::fs::remove_dir(&canonical).expect("remove empty substituted directory");
    std::fs::rename(&moved, &canonical).expect("restore owned original inode");
    target
        .validate_preparation_store(&preparation, &publication)
        .expect("restoring original inode restores the exact identity");
    preparation.abort().expect("release owned preparation");
}

#[test]
fn generation_manifest_publication_preserves_concurrent_canonical_bytes() {
    let root = TempDir::new().expect("generation publication fixture");
    let good = b"generation-one";
    let other = b"generation-two";
    assert_eq!(good.len(), other.len());
    let digest = segment_digest(good);
    let suffix = sha256_hex_suffix(digest.as_str()).expect("generation suffix");
    let canonical = root.path().join(format!("generation-{suffix}.json"));
    let stage = |bytes: &[u8]| {
        let mut temporary = tempfile::Builder::new()
            .prefix(".generation-publication-")
            .tempfile_in(root.path())
            .expect("stage generation");
        temporary.write_all(bytes).expect("write staged manifest");
        temporary
            .as_file()
            .sync_all()
            .expect("durable staged manifest");
        temporary
    };

    // The competing pathname appears after the publisher's missing-path
    // observation. A same-length conflict must not replace it.
    std::fs::write(&canonical, good).expect("competing generation");
    assert!(
        DaemonCodeIndexPublicationStoreV1::publish_missing_generation_manifest(
            stage(other),
            &canonical,
        )
        .is_err(),
        "conflicting concurrent generation must refuse publication"
    );
    assert_eq!(
        std::fs::read(&canonical).expect("competing generation retained"),
        good
    );
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("generation directory")
            .count(),
        1,
        "refused publication must clean its staging file"
    );

    // A second publisher with identical content may safely deduplicate.
    DaemonCodeIndexPublicationStoreV1::publish_missing_generation_manifest(stage(good), &canonical)
        .expect("identical immutable generation deduplicates");
    assert_eq!(std::fs::read(&canonical).expect("original content"), good);
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("generation directory")
            .count(),
        1,
        "deduplicated publication must not retain staging"
    );

    std::fs::remove_file(&canonical).expect("vacate immutable name");
    DaemonCodeIndexPublicationStoreV1::publish_missing_generation_manifest(stage(good), &canonical)
        .expect("publish new immutable name");
    assert_eq!(
        std::fs::read(&canonical).expect("new generation bytes"),
        good
    );
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("generation directory")
            .count(),
        1,
        "successful publication must consume staging"
    );
}

struct InterruptOnCheckpoint {
    interrupt_at: usize,
    deadline: bool,
    checks: AtomicUsize,
}

impl InterruptOnCheckpoint {
    fn new(interrupt_at: usize, deadline: bool) -> Self {
        Self {
            interrupt_at,
            deadline,
            checks: AtomicUsize::new(0),
        }
    }
}

impl CodeIndexExecutionControlV1 for InterruptOnCheckpoint {
    fn is_cancelled(&self) -> bool {
        let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        !self.deadline && check >= self.interrupt_at
    }

    fn is_deadline_exceeded(&self) -> bool {
        let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        self.deadline && check >= self.interrupt_at
    }
}

#[test]
fn retained_generation_read_refuses_pre_and_post_io_interruptions_without_leaking_bytes() {
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
    target
        .commit(&mut preparation, &publication)
        .expect("publish real generation");
    drop(preparation);

    let pointer = target
        .read_publication_pointer()
        .expect("read durable pointer")
        .expect("active generation pointer");
    let active = pointer
        .generation_index
        .iter()
        .find(|entry| entry.generation_id == pointer.generation_id)
        .expect("active generation entry");
    let sealed = DurableSealedCodeGenerationIdentityV1 {
        locator: active.generation_file.clone(),
        digest: ManifestDigest::new(active.state_digest.clone()).expect("canonical digest"),
        size_bytes: active.size_bytes,
    };
    let segment = CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse real manifest")
    .expect("current generation revision")
    .into_iter()
    .find(|segment| segment.size_bytes > 0)
    .expect("nonempty retained segment");
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &segment.digest,
        size_bytes: segment.size_bytes,
    };
    let mut expected = Vec::new();
    target
        .read_partitioned_segment(request, &mut expected)
        .expect("read exact committed segment");
    assert_eq!(segment_digest(&expected), segment.digest);

    // A control refuses after the initial two checks, i.e. after the real
    // pointer and segment reads. A stale successful result is not acceptable.
    for (checkpoint, deadline) in [(1, false), (2, true), (3, false), (4, true)] {
        let control = InterruptOnCheckpoint::new(checkpoint, deadline);
        let mut bytes = b"previous successful read must not survive".to_vec();
        let error = target
            .read_retained_partitioned_segment(&sealed, request, &mut bytes, &control)
            .expect_err("interrupted retained read must refuse");
        if deadline {
            assert!(matches!(
                error,
                CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
            ));
        } else {
            assert!(matches!(
                error,
                CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
            ));
        }
        assert_eq!(bytes, [] as [u8; 0], "no earlier or late bytes may escape");
        assert_eq!(
            control.checks.load(Ordering::SeqCst),
            checkpoint,
            "interrupt must occur at the intended pre/post-I/O checkpoint"
        );
    }

    let active = InterruptOnCheckpoint::new(usize::MAX, false);
    let mut bytes = b"stale".to_vec();
    target
        .read_retained_partitioned_segment(&sealed, request, &mut bytes, &active)
        .expect("unchanged generation remains readable");
    assert_eq!(bytes, expected);
}

#[test]
fn staged_existing_segments_reuse_authenticated_inodes_without_reopening() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut first = target
        .prepare(&generation.sealed_scope(), None)
        .expect("first preparation");
    let publication = generation
        .stage_publication(&mut first)
        .expect("stage real generation");
    target
        .commit(&mut first, &publication)
        .expect("publish canonical sealed segments");
    drop(first);

    // A manifest supplied without stage ownership must authenticate every
    // canonical file, not trust its digest-shaped filename.
    let unproven = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
        )
        .expect("unproven preparation");
    let full_before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let state = unproven.state.lock().expect("unproven state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("unproven preparation must remain open");
        };
        target
            .verify_prepared_segments(files, publication.manifest_bytes().as_ref())
            .expect("authenticate independently supplied manifest");
    }
    let full_opens = PREPARED_SEGMENT_DIGEST_OPENS
        .with(std::cell::Cell::get)
        .saturating_sub(full_before);
    assert!(
        full_opens > 0,
        "the counter must observe canonical file opens"
    );
    unproven.abort().expect("abort unproven preparation");

    let mut staged = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
        )
        .expect("new preparation");
    let staged_publication = generation
        .stage_publication(&mut staged)
        .expect("restage existing canonical bytes");
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    let existing_path = {
        let state = staged.state.lock().expect("staged state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("staged preparation must remain open");
        };
        let path = files
            .segments
            .authenticated_segments
            .keys()
            .next()
            .expect("previously verified canonical segment")
            .clone();
        target
            .verify_prepared_segments(files, staged_publication.manifest_bytes().as_ref())
            .expect("reuse sealed inode authentication");
        path
    };
    let warm_opens = PREPARED_SEGMENT_DIGEST_OPENS
        .with(std::cell::Cell::get)
        .saturating_sub(before);
    assert!(
        warm_opens < full_opens,
        "staged existing segments must not be opened again during verification"
    );

    // Equal bytes do not authorize silently replacing an already witnessed
    // immutable inode between stage and commit.
    let replacement = existing_path.with_extension("witness-replacement.tmp");
    std::fs::write(
        &replacement,
        std::fs::read(&existing_path).expect("canonical bytes"),
    )
    .expect("same-content replacement");
    std::fs::rename(&replacement, &existing_path).expect("replace canonical inode");
    let opens_before_refusal = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let state = staged.state.lock().expect("staged state after replacement");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("staged preparation must remain open");
        };
        assert!(
            target
                .verify_prepared_segments(files, staged_publication.manifest_bytes().as_ref())
                .is_err(),
            "a substituted inode must be refused even with identical bytes"
        );
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        opens_before_refusal,
        "identity refusal must not reopen the replaced pathname"
    );
    staged.abort().expect("abort changed preparation");
}

#[test]
fn repeated_canonical_segment_writes_reuse_witness_and_refuse_replacement() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut first = target
        .prepare(&generation.sealed_scope(), None)
        .expect("first preparation");
    let publication = generation
        .stage_publication(&mut first)
        .expect("stage canonical generation");
    target
        .commit(&mut first, &publication)
        .expect("publish canonical segments");
    drop(first);

    let identity = CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
        publication.manifest_bytes().as_ref(),
    )
    .expect("read real segment identities")
    .expect("current partitioned generation")
    .into_iter()
    .find(|identity| identity.size_bytes > 0)
    .expect("nonempty canonical segment");
    let suffix = sha256_hex_suffix(identity.digest.as_str()).expect("canonical digest suffix");
    let canonical = target.segments_root.join(format!("segment-{suffix}.json"));
    let bytes = std::fs::read(&canonical).expect("canonical segment");
    assert_eq!(segment_digest(&bytes), identity.digest);

    let mut preparation = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
        )
        .expect("repeated-write preparation");
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    for _ in 0..16 {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File {
                digest: &identity.digest,
                bytes: &bytes,
            })
            .expect("authenticate or reuse exact existing segment");
        assert_eq!(
            PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get) - before,
            1,
            "sixteen repeated writes must open and hash the canonical file only once"
        );
    }
    let reader = preparation.segment_reader();
    let mut read = Vec::new();
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &identity.digest,
            size_bytes: identity.size_bytes,
        },
        &mut read,
    )
    .expect("read the exact staged canonical segment");
    assert_eq!(read, bytes);

    // A byte-identical inode replacement is not the file that was hashed.
    // A subsequent write must reject it without another File::open.
    let replacement = canonical.with_extension("repeat-witness.tmp");
    std::fs::write(&replacement, &bytes).expect("same-byte replacement");
    std::fs::rename(&replacement, &canonical).expect("replace canonical inode");
    assert!(
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File {
                digest: &identity.digest,
                bytes: &bytes,
            })
            .is_err(),
        "a replaced immutable inode must not inherit the prior witness"
    );
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get) - before,
        1,
        "identity refusal should not reopen a substituted inode"
    );
    preparation
        .abort()
        .expect("release repeated-write preparation");
}

#[cfg(target_os = "linux")]
#[test]
fn failed_durability_admission_cannot_leave_a_publishable_segment() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() -> u32 { 7 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare target");

    // The 33rd admission flushes the first 32 real writer handles on Linux.
    // Mutating a member makes that barrier fail after the 33rd temporary was
    // created, exposing the old bug that nevertheless enrolled the temporary.
    let contents = (0..33)
        .map(|index| format!("durability-admission-{index}-bytes").into_bytes())
        .collect::<Vec<_>>();
    let digests = contents
        .iter()
        .map(|bytes| segment_digest(bytes))
        .collect::<Vec<_>>();
    for (digest, bytes) in digests.iter().zip(&contents).take(32) {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
            .expect("stage first durability window");
    }

    let first_path = staged_path(&preparation, &digests[0]);
    let prior = first_path
        .symlink_metadata()
        .expect("first member metadata");
    let mut writer = File::options()
        .write(true)
        .open(&first_path)
        .expect("mutate same inode");
    writer
        .write_all(&vec![b'X'; contents[0].len()])
        .expect("same-length mutation");
    writer
        .set_modified(prior.modified().expect("mtime") + Duration::from_secs(2))
        .expect("force distinct mtime");
    drop(writer);

    let pending_path = {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must be open");
        };
        let suffix = sha256_hex_suffix(digests[32].as_str()).expect("digest suffix");
        files
            .directory
            .path()
            .join(format!("segment-{suffix}.json"))
    };
    for _ in 0..2 {
        let result = preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[32],
            bytes: &contents[32],
        });
        assert!(matches!(
            result,
            Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
        ));
        assert!(
            !pending_path.exists(),
            "a failed writer must clean its temporary, including on retry"
        );
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain abortable");
        };
        assert!(
            !files
                .segments
                .pending
                .values()
                .any(|path| path == &pending_path),
            "failed durable admission must not be treated as staged"
        );
        assert_eq!(
            files.segments.pending.len(),
            32,
            "only the initially admitted members remain owned"
        );
    }
    preparation.abort().expect("abort failed durability window");
    assert!(!pending_path.exists());
}

#[cfg(target_os = "linux")]
fn observed_pointer_opens(
    events: &std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
) -> usize {
    let mut opens = 0;
    // Inotify delivers access events asynchronously through notify. Drain to
    // quiescence rather than observing an arbitrary instant after the call.
    while let Ok(event) = events.recv_timeout(Duration::from_millis(250)) {
        let event = event.expect("kernel file watch event");
        if matches!(
            event.kind,
            notify::EventKind::Access(notify::event::AccessKind::Open(_))
        ) {
            opens += 1;
        }
    }
    opens
}

#[cfg(target_os = "linux")]
#[test]
fn cold_publication_pointer_validation_uses_one_kernel_open_and_recovers_snapshot() {
    use notify::Watcher;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare published generation");
    let publication = generation
        .stage_publication(&mut preparation)
        .expect("stage source snapshot");
    publisher
        .commit(&mut preparation, &publication)
        .expect("commit real generation");
    drop(preparation);
    drop(publisher);

    let cold = target_store(target_root.path(), &fixture);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })
    .expect("native file observer");
    watcher
        .watch(&cold.active_path, notify::RecursiveMode::NonRecursive)
        .expect("watch the exact publication pointer");
    drop(File::open(&cold.active_path).expect("nonzero control open"));
    assert_eq!(
        observed_pointer_opens(&receiver),
        1,
        "the observer must report a real kernel pathname open"
    );

    let pointer = cold
        .read_publication_pointer()
        .expect("cold pointer validation")
        .expect("published pointer");
    assert_eq!(
        pointer.generation_id,
        generation.manifest().generation_id.as_str()
    );
    assert_eq!(
        observed_pointer_opens(&receiver),
        1,
        "parse and post-parse byte revalidation must share one file open"
    );

    let reloaded = cold
        .load_active_shared()
        .expect("read committed snapshot")
        .expect("published generation");
    assert_eq!(
        reloaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "one-open validation must not alter the durable snapshot"
    );
}

#[cfg(target_os = "linux")]
fn observed_named_opens(
    events: &std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    path: &Path,
) -> usize {
    let mut opens = 0;
    while let Ok(event) = events.recv_timeout(Duration::from_millis(250)) {
        let event = event.expect("kernel file watch event");
        if event.paths.iter().any(|candidate| candidate == path)
            && matches!(
                event.kind,
                notify::EventKind::Access(notify::event::AccessKind::Open(_))
            )
        {
            opens += 1;
        }
    }
    opens
}

#[cfg(target_os = "linux")]
#[test]
fn initial_pointer_publication_reuses_persisted_descriptor_without_pathname_open() {
    use notify::Watcher;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let active_path = publisher.active_path.clone();

    // Watch the parent: the final pathname does not exist until the first
    // publication rename, so it cannot be watched directly beforehand.
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })
    .expect("native file observer");
    watcher
        .watch(
            active_path.parent().expect("pointer parent"),
            notify::RecursiveMode::NonRecursive,
        )
        .expect("watch publication directory");

    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None)
        .expect("prepare first generation");
    let publication = generation
        .stage_publication(&mut preparation)
        .expect("stage source snapshot");
    publisher
        .commit(&mut preparation, &publication)
        .expect("publish first generation");

    assert!(active_path.is_file(), "the committed pointer must exist");
    assert_eq!(
        observed_named_opens(&receiver, &active_path),
        0,
        "memo authentication must reuse the persisted writer, not reopen the pointer"
    );
    drop(preparation);
    drop(publisher);

    // Non-vacuous kernel-observer control: the same directory watch must
    // record a real pathname open after the publication.
    drop(File::open(&active_path).expect("control open of persisted pointer"));
    assert_eq!(
        observed_named_opens(&receiver, &active_path),
        1,
        "watcher must observe a real open of the committed pointer"
    );

    let cold = target_store(target_root.path(), &fixture);
    let reloaded = cold
        .load_active_shared()
        .expect("load persisted generation")
        .expect("published generation");
    assert_eq!(
        reloaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "open-free memo population must preserve cold snapshot identity"
    );
}
