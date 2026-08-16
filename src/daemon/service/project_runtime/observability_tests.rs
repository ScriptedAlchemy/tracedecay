use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use tracedecay_domain::{
    CoverageStateV1, ManifestDigest, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, ProjectId,
    RetrievalQueryObservedV1,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerDeadlinesV1, ObservabilityProducerIdentityV1,
    RegisteredObservabilityPortV1,
};

use crate::daemon::service::invocation::DaemonInvocationService;

use super::RegisteredObservabilityProducerV1;

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn envelope(scope: &ProjectId, event: &str) -> ObservabilityEnvelopeV1 {
    let payload = ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
        query_family: "exact_technical".to_owned(),
        enabled_lanes: vec!["exact_literal".to_owned()],
        candidate_budget: 8,
        context_budget: 4,
        token_budget: 128,
        answered: true,
        source_coverage: CoverageStateV1::Known,
        lane_coverage: CoverageStateV1::Known,
    });
    ObservabilityEnvelopeV1 {
        event_id: event.to_owned(),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: event.to_owned(),
        trace_id: event.to_owned(),
        scope_ref: scope.as_str().to_owned(),
        capability: "retrieval".to_owned(),
        operation: "query".to_owned(),
        event_time_micros: 10,
        observation_time_micros: 11,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(1.0),
        unit: Some("events".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "caller".to_owned(),
        configuration_revision: "caller".to_owned(),
        policy_revision: "caller".to_owned(),
        watermark: "caller".to_owned(),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 1,
        dropped_count: 0,
        process_boot_id: "caller".to_owned(),
        producer_sequence: 1,
        payload,
    }
}

async fn runtime(
    name: &str,
) -> (
    tempfile::TempDir,
    ProjectId,
    crate::global_db::RegisteredGlobalDbLeaseV1,
) {
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new(format!("project.{name}")).expect("project id");
    let runtime = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    (project, project_id, database)
}

#[tokio::test]
async fn project_runtime_reuses_one_producer_and_shutdown_flushes_it() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database) = runtime("observability-mount").await;
    let root = PathBuf::from("/project/observability-mount");
    let service = DaemonInvocationService::default();
    let first = service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('a'),
            digest('b'),
        )
        .await
        .expect("first mount");
    let second = service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('a'),
            digest('b'),
        )
        .await
        .expect("reconciled mount");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        first
            .try_emit(envelope(&project_id, "mounted:event"))
            .expect("enqueue"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );

    service.expire_all().await;
    assert_eq!(
        first
            .try_emit(envelope(&project_id, "mounted:after-shutdown"))
            .expect_err("producer closed"),
        "observability_producer_closed"
    );
    let page = RegisteredObservabilityPortV1::new(&database)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 100,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query flushed event");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].delayed_count, 1);
}

#[tokio::test]
async fn a_new_daemon_runtime_restarts_the_project_producer_after_clean_shutdown() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database) = runtime("observability-restart").await;
    let root = PathBuf::from("/project/observability-restart");
    let first_service = DaemonInvocationService::default();
    let first = first_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('c'),
            digest('d'),
        )
        .await
        .expect("first daemon mount");
    first_service.expire_all().await;

    let restarted_service = DaemonInvocationService::default();
    let restarted = restarted_service
        .mount_observability_producer(
            root,
            database.clone(),
            project_id.clone(),
            digest('c'),
            digest('d'),
        )
        .await
        .expect("restarted daemon mount");
    assert!(!Arc::ptr_eq(&first, &restarted));
    assert_eq!(
        restarted
            .try_emit(envelope(&project_id, "restart:event"))
            .expect("restarted enqueue"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    restarted_service.expire_all().await;
}

#[tokio::test]
async fn each_producer_lifetime_uses_a_disjoint_ordered_stream() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database) = runtime("observability-stream-identity").await;
    let first_service = DaemonInvocationService::default();
    let first = first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-stream-identity"),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("first producer");
    let first_reconciled = first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-stream-identity"),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("reconciled first producer");
    assert!(Arc::ptr_eq(&first, &first_reconciled));
    assert_eq!(
        first.identity().process_boot_id,
        first_reconciled.identity().process_boot_id
    );
    let linked = first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-stream-identity-linked"),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("linked-root producer");
    first
        .try_emit(envelope(&project_id, "stream:first"))
        .expect("first emission");
    linked
        .try_emit(envelope(&project_id, "stream:linked"))
        .expect("linked emission");
    first_service.expire_all().await;

    let restarted_service = DaemonInvocationService::default();
    let restarted = restarted_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-stream-identity"),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("restarted producer");
    let registrations = [&first, &linked, &restarted].map(|producer| {
        producer
            .identity()
            .process_boot_id
            .rsplit(':')
            .next()
            .expect("registration suffix")
            .parse::<u64>()
            .expect("numeric registration suffix")
    });
    assert_eq!(registrations[1], registrations[0].saturating_add(1));
    assert_eq!(registrations[2], registrations[1].saturating_add(1));
    restarted
        .try_emit(envelope(&project_id, "stream:restarted"))
        .expect("restarted emission");
    restarted_service.expire_all().await;

    let page = RegisteredObservabilityPortV1::new(&database)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 100,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query producer streams");
    assert_eq!(page.events.len(), 3);
    assert!(page.events.iter().all(|event| event.producer_sequence == 1));
    let boot_ids = page
        .events
        .iter()
        .map(|event| event.process_boot_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(boot_ids.len(), 3);
    let process_prefix = format!("daemon:{}:", crate::runtime_identity::process_run_id());
    assert!(
        boot_ids
            .iter()
            .all(|boot_id| boot_id.starts_with(&process_prefix))
    );
}

#[tokio::test]
async fn exact_profile_routing_collapses_linked_roots_without_crossing_profiles() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let profile_a = tempfile::tempdir().expect("profile A");
    let profile_b = tempfile::tempdir().expect("profile B");
    let project_a = tempfile::tempdir().expect("project A");
    let project_b = tempfile::tempdir().expect("project B");
    let project_id = ProjectId::new("project.shared-observability").unwrap();
    let runtime_a = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        profile_a.path(),
        project_a.path(),
        project_id.clone(),
    )
    .await
    .expect("profile A runtime");
    let runtime_b = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        profile_b.path(),
        project_b.path(),
        project_id.clone(),
    )
    .await
    .expect("profile B runtime");
    let database_a = runtime_a
        .project_database_arc()
        .expect("profile A database");
    let database_b = runtime_b
        .project_database_arc()
        .expect("profile B database");
    let brain_a_id = database_a.binding().shard_id.brain_id.clone();
    let brain_b_id = database_b.binding().shard_id.brain_id.clone();
    let profile_a_id = database_a.binding().shard_id.profile_id.clone();
    let profile_b_id = database_b.binding().shard_id.profile_id.clone();
    assert_ne!(brain_a_id, brain_b_id);
    assert_ne!(profile_a_id, profile_b_id);
    let service = DaemonInvocationService::default();
    let root_a = PathBuf::from("/project/profile-a/shared-observability");
    let linked_a = PathBuf::from("/project/profile-a/shared-observability-linked");
    let root_b = PathBuf::from("/project/profile-b/shared-observability");
    let producer_a = service
        .mount_observability_producer(
            root_a,
            database_a.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("profile A producer");
    let linked_producer_a = service
        .mount_observability_producer(
            linked_a,
            database_a,
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("linked profile A producer");
    let producer_b = service
        .mount_observability_producer(
            root_b,
            database_b,
            project_id.clone(),
            digest('3'),
            digest('4'),
        )
        .await
        .expect("profile B producer");

    let routed_a = service
        .observability_producer_for_brain_profile_project(&brain_a_id, &profile_a_id, &project_id)
        .expect("linked roots resolve one exact profile A store");
    assert!(Arc::ptr_eq(&routed_a, &producer_a) || Arc::ptr_eq(&routed_a, &linked_producer_a));
    let routed_b = service
        .observability_producer_for_brain_profile_project(&brain_b_id, &profile_b_id, &project_id)
        .expect("profile B exact producer");
    assert!(Arc::ptr_eq(&routed_b, &producer_b));
    assert!(!Arc::ptr_eq(&routed_a, &routed_b));
    assert!(
        service
            .observability_producer_for_brain_profile_project(
                &brain_b_id,
                &profile_a_id,
                &project_id,
            )
            .is_none()
    );
    service.expire_all().await;
}

#[tokio::test]
async fn registered_shutdown_reports_a_blocked_producer_flush() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database) = runtime("observability-shutdown-failure").await;
    let producer = BoundedObservabilityProducerV1::start_with_deadlines(
        database.clone(),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "daemon:shutdown-failure".to_owned(),
            producer_revision: "producer.v1".to_owned(),
            configuration_revision: digest('e').as_str().to_owned(),
            policy_revision: digest('f').as_str().to_owned(),
        },
        1,
        ObservabilityProducerDeadlinesV1 {
            persistence: Duration::from_millis(50),
            shutdown: Duration::from_millis(250),
        },
    )
    .expect("producer");
    let registered = RegisteredObservabilityProducerV1::new(database.clone(), producer, 1)
        .expect("registered observability producer");
    let blocker = database
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    registered
        .producer()
        .try_emit(envelope(&project_id, "shutdown:blocked"))
        .expect("enqueue blocked event");
    tokio::task::yield_now().await;

    let error = registered
        .shutdown()
        .await
        .expect_err("blocked flush must fail the registered shutdown");
    blocker.commit().await.expect("release registered writer");
    assert!(
        error
            .to_string()
            .contains("observability_persistence_deadline"),
        "unexpected shutdown error: {error}"
    );
}
