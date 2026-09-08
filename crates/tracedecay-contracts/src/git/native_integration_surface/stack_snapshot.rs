//! Exact caller proof for freezing one native-integration stack selection.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, ScopeSetId, ScopeSetRevision, UtcMicros, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId,
};

use crate::error::ApplicationContractError;
use crate::git::native_integration::{
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionRequestV1,
};
use crate::{AuthorizedScopeSet, ResolvedScope};

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
    pub selection: NativeIntegrationSelectionBindingV1,
    pub grant_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
}

impl NativeIntegrationStackSnapshotSurfaceRequest {
    /// Bind the caller-visible proof to the exact topology request the
    /// resolution authority accepts. `observed_at` is minted by the daemon,
    /// never by the caller.
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
