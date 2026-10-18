use std::collections::BTreeSet;

use super::super::{
    ExecutionConcurrencyPhaseV1, ExecutionDuplicateKindV1, ExecutionQuantityUnitV1,
};
use super::*;

#[test]
fn unsupported_ready_to_integrated_metric_is_absent_from_catalog_and_projection() {
    let unsupported = "work_ready_to_integrated_seconds";
    assert!(
        EXECUTION_TOPOLOGY_METRIC_DESCRIPTORS_V1
            .iter()
            .all(|(metric, _, _)| *metric != unsupported)
    );

    let model = unavailable_model(
        "project.descriptor-regression".to_owned(),
        ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 1,
        },
        1,
        ExecutionMetricUnavailableV1::StoreUnavailable,
    );
    assert!(
        model
            .measurements
            .iter()
            .all(|measurement| measurement.value.metric != unsupported)
    );
}

#[test]
fn known_empty_projection_keeps_every_descriptor_without_discarding_dimensions() {
    let horizon = ObservabilityHorizonV1 {
        since_micros: 0,
        until_micros: 86_400_000_000,
    };
    let rollup = crate::execution_topology_metrics::build_empty_execution_topology_daily_rollup(
        "project.empty-topology-descriptors",
        &horizon,
        horizon.until_micros,
    )
    .unwrap();
    let model = crate::execution_topology_metrics::project_execution_topology_fragments(
        "project.empty-topology-descriptors",
        &horizon,
        horizon.until_micros,
        &[rollup.fragment],
    );
    let expected_metric_names = EXECUTION_TOPOLOGY_METRIC_DESCRIPTORS_V1
        .iter()
        .map(|(metric, _, _)| *metric)
        .collect::<BTreeSet<_>>();
    let actual_metric_names = model
        .measurements
        .iter()
        .map(|measurement| measurement.value.metric.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_metric_names, expected_metric_names);
    let full_cell_identities = model
        .measurements
        .iter()
        .map(|measurement| {
            (
                measurement.value.descriptor_revision.as_str(),
                measurement.value.metric.as_str(),
                measurement.value.unit.as_str(),
                measurement.value.denominator.as_str(),
                serde_json::to_string(&measurement.dimensions)
                    .expect("serializable topology dimensions"),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(full_cell_identities.len(), model.measurements.len());
    assert!(
        full_cell_identities.len() > expected_metric_names.len(),
        "the normal projection must retain its dimensional descriptor identities"
    );
    let dimensional_measurement_count = model
        .measurements
        .iter()
        .filter(|measurement| !measurement.dimensions.is_empty())
        .count();
    assert!(
        dimensional_measurement_count > 0,
        "the normal projection must retain at least one dimensional cell"
    );
    assert!(
        model.measurements.iter().any(|measurement| {
            measurement.value.metric == "work_duplicate_effort_total"
                && measurement.value.unit == "microseconds"
                && measurement.value.denominator == "adjudicated_duplicate_relations"
                && measurement.dimensions
                    == vec![
                        ExecutionTopologyDimensionV1::DuplicateKind(
                            ExecutionDuplicateKindV1::ExactDuplicate,
                        ),
                        ExecutionTopologyDimensionV1::Unit(ExecutionQuantityUnitV1::WallMicros),
                    ]
        }),
        "the normal projection must retain the duplicate kind and unit cell"
    );
    assert!(
        model.measurements.iter().any(|measurement| {
            measurement.value.metric == "work_execution_concurrency_width"
                && measurement.value.unit == "microseconds"
                && measurement.value.denominator == "duration_weighted_topology_samples"
                && measurement.dimensions
                    == vec![ExecutionTopologyDimensionV1::ConcurrencyPhase(
                        ExecutionConcurrencyPhaseV1::Requested,
                    )]
        }),
        "the normal projection must retain the refused concurrency phase cell"
    );
    assert!(
        model.measurements.iter().all(|measurement| {
            measurement.value.metric != "work_duplicate_effort_total"
                || measurement.value.unit != "events"
                || !measurement.dimensions.is_empty()
        }),
        "the normal projection must not synthesize a dimensionless duplicate-effort events cell"
    );
    assert!(model.measurements.iter().all(|measurement| {
        measurement.value.value.is_none()
            && measurement.unavailable == Some(ExecutionMetricUnavailableV1::NoEligibleEvidence)
    }));
    let known_empty_coverage = MetricCoverageV1 {
        eligible: Some(0),
        observed: 0,
        completed: 0,
        censored: 0,
        unknown: 0,
        excluded: 0,
        state: CoverageStateV1::Known,
    };
    for (metric, unit, denominator) in [
        (
            "work_merge_success_ratio",
            "ratio",
            "observed_native_integrations",
        ),
        (
            "work_blocked_cause_seconds",
            "seconds",
            "closed_blocked_intervals",
        ),
        ("work_rerun_rate", "ratio", "eligible_original_attempts"),
        (
            "work_delivery_duplicate_ratio",
            "ratio",
            "attempted_deliveries",
        ),
    ] {
        let matching = model
            .measurements
            .iter()
            .filter(|measurement| {
                measurement.value.metric == metric && measurement.dimensions.is_empty()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "known empty projection must retain one dimensionless typed absence for {metric}"
        );
        let measurement = matching[0];
        assert!(measurement.dimensions.is_empty());
        assert_eq!(
            measurement.value.descriptor_revision,
            EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1
        );
        assert_eq!(measurement.value.unit, unit);
        assert_eq!(measurement.value.denominator, denominator);
        assert_eq!(measurement.value.denominator_value, Some(0));
        assert_eq!(measurement.value.coverage, known_empty_coverage);
        assert!(measurement.value.value.is_none());
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::NoEligibleEvidence)
        );
        assert_eq!(
            measurement.value.unavailable_reason.as_deref(),
            Some(ExecutionMetricUnavailableV1::NoEligibleEvidence.as_str())
        );
    }
}
