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
    Fact,
    Diagnostics,
    Configuration,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStoreTable {
    Observations,
    SourceCursors,
    Sessions,
    SessionMessages,
    SessionSchemaMigrations,
    LcmRawMessages,
    SessionTemporalSchemaMigrations,
    SessionTemporalGenerations,
    SessionTemporalObservationEffects,
    SessionTemporalProjectionReceipts,
    SessionOccurrences,
    SessionAssertions,
    SessionSummaryNodes,
    MemoryV2Facts,
    MemoryV2CurrentFacts,
    MemoryV2Assertions,
    MemoryV2LineageEvents,
    RetrievalAnchors,
    GenerationDiagnostics,
    DiagnosticGenerationPublications,
    ConfigurationRevisions,
    ConfigurationEntries,
    ConfigurationMutationReceipts,
    ConfigurationAuditEvents,
}

impl SessionStoreTable {
    #[must_use]
    pub const fn family(self) -> SessionStoreFamily {
        match self {
            Self::Observations | Self::SourceCursors => SessionStoreFamily::Observation,
            Self::Sessions | Self::SessionMessages => SessionStoreFamily::Transcript,
            Self::SessionSchemaMigrations | Self::LcmRawMessages => SessionStoreFamily::Lcm,
            Self::SessionTemporalSchemaMigrations
            | Self::SessionTemporalGenerations
            | Self::SessionTemporalObservationEffects
            | Self::SessionTemporalProjectionReceipts
            | Self::SessionOccurrences
            | Self::SessionAssertions => SessionStoreFamily::Temporal,
            Self::SessionSummaryNodes => SessionStoreFamily::Summary,
            Self::MemoryV2Facts
            | Self::MemoryV2CurrentFacts
            | Self::MemoryV2Assertions
            | Self::MemoryV2LineageEvents
            | Self::RetrievalAnchors => SessionStoreFamily::Fact,
            Self::GenerationDiagnostics | Self::DiagnosticGenerationPublications => {
                SessionStoreFamily::Diagnostics
            }
            Self::ConfigurationRevisions
            | Self::ConfigurationEntries
            | Self::ConfigurationMutationReceipts
            | Self::ConfigurationAuditEvents => SessionStoreFamily::Configuration,
        }
    }

    #[must_use]
    pub const fn order_columns(self) -> &'static [&'static str] {
        match self {
            Self::Observations => &["sequence"],
            Self::SourceCursors => &["source_json", "scope_json"],
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
            Self::SessionAssertions => &["session_id", "generation", "assertion_id"],
            Self::SessionSummaryNodes => &["summary_id"],
            Self::MemoryV2Facts | Self::MemoryV2CurrentFacts => {
                &["fact_id", "owner_kind", "project_id"]
            }
            Self::MemoryV2Assertions => &["assertion_id", "fact_id", "owner_kind", "project_id"],
            Self::MemoryV2LineageEvents => &["event_sequence"],
            Self::RetrievalAnchors => &["anchor_id"],
            Self::GenerationDiagnostics => &["diagnostic_anchor"],
            Self::DiagnosticGenerationPublications => &["generation_id"],
            Self::ConfigurationRevisions => &["revision_id"],
            Self::ConfigurationEntries => &["revision_id", "key", "layer_kind", "layer_id"],
            Self::ConfigurationMutationReceipts => &["receipt_id"],
            Self::ConfigurationAuditEvents => &["event_id"],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "table", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionStoreCursor {
    Observations {
        sequence: i64,
    },
    SourceCursors {
        source_json: String,
        scope_json: String,
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
    SessionAssertions {
        session_id: String,
        generation: i64,
        assertion_id: String,
    },
    SessionSummaryNodes {
        summary_id: String,
    },
    MemoryV2Facts {
        fact_id: String,
        owner_kind: String,
        project_id: String,
    },
    MemoryV2CurrentFacts {
        fact_id: String,
        owner_kind: String,
        project_id: String,
    },
    MemoryV2Assertions {
        assertion_id: String,
        fact_id: String,
        owner_kind: String,
        project_id: String,
    },
    MemoryV2LineageEvents {
        event_sequence: i64,
    },
    RetrievalAnchors {
        anchor_id: String,
    },
    GenerationDiagnostics {
        diagnostic_anchor: String,
    },
    DiagnosticGenerationPublications {
        generation_id: String,
    },
    ConfigurationRevisions {
        revision_id: String,
    },
    ConfigurationEntries {
        revision_id: String,
        key: String,
        layer_kind: String,
        layer_id: String,
    },
    ConfigurationMutationReceipts {
        receipt_id: String,
    },
    ConfigurationAuditEvents {
        event_id: String,
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
    SourceCursors {
        source_json: String,
        scope_json: String,
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
    SessionAssertions {
        session_id: String,
        generation: i64,
        assertion_id: String,
        assertion_kind: String,
        row_digest: String,
    },
    SessionSummaryNodes {
        summary_id: String,
        session_id: String,
        summary_anchor_id: String,
        row_digest: String,
    },
    MemoryV2Facts {
        fact_id: String,
        owner_kind: String,
        project_id: String,
        identity_json: String,
        row_digest: String,
    },
    MemoryV2CurrentFacts {
        fact_id: String,
        owner_kind: String,
        project_id: String,
        payload_access: String,
        projection_state: String,
        row_digest: String,
    },
    MemoryV2Assertions {
        assertion_id: String,
        fact_id: String,
        owner_kind: String,
        project_id: String,
        row_digest: String,
    },
    MemoryV2LineageEvents {
        event_sequence: i64,
        event_id: String,
        fact_id: String,
        row_digest: String,
    },
    RetrievalAnchors {
        anchor_id: String,
        projection_generation: String,
        row_digest: String,
    },
    GenerationDiagnostics {
        diagnostic_anchor: String,
        generation_id: String,
        severity: String,
        record_state: String,
        row_digest: String,
    },
    DiagnosticGenerationPublications {
        generation_id: String,
        record_state: String,
        row_digest: String,
    },
    ConfigurationRevisions {
        revision_id: String,
        snapshot_id: String,
        operation_kind: String,
        row_digest: String,
    },
    ConfigurationEntries {
        revision_id: String,
        key: String,
        layer_kind: String,
        layer_id: String,
        row_digest: String,
    },
    ConfigurationMutationReceipts {
        receipt_id: String,
        result_revision_id: String,
        activation_status: String,
        row_digest: String,
    },
    ConfigurationAuditEvents {
        event_id: String,
        operation_kind: String,
        base_revision_id: String,
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
            SessionStoreTable::SourceCursors,
            SessionStoreCursor::SourceCursors {
                source_json,
                scope_json,
            },
        ) => {
            validate_cursor_text("source_json", source_json)?;
            validate_cursor_text("scope_json", scope_json)?;
            true
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
            SessionStoreTable::SessionAssertions,
            SessionStoreCursor::SessionAssertions {
                session_id,
                generation,
                assertion_id,
            },
        ) => {
            validate_cursor_text("session_id", session_id)?;
            validate_cursor_text("assertion_id", assertion_id)?;
            *generation > 0
        }
        (
            SessionStoreTable::SessionSummaryNodes,
            SessionStoreCursor::SessionSummaryNodes { summary_id },
        ) => {
            validate_cursor_text("summary_id", summary_id)?;
            true
        }
        (
            SessionStoreTable::MemoryV2Facts,
            SessionStoreCursor::MemoryV2Facts {
                fact_id,
                owner_kind,
                project_id,
            },
        )
        | (
            SessionStoreTable::MemoryV2CurrentFacts,
            SessionStoreCursor::MemoryV2CurrentFacts {
                fact_id,
                owner_kind,
                project_id,
            },
        ) => {
            validate_cursor_text("fact_id", fact_id)?;
            validate_cursor_text("owner_kind", owner_kind)?;
            validate_cursor_text("project_id", project_id)?;
            true
        }
        (
            SessionStoreTable::MemoryV2Assertions,
            SessionStoreCursor::MemoryV2Assertions {
                assertion_id,
                fact_id,
                owner_kind,
                project_id,
            },
        ) => {
            validate_cursor_text("assertion_id", assertion_id)?;
            validate_cursor_text("fact_id", fact_id)?;
            validate_cursor_text("owner_kind", owner_kind)?;
            validate_cursor_text("project_id", project_id)?;
            true
        }
        (
            SessionStoreTable::MemoryV2LineageEvents,
            SessionStoreCursor::MemoryV2LineageEvents { event_sequence },
        ) => *event_sequence > 0,
        (
            SessionStoreTable::RetrievalAnchors,
            SessionStoreCursor::RetrievalAnchors { anchor_id },
        ) => {
            validate_cursor_text("anchor_id", anchor_id)?;
            true
        }
        (
            SessionStoreTable::GenerationDiagnostics,
            SessionStoreCursor::GenerationDiagnostics { diagnostic_anchor },
        ) => {
            validate_cursor_text("diagnostic_anchor", diagnostic_anchor)?;
            true
        }
        (
            SessionStoreTable::DiagnosticGenerationPublications,
            SessionStoreCursor::DiagnosticGenerationPublications { generation_id },
        ) => {
            validate_cursor_text("generation_id", generation_id)?;
            true
        }
        (
            SessionStoreTable::ConfigurationRevisions,
            SessionStoreCursor::ConfigurationRevisions { revision_id },
        ) => {
            validate_cursor_text("revision_id", revision_id)?;
            true
        }
        (
            SessionStoreTable::ConfigurationEntries,
            SessionStoreCursor::ConfigurationEntries {
                revision_id,
                key,
                layer_kind,
                layer_id,
            },
        ) => {
            validate_cursor_text("revision_id", revision_id)?;
            validate_cursor_text("key", key)?;
            validate_cursor_text("layer_kind", layer_kind)?;
            validate_cursor_text("layer_id", layer_id)?;
            true
        }
        (
            SessionStoreTable::ConfigurationMutationReceipts,
            SessionStoreCursor::ConfigurationMutationReceipts { receipt_id },
        ) => {
            validate_cursor_text("receipt_id", receipt_id)?;
            true
        }
        (
            SessionStoreTable::ConfigurationAuditEvents,
            SessionStoreCursor::ConfigurationAuditEvents { event_id },
        ) => {
            validate_cursor_text("event_id", event_id)?;
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
