//! Registered SQLite authority for Remote Brain state and encrypted capture spool.
//!
//! Runtime attachment is deliberately read-only with respect to schema, so callers expose a
//! typed migration-required state instead of mutating a live store.

use std::sync::Arc;
use std::time::Duration;

use ring::{
    aead::{Aad, Nonce},
    rand::{SecureRandom, SystemRandom},
};
use thiserror::Error;
use tracedecay_application::remote::{
    auth::{
        RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1,
        RemoteEnrollmentAuthorityPortV1, RemoteEnrollmentCommitReceiptV1,
        RemoteEnrollmentCredentialLookupPortV1,
    },
    capture::{
        AdmittedRemoteCaptureV1, RemoteCaptureDispositionV1, RemoteCapturePersistenceErrorV1,
        RemoteCapturePortV1, RemoteCaptureReceiptV1, RemoteWriterAuthorityV1,
    },
    replay::{RemoteReplayFrameLookupPortV1, RemoteReplayFrameV1, canonical_remote_event_id_v1},
};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1,
    EnrollmentGrantV1, EntityId, ManifestDigest, UtcMicros, canonical_json_bytes, canonical_sha256,
};
use tracedecay_store::StoreRuntimeBindingV1;

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlValue,
};
use tracedecay_application::{
    OperationBudgetUsage,
    remote::replay::{
        RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1, RemoteReplayStateV1,
        RemoteReplayTransitionReceiptV1, RemoteReplayTransitionV1,
    },
};

const READ_WAIT: Duration = Duration::from_secs(5);
mod crypto;
mod enrollment;
mod replay_authority;
mod schema;
mod status;

pub use crypto::{RemoteSpoolKeyV1, RemoteSpoolKeyringV1};
use enrollment::{
    enrollment_one_row, enrollment_row_text, load_authority_state, load_enrollment,
    map_enrollment_error,
};
pub use schema::{
    REMOTE_NODE_LOCAL_SCHEMA, REMOTE_OBSERVATION_EVENTS_SCHEMA, validate_remote_schema,
};
pub use status::RemoteStorageStatusSnapshotV1;

#[derive(Debug, Error)]
pub enum RemoteSqliteStorageErrorV1 {
    #[error("remote Brain schema migration is required")]
    MigrationRequired,
    #[error("remote Brain store schema is incompatible and requires an explicit reset")]
    ResetRequired,
    #[error("remote Brain encryption key revision must be non-zero")]
    InvalidKeyRevision,
    #[error("remote Brain encryption key must contain exactly 32 bytes")]
    InvalidKeyLength,
    #[error("remote Brain store binding does not match the registered runtime")]
    BindingMismatch,
    #[error("remote Brain storage is corrupt")]
    Corruption,
    #[error("remote Brain storage is unavailable")]
    Unavailable,
    #[error(transparent)]
    Sql(#[from] ExactSqlError),
}

#[derive(Clone)]
pub struct RemoteSqliteStorageV1 {
    handle: ExactSqlHandle,
    binding: StoreRuntimeBindingV1,
    keyring: Arc<dyn RemoteSpoolKeyringV1>,
}

impl RemoteSqliteStorageV1 {
    pub fn attach(
        handle: ExactSqlHandle,
        binding: StoreRuntimeBindingV1,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        validate_remote_schema(&handle)?;
        if handle.binding() != &binding {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        Ok(Self {
            handle,
            binding,
            keyring,
        })
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn publish_authority(
        &self,
        state: &CurrentRemoteAuthorityStateV1,
        writer: &RemoteWriterAuthorityV1,
        updated_at: UtcMicros,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        state
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        writer
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let brain_id = match state {
            CurrentRemoteAuthorityStateV1::Available(authority)
                if authority.fence == writer.authority.fence =>
            {
                authority.fence.brain_id.as_str()
            }
            _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
        };
        let runtime_binding_json = serde_json::to_string(&self.binding)
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let authority_state_json =
            serde_json::to_string(state).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let writer_json =
            serde_json::to_string(writer).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        self.handle.execute(ExactSqlStatement::new(
            "INSERT INTO remote_authorities (
                    brain_id, runtime_binding_json, authority_state_json, writer_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(brain_id) DO UPDATE SET
                    runtime_binding_json = excluded.runtime_binding_json,
                    authority_state_json = excluded.authority_state_json,
                    writer_json = excluded.writer_json,
                    updated_at = excluded.updated_at
                 WHERE excluded.updated_at >= remote_authorities.updated_at"
                .to_owned(),
            vec![
                text(brain_id),
                text(&runtime_binding_json),
                text(&authority_state_json),
                text(&writer_json),
                ExactSqlValue::Integer(updated_at.0),
            ],
        )?)?;
        Ok(())
    }

    pub fn store_enrollment_grant(
        &self,
        grant: &EnrollmentGrantV1,
        admission: &RemoteEnrollmentAdmissionEvidenceV1,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        grant
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let grant_json =
            serde_json::to_string(grant).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let admission_json =
            serde_json::to_string(admission).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let result = self.handle.execute(ExactSqlStatement::new(
            "INSERT INTO remote_enrollment_grants (
                grant_id, grant_json, admission_json, consumed_at
             ) VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(grant_id) DO NOTHING"
                .to_owned(),
            vec![
                text(grant.grant_id.as_str()),
                text(&grant_json),
                text(&admission_json),
            ],
        )?)?;
        if result.changed_rows == 1 {
            return Ok(());
        }
        let existing = self
            .load_grant(&grant.grant_id)
            .map_err(|error| match error {
                RemoteEnrollmentAuthorityErrorV1::GrantConsumed => {
                    RemoteSqliteStorageErrorV1::Corruption
                }
                _ => RemoteSqliteStorageErrorV1::Unavailable,
            })?;
        if existing == *grant {
            Ok(())
        } else {
            Err(RemoteSqliteStorageErrorV1::Corruption)
        }
    }

    fn encrypt_frame(
        &self,
        event_id: &str,
        frame: &AdmittedRemoteCaptureV1,
    ) -> Result<EncryptedFrameV1, RemoteCapturePersistenceErrorV1> {
        let key = self.keyring.active_key().map_err(map_encryption_error)?;
        let nonce_bytes = random_nonce()?;
        let mut ciphertext =
            canonical_json_bytes(frame).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        key.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(event_id.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable)?;
        Ok(EncryptedFrameV1 {
            key_revision: key.revision,
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    fn decrypt_frame(
        &self,
        event_id: &str,
        key_revision: u64,
        nonce: [u8; 12],
        mut ciphertext: Vec<u8>,
    ) -> Result<AdmittedRemoteCaptureV1, RemoteCapturePersistenceErrorV1> {
        let key = self
            .keyring
            .key(key_revision)
            .map_err(map_encryption_error)?
            .ok_or(RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable)?;
        let plaintext = key
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(event_id.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        serde_json::from_slice(plaintext).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)
    }
}

impl RemoteEnrollmentAuthorityPortV1 for RemoteSqliteStorageV1 {
    fn current_authority(
        &self,
        brain_id: &BrainId,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteEnrollmentAuthorityErrorV1> {
        load_authority_state(&self.handle, brain_id).map_err(map_enrollment_error)
    }

    fn load_grant(
        &self,
        grant_id: &EntityId,
    ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT grant_json, consumed_at
             FROM remote_enrollment_grants WHERE grant_id = ?1",
            vec![text(grant_id.as_str())],
        )
        .map_err(map_enrollment_error)?;
        let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if !matches!(row.values.get(1), Some(ExactSqlValue::Null)) {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        serde_json::from_str(enrollment_row_text(&row, 0)?)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    }

    fn load_admission_evidence(
        &self,
        grant_id: &EntityId,
    ) -> Result<RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT admission_json, consumed_at
             FROM remote_enrollment_grants WHERE grant_id = ?1",
            vec![text(grant_id.as_str())],
        )
        .map_err(map_enrollment_error)?;
        let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if !matches!(row.values.get(1), Some(ExactSqlValue::Null)) {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        serde_json::from_str(enrollment_row_text(&row, 0)?)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    }

    fn commit_enrollment(
        &self,
        grant: &EnrollmentGrantV1,
        enrollment: &EnrollmentCredentialRecordV1,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?;
        let rows = transaction
            .query(
                ExactSqlStatement::new(
                    "SELECT grant_json, admission_json, consumed_at
                     FROM remote_enrollment_grants WHERE grant_id = ?1"
                        .to_owned(),
                    vec![text(grant.grant_id.as_str())],
                )
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?;
        let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if !matches!(row.values.get(2), Some(ExactSqlValue::Null)) {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        let stored_grant: EnrollmentGrantV1 =
            serde_json::from_str(enrollment_row_text(&row, 0)?)
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        if stored_grant != *grant {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        let admission: RemoteEnrollmentAdmissionEvidenceV1 =
            serde_json::from_str(enrollment_row_text(&row, 1)?)
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let prior_grant_digest = canonical_sha256(grant)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let committed_state_digest = canonical_sha256(enrollment)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let enrollment_json = serde_json::to_string(enrollment)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let budget_bytes = enrollment_json.len();
        let receipt = RemoteEnrollmentCommitReceiptV1 {
            admission,
            prior_grant_digest,
            input_digest: input_digest.clone(),
            committed_state_digest,
            consumed_at,
            budget: OperationBudgetUsage {
                units_consumed: 2,
                bytes_consumed: u64::try_from(budget_bytes)
                    .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?,
                elapsed_micros: 0,
            },
            enrollment: enrollment.clone(),
        };
        receipt
            .validate()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        transaction
            .execute(
                ExactSqlStatement::new(
                    "INSERT INTO remote_enrollments (
                        enrollment_id, brain_id, node_id, revision, credential_fingerprint,
                        enrollment_json, commit_receipt_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                        .to_owned(),
                    vec![
                        text(enrollment.enrollment_id.as_str()),
                        text(enrollment.brain_id.as_str()),
                        text(enrollment.node_id.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(enrollment.revision)
                                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
                        ),
                        text(enrollment.fingerprint.digest().as_str()),
                        text(&enrollment_json),
                        text(&receipt_json),
                    ],
                )
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let consumed = transaction
            .execute(
                ExactSqlStatement::new(
                    "UPDATE remote_enrollment_grants SET consumed_at = ?1
                     WHERE grant_id = ?2 AND consumed_at IS NULL"
                        .to_owned(),
                    vec![
                        ExactSqlValue::Integer(consumed_at.0),
                        text(grant.grant_id.as_str()),
                    ],
                )
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?;
        if consumed.changed_rows != 1 {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        transaction
            .commit()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::Unavailable)?;
        Ok(receipt)
    }
}

impl RemoteEnrollmentCredentialLookupPortV1 for RemoteSqliteStorageV1 {
    fn enrollment_by_id(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        load_enrollment(
            &self.handle,
            "SELECT enrollment_json FROM remote_enrollments WHERE enrollment_id = ?1",
            vec![text(enrollment_id.as_str())],
        )
    }

    fn authority_enrollment(
        &self,
        brain_id: &BrainId,
        node_id: &BrainNodeId,
        revision: u64,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        let revision = i64::try_from(revision)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        load_enrollment(
            &self.handle,
            "SELECT enrollment_json FROM remote_enrollments
             WHERE brain_id = ?1 AND node_id = ?2 AND revision = ?3",
            vec![
                text(brain_id.as_str()),
                text(node_id.as_str()),
                ExactSqlValue::Integer(revision),
            ],
        )
    }

    fn enrollment_commit_receipt(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT commit_receipt_json FROM remote_enrollments WHERE enrollment_id = ?1",
            vec![text(enrollment_id.as_str())],
        )
        .map_err(map_enrollment_error)?;
        let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        serde_json::from_str(enrollment_row_text(&row, 0)?)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    }
}

impl RemoteCapturePortV1 for RemoteSqliteStorageV1 {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT authority_state_json, runtime_binding_json
             FROM remote_authorities WHERE brain_id = ?1",
            vec![text(writer.authority.fence.brain_id.as_str())],
        )
        .map_err(map_persistence_error)?;
        let row = one_row(rows).map_err(map_persistence_error)?;
        let authority_json = row_text(&row, 0).map_err(map_persistence_error)?;
        let binding_json = row_text(&row, 1).map_err(map_persistence_error)?;
        let stored_binding: StoreRuntimeBindingV1 = serde_json::from_str(binding_json)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        if stored_binding != self.binding {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        serde_json::from_str(authority_json)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)
    }

    fn capture_pending(
        &self,
        command: &AdmittedRemoteCaptureV1,
    ) -> Result<RemoteCaptureReceiptV1, RemoteCapturePersistenceErrorV1> {
        let digest =
            canonical_sha256(command).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let event_id = canonical_remote_event_id_v1(command)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let enrollment_id = command.enrollment_id.as_str();
        let sequence = i64::try_from(command.sequence.sequence)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(map_persistence_error)?;
        let existing = transaction
            .query(statement(
                "SELECT event_id, frame_digest FROM remote_spool_frames
                 WHERE enrollment_id = ?1 AND sequence = ?2",
                vec![text(enrollment_id), ExactSqlValue::Integer(sequence)],
            )?)
            .map_err(map_persistence_error)?;
        if let Some(row) = existing.rows.first() {
            let existing_event = row_text(row, 0)?;
            let existing_digest = row_text(row, 1)?;
            if existing_event != event_id || existing_digest != digest.as_str() {
                return Err(RemoteCapturePersistenceErrorV1::Corruption);
            }
            transaction.commit().map_err(map_persistence_error)?;
            return Ok(RemoteCaptureReceiptV1 {
                event_id,
                sequence: command.sequence.sequence,
                disposition: RemoteCaptureDispositionV1::AlreadyPending,
            });
        }
        validate_previous_frame(&transaction, command)?;
        let encrypted = self.encrypt_frame(&event_id, command)?;
        transaction
            .execute(statement(
                "INSERT INTO remote_spool_frames (
                    event_id, enrollment_id, sequence, previous_event_id, frame_digest,
                    key_revision, nonce, ciphertext, state, captured_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                vec![
                    text(&event_id),
                    text(enrollment_id),
                    ExactSqlValue::Integer(sequence),
                    optional_text(command.sequence.previous_event_id.as_deref()),
                    text(digest.as_str()),
                    ExactSqlValue::Integer(
                        i64::try_from(encrypted.key_revision)
                            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                    ),
                    ExactSqlValue::Blob(encrypted.nonce.to_vec()),
                    ExactSqlValue::Blob(encrypted.ciphertext),
                    ExactSqlValue::Integer(command.captured_at.0),
                ],
            )?)
            .map_err(map_persistence_error)?;
        transaction.commit().map_err(map_persistence_error)?;
        Ok(RemoteCaptureReceiptV1 {
            event_id,
            sequence: command.sequence.sequence,
            disposition: RemoteCaptureDispositionV1::CapturedPending,
        })
    }
}

impl RemoteReplayFrameLookupPortV1 for RemoteSqliteStorageV1 {
    fn load_replay_frame(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplayFrameV1, RemoteCapturePersistenceErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT key_revision, nonce, ciphertext, frame_digest
             FROM remote_spool_frames WHERE event_id = ?1",
            vec![text(event_id)],
        )
        .map_err(map_persistence_error)?;
        let row = one_row(rows).map_err(map_persistence_error)?;
        let revision = row_u64(&row, 0)?;
        let nonce = row_blob(&row, 1)?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let ciphertext = row_blob(&row, 2)?.to_vec();
        let expected_digest = row_text(&row, 3)?;
        let capture = self.decrypt_frame(event_id, revision, nonce, ciphertext)?;
        let actual_digest =
            canonical_sha256(&capture).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let canonical_event_id = canonical_remote_event_id_v1(&capture)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        if actual_digest.as_str() != expected_digest || event_id != canonical_event_id {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        Ok(RemoteReplayFrameV1 {
            event_id: event_id.to_owned(),
            capture,
        })
    }
}

impl RemoteReplaySpoolPortV1 for RemoteSqliteStorageV1 {
    fn state(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT state, receipt_json, last_attempt
             FROM remote_spool_frames WHERE event_id = ?1",
            vec![text(event_id)],
        )
        .map_err(map_persistence_error)?;
        decode_spool_state(persistence_one_row(rows)?)
    }

    fn transition(
        &self,
        transition: RemoteReplayTransitionV1,
    ) -> Result<RemoteReplayTransitionReceiptV1, RemoteCapturePersistenceErrorV1> {
        transition
            .validate()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(map_persistence_error)?;
        let rows = transaction
            .query(statement(
                "SELECT state, receipt_json, last_attempt, attempt_started_at
                 FROM remote_spool_frames WHERE event_id = ?1",
                vec![text(&transition.event_id)],
            )?)
            .map_err(map_persistence_error)?;
        let row = persistence_one_row(rows)?;
        let pre_state = decode_spool_state(row.clone())?;
        if pre_state.state != transition.from
            || pre_state.last_attempt != transition.replay_attempt
            || !matches!(row.values.get(3), Some(ExactSqlValue::Integer(_)))
        {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        let pre_state_digest = canonical_sha256(&pre_state)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let terminal_state = RemoteReplaySpoolStateV1 {
            state: transition.to,
            receipt: transition.receipt.clone(),
            last_attempt: transition.replay_attempt,
        };
        let terminal_state_digest = canonical_sha256(&terminal_state)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let receipt_json = transition
            .receipt
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let finding_json = transition
            .finding
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let terminal = matches!(
            transition.to,
            RemoteReplayStateV1::Acknowledged
                | RemoteReplayStateV1::Rejected
                | RemoteReplayStateV1::Quarantined
                | RemoteReplayStateV1::GarbageCollectionEligible
        );
        let transition_bytes = canonical_json_bytes(&transition)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?
            .len();
        let result = transaction
            .execute(statement(
                "UPDATE remote_spool_frames
                 SET state = ?1, receipt_json = ?2, finding = ?3,
                     attempt_started_at = CASE WHEN ?4 = 1 THEN NULL ELSE attempt_started_at END
                 WHERE event_id = ?5 AND state = ?6 AND last_attempt = ?7
                   AND attempt_started_at IS NOT NULL",
                vec![
                    text(replay_state_name(transition.to)),
                    optional_text(receipt_json.as_deref()),
                    optional_text(finding_json.as_deref()),
                    ExactSqlValue::Integer(i64::from(terminal)),
                    text(&transition.event_id),
                    text(replay_state_name(transition.from)),
                    ExactSqlValue::Integer(
                        i64::try_from(transition.replay_attempt)
                            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                    ),
                ],
            )?)
            .map_err(map_persistence_error)?;
        if result.changed_rows != 1 {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        transaction.commit().map_err(map_persistence_error)?;
        Ok(RemoteReplayTransitionReceiptV1 {
            event_id: transition.event_id,
            replay_attempt: transition.replay_attempt,
            from: transition.from,
            to: transition.to,
            pre_state_digest,
            terminal_state_digest,
            committed_at: transition.observed_at,
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: u64::try_from(transition_bytes)
                    .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                elapsed_micros: 0,
            },
        })
    }

    fn begin_replay_attempt(
        &self,
        event_id: &str,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Result<u64, RemoteCapturePersistenceErrorV1> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(map_persistence_error)?;
        let rows = transaction
            .query(statement(
                "SELECT last_attempt, attempt_started_at
                 FROM remote_spool_frames WHERE event_id = ?1",
                vec![text(event_id)],
            )?)
            .map_err(map_persistence_error)?;
        let row = persistence_one_row(rows)?;
        if !matches!(row.values.get(1), Some(ExactSqlValue::Null)) {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        let last_attempt = row_u64(&row, 0)?;
        let replay_attempt = last_attempt
            .checked_add(1)
            .ok_or(RemoteCapturePersistenceErrorV1::Overflow)?;
        let result = transaction
            .execute(statement(
                "UPDATE remote_spool_frames
                 SET last_attempt = ?1, attempt_started_at = ?2
                 WHERE event_id = ?3 AND last_attempt = ?4 AND attempt_started_at IS NULL",
                vec![
                    ExactSqlValue::Integer(
                        i64::try_from(replay_attempt)
                            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                    ),
                    ExactSqlValue::Integer(observed_at.0),
                    text(event_id),
                    ExactSqlValue::Integer(
                        i64::try_from(last_attempt)
                            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                    ),
                ],
            )?)
            .map_err(map_persistence_error)?;
        if result.changed_rows != 1 {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        transaction.commit().map_err(map_persistence_error)?;
        Ok(replay_attempt)
    }

    fn abandon_replay_attempt(
        &self,
        event_id: &str,
        replay_attempt: u64,
    ) -> Result<(), RemoteCapturePersistenceErrorV1> {
        let result = self
            .handle
            .execute(statement(
                "UPDATE remote_spool_frames SET attempt_started_at = NULL
                 WHERE event_id = ?1 AND last_attempt = ?2 AND attempt_started_at IS NOT NULL",
                vec![
                    text(event_id),
                    ExactSqlValue::Integer(
                        i64::try_from(replay_attempt)
                            .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?,
                    ),
                ],
            )?)
            .map_err(map_persistence_error)?;
        if result.changed_rows != 1 {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        Ok(())
    }
}

struct EncryptedFrameV1 {
    key_revision: u64,
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

fn random_nonce() -> Result<[u8; 12], RemoteCapturePersistenceErrorV1> {
    let mut nonce = [0_u8; 12];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable)?;
    Ok(nonce)
}

fn validate_previous_frame(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    command: &AdmittedRemoteCaptureV1,
) -> Result<(), RemoteCapturePersistenceErrorV1> {
    if command.sequence.sequence == 1 {
        return Ok(());
    }
    let previous_sequence = i64::try_from(command.sequence.sequence - 1)
        .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?;
    let rows = transaction
        .query(statement(
            "SELECT event_id FROM remote_spool_frames
             WHERE enrollment_id = ?1 AND sequence = ?2",
            vec![
                text(command.enrollment_id.as_str()),
                ExactSqlValue::Integer(previous_sequence),
            ],
        )?)
        .map_err(map_persistence_error)?;
    let previous = rows
        .rows
        .first()
        .ok_or(RemoteCapturePersistenceErrorV1::SequenceGap)
        .and_then(|row| row_text(row, 0))?;
    if command.sequence.previous_event_id.as_deref() != Some(previous) {
        return Err(RemoteCapturePersistenceErrorV1::SequenceGap);
    }
    Ok(())
}

fn query(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, RemoteSqliteStorageErrorV1> {
    let statement = ExactSqlStatement::new(sql.to_owned(), params)?;
    Ok(handle.query(statement, READ_WAIT)?)
}

fn statement(
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlStatement, RemoteCapturePersistenceErrorV1> {
    ExactSqlStatement::new(sql.to_owned(), params).map_err(map_persistence_error)
}

fn text(value: &str) -> ExactSqlValue {
    ExactSqlValue::Text(value.to_owned())
}

fn optional_text(value: Option<&str>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, text)
}

fn one_row(
    rows: ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteSqliteStorageErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}

fn row_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteCapturePersistenceErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

fn row_blob(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&[u8], RemoteCapturePersistenceErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Blob(value)) => Ok(value),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

fn row_u64(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<u64, RemoteCapturePersistenceErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)
        }
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

fn persistence_one_row(
    rows: ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteCapturePersistenceErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

fn decode_spool_state(
    row: crate::exact_sql::ExactSqlRow,
) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1> {
    let state = parse_replay_state(row_text(&row, 0)?)?;
    let receipt = match row.values.get(1) {
        Some(ExactSqlValue::Null) => None,
        Some(ExactSqlValue::Text(value)) => Some(
            serde_json::from_str(value).map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?,
        ),
        _ => return Err(RemoteCapturePersistenceErrorV1::Corruption),
    };
    Ok(RemoteReplaySpoolStateV1 {
        state,
        receipt,
        last_attempt: row_u64(&row, 2)?,
    })
}

const fn replay_state_name(state: RemoteReplayStateV1) -> &'static str {
    match state {
        RemoteReplayStateV1::Pending => "pending",
        RemoteReplayStateV1::Admitted => "admitted",
        RemoteReplayStateV1::Duplicate => "duplicate",
        RemoteReplayStateV1::Acknowledged => "acknowledged",
        RemoteReplayStateV1::Rejected => "rejected",
        RemoteReplayStateV1::Quarantined => "quarantined",
        RemoteReplayStateV1::GarbageCollectionEligible => "garbage_collection_eligible",
    }
}

fn parse_replay_state(state: &str) -> Result<RemoteReplayStateV1, RemoteCapturePersistenceErrorV1> {
    match state {
        "pending" => Ok(RemoteReplayStateV1::Pending),
        "admitted" => Ok(RemoteReplayStateV1::Admitted),
        "duplicate" => Ok(RemoteReplayStateV1::Duplicate),
        "acknowledged" => Ok(RemoteReplayStateV1::Acknowledged),
        "rejected" => Ok(RemoteReplayStateV1::Rejected),
        "quarantined" => Ok(RemoteReplayStateV1::Quarantined),
        "garbage_collection_eligible" => Ok(RemoteReplayStateV1::GarbageCollectionEligible),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

fn map_encryption_error(error: RemoteSqliteStorageErrorV1) -> RemoteCapturePersistenceErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::InvalidKeyLength
        | RemoteSqliteStorageErrorV1::InvalidKeyRevision => {
            RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable
        }
        RemoteSqliteStorageErrorV1::Corruption => RemoteCapturePersistenceErrorV1::Corruption,
        _ => RemoteCapturePersistenceErrorV1::Unavailable,
    }
}

fn map_persistence_error(error: impl std::fmt::Display) -> RemoteCapturePersistenceErrorV1 {
    let _ = error;
    RemoteCapturePersistenceErrorV1::Unavailable
}

#[cfg(test)]
mod tests;
