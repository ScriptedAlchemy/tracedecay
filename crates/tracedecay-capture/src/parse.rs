use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use thiserror::Error;
pub use tracedecay_domain::MAX_OBSERVATION_RECORD_BYTES;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, MAX_OBSERVATION_STRUCTURE_DEPTH,
    MAX_OBSERVATION_STRUCTURE_VALUES, ObservationOrderingDomainV1, ObservationSourceRangeV1,
    ProviderId,
};

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ClaudeRecordParseErrorV1 {
    #[error("Claude record is empty")]
    Empty,
    #[error("Claude record exceeds the byte limit")]
    TooLarge,
    #[error("Claude record byte range does not match its encoded length")]
    RangeLengthMismatch,
    #[error("Claude record is malformed JSON")]
    Malformed,
    #[error("Claude record must be a JSON object")]
    NonObject,
    #[error("Claude record exceeds the nesting limit")]
    TooDeep,
    #[error("Claude record exceeds the value-count limit")]
    TooManyValues,
    #[error("provider record could not be normalized")]
    NormalizationFailed,
    #[error("canonical observation envelope is invalid")]
    InvalidCanonicalEnvelope,
    #[error("canonical observation envelope exceeds the byte limit")]
    CanonicalEnvelopeTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedPolicyLimitViolation {
    RecordSize,
    NestingDepth,
    ValueCount,
}

#[derive(Clone, Copy, Debug)]
pub struct ParseLimits {
    pub record_bytes: usize,
    pub depth: usize,
    pub values: usize,
}

impl ParseLimits {
    pub const fn default_policy() -> Self {
        Self {
            record_bytes: MAX_OBSERVATION_RECORD_BYTES,
            depth: MAX_OBSERVATION_STRUCTURE_DEPTH,
            values: MAX_OBSERVATION_STRUCTURE_VALUES,
        }
    }
}

/// Parsed and structurally bounded evidence for one complete Claude JSONL record.
///
/// Construction is intentionally restricted to [`parse_claude_record_v1`].
/// Callers may inspect the parsed object to resolve scope, then move the token
/// into the sanitizer without serializing or parsing it again.
pub struct ParsedClaudeRecordV1 {
    value: Value,
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    encoded_len: usize,
    observed_depth: usize,
    observed_values: usize,
    raw_digest: [u8; 32],
    canonical_provider: Option<ProviderId>,
}

impl ParsedClaudeRecordV1 {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn source_range(&self) -> &ObservationSourceRangeV1 {
        &self.source_range
    }

    pub fn ordering_domain(&self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn raw_digest(&self) -> &[u8; 32] {
        &self.raw_digest
    }

    pub fn canonical_provider(&self) -> Option<&ProviderId> {
        self.canonical_provider.as_ref()
    }

    pub fn verify_limits(&self, limits: ParseLimits) -> Result<(), ParsedPolicyLimitViolation> {
        if self.encoded_len > limits.record_bytes {
            return Err(ParsedPolicyLimitViolation::RecordSize);
        }
        if self.observed_depth > limits.depth {
            return Err(ParsedPolicyLimitViolation::NestingDepth);
        }
        if self.observed_values > limits.values {
            return Err(ParsedPolicyLimitViolation::ValueCount);
        }
        Ok(())
    }
}

pub type ParsedObservationRecordV1 = ParsedClaudeRecordV1;
pub type ObservationRecordParseErrorV1 = ClaudeRecordParseErrorV1;

/// Structurally validated native JSON that may be normalized independently
/// for several observation scopes without decoding or hashing the source bytes
/// again. It is process-retained evidence only; durable replay remains the
/// original JSONL plus each scope's source cursor.
#[derive(Clone)]
pub struct PreparedObservationRecordV1 {
    native: Arc<Value>,
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    encoded_len: usize,
    raw_digest: [u8; 32],
    retained_bytes: u64,
}

impl PreparedObservationRecordV1 {
    /// Conservative process-retained charge for the decoded native tree.
    pub fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
}

fn decoded_value_retained_bytes(value: &Value) -> u64 {
    const OBJECT_ENTRY_OVERHEAD: u64 = 128;

    fn payload(value: &Value) -> u64 {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
            Value::String(value) => u64::try_from(value.capacity()).unwrap_or(u64::MAX),
            Value::Array(values) => {
                let slots = values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>());
                values
                    .iter()
                    .fold(u64::try_from(slots).unwrap_or(u64::MAX), |total, value| {
                        total.saturating_add(payload(value))
                    })
            }
            Value::Object(values) => values.iter().fold(0_u64, |total, (key, value)| {
                total
                    .saturating_add(OBJECT_ENTRY_OVERHEAD)
                    .saturating_add(u64::try_from(key.capacity()).unwrap_or(u64::MAX))
                    .saturating_add(payload(value))
            }),
        }
    }

    u64::try_from(
        std::mem::size_of::<Value>()
            .saturating_add(2_usize.saturating_mul(std::mem::size_of::<usize>())),
    )
    .unwrap_or(u64::MAX)
    .saturating_add(payload(value))
}

pub fn parse_claude_record_v1(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
) -> Result<ParsedClaudeRecordV1, ClaudeRecordParseErrorV1> {
    parse_observation_record_v1(record, source_range, ObservationOrderingDomainV1::FileBytes)
}

pub fn parse_observation_record_v1(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    let parsed = parse_observation_record(
        record,
        source_range,
        ordering_domain,
        ParseLimits::default_policy(),
    );
    record_decode_outcome(parsed.is_ok());
    parsed
}

/// Decodes one bounded native JSON record, consumes that decoded value in a
/// provider normalizer, and issues a parser token containing only the canonical
/// envelope. The native record is never decoded a second time.
///
/// Measured at source-record composition, not per JSON token or structure value.
#[hotpath::measure(label = "capture.parse.normalized_record")]
pub fn parse_normalized_observation_record_v1(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    normalize: impl FnOnce(
        Value,
    ) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1>,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    let PreparedObservationRecordV1 {
        native,
        source_range,
        ordering_domain,
        encoded_len,
        raw_digest,
        retained_bytes: _,
    } = prepare_observation_record_v1(record, source_range, ordering_domain)?;
    // The token was built above and shared with nobody, so the native tree
    // moves into the normalizer; only a genuinely shared token would copy.
    let envelope = normalize(Arc::unwrap_or_clone(native))?;
    finish_canonical_envelope(
        envelope,
        source_range,
        ordering_domain,
        encoded_len,
        raw_digest,
    )
}

pub fn prepare_observation_record_v1(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
) -> Result<PreparedObservationRecordV1, ObservationRecordParseErrorV1> {
    let prepared = prepare_observation_record(record, source_range, ordering_domain);
    record_decode_outcome(prepared.is_ok());
    prepared
}

#[hotpath::measure(label = "capture.parse.prepare_record")]
fn prepare_observation_record(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
) -> Result<PreparedObservationRecordV1, ObservationRecordParseErrorV1> {
    let limits = ParseLimits::default_policy();
    validate_record_frame(record, source_range, ordering_domain, limits)?;
    let native =
        serde_json::from_slice::<Value>(record).map_err(|_| ClaudeRecordParseErrorV1::Malformed)?;
    if !native.is_object() {
        return Err(ClaudeRecordParseErrorV1::NonObject);
    }
    validate_structure(&native, limits)?;
    let retained_bytes = decoded_value_retained_bytes(&native);
    Ok(PreparedObservationRecordV1 {
        native: Arc::new(native),
        source_range,
        ordering_domain,
        encoded_len: record.len(),
        raw_digest: record_digest(record),
        retained_bytes,
    })
}

#[hotpath::measure(label = "capture.parse.normalize_prepared")]
pub fn normalize_prepared_observation_record_v1(
    prepared: PreparedObservationRecordV1,
    normalize: impl FnOnce(
        &Value,
    ) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1>,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    let envelope = normalize(prepared.native.as_ref())?;
    finish_canonical_envelope(
        envelope,
        prepared.source_range,
        prepared.ordering_domain,
        prepared.encoded_len,
        prepared.raw_digest,
    )
}

/// Turns a provider's canonical envelope into the parser token both the owned
/// and the shared normalization paths hand to admission.
///
/// `validate` is the bounded canonical-encoding check: it streams the envelope
/// through a byte-limit writer that refuses mid-serialization and tallies the
/// structure the same way `serde_json::to_value` builds it, so nothing here
/// encodes the envelope to bytes only to parse them back. The `Value` is the
/// token's payload — the sanitizer walks and rewrites it — and the structure
/// walk over it records the exact depth and value count that stricter
/// per-policy limits are verified against later.
fn finish_canonical_envelope(
    envelope: CanonicalObservationEnvelopeV1,
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    encoded_len: usize,
    raw_digest: [u8; 32],
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    envelope
        .validate()
        .map_err(|_| ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    if envelope.evidence().ordering_domain() != ordering_domain
        || envelope.evidence().range() != source_range
    {
        return Err(ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope);
    }
    let canonical_provider = envelope.provider().clone();
    let value = serde_json::to_value(&envelope)
        .map_err(|_| ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let structure = validate_structure(&value, ParseLimits::default_policy())?;
    Ok(ParsedObservationRecordV1 {
        value,
        source_range,
        ordering_domain,
        encoded_len,
        observed_depth: structure.depth,
        observed_values: structure.values,
        raw_digest,
        canonical_provider: Some(canonical_provider),
    })
}

#[hotpath::measure(label = "capture.parse.record")]
fn parse_observation_record(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    limits: ParseLimits,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    validate_record_frame(record, source_range, ordering_domain, limits)?;
    let value =
        serde_json::from_slice::<Value>(record).map_err(|_| ClaudeRecordParseErrorV1::Malformed)?;
    if !value.is_object() {
        return Err(ClaudeRecordParseErrorV1::NonObject);
    }
    let structure = validate_structure(&value, limits)?;
    Ok(ParsedObservationRecordV1 {
        value,
        source_range,
        ordering_domain,
        encoded_len: record.len(),
        observed_depth: structure.depth,
        observed_values: structure.values,
        raw_digest: record_digest(record),
        canonical_provider: None,
    })
}

/// Decode-phase entry/failure tally shared by every host record pipeline.
/// Refused records are counted too: corpus-scale waste hides in lines that are
/// read and rejected, which success-only counters never show.
fn record_decode_outcome(decoded: bool) {
    if decoded {
        hotpath::gauge!("capture.parse.records").inc(1u64);
    } else {
        hotpath::gauge!("capture.parse.failures").inc(1u64);
    }
}

fn record_digest(record: &[u8]) -> [u8; 32] {
    // Cumulative decoded bytes across every host pipeline, not a last-record
    // sample — corpus-scale throughput is the quantity being compared.
    hotpath::gauge!("capture.parse.record_bytes").inc(record.len());
    hotpath::measure_block!("capture.parse.record_digest", Sha256::digest(record).into())
}

pub(crate) fn canonical_u64_i64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

pub(crate) fn canonical_u64_string(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

pub(crate) fn sha256_hex(digest: &[u8]) -> String {
    tracedecay_domain::canonical_text::encode_lowercase_hex(digest)
}

fn validate_record_frame(
    record: &[u8],
    source_range: ObservationSourceRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    limits: ParseLimits,
) -> Result<(), ClaudeRecordParseErrorV1> {
    if record.is_empty() {
        return Err(ClaudeRecordParseErrorV1::Empty);
    }
    if record.len() > limits.record_bytes {
        return Err(ClaudeRecordParseErrorV1::TooLarge);
    }
    if ordering_domain == ObservationOrderingDomainV1::FileBytes {
        let range_len = source_range.end() - source_range.start();
        if u64::try_from(record.len()).ok() != Some(range_len) {
            return Err(ClaudeRecordParseErrorV1::RangeLengthMismatch);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct StructureMetrics {
    depth: usize,
    values: usize,
}

fn validate_structure(
    value: &Value,
    limits: ParseLimits,
) -> Result<StructureMetrics, ClaudeRecordParseErrorV1> {
    let mut stack = vec![(value, 1usize)];
    let mut values = 0usize;
    let mut max_depth = 0usize;
    while let Some((current, depth)) = stack.pop() {
        values = values.saturating_add(1);
        max_depth = max_depth.max(depth);
        if values > limits.values {
            return Err(ClaudeRecordParseErrorV1::TooManyValues);
        }
        if depth > limits.depth {
            return Err(ClaudeRecordParseErrorV1::TooDeep);
        }
        match current {
            Value::Object(fields) => {
                stack.extend(
                    fields
                        .values()
                        .map(|child| (child, depth.saturating_add(1))),
                );
            }
            Value::Array(items) => {
                stack.extend(items.iter().map(|child| (child, depth.saturating_add(1))));
            }
            _ => {}
        }
    }
    Ok(StructureMetrics {
        depth: max_depth,
        values,
    })
}

#[cfg(test)]
mod canonical_envelope_tests {
    use serde_json::{Value, json};
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
        CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
        ObservationOrderingDomainV1, ProviderId, SessionId,
    };

    use super::*;

    fn message_envelope(
        content: Value,
        range: ObservationSourceRangeV1,
    ) -> Result<CanonicalObservationEnvelopeV1, ClaudeRecordParseErrorV1> {
        CanonicalObservationEnvelopeV1::new(
            ProviderId::new("codex").unwrap(),
            "message",
            ObservationId::new("record.canonical-parity").unwrap(),
            CanonicalObservationRelationsV1::new(
                SessionId::new("session.canonical-parity").unwrap(),
            ),
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content,
                model: None,
                timestamp: None,
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
        )
        .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
    }

    fn native_record(content: &Value) -> (Vec<u8>, ObservationSourceRangeV1) {
        let record = serde_json::to_vec(&json!({ "content": content })).unwrap();
        let range = ObservationSourceRangeV1::new(0, record.len() as u64).unwrap();
        (record, range)
    }

    /// The finishing path this crate used before it converted the typed
    /// envelope directly: encode, bound, decode, walk.
    fn encoded_round_trip(envelope: &CanonicalObservationEnvelopeV1) -> (Value, StructureMetrics) {
        let bytes = serde_json::to_vec(envelope).unwrap();
        assert!(bytes.len() <= MAX_OBSERVATION_RECORD_BYTES);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let structure = validate_structure(&value, ParseLimits::default_policy()).unwrap();
        (value, structure)
    }

    /// Payloads chosen for where a structural conversion and an encoded round
    /// trip could plausibly disagree: escaping, numeric edges, nulls, empty
    /// containers, and nested tool payloads.
    fn representative_contents() -> Vec<Value> {
        let escaped = "quote \" backslash \\ newline \n tab \t nul \u{0} bell \u{7} \
                       unicode ✓ 🚀 line-sep \u{2028} \u{1F600} </script>"
            .repeat(2_048);
        vec![
            json!([{ "type": "text", "text": escaped }]),
            json!([
                { "type": "tool_use", "id": "toolu_01", "name": "edit", "input": {
                    "path": "src/lib.rs",
                    "edits": [{ "line": 1, "text": "fn main() {}" }, { "line": 2, "text": "" }],
                    "flags": { "dry_run": false, "retries": 0, "nested": { "deeper": [[[]]] } }
                } },
                { "type": "tool_result", "tool_use_id": "toolu_01", "content": [
                    { "type": "text", "text": "ok" }, { "type": "json", "value": null }
                ], "is_error": false }
            ]),
            json!({
                "u64_max": u64::MAX,
                "i64_min": i64::MIN,
                "i64_max": i64::MAX,
                "over_i64": 9_223_372_036_854_775_808_u64,
                "zero": 0,
                "negative_zero": -0.0,
                "one_point_zero": 1.0,
                "tenth": 0.1,
                "third": 1.0_f64 / 3.0,
                "huge": 1e300,
                "tiny": 5e-324,
                "f64_max": f64::MAX,
                "f64_min_positive": f64::MIN_POSITIVE,
                "exp_int": 1e21,
                "mixed": [1, -1, 1.5, -1.5, 100000000000000000000.0, null, true, false]
            }),
            json!({ "empty_object": {}, "empty_array": [], "empty_string": "", "null": null }),
            json!("bare string"),
            json!(42),
            json!(null),
        ]
    }

    #[test]
    fn direct_conversion_matches_the_encoded_round_trip_for_representative_payloads() {
        for content in representative_contents() {
            let (record, range) = native_record(&content);
            let parsed = parse_normalized_observation_record_v1(
                &record,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| message_envelope(native["content"].clone(), range),
            )
            .unwrap();
            let envelope = message_envelope(content, range).unwrap();
            let (expected_value, expected_structure) = encoded_round_trip(&envelope);

            assert_eq!(parsed.value(), &expected_value);
            assert_eq!(
                serde_json::to_vec(parsed.value()).unwrap(),
                serde_json::to_vec(&expected_value).unwrap(),
                "downstream canonical bytes must be identical"
            );
            assert_eq!(parsed.observed_depth, expected_structure.depth);
            assert_eq!(parsed.observed_values, expected_structure.values);
            assert_eq!(parsed.encoded_len(), record.len());
            assert_eq!(
                parsed.raw_digest(),
                &<[u8; 32]>::from(Sha256::digest(&record)),
                "raw digest describes the native frame, not the envelope"
            );
            assert_eq!(parsed.canonical_provider().unwrap().as_str(), "codex");
            assert_eq!(
                parsed.ordering_domain(),
                ObservationOrderingDomainV1::FileBytes
            );
            assert_eq!(parsed.source_range(), &range);
        }
    }

    #[test]
    fn owned_and_shared_normalization_issue_identical_tokens() {
        let content = representative_contents().swap_remove(1);
        let (record, range) = native_record(&content);
        let owned = parse_normalized_observation_record_v1(
            &record,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| message_envelope(native["content"].clone(), range),
        )
        .unwrap();
        let prepared =
            prepare_observation_record_v1(&record, range, ObservationOrderingDomainV1::FileBytes)
                .unwrap();
        let shared = normalize_prepared_observation_record_v1(prepared.clone(), |native| {
            message_envelope(native["content"].clone(), range)
        })
        .unwrap();
        // The shared token is still usable by another scope afterwards.
        let again = normalize_prepared_observation_record_v1(prepared, |native| {
            message_envelope(native["content"].clone(), range)
        })
        .unwrap();

        for token in [&shared, &again] {
            assert_eq!(token.value(), owned.value());
            assert_eq!(token.observed_depth, owned.observed_depth);
            assert_eq!(token.observed_values, owned.observed_values);
            assert_eq!(token.encoded_len(), owned.encoded_len());
            assert_eq!(token.raw_digest(), owned.raw_digest());
        }
    }

    #[test]
    fn stricter_policy_limits_see_the_same_structure_metrics() {
        let content = representative_contents().swap_remove(1);
        let (record, range) = native_record(&content);
        let parsed = parse_normalized_observation_record_v1(
            &record,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| message_envelope(native["content"].clone(), range),
        )
        .unwrap();
        let exact = ParseLimits {
            record_bytes: record.len(),
            depth: parsed.observed_depth,
            values: parsed.observed_values,
        };
        assert_eq!(parsed.verify_limits(exact), Ok(()));
        assert_eq!(
            parsed.verify_limits(ParseLimits {
                depth: exact.depth - 1,
                ..exact
            }),
            Err(ParsedPolicyLimitViolation::NestingDepth)
        );
        assert_eq!(
            parsed.verify_limits(ParseLimits {
                values: exact.values - 1,
                ..exact
            }),
            Err(ParsedPolicyLimitViolation::ValueCount)
        );
        assert_eq!(
            parsed.verify_limits(ParseLimits {
                record_bytes: exact.record_bytes - 1,
                ..exact
            }),
            Err(ParsedPolicyLimitViolation::RecordSize)
        );
    }

    /// An envelope carrying `content` that skips the constructor's own
    /// validation, so the finishing boundary is what has to refuse it.
    fn unvalidated_message_envelope(
        content: Value,
        range: ObservationSourceRangeV1,
    ) -> CanonicalObservationEnvelopeV1 {
        let mut envelope =
            serde_json::to_value(message_envelope(Value::Null, range).unwrap()).unwrap();
        envelope["facts"][0]["content"] = content;
        serde_json::from_value(envelope).unwrap()
    }

    /// Canonical size of the envelope wrapping `content_len` ASCII bytes.
    fn canonical_len_for(content_len: usize) -> usize {
        let content = Value::String("a".repeat(content_len));
        let (_, range) = native_record(&content);
        serde_json::to_vec(&unvalidated_message_envelope(content, range))
            .unwrap()
            .len()
    }

    #[test]
    fn canonical_limit_is_exact_and_refuses_with_the_same_typed_error() {
        // The envelope wraps the content in a near-constant number of bytes
        // (the evidence range's digit count moves with the record length), so
        // walk down from `MAX - overhead` to the largest content that fits.
        let mut at_limit = MAX_OBSERVATION_RECORD_BYTES - canonical_len_for(0);
        while canonical_len_for(at_limit) > MAX_OBSERVATION_RECORD_BYTES {
            at_limit -= 1;
        }
        assert!(canonical_len_for(at_limit + 1) > MAX_OBSERVATION_RECORD_BYTES);

        for (content_len, expected) in [
            (at_limit, Ok(())),
            (
                at_limit + 1,
                Err(ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope),
            ),
        ] {
            let content = Value::String("a".repeat(content_len));
            let (record, range) = native_record(&content);
            // The native record fits; only the normalized envelope may not.
            assert!(record.len() <= MAX_OBSERVATION_RECORD_BYTES);
            let outcome = parse_normalized_observation_record_v1(
                &record,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |mut native| {
                    Ok(unvalidated_message_envelope(
                        native["content"].take(),
                        range,
                    ))
                },
            )
            .map(|parsed| assert_eq!(parsed.value()["facts"][0]["content"], content));
            assert_eq!(outcome, expected, "content of {content_len} bytes");
        }
    }

    #[test]
    fn evidence_mismatch_is_refused_before_conversion() {
        let content = json!({ "text": "mismatch" });
        let (record, range) = native_record(&content);
        let other =
            ObservationSourceRangeV1::new(range.end(), range.end() + record.len() as u64).unwrap();
        assert_eq!(
            parse_normalized_observation_record_v1(
                &record,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| message_envelope(native["content"].clone(), other),
            )
            .err(),
            Some(ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)
        );
        assert_eq!(
            parse_normalized_observation_record_v1(
                &record,
                range,
                ObservationOrderingDomainV1::SqliteRowId,
                |native| message_envelope(native["content"].clone(), range),
            )
            .err(),
            Some(ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)
        );
    }
}
