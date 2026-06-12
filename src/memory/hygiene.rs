//! Deterministic memory-hygiene rules: secret-like content detection and
//! transient run-output detection.
//!
//! These are conservative, rule-based checks — no model is ever invoked from
//! Rust. Standalone tokensave only *rejects* secret-like writes and *proposes*
//! hygiene deletions in the curation dry-run plan; any LLM review of those
//! proposals lives exclusively in the Hermes wrapper layer (capabilities keep
//! reporting `llm_curation: false` here).

use std::sync::OnceLock;

use regex::Regex;

fn regex_set() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (
                // PEM-encoded private key blocks.
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----",
                "PEM private-key block",
            ),
            (
                // Bearer tokens with a long opaque value.
                r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}",
                "bearer token",
            ),
            (
                // Well-known credential prefixes (OpenAI, GitHub, Slack, AWS,
                // GitLab). Each requires its full opaque tail so prose like
                // "sk-something" cannot match.
                r"\b(sk-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|glpat-[A-Za-z0-9_-]{20,})\b",
                "known credential prefix",
            ),
            (
                // key=value / key: value where the key is credential-ish and
                // the value is a long unbroken token.
                r#"(?i)\b(api[_-]?key|secret|token|passwd|password|credential|private[_-]?key|access[_-]?key)\b\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{16,}"#,
                "credential-like key=value assignment",
            ),
        ]
        .into_iter()
        // Patterns are compile-time literals; a failed compile would only
        // drop that rule (and is covered by the unit tests).
        .filter_map(|(pattern, reason)| Regex::new(pattern).ok().map(|regex| (regex, reason)))
        .collect()
    })
}

/// Shannon entropy of a token in bits per character.
fn shannon_entropy(token: &str) -> f64 {
    let len = token.chars().count();
    if len == 0 {
        return 0.0;
    }
    let mut counts: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    for ch in token.chars() {
        *counts.entry(ch).or_insert(0) += 1;
    }
    counts
        .values()
        .map(|&count| {
            let p = count as f64 / len as f64;
            -p * p.log2()
        })
        .sum()
}

fn is_hex_only(token: &str) -> bool {
    token.chars().all(|c| c.is_ascii_hexdigit())
}

/// True for a single long, high-entropy, base64-ish token. Tuned to stay
/// quiet on legitimate fact content: git SHAs and other hex digests are
/// explicitly excluded (hex tops out at 4 bits/char), and the length floor
/// keeps ordinary identifiers and URLs from qualifying.
fn looks_high_entropy_token(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.chars().count() < 36 || is_hex_only(trimmed) {
        return false;
    }
    let has_alpha = trimmed.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = trimmed.chars().any(|c| c.is_ascii_digit());
    if !has_alpha || !has_digit {
        return false;
    }
    // Only token-charset candidates (base64/url-safe); anything with other
    // punctuation is treated as prose or a path, not a secret blob.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
    {
        return false;
    }
    shannon_entropy(trimmed) >= 4.2
}

/// Conservative secret-likeness check. Returns a short reason when `content`
/// matches a credential pattern, or `None` when it looks safe to store.
pub fn detect_secret_like(content: &str) -> Option<String> {
    for (regex, reason) in regex_set() {
        if regex.is_match(content) {
            return Some((*reason).to_string());
        }
    }
    for token in content.split_whitespace() {
        if looks_high_entropy_token(token) {
            return Some("high-entropy token".to_string());
        }
    }
    None
}

fn transient_regexes() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (
                r"(?i)\b(localhost|127\.0\.0\.1|0\.0\.0\.0):\d{2,5}\b",
                "ephemeral local port",
            ),
            (r"(?i)\bpid\s*[:=#]?\s*\d{2,}\b", "process id"),
            (r"/tmp/[A-Za-z0-9._-]+", "one-off /tmp path"),
            (
                r"(?i)\b(listening on|started in \d+\s*ms|exit code \d+|finished in \d+(\.\d+)?s)\b",
                "run-log output",
            ),
        ]
        .into_iter()
        // Patterns are compile-time literals; a failed compile would only
        // drop that rule (and is covered by the unit tests).
        .filter_map(|(pattern, reason)| Regex::new(pattern).ok().map(|regex| (regex, reason)))
        .collect()
    })
}

/// Flags facts that look like ephemeral run output (ports, PIDs, one-off
/// /tmp paths, run-log lines) rather than durable knowledge. Used ONLY by the
/// curation planner to mark prune CANDIDATES — never to reject or delete
/// anything on its own.
pub fn detect_transient(content: &str) -> Option<String> {
    let mut reasons: Vec<&str> = Vec::new();
    for (regex, reason) in transient_regexes() {
        if regex.is_match(content) && !reasons.contains(reason) {
            reasons.push(reason);
        }
    }
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pem_blocks_and_bearer_tokens() {
        assert!(detect_secret_like("-----BEGIN RSA PRIVATE KEY-----\nMII...").is_some());
        assert!(detect_secret_like("-----BEGIN OPENSSH PRIVATE KEY-----").is_some());
        assert!(
            detect_secret_like("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9")
                .is_some()
        );
    }

    #[test]
    fn detects_known_prefixes_and_credentialish_assignments() {
        assert!(detect_secret_like("sk-proj1234567890abcdefghijklmn").is_some());
        assert!(detect_secret_like("ghp_abcdefghijklmnopqrstuvwxyz0123456789").is_some());
        assert!(detect_secret_like("AKIAIOSFODNN7EXAMPLE is the access key").is_some());
        assert!(detect_secret_like("api_key=Zx9mQ4tR7wLp2NvK8sBd1FgH").is_some());
        assert!(detect_secret_like("password: hunter2hunter2hunter2").is_some());
    }

    #[test]
    fn detects_high_entropy_blobs_but_not_git_shas() {
        assert!(detect_secret_like(
            "value Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE"
        )
        .is_some());
        // 40-char git SHA: hex-only, must NOT be flagged.
        assert!(detect_secret_like("commit 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5").is_none());
    }

    #[test]
    fn stays_quiet_on_ordinary_facts() {
        assert!(detect_secret_like("Use pnpm rather than npm for installs in this repo").is_none());
        assert!(
            detect_secret_like("The token budget for LCM expansion defaults to 4000").is_none()
        );
        assert!(detect_secret_like("secret sauce of the planner is union-find").is_none());
        assert!(detect_secret_like("CamelCaseIdentifiersAreFineEvenWhenLong").is_none());
    }

    #[test]
    fn transient_detection_flags_run_output() {
        assert!(detect_transient("dashboard listening on http://127.0.0.1:43817").is_some());
        assert!(detect_transient("server started with pid 48213").is_some());
        assert!(detect_transient("wrote scratch file /tmp/tokensave-aborted.json").is_some());
        assert!(detect_transient("build finished in 12.4s with exit code 0").is_some());
    }

    #[test]
    fn transient_detection_ignores_durable_facts() {
        assert!(detect_transient("The dashboard binds 127.0.0.1 with an ephemeral port").is_none());
        assert!(detect_transient("Curation hard-deletes losers; there is no archive").is_none());
    }
}
