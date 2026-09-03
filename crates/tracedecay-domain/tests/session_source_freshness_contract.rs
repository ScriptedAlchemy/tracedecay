use tracedecay_domain::*;

fn source(value: &str) -> SessionSourceIdV1 {
    SessionSourceIdV1::new(value).unwrap()
}

#[test]
fn validity_intervals_reject_empty_and_reversed_bounds() {
    assert_eq!(
        ClosedUtcIntervalV1::new(None, None),
        Err(SessionContractError::EmptyCoverageInterval)
    );
    assert_eq!(
        ClosedUtcIntervalV1::new(Some(UtcMicros(20)), Some(UtcMicros(10))),
        Err(SessionContractError::ReversedCoverageInterval)
    );

    let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
    let interval = |from, through| SessionSourceCoverageIntervalV1 {
        knowledge: ClosedUtcIntervalV1::new(Some(UtcMicros(from)), Some(UtcMicros(through)))
            .unwrap(),
        valid: ValidCoverageIntervalV1::Unknown,
    };
    assert_eq!(
        SessionSourceCoverageV1::new(
            source("cursor"),
            SessionSourceFrontierV1::new(10),
            SessionSourceFrontierV1::new(10),
            SessionSourceFrontierV1::new(10),
            request,
            vec![interval(1, 5), interval(6, 10)],
            Vec::new(),
            SessionSourceCoverageStateV1::Fresh,
            SessionSourceCoverageReasonV1::CaughtUp,
        ),
        Err(SessionContractError::NonCanonicalCoverageIntervals)
    );
}

#[test]
fn source_freshness_is_derived_from_observed_projected_and_target_frontiers() {
    let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
    let stale = SessionSourceCoverageV1::from_frontiers(
        source("cursor"),
        SessionSourceFrontierV1::new(10),
        SessionSourceFrontierV1::new(8),
        SessionSourceFrontierV1::new(10),
        request.clone(),
    )
    .unwrap();
    assert_eq!(stale.state(), SessionSourceCoverageStateV1::Stale);
    assert_eq!(
        stale.reason(),
        &SessionSourceCoverageReasonV1::ProjectionBehindSource { lag: 2 }
    );

    let partial = SessionSourceCoverageV1::from_frontiers(
        source("claude"),
        SessionSourceFrontierV1::new(8),
        SessionSourceFrontierV1::new(8),
        SessionSourceFrontierV1::new(10),
        request,
    )
    .unwrap();
    assert_eq!(partial.state(), SessionSourceCoverageStateV1::Partial);
    assert_eq!(
        partial.reason(),
        &SessionSourceCoverageReasonV1::SourceBehindTarget { lag: 2 }
    );
}

#[test]
fn aggregate_receipt_preserves_sources_and_mixed_freshness() {
    let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
    let receipt = SessionSourceCoverageReceiptV1::new(
        request.clone(),
        vec![
            SessionSourceCoverageV1::from_frontiers(
                source("cursor"),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(10),
                request.clone(),
            )
            .unwrap(),
            SessionSourceCoverageV1::from_frontiers(
                source("claude"),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(7),
                SessionSourceFrontierV1::new(10),
                request,
            )
            .unwrap(),
        ],
    )
    .unwrap();

    assert_eq!(receipt.sources().len(), 2);
    assert_eq!(
        receipt.aggregate_state(),
        SessionSourceCoverageAggregateStateV1::Partial
    );
    assert_eq!(receipt.max_frontier_lag(), 3);
}

#[test]
fn refresh_key_canonicalizes_sources_and_round_trips() {
    let target = |name: &str| {
        SessionRefreshSourceTargetV1::new(
            source(name),
            SessionSourceFrontierV1::new(8),
            SessionSourceFrontierV1::new(10),
        )
        .unwrap()
    };
    let key = SessionRefreshKeyV1::new(
        "root.1",
        SessionId::new("session.1").unwrap(),
        vec![target("cursor"), target("claude")],
        "projector.v1",
        "sha256:configuration",
    )
    .unwrap();
    assert_eq!(key.sources()[0].source_id().as_str(), "claude");
    let encoded = serde_json::to_string(&key).unwrap();
    assert_eq!(
        serde_json::from_str::<SessionRefreshKeyV1>(&encoded).unwrap(),
        key
    );
}
