//! Shared workspace steering signals.

use std::path::Path;

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

/// The skill mandate a skill-loading host reads at session start.
const SESSION_START_MANDATE: &str =
    include_str!("../../../../plugin/hooks/session-start-context.md");

/// Session-start context: the skill mandate, then any Context Scout guidance
/// the daemon released. A session outside a registered project gets only the
/// guidance, because TraceDecay skills read that project's index.
pub(super) fn session_start_context(
    project_root: Option<&Path>,
    guidance: Option<String>,
) -> Option<String> {
    if project_root.is_none() {
        return guidance;
    }
    Some(match guidance {
        Some(guidance) => format!("{SESSION_START_MANDATE}\n{guidance}"),
        None => SESSION_START_MANDATE.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{SESSION_START_MANDATE, session_start_context};

    #[test]
    fn registered_session_start_leads_with_the_skill_mandate() {
        let root = Some(Path::new("/workspace/project"));
        assert_eq!(
            session_start_context(root, None).as_deref(),
            Some(SESSION_START_MANDATE)
        );
        let composed = session_start_context(root, Some("scout guidance".to_owned())).unwrap();
        assert_eq!(composed, format!("{SESSION_START_MANDATE}\nscout guidance"));
    }

    #[test]
    fn unregistered_session_start_carries_only_daemon_guidance() {
        assert_eq!(session_start_context(None, None), None);
        assert_eq!(
            session_start_context(None, Some("scout guidance".to_owned())).as_deref(),
            Some("scout guidance")
        );
    }
}
