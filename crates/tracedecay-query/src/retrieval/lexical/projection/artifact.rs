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

/// Floor for the artifact build memory ledger.
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
/// is a target the engine may transiently exceed, and per-statement and
/// allocator metadata overhead are unaccounted.
pub const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1: usize = 1536 * 1024 * 1024;
const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_FRACTION_DENOMINATOR_V1: u64 = 8;
pub const CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1: usize = 16 * 1024 * 1024 * 1024;

/// Host-derived lexical artifact build budget.
///
/// Small hosts retain the established 1.5 GiB behavior. Larger hosts grant
/// one eighth of the process resident authority, capped so one background
/// artifact cannot crowd out graph, semantic, and serving residents.
#[must_use]
pub fn code_lexical_artifact_build_memory_budget_for(admitted_process_bytes: u64) -> usize {
    let floor = CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1 as u64;
    usize::try_from(
        (admitted_process_bytes / CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_FRACTION_DENOMINATOR_V1)
            .max(floor)
            .min(CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1 as u64),
    )
    .unwrap_or(usize::MAX)
}
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
/// may use only the memory-backed remainder. Corpus-wide CREATE INDEX runs use
/// in-memory temporary b-trees only when the same budget holds their modeled
/// reservation; otherwise they spill to files. The modeled-reservation gauge
/// reports the caller plus effective helpers at the canonical 128 MiB worker
/// charge; it is a subset of the scheduler's existing admission, not another
/// cache or a second memory authority.
fn open_builder_connection(
    path: &Path,
    memory_budget_bytes: usize,
) -> Result<rusqlite::Connection, CodeLexicalArtifactErrorV1> {
    let connection = rusqlite::Connection::open(path).map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "mmap_size", 0i64)
        .map_err(sqlite_error)?;
    let cache_kib = -i64::try_from(ARTIFACT_SQLITE_CACHE_BYTES / 1024)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    connection
        .pragma_update(None, "cache_size", cache_kib)
        .map_err(sqlite_error)?;
    let sorter_budget_bytes =
        u64::try_from(memory_budget_bytes.saturating_sub(ARTIFACT_SQLITE_CACHE_BYTES))
            .unwrap_or(u64::MAX);
    let requested_sorter_workers = (0..tracedecay_code_index::parallelism::indexing_workers())
        .rev()
        .find(|helpers| {
            tracedecay_code_index::parallelism::worker_reservation_bytes(helpers.saturating_add(1))
                <= sorter_budget_bytes
        })
        .unwrap_or(0);
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
    let modeled_reservation_bytes = tracedecay_code_index::parallelism::worker_reservation_bytes(
        effective_sorter_workers.saturating_add(1),
    );
    // SQLite disables sorter helpers when temporary b-trees are memory-only.
    // FILE still uses the admitted page cache while allowing parallel PMA
    // generation and merge for corpus-wide CREATE INDEX statements.
    let temp_store_file = true;
    connection
        .pragma_update(
            None,
            "temp_store",
            if temp_store_file { "FILE" } else { "MEMORY" },
        )
        .map_err(sqlite_error)?;
    hotpath::gauge!("query.artifact.sqlite_sorter.modeled_reservation_bytes")
        .set(modeled_reservation_bytes);
    hotpath::gauge!("query.artifact.sqlite_sorter.temp_store_file").set(u64::from(temp_store_file));
    Ok(connection)
}

/// CPU units one builder statement occupies: the builder thread plus every
/// SQLite sorter helper the connection was granted.
fn builder_sorter_cpu_units(
    connection: &rusqlite::Connection,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let effective_sorter_workers: i64 = connection
        .pragma_query_value(None, "threads", |row| row.get(0))
        .map_err(sqlite_error)?;
    usize::try_from(effective_sorter_workers)
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
        })
}

fn with_builder_sorter_cpu_admission<T>(
    connection: &rusqlite::Connection,
    operation: impl FnOnce() -> T,
) -> Result<T, CodeLexicalArtifactErrorV1> {
    let admitted_units = builder_sorter_cpu_units(connection)?;
    hotpath::gauge!("query.artifact.sqlite_sorter.admitted_cpu_units").set(admitted_units);
    Ok(tracedecay_code_index::parallelism::with_background_cpu_permits(admitted_units, operation))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use tracedecay_code_index::parallelism::ProcessBackgroundCpuV1;

    use super::{
        ARTIFACT_SQLITE_CACHE_BYTES, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        builder_sorter_cpu_units, code_lexical_artifact_build_memory_budget_for,
        open_builder_connection,
    };

    /// Staging builder connections stay inside the kernel SQLite window:
    /// no mmap grant, page cache at most 64 MiB, and `synchronous = NORMAL`
    /// — never a silent mmap/cache/sync override. Sealed readers mmap the
    /// immutable file on purpose; that path is not this connection.
    #[test]
    fn builder_connections_stay_inside_the_kernel_sqlite_window() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(
            &directory.path().join("window.sqlite"),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
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
            "threaded SQLite sorters require file-backed temporary b-trees"
        );
    }

    #[test]
    fn builder_connections_reuse_canonical_worker_width_for_sqlite_sorters() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(
            &directory.path().join("workers.sqlite"),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
        .expect("builder connection");
        let capability_probe =
            rusqlite::Connection::open_in_memory().expect("open SQLite worker capability probe");
        capability_probe
            .pragma_update(None, "threads", i64::MAX)
            .expect("probe SQLite worker ceiling");
        let sqlite_worker_ceiling: i64 = capability_probe
            .pragma_query_value(None, "threads", |row| row.get(0))
            .expect("read SQLite worker ceiling");
        let sorter_budget = u64::try_from(
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1 - ARTIFACT_SQLITE_CACHE_BYTES,
        )
        .expect("sorter budget");
        let admitted_auxiliary_threads =
            (0..tracedecay_code_index::parallelism::indexing_workers())
                .rev()
                .find(|helpers| {
                    tracedecay_code_index::parallelism::worker_reservation_bytes(helpers + 1)
                        <= sorter_budget
                })
                .expect("one builder worker fits")
                .min(
                    usize::try_from(sqlite_worker_ceiling)
                        .expect("nonnegative SQLite worker ceiling"),
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

    /// A builder statement is weighted as one builder thread plus every
    /// SQLite sorter helper the connection was granted, and that weight is
    /// clamped to the authority's width and released afterwards. The
    /// authority is local to the test; nothing process-wide is installed.
    #[test]
    fn builder_sorter_statements_hold_their_weighted_cpu_width() {
        let worker_width = tracedecay_code_index::parallelism::indexing_workers();
        let authority = std::sync::Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(worker_width).expect("nonzero code-index worker width"),
        ));
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let connection = open_builder_connection(
            &directory.path().join("weighted.sqlite"),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
        .expect("builder connection");
        let configured_threads: i64 = connection
            .pragma_query_value(None, "threads", |row| row.get(0))
            .expect("read configured SQLite helper width");
        let expected_units = usize::try_from(configured_threads)
            .expect("nonnegative SQLite helper width")
            .saturating_add(1)
            .min(worker_width);

        let weighted_units =
            builder_sorter_cpu_units(&connection).expect("builder statement weight");
        assert_eq!(
            weighted_units,
            usize::try_from(configured_threads).expect("nonnegative SQLite helper width") + 1,
            "one builder plus every configured SQLite helper is the statement's weight"
        );
        let observed_units = authority.with_permits(weighted_units, || authority.active_units());

        assert_eq!(
            observed_units, expected_units,
            "the weighted statement must occupy its helpers' units, clamped to the process width"
        );
        assert_eq!(
            authority.active_units(),
            0,
            "weighted admission must release every unit after the statement"
        );
    }

    #[test]
    fn build_memory_budget_scales_from_process_admission() {
        const GIB: u64 = 1024 * 1024 * 1024;

        assert_eq!(
            code_lexical_artifact_build_memory_budget_for(6 * GIB),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1
        );
        assert_eq!(
            code_lexical_artifact_build_memory_budget_for(96 * GIB),
            12 * GIB as usize
        );
        assert_eq!(
            code_lexical_artifact_build_memory_budget_for(256 * GIB),
            16 * GIB as usize
        );
    }
}

#[cfg(test)]
mod sorter_identity_tests {
    use std::fs;
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::{CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1, open_builder_connection};

    fn build_ngram_index(path: &Path, workers: i64) -> Vec<u8> {
        let mut connection =
            open_builder_connection(path, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1)
                .expect("open ngram identity fixture");
        connection
            .pragma_update(None, "threads", workers)
            .expect("set fixture sorter width");
        connection
            .execute_batch(
                "CREATE TABLE ngram_postings (
                    page_ordinal INTEGER NOT NULL, kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL, documents BLOB NOT NULL,
                    cardinality INTEGER NOT NULL,
                    PRIMARY KEY(page_ordinal, kind, ngram)
                ) WITHOUT ROWID;",
            )
            .expect("create ngram identity fixture");
        let transaction = connection.transaction().expect("seed ngram fixture");
        {
            let mut insert = transaction
                .prepare("INSERT INTO ngram_postings VALUES (?1, ?2, ?3, ?4, ?5)")
                .expect("prepare ngram fixture insert");
            for page in 0..64i64 {
                for ngram in (0..512i64).rev() {
                    insert
                        .execute(rusqlite::params![
                            page,
                            ngram % 2,
                            ngram,
                            ngram.to_le_bytes(),
                            1i64
                        ])
                        .expect("insert ngram fixture row");
                }
            }
        }
        transaction.commit().expect("commit ngram fixture");
        connection
            .execute_batch(
                "CREATE UNIQUE INDEX ngram_postings_by_ngram
                 ON ngram_postings(kind, ngram, page_ordinal, cardinality)",
            )
            .expect("build ngram fixture index");
        drop(connection);
        fs::read(path).expect("read ngram identity fixture")
    }

    #[test]
    fn threaded_sorter_preserves_ngram_index_file_identity() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let serial = build_ngram_index(&directory.path().join("serial.sqlite"), 0);
        let parallel = build_ngram_index(&directory.path().join("parallel.sqlite"), i64::MAX);
        assert_eq!(Sha256::digest(&parallel), Sha256::digest(&serial));
        assert_eq!(parallel, serial, "sorter width must preserve every byte");
    }
}
