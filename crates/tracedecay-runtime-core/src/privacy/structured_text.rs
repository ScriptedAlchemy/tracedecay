//! Parse-before-scan for text-shaped payloads.
//!
//! [`super::detect::redact_sensitive_values`] already parses JSON before it
//! scans, but it does so by rewriting a `serde_json::Value`. Most payloads that
//! carry secrets are not JSON and must survive sanitization with their original
//! shape intact — an indexed `.env` file, a `config.toml`, a pasted request
//! header block, a callback URL. This module parses those formats first, uses
//! the parse to decide which byte ranges are sensitive, and then replaces only
//! those ranges in the original text.
//!
//! The point of parsing first is field *meaning*: `refresh_token` is a secret
//! holder even when its value is an unremarkable word that no credential regex
//! will ever match. A raw sweep over the whole blob cannot see that; a parse
//! can.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::OnceLock;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_capture::ParseLimits;
use tracedecay_domain::{
    ComponentVersion, PayloadReferenceV1, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};

use super::assessment::{
    SanitizationAssessmentV1, SanitizationComparisonSetV1, SanitizationRankComponentV1,
};
use super::detect::{
    ConfiguredSensitiveKeyPolicy, DetectionConfidenceV1, DetectionError, PrivacyDetectorV1,
    SanitizationActionV1, SanitizationDetectorOriginV1, SanitizationFindingV1, credential_patterns,
    redact_text,
};
use super::detector_kernel::{CredentialPattern, NormalizedSensitiveKey, SensitiveKeyPolicy};
use super::length_prefixed_sha256_hex;
use super::structured::{
    ParsedStructuredTextV1, StructuredSanitizationLimits, StructuredTextFieldV1,
    StructuredTextFormatV1, StructuredTextParseFailureV1, parse_structured_text,
    sanitize_structured_payload, validate_structured_text_limits,
};

pub const CODE_SOURCE_SANITIZER_VERSION_V1: &str = "privacy.code-source.v1";
const CODE_SOURCE_RECEIPT_DOMAIN_V1: &[u8] = b"tracedecay.privacy.code-source.receipt.v1\0";
pub const LCM_PAYLOAD_SANITIZER_VERSION_V1: &str = "privacy.lcm-payload.v1";
const LCM_PAYLOAD_RECEIPT_DOMAIN_V1: &[u8] = b"tracedecay.privacy.lcm-payload.receipt.v1\0";
const MAX_LCM_PAYLOAD_BYTES_V1: usize = 64 * 1024 * 1024;

/// Replacement for a value the parse proved sensitive.
///
/// Deliberately free of whitespace, quotes, and brackets: the sanitized text is
/// re-parsed on every later pass, and a marker that changes the document's
/// structure would make sanitization non-idempotent (a bracketed marker becomes
/// a YAML flow sequence, a spaced marker breaks a URL back apart).
const REDACTED_STRUCTURED_FIELD: &str = "TraceDecay-redacted-sensitive-field";

/// Shortest value that may be located by a unique whole-document match. Below
/// this length an incidental match elsewhere is likelier than the real value,
/// so location falls back to the key's own line.
const MIN_LOCATABLE_VALUE_BYTES: usize = 4;

pub(crate) struct StructuredTextSanitizationV1 {
    format: Option<StructuredTextFormatV1>,
    sanitized_text: String,
    findings: Vec<SanitizationFindingV1>,
    quarantine_findings: Vec<SanitizationFindingV1>,
}

/// Sanitized bytes of a source document, bound to its raw input by a receipt.
pub struct CodeSourceSanitizationV1 {
    sanitized_bytes: Vec<u8>,
    receipt: SanitizationReceiptV1,
}

impl CodeSourceSanitizationV1 {
    #[cfg(test)]
    pub fn sanitized_bytes(&self) -> &[u8] {
        &self.sanitized_bytes
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        &self.receipt
    }

    pub fn into_parts(self) -> (Vec<u8>, SanitizationReceiptV1) {
        (self.sanitized_bytes, self.receipt)
    }
}

/// Sanitized text of an LCM payload, with all detector evidence retained.
#[derive(Clone, Debug)]
pub struct LcmPayloadSanitizationV1 {
    sanitized_text: String,
    receipt: SanitizationReceiptV1,
    findings: Vec<SanitizationFindingV1>,
}

impl LcmPayloadSanitizationV1 {
    pub fn sanitized_text(&self) -> &str {
        &self.sanitized_text
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        &self.receipt
    }

    pub fn findings(&self) -> &[SanitizationFindingV1] {
        &self.findings
    }

    pub fn into_parts(self) -> (String, SanitizationReceiptV1, Vec<SanitizationFindingV1>) {
        (self.sanitized_text, self.receipt, self.findings)
    }
}

impl StructuredTextSanitizationV1 {
    #[cfg(test)]
    pub(crate) fn sanitized_text(&self) -> &str {
        &self.sanitized_text
    }

    #[cfg(test)]
    pub(crate) fn findings(&self) -> &[SanitizationFindingV1] {
        &self.findings
    }

    pub(crate) fn quarantine_findings(&self) -> &[SanitizationFindingV1] {
        &self.quarantine_findings
    }

    pub(crate) fn into_parts(self) -> (String, Vec<SanitizationFindingV1>) {
        (self.sanitized_text, self.findings)
    }

    #[cfg(test)]
    pub(crate) fn format(&self) -> Option<StructuredTextFormatV1> {
        self.format
    }
}

/// One field the parse proved sensitive, with every byte range of the original
/// text that holds its value.
struct SensitiveCandidate {
    key: String,
    origin: SanitizationDetectorOriginV1,
    spans: Vec<Range<usize>>,
    value_len: usize,
    decoded_value_matched: bool,
}

/// Parses `raw` as a structured document when it is one, redacts the values the
/// parse proved sensitive, and runs the bounded raw scan over everything else.
///
/// Text that does not parse whole is treated as untrusted raw input and scanned
/// exactly as before — never implicitly safe.
pub(crate) fn sanitize_structured_text(
    raw: &str,
) -> Result<StructuredTextSanitizationV1, DetectionError> {
    let patterns = credential_patterns()?;
    let no_configured_keys = BTreeSet::new();
    let policy = ConfiguredSensitiveKeyPolicy(&no_configured_keys);

    let parsed = match parse_structured_text(raw) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return Ok(raw_only(raw, patterns)),
        Err(StructuredTextParseFailureV1::LimitsExceeded) => {
            return Err(DetectionError::ScanLimitExceeded);
        }
        Err(StructuredTextParseFailureV1::Malformed) => {
            return Ok(quarantined_structured_text(raw, patterns));
        }
    };
    validate_structured_text_limits(&parsed.value)
        .map_err(|_| DetectionError::ScanLimitExceeded)?;

    let mut quarantine_findings = Vec::new();
    let candidates = if parsed.fields.is_empty() {
        tree_candidates(raw, &parsed, &policy, patterns, &mut quarantine_findings)
    } else {
        line_candidates(raw, &parsed.fields, &policy, patterns)
    };
    if candidates.is_empty() && quarantine_findings.is_empty() {
        let mut scanned = raw_only(raw, patterns);
        scanned.format = Some(parsed.format);
        return Ok(scanned);
    }

    let ranks = ordinal_ranks(&candidates)?;
    let candidate_count =
        u32::try_from(candidates.len()).map_err(|_| DetectionError::ScanLimitExceeded)?;
    let mut redactions: Vec<Range<usize>> = candidates
        .iter()
        .flat_map(|candidate| candidate.spans.iter().cloned())
        .collect();
    redactions.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
    });
    redactions.dedup_by(|later, earlier| later.start < earlier.end);

    let mut findings = Vec::new();
    let mut sanitized_text = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for span in redactions {
        if span.start < cursor {
            continue;
        }
        let mut segment = raw[cursor..span.start].to_owned();
        redact_text(
            &mut segment,
            "$",
            patterns,
            &mut findings,
            SanitizationActionV1::Redacted,
        );
        sanitized_text.push_str(&segment);
        sanitized_text.push_str(REDACTED_STRUCTURED_FIELD);
        cursor = span.end;
    }
    let mut tail = raw[cursor..].to_owned();
    redact_text(
        &mut tail,
        "$",
        patterns,
        &mut findings,
        SanitizationActionV1::Redacted,
    );
    sanitized_text.push_str(&tail);

    for (index, candidate) in candidates.iter().enumerate() {
        let mut components = vec![SanitizationRankComponentV1::KeySemantics];
        if candidate.decoded_value_matched {
            components.push(SanitizationRankComponentV1::DecodedValuePattern);
        }
        components.push(SanitizationRankComponentV1::ValueLength);
        components.sort();
        findings.push(
            SanitizationFindingV1::new_with_origin(
                PrivacyDetectorV1::SensitiveField,
                candidate.origin,
                format!("$/field[{index}]"),
                DetectionConfidenceV1::Contextual,
                SanitizationActionV1::Redacted,
            )
            .with_assessment(SanitizationAssessmentV1::OrdinalRank {
                comparison_set: SanitizationComparisonSetV1::StructuredDocumentFields,
                components,
                rank: ranks[index],
                of: candidate_count,
            }),
        );
    }

    findings.sort();
    findings.dedup();
    quarantine_findings.sort();
    quarantine_findings.dedup();
    Ok(StructuredTextSanitizationV1 {
        format: Some(parsed.format),
        sanitized_text,
        findings,
        quarantine_findings,
    })
}

fn raw_only(raw: &str, patterns: &[CredentialPattern]) -> StructuredTextSanitizationV1 {
    let mut sanitized_text = raw.to_owned();
    let mut findings = Vec::new();
    redact_text(
        &mut sanitized_text,
        "$",
        patterns,
        &mut findings,
        SanitizationActionV1::Redacted,
    );
    findings.sort();
    findings.dedup();
    StructuredTextSanitizationV1 {
        format: None,
        sanitized_text,
        findings,
        quarantine_findings: Vec::new(),
    }
}

/// A malformed structured-looking document cannot prove that field semantics
/// were scanned. Keep a best-effort raw redaction only for transient handling,
/// then emit a typed quarantine finding so every durable caller rejects it.
fn quarantined_structured_text(
    raw: &str,
    patterns: &[CredentialPattern],
) -> StructuredTextSanitizationV1 {
    let mut sanitized = raw_only(raw, patterns);
    sanitized
        .quarantine_findings
        .push(SanitizationFindingV1::new_with_origin(
            PrivacyDetectorV1::MalformedRecord,
            SanitizationDetectorOriginV1::SanitizerPolicy,
            "$",
            DetectionConfidenceV1::Contextual,
            SanitizationActionV1::Quarantined,
        ));
    sanitized
}

/// Deterministic rank of each candidate within the document: longest value
/// first, then key order, then position. Naming the comparison set and the
/// components is what makes the rank meaningful; it is not a probability.
fn ordinal_ranks(candidates: &[SensitiveCandidate]) -> Result<Vec<u32>, DetectionError> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&left, &right| {
        candidates[right]
            .value_len
            .cmp(&candidates[left].value_len)
            .then_with(|| candidates[left].key.cmp(&candidates[right].key))
            .then_with(|| left.cmp(&right))
    });
    let mut ranks = vec![0u32; candidates.len()];
    for (position, &index) in order.iter().enumerate() {
        ranks[index] =
            u32::try_from(position + 1).map_err(|_| DetectionError::ScanLimitExceeded)?;
    }
    Ok(ranks)
}

fn line_candidates(
    raw: &str,
    fields: &[StructuredTextFieldV1],
    policy: &ConfiguredSensitiveKeyPolicy<'_>,
    patterns: &[CredentialPattern],
) -> Vec<SensitiveCandidate> {
    let mut candidates = Vec::new();
    for field in fields {
        if field.value_span.start >= field.value_span.end
            || field.value_span.end > raw.len()
            || !raw.is_char_boundary(field.value_span.start)
            || !raw.is_char_boundary(field.value_span.end)
        {
            continue;
        }
        let normalized = NormalizedSensitiveKey::new(&field.key);
        let key_origin = policy.classify(&normalized);
        let decoded_value_matched = field
            .decoded_value
            .as_deref()
            .is_some_and(|decoded| trips_a_detector(decoded, patterns));
        let Some(origin) = key_origin
            .or(decoded_value_matched
                .then_some(SanitizationDetectorOriginV1::BuiltInDetectorKernel))
        else {
            continue;
        };
        candidates.push(SensitiveCandidate {
            key: field.key.clone(),
            origin,
            value_len: field.value_span.end - field.value_span.start,
            spans: vec![field.value_span.clone()],
            decoded_value_matched,
        });
    }
    candidates
}

/// Detects whether an already-decoded value carries a credential the encoded
/// bytes hid. `Authorization=Bearer%20…` only looks like a bearer token once
/// the percent escapes are resolved.
fn trips_a_detector(decoded: &str, patterns: &[CredentialPattern]) -> bool {
    let mut probe = decoded.to_owned();
    let mut ignored = Vec::new();
    redact_text(
        &mut probe,
        "$",
        patterns,
        &mut ignored,
        SanitizationActionV1::Redacted,
    )
}

fn tree_candidates(
    raw: &str,
    parsed: &ParsedStructuredTextV1,
    policy: &ConfiguredSensitiveKeyPolicy<'_>,
    patterns: &[CredentialPattern],
    quarantine_findings: &mut Vec<SanitizationFindingV1>,
) -> Vec<SensitiveCandidate> {
    let mut sensitive = Vec::new();
    collect_tree_fields(
        &parsed.value,
        policy,
        patterns,
        &mut sensitive,
        quarantine_findings,
    );

    let mut candidates = Vec::new();
    for (key, value, origin) in sensitive {
        let spans = locate_value(raw, &key, &value)
            .or_else(|| locate_key_line_tail(raw, &key).map(|span| vec![span]));
        let Some(spans) = spans else {
            quarantine_findings.push(SanitizationFindingV1::new_with_origin(
                PrivacyDetectorV1::SensitiveField,
                SanitizationDetectorOriginV1::SanitizerPolicy,
                "$",
                DetectionConfidenceV1::Contextual,
                SanitizationActionV1::Quarantined,
            ));
            continue;
        };
        candidates.push(SensitiveCandidate {
            key,
            origin,
            value_len: value.len(),
            spans,
            decoded_value_matched: false,
        });
    }
    candidates
}

fn collect_tree_fields(
    value: &Value,
    policy: &ConfiguredSensitiveKeyPolicy<'_>,
    patterns: &[CredentialPattern],
    sensitive: &mut Vec<(String, String, SanitizationDetectorOriginV1)>,
    quarantine_findings: &mut Vec<SanitizationFindingV1>,
) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let mut key_evidence = key.clone();
                redact_text(
                    &mut key_evidence,
                    "$",
                    patterns,
                    quarantine_findings,
                    SanitizationActionV1::Quarantined,
                );
                match policy.classify(&NormalizedSensitiveKey::new(key)) {
                    Some(origin) => collect_scalars(child, key, origin, sensitive),
                    None => {
                        collect_tree_fields(
                            child,
                            policy,
                            patterns,
                            sensitive,
                            quarantine_findings,
                        );
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_tree_fields(item, policy, patterns, sensitive, quarantine_findings);
            }
        }
        _ => {}
    }
}

fn collect_scalars(
    value: &Value,
    key: &str,
    origin: SanitizationDetectorOriginV1,
    sensitive: &mut Vec<(String, String, SanitizationDetectorOriginV1)>,
) {
    match value {
        Value::String(text) if !text.is_empty() => {
            sensitive.push((key.to_owned(), text.clone(), origin));
        }
        Value::Number(number) => sensitive.push((key.to_owned(), number.to_string(), origin)),
        Value::Object(fields) => {
            for child in fields.values() {
                collect_scalars(child, key, origin, sensitive);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_scalars(item, key, origin, sensitive);
            }
        }
        Value::String(_) | Value::Bool(_) | Value::Null => {}
    }
}

/// Every byte range of `raw` that holds this field's value.
///
/// A value is claimed when its own line also carries the key, which keeps
/// repeated values (two entries sharing one password) each redacted at their
/// own site. A value that occurs exactly once in the document is claimed
/// outright, because there is nothing else it could be.
fn locate_value(raw: &str, key: &str, value: &str) -> Option<Vec<Range<usize>>> {
    if value.is_empty() {
        return None;
    }
    let mut key_anchored = Vec::new();
    let mut occurrences = 0usize;
    let mut first = None;
    for (index, _) in raw.match_indices(value) {
        occurrences += 1;
        first.get_or_insert(index);
        let line_start = raw[..index].rfind('\n').map_or(0, |position| position + 1);
        if raw[line_start..index].contains(key) {
            key_anchored.push(index..index + value.len());
        }
    }
    if !key_anchored.is_empty() {
        return Some(key_anchored);
    }
    if occurrences == 1 && value.len() >= MIN_LOCATABLE_VALUE_BYTES {
        let start = first?;
        return Some(std::iter::once(start..start + value.len()).collect());
    }
    None
}

/// Fail-closed fallback when a parsed value cannot be matched byte-for-byte in
/// the original text — an escaped JSON string, a folded YAML block. Redacting
/// the rest of the key's line cannot leave the value behind.
///
/// The key must be *anchored* to an occurrence that syntactically looks like
/// a key — an unanchored `raw.find(key)` would happily match a decoy, e.g. a
/// comment mentioning the key name above the real assignment. Redacting a
/// decoy's line while the real value sails through untouched is a redaction
/// fail-open, so a candidate with no qualifying key occurrence returns `None`
/// here and is quarantined by the caller instead of guessing.
fn locate_key_line_tail(raw: &str, key: &str) -> Option<Range<usize>> {
    let index = find_key_occurrence(raw, key)?;
    let after = index + key.len();
    let line_end = raw[after..]
        .find('\n')
        .map_or(raw.len(), |position| after + position);
    let bytes = raw.as_bytes();
    let mut start = after;
    while start < line_end
        && matches!(
            bytes[start],
            b' ' | b'\t' | b':' | b'=' | b'"' | b'\'' | b'>' | b'|' | b'-'
        )
    {
        start += 1;
    }
    (start < line_end && raw.is_char_boundary(start)).then_some(start..line_end)
}

/// First occurrence of `key` in `raw` that is actually a key, not incidental
/// text mentioning the key's name.
///
/// A qualifying occurrence sits at the start of its line, modulo leading
/// whitespace or quote characters, and is followed — after an optional
/// closing quote and whitespace — by a `:` or `=` separator. A bare
/// substring match inside a comment ("# rotate the `api_key` monthly") or an
/// earlier string value ("remember to rotate the `api_key` weekly") does not
/// qualify, so it can never redirect the redaction span away from the real
/// key's line.
fn find_key_occurrence(raw: &str, key: &str) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    let bytes = raw.as_bytes();
    for (index, _) in raw.match_indices(key) {
        let line_start = raw[..index].rfind('\n').map_or(0, |position| position + 1);
        let prefix_is_key_position = raw[line_start..index]
            .chars()
            .all(|c| c == ' ' || c == '\t' || c == '"' || c == '\'');
        if !prefix_is_key_position {
            continue;
        }
        let after = index + key.len();
        let line_end = raw[after..]
            .find('\n')
            .map_or(raw.len(), |position| after + position);
        let mut cursor = after;
        if cursor < line_end && matches!(bytes[cursor], b'"' | b'\'') {
            cursor += 1;
        }
        while cursor < line_end && matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if cursor < line_end && matches!(bytes[cursor], b':' | b'=') {
            return Some(index);
        }
    }
    None
}

/// Sanitizes free-form provider metadata through the structured parse-first
/// route used by GitHub bodies, fact labels, and Claude text metadata.
pub fn sanitize_provider_metadata_text(text: &str) -> Option<String> {
    let result = sanitize_structured_text(text).ok()?;
    if !result.quarantine_findings().is_empty() {
        return None;
    }
    Some(result.into_parts().0)
}

/// Declared document shape of one code source handed to the sanitizer.
///
/// The caller already resolved the file's language from its registry
/// descriptor, so whether whole-document structured-format parsing applies is
/// a declared fact, never something to sniff back out of the bytes. Sniffing
/// misclassified ordinary code and prose — markdown with YAML frontmatter,
/// shell scripts with variable assignments — as malformed structured
/// documents and quarantined them wholesale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeSourceShapeV1 {
    /// A declared structured data format (JSON/YAML/TOML): whole-document
    /// field semantics apply, and an ambiguous parse stays a fail-closed
    /// quarantine.
    StructuredData,
    /// Ordinary code or prose: the bounded raw credential scan applies — the
    /// exact treatment an unparseable document always received — and the
    /// document is never quarantined for failing to be a data format it
    /// never claimed to be.
    CodeOrProse,
}

/// Sanitizes arbitrary source bytes and issues receipt evidence bound to both
/// raw input and sanitized text. Declared structured source files retain
/// their shape.
pub fn sanitize_code_source_bytes(
    raw: &[u8],
    shape: CodeSourceShapeV1,
) -> Result<CodeSourceSanitizationV1, DetectionError> {
    let source = String::from_utf8_lossy(raw);
    let invalid_utf8 = matches!(&source, std::borrow::Cow::Owned(_));
    let detected = match shape {
        CodeSourceShapeV1::StructuredData => sanitize_structured_text(&source)?,
        CodeSourceShapeV1::CodeOrProse => raw_only(&source, credential_patterns()?),
    };
    if !detected.quarantine_findings().is_empty() {
        return Err(DetectionError::StructuredQuarantine);
    }
    let (sanitized, findings) = detected.into_parts();
    let clean = findings.is_empty() && !invalid_utf8;
    let disposition = if clean {
        SanitizerDispositionV1::Accepted
    } else {
        SanitizerDispositionV1::Redacted
    };
    let sensitivity = if clean {
        SensitivityV1::NonSensitive
    } else {
        SensitivityV1::Secret
    };
    let receipt = issue_text_receipt(
        raw,
        &sanitized,
        disposition,
        sensitivity,
        CODE_SOURCE_SANITIZER_VERSION_V1,
        "privacy.code-source.v1.",
        CODE_SOURCE_RECEIPT_DOMAIN_V1,
    )?;
    Ok(CodeSourceSanitizationV1 {
        sanitized_bytes: sanitized.into_bytes(),
        receipt,
    })
}

/// Effective LCM-payload detector revision: the pinned sanitizer contract
/// bound to a digest of the compiled credential rule documents.
///
/// The contract string names the receipt shape and never changes with a rule
/// refresh, so it cannot tell an at-rest rescan whether previously accepted
/// bytes were evaluated under the current rules. This revision changes exactly
/// when the vendored catalogue or the local supplement changes, which is
/// exactly when a completed-rescan watermark must invalidate.
pub fn lcm_payload_detector_revision() -> &'static str {
    static REVISION: OnceLock<String> = OnceLock::new();
    REVISION.get_or_init(|| {
        let digest = super::length_prefixed_sha256_hex(&super::rules::rule_document_bytes());
        format!("{LCM_PAYLOAD_SANITIZER_VERSION_V1}+rules.{}", &digest[..16])
    })
}

pub fn sanitize_lcm_payload_text(raw: &str) -> Result<LcmPayloadSanitizationV1, DetectionError> {
    let (sanitized_text, findings) = detect_lcm_payload(raw)?;
    bind_lcm_payload(raw, sanitized_text, findings)
}

pub fn bind_sanitized_lcm_payload_text(
    raw: &str,
    candidate: &str,
) -> Result<LcmPayloadSanitizationV1, DetectionError> {
    let (_, mut findings) = detect_lcm_payload(raw)?;
    let (sanitized_text, candidate_findings) = detect_lcm_payload(candidate)?;
    findings.extend(candidate_findings);
    findings.sort();
    findings.dedup();
    bind_lcm_payload(raw, sanitized_text, findings)
}

pub fn quarantine_lcm_payload_text(raw: &str) -> Result<SanitizationReceiptV1, DetectionError> {
    if raw.len() > MAX_LCM_PAYLOAD_BYTES_V1 {
        return Err(DetectionError::ScanLimitExceeded);
    }
    let sanitizer_version = ComponentVersion::new(LCM_PAYLOAD_SANITIZER_VERSION_V1)
        .map_err(|_| DetectionError::Receipt)?;
    let disposition = SanitizerDispositionV1::Quarantined;
    let sensitivity = SensitivityV1::Secret;
    let raw_digest = Sha256::digest(raw.as_bytes());
    let receipt_id = SanitizationReceiptId::new(format!(
        "privacy.lcm-payload.v1.{}",
        length_prefixed_sha256_hex(&[
            LCM_PAYLOAD_RECEIPT_DOMAIN_V1,
            sanitizer_version.as_str().as_bytes(),
            disposition.as_str().as_bytes(),
            sensitivity.as_str().as_bytes(),
            raw_digest.as_slice(),
        ])
    ))
    .map_err(|_| DetectionError::Receipt)?;
    let receipt_ref = SanitizationReceiptRefV1::new(receipt_id, sanitizer_version)
        .map_err(|_| DetectionError::Receipt)?;
    SanitizationReceiptV1::new(receipt_ref, disposition, sensitivity, None)
        .map_err(|_| DetectionError::Receipt)
}

fn bind_lcm_payload(
    raw: &str,
    sanitized_text: String,
    findings: Vec<SanitizationFindingV1>,
) -> Result<LcmPayloadSanitizationV1, DetectionError> {
    let (disposition, sensitivity) = if findings.is_empty() {
        (
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
        )
    } else {
        (SanitizerDispositionV1::Redacted, SensitivityV1::Secret)
    };
    let receipt = issue_text_receipt(
        raw.as_bytes(),
        &sanitized_text,
        disposition,
        sensitivity,
        LCM_PAYLOAD_SANITIZER_VERSION_V1,
        "privacy.lcm-payload.v1.",
        LCM_PAYLOAD_RECEIPT_DOMAIN_V1,
    )?;
    Ok(LcmPayloadSanitizationV1 {
        sanitized_text,
        receipt,
        findings,
    })
}

fn detect_lcm_payload(raw: &str) -> Result<(String, Vec<SanitizationFindingV1>), DetectionError> {
    if raw.len() > MAX_LCM_PAYLOAD_BYTES_V1 {
        return Err(DetectionError::ScanLimitExceeded);
    }
    let trimmed = raw.trim_start();
    let json_container = trimmed.starts_with('{')
        || trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.trim_start().chars().next())
            .is_some_and(|next| {
                matches!(
                    next,
                    '"' | '{' | '[' | ']' | '-' | '0'..='9' | 't' | 'f' | 'n'
                )
            });
    if json_container {
        let policy = ParseLimits::default_policy();
        let limits = StructuredSanitizationLimits::new(
            MAX_LCM_PAYLOAD_BYTES_V1,
            MAX_LCM_PAYLOAD_BYTES_V1,
            policy.depth,
            policy.values,
        )
        .map_err(|_| DetectionError::Receipt)?;
        let sanitized = sanitize_structured_payload(raw.as_bytes(), limits)
            .map_err(|_| DetectionError::Receipt)?;
        if !sanitized.was_structurally_parsed() {
            return Err(DetectionError::Receipt);
        }
        let text =
            serde_json::to_string(sanitized.payload()).map_err(|_| DetectionError::Receipt)?;
        return Ok((text, sanitized.findings().to_vec()));
    }

    let detected = sanitize_structured_text(raw)?;
    if !detected.quarantine_findings().is_empty() {
        return Err(DetectionError::Receipt);
    }
    Ok(detected.into_parts())
}

fn issue_text_receipt(
    raw: &[u8],
    sanitized: &str,
    disposition: SanitizerDispositionV1,
    sensitivity: SensitivityV1,
    sanitizer_revision: &str,
    receipt_id_prefix: &str,
    receipt_domain: &[u8],
) -> Result<SanitizationReceiptV1, DetectionError> {
    let payload_reference = PayloadReferenceV1::for_payload(&Value::String(sanitized.to_owned()))
        .map_err(|_| DetectionError::Receipt)?;
    let sanitizer_version =
        ComponentVersion::new(sanitizer_revision).map_err(|_| DetectionError::Receipt)?;
    let raw_digest = Sha256::digest(raw);
    let payload_len = payload_reference.byte_len().to_be_bytes();
    let receipt_id = SanitizationReceiptId::new(format!(
        "{receipt_id_prefix}{}",
        length_prefixed_sha256_hex(&[
            receipt_domain,
            sanitizer_version.as_str().as_bytes(),
            disposition.as_str().as_bytes(),
            sensitivity.as_str().as_bytes(),
            raw_digest.as_slice(),
            payload_reference.digest().as_str().as_bytes(),
            payload_len.as_slice(),
        ])
    ))
    .map_err(|_| DetectionError::Receipt)?;
    let receipt_ref = SanitizationReceiptRefV1::new(receipt_id, sanitizer_version)
        .map_err(|_| DetectionError::Receipt)?;
    SanitizationReceiptV1::new(
        receipt_ref,
        disposition,
        sensitivity,
        Some(payload_reference),
    )
    .map_err(|_| DetectionError::Receipt)
}
