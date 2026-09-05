mod common;

use std::collections::BTreeSet;
use std::fmt;

use serde_json::json;
use tracedecay_application::{
    AuthorizedRootAdmission, AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, MultiRootScopeSetCasRequestV1,
    RegisteredRootLocatorV1, RequestContext, RequestId, ResolvedScope, SharedProfileStoreLocatorV1,
};
use tracedecay_domain::{
    ActorId, BrainId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    UserProfileId, UtcMicros, WorktreeId,
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
    context_at("project.fixture", "repository.fixture", worktree, suffix)
}

fn context_at(project: &str, repository: &str, worktree: &str, suffix: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>(repository),
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
    assert_eq!(
        forward.roots()[0].scope().worktree_id.as_str(),
        "worktree.linked"
    );
    assert_eq!(
        forward.roots()[1].scope().worktree_id.as_str(),
        "worktree.main"
    );
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

#[test]
fn local_worktree_ids_are_qualified_by_project_and_repository() {
    let set = authorize(vec![
        context_at(
            "project.alpha",
            "repository.alpha",
            "worktree.local",
            "alpha",
        ),
        context_at("project.beta", "repository.beta", "worktree.local", "beta"),
    ]);

    assert_eq!(set.roots().len(), 2);
    assert_ne!(
        set.roots()[0].scope().scope_digest,
        set.roots()[1].scope().scope_digest
    );
}

#[test]
fn authorized_scope_set_preserves_registered_root_locator() {
    let context = context("worktree.main", "main");
    let locator = RegisteredRootLocatorV1::new(
        context.scope().project_id.clone(),
        SharedProfileStoreLocatorV1::new(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        )
        .unwrap(),
        "/workspace/main".to_owned(),
    )
    .unwrap();
    let set = AuthorizedScopeSetAuthority::authorize_registered(
        ScopeSetId::new("scope-set.registered-root").unwrap(),
        ScopeSetRevision::new(1).unwrap(),
        vec![AuthorizedRootAdmission::new(context, locator.clone()).unwrap()],
        &CapabilityId::new(CAPABILITY).unwrap(),
        &UseCaseId::new(USE_CASE).unwrap(),
        UtcMicros(10),
    )
    .unwrap();

    assert_eq!(set.roots()[0].locator(), Some(&locator));
    assert_eq!(set.roots()[0].scope().project_id, locator.project_id);
}

#[test]
fn registered_roots_share_exact_brain_and_profile_identity() {
    let profile = SharedProfileStoreLocatorV1::new(
        BrainId::new("brain.shared").unwrap(),
        UserProfileId::new("profile.shared").unwrap(),
    )
    .unwrap();
    for (second_profile, admitted) in [
        (profile.clone(), true),
        (
            SharedProfileStoreLocatorV1::new(
                BrainId::new("brain.foreign").unwrap(),
                profile.profile_id.clone(),
            )
            .unwrap(),
            false,
        ),
        (
            SharedProfileStoreLocatorV1::new(
                profile.brain_id.clone(),
                UserProfileId::new("profile.foreign").unwrap(),
            )
            .unwrap(),
            false,
        ),
    ] {
        let roots = [("alpha", profile.clone()), ("beta", second_profile)]
            .into_iter()
            .map(|(suffix, profile)| {
                let context = context_at(
                    &format!("project.{suffix}"),
                    &format!("repository.{suffix}"),
                    &format!("worktree.{suffix}"),
                    suffix,
                );
                let locator = RegisteredRootLocatorV1::new(
                    context.scope().project_id.clone(),
                    profile,
                    format!("/workspace/{suffix}"),
                )
                .unwrap();
                AuthorizedRootAdmission::new(context, locator).unwrap()
            })
            .collect();
        let result = AuthorizedScopeSetAuthority::authorize_registered(
            ScopeSetId::new("scope-set.shared-profile").unwrap(),
            ScopeSetRevision::new(1).unwrap(),
            roots,
            &CapabilityId::new(CAPABILITY).unwrap(),
            &UseCaseId::new(USE_CASE).unwrap(),
            UtcMicros(10),
        );
        if admitted {
            let set = result.expect("distinct projects share the registered Profile shard");
            assert_eq!(set.roots().len(), 2);
            let encoded = serde_json::to_value(&set).unwrap();
            let restored: AuthorizedScopeSet = serde_json::from_value(encoded).unwrap();
            assert_eq!(restored, set);
        } else {
            assert!(
                result.is_err(),
                "foreign brain/profile identity must be denied"
            );
        }
    }
}

#[test]
fn scope_set_cas_selects_exact_registered_roots() {
    let request: MultiRootScopeSetCasRequestV1 = serde_json::from_value(json!({
        "scope_set_id": "scope-set.exact-roots",
        "expected_revision": null,
        "roots": [
            {
                "project_id": "project.same",
                "root": "/workspace/linked"
            },
            {
                "project_id": "project.same",
                "root": "/workspace/main"
            }
        ]
    }))
    .expect("exact registered root selectors");
    request.validate().expect("canonical exact root order");

    let encoded = serde_json::to_value(request).expect("serialize selector");
    assert_eq!(encoded["roots"][0]["root"], "/workspace/linked");
    assert_eq!(encoded["roots"][1]["root"], "/workspace/main");
    assert!(encoded.get("project_ids").is_none());
}
