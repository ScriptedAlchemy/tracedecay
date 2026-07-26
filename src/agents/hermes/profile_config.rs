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
use yaml_edit::{Document, Mapping, Sequence, SequenceBuilder};

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

// Mutations go through yaml-edit's CST handles. The library preserves comments,
// quoting, anchors, aliases, collection style, and unrelated fields while the
// installer changes only the owned mapping entries and sequence members.

fn enable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    let line_ending = LineEnding::detect(existing);
    let normalized = line_ending.normalize(existing);
    let document = parse_profile(&normalized)?;
    enable_document(&document)?;
    let updated = document.to_string();
    Ok(line_ending.restore(updated))
}

fn disable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let line_ending = LineEnding::detect(existing);
    let normalized = line_ending.normalize(existing);
    let document = parse_profile(&normalized)?;
    disable_document(&document)?;
    let updated = document.to_string();
    Ok(line_ending.restore(updated))
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

fn enable_document(document: &Document) -> std::result::Result<(), String> {
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));

    let plugins = ensure_mapping(&root, "plugins", PLUGINS_ERR)?;
    remove_sequence_value_if_present(&plugins, "disabled", "tracedecay", PLUGINS_ERR)?;
    remove_legacy_project_pin(&plugins)?;
    match plugins.get("enabled") {
        None => {
            let enabled = SequenceBuilder::new()
                .item("tracedecay")
                .build_document()
                .as_sequence()
                .ok_or_else(|| PLUGINS_ERR.to_string())?;
            plugins.set("enabled", enabled);
        }
        Some(enabled_node) => {
            let Some(enabled) = enabled_node.as_sequence() else {
                return Err(PLUGINS_ERR.to_string());
            };
            if !sequence_contains(enabled, "tracedecay") {
                enabled.push("tracedecay");
            }
        }
    }

    ensure_scalar(
        &root,
        "memory",
        "provider",
        "tracedecay",
        &[],
        MEMORY_ERR,
        "Hermes memory provider already configured; refusing to overwrite it",
    )?;
    ensure_scalar(
        &root,
        "context",
        "engine",
        "tracedecay",
        &["compressor"],
        CONTEXT_ERR,
        "Hermes context engine already configured; refusing to overwrite it",
    )
}

fn disable_document(document: &Document) -> std::result::Result<(), String> {
    let root = document
        .as_mapping()
        .unwrap_or_else(|| panic!("parse_profile guarantees a mapping"));
    if let Some(plugins_node) = root.get("plugins") {
        let Some(plugins) = plugins_node.as_mapping() else {
            return Err(PLUGINS_ERR.to_string());
        };
        remove_sequence_value_if_present(plugins, "enabled", "tracedecay", PLUGINS_ERR)?;
        remove_legacy_project_pin(plugins)?;
    }
    disable_scalar(&root, "context", "engine")?;
    disable_scalar(&root, "memory", "provider")
}

fn ensure_mapping(
    parent: &Mapping,
    key: &str,
    unsupported: &str,
) -> std::result::Result<Mapping, String> {
    if parent.get(key).is_none() {
        parent.set(key, Mapping::new());
    }
    parent
        .get_mapping(key)
        .ok_or_else(|| unsupported.to_string())
}

fn ensure_scalar(
    root: &Mapping,
    container: &str,
    key: &str,
    value: &str,
    allow_overwrite: &[&str],
    unsupported: &str,
    conflict: &str,
) -> std::result::Result<(), String> {
    let mapping = ensure_mapping(root, container, unsupported)?;
    if !mapping.contains_key(key) {
        mapping.set(key, value);
        return Ok(());
    }
    match string_value(&mapping, key).as_deref() {
        Some(current) if current == value => Ok(()),
        Some(current) if allow_overwrite.contains(&current) => {
            mapping.set(key, value);
            Ok(())
        }
        Some(_) => Err(conflict.to_string()),
        None => Err(unsupported.to_string()),
    }
}

/// Remove the legacy `plugins.tracedecay.project_root` pin, collapsing an
/// otherwise-empty `tracedecay` mapping in place so any comments attached to
/// that mapping remain owned by the user.
fn remove_legacy_project_pin(plugins: &Mapping) -> std::result::Result<(), String> {
    let Some(tracedecay) = plugins
        .get("tracedecay")
        .and_then(|node| node.as_mapping().cloned())
    else {
        return Ok(());
    };
    tracedecay.remove("project_root");
    Ok(())
}

fn remove_sequence_value_if_present(
    mapping: &Mapping,
    key: &str,
    value: &str,
    unsupported: &str,
) -> std::result::Result<(), String> {
    let Some(node) = mapping.get(key) else {
        return Ok(());
    };
    let Some(sequence) = node.as_sequence() else {
        return Err(unsupported.to_string());
    };
    let indexes = sequence
        .values()
        .enumerate()
        .filter_map(|(index, node)| {
            node.as_scalar()
                .is_some_and(|scalar| scalar.as_string() == value)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    for index in indexes.into_iter().rev() {
        sequence.remove(index);
    }
    Ok(())
}

fn disable_scalar(root: &Mapping, container: &str, key: &str) -> std::result::Result<(), String> {
    let Some(mapping) = root
        .get(container)
        .and_then(|node| node.as_mapping().cloned())
    else {
        return Ok(());
    };
    if string_value(&mapping, key).as_deref() != Some("tracedecay") {
        return Ok(());
    }
    mapping.remove(key);
    Ok(())
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
