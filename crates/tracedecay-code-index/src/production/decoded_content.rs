//! Decoded file pages as content, shared by every generation that sealed it.
//!
//! A file segment names repository content: its identities are markers and
//! its bytes are content-addressed, so linked worktrees sealing identical
//! trees address identical segments. What differs between their generations
//! is the manifest, snapshot, lineage, and projection evidence each worktree
//! seals for itself. The pages a decode materializes are therefore keyed by
//! the generation's segment roster, and a second worktree decoding the same
//! roster takes a reference to the pages the first one decoded instead of
//! decoding a copy.
//!
//! A page restores its generation and snapshot markers against the manifest
//! that decoded it. Like a page carried forward into a successor generation,
//! those anchors are extraction provenance; the generation that serves a page
//! binds serving identity.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use tracedecay_domain::ManifestDigest;

use super::FileGenerationArtifactsV1;

/// The file pages of one sealed content, held once however many generations
/// serve it.
pub struct DecodedGenerationContentV1 {
    digest: ManifestDigest,
    pub(super) files: Vec<Arc<FileGenerationArtifactsV1>>,
    retained_bytes: OnceLock<u64>,
}

impl fmt::Debug for DecodedGenerationContentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedGenerationContentV1")
            .field("digest", &self.digest)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl DecodedGenerationContentV1 {
    pub(super) fn new(digest: ManifestDigest, files: Vec<Arc<FileGenerationArtifactsV1>>) -> Self {
        Self {
            digest,
            files,
            retained_bytes: OnceLock::new(),
        }
    }

    /// The digest of the segment roster these pages decode, independent of
    /// the worktree and generation that sealed it.
    #[must_use]
    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    /// Bytes the pages hold, with the chunk and symbol records they share
    /// with every generation built over them.
    #[must_use]
    pub fn retained_bytes(&self) -> u64 {
        *self
            .retained_bytes
            .get_or_init(|| u64::try_from(self.measure_resident_bytes()).unwrap_or(u64::MAX))
    }
}

/// Registry-scoped index of decoded content. It holds weak references only:
/// the generations serving a content own it, and it is freed with the last
/// of them.
#[derive(Clone, Default)]
pub struct SharedDecodedContentPoolV1 {
    entries: Arc<Mutex<HashMap<ManifestDigest, Weak<DecodedGenerationContentV1>>>>,
}

impl fmt::Debug for SharedDecodedContentPoolV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedDecodedContentPoolV1")
            .finish_non_exhaustive()
    }
}

impl SharedDecodedContentPoolV1 {
    pub(super) fn lookup(
        &self,
        digest: &ManifestDigest,
    ) -> Option<Arc<DecodedGenerationContentV1>> {
        self.lock().get(digest).and_then(Weak::upgrade)
    }

    /// Admit freshly decoded content, or the content a concurrent decode of
    /// the same roster admitted first, so the pool never holds two copies.
    pub(super) fn admit(
        &self,
        content: DecodedGenerationContentV1,
    ) -> Arc<DecodedGenerationContentV1> {
        let mut entries = self.lock();
        if let Some(existing) = entries.get(&content.digest).and_then(Weak::upgrade) {
            return existing;
        }
        entries.retain(|_, entry| entry.strong_count() > 0);
        let content = Arc::new(content);
        entries.insert(content.digest.clone(), Arc::downgrade(&content));
        content
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ManifestDigest, Weak<DecodedGenerationContentV1>>> {
        // Every critical section is one lookup or insert into a weak index,
        // so a poisoned lock never guards a half-written map.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
