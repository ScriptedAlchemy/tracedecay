use std::hash::{Hash, Hasher};
use std::process::{Command, Stdio};
use std::{collections::hash_map::DefaultHasher, fs, path::Path};

const DASHBOARD_ASSET_FILES: &[&str] = &[
    "dashboard/shell/dist/shell.js",
    "dashboard/shell/dist/shell.css",
    "dashboard/holographic/dist/index.js",
    "dashboard/holographic/dist/style.css",
    "dashboard/lcm/dist/index.js",
    "dashboard/lcm/dist/style.css",
    "dashboard/graph/dist/index.js",
    "dashboard/graph/dist/style.css",
    "dashboard/savings/dist/index.js",
    "dashboard/savings/dist/style.css",
];

/// Locates a working npm executable (`npm.cmd` is the Windows launcher).
fn npm_program() -> Option<&'static str> {
    ["npm", "npm.cmd"].into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

fn run_npm(npm: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    println!(
        "cargo::warning=dashboard assets: running `{npm} {}` in {} (this can take a minute on first build)",
        args.join(" "),
        dir.display()
    );
    let output = Command::new(npm)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{npm} {}`: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let tail = combined
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Err(format!(
        "`{npm} {}` failed with {} in {}:\n{tail}",
        args.join(" "),
        output.status,
        dir.display()
    ))
}

/// Builds the dashboard frontend (`cd dashboard && npm ci/install && npm run
/// build`) when dist assets are missing, so plain `cargo build` / `cargo
/// install --path .` work from a fresh checkout. Published crates ship the
/// prebuilt dist files (see `package.include` in Cargo.toml), so this never
/// runs for crates.io builds.
fn auto_build_dashboard_assets(missing: &[&str]) {
    let fail_fast = |detail: &str| -> ! {
        panic!(
            "\n\nmissing dashboard dist assets:\n  {}\n\n\
             The dashboard UI is embedded into the binary at compile time\n\
             (src/dashboard/assets.rs), so the frontend must be built first:\n\n  \
             cd dashboard && npm ci && npm run build\n\n{detail}\n",
            missing.join("\n  ")
        );
    };

    let dashboard_dir = Path::new("dashboard");
    if !dashboard_dir.join("package.json").exists() {
        fail_fast("dashboard/package.json not found; cannot build the assets automatically.");
    }
    let Some(npm) = npm_program() else {
        fail_fast(
            "npm was not found on PATH, so the build could not produce them \
             automatically.\nInstall Node.js 22+ (https://nodejs.org) and re-run the build.",
        );
    };

    if !dashboard_dir.join("node_modules").exists() {
        // `npm ci` needs the lockfile to match package.json; fall back to
        // `npm install` so a stale lockfile doesn't hard-fail the Rust build.
        if let Err(ci_err) = run_npm(npm, &["ci"], dashboard_dir) {
            println!("cargo::warning=dashboard assets: npm ci failed, retrying with npm install");
            if let Err(install_err) = run_npm(npm, &["install"], dashboard_dir) {
                fail_fast(&format!(
                    "automatic dependency install failed.\n\nnpm ci:\n{ci_err}\n\nnpm install:\n{install_err}"
                ));
            }
        }
    }
    if let Err(build_err) = run_npm(npm, &["run", "build"], dashboard_dir) {
        fail_fast(&format!("automatic dashboard build failed.\n\n{build_err}"));
    }
    println!("cargo::warning=dashboard assets: npm build finished; embedding fresh dist files");
}

fn emit_dashboard_asset_inputs() -> String {
    let missing: Vec<&str> = DASHBOARD_ASSET_FILES
        .iter()
        .copied()
        .filter(|relative| !Path::new(relative).exists())
        .collect();
    if !missing.is_empty() {
        auto_build_dashboard_assets(&missing);
    }

    let mut hasher = DefaultHasher::new();
    let mut still_missing = Vec::new();
    for relative in DASHBOARD_ASSET_FILES {
        println!("cargo::rerun-if-changed={relative}");
        relative.hash(&mut hasher);
        match fs::read(relative) {
            Ok(bytes) => bytes.hash(&mut hasher),
            Err(_) => still_missing.push(*relative),
        }
    }
    if !still_missing.is_empty() {
        panic!(
            "\n\ndashboard dist assets still missing after the automatic npm build:\n  {}\n\n\
             Build them manually and inspect the output:\n\n  \
             cd dashboard && npm ci && npm run build\n",
            still_missing.join("\n  ")
        );
    }
    format!("{:016x}", hasher.finish())
}

fn main() {
    let out_path = Path::new("src/resources/logo.ansi");
    let logo_bytes = include_bytes!("src/resources/logo.png");
    let ansi = logo_art::image_to_ansi(logo_bytes, 90);
    // Only rewrite when the content differs: `cargo package` verification
    // rejects packages whose build script modifies files in the source dir.
    if !matches!(fs::read(out_path), Ok(current) if current == ansi.as_bytes()) {
        if let Err(e) = fs::write(out_path, &ansi) {
            panic!("failed to write {}: {e}", out_path.display());
        }
    }
    println!("cargo::rerun-if-changed=src/resources/logo.png");
    let asset_stamp = emit_dashboard_asset_inputs();
    println!("cargo::rustc-env=TRACEDECAY_DASHBOARD_ASSET_STAMP={asset_stamp}");
    // Legacy compatibility for any older embedded consumers still checking the
    // pre-rename stamp name. The runtime dashboard reads TRACEDECAY_*.
    println!("cargo::rustc-env=TOKENSAVE_DASHBOARD_ASSET_STAMP={asset_stamp}");

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
