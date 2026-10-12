//! Deterministic memory-hygiene rules: secret-like content detection.
//!
//! These are conservative, rule-based checks, no model is ever invoked from
//! Rust. Standalone tracedecay only *rejects* secret-like writes and *proposes*
//! hygiene deletions in the curation dry-run plan; any LLM review of those
//! proposals lives exclusively in the Hermes wrapper layer (capabilities keep
//! reporting `llm_curation: false` here).

use std::sync::OnceLock;

use tracedecay_privacy::detector_kernel::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile, CredentialRuleSetError,
    compile_credential_patterns, looks_high_entropy_token,
};

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
}
