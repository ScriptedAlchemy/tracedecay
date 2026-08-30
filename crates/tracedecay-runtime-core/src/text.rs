//! Small text-handling utilities shared across crates.

/// Returns the longest prefix of `s` whose byte length does not exceed
/// `max_bytes`, snapping back to the nearest preceding UTF-8 char boundary
/// when the budget lands inside a multi-byte character.
///
/// This is the safe replacement for `&s[..max_bytes]` when `s` may contain
/// non-ASCII text and the caller has a byte budget rather than a char budget.
pub fn utf8_prefix_at_or_before(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }

    let mut end = max_bytes;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

/// Formats a token count as a compact string (e.g. "1.2M", "45.3k").
pub fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Formats a UNIX timestamp as a human-readable relative time (e.g. "2m ago", "3d ago").
/// Returns "never" when the timestamp is 0.
pub fn format_relative_time(timestamp: u64) -> String {
    if timestamp == 0 {
        return "never".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let delta = now.saturating_sub(timestamp);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86400)
    }
}

/// Formats a byte count into a human-readable string (e.g. "798.0 MB").
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Formats a number with comma separators (e.g. 243302 -> "243,302").
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::utf8_prefix_at_or_before;

    #[test]
    fn returns_whole_string_when_under_budget() {
        assert_eq!(utf8_prefix_at_or_before("hello", 10), "hello");
    }

    #[test]
    fn returns_whole_string_when_at_budget() {
        assert_eq!(utf8_prefix_at_or_before("hello", 5), "hello");
    }

    #[test]
    fn truncates_ascii_at_budget() {
        assert_eq!(utf8_prefix_at_or_before("abcdef", 3), "abc");
    }

    #[test]
    fn walks_back_when_cut_lands_inside_multibyte_char() {
        // "é" is 2 bytes (0xC3 0xA9). With 20 'a's the total is 22 bytes;
        // a budget of 21 lands inside "é" and must walk back to 20.
        let s = format!("{}é", "a".repeat(20));
        assert_eq!(utf8_prefix_at_or_before(&s, 21), "a".repeat(20));
    }

    #[test]
    fn returns_empty_when_budget_lands_inside_leading_multibyte() {
        // 4-byte emoji at position 0; any budget < 4 (but > 0) walks back to 0.
        let s = "🦀tail";
        assert_eq!(utf8_prefix_at_or_before(s, 2), "");
    }

    #[test]
    fn handles_empty_string() {
        assert_eq!(utf8_prefix_at_or_before("", 10), "");
        assert_eq!(utf8_prefix_at_or_before("", 0), "");
    }

    #[test]
    fn handles_zero_budget() {
        assert_eq!(utf8_prefix_at_or_before("abc", 0), "");
    }
}
