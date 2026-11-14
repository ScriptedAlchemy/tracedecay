//! An in-memory sealed publication store.
//!
//! Benchmarks, evaluation tools, and tests publish through the same sealed
//! format the daemon store writes: a content pack, a generation evidence
//! pack, and each scope's active manifest. Logical segments exist only in an
//! unpublished preparation; committed readers resolve their packed extents.

use std::collections::{BTreeMap, HashMap};
use std::mem::size_of;
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use tracedecay_domain::{CodeGenerationId, ManifestDigest};
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

use super::resident_bytes::{ARC_HEADER_BYTES, btree_bytes};
use super::{
    CodeIndexAtomicPublicationPort, CodeIndexGenerationScopeV1, CodeIndexPreparationPort,
    CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexSealedGenerationV1, CodeIndexSealedPublicationV1, ConstructionMemoryV1,
    SealedGenerationContentExtentV1, SealedGenerationContentPackV1, SealedGenerationPackLayoutV1,
    SealedGenerationPackReadFactoryV1, SealedGenerationSegmentIdentityV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, SealedSegmentReaderV1,
    SharedDecodedContentPoolV1,
};

type MemoryActivePublicationV1 = (CodeGenerationId, Arc<[u8]>, Arc<MemoryCommittedPayloadsV1>);

#[derive(Default)]
struct MemorySealedStateV1 {
    segments: HashMap<ManifestDigest, Arc<MemoryCommittedPayloadsV1>>,
    active: BTreeMap<CodeIndexGenerationScopeV1, MemoryActivePublicationV1>,
    manifests: HashMap<CodeGenerationId, Arc<[u8]>>,
    publications: u64,
    reads: HashMap<ManifestDigest, u64>,
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

    /// A reader over every physical payload pack this store holds.
    pub fn pack_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let state = Arc::clone(&self.state);
        Arc::new(
            move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                buffer.clear();
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                let digest = request_digest(request);
                *state.reads.entry(digest.clone()).or_default() += 1;
                let bytes = state
                    .segments
                    .get(digest)
                    .and_then(|owner| owner.packs.get(digest))
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "in-memory sealed store has no such payload pack".to_owned(),
                        )
                    })?;
                buffer.extend_from_slice(checked_slice(bytes, request)?);
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

    /// Reads this store served of the segment `digest` addresses.
    pub fn segment_reads(&self, digest: &ManifestDigest) -> u64 {
        self.state().reads.get(digest).copied().unwrap_or_default()
    }

    /// Bytes every stored segment holds.
    pub fn segment_bytes(&self) -> u64 {
        self.state()
            .segments
            .iter()
            .filter_map(|(digest, owner)| owner.packs.get(digest))
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
        operation: &ResidentMemoryOperationV1,
    ) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
        let mut memory = ConstructionMemoryV1::new(operation.clone())?;
        let read = self.pack_reader();
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            manifest,
            &SharedDecodedContentPoolV1::default(),
            |request, buffer| read(request, buffer),
            &mut |bytes| {
                let bytes = u64::try_from(bytes)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                memory.resize(bytes)
            },
        )
    }

    /// Decode `scope`'s active generation whole.
    pub fn decode_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let Some(manifest) = self
            .state()
            .active
            .get(scope)
            .map(|(_, bytes, _)| Arc::clone(bytes))
        else {
            return Ok(None);
        };
        self.decode(&manifest, operation).map(Some)
    }
}

/// The exact final payload allocations and layout retain their construction
/// reservation for as long as the in-memory store or an escaped reader owns
/// them. No discarded staging segment or scratch allocation remains charged.
struct MemoryCommittedPayloadsV1 {
    layout: SealedGenerationPackLayoutV1,
    packs: BTreeMap<ManifestDigest, Arc<[u8]>>,
    _memory: ConstructionMemoryV1,
}

impl MemoryCommittedPayloadsV1 {
    fn resident_bytes(
        layout: &SealedGenerationPackLayoutV1,
        packs: &BTreeMap<ManifestDigest, Arc<[u8]>>,
    ) -> usize {
        size_of::<Self>()
            .saturating_add(ARC_HEADER_BYTES)
            .saturating_add(layout.retained_bytes())
            .saturating_add(btree_bytes(
                packs.len(),
                size_of::<(ManifestDigest, Arc<[u8]>)>(),
            ))
            .saturating_add(packs.iter().fold(0_usize, |bytes, (digest, value)| {
                bytes
                    .saturating_add(digest.as_str().len())
                    .saturating_add(ARC_HEADER_BYTES)
                    .saturating_add(value.len())
            }))
            // Each physical address also becomes one key in the store's
            // lookup index; its value shares this exact payload owner.
            .saturating_add(
                packs
                    .keys()
                    .map(|digest| digest.as_str().len())
                    .sum::<usize>(),
            )
    }
}

struct MemoryPreparationStateV1 {
    segments: BTreeMap<ManifestDigest, Arc<[u8]>>,
    packs: BTreeMap<ManifestDigest, Arc<[u8]>>,
    parent: Option<Arc<MemoryCommittedPayloadsV1>>,
    content_pack: Option<SealedGenerationContentPackV1>,
    committed_payloads: Option<Arc<MemoryCommittedPayloadsV1>>,
    evidence: Vec<u8>,
    evidence_pages: u32,
    evidence_identity: Option<SealedGenerationSegmentIdentityV1>,
    closed: bool,
    committed: bool,
    memory: ConstructionMemoryV1,
}

fn request_digest(request: SealedGenerationSegmentReadV1<'_>) -> &ManifestDigest {
    match request {
        SealedGenerationSegmentReadV1::Whole { digest, .. }
        | SealedGenerationSegmentReadV1::Range { digest, .. } => digest,
    }
}

fn checked_slice<'a>(
    bytes: &'a [u8],
    request: SealedGenerationSegmentReadV1<'_>,
) -> Result<&'a [u8], CodeIndexProductionErrorV1> {
    let (size_bytes, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { size_bytes, .. } => (size_bytes, 0, size_bytes),
        SealedGenerationSegmentReadV1::Range {
            size_bytes,
            offset,
            length,
            ..
        } => (size_bytes, offset, length),
    };
    offset
        .checked_add(length)
        .filter(|end| *end <= size_bytes && size_bytes == bytes.len() as u64)
        .and_then(|end| usize::try_from(offset).ok().zip(usize::try_from(end).ok()))
        .and_then(|(start, end)| bytes.get(start..end))
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "in-memory sealed payload extent does not verify".to_owned(),
            )
        })
}

impl MemoryPreparationStateV1 {
    fn new(
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Self, CodeIndexPublicationStoreErrorV1> {
        Ok(Self {
            segments: BTreeMap::new(),
            packs: BTreeMap::new(),
            parent: None,
            content_pack: None,
            committed_payloads: None,
            evidence: Vec::new(),
            evidence_pages: 0,
            evidence_identity: None,
            closed: false,
            committed: false,
            memory: ConstructionMemoryV1::new(operation.clone())
                .map_err(MemoryCodeIndexPreparationV1::publication_error)?,
        })
    }

    fn resident_bytes(&self) -> usize {
        let map = |values: &BTreeMap<ManifestDigest, Arc<[u8]>>| {
            btree_bytes(values.len(), size_of::<(ManifestDigest, Arc<[u8]>)>()).saturating_add(
                values.iter().fold(0_usize, |bytes, (digest, value)| {
                    bytes
                        .saturating_add(digest.as_str().len())
                        .saturating_add(ARC_HEADER_BYTES)
                        .saturating_add(value.len())
                }),
            )
        };
        size_of::<Self>()
            .saturating_add(ARC_HEADER_BYTES)
            .saturating_add(map(&self.segments))
            .saturating_add(map(&self.packs))
            .saturating_add(self.evidence.capacity())
            .saturating_add(
                self.content_pack
                    .as_ref()
                    .map_or(0, SealedGenerationContentPackV1::retained_bytes),
            )
            .saturating_add(
                self.evidence_identity
                    .as_ref()
                    .map_or(0, |identity| identity.digest.as_str().len()),
            )
    }

    fn reserve(&mut self, additional: usize) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.memory
            .resize(self.resident_bytes().saturating_add(additional) as u64)
            .map_err(MemoryCodeIndexPreparationV1::publication_error)
    }

    fn map_entry_reservation(
        map: &BTreeMap<ManifestDigest, Arc<[u8]>>,
        digest: &ManifestDigest,
        bytes: usize,
    ) -> usize {
        btree_bytes(
            map.len().saturating_add(1),
            size_of::<(ManifestDigest, Arc<[u8]>)>(),
        )
        .saturating_sub(btree_bytes(
            map.len(),
            size_of::<(ManifestDigest, Arc<[u8]>)>(),
        ))
        .saturating_add(digest.as_str().len())
        .saturating_add(ARC_HEADER_BYTES)
        .saturating_add(bytes)
    }

    fn read_logical(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
        read_pack: &SealedSegmentReaderV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        buffer.clear();
        if self.closed && !self.committed {
            return Err(CodeIndexProductionErrorV1::Contract(
                "publication preparation is closed".to_owned(),
            ));
        }
        if let Some(owner) = self.committed_payloads.as_ref() {
            return owner.layout.read(request, buffer, read_pack);
        }
        let digest = request_digest(request);
        let staged = self.segments.get(digest).or_else(|| {
            self.evidence_identity
                .as_ref()
                .filter(|identity| &identity.digest == digest)
                .and_then(|_| self.packs.get(digest))
        });
        if let Some(bytes) = staged {
            buffer.extend_from_slice(checked_slice(bytes, request)?);
            return Ok(());
        }
        self.parent
            .as_ref()
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "logical segment is absent from this preparation and its bound parent"
                        .to_owned(),
                )
            })?
            .layout
            .read(request, buffer, read_pack)
    }
}

/// Unpublished segments of one exact in-memory store and scope. An escaped
/// reader shares invalidation until commit, then the admitted final payload
/// owner; abort drops the bytes and makes every later read refuse.
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
        state.packs.clear();
        state.parent = None;
        state.content_pack = None;
        state.evidence_identity = None;
        state.evidence = Vec::new();
        drop(state.memory.charge.take());
        state.memory.charged_bytes = 0;
    }

    fn unavailable(message: &str) -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(message.to_owned())
    }

    fn publication_error(error: CodeIndexProductionErrorV1) -> CodeIndexPublicationStoreErrorV1 {
        match error {
            CodeIndexProductionErrorV1::Publication(error) => error,
            other => Self::unavailable(&other.to_string()),
        }
    }
}

impl Drop for MemoryCodeIndexPreparationV1 {
    fn drop(&mut self) {
        self.close();
    }
}

impl CodeIndexPreparationPort for MemoryCodeIndexPreparationV1 {
    fn bind_parent(
        &mut self,
        parent: &CodeIndexSealedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut prepared = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if prepared.closed
            || prepared.parent.is_some()
            || !prepared.segments.is_empty()
            || !prepared.packs.is_empty()
            || !prepared.evidence.is_empty()
        {
            return Err(Self::unavailable(
                "parent binding must precede publication staging",
            ));
        }
        let store = self.store.state();
        let Some((generation, manifest, payloads)) = store.active.get(&self.scope) else {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        };
        if Some(generation) != self.expected.as_ref()
            || manifest.as_ref() != parent.manifest_bytes()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        // The store already owns the layout from this exact committed
        // manifest. Sharing that owner requires no second parse or extent map.
        prepared.parent = Some(Arc::clone(payloads));
        Ok(())
    }

    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed || state.content_pack.is_some() {
            return Err(Self::unavailable(
                "publication preparation is closed or finalized",
            ));
        }
        match publication {
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal,
                page_digest,
                bytes,
            } => {
                if state.evidence_identity.is_some()
                    || page_ordinal != state.evidence_pages
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
                let capacity = state
                    .evidence
                    .len()
                    .checked_add(bytes.len())
                    .ok_or_else(|| Self::unavailable("staged evidence extent overflowed"))?;
                state.reserve(capacity)?;
                state
                    .evidence
                    .try_reserve_exact(bytes.len())
                    .map_err(|error| Self::unavailable(&error.to_string()))?;
                state.evidence.extend_from_slice(bytes);
                state.evidence_pages = next;
            }
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                segment_size_bytes,
                page_count,
            } => {
                if state.evidence_identity.is_some()
                    || state.evidence.len() as u64 != segment_size_bytes
                    || state.evidence_pages != page_count
                    || ManifestDigest::from_sha256_bytes(&Sha256::digest(&state.evidence))
                        .map_err(|error| Self::unavailable(&error.to_string()))?
                        != *segment_digest
                {
                    return Err(Self::unavailable("staged evidence pack does not verify"));
                }
                let reservation = MemoryPreparationStateV1::map_entry_reservation(
                    &state.packs,
                    segment_digest,
                    state.evidence.len(),
                )
                .saturating_add(segment_digest.as_str().len());
                state.reserve(reservation)?;
                let bytes = Arc::from(std::mem::take(&mut state.evidence));
                state.packs.insert(segment_digest.clone(), bytes);
                state.evidence_identity = Some(SealedGenerationSegmentIdentityV1 {
                    digest: segment_digest.clone(),
                    size_bytes: segment_size_bytes,
                });
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
                if let Some(existing) = state.segments.get(digest) {
                    if existing.as_ref() != bytes {
                        return Err(Self::unavailable(
                            "staged content address has conflicting bytes",
                        ));
                    }
                    return Ok(());
                }
                let reservation = MemoryPreparationStateV1::map_entry_reservation(
                    &state.segments,
                    digest,
                    bytes.len(),
                );
                state.reserve(reservation)?;
                state.segments.insert(digest.clone(), Arc::from(bytes));
            }
        }
        state.reserve(0)
    }

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let staged = Arc::clone(&self.state);
        let incumbent = self.store.pack_reader();
        Arc::new(move |request, buffer| {
            // Commit and abort wait for active copies, not escaped closures.
            let state = staged.lock().unwrap_or_else(PoisonError::into_inner);
            state.read_logical(request, buffer, incumbent.as_ref())
        })
    }

    fn finalize_content_pack(
        &mut self,
        segments: &[SealedGenerationSegmentIdentityV1],
    ) -> Result<SealedGenerationContentPackV1, CodeIndexPublicationStoreErrorV1> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed || state.content_pack.is_some() || state.evidence_identity.is_none() {
            return Err(Self::unavailable(
                "content pack finalization is not admitted",
            ));
        }
        let digest_bytes = state
            .evidence_identity
            .as_ref()
            .ok_or_else(|| Self::unavailable("generation evidence is not finalized"))?
            .digest
            .as_str()
            .len();
        let mut previous = None;
        let mut total = 0_usize;
        let mut largest = 0_usize;
        let mut descriptors = size_of::<SealedGenerationContentPackV1>();
        for identity in segments {
            if previous.is_some_and(|digest| digest >= &identity.digest) {
                return Err(Self::unavailable(
                    "content pack roster is not strictly ordered",
                ));
            }
            previous = Some(&identity.digest);
            let length = usize::try_from(identity.size_bytes)
                .map_err(|_| Self::unavailable("content pack extent exceeds address space"))?;
            total = total
                .checked_add(length)
                .ok_or_else(|| Self::unavailable("content pack extent overflowed"))?;
            largest = largest.max(length);
            descriptors = descriptors
                .saturating_add(size_of::<SealedGenerationContentExtentV1>())
                .saturating_add(identity.digest.as_str().len());
        }
        // Vec-to-Arc conversion retains both allocations. Segment scratch and
        // the returned and retained descriptor copies coexist with them.
        let map = btree_bytes(
            state.packs.len().saturating_add(1),
            size_of::<(ManifestDigest, Arc<[u8]>)>(),
        );
        state.reserve(
            total
                .saturating_mul(2)
                .saturating_add(largest)
                .saturating_add(descriptors.saturating_mul(2))
                .saturating_add(map)
                .saturating_add(ARC_HEADER_BYTES)
                .saturating_add(digest_bytes.saturating_mul(3)),
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|error| Self::unavailable(&error.to_string()))?;
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(largest)
            .map_err(|error| Self::unavailable(&error.to_string()))?;
        let mut extents = Vec::new();
        extents
            .try_reserve_exact(segments.len())
            .map_err(|error| Self::unavailable(&error.to_string()))?;
        let read_pack = self.store.pack_reader();
        for identity in segments {
            state
                .read_logical(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &identity.digest,
                        size_bytes: identity.size_bytes,
                    },
                    &mut scratch,
                    read_pack.as_ref(),
                )
                .map_err(Self::publication_error)?;
            if ManifestDigest::from_sha256_bytes(&Sha256::digest(&scratch))
                .map_err(|error| Self::unavailable(&error.to_string()))?
                != identity.digest
            {
                return Err(Self::unavailable(
                    "content pack source digest does not verify",
                ));
            }
            extents.push(SealedGenerationContentExtentV1 {
                digest: identity.digest.clone(),
                offset: bytes.len() as u64,
                size_bytes: identity.size_bytes,
            });
            bytes.extend_from_slice(&scratch);
        }
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
            .map_err(|error| Self::unavailable(&error.to_string()))?;
        let content_pack = SealedGenerationContentPackV1 {
            digest,
            size_bytes: bytes.len() as u64,
            extents,
        };
        if let Some(existing) = state.packs.get(&content_pack.digest) {
            if existing.as_ref() != bytes.as_slice() {
                return Err(Self::unavailable(
                    "payload pack content address has conflicting bytes",
                ));
            }
        } else {
            state
                .packs
                .insert(content_pack.digest.clone(), Arc::from(bytes));
        }
        state.content_pack = Some(content_pack.clone());
        Ok(content_pack)
    }

    fn pack_reader(&self) -> Arc<SealedSegmentReaderV1> {
        let staged = Arc::clone(&self.state);
        let published = self.store.pack_reader();
        Arc::new(move |request, buffer| {
            buffer.clear();
            let state = staged.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closed && !state.committed {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "publication preparation is closed".to_owned(),
                ));
            }
            if let Some(bytes) = state.packs.get(request_digest(request)) {
                buffer.extend_from_slice(checked_slice(bytes, request)?);
                return Ok(());
            }
            if let Some(owner) = state.committed_payloads.as_ref() {
                let digest = request_digest(request);
                if &owner.layout.content_pack().digest == digest
                    || &owner.layout.evidence_pack().digest == digest
                {
                    return published(request, buffer);
                }
            }
            Err(CodeIndexProductionErrorV1::Contract(
                "payload pack is absent from this preparation".to_owned(),
            ))
        })
    }

    fn read_pass_factory(&self) -> Arc<SealedGenerationPackReadFactoryV1> {
        let staged = Arc::clone(&self.state);
        let published = self.store.pack_reader();
        Arc::new(move || {
            let owner = staged
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .committed_payloads
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "publication preparation is not committed".to_owned(),
                    )
                })?;
            let published = Arc::clone(&published);
            Ok(Arc::new(
                move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                    buffer.clear();
                    if !owner.packs.contains_key(request_digest(request)) {
                        return Err(CodeIndexProductionErrorV1::Contract(
                            "payload pack is absent from this generation".to_owned(),
                        ));
                    }
                    published(request, buffer)
                },
            ) as Arc<SealedSegmentReaderV1>)
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
        let active = self
            .state()
            .active
            .get(scope)
            .map(|(_, bytes, owner)| (Arc::clone(bytes), Arc::clone(owner)));
        Ok(active.map(|(manifest, owner)| {
            let published = self.pack_reader();
            CodeIndexSealedGenerationV1::new(
                manifest,
                Arc::new(move || {
                    let owner = Arc::clone(&owner);
                    let published = Arc::clone(&published);
                    Ok(Arc::new(
                        move |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                            buffer.clear();
                            if !owner.packs.contains_key(request_digest(request)) {
                                return Err(CodeIndexProductionErrorV1::Contract(
                                    "payload pack is absent from this generation".to_owned(),
                                ));
                            }
                            published(request, buffer)
                        },
                    ) as Arc<SealedSegmentReaderV1>)
                }),
            )
        }))
    }

    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1> {
        if self
            .state()
            .active
            .get(scope)
            .map(|(generation, _, _)| generation)
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        let mut state = MemoryPreparationStateV1::new(operation)?;
        state.reserve(0)?;
        Ok(MemoryCodeIndexPreparationV1 {
            store: self.clone(),
            scope: scope.clone(),
            expected: expected_active_generation.cloned(),
            state: Arc::new(Mutex::new(state)),
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
        if prepared.closed
            || prepared.content_pack.is_none()
            || prepared.evidence_identity.is_none()
            || !prepared.evidence.is_empty()
            || prepared.evidence_pages != 0
        {
            return Err(MemoryCodeIndexPreparationV1::unavailable(
                "preparation is closed or its payload packs are incomplete",
            ));
        }
        let source_layout = publication.pack_layout();
        if prepared.content_pack.as_ref() != Some(source_layout.content_pack())
            || prepared.evidence_identity.as_ref() != Some(source_layout.evidence_pack())
        {
            return Err(MemoryCodeIndexPreparationV1::unavailable(
                "prepared pack layout does not match its publication manifest",
            ));
        }
        // The validated publication already owns its parsed map. Admit the
        // committed owner's independent copy before cloning it; parent binding
        // later shares this owner without parsing the manifest again.
        prepared.reserve(
            source_layout
                .retained_bytes()
                .saturating_add(size_of::<MemoryCommittedPayloadsV1>())
                .saturating_add(ARC_HEADER_BYTES)
                .saturating_add(source_layout.content_pack().digest.as_str().len())
                .saturating_add(source_layout.evidence_pack().digest.as_str().len()),
        )?;
        let layout = source_layout.clone();
        let mut retained_preparation_memory =
            ConstructionMemoryV1::new(prepared.memory.operation.clone())
                .map_err(MemoryCodeIndexPreparationV1::publication_error)?;
        retained_preparation_memory
            .resize(size_of::<MemoryPreparationStateV1>().saturating_add(ARC_HEADER_BYTES) as u64)
            .map_err(MemoryCodeIndexPreparationV1::publication_error)?;
        let mut state = self.state();
        if state
            .active
            .get(&preparation.scope)
            .map(|(generation, _, _)| generation)
            != preparation.expected.as_ref()
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        for (digest, size_bytes) in [
            (
                &layout.content_pack().digest,
                layout.content_pack().size_bytes,
            ),
            (
                &layout.evidence_pack().digest,
                layout.evidence_pack().size_bytes,
            ),
        ] {
            let bytes = prepared.packs.get(digest).ok_or_else(|| {
                MemoryCodeIndexPreparationV1::unavailable(
                    "prepared publication references a pack absent from its preparation",
                )
            })?;
            if u64::try_from(bytes.len()).ok() != Some(size_bytes) {
                return Err(MemoryCodeIndexPreparationV1::unavailable(
                    "prepared publication pack extent does not match its manifest",
                ));
            }
        }
        let packs = std::mem::take(&mut prepared.packs);
        prepared.segments.clear();
        prepared.parent = None;
        prepared.content_pack = None;
        prepared.evidence_identity = None;
        prepared.evidence = Vec::new();
        let mut memory = std::mem::replace(&mut prepared.memory, retained_preparation_memory);
        // Only final allocations remain at this point. Settle the reservation
        // before publishing either lookup index; a refusal closes this attempt
        // without changing the incumbent or leaving a partially committed read.
        if let Err(error) =
            memory.resize(MemoryCommittedPayloadsV1::resident_bytes(&layout, &packs) as u64)
        {
            prepared.closed = true;
            return Err(MemoryCodeIndexPreparationV1::publication_error(error));
        }
        let payloads = Arc::new(MemoryCommittedPayloadsV1 {
            layout,
            packs,
            _memory: memory,
        });
        state.segments.extend(
            payloads
                .packs
                .keys()
                .cloned()
                .map(|digest| (digest, Arc::clone(&payloads))),
        );
        prepared.committed_payloads = Some(Arc::clone(&payloads));
        let manifest = Arc::clone(publication.manifest_bytes());
        let generation = publication.manifest().generation_id.clone();
        state
            .manifests
            .insert(generation.clone(), Arc::clone(&manifest));
        state.active.insert(
            preparation.scope.clone(),
            (generation, Arc::clone(&manifest), payloads),
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
        let operation =
            worker_request_with_source("file.preparation", 1_100_000, b"fn prepared() {}")
                .resident_memory
                .begin_operation();
        let mut staged = store
            .prepare(&scope, None, &operation)
            .expect("staged preparation");
        let publication = generation
            .stage_publication(&mut staged, &operation)
            .expect("stage generation");
        let mut empty = store
            .prepare(&scope, None, &operation)
            .expect("independent preparation");
        assert!(store.commit(&mut empty, &publication).is_err());
        assert_eq!(store.publications(), 0);
        assert!(store.load_active(&scope).expect("active slot").is_none());
        store
            .commit(&mut staged, &publication)
            .expect("the complete preparation commits");
        assert_eq!(store.publications(), 1);
        store
            .decode_active(&scope, &operation)
            .expect("committed segments decode")
            .expect("active generation");
    }

    #[test]
    fn readers_lose_aborted_staging_and_keep_successfully_committed_segments() {
        let generation = decoded_fixture();
        let scope = generation.sealed_scope();
        let mut store = MemorySealedPublicationStoreV1::default();
        let operation =
            worker_request_with_source("file.preparation", 1_100_000, b"fn prepared() {}")
                .resident_memory
                .begin_operation();
        let mut aborted = store
            .prepare(&scope, None, &operation)
            .expect("aborted preparation");
        let publication = generation
            .stage_publication(&mut aborted, &operation)
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
        assert!(
            bytes.is_empty(),
            "refused reads cannot expose prior staging bytes"
        );
        drop(reader);
        assert!(
            operation.authority().snapshot().used_bytes > 0,
            "the returned publication retains its admitted metadata and pack map"
        );
        let retained_publication = publication.clone();
        drop(publication);
        assert!(operation.authority().snapshot().used_bytes > 0);
        drop(retained_publication);
        assert_eq!(operation.authority().snapshot().used_bytes, 0);

        let mut committed = store
            .prepare(&scope, None, &operation)
            .expect("committed preparation");
        let publication = generation
            .stage_publication(&mut committed, &operation)
            .expect("stage generation");
        let reader = committed.segment_reader();
        let staged_bytes = operation.authority().snapshot().used_bytes;
        store.commit(&mut committed, &publication).expect("commit");
        let committed_bytes = operation.authority().snapshot().used_bytes;
        assert!(committed_bytes > 0, "published payloads remain admitted");
        assert!(
            committed_bytes < staged_bytes,
            "discarded logical staging and finalization scratch must be released"
        );
        assert!(
            store.state().segments.len() <= 2,
            "only physical packs become durable"
        );
        assert!(
            CodeIndexPublishedGenerationV1::partitioned_segment_identities(
                publication.manifest_bytes(),
            )
            .unwrap()
            .len()
                > 2,
            "the fixture must contain several logical segments"
        );
        drop(committed);
        reader(request, &mut bytes).expect("committed reader outlives preparation");
        assert_eq!(bytes.len() as u64, identity.size_bytes);
        drop(publication);
        drop(store);
        assert!(operation.authority().snapshot().used_bytes > 0);
        drop(reader);
        assert_eq!(operation.authority().snapshot().used_bytes, 0);
    }
}
