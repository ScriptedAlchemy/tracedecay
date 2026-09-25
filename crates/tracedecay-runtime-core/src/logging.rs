//! Always-visible daemon operator-log lines.
//!
//! The operator contract is a single stderr logfmt line
//! `[tracedecay] event=<name> k=v …`. Unix writes it to fd 2 so it
//! appears when `RUST_LOG` is unset; the stderr tracing subscriber (default
//! WARN) does not filter it.

use std::collections::BTreeMap;
use std::fmt::Write;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Opening marker of every bespoke daemon log line. Watcher recovery anchors
/// on it, so `tracing` output, which never carries the marker, cannot forge
/// a `git_watch_*` event through a structured field that happens to be named
/// `event`.
pub const DAEMON_LOG_MARKER: &str = "[tracedecay] event=";

/// Format one operator-log line. Values that are not a safe token are quoted.
#[must_use]
pub fn format_daemon_log_line(event: &str, fields: &[(&str, String)]) -> String {
    let mut line = format!("{DAEMON_LOG_MARKER}{}", quote_log_value(event));
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote_log_value(value));
    }
    line
}

fn quote_log_value(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':'))
    {
        return value.to_string();
    }

    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(escaped, "\\u{{{:x}}}", ch as u32);
            }
            ch => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

/// Emit one operator-log line to stderr. Returns `()` so match-arm call
/// sites stay type-compatible. Unix writes fd 2 directly so a test harness
/// that captures `eprintln` cannot hide the operator line; the unix test
/// below `dup2`s fd 2 and asserts the exact [`format_daemon_log_line`] bytes.
pub fn log_daemon_event(event: &str, fields: &[(&str, String)]) {
    let line = format_daemon_log_line(event, fields);
    write_operator_line(&line);
}

#[cfg(unix)]
fn write_operator_line(line: &str) {
    const STDERR_FD: RawFd = 2;
    // SAFETY: fd 2 is process stderr for the life of the process. The
    // `File` must not close it, so it is wrapped in `ManuallyDrop`.
    let mut stderr = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(STDERR_FD) });
    let _ = writeln!(&mut *stderr, "{line}");
}

#[cfg(not(unix))]
fn write_operator_line(line: &str) {
    eprintln!("{line}");
}

/// Upper bound on distinct conditions one gate remembers. A gate that grows
/// past it forgets everything and starts over: the cost is one repeated line
/// per live condition, never unbounded memory for a log filter.
const STATE_CHANGE_LOG_GATE_CAPACITY: usize = 4096;

/// Admits a deterministic condition to the log once per state change instead
/// of once per observation.
///
/// A background pass that re-observes the same terminal or deterministic
/// condition on every tick (a corrupt store, a malformed session file, a
/// pre-admission refusal) otherwise writes an identical warning per tick, and
/// a managed service log grows without bound on a failure that is not
/// changing. The gate keys on the typed condition (`K`) and its typed state
/// (`S`): the first observation and every change of state are admitted,
/// identical repeats are counted, and a condition that clears is forgotten so
/// its next occurrence is admitted again.
pub struct StateChangeLogGate<K, S> {
    last: Mutex<BTreeMap<K, S>>,
    suppressed: AtomicU64,
}

impl<K: Ord, S: PartialEq> StateChangeLogGate<K, S> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: Mutex::new(BTreeMap::new()),
            suppressed: AtomicU64::new(0),
        }
    }

    /// Whether this observation of `key` in `state` should be logged.
    pub fn admit(&self, key: K, state: S) -> bool {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if last.get(&key) == Some(&state) {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if last.len() >= STATE_CHANGE_LOG_GATE_CAPACITY && !last.contains_key(&key) {
            last.clear();
        }
        last.insert(key, state);
        true
    }

    /// Forget `key`. Returns whether a state was being suppressed for it, so
    /// the caller can log the transition out of the condition exactly once.
    pub fn clear(&self, key: &K) -> bool {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)
            .is_some()
    }

    /// Observations this gate kept out of the log.
    #[must_use]
    pub fn suppressed(&self) -> u64 {
        self.suppressed.load(Ordering::Relaxed)
    }
}

impl<K: Ord, S: PartialEq> Default for StateChangeLogGate<K, S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod state_change_gate_tests {
    use super::StateChangeLogGate;

    #[test]
    fn identical_repeats_are_suppressed_until_the_state_changes_or_clears() {
        let gate: StateChangeLogGate<&str, &str> = StateChangeLogGate::new();
        assert!(gate.admit("store", "corrupt"));
        assert!(!gate.admit("store", "corrupt"));
        assert!(!gate.admit("store", "corrupt"));
        assert_eq!(gate.suppressed(), 2);

        assert!(gate.admit("store", "busy"), "a changed state is a new line");
        assert!(!gate.admit("store", "busy"));

        assert!(gate.admit("kimi", "invalid"), "keys are independent");
        assert!(gate.clear(&"store"));
        assert!(!gate.clear(&"store"), "a cleared key is forgotten once");
        assert!(
            gate.admit("store", "busy"),
            "the next occurrence logs again"
        );
        assert_eq!(gate.suppressed(), 3);
    }

    #[test]
    fn a_full_gate_forgets_everything_instead_of_growing() {
        let gate: StateChangeLogGate<usize, ()> = StateChangeLogGate::new();
        for key in 0..super::STATE_CHANGE_LOG_GATE_CAPACITY {
            assert!(gate.admit(key, ()));
        }
        assert!(
            !gate.admit(0, ()),
            "a full gate still suppresses known keys"
        );
        assert!(
            gate.admit(usize::MAX, ()),
            "a new key past capacity is admitted"
        );
        assert!(
            gate.admit(0, ()),
            "reaching capacity forgot the old keys rather than refusing the new one"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::format_daemon_log_line;
    #[cfg(unix)]
    use std::io::{Read, Seek, SeekFrom, Write};
    #[cfg(unix)]
    use std::os::fd::AsRawFd;

    #[cfg(unix)]
    fn capture_stderr(emit: impl FnOnce()) -> String {
        let mut tmp = tempfile::tempfile().expect("stderr capture file");
        // SAFETY: `saved` is a new fd onto the current stderr; `dup2` onto
        // fd 2 is restored before this function returns, so the process
        // stderr identity is unchanged for later tests.
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            assert!(saved >= 0, "dup stderr");
            assert_eq!(
                libc::dup2(tmp.as_raw_fd(), libc::STDERR_FILENO),
                libc::STDERR_FILENO
            );
            emit();
            let _ = std::io::stderr().flush();
            assert_eq!(libc::dup2(saved, libc::STDERR_FILENO), libc::STDERR_FILENO);
            libc::close(saved);
        }
        tmp.seek(SeekFrom::Start(0)).expect("rewind capture");
        let mut buf = String::new();
        tmp.read_to_string(&mut buf).expect("read capture");
        buf
    }

    #[cfg(unix)]
    #[test]
    fn log_daemon_event_writes_the_logfmt_line_to_stderr_when_rust_log_is_unset() {
        assert!(
            std::env::var_os("RUST_LOG").is_none(),
            "visibility is the unset-RUST_LOG path; the emitter is eprintln, not tracing"
        );
        let fields = [
            ("pass", "code_generations".to_string()),
            (
                "failure",
                "registered_enrollment_inventory_unavailable".to_string(),
            ),
        ];
        let expected = format_daemon_log_line("retention_degraded", &fields);
        let captured = capture_stderr(|| super::log_daemon_event("retention_degraded", &fields));
        assert_eq!(
            captured.lines().find(|line| *line == expected.as_str()),
            Some(expected.as_str()),
            "stderr capture: {captured:?}"
        );
        assert_eq!(
            expected,
            "[tracedecay] event=retention_degraded pass=code_generations failure=registered_enrollment_inventory_unavailable"
        );
    }
}
