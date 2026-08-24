//! Claude Code / Agent Skills portability lint for the shared skill collection
//! (`plugin/skills/`).
//!
//! These tests keep the shared skills close to Claude Code's documented skill
//! rules so a Claude bundle can reuse them without a rewrite. The Cursor
//! workflow slugs are native `commands/` on Cursor now (not
//! `disable-model-invocation` skills), so the whole shared skill set is
//! strictly Agent-Skills-spec conformant.
//!
//! Rule sources (fetched 2026-07-02):
//! - Claude Code skills reference (frontmatter field table, 1,536-char
//!   listing cap, command-name rules): <https://code.claude.com/docs/en/skills>
//! - Agent Skills open specification (allowed fields, name/description
//!   constraints): <https://agentskills.io/specification>
//! - Claude platform Agent Skills docs (64-char name, 1,024-char description,
//!   no XML tags, reserved words "anthropic"/"claude"):
//!   <https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview>
//! - Anthropic skill-creator validator (`quick_validate.py`: kebab-case name,
//!   no angle brackets in description, allowed-field whitelist):
//!   <https://github.com/anthropics/skills>
//! - Ground-truth layouts from Anthropic's published plugins
//!   (`frontend-design`, `mcp-server-dev`): `.claude-plugin/plugin.json` plus
//!   `skills/<name>/SKILL.md`, with `license` and `version` frontmatter in
//!   shipping skills.
//!
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tracedecay_agent_hosts::automation::skill_frontmatter::SkillFrontmatterValue;

use crate::plugin_validation_support::{SkillDoc, is_kebab_case_skill_name, load_skill_docs};

/// The full shared skill surface: the 29 canonical model-invocable skills
/// (`plugin/skills`) that every host ships. Cursor's workflow slugs are native
/// commands, not skills, so there is no dispatcher overlay left to lint here.
const SKILL_ROOTS: &[&str] = &["plugin/skills"];

/// Frontmatter fields Claude Code recognizes, per the field table at
/// code.claude.com/docs/en/skills, plus the Agent Skills open-spec fields
/// (`license`, `compatibility`, `metadata`) and `version`, which Anthropic's
/// own published plugin skills ship (e.g. mcp-server-dev's build-mcp-server).
const CLAUDE_CODE_ALLOWED_FRONTMATTER: &[&str] = &[
    "agent",
    "allowed-tools",
    "argument-hint",
    "arguments",
    "compatibility",
    "context",
    "description",
    "disable-model-invocation",
    "disallowed-tools",
    "effort",
    "hooks",
    "license",
    "metadata",
    "model",
    "name",
    "paths",
    "shell",
    "user-invocable",
    "version",
    "when_to_use",
];

/// Frontmatter fields the strict Agent Skills open spec allows
/// (agentskills.io/specification), which is also the whitelist enforced by
/// Anthropic's skill-creator `quick_validate.py` packaging validator.
const AGENT_SKILLS_SPEC_ALLOWED_FRONTMATTER: &[&str] = &[
    "allowed-tools",
    "compatibility",
    "description",
    "license",
    "metadata",
    "name",
];

/// platform.claude.com: skill names uploaded to the Claude API cannot contain
/// the reserved words "anthropic" or "claude".
const CLAUDE_RESERVED_NAME_WORDS: &[&str] = &["anthropic", "claude"];

/// Claude platform limit: description must be 1-1024 characters.
const CLAUDE_MAX_DESCRIPTION_CHARS: usize = 1024;
/// Claude platform limit: name must be at most 64 characters.
const CLAUDE_MAX_NAME_CHARS: usize = 64;
/// Claude Code truncates the combined `description` + `when_to_use` text at
/// 1,536 characters in the skill listing; staying under means no data loss.
const CLAUDE_CODE_LISTING_CAP_CHARS: usize = 1536;

#[test]
fn bundled_skills_use_only_frontmatter_claude_code_recognizes() {
    for skill in load_all_skill_docs() {
        let unknown = skill
            .frontmatter
            .keys()
            .filter(|key| !CLAUDE_CODE_ALLOWED_FRONTMATTER.contains(&key.as_str()))
            .collect::<Vec<_>>();
        assert!(
            unknown.is_empty(),
            "{} uses frontmatter keys {unknown:?} that Claude Code does not document; \
             allowed keys are {CLAUDE_CODE_ALLOWED_FRONTMATTER:?}",
            skill.path.display()
        );
    }
}

#[test]
fn bundled_skill_names_satisfy_claude_naming_rules() {
    for skill in load_all_skill_docs() {
        let name = required_scalar(&skill, "name");

        // Claude Code derives the /command from the directory name and the
        // open spec requires `name` to match it, so both must conform.
        assert_eq!(
            name,
            skill.name,
            "{} frontmatter name must match its directory so the Claude Code \
             command name and the spec-required name agree",
            skill.path.display()
        );
        assert!(
            name.len() <= CLAUDE_MAX_NAME_CHARS,
            "{} name is {} chars; Claude allows at most {CLAUDE_MAX_NAME_CHARS}",
            skill.path.display(),
            name.len()
        );
        assert!(
            is_kebab_case_skill_name(name),
            "{} name must be lowercase letters, digits, and hyphens without \
             leading/trailing/consecutive hyphens",
            skill.path.display()
        );
        assert!(
            !name.contains(['<', '>']),
            "{} name cannot contain XML tags",
            skill.path.display()
        );
        for reserved in CLAUDE_RESERVED_NAME_WORDS {
            assert!(
                !name.contains(reserved),
                "{} name contains reserved word {reserved:?}, which the Claude \
                 platform rejects",
                skill.path.display()
            );
        }
    }
}

#[test]
fn bundled_skill_descriptions_satisfy_claude_description_rules() {
    for skill in load_all_skill_docs() {
        let description = required_scalar(&skill, "description");

        assert!(
            !description.trim().is_empty(),
            "{} description must be non-empty",
            skill.path.display()
        );
        assert!(
            description.len() <= CLAUDE_MAX_DESCRIPTION_CHARS,
            "{} description is {} chars; Claude allows at most {CLAUDE_MAX_DESCRIPTION_CHARS}",
            skill.path.display(),
            description.len()
        );
        // quick_validate.py and the Claude platform reject angle brackets
        // anywhere in the description (XML-tag guard).
        assert!(
            !description.contains(['<', '>']),
            "{} description cannot contain angle brackets",
            skill.path.display()
        );

        let when_to_use = skill
            .frontmatter
            .get("when_to_use")
            .and_then(SkillFrontmatterValue::as_scalar)
            .unwrap_or("");
        assert!(
            description.len() + when_to_use.len() <= CLAUDE_CODE_LISTING_CAP_CHARS,
            "{} combined description + when_to_use exceeds Claude Code's \
             {CLAUDE_CODE_LISTING_CAP_CHARS}-char listing cap and would be truncated",
            skill.path.display()
        );
    }
}

/// Strict Agent Skills spec conformance.
///
/// The whole shared skill set must stay strictly Agent-Skills-spec conformant:
/// every frontmatter key must be in the open-spec whitelist. Codex validates
/// with the spec whitelist, so any extra key is a portability regression.
#[test]
fn shared_skills_stay_strictly_agent_skills_spec_conformant() {
    for root in SKILL_ROOTS {
        for skill in load_skill_docs(root) {
            let extras = skill
                .frontmatter
                .keys()
                .filter(|key| !AGENT_SKILLS_SPEC_ALLOWED_FRONTMATTER.contains(&key.as_str()))
                .collect::<Vec<_>>();
            assert!(
                extras.is_empty(),
                "{} must stay strictly Agent-Skills-spec conformant (Codex \
                 validates with the spec whitelist) but uses {extras:?}",
                skill.path.display()
            );
        }
    }
}

/// Claude Code preloads model-invocable skill metadata into its skill listing.
/// Keep the aggregate near the Cursor/Codex contract budget so the listing
/// stays small.
#[test]
fn model_invocable_skill_metadata_fits_a_claude_listing_budget() {
    const MAX_PRELOADED_METADATA_CHARS: usize = 6_000;
    for root in SKILL_ROOTS {
        let total: usize = load_skill_docs(root)
            .iter()
            .filter(|skill| {
                skill
                    .frontmatter
                    .get("disable-model-invocation")
                    .and_then(SkillFrontmatterValue::as_scalar)
                    != Some("true")
            })
            .map(|skill| {
                required_scalar(skill, "name").len() + required_scalar(skill, "description").len()
            })
            .sum();
        assert!(
            total <= MAX_PRELOADED_METADATA_CHARS,
            "{root} model-invocable skill metadata totals {total} chars; keep it \
             under {MAX_PRELOADED_METADATA_CHARS} so a Claude Code skill listing \
             is never truncated"
        );
    }
}

fn load_all_skill_docs() -> Vec<SkillDoc> {
    let mut skills = Vec::new();
    for root in SKILL_ROOTS {
        skills.extend(load_skill_docs(root));
    }
    skills
}

fn required_scalar<'a>(skill: &'a SkillDoc, field: &str) -> &'a str {
    skill
        .frontmatter
        .get(field)
        .and_then(SkillFrontmatterValue::as_scalar)
        .unwrap_or_else(|| {
            panic!(
                "{} is missing required scalar frontmatter field {field}",
                skill.path.display()
            )
        })
}
