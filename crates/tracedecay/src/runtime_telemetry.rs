//! Daemon-side runtime telemetry collection (issue #80).
//!
//! The snapshot types, the background process sampler, and the text renderer
//! live in [`tracedecay_session_memory::runtime_telemetry`]; this module owns the
//! collection pass that needs the live [`crate::tracedecay::TraceDecay`]
//! runtime — pragma reads over the owned connection, the writer-owner probe,
//! the reader-pool and store-runtime registry projections, and the
//! generation-census reader attached by the exact daemon route.

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_session_memory::runtime_telemetry::{
    DatabaseSnapshot, GenerationCensusReader, GenerationCensusSnapshot,
    GenerationCensusUnavailableReason, ReaderPoolOccupancy, RuntimeRegistrySnapshot,
    RuntimeSnapshot, WriterOwnerSnapshot, file_size, read_cached_process_sample, read_dirty_marker,
    unix_epoch_secs, with_suffix,
};

/// Capture a runtime snapshot for the given project.
///
/// Two responsibilities: (a) read the cached background process sample,
/// (b) `stat` the `SQLite` files and ask the connection for its journal
/// mode. Unavailable pragmas remain optional; failures to identify or stat the
/// store itself fail the read instead of fabricating a zero-sized database.
pub async fn collect(cg: &crate::tracedecay::TraceDecay) -> Result<RuntimeSnapshot> {
    collect_with_integrity(cg, false).await
}

pub async fn collect_with_integrity(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
) -> Result<RuntimeSnapshot> {
    collect_with_integrity_and_generation_census(cg, include_integrity, None).await
}

#[hotpath::measure(label = "runtime_ports.collect", future = true)]
pub(crate) async fn collect_with_integrity_and_generation_census(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
    generation_census_reader: Option<&GenerationCensusReader>,
) -> Result<RuntimeSnapshot> {
    let process = read_cached_process_sample();
    let database =
        collect_database_with_generation_census(cg, include_integrity, generation_census_reader)
            .await?;
    let captured_at = unix_epoch_secs()?;
    Ok(RuntimeSnapshot {
        captured_at,
        tracedecay_version: crate::version::build_version()?.to_owned(),
        host_os: std::env::consts::OS.to_owned(),
        process,
        database,
    })
}

pub(crate) async fn collect_database(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
) -> Result<DatabaseSnapshot> {
    collect_database_with_generation_census(cg, include_integrity, None).await
}

#[hotpath::measure(label = "runtime_ports.database", future = true)]
async fn collect_database_with_generation_census(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
    generation_census_reader: Option<&GenerationCensusReader>,
) -> Result<DatabaseSnapshot> {
    let project_root = cg.project_root().to_path_buf();
    let db_path = cg.db_path().clone();
    let canonical_db_path = db_path.canonicalize()?;
    let db_size_bytes = file_size(&db_path)?;
    let wal_size_bytes = file_size(&with_suffix(&db_path, "-wal"))?;
    let shm_size_bytes = file_size(&with_suffix(&db_path, "-shm"))?;
    let journal_mode = read_journal_mode(cg).await.ok();
    let synchronous = read_pragma_i64(cg, "PRAGMA synchronous", "read_synchronous")
        .await
        .ok();
    let page_size = read_pragma_i64(cg, "PRAGMA page_size", "read_page_size")
        .await
        .ok()
        .and_then(|value| u64::try_from(value).ok());
    let (quick_check_ok, quick_check_error) = if include_integrity {
        match cg.quick_check_report().await {
            Ok(None) => (Some(true), None),
            Ok(Some(problem)) => (Some(false), Some(problem)),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let dirty_marker = read_dirty_marker(&with_suffix(&db_path, ".dirty"));
    let writer_owner = match tracedecay_runtime_core::db::probe_writer_owner(&db_path) {
        Ok(tracedecay_runtime_core::db::WriterOwnership::Idle) => WriterOwnerSnapshot::Idle,
        Ok(tracedecay_runtime_core::db::WriterOwnership::Active(owner)) => {
            WriterOwnerSnapshot::Active {
                pid: owner.pid,
                started_epoch_ms: u64::try_from(owner.started_epoch_ms).unwrap_or(u64::MAX),
                version: owner.version,
                intent: owner.intent,
            }
        }
        Ok(tracedecay_runtime_core::db::WriterOwnership::ActiveUnknown) => {
            WriterOwnerSnapshot::ActiveUnknown
        }
        Err(error) => WriterOwnerSnapshot::ProbeFailed {
            error: error.to_string(),
        },
    };
    let generation_census = match generation_census_reader {
        Some(reader) => hotpath::future!(reader(), label = "runtime_ports.generation_census").await,
        None => GenerationCensusSnapshot::Unavailable {
            reason: GenerationCensusUnavailableReason::AuthorityUnavailable,
        },
    };
    let reader_pool = cg
        .db()
        .read_connection()
        .reader_pool_occupancy()
        .as_ref()
        .map(ReaderPoolOccupancy::from_pool);
    let runtime_registry =
        RuntimeRegistrySnapshot::from_projection(cg.store_runtime_registry().runtime_telemetry());
    Ok(DatabaseSnapshot {
        project_root,
        db_path,
        canonical_db_path,
        db_size_bytes,
        wal_size_bytes,
        shm_size_bytes,
        journal_mode,
        synchronous,
        page_size,
        quick_check_ok,
        quick_check_error,
        dirty_marker,
        writer_owner,
        generation_census,
        reader_pool,
        runtime_registry,
    })
}

async fn read_journal_mode(cg: &crate::tracedecay::TraceDecay) -> Result<String> {
    let mut rows = cg
        .db()
        .read_connection()
        .query("PRAGMA journal_mode", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read journal_mode: {e}"),
            operation: "read_journal_mode".to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read journal_mode row: {e}"),
            operation: "read_journal_mode".to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "no journal_mode row returned".to_string(),
            operation: "read_journal_mode".to_string(),
        })?;
    row.get::<String>(0).map_err(|e| TraceDecayError::Database {
        message: format!("failed to decode journal_mode: {e}"),
        operation: "read_journal_mode".to_string(),
    })
}

async fn read_pragma_i64(
    cg: &crate::tracedecay::TraceDecay,
    sql: &str,
    operation: &str,
) -> Result<i64> {
    let mut rows = cg
        .db()
        .read_connection()
        .query(sql, ())
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to query {sql}: {error}"),
            operation: operation.to_string(),
        })?;
    rows.next()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read {sql}: {error}"),
            operation: operation.to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: format!("{sql} returned no rows"),
            operation: operation.to_string(),
        })?
        .get(0)
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to decode {sql}: {error}"),
            operation: operation.to_string(),
        })
}
