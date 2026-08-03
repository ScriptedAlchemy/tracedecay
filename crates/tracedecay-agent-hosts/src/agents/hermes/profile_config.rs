//! Hermes profile config manipulation helpers.
//!
//! This module owns the read/patch/write path for Hermes profile `config.yaml`
//! files. The parent integration module is responsible for plugin artifacts;
//! config changes stay behind these focused helpers so install/update/uninstall
//! flows have explicit inputs and preserve the historical error messages.

use std::path::Path;

/// Reads the removed `plugins.tracedecay.project_root` setting solely as
/// provenance for one-time data migration and transcript import.
///
/// This is the single source of truth for the pin (the same
/// `plugins.<name>` block bundled Hermes plugins use): install writes it,
/// reinstalls preserve it, and the generated Python resolves it at runtime.
pub fn read_config_pinned_project_root(config_path: &Path) -> Option<String> {
    let config = std::fs::read_to_string(config_path).ok()?;
    let lines: Vec<&str> = config.lines().collect();
    let (plugins_start, plugins_end) = find_top_level_section_in(&lines, "plugins")?;
    read_pinned_project_root_from_block(&lines, plugins_start, plugins_end, "tracedecay")
}

fn read_pinned_project_root_from_block(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    plugin_key: &str,
) -> Option<String> {
    let PluginBlock::Block { start, end } =
        find_plugin_block_in(lines, plugins_start, plugins_end, plugin_key)?
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

/// Adds TraceDecay to the Hermes plugin, memory-provider, and context-engine
/// configuration while preserving unrelated profile settings.
pub fn enable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    let without_legacy_pin = remove_pinned_project_root_config(existing)?;
    let enabled = enable_plugin_list_config(&without_legacy_pin)?;
    let with_memory = enable_memory_provider_config(&enabled)?;
    enable_context_engine_config(&with_memory)
}

fn enable_plugin_list_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("plugins:\n  enabled:\n    - tracedecay\n".to_string());
    }

    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    validate_top_level_plugins_shape(existing)?;

    if find_top_level_section(existing, "plugins").is_none() {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("plugins:\n  enabled:\n    - tracedecay\n");
        return Ok(out);
    }

    let (plugins_start, plugins_end) = find_top_level_section(existing, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    match find_child_section_from_strings(&lines, plugins_start, plugins_end, "disabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        ChildSection::Block { start, end } => {
            lines = remove_list_item(lines, start, end, "tracedecay");
        }
        ChildSection::Missing | ChildSection::EmptyFlow { .. } => {}
    }

    let (plugins_start, plugins_end) = find_top_level_section_from_strings(&lines, "plugins")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?;
    match find_child_section_from_strings(&lines, plugins_start, plugins_end, "enabled")
        .ok_or_else(|| "unsupported Hermes plugins config".to_string())?
    {
        ChildSection::Block { start, end } => {
            if !list_contains_item_strings(&lines, start, end, "tracedecay") {
                // Match the existing list's item indentation (Hermes writes
                // 2-space items); only default to 4 when the list is empty.
                let indent = list_item_indent(&lines, start, end).unwrap_or(4);
                lines.insert(start + 1, format!("{}- tracedecay", " ".repeat(indent)));
            }
        }
        ChildSection::EmptyFlow { line } => {
            // Rewrite `enabled: []` into a block list containing tracedecay.
            lines[line] = "  enabled:".to_string();
            lines.insert(line + 1, "    - tracedecay".to_string());
        }
        ChildSection::Missing => {
            lines.insert(plugins_start + 1, "  enabled:".to_string());
            lines.insert(plugins_start + 2, "    - tracedecay".to_string());
        }
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

/// Removes TraceDecay-owned Hermes plugin, memory-provider, and context-engine
/// settings while preserving unrelated profile settings.
pub fn disable_plugin_config(existing: &str) -> std::result::Result<String, String> {
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
            lines = remove_list_item(lines, start, end, "tracedecay");
        }
        ChildSection::Missing | ChildSection::EmptyFlow { .. } => {}
    }
    let without_pin = remove_pinned_project_root_config(&join_lines(&lines, had_trailing_newline))?;
    let without_engine = disable_context_engine_config(&without_pin)?;
    disable_memory_provider_config(&without_engine)
}

fn enable_memory_provider_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("memory:\n  provider: tracedecay\n".to_string());
    }

    validate_top_level_memory_shape(existing)?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    let Some((memory_start, memory_end)) = find_top_level_section(existing, "memory") else {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("memory:\n  provider: tracedecay\n");
        return Ok(out);
    };

    let provider_line = find_memory_provider_line(&lines, memory_start, memory_end)
        .ok_or_else(|| "unsupported Hermes memory config".to_string())?;
    if let Some(provider_line) = provider_line {
        let provider = memory_provider_value(&lines[provider_line])
            .ok_or_else(|| "unsupported Hermes memory config".to_string())?;
        if provider != "tracedecay" {
            return Err(
                "Hermes memory provider already configured; refusing to overwrite it".to_string(),
            );
        }
    } else {
        lines.insert(memory_start + 1, "  provider: tracedecay".to_string());
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

fn memory_provider_value(line: &str) -> Option<&str> {
    let value = line.trim().strip_prefix("provider:")?.trim();
    Some(value.trim_matches(['"', '\'']))
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
        let provider = lines[provider_line].trim();
        if provider == "provider: tracedecay" {
            lines.remove(provider_line);
            removed_provider = true;
        }
    }
    if removed_provider {
        remove_empty_top_level_section(&mut lines, "memory");
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

/// Sets `context.engine: tracedecay` so Hermes activates the registered
/// context engine (selection is config-driven; the host never auto-activates
/// plugin engines). The built-in default `compressor` is replaced; any other
/// configured engine is left alone with an error, mirroring the
/// memory-provider guard.
fn enable_context_engine_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok("context:\n  engine: tracedecay\n".to_string());
    }

    validate_top_level_section_shape(existing, "context")?;
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');

    let Some((context_start, context_end)) = find_top_level_section(existing, "context") else {
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("context:\n  engine: tracedecay\n");
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
                lines[engine_line] = "  engine: tracedecay".to_string();
            }
            Some("tracedecay") => {}
            Some(_) => {
                return Err(
                    "Hermes context engine already configured; refusing to overwrite it"
                        .to_string(),
                );
            }
        }
    } else {
        lines.insert(context_start + 1, "  engine: tracedecay".to_string());
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
        let engine = lines[engine_line].trim();
        if engine == "engine: tracedecay" {
            lines.remove(engine_line);
            removed_engine = true;
        }
    }
    if removed_engine {
        remove_empty_top_level_section(&mut lines, "context");
    }

    Ok(join_lines(&lines, had_trailing_newline))
}

/// Shape of the `plugins.tracedecay` child mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginBlock {
    Missing,
    /// Block-style `<plugin_name>:` at `start`; entries end (exclusive) at `end`.
    Block {
        start: usize,
        end: usize,
    },
    /// Flow-style empty mapping `<plugin_name>: {}` on `line`.
    EmptyFlow {
        line: usize,
    },
}

fn find_plugin_block_in(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    plugin_name: &str,
) -> Option<PluginBlock> {
    let block_header = format!("{plugin_name}:");
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
            if trimmed == block_header {
                start = Some(idx);
                break;
            }
            if let Some(rest) = trimmed.strip_prefix(&block_header) {
                if rest.trim() == "{}" {
                    return Some(PluginBlock::EmptyFlow { line: idx });
                }
                return None;
            }
        }
    }
    let Some(start) = start else {
        return Some(PluginBlock::Missing);
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
    Some(PluginBlock::Block { start, end })
}

/// Removes `plugins.tracedecay.project_root`, then the `tracedecay:` block when
/// nothing else (user-added keys) remains in it.
fn remove_pinned_project_root_config(existing: &str) -> std::result::Result<String, String> {
    remove_pinned_project_root_from_block(existing, "tracedecay")
}

fn remove_pinned_project_root_from_block(
    existing: &str,
    plugin_key: &str,
) -> std::result::Result<String, String> {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let had_trailing_newline = existing.ends_with('\n');
    let Some((plugins_start, plugins_end)) = find_top_level_section(existing, "plugins") else {
        return Ok(existing.to_string());
    };
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let PluginBlock::Block { start, end } =
        find_plugin_block_in(&borrowed, plugins_start, plugins_end, plugin_key)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn enable_plugin_creates_missing_profile_config() {
        let updated = enable_plugin_config("").unwrap();
        assert!(updated.contains("plugins:\n  enabled:\n    - tracedecay\n"));
        assert!(updated.contains("memory:\n  provider: tracedecay\n"));
        assert!(updated.contains("context:\n  engine: tracedecay\n"));
    }

    #[test]
    fn disable_plugin_ignores_missing_config() {
        assert_eq!(disable_plugin_config(""), Ok(String::new()));
    }

    #[test]
    fn enable_plugin_updates_existing_config_without_project_pin() {
        let updated =
            enable_plugin_config("theme: dark\nplugins:\n  enabled:\n    - other\n").unwrap();
        assert!(updated.contains("theme: dark\n"));
        assert!(updated.contains("    - tracedecay\n    - other\n"));
        assert!(updated.contains("memory:\n  provider: tracedecay\n"));
        assert!(updated.contains("context:\n  engine: tracedecay\n"));
    }

    #[test]
    fn enable_plugin_removes_legacy_project_pin_but_preserves_behavior_settings() {
        let updated = enable_plugin_config(
            "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: /legacy/repo\n    summary_model: glm-4.7\n",
        )
        .unwrap();
        assert!(!updated.contains("project_root:"), "{updated}");
        assert!(updated.contains("summary_model: glm-4.7"), "{updated}");
    }

    #[test]
    fn enable_plugin_rejects_malformed_config_without_rewrite() {
        let original = "plugins: {enabled: [other]}\n";
        let err = enable_plugin_config(original).unwrap_err();

        assert!(err.contains("unsupported Hermes plugins config"));
    }

    #[test]
    fn enable_plugin_is_idempotent_on_rerun() {
        let first = enable_plugin_config(
            "plugins:\n  enabled:\n  - other\nmemory:\n  provider: tracedecay\ncontext:\n  engine: tracedecay\n",
        )
        .unwrap();
        let second = enable_plugin_config(&first).unwrap();

        assert_eq!(second, first);
        assert_eq!(second.matches("- tracedecay").count(), 1);
    }

    #[test]
    fn enable_plugin_still_rejects_unrelated_memory_provider() {
        let original = "memory:\n  provider: other\n";
        let err = enable_plugin_config(original).unwrap_err();

        assert!(err.contains("Hermes memory provider already configured"));
    }

    #[test]
    fn read_project_pin_decodes_yaml_scalars() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(
            &config,
            "plugins:\n  tracedecay:\n    project_root: '/repo/it''s-ok'\n",
        )
        .unwrap();
        assert_eq!(
            read_config_pinned_project_root(&config).as_deref(),
            Some("/repo/it's-ok")
        );
    }

    #[test]
    fn disable_plugin_removes_only_tracedecay_config() {
        let updated = disable_plugin_config(
            "theme: dark\nplugins:\n  enabled:\n    - tracedecay\n    - other\nmemory:\n  provider: tracedecay\ncontext:\n  engine: tracedecay\n",
        )
        .unwrap();
        assert!(updated.contains("theme: dark"));
        assert!(updated.contains("    - other"));
        assert!(!updated.contains("tracedecay"));
        assert!(!updated.contains("memory:\n"));
        assert!(!updated.contains("context:\n"));
    }
}
