//! Durable single-use handoff opens over the registered Work SQL channel.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    HandoffAuthoritySnapshotV1, HandoffOpenAuthorityError, HandoffOpenAuthorityPort,
    HandoffOpenBindingV1, HandoffOpenError, HandoffOpenExpectationV1, HandoffOpenKindV1,
    HandoffOpenService, HandoffOpenTargetError, HandoffOpenTargetPort, HandoffOpenToken,
    HandoffSessionId, OpenTaskHandoffRequestV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkVersion, WorktreeId,
};
use tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

mod registered_workflow_store;

use registered_workflow_store::RegisteredWorkflowStore;

const TOKEN_SECRET: &str = "handoff-open-secret-00000000000000000001";

#[derive(Clone, Copy)]
struct CurrentTarget;

impl HandoffOpenTargetPort for CurrentTarget {
    fn is_current<'a>(
        &'a self,
        _context: &'a RequestContext,
        _binding: &'a HandoffOpenBindingV1,
    ) -> Pin<Box<dyn Future<Output = Result<bool, HandoffOpenTargetError>> + Send + 'a>> {
        Box::pin(async { Ok(true) })
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
}

fn context(request_id: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.handoff.runtime-store"),
        id::<RepositoryId>("repository.handoff.runtime-store"),
        id::<WorktreeId>("worktree.handoff.runtime-store"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id::<CapabilityGrantId>("grant.handoff.runtime-store"),
        3,
        digest('a'),
        id::<ActorId>("actor.handoff.runtime-store"),
        UtcMicros(1),
        UtcMicros(120_000_000),
        scope.clone(),
        BTreeSet::from([
            CapabilityId::new("capability.handoff.issue_task_handoff").unwrap(),
            CapabilityId::new("capability.handoff.open_task_handoff").unwrap(),
        ]),
        BTreeSet::from([
            UseCaseId::new("use-case.handoff.issue_task_handoff").unwrap(),
            UseCaseId::new("use-case.handoff.open_task_handoff").unwrap(),
        ]),
        DisclosureClass::Metadata,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.handoff.runtime-store"),
        scope,
        grant,
        id::<RequestId>(request_id),
        Deadline::new(UtcMicros(90_000_000)).unwrap(),
        CancellationContext::active(format!("cancel.{request_id}")).unwrap(),
    )
    .unwrap()
}

fn authority_snapshot() -> HandoffAuthoritySnapshotV1 {
    HandoffAuthoritySnapshotV1::new(digest('b'), digest('c')).unwrap()
}

fn binding(context: &RequestContext) -> HandoffOpenBindingV1 {
    HandoffOpenBindingV1::task(
        context,
        id::<HandoffSessionId>("lsp-session.handoff.runtime-store"),
        id::<TaskId>("task.handoff.runtime-store"),
        WorkVersion::new(8).unwrap(),
        context.actor().clone(),
        authority_snapshot(),
    )
    .unwrap()
}

fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn consume_is_atomic_secret_free_and_idempotent_across_restart() {
    let store = RegisteredWorkflowStore::start("handoff-open");
    let sqlite = HandoffOpenSqliteAuthority::from_registered(store.storage().clone()).unwrap();
    let issue_context = context("request.handoff.issue");
    let service = HandoffOpenService::new(sqlite, CurrentTarget);
    let token = HandoffOpenToken::new(TOKEN_SECRET.to_owned()).unwrap();
    let issued = run(service.issue(
        &issue_context,
        binding(&issue_context),
        &token,
        UtcMicros(1_000_000),
        UtcMicros(61_000_000),
    ))
    .unwrap();
    let issue_replay = run(service.issue(
        &issue_context,
        binding(&issue_context),
        &token,
        UtcMicros(1_100_000),
        UtcMicros(61_100_000),
    ))
    .unwrap();
    assert_eq!(issue_replay, issued);

    store.inspect(|connection| {
        let (token_digest, grant_payload): (String, String) = connection
            .query_row(
                "SELECT token_digest, grant_payload FROM handoff_open_grants_v1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_ne!(token_digest, TOKEN_SECRET);
        assert!(!grant_payload.contains(TOKEN_SECRET));
    });

    drop(service);
    let store = store.restart("handoff-open-restart");
    let sqlite = HandoffOpenSqliteAuthority::from_registered(store.storage().clone()).unwrap();
    let open_context = context("request.handoff.open");
    let service = HandoffOpenService::new(sqlite, CurrentTarget);
    let request = OpenTaskHandoffRequestV1 {
        token: TOKEN_SECRET.to_owned(),
        session_id: id::<HandoffSessionId>("lsp-session.handoff.runtime-store"),
    };
    let first = run(service.open_task(
        &open_context,
        request.clone(),
        authority_snapshot(),
        UtcMicros(2_000_000),
    ))
    .unwrap();
    let replay = run(service.open_task(
        &open_context,
        request.clone(),
        authority_snapshot(),
        UtcMicros(2_100_000),
    ))
    .unwrap();
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(store.count("handoff_open_grants_v1"), 1);

    let replacement_context = context("request.handoff.replacement");
    assert_eq!(
        run(service.open_task(
            &replacement_context,
            request,
            authority_snapshot(),
            UtcMicros(2_200_000),
        )),
        Err(HandoffOpenError::NotFoundOrNotAuthorized)
    );
    store.inspect(|connection| {
        let consumed_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM handoff_open_grants_v1
                 WHERE consumption_payload IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(consumed_rows, 1);
    });
}

#[test]
fn changed_input_for_the_same_request_is_an_idempotency_conflict() {
    let store = RegisteredWorkflowStore::start("handoff-open-idempotency-conflict");
    let sqlite = HandoffOpenSqliteAuthority::from_registered(store.storage().clone()).unwrap();
    let issue_context = context("request.handoff.issue-idempotency-conflict");
    let service = HandoffOpenService::new(sqlite.clone(), CurrentTarget);
    let token = HandoffOpenToken::new(TOKEN_SECRET.to_owned()).unwrap();
    let grant = run(service.issue(
        &issue_context,
        binding(&issue_context),
        &token,
        UtcMicros(1_000_000),
        UtcMicros(61_000_000),
    ))
    .unwrap();

    let open_context = context("request.handoff.open-idempotency-conflict");
    let request = OpenTaskHandoffRequestV1 {
        token: TOKEN_SECRET.to_owned(),
        session_id: id::<HandoffSessionId>("lsp-session.handoff.runtime-store"),
    };
    let first = run(service.open_task(
        &open_context,
        request.clone(),
        authority_snapshot(),
        UtcMicros(2_000_000),
    ))
    .unwrap();
    let replay = run(service.open_task(
        &open_context,
        request,
        authority_snapshot(),
        UtcMicros(2_100_000),
    ))
    .unwrap();
    assert_eq!(replay.receipt, first.receipt);

    assert_eq!(
        sqlite.consume(
            grant.token_digest(),
            &HandoffOpenExpectationV1::from_request(
                &open_context,
                HandoffOpenKindV1::Task,
                id::<HandoffSessionId>("lsp-session.handoff.runtime-store"),
            )
            .unwrap(),
            open_context.request_id(),
            &digest('d'),
            UtcMicros(2_200_000),
        ),
        Err(HandoffOpenAuthorityError::IdempotencyConflict)
    );

    let wrong_session = OpenTaskHandoffRequestV1 {
        token: TOKEN_SECRET.to_owned(),
        session_id: id::<HandoffSessionId>("lsp-session.handoff.wrong"),
    };
    assert_eq!(
        run(service.open_task(
            &context("request.handoff.wrong-session-after-conflict"),
            wrong_session,
            authority_snapshot(),
            UtcMicros(2_300_000),
        )),
        Err(HandoffOpenError::NotFoundOrNotAuthorized)
    );
}

#[test]
fn wrong_session_and_expired_grants_are_concealed_without_consuming() {
    let store = RegisteredWorkflowStore::start("handoff-open-conceal");
    let sqlite = HandoffOpenSqliteAuthority::from_registered(store.storage().clone()).unwrap();
    let issue_context = context("request.handoff.issue-conceal");
    let service = HandoffOpenService::new(sqlite, CurrentTarget);
    let token = HandoffOpenToken::new(TOKEN_SECRET.to_owned()).unwrap();
    run(service.issue(
        &issue_context,
        binding(&issue_context),
        &token,
        UtcMicros(1_000_000),
        UtcMicros(61_000_000),
    ))
    .unwrap();

    let wrong_session = OpenTaskHandoffRequestV1 {
        token: TOKEN_SECRET.to_owned(),
        session_id: id::<HandoffSessionId>("lsp-session.handoff.wrong"),
    };
    assert_eq!(
        run(service.open_task(
            &context("request.handoff.wrong-session"),
            wrong_session,
            authority_snapshot(),
            UtcMicros(2_000_000),
        )),
        Err(HandoffOpenError::NotFoundOrNotAuthorized)
    );
    let expired = OpenTaskHandoffRequestV1 {
        token: TOKEN_SECRET.to_owned(),
        session_id: id::<HandoffSessionId>("lsp-session.handoff.runtime-store"),
    };
    assert_eq!(
        run(service.open_task(
            &context("request.handoff.expired"),
            expired,
            authority_snapshot(),
            UtcMicros(61_000_000),
        )),
        Err(HandoffOpenError::NotFoundOrNotAuthorized)
    );
    store.inspect(|connection| {
        let consumption: Option<String> = connection
            .query_row(
                "SELECT consumption_payload FROM handoff_open_grants_v1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(consumption, None);
    });
}
