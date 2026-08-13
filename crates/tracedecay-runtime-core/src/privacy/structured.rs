use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;

use percent_encoding::percent_decode_str;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;

use super::detect::{SanitizationFindingV1, redact_sensitive_values};
use tracedecay_capture::ParseLimits;

mod code_shape;

use code_shape::has_code_shape_context;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StructuredSanitizationLimits {
    raw_bytes: usize,
    expanded_bytes: usize,
    depth: usize,
    items: usize,
}

impl StructuredSanitizationLimits {
    pub(crate) fn new(
        raw_bytes: usize,
        expanded_bytes: usize,
        depth: usize,
        items: usize,
    ) -> Result<Self, StructuredSanitizationError> {
        if raw_bytes == 0 || expanded_bytes == 0 || depth == 0 || items == 0 {
            return Err(StructuredSanitizationError::InvalidLimits);
        }
        Ok(Self {
            raw_bytes,
            expanded_bytes,
            depth,
            items,
        })
    }
}

#[derive(Debug)]
pub(crate) struct StructuredSanitizedPayload {
    payload: Value,
    findings: Vec<SanitizationFindingV1>,
    structurally_parsed: bool,
}

impl StructuredSanitizedPayload {
    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }

    pub(crate) const fn was_structurally_parsed(&self) -> bool {
        self.structurally_parsed
    }

    pub(crate) fn findings(&self) -> &[SanitizationFindingV1] {
        &self.findings
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum StructuredSanitizationError {
    #[error("structured sanitizer limits are invalid")]
    InvalidLimits,
    #[error("structured payload exceeds the raw byte limit")]
    RawBytesExceeded,
    #[error("structured payload exceeds the expanded byte limit")]
    ExpandedBytesExceeded,
    #[error("structured payload exceeds the nesting-depth limit")]
    NestingDepthExceeded,
    #[error("structured payload exceeds the item-count limit")]
    ItemCountExceeded,
    #[error("structured payload is not UTF-8")]
    InvalidEncoding,
    #[error("structured JSON has an ambiguous duplicate key or exceeds parse limits")]
    UnsafeJsonStructure,
    #[error("structured sanitizer is unavailable")]
    SanitizerUnavailable,
}

pub(crate) fn sanitize_structured_payload(
    raw: &[u8],
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    if raw.len() > limits.raw_bytes {
        return Err(StructuredSanitizationError::RawBytesExceeded);
    }
    let text =
        std::str::from_utf8(raw).map_err(|_| StructuredSanitizationError::InvalidEncoding)?;
    match parse_json_value(text, limits.depth, limits.items) {
        Ok(value) => sanitize_parsed(value, limits),
        Err(JsonPreflightFailureV1::Malformed) => sanitize_malformed(text, limits),
        // The preflight refuses before `Value` materializes, so it is the only
        // place these overruns can still be attributed to a specific budget.
        // Reporting them as one opaque "unsafe structure" would lose the
        // distinction the post-parse expansion check already draws.
        Err(JsonPreflightFailureV1::DepthExceeded) => {
            Err(StructuredSanitizationError::NestingDepthExceeded)
        }
        Err(JsonPreflightFailureV1::ValueCountExceeded) => {
            Err(StructuredSanitizationError::ItemCountExceeded)
        }
        Err(JsonPreflightFailureV1::DuplicateKey) => {
            Err(StructuredSanitizationError::UnsafeJsonStructure)
        }
    }
}

pub fn sanitize_provider_metadata_json(text: &str, max_bytes: u64) -> Option<Value> {
    let max_bytes = usize::try_from(max_bytes).ok()?;
    if text.len() > max_bytes {
        return None;
    }
    let policy = ParseLimits::default_policy();
    let limits =
        StructuredSanitizationLimits::new(max_bytes, max_bytes, policy.depth, policy.values)
            .ok()?;
    let sanitized = sanitize_structured_payload(text.as_bytes(), limits).ok()?;
    sanitized
        .was_structurally_parsed()
        .then_some(sanitized.payload)
        .filter(Value::is_object)
}

fn sanitize_parsed(
    value: Value,
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    validate_expansion(&value, limits)?;
    let detected = redact_sensitive_values(value, &BTreeSet::new())
        .map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if !detected.quarantine_findings.is_empty() {
        return Err(StructuredSanitizationError::SanitizerUnavailable);
    }
    validate_expansion(&detected.payload, limits)?;
    Ok(StructuredSanitizedPayload {
        payload: detected.payload,
        findings: detected.findings,
        structurally_parsed: true,
    })
}

fn sanitize_malformed(
    text: &str,
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    let detected = redact_sensitive_values(Value::String(text.to_owned()), &BTreeSet::new())
        .map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if !detected.quarantine_findings.is_empty() {
        return Err(StructuredSanitizationError::SanitizerUnavailable);
    }
    validate_expansion(&detected.payload, limits)?;
    Ok(StructuredSanitizedPayload {
        payload: detected.payload,
        findings: detected.findings,
        structurally_parsed: false,
    })
}

/// Text formats the privacy boundary parses before it scans.
///
/// Parsing first is what lets the detectors reason about field *meaning*: a
/// value under a semantically sensitive key is a secret even when its bytes
/// look ordinary, and no raw regex sweep over the whole blob can see that.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredTextFormatV1 {
    Json,
    Yaml,
    Toml,
    Dotenv,
    Url,
    HttpHeaders,
}

/// Largest input the structured pre-parsers accept. Bigger payloads keep the
/// bounded raw scan rather than paying an unbounded parse.
pub(crate) const MAX_STRUCTURED_TEXT_BYTES: usize = 1024 * 1024;

/// One parsed field whose value is anchored to an exact byte range of the
/// original text, so redaction can replace the value in place instead of
/// re-serializing (and thereby corrupting) the document.
#[derive(Clone, Debug)]
pub(crate) struct StructuredTextFieldV1 {
    pub(crate) key: String,
    pub(crate) value_span: Range<usize>,
    /// The decoded value when the on-the-wire bytes are an encoded form
    /// (percent-encoded URL component, quoted dotenv value). Detectors inspect
    /// this as well as the raw slice.
    pub(crate) decoded_value: Option<String>,
}

pub(crate) struct ParsedStructuredTextV1 {
    pub(crate) format: StructuredTextFormatV1,
    pub(crate) value: Value,
    /// Span-anchored fields for the line and segment formats. Tree formats
    /// leave this empty; their spans are recovered by locating the parsed
    /// scalar in the original text.
    pub(crate) fields: Vec<StructuredTextFieldV1>,
}

/// A document advertised itself as a supported structured format but could not
/// be parsed without ambiguity. The caller must quarantine it rather than
/// downgrading it to a raw scan, which cannot prove an ordinary value under a
/// sensitive key is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StructuredTextParseFailureV1 {
    Malformed,
    LimitsExceeded,
}

/// Parses `text` as the first structured format that accepts it whole.
///
/// JSON is tried first because it is also valid YAML, and YAML is tried last
/// because it accepts the widest range of inputs. A format that only parses
/// part of the input is rejected outright: partially structured input is
/// untrusted raw input, never implicitly safe.
pub(crate) fn parse_structured_text(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    if text.len() > MAX_STRUCTURED_TEXT_BYTES {
        return Err(StructuredTextParseFailureV1::LimitsExceeded);
    }
    if text.trim().is_empty() {
        return Ok(None);
    }
    if let Some(parsed) = parse_json_document(text)? {
        return Ok(Some(parsed));
    }
    if let Some(parsed) = parse_url_document(text)? {
        return Ok(Some(parsed));
    }
    if let Some(parsed) = parse_http_header_document(text)? {
        return Ok(Some(parsed));
    }
    if let Some(parsed) = parse_toml_document(text)? {
        return Ok(Some(parsed));
    }
    if let Some(parsed) = parse_dotenv_document(text)? {
        return Ok(Some(parsed));
    }
    parse_yaml_document(text)
}

/// Applies the canonical observation parse policy to a parsed text document
/// before its fields are inspected or redacted. This keeps JSON/YAML/TOML
/// parse expansion, depth, and item counts on the same authority as captured
/// observations rather than assigning text-shaped metadata a weaker budget.
pub(super) fn validate_structured_text_limits(
    value: &Value,
) -> Result<(), StructuredSanitizationError> {
    let policy = ParseLimits::default_policy();
    let limits = StructuredSanitizationLimits::new(
        MAX_STRUCTURED_TEXT_BYTES,
        MAX_STRUCTURED_TEXT_BYTES,
        policy.depth,
        policy.values,
    )?;
    validate_expansion(value, limits)
}

fn parsed(format: StructuredTextFormatV1, value: Value) -> ParsedStructuredTextV1 {
    ParsedStructuredTextV1 {
        format,
        value,
        fields: Vec::new(),
    }
}

fn first_content_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && !is_yaml_preamble_line(line))
}

fn is_yaml_preamble_line(line: &str) -> bool {
    line == "..."
        || line.starts_with("%YAML ")
        || line
            .strip_prefix("---")
            .is_some_and(|rest| rest.is_empty() || rest.trim_start().starts_with('#'))
}

fn has_yaml_preamble(text: &str) -> bool {
    text.lines().map(str::trim).any(is_yaml_preamble_line)
}

fn looks_like_yaml_mapping(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix("- ").unwrap_or(line);
    let line = line.strip_prefix('{').map(str::trim_start).unwrap_or(line);
    let Some((key, rest)) = split_yaml_mapping_entry(line) else {
        return false;
    };
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return false;
    }
    is_yaml_mapping_key(key.trim_end())
}

fn split_yaml_mapping_entry(line: &str) -> Option<(&str, &str)> {
    if let Some(quote) = line
        .chars()
        .next()
        .filter(|quote| matches!(quote, '\'' | '"'))
    {
        let quoted = line.strip_prefix(quote)?;
        let closing = quoted.find(quote)?;
        let key_end = quote.len_utf8() + closing + quote.len_utf8();
        let rest = line.get(key_end..)?.trim_start().strip_prefix(':')?;
        return Some((&line[..key_end], rest));
    }
    line.split_once(':')
}

fn is_yaml_mapping_key(key: &str) -> bool {
    let quoted = key.len() >= 2
        && ((key.starts_with('"') && key.ends_with('"'))
            || (key.starts_with('\'') && key.ends_with('\'')));
    if quoted {
        return true;
    }
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'_' | b'-' | b'.'))
}

fn has_yaml_mapping_intent(text: &str) -> bool {
    if !has_yaml_preamble(text) && has_code_shape_context(text) {
        return false;
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !is_yaml_preamble_line(line))
        .any(looks_like_yaml_mapping)
}

fn is_assignment_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|character: char| character.is_ascii_digit())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn is_dotenv_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|character: char| character.is_ascii_digit())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
}

fn has_assignment_intent(text: &str) -> bool {
    if has_code_shape_context(text) {
        return false;
    }
    text.lines().any(|line| {
        let mut body = line.trim();
        if body.is_empty() || body.starts_with('#') {
            return false;
        }
        if let Some(rest) = body.strip_prefix("export ") {
            body = rest.trim_start();
        }
        body.split_once('=')
            .is_some_and(|(key, _)| is_assignment_key(key.trim()))
    })
}

fn parse_json_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with(['{', '[']) {
        return Ok(None);
    }
    let policy = ParseLimits::default_policy();
    let value = match parse_json_value(text, policy.depth, policy.values) {
        Ok(value) => value,
        Err(JsonPreflightFailureV1::DepthExceeded | JsonPreflightFailureV1::ValueCountExceeded) => {
            return Err(StructuredTextParseFailureV1::LimitsExceeded);
        }
        // A duplicate member is ambiguity inside a document that *is* JSON, so
        // it is refused outright rather than handed on.
        Err(JsonPreflightFailureV1::DuplicateKey) => {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        // `[` opens a JSON array and a TOML table header alike, so a leading
        // bracket is not evidence of JSON -- only a successful parse is. When
        // the parse fails, hand the document to the formats that have not had
        // their turn instead of quarantining a perfectly good TOML table. `{`
        // is not shared with any format here, so a broken object stays a
        // refusal rather than falling through to a raw scan.
        Err(JsonPreflightFailureV1::Malformed) => {
            return if trimmed.starts_with('[') {
                Ok(None)
            } else {
                Err(StructuredTextParseFailureV1::Malformed)
            };
        }
    };
    (value.is_object() || value.is_array())
        .then(|| parsed(StructuredTextFormatV1::Json, value))
        .ok_or(StructuredTextParseFailureV1::Malformed)
        .map(Some)
}

/// Walks the JSON stream with serde before `Value` materializes it. `Value`
/// normally keeps the final duplicate member and loses the earlier bytes,
/// which would let an earlier sensitive value survive the span sanitizer.
pub(crate) fn parse_json_value(
    text: &str,
    max_depth: usize,
    max_values: usize,
) -> Result<Value, JsonPreflightFailureV1> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let mut budget = JsonStructureBudget {
        max_depth,
        max_values,
        values: 0,
    };
    let mut failure = None;
    let preflight = JsonStructurePreflight {
        budget: &mut budget,
        failure: &mut failure,
        depth: 1,
    }
    .deserialize(&mut deserializer)
    .and_then(|_| deserializer.end());
    if preflight.is_err() {
        return Err(failure.unwrap_or(JsonPreflightFailureV1::Malformed));
    }
    serde_json::from_str(text).map_err(|_| JsonPreflightFailureV1::Malformed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JsonPreflightFailureV1 {
    Malformed,
    DuplicateKey,
    /// The document nests deeper than the canonical parse policy allows.
    ///
    /// Depth and value-count overruns stay distinct because the sanitizer
    /// contract reports them as distinct refusals: collapsing them into one
    /// "limits exceeded" would tell a caller that its payload was rejected
    /// without telling it which budget to look at.
    DepthExceeded,
    ValueCountExceeded,
}

struct JsonStructureBudget {
    max_depth: usize,
    max_values: usize,
    values: usize,
}

impl JsonStructureBudget {
    fn record_value(&mut self, depth: usize) -> Result<(), JsonPreflightFailureV1> {
        if depth > self.max_depth {
            return Err(JsonPreflightFailureV1::DepthExceeded);
        }
        self.values = self
            .values
            .checked_add(1)
            .ok_or(JsonPreflightFailureV1::ValueCountExceeded)?;
        if self.values > self.max_values {
            return Err(JsonPreflightFailureV1::ValueCountExceeded);
        }
        Ok(())
    }
}

struct JsonStructurePreflight<'a> {
    budget: &'a mut JsonStructureBudget,
    failure: &'a mut Option<JsonPreflightFailureV1>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for JsonStructurePreflight<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(JsonStructurePreflightVisitor {
            budget: self.budget,
            failure: self.failure,
            depth: self.depth,
        })
    }
}

struct JsonStructurePreflightVisitor<'a> {
    budget: &'a mut JsonStructureBudget,
    failure: &'a mut Option<JsonPreflightFailureV1>,
    depth: usize,
}

impl JsonStructurePreflightVisitor<'_> {
    fn record<E: de::Error>(&mut self) -> Result<(), E> {
        self.budget.record_value(self.depth).map_err(|failure| {
            *self.failure = Some(failure);
            E::custom("JSON value exceeds the canonical parse limit")
        })
    }
}

impl<'de> Visitor<'de> for JsonStructurePreflightVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("a JSON value within canonical limits and without duplicate object keys")
    }

    fn visit_bool<E>(mut self, _: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_i64<E>(mut self, _: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_u64<E>(mut self, _: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_f64<E>(mut self, _: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_str<E>(mut self, _: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_none<E>(mut self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_unit<E>(mut self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record()
    }

    fn visit_seq<A>(mut self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.record()?;
        while sequence
            .next_element_seed(JsonStructurePreflight {
                budget: &mut *self.budget,
                failure: &mut *self.failure,
                depth: self.depth.saturating_add(1),
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(mut self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.record()?;
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                *self.failure = Some(JsonPreflightFailureV1::DuplicateKey);
                return Err(<A::Error as de::Error>::custom("duplicate JSON object key"));
            }
            map.next_value_seed(JsonStructurePreflight {
                budget: &mut *self.budget,
                failure: &mut *self.failure,
                depth: self.depth.saturating_add(1),
            })?;
        }
        Ok(())
    }
}

fn parse_yaml_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    // Anchors and aliases are the YAML expansion bomb; a tab is illegal YAML
    // indentation. Both are cheap to reject before paying for a parse, and
    // rejecting an apparent YAML document quarantines it rather than giving a
    // raw scan an opportunity to miss field semantics.
    let Some(first) = first_content_line(text) else {
        return Ok(None);
    };
    let explicit_yaml_preamble = has_yaml_preamble(text);
    let yaml_intent = explicit_yaml_preamble || has_yaml_mapping_intent(text);
    let established_mapping =
        looks_like_yaml_mapping(first) && (!has_code_shape_context(text) || explicit_yaml_preamble);
    if !established_mapping {
        if yaml_intent {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        return Ok(None);
    }
    if text.bytes().any(|byte| matches!(byte, b'&' | b'*' | b'\t')) {
        return Err(StructuredTextParseFailureV1::Malformed);
    }
    preflight_tree_document(text)?;
    // Parse through YAML's own value authority before converting to the common
    // JSON-shaped tree. In particular, YAML tags arrive through serde's enum
    // data model, which the JSON duplicate-key preflight cannot accept. The
    // YAML mapping implementation rejects duplicate keys while materializing,
    // so this preserves the fail-closed ambiguity fence without rejecting
    // valid YAML-specific syntax ahead of the canonical parser.
    let yaml_value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|_| StructuredTextParseFailureV1::Malformed)?;
    let value =
        serde_json::to_value(yaml_value).map_err(|_| StructuredTextParseFailureV1::Malformed)?;
    (value.is_object() || value.is_array())
        .then(|| parsed(StructuredTextFormatV1::Yaml, value))
        .ok_or(StructuredTextParseFailureV1::Malformed)
        .map(Some)
}

fn parse_toml_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    let Some(first) = first_content_line(text) else {
        return Ok(None);
    };
    let table_candidate = first
        .split_once('#')
        .map_or(first, |(header, _)| header.trim_end());
    let table_header = table_candidate.starts_with('[') && table_candidate.ends_with(']');
    if first.starts_with('[') && !table_header {
        return Err(StructuredTextParseFailureV1::Malformed);
    }
    if !table_header && !first.contains('=') {
        if looks_like_yaml_mapping(first) {
            return Ok(None);
        }
        if has_assignment_intent(text) {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        return Ok(None);
    }
    preflight_tree_document(text)?;
    // A dotenv document is syntactically close to TOML but permits unquoted
    // values, so leave a TOML parse failure for the dotenv parser to classify.
    // Deserialize through TOML's own table model and re-encode, rather than
    // asking TOML to fill a `serde_json::Value` directly: the latter drives the
    // whole document through `deserialize_any`, which TOML does not answer for
    // a document root, so every well-formed table was being classified as
    // unparseable and quarantined.
    let Ok(table) = toml::from_str::<toml::Table>(text) else {
        if table_header {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        return Ok(None);
    };
    let value = serde_json::to_value(table).map_err(|_| StructuredTextParseFailureV1::Malformed)?;
    value
        .is_object()
        .then(|| parsed(StructuredTextFormatV1::Toml, value))
        .ok_or(StructuredTextParseFailureV1::Malformed)
        .map(Some)
}

/// Parses a `KEY=value` environment file, recording the exact span of each
/// value so quoting is preserved when the value is redacted.
fn parse_dotenv_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    let assignment_intent = has_assignment_intent(text);
    let yaml_mapping_intent = first_content_line(text).is_some_and(looks_like_yaml_mapping)
        && (!has_code_shape_context(text) || has_yaml_preamble(text));
    let mut map = Map::new();
    let mut fields = Vec::new();
    let mut consumed = 0usize;
    for line in text.split_inclusive('\n') {
        let line_start = consumed;
        consumed += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let leading = content.len() - content.trim_start().len();
        let content = content.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let mut key_start = line_start + leading;
        let mut body = content;
        if let Some(rest) = body.strip_prefix("export ") {
            key_start += body.len() - rest.trim_start().len();
            body = rest.trim_start();
        }
        let Some((key, value)) = body.split_once('=') else {
            return if fields.is_empty() && (!assignment_intent || yaml_mapping_intent) {
                Ok(None)
            } else {
                Err(StructuredTextParseFailureV1::Malformed)
            };
        };
        if !is_dotenv_key(key) {
            return if fields.is_empty() && (!assignment_intent || yaml_mapping_intent) {
                Ok(None)
            } else {
                Err(StructuredTextParseFailureV1::Malformed)
            };
        }
        let value_start = key_start + key.len() + 1;
        let raw_value = value.trim_end();
        let quoted = raw_value.len() >= 2
            && (raw_value.starts_with('"') && raw_value.ends_with('"')
                || raw_value.starts_with('\'') && raw_value.ends_with('\''));
        let (span, decoded) = if quoted {
            (
                value_start + 1..value_start + raw_value.len() - 1,
                Some(raw_value[1..raw_value.len() - 1].to_owned()),
            )
        } else {
            (value_start..value_start + raw_value.len(), None)
        };
        let stored = decoded.clone().unwrap_or_else(|| raw_value.to_owned());
        map.insert(key.to_owned(), Value::String(stored));
        fields.push(StructuredTextFieldV1 {
            key: key.to_owned(),
            value_span: span,
            decoded_value: decoded,
        });
    }
    Ok((!fields.is_empty()).then_some(ParsedStructuredTextV1 {
        format: StructuredTextFormatV1::Dotenv,
        value: Value::Object(map),
        fields,
    }))
}

fn is_http_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_http_start_line(line: &str) -> bool {
    line.starts_with("HTTP/")
        || line
            .split_once(' ')
            .is_some_and(|(_, rest)| rest.contains("HTTP/"))
}

/// Parses an HTTP header block. A blank line ends the block; anything after it
/// makes the input a message with a body rather than a header block, which the
/// raw scan handles instead.
fn parse_http_header_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    let Some(first) = first_content_line(text) else {
        return Ok(None);
    };
    let has_start_line = is_http_start_line(first);
    let has_indented_mapping_member = !has_start_line
        && looks_like_yaml_mapping(first)
        && text.lines().skip(1).any(|line| {
            let content = line.trim_start_matches(' ');
            content.len() != line.len() && looks_like_yaml_mapping(content)
        });
    if has_indented_mapping_member {
        return Ok(None);
    }
    if !has_start_line
        && !first
            .split_once(':')
            .is_some_and(|(name, _)| is_http_field_name(name))
    {
        return Ok(None);
    }
    let mut map = Map::new();
    let mut header_names = BTreeSet::new();
    let mut fields = Vec::new();
    let mut consumed = 0usize;
    let mut ended = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let line_start = consumed;
        consumed += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        if content.trim().is_empty() {
            ended = !fields.is_empty();
            continue;
        }
        if ended {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        if index == 0 && has_start_line {
            continue;
        }
        let Some((name, value)) = content.split_once(':') else {
            return if has_start_line || !fields.is_empty() {
                Err(StructuredTextParseFailureV1::Malformed)
            } else {
                Ok(None)
            };
        };
        if !is_http_field_name(name) {
            return if has_start_line || !fields.is_empty() {
                Err(StructuredTextParseFailureV1::Malformed)
            } else {
                Ok(None)
            };
        }
        if !header_names.insert(name.to_ascii_lowercase()) {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        let padding = value.len() - value.trim_start().len();
        let value_start = line_start + name.len() + 1 + padding;
        let trimmed_value = value.trim();
        map.insert(name.to_owned(), Value::String(trimmed_value.to_owned()));
        fields.push(StructuredTextFieldV1 {
            key: name.to_owned(),
            value_span: value_start..value_start + trimmed_value.len(),
            decoded_value: None,
        });
    }
    Ok((!fields.is_empty()).then_some(ParsedStructuredTextV1 {
        format: StructuredTextFormatV1::HttpHeaders,
        value: Value::Object(map),
        fields,
    }))
}

/// Parses a single absolute URL, exposing userinfo password and query
/// parameters as fields. Values keep their percent-encoded span so redaction
/// stays byte-exact, while `decoded_value` carries the decoded form detectors
/// must also inspect.
fn parse_url_document(
    text: &str,
) -> Result<Option<ParsedStructuredTextV1>, StructuredTextParseFailureV1> {
    let trimmed = text.trim();
    let url_intent = has_url_intent(trimmed);
    if trimmed.bytes().any(|byte| byte.is_ascii_whitespace()) || !url_intent {
        if url_intent {
            return Err(StructuredTextParseFailureV1::Malformed);
        }
        return Ok(None);
    }
    let offset = text.len() - text.trim_start().len();
    let url = Url::parse(trimmed).map_err(|_| StructuredTextParseFailureV1::Malformed)?;
    let host = url
        .host_str()
        .ok_or(StructuredTextParseFailureV1::Malformed)?;

    let mut map = Map::new();
    let mut fields = Vec::new();
    map.insert("scheme".to_owned(), Value::String(url.scheme().to_owned()));
    map.insert("host".to_owned(), Value::String(host.to_owned()));

    let before_fragment = trimmed.split_once('#').map_or(trimmed, |(head, _)| head);
    let (before_query, query) = before_fragment
        .split_once('?')
        .map_or((before_fragment, ""), |(head, tail)| (head, tail));

    if url.password().is_some() {
        let authority_start = before_query
            .find("://")
            .ok_or(StructuredTextParseFailureV1::Malformed)?
            + 3;
        let authority = &before_query[authority_start..];
        let authority_end = authority.find('/').unwrap_or(authority.len());
        let userinfo = &authority[..authority_end];
        if let Some(at) = userinfo.rfind('@')
            && let Some(separator) = userinfo[..at].find(':')
        {
            let start = offset + authority_start + separator + 1;
            let end = offset + authority_start + at;
            let raw = &trimmed[authority_start + separator + 1..authority_start + at];
            map.insert("password".to_owned(), Value::String(raw.to_owned()));
            fields.push(StructuredTextFieldV1 {
                key: "password".to_owned(),
                value_span: start..end,
                decoded_value: decoded_component(raw),
            });
        }
    }

    if !query.is_empty() {
        let mut cursor = offset + before_query.len() + 1;
        let mut pairs = Map::new();
        for pair in query.split('&') {
            let pair_start = cursor;
            cursor += pair.len() + 1;
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            let start = pair_start + key.len() + 1;
            let decoded_key = decode_url_key(key)?;
            pairs.insert(decoded_key.clone(), Value::String(value.to_owned()));
            fields.push(StructuredTextFieldV1 {
                key: decoded_key,
                value_span: start..start + value.len(),
                decoded_value: decoded_component(value),
            });
        }
        map.insert("query".to_owned(), Value::Object(pairs));
    }

    Ok((!fields.is_empty()).then_some(ParsedStructuredTextV1 {
        format: StructuredTextFormatV1::Url,
        value: Value::Object(map),
        fields,
    }))
}

fn has_url_intent(text: &str) -> bool {
    let Some((scheme, remainder)) = text.split_once(':') else {
        return false;
    };
    let scheme = scheme.trim().to_ascii_lowercase();
    matches!(
        scheme.as_str(),
        "http"
            | "https"
            | "ws"
            | "wss"
            | "ftp"
            | "file"
            | "postgres"
            | "postgresql"
            | "mysql"
            | "redis"
            | "mongodb"
    ) && remainder.trim_start().starts_with("//")
}

fn decoded_component(raw: &str) -> Option<String> {
    let decoded = percent_decode_str(raw).decode_utf8().ok()?.into_owned();
    (decoded != raw).then_some(decoded)
}

fn decode_url_key(raw: &str) -> Result<String, StructuredTextParseFailureV1> {
    percent_decode_str(raw)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| StructuredTextParseFailureV1::Malformed)
}

/// Bounds tree-shaped syntax before YAML or TOML deserialization. JSON has an
/// exact streaming preflight above; these formats do not expose equivalent
/// parser-native limits, so this conservative lexical guard uses the canonical
/// `ParseLimits` authority to stop excessive indentation, delimiter nesting,
/// and structural members before their parsers can recurse or expand values.
fn preflight_tree_document(text: &str) -> Result<(), StructuredTextParseFailureV1> {
    let policy = ParseLimits::default_policy();
    let mut delimiter_depth = 0usize;
    let mut structural_values = 1usize;

    for line in text.lines() {
        let content = line.trim_start_matches(' ');
        let indentation = line.len() - content.len();
        if !content.is_empty() && !content.starts_with('#') && indentation >= policy.depth {
            return Err(StructuredTextParseFailureV1::Malformed);
        }

        let mut quote = None;
        let mut escaped = false;
        for character in content.chars() {
            if let Some(delimiter) = quote {
                if character == delimiter && !escaped {
                    quote = None;
                }
                escaped = character == '\\' && !escaped;
                continue;
            }
            match character {
                '#' => break,
                '"' | '\'' => quote = Some(character),
                '{' | '[' | '(' => {
                    delimiter_depth = delimiter_depth.saturating_add(1);
                    structural_values = structural_values.saturating_add(1);
                }
                '}' | ']' | ')' => delimiter_depth = delimiter_depth.saturating_sub(1),
                ':' | '=' => structural_values = structural_values.saturating_add(2),
                ',' => structural_values = structural_values.saturating_add(1),
                '-' if content.starts_with('-') => {
                    structural_values = structural_values.saturating_add(1);
                }
                _ => {}
            }
            if delimiter_depth >= policy.depth || structural_values > policy.values {
                return Err(StructuredTextParseFailureV1::Malformed);
            }
        }
    }
    Ok(())
}

fn validate_expansion(
    value: &Value,
    limits: StructuredSanitizationLimits,
) -> Result<(), StructuredSanitizationError> {
    let expanded =
        serde_json::to_vec(value).map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if expanded.len() > limits.expanded_bytes {
        return Err(StructuredSanitizationError::ExpandedBytesExceeded);
    }

    let mut stack = vec![(value, 1usize)];
    let mut items = 0usize;
    while let Some((current, depth)) = stack.pop() {
        items = items.saturating_add(1);
        if items > limits.items {
            return Err(StructuredSanitizationError::ItemCountExceeded);
        }
        if depth > limits.depth {
            return Err(StructuredSanitizationError::NestingDepthExceeded);
        }
        match current {
            Value::Object(fields) => stack.extend(
                fields
                    .values()
                    .map(|child| (child, depth.saturating_add(1))),
            ),
            Value::Array(values) => {
                stack.extend(values.iter().map(|child| (child, depth.saturating_add(1))));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}
