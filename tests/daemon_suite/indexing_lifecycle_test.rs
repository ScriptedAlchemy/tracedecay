//! Mounted incremental-index lifecycle through the shipped daemon process.
//!
//! One journey exercises the production project composition, host hook ingress,
//! Git watcher, scheduler publication, query admission, graceful cancellation,
//! retained generation store, and physical daemon restart in their user-visible
//! order. Every indexing wait is bounded by a typed status/search/cadence
//! receipt; no elapsed-time sleep stands in for readiness.

use std::fmt::Write as _;
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
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_usecases::retention::code_index_generations::{
    DurablePublicationPointerV1, scoped_code_index_store_root,
};

use crate::common::{
    EnvVarGuard, IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with,
    tracedecay_command_with_home,
};

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactIndexIdentity {
    project_id: String,
    repository_id: String,
    worktree_id: String,
}

#[derive(Clone, Debug)]
struct TerminalGenerationReceipt {
    generation_id: String,
    status: Value,
    search: Value,
}

fn git(project: &Path, args: &[&str]) -> String {
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

fn commit_all(project: &Path, message: &str) -> String {
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

fn initialize_repository(project: &Path) -> (String, String) {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"incremental-lifecycle\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn lifecycle_main_symbol() -> &'static str { \"main\" }\n",
    )
    .expect("fixture main source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    let main_revision = commit_all(project, "initial main fixture");

    git(project, &["checkout", "--quiet", "-b", "feature/lifecycle"]);
    fs::write(
        project.join("src/branch.rs"),
        "pub fn lifecycle_branch_symbol() -> &'static str { \"feature\" }\n",
    )
    .expect("fixture branch source");
    let feature_revision = commit_all(project, "feature fixture");
    git(project, &["checkout", "--quiet", "main"]);
    (main_revision, feature_revision)
}

fn initialize_ignored_dependency_repository(project: &Path) -> String {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::create_dir_all(project.join("node_modules/pkg")).expect("dependency package directory");
    fs::create_dir_all(project.join("node_modules/unrelated"))
        .expect("unrelated package directory");
    fs::write(project.join(".gitignore"), "node_modules/\n").expect("fixture ignore rules");
    fs::write(
        project.join("src/app.ts"),
        "import type { LifecycleIgnoredDependency } from \"pkg\";\nexport function LifecycleGenerationAnchor(value: LifecycleIgnoredDependency) { return value; }\n",
    )
    .expect("fixture importer");
    fs::write(
        project.join("node_modules/pkg/index.d.ts"),
        "export interface LifecycleIgnoredDependency { value: string }\n",
    )
    .expect("dependency entrypoint");
    fs::write(
        project.join("node_modules/pkg/internal.ts"),
        "export interface BroadPackageLeak { leaked: true }\n",
    )
    .expect("ignored package sibling");
    fs::write(
        project.join("node_modules/unrelated/index.d.ts"),
        "export interface UnrelatedDependency { leaked: true }\n",
    )
    .expect("unrelated ignored package");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(project, "ignored dependency fixture")
}

fn initialize_tracedecay(home: &Path, project: &Path) -> String {
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

fn exact_identity(project: &Path, project_id: String) -> ExactIndexIdentity {
    let canonical_project = project.canonicalize().expect("canonical fixture project");
    let common_dir = tracedecay::worktree::git_common_dir(&canonical_project)
        .expect("fixture Git common directory");
    ExactIndexIdentity {
        project_id,
        repository_id: format!("repository.daemon.{}", sha256_path(&common_dir)),
        worktree_id: format!("worktree.daemon.{}", sha256_path(&canonical_project)),
    }
}

fn tool_payload(result: Value, operation: &str) -> Value {
    let text = result["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
            })
        })
        .unwrap_or_else(|| panic!("{operation} did not return JSON content: {result}"));
    text
}

async fn tool(socket: &Path, handshake: &DaemonHandshake, name: &str, arguments: Value) -> Value {
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

async fn status(socket: &Path, handshake: &DaemonHandshake) -> Value {
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

async fn search(socket: &Path, handshake: &DaemonHandshake, query: &str) -> Value {
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

async fn exact_symbol(
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

fn result_paths(search: &Value) -> Vec<&str> {
    search["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|result| result["display"]["path"].as_str())
        .collect()
}

fn assert_exact_identity(
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

async fn assert_project_identity(
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

#[allow(clippy::too_many_arguments)]
async fn wait_for_terminal_generation(
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
            "timed out waiting for terminal generation for {query}; status={last_status}; search={last_search}"
        )
    })
}

async fn deliver_save(project: &Path, paths: &[&str]) {
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

async fn inject_overflow(socket: &Path, handshake: &DaemonHandshake) {
    let receipt = tool(
        socket,
        handshake,
        "tracedecay_admin_sync",
        json!({ "force": true, "format": "json" }),
    )
    .await;
    assert_eq!(receipt["status"], "queued", "overflow receipt: {receipt}");
    assert_eq!(
        receipt["reconcile_scope"], "authoritative_project",
        "overflow must route to the mounted project authority: {receipt}"
    );
}

async fn wait_for_overflow_cadence_receipt(log_path: &Path) {
    let mut last = String::new();
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            last = fs::read_to_string(log_path).unwrap_or_default();
            let terminal_overflow = last.lines().any(|line| {
                line.contains("code_index_event_to_ready")
                    && (line.contains("trigger=overflow") || line.contains("trigger=\"overflow\""))
                    && line.contains("overflow_reconciled=true")
            });
            if terminal_overflow {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("overflow omitted its terminal cadence receipt; log={last}"));
}

fn write_cancellation_batch(project: &Path, scratch: &Path) {
    let batch = scratch.join("cancelled_batch_staging");
    fs::create_dir_all(&batch).expect("cancellation batch directory");
    for file_index in 0..768_u32 {
        let mut source = String::new();
        for symbol_index in 0..128_u32 {
            writeln!(
                source,
                "pub fn cancellation_probe_{file_index:04}_{symbol_index:03}(input: u32) -> u32 {{ input + {symbol_index} }}"
            )
            .expect("format cancellation source");
        }
        fs::write(batch.join(format!("file_{file_index:04}.rs")), source)
            .expect("write cancellation source");
    }
    fs::rename(&batch, project.join("src/cancelled_batch"))
        .expect("atomically install cancellation batch");
}

async fn wait_for_refreshing_old_generation(
    socket: &Path,
    handshake: &DaemonHandshake,
    project: &Path,
    identity: &ExactIndexIdentity,
    expected_reference: &str,
    expected_revision: Option<&str>,
    old_generation: &str,
) -> Value {
    let mut last = Value::Null;
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            last = status(socket, handshake).await;
            let worktree = &last["code_index_freshness"]["worktree"];
            if last["code_index_freshness"]["status"] == "warming"
                && worktree["latest_generation_id"] == old_generation
                && worktree["staleness_state"] == "refreshing"
                && worktree["coverage"] == "partial_refresh_in_progress"
            {
                assert_exact_identity(
                    &last,
                    project,
                    identity,
                    expected_reference,
                    expected_revision,
                );
                assert_project_identity(socket, handshake, project, identity).await;
                return last.clone();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("refresh never exposed its truthful in-flight receipt: {last}"))
}

fn read_active_generation(home: &Path, project: &Path) -> CodeIndexPublishedGenerationV1 {
    let marker = tracedecay::storage::read_enrollment_marker(project)
        .expect("read exact project enrollment marker")
        .expect("project enrollment marker must exist");
    let layout =
        tracedecay::storage::profile_sharded_layout(project, &home.join(".tracedecay"), &marker)
            .expect("profile-sharded project layout");
    let scope = scoped_code_index_store_root(&layout.data_root.join("code-index-v1"), project);
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &fs::read(scope.join("active-code-generation-v1.json"))
            .expect("active code generation pointer"),
    )
    .expect("valid active code generation pointer");
    CodeIndexPublishedGenerationV1::decode_sealed(
        &fs::read(
            scope
                .join("code-generations-v1")
                .join(pointer.generation_file),
        )
        .expect("sealed active code generation"),
    )
    .expect("active generation must be sealed and compatible")
}

fn assert_sealed_generation_identity(
    generation: &CodeIndexPublishedGenerationV1,
    identity: &ExactIndexIdentity,
    reference: &str,
    revision: Option<&str>,
    generation_id: &str,
) {
    assert_eq!(
        generation.manifest().project_id.as_str(),
        identity.project_id.as_str()
    );
    assert_eq!(generation.manifest().generation_id.as_str(), generation_id);
    assert_eq!(
        generation.snapshot().repository.as_str(),
        identity.repository_id.as_str()
    );
    assert_eq!(
        generation
            .snapshot()
            .worktree
            .as_ref()
            .map(|worktree| worktree.as_str()),
        Some(identity.worktree_id.as_str())
    );
    assert_eq!(
        generation
            .snapshot()
            .reference
            .as_ref()
            .map(|reference| reference.as_str()),
        Some(reference)
    );
    assert_eq!(
        generation
            .snapshot()
            .source_revision
            .as_ref()
            .map(|revision| revision.as_str()),
        revision
    );
}

fn assert_exact_ignored_dependency_roster(generation: &CodeIndexPublishedGenerationV1) {
    assert_eq!(
        generation
            .ignored_source_admissions()
            .iter()
            .map(|admission| admission.logical_path.as_str())
            .collect::<Vec<_>>(),
        ["node_modules/pkg/index.d.ts"],
        "only the verified package entrypoint may enter the durable roster"
    );
    assert_eq!(
        generation
            .snapshot()
            .files
            .iter()
            .filter(|file| file.logical_path.starts_with("node_modules/"))
            .map(|file| file.logical_path.as_str())
            .collect::<Vec<_>>(),
        ["node_modules/pkg/index.d.ts"],
        "lazy admission must not widen to package siblings or unrelated dependencies"
    );
    assert!(generation.symbols().symbols.iter().any(|symbol| {
        symbol.simple_name == "LifecycleIgnoredDependency"
            && symbol
                .qualified_name
                .ends_with("LifecycleIgnoredDependency")
    }));
    assert!(generation.symbols().symbols.iter().all(|symbol| {
        symbol.simple_name != "BroadPackageLeak" && symbol.simple_name != "UnrelatedDependency"
    }));
}

#[tokio::test]
async fn ignored_dependency_admission_survives_physical_daemon_restart_without_widening() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    let revision = initialize_ignored_dependency_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    let identity = exact_identity(&project, project_id);
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("production daemon handshake");

    let baseline = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "LifecycleGenerationAnchor",
        Some("src/app.ts"),
    )
    .await;
    let absent = exact_symbol(&socket, &handshake, "LifecycleIgnoredDependency", false).await;
    assert_eq!(
        absent["count"], 0,
        "dependency starts outside the index: {absent}"
    );

    let error = tokio::time::timeout(
        RECEIPT_TIMEOUT,
        call_tool(
            &socket,
            &handshake,
            "tracedecay_find_exact_symbol",
            json!({
                "name": "LifecycleIgnoredDependency",
                "limit": 5,
                "lazy_index_ignored_dependencies": true,
            }),
        ),
    )
    .await
    .expect("lazy admission timed out")
    .expect_err("generation-advancing admission must require a retry");
    assert!(
        error
            .to_string()
            .contains("advanced the graph generation; retry the request"),
        "lazy admission returned the wrong typed retry: {error}"
    );
    let advanced = read_active_generation(environment.home(), &project);
    assert_ne!(
        advanced.manifest().generation_id.as_str(),
        baseline.generation_id
    );
    assert_exact_ignored_dependency_roster(&advanced);
    let admitted = exact_symbol(&socket, &handshake, "LifecycleIgnoredDependency", true).await;
    assert_eq!(
        admitted["count"], 1,
        "retry must find one symbol: {admitted}"
    );
    let sealed = read_active_generation(environment.home(), &project);
    assert_eq!(
        sealed.manifest().generation_id.as_str(),
        advanced.manifest().generation_id.as_str(),
        "a positive retry must not schedule another generation"
    );
    assert_exact_ignored_dependency_roster(&sealed);

    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "stop daemon before physical restart");
    assert!(
        daemon
            .wait_for_exit(RECEIPT_TIMEOUT)
            .expect("wait for daemon shutdown")
            .expect("daemon exits after SIGTERM")
            .success()
    );
    daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let restored = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        None,
        None,
        "LifecycleIgnoredDependency",
        Some("node_modules/pkg/index.d.ts"),
    )
    .await;
    let retry_without_readmission =
        exact_symbol(&socket, &handshake, "LifecycleIgnoredDependency", false).await;
    assert_eq!(retry_without_readmission["count"], 1);
    let sealed = read_active_generation(environment.home(), &project);
    assert_eq!(
        sealed.manifest().generation_id.as_str(),
        restored.generation_id
    );
    assert_exact_ignored_dependency_roster(&sealed);

    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "stop restarted daemon");
    assert!(
        daemon
            .wait_for_exit(RECEIPT_TIMEOUT)
            .expect("wait for restarted daemon shutdown")
            .expect("restarted daemon exits after SIGTERM")
            .success()
    );
}

#[tokio::test]
async fn mounted_incremental_lifecycle_preserves_only_complete_compatible_generations() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    let (main_revision, feature_revision) = initialize_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let log_path = environment
        .scratch()
        .join("incremental-lifecycle-daemon.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |command| {
        command.env(
            "RUST_LOG",
            "tracedecay::daemon::code_index_scheduler::registry=debug",
        );
    });
    let project_id = initialize_tracedecay(environment.home(), &project);
    let identity = exact_identity(&project, project_id);
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("production daemon handshake");

    let initial = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&main_revision),
        None,
        "lifecycle_main_symbol",
        Some("src/lib.rs"),
    )
    .await;

    fs::write(
        project.join("src/saved.rs"),
        "pub fn lifecycle_saved_symbol() -> &'static str { \"saved\" }\n",
    )
    .expect("save source file");
    deliver_save(&project, &["src/saved.rs"]).await;
    // Dirty worktree generations keep ref/worktree identity but must not claim
    // HEAD as source_revision — that field is exact-commit evidence only.
    let saved = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        None,
        Some(&initial.generation_id),
        "lifecycle_saved_symbol",
        Some("src/saved.rs"),
    )
    .await;

    fs::rename(project.join("src/saved.rs"), project.join("src/renamed.rs"))
        .expect("rename source file");
    deliver_save(&project, &["src/saved.rs", "src/renamed.rs"]).await;
    let renamed = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        None,
        Some(&saved.generation_id),
        "lifecycle_saved_symbol",
        Some("src/renamed.rs"),
    )
    .await;
    assert!(
        !result_paths(&renamed.search).contains(&"src/saved.rs"),
        "rename retained the deleted logical path: {}",
        renamed.search
    );

    fs::remove_file(project.join("src/renamed.rs")).expect("delete renamed source file");
    deliver_save(&project, &["src/renamed.rs"]).await;
    let deleted = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&main_revision),
        Some(&renamed.generation_id),
        "lifecycle_saved_symbol",
        None,
    )
    .await;

    git(&project, &["checkout", "--quiet", "feature/lifecycle"]);
    // External checkout is not a hooked file-edit, and shell hooks are a typed
    // noop. Other ref-switch journeys request the same authoritative reconcile
    // the production CLI uses after an out-of-band git operation.
    inject_overflow(&socket, &handshake).await;
    let switched = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/feature/lifecycle",
        Some(&feature_revision),
        Some(&deleted.generation_id),
        "lifecycle_branch_symbol",
        Some("src/branch.rs"),
    )
    .await;

    fs::write(
        project.join("src/overflow.rs"),
        "pub fn lifecycle_overflow_symbol() -> &'static str { \"overflow\" }\n",
    )
    .expect("overflow source file");
    inject_overflow(&socket, &handshake).await;
    let overflowed = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        Some(&switched.generation_id),
        "lifecycle_overflow_symbol",
        Some("src/overflow.rs"),
    )
    .await;
    wait_for_overflow_cadence_receipt(&log_path).await;

    write_cancellation_batch(&project, environment.scratch());
    inject_overflow(&socket, &handshake).await;
    let refreshing = wait_for_refreshing_old_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        &overflowed.generation_id,
    )
    .await;
    assert_eq!(
        refreshing["code_index_freshness"]["worktree"]["latest_generation_id"],
        overflowed.generation_id,
        "a partial refresh must not replace the serving generation"
    );
    let stale = search(&socket, &handshake, "lifecycle_overflow_symbol").await;
    assert_eq!(
        stale["code_generation"], overflowed.generation_id,
        "in-flight refresh must serve the last complete generation: {stale}"
    );
    assert!(
        result_paths(&stale).contains(&"src/overflow.rs"),
        "last complete generation stopped serving during refresh: {stale}"
    );
    assert_eq!(
        stale["coverage"]["recall"], "partial",
        "serve-old query must report truthful partial recall: {stale}"
    );
    for lane in ["exact", "lexical", "graph"] {
        assert_eq!(
            stale["coverage"][lane]["status"], "stale",
            "{lane} lane did not report stale during refresh: {stale}"
        );
        assert_eq!(
            stale["coverage"][lane]["generation"], overflowed.generation_id,
            "{lane} lane attributed stale data to the wrong generation: {stale}"
        );
    }
    let pre_cancel = wait_for_refreshing_old_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        &overflowed.generation_id,
    )
    .await;
    assert_eq!(
        pre_cancel["code_index_freshness"]["worktree"]["latest_generation_id"],
        overflowed.generation_id,
        "cancellation must intersect the observed in-flight refresh"
    );

    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "send graceful cancellation to daemon");
    let exit = daemon
        .wait_for_exit(RECEIPT_TIMEOUT)
        .expect("wait for cancelled daemon")
        .expect("daemon must exit after SIGTERM");
    assert!(
        exit.success(),
        "daemon cancellation was not graceful: {exit}"
    );

    let retained = read_active_generation(environment.home(), &project);
    assert_sealed_generation_identity(
        &retained,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        &overflowed.generation_id,
    );
    assert!(
        retained
            .symbols()
            .symbols
            .iter()
            .all(|symbol| !symbol.simple_name.starts_with("cancellation_probe_")),
        "cancelled partial work leaked into the active sealed generation"
    );

    daemon = spawn_tracedecay_daemon_with(environment.home(), |command| {
        command.env(
            "RUST_LOG",
            "tracedecay::daemon::code_index_scheduler::registry=debug",
        );
    });
    let restarted = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        Some(&overflowed.generation_id),
        "cancellation_probe_0000_000",
        Some("src/cancelled_batch/file_0000.rs"),
    )
    .await;
    assert_eq!(
        restarted.status["code_index_freshness"]["status"], "current",
        "restart must publish only a current complete generation"
    );
    let recovered = read_active_generation(environment.home(), &project);
    assert_sealed_generation_identity(
        &recovered,
        &identity,
        "refs/heads/feature/lifecycle",
        None,
        &restarted.generation_id,
    );
    assert!(
        recovered.symbols().symbols.iter().any(|symbol| {
            symbol.simple_name == "cancellation_probe_0000_000"
                && symbol
                    .qualified_name
                    .ends_with("cancellation_probe_0000_000")
        }),
        "restart did not seal the recovered cancellation batch"
    );

    let signal_result = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "stop restarted daemon");
    let exit = daemon
        .wait_for_exit(RECEIPT_TIMEOUT)
        .expect("wait for restarted daemon")
        .expect("restarted daemon must exit after SIGTERM");
    assert!(
        exit.success(),
        "restarted daemon did not stop cleanly: {exit}"
    );
}
