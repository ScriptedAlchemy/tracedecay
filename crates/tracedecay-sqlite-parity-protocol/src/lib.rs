//! Driver-free wire protocol for process-isolated SQLite parity inspection.
//!
//! This crate deliberately contains only versioned DTOs and small `std`-only
//! normalization helpers. SQLite access, SQL text, and table allowlists belong
//! to the helper implementation, never here.

use std::{fs::Metadata as FileMetadata, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The only supported wire revision.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum request size accepted by the helper's one-request transport.
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// Maximum UTF-8 byte length accepted for a caller-supplied request ID.
pub const MAX_REQUEST_ID_BYTES: usize = 128;
/// Maximum UTF-8 byte length accepted for an FTS query.
pub const MAX_FTS_QUERY_BYTES: usize = 4096;
/// Maximum number of FTS matches returned by one command.
pub const MAX_FTS_RESULTS: u16 = 100;
/// Maximum number of session-store rows returned by one page command.
pub const MAX_SESSION_STORE_PAGE_SIZE: u16 = 100;
/// Maximum UTF-8 byte length accepted for one textual session-store cursor key.
pub const MAX_CURSOR_TEXT_BYTES: usize = 4096;
/// Maximum UTF-8 byte length accepted for a copied-snapshot authority identity.
pub const MAX_AUTHORITY_ID_BYTES: usize = 4096;
/// Stable Cargo binary target name for the process-isolated helper.
pub const HELPER_BINARY_NAME: &str = "tracedecay-rusqlite-parity";
/// Digest format used to seal copied snapshot bytes.
pub const SNAPSHOT_DIGEST_ALGORITHM: &str = "sha256";
/// Canonical SQLite row digest format shared by both parity engines.
pub const ROW_DIGEST_ALGORITHM: &str = "sha256-v1";

/// One closed parity request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u16,
    pub request_id: String,
    pub database: CopiedDatabase,
    pub command: Command,
}

/// A sealed copied database that the helper may inspect.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CopiedDatabase {
    pub path: PathBuf,
    pub kind: DatabaseKind,
    pub provenance: CopiedSnapshotProvenance,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    CopiedSnapshot,
}

/// Identity captured by the authority after its private snapshot file is sealed.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CopiedSnapshotProvenance {
    /// Stable authority identity supplied by the daemon, never inferred from a path.
    pub authority_identity: String,
    /// Canonical private staging directory that must contain `canonical_path`.
    pub staging_root: PathBuf,
    /// Canonical path captured immediately after the copy was sealed.
    pub canonical_path: PathBuf,
    /// Byte length captured immediately after the copy was sealed.
    pub byte_len: u64,
    /// SHA-256 of the complete copied database captured when it was sealed.
    pub content_digest: String,
    /// Platform file identity captured immediately after the copy was sealed.
    pub file_identity: SnapshotFileIdentity,
}

/// Platform-specific file identity used to reject replaced copied snapshots.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum SnapshotFileIdentity {
    Unix {
        device: u64,
        inode: u64,
        links: u64,
    },
    Windows {
        volume_serial: u32,
        file_index: u64,
        links: u32,
    },
    Unsupported,
}

impl SnapshotFileIdentity {
    /// Captures the stable identity available from portable `std` metadata.
    ///
    /// Windows' file-index accessors are not stable in Rust's standard library,
    /// so callers on that platform must use the explicit `Unsupported` variant
    /// until an authority supplies a separately captured Windows identity.
    #[must_use]
    pub fn from_metadata(metadata: &FileMetadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Self::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
                links: metadata.nlink(),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = metadata;
            Self::Unsupported
        }
    }
}

/// Closed parity operations. A SQL string is intentionally not representable.
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

/// Semantic graph row-count target. Its SQL mapping is private to the helper.
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

/// Semantic graph FTS target. Its SQL mapping is private to the helper.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GraphFtsTable {
    Nodes,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStoreFamily {
    Observation,
    Transcript,
    Lcm,
    Temporal,
}

/// Semantic session-store target. Its SQL mapping is private to the helper.
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
}

impl SessionStoreTable {
    /// Returns the semantic store family that owns this closed protocol target.
    #[must_use]
    pub const fn family(self) -> SessionStoreFamily {
        match self {
            Self::Observations => SessionStoreFamily::Observation,
            Self::Sessions | Self::SessionMessages => SessionStoreFamily::Transcript,
            Self::SessionSchemaMigrations | Self::LcmRawMessages => SessionStoreFamily::Lcm,
            Self::SessionTemporalSchemaMigrations
            | Self::SessionTemporalGenerations
            | Self::SessionTemporalObservationEffects => SessionStoreFamily::Temporal,
        }
    }

    /// Canonical keyset ordering for this closed table target.
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
        }
    }
}

/// A closed, table-specific keyset cursor. Arbitrary columns are not representable.
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
}

/// A response to one closed parity request.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Response {
    pub protocol_version: u16,
    pub request_id: Option<String>,
    /// Present only after the helper independently revalidated the copied file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_snapshot: Option<VerifiedCopiedSnapshot>,
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

/// Revalidated copied-snapshot identity returned to the authority.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCopiedSnapshot {
    pub authority_identity: String,
    pub canonical_path: PathBuf,
    pub byte_len: u64,
    pub content_digest: String,
    pub file_identity: SnapshotFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseOutcome {
    Ok { output: Output },
    Error { error: ErrorPayload },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseWire {
    protocol_version: u16,
    request_id: Option<String>,
    #[serde(default)]
    verified_snapshot: Option<VerifiedCopiedSnapshot>,
    status: String,
    #[serde(default)]
    output: Option<Output>,
    #[serde(default)]
    error: Option<ErrorPayload>,
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ResponseWire::deserialize(deserializer)?;
        let outcome = match (wire.status.as_str(), wire.output, wire.error) {
            ("ok", Some(output), None) => ResponseOutcome::Ok { output },
            ("error", None, Some(error)) => ResponseOutcome::Error { error },
            ("ok", _, _) => {
                return Err(serde::de::Error::custom(
                    "an ok response must contain output and no error",
                ));
            }
            ("error", _, _) => {
                return Err(serde::de::Error::custom(
                    "an error response must contain error and no output",
                ));
            }
            (status, _, _) => {
                return Err(serde::de::Error::custom(format!(
                    "unsupported response status {status:?}"
                )));
            }
        };
        Ok(Self {
            protocol_version: wire.protocol_version,
            request_id: wire.request_id,
            verified_snapshot: wire.verified_snapshot,
            outcome,
        })
    }
}

/// Result of a closed parity operation.
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
    /// Compatibility field retained as the immutable effective mode in v1.
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

/// Driver-neutral canonicalizer for SQLite row values.
///
/// Each value is framed by a stable type tag and byte length before hashing, so
/// concatenated values cannot collide through ambiguous boundaries. Callers
/// must feed columns in the query's declared order.
pub struct CanonicalRowHasher {
    hasher: Sha256,
}

impl CanonicalRowHasher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    pub fn update_null(&mut self) {
        self.update(0, &[]);
    }

    pub fn update_integer(&mut self, value: i64) {
        self.update(1, &value.to_be_bytes());
    }

    pub fn update_real(&mut self, value: f64) {
        self.update(2, &value.to_bits().to_be_bytes());
    }

    pub fn update_text(&mut self, value: &[u8]) {
        self.update(3, value);
    }

    pub fn update_blob(&mut self, value: &[u8]) {
        self.update(4, value);
    }

    fn update(&mut self, tag: u8, bytes: &[u8]) {
        self.hasher.update([tag]);
        self.hasher.update((bytes.len() as u64).to_be_bytes());
        self.hasher.update(bytes);
    }

    #[must_use]
    pub fn finish(self) -> String {
        format!(
            "{SNAPSHOT_DIGEST_ALGORITHM}:{}",
            hex::encode(self.hasher.finalize())
        )
    }
}

impl Default for CanonicalRowHasher {
    fn default() -> Self {
        Self::new()
    }
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
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RequestTooLarge,
    InvalidRequest,
    UnsupportedProtocolVersion,
    InvalidPath,
    InvalidSnapshotProvenance,
    RefusedLiveProfile,
    OpenFailed,
    ReadOnlyInvariant,
    InvalidFtsQuery,
    InvalidFtsLimit,
    InvalidStoreFamily,
    InvalidPageCursor,
    InvalidPageLimit,
    ResultLimitExceeded,
    InvalidSqliteValue,
    InvalidSqliteHeader,
    SqliteFailure,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite_code: Option<String>,
}

impl ErrorPayload {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            sqlite_code: None,
        }
    }

    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// Decodes one JSON wire request and enforces all driver-independent invariants.
pub fn decode_request_value(value: serde_json::Value) -> Result<Request, ErrorPayload> {
    validate_request_wire_shape(&value)?;
    let request = serde_json::from_value(value).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidRequest,
            format!("request does not match protocol v{PROTOCOL_VERSION}: {error}"),
        )
    })?;
    validate_request(&request)?;
    Ok(request)
}

/// Validates request-wide, driver-independent protocol invariants.
///
/// Path existence, canonicalization, and copied-file identity are deliberately
/// left to the process-isolated helper because they require filesystem access.
pub fn validate_request(request: &Request) -> Result<(), ErrorPayload> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(ErrorPayload::new(
            ErrorCode::UnsupportedProtocolVersion,
            format!(
                "unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                request.protocol_version
            ),
        ));
    }
    if request.request_id.is_empty() || request.request_id.len() > MAX_REQUEST_ID_BYTES {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidRequest,
            format!("request_id must contain 1..={MAX_REQUEST_ID_BYTES} bytes"),
        ));
    }
    validate_copied_snapshot_provenance(&request.database.provenance)?;
    validate_command(&request.command)
}

/// Rejects unknown command fields before deserializing a JSON wire request.
///
/// Serde accepts unknown fields on internally tagged unit variants, so the
/// protocol owns this small shape check rather than relying on helper-local
/// JSON handling. Missing or malformed command fields are left to `Request`
/// deserialization, which reports them as an invalid request.
fn validate_request_wire_shape(value: &serde_json::Value) -> Result<(), ErrorPayload> {
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

/// Validates driver-independent copied-snapshot provenance fields.
pub fn validate_copied_snapshot_provenance(
    provenance: &CopiedSnapshotProvenance,
) -> Result<(), ErrorPayload> {
    if provenance.authority_identity.is_empty()
        || provenance.authority_identity.len() > MAX_AUTHORITY_ID_BYTES
        || provenance.authority_identity.as_bytes().contains(&0)
    {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!(
                "authority_identity must be nonempty, NUL-free, and at most {MAX_AUTHORITY_ID_BYTES} bytes"
            ),
        ));
    }
    if !is_canonical_sha256_digest(&provenance.content_digest) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "content_digest must be a lowercase sha256:<64 hex digits> value",
        ));
    }
    Ok(())
}

#[must_use]
pub fn is_canonical_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Validates closed command semantics before a helper opens a copied snapshot.
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
        } => {
            validate_session_store_family(*family, *table)?;
            validate_page_limit(*limit)?;
            validate_page_cursor(*table, cursor.as_ref())?;
        }
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

fn validate_session_store_family(
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

/// Compatibility aliases make the v1 nature explicit to daemon-side callers
/// while preserving concise wire-DTO names for this single-version crate.
pub type RequestV1 = Request;
pub type ResponseV1 = Response;
pub type CommandV1 = Command;
pub type OutputV1 = Output;
pub type ResponseOutcomeV1 = ResponseOutcome;
pub type ErrorCodeV1 = ErrorCode;
pub type ErrorPayloadV1 = ErrorPayload;
pub type CopiedDatabaseV1 = CopiedDatabase;
pub type CopiedSnapshotProvenanceV1 = CopiedSnapshotProvenance;
pub type VerifiedCopiedSnapshotV1 = VerifiedCopiedSnapshot;
pub type DatabaseKindV1 = DatabaseKind;
pub type SnapshotFileIdentityV1 = SnapshotFileIdentity;
pub type IntegrityCheckV1 = IntegrityCheck;
pub type GraphTableV1 = GraphTable;
pub type GraphFtsTableV1 = GraphFtsTable;
pub type SessionStoreFamilyV1 = SessionStoreFamily;
pub type SessionStoreTableV1 = SessionStoreTable;
pub type SessionStoreCursorV1 = SessionStoreCursor;
pub type MetadataV1 = Metadata;
pub type SchemaMetadataV1 = SchemaMetadata;
pub type SchemaObjectV1 = SchemaObject;
pub type SchemaObjectKindV1 = SchemaObjectKind;
pub type JournalModeMetadataV1 = JournalModeMetadata;
pub type SourceHeaderJournalModeV1 = SourceHeaderJournalMode;
pub type SourceJournalModeV1 = SourceJournalMode;
pub type EffectiveJournalModeV1 = EffectiveJournalMode;
pub type JournalModeNormalizationV1 = JournalModeNormalization;
pub type IntegrityReportV1 = IntegrityReport;
pub type RowParityV1 = RowParity;
pub type FtsParityV1 = FtsParity;
pub type FtsMatchV1 = FtsMatch;
pub type SessionStoreCountV1 = SessionStoreCount;
pub type SessionStoreSchemaV1 = SessionStoreSchema;
pub type SessionStoreColumnV1 = SessionStoreColumn;
pub type SessionStoreForeignKeyV1 = SessionStoreForeignKey;
pub type SessionStorePageV1 = SessionStorePage;
pub type SessionStoreRowV1 = SessionStoreRow;

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, fs, path::PathBuf};

    use serde::{Serialize, de::DeserializeOwned};

    use super::*;

    fn round_trip<T>(value: T)
    where
        T: Debug + DeserializeOwned + PartialEq + Serialize,
    {
        let bytes = serde_json::to_vec(&value).expect("serialize DTO");
        assert_eq!(
            serde_json::from_slice::<T>(&bytes).expect("deserialize DTO"),
            value
        );
    }

    fn provenance() -> CopiedSnapshotProvenance {
        CopiedSnapshotProvenance {
            authority_identity: "store:project:example".to_owned(),
            staging_root: PathBuf::from("/private/staging"),
            canonical_path: PathBuf::from("/private/staging/snapshot.db"),
            byte_len: 17,
            content_digest: format!("sha256:{}", "1".repeat(64)),
            file_identity: SnapshotFileIdentity::Unix {
                device: 1,
                inode: 2,
                links: 1,
            },
        }
    }

    #[test]
    fn every_request_command_cursor_and_error_variant_round_trips() {
        round_trip(Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            database: CopiedDatabase {
                path: PathBuf::from("/private/staging/snapshot.db"),
                kind: DatabaseKind::CopiedSnapshot,
                provenance: provenance(),
            },
            command: Command::Metadata,
        });
        round_trip(DatabaseKind::CopiedSnapshot);
        for identity in [
            SnapshotFileIdentity::Unix {
                device: 1,
                inode: 2,
                links: 3,
            },
            SnapshotFileIdentity::Windows {
                volume_serial: 1,
                file_index: 2,
                links: 3,
            },
            SnapshotFileIdentity::Unsupported,
        ] {
            round_trip(identity);
        }
        for command in [
            Command::Metadata,
            Command::Schema,
            Command::ForeignKeys,
            Command::PageSize,
            Command::JournalMode,
            Command::Integrity {
                check: IntegrityCheck::Quick,
            },
            Command::Integrity {
                check: IntegrityCheck::Full,
            },
            Command::RowParity {
                table: GraphTable::Nodes,
            },
            Command::FtsParity {
                table: GraphFtsTable::Nodes,
                query: "parity".to_owned(),
                limit: 1,
            },
            Command::SessionStoreCount {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
            },
            Command::SessionStoreSchema {
                family: SessionStoreFamily::Transcript,
                table: SessionStoreTable::Sessions,
            },
            Command::SessionStorePage {
                family: SessionStoreFamily::Lcm,
                table: SessionStoreTable::LcmRawMessages,
                cursor: Some(SessionStoreCursor::LcmRawMessages { store_id: 1 }),
                limit: 1,
            },
        ] {
            round_trip(command);
        }
        for table in [
            GraphTable::Nodes,
            GraphTable::Edges,
            GraphTable::Files,
            GraphTable::UnresolvedRefs,
            GraphTable::Vectors,
            GraphTable::Metadata,
            GraphTable::NodesFts,
        ] {
            round_trip(table);
        }
        round_trip(GraphFtsTable::Nodes);
        for family in [
            SessionStoreFamily::Observation,
            SessionStoreFamily::Transcript,
            SessionStoreFamily::Lcm,
            SessionStoreFamily::Temporal,
        ] {
            round_trip(family);
        }
        for table in [
            SessionStoreTable::Observations,
            SessionStoreTable::Sessions,
            SessionStoreTable::SessionMessages,
            SessionStoreTable::SessionSchemaMigrations,
            SessionStoreTable::LcmRawMessages,
            SessionStoreTable::SessionTemporalSchemaMigrations,
            SessionStoreTable::SessionTemporalGenerations,
            SessionStoreTable::SessionTemporalObservationEffects,
        ] {
            round_trip(table);
        }
        for cursor in [
            SessionStoreCursor::Observations { sequence: 1 },
            SessionStoreCursor::Sessions {
                provider: "codex".to_owned(),
                session_id: "session".to_owned(),
            },
            SessionStoreCursor::SessionMessages {
                provider: "codex".to_owned(),
                session_id: "session".to_owned(),
                ordinal: 1,
                message_id: "message".to_owned(),
            },
            SessionStoreCursor::SessionSchemaMigrations {
                name: "migration".to_owned(),
            },
            SessionStoreCursor::LcmRawMessages { store_id: 1 },
            SessionStoreCursor::SessionTemporalSchemaMigrations {
                name: "migration".to_owned(),
            },
            SessionStoreCursor::SessionTemporalGenerations {
                session_id: "session".to_owned(),
                generation: 1,
            },
            SessionStoreCursor::SessionTemporalObservationEffects {
                observation_sequence: 1,
            },
        ] {
            round_trip(cursor);
        }
        for code in [
            ErrorCode::RequestTooLarge,
            ErrorCode::InvalidRequest,
            ErrorCode::UnsupportedProtocolVersion,
            ErrorCode::InvalidPath,
            ErrorCode::InvalidSnapshotProvenance,
            ErrorCode::RefusedLiveProfile,
            ErrorCode::OpenFailed,
            ErrorCode::ReadOnlyInvariant,
            ErrorCode::InvalidFtsQuery,
            ErrorCode::InvalidFtsLimit,
            ErrorCode::InvalidStoreFamily,
            ErrorCode::InvalidPageCursor,
            ErrorCode::InvalidPageLimit,
            ErrorCode::ResultLimitExceeded,
            ErrorCode::InvalidSqliteValue,
            ErrorCode::InvalidSqliteHeader,
            ErrorCode::SqliteFailure,
        ] {
            round_trip(code);
        }
    }

    #[test]
    fn every_graph_session_journal_and_response_result_variant_round_trips() {
        let column = SessionStoreColumn {
            ordinal: 0,
            name: "id".to_owned(),
            declared_type: "TEXT".to_owned(),
            not_null: true,
            default_value: None,
            primary_key_ordinal: 1,
        };
        let foreign_key = SessionStoreForeignKey {
            id: 0,
            sequence: 0,
            referenced_table: "parent".to_owned(),
            from_column: "parent_id".to_owned(),
            to_column: Some("id".to_owned()),
            on_update: "NO ACTION".to_owned(),
            on_delete: "CASCADE".to_owned(),
            match_kind: "NONE".to_owned(),
        };
        let journal = JournalModeMetadata {
            source_header: SourceHeaderJournalMode {
                read_version: 2,
                write_version: 2,
                mode: SourceJournalMode::Wal,
            },
            mode: EffectiveJournalMode::Delete,
            immutable_effective_mode: EffectiveJournalMode::Delete,
            normalization: JournalModeNormalization::WalSourceImmutableDelete,
        };
        for mode in [SourceJournalMode::Rollback, SourceJournalMode::Wal] {
            round_trip(mode);
        }
        round_trip(EffectiveJournalMode::Delete);
        for normalization in [
            JournalModeNormalization::RollbackSourceImmutableDelete,
            JournalModeNormalization::WalSourceImmutableDelete,
        ] {
            round_trip(normalization);
        }
        for kind in [
            SchemaObjectKind::Table,
            SchemaObjectKind::Index,
            SchemaObjectKind::Trigger,
            SchemaObjectKind::View,
        ] {
            round_trip(kind);
        }
        for output in [
            Output::Metadata(Metadata {
                canonical_path: PathBuf::from("/private/staging/snapshot.db"),
                query_only: true,
                immutable: true,
                sqlite_version: "3.0.0".to_owned(),
                compile_options: vec!["ENABLE_FTS5".to_owned()],
            }),
            Output::Schema(SchemaMetadata {
                schema_version: 1,
                user_version: 2,
                objects: vec![SchemaObject {
                    kind: SchemaObjectKind::Table,
                    name: "nodes".to_owned(),
                    table_name: "nodes".to_owned(),
                    sql: Some("CREATE TABLE nodes".to_owned()),
                }],
            }),
            Output::ForeignKeys { enabled: true },
            Output::PageSize { bytes: 4096 },
            Output::JournalMode(journal),
            Output::Integrity(IntegrityReport {
                check: IntegrityCheck::Full,
                findings: vec!["ok".to_owned()],
            }),
            Output::RowParity(RowParity {
                table: GraphTable::Nodes,
                row_count: Some(1),
            }),
            Output::FtsParity(FtsParity {
                table: GraphFtsTable::Nodes,
                matches: vec![FtsMatch {
                    rowid: 1,
                    rank: 1.5,
                    snippet: "match".to_owned(),
                }],
            }),
            Output::SessionStoreCount(SessionStoreCount {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                row_count: Some(1),
            }),
            Output::SessionStoreSchema(SessionStoreSchema {
                family: SessionStoreFamily::Transcript,
                table: SessionStoreTable::Sessions,
                exists: true,
                columns: vec![column.clone()],
                foreign_keys: vec![foreign_key.clone()],
            }),
            Output::SessionStorePage(SessionStorePage {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                order_columns: vec!["sequence".to_owned()],
                digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
                rows: vec![SessionStoreRow::Observations {
                    sequence: 1,
                    observation_id: "observation".to_owned(),
                    payload_digest: "payload".to_owned(),
                    row_digest: "row".to_owned(),
                }],
                next_cursor: None,
            }),
        ] {
            round_trip(output);
        }
        for row in [
            SessionStoreRow::Observations {
                sequence: 1,
                observation_id: "observation".to_owned(),
                payload_digest: "payload".to_owned(),
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::Sessions {
                provider: "codex".to_owned(),
                session_id: "session".to_owned(),
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::SessionMessages {
                provider: "codex".to_owned(),
                session_id: "session".to_owned(),
                ordinal: 1,
                message_id: "message".to_owned(),
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::SessionSchemaMigrations {
                name: "migration".to_owned(),
                version: 1,
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::LcmRawMessages {
                store_id: 1,
                provider: "codex".to_owned(),
                session_id: "session".to_owned(),
                ordinal: 1,
                message_id: "message".to_owned(),
                content_hash: "content".to_owned(),
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::SessionTemporalSchemaMigrations {
                name: "migration".to_owned(),
                version: 1,
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::SessionTemporalGenerations {
                session_id: "session".to_owned(),
                generation: 1,
                state: "ready".to_owned(),
                row_digest: "row".to_owned(),
            },
            SessionStoreRow::SessionTemporalObservationEffects {
                observation_id: "observation".to_owned(),
                observation_sequence: 1,
                session_id: "session".to_owned(),
                effect_digest: "effect".to_owned(),
                row_digest: "row".to_owned(),
            },
        ] {
            round_trip(row);
        }
        round_trip(Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some("request-1".to_owned()),
            verified_snapshot: Some(VerifiedCopiedSnapshot {
                authority_identity: "store:project:example".to_owned(),
                canonical_path: PathBuf::from("/private/staging/snapshot.db"),
                byte_len: 17,
                content_digest: format!("sha256:{}", "1".repeat(64)),
                file_identity: SnapshotFileIdentity::Unsupported,
            }),
            outcome: ResponseOutcome::Error {
                error: ErrorPayload::new(ErrorCode::InvalidRequest, "invalid request"),
            },
        });
        round_trip(Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some("request-2".to_owned()),
            verified_snapshot: None,
            outcome: ResponseOutcome::Ok {
                output: Output::PageSize { bytes: 4096 },
            },
        });
    }

    #[test]
    fn dto_envelopes_and_tagged_variants_reject_unknown_fields() {
        let request = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            database: CopiedDatabase {
                path: PathBuf::from("/private/staging/snapshot.db"),
                kind: DatabaseKind::CopiedSnapshot,
                provenance: provenance(),
            },
            command: Command::Metadata,
        };
        let mut unknown_request = serde_json::to_value(&request).expect("serialize request");
        unknown_request["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Request>(unknown_request).is_err());

        let mut unknown_provenance = serde_json::to_value(&request).expect("serialize request");
        unknown_provenance["database"]["provenance"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Request>(unknown_provenance).is_err());

        assert_eq!(
            decode_request_value(serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": "request-1",
                "database": request.database,
                "command": { "type": "metadata", "unexpected": true },
            }))
            .expect_err("unknown command fields must be rejected")
            .code,
            ErrorCode::InvalidRequest
        );
        assert!(
            serde_json::from_value::<Output>(serde_json::json!({
                "type": "page_size",
                "bytes": 4096,
                "unexpected": true,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SessionStoreRow>(serde_json::json!({
                "table": "observations",
                "sequence": 1,
                "observation_id": "observation",
                "payload_digest": "payload",
                "row_digest": "row",
                "unexpected": true,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ErrorPayload>(serde_json::json!({
                "code": "invalid_request",
                "message": "bad request",
                "unexpected": true,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Response>(serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": "request-1",
                "status": "ok",
                "output": { "type": "page_size", "bytes": 4096 },
                "unexpected": true,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Response>(serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": "request-1",
                "status": "ok",
                "error": {
                    "code": "invalid_request",
                    "message": "wrong outcome shape"
                },
            }))
            .is_err()
        );
    }

    #[test]
    fn semantic_validation_rejects_invalid_closed_commands_before_io() {
        let valid = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            database: CopiedDatabase {
                path: PathBuf::from("/private/staging/snapshot.db"),
                kind: DatabaseKind::CopiedSnapshot,
                provenance: provenance(),
            },
            command: Command::SessionStorePage {
                family: SessionStoreFamily::Transcript,
                table: SessionStoreTable::Sessions,
                cursor: Some(SessionStoreCursor::Sessions {
                    provider: "codex".to_owned(),
                    session_id: "session-1".to_owned(),
                }),
                limit: 1,
            },
        };
        assert!(validate_request(&valid).is_ok());

        for (command, code) in [
            (
                Command::FtsParity {
                    table: GraphFtsTable::Nodes,
                    query: " ".to_owned(),
                    limit: 1,
                },
                ErrorCode::InvalidFtsQuery,
            ),
            (
                Command::FtsParity {
                    table: GraphFtsTable::Nodes,
                    query: "nodes".to_owned(),
                    limit: 0,
                },
                ErrorCode::InvalidFtsLimit,
            ),
            (
                Command::SessionStoreCount {
                    family: SessionStoreFamily::Lcm,
                    table: SessionStoreTable::Observations,
                },
                ErrorCode::InvalidStoreFamily,
            ),
            (
                Command::SessionStorePage {
                    family: SessionStoreFamily::Observation,
                    table: SessionStoreTable::Observations,
                    cursor: Some(SessionStoreCursor::LcmRawMessages { store_id: 1 }),
                    limit: 1,
                },
                ErrorCode::InvalidPageCursor,
            ),
            (
                Command::SessionStorePage {
                    family: SessionStoreFamily::Observation,
                    table: SessionStoreTable::Observations,
                    cursor: None,
                    limit: MAX_SESSION_STORE_PAGE_SIZE + 1,
                },
                ErrorCode::InvalidPageLimit,
            ),
        ] {
            assert_eq!(validate_command(&command).unwrap_err().code, code);
        }

        let mut invalid_provenance = provenance();
        invalid_provenance.authority_identity.clear();
        assert_eq!(
            validate_copied_snapshot_provenance(&invalid_provenance)
                .expect_err("empty authority identity must be rejected")
                .code,
            ErrorCode::InvalidSnapshotProvenance
        );
        let mut invalid_provenance = provenance();
        invalid_provenance.content_digest = "sha256:ABC".to_owned();
        assert_eq!(
            validate_copied_snapshot_provenance(&invalid_provenance)
                .expect_err("noncanonical content digest must be rejected")
                .code,
            ErrorCode::InvalidSnapshotProvenance
        );
    }

    #[test]
    fn canonical_row_digest_frames_every_sqlite_value_type() {
        let mut first = CanonicalRowHasher::new();
        first.update_null();
        first.update_integer(-7);
        first.update_real(1.5);
        first.update_text("東京".as_bytes());
        first.update_blob(&[0, 1, 2]);
        let first = first.finish();
        assert!(is_canonical_sha256_digest(&first));

        let mut second = CanonicalRowHasher::new();
        second.update_null();
        second.update_integer(-7);
        second.update_real(1.5);
        second.update_text("東京".as_bytes());
        second.update_blob(&[0, 1, 2]);
        assert_eq!(second.finish(), first);

        let mut different_boundaries = CanonicalRowHasher::new();
        different_boundaries.update_text(b"ab");
        different_boundaries.update_text(b"c");
        let mut joined = CanonicalRowHasher::new();
        joined.update_text(b"a");
        joined.update_text(b"bc");
        assert_ne!(different_boundaries.finish(), joined.finish());
    }

    #[test]
    fn metadata_identity_uses_the_supported_platform_shape() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("snapshot.db");
        fs::write(&path, b"sealed").expect("write temporary snapshot");
        let identity = SnapshotFileIdentity::from_metadata(
            &fs::metadata(&path).expect("read temporary snapshot metadata"),
        );
        #[cfg(unix)]
        assert!(matches!(identity, SnapshotFileIdentity::Unix { .. }));
        #[cfg(not(unix))]
        assert_eq!(identity, SnapshotFileIdentity::Unsupported);
    }
}
