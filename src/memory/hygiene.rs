//! Deterministic memory-hygiene rules: secret-like content detection and
//! transient run-output detection.
//!
//! These are conservative, rule-based checks — no model is ever invoked from
//! Rust. Standalone tracedecay only *rejects* secret-like writes and *proposes*
//! hygiene deletions in the curation dry-run plan; any LLM review of those
//! proposals lives exclusively in the Hermes wrapper layer (capabilities keep
//! reporting `llm_curation: false` here).

use std::sync::OnceLock;

use regex::Regex;

use crate::privacy::detector_kernel::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile,
    compile_credential_patterns_lossy, looks_high_entropy_token,
};

fn compile_patterns(patterns: &[(&'static str, &'static str)]) -> Vec<(Regex, &'static str)> {
    patterns
        .iter()
        // Patterns are compile-time literals; a failed compile would only
        // drop that rule (and is covered by the unit tests).
        .filter_map(|(pattern, reason)| Regex::new(pattern).ok().map(|regex| (regex, *reason)))
        .collect()
}

fn regex_set() -> &'static [CredentialPattern] {
    static PATTERNS: OnceLock<Vec<CredentialPattern>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| compile_credential_patterns_lossy(CredentialPatternProfile::Memory))
        .as_slice()
}

fn credential_reason(kind: CredentialPatternKind) -> &'static str {
    match kind {
        CredentialPatternKind::PrivateKey => "PEM private-key block",
        CredentialPatternKind::BearerToken => "bearer token",
        CredentialPatternKind::KnownCredential => "known credential prefix",
        CredentialPatternKind::CredentialAssignment => "credential-like key=value assignment",
    }
}

/// Conservative secret-likeness check. Returns a short reason when `content`
/// matches a credential pattern, or `None` when it looks safe to store.
pub fn detect_secret_like(content: &str) -> Option<String> {
    for pattern in regex_set() {
        if pattern.regex().is_match(content) {
            return Some(credential_reason(pattern.kind()).to_string());
        }
    }
    for token in content.split_whitespace() {
        let trimmed = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());
        if looks_high_entropy_token(trimmed) {
            return Some("high-entropy token".to_string());
        }
    }
    None
}

fn transient_regexes() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        compile_patterns(&[
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
        ])
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
        assert!(
            detect_secret_like(concat!(
                "-----BEGIN ",
                "PRIVATE KEY-----\nNOT-A-VALID-PRIVATE-KEY"
            ))
            .is_some()
        );
        assert!(detect_secret_like("-----BEGIN OPENSSH PRIVATE KEY-----").is_some());
        assert!(
            detect_secret_like("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9")
                .is_some()
        );
    }

    #[test]
    fn detects_known_prefixes_and_credentialish_assignments() {
        assert!(detect_secret_like("sk-proj1234567890abcdefghijklmn").is_some());
        assert!(detect_secret_like("Deploys used sk-test-742913 before rotation").is_some());
        assert!(detect_secret_like("ghp_abcdefghijklmnopqrstuvwxyz0123456789").is_some());
        assert!(detect_secret_like("AKIAIOSFODNN7EXAMPLE is the access key").is_some());
        assert!(detect_secret_like(concat!("api_", "key=", "0000000000000000")).is_some());
        assert!(detect_secret_like("password: hunter2hunter2hunter2").is_some());
    }

    #[test]
    fn detects_high_entropy_blobs_but_not_git_shas() {
        assert!(
            detect_secret_like(
                "value Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE"
            )
            .is_some()
        );
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
        assert!(detect_secret_like("Use the sk-test fixture profile for dry runs").is_none());
        assert!(detect_secret_like("CamelCaseIdentifiersAreFineEvenWhenLong").is_none());
    }

    #[test]
    fn transient_detection_flags_run_output() {
        assert!(detect_transient("dashboard listening on http://127.0.0.1:43817").is_some());
        assert!(detect_transient("server started with pid 48213").is_some());
        assert!(detect_transient("wrote scratch file /tmp/tracedecay-aborted.json").is_some());
        assert!(detect_transient("build finished in 12.4s with exit code 0").is_some());
    }

    #[test]
    fn transient_detection_ignores_durable_facts() {
        assert!(detect_transient("The dashboard binds 127.0.0.1 with an ephemeral port").is_none());
        assert!(detect_transient("Curation hard-deletes losers; there is no archive").is_none());
    }
}
