//! A graceful daemon stop checkpoints the profile and project stores.
//!
//! The writer's shutdown TRUNCATE checkpoint runs only when the store runtime
//! closes, and the runtime closes only once nothing holds a client lease. A
//! daemon-lifetime owner that keeps a lease past terminal close, or a project
//! open that registers owners after they were drained, leaves the store's WAL
//! behind on disk.

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, exact_identity, git, initialize_tracedecay,
    stop_daemon_gracefully, wait_for_terminal_generation,
};
use crate::common::{
    EnvVarGuard, IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with,
    tracedecay_command_with_home,
};

const ANCHOR: &str = "shutdown_checkpoint_anchor";

fn wal_bytes(database: &Path) -> u64 {
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    fs::metadata(PathBuf::from(wal)).map_or(0, |metadata| metadata.len())
}

fn initialize_repository(project: &Path) -> String {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        format!("pub fn {ANCHOR}() -> u32 {{ 1 }}\n"),
    )
    .expect("fixture source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(project, "fixture")
}

fn project_stores(home: &Path, project: &Path) -> (PathBuf, PathBuf) {
    let layout =
        tracedecay_runtime_core::storage::resolve_layout(project, &home.join(".tracedecay"))
            .expect("profile-sharded project layout");
    (layout.graph_db_path, layout.sessions_db_path)
}

/// Starts `tracedecay init` without waiting for it, then waits until the
/// daemon log reports `phase` for this project's open.
async fn init_until_open_phase(
    home: &Path,
    project: &Path,
    log_path: &Path,
    phase: &str,
) -> std::thread::JoinHandle<std::io::Result<Output>> {
    let mut init = tracedecay_command_with_home(home);
    init.arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let init = std::thread::spawn(move || init.output());
    let marker = format!("project={} phase={phase}", project.display());
    let mut log = String::new();
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            log = fs::read_to_string(log_path).unwrap_or_default();
            if log.contains(&marker) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("project open never reached {phase}; log={log}"));
    init
}

/// Joins the interrupted `init` and proves no daemon answers afterwards, so
/// the store files are read after the last writer left.
fn settle_interrupted_init(
    init: std::thread::JoinHandle<std::io::Result<Output>>,
    socket: &Path,
) {
    init.join()
        .expect("init thread")
        .expect("interrupted init ran");
    assert!(
        UnixStream::connect(socket).is_err(),
        "no daemon may be serving after the stop"
    );
}

fn assert_clean_shutdown_log(log: &str) {
    assert!(
        !log.contains("still held at shutdown"),
        "every store runtime must close at shutdown; log={log}"
    );
    assert!(
        !log.contains("event=project_server_warmup outcome=error"),
        "a stop during project open is not a warm-up error; log={log}"
    );
}

#[tokio::test]
async fn graceful_stop_truncates_profile_and_project_store_wals() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    let revision = initialize_repository(&project);

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
        ANCHOR,
        Some("src/lib.rs"),
    )
    .await;

    let (graph_db, _) = project_stores(environment.home(), &project);
    let stores = [environment.home().join(".tracedecay/global.db"), graph_db];
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

#[tokio::test]
async fn stop_during_project_open_truncates_project_store_wals() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    initialize_repository(&project);
    let log_path = environment.scratch().join("stop-during-open.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});

    // Both project stores are mounted here while the open still has its full
    // server and owner registration ahead of it.
    let init = init_until_open_phase(
        environment.home(),
        &project,
        &log_path,
        "project_sessions_admitted",
    )
    .await;
    stop_daemon_gracefully(&mut daemon);
    settle_interrupted_init(init, &socket);

    let (graph_db, sessions_db) = project_stores(environment.home(), &project);
    for store in [&graph_db, &sessions_db] {
        assert!(store.is_file(), "{} was mounted", store.display());
        assert_eq!(
            wal_bytes(store),
            0,
            "a stop during project open keeps no WAL for {}",
            store.display()
        );
    }
    assert_clean_shutdown_log(&fs::read_to_string(&log_path).expect("daemon log"));
}

#[tokio::test]
async fn stop_cancels_an_admitted_project_open() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    initialize_repository(&project);
    let log_path = environment.scratch().join("cancelled-open.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});

    // The graph store is admitted; core and full construction, each behind a
    // cancellation boundary, are still ahead of the open.
    let init =
        init_until_open_phase(environment.home(), &project, &log_path, "graph_admitted").await;
    stop_daemon_gracefully(&mut daemon);
    settle_interrupted_init(init, &socket);

    let log = fs::read_to_string(&log_path).expect("daemon log");
    assert!(
        log.contains(&format!(
            "event=project_server_warmup outcome=cancelled project={}",
            project.display()
        )),
        "the stop must record the open as cancelled; log={log}"
    );
    assert_clean_shutdown_log(&log);
    let (graph_db, sessions_db) = project_stores(environment.home(), &project);
    assert!(graph_db.is_file(), "{} was admitted", graph_db.display());
    for store in [&graph_db, &sessions_db] {
        assert_eq!(
            wal_bytes(store),
            0,
            "a cancelled open keeps no WAL for {}",
            store.display()
        );
    }
}
