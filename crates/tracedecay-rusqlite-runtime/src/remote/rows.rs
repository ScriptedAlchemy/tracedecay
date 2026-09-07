use super::*;

pub(super) fn query(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, RemoteSqliteStorageErrorV1> {
    let statement = ExactSqlStatement::new(sql.to_owned(), params)?;
    Ok(handle.query(statement, READ_WAIT)?)
}

pub(super) fn statement(
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlStatement, RemoteCapturePersistenceErrorV1> {
    ExactSqlStatement::new(sql.to_owned(), params).map_err(map_persistence_error)
}

pub(super) fn text(value: &str) -> ExactSqlValue {
    ExactSqlValue::Text(value.to_owned())
}

pub(super) fn optional_text(value: Option<&str>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, text)
}

pub(super) fn one_row(
    rows: ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteSqliteStorageErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}

pub(super) fn row_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteCapturePersistenceErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

pub(super) fn row_blob(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&[u8], RemoteCapturePersistenceErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Blob(value)) => Ok(value),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

pub(super) fn row_u64(
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

pub(super) fn persistence_one_row(
    rows: ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteCapturePersistenceErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(RemoteCapturePersistenceErrorV1::Corruption),
    }
}

pub(super) fn decode_spool_state(
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

pub(super) const fn replay_state_name(state: RemoteReplayStateV1) -> &'static str {
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

pub(super) fn map_encryption_error(
    error: RemoteSqliteStorageErrorV1,
) -> RemoteCapturePersistenceErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::InvalidKeyLength
        | RemoteSqliteStorageErrorV1::InvalidKeyRevision => {
            RemoteCapturePersistenceErrorV1::AtRestEncryptionUnavailable
        }
        RemoteSqliteStorageErrorV1::Corruption => RemoteCapturePersistenceErrorV1::Corruption,
        _ => RemoteCapturePersistenceErrorV1::Unavailable,
    }
}

pub(super) fn map_persistence_error(
    error: impl std::fmt::Display,
) -> RemoteCapturePersistenceErrorV1 {
    let _ = error;
    RemoteCapturePersistenceErrorV1::Unavailable
}
