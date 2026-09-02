use std::collections::BTreeSet;

use serde_json::Value;

use super::detect::DetectionError;
use super::detector_kernel::{
    JsonVisitMut, NormalizedSensitiveKey, SensitiveKeyPolicy, visit_sensitive_json_mut,
};
use super::structured::{JsonPreflightFailureV1, parse_json_value};
use tracedecay_capture::ParseLimits;

const REDACTED_ASSIGNMENT: &str = "[TraceDecay redacted: credential assignment]";
const REDACTED_BEARER: &str = "[TraceDecay redacted: bearer token]";
const REDACTED_PRIVATE_KEY: &str = "[TraceDecay redacted: private key]";
const REDACTED_SENSITIVE_FIELD: &str = "[TraceDecay redacted: sensitive field]";
const BUILT_IN_PATTERNS: [&str; 4] = [
    "api_key",
    "bearer_token",
    "password_assignment",
    "private_key",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmSensitiveRedactionPolicyV1 {
    patterns: BTreeSet<String>,
}

impl LcmSensitiveRedactionPolicyV1 {
    pub fn enabled(patterns: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut patterns = patterns
            .into_iter()
            .map(|pattern| pattern.as_ref().trim().to_ascii_lowercase())
            .filter(|pattern| !pattern.is_empty())
            .collect::<BTreeSet<_>>();
        let contains_unknown = patterns.iter().any(|pattern| {
            !BUILT_IN_PATTERNS.contains(&pattern.as_str())
                && !matches!(pattern.as_str(), "all" | "default")
        });
        if patterns.is_empty()
            || patterns.contains("all")
            || patterns.contains("default")
            || contains_unknown
        {
            patterns = BUILT_IN_PATTERNS.into_iter().map(str::to_string).collect();
        }
        Self { patterns }
    }

    fn active(&self, pattern: &str) -> bool {
        self.patterns.contains(pattern)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmSensitiveRedactionV1 {
    text: String,
    patterns: Vec<String>,
}

impl LcmSensitiveRedactionV1 {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn was_redacted(&self) -> bool {
        !self.patterns.is_empty()
    }
}

pub fn redact_lcm_sensitive_payload(
    raw: &str,
    policy: &LcmSensitiveRedactionPolicyV1,
) -> Result<LcmSensitiveRedactionV1, DetectionError> {
    let limits = ParseLimits::default_policy();
    if raw.len() > limits.record_bytes {
        return Err(DetectionError::ScanLimitExceeded);
    }
    let mut patterns = Vec::new();
    let text = if raw.trim_start().starts_with(['{', '[']) {
        let mut payload = match parse_json_value(raw, limits.depth, limits.values) {
            Ok(payload) => payload,
            Err(
                JsonPreflightFailureV1::DepthExceeded | JsonPreflightFailureV1::ValueCountExceeded,
            ) => return Err(DetectionError::ScanLimitExceeded),
            Err(JsonPreflightFailureV1::DuplicateKey | JsonPreflightFailureV1::Malformed) => {
                return Err(DetectionError::StructuredQuarantine);
            }
        };
        if !(payload.is_object() || payload.is_array()) {
            return Err(DetectionError::StructuredQuarantine);
        }
        redact_structured(&mut payload, policy, &mut patterns);
        serde_json::to_string(&payload).map_err(|_| DetectionError::Receipt)?
    } else {
        redact_text(raw, policy, &mut patterns)
    };
    patterns.sort();
    patterns.dedup();
    Ok(LcmSensitiveRedactionV1 { text, patterns })
}

struct LcmSensitiveKeyPolicy<'a>(&'a LcmSensitiveRedactionPolicyV1);

impl SensitiveKeyPolicy for LcmSensitiveKeyPolicy<'_> {
    type Match = &'static str;

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match> {
        let normalized = key.separated();
        let compact = key.compact();
        let policy = self.0;
        if policy.active("api_key")
            && (matches!(
                compact,
                "apikey" | "apitoken" | "accesstoken" | "secretkey" | "clientsecret"
            ) || (normalized.contains("api") && normalized.contains("key"))
                || (normalized.contains("access") && normalized.contains("token"))
                || (normalized.contains("secret") && normalized.contains("key")))
        {
            return Some("api_key");
        }
        if policy.active("bearer_token")
            && matches!(
                compact,
                "authorization" | "authtoken" | "bearertoken" | "token"
            )
        {
            return Some("bearer_token");
        }
        if policy.active("password_assignment")
            && matches!(compact, "password" | "passwd" | "pwd" | "passphrase")
        {
            return Some("password_assignment");
        }
        None
    }
}

fn redact_structured(
    payload: &mut Value,
    policy: &LcmSensitiveRedactionPolicyV1,
    patterns: &mut Vec<String>,
) {
    visit_sensitive_json_mut(
        payload,
        &LcmSensitiveKeyPolicy(policy),
        |visit, _path| match visit {
            JsonVisitMut::SensitiveValue(child, pattern) if !child.is_null() => {
                *child = Value::String(REDACTED_SENSITIVE_FIELD.to_string());
                patterns.push(pattern.to_string());
                true
            }
            JsonVisitMut::SensitiveValue(_, _) => false,
            JsonVisitMut::String(text) => {
                let redacted = redact_text(text, policy, patterns);
                let changed = redacted != *text;
                *text = redacted;
                changed
            }
        },
    );
}

fn redact_text(
    text: &str,
    policy: &LcmSensitiveRedactionPolicyV1,
    patterns: &mut Vec<String>,
) -> String {
    let mut protected = text.to_string();
    if policy.active("api_key") {
        let next = redact_assignments(
            &protected,
            &[
                "apikey",
                "api_key",
                "api-key",
                "apitoken",
                "api token",
                "api_token",
                "access_token",
                "access-token",
                "secret_key",
                "secret-key",
                "client_secret",
                "client-secret",
            ],
            12,
        );
        record_pattern_change(&mut protected, next, "api_key", patterns);
    }
    if policy.active("bearer_token") {
        let next = redact_bearer_tokens(&protected);
        record_pattern_change(&mut protected, next, "bearer_token", patterns);
    }
    if policy.active("password_assignment") {
        let next = redact_assignments(&protected, &["password", "passwd", "pwd", "passphrase"], 6);
        record_pattern_change(&mut protected, next, "password_assignment", patterns);
    }
    if policy.active("private_key") {
        let next = redact_private_keys(&protected);
        record_pattern_change(&mut protected, next, "private_key", patterns);
    }
    protected
}

fn record_pattern_change(
    protected: &mut String,
    next: String,
    pattern: &str,
    patterns: &mut Vec<String>,
) {
    if next != *protected {
        *protected = next;
        patterns.push(pattern.to_string());
    }
}

fn redact_assignments(text: &str, keys: &[&str], min_secret_len: usize) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let Some((key_start, key_len)) = find_next_key(&lower, cursor, keys) else {
            out.push_str(&text[cursor..]);
            break;
        };
        let mut pos = key_start + key_len;
        pos = skip_chars(text, pos, |ch| {
            ch.is_whitespace() || matches!(ch, '"' | '\'')
        });
        if !text[pos..]
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '=' | ':'))
        {
            out.push_str(&text[cursor..pos.min(text.len())]);
            cursor = pos.min(text.len());
            continue;
        }
        pos += 1;
        pos = skip_chars(text, pos, char::is_whitespace);
        let mut secret_start = pos;
        let (secret_end, consumed_to) = if let Some(quote) = text[pos..]
            .chars()
            .next()
            .filter(|ch| matches!(*ch, '"' | '\''))
        {
            pos += quote.len_utf8();
            secret_start = pos;
            while pos < text.len() {
                let Some(ch) = text[pos..].chars().next() else {
                    break;
                };
                if ch == quote || matches!(ch, '\r' | '\n' | ']' | '}') {
                    break;
                }
                pos += ch.len_utf8();
            }
            let secret_end = pos;
            if text[pos..].chars().next().is_some_and(|ch| ch == quote) {
                pos += quote.len_utf8();
            }
            (secret_end, pos)
        } else {
            pos = skip_chars(text, pos, |ch| {
                !ch.is_whitespace() && !matches!(ch, ',' | '"' | '\'' | ']' | '}')
            });
            (pos, pos)
        };
        if text[secret_start..secret_end].chars().count() < min_secret_len {
            out.push_str(&text[cursor..consumed_to]);
            cursor = consumed_to;
            continue;
        }
        out.push_str(&text[cursor..secret_start]);
        out.push_str(REDACTED_ASSIGNMENT);
        out.push_str(&text[secret_end..consumed_to]);
        cursor = consumed_to;
    }
    out
}

fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find("bearer ") {
        let start = cursor + relative;
        let secret_start = start + "bearer ".len();
        let secret_end = skip_chars(text, secret_start, |ch| {
            !ch.is_whitespace() && !matches!(ch, ',' | '"' | '\'' | ']' | '}')
        });
        if text[secret_start..secret_end].chars().count() < 12 {
            out.push_str(&text[cursor..secret_end]);
        } else {
            out.push_str(&text[cursor..secret_start]);
            out.push_str(REDACTED_BEARER);
        }
        cursor = secret_end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn redact_private_keys(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    let mut search = 0usize;
    while let Some((block_start, block_end)) = find_next_private_key_block(text, &lower, search) {
        out.push_str(&text[cursor..block_start]);
        out.push_str(REDACTED_PRIVATE_KEY);
        cursor = block_end;
        search = block_end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn find_next_private_key_block(
    text: &str,
    lower: &str,
    mut search: usize,
) -> Option<(usize, usize)> {
    while let Some(relative) = lower[search..].find("-----begin ") {
        let block_start = search + relative;
        let header_name_start = block_start + "-----begin ".len();
        let header_end_relative = lower[header_name_start..].find("-----")?;
        let header_end = header_name_start + header_end_relative + "-----".len();
        if !lower[block_start..header_end].contains("private key") {
            search = header_name_start.min(text.len());
            continue;
        }
        let mut end_search = header_end;
        while let Some(end_relative) = lower[end_search..].find("-----end ") {
            let footer_start = end_search + end_relative;
            let footer_name_start = footer_start + "-----end ".len();
            let footer_end_relative = lower[footer_name_start..].find("-----")?;
            let block_end = footer_name_start + footer_end_relative + "-----".len();
            if lower[footer_start..block_end].contains("private key") {
                return Some((block_start, block_end));
            }
            end_search = footer_name_start.min(text.len());
        }
        return None;
    }
    None
}

fn find_next_key(lower: &str, cursor: usize, keys: &[&str]) -> Option<(usize, usize)> {
    keys.iter()
        .filter_map(|key| {
            lower[cursor..]
                .find(key)
                .map(|index| (cursor + index, key.len()))
        })
        .min_by_key(|(index, _)| *index)
}

fn skip_chars(text: &str, mut position: usize, predicate: impl Fn(char) -> bool) -> usize {
    while position < text.len() {
        let Some(character) = text[position..].chars().next() else {
            break;
        };
        if !predicate(character) {
            break;
        }
        position += character.len_utf8();
    }
    position
}
