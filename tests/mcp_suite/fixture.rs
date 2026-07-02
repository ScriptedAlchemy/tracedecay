//! Cross-process template store for MCP suite fixtures.
//!
//! nextest runs every test in its own process, so per-process caches cannot
//! amortize the fixed cost of `TraceDecay::init` (graph-DB schema creation,
//! global-DB schema creation) that almost every test in this suite pays.
//! This module builds a fully initialized — and, for `setup_project`, fully
//! indexed — store **once per target directory** on disk, and every test
//! process seeds its own isolated copy from that template instead of
//! re-running schema creation and indexing. Schema creation is the dominant
//! per-test fixed cost on Windows CI.
//!
//! The graph DB only stores project-root-relative paths, so a copied store is
//! location-independent; the two files that embed absolute paths
//! (`config.json` `root_dir` and `store_manifest.json`) are rewritten after
//! copying. Source-file mtimes are preserved so staleness checks see the
//! copied project exactly as the template indexer left it.
//!
//! Every entry point falls back to the real `TraceDecay::init` path when the
//! template cannot be built or the seeded store cannot be opened, so tests
//! never depend on the template for correctness.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::sync::OnceCell;
use tracedecay::errors::Result as TdResult;
use tracedecay::storage::{default_profile_project_id, default_profile_root};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::common::GLOBAL_DB_ENV;

/// Bump when the template layout or fixture sources change, so stale
/// templates from previous revisions in a cached target dir are ignored.
const TEMPLATE_DIR_NAME: &str = "mcp-suite-store-template-v1";

const EMPTY_FLAVOR: &str = "empty";
const INDEXED_FLAVOR: &str = "indexed";

static TEMPLATE_ROOT: OnceCell<Option<PathBuf>> = OnceCell::const_new();

/// Writes the shared `setup_project` fixture sources: cross-file calls,
/// structs, impls, a test file, and doc comments.
pub fn write_indexed_fixture_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/main.rs"),
        r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#,
    )
    .unwrap();

    fs::write(
        project.join("src/utils.rs"),
        r#"
/// Returns a greeting string.
pub fn helper() -> String {
    format_greeting("world")
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#,
    )
    .unwrap();

    // Test file so affected-tests can find something
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests/test_utils.rs"),
        r#"
use crate::utils::helper;

#[test]
fn test_helper() { assert!(!helper().is_empty()); }
"#,
    )
    .unwrap();
}

/// Drop-in replacement for `TraceDecay::init(project)` in tests: seeds an
/// initialized (schema-complete, empty) store from the on-disk template and
/// opens it. Falls back to the real init when seeding is not possible.
pub async fn init_project_from_template(project_root: &Path) -> TdResult<TraceDecay> {
    if let Some(template) = template_root().await {
        if let Some(targets) = SeedTargets::from_env() {
            if seed_store(&template.join(EMPTY_FLAVOR), project_root, &targets).is_ok() {
                if let Ok(cg) = TraceDecay::open(project_root).await {
                    return Ok(cg);
                }
            }
        }
    }
    TraceDecay::init(project_root).await
}

/// Like [`init_project_from_template`] but for callers that pass an explicit
/// profile root via `TraceDecayOpenOptions` instead of env vars.
pub async fn init_project_from_template_with_options(
    project_root: &Path,
    options: TraceDecayOpenOptions,
) -> TdResult<TraceDecay> {
    if let (Some(template), Some(targets)) =
        (template_root().await, SeedTargets::from_options(&options))
    {
        if seed_store(&template.join(EMPTY_FLAVOR), project_root, &targets).is_ok() {
            if let Ok(cg) = TraceDecay::open_with_options(project_root, options.clone()).await {
                return Ok(cg);
            }
        }
    }
    TraceDecay::init_with_options(project_root, options).await
}

/// Seeds the fully indexed `setup_project` fixture (sources + graph data)
/// into `project_root` and opens it. Returns `None` when the template is
/// unavailable so the caller can run the real init+index path.
pub async fn open_indexed_project_from_template(project_root: &Path) -> Option<TraceDecay> {
    let template = template_root().await?;
    let targets = SeedTargets::from_env()?;
    let flavor = template.join(INDEXED_FLAVOR);
    copy_tree(&flavor.join("project"), project_root).ok()?;
    seed_store(&flavor, project_root, &targets).ok()?;
    TraceDecay::open(project_root).await.ok()
}

struct SeedTargets {
    profile_root: PathBuf,
    global_db_path: PathBuf,
}

impl SeedTargets {
    fn from_env() -> Option<Self> {
        let profile_root = default_profile_root().ok()?;
        let global_db_path = std::env::var_os(GLOBAL_DB_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| profile_root.join("global.db"));
        Some(Self {
            profile_root,
            global_db_path,
        })
    }

    fn from_options(options: &TraceDecayOpenOptions) -> Option<Self> {
        let profile_root = options.profile_root.clone()?;
        let global_db_path = options
            .global_db_path
            .clone()
            .unwrap_or_else(|| profile_root.join("global.db"));
        Some(Self {
            profile_root,
            global_db_path,
        })
    }
}

/// Copies one template flavor's store into place for `project_root`:
/// data dir under the target profile root, global DB (only if absent), and
/// rewrites the absolute paths embedded in config + manifest.
fn seed_store(flavor: &Path, project_root: &Path, targets: &SeedTargets) -> io::Result<()> {
    fs::create_dir_all(project_root)?;
    let src_home = flavor.join("home/.tracedecay");
    let src_data = sole_subdir(&src_home.join("projects"))?;

    let project_id = default_profile_project_id(project_root);
    let data_dest = targets.profile_root.join("projects").join(&project_id);
    if data_dest.exists() {
        // A store already exists for this project (e.g. re-init); let the
        // caller fall back to the real init path.
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "store data dir already exists",
        ));
    }
    copy_tree(&src_data, &data_dest)?;

    rewrite_json(&data_dest.join("config.json"), |config| {
        config["root_dir"] = Value::String(project_root.to_string_lossy().into_owned());
    })?;
    rewrite_json(&data_dest.join("store_manifest.json"), |manifest| {
        manifest["project_id"] = Value::String(project_id.clone());
        manifest["project_root"] = Value::String(project_root.to_string_lossy().into_owned());
        manifest["data_root"] = Value::String(data_dest.to_string_lossy().into_owned());
    })?;

    if !targets.global_db_path.exists() {
        if let Some(parent) = targets.global_db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_db_files(&src_home.join("global.db"), &targets.global_db_path)?;
    }
    Ok(())
}

async fn template_root() -> Option<&'static Path> {
    TEMPLATE_ROOT
        .get_or_init(|| async { ensure_template().await })
        .await
        .as_deref()
}

/// Returns the shared template dir, building it if this is the first test
/// process to need it. nextest runs one process per test, so an exclusive
/// file lock serializes the build machine-wide: exactly one process builds,
/// every concurrent process blocks briefly and then finds READY.
async fn ensure_template() -> Option<PathBuf> {
    let tmp_root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let shared = tmp_root.join(TEMPLATE_DIR_NAME);
    if shared.join("READY").is_file() {
        return Some(shared);
    }

    fs::create_dir_all(tmp_root).ok()?;
    let lock_path = tmp_root.join(format!("{TEMPLATE_DIR_NAME}.lock"));
    let lock_file = tokio::task::spawn_blocking(move || -> io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await
    .ok()?
    .ok()?;

    // Another process may have finished the build while we waited.
    if shared.join("READY").is_file() {
        let _ = fs2::FileExt::unlock(&lock_file);
        return Some(shared);
    }

    let build = shared.with_file_name(format!("{TEMPLATE_DIR_NAME}-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&build);
    let built = build_template(&build).await;
    let result = match built {
        Ok(()) => match fs::rename(&build, &shared) {
            Ok(()) => Some(shared),
            // Rename failed (e.g. leftover partial dir); the private build
            // tree is still a valid template for this process.
            Err(_) if shared.join("READY").is_file() => {
                let _ = fs::remove_dir_all(&build);
                Some(shared)
            }
            Err(_) => Some(build),
        },
        Err(err) => {
            eprintln!(
                "[mcp_suite::fixture] template build failed, falling back to real init: {err}"
            );
            let _ = fs::remove_dir_all(&build);
            None
        }
    };
    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

async fn build_template(dest: &Path) -> io::Result<()> {
    // Build in a system temp dir, not under the repository's target/, so
    // branch detection walking up from the fixture project cannot find this
    // repo's .git and bootstrap branch metadata that a TempDir-based test
    // project would never have.
    let scratch = tempfile::TempDir::new()?;

    for (flavor, indexed) in [(EMPTY_FLAVOR, false), (INDEXED_FLAVOR, true)] {
        let root = scratch.path().join(flavor);
        let project = root.join("project");
        fs::create_dir_all(&project)?;
        if indexed {
            write_indexed_fixture_sources(&project);
        }

        let profile_root = root.join("home/.tracedecay");
        let global_db_path = profile_root.join("global.db");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(global_db_path.clone()),
        };
        let cg = TraceDecay::init_with_options(&project, options)
            .await
            .map_err(io_other)?;
        if indexed {
            cg.index_all().await.map_err(io_other)?;
        }
        cg.checkpoint().await.map_err(io_other)?;
        cg.close();

        purge_global_registry(&global_db_path).await?;
        copy_tree(&root, &dest.join(flavor))?;
    }

    fs::write(dest.join("READY"), b"ok")?;
    Ok(())
}

/// Removes the template project's own registration from the template global
/// DB so seeded copies start with a schema-complete but empty registry;
/// each test's `TraceDecay::open` re-registers its own project cleanly.
async fn purge_global_registry(global_db_path: &Path) -> io::Result<()> {
    let db = libsql::Builder::new_local(global_db_path)
        .build()
        .await
        .map_err(io_other)?;
    let conn = db.connect().map_err(io_other)?;
    conn.execute_batch(
        "DELETE FROM store_artifacts;
         DELETE FROM graph_scopes;
         DELETE FROM store_instances;
         DELETE FROM project_aliases;
         DELETE FROM code_projects;
         DELETE FROM projects;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .await
    .map_err(io_other)?;
    Ok(())
}

fn sole_subdir(dir: &Path) -> io::Result<PathBuf> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    match (entries.pop(), entries.pop()) {
        (Some(path), None) if path.is_dir() => Ok(path),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected exactly one store dir under {}", dir.display()),
        )),
    }
}

/// Recursively copies `src` into `dest`, preserving file mtimes so the
/// staleness pipeline sees copied sources exactly as the template indexed
/// them.
fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            copy_file_preserving_mtime(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_file_preserving_mtime(src: &Path, dest: &Path) -> io::Result<()> {
    fs::copy(src, dest)?;
    let modified = fs::metadata(src)?.modified()?;
    let file = fs::OpenOptions::new().write(true).open(dest)?;
    file.set_times(fs::FileTimes::new().set_modified(modified))?;
    Ok(())
}

/// Copies a SQLite database file together with any `-wal`/`-shm` sidecars.
fn copy_db_files(src: &Path, dest: &Path) -> io::Result<()> {
    fs::copy(src, dest)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = sibling_with_suffix(src, suffix);
        if sidecar.exists() {
            fs::copy(&sidecar, sibling_with_suffix(dest, suffix))?;
        }
    }
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn rewrite_json(path: &Path, edit: impl FnOnce(&mut Value)) -> io::Result<()> {
    let mut value: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    edit(&mut value);
    fs::write(path, serde_json::to_string_pretty(&value)?)
}

fn io_other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}
