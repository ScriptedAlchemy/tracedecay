use std::process::Command;
use std::time::{Duration, Instant};

use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::{
    GitCommandBounds, GitCommandError, bounded_command_output, bounded_git_output,
};

#[test]
fn read_only_git_output_enforces_deadline_and_byte_limit() {
    let root = tempfile::tempdir().expect("temporary repository root");
    let expired = GitCommandBounds {
        deadline: Instant::now(),
        ..GitCommandBounds::default()
    };
    assert!(matches!(
        bounded_git_output(root.path(), &["--version"], &expired),
        Err(GitCommandError::DeadlineExceeded)
    ));

    let limited = GitCommandBounds {
        max_stdout_bytes: 1,
        ..GitCommandBounds::default()
    };
    assert!(matches!(
        bounded_git_output(root.path(), &["--version"], &limited),
        Err(GitCommandError::OutputLimitExceeded {
            stream: "stdout",
            bound: 1
        })
    ));
}

#[cfg(unix)]
#[test]
fn generic_command_is_interrupted_by_live_cancellation() {
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let notifier = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        trigger.cancel();
    });
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 30"]);
    let bounds = GitCommandBounds {
        cancel: Some(cancellation),
        ..GitCommandBounds::default()
    };
    let started = Instant::now();
    let result = bounded_command_output(command, None, &bounds);
    notifier.join().expect("cancellation notifier");

    assert!(matches!(result, Err(GitCommandError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(1));
}
