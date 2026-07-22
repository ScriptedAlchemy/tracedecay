use serde::{Deserialize, Serialize};

use crate::{
    ErrorCode, ErrorPayload, MAX_FTS_QUERY_BYTES, MAX_FTS_RESULTS, SessionStoreCursor,
    SessionStoreFamily, SessionStoreTable,
    session::{validate_session_store_family, validate_session_store_page},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Metadata,
    Schema,
    ForeignKeys,
    PageSize,
    JournalMode,
    Integrity {
        check: IntegrityCheck,
    },
    RowParity {
        table: GraphTable,
    },
    FtsParity {
        table: GraphFtsTable,
        query: String,
        limit: u16,
    },
    SessionStoreCount {
        family: SessionStoreFamily,
        table: SessionStoreTable,
    },
    SessionStoreSchema {
        family: SessionStoreFamily,
        table: SessionStoreTable,
    },
    SessionStorePage {
        family: SessionStoreFamily,
        table: SessionStoreTable,
        cursor: Option<SessionStoreCursor>,
        limit: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityCheck {
    Quick,
    Full,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GraphTable {
    Nodes,
    Edges,
    Files,
    UnresolvedRefs,
    Vectors,
    Metadata,
    NodesFts,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GraphFtsTable {
    Nodes,
}

pub fn validate_command(command: &Command) -> Result<(), ErrorPayload> {
    match command {
        Command::FtsParity { query, limit, .. } => {
            if query.trim().is_empty()
                || query.as_bytes().contains(&0)
                || query.len() > MAX_FTS_QUERY_BYTES
            {
                return Err(ErrorPayload::new(
                    ErrorCode::InvalidFtsQuery,
                    format!(
                        "FTS query must be nonempty, NUL-free, and at most {MAX_FTS_QUERY_BYTES} bytes"
                    ),
                ));
            }
            if !(1..=MAX_FTS_RESULTS).contains(limit) {
                return Err(ErrorPayload::new(
                    ErrorCode::InvalidFtsLimit,
                    format!("FTS limit must be within 1..={MAX_FTS_RESULTS}"),
                ));
            }
        }
        Command::SessionStoreCount { family, table }
        | Command::SessionStoreSchema { family, table } => {
            validate_session_store_family(*family, *table)?;
        }
        Command::SessionStorePage {
            family,
            table,
            cursor,
            limit,
        } => validate_session_store_page(*family, *table, cursor.as_ref(), *limit)?,
        Command::Metadata
        | Command::Schema
        | Command::ForeignKeys
        | Command::PageSize
        | Command::JournalMode
        | Command::Integrity { .. }
        | Command::RowParity { .. } => {}
    }
    Ok(())
}

pub(crate) fn validate_request_wire_shape(value: &serde_json::Value) -> Result<(), ErrorPayload> {
    let Some(command) = value.get("command").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    let Some(command_type) = command.get("type").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let allowed: &[&str] = match command_type {
        "metadata" | "schema" | "foreign_keys" | "page_size" | "journal_mode" => &["type"],
        "integrity" => &["type", "check"],
        "row_parity" => &["type", "table"],
        "fts_parity" => &["type", "table", "query", "limit"],
        "session_store_count" | "session_store_schema" => &["type", "family", "table"],
        "session_store_page" => &["type", "family", "table", "cursor", "limit"],
        _ => return Ok(()),
    };
    if let Some(unexpected) = command.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidRequest,
            format!("command {command_type:?} has unknown option {unexpected:?}"),
        ));
    }
    Ok(())
}
