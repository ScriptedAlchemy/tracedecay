//! Root build script: embeds the dashboard dist, renders the CLI logo, and
//! bakes the build's commit identity. The plugin-bundle manifest moved to
//! `crates/tracedecay-agent-hosts/build.rs` with the `agents`/`automation`
//! subsystems.
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
    // The plugin-bundle manifest (`$OUT_DIR/plugin_bundle_generated.rs`) moved
    // to `crates/tracedecay-agent-hosts/build.rs` along with its only consumer,
    // `agents::plugin_bundle`. Its `plugin/`-relative paths are rebased there.
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
    // reports as its own version. Git metadata tracks commits and staging.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
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
