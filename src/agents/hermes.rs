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
            ctx.project_root.as_deref(),
            ctx.dashboard,
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
        install_plugin(
            &plugin_dir,
            &ctx.tokensave_bin,
            ctx.project_root.as_deref(),
            ctx.dashboard,
        )?;
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

// Profile names are deliberately restricted to ASCII `[a-z0-9][a-z0-9_-]`:
// non-ASCII names are rejected outright (clear error below), so Unicode
// normalization concerns (NFC vs NFD on macOS filesystems producing
// colliding/diverging directory names) cannot arise by construction.
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
    let existing: Vec<&PathBuf> = candidates.iter().filter(|plugin| plugin.exists()).collect();
    let Some(first) = existing.first() else {
        if let Some(plugin) = candidates.first() {
            dc.warn(&format!(
                "{} not found — run `tokensave install --agent hermes` if you use Hermes",
                plugin.display()
            ));
        } else {
            dc.warn("Hermes tokensave plugin not found — run `tokensave install --agent hermes` if you use Hermes");
        }
        return;
    };
    dc.pass(&format!(
        "Hermes tokensave plugin found at {}",
        first.display()
    ));

    for manifest_path in &existing {
        let Some(plugin_dir) = manifest_path.parent() else {
            continue;
        };
        // Stale generated plugins keep working but miss new tools/config
        // surfaces; `hermes plugins list` shows the same manifest version.
        match read_manifest_version(manifest_path) {
            Some(version) if version == env!("CARGO_PKG_VERSION") => {}
            Some(version) => dc.warn(&format!(
                "{} was generated by tokensave {version} (installed binary is {}) — re-run `tokensave install --agent hermes` to refresh it",
                manifest_path.display(),
                env!("CARGO_PKG_VERSION"),
            )),
            None => dc.warn(&format!(
                "{} has no manifest version — re-run `tokensave install --agent hermes` to refresh it",
                manifest_path.display(),
            )),
        }
        // A pinned project that no longer exists makes every Hermes tool
        // call fail; surface it here instead of at first tool use.
        if let Some(pin) = effective_pinned_project_root(plugin_dir) {
            if !Path::new(&pin).is_dir() {
                dc.warn(&format!(
                    "{} pins project_root {pin}, which does not exist — re-run `tokensave install --agent hermes --project-root <path>`",
                    plugin_dir.display(),
                ));
            }
        }
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

/// Reads `plugins.tokensave.project_root` from a Hermes profile config.yaml.
///
/// This is the single source of truth for the pin (the same
/// `plugins.<name>` block bundled Hermes plugins use): install writes it,
/// reinstalls preserve it, and the generated Python resolves it at runtime.
fn read_config_pinned_project_root(config_path: &Path) -> Option<String> {
    let config = std::fs::read_to_string(config_path).ok()?;
    let lines: Vec<&str> = config.lines().collect();
    let (plugins_start, plugins_end) = find_top_level_section_in(&lines, "plugins")?;
    let TokensaveBlock::Block { start, end } =
        find_tokensave_block_in(&lines, plugins_start, plugins_end)?
    else {
        return None;
    };
    let value = lines
        .iter()
        .take(end)
        .skip(start + 1)
        .find_map(|line| line.trim().strip_prefix("project_root:"))?
        .trim();
    parse_yaml_scalar(value)
}

/// Decodes a single-line YAML scalar (double-quoted, single-quoted, or plain).
fn parse_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).ok();
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Some(value[1..value.len() - 1].replace("''", "'"));
    }
    Some(value.to_string())
}

/// The pin currently in effect for a generated plugin: the
/// `plugins.tokensave.project_root` key of the profile config.yaml.
fn effective_pinned_project_root(plugin_dir: &Path) -> Option<String> {
    let config_path = plugin_dir.parent()?.parent()?.join("config.yaml");
    read_config_pinned_project_root(&config_path)
}

/// Reads the `version:` field from a generated plugin.yaml manifest.
fn read_manifest_version(manifest_path: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(manifest_path).ok()?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version:"))
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

fn install_plugin(
    plugin_dir: &Path,
    tokensave_bin: &str,
    project_root: Option<&Path>,
    deploy_dashboard: bool,
) -> Result<()> {
    // An explicit pin wins; otherwise preserve whatever pin a previous
    // install wrote to the config block, so `tokensave reinstall` does not
    // silently unpin.
    let pinned_project_root = match project_root {
        Some(path) => Some(path.display().to_string()),
        None => effective_pinned_project_root(plugin_dir),
    };
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
    // The dashboard plugin page is part of the default install; the
    // `--no-dashboard` opt-out also removes a previously deployed page so
    // the flag is a real toggle rather than install-order dependent.
    if deploy_dashboard {
        super::hermes_dashboard::install_dashboard(
            plugin_dir,
            tokensave_bin,
            pinned_project_root.as_deref(),
        )?;
    } else {
        super::hermes_dashboard::uninstall_dashboard(plugin_dir)?;
    }
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        let config_path = profile_dir.join("config.yaml");
        enable_plugin(&config_path, pinned_project_root.as_deref())?;
    }

    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote Hermes tokensave plugin to {}",
        plugin_dir.display()
    );
    Ok(())
}

fn enable_plugin(config_path: &Path, pinned_project_root: Option<&str>) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = enable_plugin_config(&existing, pinned_project_root).map_err(|message| {
        TokenSaveError::Config {
            message: format!(
                "{message} in {}.\nFix the config by hand, then re-run: tokensave install --agent hermes",
                config_path.display()
            ),
        }
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
    super::hermes_dashboard::uninstall_dashboard(plugin_dir)?;

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

fn enable_plugin_config(
    existing: &str,
    pinned_project_root: Option<&str>,
) -> std::result::Result<String, String> {
    let enabled = enable_plugin_list_config(existing)?;
    let with_memory = enable_memory_provider_config(&enabled)?;
    let with_engine = enable_context_engine_config(&with_memory)?;
    match pinned_project_root {
        Some(pin) => set_pinned_project_root_config(&with_engine, pin),
        None => Ok(with_engine),
    }
}

fn enable_plugin_list_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("plugins:\n  enabled:\n    - tokensave\n".to_string());
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
        return Ok(out);
    }

    let (plugins_start, plugins_end) = find_top_level_section(existing, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    match find_child_section_from_strings(&lines, plugins_start, plugins_end, "disabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        ChildSection::Block { start, end } => {
            lines = remove_list_item(lines, start, end, "tokensave");
        }
        ChildSection::Missing | ChildSection::EmptyFlow { .. } => {}
    }

    let (plugins_start, plugins_end) = find_top_level_section_from_strings(&lines, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    match find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        ChildSection::Block { start, end } => {
            if !list_contains_item_strings(&lines, start, end, "tokensave") {
                // Match the existing list's item indentation (Hermes writes
                // 2-space items); only default to 4 when the list is empty.
                let indent = list_item_indent(&lines, start, end).unwrap_or(4);
                lines.insert(start + 1, format!("{}- tokensave", " ".repeat(indent)));
            }
        }
        ChildSection::EmptyFlow { line } => {
            // Rewrite `enabled: []` into a block list containing tokensave.
            lines[line] = "  enabled:".to_string();
            lines.insert(line + 1, "    - tokensave".to_string());
        }
        ChildSection::Missing => {
            lines.insert(plugins_start + 1, "  enabled:".to_string());
            lines.insert(plugins_start + 2, "    - tokensave".to_string());
        }
    }

    Ok(join_lines(&lines, had_trailing_newline))
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
    match find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        ChildSection::Block { start, end } => {
            lines = remove_list_item(lines, start, end, "tokensave");
        }
        ChildSection::Missing | ChildSection::EmptyFlow { .. } => {}
    }
    let without_pin = remove_pinned_project_root_config(&join_lines(&lines, had_trailing_newline))?;
    let without_engine = disable_context_engine_config(&without_pin)?;
    disable_memory_provider_config(&without_engine)
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

/// Sets `context.engine: tokensave` so Hermes activates the registered
/// context engine (selection is config-driven; the host never auto-activates
/// plugin engines). The built-in default `compressor` is replaced; any other
/// configured engine is left alone with an error, mirroring the
/// memory-provider guard.
fn enable_context_engine_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("context:\n  engine: tokensave\n".to_string());
    }

    validate_top_level_section_shape(existing, "context")?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    let Some((context_start, context_end)) = find_top_level_section(existing, "context") else {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("context:\n  engine: tokensave\n");
        return Ok(out);
    };

    let engine_line = find_child_scalar_line(&lines, context_start, context_end, "engine")
        .ok_or_else(|| "unsupported Hermes context config".to_string())?;
    if let Some(engine_line) = engine_line {
        let current = lines[engine_line]
            .trim()
            .strip_prefix("engine:")
            .map(str::trim)
            .unwrap_or_default();
        match parse_yaml_scalar(current).as_deref() {
            None | Some("compressor") => {
                lines[engine_line] = "  engine: tokensave".to_string();
            }
            Some("tokensave") => {}
            Some(_) => {
                return Err(
                    "Hermes context engine already configured; refusing to overwrite it"
                        .to_string(),
                );
            }
        }
    } else {
        lines.insert(context_start + 1, "  engine: tokensave".to_string());
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

fn disable_context_engine_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }

    validate_top_level_section_shape(existing, "context")?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((context_start, context_end)) = find_top_level_section(existing, "context") else {
        return Ok(existing.to_string());
    };
    let engine_line = find_child_scalar_line(&lines, context_start, context_end, "engine")
        .ok_or_else(|| "unsupported Hermes context config".to_string())?;
    let mut removed_engine = false;
    if let Some(engine_line) = engine_line {
        if lines[engine_line].trim() == "engine: tokensave" {
            lines.remove(engine_line);
            removed_engine = true;
        }
    }
    if removed_engine {
        remove_empty_top_level_section(&mut lines, "context");
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

/// Shape of the `plugins.tokensave` child mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokensaveBlock {
    Missing,
    /// Block-style `tokensave:` at `start`; entries end (exclusive) at `end`.
    Block {
        start: usize,
        end: usize,
    },
    /// Flow-style empty mapping `tokensave: {}` on `line`.
    EmptyFlow {
        line: usize,
    },
}

fn find_tokensave_block_in(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
) -> Option<TokensaveBlock> {
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
            if trimmed == "tokensave:" {
                start = Some(idx);
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("tokensave:") {
                if rest.trim() == "{}" {
                    return Some(TokensaveBlock::EmptyFlow { line: idx });
                }
                return None;
            }
        }
    }
    let Some(start) = start else {
        return Some(TokensaveBlock::Missing);
    };
    // Entries live at indent >= 4; the block ends at the first non-blank,
    // non-comment line at indent <= 2 (a sibling plugins key or new section).
    let end = lines
        .iter()
        .enumerate()
        .take(plugins_end)
        .skip(start + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            (line_indent(line) <= 2).then_some(idx)
        })
        .unwrap_or(plugins_end);
    Some(TokensaveBlock::Block { start, end })
}

/// Writes `plugins.tokensave.project_root` — the conventional config home
/// for the install-time project pin. Expects the `plugins:` section to exist
/// (the enable chain creates it first).
fn set_pinned_project_root_config(
    existing: &str,
    pin: &str,
) -> std::result::Result<String, String> {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let value = serde_json::to_string(pin).map_err(|e| format!("unencodable project pin: {e}"))?;
    let pin_line = format!("    project_root: {value}");

    let (plugins_start, plugins_end) = find_top_level_section(existing, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    match find_tokensave_block_in(&borrowed, plugins_start, plugins_end)
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        TokensaveBlock::Missing => {
            lines.insert(plugins_start + 1, "  tokensave:".to_string());
            lines.insert(plugins_start + 2, pin_line);
        }
        TokensaveBlock::EmptyFlow { line } => {
            lines[line] = "  tokensave:".to_string();
            lines.insert(line + 1, pin_line);
        }
        TokensaveBlock::Block { start, end } => {
            let existing_pin = lines
                .iter()
                .enumerate()
                .take(end)
                .skip(start + 1)
                .find_map(|(idx, line)| line.trim().starts_with("project_root:").then_some(idx));
            match existing_pin {
                Some(idx) => lines[idx] = pin_line,
                None => lines.insert(start + 1, pin_line),
            }
        }
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

/// Removes `plugins.tokensave.project_root`, then the `tokensave:` block when
/// nothing else (user-added keys) remains in it.
fn remove_pinned_project_root_config(existing: &str) -> std::result::Result<String, String> {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((plugins_start, plugins_end)) = find_top_level_section(existing, "plugins") else {
        return Ok(existing.to_string());
    };
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let TokensaveBlock::Block { start, end } =
        find_tokensave_block_in(&borrowed, plugins_start, plugins_end)
            .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    else {
        return Ok(existing.to_string());
    };
    let Some(pin_idx) = lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start + 1)
        .find_map(|(idx, line)| line.trim().starts_with("project_root:").then_some(idx))
    else {
        return Ok(existing.to_string());
    };
    lines.remove(pin_idx);
    let block_is_empty = !lines
        .iter()
        .take(end - 1)
        .skip(start + 1)
        .any(|line| !line.trim().is_empty() && !line.trim().starts_with('#'));
    if block_is_empty {
        lines.remove(start);
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

fn validate_top_level_plugins_shape(existing: &str) -> std::result::Result<(), String> {
    validate_top_level_section_shape(existing, "plugins")
}

fn validate_top_level_memory_shape(existing: &str) -> std::result::Result<(), String> {
    validate_top_level_section_shape(existing, "memory")
}

fn validate_top_level_section_shape(existing: &str, key: &str) -> std::result::Result<(), String> {
    let target = format!("{key}:");
    let section_lines = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            line_indent(line) == 0 && !trimmed.starts_with('#') && trimmed.starts_with(&target)
        })
        .collect::<Vec<_>>();
    match section_lines.as_slice() {
        [] => Ok(()),
        [line] if line.trim() == target => Ok(()),
        _ => Err(format!(
            "unsupported Hermes {key} config; expected a block-style `{key}:` mapping"
        )),
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

/// Shape of a `plugins.<key>` child section found by
/// [`find_child_section_in`]. `None` from the finder means the config is
/// unsupported/ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildSection {
    /// The key is not present inside the parent section.
    Missing,
    /// Block-style `key:` at `start`; the section ends (exclusive) at `end`.
    Block { start: usize, end: usize },
    /// Flow-style empty list `key: []` (Hermes writes this) on `line`.
    EmptyFlow { line: usize },
}

fn find_child_section_from_strings(
    lines: &[String],
    plugins_start: usize,
    plugins_end: usize,
    key: &str,
) -> Option<ChildSection> {
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    find_child_section_in(&borrowed, plugins_start, plugins_end, key)
}

fn find_child_section_in(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    key: &str,
) -> Option<ChildSection> {
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
            if let Some(rest) = trimmed.strip_prefix(&target) {
                // `key: []` is a flow-style empty list; anything else after
                // the colon (flow lists with items, scalars) is unsupported.
                if rest.trim() == "[]" {
                    return Some(ChildSection::EmptyFlow { line: idx });
                }
                return None;
            }
        }
    }
    let Some(start) = start else {
        return Some(ChildSection::Missing);
    };
    // YAML allows sequence items at the same indent as the parent key
    // (`enabled:` followed by `  - item`), which Hermes itself writes. The
    // section therefore ends at the first line that is shallower than the
    // key, or at key depth without being a list item (e.g. a sibling key).
    let end = lines
        .iter()
        .enumerate()
        .take(plugins_end)
        .skip(start + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let indent = line_indent(line);
            (indent < 2 || (indent == 2 && !trimmed.starts_with("- "))).then_some(idx)
        })
        .unwrap_or(plugins_end);
    Some(ChildSection::Block { start, end })
}

/// Indent (in spaces) of the first `- ` list item inside a block section, if
/// the list already has items.
fn list_item_indent(lines: &[String], start: usize, end: usize) -> Option<usize> {
    lines
        .iter()
        .take(end)
        .skip(start + 1)
        .find(|line| line.trim().starts_with("- "))
        .map(|line| line_indent(line))
}

#[allow(clippy::option_option)]
fn find_memory_provider_line(
    lines: &[String],
    memory_start: usize,
    memory_end: usize,
) -> Option<Option<usize>> {
    find_child_scalar_line(lines, memory_start, memory_end, "provider")
}

/// Finds the `  <key>:` scalar line inside a top-level section.
///
/// Outer `None` means the section is unsupported (tab indentation); inner
/// `None` means the key is simply absent.
#[allow(clippy::option_option)]
fn find_child_scalar_line(
    lines: &[String],
    section_start: usize,
    section_end: usize,
    key: &str,
) -> Option<Option<usize>> {
    let target = format!("{key}:");
    for (idx, line) in lines
        .iter()
        .enumerate()
        .take(section_end)
        .skip(section_start + 1)
    {
        if line.trim_start().starts_with('\t') {
            return None;
        }
        if line_indent(line) == 2 && line.trim_start().starts_with(&target) {
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

pub(super) fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == contents {
        return Ok(());
    }
    // Write-to-.new-then-rename so a mid-write crash can never leave a
    // truncated/corrupt generated file behind (same pattern as
    // write_config_file, minus the backup — these files are regenerable).
    let new_path = PathBuf::from(format!("{}.new", path.display()));
    if let Err(e) = std::fs::write(&new_path, contents) {
        std::fs::remove_file(&new_path).ok();
        return Err(TokenSaveError::Config {
            message: format!("failed to write {}: {e}", new_path.display()),
        });
    }
    if let Err(e) = std::fs::rename(&new_path, path) {
        std::fs::remove_file(&new_path).ok();
        return Err(TokenSaveError::Config {
            message: format!(
                "failed to replace {} with {}: {e}",
                path.display(),
                new_path.display()
            ),
        });
    }
    Ok(())
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

pub(super) fn remove_generated_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", path.display()),
        }),
    }
}

pub(super) fn remove_empty_dir(path: &Path) -> Result<bool> {
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
    // `version` tracks the generating binary so `hermes plugins list` and
    // `tokensave doctor` can detect stale generated plugins after upgrades.
    format!(
        "name: tokensave\n\
         kind: standalone\n\
         version: {version}\n\
         description: TokenSave code-graph tools, memory provider, and LCM context engine for Hermes\n\
         author: tokensave (generated by `tokensave install --agent hermes`)\n\
         provides_tools:\n{tools}\n\
         provides_hooks:\n\
           - pre_llm_call\n\
         provides_commands:\n\
           - /tokensave_status\n",
        version = env!("CARGO_PKG_VERSION"),
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
import os
import subprocess

TOKENSAVE_BIN = {bin}
TOKENSAVE_TIMEOUT_SECONDS = 600
MAX_CAPTURE_CHARS = 4000

_PLUGIN_CONFIG_CACHE = {{}}

def plugin_config_block(hermes_home=None):
    """Return the `plugins.tokensave` mapping from the profile config.yaml.

    This is the conventional Hermes home for plugin settings (the same
    `plugins.<name>` block bundled plugins use). Cached per config file
    mtime so the hot tool-dispatch path stays cheap.
    """
    home = (
        hermes_home
        or os.environ.get("HERMES_HOME")
        or os.path.join(os.path.expanduser("~"), ".hermes")
    )
    path = os.path.join(str(home), "config.yaml")
    try:
        stat = os.stat(path)
        cache_key = (stat.st_mtime_ns, stat.st_size)
    except OSError:
        return {{}}
    cached = _PLUGIN_CONFIG_CACHE.get(path)
    if cached is not None and cached[0] == cache_key:
        return cached[1]
    block = {{}}
    try:
        import yaml
        with open(path, encoding="utf-8-sig") as config_file:
            config = yaml.safe_load(config_file) or {{}}
        plugins = config.get("plugins")
        candidate = plugins.get("tokensave") if isinstance(plugins, dict) else None
        if isinstance(candidate, dict):
            block = candidate
    except Exception:
        block = {{}}
    _PLUGIN_CONFIG_CACHE[path] = (cache_key, block)
    return block

def config_pinned_project_root(hermes_home=None):
    value = plugin_config_block(hermes_home).get("project_root")
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None

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
        # Install-time project pins (tokensave install --agent hermes
        # --project-root) live in the profile config.yaml under
        # plugins.tokensave.project_root; when set, every tool call resolves
        # that project's .tokensave/ stores regardless of the Hermes process
        # working directory.
        project_root = (
            kwargs.get("project_root")
            or tool_args.get("project_root")
            or config_pinned_project_root()
        )
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
import hashlib
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
                "session_id": {
                    "type": "string",
                    "description": "Optional session id override (for example, expand a cross-session grep hit in its owning session).",
                },
                "max_tokens": {"type": "integer", "description": "Token budget for returned content.", "default": 4000},
                "source_offset": {
                    "type": "integer",
                    "description": "Source pagination offset for node_id mode.",
                    "default": 0,
                },
                "source_limit": {
                    "type": "integer",
                    "description": "Maximum immediate sources to return from source_offset. If a returned source marks content_truncated=true, continue from its own store_id + content_offset.",
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
    if (
        not isinstance(content, list)
        or not content
        or not isinstance(content[0], dict)
        or not isinstance(content[0].get("text"), str)
    ):
        return {
            "error": "tokensave tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    text = content[0]["text"]
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

def _lcm_tool_schemas() -> list:
    return list(LCM_NATIVE_SCHEMAS)

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

# Conventional config home: a `plugins.tokensave` block in the profile
# config.yaml (the same `plugins.<name>` convention bundled Hermes plugins
# use). Keys are flat and mirror the host-config attribute names the
# `_configured_*` helpers read (project_root, summary_model, ...).
PLUGIN_CONFIG_DEFAULTS = {
    "project_root": "",
}

def _plugin_config_defaults():
    # The install-time pin lives in the profile config.yaml itself
    # (plugins.tokensave.project_root), so the defaults carry no pin.
    return dict(PLUGIN_CONFIG_DEFAULTS)

class _ConfigChain:
    """Attribute-style config wrapper layering plugins.tokensave under a host config object."""

    def __init__(self, primary, block):
        self._primary = primary
        self._block = dict(block)

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        value = getattr(self._primary, name, None)
        if value is None:
            value = self._block.get(name)
        if value is None:
            raise AttributeError(name)
        return value

def _with_plugin_block(config, hermes_home=None):
    """Layer the profile's plugins.tokensave config block under a host config.

    Host-provided values always win; the block fills the gaps so profile
    config.yaml settings reach the engine/provider without bespoke env vars.
    """
    block = {
        key: value
        for key, value in tools.plugin_config_block(hermes_home).items()
        if value is not None and value != ""
    }
    if not block:
        return config
    if config is None:
        return dict(block)
    if isinstance(config, dict):
        merged = dict(block)
        for key, value in config.items():
            if value is not None:
                merged[key] = value
        return merged
    return _ConfigChain(config, block)

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

def _configured_project_root(config):
    # Profiles can pin the indexed project via a `project_root` config key
    # (kwargs from the host take precedence; cwd is the last fallback).
    if config is None:
        return None
    if isinstance(config, dict):
        value = config.get("project_root") or config.get("tokensave_project_root")
        return str(value) if value else None
    for attr in ("project_root", "tokensave_project_root"):
        value = getattr(config, attr, None)
        if value:
            return str(value)
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
    except (TypeError, ValueError):
        return None

def _configured_bool(config, *names, default=None):
    value = _configured_value(config, *names, default=default)
    if value is None:
        return None
    if isinstance(value, str):
        return value.strip().lower() in ("1", "true", "yes", "on")
    return bool(value)

def _parse_pattern_list(raw):
    return [part.strip() for part in str(raw).split(",") if part.strip()]

# Env-aware settings mirroring hermes-lcm LCMConfig.from_env: documented LCM_*
# env vars take precedence over host ctx.config attributes, which take
# precedence over the hermes-lcm hardcoded defaults.

def _lcm_str_setting(config, env_key, *names, default=None):
    env_value = os.environ.get(env_key)
    if env_value is not None:
        return env_value
    value = _configured_value(config, *names)
    return value if value is not None else default

def _lcm_int_setting(config, env_key, *names, default=None):
    raw = os.environ.get(env_key)
    if raw is not None:
        try:
            return int(raw)
        except (TypeError, ValueError):
            pass
    return _configured_int(config, *names, default=default)

def _lcm_float_setting(config, env_key, *names, default=None):
    raw = os.environ.get(env_key)
    if raw is not None:
        try:
            return float(raw)
        except (TypeError, ValueError):
            pass
    value = _configured_value(config, *names)
    if value is not None:
        try:
            return float(value)
        except (TypeError, ValueError):
            pass
    return default

def _lcm_bool_setting(config, env_key, *names, default=None):
    raw = os.environ.get(env_key)
    if raw is not None:
        normalized = raw.strip().lower()
        if normalized in ("1", "true", "yes", "on"):
            return True
        if normalized in ("0", "false", "no", "off"):
            return False
    return _configured_bool(config, *names, default=default)

def _lcm_list_setting(config, env_key, *names, default=None):
    raw = os.environ.get(env_key)
    if raw is not None:
        return _parse_pattern_list(raw)
    value = _configured_value(config, *names)
    if value is None:
        return default
    if isinstance(value, str):
        return _parse_pattern_list(value)
    if isinstance(value, (list, tuple)):
        return [str(item).strip() for item in value if str(item).strip()]
    return default

def _config_bool_disabled(value):
    if isinstance(value, bool):
        return value is False
    if isinstance(value, (int, float)):
        return value == 0
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in ("0", "false", "no", "off"):
            return True
        try:
            return float(normalized) == 0
        except ValueError:
            return False
    return False

def _hermes_yaml_compression_threshold(default, hermes_home=None):
    # Port of hermes-lcm config._hermes_compression_threshold: read the main
    # Hermes compression.threshold from {HERMES_HOME}/config.yaml when no LCM
    # override exists. Disabled Hermes compression must not leak its threshold.
    home = (
        hermes_home
        or os.environ.get("HERMES_HOME")
        or os.path.join(os.path.expanduser("~"), ".hermes")
    )
    cfg_path = Path(home) / "config.yaml"
    try:
        text = cfg_path.read_text()
    except Exception:
        return default
    try:
        import yaml
    except Exception:
        yaml = None
    try:
        if yaml is not None:
            cfg = yaml.safe_load(text) or {}
            compression = cfg.get("compression") or {}
            if _config_bool_disabled(compression.get("enabled")):
                return default
            value = compression.get("threshold")
            if value is None:
                return default
            return float(value)

        in_compression = False
        direct_indent = None
        compression_disabled = False
        threshold_value = None
        for raw_line in text.splitlines():
            line = raw_line.split('#', 1)[0].rstrip()
            if not line.strip():
                continue
            if not line.startswith((" ", "\t")):
                in_compression = line.strip() == "compression:"
                direct_indent = None
                continue
            if not in_compression:
                continue
            indent = len(line) - len(line.lstrip(" \t"))
            if direct_indent is None:
                direct_indent = indent
            if indent != direct_indent or ":" not in line:
                continue
            key, raw_value = line.strip().split(":", 1)
            value = raw_value.strip().strip("'\"")
            if key == "enabled" and _config_bool_disabled(value):
                compression_disabled = True
            elif key == "threshold":
                threshold_value = value
        if compression_disabled or threshold_value is None:
            return default
        return float(threshold_value)
    except Exception:
        return default

def _hermes_yaml_auxiliary_compression_timeout_ms(default, hermes_home=None):
    # Port of hermes-lcm config._hermes_auxiliary_compression_timeout_ms:
    # read auxiliary.compression.timeout (seconds) from config.yaml and expose
    # it in milliseconds for LCM summary timeout parity.
    home = (
        hermes_home
        or os.environ.get("HERMES_HOME")
        or os.path.join(os.path.expanduser("~"), ".hermes")
    )
    cfg_path = Path(home) / "config.yaml"
    try:
        text = cfg_path.read_text()
    except Exception:
        return default
    try:
        import yaml
    except Exception:
        yaml = None
    try:
        if yaml is not None:
            cfg = yaml.safe_load(text) or {}
            auxiliary = cfg.get("auxiliary") or {}
            compression = auxiliary.get("compression") or {}
            value = compression.get("timeout")
            if value is None:
                return default
            return int(float(value) * 1000)

        in_auxiliary = False
        in_compression = False
        auxiliary_indent = None
        compression_indent = None
        for raw_line in text.splitlines():
            line = raw_line.split('#', 1)[0].rstrip()
            if not line.strip():
                continue
            indent = len(line) - len(line.lstrip(" \t"))
            stripped = line.strip()
            if indent == 0:
                in_auxiliary = stripped == "auxiliary:"
                in_compression = False
                auxiliary_indent = None
                compression_indent = None
                continue
            if not in_auxiliary:
                continue
            if auxiliary_indent is None:
                auxiliary_indent = indent
            if indent == auxiliary_indent:
                if stripped == "compression:":
                    in_compression = True
                    compression_indent = None
                    continue
                in_compression = False
                compression_indent = None
                continue
            if not in_compression:
                continue
            if compression_indent is None:
                compression_indent = indent
            if indent != compression_indent or ":" not in stripped:
                continue
            key, raw_value = stripped.split(":", 1)
            if key == "timeout":
                return int(float(raw_value.strip().strip("'\"")) * 1000)
        return default
    except Exception:
        return default

def _lcm_summary_timeout_ms(config, hermes_home=None):
    raw = os.environ.get("LCM_SUMMARY_TIMEOUT_MS")
    if raw is not None:
        try:
            return int(raw)
        except (TypeError, ValueError):
            pass
    configured = _configured_int(config, "summary_timeout_ms")
    if configured is not None:
        return configured
    return _hermes_yaml_auxiliary_compression_timeout_ms(60000, hermes_home=hermes_home)

def _summary_circuit_breaker_settings(config):
    threshold = _lcm_clamped_int_setting(
        config,
        "LCM_SUMMARY_CIRCUIT_BREAKER_FAILURE_THRESHOLD",
        "summary_circuit_breaker_failure_threshold",
        default=2,
        minimum=1,
    )
    cooldown = _lcm_clamped_int_setting(
        config,
        "LCM_SUMMARY_CIRCUIT_BREAKER_COOLDOWN_SECONDS",
        "summary_circuit_breaker_cooldown_seconds",
        default=300,
        minimum=0,
    )
    return threshold, cooldown

def _lcm_context_threshold(config, hermes_home=None):
    raw = os.environ.get("LCM_CONTEXT_THRESHOLD")
    if raw is not None:
        try:
            return float(raw)
        except (TypeError, ValueError):
            pass
    configured = _configured_value(config, "context_threshold")
    if configured is not None:
        try:
            return float(configured)
        except (TypeError, ValueError):
            pass
    return _hermes_yaml_compression_threshold(0.75, hermes_home=hermes_home)

def _configured_threshold_tokens(config, hermes_home=None, context_length_override=None):
    explicit = _configured_int(config, "threshold_tokens")
    if explicit is not None:
        return explicit
    context_length = context_length_override
    if context_length is None:
        context_length = _configured_int(
            config,
            "context_length",
            "max_context_tokens",
            "model_context_tokens",
        )
    if context_length is None:
        return None
    try:
        return int(int(context_length) * float(_lcm_context_threshold(config, hermes_home=hermes_home)))
    except (TypeError, ValueError):
        return None

def _lcm_config_args(config, hermes_home=None, runtime_context_length=None) -> dict:
    context_length = runtime_context_length
    if context_length is None:
        context_length = _configured_int(
            config,
            "context_length",
            "max_context_tokens",
            "model_context_tokens",
        )
    args = {
        "fresh_tail_count": _lcm_int_setting(config, "LCM_FRESH_TAIL_COUNT", "fresh_tail_count", default=64),
        "leaf_chunk_tokens": _lcm_int_setting(config, "LCM_LEAF_CHUNK_TOKENS", "leaf_chunk_tokens", default=20000),
        "dynamic_leaf_chunk_enabled": _lcm_bool_setting(
            config,
            "LCM_DYNAMIC_LEAF_CHUNK_ENABLED",
            "dynamic_leaf_chunk_enabled",
            default=False,
        ),
        "dynamic_leaf_chunk_max": _lcm_int_setting(
            config,
            "LCM_DYNAMIC_LEAF_CHUNK_MAX",
            "dynamic_leaf_chunk_max",
            default=40000,
        ),
        "max_assembly_tokens": _lcm_int_setting(config, "LCM_MAX_ASSEMBLY_TOKENS", "max_assembly_tokens", default=0),
        # Hermes derives an assembly cap of context_length - reserve_tokens_floor
        # when both are positive; pass both through so tokensave can apply the
        # same derivation (reserve_tokens_floor defaults to 0 = disabled).
        "reserve_tokens_floor": _lcm_int_setting(
            config,
            "LCM_RESERVE_TOKENS_FLOOR",
            "reserve_tokens_floor",
            default=0,
        ),
        "context_length": context_length,
        "summary_fan_in": _lcm_int_setting(
            config,
            "LCM_CONDENSATION_FANIN",
            "summary_fan_in",
            "condensation_fanin",
            default=4,
        ),
        # hermes-lcm caps condensation at depth 1 by default; pass the knob
        # through so the Rust engine can enforce the same ceiling.
        "incremental_max_depth": _lcm_int_setting(
            config,
            "LCM_INCREMENTAL_MAX_DEPTH",
            "incremental_max_depth",
            default=1,
        ),
    }
    threshold_tokens = _configured_threshold_tokens(
        config,
        hermes_home=hermes_home,
        context_length_override=context_length,
    )
    if threshold_tokens is not None:
        args["threshold_tokens"] = threshold_tokens
    for env_key, name in (
        ("LCM_IGNORE_SESSION_PATTERNS", "ignore_session_patterns"),
        ("LCM_STATELESS_SESSION_PATTERNS", "stateless_session_patterns"),
        ("LCM_IGNORE_MESSAGE_PATTERNS", "ignore_message_patterns"),
    ):
        patterns = _lcm_list_setting(config, env_key, name)
        if patterns:
            args[name] = patterns
    return {key: value for key, value in args.items() if value is not None}

def _lcm_expansion_model(config):
    value = _lcm_str_setting(config, "LCM_EXPANSION_MODEL", "expansion_model", default="")
    return str(value or "").strip()

def _lcm_clamped_int_setting(config, env_key, *names, default, minimum=1):
    value = _lcm_int_setting(config, env_key, *names, default=default)
    if value is None:
        value = default
    return max(minimum, int(value))

def _lcm_expansion_settings(config):
    return {
        "model": _lcm_expansion_model(config),
        "context_tokens": _lcm_clamped_int_setting(
            config,
            "LCM_EXPANSION_CONTEXT_TOKENS",
            "expansion_context_tokens",
            default=32000,
            minimum=1,
        ),
        "timeout_ms": _lcm_clamped_int_setting(
            config,
            "LCM_EXPANSION_TIMEOUT_MS",
            "expansion_timeout_ms",
            default=120000,
            minimum=1,
        ),
    }

def _lcm_expansion_context_tokens(config):
    return _lcm_expansion_settings(config)["context_tokens"]

def _lcm_expansion_timeout_ms(config):
    return _lcm_expansion_settings(config)["timeout_ms"]

def _lcm_extraction_settings(config):
    return {
        "enabled": bool(
            _lcm_bool_setting(
                config,
                "LCM_EXTRACTION_ENABLED",
                "extraction_enabled",
                default=False,
            )
        ),
        "model": str(
            _lcm_str_setting(config, "LCM_EXTRACTION_MODEL", "extraction_model", default="") or ""
        ).strip(),
        "output_path": str(
            _lcm_str_setting(
                config,
                "LCM_EXTRACTION_OUTPUT_PATH",
                "extraction_output_path",
                default="",
            )
            or ""
        ).strip(),
    }

def _lcm_extraction_enabled(config):
    return _lcm_extraction_settings(config)["enabled"]

def _lcm_extraction_model(config):
    return _lcm_extraction_settings(config)["model"]

def _lcm_extraction_output_path(config):
    return _lcm_extraction_settings(config)["output_path"]

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

def _messages_hash(messages):
    # Keep a full-content hash to preserve debounce correctness: any message
    # change must invalidate the signature and trigger preflight.
    try:
        payload = json.dumps(messages or [], sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    except Exception:
        payload = repr(messages)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()

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
        entry = {
            "role": message.get("role") or "user",
            "content": _message_content(message),
        }
        if message.get("tool_calls"):
            entry["tool_calls"] = message["tool_calls"]
        if message.get("tool_call_id"):
            entry["tool_call_id"] = message["tool_call_id"]
        normalized.append(entry)
    return normalized

_TOKEN_ENCODER = None
_TOKEN_ENCODER_CHECKED = False

def _token_encoder():
    global _TOKEN_ENCODER, _TOKEN_ENCODER_CHECKED
    if _TOKEN_ENCODER_CHECKED:
        return _TOKEN_ENCODER
    _TOKEN_ENCODER_CHECKED = True
    try:
        import tiktoken
        _TOKEN_ENCODER = tiktoken.get_encoding("cl100k_base")
    except Exception:
        _TOKEN_ENCODER = None
    return _TOKEN_ENCODER

def _count_tokens(text):
    # tiktoken when available; otherwise the same ceil(len/4) chars-per-token
    # estimate the Rust side uses everywhere ((len+3)//4 — see
    # dashboard/token_count.rs chars_estimate), so token numbers reported
    # from Hermes reconcile with dashboard numbers.
    if not text:
        return 0
    encoder = _token_encoder()
    if encoder is not None:
        try:
            return len(encoder.encode(text))
        except Exception:
            pass
    return (len(text) + 3) // 4

def _tool_call_arguments_text(arguments):
    if isinstance(arguments, str):
        return arguments
    if arguments is None:
        return ""
    try:
        return json.dumps(arguments, ensure_ascii=False)
    except Exception:
        return str(arguments)

def _count_message_tokens(message):
    total = 4
    if not isinstance(message, dict):
        return total + _count_tokens(str(message))
    total += _count_tokens(_message_content(message))
    for tool_call in message.get("tool_calls") or []:
        if isinstance(tool_call, dict):
            function = tool_call.get("function") or {}
            total += _count_tokens(str(function.get("name") or ""))
            total += _count_tokens(_tool_call_arguments_text(function.get("arguments")))
        total += 3
    return total

def _count_messages_tokens(messages):
    return sum(_count_message_tokens(message) for message in messages or [])

def _matched_tool_call_ids(messages):
    matched = set()
    for message in messages or []:
        if isinstance(message, dict) and message.get("role") == "tool":
            tool_id = str(message.get("tool_call_id") or "").strip()
            if tool_id:
                matched.add(tool_id)
    return matched

def _summary_tool_call_id(tool_call):
    if isinstance(tool_call, dict):
        return str(tool_call.get("id") or "").strip()
    return ""

def _truncate_serialized_content(content):
    if len(content) > 3000:
        return content[:2000] + "\n...[truncated]...\n" + content[-800:]
    return content

def _serialize_summary_messages(messages):
    # Mirrors hermes-lcm engine._serialize_messages: labeled per-role text with
    # matched tool-call enrichment and long-content truncation. Redaction and
    # externalization stay in the Rust ingest pipeline.
    parts = []
    matched_tool_ids = _matched_tool_call_ids(messages)
    for message in messages or []:
        if not isinstance(message, dict):
            parts.append(f"[USER]: {message}")
            continue
        role = str(message.get("role") or "unknown")
        content = _message_content(message)
        if role == "tool":
            tool_id = str(message.get("tool_call_id") or "").strip()
            parts.append(f"[TOOL RESULT {tool_id}]: {_truncate_serialized_content(content)}")
            continue
        if role == "assistant":
            tool_calls = message.get("tool_calls") or []
            matched_tool_calls = [
                tool_call
                for tool_call in tool_calls
                if not _summary_tool_call_id(tool_call)
                or _summary_tool_call_id(tool_call) in matched_tool_ids
            ]
            content = _truncate_serialized_content(content)
            if matched_tool_calls:
                tool_call_parts = []
                for tool_call in matched_tool_calls:
                    if isinstance(tool_call, dict):
                        function = tool_call.get("function") or {}
                        name = function.get("name") or "?"
                        arguments = _tool_call_arguments_text(function.get("arguments"))
                        if len(arguments) > 500:
                            arguments = arguments[:400] + "..."
                        tool_call_parts.append(f"  {name}({arguments})")
                content += "\n[Tool calls:\n" + "\n".join(tool_call_parts) + "\n]"
            parts.append(f"[ASSISTANT]: {content}")
            continue
        parts.append(f"[{role.upper()}]: {_truncate_serialized_content(content)}")
    return "\n\n".join(parts)

def _normalized_focus_topic(focus_topic, max_chars=160):
    normalized = " ".join(str(focus_topic or "").split())
    if len(normalized) <= max_chars:
        return normalized
    return normalized[: max(0, max_chars - 1)].rstrip() + "…"

def _build_l1_focus_brief(focus_topic):
    topic = _normalized_focus_topic(focus_topic)
    if not topic:
        return ""
    return (
        "Focus brief:\n"
        f"Primary focus: {topic}\n"
        "Preserve concrete decisions, constraints, files, commands, identifiers, and current state for this focus.\n"
        "Spend roughly 60-70% of the summary budget on the focus when relevant.\n"
        "Do not discard unrelated blockers or active tasks just because they are off-focus.\n"
    )

def _build_l2_focus_brief(focus_topic):
    topic = _normalized_focus_topic(focus_topic)
    if not topic:
        return ""
    return (
        "Focus brief:\n"
        f"Primary focus: {topic}\n"
        "Prefer bullets that preserve decisions, blockers, files, commands, identifiers, and current state for this focus.\n"
        "Keep other active tasks only when they are current blockers or handoff state.\n"
    )

_L1_DEPTH_GUIDANCE = {
    0: "Preserve decisions, rationale, constraints, active tasks, file paths, commands, and specific values.",
    1: "Distill into arc-level outcomes: what evolved, what was decided, current state. Drop per-turn detail.",
    2: "Capture durable narrative: decisions in effect, completed milestones, timeline. Drop process detail.",
}

def _build_l1_prompt(text, token_budget, depth, focus_topic="", custom_instructions=""):
    guidance = _L1_DEPTH_GUIDANCE.get(depth, _L1_DEPTH_GUIDANCE[2])
    focus_guidance = _build_l1_focus_brief(focus_topic)
    custom_block = ""
    if custom_instructions:
        custom_block = f"\nAdditional instructions:\n{custom_instructions}\n"
    return f"""Summarize this conversation segment for future turns.
{guidance}
Remove repetition and conversational filler.
End with: "Expand for details about: <what was compressed>"
{focus_guidance}{custom_block}

Target ~{token_budget} tokens.

CONTENT:
{text}"""

def _build_l2_prompt(text, token_budget, focus_topic="", custom_instructions=""):
    focus_guidance = _build_l2_focus_brief(focus_topic)
    custom_block = ""
    if custom_instructions:
        custom_block = f"\nAdditional instructions:\n{custom_instructions}\n"
    return f"""Compress this into bullet points. Maximum {token_budget} tokens.
Keep only: decisions made, files changed, errors hit, current state.
Drop all reasoning, alternatives considered, and process detail.
{focus_guidance}{custom_block}

CONTENT:
{text}"""

# Conservative allowlist mirroring hermes-lcm model_routing._PROVIDER_PREFIXES:
# many registry provider IDs double as OpenRouter model namespaces, so only
# explicit entries (plus non-canonical named custom providers) split into
# provider/model routes.
_LCM_PROVIDER_ROUTE_PREFIXES = frozenset({"cerebras"})

def _provider_route_is_resolvable(provider):
    provider = str(provider or "").strip().lower()
    if not provider:
        return False
    if provider.startswith("custom:"):
        provider = provider.split(":", 1)[1].strip()
        if not provider:
            return False
    try:
        from hermes_cli.auth import PROVIDER_REGISTRY
        if provider in PROVIDER_REGISTRY:
            return provider in _LCM_PROVIDER_ROUTE_PREFIXES
    except Exception:
        pass
    try:
        from hermes_cli.runtime_provider import _get_named_custom_provider
        if _get_named_custom_provider(provider):
            return True
    except Exception:
        pass
    return False

def _parse_lcm_model_override(value):
    model = str(value or "").strip()
    if not model:
        return None, ""
    provider, separator, rest = model.partition("/")
    provider = provider.strip().lower()
    rest = rest.strip()
    route_provider = provider
    if provider.startswith("custom:"):
        route_provider = provider.split(":", 1)[1].strip()
    if separator and rest and route_provider and _provider_route_is_resolvable(route_provider):
        return route_provider, rest
    return None, model

def _apply_lcm_model_route(call_kwargs, model):
    # Mirrors hermes-lcm model_routing.apply_lcm_model_route.
    provider, routed_model = _parse_lcm_model_override(model)
    if provider:
        call_kwargs["provider"] = provider
    if routed_model:
        call_kwargs["model"] = routed_model

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

def _normalize_extraction_items(text):
    cleaned = str(text or "").strip()
    if not cleaned:
        return []
    items = []
    for line in cleaned.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith(("- ", "* ")):
            stripped = stripped[2:].strip()
        items.append(stripped)
    if not items:
        items = [cleaned]
    return items

def _extraction_route_payload(route, extraction_result):
    if extraction_result is None:
        return route
    # Route-envelope contract with Rust extraction.rs:
    # keys are `route` and `pre_compaction_extraction`.
    payload = {"pre_compaction_extraction": extraction_result}
    if route:
        payload["route"] = route
    return json.dumps(payload, ensure_ascii=False)

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
        return translated
    return translated

class TokenSaveContextEngine(ContextEngine):
    def __init__(self, config=None, hermes_home=None):
        self.active_session_id = None
        self._host_config = config
        self.hermes_home = _resolve_hermes_home(config, hermes_home)
        self.config = _with_plugin_block(config, self.hermes_home)
        self.project_root = _configured_project_root(self.config)
        self.agent = None
        self.model = ""
        self.last_prompt_tokens = 0
        self.last_completion_tokens = 0
        self.last_total_tokens = 0
        self._runtime_context_length = None
        self._session_start_context_length = None
        self._route_failures = {}
        self._cooldown_until = {}
        self._last_preflight_signature = None

    @property
    def name(self) -> str:
        return "tokensave"

    def _bind_session(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        if session_id is not None:
            if session_id != self.active_session_id:
                self._last_preflight_signature = None
            self.active_session_id = session_id
        if kwargs.get("config") is not None:
            self._host_config = kwargs.get("config")
        if "context_length" in kwargs:
            try:
                self._session_start_context_length = int(kwargs.get("context_length"))
            except Exception:
                pass
        next_agent = kwargs.get("agent")
        if next_agent is not None:
            self.agent = next_agent
        explicit_hermes_home = hermes_home or kwargs.get("hermes_home")
        if explicit_hermes_home or kwargs.get("config") is not None or self.hermes_home is None:
            next_hermes_home = _resolve_hermes_home(
                kwargs.get("config", self._host_config),
                explicit_hermes_home,
            )
            if next_hermes_home:
                self.hermes_home = next_hermes_home
        # Re-layer the profile's plugins.tokensave block now that the host
        # config and hermes_home are settled for this session.
        self.config = _with_plugin_block(self._host_config, self.hermes_home)
        next_project_root = (
            project_root
            or kwargs.get("project_root")
            or _configured_project_root(self.config)
            or kwargs.get("cwd")
        )
        if next_project_root:
            self.project_root = next_project_root

    def initialize(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        self._bind_session(session_id, hermes_home, project_root, **kwargs)

    def on_session_start(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        bound_session_id = self.active_session_id
        self._bind_session(session_id, hermes_home, project_root, **kwargs)
        self._report_compression_boundary(session_id, bound_session_id, kwargs)

    def update_model(self, model, context_length, base_url="", api_key="", provider="", api_mode=""):
        self.model = str(model or "")
        try:
            self._runtime_context_length = int(context_length)
        except Exception:
            self._runtime_context_length = None

    def update_from_response(self, usage):
        # Required by newer Hermes ContextEngine ABCs (abstract method).
        # Tracks normalized token usage; run_agent.py reads these attrs.
        usage = usage or {}

        def _as_int(value):
            try:
                return int(value)
            except (TypeError, ValueError):
                return 0

        self.last_prompt_tokens = _as_int(
            usage.get("prompt_tokens") or usage.get("input_tokens")
        )
        self.last_completion_tokens = _as_int(
            usage.get("completion_tokens") or usage.get("output_tokens")
        )
        self.last_total_tokens = _as_int(usage.get("total_tokens")) or (
            self.last_prompt_tokens + self.last_completion_tokens
        )

    def _effective_context_length(self):
        if self._runtime_context_length is not None:
            return self._runtime_context_length
        if self._session_start_context_length is not None:
            return self._session_start_context_length
        return None

    def _tool_args(self, session_id=None):
        args = _storage_args(self.project_root, self.hermes_home)
        args["session_id"] = session_id if session_id is not None else self.active_session_id
        return args

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
        args = self._tool_args()
        args.update(
            _lcm_config_args(
                self.config,
                self.hermes_home,
                runtime_context_length=self._effective_context_length(),
            )
        )
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
            "incremental_max_depth",
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

    def should_compress(self, prompt_tokens=None, **kwargs):
        response = self.should_compress_preflight([], current_tokens=prompt_tokens, **kwargs)
        if isinstance(response, dict):
            return bool(response.get("should_compress"))
        return False

    def status(self, session_id=None, **kwargs):
        args = self._tool_args(session_id)
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
        signature = f"{self.active_session_id}:{_messages_hash(messages)}"
        if signature == self._last_preflight_signature:
            return
        args = _storage_args(self.project_root, self.hermes_home)
        args.update(
            _lcm_config_args(
                self.config,
                self.hermes_home,
                runtime_context_length=self._effective_context_length(),
            )
        )
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
            "incremental_max_depth",
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
            self._last_preflight_signature = signature
        except Exception as exc:
            logger.warning("LCM current-turn preflight failed: %s", exc)

    def handle_tool_call(self, name, arguments=None, **kwargs) -> str:
        tool_name, tool_args = _normalize_memory_tool_call(name, arguments)
        native_name = tool_name
        tokensave_name = LCM_TOOL_ALIASES.get(native_name)
        if tokensave_name is None and native_name in LCM_DIRECT_TOOL_NAMES:
            tokensave_name = native_name
            native_name = LCM_DIRECT_TO_NATIVE.get(native_name, native_name)
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
        kwargs = dict(kwargs)
        args = self._tool_args(kwargs.pop("session_id", None))
        args["prompt"] = prompt
        if query is not None:
            args["query"] = query
        if node_ids is not None:
            args["node_ids"] = node_ids
        for key in ("max_results", "max_tokens", "context_max_tokens"):
            if key in kwargs and kwargs[key] is not None:
                args[key] = kwargs[key]
        if "context_max_tokens" not in args:
            args["context_max_tokens"] = _lcm_expansion_context_tokens(self.config)
        retrieval = call_tokensave_json("tokensave_lcm_expand_query", args, **kwargs)
        synthesis_kwargs = dict(kwargs)
        if synthesis_kwargs.get("model") is None:
            expansion_model = _lcm_expansion_model(self.config)
            if expansion_model:
                synthesis_kwargs["model"] = expansion_model
        if (
            synthesis_kwargs.get("timeout") is None
            and synthesis_kwargs.get("expansion_timeout") is None
        ):
            synthesis_kwargs["expansion_timeout"] = _lcm_expansion_timeout_ms(self.config) / 1000
        return _synthesize_expand_query_payload(retrieval, agent=self.agent, **synthesis_kwargs)

    def _auxiliary_routes(self, summary_request=None, **kwargs):
        routes = (
            kwargs.get("routes")
            or kwargs.get("auxiliary_routes")
            or (summary_request or {}).get("routes")
        )
        if isinstance(routes, dict):
            routes = [routes]
        defaults = {}
        for key in ("model", "temperature", "max_tokens", "timeout"):
            if kwargs.get(key) is not None:
                defaults[key] = kwargs[key]
        if "timeout" not in defaults:
            timeout_ms = _lcm_summary_timeout_ms(self.config, hermes_home=self.hermes_home)
            if timeout_ms:
                defaults["timeout"] = timeout_ms / 1000
        if not routes:
            if defaults.get("model") is not None:
                routes = [{}]
            else:
                # Mirror hermes-lcm escalation._summary_model_chain: the
                # configured summary_model plus summary_fallback_models form
                # the default route chain, falling back to one task-default
                # route when nothing is configured.
                primary = str(
                    _lcm_str_setting(self.config, "LCM_SUMMARY_MODEL", "summary_model", default="") or ""
                )
                fallbacks = _lcm_list_setting(
                    self.config,
                    "LCM_SUMMARY_FALLBACK_MODELS",
                    "summary_fallback_models",
                    default=[],
                )
                chain = []
                for model in [primary, *(fallbacks or [])]:
                    normalized_model = str(model or "").strip()
                    if normalized_model not in chain:
                        chain.append(normalized_model)
                if not chain:
                    chain.append("")
                routes = [{"model": model} if model else {} for model in chain]
        normalized = []
        for route in routes:
            if not isinstance(route, dict):
                route = {"model": str(route)}
            normalized.append({**defaults, **route})
        return normalized

    def _call_auxiliary_summary(self, prompt, messages, **kwargs):
        client = getattr(getattr(self, "agent", None), "auxiliary_client", None)
        summary_request = kwargs.get("summary_request")
        allow_retry_signal = bool(kwargs.pop("allow_retry_signal", False))
        accepts_result = kwargs.pop("accepts_result", None)
        route_kwargs = dict(kwargs)
        route_kwargs.pop("summary_request", None)
        routes = self._auxiliary_routes(summary_request, **route_kwargs)
        breaker_threshold, breaker_cooldown_seconds = _summary_circuit_breaker_settings(self.config)
        last_error = None
        last_classification = None
        rejected_text = None
        rejected_route = None
        rejected_model = None
        if client is None or not callable(getattr(client, "call_llm", None)):
            last_error = RuntimeError("Hermes auxiliary_client.call_llm is unavailable")
        else:
            now = time.time()
            for route in routes:
                route_name = route.get("route") or route.get("name") or route.get("model") or "default"
                route_key = str(route_name)
                if self._cooldown_until.get(route_key, 0) > now:
                    continue
                call_kwargs = {
                    "task": "compression",
                    # Hermes escalation sends the full prompt (guidance plus
                    # serialized CONTENT block) as a single user message.
                    "messages": [{"role": "user", "content": prompt}],
                    "temperature": route.get("temperature", 0.3),
                    "max_tokens": route.get("max_tokens", 2048),
                    "timeout": route.get("timeout", 60),
                }
                _apply_lcm_model_route(call_kwargs, route.get("model"))
                try:
                    response = client.call_llm(**call_kwargs)
                    text = _strip_reasoning(_llm_response_text(response))
                    if not text:
                        raise RuntimeError("Hermes auxiliary summary was empty")
                    if accepts_result is not None and not accepts_result(text):
                        rejected_text = text
                        rejected_route = route_key
                        rejected_model = route.get("model")
                        failures = self._route_failures.get(route_key, 0) + 1
                        self._route_failures[route_key] = failures
                        if failures >= breaker_threshold:
                            self._cooldown_until[route_key] = (
                                time.time() + breaker_cooldown_seconds
                            )
                        continue
                    self._route_failures.pop(route_key, None)
                    self._cooldown_until.pop(route_key, None)
                    return {
                        "status": "ok",
                        "text": text,
                        "route": route_key,
                        "model": route.get("model"),
                    }
                except Exception as exc:
                    last_error = exc
                    last_classification = _auxiliary_error_classification(exc)
                    failures = self._route_failures.get(route_key, 0) + 1
                    self._route_failures[route_key] = failures
                    if failures >= breaker_threshold:
                        self._cooldown_until[route_key] = (
                            time.time() + breaker_cooldown_seconds
                        )
        if rejected_text is not None:
            return {
                "status": "rejected",
                "text": rejected_text,
                "route": rejected_route,
                "model": rejected_model,
                "error": "Hermes auxiliary summary was not smaller than source",
                "error_classification": "non_compressing_summary",
            }
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

    def _summarize_with_escalation(self, source_messages, focus_topic="", **kwargs):
        # Port of hermes-lcm escalation.summarize_with_escalation: L1 detailed
        # summary, L2 aggressive bullets at reduced budget, then deterministic
        # L3 truncation. Each LLM rung accepts a result only when its token
        # estimate is below the source token estimate.
        serialized = _serialize_summary_messages(source_messages)
        source_tokens = _count_messages_tokens(source_messages)
        # Mirrors the leaf budget in hermes-lcm engine._summarize_leaf_chunk_with_rescue.
        token_budget = min(12000, max(2000, int(source_tokens * 0.20)))
        custom_instructions = str(
            _lcm_str_setting(self.config, "LCM_CUSTOM_INSTRUCTIONS", "custom_instructions", default="") or ""
        )
        l2_budget_ratio = _lcm_float_setting(
            self.config,
            "LCM_L2_BUDGET_RATIO",
            "l2_budget_ratio",
            default=0.50,
        )
        if l2_budget_ratio is None:
            l2_budget_ratio = 0.50
        l3_truncate_tokens = (
            _lcm_int_setting(self.config, "LCM_L3_TRUNCATE_TOKENS", "l3_truncate_tokens", default=512) or 512
        )

        def accepts_result(text):
            return source_tokens <= 0 or _count_tokens(text) < source_tokens

        l1_kwargs = dict(kwargs)
        l1_kwargs["accepts_result"] = accepts_result
        l1_kwargs["max_tokens"] = token_budget * 2
        l1_prompt = _build_l1_prompt(
            serialized,
            token_budget,
            0,
            focus_topic=focus_topic,
            custom_instructions=custom_instructions,
        )
        rung_failures = []

        def record_rung_failure(level, summary):
            rung_failures.append({
                "level": level,
                "status": summary.get("status"),
                "route": summary.get("route"),
                "model": summary.get("model"),
                "error": summary.get("error"),
                "error_classification": summary.get("error_classification"),
            })

        summary = self._call_auxiliary_summary(l1_prompt, source_messages, **l1_kwargs)
        if summary.get("status") == "ok":
            return summary
        record_rung_failure(1, summary)

        l2_budget = max(1, int(token_budget * l2_budget_ratio))
        l2_kwargs = dict(kwargs)
        l2_kwargs["accepts_result"] = accepts_result
        l2_kwargs["max_tokens"] = l2_budget * 2
        l2_prompt = _build_l2_prompt(
            serialized,
            l2_budget,
            focus_topic=focus_topic,
            custom_instructions=custom_instructions,
        )
        summary = self._call_auxiliary_summary(l2_prompt, source_messages, **l2_kwargs)
        if summary.get("status") == "ok":
            if rung_failures:
                summary["rung_failures"] = rung_failures
            return summary
        record_rung_failure(2, summary)
        if summary.get("status") == "fallback":
            summary.setdefault("error", "Hermes auxiliary summary was not smaller than source")
            summary.setdefault("error_classification", "non_compressing_summary")
        fallback = {
            "status": "fallback",
            "text": _deterministic_truncation(source_messages, limit=max(1, l3_truncate_tokens * 4)),
            "route": "deterministic_fallback",
            "model": None,
            "error": summary.get("error") or "Hermes auxiliary summary was not smaller than source",
            "error_classification": summary.get("error_classification") or "non_compressing_summary",
            "rung_failures": rung_failures,
        }
        if summary.get("error"):
            fallback["auxiliary_error"] = summary.get("error")
        if summary.get("error_classification"):
            fallback["auxiliary_error_classification"] = summary.get("error_classification")
        return fallback

    def _run_pre_compaction_extraction(self, summary_request, source_messages):
        if not _lcm_extraction_enabled(self.config):
            return None
        if not source_messages:
            return {"status": "no_source"}
        extraction_request = (
            summary_request.get("extraction_request") if isinstance(summary_request, dict) else None
        )
        prompt = extraction_request.get("prompt") if isinstance(extraction_request, dict) else None
        if not isinstance(prompt, str) or not prompt.strip():
            return {
                "status": "failed_non_blocking",
                "error": "LCM extraction envelope missing prompt",
                "model": None,
                "output_path": None,
            }
        extraction_model = str(
            _lcm_extraction_model(self.config)
            or _lcm_str_setting(self.config, "LCM_SUMMARY_MODEL", "summary_model", default="")
            or ""
        ).strip()
        timeout_seconds = _lcm_summary_timeout_ms(self.config, hermes_home=self.hermes_home) / 1000
        # Intentional divergence from upstream hermes-lcm: Rust stores extraction results in
        # summary-node metadata instead of writing daily markdown files. We still surface
        # output_path in the extraction contract for config/API parity.
        output_path = _lcm_extraction_output_path(self.config)
        client = getattr(getattr(self, "agent", None), "auxiliary_client", None)
        if client is None or not callable(getattr(client, "call_llm", None)):
            return {
                "status": "failed_non_blocking",
                "error": "Hermes auxiliary_client.call_llm is unavailable",
                "model": extraction_model or None,
                "output_path": output_path or None,
            }
        call_kwargs = {
            "task": "extraction",
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.2,
            "max_tokens": 2000,
        }
        _apply_lcm_model_route(call_kwargs, extraction_model)
        if timeout_seconds is not None:
            call_kwargs["timeout"] = timeout_seconds
        try:
            response = client.call_llm(**call_kwargs)
        except Exception as exc:
            return {
                "status": "failed_non_blocking",
                "error": str(exc),
                "model": extraction_model or None,
                "output_path": output_path or None,
            }
        cleaned = _strip_reasoning(_llm_response_text(response)).strip()
        if not cleaned or cleaned == "NOTHING_TO_EXTRACT":
            return {
                "status": "nothing_to_extract",
                "model": extraction_model or None,
                "output_path": output_path or None,
            }
        return {
            "status": "ok",
            "items": _normalize_extraction_items(cleaned),
            "text": cleaned,
            "model": extraction_model or None,
            "output_path": output_path or None,
        }

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
            "incremental_max_depth",
            "fresh_tail_count",
            "dynamic_leaf_chunk_enabled",
            "dynamic_leaf_chunk_max",
            "context_length",
            "reserve_tokens_floor",
            "ignore_session_patterns",
            "stateless_session_patterns",
            "ignore_message_patterns",
        )
        args = self._tool_args()
        args.update(
            _lcm_config_args(
                self.config,
                self.hermes_home,
                runtime_context_length=self._effective_context_length(),
            )
        )
        args.update({
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
            extraction_result = self._run_pre_compaction_extraction(
                summary_request,
                source_messages,
            )
            summary = self._summarize_with_escalation(
                source_messages,
                focus_topic=summary_request.get("focus_topic") or focus_topic or "",
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

            if summary_status == "fallback":
                fallback_used = True
                retry_status = retry_status or "fallback_summary"
                error_classification = summary.get("error_classification") or error_classification

            provided_args = dict(attempt_args)
            provided_route = _extraction_route_payload(summary.get("route"), extraction_result)
            provided_args["summarizer"] = {
                "mode": "provided",
                "summary_text": summary["text"],
                "route": provided_route,
            }
            result = call_tokensave_json("tokensave_lcm_compress", provided_args, **kwargs)
            return _with_auxiliary_metadata(
                result,
                attempts=attempts,
                retry_status=retry_status,
                error_classification=error_classification,
                fallback_used=fallback_used,
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

    def get_config_schema(self):
        # Consumed by `hermes memory setup` / `hermes memory status`.
        return [
            {
                "key": "project_root",
                "description": "Absolute path of the project tokensave serves (install-time pin)",
                "default": tools.config_pinned_project_root() or "",
            },
        ]

    def get_config_defaults(self):
        # Hermes layers these under DEFAULT_CONFIG so plugins.tokensave
        # exists in loaded configs without core changes.
        return {"plugins": {"tokensave": _plugin_config_defaults()}}

    def get_config_field_meta(self):
        # Dashboard config-page hints for provider-owned dot-paths.
        return {
            "plugins.tokensave.project_root": {
                "category": "plugins",
                "description": (
                    "Project root pinned for tokensave tool calls "
                    "(set by `tokensave install --agent hermes --project-root`)."
                ),
            },
        }

    def save_config(self, values, hermes_home):
        # `hermes memory setup` hands collected non-secret values here; the
        # conventional home is the plugins.tokensave block (same pattern as
        # the bundled holographic provider).
        config_path = Path(hermes_home) / "config.yaml"
        try:
            import yaml
            existing = {}
            if config_path.exists():
                with open(config_path, encoding="utf-8-sig") as config_file:
                    existing = yaml.safe_load(config_file) or {}
            plugins_cfg = existing.get("plugins")
            if not isinstance(plugins_cfg, dict):
                plugins_cfg = {}
            block = plugins_cfg.get("tokensave")
            if not isinstance(block, dict):
                block = {}
            block.update(values or {})
            plugins_cfg["tokensave"] = block
            existing["plugins"] = plugins_cfg
            with open(config_path, "w", encoding="utf-8") as config_file:
                yaml.dump(existing, config_file, default_flow_style=False)
        except Exception as exc:
            logger.warning("tokensave memory provider save_config failed: %s", exc)

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
    # Declare the plugins.tokensave config block so its keys exist in
    # load_config() even before the user edits config.yaml.
    register_config_defaults = getattr(ctx, "register_config_defaults", None)
    if callable(register_config_defaults):
        try:
            register_config_defaults({"plugins": {"tokensave": _plugin_config_defaults()}})
        except Exception as exc:
            logger.warning("tokensave config defaults registration failed: %s", exc)
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
        # Newer Hermes derives the namespace from the plugin name and
        # rejects ':' in skill names, so register the bare name.
        try:
            register_skill("tokensave", skill_path)
        except Exception as exc:
            logger.warning("tokensave skill registration failed: %s", exc)
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
