use std::collections::BTreeSet;

use tracedecay_application::{
    AcceptProposalCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, ReviewProposalCommand,
    WorkAppendRequest, WorkProjectionReadPort, WorkService, WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkEvent, WorkEventKind, WorkVersion, WorktreeId,
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
fn projection_reads_are_scope_exact_generation_bound_and_incremental() {
    let store = RegisteredWorkStore::start("read-port");
    let storage = store.storage().clone();
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
fn exact_projection_lookup_is_not_limited_by_snapshot_page_position() {
    let store = RegisteredWorkStore::start("exact-read");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.exact-read", "actor.work.owner");
    for index in 0..513 {
        create(
            &service,
            &owner,
            &format!("task.work.exact-read.{index:04}"),
        );
    }
    let target = id::<TaskId>("task.work.exact-read.zzzz");
    create(&service, &owner, target.as_str());

    let capped = WorkProjectionReadPort::snapshot(&storage, &authority(&owner), 512).unwrap();
    assert!(
        capped
            .projections()
            .iter()
            .all(|projection| projection.task_id() != &target)
    );

    let exact =
        WorkProjectionReadPort::exact_snapshot(&storage, &authority(&owner), &target).unwrap();
    assert_eq!(exact.projections().len(), 1);
    assert_eq!(exact.projections()[0].task_id(), &target);
    assert!(matches!(
        exact.coverage(),
        tracedecay_domain::WorkProjectionCoverageV1::Complete {
            returned: 1,
            total: 1
        }
    ));
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

    for table in [
        "work_events_v1",
        "work_projection_snapshots_v1",
        "work_projection_deltas_v1",
        "work_projection_fold_state_v1",
        "work_owner_cursors_v1",
    ] {
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
    assert_eq!(store.count("work_projection_snapshots_v1"), 1);
    assert_eq!(store.count("work_projection_fold_state_v1"), 1);
}

#[test]
fn stale_projection_snapshot_aborts_event_and_cursor_publication() {
    let store = RegisteredWorkStore::start("snapshot-cas");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.snapshot-cas", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.snapshot-cas");
    create(&service, &owner, task_id.as_str());
    store.inspect(|connection| {
        connection
            .execute("UPDATE work_projection_snapshots_v1 SET version = 99", [])
            .unwrap();
    });

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
    assert_eq!(store.count("work_events_v1"), 1);
    let owner_authority = authority(&owner);
    assert_eq!(
        store
            .inspect(|connection| WorkSqliteStorage::owner_cursor(connection, &owner_authority))
            .unwrap(),
        1
    );
}

#[test]
fn a_task_with_no_published_fold_state_rebuilds_once_and_then_folds() {
    let store = RegisteredWorkStore::start("fold-migration");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.fold-migration", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.fold-migration");
    create(&service, &owner, task_id.as_str());

    // A database written before the fold state existed has events and a
    // published projection but no fold row.
    store.inspect(|connection| {
        connection
            .execute("DELETE FROM work_projection_fold_state_v1", [])
            .unwrap();
    });
    assert_eq!(store.count("work_projection_fold_state_v1"), 0);

    let accepted = service
        .accept_proposal(
            &owner,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: id::<ProposalId>("proposal.work.fold-migration"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.accept-proposal.work.fold-migration"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();

    assert_eq!(accepted, service.load(&owner, &task_id).unwrap());
    assert_eq!(accepted.version(), WorkVersion::new(2).unwrap());
    assert_eq!(store.count("work_projection_fold_state_v1"), 1);

    // The republished fold state carries the append forward without another
    // rebuild, and the projection still matches the events on disk.
    let admitted = service
        .admit_execution(
            &owner,
            tracedecay_application::AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.admit.work.fold-migration"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    assert!(admitted.is_execution_admitted());
    assert_eq!(admitted, service.load(&owner, &task_id).unwrap());
}

#[test]
fn a_fold_state_written_at_an_unknown_version_is_not_trusted() {
    let store = RegisteredWorkStore::start("fold-version");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.fold-version", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.fold-version");
    create(&service, &owner, task_id.as_str());

    // A future binary published a fold payload this binary cannot fold.
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_projection_fold_state_v1
                 SET state_version = 999, state_payload = '{\"unreadable\":true}'",
                [],
            )
            .unwrap();
    });

    let accepted = service
        .accept_proposal(
            &owner,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: id::<ProposalId>("proposal.work.fold-version"),
                    proposal_digest: digest('b'),
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.accept-proposal.work.fold-version"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();

    assert_eq!(accepted, service.load(&owner, &task_id).unwrap());
    let republished: i64 = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT state_version FROM work_projection_fold_state_v1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    });
    assert_eq!(republished, 1);
}

#[test]
fn an_append_with_published_fold_state_never_re_reads_the_history() {
    let store = RegisteredWorkStore::start("fold-no-reread");
    let storage = store.storage().clone();
    let service = WorkService::new(storage.clone());
    let owner = context("project.work.fold-no-reread", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.fold-no-reread");
    create(&service, &owner, task_id.as_str());
    let owner_authority = authority(&owner);

    // Remove the stored history. A storage path that still rebuilt from
    // events could not produce a version-2 projection from nothing; only the
    // published fold state can carry this append.
    store.inspect(|connection| {
        connection
            .execute("DELETE FROM work_events_v1", [])
            .unwrap();
    });
    assert_eq!(store.count("work_events_v1"), 0);

    let outcome = WorkStoragePort::append(
        &storage,
        &WorkAppendRequest {
            expected_version: Some(WorkVersion::initial()),
            event: WorkEvent::new(
                task_id.clone(),
                WorkVersion::new(2).unwrap(),
                owner_authority,
                UtcMicros(20),
                id::<WorkCommandId>("command.work.fold-no-reread.accept"),
                digest('b'),
                WorkEventKind::ProposalAccepted {
                    proposal_id: id::<ProposalId>("proposal.work.fold-no-reread"),
                    proposal_digest: digest('c'),
                },
            )
            .unwrap(),
        },
    )
    .unwrap();

    let projection = outcome.into_projection();
    assert_eq!(projection.version(), WorkVersion::new(2).unwrap());
    assert_eq!(
        projection.accepted_proposal(),
        Some(&id::<ProposalId>("proposal.work.fold-no-reread"))
    );
    assert_eq!(store.count("work_events_v1"), 1);
}
