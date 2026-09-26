//! CLI journeys for the product surfaces that replaced the retired direct
//! `TraceDecay` graph/index API.

use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::common::{self, canonical_existing_path, tracedecay_command_with_home};

fn init_daemon_project(project: &Path, home: &Path, source: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), source).unwrap();
    let git = Command::new(common::git_program())
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(project)
        .output()
        .expect("initialize Git worktree");
    assert!(
        git.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git.stderr)
    );

    crate::common::initialize_tracedecay_cli_project(home, project);
}

fn run_tool(project: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    tracedecay_command_with_home(home)
        .current_dir(project)
        .arg("tool")
        .args(args)
        .output()
        .expect("tracedecay tool should run")
}

fn setup_daemon_project(
    source: &str,
) -> (TempDir, TempDir, std::path::PathBuf, std::path::PathBuf) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    common::ensure_tracedecay_daemon(&home_path);
    init_daemon_project(&project_path, &home_path, source);
    (home, project, home_path, project_path)
}

#[test]
fn daemon_tool_searches_the_active_project() {
    let (_home, _project, home_path, project_path) =
        setup_daemon_project("pub fn findable_symbol() {}\n");
    let project_arg = project_path.to_string_lossy().to_string();
    let output = common::poll_until(
        Instant::now() + Duration::from_secs(30),
        Duration::from_millis(100),
        || {
            let output = run_tool(
                &project_path,
                &home_path,
                &[
                    "--project",
                    &project_arg,
                    "search",
                    "--json",
                    "--args",
                    r#"{"query":"findable_symbol","limit":10}"#,
                ],
            );
            (output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("findable_symbol"))
            .then_some(output)
        },
        || "daemon scheduler did not publish findable_symbol for search".to_owned(),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("findable_symbol"),
        "daemon-owned search must return the indexed symbol"
    );
}

/// `status --json .` names the CLI's working directory as the diagnostic
/// target. The daemon runs from its own directory, so a `.` forwarded verbatim
/// resolved to whatever that was (`/` under launchd) and reported the wrong
/// project, or no project, instead of the one the operator stood in.
#[test]
fn status_anchors_an_explicit_dot_to_the_cli_working_directory() {
    let (_home, _project, home_path, project_path) =
        setup_daemon_project("pub fn status_marker() {}\n");

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .args(["status", "--json", "."])
        .output()
        .expect("tracedecay status should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "status --json . must report the project the CLI ran in\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_str(&stdout).expect("status JSON");
    let reported = status["project_root"]
        .as_str()
        .expect("status names its project_root");
    assert_eq!(
        canonical_existing_path(Path::new(reported)),
        project_path,
        "status must select the CLI's project, never the daemon's working directory"
    );
}

/// `--project-path .` names the registered project the operator stands in,
/// exactly as its absolute path does; the registry never sees the bare `.`.
#[test]
fn project_path_flags_resolve_a_relative_path_from_inside_the_project() {
    let (_home, _project, home_path, project_path) =
        setup_daemon_project("pub fn imported_marker() {}\n");

    let import = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .args(["sessions", "import", "--project-path", "."])
        .output()
        .expect("tracedecay sessions import should run");
    let stdout = String::from_utf8_lossy(&import.stdout);
    assert!(
        import.status.success()
            && stdout.starts_with("session import completed (")
            && stdout.ends_with(")\n"),
        "sessions import --project-path . must import into the CLI's project\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&import.stderr)
    );

    let status = tracedecay_command_with_home(&home_path)
        .current_dir(project_path.join("src"))
        .args(["status", "--json", "--project-path", ".."])
        .output()
        .expect("tracedecay status should run");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success(),
        "status --project-path .. must report the CLI's project\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value = serde_json::from_str(&stdout).expect("status JSON");
    assert_eq!(
        status["project_root"],
        project_path.to_string_lossy().as_ref()
    );
}

#[test]
fn daemon_tool_search_discloses_configured_alias_recovery() {
    let (_home, _project, home_path, project_path) = setup_daemon_project("pub fn cache() {}\n");
    let project_arg = project_path.to_string_lossy().to_string();
    let output = common::poll_until(
        Instant::now() + Duration::from_secs(30),
        Duration::from_millis(100),
        || {
            let output = run_tool(
                &project_path,
                &home_path,
                &[
                    "--project",
                    &project_arg,
                    "search",
                    "--json",
                    "--args",
                    r#"{"query":"memoization","lexical_aliases":[{"strict_query":"memoization","alternative":"cache"}],"limit":10}"#,
                ],
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            (output.status.success()
                && stdout.contains("Strict query: `memoization`")
                && stdout.contains("Alternative tried: `cache`")
                && stdout.contains("Reason: configured vocabulary alias"))
            .then_some(output)
        },
        || "daemon scheduler did not expose alias recovery for cache".to_owned(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cache"), "{stdout}");
}

/// A daemon-owned source edit is a preview-then-apply effect: an apply must
/// carry a fresh `idempotency_key` and the `expected_state` its own preview
/// returned, so the write is compare-and-set against the exact bytes the
/// preview was computed from. Drive that journey end to end from the CLI.
#[test]
fn daemon_tool_str_replace_updates_source() {
    let (_home, _project, home_path, project_path) =
        setup_daemon_project("pub fn answer() -> u32 { 1 }\n");
    let project_arg = project_path.to_string_lossy().to_string();
    let preview = run_tool(
        &project_path,
        &home_path,
        &[
            "--project",
            &project_arg,
            "str_replace",
            "--json",
            "--args",
            r#"{"path":"src/lib.rs","old_str":"pub fn answer() -> u32 { 1 }","new_str":"pub fn answer() -> u32 { 2 }","dry_run":true,"format":"json"}"#,
        ],
    );
    assert!(
        preview.status.success(),
        "daemon-owned source edit preview failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&preview.stdout),
        String::from_utf8_lossy(&preview.stderr),
    );
    assert_eq!(
        fs::read_to_string(project_path.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> u32 { 1 }\n",
        "a preview must not write"
    );
    let expected_state = source_edit_expected_state(&preview.stdout);

    let apply_args = serde_json::json!({
        "path": "src/lib.rs",
        "old_str": "pub fn answer() -> u32 { 1 }",
        "new_str": "pub fn answer() -> u32 { 2 }",
        "idempotency_key": "core-cli-suite.source-edit.str-replace",
        "expected_state": expected_state,
    })
    .to_string();
    let output = run_tool(
        &project_path,
        &home_path,
        &[
            "--project",
            &project_arg,
            "str_replace",
            "--json",
            "--args",
            apply_args.as_str(),
        ],
    );

    assert!(
        output.status.success(),
        "daemon-owned source edit failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(project_path.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> u32 { 2 }\n"
    );
}

/// Reads `expected_state` out of a `format: "json"` source-edit preview.
///
/// `tracedecay tool --json` prints the MCP tool-result envelope; the requested
/// JSON document travels inside the first content block's text.
fn source_edit_expected_state(stdout: &[u8]) -> String {
    let envelope: serde_json::Value =
        serde_json::from_slice(stdout).expect("source edit preview should print JSON");
    let text = envelope["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("preview returned no content text: {envelope}"));
    let document: serde_json::Value =
        serde_json::from_str(text).expect("preview content should carry the JSON document");
    document["expected_state"]
        .as_str()
        .unwrap_or_else(|| panic!("preview omitted expected_state: {document}"))
        .to_owned()
}
