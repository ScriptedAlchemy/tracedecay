use thiserror::Error;
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1, RemoteCredentialLookupErrorV1,
    RemoteCredentialLookupPortV1,
};
use tracedecay_domain::{
    BrainId, BrainNodeId, EnrollmentCredentialRecordV1, EnrollmentGrantV1,
    RemoteCredentialFingerprintV1,
};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCredentialRegistrationV1 {
    pub class: RemoteCredentialClassV1,
    pub fingerprint: RemoteCredentialFingerprintV1,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteCredentialInventoryErrorV1 {
    #[error("remote credential inventory limit must be non-zero")]
    InvalidLimit,
    #[error("remote credential inventory exceeds the bounded registry capacity")]
    CapacityExceeded,
    #[error(transparent)]
    Lookup(#[from] RemoteCredentialLookupErrorV1),
}

impl RemoteSqliteStorageV1 {
    /// Reads only credential routing identities from one already-registered
    /// node store. The extra row detects overflow without materializing or
    /// scanning an unbounded credential set.
    pub fn credential_registrations(
        &self,
        maximum: usize,
    ) -> Result<Vec<RemoteCredentialRegistrationV1>, RemoteCredentialInventoryErrorV1> {
        if maximum == 0 {
            return Err(RemoteCredentialInventoryErrorV1::InvalidLimit);
        }
        let row_limit = maximum
            .checked_add(1)
            .and_then(|limit| i64::try_from(limit).ok())
            .ok_or(RemoteCredentialInventoryErrorV1::InvalidLimit)?;
        let rows = query(
            self.handle(),
            "SELECT credential_class, credential_fingerprint, credential_json
             FROM (
                 SELECT 0 AS credential_class, credential_fingerprint,
                        grant_json AS credential_json
                 FROM remote_enrollment_grants
                 WHERE consumed_at IS NULL
                 UNION ALL
                 SELECT 1 AS credential_class, credential_fingerprint,
                        enrollment_json AS credential_json
                 FROM remote_enrollments
             )
             ORDER BY credential_class, credential_fingerprint
             LIMIT ?1",
            vec![ExactSqlValue::Integer(row_limit)],
        )
        .map_err(map_lookup_error)?;
        if rows.rows.len() > maximum {
            return Err(RemoteCredentialInventoryErrorV1::CapacityExceeded);
        }
        rows.rows.into_iter().map(decode_registration).collect()
    }
}

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
                    self.handle(),
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
                    self.handle(),
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

fn decode_registration(
    row: crate::exact_sql::ExactSqlRow,
) -> Result<RemoteCredentialRegistrationV1, RemoteCredentialInventoryErrorV1> {
    let class = match row.values.first() {
        Some(ExactSqlValue::Integer(0)) => RemoteCredentialClassV1::EnrollmentGrant,
        Some(ExactSqlValue::Integer(1)) => RemoteCredentialClassV1::Enrollment,
        _ => return Err(RemoteCredentialLookupErrorV1::Corruption.into()),
    };
    let credential_fingerprint = match row.values.get(1) {
        Some(ExactSqlValue::Text(value)) => value,
        _ => return Err(RemoteCredentialLookupErrorV1::Corruption.into()),
    };
    let encoded = match row.values.get(2) {
        Some(ExactSqlValue::Text(value)) => value,
        _ => return Err(RemoteCredentialLookupErrorV1::Corruption.into()),
    };
    let (record_fingerprint, brain_id, node_id) = match class {
        RemoteCredentialClassV1::EnrollmentGrant => {
            let grant = serde_json::from_str::<EnrollmentGrantV1>(encoded)
                .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
            grant
                .validate()
                .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
            (grant.fingerprint, grant.brain_id, grant.node_id)
        }
        RemoteCredentialClassV1::Enrollment => {
            let enrollment = serde_json::from_str::<EnrollmentCredentialRecordV1>(encoded)
                .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
            enrollment
                .validate()
                .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
            (
                enrollment.fingerprint,
                enrollment.brain_id,
                enrollment.node_id,
            )
        }
    };
    if credential_fingerprint != record_fingerprint.digest().as_str() {
        return Err(RemoteCredentialLookupErrorV1::Corruption.into());
    }
    Ok(RemoteCredentialRegistrationV1 {
        class,
        fingerprint: record_fingerprint,
        brain_id,
        node_id,
    })
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
