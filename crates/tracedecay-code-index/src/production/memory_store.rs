//! An in-memory sealed publication store.
//!
//! Benchmarks, evaluation tools, and tests publish through the same sealed
//! format the daemon store writes: segments by content address, each
//! scope's active manifest, and the generation evidence pack assembled from
//! its pages.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use tracedecay_domain::{CodeGenerationId, ManifestDigest};

use super::{
    CodeIndexAtomicPublicationPort, CodeIndexGenerationScopeV1, CodeIndexProductionErrorV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1, CodeIndexSealedGenerationV1,
    CodeIndexSealedPublicationV1, SealedGenerationSegmentPublicationV1,
    SealedGenerationSegmentReadV1, SealedSegmentReaderV1, SharedDecodedContentPoolV1,
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

    /// Seal `generation` and store its segments, without making it active.
    pub fn seal(
        &self,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        let parent = generation
            .manifest()
            .parent_generation
            .as_ref()
            .and_then(|parent| self.manifest_bytes(parent));
        let mut staged = Vec::<(ManifestDigest, Arc<[u8]>)>::new();
        let mut pack = Vec::new();
        let manifest = generation
            .encode(parent.as_deref(), |publication| {
                match publication {
                    SealedGenerationSegmentPublicationV1::File { digest, bytes }
                    | SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
                    | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => {
                        staged.push((digest.clone(), Arc::from(bytes)));
                    }
                    SealedGenerationSegmentPublicationV1::CodeGraphPage {
                        page_digest,
                        bytes,
                        ..
                    } => staged.push((page_digest.clone(), Arc::from(bytes))),
                    SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                        bytes, ..
                    } => pack.extend_from_slice(bytes),
                    SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                        segment_digest,
                        ..
                    } => {
                        staged.push((segment_digest.clone(), Arc::from(std::mem::take(&mut pack))))
                    }
                }
                Ok(())
            })
            .map_err(|error| CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string()))?;
        let manifest: Arc<[u8]> = Arc::from(manifest);
        let mut state = self.state();
        state.segments.extend(staged);
        state.manifests.insert(
            generation.manifest().generation_id.clone(),
            Arc::clone(&manifest),
        );
        Ok(manifest)
    }
}

impl CodeIndexAtomicPublicationPort for MemorySealedPublicationStoreV1 {
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

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        let incumbent = || {
            self.state()
                .active
                .get(scope)
                .map(|(generation, _)| generation.clone())
        };
        if incumbent().as_ref() != expected_active_generation {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let manifest = self.seal(generation)?;
        let mut state = self.state();
        if state.active.get(scope).map(|(generation, _)| generation) != expected_active_generation {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        state.active.insert(
            scope.clone(),
            (
                generation.manifest().generation_id.clone(),
                Arc::clone(&manifest),
            ),
        );
        state.publications = state.publications.saturating_add(1);
        Ok(manifest)
    }
}
