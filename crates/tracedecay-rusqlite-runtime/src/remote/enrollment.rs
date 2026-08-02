use super::*;

pub(super) fn load_authority_state(
    handle: &MigrationSqlHandle,
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
        Some(MigrationSqlValue::Text(value)) => value,
        _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
    };
    let binding: StoreRuntimeBindingV1 =
        serde_json::from_str(binding_json).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    if &binding != handle.binding() {
        return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
    }
    let authority_json = match row.values.first() {
        Some(MigrationSqlValue::Text(value)) => value,
        _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
    };
    serde_json::from_str(authority_json).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

pub(super) fn load_enrollment(
    handle: &MigrationSqlHandle,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
    let rows = query(handle, sql, params).map_err(map_enrollment_error)?;
    let row = enrollment_one_row(rows, RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
    serde_json::from_str(enrollment_row_text(&row, 0)?)
        .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
}

pub(super) fn enrollment_one_row(
    rows: MigrationSqlRows,
    missing: RemoteEnrollmentAuthorityErrorV1,
) -> Result<crate::migration_sql::MigrationSqlRow, RemoteEnrollmentAuthorityErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        (None, None) => Err(missing),
        _ => Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict),
    }
}

pub(super) fn enrollment_row_text(
    row: &crate::migration_sql::MigrationSqlRow,
    index: usize,
) -> Result<&str, RemoteEnrollmentAuthorityErrorV1> {
    match row.values.get(index) {
        Some(MigrationSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict),
    }
}

pub(super) fn map_enrollment_error(
    error: RemoteSqliteStorageErrorV1,
) -> RemoteEnrollmentAuthorityErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::Corruption | RemoteSqliteStorageErrorV1::BindingMismatch => {
            RemoteEnrollmentAuthorityErrorV1::IdentityConflict
        }
        _ => RemoteEnrollmentAuthorityErrorV1::Unavailable,
    }
}
