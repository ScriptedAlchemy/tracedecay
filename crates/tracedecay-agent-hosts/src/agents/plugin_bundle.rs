//! Shared plugin bundle registry.
//!
//! The source tree is unified where host formats match; host-specific overlays
//! remain where each installer needs a different manifest, hook, command, or
//! agent format.
//!
//! Layout of `plugin/`:
//! - `plugin/skills/*/SKILL.md` — the 15 shared model-invocable skills. All
//!   three hosts deploy the full set; the workflow dispatcher skills were
//!   removed (their behavior lives in the native slash commands below), so no
//!   host filters the skill set today. The `cursor_skill_files` filter is kept
//!   as a guard against a dispatcher skill being reintroduced.
//! - `plugin/overlays/cursor/commands/tracedecay-*.md` — Cursor 1.6+ native
//!   slash commands, one per workflow slug, deployed to `commands/<slug>.md`.
//!   These provide the explicit workflow dispatch (no dispatcher *skills*).
//! - `plugin/agents/*.md` — canonical subagents. Claude deploys them verbatim;
//!   build.rs derives Cursor markdown and Codex TOML adapters from them.
//! - `plugin/commands/*.md` — Claude slash commands.
//! - `plugin/rules/*.mdc` — Cursor rules.
//! - `plugin/hooks/hooks-<host>.json` — per-host hook wiring; each deploys to
//!   `hooks/hooks.json`.
//! - `plugin/.claude-plugin/{plugin,marketplace}.json`,
//!   `plugin/.cursor-plugin/plugin.json`, `plugin/.codex-plugin/plugin.json` —
//!   host manifests (deploy to the same dot-dir path).
//! - `plugin/.mcp.json` — shared Claude/Codex MCP config (byte-identical);
//!   `plugin/mcp-cursor.json` — Cursor MCP config (deploys to `mcp.json`).
//! - `plugin/README-<host>.md` — per-host README (deploys to `README.md`).
//!
//! Composed per-host view = `GENERATED_SKILL_FILES` (recursively embedded from
//! `plugin/skills/`, filtered per host) ∪ `<HOST>_MANIFEST_FILES` and extras.

use crate::errors::Result;

/// Stamp the plugin manifest `version` field with the crate version, returning
/// pretty-printed JSON with a trailing newline. Shared by every host installer
/// (Claude/Cursor/Codex), which all render the same manifest round-trip.
pub(crate) fn stamp_manifest_version(raw: &str) -> Result<String> {
    stamp_manifest_version_with(raw, |_| {})
}

/// Stamp the version and let the host apply manifest edits on the parsed
/// `Value` before the single serialize — hosts that post-process the manifest
/// (e.g. Codex stripping `hooks` from repo-local bundles) avoid a second
/// parse/pretty-print round-trip and cannot drift from this output contract.
pub(crate) fn stamp_manifest_version_with(
    raw: &str,
    mutate: impl FnOnce(&mut serde_json::Value),
) -> Result<String> {
    let mut manifest: serde_json::Value = serde_json::from_str(raw)?;
    manifest["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    mutate(&mut manifest);
    Ok(format!("{}\n", serde_json::to_string_pretty(&manifest)?))
}

/// Point the MCP config's sole `mcpServers.<key>.command` at the resolved
/// binary path, returning pretty-printed JSON with a trailing newline. Claude
/// and Cursor use this directly; Codex layers scope-specific args/env on top.
///
/// Host templates choose the server key deliberately:
/// - Claude/Codex keep `graph` so namespaced UIs render `tracedecay graph`
///   rather than the redundant `tracedecay tracedecay`.
/// - Cursor uses `tracedecay` because Settings surfaces the MCP server key
///   literally (`plugin-tracedecay-graph` looked like a bare "graph" entry).
pub(crate) fn set_mcp_command(raw: &str, bin: &str) -> Result<String> {
    let mut mcp: serde_json::Value = serde_json::from_str(raw)?;
    let servers = mcp
        .get_mut("mcpServers")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "plugin MCP config is missing mcpServers object".to_string(),
        })?;
    let key = if servers.contains_key("tracedecay") {
        "tracedecay"
    } else if servers.contains_key("graph") {
        "graph"
    } else {
        return Err(crate::errors::TraceDecayError::Config {
            message: "plugin MCP config must declare mcpServers.tracedecay or mcpServers.graph"
                .to_string(),
        });
    };
    servers
        .get_mut(key)
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("plugin MCP config is missing mcpServers.{key}"),
        })?
        .as_object_mut()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("plugin MCP config mcpServers.{key} must be an object"),
        })?
        .insert("command".to_string(), serde_json::json!(bin));
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

/// One embedded plugin file: `relative` is its deploy path; `contents` may come
/// from a different source path in the shared `plugin/` tree.
#[derive(Clone, Copy)]
pub struct PluginFile {
    pub relative: &'static str,
    pub contents: &'static str,
}

macro_rules! plugin_file {
    ($relative:literal, $source:literal) => {
        PluginFile {
            relative: $relative,
            contents: include_str!(concat!("../../../../plugin/", $source)),
        }
    };
}

// Every shared skill and canonical/generated agent file, embedded by build.rs.
include!(concat!(env!("OUT_DIR"), "/plugin_bundle_generated.rs"));

pub(crate) fn codex_agent_files() -> &'static [PluginFile] {
    GENERATED_CODEX_AGENT_FILES
}

/// Prefix of the dispatcher skills that Cursor does **not** deploy (they are
/// native commands on Cursor). Claude/Codex deploy every skill.
const CURSOR_EXCLUDED_SKILL_PREFIX: &str = "skills/tracedecay-";

fn all_skill_files() -> impl Iterator<Item = &'static PluginFile> {
    GENERATED_SKILL_FILES.iter()
}

fn cursor_skill_files() -> impl Iterator<Item = &'static PluginFile> {
    GENERATED_SKILL_FILES
        .iter()
        .filter(|file| !file.relative.starts_with(CURSOR_EXCLUDED_SKILL_PREFIX))
}

/// Cursor's native slash commands for the canonical workflow slugs.
const CURSOR_COMMAND_FILES: &[PluginFile] = &[
    plugin_file!(
        "commands/tracedecay-audit-safety.md",
        "overlays/cursor/commands/tracedecay-audit-safety.md"
    ),
    plugin_file!(
        "commands/tracedecay-check-health.md",
        "overlays/cursor/commands/tracedecay-check-health.md"
    ),
    plugin_file!(
        "commands/tracedecay-clean-dead-code.md",
        "overlays/cursor/commands/tracedecay-clean-dead-code.md"
    ),
    plugin_file!(
        "commands/tracedecay-compare-branches.md",
        "overlays/cursor/commands/tracedecay-compare-branches.md"
    ),
    plugin_file!(
        "commands/tracedecay-curate-memory.md",
        "overlays/cursor/commands/tracedecay-curate-memory.md"
    ),
    plugin_file!(
        "commands/tracedecay-draft-commit.md",
        "overlays/cursor/commands/tracedecay-draft-commit.md"
    ),
    plugin_file!(
        "commands/tracedecay-find-impact.md",
        "overlays/cursor/commands/tracedecay-find-impact.md"
    ),
    plugin_file!(
        "commands/tracedecay-fix-build.md",
        "overlays/cursor/commands/tracedecay-fix-build.md"
    ),
    plugin_file!(
        "commands/tracedecay-map-architecture.md",
        "overlays/cursor/commands/tracedecay-map-architecture.md"
    ),
    plugin_file!(
        "commands/tracedecay-port-code.md",
        "overlays/cursor/commands/tracedecay-port-code.md"
    ),
    plugin_file!(
        "commands/tracedecay-recall-memory.md",
        "overlays/cursor/commands/tracedecay-recall-memory.md"
    ),
    plugin_file!(
        "commands/tracedecay-review-diff.md",
        "overlays/cursor/commands/tracedecay-review-diff.md"
    ),
    plugin_file!(
        "commands/tracedecay-test-changes.md",
        "overlays/cursor/commands/tracedecay-test-changes.md"
    ),
];

/// Claude slash commands.
const CLAUDE_COMMAND_FILES: &[PluginFile] = &[
    plugin_file!("commands/audit-safety.md", "commands/audit-safety.md"),
    plugin_file!("commands/check-health.md", "commands/check-health.md"),
    plugin_file!("commands/clean-dead-code.md", "commands/clean-dead-code.md"),
    plugin_file!(
        "commands/compare-branches.md",
        "commands/compare-branches.md"
    ),
    plugin_file!("commands/curate-memory.md", "commands/curate-memory.md"),
    plugin_file!("commands/draft-commit.md", "commands/draft-commit.md"),
    plugin_file!("commands/find-impact.md", "commands/find-impact.md"),
    plugin_file!("commands/fix-build.md", "commands/fix-build.md"),
    plugin_file!(
        "commands/map-architecture.md",
        "commands/map-architecture.md"
    ),
    plugin_file!("commands/port-code.md", "commands/port-code.md"),
    plugin_file!("commands/recall-memory.md", "commands/recall-memory.md"),
    plugin_file!("commands/review-diff.md", "commands/review-diff.md"),
    plugin_file!("commands/test-changes.md", "commands/test-changes.md"),
];

/// Cursor `.mdc` rules.
const CURSOR_RULE_FILES: &[PluginFile] = &[
    plugin_file!("rules/tracedecay.mdc", "rules/tracedecay.mdc"),
    plugin_file!("rules/tracedecay-memory.mdc", "rules/tracedecay-memory.mdc"),
];

/// Claude manifest dir + shared MCP + Claude hooks + README.
pub const CLAUDE_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(
        ".claude-plugin/marketplace.json",
        ".claude-plugin/marketplace.json"
    ),
    plugin_file!(".claude-plugin/plugin.json", ".claude-plugin/plugin.json"),
    plugin_file!(".mcp.json", ".mcp.json"),
    plugin_file!("README.md", "README-claude.md"),
    plugin_file!("hooks/hooks.json", "hooks/hooks-claude.json"),
];

/// Cursor manifest + Cursor MCP + Cursor hooks + README.
pub const CURSOR_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(".cursor-plugin/plugin.json", ".cursor-plugin/plugin.json"),
    plugin_file!("README.md", "README-cursor.md"),
    plugin_file!("mcp.json", "mcp-cursor.json"),
    plugin_file!("hooks/hooks.json", "hooks/hooks-cursor.json"),
];

/// Codex manifest + shared MCP + Codex hooks + README.
pub const CODEX_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(".codex-plugin/plugin.json", ".codex-plugin/plugin.json"),
    plugin_file!(".mcp.json", ".mcp.json"),
    plugin_file!("README.md", "README-codex.md"),
    plugin_file!("hooks/hooks.json", "hooks/hooks-codex.json"),
];

/// Compose a host's deploy set as deterministic `(relative, contents)` tuples.
fn compose(
    sections: &[&'static [PluginFile]],
    skills: impl Iterator<Item = &'static PluginFile>,
) -> Vec<(&'static str, &'static str)> {
    sections
        .iter()
        .flat_map(|section| section.iter())
        .chain(skills)
        .map(|file| (file.relative, file.contents))
        .collect()
}

/// Files Claude deploys: manifest + Claude agents + Claude commands + every
/// skill file (all 30 skills incl. dispatchers, plus any support files).
pub fn claude_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[
            CLAUDE_MANIFEST_FILES,
            GENERATED_CLAUDE_AGENT_FILES,
            CLAUDE_COMMAND_FILES,
        ],
        all_skill_files(),
    )
}

/// Files Cursor deploys: manifest + Cursor rules + Cursor agents + Cursor
/// native commands + the shared skill files *without* the `tracedecay-*`
/// dispatcher skills (those slugs are native commands on Cursor).
pub fn cursor_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[
            CURSOR_MANIFEST_FILES,
            CURSOR_RULE_FILES,
            GENERATED_CURSOR_AGENT_FILES,
            CURSOR_COMMAND_FILES,
        ],
        cursor_skill_files(),
    )
}

/// Files Codex deploys: manifest + every skill file (all 30 skills incl.
/// dispatchers, plus any support files). Codex ships no agents/commands/rules.
pub fn codex_files() -> Vec<(&'static str, &'static str)> {
    compose(&[CODEX_MANIFEST_FILES], all_skill_files())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn plugin_source_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin")
    }

    /// No host deploys the same relative path twice.
    fn assert_unique_relatives(files: &[(&str, &str)], host: &str) {
        let mut seen = BTreeSet::new();
        for (relative, _) in files {
            assert!(
                seen.insert(*relative),
                "{host}: duplicate deploy path {relative}"
            );
        }
    }

    fn embedded_skill(relative: &str) -> &'static str {
        GENERATED_SKILL_FILES
            .iter()
            .find(|file| file.relative == relative)
            .map(|file| file.contents)
            .unwrap_or_else(|| panic!("shared skill should be embedded: {relative}"))
    }

    #[test]
    fn set_mcp_command_updates_tracedecay_or_graph_key() {
        let tracedecay = set_mcp_command(
            r#"{"mcpServers":{"tracedecay":{"type":"stdio","command":"tracedecay","args":["serve"]}}}"#,
            "/abs/tracedecay",
        )
        .unwrap();
        let tracedecay: serde_json::Value = serde_json::from_str(&tracedecay).unwrap();
        assert_eq!(
            tracedecay["mcpServers"]["tracedecay"]["command"],
            "/abs/tracedecay"
        );
        assert!(tracedecay["mcpServers"].get("graph").is_none());

        let graph = set_mcp_command(
            r#"{"mcpServers":{"graph":{"type":"stdio","command":"tracedecay","args":["serve"]}}}"#,
            "/abs/tracedecay",
        )
        .unwrap();
        let graph: serde_json::Value = serde_json::from_str(&graph).unwrap();
        assert_eq!(graph["mcpServers"]["graph"]["command"], "/abs/tracedecay");
        assert!(graph["mcpServers"].get("tracedecay").is_none());
    }

    #[test]
    fn set_mcp_command_rejects_missing_server_key() {
        let err = set_mcp_command(r#"{"mcpServers":{}}"#, "/abs/tracedecay").unwrap_err();
        assert!(
            err.to_string()
                .contains("mcpServers.tracedecay or mcpServers.graph"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn each_host_deploys_unique_relative_paths() {
        assert_unique_relatives(&claude_files(), "claude");
        assert_unique_relatives(&cursor_files(), "cursor");
        assert_unique_relatives(&codex_files(), "codex");
    }

    #[test]
    fn cursor_mcp_template_uses_tracedecay_key() {
        let mcp = cursor_files()
            .into_iter()
            .find(|(relative, _)| *relative == "mcp.json")
            .map(|(_, contents)| contents)
            .expect("cursor deploy set must include mcp.json");
        let parsed: serde_json::Value = serde_json::from_str(mcp).unwrap();
        assert!(
            parsed["mcpServers"]["tracedecay"].is_object(),
            "Cursor mcp-cursor.json must declare mcpServers.tracedecay"
        );
        assert!(
            parsed["mcpServers"].get("graph").is_none(),
            "Cursor mcp-cursor.json must not declare mcpServers.graph"
        );
    }

    #[test]
    fn every_embedded_file_has_content() {
        // The macro embeds at compile time, so a missing source fails the build.
        // Every file we ship (skills, manifests, mcp, hooks, README) is
        // non-empty, so an empty embed signals a truncated or wrong source.
        for host in [claude_files(), cursor_files(), codex_files()] {
            for (relative, contents) in host {
                assert!(!contents.is_empty(), "{relative} embedded empty");
            }
        }
    }

    #[test]
    fn each_host_composes_the_expected_file_count() {
        // Skill files are embedded recursively (SKILL.md + support files), so
        // the skill count is derived from the generated set rather than a
        // frozen literal. The `tracedecay-*` dispatcher skills were removed, so
        // Cursor's subset now equals the full skill set; the filter is kept as a
        // guard against a dispatcher skill ever being reintroduced.
        let all_skills = GENERATED_SKILL_FILES.len();
        let cursor_skills = cursor_skill_files().count();

        // Claude: skills + 5 manifest (2 dot + mcp + hooks + README) + native
        // agents + 13 commands.
        assert_eq!(
            claude_files().len(),
            all_skills + 5 + GENERATED_CLAUDE_AGENT_FILES.len() + 13
        );
        // Cursor: cursor-subset skills + 4 manifest (dot + mcp + hooks +
        //   README) + 2 rules + native agents + 13 native commands.
        assert_eq!(
            cursor_files().len(),
            cursor_skills + 4 + 2 + GENERATED_CURSOR_AGENT_FILES.len() + 13
        );
        // Codex: skills + 4 manifest (dot + mcp + hooks + README).
        assert_eq!(codex_files().len(), all_skills + 4);
    }

    #[test]
    fn capability_discovery_skill_is_shared_and_cli_native() {
        let relative = "skills/discovering-tracedecay/SKILL.md";
        let source = embedded_skill(relative);

        assert!(source.contains("`tracedecay tool`"));
        assert!(source.contains("`tracedecay tool <name> --help`"));
        assert!(source.contains("`tracedecay --help`"));
        assert!(
            !source.contains("tool describe"),
            "the CLI has no `tool describe` subcommand"
        );

        for (host, files) in [
            ("claude", claude_files()),
            ("cursor", cursor_files()),
            ("codex", codex_files()),
        ] {
            let deployed = files
                .iter()
                .find(|(path, _)| *path == relative)
                .map(|(_, contents)| *contents)
                .unwrap_or_else(|| panic!("{host} is missing {relative}"));
            assert_eq!(
                deployed, source,
                "{host} must ship the shared source verbatim"
            );
        }
    }

    #[test]
    fn using_cli_skill_supports_intentional_mcp_absence() {
        let source = embedded_skill("skills/using-the-cli/SKILL.md");

        assert!(source.contains("MCP is optional"));
        assert!(source.contains("intentionally unavailable"));
    }

    /// Every embedded skill file maps to an on-disk source under `plugin/`.
    #[test]
    fn generated_skill_files_have_source_paths() {
        let root = plugin_source_root();
        assert!(
            !GENERATED_SKILL_FILES.is_empty(),
            "generated skill file set is empty"
        );
        for file in GENERATED_SKILL_FILES {
            assert!(
                root.join(file.relative).exists(),
                "skill source missing: plugin/{}",
                file.relative
            );
        }
    }

    /// The recursive embed must cover the on-disk skill tree exactly — every
    /// file under `plugin/skills/` is embedded, and nothing extra.
    #[test]
    fn generated_skill_files_cover_the_skill_tree_exactly() {
        let skills_root = plugin_source_root().join("skills");
        let mut on_disk = BTreeSet::new();
        collect_relative(&skills_root, &skills_root, &mut on_disk);

        let embedded: BTreeSet<String> = GENERATED_SKILL_FILES
            .iter()
            .map(|file| {
                file.relative
                    .strip_prefix("skills/")
                    .expect("skill deploy path is under skills/")
                    .to_string()
            })
            .collect();

        assert_eq!(
            embedded, on_disk,
            "GENERATED_SKILL_FILES must match every file under plugin/skills/ exactly"
        );
    }

    fn collect_relative(base: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read skills dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_relative(base, &path, out);
            } else if path.is_file() {
                out.insert(
                    path.strip_prefix(base)
                        .expect("under base")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}
