//! RED/GREEN coverage for `tracedecay dashboard`'s handling of the daemon's
//! process-wide dashboard singleton (see `src/mcp/tools/handlers/dashboard.rs`).
//!
//! These fabricate the `tracedecay_dashboard` tool response directly instead
//! of spinning up a real daemon, so the assertions are on the reported
//! URL/status/exit behavior — never on timing — and can be pasted verbatim
//! as RED/GREEN evidence.
//!
//! The fixtures mirror the exact reproduction that motivated this fix: a
//! second `tracedecay dashboard --path /fast/projects/mold --port 7400`
//! call against a daemon already serving `/fast/projects/react-router` on
//! port 7399.

use std::path::Path;

use super::{DashboardStartOutcome, interpret_dashboard_start_response};

fn mold_request() -> (&'static Path, &'static str, u16) {
    (Path::new("/fast/projects/mold"), "127.0.0.1", 7400)
}

#[test]
fn started_response_is_reported_as_started() {
    let result = serde_json::json!({
        "status": "started",
        "url": "http://127.0.0.1:7400/",
        "host": "127.0.0.1",
        "port": 7400,
        "project_root": "/fast/projects/mold",
    });
    let (project_path, host, port) = mold_request();

    let outcome = interpret_dashboard_start_response(&result, project_path, host, port)
        .expect("a `started` response must be interpretable");

    assert_eq!(
        outcome,
        DashboardStartOutcome::Started {
            url: "http://127.0.0.1:7400/".to_string(),
        }
    );
}

#[test]
fn already_running_response_matching_the_request_is_a_truthful_no_op() {
    // Same host, same port, same project as what was requested: genuinely
    // idempotent, not a conflict.
    let result = serde_json::json!({
        "status": "already_running",
        "url": "http://127.0.0.1:7400/",
        "host": "127.0.0.1",
        "port": 7400,
        "project_root": "/fast/projects/mold",
        "requested_host": "127.0.0.1",
        "requested_port": 7400,
        "requested_project_root": "/fast/projects/mold",
        "matches_request": true,
    });
    let (project_path, host, port) = mold_request();

    let outcome = interpret_dashboard_start_response(&result, project_path, host, port)
        .expect("a matching already_running response must be interpretable");

    assert_eq!(
        outcome,
        DashboardStartOutcome::AlreadyServingRequest {
            url: "http://127.0.0.1:7400/".to_string(),
            project_root: "/fast/projects/mold".to_string(),
        }
    );
}

/// RED (pre-fix behavior, reconstructed here): the CLI printed
///   "tracedecay dashboard listening on http://127.0.0.1:7399/"
///   "Serving project /fast/projects/mold"
/// and exited 0 — the URL was real, but the "Serving project mold" line was
/// false (react-router's store was still the one being served) and the
/// explicitly requested port 7400 was silently discarded with no error.
///
/// GREEN (this test, post-fix): the exact same daemon response must be
/// reported as a `Conflict`, never as success, and the resulting message
/// must name the ACTUAL project/URL being served (react-router / 7399) and
/// say plainly that the requested port and project were not started.
#[test]
fn already_running_response_for_a_different_project_is_reported_as_a_conflict_not_a_falsehood() {
    let result = serde_json::json!({
        "status": "already_running",
        "url": "http://127.0.0.1:7399/",
        "host": "127.0.0.1",
        "port": 7399,
        "project_root": "/fast/projects/react-router",
        "requested_host": "127.0.0.1",
        "requested_port": 7400,
        "requested_project_root": "/fast/projects/mold",
        "matches_request": false,
    });
    let (project_path, host, port) = mold_request();

    let outcome = interpret_dashboard_start_response(&result, project_path, host, port)
        .expect("an already_running response must still be interpretable, not an Err");

    let DashboardStartOutcome::Conflict { message } = outcome else {
        panic!("expected a Conflict outcome for a mismatched already_running response, got {outcome:?}");
    };

    // The message must name the truth (react-router at 7399)...
    assert!(
        message.contains("react-router"),
        "conflict message must name the project actually being served: {message}"
    );
    assert!(
        message.contains("7399"),
        "conflict message must name the URL/port actually reachable: {message}"
    );
    // ...and must not claim the requested port/project were honored.
    assert!(
        message.contains("7400") && message.contains("mold"),
        "conflict message must name the requested host/port/project that were NOT started: {message}"
    );
    assert!(
        message.contains("NOT started") || message.contains("were not"),
        "conflict message must say plainly that the request was not fulfilled: {message}"
    );
}

#[test]
fn stopping_status_mismatched_with_the_request_is_also_a_conflict() {
    let result = serde_json::json!({
        "status": "stopping",
        "url": "http://127.0.0.1:7399/",
        "host": "127.0.0.1",
        "port": 7399,
        "project_root": "/fast/projects/react-router",
        "matches_request": false,
    });
    let (project_path, host, port) = mold_request();

    let outcome = interpret_dashboard_start_response(&result, project_path, host, port)
        .expect("a stopping-status response must still be interpretable");

    let DashboardStartOutcome::Conflict { message } = outcome else {
        panic!("expected a Conflict outcome while the prior dashboard is shutting down, got {outcome:?}");
    };
    assert!(
        message.contains("shutting down"),
        "conflict message should reflect the in-progress shutdown: {message}"
    );
}

#[test]
fn a_response_missing_url_is_an_error_not_a_silent_success() {
    let result = serde_json::json!({ "status": "started" });
    let (project_path, host, port) = mold_request();

    let error = interpret_dashboard_start_response(&result, project_path, host, port)
        .expect_err("a response with no URL must not be treated as success");
    assert!(error.to_string().contains("omitted URL"));
}
