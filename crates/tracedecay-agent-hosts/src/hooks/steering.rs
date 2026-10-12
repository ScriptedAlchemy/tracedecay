//! Shared workspace steering signals.

/// Index status line for a code workspace with no tracedecay project index.
pub(super) const UNINDEXED_STATUS_LINE: &str = "tracedecay index status: no project index found in this workspace. \
     run `tracedecay init` to enable tracedecay MCP tools.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookWorkspaceStatus {
    Initialized,
    UnindexedProject,
    Generic,
}

impl HookWorkspaceStatus {
    pub(super) fn as_key(self) -> &'static str {
        match self {
            HookWorkspaceStatus::Initialized => "initialized",
            HookWorkspaceStatus::UnindexedProject => "unindexed_project",
            HookWorkspaceStatus::Generic => "generic",
        }
    }
}
