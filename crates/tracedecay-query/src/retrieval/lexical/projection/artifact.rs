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
    CodeLexicalArtifactMountMetadataV1, CodeLexicalArtifactOccurrenceV1,
    CodeLexicalArtifactSectionDigestV1, CodeLexicalImportMembershipWitnessV1,
    VerifiedCodeLexicalArtifactV1,
};
pub use reader::{CodeExactLexicalArtifactReaderV1, CodeLexicalArtifactReaderV1};

/// Default and maximum budget for the artifact build memory ledger.
///
/// This is a *ledger claim over tracked allocations*, not a hard RSS bound.
/// The enforced ledger charges, as if simultaneous: the SQLite page-cache
/// authority granted to the staging connection, the builder-retained
/// projection metadata (identity and logical-path capacities), the sealed
/// page's retained owned bytes, and an allocation-free conservative
/// per-chunk/per-import transient reservation — the cloned chunk, projected row, field/token
/// vectors and per-field frequency map at `Vec`/`String` capacity
/// granularity, the row and import JSON serialization buffers, and the
/// pre-compaction n-gram scratch. A page whose charge exceeds the budget is
/// refused before any staging mutation and before the source advances.
///
/// Explicitly outside the claim (the narrowed part): SQLite's `cache_size`
/// is a target the engine may transiently exceed, per-statement and
/// allocator metadata overhead are unaccounted, and `temp_store = FILE`
/// keeps temporary b-trees on disk rather than bounding them in memory.
pub const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1: usize = 256 * 1024 * 1024;
/// Maximum reader reservation: a fixed corpus-independent mount/verifier
/// window plus the SQLite page-cache grant, which stays inside the kernel
/// SQLite window ([2, 64] MiB page cache, mmap disabled). Logical paths live
/// only in artifact rows and are never retained as a corpus map at mount.
/// The same narrowed claim as the build budget applies: `cache_size` is a
/// target, not a hard allocator bound.
pub const CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1: usize = 256 * 1024 * 1024;
pub const CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1: usize = 96 * 1024 * 1024;

/// Page-cache authority granted to artifact connections; charged in full
/// against the memory ledgers because SQLite may use all of it. Sized to
/// the top of the kernel SQLite window ([2, 64] MiB page cache, mmap
/// disabled): artifact connections never grant an mmap window and never
/// exceed this cache target.
const ARTIFACT_SQLITE_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// The kernel SQLite window's page-cache floor.
const ARTIFACT_SQLITE_CACHE_FLOOR_BYTES: usize = 2 * 1024 * 1024;
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

#[cfg(test)]
mod tests {
    use super::{ARTIFACT_SQLITE_CACHE_BYTES, open_builder_connection};

    /// Artifact connections stay inside the kernel SQLite window: no mmap
    /// grant, page cache at most 64 MiB, and `synchronous = NORMAL` — never
    /// a silent mmap/cache/sync override.
    #[test]
    fn builder_connections_stay_inside_the_kernel_sqlite_window() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(&directory.path().join("window.sqlite"))
            .expect("builder connection");
        let mmap: i64 = connection
            .pragma_query_value(None, "mmap_size", |row| row.get(0))
            .expect("mmap pragma");
        assert_eq!(
            mmap, 0,
            "artifact connections must not grant an mmap window"
        );
        let cache_kib: i64 = connection
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .expect("cache pragma");
        assert_eq!(
            cache_kib,
            -i64::try_from(ARTIFACT_SQLITE_CACHE_BYTES / 1024).expect("cache bound"),
            "the page cache must sit at the kernel window's 64 MiB ceiling"
        );
        assert!(
            (2 * 1024..=64 * 1024).contains(&(-cache_kib)),
            "the page cache must stay within the kernel [2, 64] MiB window"
        );
        let synchronous: i64 = connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .expect("synchronous pragma");
        assert_eq!(
            synchronous, 1,
            "artifact staging must use synchronous=NORMAL"
        );
    }
}

fn sqlite_corrupt(error: rusqlite::Error) -> CodeLexicalArtifactErrorV1 {
    match error.sqlite_error_code() {
        Some(
            rusqlite::ffi::ErrorCode::DatabaseCorrupt | rusqlite::ffi::ErrorCode::NotADatabase,
        ) => CodeLexicalArtifactErrorV1::Corrupt(error.to_string()),
        _ => CodeLexicalArtifactErrorV1::Io(error.to_string()),
    }
}

/// Open one artifact staging connection inside the kernel SQLite window:
/// no mmap grant, page cache at the kernel's 64 MiB ceiling, and
/// `synchronous = NORMAL`. The single deliberate exception is
/// `journal_mode = DELETE`: a sealed artifact is one content-addressed file,
/// and a WAL sidecar would fall outside its digest; the finalization replay
/// re-verifies every derived row, so rollback-journal durability suffices.
fn open_builder_connection(
    path: &Path,
) -> Result<rusqlite::Connection, CodeLexicalArtifactErrorV1> {
    let connection = rusqlite::Connection::open(path).map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "temp_store", "FILE")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "mmap_size", 0i64)
        .map_err(sqlite_error)?;
    let cache_kib = -i64::try_from(ARTIFACT_SQLITE_CACHE_BYTES / 1024)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    connection
        .pragma_update(None, "cache_size", cache_kib)
        .map_err(sqlite_error)?;
    Ok(connection)
}
