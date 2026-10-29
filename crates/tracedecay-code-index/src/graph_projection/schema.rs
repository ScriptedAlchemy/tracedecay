//! Durable labels, properties, and identities for the code-graph projection.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::mem::size_of;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::write::DeflateEncoder;
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZFlush, MZStatus};
use serde::Serialize;
use serde::de::{DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use tracedecay_domain::FileOccurrenceId;
pub(super) use tracedecay_graph_db::graph_stable_identity as stable_identity;
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphEntity, GraphEntityId, GraphProperty, GraphPropertyName,
    GraphRelationId, MAX_GRAPH_PROPERTY_AGGREGATE_BYTES, MAX_GRAPH_PROPERTY_VALUE_BYTES,
};

use super::CodeGraphProjectionError;
use crate::chunks::CodeIndexImportEvidenceV1;

pub(super) const SYMBOL_RECORD_PROPERTY: &str = "symbol-record";
pub(super) const FILE_RECORD_PROPERTY: &str = "file-record";
pub(super) const IMPORT_RECORD_PROPERTY: &str = "import-record";
pub(super) const SYMBOL_LABEL: &str = "CodeSymbol";
pub(super) const FILE_LABEL: &str = "CodeFile";
pub(super) const IMPORT_LABEL: &str = "CodeImport";
pub(super) const FILE_IMPORT_EDGE_KIND: &str = "CodeFileContainsImport";

pub(super) fn file_entity_id(
    file: &FileOccurrenceId,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("file", file.as_str())).map_err(Into::into)
}

pub(super) fn import_entity_id(
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("import", &hex::encode(serialize(import)?)))
        .map_err(Into::into)
}

pub(super) fn file_import_relation_id(
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphRelationId, CodeGraphProjectionError> {
    let import_id = import_entity_id(import)?;
    file_import_relation_id_with(import, &import_id)
}

/// Same relation identity with the import entity id already derived, so a
/// caller that just computed it does not serialize and hash the import again.
pub(super) fn file_import_relation_id_with(
    import: &CodeIndexImportEvidenceV1,
    import_id: &GraphEntityId,
) -> Result<GraphRelationId, CodeGraphProjectionError> {
    GraphRelationId::new(stable_identity(
        "file-import",
        &format!(
            "{}\0{}",
            import.file_occurrence_id.as_str(),
            import_id.as_str()
        ),
    ))
    .map_err(Into::into)
}

pub(super) fn serialize(value: &impl Serialize) -> Result<Vec<u8>, CodeGraphProjectionError> {
    serde_json::to_vec(value).map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))
}

/// Compact text preserves existing records; oversized records use lossless
/// DEFLATE within the same property limit and the graph's aggregate decode bound.
pub(super) fn record_property(payload: Vec<u8>) -> Result<GraphProperty, CodeGraphProjectionError> {
    let json = String::from_utf8(payload)
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let compact = compact_record(&json);
    if compact.len() <= MAX_GRAPH_PROPERTY_VALUE_BYTES {
        return Ok(GraphProperty::String(compact));
    }
    if json.len() > MAX_GRAPH_PROPERTY_AGGREGATE_BYTES {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Capacity,
            MAX_GRAPH_PROPERTY_AGGREGATE_BYTES,
        )
        .into());
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(json.as_bytes())
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    if compressed.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Capacity,
            MAX_GRAPH_PROPERTY_VALUE_BYTES,
        )
        .into());
    }
    Ok(GraphProperty::Bytes(compressed))
}

pub(super) fn deserialize_property<T>(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
) -> Result<T, CodeGraphProjectionError>
where
    T: DeserializeOwned,
{
    decode_property(properties, name, |_| Ok(()), false)
}

pub(super) trait CatalogRecord: DeserializeOwned {
    const PROPERTY: &'static str;
}

/// The serializer's canonical byte count, without retaining another record.
pub(super) fn serialized_record_len(
    value: &impl Serialize,
) -> Result<usize, CodeGraphProjectionError> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| {
                std::io::Error::other("serialized code graph record size overflow")
            })?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value)
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    Ok(count.0)
}

pub(super) fn deserialize_catalog_property<T: CatalogRecord>(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
    admit: impl FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<T, CodeGraphProjectionError> {
    decode_property(properties, T::PROPERTY, admit, true)
}

fn decode_property<T: DeserializeOwned>(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
    mut admit: impl FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    catalog: bool,
) -> Result<T, CodeGraphProjectionError> {
    let property = properties
        .iter()
        .find_map(|(key, value)| (key.as_str() == name).then_some(value))
        .ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(format!("code graph row is missing {name}"))
        })?;
    match property {
        GraphProperty::String(stored) => {
            let expanded = expanded_record_len(stored)?;
            let scratch = json_scratch_bytes(expanded);
            if catalog {
                admit(expanded.saturating_add(scratch))?;
            }
            let json = expand_record(stored)?;
            if catalog {
                admit_catalog_json(json.as_bytes(), expanded, &mut admit)?;
            }
            serde_json::from_str(&json)
                .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
        }
        GraphProperty::Bytes(stored) => {
            let corrupt = || {
                CodeGraphProjectionError::Corrupt(format!(
                    "code graph row {name} has invalid or oversized compressed content"
                ))
            };
            if stored.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES {
                return Err(corrupt());
            }
            let capacity = MAX_GRAPH_PROPERTY_AGGREGATE_BYTES + 1;
            admit(size_of::<InflateState>().saturating_add(capacity))?;
            let mut decoder = InflateState::new_boxed(DataFormat::Raw);
            let mut decoded = vec![0_u8; capacity];
            let result = inflate(&mut decoder, stored, &mut decoded, MZFlush::Finish);
            if result.status != Ok(MZStatus::StreamEnd)
                || result.bytes_consumed != stored.len()
                || result.bytes_written > MAX_GRAPH_PROPERTY_AGGREGATE_BYTES
            {
                return Err(corrupt());
            }
            decoded.truncate(result.bytes_written);
            drop(decoder);
            if catalog {
                admit_catalog_json(&decoded, capacity, &mut admit)?;
            }
            serde_json::from_slice(&decoded)
                .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
        }
        _ => Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph row {name} has the wrong type"
        ))),
    }
}

fn admit_catalog_json(
    json: &[u8],
    buffer_bytes: usize,
    admit: &mut impl FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<(), CodeGraphProjectionError> {
    let scratch = json_scratch_bytes(json.len());
    admit(buffer_bytes.saturating_add(scratch))?;
    let mut census = CatalogAllocationCensus::default();
    let mut decoder = serde_json::Deserializer::from_slice(json);
    CatalogCensusSeed(&mut census)
        .deserialize(&mut decoder)
        .and_then(|()| decoder.end())
        .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))?;
    admit(
        buffer_bytes
            .saturating_add(scratch)
            .saturating_add(census.output_bytes()),
    )
}

/// Leads a [`RECORD_TOKENS`] code in stored record text.
const TOKEN_MARKER: char = '\u{1}';
/// Leads a 64-hex digest stored as unpadded base64url.
const DIGEST_MARKER: char = '\u{2}';
const DIGEST_HEX_CHARS: usize = 64;
const DIGEST_TEXT_CHARS: usize = 43;
const _: () = assert!(RECORD_TOKENS.len() <= TOKEN_CODES.len());
const TOKEN_CODES: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Structural JSON the projector's records repeat: field keys with their
/// colon, enum values, and identifier prefixes, each starting at a quote.
/// A stored code is the token's index here, so entries are only appended
/// under a projector revision bump.
const RECORD_TOKENS: &[&str] = &[
    "\"occurrence\":",
    "\"binding\":",
    "\"metadata\":",
    "\"unresolved_calls\":",
    "\"file\":",
    "\"logical_path\":",
    "\"source_span\":",
    "\"chunk\":",
    "\"language_descriptor_revision\":",
    "\"start_byte\":",
    "\"end_byte\":",
    "\"identity\":",
    "\"qualified_name\":",
    "\"simple_name\":",
    "\"kind\":",
    "\"visibility\":",
    "\"branches\":",
    "\"loops\":",
    "\"max_nesting\":",
    "\"complexity_analysis\":",
    "\"line_span\":",
    "\"start_line\":",
    "\"signature\":",
    "\"docstring\":",
    "\"is_async\":",
    "\"derives\":",
    "\"skip_test_coverage\":",
    "\"file_identity\":",
    "\"content_digest\":",
    "\"from_occurrence\":",
    "\"to_occurrence\":",
    "\"authority\":",
    "\"evidence_span\":",
    "\"reference_name\":",
    "\"file_occurrence_id\":",
    "\"language\":",
    "\"disposition\":",
    "\"module_specifier\":",
    "\"imported_name\":",
    "\"local_name\":",
    "\"is_public\":",
    "\"reexport_scope\":",
    "\"is_glob\":",
    "\"namespace\":",
    "\"module_kind\":",
    "\"span\":",
    "\"start_column\":",
    "\"symbol.v1.sha256:",
    "\"chunk.v1.sha256:",
    "\"descriptor.",
    "\"sha256:",
    "\"function\"",
    "\"method\"",
    "\"public\"",
    "\"private\"",
    "\"syntax_exact\"",
    "\"name_resolved\"",
    "\"calls\"",
    "\"uses\"",
    "\"present\"",
    ":null",
    ":false",
    ":true",
    ":[]",
];

/// Stored text for record JSON: every [`RECORD_TOKENS`] entry becomes
/// [`TOKEN_MARKER`] and its code, and every maximal run of exactly 64
/// lowercase hex characters becomes [`DIGEST_MARKER`] and base64url.
/// `serde_json` escapes every control character, so neither marker occurs in
/// the JSON and [`expand_record`] inverts this exactly.
pub(super) fn compact_record(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len());
    let mut index = 0;
    let mut run_start = None::<usize>;
    let flush = |out: &mut String, start: usize, end: usize| {
        let run = &json[start..end];
        let mut digest = [0_u8; DIGEST_HEX_CHARS / 2];
        if run.len() == DIGEST_HEX_CHARS && hex::decode_to_slice(run, &mut digest).is_ok() {
            out.push(DIGEST_MARKER);
            out.push_str(&URL_SAFE_NO_PAD.encode(digest));
        } else {
            out.push_str(run);
        }
    };
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) {
            run_start.get_or_insert(index);
            index += 1;
            continue;
        }
        if let Some(start) = run_start.take() {
            flush(&mut out, start, index);
        }
        if matches!(byte, b'"' | b':')
            && let Some((code, token)) = RECORD_TOKENS
                .iter()
                .enumerate()
                .filter(|(_, token)| json[index..].starts_with(**token))
                .max_by_key(|(_, token)| token.len())
        {
            out.push(TOKEN_MARKER);
            out.push(char::from(TOKEN_CODES[code]));
            index += token.len();
            continue;
        }
        let Some(character) = json[index..].chars().next() else {
            break;
        };
        out.push(character);
        index += character.len_utf8();
    }
    if let Some(start) = run_start {
        flush(&mut out, start, bytes.len());
    }
    out
}

/// Inverts [`compact_record`].
pub(super) fn expand_record(stored: &str) -> Result<String, CodeGraphProjectionError> {
    let mut out = String::with_capacity(expanded_record_len(stored)?);
    visit_record_fragments(stored, |fragment| out.push_str(fragment))?;
    Ok(out)
}

fn expanded_record_len(stored: &str) -> Result<usize, CodeGraphProjectionError> {
    let mut bytes = 0_usize;
    visit_record_fragments(stored, |fragment| {
        bytes = bytes.saturating_add(fragment.len());
    })?;
    Ok(bytes)
}

fn visit_record_fragments(
    stored: &str,
    mut visit: impl FnMut(&str),
) -> Result<(), CodeGraphProjectionError> {
    let malformed =
        || CodeGraphProjectionError::Corrupt("code graph record text is malformed".to_owned());
    let mut rest = stored;
    while let Some(position) = rest.find([TOKEN_MARKER, DIGEST_MARKER]) {
        visit(&rest[..position]);
        let marker = rest[position..].chars().next().ok_or_else(malformed)?;
        let body = &rest[position + marker.len_utf8()..];
        if marker == TOKEN_MARKER {
            let code = *body.as_bytes().first().ok_or_else(malformed)?;
            let token = TOKEN_CODES
                .iter()
                .position(|candidate| *candidate == code)
                .and_then(|index| RECORD_TOKENS.get(index))
                .ok_or_else(malformed)?;
            visit(token);
            rest = &body[1..];
        } else {
            let encoded = body.get(..DIGEST_TEXT_CHARS).ok_or_else(malformed)?;
            let mut digest = [0_u8; DIGEST_HEX_CHARS / 2];
            let written = URL_SAFE_NO_PAD
                .decode_slice(encoded, &mut digest)
                .map_err(|_| malformed())?;
            if written != digest.len() {
                return Err(malformed());
            }
            let mut hex = [0_u8; DIGEST_HEX_CHARS];
            hex::encode_to_slice(digest, &mut hex).map_err(|_| malformed())?;
            visit(std::str::from_utf8(&hex).map_err(|_| malformed())?);
            rest = &body[DIGEST_TEXT_CHARS..];
        }
    }
    visit(rest);
    Ok(())
}

fn json_scratch_bytes(bytes: usize) -> usize {
    // Vec::reserve grows to max(required, 2 * old_capacity, 8). Before
    // growth old_capacity < required <= input length, so old + new <=
    // 3 * input length. Include the byte Vec's minimum allocation.
    bytes.saturating_mul(3).saturating_add(8)
}

#[derive(Default)]
struct CatalogAllocationCensus {
    strings: usize,
    largest_string: usize,
    sequence_bytes: usize,
}

impl CatalogAllocationCensus {
    fn output_bytes(&self) -> usize {
        // StoredSymbolRecord clones metadata.occurrence on conversion. The
        // largest string also covers that one extra string.
        self.strings
            .saturating_add(self.largest_string)
            .saturating_add(self.sequence_bytes)
    }
}

struct CatalogCensusSeed<'a>(&'a mut CatalogAllocationCensus);

impl<'de> DeserializeSeed<'de> for CatalogCensusSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, decoder: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for CatalogCensusSeed<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a catalog record")
    }

    fn visit_str<E>(self, value: &str) -> Result<(), E> {
        self.0.strings = self.0.strings.saturating_add(value.len());
        self.0.largest_string = self.0.largest_string.max(value.len());
        Ok(())
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0_usize;
        while sequence
            .next_element_seed(CatalogCensusSeed(self.0))?
            .is_some()
        {
            count = count.saturating_add(1);
        }
        if count != 0 {
            // The catalog schemas contain only derives (String) and unresolved
            // references. JSON has no sequence hint, so Vec starts at four and
            // doubles. Include the old buffer during its final reallocation.
            let capacity = count
                .max(4)
                .checked_next_power_of_two()
                .unwrap_or(usize::MAX);
            let element =
                size_of::<String>().max(size_of::<crate::chunks::CodeIndexUnresolvedReferenceV1>());
            self.0.sequence_bytes = self.0.sequence_bytes.saturating_add(
                capacity
                    .saturating_add(capacity / 2)
                    .saturating_mul(element),
            );
        }
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_key_seed(CatalogCensusSeed(self.0))?.is_some() {
            map.next_value_seed(CatalogCensusSeed(self.0))?;
        }
        Ok(())
    }
}

pub(super) fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(property: GraphProperty) -> Result<String, CodeGraphProjectionError> {
        deserialize_property(
            &BTreeMap::from([(GraphPropertyName::new("record").unwrap(), property)]),
            "record",
        )
    }

    #[test]
    fn oversized_record_compresses_and_round_trips_exactly() {
        let original = "\"\\".repeat(320_000);
        let raw = serde_json::to_vec(&original).unwrap();
        let compact_bytes = compact_record(std::str::from_utf8(&raw).unwrap()).len();
        let property = record_property(raw.clone()).unwrap();
        let GraphProperty::Bytes(compressed) = &property else {
            panic!("oversized compact record must use compressed bytes");
        };
        assert!(compact_bytes > MAX_GRAPH_PROPERTY_VALUE_BYTES);
        assert!(
            compressed.len() <= MAX_GRAPH_PROPERTY_VALUE_BYTES,
            "raw={} compact={} compressed={}",
            raw.len(),
            compact_bytes,
            compressed.len()
        );
        assert_eq!(decode(property).unwrap(), original);
    }

    #[test]
    fn published_compact_strings_remain_byte_exact() {
        let original = "quoted \" text \\ and \u{1} sha256:".to_owned() + &"a".repeat(64);
        let raw = serde_json::to_vec(&original).unwrap();
        let published = GraphProperty::String(compact_record(std::str::from_utf8(&raw).unwrap()));
        assert_eq!(record_property(raw).unwrap(), published);
        assert_eq!(decode(published).unwrap(), original);
    }

    #[test]
    fn compressed_records_refuse_corruption_and_decoded_overflow() {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"\"valid\"").unwrap();
        let valid = encoder.finish().unwrap();
        assert_eq!(
            decode(GraphProperty::Bytes(valid.clone())).unwrap(),
            "valid"
        );
        let mut trailing = valid.clone();
        trailing.push(0);
        for bytes in [vec![0xff], valid[..valid.len() - 1].to_vec(), trailing] {
            assert!(matches!(
                decode(GraphProperty::Bytes(bytes)),
                Err(CodeGraphProjectionError::Corrupt(_))
            ));
        }
        let oversized = vec![b' '; MAX_GRAPH_PROPERTY_AGGREGATE_BYTES + 1];
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&oversized).unwrap();
        assert!(matches!(
            decode(GraphProperty::Bytes(encoder.finish().unwrap())),
            Err(CodeGraphProjectionError::Corrupt(_))
        ));
        assert!(matches!(
            record_property(
                serde_json::to_vec(&"x".repeat(MAX_GRAPH_PROPERTY_AGGREGATE_BYTES)).unwrap()
            ),
            Err(CodeGraphProjectionError::BudgetExhausted { .. })
        ));
    }
}
