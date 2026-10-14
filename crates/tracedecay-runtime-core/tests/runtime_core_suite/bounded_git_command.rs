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

/// A successful parent exit does not close a pipe a descendant inherited, so
/// draining after exit must stay under the caller's deadline instead of
/// waiting for an EOF the descendant controls.
#[cfg(unix)]
#[test]
fn draining_after_exit_is_bounded_when_a_descendant_holds_stdout() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'tracedecay 1.2.3\\n'; sleep 20 & exit 0"]);
    let bounds = GitCommandBounds {
        deadline: Instant::now() + Duration::from_millis(300),
        ..GitCommandBounds::default()
    };
    let started = Instant::now();

    let result = bounded_command_output(command, None, &bounds);

    assert!(matches!(result, Err(GitCommandError::DeadlineExceeded)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the inherited pipe must not hold the caller past its deadline"
    );
}

#[cfg(unix)]
#[test]
fn stdin_drain_after_exit_is_bounded_when_a_descendant_holds_input() {
    let mut command = Command::new("sh");
    // POSIX shells point a background job's stdin at /dev/null before its own
    // redirections run, so the pipe is handed over through fd 3.
    command.args(["-c", "exec 3<&0; sleep 2 <&3 3<&- >/dev/null 2>&1 & exit 0"]);
    let bounds = GitCommandBounds {
        deadline: Instant::now() + Duration::from_millis(300),
        ..GitCommandBounds::default()
    };
    let started = Instant::now();
    let result = bounded_command_output(command, Some(&vec![0xa5; 4 * 1024 * 1024]), &bounds);
    assert!(matches!(result, Err(GitCommandError::DeadlineExceeded)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// Returning must close our end of every pipe, so a descendant still writing
/// to the inherited stdout hits a broken pipe instead of a reader that drains
/// it for as long as the descendant lives.
#[cfg(unix)]
#[test]
fn returning_breaks_the_stdout_pipe_for_a_descendant_still_writing() {
    let root = tempfile::tempdir().expect("temporary marker directory");
    let stopped = root.path().join("stopped");
    let mut command = Command::new("sh");
    command.env("STOPPED", &stopped).args([
        "-c",
        "(trap '' PIPE; while :; do echo x || { : > \"$STOPPED\"; exit 0; }; sleep 0.05; done) & exit 0",
    ]);
    let bounds = GitCommandBounds {
        deadline: Instant::now() + Duration::from_millis(300),
        ..GitCommandBounds::default()
    };

    let result = bounded_command_output(command, None, &bounds);

    assert!(matches!(result, Err(GitCommandError::DeadlineExceeded)));
    let give_up = Instant::now() + Duration::from_secs(5);
    while !stopped.exists() {
        assert!(
            Instant::now() < give_up,
            "the descendant kept writing into a stdout pipe we still hold open"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Returning must also close our stdin end, so a descendant that reads the
/// inherited stdin later sees EOF after what was already buffered instead of
/// the rest of the input.
#[cfg(unix)]
#[test]
fn returning_closes_the_stdin_pipe_a_descendant_reads_later() {
    let root = tempfile::tempdir().expect("temporary marker directory");
    let gate = root.path().join("gate");
    let count = root.path().join("count");
    let mut command = Command::new("sh");
    command.env("GATE", &gate).env("COUNT", &count).args([
        "-c",
        "exec 3<&0; (while [ ! -e \"$GATE\" ]; do sleep 0.01; done; wc -c <&3 3<&- > \"$COUNT.tmp\"; mv \"$COUNT.tmp\" \"$COUNT\") >/dev/null 2>&1 & exit 0",
    ]);
    let bounds = GitCommandBounds {
        deadline: Instant::now() + Duration::from_millis(300),
        ..GitCommandBounds::default()
    };
    let input = vec![0xa5; 4 * 1024 * 1024];

    let result = bounded_command_output(command, Some(&input), &bounds);

    assert!(matches!(result, Err(GitCommandError::DeadlineExceeded)));
    std::fs::write(&gate, b"").expect("open the descendant's gate");
    let give_up = Instant::now() + Duration::from_secs(5);
    while !count.exists() {
        assert!(Instant::now() < give_up, "the descendant never reached EOF");
        std::thread::sleep(Duration::from_millis(10));
    }
    let read: usize = std::fs::read_to_string(&count)
        .expect("descendant byte count")
        .trim()
        .parse()
        .expect("numeric byte count");
    assert!(
        read < input.len(),
        "the descendant read all {read} input bytes through a stdin pipe we still hold open"
    );
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

#[cfg(unix)]
#[test]
fn output_limit_stops_a_child_that_is_still_running() {
    let mut command = Command::new("sh");
    command.args(["-c", "head -c 1048576 /dev/zero; exec sleep 30"]);
    let bounds = GitCommandBounds {
        deadline: Instant::now() + Duration::from_secs(5),
        max_stdout_bytes: 1024,
        ..GitCommandBounds::default()
    };
    let started = Instant::now();

    let result = bounded_command_output(command, None, &bounds);

    assert!(
        matches!(
            result,
            Err(GitCommandError::OutputLimitExceeded {
                stream: "stdout",
                bound: 1024
            })
        ),
        "got {result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}
