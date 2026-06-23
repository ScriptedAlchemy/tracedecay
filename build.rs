use std::hash::{Hash, Hasher};
use std::process::{Command, Stdio};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    path::{Path, PathBuf},
};

const DASHBOARD_ASSET_FILES: &[&str] = &[
    "dashboard/shell/dist/shell.js",
    "dashboard/shell/dist/shell.css",
    "dashboard/shell/dist/source-stamp",
    "dashboard/holographic/dist/index.js",
    "dashboard/holographic/dist/style.css",
    "dashboard/lcm/dist/index.js",
    "dashboard/lcm/dist/style.css",
    "dashboard/graph/dist/index.js",
    "dashboard/graph/dist/style.css",
    "dashboard/savings/dist/index.js",
    "dashboard/savings/dist/style.css",
];

const DASHBOARD_SOURCE_FILES: &[&str] = &[
    "dashboard/build.mjs",
    "dashboard/build.shared.mjs",
    "dashboard/package.json",
    "dashboard/package-lock.json",
];

const DASHBOARD_SOURCE_DIRS: &[&str] = &[
    "dashboard/graph/src",
    "dashboard/holographic/src",
    "dashboard/lcm/src",
    "dashboard/lib",
    "dashboard/savings/src",
    "dashboard/shell/src",
];

const DASHBOARD_DIST_SOURCE_STAMP: &str = "dashboard/shell/dist/source-stamp";
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

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
/// build`) when dist assets are missing or stale, so plain `cargo build` / `cargo
/// install --path .` work from a fresh checkout. Published crates ship the
/// prebuilt dist files (see `package.include` in Cargo.toml), so this never
/// runs for crates.io builds unless those packaged files are incomplete.
fn auto_build_dashboard_assets(reason: &str, affected: &[&str]) {
    let fail_fast = |detail: &str| -> ! {
        let affected = if affected.is_empty() {
            "dashboard source files changed since the embedded dist assets were built".to_string()
        } else {
            affected.join("\n  ")
        };
        panic!(
            "\n\ndashboard dist assets are {reason}:\n  {affected}\n\n\
             The dashboard UI is embedded into the binary at compile time\n\
             (src/dashboard/assets.rs), so the frontend must be built first:\n\n  \
             cd dashboard && npm ci && npm run build\n\n{detail}\n",
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

    // Automatic rebuilds must refresh dependencies even when `node_modules`
    // already exists: a pulled package-lock change can add build-time imports
    // that the stale install does not contain yet.
    if let Err(ci_err) = run_npm(npm, &["ci"], dashboard_dir) {
        println!("cargo::warning=dashboard assets: npm ci failed, retrying with npm install");
        if let Err(install_err) = run_npm(npm, &["install"], dashboard_dir) {
            fail_fast(&format!(
                "automatic dependency install failed.\n\nnpm ci:\n{ci_err}\n\nnpm install:\n{install_err}"
            ));
        }
    }
    if let Err(build_err) = run_npm(npm, &["run", "build"], dashboard_dir) {
        fail_fast(&format!("automatic dashboard build failed.\n\n{build_err}"));
    }
    println!("cargo::warning=dashboard assets: npm build finished; embedding fresh dist files");
}

fn fnv_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn normalized_dashboard_source_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Content hash of the production dashboard source inputs (each input's path +
/// bytes), independent of filesystem mtimes. Returns `None` when there are no
/// source inputs - e.g. a published crate that ships only the prebuilt dist -
/// so a stamp is never recorded and a rebuild is never triggered for crates.io.
fn dashboard_source_stamp(source_inputs: &[PathBuf]) -> Option<String> {
    if source_inputs.is_empty() {
        return None;
    }
    // Hash in a stable path order so the stamp depends only on file content,
    // not on the unspecified `read_dir` traversal order. Sort by the same
    // normalized forward-slash string key the JS builder uses
    // (build.shared.mjs `normalizedSourcePath`) so the two stamps stay
    // byte-identical; `PathBuf`'s default component-wise ordering can diverge
    // from JS string ordering.
    let mut paths: Vec<&PathBuf> = source_inputs.iter().collect();
    paths.sort_by(|a, b| {
        normalized_dashboard_source_path(a).cmp(&normalized_dashboard_source_path(b))
    });
    let mut hasher = FNV_OFFSET_BASIS;
    for path in paths {
        // Hashing the path makes adds/removes/renames flip the stamp even when
        // the surviving files are byte-identical.
        fnv_hash_bytes(
            &mut hasher,
            normalized_dashboard_source_path(path).as_bytes(),
        );
        fnv_hash_bytes(&mut hasher, &[0]);
        if let Ok(bytes) = fs::read(path) {
            fnv_hash_bytes(&mut hasher, &bytes);
        }
        fnv_hash_bytes(&mut hasher, &[0]);
    }
    Some(format!("{hasher:016x}"))
}

/// True when the dashboard source inputs differ from the content stamp recorded
/// by the previous build in this `OUT_DIR` - i.e. the sources genuinely changed
/// rather than just having their mtimes rewritten by a `git checkout`/`pull`.
///
/// A build with no recorded stamp (a fresh checkout, a clean target dir, or a
/// crates.io build that ships only dist) returns false here; the dist-carried
/// source stamp is checked separately before this OUT_DIR fallback is used.
fn dashboard_sources_changed(current_stamp: Option<&str>) -> bool {
    let Some(current) = current_stamp else {
        return false;
    };
    match read_dashboard_source_stamp() {
        Some(previous) => previous != current,
        None => false,
    }
}

/// True when the committed/generated dist was built from a different set of
/// production sources. Unlike the OUT_DIR stamp, this survives `cargo clean`
/// because `npm run build` writes it next to the dist assets that Cargo embeds.
fn dashboard_dist_stale(current_stamp: Option<&str>) -> bool {
    let Some(current) = current_stamp else {
        return false;
    };
    match fs::read_to_string(DASHBOARD_DIST_SOURCE_STAMP) {
        Ok(contents) => contents.trim() != current,
        Err(_) => true,
    }
}

/// Location of the persisted source stamp inside cargo's `OUT_DIR`. Keeping it
/// in the build output (never the source tree) keeps `cargo package`
/// verification - which forbids build scripts from editing tracked files -
/// happy.
fn dashboard_source_stamp_path() -> Option<PathBuf> {
    let out_dir = std::env::var_os("OUT_DIR")?;
    Some(Path::new(&out_dir).join("dashboard-source-stamp"))
}

fn read_dashboard_source_stamp() -> Option<String> {
    let contents = fs::read_to_string(dashboard_source_stamp_path()?).ok()?;
    let stamp = contents.trim();
    (!stamp.is_empty()).then(|| stamp.to_string())
}

fn store_dashboard_source_stamp(stamp: &str) {
    // Best-effort: if the stamp can't be written, the next build still checks
    // the source stamp that `npm run build` wrote next to the dist assets.
    if let Some(path) = dashboard_source_stamp_path() {
        let _ = fs::write(path, stamp);
    }
}

fn collect_dashboard_source_inputs() -> Vec<PathBuf> {
    let mut inputs = Vec::new();
    for relative in DASHBOARD_SOURCE_FILES {
        println!("cargo::rerun-if-changed={relative}");
        let path = PathBuf::from(relative);
        if path.is_file() {
            inputs.push(path);
        }
    }
    for relative in DASHBOARD_SOURCE_DIRS {
        println!("cargo::rerun-if-changed={relative}");
        collect_dashboard_source_dir(Path::new(relative), &mut inputs);
    }
    inputs
}

fn collect_dashboard_source_dir(dir: &Path, inputs: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dashboard_source_dir(&path, inputs);
        } else if path.is_file() {
            println!("cargo::rerun-if-changed={}", path.display());
            inputs.push(path);
        }
    }
}

fn emit_dashboard_asset_inputs() -> String {
    let source_inputs = collect_dashboard_source_inputs();
    let missing: Vec<&str> = DASHBOARD_ASSET_FILES
        .iter()
        .copied()
        .filter(|relative| !Path::new(relative).exists())
        .collect();
    let source_stamp = dashboard_source_stamp(&source_inputs);
    if !missing.is_empty() {
        auto_build_dashboard_assets("missing", &missing);
    } else if dashboard_dist_stale(source_stamp.as_deref())
        || dashboard_sources_changed(source_stamp.as_deref())
    {
        auto_build_dashboard_assets("stale", &[]);
    }
    // Record the source content hash we just accepted so the next build can
    // distinguish a genuine source edit from a mtime-only churn (git
    // checkout/pull). Skipped when no source inputs ship (crates.io).
    if let Some(stamp) = source_stamp.as_deref() {
        store_dashboard_source_stamp(stamp);
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
