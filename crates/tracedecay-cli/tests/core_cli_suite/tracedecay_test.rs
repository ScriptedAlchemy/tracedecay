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
fn test_is_test_file_test_dir() {
    assert!(tracedecay::tracedecay::is_test_file("tests/my_test.rs"));
    assert!(tracedecay::tracedecay::is_test_file("tests/integration.rs"));
}

#[test]
fn test_is_test_file_test_prefix() {
    assert!(tracedecay::tracedecay::is_test_file("test/foo.rs"));
}

#[test]
fn test_is_test_file_spec_dir() {
    assert!(tracedecay::tracedecay::is_test_file(
        "spec/models/user_spec.rb"
    ));
}

#[test]
fn test_is_test_file_e2e_dir() {
    assert!(tracedecay::tracedecay::is_test_file("e2e/login.test.ts"));
}

#[test]
fn test_is_test_file_dot_test() {
    assert!(tracedecay::tracedecay::is_test_file("src/utils.test.ts"));
    assert!(tracedecay::tracedecay::is_test_file("src/utils.spec.js"));
}

#[test]
fn test_is_test_file_underscore_test() {
    assert!(tracedecay::tracedecay::is_test_file("src/utils_test.rs"));
    assert!(tracedecay::tracedecay::is_test_file("src/utils_spec.py"));
}

#[test]
fn test_is_test_file_dunder_tests() {
    assert!(tracedecay::tracedecay::is_test_file(
        "__tests__/component.test.tsx"
    ));
}

#[test]
fn test_is_test_file_normal_source() {
    assert!(!tracedecay::tracedecay::is_test_file("src/lib.rs"));
    assert!(!tracedecay::tracedecay::is_test_file("src/main.rs"));
    assert!(!tracedecay::tracedecay::is_test_file("src/utils.rs"));
}

#[test]
fn test_is_test_file_case_insensitive() {
    assert!(tracedecay::tracedecay::is_test_file("Tests/MyTest.rs"));
    assert!(tracedecay::tracedecay::is_test_file("TESTS/foo.rs"));
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

#[test]
fn daemon_tool_str_replace_updates_source() {
    let (_home, _project, home_path, project_path) =
        setup_daemon_project("pub fn answer() -> u32 { 1 }\n");
    let project_arg = project_path.to_string_lossy().to_string();
    let output = run_tool(
        &project_path,
        &home_path,
        &[
            "--project",
            &project_arg,
            "str_replace",
            "--json",
            "--args",
            r#"{"path":"src/lib.rs","old_str":"pub fn answer() -> u32 { 1 }","new_str":"pub fn answer() -> u32 { 2 }"}"#,
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

#[test]
fn daemon_tool_str_replace_dry_run_preserves_source() {
    let original = "pub fn answer() -> u32 { 1 }\n";
    let (_home, _project, home_path, project_path) = setup_daemon_project(original);
    let project_arg = project_path.to_string_lossy().to_string();
    let output = run_tool(
        &project_path,
        &home_path,
        &[
            "--project",
            &project_arg,
            "str_replace",
            "--json",
            "--args",
            r#"{"path":"src/lib.rs","old_str":"pub fn answer() -> u32 { 1 }","new_str":"pub fn answer() -> u32 { 2 }","dry_run":true}"#,
        ],
    );

    assert!(
        output.status.success(),
        "daemon-owned source-edit dry run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(project_path.join("src/lib.rs")).unwrap(),
        original
    );
}
