use std::sync::Arc;

use tracedecay_application::{RequestContext, ResolvedScope};
use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
use tracedecay_usecases::context::{
    BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_usecases::session::SessionTemporalQuery;

use super::MountedProjectApplicationRetrievalV1;

struct DeniedSessionRetrieval;

impl crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1
    for DeniedSessionRetrieval
{
    fn retrieve_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _query: SessionTemporalQuery,
    ) -> crate::daemon::session_retrieval::SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async { crate::daemon::session_retrieval::SessionRetrievalServiceOutcome::Denied })
    }
}

struct MissingFederatedAuthority;

impl crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1
    for MissingFederatedAuthority
{
    fn authority_for<'a>(
        &'a self,
        _scope: &'a ResolvedScope,
    ) -> crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityFutureV1<'a> {
        Box::pin(async { None })
    }
}

fn mounted_scope(project: &str) -> (MountedProjectApplicationRetrievalV1, ResolvedScope) {
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.work-evidence-mount").unwrap(),
        ProjectId::new(project).unwrap(),
        SessionStoreId::new("store.work-evidence-mount").unwrap(),
        SessionRootId::new("root.work-evidence-mount").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.work-evidence-mount").unwrap(),
            WorktreeId::new("worktree.work-evidence-mount").unwrap(),
            BranchId::new("branch.work-evidence-mount").unwrap(),
        ),
    );
    let scope = identity.session_request_scope().unwrap();
    (
        MountedProjectApplicationRetrievalV1 {
            identity,
            service: Arc::new(DeniedSessionRetrieval),
        },
        scope,
    )
}

#[test]
fn concrete_work_evidence_mount_accepts_only_its_exact_project_scope() {
    let (mounted, exact_scope) = mounted_scope("project.work-evidence-mount");
    let federated = Arc::new(MissingFederatedAuthority);

    let first = mounted
        .work_evidence_retrieval(&exact_scope, federated.clone())
        .expect("exact project scope must bind the concrete evidence adapter");
    let second = mounted
        .work_evidence_retrieval(&exact_scope, federated)
        .expect("the same concrete authority must be reusable");
    assert!(first.same_authority(&second));

    let (_, foreign_scope) = mounted_scope("project.work-evidence-foreign");
    assert!(
        mounted
            .work_evidence_retrieval(&foreign_scope, Arc::new(MissingFederatedAuthority))
            .is_err(),
        "a different project scope must not receive the mounted session authority",
    );
}

/// The mounted identity carries the branch the graph scope was *registered*
/// under, while a live request carries whatever branch HEAD is on now. The
/// branch label is not checkout identity: a checkout that switched branches
/// must keep its work-evidence authority, or project open degrades and every
/// automation task behind the retained runtime registration stops running.
#[test]
fn concrete_work_evidence_mount_accepts_a_moved_branch_reference() {
    let (mounted, exact_scope) = mounted_scope("project.work-evidence-mount");
    let moved_branch = ResolvedScope::new(
        exact_scope.project_id,
        exact_scope.repository_id,
        exact_scope.worktree_id,
        Some(tracedecay_domain::RefId::new("refs/heads/branch.after-switch").unwrap()),
    )
    .unwrap();

    mounted
        .work_evidence_retrieval(&moved_branch, Arc::new(MissingFederatedAuthority))
        .expect("a moved HEAD branch must not cost the checkout its mounted session authority");
}

#[test]
fn concrete_work_evidence_mount_accepts_reference_free_matching_coordinates() {
    let (mounted, exact_scope) = mounted_scope("project.work-evidence-mount");
    let reference_free = ResolvedScope::new(
        exact_scope.project_id,
        exact_scope.repository_id,
        exact_scope.worktree_id,
        None,
    )
    .unwrap();

    mounted
        .work_evidence_retrieval(&reference_free, Arc::new(MissingFederatedAuthority))
        .expect("a non-git scope must bind by its exact project and worktree coordinates");
}
