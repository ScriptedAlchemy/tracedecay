use std::sync::Arc;

use tracedecay_application::{
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use tracedecay_domain::{
    DeliveryChannelIdentityV1, DeliveryEventClassV1, DeliverySettlementAttemptV1,
    DeliverySettlementOutcomeV1, DeliverySettlementV1, DeliverySurfaceFamilyV1,
    ObservabilityPayloadV1, ProjectId, UtcMicros,
};
use tracedecay_usecases::observability::{
    BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
    DeliverySettlementAuthorityV1, DeliverySettlementRecordOutcomeV1,
    ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1,
};

fn attempt() -> DeliverySettlementAttemptV1 {
    DeliverySettlementAttemptV1 {
        owner_event_id: "work:attempt:42:artifact:result".to_owned(),
        event_class: DeliveryEventClassV1::OperationTerminal,
        channel: DeliveryChannelIdentityV1 {
            surface: DeliverySurfaceFamilyV1::Mcp,
            channel_ref: "mcp:connection-7:request-42".to_owned(),
        },
        work_attempt: None,
        eligible: 1,
        valid_at: UtcMicros(100),
        attempted_at: UtcMicros(110),
    }
}

#[tokio::test]
async fn fanout_observation_exists_only_after_durable_terminal_settlement() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.delivery.settlement").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:delivery-settlement".to_owned(),
        producer_revision: "delivery-settlement-producer.v1".to_owned(),
        configuration_revision: "delivery-settlement-config.v1".to_owned(),
        policy_revision: "delivery-settlement-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity.clone(), 8)
            .expect("producer"),
    );
    let authority =
        DeliverySettlementAuthorityV1::new(Arc::clone(&db), Arc::clone(&producer), identity)
            .expect("settlement authority");

    authority
        .begin(&attempt())
        .await
        .expect("durable attempt admission");
    let before = RegisteredObservabilityPortV1::new(db.as_ref())
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("pre-settlement query");
    assert!(before.events.is_empty());

    let terminal = DeliverySettlementV1 {
        attempt: attempt(),
        outcome: DeliverySettlementOutcomeV1::Delivered,
        settled_at: UtcMicros(120),
        drop_reason: None,
    };
    let first = authority
        .settle(&terminal)
        .await
        .expect("terminal settlement");
    let replay = authority.settle(&terminal).await.expect("terminal replay");
    assert!(!first.receipt.replayed);
    assert!(replay.receipt.replayed);

    drop(authority);
    let producer = match Arc::try_unwrap(producer) {
        Ok(producer) => producer,
        Err(_) => panic!("authority must release producer after drop"),
    };
    producer.shutdown().await.expect("flush producer");
    let page = RegisteredObservabilityPortV1::new(db.as_ref())
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("settled observation query");
    assert_eq!(page.events.len(), 1, "outbox replay must not double count");
    let ObservabilityPayloadV1::WorkDeliveryFanout(fanout) = &page.events[0].payload else {
        panic!("expected delivery fanout payload");
    };
    assert_eq!(fanout.eligible, 1);
    assert_eq!(fanout.attempted, 1);
    assert_eq!(fanout.delivered, 1);
    assert_eq!(fanout.unknown, 0);
}

#[tokio::test]
async fn bounded_recorder_keeps_settlement_io_off_the_delivery_boundary_and_drains() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.delivery.recorder").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:delivery-recorder".to_owned(),
        producer_revision: "delivery-recorder-producer.v1".to_owned(),
        configuration_revision: "delivery-recorder-config.v1".to_owned(),
        policy_revision: "delivery-recorder-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity.clone(), 8)
            .expect("producer"),
    );
    let authority = Arc::new(
        DeliverySettlementAuthorityV1::new(Arc::clone(&db), Arc::clone(&producer), identity)
            .expect("settlement authority"),
    );
    let recorder =
        BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 1).expect("recorder");
    let terminal = DeliverySettlementV1 {
        attempt: attempt(),
        outcome: DeliverySettlementOutcomeV1::Delivered,
        settled_at: UtcMicros(120),
        drop_reason: None,
    };

    assert_eq!(
        recorder.try_record(terminal).expect("nonblocking offer"),
        DeliverySettlementRecordOutcomeV1::Enqueued
    );
    let summary = recorder.shutdown().await.expect("drain recorder");
    assert_eq!(summary.settled, 1);
    assert_eq!(summary.failed, 0);

    drop(recorder);
    drop(authority);
    let producer = match Arc::try_unwrap(producer) {
        Ok(producer) => producer,
        Err(_) => panic!("recorder must release producer after shutdown"),
    };
    producer.shutdown().await.expect("flush producer");
    let page = RegisteredObservabilityPortV1::new(db.as_ref())
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("settled observation query");
    assert_eq!(page.events.len(), 1);
}

#[tokio::test]
async fn recorder_queue_saturation_retains_every_durable_receipt() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.delivery.saturation").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:delivery-saturation".to_owned(),
        producer_revision: "delivery-saturation-producer.v1".to_owned(),
        configuration_revision: "delivery-saturation-config.v1".to_owned(),
        policy_revision: "delivery-saturation-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity.clone(), 1)
            .expect("producer"),
    );
    let authority = Arc::new(
        DeliverySettlementAuthorityV1::new(Arc::clone(&db), Arc::clone(&producer), identity)
            .expect("settlement authority"),
    );
    let recorder =
        BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 1).expect("recorder");

    for index in 0..32 {
        let terminal = DeliverySettlementV1 {
            attempt: DeliverySettlementAttemptV1 {
                owner_event_id: format!("work:attempt:saturation:{index}"),
                event_class: DeliveryEventClassV1::OperationTerminal,
                channel: DeliveryChannelIdentityV1 {
                    surface: DeliverySurfaceFamilyV1::Mcp,
                    channel_ref: format!("mcp:saturation:{index}"),
                },
                work_attempt: None,
                eligible: 1,
                valid_at: UtcMicros(100 + index),
                attempted_at: UtcMicros(200 + index),
            },
            outcome: DeliverySettlementOutcomeV1::Delivered,
            settled_at: UtcMicros(300 + index),
            drop_reason: None,
        };
        assert_eq!(
            recorder.try_record(terminal).expect("durable offer"),
            DeliverySettlementRecordOutcomeV1::Enqueued
        );
    }

    let summary = recorder.shutdown().await.expect("drain recorder");
    assert_eq!(summary.settled, 32);
    assert_eq!(summary.retained, 0);
    drop(recorder);
    drop(authority);
    let producer = match Arc::try_unwrap(producer) {
        Ok(producer) => producer,
        Err(_) => panic!("recorder must release producer after shutdown"),
    };
    producer.shutdown().await.expect("flush producer");
}

#[tokio::test]
async fn recorder_replays_retained_receipt_after_transient_db_failure_and_restart() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.delivery.restart").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:delivery-restart".to_owned(),
        producer_revision: "delivery-restart-producer.v1".to_owned(),
        configuration_revision: "delivery-restart-config.v1".to_owned(),
        policy_revision: "delivery-restart-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity.clone(), 8)
            .expect("producer"),
    );
    let authority = Arc::new(
        DeliverySettlementAuthorityV1::new(Arc::clone(&db), Arc::clone(&producer), identity)
            .expect("settlement authority"),
    );
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("failure trigger transaction");
    transaction
        .execute(
            "CREATE TRIGGER delivery_settlement_test_reject
             BEFORE INSERT ON delivery_settlements BEGIN
                 SELECT RAISE(ABORT, 'transient delivery settlement failure');
             END",
            tracedecay_runtime_core::db::engine::params![],
        )
        .await
        .expect("install failure trigger");
    transaction.commit().await.expect("commit failure trigger");

    let recorder =
        BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 1).expect("recorder");
    let terminal = DeliverySettlementV1 {
        attempt: attempt(),
        outcome: DeliverySettlementOutcomeV1::Delivered,
        settled_at: UtcMicros(120),
        drop_reason: None,
    };
    assert_eq!(
        recorder
            .try_record(terminal.clone())
            .expect("durable offer"),
        DeliverySettlementRecordOutcomeV1::Enqueued
    );
    let failed = recorder.shutdown().await.expect("retain failed receipt");
    assert_eq!(failed.settled, 0);
    assert_eq!(failed.retained, 1);
    drop(recorder);

    let transaction = db
        .begin_write_transaction()
        .await
        .expect("recovery transaction");
    transaction
        .execute(
            "DROP TRIGGER delivery_settlement_test_reject",
            tracedecay_runtime_core::db::engine::params![],
        )
        .await
        .expect("remove failure trigger");
    transaction.commit().await.expect("commit recovery");

    let restarted =
        BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 1).expect("restart");
    let recovered = restarted.shutdown().await.expect("restart replay");
    assert_eq!(recovered.settled, 1);
    assert_eq!(recovered.retained, 0);
    drop(restarted);

    let replay = authority
        .settle(&terminal)
        .await
        .expect("durable settlement replay");
    assert!(replay.receipt.replayed);
    drop(authority);
    let producer = match Arc::try_unwrap(producer) {
        Ok(producer) => producer,
        Err(_) => panic!("recorder must release producer after restart"),
    };
    producer.shutdown().await.expect("flush producer");
}
