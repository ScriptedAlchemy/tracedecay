//! Cursor-specific lint for the composed Cursor skill set (the 17 shared
//! model-invocable skills from `plugin/skills/`) plus the Cursor native slash
//! commands (`plugin/overlays/cursor/commands/`), ported from
//! community/official skill linters so enforcement runs offline inside
//! `cargo test` (no node/python CI dependency).
//!
//! Rule sources:
//! - skillmark (<https://github.com/michellepellon/skillmark>): broken file
//!   references (E031), BOM/structural hygiene (E032-E034), angle brackets in
//!   frontmatter values (E036), reserved name prefixes (E037), short
//!   descriptions (W003), placeholder text (W006), heading presence (W009).
//! - skilldoctor (<https://github.com/studiomeyer-io/skilldoctor>): empty
//!   body, trailing whitespace.
//! - skillkit (<https://github.com/sakhilchawla/skillkit>): skipped heading
//!   levels, consistent structure.
//! - Cursor docs (<https://cursor.com/docs/skills>): `paths` glob scoping;
//!   native slash commands (<https://cursor.com/docs/commands>) whose `/slug`
//!   title matches the command file name.
//!
//! Repo-specific reference-integrity rules (same spirit as skillmark E031,
//! applied to this bundle's conventions): `tracedecay:<skill>` cross-skill
//! references, backticked `/skill` invocations, and `tracedecay_*` MCP tool
//! mentions must all resolve against the bundle / the live MCP tool list.
//!
//! The generic per-file intersection contract — frontmatter whitelist,
//! name/folder match, description budgets/trigger/uniqueness, one plain-title
//! H1 / heading levels / no `## When to Use`, the 500-line cap, LF hygiene,
//! placeholder + reserved-prefix checks, and resource-dir layout — now lives
//! once in `tests/agent_suite/shared_skill_contract_test.rs` over the single
//! `plugin/skills/` tree. The receipt-backed lifecycle suite owns install
//! byte-parity + host-extra frontmatter + metadata budgets. This file keeps
//! only the Cursor-specific reference-integrity checks (skill/tool/link
//! resolution and `paths` glob scoping) plus the native-command lint.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use regex::Regex;
use tracedecay::automation::skill_frontmatter::{SkillFrontmatterValue, parse_skill_frontmatter};
use tracedecay::mcp::get_tool_definitions;

use crate::plugin_validation_support::{load_skill_docs_from, repo_path};
use tempfile::TempDir;

/// Cursor's native slash commands (the 13 `tracedecay-*` workflow commands).
const CURSOR_COMMAND_ROOT: &str = "plugin/overlays/cursor/commands";

/// Stages the Cursor skill *source* set into a temp dir: the 17 shared
/// model-invocable skills from `plugin/skills/` (all non-`tracedecay-*` slugs).
/// This is exactly the skill set Cursor deploys — the `tracedecay-*` workflow
/// slugs are native commands there (see [`command_slugs`]), not skills.
fn staged_cursor_skills() -> TempDir {
    let staged = TempDir::new().expect("temp cursor skill source");
    let shared = repo_path("plugin/skills");
    for entry in std::fs::read_dir(&shared).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().unwrap().is_dir() || name.starts_with("tracedecay-") {
            continue;
        }
        copy_dir(&entry.path(), &staged.path().join(&name));
    }
    staged
}

/// The `/slug` names Cursor exposes as native commands (the file stems under
/// `plugin/overlays/cursor/commands/`). Backticked `/slug` references in skill
/// or command bodies resolve against this set.
fn command_slugs() -> BTreeSet<String> {
    std::fs::read_dir(repo_path(CURSOR_COMMAND_ROOT))
        .expect("cursor commands dir readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect()
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `tracedecay_*` identifiers that are documented output artifacts or tool-family
/// globs, not single MCP tools (skills tell agents to report the
/// `tracedecay_metrics:` line, and may reference the `tracedecay_lcm_*` family).
const NON_TOOL_IDENTIFIERS: &[&str] = &["tracedecay_metrics", "tracedecay_lcm"];

#[test]
fn cursor_skill_references_resolve() {
    let staged = staged_cursor_skills();
    let skills = load_skill_docs_from(staged.path());
    let skill_names: BTreeSet<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    let command_names = command_slugs();
    let tool_names = mcp_tool_names();
    let mut violations = Vec::new();

    let link_re = Regex::new(r"\[[^\]]*\]\(([^)]+)\)").unwrap();
    let resource_re =
        Regex::new(r"\b(?:agents|scripts|references|assets)/[A-Za-z0-9][A-Za-z0-9._/-]*").unwrap();
    let skill_ref_re = Regex::new(r"tracedecay:([a-z0-9][a-z0-9-]*)").unwrap();
    let slash_ref_re = Regex::new(r"`/([a-z0-9][a-z0-9-]*)`").unwrap();
    let tool_ref_re = Regex::new(r"tracedecay_[a-z_]+").unwrap();
    let mut skill_refs_seen = 0usize;
    let mut tool_refs_seen = 0usize;

    for skill in &skills {
        let at = skill.path.display();
        let skill_dir = skill.path.parent().expect("skill path has parent");

        // skillmark E031: relative markdown link targets must exist.
        for capture in link_re.captures_iter(&skill.raw) {
            let target = capture[1].trim();
            let target = target.split_once(' ').map_or(target, |(path, _title)| path);
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
                || target.starts_with('#')
            {
                continue;
            }
            if target.starts_with('/') {
                violations.push(format!("{at}: link target {target:?} is an absolute path"));
            } else if !skill_dir.join(target.split('#').next().unwrap()).exists() {
                violations.push(format!("{at}: broken relative link {target:?}"));
            }
        }

        // skillmark W024 inverse: a mentioned bundled-resource path
        // (scripts/x.sh, references/y.md, ...) must actually be shipped.
        for found in resource_re.find_iter(&skill.body) {
            let mentioned = found.as_str().trim_end_matches(['.', ',', ';', ':']);
            if !skill_dir.join(mentioned).exists() {
                violations.push(format!(
                    "{at}: mentions bundled resource {mentioned:?} which does not exist"
                ));
            }
        }

        // Bundle convention: `tracedecay:<skill>` hands off to another
        // bundled skill; a stale slug strands the agent mid-workflow.
        for capture in skill_ref_re.captures_iter(&skill.raw) {
            skill_refs_seen += 1;
            let slug = &capture[1];
            if !skill_names.contains(slug) {
                violations.push(format!(
                    "{at}: references skill tracedecay:{slug} which is not bundled"
                ));
            }
        }

        // Cursor docs: `/name` invokes a native command; a backticked slash
        // reference must resolve to a bundled command.
        for capture in slash_ref_re.captures_iter(&skill.raw) {
            let slug = capture[1].to_string();
            if !command_names.contains(&slug) {
                violations.push(format!(
                    "{at}: references slash command /{slug} which is not a bundled command"
                ));
            }
        }

        // Stale tool references: every `tracedecay_*` identifier must be a
        // live MCP tool (or a documented non-tool artifact).
        for found in tool_ref_re.find_iter(&skill.raw) {
            tool_refs_seen += 1;
            let identifier = found.as_str().trim_end_matches('_');
            if !tool_names.contains(identifier) && !NON_TOOL_IDENTIFIERS.contains(&identifier) {
                violations.push(format!(
                    "{at}: mentions MCP tool {identifier} which the server does not define"
                ));
            }
        }

        // Cursor docs scope `paths` globs to workspace-relative matching;
        // absolute paths and parent escapes can never match.
        if let Some(SkillFrontmatterValue::Block(_)) = skill.frontmatter.get("paths") {
            let globs = skill.frontmatter["paths"]
                .as_list_items()
                .unwrap_or_default();
            for glob in &globs {
                if glob.starts_with('/') || glob.contains('\\') || glob.contains("..") {
                    violations.push(format!(
                        "{at}: paths glob {glob:?} must be a relative forward-slash glob"
                    ));
                }
            }
        }
    }

    // Self-check: the bundle is known to cross-reference skills and mention
    // MCP tools heavily; zero matches would mean the extraction regexes
    // rotted and the rules above passed vacuously.
    assert!(
        skill_refs_seen > 0 && tool_refs_seen > 0,
        "reference extraction found no tracedecay:<skill> or tracedecay_<tool> mentions; \
         the lint regexes are broken"
    );
    assert_no_violations("reference integrity", &violations);
}

/// The Cursor native slash commands (`plugin/overlays/cursor/commands/*.md`)
/// must be LF-clean, open with a `# /<slug>` H1 that matches the file name, and
/// only reference bundled skills and live MCP tools. This is the command-side
/// analogue of the retired dispatcher-skill slash lint.
#[test]
fn cursor_commands_are_hygienic_and_reference_resolve() {
    let command_dir = repo_path(CURSOR_COMMAND_ROOT);
    let staged = staged_cursor_skills();
    let skill_names: BTreeSet<String> = load_skill_docs_from(staged.path())
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    let tool_names = mcp_tool_names();
    let skill_ref_re = Regex::new(r"tracedecay:([a-z0-9][a-z0-9-]*)").unwrap();
    let tool_ref_re = Regex::new(r"tracedecay_[a-z_]+").unwrap();
    let mut violations = Vec::new();
    let mut command_count = 0usize;

    let mut entries: Vec<_> = std::fs::read_dir(&command_dir)
        .expect("cursor commands dir readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    entries.sort();

    for path in entries {
        command_count += 1;
        let at = path.display();
        let slug = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("command file stem")
            .to_string();
        let raw = std::fs::read_to_string(&path).expect("read command");
        let frontmatter = match parse_skill_frontmatter(&raw) {
            Ok(frontmatter) => frontmatter,
            Err(error) => {
                violations.push(format!(
                    "{at}: command file missing valid YAML frontmatter: {error}"
                ));
                continue;
            }
        };
        match frontmatter.get("name").and_then(|value| value.as_scalar()) {
            Some(name) if name == slug => {}
            Some(name) => violations.push(format!(
                "{at}: frontmatter name {name:?} must match command slug {slug:?}"
            )),
            None => violations.push(format!(
                "{at}: command frontmatter must include scalar `name`"
            )),
        }
        match frontmatter
            .get("description")
            .and_then(|value| value.as_scalar())
        {
            Some(description) if !description.trim().is_empty() => {}
            Some(_) => violations.push(format!(
                "{at}: command frontmatter `description` must not be empty"
            )),
            None => violations.push(format!(
                "{at}: command frontmatter must include scalar `description`"
            )),
        }

        if raw.contains('\r') {
            violations.push(format!("{at}: contains CRLF line endings"));
        }
        if !raw.ends_with('\n') {
            violations.push(format!("{at}: missing trailing newline"));
        }
        if raw.ends_with("\n\n") {
            violations.push(format!("{at}: ends with blank lines"));
        }
        for (idx, line) in raw.lines().enumerate() {
            if line.ends_with(' ') || line.ends_with('\t') || line.contains('\t') {
                violations.push(format!("{at}:{}: trailing whitespace or tab", idx + 1));
            }
        }

        // The command body opens with a `# /<slug>` H1 matching the file name,
        // so the documented invocation is the one Cursor exposes.
        let h1 = raw
            .lines()
            .find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").trim());
        match h1 {
            Some(title) if title == format!("/{slug}") => {}
            Some(title) => violations.push(format!(
                "{at}: H1 {title:?} must be the slash form `/{slug}`"
            )),
            None => violations.push(format!("{at}: command body must open with an H1 title")),
        }

        for capture in skill_ref_re.captures_iter(&raw) {
            let referenced = capture[1].to_string();
            if !skill_names.contains(&referenced) {
                violations.push(format!(
                    "{at}: references skill tracedecay:{referenced} which is not bundled"
                ));
            }
        }
        for found in tool_ref_re.find_iter(&raw) {
            let identifier = found.as_str().trim_end_matches('_');
            if !tool_names.contains(identifier) && !NON_TOOL_IDENTIFIERS.contains(&identifier) {
                violations.push(format!(
                    "{at}: mentions MCP tool {identifier} which the server does not define"
                ));
            }
        }
    }

    assert_eq!(
        command_count, 13,
        "expected 13 Cursor native slash commands, found {command_count}"
    );
    assert_no_violations("cursor command integrity", &violations);
}

fn mcp_tool_names() -> BTreeSet<String> {
    let mut names = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .map(|definition| definition.name)
        .collect::<BTreeSet<_>>();
    // Host-gated: filtered out of the definition list when the `ast-grep`
    // CLI is absent, but still a real tool skills may reference.
    names.insert("tracedecay_ast_grep_rewrite".to_string());
    names
}

fn assert_no_violations(rule_family: &str, violations: &[String]) {
    assert!(
        violations.is_empty(),
        "cursor skill lint ({rule_family}) found {} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}
