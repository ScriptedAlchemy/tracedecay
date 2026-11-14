//! Immutable deduplication must never accept bytes from a displaced pathname.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_domain::{ManifestDigest, sha256_hex_suffix};
use tracedecay_runtime_core::logging::set_tracing_capture;
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::{
    DaemonCodeIndexPublicationStoreV1, PREPARED_SEGMENT_DIGEST_OPENS, StagedGenerationSegmentsV1,
    TemporaryPayloadPackV1,
};

struct ReplaceCanonicalDuringComparison {
    canonical: PathBuf,
    displaced: PathBuf,
    replacement: PathBuf,
    observed: Arc<AtomicUsize>,
}

impl Subscriber for ReplaceCanonicalDuringComparison {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_span() && metadata.name() == "code_index.generation.compare_immutable_bytes"
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {
        assert_eq!(
            self.observed.fetch_add(1, Ordering::SeqCst),
            0,
            "the byte comparison must enter exactly once"
        );
        // Both real files were already opened; substitute an equal-byte new
        // inode so comparing descriptor bytes alone incorrectly returns true.
        fs::rename(&self.canonical, &self.displaced).expect("displace original inode");
        fs::rename(&self.replacement, &self.canonical).expect("install different inode");
    }

    fn exit(&self, _span: &Id) {}
}

#[test]
fn canonical_deduplication_refuses_same_byte_path_replacement_during_read() {
    let root = TempDir::new().expect("private immutable comparison fixture");
    let canonical = root.path().join("segment-canonical.json");
    let pending = root.path().join("segment-pending.json");
    let displaced = root.path().join("segment-displaced.json");
    let replacement = root.path().join("segment-replacement.json");
    let bytes = b"nonempty deterministic immutable segment bytes";
    fs::write(&canonical, bytes).expect("canonical content");
    fs::write(&pending, bytes).expect("pending content");
    fs::write(&replacement, bytes).expect("replacement content");

    let canonical_identity = fs::symlink_metadata(&canonical).expect("canonical identity");
    let replacement_identity = fs::symlink_metadata(&replacement).expect("replacement identity");
    assert_ne!(
        (canonical_identity.dev(), canonical_identity.ino()),
        (replacement_identity.dev(), replacement_identity.ino()),
        "the replacement must be a real, different inode"
    );
    assert!(
        DaemonCodeIndexPublicationStoreV1::files_equal(&canonical, &pending)
            .expect("intact files compare equal"),
        "positive control reads two nonempty actual files"
    );

    let observed = Arc::new(AtomicUsize::new(0));
    let swap = ReplaceCanonicalDuringComparison {
        canonical: canonical.clone(),
        displaced: displaced.clone(),
        replacement,
        observed: Arc::clone(&observed),
    };
    let result = {
        let _capture = set_tracing_capture(swap);
        DaemonCodeIndexPublicationStoreV1::files_equal(&canonical, &pending)
    };
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    let error = result.expect_err("a changed canonical name cannot authorize deduplication");
    assert!(
        error.to_string().contains("changed during comparison"),
        "the exact inode change must be the refusal: {error}"
    );
    assert_eq!(fs::read(&canonical).expect("replacement bytes"), bytes);
    assert_eq!(fs::read(&pending).expect("pending bytes"), bytes);
    let installed = fs::symlink_metadata(&canonical).expect("installed identity");
    assert_eq!(
        (installed.dev(), installed.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );

    fs::remove_file(&canonical).expect("discard substituted fixture");
    fs::rename(&displaced, &canonical).expect("restore original fixture inode");
    assert!(
        DaemonCodeIndexPublicationStoreV1::files_equal(&canonical, &pending)
            .expect("recovered exact original comparison"),
        "the original bytes still deduplicate after recovery"
    );
}

struct ReplacePendingBeforeComparison {
    pending: PathBuf,
    displaced: PathBuf,
    replacement: PathBuf,
    observed: Arc<AtomicUsize>,
}

impl Subscriber for ReplacePendingBeforeComparison {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_span() && metadata.name() == "code_index.generation.compare_immutable_pathnames"
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {
        assert_eq!(
            self.observed.fetch_add(1, Ordering::SeqCst),
            0,
            "a single canonical comparison must run"
        );
        // The publisher already checked the original staged inode. Substitute
        // a second same-byte inode BEFORE the comparator samples metadata.
        fs::rename(&self.pending, &self.displaced).expect("retain original staged inode");
        fs::rename(&self.replacement, &self.pending).expect("replace staged pathname");
    }

    fn exit(&self, _span: &Id) {}
}

#[test]
fn staged_deduplication_refuses_a_swapped_temporary_before_comparison() {
    let root = TempDir::new().expect("isolated immutable publication fixture");
    let segments = root.path().join("segments");
    let temporary_root = root.path().join("staging");
    fs::create_dir(&segments).expect("canonical segment directory");
    fs::create_dir(&temporary_root).expect("staging directory");
    let bytes = b"authenticated nonempty segment for canonical deduplication";
    let digest =
        ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).expect("content address");
    let suffix = sha256_hex_suffix(digest.as_str()).expect("canonical hex suffix");
    let canonical = segments.join(format!("segment-{suffix}.json"));
    let mut staged = StagedGenerationSegmentsV1::new();
    let digest_opens = PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get);
    let mut pack = TemporaryPayloadPackV1::create(temporary_root.join("payload.tmp"))
        .expect("real payload writer");
    pack.append_page(0, &digest, bytes)
        .expect("append the authenticated payload");
    assert!(
        pack.commit(&segments, &temporary_root, &digest, bytes.len() as u64, 1)
            .expect("durably stage the payload through its original writer")
    );
    let (temporary, identity) = pack
        .authenticated
        .take()
        .expect("the real writer must issue its staged inode witness");
    staged.pending.insert(canonical.clone(), temporary.clone());
    staged
        .authenticated_segments
        .insert(temporary.clone(), identity);
    pack.attach_to_manifest();
    let original = fs::symlink_metadata(&temporary).expect("authenticated inode");
    fs::write(&canonical, bytes).expect("concurrently named canonical bytes");
    let replacement = temporary_root.join("replacement");
    let displaced = temporary_root.join("original");
    fs::write(&replacement, bytes).expect("same bytes in a second inode");
    let changed = fs::symlink_metadata(&replacement).expect("replacement inode");
    assert_ne!(
        (original.dev(), original.ino()),
        (changed.dev(), changed.ino())
    );
    assert_eq!(
        PREPARED_SEGMENT_DIGEST_OPENS.with(std::cell::Cell::get),
        digest_opens,
        "staging authenticates the original writer without opening a canonical payload"
    );

    let observed = Arc::new(AtomicUsize::new(0));
    let swap = ReplacePendingBeforeComparison {
        pending: temporary.clone(),
        displaced: displaced.clone(),
        replacement,
        observed: Arc::clone(&observed),
    };
    let result = {
        let _capture = set_tracing_capture(swap);
        staged.publish()
    };
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    let error = result.expect_err("a new staged inode cannot authorize deduplication");
    assert!(
        error
            .to_string()
            .contains("changed during canonical deduplication"),
        "refuse the stale staging witness: {error}"
    );
    assert_eq!(
        fs::read(&canonical).expect("original canonical bytes"),
        bytes
    );
    assert_eq!(fs::read(&temporary).expect("replacement bytes"), bytes);
    assert_eq!(fs::read(&displaced).expect("original staged bytes"), bytes);
    assert!(staged.pending.contains_key(&canonical));

    fs::remove_file(&temporary).expect("remove substituted fixture");
    fs::rename(&displaced, &temporary).expect("restore the authenticated inode");
    // Rename changes ctime even when the original inode is restored. That
    // original witness must remain invalid; only a fresh preparation may
    // authenticate a new epoch of staged bytes.
    let retry = staged
        .publish()
        .expect_err("a changed witness cannot be reused");
    assert!(
        retry
            .to_string()
            .contains("changed during canonical deduplication"),
        "the previous authentication must not become reusable: {retry}"
    );
    assert!(staged.pending.contains_key(&canonical));
    assert_eq!(
        fs::read(&canonical).expect("canonical remains intact"),
        bytes
    );
}
