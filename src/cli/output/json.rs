//! Canonical single-frame JSON output for CLI adapters.

use serde::Serialize;

/// Serializes one canonical value as exactly one UTF-8 JSON line.
pub fn json_line<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut rendered = serde_json::to_string(value)?;
    rendered.push('\n');
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::json_line;

    #[test]
    fn emits_one_json_line() {
        assert_eq!(
            json_line(&serde_json::json!({"ok": true})).unwrap(),
            "{\"ok\":true}\n"
        );
    }
}
