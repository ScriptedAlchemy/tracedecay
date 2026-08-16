use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tracedecay_application::storage::{
    StorageByteSizeV1, StorageTelemetryFuture, StorageTelemetryReadV1, StoreKeyV1,
    StoreSizeSampleV1, StoreSizeTelemetryPort, TableGrowthBaselinePendingV1, TableGrowthSampleV1,
    TableGrowthTelemetryReadV1, TableNameV1,
};
use tracedecay_application::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestAdmission, RequestContext, ResolvedScope, now_micros,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::branch_admin::StoreAdministration;
use crate::db::DatabaseStorageTelemetryHandle;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

pub(super) mod generation;

const COLD_STORE_PAGE_LIMIT: usize = 8;
/// Upper bound on mounted session databases + project graphs a single
/// maintenance tick may process. Each store gets one writer admission, so an
/// unbounded loop over every mounted project×branch cannot monopolize the lane;
/// this budget caps total work and a round-robin cursor (`store_cursor`)
/// guarantees every store is still reached across ticks.
const MAINTENANCE_STORE_PAGE_LIMIT: usize = 8;
const CHECKPOINT_DIRECTORY: &str = "maintenance";
const CHECKPOINT_FILE: &str = "retention-cold-store-cursor-v1.json";
const STORAGE_TELEMETRY_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;
const STORAGE_TELEMETRY_CAPABILITY: &str = "capability.application.storage.telemetry";
const STORAGE_TELEMETRY_USE_CASE: &str = "use-case.application.storage.telemetry.read";

#[derive(Clone, Copy)]
struct TableWatermark {
    bytes: StorageByteSizeV1,
    observed_at: UtcMicros,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TableGrowthObservation {
    Preview,
    Advance,
}

/// Store telemetry bound to the database's guarded read capability.
///
/// The runtime-core handle retains the exact database client that issued it;
/// this daemon adapter must not unwrap that guard into a raw SQL handle just to
/// retain the maintenance-owned table-growth baseline.
#[derive(Clone)]
pub(super) struct GuardedStoreTelemetryPort {
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

    pub(super) fn preview_table_growth<'a>(
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
        Box::pin(async move {
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
        })
    }
}

impl StoreSizeTelemetryPort for GuardedStoreTelemetryPort {
    fn store_size<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, StorageTelemetryReadV1> {
        Box::pin(async move {
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
        })
    }

    fn table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        self.read_table_growth(context, store, TableGrowthObservation::Advance)
    }
}

fn compare_table_growth(
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
pub(super) struct StoreTelemetrySamplingRegistry {
    ports: Arc<std::sync::Mutex<HashMap<PathBuf, CachedStoreTelemetryPort>>>,
    semantic_vector_retention:
        Arc<std::sync::Mutex<HashMap<PathBuf, SemanticVectorRetentionProgressV1>>>,
}

#[derive(Clone, Copy, Default)]
struct StoreTelemetrySamplingOutcome {
    observed: u64,
    unavailable: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SemanticVectorRetentionBacklogV1 {
    pub(super) pending: u64,
    pub(super) ready: u64,
    pub(super) published: u64,
    pub(super) cancelled: u64,
}

impl SemanticVectorRetentionBacklogV1 {
    pub(super) fn from_receipt(
        receipt: &tracedecay_store::SemanticVectorProjectCensusReceipt,
    ) -> Self {
        Self {
            pending: receipt.counts.pending,
            ready: receipt.counts.ready,
            published: receipt.counts.published,
            cancelled: receipt.counts.cancelled,
        }
    }
}

// `Observed` is matched by field-destructuring across several call sites
// (doctor_kernel, git_watch/store_maintenance); boxing the receipt would
// ripple through all of them for a cold, infrequently-read maintenance
// status.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SemanticVectorRetentionReadV1 {
    Unknown,
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
}

impl StoreTelemetrySamplingRegistry {
    pub(super) fn register_port<E>(
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

    pub(super) fn registered_port(
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

    pub(super) fn semantic_vector_retention_cursor(
        &self,
        project_root: &Path,
    ) -> Option<tracedecay_store::SemanticVectorStageCensusCursor> {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(|progress| progress.cursor.clone())
    }

    fn retain_semantic_vector_projects(&self, active_projects: &BTreeSet<PathBuf>) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
    }

    pub(super) fn record_semantic_vector_retention_failure(&self, project_root: &Path) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root.to_path_buf(),
                SemanticVectorRetentionProgressV1::default(),
            );
    }

    pub(super) fn record_semantic_vector_retention_census(
        &self,
        project_root: &Path,
        census: &tracedecay_graph_db::SemanticVectorRetentionCensus,
    ) -> bool {
        use tracedecay_graph_db::SemanticVectorRetentionAction;

        let mut retention = self
            .semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let progress = retention.entry(project_root.to_path_buf()).or_default();
        if matches!(
            census.action,
            SemanticVectorRetentionAction::Retired(_)
                | SemanticVectorRetentionAction::Finalized(_)
                | SemanticVectorRetentionAction::CancelledRemoved(_)
        ) {
            // The returned page describes the pre-action state. Restart from
            // the beginning on the next tick instead of publishing stale sums.
            *progress = SemanticVectorRetentionProgressV1::default();
            return true;
        }
        progress.cursor.clone_from(&census.continuation);
        if census.continuation.is_some() {
            if census.complete_receipt.is_some() {
                *progress = SemanticVectorRetentionProgressV1::default();
                return false;
            }
            progress.scanning = true;
            progress.observed = None;
        } else {
            let Some(receipt) = census.complete_receipt.clone() else {
                *progress = SemanticVectorRetentionProgressV1::default();
                return false;
            };
            if receipt.validate().is_err()
                || receipt.shard_id != census.shard_id
                || receipt.revision != census.revision
            {
                *progress = SemanticVectorRetentionProgressV1::default();
                return false;
            }
            progress.observed = Some(receipt);
            progress.cursor = None;
            progress.scanning = false;
        }
        true
    }

    pub(super) fn semantic_vector_retention_read(
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

    pub(super) fn semantic_vector_scope_collection_ready(&self, project_root: &Path) -> bool {
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

    async fn advance_registered(
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
    let capability =
        tracedecay_tool_catalog::CapabilityId::new(STORAGE_TELEMETRY_CAPABILITY.to_owned())?;
    let use_case = tracedecay_tool_catalog::UseCaseId::new(STORAGE_TELEMETRY_USE_CASE.to_owned())?;
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

#[derive(Debug)]
pub(super) struct MaintenanceCadence {
    interval: Duration,
    retry_delay: Duration,
    not_before: Option<Instant>,
    in_flight: bool,
}

impl MaintenanceCadence {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            retry_delay: interval.min(Duration::from_mins(1)),
            not_before: None,
            in_flight: false,
        }
    }

    pub(super) fn reserve(&mut self, now: Instant) -> bool {
        if self.in_flight || self.not_before.is_some_and(|not_before| now < not_before) {
            return false;
        }
        self.in_flight = true;
        true
    }

    pub(super) fn finish(&mut self, now: Instant, succeeded: bool) -> Duration {
        self.in_flight = false;
        let delay = if succeeded {
            self.interval
        } else {
            self.retry_delay
        };
        self.not_before = Some(now + delay);
        delay
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct ColdStoreCursorV1 {
    pub(super) after_project_id: Option<String>,
}

fn next_cold_store_cursor(
    previous: Option<&str>,
    project_ids: &[String],
    has_more: bool,
) -> Option<ColdStoreCursorV1> {
    if !has_more {
        return None;
    }
    Some(ColdStoreCursorV1 {
        after_project_id: project_ids
            .last()
            .cloned()
            .or_else(|| previous.map(str::to_owned)),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaintenanceStoreOutcomeV1 {
    Processed,
    Busy,
    Missing,
    Unreadable,
    Cancelled,
}

impl MaintenanceStoreOutcomeV1 {
    fn was_processed(self) -> bool {
        self == Self::Processed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct MaintenanceMetricsV1 {
    pub(super) ticks: u64,
    pub(super) processed_stores: u64,
    pub(super) deferred_stores: u64,
    pub(super) unavailable_stores: u64,
    pub(super) reclaimed_bytes: u64,
    pub(super) last_outcome: Option<MaintenanceStoreOutcomeV1>,
}

/// Grace windows for the daily branch-store GC pass, taken from the pinned
/// sync configuration at daemon startup.
#[derive(Clone, Copy, Debug)]
pub(super) struct BranchStoreGcCadenceV1 {
    pub(super) branch_gc_days: u64,
    pub(super) orphan_db_gc_days: u64,
}

/// Interval between branch-store GC passes across mounted projects.
const BRANCH_STORE_GC_PERIOD: Duration = Duration::from_hours(24);

#[derive(Clone)]
pub(super) struct MaintenanceCoordinator {
    cancellation: tracedecay_usecases::context::CancellationToken,
    wake: Arc<Notify>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    metrics: Arc<Mutex<MaintenanceMetricsV1>>,
    /// Round-robin fairness cursor over mounted stores: the sort key of the
    /// last store processed. The next tick resumes immediately after it so no
    /// store is starved when the mounted set exceeds `MAINTENANCE_STORE_PAGE_LIMIT`.
    store_cursor: Arc<Mutex<Option<String>>>,
    /// Instant of the last branch-store GC pass that succeeded for every
    /// mounted project. `None` keeps the daily cadence retry-eligible.
    last_branch_gc: Arc<Mutex<Option<Instant>>>,
}

impl Default for MaintenanceCoordinator {
    fn default() -> Self {
        Self {
            cancellation: tracedecay_usecases::context::CancellationToken::new(),
            wake: Arc::new(Notify::new()),
            task: Arc::new(Mutex::new(None)),
            metrics: Arc::new(Mutex::new(MaintenanceMetricsV1::default())),
            store_cursor: Arc::new(Mutex::new(None)),
            last_branch_gc: Arc::new(Mutex::new(None)),
        }
    }
}

/// One unit of bounded per-tick maintenance work: either a mounted session
/// database or a mounted project graph. Arcs are cloned into the item so the
/// store stays alive for the duration of the writer-held critical section.
enum MaintenanceStoreWork {
    Session(crate::global_db::RegisteredGlobalDbLeaseV1),
    Graph(Arc<crate::tracedecay::TraceDecay>),
}

impl MaintenanceStoreWork {
    fn database_path(&self) -> &Path {
        match self {
            Self::Session(database) => database.db_path(),
            Self::Graph(graph) => graph.db().database_path(),
        }
    }
}

/// Pure round-robin window selection over stably-sorted store keys.
///
/// Returns the indices to process this tick (at most `budget`, always
/// `min(budget, keys.len())`) and the cursor to resume after next tick. Sorting
/// the keys and resuming after the previous cursor guarantees that, across
/// `ceil(len / budget)` consecutive ticks, every store is processed at least
/// once — nothing that should be reclaimed is starved forever — while any
/// single tick touches no more than `budget` stores.
fn select_store_window(
    keys: &[String],
    after: Option<&str>,
    budget: usize,
) -> (Vec<usize>, Option<String>) {
    let count = keys.len();
    if count == 0 || budget == 0 {
        return (Vec::new(), after.map(str::to_owned));
    }
    let start = match after {
        Some(cursor) => keys.partition_point(|key| key.as_str() <= cursor) % count,
        None => 0,
    };
    let take = budget.min(count);
    let indices = (0..take)
        .map(|offset| (start + offset) % count)
        .collect::<Vec<_>>();
    let next = indices.last().map(|&index| keys[index].clone());
    (indices, next)
}

fn cursor_after_attempted_units(
    keys: &[String],
    window: &[usize],
    attempted: usize,
    prior: Option<&str>,
) -> Option<String> {
    attempted
        .checked_sub(1)
        .and_then(|last| window.get(last))
        .and_then(|&index| keys.get(index))
        .cloned()
        .or_else(|| prior.map(str::to_owned))
}

fn code_generation_retention_is_eligible(
    semantic_vector_retention_succeeded: bool,
    cancelled: bool,
) -> bool {
    semantic_vector_retention_succeeded && !cancelled
}

impl MaintenanceCoordinator {
    pub(super) async fn spawn(
        profile_root: PathBuf,
        profile_database: crate::global_db::RegisteredGlobalDbLeaseV1,
        administration: StoreAdministration,
        code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
    ) -> Self {
        let coordinator = Self::default();
        if !retention_maintenance_enabled(&retention) {
            return coordinator;
        }
        let task_owner = coordinator.clone();
        let interval = Duration::from_secs(retention.interval_hours.max(1).saturating_mul(3_600));
        let handle = tokio::spawn(async move {
            task_owner
                .run(
                    profile_root,
                    profile_database,
                    administration,
                    code_index_schedulers,
                    retention,
                    branch_gc,
                    interval,
                )
                .await;
        });
        *coordinator.task.lock().await = Some(handle);
        coordinator
    }

    #[cfg(unix)]
    pub(super) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(super) async fn shutdown(&self) {
        self.cancellation.cancel();
        self.wake.notify_waiters();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    async fn run(
        &self,
        profile_root: PathBuf,
        profile_database: crate::global_db::RegisteredGlobalDbLeaseV1,
        administration: StoreAdministration,
        code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
        interval: Duration,
    ) {
        let mut cadence = MaintenanceCadence::new(interval);
        let mut next_delay = cadence.retry_delay;
        loop {
            tokio::select! {
                biased;
                () = self.cancellation.cancelled() => break,
                () = self.wake.notified() => {}
                () = tokio::time::sleep(next_delay) => {}
            }
            if self.cancellation.is_cancelled() {
                break;
            }
            let now = Instant::now();
            if !cadence.reserve(now) {
                continue;
            }
            let succeeded = self
                .run_tick(
                    &profile_root,
                    profile_database.as_ref(),
                    &administration,
                    &code_index_schedulers,
                    &retention,
                    branch_gc,
                )
                .await;
            next_delay = cadence.finish(Instant::now(), succeeded);
        }
    }

    async fn run_tick(
        &self,
        profile_root: &Path,
        profile_database: &crate::global_db::RegisteredGlobalDb,
        administration: &StoreAdministration,
        code_index_schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: &crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
    ) -> bool {
        let session_databases = administration.mounted_registered_session_databases().await;
        let project_graphs = administration.mounted_project_graphs().await;
        let mut active_telemetry_paths = BTreeSet::from([profile_database.db_path().to_path_buf()]);
        active_telemetry_paths.extend(
            session_databases
                .iter()
                .map(|database| database.db_path().to_path_buf()),
        );
        active_telemetry_paths.extend(
            project_graphs
                .iter()
                .map(|graph| graph.db().database_path().to_path_buf()),
        );
        // Build one stably-sorted work list across both store kinds so the
        // per-tick budget and round-robin cursor bound the total work, not each
        // loop independently. Keys are unique on-disk identities (session db
        // path; project root + serving branch), prefixed by kind so the order
        // is deterministic regardless of the mounted maps' iteration order.
        let mut work: Vec<(String, MaintenanceStoreWork)> =
            Vec::with_capacity(session_databases.len() + project_graphs.len());
        for database in &session_databases {
            work.push((
                format!("s:{}", database.db_path().display()),
                MaintenanceStoreWork::Session(database.clone()),
            ));
        }
        for graph in &project_graphs {
            work.push((
                format!(
                    "g:{}\u{1f}{}",
                    graph.project_root().display(),
                    graph.serving_branch().unwrap_or_default()
                ),
                MaintenanceStoreWork::Graph(Arc::clone(graph)),
            ));
        }
        work.sort_by(|left, right| left.0.cmp(&right.0));
        let keys = work.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
        let after = self.store_cursor.lock().await.clone();
        let (window, _) =
            select_store_window(&keys, after.as_deref(), MAINTENANCE_STORE_PAGE_LIMIT);
        let mut sampled_telemetry_paths =
            BTreeSet::from([profile_database.db_path().to_path_buf()]);
        sampled_telemetry_paths.extend(
            window
                .iter()
                .map(|index| work[*index].1.database_path().to_path_buf()),
        );
        let maintenance_observations = administration.store_telemetry_sampling();
        let active_semantic_vector_projects = project_graphs
            .iter()
            .map(|graph| graph.project_root().to_path_buf())
            .collect::<BTreeSet<_>>();
        maintenance_observations.retain_semantic_vector_projects(&active_semantic_vector_projects);
        let telemetry_sampling = maintenance_observations
            .advance_registered(&active_telemetry_paths, &sampled_telemetry_paths)
            .await;

        // Bounded, round-robin slice of mounted stores. Writer admission is
        // per unit so one busy store defers only itself, and the cursor
        // advances past attempted units even on cancellation.
        let mut attempted = 0usize;
        let mut deferred = 0u64;
        let mut succeeded = true;
        for &index in &window {
            if self.cancellation.is_cancelled() {
                succeeded = false;
                break;
            }
            let admitted = administration
                .try_with_writer(|| async {
                    match &work[index].1 {
                        MaintenanceStoreWork::Session(database) => {
                            super::store_maintenance::run_session_retention(database, retention)
                                .await
                        }
                        MaintenanceStoreWork::Graph(graph) => {
                            generation::run_project_generation_maintenance(
                                graph,
                                code_index_schedulers,
                                &maintenance_observations,
                                &self.cancellation,
                                retention,
                            )
                            .await
                        }
                    }
                })
                .await;
            attempted = attempted.saturating_add(1);
            match admitted {
                Some(unit_succeeded) => succeeded &= unit_succeeded,
                None => {
                    deferred = deferred.saturating_add(1);
                    succeeded = false;
                }
            }
            if self.cancellation.is_cancelled() {
                succeeded = false;
                break;
            }
        }
        *self.store_cursor.lock().await =
            cursor_after_attempted_units(&keys, &window, attempted, after.as_deref());

        // Profile-wide observability retention is a single bounded op, not a
        // per-store loop, so it runs every tick outside the round-robin.
        if !self.cancellation.is_cancelled() {
            match administration
                .try_with_writer(|| async {
                    super::store_maintenance::run_observability_analytics_retention(
                        profile_database,
                        "global.db",
                    )
                    .await
                })
                .await
            {
                Some(unit_succeeded) => succeeded &= unit_succeeded,
                None => {
                    deferred = deferred.saturating_add(1);
                    succeeded = false;
                }
            }
        }

        if !self.cancellation.is_cancelled()
            && let Some(compaction) = &retention.compaction
        {
            match administration
                .try_with_writer(|| async {
                    super::store_maintenance::run_global_compaction(profile_database, compaction)
                        .await
                })
                .await
            {
                Some(unit_succeeded) => succeeded &= unit_succeeded,
                None => {
                    deferred = deferred.saturating_add(1);
                    succeeded = false;
                }
            }
        }
        if !self.cancellation.is_cancelled() {
            match administration
                .try_with_writer(|| {
                    run_cold_store_page(
                        profile_root,
                        profile_database,
                        retention,
                        &self.cancellation,
                    )
                })
                .await
            {
                Some(Ok(page)) => {
                    let mut metrics = self.metrics.lock().await;
                    metrics.processed_stores = metrics
                        .processed_stores
                        .saturating_add(page.processed_stores);
                    metrics.unavailable_stores = page.unavailable_stores;
                    metrics.reclaimed_bytes =
                        metrics.reclaimed_bytes.saturating_add(page.reclaimed_bytes);
                    metrics.last_outcome = Some(page.outcome);
                    succeeded &= page.outcome.was_processed();
                }
                Some(Err(_)) => succeeded = false,
                None => {
                    deferred = deferred.saturating_add(1);
                    succeeded = false;
                }
            }
        } else {
            succeeded = false;
        }

        // Branch-store GC, relocated here from the watcher backstop: the
        // watcher owns no store authorities, while this owner already holds
        // the administration coordinator. Daily cadence, retry-eligible — the
        // stamp advances only when every mounted project's pass succeeded.
        if !self.cancellation.is_cancelled() {
            let gc_due = self
                .last_branch_gc
                .lock()
                .await
                .is_none_or(|at| at.elapsed() >= BRANCH_STORE_GC_PERIOD);
            if gc_due {
                let mut gc_succeeded = true;
                for graph in &project_graphs {
                    if self.cancellation.is_cancelled() {
                        gc_succeeded = false;
                        break;
                    }
                    gc_succeeded &= super::store_maintenance::run_gc(
                        administration,
                        code_index_schedulers,
                        branch_gc.branch_gc_days,
                        branch_gc.orphan_db_gc_days,
                        graph,
                    )
                    .await;
                }
                if gc_succeeded {
                    *self.last_branch_gc.lock().await = Some(Instant::now());
                } else {
                    succeeded = false;
                }
            }
        }

        let mut metrics = self.metrics.lock().await;
        metrics.ticks = metrics.ticks.saturating_add(1);
        metrics.deferred_stores = metrics.deferred_stores.saturating_add(deferred);
        if deferred > 0 {
            metrics.last_outcome = Some(MaintenanceStoreOutcomeV1::Busy);
        } else if self.cancellation.is_cancelled() {
            metrics.last_outcome = Some(MaintenanceStoreOutcomeV1::Cancelled);
        }
        super::log_daemon_event(
            "retention_maintenance_tick",
            &[
                ("succeeded", succeeded.to_string()),
                ("processed_stores", metrics.processed_stores.to_string()),
                ("deferred_stores", metrics.deferred_stores.to_string()),
                ("unavailable_stores", metrics.unavailable_stores.to_string()),
                ("reclaimed_bytes", metrics.reclaimed_bytes.to_string()),
                ("telemetry_samples", telemetry_sampling.observed.to_string()),
                (
                    "telemetry_unavailable",
                    telemetry_sampling.unavailable.to_string(),
                ),
            ],
        );
        succeeded
    }
}

#[derive(Debug)]
struct ColdStorePageMetrics {
    processed_stores: u64,
    unavailable_stores: u64,
    reclaimed_bytes: u64,
    outcome: MaintenanceStoreOutcomeV1,
}

impl Default for ColdStorePageMetrics {
    fn default() -> Self {
        Self {
            processed_stores: 0,
            unavailable_stores: 0,
            reclaimed_bytes: 0,
            outcome: MaintenanceStoreOutcomeV1::Processed,
        }
    }
}

async fn run_cold_store_page(
    profile_root: &Path,
    profile_database: &crate::global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    cancellation: &tracedecay_usecases::context::CancellationToken,
) -> crate::errors::Result<ColdStorePageMetrics> {
    let checkpoint_path = checkpoint_path(profile_root);
    let cursor = load_cursor(&checkpoint_path).unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    let page = crate::retention::orphan_stores::build_store_census_page(
        profile_database,
        profile_root,
        cursor.after_project_id.as_deref(),
        COLD_STORE_PAGE_LIMIT,
    )
    .await?;
    let retention_now = if retention.orphan_store_gc_days.is_some()
        || retention.incident_debris_retention_days.is_some()
    {
        Some(
            now_secs_i64().map_err(|message| crate::errors::TraceDecayError::Config {
                message: message.to_owned(),
            })?,
        )
    } else {
        None
    };
    let mut metrics = ColdStorePageMetrics::default();
    for entry in &page.entries {
        let outcome = classify_cold_store_state(
            cancellation.is_cancelled(),
            entry.manifest_readable,
            entry.data_root.is_dir(),
        );
        match outcome {
            MaintenanceStoreOutcomeV1::Processed => {
                metrics.processed_stores = metrics.processed_stores.saturating_add(1);
            }
            MaintenanceStoreOutcomeV1::Cancelled => {
                metrics.outcome = outcome;
                return Ok(metrics);
            }
            MaintenanceStoreOutcomeV1::Busy
            | MaintenanceStoreOutcomeV1::Missing
            | MaintenanceStoreOutcomeV1::Unreadable => {
                if metrics.outcome == MaintenanceStoreOutcomeV1::Processed {
                    metrics.outcome = outcome;
                }
                metrics.unavailable_stores = metrics.unavailable_stores.saturating_add(1);
            }
        }
    }
    if let Some(days) = retention.orphan_store_gc_days {
        let findings = crate::retention::orphan_stores::classify_stores(
            &page.entries,
            retention_now.ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "maintenance retention clock unavailable".to_owned(),
            })?,
        );
        let plan =
            crate::retention::orphan_stores::plan_collection(findings, retention_window_secs(days));
        let (outcome, _) = crate::retention::orphan_stores::execute_registered_collection(
            profile_database,
            &plan,
            profile_root,
        )
        .await?;
        metrics.reclaimed_bytes = metrics
            .reclaimed_bytes
            .saturating_add(outcome.reclaimed_bytes);
        metrics.unavailable_stores = metrics
            .unavailable_stores
            .saturating_add(outcome.errors.len() as u64);
        if !outcome.errors.is_empty() {
            metrics.outcome = MaintenanceStoreOutcomeV1::Unreadable;
        }
    }
    if let Some(days) = retention.incident_debris_retention_days {
        let report = crate::retention::incident_debris::sweep_incident_debris(
            &page.entries,
            profile_root,
            retention_window_secs(days),
            retention_now.ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "maintenance retention clock unavailable".to_owned(),
            })?,
        );
        metrics.reclaimed_bytes = metrics
            .reclaimed_bytes
            .saturating_add(report.reclaimed_bytes);
        metrics.unavailable_stores = metrics
            .unavailable_stores
            .saturating_add(report.errors.len() as u64);
        if !report.errors.is_empty() {
            metrics.outcome = MaintenanceStoreOutcomeV1::Unreadable;
        }
    }
    let project_ids = page
        .entries
        .iter()
        .map(|entry| entry.project_id.clone())
        .collect::<Vec<_>>();
    let next_cursor = next_cold_store_cursor(
        cursor.after_project_id.as_deref(),
        &project_ids,
        page.next_cursor.is_some(),
    )
    .unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    persist_cursor(&checkpoint_path, &next_cursor).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("persist maintenance cold-store cursor: {error}"),
        }
    })?;
    Ok(metrics)
}

fn classify_cold_store_state(
    cancelled: bool,
    manifest_readable: bool,
    data_root_exists: bool,
) -> MaintenanceStoreOutcomeV1 {
    if cancelled {
        MaintenanceStoreOutcomeV1::Cancelled
    } else if !data_root_exists {
        MaintenanceStoreOutcomeV1::Missing
    } else if !manifest_readable {
        MaintenanceStoreOutcomeV1::Unreadable
    } else {
        MaintenanceStoreOutcomeV1::Processed
    }
}

fn checkpoint_path(profile_root: &Path) -> PathBuf {
    profile_root
        .join(CHECKPOINT_DIRECTORY)
        .join(CHECKPOINT_FILE)
}

fn load_cursor(path: &Path) -> Option<ColdStoreCursorV1> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn persist_cursor(path: &Path, cursor: &ColdStoreCursorV1) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("maintenance cursor has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(cursor).map_err(std::io::Error::other)?;
    let mut file = std::fs::File::create(&temporary)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

pub(super) fn retention_maintenance_enabled(retention: &crate::config::RetentionConfig) -> bool {
    retention.session_lcm.enabled
        || retention.observation.enabled
        || retention.orphan_store_gc_days.is_some()
        || retention.incident_debris_retention_days.is_some()
        || retention.compaction.is_some()
}

pub(super) fn retention_window_secs(days: u64) -> i64 {
    i64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .unwrap_or(i64::MAX)
}

fn now_secs_i64() -> Result<i64, &'static str> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the unix epoch")?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system clock exceeds retention timestamp range")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tracedecay_application::storage::{
        StorageByteSizeV1, StoreKeyV1, TableGrowthTelemetryReadV1, TableNameV1,
    };
    use tracedecay_domain::UtcMicros;

    use super::{
        ColdStoreCursorV1, MAINTENANCE_STORE_PAGE_LIMIT, MaintenanceCadence,
        MaintenanceStoreOutcomeV1, SemanticVectorRetentionReadV1, StoreTelemetrySamplingRegistry,
        TableGrowthObservation, checkpoint_path, classify_cold_store_state,
        code_generation_retention_is_eligible, compare_table_growth, cursor_after_attempted_units,
        load_cursor, next_cold_store_cursor, persist_cursor, select_store_window,
    };

    #[test]
    fn table_growth_preview_never_mutates_the_maintenance_baseline() {
        let store = StoreKeyV1::new("project.db").expect("store key");
        let table = TableNameV1::new("messages").expect("table name");
        let mut watermarks = None;

        let preview = compare_table_growth(
            &store,
            std::collections::BTreeMap::from([(table.clone(), StorageByteSizeV1(10))]),
            UtcMicros(1),
            &mut watermarks,
            TableGrowthObservation::Preview,
        );
        assert!(matches!(
            preview,
            TableGrowthTelemetryReadV1::Unknown { .. }
        ));
        assert!(
            watermarks.is_none(),
            "preview must not establish a baseline"
        );

        let baseline = compare_table_growth(
            &store,
            std::collections::BTreeMap::from([(table.clone(), StorageByteSizeV1(10))]),
            UtcMicros(2),
            &mut watermarks,
            TableGrowthObservation::Advance,
        );
        assert!(matches!(
            baseline,
            TableGrowthTelemetryReadV1::BaselineEstablished {
                tables_observed: 1,
                ..
            }
        ));

        let observed = compare_table_growth(
            &store,
            std::collections::BTreeMap::from([(table, StorageByteSizeV1(20))]),
            UtcMicros(3),
            &mut watermarks,
            TableGrowthObservation::Preview,
        );
        let TableGrowthTelemetryReadV1::Observed { samples, .. } = observed else {
            panic!("preview should compare with the maintenance baseline");
        };
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].growth_bytes().get(), 10);
    }

    #[test]
    fn cadence_rate_limits_failures_and_successes() {
        let started = Instant::now();
        let mut cadence = MaintenanceCadence::new(Duration::from_mins(1));

        assert!(cadence.reserve(started));
        assert!(!cadence.reserve(started));
        assert_eq!(cadence.finish(started, false), Duration::from_mins(1));
        assert!(!cadence.reserve(started + Duration::from_secs(59)));
        let retried = started + Duration::from_mins(1);
        assert!(cadence.reserve(retried));
        assert_eq!(cadence.finish(retried, true), Duration::from_mins(1));
        assert!(!cadence.reserve(retried + Duration::from_secs(59)));
        assert!(cadence.reserve(retried + Duration::from_mins(1)));
    }

    fn store_keys(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("s:{index:03}")).collect()
    }

    #[test]
    fn store_window_is_bounded_by_the_per_tick_budget() {
        let keys = store_keys(50);
        let (window, _) = select_store_window(&keys, None, MAINTENANCE_STORE_PAGE_LIMIT);
        assert_eq!(window.len(), MAINTENANCE_STORE_PAGE_LIMIT);

        // A mounted set smaller than the budget is processed whole.
        let small = store_keys(3);
        let (window, _) = select_store_window(&small, None, MAINTENANCE_STORE_PAGE_LIMIT);
        assert_eq!(window, vec![0, 1, 2]);
    }

    #[test]
    fn store_window_round_robin_reaches_every_store_and_never_starves() {
        // With more stores than the budget, feeding each tick's cursor into the
        // next must cover every store within ceil(count / budget) ticks while
        // no tick exceeds the budget — nothing reclaimable is skipped forever.
        for &(count, budget) in &[(7usize, 3usize), (50, 8), (17, 5), (8, 8), (1, 8)] {
            let keys = store_keys(count);
            let ticks = count.div_ceil(budget);
            let mut cursor: Option<String> = None;
            let mut covered = std::collections::BTreeSet::new();
            for _ in 0..ticks {
                let (window, next) = select_store_window(&keys, cursor.as_deref(), budget);
                assert!(
                    window.len() <= budget,
                    "count={count} budget={budget}: tick exceeded budget"
                );
                for index in window {
                    covered.insert(index);
                }
                cursor = next;
            }
            assert_eq!(
                covered.len(),
                count,
                "count={count} budget={budget}: not every store reached within {ticks} ticks"
            );
        }
    }

    #[test]
    fn store_window_resumes_after_the_cursor() {
        let keys = store_keys(10);
        let (first, next) = select_store_window(&keys, None, 4);
        assert_eq!(first, vec![0, 1, 2, 3]);
        assert_eq!(next.as_deref(), Some("s:003"));
        let (second, next) = select_store_window(&keys, next.as_deref(), 4);
        assert_eq!(second, vec![4, 5, 6, 7]);
        assert_eq!(next.as_deref(), Some("s:007"));
        // The window wraps past the end back to the front.
        let (third, _) = select_store_window(&keys, next.as_deref(), 4);
        assert_eq!(third, vec![8, 9, 0, 1]);
    }

    #[test]
    fn store_window_empty_set_preserves_cursor() {
        let (window, next) = select_store_window(&[], Some("s:005"), 8);
        assert!(window.is_empty());
        assert_eq!(next.as_deref(), Some("s:005"));
    }

    #[test]
    fn maintenance_cursor_advances_only_past_attempted_units() {
        let keys = store_keys(8);
        let (window, _) = select_store_window(&keys, None, 4);

        assert_eq!(
            cursor_after_attempted_units(&keys, &window, 2, None).as_deref(),
            Some("s:001")
        );
        assert_eq!(
            cursor_after_attempted_units(&keys, &window, 0, Some("s:007")).as_deref(),
            Some("s:007")
        );
    }

    #[test]
    fn semantic_vector_census_cursor_advances_and_resets_at_end() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");
        let shard_id = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.maintenance").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.maintenance").unwrap(),
            tracedecay_domain::ProjectId::new("project.maintenance").unwrap(),
        );
        let revision = tracedecay_store::SemanticVectorStageCensusRevision::new(7).unwrap();
        let first_counts = tracedecay_store::SemanticVectorStageCensusCounts {
            pending: 2,
            ready: 3,
            published: 4,
            cancelled: 5,
        };
        let first_digest = tracedecay_domain::canonical_sha256(&"first-page").unwrap();
        let cursor = tracedecay_store::SemanticVectorStageCensusCursor::new(
            shard_id.clone(),
            None,
            revision,
            256,
            first_counts,
            first_digest,
        )
        .expect("valid semantic vector cursor");
        let first = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id: shard_id.clone(),
            revision,
            pending: 2,
            ready: 3,
            published: 4,
            cancelled: 5,
            complete_receipt: None,
            continuation: Some(cursor.clone()),
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert!(registry.record_semantic_vector_retention_census(project, &first));
        assert_eq!(
            registry.semantic_vector_retention_cursor(project),
            Some(cursor)
        );
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Scanning
        );

        let second = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id: shard_id.clone(),
            revision,
            pending: 7,
            ready: 11,
            published: 13,
            cancelled: 17,
            complete_receipt: Some(tracedecay_store::SemanticVectorProjectCensusReceipt {
                shard_id,
                revision,
                counts: tracedecay_store::SemanticVectorStageCensusCounts {
                    pending: 9,
                    ready: 14,
                    published: 17,
                    cancelled: 22,
                },
                record_digest: tracedecay_domain::canonical_sha256(&"complete").unwrap(),
            }),
            continuation: None,
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert!(registry.record_semantic_vector_retention_census(project, &second));
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Observed {
                receipt: second.complete_receipt.unwrap(),
            }
        );
    }

    #[test]
    fn semantic_vector_mutation_and_failure_restart_census() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");
        let shard_id = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.maintenance").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.maintenance").unwrap(),
            tracedecay_domain::ProjectId::new("project.maintenance").unwrap(),
        );
        let revision = tracedecay_store::SemanticVectorStageCensusRevision::new(7).unwrap();
        let cursor = tracedecay_store::SemanticVectorStageCensusCursor::new(
            shard_id.clone(),
            None,
            revision,
            256,
            tracedecay_store::SemanticVectorStageCensusCounts {
                pending: 1,
                ready: 0,
                published: 1,
                cancelled: 0,
            },
            tracedecay_domain::canonical_sha256(&"page").unwrap(),
        )
        .expect("valid semantic vector cursor");
        let page = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id,
            revision,
            pending: 1,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: None,
            continuation: Some(cursor),
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert!(registry.record_semantic_vector_retention_census(project, &page));

        let generation = tracedecay_domain::VectorGenerationIdV1::new(
            tracedecay_domain::canonical_sha256(&"retired-generation")
                .expect("canonical generation digest"),
        );
        let mutated = tracedecay_graph_db::SemanticVectorRetentionCensus {
            action: tracedecay_graph_db::SemanticVectorRetentionAction::Retired(generation),
            ..page.clone()
        };
        assert!(registry.record_semantic_vector_retention_census(project, &mutated));
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );

        assert!(registry.record_semantic_vector_retention_census(project, &page));
        registry.record_semantic_vector_retention_failure(project);
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );
    }

    #[test]
    fn code_generation_retention_requires_prior_vector_success() {
        assert!(!code_generation_retention_is_eligible(false, false));
        assert!(!code_generation_retention_is_eligible(true, true));
        assert!(code_generation_retention_is_eligible(true, false));
    }

    #[test]
    fn cold_store_cursor_resumes_after_the_last_complete_project() {
        let first = next_cold_store_cursor(
            None,
            &["project-a".to_owned(), "project-b".to_owned()],
            true,
        )
        .expect("first page cursor");
        assert_eq!(
            first,
            ColdStoreCursorV1 {
                after_project_id: Some("project-b".to_owned()),
            }
        );

        assert_eq!(
            next_cold_store_cursor(
                first.after_project_id.as_deref(),
                &["project-c".to_owned()],
                false,
            ),
            None
        );
    }

    #[test]
    fn cold_store_outcomes_do_not_report_deferred_work_as_processed() {
        for outcome in [
            MaintenanceStoreOutcomeV1::Busy,
            MaintenanceStoreOutcomeV1::Missing,
            MaintenanceStoreOutcomeV1::Unreadable,
            MaintenanceStoreOutcomeV1::Cancelled,
        ] {
            assert!(!outcome.was_processed());
        }
        assert!(MaintenanceStoreOutcomeV1::Processed.was_processed());
    }

    #[test]
    fn cold_store_checkpoint_survives_restart() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-b".to_owned()),
        };

        persist_cursor(&path, &expected).expect("persist cursor");

        assert_eq!(load_cursor(&path), Some(expected));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn cold_store_state_distinguishes_missing_unreadable_and_cancelled() {
        assert_eq!(
            classify_cold_store_state(false, true, true),
            MaintenanceStoreOutcomeV1::Processed
        );
        assert_eq!(
            classify_cold_store_state(false, true, false),
            MaintenanceStoreOutcomeV1::Missing
        );
        assert_eq!(
            classify_cold_store_state(false, false, true),
            MaintenanceStoreOutcomeV1::Unreadable
        );
        assert_eq!(
            classify_cold_store_state(true, true, true),
            MaintenanceStoreOutcomeV1::Cancelled
        );
    }
}
