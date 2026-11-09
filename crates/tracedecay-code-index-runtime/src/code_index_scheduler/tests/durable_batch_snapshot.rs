//! Real Linux durability: an earlier flushed file may change during the final fsync.
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_private_fs::framed_log::DurableFileBatch;
use tracedecay_runtime_core::logging::set_tracing_capture;
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

// The kernel fsync remains real. The scoped subscriber substitutes a real
// same-byte inode on the final pending member's production fsync span.
struct ReplaceEarlierWindowAtBarrier {
    destination: PathBuf,
    replacement: PathBuf,
    entered: Arc<AtomicUsize>,
}

impl Subscriber for ReplaceEarlierWindowAtBarrier {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_span() && metadata.name() == "private_fs.framed_log.fsync"
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        assert_eq!(attributes.metadata().name(), "private_fs.framed_log.fsync");
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {
        assert_eq!(
            self.entered.fetch_add(1, Ordering::SeqCst),
            0,
            "exactly one final member barrier must execute"
        );
        fs::rename(&self.replacement, &self.destination)
            .expect("replace an earlier window during the final barrier");
    }

    fn exit(&self, _span: &Id) {}
}

#[test]
fn durable_batch_rechecks_earlier_windows_after_the_final_barrier() {
    let root = tempfile::tempdir().expect("isolated durability fixture");
    let mut batch = DurableFileBatch::new();
    let bytes = b"immutable batch member";
    let first = root.path().join("member-0.tmp");
    // The 33rd real writer forces the prior 32-member window to fsync.
    for index in 0..33 {
        let path = root.path().join(format!("member-{index}.tmp"));
        let mut writer = File::create(&path).expect("member writer");
        writer.write_all(bytes).expect("nonempty member bytes");
        batch
            .written(&path, &writer)
            .expect("admit immutable member");
    }
    batch
        .sync()
        .expect("intact multi-window batch must succeed");
    assert_eq!(fs::read(&first).expect("exact original bytes"), bytes);

    let original = fs::symlink_metadata(&first).expect("original identity");
    let replacement = root.path().join("replacement.tmp");
    fs::write(&replacement, bytes).expect("same-byte distinct inode");
    let replacement_identity = fs::symlink_metadata(&replacement).expect("replacement identity");
    assert_ne!(
        (original.dev(), original.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );

    let entered = Arc::new(AtomicUsize::new(0));
    let observer = ReplaceEarlierWindowAtBarrier {
        destination: first.clone(),
        replacement: replacement.clone(),
        entered: Arc::clone(&entered),
    };
    let result = {
        let _capture = set_tracing_capture(observer);
        batch.sync()
    };
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    assert_eq!(
        result
            .expect_err("the final fsync must not authorize a replaced earlier member")
            .kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&first).expect("replacement readback"), bytes);
    let current = fs::symlink_metadata(&first).expect("current identity");
    assert_eq!(
        (current.dev(), current.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );
    // The invalidated batch retains its failure; it cannot run another fsync
    // or silently discard the previous member on retry.
    let observer = ReplaceEarlierWindowAtBarrier {
        destination: first,
        replacement,
        entered: Arc::clone(&entered),
    };
    let retry = {
        let _capture = set_tracing_capture(observer);
        batch.sync()
    };
    assert_eq!(
        retry.expect_err("retry must still refuse").kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        entered.load(Ordering::SeqCst),
        1,
        "invalid retry must not fsync"
    );
}
