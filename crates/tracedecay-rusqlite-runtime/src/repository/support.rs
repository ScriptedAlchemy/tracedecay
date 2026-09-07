use std::fmt::Display;

use rusqlite::types::{ToSqlOutput, Type, Value, ValueRef};
use rusqlite::{OptionalExtension, ToSql};
use serde::{Serialize, de::DeserializeOwned};
pub(super) fn encode<T: Serialize + ?Sized>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value).map_err(|error| conversion(error.to_string()))
}

pub(super) fn decode<T: DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    serde_json::from_str(&value).map_err(|error| conversion(error.to_string()))
}

pub(super) fn canonical_digest<T: Serialize + ?Sized>(value: &T) -> rusqlite::Result<String> {
    let value = serde_json::to_value(value).map_err(|error| conversion(error.to_string()))?;
    tracedecay_domain::canonical_sha256(&value)
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| conversion(error.to_string()))
}

pub(super) fn conversion(error: impl Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, error.to_string().into())
}

pub(super) fn invalid(error: impl Display) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

pub(super) fn usize_to_i64(value: usize, field: &'static str) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| invalid(format!("{field} exceeds SQLite integer range")))
}

pub(super) fn u64_to_i64(value: u64, field: &'static str) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| invalid(format!("{field} exceeds SQLite integer range")))
}

/// One column of a row this crate writes and later proves it wrote.
///
/// The variant chooses the binding, so a caller keeps whatever storage class it
/// already used; comparison is always textual (see [`stored_row_matches`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ColumnValue {
    Text(String),
    Integer(i64),
    Null,
}

impl ColumnValue {
    /// The text a faithfully stored copy of this value projects back as.
    fn expected(&self) -> Option<String> {
        match self {
            Self::Text(value) => Some(value.clone()),
            Self::Integer(value) => Some(value.to_string()),
            Self::Null => None,
        }
    }
}

impl From<String> for ColumnValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ColumnValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<i64> for ColumnValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl<T: Into<ColumnValue>> From<Option<T>> for ColumnValue {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

impl ToSql for ColumnValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Self::Text(value) => ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes())),
            Self::Integer(value) => ToSqlOutput::Owned(Value::Integer(*value)),
            Self::Null => ToSqlOutput::Owned(Value::Null),
        })
    }
}

/// A named column plus the value a caller wrote or expects to find.
pub(super) type Column<'a> = (&'a str, ColumnValue);

fn column_names<'a>(columns: &'a [Column<'a>]) -> Vec<&'a str> {
    columns.iter().map(|(name, _)| *name).collect()
}

fn bindings<'a>(columns: &'a [Column<'a>]) -> impl Iterator<Item = &'a ColumnValue> {
    columns.iter().map(|(_, value)| value)
}

fn insert(
    connection: &rusqlite::Connection,
    conflict_clause: &str,
    table: &str,
    columns: &[Column<'_>],
) -> rusqlite::Result<usize> {
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    connection.execute(
        &format!(
            "INSERT{conflict_clause} INTO {table} ({}) VALUES ({placeholders})",
            column_names(columns).join(", ")
        ),
        rusqlite::params_from_iter(bindings(columns)),
    )
}

/// Writes a row the caller has already established is absent, described the
/// same way [`idempotent_insert`] describes one.
///
/// A constraint violation here is a real defect and surfaces as the driver's
/// error rather than being swallowed.
pub(super) fn insert_row(
    connection: &rusqlite::Connection,
    table: &str,
    columns: &[Column<'_>],
) -> rusqlite::Result<()> {
    insert(connection, "", table, columns).map(|_| ())
}

/// Reads `values`' columns from the row `keys` identifies and reports whether
/// every one of them matches what the caller expects.
///
/// `Ok(None)` means no such row exists. Each column is projected through
/// `CAST(... AS TEXT)` so a value that SQLite converted on the way in — a text
/// binding landing in an `INTEGER` column, say — still compares equal to what
/// the caller wrote, and so one comparison covers every storage class.
pub(super) fn stored_row_matches(
    connection: &rusqlite::Connection,
    table: &str,
    keys: &[Column<'_>],
    values: &[Column<'_>],
) -> rusqlite::Result<Option<bool>> {
    let projection = values
        .iter()
        .map(|(name, _)| format!("CAST({name} AS TEXT)"))
        .collect::<Vec<_>>()
        .join(", ");
    let predicate = keys
        .iter()
        .enumerate()
        .map(|(index, (name, _))| format!("{name} = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let stored = connection
        .query_row(
            &format!("SELECT {projection} FROM {table} WHERE {predicate}"),
            rusqlite::params_from_iter(bindings(keys)),
            |row| {
                (0..values.len())
                    .map(|index| row.get::<_, Option<String>>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            },
        )
        .optional()?;
    Ok(stored.map(|stored| {
        stored
            == values
                .iter()
                .map(|(_, value)| value.expected())
                .collect::<Vec<_>>()
    }))
}

/// Writes a row that may already be there, and proves the one already there is
/// the row this caller would have written.
///
/// This is the shape every immutable table in this crate needs: `INSERT OR
/// IGNORE`, and on a swallowed conflict read the row back under `keys` and
/// compare `values`. An exact replay is a no-op; a reused key carrying
/// different content raises `conflict` instead of surfacing a raw primary-key
/// violation from the driver.
///
/// `keys` must cover the constraint that `OR IGNORE` can swallow. When it does,
/// the read-back always finds the conflicting row; when it would not — every
/// caller here keys on the primary key, satisfies its `CHECK`s by construction,
/// and foreign-key violations are not swallowed by `OR IGNORE` at all — the
/// missing row surfaces as [`rusqlite::Error::QueryReturnedNoRows`].
pub(super) fn idempotent_insert(
    connection: &rusqlite::Connection,
    table: &str,
    keys: &[Column<'_>],
    values: &[Column<'_>],
    conflict: &str,
) -> rusqlite::Result<()> {
    let changed = insert(connection, " OR IGNORE", table, &[keys, values].concat())?;
    if changed == 1 {
        return Ok(());
    }
    match stored_row_matches(connection, table, keys, values)? {
        Some(true) => Ok(()),
        Some(false) => Err(invalid(conflict)),
        None => Err(rusqlite::Error::QueryReturnedNoRows),
    }
}
