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

#[cfg(target_os = "linux")]
use notify::Watcher;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexInterruptionV1,
    CodeIndexPreparationPort, CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
    CodeIndexPublishedGenerationV1, SealedGenerationContentPackV1, SealedGenerationPackLayoutV1,
    SealedGenerationSegmentIdentityV1, SealedGenerationSegmentPublicationV1,
    SealedGenerationSegmentReadV1,
};
use tracedecay_code_index_retention::code_index_generations::DurableSealedCodeGenerationIdentityV1;
use tracedecay_domain::{ManifestDigest, ProjectId, sha256_hex_suffix};

use super::{
    DaemonCodeIndexPreparationStateV1, DaemonCodeIndexPreparationV1,
    DaemonCodeIndexPublicationStoreV1, PREPARED_SEGMENT_DIGEST_OPENS, SharedCodeIndexBytePoolV1,
    StagedGenerationSegmentsV1,
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

pub(super) struct PublicationFixture {
    store: DaemonCodeIndexPublicationStoreV1,
    _scheduler: CodeIndexWorktreeSchedulerV1,
}

impl std::ops::Deref for PublicationFixture {
    type Target = DaemonCodeIndexPublicationStoreV1;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl std::ops::DerefMut for PublicationFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.store
    }
}

pub(super) fn target_store(root: &Path, fixture: &GitFixture) -> PublicationFixture {
    let scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-tests").expect("project identity"),
        fixture.path(),
        root.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("target scheduler binds its canonical memory authority");
    PublicationFixture {
        store: scheduler.publication.clone(),
        _scheduler: scheduler,
    }
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

fn logical_path(preparation: &DaemonCodeIndexPreparationV1) -> PathBuf {
    let state = preparation.state.lock().expect("preparation state");
    let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
        panic!("preparation must still be open");
    };
    files.logical.path.clone()
}

#[cfg(target_os = "linux")]
fn live_inode_handles(identity: &std::fs::Metadata) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("inspect real process descriptors")
        .filter(|entry| {
            let path = entry.as_ref().expect("descriptor entry").path();
            match std::fs::metadata(path) {
                Ok(opened) => (opened.dev(), opened.ino()) == (identity.dev(), identity.ino()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => panic!("inspect live descriptor: {error}"),
            }
        })
        .count()
}

fn finalize_payloads(
    preparation: &mut DaemonCodeIndexPreparationV1,
    contents: &[Vec<u8>],
) -> SealedGenerationContentPackV1 {
    let empty = segment_digest(&[]);
    preparation
        .write_segment(
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest: &empty,
                segment_size_bytes: 0,
                page_count: 0,
            },
        )
        .expect("finalize the real empty evidence role");
    let mut identities = contents
        .iter()
        .map(|bytes| SealedGenerationSegmentIdentityV1 {
            digest: segment_digest(bytes),
            size_bytes: bytes.len() as u64,
        })
        .collect::<Vec<_>>();
    identities.sort_by(|left, right| left.digest.cmp(&right.digest));
    preparation
        .finalize_content_pack(&identities)
        .expect("finalize the complete canonical content roster")
}

#[test]
fn staged_logical_ranges_share_one_stream_and_refuse_replacement() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() -> u32 { 7 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
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
    let path = logical_path(&preparation);
    let staged_bytes = std::fs::read(&path).expect("read the real shared logical stream");
    assert_eq!(staged_bytes, contents.concat());
    #[cfg(target_os = "linux")]
    let stream_identity = path.symlink_metadata().expect("owned stream inode");
    #[cfg(target_os = "linux")]
    assert_eq!(live_inode_handles(&stream_identity), 1);
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
        0,
        "overlapping logical reads reuse the original writer handle without pathname opens",
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
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);

    // Interleaved ranges exercise distinct logical extents in the same stream.
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
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
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
        0,
        "nine distinct logical extents need no additional file open",
    );
    #[cfg(target_os = "linux")]
    assert_eq!(live_inode_handles(&stream_identity), 1);
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
        },
        &mut read,
    )
    .expect("read the first logical extent again");
    assert_eq!(&read, &contents[0]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        0,
        "revisiting the first extent retains the same single stream handle",
    );

    let replacement_path = path.with_extension("replacement.tmp");
    std::fs::write(&replacement_path, vec![b'Z'; staged_bytes.len()])
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
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
    }
    assert!(matches!(
        preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[1],
            bytes: &contents[1],
        }),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(_)
            | CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
    ));
    preparation.abort().expect("abort and clean staged files");
    assert!(!path.exists(), "abort removes replaced staged files");
    #[cfg(target_os = "linux")]
    assert_eq!(
        live_inode_handles(&stream_identity),
        0,
        "abort closes the original unlinked stream"
    );
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
fn staged_segment_cold_digest_mismatch_poison_survives_restoration() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage a real packed generation");
    let content = publication.pack_layout().content_pack();
    let path = staged_path(&preparation, &content.digest);
    let original = std::fs::read(&path).expect("original content pack");
    assert_ne!(original, [] as [u8; 0]);
    assert_eq!(segment_digest(&original), content.digest);
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    std::fs::write(&path, &corrupt).expect("corrupt the same pack inode before first read");

    let reader = preparation.pack_reader();
    let opens_before = preparation.staged_segment_read_open_attempts_for_test();
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &content.digest,
        size_bytes: content.size_bytes,
    };
    let mut read = b"prior success sentinel".to_vec();
    assert!(matches!(
        reader(request, &mut read),
        Err(CodeIndexProductionErrorV1::Contract(detail)) if detail.contains("content address")
    ));
    assert!(
        read.is_empty(),
        "cold digest failure must not expose corrupt bytes"
    );
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        opens_before + 2,
        "cold pack admission opens the two physical roles exactly once"
    );

    // Restoring the exact bytes cannot revive an invalidated transaction.
    std::fs::write(&path, &original).expect("restore the original complete pack");
    for _ in 0..2 {
        read.extend_from_slice(b"stale retry");
        assert!(matches!(reader(request, &mut read),
            Err(CodeIndexProductionErrorV1::Contract(detail)) if detail.contains("invalidated")));
        assert_eq!(read, [] as [u8; 0]);
    }
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        opens_before + 2,
        "poisoned reads never reopen the restored path"
    );
    assert!(matches!(target.commit(&mut preparation, &publication),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(detail)) if detail.contains("identity was invalidated")));
    assert!(
        target
            .load_active_manifest()
            .expect("active pointer probe")
            .is_none(),
        "a poisoned preparation cannot publish its otherwise valid manifest"
    );
    preparation.abort().expect("discard poisoned preparation");
    assert!(!path.exists());
}

#[test]
fn staged_segment_in_place_mutation_poison_survives_restoration_and_refuses_commit() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");

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
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage a real valid publication");

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
        opens_before
    );
    let path = logical_path(&preparation);
    let original_stream = std::fs::read(&path).expect("complete logical staging stream");
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

    // Restoration cannot unpoison an already-invalidated transaction.
    std::fs::write(&path, &original_stream).expect("restore every original staging byte");
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
        opens_before
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
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
    .expect("read the first owned logical extent");
    assert_eq!(read.as_slice(), first);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
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
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
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
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &cold_digest,
            size_bytes: cold.len() as u64,
        },
        &mut read,
    )
    .expect("valid previously unread extent uses the same owned stream");
    assert_eq!(read.as_slice(), cold);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);
    preparation.abort().expect("clean up preparation");
}

#[test]
fn staged_pack_path_replacement_invalidates_fixed_handles_without_reopen() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
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

    let content = finalize_payloads(&mut preparation, &contents);
    let path = staged_path(&preparation, &content.digest);
    let original = std::fs::read(&path).expect("complete physical content pack");
    #[cfg(target_os = "linux")]
    let original_identity = path.symlink_metadata().expect("original pack inode");
    let reader = preparation.pack_reader();
    let mut read = Vec::new();
    for (digest, bytes) in digests.iter().zip(&contents) {
        let extent = content
            .extents
            .iter()
            .find(|extent| &extent.digest == digest)
            .unwrap();
        reader(
            SealedGenerationSegmentReadV1::Range {
                digest: &content.digest,
                size_bytes: content.size_bytes,
                offset: extent.offset,
                length: extent.size_bytes,
            },
            &mut read,
        )
        .expect("read every logical extent through one physical pack");
        assert_eq!(&read, bytes);
    }
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        2,
        "all nine extents share exactly two admitted physical-role handles"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(live_inode_handles(&original_identity), 1);

    let replacement = path.with_extension("equal-byte-replacement.tmp");
    std::fs::write(&replacement, &original).expect("same-content replacement");
    std::fs::rename(&replacement, &path).expect("replace the admitted physical pack pathname");
    read.extend_from_slice(b"old data");
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &content.digest,
                size_bytes: content.size_bytes,
            },
            &mut read
        ),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert!(
        read.is_empty(),
        "replacement cannot expose old or substituted bytes"
    );
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        2,
        "identity refusal must not reopen the replaced physical pack"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        live_inode_handles(&original_identity),
        0,
        "invalidation immediately closes the original unlinked pack"
    );
    assert!(
        matches!(preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
        digest: &digests[1], bytes: &contents[1],
    }), Err(CodeIndexPublicationStoreErrorV1::Unavailable(detail)) if detail.contains("invalidated"))
    );
    preparation.abort().expect("remove poisoned staging");
    assert!(!path.exists());
}

#[test]
fn staged_preparation_exact_segment_bytes_commit_and_reload() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real generation");
    let identities = CodeIndexPublishedGenerationV1::partitioned_segment_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse staged segment identities");
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
        preparation.staged_segment_read_open_attempts_for_test() == initial_opens,
        "logical whole and range reads reuse the staging stream without a pathname open"
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real generation");
    let identities = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse staged segment identities");
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
            .verify_prepared_segments(files, &publication)
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
                .verify_prepared_segments(files, &publication)
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
fn cold_staged_whole_segment_read_rejects_same_inode_content_corruption() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real generation");
    let identity = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse staged segment identities")
    .into_iter()
    .find(|segment| segment.size_bytes > 4)
    .expect("nonempty staged segment");
    let path = staged_path(&preparation, &identity.digest);
    let original = std::fs::read(&path).expect("staged segment bytes");
    assert_eq!(segment_digest(&original), identity.digest);
    let before = path.symlink_metadata().expect("staged inode");

    // Mutate before the first reader open: metadata checks within the pinned
    // reader cannot discover a change to an inode they have never observed.
    let mut corrupted = original.clone();
    corrupted[0] ^= 1;
    std::fs::write(&path, &corrupted).expect("corrupt staged inode in place");
    assert_ne!(segment_digest(&corrupted), identity.digest);
    let after = path.symlink_metadata().expect("changed inode");
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (after.dev(), after.ino(), after.len()),
        "the regression must preserve the pathname, inode and extent"
    );

    let reader = preparation.pack_reader();
    let open_count = preparation.staged_segment_read_open_attempts_for_test();
    assert_eq!(open_count, 0, "exercise an actual cold pinned read");
    let mut output = b"stale response".to_vec();
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &identity.digest,
                size_bytes: identity.size_bytes,
            },
            &mut output,
        ),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        open_count + 2,
        "the digest refusal must follow admission of the two physical pack roles"
    );
    assert!(output.is_empty(), "corrupt bytes must not escape");
    preparation
        .abort()
        .expect("abort corrupted staged generation");
}

#[test]
fn committed_whole_segment_read_refuses_same_length_corruption() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real generation");
    target
        .commit(&mut preparation, &publication)
        .expect("commit real generation");
    drop(preparation);
    drop(target);

    let cold = target_store(target_root.path(), &fixture);
    let identity = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse real manifest")
    .into_iter()
    .find(|segment| segment.size_bytes > 4)
    .expect("nonempty addressed segment");
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &identity.digest,
        size_bytes: identity.size_bytes,
    };
    let sealed = cold
        .load_active(&generation.sealed_scope())
        .expect("load canonical carrier")
        .expect("active generation");
    let reader = sealed
        .begin_read_pass()
        .expect("open one actual fixed-role pass");
    let mut bytes = Vec::new();
    reader(request, &mut bytes).expect("cold valid whole-segment read");
    assert_eq!(segment_digest(&bytes), identity.digest);
    let mut range = Vec::new();
    reader(
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
        reader(request, &mut reused),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(reused, [] as [u8; 0], "corrupt bytes must not escape");
}

#[test]
fn staged_pack_rejected_requests_preserve_fixed_handles_and_valid_reads() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
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

    let content = finalize_payloads(&mut preparation, &contents);
    let reader = preparation.pack_reader();
    let missing = segment_digest(b"never staged admission");
    let invalid = [
        SealedGenerationSegmentReadV1::Range {
            digest: &content.digest,
            size_bytes: content.size_bytes,
            offset: content.size_bytes - 1,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Range {
            digest: &content.digest,
            size_bytes: content.size_bytes,
            offset: u64::MAX,
            length: 2,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &content.digest,
            size_bytes: content.size_bytes + 1,
        },
        SealedGenerationSegmentReadV1::Whole {
            digest: &missing,
            size_bytes: 12,
        },
        // A raw physical pass never accepts an addressed logical extent as a file.
        SealedGenerationSegmentReadV1::Whole {
            digest: &digests[0],
            size_bytes: contents[0].len() as u64,
        },
    ];
    let mut read = Vec::new();
    for expected_opens in [0, 2] {
        for request in invalid {
            read.extend_from_slice(b"stale rejected-read sentinel");
            assert!(matches!(
                reader(request, &mut read),
                Err(CodeIndexProductionErrorV1::Contract(_))
            ));
            assert_eq!(read, [] as [u8; 0]);
            assert_eq!(
                preparation.staged_segment_read_open_attempts_for_test(),
                expected_opens,
                "malformed requests neither open nor invalidate healthy physical handles"
            );
        }
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &content.digest,
                size_bytes: content.size_bytes,
            },
            &mut read,
        )
        .expect("valid whole pack remains readable after refusal");
        assert_eq!(segment_digest(&read), content.digest);
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 2);
        for extent in &content.extents {
            reader(
                SealedGenerationSegmentReadV1::Range {
                    digest: &content.digest,
                    size_bytes: content.size_bytes,
                    offset: extent.offset,
                    length: extent.size_bytes,
                },
                &mut read,
            )
            .expect("every admitted extent remains readable");
            let expected = contents
                .iter()
                .find(|bytes| segment_digest(bytes) == extent.digest)
                .unwrap();
            assert_eq!(&read, expected);
        }
        assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 2);
    }
    preparation
        .abort()
        .expect("close the fixed payload pass and remove staging");
}

// Admission of the second physical role can fail after the first real open.
#[cfg(target_os = "linux")]
#[test]
fn staged_pack_admission_failure_closes_partial_handles_and_records_opens() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn fixture() -> u32 { 7 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("owning memory authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let contents = vec![b"first physical role with real bytes".to_vec()];
    let digest = segment_digest(&contents[0]);
    preparation
        .write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digest,
            bytes: &contents[0],
        })
        .expect("stage the real content extent");
    let content = finalize_payloads(&mut preparation, &contents);
    let content_path = staged_path(&preparation, &content.digest);
    let content_identity = content_path.symlink_metadata().expect("content pack inode");
    let evidence_digest = segment_digest(&[]);
    let evidence_path = staged_path(&preparation, &evidence_digest);
    let moved = evidence_path.with_extension("unavailable-during-admission");
    std::fs::rename(&evidence_path, &moved).expect("withdraw the second physical role");
    let reader = preparation.pack_reader();
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &content.digest,
        size_bytes: content.size_bytes,
    };
    let mut output = b"stale output".to_vec();
    assert!(matches!(
        reader(request, &mut output),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(output, [] as [u8; 0]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        1,
        "the successful first open remains counted when second-role admission fails"
    );
    assert_eq!(
        live_inode_handles(&content_identity),
        0,
        "a partially admitted pass must close the first pack handle on refusal"
    );

    std::fs::rename(&moved, &evidence_path).expect("restore the original evidence inode");
    output.extend_from_slice(b"stale after restoration");
    assert!(matches!(reader(request, &mut output),
        Err(CodeIndexProductionErrorV1::Contract(detail)) if detail.contains("invalidated")));
    assert_eq!(output, [] as [u8; 0]);
    assert_eq!(
        preparation.staged_segment_read_open_attempts_for_test(),
        1,
        "an invalidated transaction cannot retry pathname admission"
    );
    preparation.abort().expect("remove all temporary owners");
    assert!(!content_path.exists());
    assert!(!evidence_path.exists());
    assert_eq!(live_inode_handles(&content_identity), 0);
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
        StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical, None)
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
    StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical, None)
        .expect("identical concurrent publication is safe to deduplicate");
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(&canonical).expect("canonical name retained"),
        original
    );

    std::fs::remove_file(&canonical).expect("vacate canonical name");
    std::fs::write(&temporary, changed).expect("new staged segment");
    StagedGenerationSegmentsV1::publish_missing_immutable_file(&temporary, &canonical, None)
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real generation");
    let pack = publication.pack_layout().content_pack();
    let temporary = staged_path(&preparation, &pack.digest);
    let suffix = sha256_hex_suffix(pack.digest.as_str()).expect("content pack digest suffix");
    let canonical = target.segments_root.join(format!("segment-{suffix}.json"));
    let bytes = std::fs::read(&temporary).expect("staged content pack bytes");
    let original = temporary.metadata().expect("original pack writer inode");
    assert_eq!(segment_digest(&bytes), pack.digest);
    assert!(!canonical.exists(), "the content pack is still pending");

    let logical = pack.extents.first().expect("real logical content extent");
    let mut logical_bytes = Vec::new();
    preparation.segment_reader()(
        SealedGenerationSegmentReadV1::Whole {
            digest: &logical.digest,
            size_bytes: logical.size_bytes,
        },
        &mut logical_bytes,
    )
    .expect("read the logical bytes whose writes are now frozen");

    // A separate publisher wins the physical pack name after finalization.
    // Equal bytes do not transfer ownership of this preparation's writer inode.
    let mut foreign = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&canonical)
        .expect("concurrent canonical pack publication");
    foreign.write_all(&bytes).expect("canonical bytes");
    foreign.sync_all().expect("durable competing pack");
    let foreign_identity = foreign.metadata().expect("canonical inode");
    assert_ne!(
        (original.dev(), original.ino()),
        (foreign_identity.dev(), foreign_identity.ino()),
    );
    drop(foreign);
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    assert!(matches!(
        preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &logical.digest,
            bytes: &logical_bytes,
        }),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
    ));
    for _ in 0..16 {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert_eq!(
            files.segments.pending.get(&canonical),
            Some(&temporary),
            "a refused frozen write must retain the original pending pack"
        );
        assert!(
            files
                .segments
                .authenticated_segments
                .contains_key(&temporary)
        );
        assert!(
            !files
                .segments
                .authenticated_segments
                .contains_key(&canonical),
            "the concurrently named inode must not inherit our writer witness"
        );
        target
            .verify_prepared_segments(files, &publication)
            .expect("verification authenticates the original pending pack");
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before,
        "repeated verification reuses the actual writer's authentication"
    );

    target
        .commit(&mut preparation, &publication)
        .expect("identical canonical pack bytes deduplicate at commit");
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
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

    fn assert_refusal(&self, error: CodeIndexProductionErrorV1) {
        if self.deadline {
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
        assert_eq!(
            self.checks.load(Ordering::SeqCst),
            self.interrupt_at,
            "interrupt must occur at the intended pre/post-I/O checkpoint"
        );
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
    let operation = target
        .resident_memory_authority()
        .expect("owning store memory authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
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
    let segment = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse real manifest")
    .into_iter()
    .find(|segment| segment.size_bytes > 0)
    .expect("nonempty retained segment");
    let request = SealedGenerationSegmentReadV1::Whole {
        digest: &segment.digest,
        size_bytes: segment.size_bytes,
    };
    let suffix = sha256_hex_suffix(segment.digest.as_str()).expect("pack digest suffix");
    let expected = std::fs::read(target.segments_root.join(format!("segment-{suffix}.json")))
        .expect("read exact committed pack independently");
    assert_eq!(segment_digest(&expected), segment.digest);
    let layout = SealedGenerationPackLayoutV1::open(publication.manifest_bytes())
        .expect("canonical packed layout");
    let factory = target
        .retained_pack_read_factory(sealed)
        .expect("retained generation factory");
    let uninterrupted = tracedecay_code_index::production::UninterruptibleCodeIndexControlV1;

    // Both activation and payload reads preserve typed pre/post-I/O refusal.
    for (checkpoint, deadline) in [(1, false), (2, true), (3, false), (4, true)] {
        let control = InterruptOnCheckpoint::new(checkpoint, deadline);
        let error = factory(&control, &layout)
            .err()
            .expect("interrupted pass activation must refuse");
        control.assert_refusal(error);

        let mut read = factory(&uninterrupted, &layout).expect("activate payload read pass");
        let control = InterruptOnCheckpoint::new(checkpoint, deadline);
        let mut bytes = b"previous successful read must not survive".to_vec();
        let error =
            read(request, &mut bytes, &control).expect_err("interrupted retained read must refuse");
        control.assert_refusal(error);
        assert_eq!(bytes, [] as [u8; 0], "no earlier or late bytes may escape");
    }

    let active = InterruptOnCheckpoint::new(usize::MAX, false);
    let mut bytes = b"stale".to_vec();
    let mut read = factory(&uninterrupted, &layout).expect("activate unchanged generation");
    read(request, &mut bytes, &active).expect("unchanged generation remains readable");
    assert_eq!(bytes, expected);
}

#[test]
fn staged_existing_segments_reuse_authenticated_inodes_without_reopening() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut first = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("first preparation");
    let publication = generation
        .stage_publication(&mut first, &operation)
        .expect("stage real generation");
    target
        .commit(&mut first, &publication)
        .expect("publish canonical sealed packs");
    drop(first);

    // A publication from another preparation cannot supply this owner's
    // missing pack layout, even when all canonical files already exist.
    let mut unproven = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
            &operation,
        )
        .expect("unproven preparation");
    let unopened = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let state = unproven.state.lock().expect("unproven state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("unproven preparation must remain open");
        };
        assert!(matches!(
            target.verify_prepared_segments(files, &publication),
            Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
        ));
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        unopened
    );

    // Establish the real packed lifecycle, then discard only its inode memo.
    // Without that proof, verification must hash the canonical physical files.
    let unproven_publication = generation
        .stage_publication(&mut unproven, &operation)
        .expect("stage the real canonical layout");
    let full_before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let mut state = unproven.state.lock().expect("unproven staged state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *state else {
            panic!("staged preparation must remain open");
        };
        assert!(
            files.segments.pending.is_empty(),
            "both packs already exist"
        );
        files.segments.authenticated_segments.clear();
        target
            .verify_prepared_segments(files, &unproven_publication)
            .expect("authenticate canonical files without a retained inode memo");
    }
    let full_opens = PREPARED_SEGMENT_DIGEST_OPENS
        .with(std::cell::Cell::get)
        .saturating_sub(full_before);
    let layout = unproven_publication.pack_layout();
    let physical_packs = if layout.content_pack().digest == layout.evidence_pack().digest {
        1
    } else {
        2
    };
    assert_eq!(
        full_opens, physical_packs,
        "hash each distinct physical pack once"
    );
    unproven.abort().expect("abort unproven preparation");

    let mut staged = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
            &operation,
        )
        .expect("new preparation");
    let staged_publication = generation
        .stage_publication(&mut staged, &operation)
        .expect("restage existing canonical bytes");
    let pack = staged_publication.pack_layout().content_pack();
    let suffix = sha256_hex_suffix(pack.digest.as_str()).expect("content pack digest suffix");
    let existing_path = target.segments_root.join(format!("segment-{suffix}.json"));
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    for _ in 0..16 {
        let state = staged.state.lock().expect("staged state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("staged preparation must remain open");
        };
        assert!(files.segments.pending.is_empty());
        assert!(
            files
                .segments
                .authenticated_segments
                .contains_key(&existing_path)
        );
        target
            .verify_prepared_segments(files, &staged_publication)
            .expect("reuse sealed pack inode authentication");
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before,
        "authenticated physical packs must stay closed during repeated verification"
    );

    // Equal bytes do not authorize replacing a witnessed immutable inode.
    let replacement = existing_path.with_extension("witness-replacement.tmp");
    std::fs::write(
        &replacement,
        std::fs::read(&existing_path).expect("canonical pack bytes"),
    )
    .expect("same-content replacement");
    std::fs::rename(&replacement, &existing_path).expect("replace canonical inode");
    {
        let state = staged.state.lock().expect("staged state after replacement");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("staged preparation must remain open");
        };
        assert!(matches!(
            target.verify_prepared_segments(files, &staged_publication),
            Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
        ));
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before,
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut first = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("first preparation");
    let publication = generation
        .stage_publication(&mut first, &operation)
        .expect("stage canonical generation");
    target
        .commit(&mut first, &publication)
        .expect("publish canonical packs");
    let layout = publication.pack_layout();
    let logical = layout
        .content_pack()
        .extents
        .iter()
        .find(|extent| extent.size_bytes > 0)
        .expect("nonempty logical extent in the canonical content pack");
    let mut bytes = Vec::new();
    first.segment_reader()(
        SealedGenerationSegmentReadV1::Whole {
            digest: &logical.digest,
            size_bytes: logical.size_bytes,
        },
        &mut bytes,
    )
    .expect("read one real logical extent through its committed pack");
    assert_eq!(segment_digest(&bytes), logical.digest);
    drop(first);

    let mut preparation = target
        .prepare(
            &generation.sealed_scope(),
            Some(&generation.manifest().generation_id),
            &operation,
        )
        .expect("repeated-write preparation");
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    for _ in 0..16 {
        preparation
            .write_segment(SealedGenerationSegmentPublicationV1::File {
                digest: &logical.digest,
                bytes: &bytes,
            })
            .expect("append or reuse the exact logical extent");
        assert_eq!(
            PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
            before,
            "logical writes must not reopen a canonical file per extent"
        );
    }
    {
        let state = preparation.state.lock().expect("logical stream state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert_eq!(files.logical.extents.len(), 1);
        assert_eq!(files.logical.size_bytes, logical.size_bytes);
        assert_eq!(
            files
                .logical
                .file
                .metadata()
                .expect("writer metadata")
                .len(),
            logical.size_bytes
        );
    }
    let mut read = Vec::new();
    preparation.segment_reader()(
        SealedGenerationSegmentReadV1::Whole {
            digest: &logical.digest,
            size_bytes: logical.size_bytes,
        },
        &mut read,
    )
    .expect("read the repeated logical extent from its original writer");
    assert_eq!(read, bytes);
    assert_eq!(preparation.staged_segment_read_open_attempts_for_test(), 0);

    let staged_publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("finalize the full real generation after repeated logical writes");
    let physical_packs = if layout.content_pack().digest == layout.evidence_pack().digest {
        1
    } else {
        2
    };
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get) - before,
        physical_packs,
        "finalization hashes each existing physical pack only once"
    );
    for _ in 0..16 {
        let state = preparation
            .state
            .lock()
            .expect("finalized preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        target
            .verify_prepared_segments(files, &staged_publication)
            .expect("reuse the finalized physical pack witnesses");
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get) - before,
        physical_packs,
        "repeated verification must not hash the canonical packs again"
    );

    let suffix = sha256_hex_suffix(layout.content_pack().digest.as_str())
        .expect("content pack digest suffix");
    let canonical = target.segments_root.join(format!("segment-{suffix}.json"));
    let replacement = canonical.with_extension("repeat-witness.tmp");
    std::fs::write(
        &replacement,
        std::fs::read(&canonical).expect("canonical pack bytes"),
    )
    .expect("same-byte replacement");
    std::fs::rename(&replacement, &canonical).expect("replace canonical inode");
    {
        let state = preparation.state.lock().expect("changed finalized state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert!(matches!(
            target.verify_prepared_segments(files, &staged_publication),
            Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
        ));
    }
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get) - before,
        physical_packs,
        "identity refusal should not reopen a substituted physical pack"
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
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let contents = [
        b"durability-admission-first-bytes".to_vec(),
        b"durability-admission-refused-bytes".to_vec(),
    ];
    let digests = contents
        .iter()
        .map(|bytes| segment_digest(bytes))
        .collect::<Vec<_>>();
    preparation
        .write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[0],
            bytes: &contents[0],
        })
        .expect("stage the original logical extent");
    let empty = segment_digest(&[]);
    preparation
        .write_segment(
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest: &empty,
                segment_size_bytes: 0,
                page_count: 0,
            },
        )
        .expect("admit the independent empty evidence pack");
    let (stream_path, directory, initial_pending) = {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain open");
        };
        assert_eq!(files.segments.pending.len(), 1);
        (
            files.logical.path.clone(),
            files.directory.path().to_path_buf(),
            files.segments.pending.clone(),
        )
    };

    // Every new extent authenticates the one original writer stream. A changed
    // earlier extent must poison the owner before another append is enrolled.
    let prior = stream_path
        .symlink_metadata()
        .expect("original stream metadata");
    let mut writer = File::options()
        .write(true)
        .open(&stream_path)
        .expect("mutate same inode");
    writer
        .write_all(&vec![b'X'; contents[0].len()])
        .expect("same-length mutation");
    writer
        .set_modified(prior.modified().expect("mtime") + Duration::from_secs(2))
        .expect("force distinct mtime");
    drop(writer);
    let changed = stream_path.metadata().expect("mutated stream metadata");
    assert_eq!((changed.dev(), changed.ino()), (prior.dev(), prior.ino()));
    for attempt in 0..2 {
        let result = preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
            digest: &digests[1],
            bytes: &contents[1],
        });
        if attempt == 0 {
            assert!(matches!(
                result,
                Err(CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_))
            ));
        } else {
            assert!(matches!(
                result,
                Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
            ));
        }
        let state = preparation.state.lock().expect("failed append state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("preparation must remain abortable");
        };
        assert!(files.logical.poisoned);
        assert_eq!(files.logical.extents.len(), 1);
        assert!(files.logical.extents.contains_key(&digests[0]));
        assert!(!files.logical.extents.contains_key(&digests[1]));
        assert_eq!(files.logical.size_bytes, contents[0].len() as u64);
        assert_eq!(
            files
                .logical
                .file
                .metadata()
                .expect("failed stream metadata")
                .len(),
            contents[0].len() as u64
        );
        assert_eq!(
            files.segments.pending, initial_pending,
            "a refused append must not enroll another physical pack"
        );
        assert!(files.content_pack.is_none());
    }
    let mut identities = digests
        .iter()
        .zip(&contents)
        .map(|(digest, bytes)| SealedGenerationSegmentIdentityV1 {
            digest: digest.clone(),
            size_bytes: bytes.len() as u64,
        })
        .collect::<Vec<_>>();
    identities.sort_by(|left, right| left.digest.cmp(&right.digest));
    assert!(matches!(
        preparation.finalize_content_pack(&identities),
        Err(CodeIndexPublicationStoreErrorV1::Unavailable(_))
    ));
    assert!(
        !directory.join("content-pack.tmp").exists(),
        "a poisoned stream cannot produce a content pack"
    );
    preparation
        .abort()
        .expect("abort the poisoned append owner");
    assert!(!stream_path.exists());
    assert!(!directory.exists());
    for (canonical, temporary) in initial_pending {
        assert!(
            !canonical.exists(),
            "failed preparation must not publish evidence alone"
        );
        assert!(
            !temporary.exists(),
            "abort removes the already-staged evidence pack"
        );
    }
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
fn publication_pointer_cas_reads_exact_bytes_with_one_kernel_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare real generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage real snapshot");
    publisher
        .commit(&mut preparation, &publication)
        .expect("persist actual generation");
    drop(preparation);
    drop(publisher);

    let cold = target_store(target_root.path(), &fixture);
    let expected = std::fs::read(&cold.active_path).expect("persisted exact pointer bytes");
    assert!(!expected.is_empty(), "CAS must compare an actual pointer");

    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })
    .expect("native file observer");
    watcher
        .watch(&cold.active_path, notify::RecursiveMode::NonRecursive)
        .expect("watch exact pointer inode");
    drop(File::open(&cold.active_path).expect("positive pathname-open control"));
    assert_eq!(
        observed_pointer_opens(&receiver),
        1,
        "observer must count a real kernel open"
    );

    let (pointer, bytes) = cold
        .read_publication_pointer_with_bytes()
        .expect("read one authenticated pointer and its CAS bytes")
        .expect("published pointer");
    assert_eq!(
        observed_pointer_opens(&receiver),
        1,
        "reading the exact CAS bytes must not reopen the pointer"
    );
    assert_eq!(bytes, expected, "CAS must retain the exact persisted bytes");
    assert_eq!(
        pointer.generation_id,
        generation.manifest().generation_id.as_str()
    );

    let reloaded = cold
        .load_active_shared()
        .expect("cold read published generation")
        .expect("published snapshot");
    assert_eq!(
        reloaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "one-open pointer CAS read must preserve cold snapshot identity"
    );
}

#[test]
fn cached_publication_pointer_refuses_equal_byte_symlink_and_recovers_snapshot() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare real generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage generation");
    publisher
        .commit(&mut preparation, &publication)
        .expect("publish generation");
    drop(preparation);

    let pointer_path = publisher.active_path.clone();
    let exact_bytes = std::fs::read(&pointer_path).expect("committed pointer bytes");
    assert_ne!(exact_bytes.as_slice(), b"");
    assert_eq!(
        publisher
            .read_publication_pointer()
            .expect("legitimate cached pointer")
            .expect("active generation")
            .generation_id,
        generation.manifest().generation_id.as_str()
    );

    let original = target_root.path().join("original-active-pointer");
    std::fs::rename(&pointer_path, &original).expect("move disposable pointer");
    std::os::unix::fs::symlink(&original, &pointer_path)
        .expect("substitute symlink to the exact same bytes");
    assert_eq!(
        std::fs::read(&pointer_path).expect("linked pointer data"),
        exact_bytes,
        "symlink replacement must preserve the same bytes and digest"
    );
    assert!(
        publisher.read_publication_pointer().is_err(),
        "the warm memo must refuse an equal-byte symlink replacement"
    );
    let cold = target_store(target_root.path(), &fixture);
    assert!(
        cold.read_publication_pointer().is_err(),
        "cold pointer validation must also refuse a symlink"
    );

    std::fs::remove_file(&pointer_path).expect("remove test symlink");
    std::fs::rename(&original, &pointer_path).expect("restore original pointer");
    let loaded = cold
        .load_active_shared()
        .expect("load restored generation")
        .expect("published snapshot");
    assert_eq!(
        loaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "restored exact inode must recover the original published snapshot"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cold_publication_pointer_validation_uses_one_kernel_open_and_recovers_snapshot() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare published generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
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
fn retained_generation_ranges_reuse_one_open_and_refuse_a_replaced_inode() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained() -> u32 { 7 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("owning store memory authority")
        .begin_operation();
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare real generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage source snapshot");
    let identities = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("parse sealed pack identities");
    let identity = identities
        .into_iter()
        .find(|identity| identity.size_bytes > 4)
        .expect("real generation has a nonempty segment");
    publisher
        .commit(&mut preparation, &publication)
        .expect("publish real generation");
    drop(preparation);
    drop(publisher);

    let cold = target_store(target_root.path(), &fixture);
    let reloaded = cold
        .load_active_shared()
        .expect("decode retained generation")
        .expect("committed generation");
    assert_eq!(
        reloaded.snapshot().content_identity,
        generation.snapshot().content_identity,
        "retained pack reads must preserve the exact sealed snapshot"
    );
    let suffix = sha256_hex_suffix(identity.digest.as_str()).expect("digest suffix");
    let path = cold.segments_root.join(format!("segment-{suffix}.json"));
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })
    .expect("native file-open observer");
    watcher
        .watch(&cold.segments_root, notify::RecursiveMode::NonRecursive)
        .expect("watch sealed segment directory");

    let sealed = cold
        .load_active(&generation.sealed_scope())
        .expect("load committed carrier")
        .expect("published generation");
    let reader = sealed
        .begin_read_pass()
        .expect("activate fixed-role read pass");
    let mut whole = Vec::new();
    reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &identity.digest,
            size_bytes: identity.size_bytes,
        },
        &mut whole,
    )
    .expect("read committed content-addressed segment");
    assert_eq!(segment_digest(&whole), identity.digest);
    assert!(whole.len() > 4);
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
        .expect("read retained page without pathname reopen");
        assert_eq!(range, whole[1..4]);
    }
    assert_eq!(
        observed_named_opens(&receiver, &path),
        1,
        "one active pass must serve whole and range reads with one kernel open per pack"
    );
    // Prove the observer itself records a real open.
    drop(File::open(&path).expect("control pathname open"));
    assert_eq!(observed_named_opens(&receiver, &path), 1);

    let previous = target_root.path().join("retained-original-inode");
    std::fs::rename(&path, &previous).expect("move original inode");
    std::fs::write(&path, &whole).expect("replace with identical bytes");
    assert_ne!(
        std::fs::metadata(&path)
            .expect("replacement metadata")
            .ino(),
        std::fs::metadata(&previous)
            .expect("original metadata")
            .ino(),
        "replacement must use a distinct inode"
    );
    // Drain opens caused by writing the replacement, not by the reader.
    let _ = observed_named_opens(&receiver, &path);
    let mut refused = vec![0x55, 0xaa];
    assert!(matches!(
        reader(
            SealedGenerationSegmentReadV1::Whole {
                digest: &identity.digest,
                size_bytes: identity.size_bytes,
            },
            &mut refused,
        ),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert!(
        refused.is_empty(),
        "refused read must expose no stale bytes"
    );
    assert_eq!(
        observed_named_opens(&receiver, &path),
        0,
        "an invalidated pinned inode must be refused without a pathname open"
    );
    std::fs::remove_file(&path).expect("remove substituted name");
    std::fs::rename(&previous, &path).expect("restore original inode");
}

#[cfg(target_os = "linux")]
#[test]
fn initial_pointer_publication_reuses_persisted_descriptor_without_pathname_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
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
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare first generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
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

#[test]
fn staged_evidence_pack_writer_authentication_avoids_extra_opens_and_refuses_replacement() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn evidence() -> u32 { 1 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut target = target_store(target_root.path(), &fixture);
    let operation = target
        .resident_memory_authority()
        .expect("fixture process authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare target");
    let bytes = b"nonempty evidence pack data, authenticated after the staging rename";
    let digest = segment_digest(bytes);
    preparation
        .write_segment(
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal: 0,
                page_digest: &digest,
                bytes,
            },
        )
        .expect("append a real evidence pack page");
    let before = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    preparation
        .write_segment(
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest: &digest,
                segment_size_bytes: bytes.len() as u64,
                page_count: 1,
            },
        )
        .expect("flush and stage evidence pack");
    let (path, witness) = {
        let state = preparation.state.lock().expect("preparation state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &*state else {
            panic!("evidence preparation must remain open");
        };
        let path = files
            .evidence
            .published_path
            .as_ref()
            .expect("nonempty evidence pack was staged")
            .clone();
        let witness = files
            .segments
            .authenticated_segments
            .get(&path)
            .expect("the evidence pack must retain its writer's authenticated inode")
            .clone();
        assert!(
            files
                .segments
                .pending
                .values()
                .any(|pending| pending == &path),
            "the authenticated file must still be pending publication"
        );
        (path, witness)
    };
    assert_eq!(
        std::fs::read(&path)
            .expect("staged evidence bytes")
            .as_slice(),
        &bytes[..]
    );
    assert_eq!(segment_digest(bytes), digest);
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before,
        "staging must authenticate through the writer, not reopen the evidence pack"
    );

    // Prove the instrumented counter would observe a real digest pathname open.
    assert_eq!(
        DaemonCodeIndexPublicationStoreV1::state_digest_existing_segment(
            &path,
            &path.symlink_metadata().expect("original staged inode"),
        )
        .expect("positive digest-open control"),
        digest.as_str()
    );
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        before + 1,
        "positive control must observe one actual pathname open"
    );
    let replacement = path.with_extension("evidence-replacement.tmp");
    std::fs::write(&replacement, bytes).expect("write equal-byte different inode");
    std::fs::rename(&replacement, &path).expect("replace staged inode");
    assert!(
        !DaemonCodeIndexPublicationStoreV1::same_unix_segment_identity(
            &witness,
            &path.symlink_metadata().expect("replacement inode"),
        ),
        "equal bytes cannot reuse an authenticated inode witness"
    );
    // The real publication path must reject the substituted temporary before
    // it gains a durable canonical name. The payload writer is already synced;
    // promotion must still prove the identity of its authenticated staged inode.
    let canonical = target.segments_root.join(format!(
        "segment-{}.json",
        sha256_hex_suffix(digest.as_str()).expect("digest suffix")
    ));
    assert!(!canonical.exists(), "evidence pack has not been published");
    let opens_before_refusal = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    {
        let mut state = preparation.state.lock().expect("changed evidence state");
        let DaemonCodeIndexPreparationStateV1::Open(files) = &mut *state else {
            panic!("changed evidence preparation must remain open");
        };
        assert!(
            files.segments.publish().is_err(),
            "a same-byte, different-inode evidence pack must not be published"
        );
        assert!(
            files
                .segments
                .pending
                .values()
                .any(|pending| pending == &path),
            "rejected pack remains under private temporary ownership"
        );
    }
    assert!(
        !canonical.exists(),
        "refused pack must not gain a canonical name"
    );
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        opens_before_refusal,
        "inode refusal must not introduce an extra pathname digest open"
    );
    preparation.abort().expect("release failed preparation");
}

#[cfg(target_os = "linux")]
#[test]
fn retained_history_requests_own_fixed_pack_passes_and_refuse_replacement() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained() -> u32 { 9 }\n")]);
    let source_root = TempDir::new().expect("source store");
    let generation = source_generation(&fixture, source_root.path());
    let target_root = TempDir::new().expect("target store");
    let mut publisher = target_store(target_root.path(), &fixture);
    let operation = publisher
        .resident_memory_authority()
        .expect("owning store memory authority")
        .begin_operation();
    let mut preparation = publisher
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage production evidence");
    let segment = CodeIndexPublishedGenerationV1::partitioned_pack_identities(
        publication.manifest_bytes().as_ref(),
    )
    .expect("sealed physical pack identities")
    .into_iter()
    .find(|identity| identity.size_bytes > 4)
    .expect("nonempty real segment");
    publisher
        .commit(&mut preparation, &publication)
        .expect("publish complete generation");
    drop(preparation);
    drop(publisher);

    let cold = target_store(target_root.path(), &fixture);
    let (pointer_sender, pointer_receiver) = std::sync::mpsc::channel();
    let mut pointer_watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let _ = pointer_sender.send(event);
        })
        .expect("native pointer open observer");
    pointer_watcher
        .watch(&cold.active_path, notify::RecursiveMode::NonRecursive)
        .expect("watch the authenticated active pointer");
    let pointer = cold
        .read_publication_pointer()
        .expect("read installed pointer")
        .expect("active generation");
    assert_eq!(
        observed_pointer_opens(&pointer_receiver),
        1,
        "cold publication pointer authentication requires one kernel open"
    );
    let entry = pointer
        .generation_index
        .iter()
        .find(|entry| entry.generation_id == pointer.generation_id)
        .expect("indexed active generation");
    let identity = DurableSealedCodeGenerationIdentityV1 {
        locator: pointer.generation_file.clone(),
        digest: ManifestDigest::new(pointer.state_digest.clone()).expect("generation digest"),
        size_bytes: entry.size_bytes,
    };
    let layout = SealedGenerationPackLayoutV1::open(publication.manifest_bytes())
        .expect("canonical packed layout");
    let factory = cold
        .retained_pack_read_factory(identity.clone())
        .expect("retained generation factory");
    let suffix = sha256_hex_suffix(segment.digest.as_str()).expect("segment digest");
    let path = cold.segments_root.join(format!("segment-{suffix}.json"));
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })
    .expect("native observer");
    watcher
        .watch(&cold.segments_root, notify::RecursiveMode::NonRecursive)
        .expect("watch real sealed segment directory");

    let store_root = cold.active_path.parent().expect("owning store root");
    let lock_path = store_root.join(".code-generation-retention.lock");
    let (lock_sender, lock_receiver) = std::sync::mpsc::channel();
    let mut lock_watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let _ = lock_sender.send(event);
        })
        .expect("native generation-store lock open observer");
    lock_watcher
        .watch(store_root, notify::RecursiveMode::NonRecursive)
        .expect("watch the real generation-store lock directory");
    let control = tracedecay_code_index::production::UninterruptibleCodeIndexControlV1;
    let mut whole = Vec::new();
    let mut read = factory(&control, &layout).expect("activate first historical request");
    read(
        SealedGenerationSegmentReadV1::Whole {
            digest: &segment.digest,
            size_bytes: segment.size_bytes,
        },
        &mut whole,
        &control,
    )
    .expect("first authenticated historical segment read");
    assert_eq!(whole.len() as u64, segment.size_bytes);
    assert_eq!(segment_digest(&whole), segment.digest);
    assert!(whole.len() > 4);
    drop(read);
    let mut range = Vec::new();
    for _ in 0..16 {
        let mut read = factory(&control, &layout).expect("activate another historical request");
        read(
            SealedGenerationSegmentReadV1::Range {
                digest: &segment.digest,
                size_bytes: segment.size_bytes,
                offset: 1,
                length: 3,
            },
            &mut range,
            &control,
        )
        .expect("read historical page through its request-owned pack pass");
        assert_eq!(range, whole[1..4]);
    }
    assert_eq!(
        observed_named_opens(&receiver, &path),
        17,
        "each historical request must own one fresh open of this physical pack"
    );
    assert_eq!(
        observed_named_opens(&lock_receiver, &lock_path),
        17,
        "each historical request acquires and releases its own retention lease"
    );
    drop(File::open(&lock_path).expect("positive lock pathname-open control"));
    assert_eq!(
        observed_named_opens(&lock_receiver, &lock_path),
        1,
        "kernel observer must count an independent lock-file open"
    );
    assert_eq!(
        observed_pointer_opens(&pointer_receiver),
        0,
        "17 historical reads must reuse the pointer's authenticated descriptor"
    );
    drop(File::open(&cold.active_path).expect("positive pointer pathname-open control"));
    assert_eq!(
        observed_pointer_opens(&pointer_receiver),
        1,
        "kernel observer must detect an independently opened pointer"
    );
    drop(File::open(&path).expect("control pathname open"));
    assert_eq!(
        observed_named_opens(&receiver, &path),
        1,
        "kernel observer must count a separate real pathname open"
    );

    let peer = cold.clone();
    let peer_factory = peer
        .retained_pack_read_factory(identity)
        .expect("cloned store retained factory");
    let mut peer_read = peer_factory(&control, &layout).expect("activate cloned-store request");
    peer_read(
        SealedGenerationSegmentReadV1::Range {
            digest: &segment.digest,
            size_bytes: segment.size_bytes,
            offset: 2,
            length: 2,
        },
        &mut range,
        &control,
    )
    .expect("a second store handle activates its own bounded read pass");
    assert_eq!(range, whole[2..4]);
    assert_eq!(
        observed_named_opens(&receiver, &path),
        1,
        "a cloned publication store activates a fresh payload handle for its request"
    );
    assert_eq!(
        observed_pointer_opens(&pointer_receiver),
        0,
        "a clone must share the pinned publication pointer without another open"
    );
    assert_eq!(
        observed_named_opens(&lock_receiver, &lock_path),
        1,
        "a cloned store owns its retention lease only while its request is active"
    );

    let previous = target_root.path().join("retained-history-original-inode");
    std::fs::rename(&path, &previous).expect("move disposable canonical inode");
    std::fs::write(&path, &whole).expect("replace with identical bytes and another inode");
    assert_ne!(
        std::fs::metadata(&previous)
            .expect("original metadata")
            .ino(),
        std::fs::metadata(&path)
            .expect("replacement metadata")
            .ino(),
        "the test must substitute a different inode"
    );
    // Fixture replacement opens are not reader opens.
    let _ = observed_named_opens(&receiver, &path);
    range = vec![0x11, 0x22];
    assert!(
        peer_read(
            SealedGenerationSegmentReadV1::Range {
                digest: &segment.digest,
                size_bytes: segment.size_bytes,
                offset: 1,
                length: 3,
            },
            &mut range,
            &control,
        )
        .is_err(),
        "a replaced pathname must not inherit an active pass's pinned pack identity"
    );
    assert_eq!(
        range,
        Vec::<u8>::new(),
        "failure reveals no previously read bytes"
    );
    assert_eq!(
        observed_named_opens(&receiver, &path),
        0,
        "inode rejection does not need another endpoint-authorized open"
    );
    std::fs::remove_file(&path).expect("remove disposable replacement");
    std::fs::rename(&previous, &path).expect("restore original inode");
}
