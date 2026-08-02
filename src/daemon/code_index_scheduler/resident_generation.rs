use std::{
    collections::{BTreeSet, VecDeque},
    io::Read,
    ops::Deref,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use sha2::{Digest as _, Sha256};
use tracedecay_code_index::production::{
    CodeIndexCapturedFileV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
};
use tracedecay_domain::{
    ProjectId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, WorktreeId,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryReservationV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GenerationDecodeAdmissionV1 {
    AwaitDecode,
    AlreadyDecoded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DecodeSubjectV1 {
    Active,
    Generation(tracedecay_domain::CodeGenerationId),
}

pub(super) const DECODED_GENERATION_CACHE_CAPACITY: usize = 4;

#[derive(Default)]
pub(super) struct DecodedGenerationStateV1 {
    pub(super) active: Option<Arc<ResidentPublishedGenerationV1>>,
    pub(super) active_epoch: u64,
    decoded: VecDeque<Arc<ResidentPublishedGenerationV1>>,
    pub(super) in_flight: Vec<DecodeSubjectV1>,
}

impl DecodedGenerationStateV1 {
    pub(super) fn is_in_flight(&self, subject: &DecodeSubjectV1) -> bool {
        self.in_flight.iter().any(|pending| pending == subject)
    }

    pub(super) fn forget(&mut self, generation_id: &tracedecay_domain::CodeGenerationId) {
        self.decoded
            .retain(|cached| cached.manifest().generation_id != *generation_id);
    }

    pub(super) fn cached(
        &mut self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Option<Arc<ResidentPublishedGenerationV1>> {
        let position = self
            .decoded
            .iter()
            .position(|cached| cached.manifest().generation_id == *generation_id)?;
        let generation = self.decoded.remove(position)?;
        self.decoded.push_back(Arc::clone(&generation));
        Some(generation)
    }
}

#[derive(Default)]
pub(super) struct DecodedGenerationCacheV1 {
    pub(super) state: Mutex<DecodedGenerationStateV1>,
    pub(super) ready: Condvar,
    decodes: AtomicU64,
}

impl DecodedGenerationCacheV1 {
    pub(super) fn poisoned() -> CodeIndexPublicationStoreErrorV1 {
        CodeIndexPublicationStoreErrorV1::Unavailable(
            "daemon decoded-generation lock is poisoned".to_owned(),
        )
    }

    pub(super) fn lock_state(
        &self,
    ) -> Result<MutexGuard<'_, DecodedGenerationStateV1>, CodeIndexPublicationStoreErrorV1> {
        self.state.lock().map_err(|_| Self::poisoned())
    }

    pub(super) fn note_decode(&self) {
        self.decodes.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn decode_count(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    pub(super) fn remember(
        &self,
        generation: Arc<ResidentPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut state = self.lock_state()?;
        let generation_id = generation.manifest().generation_id.clone();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.manifest().generation_id == generation_id)
        {
            return Ok(());
        }
        state.forget(&generation_id);
        state.decoded.push_back(generation);
        while state.decoded.len() > DECODED_GENERATION_CACHE_CAPACITY {
            state.decoded.pop_front();
        }
        Ok(())
    }

    pub(super) fn reclaim_inactive(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.decoded.clear();
    }
}

pub(super) struct DecodeLeaseV1<'cache> {
    pub(super) cache: &'cache DecodedGenerationCacheV1,
    pub(super) subject: DecodeSubjectV1,
    pub(super) epoch: u64,
}

impl Drop for DecodeLeaseV1<'_> {
    fn drop(&mut self) {
        {
            let mut state = self
                .cache
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.in_flight.retain(|pending| *pending != self.subject);
        }
        self.cache.ready.notify_all();
    }
}

#[cfg(test)]
pub(super) struct HeldActiveDecodeV1 {
    pub(super) cache: Arc<DecodedGenerationCacheV1>,
    pub(super) restore: Option<Arc<ResidentPublishedGenerationV1>>,
}

#[cfg(test)]
impl Drop for HeldActiveDecodeV1 {
    fn drop(&mut self) {
        {
            let mut state = self
                .cache
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state
                .in_flight
                .retain(|pending| *pending != DecodeSubjectV1::Active);
            if state.active.is_none() {
                state.active = self.restore.take();
            }
        }
        self.cache.ready.notify_all();
    }
}

pub(super) struct CapturedCandidateV1 {
    pub(super) file: SanitizedCodeFileV1,
    pub(super) captured: CodeIndexCapturedFileV1,
    pub(super) receipt_id: SanitizationReceiptId,
    pub(super) retained: Arc<[u8]>,
}

pub(super) struct CapturedSnapshotV1 {
    pub(super) snapshot: SanitizedCodeSnapshotV1,
    pub(super) captured_files: Vec<CodeIndexCapturedFileV1>,
    pub(super) changed_paths: BTreeSet<String>,
    pub(super) retained_bytes: Vec<Arc<[u8]>>,
    pub(super) reservation: ResidentMemoryReservationV1,
}

pub(super) struct ResidentPublishedGenerationV1 {
    pub(super) generation: Arc<CodeIndexPublishedGenerationV1>,
    pub(super) resident_memory: Arc<ProcessResidentMemoryV1>,
    pub(super) project_id: ProjectId,
    pub(super) worktree_id: WorktreeId,
    pub(super) sealed_bytes: u64,
    pub(super) _reservation: ResidentMemoryReservationV1,
}

impl Deref for ResidentPublishedGenerationV1 {
    type Target = CodeIndexPublishedGenerationV1;

    fn deref(&self) -> &Self::Target {
        self.generation.as_ref()
    }
}

pub(super) struct DigestingReaderV1<R> {
    inner: R,
    hasher: Sha256,
    pub(super) bytes_read: u64,
}

impl<R> DigestingReaderV1<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_read: 0,
        }
    }

    pub(super) fn state_digest(self) -> String {
        format!("sha256:{}", hex::encode(self.hasher.finalize()))
    }
}

impl<R: Read> Read for DigestingReaderV1<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        let read_bytes = u64::try_from(read)
            .map_err(|_| std::io::Error::other("sealed generation read length exceeds u64"))?;
        self.bytes_read = self
            .bytes_read
            .checked_add(read_bytes)
            .ok_or_else(|| std::io::Error::other("sealed generation byte count exceeds u64"))?;
        Ok(read)
    }
}
