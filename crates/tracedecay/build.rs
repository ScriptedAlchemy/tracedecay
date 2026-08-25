//! Composition-library build script: embeds the dashboard dist and bakes the build's commit
//! identity. The plugin-bundle manifest moved to
//! `crates/tracedecay-agent-hosts/build.rs` with the `agents`/`automation`
//! subsystems.
//!
//! Rerun-edge contract. Cargo recompiles the composition crate whenever this
//! script reruns, regardless of whether the generated output changed, so every
//! `rerun-if-changed` path below costs a full root rebuild when it moves and
//! must be load-bearing. Two rules follow:
//!
//! - This script must never watch `dashboard/app-dist`, which Rsbuild cleans
//!   and rewrites. Only frontend source and configuration inputs are watched.
//! - Moving this dashboard generator into a dependency would not shield the
//!   composition crate from churn: a dependency's build-script rerun recompiles its
//!   dependents unconditionally.

use std::{
    collections::hash_map::DefaultHasher,
    error::Error,
    fmt::Write as _,
    fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
};

#[path = "build-support/dashboard_manifest.rs"]
mod dashboard_manifest;

// Shared with the crate as `tracedecay::version::build_identity`, so the probe
// that bakes the build's commit identity is the code its unit tests exercise
// rather than a second copy that can drift.
include!("src/version/build_identity.rs");

const DASHBOARD_BUILD_INPUTS: &[&str] = &[
    "dashboard/src",
    "dashboard/codegen/schemas",
    "dashboard/package.json",
    "dashboard/package-lock.json",
    "dashboard/postcss.config.mjs",
    "dashboard/rsbuild.config.ts",
    "dashboard/tsconfig.json",
];

const REPOSITORY_ROOT_FROM_CRATE: &str = "../..";

fn repository_root(manifest_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let checked_out_root = manifest_dir.join(REPOSITORY_ROOT_FROM_CRATE);
    let repository_root =
        if manifest_dir.join("dashboard").is_dir() && manifest_dir.join("plugin").is_dir() {
            manifest_dir.to_path_buf()
        } else {
            checked_out_root
        };
    for sentinel in ["Cargo.toml", "dashboard", "plugin"] {
        if !repository_root.join(sentinel).exists() {
            return Err(format!(
                "TraceDecay repository root {} is missing required sentinel {sentinel}",
                repository_root.display()
            )
            .into());
        }
    }
    Ok(repository_root)
}

/// Builds the dashboard with Rsbuild when frontend sources are present, then
/// embeds exactly the files listed in Rsbuild's asset manifest. Packaged crates
/// omit the frontend sources and consume their prebuilt app-dist as-is.
fn build_and_embed_dashboard_app(
    manifest_dir: &Path,
    repository_root: &Path,
) -> Result<(), Box<dyn Error>> {
    let package_local_dashboard = manifest_dir.join("dashboard");
    let (dashboard, include_root) = if package_local_dashboard.is_dir() {
        (package_local_dashboard, "/dashboard/app-dist")
    } else {
        (
            repository_root.join("dashboard"),
            "/../../dashboard/app-dist",
        )
    };
    let app_dist = dashboard.join("app-dist");

    for input in DASHBOARD_BUILD_INPUTS {
        println!(
            "cargo::rerun-if-changed={}",
            repository_root.join(input).display()
        );
    }
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_SKIP_DASHBOARD_BUILD");
    let contract_schema_export =
        std::env::var_os("TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT").is_some();
    let skip_dashboard_build = std::env::var_os("TRACEDECAY_SKIP_DASHBOARD_BUILD").is_some();
    let sources_present = dashboard.join("package.json").is_file();

    let asset_paths = if contract_schema_export {
        println!("cargo::warning=skipping dashboard asset build for contract schema export");
        Vec::new()
    } else {
        if sources_present && skip_dashboard_build {
            println!(
                "cargo::warning=TRACEDECAY_SKIP_DASHBOARD_BUILD is set; embedding existing dashboard app-dist"
            );
        } else if sources_present {
            if !dashboard.join("node_modules").is_dir() {
                run_npm(&dashboard, &["ci"])?;
            }
            run_npm(&dashboard, &["run", "build"])?;
        }
        dashboard_manifest::dashboard_asset_paths(&app_dist)?
    };

    let mut code = String::from(
        "pub struct AppAsset { pub path: &'static str, pub contents: &'static [u8], pub content_type: &'static str }\n",
    );
    let mut app_hasher = DefaultHasher::new();
    let _ = writeln!(code, "pub const APP_ASSETS: &[AppAsset] = &[");
    for relative in asset_paths {
        relative.hash(&mut app_hasher);
        let bytes = fs::read(app_dist.join(&relative))?;
        bytes.hash(&mut app_hasher);
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
        let include_path = format!("{include_root}/{relative}");
        let _ = writeln!(
            code,
            "    AppAsset {{ path: {relative:?}, contents: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {include_path:?})), content_type: {content_type:?} }},"
        );
    }
    code.push_str("];\n");
    let app_stamp = format!("{:016x}", app_hasher.finish());
    let _ = writeln!(code, "pub const APP_ASSET_STAMP: &str = {app_stamp:?};");

    let out_dir = std::env::var("OUT_DIR")?;
    let out = Path::new(&out_dir).join("dashboard_app_assets.rs");
    if !matches!(fs::read_to_string(&out), Ok(current) if current == code) {
        fs::write(&out, code)?;
    }
    Ok(())
}

fn run_npm(dir: &Path, args: &[&str]) -> io::Result<()> {
    let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|error| {
            io::Error::other(format!("failed to run npm {}: {error}", args.join(" ")))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "npm {} failed in {} (status {status}); the dashboard frontend must build for the binary to embed it",
            args.join(" "),
            dir.display()
        )))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let manifest_dir = Path::new(&manifest_dir);
    let repository_root = repository_root(manifest_dir)?;
    build_and_embed_dashboard_app(manifest_dir, &repository_root)?;
    // The plugin-bundle manifest (`$OUT_DIR/plugin_bundle_generated.rs`) moved
    // to `crates/tracedecay-agent-hosts/build.rs` along with its only consumer,
    // `agents::plugin_bundle`. Its `plugin/`-relative paths are rebased there.
    // Build identity: the commit this binary is compiled from and whether the
    // worktree was clean. Feeds the generated agent plugins' provenance header
    // (so a stale installed plugin is distinguishable from the binary that
    // should have generated it) and the SemVer build metadata the binary
    // reports as its own version. Git metadata tracks commits and staging.
    let identity = resolve(&repository_root);
    for path in watch_paths(&repository_root) {
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
    Ok(())
}
