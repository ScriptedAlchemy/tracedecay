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
/// schema-defining sources, the template name, the unsafe-fast env toggle
/// (it changes journal/synchronous file properties), and any
/// builder-specific fingerprint supplied by the caller (for templates whose
/// contents also depend on sources outside src/db, such as fixture SQL
/// defined in a test file).
fn template_hash(name: &str, builder_fingerprint: &[u8]) -> u64 {
    let unsafe_fast = std::env::var(tracedecay::db::SQLITE_UNSAFE_FAST_ENV).unwrap_or_default();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in include_bytes!("../../src/db/migrations.rs")
        .iter()
        .chain(include_bytes!("../../src/db/connection.rs"))
        .chain(name.as_bytes())
        .chain(unsafe_fast.as_bytes())
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
/// outside `src/db` — typically `include_bytes!` of the defining test file —
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
        let (db, _) = tracedecay::db::Database::initialize(&path)
            .await
            .expect("failed to initialize template database");
        db.checkpoint()
            .await
            .expect("failed to checkpoint template database");
        db.close();
    })
    .await;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).expect("failed to create test database directory");
    }
    fs::copy(&template, dest).expect("failed to seed database from template");
}
