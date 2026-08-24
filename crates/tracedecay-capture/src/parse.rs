use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use thiserror::Error;
pub use tracedecay_domain::MAX_OBSERVATION_RECORD_BYTES;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ClaudeByteRangeV1, MAX_OBSERVATION_STRUCTURE_DEPTH,
    MAX_OBSERVATION_STRUCTURE_VALUES, ObservationOrderingDomainV1, ProviderId,
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
    source_range: ClaudeByteRangeV1,
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

    pub fn source_range(&self) -> &ClaudeByteRangeV1 {
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
    source_range: ClaudeByteRangeV1,
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
    source_range: ClaudeByteRangeV1,
) -> Result<ParsedClaudeRecordV1, ClaudeRecordParseErrorV1> {
    parse_observation_record_v1(record, source_range, ObservationOrderingDomainV1::FileBytes)
}

pub fn parse_observation_record_v1(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    parse_observation_record(
        record,
        source_range,
        ordering_domain,
        ParseLimits::default_policy(),
    )
}

/// Decodes one bounded native JSON record, consumes that decoded value in a
/// provider normalizer, and issues a parser token containing only the canonical
/// envelope. The native record is never decoded a second time.
///
/// Measured at source-record composition, not per JSON token or structure value.
#[hotpath::measure]
pub fn parse_normalized_observation_record_v1(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
    ordering_domain: ObservationOrderingDomainV1,
    normalize: impl FnOnce(
        Value,
    ) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1>,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    normalize_prepared_observation_record_v1(
        prepare_observation_record_v1(record, source_range, ordering_domain)?,
        |native| normalize(native.clone()),
    )
}

#[hotpath::measure]
pub fn prepare_observation_record_v1(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
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

#[hotpath::measure]
pub fn normalize_prepared_observation_record_v1(
    prepared: PreparedObservationRecordV1,
    normalize: impl FnOnce(
        &Value,
    ) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1>,
) -> Result<ParsedObservationRecordV1, ObservationRecordParseErrorV1> {
    let limits = ParseLimits::default_policy();
    let envelope = normalize(prepared.native.as_ref())?;
    envelope
        .validate()
        .map_err(|_| ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    if envelope.evidence().ordering_domain() != prepared.ordering_domain
        || envelope.evidence().range() != prepared.source_range
    {
        return Err(ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope);
    }
    let canonical_provider = envelope.provider().clone();
    let canonical_bytes = serde_json::to_vec(&envelope)
        .map_err(|_| ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    if canonical_bytes.len() > limits.record_bytes {
        return Err(ClaudeRecordParseErrorV1::CanonicalEnvelopeTooLarge);
    }
    let value = serde_json::from_slice(&canonical_bytes)
        .map_err(|_| ClaudeRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let structure = validate_structure(&value, limits)?;
    Ok(ParsedObservationRecordV1 {
        value,
        source_range: prepared.source_range,
        ordering_domain: prepared.ordering_domain,
        encoded_len: prepared.encoded_len,
        observed_depth: structure.depth,
        observed_values: structure.values,
        raw_digest: prepared.raw_digest,
        canonical_provider: Some(canonical_provider),
    })
}

#[hotpath::measure]
fn parse_observation_record(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
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

fn record_digest(record: &[u8]) -> [u8; 32] {
    hotpath::gauge!("capture.parse.record_bytes").set(record.len());
    hotpath::measure_block!("capture.parse.record_digest", Sha256::digest(record).into())
}

fn validate_record_frame(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
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
