//! Host-specific contract tests for skills installed from the shared
//! `plugin/skills/` tree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use crate::plugin_validation_support::{
    SkillDoc, is_kebab_case_skill_name, load_skill_docs, load_skill_docs_from,
    relative_files_under, repo_path,
};
use tempfile::TempDir;
use tracedecay::agents::{InstallContext, get_integration};

const CODEX_SKILL_ROOT: &str = "plugin/skills";
const REPO_LOCAL_SKILL_ROOT: &str = ".codex/skills";
const MAX_BUNDLED_SKILL_METADATA_CHARS: usize = 6_000;
const CODEX_QUICK_VALIDATE_ALLOWED_FRONTMATTER: &[&str] = &[
    "allowed-tools",
    "description",
    "license",
    "metadata",
    "name",
];
const CURSOR_ALLOWED_FRONTMATTER: &[&str] = &[
    "allowed-tools",
    "description",
    "disable-model-invocation",
    "license",
    "metadata",
    "name",
    "paths",
];

#[test]
fn codex_plugin_skills_match_codex_skill_creator_quick_validate_rules() {
    let skills = load_skill_docs(CODEX_SKILL_ROOT);
    assert!(!skills.is_empty(), "expected bundled Codex skills");

    for skill in &skills {
        assert_codex_quick_validate_equivalent(skill);
    }
}

#[test]
fn generated_codex_plugin_skills_are_byte_copies_of_the_source_bundle() {
    let home = TempDir::new().expect("temp home");
    let _agent_env = crate::common::AgentEnvLock::pin(home.path());
    let codex = get_integration("codex").expect("codex integration");
    codex
        .install(&install_ctx(home.path()))
        .expect("install generated Codex plugin bundle");

    let source_root = repo_path(CODEX_SKILL_ROOT);
    let installed_root = home.path().join("plugins/tracedecay/skills");
    assert!(
        !installed_root
            .join("agent-managed-memory/SKILL.md")
            .exists(),
        "generated Codex plugin install must not add a duplicate memory digest skill"
    );
    assert_eq!(
        skill_dir_names(&installed_root),
        skill_dir_names(&source_root),
        "generated Codex plugin bundle must ship the same bundled skills as the source bundle"
    );
    assert_skill_trees_byte_identical(&source_root, &installed_root);
}

#[test]
fn repo_local_usage_driven_operator_skills_are_not_template_stubs() {
    let skills = load_skill_docs(REPO_LOCAL_SKILL_ROOT);
    for skill_name in [
        "inspecting-automation-cycles",
        "interpreting-tracedecay-diagnostics",
        "self-improving-from-usage-logs",
        "writing-agent-managed-skills",
    ] {
        let skill = skills
            .iter()
            .find(|skill| skill.name == skill_name)
            .unwrap_or_else(|| panic!("missing bundled operator skill {skill_name}"));
        assert!(
            !skill.raw.contains("[TODO"),
            "{} should not contain skill template TODOs",
            skill.path.display()
        );
    }
}

#[test]
fn bundled_skills_do_not_expose_hermes_profile_storage() {
    let skills = load_skill_docs(CODEX_SKILL_ROOT);

    for skill_name in ["inspecting-managed-skills", "managing-session-context"] {
        let skill = skill_named(&skills, skill_name);
        for forbidden in ["hermes_home", "hermes_profile"] {
            assert!(
                !skill.raw.contains(forbidden),
                "{} must not expose removed Hermes profile storage surface {forbidden}",
                skill.path.display()
            );
        }
    }
    let inspecting = skill_named(&skills, "inspecting-managed-skills");
    assert!(inspecting.raw.contains("tracedecay_hermes_skill_bridge"));
    assert!(inspecting.raw.contains("~/.hermes"));
}

#[test]
fn cursor_plugin_skills_match_cursor_skill_contract() {
    let staged = staged_cursor_skill_source();
    let skills = load_skill_docs_from(staged.path());
    assert!(!skills.is_empty(), "expected bundled Cursor skills");

    for skill in &skills {
        assert_cursor_skill_contract(skill);
    }
}

#[test]
fn generated_cursor_plugin_skills_are_byte_copies_of_the_source_bundle() {
    let home = TempDir::new().expect("temp home");
    let _agent_env = crate::common::AgentEnvLock::pin(home.path());
    let cursor = get_integration("cursor").expect("cursor integration");
    cursor
        .install(&install_ctx(home.path()))
        .expect("install generated Cursor plugin bundle");

    let staged = staged_cursor_skill_source();
    let source_root = staged.path();
    let installed_root = home.path().join(".cursor/plugins/local/tracedecay/skills");
    assert_eq!(
        skill_dir_names(&installed_root),
        skill_dir_names(source_root),
        "generated Cursor plugin bundle must ship the same skills as the composed source"
    );
    assert_skill_trees_byte_identical(source_root, &installed_root);
}

#[test]
fn produced_plugin_skills_meet_the_metadata_budget_and_openai_contract() {
    let codex_skills = load_skill_docs(CODEX_SKILL_ROOT);
    let cursor_staged = staged_cursor_skill_source();
    let cursor_skills = load_skill_docs_from(cursor_staged.path());

    assert_metadata_budget("Codex", &codex_skills, |_| true);
    assert_metadata_budget("Cursor model-invoked", &cursor_skills, |skill| {
        !is_cursor_explicit_invoke_only(skill)
    });

    for skill in codex_skills.iter().chain(cursor_skills.iter()) {
        let skill_dir = skill.path.parent().expect("skill path has parent");
        assert_openai_yaml_contract_if_present(skill_dir);
    }
}

fn install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        tracedecay_bin: "/tmp/tracedecay-test-bin".to_string(),
        ..crate::agent_test_support::install_ctx(home, true)
    }
}

fn staged_cursor_skill_source() -> TempDir {
    let staged = TempDir::new().expect("temp cursor skill source");
    let shared = repo_path("plugin/skills");
    for name in skill_dir_names(&shared) {
        if name.starts_with("tracedecay-") {
            continue;
        }
        copy_dir(&shared.join(&name), &staged.path().join(&name));
    }
    staged
}

fn copy_dir(src: &Path, dst: &Path) {
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

fn skill_dir_names(skills_root: &Path) -> Vec<String> {
    skill_dir_names_except(skills_root, &[])
}

fn skill_dir_names_except(skills_root: &Path, excluded: &[&str]) -> Vec<String> {
    let mut names = std::fs::read_dir(skills_root)
        .unwrap_or_else(|err| panic!("failed to read skills at {}: {err}", skills_root.display()))
        .map(|entry| entry.expect("read skill dir entry").path())
        .filter(|path| path.is_dir())
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("skill directory name should be utf-8")
                .to_string()
        })
        .filter(|name| !excluded.contains(&name.as_str()))
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// The installed skills are `include_str!` byte-copies of the source tree
/// (`src/agents/codex.rs` asserts the embedded list covers every source
/// file), so byte-parity subsumes re-running the per-skill contract over the
/// installed copies and additionally catches any install-time mutation.
fn assert_skill_trees_byte_identical(source_root: &Path, installed_root: &Path) {
    assert_skill_trees_byte_identical_except(source_root, installed_root, &[]);
}

fn assert_skill_trees_byte_identical_except(
    source_root: &Path,
    installed_root: &Path,
    excluded: &[&str],
) {
    let source_files = relative_files_under(source_root);
    let installed_files = relative_files_under(installed_root)
        .into_iter()
        .filter(|relative| {
            relative
                .components()
                .next()
                .and_then(|component| match component {
                    std::path::Component::Normal(name) => name.to_str(),
                    _ => None,
                })
                .is_none_or(|name| !excluded.contains(&name))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        installed_files,
        source_files,
        "installed skill tree {} must contain exactly the files of source tree {}",
        installed_root.display(),
        source_root.display()
    );
    for relative in &source_files {
        let read = |root: &Path| {
            let path = root.join(relative);
            std::fs::read(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
        };
        assert!(
            read(installed_root) == read(source_root),
            "installed {} must be a byte-identical copy of the source skill file",
            installed_root.join(relative).display()
        );
    }
}

fn assert_codex_quick_validate_equivalent(skill: &SkillDoc) {
    assert_allowed_frontmatter(skill, CODEX_QUICK_VALIDATE_ALLOWED_FRONTMATTER);
    assert_required_skill_creator_frontmatter(skill);
    let description = required_scalar_field(skill, "description");
    assert!(
        !description.contains(['<', '>']),
        "{} description cannot contain angle brackets",
        skill.path.display()
    );
    assert!(
        description.len() <= 1024,
        "{} description exceeds Codex quick_validate.py's 1024 character limit",
        skill.path.display()
    );
}

fn assert_cursor_skill_contract(skill: &SkillDoc) {
    assert_allowed_frontmatter(skill, CURSOR_ALLOWED_FRONTMATTER);
    assert_required_skill_creator_frontmatter(skill);

    if let Some(disable_model_invocation) = scalar_field(skill, "disable-model-invocation") {
        assert!(
            matches!(disable_model_invocation, "true" | "false"),
            "{} disable-model-invocation must be a boolean scalar",
            skill.path.display()
        );
    }
    if let Some(paths) = skill.frontmatter.get("paths") {
        let path_globs = paths
            .as_list_items()
            .unwrap_or_else(|| panic!("{} paths must be a YAML list block", skill.path.display()));
        assert!(
            path_globs.iter().all(|glob| !glob.is_empty()),
            "{} paths must be a non-empty YAML list of path globs",
            skill.path.display()
        );
    }
}

fn assert_required_skill_creator_frontmatter(skill: &SkillDoc) {
    let skill_dir = skill.path.parent().expect("skill path has parent");
    let folder_name = skill_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("skill dir should be utf-8");

    let name = required_scalar_field(skill, "name");
    let description = required_scalar_field(skill, "description");

    assert_eq!(
        name,
        folder_name,
        "{} skill name must match its folder",
        skill.path.display()
    );
    assert!(
        is_kebab_case_skill_name(name),
        "{} skill name must be hyphen-case lowercase letters, digits, and hyphens, \
         without leading/trailing/consecutive hyphens",
        skill.path.display()
    );
    assert!(
        name.len() <= 64,
        "{} skill name exceeds Codex quick_validate.py's 64 character limit",
        skill.path.display()
    );
    assert_scalar("description", description, &skill.path);
}

fn assert_allowed_frontmatter(skill: &SkillDoc, allowed: &[&str]) {
    let unexpected = skill
        .frontmatter
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .collect::<Vec<_>>();
    assert!(
        unexpected.is_empty(),
        "{} has unexpected frontmatter keys {unexpected:?}; allowed keys are {allowed:?}",
        skill.path.display()
    );
}

fn scalar_field<'a>(skill: &'a SkillDoc, field: &str) -> Option<&'a str> {
    skill.frontmatter.get(field).map(|value| {
        value.as_scalar().unwrap_or_else(|| {
            panic!(
                "{} frontmatter {field} must be an inline scalar",
                skill.path.display()
            )
        })
    })
}

fn skill_named<'a>(skills: &'a [SkillDoc], name: &str) -> &'a SkillDoc {
    skills
        .iter()
        .find(|skill| skill.name == name)
        .unwrap_or_else(|| panic!("missing bundled skill {name}"))
}

fn required_scalar_field<'a>(skill: &'a SkillDoc, field: &str) -> &'a str {
    scalar_field(skill, field)
        .unwrap_or_else(|| panic!("{} is missing {field}", skill.path.display()))
}

fn assert_metadata_budget(label: &str, skills: &[SkillDoc], include: impl Fn(&SkillDoc) -> bool) {
    let total_metadata_chars = skills
        .iter()
        .filter(|skill| include(skill))
        .map(|skill| {
            required_scalar_field(skill, "name").len()
                + required_scalar_field(skill, "description").len()
        })
        .sum::<usize>();

    assert!(
        total_metadata_chars <= MAX_BUNDLED_SKILL_METADATA_CHARS,
        "{label} skill metadata uses {total_metadata_chars} chars; keep bundled descriptions concise"
    );
}

fn assert_scalar(field: &str, value: &str, path: &Path) {
    assert!(
        !value.trim().is_empty(),
        "{} frontmatter {field} cannot be empty",
        path.display()
    );
    assert_eq!(
        value.trim(),
        value,
        "{} frontmatter {field} cannot have leading or trailing whitespace",
        path.display()
    );
}

fn is_cursor_explicit_invoke_only(skill: &SkillDoc) -> bool {
    scalar_field(skill, "disable-model-invocation") == Some("true")
}

fn assert_openai_yaml_contract_if_present(skill_dir: &Path) {
    let openai_yaml = skill_dir.join("agents/openai.yaml");
    if !openai_yaml.exists() {
        return;
    }
    let body = std::fs::read_to_string(&openai_yaml)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", openai_yaml.display()));
    for field in ["display_name:", "short_description:", "default_prompt:"] {
        assert!(
            body.lines().any(|line| line.starts_with(field)),
            "{} must include {field}",
            openai_yaml.display()
        );
    }
}
