use serde::{Deserialize, Serialize};

use crate::{
    ErrorCode, ErrorPayload, SessionStoreCursor, SessionStoreFamily, SessionStoreTable,
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

pub fn validate_command(command: &Command) -> Result<(), ErrorPayload> {
    match command {
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
        | Command::Integrity { .. } => {}
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
