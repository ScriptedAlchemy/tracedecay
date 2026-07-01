#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::agents::{expected_tool_perms, get_integration, InstallContext};

const CODEX_SKILL_ROOT: &str = "codex-plugin/skills";
const CURSOR_SKILL_ROOT: &str = "cursor-plugin/skills";
const MAX_SKILL_MD_LINES: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 320;
const MAX_DESCRIPTION_WORDS: usize = 45;
const MAX_CODEX_METADATA_CHARS: usize = 6_000;
const MAX_CURSOR_MODEL_INVOKED_METADATA_CHARS: usize = 6_000;
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
    frontmatter: BTreeMap<String, String>,
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
fn generated_codex_plugin_skills_match_codex_skill_creator_quick_validate_rules() {
    let home = TempDir::new().expect("temp home");
    let codex = get_integration("codex").expect("codex integration");
    codex
        .install(&install_ctx(home.path()))
        .expect("install generated Codex plugin bundle");

    let skills = load_skill_docs_from_root(home.path().join("plugins/tracedecay/skills"));
    assert_eq!(
        skill_names(&skills),
        skill_names(&load_skill_docs(CODEX_SKILL_ROOT)),
        "generated Codex plugin bundle must ship the same skills as the source bundle"
    );
    for skill in &skills {
        assert_codex_quick_validate_equivalent(skill);
    }
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
fn generated_cursor_plugin_skills_match_cursor_skill_contract() {
    let home = TempDir::new().expect("temp home");
    let cursor = get_integration("cursor").expect("cursor integration");
    cursor
        .install(&install_ctx(home.path()))
        .expect("install generated Cursor plugin bundle");

    let skills =
        load_skill_docs_from_root(home.path().join(".cursor/plugins/local/tracedecay/skills"));
    assert_eq!(
        skill_names(&skills),
        skill_names(&load_skill_docs(CURSOR_SKILL_ROOT)),
        "generated Cursor plugin bundle must ship the same skills as the source bundle"
    );
    for skill in &skills {
        assert_cursor_skill_contract(skill);
    }
}

#[test]
fn produced_plugin_skills_follow_skill_creator_design_advice() {
    let codex_skills = load_skill_docs(CODEX_SKILL_ROOT);
    let cursor_skills = load_skill_docs(CURSOR_SKILL_ROOT);

    assert_metadata_budget("Codex", &codex_skills, MAX_CODEX_METADATA_CHARS, |_| true);
    assert_metadata_budget(
        "Cursor model-invoked",
        &cursor_skills,
        MAX_CURSOR_MODEL_INVOKED_METADATA_CHARS,
        |skill| !is_cursor_explicit_invoke_only(skill),
    );

    for skill in codex_skills.iter().chain(cursor_skills.iter()) {
        let description = skill
            .frontmatter
            .get("description")
            .expect("description present");
        assert_scalar("description", description, &skill.path);
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
            !skill.body.contains("\n## When to Use")
                && !skill.body.contains("\n## When To Use")
                && !skill.body.contains("\n## When to use"),
            "{} must keep trigger guidance in description metadata, not a body-only When to Use section",
            skill.path.display()
        );

        let skill_dir = skill.path.parent().expect("skill path has parent");
        assert_skill_tree_uses_supported_files(skill_dir);
        assert_openai_yaml_contract_if_present(skill_dir);
    }
}

fn load_skill_docs(root: &str) -> Vec<SkillDoc> {
    load_skill_docs_from_root(Path::new(env!("CARGO_MANIFEST_DIR")).join(root))
}

fn load_skill_docs_from_root(skills_root: PathBuf) -> Vec<SkillDoc> {
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
            let frontmatter = parse_skill_frontmatter(&path, &body);
            SkillDoc {
                path,
                body,
                frontmatter,
            }
        })
        .collect()
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

fn skill_names(skills: &[SkillDoc]) -> Vec<String> {
    skills
        .iter()
        .map(|skill| {
            skill
                .path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .expect("skill path has utf-8 directory name")
                .to_string()
        })
        .collect()
}

fn assert_codex_quick_validate_equivalent(skill: &SkillDoc) {
    assert_allowed_frontmatter(skill, CODEX_QUICK_VALIDATE_ALLOWED_FRONTMATTER);
    assert_required_skill_creator_frontmatter(skill);
    let description = skill
        .frontmatter
        .get("description")
        .expect("description present");
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

    if let Some(disable_model_invocation) = skill.frontmatter.get("disable-model-invocation") {
        assert!(
            matches!(disable_model_invocation.as_str(), "true" | "false"),
            "{} disable-model-invocation must be a boolean scalar",
            skill.path.display()
        );
    }
    if let Some(paths) = skill.frontmatter.get("paths") {
        let path_globs = paths
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert!(
            !path_globs.is_empty()
                && path_globs
                    .iter()
                    .all(|line| line.starts_with("- ") && line.len() > 2),
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

    let name = skill
        .frontmatter
        .get("name")
        .unwrap_or_else(|| panic!("{} is missing name", skill.path.display()));
    let description = skill
        .frontmatter
        .get("description")
        .unwrap_or_else(|| panic!("{} is missing description", skill.path.display()));

    assert_eq!(
        name,
        folder_name,
        "{} skill name must match its folder",
        skill.path.display()
    );
    assert!(
        is_skill_creator_name(name),
        "{} skill name must be hyphen-case lowercase letters, digits, and hyphens",
        skill.path.display()
    );
    assert!(
        !name.starts_with('-') && !name.ends_with('-') && !name.contains("--"),
        "{} skill name cannot start/end with hyphen or contain consecutive hyphens",
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

fn parse_skill_frontmatter(path: &Path, body: &str) -> BTreeMap<String, String> {
    let rest = body
        .strip_prefix("---\n")
        .unwrap_or_else(|| panic!("{} must start with YAML frontmatter", path.display()));
    let (frontmatter, _) = rest
        .split_once("\n---\n")
        .unwrap_or_else(|| panic!("{} must close YAML frontmatter", path.display()));
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for line in frontmatter.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with(char::is_whitespace) {
            let Some(key) = &last_key else {
                panic!(
                    "{} has indented frontmatter line before any key: {line:?}",
                    path.display()
                );
            };
            let value = fields
                .get_mut(key)
                .expect("last frontmatter key should be present");
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(line);
            continue;
        }
        let (key, raw_value) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("{} has invalid frontmatter line {line:?}", path.display()));
        assert!(
            fields
                .insert(
                    key.to_string(),
                    unquote_frontmatter_scalar(raw_value.trim())
                )
                .is_none(),
            "{} duplicates frontmatter key {key}",
            path.display()
        );
        last_key = Some(key.to_string());
    }
    fields
}

fn unquote_frontmatter_scalar(value: &str) -> String {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        inner.to_string()
    } else if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        inner.to_string()
    } else {
        value.to_string()
    }
}

fn assert_metadata_budget(
    label: &str,
    skills: &[SkillDoc],
    max_total_chars: usize,
    include: impl Fn(&SkillDoc) -> bool,
) {
    let total_metadata_chars = skills
        .iter()
        .filter(|skill| include(skill))
        .map(|skill| {
            skill.frontmatter.get("name").map_or(0, String::len)
                + skill.frontmatter.get("description").map_or(0, String::len)
        })
        .sum::<usize>();

    assert!(
        total_metadata_chars <= max_total_chars,
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
    assert!(
        !value.contains(['\n', '\r']),
        "{} frontmatter {field} must be a single line",
        path.display()
    );
}

fn is_skill_creator_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn has_trigger_language(description: &str) -> bool {
    description.contains("Use when")
        || description.contains("Use before")
        || description.contains("Use to")
}

fn is_cursor_explicit_invoke_only(skill: &SkillDoc) -> bool {
    skill
        .frontmatter
        .get("disable-model-invocation")
        .is_some_and(|value| value == "true")
}

fn assert_skill_tree_uses_supported_files(skill_dir: &Path) {
    let allowed_resource_dirs = ["agents", "scripts", "references", "assets"];
    let forbidden_doc_files = [
        "README.md",
        "CHANGELOG.md",
        "INSTALLATION_GUIDE.md",
        "QUICK_REFERENCE.md",
    ];
    let mut stack = vec![skill_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let entry = entry.expect("read skill tree entry");
            let path = entry.path();
            let relative = path.strip_prefix(skill_dir).expect("relative skill path");
            let first = relative
                .components()
                .next()
                .and_then(|component| component.as_os_str().to_str())
                .expect("relative component");
            if path.is_dir() {
                assert!(
                    allowed_resource_dirs.contains(&first),
                    "{} contains unsupported top-level directory {}",
                    skill_dir.display(),
                    first
                );
                stack.push(path);
                continue;
            }

            let file_name = path
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
                "{} contains unsupported top-level file {}",
                skill_dir.display(),
                relative.display()
            );
        }
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
