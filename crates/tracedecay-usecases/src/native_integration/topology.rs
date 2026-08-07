//! Exact-pair native-integration topology resolution.
//!
//! Plan 36 lets a caller freeze either an explicit independent-branch proposal
//! or an exact visible declared stack edge. This resolver owns the first case
//! only: it proves the authorized source/destination pair against native Git
//! and freezes it into the immutable domain selection.
//!
//! Declared stack edges resolve to `Unavailable` here. Plan 16's branch-stack
//! projection is the authority for edge meaning, visibility, and readiness, and
//! it is not bound to this resolver; answering `Unavailable` keeps that honest
//! while disclosing no node identity, count, or topology — which is exactly
//! what Plan 36 requires of a partially visible stack.
//!
//! Branch names, paths, provider order, and graph proximity never select or
//! infer the pair: the enrolled repository identity is supplied at trusted
//! composition and the refs come from the already-authorized scope.

use std::path::Path;

use tracedecay_application::{
    CancellationSignal, NativeIntegrationPortError, NativeIntegrationSelectionBindingV1,
    NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionPort,
    NativeIntegrationStackResolutionRequestV1,
};
use tracedecay_domain::{
    FrozenIndependentBranchSelectionV1, GitHeadStateV1, GitOidV1, NativeIntegrationSelectionV1,
    ProjectId, RefId, RepositoryId,
};
use tracedecay_runtime_core::git_repository::GitRepositoryAuthority;

use super::{domain_error, native_error};

/// One enrolled repository's exact-pair resolver.
///
/// The root is supplied only at trusted composition; no request field can
/// replace it, so a caller cannot redirect resolution at another repository.
pub struct ExactPairNativeIntegrationTopology {
    project_id: ProjectId,
    repository_id: RepositoryId,
    repository: GitRepositoryAuthority,
}

impl ExactPairNativeIntegrationTopology {
    pub fn open(
        project_id: ProjectId,
        repository_id: RepositoryId,
        enrolled_repository_root: &Path,
    ) -> Result<Self, NativeIntegrationPortError> {
        project_id.validate().map_err(domain_error)?;
        repository_id.validate().map_err(domain_error)?;
        let repository =
            GitRepositoryAuthority::discover(enrolled_repository_root).map_err(native_error)?;
        Ok(Self {
            project_id,
            repository_id,
            repository,
        })
    }

    /// Whether this repository's HEAD is attached to `reference`.
    ///
    /// A destination that is checked out cannot take a plain compare-and-swap
    /// ref update, so the preview must know. `GitHeadStateV1::Attached` carries
    /// the branch as Git spells it, which may be the full ref or its
    /// `refs/heads/` short form; both are accepted, and nothing else is
    /// treated as a match.
    fn head_occupies(&self, reference: &RefId) -> Result<bool, NativeIntegrationPortError> {
        let status = self.repository.status().map_err(native_error)?;
        let GitHeadStateV1::Attached { branch, .. } = &status.head else {
            // Detached and unborn heads occupy no branch, so neither can hold
            // the destination ref.
            return Ok(false);
        };
        let reference = reference.as_str();
        Ok(branch == reference
            || reference
                .strip_prefix("refs/heads/")
                .is_some_and(|short| branch == short))
    }

    fn tip(&self, reference: &RefId) -> Result<Option<GitOidV1>, NativeIntegrationPortError> {
        // A ref this repository cannot resolve is missing evidence, not a
        // failure of the resolver: the caller still gets useful read-only
        // partial state and apply stays blocked.
        Ok(self.repository.exact_reference_tip(reference.as_str()).ok())
    }
}

impl NativeIntegrationStackResolutionPort for ExactPairNativeIntegrationTopology {
    fn resolve(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError> {
        if cancellation.is_cancelled() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        if request.validate().is_err() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }

        let NativeIntegrationSelectionBindingV1::IndependentBranch { proposal_digest } =
            &request.selection
        else {
            // Plan 16 owns declared-edge authorization and visibility. Without
            // that authority bound, no edge is resolvable and no topology is
            // disclosed.
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };

        // The authorized scope must name the repository this resolver was
        // enrolled for. A mismatch is denied without revealing whether the
        // named target exists.
        if request.destination.project_id != self.project_id
            || request.destination.repository_id != self.repository_id
            || request.source.project_id != self.project_id
            || request.source.repository_id != self.repository_id
        {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Denied);
        }

        // `validate` already proved both references are present and distinct;
        // treat their absence as unresolvable rather than unwrapping.
        let (Some(source_ref), Some(destination_ref)) = (
            request.source.reference.as_ref(),
            request.destination.reference.as_ref(),
        ) else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };

        let (Some(source_tip), Some(destination_tip)) =
            (self.tip(source_ref)?, self.tip(destination_ref)?)
        else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Partial);
        };

        // Occupancy is observed, never assumed from the caller's scope: the
        // scope's worktree id names where the *request* came from, which is not
        // evidence about where a ref is checked out.
        let source_worktree_id = self
            .head_occupies(source_ref)?
            .then(|| request.source.worktree_id.clone());
        let destination_worktree_id = self
            .head_occupies(destination_ref)?
            .then(|| request.destination.worktree_id.clone());

        let selection = FrozenIndependentBranchSelectionV1::new(
            self.project_id.clone(),
            self.repository_id.clone(),
            request.inventory_snapshot_id.clone(),
            request.inventory_epoch,
            source_worktree_id,
            destination_worktree_id,
            source_ref.clone(),
            destination_ref.clone(),
            source_tip,
            destination_tip,
            proposal_digest.clone(),
            request.observed_at,
        )
        .map_err(domain_error)?;

        Ok(NativeIntegrationStackResolutionOutcomeV1::Complete(
            Box::new(NativeIntegrationSelectionV1::IndependentBranch(selection)),
        ))
    }
}
