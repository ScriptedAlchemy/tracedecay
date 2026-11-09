//! A final durability barrier must not reuse an earlier window's stale identity.
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::{DURABLE_BATCH_CONCURRENT_SYNCS, DurableFileBatch, sync_latency};

// Interleave a real rename at the existing production fsync span. The test
// leaves exactly one pending member, so its barrier executes on this thread:
// no sleeps, global subscriber, fake fsync, or production-only test hook.
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
    let barriers = sync_latency::inject(root.path(), Duration::ZERO);
    let mut batch = DurableFileBatch::new();
    let bytes = b"immutable batch member";
    let first = root.path().join("member-0.tmp");
    for index in 0..=DURABLE_BATCH_CONCURRENT_SYNCS {
        let path = root.path().join(format!("member-{index}.tmp"));
        let mut writer = File::create(&path).expect("member writer");
        writer.write_all(bytes).expect("nonempty member bytes");
        batch.written(&path, &writer).expect("admit immutable member");
    }
    assert_eq!(batch.synced.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    assert_eq!(batch.pending.len(), 1);
    assert_eq!(barriers.syncs(), DURABLE_BATCH_CONCURRENT_SYNCS as u64);

    batch.sync().expect("intact multi-window batch must succeed");
    assert_eq!(barriers.syncs(), DURABLE_BATCH_CONCURRENT_SYNCS as u64 + 1);
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
        replacement,
        entered: Arc::clone(&entered),
    };
    let result = tracing::subscriber::with_default(observer, || batch.sync());
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    assert_eq!(barriers.syncs(), DURABLE_BATCH_CONCURRENT_SYNCS as u64 + 2);
    assert_eq!(
        result
            .expect_err("the final barrier must not authorize a replaced earlier member")
            .kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&first).expect("replacement readback"), bytes);
    let current = fs::symlink_metadata(&first).expect("current identity");
    assert_eq!(
        (current.dev(), current.ino()),
        (replacement_identity.dev(), replacement_identity.ino())
    );
    assert_eq!(batch.synced.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    assert_eq!(batch.pending.len(), 1);
    assert_eq!(
        batch.sync().expect_err("retry must still refuse").kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        barriers.syncs(),
        DURABLE_BATCH_CONCURRENT_SYNCS as u64 + 2,
        "a known-invalid earlier window must refuse before another barrier"
    );
}
