use std::ops::Range;

use regex::Regex;
use serde_json::Value;

const KNOWN_CREDENTIAL_PATTERN: &str = r"\b(?:sk-[A-Za-z0-9_-]{20,}|sk-test-[0-9]{6,}|ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|glpat-[A-Za-z0-9_-]{20,})\b";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialPatternKind {
    PrivateKey,
    BearerToken,
    KnownCredential,
    CredentialAssignment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialPatternProfile {
    Observation,
    Memory,
}

pub(crate) struct CredentialPattern {
    kind: CredentialPatternKind,
    regex: Regex,
}

impl CredentialPattern {
    pub(crate) fn kind(&self) -> CredentialPatternKind {
        self.kind
    }

    pub(crate) fn regex(&self) -> &Regex {
        &self.regex
    }
}

pub(crate) fn compile_credential_patterns(
    profile: CredentialPatternProfile,
) -> Result<Vec<CredentialPattern>, regex::Error> {
    pattern_specs(profile)
        .iter()
        .map(|&(kind, pattern)| compile_pattern(kind, pattern))
        .collect()
}

pub(crate) fn compile_credential_patterns_lossy(
    profile: CredentialPatternProfile,
) -> Vec<CredentialPattern> {
    pattern_specs(profile)
        .iter()
        .filter_map(|&(kind, pattern)| compile_pattern(kind, pattern).ok())
        .collect()
}

fn compile_pattern(
    kind: CredentialPatternKind,
    pattern: &str,
) -> Result<CredentialPattern, regex::Error> {
    Ok(CredentialPattern {
        kind,
        regex: Regex::new(pattern)?,
    })
}

fn pattern_specs(
    profile: CredentialPatternProfile,
) -> &'static [(CredentialPatternKind, &'static str)] {
    use CredentialPatternKind::{BearerToken, CredentialAssignment, KnownCredential, PrivateKey};

    match profile {
        CredentialPatternProfile::Observation => &[
            (
                PrivateKey,
                r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|$)",
            ),
            (BearerToken, r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}"),
            (KnownCredential, KNOWN_CREDENTIAL_PATTERN),
            (
                CredentialAssignment,
                r#"(?i)\b(?:api[_ -]?key|secret|token|passwd|password|credential|private[_ -]?key|access[_ -]?key)\b\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{6,}"#,
            ),
        ],
        CredentialPatternProfile::Memory => &[
            (
                PrivateKey,
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----",
            ),
            (BearerToken, r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}"),
            (KnownCredential, KNOWN_CREDENTIAL_PATTERN),
            (
                CredentialAssignment,
                r#"(?i)\b(?:api[_-]?key|secret|token|passwd|password|credential|private[_-]?key|access[_-]?key)\b\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{16,}"#,
            ),
        ],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedSensitiveKey {
    ascii_compact: String,
    separated: String,
    compact: String,
}

impl NormalizedSensitiveKey {
    pub(crate) fn new(key: &str) -> Self {
        let ascii_compact = key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|character| character.to_ascii_lowercase())
            .collect();

        let mut separated = String::with_capacity(key.len());
        for character in key.to_lowercase().chars() {
            if character.is_ascii_alphanumeric() {
                separated.push(character);
            } else if !separated.ends_with('_') {
                separated.push('_');
            }
        }
        let separated = separated.trim_matches('_').to_string();
        let compact = separated.replace('_', "");

        Self {
            ascii_compact,
            separated,
            compact,
        }
    }

    pub(crate) fn ascii_compact(&self) -> &str {
        &self.ascii_compact
    }

    pub(crate) fn separated(&self) -> &str {
        &self.separated
    }

    pub(crate) fn compact(&self) -> &str {
        &self.compact
    }
}

pub(crate) trait SensitiveKeyPolicy {
    type Match: Copy;

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JsonPathSegment {
    Field(usize),
    Index(usize),
}

pub(crate) fn redact_sensitive_json_values<P, R>(
    value: &mut Value,
    policy: &P,
    mut redact: R,
) -> bool
where
    P: SensitiveKeyPolicy,
    R: FnMut(&mut Value, P::Match, &[JsonPathSegment]) -> bool,
{
    fn walk<P, R>(
        value: &mut Value,
        policy: &P,
        path: &mut Vec<JsonPathSegment>,
        redact: &mut R,
    ) -> bool
    where
        P: SensitiveKeyPolicy,
        R: FnMut(&mut Value, P::Match, &[JsonPathSegment]) -> bool,
    {
        match value {
            Value::Object(fields) => {
                let mut changed = false;
                for (index, (key, child)) in fields.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    let normalized = NormalizedSensitiveKey::new(key);
                    let redacted = policy
                        .classify(&normalized)
                        .is_some_and(|matched| redact(child, matched, path));
                    changed |= redacted || walk(child, policy, path, redact);
                    path.pop();
                }
                changed
            }
            Value::Array(items) => {
                let mut changed = false;
                for (index, child) in items.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    changed |= walk(child, policy, path, redact);
                    path.pop();
                }
                changed
            }
            _ => false,
        }
    }

    walk(value, policy, &mut Vec::new(), &mut redact)
}

pub(crate) fn visit_json_strings_mut(
    value: &mut Value,
    mut visit: impl FnMut(&mut String, &[JsonPathSegment]),
) {
    fn walk(
        value: &mut Value,
        path: &mut Vec<JsonPathSegment>,
        visit: &mut impl FnMut(&mut String, &[JsonPathSegment]),
    ) {
        match value {
            Value::Object(fields) => {
                for (index, child) in fields.values_mut().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    walk(child, path, visit);
                    path.pop();
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    walk(child, path, visit);
                    path.pop();
                }
            }
            Value::String(text) => visit(text, path),
            _ => {}
        }
    }

    walk(value, &mut Vec::new(), &mut visit);
}

pub(crate) fn high_entropy_ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        if !token_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len() && token_byte(bytes[end]) {
            end += 1;
        }
        if looks_high_entropy_token(&text[start..end]) {
            ranges.push(start..end);
        }
        start = end;
    }
    ranges
}

pub(crate) fn looks_high_entropy_token(token: &str) -> bool {
    if token.len() < 36
        || !token.bytes().all(token_byte)
        || token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    if !token.bytes().any(|byte| byte.is_ascii_alphabetic())
        || !token.bytes().any(|byte| byte.is_ascii_digit())
    {
        return false;
    }

    let mut counts = [0usize; 256];
    for byte in token.bytes() {
        counts[byte as usize] += 1;
    }
    let len = token.len() as f64;
    counts
        .into_iter()
        .filter(|count| *count != 0)
        .map(|count| {
            let probability = count as f64 / len;
            -probability * probability.log2()
        })
        .sum::<f64>()
        >= 4.2
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    struct KeySet(BTreeSet<String>);

    impl SensitiveKeyPolicy for KeySet {
        type Match = ();

        fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match> {
            self.0.contains(key.ascii_compact()).then_some(())
        }
    }

    #[test]
    fn normalizes_keys_once_for_policy_adapters() {
        let key = NormalizedSensitiveKey::new(" Client--Secret ");
        assert_eq!(key.ascii_compact(), "clientsecret");
        assert_eq!(key.separated(), "client_secret");
        assert_eq!(key.compact(), "clientsecret");
    }

    #[test]
    fn recursively_redacts_structured_sensitive_values() {
        let policy = KeySet(BTreeSet::from(["token".to_string()]));
        let mut value = json!({"outer": [{"token": "hidden"}], "safe": "kept"});
        let changed = redact_sensitive_json_values(&mut value, &policy, |child, (), path| {
            assert_eq!(
                path,
                &[
                    JsonPathSegment::Field(0),
                    JsonPathSegment::Index(0),
                    JsonPathSegment::Field(0)
                ]
            );
            *child = Value::String("redacted".to_string());
            true
        });

        assert!(changed);
        assert_eq!(value["outer"][0]["token"], "redacted");
        assert_eq!(value["safe"], "kept");
    }

    #[test]
    fn entropy_kernel_finds_tokens_and_excludes_hex_digests() {
        let token = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        assert!(looks_high_entropy_token(token));
        assert_eq!(
            high_entropy_ranges(&format!("value: {token}")),
            vec![7..7 + token.len()]
        );
        assert!(!looks_high_entropy_token(
            "3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5"
        ));
    }
}
