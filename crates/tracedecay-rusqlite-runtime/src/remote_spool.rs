//! Encrypted, bounded local persistence for PR16 remote capture.
//!
//! The spool never invents encryption. It requires an admitted at-rest
//! encryption authority and fails closed before opening a durable file when
//! that authority is unavailable.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    DirectorySyncPolicy, append_durable, atomic_write, file_len, read_bounded,
    remote::{
        capture::{
            RemoteCapturePersistenceErrorV1, RemoteCaptureSpoolPortV1,
            RemoteSpoolCaptureOutcome as ApplicationCaptureOutcome,
        },
        replay::{
            RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1,
            RemoteReplayTransactionErrorV1, RemoteReplayTransactionOutcomeV1,
            RemoteReplayTransactionPortV1,
        },
    },
};
use tracedecay_store::{
    RemoteCaptureEventIdV1, RemoteCaptureFindingV1, RemoteCaptureFrameV1, RemoteCaptureStateV1,
    RemoteCaptureTransitionV1, RemoteWriterBindingV1, RepositoryWritePayloadV1,
    RuntimeSubmitRequestV1, StoreCommitReceiptV1,
};

use crate::{
    StorageOperationExecutor,
    ledger::{self, LedgerDisposition},
    operation,
};

const FRAME_MAGIC: &[u8; 8] = b"TDRSPL01";
const FRAME_VERSION: u16 = 1;
const FRAME_HEADER_BYTES: usize = 8 + 2 + 8 + 32;

pub trait RemoteSpoolEncryption: Send + Sync {
    /// False means no production key authority is admitted. Callers must not
    /// create or inspect a spool in this state.
    fn is_available(&self) -> bool;
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
    fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSpoolEncryptionError {
    pub operation: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSpoolConfig {
    pub maximum_file_bytes: u64,
    pub maximum_record_bytes: usize,
    pub maximum_events: usize,
}

impl RemoteSpoolConfig {
    pub fn validate(self) -> Result<Self, RemoteSpoolError> {
        if self.maximum_file_bytes == 0
            || self.maximum_record_bytes == 0
            || self.maximum_events == 0
            || self.maximum_file_bytes > usize::MAX as u64
            || self.maximum_record_bytes as u64 > self.maximum_file_bytes
        {
            return Err(RemoteSpoolError::InvalidConfig);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableSpoolRecordV1 {
    Capture {
        frame: RemoteCaptureFrameV1,
    },
    Transition {
        transition: RemoteCaptureTransitionV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSpoolEventV1 {
    pub frame: RemoteCaptureFrameV1,
    pub state: RemoteCaptureStateV1,
    pub replay_attempt: u64,
    pub receipt: Option<StoreCommitReceiptV1>,
    pub findings: Vec<RemoteCaptureFindingV1>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteSpoolSnapshotV1 {
    pub events: BTreeMap<RemoteCaptureEventIdV1, RemoteSpoolEventV1>,
}

impl RemoteSpoolSnapshotV1 {
    pub fn pending(&self) -> impl Iterator<Item = &RemoteSpoolEventV1> {
        self.events
            .values()
            .filter(|event| event.state == RemoteCaptureStateV1::Pending)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteSpoolCaptureOutcome {
    Captured,
    AlreadyCaptured,
}

pub struct RemoteCaptureSpool {
    path: PathBuf,
    config: RemoteSpoolConfig,
    encryption: Box<dyn RemoteSpoolEncryption>,
}

impl RemoteCaptureSpool {
    pub fn open(
        path: PathBuf,
        config: RemoteSpoolConfig,
        encryption: Box<dyn RemoteSpoolEncryption>,
    ) -> Result<Self, RemoteSpoolError> {
        let config = config.validate()?;
        if !path.is_absolute() {
            return Err(RemoteSpoolError::PathNotAbsolute);
        }
        if !encryption.is_available() {
            return Err(RemoteSpoolError::AtRestEncryptionUnavailable);
        }
        if file_len(&path).map_err(RemoteSpoolError::Io)? > config.maximum_file_bytes {
            return Err(RemoteSpoolError::Overflow);
        }
        let spool = Self {
            path,
            config,
            encryption,
        };
        spool.snapshot()?;
        Ok(spool)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn capture(
        &self,
        frame: RemoteCaptureFrameV1,
    ) -> Result<RemoteSpoolCaptureOutcome, RemoteSpoolError> {
        frame.validate().map_err(RemoteSpoolError::Contract)?;
        let snapshot = self.snapshot()?;
        if let Some(existing) = snapshot.events.get(&frame.event_id) {
            return if existing.frame == frame {
                Ok(RemoteSpoolCaptureOutcome::AlreadyCaptured)
            } else {
                Err(RemoteSpoolError::EventIdentityConflict)
            };
        }
        if snapshot.events.len() >= self.config.maximum_events {
            return Err(RemoteSpoolError::Overflow);
        }
        validate_next_sequence(&snapshot, &frame)?;
        self.append(&DurableSpoolRecordV1::Capture { frame })?;
        Ok(RemoteSpoolCaptureOutcome::Captured)
    }

    pub fn transition(
        &self,
        transition: RemoteCaptureTransitionV1,
    ) -> Result<(), RemoteSpoolError> {
        transition.validate().map_err(RemoteSpoolError::Contract)?;
        let snapshot = self.snapshot()?;
        let event = snapshot
            .events
            .get(&transition.event_id)
            .ok_or(RemoteSpoolError::UnknownEvent)?;
        if event.state != transition.from {
            return Err(RemoteSpoolError::StaleTransition);
        }
        if let Some(receipt) = &transition.receipt {
            validate_receipt(&event.frame, receipt)?;
        }
        self.append(&DurableSpoolRecordV1::Transition { transition })
    }

    pub fn snapshot(&self) -> Result<RemoteSpoolSnapshotV1, RemoteSpoolError> {
        if file_len(&self.path).map_err(RemoteSpoolError::Io)? == 0 {
            return Ok(RemoteSpoolSnapshotV1::default());
        }
        let Some(bytes) = read_bounded(&self.path, self.config.maximum_file_bytes as usize)
            .map_err(RemoteSpoolError::Io)?
        else {
            return Ok(RemoteSpoolSnapshotV1::default());
        };
        decode_records(
            &bytes,
            self.config.maximum_record_bytes,
            self.encryption.as_ref(),
        )
    }

    /// Remove only events that have durably passed through Acknowledged into
    /// GarbageCollectionEligible.
    pub fn garbage_collect(&self) -> Result<usize, RemoteSpoolError> {
        let snapshot = self.snapshot()?;
        let removed = snapshot
            .events
            .values()
            .filter(|event| event.state == RemoteCaptureStateV1::GarbageCollectionEligible)
            .count();
        if removed == 0 {
            return Ok(0);
        }
        let mut replacement = Vec::new();
        for event in snapshot
            .events
            .values()
            .filter(|event| event.state != RemoteCaptureStateV1::GarbageCollectionEligible)
        {
            replacement.extend(self.encode(&DurableSpoolRecordV1::Capture {
                frame: event.frame.clone(),
            })?);
            if event.state != RemoteCaptureStateV1::Captured {
                let transition = RemoteCaptureTransitionV1 {
                    event_id: event.frame.event_id.clone(),
                    from: RemoteCaptureStateV1::Captured,
                    to: event.state,
                    replay_attempt: event.replay_attempt,
                    observed_at: event.frame.captured_at,
                    finding: event.findings.last().copied(),
                    receipt: event.receipt.clone(),
                };
                // Compaction never weakens the state machine. Multi-step states
                // are retained by writing the original history below instead.
                if transition.validate().is_ok() {
                    replacement
                        .extend(self.encode(&DurableSpoolRecordV1::Transition { transition })?);
                } else {
                    return self.compact_from_history(removed);
                }
            }
        }
        atomic_write(
            &self.path,
            "remote-spool-gc",
            &replacement,
            DirectorySyncPolicy::Strict,
        )
        .map_err(RemoteSpoolError::Io)?;
        Ok(removed)
    }

    fn compact_from_history(&self, removed: usize) -> Result<usize, RemoteSpoolError> {
        let bytes = read_bounded(&self.path, self.config.maximum_file_bytes as usize)
            .map_err(RemoteSpoolError::Io)?
            .ok_or(RemoteSpoolError::Corruption)?;
        let records = decode_record_list(
            &bytes,
            self.config.maximum_record_bytes,
            self.encryption.as_ref(),
        )?;
        let snapshot = fold_records(records.clone())?;
        let retained = snapshot
            .events
            .iter()
            .filter_map(|(id, event)| {
                (event.state != RemoteCaptureStateV1::GarbageCollectionEligible).then_some(id)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut replacement = Vec::new();
        for record in records {
            let id = match &record {
                DurableSpoolRecordV1::Capture { frame } => &frame.event_id,
                DurableSpoolRecordV1::Transition { transition } => &transition.event_id,
            };
            if retained.contains(id) {
                replacement.extend(self.encode(&record)?);
            }
        }
        atomic_write(
            &self.path,
            "remote-spool-gc",
            &replacement,
            DirectorySyncPolicy::Strict,
        )
        .map_err(RemoteSpoolError::Io)?;
        Ok(removed)
    }

    fn append(&self, record: &DurableSpoolRecordV1) -> Result<(), RemoteSpoolError> {
        let encoded = self.encode(record)?;
        let current = file_len(&self.path).map_err(RemoteSpoolError::Io)?;
        if current.saturating_add(encoded.len() as u64) > self.config.maximum_file_bytes {
            return Err(RemoteSpoolError::Overflow);
        }
        append_durable(&self.path, &encoded, DirectorySyncPolicy::Strict)
            .map(|_| ())
            .map_err(RemoteSpoolError::Io)
    }

    fn encode(&self, record: &DurableSpoolRecordV1) -> Result<Vec<u8>, RemoteSpoolError> {
        let plaintext = serde_json::to_vec(record).map_err(|_| RemoteSpoolError::Encoding)?;
        let ciphertext = self
            .encryption
            .seal(&plaintext)
            .map_err(RemoteSpoolError::Encryption)?;
        if ciphertext.is_empty() || ciphertext.len() > self.config.maximum_record_bytes {
            return Err(RemoteSpoolError::RecordTooLarge);
        }
        let mut encoded = Vec::with_capacity(FRAME_HEADER_BYTES + ciphertext.len());
        encoded.extend_from_slice(FRAME_MAGIC);
        encoded.extend_from_slice(&FRAME_VERSION.to_be_bytes());
        encoded.extend_from_slice(&(ciphertext.len() as u64).to_be_bytes());
        encoded.extend_from_slice(&Sha256::digest(&ciphertext));
        encoded.extend_from_slice(&ciphertext);
        Ok(encoded)
    }
}

fn decode_records(
    bytes: &[u8],
    maximum_record_bytes: usize,
    encryption: &dyn RemoteSpoolEncryption,
) -> Result<RemoteSpoolSnapshotV1, RemoteSpoolError> {
    fold_records(decode_record_list(bytes, maximum_record_bytes, encryption)?)
}

fn decode_record_list(
    bytes: &[u8],
    maximum_record_bytes: usize,
    encryption: &dyn RemoteSpoolEncryption,
) -> Result<Vec<DurableSpoolRecordV1>, RemoteSpoolError> {
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(FRAME_HEADER_BYTES)
            .ok_or(RemoteSpoolError::Corruption)?;
        let header = bytes
            .get(offset..header_end)
            .ok_or(RemoteSpoolError::Corruption)?;
        if &header[..8] != FRAME_MAGIC {
            return Err(RemoteSpoolError::Corruption);
        }
        let version = u16::from_be_bytes([header[8], header[9]]);
        if version != FRAME_VERSION {
            return Err(RemoteSpoolError::UnsupportedVersion);
        }
        let length = u64::from_be_bytes(
            header[10..18]
                .try_into()
                .map_err(|_| RemoteSpoolError::Corruption)?,
        );
        let length = usize::try_from(length).map_err(|_| RemoteSpoolError::RecordTooLarge)?;
        if length == 0 || length > maximum_record_bytes {
            return Err(RemoteSpoolError::RecordTooLarge);
        }
        let end = header_end
            .checked_add(length)
            .ok_or(RemoteSpoolError::Corruption)?;
        let ciphertext = bytes
            .get(header_end..end)
            .ok_or(RemoteSpoolError::Corruption)?;
        if Sha256::digest(ciphertext).as_slice() != &header[18..50] {
            return Err(RemoteSpoolError::Corruption);
        }
        let plaintext = encryption
            .open(ciphertext)
            .map_err(RemoteSpoolError::Encryption)?;
        let record =
            serde_json::from_slice(&plaintext).map_err(|_| RemoteSpoolError::Corruption)?;
        records.push(record);
        offset = end;
    }
    Ok(records)
}

fn fold_records(
    records: Vec<DurableSpoolRecordV1>,
) -> Result<RemoteSpoolSnapshotV1, RemoteSpoolError> {
    let mut snapshot = RemoteSpoolSnapshotV1::default();
    for record in records {
        match record {
            DurableSpoolRecordV1::Capture { frame } => {
                frame.validate().map_err(RemoteSpoolError::Contract)?;
                if snapshot.events.contains_key(&frame.event_id) {
                    return Err(RemoteSpoolError::Corruption);
                }
                validate_next_sequence(&snapshot, &frame)?;
                snapshot.events.insert(
                    frame.event_id.clone(),
                    RemoteSpoolEventV1 {
                        frame,
                        state: RemoteCaptureStateV1::Captured,
                        replay_attempt: 0,
                        receipt: None,
                        findings: Vec::new(),
                    },
                );
            }
            DurableSpoolRecordV1::Transition { transition } => {
                transition.validate().map_err(RemoteSpoolError::Contract)?;
                let event = snapshot
                    .events
                    .get_mut(&transition.event_id)
                    .ok_or(RemoteSpoolError::Corruption)?;
                if event.state != transition.from {
                    return Err(RemoteSpoolError::Corruption);
                }
                if let Some(receipt) = &transition.receipt {
                    validate_receipt(&event.frame, receipt)?;
                }
                event.state = transition.to;
                event.replay_attempt = event.replay_attempt.max(transition.replay_attempt);
                event.receipt = transition.receipt;
                if let Some(finding) = transition.finding {
                    event.findings.push(finding);
                }
            }
        }
    }
    Ok(snapshot)
}

fn validate_next_sequence(
    snapshot: &RemoteSpoolSnapshotV1,
    frame: &RemoteCaptureFrameV1,
) -> Result<(), RemoteSpoolError> {
    let previous = snapshot
        .events
        .values()
        .filter(|event| event.frame.enrollment == frame.enrollment)
        .max_by_key(|event| event.frame.sequence.sequence);
    match previous {
        None if frame.sequence.sequence == 1 && frame.sequence.previous_event_id.is_none() => {
            Ok(())
        }
        Some(previous)
            if frame.sequence.sequence == previous.frame.sequence.sequence.saturating_add(1)
                && frame.sequence.previous_event_id.as_ref() == Some(&previous.frame.event_id) =>
        {
            Ok(())
        }
        _ => Err(RemoteSpoolError::SequenceGap),
    }
}

fn validate_receipt(
    frame: &RemoteCaptureFrameV1,
    receipt: &StoreCommitReceiptV1,
) -> Result<(), RemoteSpoolError> {
    receipt
        .validate()
        .map_err(|_| RemoteSpoolError::ReceiptMismatch)?;
    if receipt.idempotency.key.as_str() != frame.event_id.as_str()
        || receipt.shard_id != frame.captured_writer.runtime.shard_id
    {
        return Err(RemoteSpoolError::ReceiptMismatch);
    }
    Ok(())
}

impl RemoteCaptureSpoolPortV1 for RemoteCaptureSpool {
    fn capture(
        &self,
        frame: RemoteCaptureFrameV1,
    ) -> Result<ApplicationCaptureOutcome, RemoteCapturePersistenceErrorV1> {
        let event_id = frame.event_id.clone();
        match RemoteCaptureSpool::capture(self, frame).map_err(map_spool_persistence_error)? {
            RemoteSpoolCaptureOutcome::Captured => Ok(ApplicationCaptureOutcome::Captured),
            RemoteSpoolCaptureOutcome::AlreadyCaptured => {
                let state = self
                    .snapshot()
                    .map_err(map_spool_persistence_error)?
                    .events
                    .get(&event_id)
                    .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?
                    .state;
                Ok(ApplicationCaptureOutcome::AlreadyCaptured { state })
            }
        }
    }

    fn transition(
        &self,
        transition: RemoteCaptureTransitionV1,
    ) -> Result<(), RemoteCapturePersistenceErrorV1> {
        RemoteCaptureSpool::transition(self, transition).map_err(map_spool_persistence_error)
    }
}

impl RemoteReplaySpoolPortV1 for RemoteCaptureSpool {
    fn state(
        &self,
        event_id: &RemoteCaptureEventIdV1,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1> {
        let snapshot = self.snapshot().map_err(map_spool_persistence_error)?;
        let event = snapshot
            .events
            .get(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        Ok(RemoteReplaySpoolStateV1 {
            state: event.state,
            receipt: event.receipt.clone(),
        })
    }

    fn transition(
        &self,
        transition: RemoteCaptureTransitionV1,
    ) -> Result<(), RemoteCapturePersistenceErrorV1> {
        RemoteCaptureSpool::transition(self, transition).map_err(map_spool_persistence_error)
    }
}

fn map_spool_persistence_error(error: RemoteSpoolError) -> RemoteCapturePersistenceErrorV1 {
    match error {
        RemoteSpoolError::AtRestEncryptionUnavailable => {
            RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable
        }
        RemoteSpoolError::Overflow | RemoteSpoolError::RecordTooLarge => {
            RemoteCapturePersistenceErrorV1::Overflow
        }
        RemoteSpoolError::SequenceGap => RemoteCapturePersistenceErrorV1::SequenceGap,
        RemoteSpoolError::Corruption
        | RemoteSpoolError::UnsupportedVersion
        | RemoteSpoolError::EventIdentityConflict
        | RemoteSpoolError::ReceiptMismatch
        | RemoteSpoolError::Contract(_) => RemoteCapturePersistenceErrorV1::Corruption,
        RemoteSpoolError::InvalidConfig
        | RemoteSpoolError::PathNotAbsolute
        | RemoteSpoolError::Encryption(_)
        | RemoteSpoolError::Io(_)
        | RemoteSpoolError::Encoding
        | RemoteSpoolError::UnknownEvent
        | RemoteSpoolError::StaleTransition => RemoteCapturePersistenceErrorV1::Unavailable,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReplayStorageOutcomeV1 {
    Admitted(StoreCommitReceiptV1),
    Duplicate(StoreCommitReceiptV1),
}

/// Apply one authorized remote replay through the same native operation and
/// runtime ledger transaction used by the production writer.
///
/// Authentication, scope authorization, current placement selection, and
/// revocation checks belong to the application owner and must complete before
/// this function is called.
pub fn commit_remote_replay_transaction<E>(
    connection: &mut Connection,
    frame: &RemoteCaptureFrameV1,
    current_writer: &RemoteWriterBindingV1,
    request: &RuntimeSubmitRequestV1,
    executor: &mut E,
) -> Result<RemoteReplayStorageOutcomeV1, RemoteReplayStorageErrorV1>
where
    E: StorageOperationExecutor,
{
    frame
        .validate()
        .map_err(|_| RemoteReplayStorageErrorV1::InvalidFrame)?;
    current_writer
        .validate()
        .map_err(|_| RemoteReplayStorageErrorV1::FenceMismatch)?;
    validate_replay_request(frame, current_writer, request)?;
    let binding = request.binding();
    let mut transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
    ledger::initialize_schema(&transaction).map_err(|_| RemoteReplayStorageErrorV1::Ledger)?;
    if let Some(receipt) = ledger::lookup_receipt(
        &transaction,
        binding,
        &request.envelope().metadata.idempotency,
    )
    .map_err(|_| RemoteReplayStorageErrorV1::Ledger)?
    {
        transaction
            .commit()
            .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
        return Ok(RemoteReplayStorageOutcomeV1::Duplicate(receipt));
    }
    let transaction_outcome = {
        let mut savepoint = transaction
            .savepoint()
            .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
        operation::execute(&savepoint, request, executor)
            .map_err(|_| RemoteReplayStorageErrorV1::CanonicalEffect)?;
        let disposition = ledger::record_runtime_commit(
            &savepoint,
            &request.envelope().metadata,
            request.transaction_scope(),
            &request.envelope().payload,
        )
        .map_err(|_| RemoteReplayStorageErrorV1::Ledger)?;
        match disposition {
            LedgerDisposition::Committed(receipt) => {
                savepoint
                    .commit()
                    .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
                RemoteReplayStorageOutcomeV1::Admitted(receipt)
            }
            LedgerDisposition::Replay(receipt) => {
                savepoint
                    .rollback()
                    .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
                RemoteReplayStorageOutcomeV1::Duplicate(receipt)
            }
            LedgerDisposition::Conflict(_) => {
                return Err(RemoteReplayStorageErrorV1::IdempotencyConflict);
            }
            LedgerDisposition::New => return Err(RemoteReplayStorageErrorV1::Ledger),
        }
    };
    transaction
        .commit()
        .map_err(|_| RemoteReplayStorageErrorV1::Transaction)?;
    Ok(transaction_outcome)
}

/// Concrete application replay port over an already-opened authority
/// connection. It never resolves or opens a client-provided database path.
pub struct RusqliteRemoteReplayPort<E> {
    state: Mutex<(Connection, E)>,
}

impl<E> RusqliteRemoteReplayPort<E> {
    pub fn new(connection: Connection, executor: E) -> Self {
        Self {
            state: Mutex::new((connection, executor)),
        }
    }
}

impl<E> RemoteReplayTransactionPortV1 for RusqliteRemoteReplayPort<E>
where
    E: StorageOperationExecutor + Send,
{
    fn commit(
        &self,
        frame: &RemoteCaptureFrameV1,
        current_writer: &RemoteWriterBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        let (connection, executor) = &mut *state;
        match commit_remote_replay_transaction(
            connection,
            frame,
            current_writer,
            request,
            executor,
        )
        .map_err(map_replay_transaction_error)?
        {
            RemoteReplayStorageOutcomeV1::Admitted(receipt) => {
                Ok(RemoteReplayTransactionOutcomeV1::Admitted(receipt))
            }
            RemoteReplayStorageOutcomeV1::Duplicate(receipt) => {
                Ok(RemoteReplayTransactionOutcomeV1::Duplicate(receipt))
            }
        }
    }
}

fn map_replay_transaction_error(
    error: RemoteReplayStorageErrorV1,
) -> RemoteReplayTransactionErrorV1 {
    match error {
        RemoteReplayStorageErrorV1::FenceMismatch
        | RemoteReplayStorageErrorV1::InvalidFrame
        | RemoteReplayStorageErrorV1::PayloadMismatch
        | RemoteReplayStorageErrorV1::UnsupportedPayload => {
            RemoteReplayTransactionErrorV1::FenceMismatch
        }
        RemoteReplayStorageErrorV1::IdempotencyConflict => {
            RemoteReplayTransactionErrorV1::IdempotencyConflict
        }
        RemoteReplayStorageErrorV1::CanonicalEffect => {
            RemoteReplayTransactionErrorV1::CanonicalEffect
        }
        RemoteReplayStorageErrorV1::Ledger | RemoteReplayStorageErrorV1::Transaction => {
            RemoteReplayTransactionErrorV1::Unavailable
        }
    }
}

fn validate_replay_request(
    frame: &RemoteCaptureFrameV1,
    current_writer: &RemoteWriterBindingV1,
    request: &RuntimeSubmitRequestV1,
) -> Result<(), RemoteReplayStorageErrorV1> {
    if request.binding() != &current_writer.runtime
        || frame.captured_writer.runtime.shard_id != current_writer.runtime.shard_id
        || request.envelope().metadata.idempotency.key.as_str() != frame.event_id.as_str()
    {
        return Err(RemoteReplayStorageErrorV1::FenceMismatch);
    }
    match &request.envelope().payload {
        RepositoryWritePayloadV1::Observation(write)
            if write.observation() == &frame.observation =>
        {
            Ok(())
        }
        RepositoryWritePayloadV1::Observation(_) => {
            Err(RemoteReplayStorageErrorV1::PayloadMismatch)
        }
        _ => Err(RemoteReplayStorageErrorV1::UnsupportedPayload),
    }
}

#[derive(Debug)]
pub enum RemoteSpoolError {
    InvalidConfig,
    PathNotAbsolute,
    AtRestEncryptionUnavailable,
    Encryption(RemoteSpoolEncryptionError),
    Io(std::io::Error),
    Encoding,
    Overflow,
    RecordTooLarge,
    UnsupportedVersion,
    Corruption,
    SequenceGap,
    EventIdentityConflict,
    UnknownEvent,
    StaleTransition,
    ReceiptMismatch,
    Contract(tracedecay_store::RemoteCaptureContractErrorV1),
}

impl fmt::Display for RemoteSpoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => formatter.write_str("remote spool configuration is invalid"),
            Self::PathNotAbsolute => formatter.write_str("remote spool path must be absolute"),
            Self::AtRestEncryptionUnavailable => {
                formatter.write_str("remote spool at-rest encryption is unavailable")
            }
            Self::Encryption(error) => {
                write!(
                    formatter,
                    "remote spool encryption failed: {}",
                    error.operation
                )
            }
            Self::Io(error) => write!(formatter, "remote spool I/O failed: {error}"),
            Self::Encoding => formatter.write_str("remote spool record encoding failed"),
            Self::Overflow => formatter.write_str("remote spool capacity is exhausted"),
            Self::RecordTooLarge => formatter.write_str("remote spool record is too large"),
            Self::UnsupportedVersion => {
                formatter.write_str("remote spool frame version is unsupported")
            }
            Self::Corruption => formatter.write_str("remote spool is corrupt"),
            Self::SequenceGap => formatter.write_str("remote spool sequence has a gap"),
            Self::EventIdentityConflict => {
                formatter.write_str("remote spool event identity conflicts with durable content")
            }
            Self::UnknownEvent => formatter.write_str("remote spool event is unknown"),
            Self::StaleTransition => formatter.write_str("remote spool transition is stale"),
            Self::ReceiptMismatch => formatter.write_str("remote spool receipt is mismatched"),
            Self::Contract(error) => write!(formatter, "remote spool contract failed: {error}"),
        }
    }
}

impl std::error::Error for RemoteSpoolError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteReplayStorageErrorV1 {
    InvalidFrame,
    FenceMismatch,
    PayloadMismatch,
    UnsupportedPayload,
    IdempotencyConflict,
    CanonicalEffect,
    Ledger,
    Transaction,
}

impl fmt::Display for RemoteReplayStorageErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remote replay storage failed: {self:?}")
    }
}

impl std::error::Error for RemoteReplayStorageErrorV1 {}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnavailableEncryption;

    impl RemoteSpoolEncryption for UnavailableEncryption {
        fn is_available(&self) -> bool {
            false
        }

        fn seal(&self, _plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            Err(RemoteSpoolEncryptionError { operation: "seal" })
        }

        fn open(&self, _ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            Err(RemoteSpoolEncryptionError { operation: "open" })
        }
    }

    #[test]
    fn durable_spool_fails_closed_without_real_encryption_authority() {
        let path = std::env::temp_dir().join("tracedecay-remote-spool-unavailable");
        let result = RemoteCaptureSpool::open(
            path,
            RemoteSpoolConfig {
                maximum_file_bytes: 4096,
                maximum_record_bytes: 2048,
                maximum_events: 4,
            },
            Box::new(UnavailableEncryption),
        );
        assert!(matches!(
            result,
            Err(RemoteSpoolError::AtRestEncryptionUnavailable)
        ));
    }

    #[test]
    fn invalid_capacity_is_rejected_before_file_access() {
        let result = RemoteSpoolConfig {
            maximum_file_bytes: 1,
            maximum_record_bytes: 2,
            maximum_events: 1,
        }
        .validate();
        assert!(matches!(result, Err(RemoteSpoolError::InvalidConfig)));
    }
}
