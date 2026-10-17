//! Store telemetry sampling and retention progress.
//!
//! Shared by the daemon maintenance loop and diagnostic projections. The
//! registry is a concrete owner, not a port.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::storage::{
    StorageByteSizeV1, StorageTelemetryFuture, StorageTelemetryReadV1, StoreKeyV1,
    StoreSizeSampleV1, StoreSizeTelemetryPort, TableGrowthBaselinePendingV1, TableGrowthSampleV1,
    TableGrowthTelemetryReadV1, TableGrowthUnavailableReasonV1, TableNameV1,
};
use tracedecay_contracts::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestAdmission, RequestContext, ResolvedScope, now_micros,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_runtime_core::db::DatabaseStorageTelemetryHandle;
use tracedecay_runtime_core::native_runtime;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::tick::MaintenanceTickOutcome;

const STORAGE_TELEMETRY_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;
const STORAGE_TELEMETRY_CAPABILITY: &str = "capability.application.storage.telemetry";
const STORAGE_TELEMETRY_USE_CASE: &str = "use-case.application.storage.telemetry.read";

const TABLE_GROWTH_STATE_DIRECTORY: &str = "maintenance";
const TABLE_GROWTH_STATE_FILE: &str = "table-growth-v1.json";
/// How long a walk that ran out of its deadline suppresses the next one.
const TABLE_SIZE_WALK_REPROBE_MICROS: i64 = 24 * 60 * 60 * 1_000_000;

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize)]
struct TableWatermark {
    bytes: StorageByteSizeV1,
    observed_at: UtcMicros,
}

/// The store size at which a per-table `dbstat` walk last ran out of its
/// deadline.
///
/// The walk reads every page of every table, so its cost tracks the store
/// size: a multi-gigabyte session store needs minutes. Once a walk has run
/// out of time, repeating it on a store at least as large only spends the
/// deadline again. A smaller store, or a day-old mark, is walked again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct OverBudgetTableSizeWalk {
    page_count: u64,
    observed_at: UtcMicros,
}

impl OverBudgetTableSizeWalk {
    fn still_applies(self, page_count: u64, now: UtcMicros) -> bool {
        page_count >= self.page_count
            && now.0.saturating_sub(self.observed_at.0) < TABLE_SIZE_WALK_REPROBE_MICROS
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TableSizeWalk {
    Measured(Vec<(String, u64)>),
    OverBudget(OverBudgetTableSizeWalk),
    Skipped,
}

/// A walk that ran out of its deadline marks the store over budget; any other
/// failure stays a plain unavailable reason.
fn settle_table_size_walk(
    walked: Result<Vec<(String, u64)>, TableGrowthUnavailableReasonV1>,
    page_count: u64,
    observed_at: UtcMicros,
) -> Result<TableSizeWalk, TableGrowthUnavailableReasonV1> {
    match walked {
        Ok(tables) => Ok(TableSizeWalk::Measured(tables)),
        Err(TableGrowthUnavailableReasonV1::DeadlineExceeded) => {
            Ok(TableSizeWalk::OverBudget(OverBudgetTableSizeWalk {
                page_count,
                observed_at,
            }))
        }
        Err(reason) => Err(reason),
    }
}

/// One store's table-growth baseline and the latest maintenance measurement.
///
/// Persisted per store database path so a restarted daemon compares against
/// the watermarks it had and reports the last measurement instead of
/// re-baselining every store.
#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
struct TableGrowthStateV1 {
    watermarks: Option<BTreeMap<TableNameV1, TableWatermark>>,
    latest: Option<TableGrowthTelemetryReadV1>,
    over_budget: Option<OverBudgetTableSizeWalk>,
}

/// Store telemetry bound to the database's guarded read capability.
///
/// The runtime-core handle retains the exact database client that issued it;
/// this daemon adapter must not unwrap that guard into a raw SQL handle just to
/// retain the maintenance-owned table-growth baseline.
#[derive(Clone)]
pub struct GuardedStoreTelemetryPort {
    handle: DatabaseStorageTelemetryHandle,
    store: StoreKeyV1,
    scope: ResolvedScope,
    reader_wait: Duration,
    table_growth: Arc<std::sync::Mutex<TableGrowthStateV1>>,
}

impl GuardedStoreTelemetryPort {
    fn new(
        handle: DatabaseStorageTelemetryHandle,
        store: StoreKeyV1,
        scope: ResolvedScope,
        reader_wait: Duration,
        table_growth: TableGrowthStateV1,
    ) -> Self {
        Self {
            handle,
            store,
            scope,
            reader_wait,
            table_growth: Arc::new(std::sync::Mutex::new(table_growth)),
        }
    }

    fn table_growth_state(&self) -> TableGrowthStateV1 {
        self.table_growth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn admits(&self, context: &RequestContext, store: &StoreKeyV1) -> bool {
        context.validate().is_ok()
            && context.scope() == &self.scope
            && store == &self.store
            && context.admission_at(now_micros()) == RequestAdmission::Admitted
    }

    fn for_scope(&self, scope: ResolvedScope) -> Self {
        Self {
            handle: self.handle.clone(),
            store: self.store.clone(),
            scope,
            reader_wait: self.reader_wait,
            table_growth: Arc::clone(&self.table_growth),
        }
    }

    fn rebind(&self, handle: DatabaseStorageTelemetryHandle, scope: ResolvedScope) -> Self {
        Self {
            handle,
            store: self.store.clone(),
            scope,
            reader_wait: self.reader_wait,
            table_growth: Arc::clone(&self.table_growth),
        }
    }

    /// The table growth the last maintenance pass measured.
    ///
    /// Reading never walks `dbstat`: the walk costs time proportional to the
    /// store, so it runs once per maintenance pass and every reader shares
    /// that result. `Unknown` until a pass has advanced this store.
    pub fn latest_table_growth(
        &self,
        context: &RequestContext,
        store: &StoreKeyV1,
    ) -> TableGrowthTelemetryReadV1 {
        if !self.admits(context, store) {
            return TableGrowthTelemetryReadV1::Denied {
                store: store.clone(),
            };
        }
        self.table_growth_state()
            .latest
            .unwrap_or_else(|| TableGrowthTelemetryReadV1::Unknown {
                store: store.clone(),
                reason: TableGrowthUnavailableReasonV1::NotYetMeasured,
            })
    }

    async fn advance_table_growth(
        &self,
        context: &RequestContext,
        store: &StoreKeyV1,
    ) -> TableGrowthTelemetryReadV1 {
        let unknown = |reason| TableGrowthTelemetryReadV1::Unknown {
            store: store.clone(),
            reason,
        };
        let handle = self.handle.clone();
        let reader_wait = self.reader_wait;
        let context = context.clone();
        let over_budget = self.table_growth_state().over_budget;
        let walk = native_runtime::spawn_blocking(move || {
            let interrupted = || telemetry_interruption(&context);
            let page_count = handle
                .store_size_telemetry(reader_wait, interrupted)
                .map_err(|error| error.table_growth_unavailable_reason())?
                .page_count;
            if over_budget.is_some_and(|walk| walk.still_applies(page_count, now_micros())) {
                return Ok(TableSizeWalk::Skipped);
            }
            let walked = handle
                .table_size_telemetry(reader_wait, interrupted)
                .map(|samples| {
                    samples
                        .into_iter()
                        .map(|sample| (sample.table_name, sample.bytes))
                        .collect()
                })
                .map_err(|error| error.table_growth_unavailable_reason());
            settle_table_size_walk(walked, page_count, now_micros())
        })
        .await;
        let current = match walk {
            Ok(Ok(TableSizeWalk::Measured(current))) => current,
            Ok(Ok(TableSizeWalk::Skipped)) => {
                return unknown(TableGrowthUnavailableReasonV1::ExceedsMeasurementBudget);
            }
            Ok(Ok(TableSizeWalk::OverBudget(walk))) => {
                self.table_growth
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .over_budget = Some(walk);
                return unknown(TableGrowthUnavailableReasonV1::ExceedsMeasurementBudget);
            }
            Ok(Err(reason)) => return unknown(reason),
            Err(_) => return unknown(TableGrowthUnavailableReasonV1::StorageOther),
        };
        let observed_at = now_micros();
        let mut current_tables = BTreeMap::new();
        for (table_name, bytes) in current {
            let Ok(table) = TableNameV1::new(table_name) else {
                return unknown(TableGrowthUnavailableReasonV1::InvalidTableName);
            };
            current_tables.insert(table, StorageByteSizeV1(bytes));
        }
        let mut state = self
            .table_growth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.over_budget = None;
        compare_table_growth(store, current_tables, observed_at, &mut state.watermarks)
    }
}

impl StoreSizeTelemetryPort for GuardedStoreTelemetryPort {
    fn store_size<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, StorageTelemetryReadV1> {
        Box::pin(tracing::Instrument::instrument(
            async move {
                if !self.admits(context, store) {
                    return StorageTelemetryReadV1::Denied {
                        store: store.clone(),
                    };
                }
                let handle = self.handle.clone();
                let reader_wait = self.reader_wait;
                let context = context.clone();
                let Ok(Ok(sample)) = native_runtime::spawn_blocking(move || {
                    handle.store_size_telemetry(reader_wait, || telemetry_interruption(&context))
                })
                .await
                else {
                    return StorageTelemetryReadV1::Unknown {
                        store: store.clone(),
                    };
                };
                let sample = StoreSizeSampleV1 {
                    store: store.clone(),
                    page_size_bytes: sample.page_size_bytes,
                    page_count: sample.page_count,
                    freelist_pages: sample.freelist_pages,
                    observed_at: now_micros(),
                };
                if sample.validate().is_err() {
                    return StorageTelemetryReadV1::Unknown {
                        store: store.clone(),
                    };
                }
                StorageTelemetryReadV1::Observed { sample }
            },
            tracing::trace_span!("daemon.maintenance.read_store_size"),
        ))
    }

    fn table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        Box::pin(tracing::Instrument::instrument(
            async move {
                if !self.admits(context, store) {
                    return TableGrowthTelemetryReadV1::Denied {
                        store: store.clone(),
                    };
                }
                let read = self.advance_table_growth(context, store).await;
                self.table_growth
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .latest = Some(read.clone());
                read
            },
            tracing::trace_span!("daemon.maintenance.read_table_growth"),
        ))
    }
}

#[tracing::instrument(
    name = "daemon.maintenance.compare_table_growth",
    level = "trace",
    skip_all
)]
fn compare_table_growth(
    store: &StoreKeyV1,
    current_tables: BTreeMap<TableNameV1, StorageByteSizeV1>,
    observed_at: UtcMicros,
    watermarks: &mut Option<BTreeMap<TableNameV1, TableWatermark>>,
) -> TableGrowthTelemetryReadV1 {
    let Some(previous_watermarks) = watermarks.as_ref() else {
        let tables_observed = u64::try_from(current_tables.len()).unwrap_or(u64::MAX);
        *watermarks = Some(
            current_tables
                .into_iter()
                .map(|(table, bytes)| (table, TableWatermark { bytes, observed_at }))
                .collect(),
        );
        return TableGrowthTelemetryReadV1::BaselineEstablished {
            store: store.clone(),
            observed_at,
            tables_observed,
        };
    };

    let mut growth = Vec::new();
    let mut baseline_pending = Vec::new();
    for (table, current_bytes) in &current_tables {
        if let Some(previous) = previous_watermarks.get(table) {
            let sample = TableGrowthSampleV1 {
                store: store.clone(),
                table: table.clone(),
                previous_bytes: previous.bytes,
                current_bytes: *current_bytes,
                previous_observed_at: previous.observed_at,
                current_observed_at: observed_at,
            };
            if sample.validate().is_err() {
                return TableGrowthTelemetryReadV1::Unknown {
                    store: store.clone(),
                    reason: TableGrowthUnavailableReasonV1::ClockRegressed,
                };
            }
            growth.push(sample);
        } else {
            baseline_pending.push(TableGrowthBaselinePendingV1 {
                store: store.clone(),
                table: table.clone(),
                current_bytes: *current_bytes,
                observed_at,
            });
        }
    }
    *watermarks = Some(
        current_tables
            .into_iter()
            .map(|(table, bytes)| (table, TableWatermark { bytes, observed_at }))
            .collect(),
    );
    TableGrowthTelemetryReadV1::Observed {
        store: store.clone(),
        samples: growth,
        baseline_pending,
    }
}

fn telemetry_interruption(
    context: &RequestContext,
) -> Option<tracedecay_store::UnavailableReasonV1> {
    match context.admission_at(now_micros()) {
        RequestAdmission::Admitted => None,
        RequestAdmission::Cancelled => Some(tracedecay_store::UnavailableReasonV1::Cancelled),
        RequestAdmission::TimedOut => Some(tracedecay_store::UnavailableReasonV1::DeadlineExceeded),
        RequestAdmission::GrantExpired => {
            Some(tracedecay_store::UnavailableReasonV1::MissingAuthority)
        }
    }
}

#[derive(Clone)]
struct CachedStoreTelemetryPort {
    scope: ResolvedScope,
    store: StoreKeyV1,
    port: GuardedStoreTelemetryPort,
    /// The file the store path named when the port was mounted; `None` when
    /// it could not be read then.
    file_identity: Option<StoreFileIdentityV1>,
}

/// Whether a mounted store path still names the file the daemon opened.
///
/// A store deleted or replaced while the daemon holds it open keeps taking
/// writes into the unlinked inode: no size census sees them and the space is
/// never freed until the daemon closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreFileIdentityStateV1 {
    Current,
    Missing,
    Replaced,
    Unverifiable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StoreFileIdentityV1 {
    device: u64,
    file: u64,
}

#[cfg(unix)]
fn store_file_identity(path: &Path) -> std::io::Result<StoreFileIdentityV1> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    Ok(StoreFileIdentityV1 {
        device: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
fn store_file_identity(path: &Path) -> std::io::Result<StoreFileIdentityV1> {
    let information =
        tracedecay_private_fs::windows_file::information(&std::fs::File::open(path)?)?;
    Ok(StoreFileIdentityV1 {
        device: u64::from(information.volume_serial_number),
        file: information.file_index,
    })
}

#[cfg(not(any(unix, windows)))]
fn store_file_identity(_path: &Path) -> std::io::Result<StoreFileIdentityV1> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn store_file_identity_state(
    path: &Path,
    mounted: Option<StoreFileIdentityV1>,
) -> StoreFileIdentityStateV1 {
    match (mounted, store_file_identity(path)) {
        (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            StoreFileIdentityStateV1::Missing
        }
        (None, _) | (_, Err(_)) => StoreFileIdentityStateV1::Unverifiable,
        (Some(mounted), Ok(current)) if mounted == current => StoreFileIdentityStateV1::Current,
        (Some(_), Ok(_)) => StoreFileIdentityStateV1::Replaced,
    }
}

/// Daemon-owned table-growth baseline authority shared by maintenance and
/// read-only diagnostic projections.
#[derive(Clone, Default)]
pub struct StoreTelemetrySamplingRegistry {
    ports: Arc<std::sync::Mutex<HashMap<PathBuf, CachedStoreTelemetryPort>>>,
    /// Where table-growth state survives a restart; `None` keeps it in memory.
    table_growth_state_path: Option<PathBuf>,
    graph_replay_release: Arc<std::sync::Mutex<HashMap<PathBuf, GraphReplayReleaseProgressV1>>>,
    graph_staging_release:
        Arc<std::sync::Mutex<HashMap<PathBuf, tracedecay_store::GraphProjectionIdentityV1>>>,
    /// Whether the by-design quiet `retry` tick line has already been written.
    /// A persistent quiet retry logs once, then counts on
    /// [`daemon.git.maintenance.retention_quiet_total`]; a loud anomaly or an
    /// outcome change emits again.
    quiet_retry_tick_logged: Arc<AtomicBool>,
    loud_retention_this_tick: Arc<AtomicBool>,
}

/// Longest run of short-cadence ticks a project's graph-replay release
/// reconcile may be skipped after consecutive unhealthy attempts. At the
/// one-minute retry cadence this bounds post-recovery release latency to
/// roughly eight minutes while a wedged runtime is probed a handful of times
/// per hour instead of once per tick.
const GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS: u32 = 8;

/// Per-project reconcile state for the graph-replay release queue.
///
/// Release evidence is durable on disk, so none of this state guards
/// correctness: the cursor makes the queue walk incremental across ticks
/// (retained entries stop blocking later pages), and the backoff window
/// converts "retry a known-wedged graph runtime every tick" into a bounded
/// re-probe. Losing the state (restart, project retirement) only means the
/// next attempt starts from the front of the queue immediately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GraphReplayReleaseProgressV1 {
    consecutive_unhealthy: u32,
    skip_remaining: u32,
    cursor: Option<String>,
}

#[derive(Clone, Copy, Default)]
pub struct StoreTelemetrySamplingOutcome {
    pub observed: u64,
    pub unavailable: u64,
}

impl StoreTelemetrySamplingRegistry {
    /// A registry whose table-growth state persists under the profile's
    /// maintenance directory.
    #[must_use]
    pub fn persisted_under(profile_root: &Path) -> Self {
        Self {
            table_growth_state_path: Some(
                profile_root
                    .join(TABLE_GROWTH_STATE_DIRECTORY)
                    .join(TABLE_GROWTH_STATE_FILE),
            ),
            ..Self::default()
        }
    }

    pub fn register_port<E>(
        &self,
        path: &Path,
        scope: &ResolvedScope,
        open: impl FnOnce() -> Result<DatabaseStorageTelemetryHandle, E>,
    ) -> bool {
        let Some(store_name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return false;
        };
        let Ok(store) = StoreKeyV1::new(store_name.to_owned()) else {
            return false;
        };
        let Ok(handle) = open() else {
            return false;
        };
        let mut ports = self
            .ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = ports.get_mut(path) {
            cached.scope = scope.clone();
            cached.store = store;
            cached.port = cached.port.rebind(handle, scope.clone());
            cached.file_identity = store_file_identity(path).ok();
            return true;
        }
        let persisted = self
            .table_growth_state_path
            .as_deref()
            .zip(path.to_str())
            .and_then(|(state_path, key)| load_table_growth_states(state_path).remove(key))
            .unwrap_or_default();
        let port = GuardedStoreTelemetryPort::new(
            handle,
            store.clone(),
            scope.clone(),
            Duration::from_secs(5),
            persisted,
        );
        ports.insert(
            path.to_path_buf(),
            CachedStoreTelemetryPort {
                scope: scope.clone(),
                store,
                port,
                file_identity: store_file_identity(path).ok(),
            },
        );
        true
    }

    /// Stat every mounted store path now and compare it with the file mounted
    /// there: one `stat` per store.
    #[must_use]
    pub fn check_store_identities(&self) -> Vec<(PathBuf, StoreFileIdentityStateV1)> {
        let mounted: Vec<_> = self
            .ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(path, cached)| (path.clone(), cached.file_identity))
            .collect();
        mounted
            .into_iter()
            .map(|(path, identity)| {
                let state = store_file_identity_state(&path, identity);
                (path, state)
            })
            .collect()
    }

    pub fn registered_port(
        &self,
        path: &Path,
        scope: &ResolvedScope,
    ) -> Option<(StoreKeyV1, GuardedStoreTelemetryPort)> {
        let ports = self
            .ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cached = ports.get(path)?;
        Some((cached.store.clone(), cached.port.for_scope(scope.clone())))
    }

    /// Release the telemetry client's exact database lease before the owning
    /// project store is retired. Other project and profile sampling ports stay
    /// mounted.
    pub fn release_retained_handle(&self, path: &Path) {
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
    }

    pub fn release_retained_handles_for_shutdown(&self) {
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.quiet_retry_tick_logged.store(false, Ordering::Release);
        self.loud_retention_this_tick
            .store(false, Ordering::Release);
    }

    pub fn retain_project_maintenance_state(&self, active_projects: &BTreeSet<PathBuf>) {
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
        self.graph_staging_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
    }

    /// Whether this tick may attempt the graph-replay release reconcile.
    ///
    /// Consecutive unhealthy attempts open a bounded skip window; each denied
    /// tick burns one unit of it, so a wedged runtime is re-probed after at
    /// most `GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS` short-cadence ticks
    /// rather than being polled (and timing out) on every one.
    pub fn graph_replay_release_attempt_admitted(&self, project_root: &Path) -> bool {
        let mut progress = self
            .graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = progress.get_mut(project_root) else {
            return true;
        };
        if state.skip_remaining == 0 {
            return true;
        }
        state.skip_remaining -= 1;
        false
    }

    /// Record a release attempt the graph runtime could not serve (deadline,
    /// unavailability, or a held replay pool) and widen the skip window:
    /// 1, 2, 4, then capped at `GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS`.
    pub fn record_graph_replay_release_unhealthy(&self, project_root: &Path) {
        let mut progress = self
            .graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = progress.entry(project_root.to_path_buf()).or_default();
        state.consecutive_unhealthy = state.consecutive_unhealthy.saturating_add(1);
        state.skip_remaining = GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS
            .min(1_u32 << state.consecutive_unhealthy.saturating_sub(1).min(3));
    }

    /// Record a served release attempt: close the skip window and advance the
    /// durable-queue cursor to `continuation` (`None` restarts from the front
    /// of the queue on the next attempt).
    pub fn record_graph_replay_release_served(
        &self,
        project_root: &Path,
        continuation: Option<String>,
    ) {
        let mut progress = self
            .graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match progress.entry(project_root.to_path_buf()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if continuation.is_none() {
                    entry.remove();
                } else {
                    *entry.get_mut() = GraphReplayReleaseProgressV1 {
                        consecutive_unhealthy: 0,
                        skip_remaining: 0,
                        cursor: continuation,
                    };
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                if continuation.is_some() {
                    entry.insert(GraphReplayReleaseProgressV1 {
                        consecutive_unhealthy: 0,
                        skip_remaining: 0,
                        cursor: continuation,
                    });
                }
            }
        }
    }

    /// The release-queue cursor recorded by the last served attempt.
    pub fn graph_replay_release_cursor(&self, project_root: &Path) -> Option<String> {
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(|state| state.cursor.clone())
    }

    pub fn graph_staging_release_cursor(
        &self,
        project_root: &Path,
    ) -> Option<tracedecay_store::GraphProjectionIdentityV1> {
        self.graph_staging_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()
    }

    pub fn record_graph_staging_release_cursor(
        &self,
        project_root: &Path,
        cursor: Option<tracedecay_store::GraphProjectionIdentityV1>,
    ) {
        let mut cursors = self
            .graph_staging_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cursor) = cursor {
            cursors.insert(project_root.to_path_buf(), cursor);
        } else {
            cursors.remove(project_root);
        }
    }

    /// Open a fresh per-tick loud-vs-quiet window before any retention pass
    /// emits. A genuine anomaly during the tick keeps the tick line loud.
    pub fn begin_retention_tick_log_window(&self) {
        self.loud_retention_this_tick
            .store(false, Ordering::Release);
    }

    /// Mark that this tick emitted a genuine retention anomaly. By-design
    /// unavailable pins stay quiet; this forces the tick summary to stay loud.
    pub fn mark_loud_retention_log(&self) {
        self.loud_retention_this_tick.store(true, Ordering::Release);
    }

    /// Whether the tick summary line should be written. Repeated by-design
    /// `retry` ticks stay quiet and count on
    /// `daemon.git.maintenance.retention_quiet_total`; a loud anomaly or an
    /// outcome change logs.
    pub fn admit_retention_tick_log(&self, outcome: MaintenanceTickOutcome) -> bool {
        let loud = self.loud_retention_this_tick.load(Ordering::Acquire);
        if matches!(outcome, MaintenanceTickOutcome::Retry) && !loud {
            if self.quiet_retry_tick_logged.swap(true, Ordering::AcqRel) {
                return false;
            }
            return true;
        }
        self.quiet_retry_tick_logged.store(false, Ordering::Release);
        true
    }

    #[tracing::instrument(
        name = "daemon.maintenance.sample_store_telemetry",
        level = "trace",
        skip_all
    )]
    pub async fn advance_registered(
        &self,
        active_paths: &BTreeSet<PathBuf>,
        sampled_paths: &BTreeSet<PathBuf>,
        cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    ) -> StoreTelemetrySamplingOutcome {
        let ports = {
            let mut ports = self
                .ports
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ports.retain(|path, _| active_paths.contains(path));
            ports
                .iter()
                .filter(|(path, _)| sampled_paths.contains(*path))
                .map(|(path, cached)| (path.clone(), cached.clone()))
                .collect::<Vec<_>>()
        };
        let mut outcome = StoreTelemetrySamplingOutcome::default();
        for (_, cached) in &ports {
            if cancellation.is_cancelled() {
                break;
            }
            let Ok(context) = storage_telemetry_request_context(cached.scope.clone()) else {
                outcome.unavailable = outcome.unavailable.saturating_add(1);
                continue;
            };
            match cached.port.table_growth(&context, &cached.store).await {
                TableGrowthTelemetryReadV1::BaselineEstablished { .. }
                | TableGrowthTelemetryReadV1::Observed { .. } => {
                    outcome.observed = outcome.observed.saturating_add(1);
                }
                TableGrowthTelemetryReadV1::Unsupported { .. }
                | TableGrowthTelemetryReadV1::Denied { .. }
                | TableGrowthTelemetryReadV1::Unknown { .. } => {
                    outcome.unavailable = outcome.unavailable.saturating_add(1);
                }
            }
        }
        if let Some(state_path) = self.table_growth_state_path.clone() {
            let states = ports
                .iter()
                .filter_map(|(path, cached)| {
                    Some((path.to_str()?.to_owned(), cached.port.table_growth_state()))
                })
                .collect::<Vec<_>>();
            let persisted = native_runtime::spawn_blocking(move || {
                persist_table_growth_states(&state_path, states)
            })
            .await;
            let failure = match persisted {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(error) => Some(error),
            };
            if let Some(error) = failure {
                tracing::warn!(
                    target: "tracedecay::storage_telemetry",
                    error = %error,
                    "table growth state was not persisted; a restart re-baselines it"
                );
            }
        }
        outcome
    }
}

/// Every persisted store state, keyed by database path. A missing or
/// unreadable file is an empty state: the next pass re-baselines.
fn load_table_growth_states(state_path: &Path) -> BTreeMap<String, TableGrowthStateV1> {
    std::fs::read(state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Merge `updates` into the persisted state and drop stores whose database no
/// longer exists, so retired stores do not accumulate.
#[tracing::instrument(
    name = "daemon.maintenance.persist_table_growth",
    level = "trace",
    skip_all
)]
fn persist_table_growth_states(
    state_path: &Path,
    updates: Vec<(String, TableGrowthStateV1)>,
) -> std::io::Result<()> {
    let mut states = load_table_growth_states(state_path);
    states.extend(updates);
    states.retain(|database_path, _| Path::new(database_path).exists());
    let bytes = serde_json::to_vec(&states).map_err(std::io::Error::other)?;
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracedecay_private_fs::framed_log::atomic_write_accelerator(state_path, "table-growth", &bytes)
}

#[tracing::instrument(
    name = "daemon.maintenance.mint_telemetry_context",
    level = "trace",
    skip_all
)]
fn storage_telemetry_request_context(
    scope: ResolvedScope,
) -> Result<RequestContext, ApplicationContractError> {
    let observed_at = now_micros();
    let expires_at = tracedecay_domain::UtcMicros(
        observed_at
            .0
            .saturating_add(STORAGE_TELEMETRY_CONTEXT_HORIZON_MICROS),
    );
    let request_id =
        mint_global_request_id(GlobalRequestSurface::DaemonStorageTelemetry).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "storage telemetry request identity",
            }
        })?;
    let suffix = request_id.as_str().to_owned();
    let actor = tracedecay_domain::ActorId::new("actor.tracedecay-daemon-storage-telemetry")?;
    let capability = CapabilityId::new(STORAGE_TELEMETRY_CAPABILITY.to_owned())?;
    let use_case = UseCaseId::new(STORAGE_TELEMETRY_USE_CASE.to_owned())?;
    let manifest: ManifestDigest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.storage-telemetry-grant.v1",
        &scope,
        &capability,
        &use_case,
        expires_at,
    ))?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.storage-telemetry.{suffix}"))?,
        1,
        manifest,
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )?;
    RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at)?,
        CancellationContext::active(format!("cancel.daemon.storage-telemetry.{suffix}"))?,
    )
}

#[cfg(test)]
mod retention_tick_log_tests {
    use super::StoreTelemetrySamplingRegistry;
    use crate::tick::{MaintenanceContinuation, MaintenanceTickOutcome};

    #[test]
    fn quiet_retry_ticks_log_once_until_a_loud_anomaly_or_outcome_change() {
        let registry = StoreTelemetrySamplingRegistry::default();
        registry.begin_retention_tick_log_window();
        assert!(registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
        assert!(!registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
        assert!(!registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));

        registry.mark_loud_retention_log();
        assert!(registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
        registry.begin_retention_tick_log_window();
        assert!(registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
        assert!(!registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));

        assert!(
            registry.admit_retention_tick_log(MaintenanceTickOutcome::Continue(
                MaintenanceContinuation::CodeGenerationRetention,
            ))
        );
        assert!(registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
        assert!(!registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));

        registry.release_retained_handles_for_shutdown();
        assert!(registry.admit_retention_tick_log(MaintenanceTickOutcome::Retry));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod table_growth_tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::time::Duration;

    use futures_util::FutureExt;
    use tracedecay_contracts::ResolvedScope;
    use tracedecay_contracts::now_micros;
    use tracedecay_contracts::storage::{
        StorageTelemetryReadV1, StoreKeyV1, StoreSizeTelemetryPort, TableGrowthDoctorEvidenceV1,
        TableGrowthTelemetryReadV1, TableGrowthUnavailableReasonV1, table_growth_doctor_evidence,
    };
    use tracedecay_domain::UtcMicros;
    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracedecay_runtime_core::db::Database;

    use super::{
        GuardedStoreTelemetryPort, OverBudgetTableSizeWalk, StoreTelemetrySamplingRegistry,
        TableGrowthStateV1, TableSizeWalk, settle_table_size_walk,
        storage_telemetry_request_context,
    };

    const GROWN_TABLE_BYTES: u64 = 80 * 1024 * 1024;

    pub(super) async fn growth_fixture(database_path: &Path) -> (Database, ResolvedScope) {
        tracedecay_global_db::register_registered_schema_installer();
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            database_path,
            "maintenance table growth fixture",
        )
        .expect("fixture database authority");
        let (database, _) = Database::publish_test_runtime(
            database_path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("fixture database");
        database
            .execute_write_batch(
                "create table growth fixture",
                "CREATE TABLE growth_payload (payload BLOB NOT NULL);",
            )
            .await
            .expect("growth table");
        let scope = ResolvedScope::new(
            tracedecay_domain::ProjectId::new("project.table-growth").expect("project identity"),
            tracedecay_domain::RepositoryId::new("repository.table-growth")
                .expect("repository identity"),
            tracedecay_domain::WorktreeId::new("worktree.table-growth").expect("worktree identity"),
            None,
        )
        .expect("resolved scope");
        (database, scope)
    }

    #[asupersync::test]
    async fn readers_see_the_growth_the_maintenance_pass_measured_on_a_large_store() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let database_path = temporary.path().join("project.db");
        let (database, scope) = growth_fixture(&database_path).await;
        let registry = StoreTelemetrySamplingRegistry::default();
        assert!(registry.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        let paths = BTreeSet::from([database_path.clone()]);
        let context = storage_telemetry_request_context(scope.clone()).expect("reader context");
        let (store, port) = registry
            .registered_port(&database_path, &scope)
            .expect("registered port");
        assert!(
            matches!(
                port.latest_table_growth(&context, &store),
                TableGrowthTelemetryReadV1::Unknown {
                    reason: TableGrowthUnavailableReasonV1::NotYetMeasured,
                    ..
                }
            ),
            "nothing is measured before the first maintenance pass"
        );

        let baseline = registry
            .advance_registered(&paths, &paths, &CancellationToken::new())
            .await;
        assert_eq!(
            baseline.observed, 1,
            "the first pass establishes a baseline"
        );
        database
            .execute_write_batch(
                "grow table growth fixture",
                "WITH RECURSIVE megabyte(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM megabyte \
                 WHERE n < 80) INSERT INTO growth_payload SELECT zeroblob(1048576) FROM megabyte;",
            )
            .await
            .expect("grow the table past 64 MiB");
        registry
            .advance_registered(&paths, &paths, &CancellationToken::new())
            .await;

        let (store, port) = registry
            .registered_port(&database_path, &scope)
            .expect("registered port");
        let read = port.latest_table_growth(&context, &store);
        let TableGrowthTelemetryReadV1::Observed { samples, .. } = &read else {
            panic!("the reader must see the maintenance measurement, got {read:?}");
        };
        let grown = samples
            .iter()
            .find(|sample| sample.table.as_str() == "growth_payload")
            .expect("growth sample for the grown table");
        assert!(grown.current_bytes.get() >= GROWN_TABLE_BYTES);
        assert!(grown.growth_bytes().get() >= GROWN_TABLE_BYTES);
        assert!(
            table_growth_doctor_evidence(&read)
                .iter()
                .any(|evidence| matches!(
                    evidence,
                    TableGrowthDoctorEvidenceV1::SignificantGrowth { table, .. }
                        if table.as_str() == "growth_payload"
                )),
            "doctor evidence must flag the grown table"
        );
    }

    async fn fixture_port(
        database_path: &Path,
        reader_wait: Duration,
    ) -> (Database, StoreKeyV1, GuardedStoreTelemetryPort) {
        let (database, scope) = growth_fixture(database_path).await;
        let store = StoreKeyV1::new("project.db".to_owned()).expect("store key");
        let port = GuardedStoreTelemetryPort::new(
            database
                .storage_telemetry_handle()
                .expect("telemetry handle"),
            store.clone(),
            scope,
            reader_wait,
            TableGrowthStateV1::default(),
        );
        (database, store, port)
    }

    #[asupersync::test]
    async fn store_size_reports_the_exact_page_pragmas() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let (_database, store, port) =
            fixture_port(&temporary.path().join("project.db"), Duration::from_secs(5)).await;
        let context = storage_telemetry_request_context(port.scope.clone()).expect("context");

        let read = port.store_size(&context, &store).await;

        let StorageTelemetryReadV1::Observed { sample } = read else {
            panic!("the mounted store's size must be observed, got {read:?}");
        };
        assert!(sample.page_size_bytes > 0);
        assert!(sample.page_count > 0);
        assert!(sample.freelist_pages <= sample.page_count);
    }

    #[asupersync::test]
    async fn a_table_created_after_the_baseline_is_pending_rather_than_grown_from_zero() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let (database, store, port) =
            fixture_port(&temporary.path().join("project.db"), Duration::from_secs(5)).await;
        let context = storage_telemetry_request_context(port.scope.clone()).expect("context");
        assert!(matches!(
            port.table_growth(&context, &store).await,
            TableGrowthTelemetryReadV1::BaselineEstablished { .. }
        ));
        database
            .execute_write_batch(
                "create a table after the baseline",
                "CREATE TABLE created_after_baseline (payload TEXT NOT NULL); \
                 INSERT INTO created_after_baseline VALUES ('new');",
            )
            .await
            .expect("new table");

        let read = port.table_growth(&context, &store).await;

        let TableGrowthTelemetryReadV1::Observed {
            samples,
            baseline_pending,
            ..
        } = read
        else {
            panic!("the second pass compares watermarks, got {read:?}");
        };
        assert!(
            samples
                .iter()
                .all(|sample| sample.table.as_str() != "created_after_baseline"),
            "a new table must not be compared against fabricated zero bytes"
        );
        let pending = baseline_pending
            .iter()
            .find(|pending| pending.table.as_str() == "created_after_baseline")
            .expect("the new table is explicitly baseline-pending");
        assert!(pending.current_bytes.get() > 0);
    }

    #[asupersync::test]
    async fn another_store_is_denied_without_bytes() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let (_database, _store, port) =
            fixture_port(&temporary.path().join("project.db"), Duration::from_secs(5)).await;
        let context = storage_telemetry_request_context(port.scope.clone()).expect("context");
        let other = StoreKeyV1::new("other.db".to_owned()).expect("store key");
        let denied = TableGrowthTelemetryReadV1::Denied {
            store: other.clone(),
        };

        assert_eq!(port.table_growth(&context, &other).await, denied);
        assert_eq!(port.latest_table_growth(&context, &other), denied);
        assert_eq!(
            port.store_size(&context, &other).await,
            StorageTelemetryReadV1::Denied { store: other }
        );
    }

    #[asupersync::test]
    async fn a_telemetry_wait_yields_the_polling_thread() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let (database, store, port) = fixture_port(
            &temporary.path().join("project.db"),
            Duration::from_millis(200),
        )
        .await;
        let context = storage_telemetry_request_context(port.scope.clone()).expect("context");
        let health_reader = database
            .begin_engine_health_read_snapshot()
            .await
            .expect("hold the reserved health reader");

        let mut read = port.table_growth(&context, &store);
        assert!(
            (&mut read).now_or_never().is_none(),
            "a measurement waiting on the health reader must not block the poller"
        );
        assert!(matches!(
            read.await,
            TableGrowthTelemetryReadV1::Unknown {
                reason: TableGrowthUnavailableReasonV1::ReaderUnavailable,
                ..
            }
        ));
        drop(health_reader);
    }

    #[test]
    fn only_a_walk_that_ran_out_of_its_deadline_marks_the_store_over_budget() {
        let at = UtcMicros(7);
        assert_eq!(
            settle_table_size_walk(
                Err(TableGrowthUnavailableReasonV1::DeadlineExceeded),
                42,
                at
            ),
            Ok(TableSizeWalk::OverBudget(OverBudgetTableSizeWalk {
                page_count: 42,
                observed_at: at,
            }))
        );
        assert_eq!(
            settle_table_size_walk(
                Err(TableGrowthUnavailableReasonV1::ReaderUnavailable),
                42,
                at
            ),
            Err(TableGrowthUnavailableReasonV1::ReaderUnavailable)
        );
    }

    #[asupersync::test]
    async fn an_over_budget_store_is_not_walked_until_it_shrinks_or_the_mark_ages_out() {
        let temporary = tempfile::tempdir().expect("telemetry fixture root");
        let (_database, store, port) =
            fixture_port(&temporary.path().join("project.db"), Duration::from_secs(5)).await;
        let context = storage_telemetry_request_context(port.scope.clone()).expect("context");
        let mark = |page_count, observed_at| {
            port.table_growth
                .lock()
                .expect("table growth state")
                .over_budget = Some(OverBudgetTableSizeWalk {
                page_count,
                observed_at,
            });
        };

        mark(1, now_micros());
        let skipped = port.table_growth(&context, &store).await;
        assert_eq!(
            skipped,
            TableGrowthTelemetryReadV1::Unknown {
                store: store.clone(),
                reason: TableGrowthUnavailableReasonV1::ExceedsMeasurementBudget,
            }
        );
        assert!(
            port.table_growth_state().watermarks.is_none(),
            "a store at least as large as its over-budget walk must not be walked again"
        );
        assert_eq!(port.latest_table_growth(&context, &store), skipped);

        mark(u64::MAX, now_micros());
        assert!(
            matches!(
                port.table_growth(&context, &store).await,
                TableGrowthTelemetryReadV1::BaselineEstablished { .. }
            ),
            "a store smaller than its over-budget walk is walked again"
        );
        assert_eq!(port.table_growth_state().over_budget, None);

        mark(
            1,
            UtcMicros(now_micros().0 - super::TABLE_SIZE_WALK_REPROBE_MICROS - 1),
        );
        assert!(
            matches!(
                port.table_growth(&context, &store).await,
                TableGrowthTelemetryReadV1::Observed { .. }
            ),
            "a day-old over-budget mark is probed again"
        );
        assert_eq!(port.table_growth_state().over_budget, None);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod table_growth_restart_tests {
    use std::collections::BTreeSet;

    use tracedecay_contracts::storage::TableGrowthTelemetryReadV1;
    use tracedecay_runtime_core::cancellation::CancellationToken;

    use super::{StoreTelemetrySamplingRegistry, storage_telemetry_request_context};

    #[asupersync::test]
    async fn a_restarted_registry_reports_the_persisted_measurement() {
        let temporary = tempfile::tempdir().expect("profile root");
        let profile_root = temporary.path().join("profile");
        let database_path = temporary.path().join("project.db");
        let (database, scope) = super::table_growth_tests::growth_fixture(&database_path).await;
        let paths = BTreeSet::from([database_path.clone()]);
        let before_restart = StoreTelemetrySamplingRegistry::persisted_under(&profile_root);
        assert!(before_restart.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        before_restart
            .advance_registered(&paths, &paths, &CancellationToken::new())
            .await;
        let context = storage_telemetry_request_context(scope.clone()).expect("reader context");
        let (store, port) = before_restart
            .registered_port(&database_path, &scope)
            .expect("registered port");
        let measured = port.latest_table_growth(&context, &store);
        let TableGrowthTelemetryReadV1::BaselineEstablished { observed_at, .. } = measured else {
            panic!("the first pass establishes a baseline, got {measured:?}");
        };
        drop(port);
        drop(before_restart);

        let after_restart = StoreTelemetrySamplingRegistry::persisted_under(&profile_root);
        assert!(after_restart.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        let (store, port) = after_restart
            .registered_port(&database_path, &scope)
            .expect("registered port after restart");
        assert_eq!(
            port.latest_table_growth(&context, &store),
            measured,
            "the restarted registry reports the measurement taken at {observed_at:?}"
        );

        after_restart
            .advance_registered(&paths, &paths, &CancellationToken::new())
            .await;
        let (store, port) = after_restart
            .registered_port(&database_path, &scope)
            .expect("registered port after restart");
        let TableGrowthTelemetryReadV1::Observed { samples, .. } =
            port.latest_table_growth(&context, &store)
        else {
            panic!("the restored watermarks compare instead of re-baselining");
        };
        assert!(
            samples
                .iter()
                .any(|sample| sample.table.as_str() == "growth_payload")
        );
        assert!(
            samples
                .iter()
                .all(|sample| sample.previous_observed_at == observed_at)
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod store_file_identity_tests {
    use std::path::{Path, PathBuf};

    use super::{StoreFileIdentityStateV1, StoreTelemetrySamplingRegistry};

    fn state_of(
        registry: &StoreTelemetrySamplingRegistry,
        path: &Path,
    ) -> Vec<(PathBuf, StoreFileIdentityStateV1)> {
        registry
            .check_store_identities()
            .into_iter()
            .filter(|(checked, _)| checked == path)
            .collect()
    }

    #[asupersync::test]
    async fn a_mounted_store_path_reports_replacement_and_removal() {
        let temporary = tempfile::tempdir().expect("identity fixture root");
        let database_path = temporary.path().join("project.db");
        let (database, scope) = super::table_growth_tests::growth_fixture(&database_path).await;
        let registry = StoreTelemetrySamplingRegistry::default();
        assert!(registry.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        assert_eq!(
            state_of(&registry, &database_path),
            [(database_path.clone(), StoreFileIdentityStateV1::Current)]
        );

        let replacement = temporary.path().join("replacement.db");
        std::fs::write(&replacement, b"replacement").expect("replacement file");
        std::fs::rename(&replacement, &database_path).expect("rename over the store");
        assert_eq!(
            state_of(&registry, &database_path),
            [(database_path.clone(), StoreFileIdentityStateV1::Replaced)]
        );

        // Profile-wide stores rebind on every project open; the held database
        // refuses to issue a reader over the replacement, so the port keeps
        // the identity it mounted.
        registry.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        });
        assert_eq!(
            state_of(&registry, &database_path),
            [(database_path.clone(), StoreFileIdentityStateV1::Replaced)],
            "a rebind through the held handle must not adopt the replacement"
        );

        std::fs::remove_file(&database_path).expect("remove the store file");
        assert_eq!(
            state_of(&registry, &database_path),
            [(database_path.clone(), StoreFileIdentityStateV1::Missing)]
        );
    }
}
