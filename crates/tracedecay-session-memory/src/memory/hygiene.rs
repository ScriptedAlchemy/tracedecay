//! Deterministic memory-hygiene rules: secret-like content detection and
//! transient run-output detection.
//!
//! These are conservative, rule-based checks, no model is ever invoked from
//! Rust. Standalone tracedecay only *rejects* secret-like writes and *proposes*
//! hygiene deletions in the curation dry-run plan; any LLM review of those
//! proposals lives exclusively in the Hermes wrapper layer (capabilities keep
//! reporting `llm_curation: false` here).

use std::sync::OnceLock;

use regex::Regex;

use tracedecay_privacy::detector_kernel::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile, CredentialRuleSetError,
    compile_credential_patterns, looks_high_entropy_token,
};

fn compile_patterns(
    patterns: &[(&'static str, &'static str)],
) -> Result<Vec<(Regex, &'static str)>, regex::Error> {
    patterns
        .iter()
        .map(|(pattern, reason)| Regex::new(pattern).map(|regex| (regex, *reason)))
        .collect()
}

fn regex_set() -> Result<&'static [CredentialPattern], &'static CredentialRuleSetError> {
    static PATTERNS: OnceLock<Result<Vec<CredentialPattern>, CredentialRuleSetError>> =
        OnceLock::new();
    PATTERNS
        .get_or_init(|| compile_credential_patterns(CredentialPatternProfile::Memory))
        .as_deref()
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
    let Ok(patterns) = regex_set() else {
        return Some("credential detector unavailable".to_string());
    };
    for pattern in patterns {
        match pattern.is_match(content) {
            Ok(true) => return Some(credential_reason(pattern.kind()).to_string()),
            Ok(false) => {}
            Err(_) => return Some("credential detector unavailable".to_string()),
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

fn transient_regexes() -> Result<&'static [(Regex, &'static str)], &'static regex::Error> {
    static PATTERNS: OnceLock<Result<Vec<(Regex, &'static str)>, regex::Error>> = OnceLock::new();
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
    .as_deref()
}

/// Flags facts that look like ephemeral run output (ports, PIDs, one-off
/// /tmp paths, run-log lines) rather than durable knowledge. Used ONLY by the
/// curation planner to mark prune CANDIDATES, never to reject or delete
/// anything on its own.
pub fn detect_transient(content: &str) -> Option<String> {
    let Ok(patterns) = transient_regexes() else {
        return Some("transient detector unavailable".to_string());
    };
    let mut reasons: Vec<&str> = Vec::new();
    for (regex, reason) in patterns {
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

    fn secret_reason(content: &str) -> Option<&'static str> {
        detect_secret_like(content).map(|reason| &*reason.leak())
    }

    #[test]
    fn detects_pem_blocks_and_bearer_tokens() {
        assert_eq!(
            secret_reason(concat!(
                "-----BEGIN ",
                "PRIVATE KEY-----\nNOT-A-VALID-PRIVATE-KEY"
            )),
            Some("PEM private-key block")
        );
        assert_eq!(
            secret_reason("-----BEGIN OPENSSH PRIVATE KEY-----"),
            Some("PEM private-key block")
        );
        assert_eq!(
            secret_reason("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            Some("bearer token")
        );
    }

    #[test]
    fn detects_known_prefixes_and_credentialish_assignments() {
        for content in [
            "sk-proj1234567890abcdefghijklmn",
            "Deploys used sk-test-742913 before rotation",
            "ghp_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ",
            "AKIA4S27TQXBVCZ5MJ6L is the access key",
        ] {
            assert_eq!(
                secret_reason(content),
                Some("known credential prefix"),
                "{content}"
            );
        }
        for content in [
            concat!("api_", "key=", "0000000000000000"),
            "password: hunter2hunter2hunter2",
        ] {
            assert_eq!(
                secret_reason(content),
                Some("credential-like key=value assignment"),
                "{content}"
            );
        }
    }

    #[test]
    fn detects_high_entropy_blobs_but_not_git_shas() {
        assert_eq!(
            secret_reason("value Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE"),
            Some("high-entropy token")
        );
        // 40-char git SHA: hex-only, must NOT be flagged.
        assert_eq!(
            secret_reason("commit 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5"),
            None
        );
    }

    #[test]
    fn stays_quiet_on_ordinary_facts() {
        for content in [
            "Use pnpm rather than npm for installs in this repo",
            "The token budget for LCM expansion defaults to 4000",
            "secret sauce of the planner is union-find",
            "Use the sk-test fixture profile for dry runs",
            "CamelCaseIdentifiersAreFineEvenWhenLong",
        ] {
            assert_eq!(secret_reason(content), None, "{content}");
        }
        assert_eq!(
            secret_reason("Use the sk-test-742913 fixture profile for dry runs"),
            Some("known credential prefix")
        );
    }

    #[test]
    fn pattern_compilation_errors_are_not_dropped() {
        assert!(matches!(
            compile_patterns(&[("(", "invalid fixture")]),
            Err(regex::Error::Syntax(_))
        ));
        let compiled = compile_patterns(&[("a+", "valid fixture")]).unwrap();
        assert_eq!(
            compiled
                .iter()
                .map(|(regex, reason)| (regex.as_str(), *reason))
                .collect::<Vec<_>>(),
            vec![("a+", "valid fixture")]
        );
    }

    #[test]
    fn transient_detection_flags_run_output() {
        assert_eq!(
            detect_transient("dashboard listening on http://127.0.0.1:43817").as_deref(),
            Some("ephemeral local port, run-log output")
        );
        assert_eq!(
            detect_transient("server started with pid 48213").as_deref(),
            Some("process id")
        );
        assert_eq!(
            detect_transient("wrote scratch file /tmp/tracedecay-aborted.json").as_deref(),
            Some("one-off /tmp path")
        );
        assert_eq!(
            detect_transient("build finished in 12.4s with exit code 0").as_deref(),
            Some("run-log output")
        );
    }

    #[test]
    fn transient_detection_ignores_durable_facts() {
        assert_eq!(
            detect_transient("The dashboard binds 127.0.0.1 with an ephemeral port"),
            None
        );
        assert_eq!(
            detect_transient("Curation hard-deletes losers; there is no archive"),
            None
        );
        assert_eq!(
            detect_transient("The dashboard binds 127.0.0.1:43817 with an ephemeral port")
                .as_deref(),
            Some("ephemeral local port")
        );
    }
}
