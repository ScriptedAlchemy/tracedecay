//! One shipped daemon keeps opening projects after its project-server cache
//! fills.
//!
//! Every opened project runs its automation loop. When a new open retires the
//! least-recently-used idle project, that loop must release the project's
//! session store too, or the retirement is refused and the new project never
//! serves.

use std::fs;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, exact_symbol, git, initialize_tracedecay, stop_daemon_gracefully,
    wait_for_readiness,
};
use crate::common::{IsolatedHome, daemon_socket_path, spawn_tracedecay_daemon_logged};

/// Two more projects than the daemon caches (eight), so at least two opens
/// must retire an idle project first.
const PROJECTS: usize = 10;

#[tokio::test]
async fn projects_past_the_server_cache_open_and_serve() {
    let (environment, first) = IsolatedHome::new();
    let mut projects = vec![first];
    projects.extend((1..PROJECTS).map(|ordinal| environment.scratch().join(format!("p{ordinal}"))));
    for (ordinal, project) in projects.iter().enumerate() {
        fs::create_dir_all(project.join("src")).expect("fixture source directory");
        fs::write(
            project.join("src/lib.rs"),
            format!("pub fn capacity_probe_{ordinal}() -> usize {{ {ordinal} }}\n"),
        )
        .expect("fixture source");
        git(project, &["init", "--quiet", "--initial-branch=main"]);
        commit_all(project, "capacity fixture");
    }

    let socket = daemon_socket_path(environment.home());
    let log_path = environment.scratch().join("project-capacity-daemon.log");
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |_| {});
    for project in &projects {
        initialize_tracedecay(environment.home(), project);
    }

    let last = projects[PROJECTS - 1]
        .canonicalize()
        .expect("canonical last project");
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(last.clone()),
        None,
        false,
        false,
    )
    .expect("production daemon handshake");
    wait_for_readiness(&socket, &handshake, "ready", RECEIPT_TIMEOUT).await;
    let probe = format!("capacity_probe_{}", PROJECTS - 1);
    let found = exact_symbol(&socket, &handshake, &probe, false).await;
    assert_eq!(found["count"], 1, "the last project must serve: {found}");

    stop_daemon_gracefully(&mut daemon);
}
