use serde::{Deserialize, Serialize};

use crate::{ErrorCode, ErrorPayload, MAX_CURSOR_TEXT_BYTES, MAX_SESSION_STORE_PAGE_SIZE};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStoreFamily {
    Observation,
    Transcript,
    Lcm,
    Temporal,
    Summary,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStoreTable {
    Observations,
    Sessions,
    SessionMessages,
    SessionSchemaMigrations,
    LcmRawMessages,
    SessionTemporalSchemaMigrations,
    SessionTemporalGenerations,
    SessionTemporalObservationEffects,
    SessionTemporalProjectionReceipts,
    SessionOccurrences,
    SessionSummaryNodes,
}

impl SessionStoreTable {
    #[must_use]
    pub const fn family(self) -> SessionStoreFamily {
        match self {
            Self::Observations => SessionStoreFamily::Observation,
            Self::Sessions | Self::SessionMessages => SessionStoreFamily::Transcript,
            Self::SessionSchemaMigrations | Self::LcmRawMessages => SessionStoreFamily::Lcm,
            Self::SessionTemporalSchemaMigrations
            | Self::SessionTemporalGenerations
            | Self::SessionTemporalObservationEffects
            | Self::SessionTemporalProjectionReceipts
            | Self::SessionOccurrences => SessionStoreFamily::Temporal,
            Self::SessionSummaryNodes => SessionStoreFamily::Summary,
        }
    }

    #[must_use]
    pub const fn order_columns(self) -> &'static [&'static str] {
        match self {
            Self::Observations => &["sequence"],
            Self::Sessions => &["provider", "session_id"],
            Self::SessionMessages => &["provider", "session_id", "ordinal", "message_id"],
            Self::SessionSchemaMigrations | Self::SessionTemporalSchemaMigrations => &["name"],
            Self::LcmRawMessages => &["store_id"],
            Self::SessionTemporalGenerations => &["session_id", "generation"],
            Self::SessionTemporalObservationEffects => &["observation_sequence"],
            Self::SessionTemporalProjectionReceipts => {
                &["session_id", "generation", "batch_ordinal"]
            }
            Self::SessionOccurrences => &["session_id", "generation", "occurrence_id"],
            Self::SessionSummaryNodes => &["summary_id"],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "table", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionStoreCursor {
    Observations {
        sequence: i64,
    },
    Sessions {
        provider: String,
        session_id: String,
    },
    SessionMessages {
        provider: String,
        session_id: String,
        ordinal: i64,
        message_id: String,
    },
    SessionSchemaMigrations {
        name: String,
    },
    LcmRawMessages {
        store_id: i64,
    },
    SessionTemporalSchemaMigrations {
        name: String,
    },
    SessionTemporalGenerations {
        session_id: String,
        generation: i64,
    },
    SessionTemporalObservationEffects {
        observation_sequence: i64,
    },
    SessionTemporalProjectionReceipts {
        session_id: String,
        generation: i64,
        batch_ordinal: i64,
    },
    SessionOccurrences {
        session_id: String,
        generation: i64,
        occurrence_id: String,
    },
    SessionSummaryNodes {
        summary_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionStoreCount {
    pub family: SessionStoreFamily,
    pub table: SessionStoreTable,
    pub row_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionStoreSchema {
    pub family: SessionStoreFamily,
    pub table: SessionStoreTable,
    pub exists: bool,
    pub columns: Vec<SessionStoreColumn>,
    pub foreign_keys: Vec<SessionStoreForeignKey>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionStoreColumn {
    pub ordinal: u32,
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default_value: Option<String>,
    pub primary_key_ordinal: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct SessionStoreForeignKey {
    pub id: u32,
    pub sequence: u32,
    pub referenced_table: String,
    pub from_column: String,
    pub to_column: Option<String>,
    pub on_update: String,
    pub on_delete: String,
    pub match_kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionStorePage {
    pub family: SessionStoreFamily,
    pub table: SessionStoreTable,
    pub order_columns: Vec<String>,
    pub digest_algorithm: String,
    pub rows: Vec<SessionStoreRow>,
    pub next_cursor: Option<SessionStoreCursor>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "table", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionStoreRow {
    Observations {
        sequence: i64,
        observation_id: String,
        payload_digest: String,
        row_digest: String,
    },
    Sessions {
        provider: String,
        session_id: String,
        row_digest: String,
    },
    SessionMessages {
        provider: String,
        session_id: String,
        ordinal: i64,
        message_id: String,
        row_digest: String,
    },
    SessionSchemaMigrations {
        name: String,
        version: i64,
        row_digest: String,
    },
    LcmRawMessages {
        store_id: i64,
        provider: String,
        session_id: String,
        ordinal: i64,
        message_id: String,
        content_hash: String,
        row_digest: String,
    },
    SessionTemporalSchemaMigrations {
        name: String,
        version: i64,
        row_digest: String,
    },
    SessionTemporalGenerations {
        session_id: String,
        generation: i64,
        state: String,
        row_digest: String,
    },
    SessionTemporalObservationEffects {
        observation_id: String,
        observation_sequence: i64,
        session_id: String,
        effect_digest: String,
        row_digest: String,
    },
    SessionTemporalProjectionReceipts {
        session_id: String,
        generation: i64,
        batch_ordinal: i64,
        batch_digest: String,
        row_digest: String,
    },
    SessionOccurrences {
        session_id: String,
        generation: i64,
        occurrence_id: String,
        role: String,
        row_digest: String,
    },
    SessionSummaryNodes {
        summary_id: String,
        session_id: String,
        summary_anchor_id: String,
        row_digest: String,
    },
}

pub(crate) fn validate_session_store_family(
    family: SessionStoreFamily,
    table: SessionStoreTable,
) -> Result<(), ErrorPayload> {
    let expected = table.family();
    if expected != family {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidStoreFamily,
            format!(
                "table {:?} belongs to {:?}, not {:?}",
                table, expected, family
            ),
        ));
    }
    Ok(())
}

pub(crate) fn validate_session_store_page(
    family: SessionStoreFamily,
    table: SessionStoreTable,
    cursor: Option<&SessionStoreCursor>,
    limit: u16,
) -> Result<(), ErrorPayload> {
    validate_session_store_family(family, table)?;
    validate_page_limit(limit)?;
    validate_page_cursor(table, cursor)
}

fn validate_page_limit(limit: u16) -> Result<(), ErrorPayload> {
    if !(1..=MAX_SESSION_STORE_PAGE_SIZE).contains(&limit) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPageLimit,
            format!("session-store page limit must be within 1..={MAX_SESSION_STORE_PAGE_SIZE}"),
        ));
    }
    Ok(())
}

fn validate_cursor_text(label: &str, value: &str) -> Result<(), ErrorPayload> {
    if value.is_empty() || value.len() > MAX_CURSOR_TEXT_BYTES || value.as_bytes().contains(&0) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPageCursor,
            format!(
                "session-store cursor {label} must be nonempty, NUL-free, and at most {MAX_CURSOR_TEXT_BYTES} bytes"
            ),
        ));
    }
    Ok(())
}

fn validate_page_cursor(
    table: SessionStoreTable,
    cursor: Option<&SessionStoreCursor>,
) -> Result<(), ErrorPayload> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    let valid = match (table, cursor) {
        (SessionStoreTable::Observations, SessionStoreCursor::Observations { sequence }) => {
            *sequence > 0
        }
        (
            SessionStoreTable::Sessions,
            SessionStoreCursor::Sessions {
                provider,
                session_id,
            },
        ) => {
            validate_cursor_text("provider", provider)?;
            validate_cursor_text("session_id", session_id)?;
            true
        }
        (
            SessionStoreTable::SessionMessages,
            SessionStoreCursor::SessionMessages {
                provider,
                session_id,
                ordinal,
                message_id,
            },
        ) => {
            validate_cursor_text("provider", provider)?;
            validate_cursor_text("session_id", session_id)?;
            validate_cursor_text("message_id", message_id)?;
            *ordinal >= 0
        }
        (
            SessionStoreTable::SessionSchemaMigrations,
            SessionStoreCursor::SessionSchemaMigrations { name },
        ) => {
            validate_cursor_text("name", name)?;
            true
        }
        (SessionStoreTable::LcmRawMessages, SessionStoreCursor::LcmRawMessages { store_id }) => {
            *store_id > 0
        }
        (
            SessionStoreTable::SessionTemporalSchemaMigrations,
            SessionStoreCursor::SessionTemporalSchemaMigrations { name },
        ) => {
            validate_cursor_text("name", name)?;
            true
        }
        (
            SessionStoreTable::SessionTemporalGenerations,
            SessionStoreCursor::SessionTemporalGenerations {
                session_id,
                generation,
            },
        ) => {
            validate_cursor_text("session_id", session_id)?;
            *generation > 0
        }
        (
            SessionStoreTable::SessionTemporalObservationEffects,
            SessionStoreCursor::SessionTemporalObservationEffects {
                observation_sequence,
            },
        ) => *observation_sequence > 0,
        (
            SessionStoreTable::SessionTemporalProjectionReceipts,
            SessionStoreCursor::SessionTemporalProjectionReceipts {
                session_id,
                generation,
                batch_ordinal,
            },
        ) => {
            validate_cursor_text("session_id", session_id)?;
            *generation > 0 && *batch_ordinal >= 0
        }
        (
            SessionStoreTable::SessionOccurrences,
            SessionStoreCursor::SessionOccurrences {
                session_id,
                generation,
                occurrence_id,
            },
        ) => {
            validate_cursor_text("session_id", session_id)?;
            validate_cursor_text("occurrence_id", occurrence_id)?;
            *generation > 0
        }
        (
            SessionStoreTable::SessionSummaryNodes,
            SessionStoreCursor::SessionSummaryNodes { summary_id },
        ) => {
            validate_cursor_text("summary_id", summary_id)?;
            true
        }
        _ => false,
    };
    if !valid {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPageCursor,
            format!("cursor does not contain a valid keyset for table {table:?}"),
        ));
    }
    Ok(())
}
