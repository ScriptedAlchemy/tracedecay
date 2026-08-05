use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tracedecay_application::{WorkProviderRun, WorkProviderSettlementV1};
use tracedecay_domain::{WorkApprovalPolicy, WorkExecutionBudgetV1, WorkFilesystemPolicy};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERM_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeCliKind {
    ClaudeCode,
    Codex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeCliLaunchPlan {
    pub executable: PathBuf,
    pub kind: NativeCliKind,
    pub model: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub budget: WorkExecutionBudgetV1,
    pub approval: WorkApprovalPolicy,
    pub filesystem: WorkFilesystemPolicy,
    pub environment: BTreeMap<String, String>,
}

impl NativeCliLaunchPlan {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        match self.kind {
            NativeCliKind::ClaudeCode => {
                let permission_mode = match self.filesystem {
                    WorkFilesystemPolicy::ReadOnly => "dontAsk",
                    WorkFilesystemPolicy::WorkspaceWrite => "acceptEdits",
                };
                command.args([
                    "-p",
                    &self.prompt,
                    "--model",
                    &self.model,
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--permission-mode",
                    permission_mode,
                ]);
            }
            NativeCliKind::Codex => {
                let approval = match self.approval {
                    WorkApprovalPolicy::Never => "never",
                    WorkApprovalPolicy::OnRequest => "on-request",
                };
                let sandbox = match self.filesystem {
                    WorkFilesystemPolicy::ReadOnly => "read-only",
                    WorkFilesystemPolicy::WorkspaceWrite => "workspace-write",
                };
                command.args([
                    "exec",
                    "--json",
                    "--cd",
                    self.cwd.to_string_lossy().as_ref(),
                    "--model",
                    &self.model,
                    "--ask-for-approval",
                    approval,
                    "--sandbox",
                    sandbox,
                    &self.prompt,
                ]);
            }
        }
        command
            .env_clear()
            .envs(&self.environment)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        command
    }
}

#[derive(Clone, Default)]
pub(super) struct NativeCliCancellation {
    cancelled: Arc<AtomicBool>,
    process_group: Arc<Mutex<Option<u32>>>,
}

impl NativeCliCancellation {
    fn register(&self, process_group: u32) {
        *self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(process_group);
        if self.is_cancelled() {
            signal_process_tree(process_group, TerminationSignal::Terminate);
        }
    }

    fn unregister(&self, process_group: u32) {
        let mut registered = self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *registered == Some(process_group) {
            *registered = None;
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(process_group) = *self
            .process_group
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            signal_process_tree(process_group, TerminationSignal::Terminate);
        }
    }
}

pub(super) struct NativeCliWorkRun {
    pub plan: NativeCliLaunchPlan,
    pub cancellation: NativeCliCancellation,
}

impl WorkProviderRun for NativeCliWorkRun {
    fn execute(&self) -> WorkProviderSettlementV1 {
        match self.execute_inner() {
            NativeProcessOutcome::Completed { stdout } => {
                if stdout.len() as u64 > self.plan.budget.max_protocol_bytes() {
                    return WorkProviderSettlementV1::Failed {
                        message: "provider protocol stream exceeded its bound".to_owned(),
                    };
                }
                match self.plan.kind {
                    NativeCliKind::ClaudeCode => parse_claude_terminal(&stdout),
                    NativeCliKind::Codex => parse_codex_terminal(&stdout),
                }
            }
            NativeProcessOutcome::Cancelled => WorkProviderSettlementV1::Cancelled,
            NativeProcessOutcome::TimedOut => WorkProviderSettlementV1::TimedOut,
            NativeProcessOutcome::Failed(message) => WorkProviderSettlementV1::Failed { message },
        }
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl NativeCliWorkRun {
    fn execute_inner(&self) -> NativeProcessOutcome {
        if self.plan.timeout.is_zero() {
            return NativeProcessOutcome::TimedOut;
        }
        let mut child = match self.plan.command().spawn() {
            Ok(child) => child,
            Err(_) => {
                return NativeProcessOutcome::Failed(
                    "configured provider process could not start".to_owned(),
                );
            }
        };
        let process_group = child.id();
        self.cancellation.register(process_group);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_limit = self.plan.budget.max_stdout_bytes();
        let stdout_reader =
            stdout.map(|stdout| thread::spawn(move || read_bounded(stdout, stdout_limit)));
        let stderr_limit = self.plan.budget.max_stderr_bytes();
        let stderr_reader =
            stderr.map(|stderr| thread::spawn(move || read_bounded(stderr, stderr_limit)));

        let deadline = Instant::now() + self.plan.timeout;
        let (status, stop_reason) =
            wait_for_process(&mut child, process_group, deadline, &self.cancellation);
        self.cancellation.unregister(process_group);
        let stdout = join_reader(stdout_reader);
        let stderr = join_reader(stderr_reader);

        match stop_reason {
            Some(StopReason::Cancelled) => NativeProcessOutcome::Cancelled,
            Some(StopReason::TimedOut) => NativeProcessOutcome::TimedOut,
            None => match (status, stdout, stderr) {
                (Some(status), Ok(stdout), Ok(_)) if status.success() => {
                    NativeProcessOutcome::Completed { stdout }
                }
                (_, Err(()), _) => {
                    NativeProcessOutcome::Failed("provider stdout exceeded its bound".to_owned())
                }
                (_, _, Err(())) => {
                    NativeProcessOutcome::Failed("provider stderr exceeded its bound".to_owned())
                }
                _ => NativeProcessOutcome::Failed(
                    "provider exited without a successful terminal event".to_owned(),
                ),
            },
        }
    }
}

enum NativeProcessOutcome {
    Completed { stdout: Vec<u8> },
    Cancelled,
    TimedOut,
    Failed(String),
}

#[derive(Clone, Copy)]
enum StopReason {
    Cancelled,
    TimedOut,
}

fn wait_for_process(
    child: &mut Child,
    process_group: u32,
    deadline: Instant,
    cancellation: &NativeCliCancellation,
) -> (Option<ExitStatus>, Option<StopReason>) {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), None),
            Ok(None) => {}
            Err(_) => {
                terminate_and_reap(child, process_group);
                return (None, None);
            }
        }
        let stop_reason = if cancellation.is_cancelled() {
            Some(StopReason::Cancelled)
        } else if Instant::now() >= deadline {
            Some(StopReason::TimedOut)
        } else {
            None
        };
        if let Some(stop_reason) = stop_reason {
            terminate_and_reap(child, process_group);
            return (child.try_wait().ok().flatten(), Some(stop_reason));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn terminate_and_reap(child: &mut Child, process_group: u32) {
    signal_process_tree(process_group, TerminationSignal::Terminate);
    let grace_deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < grace_deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
    signal_process_tree(process_group, TerminationSignal::Kill);
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded(mut reader: impl Read, limit: u64) -> Result<Vec<u8>, ()> {
    let limit = usize::try_from(limit).map_err(|_| ())?;
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    let mut oversized = false;
    loop {
        let read = reader.read(&mut chunk).map_err(|_| ())?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > limit {
            oversized = true;
        } else if !oversized {
            output.extend_from_slice(&chunk[..read]);
        }
    }
    if oversized { Err(()) } else { Ok(output) }
}

fn join_reader(reader: Option<thread::JoinHandle<Result<Vec<u8>, ()>>>) -> Result<Vec<u8>, ()> {
    reader.ok_or(())?.join().map_err(|_| ())?
}

fn parse_claude_terminal(stdout: &[u8]) -> WorkProviderSettlementV1 {
    let Ok(stdout) = std::str::from_utf8(stdout) else {
        return malformed_protocol();
    };
    let mut terminal = None;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return malformed_protocol();
        };
        if protocol_version_drifted(&value) {
            return malformed_protocol();
        }
        if value.get("type").and_then(Value::as_str) == Some("result") {
            if terminal.is_some() || value.get("is_error").and_then(Value::as_bool) == Some(true) {
                return malformed_protocol();
            }
            terminal = value
                .get("result")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
        } else if terminal.is_some() {
            return malformed_protocol();
        }
    }
    terminal.map_or_else(malformed_protocol, |evidence| {
        WorkProviderSettlementV1::Completed { evidence }
    })
}

fn parse_codex_terminal(stdout: &[u8]) -> WorkProviderSettlementV1 {
    let Ok(stdout) = std::str::from_utf8(stdout) else {
        return malformed_protocol();
    };
    let mut terminal = false;
    let mut evidence = String::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return malformed_protocol();
        };
        if protocol_version_drifted(&value) {
            return malformed_protocol();
        }
        match value.get("type").and_then(Value::as_str) {
            Some("item.completed")
                if value.pointer("/item/type").and_then(Value::as_str) == Some("agent_message") =>
            {
                if let Some(text) = value.pointer("/item/text").and_then(Value::as_str) {
                    evidence.push_str(text);
                }
            }
            Some("turn.completed") if !terminal => terminal = true,
            Some("turn.failed" | "error") => {
                return WorkProviderSettlementV1::Failed {
                    message: "Codex reported a terminal protocol failure".to_owned(),
                };
            }
            _ if terminal => return malformed_protocol(),
            _ => {}
        }
    }
    let evidence = evidence.trim();
    if !terminal || evidence.is_empty() {
        return malformed_protocol();
    }
    WorkProviderSettlementV1::Completed {
        evidence: evidence.to_owned(),
    }
}

fn protocol_version_drifted(value: &Value) -> bool {
    value
        .get("protocol_version")
        .is_some_and(|version| version.as_u64() != Some(1))
}

fn malformed_protocol() -> WorkProviderSettlementV1 {
    WorkProviderSettlementV1::Failed {
        message: "provider stream lacked a valid structured terminal event".to_owned(),
    }
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
fn signal_process_tree(process_group: u32, signal: TerminationSignal) {
    if matches!(signal, TerminationSignal::Kill) {
        let _ = Command::new("taskkill")
            .args(["/PID", &process_group.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(not(any(unix, windows)))]
fn signal_process_tree(_process_group: u32, _signal: TerminationSignal) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_native_terminal_fixtures_bind_both_parsers() {
        assert_eq!(
            parse_claude_terminal(include_bytes!(
                "../../../tests/fixtures/workflow_provider/claude-code-result.json"
            )),
            WorkProviderSettlementV1::Completed {
                evidence: "native claude terminal fixture".to_owned(),
            }
        );
        assert_eq!(
            parse_codex_terminal(include_bytes!(
                "../../../tests/fixtures/workflow_provider/codex-exec.jsonl"
            )),
            WorkProviderSettlementV1::Completed {
                evidence: "native codex terminal fixture".to_owned(),
            }
        );
    }

    #[test]
    fn launch_plans_clear_ambient_environment_and_pin_protocol_arguments() {
        let plan = NativeCliLaunchPlan {
            executable: PathBuf::from("/opt/provider"),
            kind: NativeCliKind::ClaudeCode,
            model: "claude-model".to_owned(),
            prompt: "perform work".to_owned(),
            cwd: PathBuf::from("/workspace"),
            timeout: Duration::from_secs(5),
            budget: WorkExecutionBudgetV1::new(100, 100, 100).unwrap(),
            approval: WorkApprovalPolicy::Never,
            filesystem: WorkFilesystemPolicy::WorkspaceWrite,
            environment: BTreeMap::new(),
        };
        let command = plan.command();
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--output-format", "stream-json"])
        );
        assert!(arguments.iter().any(|argument| argument == "--verbose"));
        assert!(command.get_envs().all(|(_, value)| value.is_none()));
    }

    #[test]
    fn free_text_malformed_and_version_drift_never_succeed() {
        for outcome in [
            parse_claude_terminal(b"looks successful"),
            parse_codex_terminal(b"{\"type\":\"item.completed\"}\n"),
            parse_codex_terminal(b"{\"protocol_version\":2,\"type\":\"turn.completed\"}\n"),
        ] {
            assert!(matches!(outcome, WorkProviderSettlementV1::Failed { .. }));
        }
    }

    #[test]
    fn bounded_reader_drains_but_rejects_oversized_streams() {
        assert_eq!(read_bounded(&b"1234"[..], 4).unwrap(), b"1234");
        assert_eq!(read_bounded(&b"12345"[..], 4), Err(()));
    }
}
