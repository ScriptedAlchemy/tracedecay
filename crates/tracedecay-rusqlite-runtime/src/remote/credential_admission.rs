use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1, RemoteCredentialLookupErrorV1,
    RemoteCredentialLookupPortV1,
};
use tracedecay_domain::RemoteCredentialFingerprintV1;

use super::*;

impl RemoteCredentialLookupPortV1 for RemoteSqliteStorageV1 {
    fn credential_by_fingerprint(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1> {
        fingerprint
            .validate()
            .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
        match class {
            RemoteCredentialClassV1::EnrollmentGrant => {
                let rows = query(
                    &self.handle,
                    "SELECT grant_json, admission_json, consumed_at
                     FROM remote_enrollment_grants
                     WHERE credential_fingerprint = ?1",
                    vec![text(fingerprint.digest().as_str())],
                )
                .map_err(map_lookup_error)?;
                let row = credential_one_row(rows)?;
                if !matches!(row.values.get(2), Some(ExactSqlValue::Null)) {
                    return Err(RemoteCredentialLookupErrorV1::NotFound);
                }
                let grant = serde_json::from_str(credential_text(&row, 0)?)
                    .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
                let admission = serde_json::from_str(credential_text(&row, 1)?)
                    .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
                Ok(RemoteCredentialAuthorityRecordV1::Grant { grant, admission })
            }
            RemoteCredentialClassV1::Enrollment => {
                let rows = query(
                    &self.handle,
                    "SELECT enrollment_json, commit_receipt_json
                     FROM remote_enrollments
                     WHERE credential_fingerprint = ?1",
                    vec![text(fingerprint.digest().as_str())],
                )
                .map_err(map_lookup_error)?;
                let row = credential_one_row(rows)?;
                let enrollment = serde_json::from_str(credential_text(&row, 0)?)
                    .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
                let receipt = serde_json::from_str(credential_text(&row, 1)?)
                    .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
                Ok(RemoteCredentialAuthorityRecordV1::Enrollment {
                    enrollment,
                    receipt,
                })
            }
        }
    }
}

fn credential_one_row(
    rows: ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteCredentialLookupErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        (None, None) => Err(RemoteCredentialLookupErrorV1::NotFound),
        _ => Err(RemoteCredentialLookupErrorV1::Corruption),
    }
}

fn credential_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteCredentialLookupErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteCredentialLookupErrorV1::Corruption),
    }
}

fn map_lookup_error(error: RemoteSqliteStorageErrorV1) -> RemoteCredentialLookupErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::ResetRequired => RemoteCredentialLookupErrorV1::ResetRequired,
        RemoteSqliteStorageErrorV1::Corruption => RemoteCredentialLookupErrorV1::Corruption,
        RemoteSqliteStorageErrorV1::InvalidKeyRevision
        | RemoteSqliteStorageErrorV1::InvalidKeyLength
        | RemoteSqliteStorageErrorV1::BindingMismatch
        | RemoteSqliteStorageErrorV1::Conflict
        | RemoteSqliteStorageErrorV1::Unavailable
        | RemoteSqliteStorageErrorV1::Sql(_) => RemoteCredentialLookupErrorV1::Unavailable,
    }
}
