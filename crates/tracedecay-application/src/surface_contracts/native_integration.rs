//! Canonical native-integration and native-worktree journey requests.
//!
//! Each variant carries exact typed identity. There is no variant that can
//! express a path, a raw SHA, a commit message, a patch, a Git argument, or a
//! remote, so no transport can widen this journey into generic Git execution.

use serde::{Deserialize, Serialize};

use crate::git::{
    NativeIntegrationApplySurfaceRequest, NativeIntegrationApproveSurfaceRequest,
    NativeIntegrationCancelSurfaceRequest, NativeIntegrationPreflightSurfaceRequest,
    NativeIntegrationStackSnapshotSurfaceRequest, NativeIntegrationStatusSurfaceRequest,
    NativeWorktreeSurfaceRequest,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum NativeIntegrationSurfaceRequest {
    StackSnapshot(Box<NativeIntegrationStackSnapshotSurfaceRequest>),
    Preflight(Box<NativeIntegrationPreflightSurfaceRequest>),
    Approve(NativeIntegrationApproveSurfaceRequest),
    Apply(NativeIntegrationApplySurfaceRequest),
    Status(NativeIntegrationStatusSurfaceRequest),
    Cancel(NativeIntegrationCancelSurfaceRequest),
    Worktree(NativeWorktreeSurfaceRequest),
}
