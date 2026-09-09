use std::collections::{BTreeMap, BTreeSet};

use tracedecay_contracts::{
    AcceptProposalCommand, AdmitExecutionCommand, CancellationContext, CapabilityGrantSnapshot,
    CreateWorkCommand, Deadline, DisclosureClass, ReplanDependenciesCommand, RequestContext,
    RequestId, ResolvedScope, ReviewProposalCommand, WorkProjectionPortError,
    WorkProjectionReadPort, WorkService, WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkEvent, WorkEventKind, WorkProjection, WorkProjectionDeltaV1,
    WorkProjectionResumeCursorV1, WorkVersion, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::work_registered_store;

use work_registered_store::RegisteredWorkStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(project: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.work.storage"),
        id::<WorktreeId>("worktree.work.storage"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.storage"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.storage").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.storage").unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{project}.{actor}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.{actor}")).unwrap(),
    )
    .unwrap()
}

fn authority(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap()
}

fn create(service: &WorkService<WorkSqliteStorage>, context: &RequestContext, task_id: &str) {
    service
        .create(
            context,
            CreateWorkCommand {
                task_id: id(task_id),
                title: format!("Persist {task_id}"),
                dependencies: BTreeSet::new(),
                command_id: id::<WorkCommandId>(&format!("command.create.{task_id}")),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
}

fn accept(
    service: &WorkService<WorkSqliteStorage>,
    context: &RequestContext,
    task_id: &str,
    occurred_at: i64,
) -> WorkProjection {
    service
        .accept_proposal(
            context,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: id(task_id),
                    proposal_id: id(&format!("proposal.{task_id}")),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id(&format!("command.accept-proposal.{task_id}")),
                    occurred_at: UtcMicros(occurred_at),
                },
            },
        )
        .unwrap()
}

fn admit(
    service: &WorkService<WorkSqliteStorage>,
    context: &RequestContext,
    task_id: &str,
    occurred_at: i64,
) -> WorkProjection {
    service
        .admit_execution(
            context,
            AdmitExecutionCommand {
                task_id: id(task_id),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id(&format!("command.admit-execution.{task_id}")),
                occurred_at: UtcMicros(occurred_at),
            },
        )
        .unwrap()
}

fn by_task(projections: &[WorkProjection]) -> BTreeMap<TaskId, WorkProjection> {
    projections
        .iter()
        .map(|projection| (projection.task_id().clone(), projection.clone()))
        .collect()
}

/// Folds one delta page into a follower's task map and records what it
/// delivered as `(task, version)` pairs.
fn apply_delta(
    state: &mut BTreeMap<TaskId, WorkProjection>,
    delivered: &mut Vec<(TaskId, u64)>,
    delta: &WorkProjectionDeltaV1,
) {
    for projection in delta.changed() {
        delivered.push((projection.task_id().clone(), projection.version().get()));
        state.insert(projection.task_id().clone(), projection.clone());
    }
}

/// The projection delta cursor must follow the order events were appended,
/// not the `task_id, version` order the journal happens to sort by. With a
/// task-sorted offset, appending to a task that sorts before the saved
/// position shifts later rows past the cursor: the resumed walk skips the
/// new event and replays an unchanged one. This interleaves appends to two
/// tasks across a one-task page boundary, a restart, and an append between
/// pages, and asserts the pages together deliver every appended event exactly
/// once and converge on the same state a fresh snapshot reports.
#[test]
fn delta_resumes_by_append_order_across_interleaved_task_appends() {
    let mut store = RegisteredWorkStore::start("append-order");
    let owner = context("project.work.append-order", "actor.work.owner");
    let owner_authority = authority(&owner);
    let task_a = id::<TaskId>("task.work.append-order.a");
    let task_b = id::<TaskId>("task.work.append-order.b");
    let page_size = 1;

    let service = WorkService::new(store.storage().clone());
    create(&service, &owner, task_a.as_str());
    create(&service, &owner, task_b.as_str());
    let snapshot = WorkProjectionReadPort::snapshot(store.storage(), &owner_authority, 10).unwrap();
    assert!(snapshot.coverage().resume_cursor().is_none());
    let mut state = by_task(snapshot.projections());
    let mut delivered = Vec::new();

    // Both tasks advance after the snapshot, `a` first. `a` sorts before the
    // saved position, which is exactly the append a task-sorted offset loses.
    accept(&service, &owner, task_a.as_str(), 20);
    accept(&service, &owner, task_b.as_str(), 21);

    let resume = WorkSqliteStorage::resume_cursor(&snapshot).unwrap();
    let first =
        WorkProjectionReadPort::delta(store.storage(), &owner_authority, &resume, page_size)
            .unwrap();
    first.validate_after(&snapshot).unwrap();
    assert_eq!(
        first.coverage().total(),
        2,
        "both tasks changed after the snapshot"
    );
    assert_eq!(
        first
            .changed()
            .iter()
            .map(|projection| (projection.task_id().clone(), projection.version().get()))
            .collect::<Vec<_>>(),
        vec![(task_a.clone(), 2)],
        "the first page must carry the first appended change, not the lexically last task"
    );
    apply_delta(&mut state, &mut delivered, &first);
    let mut cursor = first.coverage().resume_cursor().cloned();

    // The cursor is a durable append position, so a restart cannot lose it,
    // and an append between pages lands in a later page.
    drop(service);
    store = store.restart("append-order");
    let service = WorkService::new(store.storage().clone());
    admit(&service, &owner, task_a.as_str(), 30);

    let mut pages = 0;
    while let Some(resume) = cursor {
        pages += 1;
        assert!(pages <= 4, "the walk must terminate");
        let delta =
            WorkProjectionReadPort::delta(store.storage(), &owner_authority, &resume, page_size)
                .unwrap();
        apply_delta(&mut state, &mut delivered, &delta);
        cursor = delta.coverage().resume_cursor().cloned();
    }

    let fresh = WorkProjectionReadPort::snapshot(store.storage(), &owner_authority, 10).unwrap();
    assert_eq!(
        state,
        by_task(fresh.projections()),
        "following every delta page must converge on the fresh snapshot"
    );
    assert_eq!(
        delivered,
        vec![
            (task_a.clone(), 2),
            (task_b.clone(), 2),
            (task_a.clone(), 3)
        ],
        "each appended event must be delivered exactly once, in append order"
    );

    // An exact command replay commits nothing, so the frontier does not move
    // and the head cursor stays at the head.
    accept(&service, &owner, task_b.as_str(), 21);
    assert_eq!(
        store
            .inspect(|connection| WorkSqliteStorage::owner_cursor(connection, &owner_authority))
            .unwrap(),
        5
    );
    let head = WorkSqliteStorage::resume_cursor(&fresh).unwrap();
    assert_eq!(
        WorkProjectionReadPort::delta(store.storage(), &owner_authority, &head, page_size)
            .unwrap_err(),
        WorkProjectionPortError::StaleCursor
    );

    // A cursor minted for another authority, an unparseable token, and a
    // position past the frontier all refuse rather than guess.
    let foreign = context("project.work.append-order.other", "actor.work.owner");
    let foreign_snapshot =
        WorkProjectionReadPort::snapshot(store.storage(), &authority(&foreign), 10).unwrap();
    for cursor in [
        WorkSqliteStorage::resume_cursor(&foreign_snapshot).unwrap(),
        WorkProjectionResumeCursorV1::new(
            fresh.generation_id().clone(),
            "work-projection-sequence.v1:1",
        )
        .unwrap(),
        WorkProjectionResumeCursorV1::new(fresh.generation_id().clone(), "not-a-cursor").unwrap(),
        WorkProjectionResumeCursorV1::new(
            fresh.generation_id().clone(),
            "work-projection-append-sequence.v1:99",
        )
        .unwrap(),
    ] {
        assert_eq!(
            WorkProjectionReadPort::delta(store.storage(), &owner_authority, &cursor, page_size)
                .unwrap_err(),
            WorkProjectionPortError::StaleCursor
        );
    }
}

/// A task created after the snapshot that sorts before every existing task
/// is the other shape of the same defect: under a task-sorted offset its
/// creation lands before the saved position and the delta reports the wrong
/// task.
#[test]
fn delta_reports_a_lexically_earlier_task_created_after_the_snapshot() {
    let store = RegisteredWorkStore::start("earlier-task");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.earlier-task", "actor.work.owner");
    let owner_authority = authority(&owner);
    create(&service, &owner, "task.work.earlier-task.z");
    let snapshot = WorkProjectionReadPort::snapshot(&storage, &owner_authority, 10).unwrap();
    create(&service, &owner, "task.work.earlier-task.a");

    let resume = WorkSqliteStorage::resume_cursor(&snapshot).unwrap();
    let delta = WorkProjectionReadPort::delta(&storage, &owner_authority, &resume, 10).unwrap();
    assert_eq!(
        delta
            .changed()
            .iter()
            .map(|projection| projection.task_id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["task.work.earlier-task.a".to_owned()]
    );
    assert!(delta.coverage().resume_cursor().is_none());
}

#[test]
fn immutable_history_and_projection_rebuild_survive_restart() {
    let store = RegisteredWorkStore::start("restart");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.restart", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.restart");
    create(&service, &owner, task_id.as_str());
    let proposal_id = id::<ProposalId>("proposal.work.restart");
    let accepted = service
        .accept_proposal(
            &owner,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: proposal_id.clone(),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.accept-proposal.work.restart"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();
    assert_eq!(accepted.accepted_proposal(), Some(&proposal_id));
    drop(service);

    let store = store.restart("restart");
    let service = WorkService::new(store.storage().clone());
    assert_eq!(service.load(&owner, &task_id).unwrap(), accepted);
}

#[test]
fn schema_has_no_materialized_work_projection_tables() {
    let store = RegisteredWorkStore::start("schema");
    let tables = store.inspect(|connection| {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name LIKE 'work_%'
                 ORDER BY name",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    });
    // Every Work table is an immutable journal, a monotonic cursor or fence, a
    // durable attempt/effect row, a version-checked run-control, placement, or
    // adjudication authority, an observation cursor, or the index of verified
    // graph versions. None is a materialized projection: a projection is always
    // rebuilt by folding the journal, so no stored table can ever disagree with
    // the events that produced it. `work/projection.rs` holds to that: every
    // read there replays the authority events and rebuilds, storing nothing.
    assert_eq!(
        tables,
        vec![
            "work_attempt_effect_holders_v1".to_owned(),
            "work_attempt_fences_v1".to_owned(),
            "work_attempts_v1".to_owned(),
            "work_blocked_interval_observation_cursors_v1".to_owned(),
            "work_blocked_intervals_v1".to_owned(),
            "work_duplicate_adjudications_v1".to_owned(),
            "work_events_v1".to_owned(),
            "work_leak_adjudications_v1".to_owned(),
            "work_owner_cursors_v1".to_owned(),
            "work_placements_v1".to_owned(),
            "work_product_events_v1".to_owned(),
            "work_product_graph_versions_v1".to_owned(),
            "work_retry_receipts_v1".to_owned(),
            "work_run_controls_v1".to_owned(),
        ]
    );
}

#[test]
fn authority_events_are_scope_exact_and_deterministically_ordered() {
    let store = RegisteredWorkStore::start("authority-events");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.authority-events", "actor.work.owner");
    let later_task = id::<TaskId>("task.work.authority-events.z");
    let earlier_task = id::<TaskId>("task.work.authority-events.a");
    create(&service, &owner, later_task.as_str());
    create(&service, &owner, earlier_task.as_str());
    service
        .accept_proposal(
            &owner,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: later_task.clone(),
                    proposal_id: id("proposal.work.authority-events"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.accept-proposal.work.authority-events"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();

    let events = storage.load_authority_events(&authority(&owner)).unwrap();
    let order = events
        .iter()
        .map(|event| (event.task_id().clone(), event.version()))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            (earlier_task, WorkVersion::initial()),
            (later_task.clone(), WorkVersion::initial()),
            (later_task, WorkVersion::new(2).unwrap()),
        ]
    );
    assert!(
        storage
            .load_authority_events(&authority(&context(
                "project.work.authority-events.other",
                "actor.work.owner"
            )))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn append_is_idempotent_cas_checked_and_exactly_scope_bound() {
    let store = RegisteredWorkStore::start("cas");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.cas", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.cas");
    let command = CreateWorkCommand {
        task_id: task_id.clone(),
        title: "CAS work".to_owned(),
        dependencies: BTreeSet::new(),
        command_id: id("command.work.cas"),
        occurred_at: UtcMicros(10),
    };
    let first = service.create(&owner, command.clone()).unwrap();
    assert_eq!(service.create(&owner, command).unwrap(), first);
    assert_eq!(service.load(&owner, &task_id).unwrap(), first);
    assert!(
        service
            .create(
                &owner,
                CreateWorkCommand {
                    task_id: task_id.clone(),
                    title: "Conflicting replay".to_owned(),
                    dependencies: BTreeSet::new(),
                    command_id: id("command.work.cas"),
                    occurred_at: UtcMicros(10),
                },
            )
            .is_err()
    );
    assert_eq!(store.count("work_events_v1"), 1);

    let concealed = service
        .load(
            &context("project.work.cas.other", "actor.work.owner"),
            &task_id,
        )
        .unwrap_err();
    assert_eq!(
        concealed.kind(),
        tracedecay_contracts::ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn failed_event_insert_cannot_advance_owner_cursor() {
    let store = RegisteredWorkStore::start_with_setup("atomic", |connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER reject_work_event
                 BEFORE INSERT ON work_events_v1
                 BEGIN
                   SELECT RAISE(ABORT, 'injected work append failure');
                 END;",
            )
            .unwrap();
    });
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.atomic", "actor.work.owner");
    assert!(
        service
            .create(
                &owner,
                CreateWorkCommand {
                    task_id: id("task.work.atomic"),
                    title: "Atomic work".to_owned(),
                    dependencies: BTreeSet::new(),
                    command_id: id("command.work.atomic"),
                    occurred_at: UtcMicros(10),
                },
            )
            .is_err()
    );

    for table in ["work_events_v1", "work_owner_cursors_v1"] {
        assert_eq!(
            store.count(table),
            0,
            "{table} must roll back with the event"
        );
    }
}

/// A capped snapshot page is only honest if its cursor leads somewhere. This
/// follows that cursor through `delta` until coverage reports completion and
/// checks the pages together name every task in the authority: a cursor minted
/// at the journal head rather than at the page's own event boundary is stale
/// the moment it is used, which silently strands every task past the cap.
#[test]
fn capped_work_projection_snapshot_pages_every_task_through_delta() {
    let store = RegisteredWorkStore::start("projection-paging");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.projection-paging", "actor.work.owner");
    let task_ids = ["a", "b", "c", "d", "e"]
        .map(|suffix| id::<TaskId>(&format!("task.work.projection-paging.{suffix}")));
    for task_id in &task_ids {
        create(&service, &owner, task_id.as_str());
    }
    let owner_authority = authority(&owner);
    let page_size = 2;

    let snapshot = WorkProjectionReadPort::snapshot(&storage, &owner_authority, page_size).unwrap();
    assert_eq!(snapshot.coverage().returned(), page_size);
    assert_eq!(
        snapshot.coverage().total(),
        u32::try_from(task_ids.len()).unwrap()
    );
    let mut covered = snapshot
        .projections()
        .iter()
        .map(|projection| projection.task_id().clone())
        .collect::<BTreeSet<_>>();

    let mut cursor = snapshot.coverage().resume_cursor().cloned();
    let mut pages = 0usize;
    while let Some(resume) = cursor {
        pages += 1;
        assert!(
            pages <= task_ids.len(),
            "a page must advance the walk, not repeat it"
        );
        let delta =
            WorkProjectionReadPort::delta(&storage, &owner_authority, &resume, page_size).unwrap();
        if pages == 1 {
            // The first continuation must line up with the snapshot it
            // continues, so a follower can prove the two are one read.
            delta.validate_after(&snapshot).unwrap();
        }
        for projection in delta.changed() {
            covered.insert(projection.task_id().clone());
        }
        cursor = delta.coverage().resume_cursor().cloned();
    }
    assert_eq!(covered, BTreeSet::from(task_ids.clone()));

    // A cursor already at the journal head has nothing to hand back.
    let head = WorkProjectionResumeCursorV1::new(
        snapshot.generation_id().clone(),
        format!("work-projection-append-sequence.v1:{}", task_ids.len()),
    )
    .unwrap();
    assert_eq!(
        WorkProjectionReadPort::delta(&storage, &owner_authority, &head, page_size).unwrap_err(),
        WorkProjectionPortError::StaleCursor
    );

    // A page wide enough for the whole authority is complete and offers no
    // continuation to follow.
    let whole = WorkProjectionReadPort::snapshot(&storage, &owner_authority, 1_000).unwrap();
    assert_eq!(
        whole.coverage().returned(),
        u32::try_from(task_ids.len()).unwrap()
    );
    assert!(whole.coverage().resume_cursor().is_none());
}

/// The `work_events_v1` and `work_owner_cursors_v1` shapes `v0.1.0-beta.37`
/// installed, before events carried their append position.
const SHIPPED_WORK_JOURNAL_SCHEMA: &str = "
CREATE TABLE work_owner_cursors_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest)
) STRICT;
CREATE TABLE work_events_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest, task_id, version
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest, task_id, command_id
    )
) STRICT;
";

/// A journal written before `owner_sequence` existed opens under the current
/// schema with its rows numbered in the order they were inserted, keeps
/// accepting appends at the next position, and refuses the cursor tokens the
/// earlier task-sorted contract minted.
#[test]
fn shipped_journal_without_append_positions_gains_them_in_insertion_order() {
    let owner = context("project.work.shipped", "actor.work.owner");
    let owner_authority = authority(&owner);
    let task_a = id::<TaskId>("task.work.shipped.a");
    let task_b = id::<TaskId>("task.work.shipped.b");
    let created = |task_id: &TaskId, occurred_at: i64| {
        WorkEvent::new(
            task_id.clone(),
            WorkVersion::initial(),
            owner_authority.clone(),
            UtcMicros(occurred_at),
            id(&format!("command.create.{task_id}")),
            digest('c'),
            WorkEventKind::Created {
                title: format!("Persist {task_id}"),
                dependencies: BTreeSet::new(),
            },
        )
        .unwrap()
    };
    let accepted = WorkEvent::new(
        task_a.clone(),
        WorkVersion::new(2).unwrap(),
        owner_authority.clone(),
        UtcMicros(12),
        id(&format!("command.accept-proposal.{task_a}")),
        digest('c'),
        WorkEventKind::ProposalAccepted {
            proposal_id: id(&format!("proposal.{task_a}")),
            proposal_digest: digest('b'),
        },
    )
    .unwrap();
    // Inserted a1, b1, a2: `a` gains its second version after `b` exists, so
    // insertion order and `task_id, version` order disagree.
    let shipped = [created(&task_a, 10), created(&task_b, 11), accepted];

    let store = RegisteredWorkStore::start_seeded("shipped", |connection| {
        connection
            .execute_batch(SHIPPED_WORK_JOURNAL_SCHEMA)
            .unwrap();
        for event in &shipped {
            connection
                .execute(
                    "INSERT INTO work_events_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        task_id, version, command_id, input_digest, occurred_at, event_payload
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        owner_authority.project_id().as_str(),
                        owner_authority.repository_id().as_str(),
                        owner_authority.worktree_id().as_str(),
                        owner_authority.actor_id().as_str(),
                        owner_authority.policy_digest().as_str(),
                        event.task_id().as_str(),
                        i64::try_from(event.version().get()).unwrap(),
                        event.command_id().as_str(),
                        event.input_digest().as_str(),
                        event.occurred_at().0,
                        serde_json::to_string(event).unwrap(),
                    ],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO work_owner_cursors_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, sequence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    owner_authority.project_id().as_str(),
                    owner_authority.repository_id().as_str(),
                    owner_authority.worktree_id().as_str(),
                    owner_authority.actor_id().as_str(),
                    owner_authority.policy_digest().as_str(),
                    i64::try_from(shipped.len()).unwrap(),
                ],
            )
            .unwrap();
    });

    let positions = store.inspect(|connection| {
        let mut statement = connection
            .prepare(
                "SELECT task_id, version, owner_sequence FROM work_events_v1
                 ORDER BY owner_sequence",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    });
    assert_eq!(
        positions,
        vec![
            (task_a.as_str().to_owned(), 1, 1),
            (task_b.as_str().to_owned(), 1, 2),
            (task_a.as_str().to_owned(), 2, 3),
        ]
    );

    let storage = store.storage().clone();
    let snapshot = WorkProjectionReadPort::snapshot(&storage, &owner_authority, 10).unwrap();
    assert_eq!(snapshot.sequence().get(), 3);
    assert_eq!(
        by_task(snapshot.projections())
            .values()
            .map(|projection| projection.version().get())
            .collect::<Vec<_>>(),
        vec![2, 1]
    );

    let service = WorkService::new(storage.clone());
    accept(&service, &owner, task_b.as_str(), 20);
    let resume = WorkSqliteStorage::resume_cursor(&snapshot).unwrap();
    let delta = WorkProjectionReadPort::delta(&storage, &owner_authority, &resume, 10).unwrap();
    assert_eq!(delta.to_sequence().get(), 4);
    assert_eq!(
        delta
            .changed()
            .iter()
            .map(|projection| (projection.task_id().clone(), projection.version().get()))
            .collect::<Vec<_>>(),
        vec![(task_b, 2)]
    );

    let shipped_cursor = WorkProjectionResumeCursorV1::new(
        snapshot.generation_id().clone(),
        "work-projection-sequence.v1:2",
    )
    .unwrap();
    assert_eq!(
        WorkProjectionReadPort::delta(&storage, &owner_authority, &shipped_cursor, 10).unwrap_err(),
        WorkProjectionPortError::StaleCursor
    );
}

/// Reader-side SQL work for one closure: how many reader lanes it acquired
/// (one per statement) and how many SQLite VM steps those statements ran.
/// VM steps grow with the rows a statement visits and returns, so they witness
/// how much history a read touched without instrumenting the decode path.
fn measure_reads<T>(store: &RegisteredWorkStore, read: impl FnOnce() -> T) -> (T, u64, u64) {
    let before = store.readers.telemetry_snapshot();
    let value = read();
    let after = store.readers.telemetry_snapshot();
    (
        value,
        after.acquire_events - before.acquire_events,
        after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps,
    )
}

/// Writer-side SQLite VM steps for one closure — the statements the exact-SQL
/// transaction it runs executed.
fn measure_writes<T>(store: &RegisteredWorkStore, write: impl FnOnce() -> T) -> (T, u64) {
    let before = store.writer.telemetry_snapshot().sqlite_vm.vm_steps;
    let value = write();
    (
        value,
        store.writer.telemetry_snapshot().sqlite_vm.vm_steps - before,
    )
}

fn create_accepted_tasks(
    service: &WorkService<WorkSqliteStorage>,
    context: &RequestContext,
    prefix: &str,
    count: usize,
) {
    for index in 0..count {
        let task_id = format!("{prefix}.{index:03}");
        create(service, context, &task_id);
        accept(service, context, &task_id, 20);
    }
}

/// An exact read answers from the task's own history plus the owner frontier.
/// Its SQL work must therefore not grow with the history of unrelated tasks
/// in the same authority: the same read over an authority with many more
/// unrelated tasks costs the same statements and about the same VM steps.
#[test]
fn exact_task_read_costs_its_own_history_not_the_authoritys() {
    let store = RegisteredWorkStore::start("exact-read");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let target = id::<TaskId>("task.work.exact-read.target");
    let sparse = context("project.work.exact-read.sparse", "actor.work.owner");
    let crowded = context("project.work.exact-read.crowded", "actor.work.owner");
    for (owner, unrelated) in [(&sparse, 2), (&crowded, 64)] {
        create(&service, owner, target.as_str());
        accept(&service, owner, target.as_str(), 20);
        create_accepted_tasks(&service, owner, "task.work.exact-read.unrelated", unrelated);
    }

    let mut costs = [(&sparse, 0, 0), (&crowded, 0, 0)];
    for (owner, statements, steps) in &mut costs {
        let (snapshot, read_statements, read_steps) = measure_reads(&store, || {
            WorkProjectionReadPort::exact_snapshot(&storage, &authority(owner), &target).unwrap()
        });
        assert_eq!(snapshot.projections()[0].version().get(), 2);
        assert_eq!(
            snapshot.sequence().get(),
            storage
                .load_authority_events(&authority(owner))
                .unwrap()
                .len() as u64,
            "an exact read is positioned at the authority frontier"
        );
        *statements = read_statements;
        *steps = read_steps;
    }
    let [
        (_, sparse_statements, sparse_steps),
        (_, crowded_statements, crowded_steps),
    ] = costs;
    eprintln!(
        "exact read: sparse authority {sparse_statements} statements / {sparse_steps} vm steps, \
         crowded authority {crowded_statements} statements / {crowded_steps} vm steps"
    );
    assert_eq!(
        (sparse_statements, crowded_statements),
        (2, 2),
        "the frontier and the task's history, nothing else"
    );
    assert!(
        crowded_steps <= sparse_steps * 2,
        "sixty-two more unrelated tasks must not be read to answer an exact read: \
         sparse {sparse_steps} vm steps, crowded {crowded_steps} vm steps"
    );

    assert_eq!(
        WorkProjectionReadPort::exact_snapshot(
            &storage,
            &authority(&sparse),
            &id::<TaskId>("task.work.exact-read.absent"),
        )
        .unwrap_err(),
        WorkProjectionPortError::NotFoundOrNotAuthorized
    );
    assert_eq!(
        WorkProjectionReadPort::exact_snapshot(
            &storage,
            &authority(&context(
                "project.work.exact-read.other",
                "actor.work.owner"
            )),
            &target,
        )
        .unwrap_err(),
        WorkProjectionPortError::NotFoundOrNotAuthorized
    );
}

/// A capped page decodes and folds the histories of the tasks it returns,
/// not every event in the authority, so it costs strictly less SQL work than
/// the complete page over the same journal — and both agree with a full
/// per-task replay.
#[test]
fn capped_page_reads_only_the_selected_histories() {
    let store = RegisteredWorkStore::start("capped-page");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.capped-page", "actor.work.owner");
    let owner_authority = authority(&owner);
    create_accepted_tasks(&service, &owner, "task.work.capped-page", 24);
    for index in 0..24 {
        admit(
            &service,
            &owner,
            &format!("task.work.capped-page.{index:03}"),
            30,
        );
    }

    let (capped, capped_statements, capped_steps) = measure_reads(&store, || {
        WorkProjectionReadPort::snapshot(&storage, &owner_authority, 2).unwrap()
    });
    let (complete, complete_statements, complete_steps) = measure_reads(&store, || {
        WorkProjectionReadPort::snapshot(&storage, &owner_authority, 1_000).unwrap()
    });
    eprintln!(
        "page of 2 tasks: {capped_statements} statements / {capped_steps} vm steps; \
         page of 24 tasks: {complete_statements} statements / {complete_steps} vm steps"
    );
    assert_eq!(capped.coverage().returned(), 2);
    assert_eq!(complete.coverage().returned(), 24);
    assert_eq!(
        (capped_statements, complete_statements),
        (3, 3),
        "frontier, changed-task discovery, selected histories"
    );
    assert!(
        capped_steps < complete_steps,
        "a page of two tasks must read less than the page of all twenty-four: \
         capped {capped_steps} vm steps, complete {complete_steps} vm steps"
    );

    // The capped page is cut before the third task's first event, so it
    // carries the first two tasks as of that position: created and accepted,
    // not yet admitted. The complete page equals every task's full replay.
    assert_eq!(capped.sequence().get(), 4);
    for projection in capped.projections() {
        assert_eq!(projection.version().get(), 2);
    }
    for projection in complete.projections() {
        assert_eq!(
            *projection,
            WorkStoragePort::projection(&storage, &owner_authority, projection.task_id()).unwrap(),
            "a page projection must equal the task's full replay"
        );
        assert_eq!(projection.version().get(), 3);
    }
}

/// An append reconstructs the task's prior state once, admits the new event
/// onto it, and inserts — it does not re-read and re-fold the history it just
/// extended. The witness is the writer's SQL work: appending onto a long
/// history must cost about one history read, not two, measured against the
/// reader's cost for that same history select.
#[test]
fn append_folds_the_admitted_event_onto_one_prior_reconstruction() {
    let store = RegisteredWorkStore::start("append-once");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.append-once", "actor.work.owner");
    let owner_authority = authority(&owner);
    let task_id = id::<TaskId>("task.work.append-once");
    create(&service, &owner, task_id.as_str());
    let history_len = 240u64;
    let replan = |version: u64, command: &str| ReplanDependenciesCommand {
        task_id: task_id.clone(),
        dependencies: BTreeSet::new(),
        expected_version: WorkVersion::new(version).unwrap(),
        command_id: id(&format!("command.replan.work.append-once.{command}")),
        occurred_at: UtcMicros(10),
    };
    for version in 1..history_len {
        service
            .replan_dependencies(&owner, replan(version, &version.to_string()))
            .unwrap();
    }

    let (history, _, history_steps) = measure_reads(&store, || {
        WorkStoragePort::load(&storage, &owner_authority, &task_id).unwrap()
    });
    assert_eq!(history.len() as u64, history_len);
    let (appended, append_steps) = measure_writes(&store, || {
        service
            .replan_dependencies(&owner, replan(history_len, "measured"))
            .unwrap()
    });
    eprintln!(
        "append onto {history_len} events: {append_steps} vm steps; \
         one history select: {history_steps} vm steps"
    );
    assert!(
        append_steps < history_steps * 2,
        "an append must not select the history twice: append {append_steps} vm steps, \
         one history select {history_steps} vm steps"
    );
    assert_eq!(appended.version().get(), history_len + 1);
    assert_eq!(
        appended,
        WorkStoragePort::projection(&storage, &owner_authority, &task_id).unwrap(),
        "the folded state must equal the full replay of the committed history"
    );

    // Refusals leave the journal and the frontier exactly where they were.
    let events = store.count("work_events_v1");
    let cursor = store
        .inspect(|connection| WorkSqliteStorage::owner_cursor(connection, &owner_authority))
        .unwrap();
    assert_eq!(
        service
            .replan_dependencies(&owner, replan(history_len, "measured"))
            .unwrap(),
        appended,
        "an exact replay returns the committed state"
    );
    assert!(
        service
            .replan_dependencies(&owner, replan(2, "losing"))
            .is_err(),
        "a losing compare-and-swap is refused"
    );
    assert!(
        service
            .admit_execution(
                &owner,
                AdmitExecutionCommand {
                    task_id: task_id.clone(),
                    expected_version: appended.version(),
                    command_id: id("command.admit.work.append-once.invalid"),
                    occurred_at: UtcMicros(10),
                },
            )
            .is_err(),
        "admitting execution without an accepted proposal is refused"
    );
    assert_eq!(store.count("work_events_v1"), events);
    assert_eq!(
        store
            .inspect(|connection| WorkSqliteStorage::owner_cursor(connection, &owner_authority))
            .unwrap(),
        cursor
    );
}

#[test]
fn proposal_state_and_owner_cursor_advance_once_per_new_event() {
    let store = RegisteredWorkStore::start("cursor");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.cursor", "actor.work.owner");
    create(&service, &owner, "task.work.cursor");

    let owner_authority = authority(&owner);
    let cursor = store
        .inspect(|connection| WorkSqliteStorage::owner_cursor(connection, &owner_authority))
        .unwrap();
    assert_eq!(cursor, 1);
}
