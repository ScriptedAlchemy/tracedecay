//! Daemon-compatible stderr event lines for the git watcher.

pub(crate) fn log_daemon_event(event: &str, fields: &[(&str, String)]) {
    eprintln!("{}", format_daemon_log_line(event, fields));
}

fn format_daemon_log_line(event: &str, fields: &[(&str, String)]) -> String {
    let mut line = event.to_owned();
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&escape_field(value));
    }
    line
}

fn escape_field(value: &str) -> String {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
    }) {
        return value.to_owned();
    }
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            ch => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}
