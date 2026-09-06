use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracedecay_application::{
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use tracedecay_daemon_service::{
    DaemonInvocationService, StoreObservabilityMountErrorV1, StoreObservabilityMountV1,
    StoreObservabilityRegistryV1,
};
use tracedecay_domain::{
    CoverageStateV1, DeliveryChannelIdentityV1, DeliveryEventClassV1, DeliverySettlementAttemptV1,
    DeliverySettlementOutcomeV1, DeliverySettlementV1, DeliverySurfaceFamilyV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, ProjectId, RepositoryId, RetrievalQueryObservedV1, UtcMicros,
    WorktreeId, canonical_sha256,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, DeliverySettlementRecordOutcomeV1,
    ObservabilityEmissionOutcomeV1, ObservabilityProducerDeadlinesV1,
    ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

/// A registry mount request whose store-authority fields match `identity`,
/// stamping that identity's configuration and policy revisions for the
/// mounting root.
fn store_mount(identity: &ObservabilityProducerIdentityV1) -> StoreObservabilityMountV1 {
    StoreObservabilityMountV1::new(
        identity.authorized_scope_ref.clone(),
        identity.producer_revision.clone(),
        identity.configuration_revision.clone(),
        identity.policy_revision.clone(),
        1,
    )
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
    tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
) {
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new(format!("project.{name}")).expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    // The fixture owns the active daemon write scope; it must outlive every
    // store write in the test, so it is returned rather than dropped here.
    (project, project_id, database, runtime)
}

#[tokio::test]
async fn project_runtime_reuses_one_producer_and_shutdown_flushes_it() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) = runtime("observability-mount").await;
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
async fn a_same_root_remount_stamps_the_configuration_it_resolved() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) = runtime("observability-remount").await;
    let root = PathBuf::from("/project/observability-remount");
    let service = DaemonInvocationService::default();
    let under_a = service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('a'),
            digest('b'),
        )
        .await
        .expect("mount under configuration A");
    assert_eq!(
        under_a
            .try_emit(envelope(&project_id, "remount:under-a"))
            .expect("configuration A emission"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    // The store's canonical configuration advances and this same root opens
    // again, resolving the newer revision. The mount must hand back a frontend
    // that stamps it rather than the revision frozen at the first mount.
    let under_b = service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('c'),
            digest('d'),
        )
        .await
        .expect("remount under configuration B");
    assert!(!Arc::ptr_eq(&under_a, &under_b));
    assert_eq!(
        under_b.identity().configuration_revision,
        digest('c').as_str()
    );
    assert_eq!(under_b.identity().policy_revision, digest('d').as_str());
    // One owner: the remount joins the incumbent boot stream rather than
    // starting a second producer for the same store.
    assert_eq!(
        under_a.identity().process_boot_id,
        under_b.identity().process_boot_id
    );
    assert_eq!(
        under_b
            .try_emit(envelope(&project_id, "remount:under-b"))
            .expect("configuration B emission"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    service.expire_all().await;

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
        .expect("query both remount streams");
    let stamped: BTreeMap<&str, (&str, &str, u64)> = page
        .events
        .iter()
        .map(|event| {
            (
                event.event_id.as_str(),
                (
                    event.configuration_revision.as_str(),
                    event.policy_revision.as_str(),
                    event.producer_sequence,
                ),
            )
        })
        .collect();
    assert_eq!(
        stamped.get("remount:under-a").copied(),
        Some((digest('a').as_str(), digest('b').as_str(), 1)),
        "an observation admitted under configuration A keeps A's provenance"
    );
    assert_eq!(
        stamped.get("remount:under-b").copied(),
        Some((digest('c').as_str(), digest('d').as_str(), 2)),
        "an observation admitted after the remount carries configuration B, \
         from the one sequence the store owner allocates"
    );
}

#[tokio::test]
async fn a_new_daemon_runtime_restarts_the_project_producer_after_clean_shutdown() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) = runtime("observability-restart").await;
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
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.observability-store-alias").expect("project id");
    let registered_runtime =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
    let database = registered_runtime
        .project_database_arc()
        .expect("first project database client");
    let linked_database = registered_runtime
        .issue_project_database_lease_for_test()
        .expect("independent linked-root database client");
    assert!(!database.shares_client_with(&linked_database));
    assert_eq!(database.binding(), linked_database.binding());
    assert_eq!(
        database.verified_locator(),
        linked_database.verified_locator()
    );
    let repository_id =
        RepositoryId::new("repository.observability-store-alias").expect("repository id");
    let root_scope = tracedecay_application::ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.observability-store-alias").expect("root worktree id"),
        None,
    )
    .expect("root scope");
    let linked_scope = tracedecay_application::ResolvedScope::new(
        project_id.clone(),
        repository_id,
        WorktreeId::new("worktree.observability-store-alias-linked").expect("linked worktree id"),
        None,
    )
    .expect("linked scope");
    assert_ne!(root_scope.scope_digest, linked_scope.scope_digest);
    let configuration_revision = digest('1');
    let configuration_provenance_revision = digest('2');
    let policy_revision = |scope: &tracedecay_application::ResolvedScope| {
        canonical_sha256(&(
            "tracedecay.daemon.configuration-policy.v1",
            &scope.scope_digest,
            &configuration_revision,
            &configuration_provenance_revision,
        ))
        .expect("scope-derived configuration policy")
    };
    let root_policy_revision = policy_revision(&root_scope);
    let linked_policy_revision = policy_revision(&linked_scope);
    assert_ne!(root_policy_revision, linked_policy_revision);
    let root = PathBuf::from("/project/observability-store-alias");
    let linked_root = PathBuf::from("/project/observability-store-alias-linked");
    let first_service = DaemonInvocationService::default();
    let first = first_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            configuration_revision.clone(),
            root_policy_revision.clone(),
        )
        .await
        .expect("first producer");
    let first_reconciled = first_service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            configuration_revision.clone(),
            root_policy_revision.clone(),
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
            linked_database.clone(),
            project_id.clone(),
            configuration_revision.clone(),
            linked_policy_revision.clone(),
        )
        .await
        .expect("linked-root producer");
    // Linked roots have distinct policy-stamping frontends over one ordered
    // store backend and boot stream.
    assert!(!Arc::ptr_eq(&first, &linked));
    assert_eq!(
        first.identity().process_boot_id,
        linked.identity().process_boot_id
    );
    assert_eq!(
        first.identity().policy_revision,
        root_policy_revision.as_str()
    );
    assert_eq!(
        linked.identity().policy_revision,
        linked_policy_revision.as_str()
    );
    let linked_reconciled = first_service
        .mount_observability_producer(
            linked_root.clone(),
            registered_runtime
                .issue_project_database_lease_for_test()
                .expect("fresh reconciled linked-root database client"),
            project_id.clone(),
            configuration_revision.clone(),
            linked_policy_revision.clone(),
        )
        .await
        .expect("reconciled linked-root producer");
    assert!(Arc::ptr_eq(&linked, &linked_reconciled));
    // Each root keeps its policy-specific admission frontend while both
    // frontends share the exact store recorder backend and drain worker.
    let first_recorder = first_service
        .delivery_settlement_recorder(Some(&root))
        .await
        .expect("first-root recorder");
    let linked_recorder = first_service
        .delivery_settlement_recorder(Some(&linked_root))
        .await
        .expect("linked-root recorder");
    assert!(!Arc::ptr_eq(&first_recorder, &linked_recorder));
    assert_eq!(
        linked_recorder
            .try_record(DeliverySettlementV1 {
                attempt: DeliverySettlementAttemptV1 {
                    owner_event_id: "linked:settlement:b".to_owned(),
                    event_class: DeliveryEventClassV1::OperationTerminal,
                    channel: DeliveryChannelIdentityV1 {
                        surface: DeliverySurfaceFamilyV1::Lsp,
                        channel_ref: "lsp:linked-root-b".to_owned(),
                    },
                    work_attempt: None,
                    eligible: 1,
                    valid_at: UtcMicros(20),
                    attempted_at: UtcMicros(21),
                },
                outcome: DeliverySettlementOutcomeV1::Delivered,
                settled_at: UtcMicros(22),
                drop_reason: None,
            })
            .expect("record linked-root B settlement"),
        DeliverySettlementRecordOutcomeV1::Enqueued
    );
    // Retaining recorder handles would pin the store spool lock past the
    // last-alias shutdown below and block the restart from reopening it.
    drop(first_recorder);
    drop(linked_recorder);
    // The store's canonical configuration advances while these roots stay
    // mounted, so a later root of the same store legitimately resolves a newer
    // revision. It aliases the incumbent owners — one producer, one boot
    // stream — and stamps its own configuration and policy provenance instead
    // of being refused for the life of the daemon.
    let advanced_configuration_revision = digest('9');
    let advanced_policy_revision = canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &linked_scope.scope_digest,
        &advanced_configuration_revision,
        &configuration_provenance_revision,
    ))
    .expect("advanced configuration policy");
    let advanced = first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-store-alias-advanced"),
            database.clone(),
            project_id.clone(),
            advanced_configuration_revision.clone(),
            advanced_policy_revision.clone(),
        )
        .await
        .expect("a newer store configuration must alias the incumbent owners");
    assert!(!Arc::ptr_eq(&first, &advanced));
    assert_eq!(
        first.identity().process_boot_id,
        advanced.identity().process_boot_id,
        "an advanced-configuration alias must join the incumbent boot stream"
    );
    assert_eq!(
        advanced.identity().configuration_revision,
        advanced_configuration_revision.as_str()
    );
    assert_eq!(
        advanced.identity().policy_revision,
        advanced_policy_revision.as_str()
    );
    // A mount for a different authorized scope is still refused: that is store
    // authority, not provenance, and it must never be silently aliased.
    let foreign_project_id =
        ProjectId::new("project.observability-store-alias-foreign").expect("foreign project id");
    let refused = match first_service
        .mount_observability_producer(
            PathBuf::from("/project/observability-store-alias-foreign"),
            database.clone(),
            foreign_project_id,
            configuration_revision.clone(),
            root_policy_revision.clone(),
        )
        .await
    {
        Ok(_) => panic!("a foreign authorized scope must not alias the store producer"),
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
            configuration_revision.clone(),
            root_policy_revision.clone(),
        )
        .await
        .expect("remounted producer after quiescence");
    assert!(!Arc::ptr_eq(&remounted, &linked));
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
            configuration_revision.clone(),
            root_policy_revision.clone(),
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
    let persisted_policies: BTreeMap<&str, &str> = page
        .events
        .iter()
        .map(|event| (event.event_id.as_str(), event.policy_revision.as_str()))
        .collect();
    assert_eq!(
        persisted_policies.get("alias:first").copied(),
        Some(root_policy_revision.as_str())
    );
    assert_eq!(
        persisted_policies.get("alias:linked").copied(),
        Some(linked_policy_revision.as_str()),
        "the linked-root frontend must stamp its own policy provenance"
    );
    let delivery_page = RegisteredObservabilityPortV1::new(&database)
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
        .expect("query linked-root settlement");
    let [delivery] = delivery_page.events.as_slice() else {
        panic!("one linked-root delivery settlement observation is required");
    };
    assert_eq!(
        delivery.policy_revision,
        linked_policy_revision.as_str(),
        "the root-selected delivery recorder must retain root B's policy"
    );
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
    let process_prefix = format!(
        "daemon:{}:",
        tracedecay_runtime_core::runtime_identity::process_run_id()
    );
    assert!(
        streams
            .keys()
            .all(|boot_id| boot_id.starts_with(&process_prefix))
    );
}

#[tokio::test]
async fn exact_store_routing_collapses_linked_roots_without_crossing_stores() {
    let profile_a_dir = tempfile::tempdir().expect("profile A");
    let profile_b_dir = tempfile::tempdir().expect("profile B");
    let profile_a = profile_a_dir.path().join("profile");
    let profile_b = profile_b_dir.path().join("profile");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_a)
        .expect("create private profile A");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_b)
        .expect("create private profile B");
    let project_a = tempfile::tempdir().expect("project A");
    let project_b = tempfile::tempdir().expect("project B");
    let project_id = ProjectId::new("project.shared-observability").unwrap();
    let profile_identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_a)
        .expect("persist production profile identity");
    let runtime_identity = tracedecay_runtime_core::db::TestRuntimeProfileIdentityV1::new(
        profile_identity.brain_id().clone(),
        profile_identity.profile_id().clone(),
    );
    let runtime_a =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project_for_profile_identity(
            &profile_a,
            project_a.path(),
            project_id.clone(),
            runtime_identity.clone(),
        )
        .await
        .expect("profile A runtime");
    let runtime_b =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project_for_profile_identity(
            &profile_b,
            project_b.path(),
            project_id.clone(),
            runtime_identity,
        )
        .await
        .expect("profile B runtime");
    let database_a = runtime_a
        .project_database_arc()
        .expect("profile A database");
    let database_b = runtime_b
        .project_database_arc()
        .expect("profile B database");
    // Both collision stores are published under one identity minted and
    // persisted by the production profile authority. They are distinguished
    // by the exact registered-store locator, never by a synthetic identity or
    // an independently issued client token.
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
    // Linked roots of one exact store alias one backend, and while that is
    // the only mounted store its exact identity routing resolves it.
    assert!(!Arc::ptr_eq(&producer_a, &linked_producer_a));
    assert_eq!(
        producer_a.identity().process_boot_id,
        linked_producer_a.identity().process_boot_id
    );
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
async fn last_alias_shutdown_keeps_the_store_retiring_until_drain_finishes() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) =
        runtime("observability-retiring-shutdown").await;
    let root = PathBuf::from("/project/observability-retiring-shutdown");
    let linked_root = PathBuf::from("/project/observability-retiring-shutdown-linked");
    let service = DaemonInvocationService::default();
    let producer = service
        .mount_observability_producer(
            root.clone(),
            database.clone(),
            project_id.clone(),
            digest('3'),
            digest('4'),
        )
        .await
        .expect("first producer");
    let blocker = database
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    producer
        .try_emit(envelope(&project_id, "retiring:blocked"))
        .expect("enqueue blocked event");

    let lsp_registry = Arc::new(tokio::sync::Mutex::new(
        tracedecay_lsp::LspSessionRegistry::default(),
    ));
    let profile_id = database.binding().shard_id.profile_id.clone();
    let quiescing_service = service.clone();
    let quiescing_lsp = Arc::clone(&lsp_registry);
    let quiescing_project = project_id.clone();
    let quiescing_root = root.clone();
    let quiescence = tokio::spawn(async move {
        quiescing_service
            .quiesce_project(
                &quiescing_lsp,
                &profile_id,
                &quiescing_project,
                &BTreeSet::from([quiescing_root]),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut probe = 0_u64;
        loop {
            match producer.try_emit(envelope(&project_id, &format!("retiring:probe:{probe}"))) {
                Err("observability_producer_closed") => break,
                Err(error) => panic!("unexpected producer state: {error}"),
                Ok(_) => {
                    probe += 1;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    })
    .await
    .expect("last-alias shutdown reaches the producer");

    let retiring = match service
        .mount_observability_producer(
            linked_root.clone(),
            database.clone(),
            project_id.clone(),
            digest('3'),
            digest('5'),
        )
        .await
    {
        Ok(_) => panic!("retiring store must refuse a replacement owner"),
        Err(error) => error,
    };
    assert!(
        retiring
            .to_string()
            .contains("store_observability_retiring"),
        "unexpected retiring result: {retiring}"
    );

    blocker.commit().await.expect("release registered writer");
    let quiescence = tokio::time::timeout(Duration::from_secs(3), quiescence)
        .await
        .expect("quiescence completes")
        .expect("quiescence task")
        .expect("clean project quiescence");
    drop(quiescence);
    service
        .mount_observability_producer(linked_root, database, project_id, digest('3'), digest('5'))
        .await
        .expect("one replacement mounts after retirement");
    service.expire_all().await;
}

#[tokio::test]
async fn concurrent_two_alias_release_keeps_the_store_retiring_until_drain_finishes() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) =
        runtime("observability-retiring-two-aliases").await;
    let registry = StoreObservabilityRegistryV1::default();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "daemon:retiring-two-aliases".to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: digest('6').as_str().to_owned(),
        policy_revision: digest('7').as_str().to_owned(),
    };
    let producer = BoundedObservabilityProducerV1::start(database.clone(), identity.clone(), 1)
        .expect("producer");
    let first = registry
        .acquire_or_start(&database, &store_mount(&identity), || Ok(producer))
        .expect("first alias");
    let linked_identity = ObservabilityProducerIdentityV1 {
        policy_revision: digest('8').as_str().to_owned(),
        ..identity.clone()
    };
    let linked = registry
        .acquire_or_start(&database, &store_mount(&linked_identity), || {
            Err(StoreObservabilityMountErrorV1::Unavailable(
                "must not start a second producer",
            ))
        })
        .expect("linked alias");
    let first_producer = first.producer();
    let producer = linked.producer();
    assert!(!Arc::ptr_eq(&first_producer, &producer));
    assert_eq!(
        first_producer.identity().process_boot_id,
        producer.identity().process_boot_id
    );

    let blocker = database
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    producer
        .try_emit(envelope(&project_id, "retiring:two-aliases:blocked"))
        .expect("enqueue blocked event");
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let first_barrier = Arc::clone(&barrier);
    let first_release = tokio::spawn(async move {
        first_barrier.wait().await;
        first.shutdown().await
    });
    let linked_barrier = Arc::clone(&barrier);
    let linked_release = tokio::spawn(async move {
        linked_barrier.wait().await;
        linked.shutdown().await
    });
    barrier.wait().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut probe = 0_u64;
        loop {
            match producer.try_emit(envelope(
                &project_id,
                &format!("retiring:two-aliases:probe:{probe}"),
            )) {
                Err("observability_producer_closed") => break,
                Err(error) => panic!("unexpected producer state: {error}"),
                Ok(_) => {
                    probe += 1;
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .expect("concurrent last release reaches the producer");

    let retiring = registry.acquire_or_start(&database, &store_mount(&identity), || {
        panic!("retiring store must not start an overlapping producer")
    });
    assert!(matches!(
        retiring,
        Err(StoreObservabilityMountErrorV1::Retiring)
    ));
    blocker.commit().await.expect("release registered writer");
    tokio::time::timeout(Duration::from_secs(3), async {
        first_release
            .await
            .expect("first release task")
            .expect("first alias release");
        linked_release
            .await
            .expect("linked release task")
            .expect("linked alias release");
    })
    .await
    .expect("concurrent owner retirement completes");

    let replacement_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:retiring-two-aliases-replacement".to_owned(),
        ..identity.clone()
    };
    let replacement_mount = store_mount(&replacement_identity);
    let replacement = registry
        .acquire_or_start(&database, &replacement_mount, || {
            BoundedObservabilityProducerV1::start(database.clone(), replacement_identity, 1)
                .map_err(StoreObservabilityMountErrorV1::Unavailable)
        })
        .expect("one replacement after both aliases release");
    replacement.shutdown().await.expect("replacement shutdown");
}

#[tokio::test]
async fn adjacent_linked_alias_capacity_drops_retain_distinct_policy_carriers() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) =
        runtime("observability-linked-capacity-drops").await;
    let blocker = database
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let registry = StoreObservabilityRegistryV1::default();
    let policy_a = digest('a');
    let policy_b = digest('b');
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "daemon:linked-capacity-drops".to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: digest('c').as_str().to_owned(),
        policy_revision: policy_a.as_str().to_owned(),
    };
    let producer = BoundedObservabilityProducerV1::start(database.clone(), identity.clone(), 1)
        .expect("producer");
    let first = registry
        .acquire_or_start(&database, &store_mount(&identity), || Ok(producer))
        .expect("first alias");
    let linked_identity = ObservabilityProducerIdentityV1 {
        policy_revision: policy_b.as_str().to_owned(),
        ..identity.clone()
    };
    let linked = registry
        .acquire_or_start(&database, &store_mount(&linked_identity), || {
            Err(StoreObservabilityMountErrorV1::Unavailable(
                "must not start a second producer",
            ))
        })
        .expect("linked alias");
    let first_producer = first.producer();
    let linked_producer = linked.producer();

    let mut index = 0_u64;
    let first_missing_sequence = loop {
        let outcome = first_producer
            .try_emit(envelope(&project_id, &format!("capacity:alias-a:{index}")))
            .expect("bounded alias A emission");
        index = index.saturating_add(1);
        if outcome == ObservabilityEmissionOutcomeV1::DroppedAtCapacity {
            break index;
        }
        assert!(index < 1_024, "writer pressure must fill the bounded queue");
    };
    assert_eq!(
        linked_producer
            .try_emit(envelope(&project_id, "capacity:alias-b"))
            .expect("adjacent bounded alias B emission"),
        ObservabilityEmissionOutcomeV1::DroppedAtCapacity
    );

    blocker.commit().await.expect("release registered writer");
    first.shutdown().await.expect("release first alias");
    linked.shutdown().await.expect("drain last alias");

    let page = RegisteredObservabilityPortV1::new(&database)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query linked-root capacity drops");
    let dropped: BTreeMap<&str, (u64, u64, u64)> = page
        .events
        .iter()
        .filter_map(|event| {
            let ObservabilityPayloadV1::TelemetryDrop(drop) = &event.payload else {
                return None;
            };
            (drop.proved_drop_lower_bound > 0).then_some((
                event.policy_revision.as_str(),
                (
                    drop.first_missing_sequence,
                    drop.last_missing_sequence,
                    drop.proved_drop_lower_bound,
                ),
            ))
        })
        .collect();
    assert_eq!(
        dropped,
        BTreeMap::from([
            (
                policy_a.as_str(),
                (first_missing_sequence, first_missing_sequence, 1),
            ),
            (
                policy_b.as_str(),
                (first_missing_sequence + 1, first_missing_sequence + 1, 1),
            ),
        ]),
        "adjacent alias losses must not coalesce or inherit core A's policy"
    );
}

#[tokio::test]
async fn dropped_last_alias_keeps_the_store_retiring_until_owners_release() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) = runtime("observability-retiring-drop").await;
    let registry = StoreObservabilityRegistryV1::default();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "daemon:retiring-drop".to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: digest('6').as_str().to_owned(),
        policy_revision: digest('7').as_str().to_owned(),
    };
    let producer = BoundedObservabilityProducerV1::start(database.clone(), identity.clone(), 1)
        .expect("producer");
    let registered = registry
        .acquire_or_start(&database, &store_mount(&identity), || Ok(producer))
        .expect("registered observability producer");
    let blocker = database
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    registered
        .producer()
        .try_emit(envelope(&project_id, "drop:blocked"))
        .expect("enqueue blocked event");
    drop(registered);

    let overlap_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:retiring-drop-overlap".to_owned(),
        ..identity.clone()
    };
    let overlap_mount = store_mount(&overlap_identity);
    let retiring = registry.acquire_or_start(&database, &overlap_mount, || {
        BoundedObservabilityProducerV1::start(database.clone(), overlap_identity.clone(), 1)
            .map_err(StoreObservabilityMountErrorV1::Unavailable)
    });
    assert!(matches!(
        retiring,
        Err(StoreObservabilityMountErrorV1::Retiring)
    ));
    blocker.commit().await.expect("release registered writer");

    let replacement_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:retiring-drop-replacement".to_owned(),
        ..identity.clone()
    };
    let replacement_mount = store_mount(&replacement_identity);
    let replacement = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let attempt = registry.acquire_or_start(&database, &replacement_mount, || {
                BoundedObservabilityProducerV1::start(
                    database.clone(),
                    replacement_identity.clone(),
                    1,
                )
                .map_err(StoreObservabilityMountErrorV1::Unavailable)
            });
            match attempt {
                Ok(replacement) => break replacement,
                Err(StoreObservabilityMountErrorV1::Retiring) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected replacement result: {error}"),
            }
        }
    })
    .await
    .expect("dropped owner retirement completes");
    replacement.shutdown().await.expect("replacement shutdown");
}

#[tokio::test]
async fn runtimeless_last_alias_drop_keeps_the_store_retiring_until_the_drain_confirms() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) =
        runtime("observability-runtimeless-drop").await;
    let registry = StoreObservabilityRegistryV1::default();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "daemon:runtimeless-drop".to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: digest('6').as_str().to_owned(),
        policy_revision: digest('7').as_str().to_owned(),
    };
    let producer = BoundedObservabilityProducerV1::start(database.clone(), identity.clone(), 8)
        .expect("producer");
    let registered = registry
        .acquire_or_start(&database, &store_mount(&identity), || Ok(producer))
        .expect("registered observability producer");
    // Owners that record through the mounted producer retain frontends past
    // the alias handle's lifetime; this one keeps the shared core open.
    let retained = registered.producer();
    std::thread::spawn(move || drop(registered))
        .join()
        .expect("drop the last alias handle outside any tokio runtime");
    assert_eq!(
        retained
            .try_emit(envelope(&project_id, "runtimeless:retained"))
            .expect("retained frontend still emits through the open core"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );

    // The store entry must stay retiring: the drain never ran, so vacating
    // it would let a second producer start while the first still writes.
    let overlap_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:runtimeless-drop-overlap".to_owned(),
        ..identity.clone()
    };
    let overlap_mount = store_mount(&overlap_identity);
    let refused = registry.acquire_or_start(&database, &overlap_mount, || {
        panic!("a runtimeless drop must not vacate the store entry into a duplicate producer")
    });
    assert!(matches!(
        refused,
        Err(StoreObservabilityMountErrorV1::Retiring)
    ));

    // That refused mount ran on a live runtime, so the deferred drain is now
    // in flight; only its confirmed close releases the store entry.
    let replacement_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:runtimeless-drop-replacement".to_owned(),
        ..identity.clone()
    };
    let replacement_mount = store_mount(&replacement_identity);
    let replacement = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let attempt = registry.acquire_or_start(&database, &replacement_mount, || {
                BoundedObservabilityProducerV1::start(
                    database.clone(),
                    replacement_identity.clone(),
                    1,
                )
                .map_err(StoreObservabilityMountErrorV1::Unavailable)
            });
            match attempt {
                Ok(replacement) => break replacement,
                Err(StoreObservabilityMountErrorV1::Retiring) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected replacement result: {error}"),
            }
        }
    })
    .await
    .expect("deferred drain confirms and releases the store");
    assert_eq!(
        retained
            .try_emit(envelope(&project_id, "runtimeless:after-drain"))
            .expect_err("the confirmed drain closes the retained frontend"),
        "observability_producer_closed"
    );
    replacement.shutdown().await.expect("replacement shutdown");
}

#[tokio::test]
async fn registered_shutdown_reports_a_blocked_producer_flush() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, project_id, database, _runtime) =
        runtime("observability-shutdown-failure").await;
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "daemon:shutdown-failure".to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: digest('e').as_str().to_owned(),
        policy_revision: digest('f').as_str().to_owned(),
    };
    let producer = BoundedObservabilityProducerV1::start_with_deadlines(
        database.clone(),
        identity.clone(),
        1,
        ObservabilityProducerDeadlinesV1 {
            persistence: Duration::from_millis(50),
            shutdown: Duration::from_millis(250),
        },
    )
    .expect("producer");
    let registry = StoreObservabilityRegistryV1::default();
    let registered = registry
        .acquire_or_start(&database, &store_mount(&identity), || Ok(producer))
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
    let start_called = Arc::new(AtomicBool::new(false));
    let observed_start = Arc::clone(&start_called);
    let replacement_identity = ObservabilityProducerIdentityV1 {
        process_boot_id: "daemon:shutdown-failure-replacement".to_owned(),
        ..identity.clone()
    };
    let replacement_mount = store_mount(&replacement_identity);
    let failed = registry.acquire_or_start(&database, &replacement_mount, || {
        observed_start.store(true, Ordering::Release);
        BoundedObservabilityProducerV1::start(database.clone(), replacement_identity.clone(), 1)
            .map_err(StoreObservabilityMountErrorV1::Unavailable)
    });
    assert!(matches!(
        failed,
        Err(StoreObservabilityMountErrorV1::ShutdownFailed)
    ));
    assert!(!start_called.load(Ordering::Acquire));
}
