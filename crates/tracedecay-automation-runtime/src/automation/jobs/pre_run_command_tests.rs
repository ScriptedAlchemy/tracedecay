use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracedecay_runtime_core::native_runtime;

use super::{JOB_COMMAND_OUTPUT_CAP_CHARS, PreRunCommandError, run_pre_run_command};
use crate::automation::backend::AgentTaskFailureClass;

/// Runs the command with no Tokio runtime anywhere on the stack.
fn run(command: &str, timeout: Duration) -> Result<String, PreRunCommandError> {
    native_runtime::block_on(run_pre_run_command(command, None, timeout))
        .expect("native runtime available")
}

fn record_pid_then_sleep(pid_file: &Path) -> String {
    format!("echo $$ > '{}'; exec sleep 30", pid_file.display())
}

fn recorded_pid(pid_file: &Path) -> Option<String> {
    let pid = std::fs::read_to_string(pid_file).ok()?;
    pid.ends_with('\n').then(|| pid.trim().to_owned())
}

fn process_exists(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .expect("kill runs")
        .success()
}

#[test]
fn a_command_past_its_timeout_is_killed_and_reported_timed_out() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pid");
    let started = Instant::now();

    let result = run(&record_pid_then_sleep(&pid_file), Duration::from_secs(2));

    assert!(
        matches!(result, Err(PreRunCommandError::TimedOut)),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(25));
    let pid = recorded_pid(&pid_file).expect("command recorded its pid before the timeout");
    assert!(
        !process_exists(&pid),
        "a timed-out command is killed and reaped before the error returns"
    );
    let error = result.unwrap_err();
    assert_eq!(error.failure_class(), AgentTaskFailureClass::Timeout);
    assert_eq!(error.to_string(), "command timed out after 30s");
}

#[test]
fn a_command_flooding_both_pipes_is_bounded() {
    let flood = "head -c 1048576 /dev/zero | tr '\\0' e >&2; \
                 head -c 1048576 /dev/zero | tr '\\0' o";

    let result = run(flood, Duration::from_secs(60));
    assert!(
        matches!(result, Err(PreRunCommandError::OutputTooLarge)),
        "{result:?}"
    );
}

#[test]
fn a_nonzero_exit_reports_its_status_and_trimmed_stderr() {
    let result = run("echo denied >&2; exit 7", Duration::from_secs(60));

    let Err(PreRunCommandError::Exited { status, stderr }) = &result else {
        panic!("non-zero exit must surface as Exited: {result:?}");
    };
    assert_eq!(status.code(), Some(7));
    assert_eq!(stderr, "denied");
    assert_eq!(
        result.unwrap_err().failure_class(),
        AgentTaskFailureClass::Permanent
    );
}

#[test]
fn dropping_a_running_command_kills_it() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pid");
    let command = record_pid_then_sleep(&pid_file);

    let pid = native_runtime::block_on(async {
        let started = run_pre_run_command(&command, None, Duration::from_secs(120));
        let recorded = async {
            loop {
                if let Some(pid) = recorded_pid(&pid_file) {
                    return pid;
                }
                native_runtime::sleep_until(Instant::now() + Duration::from_millis(10))
                    .await
                    .expect("native timer");
            }
        };
        tokio::select! {
            result = started => panic!("command ended before it was dropped: {result:?}"),
            pid = recorded => pid,
        }
    })
    .expect("native runtime available");

    let deadline = Instant::now() + Duration::from_secs(30);
    while process_exists(&pid) {
        assert!(
            Instant::now() < deadline,
            "a dropped command must be killed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn bounded_capture_still_truncates_prompt_characters() {
    let output = run(
        "head -c 32768 /dev/zero | tr '\\0' x",
        Duration::from_secs(10),
    )
    .expect("bounded command");
    assert_eq!(output.len(), JOB_COMMAND_OUTPUT_CAP_CHARS);
}
