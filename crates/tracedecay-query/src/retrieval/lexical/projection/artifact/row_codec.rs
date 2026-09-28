use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId,
    LanguageDescriptorRevision, MAX_CHUNK_TEXT_BYTES, SourceSpan, SymbolOccurrenceId,
};

use super::super::LexicalFieldV1;
use super::CodeLexicalArtifactErrorV1;
use super::format::{ArtifactRowV1, deflate_bytes, inflate_bytes};
use super::schema::stable_row_dictionary_id;

/// Rows are stored in blocks of up to `ROW_BLOCK_MAX_ROWS` consecutive
/// documents of one source page, closed early once their uncompressed
/// payload reaches `ROW_BLOCK_TARGET_BYTES`, and deflated as one stream: a
/// read inflates at most one block, and neighbouring chunks of one file
/// share a deflate window (about twice the ratio of per-row deflate).
pub(super) const ROW_BLOCK_MAX_ROWS: usize = 32;
const ROW_BLOCK_TARGET_BYTES: usize = 64 * 1024;
/// Hard bound on one block's inflated payload: a block closes at its target
/// before its last row, and one row holds at most a chunk's text plus its
/// metadata.
const ROW_BLOCK_MAX_INFLATED_BYTES: usize = ROW_BLOCK_TARGET_BYTES + 4 * MAX_CHUNK_TEXT_BYTES;
const ROW_BLOCK_DEFLATE: u8 = 23;
/// Uncompressed scoring preface ahead of the deflated row payload. Lexical
/// ranking reads field lengths from this preface and inflates a block only
/// for phrase or proximity text and for the capped winners.
///
/// On disk the payload is `tag 24`, a little-endian `u32` preface length,
/// that many preface bytes, then the deflated block (whose own encoding
/// starts with tag 23). `decode_row_block` rejects a payload that starts
/// with the deflate tag. A copied parent payload must keep this prefix;
/// `encode_row_blocks` is the writer for a block that changes.
const ROW_BLOCK_PREFACE_TAG: u8 = 24;
const ROW_BLOCK_PREFACE_MAX_BYTES: usize = 16 * 1024;
pub(super) const ROW_BLOCK_PREFACE_PREFIX_BYTES: usize = 5 + ROW_BLOCK_PREFACE_MAX_BYTES;
const BLOCK_CHUNK_DIGEST: u8 = 1;
const BLOCK_CHUNK_LITERAL: u8 = 2;
/// A row's text is stored raw, or as the length of the prefix it shares
/// with the raw text of its parent chunk in the same block (a signature
/// chunk is the first line of its symbol's body chunk).
const BLOCK_TEXT_RAW: u8 = 0;
const BLOCK_TEXT_PARENT_PREFIX: u8 = 1;

/// Chunk identities the chunker mints: `chunk.v1.` followed by a tagged
/// lowercase SHA-256. Revision 14 stores such a parent as its 32 digest bytes
/// and any other shape as the literal string.
const CANONICAL_CHUNK_ID_PREFIX: &str = "chunk.v1.sha256:";
/// Symbol identities the extractor mints, stored the same way in symbol
/// dictionary entries.
const CANONICAL_SYMBOL_ID_PREFIX: &str = "symbol.v1.sha256:";
const PARENT_NONE: u8 = 0;
const PARENT_CANONICAL_DIGEST: u8 = 1;
const PARENT_LITERAL: u8 = 2;
const OPTIONAL_ABSENT: u8 = 0;
const OPTIONAL_PRESENT: u8 = 1;
/// Symbol-entry presence tags beyond `OPTIONAL_PRESENT`: a canonical symbol
/// id as its 32 digest bytes, and a qualified name stored as the suffix
/// after its file's `"<logical path>::"`.
const SYMBOL_ID_CANONICAL_DIGEST: u8 = 2;
const QUALIFIED_NAME_IN_FILE: u8 = 2;
/// An exact term's symbol authority is almost always the row's own symbol;
/// spell that as one byte instead of a second reference.
const TERM_SYMBOL_NONE: u8 = 0;
const TERM_SYMBOL_ROW: u8 = 1;
const TERM_SYMBOL_REFERENCE: u8 = 2;

/// Field order for the `field_lengths` presence bitmap.
const FIELD_LENGTH_ORDER: [LexicalFieldV1; 9] = [
    LexicalFieldV1::SymbolName,
    LexicalFieldV1::QualifiedName,
    LexicalFieldV1::Path,
    LexicalFieldV1::BodyText,
    LexicalFieldV1::PreambleText,
    LexicalFieldV1::ExactTerm,
    LexicalFieldV1::Subtoken,
    LexicalFieldV1::Signature,
    LexicalFieldV1::Documentation,
];

const GRAIN_ORDER: &[CodeSearchChunkGrainV1] = CodeSearchChunkGrainV1::ORDER.as_slice();

const EXACT_TERM_KIND_ORDER: &[ExactTechnicalTermKindV1] =
    ExactTechnicalTermKindV1::ORDER.as_slice();

/// Dictionary entries a revision-14 row references by content-addressed id:
/// one per file (occurrence identity, logical path, descriptor revision) and
/// one per symbol (occurrence identity, names, and kind), and one per symbol
/// display (signature and documentation). Only a symbol's signature-grain
/// rows carry its display, so its body and signature rows share one symbol
/// entry.
/// The encoder fills one table per prepared page; the batch writer stages
/// the union and finalization derives the sealed `row_dictionary`.
pub(super) type RowDictionaryTableV1 = BTreeMap<i64, Vec<u8>>;

const ENTRY_FILE: u8 = 1;
const ENTRY_SYMBOL: u8 = 2;
const ENTRY_SYMBOL_DISPLAY: u8 = 3;
/// Symbol presence tag beyond `OPTIONAL_PRESENT`: a symbol reference
/// followed by its display reference.
const SYMBOL_WITH_DISPLAY: u8 = 2;

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
        qualified_name: Option<QualifiedNameV1>,
        kind: Option<String>,
    },
    SymbolDisplay {
        signature: Option<String>,
        documentation: Option<String>,
    },
}

/// A symbol's qualified name. Parser-attested names almost always spell out
/// their file as `"<logical path>::<suffix>"`; that prefix is the row's file
/// entry and is not stored twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum QualifiedNameV1 {
    Literal(String),
    InFile(String),
}

impl QualifiedNameV1 {
    fn for_path(qualified_name: &str, logical_path: &str) -> Self {
        qualified_name
            .strip_prefix(logical_path)
            .and_then(|suffix| suffix.strip_prefix("::"))
            .map_or_else(
                || Self::Literal(qualified_name.to_owned()),
                |suffix| Self::InFile(suffix.to_owned()),
            )
    }

    fn resolve(&self, logical_path: &str) -> String {
        match self {
            Self::Literal(name) => name.clone(),
            Self::InFile(suffix) => format!("{logical_path}::{suffix}"),
        }
    }
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
                match symbol_occurrence_id
                    .as_deref()
                    .map(|id| (id, canonical_digest(CANONICAL_SYMBOL_ID_PREFIX, id)))
                {
                    None => out.push(OPTIONAL_ABSENT),
                    Some((_, Some(digest))) => {
                        out.push(SYMBOL_ID_CANONICAL_DIGEST);
                        out.extend_from_slice(&digest);
                    }
                    Some((id, None)) => {
                        out.push(OPTIONAL_PRESENT);
                        put_bytes(&mut out, id.as_bytes())?;
                    }
                }
                put_optional_string(&mut out, simple_name.as_deref())?;
                match qualified_name {
                    None => out.push(OPTIONAL_ABSENT),
                    Some(QualifiedNameV1::Literal(name)) => {
                        out.push(OPTIONAL_PRESENT);
                        put_bytes(&mut out, name.as_bytes())?;
                    }
                    Some(QualifiedNameV1::InFile(suffix)) => {
                        out.push(QUALIFIED_NAME_IN_FILE);
                        put_bytes(&mut out, suffix.as_bytes())?;
                    }
                }
                put_optional_string(&mut out, kind.as_deref())?;
            }
            Self::SymbolDisplay {
                signature,
                documentation,
            } => {
                out.push(ENTRY_SYMBOL_DISPLAY);
                put_optional_string(&mut out, signature.as_deref())?;
                put_optional_string(&mut out, documentation.as_deref())?;
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
                symbol_occurrence_id: match cursor.take_u8()? {
                    OPTIONAL_ABSENT => None,
                    OPTIONAL_PRESENT => Some(cursor.take_string()?),
                    SYMBOL_ID_CANONICAL_DIGEST => Some(format!(
                        "{CANONICAL_SYMBOL_ID_PREFIX}{}",
                        hex::encode(cursor.take_exact(32)?)
                    )),
                    _ => {
                        return Err(CodeLexicalArtifactErrorV1::Corrupt(
                            "lexical artifact symbol identity tag is unknown".to_owned(),
                        ));
                    }
                },
                simple_name: cursor.take_optional_string()?,
                qualified_name: match cursor.take_u8()? {
                    OPTIONAL_ABSENT => None,
                    OPTIONAL_PRESENT => Some(QualifiedNameV1::Literal(cursor.take_string()?)),
                    QUALIFIED_NAME_IN_FILE => Some(QualifiedNameV1::InFile(cursor.take_string()?)),
                    _ => {
                        return Err(CodeLexicalArtifactErrorV1::Corrupt(
                            "lexical artifact qualified-name tag is unknown".to_owned(),
                        ));
                    }
                },
                kind: cursor.take_optional_string()?,
            },
            ENTRY_SYMBOL_DISPLAY => Self::SymbolDisplay {
                signature: cursor.take_optional_string()?,
                documentation: cursor.take_optional_string()?,
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

/// Resolves row dictionary references.
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
    row: &ArtifactRowV1,
    dictionary: &mut RowDictionaryTableV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    encode_binary(row, dictionary)
}

/// Decode one row's metadata and restore its text, which the row block
/// stores beside it.
pub(super) fn decode_artifact_row(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
    text: &str,
    dictionary: &dyn RowDictionaryV1,
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    decode_binary(generation, chunk_id, bytes, text, dictionary)
}

// ---------------------------------------------------------------------------
// Binary row with a per-file / per-symbol dictionary
// ---------------------------------------------------------------------------
//
// In order:
//   ref file entry · symbol tag [ref symbol entry [ref display entry]]
//   parent (tag, digest | literal)
//   varint span start/end · u8 grain · varint ordinal
//   varint term count × (u8 kind, bytes, varint span start/end, symbol tag [ref])
//   u16 field bitmap · varint lengths
//
// The sanitized text is not part of the row; its row block stores it.
// A `ref` is the little-endian `row_dictionary.entry_id`. The symbol tag is
// absent (0), a symbol ref (1), or a symbol ref and a display ref (2).
// `bytes` is a varint length
// followed by the bytes. Decoders consume the whole payload and fail closed
// on any trailing byte.

fn encode_binary(
    row: &ArtifactRowV1,
    dictionary: &mut RowDictionaryTableV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut out = Vec::with_capacity(96);
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
        qualified_name: row
            .symbol_qualified_name
            .as_deref()
            .map(|name| QualifiedNameV1::for_path(name, &row.logical_path)),
        kind: row.symbol_kind.clone(),
    };
    let has_symbol = row.anchor.symbol_occurrence_id.is_some()
        || row.symbol_simple_name.is_some()
        || row.symbol_qualified_name.is_some()
        || row.symbol_kind.is_some()
        || row.symbol_signature.is_some()
        || row.symbol_documentation.is_some();
    if row.symbol_signature.is_some() || row.symbol_documentation.is_some() {
        out.push(SYMBOL_WITH_DISPLAY);
        put_reference(&mut out, dictionary, &symbol)?;
        put_reference(
            &mut out,
            dictionary,
            &RowDictionaryEntryV1::SymbolDisplay {
                signature: row.symbol_signature.clone(),
                documentation: row.symbol_documentation.clone(),
            },
        )?;
    } else if has_symbol {
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
    out.push(ordinal_of(GRAIN_ORDER, &row.anchor.grain, "grain")?);
    put_varint(&mut out, u64::from(row.anchor.ordinal));
    put_varint(&mut out, length_u64(row.exact_terms.len())?);
    for term in &row.exact_terms {
        out.push(ordinal_of(
            EXACT_TERM_KIND_ORDER,
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
    encode_field_lengths(&mut out, &row.field_lengths)?;
    Ok(out)
}

fn decode_binary(
    generation: &CodeGenerationId,
    chunk_id: &str,
    bytes: &[u8],
    text: &str,
    dictionary: &dyn RowDictionaryV1,
) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    let mut cursor = RowCursorV1 { bytes };
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
    let (
        symbol_occurrence_id,
        symbol_simple_name,
        symbol_qualified_name,
        symbol_kind,
        symbol_signature,
        symbol_documentation,
    ) = match cursor.take_u8()? {
        OPTIONAL_ABSENT => (None, None, None, None, None, None),
        tag @ (OPTIONAL_PRESENT | SYMBOL_WITH_DISPLAY) => {
            let (symbol_occurrence_id, simple_name, qualified_name, kind) =
                symbol_entry_fields(dictionary.entry(cursor.take_reference()?)?.as_ref())?;
            let (signature, documentation) = if tag == SYMBOL_WITH_DISPLAY {
                symbol_display_fields(dictionary.entry(cursor.take_reference()?)?.as_ref())?
            } else {
                (None, None)
            };
            (
                symbol_occurrence_id
                    .map(SymbolOccurrenceId::new)
                    .transpose()
                    .map_err(corrupt)?,
                simple_name,
                qualified_name.map(|name| name.resolve(&logical_path)),
                kind,
                signature,
                documentation,
            )
        }
        _ => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row symbol tag is unknown".to_owned(),
            ));
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
    let grain = *from_ordinal(GRAIN_ORDER, cursor.take_u8()?, "grain")?;
    let ordinal = u32::try_from(cursor.take_varint()?).map_err(corrupt)?;
    let term_count = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
    if term_count > cursor.bytes.len() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row exact term count exceeds its payload".to_owned(),
        ));
    }
    let mut exact_terms = Vec::with_capacity(term_count);
    for _ in 0..term_count {
        let kind = *from_ordinal(EXACT_TERM_KIND_ORDER, cursor.take_u8()?, "exact term kind")?;
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
    let sanitized_text = BoundedSanitizedText::new(text).map_err(corrupt)?;
    let field_lengths = decode_field_lengths(&mut cursor)?;
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
        symbol_signature,
        symbol_documentation,
        field_lengths,
        normalized_text,
    })
}

type SymbolEntryFieldsV1 = (
    Option<String>,
    Option<String>,
    Option<QualifiedNameV1>,
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
        RowDictionaryEntryV1::File { .. } | RowDictionaryEntryV1::SymbolDisplay { .. } => {
            Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row symbol reference resolved to a non-symbol entry".to_owned(),
            ))
        }
    }
}

fn symbol_display_fields(
    entry: &RowDictionaryEntryV1,
) -> Result<(Option<String>, Option<String>), CodeLexicalArtifactErrorV1> {
    match entry {
        RowDictionaryEntryV1::SymbolDisplay {
            signature,
            documentation,
        } => Ok((signature.clone(), documentation.clone())),
        RowDictionaryEntryV1::File { .. } | RowDictionaryEntryV1::Symbol { .. } => {
            Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row display reference resolved to a non-display entry".to_owned(),
            ))
        }
    }
}

/// The 32 digest bytes of a chunker-minted chunk id, when re-encoding them
/// reproduces the id byte for byte.
pub(super) fn canonical_chunk_digest(chunk_id: &str) -> Option<[u8; 32]> {
    canonical_digest(CANONICAL_CHUNK_ID_PREFIX, chunk_id)
}

/// The value `row_chunks.chunk_id` stores: 32 digest bytes for a canonical
/// chunk id, the literal text otherwise.
pub(super) fn stored_chunk_key(chunk_id: &str) -> rusqlite::types::Value {
    canonical_chunk_digest(chunk_id).map_or_else(
        || rusqlite::types::Value::Text(chunk_id.to_owned()),
        |digest| rusqlite::types::Value::Blob(digest.to_vec()),
    )
}

/// The value `clone_occurrences.symbol_key` stores: 32 digest bytes for an
/// extractor-minted symbol id, the literal text otherwise.
pub(super) fn stored_symbol_key(symbol: &str) -> rusqlite::types::Value {
    canonical_digest(CANONICAL_SYMBOL_ID_PREFIX, symbol).map_or_else(
        || rusqlite::types::Value::Text(symbol.to_owned()),
        |digest| rusqlite::types::Value::Blob(digest.to_vec()),
    )
}

/// Inverse of [`stored_symbol_key`].
pub(super) fn symbol_id_from_key(
    key: rusqlite::types::ValueRef<'_>,
) -> Result<String, CodeLexicalArtifactErrorV1> {
    match key {
        rusqlite::types::ValueRef::Blob(digest) if digest.len() == 32 => Ok(format!(
            "{CANONICAL_SYMBOL_ID_PREFIX}{}",
            hex::encode(digest)
        )),
        rusqlite::types::ValueRef::Text(text) => std::str::from_utf8(text)
            .map(str::to_owned)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string())),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone occurrence symbol key is malformed".to_owned(),
        )),
    }
}

/// One row as a page hands it to [`encode_row_blocks`].
///
/// `field_lengths` and `trimmed_normalized_len` are written into the
/// uncompressed preface. They are not recovered from `row`. A rewrite of a
/// stored block, including a block copied from a parent artifact, passes the
/// lengths `decode_artifact_row` reports and the trimmed length of that
/// row's normalized text.
pub(super) struct BlockRowV1<'a> {
    pub(super) document_id: i64,
    pub(super) chunk_id: &'a str,
    pub(super) parent_chunk_id: Option<&'a str>,
    pub(super) row: &'a [u8],
    pub(super) text: &'a str,
    pub(super) field_lengths: &'a BTreeMap<LexicalFieldV1, usize>,
    /// Byte length of `normalized_text.trim()`, the echo-penalty comparison.
    pub(super) trimmed_normalized_len: usize,
}

/// One row restored from its block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StoredRowV1 {
    pub(super) document_id: u32,
    pub(super) chunk_id: String,
    pub(super) row: Vec<u8>,
    pub(super) text: String,
}

/// Split one page's rows (ascending documents) into stored blocks, each
/// keyed by its first document.
pub(super) fn encode_row_blocks(
    rows: &[BlockRowV1<'_>],
) -> Result<Vec<(i64, Vec<u8>)>, CodeLexicalArtifactErrorV1> {
    let mut blocks = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let mut end = start;
        let mut bytes = 0usize;
        while end < rows.len() && end - start < ROW_BLOCK_MAX_ROWS && bytes < ROW_BLOCK_TARGET_BYTES
        {
            bytes = bytes
                .saturating_add(rows[end].row.len())
                .saturating_add(rows[end].text.len());
            end += 1;
        }
        blocks.push((
            rows[start].document_id,
            encode_row_block(&rows[start..end])?,
        ));
        start = end;
    }
    Ok(blocks)
}

fn encode_row_block(rows: &[BlockRowV1<'_>]) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    // A row may name only a parent whose own text is stored raw.
    let candidate = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let parent = row.parent_chunk_id?;
            rows.iter()
                .position(|other| other.chunk_id == parent)
                .filter(|&position| {
                    position != index
                        && !row.text.is_empty()
                        && rows[position].text.starts_with(row.text)
                })
        })
        .collect::<Vec<_>>();
    let mut payload = Vec::new();
    let mut preface = Vec::new();
    put_varint(&mut payload, length_u64(rows.len())?);
    put_varint(&mut preface, length_u64(rows.len())?);
    let mut previous: Option<i64> = None;
    for (index, row) in rows.iter().enumerate() {
        // The first row spells its document out, binding the block to its key.
        let gap = document_gap(previous, row.document_id)?;
        previous = Some(row.document_id);
        put_varint(&mut payload, gap);
        put_varint(&mut preface, gap);
        put_chunk_id(&mut payload, row.chunk_id)?;
        put_chunk_id(&mut preface, row.chunk_id)?;
        put_bytes(&mut payload, row.row)?;
        encode_field_lengths(&mut preface, row.field_lengths)?;
        put_varint(&mut preface, length_u64(row.trimmed_normalized_len)?);
        match candidate[index].filter(|parent| candidate[*parent].is_none()) {
            Some(parent) => {
                payload.push(BLOCK_TEXT_PARENT_PREFIX);
                put_varint(&mut payload, length_u64(parent)?);
                put_varint(&mut payload, length_u64(row.text.len())?);
            }
            None => {
                payload.push(BLOCK_TEXT_RAW);
                put_bytes(&mut payload, row.text.as_bytes())?;
            }
        }
    }
    if payload.len() > ROW_BLOCK_MAX_INFLATED_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact row block exceeds its inflated bound".to_owned(),
        ));
    }
    if preface.len() > ROW_BLOCK_PREFACE_MAX_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact row block scoring preface exceeds its bound".to_owned(),
        ));
    }
    let deflated = deflate_bytes(ROW_BLOCK_DEFLATE, &payload)?;
    wrap_scoring_preface(&preface, &deflated)
}

fn document_gap(
    previous: Option<i64>,
    document_id: i64,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    match previous {
        None => u64::try_from(document_id)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string())),
        Some(previous) => document_id
            .checked_sub(previous)
            .and_then(|delta| delta.checked_sub(1))
            .and_then(|gap| u64::try_from(gap).ok())
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact row block documents are not ascending".to_owned(),
                )
            }),
    }
}

fn put_chunk_id(out: &mut Vec<u8>, chunk_id: &str) -> Result<(), CodeLexicalArtifactErrorV1> {
    match canonical_chunk_digest(chunk_id) {
        Some(digest) => {
            out.push(BLOCK_CHUNK_DIGEST);
            out.extend_from_slice(&digest);
            Ok(())
        }
        None => {
            out.push(BLOCK_CHUNK_LITERAL);
            put_bytes(out, chunk_id.as_bytes())
        }
    }
}

fn wrap_scoring_preface(
    preface: &[u8],
    deflated: &[u8],
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let len = u32::try_from(preface.len())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut stored = Vec::with_capacity(5 + preface.len() + deflated.len());
    stored.push(ROW_BLOCK_PREFACE_TAG);
    stored.extend_from_slice(&len.to_le_bytes());
    stored.extend_from_slice(preface);
    stored.extend_from_slice(deflated);
    Ok(stored)
}

enum BlockChunkV1 {
    Digest(Range<usize>),
    Literal(Range<usize>),
}

enum BlockTextV1 {
    Raw(Range<usize>),
    ParentPrefix { parent: usize, length: usize },
}

struct BlockEntryV1 {
    document_id: u32,
    chunk: BlockChunkV1,
    row: Range<usize>,
    text: BlockTextV1,
}

/// One inflated, structurally verified row block. The scoring preface is
/// outside this payload: a sparse reader inflates a block only when it
/// needs the row text.
pub(super) struct RowBlockV1 {
    payload: Vec<u8>,
    entries: Vec<BlockEntryV1>,
}

impl RowBlockV1 {
    /// Inflate one stored block (bounded by `ROW_BLOCK_MAX_INFLATED_BYTES`)
    /// and index its rows, failing closed on any malformed or trailing byte.
    pub(super) fn parse(
        first_document: i64,
        stored: &[u8],
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let (preface, deflated) = split_scoring_preface(stored)?;
        let preface_rows = decode_preface_rows(first_document, preface)?;
        let payload = inflate_bytes(ROW_BLOCK_DEFLATE, deflated, ROW_BLOCK_MAX_INFLATED_BYTES)?;
        let total = payload.len();
        let mut cursor = RowCursorV1 { bytes: &payload };
        let offset = |cursor: &RowCursorV1<'_>| total - cursor.bytes.len();
        let count = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
        if count == 0 || count > ROW_BLOCK_MAX_ROWS {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row block row count is out of range".to_owned(),
            ));
        }
        let mut entries = Vec::with_capacity(count);
        let mut document = first_document;
        for index in 0..count {
            let gap = i64::try_from(cursor.take_varint()?).map_err(corrupt)?;
            document = if index == 0 {
                if gap != first_document {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row block does not start at its key".to_owned(),
                    ));
                }
                first_document
            } else {
                document
                    .checked_add(1)
                    .and_then(|next| next.checked_add(gap))
                    .ok_or_else(|| corrupt("lexical artifact row block document overflowed"))?
            };
            let chunk = match cursor.take_u8()? {
                BLOCK_CHUNK_DIGEST => {
                    let start = offset(&cursor);
                    cursor.take_exact(32)?;
                    BlockChunkV1::Digest(start..offset(&cursor))
                }
                BLOCK_CHUNK_LITERAL => {
                    let literal = cursor.take_bytes()?;
                    let end = offset(&cursor);
                    BlockChunkV1::Literal(end - literal.len()..end)
                }
                _ => {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row block chunk tag is unknown".to_owned(),
                    ));
                }
            };
            let row = cursor.take_bytes()?;
            let row = offset(&cursor) - row.len()..offset(&cursor);
            let text = match cursor.take_u8()? {
                BLOCK_TEXT_RAW => {
                    let text = cursor.take_bytes()?;
                    let end = offset(&cursor);
                    BlockTextV1::Raw(end - text.len()..end)
                }
                BLOCK_TEXT_PARENT_PREFIX => BlockTextV1::ParentPrefix {
                    parent: usize::try_from(cursor.take_varint()?).map_err(corrupt)?,
                    length: usize::try_from(cursor.take_varint()?).map_err(corrupt)?,
                },
                _ => {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row block text tag is unknown".to_owned(),
                    ));
                }
            };
            entries.push(BlockEntryV1 {
                document_id: u32::try_from(document).map_err(corrupt)?,
                chunk,
                row,
                text,
            });
        }
        if !cursor.bytes.is_empty() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row block has trailing bytes".to_owned(),
            ));
        }
        for (index, entry) in entries.iter().enumerate() {
            if let BlockTextV1::ParentPrefix { parent, length } = entry.text {
                let valid = parent != index
                    && length > 0
                    && matches!(
                        entries.get(parent).map(|parent| &parent.text),
                        Some(BlockTextV1::Raw(text)) if length <= text.len()
                    );
                if !valid {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row block prefix names no raw parent row".to_owned(),
                    ));
                }
            }
        }
        if preface_rows.len() != entries.len() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact scoring preface row count does not match its block".to_owned(),
            ));
        }
        for (preface, entry) in preface_rows.iter().zip(&entries) {
            if preface.document_id != entry.document_id
                || preface.chunk_id != chunk_id_of(&payload, &entry.chunk)?
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact scoring preface does not match its block".to_owned(),
                ));
            }
        }
        Ok(Self { payload, entries })
    }

    /// The row stored for `document`, when this block holds it.
    pub(super) fn row(
        &self,
        document: u32,
    ) -> Option<Result<StoredRowV1, CodeLexicalArtifactErrorV1>> {
        self.entries
            .binary_search_by_key(&document, |entry| entry.document_id)
            .ok()
            .map(|index| self.materialize(index))
    }

    pub(super) fn rows(&self) -> Result<Vec<StoredRowV1>, CodeLexicalArtifactErrorV1> {
        (0..self.entries.len())
            .map(|index| self.materialize(index))
            .collect()
    }

    fn materialize(&self, index: usize) -> Result<StoredRowV1, CodeLexicalArtifactErrorV1> {
        let entry = &self.entries[index];
        let chunk_id = chunk_id_of(&self.payload, &entry.chunk)?;
        let text = match entry.text {
            BlockTextV1::Raw(ref range) => &self.payload[range.clone()],
            BlockTextV1::ParentPrefix { parent, length } => match &self.entries[parent].text {
                BlockTextV1::Raw(range) => &self.payload[range.start..range.start + length],
                BlockTextV1::ParentPrefix { .. } => {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row block prefix names no raw parent row".to_owned(),
                    ));
                }
            },
        };
        Ok(StoredRowV1 {
            document_id: entry.document_id,
            chunk_id,
            row: self.payload[entry.row.clone()].to_vec(),
            text: std::str::from_utf8(text).map_err(corrupt)?.to_owned(),
        })
    }
}

/// Every row of one stored block.
pub(super) fn decode_row_block(
    first_document: i64,
    stored: &[u8],
) -> Result<Vec<StoredRowV1>, CodeLexicalArtifactErrorV1> {
    RowBlockV1::parse(first_document, stored)?.rows()
}

fn chunk_id_of(payload: &[u8], chunk: &BlockChunkV1) -> Result<String, CodeLexicalArtifactErrorV1> {
    match chunk {
        BlockChunkV1::Digest(range) => Ok(format!(
            "{CANONICAL_CHUNK_ID_PREFIX}{}",
            hex::encode(&payload[range.clone()])
        )),
        BlockChunkV1::Literal(range) => {
            String::from_utf8(payload[range.clone()].to_vec()).map_err(corrupt)
        }
    }
}

/// Field lengths and the trimmed normalized-text length for one document,
/// readable without inflating the row block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScoringPrefaceRowV1 {
    pub(super) document_id: u32,
    pub(super) chunk_id: String,
    pub(super) field_lengths: BTreeMap<LexicalFieldV1, usize>,
    pub(super) trimmed_normalized_len: usize,
}

fn split_scoring_preface(stored: &[u8]) -> Result<(&[u8], &[u8]), CodeLexicalArtifactErrorV1> {
    let corrupt = |detail: &str| {
        CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact row block scoring preface {detail}"
        ))
    };
    let Some((&tag, rest)) = stored.split_first() else {
        return Err(corrupt("is empty"));
    };
    if tag != ROW_BLOCK_PREFACE_TAG {
        return Err(corrupt("has an unknown tag"));
    }
    if rest.len() < 4 {
        return Err(corrupt("is truncated"));
    }
    let len =
        u32::from_le_bytes(rest[..4].try_into().map_err(|_| corrupt("is truncated"))?) as usize;
    if len > ROW_BLOCK_PREFACE_MAX_BYTES {
        return Err(corrupt("exceeds its bound"));
    }
    let rest = &rest[4..];
    if rest.len() < len {
        return Err(corrupt("is truncated"));
    }
    Ok((&rest[..len], &rest[len..]))
}

fn decode_preface_rows(
    first_document: i64,
    preface: &[u8],
) -> Result<Vec<ScoringPrefaceRowV1>, CodeLexicalArtifactErrorV1> {
    let mut cursor = RowCursorV1 { bytes: preface };
    let count = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
    if count == 0 || count > ROW_BLOCK_MAX_ROWS {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact scoring preface row count is out of range".to_owned(),
        ));
    }
    let mut rows = Vec::with_capacity(count);
    let mut document = first_document;
    for index in 0..count {
        let gap = i64::try_from(cursor.take_varint()?).map_err(corrupt)?;
        document = if index == 0 {
            if gap != first_document {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact scoring preface does not start at its key".to_owned(),
                ));
            }
            first_document
        } else {
            document
                .checked_add(1)
                .and_then(|next| next.checked_add(gap))
                .ok_or_else(|| corrupt("lexical artifact scoring preface document overflowed"))?
        };
        let chunk_id = take_chunk_id(&mut cursor)?;
        let field_lengths = decode_field_lengths(&mut cursor)?;
        let trimmed_normalized_len = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
        rows.push(ScoringPrefaceRowV1 {
            document_id: u32::try_from(document).map_err(corrupt)?,
            chunk_id,
            field_lengths,
            trimmed_normalized_len,
        });
    }
    if !cursor.bytes.is_empty() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact scoring preface has trailing bytes".to_owned(),
        ));
    }
    Ok(rows)
}

fn take_chunk_id(cursor: &mut RowCursorV1<'_>) -> Result<String, CodeLexicalArtifactErrorV1> {
    match cursor.take_u8()? {
        BLOCK_CHUNK_DIGEST => {
            let digest = cursor.take_exact(32)?;
            Ok(format!(
                "{CANONICAL_CHUNK_ID_PREFIX}{}",
                hex::encode(digest)
            ))
        }
        BLOCK_CHUNK_LITERAL => cursor.take_string(),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact scoring preface chunk tag is unknown".to_owned(),
        )),
    }
}

fn encode_field_lengths(
    out: &mut Vec<u8>,
    lengths: &BTreeMap<LexicalFieldV1, usize>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut bitmap = 0u16;
    for (bit, field) in FIELD_LENGTH_ORDER.iter().enumerate() {
        if lengths.contains_key(field) {
            bitmap |= 1 << bit;
        }
    }
    let expected_fields = lengths
        .keys()
        .filter(|field| FIELD_LENGTH_ORDER.contains(field))
        .count();
    if expected_fields != bitmap.count_ones() as usize {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact row carries a field length outside the encodable field set"
                .to_owned(),
        ));
    }
    out.extend_from_slice(&bitmap.to_le_bytes());
    for field in &FIELD_LENGTH_ORDER {
        if let Some(length) = lengths.get(field) {
            put_varint(out, length_u64(*length)?);
        }
    }
    Ok(())
}

fn decode_field_lengths(
    cursor: &mut RowCursorV1<'_>,
) -> Result<BTreeMap<LexicalFieldV1, usize>, CodeLexicalArtifactErrorV1> {
    let bitmap = cursor.take_u16()?;
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
    Ok(field_lengths)
}

/// The one block holding a document: the greatest block key at or below it.
pub(super) const ROW_BLOCK_BY_DOCUMENT_SQL: &str = "SELECT first_document, payload FROM row_blocks WHERE first_document <= ?1 ORDER BY first_document DESC LIMIT 1";

/// Every block's scoring preface, in document order. One scan replaces the
/// per-query window that re-decoded the corpus for each common-term walk.
pub(super) const ROW_BLOCK_PREFACE_SCAN_SQL: &str =
    "SELECT first_document, substr(payload, 1, ?1) FROM row_blocks ORDER BY first_document";

/// Every block's encoded scoring preface, loaded once per reader. It keeps
/// the stored encoding (tens of bytes per document) instead of decoded rows,
/// which cost several heap allocations each; a lookup finds the block by key
/// and decodes only the requested row.
pub(super) struct ScoringPrefaceIndexV1 {
    /// Block keys, strictly ascending, each with the end of its preface in
    /// `bytes`. A block's preface starts where the previous one ends.
    blocks: Vec<(u32, u32)>,
    bytes: Vec<u8>,
}

impl ScoringPrefaceIndexV1 {
    pub(super) fn empty() -> &'static Self {
        static EMPTY: ScoringPrefaceIndexV1 = ScoringPrefaceIndexV1 {
            blocks: Vec::new(),
            bytes: Vec::new(),
        };
        &EMPTY
    }

    #[cfg(feature = "hotpath")]
    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes.capacity() + self.blocks.capacity() * std::mem::size_of::<(u32, u32)>()
    }

    pub(super) fn get(
        &self,
        document: u32,
    ) -> Result<Option<ScoringPrefaceRowV1>, CodeLexicalArtifactErrorV1> {
        let Some(block) = self
            .blocks
            .partition_point(|(first, _)| *first <= document)
            .checked_sub(1)
        else {
            return Ok(None);
        };
        let start = block
            .checked_sub(1)
            .map_or(0, |previous| self.blocks[previous].1 as usize);
        let (first_document, end) = self.blocks[block];
        find_preface_row(
            i64::from(first_document),
            &self.bytes[start..end as usize],
            i64::from(document),
        )
    }

    fn push(
        &mut self,
        first_document: i64,
        preface: &[u8],
        ceiling_bytes: usize,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        let first_document = u32::try_from(first_document).map_err(corrupt)?;
        if self
            .blocks
            .last()
            .is_some_and(|(previous, _)| *previous >= first_document)
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact scoring prefaces are not strictly ascending".to_owned(),
            ));
        }
        let end = self.bytes.len() + preface.len();
        if end + (self.blocks.len() + 1) * std::mem::size_of::<(u32, u32)>() > ceiling_bytes {
            return Err(CodeLexicalArtifactErrorV1::Unreserved(format!(
                "lexical artifact scoring prefaces exceed the reader's {ceiling_bytes}-byte headroom"
            )));
        }
        self.blocks
            .push((first_document, u32::try_from(end).map_err(corrupt)?));
        self.bytes.extend_from_slice(preface);
        Ok(())
    }
}

/// The preface row for `document` in one block, skipping earlier rows
/// without decoding their chunk ids or field lengths.
fn find_preface_row(
    first_document: i64,
    preface: &[u8],
    document: i64,
) -> Result<Option<ScoringPrefaceRowV1>, CodeLexicalArtifactErrorV1> {
    let mut cursor = RowCursorV1 { bytes: preface };
    let count = usize::try_from(cursor.take_varint()?).map_err(corrupt)?;
    if count == 0 || count > ROW_BLOCK_MAX_ROWS {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact scoring preface row count is out of range".to_owned(),
        ));
    }
    let mut current = first_document;
    for index in 0..count {
        let gap = i64::try_from(cursor.take_varint()?).map_err(corrupt)?;
        current = if index == 0 {
            if gap != first_document {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact scoring preface does not start at its key".to_owned(),
                ));
            }
            first_document
        } else {
            current
                .checked_add(1)
                .and_then(|next| next.checked_add(gap))
                .ok_or_else(|| corrupt("lexical artifact scoring preface document overflowed"))?
        };
        if current > document {
            return Ok(None);
        }
        if current == document {
            return Ok(Some(ScoringPrefaceRowV1 {
                document_id: u32::try_from(document).map_err(corrupt)?,
                chunk_id: take_chunk_id(&mut cursor)?,
                field_lengths: decode_field_lengths(&mut cursor)?,
                trimmed_normalized_len: usize::try_from(cursor.take_varint()?).map_err(corrupt)?,
            }));
        }
        match cursor.take_u8()? {
            BLOCK_CHUNK_DIGEST => {
                cursor.take_exact(32)?;
            }
            BLOCK_CHUNK_LITERAL => {
                cursor.take_bytes()?;
            }
            _ => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact scoring preface chunk tag is unknown".to_owned(),
                ));
            }
        }
        let bitmap = cursor.take_u16()?;
        if bitmap >> FIELD_LENGTH_ORDER.len() != 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row field bitmap names an unknown field".to_owned(),
            ));
        }
        // One varint per present field length, then the trimmed length.
        for _ in 0..bitmap.count_ones() + 1 {
            cursor.take_varint()?;
        }
    }
    Ok(None)
}

/// Load every block's preface, refusing with `Unreserved` once the encoded
/// prefaces would exceed `ceiling_bytes`.
pub(super) fn load_scoring_preface_index(
    connection: &Connection,
    ceiling_bytes: usize,
    mut on_block: impl FnMut() -> Result<(), CodeLexicalArtifactErrorV1>,
) -> Result<ScoringPrefaceIndexV1, CodeLexicalArtifactErrorV1> {
    let prefix = i64::try_from(ROW_BLOCK_PREFACE_PREFIX_BYTES)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut statement = connection
        .prepare_cached(ROW_BLOCK_PREFACE_SCAN_SQL)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut scanned = statement
        .query(params![prefix])
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut index = ScoringPrefaceIndexV1 {
        blocks: Vec::new(),
        bytes: Vec::new(),
    };
    while let Some(row) = scanned
        .next()
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
    {
        on_block()?;
        let first_document: i64 = row
            .get(0)
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let stored = row
            .get_ref(1)
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
            .as_blob()
            .map_err(corrupt)?;
        let (preface, _) = split_scoring_preface(stored)?;
        index.push(first_document, preface, ceiling_bytes)?;
    }
    drop(scanned);
    index.blocks.shrink_to_fit();
    index.bytes.shrink_to_fit();
    Ok(index)
}

/// Rows by document over an open artifact connection. Scoring reads the
/// reader-owned preface index. Inflation is reserved for phrase or proximity
/// text and for the capped winners.
pub(super) struct RowBlocksV1<'a> {
    connection: &'a Connection,
    block: RefCell<Option<RowBlockV1>>,
    prefaces: &'a ScoringPrefaceIndexV1,
}

impl<'a> RowBlocksV1<'a> {
    pub(super) fn new(connection: &'a Connection, prefaces: &'a ScoringPrefaceIndexV1) -> Self {
        Self {
            connection,
            block: RefCell::new(None),
            prefaces,
        }
    }

    pub(super) fn scoring_preface(
        &self,
        document: u32,
    ) -> Result<ScoringPrefaceRowV1, CodeLexicalArtifactErrorV1> {
        self.prefaces
            .get(document)?
            .ok_or_else(missing_document_row)
    }

    pub(super) fn row(&self, document: u32) -> Result<StoredRowV1, CodeLexicalArtifactErrorV1> {
        if let Some(row) = self
            .block
            .borrow()
            .as_ref()
            .and_then(|block| block.row(document))
        {
            return row;
        }
        let mut statement = self
            .connection
            .prepare_cached(ROW_BLOCK_BY_DOCUMENT_SQL)
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let block: Option<(i64, Vec<u8>)> = statement
            .query_row([i64::from(document)], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let (first_document, payload) = block.ok_or_else(missing_document_row)?;
        let block = RowBlockV1::parse(first_document, &payload)?;
        let row = block.row(document).ok_or_else(missing_document_row)?;
        *self.block.borrow_mut() = Some(block);
        row
    }
}

fn missing_document_row() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt("lexical artifact document has no stored row".to_owned())
}

fn canonical_digest(prefix: &str, id: &str) -> Option<[u8; 32]> {
    let hex = id.strip_prefix(prefix)?;
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

fn put_optional_string(
    out: &mut Vec<u8>,
    value: Option<&str>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    match value {
        None => out.push(OPTIONAL_ABSENT),
        Some(value) => {
            out.push(OPTIONAL_PRESENT);
            put_bytes(out, value.as_bytes())?;
        }
    }
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

    fn take_u16(&mut self) -> Result<u16, CodeLexicalArtifactErrorV1> {
        let bytes: [u8; 2] = self.take_exact(2)?.try_into().map_err(corrupt)?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn take_reference(&mut self) -> Result<i64, CodeLexicalArtifactErrorV1> {
        let bytes: [u8; 8] = self.take_exact(8)?.try_into().map_err(corrupt)?;
        Ok(i64::from_le_bytes(bytes))
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use rusqlite::{Connection, params};

    use super::{
        ArtifactRowV1, BlockRowV1, QualifiedNameV1, ROW_BLOCK_MAX_ROWS, RowBlocksV1, RowCursorV1,
        RowDictionaryEntryV1, RowDictionaryTableV1, RowDictionaryV1, StoredRowV1,
        decode_artifact_row, decode_row_block, encode_artifact_row, encode_row_blocks,
        find_preface_row, load_scoring_preface_index, split_scoring_preface, wrap_scoring_preface,
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
        let normalized_text = sanitized.as_str().to_ascii_lowercase();
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
            symbol_signature: None,
            symbol_documentation: None,
            field_lengths: BTreeMap::from([(LexicalFieldV1::BodyText, 4)]),
            normalized_text,
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
        let normalized_text = sanitized.as_str().to_ascii_lowercase();
        let signature = "pub fn cancellation_probe_0001_003(input: u32) -> u32";
        let documentation = "Cancels one probe after its bounded input.";
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
            symbol_signature: Some(signature.to_owned()),
            symbol_documentation: Some(documentation.to_owned()),
            field_lengths: BTreeMap::from([
                (LexicalFieldV1::SymbolName, 1),
                (LexicalFieldV1::QualifiedName, 1),
                (LexicalFieldV1::Path, 1),
                (LexicalFieldV1::Signature, 6),
                (LexicalFieldV1::Documentation, 7),
                (LexicalFieldV1::BodyText, 9),
                (LexicalFieldV1::ExactTerm, 1),
                (LexicalFieldV1::Subtoken, 10),
            ]),
            normalized_text,
        }
    }

    fn legacy_symbol_row() -> ArtifactRowV1 {
        let mut row = symbol_row();
        row.symbol_signature = None;
        row.symbol_documentation = None;
        row.field_lengths.remove(&LexicalFieldV1::Signature);
        row.field_lengths.remove(&LexicalFieldV1::Documentation);
        row
    }

    fn round_trip(row: &ArtifactRowV1) -> (Vec<u8>, RowDictionaryTableV1, ArtifactRowV1) {
        let mut dictionary = RowDictionaryTableV1::new();
        let encoded = encode_artifact_row(row, &mut dictionary).expect("encode");
        let decoded = decode_artifact_row(
            &row.anchor.generation_id,
            row.id.as_str(),
            &encoded,
            row.sanitized_text.as_str(),
            &dictionary,
        )
        .expect("decode");
        (encoded, dictionary, decoded)
    }

    #[test]
    fn binary_rows_round_trip_without_their_text() {
        for row in [
            sample_row(),
            window_row(),
            legacy_symbol_row(),
            symbol_row(),
        ] {
            let (encoded, _, decoded) = round_trip(&row);
            assert_eq!(decoded, row);
            assert!(
                !encoded
                    .windows(row.sanitized_text.as_str().len())
                    .any(|window| window == row.sanitized_text.as_str().as_bytes()),
                "the row block, not the row, stores the text"
            );
        }
    }

    #[test]
    fn binary_v14_references_one_file_and_one_symbol_entry_per_row() {
        let row = legacy_symbol_row();
        let (encoded, dictionary, _) = round_trip(&row);
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
                qualified_name: row
                    .symbol_qualified_name
                    .as_deref()
                    .map(|name| QualifiedNameV1::for_path(name, &row.logical_path)),
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
        let (_, window_dictionary, _) = round_trip(&window_row());
        assert_eq!(
            window_dictionary.len(),
            1,
            "a symbol-less window row references only its file entry"
        );
    }

    fn block_row<'a>(
        document_id: i64,
        chunk_id: &'a str,
        parent_chunk_id: Option<&'a str>,
        text: &'a str,
    ) -> BlockRowV1<'a> {
        BlockRowV1 {
            document_id,
            chunk_id,
            parent_chunk_id,
            row: b"meta",
            text,
            field_lengths: empty_field_lengths(),
            trimmed_normalized_len: text.trim().len(),
        }
    }

    fn empty_field_lengths() -> &'static BTreeMap<LexicalFieldV1, usize> {
        use std::sync::OnceLock;
        static EMPTY: OnceLock<BTreeMap<LexicalFieldV1, usize>> = OnceLock::new();
        EMPTY.get_or_init(BTreeMap::new)
    }

    #[test]
    fn scoring_preface_round_trips_real_fields_and_rejects_mismatched_identity() {
        let row = symbol_row();
        let (encoded, dictionary, _) = round_trip(&row);
        let blocks = encode_row_blocks(&[BlockRowV1 {
            document_id: 7,
            chunk_id: row.id.as_str(),
            parent_chunk_id: row.anchor.parent_chunk_id.as_ref().map(|id| id.as_str()),
            row: &encoded,
            text: row.sanitized_text.as_str(),
            field_lengths: &row.field_lengths,
            trimmed_normalized_len: row.normalized_text.trim().len(),
        }])
        .expect("encode real symbol row block");
        let (first, stored) = &blocks[0];
        let (preface, _) = split_scoring_preface(stored).expect("split envelope");
        let scored = find_preface_row(*first, preface, 7)
            .expect("decode preface")
            .expect("document 7 has a preface");
        let restored = decode_row_block(*first, stored).expect("decode row block");
        assert_eq!(restored.len(), 1);
        let decoded = decode_artifact_row(
            &row.anchor.generation_id,
            &restored[0].chunk_id,
            &restored[0].row,
            &restored[0].text,
            &dictionary,
        )
        .expect("decode real symbol metadata");
        assert!(!decoded.field_lengths.is_empty());
        assert_eq!(scored.document_id, restored[0].document_id);
        assert_eq!(scored.chunk_id, decoded.id.as_str());
        assert_eq!(scored.field_lengths, decoded.field_lengths);
        assert_eq!(
            scored.trimmed_normalized_len,
            decoded.normalized_text.trim().len()
        );
        assert_eq!(decoded, row);

        let (preface, deflated) = split_scoring_preface(stored).expect("split envelope");
        let mut cursor = RowCursorV1 { bytes: preface };
        assert_eq!(cursor.take_varint().unwrap(), 1);
        let document_offset = preface.len() - cursor.bytes.len();
        assert_eq!(cursor.take_varint().unwrap(), 7);
        assert_eq!(cursor.take_u8().unwrap(), super::BLOCK_CHUNK_DIGEST);
        let chunk_offset = preface.len() - cursor.bytes.len();

        let mut changed_document = preface.to_vec();
        changed_document[document_offset] = 8;
        let damaged = wrap_scoring_preface(&changed_document, deflated).unwrap();
        assert!(matches!(
            decode_row_block(*first, &damaged),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));

        let mut changed_chunk = preface.to_vec();
        changed_chunk[chunk_offset] ^= 1;
        let damaged = wrap_scoring_preface(&changed_chunk, deflated).unwrap();
        assert!(matches!(
            decode_row_block(*first, &damaged),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
    }

    /// A warm query scores from the reader's preface cache. Each lookup
    /// decodes its row out of the cached block, skipping earlier digest and
    /// literal chunk ids and field lengths, and costs no SQLite work.
    #[test]
    fn warm_scoring_reads_every_preface_from_the_cache_without_sqlite_work() {
        let digest_id = format!("chunk.v1.sha256:{}", "cd".repeat(32));
        let chunk_ids = (0..70)
            .map(|ordinal| format!("chunk.{ordinal}"))
            .collect::<Vec<_>>();
        let lengths = (0..70)
            .map(|ordinal| {
                BTreeMap::from([
                    (LexicalFieldV1::SymbolName, ordinal + 1),
                    (LexicalFieldV1::BodyText, 300 * ordinal),
                ])
            })
            .collect::<Vec<_>>();
        // Every third document is absent, so blocks carry document gaps.
        let rows = (0..70)
            .map(|ordinal| BlockRowV1 {
                document_id: (ordinal * 3 / 2) as i64,
                chunk_id: if ordinal == 40 {
                    &digest_id
                } else {
                    &chunk_ids[ordinal]
                },
                parent_chunk_id: None,
                row: b"meta",
                text: "fn alpha() {}",
                field_lengths: &lengths[ordinal],
                trimmed_normalized_len: ordinal + 7,
            })
            .collect::<Vec<_>>();
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE row_blocks (first_document INTEGER PRIMARY KEY, payload BLOB NOT NULL);",
            )
            .expect("row block schema");
        let blocks = encode_row_blocks(&rows).expect("encode blocks");
        assert_eq!(blocks.len(), 3, "the fixture spans several blocks");
        for (first_document, payload) in &blocks {
            connection
                .execute(
                    "INSERT INTO row_blocks(first_document, payload) VALUES (?1, ?2)",
                    params![first_document, payload],
                )
                .expect("insert row block");
        }
        let cache = load_scoring_preface_index(&connection, usize::MAX, || Ok(()))
            .expect("load preface cache");

        let sqlite_steps = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&sqlite_steps);
        connection
            .progress_handler(
                1,
                Some(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    false
                }),
            )
            .expect("install progress handler");
        for _query in 0..2 {
            let row_blocks = RowBlocksV1::new(&connection, &cache);
            for row in &rows {
                let preface = row_blocks
                    .scoring_preface(u32::try_from(row.document_id).expect("document"))
                    .expect("cached preface");
                assert_eq!(preface.chunk_id, row.chunk_id);
                assert_eq!(&preface.field_lengths, row.field_lengths);
                assert_eq!(preface.trimmed_normalized_len, row.trimmed_normalized_len);
            }
            assert!(matches!(
                row_blocks.scoring_preface(2),
                Err(CodeLexicalArtifactErrorV1::Corrupt(_))
            ));
        }
        assert_eq!(
            sqlite_steps.load(Ordering::SeqCst),
            0,
            "warm scoring must not re-read row block prefaces"
        );
        let inflated = RowBlocksV1::new(&connection, &cache)
            .row(1)
            .expect("inflate a winner's block");
        assert_eq!(inflated.chunk_id, "chunk.1");
        assert_ne!(
            sqlite_steps.load(Ordering::SeqCst),
            0,
            "the observer sees the SQLite read a winner's inflation makes"
        );
        connection
            .progress_handler(1, None::<fn() -> bool>)
            .expect("remove progress handler");

        assert!(matches!(
            load_scoring_preface_index(&connection, 256, || Ok(())),
            Err(CodeLexicalArtifactErrorV1::Unreserved(_))
        ));
    }

    #[test]
    fn row_blocks_round_trip_share_parent_prefixes_and_bound_their_rows() {
        let body_id = format!("chunk.v1.sha256:{}", "ab".repeat(32));
        let body = "pub fn render(widget: &Widget) -> Frame {\n    widget.frame()\n}".repeat(20);
        let signature = "pub fn render(widget: &Widget) -> Frame {";
        let texts = (0..40)
            .map(|ordinal| format!("let value_{ordinal} = compute(value);\n").repeat(8))
            .collect::<Vec<_>>();
        let chunk_ids = (0..40)
            .map(|ordinal| format!("chunk.{ordinal}"))
            .collect::<Vec<_>>();
        let mut rows = vec![
            block_row(10, "chunk.signature", Some(&body_id), signature),
            block_row(11, &body_id, None, &body),
            block_row(13, "chunk.unrelated", Some("chunk.absent"), "fn other() {}"),
        ];
        rows.extend(
            texts
                .iter()
                .zip(&chunk_ids)
                .enumerate()
                .map(|(ordinal, (text, chunk_id))| {
                    block_row(14 + ordinal as i64, chunk_id, None, text)
                }),
        );
        let blocks = encode_row_blocks(&rows).expect("encode blocks");
        assert_eq!(blocks[0].0, 10, "a block is keyed by its first document");
        let decoded = blocks
            .iter()
            .flat_map(|(first, stored)| {
                let rows = decode_row_block(*first, stored).expect("decode block");
                assert!(rows.len() <= ROW_BLOCK_MAX_ROWS);
                rows
            })
            .collect::<Vec<_>>();
        let expected = rows
            .iter()
            .map(|row| StoredRowV1 {
                document_id: u32::try_from(row.document_id).expect("document"),
                chunk_id: row.chunk_id.to_owned(),
                row: row.row.to_vec(),
                text: row.text.to_owned(),
            })
            .collect::<Vec<_>>();
        assert_eq!(decoded, expected);
        let raw_text_bytes = rows.iter().map(|row| row.text.len()).sum::<usize>();
        let stored_bytes = blocks.iter().map(|(_, stored)| stored.len()).sum::<usize>();
        assert!(
            stored_bytes * 8 < raw_text_bytes,
            "neighbouring rows must deflate together: {stored_bytes} of {raw_text_bytes} bytes"
        );

        let (first, stored) = &blocks[0];
        let mut damaged = stored.clone();
        let tail = damaged.len() - 4;
        damaged[tail] ^= 0xff;
        assert!(
            decode_row_block(*first, &damaged).is_err(),
            "damaged stream"
        );
        assert!(
            decode_row_block(first + 1, stored).is_err(),
            "wrong block key"
        );
        let mut trailing = stored.clone();
        trailing.push(0);
        assert!(
            decode_row_block(*first, &trailing).is_err(),
            "trailing byte"
        );
        assert!(
            encode_row_blocks(&[block_row(5, "a", None, "x"), block_row(5, "b", None, "y")])
                .is_err(),
            "documents must ascend"
        );
    }

    #[test]
    fn body_and_signature_rows_share_one_symbol_entry() {
        let signature_row = symbol_row();
        let body_row = legacy_symbol_row();
        let mut dictionary = RowDictionaryTableV1::new();
        let mut decoded = Vec::new();
        for row in [&body_row, &signature_row] {
            let encoded = encode_artifact_row(row, &mut dictionary).expect("encode");
            decoded.push(
                decode_artifact_row(
                    &row.anchor.generation_id,
                    row.id.as_str(),
                    &encoded,
                    row.sanitized_text.as_str(),
                    &dictionary,
                )
                .expect("decode"),
            );
        }
        assert_eq!(decoded, [body_row, signature_row]);
        let kinds = dictionary
            .values()
            .map(|bytes| bytes[0])
            .collect::<Vec<_>>();
        assert_eq!(
            kinds.iter().filter(|kind| **kind == 1).count(),
            1,
            "one file entry"
        );
        assert_eq!(
            kinds.iter().filter(|kind| **kind == 2).count(),
            1,
            "one symbol entry"
        );
        let display = dictionary
            .values()
            .find(|bytes| bytes[0] == 3)
            .expect("display entry");
        assert_eq!(
            RowDictionaryEntryV1::decode(display).expect("display"),
            RowDictionaryEntryV1::SymbolDisplay {
                signature: Some("pub fn cancellation_probe_0001_003(input: u32) -> u32".to_owned()),
                documentation: Some("Cancels one probe after its bounded input.".to_owned()),
            }
        );
        assert_eq!(dictionary.len(), 3);
    }

    #[test]
    fn symbol_entries_store_file_relative_names_and_digest_identities() {
        let row = symbol_row();
        let (_, dictionary, decoded) = round_trip(&row);
        assert_eq!(decoded, row);
        let symbol = dictionary
            .values()
            .find(|bytes| bytes.first() == Some(&super::ENTRY_SYMBOL))
            .expect("symbol entry");
        assert!(
            !symbol
                .windows(row.logical_path.len())
                .any(|window| window == row.logical_path.as_bytes()),
            "the qualified name must not repeat the file's logical path"
        );
        assert!(
            !symbol
                .windows(b"symbol.v1.sha256:".len())
                .any(|window| window == b"symbol.v1.sha256:"),
            "a canonical symbol id is stored as digest bytes, not hex"
        );

        let mut elsewhere = symbol_row();
        elsewhere.symbol_qualified_name = Some("other/file.rs::probe".to_owned());
        elsewhere.anchor.symbol_occurrence_id =
            Some(SymbolOccurrenceId::new("symbol.literal").expect("literal symbol"));
        let (_, _, decoded) = round_trip(&elsewhere);
        assert_eq!(
            decoded, elsewhere,
            "names outside the row's file and non-canonical ids survive verbatim"
        );
        assert_eq!(
            QualifiedNameV1::for_path("src/a.rs::f", "src/a.rs"),
            QualifiedNameV1::InFile("f".to_owned())
        );
        assert_eq!(
            QualifiedNameV1::for_path("src/a.rsx::f", "src/a.rs"),
            QualifiedNameV1::Literal("src/a.rsx::f".to_owned())
        );
    }

    #[test]
    fn binary_v14_keeps_non_canonical_parents_and_foreign_term_symbols_verbatim() {
        let mut row = legacy_symbol_row();
        row.anchor.parent_chunk_id =
            Some(CodeSearchChunkId::new("chunk.v1.sha256:NOTHEX").expect("literal parent"));
        let foreign = SymbolOccurrenceId::new("symbol.elsewhere").expect("foreign symbol");
        row.exact_terms[0] = ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
            b"cancellation_probe_0001_003".to_vec(),
            row.exact_terms[0].span(),
            foreign.clone(),
        )
        .expect("foreign whole symbol");
        let (_, dictionary, decoded) = round_trip(&row);
        assert_eq!(decoded, row);
        assert_eq!(
            dictionary.len(),
            3,
            "a foreign exact-term symbol adds its own identity-only entry"
        );
        let uppercase_hex = format!("chunk.v1.sha256:{}", "C0".repeat(32));
        row.anchor.parent_chunk_id = Some(CodeSearchChunkId::new(uppercase_hex).expect("upper"));
        let (_, _, decoded) = round_trip(&row);
        assert_eq!(
            decoded, row,
            "uppercase hex is not canonical and must survive verbatim"
        );
    }

    #[test]
    fn binary_v14_decoder_fails_closed_on_truncation_trailing_bytes_and_missing_entries() {
        let row = legacy_symbol_row();
        let (encoded, dictionary, _) = round_trip(&row);
        let decode = |bytes: &[u8], dictionary: &RowDictionaryTableV1| {
            decode_artifact_row(
                &row.anchor.generation_id,
                row.id.as_str(),
                bytes,
                row.sanitized_text.as_str(),
                dictionary,
            )
        };
        for cut in [0usize, 1, 8, 20, 40, encoded.len() - 1] {
            assert!(
                decode(&encoded[..cut], &dictionary).is_err(),
                "truncated at {cut}"
            );
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode(&trailing, &dictionary).is_err(), "trailing byte");
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
