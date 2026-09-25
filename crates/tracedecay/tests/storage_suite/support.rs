//! Shared fixtures for the consolidated storage suite.
//!
//! Building a schema from scratch is a large fixed cost per test (especially
//! on Windows), so the first test process to need a given fixture builds it
//! once under the system temp dir and every other test, including tests in
//! other processes, since nextest runs one process per test, copies the
//! finished file instead.

use std::fs::{self, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// FNV-1a hash of the template name and the running test executable's
/// identity. Every schema definition the template captures is compiled into
/// that executable, wherever in the workspace it lives, so a rebuild can never
/// reuse a template cut from an older schema by this or another checkout.
fn template_hash(name: &str) -> u64 {
    let exe = std::env::current_exe().expect("failed to resolve test executable");
    let metadata = fs::metadata(&exe).expect("failed to stat test executable");
    let modified = metadata
        .modified()
        .expect("test executable has no modification time")
        .duration_since(UNIX_EPOCH)
        .expect("test executable modified before the Unix epoch")
        .as_nanos();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in exe
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .chain(&metadata.len().to_le_bytes())
        .chain(&modified.to_le_bytes())
        .chain(name.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn template_db_path(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("tracedecay-test-fixtures")
        .join(format!("{name}-{:016x}.db", template_hash(name)))
}

fn template_cache_exists(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.len() > 0)
}

/// Returns the path of the cached template database named `name`, building
/// it first if this test executable has no template yet.
///
/// `build` must write a fully checkpointed database (no live WAL) at the
/// path it is given. Concurrent test processes coordinate through an
/// exclusive file lock and an atomic rename, so at most one process pays the
/// build cost.
pub async fn ensure_template_db<F, Fut>(name: &str, build: F) -> PathBuf
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = ()>,
{
    let template_path = template_db_path(name);
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
    lock_file.lock().expect("failed to lock template cache");

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

/// Seeds `dest` with an empty latest-schema graph database, the exact file
/// `Database::initialize` would produce, without paying schema creation.
pub async fn seed_latest_graph_db(dest: &Path) {
    let template = ensure_template_db("graph-empty", |path| async move {
        // Initialise on a throwaway path, then snapshot the committed schema to
        // `path`. The registered runtime's `checkpoint` follows a bounded WAL
        // policy that no-ops below its soft threshold, so a freshly-created
        // schema can still live entirely in the WAL, copying the bare `.db`
        // file would then capture an empty (v0) database. `VACUUM INTO` writes
        // a transactionally consistent standalone copy of the closed fixture.
        let init_path = path.with_file_name("template-init.db");
        let (db, _) = crate::common::initialize_test_database(&init_path)
            .await
            .expect("failed to initialize template database");
        db.close();
        let template_source =
            rusqlite::Connection::open(&init_path).expect("failed to open template database");
        template_source
            .execute(
                "VACUUM INTO ?1",
                [path.to_str().expect("utf-8 template path")],
            )
            .expect("failed to snapshot template database");
        drop(template_source);
    })
    .await;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).expect("failed to create test database directory");
    }
    fs::copy(&template, dest).expect("failed to seed database from template");
}
