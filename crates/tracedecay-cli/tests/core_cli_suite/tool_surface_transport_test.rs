//! Transport parity for `tracedecay tool <application surface>`.
//!
//! Every application-surface operation requires a project route on the daemon
//! side (`DaemonInvocationRequest::requires_project`). The compatibility tool
//! path resolves that route by walking up from the working directory, so
//! `tracedecay tool circular` works from a checkout without `--project`. The
//! typed application-surface path must present the same authenticated route:
//! otherwise `storage_status`, `source_outline`, `file_metadata`, and the git
//! reads answer `application.surface.unavailable` /
//! `not_found_or_not_authorized` from a checkout the operator is standing in.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

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
    assert_surface_resolves_project(
        &home_path,
        &project_path,
        "file_metadata",
        r#"{"files":["src/lib.rs"],"format":"json"}"#,
    );
}

#[test]
fn application_surface_git_reads_resolve_the_working_directory_worktree() {
    let (_home, _project, home_path, project_path) = surface_fixture();
    let _daemon = spawn_tracedecay_daemon(&home_path);

    for tool in ["git_status", "git_diff", "git_history"] {
        assert_surface_resolves_project(&home_path, &project_path, tool, r#"{"format":"json"}"#);
    }
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
