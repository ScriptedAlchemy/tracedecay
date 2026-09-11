//! Exact caller proof for freezing one native-integration stack selection.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, ManifestDigest, NativeIntegrationDirectionV1,
    RefId, ScopeSetId, ScopeSetRevision, StackNodeId, UtcMicros, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId,
};

use crate::error::ApplicationContractError;
use crate::git::native_integration::{
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionRequestV1,
};
use crate::{AuthorizedScopeSet, ResolvedScope};

/// Explicit caller declaration for one native-integration selection.
///
/// Declared stacks contain the visible nodes and edges, while the daemon
/// derives canonical order and the revision digest through the domain
/// constructor. Callers cannot submit either derived field.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "binding",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeIntegrationSelectionDeclarationV1 {
    DeclaredStackEdge {
        stack_id: BranchStackId,
        revision_id: BranchStackRevisionId,
        nodes: Vec<BranchStackNodeV1>,
        edges: Vec<BranchStackEdgeV1>,
        source_node_id: StackNodeId,
        destination_node_id: StackNodeId,
        direction: NativeIntegrationDirectionV1,
    },
    IndependentBranch {
        proposal_digest: ManifestDigest,
        source_ref: RefId,
        destination_ref: RefId,
    },
}

impl NativeIntegrationSelectionDeclarationV1 {
    fn seal(
        self,
        inventory_snapshot_id: WorktreeInventorySnapshotId,
        inventory_epoch: WorktreeInventoryEpoch,
    ) -> Result<NativeIntegrationSelectionBindingV1, ApplicationContractError> {
        Ok(match self {
            Self::DeclaredStackEdge {
                stack_id,
                revision_id,
                nodes,
                edges,
                source_node_id,
                destination_node_id,
                direction,
            } => {
                let revision = BranchStackRevisionV1::new(
                    stack_id,
                    revision_id,
                    inventory_snapshot_id,
                    inventory_epoch,
                    BranchStackSourceV1::ExplicitDeclaration,
                    nodes,
                    edges,
                )?;
                NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                    stack_id: revision.stack_id.clone(),
                    revision_id: revision.revision_id.clone(),
                    revision_digest: revision.digest.clone(),
                    declared_revision: Box::new(revision),
                    source_node_id,
                    destination_node_id,
                    direction,
                }
            }
            Self::IndependentBranch {
                proposal_digest,
                source_ref,
                destination_ref,
            } => NativeIntegrationSelectionBindingV1::IndependentBranch {
                proposal_digest,
                source_ref,
                destination_ref,
            },
        })
    }
}

/// Exact caller-supplied identity frozen by `stack_snapshot`.
///
/// This proof binds the exact authorized `ProjectId`, `RepositoryId`, source
/// and destination worktree/ref identity, frozen inventory, scope/grant/policy
/// revisions, and one declared-edge or independent-branch selection. Paths,
/// free-form SHA values, branch display names, and provider topology remain
/// unrepresentable.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationStackSnapshotSurfaceRequest {
    pub source: ResolvedScope,
    pub destination: ResolvedScope,
    pub authorized_scope_set_id: ScopeSetId,
    pub authorized_scope_set_revision: ScopeSetRevision,
    pub authorized_scope_set_digest: ManifestDigest,
    pub inventory_snapshot_id: WorktreeInventorySnapshotId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub selection: NativeIntegrationSelectionDeclarationV1,
    pub grant_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
}

impl NativeIntegrationStackSnapshotSurfaceRequest {
    /// Seal caller declaration content into the exact proof returned by
    /// `stack_snapshot` and accepted by preflight.
    pub fn seal(self) -> Result<NativeIntegrationSealedStackSnapshotV1, ApplicationContractError> {
        let selection = self
            .selection
            .seal(self.inventory_snapshot_id.clone(), self.inventory_epoch)?;
        Ok(NativeIntegrationSealedStackSnapshotV1 {
            source: self.source,
            destination: self.destination,
            authorized_scope_set_id: self.authorized_scope_set_id,
            authorized_scope_set_revision: self.authorized_scope_set_revision,
            authorized_scope_set_digest: self.authorized_scope_set_digest,
            inventory_snapshot_id: self.inventory_snapshot_id,
            inventory_epoch: self.inventory_epoch,
            selection,
            grant_digest: self.grant_digest,
            policy_digest: self.policy_digest,
        })
    }
}

/// Canonical stack-snapshot proof returned by `stack_snapshot` and consumed
/// verbatim by preflight.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationSealedStackSnapshotV1 {
    pub source: ResolvedScope,
    pub destination: ResolvedScope,
    pub authorized_scope_set_id: ScopeSetId,
    pub authorized_scope_set_revision: ScopeSetRevision,
    pub authorized_scope_set_digest: ManifestDigest,
    pub inventory_snapshot_id: WorktreeInventorySnapshotId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub selection: NativeIntegrationSelectionBindingV1,
    pub grant_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
}

impl NativeIntegrationSealedStackSnapshotV1 {
    /// Bind the sealed proof to the registered scope set. `observed_at` is
    /// minted by the daemon, never by the caller.
    pub fn into_resolution_request(
        self,
        authorized_scope_set: AuthorizedScopeSet,
        observed_at: UtcMicros,
    ) -> Result<NativeIntegrationStackResolutionRequestV1, ApplicationContractError> {
        if authorized_scope_set.scope_set_id() != &self.authorized_scope_set_id
            || authorized_scope_set.revision() != self.authorized_scope_set_revision
            || authorized_scope_set.digest() != &self.authorized_scope_set_digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration registered scope set",
            });
        }
        Ok(NativeIntegrationStackResolutionRequestV1 {
            source: self.source,
            destination: self.destination,
            authorized_scope_set,
            inventory_snapshot_id: self.inventory_snapshot_id,
            inventory_epoch: self.inventory_epoch,
            selection: self.selection,
            grant_digest: self.grant_digest,
            policy_digest: self.policy_digest,
            observed_at,
        })
    }
}
