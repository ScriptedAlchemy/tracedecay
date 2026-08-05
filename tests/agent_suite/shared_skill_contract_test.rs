//! Intersection contract for the single shared `plugin/skills/` tree.
//!
//! Host-specific install parity stays in the receipt-backed lifecycle suite; this
//! file owns per-skill frontmatter, body, hygiene, and support-file rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use tracedecay::automation::skill_frontmatter::SkillFrontmatterValue;

use crate::plugin_validation_support::{
    SkillDoc, is_kebab_case_skill_name, load_skill_docs, relative_files_under, repo_path,
};

const SHARED_SKILL_ROOT: &str = "plugin/skills";
const CURSOR_COMMAND_ROOT: &str = "plugin/overlays/cursor/commands";

const INTERSECTION_FRONTMATTER: &[&str] = &[
    "allowed-tools",
    "description",
    "license",
    "metadata",
    "name",
];

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
        if level <= 6
            && let Some(text) = line[level..].strip_prefix(' ')
        {
            headings.push((level, text.trim().to_string()));
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
        if let Some(title) = h1s.first()
            && title.starts_with('/')
        {
            violations.push(format!(
                "{at}: model-invocable skill must use a plain-title H1, not {title:?}"
            ));
        }

        match skill.body.lines().find(|line| !line.trim().is_empty()) {
            Some(first) if first.starts_with("# ") => {}
            Some(first) => violations.push(format!(
                "{at}: body must open with a plain-title H1 (`# …`), found {first:?}"
            )),
            None => {} // empty-body already flagged by the hygiene test
        }

        let mut prev = 0usize;
        for (level, text) in &headings {
            if prev > 0 && *level > prev + 1 {
                violations.push(format!("{at}: heading {text:?} skips h{prev}→h{level}"));
            }
            prev = *level;
        }

        if skill.raw.to_ascii_lowercase().contains("\n## when to use") {
            violations.push(format!(
                "{at}: body must not carry a `## When to Use` section"
            ));
        }

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

#[test]
fn generated_cursor_agents_are_present_and_clean() {
    let agents: BTreeMap<&str, &str> = tracedecay::agents::plugin_bundle::cursor_files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("agents/"))
        .collect();
    for expected in [
        "automation-auditor.md",
        "change-risk-reviewer.md",
        "code-explorer.md",
        "code-health-auditor.md",
        "cross-host-integration-auditor.md",
        "runtime-storage-doctor.md",
        "session-historian.md",
        "usage-intelligence-analyst.md",
    ] {
        assert!(
            agents.contains_key(format!("agents/{expected}").as_str()),
            "generated Cursor agents missing {expected}"
        );
    }
    let mut violations = Vec::new();
    for (file, raw) in agents {
        if raw.contains('\r') || !raw.ends_with('\n') {
            violations.push(format!("{}: line-ending hygiene", file));
        }
        if file == "agents/session-historian.md" && raw.contains("after_store_id") {
            violations.push(format!(
                "{file}: must teach only opaque session continuation cursors"
            ));
        }
    }
    assert_no_violations("generated cursor agents", &violations);
}
