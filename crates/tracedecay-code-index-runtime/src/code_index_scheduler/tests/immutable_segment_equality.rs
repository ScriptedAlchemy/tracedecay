//! Immutable deduplication must never accept bytes from a displaced pathname.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tempfile::TempDir;
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::DaemonCodeIndexPublicationStoreV1;

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
    let result = tracing::subscriber::with_default(swap, || {
        DaemonCodeIndexPublicationStoreV1::files_equal(&canonical, &pending)
    });
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
