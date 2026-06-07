//! Hermes agent integration.
//!
//! Installs a Hermes profile plugin that exposes tokensave tools as
//! Hermes-native plugin tools.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TokenSaveError};
use crate::mcp::tools::get_tool_definitions;

use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext};

/// Hermes agent.
pub struct HermesIntegration;

impl AgentIntegration for HermesIntegration {
    fn name(&self) -> &'static str {
        "Hermes"
    }

    fn id(&self) -> &'static str {
        "hermes"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let profile = normalize_profile(ctx.profile.as_deref())?;
        install_plugin(
            &hermes_plugin_dir(&ctx.home, profile.as_deref()),
            &ctx.tokensave_bin,
        )?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tokensave init");
        eprintln!("  2. Start Hermes — tokensave plugin tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let profile = normalize_profile(ctx.profile.as_deref())?;
        let plugin_dir = match profile.as_deref() {
            Some(profile) => hermes_plugin_dir(&ctx.home, Some(profile)),
            None => project_path.join(".hermes/plugins/tokensave"),
        };
        install_plugin(&plugin_dir, &ctx.tokensave_bin)?;
        if profile.is_none() {
            eprintln!(
                "  Hermes project plugins require HERMES_ENABLE_PROJECT_PLUGINS=true when launching Hermes."
            );
        }
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let profile = normalize_profile(ctx.profile.as_deref())?;
        uninstall_plugin(&hermes_plugin_dir(&ctx.home, profile.as_deref()))?;
        eprintln!();
        eprintln!("Uninstall complete. Tokensave has been removed from Hermes.");
        eprintln!("Restart Hermes for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mHermes integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        hermes_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(hermes_plugin_dir(home, None).join("plugin.yaml"))
    }

    fn has_tokensave(&self, home: &Path) -> bool {
        hermes_plugin_dir(home, None).join("plugin.yaml").exists()
    }
}

fn hermes_home(home: &Path) -> PathBuf {
    home.join(".hermes")
}

fn hermes_profile_dir(home: &Path, profile: Option<&str>) -> PathBuf {
    match profile {
        Some(profile) => hermes_home(home).join("profiles").join(profile),
        None => hermes_home(home),
    }
}

fn hermes_plugin_dir(home: &Path, profile: Option<&str>) -> PathBuf {
    hermes_profile_dir(home, profile).join("plugins/tokensave")
}

fn normalize_profile(profile: Option<&str>) -> Result<Option<String>> {
    let Some(profile) = profile else {
        return Ok(None);
    };
    let normalized = profile.to_ascii_lowercase();
    let mut chars = normalized.chars();
    let valid = normalized.len() <= 64
        && chars
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-');
    if !valid {
        return Err(TokenSaveError::Config {
            message: format!(
                "invalid Hermes profile '{profile}': expected [a-z0-9][a-z0-9_-]{{0,63}}"
            ),
        });
    }
    Ok(Some(normalized))
}

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let plugin = hermes_plugin_dir(home, None).join("plugin.yaml");
    if plugin.exists() {
        dc.pass(&format!(
            "Hermes tokensave plugin found at {}",
            plugin.display()
        ));
    } else {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent hermes` if you use Hermes",
            plugin.display()
        ));
    }
}

fn install_plugin(plugin_dir: &Path, tokensave_bin: &str) -> Result<()> {
    std::fs::create_dir_all(plugin_dir).map_err(|e| TokenSaveError::Config {
        message: format!("failed to create {}: {e}", plugin_dir.display()),
    })?;
    std::fs::create_dir_all(plugin_dir.join("skills/tokensave")).map_err(|e| {
        TokenSaveError::Config {
            message: format!(
                "failed to create {}: {e}",
                plugin_dir.join("skills/tokensave").display()
            ),
        }
    })?;

    write_text_file(&plugin_dir.join("plugin.yaml"), &plugin_manifest())?;
    write_text_file(&plugin_dir.join("schemas.py"), &plugin_schemas())?;
    write_text_file(&plugin_dir.join("tools.py"), &plugin_tools(tokensave_bin))?;
    write_text_file(&plugin_dir.join("__init__.py"), &plugin_init())?;
    write_text_file(&plugin_dir.join("skills/tokensave/SKILL.md"), HERMES_SKILL)?;
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        let config_path = profile_dir.join("config.yaml");
        if !enable_plugin(&config_path)? {
            eprintln!(
                "  Could not safely edit {}. Enable the plugin manually with:\n  plugins:\n    enabled:\n      - tokensave",
                config_path.display()
            );
        }
    }

    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote Hermes tokensave plugin to {}",
        plugin_dir.display()
    );
    Ok(())
}

fn enable_plugin(config_path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let Some(updated) = enable_plugin_config(&existing) else {
        return Ok(false);
    };
    if updated != existing {
        write_text_file(config_path, &updated)?;
    }
    Ok(true)
}

fn uninstall_plugin(plugin_dir: &Path) -> Result<()> {
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        disable_plugin(&profile_dir.join("config.yaml"))?;
    }
    if !plugin_dir.exists() {
        eprintln!("  {} not found, skipping", plugin_dir.display());
        return Ok(());
    }
    if std::fs::remove_dir_all(plugin_dir).is_ok() {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed Hermes tokensave plugin from {}",
            plugin_dir.display()
        );
    }
    Ok(())
}

fn disable_plugin(config_path: &Path) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(config_path) else {
        return Ok(());
    };
    let Some(updated) = disable_plugin_config(&existing) else {
        return Err(TokenSaveError::Config {
            message: format!(
                "could not safely remove tokensave from {}; leaving Hermes plugin files in place",
                config_path.display()
            ),
        });
    };
    if updated != existing {
        write_text_file(config_path, &updated)?;
    }
    Ok(())
}

fn enable_plugin_config(existing: &str) -> Option<String> {
    if existing.trim().is_empty() {
        return Some("plugins:\n  enabled:\n    - tokensave\n".to_string());
    }

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    if find_top_level_section(existing, "plugins").is_none() {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("plugins:\n  enabled:\n    - tokensave\n");
        return Some(out);
    }

    let (plugins_start, plugins_end) = find_top_level_section(existing, "plugins")?;
    let disabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "disabled")?;
    if let Some((disabled_start, disabled_end)) = disabled {
        lines = remove_list_item(lines, disabled_start, disabled_end, "tokensave");
    }

    let (plugins_start, plugins_end) = find_top_level_section_from_strings(&lines, "plugins")?;
    let enabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")?;
    if let Some((enabled_start, enabled_end)) = enabled {
        if !list_contains_item_strings(&lines, enabled_start, enabled_end, "tokensave") {
            lines.insert(enabled_start + 1, "    - tokensave".to_string());
        }
    } else {
        lines.insert(plugins_start + 1, "  enabled:".to_string());
        lines.insert(plugins_start + 2, "    - tokensave".to_string());
    }

    Some(join_lines(lines, had_trailing_newline))
}

fn disable_plugin_config(existing: &str) -> Option<String> {
    if existing.trim().is_empty() {
        return Some(existing.to_string());
    }
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((plugins_start, plugins_end)) = find_top_level_section(existing, "plugins") else {
        return Some(existing.to_string());
    };
    let enabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")?;
    if let Some((enabled_start, enabled_end)) = enabled {
        lines = remove_list_item(lines, enabled_start, enabled_end, "tokensave");
    }
    Some(join_lines(lines, had_trailing_newline))
}

fn find_top_level_section(config: &str, key: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = config.lines().collect();
    find_top_level_section_in(&lines, key)
}

fn find_top_level_section_from_strings(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    find_top_level_section_in(&borrowed, key)
}

fn find_top_level_section_in(lines: &[&str], key: &str) -> Option<(usize, usize)> {
    let target = format!("{key}:");
    let start = lines
        .iter()
        .position(|line| line_indent(line) == 0 && line.trim() == target)?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            (!trimmed.is_empty() && !trimmed.starts_with('#') && line_indent(line) == 0)
                .then_some(idx)
        })
        .unwrap_or(lines.len());
    Some((start, end))
}

fn find_child_section_from_strings(
    lines: &[String],
    plugins_start: usize,
    plugins_end: usize,
    key: &str,
) -> Option<Option<(usize, usize)>> {
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    find_child_section_in(&borrowed, plugins_start, plugins_end, key)
}

fn find_child_section_in(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    key: &str,
) -> Option<Option<(usize, usize)>> {
    let target = format!("{key}:");
    let mut start = None;
    for (idx, line) in lines
        .iter()
        .enumerate()
        .take(plugins_end)
        .skip(plugins_start + 1)
    {
        if line.trim_start().starts_with('\t') {
            return None;
        }
        if line_indent(line) == 2 {
            let trimmed = line.trim();
            if trimmed == target {
                start = Some(idx);
                break;
            }
            if trimmed.starts_with(&target) {
                return None;
            }
        }
    }
    let Some(start) = start else {
        return Some(None);
    };
    let end = lines
        .iter()
        .enumerate()
        .take(plugins_end)
        .skip(start + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            (!trimmed.is_empty() && !trimmed.starts_with('#') && line_indent(line) <= 2)
                .then_some(idx)
        })
        .unwrap_or(plugins_end);
    Some(Some((start, end)))
}

fn list_contains_item_strings(lines: &[String], start: usize, end: usize, item: &str) -> bool {
    lines
        .iter()
        .take(end)
        .skip(start + 1)
        .any(|line| line.trim() == format!("- {item}"))
}

fn remove_list_item(lines: Vec<String>, start: usize, end: usize, item: &str) -> Vec<String> {
    lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let remove = idx > start && idx < end && line.trim() == format!("- {item}");
            (!remove).then_some(line)
        })
        .collect()
}

fn line_indent(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn join_lines(lines: Vec<String>, had_trailing_newline: bool) -> String {
    let mut out = lines.join("\n");
    if had_trailing_newline || !out.is_empty() {
        out.push('\n');
    }
    out
}

fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == contents {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| TokenSaveError::Config {
        message: format!("failed to write {}: {e}", path.display()),
    })
}

fn plugin_manifest() -> String {
    let tools = get_tool_definitions()
        .into_iter()
        .map(|tool| format!("  - {}", tool.name))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "name: tokensave\n\
         kind: standalone\n\
         version: 1.0.0\n\
         description: TokenSave code intelligence tools for Hermes\n\
         provides_tools:\n{tools}\n\
         provides_hooks:\n\
           - pre_llm_call\n"
    )
}

fn plugin_schemas() -> String {
    let defs = get_tool_definitions()
        .into_iter()
        .map(|tool| {
            let schema = serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            });
            serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".to_string())
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "\"\"\"Generated tokensave tool schemas for Hermes.\"\"\"\n\n\
         TOOL_SCHEMAS = [\n{defs}\n]\n"
    )
}

fn plugin_tools(tokensave_bin: &str) -> String {
    let bin = tokensave_bin.replace('\\', "/");
    format!(
        r#""""Generated tokensave tool handlers for Hermes."""
import json
import subprocess

TOKENSAVE_BIN = {bin:?}

def call_tokensave_tool(name: str, args: dict, **kwargs) -> str:
    try:
        payload = json.dumps(args or {{}})
        result = subprocess.run(
            [TOKENSAVE_BIN, "tool", name, "--json", "--args", payload],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode != 0:
            return json.dumps({{"error": f"tokensave tool exited with status {{result.returncode}}"}})
        output = result.stdout.strip()
        if not output:
            return "{{}}"
        try:
            json.loads(output)
            return output
        except json.JSONDecodeError:
            return json.dumps({{"error": "tokensave tool returned invalid JSON"}})
    except subprocess.TimeoutExpired:
        return json.dumps({{"error": "tokensave tool timed out"}})
    except Exception as exc:
        return json.dumps({{"error": f"tokensave tool failed: {{exc}}"}})

def make_handler(name: str):
    def handler(args: dict, **kwargs) -> str:
        return call_tokensave_tool(name, args, **kwargs)
    return handler
"#
    )
}

fn plugin_init() -> String {
    r#""""tokensave Hermes plugin registration."""
from pathlib import Path

from . import schemas, tools

def _pre_llm_call(*args, **kwargs):
    return (
        "Prefer tokensave tools for codebase exploration, symbol lookup, call graphs, "
        "impact analysis, affected files, and architectural navigation before broad file reads."
    )

def _tokensave_status(raw_args: str = ""):
    return tools.call_tokensave_tool("tokensave_status", {})

def register(ctx):
    for schema in schemas.TOOL_SCHEMAS:
        name = schema["name"]
        ctx.register_tool(
            name=name,
            toolset="tokensave",
            schema=schema,
            handler=tools.make_handler(name),
        )

    ctx.register_hook("pre_llm_call", _pre_llm_call)
    register_command = getattr(ctx, "register_command", None)
    if callable(register_command):
        register_command(
            "tokensave_status",
            _tokensave_status,
            description="Show tokensave project status.",
        )

    skills_dir = Path(__file__).parent / "skills"
    skill_path = skills_dir / "tokensave" / "SKILL.md"
    if skill_path.exists():
        ctx.register_skill("tokensave:tokensave", skill_path)
"#
    .to_string()
}

const HERMES_SKILL: &str = r#"---
name: tokensave
description: Prefer tokensave tools for codebase exploration and graph queries.
---

# Use tokensave

Use tokensave tools before broad file reads for codebase exploration, symbol lookup,
call graph traversal, impact analysis, affected files, and architectural navigation.
"#;
