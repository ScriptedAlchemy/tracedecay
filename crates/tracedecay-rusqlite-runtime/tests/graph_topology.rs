use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkEvent, WorkEventKind, WorkProjection, WorkVersion, WorktreeId,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
    NeverCancelled,
};
use tracedecay_rusqlite_runtime::work::topology::{WorkGraphTopologyStore, WorkTopologyError};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: u8) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", format!("{byte:02x}").repeat(32))).unwrap()
}

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.graph-topology"),
        id::<RepositoryId>("repository.graph-topology"),
        id::<WorktreeId>("worktree.graph-topology"),
        id::<ActorId>("actor.graph-topology"),
        digest(1),
    )
    .unwrap()
}

fn projection(task: &str, dependencies: &[&str], version: u64) -> WorkProjection {
    let task_id = id::<TaskId>(task);
    let authority = authority();
    let mut events = vec![
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::initial(),
            authority.clone(),
            UtcMicros(1),
            id::<WorkCommandId>(format!("command.{task}.create").as_str()),
            digest(2),
            WorkEventKind::Created {
                title: task.to_owned(),
                dependencies: dependencies
                    .iter()
                    .map(|dependency| id::<TaskId>(dependency))
                    .collect(),
            },
        )
        .unwrap(),
    ];
    for revision in 2..=version {
        events.push(
            WorkEvent::new(
                task_id.clone(),
                WorkVersion::new(revision).unwrap(),
                authority.clone(),
                UtcMicros(i64::try_from(revision).unwrap()),
                id::<WorkCommandId>(format!("command.{task}.replan.{revision}").as_str()),
                digest(u8::try_from(revision + 2).unwrap()),
                WorkEventKind::DependenciesReplanned {
                    dependencies: dependencies
                        .iter()
                        .map(|dependency| id::<TaskId>(dependency))
                        .collect(),
                },
            )
            .unwrap(),
        );
    }
    WorkProjection::rebuild(&events).unwrap()
}

fn persistent_graph(path: &std::path::Path) -> GraphDb {
    GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
}

#[test]
fn work_publication_survives_reopen() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("work-topology.grafeo");
    {
        let topology = WorkGraphTopologyStore::new(persistent_graph(&path));
        for projection in [
            projection("task.a", &[], 1),
            projection("task.b", &["task.a"], 1),
            projection("task.c", &["task.a"], 1),
            projection("task.d", &["task.b", "task.c"], 1),
        ] {
            topology.publish(&projection).unwrap();
        }
        assert_eq!(
            topology.projection(&authority(), &id("task.d")).unwrap(),
            projection("task.d", &["task.b", "task.c"], 1)
        );
    }

    let reopened = WorkGraphTopologyStore::new(persistent_graph(&path));
    assert_eq!(
        reopened.projection(&authority(), &id("task.d")).unwrap(),
        projection("task.d", &["task.b", "task.c"], 1)
    );
}

#[test]
fn rejected_work_cycle_leaves_prior_topology_readable() {
    let topology = WorkGraphTopologyStore::memory().unwrap();
    topology.publish(&projection("task.a", &[], 1)).unwrap();
    topology
        .publish(&projection("task.b", &["task.a"], 1))
        .unwrap();

    assert_eq!(
        topology
            .publish(&projection("task.a", &["task.b"], 2))
            .unwrap_err(),
        WorkTopologyError::Cycle
    );
    assert_eq!(
        topology
            .projection(&authority(), &id("task.a"))
            .unwrap()
            .version(),
        WorkVersion::initial()
    );
    assert_eq!(
        topology
            .publish(&projection("task.a", &["task.b"], 2))
            .unwrap_err(),
        WorkTopologyError::Cycle
    );
}

#[test]
fn stale_work_projection_is_rejected_without_rolling_topology_back() {
    let topology = WorkGraphTopologyStore::memory().unwrap();
    topology.publish(&projection("task.a", &[], 1)).unwrap();
    topology
        .publish(&projection("task.b", &["task.a"], 2))
        .unwrap();

    assert_eq!(
        topology.publish(&projection("task.b", &[], 1)).unwrap_err(),
        WorkTopologyError::Stale
    );
    assert_eq!(
        topology
            .projection(&authority(), &id("task.b"))
            .unwrap()
            .version(),
        WorkVersion::new(2).unwrap()
    );
}

#[test]
fn foreign_graph_format_requires_explicit_reset() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("foreign-work-topology.grafeo");
    let foreign = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.clone()),
        expected_format: GraphFormatVersion::new(1).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
    foreign.close().unwrap();

    assert_eq!(
        WorkGraphTopologyStore::open(&path).unwrap_err(),
        WorkTopologyError::ResetRequired
    );
}

#[test]
fn one_bounded_batch_updates_one_thousand_of_one_hundred_thousand_tasks() {
    const PREEXISTING_TASKS: usize = 100_000;
    const CHANGED_TASKS: usize = 1_000;

    let temp = TempDir::new().unwrap();
    let topology =
        WorkGraphTopologyStore::new(persistent_graph(&temp.path().join("work-batch.grafeo")));
    let preexisting = (0..PREEXISTING_TASKS)
        .map(|index| projection(&format!("task.batch.{index:06}"), &[], 1))
        .collect::<Vec<_>>();
    topology.publish_batch(&preexisting).unwrap();
    drop(preexisting);

    for chunk_start in (0..10_000).step_by(CHANGED_TASKS) {
        let with_dependencies = (chunk_start..chunk_start + CHANGED_TASKS)
            .map(|index| projection(&format!("task.batch.{index:06}"), &["task.batch.099999"], 2))
            .collect::<Vec<_>>();
        topology.publish_batch(&with_dependencies).unwrap();
    }
    let changed = (0..CHANGED_TASKS)
        .map(|index| projection(&format!("task.batch.{index:06}"), &[], 3))
        .collect::<Vec<_>>();

    let started = Instant::now();
    topology.publish_batch(&changed).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        topology
            .projection(&authority(), &id::<TaskId>("task.batch.000000"))
            .unwrap()
            .version(),
        WorkVersion::new(3).unwrap()
    );
    assert_eq!(
        topology
            .projection(&authority(), &id::<TaskId>("task.batch.050000"))
            .unwrap()
            .version(),
        WorkVersion::initial()
    );
    assert_eq!(
        topology
            .projection(&authority(), &id::<TaskId>("task.batch.099999"))
            .unwrap()
            .version(),
        WorkVersion::initial()
    );
    eprintln!(
        "work graph timing: preexisting_tasks={PREEXISTING_TASKS} preexisting_relations=10000 changed_tasks={CHANGED_TASKS} delta_publication={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "bounded 1000-task graph delta exceeded 1s with 100000 preexisting tasks: {elapsed:?}"
    );

    let mut next_task = 20_000;
    for batch_size in [1_usize, 10, 100] {
        let mut samples = Vec::new();
        for _ in 0..20 {
            let batch = (next_task..next_task + batch_size)
                .map(|index| projection(&format!("task.batch.{index:06}"), &[], 2))
                .collect::<Vec<_>>();
            next_task += batch_size;
            let started = Instant::now();
            topology.publish_batch(&batch).unwrap();
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        let p50 = samples[(samples.len() - 1) * 50 / 100];
        let p95 = samples[(samples.len() - 1) * 95 / 100];
        eprintln!(
            "work graph timing: preexisting_tasks={PREEXISTING_TASKS} batch_size={batch_size} p50={p50:?} p95={p95:?}"
        );
        assert!(
            p95 < Duration::from_secs(1),
            "{batch_size}-task graph delta p95 exceeded 1s: {p95:?}"
        );
    }
}
