//! Mistral Vibe agent integration.
//!
//! Handles registration of the tracedecay MCP server in Vibe's
//! `~/.vibe/config.toml` as a `[[mcp_servers]]` entry with stdio transport,
//! and prompt rules via `~/.vibe/prompts/cli.md`.

use std::io::Write;
use std::path::Path;

use crate::errors::{Result, TraceDecayError};

use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext};

/// Mistral Vibe agent.
pub struct VibeIntegration;

/// Returns the Vibe home directory.
/// Respects `VIBE_HOME` only when it falls under `home` (so tests with
/// temp-dir homes are not polluted by the real user's environment).
fn vibe_home(home: &Path) -> std::path::PathBuf {
    if let Ok(vibe) = std::env::var("VIBE_HOME") {
        let vibe_path = std::path::PathBuf::from(&vibe);
        if vibe_path.starts_with(home) {
            return vibe_path;
        }
    }
    home.join(".vibe")
}

fn vibe_config_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("config.toml")
}

fn vibe_prompt_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("prompts/cli.md")
}

/// The TOML marker that identifies a tracedecay MCP server entry.
const TOML_MARKER: &str = "name = \"tracedecay\"";
const LEGACY_TOML_MARKER: &str = "name = \"tokensave\"";
const PROMPT_RULE_MARKER: &str = "## Prefer tracedecay MCP tools";
const LEGACY_PROMPT_RULE_MARKER: &str = "## Prefer tokensave MCP tools";

impl AgentIntegration for VibeIntegration {
    fn name(&self) -> &'static str {
        "Mistral Vibe"
    }

    fn id(&self) -> &'static str {
        "vibe"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let vibe_dir = vibe_home(&ctx.home);
        std::fs::create_dir_all(&vibe_dir).ok();

        let config_path = vibe_config_path(&ctx.home);
        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;

        let prompt_dir = vibe_dir.join("prompts");
        std::fs::create_dir_all(&prompt_dir).ok();
        let prompt_path = vibe_prompt_path(&ctx.home);
        install_prompt_rules(&prompt_path)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Vibe session — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let vibe_dir = project_path.join(".vibe");
        std::fs::create_dir_all(&vibe_dir).ok();
        std::fs::create_dir_all(vibe_dir.join("prompts")).ok();

        install_mcp_server(&vibe_dir.join("config.toml"), &ctx.tracedecay_bin)?;
        install_prompt_rules(&vibe_dir.join("prompts/cli.md"))
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = vibe_config_path(&ctx.home);
        uninstall_mcp_server(&config_path);
        uninstall_prompt_rules(&vibe_prompt_path(&ctx.home));

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Mistral Vibe.");
        eprintln!("Start a new Vibe session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mMistral Vibe integration\x1b[0m");
        doctor_check_config(dc, &ctx.home);
        doctor_check_prompt(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        vibe_home(home).is_dir()
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = vibe_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
        contents.contains(TOML_MARKER) || contents.contains(LEGACY_TOML_MARKER)
    }
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Append a `[[mcp_servers]]` entry for tracedecay to `config.toml` (idempotent).
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    let existing = if config_path.exists() {
        std::fs::read_to_string(config_path).unwrap_or_default()
    } else {
        String::new()
    };

    if existing.contains(TOML_MARKER) {
        eprintln!(
            "  tracedecay MCP server already registered in {}, skipping",
            config_path.display()
        );
        return Ok(());
    }

    let block = format!(
        "\n[[mcp_servers]]\n\
         name = \"tracedecay\"\n\
         transport = \"stdio\"\n\
         command = \"{tracedecay_bin}\"\n\
         args = [\"serve\"]\n"
    );

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_path)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to open {}: {e}", config_path.display()),
        })?;
    f.write_all(block.as_bytes())
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to write {}: {e}", config_path.display()),
        })?;

    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// Append prompt rules to the Vibe system prompt (idempotent).
fn install_prompt_rules(prompt_path: &Path) -> Result<()> {
    let existing = if prompt_path.exists() {
        std::fs::read_to_string(prompt_path).unwrap_or_default()
    } else {
        String::new()
    };
    if existing.contains(PROMPT_RULE_MARKER) {
        eprintln!("  Vibe prompt already contains tracedecay rules, skipping");
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(prompt_path)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to open {}: {e}", prompt_path.display()),
        })?;
    write!(
        f,
        "\n{PROMPT_RULE_MARKER}\n\n\
        Before reading source files or scanning the codebase, use the tracedecay MCP tools \
        (`tracedecay_context`, `tracedecay_search`, `tracedecay_callers`, `tracedecay_callees`, \
        `tracedecay_impact`, `tracedecay_node`, `tracedecay_files`, `tracedecay_affected`). \
        They provide instant semantic results from a pre-built knowledge graph and are \
        faster than file reads.\n\n\
        If a code analysis question cannot be fully answered by tracedecay MCP tools, \
        try querying the SQLite database directly at `.tracedecay/tracedecay.db` \
        (tables: `nodes`, `edges`, `files`, `memory_facts`, `memory_entities`, \
        `memory_feedback_events`). Use SQL to answer complex structural queries \
        that go beyond what the built-in tools expose.\n\n\
        For durable project/user facts, prefer `tracedecay_fact_store`, \
        `tracedecay_fact_feedback`, and `tracedecay_memory_status` over ad-hoc notes. \
        Use `tracedecay_message_search` for project-local Cursor transcript recall when \
        prior conversation context matters. Do not store secrets, credentials, or \
        unnecessary PII in persistent facts.\n\n\
        If you find a gap where tracedecay could answer a question natively, propose opening \
        an issue at https://github.com/ScriptedAlchemy/tracedecay. Remind the user to strip \
        sensitive or proprietary code from any issue text before submitting.\n\n\
        When a tracedecay tool result contains a `tracedecay_metrics:` line, report the \
        savings to the user (e.g. \"TraceDecay'd ~N tokens\"). Never silently omit this.\n"
    )
    .map_err(|e| TraceDecayError::Config {
        message: format!("failed to write Vibe prompt: {e}"),
    })?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay rules to {}",
        prompt_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove the tracedecay `[[mcp_servers]]` block from `config.toml`.
fn uninstall_mcp_server(config_path: &Path) {
    if !config_path.exists() {
        eprintln!("  {} not found, skipping", config_path.display());
        return;
    }

    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return;
    };

    if !contents.contains(TOML_MARKER) && !contents.contains(LEGACY_TOML_MARKER) {
        eprintln!(
            "  No tracedecay/tokensave MCP server in {}, skipping",
            config_path.display()
        );
        return;
    }

    // Remove the [[mcp_servers]] block that contains name = "tracedecay".
    // Strategy: split into lines, find the block, remove it.
    let lines: Vec<&str> = contents.lines().collect();
    let mut result: Vec<&str> = Vec::new();
    let mut skip = false;

    for line in &lines {
        if line.trim() == "[[mcp_servers]]" {
            // Peek ahead to see if this block is the tracedecay one.
            // We'll collect the block and decide whether to keep it.
            skip = false;
        }

        if skip {
            // If we hit a new section header, stop skipping.
            let trimmed = line.trim();
            if trimmed.starts_with("[[") || (trimmed.starts_with('[') && !trimmed.starts_with("[["))
            {
                skip = false;
            } else {
                continue;
            }
        }

        if line.contains(TOML_MARKER) || line.contains(LEGACY_TOML_MARKER) {
            // This line is inside the tracedecay block — remove it and
            // the preceding [[mcp_servers]] header.
            // Pop the header we already pushed.
            while let Some(last) = result.last() {
                if last.trim() == "[[mcp_servers]]" {
                    result.pop();
                    break;
                }
                // Pop blank lines between header and this line
                if last.trim().is_empty() {
                    result.pop();
                } else {
                    break;
                }
            }
            skip = true;
            continue;
        }

        result.push(line);
    }

    // Trim trailing blank lines
    while result.last().is_some_and(|l| l.trim().is_empty()) {
        result.pop();
    }

    let new_contents = if result.is_empty() {
        String::new()
    } else {
        format!("{}\n", result.join("\n"))
    };

    if new_contents.trim().is_empty() {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else {
        std::fs::write(config_path, &new_contents).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave MCP server from {}",
            config_path.display()
        );
    }
}

/// Remove tracedecay rules from the Vibe system prompt.
fn uninstall_prompt_rules(prompt_path: &Path) {
    if !prompt_path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(prompt_path) else {
        return;
    };
    if !contents.contains("tracedecay") && !contents.contains("tokensave") {
        eprintln!("  Vibe prompt does not contain tracedecay/tokensave rules, skipping");
        return;
    }
    let marker = if contents.contains(PROMPT_RULE_MARKER) {
        PROMPT_RULE_MARKER
    } else {
        LEGACY_PROMPT_RULE_MARKER
    };
    let Some(start) = contents.find(marker) else {
        return;
    };
    let before = &contents[..start];
    let trimmed = before.trim_end().to_string();
    if trimmed.is_empty() {
        std::fs::remove_file(prompt_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            prompt_path.display()
        );
    } else {
        std::fs::write(prompt_path, format!("{trimmed}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            prompt_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_config(dc: &mut DoctorCounters, home: &Path) {
    let config_path = vibe_config_path(home);

    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent vibe` if you use Mistral Vibe",
            config_path.display()
        ));
        return;
    }

    let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
    if contents.contains(TOML_MARKER) {
        dc.pass(&format!(
            "MCP server registered in {}",
            config_path.display()
        ));
    } else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent vibe`",
            config_path.display()
        ));
    }
}

fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path) {
    let prompt_path = vibe_prompt_path(home);
    if prompt_path.exists() {
        let has_rules = std::fs::read_to_string(&prompt_path)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("Vibe prompt contains tracedecay rules");
        } else {
            dc.fail("Vibe prompt missing tracedecay rules — run `tracedecay install --agent vibe`");
        }
    } else {
        dc.warn("Vibe prompt does not exist");
    }
}
