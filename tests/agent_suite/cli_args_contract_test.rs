//! Taught-model ↔ parser contract for `tracedecay tool` arguments.
//!
//! Every surface an agent can learn the CLI from — session steering, the
//! `using-the-cli` skill, the arg catalog, and prompt rules — must teach the
//! JSON-first contract (`--args` carries the MCP arguments object, `--args -`
//! reads a heredoc from stdin) and must not teach per-key flags for
//! array-of-array parameters the per-key grammar cannot express. These
//! assertions pin the taught text so it cannot silently drift from the
//! parser again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::plugin_validation_support::repo_path;

fn read_repo_file(relative: &str) -> String {
    let path = repo_path(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn cli_fallback_prompt_source() -> String {
    let source = read_repo_file("crates/tracedecay-agent-hosts/src/agents/mod.rs");
    let start = source
        .find("cli_fallback_args_invocation_lit")
        .expect("cli_fallback_args_invocation_lit in agent-hosts");
    let end = source
        .find("pub(crate) const CLI_FALLBACK_PROMPT_RULES")
        .or_else(|| source.find("pub const CLI_FALLBACK_PROMPT_RULES"))
        .expect("CLI_FALLBACK_PROMPT_RULES in agent-hosts");
    source[start..end].to_string()
}

#[test]
fn prompt_rules_teach_the_json_args_contract() {
    // CLI_FALLBACK_PROMPT_RULES is pub(crate); pin its taught text via source.
    let rules = cli_fallback_prompt_source();
    assert!(
        rules.contains("--args"),
        "CLI fallback prompt rules must teach the --args JSON contract"
    );
    assert!(
        rules.contains("JSON arguments object"),
        "CLI fallback prompt rules must state the payload is the MCP arguments object"
    );
    assert!(
        !rules.contains("<name> --key value"),
        "prompt rules must not lead with the per-key grammar"
    );
    let source = read_repo_file("crates/tracedecay-agent-hosts/src/agents/mod.rs");
    assert!(
        source.contains("never invent per-key flags or enum values from memory"),
        "CLI fallback prompt rules must prohibit guessed flags and enum values"
    );
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
    let defs = tracedecay::mcp::tools::get_tool_definitions();
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

#[test]
fn codex_steering_teaches_the_json_args_contract() {
    let steering = read_repo_file("src/hooks/steering.rs");
    assert!(
        steering.contains("CLI_FALLBACK_PROMPT_RULES"),
        "Codex session steering must include the shared CLI fallback prompt rules"
    );
    let rules = cli_fallback_prompt_source();
    assert!(
        rules.contains("--args '<json>'"),
        "Codex session steering must teach the --args JSON contract"
    );
    assert!(
        !rules.contains("<name> --key value"),
        "Codex session steering must not lead with the per-key grammar"
    );
}
