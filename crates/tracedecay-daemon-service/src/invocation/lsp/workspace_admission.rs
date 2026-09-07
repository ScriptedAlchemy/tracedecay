use std::path::PathBuf;

use tracedecay_application::{
    AuthorizedRoot, AuthorizedRootAdmission, AuthorizedScopeSetAuthority, CancellationContext,
    Deadline, RegisteredRootLocatorV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_daemon_protocol::MAX_LSP_WORKSPACE_ROOTS;
use tracedecay_domain::{ScopeSetId, ScopeSetRevision, UtcMicros, canonical_sha256};
use tracedecay_lsp::{AdmittedRoot, AuthorizedLspWorkspace};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{
    AuthorizedDaemonLspWorkspace, DaemonInvocationService, DaemonLspInvocationOwner,
    canonicalize_lsp_roots,
};

pub(super) enum CurrentLspWorkspaceAuthorityV1 {
    Single,
    Federated(AuthorizedDaemonLspWorkspace),
}

impl DaemonInvocationService {
    #[hotpath::measure(label = "daemon.service.lsp.workspace_authority", future = true)]
    pub(super) async fn current_lsp_workspace_authority(
        &self,
        workspace: &AuthorizedLspWorkspace,
        expected_owner: Option<&DaemonLspInvocationOwner>,
    ) -> Option<CurrentLspWorkspaceAuthorityV1> {
        let Some(digest) = workspace.scope_set_digest() else {
            let [root] = workspace.roots() else {
                return None;
            };
            let path = url::Url::parse(root.uri()).ok()?.to_file_path().ok()?;
            let current_owner = self.lsp_owner(Some(&path)).await?;
            if !current_owner.project_identity.matches_project_root(&path)
                || expected_owner.is_some_and(|expected| {
                    expected.project_identity != current_owner.project_identity
                        || !std::sync::Arc::ptr_eq(&current_owner.factory, &expected.factory)
                })
            {
                return None;
            }
            return Some(CurrentLspWorkspaceAuthorityV1::Single);
        };
        let authorized = self
            .authorized_lsp_workspaces
            .lock()
            .await
            .get(digest)
            .cloned()?;
        if !authorized
            .factories
            .iter()
            .map(|(root, _)| root)
            .eq(workspace.roots())
        {
            return None;
        }
        if authorized.scope_set.roots().len() != authorized.factories.len() {
            return None;
        }
        // The factories keep the workspace's own scope-digest order while
        // `AuthorizedScopeSet` canonicalizes its roots by scope identity
        // (project, repository, worktree, reference, canonical root). The two
        // lists therefore agree on membership but not on position, so pairing
        // them by index attributed a factory to another root's locator and
        // refused a workspace whose every root was in fact its own owner —
        // for whichever root orderings happened to disagree. Pair on the exact
        // scope digest both sides carry instead.
        let locator_for = |root: &AdmittedRoot| -> Option<&RegisteredRootLocatorV1> {
            let scope_digest = root.scope_digest()?;
            authorized
                .scope_set
                .roots()
                .iter()
                .find(|candidate| candidate.scope().scope_digest == *scope_digest)
                .and_then(AuthorizedRoot::locator)
        };
        if expected_owner.is_some_and(|expected| {
            !authorized.factories.iter().any(|(root, factory)| {
                locator_for(root).is_some_and(|locator| {
                    expected.project_identity.matches_locator(locator)
                        && std::sync::Arc::ptr_eq(factory, &expected.factory)
                })
            })
        }) {
            return None;
        }
        for (root, factory) in &authorized.factories {
            let locator = locator_for(root)?;
            let path = url::Url::parse(root.uri()).ok()?.to_file_path().ok()?;
            let current_owner = self.lsp_owner(Some(&path)).await?;
            if !current_owner.project_identity.matches_locator(locator)
                || !std::sync::Arc::ptr_eq(&current_owner.factory, factory)
            {
                return None;
            }
        }
        Some(CurrentLspWorkspaceAuthorityV1::Federated(authorized))
    }

    #[hotpath::measure(label = "daemon.service.lsp.authorize_workspace", future = true)]
    pub async fn authorize_lsp_workspace(
        &self,
        mut roots: Vec<(PathBuf, String, ResolvedScope, RegisteredRootLocatorV1)>,
        observed_at: UtcMicros,
    ) -> Option<AuthorizedLspWorkspace> {
        let admission_guard = self.lsp_admission_open.lock().await;
        if !*admission_guard {
            return None;
        }
        if roots.is_empty() || roots.len() > MAX_LSP_WORKSPACE_ROOTS {
            return None;
        }
        if !canonicalize_lsp_roots(&mut roots) {
            return None;
        }
        if let [(project_root, uri, scope, _locator)] = roots.as_slice() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            return Some(AuthorizedLspWorkspace::single(AdmittedRoot::authorized(
                uri.clone(),
                scope.scope_digest.clone(),
            )));
        }
        self.authorize_federated_lsp_workspace(&roots, observed_at)
            .await
    }

    #[hotpath::measure(label = "daemon.service.lsp.authorize_federated", future = true)]
    async fn authorize_federated_lsp_workspace(
        &self,
        roots: &[(PathBuf, String, ResolvedScope, RegisteredRootLocatorV1)],
        observed_at: UtcMicros,
    ) -> Option<AuthorizedLspWorkspace> {
        let selector_digest = canonical_sha256(&(
            "tracedecay.daemon.lsp-workspace-selector.v1",
            roots
                .iter()
                .map(|(_, _, scope, _)| &scope.scope_digest)
                .collect::<Vec<_>>(),
        ))
        .ok()?;
        let scope_set_id = ScopeSetId::new(format!(
            "scope-set.lsp.{}",
            selector_digest.as_str().trim_start_matches("sha256:")
        ))
        .ok()?;
        let capability = CapabilityId::new(super::LSP_WORKSPACE_CAPABILITY_ID_V1).ok()?;
        let use_case = UseCaseId::new(super::LSP_WORKSPACE_USE_CASE_ID_V1).ok()?;
        let mut admissions = Vec::with_capacity(roots.len());
        let mut factories = Vec::with_capacity(roots.len());
        let mut admitted = Vec::with_capacity(roots.len());
        for (ordinal, (project_root, uri, scope, locator)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            let context = RequestContext::new(
                grant.issuer.clone(),
                scope.clone(),
                grant,
                RequestId::new(format!("request.lsp-workspace.admit.{ordinal}")).ok()?,
                Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000))).ok()?,
                CancellationContext::active(format!("cancel.lsp-workspace.admit.{ordinal}"))
                    .ok()?,
            )
            .ok()?;
            admissions.push(AuthorizedRootAdmission::new(context, locator.clone()).ok()?);
            let root = AdmittedRoot::authorized(uri.clone(), scope.scope_digest.clone());
            factories.push((root.clone(), owner.factory.clone()));
            admitted.push(root);
        }
        // Workspace-folder admission is an in-memory session boundary, not a
        // saved scope-set mutation. Persisting the same synthetic selector in
        // every participating project would create partial visibility outside
        // the daemon-owned coordinator/recovery path.
        let scope_set = AuthorizedScopeSetAuthority::authorize_registered(
            scope_set_id,
            ScopeSetRevision::new(1).ok()?,
            admissions,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        let digest = scope_set.digest().clone();
        let workspace = AuthorizedLspWorkspace::new(Some(digest.clone()), admitted).ok()?;
        self.authorized_lsp_workspaces.lock().await.insert(
            digest,
            AuthorizedDaemonLspWorkspace {
                scope_set,
                factories,
            },
        );
        Some(workspace)
    }
}
