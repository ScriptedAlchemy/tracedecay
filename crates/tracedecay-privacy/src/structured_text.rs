//! Parse-before-scan for text-shaped payloads.
//!
//! [`super::detect::redact_sensitive_values`] already parses JSON before it
//! scans, but it does so by rewriting a `serde_json::Value`. Most payloads that
//! carry secrets are not JSON and must survive sanitization with their original
//! shape intact, an indexed `.env` file, a `config.toml`, a pasted request
//! header block, a callback URL. This module parses those formats first, uses
//! the parse to decide which byte ranges are sensitive, and then replaces only
//! those ranges in the original text.
//!
//! The point of parsing first is field *meaning*: `refresh_token` is a secret
//! holder even when its value is an unremarkable word that no credential regex
//! will ever match. A raw sweep over the whole blob cannot see that; a parse
//! can.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::OnceLock;

use jsonc_parser::ast::{ObjectPropName, Value as Json};
use serde_json::Value;
use sha2::{Digest, Sha256};
use toml::Spanned;
use toml::de::{DeTable, DeValue};
use tracedecay_capture::ParseLimits;
use tracedecay_domain::{
    ComponentVersion, LanguageId, PayloadReferenceV1, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};

use super::assessment::{
    SanitizationAssessmentV1, SanitizationComparisonSetV1, SanitizationRankComponentV1,
};
use super::detect::{
    ConfiguredSensitiveKeyPolicy, DetectionConfidenceV1, DetectionError, PrivacyDetectorV1,
    SanitizationActionV1, SanitizationDetectorOriginV1, SanitizationFindingV1, credential_patterns,
    redact_text,
};
use super::detector_kernel::{NormalizedSensitiveKey, SensitiveKeyPolicy};
use super::length_prefixed_sha256_hex;
use super::rules::{CredentialPatternSet, CredentialScanBatchV1};
use super::structured::{
    ParsedStructuredTextV1, StructuredSanitizationError, StructuredSanitizationLimits,
    StructuredTextFieldV1, StructuredTextFormatV1, StructuredTextParseFailureV1,
    parse_structured_text, sanitize_structured_payload, validate_structured_text_limits,
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

/// Replacement for a JSON or TOML number: neither grammar has a bare-word
/// scalar, so the marker must become a string to keep the document parseable.
const QUOTED_REDACTED_STRUCTURED_FIELD: &str = "\"TraceDecay-redacted-sensitive-field\"";

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
    marker: &'static str,
    value_len: usize,
    decoded_value_matched: bool,
    /// The spans came from searching the raw text for a decoded value rather
    /// than from the parser, so they may not be exactly the value's bytes.
    text_located: bool,
}

/// Parses `raw` as a structured document when it is one, redacts the values the
/// parse proved sensitive, and runs the bounded raw scan over everything else.
///
/// Text that does not parse whole is treated as untrusted raw input and scanned
/// exactly as before, never implicitly safe.
pub(crate) fn sanitize_structured_text(
    raw: &str,
) -> Result<StructuredTextSanitizationV1, DetectionError> {
    let patterns = credential_patterns()?;
    let sanitized = sanitize_structured_text_with(raw, patterns)?;
    patterns
        .checked(sanitized)
        .map_err(|_| DetectionError::Initialization)
}

fn sanitize_structured_text_with(
    raw: &str,
    patterns: &CredentialPatternSet,
) -> Result<StructuredTextSanitizationV1, DetectionError> {
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
        .map_err(detection_error_from_structured_sanitization)?;

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
    let mut redactions: Vec<(Range<usize>, &'static str)> = candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .spans
                .iter()
                .map(|span| (span.clone(), candidate.marker))
        })
        .collect();
    redactions.sort_by(|(left, _), (right, _)| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
    });
    redactions.dedup_by(|(later, _), (earlier, _)| later.start < earlier.end);

    let mut findings = Vec::new();
    let mut sanitized_text = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for (span, marker) in redactions {
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
        sanitized_text.push_str(marker);
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
    // Text-located spans (YAML only) can cover more than the value, so that
    // redaction keeps its format label only if it still parses as the input's
    // format. Parser spans replace exactly the value token.
    let format = (!candidates.iter().any(|candidate| candidate.text_located)
        || matches!(
            parse_structured_text(&sanitized_text),
            Ok(Some(ref reparsed)) if reparsed.format == parsed.format
        ))
    .then_some(parsed.format);
    Ok(StructuredTextSanitizationV1 {
        format,
        sanitized_text,
        findings,
        quarantine_findings,
    })
}

fn raw_only(raw: &str, patterns: &CredentialPatternSet) -> StructuredTextSanitizationV1 {
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
    patterns: &CredentialPatternSet,
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
    patterns: &CredentialPatternSet,
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
            marker: REDACTED_STRUCTURED_FIELD,
            decoded_value_matched,
            text_located: false,
        });
    }
    candidates
}

/// Detects whether an already-decoded value carries a credential the encoded
/// bytes hid. `Authorization=Bearer%20…` only looks like a bearer token once
/// the percent escapes are resolved.
fn trips_a_detector(decoded: &str, patterns: &CredentialPatternSet) -> bool {
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
    patterns: &CredentialPatternSet,
    quarantine_findings: &mut Vec<SanitizationFindingV1>,
) -> Vec<SensitiveCandidate> {
    let Some(document) = document_node(raw, parsed) else {
        quarantine_findings.push(unlocatable_sensitive_field());
        return Vec::new();
    };
    let mut sensitive = Vec::new();
    collect_tree_fields(
        document,
        policy,
        patterns,
        &mut sensitive,
        quarantine_findings,
    );

    let mut candidates = Vec::new();
    for SensitiveScalar {
        key,
        value,
        origin,
        decoded_value_matched,
    } in sensitive
    {
        let (spans, marker, value_len, text_located) = match value {
            ScalarValue::Exact {
                span,
                marker,
                value_len,
                ..
            } => {
                if raw.get(span.clone()).is_none() {
                    quarantine_findings.push(unlocatable_sensitive_field());
                    continue;
                }
                (vec![span], marker, value_len, false)
            }
            ScalarValue::Decoded(value) => {
                let spans = locate_value(raw, &key, &value)
                    .or_else(|| locate_key_line_tail(raw, &key).map(|span| vec![span]));
                let Some(spans) = spans else {
                    quarantine_findings.push(unlocatable_sensitive_field());
                    continue;
                };
                (spans, REDACTED_STRUCTURED_FIELD, value.len(), true)
            }
        };
        candidates.push(SensitiveCandidate {
            key,
            origin,
            value_len,
            spans,
            marker,
            decoded_value_matched,
            text_located,
        });
    }
    candidates
}

fn unlocatable_sensitive_field() -> SanitizationFindingV1 {
    SanitizationFindingV1::new_with_origin(
        PrivacyDetectorV1::SensitiveField,
        SanitizationDetectorOriginV1::SanitizerPolicy,
        "$",
        DetectionConfidenceV1::Contextual,
        SanitizationActionV1::Quarantined,
    )
}

/// One node of a parsed tree document.
enum DocumentNode<'a> {
    Object(Vec<(Cow<'a, str>, DocumentNode<'a>)>),
    Array(Vec<DocumentNode<'a>>),
    Scalar(ScalarValue),
    /// A boolean, null, or empty string: nothing to redact.
    Empty,
}

/// Where a scalar's value sits in the raw text.
enum ScalarValue {
    /// The byte range the format's own parser read, and the marker that keeps
    /// the replaced token valid in that grammar.
    Exact {
        span: Range<usize>,
        marker: &'static str,
        value_len: usize,
        decoded_value: Option<String>,
    },
    /// Only the decoded value is known (`serde_yaml_ng` reports no spans), so
    /// the value is located by searching the raw text.
    Decoded(String),
}

impl ScalarValue {
    fn decoded_value(&self) -> Option<&str> {
        match self {
            Self::Exact { decoded_value, .. } => decoded_value.as_deref(),
            Self::Decoded(value) => Some(value),
        }
    }
}

/// JSON and TOML are walked through span-reporting parsers so every value is
/// replaced exactly where it was read. `None` when that parser refuses a
/// document the canonical parse accepted.
fn document_node<'a>(raw: &'a str, parsed: &'a ParsedStructuredTextV1) -> Option<DocumentNode<'a>> {
    match parsed.format {
        StructuredTextFormatV1::Json => {
            let options = jsonc_parser::ParseOptions {
                allow_comments: true,
                allow_loose_object_property_names: false,
                allow_trailing_commas: false,
                allow_missing_commas: false,
                allow_single_quoted_strings: false,
                allow_hexadecimal_numbers: false,
                allow_unary_plus_numbers: false,
            };
            let parsed = jsonc_parser::parse_to_ast(raw, &Default::default(), &options).ok()?;
            Some(json_node(parsed.value?))
        }
        StructuredTextFormatV1::Toml => {
            let table = DeTable::parse(raw).ok()?;
            Some(toml_table_node(raw, table.into_inner()))
        }
        _ => Some(decoded_node(&parsed.value)),
    }
}

fn json_node(value: Json<'_>) -> DocumentNode<'_> {
    match value {
        Json::Object(object) => DocumentNode::Object(
            object
                .properties
                .into_iter()
                .map(|property| {
                    let key = match property.name {
                        ObjectPropName::String(name) => name.value,
                        ObjectPropName::Word(name) => Cow::Borrowed(name.value),
                    };
                    (key, json_node(property.value))
                })
                .collect(),
        ),
        Json::Array(array) => {
            DocumentNode::Array(array.elements.into_iter().map(json_node).collect())
        }
        Json::StringLit(text) if text.value.is_empty() => DocumentNode::Empty,
        // The range includes both quotes; the marker replaces the body only.
        Json::StringLit(text) => {
            let span = text.range.start + 1..text.range.end.saturating_sub(1);
            let decoded_value = text.value.into_owned();
            DocumentNode::Scalar(ScalarValue::Exact {
                span,
                marker: REDACTED_STRUCTURED_FIELD,
                value_len: decoded_value.len(),
                decoded_value: Some(decoded_value),
            })
        }
        Json::NumberLit(number) => DocumentNode::Scalar(ScalarValue::Exact {
            span: number.range.start..number.range.end,
            marker: QUOTED_REDACTED_STRUCTURED_FIELD,
            value_len: number.value.len(),
            decoded_value: None,
        }),
        Json::BooleanLit(_) | Json::NullKeyword(_) => DocumentNode::Empty,
    }
}

fn toml_table_node<'a>(raw: &str, table: DeTable<'a>) -> DocumentNode<'a> {
    DocumentNode::Object(
        table
            .into_iter()
            .map(|(key, value)| (key.into_inner(), toml_node(raw, value)))
            .collect(),
    )
}

fn toml_node<'a>(raw: &str, value: Spanned<DeValue<'a>>) -> DocumentNode<'a> {
    let span = value.span();
    match value.into_inner() {
        DeValue::Table(table) => toml_table_node(raw, table),
        DeValue::Array(items) => {
            DocumentNode::Array(items.into_iter().map(|item| toml_node(raw, item)).collect())
        }
        DeValue::String(text) if text.is_empty() => DocumentNode::Empty,
        // The span includes the delimiters: `"""`/`'''` for multi-line
        // strings, one quote otherwise. The marker replaces the body only.
        DeValue::String(text) => {
            let decoded_value = text.into_owned();
            let delimiter = raw
                .get(span.clone())
                .filter(|token| token.starts_with("\"\"\"") || token.starts_with("'''"))
                .map_or(1, |_| 3);
            DocumentNode::Scalar(ScalarValue::Exact {
                span: span.start + delimiter..span.end.saturating_sub(delimiter),
                marker: REDACTED_STRUCTURED_FIELD,
                value_len: decoded_value.len(),
                decoded_value: Some(decoded_value),
            })
        }
        DeValue::Integer(_) | DeValue::Float(_) | DeValue::Datetime(_) => {
            DocumentNode::Scalar(ScalarValue::Exact {
                value_len: span.len(),
                span,
                marker: QUOTED_REDACTED_STRUCTURED_FIELD,
                decoded_value: None,
            })
        }
        DeValue::Boolean(_) => DocumentNode::Empty,
    }
}

fn decoded_node(value: &Value) -> DocumentNode<'_> {
    match value {
        Value::Object(fields) => DocumentNode::Object(
            fields
                .iter()
                .map(|(key, child)| (Cow::Borrowed(key.as_str()), decoded_node(child)))
                .collect(),
        ),
        Value::Array(items) => DocumentNode::Array(items.iter().map(decoded_node).collect()),
        Value::String(text) if !text.is_empty() => {
            DocumentNode::Scalar(ScalarValue::Decoded(text.clone()))
        }
        Value::Number(number) => DocumentNode::Scalar(ScalarValue::Decoded(number.to_string())),
        Value::String(_) | Value::Bool(_) | Value::Null => DocumentNode::Empty,
    }
}

/// A scalar under a sensitive key.
struct SensitiveScalar {
    key: String,
    value: ScalarValue,
    origin: SanitizationDetectorOriginV1,
    decoded_value_matched: bool,
}

fn collect_tree_fields(
    node: DocumentNode<'_>,
    policy: &ConfiguredSensitiveKeyPolicy<'_>,
    patterns: &CredentialPatternSet,
    sensitive: &mut Vec<SensitiveScalar>,
    quarantine_findings: &mut Vec<SanitizationFindingV1>,
) {
    match node {
        DocumentNode::Object(fields) => {
            for (key, child) in fields {
                let mut key_evidence = key.to_string();
                redact_text(
                    &mut key_evidence,
                    "$",
                    patterns,
                    quarantine_findings,
                    SanitizationActionV1::Quarantined,
                );
                match policy.classify(&NormalizedSensitiveKey::new(&key)) {
                    Some(origin) => collect_scalars(child, &key, origin, patterns, sensitive),
                    None => {
                        collect_detected_scalars(
                            child,
                            &key,
                            policy,
                            patterns,
                            sensitive,
                            quarantine_findings,
                        );
                    }
                }
            }
        }
        DocumentNode::Array(items) => {
            for item in items {
                collect_tree_fields(item, policy, patterns, sensitive, quarantine_findings);
            }
        }
        DocumentNode::Scalar(_) | DocumentNode::Empty => {}
    }
}

fn collect_scalars(
    node: DocumentNode<'_>,
    key: &str,
    origin: SanitizationDetectorOriginV1,
    patterns: &CredentialPatternSet,
    sensitive: &mut Vec<SensitiveScalar>,
) {
    match node {
        DocumentNode::Scalar(value) => {
            let decoded_value_matched = value
                .decoded_value()
                .is_some_and(|decoded| trips_a_detector(decoded, patterns));
            sensitive.push(SensitiveScalar {
                key: key.to_owned(),
                value,
                origin,
                decoded_value_matched,
            });
        }
        DocumentNode::Object(fields) => {
            for (_, child) in fields {
                collect_scalars(child, key, origin, patterns, sensitive);
            }
        }
        DocumentNode::Array(items) => {
            for item in items {
                collect_scalars(item, key, origin, patterns, sensitive);
            }
        }
        DocumentNode::Empty => {}
    }
}

fn collect_detected_scalars(
    node: DocumentNode<'_>,
    key: &str,
    policy: &ConfiguredSensitiveKeyPolicy<'_>,
    patterns: &CredentialPatternSet,
    sensitive: &mut Vec<SensitiveScalar>,
    quarantine_findings: &mut Vec<SanitizationFindingV1>,
) {
    match node {
        DocumentNode::Scalar(value) => {
            if value
                .decoded_value()
                .is_some_and(|decoded| trips_a_detector(decoded, patterns))
            {
                sensitive.push(SensitiveScalar {
                    key: key.to_owned(),
                    value,
                    origin: SanitizationDetectorOriginV1::BuiltInDetectorKernel,
                    decoded_value_matched: true,
                });
            }
        }
        DocumentNode::Object(fields) => collect_tree_fields(
            DocumentNode::Object(fields),
            policy,
            patterns,
            sensitive,
            quarantine_findings,
        ),
        DocumentNode::Array(items) => {
            for item in items {
                collect_detected_scalars(
                    item,
                    key,
                    policy,
                    patterns,
                    sensitive,
                    quarantine_findings,
                );
            }
        }
        DocumentNode::Empty => {}
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

/// Fail-closed fallback when a parsed YAML value cannot be matched
/// byte-for-byte in the original text, an escaped string, a folded block. Redacting
/// the rest of the key's line cannot leave the value behind. A value that
/// opens a quoted string on the key's line is redacted only up to its closing
/// quote, so the terminator and whatever follows it (a JSON `,`) survive.
///
/// The key must be *anchored* to an occurrence that syntactically looks like
/// a key. An unanchored `raw.find(key)` would happily match a decoy, e.g. a
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
    if start < line_end && matches!(bytes[start], b'"' | b'\'') {
        start += 1;
    }
    while start < line_end
        && matches!(
            bytes[start],
            b' ' | b'\t' | b':' | b'=' | b'>' | b'|' | b'-'
        )
    {
        start += 1;
    }
    let mut end = line_end;
    if start < line_end && matches!(bytes[start], b'"' | b'\'') {
        let quote = bytes[start];
        start += 1;
        if let Some(close) = closing_quote(&bytes[start..line_end], quote) {
            end = start + close;
        }
    }
    (start < end && raw.is_char_boundary(start)).then_some(start..end)
}

/// Offset of the quote that terminates a string whose body starts at
/// `body[0]`: `"` strings honor backslash escapes, `'` strings the doubled
/// `''` escape.
fn closing_quote(body: &[u8], quote: u8) -> Option<usize> {
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            b'\\' if quote == b'"' => index += 2,
            byte if byte == quote => {
                if quote == b'\'' && body.get(index + 1) == Some(&b'\'') {
                    index += 2;
                } else {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// First occurrence of `key` in `raw` that is actually a key, not incidental
/// text mentioning the key's name.
///
/// A qualifying occurrence sits at the start of its line, modulo leading
/// whitespace or quote characters, and is followed, after an optional
/// closing quote and whitespace, by a `:` or `=` separator. A bare
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
/// misclassified ordinary code and prose, markdown with YAML frontmatter,
/// shell scripts with variable assignments, as malformed structured
/// documents and quarantined them wholesale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeSourceShapeV1 {
    /// A declared structured data format (JSON/YAML/TOML): whole-document
    /// field semantics apply, and an ambiguous parse stays a fail-closed
    /// quarantine.
    StructuredData,
    /// Ordinary code or prose: the bounded raw credential scan applies, the
    /// exact treatment an unparseable document always received, and the
    /// document is never quarantined for failing to be a data format it
    /// never claimed to be.
    CodeOrProse,
}

/// The sanitizer shape a captured file's registry-declared language implies.
///
/// Only declared structured data formats get whole-document field semantics
/// (and the fail-closed quarantine of an ambiguous parse). Everything else in
/// the language registry is code or prose and takes the bounded raw
/// credential scan: sniffing the shape out of the bytes misread markdown
/// frontmatter and shell assignments as malformed structured documents and
/// withheld hundreds of ordinary sources from indexing.
pub fn declared_code_source_shape(language: &LanguageId) -> CodeSourceShapeV1 {
    match language.as_str() {
        "json" | "toml" | "yaml" => CodeSourceShapeV1::StructuredData,
        _ => CodeSourceShapeV1::CodeOrProse,
    }
}

/// Sanitizes arbitrary source bytes and issues receipt evidence bound to both
/// raw input and sanitized text. Declared structured source files retain
/// their shape.
/// Keeps the code-source detector's search caches warm across one batch of
/// [`sanitize_code_source_bytes`] calls, such as one snapshot capture.
pub fn code_source_scan_batch() -> CredentialScanBatchV1 {
    CredentialScanBatchV1::new(credential_patterns().ok())
}

pub fn sanitize_code_source_bytes(
    raw: &[u8],
    shape: CodeSourceShapeV1,
) -> Result<CodeSourceSanitizationV1, DetectionError> {
    let source = String::from_utf8_lossy(raw);
    let invalid_utf8 = matches!(&source, std::borrow::Cow::Owned(_));
    let detected = match shape {
        CodeSourceShapeV1::StructuredData => sanitize_structured_text(&source)?,
        CodeSourceShapeV1::CodeOrProse => {
            let patterns = credential_patterns()?;
            patterns
                .checked(raw_only(&source, patterns))
                .map_err(|_| DetectionError::Initialization)?
        }
    };
    if !detected.quarantine_findings().is_empty() {
        return Err(quarantine_detection_error(detected.quarantine_findings()));
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
        .map_err(detection_error_from_structured_sanitization)?;
        let sanitized = sanitize_structured_payload(raw.as_bytes(), limits)
            .map_err(detection_error_from_structured_sanitization)?;
        if !sanitized.was_structurally_parsed() {
            return Err(DetectionError::StructuredQuarantine);
        }
        let text =
            serde_json::to_string(sanitized.payload()).map_err(|_| DetectionError::Receipt)?;
        return Ok((text, sanitized.findings().to_vec()));
    }

    let detected = sanitize_structured_text(raw)?;
    if !detected.quarantine_findings().is_empty() {
        return Err(quarantine_detection_error(detected.quarantine_findings()));
    }
    Ok(detected.into_parts())
}

/// Routes a non-empty quarantine-finding set to its typed refusal.
///
/// Malformed records, unlocatable sensitive values, and credential-bearing
/// keys require different remediation, so preserve their distinct typed
/// refusals. A parsed document can contain both an unlocatable sensitive value
/// and a credential-bearing key; the unlocatable-field result takes precedence
/// because reporting only the key would conceal the value-location failure.
fn quarantine_detection_error(findings: &[SanitizationFindingV1]) -> DetectionError {
    if findings
        .iter()
        .any(|finding| finding.detector() == PrivacyDetectorV1::MalformedRecord)
    {
        DetectionError::StructuredQuarantine
    } else if findings
        .iter()
        .any(|finding| finding.detector() == PrivacyDetectorV1::SensitiveField)
    {
        DetectionError::SensitiveFieldQuarantine
    } else {
        DetectionError::CredentialKeyQuarantine
    }
}

/// Maps the structured sanitizer's typed refusals onto detection errors
/// without conflating classes: quarantines stay quarantines, limit overruns
/// stay bounded-scan refusals, an unavailable or misconfigured detector is an
/// initialization failure, and [`DetectionError::Receipt`] is reserved for
/// actual receipt/canonical construction faults.
fn detection_error_from_structured_sanitization(
    error: StructuredSanitizationError,
) -> DetectionError {
    match error {
        StructuredSanitizationError::RawBytesExceeded
        | StructuredSanitizationError::ExpandedBytesExceeded
        | StructuredSanitizationError::NestingDepthExceeded
        | StructuredSanitizationError::ItemCountExceeded => DetectionError::ScanLimitExceeded,
        StructuredSanitizationError::UnsafeJsonStructure
        | StructuredSanitizationError::InvalidEncoding => DetectionError::StructuredQuarantine,
        StructuredSanitizationError::CredentialKeyQuarantine => {
            DetectionError::CredentialKeyQuarantine
        }
        StructuredSanitizationError::InvalidLimits
        | StructuredSanitizationError::SanitizerUnavailable => DetectionError::Initialization,
        StructuredSanitizationError::CanonicalEncoding => DetectionError::Receipt,
    }
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
