//! Shared plugin bundle registry.
//!
//! The source tree is unified where host formats match; host-specific overlays
//! remain where each installer needs a different manifest, hook, command, or
//! agent format.
//!
//! Layout of `plugin/`:
//! - `plugin/skills/*/SKILL.md` — the shared model-invocable skills (every
//!   `SKILL.md` directory under `plugin/skills/`). All five hosts deploy the
//!   full set; the workflow dispatcher skills were removed (their behavior
//!   lives in the native slash commands below), so no host filters the skill
//!   set today. The `cursor_skill_files` filter is kept as a guard against a
//!   dispatcher skill being reintroduced.
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
//!   dot-dir path). Kimi's manifest also carries its MCP server and hooks
//!   inline (`mcpServers.tracedecay`, `PostToolUse`/`Stop`), so there is no
//!   separate Kimi MCP or hooks file.
//! - `plugin/opencode/{tracedecay.ts,tracedecay-mcp.ts,opencode.registration.json}`
//!   — OpenCode native plugin, MCP companion, and MCP/LSP registration.
//!   OpenCode has no `plugin.json`.
//! - `plugin/.mcp.json` — shared Claude/Codex MCP config (byte-identical);
//!   `plugin/mcp-cursor.json` — Cursor MCP config (deploys to `mcp.json`).
//! - `plugin/README-<host>.md` — per-host README (Claude/Cursor/Codex/Kimi
//!   deploy to `README.md`; OpenCode's README is source documentation).
//!
//! Composed per-host view = `GENERATED_SKILL_FILES` (recursively embedded from
//! `plugin/skills/`, filtered per host) ∪ `<HOST>_MANIFEST_FILES` and extras.

use tracedecay_domain::errors::Result;

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
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "plugin MCP config is missing mcpServers object".to_string(),
        })?;
    let key = if servers.contains_key("tracedecay") {
        "tracedecay"
    } else if servers.contains_key("graph") {
        "graph"
    } else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "plugin MCP config must declare mcpServers.tracedecay or mcpServers.graph"
                .to_string(),
        });
    };
    servers
        .get_mut(key)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("plugin MCP config is missing mcpServers.{key}"),
        })?
        .as_object_mut()
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("plugin MCP config mcpServers.{key} must be an object"),
        })?
        .insert("command".to_string(), serde_json::json!(bin));
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

/// Hook/plugin template token replaced with the resolved tracedecay binary.
pub(crate) const TRACEDECAY_BIN_PLACEHOLDER: &str = "__TRACEDECAY_BIN__";
/// Hook template token replaced with the host's sync/event hook command.
pub(crate) const TRACEDECAY_SYNC_PLACEHOLDER: &str = "__TRACEDECAY_SYNC__";
/// Hook template token replaced with the host's stop hook command.
pub(crate) const TRACEDECAY_STOP_PLACEHOLDER: &str = "__TRACEDECAY_STOP__";

const TRACEDECAY_COMMAND_PLACEHOLDERS: &[&str] = &[
    TRACEDECAY_BIN_PLACEHOLDER,
    TRACEDECAY_SYNC_PLACEHOLDER,
    TRACEDECAY_STOP_PLACEHOLDER,
];

/// Fail closed when a rendered host file still carries a TraceDecay placeholder.
///
/// Claude, Cursor, OpenCode, and Gemini used to substitute and ship; only Kimi
/// rejected leftovers. One residual check keeps an unresolved token from
/// reaching a host config.
pub(crate) fn reject_unresolved_placeholders(rendered: &str, host: &str) -> Result<()> {
    if TRACEDECAY_COMMAND_PLACEHOLDERS
        .iter()
        .any(|placeholder| rendered.contains(*placeholder))
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("{host} retained an unresolved TraceDecay placeholder"),
        });
    }
    Ok(())
}

/// One embedded plugin file: `relative` is its deploy path; `contents` may come
/// from a different source path in the shared `plugin/` tree. The type is owned
/// by the automation runtime so its `HostIo` bundle can hand these slices
/// through without a per-crate copy.
pub use tracedecay_automation_runtime::automation::host_io::PluginFile;

macro_rules! plugin_file {
    ($relative:literal, $source:literal) => {
        PluginFile {
            relative: $relative,
            contents: include_str!(concat!("../../../../plugin/", $source)),
        }
    };
}

// Every shared skill and canonical/generated agent file, embedded by build.rs
// into an isolated module so Hawk can exclude the OUT_DIR surface.
mod plugin_bundle_generated {
    use super::PluginFile;

    include!(concat!(env!("OUT_DIR"), "/plugin_bundle_generated.rs"));
}
use plugin_bundle_generated::*;

pub(crate) fn codex_agent_files() -> &'static [PluginFile] {
    GENERATED_CODEX_AGENT_FILES
}

/// Prefix of the dispatcher skills that Cursor does **not** deploy (they are
/// native commands on Cursor). Claude/Codex/Kimi/OpenCode deploy every skill.
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
        "cursor-native-extension/embedded/extension.js"
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
/// file under `plugin/skills/` (`SKILL.md` plus support files).
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

/// Files Codex deploys: manifest + every file under `plugin/skills/`
/// (`SKILL.md` plus support files). Codex ships no agents/commands/rules.
/// The host-bundle catalog deploys the rendered variants of this inventory via
/// `agents::codex::rendered_global_plugin_files` — the raw templates here are
/// not directly installable (`hooks/hooks.json` is an empty scaffold).
pub fn codex_files() -> Vec<(&'static str, &'static str)> {
    compose(&[CODEX_MANIFEST_FILES], all_skill_files())
}

/// Files Kimi deploys: manifest + README + the shared Claude command Markdown
/// (Kimi plugin commands use the same frontmatter/`$ARGUMENTS` format, so the
/// shared sources ship verbatim) + every skill file. Hooks live inline in
/// `.kimi-plugin/plugin.json` (`PostToolUse`, `Stop`); Kimi ships no
/// agents/rules and no separate hooks file.
pub fn kimi_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[KIMI_MANIFEST_FILES, CLAUDE_COMMAND_FILES],
        all_skill_files(),
    )
}

/// `OpenCode` Agent component: host-loadable skills, agent definitions, and
/// command prompt templates. `AGENTS.md` remains Core instruction content.
///
/// Agents deploy in the OpenCode-derived form: stock OpenCode validates agent
/// frontmatter against its own schema and rejects the Claude `tools:` string,
/// which would invalidate the host's entire configuration.
pub fn opencode_agent_files() -> Vec<(&'static str, &'static str)> {
    compose(
        &[GENERATED_OPENCODE_AGENT_FILES, CLAUDE_COMMAND_FILES],
        all_skill_files(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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
    fn first_party_plugin_assets_fit_host_bundle_artifact_bound() {
        use tracedecay_host_integration::MAX_ARTIFACT_CONTENT_BYTES;

        for (host, files) in [
            ("claude", claude_files()),
            ("cursor", cursor_files()),
            ("cursor-native", cursor_native_extension_files()),
            ("codex", codex_files()),
            ("kimi", kimi_files()),
            ("opencode-agent", opencode_agent_files()),
        ] {
            for (relative, contents) in files {
                assert!(
                    contents.len() <= MAX_ARTIFACT_CONTENT_BYTES,
                    "{host} {relative} is {} bytes, exceeds MAX_ARTIFACT_CONTENT_BYTES ({MAX_ARTIFACT_CONTENT_BYTES})",
                    contents.len()
                );
            }
        }
    }
}
