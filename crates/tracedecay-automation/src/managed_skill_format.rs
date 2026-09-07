use crate::managed_skill_model::{ManagedSkillSource, ManagedSkillState, SkillInstallTarget};

#[doc(hidden)]
pub fn frontmatter_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

#[doc(hidden)]
pub fn source_key(source: ManagedSkillSource) -> &'static str {
    match source {
        ManagedSkillSource::AutomationRun => "automation_run",
        ManagedSkillSource::User => "user",
        ManagedSkillSource::Import => "import",
    }
}

#[doc(hidden)]
pub fn state_key(state: ManagedSkillState) -> &'static str {
    match state {
        ManagedSkillState::Active => "active",
        ManagedSkillState::Disabled => "disabled",
        ManagedSkillState::Archived => "archived",
    }
}

#[doc(hidden)]
pub fn target_key(target: SkillInstallTarget) -> &'static str {
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
