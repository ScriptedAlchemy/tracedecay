//! Daemon log formatting and git-watcher event recovery for `tracedecay doctor`.

#[cfg(unix)]
use std::collections::HashMap;
use std::fmt::Write;

#[cfg(unix)]
use super::SERVICE_NAME;
use super::{Path, TraceDecayError};
/// A single git-watcher lifecycle event recovered from the daemon log, for the
/// `tracedecay doctor` watcher-health section.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct WatcherEvent {
    /// The `git_watch_*` event name (`started`, `synced`, `degraded`, `restart`).
    pub event: String,
    /// The `project=` field, when present.
    pub project: Option<String>,
    /// The `action=`/`reason=` field, when present (context for the event).
    pub detail: Option<String>,
}

pub(crate) fn format_daemon_log_line(event: &str, fields: &[(&str, String)]) -> String {
    let mut line = format!("[tracedecay] event={}", quote_log_value(event));
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

pub(crate) fn log_daemon_event(event: &str, fields: &[(&str, String)]) {
    eprintln!("{}", format_daemon_log_line(event, fields));
}

/// Installs the process-wide stderr `tracing` subscriber, honoring `RUST_LOG`
/// (default `warn`). Additive to the bespoke `[tracedecay] event=` stderr
/// lines above — both channels share stderr, and tools that parse `event=`
/// lines are unaffected because tracing output never carries that prefix.
///
/// `tracing-subscriber` is deliberately built without the `env-filter`
/// feature (it pulls `matchers`/regex machinery into every build), so the
/// `RUST_LOG` value is reduced to a plain global level with
/// [`stderr_tracing_level`] instead of full per-target directives.
pub fn install_stderr_tracing() {
    let level = stderr_tracing_level(std::env::var("RUST_LOG").ok().as_deref());
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_target(true)
        .compact()
        .try_init();
}

/// Reduces a `RUST_LOG`-style directive list to one global level: the most
/// verbose level named anywhere in it (so `foo=debug` keeps debug events
/// flowing even though per-target filtering is unavailable). Unset, empty, or
/// unparseable input yields the crate's default of `warn`.
pub(crate) fn stderr_tracing_level(env_value: Option<&str>) -> tracing::level_filters::LevelFilter {
    use tracing::level_filters::LevelFilter;
    let default = LevelFilter::WARN;
    let Some(env_value) = env_value else {
        return default;
    };
    env_value
        .split(',')
        .filter_map(|directive| {
            let level = directive
                .rsplit_once('=')
                .map_or(directive, |(_, level)| level);
            level.trim().parse::<LevelFilter>().ok()
        })
        .max()
        .unwrap_or(default)
}

#[cfg(test)]
mod stderr_tracing_tests {
    use tracing::level_filters::LevelFilter;

    use super::stderr_tracing_level;

    #[test]
    fn defaults_to_warn_without_rust_log() {
        assert_eq!(stderr_tracing_level(None), LevelFilter::WARN);
        assert_eq!(stderr_tracing_level(Some("")), LevelFilter::WARN);
        assert_eq!(stderr_tracing_level(Some("not-a-level")), LevelFilter::WARN);
    }

    #[test]
    fn honors_plain_levels_case_insensitively() {
        assert_eq!(stderr_tracing_level(Some("debug")), LevelFilter::DEBUG);
        assert_eq!(stderr_tracing_level(Some("TRACE")), LevelFilter::TRACE);
        assert_eq!(stderr_tracing_level(Some("error")), LevelFilter::ERROR);
        assert_eq!(stderr_tracing_level(Some("off")), LevelFilter::OFF);
    }

    #[test]
    fn takes_the_most_verbose_level_from_directive_lists() {
        assert_eq!(
            stderr_tracing_level(Some("tracedecay=debug,hyper=warn")),
            LevelFilter::DEBUG
        );
        assert_eq!(
            stderr_tracing_level(Some("warn,tokio::task=trace")),
            LevelFilter::TRACE
        );
        assert_eq!(
            stderr_tracing_level(Some("garbage,info")),
            LevelFilter::INFO
        );
    }
}

/// Parses one daemon log line into a [`WatcherEvent`] when it is a `git_watch_*`
/// event. Mirrors [`format_daemon_log_line`] (space-separated `key=value`, values
/// optionally double-quoted). Returns `None` for non-watcher lines.
#[cfg(unix)]
fn parse_watcher_log_line(line: &str) -> Option<WatcherEvent> {
    let idx = line.find("event=")?;
    let rest = &line[idx + "event=".len()..];
    let mut fields = parse_log_fields(rest);
    let event = fields.remove("__first__")?;
    if !event.starts_with("git_watch_") {
        return None;
    }
    let detail = fields
        .remove("action")
        .or_else(|| fields.remove("reason"))
        .or_else(|| fields.remove("branch"));
    Some(WatcherEvent {
        event,
        project: fields.remove("project"),
        detail,
    })
}

/// Splits a `key=value key="quoted value" …` tail into a map. The leading value
/// (the event name, which has no key) is stored under `__first__`.
#[cfg(unix)]
fn parse_log_fields(rest: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if first {
            // Leading unkeyed event-name token.
            let start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            out.insert("__first__".to_string(), unquote(&rest[start..i]));
            first = false;
            continue;
        }
        // key
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        let key = rest[key_start..i].to_string();
        i += 1; // skip '='
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let val_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let v = rest[val_start..i.min(rest.len())].to_string();
            if i < bytes.len() {
                i += 1; // closing quote
            }
            v.replace("\\\"", "\"").replace("\\\\", "\\")
        } else {
            let val_start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            rest[val_start..i].to_string()
        };
        out.insert(key, value);
    }
    out
}

#[cfg(unix)]
fn unquote(s: &str) -> String {
    s.trim_matches('"').to_string()
}

/// Reads recent `git_watch_*` events from the daemon log and returns the most
/// recent event per project. Read-only; used by `tracedecay doctor`.
///
/// Source is platform-specific: systemd user journal on Linux, the launchd
/// `daemon.err.log` on macOS. Returns an empty map when no log source is
/// readable (the doctor treats that as "no watcher telemetry available").
#[cfg(unix)]
pub fn recent_watcher_events(max_lines: usize) -> HashMap<String, WatcherEvent> {
    let text = read_daemon_log_tail(max_lines);
    let mut latest: HashMap<String, WatcherEvent> = HashMap::new();
    for line in text.lines() {
        if let Some(ev) = parse_watcher_log_line(line) {
            let key = ev.project.clone().unwrap_or_else(|| "<global>".to_string());
            latest.insert(key, ev);
        }
    }
    latest
}

/// Best-effort read of the tail of the daemon log across service runners.
#[cfg(unix)]
fn read_daemon_log_tail(max_lines: usize) -> String {
    // macOS launchd: a plain err-log file next to the data dir.
    if let Some(data_dir) = crate::config::user_data_dir() {
        let err_log = data_dir.join("daemon.err.log");
        if let Ok(contents) = std::fs::read_to_string(&err_log) {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            return lines[start..].join("\n");
        }
    }
    // Linux systemd: pull recent journal lines for the user unit.
    let output = std::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            SERVICE_NAME,
            "--no-pager",
            "-n",
            &max_lines.to_string(),
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    }
}

pub fn unavailable_error(socket_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon socket '{}' is not available. Run `tracedecay daemon install-service` and ensure the service is running.",
            socket_path.display()
        ),
    }
}
