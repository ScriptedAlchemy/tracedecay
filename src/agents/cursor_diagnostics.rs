//! Doctor diagnostics for the Cursor MCP integration: log scanning and
//! plugin-bundle staleness.
//!
//! Cursor never retries an MCP server whose spawn failed: one bad startup
//! (literal `${workspaceFolder}` path, uninitialized project, …) turns every
//! later tool call in that session into "Timed out waiting for connection"
//! until the user toggles the server or reloads the window. The stdio spawn
//! happens outside tracedecay's control, so `tracedecay doctor --agent
//! cursor` inspects Cursor's own logs to surface that failure class with
//! concrete remediation.
//!
//! Everything here is best-effort: missing directories, unreadable files, and
//! platform layout differences all degrade to "no findings" instead of
//! failing the doctor run.

use std::path::{Path, PathBuf};

use crate::serve::DEGRADED_SERVE_STDERR_MARKER;

use super::DoctorCounters;

/// How many of the newest Cursor log sessions to scan. Each session directory
/// corresponds to one Cursor launch; older sessions describe long-fixed runs.
const MAX_SESSIONS_SCANNED: usize = 3;

/// Per-file read cap. Cursor MCP logs are normally tens of KB; anything huge
/// is truncated to its tail, which holds the most recent (most relevant)
/// entries.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Findings from scanning Cursor's MCP logs for tracedecay-scoped failures.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CursorMcpLogFindings {
    /// Log lines where tracedecay was spawned with a literal, unexpanded
    /// `${workspaceFolder}` argument.
    pub literal_placeholder_lines: usize,
    /// "Connection failed: MCP error -32000" lines — each one is a failed
    /// spawn whose scope Cursor will never retry.
    pub connection_failures: usize,
    /// Lines where a newer tracedecay serve stayed alive in degraded MCP mode
    /// instead of exiting.
    pub degraded_mode_notices: usize,
    /// Log files (newest session first) that contained at least one finding.
    pub affected_logs: Vec<PathBuf>,
    /// Whether any Cursor MCP log was found at all (distinguishes "clean"
    /// from "nothing to scan").
    pub scanned_any_log: bool,
}

impl CursorMcpLogFindings {
    pub(crate) fn has_findings(&self) -> bool {
        self.literal_placeholder_lines > 0
            || self.connection_failures > 0
            || self.degraded_mode_notices > 0
    }
}

/// Candidate Cursor log roots for the current platform, derived from the home
/// directory only (no env lookups) so doctor runs against temp homes stay
/// hermetic. Non-existent candidates are simply skipped by the scan.
pub(crate) fn cursor_log_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/Cursor/logs"),
        home.join("Library/Application Support/Cursor/logs"),
        home.join("AppData/Roaming/Cursor/logs"),
    ]
}

/// Scans the newest Cursor log sessions under `logs_root` for tracedecay MCP
/// spawn failures.
pub(crate) fn scan_cursor_mcp_logs(logs_root: &Path) -> CursorMcpLogFindings {
    let mut findings = CursorMcpLogFindings::default();
    for session_dir in newest_session_dirs(logs_root, MAX_SESSIONS_SCANNED) {
        for log_path in tracedecay_mcp_logs_in_session(&session_dir) {
            let Some(contents) = read_log_tail(&log_path) else {
                continue;
            };
            findings.scanned_any_log = true;
            // `mcpprocess.log` interleaves every MCP server; only count lines
            // that mention tracedecay. The per-server log is all tracedecay.
            let require_tracedecay_mention = log_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.contains("tracedecay"));
            let mut affected = false;
            for line in contents.lines() {
                if require_tracedecay_mention && !line.contains("tracedecay") {
                    continue;
                }
                if line.contains("${workspaceFolder}") && line.contains("no TraceDecay index") {
                    findings.literal_placeholder_lines += 1;
                    affected = true;
                }
                if line.contains("Connection failed: MCP error -32000") {
                    findings.connection_failures += 1;
                    affected = true;
                }
                if line.contains(DEGRADED_SERVE_STDERR_MARKER) {
                    findings.degraded_mode_notices += 1;
                    affected = true;
                }
            }
            if affected {
                findings.affected_logs.push(log_path);
            }
        }
    }
    findings
}

/// Reports Cursor MCP log findings through the doctor counters, with the
/// concrete remediation for Cursor's no-retry behavior.
pub(crate) fn report_cursor_mcp_log_findings(dc: &mut DoctorCounters, home: &Path) {
    let mut findings = CursorMcpLogFindings::default();
    for root in cursor_log_roots(home) {
        let scanned = scan_cursor_mcp_logs(&root);
        findings.literal_placeholder_lines += scanned.literal_placeholder_lines;
        findings.connection_failures += scanned.connection_failures;
        findings.degraded_mode_notices += scanned.degraded_mode_notices;
        findings.scanned_any_log |= scanned.scanned_any_log;
        findings.affected_logs.extend(scanned.affected_logs);
    }
    if !findings.scanned_any_log {
        // No Cursor MCP logs on this machine (different platform layout, or
        // Cursor has not run) — nothing to report.
        return;
    }
    if !findings.has_findings() {
        dc.pass("No tracedecay MCP spawn failures in recent Cursor logs");
        return;
    }
    if findings.literal_placeholder_lines > 0 {
        dc.warn(&format!(
            "Cursor spawned tracedecay with a literal unexpanded `${{workspaceFolder}}` \
             {} time(s) in recent logs (headless/background agent scopes may not expand it)",
            findings.literal_placeholder_lines
        ));
    }
    if findings.connection_failures > 0 {
        dc.warn(&format!(
            "{} failed tracedecay MCP connection(s) in recent Cursor logs — Cursor never \
             retries a failed MCP server, so affected sessions report \"Timed out waiting \
             for connection\" on every tool call",
            findings.connection_failures
        ));
    }
    if findings.degraded_mode_notices > 0 {
        dc.warn(&format!(
            "tracedecay serve ran in degraded MCP mode {} time(s) recently (project \
             resolution failed at startup); run `tracedecay init` in the affected project",
            findings.degraded_mode_notices
        ));
    }
    dc.info(
        "    After fixing the cause, toggle the tracedecay MCP server in Cursor Settings → MCP \
         or reload the Cursor window — Cursor does not retry a failed MCP scope on its own.",
    );
    for log in findings.affected_logs.iter().take(3) {
        dc.info(&format!("    log: {}", log.display()));
    }
}

/// A warning when the installed plugin bundle was rendered by a different
/// tracedecay version than the running binary. A stale bundle keeps steering
/// agents at old tool surfaces (and old MCP/hook commands) until
/// `tracedecay update-plugin` re-renders it.
pub(crate) fn plugin_version_staleness(
    manifest: &serde_json::Value,
    binary_version: &str,
) -> Option<String> {
    let plugin_version = manifest.get("version").and_then(|v| v.as_str())?;
    if plugin_version == binary_version {
        return None;
    }
    Some(format!(
        "Cursor plugin bundle was rendered by tracedecay {plugin_version} but this binary is \
         {binary_version} — run `tracedecay update-plugin`, then reload Cursor"
    ))
}

/// The newest `limit` session directories under a Cursor logs root. Session
/// directory names are `YYYYMMDDTHHMMSS` timestamps, so a lexicographic sort
/// is a chronological sort.
fn newest_session_dirs(logs_root: &Path, limit: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(logs_root) else {
        return Vec::new();
    };
    let mut sessions: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|entry| entry.path())
        .collect();
    sessions.sort();
    sessions.reverse();
    sessions.truncate(limit);
    sessions
}

/// The MCP log files inside one Cursor session directory that can mention
/// tracedecay: the per-server plugin log and the shared MCP process log.
fn tracedecay_mcp_logs_in_session(session_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(session_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == "mcpprocess.log"
                        || (name.starts_with("mcp-server-") && name.contains("tracedecay"))
                })
        })
        .collect()
}

/// Reads a log file, capped to its final [`MAX_LOG_BYTES`] bytes.
fn read_log_tail(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > MAX_LOG_BYTES {
        file.seek(SeekFrom::End(-(MAX_LOG_BYTES as i64))).ok()?;
    }
    let mut contents = String::new();
    // Lossy-tolerant read: a mid-character seek point or stray bytes must not
    // abort the whole scan.
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    contents.push_str(&String::from_utf8_lossy(&bytes));
    Some(contents)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_session_log(logs_root: &Path, session: &str, file_name: &str, contents: &str) {
        let dir = logs_root.join(session);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file_name), contents).unwrap();
    }

    /// Fixture mirroring the real failure captured on 2026-07-02: Cursor
    /// passed a literal `${workspaceFolder}` to `serve --path`, serve exited,
    /// and the scope died with `MCP error -32000`.
    const REAL_FAILURE_LOG: &str = "\
2026-07-02 02:46:22.011 [info] connecting stdio for \"tracedecay\" (plugin-tracedecay-tracedecay)\n\
2026-07-02 02:46:22.473 [error] config error: no TraceDecay index found at '${workspaceFolder}' — run 'tracedecay init' first\n\
2026-07-02 02:46:22.474 [warning] Connection failed: MCP error -32000: Connection closed\n\
2026-07-02 02:46:23.271 [info] Successfully connected to stdio server\n";

    #[test]
    fn scan_detects_literal_placeholder_and_connection_failure() {
        let logs = TempDir::new().unwrap();
        write_session_log(
            logs.path(),
            "20260702T024608",
            "mcp-server-plugin-tracedecay-tracedecay.log",
            REAL_FAILURE_LOG,
        );

        let findings = scan_cursor_mcp_logs(logs.path());
        assert!(findings.scanned_any_log);
        assert_eq!(findings.literal_placeholder_lines, 1);
        assert_eq!(findings.connection_failures, 1);
        assert_eq!(findings.degraded_mode_notices, 0);
        assert_eq!(findings.affected_logs.len(), 1);
        assert!(findings.has_findings());
    }

    /// The scanner must match the exact marker `serve` emits — shared via
    /// [`DEGRADED_SERVE_STDERR_MARKER`] so the two cannot drift.
    #[test]
    fn scan_detects_degraded_mode_notice() {
        let logs = TempDir::new().unwrap();
        write_session_log(
            logs.path(),
            "20260702T030000",
            "mcp-server-plugin-tracedecay-tracedecay.log",
            &format!(
                "2026-07-02 03:00:00.000 [warning] {DEGRADED_SERVE_STDERR_MARKER} — MCP \
                 handshake will complete\n"
            ),
        );

        let findings = scan_cursor_mcp_logs(logs.path());
        assert_eq!(findings.degraded_mode_notices, 1);
        assert!(findings.has_findings());
    }

    #[test]
    fn mcpprocess_log_only_counts_tracedecay_lines() {
        let logs = TempDir::new().unwrap();
        write_session_log(
            logs.path(),
            "20260702T024608",
            "mcpprocess.log",
            "2026-07-02 02:46:22 [warning] Connection failed: MCP error -32000: Connection closed\n\
             2026-07-02 02:46:24 [warning] [McpProcess stderr] tracedecay WARN Connection failed: MCP error -32000: Connection closed\n",
        );

        let findings = scan_cursor_mcp_logs(logs.path());
        assert!(findings.scanned_any_log);
        assert_eq!(
            findings.connection_failures, 1,
            "shared mcpprocess.log lines without a tracedecay mention belong to other servers"
        );
    }

    #[test]
    fn scan_only_reads_newest_sessions() {
        let logs = TempDir::new().unwrap();
        // Old failure beyond the scan window plus MAX_SESSIONS_SCANNED clean
        // newer sessions.
        write_session_log(
            logs.path(),
            "20250101T000000",
            "mcp-server-plugin-tracedecay-tracedecay.log",
            REAL_FAILURE_LOG,
        );
        for session in ["20260701T000000", "20260702T000000", "20260703T000000"] {
            write_session_log(
                logs.path(),
                session,
                "mcp-server-plugin-tracedecay-tracedecay.log",
                "2026-07-02 02:46:23.271 [info] Successfully connected to stdio server\n",
            );
        }

        let findings = scan_cursor_mcp_logs(logs.path());
        assert!(findings.scanned_any_log);
        assert!(
            !findings.has_findings(),
            "failures in sessions older than the scan window must be ignored: {findings:?}"
        );
    }

    #[test]
    fn scan_is_silent_without_logs() {
        let logs = TempDir::new().unwrap();
        let findings = scan_cursor_mcp_logs(&logs.path().join("does-not-exist"));
        assert!(!findings.scanned_any_log);
        assert!(!findings.has_findings());

        // A session dir without MCP logs also stays silent.
        std::fs::create_dir_all(logs.path().join("20260702T024608")).unwrap();
        let findings = scan_cursor_mcp_logs(logs.path());
        assert!(!findings.scanned_any_log);
    }

    #[test]
    fn log_roots_cover_supported_platform_layouts() {
        let home = Path::new("/home/user");
        let roots = cursor_log_roots(home);
        assert!(roots.contains(&home.join(".config/Cursor/logs")));
        assert!(roots.contains(&home.join("Library/Application Support/Cursor/logs")));
        assert!(roots.contains(&home.join("AppData/Roaming/Cursor/logs")));
    }

    #[test]
    fn plugin_version_staleness_flags_mismatch_only() {
        let stale = serde_json::json!({ "name": "tracedecay", "version": "0.1.0" });
        let message = plugin_version_staleness(&stale, "0.2.0")
            .expect("mismatched versions should produce a warning");
        assert!(message.contains("0.1.0"), "{message}");
        assert!(message.contains("0.2.0"), "{message}");
        assert!(message.contains("update-plugin"), "{message}");

        let current = serde_json::json!({ "name": "tracedecay", "version": "0.2.0" });
        assert_eq!(plugin_version_staleness(&current, "0.2.0"), None);

        // A manifest without a version (or a non-string one) is not a
        // staleness signal — the manifest-completeness check owns that.
        assert_eq!(
            plugin_version_staleness(&serde_json::json!({ "name": "tracedecay" }), "0.2.0"),
            None
        );
    }
}
