//! Unified contract for the single shared `plugin/skills/` tree.
//!
//! Since the three host bundles collapsed into one `plugin/` tree, there is one
//! model-invocable skill set that every host (Claude, Codex, Cursor) ships
//! byte-identically. This test validates that one set against the **intersection
//! contract** — the rules a SKILL.md must satisfy to install cleanly on *all*
//! three hosts — plus each host's extra allowances. It supersedes the shared
//! frontmatter/description/heading/hygiene checks that previously lived split
//! across `plugin_skill_contract_test.rs` and `skill_lint_cursor_test.rs`.
//!
//! Covered here (the intersection contract, over `plugin/skills/`):
//! - Frontmatter keys ⊆ {name, description, allowed-tools, license, metadata};
//!   `name` matches the directory and is kebab-case; `name`/`description`
//!   required and non-empty.
//! - `description`: 50–320 chars, ≤45 words, trigger-first ("Use …"), ends with
//!   a period, no angle brackets, unique across the set.
//! - Body: exactly one plain-title H1 (never the slash form), no skipped
//!   heading levels, no `## When to Use` section, ≤500 lines.
//! - Hygiene: no BOM, LF-only, exactly one trailing newline, no trailing
//!   whitespace/tabs, balanced code fences, non-empty body.
//! - Support-file layout: only SKILL.md + scripts/references/assets/agents.
//!
//! Also validated **separately** (host-extra surfaces):
//! - Cursor native commands (`plugin/overlays/cursor/commands/*.md`): a
//!   `# /<slug>` H1 matching the file name, and hygiene.
//! - Cursor agent overlay (`plugin/overlays/cursor/agents/*.md`): present and
//!   hygienic.
//! - Host-extra frontmatter: Codex is spec-strict (intersection only); Cursor
//!   additionally tolerates `disable-model-invocation` / `paths` (none are used
//!   in the shared set today, but the allowance is asserted so a future
//!   Cursor-only key does not silently pass the strict intersection).
//!
//! Install-time byte-parity (`generated_*_plugin_skills_are_byte_copies_*`) and
//! the metadata/openai.yaml budgets stay in `plugin_skill_contract_test.rs`;
//! this file owns the pure per-file contract over the single source tree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use tracedecay::automation::skill_frontmatter::SkillFrontmatterValue;

use crate::plugin_validation_support::{
    is_kebab_case_skill_name, load_skill_docs, relative_files_under, repo_path, SkillDoc,
};

/// The one shared model-invocable skill tree every host ships.
const SHARED_SKILL_ROOT: &str = "plugin/skills";
/// Cursor native slash commands (the 13 `tracedecay-*` workflow slugs).
const CURSOR_COMMAND_ROOT: &str = "plugin/overlays/cursor/commands";
/// Cursor agent overlay.
const CURSOR_AGENT_OVERLAY_ROOT: &str = "plugin/overlays/cursor/agents";

/// The intersection frontmatter whitelist: the keys accepted by *every* host's
/// validator (Codex `quick_validate.py` ∩ Cursor ∩ Claude Agent Skills spec).
const INTERSECTION_FRONTMATTER: &[&str] = &[
    "allowed-tools",
    "description",
    "license",
    "metadata",
    "name",
];

/// Cursor tolerates two extra keys on top of the intersection. None are used in
/// the shared set today (workflow dispatch is native commands), but the
/// allowance is documented so the strict intersection check below can point at
/// it if a Cursor-only key ever appears.
const CURSOR_EXTRA_FRONTMATTER: &[&str] = &["disable-model-invocation", "paths"];

const MIN_DESCRIPTION_CHARS: usize = 50;
const MAX_DESCRIPTION_CHARS: usize = 320;
const MAX_DESCRIPTION_WORDS: usize = 45;
const MAX_SKILL_MD_LINES: usize = 500;

/// skillmark E037: reserved vendor prefixes a skill name must not claim.
const RESERVED_NAME_PREFIXES: &[&str] = &["claude", "anthropic"];

/// skillmark W006: placeholder fragments that mark unfinished authoring.
const PLACEHOLDER_FRAGMENTS: &[&str] = &["{{", "}}", "tktk", "lorem ipsum", "<placeholder"];

fn assert_no_violations(rule_family: &str, violations: &[String]) {
    assert!(
        violations.is_empty(),
        "shared skill contract ({rule_family}) found {} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

fn scalar<'a>(skill: &'a SkillDoc, field: &str) -> Option<&'a str> {
    skill
        .frontmatter
        .get(field)
        .and_then(SkillFrontmatterValue::as_scalar)
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
        if level <= 6 {
            if let Some(text) = line[level..].strip_prefix(' ') {
                headings.push((level, text.trim().to_string()));
            }
        }
    }
    headings
}

#[test]
fn shared_skills_pass_the_intersection_frontmatter_contract() {
    let skills = load_skill_docs(SHARED_SKILL_ROOT);
    assert!(!skills.is_empty(), "expected shared skills");
    let mut violations = Vec::new();

    for skill in &skills {
        let at = skill.path.display();
        // Frontmatter keys ⊆ the intersection whitelist. A Cursor-only key
        // (disable-model-invocation / paths) would break Codex/Claude, so the
        // shared set must not carry it.
        for key in skill.frontmatter.keys() {
            if !INTERSECTION_FRONTMATTER.contains(&key.as_str()) {
                let hint = if CURSOR_EXTRA_FRONTMATTER.contains(&key.as_str()) {
                    " (Cursor-only key; move this surface to plugin/overlays/cursor/commands/)"
                } else {
                    ""
                };
                violations.push(format!(
                    "{at}: frontmatter key {key:?} is outside the intersection whitelist \
                     {INTERSECTION_FRONTMATTER:?}{hint}"
                ));
            }
        }

        // name required, matches dir, kebab-case, ≤64 chars.
        match scalar(skill, "name") {
            None => violations.push(format!("{at}: missing name")),
            Some(name) => {
                if name != skill.name {
                    violations.push(format!(
                        "{at}: name {name:?} must match folder {:?}",
                        skill.name
                    ));
                }
                if !is_kebab_case_skill_name(name) {
                    violations.push(format!("{at}: name {name:?} must be kebab-case"));
                }
                if name.len() > 64 {
                    violations.push(format!("{at}: name exceeds 64 chars"));
                }
                for prefix in RESERVED_NAME_PREFIXES {
                    if name.starts_with(prefix) {
                        violations.push(format!("{at}: name uses reserved prefix {prefix:?}"));
                    }
                }
            }
        }
        if scalar(skill, "description").is_none() {
            violations.push(format!("{at}: missing description"));
        }
    }
    assert_no_violations("frontmatter", &violations);
}

#[test]
fn shared_skill_descriptions_meet_the_intersection_budget() {
    let skills = load_skill_docs(SHARED_SKILL_ROOT);
    let mut violations = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();

    for skill in &skills {
        let at = skill.path.display();
        let Some(description) = scalar(skill, "description") else {
            continue; // missing-description already flagged
        };
        let chars = description.chars().count();
        if chars < MIN_DESCRIPTION_CHARS {
            violations.push(format!(
                "{at}: description under {MIN_DESCRIPTION_CHARS} chars"
            ));
        }
        if chars > MAX_DESCRIPTION_CHARS {
            violations.push(format!(
                "{at}: description over {MAX_DESCRIPTION_CHARS} chars"
            ));
        }
        if description.split_whitespace().count() > MAX_DESCRIPTION_WORDS {
            violations.push(format!(
                "{at}: description over {MAX_DESCRIPTION_WORDS} words"
            ));
        }
        // Trigger-first: agents route on metadata alone, so a "Use …" trigger
        // must lead or follow a short capability summary.
        if !(description.starts_with("Use ") || description.contains(". Use ")) {
            violations.push(format!(
                "{at}: description must be trigger-first (\"Use …\")"
            ));
        }
        if !description.ends_with('.') {
            violations.push(format!("{at}: description must end with a period"));
        }
        if description.contains(['<', '>']) {
            violations.push(format!("{at}: description contains angle brackets"));
        }
        if let Some(other) = seen.insert(description.to_string(), skill.name.clone()) {
            violations.push(format!("{at}: description duplicates skill {other:?}"));
        }
    }
    assert_no_violations("description budget", &violations);
}

#[test]
fn shared_skill_bodies_follow_the_intersection_body_rules() {
    let skills = load_skill_docs(SHARED_SKILL_ROOT);
    let mut violations = Vec::new();

    for skill in &skills {
        let at = skill.path.display();
        let headings = unfenced_headings(&skill.body);

        // Exactly one H1, plain-title form (never `# /slug`).
        let h1s: Vec<&String> = headings
            .iter()
            .filter(|(level, _)| *level == 1)
            .map(|(_, text)| text)
            .collect();
        if h1s.len() != 1 {
            violations.push(format!(
                "{at}: expected exactly one H1, found {}",
                h1s.len()
            ));
        }
        if let Some(title) = h1s.first() {
            if title.starts_with('/') {
                violations.push(format!(
                    "{at}: model-invocable skill must use a plain-title H1, not {title:?}"
                ));
            }
        }

        // The first content line after the frontmatter must be that H1: a
        // single plain-title H1 opens the body (restores the retired
        // `cursor_skill_bodies_follow_heading_conventions` check).
        match skill.body.lines().find(|line| !line.trim().is_empty()) {
            Some(first) if first.starts_with("# ") => {}
            Some(first) => violations.push(format!(
                "{at}: body must open with a plain-title H1 (`# …`), found {first:?}"
            )),
            None => {} // empty-body already flagged by the hygiene test
        }

        // No skipped heading levels.
        let mut prev = 0usize;
        for (level, text) in &headings {
            if prev > 0 && *level > prev + 1 {
                violations.push(format!("{at}: heading {text:?} skips h{prev}→h{level}"));
            }
            prev = *level;
        }

        // Trigger lives in the description, never a body `## When to Use`.
        if skill.raw.to_ascii_lowercase().contains("\n## when to use") {
            violations.push(format!(
                "{at}: body must not carry a `## When to Use` section"
            ));
        }

        // ≤500 lines.
        let lines = skill.raw.lines().count();
        if lines > MAX_SKILL_MD_LINES {
            violations.push(format!("{at}: {lines} lines exceeds {MAX_SKILL_MD_LINES}"));
        }
    }
    assert_no_violations("body rules", &violations);
}

#[test]
fn shared_skill_files_are_hygienic_and_use_supported_layout() {
    let skills = load_skill_docs(SHARED_SKILL_ROOT);
    let allowed_resource_dirs = ["agents", "scripts", "references", "assets"];
    let mut violations = Vec::new();

    for skill in &skills {
        let at = skill.path.display();
        let bytes = std::fs::read(&skill.path).expect("re-read skill bytes");
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            violations.push(format!("{at}: starts with a UTF-8 BOM"));
        }
        if skill.raw.contains('\r') {
            violations.push(format!("{at}: contains CRLF line endings"));
        }
        if !skill.raw.ends_with('\n') {
            violations.push(format!("{at}: missing trailing newline"));
        }
        if skill.raw.ends_with("\n\n") {
            violations.push(format!("{at}: ends with blank lines"));
        }
        if skill.body.trim().is_empty() {
            violations.push(format!("{at}: instruction body is empty"));
        }
        for (idx, line) in skill.raw.lines().enumerate() {
            if line.ends_with(' ') || line.ends_with('\t') || line.contains('\t') {
                violations.push(format!("{at}:{}: trailing whitespace or tab", idx + 1));
            }
        }
        let fences = skill
            .raw
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        if fences % 2 != 0 {
            violations.push(format!("{at}: unbalanced ``` code fences"));
        }
        for fragment in PLACEHOLDER_FRAGMENTS {
            if skill.raw.to_ascii_lowercase().contains(fragment) {
                violations.push(format!("{at}: contains placeholder text {fragment:?}"));
            }
        }

        // Support-file layout: only SKILL.md + the allowed resource dirs, and
        // no auxiliary documentation files (keep skill folders lean).
        let forbidden_doc_files = [
            "README.md",
            "CHANGELOG.md",
            "INSTALLATION_GUIDE.md",
            "QUICK_REFERENCE.md",
        ];
        let skill_dir = skill.path.parent().expect("skill path has parent");
        for relative in relative_files_under(skill_dir) {
            let first = relative
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .expect("relative component");
            let file_name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skill file name should be utf-8");
            if forbidden_doc_files.contains(&file_name) {
                violations.push(format!(
                    "{}: auxiliary documentation file {file_name} not allowed",
                    skill_dir.display()
                ));
            }
            if relative != std::path::Path::new("SKILL.md")
                && !allowed_resource_dirs.contains(&first)
            {
                violations.push(format!(
                    "{}: unsupported top-level entry {}",
                    skill_dir.display(),
                    relative.display()
                ));
            }
        }
    }
    assert_no_violations("hygiene + layout", &violations);
}

/// Cursor native commands are a separate surface from the shared skills: they
/// carry the slash-form H1 the model-invocable skills must NOT use.
#[test]
fn cursor_native_commands_are_hygienic_slash_commands() {
    let command_dir = repo_path(CURSOR_COMMAND_ROOT);
    let mut entries: Vec<_> = std::fs::read_dir(&command_dir)
        .expect("cursor commands dir readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    entries.sort();
    assert_eq!(entries.len(), 13, "expected 13 cursor native commands");

    let mut violations = Vec::new();
    for path in entries {
        let at = path.display();
        let slug = path.file_stem().and_then(|s| s.to_str()).expect("stem");
        let raw = std::fs::read_to_string(&path).expect("read command");
        if raw.contains('\r') {
            violations.push(format!("{at}: CRLF line endings"));
        }
        if !raw.ends_with('\n') || raw.ends_with("\n\n") {
            violations.push(format!("{at}: trailing-newline hygiene"));
        }
        let h1 = raw
            .lines()
            .find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").trim());
        match h1 {
            Some(title) if title == format!("/{slug}") => {}
            Some(title) => violations.push(format!(
                "{at}: H1 {title:?} must be the slash form `/{slug}`"
            )),
            None => violations.push(format!("{at}: command must open with an H1 title")),
        }
    }
    assert_no_violations("cursor commands", &violations);
}

/// The Cursor agent overlay is a small separate surface; assert it ships and is
/// LF-clean so a byte-copy install of it stays stable.
#[test]
fn cursor_agent_overlay_is_present_and_clean() {
    let overlay = repo_path(CURSOR_AGENT_OVERLAY_ROOT);
    let files: BTreeSet<String> = std::fs::read_dir(&overlay)
        .expect("cursor agent overlay readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    for expected in [
        "code-explorer.md",
        "code-health-auditor.md",
        "session-historian.md",
    ] {
        assert!(
            files.contains(expected),
            "cursor agent overlay missing {expected}"
        );
    }
    let mut violations = Vec::new();
    for file in &files {
        let raw = std::fs::read_to_string(overlay.join(file)).expect("read overlay agent");
        if raw.contains('\r') || !raw.ends_with('\n') {
            violations.push(format!("{}: line-ending hygiene", file));
        }
    }
    assert_no_violations("cursor agent overlay", &violations);
}
