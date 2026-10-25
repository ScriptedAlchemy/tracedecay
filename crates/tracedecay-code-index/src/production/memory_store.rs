//! An in-memory sealed publication store.
//!
//! Benchmarks, evaluation tools, and tests publish through the same sealed
//! format the daemon store writes: segments by content address, each
//! scope's active manifest, and the generation evidence pack assembled from
//! its pages.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use tracedecay_domain::{CodeGenerationId, ManifestDigest};

use super::{
    CodeIndexAtomicPublicationPort, CodeIndexGenerationScopeV1, CodeIndexPreparationPort,
    CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexSealedGenerationV1, CodeIndexSealedPublicationV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, SealedSegmentReaderV1,
    SharedDecodedContentPoolV1,
};

#[derive(Default)]
struct MemorySealedStateV1 {
    segments: HashMap<ManifestDigest, Arc<[u8]>>,
    active: BTreeMap<CodeIndexGenerationScopeV1, (CodeGenerationId, Arc<[u8]>)>,
    manifests: HashMap<CodeGenerationId, Arc<[u8]>>,
    publications: u64,
}

/// Sealed generations held in memory, one active generation per scope.
#[derive(Clone, Default)]
pub struct MemorySealedPublicationStoreV1 {
    state: Arc<Mutex<MemorySealedStateV1>>,
}

impl MemorySealedPublicationStoreV1 {
    fn state(&self) -> std::sync::MutexGuard<'_, MemorySealedStateV1> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A reader over every segment this store holds.
    pub fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let state = Arc::clone(&self.state);
        Arc::new(
            move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                let (digest, range) = match request {
                    SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                        (digest, 0..size_bytes)
                    }
                    SealedGenerationSegmentReadV1::Range {
                        digest,
                        offset,
                        length,
                        ..
                    } => (digest, offset..offset.saturating_add(length)),
                };
                let bytes = state.segments.get(digest).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "in-memory sealed store has no such segment".to_owned(),
                    )
                })?;
                let range = usize::try_from(range.start)
                    .ok()
                    .zip(usize::try_from(range.end).ok());
                let slice = range
                    .and_then(|(start, end)| bytes.get(start..end))
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "in-memory sealed segment read is out of range".to_owned(),
                        )
                    })?;
                buffer.clear();
                buffer.extend_from_slice(slice);
                Ok(())
            },
        )
    }

    /// The manifest of the generation `generation` names, when this store
    /// sealed it.
    pub fn manifest_bytes(&self, generation: &CodeGenerationId) -> Option<Arc<[u8]>> {
        self.state().manifests.get(generation).map(Arc::clone)
    }

    /// Publications this store committed.
    pub fn publications(&self) -> u64 {
        self.state().publications
    }

    /// Bytes every stored segment holds.
    pub fn segment_bytes(&self) -> u64 {
        self.state()
            .segments
            .values()
            .map(|bytes| bytes.len() as u64)
            .sum()
    }

    /// Scopes with an active generation.
    pub fn active_scopes(&self) -> usize {
        self.state().active.len()
    }

    /// Whether `other` is a handle on this same store.
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Decode the generation `manifest` seals whole from this store's
    /// segments.
    pub fn decode(
        &self,
        manifest: &[u8],
    ) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
        let read = self.segment_reader();
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            manifest,
            &SharedDecodedContentPoolV1::default(),
            |request, buffer| read(request, buffer),
        )
    }

    /// Decode `scope`'s active generation whole.
    pub fn decode_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let Some(manifest) = self
            .state()
            .active
            .get(scope)
            .map(|(_, bytes)| Arc::clone(bytes))
        else {
            return Ok(None);
        };
        self.decode(&manifest).map(Some)
    }
}

#[derive(Default)]
struct MemoryPreparationStateV1 {
    segments: HashMap<ManifestDigest, Arc<[u8]>>,
    evidence: Vec<u8>,
    evidence_pages: u32,
    closed: bool,
    committed: bool,
}

/// Unpublished segments of one exact in-memory store and scope. An escaped
/// preparation reader shares the invalidation state, not ownership of bytes
/// after commit or abort.
pub struct MemoryCodeIndexPreparationV1 {
    store: MemorySealedPublicationStoreV1,
    scope: CodeIndexGenerationScopeV1,
    expected: Option<CodeGenerationId>,
    state: Arc<Mutex<MemoryPreparationStateV1>>,
}

impl MemoryCodeIndexPreparationV1 {
    fn close(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.committed {
            return;
        }
        state.closed = true;
        state.segments.clear();
        state.evidence = Vec::new();
    }

    fn unavailable(message: &str) -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(message.to_owned())
    }
}

impl Drop for MemoryCodeIndexPreparationV1 {
    fn drop(&mut self) {
        self.close();
    }
}

impl CodeIndexPreparationPort for MemoryCodeIndexPreparationV1 {
    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Err(Self::unavailable("publication preparation is closed"));
        }
        match publication {
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal,
                page_digest,
                bytes,
            } => {
                if page_ordinal != state.evidence_pages
                    || ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
                        .map_err(|error| Self::unavailable(&error.to_string()))?
                        != *page_digest
                {
                    return Err(Self::unavailable(
                        "staged evidence page identity or order does not verify",
                    ));
                }
                let next = state
                    .evidence_pages
                    .checked_add(1)
                    .ok_or_else(|| Self::unavailable("staged evidence page count overflowed"))?;
                state.evidence.extend_from_slice(bytes);
                state.evidence_pages = next;
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                segment_size_bytes,
                page_count,
            } => {
                if state.evidence.len() as u64 != segment_size_bytes
                    || state.evidence_pages != page_count
                    || ManifestDigest::from_sha256_bytes(&Sha256::digest(&state.evidence))
                        .map_err(|error| Self::unavailable(&error.to_string()))?
                        != *segment_digest
                {
                    return Err(Self::unavailable("staged evidence pack does not verify"));
                }
                let bytes = Arc::from(std::mem::take(&mut state.evidence));
                state.segments.insert(segment_digest.clone(), bytes);
                state.evidence_pages = 0;
            }
            SealedGenerationSegmentPublicationV1::File { digest, bytes }
            | SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
            | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes }
            | SealedGenerationSegmentPublicationV1::CodeGraphPage {
                page_digest: digest,
                bytes,
                ..
            } => {
                if ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
                    .map_err(|error| Self::unavailable(&error.to_string()))?
                    != *digest
                {
                    return Err(Self::unavailable(
                        "staged segment content address does not verify",
                    ));
                }
                state.segments.insert(digest.clone(), Arc::from(bytes));
            }
        }
        Ok(())
    }

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let staged = Arc::clone(&self.state);
        let incumbent = self.store.segment_reader();
        Arc::new(move |request, buffer| {
            let (digest, size, range) = match request {
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                    (digest, size_bytes, 0..size_bytes)
                }
                SealedGenerationSegmentReadV1::Range {
                    digest,
                    size_bytes,
                    offset,
                    length,
                } => {
                    let end = offset.checked_add(length).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "staged segment range overflowed".to_owned(),
                        )
                    })?;
                    (digest, size_bytes, offset..end)
                }
            };
            let bytes = {
                let state = staged.lock().unwrap_or_else(PoisonError::into_inner);
                if state.closed && !state.committed {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "publication preparation is closed".to_owned(),
                    ));
                }
                state.segments.get(digest).map(Arc::clone)
            };
            let Some(bytes) = bytes else {
                return incumbent(request, buffer);
            };
            let slice = usize::try_from(range.start)
                .ok()
                .zip(usize::try_from(range.end).ok())
                .filter(|_| size == bytes.len() as u64)
                .and_then(|(start, end)| bytes.get(start..end))
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "staged segment extent does not verify".to_owned(),
                    )
                })?;
            buffer.clear();
            buffer.extend_from_slice(slice);
            Ok(())
        })
    }

    fn abort(mut self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.close();
        Ok(())
    }
}

impl CodeIndexAtomicPublicationPort for MemorySealedPublicationStoreV1 {
    type Preparation = MemoryCodeIndexPreparationV1;

    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let manifest = self
            .state()
            .active
            .get(scope)
            .map(|(_, bytes)| Arc::clone(bytes));
        Ok(manifest
            .map(|manifest| CodeIndexSealedGenerationV1::new(manifest, self.segment_reader())))
    }

    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1> {
        if self
            .state()
            .active
            .get(scope)
            .map(|(generation, _)| generation)
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        Ok(MemoryCodeIndexPreparationV1 {
            store: self.clone(),
            scope: scope.clone(),
            expected: expected_active_generation.cloned(),
            state: Arc::new(Mutex::new(MemoryPreparationStateV1::default())),
        })
    }

    fn commit(
        &mut self,
        preparation: &mut Self::Preparation,
        publication: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        if !self.shares_state_with(&preparation.store)
            || preparation.scope != CodeIndexGenerationScopeV1::for_snapshot(publication.snapshot())
        {
            return Err(MemoryCodeIndexPreparationV1::unavailable(
                "preparation belongs to another publication store or scope",
            ));
        }
        let mut prepared = preparation
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if prepared.closed || !prepared.evidence.is_empty() || prepared.evidence_pages != 0 {
            return Err(MemoryCodeIndexPreparationV1::unavailable(
                "preparation is closed or its evidence pack is incomplete",
            ));
        }
        let mut state = self.state();
        if state
            .active
            .get(&preparation.scope)
            .map(|(generation, _)| generation)
            != preparation.expected.as_ref()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let required = CodeIndexPublishedGenerationV1::partitioned_segment_identities(
            publication.manifest_bytes(),
        )
        .map_err(|error| MemoryCodeIndexPreparationV1::unavailable(&error.to_string()))?;
        for identity in required {
            let bytes = prepared.segments.get(&identity.digest)
                .or_else(|| state.segments.get(&identity.digest))
                .ok_or_else(|| MemoryCodeIndexPreparationV1::unavailable(
                    "prepared publication references a segment absent from its preparation and store"))?;
            if u64::try_from(bytes.len()).ok() != Some(identity.size_bytes) {
                return Err(MemoryCodeIndexPreparationV1::unavailable(
                    "prepared publication segment extent does not match its manifest",
                ));
            }
        }
        state.segments.extend(prepared.segments.drain());
        let manifest = Arc::clone(publication.manifest_bytes());
        let generation = publication.manifest().generation_id.clone();
        state
            .manifests
            .insert(generation.clone(), Arc::clone(&manifest));
        state.active.insert(
            preparation.scope.clone(),
            (generation, Arc::clone(&manifest)),
        );
        state.publications = state.publications.saturating_add(1);
        prepared.closed = true;
        prepared.committed = true;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::production::worker_tests::{
        decoded_generation, worker_config, worker_request_with_source,
    };
    use crate::production::{CodeIndexProductionOwnerV1, UninterruptibleCodeIndexControlV1};

    fn decoded_fixture() -> Arc<CodeIndexPublishedGenerationV1> {
        let mut owner = CodeIndexProductionOwnerV1::new(
            worker_config(),
            MemorySealedPublicationStoreV1::default(),
        )
        .expect("source owner");
        let built = owner
            .build_and_publish(
                worker_request_with_source(
                    "file.preparation",
                    1_100_000,
                    b"pub fn prepared() {}\n",
                ),
                &UninterruptibleCodeIndexControlV1,
            )
            .expect("source publication");
        decoded_generation(&built)
    }

    #[test]
    fn an_unstaged_preparation_cannot_activate_another_preparations_publication() {
        let generation = decoded_fixture();
        let scope = generation.sealed_scope();
        let mut store = MemorySealedPublicationStoreV1::default();
        let mut staged = store.prepare(&scope, None).expect("staged preparation");
        let publication = generation
            .stage_publication(&mut staged)
            .expect("stage generation");
        let mut empty = store
            .prepare(&scope, None)
            .expect("independent preparation");
        assert!(store.commit(&mut empty, &publication).is_err());
        assert_eq!(store.publications(), 0);
        assert!(store.load_active(&scope).expect("active slot").is_none());
        store
            .commit(&mut staged, &publication)
            .expect("the complete preparation commits");
        assert_eq!(store.publications(), 1);
        store
            .decode_active(&scope)
            .expect("committed segments decode")
            .expect("active generation");
    }

    #[test]
    fn readers_lose_aborted_staging_and_keep_successfully_committed_segments() {
        let generation = decoded_fixture();
        let scope = generation.sealed_scope();
        let mut store = MemorySealedPublicationStoreV1::default();
        let mut aborted = store.prepare(&scope, None).expect("aborted preparation");
        let publication = generation
            .stage_publication(&mut aborted)
            .expect("stage generation");
        let identity = CodeIndexPublishedGenerationV1::partitioned_segment_identities(
            publication.manifest_bytes(),
        )
        .expect("segment identities")
        .remove(0);
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &identity.digest,
            size_bytes: identity.size_bytes,
        };
        let reader = aborted.segment_reader();
        let mut bytes = Vec::new();
        reader(request, &mut bytes).expect("open staging is readable");
        aborted.abort().expect("abort");
        assert!(reader(request, &mut bytes).is_err());

        let mut committed = store.prepare(&scope, None).expect("committed preparation");
        let publication = generation
            .stage_publication(&mut committed)
            .expect("stage generation");
        let reader = committed.segment_reader();
        store.commit(&mut committed, &publication).expect("commit");
        drop(committed);
        reader(request, &mut bytes).expect("committed reader outlives preparation");
        assert_eq!(bytes.len() as u64, identity.size_bytes);
    }
}
