//! Root composition façade for agent-host integrations.
//!
//! Host behavior lives in `tracedecay-agent-hosts`; the path-based Hermes
//! profile adapter remains here because it owns filesystem backup/error policy.

pub use tracedecay_agent_hosts::agents::{
    AgentIntegration, AntigravityIntegration, ClaudeIntegration, ClineIntegration,
    CodexIntegration, CopilotIntegration, CursorIntegration, DoctorCounters, GeminiIntegration,
    HealthcheckContext, HermesIntegration, InstallContext, KiloIntegration, KimiIntegration,
    ManagedSkillExportReport, OpenCodeIntegration, RooCodeIntegration, UpdatePluginOutcome,
    VibeIntegration, ZedIntegration, all_integrations, available_integrations,
    backup_and_write_json, backup_config_file, copilot_cli_dir, detect_missing_installed_agents,
    expected_tool_perms, export_managed_skills_to_agent_hosts, export_managed_skills_to_agents,
    get_integration, home_dir, kiro_data_dir, load_json_file, load_json_file_strict,
    load_jsonc_file, load_jsonc_file_strict, load_toml_file, offer_git_post_commit_hook,
    parse_jsonc, pick_integrations_interactive, read_only_tool_names, restore_config_backup,
    safe_write_json_file, safe_write_text_file, tool_names, vscode_data_dir,
    vscode_insiders_data_dir, which_tracedecay, write_json_file, write_toml_file,
};
pub use tracedecay_agent_hosts::agents::{
    antigravity, claude, cline, codex, copilot, cursor, gemini, kilo, kimi, kiro, opencode,
    plugin_bundle, prompt_rules, roo_code, vibe, zed,
};

/// Compatibility module retaining the root-owned Hermes profile I/O seam.
pub mod hermes {
    pub use tracedecay_agent_hosts::agents::HermesIntegration;

    pub(crate) use crate::hermes_profile_config::read_config_pinned_project_root;
}

/// Backfill `installed_agents` without leaking the root `UserConfig` into the
/// lower host crate.
pub fn migrate_installed_agents(
    home: &std::path::Path,
    config: &mut crate::user_config::UserConfig,
) {
    let additions = detect_missing_installed_agents(home, &config.installed_agents);
    if additions.is_empty() {
        return;
    }
    config.installed_agents.extend(additions);
    if let Err(error) = config.save() {
        eprintln!("warning: could not save tracedecay config: {error}");
    }
}
