//! Sealed segments this process already authenticated.
//!
//! Every sealed graph publication proves its generation's durable source
//! before it builds rows. Segments are content-addressed and written once, so
//! a successor shares all but the few segments its edit re-sealed. Re-reading
//! and re-hashing the unchanged ones made each one-file refresh read the
//! whole generation. A segment hashed against its digest is recorded with the
//! exact file identity it was read at; while the path still names that
//! identity, its bytes are the ones that were proven, and a later proof skips
//! it. Any rewrite, truncation, or replacement moves the identity and the
//! segment is read and hashed again.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracedecay_domain::ManifestDigest;

/// The file identity a segment's bytes were hashed at. Rewriting a file
/// advances its change time, which user space cannot set back, and replacing
/// it changes the inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SealedSegmentFingerprintV1 {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl SealedSegmentFingerprintV1 {
    /// The identity of a regular file, where the platform exposes one that a
    /// rewrite always moves. Elsewhere nothing is remembered and every proof
    /// reads every segment.
    pub(super) fn of(metadata: &std::fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            metadata.file_type().is_file().then(|| Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                len: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            None
        }
    }
}

type VerifiedSegmentMap = BTreeMap<ManifestDigest, SealedSegmentFingerprintV1>;

/// Per segments root, the last successfully committed proof. In-flight proofs
/// share that immutable snapshot without evicting or copying its entries.
#[derive(Debug, Default)]
pub(crate) struct VerifiedSealedSegmentsV1 {
    roots: Mutex<HashMap<PathBuf, Arc<VerifiedSegmentMap>>>,
}

/// One proof's view: what it may skip, and what it hashed.
pub(super) struct SealedSegmentProofV1<'v> {
    verified: &'v VerifiedSealedSegmentsV1,
    segments_root: PathBuf,
    prior: Option<Arc<VerifiedSegmentMap>>,
    proven: VerifiedSegmentMap,
    has_new_fingerprints: bool,
}

impl VerifiedSealedSegmentsV1 {
    /// Start from the last successful proof. Cancellation, corruption or an
    /// overlapping proof must not evict already authenticated identities.
    /// New observations remain private until the whole proof succeeds.
    pub(super) fn begin(&self, segments_root: &Path) -> SealedSegmentProofV1<'_> {
        let prior = self
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(segments_root)
            .cloned();
        SealedSegmentProofV1 {
            verified: self,
            segments_root: segments_root.to_path_buf(),
            prior,
            proven: BTreeMap::new(),
            has_new_fingerprints: false,
        }
    }
}

impl SealedSegmentProofV1<'_> {
    /// Whether `path` still names the identity `digest` was proven at and
    /// its byte length matches this manifest's declared extent.
    pub(super) fn already_verified(
        &mut self,
        digest: &ManifestDigest,
        expected_size: u64,
        path: &Path,
    ) -> bool {
        let Some(prior) = self
            .prior
            .as_ref()
            .and_then(|prior| prior.get(digest))
            .copied()
        else {
            return false;
        };
        let current = path
            .symlink_metadata()
            .ok()
            .and_then(|metadata| SealedSegmentFingerprintV1::of(&metadata));
        if current != Some(prior) || prior.len != expected_size {
            return false;
        }
        self.proven.insert(digest.clone(), prior);
        true
    }

    /// Record that the bytes read at `fingerprint` will be hashed against
    /// `digest`; kept only if the whole proof succeeds.
    pub(super) fn hashed(
        &mut self,
        digest: &ManifestDigest,
        fingerprint: Option<SealedSegmentFingerprintV1>,
    ) {
        if let Some(fingerprint) = fingerprint {
            let prior = self.prior.as_ref().and_then(|prior| prior.get(digest));
            self.has_new_fingerprints |= prior != Some(&fingerprint);
            self.proven.insert(digest.clone(), fingerprint);
        }
    }

    /// Remember exactly this generation, but do not let a late proof replace
    /// a snapshot another successful proof has committed since we began.
    pub(super) fn commit(self) {
        // Every reused entry equals its prior value. With no new fingerprints,
        // equal cardinality proves the same map without another full scan.
        // Keep that snapshot's identity: an unchanged overlapping proof must
        // not make a genuinely changed successor lose its commit below.
        if !self.has_new_fingerprints
            && self
                .prior
                .as_ref()
                .is_some_and(|prior| prior.len() == self.proven.len())
        {
            return;
        }
        let mut roots = self
            .verified
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unchanged = match (roots.get(&self.segments_root), self.prior.as_ref()) {
            (None, None) => true,
            (Some(current), Some(prior)) => Arc::ptr_eq(current, prior),
            _ => false,
        };
        if unchanged {
            roots.insert(self.segments_root, Arc::new(self.proven));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::{File, FileTimes};
    use std::io::Read;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_domain::{ManifestDigest, sha256_hex_suffix};

    use super::{SealedSegmentFingerprintV1, SealedSegmentProofV1, VerifiedSealedSegmentsV1};

    fn write_segment(root: &Path, bytes: &[u8]) -> (ManifestDigest, PathBuf) {
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = root.join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        (digest, path)
    }

    fn authenticate(proof: &mut SealedSegmentProofV1<'_>, digest: &ManifestDigest, path: &Path) {
        let mut file = File::open(path).unwrap();
        let fingerprint = SealedSegmentFingerprintV1::of(&file.metadata().unwrap());
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes)).unwrap(),
            *digest
        );
        proof.hashed(digest, fingerprint);
    }

    #[test]
    fn aborted_proof_keeps_parent_but_never_publishes_tentative_hashes() {
        let root = TempDir::new().unwrap();
        let (parent, parent_path) = write_segment(root.path(), b"[1]");
        let (candidate, candidate_path) = write_segment(root.path(), b"[2]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut first = verified.begin(root.path());
        authenticate(&mut first, &parent, &parent_path);
        first.commit();

        let mut interrupted = verified.begin(root.path());
        assert!(interrupted.already_verified(&parent, 3, &parent_path));
        // The reader records an identity before the caller checks its digest.
        // A failed or cancelled proof must never publish that tentative fact.
        std::fs::write(&candidate_path, b"[3]").unwrap();
        interrupted.hashed(
            &candidate,
            SealedSegmentFingerprintV1::of(&std::fs::metadata(&candidate_path).unwrap()),
        );
        drop(interrupted);

        let mut retry = verified.begin(root.path());
        assert!(
            retry.already_verified(&parent, 3, &parent_path),
            "an interrupted successor must not make its authenticated parent cold"
        );
        assert!(
            !retry.already_verified(&candidate, 3, &candidate_path),
            "a tentative digest from a failed proof must not become reusable"
        );
    }

    #[test]
    fn overlapping_proofs_share_parent_and_stale_commit_cannot_replace_successor() {
        let root = TempDir::new().unwrap();
        let (parent, parent_path) = write_segment(root.path(), b"[1]");
        let (successor, successor_path) = write_segment(root.path(), b"[2]");
        let (abandoned, abandoned_path) = write_segment(root.path(), b"[3]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut first = verified.begin(root.path());
        authenticate(&mut first, &parent, &parent_path);
        first.commit();

        let mut stale = verified.begin(root.path());
        assert!(stale.already_verified(&parent, 3, &parent_path));
        authenticate(&mut stale, &abandoned, &abandoned_path);
        let mut sibling = verified.begin(root.path());
        assert!(
            sibling.already_verified(&parent, 3, &parent_path),
            "an in-flight proof must not evict the parent's authenticated identities"
        );
        drop(sibling);
        let mut winner = verified.begin(root.path());
        authenticate(&mut winner, &successor, &successor_path);
        winner.commit();
        stale.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&successor, 3, &successor_path));
        assert!(
            !latest.already_verified(&parent, 3, &parent_path),
            "a stale proof must not resurrect segments pruned by a committed successor"
        );
        assert!(!latest.already_verified(&abandoned, 3, &abandoned_path));
    }

    #[test]
    fn unchanged_overlapping_proof_preserves_authenticated_successor() {
        let root = TempDir::new().unwrap();
        let (retained, retained_path) = write_segment(root.path(), b"[1]");
        let (replaced, replaced_path) = write_segment(root.path(), b"[2]");
        let (changed, changed_path) = write_segment(root.path(), b"[3]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut parent = verified.begin(root.path());
        authenticate(&mut parent, &retained, &retained_path);
        authenticate(&mut parent, &replaced, &replaced_path);
        parent.commit();

        let mut successor = verified.begin(root.path());
        assert!(successor.already_verified(&retained, 3, &retained_path));
        authenticate(&mut successor, &changed, &changed_path);

        // A completed parent recheck must not invalidate the in-flight edit.
        let mut unchanged = verified.begin(root.path());
        assert!(unchanged.already_verified(&retained, 3, &retained_path));
        assert!(unchanged.already_verified(&replaced, 3, &replaced_path));
        unchanged.commit();
        successor.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&retained, 3, &retained_path));
        assert!(
            latest.already_verified(&changed, 3, &changed_path),
            "an unchanged overlapping proof must not discard authenticated edited source"
        );
        assert!(
            !latest.already_verified(&replaced, 3, &replaced_path),
            "equal segment counts must not hide a genuinely changed proof"
        );
    }

    #[test]
    fn pruning_without_new_fingerprints_still_replaces_the_snapshot() {
        let root = TempDir::new().unwrap();
        let (retained, retained_path) = write_segment(root.path(), b"[1]");
        let (removed, removed_path) = write_segment(root.path(), b"[2]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut parent = verified.begin(root.path());
        authenticate(&mut parent, &retained, &retained_path);
        authenticate(&mut parent, &removed, &removed_path);
        parent.commit();

        let mut successor = verified.begin(root.path());
        assert!(successor.already_verified(&retained, 3, &retained_path));
        successor.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&retained, 3, &retained_path));
        assert!(
            !latest.already_verified(&removed, 3, &removed_path),
            "a deletion-only successor must still prune obsolete authentication entries"
        );
        drop(latest);
        verified.begin(root.path()).commit();
        let mut empty = verified.begin(root.path());
        assert!(!empty.already_verified(&retained, 3, &retained_path));
    }

    #[test]
    fn retained_parent_rejects_same_size_replacement_with_restored_mtime() {
        let root = TempDir::new().unwrap();
        let (digest, path) = write_segment(root.path(), b"[1]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut first = verified.begin(root.path());
        authenticate(&mut first, &digest, &path);
        first.commit();
        drop(verified.begin(root.path()));

        // Hold the original inode open so replacement cannot reuse it, even
        // when the filesystem happens to assign identical timestamps.
        let original = File::open(&path).unwrap();
        let metadata = original.metadata().unwrap();
        let before = SealedSegmentFingerprintV1::of(&metadata).unwrap();
        let (_, replacement_path) = write_segment(root.path(), b"[2]");
        File::options()
            .write(true)
            .open(&replacement_path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
        std::fs::rename(&replacement_path, &path).unwrap();
        let after = SealedSegmentFingerprintV1::of(&std::fs::metadata(&path).unwrap()).unwrap();
        assert_eq!(before.len, after.len);
        assert_eq!(before.modified, after.modified);
        assert_ne!(before.inode, after.inode);
        assert_ne!(
            ManifestDigest::from_sha256_bytes(&Sha256::digest(std::fs::read(&path).unwrap()))
                .unwrap(),
            digest
        );
        let mut retry = verified.begin(root.path());
        assert!(
            !retry.already_verified(&digest, 3, &path),
            "retaining an old proof must not trust a replacement's same length and mtime"
        );
        drop(original);
    }

    #[test]
    fn retained_proof_still_checks_root_extent_and_missing_source() {
        let root = TempDir::new().unwrap();
        let other_root = TempDir::new().unwrap();
        let (digest, path) = write_segment(root.path(), b"[1]");
        let verified = VerifiedSealedSegmentsV1::default();
        let mut first = verified.begin(root.path());
        authenticate(&mut first, &digest, &path);
        first.commit();

        let mut foreign = verified.begin(other_root.path());
        assert!(!foreign.already_verified(&digest, 3, &path));
        let mut retry = verified.begin(root.path());
        assert!(!retry.already_verified(&digest, 4, &path));
        assert!(retry.already_verified(&digest, 3, &path));
        std::fs::remove_file(&path).unwrap();
        assert!(!retry.already_verified(&digest, 3, &path));
    }
}
