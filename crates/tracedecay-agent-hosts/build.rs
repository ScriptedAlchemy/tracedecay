use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn collect_files_relative(root: &Path) -> Vec<String> {
    fn walk(base: &Path, directory: &Path, files: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, files);
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(base)
            {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

fn append_plugin_files(code: &mut String, constant: &str, source_root: &Path, deploy_prefix: &str) {
    code.push_str(&format!("pub const {constant}: &[PluginFile] = &[\n"));
    for relative in collect_files_relative(source_root) {
        let deploy_path = format!("{deploy_prefix}/{relative}");
        let source_path = source_root.join(&relative);
        let contents = fs::read_to_string(&source_path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()))
            .replace("\r\n", "\n");
        code.push_str(&format!(
            "    PluginFile {{ relative: {deploy_path:?}, contents: {contents:?} }},\n"
        ));
    }
    code.push_str("];\n");
}

fn product_version(repository: &Path) -> String {
    let manifest = repository.join("Cargo.toml");
    let raw = fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", manifest.display()));
    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package
            && let Some(value) = trimmed.strip_prefix("version = ")
            && let Some(version) = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        {
            return version.to_string();
        }
    }
    panic!("{} is missing [package].version", manifest.display());
}

struct CanonicalAgent {
    file_name: String,
    name: String,
    description: String,
    body: String,
}

fn parse_agent_source(path: &Path) -> CanonicalAgent {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .replace("\r\n", "\n");
    let marker = raw
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n"))
        .unwrap_or_else(|| panic!("{} must have fenced YAML frontmatter", path.display()));
    let end = 4 + marker;
    let frontmatter = &raw[4..end];
    let field = |key: &str| {
        frontmatter
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}: ")))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("{} is missing `{key}` frontmatter", path.display()))
            .to_string()
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("{} has a non-UTF-8 filename", path.display()))
        .to_string();
    let name = field("name");
    assert_eq!(
        file_name.strip_suffix(".md"),
        Some(name.as_str()),
        "{} filename must match its `name` frontmatter",
        path.display()
    );
    CanonicalAgent {
        file_name,
        name,
        description: field("description"),
        body: raw[end + "\n---\n".len()..].to_string(),
    }
}

fn quoted_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                panic!("agent adapter contains control character")
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn append_generated_plugin_files(
    code: &mut String,
    constant: &str,
    files: impl IntoIterator<Item = (String, String)>,
) {
    code.push_str(&format!("pub const {constant}: &[PluginFile] = &[\n"));
    for (relative, contents) in files {
        code.push_str(&format!(
            "    PluginFile {{ relative: {relative:?}, contents: {contents:?} }},\n"
        ));
    }
    code.push_str("];\n");
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest"));
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("agent-hosts must live under crates/");
    let plugin_root = repository.join("plugin");
    let mut code = String::from("// @generated by build.rs; do not edit.\n");
    append_plugin_files(
        &mut code,
        "GENERATED_SKILL_FILES",
        &plugin_root.join("skills"),
        "skills",
    );
    append_plugin_files(
        &mut code,
        "GENERATED_CLAUDE_AGENT_FILES",
        &plugin_root.join("agents"),
        "agents",
    );
    let agents = collect_files_relative(&plugin_root.join("agents"))
        .into_iter()
        .map(|relative| parse_agent_source(&plugin_root.join("agents").join(relative)))
        .collect::<Vec<_>>();
    append_generated_plugin_files(
        &mut code,
        "GENERATED_CURSOR_AGENT_FILES",
        agents.iter().map(|agent| {
            (
                format!("agents/{}", agent.file_name),
                format!(
                    "---\nname: {}\ndescription: {}\nreadonly: true\n---\n{}",
                    quoted_string(&agent.name),
                    quoted_string(&agent.description),
                    agent.body
                ),
            )
        }),
    );
    append_generated_plugin_files(
        &mut code,
        "GENERATED_CODEX_AGENT_FILES",
        agents.iter().map(|agent| {
            (
                format!("tracedecay-{}.toml", agent.name),
                format!(
                    "name = {}\ndescription = {}\nsandbox_mode = \"read-only\"\ndeveloper_instructions = {}\n",
                    quoted_string(&format!("tracedecay-{}", agent.name)),
                    quoted_string(&agent.description),
                    quoted_string(&agent.body),
                ),
            )
        }),
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("plugin_bundle_generated.rs"), code).expect("write plugin bundle");
    println!("cargo::rerun-if-changed={}", plugin_root.display());
    println!(
        "cargo::rerun-if-changed={}",
        repository.join("Cargo.toml").display()
    );
    println!(
        "cargo::rustc-env=TRACEDECAY_PRODUCT_VERSION={}",
        product_version(repository)
    );
    println!(
        "cargo::rustc-env=TRACEDECAY_REPOSITORY_ROOT={}",
        repository.display()
    );
    let git_sha = Command::new("git")
        .current_dir(repository)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo::rustc-env=TRACEDECAY_GIT_SHA={git_sha}");
}
