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
use std::time::{Duration, Instant};

use crate::code_index_journey::{
    commit_all, exact_identity, git, initialize_tracedecay, stop_daemon_gracefully,
    wait_for_open_phase, wait_for_terminal_generation,
};
use crate::common::{
    IsolatedHome, daemon_socket_path, poll_until, spawn_tracedecay_daemon_logged,
    spawn_tracedecay_daemon_with, tracedecay_command_with_home,
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
    wait_for_open_phase(log_path, project, phase).await;
    init
}

/// Joins the interrupted `init` and proves no daemon answers afterwards, so
/// the store files are read after the last writer left.
fn settle_interrupted_init(init: std::thread::JoinHandle<std::io::Result<Output>>, socket: &Path) {
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
    let (environment, project) = IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    let revision = initialize_repository(&project);

    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(project.clone()),
        None,
        false,
        false,
    )
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
    let (environment, project) = IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    initialize_repository(&project);
    let log_path = environment.scratch().join("stop-during-open.log");
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |_| {});

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

/// The open is parked after its independent owners registered, so the Hook V2
/// replay consumer is running when the stop cancels it. The failed open removes
/// its servers before shutdown can find them, so it must stop that consumer
/// itself or the consumer's leases hold both project stores open.
#[tokio::test]
async fn stop_after_independent_owners_releases_every_project_store() {
    let (environment, project) = IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    initialize_repository(&project);
    let log_path = environment.scratch().join("owners-registered.log");
    let hold = environment.scratch().join("hold-after-independent-owners");
    fs::write(&hold, b"hold").expect("hold file");
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command.env("TRACEDECAY_TEST_HOLD_AFTER_INDEPENDENT_OWNERS", &hold);
    });

    let init = init_until_open_phase(
        environment.home(),
        &project,
        &log_path,
        "independent_owners_registered",
    )
    .await;
    poll_until(
        Instant::now() + Duration::from_secs(30),
        Duration::from_millis(20),
        || entered.is_file().then_some(()),
        || "the project open never parked after its owners registered".to_owned(),
    );
    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "send SIGTERM to daemon");
    // Shutdown cancels every admitted open before it starts draining clients.
    poll_until(
        Instant::now() + Duration::from_secs(30),
        Duration::from_millis(20),
        || {
            fs::read_to_string(&log_path)
                .unwrap_or_default()
                .contains("outcome=client_drain_start")
                .then_some(())
        },
        || "the daemon never started draining".to_owned(),
    );
    fs::remove_file(&hold).expect("release the held open");
    let exit = daemon
        .wait_for_exit(Duration::from_secs(120))
        .expect("wait for daemon shutdown")
        .expect("daemon exits once the held open settles");
    assert!(exit.success(), "daemon did not stop cleanly: {exit}");
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
    for store in [&graph_db, &sessions_db] {
        assert!(store.is_file(), "{} was mounted", store.display());
        assert_eq!(
            wal_bytes(store),
            0,
            "a stop after owner registration keeps no WAL for {}",
            store.display()
        );
    }
}

#[tokio::test]
async fn stop_cancels_an_admitted_project_open() {
    let (environment, project) = IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    initialize_repository(&project);
    let log_path = environment.scratch().join("cancelled-open.log");
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |_| {});

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
