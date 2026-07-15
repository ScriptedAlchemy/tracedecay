use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use thiserror::Error;

const REDACTED_EXACT: &str = "[TraceDecay redacted: exact credential]";
const REDACTED_BEARER: &str = "[TraceDecay redacted: bearer token]";
const REDACTED_ASSIGNMENT: &str = "[TraceDecay redacted: credential assignment]";
const REDACTED_PRIVATE_KEY: &str = "[TraceDecay redacted: private key]";
const REDACTED_ENTROPY: &str = "[TraceDecay redacted: high-entropy token]";
const REDACTED_SENSITIVE_FIELD: &str = "[TraceDecay redacted: sensitive field]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrivacyDetectorV1 {
    ExactCredential,
    BearerToken,
    CredentialAssignment,
    PrivateKey,
    SensitiveField,
    HighEntropyToken,
    MalformedRecord,
    RecordSizeLimit,
    StructureLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectionConfidenceV1 {
    Exact,
    Contextual,
    Heuristic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SanitizationActionV1 {
    Redacted,
    Rejected,
    Quarantined,
}

/// Safe diagnostic evidence. It intentionally has no field for matched text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SanitizationFindingV1 {
    detector: PrivacyDetectorV1,
    location: String,
    confidence: DetectionConfidenceV1,
    action: SanitizationActionV1,
}

impl SanitizationFindingV1 {
    pub(crate) fn new(
        detector: PrivacyDetectorV1,
        location: impl Into<String>,
        confidence: DetectionConfidenceV1,
        action: SanitizationActionV1,
    ) -> Self {
        Self {
            detector,
            location: bounded_location(location.into()),
            confidence,
            action,
        }
    }

    pub fn detector(&self) -> PrivacyDetectorV1 {
        self.detector
    }

    pub fn location(&self) -> &str {
        &self.location
    }

    pub fn confidence(&self) -> DetectionConfidenceV1 {
        self.confidence
    }

    pub fn action(&self) -> SanitizationActionV1 {
        self.action
    }
}

#[derive(Debug, Error)]
pub(crate) enum DetectionError {
    #[error("privacy detector initialization failed")]
    Initialization,
}

pub(crate) struct DetectionResult {
    pub payload: Value,
    pub findings: Vec<SanitizationFindingV1>,
}

struct Pattern {
    detector: PrivacyDetectorV1,
    confidence: DetectionConfidenceV1,
    regex: Regex,
    replacement: &'static str,
}

pub(crate) fn redact_sensitive_values(
    mut payload: Value,
    sensitive_keys: &BTreeSet<String>,
) -> Result<DetectionResult, DetectionError> {
    let patterns = patterns()?;
    let mut findings = Vec::new();
    redact_value(&mut payload, "$", sensitive_keys, patterns, &mut findings);
    findings.sort();
    findings.dedup();
    Ok(DetectionResult { payload, findings })
}

fn redact_value(
    value: &mut Value,
    path: &str,
    sensitive_keys: &BTreeSet<String>,
    patterns: &[Pattern],
    findings: &mut Vec<SanitizationFindingV1>,
) {
    match value {
        Value::Object(fields) => {
            for (index, (key, child)) in fields.iter_mut().enumerate() {
                let child_path = object_path(path, index);
                if sensitive_key(key, sensitive_keys) && !child.is_null() {
                    *child = Value::String(REDACTED_SENSITIVE_FIELD.to_string());
                    findings.push(SanitizationFindingV1::new(
                        PrivacyDetectorV1::SensitiveField,
                        child_path,
                        DetectionConfidenceV1::Contextual,
                        SanitizationActionV1::Redacted,
                    ));
                } else {
                    redact_value(child, &child_path, sensitive_keys, patterns, findings);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                redact_value(
                    child,
                    &format!("{path}/{index}"),
                    sensitive_keys,
                    patterns,
                    findings,
                );
            }
        }
        Value::String(text) => redact_text(text, path, patterns, findings),
        _ => {}
    }
}

fn redact_text(
    text: &mut String,
    path: &str,
    patterns: &[Pattern],
    findings: &mut Vec<SanitizationFindingV1>,
) {
    for pattern in patterns {
        if pattern.regex.is_match(text) {
            *text = pattern
                .regex
                .replace_all(text, pattern.replacement)
                .into_owned();
            findings.push(SanitizationFindingV1::new(
                pattern.detector,
                path,
                pattern.confidence,
                SanitizationActionV1::Redacted,
            ));
        }
    }

    let ranges = high_entropy_ranges(text);
    if !ranges.is_empty() {
        for (start, end) in ranges.into_iter().rev() {
            text.replace_range(start..end, REDACTED_ENTROPY);
        }
        findings.push(SanitizationFindingV1::new(
            PrivacyDetectorV1::HighEntropyToken,
            path,
            DetectionConfidenceV1::Heuristic,
            SanitizationActionV1::Redacted,
        ));
    }
}

fn patterns() -> Result<&'static [Pattern], DetectionError> {
    static PATTERNS: OnceLock<Result<Vec<Pattern>, regex::Error>> = OnceLock::new();
    PATTERNS
        .get_or_init(build_patterns)
        .as_deref()
        .map_err(|_| DetectionError::Initialization)
}

fn build_patterns() -> Result<Vec<Pattern>, regex::Error> {
    Ok(vec![
        Pattern {
            detector: PrivacyDetectorV1::PrivateKey,
            confidence: DetectionConfidenceV1::Exact,
            regex: Regex::new(
                r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|$)",
            )?,
            replacement: REDACTED_PRIVATE_KEY,
        },
        Pattern {
            detector: PrivacyDetectorV1::BearerToken,
            confidence: DetectionConfidenceV1::Exact,
            regex: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}")?,
            replacement: REDACTED_BEARER,
        },
        Pattern {
            detector: PrivacyDetectorV1::ExactCredential,
            confidence: DetectionConfidenceV1::Exact,
            regex: Regex::new(
                r"\b(?:sk-[A-Za-z0-9_-]{20,}|sk-test-[0-9]{6,}|ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|glpat-[A-Za-z0-9_-]{20,})\b",
            )?,
            replacement: REDACTED_EXACT,
        },
        Pattern {
            detector: PrivacyDetectorV1::CredentialAssignment,
            confidence: DetectionConfidenceV1::Contextual,
            regex: Regex::new(
                r#"(?i)\b(?:api[_ -]?key|secret|token|passwd|password|credential|private[_ -]?key|access[_ -]?key)\b\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{6,}"#,
            )?,
            replacement: REDACTED_ASSIGNMENT,
        },
    ])
}

fn sensitive_key(key: &str, configured: &BTreeSet<String>) -> bool {
    let normalized = normalize_key(key);
    configured.contains(&normalized)
}

pub(crate) fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn high_entropy_ranges(text: &str) -> Vec<(usize, usize)> {
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
        let token = &text[start..end];
        if looks_high_entropy(token) {
            ranges.push((start, end));
        }
        start = end;
    }
    ranges
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn looks_high_entropy(token: &str) -> bool {
    if token.len() < 36 || token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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

fn object_path(parent: &str, index: usize) -> String {
    format!("{parent}/field[{index}]")
}

fn bounded_location(location: String) -> String {
    if location.len() <= 256 {
        location
    } else {
        "$/<bounded-location>".to_string()
    }
}
