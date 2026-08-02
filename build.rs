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
    "dashboard/code-diagnostics/dist/index.js",
    "dashboard/code-diagnostics/dist/style.css",
    "dashboard/savings/dist/index.js",
    "dashboard/savings/dist/style.css",
    "dashboard/settings/dist/index.js",
    "dashboard/settings/dist/style.css",
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
    "dashboard/code-diagnostics/src",
    "dashboard/lib",
    "dashboard/savings/src",
    "dashboard/settings/src",
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
    code.push_str(&format!(
        "/// Every UTF-8 file under `plugin/{source_prefix}/`.\n\
         pub const {const_name}: &[PluginFile] = &[\n"
    ));
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
        code.push_str(&format!(
            "    PluginFile {{ relative: {deploy:?}, contents: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/plugin/{source}\")) }},\n"
        ));
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
    code.push_str(&format!("pub const {const_name}: &[PluginFile] = &[\n"));
    for (relative, contents) in files {
        code.push_str(&format!(
            "    PluginFile {{ relative: {relative:?}, contents: {contents:?} }},\n"
        ));
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
}
