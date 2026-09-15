//! Always-visible daemon operator-log lines.
//!
//! The operator contract is a single stderr logfmt line
//! `[tracedecay] event=<name> k=v …`. Unix writes it to fd 2 so it
//! appears when `RUST_LOG` is unset; the stderr tracing subscriber (default
//! WARN) does not filter it.

use std::fmt::Write;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};

/// Opening marker of every bespoke daemon log line. Watcher recovery anchors
/// on it, so `tracing` output — which never carries the marker — cannot forge
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
