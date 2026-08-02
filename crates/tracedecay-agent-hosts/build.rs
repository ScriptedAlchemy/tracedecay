//! Crate-local build script for the agent-host bundle.
//!
//! Moved verbatim (paths rebased) from the root `build.rs` when `src/agents/`
//! and `src/automation/` were extracted into `tracedecay-agent-hosts`. It
//! generates `$OUT_DIR/plugin_bundle_generated.rs`, which
//! `src/agents/plugin_bundle.rs` includes.
//!
//! Path rebase contract: the shared `plugin/` tree stays at the repository
//! root, which is two directories above this crate
//! (`crates/tracedecay-agent-hosts/`). Both the `rerun-if-changed` watches and
//! the `include_str!` paths this script emits therefore go through
//! `../../plugin/`. `CARGO_MANIFEST_DIR` is this crate's directory, not the
//! workspace root, so the generated `concat!` prefix must carry the same
//! `/../..` hop.

use std::{fmt::Write as _, fs, path::Path};

/// Repository-root-relative prefix from this crate's directory.
const REPO_ROOT_FROM_CRATE: &str = "../..";

// Same include the root build script uses, so the commit stamp this crate
// bakes into the Hermes plugin provenance headers is produced by the code its
// unit tests exercise rather than a second copy that can drift. The probe is
// pointed at the repository root, not this crate's directory: a crate
// subdirectory is never its own git worktree top level, so `resolve` would
// otherwise report an empty identity by design.
#[path = "../../src/version/build_identity.rs"]
mod build_identity;

// The product version this crate stamps into host-visible artifacts comes from
// the root package, not from this sub-crate. Compiling the crate's own parser
// here keeps the baked value and the constant's drift test on one
// implementation.
#[path = "src/product_version/root_manifest.rs"]
mod root_manifest;

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
    println!("cargo::rerun-if-changed={REPO_ROOT_FROM_CRATE}/plugin/{source_prefix}");
    let _ = write!(
        code,
        "/// Every UTF-8 file under `plugin/{source_prefix}/`.\n\
         pub const {const_name}: &[PluginFile] = &[\n"
    );
    for relative in collect_files_relative(source_root) {
        println!(
            "cargo::rerun-if-changed={REPO_ROOT_FROM_CRATE}/plugin/{source_prefix}/{relative}"
        );
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
            "    PluginFile {{ relative: {deploy:?}, contents: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{REPO_ROOT_FROM_CRATE}/plugin/{source}\")) }},"
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
    let plugin_root = Path::new(&manifest_dir)
        .join(REPO_ROOT_FROM_CRATE)
        .join("plugin");

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
    // Only rewrite when the content differs, matching the dashboard manifest
    // and logo: this script reruns for inputs it does not consume, and an
    // identical rewrite would still churn the manifest every time.
    if !matches!(fs::read_to_string(&out_path), Ok(current) if current == code)
        && let Err(e) = fs::write(&out_path, code)
    {
        panic!("failed to write {}: {e}", out_path.display());
    }
}

/// Bakes the build's commit identity into `TRACEDECAY_GIT_SHA`.
///
/// `agents/hermes/templates.rs` stamps it into every generated Hermes plugin
/// file so `tracedecay doctor` can tell a live install apart from one clobbered
/// by a different generator build. The root build script emits the same pair
/// for the root crate; `env!` is resolved per compiled crate, so this crate
/// must emit its own.
fn bake_build_identity() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(&manifest_dir).join(REPO_ROOT_FROM_CRATE);
    let identity = build_identity::resolve(&repo_root);
    for path in build_identity::watch_paths(&repo_root) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    println!("cargo::rerun-if-changed={REPO_ROOT_FROM_CRATE}/src/version/build_identity.rs");
    println!(
        "cargo::rustc-env=TRACEDECAY_GIT_SHA={}",
        identity.sha.as_deref().unwrap_or("unknown")
    );
}

/// Bakes the root package's version into `TRACEDECAY_PRODUCT_VERSION`.
///
/// `env!("CARGO_PKG_VERSION")` is resolved per compiled crate, so inside this
/// library it is this crate's own version rather than the version of the
/// `tracedecay` product a user installed. Everything this crate stamps into a
/// place a host can see — plugin manifests, plugin cache paths, staleness
/// warnings, provenance headers — is compared against that product version, so
/// it is read here from the one place that authors it: the root package's
/// `version` in the workspace-root `Cargo.toml`.
///
/// An unresolvable root manifest is fatal on purpose. Falling back to this
/// crate's `CARGO_PKG_VERSION` is exactly the silent mismatch this exists to
/// prevent, and the surrounding script already requires the repository root
/// for the shared `plugin/` tree.
fn bake_product_version() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(&manifest_dir).join(REPO_ROOT_FROM_CRATE);
    let manifest_path = root_manifest::manifest_path(&repo_root);
    println!("cargo::rerun-if-changed={}", manifest_path.display());
    println!("cargo::rerun-if-changed=src/product_version/root_manifest.rs");
    let Some(version) = root_manifest::resolve(&repo_root) else {
        panic!(
            "{} must declare the `{}` package's version; it is the product version this crate stamps",
            manifest_path.display(),
            root_manifest::ROOT_PACKAGE_NAME,
        );
    };
    println!("cargo::rustc-env=TRACEDECAY_PRODUCT_VERSION={version}");
}

fn main() {
    bake_build_identity();
    bake_product_version();
    generate_plugin_bundle();
}
