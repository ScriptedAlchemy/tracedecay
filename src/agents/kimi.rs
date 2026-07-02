// Rust guideline compliant 2025-10-17
//! Moonshot Kimi CLI agent integration.
//!
//! Registers the tracedecay MCP server in Kimi's `~/.kimi/mcp.json`
//! (standard `mcpServers` JSON schema, same shape as Claude/Cursor) and
//! appends prompt rules to `~/.kimi/AGENTS.md`. Kimi has no hook system
//! and no per-tool auto-approval — approval is handled globally via
//! Kimi's YOLO / AFK modes.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    safe_write_json_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

use super::prompt_rules::{PromptRulesOptions, PROMPT_RULE_MARKER};

/// Moonshot Kimi CLI agent.
pub struct KimiIntegration;

impl AgentIntegration for KimiIntegration {
    fn name(&self) -> &'static str {
        "Kimi CLI"
    }

    fn id(&self) -> &'static str {
        "kimi"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let kimi_dir = ctx.home.join(".kimi");
        std::fs::create_dir_all(&kimi_dir).ok();

        let mcp_path = kimi_dir.join("mcp.json");
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;

        let agents_md = kimi_dir.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Kimi session — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let kimi_dir = project_path.join(".kimi-code");
        std::fs::create_dir_all(&kimi_dir).ok();
        install_mcp_server(&kimi_dir.join("mcp.json"), &ctx.tracedecay_bin)?;
        let agents_md = project_path.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let kimi_dir = ctx.home.join(".kimi");
        let mcp_path = kimi_dir.join("mcp.json");
        uninstall_mcp_server(&mcp_path);

        let agents_md = kimi_dir.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(&agents_md)?;
        uninstall_prompt_rules(&agents_md);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Kimi CLI.");
        eprintln!("Start a new Kimi session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKimi CLI integration\x1b[0m");
        let kimi_dir = ctx.home.join(".kimi");
        doctor_check_mcp(dc, &kimi_dir.join("mcp.json"));
        doctor_check_prompt(dc, &kimi_dir);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".kimi").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(home.join(".kimi/mcp.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let mcp_path = home.join(".kimi/mcp.json");
        if !mcp_path.exists() {
            return false;
        }
        let json = load_json_file(&mcp_path);
        let servers = json.get("mcpServers");
        servers.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Register tracedecay under `mcpServers` in `~/.kimi/mcp.json`.
fn install_mcp_server(mcp_path: &Path, tracedecay_bin: &str) -> Result<()> {
    let backup = backup_config_file(mcp_path)?;
    let mut settings = match load_json_file_strict(mcp_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    settings["mcpServers"]["tracedecay"] = json!({
        "command": tracedecay_bin,
        "args": ["serve"]
    });

    safe_write_json_file(mcp_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        mcp_path.display()
    );
    Ok(())
}

/// Install-or-refresh prompt rules in AGENTS.md.
fn install_prompt_rules(agents_md: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(agents_md, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove tracedecay from `~/.kimi/mcp.json`.
fn uninstall_mcp_server(mcp_path: &Path) {
    if !mcp_path.exists() {
        eprintln!("  {} not found, skipping", mcp_path.display());
        return;
    }

    let Ok(contents) = std::fs::read_to_string(mcp_path) else {
        return;
    };
    let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return;
    };

    let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
    else {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            mcp_path.display()
        );
        return;
    };

    let removed = servers.remove("tracedecay").is_some();
    if !removed {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            mcp_path.display()
        );
        return;
    }

    let is_empty = settings.as_object().is_some_and(|o| {
        o.iter()
            .all(|(k, v)| k == "mcpServers" && v.as_object().is_some_and(serde_json::Map::is_empty))
    });

    if is_empty {
        std::fs::remove_file(mcp_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            mcp_path.display()
        );
    } else if backup_and_write_json(mcp_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            mcp_path.display()
        );
    }
}

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) {
    super::prompt_rules::remove_prompt_rules(agents_md, PROMPT_RULE_MARKER);
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check `~/.kimi/mcp.json` has tracedecay registered.
fn doctor_check_mcp(dc: &mut DoctorCounters, mcp_path: &Path) {
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent kimi` if you use Kimi CLI",
            mcp_path.display()
        ));
        return;
    }
    let settings = load_json_file(mcp_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tracedecay"));
    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!("MCP server registered in {}", mcp_path.display()));
    } else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent kimi`",
            mcp_path.display()
        ));
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt(dc: &mut DoctorCounters, kimi_dir: &Path) {
    let agents_md = kimi_dir.join("AGENTS.md");
    if agents_md.exists() {
        let has_rules = std::fs::read_to_string(&agents_md)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("AGENTS.md contains tracedecay rules");
        } else {
            dc.fail("AGENTS.md missing tracedecay rules — run `tracedecay install --agent kimi`");
        }
    } else {
        dc.warn("~/.kimi/AGENTS.md does not exist");
    }
}
