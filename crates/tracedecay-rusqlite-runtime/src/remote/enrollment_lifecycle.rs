use tracedecay_domain::{
    CredentialRevocationReceiptV1, CredentialRotationReceiptV1, EnrollmentCredentialRecordV1,
};

use super::*;

impl RemoteSqliteStorageV1 {
    pub fn rotate_enrollment(
        &self,
        expected: &EnrollmentCredentialRecordV1,
        replacement: &EnrollmentCredentialRecordV1,
        receipt: &CredentialRotationReceiptV1,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        expected
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        replacement
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        if receipt.enrollment_id != expected.enrollment_id
            || receipt.node_id != expected.node_id
            || receipt.prior_revision != expected.revision
            || receipt.current_revision != replacement.revision
            || receipt.rotated_at != replacement.issued_at
            || receipt.expires_at != replacement.expires_at
            || replacement.revision != expected.revision.checked_add(1).unwrap_or(0)
            || replacement.enrollment_id != expected.enrollment_id
            || replacement.brain_id != expected.brain_id
            || replacement.node_id != expected.node_id
            || replacement.scope != expected.scope
            || replacement.capabilities != expected.capabilities
            || replacement.revoked_at.is_some()
        {
            return Err(RemoteSqliteStorageErrorV1::Corruption);
        }
        replace_enrollment(self, expected, replacement)
    }

    pub fn revoke_enrollment(
        &self,
        expected: &EnrollmentCredentialRecordV1,
        replacement: &EnrollmentCredentialRecordV1,
        receipt: &CredentialRevocationReceiptV1,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        expected
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        replacement
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let idempotent = expected == replacement
            && expected.revoked_at == Some(receipt.revoked_at)
            && receipt.current_revision == expected.revision
            && receipt.prior_revision == expected.revision.saturating_sub(1);
        let newly_revoked = replacement.revision == expected.revision.checked_add(1).unwrap_or(0)
            && expected.revoked_at.is_none()
            && replacement.revoked_at == Some(receipt.revoked_at)
            && receipt.prior_revision == expected.revision
            && receipt.current_revision == replacement.revision;
        if receipt.enrollment_id != expected.enrollment_id
            || receipt.node_id != expected.node_id
            || replacement.enrollment_id != expected.enrollment_id
            || replacement.brain_id != expected.brain_id
            || replacement.node_id != expected.node_id
            || replacement.fingerprint != expected.fingerprint
            || replacement.issued_at != expected.issued_at
            || replacement.expires_at != expected.expires_at
            || replacement.scope != expected.scope
            || replacement.capabilities != expected.capabilities
            || (!idempotent && !newly_revoked)
        {
            return Err(RemoteSqliteStorageErrorV1::Corruption);
        }
        replace_enrollment(self, expected, replacement)
    }
}

fn replace_enrollment(
    storage: &RemoteSqliteStorageV1,
    expected: &EnrollmentCredentialRecordV1,
    replacement: &EnrollmentCredentialRecordV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let expected_json =
        serde_json::to_string(expected).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    let replacement_json =
        serde_json::to_string(replacement).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    let result = storage.handle().execute(ExactSqlStatement::new(
        "UPDATE remote_enrollments
         SET revision = ?1, credential_fingerprint = ?2, enrollment_json = ?3
         WHERE enrollment_id = ?4 AND revision = ?5 AND enrollment_json = ?6"
            .to_owned(),
        vec![
            ExactSqlValue::Integer(
                i64::try_from(replacement.revision)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            ),
            text(replacement.fingerprint.digest().as_str()),
            text(&replacement_json),
            text(expected.enrollment_id.as_str()),
            ExactSqlValue::Integer(
                i64::try_from(expected.revision)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            ),
            text(&expected_json),
        ],
    )?)?;
    if result.changed_rows != 1 {
        return Err(RemoteSqliteStorageErrorV1::Conflict);
    }
    Ok(())
}
