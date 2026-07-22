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
    document: Document,
    root: Mapping,
    line_ending: LineEnding,
}

impl ProfileConfigDocument {
    fn parse(contents: &str) -> std::result::Result<Self, String> {
        let line_ending = LineEnding::detect(contents);
        let normalized = line_ending.normalize(contents);
        let document = if normalized.trim().is_empty() {
            Document::new_mapping()
        } else {
            Document::from_str(&normalized)
                .map_err(|error| format!("invalid Hermes YAML config: {error}"))?
        };
        let root = document
            .as_mapping()
            .ok_or_else(|| "unsupported Hermes config; expected a top-level mapping".to_string())?;
        Ok(Self {
            document,
            root,
            line_ending,
        })
    }

    fn render(&self) -> String {
        self.line_ending.restore(self.document.to_string())
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

fn enable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    let config = ProfileConfigDocument::parse(existing)?;
    let root = config.root();
    let plugins = get_or_insert_mapping(&root, "plugins", "unsupported Hermes plugins config")?;

    remove_legacy_project_pin(&plugins)?;

    if let Some(disabled) =
        optional_sequence(&plugins, "disabled", "unsupported Hermes plugins config")?
    {
        remove_sequence_value(&disabled, "tracedecay");
    }
    let enabled = get_or_insert_sequence(&plugins, "enabled", "unsupported Hermes plugins config")?;
    if !sequence_contains(&enabled, "tracedecay") {
        enabled.push("tracedecay");
    }

    enable_memory_provider(&root)?;
    enable_context_engine(&root)?;
    Ok(config.render())
}

fn disable_plugin_config(existing: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }

    let config = ProfileConfigDocument::parse(existing)?;
    let root = config.root();
    if let Some(plugins) = optional_mapping(&root, "plugins", "unsupported Hermes plugins config")?
    {
        if let Some(enabled) =
            optional_sequence(&plugins, "enabled", "unsupported Hermes plugins config")?
        {
            remove_sequence_value(&enabled, "tracedecay");
        }
        remove_legacy_project_pin(&plugins)?;
    }

    disable_context_engine(&root)?;
    disable_memory_provider(&root)?;
    Ok(config.render())
}

fn remove_legacy_project_pin(plugins: &Mapping) -> std::result::Result<(), String> {
    let Some(tracedecay) =
        optional_mapping(plugins, "tracedecay", "unsupported Hermes plugins config")?
    else {
        return Ok(());
    };
    if tracedecay.remove("project_root").is_some() {
        remove_empty_mapping(plugins, "tracedecay", &tracedecay);
    }
    Ok(())
}

fn enable_memory_provider(root: &Mapping) -> std::result::Result<(), String> {
    let memory = get_or_insert_mapping(root, "memory", "unsupported Hermes memory config")?;
    match string_value(&memory, "provider") {
        Some(provider) if provider == "tracedecay" => Ok(()),
        Some(_) => {
            Err("Hermes memory provider already configured; refusing to overwrite it".to_string())
        }
        None if memory.contains_key("provider") => {
            Err("unsupported Hermes memory config".to_string())
        }
        None => {
            memory.set("provider", "tracedecay");
            Ok(())
        }
    }
}

fn disable_memory_provider(root: &Mapping) -> std::result::Result<(), String> {
    let Some(memory) = optional_mapping(root, "memory", "unsupported Hermes memory config")? else {
        return Ok(());
    };
    if string_value(&memory, "provider").as_deref() == Some("tracedecay") {
        memory.remove("provider");
        remove_empty_mapping(root, "memory", &memory);
    }
    Ok(())
}

fn enable_context_engine(root: &Mapping) -> std::result::Result<(), String> {
    let context = get_or_insert_mapping(root, "context", "unsupported Hermes context config")?;
    match string_value(&context, "engine").as_deref() {
        None if context.contains_key("engine") => {
            Err("unsupported Hermes context config".to_string())
        }
        None | Some("compressor") => {
            context.set("engine", "tracedecay");
            Ok(())
        }
        Some("tracedecay") => Ok(()),
        Some(_) => {
            Err("Hermes context engine already configured; refusing to overwrite it".to_string())
        }
    }
}

fn disable_context_engine(root: &Mapping) -> std::result::Result<(), String> {
    let Some(context) = optional_mapping(root, "context", "unsupported Hermes context config")?
    else {
        return Ok(());
    };
    if string_value(&context, "engine").as_deref() == Some("tracedecay") {
        context.remove("engine");
        remove_empty_mapping(root, "context", &context);
    }
    Ok(())
}

fn optional_mapping(
    parent: &Mapping,
    key: &str,
    error: &str,
) -> std::result::Result<Option<Mapping>, String> {
    let Some(value) = parent.get(key) else {
        return Ok(None);
    };
    value
        .as_mapping()
        .cloned()
        .map(Some)
        .ok_or_else(|| error.to_string())
}

fn get_or_insert_mapping(
    parent: &Mapping,
    key: &str,
    error: &str,
) -> std::result::Result<Mapping, String> {
    if let Some(mapping) = optional_mapping(parent, key, error)? {
        return Ok(mapping);
    }
    let mapping = empty_mapping(parent.is_flow_style())?;
    parent.set(key, &mapping);
    parent
        .get_mapping(key)
        .ok_or_else(|| "yaml-edit failed to insert a Hermes mapping".to_string())
}

fn optional_sequence(
    parent: &Mapping,
    key: &str,
    error: &str,
) -> std::result::Result<Option<Sequence>, String> {
    let Some(value) = parent.get(key) else {
        return Ok(None);
    };
    value
        .as_sequence()
        .cloned()
        .map(Some)
        .ok_or_else(|| error.to_string())
}

fn get_or_insert_sequence(
    parent: &Mapping,
    key: &str,
    error: &str,
) -> std::result::Result<Sequence, String> {
    if let Some(sequence) = optional_sequence(parent, key, error)? {
        return Ok(sequence);
    }
    let sequence = empty_sequence(parent.is_flow_style())?;
    parent.set(key, &sequence);
    parent
        .get_sequence(key)
        .ok_or_else(|| "yaml-edit failed to insert a Hermes sequence".to_string())
}

fn empty_mapping(flow_style: bool) -> std::result::Result<Mapping, String> {
    if !flow_style {
        return Ok(Mapping::new());
    }
    Document::from_str("{}")
        .map_err(|error| format!("yaml-edit failed to build a flow mapping: {error}"))?
        .as_mapping()
        .ok_or_else(|| "yaml-edit failed to build a flow mapping".to_string())
}

fn empty_sequence(flow_style: bool) -> std::result::Result<Sequence, String> {
    let document = if flow_style {
        Document::from_str("[]")
            .map_err(|error| format!("yaml-edit failed to build a flow sequence: {error}"))?
    } else {
        SequenceBuilder::new().build_document()
    };
    document
        .as_sequence()
        .ok_or_else(|| "yaml-edit failed to build a sequence".to_string())
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

fn remove_sequence_value(sequence: &Sequence, expected: &str) {
    while let Some(index) = sequence.values().position(|value| {
        value
            .as_scalar()
            .is_some_and(|scalar| scalar.as_string() == expected)
    }) {
        sequence.remove(index);
    }
}

fn remove_empty_mapping(parent: &Mapping, key: &str, mapping: &Mapping) {
    if mapping.is_empty() {
        let syntax = mapping.to_string();
        if !syntax.contains('#') && !syntax.contains('&') && !syntax.contains('*') {
            parent.remove(key);
        }
    }
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
