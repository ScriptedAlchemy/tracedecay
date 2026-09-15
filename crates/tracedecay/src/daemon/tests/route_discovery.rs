//! Route resolution must never charge repository discovery to the async
//! workers that also poll the daemon's accept loop.
//!
//! Live wedge these cover: every tokio worker sat inside `gix` discovery
//! reached from connection admission, so `accept()` was never polled and the
//! listening socket refused new clients (errno 61) while the process stayed
//! alive and its other servers answered.

#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_runtime_core::git_discovery::identity_resolution_elapsed;
use tracedecay_runtime_core::git_repository::{
    delay_repository_discovery_for_test, observe_repository_discovery_for_test,
    repository_discovery_count_for_test, repository_topology_resolution_count_for_test,
    reset_repository_discovery_for_test,
};

use super::bootstrap::run_git;
use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::daemon::{DaemonEngine, DaemonHandshake};

/// A committed repository, because repository discovery reads HEAD as well as
/// the ancestor walk.
///
/// Registers the runtime ports too, so each test here stands alone instead of
/// inheriting another test's composition root.
fn committed_repository(root: &Path) {
    crate::register_runtime_ports().expect("runtime port registration");
    std::fs::create_dir_all(root).expect("create repository");
    run_git(root, &["init", "-b", "main", "--quiet"]);
    std::fs::write(root.join("README.md"), "route discovery fixture\n").expect("fixture content");
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "--quiet", "-m", "fixture"]);
}

fn handshake_for(project: &Path, profile_root: &Path) -> DaemonHandshake {
    DaemonHandshake {
        project_path: Some(project.to_path_buf()),
        client_identity: test_client_identity_for(profile_root.to_path_buf()),
        ..test_handshake_defaults()
    }
}

/// A slow checkout must not hold the worker its connection is served on.
///
/// One worker thread is the whole point: before repository probes moved off
/// the async workers, the delayed discovery below ran inline on the only
/// worker and the second route could not be polled at all until it finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn slow_repository_discovery_does_not_stall_another_route() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join("profile");
    let slow = home.path().join("slow-volume");
    let responsive = home.path().join("responsive");
    committed_repository(&slow);
    committed_repository(&responsive);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "slow route discovery");
    let engine = test_daemon_engine_for_profile(&profile_root);

    // Warm the profile registry so the measured call below times route
    // resolution, not first-touch database creation.
    let responsive_handshake = handshake_for(&responsive, &profile_root);
    let _ = engine.cached_project_server(&responsive_handshake).await;

    let discovery_delay = Duration::from_millis(750);
    delay_repository_discovery_for_test(&slow, discovery_delay);

    let slow_handshake = handshake_for(&slow, &profile_root);
    let slow_engine: DaemonEngine = engine.clone();
    let slow_route = tokio::spawn(async move {
        let _ = slow_engine.cached_project_server(&slow_handshake).await;
    });
    // Let the slow route reach its discovery before the second one starts.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    let admitted = tokio::time::timeout(
        Duration::from_millis(400),
        engine.cached_project_server(&responsive_handshake),
    )
    .await;
    let elapsed = started.elapsed();
    reset_repository_discovery_for_test(&slow);
    slow_route.await.expect("slow route task");
    reset_repository_discovery_for_test(&responsive);

    assert!(
        admitted.is_ok(),
        "a second route must resolve while another checkout's discovery is slow"
    );
    assert!(
        elapsed < discovery_delay,
        "route resolution waited {elapsed:?} on an unrelated checkout's discovery"
    );
}

/// Concurrent connections to one project share a single identity resolution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_routes_resolve_one_repository_topology() {
    const CONNECTIONS: usize = 8;

    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join("profile");
    let project = home.path().join("project");
    committed_repository(&project);
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "concurrent route discovery");
    let engine = test_daemon_engine_for_profile(&profile_root);

    observe_repository_discovery_for_test(&project);
    let mut routes = tokio::task::JoinSet::new();
    for _ in 0..CONNECTIONS {
        let engine = engine.clone();
        let handshake = handshake_for(&project, &profile_root);
        routes.spawn(async move {
            engine
                .cached_project_server(&handshake)
                .await
                .expect("registered route resolves")
        });
    }
    while let Some(route) = routes.join_next().await {
        route.expect("route task");
    }
    let resolutions = repository_topology_resolution_count_for_test(&project);
    let discoveries = repository_discovery_count_for_test(&project);
    reset_repository_discovery_for_test(&project);

    assert_eq!(
        resolutions, 1,
        "{CONNECTIONS} concurrent routes must share one repository identity resolution"
    );
    // Every route still reads HEAD live, but once the topology is published
    // those reads open the repository at its own Git directory instead of
    // walking the volume to find it.
    assert!(
        discoveries <= CONNECTIONS as u64 + 1,
        "{discoveries} live discoveries for {CONNECTIONS} routes: topology is being rediscovered"
    );
}

/// A discovery that outlives its budget is deferred, never a hang and never
/// "this project is not enrolled".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_repository_discovery_defers_the_route() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join("profile");
    let project = home.path().join("project");
    committed_repository(&project);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "deferred discovery");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = handshake_for(&project, &profile_root);
    // Warm the profile registry so only discovery is slow.
    let _ = engine.cached_project_server(&handshake).await;

    delay_repository_discovery_for_test(
        &project,
        crate::daemon::REPOSITORY_DISCOVERY_DEADLINE + Duration::from_millis(500),
    );
    let route = tokio::time::timeout(
        crate::daemon::REPOSITORY_DISCOVERY_DEADLINE * 3,
        engine.cached_project_server(&handshake),
    )
    .await
    .expect("a discovery over budget must refuse rather than hang");
    reset_repository_discovery_for_test(&project);

    let Err(refusal) = route else {
        panic!("a discovery over budget must not resolve a route");
    };
    let message = refusal.to_string();
    assert!(
        message.contains("repository discovery") && message.contains("deferred"),
        "expected a deferred-discovery refusal, got: {message}"
    );
    assert!(
        message.contains(crate::daemon::PROJECT_WARMING_RETRY_HINT),
        "a deferred discovery must stay retryable, got: {message}"
    );
    assert!(
        message.contains("retry after"),
        "a deferral must say when to come back, got: {message}"
    );
}

/// A deferral has to converge. The resolution the refusal abandoned keeps
/// running and publishes its result, so the next request decides the route from
/// it instead of starting the walk over.
///
/// Live wedge this covers: on a slow volume every retry re-ran discovery, was
/// deferred at the same budget, and the root stayed "warming" for as long as
/// the client kept asking — minutes, with no path to a resolved route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deferred_discovery_converges_on_the_next_request() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join("profile");
    let project = home.path().join("project");
    committed_repository(&project);
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "converging discovery");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = handshake_for(&project, &profile_root);
    // Warm the profile registry so only discovery is slow.
    let _ = engine.cached_project_server(&handshake).await;

    delay_repository_discovery_for_test(
        &project,
        crate::daemon::REPOSITORY_DISCOVERY_DEADLINE + Duration::from_millis(500),
    );
    let deferred = engine.cached_project_server(&handshake).await;
    assert!(
        deferred
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("repository discovery")),
        "a discovery over budget must defer the route"
    );

    await_published_resolution(&project).await;
    let resolutions = repository_topology_resolution_count_for_test(&project);
    let converged = tokio::time::timeout(
        crate::daemon::REPOSITORY_DISCOVERY_DEADLINE,
        engine.cached_project_server(&handshake),
    )
    .await;
    let converged_resolutions = repository_topology_resolution_count_for_test(&project);
    reset_repository_discovery_for_test(&project);

    let converged = converged.expect("a converged route must answer inside the discovery budget");
    if let Err(error) = &converged {
        let message = error.to_string();
        assert!(
            !message.contains("repository discovery"),
            "the deferral repeated instead of converging: {message}"
        );
    }
    assert_eq!(
        converged_resolutions, resolutions,
        "the converged route re-ran repository discovery instead of reading the published topology"
    );
}

/// Wait for the resolution the deferral abandoned to retire its slot, which it
/// does only after publishing into the retained topology.
async fn await_published_resolution(project: &Path) {
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    for _ in 0..400 {
        if identity_resolution_elapsed(project).is_none()
            && identity_resolution_elapsed(&canonical).is_none()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the abandoned resolution for {} never published",
        project.display()
    );
}
