//! A daemon that restarts without its authority record starts at authority
//! epoch 1 again and so reuses the previous generation's store incarnation.
//! The project sessions store then already holds a writer authority above the
//! epoch the new daemon allocates for it. The restarted daemon must converge
//! on that persisted authority and keep projecting; it must not retry a write
//! the store fences on every pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::common::{self, canonical_existing_path, tracedecay_command_with_home};

const THREAD_ID: &str = "codex-reopen-thread";
const FIRST_MARKER: &str = "ledgerquartz";
const RESTART_MARKER: &str = "restartquartz";
/// A projection pass runs in the background; this is how long one import may
/// take to become searchable on a loaded runner.
const PROJECTION_BUDGET: Duration = Duration::from_secs(120);

fn git(project: &Path, args: &[&str]) {
    let output = Command::new(common::git_program())
        .args(args)
        .current_dir(project)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rollout_line(timestamp: &str, kind: &str, payload: serde_json::Value) -> String {
    serde_json::json!({ "timestamp": timestamp, "type": kind, "payload": payload }).to_string()
}

fn write_rollout(home: &Path, project: &Path, messages: &[&str]) -> PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{THREAD_ID}.jsonl"));
    let mut lines = vec![rollout_line(
        "2026-01-01T00:00:00.000Z",
        "session_meta",
        serde_json::json!({
            "id": THREAD_ID,
            "cwd": project.to_string_lossy(),
            "model": "gpt-5.5"
        }),
    )];
    for (index, message) in messages.iter().enumerate() {
        lines.push(rollout_line(
            &format!("2026-01-01T00:00:{:02}.000Z", index + 1),
            "event_msg",
            serde_json::json!({ "type": "agent_message", "message": message }),
        ));
    }
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

fn import_sessions(home: &Path, project: &Path) {
    let output = tracedecay_command_with_home(home)
        .current_dir(project)
        .args(["sessions", "import", "--project-path"])
        .arg(project)
        .output()
        .expect("tracedecay sessions import should run");
    assert!(
        output.status.success(),
        "sessions import failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_until_searchable(home: &Path, project: &Path, marker: &str, context: &str) {
    common::poll_until(
        Instant::now() + PROJECTION_BUDGET,
        Duration::from_millis(500),
        || {
            let output = tracedecay_command_with_home(home)
                .current_dir(project)
                .args([
                    "sessions",
                    "search",
                    marker,
                    "--provider",
                    "codex",
                    "--project-path",
                ])
                .arg(project)
                .args(["--limit", "20", "--json"])
                .output()
                .expect("tracedecay sessions search should run");
            (output.status.success() && String::from_utf8_lossy(&output.stdout).contains(marker))
                .then_some(())
        },
        || format!("{context}: {marker} never became searchable"),
    );
}

#[test]
fn a_daemon_restarted_on_a_reused_incarnation_converges_on_the_persisted_writer_authority() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    fs::write(project_path.join("main.rs"), "fn main() {}\n").unwrap();
    git(&project_path, &["init", "--quiet", "--initial-branch=main"]);
    git(&project_path, &["add", "."]);
    git(
        &project_path,
        &[
            "-c",
            "user.email=reopen@tracedecay.invalid",
            "-c",
            "user.name=Reopen",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );

    let first_log = home_path.join("daemon-first.log");
    let mut first = common::spawn_tracedecay_daemon_logged(&home_path, &first_log, |_| {});
    let init = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .arg("init")
        .output()
        .expect("tracedecay init should run");
    assert!(
        init.status.success(),
        "tracedecay init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    write_rollout(&home_path, &project_path, &[FIRST_MARKER]);
    import_sessions(&home_path, &project_path);
    wait_until_searchable(&home_path, &project_path, FIRST_MARKER, "first daemon");
    first.kill_and_wait().expect("stop the first daemon");

    // The managed-daemon fixture and the work-loop journey both drop this
    // record between restarts, as does an operator clearing stale daemon
    // state. The next daemon then takes authority epoch 1 again and the
    // same store incarnation as the generation that just wrote the store.
    fs::remove_file(common::daemon_authority_path(
        &home_path.join(".tracedecay"),
    ))
    .expect("remove the first daemon's authority record");
    let second_log = home_path.join("daemon-second.log");
    let _second = common::spawn_tracedecay_daemon_logged(&home_path, &second_log, |_| {});
    write_rollout(&home_path, &project_path, &[FIRST_MARKER, RESTART_MARKER]);
    import_sessions(&home_path, &project_path);
    wait_until_searchable(
        &home_path,
        &project_path,
        RESTART_MARKER,
        "restarted daemon",
    );

    let second_stderr = fs::read_to_string(&second_log).expect("read the second daemon's log");
    assert!(
        !second_stderr.contains("stale writer authority epoch"),
        "the restarted daemon retried writes its store had fenced:\n{second_stderr}"
    );
}
