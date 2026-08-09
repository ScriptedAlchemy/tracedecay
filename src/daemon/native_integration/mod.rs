//! Daemon composition of the Plan 36 native-integration authority.
//!
//! `registry` retains one composed transaction coordinator per exact
//! project/repository identity; `store` bridges the synchronous
//! `tracedecay-store` contract onto the async registered session database
//! through one bounded actor per database.

mod registry;
mod stack_hook_wakeup;
mod stack_runtime;
pub(crate) mod stack_signals;
mod store;
mod worktree;
mod worktree_cleanup;

#[cfg(test)]
mod journey_tests;

pub(crate) use registry::{DaemonNativeIntegrationOwner, DaemonNativeIntegrationServiceRegistry};
pub(crate) use stack_hook_wakeup::{
    github_stack_hook_available, register_github_stack_hook_runtime,
};
pub(crate) use stack_runtime::DaemonGitHubStackRuntimeV1;
pub(crate) use worktree::{
    DaemonAuthorizedScopeSetReader, DaemonNativeWorktreeAuthority, WorktreeHolderAdmissionFenceV1,
    daemon_worktree_holder_admission_fence,
};
