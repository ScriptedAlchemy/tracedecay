use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde_json::Value;
use thiserror::Error;

use super::detector_kernel::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile, JsonPathSegment,
    JsonVisitMut, NormalizedSensitiveKey, SensitiveKeyPolicy, compile_credential_patterns,
    high_entropy_ranges, visit_json_object_keys, visit_sensitive_json_mut,
};

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
    /// Reserved for public V1 compatibility; malformed input is reported by
    /// `ClaudeRecordParseErrorV1` before detector findings are constructed.
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
    pub quarantine_findings: Vec<SanitizationFindingV1>,
}

struct ConfiguredSensitiveKeyPolicy<'a>(&'a BTreeSet<String>);

impl SensitiveKeyPolicy for ConfiguredSensitiveKeyPolicy<'_> {
    type Match = ();

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match> {
        (self.0.contains(key.ascii_compact()) || is_semantically_sensitive_key(key)).then_some(())
    }
}

fn is_semantically_sensitive_key(key: &NormalizedSensitiveKey) -> bool {
    const SAFE_METADATA_KEYS: &[&str] = &[
        "api_key_hint",
        "credential_type",
        "password_policy",
        "token_budget",
        "token_count",
        "token_counts",
        "token_limit",
        "token_type",
        "token_usage",
    ];

    let separated = key.separated();
    if SAFE_METADATA_KEYS.contains(&separated) {
        return false;
    }

    let suffix = separated.rsplit('_').next().unwrap_or(separated);
    matches!(
        suffix,
        "credential" | "passphrase" | "passwd" | "password" | "secret" | "token"
    ) || matches!(
        separated,
        "access_key" | "api_key" | "private_key" | "secret_key"
    ) || ["_access_key", "_api_key", "_private_key", "_secret_key"]
        .iter()
        .any(|compound| separated.ends_with(compound))
}

pub(crate) fn redact_sensitive_values(
    mut payload: Value,
    sensitive_keys: &BTreeSet<String>,
) -> Result<DetectionResult, DetectionError> {
    let patterns = patterns()?;
    let mut findings = Vec::new();
    let mut quarantine_findings = Vec::new();
    let policy = ConfiguredSensitiveKeyPolicy(sensitive_keys);
    visit_json_object_keys(&payload, &policy, |key, path| {
        let mut key_evidence = key.to_string();
        redact_text(
            &mut key_evidence,
            &structural_location(path),
            patterns,
            &mut quarantine_findings,
            SanitizationActionV1::Quarantined,
        )
    });
    if quarantine_findings.is_empty() {
        visit_sensitive_json_mut(&mut payload, &policy, |value, path| match value {
            JsonVisitMut::SensitiveValue(child, ()) if !child.is_null() => {
                *child = Value::String(REDACTED_SENSITIVE_FIELD.to_string());
                findings.push(SanitizationFindingV1::new(
                    PrivacyDetectorV1::SensitiveField,
                    structural_location(path),
                    DetectionConfidenceV1::Contextual,
                    SanitizationActionV1::Redacted,
                ));
                true
            }
            JsonVisitMut::SensitiveValue(_, ()) => false,
            JsonVisitMut::String(text) => redact_text(
                text,
                &structural_location(path),
                patterns,
                &mut findings,
                SanitizationActionV1::Redacted,
            ),
        });
    }
    findings.sort();
    findings.dedup();
    quarantine_findings.sort();
    quarantine_findings.dedup();
    Ok(DetectionResult {
        payload,
        findings,
        quarantine_findings,
    })
}

pub(crate) fn sanitize_provider_metadata_text(text: &str) -> Option<String> {
    let result = redact_sensitive_values(Value::String(text.to_owned()), &BTreeSet::new()).ok()?;
    if !result.quarantine_findings.is_empty() {
        return None;
    }
    result.payload.as_str().map(str::to_owned)
}

fn redact_text(
    text: &mut String,
    path: &str,
    patterns: &[CredentialPattern],
    findings: &mut Vec<SanitizationFindingV1>,
    action: SanitizationActionV1,
) -> bool {
    let mut changed = false;
    for pattern in patterns {
        let ranges = pattern.ranges(text);
        if !ranges.is_empty() {
            let (detector, confidence, replacement) = pattern_metadata(pattern.kind());
            for range in ranges.into_iter().rev() {
                text.replace_range(range, replacement);
            }
            changed = true;
            findings.push(SanitizationFindingV1::new(
                detector, path, confidence, action,
            ));
        }
    }

    let ranges = high_entropy_ranges(text);
    if !ranges.is_empty() {
        changed = true;
        for range in ranges.into_iter().rev() {
            text.replace_range(range, REDACTED_ENTROPY);
        }
        findings.push(SanitizationFindingV1::new(
            PrivacyDetectorV1::HighEntropyToken,
            path,
            DetectionConfidenceV1::Heuristic,
            action,
        ));
    }
    changed
}

fn patterns() -> Result<&'static [CredentialPattern], DetectionError> {
    static PATTERNS: OnceLock<Result<Vec<CredentialPattern>, regex::Error>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| compile_credential_patterns(CredentialPatternProfile::Observation))
        .as_deref()
        .map_err(|_| DetectionError::Initialization)
}

fn pattern_metadata(
    kind: CredentialPatternKind,
) -> (PrivacyDetectorV1, DetectionConfidenceV1, &'static str) {
    match kind {
        CredentialPatternKind::PrivateKey => (
            PrivacyDetectorV1::PrivateKey,
            DetectionConfidenceV1::Exact,
            REDACTED_PRIVATE_KEY,
        ),
        CredentialPatternKind::BearerToken => (
            PrivacyDetectorV1::BearerToken,
            DetectionConfidenceV1::Exact,
            REDACTED_BEARER,
        ),
        CredentialPatternKind::KnownCredential => (
            PrivacyDetectorV1::ExactCredential,
            DetectionConfidenceV1::Exact,
            REDACTED_EXACT,
        ),
        CredentialPatternKind::CredentialAssignment => (
            PrivacyDetectorV1::CredentialAssignment,
            DetectionConfidenceV1::Contextual,
            REDACTED_ASSIGNMENT,
        ),
    }
}

pub(crate) fn normalize_key(key: &str) -> String {
    NormalizedSensitiveKey::new(key).ascii_compact().to_string()
}

fn structural_location(path: &[JsonPathSegment]) -> String {
    let mut location = String::from("$");
    for segment in path {
        match segment {
            JsonPathSegment::Field(index) => {
                location.push_str("/field[");
                location.push_str(&index.to_string());
                location.push(']');
            }
            JsonPathSegment::Index(index) => {
                location.push('/');
                location.push_str(&index.to_string());
            }
        }
    }
    location
}

fn bounded_location(location: String) -> String {
    if location.len() <= 256 {
        location
    } else {
        "$/<bounded-location>".to_string()
    }
}
