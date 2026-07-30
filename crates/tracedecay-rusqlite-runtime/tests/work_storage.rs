use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_application::{
    AcceptProposalCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, ReviewProposalCommand,
    WorkProjectionReadPort, WorkService,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkVersion, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::{WorkSqliteStorage, install_work_schema};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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
fn projection_reads_are_scope_exact_generation_bound_and_incremental() {
    let connection = Connection::open_in_memory().unwrap();
    install_work_schema(&connection).unwrap();
    let storage = WorkSqliteStorage::new(Arc::new(Mutex::new(connection)));
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.read-port", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.read-port");
    create(&service, &owner, task_id.as_str());

    let owner_authority = authority(&owner);
    let snapshot = WorkProjectionReadPort::snapshot(&storage, &owner_authority, 100).unwrap();
    assert_eq!(snapshot.projections().len(), 1);
    assert_eq!(snapshot.projections()[0].task_id(), &task_id);
    assert_eq!(snapshot.sequence().get(), 1);

    let other = context("project.work.read-port.other", "actor.work.owner");
    let other_snapshot =
        WorkProjectionReadPort::snapshot(&storage, &authority(&other), 100).unwrap();
    assert!(other_snapshot.projections().is_empty());
    assert_ne!(other_snapshot.generation_id(), snapshot.generation_id());

    service
        .accept_proposal(
            &owner,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: id("proposal.work.read-port"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.accept-proposal.work.read-port"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();
    let cursor = WorkSqliteStorage::resume_cursor(&snapshot).unwrap();
    let delta = WorkProjectionReadPort::delta(&storage, &owner_authority, &cursor, 100).unwrap();
    assert_eq!(delta.from_sequence(), snapshot.sequence());
    assert_eq!(delta.to_sequence().get(), 2);
    assert_eq!(delta.changed().len(), 1);
    assert_eq!(delta.changed()[0].task_id(), &task_id);
}

#[test]
fn immutable_history_and_projection_rebuild_survive_restart() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("work.sqlite");
    let connection = Connection::open(&path).unwrap();
    install_work_schema(&connection).unwrap();
    let storage = WorkSqliteStorage::new(Arc::new(Mutex::new(connection)));
    let service = WorkService::new(storage);
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

    let reopened = Connection::open(&path).unwrap();
    let service = WorkService::new(WorkSqliteStorage::new(Arc::new(Mutex::new(reopened))));
    assert_eq!(service.load(&owner, &task_id).unwrap(), accepted);
}

#[test]
fn append_is_idempotent_cas_checked_and_exactly_scope_bound() {
    let connection = Connection::open_in_memory().unwrap();
    install_work_schema(&connection).unwrap();
    let service = WorkService::new(WorkSqliteStorage::new(Arc::new(Mutex::new(connection))));
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
fn failed_event_insert_cannot_publish_projection_or_cursor() {
    let connection = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    install_work_schema(&connection.lock().unwrap()).unwrap();
    connection
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_work_event
             BEFORE INSERT ON work_events_v1
             BEGIN
               SELECT RAISE(ABORT, 'injected work append failure');
             END;",
        )
        .unwrap();
    let storage = WorkSqliteStorage::new(Arc::clone(&connection));
    let service = WorkService::new(storage);
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

    let connection = connection.lock().unwrap();
    for table in [
        "work_events_v1",
        "work_projection_snapshots_v1",
        "work_projection_deltas_v1",
        "work_owner_cursors_v1",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} must roll back with the event");
    }
}

#[test]
fn proposal_state_and_owner_cursor_advance_once_per_new_event() {
    let connection = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    install_work_schema(&connection.lock().unwrap()).unwrap();
    let storage = WorkSqliteStorage::new(Arc::clone(&connection));
    let service = WorkService::new(storage);
    let owner = context("project.work.cursor", "actor.work.owner");
    create(&service, &owner, "task.work.cursor");

    let owner_authority = authority(&owner);
    let cursor =
        WorkSqliteStorage::owner_cursor(&connection.lock().unwrap(), &owner_authority).unwrap();
    assert_eq!(cursor, 1);
    let snapshot_count: i64 = connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM work_projection_snapshots_v1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snapshot_count, 1);
}

#[test]
fn stale_projection_snapshot_aborts_event_and_cursor_publication() {
    let connection = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    install_work_schema(&connection.lock().unwrap()).unwrap();
    let service = WorkService::new(WorkSqliteStorage::new(Arc::clone(&connection)));
    let owner = context("project.work.snapshot-cas", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.snapshot-cas");
    create(&service, &owner, task_id.as_str());
    connection
        .lock()
        .unwrap()
        .execute("UPDATE work_projection_snapshots_v1 SET version = 99", [])
        .unwrap();

    let result = service.accept_proposal(
        &owner,
        AcceptProposalCommand {
            review: ReviewProposalCommand {
                task_id,
                proposal_id: id("proposal.work.snapshot-cas"),
                proposal_digest: digest('c'),
                expected_version: WorkVersion::initial(),
                command_id: id("command.accept-proposal.work.snapshot-cas"),
                occurred_at: UtcMicros(20),
            },
        },
    );

    assert!(result.is_err());
    let connection = connection.lock().unwrap();
    let event_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM work_events_v1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_count, 1);
    assert_eq!(
        WorkSqliteStorage::owner_cursor(&connection, &authority(&owner)).unwrap(),
        1
    );
}
