//! Failed scope resets release pointer descriptors and respect active read passes.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
    SealedGenerationSegmentReadV1,
};
use tracedecay_code_index_retention::code_index_generations::{
    try_acquire_code_generation_store_lock, try_acquire_code_generation_store_read_lock,
};
use tracedecay_domain::ProjectId;
#[cfg(target_os = "linux")]
use tracedecay_domain::sha256_hex_suffix;

use super::SharedCodeIndexBytePoolV1;
use super::staged_segment_reads::target_store;
use crate::code_index_scheduler::tests::GitFixture;
use crate::code_index_scheduler::{CodeIndexReconcileOutcomeV1, CodeIndexWorktreeSchedulerV1};

#[cfg(target_os = "linux")]
fn live_inode_handles(identity: &fs::Metadata) -> usize {
    let mut count = 0;
    for entry in fs::read_dir("/proc/self/fd").expect("observe process descriptors") {
        let path = entry.expect("descriptor directory entry").path();
        match fs::metadata(&path) {
            Ok(current) if (current.dev(), current.ino()) == (identity.dev(), identity.ino()) => {
                count += 1;
            }
            Ok(_) => {}
            // Unrelated test threads may close a descriptor after enumeration.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot inspect {}: {error}", path.display()),
        }
    }
    count
}

#[test]
fn failed_reset_releases_publication_handles_across_store_clones() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn reset_fixture() -> u32 { 17 }\n")]);
    let source_root = TempDir::new().expect("isolated source store");
    let mut source = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-tests").expect("project identity"),
        fixture.path(),
        source_root.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("source scheduler");
    assert!(matches!(
        source.reconcile_now().expect("publish source generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let generation = Arc::clone(&source.latest_complete().expect("decode source").generation);
    drop(source);

    let target_root = TempDir::new().expect("isolated target store");
    let open_target = || target_store(target_root.path(), &fixture);
    let mut target = open_target();
    let store_root = target
        .active_path
        .parent()
        .expect("owning store root")
        .to_path_buf();
    let operation = target
        .resident_memory_authority()
        .expect("owning memory authority")
        .begin_operation();
    let mut preparation = target
        .prepare(&generation.sealed_scope(), None, &operation)
        .expect("prepare actual generation");
    let publication = generation
        .stage_publication(&mut preparation, &operation)
        .expect("stage actual generation");
    target
        .commit(&mut preparation, &publication)
        .expect("publish actual generation");
    drop(preparation);
    let clone = target.clone();
    assert_eq!(
        clone
            .read_publication_pointer()
            .expect("authenticate cached pointer")
            .expect("published pointer")
            .generation_id,
        generation.manifest().generation_id.as_str()
    );
    let pointer_bytes = fs::read(&target.active_path).expect("exact pointer bytes");
    assert_ne!(pointer_bytes.as_slice(), b"");
    let identity = target
        .active_path
        .symlink_metadata()
        .expect("pointer inode");
    {
        let memo = target.pointer_memo.lock().expect("shared pointer memo");
        let file = memo
            .as_ref()
            .expect("memoized publication")
            .file
            .as_ref()
            .expect("memo owns an actual descriptor");
        let opened = file.metadata().expect("cached descriptor metadata");
        assert_eq!(
            (opened.dev(), opened.ino()),
            (identity.dev(), identity.ino())
        );
    }
    #[cfg(target_os = "linux")]
    assert_ne!(
        live_inode_handles(&identity),
        0,
        "positive live-pointer-handle control"
    );

    let segment_identity = publication.pack_layout().content_pack();
    assert!(
        segment_identity.size_bytes > 0,
        "real nonempty content pack"
    );
    let sealed = target
        .load_active(&generation.sealed_scope())
        .expect("load sealed reader")
        .expect("active generation");
    // The carrier may escape the request, but actual handles and their
    // retention lease belong to this explicit operation.
    let segment_reader = sealed.begin_read_pass().expect("open one active read pass");
    let mut segment_bytes = Vec::new();
    segment_reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &segment_identity.digest,
            size_bytes: segment_identity.size_bytes,
        },
        &mut segment_bytes,
    )
    .expect("pin one exact active segment through the production reader");
    assert_eq!(segment_bytes.len() as u64, segment_identity.size_bytes);
    #[cfg(target_os = "linux")]
    let segment_path = target.segments_root.join(format!(
        "segment-{}.json",
        sha256_hex_suffix(segment_identity.digest.as_str()).expect("segment digest suffix")
    ));
    #[cfg(target_os = "linux")]
    let segment_file_identity = segment_path.symlink_metadata().expect("segment inode");
    #[cfg(target_os = "linux")]
    assert_ne!(
        live_inode_handles(&segment_file_identity),
        0,
        "positive live-segment-handle control"
    );

    assert!(
        try_acquire_code_generation_store_lock(&store_root)
            .expect("probe retirement while the pass is alive")
            .is_none(),
        "an active payload pass prevents retirement"
    );
    drop(segment_reader);
    #[cfg(target_os = "linux")]
    assert_eq!(
        live_inode_handles(&segment_file_identity),
        0,
        "finishing the pass closes its payload handles while the carrier remains alive"
    );

    // The escaped carrier must not admit a new operation between retirement
    // and unlink, even though the pack pathname still exists in this window.
    {
        let _retirement = try_acquire_code_generation_store_lock(&store_root)
            .expect("acquire retirement authority")
            .expect("completed read passes release their store lease");
        assert!(matches!(
            sealed.begin_read_pass(),
            Err(CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::StoreLockContended
            ))
        ));
        #[cfg(target_os = "linux")]
        assert_eq!(
            live_inode_handles(&segment_file_identity),
            0,
            "retirement leaves no open segment inode before unlink"
        );
    }

    // A rejected reset owns no deletion authority and must preserve its memo.
    let reader = try_acquire_code_generation_store_read_lock(&store_root)
        .expect("acquire production read lock")
        .expect("uncontended read lock");
    let segment_reader = sealed.begin_read_pass().expect("begin a later read pass");
    segment_reader(
        SealedGenerationSegmentReadV1::Whole {
            digest: &segment_identity.digest,
            size_bytes: segment_identity.size_bytes,
        },
        &mut segment_bytes,
    )
    .expect("a released retirement and another shared reader permit a new read");
    assert_eq!(segment_bytes.len() as u64, segment_identity.size_bytes);
    assert!(matches!(
        target.reset_corrupt_store(),
        Err(CodeIndexPublicationStoreErrorV1::StoreLockContended)
    ));
    assert_eq!(
        fs::read(&target.active_path).expect("preserved pointer"),
        pointer_bytes
    );
    assert!(clone.pointer_memo.lock().expect("preserved memo").is_some());
    drop(segment_reader);
    drop(reader);

    // The existing fault fixture makes cache reset fail after real on-disk
    // deletion and directory reconstruction. No filesystem operation is mocked.
    target.poison_decoded_cache_for_test();
    let error = target
        .reset_corrupt_store()
        .expect_err("decoded-cache refusal must propagate after deletion");
    assert!(
        matches!(
            &error,
            CodeIndexPublicationStoreErrorV1::Unavailable(detail)
                if detail == "daemon decoded-generation lock is poisoned"
        ),
        "unexpected reset error: {error}"
    );
    assert!(
        !target
            .active_path
            .try_exists()
            .expect("inspect reset pointer"),
        "the injected failure must occur after the actual pointer unlink"
    );
    // Observe before any pointer read: a read of the missing name could itself
    // clear the old memo and conceal the reset's descriptor leak.
    #[cfg(target_os = "linux")]
    assert_eq!(
        live_inode_handles(&identity),
        0,
        "no deleted pointer inode remains open"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        live_inode_handles(&segment_file_identity),
        0,
        "an escaped sealed reader must not retain the deleted segment inode"
    );
    assert!(
        sealed.begin_read_pass().is_err(),
        "an escaped carrier cannot reopen deleted payloads after reset"
    );
    assert!(target.pointer_memo.lock().expect("reset memo").is_none());
    assert!(
        clone
            .pointer_memo
            .lock()
            .expect("cloned reset memo")
            .is_none()
    );

    // A fresh store must be able to rebuild the reset scope and recover the
    // same snapshot, independently of the deliberately poisoned old cache.
    let mut recovered = open_target();
    let recovered_operation = recovered
        .resident_memory_authority()
        .expect("recovered process authority")
        .begin_operation();
    assert!(
        recovered
            .load_active_shared()
            .expect("empty reset store")
            .is_none()
    );
    let mut preparation = recovered
        .prepare(&generation.sealed_scope(), None, &recovered_operation)
        .expect("prepare after failed reset");
    let publication = generation
        .stage_publication(&mut preparation, &recovered_operation)
        .expect("restage exact source snapshot");
    recovered
        .commit(&mut preparation, &publication)
        .expect("publish after failed reset");
    drop(preparation);
    drop(recovered);
    let cold = open_target();
    let loaded = cold
        .load_active_shared()
        .expect("cold reload after rebuilding")
        .expect("republished generation");
    assert_eq!(
        loaded.snapshot().content_identity,
        generation.snapshot().content_identity
    );
}
