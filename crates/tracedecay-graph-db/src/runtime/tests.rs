use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use grafeo_common::types::{PropertyKey, Value};

use super::{
    VectorRefreshUpdate, refresh_vector_indexes, require_committed_vector_scalar, sync_wal,
    vector_property_key,
};
use crate::recovery::set_projection_quarantine;
use crate::{
    GraphCommit, GraphDbError, GraphDbLeaseV1, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner,
    GraphDbRuntimeState, GraphDurability, GraphEntity, GraphEntityId, GraphFormatVersion,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphVector,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration, TraversalRequest,
    VectorMetric, VectorSearchRequest, mutation,
};

fn memory_db() -> GraphDbLeaseV1 {
    GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
    .issue_lease()
    .unwrap()
}

#[test]
fn runtime_state_distinguishes_ready_closed_and_durability_uncertain() {
    let ready = memory_db();
    assert_eq!(ready.runtime_state(), GraphDbRuntimeState::Ready);
    ready.close().unwrap();
    assert_eq!(ready.runtime_state(), GraphDbRuntimeState::Closed);

    let uncertain = memory_db();
    uncertain.inner.poisoned.store(true, Ordering::Release);
    assert_eq!(
        uncertain.runtime_state(),
        GraphDbRuntimeState::DurabilityUncertain
    );
}

#[test]
fn owner_close_releases_the_physical_database_after_durability_uncertainty() {
    let owner = GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
    let handle = owner.issue_lease().unwrap();
    handle.inner.poisoned.store(true, Ordering::Release);

    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
    assert!(
        handle
            .inner
            .database
            .read()
            .expect("database lock remains readable")
            .is_none(),
        "owner close must release the physical Grafeo database even after poisoning"
    );
}

#[test]
fn poisoned_database_lock_makes_close_terminally_uncertain() {
    let owner = GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
    let lease = owner.issue_lease().unwrap();
    let poison = lease.clone();

    assert!(
        std::thread::spawn(move || {
            let _guard = poison.inner.database.write().unwrap();
            panic!("poison the graph database lock");
        })
        .join()
        .is_err()
    );

    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
    assert_eq!(
        owner.runtime_state(),
        GraphDbRuntimeState::DurabilityUncertain
    );
    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

fn scalar_batch(value: &str) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new(value).unwrap(),
        GraphWatermark::new(value).unwrap(),
        vec![GraphMutation::UpsertEntity(
            GraphEntity::new(
                GraphEntityId::new("a").unwrap(),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("name").unwrap(),
                    GraphProperty::String(value.to_owned()),
                )]),
            )
            .unwrap(),
        )],
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

fn stable_replay_batch(source: &str) -> GraphWriteBatch {
    let entity = |identity: &str| {
        GraphEntity::new(
            GraphEntityId::new(identity).unwrap(),
            BTreeSet::new(),
            BTreeMap::from([(
                GraphPropertyName::new("name").unwrap(),
                GraphProperty::String(identity.to_owned()),
            )]),
        )
        .unwrap()
    };
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new(source).unwrap(),
        GraphWatermark::new(source).unwrap(),
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(
                GraphRelation::new(
                    GraphRelationId::new("a-calls-b").unwrap(),
                    GraphEntityId::new("a").unwrap(),
                    GraphEntityId::new("b").unwrap(),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::new(),
                )
                .unwrap(),
            ),
        ],
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

fn stable_replay_native_identity(
    db: &GraphDbLeaseV1,
    batch: &GraphWriteBatch,
) -> (
    Vec<grafeo_common::types::NodeId>,
    grafeo_common::types::NodeId,
    grafeo_common::types::EdgeId,
) {
    let guard = db.read_guard().unwrap();
    let database = guard.as_ref().unwrap();
    let existing = crate::state::ExistingBatchState::load(database, batch).unwrap();
    let entities = existing
        .entities
        .values()
        .map(|entity| entity.node)
        .collect::<Vec<_>>();
    let relation = existing.relations.values().next().unwrap();
    (entities, relation.locator, relation.edge)
}

#[test]
fn identical_upsert_replay_preserves_native_entity_and_relation_identity() {
    let db = memory_db();
    db.apply_unverified(stable_replay_batch("generation-1"))
        .unwrap();
    let before = stable_replay_native_identity(&db, &stable_replay_batch("generation-2"));

    db.apply_unverified(stable_replay_batch("generation-2"))
        .unwrap();
    let after = stable_replay_native_identity(&db, &stable_replay_batch("generation-3"));

    assert_eq!(
        after, before,
        "identical graph rows must be mutation no-ops"
    );
}

#[test]
fn queued_reader_rechecks_postcommit_poison_after_database_lock() {
    let db = memory_db();
    let database_guard = db.inner.database.write().unwrap();
    let started = Arc::new(Barrier::new(2));
    let reader_db = db.clone();
    let reader_started = Arc::clone(&started);
    let reader = std::thread::spawn(move || {
        reader_started.wait();
        reader_db.traverse(TraversalRequest {
            namespace: GraphNamespace::new("workspace").unwrap(),
            start: GraphEntityId::new("missing").unwrap(),
            relation_kinds: BTreeSet::new(),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 1,
            max_visits: 1,
            max_results: 1,
            cancellation: Arc::new(NeverCancelled),
        })
    });
    started.wait();
    std::thread::sleep(Duration::from_millis(25));
    assert!(
        !reader.is_finished(),
        "reader must be queued behind the writer"
    );

    db.inner.poisoned.store(true, Ordering::Release);
    drop(database_guard);

    assert!(matches!(
        reader.join().unwrap(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

#[test]
fn snapshot_is_a_zero_copy_read_lease_that_blocks_writes_until_drop() {
    let db = memory_db();
    db.apply_unverified(scalar_batch("before")).unwrap();
    let snapshot = db.snapshot().unwrap();
    let writer_db = db.clone();
    let (sent, received) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = writer_db.apply_unverified(scalar_batch("after"));
        sent.send(result).unwrap();
    });

    assert!(
        received.recv_timeout(Duration::from_millis(50)).is_err(),
        "a live snapshot must retain its read lease instead of copying the whole store"
    );
    assert_eq!(
        snapshot
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("a").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap()
            .unwrap()
            .properties
            .get(&GraphPropertyName::new("name").unwrap()),
        Some(&GraphProperty::String("before".to_owned()))
    );

    drop(snapshot);
    received
        .recv_timeout(Duration::from_secs(1))
        .expect("writer must resume when the snapshot lease is released")
        .unwrap();
    writer.join().unwrap();
}

#[test]
fn open_installs_native_locator_indexes_and_entity_scalars() {
    let db = memory_db();
    db.apply_unverified(scalar_batch("native")).unwrap();
    let database_guard = db.inner.database.read().unwrap();
    let database = database_guard.as_ref().unwrap();
    for property in [
        "__tracedecay_graph_db_entity_key",
        "__tracedecay_graph_db_relation_key",
        "__tracedecay_graph_db_projection_key",
        "__tracedecay_graph_db_publication_key",
    ] {
        assert!(
            database.has_property_index(property),
            "missing native property index {property}"
        );
    }

    let nodes = database.find_nodes_by_property(
        "__tracedecay_graph_db_entity_key",
        &Value::from("70726f6a656374:61"),
    );
    assert_eq!(nodes.len(), 1);
    let node = database.get_node(nodes[0]).unwrap();
    assert_eq!(
        node.get_property("__tracedecay_graph_db_namespace"),
        Some(&Value::from("project"))
    );
    assert_eq!(
        node.get_property("__tracedecay_graph_db_projection"),
        Some(&Value::from("code"))
    );
    assert_eq!(
        node.get_property("__tracedecay_graph_db_entity_id"),
        Some(&Value::from("a"))
    );
    assert!(
        node.get_property("__tracedecay_graph_db_payload").is_none(),
        "typed graph state must not be hidden in a JSON payload"
    );
}

#[test]
fn grafeo_query_mutations_track_conflicting_marker_writes() {
    let database = grafeo_engine::GrafeoDB::new_in_memory();
    database.create_property_index("marker_key");
    database
        .session()
        .create_node_with_props(
            &["Marker"],
            [
                ("marker_key", Value::from("one")),
                ("value", Value::from(0_i64)),
            ],
        )
        .unwrap();
    let mut first = database.session();
    let mut second = database.session();
    first.begin_transaction().unwrap();
    second.begin_transaction().unwrap();
    first
        .execute("MATCH (n:Marker {marker_key: 'one'}) SET n.value = 1")
        .unwrap();
    assert!(
        second
            .execute("MATCH (n:Marker {marker_key: 'one'}) SET n.value = 2")
            .is_err(),
        "the second tracked writer must conflict before changing the marker"
    );
    first.rollback().unwrap();
    second.rollback().unwrap();
}

#[test]
fn vector_index_refresh_requires_the_identical_committed_scalar() {
    let database = grafeo_engine::GrafeoDB::new_in_memory();
    let expected = Value::Vector(vec![1.0_f32, 2.0].into());
    let node = database
        .session()
        .create_node_with_props(&["Vector"], [("embedding", expected.clone())])
        .unwrap();
    let committed = database
        .graph_store()
        .get_node_property(node, &PropertyKey::new("embedding"));

    assert_eq!(
        require_committed_vector_scalar(committed.as_ref(), "embedding", &expected),
        Ok(())
    );
    assert!(matches!(
        require_committed_vector_scalar(
            committed.as_ref(),
            "embedding",
            &Value::Vector(vec![2.0_f32, 1.0].into()),
        ),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
    assert!(matches!(
        require_committed_vector_scalar(None, "embedding", &expected),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

fn refresh_update(identity: &str, values: Vec<f32>) -> VectorRefreshUpdate {
    VectorRefreshUpdate {
        identity: GraphEntityId::new(identity).unwrap(),
        property: PropertyKey::new(vector_property_key(
            &GraphPropertyName::new("embedding").unwrap(),
            2,
            VectorMetric::Cosine,
        )),
        value: Value::Vector(values.into()),
    }
}

/// The batched refresh must fail closed on exactly the rows the per-row path
/// did: an identity the index no longer resolves, and a committed scalar that
/// is not the one about to be re-applied. A healthy row still refreshes.
#[test]
fn vector_index_refresh_fails_closed_on_missing_and_differing_committed_rows() {
    let db = memory_db();
    db.apply_unverified(vector_batch("committed")).unwrap();
    let guard = db.write_guard().unwrap();
    let database = guard.as_ref().unwrap();
    let namespace = GraphNamespace::new("project").unwrap();

    assert_eq!(
        refresh_vector_indexes(
            database,
            &namespace,
            vec![refresh_update("vector-entity", vec![1.0, 2.0])],
        ),
        Ok(())
    );

    let differing = refresh_vector_indexes(
        database,
        &namespace,
        vec![
            refresh_update("vector-entity", vec![1.0, 2.0]),
            refresh_update("vector-entity", vec![2.0, 1.0]),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(
            &differing,
            GraphDbError::DurabilityUncertain { message }
                if message.contains("differs before native index refresh")
        ),
        "a differing committed scalar must be durability-uncertain: {differing:?}"
    );

    let missing = refresh_vector_indexes(
        database,
        &namespace,
        vec![
            refresh_update("vector-entity", vec![1.0, 2.0]),
            refresh_update("never-committed", vec![1.0, 2.0]),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(
            &missing,
            GraphDbError::DurabilityUncertain { message }
                if message.contains("`never-committed` is missing from native identity index")
        ),
        "an unresolvable identity must be durability-uncertain: {missing:?}"
    );
}

/// The refresh learns each committed row's node id, projection, and vector
/// scalar from the identity index plus one projected column read; it must not
/// hydrate the committed entities (labels, every property, and the vector
/// decoded into a `GraphEntity`) to get there.
#[test]
fn vector_index_refresh_does_not_hydrate_committed_entities() {
    let db = memory_db();
    db.apply_unverified(vector_batch("seed")).unwrap();
    let page = GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new("page").unwrap(),
        GraphWatermark::new("page").unwrap(),
        (0..64)
            .map(|index| {
                GraphMutation::UpsertEntity(
                    GraphEntity::new(
                        GraphEntityId::new(format!("chunk:{index}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::from([(
                            GraphPropertyName::new("embedding").unwrap(),
                            GraphProperty::Vector(
                                GraphVector::new(
                                    vec![index as f32, (64 - index) as f32],
                                    2,
                                    VectorMetric::Cosine,
                                )
                                .unwrap(),
                            ),
                        )]),
                    )
                    .unwrap(),
                )
            })
            .collect(),
        Arc::new(NeverCancelled),
    )
    .unwrap();

    let _ = crate::hotpath_observe::take_traversal_counters();
    db.apply_unverified(page).unwrap();
    let counters = crate::hotpath_observe::take_traversal_counters();

    assert_eq!(
        counters.property_decodes, 0,
        "committing a page of new vector rows must not decode any stored entity"
    );
    assert_eq!(
        db.vector_search(VectorSearchRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code").unwrap(),
            property: GraphPropertyName::new("embedding").unwrap(),
            query: vec![63.0, 1.0],
            dimension: 2,
            metric: VectorMetric::Cosine,
            limit: 1,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap()
        .matches
        .into_iter()
        .map(|candidate| candidate.entity)
        .collect::<Vec<_>>(),
        vec![GraphEntityId::new("chunk:63").unwrap()],
        "the refreshed index must serve the committed page"
    );
}

#[test]
fn snapshots_can_cross_daemon_worker_boundaries() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<super::GraphSnapshot>();
}

const SQLITE_UNSAFE_FAST_ENV: &str = "TRACEDECAY_SQLITE_UNSAFE_FAST";
const GRAPH_CRASH_CHILD_ROOT_ENV: &str = "TRACEDECAY_GRAPH_CRASH_CHILD_ROOT";
const GRAPH_CRASH_CHILD_READY: &str = "durable-phase.ready";

fn walsync_db(dir: &tempfile::TempDir) -> GraphDbLeaseV1 {
    walsync_db_at(dir.path())
}

fn walsync_db_at(root: &Path) -> GraphDbLeaseV1 {
    GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(root.join("graph.grafeo")),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::WalSync,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
    .issue_lease()
    .unwrap()
}

fn refuse_sqlite_unsafe_fast() {
    unsafe {
        std::env::remove_var(SQLITE_UNSAFE_FAST_ENV);
    }
    assert!(
        std::env::var_os(SQLITE_UNSAFE_FAST_ENV).is_none(),
        "WalSync crash fixtures must not credit {SQLITE_UNSAFE_FAST_ENV}"
    );
}

fn capture_unclean_walsync_image(destination: &Path) {
    let source = tempfile::tempdir().unwrap();
    let test_name = std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .expect("crash child spawn needs the libtest/nextest test name");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test_name, "--nocapture"])
        .env(GRAPH_CRASH_CHILD_ROOT_ENV, source.path())
        .env_remove(SQLITE_UNSAFE_FAST_ENV)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "crash child {test_name} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        source.path().join(GRAPH_CRASH_CHILD_READY).is_file(),
        "crash child must reach the durable phase before exiting"
    );
    let source_store = source.path().join("graph.grafeo");
    let target = destination.join("graph.grafeo");
    std::fs::copy(&source_store, &target).unwrap_or_else(|error| {
        panic!(
            "copy abandoned crash container {} -> {}: {error}",
            source_store.display(),
            target.display()
        );
    });
    copy_directory(&sidecar_wal_path(&source_store), &sidecar_wal_path(&target));
}

fn vector_batch(value: &str) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new(value).unwrap(),
        GraphWatermark::new(value).unwrap(),
        vec![GraphMutation::UpsertEntity(
            GraphEntity::new(
                GraphEntityId::new("vector-entity").unwrap(),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("embedding").unwrap(),
                    GraphProperty::Vector(
                        GraphVector::new(vec![1.0, 2.0], 2, VectorMetric::Cosine).unwrap(),
                    ),
                )]),
            )
            .unwrap(),
        )],
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

fn apply_vector_batch_with_check(
    db: &GraphDbLeaseV1,
    value: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphCommit, GraphDbError> {
    let mut batch = vector_batch(value);
    let digest = batch.validate_and_digest().unwrap();
    let _snapshot_gate = db.inner.snapshot_gate.write();
    let guard = db.write_guard().unwrap();
    let database = guard.as_ref().unwrap();
    let mut state = db.state_write_guard().unwrap();
    let state = state.as_mut().unwrap();
    db.apply_locked(
        database,
        state,
        batch,
        mutation::CommitMetadata::for_digest(digest),
        &mutation::RelationEndpointNamespaces::new(),
        check,
    )
}

fn committed_entity_present(db: &GraphDbLeaseV1) -> bool {
    db.snapshot()
        .unwrap()
        .entity(
            &GraphNamespace::new("project").unwrap(),
            &GraphEntityId::new("vector-entity").unwrap(),
            Arc::new(NeverCancelled),
        )
        .unwrap()
        .is_some()
}

fn sidecar_wal_path(store: &std::path::Path) -> std::path::PathBuf {
    let mut sidecar = store.as_os_str().to_owned();
    sidecar.push(".wal");
    std::path::PathBuf::from(sidecar)
}

fn copy_directory(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target_path = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target_path);
        } else {
            std::fs::copy(entry.path(), &target_path).unwrap();
        }
    }
}

fn entity_marker(db: &GraphDbLeaseV1) -> Option<String> {
    db.snapshot()
        .unwrap()
        .entity(
            &GraphNamespace::new("project").unwrap(),
            &GraphEntityId::new("a").unwrap(),
            Arc::new(NeverCancelled),
        )
        .unwrap()
        .map(|entity| {
            match entity
                .properties
                .get(&GraphPropertyName::new("name").unwrap())
            {
                Some(GraphProperty::String(value)) => value.clone(),
                other => panic!("committed entity lost its scalar marker: {other:?}"),
            }
        })
}

/// A clean close must checkpoint the WAL sidecar away: the next open then has
/// no journal to replay and hydrates from the checkpointed sections alone. A
/// surviving sidecar would make every reopen replay session history that the
/// checkpoint already made durable.
#[test]
fn clean_shutdown_checkpoints_the_wal_so_reopen_has_no_journal_to_replay() {
    let dir = tempfile::tempdir().unwrap();
    let sidecar = sidecar_wal_path(&dir.path().join("graph.grafeo"));

    let db = walsync_db(&dir);
    db.apply_unverified(scalar_batch("checkpointed")).unwrap();
    assert!(
        sidecar.is_dir(),
        "a WalSync store must journal into its sidecar before checkpoint"
    );
    db.close().unwrap();
    assert!(
        !sidecar.exists(),
        "clean close left a WAL journal behind; the next open would replay \
         work the checkpoint already persisted"
    );

    let reopened = walsync_db(&dir);
    assert_eq!(entity_marker(&reopened), Some("checkpointed".to_owned()));
    reopened.close().unwrap();
}

/// A hard stop leaves the container without its latest sections and only the
/// synced WAL sidecar beside it. Opening that on-disk shape must replay the
/// journal and serve the committed write; dropping it would silently lose a
/// commit `WalSync` already acknowledged.
#[test]
fn reopen_of_a_dirty_store_copy_replays_the_walled_commit() {
    refuse_sqlite_unsafe_fast();
    if let Some(root) = std::env::var_os(GRAPH_CRASH_CHILD_ROOT_ENV) {
        let live = walsync_db_at(Path::new(&root));
        assert_eq!(live.inner.durability, GraphDurability::WalSync);
        live.apply_unverified(scalar_batch("unclean")).unwrap();
        std::fs::write(
            Path::new(&root).join(GRAPH_CRASH_CHILD_READY),
            b"wal-synced",
        )
        .unwrap();
        std::mem::forget(live);
        std::process::exit(0);
    }

    // A child writes the WalSync commit, exits without a clean close, and only
    // then is the abandoned image copied. Copying the live parent handle is
    // illegal on Windows (lock violation 33; issue #933).
    let crash_dir = tempfile::tempdir().unwrap();
    capture_unclean_walsync_image(crash_dir.path());
    let target = crash_dir.path().join("graph.grafeo");
    assert!(
        sidecar_wal_path(&target).is_dir(),
        "a WalSync store must journal into its sidecar before checkpoint"
    );

    let recovered = walsync_db(&crash_dir);
    assert_eq!(entity_marker(&recovered), Some("unclean".to_owned()));
    recovered.close().unwrap();
}

#[cfg(windows)]
#[test]
fn live_walsync_store_refuses_raw_byte_copy() {
    refuse_sqlite_unsafe_fast();
    let dir = tempfile::tempdir().unwrap();
    let live = walsync_db(&dir);
    live.apply_unverified(scalar_batch("live")).unwrap();
    let source = dir.path().join("graph.grafeo");
    let dest = dir.path().join("illegal-copy.grafeo");
    let error = std::fs::copy(&source, &dest).expect_err("copying a live Windows store must fail");
    assert!(
        matches!(error.raw_os_error(), Some(32 | 33)),
        "expected sharing/lock violation, got {error}"
    );
    live.close().unwrap();
}

/// Recovery persists a projection quarantine before checkpointing; an open of
/// that dirty state must load the marker and fail closed on reads until the
/// quarantine is explicitly cleared and checkpointed away.
#[test]
fn persisted_quarantine_survives_checkpointed_reopen_and_blocks_reads_until_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = GraphNamespace::new("project").unwrap();
    let projection = GraphProjectionId::new("code").unwrap();

    let db = walsync_db(&dir);
    db.apply_unverified(scalar_batch("guarded")).unwrap();
    {
        let guard = db.read_guard().unwrap();
        let database = guard.as_ref().unwrap();
        set_projection_quarantine(database, &namespace, &projection, true).unwrap();
        sync_wal(database).unwrap();
    }
    db.close().unwrap();

    let reopened = walsync_db(&dir);
    assert!(
        matches!(
            reopened.ensure_projection_readable(&namespace, &projection),
            Err(GraphDbError::ProjectionMismatch { .. })
        ),
        "a reopened store must fail closed on a persisted quarantine"
    );
    {
        let guard = reopened.read_guard().unwrap();
        let database = guard.as_ref().unwrap();
        set_projection_quarantine(database, &namespace, &projection, false).unwrap();
        sync_wal(database).unwrap();
    }
    reopened.close().unwrap();

    let cleared = walsync_db(&dir);
    assert_eq!(
        cleared.ensure_projection_readable(&namespace, &projection),
        Ok(())
    );
    cleared.close().unwrap();
}

/// A threaded deadline may expire at any observation point in the write path.
/// Sweeping every expiry point proves the durability contract: cancellation
/// may only surface while the Grafeo transaction is still uncommitted (rolled
/// back, nothing durable); once the transaction commits, the apply must settle
/// HNSW refresh and WAL sync and report the commit. A committed write reported
/// as `Cancelled`/`DeadlineExceeded` on a `Ready` handle is the F-class defect:
/// replay short-circuits on the committed publication record and the skipped
/// settlement is never repaired.
#[test]
fn deadline_expiry_after_commit_settles_the_write_instead_of_cancelling() {
    // Calibration: count every cancellation observation one full apply makes.
    let total_checks = {
        let dir = tempfile::tempdir().unwrap();
        let db = walsync_db(&dir);
        let observations = AtomicUsize::new(0);
        apply_vector_batch_with_check(&db, "calibrate", &|| {
            observations.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .expect("unrestricted apply must commit");
        observations.into_inner()
    };
    assert!(total_checks > 0, "write path must observe cancellation");

    // Sweep: expire the deadline at the k-th observation on a fresh store.
    for expiry_point in 1..=total_checks + 1 {
        let dir = tempfile::tempdir().unwrap();
        let db = walsync_db(&dir);
        let observations = AtomicUsize::new(0);
        let result = apply_vector_batch_with_check(&db, "window", &|| {
            if observations.fetch_add(1, Ordering::Relaxed) + 1 >= expiry_point {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        });

        let committed = committed_entity_present(&db);
        match result {
            Ok(_) => {
                assert!(
                    committed,
                    "expiry point {expiry_point}: reported commit is not readable"
                );
                assert_eq!(
                    db.runtime_state(),
                    GraphDbRuntimeState::Ready,
                    "expiry point {expiry_point}: settled commit left a non-ready handle"
                );
            }
            Err(GraphDbError::DeadlineExceeded | GraphDbError::Cancelled) => {
                assert!(
                    !committed,
                    "expiry point {expiry_point}: durably committed write was mistyped as \
                     cancelled, stranding unsynced WAL and stale HNSW state on a Ready handle"
                );
                assert_eq!(
                    db.runtime_state(),
                    GraphDbRuntimeState::Ready,
                    "expiry point {expiry_point}: pre-commit cancellation must roll back cleanly"
                );
            }
            Err(other) => {
                panic!("expiry point {expiry_point}: unexpected write outcome {other:?}")
            }
        }
    }
}
