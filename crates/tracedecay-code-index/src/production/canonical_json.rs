//! Byte-level canonical JSON rewriting for the partitioned generation codec.
//!
//! The shipped partitioned segment payload is the compact serialization of a
//! `serde_json::Value` tree: the encoder used to call `serde_json::to_value`,
//! substitute identity strings in place, reorder three artifact arrays, then
//! serialize the transformed tree again. Because this crate does not enable
//! `serde_json/preserve_order`, `serde_json::Map` is a `BTreeMap`, so the
//! canonical bytes carry **sorted object keys at every nesting level** rather
//! than Rust field-declaration order.
//!
//! Reproducing that DOM per file is what made generation encode allocate
//! gigabytes. This module emits the identical bytes from one direct
//! `serde_json::to_writer` serialization by rewriting it as a byte stream:
//! objects are re-emitted with their members sorted, identity strings are
//! substituted while they are copied, and the reordered arrays are sorted by
//! the same keys the DOM comparators used. Nothing but the input slice and the
//! output buffer is retained, so encode memory is proportional to one segment
//! rather than to one generation.

use std::borrow::Cow;
use std::ops::Range;

use super::CodeIndexProductionErrorV1;

/// Matches `serde_json`'s own default recursion limit so a hostile or damaged
/// payload is refused here exactly where the DOM decoder refused it before.
const MAXIMUM_CANONICAL_DEPTH_V1: usize = 128;

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// How one array's elements are ordered in the canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CanonicalArrayOrderV1 {
    /// Emit the elements in their serialized order.
    AsIs,
    /// Sort by one direct string member; a missing or non-string member sorts
    /// first, matching `Option<&str>` ordering in the replaced comparator.
    ByStringMember(&'static str),
    /// Sort by each element's own canonical encoding, matching the replaced
    /// `sort_by_cached_key(Value::to_string)` comparator.
    ByEncodedBytes,
}

/// The rewriting authority for one canonical pass.
pub(super) trait CanonicalPolicyV1 {
    /// Per-value identity classification. It is reset at every object member
    /// and inherited through arrays, exactly like the replaced DOM walks.
    type Field: Copy;

    fn root_field(&self) -> Self::Field;

    fn field_for_key(&self, key: &str) -> Self::Field;

    /// Write a replacement for one string value, or return `false` to copy the
    /// serialized string verbatim.
    fn rewrite_string(
        &mut self,
        field: Self::Field,
        value: &str,
        out: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1>;

    /// Object keys are re-sorted only where the shipped bytes were produced by
    /// serializing a `serde_json::Value`.
    fn sorts_object_keys(&self) -> bool;

    /// `path` holds the unquoted object keys enclosing this array, outermost
    /// first.
    fn array_order(&self, path: &[&[u8]]) -> CanonicalArrayOrderV1 {
        let _ = path;
        CanonicalArrayOrderV1::AsIs
    }
}

/// Rewrite `input` into `out` under `policy`.
pub(super) fn canonicalize_json_into<P: CanonicalPolicyV1>(
    input: &[u8],
    policy: &mut P,
    out: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let mut pass = CanonicalPassV1 {
        scan: JsonScanV1 { input, pos: 0 },
        policy,
        members: Vec::new(),
        path: Vec::new(),
        scratch: Vec::new(),
    };
    let field = pass.policy.root_field();
    pass.write_value(field, 0, out)?;
    pass.scan.skip_whitespace();
    if pass.scan.pos != input.len() {
        return Err(contract(
            "canonical json rewrite found trailing bytes after its value",
        ));
    }
    Ok(())
}

/// Visit every string in `input` with the identity classification its nearest
/// enclosing object key assigns, inherited through arrays. This replaces the
/// DOM walk that used to collect a file segment's symbol occurrences: it reads
/// the same serialized bytes the writer will rewrite, so the collected set is
/// identical without materializing a `serde_json::Value`.
pub(super) fn visit_json_strings<'a, F, P, V>(
    input: &'a [u8],
    root_field: P,
    field_for_key: &F,
    visit: &mut V,
) -> Result<(), CodeIndexProductionErrorV1>
where
    F: Fn(&str) -> P,
    P: Copy,
    V: FnMut(P, Cow<'a, str>) -> Result<(), CodeIndexProductionErrorV1>,
{
    let mut scan = JsonScanV1 { input, pos: 0 };
    visit_json_strings_at(&mut scan, root_field, 0, field_for_key, visit)?;
    scan.skip_whitespace();
    if scan.pos != input.len() {
        return Err(contract(
            "canonical json rewrite found trailing bytes after its value",
        ));
    }
    Ok(())
}

fn visit_json_strings_at<'a, F, P, V>(
    scan: &mut JsonScanV1<'a>,
    field: P,
    depth: usize,
    field_for_key: &F,
    visit: &mut V,
) -> Result<(), CodeIndexProductionErrorV1>
where
    F: Fn(&str) -> P,
    P: Copy,
    V: FnMut(P, Cow<'a, str>) -> Result<(), CodeIndexProductionErrorV1>,
{
    if depth > MAXIMUM_CANONICAL_DEPTH_V1 {
        return Err(contract(
            "canonical json rewrite exceeded its nesting limit",
        ));
    }
    match scan.peek()? {
        b'"' => {
            let raw = scan.read_string_raw()?;
            visit(field, unescape_json_string(raw)?)?;
        }
        b'{' => {
            scan.pos += 1;
            if scan.peek()? == b'}' {
                scan.pos += 1;
                return Ok(());
            }
            loop {
                let key_raw = scan.read_string_raw()?;
                let member = field_for_key(unescape_json_string(key_raw)?.as_ref());
                scan.expect(b':')?;
                visit_json_strings_at(scan, member, depth + 1, field_for_key, visit)?;
                if !scan.step_container(b'}')? {
                    break;
                }
            }
        }
        b'[' => {
            scan.pos += 1;
            if scan.peek()? == b']' {
                scan.pos += 1;
                return Ok(());
            }
            loop {
                visit_json_strings_at(scan, field, depth + 1, field_for_key, visit)?;
                if !scan.step_container(b']')? {
                    break;
                }
            }
        }
        _ => scan.skip_value(depth)?,
    }
    Ok(())
}

/// Escape and write one string exactly as `serde_json` would.
pub(super) fn write_json_string(
    value: &str,
    out: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    serde_json::to_writer(&mut *out, value)
        .map_err(|error| contract(&format!("canonical json string write failed: {error}")))
}

fn unescape_json_string(raw: &[u8]) -> Result<Cow<'_, str>, CodeIndexProductionErrorV1> {
    let inner = raw
        .len()
        .checked_sub(1)
        .and_then(|end| raw.get(1..end))
        .ok_or_else(|| contract("canonical json rewrite found a malformed string"))?;
    if !inner.contains(&b'\\') {
        return std::str::from_utf8(inner)
            .map(Cow::Borrowed)
            .map_err(|_| contract("canonical json rewrite found a non-UTF-8 string"));
    }
    serde_json::from_slice::<String>(raw)
        .map(Cow::Owned)
        .map_err(|_| contract("canonical json rewrite found an invalid string escape"))
}

struct JsonScanV1<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanV1<'a> {
    fn skip_whitespace(&mut self) {
        while matches!(self.input.get(self.pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Result<u8, CodeIndexProductionErrorV1> {
        self.skip_whitespace();
        self.input
            .get(self.pos)
            .copied()
            .ok_or_else(|| contract("canonical json rewrite reached the end of its input"))
    }

    fn expect(&mut self, byte: u8) -> Result<(), CodeIndexProductionErrorV1> {
        if self.peek()? != byte {
            return Err(contract("canonical json rewrite found an unexpected token"));
        }
        self.pos += 1;
        Ok(())
    }

    /// Returns the raw string slice including both quotes.
    fn read_string_raw(&mut self) -> Result<&'a [u8], CodeIndexProductionErrorV1> {
        if self.peek()? != b'"' {
            return Err(contract("canonical json rewrite expected a string"));
        }
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.input.get(self.pos) {
                None => {
                    return Err(contract(
                        "canonical json rewrite found an unterminated string",
                    ));
                }
                // A `\uXXXX` payload never contains a quote or a backslash, so
                // skipping the escaped byte is enough to find the terminator.
                Some(b'\\') => self.pos += 2,
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => self.pos += 1,
            }
        }
        Ok(&self.input[start..self.pos])
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), CodeIndexProductionErrorV1> {
        if depth > MAXIMUM_CANONICAL_DEPTH_V1 {
            return Err(contract(
                "canonical json rewrite exceeded its nesting limit",
            ));
        }
        match self.peek()? {
            b'"' => {
                self.read_string_raw()?;
            }
            b'{' => {
                self.pos += 1;
                if self.peek()? == b'}' {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.read_string_raw()?;
                    self.expect(b':')?;
                    self.skip_value(depth + 1)?;
                    if !self.step_container(b'}')? {
                        break;
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                if self.peek()? == b']' {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    if !self.step_container(b']')? {
                        break;
                    }
                }
            }
            _ => {
                let start = self.pos;
                while self.input.get(self.pos).is_some_and(|byte| {
                    !matches!(byte, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
                }) {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(contract("canonical json rewrite found an empty scalar"));
                }
            }
        }
        Ok(())
    }

    /// Consumes one `,` and reports `true`, or consumes `close` and reports
    /// `false`.
    fn step_container(&mut self, close: u8) -> Result<bool, CodeIndexProductionErrorV1> {
        let byte = self.peek()?;
        if byte == b',' {
            self.pos += 1;
            return Ok(true);
        }
        if byte == close {
            self.pos += 1;
            return Ok(false);
        }
        Err(contract(
            "canonical json rewrite found an unterminated container",
        ))
    }
}

/// The direct string member `name` of the object starting at `start`, or
/// `None` when it is absent or is not a string.
fn direct_string_member<'a>(
    input: &'a [u8],
    start: usize,
    name: &str,
) -> Result<Option<Cow<'a, str>>, CodeIndexProductionErrorV1> {
    let mut scan = JsonScanV1 { input, pos: start };
    if scan.peek()? != b'{' {
        return Ok(None);
    }
    scan.pos += 1;
    if scan.peek()? == b'}' {
        return Ok(None);
    }
    loop {
        let key_raw = scan.read_string_raw()?;
        scan.expect(b':')?;
        let matched = unescape_json_string(key_raw)? == name;
        if matched && scan.peek()? == b'"' {
            let value_raw = scan.read_string_raw()?;
            return unescape_json_string(value_raw).map(Some);
        }
        if matched {
            return Ok(None);
        }
        scan.skip_value(0)?;
        if !scan.step_container(b'}')? {
            return Ok(None);
        }
    }
}

struct ObjectMemberV1<'a> {
    key_text: Cow<'a, str>,
    key_raw: &'a [u8],
    value_start: usize,
}

struct CanonicalPassV1<'a, 'p, P: CanonicalPolicyV1> {
    scan: JsonScanV1<'a>,
    policy: &'p mut P,
    /// One shared, mark-disciplined member stack: every nested object appends
    /// its own members and truncates back, so a whole segment costs the growth
    /// of one vector instead of a map allocation per object.
    members: Vec<ObjectMemberV1<'a>>,
    path: Vec<&'a [u8]>,
    scratch: Vec<u8>,
}

impl<'a, P: CanonicalPolicyV1> CanonicalPassV1<'a, '_, P> {
    fn write_value(
        &mut self,
        field: P::Field,
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if depth > MAXIMUM_CANONICAL_DEPTH_V1 {
            return Err(contract(
                "canonical json rewrite exceeded its nesting limit",
            ));
        }
        match self.scan.peek()? {
            b'"' => {
                let raw = self.scan.read_string_raw()?;
                let text = unescape_json_string(raw)?;
                if !self.policy.rewrite_string(field, text.as_ref(), out)? {
                    out.extend_from_slice(raw);
                }
                Ok(())
            }
            b'{' => self.write_object(depth, out),
            b'[' => self.write_array(field, depth, out),
            _ => {
                let start = self.scan.pos;
                self.scan.skip_value(depth)?;
                out.extend_from_slice(&self.scan.input[start..self.scan.pos]);
                Ok(())
            }
        }
    }

    fn write_object(
        &mut self,
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        self.scan.expect(b'{')?;
        out.push(b'{');
        if self.scan.peek()? == b'}' {
            self.scan.pos += 1;
            out.push(b'}');
            return Ok(());
        }
        if !self.policy.sorts_object_keys() {
            let mut first = true;
            loop {
                let key_raw = self.scan.read_string_raw()?;
                self.scan.expect(b':')?;
                let field = self
                    .policy
                    .field_for_key(unescape_json_string(key_raw)?.as_ref());
                if !first {
                    out.push(b',');
                }
                first = false;
                out.extend_from_slice(key_raw);
                out.push(b':');
                self.path.push(key_content(key_raw)?);
                let written = self.write_value(field, depth + 1, out);
                self.path.pop();
                written?;
                if !self.scan.step_container(b'}')? {
                    break;
                }
            }
            out.push(b'}');
            return Ok(());
        }
        let mark = self.members.len();
        loop {
            let key_raw = self.scan.read_string_raw()?;
            let key_text = unescape_json_string(key_raw)?;
            self.scan.expect(b':')?;
            self.scan.skip_whitespace();
            let value_start = self.scan.pos;
            self.scan.skip_value(depth)?;
            self.members.push(ObjectMemberV1 {
                key_text,
                key_raw,
                value_start,
            });
            if !self.scan.step_container(b'}')? {
                break;
            }
        }
        let end = self.members.len();
        self.members[mark..end].sort_by(|left, right| left.key_text.cmp(&right.key_text));
        let resume = self.scan.pos;
        for index in mark..end {
            let key_raw = self.members[index].key_raw;
            let value_start = self.members[index].value_start;
            let field = self
                .policy
                .field_for_key(self.members[index].key_text.as_ref());
            if index > mark {
                out.push(b',');
            }
            out.extend_from_slice(key_raw);
            out.push(b':');
            self.scan.pos = value_start;
            self.path.push(key_content(key_raw)?);
            let written = self.write_value(field, depth + 1, out);
            self.path.pop();
            written?;
        }
        self.members.truncate(mark);
        self.scan.pos = resume;
        out.push(b'}');
        Ok(())
    }

    fn write_array(
        &mut self,
        field: P::Field,
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let order = self.policy.array_order(&self.path);
        self.scan.expect(b'[')?;
        out.push(b'[');
        if self.scan.peek()? == b']' {
            self.scan.pos += 1;
            out.push(b']');
            return Ok(());
        }
        match order {
            CanonicalArrayOrderV1::AsIs => {
                let mut first = true;
                loop {
                    if !first {
                        out.push(b',');
                    }
                    first = false;
                    self.write_value(field, depth + 1, out)?;
                    if !self.scan.step_container(b']')? {
                        break;
                    }
                }
            }
            CanonicalArrayOrderV1::ByStringMember(name) => {
                let mut elements: Vec<(Option<Cow<'a, str>>, usize)> = Vec::new();
                loop {
                    self.scan.skip_whitespace();
                    let start = self.scan.pos;
                    self.scan.skip_value(depth)?;
                    let key = direct_string_member(self.scan.input, start, name)?;
                    elements.push((key, start));
                    if !self.scan.step_container(b']')? {
                        break;
                    }
                }
                let resume = self.scan.pos;
                elements.sort_by(|left, right| left.0.cmp(&right.0));
                for (index, (_, start)) in elements.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    self.scan.pos = *start;
                    self.write_value(field, depth + 1, out)?;
                }
                self.scan.pos = resume;
            }
            CanonicalArrayOrderV1::ByEncodedBytes => {
                let mut scratch = std::mem::take(&mut self.scratch);
                scratch.clear();
                let mut ranges: Vec<Range<usize>> = Vec::new();
                let encoded = self.encode_elements(field, depth, &mut scratch, &mut ranges);
                self.scratch = scratch;
                encoded?;
                ranges.sort_by(|left, right| {
                    self.scratch[left.clone()].cmp(&self.scratch[right.clone()])
                });
                for (index, range) in ranges.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(&self.scratch[range.clone()]);
                }
            }
        }
        out.push(b']');
        Ok(())
    }

    fn encode_elements(
        &mut self,
        field: P::Field,
        depth: usize,
        scratch: &mut Vec<u8>,
        ranges: &mut Vec<Range<usize>>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        loop {
            let start = scratch.len();
            self.write_value(field, depth + 1, scratch)?;
            ranges.push(start..scratch.len());
            if !self.scan.step_container(b']')? {
                return Ok(());
            }
        }
    }
}

fn key_content(key_raw: &[u8]) -> Result<&[u8], CodeIndexProductionErrorV1> {
    key_raw
        .len()
        .checked_sub(1)
        .and_then(|end| key_raw.get(1..end))
        .ok_or_else(|| contract("canonical json rewrite found a malformed object key"))
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::Value;

    use super::*;

    #[derive(Clone, Copy)]
    struct SortingPolicy;

    impl CanonicalPolicyV1 for SortingPolicy {
        type Field = ();

        fn root_field(&self) -> Self::Field {}

        fn field_for_key(&self, _key: &str) -> Self::Field {}

        fn rewrite_string(
            &mut self,
            _field: Self::Field,
            _value: &str,
            _out: &mut Vec<u8>,
        ) -> Result<bool, CodeIndexProductionErrorV1> {
            Ok(false)
        }

        fn sorts_object_keys(&self) -> bool {
            true
        }
    }

    #[derive(Serialize)]
    struct Unsorted {
        zulu: u32,
        alpha: Nested,
        mike: Vec<Value>,
        #[serde(rename = "with\"quote")]
        quoted: String,
        empty_object: serde_json::Map<String, Value>,
        empty_array: Vec<u32>,
    }

    #[derive(Serialize)]
    struct Nested {
        second: Option<u8>,
        first: bool,
        third: f64,
    }

    fn reference(value: &impl Serialize) -> Vec<u8> {
        serde_json::to_vec(&serde_json::to_value(value).expect("reference value"))
            .expect("reference bytes")
    }

    fn streamed(value: &impl Serialize) -> Vec<u8> {
        let mut payload = Vec::new();
        serde_json::to_writer(&mut payload, value).expect("direct serialization");
        let mut out = Vec::new();
        canonicalize_json_into(&payload, &mut SortingPolicy, &mut out).expect("canonical rewrite");
        out
    }

    #[test]
    fn streaming_rewrite_matches_the_value_dom_key_order() {
        let value = Unsorted {
            zulu: 1,
            alpha: Nested {
                second: None,
                first: true,
                third: 1.5,
            },
            mike: vec![
                serde_json::json!({"beta": 1, "alpha": [1, 2, {"z": "y", "a": "b"}]}),
                Value::Null,
                Value::String("tab\t\"quote\"\u{1}".to_owned()),
            ],
            quoted: "unicode \u{2603} and \\ backslash".to_owned(),
            empty_object: serde_json::Map::new(),
            empty_array: Vec::new(),
        };

        assert_eq!(streamed(&value), reference(&value));
    }

    #[test]
    fn streaming_rewrite_preserves_escaped_keys_and_numbers() {
        #[derive(Serialize)]
        struct Numbers {
            big: u64,
            negative: i64,
            fraction: f64,
            exponent: f64,
        }
        let value = Numbers {
            big: u64::MAX,
            negative: i64::MIN,
            fraction: 0.1,
            exponent: 1e-300,
        };

        assert_eq!(streamed(&value), reference(&value));
    }

    #[test]
    fn streaming_rewrite_refuses_trailing_bytes() {
        let mut out = Vec::new();

        let error = canonicalize_json_into(b"{}{}", &mut SortingPolicy, &mut out)
            .expect_err("trailing bytes must be refused");

        assert!(error.to_string().contains("trailing bytes"));
    }

    #[test]
    fn direct_string_member_reads_only_string_values() {
        let object = br#"{"identity":"alpha","other":{"identity":"nested"},"number":3}"#;

        assert_eq!(
            direct_string_member(object, 0, "identity").expect("member"),
            Some(Cow::Borrowed("alpha"))
        );
        assert_eq!(
            direct_string_member(object, 0, "number").expect("member"),
            None
        );
        assert_eq!(
            direct_string_member(object, 0, "missing").expect("member"),
            None
        );
    }
}
