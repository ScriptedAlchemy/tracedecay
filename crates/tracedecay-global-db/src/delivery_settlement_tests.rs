use tracedecay_domain::{
    AttemptId, DeliveryChannelIdentityV1, DeliveryDropReasonV1, DeliveryEventClassV1,
    DeliverySettlementAttemptV1, DeliverySettlementOutcomeV1, DeliverySettlementV1,
    DeliverySurfaceFamilyV1, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
};

use crate::{
    DeliveryAttemptClaimV1, DeliverySourceReceiptReadV1, MAX_PENDING_RECEIPTED_DELIVERIES_V1,
    WorkAttemptDeliveryCensusReadV1, tests::harness::RegisteredGlobalDbHarness,
};

fn work_attempt() -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        TaskId::try_from("task.delivery.bound".to_owned()).expect("task id"),
        RunId::try_from("run.delivery.bound".to_owned()).expect("run id"),
        AttemptId::try_from("attempt.delivery.bound".to_owned()).expect("attempt id"),
    )
    .expect("attempt identity")
}

fn attempt(surface: DeliverySurfaceFamilyV1, channel_ref: &str) -> DeliverySettlementAttemptV1 {
    DeliverySettlementAttemptV1 {
        owner_event_id: "work:attempt:42:terminal".to_owned(),
        event_class: DeliveryEventClassV1::OperationTerminal,
        channel: DeliveryChannelIdentityV1 {
            surface,
            channel_ref: channel_ref.to_owned(),
        },
        work_attempt: None,
        eligible: 2,
        valid_at: UtcMicros(100),
        attempted_at: UtcMicros(110),
    }
}

fn settlement(
    attempt: DeliverySettlementAttemptV1,
    outcome: DeliverySettlementOutcomeV1,
) -> DeliverySettlementV1 {
    DeliverySettlementV1 {
        attempt,
        outcome,
        settled_at: UtcMicros(120),
        drop_reason: (outcome == DeliverySettlementOutcomeV1::Dropped)
            .then_some(DeliveryDropReasonV1::Disconnected),
    }
}

#[tokio::test]
async fn durable_delivery_settlement_replays_without_double_counting() {
    let harness = RegisteredGlobalDbHarness::open("delivery-settlement-replay").await;
    let first = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:connection-7:request-42");
    let second = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:connection-8:request-42");

    assert_eq!(
        harness
            .registered
            .begin_delivery_attempt("scope:fixture", &first)
            .await
            .expect("first attempt"),
        DeliveryAttemptClaimV1::Claimed
    );
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &second)
        .await
        .expect("second attempt");

    let delivered = settlement(first.clone(), DeliverySettlementOutcomeV1::Delivered);
    let receipt = harness
        .registered
        .settle_delivery_attempt("scope:fixture", &delivered)
        .await
        .expect("delivery settlement");
    assert!(!receipt.replayed);
    assert_eq!(receipt.census.attempted, 2);
    assert_eq!(receipt.census.delivered, 1);
    assert_eq!(receipt.census.unknown, 1);

    let replay = harness
        .registered
        .settle_delivery_attempt("scope:fixture", &delivered)
        .await
        .expect("exact replay");
    assert!(replay.replayed);
    assert_eq!(replay.census.delivered, 1);

    let dropped = settlement(second, DeliverySettlementOutcomeV1::Dropped);
    let complete = harness
        .registered
        .settle_delivery_attempt("scope:fixture", &dropped)
        .await
        .expect("dropped recipient settlement");
    assert_eq!(complete.census.attempted, 2);
    assert_eq!(complete.census.delivered, 1);
    assert_eq!(complete.census.dropped, 1);
    assert_eq!(complete.census.unknown, 0);
    assert_eq!(
        complete.census.coverage,
        tracedecay_domain::CoverageStateV1::Known
    );

    let changed = settlement(first, DeliverySettlementOutcomeV1::Dropped);
    let error = harness
        .registered
        .settle_delivery_attempt("scope:fixture", &changed)
        .await
        .expect_err("terminal outcome drift must conflict");
    assert!(error.contains("settlement conflict"), "{error}");
}

#[tokio::test]
async fn delivery_census_isolated_by_surface_and_refuses_denominator_drift() {
    let harness = RegisteredGlobalDbHarness::open("delivery-settlement-surface").await;
    let mcp = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:request-42");
    let mut lsp = attempt(DeliverySurfaceFamilyV1::Lsp, "lsp:session-4:change-9");
    lsp.eligible = 1;
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &mcp)
        .await
        .expect("MCP attempt");
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &lsp)
        .await
        .expect("LSP attempt");

    let lsp_receipt = harness
        .registered
        .settle_delivery_attempt(
            "scope:fixture",
            &settlement(lsp, DeliverySettlementOutcomeV1::Deduplicated),
        )
        .await
        .expect("LSP settlement");
    assert_eq!(lsp_receipt.census.eligible, 1);
    assert_eq!(lsp_receipt.census.deduplicated, 1);
    assert_eq!(lsp_receipt.census.delivered, 0);

    let mut drift = mcp;
    drift.eligible = 3;
    let error = harness
        .registered
        .begin_delivery_attempt("scope:fixture", &drift)
        .await
        .expect_err("denominator drift must conflict");
    assert!(error.contains("fanout identity conflict"), "{error}");
}

#[tokio::test]
async fn typed_work_binding_is_immutable_and_returns_a_bounded_current_census() {
    let harness = RegisteredGlobalDbHarness::open("delivery-work-binding").await;
    let work_attempt = work_attempt();
    let mut first = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:bound-first");
    first.work_attempt = Some(work_attempt.clone());
    let mut second = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:bound-second");
    second.work_attempt = Some(work_attempt.clone());
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &first)
        .await
        .expect("bound first attempt");
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &second)
        .await
        .expect("bound second attempt");
    harness
        .registered
        .settle_delivery_attempt(
            "scope:fixture",
            &settlement(first.clone(), DeliverySettlementOutcomeV1::Delivered),
        )
        .await
        .expect("bound delivery settlement");

    let census = harness
        .registered
        .work_attempt_delivery_censuses("scope:fixture", &work_attempt)
        .await
        .expect("bounded Work delivery census");
    let WorkAttemptDeliveryCensusReadV1::Bounded(censuses) = census else {
        panic!("expected typed bounded Work delivery census");
    };
    assert_eq!(censuses.len(), 1);
    assert_eq!(censuses[0].work_attempt.as_ref(), Some(&work_attempt));
    assert_eq!(censuses[0].unknown, 1);

    let mut rebound = first;
    rebound.work_attempt = Some(
        WorkAttemptIdentityV1::new(
            TaskId::try_from("task.delivery.other".to_owned()).expect("task id"),
            RunId::try_from("run.delivery.other".to_owned()).expect("run id"),
            AttemptId::try_from("attempt.delivery.other".to_owned()).expect("attempt id"),
        )
        .expect("other attempt identity"),
    );
    let error = harness
        .registered
        .begin_delivery_attempt("scope:fixture", &rebound)
        .await
        .expect_err("typed Work binding must be immutable");
    assert!(error.contains("fanout identity conflict"), "{error}");
}

#[tokio::test]
async fn absent_typed_work_binding_stays_unbound_not_empty_delivery() {
    let harness = RegisteredGlobalDbHarness::open("delivery-work-unbound").await;
    let unbound = attempt(DeliverySurfaceFamilyV1::Mcp, "mcp:unbound");
    harness
        .registered
        .begin_delivery_attempt("scope:fixture", &unbound)
        .await
        .expect("unbound delivery attempt");
    assert_eq!(
        harness
            .registered
            .work_attempt_delivery_censuses("scope:fixture", &work_attempt())
            .await
            .expect("unbound read"),
        WorkAttemptDeliveryCensusReadV1::Unbound
    );
}

#[tokio::test]
async fn opaque_source_receipt_reopens_exact_attempt_and_rejects_rebinding() {
    let harness = RegisteredGlobalDbHarness::open("delivery-source-receipt").await;
    let first = attempt(
        DeliverySurfaceFamilyV1::Dashboard,
        "dashboard:connection-7:event-42",
    );
    let receipt_ref = "dsa1:0123456789abcdef";

    assert_eq!(
        harness
            .registered
            .begin_receipted_delivery_attempt("scope:fixture", &first, receipt_ref)
            .await
            .expect("receipted attempt"),
        DeliveryAttemptClaimV1::Claimed
    );
    assert_eq!(
        harness
            .registered
            .delivery_attempt_for_source_receipt("scope:fixture", receipt_ref)
            .await
            .expect("receipt lookup"),
        Some(DeliverySourceReceiptReadV1::Pending(first.clone()))
    );
    assert!(
        harness
            .registered
            .pending_receipted_delivery_attempts_due(
                "scope:fixture",
                DeliverySurfaceFamilyV1::Dashboard,
                UtcMicros(109),
                1,
            )
            .await
            .expect("not-yet-due page")
            .is_empty()
    );
    let due = harness
        .registered
        .pending_receipted_delivery_attempts_due(
            "scope:fixture",
            DeliverySurfaceFamilyV1::Dashboard,
            UtcMicros(110),
            MAX_PENDING_RECEIPTED_DELIVERIES_V1,
        )
        .await
        .expect("due page");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].receipt_ref, receipt_ref);
    assert_eq!(due[0].attempt, first);
    assert!(
        harness
            .registered
            .pending_receipted_delivery_attempts_due(
                "scope:fixture",
                DeliverySurfaceFamilyV1::Mcp,
                UtcMicros(110),
                1,
            )
            .await
            .expect("surface-isolated page")
            .is_empty()
    );
    assert_eq!(
        harness
            .registered
            .begin_receipted_delivery_attempt("scope:fixture", &first, receipt_ref)
            .await
            .expect("exact receipt replay"),
        DeliveryAttemptClaimV1::ReplayedAttempt
    );

    let second = attempt(
        DeliverySurfaceFamilyV1::Dashboard,
        "dashboard:connection-8:event-42",
    );
    let error = harness
        .registered
        .begin_receipted_delivery_attempt("scope:fixture", &second, receipt_ref)
        .await
        .expect_err("one receipt cannot address another recipient");
    assert!(error.contains("receipt identity conflict"), "{error}");
    assert_eq!(
        harness
            .registered
            .delivery_attempt_for_source_receipt("scope:other", receipt_ref)
            .await
            .expect("project-isolated lookup"),
        None
    );

    let terminal = settlement(first.clone(), DeliverySettlementOutcomeV1::Delivered);
    harness
        .registered
        .settle_delivery_attempt("scope:fixture", &terminal)
        .await
        .expect("settle receipted attempt");
    assert_eq!(
        harness
            .registered
            .delivery_attempt_for_source_receipt("scope:fixture", receipt_ref)
            .await
            .expect("settled receipt lookup"),
        Some(DeliverySourceReceiptReadV1::Settled(terminal))
    );
    assert!(
        harness
            .registered
            .pending_receipted_delivery_attempts_due(
                "scope:fixture",
                DeliverySurfaceFamilyV1::Dashboard,
                UtcMicros(120),
                1,
            )
            .await
            .expect("settled receipt excluded")
            .is_empty()
    );
}
