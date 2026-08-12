//! Shared row helpers, storage-error plumbing, and owner-key handling.

use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::DatabaseMemoryTransaction as Transaction;
use serde::{Serialize, de::DeserializeOwned};

use tracedecay_domain::{FactCategoryV1, FactOwnerV1, PayloadAccessState, UtcMicros};
use tracedecay_store::{FactReadControl, FactStoreError, FactStoreResult};

pub(super) const COMMIT_OPERATION: &str = "commit canonical memory fact";

pub(super) const QUERY_OPERATION: &str = "query canonical memory facts";

pub(super) const PROJECT_MEMORY_READ_OPERATION: &str = "read project memory facts";

pub(super) const PROJECT_MEMORY_WRITE_OPERATION: &str = "write project memory facts";

pub(super) fn nonnegative_u64(value: i64, field: &'static str) -> FactStoreResult<u64> {
    u64::try_from(value)
        .map_err(|_| storage_message(QUERY_OPERATION, format!("{field} must be non-negative")))
}

pub(super) fn project_memory_category_label(category: FactCategoryV1) -> &'static str {
    match category {
        FactCategoryV1::General => "general",
        FactCategoryV1::UserPref => "user_pref",
        FactCategoryV1::Project => "project",
        FactCategoryV1::Tool => "tool",
        FactCategoryV1::Decision => "decision",
        FactCategoryV1::CodeArea => "code_area",
    }
}

pub(super) fn project_memory_now() -> FactStoreResult<UtcMicros> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let micros = i64::try_from(elapsed.as_micros()).map_err(|_| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "project-memory clock exceeds supported timestamp range",
        )
    })?;
    Ok(UtcMicros(micros))
}

pub(super) fn ensure_project_memory_read_active(
    read_control: &FactReadControl,
) -> FactStoreResult<()> {
    if read_control.interrupted() {
        Err(FactStoreError::ReadCancelled)
    } else {
        Ok(())
    }
}

pub(super) fn project_memory_event_time(now: UtcMicros, offset: i64) -> FactStoreResult<UtcMicros> {
    now.0.checked_add(offset).map(UtcMicros).ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "project-memory event timestamp overflow",
        )
    })
}

#[derive(Clone)]
pub(super) struct OwnerKey {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}

impl OwnerKey {
    pub(super) fn new(owner: &FactOwnerV1) -> FactStoreResult<Self> {
        let (kind, project_id) = match owner {
            FactOwnerV1::Profile => ("profile", String::new()),
            FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
        };
        Ok(Self {
            kind,
            project_id,
            json: to_json(owner, "serialize fact owner")?,
        })
    }
}

pub(super) fn storage_error(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> FactStoreError {
    FactStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn storage_message(
    operation: &'static str,
    message: impl Into<String>,
) -> FactStoreError {
    storage_error(operation, std::io::Error::other(message.into()))
}

pub(super) fn identity_collision<T>(kind: &'static str, id: &str) -> FactStoreResult<T> {
    Err(storage_message(
        COMMIT_OPERATION,
        format!("{kind} identity collision for {id}"),
    ))
}

pub(super) fn to_json<T: Serialize + ?Sized>(
    value: &T,
    operation: &'static str,
) -> FactStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage_error(operation, error))
}

pub(super) fn from_json<T: DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> FactStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage_error(operation, error))
}

pub(super) fn row_string(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<String> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) fn row_optional_string(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<String>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) fn row_i64(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<i64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) fn row_optional_i64(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<i64>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) fn row_optional_f64(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<f64>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) fn row_f64(
    row: &crate::db::engine::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<f64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

pub(super) async fn row_exists(
    transaction: &Transaction<'_>,
    sql: &str,
    values: impl crate::db::engine::IntoParams,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(sql, values)
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .is_some())
}

pub(super) async fn row_exists_params(
    transaction: &Transaction<'_>,
    sql: &str,
    values: impl crate::db::engine::IntoParams,
) -> FactStoreResult<bool> {
    row_exists(transaction, sql, values).await
}

pub(super) fn payload_access_label(state: PayloadAccessState) -> &'static str {
    match state {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

pub(super) fn parse_payload_access(value: &str) -> FactStoreResult<PayloadAccessState> {
    match value {
        "eligible" => Ok(PayloadAccessState::Eligible),
        "redacted" => Ok(PayloadAccessState::Redacted),
        "quarantined" => Ok(PayloadAccessState::Quarantined),
        "retention_expired" => Ok(PayloadAccessState::RetentionExpired),
        "deleted" => Ok(PayloadAccessState::Deleted),
        "unavailable" => Ok(PayloadAccessState::Unavailable),
        "ambiguous" => Ok(PayloadAccessState::Ambiguous),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown payload access state {value:?}"),
        )),
    }
}

pub(super) fn requires_payload_purge(access: PayloadAccessState) -> bool {
    matches!(
        access,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    )
}
