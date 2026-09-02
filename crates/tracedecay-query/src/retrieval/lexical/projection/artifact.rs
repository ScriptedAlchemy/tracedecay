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
mod prepared;
mod reader;
mod row_codec;
mod schema;

pub use builder::{
    CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactBuilderV1,
    CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
    PreparedCodeLexicalArtifactBatchV1,
};
pub use format::{
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactSectionDigestV1,
    CodeLexicalImportMembershipWitnessV1, VerifiedCodeLexicalArtifactV1,
};
pub use prepared::PreparedCodeLexicalArtifactPageV1;
pub use reader::{CodeExactLexicalArtifactReaderV1, CodeLexicalArtifactReaderV1};
pub use schema::CodeLexicalArtifactWriterRevisionV1;

/// Default and maximum budget for the artifact build memory ledger.
///
/// This is a *ledger claim over tracked allocations*, not a hard RSS bound.
/// The enforced ledger charges, as if simultaneous: the SQLite page-cache
/// authority granted to the staging connection, the builder-retained
/// projection metadata (identity and logical-path capacities), every sealed
/// page retained by an admitted batch, every prepared relational value, and
/// the widest in-flight per-record preparation scratch. A batch whose charge
/// exceeds the budget is refused before SQLite mutation or source advance.
///
/// Explicitly outside the claim (the narrowed part): SQLite's `cache_size`
/// is a target the engine may transiently exceed, per-statement and
/// allocator metadata overhead are unaccounted, and `temp_store = FILE`
/// keeps temporary b-trees on disk rather than bounding them in memory.
pub const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1: usize = 1536 * 1024 * 1024;
/// Maximum reader cache budget: the stored metadata copy plus the SQLite
/// page-cache grant, which stays inside the kernel SQLite window ([2, 64]
/// MiB page cache). Sealed read-only readers also mmap the immutable file
/// itself; that mapping is file-backed and is not part of this heap claim.
/// The reader's retained claim is the metadata copy plus the cache actually
/// granted, never this whole bound.
/// The same narrowed claim as the build budget applies: `cache_size` is a
/// target, not a hard allocator bound.
pub const CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1: usize = 256 * 1024 * 1024;
pub const CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1: usize = 96 * 1024 * 1024;
pub const CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1: usize = 2_000_000;
pub const CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeLexicalArtifactBatchLimitV1 {
    Memory,
    PreparedRows,
    EstimatedWriteBytes,
}

/// Page-cache authority granted to artifact connections; charged in full
/// against the memory ledgers because SQLite may use all of it. Sized to
/// the top of the kernel SQLite window ([2, 64] MiB page cache). Staging
/// builder connections never grant an mmap window (rollback-journal
/// durability + WAL-coherence). Sealed read-only readers mmap the
/// content-addressed file so serving does not re-pread the same pages.
const ARTIFACT_SQLITE_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// The kernel SQLite window's page-cache floor.
const ARTIFACT_SQLITE_CACHE_FLOOR_BYTES: usize = 2 * 1024 * 1024;
const ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// Conservative live charge while one page's n-grams move from the ordered
/// key map and Roaring containers into canonical encoded shards. One logical
/// membership pays for a worst-case distinct B-tree entry/container plus the
/// sparse document value; the separately retained shard bytes cover encoded
/// output that overlaps the shrinking map.
const NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1: usize = 160;

#[derive(Debug, Error)]
pub enum CodeLexicalArtifactErrorV1 {
    #[error("lexical artifact is corrupt: {0}")]
    Corrupt(String),
    #[error("lexical artifact is incompatible: {0}")]
    Incompatible(String),
    #[error("lexical artifact I/O is unavailable: {0}")]
    Io(String),
    #[error("lexical artifact authority is missing: {0}")]
    Missing(String),
    #[error("lexical artifact reservation is unavailable: {0}")]
    Unreserved(String),
    #[error(
        "lexical artifact page batch exceeds its {limit:?} bound: needs {required}, maximum {maximum}"
    )]
    BatchTooLarge {
        limit: CodeLexicalArtifactBatchLimitV1,
        required: usize,
        maximum: usize,
    },
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

/// Open one artifact staging connection inside the kernel SQLite window:
/// no mmap grant, page cache at the kernel's 64 MiB ceiling, and
/// `synchronous = NORMAL`. The single deliberate exception is
/// `journal_mode = DELETE`: a sealed artifact is one content-addressed file,
/// and a WAL sidecar would fall outside its digest; bounded finalization
/// persists its own verified progress, so rollback-journal durability suffices.
/// SQLite's auxiliary sorter width reuses the canonical code-index worker
/// authority: the connection thread occupies one admitted worker and SQLite
/// may use only the remainder. `temp_store = FILE` keeps corpus-wide CREATE
/// INDEX runs disk-backed; their allocator/statement overhead remains outside
/// this module's narrowed memory-ledger claim. The modeled-reservation gauge
/// reports the caller plus effective helpers at the canonical 128 MiB worker
/// charge; it is a subset of the scheduler's existing admission, not another
/// cache or a second memory authority.
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
    let requested_sorter_workers =
        tracedecay_code_index::parallelism::indexing_workers().saturating_sub(1);
    let requested_sorter_workers_i64 = i64::try_from(requested_sorter_workers)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    connection
        .pragma_update(None, "threads", requested_sorter_workers_i64)
        .map_err(sqlite_error)?;
    let effective_sorter_workers: i64 = connection
        .pragma_query_value(None, "threads", |row| row.get(0))
        .map_err(sqlite_error)?;
    let effective_sorter_workers = usize::try_from(effective_sorter_workers).map_err(|_| {
        CodeLexicalArtifactErrorV1::Contract(
            "SQLite returned a negative lexical sorter worker limit".to_owned(),
        )
    })?;
    if effective_sorter_workers > requested_sorter_workers {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "SQLite granted {effective_sorter_workers} lexical sorter workers above the canonical {requested_sorter_workers} auxiliary-worker bound"
        )));
    }
    hotpath::gauge!("query.artifact.sqlite_sorter_workers.requested").set(requested_sorter_workers);
    hotpath::gauge!("query.artifact.sqlite_sorter_workers.effective").set(effective_sorter_workers);
    hotpath::gauge!("query.artifact.sqlite_sorter.modeled_reservation_bytes").set(
        tracedecay_code_index::parallelism::worker_reservation_bytes(
            effective_sorter_workers.saturating_add(1),
        ),
    );
    hotpath::gauge!("query.artifact.sqlite_sorter.temp_store_file").set(1u64);
    Ok(connection)
}

fn with_builder_sorter_cpu_admission<T>(
    connection: &rusqlite::Connection,
    operation: impl FnOnce() -> T,
) -> Result<T, CodeLexicalArtifactErrorV1> {
    let effective_sorter_workers: i64 = connection
        .pragma_query_value(None, "threads", |row| row.get(0))
        .map_err(sqlite_error)?;
    let admitted_units = usize::try_from(effective_sorter_workers)
        .map_err(|_| {
            CodeLexicalArtifactErrorV1::Contract(
                "SQLite returned a negative lexical sorter worker limit".to_owned(),
            )
        })?
        .checked_add(1)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical sorter CPU admission width overflowed".to_owned(),
            )
        })?;
    hotpath::gauge!("query.artifact.sqlite_sorter.admitted_cpu_units").set(admitted_units);
    Ok(tracedecay_code_index::parallelism::with_background_cpu_permits(admitted_units, operation))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::{
        ARTIFACT_SQLITE_CACHE_BYTES, open_builder_connection, with_builder_sorter_cpu_admission,
    };

    /// Staging builder connections stay inside the kernel SQLite window:
    /// no mmap grant, page cache at most 64 MiB, and `synchronous = NORMAL`
    /// — never a silent mmap/cache/sync override. Sealed readers mmap the
    /// immutable file on purpose; that path is not this connection.
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
            "staging builder connections must not grant an mmap window"
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
        let temp_store: i64 = connection
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .expect("temp-store pragma");
        assert_eq!(
            temp_store, 1,
            "SQLite sorter PMAs must spill to files rather than retaining the corpus in memory"
        );
    }

    #[test]
    fn builder_connections_reuse_canonical_worker_width_for_sqlite_sorters() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(&directory.path().join("workers.sqlite"))
            .expect("builder connection");
        let capability_probe =
            rusqlite::Connection::open_in_memory().expect("open SQLite worker capability probe");
        capability_probe
            .pragma_update(None, "threads", i64::MAX)
            .expect("probe SQLite worker ceiling");
        let sqlite_worker_ceiling: i64 = capability_probe
            .pragma_query_value(None, "threads", |row| row.get(0))
            .expect("read SQLite worker ceiling");
        let admitted_auxiliary_threads = tracedecay_code_index::parallelism::indexing_workers()
            .saturating_sub(1)
            .min(
                usize::try_from(sqlite_worker_ceiling).expect("nonnegative SQLite worker ceiling"),
            );
        let configured_threads: i64 = connection
            .pragma_query_value(None, "threads", |row| row.get(0))
            .expect("read artifact SQLite worker limit");
        assert_eq!(
            usize::try_from(configured_threads).expect("nonnegative artifact worker limit"),
            admitted_auxiliary_threads,
            "SQLite must receive the maximum auxiliary width available below the canonical worker bound and its own compile-time ceiling"
        );
    }

    #[test]
    fn builder_sorter_statements_hold_their_weighted_cpu_width() {
        let worker_width = tracedecay_code_index::parallelism::indexing_workers();
        let authority = tracedecay_private_fs::background_cpu::install_process_background_cpu(
            NonZeroUsize::new(worker_width).expect("nonzero code-index worker width"),
        )
        .expect("install matching process background CPU authority");
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(&directory.path().join("weighted.sqlite"))
            .expect("builder connection");
        let configured_threads: i64 = connection
            .pragma_query_value(None, "threads", |row| row.get(0))
            .expect("read configured SQLite helper width");
        let expected_units = usize::try_from(configured_threads)
            .expect("nonnegative SQLite helper width")
            .saturating_add(1)
            .min(worker_width);

        let observed_units =
            with_builder_sorter_cpu_admission(&connection, || authority.active_units())
                .expect("run weighted SQLite statement");

        assert_eq!(
            observed_units, expected_units,
            "one builder plus every configured SQLite helper must share the process CPU authority"
        );
        assert_eq!(
            authority.active_units(),
            0,
            "weighted admission must release every unit after the statement"
        );
    }
}
