use std::hash::{Hash, Hasher};
use std::process::Command;
use std::{collections::hash_map::DefaultHasher, fmt::Write as _, fs, path::Path};

// Prebuilt dashboard bundles embedded by `src/dashboard/assets.rs`. The
// dashboard frontend is being rewritten from scratch; until the new UI lands,
// `dashboard/*/dist/` ships committed placeholder bundles (a minimal
// "rebuild in progress" shell). The build no longer shells out to npm/node —
// it only verifies the placeholder bundles exist and hashes them into the
// asset stamp used for HTTP caching/ETags.
const DASHBOARD_ASSET_FILES: &[&str] = &[
    "dashboard/shell/dist/shell.js",
    "dashboard/shell/dist/shell.css",
    "dashboard/holographic/dist/index.js",
    "dashboard/holographic/dist/style.css",
    "dashboard/lcm/dist/index.js",
    "dashboard/lcm/dist/style.css",
    "dashboard/graph/dist/index.js",
    "dashboard/graph/dist/style.css",
    "dashboard/code-diagnostics/dist/index.js",
    "dashboard/code-diagnostics/dist/style.css",
    "dashboard/savings/dist/index.js",
    "dashboard/savings/dist/style.css",
    "dashboard/settings/dist/index.js",
    "dashboard/settings/dist/style.css",
];

/// Hashes the committed dashboard placeholder bundles into a stable asset stamp
/// (used for HTTP `ETag`/cache validation in `src/dashboard/assets.rs`). Fails
/// fast with a clear message if any placeholder bundle is missing — the
/// bundles are tracked in git, so a missing file means a corrupt checkout, not
/// a skipped frontend build.
fn emit_dashboard_asset_inputs() -> String {
    let mut hasher = DefaultHasher::new();
    let mut missing = Vec::new();
    for relative in DASHBOARD_ASSET_FILES {
        println!("cargo::rerun-if-changed={relative}");
        relative.hash(&mut hasher);
        match fs::read(relative) {
            Ok(bytes) => bytes.hash(&mut hasher),
            Err(_) => missing.push(*relative),
        }
    }
    if !missing.is_empty() {
        panic!(
            "\n\ndashboard placeholder bundles are missing:\n  {}\n\n\
             These are committed to git (dashboard/*/dist/**). A missing file\n\
             means the checkout is incomplete; restore them with `git checkout\n\
             -- dashboard`.\n",
            missing.join("\n  ")
        );
    }
    format!("{:016x}", hasher.finish())
}

/// Recursively collects every file under `root`, relative to `root`, using
/// forward-slash separators. Returns sorted paths so codegen is deterministic.
fn collect_files_relative(root: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(base)
            {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

/// True when `path` is a readable UTF-8 text file. Used to fail the skill
/// bundle codegen early with a clear message when a binary support file would
/// otherwise break `include_str!` with an opaque compile error.
fn is_probably_utf8_text(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => std::str::from_utf8(&bytes).is_ok(),
        // Unreadable files fall through to include_str!'s own error.
        Err(_) => true,
    }
}

fn append_plugin_files(
    code: &mut String,
    const_name: &str,
    source_root: &Path,
    source_prefix: &str,
    deploy_prefix: &str,
) {
    println!("cargo::rerun-if-changed=plugin/{source_prefix}");
    let _ = write!(
        code,
        "/// Every UTF-8 file under `plugin/{source_prefix}/`.\n\
         pub const {const_name}: &[PluginFile] = &[\n"
    );
    for relative in collect_files_relative(source_root) {
        println!("cargo::rerun-if-changed=plugin/{source_prefix}/{relative}");
        let abs = source_root.join(&relative);
        if !is_probably_utf8_text(&abs) {
            panic!(
                "plugin/{source_prefix}/{relative} is not a UTF-8 text file; plugin bundle files are embedded with include_str!"
            );
        }
        let deploy = format!("{deploy_prefix}/{relative}");
        let source = format!("{source_prefix}/{relative}");
        let _ = writeln!(
            code,
            "    PluginFile {{ relative: {deploy:?}, contents: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/plugin/{source}\")) }},"
        );
    }
    code.push_str("];\n");
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
    let frontmatter_marker = raw
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n"))
        .unwrap_or_else(|| panic!("{} must have fenced YAML frontmatter", path.display()));
    let frontmatter_end = 4 + frontmatter_marker;
    let body_start = frontmatter_end + "\n---\n".len();
    let frontmatter = &raw[4..frontmatter_end];
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
        .unwrap_or_else(|| panic!("{} has a non-UTF-8 file name", path.display()))
        .to_string();
    let name = field("name");
    assert_eq!(
        file_name.strip_suffix(".md"),
        Some(name.as_str()),
        "{} file name must match its agent name",
        path.display()
    );
    CanonicalAgent {
        file_name,
        name,
        description: field("description"),
        body: raw[body_start..].to_string(),
    }
}

/// Quote the shared JSON-compatible string subset accepted by both YAML and
/// TOML basic strings.
fn quoted_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => panic!("agent adapter contains unsupported control character"),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn append_generated_plugin_files(
    code: &mut String,
    const_name: &str,
    files: impl IntoIterator<Item = (String, String)>,
) {
    let _ = writeln!(code, "pub const {const_name}: &[PluginFile] = &[");
    for (relative, contents) in files {
        let _ = writeln!(
            code,
            "    PluginFile {{ relative: {relative:?}, contents: {contents:?} }},"
        );
    }
    code.push_str("];\n");
}

/// Generates `$OUT_DIR/plugin_bundle_generated.rs`: recursive manifests for
/// shared skills and the canonical Claude agent catalog. Cursor markdown and
/// Codex TOML adapters are derived from that catalog, so host metadata and
/// instructions cannot drift between hand-maintained copies.
///
/// Each entry's deploy path equals its `plugin/`-relative source path
/// (`skills/<skill>/<subpath>`), which is identical for every host, so a single
/// generated slice serves Claude, Codex, and Cursor (Cursor filters out the
/// `tracedecay-*` dispatcher skills at compose time).
fn generate_plugin_bundle() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let plugin_root = Path::new(&manifest_dir).join("plugin");

    let mut code =
        String::from("// @generated by build.rs (generate_plugin_bundle). Do not edit.\n");
    append_plugin_files(
        &mut code,
        "GENERATED_SKILL_FILES",
        &plugin_root.join("skills"),
        "skills",
        "skills",
    );
    append_plugin_files(
        &mut code,
        "GENERATED_CLAUDE_AGENT_FILES",
        &plugin_root.join("agents"),
        "agents",
        "agents",
    );
    let agents = collect_files_relative(&plugin_root.join("agents"))
        .into_iter()
        .map(|relative| {
            assert!(
                relative.ends_with(".md"),
                "plugin/agents/{relative} must be Markdown"
            );
            parse_agent_source(&plugin_root.join("agents").join(relative))
        })
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

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let out_path = Path::new(&out_dir).join("plugin_bundle_generated.rs");
    if let Err(e) = fs::write(&out_path, code) {
        panic!("failed to write {}: {e}", out_path.display());
    }
}

fn main() {
    generate_plugin_bundle();
    let out_path = Path::new("src/resources/logo.ansi");
    let logo_bytes = include_bytes!("src/resources/logo.png");
    let ansi = logo_art::image_to_ansi(logo_bytes, 90);
    // Only rewrite when the content differs: `cargo package` verification
    // rejects packages whose build script modifies files in the source dir.
    if !matches!(fs::read(out_path), Ok(current) if current == ansi.as_bytes())
        && let Err(e) = fs::write(out_path, &ansi)
    {
        panic!("failed to write {}: {e}", out_path.display());
    }
    println!("cargo::rerun-if-changed=src/resources/logo.png");
    let asset_stamp = emit_dashboard_asset_inputs();
    println!("cargo::rustc-env=TRACEDECAY_DASHBOARD_ASSET_STAMP={asset_stamp}");

    // Generator provenance: baked into generated agent plugins (manifest +
    // module header) so a stale installed plugin is distinguishable from
    // the binary that should have generated it. Advisory only — may lag a
    // commit until the next build-script rerun.
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo::rustc-env=TRACEDECAY_GIT_SHA={git_sha}");

    // Vendored WGSL grammar — compiled only when lang-wgsl is enabled.
    // Using vendored sources avoids pulling in tree-sitter-wgsl 0.0.6 which was
    // built against the incompatible tree-sitter 0.20 API.
    if std::env::var("CARGO_FEATURE_LANG_WGSL").is_ok() {
        let wgsl_dir = Path::new("vendor/tree-sitter-wgsl/src");
        cc::Build::new()
            .include(wgsl_dir)
            .file(wgsl_dir.join("parser.c"))
            .file(wgsl_dir.join("scanner.c"))
            .warnings(false)
            .compile("tree_sitter_wgsl");
        println!("cargo::rerun-if-changed=vendor/tree-sitter-wgsl/src/parser.c");
        println!("cargo::rerun-if-changed=vendor/tree-sitter-wgsl/src/scanner.c");
    }
}
