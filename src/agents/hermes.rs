//! Hermes agent integration.
//!
//! Installs a Hermes profile plugin that exposes tokensave tools as
//! Hermes-native plugin tools.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TokenSaveError};
use crate::mcp::tools::get_tool_definitions;

use super::{
    backup_config_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

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
                "  Launch Hermes with HERMES_HOME={} so it reads this project-local plugin and memory provider config.",
                project_path.join(".hermes").display()
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
        doctor_check_plugin(dc, &ctx.home, &ctx.project_path);
    }

    fn is_detected(&self, home: &Path) -> bool {
        hermes_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(hermes_profile_dir(home, None).join("config.yaml"))
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

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path, project_path: &Path) {
    let candidates = hermes_healthcheck_plugin_paths(home, project_path);
    let plugin = candidates.iter().find(|plugin| plugin.exists());
    if let Some(plugin) = plugin {
        dc.pass(&format!(
            "Hermes tokensave plugin found at {}",
            plugin.display()
        ));
    } else if let Some(plugin) = candidates.first() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent hermes` if you use Hermes",
            plugin.display()
        ));
    } else {
        dc.warn("Hermes tokensave plugin not found — run `tokensave install --agent hermes` if you use Hermes");
    }
}

fn hermes_healthcheck_plugin_paths(home: &Path, project_path: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(hermes_home(home));

    if let Some(env_home) = std::env::var_os("HERMES_HOME") {
        if !env_home.is_empty() {
            roots.push(PathBuf::from(env_home));
        }
    }

    roots.extend(hermes_profile_dirs(home));
    roots.push(project_path.join(".hermes"));

    let mut seen = std::collections::BTreeSet::new();
    let mut plugins = Vec::new();
    for root in roots {
        let plugin = root.join("plugins/tokensave/plugin.yaml");
        if seen.insert(plugin.clone()) {
            plugins.push(plugin);
        }
    }
    plugins
}

fn hermes_profile_dirs(home: &Path) -> Vec<PathBuf> {
    let profiles_dir = hermes_home(home).join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return Vec::new();
    };
    let mut profiles = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            file_type.is_dir().then(|| entry.path())
        })
        .collect::<Vec<_>>();
    profiles.sort();
    profiles
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
    write_text_file(&plugin_dir.join("schemas.json"), &plugin_schemas_json()?)?;
    write_text_file(&plugin_dir.join("tools.py"), &plugin_tools(tokensave_bin))?;
    write_text_file(&plugin_dir.join("__init__.py"), &plugin_init())?;
    write_text_file(&plugin_dir.join("skills/tokensave/SKILL.md"), HERMES_SKILL)?;
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        let config_path = profile_dir.join("config.yaml");
        enable_plugin(&config_path)?;
    }

    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote Hermes tokensave plugin to {}",
        plugin_dir.display()
    );
    Ok(())
}

fn enable_plugin(config_path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = enable_plugin_config(&existing).map_err(|message| TokenSaveError::Config {
        message: format!("{message} in {}", config_path.display()),
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
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

    remove_generated_file(&plugin_dir.join("plugin.yaml"))?;
    remove_generated_file(&plugin_dir.join("schemas.py"))?;
    remove_generated_file(&plugin_dir.join("schemas.json"))?;
    remove_generated_file(&plugin_dir.join("tools.py"))?;
    remove_generated_file(&plugin_dir.join("__init__.py"))?;
    remove_generated_file(&plugin_dir.join("skills/tokensave/SKILL.md"))?;
    remove_empty_dir(&plugin_dir.join("skills/tokensave"))?;
    remove_empty_dir(&plugin_dir.join("skills"))?;

    if remove_empty_dir(plugin_dir)? {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed Hermes tokensave plugin from {}",
            plugin_dir.display()
        );
    } else {
        eprintln!(
            "  Left {} in place because it contains files not generated by tokensave",
            plugin_dir.display()
        );
    }
    Ok(())
}

fn disable_plugin(config_path: &Path) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(config_path) else {
        return Ok(());
    };
    let updated = disable_plugin_config(&existing).map_err(|message| TokenSaveError::Config {
        message: format!(
            "{message} in {}; leaving Hermes plugin files in place",
            config_path.display()
        ),
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(())
}

fn enable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(
            "memory:\n  provider: tokensave\nplugins:\n  enabled:\n    - tokensave\n".to_string(),
        );
    }

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    validate_top_level_plugins_shape(existing)?;

    if find_top_level_section(existing, "plugins").is_none() {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("plugins:\n  enabled:\n    - tokensave\n");
        return enable_memory_provider_config(&out);
    }

    let (plugins_start, plugins_end) = find_top_level_section(existing, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    let disabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "disabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    if let Some((disabled_start, disabled_end)) = disabled {
        lines = remove_list_item(lines, disabled_start, disabled_end, "tokensave");
    }

    let (plugins_start, plugins_end) = find_top_level_section_from_strings(&lines, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    let enabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    if let Some((enabled_start, enabled_end)) = enabled {
        if !list_contains_item_strings(&lines, enabled_start, enabled_end, "tokensave") {
            lines.insert(enabled_start + 1, "    - tokensave".to_string());
        }
    } else {
        lines.insert(plugins_start + 1, "  enabled:".to_string());
        lines.insert(plugins_start + 2, "    - tokensave".to_string());
    }

    enable_memory_provider_config(&join_lines(lines, had_trailing_newline))
}

fn disable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    validate_top_level_plugins_shape(existing)?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((plugins_start, plugins_end)) = find_top_level_section(existing, "plugins") else {
        return Ok(existing.to_string());
    };
    let enabled = find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    if let Some((enabled_start, enabled_end)) = enabled {
        lines = remove_list_item(lines, enabled_start, enabled_end, "tokensave");
    }
    disable_memory_provider_config(&join_lines(lines, had_trailing_newline))
}

fn enable_memory_provider_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("memory:\n  provider: tokensave\n".to_string());
    }

    validate_top_level_memory_shape(existing)?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    let Some((memory_start, memory_end)) = find_top_level_section(existing, "memory") else {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("memory:\n  provider: tokensave\n");
        return Ok(out);
    };

    let provider_line = find_memory_provider_line(&lines, memory_start, memory_end)
        .ok_or_else(|| "unsupported Hermes memory config".to_string())?;
    if let Some(provider_line) = provider_line {
        if lines[provider_line].trim() != "provider: tokensave" {
            return Err(
                "Hermes memory provider already configured; refusing to overwrite it".to_string(),
            );
        }
    } else {
        lines.insert(memory_start + 1, "  provider: tokensave".to_string());
    }

    Ok(join_lines(lines, had_trailing_newline))
}

fn disable_memory_provider_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }

    validate_top_level_memory_shape(existing)?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((memory_start, memory_end)) = find_top_level_section(existing, "memory") else {
        return Ok(existing.to_string());
    };
    let provider_line = find_memory_provider_line(&lines, memory_start, memory_end)
        .ok_or_else(|| "unsupported Hermes memory config".to_string())?;
    let mut removed_provider = false;
    if let Some(provider_line) = provider_line {
        if lines[provider_line].trim() == "provider: tokensave" {
            lines.remove(provider_line);
            removed_provider = true;
        }
    }
    if removed_provider {
        remove_empty_top_level_section(&mut lines, "memory");
    }

    Ok(join_lines(lines, had_trailing_newline))
}

fn validate_top_level_plugins_shape(existing: &str) -> std::result::Result<(), String> {
    let plugin_lines = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            line_indent(line) == 0 && !trimmed.starts_with('#') && trimmed.starts_with("plugins:")
        })
        .collect::<Vec<_>>();
    match plugin_lines.as_slice() {
        [] => Ok(()),
        [line] if line.trim() == "plugins:" => Ok(()),
        _ => Err(
            "unsupported Hermes plugins config; expected a block-style `plugins:` mapping"
                .to_string(),
        ),
    }
}

fn validate_top_level_memory_shape(existing: &str) -> std::result::Result<(), String> {
    let memory_lines = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            line_indent(line) == 0 && !trimmed.starts_with('#') && trimmed.starts_with("memory:")
        })
        .collect::<Vec<_>>();
    match memory_lines.as_slice() {
        [] => Ok(()),
        [line] if line.trim() == "memory:" => Ok(()),
        _ => Err(
            "unsupported Hermes memory config; expected a block-style `memory:` mapping"
                .to_string(),
        ),
    }
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

fn find_memory_provider_line(
    lines: &[String],
    memory_start: usize,
    memory_end: usize,
) -> Option<Option<usize>> {
    for (idx, line) in lines
        .iter()
        .enumerate()
        .take(memory_end)
        .skip(memory_start + 1)
    {
        if line.trim_start().starts_with('\t') {
            return None;
        }
        if line_indent(line) == 2 && line.trim_start().starts_with("provider:") {
            return Some(Some(idx));
        }
    }
    Some(None)
}

fn remove_empty_top_level_section(lines: &mut Vec<String>, key: &str) {
    let Some((start, end)) = find_top_level_section_from_strings(lines, key) else {
        return;
    };
    let has_content = lines.iter().take(end).skip(start + 1).any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
    });
    if !has_content {
        lines.drain(start..end);
    }
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

fn write_config_file(path: &Path, contents: &str) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(current) => Some(current),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => {
            return Err(TokenSaveError::Config {
                message: format!("failed to read {}: {e}", path.display()),
            });
        }
    };
    if current.as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let backup = backup_config_file(path)?;
    let new_path = PathBuf::from(format!("{}.new", path.display()));
    if let Err(e) = std::fs::write(&new_path, contents) {
        std::fs::remove_file(&new_path).ok();
        return Err(TokenSaveError::Config {
            message: format!("failed to write {}: {e}", new_path.display()),
        });
    }
    if let Err(e) = std::fs::rename(&new_path, path) {
        std::fs::remove_file(&new_path).ok();
        let backup_hint = backup
            .as_ref()
            .map(|path| format!(" Backup is at {}.", path.display()))
            .unwrap_or_default();
        return Err(TokenSaveError::Config {
            message: format!(
                "failed to replace {} with {}: {e}.{backup_hint}",
                path.display(),
                new_path.display()
            ),
        });
    }
    Ok(())
}

fn remove_generated_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", path.display()),
        }),
    }
}

fn remove_empty_dir(path: &Path) -> Result<bool> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {
            Ok(false)
        }
        Err(e) => Err(TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", path.display()),
        }),
    }
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
           - pre_llm_call\n\
         provides_commands:\n\
           - /tokensave_status\n"
    )
}

fn plugin_schemas() -> String {
    r#""""Generated tokensave tool schemas for Hermes."""
import json
from pathlib import Path

with Path(__file__).with_name("schemas.json").open("r", encoding="utf-8") as schema_file:
    TOOL_SCHEMAS = json.load(schema_file)
"#
    .to_string()
}

fn plugin_schemas_json() -> Result<String> {
    let defs = get_tool_definitions()
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&defs)
        .map(|json| format!("{json}\n"))
        .map_err(|e| TokenSaveError::Config {
            message: format!("failed to serialize Hermes schemas.json: {e}"),
        })
}

fn plugin_tools(tokensave_bin: &str) -> String {
    let bin = serde_json::to_string(tokensave_bin).unwrap_or_else(|_| "\"tokensave\"".to_string());
    format!(
        r#""""Generated tokensave tool handlers for Hermes."""
import json
import subprocess

TOKENSAVE_BIN = {bin}
TOKENSAVE_TIMEOUT_SECONDS = 600
MAX_CAPTURE_CHARS = 4000

def normalize_output(value) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)

def truncate_output(value, limit: int = MAX_CAPTURE_CHARS) -> str:
    output = normalize_output(value)
    if len(output) <= limit:
        return output
    return output[:limit] + "...<truncated>"

def error_payload(message: str, result=None) -> str:
    payload = {{"error": message}}
    if result is not None:
        stdout = truncate_output(getattr(result, "stdout", ""))
        stderr = truncate_output(getattr(result, "stderr", ""))
        if stdout:
            payload["stdout"] = stdout
        if stderr:
            payload["stderr"] = stderr
    return json.dumps(payload)

def call_tokensave_tool(name: str, args: dict, **kwargs) -> str:
    try:
        payload = json.dumps(args or {{}})
        result = subprocess.run(
            [TOKENSAVE_BIN, "tool", name, "--json", "--args", payload],
            check=False,
            capture_output=True,
            text=True,
            timeout=TOKENSAVE_TIMEOUT_SECONDS,
            shell=False,
        )
        if result.returncode != 0:
            return error_payload(f"tokensave tool exited with status {{result.returncode}}", result)
        output = result.stdout.strip()
        if not output:
            return "{{}}"
        try:
            json.loads(output)
            return output
        except json.JSONDecodeError:
            return error_payload("tokensave tool returned invalid JSON", result)
    except subprocess.TimeoutExpired as exc:
        return error_payload("tokensave tool timed out", exc)
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
import json
import os
import shutil
from pathlib import Path

from . import schemas, tools

try:
    from agent.memory_provider import MemoryProvider
except Exception:
    class MemoryProvider:
        pass

try:
    from agent.context_engine import ContextEngine
except Exception:
    class ContextEngine:
        pass

MEMORY_FACT_ACTIONS = {
    "fact_add": "add",
    "fact_search": "search",
    "fact_probe": "probe",
    "fact_related": "related",
    "fact_reason": "reason",
    "fact_contradict": "contradict",
    "fact_update": "update",
    "fact_remove": "remove",
    "fact_list": "list",
}

MEMORY_ACTION_DESCRIPTIONS = {
    "fact_add": "Add a holographic memory fact.",
    "fact_search": "Search holographic memory facts by query.",
    "fact_probe": "Find facts connected to one entity.",
    "fact_related": "List entities related to one entity.",
    "fact_reason": "Reason over facts that connect multiple entities.",
    "fact_contradict": "Scan memory facts for likely contradictions.",
    "fact_update": "Update an existing holographic memory fact.",
    "fact_remove": "Remove a holographic memory fact.",
    "fact_list": "List holographic memory facts.",
}

MEMORY_TOOL_MAP = {"fact_store": {"tokensave_name": "tokensave_fact_store"}}
for _hermes_name, _action in MEMORY_FACT_ACTIONS.items():
    MEMORY_TOOL_MAP[_hermes_name] = {
        "tokensave_name": "tokensave_fact_store",
        "fixed_args": {"action": _action},
    }
MEMORY_TOOL_MAP["fact_feedback"] = {"tokensave_name": "tokensave_fact_feedback"}
MEMORY_TOOL_MAP["memory_status"] = {"tokensave_name": "tokensave_memory_status"}

def _pre_llm_call(*args, **kwargs):
    return (
        "Prefer tokensave tools for codebase exploration, symbol lookup, call graphs, "
        "impact analysis, affected files, and architectural navigation before broad file reads."
    )

def _tokensave_status(raw_args: str = ""):
    return tools.call_tokensave_tool("tokensave_status", {})

def call_tokensave_json(name: str, args: dict, **kwargs) -> dict:
    raw = tools.call_tokensave_tool(name, args, **kwargs)
    try:
        outer = json.loads(raw)
    except json.JSONDecodeError:
        return {"error": "tokensave tool returned invalid JSON", "raw": raw}
    content = outer.get("content")
    if not isinstance(content, list) or not content:
        return outer
    first = content[0]
    if not isinstance(first, dict):
        return outer
    text = first.get("text")
    if not isinstance(text, str):
        return outer
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"error": "tokensave tool returned invalid nested JSON", "text": text}

def _memory_schema(tokensave_name: str, hermes_name: str, action: str = None) -> dict:
    for schema in schemas.TOOL_SCHEMAS:
        if schema.get("name") == tokensave_name:
            parameters = json.loads(json.dumps(schema.get("parameters", {})))
            if action is not None:
                properties = parameters.get("properties")
                if isinstance(properties, dict):
                    properties.pop("action", None)
                required = parameters.get("required")
                if isinstance(required, list):
                    required = [field for field in required if field != "action"]
                    if required:
                        parameters["required"] = required
                    else:
                        parameters.pop("required", None)
            return {
                "name": hermes_name,
                "description": MEMORY_ACTION_DESCRIPTIONS.get(
                    hermes_name, schema.get("description", "")
                ),
                "parameters": parameters,
            }
    return {
        "name": hermes_name,
        "description": f"Tokensave memory tool {hermes_name}.",
        "parameters": {"type": "object", "properties": {}},
    }

def _decode_tool_args(arguments):
    if arguments is None:
        return {}
    if isinstance(arguments, dict):
        return arguments
    if isinstance(arguments, str):
        if not arguments.strip():
            return {}
        try:
            return json.loads(arguments)
        except json.JSONDecodeError:
            return {"arguments": arguments}
    return {"arguments": arguments}

def _normalize_memory_tool_call(name, arguments):
    if isinstance(name, dict):
        function = name.get("function") or {}
        tool_name = name.get("name") or function.get("name")
        tool_args = name.get("arguments", function.get("arguments", arguments))
        return tool_name, _decode_tool_args(tool_args)
    return name, _decode_tool_args(arguments)

def _tokensave_binary_available() -> bool:
    if os.path.dirname(tools.TOKENSAVE_BIN):
        return Path(tools.TOKENSAVE_BIN).is_file() and os.access(tools.TOKENSAVE_BIN, os.X_OK)
    return shutil.which(tools.TOKENSAVE_BIN) is not None

def _storage_args(project_root=None, hermes_home=None):
    args = {}
    if project_root:
        args["storage_scope"] = "project_local"
        args["project_root"] = str(project_root)
    elif hermes_home:
        args["storage_scope"] = "hermes_profile"
        args["hermes_home"] = str(hermes_home)
    else:
        args["storage_scope"] = "hermes_profile"
    return args

class TokenSaveContextEngine(ContextEngine):
    def __init__(self):
        self.active_session_id = None
        self.hermes_home = None
        self.project_root = None

    def _bind_session(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        if session_id is not None:
            self.active_session_id = session_id
        next_hermes_home = hermes_home or kwargs.get("hermes_home")
        if next_hermes_home:
            self.hermes_home = next_hermes_home
        next_project_root = project_root or kwargs.get("project_root") or kwargs.get("cwd")
        if next_project_root:
            self.project_root = next_project_root

    def initialize(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        self._bind_session(session_id, hermes_home, project_root, **kwargs)

    def on_session_start(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        self._bind_session(session_id, hermes_home, project_root, **kwargs)

    def should_compress_preflight(self, messages, current_tokens=None, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
            "current_tokens": current_tokens,
        })
        return call_tokensave_json("tokensave_lcm_preflight", args, **kwargs)

    def compress(self, messages, current_tokens=None, focus_topic=None, **kwargs):
        summarizer = kwargs.pop("summarizer", None) or {"mode": "hermes_auxiliary"}
        args = _storage_args(self.project_root, self.hermes_home)
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
            "current_tokens": current_tokens,
            "focus_topic": focus_topic,
            "summarizer": summarizer,
        })
        return call_tokensave_json("tokensave_lcm_compress", args, **kwargs)

class TokensaveMemoryProvider(MemoryProvider):
    provider_id = "tokensave"

    def __init__(self):
        self.hermes_home = None
        self.session_id = None

    @property
    def name(self) -> str:
        return "tokensave"

    def is_available(self) -> bool:
        return _tokensave_binary_available()

    def initialize(self, session_id=None, **kwargs):
        self.hermes_home = kwargs.get("hermes_home")
        self.session_id = session_id

    def get_tool_schemas(self):
        memory_schemas = [_memory_schema("tokensave_fact_store", "fact_store")]
        for hermes_name, action in MEMORY_FACT_ACTIONS.items():
            memory_schemas.append(_memory_schema("tokensave_fact_store", hermes_name, action))
        memory_schemas.append(_memory_schema("tokensave_fact_feedback", "fact_feedback"))
        memory_schemas.append(_memory_schema("tokensave_memory_status", "memory_status"))
        return memory_schemas

    def handle_tool_call(self, name, arguments=None, **kwargs) -> str:
        tool_name, tool_args = _normalize_memory_tool_call(name, arguments)
        mapping = MEMORY_TOOL_MAP.get(tool_name)
        if mapping is None:
            return tools.error_payload(f"unknown memory tool: {tool_name}")
        tokensave_name = mapping["tokensave_name"]
        fixed_args = mapping.get("fixed_args")
        if fixed_args:
            tool_args = dict(tool_args)
            tool_args.update(fixed_args)
        return tools.call_tokensave_tool(tokensave_name, tool_args, **kwargs)

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
            "/tokensave_status",
            _tokensave_status,
            description="Show tokensave project status.",
        )

    if callable(getattr(ctx, "register_memory_provider", None)):
        ctx.register_memory_provider(TokensaveMemoryProvider())

    if callable(getattr(ctx, "register_context_engine", None)):
        ctx.register_context_engine(TokenSaveContextEngine())

    skills_dir = Path(__file__).parent / "skills"
    skill_path = skills_dir / "tokensave" / "SKILL.md"
    register_skill = getattr(ctx, "register_skill", None)
    if skill_path.exists() and callable(register_skill):
        register_skill("tokensave:tokensave", skill_path)
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
