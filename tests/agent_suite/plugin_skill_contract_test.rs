//! Contract tests for the bundled Codex (`codex-plugin/`) and Cursor
//! (`cursor-plugin/`) plugin skills: frontmatter schema per host, plus the
//! shared skill-creator design-advice checks.
//!
//! Cross-host parity — each Codex skill mirroring its Cursor source, and the
//! divergence allowlists — is covered by the unit tests in
//! `src/agents/codex.rs` (`codex_skills_match_the_cursor_source_for_parity`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK};
use tempfile::TempDir;
use tracedecay::agents::{expected_tool_perms, get_integration, InstallContext};
use tracedecay::automation::skill_frontmatter::{parse_skill_frontmatter, SkillFrontmatterValue};
use tracedecay::config::USER_DATA_DIR_ENV;

const CODEX_SKILL_ROOT: &str = "codex-plugin/skills";
const CURSOR_SKILL_ROOT: &str = "cursor-plugin/skills";
// Size budgets: the 500-line body cap and the "concise, trigger-first
// description" rule come from Anthropic's skill-creator design advice. The
// numeric description and metadata caps are house budgets chosen when these
// bundles were written: one description stays scannable at roughly two
// sentences (320 chars / 45 words), and a bundle's preloaded name+description
// metadata stays under 6,000 chars (~1.5k tokens) so skill discovery never
// crowds an agent host's context window.
const MAX_SKILL_MD_LINES: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 320;
const MAX_DESCRIPTION_WORDS: usize = 45;
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

#[derive(Debug)]
struct SkillDoc {
    path: PathBuf,
    body: String,
    frontmatter: BTreeMap<String, SkillFrontmatterValue>,
}

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
    let _env_lock = install_env_lock();
    let home = TempDir::new().expect("temp home");
    let _data_dir_guard = pinned_profile_storage(home.path());
    let codex = get_integration("codex").expect("codex integration");
    codex
        .install(&install_ctx(home.path()))
        .expect("install generated Codex plugin bundle");

    let source_root = skills_source_root(CODEX_SKILL_ROOT);
    let installed_root = home.path().join("plugins/tracedecay/skills");
    assert_eq!(
        skill_dir_names(&installed_root),
        skill_dir_names(&source_root),
        "generated Codex plugin bundle must ship the same skills as the source bundle"
    );
    assert_skill_trees_byte_identical(&source_root, &installed_root);
}

#[test]
fn cursor_plugin_skills_match_cursor_skill_contract() {
    let skills = load_skill_docs(CURSOR_SKILL_ROOT);
    assert!(!skills.is_empty(), "expected bundled Cursor skills");

    for skill in &skills {
        assert_cursor_skill_contract(skill);
    }
}

#[test]
fn generated_cursor_plugin_skills_are_byte_copies_of_the_source_bundle() {
    let _env_lock = install_env_lock();
    let home = TempDir::new().expect("temp home");
    let _data_dir_guard = pinned_profile_storage(home.path());
    let cursor = get_integration("cursor").expect("cursor integration");
    cursor
        .install(&install_ctx(home.path()))
        .expect("install generated Cursor plugin bundle");

    let source_root = skills_source_root(CURSOR_SKILL_ROOT);
    let installed_root = home.path().join(".cursor/plugins/local/tracedecay/skills");
    assert_eq!(
        skill_dir_names(&installed_root),
        skill_dir_names(&source_root),
        "generated Cursor plugin bundle must ship the same skills as the source bundle"
    );
    assert_skill_trees_byte_identical(&source_root, &installed_root);
}

#[test]
fn produced_plugin_skills_follow_skill_creator_design_advice() {
    let codex_skills = load_skill_docs(CODEX_SKILL_ROOT);
    let cursor_skills = load_skill_docs(CURSOR_SKILL_ROOT);

    assert_metadata_budget("Codex", &codex_skills, |_| true);
    assert_metadata_budget("Cursor model-invoked", &cursor_skills, |skill| {
        !is_cursor_explicit_invoke_only(skill)
    });

    for skill in codex_skills.iter().chain(cursor_skills.iter()) {
        let description = required_scalar_field(skill, "description");
        assert!(
            has_trigger_language(description),
            "{} description must include trigger language because agents only see metadata before loading the body",
            skill.path.display()
        );
        assert!(
            description.len() <= MAX_DESCRIPTION_CHARS,
            "{} description is too long for the shared skills metadata budget",
            skill.path.display()
        );
        assert!(
            description.split_whitespace().count() <= MAX_DESCRIPTION_WORDS,
            "{} description has too many words for the shared skills metadata budget",
            skill.path.display()
        );

        let line_count = skill.body.lines().count();
        assert!(
            line_count <= MAX_SKILL_MD_LINES,
            "{} has {line_count} lines; split details into direct references before exceeding {MAX_SKILL_MD_LINES}",
            skill.path.display()
        );
        assert!(
            !skill.body.to_ascii_lowercase().contains("\n## when to use"),
            "{} must keep trigger guidance in description metadata, not a body-only When to Use section",
            skill.path.display()
        );

        let skill_dir = skill.path.parent().expect("skill path has parent");
        assert_skill_tree_uses_supported_files(skill_dir);
        assert_openai_yaml_contract_if_present(skill_dir);
    }
}

fn skills_source_root(root: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(root)
}

fn load_skill_docs(root: &str) -> Vec<SkillDoc> {
    let skills_root = skills_source_root(root);
    let mut paths = std::fs::read_dir(&skills_root)
        .unwrap_or_else(|err| {
            panic!(
                "failed to read bundled skills at {}: {err}",
                skills_root.display()
            )
        })
        .map(|entry| entry.expect("read skill dir entry").path())
        .filter(|path| path.is_dir())
        .map(|path| path.join("SKILL.md"))
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            let frontmatter = parse_skill_frontmatter(&body)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            SkillDoc {
                path,
                body,
                frontmatter,
            }
        })
        .collect()
}

/// Serializes the generated-bundle tests, which mutate process-wide env vars.
fn install_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    PROCESS_ENV_LOCK.blocking_lock()
}

/// Pins TraceDecay profile storage to the temp home so an ambient
/// `TRACEDECAY_DATA_DIR` with active managed skills cannot leak an
/// `agent-managed` overlay into the generated bundle.
fn pinned_profile_storage(home: &Path) -> EnvVarGuard {
    EnvVarGuard::set(USER_DATA_DIR_ENV, home.join(".tracedecay"))
}

fn install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/tmp/tracedecay-test-bin".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

fn skill_dir_names(skills_root: &Path) -> Vec<String> {
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
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// The installed skills are `include_str!` byte-copies of the source tree
/// (`src/agents/codex.rs` asserts the embedded list covers every source
/// file), so byte-parity subsumes re-running the per-skill contract over the
/// installed copies and additionally catches any install-time mutation.
fn assert_skill_trees_byte_identical(source_root: &Path, installed_root: &Path) {
    let source_files = relative_files_under(source_root);
    let installed_files = relative_files_under(installed_root);
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

fn relative_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("read skill tree entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .expect("collected paths live under root")
                        .to_path_buf(),
                );
            }
        }
    }
    files.sort();
    files
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
        is_skill_creator_name(name),
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

fn is_skill_creator_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Agents choose skills from metadata alone, so each description must carry
/// an imperative "Use ..." trigger sentence: either leading the description
/// or following a short capability summary (e.g. "Find code by concept ...
/// Use when searching the codebase").
fn has_trigger_language(description: &str) -> bool {
    description.starts_with("Use ") || description.contains(". Use ")
}

fn is_cursor_explicit_invoke_only(skill: &SkillDoc) -> bool {
    scalar_field(skill, "disable-model-invocation") == Some("true")
}

fn assert_skill_tree_uses_supported_files(skill_dir: &Path) {
    let allowed_resource_dirs = ["agents", "scripts", "references", "assets"];
    let forbidden_doc_files = [
        "README.md",
        "CHANGELOG.md",
        "INSTALLATION_GUIDE.md",
        "QUICK_REFERENCE.md",
    ];
    for relative in relative_files_under(skill_dir) {
        let first = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .expect("relative component");
        let file_name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .expect("skill file name should be utf-8");
        assert!(
            !forbidden_doc_files.contains(&file_name),
            "{} contains auxiliary documentation file {}; keep skill folders lean",
            skill_dir.display(),
            file_name
        );
        assert!(
            relative == Path::new("SKILL.md") || allowed_resource_dirs.contains(&first),
            "{} contains unsupported top-level entry {}",
            skill_dir.display(),
            relative.display()
        );
    }
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
