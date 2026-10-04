//! Daemon log formatting and git-watcher event recovery for `tracedecay doctor`.

#[cfg(unix)]
use std::collections::HashMap;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use std::path::Path;

#[cfg(unix)]
use tracedecay_daemon_control::systemd_unit_name;
use tracedecay_domain::errors::TraceDecayError;
#[cfg(unix)]
use tracedecay_runtime_core::config::ProfileRoot;
#[cfg(unix)]
use tracedecay_runtime_core::logging::DAEMON_LOG_MARKER;
use tracedecay_runtime_core::logging::format_daemon_log_line;

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

/// The stderr tracing filter derived from a `RUST_LOG` value.
///
/// `tracing-subscriber` is deliberately built without the `env-filter`
/// feature (it pulls `matchers`/regex machinery into every build), so this
/// understands a documented subset of the `RUST_LOG` grammar rather than the
/// full directive language: a bare `level` sets the level for every target,
/// and `target=level` sets the level for targets that start with `target`.
/// Anything else is recorded as unparsed and reported once, never
/// reinterpreted as something the operator did not write. That includes span
/// selectors, field predicates, and a target with no level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrTracingFilter {
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
    pub fn parse(env_value: Option<&str>, default: LevelFilter) -> Self {
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
    pub fn level_for_target(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(prefix, _)| target.starts_with(prefix.as_str()))
            .map_or(self.global, |(_, level)| *level)
    }

    /// The most verbose level any directive can enable. Only a max-level hint
    /// for the subscriber, [`Self::level_for_target`] still decides each
    /// event, so a target directive never globalizes.
    pub fn max_level(&self) -> LevelFilter {
        self.targets
            .iter()
            .fold(self.global, |max, (_, level)| max.max(*level))
    }

    /// The directives this subset could not honor.
    pub fn unparsed(&self) -> &[String] {
        &self.unparsed
    }

    /// One machine-readable line naming every unhonored directive, or `None`
    /// when the whole value was understood. Emitted so a typo in `RUST_LOG`
    /// surfaces as a diagnostic instead of silently changing nothing.
    pub fn diagnostic(&self) -> Option<String> {
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
/// above, both channels share stderr, and tools that parse `event=` lines are
/// unaffected because tracing output never carries that marker.
///
/// An explicit `RUST_LOG` is operator intent and outranks `default`, including
/// for hooks: `Silent` only decides what happens in its absence.
///
/// `TRACEDECAY_SPAN_TIMINGS=1` also logs a `close` line with `time.busy` and
/// `time.idle` for every span `RUST_LOG` enables.
pub fn install_stderr_tracing(default: StderrTracingDefault) {
    let filter =
        StderrTracingFilter::parse(std::env::var("RUST_LOG").ok().as_deref(), default.level());
    if let Some(diagnostic) = filter.diagnostic() {
        eprintln!("{diagnostic}");
    }
    let span_timings = std::env::var_os(SPAN_TIMINGS_ENV).is_some_and(|value| value == "1");
    let layer = stderr_tracing_layer(filter, span_timings, std::io::stderr);
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

const SPAN_TIMINGS_ENV: &str = "TRACEDECAY_SPAN_TIMINGS";

fn stderr_tracing_layer<S, W>(
    filter: StderrTracingFilter,
    span_timings: bool,
    writer: W,
) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + 'static,
{
    let span_events = if span_timings {
        tracing_subscriber::fmt::format::FmtSpan::CLOSE
    } else {
        tracing_subscriber::fmt::format::FmtSpan::NONE
    };
    let max_level = filter.max_level();
    tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_target(true)
        .with_span_events(span_events)
        .compact()
        .with_filter(
            tracing_subscriber::filter::filter_fn(move |metadata| {
                filter.level_for_target(metadata.target()) >= *metadata.level()
            })
            .with_max_level_hint(max_level),
        )
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
#[tracing::instrument(
    name = "daemon.engine.logging.watcher_events",
    level = "trace",
    skip_all
)]
pub fn recent_watcher_events(
    profile: &ProfileRoot,
    max_lines: usize,
) -> HashMap<String, WatcherEvent> {
    let text = read_daemon_log_tail(profile, max_lines);
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
#[tracing::instrument(name = "daemon.engine.logging.read_tail", level = "trace", skip_all)]
fn read_daemon_log_tail(profile: &ProfileRoot, max_lines: usize) -> String {
    // macOS launchd: a plain err-log file next to the data dir.
    let err_log = profile.data_dir().join("daemon.err.log");
    if let Ok(contents) = std::fs::read_to_string(&err_log) {
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        return lines[start..].join("\n");
    }
    // Linux systemd: pull recent journal lines for the user unit.
    let output = std::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            &systemd_unit_name(profile),
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

pub fn unavailable_error(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    socket_path: &Path,
) -> TraceDecayError {
    TraceDecayError::project_route_with_detail(
        tracedecay_daemon_protocol::DAEMON_CONNECT_DOWN,
        true,
        tracedecay_daemon_control::unreachable_daemon_detail(profile, socket_path),
    )
}

#[cfg(test)]
mod stderr_tracing_tests {
    use tracing::level_filters::LevelFilter;

    use super::{StderrTracingDefault, StderrTracingFilter};

    fn parse(env_value: Option<&str>) -> StderrTracingFilter {
        StderrTracingFilter::parse(env_value, LevelFilter::WARN)
    }

    fn parse_for_hook(env_value: Option<&str>) -> StderrTracingFilter {
        StderrTracingFilter::parse(env_value, StderrTracingDefault::Silent.level())
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn log_one_span(span_timings: bool) -> String {
        use tracing_subscriber::layer::SubscriberExt as _;

        let captured = Captured::default();
        let writer = captured.clone();
        let layer =
            super::stderr_tracing_layer(parse(Some("tracedecay=debug")), span_timings, move || {
                writer.clone()
            });
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            let _span = tracing::debug_span!(target: "tracedecay::probe", "probe_span").entered();
        });
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn span_timings_log_busy_and_idle_on_close() {
        let output = log_one_span(true);
        assert!(output.contains("probe_span"), "{output}");
        assert!(output.contains("close"), "{output}");
        assert!(output.contains("time.busy="), "{output}");
        assert!(output.contains("time.idle="), "{output}");
    }

    #[test]
    fn spans_stay_silent_without_span_timings() {
        assert_eq!(log_one_span(false), "");
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
    fn unset_rust_log_defaults_maintenance_to_warn() {
        for value in [None, Some(""), Some("  ")] {
            let filter = parse(value);
            assert_eq!(filter.level_for_target("tracedecay"), LevelFilter::WARN);
            assert_eq!(
                filter.level_for_target("tracedecay_maintenance"),
                LevelFilter::WARN
            );
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
}

#[cfg(all(unix, test))]
mod watcher_log_tests {
    use super::parse_watcher_log_line;

    fn assert_absent(line: &str) {
        match parse_watcher_log_line(line) {
            None => {}
            Some(event) => panic!("line was accepted as a watcher event: {event:?}"),
        }
    }

    fn assert_event(line: &str, name: &str, project: Option<&str>, detail: Option<&str>) {
        let event = parse_watcher_log_line(line).expect("marked git_watch line");
        assert_eq!(event.event, name);
        assert_eq!(event.project.as_deref(), project);
        assert_eq!(event.detail.as_deref(), detail);
    }

    #[test]
    fn journal_prefixed_daemon_lines_yield_watcher_events() {
        assert_event(
            concat!(
                "Jul 28 03:00:00 host tracedecay[1234]: ",
                "[tracedecay] event=git_watch_degraded project=/tmp/project ",
                "reason=\"watch limit reached\""
            ),
            "git_watch_degraded",
            Some("/tmp/project"),
            Some("watch limit reached"),
        );
    }

    #[test]
    fn tracing_formatted_lines_cannot_forge_watcher_events() {
        assert_absent(concat!(
            "2026-07-28T03:00:00.000000Z  WARN tracedecay::daemon: ",
            "event=git_watch_started project=/tmp/project"
        ));
        assert_event(
            "[tracedecay] event=git_watch_started project=/tmp/project",
            "git_watch_started",
            Some("/tmp/project"),
            None,
        );
    }

    #[test]
    fn non_watcher_daemon_events_are_ignored() {
        assert_absent("[tracedecay] event=scheduler_task task=memory_curator outcome=start");
        assert_event(
            "[tracedecay] event=git_watch_degraded project=/srv/repo reason=\"watch limit reached\"",
            "git_watch_degraded",
            Some("/srv/repo"),
            Some("watch limit reached"),
        );
    }
}
