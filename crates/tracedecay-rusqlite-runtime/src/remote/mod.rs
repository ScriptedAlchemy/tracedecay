//! Registered SQLite authority for Remote Brain state and encrypted capture spool.
//!
//! Schema admission belongs to the registered runtime. This adapter accepts only a handle bound
//! to a Remote-node shard and never probes or mutates schema.

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
    transfer::{
        RemoteFrameTransferDispositionV1, RemoteFrameTransferErrorV1, RemoteFrameTransferPortV1,
        RemoteFrameTransferReceiptV1, RemoteFrameTransferRequestV1,
    },
};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1,
    EnrollmentGrantV1, EntityId, ManifestDigest, UtcMicros, canonical_json_bytes, canonical_sha256,
};
use tracedecay_store::StoreRuntimeBindingV1;

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlValue,
};
use crate::repository::RetainedExactSqlCapability;
use tracedecay_application::{
    OperationBudgetUsage,
    remote::replay::{
        RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1, RemoteReplayStateV1,
        RemoteReplayTransitionReceiptV1, RemoteReplayTransitionV1,
    },
};

const READ_WAIT: Duration = Duration::from_secs(5);
mod credential_admission;
mod crypto;
mod enrollment;
mod enrollment_lifecycle;
mod identity;
mod policy;
mod promotion_gate;
mod recovery_authority;
mod replay_authority;
mod replay_recovery;
mod rows;
mod schema;
mod spool_limits;
mod status;

pub use credential_admission::{RemoteCredentialInventoryErrorV1, RemoteCredentialRegistrationV1};
pub use crypto::{CredentialDerivedSpoolKeyringV1, RemoteSpoolKeyV1, RemoteSpoolKeyringV1};
use enrollment::{
    enrollment_one_row, enrollment_row_text, load_authority_state, load_enrollment,
    map_enrollment_error,
};
use identity::{bind_node_identity, provision_node_identity};
use promotion_gate::{promotion_pending, promotion_pending_in};
pub use recovery_authority::{
    RemoteRecoveryPhysicalCommitV1, RemoteRecoveryPhysicalEffectErrorV1,
    RemoteRecoveryPhysicalEffectsV1, RemoteRecoverySqliteAuthorityV1,
};
pub use replay_authority::RemoteQueryAuthoritySnapshotV1;
pub use replay_recovery::RemoteReplayStartupRecoveryV1;
use rows::*;
pub use schema::REMOTE_NODE_LOCAL_SCHEMA;
pub use status::{RemoteRecoveryOperationalSnapshotV1, RemoteStorageStatusSnapshotV1};

#[derive(Debug, Error)]
pub enum RemoteSqliteStorageErrorV1 {
    #[error("remote Brain encryption key revision must be non-zero")]
    InvalidKeyRevision,
    #[error("remote Brain encryption key must contain exactly 32 bytes")]
    InvalidKeyLength,
    #[error("remote Brain store binding does not match the registered runtime")]
    BindingMismatch,
    #[error("remote Brain store compare-and-swap precondition did not match")]
    Conflict,
    #[error("remote Brain store does not have the exact final persisted shape and requires reset")]
    ResetRequired,
    #[error("remote Brain storage is corrupt")]
    Corruption,
    #[error("remote Brain storage is unavailable")]
    Unavailable,
    #[error(transparent)]
    Sql(#[from] ExactSqlError),
}

impl From<RemoteCapturePersistenceErrorV1> for RemoteSqliteStorageErrorV1 {
    fn from(error: RemoteCapturePersistenceErrorV1) -> Self {
        match error {
            RemoteCapturePersistenceErrorV1::Corruption
            | RemoteCapturePersistenceErrorV1::SequenceGap => Self::Corruption,
            RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable
            | RemoteCapturePersistenceErrorV1::Overflow
            | RemoteCapturePersistenceErrorV1::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSpoolLimitsV1 {
    pub maximum_events: u64,
    pub maximum_ciphertext_bytes: u64,
}

impl RemoteSpoolLimitsV1 {
    pub fn new(
        maximum_events: u64,
        maximum_ciphertext_bytes: u64,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        if maximum_events == 0 || maximum_ciphertext_bytes == 0 {
            return Err(RemoteSqliteStorageErrorV1::ResetRequired);
        }
        Ok(Self {
            maximum_events,
            maximum_ciphertext_bytes,
        })
    }
}

impl Default for RemoteSpoolLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_events: 4_096,
            maximum_ciphertext_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone)]
pub struct RemoteSqliteStorageV1 {
    retained: RetainedExactSqlCapability,
    binding: StoreRuntimeBindingV1,
    keyring: Arc<dyn RemoteSpoolKeyringV1>,
    limits: RemoteSpoolLimitsV1,
}

impl RemoteSqliteStorageV1 {
    /// Attaches remote-node storage to one retained, write-authorized runtime.
    ///
    /// The sealed capability keeps the issuing client token alive and never
    /// exposes its exact SQL handle to a remote-storage caller.
    pub fn from_retained_exact_sql(
        retained: RetainedExactSqlCapability,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        Self::from_retained_exact_sql_with_limits(retained, keyring, RemoteSpoolLimitsV1::default())
    }

    pub fn from_retained_exact_sql_with_limits(
        retained: RetainedExactSqlCapability,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
        limits: RemoteSpoolLimitsV1,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        let binding = retained.handle().binding().clone();
        if !matches!(
            binding.shard_id.scope,
            tracedecay_store::StoreShardScopeV1::RemoteNode { .. }
        ) {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        validate_final_schema(retained.handle())?;
        bind_node_identity(retained.handle(), &binding)?;
        Ok(Self {
            retained,
            binding,
            keyring,
            limits,
        })
    }

    /// The same registered storage bound to a request-scoped keyring, used to
    /// serve spool encryption under the presented enrollment credential.
    #[must_use]
    pub fn with_keyring(&self, keyring: Arc<dyn RemoteSpoolKeyringV1>) -> Self {
        Self {
            retained: self.retained.clone(),
            binding: self.binding.clone(),
            keyring,
            limits: self.limits,
        }
    }

    /// Attaches a newly mounted remote-node runtime and seeds its singleton
    /// node identity after final-schema admission.
    pub fn provision_retained_exact_sql(
        retained: RetainedExactSqlCapability,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        let binding = retained.handle().binding().clone();
        if !matches!(
            binding.shard_id.scope,
            tracedecay_store::StoreShardScopeV1::RemoteNode { .. }
        ) {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        validate_final_schema(retained.handle())?;
        provision_node_identity(retained.handle(), &binding)?;
        Ok(Self {
            retained,
            binding,
            keyring,
            limits: RemoteSpoolLimitsV1::default(),
        })
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn handle(&self) -> &ExactSqlHandle {
        self.retained.handle()
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
        self.handle().execute(ExactSqlStatement::new(
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
        let result = self.handle().execute(ExactSqlStatement::new(
            "INSERT INTO remote_enrollment_grants (
                grant_id, credential_fingerprint, grant_json, admission_json, consumed_at
             ) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(grant_id) DO NOTHING"
                .to_owned(),
            vec![
                text(grant.grant_id.as_str()),
                text(grant.fingerprint.digest().as_str()),
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
    fn load_grant(
        &self,
        grant_id: &EntityId,
    ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = query(
            self.handle(),
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
            self.handle(),
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
            .handle()
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
            self.handle(),
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
            self.handle(),
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
            self.handle(),
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
        if promotion_pending(self.handle(), &writer.authority.fence)
            .map_err(map_persistence_error)?
        {
            return Err(RemoteCapturePersistenceErrorV1::Unavailable);
        }
        let rows = query(
            self.handle(),
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
            .handle()
            .begin_immediate()
            .map_err(map_persistence_error)?;
        if promotion_pending_in(&transaction, &command.writer.authority.fence)
            .map_err(map_persistence_error)?
        {
            return Err(RemoteCapturePersistenceErrorV1::Unavailable);
        }
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
        spool_limits::enforce(&transaction, self.limits, encrypted.ciphertext.len())?;
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

impl RemoteSqliteStorageV1 {
    /// Exports one locally encrypted frame for an authenticated reconnect
    /// upload. The receiving node still decrypts and validates it with the
    /// presented enrollment credential before it can enter that node's spool.
    pub fn export_frame_transfer(
        &self,
        event_id: &str,
        expires_at_micros: i64,
    ) -> Result<RemoteFrameTransferRequestV1, RemoteSqliteStorageErrorV1> {
        let frame = self
            .load_replay_frame(event_id)
            .map_err(RemoteSqliteStorageErrorV1::from)?;
        let rows = query(
            self.handle(),
            "SELECT key_revision, nonce, ciphertext, frame_digest, state
             FROM remote_spool_frames WHERE event_id = ?1",
            vec![text(event_id)],
        )?;
        let row = one_row(rows)?;
        let key_revision = row_u64(&row, 0).map_err(RemoteSqliteStorageErrorV1::from)?;
        let nonce = row_blob(&row, 1).map_err(RemoteSqliteStorageErrorV1::from)?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let ciphertext = row_blob(&row, 2)
            .map_err(RemoteSqliteStorageErrorV1::from)?
            .to_vec();
        let frame_digest = ManifestDigest::new(
            row_text(&row, 3)
                .map_err(RemoteSqliteStorageErrorV1::from)?
                .to_owned(),
        )
        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        if row_text(&row, 4).map_err(RemoteSqliteStorageErrorV1::from)? != "pending" {
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        let observed_authority_epoch = frame.capture.writer.authority.fence.authority_epoch.0;
        Ok(RemoteFrameTransferRequestV1 {
            event_id: event_id.to_owned(),
            enrollment_id: frame.capture.enrollment_id,
            enrollment_revision: frame.capture.enrollment_revision,
            node_id: frame.capture.node_id,
            writer: frame.capture.writer,
            policy_revision: frame.capture.policy_revision,
            sequence: frame.capture.sequence,
            frame_digest,
            key_revision,
            nonce,
            ciphertext,
            observed_authority_epoch,
            expires_at_micros,
        })
    }

    fn transfer_pending_frame(
        &self,
        request: &RemoteFrameTransferRequestV1,
    ) -> Result<RemoteFrameTransferReceiptV1, RemoteFrameTransferErrorV1> {
        let capture = self
            .decrypt_frame(
                &request.event_id,
                request.key_revision,
                request.nonce,
                request.ciphertext.clone(),
            )
            .map_err(map_transfer_persistence_error)?;
        let digest =
            canonical_sha256(&capture).map_err(|_| RemoteFrameTransferErrorV1::Corruption)?;
        let canonical_event = canonical_remote_event_id_v1(&capture)
            .map_err(|_| RemoteFrameTransferErrorV1::Corruption)?;
        if canonical_event != request.event_id
            || digest != request.frame_digest
            || capture.enrollment_id != request.enrollment_id
            || capture.enrollment_revision != request.enrollment_revision
            || capture.node_id != request.node_id
            || capture.writer != request.writer
            || capture.policy_revision != request.policy_revision
            || capture.sequence != request.sequence
        {
            return Err(RemoteFrameTransferErrorV1::InvalidFrame);
        }
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
        let existing = transaction
            .query(
                statement(
                    "SELECT event_id, frame_digest FROM remote_spool_frames
                     WHERE enrollment_id = ?1 AND sequence = ?2",
                    vec![
                        text(request.enrollment_id.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(request.sequence.sequence)
                                .map_err(|_| RemoteFrameTransferErrorV1::Corruption)?,
                        ),
                    ],
                )
                .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
        if let Some(row) = existing.rows.first() {
            let event_id = row_text(row, 0).map_err(map_transfer_persistence_error)?;
            let frame_digest = row_text(row, 1).map_err(map_transfer_persistence_error)?;
            if event_id != request.event_id || frame_digest != request.frame_digest.as_str() {
                transaction
                    .rollback()
                    .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
                return Err(RemoteFrameTransferErrorV1::Corruption);
            }
            transaction
                .commit()
                .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
            return Ok(RemoteFrameTransferReceiptV1 {
                event_id: request.event_id.clone(),
                sequence: request.sequence.sequence,
                disposition: RemoteFrameTransferDispositionV1::AlreadyTransferred,
            });
        }
        validate_previous_frame(&transaction, &capture).map_err(|error| match error {
            RemoteCapturePersistenceErrorV1::SequenceGap => RemoteFrameTransferErrorV1::SequenceGap,
            _ => RemoteFrameTransferErrorV1::Corruption,
        })?;
        spool_limits::enforce(&transaction, self.limits, request.ciphertext.len())
            .map_err(map_transfer_persistence_error)?;
        transaction
            .execute(
                statement(
                    "INSERT INTO remote_spool_frames (
                        event_id, enrollment_id, sequence, previous_event_id, frame_digest,
                        key_revision, nonce, ciphertext, state, captured_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                    vec![
                        text(&request.event_id),
                        text(request.enrollment_id.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(request.sequence.sequence)
                                .map_err(|_| RemoteFrameTransferErrorV1::Corruption)?,
                        ),
                        optional_text(request.sequence.previous_event_id.as_deref()),
                        text(request.frame_digest.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(request.key_revision)
                                .map_err(|_| RemoteFrameTransferErrorV1::Corruption)?,
                        ),
                        ExactSqlValue::Blob(request.nonce.to_vec()),
                        ExactSqlValue::Blob(request.ciphertext.clone()),
                        ExactSqlValue::Integer(capture.captured_at.0),
                    ],
                )
                .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| RemoteFrameTransferErrorV1::Unavailable)?;
        Ok(RemoteFrameTransferReceiptV1 {
            event_id: request.event_id.clone(),
            sequence: request.sequence.sequence,
            disposition: RemoteFrameTransferDispositionV1::TransferredPending,
        })
    }
}

impl RemoteFrameTransferPortV1 for RemoteSqliteStorageV1 {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
        <Self as RemoteCapturePortV1>::current_writer_authority(self, writer)
    }

    fn transfer_pending(
        &self,
        request: &RemoteFrameTransferRequestV1,
    ) -> Result<RemoteFrameTransferReceiptV1, RemoteFrameTransferErrorV1> {
        self.transfer_pending_frame(request)
    }
}

fn map_transfer_persistence_error(
    error: RemoteCapturePersistenceErrorV1,
) -> RemoteFrameTransferErrorV1 {
    match error {
        RemoteCapturePersistenceErrorV1::SequenceGap => RemoteFrameTransferErrorV1::SequenceGap,
        RemoteCapturePersistenceErrorV1::Corruption => RemoteFrameTransferErrorV1::Corruption,
        RemoteCapturePersistenceErrorV1::Overflow => RemoteFrameTransferErrorV1::Overflow,
        _ => RemoteFrameTransferErrorV1::Unavailable,
    }
}

fn validate_final_schema(handle: &ExactSqlHandle) -> Result<(), RemoteSqliteStorageErrorV1> {
    let rows = query(
        handle,
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
        Vec::new(),
    )
    .map_err(|_| RemoteSqliteStorageErrorV1::ResetRequired)?;
    let names = rows
        .rows
        .iter()
        .map(|row| match row.values.as_slice() {
            [ExactSqlValue::Text(name)] => Ok(name.as_str()),
            _ => Err(RemoteSqliteStorageErrorV1::ResetRequired),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names != schema::REMOTE_NODE_LOCAL_TABLES {
        return Err(RemoteSqliteStorageErrorV1::ResetRequired);
    }
    let columns = query(
        handle,
        "SELECT tables.name, columns.name
         FROM sqlite_master AS tables
         JOIN pragma_table_info(tables.name) AS columns
         WHERE tables.type = 'table' AND tables.name NOT LIKE 'sqlite_%'
         ORDER BY tables.name, columns.cid",
        Vec::new(),
    )
    .map_err(|_| RemoteSqliteStorageErrorV1::ResetRequired)?;
    let columns = columns
        .rows
        .iter()
        .map(|row| match row.values.as_slice() {
            [ExactSqlValue::Text(table), ExactSqlValue::Text(column)] => {
                Ok((table.as_str(), column.as_str()))
            }
            _ => Err(RemoteSqliteStorageErrorV1::ResetRequired),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if columns != schema::REMOTE_NODE_LOCAL_COLUMNS {
        return Err(RemoteSqliteStorageErrorV1::ResetRequired);
    }
    let marker = query(
        handle,
        "SELECT contract_id FROM remote_store_contract WHERE singleton = 1",
        Vec::new(),
    )
    .map_err(|_| RemoteSqliteStorageErrorV1::ResetRequired)?;
    match marker.rows.as_slice() {
        [row]
            if matches!(
                row.values.as_slice(),
                [ExactSqlValue::Text(contract)]
                    if contract == "tracedecay.remote-node.final-v2"
            ) =>
        {
            Ok(())
        }
        _ => Err(RemoteSqliteStorageErrorV1::ResetRequired),
    }
}

impl RemoteReplayFrameLookupPortV1 for RemoteSqliteStorageV1 {
    fn load_replay_frame(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplayFrameV1, RemoteCapturePersistenceErrorV1> {
        let rows = query(
            self.handle(),
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
            self.handle(),
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
            .handle()
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
            .handle()
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
            .handle()
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

#[cfg(test)]
mod tests;
