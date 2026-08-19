use serde::Serialize;

use super::canonical::{CanonicalError, CanonicalResult, SERDE_JSON_PRIVATE_TOKEN_PREFIX};
use super::canonical_sink::{CanonicalSink, write_f64, write_i64, write_json_string, write_u64};
use super::canonical_value::write_canonical;
use super::error::DomainError;

/// Stream `value` into `sink` in canonical JSON form.
pub(super) fn serialize_canonical<T, S>(value: &T, sink: &mut S) -> Result<(), DomainError>
where
    T: Serialize + ?Sized,
    S: CanonicalSink,
{
    value
        .serialize(CanonicalSerializer { sink })
        .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))
}

/// `serde_json`'s private struct tokens (`RawValue`, and the
/// arbitrary-precision `Number`) carry payloads that only `serde_json`'s own
/// value serializer knows how to decode. Streaming cannot reproduce them, so
/// those subtrees fall back to materializing a `Value`.
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
        let key = self.pending_key.take().ok_or_else(|| {
            serde::ser::Error::custom("serialize_value called before serialize_key")
        })?;
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
