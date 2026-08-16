// Rust guideline compliant 2026-05-25
//! Runtime telemetry snapshot for diagnosing CPU/RAM regressions
//! (issue #80).
//!
//! Captures process-level resource use (RSS, virtual size, CPU%, thread
//! count) via [`sysinfo`] and database-level signals (sqlite + WAL + SHM
//! sizes, journal mode) so users hitting unexpected resource pressure
//! can attach a structured snapshot to a bug report.
//!
//! `cpu_percent` requires a refresh interval to be meaningful — sysinfo
//! reports CPU% as a delta between two refreshes. [`collect`] performs
//! the refresh, sleeps for [`CPU_SAMPLE_WINDOW`], then refreshes again.
//! Callers therefore pay ~200 ms latency per snapshot.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use crate::errors::{Result, TraceDecayError};

mod store_runtime;

pub use store_runtime::{
    RuntimeRegistryAggregateSnapshot, RuntimeRegistryShardSnapshot, RuntimeRegistrySnapshot,
    RuntimeRegistryWriterSnapshot,
};

/// Async reader for the generation census attached by an exact daemon route.
pub(crate) type GenerationCensusFuture =
    Pin<Box<dyn Future<Output = GenerationCensusSnapshot> + Send + 'static>>;
pub(crate) type GenerationCensusReader =
    Arc<dyn Fn() -> GenerationCensusFuture + Send + Sync + 'static>;

/// Closed reasons for a generation census that cannot be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationCensusUnavailableReason {
    AuthorityUnavailable,
    ExactScopeGenerationNotReady,
    SealedGenerationCensusInvalid,
}

impl GenerationCensusUnavailableReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::ExactScopeGenerationNotReady => "exact_scope_generation_not_ready",
            Self::SealedGenerationCensusInvalid => "sealed_generation_census_invalid",
        }
    }
}

/// Runtime telemetry's truthful view of code-index census availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GenerationCensusSnapshot {
    Observed {
        #[serde(flatten)]
        statistics: tracedecay_code_index::production::CodeIndexGenerationStatisticsV1,
    },
    Unavailable {
        reason: GenerationCensusUnavailableReason,
    },
}

/// Window over which `cpu_percent` is sampled.
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

/// Captured process + database telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Captured at (Unix epoch seconds).
    pub captured_at: u64,
    /// `tracedecay` build version (e.g. `6.0.0`).
    pub tracedecay_version: String,
    /// Host OS short name (`macos`, `linux`, `windows`, …).
    pub host_os: String,
    pub process: ProcessSnapshot,
    pub database: DatabaseSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub rss_bytes: u64,
    pub virtual_bytes: u64,
    /// Sustained CPU% across [`CPU_SAMPLE_WINDOW`] (0-100 per core, may
    /// exceed 100 on multi-threaded workloads).
    pub cpu_percent: f32,
    pub uptime_secs: u64,
    /// Number of CPUs the kernel reports — useful for interpreting
    /// `cpu_percent > 100`.
    pub system_cpu_count: usize,
    /// Total system memory in bytes (for ratio reporting).
    pub system_total_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseSnapshot {
    pub project_root: PathBuf,
    /// Canonical durable database currently owned by this project runtime.
    pub db_path: PathBuf,
    /// Canonical identity of the file owned by this process, when resolvable.
    pub canonical_db_path: PathBuf,
    pub db_size_bytes: u64,
    /// Size of the WAL (`-wal`) file alongside the DB, when present.
    pub wal_size_bytes: u64,
    /// Size of the shared-memory file (`-shm`).
    pub shm_size_bytes: u64,
    /// `journal_mode` PRAGMA (`wal`, `delete`, `truncate`, …).
    pub journal_mode: Option<String>,
    /// Numeric `synchronous` PRAGMA (`0` OFF, `1` NORMAL, `2` FULL, `3` EXTRA).
    pub synchronous: Option<i64>,
    pub page_size: Option<u64>,
    /// `PRAGMA quick_check` executed on the already-owned daemon connection.
    pub quick_check_ok: Option<bool>,
    pub quick_check_error: Option<String>,
    pub dirty_marker: DirtyMarkerSnapshot,
    /// Kernel writer lease currently observed for this database.
    pub writer_owner: WriterOwnerSnapshot,
    /// Aggregate source, symbol, and edge facts from the exact sealed code
    /// generation, or the typed reason that authority is not mounted.
    pub generation_census: GenerationCensusSnapshot,
    /// Live reader-pool occupancy for this store, when the pool is attached.
    ///
    /// Reader saturation is the failure users actually hit — a query reports
    /// `reader acquisition saturated` and there is otherwise nothing to look
    /// at. This is the after-the-fact evidence: where the workers went, and
    /// whether anyone is queued behind them.
    pub reader_pool: Option<ReaderPoolOccupancy>,
    /// Bounded daemon store-runtime registry telemetry for all mounted shards.
    pub runtime_registry: RuntimeRegistrySnapshot,
    /// Exact project runtime and reconciliation ownership retained by the
    /// current profile's canonical daemon session registry.
    #[serde(default)]
    pub session_runtime_retention: SessionRuntimeRetentionSnapshot,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRuntimeRetentionSnapshot {
    pub project_runtime_capacity: u64,
    pub profile_memory_runtimes: u64,
    pub profile_session_runtimes: u64,
    pub project_memory_runtimes: u64,
    pub project_session_runtimes: u64,
    pub retained_memory_graph_reconciliation_tasks: u64,
    pub retired_project_memory_runtimes: u64,
    pub retired_project_session_runtimes: u64,
    pub retirement_refusals: u64,
}

/// Per-lane reader-pool occupancy at one instant.
///
/// `available + leased + limbo` accounts for every worker in a lane. `limbo`
/// is a worker whose lease ended but whose snapshot rollback has not been
/// confirmed; `waiting` is acquisitions blocked for capacity, which is what
/// separates a lane that is busy from one that is turning callers away.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderPoolOccupancy {
    /// `ready` or `draining`.
    pub state: String,
    /// Successful exact-SQL snapshot admissions since this reader pool attached.
    pub snapshot_admissions: u64,
    pub general: ReaderLaneOccupancy,
    pub health: ReaderLaneOccupancy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderLaneOccupancy {
    pub workers: u16,
    pub available: u16,
    pub leased: u16,
    pub limbo: u16,
    pub waiting: u16,
}

impl ReaderPoolOccupancy {
    fn from_pool(snapshot: &crate::db::engine::ReaderPoolSnapshot) -> Self {
        Self {
            state: match snapshot.state {
                crate::db::engine::ReaderPoolState::Ready => "ready".to_string(),
                crate::db::engine::ReaderPoolState::Draining => "draining".to_string(),
            },
            snapshot_admissions: snapshot.snapshot_admissions,
            general: ReaderLaneOccupancy {
                workers: snapshot.general_workers,
                available: snapshot.available_general,
                leased: snapshot.leased_general,
                limbo: snapshot.limbo_general,
                waiting: snapshot.waiting_general,
            },
            health: ReaderLaneOccupancy {
                workers: snapshot.health_workers,
                available: snapshot.available_health,
                leased: snapshot.leased_health,
                limbo: snapshot.limbo_health,
                waiting: snapshot.waiting_health,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WriterOwnerSnapshot {
    Idle,
    Active {
        pid: u32,
        started_epoch_ms: u128,
        version: String,
        intent: String,
    },
    ActiveUnknown,
    ProbeFailed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyMarkerSnapshot {
    pub path: PathBuf,
    pub exists: bool,
    pub parsed: bool,
    pub owner_pid: Option<u64>,
    pub epoch: Option<String>,
    pub state: Option<String>,
    pub schema: Option<u64>,
}

/// Capture a runtime snapshot for the given project.
///
/// Two responsibilities: (a) sample our own process via `sysinfo`,
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

pub(crate) async fn collect_with_integrity_and_generation_census(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
    generation_census_reader: Option<&GenerationCensusReader>,
) -> Result<RuntimeSnapshot> {
    let process = sample_process()?;
    let database =
        collect_database_with_generation_census(cg, include_integrity, generation_census_reader)
            .await?;
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            TraceDecayError::Io(std::io::Error::other(format!(
                "runtime telemetry clock precedes Unix epoch: {error}"
            )))
        })?
        .as_secs();
    Ok(RuntimeSnapshot {
        captured_at,
        tracedecay_version: crate::version::build_version().to_owned(),
        host_os: std::env::consts::OS.to_owned(),
        process,
        database,
    })
}

/// Render a `RuntimeSnapshot` as a human-readable status block for
/// terminals. Mirrors the structure of `tracedecay status` so it's
/// familiar to users running the CLI manually.
pub fn to_text_report(snap: &RuntimeSnapshot) -> String {
    let p = &snap.process;
    let d = &snap.database;
    let pct_of_system_mem = (p.rss_bytes as f64 / p.system_total_memory_bytes as f64) * 100.0;
    let generation_census = match &d.generation_census {
        GenerationCensusSnapshot::Observed { statistics } if statistics.source_total_bytes > 0 => {
            format!(
                "{} source / {} symbols / {} edges; db/source {:.1}×",
                bytes_human(statistics.source_total_bytes),
                statistics.symbol_count,
                statistics.edge_count,
                d.db_size_bytes as f64 / statistics.source_total_bytes as f64,
            )
        }
        GenerationCensusSnapshot::Observed { statistics } => format!(
            "{} source / {} symbols / {} edges; db/source unavailable",
            bytes_human(statistics.source_total_bytes),
            statistics.symbol_count,
            statistics.edge_count,
        ),
        GenerationCensusSnapshot::Unavailable { reason } => {
            format!("unavailable: {}", reason.as_str())
        }
    };
    let runtime_queue = format!(
        "{} shard ops / {}; global {} / {} budget",
        d.runtime_registry.aggregate.queued_operations,
        bytes_human(d.runtime_registry.aggregate.queued_bytes),
        d.runtime_registry
            .aggregate
            .global_queued_bytes
            .map_or_else(|| "unknown".to_owned(), bytes_human),
        bytes_human(d.runtime_registry.global_queue_max_bytes),
    );
    let runtime_contention = format!(
        "writer busy {}, readers waiting general {} / health {}",
        d.runtime_registry.aggregate.writer_busy_events,
        d.runtime_registry.aggregate.general_reader_waiters,
        d.runtime_registry.aggregate.health_reader_waiters,
    );
    let runtime_interrupts = format!(
        "cancelled {}, deadline {}, shed {}, conflicts {}; {}/{} active writers observed ({})",
        d.runtime_registry.aggregate.cancelled_operations,
        d.runtime_registry.aggregate.deadline_exceeded_operations,
        d.runtime_registry.aggregate.shed_operations,
        d.runtime_registry.aggregate.conflicted_operations,
        d.runtime_registry.aggregate.writer_telemetry_shards,
        d.runtime_registry.aggregate.writer_present,
        if d.runtime_registry.aggregate.writer_telemetry_complete {
            "complete"
        } else {
            "partial"
        },
    );
    let runtime_writer_time = format!(
        "queue {} µs / transaction {} µs across {} committed batches since current writer start",
        d.runtime_registry.aggregate.writer_queue_wait_micros,
        d.runtime_registry.aggregate.writer_transaction_micros,
        d.runtime_registry.aggregate.committed_batches,
    );
    let retained_runtimes = &d.session_runtime_retention;
    format!(
        "tracedecay {ver} runtime snapshot ({os})\n\
         ────────────────────────────────────────\n\
           pid              {pid}\n\
           rss              {rss}  ({rss_pct:.2}% of system)\n\
           virtual          {vsz}\n\
           cpu              {cpu:.1}% (sampled over {win}ms, {ncpu} CPUs)\n\
           uptime           {up}s\n\
           system memory    {sysmem}\n\
         \n\
           db file          {db}\n\
           db size          {dbsz}\n\
           wal size         {wal}\n\
           shm size         {shm}\n\
           journal mode     {jm}\n\
           synchronous      {sync}\n\
           page size        {page_size}\n\
           quick check      {quick_check}\n\
           dirty marker     {dirty}\n\
           generation census {generation_census}\n\
           readers general  {readers_general}\n\
           readers health   {readers_health}\n\
           runtime shards    {runtime_shards} observed ({runtime_opening} opening, {runtime_draining} draining, {runtime_omitted} detail omitted)\n\
           runtime queue     {runtime_queue}\n\
           runtime contention {runtime_contention}\n\
           runtime interrupts {runtime_interrupts}\n\
           runtime writer time {runtime_writer_time}\n\
           runtime wal       {runtime_wal}\n\
           session retention profiles memory/session {profile_memory}/{profile_sessions}; projects memory/session {project_memory}/{project_sessions} of {project_capacity}; reconciliation {reconciliation}; retired memory/session {retired_memory}/{retired_sessions}; refusals {retirement_refusals}\n\
        ",
        ver = snap.tracedecay_version,
        os = snap.host_os,
        pid = p.pid,
        rss = bytes_human(p.rss_bytes),
        rss_pct = pct_of_system_mem,
        vsz = bytes_human(p.virtual_bytes),
        cpu = p.cpu_percent,
        win = CPU_SAMPLE_WINDOW.as_millis(),
        ncpu = p.system_cpu_count,
        up = p.uptime_secs,
        sysmem = bytes_human(p.system_total_memory_bytes),
        db = d.db_path.display(),
        dbsz = bytes_human(d.db_size_bytes),
        wal = bytes_human(d.wal_size_bytes),
        shm = bytes_human(d.shm_size_bytes),
        jm = d.journal_mode.as_deref().unwrap_or("(unknown)"),
        sync = d
            .synchronous
            .map_or_else(|| "(unknown)".to_string(), |v| v.to_string()),
        page_size = d
            .page_size
            .map_or_else(|| "(unknown)".to_string(), |v| v.to_string()),
        quick_check = match (d.quick_check_ok, d.quick_check_error.as_deref()) {
            (Some(true), _) => "ok".to_string(),
            (Some(false), _) => "failed".to_string(),
            (None, Some(error)) => format!("unavailable: {error}"),
            (None, None) => "unavailable".to_string(),
        },
        dirty = if d.dirty_marker.exists {
            d.dirty_marker.state.as_deref().unwrap_or("unparsed")
        } else {
            "absent"
        },
        generation_census = generation_census,
        readers_general = d.reader_pool.as_ref().map_or_else(
            || "(unattached)".to_string(),
            |pool| lane_line(&pool.general)
        ),
        readers_health = d.reader_pool.as_ref().map_or_else(
            || "(unattached)".to_string(),
            |pool| lane_line(&pool.health)
        ),
        runtime_shards = d.runtime_registry.inventory_shards,
        runtime_opening = d.runtime_registry.aggregate.opening,
        runtime_draining = d.runtime_registry.aggregate.draining,
        runtime_omitted = d.runtime_registry.omitted_shards,
        runtime_queue = runtime_queue,
        runtime_contention = runtime_contention,
        runtime_interrupts = runtime_interrupts,
        runtime_writer_time = runtime_writer_time,
        runtime_wal = d
            .runtime_registry
            .aggregate
            .wal_bytes
            .map_or_else(|| "unknown".to_owned(), bytes_human),
        profile_memory = retained_runtimes.profile_memory_runtimes,
        profile_sessions = retained_runtimes.profile_session_runtimes,
        project_memory = retained_runtimes.project_memory_runtimes,
        project_sessions = retained_runtimes.project_session_runtimes,
        project_capacity = retained_runtimes.project_runtime_capacity,
        reconciliation = retained_runtimes.retained_memory_graph_reconciliation_tasks,
        retired_memory = retained_runtimes.retired_project_memory_runtimes,
        retired_sessions = retained_runtimes.retired_project_session_runtimes,
        retirement_refusals = retained_runtimes.retirement_refusals,
    )
}

/// One reader lane as a single terminal line. Waiters are the saturation
/// signal, so they are always shown rather than elided when zero.
fn lane_line(lane: &ReaderLaneOccupancy) -> String {
    format!(
        "{} workers ({} available, {} leased, {} limbo), {} waiting",
        lane.workers, lane.available, lane.leased, lane.limbo, lane.waiting
    )
}

// ---------------------------------------------------------------------------
// Process sampling
// ---------------------------------------------------------------------------

fn sample_process() -> Result<ProcessSnapshot> {
    sample_process_with_window(CPU_SAMPLE_WINDOW)
}

fn sample_process_with_window(cpu_sample_window: Duration) -> Result<ProcessSnapshot> {
    let pid = Pid::from_u32(std::process::id());

    // Refresh only *our own* process. The previous implementation passed
    // `.with_processes(..)` to `System::new_with_specifics`, which enumerates
    // and samples every process on the host — by far the heaviest part of the
    // reported Windows `tracedecay_runtime` crash (STATUS_STACK_OVERFLOW on a
    // host with a large process table). The primary fix for that crash is the
    // explicit-stack entrypoint in `main.rs` (`ASYNC_STACK_BYTES`: Windows
    // gives the main thread only 1 MiB); scoping the refresh to our PID
    // additionally bounds this handler's work and memory regardless of host
    // process count. `sample_process_fits_in_a_small_stack` guards the stack
    // footprint of this path.
    let refresh = ProcessRefreshKind::new().with_cpu().with_memory();
    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_memory(sysinfo::MemoryRefreshKind::new().with_ram())
            .with_cpu(sysinfo::CpuRefreshKind::new()),
    );
    // Two reads bracketing a sleep are required: sysinfo reports
    // `cpu_usage()` as the delta between successive refreshes.
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, refresh);
    std::thread::sleep(cpu_sample_window);
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, refresh);

    let process = sys.process(pid).ok_or_else(|| {
        TraceDecayError::Io(std::io::Error::other(
            "runtime telemetry could not sample the serving process",
        ))
    })?;
    let system_cpu_count = sys.cpus().len();
    let system_total_memory_bytes = sys.total_memory();
    if system_cpu_count == 0 || system_total_memory_bytes == 0 {
        return Err(TraceDecayError::Io(std::io::Error::other(
            "runtime telemetry could not sample host capacity",
        )));
    }

    Ok(ProcessSnapshot {
        pid: std::process::id(),
        rss_bytes: process.memory(),
        virtual_bytes: process.virtual_memory(),
        cpu_percent: process.cpu_usage(),
        uptime_secs: process.run_time(),
        system_cpu_count,
        system_total_memory_bytes,
    })
}

// ---------------------------------------------------------------------------
// Database sampling
// ---------------------------------------------------------------------------

pub(crate) async fn collect_database(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
) -> Result<DatabaseSnapshot> {
    collect_database_with_generation_census(cg, include_integrity, None).await
}

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
    let writer_owner = match crate::db::probe_writer_owner(&db_path) {
        Ok(crate::db::WriterOwnership::Idle) => WriterOwnerSnapshot::Idle,
        Ok(crate::db::WriterOwnership::Active(owner)) => WriterOwnerSnapshot::Active {
            pid: owner.pid,
            started_epoch_ms: owner.started_epoch_ms,
            version: owner.version,
            intent: owner.intent,
        },
        Ok(crate::db::WriterOwnership::ActiveUnknown) => WriterOwnerSnapshot::ActiveUnknown,
        Err(error) => WriterOwnerSnapshot::ProbeFailed {
            error: error.to_string(),
        },
    };
    let generation_census = match generation_census_reader {
        Some(reader) => reader().await,
        None => GenerationCensusSnapshot::Unavailable {
            reason: GenerationCensusUnavailableReason::AuthorityUnavailable,
        },
    };
    let reader_pool = cg
        .db()
        .conn()
        .reader_pool_occupancy()
        .as_ref()
        .map(ReaderPoolOccupancy::from_pool);
    let runtime_registry =
        RuntimeRegistrySnapshot::from_projection(cg.store_runtime_registry().runtime_telemetry());
    let retention = cg
        .store_runtime_registry()
        .session_runtime_retention_telemetry()
        .await?;
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
        session_runtime_retention: SessionRuntimeRetentionSnapshot {
            project_runtime_capacity: retention.project_runtime_capacity,
            profile_memory_runtimes: retention.profile_memory_runtimes,
            profile_session_runtimes: retention.profile_session_runtimes,
            project_memory_runtimes: retention.project_memory_runtimes,
            project_session_runtimes: retention.project_session_runtimes,
            retained_memory_graph_reconciliation_tasks: retention
                .retained_memory_graph_reconciliation_tasks,
            retired_project_memory_runtimes: retention.retired_project_memory_runtimes,
            retired_project_session_runtimes: retention.retired_project_session_runtimes,
            retirement_refusals: retention.retirement_refusals,
        },
    })
}

fn read_dirty_marker(path: &Path) -> DirtyMarkerSnapshot {
    let Ok(contents) = std::fs::read(path) else {
        return DirtyMarkerSnapshot {
            path: path.to_path_buf(),
            exists: path.exists(),
            parsed: false,
            owner_pid: None,
            epoch: None,
            state: None,
            schema: None,
        };
    };
    let value = serde_json::from_slice::<serde_json::Value>(&contents).ok();
    DirtyMarkerSnapshot {
        path: path.to_path_buf(),
        exists: true,
        parsed: value.is_some(),
        owner_pid: value
            .as_ref()
            .and_then(|marker| marker.pointer("/owner/pid"))
            .and_then(serde_json::Value::as_u64),
        epoch: value
            .as_ref()
            .and_then(|marker| marker.get("epoch"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        state: value
            .as_ref()
            .and_then(|marker| marker.get("state"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        schema: value
            .as_ref()
            .and_then(|marker| marker.get("schema"))
            .and_then(serde_json::Value::as_u64),
    }
}

fn file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

async fn read_journal_mode(cg: &crate::tracedecay::TraceDecay) -> Result<String> {
    let mut rows = cg
        .db()
        .conn()
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
    let mut rows =
        cg.db()
            .conn()
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a byte count as a short human-readable string (`353.2 MB`).
fn bytes_human(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn runtime_snapshot_deserializes_from_owned_transport_json() {
        fn require_owned_transport_decode<T: serde::de::DeserializeOwned>() {}

        require_owned_transport_decode::<RuntimeSnapshot>();
    }

    #[test]
    fn bytes_human_formats_units() {
        assert_eq!(bytes_human(0), "0 B");
        assert_eq!(bytes_human(512), "512 B");
        assert_eq!(bytes_human(2 * 1024), "2.0 KB");
        assert_eq!(bytes_human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(bytes_human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn with_suffix_appends_to_path() {
        let p = Path::new("/tmp/x.db");
        assert_eq!(with_suffix(p, "-wal"), Path::new("/tmp/x.db-wal"));
        assert_eq!(with_suffix(p, "-shm"), Path::new("/tmp/x.db-shm"));
    }

    #[test]
    fn dirty_marker_snapshot_parses_owner_epoch_and_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("graph.db.dirty");
        std::fs::write(
            &path,
            br#"{"schema":2,"owner":{"pid":42},"epoch":"epoch-7","state":"dirty"}"#,
        )
        .unwrap();

        let marker = read_dirty_marker(&path);
        assert!(marker.exists);
        assert!(marker.parsed);
        assert_eq!(marker.owner_pid, Some(42));
        assert_eq!(marker.epoch.as_deref(), Some("epoch-7"));
        assert_eq!(marker.state.as_deref(), Some("dirty"));
        assert_eq!(marker.schema, Some(2));
    }

    #[test]
    fn dirty_marker_snapshot_preserves_unparsed_presence() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("graph.db.dirty");
        std::fs::write(&path, b"legacy-dirty-marker").unwrap();

        let marker = read_dirty_marker(&path);
        assert!(marker.exists);
        assert!(!marker.parsed);
        assert_eq!(marker.state, None);
    }

    /// A saturated pool is the case the survey exists for, so the rendered
    /// report has to show where the workers went and who is queued behind
    /// them — not just a total.
    #[test]
    fn reader_lane_line_reports_occupancy_and_waiters() {
        let line = lane_line(&ReaderLaneOccupancy {
            workers: 8,
            available: 0,
            leased: 6,
            limbo: 2,
            waiting: 3,
        });

        assert_eq!(
            line,
            "8 workers (0 available, 6 leased, 2 limbo), 3 waiting"
        );
    }

    #[test]
    fn reader_pool_occupancy_projects_both_lanes_onto_the_wire() {
        let occupancy = ReaderPoolOccupancy::from_pool(&crate::db::engine::ReaderPoolSnapshot {
            state: crate::db::engine::ReaderPoolState::Draining,
            general_workers: 8,
            available_general: 1,
            health_workers: 1,
            available_health: 0,
            leased_general: 5,
            leased_health: 1,
            limbo_general: 2,
            limbo_health: 0,
            waiting_general: 4,
            waiting_health: 0,
            snapshot_admissions: 73,
        });
        let wire = serde_json::to_value(&occupancy).unwrap();

        assert_eq!(wire["state"], "draining");
        assert_eq!(wire["general"]["workers"], 8);
        assert_eq!(wire["general"]["limbo"], 2);
        assert_eq!(wire["general"]["waiting"], 4);
        assert_eq!(wire["health"]["leased"], 1);
        assert_eq!(wire["snapshot_admissions"], 73);
        // Every worker in a lane is accounted for by exactly one bucket.
        assert_eq!(
            occupancy.general.available + occupancy.general.leased + occupancy.general.limbo,
            occupancy.general.workers
        );
    }

    #[tokio::test]
    async fn production_database_snapshot_reports_session_runtime_retention() {
        let temporary = tempfile::tempdir().expect("runtime telemetry fixture");
        let project_root = temporary.path().join("project");
        let profile_root = temporary.path().join("profile");
        std::fs::create_dir_all(&project_root).expect("project root");
        gix::init(&project_root).expect("project repository");
        let graph = crate::tracedecay::TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: Some(profile_root.join("global.db")),
            },
        )
        .await
        .expect("production graph initialization");

        let snapshot = collect_database(&graph, false)
            .await
            .expect("production runtime telemetry snapshot");
        assert_eq!(
            snapshot.session_runtime_retention.project_runtime_capacity,
            u64::try_from(
                crate::daemon::store_runtime::session_registry::DEFAULT_RETAINED_PROJECT_RUNTIME_CAPACITY,
            )
            .expect("retention capacity fits telemetry wire range")
        );
        let serialized = serde_json::to_value(snapshot)
            .expect("session runtime retention uses the canonical telemetry wire snapshot");
        assert!(serialized["session_runtime_retention"].is_object());
        graph.close();
    }

    /// Regression guard for the Windows `STATUS_STACK_OVERFLOW` report against
    /// `tracedecay_runtime`: the process-sampling path must fit comfortably
    /// inside a stack far smaller than Windows' 1 MiB main-thread default.
    #[test]
    fn sample_process_fits_in_a_small_stack() {
        // Zero CPU sample window: the stack-footprint guard cares about the
        // sysinfo refresh path, not the CPU delta, so skip the 200 ms sleep.
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| sample_process_with_window(Duration::ZERO))
            .expect("spawn small-stack thread");
        let snap = handle
            .join()
            .expect("sample_process must not overflow a 512 KiB stack")
            .expect("sample_process must observe process and host capacity");
        assert_eq!(snap.pid, std::process::id());
    }
}
