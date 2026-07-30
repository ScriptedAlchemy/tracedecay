//! Encrypted local persistence for admitted remote observations.
//!
//! The application owns capture identity and admission. This adapter persists
//! those canonical contracts directly; it does not define a second frame DTO.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ring::rand::SecureRandom;
use ring::{aead, rand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    DirectorySyncPolicy, OperationBudgetUsage, append_durable, file_len, read_bounded,
    remote::capture::{
        AdmittedRemoteCaptureV1, RemoteCaptureDispositionV1, RemoteCapturePersistenceErrorV1,
        RemoteCapturePortV1, RemoteCaptureReceiptV1, RemoteCaptureSequenceV1,
        RemoteWriterAuthorityV1,
    },
    remote::replay::{
        RemoteReplayCommitReceiptV1, RemoteReplayFrameLookupPortV1, RemoteReplayFrameV1,
        RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1, RemoteReplayStateV1,
        RemoteReplayTransitionReceiptV1, RemoteReplayTransitionV1,
    },
};
use tracedecay_domain::{
    BrainNodeId, CurrentRemoteAuthorityStateV1, DurableObservationV1, EntityId, ManifestDigest,
    UtcMicros, canonical_sha256,
};

const FRAME_MAGIC: &[u8; 8] = b"TDRSPL02";
const FRAME_VERSION: u16 = 2;
const FRAME_HEADER_BYTES: usize = 8 + 2 + 8 + 32;
const ENCRYPTION_VERSION: u8 = 1;
const AES_GCM_NONCE_BYTES: usize = 12;
const REMOTE_SPOOL_AAD: &[u8] = b"tracedecay.remote-spool.aes-256-gcm.v1";

pub trait RemoteSpoolEncryption: Send + Sync {
    fn is_available(&self) -> bool;
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
    fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError>;
}

pub struct Aes256GcmRemoteSpoolEncryption {
    key: aead::LessSafeKey,
    random: rand::SystemRandom,
}

impl Aes256GcmRemoteSpoolEncryption {
    pub fn new(mut key_bytes: [u8; 32]) -> Result<Self, RemoteSpoolEncryptionError> {
        if key_bytes == [0; 32] {
            return Err(RemoteSpoolEncryptionError { operation: "key" });
        }
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes)
            .map(aead::LessSafeKey::new)
            .map_err(|_| RemoteSpoolEncryptionError { operation: "key" });
        key_bytes.fill(0);
        black_box(&key_bytes);
        Ok(Self {
            key: key?,
            random: rand::SystemRandom::new(),
        })
    }
}

impl std::fmt::Debug for Aes256GcmRemoteSpoolEncryption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Aes256GcmRemoteSpoolEncryption([REDACTED])")
    }
}

impl RemoteSpoolEncryption for Aes256GcmRemoteSpoolEncryption {
    fn is_available(&self) -> bool {
        true
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
        let mut nonce_bytes = [0_u8; AES_GCM_NONCE_BYTES];
        self.random
            .fill(&mut nonce_bytes)
            .map_err(|_| RemoteSpoolEncryptionError { operation: "seal" })?;
        let mut ciphertext = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce_bytes),
                aead::Aad::from(REMOTE_SPOOL_AAD),
                &mut ciphertext,
            )
            .map_err(|_| RemoteSpoolEncryptionError { operation: "seal" })?;
        let mut sealed = Vec::with_capacity(1 + AES_GCM_NONCE_BYTES + ciphertext.len());
        sealed.push(ENCRYPTION_VERSION);
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
        if ciphertext.len() < 1 + AES_GCM_NONCE_BYTES + aead::AES_256_GCM.tag_len()
            || ciphertext[0] != ENCRYPTION_VERSION
        {
            return Err(RemoteSpoolEncryptionError { operation: "open" });
        }
        let nonce_bytes: [u8; AES_GCM_NONCE_BYTES] = ciphertext[1..1 + AES_GCM_NONCE_BYTES]
            .try_into()
            .map_err(|_| RemoteSpoolEncryptionError { operation: "open" })?;
        let mut plaintext = ciphertext[1 + AES_GCM_NONCE_BYTES..].to_vec();
        let opened = self
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce_bytes),
                aead::Aad::from(REMOTE_SPOOL_AAD),
                &mut plaintext,
            )
            .map_err(|_| RemoteSpoolEncryptionError { operation: "open" })?;
        let length = opened.len();
        plaintext.truncate(length);
        Ok(plaintext)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSpoolEncryptionError {
    pub operation: &'static str,
}

pub trait RemoteAuthorityReachabilityPortV1: Send + Sync {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1>;
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
    Pending {
        capture: PersistedRemoteCaptureV2,
    },
    Transition {
        transition: RemoteReplayTransitionV1,
        receipt: RemoteReplayTransitionReceiptV1,
    },
    ReplayAttempt {
        event_id: String,
        replay_attempt: u64,
        observed_at: UtcMicros,
    },
    ReplayAttemptAbandoned {
        event_id: String,
        replay_attempt: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistedSpoolEventV2 {
    capture: PersistedRemoteCaptureV2,
    state: RemoteReplayStateV1,
    receipt: Option<RemoteReplayCommitReceiptV1>,
    last_attempt: u64,
    active_attempt: Option<u64>,
}

pub struct RemoteCaptureSpool {
    path: PathBuf,
    config: RemoteSpoolConfig,
    encryption: Box<dyn RemoteSpoolEncryption>,
    reachability: Box<dyn RemoteAuthorityReachabilityPortV1>,
    write_guard: Mutex<()>,
    _lease: File,
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
        let mut lease_path = path.as_os_str().to_owned();
        lease_path.push(".lock");
        let lease = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(PathBuf::from(lease_path))
            .map_err(RemoteSpoolError::Io)?;
        lease
            .try_lock()
            .map_err(|_| RemoteSpoolError::Unavailable)?;
        if file_len(&path).map_err(RemoteSpoolError::Io)? > config.maximum_file_bytes {
            return Err(RemoteSpoolError::Overflow);
        }
        let spool = Self {
            path,
            config,
            encryption,
            reachability,
            write_guard: Mutex::new(()),
            _lease: lease,
        };
        spool.read_records()?;
        spool.recover_interrupted_attempts()?;
        Ok(spool)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pending(&self) -> Result<Vec<(String, AdmittedRemoteCaptureV1)>, RemoteSpoolError> {
        let mut pending = self
            .snapshot()?
            .into_values()
            .filter(|event| {
                matches!(
                    event.state,
                    RemoteReplayStateV1::Pending
                        | RemoteReplayStateV1::Admitted
                        | RemoteReplayStateV1::Duplicate
                )
            })
            .map(|event| (event.capture.event_id.clone(), event.capture.admitted()))
            .collect::<Vec<_>>();
        pending.sort_by_key(|(_, capture)| capture.sequence.sequence);
        Ok(pending)
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
            if existing.capture == capture {
                return Ok(receipt(
                    &capture,
                    RemoteCaptureDispositionV1::AlreadyPending,
                ));
            }
            return Err(RemoteSpoolError::EventIdentityConflict);
        }
        if snapshot
            .values()
            .filter(|event| event.state != RemoteReplayStateV1::GarbageCollectionEligible)
            .count()
            >= self.config.maximum_events
        {
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

    fn snapshot(&self) -> Result<BTreeMap<String, PersistedSpoolEventV2>, RemoteSpoolError> {
        let mut events = BTreeMap::new();
        for record in self.read_records()? {
            match record {
                DurableSpoolRecordV2::Pending { capture } => {
                    if capture.event_id != derive_persisted_event_id(&capture)?
                        || events
                            .insert(
                                capture.event_id.clone(),
                                PersistedSpoolEventV2 {
                                    capture,
                                    state: RemoteReplayStateV1::Pending,
                                    receipt: None,
                                    last_attempt: 0,
                                    active_attempt: None,
                                },
                            )
                            .is_some()
                    {
                        return Err(RemoteSpoolError::Corruption);
                    }
                }
                DurableSpoolRecordV2::Transition {
                    transition,
                    receipt,
                } => {
                    transition
                        .validate()
                        .map_err(|_| RemoteSpoolError::Corruption)?;
                    let event = events
                        .get_mut(&transition.event_id)
                        .ok_or(RemoteSpoolError::Corruption)?;
                    if event.state != transition.from
                        || event.last_attempt != transition.replay_attempt
                        || event.active_attempt != Some(transition.replay_attempt)
                    {
                        return Err(RemoteSpoolError::Corruption);
                    }
                    receipt
                        .validate_for(&transition)
                        .map_err(|_| RemoteSpoolError::Corruption)?;
                    let expected_pre = replay_state_digest(event)?;
                    let mut terminal = event.clone();
                    terminal.state = transition.to;
                    terminal.receipt = transition.receipt.clone();
                    let expected_terminal = replay_state_digest(&terminal)?;
                    if receipt.pre_state_digest != expected_pre
                        || receipt.terminal_state_digest != expected_terminal
                    {
                        return Err(RemoteSpoolError::Corruption);
                    }
                    event.state = transition.to;
                    event.receipt = transition.receipt;
                    if matches!(
                        event.state,
                        RemoteReplayStateV1::Acknowledged
                            | RemoteReplayStateV1::Rejected
                            | RemoteReplayStateV1::Quarantined
                            | RemoteReplayStateV1::GarbageCollectionEligible
                    ) {
                        event.active_attempt = None;
                    }
                }
                DurableSpoolRecordV2::ReplayAttempt {
                    event_id,
                    replay_attempt,
                    observed_at: _,
                } => {
                    let event = events
                        .get_mut(&event_id)
                        .ok_or(RemoteSpoolError::Corruption)?;
                    if event.active_attempt.is_some()
                        || replay_attempt == 0
                        || replay_attempt
                            != event
                                .last_attempt
                                .checked_add(1)
                                .ok_or(RemoteSpoolError::Corruption)?
                    {
                        return Err(RemoteSpoolError::Corruption);
                    }
                    event.last_attempt = replay_attempt;
                    event.active_attempt = Some(replay_attempt);
                }
                DurableSpoolRecordV2::ReplayAttemptAbandoned {
                    event_id,
                    replay_attempt,
                } => {
                    let event = events
                        .get_mut(&event_id)
                        .ok_or(RemoteSpoolError::Corruption)?;
                    if event.active_attempt != Some(replay_attempt)
                        || event.last_attempt != replay_attempt
                    {
                        return Err(RemoteSpoolError::Corruption);
                    }
                    event.active_attempt = None;
                }
            }
        }
        Ok(events)
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

    fn recover_interrupted_attempts(&self) -> Result<(), RemoteSpoolError> {
        for (event_id, event) in self.snapshot()? {
            if let Some(replay_attempt) = event.active_attempt {
                self.append(&DurableSpoolRecordV2::ReplayAttemptAbandoned {
                    event_id,
                    replay_attempt,
                })?;
            }
        }
        Ok(())
    }
}

impl RemoteReplaySpoolPortV1 for RemoteCaptureSpool {
    fn state(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1> {
        let snapshot = self.snapshot().map_err(map_persistence_error)?;
        let event = snapshot
            .get(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        Ok(RemoteReplaySpoolStateV1 {
            state: event.state,
            receipt: event.receipt.clone(),
            last_attempt: event.last_attempt,
        })
    }

    fn transition(
        &self,
        transition: RemoteReplayTransitionV1,
    ) -> Result<RemoteReplayTransitionReceiptV1, RemoteCapturePersistenceErrorV1> {
        let _guard = self
            .write_guard
            .lock()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        transition
            .validate()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let snapshot = self.snapshot().map_err(map_persistence_error)?;
        let event = snapshot
            .get(&transition.event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if event.state != transition.from
            || event.last_attempt != transition.replay_attempt
            || event.active_attempt != Some(transition.replay_attempt)
        {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        if let Some(receipt) = &transition.receipt {
            let captured_fence = &event.capture.writer.authority.fence;
            if receipt.event_id != event.capture.event_id
                || !(receipt.writer_fence == *captured_fence
                    || receipt.writer_fence.fences(captured_fence))
                || receipt.commit_sequence == 0
            {
                return Err(RemoteCapturePersistenceErrorV1::Corruption);
            }
        }
        let started = Instant::now();
        let pre_state_digest = replay_state_digest(event).map_err(map_persistence_error)?;
        let mut terminal = event.clone();
        terminal.state = transition.to;
        terminal.receipt = transition.receipt.clone();
        let terminal_state_digest =
            replay_state_digest(&terminal).map_err(map_persistence_error)?;
        let transition_bytes = serde_json::to_vec(&transition)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let receipt = RemoteReplayTransitionReceiptV1 {
            event_id: transition.event_id.clone(),
            replay_attempt: transition.replay_attempt,
            from: transition.from,
            to: transition.to,
            pre_state_digest,
            terminal_state_digest,
            committed_at: current_utc_micros()
                .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?,
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: u64::try_from(transition_bytes.len())
                    .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?,
                elapsed_micros: u64::try_from(started.elapsed().as_micros())
                    .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?,
            },
        };
        self.append(&DurableSpoolRecordV2::Transition {
            transition,
            receipt: receipt.clone(),
        })
        .map_err(map_persistence_error)?;
        Ok(receipt)
    }

    fn begin_replay_attempt(
        &self,
        event_id: &str,
        observed_at: UtcMicros,
    ) -> Result<u64, RemoteCapturePersistenceErrorV1> {
        let _guard = self
            .write_guard
            .lock()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        let snapshot = self.snapshot().map_err(map_persistence_error)?;
        let event = snapshot
            .get(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if event.active_attempt.is_some() {
            return Err(RemoteCapturePersistenceErrorV1::Unavailable);
        }
        let replay_attempt = event
            .last_attempt
            .checked_add(1)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        self.append(&DurableSpoolRecordV2::ReplayAttempt {
            event_id: event_id.to_owned(),
            replay_attempt,
            observed_at,
        })
        .map_err(map_persistence_error)?;
        Ok(replay_attempt)
    }

    fn abandon_replay_attempt(
        &self,
        event_id: &str,
        replay_attempt: u64,
    ) -> Result<(), RemoteCapturePersistenceErrorV1> {
        let _guard = self
            .write_guard
            .lock()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        let snapshot = self.snapshot().map_err(map_persistence_error)?;
        let event = snapshot
            .get(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if event.last_attempt != replay_attempt {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        if event.active_attempt.is_none() {
            return Ok(());
        }
        if event.active_attempt != Some(replay_attempt) {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        self.append(&DurableSpoolRecordV2::ReplayAttemptAbandoned {
            event_id: event_id.to_owned(),
            replay_attempt,
        })
        .map_err(map_persistence_error)
    }
}

impl RemoteReplayFrameLookupPortV1 for RemoteCaptureSpool {
    fn load_replay_frame(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplayFrameV1, RemoteCapturePersistenceErrorV1> {
        let snapshot = self.snapshot().map_err(map_persistence_error)?;
        let event = snapshot
            .get(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        let frame = RemoteReplayFrameV1 {
            event_id: event.capture.event_id.clone(),
            capture: event.capture.admitted(),
        };
        frame
            .validate()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        Ok(frame)
    }
}

impl RemoteCapturePortV1 for RemoteCaptureSpool {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
        self.reachability.current_writer_authority(writer)
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

fn replay_state_digest(event: &PersistedSpoolEventV2) -> Result<ManifestDigest, RemoteSpoolError> {
    canonical_sha256(&(
        "tracedecay.remote-replay-spool-state.v1",
        &event.capture.event_id,
        event.state,
        &event.receipt,
        event.last_attempt,
    ))
    .map_err(|_| RemoteSpoolError::Encoding)
}

fn current_utc_micros() -> Result<UtcMicros, ()> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ())?
        .as_micros();
    i64::try_from(micros).map(UtcMicros).map_err(|_| ())
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
    snapshot: &BTreeMap<String, PersistedSpoolEventV2>,
    capture: &PersistedRemoteCaptureV2,
) -> Result<(), RemoteSpoolError> {
    let previous = snapshot
        .values()
        .filter(|candidate| {
            candidate.capture.enrollment_id == capture.enrollment_id
                && candidate.capture.node_id == capture.node_id
                && candidate.capture.writer == capture.writer
        })
        .max_by_key(|candidate| candidate.capture.sequence.sequence);
    match previous {
        None if capture.sequence.sequence == 1 && capture.sequence.previous_event_id.is_none() => {
            Ok(())
        }
        Some(previous)
            if capture.sequence.sequence
                == previous.capture.sequence.sequence.saturating_add(1)
                && capture.sequence.previous_event_id.as_deref()
                    == Some(previous.capture.event_id.as_str()) =>
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
        AuthorityEpoch, BrainId, ComponentVersion, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
        ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProjectId,
        ProjectionGenerationId, ProviderId, RefId, RemoteAuthorityUnavailableReasonV1,
        RemotePlacementRevisionV1, RepositoryId, RepositoryStateSnapshotId, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId, ShardId, WorktreeId,
    };
    use tracedecay_store::{RepositoryWritePayloadV1, StoreRuntimeBindingV1};

    use super::*;
    use crate::remote_replay::{
        CanonicalRemoteReplayRequestFactoryV1, RemoteReplayRequestFactoryV1,
    };
    use tracedecay_application::remote::replay::RemoteReplayFrameV1;

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
        fn current_writer_authority(
            &self,
            _writer: &RemoteWriterAuthorityV1,
        ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
            Ok(CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                observed_at: UtcMicros(1),
            })
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
                project_id: ProjectId::new("project.remote-test").unwrap(),
                repository_id: RepositoryId::new("repository.remote-test").unwrap(),
                worktree_id: WorktreeId::new("worktree.remote-test").unwrap(),
                reference: Some(RefId::new("refs/heads/main").unwrap()),
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-test").unwrap(),
            },
            authority: tracedecay_domain::CurrentRemoteAuthorityV1 {
                fence: tracedecay_domain::RemoteWriterFenceV1 {
                    brain_id: BrainId::new("brain.remote-test").unwrap(),
                    shard_id: ShardId::new("shard.remote-test").unwrap(),
                    generation_id: ProjectionGenerationId::new("generation.remote-test").unwrap(),
                    placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
                    authority_epoch: AuthorityEpoch(1),
                    authority_node_id: BrainNodeId::new("node.remote-test").unwrap(),
                },
                credential_revision: 1,
                observed_at: UtcMicros(1),
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

    #[test]
    fn production_encryption_hides_plaintext_and_rejects_tampering() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("remote.spool");
        let spool = RemoteCaptureSpool::open(
            path.clone(),
            config(),
            Box::new(Aes256GcmRemoteSpoolEncryption::new([7; 32]).unwrap()),
            Box::new(Offline),
        )
        .unwrap();
        let capture = command(1, None);
        let first = spool.capture_pending(&capture).unwrap();
        let duplicate = spool.capture_pending(&capture).unwrap();
        assert_eq!(
            duplicate.disposition,
            RemoteCaptureDispositionV1::AlreadyPending
        );
        assert_eq!(duplicate.event_id, first.event_id);
        let mut bytes = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sanitized-1"));
        drop(spool);
        assert_eq!(
            RemoteCaptureSpool::open(
                path.clone(),
                config(),
                Box::new(Aes256GcmRemoteSpoolEncryption::new([7; 32]).unwrap()),
                Box::new(Offline),
            )
            .unwrap()
            .pending()
            .unwrap()
            .len(),
            1
        );

        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            RemoteCaptureSpool::open(
                path,
                config(),
                Box::new(Aes256GcmRemoteSpoolEncryption::new([7; 32]).unwrap()),
                Box::new(Offline),
            ),
            Err(RemoteSpoolError::Encryption(_) | RemoteSpoolError::Corruption)
        ));
    }

    #[test]
    fn canonical_replay_request_preserves_binding_observation_and_idempotency() {
        let root = TempDir::new().unwrap();
        let capture = command(1, None);
        let receipt = spool(&root).capture_pending(&capture).unwrap();
        let frame = RemoteReplayFrameV1 {
            event_id: receipt.event_id,
            capture,
        };
        let binding: StoreRuntimeBindingV1 = serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.remote-test",
                "profile_id": "profile.remote-test",
                "scope": { "kind": "project_sessions", "project_id": "project.remote-test" }
            },
            "incarnation": 1,
            "authority_epoch": 1
        }))
        .unwrap();
        let mut factory = CanonicalRemoteReplayRequestFactoryV1;

        let first = factory.build_request(&frame, &binding).unwrap();
        let duplicate = factory.build_request(&frame, &binding).unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(first.binding(), &binding);
        assert_eq!(
            first.envelope().metadata.idempotency.key.as_str(),
            frame.event_id
        );
        assert!(matches!(
            &first.envelope().payload,
            RepositoryWritePayloadV1::Observation(write)
                if write.observation() == &frame.capture.observation
        ));
    }

    #[test]
    fn replay_transition_and_receipt_survive_reopen() {
        let root = TempDir::new().unwrap();
        let capture_spool = spool(&root);
        let capture = command(1, None);
        let capture_receipt = capture_spool.capture_pending(&capture).unwrap();
        let replay_receipt = RemoteReplayCommitReceiptV1 {
            event_id: capture_receipt.event_id.clone(),
            writer_fence: capture.writer.authority.fence.clone(),
            commit_sequence: 7,
            committed_at: UtcMicros(20),
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: 1,
                elapsed_micros: 0,
            },
        };
        assert_eq!(
            capture_spool
                .begin_replay_attempt(&capture_receipt.event_id, UtcMicros(20))
                .unwrap(),
            1
        );
        let durable_transition = RemoteReplaySpoolPortV1::transition(
            &capture_spool,
            RemoteReplayTransitionV1 {
                event_id: capture_receipt.event_id.clone(),
                from: RemoteReplayStateV1::Pending,
                to: RemoteReplayStateV1::Admitted,
                replay_attempt: 1,
                observed_at: UtcMicros(20),
                finding: None,
                receipt: Some(replay_receipt.clone()),
            },
        )
        .unwrap();
        assert_ne!(
            durable_transition.pre_state_digest,
            durable_transition.terminal_state_digest
        );
        assert!(durable_transition.budget.bytes_consumed > 0);
        assert!(durable_transition.committed_at >= UtcMicros(20));
        drop(capture_spool);

        let reopened = spool(&root);
        assert_eq!(
            reopened.state(&capture_receipt.event_id).unwrap(),
            RemoteReplaySpoolStateV1 {
                state: RemoteReplayStateV1::Admitted,
                receipt: Some(replay_receipt),
                last_attempt: 1,
            }
        );
        assert_eq!(
            reopened.pending().unwrap(),
            vec![(capture_receipt.event_id, capture)]
        );
    }

    #[test]
    fn replay_selector_hydrates_only_the_encrypted_canonical_frame() {
        let root = TempDir::new().unwrap();
        let capture_spool = spool(&root);
        let capture = command(1, None);
        let receipt = capture_spool.capture_pending(&capture).unwrap();

        let frame = capture_spool.load_replay_frame(&receipt.event_id).unwrap();
        assert_eq!(frame.event_id, receipt.event_id);
        assert_eq!(frame.capture, capture);
        assert!(
            capture_spool
                .load_replay_frame("remote.event.sha256:missing000000000000000000000000000000000000000000000000000000000")
                .is_err()
        );
    }

    #[test]
    fn replay_attempts_are_monotonic_across_reopen() {
        let root = TempDir::new().unwrap();
        let capture_spool = spool(&root);
        assert!(matches!(
            RemoteCaptureSpool::open(
                root.path().join("remote.spool"),
                config(),
                Box::new(XorEncryption(0xA5)),
                Box::new(Offline),
            ),
            Err(RemoteSpoolError::Unavailable)
        ));
        let receipt = capture_spool.capture_pending(&command(1, None)).unwrap();
        assert_eq!(
            capture_spool
                .begin_replay_attempt(&receipt.event_id, UtcMicros(20))
                .unwrap(),
            1
        );
        assert_eq!(
            capture_spool.begin_replay_attempt(&receipt.event_id, UtcMicros(21)),
            Err(RemoteCapturePersistenceErrorV1::Unavailable)
        );
        drop(capture_spool);

        let reopened = spool(&root);
        assert_eq!(reopened.state(&receipt.event_id).unwrap().last_attempt, 1);
        assert_eq!(
            reopened
                .begin_replay_attempt(&receipt.event_id, UtcMicros(21))
                .unwrap(),
            2
        );
        assert_eq!(reopened.state(&receipt.event_id).unwrap().last_attempt, 2);
    }

    #[test]
    fn abandoned_attempt_allows_monotonic_same_process_retry() {
        let root = TempDir::new().unwrap();
        let capture_spool = spool(&root);
        let receipt = capture_spool.capture_pending(&command(1, None)).unwrap();
        let first = capture_spool
            .begin_replay_attempt(&receipt.event_id, UtcMicros(20))
            .unwrap();
        capture_spool
            .abandon_replay_attempt(&receipt.event_id, first)
            .unwrap();
        assert_eq!(
            capture_spool
                .begin_replay_attempt(&receipt.event_id, UtcMicros(21))
                .unwrap(),
            2
        );
        assert_eq!(
            capture_spool.state(&receipt.event_id).unwrap().last_attempt,
            2
        );
    }

    #[test]
    fn garbage_collection_reclaims_event_capacity_after_reopen() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("remote.spool");
        let mut bounded = config();
        bounded.maximum_events = 1;
        let capture_spool = RemoteCaptureSpool::open(
            path.clone(),
            bounded.clone(),
            Box::new(XorEncryption(0xA5)),
            Box::new(Offline),
        )
        .unwrap();
        let first = command(1, None);
        let first_receipt = capture_spool.capture_pending(&first).unwrap();
        let replay_receipt = RemoteReplayCommitReceiptV1 {
            event_id: first_receipt.event_id.clone(),
            writer_fence: first.writer.authority.fence.clone(),
            commit_sequence: 7,
            committed_at: UtcMicros(20),
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: 1,
                elapsed_micros: 0,
            },
        };
        assert_eq!(
            capture_spool
                .begin_replay_attempt(&first_receipt.event_id, UtcMicros(20))
                .unwrap(),
            1
        );
        for (index, (from, to)) in [
            (RemoteReplayStateV1::Pending, RemoteReplayStateV1::Admitted),
            (
                RemoteReplayStateV1::Admitted,
                RemoteReplayStateV1::Acknowledged,
            ),
            (
                RemoteReplayStateV1::Acknowledged,
                RemoteReplayStateV1::GarbageCollectionEligible,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let replay_attempt = if index == 2 {
                capture_spool
                    .begin_replay_attempt(&first_receipt.event_id, UtcMicros(21))
                    .unwrap()
            } else {
                1
            };
            RemoteReplaySpoolPortV1::transition(
                &capture_spool,
                RemoteReplayTransitionV1 {
                    event_id: first_receipt.event_id.clone(),
                    from,
                    to,
                    replay_attempt,
                    observed_at: UtcMicros(20),
                    finding: None,
                    receipt: Some(replay_receipt.clone()),
                },
            )
            .unwrap();
        }
        drop(capture_spool);

        let reopened = RemoteCaptureSpool::open(
            path,
            bounded,
            Box::new(XorEncryption(0xA5)),
            Box::new(Offline),
        )
        .unwrap();
        assert!(reopened.pending().unwrap().is_empty());
        let second = command(2, Some(first_receipt.event_id));
        assert!(reopened.capture_pending(&second).is_ok());
        assert_eq!(reopened.pending().unwrap().len(), 1);
    }
}
