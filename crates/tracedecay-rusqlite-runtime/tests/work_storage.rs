use std::collections::BTreeSet;

use tracedecay_application::{
    AcceptProposalCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, ReviewProposalCommand,
    WorkService,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkVersion, WorktreeId,
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
    assert_eq!(
        tables,
        vec![
            "work_attempt_fences_v1".to_owned(),
            "work_attempts_v1".to_owned(),
            "work_events_v1".to_owned(),
            "work_owner_cursors_v1".to_owned()
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
