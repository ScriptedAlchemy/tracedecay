#![allow(dead_code)] // shared test support helpers
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::Validator;
use serde_json::Value;
use tracedecay_agent_hosts::automation::skill_frontmatter::{
    SkillFrontmatterValue, parse_skill_frontmatter,
};

/// A path relative to the repository root (`CARGO_MANIFEST_DIR`).
pub fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

pub fn read_json_file(path: &Path) -> Value {
    let body = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|err| panic!("failed to parse JSON {}: {err}", path.display()))
}

/// Compiles a vendored draft-07 schema with format assertions enabled.
pub fn compile_schema(schema: &Value) -> Validator {
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .expect("vendored schema should compile")
}

/// Asserts `instance` validates against `validator`, reporting every
/// violation with its JSON pointer.
pub fn assert_schema_valid(validator: &Validator, instance: &Value, instance_path: &Path) {
    let errors = validator
        .iter_errors(instance)
        .map(|err| format!("  {}: {err}", err.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{} violates the vendored schema:\n{}",
        instance_path.display(),
        errors.join("\n")
    );
}

/// One bundled `SKILL.md`, parsed once for every lint/contract consumer.
#[derive(Debug)]
pub struct SkillDoc {
    /// The skill directory name (which the contracts force `name` to match).
    pub name: String,
    /// Path to the `SKILL.md` file.
    pub path: PathBuf,
    /// Full file contents, frontmatter included.
    pub raw: String,
    /// Contents after the closing `---` frontmatter fence.
    pub body: String,
    pub frontmatter: BTreeMap<String, SkillFrontmatterValue>,
}

/// Loads every `<root>/<skill>/SKILL.md` under a repo-relative skills root,
/// sorted by path. Panics on unreadable/unparsable skills and asserts the
/// root is non-empty.
pub fn load_skill_docs(root: &str) -> Vec<SkillDoc> {
    load_skill_docs_from(&repo_path(root))
}

/// Like [`load_skill_docs`] but takes an absolute skills-root path (e.g. a
/// staged temp dir composing per-host skill sources).
pub fn load_skill_docs_from(skills_root: &Path) -> Vec<SkillDoc> {
    let mut dirs = fs::read_dir(skills_root)
        .unwrap_or_else(|err| {
            panic!(
                "failed to read bundled skills at {}: {err}",
                skills_root.display()
            )
        })
        .map(|entry| entry.expect("read skill dir entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    assert!(
        !dirs.is_empty(),
        "expected skill directories under {}",
        skills_root.display()
    );

    dirs.into_iter()
        .map(|dir| {
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skill directory name should be utf-8")
                .to_string();
            let path = dir.join("SKILL.md");
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            let frontmatter = parse_skill_frontmatter(&raw)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let body = body_after_frontmatter(&raw).to_string();
            SkillDoc {
                name,
                path,
                raw,
                body,
                frontmatter,
            }
        })
        .collect()
}

/// Returns the markdown body following the closing `---` frontmatter fence.
pub fn body_after_frontmatter(raw: &str) -> &str {
    let mut offset = 0usize;
    for (index, line) in raw.split_inclusive('\n').enumerate() {
        offset += line.len();
        if index > 0 && line.trim() == "---" {
            return &raw[offset..];
        }
    }
    ""
}

/// Skill-name rule shared by the Agent Skills spec, Codex's
/// `quick_validate.py`, and Cursor: lowercase alphanumerics and hyphens,
/// without leading/trailing/consecutive hyphens.
pub fn is_kebab_case_skill_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Every regular file under `root`, relative to it, sorted.
pub fn relative_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("read tree entry").path();
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
