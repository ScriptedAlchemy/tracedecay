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

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use crate::errors::{Result, TraceDecayError};

/// Window over which `cpu_percent` is sampled.
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

/// Captured process + database telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Captured at (Unix epoch seconds).
    pub captured_at: u64,
    /// `tracedecay` build version (e.g. `6.0.0`).
    pub tracedecay_version: &'static str,
    /// Host OS short name (`macos`, `linux`, `windows`, …).
    pub host_os: &'static str,
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
    /// `<root>/.tracedecay/<branch>.db` or whichever DB is being served.
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
    /// Total source size we've indexed, from the `files` table sum, in
    /// bytes — useful to compute the "DB / source" ratio.
    pub source_total_bytes: u64,
    /// Total node + edge counts. Lets the user compare DB bloat to
    /// graph size — a 25× ratio with a tiny graph is suspicious.
    pub node_count: u64,
    pub edge_count: u64,
    /// Live reader-pool occupancy for this store, when the pool is attached.
    ///
    /// Reader saturation is the failure users actually hit — a query reports
    /// `reader acquisition saturated` and there is otherwise nothing to look
    /// at. This is the after-the-fact evidence: where the workers went, and
    /// whether anyone is queued behind them.
    pub reader_pool: Option<ReaderPoolOccupancy>,
    /// Bounded daemon store-runtime registry telemetry for all mounted shards.
    pub runtime_registry: RuntimeRegistrySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistrySnapshot {
    pub inventory_shards: u32,
    pub returned_shards: u32,
    pub omitted_shards: u32,
    pub per_shard_queue_max_operations: u32,
    pub per_shard_queue_max_bytes: u64,
    pub global_queue_max_bytes: u64,
    pub wal_soft_limit_bytes: u64,
    pub wal_hard_limit_bytes: u64,
    pub aggregate: RuntimeRegistryAggregateSnapshot,
    pub shards: Vec<RuntimeRegistryShardSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistryAggregateSnapshot {
    pub ready: u32,
    pub opening: u32,
    pub draining: u32,
    pub exclusive_maintenance: u32,
    pub reopening: u32,
    pub faulted: u32,
    pub closed: u32,
    pub healthy: u32,
    pub degraded: u32,
    pub unknown_health: u32,
    pub pinned_profiles: u32,
    pub eviction_eligible: u32,
    pub writer_present: u32,
    pub physical_reader_handles: u64,
    pub general_reader_waiters: u64,
    pub health_reader_waiters: u64,
    pub writer_busy_events: u64,
    pub queued_operations: u64,
    pub queued_bytes: u64,
    pub total_leases: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub global_queued_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistryShardSnapshot {
    pub shard: String,
    pub incarnation: u64,
    pub authority_epoch: u64,
    pub state: String,
    pub health: String,
    pub writer_present: bool,
    pub physical_reader_handles: u32,
    pub general_reader_waiters: u16,
    pub health_reader_waiters: u16,
    pub writer_busy_events: u64,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub total_leases: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub pinned_profile: bool,
    pub idle_for_ms: u64,
    pub eviction_eligible: bool,
    pub eviction_blocker_count: u32,
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
/// mode. Both are best-effort — failures degrade to zeroes / `None`
/// rather than failing the whole snapshot, because the value of this
/// tool is recording *what's available* during a spike.
pub async fn collect(cg: &crate::tracedecay::TraceDecay) -> Result<RuntimeSnapshot> {
    collect_with_integrity(cg, false).await
}

pub async fn collect_with_integrity(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
) -> Result<RuntimeSnapshot> {
    let process = sample_process();
    let database = collect_database(cg, include_integrity).await?;
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(RuntimeSnapshot {
        captured_at,
        tracedecay_version: crate::version::build_version(),
        host_os: std::env::consts::OS,
        process,
        database,
    })
}

/// Render a `RuntimeSnapshot` as the JSON wire shape used by both the
/// CLI (`--json` flag) and the MCP tool result.
pub fn to_pretty_json(snap: &RuntimeSnapshot) -> String {
    serde_json::to_string_pretty(snap).unwrap_or_default()
}

/// Render a `RuntimeSnapshot` as a human-readable status block for
/// terminals. Mirrors the structure of `tracedecay status` so it's
/// familiar to users running the CLI manually.
pub fn to_text_report(snap: &RuntimeSnapshot) -> String {
    let p = &snap.process;
    let d = &snap.database;
    let pct_of_system_mem = if p.system_total_memory_bytes > 0 {
        (p.rss_bytes as f64 / p.system_total_memory_bytes as f64) * 100.0
    } else {
        0.0
    };
    let bloat_ratio = if d.source_total_bytes > 0 {
        d.db_size_bytes as f64 / d.source_total_bytes as f64
    } else {
        0.0
    };
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
           source indexed   {src}\n\
           db / source      {ratio:.1}×\n\
           nodes / edges    {nodes} / {edges}\n\
           readers general  {readers_general}\n\
           readers health   {readers_health}\n\
           runtime shards    {runtime_shards} mounted ({runtime_omitted} omitted)\n\
           runtime queue     {runtime_queue}\n\
           runtime contention {runtime_contention}\n\
           runtime wal       {runtime_wal}\n\
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
        src = bytes_human(d.source_total_bytes),
        ratio = bloat_ratio,
        nodes = d.node_count,
        edges = d.edge_count,
        readers_general = d.reader_pool.as_ref().map_or_else(
            || "(unattached)".to_string(),
            |pool| lane_line(&pool.general)
        ),
        readers_health = d.reader_pool.as_ref().map_or_else(
            || "(unattached)".to_string(),
            |pool| lane_line(&pool.health)
        ),
        runtime_shards = d.runtime_registry.inventory_shards,
        runtime_omitted = d.runtime_registry.omitted_shards,
        runtime_queue = format!(
            "{} ops / {}",
            d.runtime_registry.aggregate.queued_operations,
            bytes_human(d.runtime_registry.aggregate.queued_bytes)
        ),
        runtime_contention = format!(
            "writer busy {}, readers waiting general {} / health {}",
            d.runtime_registry.aggregate.writer_busy_events,
            d.runtime_registry.aggregate.general_reader_waiters,
            d.runtime_registry.aggregate.health_reader_waiters,
        ),
        runtime_wal = bytes_human(d.runtime_registry.aggregate.wal_bytes),
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

fn sample_process() -> ProcessSnapshot {
    sample_process_with_window(CPU_SAMPLE_WINDOW)
}

fn sample_process_with_window(cpu_sample_window: Duration) -> ProcessSnapshot {
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

    let proc = sys.process(pid);
    let rss_bytes = proc.map_or(0, sysinfo::Process::memory);
    let virtual_bytes = proc.map_or(0, sysinfo::Process::virtual_memory);
    let cpu_percent = proc.map_or(0.0, sysinfo::Process::cpu_usage);
    let uptime_secs = proc.map_or(0, sysinfo::Process::run_time);

    ProcessSnapshot {
        pid: std::process::id(),
        rss_bytes,
        virtual_bytes,
        cpu_percent,
        uptime_secs,
        system_cpu_count: sys.cpus().len(),
        system_total_memory_bytes: sys.total_memory(),
    }
}

// ---------------------------------------------------------------------------
// Database sampling
// ---------------------------------------------------------------------------

pub(crate) async fn collect_database(
    cg: &crate::tracedecay::TraceDecay,
    include_integrity: bool,
) -> Result<DatabaseSnapshot> {
    let project_root = cg.project_root().to_path_buf();
    let db_path = cg.db_path().clone();
    let canonical_db_path = db_path.canonicalize().unwrap_or_else(|_| db_path.clone());
    let db_size_bytes = file_size(&db_path);
    let wal_size_bytes = file_size(&with_suffix(&db_path, "-wal"));
    let shm_size_bytes = file_size(&with_suffix(&db_path, "-shm"));
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
    let source_total_bytes = read_source_total_bytes(cg).await?;
    let (node_count, edge_count) = read_graph_counts(cg).await?;
    let reader_pool = cg
        .db()
        .conn()
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
        source_total_bytes,
        node_count,
        edge_count,
        reader_pool,
        runtime_registry,
    })
}

impl RuntimeRegistrySnapshot {
    fn from_projection(
        projection: crate::daemon::store_runtime::telemetry::RuntimeTelemetryProjection,
    ) -> Self {
        let aggregate = &projection.aggregate;
        let shards = projection
            .shards
            .iter()
            .map(RuntimeRegistryShardSnapshot::from_telemetry)
            .collect();
        Self {
            inventory_shards: aggregate.inventory_shards,
            returned_shards: aggregate.returned_shards,
            omitted_shards: aggregate.omitted_shards,
            per_shard_queue_max_operations: projection.per_shard_queue_budget.max_operations,
            per_shard_queue_max_bytes: projection.per_shard_queue_budget.max_bytes,
            global_queue_max_bytes: projection.global_queue_budget_bytes,
            wal_soft_limit_bytes: projection.wal_budget.soft_limit_bytes,
            wal_hard_limit_bytes: projection.wal_budget.hard_limit_bytes,
            aggregate: RuntimeRegistryAggregateSnapshot {
                ready: aggregate.states.ready,
                opening: aggregate.states.opening,
                draining: aggregate.states.draining,
                exclusive_maintenance: aggregate.states.exclusive_maintenance,
                reopening: aggregate.states.reopening,
                faulted: aggregate.states.faulted,
                closed: aggregate.states.closed,
                healthy: aggregate.health.healthy,
                degraded: aggregate.health.degraded,
                unknown_health: aggregate.health.unknown,
                pinned_profiles: aggregate.pinned_profiles,
                eviction_eligible: aggregate.eviction_eligible,
                writer_present: aggregate.writer_present,
                physical_reader_handles: aggregate.physical_reader_handles,
                general_reader_waiters: aggregate.general_reader_waiters,
                health_reader_waiters: aggregate.health_reader_waiters,
                writer_busy_events: aggregate.writer_busy_events,
                queued_operations: aggregate.queued_operations,
                queued_bytes: aggregate.queued_bytes,
                total_leases: aggregate.total_leases,
                wal_bytes: aggregate.wal_bytes,
                memory_estimate_bytes: aggregate.memory_estimate_bytes,
                global_queued_bytes: aggregate.global_queued_bytes,
            },
            shards,
        }
    }
}

impl RuntimeRegistryShardSnapshot {
    fn from_telemetry(
        telemetry: &crate::daemon::store_runtime::telemetry::ShardRuntimeTelemetry,
    ) -> Self {
        Self {
            shard: format!("{:?}", telemetry.binding.shard_id.scope),
            incarnation: telemetry.binding.incarnation.get(),
            authority_epoch: telemetry.binding.authority_epoch.get(),
            state: runtime_state_label(telemetry.state).to_string(),
            health: runtime_health_label(telemetry.health).to_string(),
            writer_present: telemetry.writer_present,
            physical_reader_handles: telemetry.physical_reader_handles,
            general_reader_waiters: telemetry.general_reader_waiters,
            health_reader_waiters: telemetry.health_reader_waiters,
            writer_busy_events: telemetry.writer_busy_events,
            queued_operations: telemetry.queued_operations,
            queued_bytes: telemetry.queued_bytes,
            total_leases: u64::from(telemetry.leases.general_readers)
                .saturating_add(u64::from(telemetry.leases.health_readers))
                .saturating_add(u64::from(telemetry.leases.snapshots))
                .saturating_add(u64::from(telemetry.leases.watchers))
                .saturating_add(u64::from(telemetry.leases.schedulers))
                .saturating_add(u64::from(telemetry.leases.clients)),
            wal_bytes: telemetry.wal_bytes,
            memory_estimate_bytes: telemetry.memory_estimate_bytes,
            pinned_profile: telemetry.pinned_profile,
            idle_for_ms: telemetry.idle_for_ms,
            eviction_eligible: telemetry.eviction_eligible,
            eviction_blocker_count: telemetry.eviction_blocker_count,
        }
    }
}

fn runtime_state_label(state: tracedecay_store::RuntimeMaintenanceStateV1) -> &'static str {
    match state {
        tracedecay_store::RuntimeMaintenanceStateV1::Closed => "closed",
        tracedecay_store::RuntimeMaintenanceStateV1::Opening => "opening",
        tracedecay_store::RuntimeMaintenanceStateV1::Ready => "ready",
        tracedecay_store::RuntimeMaintenanceStateV1::Draining => "draining",
        tracedecay_store::RuntimeMaintenanceStateV1::ExclusiveMaintenance => {
            "exclusive_maintenance"
        }
        tracedecay_store::RuntimeMaintenanceStateV1::Reopening => "reopening",
        tracedecay_store::RuntimeMaintenanceStateV1::Faulted => "faulted",
    }
}

fn runtime_health_label(
    health: crate::daemon::store_runtime::shard::ShardRuntimeHealth,
) -> &'static str {
    match health {
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Unknown => "unknown",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Healthy => "healthy",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Degraded => "degraded",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Faulted => "faulted",
    }
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

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
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

async fn read_source_total_bytes(cg: &crate::tracedecay::TraceDecay) -> Result<u64> {
    let mut rows = cg
        .db()
        .conn()
        .query("SELECT COALESCE(SUM(size), 0) FROM files", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to sum source bytes: {e}"),
            operation: "read_source_total_bytes".to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read source-sum row: {e}"),
            operation: "read_source_total_bytes".to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "no source-sum row returned".to_string(),
            operation: "read_source_total_bytes".to_string(),
        })?;
    let v: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
        message: format!("failed to decode source-sum: {e}"),
        operation: "read_source_total_bytes".to_string(),
    })?;
    Ok(u64::try_from(v).unwrap_or(0))
}

async fn read_graph_counts(cg: &crate::tracedecay::TraceDecay) -> Result<(u64, u64)> {
    let nodes = scalar_count(cg, "SELECT COUNT(*) FROM nodes").await?;
    let edges = scalar_count(cg, "SELECT COUNT(*) FROM edges").await?;
    Ok((nodes, edges))
}

async fn scalar_count(cg: &crate::tracedecay::TraceDecay, sql: &str) -> Result<u64> {
    let mut rows = cg
        .db()
        .conn()
        .query(sql, ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("scalar query failed: {e}"),
            operation: "scalar_count".to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("scalar row read failed: {e}"),
            operation: "scalar_count".to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "no scalar row".to_string(),
            operation: "scalar_count".to_string(),
        })?;
    let v: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
        message: format!("scalar decode failed: {e}"),
        operation: "scalar_count".to_string(),
    })?;
    Ok(u64::try_from(v).unwrap_or(0))
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
        });
        let wire = serde_json::to_value(&occupancy).unwrap();

        assert_eq!(wire["state"], "draining");
        assert_eq!(wire["general"]["workers"], 8);
        assert_eq!(wire["general"]["limbo"], 2);
        assert_eq!(wire["general"]["waiting"], 4);
        assert_eq!(wire["health"]["leased"], 1);
        // Every worker in a lane is accounted for by exactly one bucket.
        assert_eq!(
            occupancy.general.available + occupancy.general.leased + occupancy.general.limbo,
            occupancy.general.workers
        );
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
            .expect("sample_process must not overflow a 512 KiB stack");
        assert_eq!(snap.pid, std::process::id());
    }
}
