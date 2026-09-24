//! A graceful daemon stop checkpoints the profile and project stores.
//!
//! The writer's shutdown TRUNCATE checkpoint runs only when the store runtime
//! closes, and the runtime closes only once nothing holds a client lease. A
//! daemon-lifetime owner that keeps a lease past terminal close leaves the
//! store's WAL behind on disk.

use std::fs;
use std::path::{Path, PathBuf};

use crate::code_index_journey::{
    commit_all, exact_identity, git, initialize_tracedecay, stop_daemon_gracefully,
    wait_for_terminal_generation,
};
use crate::common::{IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with};

fn wal_bytes(database: &Path) -> u64 {
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    fs::metadata(PathBuf::from(wal)).map_or(0, |metadata| metadata.len())
}

#[tokio::test]
async fn graceful_stop_truncates_profile_and_project_store_wals() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn shutdown_checkpoint_anchor() -> u32 { 1 }\n",
    )
    .expect("fixture source");
    git(&project, &["init", "--quiet", "--initial-branch=main"]);
    let revision = commit_all(&project, "fixture");

    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");
    // Indexing starts only after the project open has registered its owners,
    // so a sealed generation proves the open settled before the stop.
    wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &exact_identity(&project, project_id),
        "refs/heads/main",
        Some(&revision),
        None,
        "shutdown_checkpoint_anchor",
        Some("src/lib.rs"),
    )
    .await;

    let profile_root = environment.home().join(".tracedecay");
    let layout = tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root)
        .expect("profile-sharded project layout");
    let stores = [profile_root.join("global.db"), layout.graph_db_path];
    for store in &stores {
        assert!(
            wal_bytes(store) > 0,
            "a live daemon leaves frames in {}",
            store.display()
        );
    }

    stop_daemon_gracefully(&mut daemon);

    for store in &stores {
        assert_eq!(
            wal_bytes(store),
            0,
            "a stopped daemon keeps no WAL for {}",
            store.display()
        );
    }
}
