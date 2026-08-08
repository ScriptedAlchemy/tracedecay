//! The registered Work store's routing-snapshot contract.
//!
//! `WorkStoragePort::routing_snapshot` is what `WorkService::generate_proposal`
//! routes every production proposal against. It used to carry a default trait
//! body returning `WorkRoutingSnapshotV1::default()`, which this storage
//! adapter silently inherited: production planned against a snapshot no
//! adapter had authored. The method is now required, and this suite pins what
//! the SQLite adapter actually answers.
//!
//! Two claims are proved here. First, the answer is derived from REAL stored
//! state: it is scope-checked against `work_events_v1`, so an unheld task and
//! another actor's task are refused rather than handed an empty snapshot that
//! would read as "held, and holding no routes". Second, for a task this
//! authority does hold, the answer is the EMPTY snapshot — because this store
//! persists no routing authority at all: no table or `WorkEventKind` records a
//! route candidate, a budget envelope, a content-location limit, a
//! route-attributed prior outcome, or a human route override. That empty
//! answer is now an explicit, reviewed one, and `generate_proposal` records
//! `NoEligibleRoutes`, which is a decision rather than a failure. When a
//! routing authority is persisted, these assertions are what must change.

mod work_registered_store;

use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope, WorkRoutingSnapshotV1, WorkService, WorkStorageError,
    WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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
        id::<RepositoryId>("repository.work.routing"),
        id::<WorktreeId>("worktree.work.routing"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.routing"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.routing").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.routing").unwrap()]),
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
                title: format!("Route {task_id}"),
                dependencies: BTreeSet::new(),
                command_id: id::<WorkCommandId>(&format!("command.create.{task_id}")),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
}

#[test]
fn routing_snapshot_refuses_a_task_the_authority_does_not_hold() {
    let store = RegisteredWorkStore::start("routing-unheld");
    let owner = context("project.work.routing-unheld", "actor.work.owner");

    // Nothing has been appended, so there is no scoped task to answer for.
    assert_eq!(
        store
            .storage()
            .routing_snapshot(&authority(&owner), &id::<TaskId>("task.work.absent")),
        Err(WorkStorageError::NotFoundOrNotAuthorized)
    );
}

#[test]
fn routing_snapshot_answers_a_held_task_with_the_empty_snapshot() {
    let store = RegisteredWorkStore::start("routing-held");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.routing-held", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.routing-held");
    create(&service, &owner, task_id.as_str());

    let snapshot = store
        .storage()
        .routing_snapshot(&authority(&owner), &task_id)
        .expect("a held task is answered, not refused");

    // The empty snapshot is the honest answer: this store persists no routing
    // authority, so every field below has no backing row to read.
    assert_eq!(snapshot, WorkRoutingSnapshotV1::default());
    assert!(snapshot.eligible_routes.is_empty());
    assert_eq!(snapshot.budget, None);
    assert_eq!(snapshot.content_location, None);
    assert!(snapshot.prior_outcomes.is_empty());
    assert_eq!(snapshot.human_override, None);
}

#[test]
fn routing_snapshot_reads_real_scoped_state_and_not_a_constant() {
    let store = RegisteredWorkStore::start("routing-scope");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.routing-scope", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.routing-scope");
    create(&service, &owner, task_id.as_str());

    // Same task identity, a different actor: the routing read is scope-bound
    // the way `load` is, so it refuses rather than answering for a history it
    // is not authorized to see. An adapter answering a constant could not tell
    // these two calls apart.
    let intruder = context("project.work.routing-scope", "actor.work.intruder");
    assert_eq!(
        store
            .storage()
            .routing_snapshot(&authority(&intruder), &task_id),
        Err(WorkStorageError::NotFoundOrNotAuthorized)
    );
    assert!(
        store
            .storage()
            .routing_snapshot(&authority(&owner), &task_id)
            .is_ok()
    );
}

#[test]
fn the_work_schema_persists_no_routing_authority() {
    let store = RegisteredWorkStore::start("routing-schema");
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
    // This is the evidence behind the empty snapshot above: no installed table
    // declares a route, a budget envelope, a content-location limit, or a
    // recorded route override, so there is nothing for the adapter to read and
    // any non-empty answer it gave would be fabricated. The day a routing
    // authority is persisted, this assertion fails first and points at the
    // adapter that must learn to read it.
    let routing_tables = tables
        .iter()
        .filter(|table| {
            ["route", "routing", "budget", "content_location", "override"]
                .iter()
                .any(|marker| table.contains(marker))
        })
        .collect::<Vec<_>>();
    assert!(
        routing_tables.is_empty(),
        "the Work schema grew a routing authority: {routing_tables:?}"
    );
    assert_eq!(store.count("work_events_v1"), 0);
}

#[test]
fn routing_snapshot_survives_a_restart_with_the_stored_task() {
    let store = RegisteredWorkStore::start("routing-restart");
    let service = WorkService::new(store.storage().clone());
    let owner = context("project.work.routing-restart", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.routing-restart");
    create(&service, &owner, task_id.as_str());
    drop(service);

    let store = store.restart("routing-restart");
    assert_eq!(
        store
            .storage()
            .routing_snapshot(&authority(&owner), &task_id),
        Ok(WorkRoutingSnapshotV1::default())
    );
    assert_eq!(
        store
            .storage()
            .routing_snapshot(&authority(&owner), &id::<TaskId>("task.work.absent")),
        Err(WorkStorageError::NotFoundOrNotAuthorized)
    );
}
