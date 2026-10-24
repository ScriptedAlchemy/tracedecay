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
use std::sync::Mutex;

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

/// Per segments root, the segments of the generation last proven there.
#[derive(Debug, Default)]
pub(crate) struct VerifiedSealedSegmentsV1 {
    roots: Mutex<HashMap<PathBuf, BTreeMap<ManifestDigest, SealedSegmentFingerprintV1>>>,
}

/// One proof's view: what it may skip, and what it hashed.
pub(super) struct SealedSegmentProofV1<'v> {
    verified: &'v VerifiedSealedSegmentsV1,
    segments_root: PathBuf,
    prior: BTreeMap<ManifestDigest, SealedSegmentFingerprintV1>,
    proven: BTreeMap<ManifestDigest, SealedSegmentFingerprintV1>,
}

impl VerifiedSealedSegmentsV1 {
    /// Start a proof under `segments_root`. The record leaves with it, so a
    /// proof that fails leaves the next one to read every segment.
    pub(super) fn begin(&self, segments_root: &Path) -> SealedSegmentProofV1<'_> {
        let prior = self
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(segments_root)
            .unwrap_or_default();
        SealedSegmentProofV1 {
            verified: self,
            segments_root: segments_root.to_path_buf(),
            prior,
            proven: BTreeMap::new(),
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
        let Some(prior) = self.prior.get(digest).copied() else {
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
            self.proven.insert(digest.clone(), fingerprint);
        }
    }

    /// The proof authenticated every segment: remember exactly this
    /// generation's segments, so the record stays one generation large.
    pub(super) fn commit(self) {
        self.verified
            .roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.segments_root, self.proven);
    }
}
