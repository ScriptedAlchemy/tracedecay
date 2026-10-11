//! Immutable, generation-bound lexical artifacts.
//!
//! The daemon owns durability and head publication. This module owns only the
//! deterministic staging format, bounded page admission, verification, and
//! lightweight read ports over an already-published file.

use std::path::{Path, PathBuf};

use thiserror::Error;
pub use tracedecay_code_index::clones::CloneSelectedBlockV1;
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, CodeIndexInterruptionV1};
use tracedecay_sql::{Capability, CheckpointMode, ConnectionControl, ErrorExt, ErrorKind};

mod builder;
mod clone_census;
mod clone_codec;
mod fingerprints;
mod format;
mod increment;
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
pub use clone_census::CodeLexicalCloneIndexCensusV1;
pub use fingerprints::{
    CLONE_FINGERPRINT_CANDIDATE_BODY_BUDGET_V1, CLONE_FINGERPRINT_HOT_POSTING_THRESHOLD_V1,
    CLONE_FINGERPRINT_POSTING_ROW_BUDGET_V1, CLONE_NEAR_MATCH_BODY_COMPARISON_BUDGET_V1,
    CLONE_NEAR_MATCH_MINIMUM_COVERAGE_MILLIONTHS_V1, CLONE_NEAR_MATCH_TOKEN_WORK_BUDGET_V1,
    CloneFingerprintArtifactReadV1, CloneFingerprintCancellationPointV1,
    CloneFingerprintPartialReasonV1, CloneFingerprintReadAccountingV1,
    CloneFingerprintStreamDescriptorV1, CloneNearMatchArtifactV1, CloneNearMatchExtentV1,
    CloneSelectedBlockArtifactCandidateV1, CloneSelectedBlockArtifactReadV1,
    CloneSelectedBlockContainmentClassV1, MAX_CLONE_FINGERPRINT_PAGE_BODIES_V1,
};
pub use format::{
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactSectionDigestV1,
    VerifiedCodeLexicalArtifactV1, code_lexical_artifact_content_key,
};
pub use increment::CarriedFilePagesV1;
pub use prepared::PreparedCodeLexicalArtifactPageV1;
pub use reader::{
    CloneArtifactCursorV1, CloneArtifactPageV1, CloneExactArtifactMemberV1,
    CloneExactFamilyArtifactCandidateV1, CloneExactFamilyArtifactPageV1,
    CodeExactLexicalArtifactReaderV1, CodeLexicalArtifactReaderV1,
    CodeLexicalArtifactRestoreWitnessV1, MAX_CLONE_EXACT_PAGE_MEMBERS_V1,
};
pub use schema::CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1;

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
/// artifact cannot crowd out graph, shared-code, and serving residents.
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
/// MiB page cache). The reader's retained claim is the metadata copy plus
/// the cache actually granted, never this whole bound.
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
/// the top of the kernel SQLite window ([2, 64] MiB page cache).
pub const CODE_LEXICAL_ARTIFACT_SQLITE_CACHE_BYTES_V1: usize = 64 * 1024 * 1024;
const ARTIFACT_SQLITE_CACHE_BYTES: usize = CODE_LEXICAL_ARTIFACT_SQLITE_CACHE_BYTES_V1;
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
    /// A clone-family paging cursor names another artifact generation or
    /// request: the caller's evidence is stale, not the artifact.
    #[error("clone family cursor does not match its artifact or request")]
    StaleCloneFamilyCursor,
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

fn sqlite_error(error: tracedecay_sql::Error) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Io(error.to_string())
}

/// The absolute spelling every artifact open hands to the engine. SQLite's
/// Windows VFS passes the name to `CreateFileW` as given, so a long Windows
/// path takes the extended-length prefix. A name longer than the engine can
/// open is a contract failure, which ends the build instead of retrying it as
/// an unavailable store.
fn sqlite_open_path(path: &Path) -> Result<PathBuf, CodeLexicalArtifactErrorV1> {
    let absolute = std::path::absolute(path).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "cannot resolve lexical artifact path '{}': {error}",
            path.display()
        ))
    })?;
    #[cfg(windows)]
    let absolute = tracedecay_runtime_core::path_safety::extended_length_path(&absolute);
    let bytes = absolute.as_os_str().len();
    if let Some(limit) = tracedecay_sql::max_database_path_bytes()
        && bytes >= limit
    {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "SQLite database path '{}' is {bytes} bytes; SQLite's default VFS opens names shorter than {limit} bytes",
            absolute.display()
        )));
    }
    Ok(absolute)
}

fn sqlite_corrupt(error: tracedecay_sql::Error) -> CodeLexicalArtifactErrorV1 {
    match error.kind() {
        // The native engine reports a damaged header as a foreign format.
        ErrorKind::Corrupt | ErrorKind::ForeignFormat => {
            CodeLexicalArtifactErrorV1::Corrupt(error.to_string())
        }
        _ => CodeLexicalArtifactErrorV1::Io(error.to_string()),
    }
}

/// Open one artifact staging connection with its page cache at the kernel
/// window's 64 MiB ceiling. Every commit is durable when it returns, so each
/// committed batch or finalization wake is a resume point.
fn open_builder_connection(
    path: &Path,
) -> Result<tracedecay_sql::Connection, CodeLexicalArtifactErrorV1> {
    let connection =
        tracedecay_sql::Connection::open(sqlite_open_path(path)?).map_err(sqlite_error)?;
    let cache_kib = -i64::try_from(ARTIFACT_SQLITE_CACHE_BYTES / 1024)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    connection
        .pragma_update(None, "cache_size", cache_kib)
        .map_err(sqlite_error)?;
    // The builder installs the private schema, its triggers and every
    // finalization drop on this one connection.
    connection
        .set_capability(Capability::Maintenance)
        .map_err(sqlite_error)?;
    Ok(connection)
}

/// Fold every committed WAL frame into the file at `path` and remove the
/// `-wal` name, so the one file holds the committed state before it is
/// renamed, hashed or sealed. A rollback-journal store has nothing to fold.
/// The connection must make no further commit afterwards: it would write a
/// WAL that no longer has a name.
fn fold_journal(
    connection: &tracedecay_sql::Connection,
    path: &Path,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _span = tracing::trace_span!("query.artifact.staging.journal_fold").entered();
    let outcome = connection
        .checkpoint(CheckpointMode::Truncate)
        .map_err(sqlite_error)?;
    if outcome.busy {
        return Err(CodeLexicalArtifactErrorV1::Io(
            "lexical artifact staging could not fold its WAL".to_owned(),
        ));
    }
    remove_journal_sidecar(path)
}

fn remove_journal_sidecar(path: &Path) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut name = path.as_os_str().to_os_string();
    name.push("-wal");
    match std::fs::remove_file(PathBuf::from(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CodeLexicalArtifactErrorV1::Io(format!(
            "lexical artifact staging could not remove its folded WAL: {error}"
        ))),
    }
}

/// The `EXPLAIN QUERY PLAN` detail lines of `sql`, so tests pin a
/// statement's access path (`SEARCH` on a key, no `SCAN`, no temporary
/// b-tree for its order) on either engine.
#[cfg(test)]
fn query_plan(
    connection: &tracedecay_sql::Connection,
    sql: &str,
    params: impl tracedecay_sql::Params,
) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare query plan");
    let mut rows = statement.query(params).expect("read query plan");
    let mut details = Vec::new();
    while let Some(row) = rows.next().expect("step query plan") {
        details.push(row.get::<_, String>(3).expect("query plan detail"));
    }
    details
}

/// Plan lines that walk a whole table or index.
#[cfg(test)]
fn full_scans(plan: &[String]) -> usize {
    plan.iter()
        .filter(|detail| detail.starts_with("SCAN"))
        .count()
}

/// Plan lines that sort through a temporary b-tree.
#[cfg(test)]
fn temporary_sorts(plan: &[String]) -> usize {
    plan.iter()
        .filter(|detail| detail.contains("USE TEMP B-TREE"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        code_lexical_artifact_build_memory_budget_for,
    };

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
