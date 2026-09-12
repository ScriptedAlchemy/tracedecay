//! Claude agent permissions and the generated Cursor/Codex agent adapters.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::plugin_validation_support::body_after_frontmatter;
use tracedecay_automation_runtime::automation::skill_frontmatter::parse_skill_frontmatter;

/// Reads a required scalar frontmatter field from a `---`-fenced markdown file,
/// asserting it is present and non-empty through the production parser.
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

/// Claude agents use a positive allowlist. A denylist would fail open whenever
/// a new mutating MCP tool ships.
#[test]
fn claude_agents_allow_only_live_read_only_mcp_tools() {
    const DIRECT_PREFIX: &str = "mcp__tracedecay__";
    const PLUGIN_PREFIX: &str = "mcp__plugin_tracedecay_graph__";

    // `read_only_tool_names()` reads the catalog straight from the crate that
    // owns it, so no composition-root registration precedes this and an
    // unavailable catalog is an error rather than an empty allowlist.
    let live_read_only: BTreeSet<String> = tracedecay_agent_hosts::agents::read_only_tool_names()
        .expect("the advertised tool catalog")
        .into_iter()
        .collect();
    assert!(
        !live_read_only.is_empty(),
        "the advertised catalog must expose read-only tools; an empty set would \
         pass every allowlist vacuously"
    );
    for (relative, raw) in tracedecay_agent_hosts::agents::plugin_bundle::claude_files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("agents/"))
    {
        let path = Path::new(relative);

        let tools = required_scalar(raw, "tools", path);
        let tool_entries: Vec<&str> = tools.split(',').map(str::trim).collect();
        for required in ["Read", "Grep", "Glob", "ToolSearch"] {
            assert!(
                tool_entries.contains(&required),
                "{} tools allowlist must grant {required}; got: {tools}",
                path.display()
            );
        }
        for entry in &tool_entries {
            assert!(
                ["Read", "Grep", "Glob", "ToolSearch"].contains(entry)
                    || entry.starts_with(DIRECT_PREFIX)
                    || entry.starts_with(PLUGIN_PREFIX),
                "{} grants unexpected tool {entry}",
                path.display()
            );
        }
        assert!(
            !tool_entries.contains(&"Bash"),
            "{} grants Bash",
            path.display()
        );
        assert!(
            !tool_entries
                .iter()
                .any(|entry| matches!(*entry, "mcp__tracedecay" | "mcp__plugin_tracedecay_graph")),
            "{} grants a server-wide MCP wildcard",
            path.display()
        );

        let direct: BTreeSet<&str> = tool_entries
            .iter()
            .filter_map(|entry| entry.strip_prefix(DIRECT_PREFIX))
            .collect();
        let plugin: BTreeSet<&str> = tool_entries
            .iter()
            .filter_map(|entry| entry.strip_prefix(PLUGIN_PREFIX))
            .collect();
        assert!(
            !direct.is_empty(),
            "{} grants no TraceDecay tools",
            path.display()
        );
        assert_eq!(
            direct,
            plugin,
            "{} namespace grants drifted",
            path.display()
        );
        for tool in direct {
            assert!(
                live_read_only.contains(tool),
                "{} grants {tool}, but its live readOnlyHint is not true",
                path.display()
            );
        }
        assert!(
            !parse_skill_frontmatter(raw)
                .unwrap()
                .contains_key("disallowedTools"),
            "{} must not rely on a finite mutator denylist",
            path.display()
        );
    }
}

#[test]
fn cursor_and_codex_agents_are_generated_from_the_canonical_catalog() {
    let cursor_files = tracedecay_agent_hosts::agents::plugin_bundle::cursor_files();
    let temp = tempfile::tempdir().unwrap();
    tracedecay_automation_runtime::automation::agent_targets::install_codex_managed_agents(
        &tracedecay_agent_hosts::host_io(),
        temp.path(),
    )
    .unwrap();
    for (relative, claude) in tracedecay_agent_hosts::agents::plugin_bundle::claude_files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("agents/"))
    {
        let claude_path = Path::new(relative);
        let agent = relative.strip_prefix("agents/").unwrap();
        let stem = agent.strip_suffix(".md").unwrap();
        let cursor = cursor_files
            .iter()
            .find(|(path, _)| *path == format!("agents/{agent}"))
            .map_or_else(
                || panic!("missing generated Cursor adapter for {agent}"),
                |(_, contents)| *contents,
            );
        let codex_path = temp
            .path()
            .join(".codex/agents")
            .join(format!("tracedecay-{stem}.toml"));
        let codex = fs::read_to_string(&codex_path).unwrap();
        let codex_toml: toml::Value = toml::from_str(&codex).unwrap();

        assert_eq!(required_scalar(cursor, "name", Path::new(agent)), stem);
        assert_eq!(
            required_scalar(cursor, "description", Path::new(agent)),
            required_scalar(claude, "description", claude_path)
        );
        let canonical_body = body_after_frontmatter(claude).replace("\r\n", "\n");
        assert_eq!(
            body_after_frontmatter(cursor).replace("\r\n", "\n"),
            canonical_body
        );
        assert_eq!(
            required_scalar(cursor, "readonly", Path::new(agent)),
            "true"
        );
        let expected_codex_name = format!("tracedecay-{stem}");
        assert_eq!(
            codex_toml["name"].as_str(),
            Some(expected_codex_name.as_str())
        );
        assert_eq!(
            codex_toml["description"].as_str(),
            Some(required_scalar(claude, "description", claude_path).as_str())
        );
        assert_eq!(codex_toml["sandbox_mode"].as_str(), Some("read-only"));
        assert_eq!(
            codex_toml["developer_instructions"].as_str().map(str::trim),
            Some(canonical_body.trim())
        );
    }
}
