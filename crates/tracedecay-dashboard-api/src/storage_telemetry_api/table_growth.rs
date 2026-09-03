use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_application::storage::telemetry::TableGrowthTelemetryReadV1;
use tracedecay_application::storage::{
    SIGNIFICANT_TABLE_GROWTH_ABSOLUTE_BYTES, SIGNIFICANT_TABLE_GROWTH_PERCENT,
    SIGNIFICANT_TABLE_GROWTH_RELATIVE_FLOOR_BYTES, is_significant_table_growth,
};

use super::StoreTelemetryEntryV1;
use crate::read_model::DashboardCoverageV1;

const TABLE_GROWTH_BASELINE_REASON: &str =
    "no baseline yet; this read established the first per-table payload watermark";
const TABLE_GROWTH_UNSUPPORTED_REASON: &str =
    "per-table payload growth measurement is unsupported for this store";
const TABLE_GROWTH_DENIED_REASON: &str =
    "per-table payload growth measurement was denied for this store";
const TABLE_GROWTH_UNKNOWN_REASON: &str =
    "per-table payload growth measurement is unavailable for this store";

/// Informational threshold applied to per-table payload growth samples.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
pub struct TableGrowthThresholdV1 {
    pub absolute_bytes: u64,
    pub relative_floor_bytes: u64,
    pub relative_percent: u64,
}

pub(super) const TABLE_GROWTH_THRESHOLD: TableGrowthThresholdV1 = TableGrowthThresholdV1 {
    absolute_bytes: SIGNIFICANT_TABLE_GROWTH_ABSOLUTE_BYTES,
    relative_floor_bytes: SIGNIFICANT_TABLE_GROWTH_RELATIVE_FLOOR_BYTES,
    relative_percent: SIGNIFICANT_TABLE_GROWTH_PERCENT,
};

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

fn unavailable_table_growth_coverage(reason: &str) -> DashboardCoverageV1 {
    DashboardCoverageV1::partial(1, 0, "store_table_growth_reads", vec![reason.to_string()])
}

/// Project the application telemetry read into the dashboard contract without
/// inventing bytes for unavailable states or silently dropping below-threshold
/// tables.
pub(super) fn table_growth_dimension(read: TableGrowthTelemetryReadV1) -> TableGrowthDimensionV1 {
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

pub(super) fn table_growth_payload_coverage(
    entries: &[StoreTelemetryEntryV1],
) -> DashboardCoverageV1 {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tracedecay_application::storage::identity::StoreKeyV1;
    use tracedecay_application::storage::telemetry::TableGrowthTelemetryReadV1;
    use tracedecay_application::storage::{
        StorageByteSizeV1, TableGrowthBaselinePendingV1, TableGrowthSampleV1, TableNameV1,
    };
    use tracedecay_domain::UtcMicros;

    use super::{TableGrowthDimensionV1, TableGrowthOmissionV1, table_growth_dimension};

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
        let significant = TableGrowthSampleV1 {
            store: store.clone(),
            table: TableNameV1::new("messages").expect("table"),
            previous_bytes: StorageByteSizeV1(10 * 1024 * 1024),
            current_bytes: StorageByteSizeV1(11 * 1024 * 1024),
            previous_observed_at: UtcMicros(10),
            current_observed_at: UtcMicros(20),
        };
        let insignificant = TableGrowthSampleV1 {
            store: store.clone(),
            table: TableNameV1::new("metadata").expect("table"),
            previous_bytes: StorageByteSizeV1(100 * 1024 * 1024),
            current_bytes: StorageByteSizeV1(100 * 1024 * 1024 + 512 * 1024),
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
        let pending = TableGrowthBaselinePendingV1 {
            store: store.clone(),
            table: TableNameV1::new("new_messages").expect("table"),
            current_bytes: StorageByteSizeV1(4096),
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
}
