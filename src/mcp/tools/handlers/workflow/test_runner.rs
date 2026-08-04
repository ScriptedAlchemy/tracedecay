//! Bounded, cancellable process execution for managed affected-test runs.
//!
//! The test producer invokes a fixed Cargo command and retains only bounded
//! stdout/stderr. Cancellation, deadline expiry, and an overflowing stream all
//! terminate the complete child process tree before this runner resolves.

use std::fmt::{self, Display};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_millis(500);
const MAX_TEST_RUN_OUTPUT_BYTES: u64 = 128 * 1024;

#[derive(Debug)]
pub(super) struct TestRunOutput {
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) output_bytes: u64,
}

#[derive(Debug)]
pub(super) enum TestRunFailure {
    Spawn(String),
    Cancelled {
        output_bytes: u64,
    },
    Timeout {
        output_bytes: u64,
    },
    OutputLimit {
        stream: TestRunStream,
        output_bytes: u64,
    },
    Read {
        output_bytes: u64,
    },
    Harness {
        exit_code: Option<i32>,
        output_bytes: u64,
    },
    NoMatch {
        test_identity: String,
        output_bytes: u64,
    },
    InvalidIdentity {
        test_identity: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TestRunStream {
    Stdout,
    Stderr,
}

impl Display for TestRunStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => formatter.write_str("stdout"),
            Self::Stderr => formatter.write_str("stderr"),
        }
    }
}

/// Shared cancellation and output-limit state for one managed test process.
///
/// Registering exactly one process group means a caller can request
/// cancellation before or after the child starts without racing an orphaned
/// test binary. The runner always performs the matching reap before returning.
#[derive(Clone, Default)]
pub(super) struct TestRunControl {
    cancelled: Arc<AtomicBool>,
    output_limit: Arc<AtomicU8>,
    output_bytes: Arc<AtomicU64>,
    process_group: Arc<Mutex<Option<u32>>>,
}

impl TestRunControl {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(process_group) = self.active_process_group() {
            signal_process_tree(process_group, TerminationSignal::Terminate);
        }
    }

    fn register(&self, process_group: u32) {
        *self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(process_group);
        if self.cancelled.load(Ordering::Acquire) {
            signal_process_tree(process_group, TerminationSignal::Terminate);
        }
    }

    fn unregister(&self, process_group: u32) {
        let mut active = self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *active == Some(process_group) {
            *active = None;
        }
    }

    fn active_process_group(&self) -> Option<u32> {
        *self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn exceeded_output(&self) -> Option<TestRunStream> {
        match self.output_limit.load(Ordering::Acquire) {
            1 => Some(TestRunStream::Stdout),
            2 => Some(TestRunStream::Stderr),
            _ => None,
        }
    }

    fn mark_output_limit(&self, stream: TestRunStream) {
        let code = match stream {
            TestRunStream::Stdout => 1,
            TestRunStream::Stderr => 2,
        };
        if self
            .output_limit
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && let Some(process_group) = self.active_process_group()
        {
            signal_process_tree(process_group, TerminationSignal::Terminate);
        }
    }

    fn reserve_output(&self, stream: TestRunStream, bytes: usize) -> bool {
        let bytes = bytes as u64;
        loop {
            let consumed = self.output_bytes.load(Ordering::Acquire);
            let Some(next) = consumed.checked_add(bytes) else {
                self.mark_output_limit(stream);
                return false;
            };
            if next > MAX_TEST_RUN_OUTPUT_BYTES {
                self.mark_output_limit(stream);
                return false;
            }
            if self
                .output_bytes
                .compare_exchange(consumed, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn output_bytes(&self) -> u64 {
        self.output_bytes.load(Ordering::Acquire)
    }
}

pub(super) async fn run_cargo_tests(
    project_root: PathBuf,
    profile: String,
    test_names: Vec<String>,
    timeout: Duration,
    control: TestRunControl,
) -> Result<TestRunOutput, TestRunFailure> {
    tokio::task::spawn_blocking(move || {
        run_selected_cargo_tests(&project_root, &profile, &test_names, timeout, control)
    })
    .await
    .map_err(|_| TestRunFailure::Spawn("cargo test runner task ended unexpectedly".to_owned()))?
}

pub(super) fn cargo_test_args(profile: &str, test_identity: &str) -> Vec<String> {
    let mut args = vec!["test".to_string(), "--no-fail-fast".to_string()];
    if profile == "release" {
        args.push("--release".to_string());
    }
    args.extend([
        "--".to_string(),
        "--exact".to_string(),
        test_identity.to_owned(),
    ]);
    args
}

fn cargo_test_command(project_root: &Path, profile: &str, test_identity: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(project_root)
        .args(cargo_test_args(profile, test_identity));
    command
}

fn run_selected_cargo_tests(
    project_root: &Path,
    profile: &str,
    test_names: &[String],
    timeout: Duration,
    control: TestRunControl,
) -> Result<TestRunOutput, TestRunFailure> {
    if test_names.is_empty() {
        return Err(TestRunFailure::NoMatch {
            test_identity: "<none>".to_owned(),
            output_bytes: control.output_bytes(),
        });
    }
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return Err(TestRunFailure::Timeout {
            output_bytes: control.output_bytes(),
        });
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    for test_identity in test_names {
        validate_test_identity(test_identity)?;
        if control.cancelled.load(Ordering::Acquire) {
            return Err(TestRunFailure::Cancelled {
                output_bytes: control.output_bytes(),
            });
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(TestRunFailure::Timeout {
                output_bytes: control.output_bytes(),
            });
        };
        let mut command = cargo_test_command(project_root, profile, test_identity);
        let output = run_bounded_test_command(&mut command, remaining, control.clone())?;
        if output.exit_code != Some(0) {
            return Err(TestRunFailure::Harness {
                exit_code: output.exit_code,
                output_bytes: control.output_bytes(),
            });
        }
        if !parse_libtest_output(&output.stdout)
            .iter()
            .any(|(observed, _)| observed == test_identity)
        {
            return Err(TestRunFailure::NoMatch {
                test_identity: test_identity.clone(),
                output_bytes: control.output_bytes(),
            });
        }
        stdout.push_str(&output.stdout);
        stderr.push_str(&output.stderr);
    }
    Ok(TestRunOutput {
        exit_code: Some(0),
        stdout,
        stderr,
        output_bytes: control.output_bytes(),
    })
}

fn validate_test_identity(test_identity: &str) -> Result<(), TestRunFailure> {
    if test_identity.trim().is_empty()
        || test_identity.starts_with('-')
        || test_identity.contains('\0')
    {
        return Err(TestRunFailure::InvalidIdentity {
            test_identity: test_identity.to_owned(),
        });
    }
    Ok(())
}

fn configure_command(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
}

fn run_bounded_test_command(
    command: &mut Command,
    timeout: Duration,
    control: TestRunControl,
) -> Result<TestRunOutput, TestRunFailure> {
    if timeout.is_zero() {
        return Err(TestRunFailure::Timeout {
            output_bytes: control.output_bytes(),
        });
    }
    configure_command(command);
    let mut child = command
        .spawn()
        .map_err(|error| TestRunFailure::Spawn(error.to_string()))?;
    let process_group = child.id();
    control.register(process_group);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = stdout.map(|stdout| {
        let control = control.clone();
        thread::spawn(move || read_bounded(stdout, TestRunStream::Stdout, control))
    });
    let stderr_reader = stderr.map(|stderr| {
        let control = control.clone();
        thread::spawn(move || read_bounded(stderr, TestRunStream::Stderr, control))
    });

    let outcome = wait_for_process(
        &mut child,
        process_group,
        Instant::now() + timeout,
        &control,
    );
    control.unregister(process_group);
    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);
    let output_bytes = control.output_bytes();

    match outcome {
        ProcessOutcome::Completed(status) => {
            if let Some(stream) = control.exceeded_output() {
                return Err(TestRunFailure::OutputLimit {
                    stream,
                    output_bytes,
                });
            }
            let Some(stdout) = stdout.into_captured() else {
                return Err(TestRunFailure::Read { output_bytes });
            };
            let Some(stderr) = stderr.into_captured() else {
                return Err(TestRunFailure::Read { output_bytes });
            };
            Ok(TestRunOutput {
                exit_code: status.code(),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                output_bytes,
            })
        }
        ProcessOutcome::Cancelled => Err(TestRunFailure::Cancelled { output_bytes }),
        ProcessOutcome::TimedOut => Err(TestRunFailure::Timeout { output_bytes }),
        ProcessOutcome::OutputLimit(stream) => Err(TestRunFailure::OutputLimit {
            stream,
            output_bytes,
        }),
        ProcessOutcome::ReadFailure => Err(TestRunFailure::Read { output_bytes }),
    }
}

enum ProcessOutcome {
    Completed(ExitStatus),
    Cancelled,
    TimedOut,
    OutputLimit(TestRunStream),
    ReadFailure,
}

fn wait_for_process(
    child: &mut Child,
    process_group: u32,
    deadline: Instant,
    control: &TestRunControl,
) -> ProcessOutcome {
    loop {
        if let Some(stream) = control.exceeded_output() {
            terminate_and_reap(child, process_group);
            return ProcessOutcome::OutputLimit(stream);
        }
        if control.cancelled.load(Ordering::Acquire) {
            terminate_and_reap(child, process_group);
            return ProcessOutcome::Cancelled;
        }
        if Instant::now() >= deadline {
            terminate_and_reap(child, process_group);
            return ProcessOutcome::TimedOut;
        }
        match child.try_wait() {
            Ok(Some(status)) => return ProcessOutcome::Completed(status),
            Ok(None) => thread::sleep(PROCESS_POLL_INTERVAL),
            Err(_) => {
                terminate_and_reap(child, process_group);
                return ProcessOutcome::ReadFailure;
            }
        }
    }
}

fn terminate_and_reap(child: &mut Child, process_group: u32) {
    signal_process_tree(process_group, TerminationSignal::Terminate);
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
        return;
    }
    #[cfg(unix)]
    {
        let grace_deadline = Instant::now() + TERMINATION_GRACE;
        while Instant::now() < grace_deadline {
            let _ = child.try_wait();
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        signal_process_tree(process_group, TerminationSignal::Kill);
        let _ = child.kill();
        let _ = child.wait();
    }
}

enum StreamCapture {
    Captured(Vec<u8>),
    Exceeded,
    ReadFailure,
}

impl StreamCapture {
    fn into_captured(self) -> Option<Vec<u8>> {
        match self {
            Self::Captured(bytes) => Some(bytes),
            Self::Exceeded | Self::ReadFailure => None,
        }
    }
}

fn read_bounded(
    mut reader: impl Read,
    stream: TestRunStream,
    control: TestRunControl,
) -> StreamCapture {
    let mut output = Vec::with_capacity(8 * 1024);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(read) => read,
            Err(_) => return StreamCapture::ReadFailure,
        };
        if read == 0 {
            return StreamCapture::Captured(output);
        }
        if !control.reserve_output(stream, read) {
            return StreamCapture::Exceeded;
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn join_reader(reader: Option<thread::JoinHandle<StreamCapture>>) -> StreamCapture {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or(StreamCapture::ReadFailure)
}

#[derive(Clone, Copy)]
enum TerminationSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn signal_process_tree(process_group: u32, signal: TerminationSignal) {
    let signal = match signal {
        TerminationSignal::Terminate => libc::SIGTERM,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let _ = unsafe { libc::kill(-(process_group as i32), signal) };
}

#[cfg(windows)]
fn signal_process_tree(process_group: u32, _signal: TerminationSignal) {
    let _ = Command::new("taskkill")
        .args(["/PID", &process_group.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn signal_process_tree(_process_group: u32, _signal: TerminationSignal) {}

pub(super) fn parse_libtest_output(stdout: &str) -> Vec<(String, bool)> {
    let mut results = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim_start_matches("\u{1b}[0m").trim();
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        if rest.starts_with("result:") {
            continue;
        }
        let Some((name, status)) = rest.rsplit_once(" ... ") else {
            continue;
        };
        let passed = match status.trim() {
            "ok" => true,
            "FAILED" | "failed" => false,
            _ => continue,
        };
        results.push((name.trim().to_owned(), passed));
    }
    results
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        MAX_TEST_RUN_OUTPUT_BYTES, TestRunControl, TestRunFailure, TestRunStream,
        run_bounded_test_command, run_cargo_tests,
    };

    const FIXTURE_MODE: &str = "TRACEDECAY_TEST_RUNNER_FIXTURE_MODE";
    const FIXTURE_MARKER: &str = "TRACEDECAY_TEST_RUNNER_FIXTURE_MARKER";
    const LIMITED_OUTPUT_BYTES: usize = 96 * 1024;

    #[test]
    fn bounded_runner_fixture() {
        let Some(mode) = std::env::var_os(FIXTURE_MODE) else {
            return;
        };
        let marker = std::env::var_os(FIXTURE_MARKER).expect("fixture marker");
        let mode = mode.to_string_lossy();
        match mode.as_ref() {
            "parent_output" | "parent_idle" | "parent_limited_output" => {
                let child_mode = if mode == "parent_output" {
                    "child_output"
                } else if mode == "parent_limited_output" {
                    "child_limited_output"
                } else {
                    "child_idle"
                };
                let mut child = Command::new(std::env::current_exe().expect("fixture exe"))
                    .arg("mcp::tools::handlers::workflow::test_runner::tests::bounded_runner_fixture")
                    .arg("--exact")
                    .env(FIXTURE_MODE, child_mode)
                    .env(FIXTURE_MARKER, marker)
                    .spawn()
                    .expect("fixture child");
                let _ = child.wait();
            }
            "child_output" => {
                fs::write(&marker, std::process::id().to_string()).expect("fixture marker write");
                let mut stdout = std::io::stdout().lock();
                loop {
                    stdout.write_all(&[b'x'; 8 * 1024]).expect("fixture output");
                    stdout.flush().expect("fixture flush");
                }
            }
            "child_idle" => {
                fs::write(&marker, std::process::id().to_string()).expect("fixture marker write");
                thread::sleep(Duration::from_secs(60));
            }
            "child_limited_output" => {
                fs::write(&marker, std::process::id().to_string()).expect("fixture marker write");
                let mut stdout = std::io::stdout().lock();
                stdout
                    .write_all(&vec![b'x'; LIMITED_OUTPUT_BYTES])
                    .expect("fixture output");
                stdout.flush().expect("fixture flush");
            }
            _ => panic!("unknown fixture mode"),
        }
    }

    #[tokio::test]
    async fn cargo_runner_executes_every_requested_exact_test_once() {
        let temp = tempfile::TempDir::new().expect("temp");
        fs::create_dir_all(temp.path().join("src")).expect("source directory");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"bounded-runner-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        fs::write(
            temp.path().join("src/lib.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn selected_one() {}\n\n    #[test]\n    fn selected_two() {}\n\n    #[test]\n    fn excluded() { panic!(\"must stay filtered\"); }\n}\n",
        )
        .expect("source");

        let output = run_cargo_tests(
            temp.path().to_path_buf(),
            "debug".to_owned(),
            vec![
                "tests::selected_one".to_owned(),
                "tests::selected_two".to_owned(),
            ],
            Duration::from_secs(10),
            TestRunControl::default(),
        )
        .await
        .expect("selected cargo tests");

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.contains("test tests::selected_one ... ok"));
        assert!(output.stdout.contains("test tests::selected_two ... ok"));
        assert!(!output.stdout.contains("tests::excluded"));
    }

    #[tokio::test]
    async fn cargo_runner_rejects_a_vacuous_exact_test_run() {
        let temp = tempfile::TempDir::new().expect("temp");
        fs::create_dir_all(temp.path().join("src")).expect("source directory");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"vacuous-runner-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        fs::write(
            temp.path().join("src/lib.rs"),
            "#[cfg(test)]\nmod tests { #[test] fn present() {} }\n",
        )
        .expect("source");

        let result = run_cargo_tests(
            temp.path().to_path_buf(),
            "debug".to_owned(),
            vec!["tests::missing".to_owned()],
            Duration::from_secs(10),
            TestRunControl::default(),
        )
        .await;

        assert!(matches!(
            result,
            Err(TestRunFailure::NoMatch { test_identity, .. }) if test_identity == "tests::missing"
        ));
    }

    #[tokio::test]
    async fn cargo_runner_rejects_option_like_test_identity_before_spawning() {
        let result = run_cargo_tests(
            PathBuf::from("/no/such/project"),
            "debug".to_owned(),
            vec!["--nocapture".to_owned()],
            Duration::from_secs(10),
            TestRunControl::default(),
        )
        .await;

        assert!(matches!(
            result,
            Err(TestRunFailure::InvalidIdentity { test_identity }) if test_identity == "--nocapture"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn output_bound_terminates_and_reaps_the_complete_test_process_tree() {
        let temp = tempfile::TempDir::new().expect("temp");
        let marker = temp.path().join("child.pid");
        let mut command = fixture_command(&marker, "parent_output");
        let result = run_bounded_test_command(
            &mut command,
            Duration::from_secs(5),
            TestRunControl::default(),
        );
        let Err(TestRunFailure::OutputLimit {
            stream: TestRunStream::Stdout,
            output_bytes,
        }) = result
        else {
            panic!("test process must terminate when stdout exceeds its bound");
        };
        assert!(output_bytes <= MAX_TEST_RUN_OUTPUT_BYTES);
        assert!(output_bytes >= MAX_TEST_RUN_OUTPUT_BYTES - 8 * 1024);
        assert_reaped(&marker);
    }

    #[cfg(unix)]
    #[test]
    fn output_budget_is_shared_across_selected_test_commands() {
        let temp = tempfile::TempDir::new().expect("temp");
        let marker = temp.path().join("child.pid");
        let control = TestRunControl::default();
        let mut first = fixture_command(&marker, "parent_limited_output");
        let first = run_bounded_test_command(&mut first, Duration::from_secs(5), control.clone())
            .expect("first command stays inside the shared output budget");
        assert!(first.output_bytes >= LIMITED_OUTPUT_BYTES as u64);

        let mut second = fixture_command(&marker, "parent_limited_output");
        let second = run_bounded_test_command(&mut second, Duration::from_secs(5), control);
        assert!(matches!(second, Err(TestRunFailure::OutputLimit { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_and_reaps_the_complete_test_process_tree() {
        let temp = tempfile::TempDir::new().expect("temp");
        let marker = temp.path().join("child.pid");
        let mut command = fixture_command(&marker, "parent_idle");
        let control = TestRunControl::default();
        let canceller = {
            let control = control.clone();
            let marker = marker.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(1);
                while !marker.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(5));
                }
                control.cancel();
            })
        };
        let result = run_bounded_test_command(&mut command, Duration::from_secs(5), control);
        canceller.join().expect("canceller");
        assert!(matches!(result, Err(TestRunFailure::Cancelled { .. })));
        assert_reaped(&marker);
    }

    #[cfg(unix)]
    #[test]
    fn deadline_terminates_and_reaps_the_complete_test_process_tree() {
        let temp = tempfile::TempDir::new().expect("temp");
        let marker = temp.path().join("child.pid");
        let mut command = fixture_command(&marker, "parent_idle");
        let result = run_bounded_test_command(
            &mut command,
            Duration::from_millis(50),
            TestRunControl::default(),
        );
        assert!(matches!(result, Err(TestRunFailure::Timeout { .. })));
        assert_reaped(&marker);
    }

    #[cfg(unix)]
    fn fixture_command(marker: &std::path::Path, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("fixture exe"));
        command
            .arg("mcp::tools::handlers::workflow::test_runner::tests::bounded_runner_fixture")
            .arg("--exact")
            .env(FIXTURE_MODE, mode)
            .env(FIXTURE_MARKER, marker);
        command
    }

    #[cfg(unix)]
    fn assert_reaped(marker: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(marker)
            .expect("fixture child marker")
            .trim()
            .parse::<i32>()
            .expect("fixture child pid");
        while process_is_live(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_live(pid),
            "fixture descendant {pid} survived test-process cleanup"
        );
    }

    #[cfg(unix)]
    fn process_is_live(pid: i32) -> bool {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
        if let Ok(stat) = stat
            && stat.split_whitespace().nth(2) == Some("Z")
        {
            return false;
        }
        unsafe { libc::kill(pid, 0) == 0 }
    }
}
