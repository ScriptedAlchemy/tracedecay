//! Queries against a worktree whose code index is parked, through the shipped
//! daemon.
//!
//! A committed source file the daemon cannot read fails every reconcile the
//! same way, so the worker parks convergence until the operator acts. A query
//! that answers with a retryable refusal there is retried forever; it must
//! carry the park's cause and remedy and say that retrying cannot help.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, git, initialize_tracedecay, search, stop_daemon_gracefully, tool,
};
use crate::common::{EnvVarGuard, IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with};

const CAUSE: &str = "code-index repository status failed: code-index classification: IO error \
    while writing blob or reading file metadata or changing filetype";
const REMEDY: &str = "indexing this worktree fails the same way on every pass over unchanged \
    source; fix what the named failure points at, then run `tracedecay sync` to retry; if it \
    names an internal indexing contract, run `tracedecay upgrade` (a restarted daemon retries \
    automatically) and report the failure if it persists";

#[tokio::test]
async fn parked_worktree_queries_carry_the_park_and_are_not_retryable() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn alpha() {}\nmod model;\n",
    )
    .expect("fixture source");
    fs::write(
        project.join("src/model.rs"),
        "pub struct Gamma;\npub fn delta() {}\n",
    )
    .expect("fixture model source");
    git(&project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(&project, "parked fixture");
    let unreadable = project.join("src/model.rs");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
        .expect("make the committed source unreadable");

    let socket = daemon_socket_path(environment.home());
    let log_path = environment.scratch().join("code-index-park-daemon.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    initialize_tracedecay(environment.home(), &project);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");

    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    let searched = loop {
        let searched = search(&socket, &handshake, "alpha").await;
        if searched["freshness"]["indexing"]["staleness_state"] == "parked" {
            break searched;
        }
        assert!(
            Instant::now() < deadline,
            "the unreadable worktree never parked: {searched}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(searched["status"], "unavailable", "{searched}");
    let parked = &searched["freshness"]["indexing"]["parked"];
    assert_eq!(
        (
            &parked["reason"],
            &parked["remediation"],
            &parked["retries_on_wake"],
        ),
        (&json!(CAUSE), &json!(REMEDY), &json!(false)),
        "search must carry the park, not only a retryable lane reason: {searched}"
    );

    let refused = tool(
        &socket,
        &handshake,
        "tracedecay_code_symbol_search",
        json!({
            "query": "alpha",
            "scope": { "path_prefix": null },
            "lazy_index_ignored_dependencies": false,
            "meta": { "projection": "summary", "order": "relevance" },
            "format": "json",
        }),
    )
    .await;
    let problem = &refused["problem"];
    assert_eq!(
        (
            &problem["code"],
            &problem["retryable"],
            &problem["retry"],
            &problem["legal_actions"],
        ),
        (
            &json!("application.code-index.parked"),
            &json!(false),
            &json!("never"),
            &json!(["reconcile"]),
        ),
        "symbol search on a parked worktree must refuse without retry: {refused}"
    );
    assert_eq!(
        problem["message"],
        format!("The code index for this worktree is parked; remedy: {REMEDY}; cause: {CAUSE}"),
        "the refusal must name the park's remedy and cause: {refused}"
    );

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644))
        .expect("restore the source mode");
    stop_daemon_gracefully(&mut daemon);
}
