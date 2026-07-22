use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    GraphFtsTable, GraphTable, IntegrityCheck, SessionStoreCount, SessionStorePage,
    SessionStoreSchema,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Output {
    Metadata(Metadata),
    Schema(SchemaMetadata),
    ForeignKeys { enabled: bool },
    PageSize { bytes: u32 },
    JournalMode(JournalModeMetadata),
    Integrity(IntegrityReport),
    RowParity(RowParity),
    FtsParity(FtsParity),
    SessionStoreCount(SessionStoreCount),
    SessionStoreSchema(SessionStoreSchema),
    SessionStorePage(SessionStorePage),
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub canonical_path: PathBuf,
    pub query_only: bool,
    pub immutable: bool,
    pub sqlite_version: String,
    pub compile_options: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchemaMetadata {
    pub schema_version: i64,
    pub user_version: i64,
    pub objects: Vec<SchemaObject>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchemaObject {
    pub kind: SchemaObjectKind,
    pub name: String,
    pub table_name: String,
    pub sql: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SchemaObjectKind {
    Table,
    Index,
    Trigger,
    View,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JournalModeMetadata {
    pub source_header: SourceHeaderJournalMode,
    pub mode: EffectiveJournalMode,
    pub immutable_effective_mode: EffectiveJournalMode,
    pub normalization: JournalModeNormalization,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceHeaderJournalMode {
    pub read_version: u8,
    pub write_version: u8,
    pub mode: SourceJournalMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SourceJournalMode {
    Rollback,
    Wal,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveJournalMode {
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JournalModeNormalization {
    RollbackSourceImmutableDelete,
    WalSourceImmutableDelete,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IntegrityReport {
    pub check: IntegrityCheck,
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RowParity {
    pub table: GraphTable,
    pub row_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FtsParity {
    pub table: GraphFtsTable,
    pub matches: Vec<FtsMatch>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FtsMatch {
    pub rowid: i64,
    pub rank: f64,
    pub snippet: String,
}
