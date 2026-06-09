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

    enable_memory_provider_config(&join_lines(&lines, had_trailing_newline))
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
    disable_memory_provider_config(&join_lines(&lines, had_trailing_newline))
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

    Ok(join_lines(&lines, had_trailing_newline))
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

    Ok(join_lines(&lines, had_trailing_newline))
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

// Outer `None` means the config is unsupported/ambiguous; inner `None` means
// the section was simply not found.
#[allow(clippy::option_option)]
fn find_child_section_from_strings(
    lines: &[String],
    plugins_start: usize,
    plugins_end: usize,
    key: &str,
) -> Option<Option<(usize, usize)>> {
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    find_child_section_in(&borrowed, plugins_start, plugins_end, key)
}

#[allow(clippy::option_option)]
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

#[allow(clippy::option_option)]
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

fn join_lines(lines: &[String], had_trailing_newline: bool) -> String {
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
        tool_args = args or {{}}
        if "messages" in kwargs and "messages" not in tool_args:
            tool_args = dict(tool_args)
            tool_args["messages"] = kwargs["messages"]
        payload = json.dumps(tool_args)
        project_root = kwargs.get("project_root") or tool_args.get("project_root")
        argv = [TOKENSAVE_BIN, "tool"]
        if project_root:
            argv.extend(["--project", str(project_root)])
        argv.extend([name, "--json", "--args", payload])
        result = subprocess.run(
            argv,
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
import logging
import os
import re
import shutil
import time
from pathlib import Path

from . import schemas, tools

logger = logging.getLogger(__name__)

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

LCM_TOOL_ALIASES = {
    "lcm_grep": "tokensave_lcm_grep",
    "lcm_load_session": "tokensave_lcm_load_session",
    "lcm_describe": "tokensave_lcm_describe",
    "lcm_expand": "tokensave_lcm_expand",
    "lcm_expand_query": "tokensave_lcm_expand_query",
    "lcm_status": "tokensave_lcm_status",
    "lcm_doctor": "tokensave_lcm_doctor",
}
LCM_NATIVE_TOOL_NAMES = tuple(LCM_TOOL_ALIASES.keys())
LCM_DIRECT_TOOL_NAMES = frozenset(LCM_TOOL_ALIASES.values())
LCM_DIRECT_TO_NATIVE = {tokensave_name: native_name for native_name, tokensave_name in LCM_TOOL_ALIASES.items()}

LCM_NATIVE_SCHEMAS = [
    {
        "name": "lcm_grep",
        "description": (
            "Search the plugin-local LCM database for past conversation content. "
            "Default scope is the active session and returns raw messages and summary nodes."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query."},
                "limit": {"type": "integer", "description": "Max results to return.", "default": 10},
                "sort": {
                    "type": "string",
                    "enum": ["recency", "relevance", "hybrid"],
                    "description": "How to order matches.",
                    "default": "recency",
                },
                "session_scope": {
                    "type": "string",
                    "enum": ["current", "all", "session"],
                    "description": "Search scope across the local LCM database.",
                    "default": "current",
                },
                "session_id": {"type": "string", "description": "Session id when session_scope='session'."},
                "source": {"type": "string", "description": "Optional source/platform filter."},
                "role": {
                    "type": "string",
                    "enum": ["system", "user", "assistant", "tool", "unknown"],
                    "description": "Optional raw-message role filter.",
                },
                "time_from": {
                    "anyOf": [{"type": "number"}, {"type": "string"}],
                    "description": "Optional inclusive minimum raw-message timestamp.",
                },
                "time_to": {
                    "anyOf": [{"type": "number"}, {"type": "string"}],
                    "description": "Optional inclusive maximum raw-message timestamp.",
                },
            },
            "required": ["query"],
        },
    },
    {
        "name": "lcm_load_session",
        "description": "Load an ordered raw-message transcript page for one explicit session_id.",
        "parameters": {
            "type": "object",
            "properties": {
                "session_id": {"type": "string", "description": "Explicit LCM session id to load."},
                "limit": {"type": "integer", "description": "Maximum raw messages to return.", "default": 100},
                "max_content_chars": {
                    "type": "integer",
                    "description": "Maximum content characters to include per message.",
                    "default": 4000,
                },
                "after_store_id": {
                    "type": "integer",
                    "description": "Exclusive cursor for pagination.",
                    "default": 0,
                },
                "roles": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional role filter.",
                },
                "time_from": {
                    "type": "number",
                    "description": "Optional inclusive minimum message timestamp.",
                },
                "time_to": {
                    "type": "number",
                    "description": "Optional inclusive maximum message timestamp.",
                },
            },
            "required": ["session_id"],
        },
    },
    {
        "name": "lcm_describe",
        "description": "Inspect a current-session summary node, externalized payload, or top-level DAG overview.",
        "parameters": {
            "type": "object",
            "properties": {
                "node_id": {"type": "integer", "description": "Summary node ID to inspect."},
                "externalized_ref": {
                    "type": "string",
                    "description": "Externalized payload ref filename to inspect.",
                },
            },
            "required": [],
        },
    },
    {
        "name": "lcm_expand",
        "description": "Recover detail behind a summary node, externalized payload, or raw message.",
        "parameters": {
            "type": "object",
            "properties": {
                "node_id": {"type": "integer", "description": "Summary node ID to expand."},
                "externalized_ref": {
                    "type": "string",
                    "description": "Externalized payload ref filename to expand.",
                },
                "store_id": {"type": "integer", "description": "Raw message store_id to fetch."},
                "max_tokens": {"type": "integer", "description": "Token budget for returned content.", "default": 4000},
                "source_offset": {
                    "type": "integer",
                    "description": "Source pagination offset for node_id mode.",
                    "default": 0,
                },
                "source_limit": {
                    "type": "integer",
                    "description": "Maximum immediate sources to return from source_offset.",
                },
                "content_offset": {
                    "type": "integer",
                    "description": "Character offset used to continue oversized content.",
                    "default": 0,
                },
            },
            "required": [],
        },
    },
    {
        "name": "lcm_expand_query",
        "description": "Answer a natural-language question using expanded LCM context from the current session.",
        "parameters": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "The question or task to answer from expanded LCM context."},
                "query": {"type": "string", "description": "Optional search query used to find candidate summaries."},
                "node_ids": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Optional explicit summary node IDs.",
                },
                "max_results": {"type": "integer", "description": "Max candidate summaries.", "default": 5},
                "max_tokens": {"type": "integer", "description": "Max answer tokens.", "default": 2000},
                "context_max_tokens": {
                    "type": "integer",
                    "description": "Expanded context budget for the auxiliary LLM.",
                    "default": 32000,
                },
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "lcm_status",
        "description": "Get a quick health overview of the LCM engine for the current session.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "lcm_doctor",
        "description": "Run diagnostics on the LCM database and configuration.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
]

def _make_wrapped_lcm_handler(tool_name: str, engine):
    def _wrapped(args: dict, **kwargs) -> str:
        return engine.handle_tool_call(tool_name, args, **kwargs)
    return _wrapped

def _host_forwards_registered_tool_messages(ctx) -> bool:
    capability = getattr(ctx, "context_engine_tool_handlers_receive_messages", False)
    if callable(capability):
        try:
            capability = capability()
        except Exception:
            return False
    return bool(capability)

def _pre_llm_call(*args, **kwargs):
    return (
        "Prefer tokensave tools for codebase exploration, symbol lookup, call graphs, "
        "impact analysis, affected files, and architectural navigation before broad file reads."
    )

def _tokensave_status(raw_args: str = ""):
    return tools.call_tokensave_tool("tokensave_status", {})

def _bridge_preview(value, limit: int = 2048) -> str:
    if isinstance(value, str):
        preview = value
    else:
        try:
            preview = json.dumps(value, sort_keys=True)
        except Exception:
            preview = repr(value)
    if len(preview) > limit:
        return preview[:limit] + "...[truncated]"
    return preview


def call_tokensave_json(name: str, args: dict, **kwargs) -> dict:
    raw = tools.call_tokensave_tool(name, args, **kwargs)
    try:
        outer = json.loads(raw)
    except json.JSONDecodeError:
        return {
            "error": "tokensave tool returned invalid JSON",
            "raw_preview": _bridge_preview(raw),
        }
    if isinstance(outer, dict) and "error" in outer:
        return outer
    if not isinstance(outer, dict):
        return {
            "error": "tokensave tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    content = outer.get("content")
    if not isinstance(content, list) or not content:
        return {
            "error": "tokensave tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    first = content[0]
    if not isinstance(first, dict):
        return {
            "error": "tokensave tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    text = first.get("text")
    if not isinstance(text, str):
        return {
            "error": "tokensave tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {
            "error": "tokensave tool returned invalid nested JSON",
            "text_preview": _bridge_preview(text),
        }

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

def _clone_schema(tokensave_name: str, public_name: str = None) -> dict:
    for schema in schemas.TOOL_SCHEMAS:
        if schema.get("name") == tokensave_name:
            cloned = json.loads(json.dumps(schema))
            if public_name is not None:
                cloned["name"] = public_name
            return cloned
    return {
        "name": public_name or tokensave_name,
        "description": f"Tokensave tool {public_name or tokensave_name}.",
        "parameters": {"type": "object", "properties": {}},
    }

def _lcm_tool_schemas() -> list:
    return json.loads(json.dumps(LCM_NATIVE_SCHEMAS))

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

def _configured_hermes_home(config):
    if config is None:
        return None
    if isinstance(config, dict):
        return config.get("hermes_home") or config.get("home")
    for attr in ("hermes_home", "home"):
        value = getattr(config, attr, None)
        if value:
            return value
    return None

def _resolve_hermes_home(config=None, hermes_home=None):
    for candidate in (
        hermes_home,
        _configured_hermes_home(config),
        os.environ.get("HERMES_HOME"),
    ):
        if candidate:
            return str(candidate)
    try:
        from hermes_cli.config import get_hermes_home
        resolved = get_hermes_home()
        if resolved:
            return str(resolved)
    except Exception:
        pass
    fallback = os.path.expanduser("~/.hermes")
    return fallback or None

def _configured_value(config, *names, default=None):
    if config is None:
        return default
    if isinstance(config, dict):
        for name in names:
            if name in config and config[name] is not None:
                return config[name]
        return default
    for name in names:
        value = getattr(config, name, None)
        if value is not None:
            return value
    return default

def _configured_int(config, *names, default=None):
    value = _configured_value(config, *names, default=default)
    if value is None:
        return None
    try:
        return int(value)
    except Exception:
        return None

def _configured_bool(config, *names, default=None):
    value = _configured_value(config, *names, default=default)
    if value is None:
        return None
    if isinstance(value, str):
        return value.strip().lower() in ("1", "true", "yes", "on")
    return bool(value)

def _configured_threshold_tokens(config):
    explicit = _configured_int(config, "threshold_tokens")
    if explicit is not None:
        return explicit
    context_length = _configured_int(
        config,
        "context_length",
        "max_context_tokens",
        "model_context_tokens",
    )
    threshold = _configured_value(config, "context_threshold")
    if context_length is None or threshold is None:
        return None
    try:
        return int(context_length * float(threshold))
    except Exception:
        return None

def _lcm_config_args(config) -> dict:
    args = {
        "fresh_tail_count": _configured_int(config, "fresh_tail_count", default=64),
        "leaf_chunk_tokens": _configured_int(config, "leaf_chunk_tokens", default=20000),
        "dynamic_leaf_chunk_enabled": _configured_bool(
            config,
            "dynamic_leaf_chunk_enabled",
            default=False,
        ),
        "dynamic_leaf_chunk_max": _configured_int(config, "dynamic_leaf_chunk_max", default=40000),
        "max_assembly_tokens": _configured_int(config, "max_assembly_tokens", default=0),
        # Hermes derives an assembly cap of context_length - reserve_tokens_floor
        # when both are positive; pass both through so tokensave can apply the
        # same derivation (reserve_tokens_floor defaults to 0 = disabled).
        "reserve_tokens_floor": _configured_int(config, "reserve_tokens_floor", default=0),
        "context_length": _configured_int(
            config,
            "context_length",
            "max_context_tokens",
            "model_context_tokens",
        ),
        "summary_fan_in": _configured_int(config, "summary_fan_in", "condensation_fanin", default=4),
    }
    threshold_tokens = _configured_threshold_tokens(config)
    if threshold_tokens is not None:
        args["threshold_tokens"] = threshold_tokens
    return {key: value for key, value in args.items() if value is not None}

def _apply_lcm_option_overrides(args: dict, kwargs: dict, keys) -> None:
    for key in keys:
        if key in kwargs and kwargs[key] is not None:
            args[key] = kwargs.pop(key)

REASONING_TAGS = ("think", "thinking", "reasoning", "thought", "REASONING_SCRATCHPAD")
FALLBACK_MARKER = "[deterministic compression fallback]"
RETRY_WORTHY_AUXILIARY_ERRORS = (
    "context length",
    "maximum context",
    "max context",
    "token limit",
    "too many tokens",
    "prompt is too long",
    "input too long",
    "request too large",
    "timed out",
    "timeout",
)

def _strip_reasoning(text: str) -> str:
    output = text or ""
    for tag in REASONING_TAGS:
        escaped = re.escape(tag)
        output = re.sub(
            rf"<{escaped}>.*?</{escaped}>",
            "",
            output,
            flags=re.IGNORECASE | re.DOTALL,
        )
    return output.strip()

def _llm_response_text(response) -> str:
    if isinstance(response, str):
        return response
    if isinstance(response, dict):
        content = response.get("content")
        if isinstance(content, str):
            return content
        choices = response.get("choices")
        if isinstance(choices, list) and choices:
            message = choices[0].get("message") if isinstance(choices[0], dict) else None
            if isinstance(message, dict) and isinstance(message.get("content"), str):
                return message["content"]
    choices = getattr(response, "choices", None)
    if choices:
        message = getattr(choices[0], "message", None)
        content = getattr(message, "content", None)
        if isinstance(content, str):
            return content
    return "" if response is None else str(response)

def _message_content(message) -> str:
    if not isinstance(message, dict):
        return str(message)
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, dict) and isinstance(content.get("text"), str):
        return content["text"]
    if isinstance(content, list):
        parts = [
            item.get("text")
            for item in content
            if isinstance(item, dict) and isinstance(item.get("text"), str)
        ]
        if parts:
            return "\n\n".join(parts)
    return "" if content is None else str(content)

def _summary_source_messages(source_messages):
    normalized = []
    for message in source_messages or []:
        if not isinstance(message, dict):
            normalized.append({"role": "user", "content": str(message)})
            continue
        normalized.append({
            "role": message.get("role") or "user",
            "content": _message_content(message),
        })
    return normalized

def _summary_source_size(messages) -> int:
    total = 0
    for message in messages or []:
        total += len(_message_content(message))
    return total

def _deterministic_truncation(messages, limit: int = 2048) -> str:
    lines = []
    for message in messages or []:
        if isinstance(message, dict):
            role = message.get("role") or "user"
            content = _message_content(message)
        else:
            role = "user"
            content = str(message)
        if content:
            lines.append(f"{role}: {content}")
    text = "\n".join(lines).strip()
    if not text:
        text = "No auxiliary summary was available."
    max_prefix = max(0, limit - len(FALLBACK_MARKER) - 2)
    return f"{text[:max_prefix].rstrip()}\n\n{FALLBACK_MARKER}"

def _auxiliary_error_classification(error) -> str:
    message = str(error or "").lower()
    if any(pattern in message for pattern in RETRY_WORTHY_AUXILIARY_ERRORS):
        return "retry_worthy"
    return "permanent"

def _auxiliary_retry_limit(kwargs) -> int:
    try:
        limit = int(kwargs.pop("max_auxiliary_attempts", 2) or 2)
    except Exception:
        limit = 2
    return min(max(limit, 1), 8)

def _next_smaller_source_limit(source_messages, current_limit=None):
    source_count = len(source_messages or [])
    if source_count <= 1:
        return None
    next_limit = max(1, source_count // 2)
    if current_limit is not None:
        try:
            next_limit = min(next_limit, int(current_limit))
        except Exception:
            pass
    if next_limit >= source_count:
        next_limit = source_count - 1
    return max(1, next_limit)

def _with_auxiliary_metadata(
    result,
    *,
    attempts,
    retry_status=None,
    error_classification=None,
    fallback_used=False,
):
    if not isinstance(result, dict):
        result = {}
    if attempts or retry_status is not None or error_classification is not None or fallback_used:
        result.setdefault("auxiliary_attempts", attempts)
    if retry_status is not None:
        result.setdefault("auxiliary_retry_status", retry_status)
    if error_classification is not None:
        result.setdefault("auxiliary_error_classification", error_classification)
    if fallback_used:
        result["fallback_used"] = True
    return result

def _auxiliary_error_result(first, *, attempts, retry_status, error_classification, error):
    result = {}
    if isinstance(first, dict):
        for key in (
            "summary_nodes_created",
            "summary_nodes",
            "replay_messages",
            "replay_token_estimate",
            "replay_over_budget",
            "frontier",
            "summary_request",
        ):
            if key in first:
                result[key] = first[key]
    result.setdefault("summary_nodes_created", 0)
    result.setdefault("summary_nodes", [])
    result.setdefault("replay_messages", [])
    result.setdefault("frontier", {"current_frontier_store_id": None, "maintenance_debt": []})
    result["status"] = "error"
    result["reason"] = (
        "auxiliary_summary_permanent_failure"
        if error_classification == "permanent"
        else "auxiliary_summary_retry_exhausted"
    )
    result["error"] = str(error)
    return _with_auxiliary_metadata(
        result,
        attempts=attempts,
        retry_status=retry_status,
        error_classification=error_classification,
    )

def _bounded_expand_query_answer(text: str, max_tokens: int):
    try:
        token_budget = int(max_tokens or 2000)
    except Exception:
        token_budget = 2000
    char_limit = max(1, token_budget) * 4
    answer = (text or "").strip()
    if len(answer) <= char_limit:
        return answer, False
    return answer[:char_limit].rstrip(), True

def _expand_query_degraded_payload(retrieval, reason: str, *, timeout_seconds=None):
    payload = {}
    if isinstance(retrieval, dict):
        for key in (
            "status",
            "prompt",
            "query",
            "model",
            "max_tokens",
            "context_max_tokens",
            "context_truncated",
            "context_pagination",
            "node_ids",
            "matches",
            "provider",
            "session_id",
            "storage_scope",
        ):
            if key in retrieval:
                payload[key] = retrieval[key]
    payload["status"] = payload.get("status") or "ok"
    payload["needs_synthesis"] = False
    payload["degraded"] = True
    payload["error"] = reason
    if timeout_seconds is not None:
        payload["timeout_seconds"] = timeout_seconds
    return payload

def _synthesize_expand_query_payload(retrieval, agent=None, **kwargs):
    if not isinstance(retrieval, dict) or not retrieval.get("needs_synthesis"):
        return retrieval
    client = getattr(agent, "auxiliary_client", None)
    if client is None or not callable(getattr(client, "call_llm", None)):
        return _expand_query_degraded_payload(
            retrieval,
            "Hermes auxiliary_client.call_llm is unavailable",
        )

    synthesis_prompt = retrieval.get("synthesis_prompt") or {}
    context_blocks = retrieval.get("context_blocks") or []
    system_prompt = synthesis_prompt.get("system") or (
        "You answer questions using expanded LCM retrieval context. "
        "Be concise, factual, and grounded in the provided context. "
        "If the context is insufficient, say so plainly."
    )
    user_prompt = synthesis_prompt.get("user") or (
        f"QUESTION:\n{retrieval.get('prompt', '')}\n\n"
        "EXPANDED CONTEXT:\n"
        f"{json.dumps(context_blocks, ensure_ascii=False, indent=2)}"
    )
    max_tokens = retrieval.get("max_tokens") or kwargs.get("max_tokens") or 2000
    timeout = kwargs.get("timeout") or kwargs.get("expansion_timeout") or 60
    call_kwargs = {
        "task": "compression",
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt},
        ],
        "max_tokens": max_tokens,
        "timeout": timeout,
    }
    model = kwargs.get("model") or retrieval.get("model")
    if model:
        call_kwargs["model"] = model
    try:
        response = client.call_llm(**call_kwargs)
    except TimeoutError:
        return _expand_query_degraded_payload(
            retrieval,
            f"lcm_expand_query synthesis timed out after {float(timeout):.3g}s",
            timeout_seconds=timeout,
        )

    answer = _strip_reasoning(_llm_response_text(response)).strip()
    if not answer:
        return _expand_query_degraded_payload(
            retrieval,
            "lcm_expand_query synthesis returned an empty answer",
        )
    bounded_answer, truncated = _bounded_expand_query_answer(answer, max_tokens)
    payload = dict(retrieval)
    payload.pop("context_blocks", None)
    payload.pop("synthesis_prompt", None)
    payload["status"] = payload.get("status") or "ok"
    payload["needs_synthesis"] = False
    payload["answer"] = bounded_answer
    if truncated:
        payload["answer_truncated"] = True
    return payload

def _handle_lcm_expand_query(args, **kwargs) -> str:
    retrieval = call_tokensave_json("tokensave_lcm_expand_query", args or {}, **kwargs)
    agent = kwargs.get("agent")
    payload = _synthesize_expand_query_payload(retrieval, agent=agent, **kwargs)
    return json.dumps(payload)

def _copy_without_none(source: dict) -> dict:
    return {key: value for key, value in source.items() if value is not None}

def _tokens_from_native_max(max_tokens):
    if max_tokens is None:
        return None
    try:
        return max(1, min(8192, int(max_tokens) * 4))
    except Exception:
        return None

def _native_expand_target(args: dict):
    provided = [key for key in ("node_id", "store_id", "externalized_ref") if args.get(key) is not None]
    if len(provided) > 1:
        return None, "lcm_expand expects exactly one of node_id, store_id, or externalized_ref"
    if not provided:
        return None, None
    key = provided[0]
    if key == "node_id":
        return {"kind": "summary_node", "node_id": str(args[key])}, None
    if key == "store_id":
        return {"kind": "raw_message", "store_id": args[key]}, None
    return {"kind": "external_payload", "payload_ref": args[key]}, None

def _native_describe_target(args: dict):
    provided = [key for key in ("node_id", "externalized_ref") if args.get(key) is not None]
    if len(provided) > 1:
        return None, "lcm_describe expects at most one of node_id or externalized_ref"
    if not provided:
        return {"kind": "session"}, None
    key = provided[0]
    if key == "node_id":
        return {"kind": "summary_node", "node_id": str(args[key])}, None
    return {"kind": "external_payload", "payload_ref": args[key]}, None

def _translate_lcm_args(native_name: str, args: dict) -> dict:
    translated = dict(args or {})
    if native_name == "lcm_grep":
        if "session_scope" in translated:
            translated["scope"] = translated.pop("session_scope")
        else:
            translated.setdefault("scope", "current")
        if "time_from" in translated:
            translated["start_time"] = translated.pop("time_from")
        if "time_to" in translated:
            translated["end_time"] = translated.pop("time_to")
        return translated
    if native_name == "lcm_load_session":
        if "max_content_chars" in translated:
            translated["content_limit"] = translated.pop("max_content_chars")
        if "time_from" in translated:
            translated["start_time"] = translated.pop("time_from")
        if "time_to" in translated:
            translated["end_time"] = translated.pop("time_to")
        return translated
    if native_name == "lcm_describe":
        if "target" not in translated:
            target, error = _native_describe_target(translated)
            if error is not None:
                return {"error": error}
            translated["target"] = target
        translated.pop("node_id", None)
        translated.pop("externalized_ref", None)
        return translated
    if native_name == "lcm_expand":
        if "target" not in translated:
            target, error = _native_expand_target(translated)
            if error is not None:
                return {"error": error}
            if target is not None:
                translated["target"] = target
        for public_key in ("node_id", "store_id", "externalized_ref"):
            translated.pop(public_key, None)
        content_limit = _tokens_from_native_max(translated.pop("max_tokens", None))
        if content_limit is not None and "content_limit" not in translated:
            translated["content_limit"] = content_limit
        translated.pop("source_offset", None)
        translated.pop("source_limit", None)
        return translated
    return translated

class TokenSaveContextEngine(ContextEngine):
    def __init__(self, config=None, hermes_home=None):
        self.active_session_id = None
        self.config = config
        self.hermes_home = _resolve_hermes_home(config, hermes_home)
        self.project_root = None
        self.agent = None
        self._route_failures = {}
        self._cooldown_until = {}

    @property
    def name(self) -> str:
        return "lcm"

    def _bind_session(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        if session_id is not None:
            self.active_session_id = session_id
        if kwargs.get("config") is not None:
            self.config = kwargs.get("config")
        next_agent = kwargs.get("agent")
        if next_agent is not None:
            self.agent = next_agent
        explicit_hermes_home = hermes_home or kwargs.get("hermes_home")
        if explicit_hermes_home or kwargs.get("config") is not None or self.hermes_home is None:
            next_hermes_home = _resolve_hermes_home(
                kwargs.get("config", self.config),
                explicit_hermes_home,
            )
            if next_hermes_home:
                self.hermes_home = next_hermes_home
        next_project_root = project_root or kwargs.get("project_root") or kwargs.get("cwd")
        if next_project_root:
            self.project_root = next_project_root

    def initialize(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        self._bind_session(session_id, hermes_home, project_root, **kwargs)

    def on_session_start(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        bound_session_id = self.active_session_id
        self._bind_session(session_id, hermes_home, project_root, **kwargs)
        self._report_compression_boundary(session_id, bound_session_id, kwargs)

    def _report_compression_boundary(self, session_id, bound_session_id, kwargs):
        # Mirrors Hermes' compression-boundary session starts: hand the
        # bound/old session ids to tokensave so it can record a boundary-skip
        # cooldown when carry-over did not continue from the bound session.
        boundary_reason = str(kwargs.get("boundary_reason") or "")
        old_session_id = str(kwargs.get("old_session_id") or "")
        if (
            boundary_reason != "compression"
            or not old_session_id
            or not session_id
            or old_session_id == session_id
        ):
            return
        args = _storage_args(self.project_root, self.hermes_home)
        args.update({
            "session_id": session_id,
            "old_session_id": old_session_id,
            "boundary_reason": boundary_reason,
        })
        if bound_session_id:
            args["bound_session_id"] = bound_session_id
        try:
            tools.call_tokensave_tool("tokensave_lcm_session_boundary", args)
        except Exception as exc:
            logger.warning("LCM session boundary report failed: %s", exc)

    def should_compress_preflight(self, messages, current_tokens=None, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args.update(_lcm_config_args(self.config))
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
            "current_tokens": current_tokens,
        })
        _apply_lcm_option_overrides(args, kwargs, (
            "threshold_tokens",
            "max_assembly_tokens",
            "leaf_chunk_tokens",
            "max_source_messages",
            "summary_fan_in",
            "fresh_tail_count",
            "dynamic_leaf_chunk_enabled",
            "dynamic_leaf_chunk_max",
            "context_length",
            "reserve_tokens_floor",
            "ignore_session_patterns",
            "stateless_session_patterns",
            "ignore_message_patterns",
        ))
        return call_tokensave_json("tokensave_lcm_preflight", args, **kwargs)

    def status(self, session_id=None, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args["session_id"] = session_id or self.active_session_id
        return call_tokensave_json("tokensave_lcm_status", args, **kwargs)

    def get_tool_schemas(self):
        return _lcm_tool_schemas()

    def get_status(self):
        storage = _storage_args(self.project_root, self.hermes_home)
        return {
            "engine": self.name,
            "session_id": self.active_session_id,
            "storage_scope": storage.get("storage_scope"),
            "hermes_home": self.hermes_home,
            "project_root": self.project_root,
            "context_engine_tool_names": sorted(
                schema["name"] for schema in self.get_tool_schemas()
            ),
            "route_failures": dict(self._route_failures),
            "cooldown_routes": sorted(self._cooldown_until.keys()),
        }

    def _current_turn_preflight(self, messages, **kwargs):
        if not messages or not self.active_session_id:
            return
        args = _storage_args(self.project_root, self.hermes_home)
        args.update(_lcm_config_args(self.config))
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
        })
        _apply_lcm_option_overrides(args, kwargs, (
            "current_tokens",
            "threshold_tokens",
            "max_assembly_tokens",
            "leaf_chunk_tokens",
            "max_source_messages",
            "summary_fan_in",
            "fresh_tail_count",
            "dynamic_leaf_chunk_enabled",
            "dynamic_leaf_chunk_max",
            "context_length",
            "reserve_tokens_floor",
            "ignore_session_patterns",
            "stateless_session_patterns",
            "ignore_message_patterns",
        ))
        try:
            tools.call_tokensave_tool("tokensave_lcm_preflight", args, **_copy_without_none({
                "project_root": kwargs.get("project_root"),
            }))
        except Exception as exc:
            logger.warning("LCM current-turn preflight failed: %s", exc)

    def handle_tool_call(self, name, arguments=None, **kwargs) -> str:
        tool_name, tool_args = _normalize_memory_tool_call(name, arguments)
        native_name = tool_name
        tokensave_name = LCM_TOOL_ALIASES.get(native_name)
        if tokensave_name is None and native_name in LCM_DIRECT_TOOL_NAMES:
            tokensave_name = native_name
            native_name = LCM_DIRECT_TO_NATIVE.get(native_name, native_name)
        if tokensave_name is None and tool_name in LCM_DIRECT_TOOL_NAMES:
            tokensave_name = tool_name
        if tokensave_name is None:
            return tools.error_payload(f"unknown LCM tool: {tool_name}")

        messages = kwargs.get("messages")
        preflight_kwargs = dict(kwargs)
        preflight_kwargs.pop("messages", None)
        self._current_turn_preflight(messages, **preflight_kwargs)

        tool_args = _translate_lcm_args(native_name, dict(tool_args))
        if tool_args.get("error"):
            return json.dumps({"error": tool_args["error"]})
        storage_args = _storage_args(self.project_root, self.hermes_home)
        for key, value in storage_args.items():
            tool_args.setdefault(key, value)
        if self.active_session_id:
            tool_args.setdefault("session_id", self.active_session_id)

        if tokensave_name == "tokensave_lcm_expand_query":
            return _handle_lcm_expand_query(tool_args, agent=self.agent, **preflight_kwargs)
        return tools.call_tokensave_tool(tokensave_name, tool_args, **preflight_kwargs)

    def expand_query(self, prompt, query=None, node_ids=None, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args["session_id"] = kwargs.pop("session_id", None) or self.active_session_id
        args["prompt"] = prompt
        if query is not None:
            args["query"] = query
        if node_ids is not None:
            args["node_ids"] = node_ids
        for key in ("max_results", "max_tokens", "context_max_tokens"):
            if key in kwargs and kwargs[key] is not None:
                args[key] = kwargs[key]
        retrieval = call_tokensave_json("tokensave_lcm_expand_query", args, **kwargs)
        return _synthesize_expand_query_payload(retrieval, agent=self.agent, **kwargs)

    def _auxiliary_routes(self, summary_request=None, **kwargs):
        routes = (
            kwargs.get("routes")
            or kwargs.get("auxiliary_routes")
            or (summary_request or {}).get("routes")
        )
        if isinstance(routes, dict):
            routes = [routes]
        if not routes:
            route = {}
            for key in ("model", "temperature", "max_tokens", "timeout"):
                if kwargs.get(key) is not None:
                    route[key] = kwargs[key]
            routes = [route]
        normalized = []
        for route in routes:
            if not isinstance(route, dict):
                route = {"model": str(route)}
            normalized.append(route)
        return normalized

    def _call_auxiliary_summary(self, prompt, messages, **kwargs):
        client = getattr(getattr(self, "agent", None), "auxiliary_client", None)
        summary_request = kwargs.get("summary_request")
        allow_retry_signal = bool(kwargs.pop("allow_retry_signal", False))
        route_kwargs = dict(kwargs)
        route_kwargs.pop("summary_request", None)
        routes = self._auxiliary_routes(summary_request, **route_kwargs)
        last_error = None
        last_classification = None
        if client is None or not callable(getattr(client, "call_llm", None)):
            last_error = RuntimeError("Hermes auxiliary_client.call_llm is unavailable")
        else:
            now = time.time()
            for route in routes:
                route_name = route.get("route") or route.get("name") or route.get("model") or "default"
                route_key = str(route_name)
                if not allow_retry_signal and self._cooldown_until.get(route_key, 0) > now:
                    continue
                call_kwargs = {
                    "task": "compression",
                    "messages": [{"role": "system", "content": prompt}, *list(messages or [])],
                    "temperature": route.get("temperature", 0.1),
                    "max_tokens": route.get("max_tokens", 2048),
                    "timeout": route.get("timeout", 60),
                }
                model = route.get("model")
                if model is not None:
                    call_kwargs["model"] = model
                try:
                    response = client.call_llm(**call_kwargs)
                    text = _strip_reasoning(_llm_response_text(response))
                    if not text:
                        raise RuntimeError("Hermes auxiliary summary was empty")
                    return {
                        "status": "ok",
                        "text": text,
                        "route": route_key,
                        "model": model,
                    }
                except Exception as exc:
                    last_error = exc
                    last_classification = _auxiliary_error_classification(exc)
                    failures = self._route_failures.get(route_key, 0) + 1
                    self._route_failures[route_key] = failures
                    self._cooldown_until[route_key] = time.time() + min(300, 2 ** failures)
        if last_error is not None:
            last_classification = last_classification or _auxiliary_error_classification(last_error)
            if allow_retry_signal and last_classification == "retry_worthy":
                return {
                    "status": "retry",
                    "error": str(last_error),
                    "error_classification": last_classification,
                }
            if allow_retry_signal and last_classification == "permanent":
                return {
                    "status": "error",
                    "error": str(last_error),
                    "error_classification": last_classification,
                }
        fallback = {
            "status": "fallback",
            "text": _deterministic_truncation(messages),
            "route": "deterministic_fallback",
            "model": None,
        }
        if last_error is not None:
            fallback["error"] = str(last_error)
            fallback["error_classification"] = last_classification
        return fallback

    def compress(self, messages, current_tokens=None, focus_topic=None, **kwargs):
        summarizer = kwargs.pop("summarizer", None) or {"mode": "hermes_auxiliary"}
        max_auxiliary_attempts = _auxiliary_retry_limit(kwargs)
        lcm_option_keys = (
            "expected_current_frontier_store_id",
            "threshold_tokens",
            "max_assembly_tokens",
            "leaf_chunk_tokens",
            "max_source_messages",
            "summary_fan_in",
            "fresh_tail_count",
            "dynamic_leaf_chunk_enabled",
            "dynamic_leaf_chunk_max",
            "context_length",
            "reserve_tokens_floor",
            "ignore_session_patterns",
            "stateless_session_patterns",
            "ignore_message_patterns",
        )
        args = _storage_args(self.project_root, self.hermes_home)
        args.update(_lcm_config_args(self.config))
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
            "current_tokens": current_tokens,
            "focus_topic": focus_topic,
            "summarizer": summarizer,
        })
        _apply_lcm_option_overrides(args, kwargs, lcm_option_keys)

        attempts = 0
        retry_status = None
        error_classification = None
        fallback_used = False
        attempt_args = dict(args)

        while attempts < max_auxiliary_attempts:
            first = call_tokensave_json("tokensave_lcm_compress", attempt_args, **kwargs)
            if first.get("status") != "needs_summary":
                return _with_auxiliary_metadata(
                    first,
                    attempts=attempts,
                    retry_status=retry_status,
                    error_classification=error_classification,
                    fallback_used=fallback_used,
                )

            summary_request = first.get("summary_request") or {}
            source_messages = _summary_source_messages(
                summary_request.get("source_messages") or messages
            )
            attempts += 1
            summary = self._call_auxiliary_summary(
                summary_request.get("prompt") or "Summarize the conversation so far.",
                source_messages,
                summary_request=summary_request,
                allow_retry_signal=True,
                **kwargs,
            )
            summary_status = summary.get("status")
            if summary_status in ("retry", "error"):
                error_classification = summary.get("error_classification") or (
                    "retry_worthy" if summary_status == "retry" else "permanent"
                )
                smaller_limit = _next_smaller_source_limit(
                    source_messages,
                    attempt_args.get("max_source_messages"),
                )
                if (
                    summary_status == "retry"
                    and smaller_limit is not None
                    and attempts < max_auxiliary_attempts
                ):
                    retry_status = "retried"
                    attempt_args = dict(args)
                    attempt_args["max_source_messages"] = smaller_limit
                    continue
                retry_status = "retry_exhausted" if summary_status == "retry" else "not_retryable"
                return _auxiliary_error_result(
                    first,
                    attempts=attempts,
                    retry_status=retry_status,
                    error_classification=error_classification,
                    error=summary.get("error"),
                )

            source_size = _summary_source_size(source_messages)
            if (
                summary_status == "ok"
                and source_size > 0
                and len(summary.get("text", "")) >= source_size
            ):
                error_classification = "non_compressing_summary"
                smaller_limit = _next_smaller_source_limit(
                    source_messages,
                    attempt_args.get("max_source_messages"),
                )
                if smaller_limit is not None and attempts < max_auxiliary_attempts:
                    retry_status = "retried"
                    attempt_args = dict(args)
                    attempt_args["max_source_messages"] = smaller_limit
                    continue
                fallback_used = True
                retry_status = "fallback_summary"
                summary = {
                    "status": "fallback",
                    "text": _deterministic_truncation(source_messages),
                    "route": "deterministic_fallback",
                    "model": None,
                    "error": "Hermes auxiliary summary was not smaller than source",
                    "error_classification": error_classification,
                }
            elif summary_status == "fallback":
                fallback_used = True
                retry_status = retry_status or "fallback_summary"
                error_classification = summary.get("error_classification") or error_classification

            provided_args = dict(attempt_args)
            provided_args["summarizer"] = {
                "mode": "provided",
                "summary_text": summary["text"],
                "route": summary.get("route"),
            }
            result = call_tokensave_json("tokensave_lcm_compress", provided_args, **kwargs)
            return _with_auxiliary_metadata(
                result,
                attempts=attempts,
                retry_status=retry_status,
                error_classification=error_classification,
                fallback_used=fallback_used,
            )

        return _auxiliary_error_result(
            {"frontier": {"current_frontier_store_id": None, "maintenance_debt": []}},
            attempts=attempts,
            retry_status="retry_exhausted",
            error_classification=error_classification or "retry_worthy",
            error="auxiliary retry budget exhausted",
        )

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

    context_config = getattr(ctx, "config", None)
    context_hermes_home = (
        getattr(ctx, "hermes_home", None)
        or getattr(ctx, "_hermes_home", None)
    )
    context_engine = TokenSaveContextEngine(
        config=context_config,
        hermes_home=context_hermes_home,
    )
    if callable(getattr(ctx, "register_context_engine", None)):
        ctx.register_context_engine(context_engine)

    register_tool = getattr(ctx, "register_tool", None)
    if callable(register_tool) and _host_forwards_registered_tool_messages(ctx):
        for schema in schemas.TOOL_SCHEMAS:
            name = schema["name"]
            handler = _handle_lcm_expand_query if name == "tokensave_lcm_expand_query" else tools.make_handler(name)
            try:
                register_tool(
                    name=name,
                    toolset="tokensave",
                    schema=schema,
                    handler=handler,
                )
            except Exception as exc:
                logger.warning(
                    "tokensave tool registration failed for %s; continuing: %s",
                    name,
                    exc,
                )
        for schema in context_engine.get_tool_schemas():
            name = schema["name"]
            try:
                register_tool(
                    name=name,
                    toolset="context_engine",
                    schema=schema,
                    handler=_make_wrapped_lcm_handler(name, context_engine),
                    description=schema.get("description", ""),
                )
            except Exception as exc:
                logger.warning(
                    "tokensave LCM tool registration failed for %s; continuing with context-engine schemas: %s",
                    name,
                    exc,
                )
    elif callable(register_tool):
        logger.info(
            "tokensave direct tool registration skipped because this Hermes host does not advertise message forwarding"
        )
    else:
        logger.info(
            "tokensave direct tool registration unavailable on this Hermes host; continuing with context-engine schemas"
        )

    skills_dir = Path(__file__).parent / "skills"
    skill_path = skills_dir / "tokensave" / "SKILL.md"
    register_skill = getattr(ctx, "register_skill", None)
    if skill_path.exists() and callable(register_skill):
        register_skill("tokensave:tokensave", skill_path)
"#
    .to_string()
}

const HERMES_SKILL: &str = r"---
name: tokensave
description: Prefer tokensave tools for codebase exploration and graph queries.
---

# Use tokensave

Use tokensave tools before broad file reads for codebase exploration, symbol lookup,
call graph traversal, impact analysis, affected files, and architectural navigation.
";
