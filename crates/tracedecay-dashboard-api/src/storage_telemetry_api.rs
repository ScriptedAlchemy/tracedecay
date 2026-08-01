//! `GET /api/storage/telemetry` — per-store size, free-page ratio, and typed
//! budget/growth dimensions (plan 38 §7 read models over the PR14 envelope).
//!
//! The size samples are **real**: the dashboard invokes the application
//! [`StoreSizeTelemetryPort`] over retained runtime health readers. The runtime
//! owns the `PRAGMA` and `dbstat` reads; this surface only projects their typed
//! result.
//!
//! Both typed dimensions now have a real server-side source:
//! - **budget**: the owner-configurable soft budgets live in the configuration
//!   control plane under [`crate::config::SYNC_RETENTION_SETTING_KEY`]
//!   (`sync.retention.v1` → `store_soft_budgets_bytes`, keyed by store key).
//!   A configured budget is evaluated against the live sample; a store with no
//!   entry reports `unset` — *the owner has not configured a budget*, which is
//!   deliberately distinct from "the server cannot evaluate budgets". A config
//!   or sample the dashboard cannot read reports `unknown`, never a fabricated
//!   "within budget".
//! - **growth**: the daemon persists no historical per-table watermark series,
//!   so growth is served from a bounded in-process watermark ring recorded on
//!   every telemetry sample. The window is therefore **since daemon start**,
//!   and every observed growth read states that coverage explicitly rather
//!   than implying a historical series.
//!
//! Store identity: the dashboard holds several *roles* (`graph`, `memory`,
//! `lcm`, `savings`) that can resolve to the **same** store file — in project
//! storage mode the graph and project-memory roles are the same database. Roles
//! are therefore deduplicated by store file identity: one card per real store,
//! carrying every role it serves, instead of the same store reported twice with
//! identical sizes.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::OnceLock;

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_application::storage::identity::StoreKeyV1;
use tracedecay_application::storage::telemetry::{
    StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreSizeBudgetV1, StoreSizeSampleV1,
    StoreSizeTelemetryPort, TableGrowthTelemetryReadV1,
};
use tracedecay_application::storage::{
    SIGNIFICANT_TABLE_GROWTH_ABSOLUTE_BYTES, SIGNIFICANT_TABLE_GROWTH_PERCENT,
    SIGNIFICANT_TABLE_GROWTH_RELATIVE_FLOOR_BYTES, is_significant_table_growth,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext,
};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, now_micros, scope_from_state,
};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

/// One store's telemetry entry. One entry per distinct store **file**, not per
/// dashboard role.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct StoreTelemetryEntryV1 {
    /// Stable store key (the store's file name), or the raw file name when it is
    /// not a valid [`StoreKeyV1`].
    pub store: String,
    /// The dashboard's primary role label for the store (`graph` / `memory` /
    /// `lcm` / `savings`). Retained for compatibility; see `roles` for the
    /// complete set.
    pub role: String,
    /// Every dashboard role served by this one store file. More than one role
    /// here means the roles share a database, not that a store was duplicated.
    pub roles: Vec<String>,
    /// Display path of the store file.
    pub path: String,
    /// The typed telemetry read: `observed` with a sample, or `unknown` when the
    /// pragma read failed. Never silently healthy.
    pub read: StorageTelemetryReadV1,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub free_page_ratio: Option<f64>,
    pub budget: StoreBudgetDimensionV1,
    pub growth: StoreGrowthDimensionV1,
    /// Per-table payload growth from the `SQLite` `dbstat` watermarks retained by
    /// the production telemetry port.
    pub table_growth: TableGrowthDimensionV1,
}

/// The budget-evaluation dimension, sourced from owner configuration.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StoreBudgetDimensionV1 {
    /// An owner-configured soft budget was evaluated against the live sample.
    Evaluated {
        evaluation: StoreBudgetEvaluationV1,
        /// The owner setting this budget came from.
        setting_key: String,
        reason: String,
    },
    /// The budget source is wired and readable, but this owner configured no
    /// budget for this store. A missing *setting*, not a missing *feature*.
    Unset {
        reason: String,
        /// The setting an owner would set to configure a budget here.
        setting_key: String,
    },
    /// The budget could not be determined: the resolved configuration was
    /// unreadable, or no size sample was observed to evaluate against.
    Unknown { reason: String },
}

/// One recorded store-size watermark.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
pub struct StoreSizeWatermarkV1 {
    /// Wall-clock microseconds at which the size was measured.
    pub measured_at: i64,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// The per-store growth dimension. Growth is only ever reported over the window
/// the server actually observed, and that window is named in `coverage`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StoreGrowthDimensionV1 {
    /// The first watermark of this daemon lifetime: a real measurement with no
    /// earlier point to compare against. Not "zero growth".
    Baseline {
        coverage: String,
        measured_at: i64,
        total_bytes: u64,
        reason: String,
    },
    /// Growth observed across at least two watermarks in the window.
    Observed {
        coverage: String,
        first_measured_at: i64,
        last_measured_at: i64,
        sample_count: usize,
        first_total_bytes: u64,
        current_total_bytes: u64,
        /// Signed delta over the window; a shrinking store reports a negative
        /// number rather than saturating to zero.
        growth_bytes: i64,
        samples: Vec<StoreSizeWatermarkV1>,
    },
    /// No watermark could be recorded because the size read failed.
    Unknown { reason: String },
}

/// Informational threshold applied to per-table payload growth samples.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
pub struct TableGrowthThresholdV1 {
    pub absolute_bytes: u64,
    pub relative_floor_bytes: u64,
    pub relative_percent: u64,
}

/// One significant table-growth sample exposed to the dashboard.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SignificantTableGrowthSampleV1 {
    pub table: String,
    pub previous_bytes: u64,
    pub current_bytes: u64,
    pub growth_bytes: u64,
    pub previous_observed_at: i64,
    pub current_observed_at: i64,
}

/// One current table omitted from the significant-sample list. Numeric evidence
/// remains structured so clients can format units consistently.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TableGrowthOmissionV1 {
    BelowThreshold {
        table: String,
        previous_bytes: u64,
        current_bytes: u64,
        growth_bytes: u64,
        previous_observed_at: i64,
        current_observed_at: i64,
        reason: String,
    },
    BaselinePending {
        table: String,
        current_bytes: u64,
        observed_at: i64,
        reason: String,
    },
}

impl TableGrowthOmissionV1 {
    fn table(&self) -> &str {
        match self {
            Self::BelowThreshold { table, .. } | Self::BaselinePending { table, .. } => table,
        }
    }

    fn reason(&self) -> &str {
        match self {
            Self::BelowThreshold { reason, .. } | Self::BaselinePending { reason, .. } => reason,
        }
    }
}

/// Per-store typed table-growth state. Unavailable reads carry no byte values;
/// each state includes source coverage and explicit omissions.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TableGrowthDimensionV1 {
    Observed {
        coverage: DashboardCoverageV1,
        significant_samples: Vec<SignificantTableGrowthSampleV1>,
        omissions: Vec<TableGrowthOmissionV1>,
        omission_reasons: Vec<String>,
    },
    BaselineEstablished {
        coverage: DashboardCoverageV1,
        observed_at: i64,
        tables_observed: u64,
        omission_reasons: Vec<String>,
    },
    Unsupported {
        coverage: DashboardCoverageV1,
        omission_reasons: Vec<String>,
    },
    Denied {
        coverage: DashboardCoverageV1,
        omission_reasons: Vec<String>,
    },
    Unknown {
        coverage: DashboardCoverageV1,
        omission_reasons: Vec<String>,
    },
}

/// Telemetry payload: one entry per distinct store the dashboard holds a
/// connection to.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct StorageTelemetryPayloadV1 {
    pub stores: Vec<StoreTelemetryEntryV1>,
    /// Where budgets come from, stated once for the whole read.
    pub budget_note: String,
    /// The growth window's coverage, stated once for the whole read.
    pub growth_note: String,
    /// The exact significance rule applied to `table_growth` samples.
    pub table_growth_threshold: TableGrowthThresholdV1,
    /// Aggregate coverage across the expected per-store table-growth reads.
    pub table_growth_coverage: DashboardCoverageV1,
}

/// The owner setting path that configures a store's soft byte budget.
pub const BUDGET_SETTING_KEY: &str = "sync.retention.v1 store_soft_budgets_bytes";
const BUDGET_UNSET_REASON: &str = "no soft size budget is configured by the owner for this store (set \
     sync.retention.v1 store_soft_budgets_bytes for the store key to configure one)";
const BUDGET_NOTE: &str = "budgets are owner configuration: sync.retention.v1 store_soft_budgets_bytes, keyed by store \
     key; a store with no entry reports unset (no budget configured), never a fabricated pass";
const GROWTH_COVERAGE: &str = "since-daemon-start: bounded in-process watermark ring recorded on each telemetry sample, not \
     a persisted historical series";
const GROWTH_NOTE: &str = "growth is measured over the store-size watermarks this daemon has recorded since it started; \
     no persisted historical watermark series exists, so the window is not historical";
const GROWTH_BASELINE_REASON: &str =
    "first watermark recorded in this daemon lifetime; a growth delta needs a second sample";
const GROWTH_UNKNOWN_REASON: &str =
    "no watermark could be recorded because the store size read did not produce a sample";
const TABLE_GROWTH_BASELINE_REASON: &str =
    "no baseline yet; this read established the first per-table payload watermark";
const TABLE_GROWTH_UNSUPPORTED_REASON: &str =
    "per-table payload growth measurement is unsupported for this store";
const TABLE_GROWTH_DENIED_REASON: &str =
    "per-table payload growth measurement was denied for this store";
const TABLE_GROWTH_UNKNOWN_REASON: &str =
    "per-table payload growth measurement is unavailable for this store";
const BUDGET_NO_SAMPLE_REASON: &str =
    "no observed size sample, so a configured budget could not be evaluated";

/// One store the dashboard holds a connection to, sampled once.
///
#[derive(Clone, Debug)]
struct SampledStoreV1 {
    /// The store file name, used as the [`StoreKeyV1`] and as the owner budget
    /// configuration key.
    pub store: String,
    /// Display path of the store file.
    pub path: String,
    /// Every dashboard role served by this one store file.
    pub roles: Vec<String>,
    /// The typed size read: `observed` with a sample, or `unknown`.
    pub read: StorageTelemetryReadV1,
    /// `None` is used only by budget-only collection, which deliberately does
    /// not advance the table-growth watermark.
    pub table_growth_read: Option<TableGrowthTelemetryReadV1>,
    /// Why this expected store could not be examined. Kept alongside the
    /// sample so envelope coverage can name the actual omission rather than
    /// silently shrinking its denominator.
    pub omission_reason: Option<String>,
}

impl SampledStoreV1 {
    /// The observed size sample, when the pragma read produced one.
    const fn sample(&self) -> Option<&StoreSizeSampleV1> {
        match &self.read {
            StorageTelemetryReadV1::Observed { sample } => Some(sample),
            _ => None,
        }
    }

    /// The dashboard's primary role label for this store.
    fn primary_role(&self) -> String {
        self.roles
            .first()
            .cloned()
            .unwrap_or_else(|| "store".to_string())
    }
}

/// The owner-configured soft budget for one store, resolved from configuration.
///
/// `Unset` is deliberately distinct from `Unknown`: the owner configured no
/// budget (a missing *setting*), versus the configuration could not be read.
/// Neither is ever a fabricated pass.
#[derive(Clone, Debug)]
enum ResolvedStoreBudgetV1 {
    Configured(StoreSizeBudgetV1),
    Unset,
    Unknown(String),
}

/// Aggregate source coverage for the `OverBudgetStore` producer. This is not a
/// health verdict: it records how many real store samples could be evaluated,
/// how many owner budgets are unset, and how many reads remain undetermined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreBudgetSourceSummaryV1 {
    pub stores: usize,
    pub evaluated: usize,
    pub over_budget: usize,
    pub unset: usize,
    pub unknown: usize,
}

/// Resolve one store's owner-configured soft budget from the retention config.
fn resolve_store_budget(
    store_name: &str,
    retention: Option<&crate::config::RetentionConfig>,
) -> ResolvedStoreBudgetV1 {
    let Some(retention) = retention else {
        return ResolvedStoreBudgetV1::Unknown(
            "the resolved runtime configuration could not be read, so a configured budget could \
             not be determined"
                .to_string(),
        );
    };
    match retention.store_soft_budget(store_name) {
        Ok(Some(budget)) => ResolvedStoreBudgetV1::Configured(budget),
        Ok(None) => ResolvedStoreBudgetV1::Unset,
        Err(error) => ResolvedStoreBudgetV1::Unknown(format!(
            "the configured soft budget for this store is invalid: {error}"
        )),
    }
}

/// Enumerate every store the dashboard holds a connection to, deduplicated by
/// store file identity, each with one live size sample.
async fn collect_store_samples(
    state: &DashboardState,
    include_table_growth: bool,
) -> Vec<SampledStoreV1> {
    let mut entries: Vec<SampledStoreV1> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let Some(context) = storage_telemetry_context(state) else {
        for (role, path) in [
            ("graph", state.graph_db_path.as_str()),
            ("memory", state.mem_db_path.as_str()),
            ("lcm", state.lcm_db_path.as_str()),
            ("savings", state.savings_db_path.as_str()),
        ] {
            push_or_merge_unknown_role(
                &mut entries,
                &mut seen,
                role,
                path,
                "exact project telemetry scope is unavailable".to_string(),
            );
        }
        return entries;
    };

    // Graph store: use the retained database runtime that owns this exact path.
    push_or_merge_role(
        &mut entries,
        &mut seen,
        "graph",
        &state.graph_db_path,
        state.graph_telemetry_handle.clone(),
        &context,
        include_table_growth,
    )
    .await;
    // Project-memory store. In project storage mode this resolves to the same
    // file as the graph role and is merged into that entry rather than
    // reported as a second, identically-sized store.
    push_or_merge_role(
        &mut entries,
        &mut seen,
        "memory",
        &state.mem_db_path,
        state.mem_db.storage_telemetry_handle().ok(),
        &context,
        include_table_growth,
    )
    .await;
    // LCM session store, when a retained runtime is held.
    if let Some(db) = &state.lcm_db {
        push_or_merge_role(
            &mut entries,
            &mut seen,
            "lcm",
            &state.lcm_db_path,
            db.storage_telemetry_handle().ok(),
            &context,
            include_table_growth,
        )
        .await;
    }
    // Global accounting store, when available.
    if let Some(db) = &state.savings_db {
        push_or_merge_role(
            &mut entries,
            &mut seen,
            "savings",
            &state.savings_db_path,
            db.storage_telemetry_handle().ok(),
            &context,
            include_table_growth,
        )
        .await;
    }

    entries
}

/// Read the same real store samples and pinned owner configuration as the
/// telemetry route, but without recording a growth watermark. The storage
/// finding route uses this to state whether `OverBudgetStore` was evaluated,
/// unset, or only partially observable.
pub async fn budget_source_summary(state: &DashboardState) -> StoreBudgetSourceSummaryV1 {
    let samples = collect_store_samples(state, false).await;
    let mut summary = StoreBudgetSourceSummaryV1 {
        stores: samples.len(),
        ..StoreBudgetSourceSummaryV1::default()
    };
    for sampled in samples {
        match budget_dimension(
            &sampled.store,
            sampled.sample(),
            Some(&state.retention_config),
        ) {
            StoreBudgetDimensionV1::Evaluated { evaluation, .. } => {
                summary.evaluated += 1;
                if evaluation.is_over_budget() {
                    summary.over_budget += 1;
                }
            }
            StoreBudgetDimensionV1::Unset { .. } => summary.unset += 1,
            StoreBudgetDimensionV1::Unknown { .. } => summary.unknown += 1,
        }
    }
    summary
}

/// Upper bound on watermarks retained per store, and on distinct stores tracked
/// by one process. Both keep the daemon-lifetime ring strictly bounded.
const MAX_WATERMARKS_PER_STORE: usize = 128;
const MAX_TRACKED_STORES: usize = 256;

/// Bounded, daemon-lifetime store-size watermark history.
///
/// This is deliberately process-global rather than per-`DashboardState`: the
/// honest window it serves is "since this daemon started", which is exactly the
/// lifetime of the process, and every dashboard state in the process observes
/// the same stores.
#[derive(Debug, Default)]
pub struct StoreSizeHistoryV1 {
    stores: HashMap<String, VecDeque<StoreSizeWatermarkV1>>,
}

impl StoreSizeHistoryV1 {
    /// Record one watermark for `store` and return the retained window.
    fn record(
        &mut self,
        store: &str,
        watermark: StoreSizeWatermarkV1,
    ) -> Vec<StoreSizeWatermarkV1> {
        if !self.stores.contains_key(store) && self.stores.len() >= MAX_TRACKED_STORES {
            // Refuse to grow unboundedly; report the single point we hold.
            return vec![watermark];
        }
        let series = self.stores.entry(store.to_string()).or_default();
        series.push_back(watermark);
        while series.len() > MAX_WATERMARKS_PER_STORE {
            series.pop_front();
        }
        series.iter().copied().collect()
    }
}

fn history() -> &'static Mutex<StoreSizeHistoryV1> {
    static HISTORY: OnceLock<Mutex<StoreSizeHistoryV1>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(StoreSizeHistoryV1::default()))
}

/// `GET /api/storage/telemetry`
pub async fn telemetry(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<StorageTelemetryPayloadV1>> {
    // The owner-configured soft budgets, resolved once per read from the pinned
    // runtime configuration.
    let retention = &state.retention_config;

    let samples = collect_store_samples(&state, true).await;
    let coverage = telemetry_coverage(&samples);
    let entries: Vec<StoreTelemetryEntryV1> = samples
        .into_iter()
        .map(|sampled| telemetry_entry(sampled, Some(retention)))
        .collect();
    let table_growth_coverage = table_growth_payload_coverage(&entries);

    let payload = StorageTelemetryPayloadV1 {
        stores: entries,
        budget_note: BUDGET_NOTE.to_string(),
        growth_note: GROWTH_NOTE.to_string(),
        table_growth_threshold: TableGrowthThresholdV1 {
            absolute_bytes: SIGNIFICANT_TABLE_GROWTH_ABSOLUTE_BYTES,
            relative_floor_bytes: SIGNIFICANT_TABLE_GROWTH_RELATIVE_FLOOR_BYTES,
            relative_percent: SIGNIFICANT_TABLE_GROWTH_PERCENT,
        },
        table_growth_coverage,
    };

    let envelope = DashboardEnvelopeV1::ready(scope_from_state(&state), coverage, payload)
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "use-case.dashboard.storage.telemetry.refresh",
        )]);
    Json(envelope)
}

type CachedStoreTelemetryPort = (
    ManifestDigest,
    tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort,
);

fn storage_telemetry_ports() -> &'static Mutex<HashMap<String, CachedStoreTelemetryPort>> {
    static PORTS: OnceLock<Mutex<HashMap<String, CachedStoreTelemetryPort>>> = OnceLock::new();
    PORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn storage_telemetry_context(state: &DashboardState) -> Option<RequestContext> {
    // The exact scope was resolved once when the dashboard state was built;
    // this handler never re-resolves repository/worktree identity from paths.
    let scope = state.resolved_scope.clone()?;
    let now = UtcMicros(now_micros());
    let expires_at = UtcMicros(now.0.saturating_add(30_000_000));
    let actor = ActorId::new("actor.dashboard.storage-telemetry").ok()?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::DashboardStorageTelemetry).ok()?;
    let manifest = canonical_sha256(&(
        "tracedecay.dashboard.storage-telemetry-grant.v1",
        &scope.scope_digest,
    ))
    .ok()?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.dashboard.storage-telemetry.{}",
            request_id.as_str()
        ))
        .ok()?,
        1,
        manifest,
        actor.clone(),
        now,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.application.storage.telemetry").ok()?]),
        BTreeSet::from([UseCaseId::new("use-case.application.storage.telemetry.read").ok()?]),
        DisclosureClass::Metadata,
    )
    .ok()?;
    RequestContext::new(
        actor,
        scope,
        grant,
        request_id.clone(),
        Deadline::new(expires_at).ok()?,
        CancellationContext::active(format!(
            "cancel.dashboard.storage-telemetry.{}",
            request_id.as_str()
        ))
        .ok()?,
    )
    .ok()
}

fn storage_telemetry_port(
    path: &str,
    handle: Option<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle>,
    context: &RequestContext,
) -> Option<(
    StoreKeyV1,
    tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort,
)> {
    let store = StoreKeyV1::new(store_file_name(path)).ok()?;
    let identity = store_identity(path);
    let mut ports = storage_telemetry_ports()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((digest, port)) = ports.get(&identity)
        && digest == &context.scope().scope_digest
    {
        return Some((store, port.clone()));
    }
    let port = tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort::new(
        handle?,
        store.clone(),
        context.scope().clone(),
        std::time::Duration::from_secs(5),
    );
    ports.insert(
        identity,
        (context.scope().scope_digest.clone(), port.clone()),
    );
    Some((store, port))
}

/// Sample a role's store, or merge the role into the existing entry when the
/// role resolves to a store file already sampled in this read.
async fn push_or_merge_role(
    entries: &mut Vec<SampledStoreV1>,
    seen: &mut HashMap<String, usize>,
    role: &str,
    path: &str,
    handle: Option<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle>,
    context: &RequestContext,
    include_table_growth: bool,
) {
    let identity = store_identity(path);
    if let Some(index) = seen.get(&identity).copied() {
        let entry = &mut entries[index];
        if !entry.roles.iter().any(|existing| existing == role) {
            entry.roles.push(role.to_string());
        }
        return;
    }
    let entry = sample_store(role, path, handle, context, include_table_growth).await;
    seen.insert(identity, entries.len());
    entries.push(entry);
}

/// Preserve an expected dashboard-held store when its snapshot could not be
/// opened. The store stays in the denominator and projects as typed `unknown`.
fn push_or_merge_unknown_role(
    entries: &mut Vec<SampledStoreV1>,
    seen: &mut HashMap<String, usize>,
    role: &str,
    path: &str,
    omission_reason: String,
) {
    let identity = store_identity(path);
    if let Some(index) = seen.get(&identity).copied() {
        let entry = &mut entries[index];
        if !entry.roles.iter().any(|existing| existing == role) {
            entry.roles.push(role.to_string());
        }
        if entry.sample().is_none() && entry.omission_reason.is_none() {
            entry.omission_reason = Some(omission_reason);
        }
        return;
    }

    let store_name = store_file_name(path);
    let store = StoreKeyV1::new(store_name.clone()).unwrap_or_else(|_| {
        StoreKeyV1::new(sanitize_store_key(&store_name)).unwrap_or_else(|_| fallback_store_key())
    });
    let table_growth_store = store.clone();
    seen.insert(identity, entries.len());
    entries.push(SampledStoreV1 {
        store: store_name,
        path: path.to_string(),
        roles: vec![role.to_string()],
        read: StorageTelemetryReadV1::Unknown { store },
        table_growth_read: Some(TableGrowthTelemetryReadV1::Unknown {
            store: table_growth_store,
        }),
        omission_reason: Some(omission_reason),
    });
}

/// Coverage over the expected, deduplicated set of dashboard-held stores.
/// Unknown stores remain eligible and contribute their concrete failure reason.
fn telemetry_coverage(samples: &[SampledStoreV1]) -> DashboardCoverageV1 {
    let total = samples.len() as u64;
    let observed = samples
        .iter()
        .filter(|sampled| sampled.sample().is_some())
        .count() as u64;
    if observed == total {
        return DashboardCoverageV1::complete(total, "dashboard_held_stores");
    }

    let omission_reasons = samples
        .iter()
        .filter_map(|sampled| sampled.omission_reason.clone())
        .collect();
    DashboardCoverageV1::partial(total, observed, "dashboard_held_stores", omission_reasons)
}

/// Identity of a store *file*, used to deduplicate roles that share a database.
/// Canonicalization resolves symlinks and relative spellings; an
/// uncanonicalizable path falls back to its own spelling so two genuinely
/// distinct stores are never merged.
fn store_identity(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(
        |_| path.to_string(),
        |resolved| resolved.display().to_string(),
    )
}

/// Sample one store through the retained application telemetry port. A missing
/// runtime or failed read produces typed `unknown`, never a fabricated size.
async fn sample_store(
    role: &str,
    path: &str,
    handle: Option<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle>,
    context: &RequestContext,
    include_table_growth: bool,
) -> SampledStoreV1 {
    let store_name = store_file_name(path);
    let (read, table_growth_read, omission_reason) =
        match storage_telemetry_port(path, handle, context) {
            Some((store, port)) => {
                let (read, table_growth_read) = if include_table_growth {
                    let (read, table_growth) = tokio::join!(
                        port.store_size(context, &store),
                        port.table_growth(context, &store)
                    );
                    (read, Some(table_growth))
                } else {
                    (port.store_size(context, &store).await, None)
                };
                let omission = (!matches!(read, StorageTelemetryReadV1::Observed { .. }))
                    .then(|| format!("store telemetry read failed for {role} role"));
                (read, table_growth_read, omission)
            }
            // The store file name is not a valid store key; report the read as
            // unknown against a sanitized fallback key rather than inventing size.
            None => {
                let store = StoreKeyV1::new(sanitize_store_key(&store_name))
                    .unwrap_or_else(|_| fallback_store_key());
                (
                    StorageTelemetryReadV1::Unknown {
                        store: store.clone(),
                    },
                    include_table_growth.then_some(TableGrowthTelemetryReadV1::Unknown { store }),
                    Some(format!(
                        "store telemetry runtime is unavailable for {role} role"
                    )),
                )
            }
        };

    SampledStoreV1 {
        store: store_name,
        path: path.to_string(),
        roles: vec![role.to_string()],
        read,
        table_growth_read,
        omission_reason,
    }
}

/// Project one sampled store onto the telemetry read model, adding the budget
/// and growth dimensions.
fn telemetry_entry(
    sampled: SampledStoreV1,
    retention: Option<&crate::config::RetentionConfig>,
) -> StoreTelemetryEntryV1 {
    let sample = sampled.sample();
    let (total_bytes, free_bytes, free_page_ratio) = sample.map_or((None, None, None), |sample| {
        (
            Some(sample.total_bytes().get()),
            Some(sample.free_bytes().get()),
            Some(sample.free_page_ratio().as_f64()),
        )
    });
    let budget = budget_dimension(&sampled.store, sample, retention);
    let growth = growth_dimension(&store_identity(&sampled.path), total_bytes, free_bytes);
    let role = sampled.primary_role();
    let table_growth = table_growth_dimension(sampled.table_growth_read.unwrap_or_else(|| {
        let store = StoreKeyV1::new(sanitize_store_key(&sampled.store))
            .unwrap_or_else(|_| fallback_store_key());
        TableGrowthTelemetryReadV1::Unknown { store }
    }));

    StoreTelemetryEntryV1 {
        store: sampled.store,
        role,
        roles: sampled.roles,
        path: sampled.path,
        read: sampled.read,
        total_bytes,
        free_bytes,
        free_page_ratio,
        budget,
        growth,
        table_growth,
    }
}

fn unavailable_table_growth_coverage(reason: &str) -> DashboardCoverageV1 {
    DashboardCoverageV1::partial(1, 0, "store_table_growth_reads", vec![reason.to_string()])
}

/// Project the application telemetry read into the dashboard contract without
/// inventing bytes for unavailable states or silently dropping below-threshold
/// tables.
fn table_growth_dimension(read: TableGrowthTelemetryReadV1) -> TableGrowthDimensionV1 {
    match read {
        TableGrowthTelemetryReadV1::Observed {
            samples,
            baseline_pending,
            ..
        } => {
            let denominator = u64::try_from(samples.len().saturating_add(baseline_pending.len()))
                .unwrap_or(u64::MAX);
            let examined = u64::try_from(samples.len()).unwrap_or(u64::MAX);
            let mut significant_samples = Vec::new();
            let mut omissions = Vec::new();
            for sample in samples {
                if is_significant_table_growth(&sample) {
                    significant_samples.push(SignificantTableGrowthSampleV1 {
                        table: sample.table.as_str().to_string(),
                        previous_bytes: sample.previous_bytes.get(),
                        current_bytes: sample.current_bytes.get(),
                        growth_bytes: sample.growth_bytes().get(),
                        previous_observed_at: sample.previous_observed_at.0,
                        current_observed_at: sample.current_observed_at.0,
                    });
                } else {
                    omissions.push(TableGrowthOmissionV1::BelowThreshold {
                        table: sample.table.as_str().to_string(),
                        previous_bytes: sample.previous_bytes.get(),
                        current_bytes: sample.current_bytes.get(),
                        growth_bytes: sample.growth_bytes().get(),
                        previous_observed_at: sample.previous_observed_at.0,
                        current_observed_at: sample.current_observed_at.0,
                        reason:
                            "observed growth was below the informational significance threshold"
                                .to_string(),
                    });
                }
            }
            let mut coverage_omission_reasons = Vec::new();
            for pending in baseline_pending {
                let reason = format!(
                    "{}: no previous table watermark exists; baseline pending",
                    pending.table.as_str()
                );
                coverage_omission_reasons.push(reason.clone());
                omissions.push(TableGrowthOmissionV1::BaselinePending {
                    table: pending.table.as_str().to_string(),
                    current_bytes: pending.current_bytes.get(),
                    observed_at: pending.observed_at.0,
                    reason,
                });
            }
            let coverage = if coverage_omission_reasons.is_empty() {
                DashboardCoverageV1::complete(denominator, "current_tables")
            } else {
                DashboardCoverageV1::partial(
                    denominator,
                    examined,
                    "current_tables",
                    coverage_omission_reasons,
                )
            };
            let omission_reasons = omissions
                .iter()
                .map(|omission| format!("{}: {}", omission.table(), omission.reason()))
                .collect();
            TableGrowthDimensionV1::Observed {
                coverage,
                significant_samples,
                omissions,
                omission_reasons,
            }
        }
        TableGrowthTelemetryReadV1::BaselineEstablished {
            observed_at,
            tables_observed,
            ..
        } => {
            let reason = TABLE_GROWTH_BASELINE_REASON.to_string();
            TableGrowthDimensionV1::BaselineEstablished {
                coverage: unavailable_table_growth_coverage(&reason),
                observed_at: observed_at.0,
                tables_observed,
                omission_reasons: vec![reason],
            }
        }
        TableGrowthTelemetryReadV1::Unsupported { .. } => {
            let reason = TABLE_GROWTH_UNSUPPORTED_REASON.to_string();
            TableGrowthDimensionV1::Unsupported {
                coverage: unavailable_table_growth_coverage(&reason),
                omission_reasons: vec![reason],
            }
        }
        TableGrowthTelemetryReadV1::Denied { .. } => {
            let reason = TABLE_GROWTH_DENIED_REASON.to_string();
            TableGrowthDimensionV1::Denied {
                coverage: unavailable_table_growth_coverage(&reason),
                omission_reasons: vec![reason],
            }
        }
        TableGrowthTelemetryReadV1::Unknown { .. } => {
            let reason = TABLE_GROWTH_UNKNOWN_REASON.to_string();
            TableGrowthDimensionV1::Unknown {
                coverage: unavailable_table_growth_coverage(&reason),
                omission_reasons: vec![reason],
            }
        }
    }
}

fn table_growth_payload_coverage(entries: &[StoreTelemetryEntryV1]) -> DashboardCoverageV1 {
    let denominator = entries.len() as u64;
    let examined = entries
        .iter()
        .filter(|entry| {
            matches!(
                &entry.table_growth,
                TableGrowthDimensionV1::Observed { coverage, .. } if coverage.is_complete()
            )
        })
        .count() as u64;
    if examined == denominator {
        return DashboardCoverageV1::complete(denominator, "store_table_growth_reads");
    }

    let omission_reasons = entries
        .iter()
        .flat_map(|entry| -> Vec<String> {
            match &entry.table_growth {
                TableGrowthDimensionV1::Observed { coverage, .. } => coverage
                    .omission_reasons
                    .iter()
                    .map(|reason| format!("{}: {reason}", entry.store))
                    .collect(),
                TableGrowthDimensionV1::BaselineEstablished {
                    omission_reasons, ..
                }
                | TableGrowthDimensionV1::Unsupported {
                    omission_reasons, ..
                }
                | TableGrowthDimensionV1::Denied {
                    omission_reasons, ..
                }
                | TableGrowthDimensionV1::Unknown {
                    omission_reasons, ..
                } => omission_reasons
                    .iter()
                    .map(|reason| format!("{}: {reason}", entry.store))
                    .collect(),
            }
        })
        .collect();
    DashboardCoverageV1::partial(
        denominator,
        examined,
        "store_table_growth_reads",
        omission_reasons,
    )
}

/// Resolve the budget dimension for one store from owner configuration.
fn budget_dimension(
    store_name: &str,
    sample: Option<&StoreSizeSampleV1>,
    retention: Option<&crate::config::RetentionConfig>,
) -> StoreBudgetDimensionV1 {
    let budget = match resolve_store_budget(store_name, retention) {
        ResolvedStoreBudgetV1::Configured(budget) => budget,
        ResolvedStoreBudgetV1::Unset => {
            return StoreBudgetDimensionV1::Unset {
                reason: BUDGET_UNSET_REASON.to_string(),
                setting_key: BUDGET_SETTING_KEY.to_string(),
            };
        }
        ResolvedStoreBudgetV1::Unknown(reason) => {
            return StoreBudgetDimensionV1::Unknown { reason };
        }
    };
    let Some(sample) = sample else {
        return StoreBudgetDimensionV1::Unknown {
            reason: BUDGET_NO_SAMPLE_REASON.to_string(),
        };
    };
    match budget.evaluate(sample) {
        Ok(evaluation) => StoreBudgetDimensionV1::Evaluated {
            evaluation,
            setting_key: BUDGET_SETTING_KEY.to_string(),
            reason: format!(
                "evaluated against the owner-configured soft limit of {} bytes",
                budget.soft_limit_bytes.get()
            ),
        },
        Err(error) => StoreBudgetDimensionV1::Unknown {
            reason: format!("the configured budget could not be evaluated: {error}"),
        },
    }
}

/// Record this read's watermark and derive the growth dimension over the
/// daemon-lifetime window.
fn growth_dimension(
    store_identity: &str,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
) -> StoreGrowthDimensionV1 {
    let (Some(total_bytes), Some(free_bytes)) = (total_bytes, free_bytes) else {
        return StoreGrowthDimensionV1::Unknown {
            reason: GROWTH_UNKNOWN_REASON.to_string(),
        };
    };
    let watermark = StoreSizeWatermarkV1 {
        measured_at: now_micros(),
        total_bytes,
        free_bytes,
    };
    let samples = match history().lock() {
        Ok(mut history) => history.record(store_identity, watermark),
        Err(poisoned) => poisoned.into_inner().record(store_identity, watermark),
    };
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return StoreGrowthDimensionV1::Unknown {
            reason: GROWTH_UNKNOWN_REASON.to_string(),
        };
    };
    if samples.len() < 2 {
        return StoreGrowthDimensionV1::Baseline {
            coverage: GROWTH_COVERAGE.to_string(),
            measured_at: last.measured_at,
            total_bytes: last.total_bytes,
            reason: GROWTH_BASELINE_REASON.to_string(),
        };
    }
    let growth_bytes = i64::try_from(last.total_bytes)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(first.total_bytes).unwrap_or(i64::MAX));
    StoreGrowthDimensionV1::Observed {
        coverage: GROWTH_COVERAGE.to_string(),
        first_measured_at: first.measured_at,
        last_measured_at: last.measured_at,
        sample_count: samples.len(),
        first_total_bytes: first.total_bytes,
        current_total_bytes: last.total_bytes,
        growth_bytes,
        samples,
    }
}

fn store_file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.to_string(), str::to_string)
}

/// Reduce an invalid store file name to a bounded, control-free key.
fn sanitize_store_key(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "store".to_string()
    } else {
        trimmed.to_string()
    }
}

fn fallback_store_key() -> StoreKeyV1 {
    match StoreKeyV1::new("store") {
        Ok(store) => store,
        Err(_) => unreachable!("hard-coded fallback store key is valid"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::RetentionConfig;
    use crate::read_model::{DashboardDomainStateV1, DashboardFreshnessStateV1};

    async fn state_for_test() -> (tempfile::TempDir, DashboardState, u64) {
        let (project, state) =
            crate::events_api::dashboard_state_fixture("project.dashboard-storage-telemetry").await;
        let (page_size, page_count, _) = state
            .mem_db
            .storage_page_counts()
            .await
            .expect("authoritative graph page counts");
        let graph_total_bytes = page_size.saturating_mul(page_count);
        (project, state, graph_total_bytes)
    }

    #[tokio::test]
    async fn storage_telemetry_context_reuses_the_state_resolved_scope() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state, _) = state_for_test().await;

        let context = storage_telemetry_context(&state).expect("telemetry context");

        // The per-request application context is minted from the exact scope
        // resolved once at state construction; the handler never re-resolves
        // repository/worktree identity from paths ad hoc.
        assert_eq!(
            context.scope(),
            state.resolved_scope.as_ref().expect("state resolved scope"),
        );
        context.scope().validate().expect("valid scope");
    }

    #[tokio::test]
    async fn storage_telemetry_without_resolved_scope_fails_closed() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state, _) = state_for_test().await;
        state.resolved_scope = None;

        // No exact scope means no per-request application context; the handler
        // reports every store as typed unknown, never a fabricated read.
        assert!(storage_telemetry_context(&state).is_none());
        let samples = collect_store_samples(&state, false).await;
        assert!(!samples.is_empty());
        for sample in &samples {
            assert!(
                matches!(sample.read, StorageTelemetryReadV1::Unknown { .. }),
                "store {} should be typed unknown without an exact scope",
                sample.store
            );
            assert_eq!(
                sample.omission_reason.as_deref(),
                Some("exact project telemetry scope is unavailable"),
            );
        }
    }

    #[tokio::test]
    async fn telemetry_reports_real_observed_sizes_for_held_stores() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state, graph_total_bytes) = state_for_test().await;
        let Json(envelope) = telemetry(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Ready);
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Fresh);
        assert!(
            !envelope.payload.stores.is_empty(),
            "dashboard always holds at least the graph and memory stores"
        );

        for entry in &envelope.payload.stores {
            assert!(
                matches!(entry.read, StorageTelemetryReadV1::Observed { .. }),
                "store {} should have an observed size read",
                entry.store
            );
            assert!(
                entry.total_bytes.unwrap_or(0) > 0,
                "store {} sized",
                entry.store
            );
            // No budget is configured in a fresh project: the honest state is
            // "unset by owner", never "unsupported by server".
            assert!(
                matches!(entry.budget, StoreBudgetDimensionV1::Unset { .. }),
                "store {} should report an unset budget, got {:?}",
                entry.store,
                entry.budget
            );
            // Growth is real, sourced from the watermark ring, and states its
            // since-daemon-start coverage.
            match &entry.growth {
                StoreGrowthDimensionV1::Baseline { coverage, .. }
                | StoreGrowthDimensionV1::Observed { coverage, .. } => {
                    assert!(coverage.contains("since-daemon-start"));
                }
                StoreGrowthDimensionV1::Unknown { reason } => {
                    panic!("store {} growth should be real, got {reason}", entry.store)
                }
            }
            assert!(!entry.roles.is_empty());
            assert!(entry.roles.contains(&entry.role));
        }
        let graph = envelope
            .payload
            .stores
            .iter()
            .find(|entry| entry.roles.iter().any(|role| role == "graph"))
            .expect("graph store entry");
        assert_eq!(
            graph.total_bytes,
            Some(graph_total_bytes),
            "graph bytes must come from the retained graph runtime's real page counts"
        );

        // Complete coverage carries a real denominator equal to the store count.
        assert!(envelope.coverage.is_complete());
        assert_eq!(
            envelope.coverage.denominator,
            Some(envelope.payload.stores.len() as u64)
        );
    }

    #[tokio::test]
    async fn roles_sharing_one_store_file_are_reported_once_with_both_roles() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state, _) = state_for_test().await;
        let Json(envelope) = telemetry(State(state)).await;

        // No two entries may name the same store file: identical sizes reported
        // twice was the duplicate-card defect this guards.
        let mut identities: Vec<String> = envelope
            .payload
            .stores
            .iter()
            .map(|entry| store_identity(&entry.path))
            .collect();
        let before = identities.len();
        identities.sort();
        identities.dedup();
        assert_eq!(
            before,
            identities.len(),
            "one entry per distinct store file"
        );

        // The graph and project-memory roles share one database in project
        // storage mode, so a single entry carries both roles.
        let shared = envelope
            .payload
            .stores
            .iter()
            .find(|entry| entry.roles.contains(&"graph".to_string()))
            .expect("graph role served");
        assert!(
            shared.roles.contains(&"memory".to_string()),
            "graph and memory share one store file; roles: {:?}",
            shared.roles
        );
    }

    #[test]
    fn two_roles_backed_by_one_file_produce_one_entry_carrying_both_roles() {
        // The graph and project-memory roles resolve to the same database file
        // in project storage mode. Reporting them as two entries produced two
        // cards with byte-identical sizes; they must merge into one store.
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("shared.db");
        let display = path.display().to_string();

        let mut entries = Vec::new();
        let mut seen = HashMap::new();
        push_or_merge_unknown_role(
            &mut entries,
            &mut seen,
            "graph",
            &display,
            "test fixture".to_string(),
        );
        push_or_merge_unknown_role(
            &mut entries,
            &mut seen,
            "memory",
            &display,
            "test fixture".to_string(),
        );

        assert_eq!(entries.len(), 1, "one file is one store");
        assert_eq!(entries[0].primary_role(), "graph");
        assert_eq!(entries[0].roles, vec!["graph", "memory"]);

        // A genuinely distinct file is still its own entry.
        let other = directory.path().join("other.db");
        let other_display = other.display().to_string();
        push_or_merge_unknown_role(
            &mut entries,
            &mut seen,
            "savings",
            &other_display,
            "test fixture".to_string(),
        );
        assert_eq!(entries.len(), 2, "distinct files are never merged");
    }

    #[test]
    fn configured_budget_is_evaluated_and_missing_budget_is_unset_not_unsupported() {
        let store = StoreKeyV1::new("probe.db").expect("key");
        let sample = StoreSizeSampleV1 {
            store: store.clone(),
            page_size_bytes: 4096,
            page_count: 100,
            freelist_pages: 1,
            observed_at: UtcMicros(now_micros()),
        };

        // No owner entry -> unset (a missing setting, not a missing feature).
        let empty = RetentionConfig::default();
        let unset = budget_dimension("probe.db", Some(&sample), Some(&empty));
        assert!(matches!(unset, StoreBudgetDimensionV1::Unset { .. }));

        // Owner-configured limit below the observed size -> over budget.
        let mut configured = RetentionConfig::default();
        configured
            .store_soft_budgets_bytes
            .insert("probe.db".to_string(), 1024);
        let evaluated = budget_dimension("probe.db", Some(&sample), Some(&configured));
        match evaluated {
            StoreBudgetDimensionV1::Evaluated { evaluation, .. } => {
                assert!(evaluation.is_over_budget());
            }
            other => panic!("expected an evaluated budget, got {other:?}"),
        }

        // Owner-configured limit above the observed size -> within budget.
        let mut generous = RetentionConfig::default();
        generous
            .store_soft_budgets_bytes
            .insert("probe.db".to_string(), 10_000_000);
        match budget_dimension("probe.db", Some(&sample), Some(&generous)) {
            StoreBudgetDimensionV1::Evaluated { evaluation, .. } => {
                assert!(!evaluation.is_over_budget());
            }
            other => panic!("expected an evaluated budget, got {other:?}"),
        }

        // An unreadable configuration is unknown, never a silent "no budget".
        assert!(matches!(
            budget_dimension("probe.db", Some(&sample), None),
            StoreBudgetDimensionV1::Unknown { .. }
        ));
        // A configured budget with no sample cannot be evaluated.
        assert!(matches!(
            budget_dimension("probe.db", None, Some(&configured)),
            StoreBudgetDimensionV1::Unknown { .. }
        ));
    }

    #[test]
    fn table_growth_projection_keeps_unavailable_and_baseline_states_typed() {
        let store = StoreKeyV1::new("probe.db").expect("key");
        let baseline = table_growth_dimension(TableGrowthTelemetryReadV1::BaselineEstablished {
            store: store.clone(),
            observed_at: UtcMicros(42),
            tables_observed: 7,
        });
        match baseline {
            TableGrowthDimensionV1::BaselineEstablished {
                tables_observed,
                omission_reasons,
                ..
            } => {
                assert_eq!(tables_observed, 7);
                assert!(
                    omission_reasons
                        .iter()
                        .any(|reason| reason.contains("no baseline yet"))
                );
            }
            other => panic!("expected baseline state, got {other:?}"),
        }

        let unknown = table_growth_dimension(TableGrowthTelemetryReadV1::Unknown { store });
        match unknown {
            TableGrowthDimensionV1::Unknown {
                omission_reasons, ..
            } => {
                let serialized = serde_json::to_string(&omission_reasons).expect("serialize");
                assert!(serialized.contains("unavailable"));
                assert!(!serialized.contains("0 B"));
            }
            other => panic!("expected unknown state, got {other:?}"),
        }
    }

    #[test]
    fn table_growth_projection_reports_significant_samples_and_omissions() {
        let store = StoreKeyV1::new("probe.db").expect("key");
        let significant = tracedecay_application::storage::TableGrowthSampleV1 {
            store: store.clone(),
            table: tracedecay_application::storage::TableNameV1::new("messages").expect("table"),
            previous_bytes: tracedecay_application::storage::StorageByteSizeV1(10 * 1024 * 1024),
            current_bytes: tracedecay_application::storage::StorageByteSizeV1(11 * 1024 * 1024),
            previous_observed_at: UtcMicros(10),
            current_observed_at: UtcMicros(20),
        };
        let insignificant = tracedecay_application::storage::TableGrowthSampleV1 {
            store: store.clone(),
            table: tracedecay_application::storage::TableNameV1::new("metadata").expect("table"),
            previous_bytes: tracedecay_application::storage::StorageByteSizeV1(100 * 1024 * 1024),
            current_bytes: tracedecay_application::storage::StorageByteSizeV1(
                100 * 1024 * 1024 + 512 * 1024,
            ),
            previous_observed_at: UtcMicros(10),
            current_observed_at: UtcMicros(20),
        };

        match table_growth_dimension(TableGrowthTelemetryReadV1::Observed {
            store,
            samples: vec![significant, insignificant],
            baseline_pending: Vec::new(),
        }) {
            TableGrowthDimensionV1::Observed {
                significant_samples,
                omissions,
                omission_reasons,
                coverage,
            } => {
                assert_eq!(significant_samples.len(), 1);
                assert_eq!(significant_samples[0].table, "messages");
                assert_eq!(significant_samples[0].growth_bytes, 1024 * 1024);
                assert_eq!(significant_samples[0].previous_observed_at, 10);
                assert_eq!(significant_samples[0].current_observed_at, 20);
                assert_eq!(omissions.len(), 1);
                assert_eq!(omissions[0].table(), "metadata");
                assert!(omissions[0].reason().contains("below"));
                assert_eq!(omission_reasons.len(), 1);
                assert!(coverage.is_complete());
            }
            other => panic!("expected observed state, got {other:?}"),
        }
    }

    #[test]
    fn table_growth_projection_marks_new_table_as_partial_without_zero_growth() {
        let store = StoreKeyV1::new("probe.db").expect("key");
        let pending = tracedecay_application::storage::TableGrowthBaselinePendingV1 {
            store: store.clone(),
            table: tracedecay_application::storage::TableNameV1::new("new_messages")
                .expect("table"),
            current_bytes: tracedecay_application::storage::StorageByteSizeV1(4096),
            observed_at: UtcMicros(20),
        };

        match table_growth_dimension(TableGrowthTelemetryReadV1::Observed {
            store,
            samples: Vec::new(),
            baseline_pending: vec![pending],
        }) {
            TableGrowthDimensionV1::Observed {
                significant_samples,
                omissions,
                omission_reasons,
                coverage,
            } => {
                assert!(significant_samples.is_empty());
                assert_eq!(coverage.denominator, Some(1));
                assert_eq!(coverage.examined, Some(0));
                assert!(!coverage.is_complete());
                assert_eq!(omissions.len(), 1);
                assert!(matches!(
                    omissions[0],
                    TableGrowthOmissionV1::BaselinePending {
                        current_bytes: 4096,
                        ..
                    }
                ));
                assert!(
                    omission_reasons
                        .iter()
                        .any(|reason| reason.contains("no previous table watermark"))
                );
                let serialized = serde_json::to_string(&omissions).expect("serialize");
                assert!(!serialized.contains("growth_bytes\":0"));
            }
            other => panic!("expected observed state, got {other:?}"),
        }
    }

    #[test]
    fn growth_starts_at_baseline_then_reports_a_signed_delta_over_the_window() {
        let identity = format!("/telemetry-growth-test/{}", now_micros());

        let first = growth_dimension(&identity, Some(4096), Some(0));
        match first {
            StoreGrowthDimensionV1::Baseline {
                coverage,
                total_bytes,
                ..
            } => {
                assert_eq!(total_bytes, 4096);
                assert!(coverage.contains("since-daemon-start"));
            }
            other => panic!("first watermark should be a baseline, got {other:?}"),
        }

        let second = growth_dimension(&identity, Some(8192), Some(0));
        match second {
            StoreGrowthDimensionV1::Observed {
                growth_bytes,
                sample_count,
                first_total_bytes,
                current_total_bytes,
                ..
            } => {
                assert_eq!(growth_bytes, 4096);
                assert_eq!(sample_count, 2);
                assert_eq!(first_total_bytes, 4096);
                assert_eq!(current_total_bytes, 8192);
            }
            other => panic!("second watermark should observe growth, got {other:?}"),
        }

        // A shrinking store reports a negative delta rather than zero growth.
        match growth_dimension(&identity, Some(2048), Some(0)) {
            StoreGrowthDimensionV1::Observed { growth_bytes, .. } => {
                assert_eq!(growth_bytes, -2048);
            }
            other => panic!("expected an observed shrink, got {other:?}"),
        }

        // A failed size read records nothing and is typed unknown.
        assert!(matches!(
            growth_dimension(&identity, None, None),
            StoreGrowthDimensionV1::Unknown { .. }
        ));
    }

    #[test]
    fn watermark_ring_is_bounded_per_store() {
        let identity = format!("/telemetry-ring-test/{}", now_micros());
        for index in 0..(MAX_WATERMARKS_PER_STORE + 10) {
            let _ = growth_dimension(&identity, Some(4096 + index as u64), Some(0));
        }
        match growth_dimension(&identity, Some(4096), Some(0)) {
            StoreGrowthDimensionV1::Observed { sample_count, .. } => {
                assert_eq!(sample_count, MAX_WATERMARKS_PER_STORE);
            }
            other => panic!("expected a bounded observed window, got {other:?}"),
        }
    }

    #[test]
    fn failed_expected_store_stays_visible_unknown_with_partial_coverage() {
        let path = "/profile/accounting.db";
        let mut sampled = Vec::new();
        let mut seen = HashMap::new();

        push_or_merge_unknown_role(
            &mut sampled,
            &mut seen,
            "savings",
            path,
            "store snapshot open failed for savings role".to_string(),
        );
        let coverage = telemetry_coverage(&sampled);
        let entries = sampled
            .into_iter()
            .map(|store| telemetry_entry(store, Some(&RetentionConfig::default())))
            .collect::<Vec<_>>();

        assert_eq!(
            entries.len(),
            1,
            "the failed expected store must not disappear"
        );
        assert_eq!(entries[0].path, path);
        assert_eq!(entries[0].roles, vec!["savings"]);
        assert!(matches!(
            entries[0].read,
            StorageTelemetryReadV1::Unknown { .. }
        ));
        assert_eq!(coverage.denominator, Some(1));
        assert_eq!(coverage.examined, Some(0));
        assert!(!coverage.is_complete());
        assert!(
            coverage
                .omission_reasons
                .iter()
                .any(|reason| reason.contains("store snapshot open failed"))
        );
    }
}
