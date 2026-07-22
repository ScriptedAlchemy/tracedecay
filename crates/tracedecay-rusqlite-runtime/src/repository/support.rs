use std::fmt::Display;

use rusqlite::types::Type;
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
