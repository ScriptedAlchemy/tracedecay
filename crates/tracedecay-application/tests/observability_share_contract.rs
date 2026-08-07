use tracedecay_application::{
    AggregateCapabilityV1, AggregateShareCellV1, AggregateShareDimensionV1,
    AggregateShareExportRequestV1, AggregateShareMetricV1, AggregateSharePacketV1,
    AggregateShareUnitV1, ObservabilityHorizonV1,
};
use tracedecay_domain::{AnalyticsModeV1, CoverageStateV1};

fn cell() -> AggregateShareCellV1 {
    AggregateShareCellV1 {
        metric: AggregateShareMetricV1::RetrievalQueries,
        unit: AggregateShareUnitV1::Events,
        dimensions: vec![AggregateShareDimensionV1::Capability(
            AggregateCapabilityV1::Retrieval,
        )],
        eligible: 100,
        observed: 100,
        completed: 96,
        censored: 2,
        unknown: 2,
        value: Some(100.0),
        coverage: CoverageStateV1::Partial,
        contribution_windows: 100,
    }
}

#[test]
fn aggregate_share_packet_is_identity_free_and_bounded() {
    let packet = AggregateSharePacketV1 {
        schema_revision: 1,
        descriptor_revision: "aggregate-share.v1".into(),
        horizon: ObservabilityHorizonV1 {
            since_micros: 10,
            until_micros: 20,
        },
        generated_at_micros: 20,
        cells: vec![cell()],
        suppressed_cell_count: 0,
        capped_cell_count: 0,
    };

    packet.validate().expect("valid aggregate packet");
    let json = serde_json::to_value(packet).expect("serialize packet");
    let object = json.as_object().expect("packet object");
    for prohibited in [
        "scope_ref",
        "trace_id",
        "event_id",
        "project_id",
        "repository",
        "session_id",
        "task_id",
    ] {
        assert!(!object.contains_key(prohibited));
        assert!(!json.to_string().contains(prohibited));
    }
}

#[test]
fn aggregate_share_rejects_small_cohorts_and_dimension_overflow() {
    let mut insufficient = cell();
    insufficient.contribution_windows = 99;
    assert_eq!(
        insufficient.validate(),
        Err("aggregate_share_contribution_floor")
    );

    let mut dimensions = cell();
    dimensions.dimensions = vec![
        AggregateShareDimensionV1::Capability(AggregateCapabilityV1::Retrieval),
        AggregateShareDimensionV1::Outcome(tracedecay_application::AggregateOutcomeV1::Completed),
        AggregateShareDimensionV1::Os(tracedecay_application::AggregateOsFamilyV1::Linux),
        AggregateShareDimensionV1::ProductVersion { major: 2, minor: 0 },
        AggregateShareDimensionV1::Coverage(CoverageStateV1::Known),
    ];
    assert_eq!(dimensions.validate(), Err("aggregate_share_dimensions"));
}

#[test]
fn local_only_and_off_modes_refuse_export_before_egress() {
    for mode in [AnalyticsModeV1::Off, AnalyticsModeV1::LocalOnly] {
        let request = AggregateShareExportRequestV1 {
            mode,
            authorized_scope_ref: "scope:local".into(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 1,
                until_micros: 2,
            },
            max_cells: 1,
        };
        assert_eq!(
            request.validate().expect_err("egress disabled").to_string(),
            "domain contract rejected application input: aggregate_share_not_enabled"
        );
    }
}
