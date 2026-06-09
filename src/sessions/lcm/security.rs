use std::collections::HashMap;

const LARGE_TOOL_OUTPUT_CHARS: usize = 256 * 1024;
const LONG_BASE64_RUN_CHARS: usize = 64 * 1024;
const BINARYISH_SAMPLE_CHARS: usize = 8192;
const QUARANTINED_ASSISTANT_MIN_CHARS: usize = 65_536;
const QUARANTINED_ASSISTANT_MIN_TOKENS: usize = 1_000;
const QUARANTINE_HIGH_REPETITION: &str = "high_repetition";

pub fn should_externalize(role: &str, kind: Option<&str>, content: &str) -> bool {
    if quarantine_reason(role, kind, content).is_some() {
        return true;
    }
    if contains_data_uri(content) {
        return true;
    }
    if has_long_base64_run(content) {
        return true;
    }
    if is_binaryish(content) {
        return true;
    }
    is_tool_payload(role, kind) && content.chars().count() > LARGE_TOOL_OUTPUT_CHARS
}

pub fn contains_media_payload(content: &str) -> bool {
    contains_data_uri(content) || has_long_base64_run(content)
}

pub fn quarantine_reason(role: &str, _kind: Option<&str>, content: &str) -> Option<&'static str> {
    if !role.eq_ignore_ascii_case("assistant") {
        return None;
    }
    if assistant_output_is_high_repetition(content) {
        Some(QUARANTINE_HIGH_REPETITION)
    } else {
        None
    }
}

pub fn heartbeat_noise_reason(role: &str, content: &str) -> Option<&'static str> {
    if !matches!(
        role.to_ascii_lowercase().as_str(),
        "assistant" | "tool" | "system"
    ) {
        return None;
    }
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || normalized.chars().count() > 256 {
        return None;
    }
    let lower = normalized
        .trim_matches(|ch: char| matches!(ch, '.' | '!' | '…' | '-' | ' '))
        .to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "still working"
            | "working on it"
            | "processing"
            | "checking"
            | "one moment"
            | "ping"
            | "heartbeat"
            | "no update"
    )
    .then_some("heartbeat_progress")
}

pub fn contains_data_uri(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    for (idx, _) in lower.match_indices("data:") {
        let after = &lower[idx + "data:".len()..];
        let mut saw_comma = false;
        for ch in after.chars().take(256) {
            if ch == ',' {
                saw_comma = true;
                break;
            }
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>') {
                break;
            }
        }
        if saw_comma {
            return true;
        }
    }
    false
}

fn assistant_output_is_high_repetition(content: &str) -> bool {
    if content.chars().count() < QUARANTINED_ASSISTANT_MIN_CHARS {
        return false;
    }

    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let tokens = word_tokens(&normalized);
    if tokens.len() < QUARANTINED_ASSISTANT_MIN_TOKENS {
        return tokens.len() >= 20
            && normalized
                .chars()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                <= 12;
    }

    let mut counts = HashMap::<&str, usize>::new();
    for token in &tokens {
        *counts.entry(token.as_str()).or_default() += 1;
    }
    let unique_token_ratio = counts.len() as f64 / tokens.len().max(1) as f64;
    let top_token_ratio = counts.values().copied().max().unwrap_or(0) as f64 / tokens.len() as f64;

    let segments = repetition_segments(content);
    let mut top_segment_ratio = 0.0;
    let mut duplicate_segment_ratio = 0.0;
    if segments.len() >= 20 {
        let mut segment_counts = HashMap::<&str, usize>::new();
        for segment in &segments {
            *segment_counts.entry(segment.as_str()).or_default() += 1;
        }
        top_segment_ratio =
            segment_counts.values().copied().max().unwrap_or(0) as f64 / segments.len() as f64;
        duplicate_segment_ratio = 1.0 - (segment_counts.len() as f64 / segments.len() as f64);
    }

    (unique_token_ratio <= 0.03
        && (top_segment_ratio >= 0.10
            || duplicate_segment_ratio >= 0.50
            || top_token_ratio >= 0.08))
        || (unique_token_ratio <= 0.015 && distinct_char_count(&normalized) <= 64)
}

fn word_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn repetition_segments(text: &str) -> Vec<String> {
    text.split(['\n', '.', '!', '?'])
        .map(|segment| segment.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|segment| segment.to_ascii_lowercase())
        .filter(|segment| segment.chars().count() >= 32)
        .collect()
}

fn distinct_char_count(text: &str) -> usize {
    text.chars()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn is_tool_payload(role: &str, kind: Option<&str>) -> bool {
    role.eq_ignore_ascii_case("tool")
        || kind
            .map(|value| {
                let value = value.to_ascii_lowercase();
                value == "tool_result" || value == "tool_output"
            })
            .unwrap_or(false)
}

pub fn has_long_base64_run(content: &str) -> bool {
    let mut run = 0usize;
    let mut distinct = [false; 256];
    let mut distinct_count = 0usize;
    let mut has_symbol = false;
    let mut has_upper = false;
    let mut has_lower = false;
    let mut has_digit = false;
    for byte in content.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=') {
            run += 1;
            if !distinct[byte as usize] {
                distinct[byte as usize] = true;
                distinct_count += 1;
            }
            has_symbol |= matches!(byte, b'+' | b'/' | b'=');
            has_upper |= byte.is_ascii_uppercase();
            has_lower |= byte.is_ascii_lowercase();
            has_digit |= byte.is_ascii_digit();
            if run >= LONG_BASE64_RUN_CHARS {
                let categories =
                    usize::from(has_upper) + usize::from(has_lower) + usize::from(has_digit);
                return has_symbol || (categories >= 2 && distinct_count >= 8);
            }
        } else {
            run = 0;
            distinct = [false; 256];
            distinct_count = 0;
            has_symbol = false;
            has_upper = false;
            has_lower = false;
            has_digit = false;
        }
    }
    false
}

fn is_binaryish(content: &str) -> bool {
    let sample = content.chars().take(BINARYISH_SAMPLE_CHARS);
    let mut total = 0usize;
    let mut control = 0usize;
    for ch in sample {
        total += 1;
        if ch == '\0' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) {
            control += 1;
        }
    }
    total >= 1024 && control * 10 > total
}
