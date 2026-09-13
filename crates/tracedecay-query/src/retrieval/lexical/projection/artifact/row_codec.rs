use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId,
    LanguageDescriptorRevision, SourceSpan, SymbolOccurrenceId,
};

use super::super::LexicalFieldV1;
use super::CodeLexicalArtifactErrorV1;
use super::format::ArtifactRowV1;
use super::schema::{LexicalArtifactLayoutV1, stable_row_dictionary_id};

const ROW_CODEC_V11_MAGIC: &[u8] = b"TDLR11\0";
const ROW_CODEC_V14_MAGIC: &[u8] = b"TDLR14\0";

/// Chunk identities the chunker mints: `chunk.v1.` followed by a tagged
/// lowercase SHA-256. Revision 14 stores such a parent as its 32 digest bytes
/// and any other shape as the literal string.
const CANONICAL_CHUNK_ID_PREFIX: &str = "chunk.v1.sha256:";
const PARENT_NONE: u8 = 0;
const PARENT_CANONICAL_DIGEST: u8 = 1;
const PARENT_LITERAL: u8 = 2;
const OPTIONAL_ABSENT: u8 = 0;
const OPTIONAL_PRESENT: u8 = 1;
/// An exact term's symbol authority is almost always the row's own symbol;
/// spell that as one byte instead of a second reference.
const TERM_SYMBOL_NONE: u8 = 0;
const TERM_SYMBOL_ROW: u8 = 1;
const TERM_SYMBOL_REFERENCE: u8 = 2;

/// Field order for the `field_lengths` presence bitmap. Appending a field
/// here is a layout change: the bitmap is one byte and decoders index it by
/// position.
const FIELD_LENGTH_ORDER: [LexicalFieldV1; 7] = [
    LexicalFieldV1::SymbolName,
    LexicalFieldV1::QualifiedName,
    LexicalFieldV1::Path,
    LexicalFieldV1::BodyText,
    LexicalFieldV1::PreambleText,
    LexicalFieldV1::ExactTerm,
    LexicalFieldV1::Subtoken,
];

const GRAIN_ORDER: [CodeSearchChunkGrainV1; 5] = [
    CodeSearchChunkGrainV1::SymbolSignature,
    CodeSearchChunkGrainV1::SymbolBody,
    CodeSearchChunkGrainV1::SymbolMember,
    CodeSearchChunkGrainV1::FilePreamble,
    CodeSearchChunkGrainV1::FileWindow,
];

const EXACT_TERM_KIND_ORDER: [ExactTechnicalTermKindV1; 11] = [
    ExactTechnicalTermKindV1::WholeSymbol,
    ExactTechnicalTermKindV1::QualifiedName,
    ExactTechnicalTermKindV1::Path,
    ExactTechnicalTermKindV1::CompilerErrorCode,
    ExactTechnicalTermKindV1::CompilerErrorText,
    ExactTechnicalTermKindV1::RuntimeErrorCode,
    ExactTechnicalTermKindV1::RuntimeErrorText,
    ExactTechnicalTermKindV1::CliFlag,
    ExactTechnicalTermKindV1::ToolName,
    ExactTechnicalTermKindV1::ConfigurationKey,
    ExactTechnicalTermKindV1::CommitIdentifier,
];

/// Compact row payload: drop identities already stored as columns or
/// generation metadata, and reconstruct ASCII-normalized text on read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactRowCompactV11 {
    file_occurrence_id: FileOccurrenceId,
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
    parent_chunk_id: Option<CodeSearchChunkId>,
    source_span: SourceSpan,
    grain: CodeSearchChunkGrainV1,
    ordinal: u32,
    language_descriptor_revision: LanguageDescriptorRevision,
    exact_terms: Vec<ExactTechnicalTermV1>,
    sanitized_text: BoundedSanitizedText,
    logical_path: String,
    symbol_simple_name: Option<String>,
    symbol_qualified_name: Option<String>,
    symbol_kind: Option<String>,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
}

/// Dictionary entries a revision-14 row references by content-addressed id:
/// one per file (occurrence identity, logical path, descriptor revision) and
/// one per symbol display (occurrence identity and parser-attested names).
/// The encoder fills one table per prepared page; the batch writer stages
/// the union and finalization derives the sealed `row_dictionary`.
pub(super) type RowDictionaryTableV1 = BTreeMap<i64, Vec<u8>>;

const ENTRY_FILE: u8 = 1;
const ENTRY_SYMBOL: u8 = 2;

/// A decoded dictionary entry. Rows reference entries, not strings, so one
/// lookup restores every per-file or per-symbol field at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RowDictionaryEntryV1 {
    File {
        file_occurrence_id: String,
        logical_path: String,
        language_descriptor_revision: String,
    },
    Symbol {
        symbol_occurrence_id: Option<String>,
        simple_name: Option<String>,
        qualified_name: Option<String>,
        kind: Option<String>,
    },
}

impl RowDictionaryEntryV1 {
    fn encode(&self) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
        let mut out = Vec::with_capacity(256);
        match self {
            Self::File {
                file_occurrence_id,
                logical_path,
                language_descriptor_revision,
            } => {
                out.push(ENTRY_FILE);
                put_bytes(&mut out, file_occurrence_id.as_bytes())?;
                put_bytes(&mut out, logical_path.as_bytes())?;
                put_bytes(&mut out, language_descriptor_revision.as_bytes())?;
            }
            Self::Symbol {
                symbol_occurrence_id,
                simple_name,
                qualified_name,
                kind,
            } => {
                out.push(ENTRY_SYMBOL);
                for field in [symbol_occurrence_id, simple_name, qualified_name, kind] {
                    match field {
                        None => out.push(OPTIONAL_ABSENT),
                        Some(value) => {
                            out.push(OPTIONAL_PRESENT);
                            put_bytes(&mut out, value.as_bytes())?;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let mut cursor = RowCursorV1 { bytes };
        let entry = match cursor.take_u8()? {
            ENTRY_FILE => Self::File {
                file_occurrence_id: cursor.take_string()?,
                logical_path: cursor.take_string()?,
                language_descriptor_revision: cursor.take_string()?,
            },
            ENTRY_SYMBOL => Self::Symbol {
                symbol_occurrence_id: cursor.take_optional_string()?,
                simple_name: cursor.take_optional_string()?,
                qualified_name: cursor.take_optional_string()?,
                kind: cursor.take_optional_string()?,
            },
            _ => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact dictionary entry kind is unknown".to_owned(),
                ));
            }
        };
        if !cursor.bytes.is_empty() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact dictionary entry has trailing bytes".to_owned(),
            ));
        }
        Ok(entry)
    }
}

/// Resolves revision-14 dictionary references. Layouts before 14 never
/// consult it, so every call site can hand over its connection.
pub(super) trait RowDictionaryV1 {
    fn entry(&self, entry_id: i64)
    -> Result<Arc<RowDictionaryEntryV1>, CodeLexicalArtifactErrorV1>;
}

/// `row_dictionary` lookups over an open artifact connection. Every entry is
/// re-hashed against the id the row referenced, so a corrupt or foreign
/// dictionary row fails closed instead of relabeling a chunk. Entries are
/// memoised for the resolver's lifetime (one query), since a query's
/// candidate rows cluster in few files and every symbol spans two chunks;
/// the memo is bounded by the query's candidate cap and freed with it.
pub(super) struct ConnectionRowDictionaryV1<'a> {
    connection: &'a Connection,
    entries: RefCell<HashMap<i64, Arc<RowDictionaryEntryV1>>>,
}

impl<'a> ConnectionRowDictionaryV1<'a> {
    pub(super) fn new(connection: &'a Connection) -> Self {
        Self {
            connection,
            entries: RefCell::new(HashMap::new()),
        }
    }
}

impl RowDictionaryV1 for ConnectionRowDictionaryV1<'_> {
    fn entry(
        &self,
        entry_id: i64,
    ) -> Result<Arc<RowDictionaryEntryV1>, CodeLexicalArtifactErrorV1> {
        if let Some(entry) = self.entries.borrow().get(&entry_id) {
            return Ok(Arc::clone(entry));
        }
        let mut statement = self
            .connection
            .prepare_cached("SELECT entry FROM row_dictionary WHERE entry_id = ?1")
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let bytes: Option<Vec<u8>> = statement
            .query_row([entry_id], |row| row.get(0))
            .optional()
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let bytes = bytes.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row references a missing dictionary entry".to_owned(),
            )
        })?;
        if stable_row_dictionary_id(&bytes) != entry_id {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact dictionary entry does not match its identifier".to_owned(),
            ));
        }
        let entry = Arc::new(RowDictionaryEntryV1::decode(&bytes)?);
        self.entries
            .borrow_mut()
            .insert(entry_id, Arc::clone(&entry));
        Ok(entry)
    }
}

pub(super) fn encode_artifact_row(
    layout: LexicalArtifactLayoutV1,
    row: &ArtifactRowV1,
    dictionary: &mut RowDictionaryTableV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    match layout {
        LexicalArtifactLayoutV1::V10 => serde_json::to_vec(row)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string())),
        LexicalArtifactLayoutV1::V11
        | LexicalArtifactLayoutV1::V12
        | LexicalArtifactLayoutV1::V13 => encode_compact_v11(row),
        LexicalArtifactLayoutV1::V14 => encode_binary_v14(row, dictionary),
    }
}

fn encode_compact_v11(row: &ArtifactRowV1) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let compact = ArtifactRowCompactV11 {
        file_occurrence_id: row.anchor.file_occurrence_id.clone(),
        symbol_occurrence_id: row.anchor.symbol_occurrence_id.clone(),
        parent_chunk_id: row.anchor.parent_chunk_id.clone(),
        source_span: row.anchor.source_span,
        grain: row.anchor.grain,
        ordinal: row.anchor.ordinal,
        language_descriptor_revision: row.language_descriptor_revision.clone(),
        exact_terms: row.exact_terms.clone(),
        sanitized_text: row.sanitized_text.clone(),
        logical_path: row.logical_path.clone(),
        symbol_simple_name: row.symbol_simple_name.clone(),
        symbol_qualified_name: row.symbol_qualified_name.clone(),
        symbol_kind: row.symbol_kind.clone(),
        field_lengths: row.field_lengths.clone(),
    };
    let payload = serde_json::to_vec(&compact)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut bytes = Vec::with_capacity(ROW_CODEC_V11_MAGIC.len().saturating_add(payload.len()));
    bytes.extend_from_slice(ROW_CODEC_V11_MAGIC);
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub(super) fn decode_artifact_row(
    layout: LexicalArtifactLayoutV1,
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
    dictionary: &dyn RowDictionaryV1,
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    match layout {
        LexicalArtifactLayoutV1::V10 => decode_json_v10(generation, chunk_id, bytes),
        LexicalArtifactLayoutV1::V11
        | LexicalArtifactLayoutV1::V12
        | LexicalArtifactLayoutV1::V13 => decode_compact_v11(generation, chunk_id, bytes),
        LexicalArtifactLayoutV1::V14 => decode_binary_v14(generation, chunk_id, bytes, dictionary),
    }
}

fn decode_json_v10(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let row: ArtifactRowV1 = serde_json::from_slice(bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if row.id.as_str() != chunk_id || &row.anchor.generation_id != generation {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row identity does not match its stored coordinates".to_owned(),
        ));
    }
    Ok(row)
}

fn decode_compact_v11(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let payload = bytes.strip_prefix(ROW_CODEC_V11_MAGIC).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row is missing the compact v11 codec tag".to_owned(),
        )
    })?;
    let compact: ArtifactRowCompactV11 = serde_json::from_slice(payload)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let id = CodeSearchChunkId::new(chunk_id.to_owned())
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let normalized_text = compact.sanitized_text.as_str().to_ascii_lowercase();
    Ok(ArtifactRowV1 {
        id,
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: compact.file_occurrence_id,
            symbol_occurrence_id: compact.symbol_occurrence_id,
            parent_chunk_id: compact.parent_chunk_id,
            source_span: compact.source_span,
            grain: compact.grain,
            ordinal: compact.ordinal,
        },
        language_descriptor_revision: compact.language_descriptor_revision,
        exact_terms: compact.exact_terms,
        sanitized_text: compact.sanitized_text,
        logical_path: compact.logical_path,
        symbol_simple_name: compact.symbol_simple_name,
        symbol_qualified_name: compact.symbol_qualified_name,
        symbol_kind: compact.symbol_kind,
        field_lengths: compact.field_lengths,
        normalized_text,
    })
}

// ---------------------------------------------------------------------------
// Revision 14: binary row with a per-file / per-symbol dictionary
// ---------------------------------------------------------------------------
//
// magic `TDLR14\0`, then in order:
//   ref file entry · opt-ref symbol entry · parent (tag, digest | literal)
//   varint span start/end · u8 grain · varint ordinal
//   varint term count × (u8 kind, bytes, varint span start/end, symbol tag [ref])
//   bytes sanitized_text · u8 field bitmap · varint lengths
//
// A `ref` is the little-endian `row_dictionary.entry_id`; an `opt-ref` is one
// presence byte followed by the ref when present. `bytes` is a varint length
// followed by the bytes. Decoders consume the whole payload and fail closed
// on any trailing byte.

fn encode_binary_v14(
    row: &ArtifactRowV1,
    dictionary: &mut RowDictionaryTableV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut out = Vec::with_capacity(96 + row.sanitized_text.as_str().len());
    out.extend_from_slice(ROW_CODEC_V14_MAGIC);
    put_reference(
        &mut out,
        dictionary,
        &RowDictionaryEntryV1::File {
            file_occurrence_id: row.anchor.file_occurrence_id.as_str().to_owned(),
            logical_path: row.logical_path.clone(),
            language_descriptor_revision: row.language_descriptor_revision.as_str().to_owned(),
        },
    )?;
    let symbol = RowDictionaryEntryV1::Symbol {
        symbol_occurrence_id: row
            .anchor
            .symbol_occurrence_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        simple_name: row.symbol_simple_name.clone(),
        qualified_name: row.symbol_qualified_name.clone(),
        kind: row.symbol_kind.clone(),
    };
    let has_symbol = row.anchor.symbol_occurrence_id.is_some()
        || row.symbol_simple_name.is_some()
        || row.symbol_qualified_name.is_some()
        || row.symbol_kind.is_some();
    if has_symbol {
        out.push(OPTIONAL_PRESENT);
        put_reference(&mut out, dictionary, &symbol)?;
    } else {
        out.push(OPTIONAL_ABSENT);
    }
    match &row.anchor.parent_chunk_id {
        None => out.push(PARENT_NONE),
        Some(parent) => match canonical_chunk_digest(parent.as_str()) {
            Some(digest) => {
                out.push(PARENT_CANONICAL_DIGEST);
                out.extend_from_slice(&digest);
            }
            None => {
                out.push(PARENT_LITERAL);
                put_bytes(&mut out, parent.as_str().as_bytes())?;
            }
        },
    }
    put_varint(&mut out, row.anchor.source_span.start_byte);
    put_varint(&mut out, row.anchor.source_span.end_byte);
    out.push(ordinal_of(&GRAIN_ORDER, &row.anchor.grain, "grain")?);
    put_varint(&mut out, u64::from(row.anchor.ordinal));
    put_varint(&mut out, length_u64(row.exact_terms.len())?);
    for term in &row.exact_terms {
        out.push(ordinal_of(
            &EXACT_TERM_KIND_ORDER,
            &term.kind(),
            "exact term kind",
        )?);
        put_bytes(&mut out, term.original_bytes())?;
        put_varint(&mut out, term.span().start_byte);
        put_varint(&mut out, term.span().end_byte);
        match term.symbol_occurrence_id() {
            None => out.push(TERM_SYMBOL_NONE),
            Some(symbol) if row.anchor.symbol_occurrence_id.as_ref() == Some(symbol) => {
                out.push(TERM_SYMBOL_ROW);
            }
            Some(symbol) => {
                out.push(TERM_SYMBOL_REFERENCE);
                put_reference(
                    &mut out,
                    dictionary,
                    &RowDictionaryEntryV1::Symbol {
                        symbol_occurrence_id: Some(symbol.as_str().to_owned()),
                        simple_name: None,
                        qualified_name: None,
                        kind: None,
                    },
                )?;
            }
        }
    }
    put_bytes(&mut out, row.sanitized_text.as_str().as_bytes())?;
    let mut bitmap = 0u8;
    for (bit, field) in FIELD_LENGTH_ORDER.iter().enumerate() {
        if row.field_lengths.contains_key(field) {
            bitmap |= 1 << bit;
        }
    }
    if row.field_lengths.len() != bitmap.count_ones() as usize {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact row carries a field length outside the encodable field set"
                .to_owned(),
        ));
    }
    out.push(bitmap);
    for field in &FIELD_LENGTH_ORDER {
        if let Some(length) = row.field_lengths.get(field) {
            put_varint(&mut out, length_u64(*length)?);
        }
    }
    Ok(out)
}

fn decode_binary_v14(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
    dictionary: &dyn RowDictionaryV1,
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let payload = bytes.strip_prefix(ROW_CODEC_V14_MAGIC).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row is missing the binary v14 codec tag".to_owned(),
        )
    })?;
    let mut cursor = RowCursorV1 { bytes: payload };
    let file = dictionary.entry(cursor.take_reference()?)?;
    let RowDictionaryEntryV1::File {
        file_occurrence_id,
        logical_path,
        language_descriptor_revision,
    } = file.as_ref()
    else {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row file reference resolved to a non-file entry".to_owned(),
        ));
    };
    let file_occurrence_id = FileOccurrenceId::new(file_occurrence_id.clone()).map_err(corrupt)?;
    let logical_path = logical_path.clone();
    let language_descriptor_revision =
        LanguageDescriptorRevision::new(language_descriptor_revision.clone()).map_err(corrupt)?;
    let (symbol_occurrence_id, symbol_simple_name, symbol_qualified_name, symbol_kind) =
        match cursor.take_optional_reference()? {
            None => (None, None, None, None),
            Some(entry_id) => {
                let (symbol_occurrence_id, simple_name, qualified_name, kind) =
                    symbol_entry_fields(dictionary.entry(entry_id)?.as_ref())?;
                (
                    symbol_occurrence_id
                        .map(SymbolOccurrenceId::new)
                        .transpose()
                        .map_err(corrupt)?,
                    simple_name,
                    qualified_name,
                    kind,
                )
            }
        };
    let parent_chunk_id = match cursor.take_u8()? {
        PARENT_NONE => None,
        PARENT_CANONICAL_DIGEST => {
            let digest = cursor.take_exact(32)?;
            Some(
                CodeSearchChunkId::new(format!(
                    "{CANONICAL_CHUNK_ID_PREFIX}{}",
                    hex::encode(digest)
                ))
                .map_err(corrupt)?,
            )
        }
        PARENT_LITERAL => Some(CodeSearchChunkId::new(cursor.take_string()?).map_err(corrupt)?),
        _ => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row parent tag is unknown".to_owned(),
            ));
        }
    };
    let source_span = SourceSpan {
        start_byte: cursor.take_varint()?,
        end_byte: cursor.take_varint()?,
    };
    let grain = *from_ordinal(&GRAIN_ORDER, cursor.take_u8()?, "grain")?;
    let ordinal = u32::try_from(cursor.take_varint()?).map_err(corrupt)?;
    let term_count = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
    if term_count > cursor.bytes.len() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row exact term count exceeds its payload".to_owned(),
        ));
    }
    let mut exact_terms = Vec::with_capacity(term_count);
    for _ in 0..term_count {
        let kind = *from_ordinal(&EXACT_TERM_KIND_ORDER, cursor.take_u8()?, "exact term kind")?;
        let original_bytes = cursor.take_bytes()?.to_vec();
        let span = SourceSpan {
            start_byte: cursor.take_varint()?,
            end_byte: cursor.take_varint()?,
        };
        let symbol = match cursor.take_u8()? {
            TERM_SYMBOL_NONE => None,
            TERM_SYMBOL_ROW => Some(symbol_occurrence_id.clone().ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact exact term names the row symbol of a symbol-less row"
                        .to_owned(),
                )
            })?),
            TERM_SYMBOL_REFERENCE => {
                let (symbol_occurrence_id, _, _, _) =
                    symbol_entry_fields(dictionary.entry(cursor.take_reference()?)?.as_ref())?;
                let symbol_occurrence_id = symbol_occurrence_id.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact exact term references a symbol entry without identity"
                            .to_owned(),
                    )
                })?;
                Some(SymbolOccurrenceId::new(symbol_occurrence_id).map_err(corrupt)?)
            }
            _ => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact exact term symbol tag is unknown".to_owned(),
                ));
            }
        };
        exact_terms.push(
            ExactTechnicalTermV1::from_persisted_parts(kind, original_bytes, span, symbol)
                .map_err(corrupt)?,
        );
    }
    let sanitized_text = BoundedSanitizedText::new(&cursor.take_string()?).map_err(corrupt)?;
    let bitmap = cursor.take_u8()?;
    if bitmap >> FIELD_LENGTH_ORDER.len() != 0 {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row field bitmap names an unknown field".to_owned(),
        ));
    }
    let mut field_lengths = BTreeMap::new();
    for (bit, field) in FIELD_LENGTH_ORDER.iter().enumerate() {
        if bitmap & (1 << bit) != 0 {
            let length = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
            field_lengths.insert(*field, length);
        }
    }
    if !cursor.bytes.is_empty() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row has trailing bytes".to_owned(),
        ));
    }
    let id = CodeSearchChunkId::new(chunk_id.to_owned()).map_err(corrupt)?;
    let normalized_text = sanitized_text.as_str().to_ascii_lowercase();
    Ok(ArtifactRowV1 {
        id,
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id,
            symbol_occurrence_id,
            parent_chunk_id,
            source_span,
            grain,
            ordinal,
        },
        language_descriptor_revision,
        exact_terms,
        sanitized_text,
        logical_path,
        symbol_simple_name,
        symbol_qualified_name,
        symbol_kind,
        field_lengths,
        normalized_text,
    })
}

type SymbolEntryFieldsV1 = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn symbol_entry_fields(
    entry: &RowDictionaryEntryV1,
) -> Result<SymbolEntryFieldsV1, CodeLexicalArtifactErrorV1> {
    match entry {
        RowDictionaryEntryV1::Symbol {
            symbol_occurrence_id,
            simple_name,
            qualified_name,
            kind,
        } => Ok((
            symbol_occurrence_id.clone(),
            simple_name.clone(),
            qualified_name.clone(),
            kind.clone(),
        )),
        RowDictionaryEntryV1::File { .. } => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row symbol reference resolved to a file entry".to_owned(),
        )),
    }
}

/// The 32 digest bytes of a chunker-minted chunk id, when re-encoding them
/// reproduces the id byte for byte.
fn canonical_chunk_digest(chunk_id: &str) -> Option<[u8; 32]> {
    let hex = chunk_id.strip_prefix(CANONICAL_CHUNK_ID_PREFIX)?;
    let decoded: [u8; 32] = hex::decode(hex).ok()?.try_into().ok()?;
    (hex::encode(decoded) == hex).then_some(decoded)
}

fn put_reference(
    out: &mut Vec<u8>,
    dictionary: &mut RowDictionaryTableV1,
    entry: &RowDictionaryEntryV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let encoded = entry.encode()?;
    let entry_id = stable_row_dictionary_id(&encoded);
    match dictionary.get(&entry_id) {
        Some(existing) if *existing != encoded => {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact row dictionary identifier collided".to_owned(),
            ));
        }
        Some(_) => {}
        None => {
            dictionary.insert(entry_id, encoded);
        }
    }
    out.extend_from_slice(&entry_id.to_le_bytes());
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    put_varint(out, length_u64(bytes.len())?);
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn length_u64(length: usize) -> Result<u64, CodeLexicalArtifactErrorV1> {
    u64::try_from(length).map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn ordinal_of<T: PartialEq>(
    order: &[T],
    value: &T,
    what: &str,
) -> Result<u8, CodeLexicalArtifactErrorV1> {
    order
        .iter()
        .position(|candidate| candidate == value)
        .and_then(|position| u8::try_from(position).ok())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact row {what} has no revision-14 encoding"
            ))
        })
}

fn from_ordinal<'a, T>(
    order: &'a [T],
    ordinal: u8,
    what: &str,
) -> Result<&'a T, CodeLexicalArtifactErrorV1> {
    order.get(usize::from(ordinal)).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact row {what} ordinal {ordinal} is unknown"
        ))
    })
}

fn corrupt(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(error.to_string())
}

struct RowCursorV1<'a> {
    bytes: &'a [u8],
}

impl<'a> RowCursorV1<'a> {
    fn take_exact(&mut self, length: usize) -> Result<&'a [u8], CodeLexicalArtifactErrorV1> {
        if self.bytes.len() < length {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row is truncated".to_owned(),
            ));
        }
        let (head, tail) = self.bytes.split_at(length);
        self.bytes = tail;
        Ok(head)
    }

    fn take_u8(&mut self) -> Result<u8, CodeLexicalArtifactErrorV1> {
        Ok(self.take_exact(1)?[0])
    }

    fn take_reference(&mut self) -> Result<i64, CodeLexicalArtifactErrorV1> {
        let bytes: [u8; 8] = self.take_exact(8)?.try_into().map_err(corrupt)?;
        Ok(i64::from_le_bytes(bytes))
    }

    fn take_optional_reference(&mut self) -> Result<Option<i64>, CodeLexicalArtifactErrorV1> {
        match self.take_u8()? {
            OPTIONAL_ABSENT => Ok(None),
            OPTIONAL_PRESENT => self.take_reference().map(Some),
            _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row optional tag is unknown".to_owned(),
            )),
        }
    }

    /// Canonical LEB128: at most ten bytes, no overflow past 64 bits, and
    /// no zero final byte after a continuation (no padding encodings).
    fn take_varint(&mut self) -> Result<u64, CodeLexicalArtifactErrorV1> {
        let malformed = || {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row varint is malformed".to_owned(),
            )
        };
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.take_u8()?;
            let payload = u64::from(byte & 0x7f);
            if shift > 63 || (shift == 63 && payload > 1) {
                return Err(malformed());
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                if byte == 0 && shift != 0 {
                    return Err(malformed());
                }
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn take_bytes(&mut self) -> Result<&'a [u8], CodeLexicalArtifactErrorV1> {
        let length = usize::try_from(self.take_varint()?).map_err(corrupt)?;
        self.take_exact(length)
    }

    fn take_string(&mut self) -> Result<String, CodeLexicalArtifactErrorV1> {
        String::from_utf8(self.take_bytes()?.to_vec()).map_err(corrupt)
    }

    fn take_optional_string(&mut self) -> Result<Option<String>, CodeLexicalArtifactErrorV1> {
        match self.take_u8()? {
            OPTIONAL_ABSENT => Ok(None),
            OPTIONAL_PRESENT => self.take_string().map(Some),
            _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact dictionary optional tag is unknown".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::{
        ArtifactRowV1, LexicalArtifactLayoutV1, RowDictionaryEntryV1, RowDictionaryTableV1,
        RowDictionaryV1, decode_artifact_row, encode_artifact_row,
    };
    use crate::retrieval::lexical::LexicalFieldV1;
    use crate::retrieval::lexical::projection::artifact::CodeLexicalArtifactErrorV1;
    use tracedecay_domain::{
        BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
        CodeSearchChunkId, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId,
        LanguageDescriptorRevision, SourceSpan, SymbolOccurrenceId,
    };

    impl RowDictionaryV1 for RowDictionaryTableV1 {
        fn entry(
            &self,
            entry_id: i64,
        ) -> Result<Arc<RowDictionaryEntryV1>, CodeLexicalArtifactErrorV1> {
            let bytes = self.get(&entry_id).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(format!("missing entry {entry_id}"))
            })?;
            RowDictionaryEntryV1::decode(bytes).map(Arc::new)
        }
    }

    fn sample_row() -> ArtifactRowV1 {
        let sanitized =
            BoundedSanitizedText::new("fn RenderWidget() { return; }").expect("bounded text");
        ArtifactRowV1 {
            id: CodeSearchChunkId::new("chunk.sample").expect("chunk"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: CodeGenerationId::new("gen.sample").expect("generation"),
                file_occurrence_id: FileOccurrenceId::new("file.sample").expect("file"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 8,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            language_descriptor_revision: LanguageDescriptorRevision::new("lang.1")
                .expect("language"),
            exact_terms: Vec::new(),
            sanitized_text: sanitized.clone(),
            logical_path: "src/sample.rs".to_owned(),
            symbol_simple_name: Some("RenderWidget".to_owned()),
            symbol_qualified_name: Some("sample::RenderWidget".to_owned()),
            symbol_kind: Some("function".to_owned()),
            field_lengths: BTreeMap::from([(LexicalFieldV1::BodyText, 4)]),
            normalized_text: sanitized.as_str().to_ascii_lowercase(),
        }
    }

    /// A file-window row with no symbol fields at all.
    fn window_row() -> ArtifactRowV1 {
        let mut row = sample_row();
        row.symbol_simple_name = None;
        row.symbol_qualified_name = None;
        row.symbol_kind = None;
        row.field_lengths = BTreeMap::from([
            (LexicalFieldV1::Path, 1),
            (LexicalFieldV1::BodyText, 0),
            (LexicalFieldV1::Subtoken, 0),
        ]);
        row
    }

    /// A symbol row the way the chunker emits it: canonical digest ids, a
    /// whole-symbol exact term bound to the row's own symbol, a second term
    /// without symbol authority, and every lexical field populated.
    fn symbol_row() -> ArtifactRowV1 {
        let sanitized = BoundedSanitizedText::new(
            "pub fn cancellation_probe_0001_003(input: u32) -> u32 { input + 3 }",
        )
        .expect("bounded text");
        let symbol = SymbolOccurrenceId::new(format!("symbol.v1.sha256:{}", "e7".repeat(32)))
            .expect("symbol");
        let whole_symbol = ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
            b"cancellation_probe_0001_003".to_vec(),
            SourceSpan {
                start_byte: 211,
                end_byte: 238,
            },
            symbol.clone(),
        )
        .expect("whole symbol term");
        let configuration = ExactTechnicalTermV1::technical(
            ExactTechnicalTermKindV1::ConfigurationKey,
            b"0.1.0".to_vec(),
            SourceSpan {
                start_byte: 42,
                end_byte: 47,
            },
        )
        .expect("configuration term");
        ArtifactRowV1 {
            id: CodeSearchChunkId::new(format!("chunk.v1.sha256:{}", "0a".repeat(32)))
                .expect("chunk"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: CodeGenerationId::new("gen.sample").expect("generation"),
                file_occurrence_id: FileOccurrenceId::new(format!(
                    "file.daemon.{}",
                    "83".repeat(32)
                ))
                .expect("file"),
                symbol_occurrence_id: Some(symbol),
                parent_chunk_id: Some(
                    CodeSearchChunkId::new(format!("chunk.v1.sha256:{}", "c0".repeat(32)))
                        .expect("parent"),
                ),
                source_span: SourceSpan {
                    start_byte: 204,
                    end_byte: 271,
                },
                grain: CodeSearchChunkGrainV1::SymbolSignature,
                ordinal: 9,
            },
            language_descriptor_revision: LanguageDescriptorRevision::new("descriptor.rust.v1")
                .expect("language"),
            exact_terms: vec![whole_symbol, configuration],
            sanitized_text: sanitized.clone(),
            logical_path: "src/cancelled_batch/file_0001.rs".to_owned(),
            symbol_simple_name: Some("cancellation_probe_0001_003".to_owned()),
            symbol_qualified_name: Some(
                "src/cancelled_batch/file_0001.rs::cancellation_probe_0001_003".to_owned(),
            ),
            symbol_kind: Some("function".to_owned()),
            field_lengths: BTreeMap::from([
                (LexicalFieldV1::SymbolName, 1),
                (LexicalFieldV1::QualifiedName, 1),
                (LexicalFieldV1::Path, 1),
                (LexicalFieldV1::BodyText, 9),
                (LexicalFieldV1::ExactTerm, 1),
                (LexicalFieldV1::Subtoken, 10),
            ]),
            normalized_text: sanitized.as_str().to_ascii_lowercase(),
        }
    }

    fn round_trip(
        layout: LexicalArtifactLayoutV1,
        row: &ArtifactRowV1,
    ) -> (Vec<u8>, RowDictionaryTableV1, ArtifactRowV1) {
        let mut dictionary = RowDictionaryTableV1::new();
        let encoded = encode_artifact_row(layout, row, &mut dictionary).expect("encode");
        let decoded = decode_artifact_row(
            layout,
            &row.anchor.generation_id,
            row.id.as_str(),
            &encoded,
            &dictionary,
        )
        .expect("decode");
        (encoded, dictionary, decoded)
    }

    #[test]
    fn compact_v11_round_trip_is_byte_equivalent_to_logical_row() {
        let row = sample_row();
        let (encoded, dictionary, decoded) = round_trip(LexicalArtifactLayoutV1::V11, &row);
        assert!(
            encoded.starts_with(b"TDLR11\0"),
            "v11 rows must carry the compact codec tag"
        );
        assert!(dictionary.is_empty(), "v11 rows carry their strings inline");
        assert_eq!(decoded, row);
        assert!(
            encoded.len() < serde_json::to_vec(&row).expect("json").len(),
            "compact rows must drop repeated identities"
        );
    }

    #[test]
    fn compact_decoder_fails_closed_without_the_v11_tag() {
        let row = sample_row();
        let json = serde_json::to_vec(&row).expect("json");
        let error = decode_artifact_row(
            LexicalArtifactLayoutV1::V11,
            &row.anchor.generation_id,
            row.id.as_str(),
            &json,
            &RowDictionaryTableV1::new(),
        )
        .expect_err("untagged JSON is not a v11 row");
        assert!(error.to_string().contains("compact v11"));
    }

    #[test]
    fn binary_v14_round_trips_symbol_window_and_display_only_rows() {
        for row in [sample_row(), window_row(), symbol_row()] {
            let (encoded, _, decoded) = round_trip(LexicalArtifactLayoutV1::V14, &row);
            assert!(encoded.starts_with(b"TDLR14\0"));
            assert_eq!(decoded, row);
        }
    }

    #[test]
    fn binary_v14_references_one_file_and_one_symbol_entry_per_row() {
        let row = symbol_row();
        let (encoded, dictionary, _) = round_trip(LexicalArtifactLayoutV1::V14, &row);
        let (compact, _, _) = round_trip(LexicalArtifactLayoutV1::V13, &row);
        assert!(
            encoded.len() * 4 < compact.len(),
            "v14 row {} bytes must be under a quarter of the {} byte v11 payload",
            encoded.len(),
            compact.len()
        );
        let entries = dictionary
            .values()
            .map(|bytes| RowDictionaryEntryV1::decode(bytes).expect("entry"))
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2, "one file entry and one symbol entry");
        assert!(entries.contains(&RowDictionaryEntryV1::File {
            file_occurrence_id: row.anchor.file_occurrence_id.as_str().to_owned(),
            logical_path: row.logical_path.clone(),
            language_descriptor_revision: row.language_descriptor_revision.as_str().to_owned(),
        }));
        assert!(
            entries.contains(&RowDictionaryEntryV1::Symbol {
                symbol_occurrence_id: row
                    .anchor
                    .symbol_occurrence_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
                simple_name: row.symbol_simple_name.clone(),
                qualified_name: row.symbol_qualified_name.clone(),
                kind: row.symbol_kind.clone(),
            })
        );
        let parent_hex = "c0".repeat(32);
        assert!(
            !encoded
                .windows(parent_hex.len())
                .any(|window| window == parent_hex.as_bytes()),
            "a canonical parent id is stored as digest bytes, not hex"
        );
        let (_, window_dictionary, _) = round_trip(LexicalArtifactLayoutV1::V14, &window_row());
        assert_eq!(
            window_dictionary.len(),
            1,
            "a symbol-less window row references only its file entry"
        );
    }

    #[test]
    fn binary_v14_keeps_non_canonical_parents_and_foreign_term_symbols_verbatim() {
        let mut row = symbol_row();
        row.anchor.parent_chunk_id =
            Some(CodeSearchChunkId::new("chunk.v1.sha256:NOTHEX").expect("literal parent"));
        let foreign = SymbolOccurrenceId::new("symbol.elsewhere").expect("foreign symbol");
        row.exact_terms[0] = ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
            b"cancellation_probe_0001_003".to_vec(),
            row.exact_terms[0].span(),
            foreign.clone(),
        )
        .expect("foreign whole symbol");
        let (_, dictionary, decoded) = round_trip(LexicalArtifactLayoutV1::V14, &row);
        assert_eq!(decoded, row);
        assert_eq!(
            dictionary.len(),
            3,
            "a foreign exact-term symbol adds its own identity-only entry"
        );
        let uppercase_hex = format!("chunk.v1.sha256:{}", "C0".repeat(32));
        row.anchor.parent_chunk_id = Some(CodeSearchChunkId::new(uppercase_hex).expect("upper"));
        let (_, _, decoded) = round_trip(LexicalArtifactLayoutV1::V14, &row);
        assert_eq!(
            decoded, row,
            "uppercase hex is not canonical and must survive verbatim"
        );
    }

    #[test]
    fn binary_v14_decoder_fails_closed_on_truncation_trailing_bytes_and_missing_entries() {
        let row = symbol_row();
        let (encoded, dictionary, _) = round_trip(LexicalArtifactLayoutV1::V14, &row);
        let decode = |bytes: &[u8], dictionary: &RowDictionaryTableV1| {
            decode_artifact_row(
                LexicalArtifactLayoutV1::V14,
                &row.anchor.generation_id,
                row.id.as_str(),
                bytes,
                dictionary,
            )
        };
        for cut in [7usize, 8, 20, 40, encoded.len() - 1] {
            assert!(
                decode(&encoded[..cut], &dictionary).is_err(),
                "truncated at {cut}"
            );
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode(&trailing, &dictionary).is_err(), "trailing byte");
        let (compact, _, _) = round_trip(LexicalArtifactLayoutV1::V13, &row);
        assert!(
            decode(&compact, &dictionary).is_err(),
            "v11 payload under v14 layout"
        );
        let mut missing = dictionary.clone();
        let file_id = *missing
            .iter()
            .find(|(_, bytes)| bytes.first() == Some(&super::ENTRY_FILE))
            .expect("file entry")
            .0;
        missing.remove(&file_id);
        assert!(
            decode(&encoded, &missing).is_err(),
            "missing dictionary entry"
        );
        let mut swapped = dictionary.clone();
        let symbol_bytes = dictionary
            .values()
            .find(|bytes| bytes.first() == Some(&super::ENTRY_SYMBOL))
            .expect("symbol entry")
            .clone();
        swapped.insert(file_id, symbol_bytes);
        assert!(
            decode(&encoded, &swapped).is_err(),
            "a symbol entry under the file reference is refused"
        );
    }
}
