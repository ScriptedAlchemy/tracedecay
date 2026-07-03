//! Single-source-of-truth plugin bundle registry.
//!
//! All three host bundles (Claude, Cursor, Codex) used to live as three
//! byte-duplicated trees (`claude-plugin/`, `cursor-plugin/`, `codex-plugin/`)
//! embedded via three separate `include_str!` tables. They now share **one**
//! on-disk tree under `plugin/`, and this module owns the composed per-host
//! view that each installer deploys.
//!
//! Layout of `plugin/`:
//! - `plugin/skills/*/SKILL.md` — the 17 shared model-invocable skills **plus**
//!   the 13 canonical (`claude`/`codex`) workflow dispatchers.
//! - `plugin/overlays/cursor/skills/tracedecay-*/SKILL.md` — the Cursor-only
//!   dispatcher form (`disable-model-invocation: true`, `/slug` H1). Cursor
//!   deploys these **in place of** the canonical dispatcher form, at the same
//!   `skills/tracedecay-*/SKILL.md` deploy path.
//! - `plugin/agents/*.md` — Claude-form subagents (deployed by Claude).
//! - `plugin/overlays/cursor/agents/*.md` — Cursor-form subagents.
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
//! Each [`PluginFile::relative`] is the **deploy-relative** path on disk, kept
//! byte-for-byte identical to the pre-refactor bundles so no host's installed
//! tree changes. The embedded `contents` come from the shared `plugin/` source,
//! whose path may differ from the deploy path (e.g. Cursor's
//! `hooks/hooks.json` is sourced from `plugin/hooks/hooks-cursor.json`).
//!
//! Composed per-host view = `CANONICAL_PLUGIN_FILES ∪ <HOST>_MANIFEST_FILES`.

/// One embedded plugin file: `relative` is its deploy path (unchanged from the
/// legacy per-host bundles), `contents` is embedded from the shared `plugin/`
/// tree at compile time.
#[derive(Clone, Copy)]
pub struct PluginFile {
    pub relative: &'static str,
    pub contents: &'static str,
}

macro_rules! plugin_file {
    ($relative:literal, $source:literal) => {
        PluginFile {
            relative: $relative,
            contents: include_str!(concat!("../../plugin/", $source)),
        }
    };
}

/// The 17 model-invocable skills shared byte-for-byte by every host. These are
/// the "canonical" set every installer deploys unchanged.
pub const CANONICAL_PLUGIN_FILES: &[PluginFile] = &[
    plugin_file!(
        "skills/assessing-impact/SKILL.md",
        "skills/assessing-impact/SKILL.md"
    ),
    plugin_file!("skills/code-health/SKILL.md", "skills/code-health/SKILL.md"),
    plugin_file!(
        "skills/curating-project-memory/SKILL.md",
        "skills/curating-project-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/editing-safely/SKILL.md",
        "skills/editing-safely/SKILL.md"
    ),
    plugin_file!(
        "skills/exploring-code/SKILL.md",
        "skills/exploring-code/SKILL.md"
    ),
    plugin_file!(
        "skills/fixing-build-and-type-errors/SKILL.md",
        "skills/fixing-build-and-type-errors/SKILL.md"
    ),
    plugin_file!(
        "skills/inspecting-managed-skills/SKILL.md",
        "skills/inspecting-managed-skills/SKILL.md"
    ),
    plugin_file!(
        "skills/managing-session-context/SKILL.md",
        "skills/managing-session-context/SKILL.md"
    ),
    plugin_file!(
        "skills/recalling-project-memory/SKILL.md",
        "skills/recalling-project-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/recalling-session-context/SKILL.md",
        "skills/recalling-session-context/SKILL.md"
    ),
    plugin_file!(
        "skills/retrieving-cached-context/SKILL.md",
        "skills/retrieving-cached-context/SKILL.md"
    ),
    plugin_file!(
        "skills/retrieving-project-memory/SKILL.md",
        "skills/retrieving-project-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/reviewing-changes/SKILL.md",
        "skills/reviewing-changes/SKILL.md"
    ),
    plugin_file!(
        "skills/storing-project-memory/SKILL.md",
        "skills/storing-project-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/tracing-functions/SKILL.md",
        "skills/tracing-functions/SKILL.md"
    ),
    plugin_file!(
        "skills/using-the-cli/SKILL.md",
        "skills/using-the-cli/SKILL.md"
    ),
    plugin_file!(
        "skills/using-tracedecay/SKILL.md",
        "skills/using-tracedecay/SKILL.md"
    ),
];

/// The 13 workflow dispatchers in their canonical (`claude`/`codex`)
/// model-invocable form, sourced from `plugin/skills/tracedecay-*`.
const CANONICAL_DISPATCHER_FILES: &[PluginFile] = &[
    plugin_file!(
        "skills/tracedecay-audit-safety/SKILL.md",
        "skills/tracedecay-audit-safety/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-check-health/SKILL.md",
        "skills/tracedecay-check-health/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-clean-dead-code/SKILL.md",
        "skills/tracedecay-clean-dead-code/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-compare-branches/SKILL.md",
        "skills/tracedecay-compare-branches/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-curate-memory/SKILL.md",
        "skills/tracedecay-curate-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-draft-commit/SKILL.md",
        "skills/tracedecay-draft-commit/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-find-impact/SKILL.md",
        "skills/tracedecay-find-impact/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-fix-build/SKILL.md",
        "skills/tracedecay-fix-build/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-map-architecture/SKILL.md",
        "skills/tracedecay-map-architecture/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-port-code/SKILL.md",
        "skills/tracedecay-port-code/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-recall-memory/SKILL.md",
        "skills/tracedecay-recall-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-review-diff/SKILL.md",
        "skills/tracedecay-review-diff/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-test-changes/SKILL.md",
        "skills/tracedecay-test-changes/SKILL.md"
    ),
];

/// Cursor's dispatcher overlay: the same 13 slugs, in Cursor slash-dispatcher
/// form (`disable-model-invocation: true`). Deployed **at the same paths** as
/// the canonical dispatchers, overriding them for Cursor only.
const CURSOR_DISPATCHER_FILES: &[PluginFile] = &[
    plugin_file!(
        "skills/tracedecay-audit-safety/SKILL.md",
        "overlays/cursor/skills/tracedecay-audit-safety/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-check-health/SKILL.md",
        "overlays/cursor/skills/tracedecay-check-health/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-clean-dead-code/SKILL.md",
        "overlays/cursor/skills/tracedecay-clean-dead-code/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-compare-branches/SKILL.md",
        "overlays/cursor/skills/tracedecay-compare-branches/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-curate-memory/SKILL.md",
        "overlays/cursor/skills/tracedecay-curate-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-draft-commit/SKILL.md",
        "overlays/cursor/skills/tracedecay-draft-commit/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-find-impact/SKILL.md",
        "overlays/cursor/skills/tracedecay-find-impact/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-fix-build/SKILL.md",
        "overlays/cursor/skills/tracedecay-fix-build/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-map-architecture/SKILL.md",
        "overlays/cursor/skills/tracedecay-map-architecture/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-port-code/SKILL.md",
        "overlays/cursor/skills/tracedecay-port-code/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-recall-memory/SKILL.md",
        "overlays/cursor/skills/tracedecay-recall-memory/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-review-diff/SKILL.md",
        "overlays/cursor/skills/tracedecay-review-diff/SKILL.md"
    ),
    plugin_file!(
        "skills/tracedecay-test-changes/SKILL.md",
        "overlays/cursor/skills/tracedecay-test-changes/SKILL.md"
    ),
];

/// Claude-form subagents.
const CLAUDE_AGENT_FILES: &[PluginFile] = &[
    plugin_file!("agents/code-explorer.md", "agents/code-explorer.md"),
    plugin_file!(
        "agents/code-health-auditor.md",
        "agents/code-health-auditor.md"
    ),
    plugin_file!("agents/session-historian.md", "agents/session-historian.md"),
];

/// Cursor-form subagents.
const CURSOR_AGENT_FILES: &[PluginFile] = &[
    plugin_file!(
        "agents/code-explorer.md",
        "overlays/cursor/agents/code-explorer.md"
    ),
    plugin_file!(
        "agents/code-health-auditor.md",
        "overlays/cursor/agents/code-health-auditor.md"
    ),
    plugin_file!(
        "agents/session-historian.md",
        "overlays/cursor/agents/session-historian.md"
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

/// Compose a host's full deploy set as `(relative, contents)` tuples, in a
/// deterministic order matching the legacy per-host embed tables' shape.
///
/// Order mirrors what each installer historically iterated: manifest/mcp/hooks
/// pieces first, then the shared skills, dispatchers, and host extras. Exact
/// ordering does not affect the deployed tree (each file is written by its
/// deploy `relative` path), but a stable order keeps tests deterministic.
fn compose(sections: &[&[PluginFile]]) -> Vec<(&'static str, &'static str)> {
    sections
        .iter()
        .flat_map(|section| section.iter())
        .map(|file| (file.relative, file.contents))
        .collect()
}

/// Files Claude deploys: manifest + canonical skills + canonical dispatchers +
/// Claude agents + Claude commands.
pub fn claude_files() -> Vec<(&'static str, &'static str)> {
    compose(&[
        CLAUDE_MANIFEST_FILES,
        CLAUDE_AGENT_FILES,
        CLAUDE_COMMAND_FILES,
        CANONICAL_PLUGIN_FILES,
        CANONICAL_DISPATCHER_FILES,
    ])
}

/// Files Cursor deploys: manifest + canonical skills + Cursor dispatcher
/// overlay + Cursor agents + Cursor rules.
pub fn cursor_files() -> Vec<(&'static str, &'static str)> {
    compose(&[
        CURSOR_MANIFEST_FILES,
        CURSOR_RULE_FILES,
        CURSOR_AGENT_FILES,
        CANONICAL_PLUGIN_FILES,
        CURSOR_DISPATCHER_FILES,
    ])
}

/// Files Codex deploys: manifest + canonical skills + canonical dispatchers.
/// Codex ships no agents, commands, or rules.
pub fn codex_files() -> Vec<(&'static str, &'static str)> {
    compose(&[
        CODEX_MANIFEST_FILES,
        CANONICAL_PLUGIN_FILES,
        CANONICAL_DISPATCHER_FILES,
    ])
}

#[cfg(test)]
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

    #[test]
    fn each_host_deploys_unique_relative_paths() {
        assert_unique_relatives(&claude_files(), "claude");
        assert_unique_relatives(&cursor_files(), "cursor");
        assert_unique_relatives(&codex_files(), "codex");
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
        // 17 canonical skills + 13 dispatchers = 30 skills, common to all hosts.
        // Claude: 30 skills + 5 manifest (2 dot + mcp + hooks + README) + 3
        //   agents + 13 commands = 51.
        assert_eq!(claude_files().len(), 51);
        // Cursor: 30 skills + 4 manifest (dot + mcp + hooks + README) + 2 rules
        //   + 3 agents = 39.
        assert_eq!(cursor_files().len(), 39);
        // Codex: 30 skills + 4 manifest (dot + mcp + hooks + README) = 34.
        assert_eq!(codex_files().len(), 34);
    }

    /// Every model-invocable canonical skill maps to an on-disk source dir.
    #[test]
    fn canonical_skills_have_source_dirs() {
        let root = plugin_source_root();
        for file in CANONICAL_PLUGIN_FILES {
            assert!(
                root.join(file.relative).exists(),
                "canonical source missing: plugin/{}",
                file.relative
            );
        }
    }
}
