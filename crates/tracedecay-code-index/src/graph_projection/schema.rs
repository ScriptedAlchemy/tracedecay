//! Durable labels, properties, and identities for the code-graph projection.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_domain::FileOccurrenceId;
pub(super) use tracedecay_graph_db::graph_stable_identity as stable_identity;
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphProperty, GraphPropertyName, GraphRelationId,
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

/// A record's serialized JSON as the compact text property that carries it.
///
/// The record stays a string, not bytes: the sealed compact store keeps byte
/// payloads in its string dictionary as marked hex. [`compact_record`] turns
/// the JSON's structural tokens and 64-hex digests into short marked codes;
/// free-form values stay JSON text.
pub(super) fn record_property(payload: Vec<u8>) -> Result<GraphProperty, CodeGraphProjectionError> {
    let json = String::from_utf8(payload)
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    Ok(GraphProperty::String(compact_record(&json)))
}

pub(super) fn deserialize_property<T>(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
) -> Result<T, CodeGraphProjectionError>
where
    T: DeserializeOwned,
{
    let property = properties
        .get(&GraphPropertyName::new(name)?)
        .ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(format!("code graph row is missing {name}"))
        })?;
    let GraphProperty::String(stored) = property else {
        return Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph row {name} has the wrong type"
        )));
    };
    serde_json::from_str(&expand_record(stored)?)
        .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
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
    let malformed =
        || CodeGraphProjectionError::Corrupt("code graph record text is malformed".to_owned());
    let mut out = String::with_capacity(stored.len() * 2);
    let mut rest = stored;
    while let Some(position) = rest.find([TOKEN_MARKER, DIGEST_MARKER]) {
        out.push_str(&rest[..position]);
        let marker = rest[position..].chars().next().ok_or_else(malformed)?;
        let body = &rest[position + marker.len_utf8()..];
        if marker == TOKEN_MARKER {
            let code = *body.as_bytes().first().ok_or_else(malformed)?;
            let token = TOKEN_CODES
                .iter()
                .position(|candidate| *candidate == code)
                .and_then(|index| RECORD_TOKENS.get(index))
                .ok_or_else(malformed)?;
            out.push_str(token);
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
            out.push_str(&hex::encode(digest));
            rest = &body[DIGEST_TEXT_CHARS..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

pub(super) fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}
