//! Hermes profile config manipulation helpers.
//!
//! This module owns the read/patch/write path for Hermes profile `config.yaml`
//! files. The parent integration module is responsible for plugin artifacts;
//! config changes stay behind these focused helpers so install/update/uninstall
//! flows have explicit inputs and preserve the historical error messages.

use std::io::ErrorKind;
use std::path::Path;
use std::str::FromStr;

use tracedecay_application::{DirectorySyncPolicy, atomic_write};
use yaml_edit::{Document, Mapping, Sequence, YamlNode};

use crate::agents::backup_config_file;
use crate::errors::{Result, TraceDecayError};

const DIRECTORY_SYNC_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    fn detect(contents: &str) -> Self {
        if contents.contains("\r\n") {
            Self::Crlf
        } else {
            Self::Lf
        }
    }

    fn normalize(self, contents: &str) -> String {
        match self {
            Self::Lf => contents.to_string(),
            Self::Crlf => contents.replace("\r\n", "\n"),
        }
    }

    fn restore(self, contents: String) -> String {
        match self {
            Self::Lf => contents,
            Self::Crlf => contents.replace('\n', "\r\n"),
        }
    }
}

struct ProfileConfigDocument {
    root: Mapping,
}

impl ProfileConfigDocument {
    fn parse(contents: &str) -> std::result::Result<Self, String> {
        let normalized = LineEnding::detect(contents).normalize(contents);
        let document = if normalized.trim().is_empty() {
            Document::new_mapping()
        } else {
            Document::from_str(&normalized)
                .map_err(|error| format!("invalid Hermes YAML config: {error}"))?
        };
        let root = document
            .as_mapping()
            .ok_or_else(|| "unsupported Hermes config; expected a top-level mapping".to_string())?;
        Ok(Self { root })
    }

    fn root(&self) -> Mapping {
        self.root.clone()
    }
}

/// Reads the removed `plugins.tracedecay.project_root` setting solely as
/// provenance for one-time data migration and transcript import.
pub(crate) fn read_config_pinned_project_root(config_path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(config_path).ok()?;
    let config = ProfileConfigDocument::parse(&contents).ok()?;
    let plugins = config.root().get_mapping("plugins")?;
    let tracedecay = plugins.get_mapping("tracedecay")?;
    string_value(&tracedecay, "project_root")
}

pub(super) fn registration_state(
    config_path: &Path,
) -> crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 {
    use crate::agents::host_bundle_v2::HostBundleRegistrationStateV1 as State;

    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return State::Missing;
    };
    let Ok(config) = ProfileConfigDocument::parse(&contents) else {
        return State::Corrupt;
    };
    let root = config.root();
    let enabled = root
        .get_mapping("plugins")
        .and_then(|plugins| plugins.get_sequence("enabled"))
        .is_some_and(|plugins| sequence_contains(&plugins, "tracedecay"));
    let memory = root
        .get_mapping("memory")
        .and_then(|memory| string_value(&memory, "provider"))
        .as_deref()
        == Some("tracedecay");
    let context = root
        .get_mapping("context")
        .and_then(|context| string_value(&context, "engine"))
        .as_deref()
        == Some("tracedecay");
    if enabled && memory && context {
        State::Current
    } else {
        State::Repairable
    }
}

pub(super) fn enable_plugin(config_path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = enable_plugin_config(&existing).map_err(|message| TraceDecayError::Config {
        message: format!(
            "{message} in {}.\nFix the config by hand, then re-run: tracedecay install --agent hermes",
            config_path.display()
        ),
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(true)
}

pub(super) fn disable_plugin(config_path: &Path) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(config_path) else {
        return Ok(());
    };
    let updated = disable_plugin_config(&existing).map_err(|message| TraceDecayError::Config {
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

// Error messages preserved from the historical line-oriented implementation so
// install/update surfaces stay stable.
const PLUGINS_ERR: &str = "unsupported Hermes plugins config";
const MEMORY_ERR: &str = "unsupported Hermes memory config";
const CONTEXT_ERR: &str = "unsupported Hermes context config";

// The mutation path is deliberately text/span oriented rather than CST oriented.
// The pinned `yaml-edit` (0.2.3) cannot reliably synthesize nested block or flow
// collections: `Mapping::set`/`Sequence::push` render new members at column 0
// (their indent is derived from context that is lost through the cloned
// `get_mapping`/`get_sequence` handles), and node-copying `set` ignores the
// requested indent. So every mutation is applied as a byte-splice on the source
// text; `yaml-edit` is used only to parse and locate spans. This keeps comments,
// anchors, aliases, flow collections and line endings byte-for-byte intact for
// structure the installer does not own.

fn enable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    let line_ending = LineEnding::detect(existing);
    let normalized = line_ending.normalize(existing);
    let updated = enable_normalized(&normalized)?;
    Ok(line_ending.restore(updated))
}

fn disable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let line_ending = LineEnding::detect(existing);
    let normalized = line_ending.normalize(existing);
    let updated = disable_normalized(&normalized)?;
    Ok(line_ending.restore(updated))
}

fn enable_normalized(existing: &str) -> std::result::Result<String, String> {
    let text = remove_legacy_project_pin(existing)?;
    let text = remove_seq_item(&text, &["plugins", "disabled"], "tracedecay")?;
    let text = ensure_enabled(&text)?;
    let text = ensure_scalar(
        &text,
        "memory",
        "provider",
        "tracedecay",
        &[],
        MEMORY_ERR,
        "Hermes memory provider already configured; refusing to overwrite it",
    )?;
    let text = ensure_scalar(
        &text,
        "context",
        "engine",
        "tracedecay",
        &["compressor"],
        CONTEXT_ERR,
        "Hermes context engine already configured; refusing to overwrite it",
    )?;
    Ok(text)
}

fn disable_normalized(existing: &str) -> std::result::Result<String, String> {
    let text = remove_seq_item(existing, &["plugins", "enabled"], "tracedecay")?;
    let text = remove_legacy_project_pin(&text)?;
    let text = disable_scalar(&text, "context", "engine")?;
    let text = disable_scalar(&text, "memory", "provider")?;
    Ok(text)
}

/// Parse a normalized (LF) profile document, tolerating an empty file as an empty
/// mapping and preserving the historical top-level error message.
fn parse_profile(text: &str) -> std::result::Result<Document, String> {
    if text.trim().is_empty() {
        return Ok(Document::new_mapping());
    }
    let document =
        Document::from_str(text).map_err(|error| format!("invalid Hermes YAML config: {error}"))?;
    if document.as_mapping().is_none() {
        return Err("unsupported Hermes config; expected a top-level mapping".to_string());
    }
    Ok(document)
}

// ---- span/text helpers ----

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end_including_newline(text: &str, pos: usize) -> usize {
    text[pos..]
        .find('\n')
        .map_or(text.len(), |index| pos + index + 1)
}

fn leading_indent(text: &str, pos: usize) -> String {
    let start = line_start(text, pos);
    text[start..].chars().take_while(|ch| *ch == ' ').collect()
}

/// Append a root-level block, separated from prior content by a blank line.
fn append_root_block(text: &str, block: &str) -> String {
    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
    out
}

fn value_end(node: &YamlNode) -> Option<usize> {
    if let Some(scalar) = node.as_scalar() {
        Some(scalar.byte_range().end as usize)
    } else if let Some(mapping) = node.as_mapping() {
        Some(mapping.byte_range().end as usize)
    } else {
        node.as_sequence()
            .map(|sequence| sequence.byte_range().end as usize)
    }
}

// ---- enable mutations ----

fn ensure_enabled(text: &str) -> std::result::Result<String, String> {
    let document = parse_profile(text)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    let Some(plugins_node) = root.get("plugins") else {
        return Ok(append_root_block(
            text,
            "plugins:\n  enabled:\n    - tracedecay",
        ));
    };
    let Some(plugins) = plugins_node.as_mapping() else {
        return Err(PLUGINS_ERR.to_string());
    };
    match plugins.get("enabled") {
        None => {
            if plugins.is_flow_style() {
                Ok(insert_into_flow(
                    text,
                    plugins.byte_range().end as usize,
                    plugins.keys().count() == 0,
                    "enabled: [tracedecay]",
                ))
            } else {
                insert_block_child(text, &root, "plugins", "enabled:\n{CHILD}  - tracedecay")
            }
        }
        Some(enabled_node) => {
            let Some(enabled) = enabled_node.as_sequence() else {
                return Err(PLUGINS_ERR.to_string());
            };
            if sequence_contains(enabled, "tracedecay") {
                return Ok(text.to_string());
            }
            let range = enabled.byte_range();
            if enabled.is_flow_style() {
                Ok(insert_into_flow(
                    text,
                    range.end as usize,
                    enabled.values().count() == 0,
                    "tracedecay",
                ))
            } else {
                let item_indent = leading_indent(text, range.start as usize);
                let end = range.end as usize;
                let insert_at = if text[..end].ends_with('\n') {
                    end
                } else {
                    line_end_including_newline(text, end.saturating_sub(1))
                };
                let insertion = format!("{item_indent}- tracedecay\n");
                Ok(format!(
                    "{}{}{}",
                    &text[..insert_at],
                    insertion,
                    &text[insert_at..]
                ))
            }
        }
    }
}

fn ensure_scalar(
    text: &str,
    container: &str,
    key: &str,
    value: &str,
    allow_overwrite: &[&str],
    unsupported: &str,
    conflict: &str,
) -> std::result::Result<String, String> {
    let document = parse_profile(text)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    let Some(container_node) = root.get(container) else {
        return Ok(append_root_block(
            text,
            &format!("{container}:\n  {key}: {value}"),
        ));
    };
    let Some(mapping) = container_node.as_mapping() else {
        return Err(unsupported.to_string());
    };
    if !mapping.contains_key(key) {
        if mapping.is_flow_style() {
            return Ok(insert_into_flow(
                text,
                mapping.byte_range().end as usize,
                mapping.keys().count() == 0,
                &format!("{key}: {value}"),
            ));
        }
        return insert_block_child(text, &root, container, &format!("{key}: {value}"));
    }
    match string_value(mapping, key).as_deref() {
        Some(current) if current == value => Ok(text.to_string()),
        Some(current) if allow_overwrite.contains(&current) => {
            let entry = mapping
                .find_entry_by_key(key)
                .ok_or_else(|| unsupported.to_string())?;
            let range = entry
                .value_node()
                .and_then(|node| node.as_scalar().map(yaml_edit::Scalar::byte_range))
                .ok_or_else(|| unsupported.to_string())?;
            Ok(format!(
                "{}{}{}",
                &text[..range.start as usize],
                value,
                &text[range.end as usize..]
            ))
        }
        Some(_) => Err(conflict.to_string()),
        None => Err(unsupported.to_string()),
    }
}

/// Insert `content` into a flow collection whose byte range ends at `range_end`
/// (just past the closing `}`/`]`), prefixing `, ` when the collection is not
/// empty.
fn insert_into_flow(text: &str, range_end: usize, empty: bool, content: &str) -> String {
    let at = range_end - 1;
    let insertion = if empty {
        content.to_string()
    } else {
        format!(", {content}")
    };
    format!("{}{}{}", &text[..at], insertion, &text[at..])
}

/// Insert a new child line under an existing BLOCK mapping `container`. `{CHILD}`
/// in the template expands to the child indentation for nested continuations.
fn insert_block_child(
    text: &str,
    root: &Mapping,
    container: &str,
    child_template: &str,
) -> std::result::Result<String, String> {
    let mapping = root
        .get(container)
        .and_then(|node| node.as_mapping().cloned())
        .ok_or_else(|| "unsupported Hermes profile config".to_string())?;
    let entry = root
        .find_entry_by_key(container)
        .ok_or_else(|| "unsupported Hermes profile config".to_string())?;
    let key_start = entry
        .key_node()
        .and_then(|node| {
            node.as_scalar()
                .map(|scalar| scalar.byte_range().start as usize)
        })
        .ok_or_else(|| "unsupported Hermes profile config".to_string())?;
    let child_indent = format!("{}  ", leading_indent(text, key_start));
    let child = child_template.replace("{CHILD}", &child_indent);
    let insert_at = if mapping.keys().count() == 0 {
        line_end_including_newline(text, key_start)
    } else {
        let end = mapping.byte_range().end as usize;
        if text[..end].ends_with('\n') {
            end
        } else {
            line_end_including_newline(text, end.saturating_sub(1))
        }
    };
    Ok(format!(
        "{}{}{}\n{}",
        &text[..insert_at],
        child_indent,
        child,
        &text[insert_at..]
    ))
}

// ---- removals ----

/// Remove the legacy `plugins.tracedecay.project_root` pin, collapsing an
/// otherwise-empty `tracedecay` mapping when it carries no comments/anchors.
fn remove_legacy_project_pin(text: &str) -> std::result::Result<String, String> {
    let document = parse_profile(text)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    let Some(plugins) = root
        .get("plugins")
        .and_then(|node| node.as_mapping().cloned())
    else {
        return Ok(text.to_string());
    };
    let Some(tracedecay) = plugins
        .get("tracedecay")
        .and_then(|node| node.as_mapping().cloned())
    else {
        return Ok(text.to_string());
    };
    if !tracedecay.contains_key("project_root") {
        return Ok(text.to_string());
    }
    let after = remove_map_entry(text, &tracedecay, "project_root")?;

    let document = parse_profile(&after)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    if let Some(plugins) = root
        .get("plugins")
        .and_then(|node| node.as_mapping().cloned())
    {
        return collapse_if_empty(&after, &plugins, "tracedecay");
    }
    Ok(after)
}

fn remove_seq_item(text: &str, path: &[&str], value: &str) -> std::result::Result<String, String> {
    let document = parse_profile(text)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    let mut current = root.clone();
    for (index, key) in path.iter().enumerate() {
        let Some(node) = current.get(*key) else {
            return Ok(text.to_string());
        };
        if index + 1 == path.len() {
            let Some(sequence) = node.as_sequence() else {
                return Ok(text.to_string());
            };
            if !sequence_contains(sequence, value) {
                return Ok(text.to_string());
            }
            return remove_one_seq_item(text, sequence, value);
        }
        let Some(mapping) = node.as_mapping() else {
            return Ok(text.to_string());
        };
        current = mapping.clone();
    }
    Ok(text.to_string())
}

fn remove_one_seq_item(
    text: &str,
    sequence: &Sequence,
    value: &str,
) -> std::result::Result<String, String> {
    let Some(item) = sequence.values().find(|node| {
        node.as_scalar()
            .is_some_and(|scalar| scalar.as_string() == value)
    }) else {
        return Ok(text.to_string());
    };
    let range = item
        .as_scalar()
        .map(yaml_edit::Scalar::byte_range)
        .ok_or_else(|| PLUGINS_ERR.to_string())?;
    let (start, end) = (range.start as usize, range.end as usize);
    if sequence.is_flow_style() {
        let (start, end) = expand_over_flow_separator(text, start, end);
        Ok(format!("{}{}", &text[..start], &text[end..]))
    } else {
        let line_start = line_start(text, start);
        let line_end = line_end_including_newline(text, end);
        Ok(format!("{}{}", &text[..line_start], &text[line_end..]))
    }
}

fn remove_map_entry(
    text: &str,
    parent: &Mapping,
    key: &str,
) -> std::result::Result<String, String> {
    let Some(entry) = parent.find_entry_by_key(key) else {
        return Ok(text.to_string());
    };
    let key_start = entry
        .key_node()
        .and_then(|node| {
            node.as_scalar()
                .map(|scalar| scalar.byte_range().start as usize)
        })
        .ok_or_else(|| PLUGINS_ERR.to_string())?;
    // A `key:` whose only child was just removed has a null value and no value
    // node; fall back to the key position so we still drop the dangling line.
    let value_end = entry.value_node().and_then(|node| value_end(&node));
    if parent.is_flow_style() {
        let value_end = value_end.ok_or_else(|| PLUGINS_ERR.to_string())?;
        let (start, end) = expand_over_flow_separator(text, key_start, value_end);
        Ok(format!("{}{}", &text[..start], &text[end..]))
    } else {
        let line_start = line_start(text, key_start);
        let line_end = line_end_including_newline(text, value_end.unwrap_or(key_start));
        Ok(format!("{}{}", &text[..line_start], &text[line_end..]))
    }
}

/// Widen a `start..end` span to absorb one adjacent flow separator (`, `),
/// preferring the following comma and falling back to the preceding one.
fn expand_over_flow_separator(text: &str, start: usize, mut end: usize) -> (usize, usize) {
    let after = &text[end..];
    let trimmed = after.trim_start_matches(' ');
    if trimmed.starts_with(',') {
        end += (after.len() - trimmed.len()) + 1;
        if text[end..].starts_with(' ') {
            end += 1;
        }
        return (start, end);
    }
    let before = &text[..start];
    let trimmed = before.trim_end_matches(' ');
    if trimmed.ends_with(',') {
        return (trimmed.len() - 1, end);
    }
    (start, end)
}

fn disable_scalar(text: &str, container: &str, key: &str) -> std::result::Result<String, String> {
    let document = parse_profile(text)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    let Some(mapping) = root
        .get(container)
        .and_then(|node| node.as_mapping().cloned())
    else {
        return Ok(text.to_string());
    };
    if string_value(&mapping, key).as_deref() != Some("tracedecay") {
        return Ok(text.to_string());
    }
    let after = remove_map_entry(text, &mapping, key)?;

    let document = parse_profile(&after)?;
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    collapse_if_empty(&after, &root, container)
}

/// Remove `key` from `parent` when its value is now empty (an empty mapping or
/// sequence, or a null value left behind after its only child was removed) and
/// the entry carries no comments, anchors or aliases.
fn collapse_if_empty(
    text: &str,
    parent: &Mapping,
    key: &str,
) -> std::result::Result<String, String> {
    let Some(entry) = parent.find_entry_by_key(key) else {
        return Ok(text.to_string());
    };
    let empty = match entry.value_node() {
        None => true,
        Some(node) => {
            if let Some(mapping) = node.as_mapping() {
                mapping.keys().count() == 0
            } else if let Some(sequence) = node.as_sequence() {
                sequence.values().count() == 0
            } else if let Some(scalar) = node.as_scalar() {
                let value = scalar.as_string();
                value.is_empty() || value == "~" || value == "null"
            } else {
                false
            }
        }
    };
    if !empty {
        return Ok(text.to_string());
    }
    let Some(key_start) = entry.key_node().and_then(|node| {
        node.as_scalar()
            .map(|scalar| scalar.byte_range().start as usize)
    }) else {
        return Ok(text.to_string());
    };
    let end = entry
        .value_node()
        .and_then(|node| value_end(&node))
        .unwrap_or_else(|| line_end_including_newline(text, key_start));
    let span = &text[key_start..end.min(text.len())];
    if span.contains('#') || span.contains('&') || span.contains('*') {
        return Ok(text.to_string());
    }
    remove_map_entry(text, parent, key)
}

fn string_value(mapping: &Mapping, key: &str) -> Option<String> {
    mapping
        .get(key)?
        .as_scalar()
        .map(yaml_edit::Scalar::as_string)
}

fn sequence_contains(sequence: &Sequence, expected: &str) -> bool {
    sequence.values().any(|value| {
        value
            .as_scalar()
            .is_some_and(|scalar| scalar.as_string() == expected)
    })
}

fn write_config_file(path: &Path, contents: &str) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to read {}: {error}", path.display()),
            });
        }
    };
    if current.as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!("failed to create {}: {error}", parent.display()),
        })?;
    }
    let backup = backup_config_file(path)?;
    atomic_write(
        path,
        "hermes-config",
        contents.as_bytes(),
        DIRECTORY_SYNC_POLICY,
    )
    .map_err(|error| {
        let backup_hint = backup
            .as_ref()
            .map(|path| format!(" Backup is at {}.", path.display()))
            .unwrap_or_default();
        TraceDecayError::Config {
            message: format!(
                "failed to atomically replace {}: {error}.{backup_hint}",
                path.display()
            ),
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[derive(Debug, Clone, Copy)]
    enum Mutation {
        Enable,
        Disable,
    }

    struct CorpusCase {
        name: &'static str,
        mutation: Mutation,
        input: &'static str,
        preserved: &'static [&'static str],
        removed: &'static [&'static str],
        crlf: bool,
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    }

    fn mutate(case: &CorpusCase) -> String {
        match case.mutation {
            Mutation::Enable => enable_plugin_config(case.input).unwrap(),
            Mutation::Disable => disable_plugin_config(case.input).unwrap(),
        }
    }

    fn assert_enabled(contents: &str) {
        let config = ProfileConfigDocument::parse(contents).unwrap();
        let root = config.root();
        let plugins = root.get_mapping("plugins").unwrap();
        assert!(sequence_contains(
            &plugins.get_sequence("enabled").unwrap(),
            "tracedecay"
        ));
        assert!(
            plugins
                .get_sequence("disabled")
                .is_none_or(|disabled| !sequence_contains(&disabled, "tracedecay"))
        );
        assert_eq!(
            root.get_mapping("memory")
                .and_then(|memory| string_value(&memory, "provider"))
                .as_deref(),
            Some("tracedecay")
        );
        assert_eq!(
            root.get_mapping("context")
                .and_then(|context| string_value(&context, "engine"))
                .as_deref(),
            Some("tracedecay")
        );
    }

    fn assert_disabled(contents: &str) {
        let config = ProfileConfigDocument::parse(contents).unwrap();
        let root = config.root();
        if let Some(plugins) = root.get_mapping("plugins") {
            assert!(
                plugins
                    .get_sequence("enabled")
                    .is_none_or(|enabled| !sequence_contains(&enabled, "tracedecay"))
            );
            assert!(
                plugins
                    .get_mapping("tracedecay")
                    .is_none_or(|plugin| !plugin.contains_key("project_root"))
            );
        }
        assert_ne!(
            root.get_mapping("memory")
                .and_then(|memory| string_value(&memory, "provider"))
                .as_deref(),
            Some("tracedecay")
        );
        assert_ne!(
            root.get_mapping("context")
                .and_then(|context| string_value(&context, "engine"))
                .as_deref(),
            Some("tracedecay")
        );
    }

    #[test]
    fn lossless_profile_config_corpus() {
        let cases = [
            CorpusCase {
                name: "quoted keys and flow collections",
                mutation: Mutation::Enable,
                input: concat!(
                    "# leading comment\n",
                    "\"plugins\": {enabled: [other], disabled: [tracedecay, blocked], ",
                    "tracedecay: {project_root: \"/legacy\", keep: yes}}\n",
                    "memory: {note: \"keep me\"}\n",
                    "context: {note: 'keep me too'}\n",
                    "unknown: {quoted: \"value\"}\n",
                ),
                preserved: &[
                    "# leading comment",
                    "\"plugins\"",
                    "blocked",
                    "keep: yes",
                    "note: \"keep me\"",
                    "note: 'keep me too'",
                    "unknown: {quoted: \"value\"}",
                ],
                removed: &["project_root:"],
                crlf: false,
            },
            CorpusCase {
                name: "anchors aliases and merge keys",
                mutation: Mutation::Enable,
                input: concat!(
                    "defaults: &defaults {color: blue, retries: 3}\n",
                    "plugins:\n",
                    "  enabled: [other]\n",
                    "  tracedecay:\n",
                    "    project_root: /legacy\n",
                    "    options: *defaults\n",
                    "consumer:\n",
                    "  <<: *defaults\n",
                ),
                preserved: &[
                    "&defaults",
                    "options: *defaults",
                    "<<: *defaults",
                    "color: blue",
                    "retries: 3",
                ],
                removed: &["project_root:"],
                crlf: false,
            },
            CorpusCase {
                name: "crlf and unknown fields",
                mutation: Mutation::Enable,
                input: concat!(
                    "theme: dark\r\n",
                    "plugins:\r\n",
                    "  enabled:\r\n",
                    "    - other\r\n",
                    "custom:\r\n",
                    "  nested: true\r\n",
                ),
                preserved: &["theme: dark", "custom:", "nested: true"],
                removed: &[],
                crlf: true,
            },
            CorpusCase {
                name: "disable only owned paths",
                mutation: Mutation::Disable,
                input: concat!(
                    "# profile comment\n",
                    "plugins:\n",
                    "  enabled: [tracedecay, other]\n",
                    "  tracedecay:\n",
                    "    project_root: /legacy\n",
                    "    summary_model: glm-5\n",
                    "memory: {provider: tracedecay, keep: true}\n",
                    "context: {engine: tracedecay, budget: 42}\n",
                    "hooks: &hooks {pre_tool: keep}\n",
                    "mcp: {servers: *hooks}\n",
                ),
                preserved: &[
                    "# profile comment",
                    "other",
                    "summary_model: glm-5",
                    "keep: true",
                    "budget: 42",
                    "hooks: &hooks",
                    "mcp: {servers: *hooks}",
                ],
                removed: &["project_root:"],
                crlf: false,
            },
        ];

        for case in &cases {
            let updated = mutate(case);
            for expected in case.preserved {
                assert!(
                    updated.contains(expected),
                    "{} did not preserve {expected:?}:\n{updated}",
                    case.name
                );
            }
            for removed in case.removed {
                assert!(
                    !updated.contains(removed),
                    "{} retained {removed:?}:\n{updated}",
                    case.name
                );
            }
            if case.crlf {
                assert!(
                    updated
                        .as_bytes()
                        .windows(2)
                        .filter(|window| *window == b"\r\n")
                        .count()
                        > 0,
                    "{} lost CRLF line endings",
                    case.name
                );
                assert!(
                    !updated.replace("\r\n", "").contains('\n'),
                    "{} introduced bare LF line endings",
                    case.name
                );
            }
            match case.mutation {
                Mutation::Enable => assert_enabled(&updated),
                Mutation::Disable => assert_disabled(&updated),
            }
        }
    }

    #[test]
    fn enable_plugin_creates_missing_profile_config() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join(".hermes/profiles/work/config.yaml");

        enable_plugin(&config).unwrap();

        let updated = read(&config);
        assert_enabled(&updated);
        assert!(
            !config.with_extension("yaml.bak").exists(),
            "first write should not create a backup for a missing config"
        );
    }

    #[test]
    fn disable_plugin_ignores_missing_config() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join(".hermes/profiles/missing/config.yaml");

        disable_plugin(&config).unwrap();

        assert!(!config.exists());
    }

    #[test]
    fn enable_plugin_backs_up_existing_config_before_atomic_write() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("config.yaml");
        let original = "theme: dark\nplugins:\n  enabled:\n    - other\n";
        std::fs::write(&config, original).unwrap();

        enable_plugin(&config).unwrap();

        let backup = dir.path().join("config.yaml.bak");
        assert!(backup.exists());
        assert_eq!(read(&backup), original);
    }
}
