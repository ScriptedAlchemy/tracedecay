//! Store telemetry sampling and semantic-vector retention progress.
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
    TableGrowthTelemetryReadV1, TableNameV1,
};
use tracedecay_contracts::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestAdmission, RequestContext, ResolvedScope, now_micros,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_runtime_core::db::DatabaseStorageTelemetryHandle;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::tick::MaintenanceTickOutcome;
use tracedecay_runtime_core::logging::log_daemon_event;

const STORAGE_TELEMETRY_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;
const STORAGE_TELEMETRY_CAPABILITY: &str = "capability.application.storage.telemetry";
const STORAGE_TELEMETRY_USE_CASE: &str = "use-case.application.storage.telemetry.read";

#[derive(Clone, Copy)]
pub struct TableWatermark {
    bytes: StorageByteSizeV1,
    observed_at: UtcMicros,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum TableGrowthObservation {
    Preview,
    Advance,
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
    table_watermarks: Arc<std::sync::Mutex<Option<BTreeMap<TableNameV1, TableWatermark>>>>,
}

impl GuardedStoreTelemetryPort {
    fn new(
        handle: DatabaseStorageTelemetryHandle,
        store: StoreKeyV1,
        scope: ResolvedScope,
        reader_wait: Duration,
    ) -> Self {
        Self {
            handle,
            store,
            scope,
            reader_wait,
            table_watermarks: Arc::new(std::sync::Mutex::new(None)),
        }
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
            table_watermarks: Arc::clone(&self.table_watermarks),
        }
    }

    fn rebind(&self, handle: DatabaseStorageTelemetryHandle, scope: ResolvedScope) -> Self {
        Self {
            handle,
            store: self.store.clone(),
            scope,
            reader_wait: self.reader_wait,
            table_watermarks: Arc::clone(&self.table_watermarks),
        }
    }

    pub fn preview_table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        self.read_table_growth(context, store, TableGrowthObservation::Preview)
    }

    fn read_table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
        observation: TableGrowthObservation,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        Box::pin(hotpath::future!(
            async move {
                if !self.admits(context, store) {
                    return TableGrowthTelemetryReadV1::Denied {
                        store: store.clone(),
                    };
                }
                let Ok(current) = self
                    .handle
                    .table_size_telemetry(self.reader_wait, || telemetry_interruption(context))
                else {
                    return TableGrowthTelemetryReadV1::Unknown {
                        store: store.clone(),
                    };
                };
                let observed_at = now_micros();
                let mut current_tables = BTreeMap::new();
                for sample in current {
                    let Ok(table) = TableNameV1::new(sample.table_name) else {
                        return TableGrowthTelemetryReadV1::Unknown {
                            store: store.clone(),
                        };
                    };
                    current_tables.insert(table, StorageByteSizeV1(sample.bytes));
                }
                let mut watermarks = match self.table_watermarks.lock() {
                    Ok(watermarks) => watermarks,
                    Err(poisoned) => poisoned.into_inner(),
                };
                compare_table_growth(
                    store,
                    current_tables,
                    observed_at,
                    &mut watermarks,
                    observation,
                )
            },
            label = "daemon.maintenance.read_table_growth"
        ))
    }
}

impl StoreSizeTelemetryPort for GuardedStoreTelemetryPort {
    fn store_size<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, StorageTelemetryReadV1> {
        Box::pin(hotpath::future!(
            async move {
                if !self.admits(context, store) {
                    return StorageTelemetryReadV1::Denied {
                        store: store.clone(),
                    };
                }
                let Ok(sample) = self
                    .handle
                    .store_size_telemetry(self.reader_wait, || telemetry_interruption(context))
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
            label = "daemon.maintenance.read_store_size"
        ))
    }

    fn table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        self.read_table_growth(context, store, TableGrowthObservation::Advance)
    }
}

#[hotpath::measure(label = "daemon.maintenance.compare_table_growth")]
pub fn compare_table_growth(
    store: &StoreKeyV1,
    current_tables: BTreeMap<TableNameV1, StorageByteSizeV1>,
    observed_at: UtcMicros,
    watermarks: &mut Option<BTreeMap<TableNameV1, TableWatermark>>,
    observation: TableGrowthObservation,
) -> TableGrowthTelemetryReadV1 {
    let Some(previous_watermarks) = watermarks.as_ref() else {
        if observation == TableGrowthObservation::Preview {
            return TableGrowthTelemetryReadV1::Unknown {
                store: store.clone(),
            };
        }
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
    if observation == TableGrowthObservation::Advance {
        *watermarks = Some(
            current_tables
                .into_iter()
                .map(|(table, bytes)| (table, TableWatermark { bytes, observed_at }))
                .collect(),
        );
    }
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
    }
}

#[derive(Clone)]
struct CachedStoreTelemetryPort {
    scope: ResolvedScope,
    store: StoreKeyV1,
    port: GuardedStoreTelemetryPort,
}

/// Daemon-owned table-growth baseline authority shared by maintenance and
/// read-only diagnostic projections.
#[derive(Clone, Default)]
pub struct StoreTelemetrySamplingRegistry {
    ports: Arc<std::sync::Mutex<HashMap<PathBuf, CachedStoreTelemetryPort>>>,
    semantic_vector_retention:
        Arc<std::sync::Mutex<HashMap<PathBuf, SemanticVectorRetentionProgressV1>>>,
    graph_replay_release: Arc<std::sync::Mutex<HashMap<PathBuf, GraphReplayReleaseProgressV1>>>,
    graph_staging_release:
        Arc<std::sync::Mutex<HashMap<PathBuf, tracedecay_store::GraphProjectionIdentityV1>>>,
    /// Last by-design retention operator line per lane and project. A
    /// persistent unavailable-by-design condition logs once, then counts on
    /// [`daemon.git.maintenance.retention_quiet_total`]; a state change or a
    /// genuine anomaly emits again.
    retention_operator_log: Arc<std::sync::Mutex<HashMap<RetentionOperatorLogKeyV1, String>>>,
    loud_retention_this_tick: Arc<AtomicBool>,
}

/// Operator-log lane for the once-then-quiet retention pin.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RetentionOperatorLogLaneV1 {
    Semantic,
    CodeGeneration,
    Tick,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RetentionOperatorLogKeyV1 {
    lane: RetentionOperatorLogLaneV1,
    scope: PathBuf,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticVectorRetentionBacklogV1 {
    pub pending: u64,
    pub ready: u64,
    pub published: u64,
    pub cancelled: u64,
}

impl SemanticVectorRetentionBacklogV1 {
    pub fn from_receipt(receipt: &tracedecay_store::SemanticVectorProjectCensusReceipt) -> Self {
        Self {
            pending: receipt.counts.pending,
            ready: receipt.counts.ready,
            published: receipt.counts.published,
            cancelled: receipt.counts.cancelled,
        }
    }
}

/// Result of recording one semantic-vector retention census page.
///
/// Rejected variants stay fail-closed: progress resets and no receipt is
/// accepted. `CensusCountOverflow` is the only true u64-sum overflow
/// (`receipt.validate()`); other rejects name the actual page defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticVectorRetentionCensusOutcome {
    Accepted,
    InconsistentPage,
    IncompleteTerminalPage,
    CensusCountOverflow,
    ReceiptIdentityMismatch,
}

impl SemanticVectorRetentionCensusOutcome {
    #[hotpath::skip]
    pub const fn as_failure_label(self) -> Option<&'static str> {
        match self {
            Self::Accepted => None,
            Self::InconsistentPage => Some("inconsistent_page"),
            Self::IncompleteTerminalPage => Some("incomplete_terminal_page"),
            Self::CensusCountOverflow => Some("census_count_overflow"),
            Self::ReceiptIdentityMismatch => Some("receipt_identity_mismatch"),
        }
    }
}

// `Observed` is matched by field-destructuring across several call sites
// (doctor_kernel, git_watch/store_maintenance); boxing the receipt would
// ripple through all of them for a cold, infrequently-read maintenance
// status.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorRetentionReadV1 {
    Unknown,
    /// The semantic runtime is not seated for this daemon, so no vector
    /// census will ever start, let alone complete. This is the ordinary
    /// default-off state, distinct from [`Self::Unknown`] (a census that has
    /// not run yet or was reset by a failure or mutation).
    SemanticUnseated,
    Scanning,
    Observed {
        receipt: tracedecay_store::SemanticVectorProjectCensusReceipt,
    },
}

#[derive(Clone, Debug, Default)]
struct SemanticVectorRetentionProgressV1 {
    cursor: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
    observed: Option<tracedecay_store::SemanticVectorProjectCensusReceipt>,
    scanning: bool,
    semantic_unseated: bool,
}

impl StoreTelemetrySamplingRegistry {
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
            return true;
        }
        let port = GuardedStoreTelemetryPort::new(
            handle,
            store.clone(),
            scope.clone(),
            Duration::from_secs(5),
        );
        ports.insert(
            path.to_path_buf(),
            CachedStoreTelemetryPort {
                scope: scope.clone(),
                store,
                port,
            },
        );
        true
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
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
        self.retention_operator_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, _| key.scope != path);
    }

    pub fn release_retained_handles_for_shutdown(&self) {
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.retention_operator_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.loud_retention_this_tick
            .store(false, Ordering::Release);
    }

    pub fn semantic_vector_retention_cursor(
        &self,
        project_root: &Path,
    ) -> Option<tracedecay_store::SemanticVectorStageCensusCursor> {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(|progress| progress.cursor.clone())
    }

    pub fn retain_project_maintenance_state(&self, active_projects: &BTreeSet<PathBuf>) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
        self.graph_staging_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
        self.retention_operator_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, _| {
                key.scope.as_os_str().is_empty() || active_projects.contains(&key.scope)
            });
    }

    /// Whether this tick may attempt the graph-replay release reconcile.
    ///
    /// Consecutive unhealthy attempts open a bounded skip window; each denied
    /// tick burns one unit of it, so a wedged runtime is re-probed after at
    /// most [`GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS`] short-cadence ticks
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
    /// 1, 2, 4, then capped at [`GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS`].
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

    /// Whether this (lane, scope, detail) pair should emit an operator line.
    ///
    /// Identical repeats of a persistent by-design condition increment
    /// `daemon.git.maintenance.retention_quiet_total` and stay silent. A
    /// changed detail logs again.
    pub fn admit_by_design_retention_log(
        &self,
        lane: RetentionOperatorLogLaneV1,
        scope: &Path,
        detail: &str,
    ) -> bool {
        let key = RetentionOperatorLogKeyV1 {
            lane,
            scope: scope.to_path_buf(),
        };
        let mut states = self
            .retention_operator_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if states.get(&key).map(String::as_str) == Some(detail) {
            hotpath::gauge!("daemon.git.maintenance.retention_quiet_total").inc(1_u64);
            return false;
        }
        states.insert(key, detail.to_owned());
        true
    }

    pub fn clear_by_design_retention_log(&self, lane: RetentionOperatorLogLaneV1, scope: &Path) {
        let key = RetentionOperatorLogKeyV1 {
            lane,
            scope: scope.to_path_buf(),
        };
        self.retention_operator_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
    }

    /// Emit `retention_degraded` once per by-design state, or every time for
    /// a genuine anomaly. Repeat by-design ticks count on the quiet gauge.
    pub fn emit_retention_degraded(&self, project_root: &Path, pass: &'static str, failure: &str) {
        let lane = match pass {
            "semantic_vector_generations" => RetentionOperatorLogLaneV1::Semantic,
            "code_generations" => RetentionOperatorLogLaneV1::CodeGeneration,
            _ => {
                self.mark_loud_retention_log();
                log_daemon_event(
                    "retention_degraded",
                    &[("pass", pass.to_owned()), ("failure", failure.to_owned())],
                );
                return;
            }
        };
        if retention_failure_is_by_design(lane, failure) {
            if !self.admit_by_design_retention_log(lane, project_root, failure) {
                return;
            }
        } else {
            self.mark_loud_retention_log();
            self.clear_by_design_retention_log(lane, project_root);
        }
        log_daemon_event(
            "retention_degraded",
            &[("pass", pass.to_owned()), ("failure", failure.to_owned())],
        );
    }

    /// Whether the tick summary line should be written. Repeated by-design
    /// `retry` ticks stay quiet; a loud anomaly or an outcome change logs.
    pub fn admit_retention_tick_log(&self, outcome: MaintenanceTickOutcome) -> bool {
        let detail = format!("{}:{}", outcome.succeeded(), outcome.label());
        let loud = self.loud_retention_this_tick.load(Ordering::Acquire);
        if matches!(outcome, MaintenanceTickOutcome::Retry) && !loud {
            return self.admit_by_design_retention_log(
                RetentionOperatorLogLaneV1::Tick,
                Path::new(""),
                &detail,
            );
        }
        self.clear_by_design_retention_log(RetentionOperatorLogLaneV1::Tick, Path::new(""));
        true
    }

    pub fn record_semantic_vector_retention_failure(&self, project_root: &Path) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root.to_path_buf(),
                SemanticVectorRetentionProgressV1::default(),
            );
    }

    /// Pin the project's census read to [`SemanticVectorRetentionReadV1::SemanticUnseated`].
    ///
    /// The vector retention pass records this when the daemon has no seated
    /// semantic runtime, so downstream passes can distinguish "no census will
    /// ever exist" from a census that merely has not completed yet.
    pub fn record_semantic_vector_retention_unseated(&self, project_root: &Path) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root.to_path_buf(),
                SemanticVectorRetentionProgressV1 {
                    semantic_unseated: true,
                    ..SemanticVectorRetentionProgressV1::default()
                },
            );
    }

    pub fn record_semantic_vector_retention_census(
        &self,
        project_root: &Path,
        census: &tracedecay_graph_db::SemanticVectorRetentionCensus,
    ) -> SemanticVectorRetentionCensusOutcome {
        use tracedecay_graph_db::SemanticVectorRetentionAction;

        let mut retention = self
            .semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let progress = retention.entry(project_root.to_path_buf()).or_default();
        // A census page can only come from a seated semantic runtime.
        progress.semantic_unseated = false;
        if matches!(
            census.action,
            SemanticVectorRetentionAction::Retired(_)
                | SemanticVectorRetentionAction::Finalized(_)
                | SemanticVectorRetentionAction::CancelledRemoved(_)
        ) {
            // The returned page describes the pre-action state. Restart from
            // the beginning on the next tick instead of publishing stale sums.
            *progress = SemanticVectorRetentionProgressV1::default();
            return SemanticVectorRetentionCensusOutcome::Accepted;
        }
        progress.cursor.clone_from(&census.continuation);
        if census.continuation.is_some() {
            if census.complete_receipt.is_some() {
                *progress = SemanticVectorRetentionProgressV1::default();
                return SemanticVectorRetentionCensusOutcome::InconsistentPage;
            }
            progress.scanning = true;
            progress.observed = None;
        } else {
            let Some(receipt) = census.complete_receipt.clone() else {
                *progress = SemanticVectorRetentionProgressV1::default();
                return SemanticVectorRetentionCensusOutcome::IncompleteTerminalPage;
            };
            if receipt.validate().is_err() {
                *progress = SemanticVectorRetentionProgressV1::default();
                return SemanticVectorRetentionCensusOutcome::CensusCountOverflow;
            }
            if receipt.shard_id != census.shard_id || receipt.revision != census.revision {
                *progress = SemanticVectorRetentionProgressV1::default();
                return SemanticVectorRetentionCensusOutcome::ReceiptIdentityMismatch;
            }
            progress.observed = Some(receipt);
            progress.cursor = None;
            progress.scanning = false;
        }
        SemanticVectorRetentionCensusOutcome::Accepted
    }

    pub fn semantic_vector_retention_read(
        &self,
        project_root: &Path,
    ) -> SemanticVectorRetentionReadV1 {
        let retention = self
            .semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(progress) = retention.get(project_root) else {
            return SemanticVectorRetentionReadV1::Unknown;
        };
        if progress.semantic_unseated {
            return SemanticVectorRetentionReadV1::SemanticUnseated;
        }
        if progress.scanning {
            return SemanticVectorRetentionReadV1::Scanning;
        }
        progress
            .observed
            .clone()
            .map_or(SemanticVectorRetentionReadV1::Unknown, |receipt| {
                SemanticVectorRetentionReadV1::Observed { receipt }
            })
    }

    pub fn semantic_vector_scope_collection_ready(&self, project_root: &Path) -> bool {
        matches!(
            self.semantic_vector_retention_read(project_root),
            SemanticVectorRetentionReadV1::Observed {
                receipt: tracedecay_store::SemanticVectorProjectCensusReceipt {
                    counts: tracedecay_store::SemanticVectorStageCensusCounts {
                        pending: 0,
                        ready: 0,
                        published: _,
                        cancelled: 0,
                    },
                    ..
                },
            }
        )
    }

    #[hotpath::measure(label = "daemon.maintenance.sample_store_telemetry", future = true)]
    pub async fn advance_registered(
        &self,
        active_paths: &BTreeSet<PathBuf>,
        sampled_paths: &BTreeSet<PathBuf>,
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
                .map(|(_, cached)| cached.clone())
                .collect::<Vec<_>>()
        };
        let mut outcome = StoreTelemetrySamplingOutcome::default();
        for cached in ports {
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
        outcome
    }
}

/// Persistent by-design retention conditions log once, then count on gauges.
/// Corrupt, reset, denied, and cancelled failures stay loud every attempt.
pub fn retention_failure_is_by_design(lane: RetentionOperatorLogLaneV1, failure: &str) -> bool {
    match lane {
        RetentionOperatorLogLaneV1::Semantic => {
            failure == "configuration_inventory_unavailable" || failure.starts_with("unavailable:")
        }
        RetentionOperatorLogLaneV1::CodeGeneration => {
            failure.starts_with("vector_inventory_offline:")
        }
        RetentionOperatorLogLaneV1::Tick => false,
    }
}

#[hotpath::measure(label = "daemon.maintenance.mint_telemetry_context")]
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
