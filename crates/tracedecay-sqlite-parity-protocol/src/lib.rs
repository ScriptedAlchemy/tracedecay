//! Driver-free wire protocol for process-isolated SQLite parity inspection.

mod codec;
mod command;
mod error;
mod request;
mod response;
mod results;
mod session;

pub use codec::{CanonicalRowHasher, decode_request_value};
pub use command::{Command, IntegrityCheck, validate_command};
pub use error::{ErrorCode, ErrorPayload};
pub use request::{
    CopiedDatabase, CopiedSnapshotProvenance, DatabaseKind, Request, SnapshotFileIdentity,
    VerifiedCopiedSnapshot, is_canonical_sha256_digest, validate_copied_snapshot_provenance,
    validate_request,
};
pub use response::{Response, ResponseOutcome};
pub use results::{
    EffectiveJournalMode, IntegrityReport, JournalModeMetadata, JournalModeNormalization, Metadata,
    Output, SchemaMetadata, SchemaObject, SchemaObjectKind, SourceHeaderJournalMode,
    SourceJournalMode,
};
pub use session::{
    SessionStoreColumn, SessionStoreCount, SessionStoreCursor, SessionStoreFamily,
    SessionStoreForeignKey, SessionStorePage, SessionStoreRow, SessionStoreSchema,
    SessionStoreTable,
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_SESSION_STORE_PAGE_SIZE: u16 = 100;
pub const MAX_CURSOR_TEXT_BYTES: usize = 4096;
pub const MAX_AUTHORITY_ID_BYTES: usize = 4096;
pub const HELPER_BINARY_NAME: &str = "tracedecay-rusqlite-parity";
pub const SNAPSHOT_DIGEST_ALGORITHM: &str = "sha256";
pub const ROW_DIGEST_ALGORITHM: &str = "sha256-v1";

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
pub type SessionStoreCountV1 = SessionStoreCount;
pub type SessionStoreSchemaV1 = SessionStoreSchema;
pub type SessionStoreColumnV1 = SessionStoreColumn;
pub type SessionStoreForeignKeyV1 = SessionStoreForeignKey;
pub type SessionStorePageV1 = SessionStorePage;
pub type SessionStoreRowV1 = SessionStoreRow;

#[cfg(test)]
mod tests;
