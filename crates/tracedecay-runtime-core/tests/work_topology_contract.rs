use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkEvent, WorkEventKind, WorkVersion, WorktreeId,
};
use tracedecay_graph_db::{
    GraphIdempotencyKey, GraphNamespace, GraphProjectorRevision, NeverCancelled,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::work_topology::{
    WORK_TOPOLOGY_PROJECTOR_REVISION_V1, WorkTopologyError, WorkTopologyProjectionV1,
    WorkTopologyStore, build_work_topology_manifest_checked, work_topology_idempotency_key,
    work_topology_namespace, work_topology_projection_identity,
};

fn digest(label: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", label.to_string().repeat(64))).expect("digest")
}

fn task(label: &str) -> TaskId {
    TaskId::new(format!("task.{label}")).expect("task")
}

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        ProjectId::new("project.work-topology").expect("project"),
        RepositoryId::new("repository.work-topology").expect("repository"),
        WorktreeId::new("worktree.work-topology").expect("worktree"),
        ActorId::new("actor.work-topology").expect("actor"),
        digest('a'),
    )
    .expect("authority")
}

fn event(task_id: TaskId, version: u64, sequence: u64, kind: WorkEventKind) -> WorkEvent {
    WorkEvent::new(
        task_id,
        WorkVersion::new(version).expect("version"),
        authority(),
        UtcMicros(i64::try_from(sequence).expect("timestamp")),
        WorkCommandId::new(format!("command.work-topology.{sequence}")).expect("command"),
        digest(char::from_digit(u32::try_from(sequence % 10).expect("digit"), 10).expect("label")),
        kind,
    )
    .expect("event")
}

fn events() -> Vec<WorkEvent> {
    vec![
        event(
            task("a"),
            1,
            1,
            WorkEventKind::Created {
                title: "A".to_owned(),
                dependencies: BTreeSet::new(),
            },
        ),
        event(task("a"), 2, 2, WorkEventKind::TaskAccepted),
        event(
            task("b"),
            1,
            3,
            WorkEventKind::Created {
                title: "B".to_owned(),
                dependencies: BTreeSet::from([task("a")]),
            },
        ),
        event(
            task("c"),
            1,
            4,
            WorkEventKind::Created {
                title: "C".to_owned(),
                dependencies: BTreeSet::from([task("b"), task("missing")]),
            },
        ),
    ]
}

fn projection() -> WorkTopologyProjectionV1 {
    WorkTopologyProjectionV1::from_events(&events()).expect("projection")
}

fn store() -> WorkTopologyStore {
    WorkTopologyStore::publish_from_events(&events(), &|| Ok(()), |manifest, key| {
        assert_eq!(
            key,
            GraphIdempotencyKey::new(format!("publish:{}", manifest.generation.as_str()))
                .expect("idempotency")
        );
        VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))
    })
    .expect("store")
}

#[test]
fn work_topology_reads_blockers_order_closure_and_critical_path() {
    let store = store();
    let cancellation = Arc::new(NeverCancelled);

    assert_eq!(
        store
            .blockers(&task("b"), cancellation.clone())
            .expect("B blockers"),
        BTreeSet::new()
    );
    assert_eq!(
        store
            .blockers(&task("c"), cancellation.clone())
            .expect("C blockers"),
        BTreeSet::from([task("b"), task("missing")])
    );
    assert_eq!(
        store
            .topological_order(cancellation.clone())
            .expect("topological order"),
        vec![task("a"), task("b"), task("c")]
    );
    assert_eq!(
        store
            .critical_path(&task("c"), cancellation.clone())
            .expect("critical path"),
        vec![task("a"), task("b"), task("c")]
    );
    let mut closure = store
        .dependency_closure(&task("c"), 8, 16, cancellation)
        .expect("closure");
    closure.sort();
    assert_eq!(closure, vec![task("a"), task("b"), task("missing")]);
}

#[test]
fn work_topology_rejects_cycles_and_rebuilds_byte_identically() {
    let cyclic = WorkTopologyProjectionV1::from_events(&[
        event(
            task("a"),
            1,
            1,
            WorkEventKind::Created {
                title: "A".to_owned(),
                dependencies: BTreeSet::from([task("b")]),
            },
        ),
        event(
            task("b"),
            1,
            2,
            WorkEventKind::Created {
                title: "B".to_owned(),
                dependencies: BTreeSet::from([task("a")]),
            },
        ),
    ]);
    assert!(matches!(cyclic, Err(WorkTopologyError::DependencyCycle(_))));

    let input = projection();
    let mut reversed_events = vec![
        event(
            task("c"),
            1,
            4,
            WorkEventKind::Created {
                title: "C".to_owned(),
                dependencies: BTreeSet::from([task("b"), task("missing")]),
            },
        ),
        event(
            task("b"),
            1,
            3,
            WorkEventKind::Created {
                title: "B".to_owned(),
                dependencies: BTreeSet::from([task("a")]),
            },
        ),
        event(task("a"), 2, 2, WorkEventKind::TaskAccepted),
        event(
            task("a"),
            1,
            1,
            WorkEventKind::Created {
                title: "A".to_owned(),
                dependencies: BTreeSet::new(),
            },
        ),
    ];
    let rebuilt = WorkTopologyProjectionV1::from_events(&reversed_events).expect("rebuilt");
    reversed_events.reverse();
    assert_eq!(input, rebuilt);

    let identity =
        work_topology_projection_identity(GraphNamespace::new("work-topology-test").expect("ns"))
            .expect("identity");
    let revision = GraphProjectorRevision::try_from(WORK_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision");
    let original =
        build_work_topology_manifest_checked(identity.clone(), &input, &revision, &|| Ok(()))
            .expect("original");
    let replayed = build_work_topology_manifest_checked(identity, &rebuilt, &revision, &|| Ok(()))
        .expect("replayed");
    assert_eq!(original.generation, replayed.generation);
    assert_eq!(
        original
            .expected_recovered_digest(&|| Ok(()))
            .expect("original digest"),
        replayed
            .expected_recovered_digest(&|| Ok(()))
            .expect("replayed digest")
    );
}

#[test]
fn work_topology_publication_identity_is_content_addressed() {
    let projection = projection();
    let revision = GraphProjectorRevision::try_from(WORK_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .expect("revision");
    let namespace = work_topology_namespace(&authority()).expect("namespace");
    let identity =
        work_topology_projection_identity(namespace.clone()).expect("projection identity");
    let manifest =
        build_work_topology_manifest_checked(identity, &projection, &revision, &|| Ok(()))
            .expect("manifest");

    assert_eq!(
        namespace,
        work_topology_namespace(&authority()).expect("replayed namespace")
    );
    assert_eq!(
        work_topology_idempotency_key(&projection, &revision).expect("idempotency"),
        GraphIdempotencyKey::new(format!("publish:{}", manifest.generation.as_str()))
            .expect("expected idempotency")
    );
}

#[test]
fn work_topology_evidence_ref_is_minted_from_the_verified_graph_authority() {
    let first = store();
    let replayed = store();

    let first_ref = first.evidence_ref().expect("topology evidence ref");
    assert_eq!(
        first_ref,
        replayed.evidence_ref().expect("replayed evidence ref")
    );
    assert!(first_ref.as_str().starts_with("sha256:"));
    assert_ne!(first_ref.as_str(), first.generation().as_str());
}
