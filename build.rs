//! Root build script: embeds the dashboard dist and the plugin bundle, renders
//! the CLI logo, and bakes the build's commit identity.
//!
//! Rerun-edge contract. Cargo recompiles the whole root crate whenever this
//! script reruns, regardless of whether the generated output changed, so every
//! `rerun-if-changed` path below costs a full root rebuild when it moves and
//! must be load-bearing. Two rules follow:
//!
//! - This script must never write to a path it watches, or it arms its own
//!   trigger. `dashboard/app-dist/.source-stamp` is the one file it writes into
//!   a watched tree, so `app-dist` is registered file-by-file with that stamp
//!   skipped rather than as a directory.
//! - Moving these generators into their own crate would not shield the root
//!   from the churn: a dependency's build-script rerun recompiles its
//!   dependents unconditionally.

use std::hash::{Hash, Hasher};
use std::process::Command;
use std::{collections::hash_map::DefaultHasher, fmt::Write as _, fs, path::Path};

#[path = "build-support/dashboard_cache.rs"]
mod dashboard_cache;

// Shared with the crate as `tracedecay::version::build_identity`, so the probe
// that bakes the build's commit identity is the code its unit tests exercise
// rather than a second copy that can drift.
include!("src/version/build_identity.rs");

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
    // Only rewrite when the content differs, matching the dashboard manifest
    // and logo: this script reruns for inputs it does not consume, and an
    // identical rewrite would still churn the manifest every time.
    if !matches!(fs::read_to_string(&out_path), Ok(current) if current == code)
        && let Err(e) = fs::write(&out_path, code)
    {
        panic!("failed to write {}: {e}", out_path.display());
    }
}

/// The single-app dashboard (dashboard/app-dist, built by rsbuild). The build
/// keeps a content stamp over the frontend sources; when stale (or app-dist is
/// missing) it shells out to `npm run build` (npm ci first when node_modules
/// is absent) and fails fast on error. In a packaged crate the frontend
/// sources are absent, so the packaged app-dist is used as-is and npm is
/// never invoked. The dist is then embedded via a
/// generated manifest in OUT_DIR so the installed binary serves the UI with
/// zero filesystem dependency.
fn build_and_embed_dashboard_app() {
    let repository_root = Path::new(".");
    let dashboard = Path::new("dashboard");
    let app_dist = dashboard.join("app-dist");

    for input in dashboard_cache::source_inputs() {
        println!("cargo::rerun-if-changed={input}");
    }
    let source_stamp = dashboard_cache::source_stamp(repository_root);
    let stamp_path = app_dist.join(".source-stamp");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_SKIP_DASHBOARD_BUILD");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DOGFOOD_DASHBOARD_STAMP_PATH");
    let contract_schema_export =
        std::env::var_os("TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT").is_some();
    let fresh = dashboard_cache::dist_is_fresh(repository_root, &source_stamp);

    // A packaged crate ships the prebuilt app-dist but none of the frontend
    // sources the stamp is computed from, so the stamp can never match there
    // and `fresh` is always false. Rebuilding is impossible in that tree — npm
    // would run in a directory with no package.json — so treat the packaged
    // dist as authoritative whenever the sources are absent.
    let sources_present = Path::new("dashboard/package.json").exists();
    if !fresh && !sources_present {
        assert!(
            app_dist.join("index.html").exists(),
            "dashboard/app-dist/index.html is missing and dashboard sources are not packaged; \
             the published crate must ship a prebuilt dashboard/app-dist"
        );
    } else if !fresh {
        if contract_schema_export {
            println!("cargo::warning=skipping dashboard asset build for contract schema export");
        } else if std::env::var_os("TRACEDECAY_SKIP_DASHBOARD_BUILD").is_some() {
            println!(
                "cargo::warning=dashboard app-dist is stale but TRACEDECAY_SKIP_DASHBOARD_BUILD is set; embedding existing dist"
            );
        } else {
            if !dashboard.join("node_modules").exists() {
                run_npm(dashboard, &["ci"]);
            }
            run_npm(dashboard, &["run", "build"]);
            fs::write(&stamp_path, &source_stamp)
                .unwrap_or_else(|e| panic!("failed to write app-dist stamp: {e}"));
        }
    }
    assert!(
        contract_schema_export || app_dist.join("index.html").exists(),
        "dashboard/app-dist/index.html is missing after build; the dashboard frontend build failed"
    );
    if let Some(path) = std::env::var_os("TRACEDECAY_DOGFOOD_DASHBOARD_STAMP_PATH") {
        fs::write(Path::new(&path), &source_stamp).unwrap_or_else(|e| {
            panic!(
                "failed to write dogfood dashboard source stamp {}: {e}",
                Path::new(&path).display()
            )
        });
    }

    // Generated manifest: one embedded entry per dist file.
    let mut code = String::from(
        "pub struct AppAsset { pub path: &'static str, pub contents: &'static [u8], pub content_type: &'static str }\n",
    );
    let mut app_hasher = DefaultHasher::new();
    let _ = writeln!(code, "pub const APP_ASSETS: &[AppAsset] = &[");
    for relative in collect_files_relative(&app_dist) {
        if relative == ".source-stamp" {
            continue;
        }
        println!("cargo::rerun-if-changed=dashboard/app-dist/{relative}");
        relative.hash(&mut app_hasher);
        if let Ok(bytes) = fs::read(app_dist.join(&relative)) {
            bytes.hash(&mut app_hasher);
        }
        let content_type = match relative.rsplit('.').next().unwrap_or("") {
            "html" => "text/html; charset=utf-8",
            "js" | "mjs" => "application/javascript",
            "css" => "text/css",
            "json" | "map" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff2" => "font/woff2",
            "woff" => "font/woff",
            "ttf" => "font/ttf",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        let _ = writeln!(
            code,
            "    AppAsset {{ path: {relative:?}, contents: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/dashboard/app-dist/{relative}\")), content_type: {content_type:?} }},"
        );
    }
    code.push_str("];\n");
    let app_stamp = format!("{:016x}", app_hasher.finish());
    let _ = writeln!(code, "pub const APP_ASSET_STAMP: &str = {app_stamp:?};");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let out = Path::new(&out_dir).join("dashboard_app_assets.rs");
    if !matches!(fs::read_to_string(&out), Ok(current) if current == code) {
        fs::write(&out, code).unwrap_or_else(|e| panic!("failed to write app asset manifest: {e}"));
    }
}

fn run_npm(dir: &Path, args: &[&str]) {
    let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run npm {}: {e}", args.join(" ")));
    assert!(
        status.success(),
        "npm {} failed in {} (status {status}); the dashboard frontend must build for the binary to embed it",
        args.join(" "),
        dir.display()
    );
}

fn main() {
    build_and_embed_dashboard_app();
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

    // Build identity: the commit this binary is compiled from and whether the
    // worktree was clean. Feeds the generated agent plugins' provenance header
    // (so a stale installed plugin is distinguishable from the binary that
    // should have generated it) and the SemVer build metadata the binary
    // reports as its own version. Git metadata tracks commits and staging;
    // dogfood supplies a refreshed stamp for unstaged and untracked changes.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DOGFOOD_BUILD_IDENTITY_STAMP");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DOGFOOD_BUILD_IDENTITY_REFRESH");
    if let Some(path) = std::env::var_os("TRACEDECAY_DOGFOOD_BUILD_IDENTITY_STAMP") {
        println!("cargo::rerun-if-changed={}", Path::new(&path).display());
    }
    let identity = resolve(Path::new(&manifest_dir));
    for path in watch_paths(Path::new(&manifest_dir)) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    println!("cargo::rerun-if-changed=src/version/build_identity.rs");
    println!(
        "cargo::rustc-env=TRACEDECAY_GIT_SHA={}",
        identity.sha.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo::rustc-env=TRACEDECAY_GIT_DIRTY={}",
        u8::from(identity.dirty)
    );
}
