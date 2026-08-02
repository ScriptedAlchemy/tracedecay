use std::{collections::BTreeSet, io::Read, ops::Deref, sync::Arc};

use sha2::{Digest as _, Sha256};
use tracedecay_code_index::production::{CodeIndexCapturedFileV1, CodeIndexPublishedGenerationV1};
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
        self.bytes_read = self.bytes_read.saturating_add(read as u64);
        Ok(read)
    }
}
