//! Raw worktree writes and renames reach daemon-routed CLI reads without a
//! hook hint, a Git metadata move, or an explicit `sync`.

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::code_index_journey::{
    commit_all, exact_identity, git, initialize_tracedecay, stop_daemon_gracefully,
    wait_for_terminal_generation,
};
use crate::common::{
    IsolatedHome, daemon_socket_path, spawn_tracedecay_daemon_logged, tracedecay_command_with_home,
};

/// The read-refresh cooldown plus one reconcile of a two-file fixture.
const OBSERVATION_BUDGET: Duration = Duration::from_secs(90);

/// `tracedecay tool find_exact_symbol` as a user runs it in the project.
fn cli_exact_files(home: &Path, project: &Path, name: &str) -> Vec<String> {
    let output = tracedecay_command_with_home(home)
        .args(["tool", "find_exact_symbol", "--json", "--args"])
        .arg(json!({ "name": name }).to_string())
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay tool find_exact_symbol");
    assert!(
        output.status.success(),
        "find_exact_symbol failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let found: Value = serde_json::from_slice(&output.stdout).expect("find_exact_symbol JSON");
    found["structuredContent"]["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| symbol["file"].as_str().map(str::to_owned))
        .collect()
}

/// Polls the CLI read until it names `expected`, and returns the last answer
/// either way.
async fn cli_exact_files_until(
    home: &Path,
    project: &Path,
    name: &str,
    expected: &[&str],
) -> Vec<String> {
    let deadline = Instant::now() + OBSERVATION_BUDGET;
    loop {
        let files = cli_exact_files(home, project, name);
        if files == expected || Instant::now() >= deadline {
            return files;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn cli_reads_observe_unhinted_renames_and_writes() {
    let (environment, project) = IsolatedHome::new();
    let project = project.canonicalize().expect("canonical fixture project");
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"unhinted\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
    fs::write(
        project.join("src/calc.rs"),
        "pub fn unhinted_sum_pair(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .expect("fixture source");
    git(&project, &["init", "--quiet", "--initial-branch=main"]);
    let revision = commit_all(&project, "fixture");
    let socket = daemon_socket_path(environment.home());
    let log_path = environment.scratch().join("unhinted-change-daemon.log");
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    let identity = exact_identity(&project, project_id);
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let handshake = tracedecay::daemon::handshake_for_current_client(
        environment.profile(),
        Some(project.clone()),
        None,
        false,
        false,
    )
    .expect("production daemon handshake");
    wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "unhinted_sum_pair",
        Some("src/calc.rs"),
    )
    .await;
    let home = environment.home();
    assert_eq!(
        cli_exact_files(home, &project, "unhinted_sum_pair"),
        ["src/calc.rs"]
    );

    fs::rename(project.join("src/calc.rs"), project.join("src/arith.rs"))
        .expect("rename tracked source without a hook hint");
    assert_eq!(
        cli_exact_files_until(home, &project, "unhinted_sum_pair", &["src/arith.rs"]).await,
        ["src/arith.rs"],
        "a CLI read must stop naming the renamed-away path; daemon_log={}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );

    fs::write(
        project.join("src/arith.rs"),
        "pub fn unhinted_sum_pair(a: i32, b: i32) -> i32 { a + b }\n\
         pub fn unhinted_written_later() -> i32 { 3 }\n",
    )
    .expect("write tracked source without a hook hint");
    assert_eq!(
        cli_exact_files_until(home, &project, "unhinted_written_later", &["src/arith.rs"]).await,
        ["src/arith.rs"],
        "a CLI read must observe an unhinted write; daemon_log={}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );

    stop_daemon_gracefully(&mut daemon);
}
