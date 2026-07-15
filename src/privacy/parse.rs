use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::ClaudeByteRangeV1;

pub const PR5_MAX_CLAUDE_RECORD_BYTES: usize = 1024 * 1024;
const PR5_MAX_DEPTH: usize = 96;
const PR5_MAX_VALUES: usize = 50_000;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParsedPolicyLimitViolation {
    RecordSize,
    NestingDepth,
    ValueCount,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ParseLimits {
    pub(super) record_bytes: usize,
    pub(super) depth: usize,
    pub(super) values: usize,
}

impl ParseLimits {
    pub(super) const fn pr5() -> Self {
        Self {
            record_bytes: PR5_MAX_CLAUDE_RECORD_BYTES,
            depth: PR5_MAX_DEPTH,
            values: PR5_MAX_VALUES,
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
    encoded_len: usize,
    observed_depth: usize,
    observed_values: usize,
    raw_digest: [u8; 32],
}

impl ParsedClaudeRecordV1 {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn source_range(&self) -> &ClaudeByteRangeV1 {
        &self.source_range
    }

    pub fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    pub(super) fn into_value(self) -> Value {
        self.value
    }

    pub(super) fn raw_digest(&self) -> &[u8; 32] {
        &self.raw_digest
    }

    pub(super) fn verify_limits(
        &self,
        limits: ParseLimits,
    ) -> Result<(), ParsedPolicyLimitViolation> {
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

pub fn parse_claude_record_v1(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
) -> Result<ParsedClaudeRecordV1, ClaudeRecordParseErrorV1> {
    parse_claude_record(record, source_range, ParseLimits::pr5())
}

fn parse_claude_record(
    record: &[u8],
    source_range: ClaudeByteRangeV1,
    limits: ParseLimits,
) -> Result<ParsedClaudeRecordV1, ClaudeRecordParseErrorV1> {
    if record.is_empty() {
        return Err(ClaudeRecordParseErrorV1::Empty);
    }
    if record.len() > limits.record_bytes {
        return Err(ClaudeRecordParseErrorV1::TooLarge);
    }
    let range_len = source_range.end() - source_range.start();
    if u64::try_from(record.len()).ok() != Some(range_len) {
        return Err(ClaudeRecordParseErrorV1::RangeLengthMismatch);
    }

    let value =
        serde_json::from_slice::<Value>(record).map_err(|_| ClaudeRecordParseErrorV1::Malformed)?;
    if !value.is_object() {
        return Err(ClaudeRecordParseErrorV1::NonObject);
    }
    let structure = validate_structure(&value, limits)?;
    Ok(ParsedClaudeRecordV1 {
        value,
        source_range,
        encoded_len: record.len(),
        observed_depth: structure.depth,
        observed_values: structure.values,
        raw_digest: Sha256::digest(record).into(),
    })
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
