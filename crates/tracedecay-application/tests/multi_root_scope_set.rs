use std::collections::BTreeSet;
use std::fmt;

use tracedecay_application::{
    AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    UtcMicros, WorktreeId,
};
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
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new(format!("request.{suffix}")).unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active(format!("cancel.{suffix}")).unwrap(),
    )
    .unwrap()
}

fn authorize(contexts: Vec<RequestContext>) -> AuthorizedScopeSet {
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new("scope-set.fixture").unwrap(),
        ScopeSetRevision::new(1).unwrap(),
        contexts,
        &CapabilityId::new(CAPABILITY).unwrap(),
        &UseCaseId::new(USE_CASE).unwrap(),
        UtcMicros(10),
    )
    .unwrap()
}

#[test]
fn authorized_scope_set_canonicalizes_two_exact_linked_worktrees() {
    let main = context("worktree.main", "main");
    let linked = context("worktree.linked", "linked");

    let forward = authorize(vec![main.clone(), linked.clone()]);
    let reverse = authorize(vec![linked, main]);

    assert_eq!(forward.digest(), reverse.digest());
    assert_eq!(forward.roots(), reverse.roots());
    assert_eq!(forward.actor_id().as_str(), "actor.requester");
    assert_eq!(forward.roots().len(), 2);
    assert_eq!(forward.roots()[0].worktree_id.as_str(), "worktree.linked");
    assert_eq!(forward.roots()[1].worktree_id.as_str(), "worktree.main");
}

#[test]
fn scope_set_digest_and_deserialization_reject_identity_drift() {
    let set = authorize(vec![
        context("worktree.main", "main"),
        context("worktree.linked", "linked"),
    ]);
    let mut wire = serde_json::to_value(&set).unwrap();
    wire["roots"][1]["worktree_id"] = serde_json::json!("worktree.alias");

    assert!(serde_json::from_value::<AuthorizedScopeSet>(wire).is_err());

    let mut actor_drift = serde_json::to_value(&set).unwrap();
    actor_drift["actor_id"] = serde_json::json!("actor.other");
    assert!(serde_json::from_value::<AuthorizedScopeSet>(actor_drift).is_err());
}
