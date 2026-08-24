use super::*;
use tracedecay_application::remote::auth::{
    RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
};
use tracedecay_domain::EnrollmentCredentialStateV1;

pub(super) fn load_authority_state(
    handle: &ExactSqlHandle,
    brain_id: &BrainId,
) -> Result<CurrentRemoteAuthorityStateV1, RemoteSqliteStorageErrorV1> {
    let rows = query(
        handle,
        "SELECT authority_state_json, runtime_binding_json
         FROM remote_authorities WHERE brain_id = ?1",
        vec![text(brain_id.as_str())],
    )?;
    let row = one_row(rows)?;
    let binding_json = match row.values.get(1) {
        Some(ExactSqlValue::Text(value)) => value,
        _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
    };
    let binding: StoreRuntimeBindingV1 =
        serde_json::from_str(binding_json).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    if &binding != handle.binding() {
        return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
    }
    let authority_json = match row.values.first() {
        Some(ExactSqlValue::Text(value)) => value,
        _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
    };
    serde_json::from_str(authority_json).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

pub(super) fn load_enrollment(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
    let rows = query(handle, sql, params).map_err(map_enrollment_error)?;
    let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
    serde_json::from_str(enrollment_row_text(&row, 0)?)
        .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
}

pub(super) fn enrollment_one_row(
    rows: ExactSqlRows,
    missing: RemoteEnrollmentAuthorityErrorV1,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteEnrollmentAuthorityErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        (None, None) => Err(missing),
        _ => Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict),
    }
}

pub(super) fn enrollment_row_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteEnrollmentAuthorityErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict),
    }
}

pub(super) fn map_enrollment_error(
    error: RemoteSqliteStorageErrorV1,
) -> RemoteEnrollmentAuthorityErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::Corruption
        | RemoteSqliteStorageErrorV1::BindingMismatch
        | RemoteSqliteStorageErrorV1::Conflict => {
            RemoteEnrollmentAuthorityErrorV1::IdentityConflict
        }
        _ => RemoteEnrollmentAuthorityErrorV1::Unavailable,
    }
}

impl RemoteAuthorityAuthenticationPort for RemoteSqliteStorageV1 {
    fn authenticate_connected_authority(
        &self,
        expected_authority: &tracedecay_domain::CurrentRemoteAuthorityV1,
        expected_credential: &EnrollmentCredentialRecordV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteAuthenticationError> {
        let persisted = self
            .enrollment_by_id(&expected_credential.enrollment_id)
            .map_err(|_| RemoteAuthenticationError::AuthorityAuthenticationFailed)?;
        if persisted != *expected_credential
            || persisted.brain_id != expected_authority.fence.brain_id
            || persisted.node_id != expected_authority.fence.authority_node_id
            || persisted.revision != expected_authority.credential_revision
            || persisted.state_at(observed_at) != EnrollmentCredentialStateV1::Active
        {
            return Err(RemoteAuthenticationError::InvalidAuthorityCredential);
        }
        Ok(())
    }
}
