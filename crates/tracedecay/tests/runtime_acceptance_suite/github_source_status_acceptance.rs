use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common;

fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new(common::git_program())
        .current_dir(project)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn committed_checkout(project: &Path, origin: Option<&str>) {
    std::fs::create_dir_all(project.join("src")).expect("create fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn alpha() -> u32 { 1 }\n")
        .expect("write fixture source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    git(project, &["config", "user.name", "TraceDecay Test"]);
    git(project, &["config", "user.email", "tracedecay@example.com"]);
    if let Some(origin) = origin {
        git(project, &["remote", "add", "origin", origin]);
    }
    git(project, &["add", "."]);
    git(project, &["commit", "--quiet", "-m", "base"]);
}

fn init(home: &Path, project: &Path) {
    let output = common::tracedecay_command_with_home(home)
        .args(["init", "--wait"])
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
}

fn github_source(home: &Path, project: &Path) -> Value {
    let project_arg = project.to_string_lossy().into_owned();
    let output = common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "status",
            "--args",
            r#"{"format":"json"}"#,
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run status");
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    status["github_source"].clone()
}

/// The project-open owners record the GitHub source after the project is
/// first served, so status may still answer `unresolved` right after init.
fn settled_github_source(home: &Path, project: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let source = github_source(home, project);
        if source["state"] != "unresolved" || Instant::now() >= deadline {
            return source;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn status_names_why_a_checkout_has_no_observed_github_source() {
    let (environment, detached) = common::IsolatedHome::new();
    let without_origin = detached
        .parent()
        .expect("isolated project has a parent")
        .join("without-origin");
    committed_checkout(
        &detached,
        Some("https://github.com/ScriptedAlchemy/tracedecay.git"),
    );
    git(&detached, &["checkout", "--quiet", "--detach"]);
    committed_checkout(&without_origin, None);
    let _daemon = common::spawn_tracedecay_daemon_with(environment.home(), |command| {
        environment.apply_toolchain_env(command);
    });
    init(environment.home(), &detached);
    init(environment.home(), &without_origin);

    assert_eq!(
        settled_github_source(environment.home(), &detached),
        json!({
            "state": "detached_head",
            "reason": "origin is a GitHub repository, but HEAD is detached: pull-request discovery runs only on an attached branch; check out a branch, then restart the daemon (`tracedecay daemon restart`) so it reopens this checkout",
        })
    );
    assert_eq!(
        settled_github_source(environment.home(), &without_origin),
        json!({
            "state": "no_github_origin",
            "reason": "the checkout's `origin` remote is missing or does not name a GitHub repository",
        })
    );
}
