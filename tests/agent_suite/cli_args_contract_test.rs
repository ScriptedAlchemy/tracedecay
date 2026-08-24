//! Taught-model ↔ parser contract for `tracedecay tool` arguments.
//!
//! The `using-the-cli` skill and its arg catalog must teach the JSON-first
//! contract (`--args` carries the MCP arguments object, `--args -` reads a
//! heredoc from stdin) and must not document flags the tool schemas do not
//! accept, because the validation gate rejects unknown keys.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::plugin_validation_support::repo_path;

fn read_repo_file(relative: &str) -> String {
    let path = repo_path(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn using_the_cli_skill_teaches_json_first_with_heredoc() {
    let skill = read_repo_file("plugin/skills/using-the-cli/SKILL.md");
    assert!(
        skill.contains("--args -") && skill.contains("<<'JSON'"),
        "using-the-cli must show the canonical `--args -` heredoc form"
    );
    assert!(
        skill.contains("MCP `arguments` object"),
        "using-the-cli must state the arguments are the MCP arguments object"
    );
    assert!(
        skill.contains("--dry-run"),
        "using-the-cli must document --dry-run pre-flighting"
    );
}

#[test]
fn managed_skill_guidance_matches_automatic_activation() {
    let inspecting = read_repo_file("plugin/skills/inspecting-managed-skills/SKILL.md");
    let cycles = read_repo_file(".claude/skills/inspecting-automation-cycles/SKILL.md");

    let inspecting_lower = inspecting.to_ascii_lowercase();
    for behavior in ["validat", "activat", "deploy", "automatic"] {
        assert!(
            inspecting_lower.contains(behavior),
            "bundled guidance must explain automatic validated skill {behavior} behavior"
        );
    }
    assert!(
        !inspecting_lower.contains("automation skills install")
            && !inspecting_lower.contains("automation skills approve"),
        "bundled guidance must not hand users removed install or approval commands"
    );
    for line in inspecting.lines().filter(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("approval") || line.contains("approve")
    }) {
        assert!(
            line.to_ascii_lowercase().contains("hermes"),
            "approval guidance is valid only for the separate Hermes-owned lifecycle: {line}"
        );
    }

    let cycles_lower = cycles.to_ascii_lowercase();
    assert!(
        cycles_lower.contains("active managed-skill") && cycles_lower.contains("usage"),
        "cycle inspection must direct agents to active-skill adoption evidence"
    );
    for line in cycles.lines().filter(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("approval")
            || line.contains("approve")
            || line.contains("review queue")
            || line.contains("--state pending")
    }) {
        assert!(
            line.to_ascii_lowercase().contains("hermes"),
            "TraceDecay-managed skills have no review queue; only Hermes may retain approval guidance: {line}"
        );
    }
}

#[test]
fn arg_catalog_does_not_teach_per_key_replacements() {
    let catalog = read_repo_file("plugin/skills/using-the-cli/references/tool-arg-catalog.md");
    assert!(
        !catalog.contains("--replacements '["),
        "the catalog must not teach inline per-key JSON for multi_str_replace \
         replacements; the canonical form is the --args heredoc"
    );
    assert!(
        catalog.contains("multi_str_replace --args -"),
        "the catalog must show the --args heredoc form for multi_str_replace"
    );
}

/// Every `--flag` the catalog's quick-reference table documents must exist in
/// the named tool's schema. The validation gate now rejects unknown keys, so
/// a stale catalog row would actively teach agents flags that error.
#[test]
fn arg_catalog_table_flags_exist_in_tool_schemas() {
    let catalog = read_repo_file("plugin/skills/using-the-cli/references/tool-arg-catalog.md");
    let defs = tracedecay::mcp::tools::get_tool_definitions().expect("tool definitions");
    let mut violations = Vec::new();

    for line in catalog.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        // Table rows look like: | `tool` (or `a` / `b`) | `--flag`, … | … |
        if cells.len() < 4 || !cells[1].starts_with('`') {
            continue;
        }
        let tools: Vec<String> = cells[1].split('/').filter_map(extract_backticked).collect();
        if tools.is_empty() {
            continue;
        }
        let flags: Vec<String> = cells[2..]
            .iter()
            .flat_map(|cell| flag_names(cell))
            .collect();
        for tool in &tools {
            let full = format!("tracedecay_{tool}");
            let Some(def) = defs.iter().find(|d| d.name == full) else {
                violations.push(format!("catalog documents unknown tool `{tool}`"));
                continue;
            };
            let props = def.input_schema["properties"].as_object();
            for flag in &flags {
                let key = flag.replace('-', "_");
                if !props.is_some_and(|props| props.contains_key(&key)) {
                    violations.push(format!(
                        "catalog documents `--{flag}` for `{tool}`, but the schema has no \
                         `{key}` property — the unknown-key gate would reject it"
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "catalog/schema drift:\n{}",
        violations.join("\n")
    );
}

fn extract_backticked(cell: &str) -> Option<String> {
    let start = cell.find('`')? + 1;
    let end = start + cell[start..].find('`')?;
    Some(cell[start..end].to_string())
}

fn flag_names(cell: &str) -> Vec<String> {
    let mut flags = Vec::new();
    let mut rest = cell;
    while let Some(pos) = rest.find("--") {
        let after = &rest[pos + 2..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !name.is_empty() {
            flags.push(name.clone());
        }
        rest = &after[name.len()..];
    }
    flags
}
