use std::collections::{BTreeMap, BTreeSet};
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

use super::StoreObservabilityRegistryV1;

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
async fn linked_roots_alias_one_store_producer_until_the_last_alias_shuts_down() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database) = runtime("observability-store-alias").await;
    let root = PathBuf::from("/project/observability-store-alias");
    let linked_root = PathBuf::from("/project/observability-store-alias-linked");
    let first_service = DaemonInvocationService::default();
    let first = first_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("first producer");
    let first_reconciled = first_service
        .mount_observability_producer(
            root.clone(),
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
            linked_root.clone(),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("linked-root producer");
    // Linked roots are aliases of one store-keyed producer: same Arc, one
    // ordered boot stream per registered store, never one per root.
    assert!(Arc::ptr_eq(&first, &linked));
    assert_eq!(
        first.identity().process_boot_id,
        linked.identity().process_boot_id
    );
    // The delivery settlement recorder is store-keyed: both roots reach the
    // exact same recorder rather than running one drain per root.
    let first_recorder = first_service
        .delivery_settlement_recorder(Some(&root))
        .await
        .expect("first-root recorder");
    let linked_recorder = first_service
        .delivery_settlement_recorder(Some(&linked_root))
        .await
        .expect("linked-root recorder");
    assert!(Arc::ptr_eq(&first_recorder, &linked_recorder));
    // Retaining recorder handles would pin the store spool lock past the
    // last-alias shutdown below and block the restart from reopening it.
    drop(first_recorder);
    drop(linked_recorder);
    // A root presenting different revisions for the same registered store is
    // refused, not given a second store owner and not silently aliased.
    let refused = match first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-store-alias-foreign"),
            database.clone(),
            project_id.clone(),
            digest('9'),
            digest('2'),
        )
        .await
    {
        Ok(_) => panic!("mismatched revisions must not mount a second store producer"),
        Err(error) => error,
    };
    assert!(
        refused.to_string().contains("already mounted"),
        "unexpected refusal: {refused}"
    );
    first
        .try_emit(envelope(&project_id, "alias:first"))
        .expect("first emission");
    linked
        .try_emit(envelope(&project_id, "alias:linked"))
        .expect("linked emission");

    // Full-upgrade shape for one linked root: quiesce drains that root's
    // runtime while the other alias keeps the store producer and its boot
    // stream alive; the remount reattaches to the same live producer.
    let lsp_registry = Arc::new(tokio::sync::Mutex::new(
        tracedecay_lsp::LspSessionRegistry::default(),
    ));
    let profile_id = database.binding().shard_id.profile_id.clone();
    let quiescence = first_service
        .quiesce_project(
            &lsp_registry,
            &profile_id,
            &project_id,
            &BTreeSet::from([root.clone()]),
        )
        .await
        .expect("quiesce the first root");
    assert_eq!(
        linked
            .try_emit(envelope(&project_id, "alias:after-quiesce"))
            .expect("surviving alias emission"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    drop(quiescence);
    let remounted = first_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("remounted producer after quiescence");
    assert!(Arc::ptr_eq(&remounted, &linked));
    assert_eq!(
        remounted.identity().process_boot_id,
        linked.identity().process_boot_id
    );
    remounted
        .try_emit(envelope(&project_id, "alias:remounted"))
        .expect("remounted emission");

    // The last alias shuts the store producer down.
    first_service.expire_all().await;
    assert_eq!(
        linked
            .try_emit(envelope(&project_id, "alias:after-shutdown"))
            .expect_err("last alias shutdown closes the store producer"),
        "observability_producer_closed"
    );

    let restarted_service = DaemonInvocationService::default();
    let restarted = restarted_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('1'),
            digest('2'),
        )
        .await
        .expect("restarted producer");
    assert!(!Arc::ptr_eq(&first, &restarted));
    assert_ne!(
        first.identity().process_boot_id,
        restarted.identity().process_boot_id
    );
    let registration = |identity: &ObservabilityProducerIdentityV1| {
        identity
            .process_boot_id
            .rsplit(':')
            .next()
            .expect("registration suffix")
            .parse::<u64>()
            .expect("numeric registration suffix")
    };
    assert!(registration(restarted.identity()) > registration(first.identity()));
    restarted
        .try_emit(envelope(&project_id, "alias:restarted"))
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
    assert_eq!(page.events.len(), 5);
    let mut streams: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for event in &page.events {
        streams
            .entry(event.process_boot_id.as_str())
            .or_default()
            .insert(event.producer_sequence);
    }
    // One shared alias stream carries every linked-root emission in order;
    // the restart after the last-alias shutdown boots a second stream.
    assert_eq!(streams.len(), 2);
    assert_eq!(
        streams
            .get(first.identity().process_boot_id.as_str())
            .expect("shared alias stream"),
        &BTreeSet::from([1, 2, 3, 4])
    );
    assert_eq!(
        streams
            .get(restarted.identity().process_boot_id.as_str())
            .expect("restarted stream"),
        &BTreeSet::from([1])
    );
    let process_prefix = format!("daemon:{}:", crate::runtime_identity::process_run_id());
    assert!(
        streams
            .keys()
            .all(|boot_id| boot_id.starts_with(&process_prefix))
    );
}

#[tokio::test]
async fn exact_store_routing_collapses_linked_roots_without_crossing_stores() {
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
    // The test-runtime resolver pins one logical brain/profile identity for
    // every store, so the two profile stores are distinguished by exactly the
    // registered-store authority the producer registry keys on: the verified
    // locator and the registered client token. Logical shard ids alone must
    // never be treated as the same authority.
    let brain_id = database_a.binding().shard_id.brain_id.clone();
    let profile_id = database_a.binding().shard_id.profile_id.clone();
    assert_eq!(brain_id, database_b.binding().shard_id.brain_id);
    assert_eq!(profile_id, database_b.binding().shard_id.profile_id);
    assert!(!database_a.shares_client_with(&database_b));
    assert_ne!(database_a.verified_locator(), database_b.verified_locator());
    let service = DaemonInvocationService::default();
    let root_a = PathBuf::from("/project/profile-a/shared-observability");
    let linked_a = PathBuf::from("/project/profile-a/shared-observability-linked");
    let root_b = PathBuf::from("/project/profile-b/shared-observability");
    let producer_a = service
        .mount_observability_producer(
            root_a.clone(),
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
    // Linked roots of one exact store alias one producer, and while that is
    // the only mounted store its exact identity routing resolves it.
    assert!(Arc::ptr_eq(&producer_a, &linked_producer_a));
    let routed_a = service
        .observability_producer_for_brain_profile_project(&brain_id, &profile_id, &project_id)
        .expect("linked roots resolve one exact profile A store");
    assert!(Arc::ptr_eq(&routed_a, &producer_a));

    // The same logical identity behind a different registered store must not
    // alias profile A's producer, even though only the locator and client
    // token distinguish the two stores.
    let producer_b = service
        .mount_observability_producer(
            root_b.clone(),
            database_b,
            project_id.clone(),
            digest('3'),
            digest('4'),
        )
        .await
        .expect("profile B producer");
    assert!(!Arc::ptr_eq(&producer_a, &producer_b));
    let recorder_a = service
        .delivery_settlement_recorder(Some(&root_a))
        .await
        .expect("profile A recorder");
    let recorder_b = service
        .delivery_settlement_recorder(Some(&root_b))
        .await
        .expect("profile B recorder");
    assert!(!Arc::ptr_eq(&recorder_a, &recorder_b));
    // With two distinct store authorities mounted under one logical identity,
    // exact routing refuses to pick either rather than crossing stores.
    assert!(
        service
            .observability_producer_for_brain_profile_project(&brain_id, &profile_id, &project_id)
            .is_none()
    );
    // A foreign identity never routes to a mounted store.
    let foreign_project = ProjectId::new("project.unmounted-observability").unwrap();
    assert!(
        service
            .observability_producer_for_brain_profile_project(
                &brain_id,
                &profile_id,
                &foreign_project,
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
    let registered = StoreObservabilityRegistryV1::default()
        .acquire_or_start::<&'static str>(
            &database,
            |_| false,
            || "unexpected incumbent store producer",
            || Ok(producer),
            1,
            |error| error,
        )
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
