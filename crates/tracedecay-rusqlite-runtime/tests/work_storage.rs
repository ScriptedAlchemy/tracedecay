use std::collections::BTreeSet;

use tracedecay_application::{
    AcceptProposalCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, ReviewProposalCommand,
    WorkProjectionPortError, WorkProjectionReadPort, WorkService,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkProjectionResumeCursorV1, WorkVersion, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

mod work_registered_store;

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
        tracedecay_application::ApplicationProblemKind::NotFoundOrNotAuthorized
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
        format!("work-projection-sequence.v1:{}", task_ids.len()),
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
