use super::managed_skill_model::{ManagedSkillSource, ManagedSkillState, SkillInstallTarget};

pub(crate) fn frontmatter_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

pub(crate) fn source_key(source: ManagedSkillSource) -> &'static str {
    match source {
        ManagedSkillSource::AutomationRun => "automation_run",
        ManagedSkillSource::UserDraft => "user_draft",
        ManagedSkillSource::Import => "import",
    }
}

pub(crate) fn state_key(state: ManagedSkillState) -> &'static str {
    match state {
        ManagedSkillState::PendingApproval => "pending_approval",
        ManagedSkillState::Active => "active",
        ManagedSkillState::Disabled => "disabled",
        ManagedSkillState::Archived => "archived",
    }
}

pub(crate) fn target_key(target: SkillInstallTarget) -> &'static str {
    match target {
        SkillInstallTarget::Cursor => "cursor",
        SkillInstallTarget::Codex => "codex",
        SkillInstallTarget::Claude => "claude",
        SkillInstallTarget::Agents => "agents",
        SkillInstallTarget::OpenCode => "opencode",
        SkillInstallTarget::Kimi => "kimi",
        SkillInstallTarget::Kiro => "kiro",
        SkillInstallTarget::Hermes => "hermes",
    }
}
