//! Regression gate for the federated workspace authority pairing.
//!
//! `0a9ebc97a` gave every federated root the one shared profile-store locator,
//! and `8578e46eb` stopped pairing the workspace's factories with the
//! authorized scope set by list position. The two lists hold the same roots in
//! different orders — factories in `canonicalize_lsp_roots` scope-digest order,
//! the scope set in its own (project, repository, worktree, reference,
//! canonical root) order — so the old `zip` attributed one root's factory to
//! another root's locator and refused a workspace whose every root was in fact
//! its own owner, for whichever id orderings happened to disagree.
//!
//! The fixture below *proves* the two orders disagree before it asserts
//! anything, so the admitted case cannot pass vacuously, and it keeps the
//! negatives the shared locator must never substitute for: a factory the
//! current owner does not hold, and a workspace root the authorized set does
//! not name.

use std::collections::BTreeSet;
use std::path::PathBuf;

use tracedecay_application::{
    CapabilityGrantId, DisclosureClass, RegisteredRootLocatorV1, ResolvedScope,
};
use tracedecay_domain::{
    ProjectId, RefId, RepositoryId, UserProfileId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_lsp::{AdmittedRoot, AuthorizedLspWorkspace};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::super::tests::unavailable_lsp_session_factory;
use super::super::types::DaemonLspInvocationOwner;
use super::super::{CapabilityGrantSnapshot, DaemonInvocationService};
use super::workspace_admission::CurrentLspWorkspaceAuthorityV1;

const PROFILE: &str = "profile.federated-pairing";
const SHARED_STORE: &str = "store.federated-pairing.profile-sharded";

fn scope_for(ordinal: usize) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.federated-pairing.{ordinal}")).expect("project id"),
        RepositoryId::new("repository.federated-pairing").expect("repository id"),
        WorktreeId::new(format!("worktree.federated-pairing.{ordinal}")).expect("worktree id"),
        Some(RefId::new("refs/heads/main").expect("reference")),
    )
    .expect("resolved scope")
}

/// Two scopes whose canonical scope-set order (by project id) is the reverse of
/// their scope-digest order. Searched rather than hard-coded so the pair stays
/// valid if either ordering rule changes; the search is over a fixed list, so
/// it is deterministic.
fn disagreeing_scope_pair() -> (ResolvedScope, ResolvedScope) {
    let candidates = (0..24).map(scope_for).collect::<Vec<_>>();
    for (index, left) in candidates.iter().enumerate() {
        for right in &candidates[index + 1..] {
            let by_project = left.project_id.as_str() < right.project_id.as_str();
            let by_digest = left.scope_digest < right.scope_digest;
            if by_project != by_digest {
                return (left.clone(), right.clone());
            }
        }
    }
    panic!("no candidate pair orders the scope set and the factories differently");
}

fn grant(scope: &ResolvedScope, suffix: &str) -> CapabilityGrantSnapshot {
    CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.federated-pairing.{suffix}")).expect("grant id"),
        1,
        canonical_sha256(&("grant.federated-pairing", suffix)).expect("grant digest"),
        tracedecay_domain::ActorId::new("actor.federated-pairing").expect("actor"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([
            CapabilityId::new(super::LSP_WORKSPACE_CAPABILITY_ID_V1).expect("capability"),
        ]),
        BTreeSet::from([UseCaseId::new(super::LSP_WORKSPACE_USE_CASE_ID_V1).expect("use case")]),
        DisclosureClass::Sensitive,
    )
    .expect("scope grant")
}

async fn install_root(
    service: &DaemonInvocationService,
    scope: &ResolvedScope,
    suffix: &str,
) -> (PathBuf, String, ResolvedScope, RegisteredRootLocatorV1) {
    let project_root = PathBuf::from(format!("/federated-pairing-{suffix}"));
    let uri = format!("file:///federated-pairing-{suffix}");
    service
        .install_lsp_owner(
            project_root.clone(),
            DaemonLspInvocationOwner::for_test_project(
                unavailable_lsp_session_factory(),
                UserProfileId::new(PROFILE).expect("profile id"),
                scope.project_id.clone(),
                project_root.clone(),
            )
            .with_scope_grant(grant(scope, suffix)),
        )
        .await
        .expect("install the LSP owner");
    // Both roots resolve through the one shared profile store (`0a9ebc97a`);
    // a per-project store id here would fail the scope set closed.
    let locator = RegisteredRootLocatorV1::new(
        scope.project_id.clone(),
        UserProfileId::new(PROFILE).expect("profile id"),
        SHARED_STORE.to_owned(),
        project_root.clone(),
    )
    .expect("registered root locator");
    (project_root, uri, scope.clone(), locator)
}

#[tokio::test]
async fn federated_workspace_authority_pairs_by_scope_digest_not_list_position() {
    let service = DaemonInvocationService::default();
    let (left_scope, right_scope) = disagreeing_scope_pair();
    let left = install_root(&service, &left_scope, "left").await;
    let right = install_root(&service, &right_scope, "right").await;

    let workspace = service
        .authorize_lsp_workspace(vec![left.clone(), right.clone()], UtcMicros(10))
        .await
        .expect("two registered roots under one profile store are a federated workspace");
    let digest = workspace
        .scope_set_digest()
        .expect("a federated workspace carries a scope-set digest")
        .clone();
    let authorized = service
        .authorized_lsp_workspaces
        .lock()
        .await
        .get(&digest)
        .cloned()
        .expect("the authorized workspace is retained by digest");

    // The fixture is only meaningful while the two lists disagree positionally.
    let factory_order = authorized
        .factories
        .iter()
        .map(|(root, _)| root.scope_digest().expect("admitted root digest").clone())
        .collect::<Vec<_>>();
    let scope_set_order = authorized
        .scope_set
        .roots()
        .iter()
        .map(|root| root.scope().scope_digest.clone())
        .collect::<Vec<_>>();
    assert_eq!(factory_order.len(), 2);
    assert_ne!(
        factory_order, scope_set_order,
        "this gate only proves the pairing while the factory and scope-set \
         orders disagree"
    );

    let owner = service
        .lsp_owner(Some(&left.0))
        .await
        .expect("the left root's owner");
    assert!(
        matches!(
            service
                .current_lsp_workspace_authority(&workspace, Some(&owner))
                .await,
            Some(CurrentLspWorkspaceAuthorityV1::Federated(_))
        ),
        "every root is its own owner, so the workspace must stay authorized \
         regardless of the two lists' orders"
    );

    // The shared profile-store locator never replaces per-root authorization:
    // an expected owner holding a factory this workspace does not is refused.
    let substituted = DaemonLspInvocationOwner::for_test_project(
        unavailable_lsp_session_factory(),
        UserProfileId::new(PROFILE).expect("profile id"),
        left_scope.project_id.clone(),
        left.0.clone(),
    )
    .with_scope_grant(grant(&left_scope, "left"));
    assert!(
        service
            .current_lsp_workspace_authority(&workspace, Some(&substituted))
            .await
            .is_none(),
        "a substituted session factory must not be admitted by the shared \
         profile store locator"
    );

    // A workspace root the authorized scope set does not name has no locator
    // to pair with, so the whole workspace is refused rather than partially
    // admitted.
    let unnamed = AdmittedRoot::authorized(
        "file:///federated-pairing-unnamed".to_owned(),
        scope_for(999).scope_digest,
    );
    let mut widened = authorized.clone();
    widened.factories.push((
        unnamed.clone(),
        service
            .lsp_owner(Some(&right.0))
            .await
            .expect("the right root's owner")
            .factory(),
    ));
    let widened_workspace = AuthorizedLspWorkspace::new(
        Some(digest.clone()),
        widened
            .factories
            .iter()
            .map(|(root, _)| root.clone())
            .collect(),
    )
    .expect("a workspace naming one more root");
    service
        .authorized_lsp_workspaces
        .lock()
        .await
        .insert(digest, widened);
    assert!(
        service
            .current_lsp_workspace_authority(&widened_workspace, None)
            .await
            .is_none(),
        "a root missing from the authorized scope set must refuse the workspace"
    );
}
