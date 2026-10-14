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

use asupersync::io::{AsyncWriteExt, BufReader};
use serde_json::{Value, json};
use tracedecay_daemon_protocol::connection::write_daemon_handshake_preamble;
use tracedecay_daemon_protocol::{
    DaemonClientReadHalf, DaemonClientStream, DaemonClientWriteHalf, DaemonEndpoint,
    DaemonHandshake,
};
use tracedecay_framing::BoundedLineReader;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, exact_identity, git, initialize_tracedecay,
    stop_daemon_gracefully, wait_for_open_phase, wait_for_readiness, wait_for_terminal_generation,
};
use crate::common::{
    IsolatedHome, daemon_authority_path, daemon_socket_path, poll_until,
    spawn_tracedecay_daemon_logged, spawn_tracedecay_daemon_with, tracedecay_command_with_home,
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

/// An initialized MCP session on its own daemon connection. The daemon serves
/// it through the project's server for as long as the connection stays open,
/// so that server is leased until [`LiveMcpSession::close`].
struct LiveMcpSession {
    reader: BoundedLineReader<BufReader<DaemonClientReadHalf>>,
    writer: DaemonClientWriteHalf,
}

impl LiveMcpSession {
    async fn open(home: &Path, handshake: &DaemonHandshake) -> Self {
        let project = handshake.project_path.as_deref().expect("project route");
        let authority: Value = serde_json::from_slice(
            &fs::read(daemon_authority_path(&home.join(".tracedecay")))
                .expect("published daemon authority"),
        )
        .expect("daemon authority JSON");
        let token = authority["auth_token"].as_str().expect("daemon token");
        let (reader, mut writer) =
            DaemonClientStream::connect(&DaemonEndpoint::Unix(daemon_socket_path(home)))
                .await
                .expect("connect to the daemon")
                .into_split();
        write_daemon_handshake_preamble(&mut writer, token, handshake)
            .await
            .expect("write the daemon preamble");
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "route-eviction", "version": "1"}
            }
        });
        writer
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .expect("write MCP initialize");
        let mut reader = BoundedLineReader::new(BufReader::new(reader));
        let response = tokio::time::timeout(RECEIPT_TIMEOUT, async {
            loop {
                let line = reader
                    .read_mcp_line()
                    .await
                    .expect("read MCP frame")
                    .unwrap_or_else(|| panic!("the daemon closed {}", project.display()));
                let frame: Value = serde_json::from_str(&line).expect("MCP frame JSON");
                if frame["id"] == 1 {
                    return frame;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("initialize never answered for {}", project.display()));
        assert!(
            response.get("error").is_none(),
            "initialize failed for {}: {response}",
            project.display()
        );
        Self { reader, writer }
    }

    /// Ends the session and waits for the daemon to close its side.
    async fn close(mut self) {
        self.writer.shutdown().await.expect("end the MCP session");
        tokio::time::timeout(RECEIPT_TIMEOUT, async {
            while self
                .reader
                .read_mcp_line()
                .await
                .expect("read until the daemon closes")
                .is_some()
            {}
        })
        .await
        .expect("the daemon closes the ended session");
    }
}

/// Every cached project has a live session when a ninth open passes capacity
/// admission, so nothing is retired up front. One session ends before that
/// open binds its route, and the bounded bind evicts the now-idle project. That
/// eviction must stop the project's process-global owners, or they keep its
/// stores leased through shutdown.
#[tokio::test]
async fn route_bind_eviction_releases_the_evicted_project_stores() {
    let (environment, first) = IsolatedHome::new();
    let mut projects = vec![first];
    projects.extend((1..=8).map(|ordinal| environment.scratch().join(format!("p{ordinal}"))));
    let projects = projects
        .into_iter()
        .map(|project| {
            initialize_repository(&project);
            project.canonicalize().expect("canonical fixture project")
        })
        .collect::<Vec<_>>();
    let (incoming, cached) = projects.split_last().expect("nine projects");
    let log_path = environment.scratch().join("route-bind-eviction.log");
    let hold = environment.scratch().join("hold-before-route-bind");
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command.env("TRACEDECAY_TEST_HOLD_BEFORE_ROUTE_BIND", &hold);
    });
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let mut sessions = Vec::new();
    for project in cached {
        initialize_tracedecay(environment.home(), project);
        let handshake = tracedecay::daemon::handshake_for_current_client(
            environment.profile(),
            Some(project.clone()),
            None,
            false,
            false,
        )
        .expect("project handshake");
        wait_for_readiness(&socket, &handshake, "ready", RECEIPT_TIMEOUT).await;
        sessions.push(LiveMcpSession::open(environment.home(), &handshake).await);
    }

    let _ = fs::remove_file(&entered);
    fs::write(&hold, b"hold").expect("hold file");
    let home = environment.home().to_path_buf();
    let incoming_project = incoming.clone();
    let init = std::thread::spawn(move || initialize_tracedecay(&home, &incoming_project));
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || entered.is_file().then_some(()),
        || "the ninth open never reached its route bind".to_owned(),
    );
    sessions.remove(0).close().await;
    fs::remove_file(&hold).expect("release the held open");
    init.join().expect("the ninth project initializes");

    let log = fs::read_to_string(&log_path).expect("daemon log");
    assert!(
        !log.contains(&format!(
            "event=project_server_warmup outcome=error project={}",
            incoming.display()
        )),
        "every cached project was leased, so the ninth open must not retire one up front; log={log}"
    );
    assert_eq!(
        log.matches(&format!(
            "project={} phase=graph_admitted",
            incoming.display()
        ))
        .count(),
        1,
        "the ninth open must evict at its route bind, not retry through capacity retirement; log={log}"
    );
    for session in sessions {
        session.close().await;
    }
    stop_daemon_gracefully(&mut daemon);

    let log = fs::read_to_string(&log_path).expect("daemon log");
    assert_clean_shutdown_log(&log);
    let (graph_db, sessions_db) = project_stores(environment.home(), &cached[0]);
    for store in [&graph_db, &sessions_db] {
        assert!(store.is_file(), "{} was mounted", store.display());
        assert_eq!(
            wal_bytes(store),
            0,
            "the evicted project keeps no WAL for {}",
            store.display()
        );
    }
}

/// A route-bind eviction detaches the idle server, but its process-owner
/// cleanup runs in a detached task. A reopen of the evicted project registers
/// its owners before its route publishes, so the cleanup must wait out that
/// open. Otherwise it retires the owners the live reopen just adopted. The
/// evicting open is parked at its bind until the cleanup parks ahead of its
/// retire, the reopen is parked at its own bind, and then the reopened
/// project's replay consumer must still be owned after it publishes.
#[tokio::test]
async fn route_bind_eviction_cannot_retire_a_reopened_projects_owners() {
    let (environment, first) = IsolatedHome::new();
    let mut projects = vec![first];
    projects.extend((1..=8).map(|ordinal| environment.scratch().join(format!("p{ordinal}"))));
    let projects = projects
        .into_iter()
        .map(|project| {
            initialize_repository(&project);
            project.canonicalize().expect("canonical fixture project")
        })
        .collect::<Vec<_>>();
    let (incoming, cached) = projects.split_last().expect("nine projects");
    let log_path = environment.scratch().join("route-bind-eviction-reopen.log");
    let bind_hold = environment.scratch().join("hold-before-route-bind");
    let bind_entered = PathBuf::from(format!("{}.entered", bind_hold.display()));
    let retire_hold = environment
        .scratch()
        .join("hold-before-evicted-owner-retire");
    let retire_entered = PathBuf::from(format!("{}.entered", retire_hold.display()));
    let check_hold = environment
        .scratch()
        .join("hold-after-unserved-owner-check");
    let check_entered = PathBuf::from(format!("{}.entered", check_hold.display()));
    let owners_hold = environment.scratch().join("hold-after-independent-owners");
    let owners_entered = PathBuf::from(format!("{}.entered", owners_hold.display()));
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command
            .env("TRACEDECAY_TEST_HOLD_BEFORE_ROUTE_BIND", &bind_hold)
            .env(
                "TRACEDECAY_TEST_HOLD_BEFORE_EVICTED_OWNER_RETIRE",
                &retire_hold,
            )
            .env(
                "TRACEDECAY_TEST_HOLD_AFTER_UNSERVED_OWNER_CHECK",
                &check_hold,
            )
            .env(
                "TRACEDECAY_TEST_HOLD_AFTER_INDEPENDENT_OWNERS",
                &owners_hold,
            );
    });
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let mut sessions = Vec::new();
    for project in cached {
        initialize_tracedecay(environment.home(), project);
        let handshake = tracedecay::daemon::handshake_for_current_client(
            environment.profile(),
            Some(project.clone()),
            None,
            false,
            false,
        )
        .expect("project handshake");
        wait_for_readiness(&socket, &handshake, "ready", RECEIPT_TIMEOUT).await;
        sessions.push(LiveMcpSession::open(environment.home(), &handshake).await);
    }

    // Park the incoming open at its route bind, then idle the first cached
    // project so that bind evicts it.
    let _ = fs::remove_file(&bind_entered);
    fs::write(&bind_hold, b"hold").expect("hold file");
    let home = environment.home().to_path_buf();
    let incoming_project = incoming.clone();
    let init = std::thread::spawn(move || initialize_tracedecay(&home, &incoming_project));
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || bind_entered.is_file().then_some(()),
        || "the ninth open never reached its route bind".to_owned(),
    );
    sessions.remove(0).close().await;

    // The eviction's detached cleanup parks ahead of its retire; the reopen
    // then parks at its own bind with its Scout owner already adopted.
    let _ = fs::remove_file(&retire_entered);
    let _ = fs::remove_file(&owners_entered);
    fs::write(&retire_hold, b"hold").expect("hold file");
    // Park the incoming open at its owner mounts too: catching the marker
    // here is the deterministic signal that its owner phase already ran, so
    // a later park on the same hold can only belong to the reopen.
    fs::write(&owners_hold, b"hold").expect("hold file");
    fs::remove_file(&bind_hold).expect("release the held bind");
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || retire_entered.is_file().then_some(()),
        || "the eviction cleanup never reached its retire".to_owned(),
    );
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || owners_entered.is_file().then_some(()),
        || "the incoming open never reached its owner mounts".to_owned(),
    );
    fs::remove_file(&owners_hold).expect("release the held owner mounts");
    init.join().expect("the ninth project initializes");

    let _ = fs::remove_file(&bind_entered);
    fs::write(&bind_hold, b"hold").expect("hold file");
    let evicted = cached[0].clone();
    let reopen_handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(evicted.clone()),
        None,
        false,
        false,
    )
    .expect("reopen handshake");
    let reopen_home = environment.home().to_path_buf();
    let session_handshake = reopen_handshake.clone();
    let reopen =
        tokio::spawn(async move { LiveMcpSession::open(&reopen_home, &session_handshake).await });
    // The wait must yield: `poll_until` sleeps the thread and would starve the
    // spawned open on this runtime before it ever connected to the daemon.
    // `open` may return early on the daemon's warming answer while its open
    // task keeps running toward the held bind in the background.
    let reopen_deadline = Instant::now() + RECEIPT_TIMEOUT;
    while !bind_entered.is_file() {
        assert!(
            Instant::now() < reopen_deadline,
            "the reopened open never reached its route bind"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Arm the check hold only now, so that only the eviction's cleanup can
    // park on it.
    fs::write(&check_hold, b"hold").expect("hold file");
    // The replay consumer registers inside the reopen's owner mounts, after
    // its core route publishes, and keeps the still-live incumbent rather
    // than displacing it. Parking the reopen there leaves it relying on the
    // exact consumer the eviction's kill is staged to remove next.
    let _ = fs::remove_file(&owners_entered);
    fs::write(&owners_hold, b"hold").expect("hold file");

    // Release the cleanup while the reopen still parks before publishing with
    // its capacity admission held. On the unguarded build it runs the served
    // check against the not-yet-bound route and parks inside the retire
    // section; the guarded build queues behind the open's admission until the
    // open finishes.
    fs::remove_file(&retire_hold).expect("release the held retire");
    // Releasing the bind now would race the cleanup's served check against
    // the reopen's publish at file-poll granularity. The unguarded build
    // reaches the held check only once it has already found the route
    // unserved; the guarded build queues on the reopen's capacity admission
    // and never reaches that hold, so bound the wait.
    let check_deadline = Instant::now() + Duration::from_secs(2);
    while !check_entered.is_file() && Instant::now() < check_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fs::remove_file(&bind_hold).expect("release the held reopen bind");
    // The reopen publishes, registers its owners, and parks; only then can
    // the unguarded kill fall on an incumbent that open already relies on.
    let owners_deadline = Instant::now() + RECEIPT_TIMEOUT;
    while !owners_entered.is_file() {
        assert!(
            Instant::now() < owners_deadline,
            "the reopened open never reached its owner mounts"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fs::remove_file(&check_hold).expect("release the held owner check");
    fs::remove_file(&owners_hold).expect("release the held owner mounts");
    let reopened = reopen
        .await
        .expect("the reopened session finishes its initialize");

    let observed = wait_for_readiness(&socket, &reopen_handshake, "ready", RECEIPT_TIMEOUT).await;
    // The unguarded cleanup's kill lands asynchronously: it detaches the
    // reopened project's replay consumer, whose own task then removes the
    // registry entry the status surface reads. Poll through the settle window
    // so the retired state cannot hide behind the first readiness read.
    let mut observed = observed;
    let settle = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settle && observed["hook_replay"]["status"].as_str() != Some("unowned") {
        observed = wait_for_readiness(&socket, &reopen_handshake, "ready", RECEIPT_TIMEOUT).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_ne!(
        observed["hook_replay"]["status"].as_str(),
        Some("unowned"),
        "the reopened project's replay consumer was retired by the eviction; {observed}"
    );

    reopened.close().await;
    for session in sessions {
        session.close().await;
    }
    stop_daemon_gracefully(&mut daemon);
    assert_clean_shutdown_log(&fs::read_to_string(&log_path).expect("daemon log"));
}

/// A capacity retirement waits for the owner's earlier retirements before it
/// releases the owner's stores, and a route-bind eviction's cleanup of that
/// owner first waits out the in-flight opens on the capacity admission. The
/// retirement must not hold that admission across its wait, or it and the
/// cleanup deadlock and every later open queues behind them. While it waits,
/// a reopen can serve the owner again; the retirement must then leave the
/// reopened project's stores alone.
#[tokio::test]
async fn capacity_retirement_behind_a_parked_bind_eviction_cleanup_completes() {
    let (environment, first) = IsolatedHome::new();
    let mut projects = vec![first];
    projects.extend((1..=9).map(|ordinal| environment.scratch().join(format!("p{ordinal}"))));
    let projects = projects
        .into_iter()
        .map(|project| {
            initialize_repository(&project);
            project.canonicalize().expect("canonical fixture project")
        })
        .collect::<Vec<_>>();
    let (cached, later) = projects.split_at(8);
    let (incoming, retiring_open) = (later[0].clone(), later[1].clone());
    let log_path = environment.scratch().join("capacity-behind-eviction.log");
    let bind_hold = environment.scratch().join("hold-before-route-bind");
    let bind_entered = PathBuf::from(format!("{}.entered", bind_hold.display()));
    let retire_hold = environment
        .scratch()
        .join("hold-before-evicted-owner-retire");
    let retire_entered = PathBuf::from(format!("{}.entered", retire_hold.display()));
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command
            .env("TRACEDECAY_TEST_HOLD_BEFORE_ROUTE_BIND", &bind_hold)
            .env(
                "TRACEDECAY_TEST_HOLD_BEFORE_EVICTED_OWNER_RETIRE",
                &retire_hold,
            );
    });
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake = |project: &Path| {
        tracedecay::daemon::handshake_for_current_client(
            environment.profile(),
            Some(project.to_path_buf()),
            None,
            false,
            false,
        )
        .expect("project handshake")
    };
    let mut sessions = Vec::new();
    for project in cached {
        initialize_tracedecay(environment.home(), project);
        let project_handshake = handshake(project);
        wait_for_readiness(&socket, &project_handshake, "ready", RECEIPT_TIMEOUT).await;
        sessions.push(LiveMcpSession::open(environment.home(), &project_handshake).await);
    }

    // The ninth open binds with the first project idle and evicts it; that
    // eviction's cleanup parks before it waits on the capacity admission.
    // Every open passes the hold points, so clear their markers first.
    let _ = fs::remove_file(&bind_entered);
    fs::write(&bind_hold, b"hold").expect("hold file");
    let home = environment.home().to_path_buf();
    let incoming_open = incoming.clone();
    let init = std::thread::spawn(move || initialize_tracedecay(&home, &incoming_open));
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || bind_entered.is_file().then_some(()),
        || "the ninth open never reached its route bind".to_owned(),
    );
    sessions.remove(0).close().await;
    let _ = fs::remove_file(&retire_entered);
    fs::write(&retire_hold, b"hold").expect("hold file");
    fs::remove_file(&bind_hold).expect("release the held bind");
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || retire_entered.is_file().then_some(()),
        || "the eviction cleanup never parked".to_owned(),
    );
    init.join().expect("the ninth project initializes");

    // Reopen the evicted project and idle it: the next open's capacity
    // retirement picks it while its owner's eviction cleanup is still parked.
    let evicted = cached[0].clone();
    let evicted_handshake = handshake(&evicted);
    let reopened = LiveMcpSession::open(environment.home(), &evicted_handshake).await;
    wait_for_readiness(&socket, &evicted_handshake, "ready", RECEIPT_TIMEOUT).await;
    reopened.close().await;
    let home = environment.home().to_path_buf();
    let retiring_project = retiring_open.clone();
    let retiring_init = std::thread::spawn(move || initialize_tracedecay(&home, &retiring_project));
    // The retirement has no observable wait point; give it time to reach its
    // wait on the parked cleanup before anything else queues for admission.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // While the retirement waits, the evicted project opens again. Its route
    // is served once more by the time the parked cleanup and the retirement's
    // store release run.
    let served_again = LiveMcpSession::open(environment.home(), &evicted_handshake).await;
    wait_for_readiness(&socket, &evicted_handshake, "ready", RECEIPT_TIMEOUT).await;
    // An idle server for the retiring open's own bind to evict.
    sessions.remove(0).close().await;
    fs::remove_file(&retire_hold).expect("release the parked cleanup");

    let retiring_deadline = Instant::now() + RECEIPT_TIMEOUT;
    while !retiring_init.is_finished() {
        assert!(
            Instant::now() < retiring_deadline,
            "the open behind the capacity retirement never completed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    retiring_init
        .join()
        .expect("the open behind the capacity retirement initializes");

    // The project served again keeps answering: the retirement left its
    // stores alone.
    wait_for_readiness(&socket, &evicted_handshake, "ready", RECEIPT_TIMEOUT).await;

    served_again.close().await;
    for session in sessions {
        session.close().await;
    }
    stop_daemon_gracefully(&mut daemon);
    assert_clean_shutdown_log(&fs::read_to_string(&log_path).expect("daemon log"));
}

/// The maintenance loop admits its first retention tick one retry delay after
/// the daemon starts.
const FIRST_MAINTENANCE_TICK: Duration = Duration::from_mins(1);

/// A retention maintenance tick lists every mounted store before it works
/// through them one unit at a time. An idle project it listed must still
/// retire for capacity while the tick runs, so the open waiting on that
/// capacity is admitted on its first attempt and serves a ready generation.
/// The tick parks right after its listing for the whole retirement, on a
/// one-unit background CPU budget: a tick that held that unit while parked
/// would starve the indexing the retirement drains and the ninth project needs.
#[tokio::test]
async fn capacity_retirement_completes_while_a_maintenance_tick_runs() {
    let (environment, first) = IsolatedHome::new();
    let mut projects = vec![first];
    projects.extend((1..=8).map(|ordinal| environment.scratch().join(format!("p{ordinal}"))));
    let projects = projects
        .into_iter()
        .map(|project| {
            initialize_repository(&project);
            project.canonicalize().expect("canonical fixture project")
        })
        .collect::<Vec<_>>();
    let (incoming, cached) = projects.split_last().expect("nine projects");
    let log_path = environment
        .scratch()
        .join("capacity-during-maintenance.log");
    let tick_hold = environment.scratch().join("hold-after-maintenance-listing");
    let tick_entered = PathBuf::from(format!("{}.entered", tick_hold.display()));
    fs::write(&tick_hold, b"hold").expect("hold file");
    let daemon_started = Instant::now();
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command.env("TRACEDECAY_TEST_HOLD_AFTER_MAINTENANCE_LISTING", &tick_hold);
        command.env("TRACEDECAY_INDEX_WORKERS", "1");
    });
    let socket = daemon_socket_path(environment.home());
    for project in cached {
        initialize_tracedecay(environment.home(), project);
    }
    poll_until(
        daemon_started + FIRST_MAINTENANCE_TICK + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || tick_entered.is_file().then_some(()),
        || "the first maintenance tick never listed the mounted stores".to_owned(),
    );

    // Every cached project is idle, so the ninth open retires the least
    // recently used one, a store the parked tick listed.
    initialize_tracedecay(environment.home(), incoming);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let incoming_handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(incoming.clone()),
        None,
        false,
        false,
    )
    .expect("incoming project handshake");
    wait_for_readiness(&socket, &incoming_handshake, "ready", RECEIPT_TIMEOUT).await;
    fs::remove_file(&tick_hold).expect("release the parked maintenance tick");

    let log = fs::read_to_string(&log_path).expect("daemon log");
    assert!(
        !log.contains("project_server_capacity_reached"),
        "a running maintenance tick must not block capacity retirement; log={log}"
    );
    stop_daemon_gracefully(&mut daemon);
    assert_clean_shutdown_log(&fs::read_to_string(&log_path).expect("daemon log"));
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
