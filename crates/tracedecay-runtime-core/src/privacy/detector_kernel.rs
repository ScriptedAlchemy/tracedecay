#![allow(dead_code)] // white-box test include; not all items exercised
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
    assignment_min_len: Option<usize>,
}

impl CredentialPattern {
    pub fn kind(&self) -> CredentialPatternKind {
        self.kind
    }

    pub fn is_match(&self, text: &str) -> bool {
        if let Some(min_len) = self.assignment_min_len {
            return credential_assignment_ranges(text, &self.regex, min_len)
                .next()
                .is_some();
        }
        self.regex.is_match(text)
    }

    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        if let Some(min_len) = self.assignment_min_len {
            return credential_assignment_ranges(text, &self.regex, min_len).collect();
        }
        self.regex
            .find_iter(text)
            .map(|matched| matched.range())
            .collect()
    }
}

pub(crate) fn compile_credential_patterns(
    profile: CredentialPatternProfile,
) -> Result<Vec<CredentialPattern>, regex::Error> {
    pattern_specs(profile)
        .iter()
        .map(|&(kind, pattern, assignment_min_len)| {
            compile_pattern(kind, pattern, assignment_min_len)
        })
        .collect()
}

fn compile_pattern(
    kind: CredentialPatternKind,
    pattern: &str,
    assignment_min_len: Option<usize>,
) -> Result<CredentialPattern, regex::Error> {
    Ok(CredentialPattern {
        kind,
        regex: Regex::new(pattern)?,
        assignment_min_len,
    })
}

fn pattern_specs(
    profile: CredentialPatternProfile,
) -> &'static [(CredentialPatternKind, &'static str, Option<usize>)] {
    use CredentialPatternKind::{BearerToken, CredentialAssignment, KnownCredential, PrivateKey};

    match profile {
        CredentialPatternProfile::Observation => &[
            (
                PrivateKey,
                r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|$)",
                None,
            ),
            (BearerToken, r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}", None),
            (KnownCredential, KNOWN_CREDENTIAL_PATTERN, None),
            (
                CredentialAssignment,
                r"(?i)\b(?:api[_ -]?(?:key|token)|secret|token|passwd|password|credential|private[_ -]?key|access[_ -]?(?:key|token)|client[_ -]?secret)\b[ \t]*[:=][ \t]*",
                Some(6),
            ),
        ],
        CredentialPatternProfile::Memory => &[
            (
                PrivateKey,
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----",
                None,
            ),
            (BearerToken, r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}", None),
            (KnownCredential, KNOWN_CREDENTIAL_PATTERN, None),
            (
                CredentialAssignment,
                r"(?i)\b(?:api[_-]?(?:key|token)|secret|token|passwd|password|credential|private[_-]?key|access[_-]?(?:key|token)|client[_-]?secret)\b[ \t]*[:=][ \t]*",
                Some(16),
            ),
        ],
    }
}

const MAX_ASSIGNMENT_SCAN_BYTES: usize = 1_048_576;

fn credential_assignment_ranges<'a>(
    text: &'a str,
    prefix: &'a Regex,
    min_len: usize,
) -> impl Iterator<Item = Range<usize>> + 'a {
    prefix.find_iter(text).filter_map(move |matched| {
        let value_start = matched.end();
        let limit = value_start
            .saturating_add(MAX_ASSIGNMENT_SCAN_BYTES)
            .min(text.len());
        let bytes = text.as_bytes();
        let quote = bytes
            .get(value_start)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        let content_start = value_start + usize::from(quote.is_some());
        let mut cursor = content_start;
        let mut closed = false;

        while cursor < limit {
            let byte = bytes[cursor];
            if matches!(byte, b'\r' | b'\n') {
                break;
            }
            let escaped = quote.is_some_and(|quote| {
                byte == quote
                    && bytes[content_start..cursor]
                        .iter()
                        .rev()
                        .take_while(|&&previous| previous == b'\\')
                        .count()
                        % 2
                        == 1
            });
            if quote.is_some_and(|quote| byte == quote) && !escaped {
                closed = true;
                break;
            }
            if quote.is_none()
                && matches!(
                    byte,
                    b' ' | b'\t' | b',' | b';' | b'}' | b']' | b'"' | b'\''
                )
            {
                break;
            }
            cursor += 1;
        }

        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        if cursor.saturating_sub(content_start) < min_len {
            return None;
        }
        let end = cursor + usize::from(closed);
        Some(matched.start()..end)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedSensitiveKey {
    ascii_compact: String,
    separated: String,
    compact: String,
}

impl NormalizedSensitiveKey {
    pub fn new(key: &str) -> Self {
        let ascii_compact = key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|character| character.to_ascii_lowercase())
            .collect();

        let characters: Vec<_> = key.chars().collect();
        let mut separated = String::with_capacity(key.len());
        for (index, &character) in characters.iter().enumerate() {
            let previous = index.checked_sub(1).and_then(|index| characters.get(index));
            let next = characters.get(index + 1);
            let word_boundary = character.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_ascii_uppercase()
                            && next.is_some_and(char::is_ascii_lowercase))
                });
            if word_boundary && !separated.ends_with('_') {
                separated.push('_');
            }
            if character.is_ascii_alphanumeric() {
                separated.push(character.to_ascii_lowercase());
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

    pub fn separated(&self) -> &str {
        &self.separated
    }

    pub fn compact(&self) -> &str {
        &self.compact
    }
}

pub trait SensitiveKeyPolicy {
    type Match: Copy;

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonPathSegment {
    Field(usize),
    Index(usize),
}

pub enum JsonVisitMut<'a, M> {
    SensitiveValue(&'a mut Value, M),
    String(&'a mut String),
}

pub(crate) fn visit_json_object_keys<P, V>(value: &Value, policy: &P, mut visit: V) -> bool
where
    P: SensitiveKeyPolicy,
    V: FnMut(&str, &[JsonPathSegment]) -> bool,
{
    fn walk<P, V>(value: &Value, policy: &P, path: &mut Vec<JsonPathSegment>, visit: &mut V) -> bool
    where
        P: SensitiveKeyPolicy,
        V: FnMut(&str, &[JsonPathSegment]) -> bool,
    {
        match value {
            Value::Object(fields) => {
                let mut matched = false;
                for (index, (key, child)) in fields.iter().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    matched |= visit(key, path);
                    if policy.classify(&NormalizedSensitiveKey::new(key)).is_none() {
                        matched |= walk(child, policy, path, visit);
                    }
                    path.pop();
                }
                matched
            }
            Value::Array(items) => {
                let mut matched = false;
                for (index, child) in items.iter().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    matched |= walk(child, policy, path, visit);
                    path.pop();
                }
                matched
            }
            _ => false,
        }
    }

    walk(value, policy, &mut Vec::new(), &mut visit)
}

pub fn visit_sensitive_json_mut<P, V>(value: &mut Value, policy: &P, mut visit: V) -> bool
where
    P: SensitiveKeyPolicy,
    V: FnMut(JsonVisitMut<'_, P::Match>, &[JsonPathSegment]) -> bool,
{
    fn walk<P, V>(
        value: &mut Value,
        policy: &P,
        path: &mut Vec<JsonPathSegment>,
        visit: &mut V,
    ) -> bool
    where
        P: SensitiveKeyPolicy,
        V: FnMut(JsonVisitMut<'_, P::Match>, &[JsonPathSegment]) -> bool,
    {
        match value {
            Value::Object(fields) => {
                let mut changed = false;
                for (index, (key, child)) in fields.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    let normalized = NormalizedSensitiveKey::new(key);
                    let redacted = policy.classify(&normalized).is_some_and(|matched| {
                        visit(JsonVisitMut::SensitiveValue(child, matched), path)
                    });
                    changed |= redacted;
                    if !redacted {
                        changed |= walk(child, policy, path, visit);
                    }
                    path.pop();
                }
                changed
            }
            Value::Array(items) => {
                let mut changed = false;
                for (index, child) in items.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    changed |= walk(child, policy, path, visit);
                    path.pop();
                }
                changed
            }
            Value::String(text) => visit(JsonVisitMut::String(text), path),
            _ => false,
        }
    }

    walk(value, policy, &mut Vec::new(), &mut visit)
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
        let candidate = &text[start..end];
        let macos_temp_path = candidate.starts_with("/private/var/folders/")
            || candidate.starts_with("/var/folders/");
        if macos_temp_path {
            let mut component_start = 0usize;
            for component in candidate.split('/') {
                let component_end = component_start + component.len();
                if looks_high_entropy_token(component) {
                    ranges.push(start + component_start..start + component_end);
                }
                component_start = component_end + 1;
            }
        } else if looks_high_entropy_token(candidate) && !is_lcm_payload_ref(candidate) {
            ranges.push(start..end);
        }
        start = end;
    }
    ranges
}

fn is_lcm_payload_ref(candidate: &str) -> bool {
    candidate
        .strip_prefix("ref=")
        .unwrap_or(candidate)
        .strip_prefix("payload_")
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Shannon entropy of `token` in bits per character, scaled to per mille.
///
/// This is the same fixed-point sum [`looks_high_entropy_token`] thresholds on,
/// reported rather than compared, so a finding can name the score that produced
/// it on a versioned scale. Integer arithmetic keeps it exactly reproducible.
/// `None` means the fixed-point value could not be represented, so callers
/// retain the redaction but abstain from emitting an invented score.
pub(crate) fn entropy_bits_per_mille(token: &str) -> Option<u32> {
    if token.is_empty() {
        return Some(0);
    }
    let mut counts = [0usize; 256];
    for byte in token.bytes() {
        counts[byte as usize] += 1;
    }
    let len = token.len() as u128;
    let entropy_sum = len * fixed_log2(token.len())
        - counts
            .into_iter()
            .filter(|count| *count != 0)
            .map(|count| count as u128 * fixed_log2(count))
            .sum::<u128>();
    u32::try_from(entropy_sum * 1_000 / (len * ENTROPY_SCALE)).ok()
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
    let len = token.len() as u128;
    let entropy_sum = len * fixed_log2(token.len())
        - counts
            .into_iter()
            .filter(|count| *count != 0)
            .map(|count| count as u128 * fixed_log2(count))
            .sum::<u128>();
    entropy_sum * 10 >= len * 42 * ENTROPY_SCALE
}

const ENTROPY_SCALE: u128 = 1 << 20;

fn fixed_log2(value: usize) -> u128 {
    debug_assert!(value > 0);
    let integer = usize::BITS - 1 - value.leading_zeros();
    let mut result = u128::from(integer) * ENTROPY_SCALE;
    let mut normalized = (value as u128) << (63 - integer);
    for bit in 1..=20 {
        normalized = (normalized * normalized) >> 63;
        if normalized >= (2_u128 << 63) {
            normalized >>= 1;
            result += ENTROPY_SCALE >> bit;
        }
    }
    result
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

        let camel_case = NormalizedSensitiveKey::new("refreshToken");
        assert_eq!(camel_case.separated(), "refresh_token");
        assert_eq!(
            NormalizedSensitiveKey::new("vendorAPIKey").separated(),
            "vendor_api_key"
        );
        assert_eq!(
            NormalizedSensitiveKey::new("JWTToken").separated(),
            "jwt_token"
        );
    }

    #[test]
    fn recursively_redacts_structured_sensitive_values() {
        let policy = KeySet(BTreeSet::from(["token".to_string()]));
        let mut value = json!({"outer": [{"token": "hidden"}], "safe": "kept"});
        let mut visited = Vec::new();
        let changed = visit_sensitive_json_mut(&mut value, &policy, |value, path| match value {
            JsonVisitMut::SensitiveValue(child, ()) => {
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
            }
            JsonVisitMut::String(text) => {
                visited.push((text.clone(), path.to_vec()));
                false
            }
        });

        assert!(changed);
        assert_eq!(value["outer"][0]["token"], "redacted");
        assert_eq!(value["safe"], "kept");
        assert_eq!(
            visited,
            vec![("kept".to_string(), vec![JsonPathSegment::Field(1)])]
        );
    }

    #[test]
    fn replaced_sensitive_values_are_not_visited_recursively() {
        let policy = KeySet(BTreeSet::from(["credential".to_string()]));
        let mut value = json!({
            "credential": {"nested": "secret = p@ssw0rd!"},
            "safe": "kept"
        });
        let mut strings = Vec::new();

        visit_sensitive_json_mut(&mut value, &policy, |value, _| match value {
            JsonVisitMut::SensitiveValue(child, ()) => {
                *child = Value::String("redacted".to_string());
                true
            }
            JsonVisitMut::String(text) => {
                strings.push(text.clone());
                false
            }
        });

        assert_eq!(strings, ["kept"]);
    }

    #[test]
    fn assignment_patterns_include_bounded_quoted_punctuation() {
        let patterns = compile_credential_patterns(CredentialPatternProfile::Observation)
            .expect("valid observation patterns");
        let assignment = patterns
            .iter()
            .find(|pattern| pattern.kind() == CredentialPatternKind::CredentialAssignment)
            .expect("credential assignment pattern");

        assert!(assignment.is_match(r#"password = "p@ssw0rd!""#));
        assert!(assignment.is_match("password = p@ssw0rd!"));
        assert!(assignment.is_match("password = \"truncated!"));

        let escaped_quote = r#"password = "abcdef\"tailsecret""#;
        assert_eq!(
            assignment.ranges(escaped_quote),
            vec![0..escaped_quote.len()]
        );
        let truncated = "password = \"truncated!";
        assert_eq!(assignment.ranges(truncated), vec![0..truncated.len()]);
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

        let below_threshold = "abcdefghi123456789".repeat(2);
        let above_threshold = "abcdefghij123456789".repeat(2);
        assert!(!looks_high_entropy_token(&below_threshold));
        assert!(looks_high_entropy_token(&above_threshold));
    }

    #[test]
    fn entropy_kernel_preserves_ordinary_macos_temp_paths() {
        let path = "/private/var/folders/ab/cd0123456789abcdefghijklmnopqrst/T/";
        assert!(looks_high_entropy_token(path));
        assert!(high_entropy_ranges(path).is_empty());
    }

    #[test]
    fn entropy_kernel_redacts_high_entropy_macos_temp_path_components() {
        let secret = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        let path = format!("/private/var/folders/ab/{secret}/T/");
        let start = "/private/var/folders/ab/".len();
        assert_eq!(
            high_entropy_ranges(&path),
            vec![start..start + secret.len()]
        );
    }

    #[test]
    fn entropy_kernel_redacts_slash_bearing_secrets_with_short_components() {
        let secret = "/AbCdEfGhIjKlMnOpQrStUvWxYz01234/56789aBcDeFgHiJkLmNoPqRsTuVwXy";
        assert!(secret.split('/').all(|part| part.len() < 36));
        assert!(looks_high_entropy_token(secret));
        assert_eq!(high_entropy_ranges(secret), vec![0..secret.len()]);
    }
}
