//! A refused read cannot discard another operation's authenticated pack handles.
use super::{GenerationPackReadPassV1, StagedGenerationSegmentsV1, TemporaryPayloadPackV1};
#[cfg(target_os = "linux")]
use notify::{RecommendedWatcher, Watcher};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::mpsc::{self, Receiver};
#[cfg(target_os = "linux")]
use std::time::Duration;
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationSegmentIdentityV1, SealedGenerationSegmentReadV1,
};
use tracedecay_domain::{ManifestDigest, sha256_hex_suffix};

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

fn publish_pack(
    root: &Path,
    staging: &Path,
    ordinal: usize,
    bytes: &[u8],
) -> SealedGenerationSegmentIdentityV1 {
    let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).expect("pack digest");
    let mut pack = TemporaryPayloadPackV1::create(staging.join(format!("pack-{ordinal}.tmp")))
        .expect("create actual pack writer");
    pack.append_page(0, &digest, bytes)
        .expect("append exact pack bytes");
    assert!(
        pack.commit(root, staging, &digest, bytes.len() as u64, 1)
            .expect("durably stage through the production writer")
    );
    let (temporary, identity) = pack.authenticated.take().expect("original writer witness");
    let canonical = root.join(format!(
        "segment-{}.json",
        sha256_hex_suffix(digest.as_str()).expect("pack digest suffix")
    ));
    let mut pending = StagedGenerationSegmentsV1::new();
    pending.pending.insert(canonical, temporary.clone());
    pending.authenticated_segments.insert(temporary, identity);
    pending
        .publish()
        .expect("publish the authenticated immutable pack");
    pack.attach_to_manifest();
    SealedGenerationSegmentIdentityV1 {
        digest,
        size_bytes: bytes.len() as u64,
    }
}

#[test]
fn retained_failed_read_preserves_other_operation_handles_and_recovers() {
    let root = tempfile::tempdir().expect("isolated pack store");
    let segments = root.path().join("segments");
    fs::create_dir(&segments).expect("canonical pack directory");
    let staging = tempfile::tempdir_in(root.path()).expect("private staging directory");
    let bytes: Vec<Vec<u8>> = (0..3)
        .map(|index| format!("nonempty retained pack {index:04}: exact bytes").into_bytes())
        .collect();
    let identities = bytes
        .iter()
        .enumerate()
        .map(|(index, bytes)| publish_pack(&segments, staging.path(), index, bytes))
        .collect::<Vec<_>>();
    let path = |index: usize| {
        segments.join(format!(
            "segment-{}.json",
            sha256_hex_suffix(identities[index].digest.as_str()).expect("pack digest suffix")
        ))
    };
    let mut damaged = bytes[2].clone();
    damaged[0] ^= 1;
    fs::write(path(2), &damaged).expect("corrupt the disposable cold candidate");
    assert_eq!(damaged.len(), bytes[2].len());
    let request = |index: usize| SealedGenerationSegmentReadV1::Whole {
        digest: &identities[index].digest,
        size_bytes: identities[index].size_bytes,
    };
    #[cfg(target_os = "linux")]
    let observer = OpenObserver::new(&segments);
    #[cfg(target_os = "linux")]
    {
        let _ = observer.opens();
        drop(File::open(path(0)).expect("positive observer control"));
        assert_eq!(observer.opens(), 1, "the observer counts a real open");
    }
    let mut healthy_opens = 0;
    let mut healthy =
        GenerationPackReadPassV1::open(&segments, &identities[..2], None, &mut healthy_opens)
            .expect("pin the two healthy generation roles");
    let mut output = Vec::new();
    for (index, expected) in bytes.iter().take(2).enumerate() {
        healthy
            .read(request(index), &mut output, &mut healthy_opens)
            .expect("authenticate healthy pack");
        assert_eq!(&output, expected);
    }
    assert_eq!(healthy_opens, 2);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 2, "only the two payload roles are opened");
    for _ in 0..2 {
        output = vec![0x55, 0xaa];
        assert!(
            healthy
                .read(request(2), &mut output, &mut healthy_opens)
                .is_err(),
            "an outside-roster request is refused"
        );
        assert!(output.is_empty());
        assert!(
            !healthy.is_invalidated(),
            "malformed requests preserve admitted handles"
        );
        let mut candidate_opens = 0;
        let mut candidate =
            GenerationPackReadPassV1::open(&segments, &identities[2..], None, &mut candidate_opens)
                .expect("pin a separate cold operation");
        let error = candidate
            .read(request(2), &mut output, &mut candidate_opens)
            .expect_err("the first read authenticates the candidate");
        let CodeIndexProductionErrorV1::Contract(detail) = error else {
            panic!("expected an authenticated refusal, got {error}");
        };
        assert_eq!(
            detail,
            "sealed generation segment does not match its content address"
        );
        assert!(output.is_empty(), "refusal clears stale and corrupt bytes");
        assert!(candidate.is_invalidated());
        assert_eq!(candidate_opens, 1);
        assert_eq!(healthy_opens, 2);
        #[cfg(target_os = "linux")]
        assert_eq!(observer.opens(), 1, "only the cold candidate was opened");
    }
    // Many logical windows share the same fixed-role handles.
    for offset in 0..16 {
        for index in 0..2 {
            healthy
                .read(
                    SealedGenerationSegmentReadV1::Range {
                        digest: &identities[index].digest,
                        size_bytes: identities[index].size_bytes,
                        offset,
                        length: 3,
                    },
                    &mut output,
                    &mut healthy_opens,
                )
                .expect("healthy range after candidate refusal");
            assert_eq!(output, bytes[index][offset as usize..offset as usize + 3]);
        }
    }
    assert_eq!(healthy_opens, 2);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 0, "healthy handles are never reopened");
    fs::write(path(2), &bytes[2]).expect("repair disposable candidate");
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 1, "observe fixture write separately");
    let mut candidate_opens = 0;
    let mut candidate =
        GenerationPackReadPassV1::open(&segments, &identities[2..], None, &mut candidate_opens)
            .expect("new operation over repaired candidate");
    candidate
        .read(request(2), &mut output, &mut candidate_opens)
        .expect("authenticate repaired candidate");
    assert_eq!(output, bytes[2]);
    candidate
        .read(
            SealedGenerationSegmentReadV1::Range {
                digest: &identities[2].digest,
                size_bytes: identities[2].size_bytes,
                offset: 1,
                length: 3,
            },
            &mut output,
            &mut candidate_opens,
        )
        .expect("exact admitted range without reopening");
    assert_eq!(output, bytes[2][1..4]);
    assert_eq!(candidate_opens, 1);
    #[cfg(target_os = "linux")]
    assert_eq!(observer.opens(), 1);
    fs::write(path(2), &damaged).expect("mutate the admitted file");
    #[cfg(target_os = "linux")]
    let _ = observer.opens();
    assert!(
        candidate
            .read(request(2), &mut output, &mut candidate_opens)
            .is_err(),
        "admitted authority rejects later mutation"
    );
    assert!(output.is_empty());
    assert!(candidate.is_invalidated());
    #[cfg(target_os = "linux")]
    assert_eq!(
        observer.opens(),
        0,
        "changed authority is refused without reopening"
    );
    fs::write(path(2), &bytes[2]).expect("restore bytes after invalidation");
    assert!(
        candidate
            .read(request(2), &mut output, &mut candidate_opens)
            .is_err(),
        "restoration cannot revive an invalidated pass"
    );
    healthy
        .read(request(0), &mut output, &mut healthy_opens)
        .expect("independent healthy operation remains admitted");
    assert_eq!(output, bytes[0]);
}
