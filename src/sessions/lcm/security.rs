use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

const LARGE_TOOL_OUTPUT_CHARS: usize = 256 * 1024;
// Mirrors hermes-lcm `_GENERIC_BASE64_MIN_CHARS` (ingest_protection.py:103).
const GENERIC_BASE64_MIN_CHARS: usize = 4096;
const BINARYISH_SAMPLE_CHARS: usize = 8192;
const QUARANTINED_ASSISTANT_MIN_CHARS: usize = 65_536;
const QUARANTINED_ASSISTANT_MIN_TOKENS: usize = 1_000;
const QUARANTINE_HIGH_REPETITION: &str = "high_repetition";

#[derive(Default)]
pub(crate) struct CompiledPatternSet {
    regexes: Vec<Regex>,
}

impl CompiledPatternSet {
    fn is_match(&self, value: &str) -> bool {
        self.regexes.iter().any(|regex| regex.is_match(value))
    }
}

pub fn should_externalize(role: &str, kind: Option<&str>, content: &str) -> bool {
    prefers_whole_message_externalization(role, kind, content)
        || contains_data_uri(content)
        || has_long_base64_run(content)
}

/// Reasons that externalize the whole message body rather than only the
/// media/base64 spans inside it (quarantine, binary-ish content, oversized
/// tool output). Substring protection is skipped for these.
pub(crate) fn prefers_whole_message_externalization(
    role: &str,
    kind: Option<&str>,
    content: &str,
) -> bool {
    if quarantine_reason(role, kind, content).is_some() {
        return true;
    }
    if is_binaryish(content) {
        return true;
    }
    is_tool_payload(role, kind) && char_count_exceeds(content, LARGE_TOOL_OUTPUT_CHARS)
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
    if normalized.is_empty() || char_count_exceeds(&normalized, 256) {
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

pub fn ignore_message_reason<S: AsRef<str>>(
    _role: &str,
    content: &str,
    ignore_message_patterns: &[S],
) -> Option<&'static str> {
    let patterns = compile_message_patterns(ignore_message_patterns);
    ignore_message_reason_with_compiled(content, &patterns)
}

pub(crate) fn ignore_message_reason_with_compiled(
    content: &str,
    patterns: &CompiledPatternSet,
) -> Option<&'static str> {
    if content.is_empty() {
        return None;
    }
    patterns
        .is_match(content)
        .then_some("ignore_message_pattern")
}

pub fn matches_any_pattern<S: AsRef<str>>(patterns: &[S], value: &str) -> bool {
    let compiled = compile_session_patterns(patterns);
    matches_any_compiled_pattern(&compiled, value)
}

pub(crate) fn matches_any_compiled_pattern(patterns: &CompiledPatternSet, value: &str) -> bool {
    patterns.is_match(value)
}

pub(crate) fn compile_session_patterns<S: AsRef<str>>(patterns: &[S]) -> CompiledPatternSet {
    compile_patterns(patterns, |pattern| {
        Regex::new(&session_pattern_regex(pattern)).ok()
    })
}

pub(crate) fn compile_message_patterns<S: AsRef<str>>(patterns: &[S]) -> CompiledPatternSet {
    compile_patterns(patterns, |pattern| Regex::new(pattern).ok())
}

pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    Regex::new(&session_pattern_regex(pattern)).is_ok_and(|regex| regex.is_match(value))
}

fn session_pattern_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                regex.push_str(".*");
            } else {
                regex.push_str("[^:]*");
            }
        } else {
            regex.push_str(&regex::escape(&ch.to_string()));
        }
    }
    regex.push('$');
    regex
}

// Port of hermes-lcm `_DATA_URI_BASE64_RE` (ingest_protection.py:81-87): any
// data URI with a `;base64,` marker and at least 256 payload characters.
// Raw scans can see JSON-escaped slashes (`\/`, `\u002f`) before decoding.
static DATA_URI_BASE64_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    const SLASH: &str = r"(?:/|\\/|\\u002[fF])";
    Regex::new(&format!(
        r"(?i)data:(?:[A-Za-z0-9.+\-]|{SLASH})*(?:;[A-Za-z0-9_.+%\-]+=(?:[A-Za-z0-9_.+%\-]|{SLASH})*)*;base64,(?:[A-Za-z0-9+=]|{SLASH}){{256,}}"
    ))
    .ok()
});

pub fn contains_data_uri(content: &str) -> bool {
    DATA_URI_BASE64_RE.as_ref().is_some_and(|regex| {
        regex
            .find_iter(content)
            .any(|found| has_data_uri_boundary(content, found.end()))
    })
}

/// Byte spans of data-URI base64 payloads eligible for substring
/// externalization (Hermes `_protect_payload_substrings` pass 1).
pub(crate) fn data_uri_spans(content: &str) -> Vec<(usize, usize)> {
    DATA_URI_BASE64_RE.as_ref().map_or_else(Vec::new, |regex| {
        regex
            .find_iter(content)
            .filter_map(|found| {
                has_data_uri_boundary(content, found.end()).then_some((found.start(), found.end()))
            })
            .collect()
    })
}

fn has_data_uri_boundary(content: &str, end: usize) -> bool {
    match content.get(end..).and_then(|suffix| suffix.chars().next()) {
        None => true,
        Some(ch) => !is_data_uri_base64_char(ch),
    }
}

fn is_data_uri_base64_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=')
}

fn assistant_output_is_high_repetition(content: &str) -> bool {
    if !char_count_at_least(content, QUARANTINED_ASSISTANT_MIN_CHARS) {
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
        .filter(|segment| char_count_at_least(segment, 32))
        .collect()
}

fn distinct_char_count(text: &str) -> usize {
    text.chars()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn is_tool_payload(role: &str, kind: Option<&str>) -> bool {
    role.eq_ignore_ascii_case("tool")
        || kind.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value == "tool_result" || value == "tool_output"
        })
}

fn compile_patterns<S: AsRef<str>, F>(patterns: &[S], mut compile: F) -> CompiledPatternSet
where
    F: FnMut(&str) -> Option<Regex>,
{
    let mut regexes = Vec::new();
    for pattern in patterns {
        let pattern = pattern.as_ref().trim();
        if pattern.is_empty() {
            continue;
        }
        if let Some(regex) = compile(pattern) {
            regexes.push(regex);
        }
    }
    CompiledPatternSet { regexes }
}

fn char_count_at_least(content: &str, min_chars: usize) -> bool {
    if min_chars == 0 {
        return true;
    }
    content.chars().take(min_chars).count() == min_chars
}

fn char_count_exceeds(content: &str, max_chars: usize) -> bool {
    content.chars().nth(max_chars).is_some()
}

pub fn has_long_base64_run(content: &str) -> bool {
    !long_base64_run_spans(content).is_empty()
}

// Hermes `_BASE64_RUN_RE` alphabet (ingest_protection.py:89): standard and
// url-safe base64 plus padding.
fn is_base64_run_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

/// Byte spans of maximal base64-alphabet runs that qualify as long base64
/// payloads (Hermes `_BASE64_RUN_RE` + `looks_like_long_base64`).
pub(crate) fn long_base64_run_spans(content: &str) -> Vec<(usize, usize)> {
    if content.len() < GENERIC_BASE64_MIN_CHARS {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut run_start: Option<usize> = None;
    for (idx, byte) in content.bytes().enumerate() {
        if is_base64_run_byte(byte) {
            if run_start.is_none() {
                run_start = Some(idx);
            }
        } else if let Some(start) = run_start.take() {
            if looks_like_long_base64(&content[start..idx]) {
                spans.push((start, idx));
            }
        }
    }
    if let Some(start) = run_start {
        if looks_like_long_base64(&content[start..]) {
            spans.push((start, content.len()));
        }
    }
    spans
}

// Port of hermes-lcm `looks_like_long_base64` (ingest_protection.py:525-547)
// for a maximal alphabet run: very long, length mod 4 != 1, and at least a
// bit of mixed alphabet so a repeated-character log line does not match.
fn looks_like_long_base64(run: &str) -> bool {
    if run.len() < GENERIC_BASE64_MIN_CHARS || run.len() % 4 == 1 {
        return false;
    }
    let mut seen = [false; 256];
    let mut distinct = 0usize;
    for byte in run.trim_end_matches('=').bytes() {
        if !seen[byte as usize] {
            seen[byte as usize] = true;
            distinct += 1;
            if distinct >= 8 {
                return true;
            }
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
