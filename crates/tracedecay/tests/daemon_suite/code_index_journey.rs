//! Shared fixture surface for code-index journeys through the shipped daemon
//! process: a real Git repository registered with `tracedecay init`, host hook
//! ingress, and typed status/search receipts that bound every indexing wait.
//! No elapsed-time sleep stands in for readiness.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay::daemon::{
    DaemonHandshake, DaemonHookEvent, HookAgent, HookEventNotifyOutcomeV1, call_tool,
    notify_hook_event,
};

use crate::common::{DaemonProcess, tracedecay_command_with_home};

pub const RECEIPT_TIMEOUT: Duration = Duration::from_secs(45);

pub fn daemon_log_for_failure() -> String {
    let Some(path) = std::env::var_os("TRACEDECAY_TEST_DAEMON_LOG") else {
        return "daemon log path unavailable".to_owned();
    };
    fs::read_to_string(&path).unwrap_or_else(|error| {
        format!(
            "failed to read daemon log '{}': {error}",
            Path::new(&path).display()
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactIndexIdentity {
    pub project_id: String,
    pub repository_id: String,
    pub worktree_id: String,
}

#[derive(Clone, Debug)]
pub struct TerminalGenerationReceipt {
    pub generation_id: String,
    pub status: Value,
    pub search: Value,
}

pub fn git(project: &Path, args: &[&str]) -> String {
    let output = Command::new(crate::common::git_program())
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "gc.autoDetach=false"])
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git emitted UTF-8")
        .trim()
        .to_owned()
}

pub fn commit_all(project: &Path, message: &str) -> String {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
    git(project, &["rev-parse", "HEAD"])
}

fn sha256_path(path: &Path) -> String {
    hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))
}

pub fn initialize_tracedecay(home: &Path, project: &Path) -> String {
    let output = tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert!(
        output.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let context = tracedecay_command_with_home(home)
        .args(["projects", "context"])
        .arg(project)
        .arg("--json")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("read registered project identity");
    assert!(
        context.status.success(),
        "project context failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&context.stdout),
        String::from_utf8_lossy(&context.stderr)
    );
    let context: Value = serde_json::from_slice(&context.stdout).expect("project context JSON");
    context["project"]["project_id"]
        .as_str()
        .expect("registered project id")
        .to_owned()
}

pub fn exact_identity(project: &Path, project_id: String) -> ExactIndexIdentity {
    let canonical_project = project.canonicalize().expect("canonical fixture project");
    let common_dir = tracedecay_runtime_core::worktree::git_common_dir(&canonical_project)
        .expect("fixture Git common directory");
    ExactIndexIdentity {
        project_id,
        repository_id: format!("repository.daemon.{}", sha256_path(&common_dir)),
        worktree_id: format!("worktree.daemon.{}", sha256_path(&canonical_project)),
    }
}

fn tool_payload(result: Value, operation: &str) -> Value {
    result["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
            })
        })
        .unwrap_or_else(|| panic!("{operation} did not return JSON content: {result}"))
}

pub async fn tool(
    socket: &Path,
    handshake: &DaemonHandshake,
    name: &str,
    arguments: Value,
) -> Value {
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        let result = tokio::time::timeout(
            RECEIPT_TIMEOUT,
            call_tool(socket, handshake, name, arguments.clone()),
        )
        .await
        .unwrap_or_else(|_| panic!("{name} timed out"));
        match result {
            Ok(payload) => return tool_payload(payload, name),
            Err(error)
                if Instant::now() < deadline
                    && (error.to_string().contains("warming in the background")
                        || error
                            .to_string()
                            .contains("retired before response completion")) =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("{name} transport failed: {error}"),
        }
    }
}

pub async fn status(socket: &Path, handshake: &DaemonHandshake) -> Value {
    tool(
        socket,
        handshake,
        "tracedecay_status",
        json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        }),
    )
    .await
}

async fn project_context(socket: &Path, handshake: &DaemonHandshake, project: &Path) -> Value {
    tool(
        socket,
        handshake,
        "tracedecay_project_context",
        json!({
            "path": project,
            "format": "json",
        }),
    )
    .await
}

pub async fn search(socket: &Path, handshake: &DaemonHandshake, query: &str) -> Value {
    let payload = tool(
        socket,
        handshake,
        "tracedecay_search",
        json!({
            "query": query,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;
    resolve_truncated_tool_payload(socket, handshake, payload).await
}

/// Large `tracedecay_search` bodies are replaced by a handle envelope once they
/// exceed the MCP response cap. Journey waits read `code_generation` / `results`
/// from the stored original, not the truncated preview.
async fn resolve_truncated_tool_payload(
    socket: &Path,
    handshake: &DaemonHandshake,
    payload: Value,
) -> Value {
    if payload.get("truncated") != Some(&json!(true)) {
        return payload;
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("truncated search omitted retrieve handle: {payload}"));
    let retrieved = tool(
        socket,
        handshake,
        "tracedecay_retrieve",
        json!({
            "handle": handle,
            "format": "json",
        }),
    )
    .await;
    retrieved["content"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("truncated search handle did not retrieve JSON: {retrieved}"))
}

pub async fn exact_symbol(
    socket: &Path,
    handshake: &DaemonHandshake,
    name: &str,
    lazy_index: bool,
) -> Value {
    tool(
        socket,
        handshake,
        "tracedecay_find_exact_symbol",
        json!({
            "name": name,
            "limit": 5,
            "lazy_index_ignored_dependencies": lazy_index,
            "format": "json",
        }),
    )
    .await
}

pub fn result_paths(search: &Value) -> Vec<&str> {
    search["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|result| result["display"]["path"].as_str())
        .collect()
}

pub fn assert_exact_identity(
    status: &Value,
    project: &Path,
    identity: &ExactIndexIdentity,
    expected_reference: &str,
    expected_revision: Option<&str>,
) {
    let canonical_project = project.canonicalize().expect("canonical project receipt");
    assert_eq!(
        status["project_root"].as_str(),
        canonical_project.to_str(),
        "status must name the exact mounted project: {status}"
    );
    let worktree = &status["code_index_freshness"]["worktree"];
    assert_eq!(
        worktree["worktree_root"].as_str(),
        canonical_project.to_str(),
        "freshness must name the exact mounted worktree: {status}"
    );
    assert_eq!(
        worktree["repository_id"].as_str(),
        Some(identity.repository_id.as_str()),
        "freshness must retain exact repository identity: {status}"
    );
    assert_eq!(
        worktree["worktree_id"].as_str(),
        Some(identity.worktree_id.as_str()),
        "freshness must retain exact worktree identity: {status}"
    );
    assert_eq!(
        worktree["source_reference"].as_str(),
        Some(expected_reference),
        "freshness must retain exact ref identity: {status}"
    );
    assert_eq!(
        worktree["source_revision"].as_str(),
        expected_revision,
        "freshness must retain exact source revision: {status}"
    );
}

pub async fn assert_project_identity(
    socket: &Path,
    handshake: &DaemonHandshake,
    project: &Path,
    identity: &ExactIndexIdentity,
) {
    let context = project_context(socket, handshake, project).await;
    assert_eq!(context["status"], "ok", "project context: {context}");
    assert_eq!(
        context["project"]["project_id"].as_str(),
        Some(identity.project_id.as_str()),
        "terminal receipt crossed project identity: {context}"
    );
    assert_eq!(
        context["project"]["canonical_root"].as_str(),
        project.canonicalize().expect("canonical project").to_str(),
        "terminal receipt crossed project root: {context}"
    );
}

/// Reads typed freshness receipts until the daemon serves a complete, fresh
/// generation that differs from `prior_generation`, carries the expected ref and
/// source revision, and answers `query` from that same generation.
#[allow(clippy::too_many_arguments)]
pub async fn wait_for_terminal_generation(
    socket: &Path,
    handshake: &DaemonHandshake,
    project: &Path,
    identity: &ExactIndexIdentity,
    expected_reference: &str,
    expected_revision: Option<&str>,
    prior_generation: Option<&str>,
    query: &str,
    expected_path: Option<&str>,
) -> TerminalGenerationReceipt {
    let mut last_status = Value::Null;
    let mut last_search = Value::Null;
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            let observed = status(socket, handshake).await;
            let worktree = &observed["code_index_freshness"]["worktree"];
            let generation = worktree["latest_generation_id"]
                .as_str()
                .map(str::to_owned);
            let revision_matches = worktree["source_revision"].as_str() == expected_revision;
            let terminal = observed["code_index_freshness"]["status"] == "current"
                && worktree["coverage"] == "complete"
                && worktree["staleness_state"] == "fresh"
                && worktree["source_reference"] == expected_reference
                && revision_matches
                && generation.is_some()
                && prior_generation.is_none_or(|prior| generation.as_deref() != Some(prior));
            last_status = observed;
            if !terminal {
                continue;
            }
            let observed_search = search(socket, handshake, query).await;
            last_search = observed_search;
            if last_search["code_generation"].as_str() != generation.as_deref() {
                continue;
            }
            let paths = result_paths(&last_search);
            let query_matches = expected_path
                .map_or_else(|| paths.is_empty(), |expected| paths.contains(&expected));
            if !query_matches {
                continue;
            }
            assert_exact_identity(
                &last_status,
                project,
                identity,
                expected_reference,
                expected_revision,
            );
            assert_project_identity(socket, handshake, project, identity).await;
            for lane in ["exact", "lexical", "graph"] {
                assert_eq!(
                    last_search["coverage"][lane], "complete",
                    "terminal query must have complete {lane} coverage: {last_search}"
                );
            }
            return TerminalGenerationReceipt {
                generation_id: generation.expect("terminal generation"),
                status: last_status.clone(),
                search: last_search.clone(),
            };
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for terminal generation for {query}; status={last_status}; search={last_search}; daemon_log={}",
            daemon_log_for_failure()
        )
    })
}

pub async fn deliver_save(project: &Path, paths: &[&str]) {
    let outcome = notify_hook_event(
        project,
        DaemonHookEvent::post_tool_use_edit(
            HookAgent::Codex,
            paths.iter().map(|path| (*path).to_owned()).collect(),
            project.to_path_buf(),
        ),
    )
    .await;
    assert_eq!(
        outcome,
        HookEventNotifyOutcomeV1::Delivered,
        "production hook notification was not delivered"
    );
}

/// Graceful SIGTERM shutdown within the journey's receipt bound.
pub fn stop_daemon_gracefully(daemon: &mut DaemonProcess) {
    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "send SIGTERM to daemon");
    let exit = daemon
        .wait_for_exit(RECEIPT_TIMEOUT)
        .expect("wait for daemon shutdown")
        .unwrap_or_else(|| {
            panic!(
                "daemon must exit after SIGTERM; daemon_log={}",
                daemon_log_for_failure()
            )
        });
    assert!(exit.success(), "daemon did not stop cleanly: {exit}");
}
