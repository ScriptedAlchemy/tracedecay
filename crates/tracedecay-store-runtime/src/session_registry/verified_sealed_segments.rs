//! Sealed segments this process already authenticated.
//!
//! Every sealed graph publication proves its generation's durable source
//! before it builds rows. A logical segment's proof binds its physical pack,
//! the authenticated extent in that pack, and the exact file identity read.
//! An operation owns and revalidates the pack handles; this cache retains
//! metadata only. A new pack, changed extent, rewrite or replacement requires
//! the logical bytes to be authenticated again.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracedecay_domain::ManifestDigest;

/// The file identity a pack's bytes were hashed at. Rewriting a file
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

/// An authenticated logical extent in one exact physical pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SealedSegmentWitnessV1 {
    pack_digest: ManifestDigest,
    fingerprint: SealedSegmentFingerprintV1,
    offset: u64,
    size_bytes: u64,
}

impl SealedSegmentWitnessV1 {
    pub(super) fn new(
        pack_digest: &ManifestDigest,
        fingerprint: Option<SealedSegmentFingerprintV1>,
        offset: u64,
        size_bytes: u64,
    ) -> Option<Self> {
        let fingerprint = fingerprint?;
        if offset
            .checked_add(size_bytes)
            .is_none_or(|end| end > fingerprint.len)
        {
            return None;
        }
        Some(Self {
            pack_digest: pack_digest.clone(),
            fingerprint,
            offset,
            size_bytes,
        })
    }
}

type VerifiedSegmentMap = BTreeMap<ManifestDigest, SealedSegmentWitnessV1>;

// A root cache is a performance hint, not durable proof. Cap the number of
// generations held by one daemon; an evicted root is verified from disk again.
const MAX_VERIFIED_SEGMENT_ROOTS: usize = 32;

#[derive(Debug, Default)]
struct VerifiedRootCacheV1 {
    roots: HashMap<PathBuf, (Arc<VerifiedSegmentMap>, u64)>,
    last_access: u64,
}

impl VerifiedRootCacheV1 {
    fn next_access(&mut self) -> u64 {
        self.last_access = self.last_access.saturating_add(1);
        self.last_access
    }

    /// Keep the newest authenticated root warm when all older roots have
    /// active proofs. Excess entries are bounded by those outstanding pins
    /// and trimmed as soon as their proofs finish.
    fn trim_idle(&mut self) {
        if self.roots.len() <= MAX_VERIFIED_SEGMENT_ROOTS {
            return;
        }
        let newest = self
            .roots
            .iter()
            .max_by_key(|(_, (_, accessed))| *accessed)
            .map(|(root, _)| root.clone());
        while self.roots.len() > MAX_VERIFIED_SEGMENT_ROOTS {
            let oldest = self
                .roots
                .iter()
                .filter(|(root, (snapshot, _))| {
                    newest.as_deref() != Some(root.as_path()) && Arc::strong_count(snapshot) == 1
                })
                .min_by_key(|(_, (_, accessed))| *accessed)
                .map(|(root, _)| root.clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.roots.remove(&oldest);
        }
    }
}

/// Per segments root, the last successfully committed proof. In-flight proofs
/// share that immutable snapshot without evicting or copying its entries.
#[derive(Debug, Default)]
pub(crate) struct VerifiedSealedSegmentsV1 {
    roots: Mutex<VerifiedRootCacheV1>,
}

/// One proof's view: what it may skip, and what it hashed.
pub(super) struct SealedSegmentProofV1<'v> {
    verified: &'v VerifiedSealedSegmentsV1,
    segments_root: PathBuf,
    prior: Option<Arc<VerifiedSegmentMap>>,
    proven: VerifiedSegmentMap,
    has_new_witnesses: bool,
}

impl VerifiedSealedSegmentsV1 {
    /// Start from the last successful proof. Cancellation, corruption or an
    /// overlapping proof must not evict already authenticated identities.
    /// New observations remain private until the whole proof succeeds.
    pub(super) fn begin(&self, segments_root: &Path) -> SealedSegmentProofV1<'_> {
        let prior = {
            let mut cache = self
                .roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let accessed = cache.next_access();
            cache
                .roots
                .get_mut(segments_root)
                .map(|(snapshot, last_used)| {
                    *last_used = accessed;
                    Arc::clone(snapshot)
                })
        };
        SealedSegmentProofV1 {
            verified: self,
            segments_root: segments_root.to_path_buf(),
            prior,
            proven: BTreeMap::new(),
            has_new_witnesses: false,
        }
    }
}

impl SealedSegmentProofV1<'_> {
    /// Whether the admitted pack handle and authenticated extent are exactly
    /// the physical source at which this logical digest was proven.
    pub(super) fn already_verified(
        &mut self,
        digest: &ManifestDigest,
        witness: Option<SealedSegmentWitnessV1>,
    ) -> bool {
        let Some(witness) = witness else {
            return false;
        };
        if self.prior.as_ref().and_then(|prior| prior.get(digest)) != Some(&witness) {
            return false;
        }
        self.proven.insert(digest.clone(), witness);
        true
    }

    /// Record that the bytes read at `witness` will be hashed against
    /// `digest`; kept only if the whole proof succeeds.
    pub(super) fn hashed(
        &mut self,
        digest: &ManifestDigest,
        witness: Option<SealedSegmentWitnessV1>,
    ) {
        if let Some(witness) = witness {
            let prior = self.prior.as_ref().and_then(|prior| prior.get(digest));
            self.has_new_witnesses |= prior != Some(&witness);
            self.proven.insert(digest.clone(), witness);
        }
    }

    /// Remember exactly this generation, but do not let a late proof replace
    /// a snapshot another successful proof has committed since we began.
    pub(super) fn commit(mut self) {
        // Every reused entry equals its prior value. With no new fingerprints,
        // equal cardinality proves the same map without another full scan.
        // Keep that snapshot's identity: an unchanged overlapping proof must
        // not make a genuinely changed successor lose its commit below.
        if !self.has_new_witnesses
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
        let unchanged = match (roots.roots.get(&self.segments_root), self.prior.as_ref()) {
            (None, None) => true,
            (Some((current, _)), Some(prior)) => Arc::ptr_eq(current, prior),
            _ => false,
        };
        if unchanged {
            let accessed = roots.next_access();
            roots.roots.insert(
                self.segments_root.clone(),
                (Arc::new(std::mem::take(&mut self.proven)), accessed),
            );
            roots.trim_idle();
        }
    }
}

impl Drop for SealedSegmentProofV1<'_> {
    fn drop(&mut self) {
        // Release the parent pin before trimming. An all-pinned burst may
        // have temporarily retained a successfully authenticated new root
        // above the idle cap; leaving the last active proof reaps it.
        if self.prior.take().is_none() {
            return;
        }
        let mut roots = self
            .verified
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        roots.trim_idle();
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

    use super::{
        MAX_VERIFIED_SEGMENT_ROOTS, SealedSegmentFingerprintV1, SealedSegmentProofV1,
        SealedSegmentWitnessV1, VerifiedSealedSegmentsV1,
    };

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
        proof.hashed(
            digest,
            SealedSegmentWitnessV1::new(digest, fingerprint, 0, bytes.len() as u64),
        );
    }

    fn witness(
        digest: &ManifestDigest,
        size_bytes: u64,
        path: &Path,
    ) -> Option<SealedSegmentWitnessV1> {
        let fingerprint = path
            .symlink_metadata()
            .ok()
            .and_then(|metadata| SealedSegmentFingerprintV1::of(&metadata));
        SealedSegmentWitnessV1::new(digest, fingerprint, 0, size_bytes)
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
        assert!(interrupted.already_verified(&parent, witness(&parent, 3, &parent_path)));
        // The reader records an identity before the caller checks its digest.
        // A failed or cancelled proof must never publish that tentative fact.
        std::fs::write(&candidate_path, b"[3]").unwrap();
        interrupted.hashed(&candidate, witness(&candidate, 3, &candidate_path));
        drop(interrupted);

        let mut retry = verified.begin(root.path());
        assert!(
            retry.already_verified(&parent, witness(&parent, 3, &parent_path)),
            "an interrupted successor must not make its authenticated parent cold"
        );
        assert!(
            !retry.already_verified(&candidate, witness(&candidate, 3, &candidate_path)),
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
        assert!(stale.already_verified(&parent, witness(&parent, 3, &parent_path)));
        authenticate(&mut stale, &abandoned, &abandoned_path);
        let mut sibling = verified.begin(root.path());
        assert!(
            sibling.already_verified(&parent, witness(&parent, 3, &parent_path)),
            "an in-flight proof must not evict the parent's authenticated identities"
        );
        drop(sibling);
        let mut winner = verified.begin(root.path());
        authenticate(&mut winner, &successor, &successor_path);
        winner.commit();
        stale.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&successor, witness(&successor, 3, &successor_path)));
        assert!(
            !latest.already_verified(&parent, witness(&parent, 3, &parent_path)),
            "a stale proof must not resurrect segments pruned by a committed successor"
        );
        assert!(!latest.already_verified(&abandoned, witness(&abandoned, 3, &abandoned_path)));
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
        assert!(successor.already_verified(&retained, witness(&retained, 3, &retained_path)));
        authenticate(&mut successor, &changed, &changed_path);

        // A completed parent recheck must not invalidate the in-flight edit.
        let mut unchanged = verified.begin(root.path());
        assert!(unchanged.already_verified(&retained, witness(&retained, 3, &retained_path)));
        assert!(unchanged.already_verified(&replaced, witness(&replaced, 3, &replaced_path)));
        unchanged.commit();
        successor.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&retained, witness(&retained, 3, &retained_path)));
        assert!(
            latest.already_verified(&changed, witness(&changed, 3, &changed_path)),
            "an unchanged overlapping proof must not discard authenticated edited source"
        );
        assert!(
            !latest.already_verified(&replaced, witness(&replaced, 3, &replaced_path)),
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
        assert!(successor.already_verified(&retained, witness(&retained, 3, &retained_path)));
        successor.commit();

        let mut latest = verified.begin(root.path());
        assert!(latest.already_verified(&retained, witness(&retained, 3, &retained_path)));
        assert!(
            !latest.already_verified(&removed, witness(&removed, 3, &removed_path)),
            "a deletion-only successor must still prune obsolete authentication entries"
        );
        drop(latest);
        verified.begin(root.path()).commit();
        let mut empty = verified.begin(root.path());
        assert!(!empty.already_verified(&retained, witness(&retained, 3, &retained_path)));
    }

    #[test]
    fn many_distinct_roots_evict_cold_proofs_but_retain_recent_source() {
        let temp = TempDir::new().unwrap();
        let verified = VerifiedSealedSegmentsV1::default();
        let mut roots = Vec::new();
        for index in 0..MAX_VERIFIED_SEGMENT_ROOTS {
            let root = temp.path().join(format!("root-{index}"));
            std::fs::create_dir(&root).unwrap();
            let (digest, path) = write_segment(&root, b"[1]");
            let mut proof = verified.begin(&root);
            authenticate(&mut proof, &digest, &path);
            proof.commit();
            roots.push((root, digest, path));
        }

        // A completed read without a commit still makes this root recently used.
        let (recent_root, recent_digest, recent_path) = &roots[0];
        let mut recent = verified.begin(recent_root);
        assert!(recent.already_verified(recent_digest, witness(recent_digest, 3, recent_path)));
        drop(recent);

        let incoming_root = temp.path().join("incoming-root");
        std::fs::create_dir(&incoming_root).unwrap();
        let (incoming_digest, incoming_path) = write_segment(&incoming_root, b"[2]");
        let mut incoming = verified.begin(&incoming_root);
        authenticate(&mut incoming, &incoming_digest, &incoming_path);
        incoming.commit();

        assert_eq!(
            verified
                .roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .roots
                .len(),
            MAX_VERIFIED_SEGMENT_ROOTS,
            "a daemon must not retain one full authenticated map per historical root"
        );

        let (cold_root, cold_digest, cold_path) = &roots[1];
        let mut cold = verified.begin(cold_root);
        assert!(
            !cold.already_verified(cold_digest, witness(cold_digest, 3, cold_path)),
            "the least-recently-used source must be hashed again after eviction"
        );
        drop(cold);

        let mut retained = verified.begin(recent_root);
        assert!(
            retained.already_verified(recent_digest, witness(recent_digest, 3, recent_path)),
            "the recently accessed root must remain warm"
        );
        let mut newest = verified.begin(&incoming_root);
        assert!(newest.already_verified(
            &incoming_digest,
            witness(&incoming_digest, 3, &incoming_path)
        ));
    }

    #[test]
    fn in_flight_successor_retains_parent_across_other_root_evictions() {
        let temp = TempDir::new().unwrap();
        let verified = VerifiedSealedSegmentsV1::default();
        let mut roots = Vec::new();
        let mut replaced = None;
        for index in 0..MAX_VERIFIED_SEGMENT_ROOTS {
            let root = temp.path().join(format!("root-{index}"));
            std::fs::create_dir(&root).unwrap();
            let (digest, path) = write_segment(&root, b"[1]");
            let mut parent = verified.begin(&root);
            authenticate(&mut parent, &digest, &path);
            if index == 0 {
                let (old_digest, old_path) = write_segment(&root, b"[3]");
                authenticate(&mut parent, &old_digest, &old_path);
                replaced = Some((old_digest, old_path));
            }
            parent.commit();
            roots.push((root, digest, path));
        }

        let (active_root, inherited_digest, inherited_path) = &roots[0];
        let (replaced_digest, replaced_path) = replaced.unwrap();
        let mut successor = verified.begin(active_root);
        assert!(successor.already_verified(
            inherited_digest,
            witness(inherited_digest, 3, inherited_path)
        ));
        let (changed_digest, changed_path) = write_segment(active_root, b"[2]");
        authenticate(&mut successor, &changed_digest, &changed_path);

        // Make the still-running successor's root the least recently used.
        // An unpinned LRU cache would evict this root on the next insertion.
        for (root, digest, path) in roots.iter().skip(1) {
            let mut proof = verified.begin(root);
            assert!(proof.already_verified(digest, witness(digest, 3, path)));
        }

        let incoming_root = temp.path().join("incoming-root");
        std::fs::create_dir(&incoming_root).unwrap();
        let (incoming_digest, incoming_path) = write_segment(&incoming_root, b"[4]");
        let mut incoming = verified.begin(&incoming_root);
        authenticate(&mut incoming, &incoming_digest, &incoming_path);
        incoming.commit();
        successor.commit();

        let mut latest = verified.begin(active_root);
        assert!(
            latest.already_verified(
                inherited_digest,
                witness(inherited_digest, 3, inherited_path)
            ),
            "an in-flight edit must keep its inherited authenticated source"
        );
        assert!(
            latest.already_verified(&changed_digest, witness(&changed_digest, 3, &changed_path)),
            "evicting other roots must not discard the edited generation's proof"
        );
        assert!(
            !latest.already_verified(
                &replaced_digest,
                witness(&replaced_digest, 3, &replaced_path)
            ),
            "the successor must not resurrect a replaced parent segment"
        );

        let (evicted_root, evicted_digest, evicted_path) = &roots[1];
        let mut evicted = verified.begin(evicted_root);
        assert!(
            !evicted.already_verified(evicted_digest, witness(evicted_digest, 3, evicted_path)),
            "the oldest idle root, rather than an active proof, must be evicted"
        );
        let cache = verified
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(cache.roots.len(), MAX_VERIFIED_SEGMENT_ROOTS);
    }

    #[test]
    fn all_pinned_roots_preserve_newly_authenticated_source_until_idle_trim() {
        let temp = TempDir::new().unwrap();
        let verified = VerifiedSealedSegmentsV1::default();
        let mut existing = Vec::new();
        for index in 0..MAX_VERIFIED_SEGMENT_ROOTS {
            let root = temp.path().join(format!("root-{index}"));
            std::fs::create_dir(&root).unwrap();
            let (digest, path) = write_segment(&root, b"[1]");
            let mut initial = verified.begin(&root);
            authenticate(&mut initial, &digest, &path);
            initial.commit();
            existing.push((root, digest, path));
        }

        // The entire parent cache is pinned by real in-flight proofs. The
        // previously sole evictable candidate was the successful newcomer.
        let mut pinned = Vec::new();
        for (root, digest, path) in &existing {
            let mut proof = verified.begin(root);
            assert!(proof.already_verified(digest, witness(digest, 3, path)));
            pinned.push(proof);
        }
        let new_root = temp.path().join("new-root");
        std::fs::create_dir(&new_root).unwrap();
        let (new_digest, new_path) = write_segment(&new_root, b"[2]");
        let mut newcomer = verified.begin(&new_root);
        authenticate(&mut newcomer, &new_digest, &new_path);
        newcomer.commit();

        let mut warm = verified.begin(&new_root);
        assert!(
            warm.already_verified(&new_digest, witness(&new_digest, 3, &new_path)),
            "a complete new-root proof must not be its own eviction victim"
        );
        drop(warm);
        assert_eq!(
            verified
                .roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .roots
                .len(),
            MAX_VERIFIED_SEGMENT_ROOTS + 1,
            "only the one temporarily pinned-over-cap root may remain"
        );

        // Releasing old proof references restores the idle bound without
        // discarding new source or reintroducing a stale parent entry.
        drop(pinned);
        let mut retained = verified.begin(&new_root);
        assert!(retained.already_verified(&new_digest, witness(&new_digest, 3, &new_path)));
        assert_eq!(
            verified
                .roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .roots
                .len(),
            MAX_VERIFIED_SEGMENT_ROOTS
        );
        let (old_root, old_digest, old_path) = &existing[0];
        let mut evicted = verified.begin(old_root);
        assert!(
            !evicted.already_verified(old_digest, witness(old_digest, 3, old_path)),
            "the oldest retired root, not the new generation, must be evicted"
        );
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
            !retry.already_verified(&digest, witness(&digest, 3, &path)),
            "retaining an old proof must not trust a replacement's same length and mtime"
        );
        drop(original);
    }

    #[test]
    fn retained_proof_binds_pack_identity_and_logical_extent() {
        let root = TempDir::new().unwrap();
        let bytes = b"prefix[1]tail";
        let path = root.path().join("pack");
        std::fs::write(&path, bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        let fingerprint = SealedSegmentFingerprintV1::of(&file.metadata().unwrap());
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        let pack = ManifestDigest::from_sha256_bytes(&Sha256::digest(&actual)).unwrap();
        let logical = ManifestDigest::from_sha256_bytes(&Sha256::digest(&actual[6..9])).unwrap();
        let source = SealedSegmentWitnessV1::new(&pack, fingerprint, 6, 3);
        let verified = VerifiedSealedSegmentsV1::default();
        let mut first = verified.begin(root.path());
        first.hashed(&logical, source.clone());
        first.commit();

        let mut next = verified.begin(root.path());
        assert!(next.already_verified(&logical, source));
        for (offset, size_bytes) in [(0, 3), (6, 4), (u64::MAX, 3)] {
            assert!(!next.already_verified(
                &logical,
                SealedSegmentWitnessV1::new(&pack, fingerprint, offset, size_bytes),
            ));
        }
        let other_pack = ManifestDigest::from_sha256_bytes(&Sha256::digest(b"other pack")).unwrap();
        assert!(!next.already_verified(
            &logical,
            SealedSegmentWitnessV1::new(&other_pack, fingerprint, 6, 3),
        ));
        assert!(
            !next.already_verified(&pack, SealedSegmentWitnessV1::new(&pack, fingerprint, 6, 3),)
        );
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
        assert!(!foreign.already_verified(&digest, witness(&digest, 3, &path)));
        let mut retry = verified.begin(root.path());
        assert!(!retry.already_verified(&digest, witness(&digest, 4, &path)));
        assert!(retry.already_verified(&digest, witness(&digest, 3, &path)));
        std::fs::remove_file(&path).unwrap();
        assert!(!retry.already_verified(&digest, witness(&digest, 3, &path)));
    }
}
