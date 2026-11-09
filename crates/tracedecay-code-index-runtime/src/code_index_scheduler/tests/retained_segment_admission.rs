//! Failed first reads must not discard unrelated authenticated segment handles.
use std::collections::VecDeque;
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
#[cfg(target_os = "linux")]
use std::sync::mpsc::{self, Receiver};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use notify::{RecommendedWatcher, Watcher};
use sha2::{Digest, Sha256};
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationSegmentReadV1,
};
use tracedecay_domain::{ManifestDigest, sha256_hex_suffix};

use super::{
    DaemonCodeIndexPublicationStoreV1, MAX_STAGED_SEGMENT_READ_HANDLES,
    PinnedPreparationSegmentV1, StagedGenerationSegmentsV1,
};

fn cache_handles(cache: &VecDeque<PinnedPreparationSegmentV1>) -> Vec<(PathBuf, RawFd)> {
    cache
        .iter()
        .map(|entry| (entry.path.clone(), entry.segment.file.as_raw_fd()))
        .collect()
}

#[cfg(target_os = "linux")]
struct OpenObserver {
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
}

#[cfg(target_os = "linux")]
impl OpenObserver {
    fn new(root: &Path) -> Self {
        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .expect("native open observer");
        watcher
            .watch(root, notify::RecursiveMode::NonRecursive)
            .expect("watch the actual segment directory");
        Self {
            _watcher: watcher,
            events,
        }
    }

    fn opens(&self) -> usize {
        let mut count = 0;
        while let Ok(event) = self.events.recv_timeout(Duration::from_millis(250)) {
            if matches!(
                event.expect("kernel event").kind,
                notify::EventKind::Access(notify::event::AccessKind::Open(_))
            ) {
                count += 1;
            }
        }
        count
    }
}

#[test]
fn retained_failed_first_read_preserves_healthy_handles_and_recovers() {
    let root = tempfile::tempdir().expect("isolated segment store");
    let segments = root.path().join("segments");
    fs::create_dir(&segments).expect("canonical segment directory");
    let staging = tempfile::tempdir_in(root.path()).expect("private staging directory");
    let mut staged = StagedGenerationSegmentsV1::new();
    let bytes: Vec<Vec<u8>> = (0..=MAX_STAGED_SEGMENT_READ_HANDLES)
        .map(|index| format!("nonempty retained segment {index:04}: exact bytes").into_bytes())
        .collect();
    let digests: Vec<ManifestDigest> = bytes
        .iter()
        .map(|bytes| {
            ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).expect("segment digest")
        })
        .collect();
    let paths: Vec<PathBuf> = digests
        .iter()
        .map(|digest| {
            segments.join(format!(
                "segment-{}.json",
                sha256_hex_suffix(digest.as_str()).expect("canonical digest suffix")
            ))
        })
        .collect();
    let existing_bytes = AtomicU64::new(0);
    for (digest, bytes) in digests.iter().zip(&bytes) {
        DaemonCodeIndexPublicationStoreV1::stage_segment(
            &segments,
            staging.path(),
            &existing_bytes,
            digest,
            bytes,
            &mut staged,
        )
        .expect("stage through the production writer");
    }
    staged
        .publish()
        .expect("durably publish actual segment files");
    drop(staged);

    let candidate = MAX_STAGED_SEGMENT_READ_HANDLES;
    let mut damaged = bytes[candidate].clone();
    damaged[0] ^= 1;
    fs::write(&paths[candidate], &damaged).expect("corrupt the disposable cold candidate");
    assert_eq!(damaged.len(), bytes[candidate].len());
    assert_ne!(
        DaemonCodeIndexPublicationStoreV1::state_digest(&damaged),
        digests[candidate].as_str()
    );

    #[cfg(target_os = "linux")]
    let observer = OpenObserver::new(&segments);
    #[cfg(target_os = "linux")]
    {
        let _ = observer.opens();
        drop(File::open(&paths[0]).expect("positive observer control"));
        assert_eq!(observer.opens(), 1, "the observer must count a real open");
    }

    let request = |index: usize| SealedGenerationSegmentReadV1::Whole {
        digest: &digests[index],
        size_bytes: bytes[index].len() as u64,
    };
    let mut cache = VecDeque::new();
    let mut output = Vec::new();
    for (index, expected) in bytes.iter().take(candidate).enumerate() {
        DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
            &segments,
            request(index),
            &mut output,
            &mut cache,
        )
        .expect("authenticate every healthy segment");
        assert_eq!(&output, expected);
    }
    assert_eq!(cache.len(), MAX_STAGED_SEGMENT_READ_HANDLES);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), MAX_STAGED_SEGMENT_READ_HANDLES);
    let healthy = cache_handles(&cache);

    for _ in 0..2 {
        output = vec![0x55, 0xaa];
        let error = DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
            &segments,
            request(candidate),
            &mut output,
            &mut cache,
        )
        .expect_err("a corrupt candidate must fail its first authenticated read");
        let CodeIndexProductionErrorV1::Contract(detail) = error else {
            panic!("expected an authenticated read refusal, got {error}");
        };
        assert_eq!(
            detail,
            "sealed generation segment does not match its content address"
        );
        assert_eq!(
            output,
            Vec::<u8>::new(),
            "refusal clears stale and corrupt bytes"
        );
        assert_eq!(
            cache_handles(&cache),
            healthy,
            "failed admission preserves healthy descriptors and their LRU positions"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(observer.opens(), 1, "the refused read must open its file");
    }

    for (index, expected) in bytes.iter().take(candidate).enumerate() {
        DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
            &segments,
            request(index),
            &mut output,
            &mut cache,
        )
        .expect("healthy data remains readable after candidate refusal");
        assert_eq!(&output, expected);
    }
    assert_eq!(cache_handles(&cache), healthy);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 0, "healthy handles must not be reopened");

    fs::write(&paths[candidate], &bytes[candidate]).expect("repair disposable candidate");
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 1, "observe the fixture write separately");
    DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
        &segments,
        request(candidate),
        &mut output,
        &mut cache,
    )
    .expect("the repaired cold candidate can pass authentication and enter the cache");
    assert_eq!(output, bytes[candidate]);
    assert_eq!(cache.len(), MAX_STAGED_SEGMENT_READ_HANDLES);
    assert_eq!(
        cache.front().expect("remaining healthy handle").path,
        paths[1]
    );
    assert_eq!(
        cache.back().expect("admitted candidate").path,
        paths[candidate]
    );
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 1);

    DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
        &segments,
        SealedGenerationSegmentReadV1::Range {
            digest: &digests[candidate],
            size_bytes: bytes[candidate].len() as u64,
            offset: 1,
            length: 3,
        },
        &mut output,
        &mut cache,
    )
    .expect("admitted candidate serves an exact range without reopening");
    assert_eq!(output, bytes[candidate][1..4]);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 0);

    // Failed admission is cache-neutral, not permission to trust changed
    // cached data. A later mutation must retain the fail-closed hit behavior.
    fs::write(&paths[candidate], &damaged).expect("mutate the now-cached disposable file");
    #[cfg(target_os = "linux")]
    let _ = observer.opens();
    assert!(
        DaemonCodeIndexPublicationStoreV1::read_cached_retained_segment(
            &segments,
            request(candidate),
            &mut output,
            &mut cache,
        )
        .is_err(),
        "an admitted handle still refuses a changed inode or digest"
    );
    assert_eq!(output, Vec::<u8>::new());
    assert_eq!(
        cache.len(),
        0,
        "changed cached authority still invalidates handles"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 0);
}
