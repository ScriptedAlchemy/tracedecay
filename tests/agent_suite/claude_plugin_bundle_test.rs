//! Filesystem validation contract tests for the Claude Code plugin surface of
//! the shared `plugin/` tree.
//!
//! These mirror the sibling bundle tests (`plugin_manifest_schema_test.rs`,
//! `plugin_config_schema_test.rs`, `plugin_skill_contract_test.rs`) but operate
//! purely on the on-disk shared tree, asserting Claude's manifests, MCP config,
//! lifecycle hooks, skills, commands, and agents are shaped correctly and stay
//! in sync with the agent source of truth (`src/agents/claude_agents/`).
//!
//! The embedded-file-list coverage check (asserting a Rust `const` registry
//! matches the on-disk tree) is intentionally omitted here; it is handled with
//! the installer that owns that registry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::plugin_validation_support::{body_after_frontmatter, read_json_file};
use tracedecay::automation::skill_frontmatter::parse_skill_frontmatter;

/// The shared plugin tree root (holds Claude's manifest, skills, commands, and
/// agents; Claude's host-specific files are `README-claude.md`, `.mcp.json`,
/// and `hooks/hooks-claude.json`).
fn bundle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin")
}

/// The 13 model-invocable skills the bundle ships (also the codex skill set),
/// kept in sync across every skill-bundling surface. The `tracedecay-*`
/// workflow dispatcher skills were removed (their behavior lives in the native
/// slash commands), the memory write/read skills were folded into
/// `project-memory`, and `recalling-session-context`/`retrieving-cached-context`
/// were folded into `managing-session-context`/`using-the-cli`.
const EXPECTED_SKILLS: &[&str] = &[
    "assessing-impact",
    "code-health",
    "diagnosing-analytics",
    "editing-safely",
    "exploring-code",
    "fixing-build-and-type-errors",
    "inspecting-managed-skills",
    "managing-session-context",
    "project-memory",
    "reviewing-changes",
    "tracing-functions",
    "using-the-cli",
    "using-tracedecay",
];

/// The 13 slash commands the bundle ships.
const EXPECTED_COMMANDS: &[&str] = &[
    "audit-safety",
    "check-health",
    "clean-dead-code",
    "compare-branches",
    "curate-memory",
    "draft-commit",
    "find-impact",
    "fix-build",
    "map-architecture",
    "port-code",
    "recall-memory",
    "review-diff",
    "test-changes",
];

/// The 3 subagent definitions, byte-identical to `src/agents/claude_agents/`.
const EXPECTED_AGENTS: &[&str] = &[
    "code-explorer.md",
    "code-health-auditor.md",
    "session-historian.md",
];

/// Reads a required scalar frontmatter field from a `---`-fenced markdown file,
/// asserting it is present and non-empty. Mirrors the frontmatter approach in
/// `plugin_skill_contract_test.rs` (manual parse via `parse_skill_frontmatter`,
/// no new YAML dependency).
fn required_scalar(raw: &str, field: &str, path: &Path) -> String {
    let frontmatter = parse_skill_frontmatter(raw)
        .unwrap_or_else(|err| panic!("{}: failed to parse frontmatter: {err}", path.display()));
    let value = frontmatter
        .get(field)
        .unwrap_or_else(|| panic!("{} is missing frontmatter `{field}`", path.display()))
        .as_scalar()
        .unwrap_or_else(|| {
            panic!(
                "{} frontmatter `{field}` must be an inline scalar",
                path.display()
            )
        });
    assert!(
        !value.trim().is_empty(),
        "{} frontmatter `{field}` cannot be empty",
        path.display()
    );
    value.to_string()
}

/// Sorted set of subdirectory names directly under `dir`.
fn sorted_subdir_names(dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_dir())
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("directory name should be utf-8")
                .to_string()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Sorted set of file names directly under `dir` matching `extension`.
fn sorted_file_names(dir: &Path, extension: &str) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == extension)
        })
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("file name should be utf-8")
                .to_string()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn claude_bundle_manifest_declares_the_expected_plugin_metadata() {
    let manifest_path = bundle_root().join(".claude-plugin/plugin.json");
    let manifest = read_json_file(&manifest_path);

    assert_eq!(
        manifest["name"],
        "tracedecay",
        "{} name must be tracedecay",
        manifest_path.display()
    );
    for field in ["version", "description", "license", "homepage"] {
        let value = manifest.get(field).and_then(Value::as_str);
        assert!(
            value.is_some_and(|value| !value.trim().is_empty()),
            "{} must declare a non-empty `{field}`",
            manifest_path.display()
        );
    }
    let author_name = manifest
        .get("author")
        .and_then(|author| author.get("name"))
        .and_then(Value::as_str);
    assert!(
        author_name.is_some_and(|name| !name.trim().is_empty()),
        "{} must declare a non-empty author.name",
        manifest_path.display()
    );
}

#[test]
fn claude_bundle_marketplace_lists_the_tracedecay_plugin() {
    let marketplace_path = bundle_root().join(".claude-plugin/marketplace.json");
    let marketplace = read_json_file(&marketplace_path);

    assert_eq!(
        marketplace["name"],
        "tracedecay",
        "{} name must be tracedecay",
        marketplace_path.display()
    );
    assert!(
        marketplace.get("owner").is_some(),
        "{} must declare an owner",
        marketplace_path.display()
    );

    let plugins = marketplace
        .get("plugins")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "{} must declare a plugins array",
                marketplace_path.display()
            )
        });
    let entry = plugins
        .iter()
        .find(|plugin| plugin.get("name").and_then(Value::as_str) == Some("tracedecay"))
        .unwrap_or_else(|| {
            panic!(
                "{} plugins[] must contain a tracedecay entry",
                marketplace_path.display()
            )
        });
    assert_eq!(
        entry["source"],
        "./",
        "{} tracedecay plugin source must be \"./\"",
        marketplace_path.display()
    );
}

#[test]
fn claude_bundle_mcp_config_declares_the_tracedecay_server() {
    let mcp_path = bundle_root().join(".mcp.json");
    let mcp = read_json_file(&mcp_path);

    // Matches the codex-plugin/.mcp.json shape: mcpServers.tracedecay.
    let server = mcp
        .get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .unwrap_or_else(|| panic!("{} must declare mcpServers.tracedecay", mcp_path.display()));
    assert_eq!(
        server["command"],
        "tracedecay",
        "{} tracedecay server command must be tracedecay",
        mcp_path.display()
    );
}

#[test]
fn claude_bundle_hooks_wire_the_expected_lifecycle_events() {
    let hooks_path = bundle_root().join("hooks/hooks-claude.json");
    let config = read_json_file(&hooks_path);

    let hooks = config
        .get("hooks")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{} must declare a hooks object", hooks_path.display()));

    // (event, expected subcommand, expected matcher). The PostToolUse matcher
    // is derived from the tool lists so the on-disk JSON is validated against
    // the single source of truth and can never silently drift.
    let post_matcher = tracedecay::hooks::claude_post_tool_use_matcher();
    let expected: &[(&str, &str, Option<&str>)] = &[
        ("PreToolUse", "hook-pre-tool-use", Some("Agent")),
        ("UserPromptSubmit", "hook-prompt-submit", None),
        ("Stop", "hook-stop", None),
        ("SessionStart", "hook-claude-session-start", None),
        (
            "PostToolUse",
            "hook-claude-post-tool-use",
            Some(post_matcher.as_str()),
        ),
        ("SubagentStart", "hook-claude-subagent-start", None),
    ];

    let actual_events: BTreeSet<String> = hooks.keys().cloned().collect();
    let expected_events: BTreeSet<String> = expected
        .iter()
        .map(|(event, ..)| event.to_string())
        .collect();
    assert_eq!(
        actual_events,
        expected_events,
        "{} must declare exactly the 6 expected lifecycle events",
        hooks_path.display()
    );

    for (event, subcommand, matcher) in expected {
        let entries = hooks
            .get(*event)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{} {event} must be an array", hooks_path.display()));
        assert_eq!(
            entries.len(),
            1,
            "{} {event} must have exactly one entry",
            hooks_path.display()
        );
        let entry = &entries[0];

        match matcher {
            Some(expected_matcher) => assert_eq!(
                entry.get("matcher").and_then(Value::as_str),
                Some(*expected_matcher),
                "{} {event} matcher must be {expected_matcher}",
                hooks_path.display()
            ),
            None => assert!(
                entry.get("matcher").is_none(),
                "{} {event} must not declare a matcher",
                hooks_path.display()
            ),
        }

        let inner = entry
            .get("hooks")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{} {event} must declare hooks[]", hooks_path.display()));
        assert_eq!(
            inner.len(),
            1,
            "{} {event} must declare exactly one hook",
            hooks_path.display()
        );
        let hook = &inner[0];
        assert_eq!(
            hook["command"],
            "__TRACEDECAY_BIN__",
            "{} {event} hook command must be the __TRACEDECAY_BIN__ placeholder",
            hooks_path.display()
        );
        let args = hook
            .get("args")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{} {event} hook must declare args[]", hooks_path.display()));
        assert_eq!(
            args.len(),
            1,
            "{} {event} hook args must be a single-element array",
            hooks_path.display()
        );
        assert_eq!(
            args[0],
            *subcommand,
            "{} {event} hook subcommand must be {subcommand}",
            hooks_path.display()
        );
    }
}

#[test]
fn claude_bundle_ships_exactly_the_expected_skills() {
    let skills_root = bundle_root().join("skills");
    let mut expected: Vec<String> = EXPECTED_SKILLS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(
        sorted_subdir_names(&skills_root),
        expected,
        "claude-plugin/skills must contain exactly the expected 13 skill directories"
    );
}

#[test]
fn claude_bundle_skills_have_valid_frontmatter_and_body() {
    let skills_root = bundle_root().join("skills");
    for skill in EXPECTED_SKILLS {
        let path = skills_root.join(skill).join("SKILL.md");
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

        let name = required_scalar(&raw, "name", &path);
        assert_eq!(
            &name,
            skill,
            "{} frontmatter name must match its directory",
            path.display()
        );
        required_scalar(&raw, "description", &path);

        assert!(
            !body_after_frontmatter(&raw).trim().is_empty(),
            "{} must have a non-empty body",
            path.display()
        );
    }
}

#[test]
fn claude_bundle_ships_exactly_the_expected_commands() {
    let commands_root = bundle_root().join("commands");
    let mut expected: Vec<String> = EXPECTED_COMMANDS
        .iter()
        .map(|command| format!("{command}.md"))
        .collect();
    expected.sort();
    assert_eq!(
        sorted_file_names(&commands_root, "md"),
        expected,
        "claude-plugin/commands must contain exactly the expected 13 command files"
    );
}

#[test]
fn claude_bundle_commands_have_valid_frontmatter_and_body() {
    let commands_root = bundle_root().join("commands");
    for command in EXPECTED_COMMANDS {
        let path = commands_root.join(format!("{command}.md"));
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

        required_scalar(&raw, "description", &path);
        assert!(
            !body_after_frontmatter(&raw).trim().is_empty(),
            "{} must have a non-empty body",
            path.display()
        );
    }
}

#[test]
fn claude_bundle_agents_are_byte_identical_to_the_source_of_truth() {
    let bundle_agents = bundle_root().join("agents");
    let source_agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents/claude_agents");

    // The bundle ships exactly the expected agent set.
    let mut expected: Vec<String> = EXPECTED_AGENTS.iter().map(|a| a.to_string()).collect();
    expected.sort();
    assert_eq!(
        sorted_file_names(&bundle_agents, "md"),
        expected,
        "claude-plugin/agents must contain exactly the expected agent files"
    );

    for agent in EXPECTED_AGENTS {
        let bundle_path = bundle_agents.join(agent);
        let source_path = source_agents.join(agent);
        let bundle_bytes = fs::read(&bundle_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", bundle_path.display()));
        let source_bytes = fs::read(&source_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
        assert!(
            bundle_bytes == source_bytes,
            "{} must be a byte-identical copy of the single source of truth {}",
            bundle_path.display(),
            source_path.display()
        );

        let raw = String::from_utf8(bundle_bytes)
            .unwrap_or_else(|err| panic!("{} is not utf-8: {err}", bundle_path.display()));
        required_scalar(&raw, "name", &bundle_path);
        required_scalar(&raw, "description", &bundle_path);
    }
}
