use std::collections::BTreeSet;
use std::fmt;

use rusqlite::Connection;
use tracedecay_application::{
    AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetExecutor;
use tracedecay_store::runtime::ScopeSetCasOutcomeV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const CAPABILITY: &str = "capability.multi-root.query";
const USE_CASE: &str = "use-case.multi-root.query";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(worktree: &str, suffix: &str) -> RequestContext {
    context_for_actor(worktree, suffix, "actor.requester")
}

fn context_for_actor(worktree: &str, suffix: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>(worktree),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id(&format!("grant.{suffix}")),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new(CAPABILITY).unwrap()]),
        BTreeSet::from([UseCaseId::new(USE_CASE).unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{suffix}")).unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active(format!("cancel.{suffix}")).unwrap(),
    )
    .unwrap()
}

fn scope_set(revision: u64) -> AuthorizedScopeSet {
    scope_set_for_actor(revision, "actor.requester")
}

fn scope_set_for_actor(revision: u64, actor: &str) -> AuthorizedScopeSet {
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new("scope-set.fixture").unwrap(),
        ScopeSetRevision::new(revision).unwrap(),
        vec![
            context_for_actor("worktree.main", &format!("main.{revision}"), actor),
            context_for_actor("worktree.linked", &format!("linked.{revision}"), actor),
        ],
        &CapabilityId::new(CAPABILITY).unwrap(),
        &UseCaseId::new(USE_CASE).unwrap(),
        UtcMicros(10),
    )
    .unwrap()
}

#[test]
fn scope_set_cas_rejects_stale_revision_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scope-sets.db");
    let first = scope_set(1);
    let second = scope_set(2);

    {
        let mut connection = Connection::open(&path).unwrap();
        AuthorizedScopeSetExecutor::install_schema(&connection).unwrap();
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(&mut connection, None, &first).unwrap(),
            ScopeSetCasOutcomeV1::Applied(_)
        ));
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(
                &mut connection,
                Some(ScopeSetRevision::new(1).unwrap()),
                &second,
            )
            .unwrap(),
            ScopeSetCasOutcomeV1::Applied(_)
        ));
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(
                &mut connection,
                Some(ScopeSetRevision::new(1).unwrap()),
                &second,
            )
            .unwrap(),
            ScopeSetCasOutcomeV1::Conflict {
                actual_revision: Some(actual),
                ..
            } if actual == ScopeSetRevision::new(2).unwrap()
        ));
    }

    let reopened = Connection::open(&path).unwrap();
    let restored = AuthorizedScopeSetExecutor::read(&reopened, second.scope_set_id())
        .unwrap()
        .unwrap();
    assert_eq!(restored, second);
}

#[test]
fn scope_set_cas_rejects_cross_actor_update_without_changing_stored_bytes() {
    let mut connection = Connection::open_in_memory().unwrap();
    AuthorizedScopeSetExecutor::install_schema(&connection).unwrap();
    let first = scope_set_for_actor(1, "actor.owner");
    let takeover = scope_set_for_actor(2, "actor.other");
    AuthorizedScopeSetExecutor::compare_and_swap(&mut connection, None, &first).unwrap();
    let before: Vec<u8> = connection
        .query_row(
            "SELECT canonical_payload FROM authorized_scope_sets_v1 WHERE scope_set_id = ?1",
            [first.scope_set_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        AuthorizedScopeSetExecutor::compare_and_swap(
            &mut connection,
            Some(ScopeSetRevision::new(1).unwrap()),
            &takeover,
        )
        .is_err()
    );
    let after: Vec<u8> = connection
        .query_row(
            "SELECT canonical_payload FROM authorized_scope_sets_v1 WHERE scope_set_id = ?1",
            [first.scope_set_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        AuthorizedScopeSetExecutor::read(&connection, first.scope_set_id())
            .unwrap()
            .unwrap(),
        first
    );
}
