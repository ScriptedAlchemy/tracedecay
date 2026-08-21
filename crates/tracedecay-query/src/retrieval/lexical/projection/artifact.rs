//! Immutable, generation-bound lexical artifacts.
//!
//! The daemon owns durability and head publication. This module owns only the
//! deterministic staging format, bounded page admission, verification, and
//! lightweight read ports over an already-published file.

use std::path::Path;

use thiserror::Error;
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, CodeIndexInterruptionV1};

mod builder;
mod format;
mod postings;
mod reader;

pub use builder::{CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactBuilderV1};
pub use format::{
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactSectionDigestV1,
    CodeLexicalImportMembershipWitnessV1, VerifiedCodeLexicalArtifactV1,
};
pub use reader::{CodeExactLexicalArtifactReaderV1, CodeLexicalArtifactReaderV1};

pub const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1: usize = 256 * 1024 * 1024;
pub const CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1: usize = 256 * 1024 * 1024;
pub const CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1: usize = 96 * 1024 * 1024;

const ARTIFACT_SQLITE_CACHE_BYTES: usize = 96 * 1024 * 1024;
const ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CodeLexicalArtifactErrorV1 {
    #[error("lexical artifact is corrupt: {0}")]
    Corrupt(String),
    #[error("lexical artifact is incompatible: {0}")]
    Incompatible(String),
    #[error("lexical artifact I/O is unavailable: {0}")]
    Io(String),
    #[error("lexical artifact operation was interrupted: {0:?}")]
    Interrupted(CodeIndexInterruptionV1),
    #[error("lexical artifact contract violation: {0}")]
    Contract(String),
}

fn checkpoint(control: &dyn CodeIndexExecutionControlV1) -> Result<(), CodeLexicalArtifactErrorV1> {
    if control.is_cancelled() {
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled,
        ))
    } else if control.is_deadline_exceeded() {
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            CodeIndexInterruptionV1::DeadlineExceeded,
        ))
    } else {
        Ok(())
    }
}

fn sqlite_error(error: rusqlite::Error) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Io(error.to_string())
}

fn sqlite_corrupt(error: rusqlite::Error) -> CodeLexicalArtifactErrorV1 {
    match error.sqlite_error_code() {
        Some(
            rusqlite::ffi::ErrorCode::DatabaseCorrupt | rusqlite::ffi::ErrorCode::NotADatabase,
        ) => CodeLexicalArtifactErrorV1::Corrupt(error.to_string()),
        _ => CodeLexicalArtifactErrorV1::Io(error.to_string()),
    }
}

fn open_builder_connection(
    path: &Path,
) -> Result<rusqlite::Connection, CodeLexicalArtifactErrorV1> {
    let connection = rusqlite::Connection::open(path).map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "OFF")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "temp_store", "FILE")
        .map_err(sqlite_error)?;
    let cache_kib = -i64::try_from(ARTIFACT_SQLITE_CACHE_BYTES / 1024)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    connection
        .pragma_update(None, "cache_size", cache_kib)
        .map_err(sqlite_error)?;
    Ok(connection)
}
