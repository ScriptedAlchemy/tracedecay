//! Transport parity for `tracedecay tool <application surface>`.
//!
//! Every application-surface operation requires a project route on the daemon
//! side (`DaemonInvocationRequest::requires_project`). The compatibility tool
//! path resolves that route by walking up from the working directory, so
//! `tracedecay tool circular` works from a checkout without `--project`. The
//! typed application-surface path must present the same authenticated route:
//! otherwise `storage_status`, `source_outline`, and the git
//! reads answer `application.surface.unavailable` /
//! `not_found_or_not_authorized` from a checkout the operator is standing in.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::common::fixture::{
    TYPESCRIPT_FIXTURE_TSC_INVOCATIONS, TYPESCRIPT_MONOREPO_APP_FILE, TypeScriptFixtureCompiler,
    write_typescript_diagnostics_fixture, write_typescript_monorepo_diagnostics_fixture,
};
use crate::common::{
    canonical_existing_path, git_program, spawn_tracedecay_daemon, tracedecay_command_with_home,
};
use serde_json::Value;
use tempfile::TempDir;

/// The CLI must reach the daemon, resolve the project, and answer within this
/// bound. A genuine authority regression fails immediately with a problem
/// envelope rather than hanging, so this only guards against a hang.
const SURFACE_TIMEOUT: Duration = Duration::from_secs(60);

fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new(git_program())
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} should run: {error}"));
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Creates a committed git worktree and indexes it, so the daemon has both a
/// registered project store and an authenticated worktree route.
fn init_indexed_git_project(home: &Path, project: &Path) {
    std::fs::create_dir_all(project.join("src/nested")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub mod nested;\npub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/nested/mod.rs"),
        "pub fn nested_answer() -> u32 { 7 }\n",
    )
    .unwrap();
    git(project, &["init", "--initial-branch=master"]);
    git(project, &["config", "user.email", "surface@example.com"]);
    git(project, &["config", "user.name", "Surface Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);

    crate::common::initialize_tracedecay_cli_project(home, project);
}

struct SurfaceOutcome {
    success: bool,
    stdout: String,
    stderr: String,
}

impl SurfaceOutcome {
    fn payload(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "surface output should be JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                self.stdout, self.stderr
            )
        })
    }

    fn problem_code(&self) -> Option<String> {
        self.payload()
            .get("problem")
            .and_then(|problem| problem.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

#[test]
fn tool_dry_run_reads_piped_args_in_either_order() {
    let home = TempDir::new().expect("isolated home");
    let project = TempDir::new().expect("working directory");
    for trailing_args in [["--args", "-", "--dry-run"], ["--dry-run", "--args", "-"]] {
        let mut command = tracedecay_command_with_home(home.path());
        command
            .current_dir(project.path())
            .args(["tool", "storage_status"])
            .args(trailing_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("tool dry-run should spawn");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(b"{}")
            .expect("write tool arguments");
        let output = child.wait_with_output().expect("collect tool dry-run");
        assert!(
            output.status.success(),
            "tool dry-run {trailing_args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
    }
}

/// The diagnostics read answers to its MCP spelling, with or without the
/// `tracedecay_` prefix, and per-key flags parse against its advertised schema.
#[test]
fn tool_dry_run_resolves_diagnostics_by_mcp_spelling() {
    let home = TempDir::new().expect("isolated home");
    let project = TempDir::new().expect("working directory");
    for name in ["diagnostics", "tracedecay_diagnostics"] {
        let output = tracedecay_command_with_home(home.path())
            .current_dir(project.path())
            .args(["tool", name, "--scope", "workspace", "--dry-run"])
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("tool {name} dry-run should run: {error}"));
        assert!(
            output.status.success(),
            "tool {name} dry-run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let parsed: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("tool {name} dry-run must print JSON: {error}"));
        assert_eq!(
            parsed,
            serde_json::json!({ "scope": "workspace" }),
            "{name}"
        );
    }
}

/// Runs `tracedecay tool <tool>` with the given working directory and **no**
/// `--project`, exactly as an agent or operator standing in a checkout does.
fn run_surface_tool_from(
    home: &Path,
    working_directory: &Path,
    tool: &str,
    args: &str,
) -> SurfaceOutcome {
    let mut command = tracedecay_command_with_home(home);
    command
        .current_dir(working_directory)
        .args(["tool", tool, "--args", args])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("tracedecay tool {tool} should spawn: {error}"));
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll tool child").is_some() {
            break;
        }
        assert!(
            started.elapsed() < SURFACE_TIMEOUT,
            "tracedecay tool {tool} hung for {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().expect("collect tool output");
    SurfaceOutcome {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Runs the first-class Git read command from an admitted checkout. Unlike the
/// generic `tool` escape hatch, this is the operator-facing journey for
/// catalogued Git intelligence. It shares the same warm-up retry transport as
/// the typed surface path, so a cold daemon must not surface a terminal error.
fn run_git_read_from(
    home: &Path,
    working_directory: &Path,
    command_args: &[&str],
) -> SurfaceOutcome {
    let mut command = tracedecay_command_with_home(home);
    command
        .current_dir(working_directory)
        .arg("git")
        .args(command_args)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("tracedecay git {command_args:?} should spawn: {error}"));
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll git child").is_some() {
            break;
        }
        assert!(
            started.elapsed() < SURFACE_TIMEOUT,
            "tracedecay git {command_args:?} hung for {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().expect("collect git output");
    SurfaceOutcome {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn assert_surface_resolves_project(
    home: &Path,
    working_directory: &Path,
    tool: &str,
    args: &str,
) -> Value {
    let outcome = run_surface_tool_from(home, working_directory, tool, args);
    assert_eq!(
        outcome.problem_code(),
        None,
        "`tracedecay tool {tool}` from {} must resolve the surrounding project instead of \
         reporting a problem\nstdout:\n{}\nstderr:\n{}",
        working_directory.display(),
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.success,
        "`tracedecay tool {tool}` from {} should succeed\nstdout:\n{}\nstderr:\n{}",
        working_directory.display(),
        outcome.stdout,
        outcome.stderr
    );
    let payload = outcome.payload();
    assert!(
        payload.get("scope").is_some(),
        "`{tool}` must answer with an authenticated scope, got:\n{}",
        outcome.stdout
    );
    payload
}

fn surface_fixture() -> (TempDir, TempDir, PathBuf, PathBuf) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_indexed_git_project(&home_path, &project_path);
    (home, project, home_path, project_path)
}

#[test]
fn application_surface_primitive_tools_resolve_the_working_directory_project() {
    let (_home, _project, home_path, project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);

    assert_surface_resolves_project(
        &home_path,
        &project_path,
        "storage_status",
        r#"{"format":"json"}"#,
    );
    assert_surface_resolves_project(
        &home_path,
        &project_path,
        "source_outline",
        r#"{"file":"src/lib.rs","format":"json"}"#,
    );
}

#[test]
fn first_class_git_reads_wait_for_full_publication_then_dispatch() {
    let (_home, _project, home_path, project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);

    for command_args in [
        ["status"].as_slice(),
        ["history", "--count", "1"].as_slice(),
        ["blame", "--path", "src/lib.rs"].as_slice(),
    ] {
        let outcome = run_git_read_from(&home_path, &project_path, command_args);
        assert!(
            outcome.success,
            "first-class git {} must reach the fully published daemon-owned application \
             route\nstdout:\n{}\nstderr:\n{}",
            command_args[0], outcome.stdout, outcome.stderr
        );
        let payload = outcome.payload();
        assert!(
            payload.get("scope").is_some(),
            "first-class git {} must preserve the authenticated scope, got:\n{}",
            command_args[0],
            outcome.stdout
        );
    }
}

/// The first-class Git commands must present the same typed denial as the
/// tool escape hatch when the working directory is not an admitted project.
#[test]
fn first_class_git_reads_do_not_invent_a_project_outside_a_checkout() {
    let (_home, _project, home_path, _project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);
    let outside = TempDir::new().unwrap();
    let outside_path = canonical_existing_path(outside.path());

    let outcome = run_git_read_from(&home_path, &outside_path, &["status"]);
    assert!(
        !outcome.success,
        "a directory that is not inside a project must not answer as an authorized \
         worktree\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
}

#[test]
fn application_surface_tools_resolve_the_project_from_a_subdirectory() {
    let (_home, _project, home_path, project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);
    let nested = project_path.join("src/nested");

    let payload = assert_surface_resolves_project(
        &home_path,
        &nested,
        "storage_status",
        r#"{"format":"json"}"#,
    );
    let from_root = assert_surface_resolves_project(
        &home_path,
        &project_path,
        "storage_status",
        r#"{"format":"json"}"#,
    );
    assert_eq!(
        payload["scope"]["project_id"], from_root["scope"]["project_id"],
        "a subdirectory must bind the same project route as the checkout root"
    );
    assert_surface_resolves_project(&home_path, &nested, "git_status", r#"{"format":"json"}"#);
}

/// Commits a TypeScript checkout and initializes it, so the daemon admits the
/// project and, when the project carries its own compiler, its diagnostics
/// producer.
fn init_typescript_project(home: &Path, project: &Path, write_fixture: impl FnOnce(&Path)) {
    write_fixture(project);
    git(project, &["init", "--initial-branch=master"]);
    git(project, &["config", "user.email", "surface@example.com"]);
    git(project, &["config", "user.name", "Surface Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
    crate::common::initialize_tracedecay_cli_project(home, project);
}

const DIAGNOSTICS_FILE_ARGS: &str = r#"{"scope":"file","path":"src/index.ts","format":"json"}"#;

/// Polls `tracedecay tool diagnostics` until the daemon's producer has
/// published for the current generation; pending is the only state worth
/// waiting through.
fn await_published_cli_diagnostics(home: &Path, project: &Path, args: &str) -> serde_json::Value {
    let started = Instant::now();
    loop {
        let outcome = run_surface_tool_from(home, project, "diagnostics", args);
        match outcome.problem_code().as_deref() {
            None => break outcome.payload(),
            Some("application.diagnostics.pending" | "application.diagnostics.stale") => {
                assert!(
                    started.elapsed() < SURFACE_TIMEOUT,
                    "the TypeScript producer did not publish within {:?}\nstdout:\n{}",
                    started.elapsed(),
                    outcome.stdout
                );
                std::thread::sleep(Duration::from_millis(250));
            }
            Some(code) => panic!(
                "the producer reported a terminal state instead of publishing: {code}\nstdout:\n{}\nstderr:\n{}",
                outcome.stdout, outcome.stderr
            ),
        }
    }
}

/// The CLI fallback the MCP error text names must answer exactly like the
/// MCP tool: a fresh TypeScript project with its own `tsc` yields the
/// compiler's `TS4023` on the file once the daemon's producer has published.
#[test]
fn tool_diagnostics_reads_the_typescript_producer_publication() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_typescript_project(&home_path, &project_path, |project| {
        write_typescript_diagnostics_fixture(project, TypeScriptFixtureCompiler::Present);
    });
    let _daemon = spawn_tracedecay_daemon(&home_path);

    let payload = await_published_cli_diagnostics(&home_path, &project_path, DIAGNOSTICS_FILE_ARGS);
    let records = payload["outcome"]["value"]["payload"]["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("diagnostics evidence lists its records: {payload}"));
    assert_eq!(records.len(), 1, "{payload}");
    assert_eq!(records[0]["logical_path"], "src/index.ts", "{payload}");
    assert_eq!(records[0]["diagnostic"]["code"], "TS4023", "{payload}");
    assert_eq!(records[0]["diagnostic"]["severity"], "error", "{payload}");

    let invocations =
        std::fs::read_to_string(project_path.join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS))
            .expect("the fixture compiler records every invocation");
    let root = project_path.display();
    assert!(
        invocations
            .lines()
            .all(|line| line == format!("{root} -p {root}/tsconfig.json --noEmit --pretty false")),
        "the producer runs the project's own tsc from the project root: {invocations}"
    );
}

/// Before `npm install` the same read is a typed refusal that carries the
/// exact setup command and a legal action.
#[test]
fn tool_diagnostics_names_the_install_command_without_a_compiler() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_typescript_project(&home_path, &project_path, |project| {
        write_typescript_diagnostics_fixture(project, TypeScriptFixtureCompiler::Missing);
    });
    let _daemon = spawn_tracedecay_daemon(&home_path);

    let outcome = run_surface_tool_from(
        &home_path,
        &project_path,
        "diagnostics",
        DIAGNOSTICS_FILE_ARGS,
    );
    assert_eq!(
        outcome.problem_code().as_deref(),
        Some("application.diagnostics.producer-missing"),
        "stdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr
    );
    let problem = &outcome.payload()["problem"];
    assert_eq!(
        problem["legal_actions"],
        serde_json::json!(["refresh"]),
        "{problem}"
    );
    assert!(
        problem["message"]
            .as_str()
            .is_some_and(|message| message.contains("`npm install --save-dev typescript`")),
        "{problem}"
    );
}

const MONOREPO_APP_FILE_ARGS: &str =
    r#"{"scope":"file","path":"packages/app/src/index.ts","format":"json"}"#;

/// Issue #2025 through the CLI fallback: a pnpm monorepo with per-package
/// tsconfigs and no root `tsconfig.json` publishes a package's `TS4023`.
#[test]
fn tool_diagnostics_reads_a_monorepo_package_finding() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_typescript_project(&home_path, &project_path, |project| {
        write_typescript_monorepo_diagnostics_fixture(project, TypeScriptFixtureCompiler::Present);
    });
    let _daemon = spawn_tracedecay_daemon(&home_path);

    let payload =
        await_published_cli_diagnostics(&home_path, &project_path, MONOREPO_APP_FILE_ARGS);
    let records = payload["outcome"]["value"]["payload"]["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("diagnostics evidence lists its records: {payload}"));
    assert_eq!(records.len(), 1, "{payload}");
    assert_eq!(
        records[0]["logical_path"], TYPESCRIPT_MONOREPO_APP_FILE,
        "{payload}"
    );
    assert_eq!(records[0]["diagnostic"]["code"], "TS4023", "{payload}");
}

/// The issue's reported command and state, dependencies not installed: the
/// CLI names the owning tsconfig and `pnpm install`, never "no tsconfig.json".
#[test]
fn tool_diagnostics_names_pnpm_install_for_an_uninstalled_monorepo() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_typescript_project(&home_path, &project_path, |project| {
        write_typescript_monorepo_diagnostics_fixture(project, TypeScriptFixtureCompiler::Missing);
    });
    let _daemon = spawn_tracedecay_daemon(&home_path);

    let outcome = run_surface_tool_from(
        &home_path,
        &project_path,
        "diagnostics",
        MONOREPO_APP_FILE_ARGS,
    );
    assert_eq!(
        outcome.problem_code().as_deref(),
        Some("application.diagnostics.producer-missing"),
        "stdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr
    );
    let problem = &outcome.payload()["problem"];
    assert_eq!(
        problem["legal_actions"],
        serde_json::json!(["refresh"]),
        "{problem}"
    );
    assert!(
        problem["message"].as_str().is_some_and(|message| {
            message.contains("`packages/app/tsconfig.json`") && message.contains("`pnpm install`")
        }),
        "{problem}"
    );
}

/// The filesystem root is not a project. A surface call from there must keep
/// reporting the typed unavailable/unauthorized state rather than inventing a
/// project from an unrelated ancestor directory.
#[test]
fn application_surface_tools_do_not_invent_a_project_outside_a_checkout() {
    let (_home, _project, home_path, _project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);
    let outside = TempDir::new().unwrap();
    let outside_path = canonical_existing_path(outside.path());

    let outcome = run_surface_tool_from(
        &home_path,
        &outside_path,
        "git_status",
        r#"{"format":"json"}"#,
    );
    assert!(
        !outcome.success,
        "a directory that is not inside a project must not answer as an authorized \
         worktree\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    assert!(
        outcome.problem_code().is_some(),
        "an unresolved project must surface a typed problem, got:\n{}",
        outcome.stdout
    );
}

/// The executable binding id and result schema a Work or Workflow tool's
/// typed envelope must carry.
fn family_binding(
    registry: &tracedecay_tool_catalog::ExecutableBindingRegistryV1,
    operation_id: &str,
) -> (String, String) {
    let operation_id = tracedecay_tool_catalog::OperationId::new(operation_id.to_owned()).unwrap();
    let binding = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .unwrap_or_else(|| panic!("{} is not executable", operation_id.as_str()));
    let (binding_id, _) = binding
        .public_route()
        .unwrap_or_else(|| panic!("{} has no public route", operation_id.as_str()));
    (
        binding_id.as_str().to_owned(),
        binding
            .result_schema()
            .schema_ref()
            .schema_id()
            .as_str()
            .to_owned(),
    )
}

fn assert_cli_family_envelope(
    outcome: &SurfaceOutcome,
    tool: &str,
    kind: &str,
    schema_id: &str,
    binding_id: Option<&str>,
) -> Value {
    let payload = outcome.payload();
    assert_eq!(
        payload["kind"], kind,
        "`tracedecay tool {tool}` must answer with the family's typed envelope\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    let value = &payload["value"];
    assert_eq!(value["contract"]["schema_id"], schema_id, "{tool}");
    assert_eq!(value["binding_id"].as_str(), binding_id, "{tool}");
    let request_id = value["request_id"].as_str().unwrap_or_default();
    assert!(
        request_id.starts_with("request.cli."),
        "`tracedecay tool {tool}` must reach the owner as a CLI request, not a daemon MCP \
         tool call; request_id was {request_id:?}"
    );
    payload
}

/// `tracedecay tool` runs Work and Workflow through their canonical owner over
/// the daemon socket: the answer is the family's typed envelope, bound to its
/// executable result contract, under the CLI's own request identity.
#[test]
fn work_and_workflow_tools_answer_through_their_typed_owner() {
    let (_home, _project, home_path, project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);
    let work = tracedecay_contracts::work_executable_binding_registry().unwrap();
    let workflow = tracedecay_contracts::workflow_executable_binding_registry().unwrap();

    let listed = run_surface_tool_from(
        &home_path,
        &project_path,
        "tracedecay_work_list_attempts",
        r#"{"page_size":10}"#,
    );
    assert!(
        listed.success,
        "stdout:\n{}\nstderr:\n{}",
        listed.stdout, listed.stderr
    );
    let (binding_id, schema_id) = family_binding(work, "operation.work.list_attempts");
    assert_cli_family_envelope(
        &listed,
        "tracedecay_work_list_attempts",
        "success",
        &schema_id,
        Some(&binding_id),
    );

    let definitions =
        run_surface_tool_from(&home_path, &project_path, "workflow_list_definitions", "{}");
    assert!(
        definitions.success,
        "stdout:\n{}\nstderr:\n{}",
        definitions.stdout, definitions.stderr
    );
    let (binding_id, schema_id) = family_binding(workflow, "operation.workflow.list_definitions");
    assert_cli_family_envelope(
        &definitions,
        "workflow_list_definitions",
        "success",
        &schema_id,
        Some(&binding_id),
    );

    // An unknown definition is a concealed denial: it keeps the operation's
    // result contract but withholds the binding, and fails the process.
    let concealed = run_surface_tool_from(
        &home_path,
        &project_path,
        "tracedecay_workflow_get_definition",
        r#"{"definition_id":"workflow.absent","definition_version":1}"#,
    );
    assert!(
        !concealed.success,
        "a typed Workflow problem must fail the process\nstdout:\n{}",
        concealed.stdout
    );
    let (_, schema_id) = family_binding(workflow, "operation.workflow.get_definition");
    let concealed = assert_cli_family_envelope(
        &concealed,
        "tracedecay_workflow_get_definition",
        "problem",
        &schema_id,
        None,
    );
    assert_eq!(
        concealed["value"]["problem"]["kind"],
        "not_found_or_not_authorized"
    );
}
