use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::error::DomainError;
use super::id::ManifestDigest;

/// Serialize any domain value to the crate's canonical JSON byte form.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    let mut output = Vec::new();
    serialize_canonical(value, &mut output)?;
    Ok(output)
}

/// Serialize a JSON value with recursively lexicographic object keys and no
/// insignificant whitespace.
pub fn canonical_json_value(value: &Value) -> Result<String, DomainError> {
    let mut output = String::new();
    write_canonical(value, &mut output);
    Ok(output)
}

/// Compute the canonical SHA-256 digest encoding used by domain manifests.
///
/// The value is streamed straight into the hasher through a buffered sink; no
/// intermediate `serde_json::Value` tree is materialized, which matters for
/// the six-figure element sets the code index digests on every publish.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<ManifestDigest, DomainError> {
    let mut sink = BufferedSink::new(Sha256::new());
    serialize_canonical(value, &mut sink)?;
    let digest = sink.finish().finalize();
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
    }
    ManifestDigest::new(encoded)
}

/// Stream `value` into `sink` in canonical JSON form.
fn serialize_canonical<T, S>(value: &T, sink: &mut S) -> Result<(), DomainError>
where
    T: Serialize + ?Sized,
    S: CanonicalSink,
{
    value
        .serialize(CanonicalSerializer { sink })
        .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))
}

trait CanonicalSink {
    fn write(&mut self, chunk: &str);
}

/// How much canonical text accumulates before it reaches the wrapped sink.
///
/// Canonical writing emits many one-byte chunks (`"`, `:`, `,`); handing each
/// of those to `Sha256` pays block-buffer bookkeeping per call, so the hashing
/// path batches them here first.
const SINK_BUFFER_CAPACITY: usize = 64 * 1024;

/// A [`CanonicalSink`] that batches small writes before forwarding them.
struct BufferedSink<S: CanonicalSink> {
    inner: S,
    buffer: String,
}

impl<S: CanonicalSink> BufferedSink<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::with_capacity(SINK_BUFFER_CAPACITY),
        }
    }

    fn flush(&mut self) {
        if !self.buffer.is_empty() {
            self.inner.write(&self.buffer);
            self.buffer.clear();
        }
    }

    /// Flush every buffered byte and return the wrapped sink.
    fn finish(mut self) -> S {
        self.flush();
        self.inner
    }
}

impl<S: CanonicalSink> CanonicalSink for BufferedSink<S> {
    fn write(&mut self, chunk: &str) {
        if self.buffer.len() + chunk.len() > SINK_BUFFER_CAPACITY {
            self.flush();
            if chunk.len() >= SINK_BUFFER_CAPACITY {
                self.inner.write(chunk);
                return;
            }
        }
        self.buffer.push_str(chunk);
    }
}

impl CanonicalSink for String {
    fn write(&mut self, chunk: &str) {
        self.push_str(chunk);
    }
}

impl CanonicalSink for Vec<u8> {
    fn write(&mut self, chunk: &str) {
        self.extend_from_slice(chunk.as_bytes());
    }
}

impl CanonicalSink for Sha256 {
    fn write(&mut self, chunk: &str) {
        Digest::update(self, chunk.as_bytes());
    }
}

/// The JSON escape `serde_json`'s compact formatter emits for each control
/// byte. Mirroring the table here lets canonical writing stream escapes
/// straight into the sink instead of allocating a `String` per string value.
static CONTROL_ESCAPES: [&str; 32] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007", "\\b",
    "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011", "\\u0012",
    "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019", "\\u001a",
    "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

/// Write one JSON string literal (quotes included) directly into the sink.
///
/// Byte-for-byte equivalent to `serde_json::to_string(value)` for a string:
/// only `"`, `\`, and the C0 control bytes are escaped, non-ASCII is passed
/// through as UTF-8, and `\u00xx` escapes use lowercase hex.
fn write_json_string(value: &str, output: &mut impl CanonicalSink) {
    output.write("\"");
    let mut run_start = 0usize;
    for (index, byte) in value.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x00..=0x1f => CONTROL_ESCAPES[byte as usize],
            _ => continue,
        };
        if run_start < index {
            // Every escaped byte is ASCII, so both ends are char boundaries.
            output.write(&value[run_start..index]);
        }
        output.write(escape);
        run_start = index + 1;
    }
    if run_start < value.len() {
        output.write(&value[run_start..]);
    }
    output.write("\"");
}

/// Write a JSON number without allocating for the common integral cases.
///
/// `serde_json::Number`'s `Display` renders `u64`/`i64` payloads as plain
/// decimal, so the stack-formatted digits are identical; anything else (float
/// payloads) falls back to the owned rendering.
fn write_json_number(number: &serde_json::Number, output: &mut impl CanonicalSink) {
    if let Some(value) = number.as_u64() {
        write_u64(value, output);
    } else if let Some(value) = number.as_i64().filter(|value| *value < 0) {
        output.write("-");
        write_u64(value.unsigned_abs(), output);
    } else {
        output.write(&number.to_string());
    }
}

fn write_u64(value: u64, output: &mut impl CanonicalSink) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    let mut remaining = value;
    loop {
        index -= 1;
        buffer[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    match std::str::from_utf8(&buffer[index..]) {
        Ok(digits) => output.write(digits),
        // Unreachable: the buffer holds ASCII digits by construction.
        Err(_) => output.write(&value.to_string()),
    }
}

/// Whether a JSON object's keys already arrive in canonical (byte-lexicographic)
/// order, in which case the entries need not be collected and sorted.
fn keys_are_canonically_ordered(values: &serde_json::Map<String, Value>) -> bool {
    let mut previous: Option<&str> = None;
    for key in values.keys() {
        if previous.is_some_and(|previous| previous > key.as_str()) {
            return false;
        }
        previous = Some(key);
    }
    true
}

fn write_canonical(value: &Value, output: &mut impl CanonicalSink) {
    match value {
        Value::Null => output.write("null"),
        Value::Bool(value) => output.write(if *value { "true" } else { "false" }),
        Value::Number(value) => write_json_number(value, output),
        Value::String(value) => write_json_string(value, output),
        Value::Array(values) => {
            output.write("[");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.write(",");
                }
                write_canonical(value, output);
            }
            output.write("]");
        }
        Value::Object(values) => {
            output.write("{");
            if keys_are_canonically_ordered(values) {
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.write(",");
                    }
                    write_json_string(key, output);
                    output.write(":");
                    write_canonical(value, output);
                }
            } else {
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        output.write(",");
                    }
                    write_json_string(key, output);
                    output.write(":");
                    write_canonical(value, output);
                }
            }
            output.write("}");
        }
    }
}

/// `serde_json`'s private struct tokens (`RawValue`, and the
/// arbitrary-precision `Number`) carry payloads that only `serde_json`'s own
/// value serializer knows how to decode. Streaming cannot reproduce them, so
/// those subtrees fall back to materializing a `Value`.
const SERDE_JSON_PRIVATE_TOKEN_PREFIX: &str = "$serde_json::private::";

type CanonicalError = serde_json::Error;
type CanonicalResult<T = ()> = Result<T, CanonicalError>;

fn key_must_be_a_string() -> CanonicalError {
    serde::ser::Error::custom("key must be a string")
}

fn number_out_of_range() -> CanonicalError {
    serde::ser::Error::custom("number out of range")
}

/// A `serde::Serializer` that writes canonical JSON straight into a
/// [`CanonicalSink`].
///
/// Output is byte-identical to `serde_json::to_value` followed by
/// [`write_canonical`], but no whole-document `Value` tree is built: scalars,
/// arrays, and single-key variant wrappers stream directly, and only the
/// entries of the object currently being written are buffered so their keys
/// can be emitted in lexicographic order.
struct CanonicalSerializer<'sink, S: CanonicalSink> {
    sink: &'sink mut S,
}

impl<'sink, S: CanonicalSink> serde::Serializer for CanonicalSerializer<'sink, S> {
    type Ok = ();
    type Error = CanonicalError;

    type SerializeSeq = SeqWriter<'sink, S>;
    type SerializeTuple = SeqWriter<'sink, S>;
    type SerializeTupleStruct = SeqWriter<'sink, S>;
    type SerializeTupleVariant = SeqWriter<'sink, S>;
    type SerializeMap = ObjectWriter<'sink, S>;
    type SerializeStruct = StructWriter<'sink, S>;
    type SerializeStructVariant = ObjectWriter<'sink, S>;

    fn serialize_bool(self, value: bool) -> CanonicalResult {
        self.sink.write(if value { "true" } else { "false" });
        Ok(())
    }

    fn serialize_i8(self, value: i8) -> CanonicalResult {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> CanonicalResult {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> CanonicalResult {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> CanonicalResult {
        write_i64(value, self.sink);
        Ok(())
    }

    fn serialize_i128(self, value: i128) -> CanonicalResult {
        // `serde_json::to_value` narrows to `u64`, then `i64`, then rejects.
        if let Ok(value) = u64::try_from(value) {
            write_u64(value, self.sink);
            Ok(())
        } else if let Ok(value) = i64::try_from(value) {
            write_i64(value, self.sink);
            Ok(())
        } else {
            Err(number_out_of_range())
        }
    }

    fn serialize_u8(self, value: u8) -> CanonicalResult {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> CanonicalResult {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> CanonicalResult {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> CanonicalResult {
        write_u64(value, self.sink);
        Ok(())
    }

    fn serialize_u128(self, value: u128) -> CanonicalResult {
        match u64::try_from(value) {
            Ok(value) => {
                write_u64(value, self.sink);
                Ok(())
            }
            Err(_) => Err(number_out_of_range()),
        }
    }

    fn serialize_f32(self, value: f32) -> CanonicalResult {
        // `Number::from_f32` stores `value as f64`, so widening first keeps
        // the rendering identical.
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> CanonicalResult {
        write_f64(value, self.sink);
        Ok(())
    }

    fn serialize_char(self, value: char) -> CanonicalResult {
        let mut buffer = [0u8; 4];
        write_json_string(value.encode_utf8(&mut buffer), self.sink);
        Ok(())
    }

    fn serialize_str(self, value: &str) -> CanonicalResult {
        write_json_string(value, self.sink);
        Ok(())
    }

    fn serialize_bytes(self, value: &[u8]) -> CanonicalResult {
        // `to_value` renders bytes as an array of numbers.
        self.sink.write("[");
        for (index, byte) in value.iter().enumerate() {
            if index > 0 {
                self.sink.write(",");
            }
            write_u64(u64::from(*byte), self.sink);
        }
        self.sink.write("]");
        Ok(())
    }

    fn serialize_none(self) -> CanonicalResult {
        self.serialize_unit()
    }

    fn serialize_some<T>(self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> CanonicalResult {
        self.sink.write("null");
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> CanonicalResult {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> CanonicalResult {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        // A single-key object needs no reordering, so it streams.
        self.sink.write("{");
        write_json_string(variant, self.sink);
        self.sink.write(":");
        value.serialize(CanonicalSerializer { sink: self.sink })?;
        self.sink.write("}");
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> CanonicalResult<Self::SerializeSeq> {
        self.sink.write("[");
        Ok(SeqWriter {
            sink: self.sink,
            first: true,
            close: "]",
        })
    }

    fn serialize_tuple(self, len: usize) -> CanonicalResult<Self::SerializeTuple> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> CanonicalResult<Self::SerializeTupleStruct> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> CanonicalResult<Self::SerializeTupleVariant> {
        self.sink.write("{");
        write_json_string(variant, self.sink);
        self.sink.write(":[");
        Ok(SeqWriter {
            sink: self.sink,
            first: true,
            close: "]}",
        })
    }

    fn serialize_map(self, len: Option<usize>) -> CanonicalResult<Self::SerializeMap> {
        Ok(ObjectWriter::new(self.sink, len.unwrap_or(0), "{", "}"))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> CanonicalResult<Self::SerializeStruct> {
        if name.starts_with(SERDE_JSON_PRIVATE_TOKEN_PREFIX) {
            return Ok(StructWriter::Delegated {
                sink: self.sink,
                inner: serde::Serializer::serialize_struct(
                    serde_json::value::Serializer,
                    name,
                    len,
                )?,
            });
        }
        Ok(StructWriter::Object(ObjectWriter::new(
            self.sink, len, "{", "}",
        )))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> CanonicalResult<Self::SerializeStructVariant> {
        let mut prefix = String::with_capacity(variant.len() + 4);
        prefix.push('{');
        write_json_string(variant, &mut prefix);
        prefix.push_str(":{");
        self.sink.write(&prefix);
        Ok(ObjectWriter::new(self.sink, len, "", "}}"))
    }

    fn collect_str<T>(self, value: &T) -> CanonicalResult
    where
        T: ?Sized + std::fmt::Display,
    {
        write_json_string(&value.to_string(), self.sink);
        Ok(())
    }
}

fn write_i64(value: i64, output: &mut impl CanonicalSink) {
    if value < 0 {
        output.write("-");
        write_u64(value.unsigned_abs(), output);
    } else {
        write_u64(value.unsigned_abs(), output);
    }
}

/// Render an `f64` exactly as `to_value` would: non-finite floats become
/// `null`, finite floats take `serde_json::Number`'s own rendering.
fn write_f64(value: f64, output: &mut impl CanonicalSink) {
    match serde_json::Number::from_f64(value) {
        Some(number) => write_json_number(&number, output),
        None => output.write("null"),
    }
}

/// Streams array elements straight through; arrays never reorder.
struct SeqWriter<'sink, S: CanonicalSink> {
    sink: &'sink mut S,
    first: bool,
    close: &'static str,
}

impl<S: CanonicalSink> SeqWriter<'_, S> {
    fn element<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        if self.first {
            self.first = false;
        } else {
            self.sink.write(",");
        }
        value.serialize(CanonicalSerializer { sink: self.sink })
    }

    fn finish(self) -> CanonicalResult {
        self.sink.write(self.close);
        Ok(())
    }
}

impl<S: CanonicalSink> serde::ser::SerializeSeq for SeqWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_element<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

impl<S: CanonicalSink> serde::ser::SerializeTuple for SeqWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_element<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

impl<S: CanonicalSink> serde::ser::SerializeTupleStruct for SeqWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_field<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

impl<S: CanonicalSink> serde::ser::SerializeTupleVariant for SeqWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_field<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

/// Buffers one object's entries so its keys can be emitted in lexicographic
/// order.
///
/// Only the entries of the object currently being written are held; nested
/// values stream into their own entry buffer, so the cost is the size of one
/// object rather than of the whole document.
struct ObjectWriter<'sink, S: CanonicalSink> {
    sink: &'sink mut S,
    entries: Vec<(String, String)>,
    pending_key: Option<String>,
    /// Written before the first entry (empty when the caller already opened
    /// the brace, as struct variants do).
    open: &'static str,
    close: &'static str,
}

impl<'sink, S: CanonicalSink> ObjectWriter<'sink, S> {
    fn new(sink: &'sink mut S, len: usize, open: &'static str, close: &'static str) -> Self {
        Self {
            sink,
            entries: Vec::with_capacity(len),
            pending_key: None,
            open,
            close,
        }
    }

    fn push_entry<T>(&mut self, key: String, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        let mut buffer = String::new();
        value.serialize(CanonicalSerializer { sink: &mut buffer })?;
        self.entries.push((key, buffer));
        Ok(())
    }

    fn finish(mut self) -> CanonicalResult {
        let already_sorted = self.entries.windows(2).all(|pair| pair[0].0 <= pair[1].0);
        if !already_sorted {
            // Stable so that duplicate keys keep insertion order, matching
            // `serde_json::Map::insert`'s last-write-wins semantics below.
            self.entries.sort_by(|left, right| left.0.cmp(&right.0));
        }
        self.sink.write(self.open);
        let mut wrote_entry = false;
        for index in 0..self.entries.len() {
            // A duplicate key keeps only its last value, exactly as repeated
            // `Map::insert` calls would.
            if self
                .entries
                .get(index + 1)
                .is_some_and(|next| next.0 == self.entries[index].0)
            {
                continue;
            }
            if wrote_entry {
                self.sink.write(",");
            }
            wrote_entry = true;
            let (key, value) = &self.entries[index];
            write_json_string(key, self.sink);
            self.sink.write(":");
            self.sink.write(value);
        }
        self.sink.write(self.close);
        Ok(())
    }
}

impl<S: CanonicalSink> serde::ser::SerializeMap for ObjectWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_key<T>(&mut self, key: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.pending_key = Some(key.serialize(MapKeySerializer)?);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        let key = self
            .pending_key
            .take()
            .expect("serialize_value called before serialize_key");
        self.push_entry(key, value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

impl<S: CanonicalSink> serde::ser::SerializeStructVariant for ObjectWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        self.push_entry(key.to_owned(), value)
    }

    fn end(self) -> CanonicalResult {
        self.finish()
    }
}

/// Structs stream like maps, except for `serde_json`'s private tokens.
enum StructWriter<'sink, S: CanonicalSink> {
    Object(ObjectWriter<'sink, S>),
    Delegated {
        sink: &'sink mut S,
        inner: <serde_json::value::Serializer as serde::Serializer>::SerializeStruct,
    },
}

impl<S: CanonicalSink> serde::ser::SerializeStruct for StructWriter<'_, S> {
    type Ok = ();
    type Error = CanonicalError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> CanonicalResult
    where
        T: ?Sized + Serialize,
    {
        match self {
            Self::Object(object) => object.push_entry(key.to_owned(), value),
            Self::Delegated { inner, .. } => {
                serde::ser::SerializeStruct::serialize_field(inner, key, value)
            }
        }
    }

    fn end(self) -> CanonicalResult {
        match self {
            Self::Object(object) => object.finish(),
            Self::Delegated { sink, inner } => {
                let value = serde::ser::SerializeStruct::end(inner)?;
                write_canonical(&value, sink);
                Ok(())
            }
        }
    }
}

/// Renders a map key to the exact `String` `serde_json`'s own map-key
/// serializer would produce, and rejects the same key shapes it rejects.
struct MapKeySerializer;

impl serde::Serializer for MapKeySerializer {
    type Ok = String;
    type Error = CanonicalError;

    type SerializeSeq = serde::ser::Impossible<String, CanonicalError>;
    type SerializeTuple = serde::ser::Impossible<String, CanonicalError>;
    type SerializeTupleStruct = serde::ser::Impossible<String, CanonicalError>;
    type SerializeTupleVariant = serde::ser::Impossible<String, CanonicalError>;
    type SerializeMap = serde::ser::Impossible<String, CanonicalError>;
    type SerializeStruct = serde::ser::Impossible<String, CanonicalError>;
    type SerializeStructVariant = serde::ser::Impossible<String, CanonicalError>;

    fn serialize_bool(self, value: bool) -> CanonicalResult<String> {
        Ok(if value { "true" } else { "false" }.to_owned())
    }

    fn serialize_i8(self, value: i8) -> CanonicalResult<String> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> CanonicalResult<String> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> CanonicalResult<String> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> CanonicalResult<String> {
        let mut key = String::new();
        write_i64(value, &mut key);
        Ok(key)
    }

    fn serialize_i128(self, value: i128) -> CanonicalResult<String> {
        Ok(value.to_string())
    }

    fn serialize_u8(self, value: u8) -> CanonicalResult<String> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> CanonicalResult<String> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> CanonicalResult<String> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> CanonicalResult<String> {
        let mut key = String::new();
        write_u64(value, &mut key);
        Ok(key)
    }

    fn serialize_u128(self, value: u128) -> CanonicalResult<String> {
        Ok(value.to_string())
    }

    fn serialize_f32(self, value: f32) -> CanonicalResult<String> {
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> CanonicalResult<String> {
        // `Number`'s rendering of a finite float is the same formatter
        // `serde_json`'s map-key serializer uses.
        serde_json::Number::from_f64(value)
            .map(|number| number.to_string())
            .ok_or_else(|| serde::ser::Error::custom("float key must be finite"))
    }

    fn serialize_char(self, value: char) -> CanonicalResult<String> {
        Ok(value.to_string())
    }

    fn serialize_str(self, value: &str) -> CanonicalResult<String> {
        Ok(value.to_owned())
    }

    fn serialize_bytes(self, _value: &[u8]) -> CanonicalResult<String> {
        Err(key_must_be_a_string())
    }

    fn serialize_none(self) -> CanonicalResult<String> {
        Err(key_must_be_a_string())
    }

    fn serialize_some<T>(self, _value: &T) -> CanonicalResult<String>
    where
        T: ?Sized + Serialize,
    {
        Err(key_must_be_a_string())
    }

    fn serialize_unit(self) -> CanonicalResult<String> {
        Err(key_must_be_a_string())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> CanonicalResult<String> {
        Err(key_must_be_a_string())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> CanonicalResult<String> {
        Ok(variant.to_owned())
    }

    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> CanonicalResult<String>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> CanonicalResult<String>
    where
        T: ?Sized + Serialize,
    {
        Err(key_must_be_a_string())
    }

    fn serialize_seq(self, _len: Option<usize>) -> CanonicalResult<Self::SerializeSeq> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple(self, _len: usize) -> CanonicalResult<Self::SerializeTuple> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> CanonicalResult<Self::SerializeTupleStruct> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> CanonicalResult<Self::SerializeTupleVariant> {
        Err(key_must_be_a_string())
    }

    fn serialize_map(self, _len: Option<usize>) -> CanonicalResult<Self::SerializeMap> {
        Err(key_must_be_a_string())
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> CanonicalResult<Self::SerializeStruct> {
        Err(key_must_be_a_string())
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> CanonicalResult<Self::SerializeStructVariant> {
        Err(key_must_be_a_string())
    }

    fn collect_str<T>(self, value: &T) -> CanonicalResult<String>
    where
        T: ?Sized + std::fmt::Display,
    {
        Ok(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_outputs_match_for_nested_ordering_and_scalars() {
        let value = json!({
            "z": null,
            "array": [true, false, -12.5, 0, {"z": 2, "a": 1}],
            "a": {"z": "last", "a": "first"},
        });
        let expected = concat!(
            r#"{"a":{"a":"first","z":"last"},"#,
            r#""array":[true,false,-12.5,0,{"a":1,"z":2}],"#,
            r#""z":null}"#,
        );

        let text = canonical_json_value(&value).unwrap();
        let bytes = canonical_json_bytes(&value).unwrap();

        assert_eq!(text, expected);
        assert_eq!(bytes, expected.as_bytes());
    }

    #[test]
    fn canonical_outputs_preserve_json_escapes_and_unicode() {
        let value = json!({
            "unicode": "雪😀é",
            "escaped": "quote: \" slash: \\ newline:\n tab:\t control:\u{0001}",
        });
        let expected = "{\"escaped\":\"quote: \\\" slash: \\\\ newline:\\n tab:\\t control:\\u0001\",\"unicode\":\"雪😀é\"}";

        assert_eq!(canonical_json_value(&value).unwrap(), expected);
        assert_eq!(canonical_json_bytes(&value).unwrap(), expected.as_bytes());
    }

    #[test]
    fn streaming_digest_matches_digest_of_canonical_bytes() {
        let value = json!({
            "unicode": ["雪", "😀", "é"],
            "nested": {"z": null, "a": [true, "line\nfeed", 42]},
        });
        let bytes = canonical_json_bytes(&value).unwrap();
        let digest = Sha256::digest(&bytes);
        let mut expected = String::from("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut expected, "{byte:02x}").unwrap();
        }

        assert_eq!(canonical_sha256(&value).unwrap().as_str(), expected);
    }

    /// The streamed string writer must stay byte-identical to the allocating
    /// `serde_json::to_string` rendering it replaced, for every escape class.
    #[test]
    fn streamed_string_escapes_match_serde_json_for_every_scalar_byte() {
        let mut samples: Vec<String> = Vec::new();
        for code in 0u32..=0x2ff {
            if let Some(character) = char::from_u32(code) {
                samples.push(character.to_string());
                samples.push(format!("prefix{character}suffix"));
            }
        }
        samples.extend(
            [
                "",
                "plain",
                "\"",
                "\\",
                "\"\\\"",
                "back\\slash/solidus",
                "雪😀é",
                "mixed \u{0}\u{1}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{1f} tail",
                "trailing\\",
                "\u{7f}delete",
            ]
            .into_iter()
            .map(str::to_owned),
        );

        for sample in samples {
            let expected = serde_json::to_string(&sample).unwrap();
            let mut streamed = String::new();
            write_json_string(&sample, &mut streamed);
            assert_eq!(streamed, expected, "escape mismatch for {sample:?}");

            let key_object = Value::Object(
                [(sample.clone(), Value::Null)]
                    .into_iter()
                    .collect::<serde_json::Map<String, Value>>(),
            );
            assert_eq!(
                canonical_json_value(&key_object).unwrap(),
                format!("{{{expected}:null}}"),
            );
        }
    }

    /// The stack-formatted integer writer must stay byte-identical to
    /// `Number::to_string`, including the `i64::MIN` boundary.
    #[test]
    fn streamed_numbers_match_owned_number_rendering() {
        let numbers = [
            "0",
            "-0",
            "1",
            "-1",
            "9",
            "10",
            "-10",
            "18446744073709551615",
            "9223372036854775807",
            "-9223372036854775808",
            "0.0",
            "-0.5",
            "1e3",
            "-12.5",
            "1.7976931348623157e308",
        ];

        for text in numbers {
            let value: Value = serde_json::from_str(text).unwrap();
            let Value::Number(number) = &value else {
                panic!("{text} is not a JSON number");
            };
            let mut streamed = String::new();
            write_json_number(number, &mut streamed);
            assert_eq!(streamed, number.to_string(), "number mismatch for {text}");
        }
    }

    /// The exact pipeline the streaming serializer replaced: materialize a
    /// `Value` with `serde_json::to_value`, then canonicalize that tree.
    fn legacy_canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
        let value = serde_json::to_value(value).expect("legacy to_value");
        let mut output = Vec::new();
        write_canonical(&value, &mut output);
        output
    }

    fn legacy_digest(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let digest = Sha256::digest(bytes);
        let mut encoded = String::from("sha256:");
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").expect("hex encoding");
        }
        encoded
    }

    /// Assert the streaming serializer is byte-identical to the legacy
    /// `to_value` + `write_canonical` pipeline, and that both digest the same.
    #[track_caller]
    fn assert_canonical_identity<T: Serialize + ?Sized>(value: &T, label: &str) {
        let legacy = legacy_canonical_bytes(value);
        let streamed = canonical_json_bytes(&value).expect("streamed canonical bytes");
        assert_eq!(
            String::from_utf8_lossy(&streamed),
            String::from_utf8_lossy(&legacy),
            "canonical byte mismatch for {label}",
        );
        assert_eq!(
            canonical_sha256(&value).expect("streamed digest").as_str(),
            legacy_digest(&legacy),
            "canonical digest mismatch for {label}",
        );
    }

    /// A map whose keys arrive in arbitrary order, possibly repeated: the
    /// shape `#[derive(Serialize)]` never produces but `serialize_map` callers
    /// can.
    struct UnsortedMap(Vec<(&'static str, Value)>);

    impl Serialize for UnsortedMap {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap as _;
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in &self.0 {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }

    /// A value that reaches `serialize_bytes` rather than a sequence.
    struct RawBytes(&'static [u8]);

    impl Serialize for RawBytes {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    #[derive(Serialize)]
    struct UnsortedFields {
        zulu: u8,
        alpha: Option<&'static str>,
        mike: Vec<i64>,
        #[serde(rename = "\"quoted\\key\n")]
        quoted: bool,
    }

    #[derive(Serialize)]
    struct UnitStruct;

    #[derive(Serialize)]
    struct NewtypeStruct(UnsortedFields);

    #[derive(Serialize)]
    struct TupleStruct(u64, &'static str, ());

    #[derive(Serialize)]
    enum Shape {
        Unit,
        Newtype(i128),
        Tuple(u128, f64),
        Struct { zebra: char, ant: Option<u8> },
    }

    #[derive(Serialize)]
    struct Outer {
        zeta: Inner,
        #[serde(flatten)]
        inner: Inner,
        alpha: u8,
    }

    #[derive(Serialize)]
    struct Inner {
        yankee: bool,
        bravo: f32,
    }

    /// Byte-identity proof: the streaming serializer must reproduce the legacy
    /// `to_value` + `write_canonical` bytes (and digest) exactly, across every
    /// serde data-model shape the domain can hand it.
    #[test]
    fn streaming_serializer_matches_to_value_pipeline_byte_for_byte() {
        assert_canonical_identity(&(), "unit");
        assert_canonical_identity(&UnitStruct, "unit struct");
        assert_canonical_identity(&Option::<u8>::None, "none");
        assert_canonical_identity(&Some(Some(7u8)), "nested some");
        assert_canonical_identity(&true, "bool");
        assert_canonical_identity(&'雪', "char");
        assert_canonical_identity(&"quote:\" slash:\\ nl:\n ctl:\u{1} 雪😀é", "escapes");
        assert_canonical_identity(&RawBytes(&[0, 1, 127, 128, 255]), "bytes");
        assert_canonical_identity(&RawBytes(&[]), "empty bytes");
        assert_canonical_identity(&Vec::<u8>::new(), "empty sequence");
        assert_canonical_identity(&(1u8, "two", vec![3i8, -4]), "tuple");
        assert_canonical_identity(&TupleStruct(9, "nine", ()), "tuple struct");

        for value in [
            0i64,
            -1,
            1,
            i64::MIN,
            i64::MAX,
            i64::from(i32::MIN),
            -9_223_372_036_854_775_807,
        ] {
            assert_canonical_identity(&value, "i64 boundary");
        }
        for value in [0u64, 1, u64::MAX, u64::from(u32::MAX)] {
            assert_canonical_identity(&value, "u64 boundary");
        }
        for value in [
            0i128,
            -1,
            i128::from(i64::MIN),
            i128::from(u64::MAX),
            i128::from(i64::MAX),
        ] {
            assert_canonical_identity(&value, "in-range i128");
        }
        for value in [0u128, u128::from(u64::MAX)] {
            assert_canonical_identity(&value, "in-range u128");
        }
        // Out-of-range 128-bit integers must keep failing rather than digest.
        assert!(canonical_json_bytes(&(i128::from(u64::MAX) + 1)).is_err());
        assert!(canonical_json_bytes(&(i128::from(i64::MIN) - 1)).is_err());
        assert!(canonical_json_bytes(&(u128::from(u64::MAX) + 1)).is_err());

        for value in [
            0.0f64,
            -0.0,
            1.0,
            -12.5,
            1e3,
            1e-7,
            f64::MIN,
            f64::MAX,
            1.797_693_134_862_315_7e308,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert_canonical_identity(&value, "f64 boundary");
        }
        for value in [0.0f32, -0.0, 13.37, f32::MIN, f32::MAX, f32::NAN] {
            assert_canonical_identity(&value, "f32 boundary");
        }

        let fields = || UnsortedFields {
            zulu: 255,
            alpha: Some("first"),
            mike: vec![i64::MIN, 0, i64::MAX],
            quoted: false,
        };
        assert_canonical_identity(&fields(), "unsorted derived fields");
        assert_canonical_identity(&NewtypeStruct(fields()), "newtype struct");
        assert_canonical_identity(&vec![fields(), fields()], "sequence of structs");

        assert_canonical_identity(&Shape::Unit, "unit variant");
        assert_canonical_identity(&Shape::Newtype(i128::from(u64::MAX)), "newtype variant");
        assert_canonical_identity(&Shape::Tuple(u128::from(u64::MAX), -0.0), "tuple variant");
        assert_canonical_identity(
            &Shape::Struct {
                zebra: '\u{1}',
                ant: None,
            },
            "struct variant",
        );
        assert_canonical_identity(
            &vec![
                Shape::Unit,
                Shape::Newtype(-1),
                Shape::Struct {
                    zebra: '"',
                    ant: Some(3),
                },
            ],
            "sequence of variants",
        );

        assert_canonical_identity(
            &Outer {
                zeta: Inner {
                    yankee: true,
                    bravo: -1.5,
                },
                inner: Inner {
                    yankee: false,
                    bravo: 0.25,
                },
                alpha: 1,
            },
            "flattened struct",
        );

        assert_canonical_identity(&UnsortedMap(vec![]), "empty map");
        assert_canonical_identity(
            &UnsortedMap(vec![
                ("zulu", json!({"z": 1, "a": [1, 2, {"b": null, "a": true}]})),
                ("alpha", json!("value")),
                ("\u{1}control", json!(-0.0)),
                ("雪", json!({})),
                ("mike", Value::Null),
            ]),
            "unsorted map keys",
        );
        // Repeated keys collapse to the last value, exactly as repeated
        // `serde_json::Map::insert` calls do.
        assert_canonical_identity(
            &UnsortedMap(vec![
                ("dup", json!(1)),
                ("alpha", json!("a")),
                ("dup", json!(2)),
                ("dup", json!(3)),
            ]),
            "duplicate map keys",
        );

        assert_canonical_identity(
            &json!({
                "z": null,
                "array": [true, false, -12.5, 0, {"z": 2, "a": 1}],
                "a": {"z": "last", "a": "first"},
                "deep": [[[{"b": [], "a": {}}]]],
            }),
            "json value tree",
        );
    }

    /// A raw JSON payload keeps its `to_value` meaning: the private
    /// `RawValue` struct token is parsed and re-canonicalized, not streamed
    /// as an opaque struct.
    #[test]
    fn raw_value_payloads_match_to_value_pipeline() {
        let raw = serde_json::value::RawValue::from_string(
            r#"{ "z" : 1 , "a" : [ 2 , { "d" : 4 , "c" : 3 } ] }"#.to_owned(),
        )
        .expect("raw value parses");
        assert_canonical_identity(&raw, "bare raw value");
        assert_canonical_identity(
            &UnsortedMap(vec![("zulu", json!(1)), ("alpha", json!(2))]),
            "map beside raw value",
        );

        #[derive(Serialize)]
        struct WithRaw<'a> {
            zulu: &'a serde_json::value::RawValue,
            alpha: u8,
        }
        assert_canonical_identity(
            &WithRaw {
                zulu: &raw,
                alpha: 1,
            },
            "nested raw value",
        );
    }

    /// The buffered hashing sink must not change the bytes the hasher sees.
    #[test]
    fn buffered_sink_preserves_written_bytes() {
        let long = "x".repeat(SINK_BUFFER_CAPACITY * 3 + 7);
        let chunks = ["", "a", "\"", &long, "b", &"y".repeat(SINK_BUFFER_CAPACITY)];
        let mut direct = String::new();
        let mut buffered = BufferedSink::new(String::new());
        for chunk in chunks {
            direct.write(chunk);
            buffered.write(chunk);
        }
        assert_eq!(buffered.finish(), direct);
    }

    /// Objects whose keys already arrive sorted take the collect-free path; it
    /// must agree with the collect-and-sort path on the same input.
    #[test]
    fn presorted_and_unsorted_objects_canonicalize_identically() {
        let sorted: Value = serde_json::from_str(r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#)
            .expect("sorted fixture");
        let unsorted: Value = serde_json::from_str(r#"{"z":[{"a":4}],"b":{"z":3,"a":2},"a":1}"#)
            .expect("unsorted fixture");

        assert!(matches!(&sorted, Value::Object(values) if keys_are_canonically_ordered(values)));
        assert_eq!(
            canonical_json_value(&sorted).unwrap(),
            canonical_json_value(&unsorted).unwrap(),
        );
        assert_eq!(
            canonical_json_value(&sorted).unwrap(),
            r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#,
        );
    }
}
