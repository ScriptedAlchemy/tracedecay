//! Daemon log formatting and git-watcher event recovery for `tracedecay doctor`.

#[cfg(unix)]
use std::collections::HashMap;
use std::fmt::Write;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

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

/// Opening marker of every bespoke daemon log line. Watcher recovery anchors
/// on it, so `tracing` output — which never carries the marker — cannot forge
/// a `git_watch_*` event through a structured field that happens to be named
/// `event`.
const DAEMON_LOG_MARKER: &str = "[tracedecay] event=";

pub(crate) fn format_daemon_log_line(event: &str, fields: &[(&str, String)]) -> String {
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

pub(crate) fn log_daemon_event(event: &str, fields: &[(&str, String)]) {
    eprintln!("{}", format_daemon_log_line(event, fields));
}

/// The stderr tracing filter derived from a `RUST_LOG` value.
///
/// `tracing-subscriber` is deliberately built without the `env-filter`
/// feature (it pulls `matchers`/regex machinery into every build), so this
/// understands a documented subset of the `RUST_LOG` grammar rather than the
/// full directive language: a bare `level` sets the level for every target,
/// and `target=level` sets the level for targets that start with `target`.
/// Anything else — span selectors, field predicates, a target with no level —
/// is recorded as unparsed and reported once, never reinterpreted as
/// something the operator did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StderrTracingFilter {
    /// Level for targets that no directive names.
    global: LevelFilter,
    /// Target-prefix directives, most specific (longest prefix) first.
    targets: Vec<(String, LevelFilter)>,
    /// Directives this subset cannot honor, in the order they were written.
    unparsed: Vec<String>,
}

impl StderrTracingFilter {
    /// Parses a `RUST_LOG` value. `default` applies to every target the value
    /// does not name, and is what an unset or empty value resolves to.
    pub(crate) fn parse(env_value: Option<&str>, default: LevelFilter) -> Self {
        let mut filter = Self {
            global: default,
            targets: Vec::new(),
            unparsed: Vec::new(),
        };
        let Some(env_value) = env_value else {
            return filter;
        };
        for directive in env_value.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                continue;
            }
            filter.absorb(directive);
        }
        // Longest prefix first, so `level_for_target` can stop at its first
        // match and `tracedecay::daemon=trace` outranks `tracedecay=warn`.
        filter.targets.sort_by(|(left, _), (right, _)| {
            right.len().cmp(&left.len()).then_with(|| left.cmp(right))
        });
        filter
    }

    fn absorb(&mut self, directive: &str) {
        let Some((target, level)) = directive.rsplit_once('=') else {
            match directive.parse::<LevelFilter>() {
                Ok(level) => self.global = level,
                Err(_) => self.unparsed.push(directive.to_string()),
            }
            return;
        };
        let target = target.trim();
        let level = level.trim();
        // An empty level parses as `error` in `tracing-core`, which would turn
        // `foo=` into a directive the operator never wrote.
        match level.parse::<LevelFilter>() {
            Ok(parsed) if !level.is_empty() && is_plain_target(target) => {
                self.targets.push((target.to_string(), parsed));
            }
            _ => self.unparsed.push(directive.to_string()),
        }
    }

    /// Level for one event target: the most specific matching directive, or
    /// the global level when no directive names it.
    pub(crate) fn level_for_target(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(prefix, _)| target.starts_with(prefix.as_str()))
            .map_or(self.global, |(_, level)| *level)
    }

    /// The most verbose level any directive can enable. Only a max-level hint
    /// for the subscriber — [`Self::level_for_target`] still decides each
    /// event, so a target directive never globalizes.
    pub(crate) fn max_level(&self) -> LevelFilter {
        self.targets
            .iter()
            .fold(self.global, |max, (_, level)| max.max(*level))
    }

    /// The directives this subset could not honor.
    pub(crate) fn unparsed(&self) -> &[String] {
        &self.unparsed
    }

    /// One machine-readable line naming every unhonored directive, or `None`
    /// when the whole value was understood. Emitted so a typo in `RUST_LOG`
    /// surfaces as a diagnostic instead of silently changing nothing.
    pub(crate) fn diagnostic(&self) -> Option<String> {
        let unparsed = self.unparsed();
        if unparsed.is_empty() {
            return None;
        }
        Some(format_daemon_log_line(
            "rust_log_unparsed",
            &[
                ("directives", unparsed.join(",")),
                ("global_level", self.global.to_string()),
            ],
        ))
    }
}

/// Whether a `RUST_LOG` directive target is a plain module path this subset
/// can match by prefix, rather than a span or field selector it cannot.
fn is_plain_target(target: &str) -> bool {
    !target.is_empty()
        && !target
            .contains(|ch: char| ch.is_whitespace() || matches!(ch, '[' | ']' | '{' | '}' | '='))
}

/// What the stderr subscriber does when `RUST_LOG` says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrTracingDefault {
    /// Surface warnings, the crate default for ordinary commands.
    Warn,
    /// Emit nothing. Agent hosts read hook stderr as a contract surface and
    /// several treat unexpected output as a hook failure, so a hook may only
    /// speak there when an operator explicitly asks it to.
    Silent,
}

impl StderrTracingDefault {
    fn level(self) -> LevelFilter {
        match self {
            Self::Warn => LevelFilter::WARN,
            Self::Silent => LevelFilter::OFF,
        }
    }
}

/// Installs the process-wide stderr `tracing` subscriber, honoring `RUST_LOG`
/// over `default`. Additive to the bespoke `[tracedecay] event=` stderr lines
/// above — both channels share stderr, and tools that parse `event=` lines are
/// unaffected because tracing output never carries that marker.
///
/// An explicit `RUST_LOG` is operator intent and outranks `default`, including
/// for hooks: `Silent` only decides what happens in its absence.
pub fn install_stderr_tracing(default: StderrTracingDefault) {
    install_stderr_tracing_filter(StderrTracingFilter::parse(
        std::env::var("RUST_LOG").ok().as_deref(),
        default.level(),
    ));
}

fn install_stderr_tracing_filter(filter: StderrTracingFilter) {
    if let Some(diagnostic) = filter.diagnostic() {
        eprintln!("{diagnostic}");
    }
    let max_level = filter.max_level();
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .compact()
        .with_filter(
            tracing_subscriber::filter::filter_fn(move |metadata| {
                filter.level_for_target(metadata.target()) >= *metadata.level()
            })
            .with_max_level_hint(max_level),
        );
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

/// Parses one daemon log line into a [`WatcherEvent`] when it is a `git_watch_*`
/// event. Mirrors [`format_daemon_log_line`] (space-separated `key=value`, values
/// optionally double-quoted). Returns `None` for non-watcher lines.
///
/// The line must carry [`DAEMON_LOG_MARKER`] with `event=` immediately after
/// it. The marker is searched for rather than required at column zero because
/// journald and launchd prepend their own timestamp and unit prefix.
#[cfg(unix)]
fn parse_watcher_log_line(line: &str) -> Option<WatcherEvent> {
    let idx = line.find(DAEMON_LOG_MARKER)?;
    let rest = &line[idx + DAEMON_LOG_MARKER.len()..];
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

#[cfg(test)]
mod stderr_tracing_tests {
    use tracing::level_filters::LevelFilter;

    use super::{StderrTracingDefault, StderrTracingFilter, install_stderr_tracing};

    fn parse(env_value: Option<&str>) -> StderrTracingFilter {
        StderrTracingFilter::parse(env_value, LevelFilter::WARN)
    }

    fn parse_for_hook(env_value: Option<&str>) -> StderrTracingFilter {
        StderrTracingFilter::parse(env_value, StderrTracingDefault::Silent.level())
    }

    #[test]
    fn defaults_to_warn_without_rust_log() {
        for value in [None, Some(""), Some("  ")] {
            let filter = parse(value);
            assert_eq!(filter.level_for_target("tracedecay"), LevelFilter::WARN);
            assert_eq!(filter.max_level(), LevelFilter::WARN);
            assert!(filter.unparsed().is_empty());
        }
    }

    #[test]
    fn honors_plain_levels_case_insensitively() {
        for (value, expected) in [
            ("debug", LevelFilter::DEBUG),
            ("TRACE", LevelFilter::TRACE),
            ("error", LevelFilter::ERROR),
            ("off", LevelFilter::OFF),
        ] {
            let filter = parse(Some(value));
            assert_eq!(filter.level_for_target("anything"), expected);
            assert_eq!(filter.max_level(), expected);
            assert!(filter.diagnostic().is_none());
        }
    }

    #[test]
    fn target_directives_do_not_globalize() {
        let filter = parse(Some("tracedecay=debug,hyper=error"));

        assert_eq!(filter.level_for_target("tracedecay"), LevelFilter::DEBUG);
        assert_eq!(
            filter.level_for_target("tracedecay::daemon"),
            LevelFilter::DEBUG
        );
        assert_eq!(filter.level_for_target("hyper::client"), LevelFilter::ERROR);
        // Untargeted crates keep the default instead of inheriting `debug`.
        assert_eq!(filter.level_for_target("tokio::task"), LevelFilter::WARN);
        // The hint has to cover the most verbose directive or the subscriber
        // would discard the events the operator asked for.
        assert_eq!(filter.max_level(), LevelFilter::DEBUG);
    }

    #[test]
    fn the_most_specific_target_directive_wins() {
        let filter = parse(Some("tracedecay=warn,tracedecay::daemon=trace"));

        assert_eq!(
            filter.level_for_target("tracedecay::daemon::scheduler"),
            LevelFilter::TRACE
        );
        assert_eq!(filter.level_for_target("tracedecay::db"), LevelFilter::WARN);
    }

    #[test]
    fn a_bare_level_sets_the_global_level_alongside_target_directives() {
        let filter = parse(Some("info,tracedecay=trace"));

        assert_eq!(filter.level_for_target("tokio::task"), LevelFilter::INFO);
        assert_eq!(filter.level_for_target("tracedecay"), LevelFilter::TRACE);
        assert_eq!(filter.max_level(), LevelFilter::TRACE);
    }

    #[test]
    fn malformed_directives_are_reported_not_swallowed() {
        let filter = parse(Some("garbage,tracedecay[span]=debug,hyper=,info"));

        let unparsed: Vec<&str> = filter.unparsed().iter().map(String::as_str).collect();
        assert_eq!(unparsed, ["garbage", "tracedecay[span]=debug", "hyper="]);
        // The honored part of the value still applies.
        assert_eq!(filter.level_for_target("tracedecay"), LevelFilter::INFO);
        assert_eq!(
            filter.diagnostic().as_deref(),
            Some(
                "[tracedecay] event=rust_log_unparsed directives=\"garbage,tracedecay[span]=debug,hyper=\" global_level=info"
            )
        );
    }

    #[test]
    fn a_fully_understood_value_reports_nothing() {
        assert!(parse(Some("warn,tracedecay=debug")).diagnostic().is_none());
    }

    #[test]
    fn hooks_stay_silent_until_rust_log_asks_otherwise() {
        let unset = parse_for_hook(None);
        assert_eq!(unset.level_for_target("tracedecay"), LevelFilter::OFF);
        assert_eq!(unset.max_level(), LevelFilter::OFF);

        // An explicit value is operator intent and outranks the hook default.
        let explicit = parse_for_hook(Some("info"));
        assert_eq!(explicit.level_for_target("tracedecay"), LevelFilter::INFO);

        // A target directive raises only that target; everything else on the
        // hook's stderr stays off.
        let scoped = parse_for_hook(Some("tracedecay=debug"));
        assert_eq!(scoped.level_for_target("tracedecay"), LevelFilter::DEBUG);
        assert_eq!(scoped.level_for_target("hyper"), LevelFilter::OFF);
    }

    #[test]
    fn installing_the_subscriber_twice_does_not_panic() {
        install_stderr_tracing(StderrTracingDefault::Warn);
        install_stderr_tracing(StderrTracingDefault::Warn);
    }
}

#[cfg(all(unix, test))]
mod watcher_log_tests {
    use super::parse_watcher_log_line;

    #[test]
    fn journal_prefixed_daemon_lines_yield_watcher_events() {
        let line = concat!(
            "Jul 28 03:00:00 host tracedecay[1234]: ",
            "[tracedecay] event=git_watch_degraded project=/tmp/project reason=\"watch limit reached\""
        );

        let event = parse_watcher_log_line(line).expect("marked daemon line is a watcher event");

        assert_eq!(event.event, "git_watch_degraded");
        assert_eq!(event.project.as_deref(), Some("/tmp/project"));
        assert_eq!(event.detail.as_deref(), Some("watch limit reached"));
    }

    #[test]
    fn tracing_formatted_lines_cannot_forge_watcher_events() {
        // A `tracing` event carrying an `event` field renders `event=...` on
        // stderr without the daemon marker. Accepting it would report watcher
        // health the watcher never claimed.
        let line = concat!(
            "2026-07-28T03:00:00.000000Z  WARN tracedecay::daemon: ",
            "event=git_watch_started project=/tmp/project"
        );

        assert!(parse_watcher_log_line(line).is_none());
    }

    #[test]
    fn non_watcher_daemon_events_are_ignored() {
        let line = "[tracedecay] event=scheduler_task task=memory_curator outcome=start";

        assert!(parse_watcher_log_line(line).is_none());
    }
}
