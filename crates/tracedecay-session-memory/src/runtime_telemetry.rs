//! Runtime telemetry snapshot for diagnosing CPU/RAM regressions
//! (issue #80).
//!
//! Captures process-level resource use (RSS, virtual size, CPU%, thread
//! count) via [`sysinfo`] and database-level signals (sqlite + WAL + SHM
//! sizes, journal mode) so users hitting unexpected resource pressure
//! can attach a structured snapshot to a bug report.
//!
//! `cpu_percent` is the process-tick delta over [`CPU_SAMPLE_WINDOW`].
//! sysinfo 0.32 on Linux computes `cpu_usage()` only for
//! `ProcessesToUpdate::All`; a targeted PID refresh updates utime/stime
//! but leaves usage at 0. Linux therefore uses the same `/proc/self/stat`
//! utime+stime authority as semantic evaluation.
//!
//! The delta window is a real ~200 ms wall wait, so it never runs on the
//! serving path: a process-global background sampler owns the window and
//! the serving path reads the last completed sample from its cache. The
//! wire state is typed — `sampled` carries `sampled_at` so staleness is
//! visible, `not_yet_sampled` covers reads before the first sample
//! completes, and `sample_failed` carries the sampling error instead of a
//! fabricated zero snapshot.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use tracedecay_domain::errors::{Result, TraceDecayError};

mod store_runtime;

pub use store_runtime::{
    RuntimeRegistryAggregateSnapshot, RuntimeRegistryShardSnapshot, RuntimeRegistrySnapshot,
    RuntimeRegistryWriterSnapshot,
};

/// Async reader for the generation census attached by an exact daemon route.
pub type GenerationCensusFuture =
    Pin<Box<dyn Future<Output = GenerationCensusSnapshot> + Send + 'static>>;
pub type GenerationCensusReader = Arc<dyn Fn() -> GenerationCensusFuture + Send + Sync + 'static>;

/// Closed reasons for a generation census that cannot be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationCensusUnavailableReason {
    AuthorityUnavailable,
    ExactScopeGenerationNotReady,
    SealedGenerationCensusInvalid,
}

impl GenerationCensusUnavailableReason {
    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::ExactScopeGenerationNotReady => "exact_scope_generation_not_ready",
            Self::SealedGenerationCensusInvalid => "sealed_generation_census_invalid",
        }
    }
}

/// Aggregate facts from the exact sealed code-index generation, projected
/// onto runtime telemetry's own wire shape.
///
/// The daemon route that owns the census reader converts from
/// `tracedecay_code_index::production::CodeIndexGenerationStatisticsV1` at
/// that boundary, so this module carries no code-index dependency. The field
/// names are the wire contract (`#[serde(flatten)]` below) and must stay
/// byte-identical to the code-index statistics they mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationCensusStatistics {
    pub source_total_bytes: u64,
    pub symbol_count: u64,
    pub edge_count: u64,
}

/// Freshness of the exact graph generation whose census is reported.
///
/// This mirrors graph-query admission without importing the graph-query crate
/// across the telemetry boundary. Status therefore cannot present a stale
/// seated generation as current or report readiness for a generation graph
/// queries cannot open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GenerationCensusServingFreshness {
    Current,
    LastCompleteStale {
        sealed_at_micros: i64,
        rebuild_in_flight: bool,
    },
}

/// Runtime telemetry's truthful view of code-index census availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GenerationCensusSnapshot {
    Observed {
        generation_id: String,
        freshness: GenerationCensusServingFreshness,
        #[serde(flatten)]
        statistics: GenerationCensusStatistics,
    },
    Unavailable {
        reason: GenerationCensusUnavailableReason,
    },
}

/// Window over which `cpu_percent` is sampled. sysinfo's
/// `MINIMUM_CPU_UPDATE_INTERVAL` (200 ms) is the floor for a truthful CPU
/// delta, which is why the window runs on the background sampler instead of
/// the serving path.
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

/// Minimum age of a completed process sample before a read schedules a
/// background refresh. Reads inside this interval serve the cached sample.
const PROCESS_SAMPLE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Captured process + database telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Captured at (Unix epoch seconds).
    pub captured_at: u64,
    /// `tracedecay` build version (e.g. `6.0.0`).
    pub tracedecay_version: String,
    /// Host OS short name (`macos`, `linux`, `windows`, …).
    pub host_os: String,
    pub process: ProcessTelemetry,
    pub database: DatabaseSnapshot,
}

/// Typed state of the cached process sample as served on the status path.
///
/// The serving path never blocks on the ~200 ms CPU delta window: it reads
/// the last completed background sample. `sampled_at` makes staleness
/// visible instead of presenting an old sample as fresh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessTelemetry {
    Sampled {
        /// When the background sample completed (Unix epoch seconds).
        sampled_at: u64,
        #[serde(flatten)]
        snapshot: ProcessSnapshot,
    },
    /// No background sample has completed yet (first request in this
    /// process schedules one).
    NotYetSampled,
    /// The most recent background sample attempt failed.
    SampleFailed { error: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Projects the kernel reader-pool snapshot onto the wire shape.
    pub fn from_pool(snapshot: &tracedecay_runtime_core::db::engine::ReaderPoolSnapshot) -> Self {
        Self {
            state: match snapshot.state {
                tracedecay_runtime_core::db::engine::ReaderPoolState::Ready => "ready".to_string(),
                tracedecay_runtime_core::db::engine::ReaderPoolState::Draining => {
                    "draining".to_string()
                }
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
        /// Unix epoch milliseconds. Kept at `u64` on purpose: the owner
        /// record stores `u128`, but `serde_json::to_value` cannot represent
        /// a 128-bit integer, and one active writer used to collapse the
        /// whole runtime payload into `{}` (doctor then reported "omitted
        /// database telemetry").
        started_epoch_ms: u64,
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

/// Render a `RuntimeSnapshot` as a human-readable status block for
/// terminals. Mirrors the structure of `tracedecay status` so it's
/// familiar to users running the CLI manually.
pub fn to_text_report(snap: &RuntimeSnapshot) -> String {
    let process_block = process_text_block(&snap.process);
    let d = &snap.database;
    let generation_census = match &d.generation_census {
        GenerationCensusSnapshot::Observed { statistics, .. }
            if statistics.source_total_bytes > 0 =>
        {
            format!(
                "{} source / {} symbols / {} edges; db/source {:.1}×",
                bytes_human(statistics.source_total_bytes),
                statistics.symbol_count,
                statistics.edge_count,
                d.db_size_bytes as f64 / statistics.source_total_bytes as f64,
            )
        }
        GenerationCensusSnapshot::Observed { statistics, .. } => format!(
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
    format!(
        "tracedecay {ver} runtime snapshot ({os})\n\
         ────────────────────────────────────────\n\
         {process_block}\
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
        ",
        ver = snap.tracedecay_version,
        os = snap.host_os,
        process_block = process_block,
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

/// The process section of the text report, one line per field, matching the
/// typed cache state instead of fabricating zeros before the first sample.
fn process_text_block(process: &ProcessTelemetry) -> String {
    match process {
        ProcessTelemetry::Sampled {
            sampled_at,
            snapshot: p,
        } => {
            let pct_of_system_mem =
                (p.rss_bytes as f64 / p.system_total_memory_bytes as f64) * 100.0;
            format!(
                "pid              {pid}\n\
                 rss              {rss}  ({rss_pct:.2}% of system)\n\
                 virtual          {vsz}\n\
                 cpu              {cpu:.1}% (sampled over {win}ms, {ncpu} CPUs)\n\
                 sampled at       {sampled_at} (Unix epoch seconds)\n\
                 uptime           {up}s\n\
                 system memory    {sysmem}\n",
                pid = p.pid,
                rss = bytes_human(p.rss_bytes),
                rss_pct = pct_of_system_mem,
                vsz = bytes_human(p.virtual_bytes),
                cpu = p.cpu_percent,
                win = CPU_SAMPLE_WINDOW.as_millis(),
                ncpu = p.system_cpu_count,
                sampled_at = sampled_at,
                up = p.uptime_secs,
                sysmem = bytes_human(p.system_total_memory_bytes),
            )
        }
        ProcessTelemetry::NotYetSampled => {
            "process          not yet sampled (background sample pending)\n".to_string()
        }
        ProcessTelemetry::SampleFailed { error } => {
            format!("process          sample failed: {error}\n")
        }
    }
}

// ---------------------------------------------------------------------------
// Process sampling
// ---------------------------------------------------------------------------

pub fn unix_epoch_secs() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            TraceDecayError::Io(std::io::Error::other(format!(
                "runtime telemetry clock precedes Unix epoch: {error}"
            )))
        })?
        .as_secs())
}

type ProcessSampleFn = Arc<dyn Fn() -> Result<ProcessSnapshot> + Send + Sync>;

/// Last completed background sample attempt plus when it completed, for the
/// refresh-interval gate.
struct CompletedProcessSample {
    completed: Instant,
    telemetry: ProcessTelemetry,
}

#[derive(Default)]
struct ProcessSampleCache {
    outcome: Option<CompletedProcessSample>,
    sample_in_flight: bool,
}

/// Owns the ~200 ms CPU delta window on a background thread and serves the
/// last completed sample to the status path.
///
/// [`ProcessSampler::read`] never waits on sampling: it returns the cached
/// [`ProcessTelemetry`] and, when no sample is in flight and the cache is
/// older than the refresh interval (or empty), schedules one background
/// sample whose result serves later reads. The delta window itself stays at
/// [`CPU_SAMPLE_WINDOW`] so CPU percentages remain truthful.
struct ProcessSampler {
    sample: ProcessSampleFn,
    refresh_interval: Duration,
    cache: Arc<Mutex<ProcessSampleCache>>,
}

/// The cache critical sections only move small values, so a poisoned lock
/// (a panic on the sampler thread mid-store) leaves consistent data; recover
/// the guard instead of propagating an untyped panic to the serving path.
fn lock_cache(cache: &Mutex<ProcessSampleCache>) -> MutexGuard<'_, ProcessSampleCache> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProcessSampler {
    fn new(sample: ProcessSampleFn, refresh_interval: Duration) -> Self {
        Self {
            sample,
            refresh_interval,
            cache: Arc::new(Mutex::new(ProcessSampleCache::default())),
        }
    }

    /// Read the cached process sample without blocking on the sampler.
    fn read(&self) -> ProcessTelemetry {
        let mut cache = lock_cache(&self.cache);
        let needs_refresh = !cache.sample_in_flight
            && cache
                .outcome
                .as_ref()
                .is_none_or(|sample| sample.completed.elapsed() >= self.refresh_interval);
        let telemetry = cache
            .outcome
            .as_ref()
            .map_or(ProcessTelemetry::NotYetSampled, |sample| {
                sample.telemetry.clone()
            });
        if needs_refresh {
            cache.sample_in_flight = true;
            drop(cache);
            self.spawn_background_sample();
        }
        telemetry
    }

    fn spawn_background_sample(&self) {
        let sample = Arc::clone(&self.sample);
        let cache = Arc::clone(&self.cache);
        let spawned = std::thread::Builder::new()
            .name("tracedecay-process-sample".to_string())
            .spawn(move || {
                let telemetry = match sample() {
                    Ok(snapshot) => match unix_epoch_secs() {
                        Ok(sampled_at) => ProcessTelemetry::Sampled {
                            sampled_at,
                            snapshot,
                        },
                        Err(error) => ProcessTelemetry::SampleFailed {
                            error: error.to_string(),
                        },
                    },
                    Err(error) => ProcessTelemetry::SampleFailed {
                        error: error.to_string(),
                    },
                };
                let mut cache = lock_cache(&cache);
                cache.outcome = Some(CompletedProcessSample {
                    completed: Instant::now(),
                    telemetry,
                });
                cache.sample_in_flight = false;
            });
        if let Err(error) = spawned {
            let mut cache = lock_cache(&self.cache);
            cache.outcome = Some(CompletedProcessSample {
                completed: Instant::now(),
                telemetry: ProcessTelemetry::SampleFailed {
                    error: format!("could not spawn the process sample thread: {error}"),
                },
            });
            cache.sample_in_flight = false;
        }
    }
}

/// Process-global sampler: the sample observes this process as a whole, so
/// one cache serves every project route in the daemon.
fn process_sampler() -> &'static ProcessSampler {
    static SAMPLER: OnceLock<ProcessSampler> = OnceLock::new();
    SAMPLER.get_or_init(|| {
        ProcessSampler::new(Arc::new(sample_process), PROCESS_SAMPLE_REFRESH_INTERVAL)
    })
}

/// Reads the last completed background process sample without blocking on
/// the CPU delta window, scheduling a background refresh when stale.
pub fn read_cached_process_sample() -> ProcessTelemetry {
    process_sampler().read()
}

fn sample_process() -> Result<ProcessSnapshot> {
    sample_process_with_window(CPU_SAMPLE_WINDOW)
}

/// Linux process CPU ticks (`utime + stime`) from `/proc/self/stat`.
///
/// Shared with semantic evaluation so runtime and quality samples use one
/// process-tick authority.
pub fn read_linux_process_cpu_ticks() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
        let fields = fields.collect::<Vec<_>>();
        let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
        let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
        user_ticks.checked_add(system_ticks)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub fn linux_clock_ticks_per_second() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        static TICKS_PER_SECOND: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        if let Some(ticks) = TICKS_PER_SECOND.get() {
            return Some(*ticks);
        }
        let output = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let ticks = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ticks| *ticks != 0)?;
        let _ = TICKS_PER_SECOND.set(ticks);
        Some(ticks)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn cpu_percent_from_linux_ticks(
    start_ticks: u64,
    end_ticks: u64,
    elapsed: Duration,
    ticks_per_second: u64,
) -> Option<f32> {
    let wall_secs = elapsed.as_secs_f64();
    if wall_secs <= 0.0 || ticks_per_second == 0 {
        return Some(0.0);
    }
    let cpu_secs = end_ticks.saturating_sub(start_ticks) as f64 / ticks_per_second as f64;
    Some((cpu_secs / wall_secs * 100.0) as f32)
}

#[hotpath::measure(label = "usecases.runtime_telemetry.process_sample")]
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
    // Memory and identity still come from a targeted PID refresh. Linux
    // `cpu_usage()` stays 0 after `ProcessesToUpdate::Some`; CPU% is the
    // `/proc/self/stat` tick delta over the same window.
    let start_ticks = read_linux_process_cpu_ticks();
    let sampled_at = Instant::now();
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, refresh);
    std::thread::sleep(cpu_sample_window);
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, refresh);
    let elapsed = sampled_at.elapsed();
    let end_ticks = read_linux_process_cpu_ticks();

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
    let cpu_percent = match (start_ticks, end_ticks, linux_clock_ticks_per_second()) {
        (Some(start), Some(end), Some(ticks_per_second)) => {
            cpu_percent_from_linux_ticks(start, end, elapsed, ticks_per_second).ok_or_else(
                || {
                    TraceDecayError::Io(std::io::Error::other(
                        "runtime telemetry could not convert Linux process ticks to CPU percent",
                    ))
                },
            )?
        }
        _ if cfg!(target_os = "linux") => {
            return Err(TraceDecayError::Io(std::io::Error::other(
                "runtime telemetry could not read Linux process ticks",
            )));
        }
        _ => process.cpu_usage(),
    };

    Ok(ProcessSnapshot {
        pid: std::process::id(),
        rss_bytes: process.memory(),
        virtual_bytes: process.virtual_memory(),
        cpu_percent,
        uptime_secs: process.run_time(),
        system_cpu_count,
        system_total_memory_bytes,
    })
}

// ---------------------------------------------------------------------------
// Database file sampling
// ---------------------------------------------------------------------------

/// Reads and parses a store dirty-marker file into its typed snapshot.
pub fn read_dirty_marker(path: &Path) -> DirtyMarkerSnapshot {
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

/// Size of a store file, treating a missing file as zero bytes.
pub fn file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Appends a suffix (`-wal`, `-shm`, `.dirty`) to a database path.
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// `serde_json::to_value` refuses 128-bit integers, and the runtime tool
    /// builds its payload through it; an active writer owner must therefore
    /// stay representable or the whole status collapses to `{}`.
    #[test]
    fn active_writer_owner_serializes_to_a_json_value() {
        let owner = WriterOwnerSnapshot::Active {
            pid: 4242,
            started_epoch_ms: 1_788_500_000_000,
            version: "0.1.0-test".to_owned(),
            intent: "daemon".to_owned(),
        };
        let value = serde_json::to_value(&owner).expect("active writer owner must serialize");
        assert_eq!(value["state"], "active");
        assert_eq!(value["started_epoch_ms"], 1_788_500_000_000_u64);
    }

    fn test_process_snapshot() -> ProcessSnapshot {
        ProcessSnapshot {
            pid: std::process::id(),
            rss_bytes: 42 * 1024,
            virtual_bytes: 84 * 1024,
            cpu_percent: 12.5,
            uptime_secs: 7,
            system_cpu_count: 4,
            system_total_memory_bytes: 8 * 1024 * 1024 * 1024,
        }
    }

    /// Poll until the sampler serves a completed background sample.
    fn wait_for_completed_sample(sampler: &ProcessSampler) -> ProcessTelemetry {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match sampler.read() {
                ProcessTelemetry::NotYetSampled => {}
                completed => return completed,
            }
            assert!(
                Instant::now() < deadline,
                "background process sample never completed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The serving path must not wait on the CPU delta window: the sample
    /// function blocks on a gate that is only released *after* the reads
    /// return, so a blocking read would deadlock this test instead of
    /// passing.
    #[test]
    fn read_returns_typed_not_yet_sampled_while_the_sampler_is_blocked() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let sampler = ProcessSampler::new(
            Arc::new(move || {
                gate.lock()
                    .unwrap()
                    .recv()
                    .map_err(|_| TraceDecayError::Io(std::io::Error::other("gate closed")))?;
                Ok(test_process_snapshot())
            }),
            Duration::from_mins(1),
        );

        assert_eq!(sampler.read(), ProcessTelemetry::NotYetSampled);
        assert_eq!(sampler.read(), ProcessTelemetry::NotYetSampled);

        release.send(()).unwrap();
        match wait_for_completed_sample(&sampler) {
            ProcessTelemetry::Sampled {
                sampled_at,
                snapshot,
            } => {
                assert!(sampled_at > 0, "sampled_at must be a real timestamp");
                assert_eq!(snapshot, test_process_snapshot());
            }
            other => panic!("expected a completed sample, got {other:?}"),
        }
    }

    #[test]
    fn reads_within_the_refresh_interval_serve_the_cached_sample() {
        let samples = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&samples);
        let sampler = ProcessSampler::new(
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(test_process_snapshot())
            }),
            Duration::from_mins(1),
        );

        assert_eq!(sampler.read(), ProcessTelemetry::NotYetSampled);
        let first = wait_for_completed_sample(&sampler);
        for _ in 0..5 {
            assert_eq!(sampler.read(), first);
        }
        assert_eq!(
            samples.load(Ordering::SeqCst),
            1,
            "reads inside the refresh interval must not resample"
        );
    }

    /// Once the cache is warm, a stale read serves the last sample
    /// immediately and refreshes in the background — it never regresses to
    /// blocking or to `NotYetSampled`.
    #[test]
    fn stale_reads_serve_the_previous_sample_and_refresh_in_the_background() {
        let samples = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&samples);
        let sampler = ProcessSampler::new(
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(test_process_snapshot())
            }),
            Duration::ZERO,
        );

        assert_eq!(sampler.read(), ProcessTelemetry::NotYetSampled);
        wait_for_completed_sample(&sampler);
        assert!(
            matches!(sampler.read(), ProcessTelemetry::Sampled { .. }),
            "a warm cache must serve the previous sample on stale reads"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while samples.load(Ordering::SeqCst) < 2 {
            assert!(
                Instant::now() < deadline,
                "stale read never scheduled a background refresh"
            );
            let _ = sampler.read();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_failed_sample_is_typed_not_a_fabricated_snapshot() {
        let sampler = ProcessSampler::new(
            Arc::new(|| {
                Err(TraceDecayError::Io(std::io::Error::other(
                    "process table unreadable",
                )))
            }),
            Duration::from_mins(1),
        );

        assert_eq!(sampler.read(), ProcessTelemetry::NotYetSampled);
        match wait_for_completed_sample(&sampler) {
            ProcessTelemetry::SampleFailed { error } => {
                assert!(error.contains("process table unreadable"));
            }
            other => panic!("expected a typed sample failure, got {other:?}"),
        }
    }

    /// The daemon serializes `process` and `tracedecay status --runtime`
    /// deserializes the same Rust type; the sampled variant keeps the flat
    /// field layout existing consumers read.
    #[test]
    fn process_telemetry_round_trips_the_cli_runtime_decode() {
        let states = [
            ProcessTelemetry::Sampled {
                sampled_at: 9,
                snapshot: test_process_snapshot(),
            },
            ProcessTelemetry::NotYetSampled,
            ProcessTelemetry::SampleFailed {
                error: "sampler unavailable".to_string(),
            },
        ];
        for state in states {
            let wire = serde_json::to_value(&state).unwrap();
            let decoded: ProcessTelemetry = serde_json::from_value(wire).unwrap();
            assert_eq!(decoded, state);
        }

        let wire = serde_json::to_value(ProcessTelemetry::Sampled {
            sampled_at: 9,
            snapshot: test_process_snapshot(),
        })
        .unwrap();
        assert_eq!(wire["state"], "sampled");
        assert_eq!(wire["sampled_at"], 9);
        assert_eq!(wire["pid"], u64::from(std::process::id()));
        assert!(wire["rss_bytes"].is_u64());
    }

    #[test]
    fn text_report_names_the_pending_and_failed_sample_states() {
        assert!(process_text_block(&ProcessTelemetry::NotYetSampled).contains("not yet sampled"));
        assert!(
            process_text_block(&ProcessTelemetry::SampleFailed {
                error: "sampler unavailable".to_string(),
            })
            .contains("sample failed: sampler unavailable")
        );
        let sampled = process_text_block(&ProcessTelemetry::Sampled {
            sampled_at: 1_700_000_000,
            snapshot: test_process_snapshot(),
        });
        assert!(sampled.contains("sampled at       1700000000"));
        assert!(sampled.contains("cpu              12.5%"));
    }

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
        let occupancy = ReaderPoolOccupancy::from_pool(
            &tracedecay_runtime_core::db::engine::ReaderPoolSnapshot {
                state: tracedecay_runtime_core::db::engine::ReaderPoolState::Draining,
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
            },
        );
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

    #[test]
    fn cpu_percent_from_linux_ticks_is_cpu_seconds_over_wall() {
        assert_eq!(
            cpu_percent_from_linux_ticks(10, 110, Duration::from_secs(1), 100),
            Some(100.0)
        );
        assert_eq!(
            cpu_percent_from_linux_ticks(0, 50, Duration::from_secs(2), 100),
            Some(25.0)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_runtime_cpu_percent_follows_proc_tick_delta_not_sysinfo_pid_refresh() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut acc = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    acc = acc.wrapping_mul(1_664_525).wrapping_add(1);
                }
                std::hint::black_box(acc);
            })
        };
        let start_ticks = read_linux_process_cpu_ticks().expect("procfs ticks before sample");
        let sysinfo_cpu = {
            let pid = Pid::from_u32(std::process::id());
            let refresh = ProcessRefreshKind::new().with_cpu().with_memory();
            let mut sys = System::new_with_specifics(
                RefreshKind::new()
                    .with_memory(sysinfo::MemoryRefreshKind::new().with_ram())
                    .with_cpu(sysinfo::CpuRefreshKind::new()),
            );
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                refresh,
            );
            std::thread::sleep(Duration::from_millis(200));
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                refresh,
            );
            sys.process(pid)
                .expect("sysinfo must still see this pid")
                .cpu_usage()
        };
        let snap = sample_process_with_window(Duration::from_millis(200))
            .expect("runtime CPU sample must observe the serving process");
        let end_ticks = read_linux_process_cpu_ticks().expect("procfs ticks after sample");
        stop.store(true, Ordering::Relaxed);
        worker.join().expect("busy worker");

        assert!(
            end_ticks > start_ticks,
            "tick authority must observe work during the sample window ({start_ticks} -> {end_ticks})"
        );
        assert!(
            sysinfo_cpu.abs() < f32::EPSILON,
            "sysinfo 0.32 Linux leaves cpu_usage at 0 after Some(&[pid]); that field is not CPU authority (observed {sysinfo_cpu})"
        );
        assert!(
            snap.cpu_percent > 0.0,
            "cpu_percent must come from /proc tick deltas, not sysinfo cpu_usage() after Some(&[pid]); got {}",
            snap.cpu_percent
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
            .expect("sample_process must not overflow a 512 KiB stack")
            .expect("sample_process must observe process and host capacity");
        assert_eq!(snap.pid, std::process::id());
    }
}
