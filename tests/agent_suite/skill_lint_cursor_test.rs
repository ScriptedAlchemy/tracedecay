//! Cursor-specific lint for the composed Cursor skill set (shared
//! `plugin/skills/` + `plugin/overlays/cursor/skills/`) SKILL.md
//! files, ported from community/official skill linters so enforcement runs
//! offline inside `cargo test` (no node/python CI dependency).
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
//! - Cursor docs (<https://cursor.com/docs/skills>): `disable-model-invocation`
//!   slash-command semantics and `paths` glob scoping.
//!
//! Repo-specific reference-integrity rules (same spirit as skillmark E031,
//! applied to this bundle's conventions): `tracedecay:<skill>` cross-skill
//! references, backticked `/skill` invocations, and `tracedecay_*` MCP tool
//! mentions must all resolve against the bundle / the live MCP tool list.
//!
//! `tests/agent_suite/plugin_skill_contract_test.rs` already enforces the
//! frontmatter key whitelist, name/folder match and charset, description
//! budgets and trigger language, the 500-line body cap, resource-dir layout,
//! and install byte-parity. Those rules are intentionally NOT duplicated here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use tracedecay::automation::skill_frontmatter::SkillFrontmatterValue;
use tracedecay::mcp::get_tool_definitions;

use crate::plugin_validation_support::{load_skill_docs_from, repo_path, SkillDoc};
use tempfile::TempDir;

/// Cursor's dispatcher overlay (the 13 `tracedecay-*` slash dispatchers).
const CURSOR_OVERLAY_SKILL_ROOT: &str = "plugin/overlays/cursor/skills";

/// Stages the composed Cursor skill *source* set into a temp dir: the shared
/// model-invocable skills from `plugin/skills/` (all non-`tracedecay-*` slugs)
/// plus the 13 Cursor dispatcher overlays. This is exactly what Cursor deploys
/// — the canonical `tracedecay-*` bodies never reach Cursor.
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
    let overlay = repo_path(CURSOR_OVERLAY_SKILL_ROOT);
    for entry in std::fs::read_dir(&overlay).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        copy_dir(&entry.path(), &staged.path().join(entry.file_name()));
    }
    staged
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

/// skillmark W003 flags descriptions under 50 chars as too short to convey
/// what the skill does and when to trigger it.
const MIN_DESCRIPTION_CHARS: usize = 50;

/// skillmark E037: reserved vendor prefixes a skill name must not claim.
const RESERVED_NAME_PREFIXES: &[&str] = &["claude", "anthropic"];

/// skillmark W006: placeholder fragments that mark unfinished authoring.
/// Plain TODO/FIXME words are deliberately not listed: several bundled skills
/// legitimately discuss TODO/FIXME markers (`tracedecay_todos`).
const PLACEHOLDER_FRAGMENTS: &[&str] = &["{{", "}}", "tktk", "lorem ipsum", "<placeholder"];

/// `tracedecay_*` identifiers that are documented output artifacts, not MCP
/// tools (skills tell agents to report the `tracedecay_metrics:` line).
const NON_TOOL_IDENTIFIERS: &[&str] = &["tracedecay_metrics"];

#[test]
fn cursor_skill_files_are_hygienic() {
    let staged = staged_cursor_skills();
    let skills = load_skill_docs_from(staged.path());
    let mut violations = Vec::new();

    for skill in &skills {
        let at = skill.path.display();
        let bytes = std::fs::read(&skill.path).expect("re-read skill bytes");
        // skillmark E032-E034: structural hygiene (BOM, unclosed frontmatter
        // is already a parse error upstream).
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            violations.push(format!("{at}: starts with a UTF-8 BOM"));
        }
        // The install byte-parity tests copy these files verbatim, so the
        // canonical bundle must be LF-only even though the parser tolerates
        // CRLF checkouts.
        if skill.raw.contains('\r') {
            violations.push(format!("{at}: contains CRLF line endings"));
        }
        if !skill.raw.ends_with('\n') {
            violations.push(format!("{at}: missing trailing newline"));
        }
        if skill.raw.ends_with("\n\n") {
            violations.push(format!("{at}: ends with blank lines"));
        }
        // skilldoctor skill/trailing-whitespace, plus markdown consistency.
        for (idx, line) in skill.raw.lines().enumerate() {
            if line.ends_with(' ') || line.ends_with('\t') {
                violations.push(format!("{at}:{}: trailing whitespace", idx + 1));
            }
            if line.contains('\t') {
                violations.push(format!("{at}:{}: tab character", idx + 1));
            }
        }
        // skilldoctor skill/empty-body.
        if skill.body.trim().is_empty() {
            violations.push(format!("{at}: instruction body is empty"));
        }
        // Unbalanced fences would corrupt every line-oriented rule below and
        // render badly in the skill viewer.
        let fence_lines = skill
            .raw
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        if fence_lines % 2 != 0 {
            violations.push(format!("{at}: unbalanced ``` code fences"));
        }
        for fragment in PLACEHOLDER_FRAGMENTS {
            if skill.raw.to_ascii_lowercase().contains(fragment) {
                violations.push(format!("{at}: contains placeholder text {fragment:?}"));
            }
        }
    }

    assert_no_violations("file hygiene", &violations);
}

#[test]
fn cursor_skill_bodies_follow_heading_conventions() {
    let staged = staged_cursor_skills();
    let skills = load_skill_docs_from(staged.path());
    let mut violations = Vec::new();

    for skill in &skills {
        let at = skill.path.display();
        // skillmark W009 (body must have headings), tightened to the bundle's
        // convention: the body opens with exactly one H1 title.
        match first_content_line(&skill.body) {
            Some(first) if first.starts_with("# ") => {}
            Some(first) => violations.push(format!(
                "{at}: body must open with an H1 title, found {first:?}"
            )),
            None => continue, // empty body reported by the hygiene test
        }

        let headings = unfenced_headings(&skill.body);
        let h1_count = headings.iter().filter(|(level, _)| *level == 1).count();
        if h1_count != 1 {
            violations.push(format!("{at}: expected exactly one H1, found {h1_count}"));
        }

        // skillkit best-practices: no skipped heading levels (h2 -> h4).
        let mut prev_level = 0usize;
        for (level, text) in &headings {
            if prev_level > 0 && *level > prev_level + 1 {
                violations.push(format!(
                    "{at}: heading {text:?} skips from h{prev_level} to h{level}"
                ));
            }
            prev_level = *level;
        }

        // Cursor docs: `disable-model-invocation: true` makes a skill a slash
        // command. The bundle titles those skills `# /<name>`; a slash-form H1
        // on a model-invocable skill (or one naming a different slug) would
        // document an invocation that does not exist.
        if let Some((_, title)) = headings.iter().find(|(level, _)| *level == 1) {
            if let Some(slug) = title.strip_prefix('/') {
                if slug != skill.name {
                    violations.push(format!(
                        "{at}: H1 slash title `/{slug}` does not match skill name {:?}",
                        skill.name
                    ));
                }
                if scalar(skill, "disable-model-invocation") != Some("true") {
                    violations.push(format!(
                        "{at}: slash-form H1 requires disable-model-invocation: true"
                    ));
                }
            }
        }
    }

    assert_no_violations("heading conventions", &violations);
}

#[test]
fn cursor_skill_names_and_descriptions_meet_lint_quality_bar() {
    let staged = staged_cursor_skills();
    let skills = load_skill_docs_from(staged.path());
    let mut violations = Vec::new();
    let mut descriptions_seen: BTreeMap<String, String> = BTreeMap::new();

    for skill in &skills {
        let at = skill.path.display();
        // skillmark E037.
        for prefix in RESERVED_NAME_PREFIXES {
            if skill.name.starts_with(prefix) {
                violations.push(format!("{at}: name uses reserved prefix {prefix:?}"));
            }
        }

        let Some(description) = scalar(skill, "description") else {
            continue; // required-field enforcement lives in the contract test
        };
        // skillmark W003.
        if description.chars().count() < MIN_DESCRIPTION_CHARS {
            violations.push(format!(
                "{at}: description is shorter than {MIN_DESCRIPTION_CHARS} chars"
            ));
        }
        // skillmark E036: the contract test only checks Codex descriptions
        // for angle brackets; Cursor metadata is injected into prompts too.
        if description.contains(['<', '>']) {
            violations.push(format!("{at}: description contains angle brackets"));
        }
        if !description.ends_with(['.', '!', '?']) {
            violations.push(format!(
                "{at}: description must end with terminal punctuation"
            ));
        }
        // Duplicate descriptions make model routing between skills ambiguous
        // (the agent picks skills from metadata alone).
        if let Some(other) = descriptions_seen.insert(description.to_string(), skill.name.clone()) {
            violations.push(format!(
                "{at}: description duplicates skill {other:?} exactly"
            ));
        }
    }

    assert_no_violations("name/description quality", &violations);
}

#[test]
fn cursor_skill_references_resolve() {
    let staged = staged_cursor_skills();
    let skills = load_skill_docs_from(staged.path());
    let skill_names: BTreeSet<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
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

        // Cursor docs: `/name` invokes a skill by name; a backticked slash
        // reference must resolve to a bundled skill.
        for capture in slash_ref_re.captures_iter(&skill.raw) {
            let slug = &capture[1];
            if !skill_names.contains(slug) {
                violations.push(format!(
                    "{at}: references slash command /{slug} which is not a bundled skill"
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

fn first_content_line(body: &str) -> Option<&str> {
    body.lines()
        .map(str::trim_end)
        .find(|line| !line.is_empty())
}

/// ATX headings outside code fences, as (level, text-after-hashes).
fn unfenced_headings(body: &str) -> Vec<(usize, String)> {
    let mut in_fence = false;
    let mut headings = Vec::new();
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('#') {
            continue;
        }
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        let rest = &line[level..];
        if level <= 6 {
            if let Some(text) = rest.strip_prefix(' ') {
                headings.push((level, text.trim().to_string()));
            }
        }
    }
    headings
}

fn scalar<'a>(skill: &'a SkillDoc, field: &str) -> Option<&'a str> {
    skill
        .frontmatter
        .get(field)
        .and_then(SkillFrontmatterValue::as_scalar)
}

fn mcp_tool_names() -> BTreeSet<String> {
    let mut names = get_tool_definitions()
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
