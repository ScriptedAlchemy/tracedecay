//! Encrypted local persistence for admitted remote observations.
//!
//! The application owns capture identity and admission. This adapter persists
//! those canonical contracts directly; it does not define a second frame DTO.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    DirectorySyncPolicy, append_durable, file_len, read_bounded,
    remote::capture::{
        AdmittedRemoteCaptureV1, RemoteAuthorityReachabilityV1, RemoteCaptureDispositionV1,
        RemoteCapturePersistenceErrorV1, RemoteCapturePortV1, RemoteCaptureReceiptV1,
        RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
    },
};
use tracedecay_domain::{
    BrainNodeId, DurableObservationV1, EntityId, ManifestDigest, UtcMicros, canonical_sha256,
};

const FRAME_MAGIC: &[u8; 8] = b"TDRSPL02";
const FRAME_VERSION: u16 = 2;
const FRAME_HEADER_BYTES: usize = 8 + 2 + 8 + 32;

pub trait RemoteSpoolEncryption: Send + Sync {
    fn is_available(&self) -> bool;
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
    fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSpoolEncryptionError {
    pub operation: &'static str,
}

pub trait RemoteAuthorityReachabilityPortV1: Send + Sync {
    fn current_writer_reachability(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteAuthorityReachabilityV1, RemoteCapturePersistenceErrorV1>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSpoolConfig {
    pub maximum_file_bytes: u64,
    pub maximum_record_bytes: usize,
    pub maximum_events: usize,
}

impl RemoteSpoolConfig {
    fn validate(self) -> Result<Self, RemoteSpoolError> {
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
#[serde(deny_unknown_fields)]
struct PersistedRemoteCaptureV2 {
    event_id: String,
    enrollment_id: EntityId,
    enrollment_revision: u64,
    node_id: BrainNodeId,
    writer: RemoteWriterAuthorityV1,
    policy_revision: u64,
    sequence: RemoteCaptureSequenceV1,
    observation: DurableObservationV1,
    captured_at: UtcMicros,
}

impl PersistedRemoteCaptureV2 {
    fn from_admitted(command: &AdmittedRemoteCaptureV1) -> Result<Self, RemoteSpoolError> {
        let event_id = derive_event_id(command)?;
        Ok(Self {
            event_id,
            enrollment_id: command.enrollment_id.clone(),
            enrollment_revision: command.enrollment_revision,
            node_id: command.node_id.clone(),
            writer: command.writer.clone(),
            policy_revision: command.policy_revision,
            sequence: command.sequence.clone(),
            observation: command.observation.clone(),
            captured_at: command.captured_at,
        })
    }

    fn admitted(&self) -> AdmittedRemoteCaptureV1 {
        AdmittedRemoteCaptureV1 {
            enrollment_id: self.enrollment_id.clone(),
            enrollment_revision: self.enrollment_revision,
            node_id: self.node_id.clone(),
            writer: self.writer.clone(),
            policy_revision: self.policy_revision,
            sequence: self.sequence.clone(),
            observation: self.observation.clone(),
            captured_at: self.captured_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableSpoolRecordV2 {
    Pending { capture: PersistedRemoteCaptureV2 },
}

pub struct RemoteCaptureSpool {
    path: PathBuf,
    config: RemoteSpoolConfig,
    encryption: Box<dyn RemoteSpoolEncryption>,
    reachability: Box<dyn RemoteAuthorityReachabilityPortV1>,
    write_guard: Mutex<()>,
}

impl RemoteCaptureSpool {
    pub fn open(
        path: PathBuf,
        config: RemoteSpoolConfig,
        encryption: Box<dyn RemoteSpoolEncryption>,
        reachability: Box<dyn RemoteAuthorityReachabilityPortV1>,
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
            reachability,
            write_guard: Mutex::new(()),
        };
        spool.read_records()?;
        Ok(spool)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pending(&self) -> Result<Vec<(String, AdmittedRemoteCaptureV1)>, RemoteSpoolError> {
        Ok(self
            .snapshot()?
            .into_values()
            .map(|capture| (capture.event_id.clone(), capture.admitted()))
            .collect())
    }

    fn capture(
        &self,
        command: &AdmittedRemoteCaptureV1,
    ) -> Result<RemoteCaptureReceiptV1, RemoteSpoolError> {
        let _guard = self
            .write_guard
            .lock()
            .map_err(|_| RemoteSpoolError::Unavailable)?;
        let capture = PersistedRemoteCaptureV2::from_admitted(command)?;
        let snapshot = self.snapshot()?;
        if let Some(existing) = snapshot.get(&capture.event_id) {
            if existing == &capture {
                return Ok(receipt(
                    &capture,
                    RemoteCaptureDispositionV1::AlreadyPending,
                ));
            }
            return Err(RemoteSpoolError::EventIdentityConflict);
        }
        if snapshot.len() >= self.config.maximum_events {
            return Err(RemoteSpoolError::Overflow);
        }
        validate_next_sequence(&snapshot, &capture)?;
        self.append(&DurableSpoolRecordV2::Pending {
            capture: capture.clone(),
        })?;
        Ok(receipt(
            &capture,
            RemoteCaptureDispositionV1::CapturedPending,
        ))
    }

    fn snapshot(&self) -> Result<BTreeMap<String, PersistedRemoteCaptureV2>, RemoteSpoolError> {
        let mut captures = BTreeMap::new();
        for record in self.read_records()? {
            let DurableSpoolRecordV2::Pending { capture } = record;
            if capture.event_id != derive_persisted_event_id(&capture)?
                || captures.insert(capture.event_id.clone(), capture).is_some()
            {
                return Err(RemoteSpoolError::Corruption);
            }
        }
        Ok(captures)
    }

    fn read_records(&self) -> Result<Vec<DurableSpoolRecordV2>, RemoteSpoolError> {
        let Some(bytes) = read_bounded(&self.path, self.config.maximum_file_bytes as usize)
            .map_err(RemoteSpoolError::Io)?
        else {
            return Ok(Vec::new());
        };
        decode_records(
            &bytes,
            self.config.maximum_record_bytes,
            self.encryption.as_ref(),
        )
    }

    fn append(&self, record: &DurableSpoolRecordV2) -> Result<(), RemoteSpoolError> {
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
        let current = file_len(&self.path).map_err(RemoteSpoolError::Io)?;
        if current.saturating_add(encoded.len() as u64) > self.config.maximum_file_bytes {
            return Err(RemoteSpoolError::Overflow);
        }
        append_durable(&self.path, &encoded, DirectorySyncPolicy::Strict)
            .map(|_| ())
            .map_err(RemoteSpoolError::Io)
    }
}

impl RemoteCapturePortV1 for RemoteCaptureSpool {
    fn current_writer_reachability(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteAuthorityReachabilityV1, RemoteCapturePersistenceErrorV1> {
        self.reachability.current_writer_reachability(writer)
    }

    fn capture_pending(
        &self,
        command: &AdmittedRemoteCaptureV1,
    ) -> Result<RemoteCaptureReceiptV1, RemoteCapturePersistenceErrorV1> {
        self.capture(command).map_err(map_persistence_error)
    }
}

fn derive_event_id(command: &AdmittedRemoteCaptureV1) -> Result<String, RemoteSpoolError> {
    let digest = canonical_sha256(&(
        "tracedecay.remote-capture.v2",
        &command.enrollment_id,
        command.enrollment_revision,
        &command.node_id,
        &command.writer,
        command.policy_revision,
        &command.sequence,
        &command.observation,
        command.captured_at,
    ))
    .map_err(|_| RemoteSpoolError::Encoding)?;
    Ok(event_id(&digest))
}

fn derive_persisted_event_id(
    capture: &PersistedRemoteCaptureV2,
) -> Result<String, RemoteSpoolError> {
    let digest = canonical_sha256(&(
        "tracedecay.remote-capture.v2",
        &capture.enrollment_id,
        capture.enrollment_revision,
        &capture.node_id,
        &capture.writer,
        capture.policy_revision,
        &capture.sequence,
        &capture.observation,
        capture.captured_at,
    ))
    .map_err(|_| RemoteSpoolError::Encoding)?;
    Ok(event_id(&digest))
}

fn event_id(digest: &ManifestDigest) -> String {
    format!("remote.event.{}", digest.as_str())
}

fn receipt(
    capture: &PersistedRemoteCaptureV2,
    disposition: RemoteCaptureDispositionV1,
) -> RemoteCaptureReceiptV1 {
    RemoteCaptureReceiptV1 {
        event_id: capture.event_id.clone(),
        sequence: capture.sequence.sequence,
        disposition,
    }
}

fn validate_next_sequence(
    snapshot: &BTreeMap<String, PersistedRemoteCaptureV2>,
    capture: &PersistedRemoteCaptureV2,
) -> Result<(), RemoteSpoolError> {
    let previous = snapshot
        .values()
        .filter(|candidate| {
            candidate.enrollment_id == capture.enrollment_id
                && candidate.node_id == capture.node_id
                && candidate.writer == capture.writer
        })
        .max_by_key(|candidate| candidate.sequence.sequence);
    match previous {
        None if capture.sequence.sequence == 1 && capture.sequence.previous_event_id.is_none() => {
            Ok(())
        }
        Some(previous)
            if capture.sequence.sequence == previous.sequence.sequence.saturating_add(1)
                && capture.sequence.previous_event_id.as_deref()
                    == Some(previous.event_id.as_str()) =>
        {
            Ok(())
        }
        _ => Err(RemoteSpoolError::SequenceGap),
    }
}

fn decode_records(
    bytes: &[u8],
    maximum_record_bytes: usize,
    encryption: &dyn RemoteSpoolEncryption,
) -> Result<Vec<DurableSpoolRecordV2>, RemoteSpoolError> {
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(FRAME_HEADER_BYTES)
            .ok_or(RemoteSpoolError::Corruption)?;
        let header = bytes
            .get(offset..header_end)
            .ok_or(RemoteSpoolError::Corruption)?;
        if &header[..8] != FRAME_MAGIC
            || u16::from_be_bytes([header[8], header[9]]) != FRAME_VERSION
        {
            return Err(RemoteSpoolError::Corruption);
        }
        let length = usize::try_from(u64::from_be_bytes(
            header[10..18]
                .try_into()
                .map_err(|_| RemoteSpoolError::Corruption)?,
        ))
        .map_err(|_| RemoteSpoolError::RecordTooLarge)?;
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
        records.push(serde_json::from_slice(&plaintext).map_err(|_| RemoteSpoolError::Corruption)?);
        offset = end;
    }
    Ok(records)
}

fn map_persistence_error(error: RemoteSpoolError) -> RemoteCapturePersistenceErrorV1 {
    match error {
        RemoteSpoolError::AtRestEncryptionUnavailable => {
            RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable
        }
        RemoteSpoolError::Overflow | RemoteSpoolError::RecordTooLarge => {
            RemoteCapturePersistenceErrorV1::Overflow
        }
        RemoteSpoolError::SequenceGap => RemoteCapturePersistenceErrorV1::SequenceGap,
        RemoteSpoolError::Corruption | RemoteSpoolError::EventIdentityConflict => {
            RemoteCapturePersistenceErrorV1::Corruption
        }
        RemoteSpoolError::InvalidConfig
        | RemoteSpoolError::PathNotAbsolute
        | RemoteSpoolError::Encryption(_)
        | RemoteSpoolError::Io(_)
        | RemoteSpoolError::Encoding
        | RemoteSpoolError::Unavailable => RemoteCapturePersistenceErrorV1::Unavailable,
    }
}

#[derive(Debug)]
pub enum RemoteSpoolError {
    InvalidConfig,
    PathNotAbsolute,
    AtRestEncryptionUnavailable,
    Overflow,
    RecordTooLarge,
    SequenceGap,
    EventIdentityConflict,
    Corruption,
    Encoding,
    Unavailable,
    Encryption(RemoteSpoolEncryptionError),
    Io(std::io::Error),
}

impl std::fmt::Display for RemoteSpoolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RemoteSpoolError {}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_domain::{
        AuthorityEpoch, BrainId, ComponentVersion, EntityVersionId, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RefId, RepositoryId,
        RepositoryStateSnapshotId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, ShardId,
        WorktreeId,
    };

    use super::*;

    struct XorEncryption(u8);

    impl RemoteSpoolEncryption for XorEncryption {
        fn is_available(&self) -> bool {
            true
        }

        fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            Ok(plaintext.iter().map(|byte| byte ^ self.0).collect())
        }

        fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            self.seal(ciphertext)
        }
    }

    struct NoEncryption;

    impl RemoteSpoolEncryption for NoEncryption {
        fn is_available(&self) -> bool {
            false
        }

        fn seal(&self, _plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            unreachable!()
        }

        fn open(&self, _ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
            unreachable!()
        }
    }

    struct Offline;

    impl RemoteAuthorityReachabilityPortV1 for Offline {
        fn current_writer_reachability(
            &self,
            _writer: &RemoteWriterAuthorityV1,
        ) -> Result<RemoteAuthorityReachabilityV1, RemoteCapturePersistenceErrorV1> {
            Ok(RemoteAuthorityReachabilityV1::Unreachable)
        }
    }

    fn config() -> RemoteSpoolConfig {
        RemoteSpoolConfig {
            maximum_file_bytes: 1_000_000,
            maximum_record_bytes: 250_000,
            maximum_events: 16,
        }
    }

    fn spool(root: &TempDir) -> RemoteCaptureSpool {
        RemoteCaptureSpool::open(
            root.path().join("remote.spool"),
            config(),
            Box::new(XorEncryption(0xA5)),
            Box::new(Offline),
        )
        .unwrap()
    }

    fn writer() -> RemoteWriterAuthorityV1 {
        RemoteWriterAuthorityV1 {
            project_id: ProjectId::new("project.remote-test").unwrap(),
            scope: tracedecay_domain::RemoteRepositoryScopeV1 {
                repository_id: RepositoryId::new("repository.remote-test").unwrap(),
                worktree_id: WorktreeId::new("worktree.remote-test").unwrap(),
                reference: Some(RefId::new("refs/heads/main").unwrap()),
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-test").unwrap(),
            },
            fence: tracedecay_domain::RemoteWriterFenceV1 {
                brain_id: BrainId::new("brain.remote-test").unwrap(),
                shard_id: ShardId::new("shard.remote-test").unwrap(),
                generation_id: ProjectionGenerationId::new("generation.remote-test").unwrap(),
                placement_revision: EntityVersionId::new("placement.remote-test").unwrap(),
                authority_epoch: AuthorityEpoch(1),
                authority_node_id: BrainNodeId::new("node.remote-test").unwrap(),
            },
        }
    }

    fn observation(ordinal: u64) -> DurableObservationV1 {
        let payload = json!({"message": format!("sanitized-{ordinal}")});
        let range = ObservationSourceRangeV1::new(ordinal - 1, ordinal).unwrap();
        let record_id = ObservationId::new(format!("record.remote-test.{ordinal}")).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.remote-test.{ordinal}")).unwrap(),
                ComponentVersion::new("sanitizer.remote-test.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                ObservationSourceIdentityV1::for_provider(
                    ProviderId::new("remote-test").unwrap(),
                    SessionId::new("session.remote-test").unwrap(),
                )
                .unwrap(),
                ObservationScopeV1::Project {
                    project_id: ProjectId::new("project.remote-test").unwrap(),
                },
                ObservationSourceGenerationV1::new(1).unwrap(),
                range,
                ObservationOrderingDomainV1::SnapshotOrder,
                record_id,
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.remote-test").unwrap(),
            payload,
        )
        .unwrap()
    }

    fn command(sequence: u64, previous_event_id: Option<String>) -> AdmittedRemoteCaptureV1 {
        AdmittedRemoteCaptureV1 {
            enrollment_id: EntityId::new("enrollment.remote-test").unwrap(),
            enrollment_revision: 1,
            node_id: BrainNodeId::new("node.remote-test").unwrap(),
            writer: writer(),
            policy_revision: 1,
            sequence: RemoteCaptureSequenceV1 {
                sequence,
                previous_event_id,
            },
            observation: observation(sequence),
            captured_at: UtcMicros(i64::try_from(sequence).unwrap()),
        }
    }

    #[test]
    fn unavailable_encryption_fails_before_creating_spool() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("remote.spool");
        assert!(matches!(
            RemoteCaptureSpool::open(
                path.clone(),
                config(),
                Box::new(NoEncryption),
                Box::new(Offline)
            ),
            Err(RemoteSpoolError::AtRestEncryptionUnavailable)
        ));
        assert!(!path.exists());
    }

    #[test]
    fn canonical_capture_persists_and_replays_deduplication() {
        let root = TempDir::new().unwrap();
        let first = command(1, None);
        let first_receipt = spool(&root).capture_pending(&first).unwrap();
        assert_eq!(
            first_receipt.disposition,
            RemoteCaptureDispositionV1::CapturedPending
        );

        let reopened = spool(&root);
        let duplicate = reopened.capture_pending(&first).unwrap();
        assert_eq!(
            duplicate.disposition,
            RemoteCaptureDispositionV1::AlreadyPending
        );
        assert_eq!(duplicate.event_id, first_receipt.event_id);
        assert_eq!(
            reopened.pending().unwrap(),
            vec![(first_receipt.event_id, first)]
        );
    }

    #[test]
    fn sequence_cas_rejects_gap_and_accepts_exact_predecessor() {
        let root = TempDir::new().unwrap();
        let spool = spool(&root);
        let first_receipt = spool.capture_pending(&command(1, None)).unwrap();
        assert_eq!(
            spool.capture_pending(&command(3, Some(first_receipt.event_id.clone()))),
            Err(RemoteCapturePersistenceErrorV1::SequenceGap)
        );
        assert!(
            spool
                .capture_pending(&command(2, Some(first_receipt.event_id)))
                .is_ok()
        );
    }

    #[test]
    fn spool_contains_no_plaintext_observation() {
        let root = TempDir::new().unwrap();
        let spool = spool(&root);
        spool.capture_pending(&command(1, None)).unwrap();
        let bytes = fs::read(spool.path()).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("sanitized-1"),
            "canonical observation leaked outside encryption"
        );
    }
}
