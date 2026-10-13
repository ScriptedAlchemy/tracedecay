//! Brain membership between an authority's hosted node store and the joined
//! copy a node keeps on its own profile.

use tracedecay_contracts::remote::auth::RemoteEnrollmentCommitReceiptV1;
use tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1;
use tracedecay_domain::RemoteAuthorityUnavailableReasonV1;

use crate::exact_sql::ExactSqlTransaction;

use super::policy::replay_scope_digest;
use super::*;

impl RemoteSqliteStorageV1 {
    /// Authority half of discovery: the current state of this store's writer
    /// and, while it is available, the writer, the policy for the enrollment's
    /// scope, and the enrollment's commit receipt.
    pub fn authority_discovery(
        &self,
        enrollment: RemoteEnrollmentCommitReceiptV1,
        observed_at: UtcMicros,
    ) -> Result<
        (
            CurrentRemoteAuthorityStateV1,
            Option<RemoteAuthorityDiscoveryV1>,
        ),
        RemoteSqliteStorageErrorV1,
    > {
        let row = match authority_row(
            self.handle(),
            "authority_state_json, writer_json, runtime_binding_json",
            &self.member_brain,
        ) {
            Ok(row) => row,
            Err(RemoteSqliteStorageErrorV1::WriterAuthorityUnpublished) => {
                let state = CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::WriterAuthorityUnpublished,
                    observed_at,
                };
                return Ok((state, None));
            }
            Err(error) => return Err(error),
        };
        if decode_column::<StoreRuntimeBindingV1>(&row, 2)? != self.binding {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        let state: CurrentRemoteAuthorityStateV1 = decode_column(&row, 0)?;
        if !matches!(state, CurrentRemoteAuthorityStateV1::Available(_)) {
            return Ok((state, None));
        }
        let writer: RemoteWriterAuthorityV1 = decode_column(&row, 1)?;
        let policy = self
            .load_replay_policy(&enrollment.enrollment.scope)
            .map_err(|_| RemoteSqliteStorageErrorV1::Unavailable)?;
        Ok((
            state,
            Some(RemoteAuthorityDiscoveryV1 {
                writer,
                policy,
                enrollment,
            }),
        ))
    }

    /// Node half of discovery. In one transaction it adopts the authority's
    /// enrollment record and commit receipt verbatim, records the writer as
    /// available, and keeps the newest policy. A different record for the same
    /// enrollment, or a writer older than the one already observed, is a
    /// `Conflict`: the node never rolls its authority's lineage back.
    pub fn record_authority_discovery(
        &self,
        discovery: &RemoteAuthorityDiscoveryV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        let record = &discovery.enrollment.enrollment;
        if record.brain_id != self.member_brain
            || discovery.writer.authority.fence.brain_id != self.member_brain
        {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        let transaction = self.handle().begin_immediate()?;
        adopt_enrollment(&transaction, &discovery.enrollment)?;
        record_available_writer(&transaction, &self.binding, &discovery.writer, observed_at)?;
        adopt_policy(&transaction, &discovery.policy)?;
        transaction.commit()?;
        Ok(())
    }
}

fn adopt_enrollment(
    transaction: &ExactSqlTransaction,
    receipt: &RemoteEnrollmentCommitReceiptV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let record = &receipt.enrollment;
    let enrollment_json = encode(record)?;
    let receipt_json = encode(receipt)?;
    let existing = transaction.query(ExactSqlStatement::new(
        "SELECT enrollment_json, commit_receipt_json FROM remote_enrollments
         WHERE enrollment_id = ?1"
            .to_owned(),
        vec![text(record.enrollment_id.as_str())],
    )?)?;
    if let Some(row) = existing.rows.first() {
        return if decode_column::<EnrollmentCredentialRecordV1>(row, 0)? == *record
            && decode_column::<RemoteEnrollmentCommitReceiptV1>(row, 1)? == *receipt
        {
            Ok(())
        } else {
            Err(RemoteSqliteStorageErrorV1::Conflict)
        };
    }
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO remote_enrollments (
            enrollment_id, brain_id, node_id, revision, credential_fingerprint,
            enrollment_json, commit_receipt_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            .to_owned(),
        vec![
            text(record.enrollment_id.as_str()),
            text(record.brain_id.as_str()),
            text(record.node_id.as_str()),
            integer(record.revision)?,
            text(record.fingerprint.digest().as_str()),
            text(&enrollment_json),
            text(&receipt_json),
        ],
    )?)?;
    Ok(())
}

fn record_available_writer(
    transaction: &ExactSqlTransaction,
    binding: &StoreRuntimeBindingV1,
    writer: &RemoteWriterAuthorityV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let existing = transaction.query(ExactSqlStatement::new(
        "SELECT writer_json FROM remote_authorities WHERE brain_id = ?1".to_owned(),
        vec![text(writer.authority.fence.brain_id.as_str())],
    )?)?;
    if let Some(row) = existing.rows.first() {
        let stored: RemoteWriterAuthorityV1 = decode_column(row, 0)?;
        let current = &writer.authority.fence;
        if stored.authority.fence != *current && !current.fences(&stored.authority.fence) {
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
    }
    let state = CurrentRemoteAuthorityStateV1::Available(writer.authority.clone());
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO remote_authorities (
            brain_id, runtime_binding_json, authority_state_json, writer_json, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(brain_id) DO UPDATE SET
            runtime_binding_json = excluded.runtime_binding_json,
            authority_state_json = excluded.authority_state_json,
            writer_json = excluded.writer_json,
            updated_at = excluded.updated_at"
            .to_owned(),
        vec![
            text(writer.authority.fence.brain_id.as_str()),
            text(&encode(binding)?),
            text(&encode(&state)?),
            text(&encode(writer)?),
            ExactSqlValue::Integer(observed_at.0),
        ],
    )?)?;
    Ok(())
}

fn adopt_policy(
    transaction: &ExactSqlTransaction,
    policy: &tracedecay_contracts::remote::replay::RemoteReplayPolicyEvidenceV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let scope_digest = replay_scope_digest(&policy.repository_scope)
        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO remote_replay_policies (scope_digest, policy_revision, evidence_json)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(scope_digest) DO UPDATE SET
            policy_revision = excluded.policy_revision,
            evidence_json = excluded.evidence_json
         WHERE excluded.policy_revision >= remote_replay_policies.policy_revision"
            .to_owned(),
        vec![
            text(scope_digest.as_str()),
            integer(policy.policy_revision)?,
            text(&encode(policy)?),
        ],
    )?)?;
    Ok(())
}

fn decode_column<T: serde::de::DeserializeOwned>(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<T, RemoteSqliteStorageErrorV1> {
    let encoded = crate::exact_sql::text_at(&row.values, index)
        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    serde_json::from_str(encoded).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, RemoteSqliteStorageErrorV1> {
    serde_json::to_string(value).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

fn integer(value: u64) -> Result<ExactSqlValue, RemoteSqliteStorageErrorV1> {
    i64::try_from(value)
        .map(ExactSqlValue::Integer)
        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}
