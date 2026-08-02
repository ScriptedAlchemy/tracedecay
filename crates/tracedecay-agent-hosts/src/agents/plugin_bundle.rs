//! Shared plugin bundle registry.
//!
//! The source tree is unified where host formats match; host-specific overlays
//! remain where each installer needs a different manifest, hook, command, or
//! agent format.
//!
//! Layout of `plugin/`:
//! - `plugin/skills/*/SKILL.md` — the 15 shared model-invocable skills. All
//!   four hosts deploy the full set; the workflow dispatcher skills were
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
//!   `plugin/.cursor-plugin/plugin.json`, `plugin/.codex-plugin/plugin.json`,
//!   `plugin/.kimi-plugin/plugin.json` — host manifests (deploy to the same
//!   dot-dir path). Kimi's manifest also carries its MCP server inline
//!   (`mcpServers.tracedecay`), so there is no separate Kimi MCP config file.
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
    manifest["version"] = serde_json::json!(crate::PRODUCT_VERSION);
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
/// - Kimi uses `tracedecay` and embeds `mcpServers` inline in its manifest,
///   so the installer rewrites the command on the manifest itself.
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
const CURSOR_RULE_FILES: &[PluginFile] =
    &[plugin_file!("rules/tracedecay.mdc", "rules/tracedecay.mdc")];

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

/// Claude's one configured-language LSP bridge. It is part of the MCP-free
/// core bundle and is deployed separately from the compatibility manifest
/// inventory so existing aggregate installers keep their stable file set.
pub const CLAUDE_LSP_FILES: &[PluginFile] = &[plugin_file!(".lsp.json", ".lsp.json")];

/// Cursor manifest + Cursor MCP + Cursor hooks + README.
pub const CURSOR_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(".cursor-plugin/plugin.json", ".cursor-plugin/plugin.json"),
    plugin_file!("README.md", "README-cursor.md"),
    plugin_file!("mcp.json", "mcp-cursor.json"),
    plugin_file!("hooks/hooks.json", "hooks/hooks-cursor.json"),
];

/// Cursor's unpacked desktop extension. The host-component lifecycle deploys
/// these assets to Cursor's extension root rather than the plugin root.
const CURSOR_NATIVE_EXTENSION_FILES: &[PluginFile] = &[
    plugin_file!("package.json", "cursor-native-extension/package.json"),
    plugin_file!(
        "dist/extension.js",
        "cursor-native-extension/dist/extension.js"
    ),
    plugin_file!("README.md", "cursor-native-extension/README.md"),
    plugin_file!("LICENSE", "cursor-native-extension/LICENSE"),
];

/// Codex manifest + shared MCP + Codex hooks + README.
pub const CODEX_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(".codex-plugin/plugin.json", ".codex-plugin/plugin.json"),
    plugin_file!(".mcp.json", ".mcp.json"),
    plugin_file!("README.md", "README-codex.md"),
    plugin_file!("hooks/hooks.json", "hooks/hooks-codex.json"),
];

/// Kimi manifest + README. The manifest embeds `mcpServers.tracedecay`
/// inline, so Kimi needs no separate MCP config file.
pub const KIMI_MANIFEST_FILES: &[PluginFile] = &[
    plugin_file!(".kimi-plugin/plugin.json", ".kimi-plugin/plugin.json"),
    plugin_file!("README.md", "README-kimi.md"),
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

/// MCP-free Claude core: plugin metadata, hooks, skills, agents, commands, and
/// the single configured-language `TraceDecay` LSP bridge.
pub fn claude_core_files() -> Vec<(&'static str, &'static str)> {
    claude_files()
        .into_iter()
        .filter(|(relative, _)| *relative != ".mcp.json")
        .chain(
            CLAUDE_LSP_FILES
                .iter()
                .map(|file| (file.relative, file.contents)),
        )
        .collect()
}

/// Independently installable Claude MCP companion inventory.
pub fn claude_mcp_companion_files() -> Vec<(&'static str, &'static str)> {
    CLAUDE_MANIFEST_FILES
        .iter()
        .filter(|file| file.relative == ".mcp.json")
        .map(|file| (file.relative, file.contents))
        .collect()
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

/// Unpacked VS Code/Cursor extension files for the native-diagnostics host
/// component. Its bundle includes `vscode-languageclient` and leaves only the
/// host-provided `vscode` module external.
pub fn cursor_native_extension_files() -> Vec<(&'static str, &'static str)> {
    CURSOR_NATIVE_EXTENSION_FILES
        .iter()
        .map(|file| (file.relative, file.contents))
        .collect()
}

/// Files Codex deploys: manifest + every skill file (all 30 skills incl.
/// dispatchers, plus any support files). Codex ships no agents/commands/rules.
/// The host-bundle catalog deploys the rendered variants of this inventory via
/// `agents::codex::rendered_global_plugin_files` — the raw templates here are
/// not directly installable (`hooks/hooks.json` is an empty scaffold).
pub fn codex_files() -> Vec<(&'static str, &'static str)> {
    compose(&[CODEX_MANIFEST_FILES], all_skill_files())
}

/// Files Kimi deploys: manifest + README + the shared Claude command Markdown
/// (Kimi plugin commands use the same frontmatter/`$ARGUMENTS` format, so the
/// shared sources ship verbatim) + every skill file. Kimi ships no
/// agents/rules/hooks in v1.
pub fn kimi_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[KIMI_MANIFEST_FILES, CLAUDE_COMMAND_FILES],
        all_skill_files(),
    )
}

/// `OpenCode` Agent component: host-loadable skills, agent definitions, and
/// command prompt templates. `AGENTS.md` remains Core instruction content.
pub fn opencode_agent_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[GENERATED_CLAUDE_AGENT_FILES, CLAUDE_COMMAND_FILES],
        all_skill_files(),
    )
}

pub fn opencode_mcp_companion_files() -> Vec<(&'static str, &'static str)> {
    vec![(
        "tracedecay-mcp.ts",
        include_str!("../../../../plugin/opencode/tracedecay-mcp.ts"),
    )]
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
            .map_or_else(
                || panic!("shared skill should be embedded: {relative}"),
                |file| file.contents,
            )
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
        assert_unique_relatives(&kimi_files(), "kimi");
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
        for host in [claude_files(), cursor_files(), codex_files(), kimi_files()] {
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
        //   README) + rules + native agents + 13 native commands.
        // Memory lives in ~/.cursor/rules/, not the plugin inventory.
        assert_eq!(
            cursor_files().len(),
            cursor_skills + 4 + CURSOR_RULE_FILES.len() + GENERATED_CURSOR_AGENT_FILES.len() + 13
        );
        // Codex: skills + 4 manifest (dot + mcp + hooks + README).
        assert_eq!(codex_files().len(), all_skills + 4);
        // Kimi: skills + 2 manifest (dot + README) + 13 shared commands.
        assert_eq!(kimi_files().len(), all_skills + 2 + 13);
    }

    #[test]
    fn kimi_manifest_declares_identity_and_inline_mcp_server() {
        let manifest = kimi_files()
            .into_iter()
            .find(|(relative, _)| *relative == ".kimi-plugin/plugin.json")
            .map(|(_, contents)| contents)
            .expect("kimi deploy set must include .kimi-plugin/plugin.json");
        let parsed: serde_json::Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(parsed["name"], "tracedecay");
        assert_eq!(parsed["version"], "0.0.0");
        let server = &parsed["mcpServers"]["tracedecay"];
        assert!(
            server.is_object(),
            "kimi manifest must declare mcpServers.tracedecay"
        );
        assert_eq!(server["command"], "tracedecay");
        assert_eq!(server["args"], serde_json::json!(["serve"]));
        assert!(
            parsed["mcpServers"].get("graph").is_none(),
            "kimi manifest must not declare mcpServers.graph"
        );
    }

    #[test]
    fn kimi_files_ship_every_skill_command_and_the_readme() {
        let files = kimi_files();

        // Every embedded skill file deploys under its shared skills/ path.
        for skill in GENERATED_SKILL_FILES {
            assert!(
                files
                    .iter()
                    .any(|(relative, _)| *relative == skill.relative),
                "kimi deploy set is missing {}",
                skill.relative
            );
        }

        // The shared Claude commands deploy verbatim under commands/.
        for command in CLAUDE_COMMAND_FILES {
            let deployed = files
                .iter()
                .find(|(relative, _)| *relative == command.relative)
                .map_or_else(
                    || panic!("kimi deploy set is missing {}", command.relative),
                    |(_, contents)| *contents,
                );
            assert_eq!(
                deployed, command.contents,
                "kimi must ship {} verbatim",
                command.relative
            );
        }

        // The Kimi README deploys as README.md.
        let readme = files
            .iter()
            .find(|(relative, _)| *relative == "README.md")
            .map(|(_, contents)| *contents)
            .expect("kimi deploy set must include README.md");
        assert!(
            readme.contains("# TraceDecay Kimi Code Plugin"),
            "kimi README.md must come from README-kimi.md"
        );
    }

    /// The installer helpers operate on the manifest directly: the version
    /// stamp rewrites `version`, and `set_mcp_command` rewrites the inline
    /// `mcpServers.tracedecay.command`.
    #[test]
    fn kimi_manifest_round_trips_through_installer_rewrites() {
        let raw = KIMI_MANIFEST_FILES
            .iter()
            .find(|file| file.relative == ".kimi-plugin/plugin.json")
            .map(|file| file.contents)
            .expect("kimi manifest must be embedded");

        let stamped = stamp_manifest_version(raw).unwrap();
        let stamped: serde_json::Value = serde_json::from_str(&stamped).unwrap();
        assert_eq!(stamped["version"], crate::PRODUCT_VERSION);

        let rewired = set_mcp_command(raw, "/abs/tracedecay").unwrap();
        let rewired: serde_json::Value = serde_json::from_str(&rewired).unwrap();
        assert_eq!(
            rewired["mcpServers"]["tracedecay"]["command"],
            "/abs/tracedecay"
        );
    }

    #[test]
    fn capability_discovery_skill_is_shared_and_matches_plugin_source() {
        let relative = "skills/discovering-tracedecay/SKILL.md";
        let source = embedded_skill(relative);
        let on_disk = std::fs::read_to_string(plugin_source_root().join(relative))
            .expect("discovering-tracedecay skill must exist on disk");
        assert_eq!(
            source, on_disk,
            "embedded discovering-tracedecay skill must match plugin source"
        );

        for (host, files) in [
            ("claude", claude_files()),
            ("cursor", cursor_files()),
            ("codex", codex_files()),
        ] {
            let deployed = files
                .iter()
                .find(|(path, _)| *path == relative)
                .map_or_else(
                    || panic!("{host} is missing {relative}"),
                    |(_, contents)| *contents,
                );
            assert_eq!(
                deployed, source,
                "{host} must ship the shared source verbatim"
            );
        }
    }

    #[test]
    fn using_cli_skill_matches_plugin_source() {
        let relative = "skills/using-the-cli/SKILL.md";
        let source = embedded_skill(relative);
        let on_disk = std::fs::read_to_string(plugin_source_root().join(relative))
            .expect("using-the-cli skill must exist on disk");
        assert_eq!(
            source, on_disk,
            "embedded using-the-cli skill must match plugin source"
        );
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
