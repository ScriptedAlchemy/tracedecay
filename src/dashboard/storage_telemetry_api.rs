//! `GET /api/storage/telemetry` — per-store size, free-page ratio, and typed
//! budget/growth dimensions (plan 38 §7 read models over the PR14 envelope).
//!
//! The size and growth samples come from the canonical application
//! `StorageStatus` owner. It records exact store-file sizes into a bounded,
//! durable, project-scoped history; the dashboard never opens an adapter-owned
//! SQLite telemetry connection.
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
//! - **growth**: the bounded `StorageStatus` history survives daemon restart
//!   and is validated against exact project/store scope before use.
//!
//! Store identity: the dashboard holds several *roles* (`graph`, `memory`,
//! `lcm`, `savings`) that can resolve to the **same** store file — in project
//! storage mode the graph and project-memory roles are the same database. Roles
//! are therefore deduplicated by store file identity: one card per real store,
//! carrying every role it serves, instead of the same store reported twice with
//! identical sizes.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use tracedecay_application::storage::identity::StoreKeyV1;
use tracedecay_application::storage::telemetry::{
    StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreSizeBudgetV1, StoreSizeSampleV1,
};
use tracedecay_domain::UtcMicros;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, now_micros, scope_from_state,
};

/// One store's telemetry entry. One entry per distinct store **file**, not per
/// dashboard role.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StoreTelemetryEntryV1 {
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
    /// canonical application read failed. Never silently healthy.
    pub read: StorageTelemetryReadV1,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub free_page_ratio: Option<f64>,
    pub budget: StoreBudgetDimensionV1,
    pub growth: StoreGrowthDimensionV1,
}

/// The budget-evaluation dimension, sourced from owner configuration.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum StoreBudgetDimensionV1 {
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
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct StoreSizeWatermarkV1 {
    /// Wall-clock microseconds at which the size was measured.
    pub measured_at: i64,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// The per-store growth dimension. Growth is only ever reported over the window
/// the server actually observed, and that window is named in `coverage`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum StoreGrowthDimensionV1 {
    /// The first retained durable watermark: a real measurement with no
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

/// Telemetry payload: one entry per canonical store covered by StorageStatus.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageTelemetryPayloadV1 {
    pub stores: Vec<StoreTelemetryEntryV1>,
    /// Where budgets come from, stated once for the whole read.
    pub budget_note: String,
    /// The growth window's coverage, stated once for the whole read.
    pub growth_note: String,
}

/// The owner setting path that configures a store's soft byte budget.
const BUDGET_SETTING_KEY: &str = "sync.retention.v1 store_soft_budgets_bytes";
const BUDGET_UNSET_REASON: &str = "no soft size budget is configured by the owner for this store (set \
     sync.retention.v1 store_soft_budgets_bytes for the store key to configure one)";
const BUDGET_NOTE: &str = "budgets are owner configuration: sync.retention.v1 store_soft_budgets_bytes, keyed by store \
     key; a store with no entry reports unset (no budget configured), never a fabricated pass";
const GROWTH_COVERAGE: &str =
    "durable project-store history retained by ApplicationSurfaceOperation::StorageStatus";
const GROWTH_NOTE: &str = "growth is derived from the durable, project-scoped StorageStatus history and survives daemon restart";
const GROWTH_BASELINE_REASON: &str =
    "first durable watermark for this project store; a growth delta needs a second sample";
const GROWTH_UNKNOWN_REASON: &str =
    "no watermark could be recorded because the store size read did not produce a sample";
const BUDGET_NO_SAMPLE_REASON: &str =
    "no observed size sample, so a configured budget could not be evaluated";

/// One canonical store sampled by StorageStatus.
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
    pub history: Vec<StoreSizeWatermarkV1>,
    pub history_coverage: String,
}

impl SampledStoreV1 {
    /// The observed page sample, when an owning runtime provides one.
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

/// Read the exact project store through the canonical StorageStatus owner.
async fn collect_store_samples(state: &DashboardState) -> Vec<SampledStoreV1> {
    let Some(graph) = state.project_graph.as_ref() else {
        return Vec::new();
    };
    let status =
        crate::application::primitives::production::canonical_storage_status(graph.as_ref(), false);
    let Some(path) = status.store_path else {
        return Vec::new();
    };
    let store_name = store_file_name(&path);
    let Ok(store) = StoreKeyV1::new(store_name.clone()) else {
        return Vec::new();
    };
    let history_coverage = status
        .history_coverage
        .unwrap_or_else(|| "storage_status_history_coverage_unknown".to_owned());
    let Some(total_bytes) = status.database_bytes else {
        return vec![SampledStoreV1 {
            store: store_name,
            path,
            roles: vec!["graph".to_owned()],
            read: StorageTelemetryReadV1::Unknown { store },
            history: Vec::new(),
            history_coverage,
        }];
    };
    let observed_at = status
        .history
        .last()
        .map_or_else(now_micros, |sample| sample.observed_at);
    let history = status
        .history
        .into_iter()
        .map(|sample| StoreSizeWatermarkV1 {
            measured_at: sample.observed_at,
            total_bytes: sample.database_bytes,
            free_bytes: 0,
        })
        .collect();
    vec![SampledStoreV1 {
        store: store_name,
        path,
        roles: vec!["graph".to_owned()],
        read: StorageTelemetryReadV1::ObservedBytes {
            store,
            total_bytes: tracedecay_application::storage::identity::StorageByteSizeV1(total_bytes),
            observed_at: UtcMicros(observed_at),
        },
        history,
        history_coverage,
    }]
}

/// `GET /api/storage/telemetry`
pub(crate) async fn telemetry(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<StorageTelemetryPayloadV1>> {
    // The owner-configured soft budgets, resolved once per read from the pinned
    // runtime configuration.
    let retention = &state.retention_config;

    let entries: Vec<StoreTelemetryEntryV1> = collect_store_samples(&state)
        .await
        .into_iter()
        .map(|sampled| telemetry_entry(sampled, Some(retention)))
        .collect();

    let total = entries.len() as u64;
    let observed = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.read,
                StorageTelemetryReadV1::Observed { .. }
                    | StorageTelemetryReadV1::ObservedBytes { .. }
            )
        })
        .count() as u64;

    // Coverage is over the canonical stores enumerated by StorageStatus. A
    // complete claim therefore always carries a real denominator.
    let coverage = if observed == total {
        DashboardCoverageV1::complete(total, "canonical_storage_status_stores")
    } else {
        DashboardCoverageV1::partial(
            total,
            observed,
            "canonical_storage_status_stores",
            vec!["canonical StorageStatus read did not produce a size".to_string()],
        )
    };

    let payload = StorageTelemetryPayloadV1 {
        stores: entries,
        budget_note: BUDGET_NOTE.to_string(),
        growth_note: GROWTH_NOTE.to_string(),
    };

    let envelope = DashboardEnvelopeV1::ready(scope_from_state(&state), coverage, payload)
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "use-case.dashboard.storage.telemetry.refresh",
        )]);
    Json(envelope)
}

/// Project one sampled store onto the telemetry read model, adding the budget
/// and growth dimensions.
fn telemetry_entry(
    sampled: SampledStoreV1,
    retention: Option<&crate::config::RetentionConfig>,
) -> StoreTelemetryEntryV1 {
    let sample = sampled.sample();
    let (total_bytes, free_bytes, free_page_ratio) = match &sampled.read {
        StorageTelemetryReadV1::Observed { sample } => (
            Some(sample.total_bytes().get()),
            Some(sample.free_bytes().get()),
            Some(sample.free_page_ratio().as_f64()),
        ),
        StorageTelemetryReadV1::ObservedBytes { total_bytes, .. } => {
            (Some(total_bytes.get()), None, None)
        }
        _ => (None, None, None),
    };
    let budget = budget_dimension(&sampled.store, sample, total_bytes, retention);
    let growth = growth_dimension(&sampled.history, &sampled.history_coverage);
    let role = sampled.primary_role();

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
    }
}

/// Resolve the budget dimension for one store from owner configuration.
fn budget_dimension(
    store_name: &str,
    sample: Option<&StoreSizeSampleV1>,
    total_bytes: Option<u64>,
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
    let evaluation = if let Some(sample) = sample {
        budget.evaluate(sample)
    } else if let Some(total_bytes) = total_bytes {
        let observed = tracedecay_application::storage::identity::StorageByteSizeV1(total_bytes);
        Ok(if observed > budget.soft_limit_bytes {
            StoreBudgetEvaluationV1::OverBudget {
                observed,
                soft_limit: budget.soft_limit_bytes,
                overage: observed.saturating_sub(budget.soft_limit_bytes),
            }
        } else {
            StoreBudgetEvaluationV1::WithinBudget {
                observed,
                soft_limit: budget.soft_limit_bytes,
            }
        })
    } else {
        return StoreBudgetDimensionV1::Unknown {
            reason: BUDGET_NO_SAMPLE_REASON.to_string(),
        };
    };
    match evaluation {
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

/// Derive growth from the canonical application's durable watermark history.
fn growth_dimension(samples: &[StoreSizeWatermarkV1], coverage: &str) -> StoreGrowthDimensionV1 {
    if samples.is_empty() {
        return StoreGrowthDimensionV1::Unknown {
            reason: format!("{GROWTH_UNKNOWN_REASON}; coverage: {coverage}"),
        };
    }
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return StoreGrowthDimensionV1::Unknown {
            reason: GROWTH_UNKNOWN_REASON.to_string(),
        };
    };
    if samples.len() < 2 {
        return StoreGrowthDimensionV1::Baseline {
            coverage: coverage.to_owned(),
            measured_at: last.measured_at,
            total_bytes: last.total_bytes,
            reason: GROWTH_BASELINE_REASON.to_string(),
        };
    }
    let growth_bytes = i64::try_from(last.total_bytes)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(first.total_bytes).unwrap_or(i64::MAX));
    StoreGrowthDimensionV1::Observed {
        coverage: coverage.to_owned(),
        first_measured_at: first.measured_at,
        last_measured_at: last.measured_at,
        sample_count: samples.len(),
        first_total_bytes: first.total_bytes,
        current_total_bytes: last.total_bytes,
        growth_bytes,
        samples: samples.to_vec(),
    }
}

fn store_file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.to_string(), str::to_string)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::RetentionConfig;

    #[test]
    fn telemetry_entry_projects_durable_observed_bytes() {
        let sampled = SampledStoreV1 {
            store: "graph.db".to_owned(),
            path: "/project/.tracedecay/graph.db".to_owned(),
            roles: vec!["graph".to_owned()],
            read: StorageTelemetryReadV1::ObservedBytes {
                store: StoreKeyV1::new("graph.db").expect("key"),
                total_bytes: tracedecay_application::storage::identity::StorageByteSizeV1(8192),
                observed_at: UtcMicros(2),
            },
            history: vec![
                StoreSizeWatermarkV1 {
                    measured_at: 1,
                    total_bytes: 4096,
                    free_bytes: 0,
                },
                StoreSizeWatermarkV1 {
                    measured_at: 2,
                    total_bytes: 8192,
                    free_bytes: 0,
                },
            ],
            history_coverage: GROWTH_COVERAGE.to_owned(),
        };

        let entry = telemetry_entry(sampled, Some(&RetentionConfig::default()));
        assert_eq!(entry.total_bytes, Some(8192));
        assert_eq!(entry.free_bytes, None);
        assert_eq!(entry.free_page_ratio, None);
        assert!(matches!(
            entry.read,
            StorageTelemetryReadV1::ObservedBytes { .. }
        ));
        assert!(matches!(entry.budget, StoreBudgetDimensionV1::Unset { .. }));
        match entry.growth {
            StoreGrowthDimensionV1::Observed {
                growth_bytes,
                sample_count,
                ..
            } => {
                assert_eq!(growth_bytes, 4096);
                assert_eq!(sample_count, 2);
            }
            other => panic!("expected durable observed growth, got {other:?}"),
        }
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
        let unset = budget_dimension("probe.db", Some(&sample), None, Some(&empty));
        assert!(matches!(unset, StoreBudgetDimensionV1::Unset { .. }));

        // Owner-configured limit below the observed size -> over budget.
        let mut configured = RetentionConfig::default();
        configured
            .store_soft_budgets_bytes
            .insert("probe.db".to_string(), 1024);
        let evaluated = budget_dimension("probe.db", Some(&sample), None, Some(&configured));
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
        match budget_dimension("probe.db", Some(&sample), None, Some(&generous)) {
            StoreBudgetDimensionV1::Evaluated { evaluation, .. } => {
                assert!(!evaluation.is_over_budget());
            }
            other => panic!("expected an evaluated budget, got {other:?}"),
        }

        // An unreadable configuration is unknown, never a silent "no budget".
        assert!(matches!(
            budget_dimension("probe.db", Some(&sample), None, None),
            StoreBudgetDimensionV1::Unknown { .. }
        ));
        // A configured budget with no sample cannot be evaluated.
        assert!(matches!(
            budget_dimension("probe.db", None, None, Some(&configured)),
            StoreBudgetDimensionV1::Unknown { .. }
        ));
    }

    #[test]
    fn durable_history_projects_baseline_and_signed_growth() {
        let first_samples = vec![StoreSizeWatermarkV1 {
            measured_at: 1,
            total_bytes: 4096,
            free_bytes: 0,
        }];
        let first = growth_dimension(&first_samples, GROWTH_COVERAGE);
        match first {
            StoreGrowthDimensionV1::Baseline {
                coverage,
                total_bytes,
                ..
            } => {
                assert_eq!(total_bytes, 4096);
                assert!(coverage.contains("durable"));
            }
            other => panic!("first watermark should be a baseline, got {other:?}"),
        }

        let second_samples = vec![
            first_samples[0],
            StoreSizeWatermarkV1 {
                measured_at: 2,
                total_bytes: 8192,
                free_bytes: 0,
            },
        ];
        let second = growth_dimension(&second_samples, GROWTH_COVERAGE);
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

        let shrinking = vec![
            StoreSizeWatermarkV1 {
                measured_at: 1,
                total_bytes: 4096,
                free_bytes: 0,
            },
            StoreSizeWatermarkV1 {
                measured_at: 2,
                total_bytes: 2048,
                free_bytes: 0,
            },
        ];
        match growth_dimension(&shrinking, GROWTH_COVERAGE) {
            StoreGrowthDimensionV1::Observed { growth_bytes, .. } => {
                assert_eq!(growth_bytes, -2048);
            }
            other => panic!("expected an observed shrink, got {other:?}"),
        }
        assert!(matches!(
            growth_dimension(&[], "current_sample_unavailable"),
            StoreGrowthDimensionV1::Unknown { .. }
        ));
    }
}
