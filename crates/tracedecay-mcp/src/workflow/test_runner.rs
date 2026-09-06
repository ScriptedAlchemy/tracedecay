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

use super::TestProfile;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_millis(500);
const MAX_TEST_RUN_OUTPUT_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug)]
pub struct TestRunOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output_bytes: u64,
}

#[derive(Debug)]
pub enum TestRunFailure {
    Spawn(String),
    Cancelled {
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    Timeout {
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    OutputLimit {
        stream: TestRunStream,
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    Read {
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    Harness {
        exit_code: Option<i32>,
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    NoMatch {
        test_identity: String,
        output_bytes: u64,
        partial: Option<TestRunOutput>,
    },
    InvalidIdentity {
        test_identity: String,
    },
}

impl TestRunFailure {
    fn with_partial_output(
        self,
        mut stdout: String,
        mut stderr: String,
        output_bytes: u64,
    ) -> Self {
        let combine = |partial: Option<TestRunOutput>| {
            let partial = partial.unwrap_or(TestRunOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                output_bytes,
            });
            stdout.push_str(&partial.stdout);
            stderr.push_str(&partial.stderr);
            TestRunOutput {
                exit_code: partial.exit_code,
                stdout,
                stderr,
                output_bytes,
            }
        };
        match self {
            Self::Cancelled { partial, .. } => Self::Cancelled {
                output_bytes,
                partial: Some(combine(partial)),
            },
            Self::Timeout { partial, .. } => Self::Timeout {
                output_bytes,
                partial: Some(combine(partial)),
            },
            Self::OutputLimit {
                stream, partial, ..
            } => Self::OutputLimit {
                stream,
                output_bytes,
                partial: Some(combine(partial)),
            },
            Self::Read { partial, .. } => Self::Read {
                output_bytes,
                partial: Some(combine(partial)),
            },
            Self::Harness {
                exit_code, partial, ..
            } => Self::Harness {
                exit_code,
                output_bytes,
                partial: Some(combine(partial)),
            },
            Self::NoMatch {
                test_identity,
                partial,
                ..
            } => Self::NoMatch {
                test_identity,
                output_bytes,
                partial: Some(combine(partial)),
            },
            failure @ (Self::Spawn(_) | Self::InvalidIdentity { .. }) => failure,
        }
    }

    pub fn partial_output(&self) -> Option<&TestRunOutput> {
        match self {
            Self::Cancelled { partial, .. }
            | Self::Timeout { partial, .. }
            | Self::OutputLimit { partial, .. }
            | Self::Read { partial, .. }
            | Self::Harness { partial, .. }
            | Self::NoMatch { partial, .. } => partial.as_ref(),
            Self::Spawn(_) | Self::InvalidIdentity { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestRunStream {
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
pub struct TestRunControl {
    cancelled: Arc<AtomicBool>,
    output_limit: Arc<AtomicU8>,
    output_bytes: Arc<AtomicU64>,
    process_group: Arc<Mutex<Option<u32>>>,
}

impl TestRunControl {
    pub fn cancel(&self) {
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

#[hotpath::measure(future = true, label = "mcp.workflow.affected_tests.run")]
pub async fn run_cargo_tests(
    project_root: PathBuf,
    profile: TestProfile,
    test_names: Vec<String>,
    timeout: Duration,
    control: TestRunControl,
) -> Result<TestRunOutput, TestRunFailure> {
    tokio::task::spawn_blocking(move || {
        run_selected_cargo_tests(&project_root, profile, &test_names, timeout, control)
    })
    .await
    .map_err(|_| TestRunFailure::Spawn("cargo test runner task ended unexpectedly".to_owned()))?
}

/// Cargo arguments that select the compilation units for one managed test
/// run, without the libtest filter that follows the `--` separator.
fn cargo_test_build_args(profile: TestProfile) -> Vec<String> {
    let mut args = vec!["test".to_string(), "--no-fail-fast".to_string()];
    if profile == TestProfile::Release {
        args.push("--release".to_string());
    }
    args
}

pub fn cargo_test_args(profile: TestProfile, test_identity: &str) -> Vec<String> {
    let mut args = cargo_test_build_args(profile);
    args.extend([
        "--".to_string(),
        "--exact".to_string(),
        test_identity.to_owned(),
    ]);
    args
}

fn cargo_test_command(project_root: &Path, profile: TestProfile, test_identity: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(project_root)
        .args(cargo_test_args(profile, test_identity));
    command
}

#[hotpath::measure(label = "mcp.workflow.affected_tests.selected")]
fn run_selected_cargo_tests(
    project_root: &Path,
    profile: TestProfile,
    test_names: &[String],
    timeout: Duration,
    control: TestRunControl,
) -> Result<TestRunOutput, TestRunFailure> {
    if test_names.is_empty() {
        return Err(TestRunFailure::NoMatch {
            test_identity: "<none>".to_owned(),
            output_bytes: control.output_bytes(),
            partial: None,
        });
    }
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return Err(TestRunFailure::Timeout {
            output_bytes: control.output_bytes(),
            partial: None,
        });
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit_code = Some(0);
    for test_identity in test_names {
        if let Err(failure) = validate_test_identity(test_identity) {
            return Err(failure.with_partial_output(stdout, stderr, control.output_bytes()));
        }
        if control.cancelled.load(Ordering::Acquire) {
            return Err(TestRunFailure::Cancelled {
                output_bytes: control.output_bytes(),
                partial: Some(TestRunOutput {
                    exit_code,
                    stdout,
                    stderr,
                    output_bytes: control.output_bytes(),
                }),
            });
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(TestRunFailure::Timeout {
                output_bytes: control.output_bytes(),
                partial: Some(TestRunOutput {
                    exit_code,
                    stdout,
                    stderr,
                    output_bytes: control.output_bytes(),
                }),
            });
        };
        let mut command = cargo_test_command(project_root, profile, test_identity);
        let output = match run_bounded_test_command(&mut command, remaining, control.clone()) {
            Ok(output) => output,
            Err(failure) => {
                return Err(failure.with_partial_output(stdout, stderr, control.output_bytes()));
            }
        };
        let observed = parse_libtest_output(&output.stdout)
            .into_iter()
            .find(|(observed, _)| observed == test_identity);
        stdout.push_str(&output.stdout);
        stderr.push_str(&output.stderr);
        if observed.is_none() {
            let failure = if output.exit_code == Some(0) {
                TestRunFailure::NoMatch {
                    test_identity: test_identity.clone(),
                    output_bytes: control.output_bytes(),
                    partial: None,
                }
            } else {
                TestRunFailure::Harness {
                    exit_code: output.exit_code,
                    output_bytes: control.output_bytes(),
                    partial: None,
                }
            };
            return Err(failure.with_partial_output(stdout, stderr, control.output_bytes()));
        }
        if output.exit_code.is_none() {
            return Err(TestRunFailure::Harness {
                exit_code: None,
                output_bytes: control.output_bytes(),
                partial: Some(TestRunOutput {
                    exit_code: None,
                    stdout,
                    stderr,
                    output_bytes: control.output_bytes(),
                }),
            });
        }
        if output.exit_code != Some(0) {
            exit_code = output.exit_code;
        }
    }
    Ok(TestRunOutput {
        exit_code,
        stdout,
        stderr,
        output_bytes: control.output_bytes(),
    })
}

fn validate_test_identity(test_identity: &str) -> Result<(), TestRunFailure> {
    if test_identity.trim().is_empty()
        || test_identity.trim() != test_identity
        || test_identity.starts_with('-')
        || test_identity.contains('\0')
        || test_identity.chars().any(char::is_whitespace)
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
            partial: Some(TestRunOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                output_bytes: control.output_bytes(),
            }),
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
    let partial = TestRunOutput {
        exit_code: None,
        stdout: stdout.text(),
        stderr: stderr.text(),
        output_bytes,
    };

    match outcome {
        ProcessOutcome::Completed(status) => {
            if let Some(stream) = control.exceeded_output() {
                return Err(TestRunFailure::OutputLimit {
                    stream,
                    output_bytes,
                    partial: Some(partial),
                });
            }
            if !stdout.is_captured() || !stderr.is_captured() {
                return Err(TestRunFailure::Read {
                    output_bytes,
                    partial: Some(partial),
                });
            }
            Ok(TestRunOutput {
                exit_code: status.code(),
                stdout: partial.stdout,
                stderr: partial.stderr,
                output_bytes,
            })
        }
        ProcessOutcome::Cancelled => Err(TestRunFailure::Cancelled {
            output_bytes,
            partial: Some(partial),
        }),
        ProcessOutcome::TimedOut => Err(TestRunFailure::Timeout {
            output_bytes,
            partial: Some(partial),
        }),
        ProcessOutcome::OutputLimit(stream) => Err(TestRunFailure::OutputLimit {
            stream,
            output_bytes,
            partial: Some(partial),
        }),
        ProcessOutcome::ReadFailure => Err(TestRunFailure::Read {
            output_bytes,
            partial: Some(partial),
        }),
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
        // Stop sleeping as soon as the child exits; the group kill below still
        // runs so grandchildren that ignored SIGTERM cannot outlive the grace.
        let grace_deadline = Instant::now() + TERMINATION_GRACE;
        while Instant::now() < grace_deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        signal_process_tree(process_group, TerminationSignal::Kill);
        let _ = child.kill();
        let _ = child.wait();
    }
}

enum StreamCapture {
    Captured(Vec<u8>),
    Exceeded(Vec<u8>),
    ReadFailure(Vec<u8>),
}

impl StreamCapture {
    fn is_captured(&self) -> bool {
        match self {
            Self::Captured(_) => true,
            Self::Exceeded(_) | Self::ReadFailure(_) => false,
        }
    }

    fn text(&self) -> String {
        let bytes = match self {
            Self::Captured(bytes) | Self::Exceeded(bytes) | Self::ReadFailure(bytes) => bytes,
        };
        String::from_utf8_lossy(bytes).into_owned()
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
            Err(_) => return StreamCapture::ReadFailure(output),
        };
        if read == 0 {
            return StreamCapture::Captured(output);
        }
        if !control.reserve_output(stream, read) {
            return StreamCapture::Exceeded(output);
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn join_reader(reader: Option<thread::JoinHandle<StreamCapture>>) -> StreamCapture {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or(StreamCapture::ReadFailure(Vec::new()))
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

pub fn parse_libtest_output(stdout: &str) -> Vec<(String, bool)> {
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
    use std::fmt::{self, Display};
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        MAX_TEST_RUN_OUTPUT_BYTES, TestProfile, TestRunControl, TestRunFailure, TestRunOutput,
        TestRunStream, cargo_test_build_args, run_bounded_test_command, run_cargo_tests,
    };

    const FIXTURE_MODE: &str = "TRACEDECAY_TEST_RUNNER_FIXTURE_MODE";
    const FIXTURE_MARKER: &str = "TRACEDECAY_TEST_RUNNER_FIXTURE_MARKER";
    const LIMITED_OUTPUT_BYTES: usize = 96 * 1024;

    /// Budget for provisioning a cargo fixture (cold compile plus test
    /// discovery) before the phase whose execution deadline is under test.
    ///
    /// Cold compilation of even a tiny fixture on a fresh Windows runner can
    /// exceed the execution deadline the exact-test cases assert against, so
    /// provisioning gets its own generous budget and its own output budget.
    /// The production runner's timeout semantics are unchanged: the execution
    /// phase below still runs `run_cargo_tests` against the genuine deadline.
    const FIXTURE_PROVISION_BUDGET: Duration = Duration::from_mins(5);
    /// Deadline for the execution phase once the fixture test binary is warm.
    const EXACT_TEST_EXECUTION_DEADLINE: Duration = Duration::from_secs(10);

    /// A cargo fixture whose test binary is compiled and whose tests are
    /// discovered before any execution-deadline phase starts.
    ///
    /// The recorded phase timings attribute a failure to provisioning
    /// (compile, discovery) or execution rather than conflating them.
    struct ProvisionedFixture {
        root: tempfile::TempDir,
        compile_elapsed: Duration,
        discovery_elapsed: Duration,
        discovered_tests: Vec<String>,
    }

    impl ProvisionedFixture {
        fn path(&self) -> &Path {
            self.root.path()
        }

        fn phases(&self) -> FixturePhases<'_> {
            FixturePhases(self)
        }

        /// Runs the execution phase against the genuine production deadline
        /// and attributes any failure to that phase.
        async fn execute(&self, test_names: Vec<String>) -> Result<TestRunOutput, TestRunFailure> {
            let started = Instant::now();
            let result = run_cargo_tests(
                self.path().to_path_buf(),
                TestProfile::Debug,
                test_names,
                EXACT_TEST_EXECUTION_DEADLINE,
                TestRunControl::default(),
            )
            .await;
            if let Err(TestRunFailure::Timeout { partial, .. }) = &result {
                panic!(
                    "execution phase exceeded its {:?} deadline after {:?} on a warm fixture ({}); stderr:\n{}",
                    EXACT_TEST_EXECUTION_DEADLINE,
                    started.elapsed(),
                    self.phases(),
                    partial.as_ref().map_or("", |output| output.stderr.as_str()),
                );
            }
            result
        }
    }

    struct FixturePhases<'a>(&'a ProvisionedFixture);

    impl Display for FixturePhases<'_> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "compile {:?}, discovery {:?}, discovered {:?}",
                self.0.compile_elapsed, self.0.discovery_elapsed, self.0.discovered_tests
            )
        }
    }

    fn write_fixture_package(root: &Path, name: &str, lib_source: &str) {
        fs::create_dir_all(root.join("src")).expect("source directory");
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .expect("manifest");
        fs::write(root.join("src/lib.rs"), lib_source).expect("source");
    }

    fn provision_fixture(name: &str, lib_source: &str) -> ProvisionedFixture {
        let root = tempfile::TempDir::new().expect("temp");
        write_fixture_package(root.path(), name, lib_source);
        let compile_elapsed = compile_fixture_tests(root.path());
        let (discovery_elapsed, discovered_tests) = discover_fixture_tests(root.path());
        ProvisionedFixture {
            root,
            compile_elapsed,
            discovery_elapsed,
            discovered_tests,
        }
    }

    /// Compile phase: build exactly the units the production runner will
    /// invoke, using the same cargo flags plus `--no-run`.
    fn compile_fixture_tests(root: &Path) -> Duration {
        let mut args = cargo_test_build_args(TestProfile::Debug);
        args.push("--no-run".to_owned());
        run_provisioning_phase("compile", root, &args).0
    }

    /// Discovery phase: list the compiled test binary's tests so a later
    /// `NoMatch` or missing result can be attributed to selection rather than
    /// to a fixture that never contained the test.
    fn discover_fixture_tests(root: &Path) -> (Duration, Vec<String>) {
        let mut args = cargo_test_build_args(TestProfile::Debug);
        args.extend(["--".to_owned(), "--list".to_owned()]);
        let (elapsed, stdout) = run_provisioning_phase("discovery", root, &args);
        let mut discovered: Vec<String> = stdout
            .lines()
            .filter_map(|line| line.trim().strip_suffix(": test"))
            .map(str::to_owned)
            .collect();
        discovered.sort();
        (elapsed, discovered)
    }

    fn run_provisioning_phase(phase: &str, root: &Path, args: &[String]) -> (Duration, String) {
        let mut command = Command::new("cargo");
        command.current_dir(root).args(args);
        let started = Instant::now();
        let result = run_bounded_test_command(
            &mut command,
            FIXTURE_PROVISION_BUDGET,
            TestRunControl::default(),
        );
        let elapsed = started.elapsed();
        match result {
            Ok(output) if output.exit_code == Some(0) => (elapsed, output.stdout),
            Ok(output) => panic!(
                "fixture {phase} phase failed after {elapsed:?} with exit {:?}; stderr:\n{}",
                output.exit_code, output.stderr
            ),
            Err(failure) => panic!(
                "fixture {phase} phase did not complete within {FIXTURE_PROVISION_BUDGET:?} (took {elapsed:?}): {failure:?}"
            ),
        }
    }

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
                    .arg("workflow::test_runner::tests::bounded_runner_fixture")
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
                thread::sleep(Duration::from_mins(1));
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
        let fixture = provision_fixture(
            "bounded-runner-fixture",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn selected_one() {}\n\n    #[test]\n    fn selected_two() {}\n\n    #[test]\n    fn excluded() { panic!(\"must stay filtered\"); }\n}\n",
        );
        assert_eq!(
            fixture.discovered_tests,
            [
                "tests::excluded",
                "tests::selected_one",
                "tests::selected_two"
            ],
            "discovery phase must see every fixture test ({})",
            fixture.phases()
        );

        let output = fixture
            .execute(vec![
                "tests::selected_one".to_owned(),
                "tests::selected_two".to_owned(),
            ])
            .await
            .unwrap_or_else(|failure| {
                panic!(
                    "execution phase must run the selected tests ({}): {failure:?}",
                    fixture.phases()
                )
            });

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(
            super::parse_libtest_output(&output.stdout),
            vec![
                ("tests::selected_one".to_owned(), true),
                ("tests::selected_two".to_owned(), true)
            ],
            "each requested exact test must execute exactly once ({})",
            fixture.phases()
        );
        assert!(!output.stdout.contains("tests::excluded"));
    }

    #[tokio::test]
    async fn cargo_runner_reports_passing_and_failing_exact_tests() {
        let fixture = provision_fixture(
            "failing-exact-runner-fixture",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn passes() {}\n\n    #[test]\n    fn fails() { panic!(\"expected failure\"); }\n}\n",
        );
        assert_eq!(
            fixture.discovered_tests,
            ["tests::fails", "tests::passes"],
            "discovery phase must see every fixture test ({})",
            fixture.phases()
        );

        let output = fixture
            .execute(vec!["tests::passes".to_owned(), "tests::fails".to_owned()])
            .await
            .unwrap_or_else(|failure| {
                panic!(
                    "the runner must retain an observed failing test result ({}): {failure:?}",
                    fixture.phases()
                )
            });

        assert_eq!(output.exit_code, Some(101));
        assert_eq!(
            super::parse_libtest_output(&output.stdout),
            vec![
                ("tests::passes".to_owned(), true),
                ("tests::fails".to_owned(), false)
            ],
            "result collection must keep the failing result ({})",
            fixture.phases()
        );
    }

    #[tokio::test]
    async fn cargo_runner_rejects_a_vacuous_exact_test_run() {
        let fixture = provision_fixture(
            "vacuous-runner-fixture",
            "#[cfg(test)]\nmod tests { #[test] fn present() {} }\n",
        );
        assert_eq!(
            fixture.discovered_tests,
            ["tests::present"],
            "discovery phase must confirm the requested test is genuinely absent ({})",
            fixture.phases()
        );

        let result = fixture.execute(vec!["tests::missing".to_owned()]).await;

        assert!(
            matches!(
                &result,
                Err(TestRunFailure::NoMatch { test_identity, .. }) if test_identity == "tests::missing"
            ),
            "a selection that matches nothing must be NoMatch, not a timeout or pass ({}): {result:?}",
            fixture.phases()
        );
    }

    /// The cold-build contract, kept distinct from the warm execution cases:
    /// cancelling while cargo is still compiling terminates the whole cargo
    /// process tree, including the build script it spawned.
    #[tokio::test]
    async fn cancelling_a_stalled_fixture_compile_terminates_the_cargo_process_tree() {
        let temp = tempfile::TempDir::new().expect("temp");
        let marker = temp.path().join("build-script.pid");
        let staging = temp.path().join("build-script.pid.partial");
        write_fixture_package(
            temp.path(),
            "stalled-compile-fixture",
            "#[cfg(test)]\nmod tests { #[test] fn present() {} }\n",
        );
        // Write the pid to a staging file and rename it into place so the
        // marker is only ever observed complete.
        fs::write(
            temp.path().join("build.rs"),
            format!(
                concat!(
                    "fn main() {{\n",
                    "    std::fs::write({staging:?}, std::process::id().to_string()).expect(\"staging marker\");\n",
                    "    std::fs::rename({staging:?}, {marker:?}).expect(\"publish marker\");\n",
                    "    std::thread::sleep(std::time::Duration::from_secs(60));\n",
                    "}}\n",
                ),
                staging = staging.to_str().expect("utf-8 staging path"),
                marker = marker.to_str().expect("utf-8 marker path"),
            ),
        )
        .expect("build script");

        let control = TestRunControl::default();
        let canceller = {
            let control = control.clone();
            let marker = marker.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + FIXTURE_PROVISION_BUDGET;
                while !marker.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                let stalled = marker.exists();
                control.cancel();
                stalled
            })
        };

        let started = Instant::now();
        let result = run_cargo_tests(
            temp.path().to_path_buf(),
            TestProfile::Debug,
            vec!["tests::present".to_owned()],
            FIXTURE_PROVISION_BUDGET + Duration::from_secs(30),
            control,
        )
        .await;
        let elapsed = started.elapsed();
        let stalled = canceller.join().expect("canceller");

        assert!(
            stalled,
            "compile phase never reached the stalled build script within {FIXTURE_PROVISION_BUDGET:?}: {result:?}"
        );
        assert!(
            matches!(result, Err(TestRunFailure::Cancelled { .. })),
            "cancelling a stalled compile must report Cancelled after {elapsed:?}, not a timeout or NoMatch: {result:?}"
        );
        #[cfg(unix)]
        assert_reaped(&marker);
    }

    #[tokio::test]
    async fn cargo_runner_rejects_option_like_test_identity_before_spawning() {
        let result = run_cargo_tests(
            PathBuf::from("/no/such/project"),
            TestProfile::Debug,
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
            ..
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
            .arg("workflow::test_runner::tests::bounded_runner_fixture")
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
