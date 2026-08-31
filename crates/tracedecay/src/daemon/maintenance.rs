use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(feature = "hotpath", test))]
use std::sync::atomic::{AtomicUsize, Ordering};
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
use tracedecay_application::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_runtime_core::db::DatabaseStorageTelemetryHandle;

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
#[cfg(any(feature = "hotpath", test))]
static MAINTENANCE_FUTURES_ACTIVE: AtomicUsize = AtomicUsize::new(0);

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
    graph_replay_release: Arc<std::sync::Mutex<HashMap<PathBuf, GraphReplayReleaseProgressV1>>>,
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

/// Result of recording one semantic-vector retention census page.
///
/// Rejected variants stay fail-closed: progress resets and no receipt is
/// accepted. `CensusCountOverflow` is the only true u64-sum overflow
/// (`receipt.validate()`); other rejects name the actual page defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SemanticVectorRetentionCensusOutcome {
    Accepted,
    InconsistentPage,
    IncompleteTerminalPage,
    CensusCountOverflow,
    ReceiptIdentityMismatch,
}

impl SemanticVectorRetentionCensusOutcome {
    pub(super) const fn as_failure_label(self) -> Option<&'static str> {
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
pub(super) enum SemanticVectorRetentionReadV1 {
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

    /// Release the telemetry client's exact database lease before the owning
    /// project store is retired. Other project and profile sampling ports stay
    /// mounted.
    pub(super) fn release_retained_handle(&self, path: &Path) {
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
    }

    pub(super) fn release_retained_handles_for_shutdown(&self) {
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

    fn retain_project_maintenance_state(&self, active_projects: &BTreeSet<PathBuf>) {
        self.semantic_vector_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|project, _| active_projects.contains(project));
    }

    /// Whether this tick may attempt the graph-replay release reconcile.
    ///
    /// Consecutive unhealthy attempts open a bounded skip window; each denied
    /// tick burns one unit of it, so a wedged runtime is re-probed after at
    /// most [`GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS`] short-cadence ticks
    /// rather than being polled (and timing out) on every one.
    pub(super) fn graph_replay_release_attempt_admitted(&self, project_root: &Path) -> bool {
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
    pub(super) fn record_graph_replay_release_unhealthy(&self, project_root: &Path) {
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
    pub(super) fn record_graph_replay_release_served(
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
    pub(super) fn graph_replay_release_cursor(&self, project_root: &Path) -> Option<String> {
        self.graph_replay_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(|state| state.cursor.clone())
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

    /// Pin the project's census read to [`SemanticVectorRetentionReadV1::SemanticUnseated`].
    ///
    /// The vector retention pass records this when the daemon has no seated
    /// semantic runtime, so downstream passes can distinguish "no census will
    /// ever exist" from a census that merely has not completed yet.
    pub(super) fn record_semantic_vector_retention_unseated(&self, project_root: &Path) {
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

    pub(super) fn record_semantic_vector_retention_census(
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum MaintenanceContinuation {
    /// Resume the bounded semantic-vector phase over the normal graph window.
    ///
    /// This is deliberately phase-scoped rather than project-scoped: no
    /// project identifier is retained in maintenance state, so mounted graphs
    /// continue to receive the same bounded, round-robin service.
    SemanticVectorRetention,
    /// Resume bounded code-generation retention over the normal graph window:
    /// a superseded-generation backlog or a partially drained graph-replay
    /// release queue keeps the short cadence until it converges, instead of
    /// parking multi-GiB debris behind the full maintenance interval.
    ///
    /// A continuation tick for this phase still runs the bounded
    /// semantic-vector page first, so semantic convergence never starves
    /// behind a code-generation drain.
    CodeGenerationRetention,
}

impl MaintenanceContinuation {
    /// Two phases asking to continue collapse to the one whose continuation
    /// tick still advances both: a code-generation continuation re-runs the
    /// bounded semantic-vector page on every tick, while a semantic-only
    /// continuation would starve a pending code-generation backlog.
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::CodeGenerationRetention, _) | (_, Self::CodeGenerationRetention) => {
                Self::CodeGenerationRetention
            }
            (Self::SemanticVectorRetention, Self::SemanticVectorRetention) => {
                Self::SemanticVectorRetention
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum MaintenanceTickOutcome {
    Complete,
    Continue(MaintenanceContinuation),
    Retry,
}

impl MaintenanceTickOutcome {
    pub(in crate::daemon) fn is_complete(self) -> bool {
        self == Self::Complete
    }

    fn continuation(self) -> Option<MaintenanceContinuation> {
        match self {
            Self::Continue(continuation) => Some(continuation),
            Self::Complete | Self::Retry => None,
        }
    }

    fn succeeded(self) -> bool {
        !matches!(self, Self::Retry)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Continue(MaintenanceContinuation::SemanticVectorRetention) => {
                "semantic_vector_progress"
            }
            Self::Continue(MaintenanceContinuation::CodeGenerationRetention) => {
                "code_generation_progress"
            }
            Self::Retry => "retry",
        }
    }

    /// A failure wins over ordinary bounded progress so the next short tick
    /// retries the complete maintenance journey. The semantic census cursor is
    /// durable in the graph registry, so that retry still resumes its progress
    /// rather than losing the bounded semantic-vector work.
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Retry, _) | (_, Self::Retry) => Self::Retry,
            (Self::Continue(left), Self::Continue(right)) => Self::Continue(left.combine(right)),
            (Self::Continue(continuation), Self::Complete)
            | (Self::Complete, Self::Continue(continuation)) => Self::Continue(continuation),
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }
}

/// The maintenance loop parks on `tokio::time::sleep_until`, so every deadline
/// it derives must be measured on the same clock the timer wheel uses.
/// `tokio::time::Instant` is the process monotonic clock in production and the
/// runtime's virtual clock under a paused test runtime; mixing it with
/// `std::time::Instant` would leave the due check permanently in the past
/// relative to a fired timer.
type CadenceInstant = tokio::time::Instant;

#[derive(Debug)]
pub(super) struct MaintenanceCadence {
    interval: Duration,
    retry_delay: Duration,
    not_before: Option<CadenceInstant>,
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

    pub(super) fn reserve(&mut self, now: CadenceInstant) -> bool {
        if self.in_flight || self.not_before.is_some_and(|not_before| now < not_before) {
            return false;
        }
        self.in_flight = true;
        true
    }

    fn finish(&mut self, now: CadenceInstant, outcome: MaintenanceTickOutcome) -> CadenceInstant {
        self.in_flight = false;
        let delay = match outcome {
            MaintenanceTickOutcome::Complete => self.interval,
            MaintenanceTickOutcome::Continue(_) | MaintenanceTickOutcome::Retry => self.retry_delay,
        };
        let deadline = now + delay;
        self.not_before = Some(deadline);
        deadline
    }
}

struct MaintenanceLifecycleInstrumentation;

impl MaintenanceLifecycleInstrumentation {
    fn new() -> Self {
        #[cfg(any(feature = "hotpath", test))]
        {
            let active = MAINTENANCE_FUTURES_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
            hotpath::gauge!("daemon_maintenance_futures_active").set(active);
        }
        Self
    }

    fn record_outcome(&self, outcome: MaintenanceTickOutcome) {
        match outcome {
            MaintenanceTickOutcome::Complete => {
                hotpath::gauge!("daemon_maintenance_outcome_complete").inc(1.0);
            }
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_outcome_semantic_vector_progress").inc(1.0);
            }
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_outcome_code_generation_progress").inc(1.0);
            }
            MaintenanceTickOutcome::Retry => {
                hotpath::gauge!("daemon_maintenance_outcome_retry").inc(1.0);
            }
        }
    }

    fn record_cancellation(&self) {
        hotpath::gauge!("daemon_maintenance_outcome_cancelled").inc(1.0);
    }
}

impl Drop for MaintenanceLifecycleInstrumentation {
    fn drop(&mut self) {
        #[cfg(any(feature = "hotpath", test))]
        {
            let active = MAINTENANCE_FUTURES_ACTIVE
                .fetch_sub(1, Ordering::SeqCst)
                .saturating_sub(1);
            hotpath::gauge!("daemon_maintenance_futures_active").set(active);
        }
    }
}

struct MaintenancePhaseInstrumentation {
    continuation: Option<MaintenanceContinuation>,
}

impl MaintenancePhaseInstrumentation {
    fn new(continuation: Option<MaintenanceContinuation>) -> Self {
        match continuation {
            Some(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_semantic_vector_active").inc(1.0);
            }
            Some(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_code_generation_active").inc(1.0);
            }
            None => {
                hotpath::gauge!("daemon_maintenance_phase_full_tick_active").inc(1.0);
            }
        }
        Self { continuation }
    }
}

impl Drop for MaintenancePhaseInstrumentation {
    fn drop(&mut self) {
        match self.continuation {
            Some(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_semantic_vector_active").inc(-1.0);
            }
            Some(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_code_generation_active").inc(-1.0);
            }
            None => {
                hotpath::gauge!("daemon_maintenance_phase_full_tick_active").inc(-1.0);
            }
        }
    }
}

async fn run_maintenance_loop<F, Fut>(
    cancellation: &tracedecay_session_memory::context::CancellationToken,
    wake: &Notify,
    interval: Duration,
    mut run_tick: F,
) where
    F: FnMut(Option<MaintenanceContinuation>) -> Fut,
    Fut: Future<Output = MaintenanceTickOutcome>,
{
    let _lifecycle = MaintenanceLifecycleInstrumentation::new();
    let mut cadence = MaintenanceCadence::new(interval);
    let mut deadline = CadenceInstant::now() + cadence.retry_delay;
    let mut continuation = None;
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                _lifecycle.record_cancellation();
                break;
            }
            () = wake.notified() => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        if cancellation.is_cancelled() {
            _lifecycle.record_cancellation();
            break;
        }
        let now = CadenceInstant::now();
        if now < deadline || !cadence.reserve(now) {
            continue;
        }
        let _phase = MaintenancePhaseInstrumentation::new(continuation);
        let outcome = run_tick(continuation).await;
        if cancellation.is_cancelled() {
            _lifecycle.record_cancellation();
            break;
        }
        _lifecycle.record_outcome(outcome);
        continuation = outcome.continuation();
        deadline = cadence.finish(CadenceInstant::now(), outcome);
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
    cancellation: tracedecay_session_memory::context::CancellationToken,
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
            cancellation: tracedecay_session_memory::context::CancellationToken::new(),
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
    Session(tracedecay_global_db::RegisteredGlobalDbLeaseV1),
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

impl MaintenanceCoordinator {
    pub(super) async fn spawn(
        profile_root: PathBuf,
        profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        administration: StoreAdministration,
        code_index_schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
    ) -> Self {
        let coordinator = Self::default();
        if !retention_maintenance_enabled(&retention) {
            return coordinator;
        }
        let task_owner = coordinator.clone();
        let interval = Duration::from_secs(retention.interval_hours.max(1).saturating_mul(3_600));
        let handle = tokio::spawn(hotpath::future!(
            async move {
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
            },
            label = "daemon.maintenance.retention_loop"
        ));
        *coordinator.task.lock().await = Some(handle);
        coordinator
    }

    #[cfg(unix)]
    pub(super) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Stop the maintenance loop from starting another pass, synchronously.
    ///
    /// This is the half of `shutdown` that must run at shutdown *prepare*
    /// time rather than when this owner's join is finally polled: the
    /// maintenance owner sits in an early phase, but when an earlier phase
    /// overruns and the coordinator aborts the drain runner, an un-cancelled
    /// loop keeps ticking (retention passes were still logging after the
    /// terminal shutdown receipt). Cancelling here is idempotent, so the
    /// join below stays correct whether or not it already ran.
    pub(super) fn cancel(&self) {
        self.cancellation.cancel();
        self.wake.notify_waiters();
    }

    pub(super) async fn shutdown(&self) {
        self.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    async fn run(
        &self,
        profile_root: PathBuf,
        profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        administration: StoreAdministration,
        code_index_schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
        interval: Duration,
    ) {
        run_maintenance_loop(&self.cancellation, &self.wake, interval, |continuation| {
            self.run_tick(
                &profile_root,
                profile_database.as_ref(),
                &administration,
                &code_index_schedulers,
                &retention,
                branch_gc,
                continuation,
            )
        })
        .await;
    }

    #[hotpath::measure(label = "daemon.maintenance.tick", future = true)]
    async fn run_tick(
        &self,
        profile_root: &Path,
        profile_database: &tracedecay_global_db::RegisteredGlobalDb,
        administration: &StoreAdministration,
        code_index_schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
        retention: &crate::config::RetentionConfig,
        branch_gc: BranchStoreGcCadenceV1,
        continuation: Option<MaintenanceContinuation>,
    ) -> MaintenanceTickOutcome {
        // Piggybacks on the retention tick's existing cadence instead of a
        // dedicated timer thread: this is the daemon's only always-running
        // periodic loop, so it is where a slow RSS climb toward the
        // admission limit becomes visible between full telemetry snapshots.
        record_process_resident_memory_gauge();
        let session_databases = if continuation.is_none() {
            administration.mounted_registered_session_databases().await
        } else {
            Vec::new()
        };
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
        let active_maintenance_projects = project_graphs
            .iter()
            .map(|graph| graph.project_root().to_path_buf())
            .collect::<BTreeSet<_>>();
        maintenance_observations.retain_project_maintenance_state(&active_maintenance_projects);
        let telemetry_sampling = if continuation.is_none() {
            maintenance_observations
                .advance_registered(&active_telemetry_paths, &sampled_telemetry_paths)
                .await
        } else {
            StoreTelemetrySamplingOutcome::default()
        };

        // Bounded, round-robin slice of mounted stores. Writer admission is
        // per unit so one busy store defers only itself, and the cursor
        // advances past attempted units even on cancellation. A semantic
        // continuation omits session stores, but remains phase-scoped over the
        // same bounded graph window rather than pinning one project.
        let mut attempted = 0usize;
        let mut deferred = 0u64;
        let mut outcome = MaintenanceTickOutcome::Complete;
        for &index in &window {
            if self.cancellation.is_cancelled() {
                outcome = MaintenanceTickOutcome::Retry;
                break;
            }
            let admitted = administration
                .try_with_writer(|| async {
                    match &work[index].1 {
                        MaintenanceStoreWork::Session(database) => {
                            if super::store_maintenance::run_session_retention(database, retention)
                                .await
                            {
                                MaintenanceTickOutcome::Complete
                            } else {
                                MaintenanceTickOutcome::Retry
                            }
                        }
                        MaintenanceStoreWork::Graph(graph) => {
                            generation::run_project_generation_maintenance(
                                graph,
                                code_index_schedulers,
                                &maintenance_observations,
                                &self.cancellation,
                                retention,
                                continuation,
                            )
                            .await
                        }
                    }
                })
                .await;
            attempted = attempted.saturating_add(1);
            match admitted {
                Some(unit_outcome) => outcome = outcome.combine(unit_outcome),
                None => {
                    deferred = deferred.saturating_add(1);
                    outcome = MaintenanceTickOutcome::Retry;
                }
            }
            if self.cancellation.is_cancelled() {
                outcome = MaintenanceTickOutcome::Retry;
                break;
            }
        }
        *self.store_cursor.lock().await =
            cursor_after_attempted_units(&keys, &window, attempted, after.as_deref());

        // Profile-wide maintenance is intentionally excluded from a bounded
        // semantic-vector continuation: only the owning phase is eligible
        // for the short cadence.
        if continuation.is_none() && !self.cancellation.is_cancelled() {
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
                Some(true) => {}
                Some(false) => outcome = MaintenanceTickOutcome::Retry,
                None => {
                    deferred = deferred.saturating_add(1);
                    outcome = MaintenanceTickOutcome::Retry;
                }
            }
        }

        if continuation.is_none()
            && !self.cancellation.is_cancelled()
            && let Some(compaction) = &retention.compaction
        {
            match administration
                .try_with_writer(|| async {
                    super::store_maintenance::run_global_compaction(profile_database, compaction)
                        .await
                })
                .await
            {
                Some(true) => {}
                Some(false) => outcome = MaintenanceTickOutcome::Retry,
                None => {
                    deferred = deferred.saturating_add(1);
                    outcome = MaintenanceTickOutcome::Retry;
                }
            }
        }
        if continuation.is_none() && !self.cancellation.is_cancelled() {
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
                    if !page.outcome.was_processed() {
                        outcome = MaintenanceTickOutcome::Retry;
                    }
                }
                Some(Err(_)) => outcome = MaintenanceTickOutcome::Retry,
                None => {
                    deferred = deferred.saturating_add(1);
                    outcome = MaintenanceTickOutcome::Retry;
                }
            }
        }

        // Branch-store GC: the watcher owns no store authorities, while this
        // owner already holds the administration coordinator. Daily cadence,
        // retry-eligible — the stamp advances only when every mounted project's
        // pass succeeded.
        if continuation.is_none() && !self.cancellation.is_cancelled() {
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
                    outcome = MaintenanceTickOutcome::Retry;
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
                ("succeeded", outcome.succeeded().to_string()),
                ("outcome", outcome.label().to_owned()),
                ("processed_stores", metrics.processed_stores.to_string()),
                // The lifetime total reads like a queue depth on a live tail
                // (a busy writer makes it "climb every tick"); the per-tick
                // count is the actual deferral pressure of this tick.
                ("deferred_stores_tick", deferred.to_string()),
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
        outcome
    }
}

/// Samples this process's current resident set size, republishes it as a
/// Hotpath gauge, and feeds it to the resident-memory admission authority.
///
/// A 20G RSS overrun past the admission limit was visible only to `ps` during
/// a 2026-08 incident; this closes that gap using the retention tick's
/// existing cadence rather than a new background timer. Publishing the same
/// sample to
/// [`process_resident_memory_pressure_v1`](tracedecay_runtime_core::resident_memory::process_resident_memory_pressure_v1)
/// closes the loop: admission stops trusting its reservation model once the
/// measurement says the process is over budget. There is exactly one reader
/// here — the gauge and the admission cell consume the same sample.
#[cfg(target_os = "linux")]
fn record_process_resident_memory_gauge() {
    if let Some(bytes) =
        tracedecay_runtime_core::resident_memory::sampled_process_resident_bytes_v1()
    {
        hotpath::gauge!("daemon.process.resident_bytes").set(bytes);
        let state = tracedecay_runtime_core::resident_memory::process_resident_memory_pressure_v1()
            .publish_observed_resident_bytes(bytes);
        if let tracedecay_runtime_core::resident_memory::ResidentMemoryPressureStateV1::OverBudget {
            observed_bytes,
            limit_bytes,
            high_watermark_bytes,
            ..
        } = state
        {
            tracing::warn!(
                event = "daemon_resident_memory_over_budget",
                observed_bytes,
                limit_bytes,
                high_watermark_bytes,
                "measured process RSS is over the admission high watermark; refusing new growth and releasing reclaimable retained state"
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn record_process_resident_memory_gauge() {}

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

#[hotpath::measure(label = "daemon.maintenance.cold_store_page", future = true)]
async fn run_cold_store_page(
    profile_root: &Path,
    profile_database: &tracedecay_global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
) -> tracedecay_domain::errors::Result<ColdStorePageMetrics> {
    let checkpoint_path = checkpoint_path(profile_root);
    let cursor = load_cursor(&checkpoint_path).unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    let page = tracedecay_maintenance::retention::orphan_stores::build_store_census_page(
        profile_database,
        profile_root,
        cursor.after_project_id.as_deref(),
        COLD_STORE_PAGE_LIMIT,
    )
    .await?;
    let retention_now = if retention.orphan_store_gc_days.is_some()
        || retention.incident_debris_retention_days.is_some()
    {
        Some(now_secs_i64().map_err(|message| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: message.to_owned(),
            }
        })?)
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
        let findings = tracedecay_maintenance::retention::orphan_stores::classify_stores(
            &page.entries,
            retention_now.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "maintenance retention clock unavailable".to_owned(),
            })?,
        );
        let plan = tracedecay_maintenance::retention::orphan_stores::plan_collection(
            findings,
            retention_window_secs(days),
        );
        let (outcome, _) =
            tracedecay_maintenance::retention::orphan_stores::execute_registered_collection(
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
        let report = tracedecay_maintenance::retention::incident_debris::sweep_incident_debris(
            &page.entries,
            profile_root,
            retention_window_secs(days),
            retention_now.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
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
        tracedecay_domain::errors::TraceDecayError::Config {
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

pub(crate) fn now_secs_i64() -> Result<i64, &'static str> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system_clock_before_unix_epoch")?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system_clock_out_of_range")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use tokio::sync::Notify;
    use tracedecay_application::storage::{
        StorageByteSizeV1, StoreKeyV1, TableGrowthTelemetryReadV1, TableNameV1,
    };
    use tracedecay_domain::UtcMicros;

    use super::{
        CadenceInstant, ColdStoreCursorV1, MAINTENANCE_FUTURES_ACTIVE,
        MAINTENANCE_STORE_PAGE_LIMIT, MaintenanceCadence, MaintenanceContinuation,
        MaintenanceStoreOutcomeV1, MaintenanceTickOutcome, SemanticVectorRetentionCensusOutcome,
        SemanticVectorRetentionReadV1, StoreTelemetrySamplingRegistry, TableGrowthObservation,
        checkpoint_path, classify_cold_store_state, compare_table_growth,
        cursor_after_attempted_units, load_cursor, next_cold_store_cursor, persist_cursor,
        run_maintenance_loop, select_store_window,
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
        let started = CadenceInstant::now();
        let mut cadence = MaintenanceCadence::new(Duration::from_mins(1));

        assert!(cadence.reserve(started));
        assert!(!cadence.reserve(started));
        assert_eq!(
            cadence.finish(started, MaintenanceTickOutcome::Retry),
            started + Duration::from_mins(1)
        );
        assert!(!cadence.reserve(started + Duration::from_secs(59)));
        let retried = started + Duration::from_mins(1);
        assert!(cadence.reserve(retried));
        assert_eq!(
            cadence.finish(retried, MaintenanceTickOutcome::Complete),
            retried + Duration::from_mins(1)
        );
        assert!(!cadence.reserve(retried + Duration::from_secs(59)));
        assert!(cadence.reserve(retried + Duration::from_mins(1)));
    }

    #[test]
    fn retry_outcome_takes_precedence_over_bounded_progress() {
        let progress =
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::SemanticVectorRetention);

        assert_eq!(
            progress.combine(MaintenanceTickOutcome::Retry),
            MaintenanceTickOutcome::Retry
        );
        assert_eq!(
            MaintenanceTickOutcome::Retry.combine(progress),
            MaintenanceTickOutcome::Retry
        );
    }

    #[test]
    fn code_generation_continuation_dominates_the_semantic_phase() {
        // A code-generation continuation tick re-runs the bounded semantic
        // page, so it must win when both phases report bounded progress; the
        // reverse would starve the code-generation backlog.
        let semantic =
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::SemanticVectorRetention);
        let code_generation =
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention);

        assert_eq!(semantic.combine(code_generation), code_generation);
        assert_eq!(code_generation.combine(semantic), code_generation);
        assert_eq!(
            code_generation.combine(MaintenanceTickOutcome::Complete),
            code_generation
        );
        assert_eq!(
            MaintenanceTickOutcome::Complete.combine(code_generation),
            code_generation
        );
        assert_eq!(
            code_generation.combine(MaintenanceTickOutcome::Retry),
            MaintenanceTickOutcome::Retry
        );
    }

    #[test]
    fn graph_replay_release_backoff_widens_and_recovers() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");

        // No recorded state admits every attempt.
        assert!(registry.graph_replay_release_attempt_admitted(project));
        assert!(registry.graph_replay_release_attempt_admitted(project));

        // Consecutive unhealthy attempts widen the skip window 1, 2, 4, and
        // cap at GRAPH_REPLAY_RELEASE_BACKOFF_CAP_TICKS denied ticks.
        for expected_skips in [1_usize, 2, 4, 8, 8] {
            registry.record_graph_replay_release_unhealthy(project);
            let mut denied = 0_usize;
            while !registry.graph_replay_release_attempt_admitted(project) {
                denied += 1;
                assert!(denied <= 16, "the skip window must stay bounded");
            }
            assert_eq!(
                denied, expected_skips,
                "the skip window must double per consecutive failure and cap"
            );
        }

        // A served attempt closes the window entirely.
        registry.record_graph_replay_release_served(project, None);
        assert!(registry.graph_replay_release_attempt_admitted(project));
        assert_eq!(registry.graph_replay_release_cursor(project), None);

        // The next failure after recovery starts from the narrowest window.
        registry.record_graph_replay_release_unhealthy(project);
        assert!(!registry.graph_replay_release_attempt_admitted(project));
        assert!(registry.graph_replay_release_attempt_admitted(project));
    }

    #[test]
    fn graph_replay_release_cursor_survives_failures_and_resets_on_wrap() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");

        registry
            .record_graph_replay_release_served(project, Some("release-000000ff.json".to_owned()));
        assert_eq!(
            registry.graph_replay_release_cursor(project).as_deref(),
            Some("release-000000ff.json")
        );

        // An unhealthy attempt keeps the cursor: consumed events are durably
        // removed, so resuming from the same position loses nothing.
        registry.record_graph_replay_release_unhealthy(project);
        assert_eq!(
            registry.graph_replay_release_cursor(project).as_deref(),
            Some("release-000000ff.json")
        );

        // Reaching the end of the queue clears state so the next attempt
        // starts from the front.
        registry.record_graph_replay_release_served(project, None);
        assert_eq!(registry.graph_replay_release_cursor(project), None);
        assert!(registry.graph_replay_release_attempt_admitted(project));
    }

    #[test]
    fn project_maintenance_state_is_pruned_with_the_active_set() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let retained = std::path::Path::new("/retained");
        let retired = std::path::Path::new("/retired");
        registry.record_graph_replay_release_unhealthy(retained);
        registry.record_graph_replay_release_unhealthy(retired);

        registry.retain_project_maintenance_state(&std::collections::BTreeSet::from([
            retained.to_path_buf()
        ]));

        assert!(
            !registry.graph_replay_release_attempt_admitted(retained),
            "the active project's backoff window must survive pruning"
        );
        assert!(
            registry.graph_replay_release_attempt_admitted(retired),
            "a retired project's backoff state must be dropped"
        );
    }

    /// `MAINTENANCE_FUTURES_ACTIVE` is a process-wide gauge, so a test that
    /// observes absolute readings must not overlap another test that runs a
    /// maintenance loop. Every test that starts `run_maintenance_loop` holds
    /// this lock for the whole lifetime of its loop.
    static MAINTENANCE_LOOP_LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test(start_paused = true)]
    async fn repeated_wakes_do_not_move_the_maintenance_due_deadline() {
        let _lifecycle_isolation = MAINTENANCE_LOOP_LIFECYCLE.lock().await;
        let cancellation = tracedecay_session_memory::context::CancellationToken::new();
        let wake = Arc::new(Notify::new());
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let baseline = MAINTENANCE_FUTURES_ACTIVE.load(Ordering::SeqCst);
        let task_cancellation = cancellation.clone();
        let task_wake = Arc::clone(&wake);
        let task_ticks = Arc::clone(&ticks);
        let task = tokio::spawn(async move {
            run_maintenance_loop(
                &task_cancellation,
                &task_wake,
                Duration::from_mins(10),
                move |_| {
                    let ticks = Arc::clone(&task_ticks);
                    async move {
                        ticks.fetch_add(1, Ordering::SeqCst);
                        MaintenanceTickOutcome::Complete
                    }
                },
            )
            .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(
            MAINTENANCE_FUTURES_ACTIVE.load(Ordering::SeqCst),
            baseline + 1,
            "the loop lifecycle must become observable while the task is live"
        );

        for _ in 0..3 {
            wake.notify_one();
            tokio::task::yield_now().await;
            assert_eq!(ticks.load(Ordering::SeqCst), 0);
        }
        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert_eq!(ticks.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            1,
            "the original absolute deadline must run exactly once despite early wakes"
        );

        cancellation.cancel();
        task.await
            .expect("maintenance loop joins after cancellation");
        assert_eq!(
            MAINTENANCE_FUTURES_ACTIVE.load(Ordering::SeqCst),
            baseline,
            "cancellation must drop the lifecycle guard and clear the active gauge"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn progress_continuation_reenters_only_the_owning_phase() {
        let _lifecycle_isolation = MAINTENANCE_LOOP_LIFECYCLE.lock().await;
        let cancellation = tracedecay_session_memory::context::CancellationToken::new();
        let wake = Arc::new(Notify::new());
        let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
        let task_cancellation = cancellation.clone();
        let task_wake = Arc::clone(&wake);
        let task_phases = Arc::clone(&phases);
        let task = tokio::spawn(async move {
            run_maintenance_loop(
                &task_cancellation,
                &task_wake,
                Duration::from_mins(10),
                move |continuation| {
                    let phases = Arc::clone(&task_phases);
                    async move {
                        let mut phases = phases
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        phases.push(continuation);
                        if phases.len() == 1 {
                            MaintenanceTickOutcome::Continue(
                                MaintenanceContinuation::SemanticVectorRetention,
                            )
                        } else {
                            MaintenanceTickOutcome::Complete
                        }
                    }
                },
            )
            .await;
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_mins(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_mins(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            *phases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![None, Some(MaintenanceContinuation::SemanticVectorRetention)],
            "bounded progress must resume its semantic-vector phase instead of a full tick"
        );

        cancellation.cancel();
        task.await
            .expect("maintenance loop joins after cancellation");
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

    #[tokio::test]
    async fn shutdown_release_clears_retained_telemetry_handles_and_progress() {
        let temporary = tempfile::tempdir().expect("telemetry registry fixture root");
        let database_path = temporary.path().join("project.db");
        let other_database_path = temporary.path().join("other.db");
        crate::daemon::store_runtime::register_registered_schema_installer();
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &database_path,
            "maintenance telemetry shutdown fixture",
        )
        .expect("telemetry fixture database authority");
        let (database, _) = tracedecay_runtime_core::db::Database::publish_test_runtime(
            &database_path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("telemetry fixture database");
        let project_id = tracedecay_domain::ProjectId::new("project.maintenance-shutdown")
            .expect("project identity");
        let scope = tracedecay_application::ResolvedScope::new(
            project_id,
            tracedecay_domain::RepositoryId::new("repository.maintenance-shutdown")
                .expect("repository identity"),
            tracedecay_domain::WorktreeId::new("worktree.maintenance-shutdown")
                .expect("worktree identity"),
            None,
        )
        .expect("resolved scope");
        let registry = StoreTelemetrySamplingRegistry::default();
        assert!(registry.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        assert!(registry.register_port(&other_database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        registry.record_semantic_vector_retention_unseated(&database_path);
        registry.record_semantic_vector_retention_unseated(&other_database_path);
        assert!(registry.registered_port(&database_path, &scope).is_some());
        assert_eq!(
            registry.semantic_vector_retention_read(&database_path),
            SemanticVectorRetentionReadV1::SemanticUnseated
        );

        registry.release_retained_handle(&database_path);
        assert!(
            registry.registered_port(&database_path, &scope).is_none(),
            "project retirement must drop the exact maintenance-owned database client"
        );
        assert_eq!(
            registry.semantic_vector_retention_read(&database_path),
            SemanticVectorRetentionReadV1::Unknown
        );
        assert!(
            registry
                .registered_port(&other_database_path, &scope)
                .is_some(),
            "exact project retirement must preserve unrelated telemetry clients"
        );
        assert_eq!(
            registry.semantic_vector_retention_read(&other_database_path),
            SemanticVectorRetentionReadV1::SemanticUnseated
        );
        assert!(registry.register_port(&database_path, &scope, || {
            database.storage_telemetry_handle()
        }));
        registry.record_semantic_vector_retention_unseated(&database_path);

        registry.release_retained_handles_for_shutdown();

        assert!(
            registry.registered_port(&database_path, &scope).is_none(),
            "shutdown must drop maintenance-owned database clients"
        );
        assert_eq!(
            registry.semantic_vector_retention_read(&database_path),
            SemanticVectorRetentionReadV1::Unknown
        );
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
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &first),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
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
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &second),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
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
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &page),
            SemanticVectorRetentionCensusOutcome::Accepted
        );

        let generation = tracedecay_domain::VectorGenerationIdV1::new(
            tracedecay_domain::canonical_sha256(&"retired-generation")
                .expect("canonical generation digest"),
        );
        let mutated = tracedecay_graph_db::SemanticVectorRetentionCensus {
            action: tracedecay_graph_db::SemanticVectorRetentionAction::Retired(generation),
            ..page.clone()
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &mutated),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );

        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &page),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
        registry.record_semantic_vector_retention_failure(project);
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );
    }

    #[test]
    fn semantic_unseated_read_is_distinct_and_cleared_by_census_and_failure() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");

        registry.record_semantic_vector_retention_unseated(project);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::SemanticUnseated
        );
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);

        // A failure reset is Unknown, not unseated: the census could not be
        // read even though a semantic runtime is seated.
        registry.record_semantic_vector_retention_failure(project);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );

        // A census page proves a seated runtime and clears the unseated pin.
        registry.record_semantic_vector_retention_unseated(project);
        let shard_id = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.maintenance").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.maintenance").unwrap(),
            tracedecay_domain::ProjectId::new("project.maintenance").unwrap(),
        );
        let revision = tracedecay_store::SemanticVectorStageCensusRevision::new(3).unwrap();
        let complete = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id: shard_id.clone(),
            revision,
            pending: 0,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: Some(tracedecay_store::SemanticVectorProjectCensusReceipt {
                shard_id,
                revision,
                counts: tracedecay_store::SemanticVectorStageCensusCounts {
                    pending: 0,
                    ready: 0,
                    published: 1,
                    cancelled: 0,
                },
                record_digest: tracedecay_domain::canonical_sha256(&"unseated-clear").unwrap(),
            }),
            continuation: None,
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &complete),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
        assert!(matches!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Observed { .. }
        ));

        // Re-pinning unseated discards a stale observed receipt: an unseated
        // runtime cannot vouch for a census taken while it was seated.
        registry.record_semantic_vector_retention_unseated(project);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::SemanticUnseated
        );
        assert!(!registry.semantic_vector_scope_collection_ready(project));
    }

    #[test]
    fn retained_terminal_census_with_receipt_is_observed() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");
        let shard_id = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.maintenance").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.maintenance").unwrap(),
            tracedecay_domain::ProjectId::new("project.maintenance").unwrap(),
        );
        let revision = tracedecay_store::SemanticVectorStageCensusRevision::new(7).unwrap();
        let receipt = tracedecay_store::SemanticVectorProjectCensusReceipt {
            shard_id: shard_id.clone(),
            revision,
            counts: tracedecay_store::SemanticVectorStageCensusCounts {
                pending: 0,
                ready: 0,
                published: 1,
                cancelled: 0,
            },
            record_digest: tracedecay_domain::canonical_sha256(&"retained-head").unwrap(),
        };
        let generation = tracedecay_domain::VectorGenerationIdV1::new(
            tracedecay_domain::canonical_sha256(&"retained-generation")
                .expect("canonical generation digest"),
        );
        let census = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id,
            revision,
            pending: 0,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: Some(receipt.clone()),
            continuation: None,
            action: tracedecay_graph_db::SemanticVectorRetentionAction::Retained(generation),
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &census),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Observed { receipt }
        );
    }

    #[test]
    fn incomplete_terminal_census_resets_progress() {
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
                pending: 0,
                ready: 0,
                published: 1,
                cancelled: 0,
            },
            tracedecay_domain::canonical_sha256(&"paging").unwrap(),
        )
        .expect("valid semantic vector cursor");
        let paging = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id: shard_id.clone(),
            revision,
            pending: 0,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: None,
            continuation: Some(cursor.clone()),
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &paging),
            SemanticVectorRetentionCensusOutcome::Accepted
        );
        assert_eq!(
            registry.semantic_vector_retention_cursor(project),
            Some(cursor)
        );

        let incomplete = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id,
            revision,
            pending: 0,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: None,
            continuation: None,
            action: tracedecay_graph_db::SemanticVectorRetentionAction::Retained(
                tracedecay_domain::VectorGenerationIdV1::new(
                    tracedecay_domain::canonical_sha256(&"retained-incomplete")
                        .expect("canonical generation digest"),
                ),
            ),
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &incomplete),
            SemanticVectorRetentionCensusOutcome::IncompleteTerminalPage
        );
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );
    }

    #[test]
    fn invalid_sum_receipt_is_census_count_overflow() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = std::path::Path::new("/project");
        let shard_id = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.maintenance").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.maintenance").unwrap(),
            tracedecay_domain::ProjectId::new("project.maintenance").unwrap(),
        );
        let revision = tracedecay_store::SemanticVectorStageCensusRevision::new(7).unwrap();
        let census = tracedecay_graph_db::SemanticVectorRetentionCensus {
            shard_id: shard_id.clone(),
            revision,
            pending: 0,
            ready: 0,
            published: 1,
            cancelled: 0,
            complete_receipt: Some(tracedecay_store::SemanticVectorProjectCensusReceipt {
                shard_id,
                revision,
                counts: tracedecay_store::SemanticVectorStageCensusCounts {
                    pending: u64::MAX,
                    ready: 1,
                    published: 0,
                    cancelled: 0,
                },
                record_digest: tracedecay_domain::canonical_sha256(&"overflow").unwrap(),
            }),
            continuation: None,
            action: tracedecay_graph_db::SemanticVectorRetentionAction::None,
        };
        assert_eq!(
            registry.record_semantic_vector_retention_census(project, &census),
            SemanticVectorRetentionCensusOutcome::CensusCountOverflow
        );
        assert_eq!(registry.semantic_vector_retention_cursor(project), None);
        assert_eq!(
            registry.semantic_vector_retention_read(project),
            SemanticVectorRetentionReadV1::Unknown
        );
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

    #[test]
    fn debris_retention_enables_maintenance_without_orphan_gc() {
        let mut retention = crate::config::RetentionConfig::default();
        retention.session_lcm.enabled = false;
        retention.observation.enabled = false;
        retention.orphan_store_gc_days = None;
        retention.incident_debris_retention_days = Some(30);
        retention.compaction = None;

        assert!(super::retention_maintenance_enabled(&retention));
    }

    #[test]
    fn soft_budget_alone_never_enables_destructive_maintenance() {
        let mut retention = crate::config::RetentionConfig::default();
        retention.session_lcm.enabled = false;
        retention.observation.enabled = false;
        retention.orphan_store_gc_days = None;
        retention.incident_debris_retention_days = None;
        retention.compaction = None;
        retention
            .store_soft_budgets_bytes
            .insert("sessions.db".to_string(), 1);

        assert!(
            !super::retention_maintenance_enabled(&retention),
            "soft budgets are Doctor findings, never a retention trigger"
        );
    }

    #[test]
    fn retention_window_conversion_never_wraps_negative() {
        assert_eq!(super::retention_window_secs(u64::MAX), i64::MAX);
    }
}
