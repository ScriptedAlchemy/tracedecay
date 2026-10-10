//! Cross-process template store for MCP suite fixtures.
//!
//! nextest runs every test in its own process, so per-process caches cannot
//! amortize the fixed cost of `TraceDecay::init` (graph-DB schema creation,
//! global-DB schema creation) that almost every test in this suite pays.
//! This module builds a fully initialized store **once per target directory**
//! on disk, and every test process seeds its own isolated copy from that
//! template instead of re-running schema creation. Schema creation is the
//! dominant per-test fixed cost on Windows CI. Code-index publication belongs
//! to the production daemon composition and is never pre-seeded here.
//!
//! The graph DB only stores project-root-relative paths, so a copied store is
//! location-independent. The profile shard's `store_manifest.json` is the file
//! that still embeds absolute paths (`project_id`, `project_root`,
//! `data_root`); it is rewritten after copying. Configuration lives in the
//! store, not a `config.json` (#2134).
//!
//! Seeding is the only bootstrap. A template that cannot be built or a copy
//! that cannot be opened is a typed error; tests do not silently pay a second
//! full init.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use tokio::sync::OnceCell;
use tracedecay_domain::errors::Result as TdResult;
use tracedecay_project::project::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_runtime_core::path_safety::{canonical_existing_identity, canonical_root_identity};
use tracedecay_runtime_core::storage::{PrivateStoreIo, default_profile_project_id};

/// Store schema versions admitted by recorded version rather than by the
/// graph-DB final shape. Init refuses a store recorded at any other version,
/// so each one must invalidate the template when it changes.
#[derive(Clone, Copy)]
struct StoreSchemaVersions {
    lcm: i64,
    session_temporal: i64,
    git_correlation: i64,
}

impl StoreSchemaVersions {
    const CURRENT: Self = Self {
        lcm: tracedecay_lcm::schema::LCM_SCHEMA_VERSION,
        session_temporal: tracedecay_session_temporal_store::SESSION_TEMPORAL_SCHEMA_VERSION,
        git_correlation:
            tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION,
    };
}

/// Shared on-disk template identity. The graph-database final shape and the
/// other admitted schema constants (authority tables, temporal columns, format
/// revisions) are fingerprinted from those constants, so a column or revision
/// change selects a different directory without a hand-maintained revision.
/// Tables admitted by a recorded version alone are keyed on [`StoreSchemaVersions`].
fn template_dir_name(versions: StoreSchemaVersions) -> Option<String> {
    schema_template_dir_name(
        versions,
        &tracedecay_global_db::schema_contract::expected_admitted_schema_fingerprint(),
    )
}

fn schema_template_dir_name(
    versions: StoreSchemaVersions,
    admitted_schema: &str,
) -> Option<String> {
    let StoreSchemaVersions {
        lcm,
        session_temporal,
        git_correlation,
    } = versions;
    match tracedecay_runtime_core::db::migrations::expected_final_schema_fingerprint() {
        Ok(fingerprint) => Some(format!(
            "mcp-suite-store-template-{fingerprint}-{admitted_schema}-lcm{lcm}-temporal{session_temporal}-git{git_correlation}"
        )),
        Err(error) => {
            eprintln!("[mcp_suite::fixture] schema fingerprint unavailable: {error}");
            None
        }
    }
}

const EMPTY_FLAVOR: &str = "empty";

static TEMPLATE_ROOT: OnceCell<std::result::Result<PathBuf, String>> = OnceCell::const_new();

/// Writes the shared production-composition fixture sources: cross-file calls,
/// structs, impls, a test file, and doc comments.
#[cfg(feature = "test-transport")]
pub fn write_indexed_fixture_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src").join("main.rs"),
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
        project.join("src").join("utils.rs"),
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
        project.join("tests").join("test_utils.rs"),
        r#"
use crate::utils::helper;

#[test]
fn test_helper() { assert!(!helper().is_empty()); }
"#,
    )
    .unwrap();
}

/// Drop-in replacement for `TraceDecay::init_with_options(project, options)`
/// in tests: seeds an initialized (schema-complete, empty) store from the
/// on-disk template into the options' profile and opens it.
pub async fn init_project_from_template_with_options(
    project_root: &Path,
    options: TraceDecayOpenOptions,
) -> TdResult<TraceDecay> {
    match template_root().await {
        Ok(template) => {
            init_project_from_template_root(Some(template), project_root, options).await
        }
        Err(error) => Err(config_error(format!(
            "mcp suite store template is unavailable: {error}"
        ))),
    }
}

async fn init_project_from_template_root(
    template: Option<&Path>,
    project_root: &Path,
    options: TraceDecayOpenOptions,
) -> TdResult<TraceDecay> {
    let template =
        template.ok_or_else(|| config_error("mcp suite store template is unavailable"))?;
    let targets = SeedTargets::from_options(&options)
        .ok_or_else(|| config_error("mcp suite store template seed requires a profile root"))?;
    seed_store(&template.join(EMPTY_FLAVOR), project_root, &targets)
        .map_err(|error| config_error(format!("mcp suite store template seed failed: {error}")))?;
    Box::pin(TraceDecay::open_with_options(project_root, options)).await
}

fn config_error(message: impl Into<String>) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

struct SeedTargets {
    profile_root: PathBuf,
    global_db_path: PathBuf,
}

impl SeedTargets {
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
/// rewrites the absolute paths embedded in `store_manifest.json`.
fn seed_store(flavor: &Path, project_root: &Path, targets: &SeedTargets) -> io::Result<()> {
    fs::create_dir_all(project_root)?;
    let src_home = flavor.join("home").join(".tracedecay");
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
    // The profile root and store data dirs are owner-private in production;
    // seed them through the same private-store authority so fail-closed
    // permission validation accepts the template copy under any umask.
    PrivateStoreIo::create_dir_all(&data_dest)?;
    copy_tree(&src_data, &data_dest)?;

    rewrite_json(&data_dest.join("store_manifest.json"), |manifest| {
        manifest["project_id"] = Value::String(project_id.clone());
        manifest["project_root"] = Value::String(project_root.to_string_lossy().into_owned());
        manifest["data_root"] = Value::String(data_dest.to_string_lossy().into_owned());
    })?;

    if !targets.global_db_path.exists() {
        if let Some(parent) = targets.global_db_path.parent() {
            PrivateStoreIo::create_dir_all(parent)?;
        }
        copy_db_files(&src_home.join("global.db"), &targets.global_db_path)?;
    }
    Ok(())
}

async fn template_root() -> std::result::Result<&'static Path, &'static str> {
    match TEMPLATE_ROOT
        .get_or_init(|| async {
            ensure_template(
                &crate::common::fixture::cargo_target_tmpdir(),
                StoreSchemaVersions::CURRENT,
            )
            .await
            .map_err(|error| error.to_string())
        })
        .await
    {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(error.as_str()),
    }
}

/// Plain, resolved directory used for the shared template tree.
///
/// Bazel Windows `TEST_TMPDIR` / `CARGO_TARGET_TMPDIR` keep `/` separators.
/// `std::fs::canonicalize` then spells `\\?\C:\...`. Joining a `/` component
/// onto that verbatim root, or asking private-fs to extend a ≥260-char path
/// that still contains `/`, is rejected as
/// `long Windows security path must have an exact absolute spelling`.
fn host_exact_template_root(path: &Path) -> PathBuf {
    canonical_root_identity(path)
}

/// Returns the shared template dir, building it if this is the first test
/// process to need it. nextest runs one process per test, so an exclusive
/// file lock serializes the build machine-wide: exactly one process builds,
/// every concurrent process blocks briefly and then finds READY.
async fn ensure_template(tmp_root: &Path, versions: StoreSchemaVersions) -> io::Result<PathBuf> {
    let tmp_root = host_exact_template_root(tmp_root);
    let template_dir_name = template_dir_name(versions).ok_or_else(|| {
        io::Error::other("mcp suite store template schema fingerprint is unavailable")
    })?;
    let shared = tmp_root.join(&template_dir_name);
    if shared.join("READY").is_file() {
        return Ok(shared);
    }

    fs::create_dir_all(&tmp_root)?;
    let lock_path = tmp_root.join(format!("{template_dir_name}.lock"));
    let lock_file = tokio::task::spawn_blocking(move || -> io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        file.lock()?;
        Ok(file)
    })
    .await
    .map_err(io::Error::other)??;

    // Another process may have finished the build while we waited.
    if shared.join("READY").is_file() {
        let _ = lock_file.unlock();
        return Ok(shared);
    }

    let build = shared.with_file_name(format!("{template_dir_name}-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&build);
    let built = build_template(&build).await;
    let result = match built {
        Ok(()) => match fs::rename(&build, &shared) {
            Ok(()) => Ok(shared),
            // Rename failed (e.g. leftover partial dir); the private build
            // tree is still a valid template for this process.
            Err(_) if shared.join("READY").is_file() => {
                let _ = fs::remove_dir_all(&build);
                Ok(shared)
            }
            Err(_) => Ok(build),
        },
        Err(err) => {
            eprintln!("[mcp_suite::fixture] template build failed: {err}");
            let _ = fs::remove_dir_all(&build);
            Err(err)
        }
    };
    let _ = lock_file.unlock();
    result
}

async fn build_template(dest: &Path) -> io::Result<()> {
    // Build in a system temp dir, not under the repository's target/, so
    // branch detection walking up from the fixture project cannot find this
    // repo's .git and bootstrap branch metadata that a TempDir-based test
    // project would never have.
    let dest = host_exact_template_root(dest);
    let scratch = tempfile::TempDir::new()?;
    // Plain host identity, not `canonicalize`'s Windows `\\?\` spelling:
    // init and private-fs joins onto a verbatim root preserve `/` and then
    // fail the exact-absolute long-path check.
    let scratch_root = canonical_existing_identity(scratch.path())?;

    let root = scratch_root.join(EMPTY_FLAVOR);
    let project = root.join("project");
    fs::create_dir_all(&project)?;

    let profile_root = root.join("home").join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(global_db_path.clone()),
    };
    let cg = Box::pin(TraceDecay::init_with_options(&project, options))
        .await
        .map_err(io_other)?;
    let sessions_db_path = cg.store_layout().sessions_db_path.clone();
    let graph_db_path = cg.store_layout().graph_db_path.clone();
    cg.checkpoint().await.map_err(io_other)?;
    cg.close();

    purge_configuration(&sessions_db_path)?;
    purge_global_registry(&global_db_path).await?;
    let built_fingerprint = sqlite_master_shape_fingerprint(&graph_db_path)?;
    let expected_fingerprint =
        tracedecay_runtime_core::db::migrations::expected_final_schema_fingerprint()
            .map_err(io_other)?;
    if built_fingerprint != expected_fingerprint {
        return Err(io::Error::other(format!(
            "built template schema fingerprint {built_fingerprint} does not match expected {expected_fingerprint}"
        )));
    }
    copy_tree(&root, &dest.join(EMPTY_FLAVOR))?;

    fs::write(dest.join("READY"), b"ok")?;
    Ok(())
}

/// Removes the template project's own registration from the template global
/// DB so seeded copies start with a schema-complete but empty registry;
/// each test's `TraceDecay::open` re-registers its own project cleanly.
async fn purge_global_registry(global_db_path: &Path) -> io::Result<()> {
    let conn = Connection::open(global_db_path).map_err(io_other)?;
    purge_configuration_rows(&conn)?;
    conn.execute_batch(
        "DELETE FROM store_artifacts;
         DELETE FROM graph_scopes;
         DELETE FROM store_instances;
         DELETE FROM project_aliases;
         DELETE FROM code_projects;
         DELETE FROM projects;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .map_err(io_other)?;
    Ok(())
}

fn purge_configuration(database_path: &Path) -> io::Result<()> {
    let conn = Connection::open(database_path).map_err(io_other)?;
    purge_configuration_rows(&conn)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(io_other)
}

fn purge_configuration_rows(conn: &Connection) -> io::Result<()> {
    // Append-only enforcement triggers block the DELETEs below, so drop them
    // for the purge and recreate them verbatim afterwards: production opens
    // the seeded store fail-closed against the exact final schema object set,
    // triggers included.
    let configuration_triggers = {
        let mut statement = conn
            .prepare(
                "SELECT name, sql FROM sqlite_master
                 WHERE type = 'trigger' AND tbl_name LIKE 'configuration_%'",
            )
            .map_err(io_other)?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(io_other)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_other)?
    };
    for (trigger, _) in &configuration_triggers {
        let quoted = trigger.replace('"', "\"\"");
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS \"{quoted}\";"))
            .map_err(io_other)?;
    }
    // Enumerate the live configuration tables instead of hand-maintaining a
    // list; a hard-coded inventory silently breaks the template build every
    // time the configuration schema adds, drops, or renames a table.
    let configuration_tables = {
        let mut statement = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name LIKE 'configuration_%'",
            )
            .map_err(io_other)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(io_other)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_other)?
    };
    let mut purge = String::from("PRAGMA foreign_keys = OFF;\n");
    for table in configuration_tables {
        // The singleton format marker is schema, not state: production
        // refuses to open a store whose marker row is missing.
        if table == "configuration_format" {
            continue;
        }
        let quoted = table.replace('"', "\"\"");
        purge.push_str(&format!("DELETE FROM \"{quoted}\";\n"));
    }
    purge.push_str("PRAGMA foreign_keys = ON;");
    conn.execute_batch(&purge).map_err(io_other)?;
    for (_, sql) in &configuration_triggers {
        conn.execute_batch(sql).map_err(io_other)?;
    }
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

/// Recursively copies `src` into `dest`, preserving file mtimes.
///
/// Destination directories go through [`PrivateStoreIo`] so seeded hook spool
/// roots stay owner-private under a group umask; plain `create_dir_all` would
/// leave them `0775` and fail closed at Hook capture admission.
fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    PrivateStoreIo::create_dir_all(dest)?;
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

/// Reads `sqlite_master` (`name` + `sql`, sorted) from a freshly built
/// template graph database and returns the same fingerprint the cache key
/// uses. A stale template whose objects drifted without a version bump
/// cannot match.
fn sqlite_master_shape_fingerprint(database_path: &Path) -> io::Result<String> {
    let conn = Connection::open(database_path).map_err(io_other)?;
    let mut statement = conn
        .prepare(
            "SELECT name, COALESCE(sql, '') FROM sqlite_master
             WHERE type IN ('table', 'index', 'trigger', 'view')
               AND name NOT LIKE 'sqlite_%'",
        )
        .map_err(io_other)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(io_other)?;
    let objects = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(io_other)?;
    Ok(
        tracedecay_runtime_core::db::migrations::fingerprint_schema_objects(
            objects
                .iter()
                .map(|(name, sql)| (name.as_str(), sql.as_str())),
        ),
    )
}

#[test]
fn host_exact_template_root_uses_plain_canonical_identity() {
    let scratch = tempfile::TempDir::new().unwrap();
    let created = scratch.path().join("template-root");
    fs::create_dir_all(&created).unwrap();
    // Bazel Windows tmp dirs keep `/`; the helper must still name the
    // same directory without a verbatim prefix.
    let mixed = PathBuf::from(created.to_string_lossy().replace('\\', "/"));
    let exact = host_exact_template_root(&mixed);
    assert_eq!(exact, canonical_root_identity(&created));
    assert!(
        !exact.to_string_lossy().starts_with(r"\\?\"),
        "template roots must be plain host paths, got {}",
        exact.display()
    );
}

/// A column constant that admission checks is part of the template key. A warm
/// target recorded under the contract with that column removed is not selected,
/// and the selected template's `graph_scopes` has the current columns.
#[tokio::test]
async fn changing_a_schema_column_constant_does_not_reuse_the_template() {
    assert!(
        tracedecay_global_db::schema_contract::authority_schema_fingerprint_omitting_column(
            "graph_scopes",
            "db_relpath",
        )
        .is_none(),
        "db_relpath is not an admitted graph_scopes column"
    );
    let omitted =
        tracedecay_global_db::schema_contract::authority_schema_fingerprint_omitting_column(
            "graph_scopes",
            "writable",
        )
        .expect("writable is an admitted graph_scopes column");
    assert_ne!(
        omitted,
        tracedecay_global_db::schema_contract::expected_admitted_schema_fingerprint(),
        "dropping a column constant must change the admitted schema fingerprint"
    );

    let scratch = tempfile::TempDir::new().unwrap();
    let tmp_root = scratch.path().canonicalize().unwrap();
    let stale_name = schema_template_dir_name(StoreSchemaVersions::CURRENT, &omitted).unwrap();
    let stale = tmp_root.join(&stale_name);
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("READY"), b"previous-schema").unwrap();

    let selected = ensure_template(&tmp_root, StoreSchemaVersions::CURRENT)
        .await
        .expect("current schema builds its own template");
    assert_ne!(selected, stale);
    assert_eq!(fs::read(selected.join("READY")).unwrap(), b"ok");

    let global_db = selected
        .join(EMPTY_FLAVOR)
        .join("home")
        .join(".tracedecay")
        .join("global.db");
    let connection = Connection::open(&global_db).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info('graph_scopes') ORDER BY cid")
        .unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "graph_scope_id",
            "project_id",
            "store_id",
            "branch_name",
            "parent_scope_id",
            "last_synced_at",
            "writable",
        ]
    );
}

#[tokio::test]
async fn template_recorded_at_an_older_git_correlation_version_is_rebuilt() {
    let scratch = tempfile::TempDir::new().unwrap();
    let tmp_root = scratch.path().canonicalize().unwrap();
    let current = StoreSchemaVersions::CURRENT;
    let older = StoreSchemaVersions {
        git_correlation: current.git_correlation - 1,
        ..current
    };

    // What a build before the Git correlation bump left in a warm target.
    let stale = ensure_template(&tmp_root, older).await.unwrap();
    let stale_sessions = sole_subdir(
        &stale
            .join(EMPTY_FLAVOR)
            .join("home")
            .join(".tracedecay")
            .join("projects"),
    )
    .unwrap()
    .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
    let downgraded = Connection::open(&stale_sessions)
        .unwrap()
        .execute(
            "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'git_correlation'",
            [older.git_correlation],
        )
        .unwrap();
    assert_eq!(
        downgraded, 1,
        "stale template must record the older version"
    );

    let template = ensure_template(&tmp_root, current).await.unwrap();
    let profile_root = tmp_root.join("home").join(".tracedecay");
    let cg = match init_project_from_template_root(
        Some(&template),
        &tmp_root.join("project"),
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    {
        Ok(cg) => cg,
        Err(error) => panic!("project must initialize from the template: {error:?}"),
    };
    let recorded: i64 = Connection::open(&cg.store_layout().sessions_db_path)
        .unwrap()
        .query_row(
            "SELECT version FROM session_schema_migrations WHERE name = 'git_correlation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, current.git_correlation);
    cg.close();
}

/// The copied store is the template, bound to the requested project. A fresh
/// init would not carry the template marker, and it would not be how this
/// bootstrap reports success.
#[tokio::test]
async fn template_seed_opens_the_copied_store_for_the_requested_project() {
    let scratch = tempfile::TempDir::new().unwrap();
    let tmp_root = scratch.path().canonicalize().unwrap();
    let template = ensure_template(&tmp_root, StoreSchemaVersions::CURRENT)
        .await
        .expect("template builds from a real init");
    let template_store = sole_subdir(
        &template
            .join(EMPTY_FLAVOR)
            .join("home")
            .join(".tracedecay")
            .join("projects"),
    )
    .unwrap();
    let template_manifest: Value = serde_json::from_str(
        &fs::read_to_string(template_store.join("store_manifest.json")).unwrap(),
    )
    .unwrap();
    let template_project_root = template_manifest["project_root"]
        .as_str()
        .expect("template manifest records a project root")
        .to_owned();
    fs::write(
        template_store.join("template-seed-marker"),
        b"seeded-not-initialized",
    )
    .unwrap();

    let project = tmp_root.join("requested-project");
    fs::create_dir_all(&project).unwrap();
    let profile_root = tmp_root.join("profile").join(".tracedecay");
    let cg = init_project_from_template_root(
        Some(&template),
        &project,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("seeded store opens");

    let data_root = cg.store_layout().data_root.clone();
    assert_eq!(
        fs::read(data_root.join("template-seed-marker")).unwrap(),
        b"seeded-not-initialized"
    );
    assert!(data_root.join("store_manifest.json").is_file());
    assert!(!data_root.join("config.json").exists());
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(data_root.join("store_manifest.json")).unwrap())
            .unwrap();
    assert_ne!(
        manifest["project_root"].as_str(),
        Some(template_project_root.as_str())
    );
    assert_eq!(
        manifest["project_root"].as_str(),
        Some(project.to_str().unwrap())
    );
    assert_eq!(cg.project_root(), project.as_path());
    cg.close();
}

/// An occupied destination used to fall through to a full init and hide a
/// broken seed. Seeding now fails closed with the seed error.
#[tokio::test]
async fn template_seed_does_not_fall_back_when_the_destination_exists() {
    let scratch = tempfile::TempDir::new().unwrap();
    let tmp_root = scratch.path().canonicalize().unwrap();
    let template = ensure_template(&tmp_root, StoreSchemaVersions::CURRENT)
        .await
        .expect("template builds from a real init");
    let project = tmp_root.join("requested-project");
    fs::create_dir_all(&project).unwrap();
    let profile_root = tmp_root.join("profile").join(".tracedecay");
    let project_id = default_profile_project_id(&project);
    fs::create_dir_all(profile_root.join("projects").join(&project_id)).unwrap();

    let error = match init_project_from_template_root(
        Some(&template),
        &project,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    {
        Ok(_) => panic!("an existing store must not be papered over with a full init"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "config error: mcp suite store template seed failed: store data dir already exists"
    );
}
