//! Shared fixtures for the consolidated storage suite.
//!
//! The template-database cache generalizes the pattern db_query_test used:
//! building a schema from scratch is a large fixed cost per test (especially
//! on Windows), so the first test process to need a given fixture builds it
//! once under the system temp dir and every other test — including tests in
//! other processes, since nextest runs one process per test — copies the
//! finished file instead.

use std::fs::{self, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// Serializes tests across suite modules that mutate the process-wide
/// HOME/USERPROFILE/profile-dir environment variables. Only plain
/// `cargo test` shares one process between tests; nextest gives every test
/// its own process, where this lock is uncontended.
pub static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// FNV-1a hash of everything that can change a template's contents: the
/// schema-defining sources, the template name, and any builder-specific
/// fingerprint supplied by the caller (for templates whose contents also
/// depend on sources outside `tracedecay-runtime-core`'s `db` module, such as
/// fixture SQL defined in a test file).
fn template_hash(name: &str, builder_fingerprint: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in include_bytes!("../../crates/tracedecay-runtime-core/src/db/migrations.rs")
        .iter()
        .chain(include_bytes!(
            "../../crates/tracedecay-runtime-core/src/db/connection.rs"
        ))
        .chain(include_bytes!(
            "../../crates/tracedecay-runtime-core/src/db/engine/test_support.rs"
        ))
        .chain(include_bytes!("../common/mod.rs"))
        .chain(name.as_bytes())
        .chain(builder_fingerprint)
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn template_db_path(name: &str, builder_fingerprint: &[u8]) -> PathBuf {
    std::env::temp_dir()
        .join("tracedecay-test-fixtures")
        .join(format!(
            "{name}-{:016x}.db",
            template_hash(name, builder_fingerprint)
        ))
}

fn template_cache_exists(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.len() > 0)
}

/// Returns the path of the cached template database named `name`, building
/// it first if this machine has no template for the current schema revision.
///
/// `builder_fingerprint` must cover every input to `build` that lives
/// outside the `tracedecay-runtime-core` `db` module — typically
/// `include_bytes!` of the defining test file —
/// so that editing the fixture-building code invalidates the cached
/// template. Pass `&[]` when `build` depends only on the production schema
/// code that `template_hash` already covers.
///
/// `build` must write a fully checkpointed database (no live WAL) at the
/// path it is given. Concurrent test processes coordinate through an
/// exclusive file lock and an atomic rename, so at most one process pays the
/// build cost.
pub async fn ensure_template_db<F, Fut>(name: &str, builder_fingerprint: &[u8], build: F) -> PathBuf
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = ()>,
{
    let template_path = template_db_path(name, builder_fingerprint);
    if template_cache_exists(&template_path) {
        return template_path;
    }

    let cache_dir = template_path
        .parent()
        .expect("template path should have a parent directory")
        .to_path_buf();
    fs::create_dir_all(&cache_dir).expect("failed to create template cache directory");
    let lock_path = cache_dir.join(format!("{name}-template.lock"));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("failed to open template cache lock");
    lock_file
        .lock_exclusive()
        .expect("failed to lock template cache");

    if template_cache_exists(&template_path) {
        return template_path;
    }

    let dir = tempfile::TempDir::new_in(&cache_dir).expect("failed to create template temp dir");
    let db_path = dir.path().join("template.db");
    build(db_path.clone()).await;

    let tmp_path = cache_dir.join(format!("{name}-{}.tmp", std::process::id()));
    fs::copy(&db_path, &tmp_path).expect("failed to stage template database");
    if template_path.exists() {
        fs::remove_file(&template_path).expect("failed to remove stale template database");
    }
    fs::rename(&tmp_path, &template_path).expect("failed to publish template database");
    template_path
}

/// Seeds `dest` with an empty latest-schema graph database — the exact file
/// `Database::initialize` would produce — without paying schema creation.
pub async fn seed_latest_graph_db(dest: &Path) {
    let template = ensure_template_db("graph-empty", &[], |path| async move {
        // Initialise on a throwaway path, then snapshot the committed schema to
        // `path`. The registered runtime's `checkpoint` follows a bounded WAL
        // policy that no-ops below its soft threshold, so a freshly-created
        // schema can still live entirely in the WAL — copying the bare `.db`
        // file would then capture an empty (v0) database. `snapshot_to` writes a
        // transactionally consistent standalone copy, matching how the global-db
        // and session template helpers stage their fixtures.
        let init_path = path.with_file_name("template-init.db");
        let (db, _) = crate::common::initialize_test_database(&init_path)
            .await
            .expect("failed to initialize template database");
        db.snapshot_to(&path)
            .await
            .expect("failed to snapshot template database");
        db.close();
    })
    .await;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).expect("failed to create test database directory");
    }
    fs::copy(&template, dest).expect("failed to seed database from template");
}

/// A function node with reasonable defaults, shared by the suite modules that
/// need graph rows to query against. `db_test`, `db_query_test`, and
/// `corruption_test` each carried a byte-identical copy of this struct
/// literal, so the fixtures could drift apart silently; one definition keeps
/// every module asserting against the same shape.
pub fn sample_node(id: &str, name: &str, file_path: &str) -> tracedecay::types::Node {
    tracedecay::types::Node {
        id: id.to_string(),
        kind: tracedecay::types::NodeKind::Function,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: file_path.to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: Some(format!("Documentation for {name}")),
        visibility: tracedecay::types::Visibility::Pub,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1000,
        parent_id: None,
    }
}
