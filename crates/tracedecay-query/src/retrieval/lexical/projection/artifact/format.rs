use std::collections::BTreeMap;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use roaring::RoaringBitmap;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkId,
    ComponentRevision, ExactFieldV1, ExactTechnicalTermV1, FileOccurrenceId,
    LanguageDescriptorRevision, ManifestDigest, ScoreDomainId, SourceSpan, SymbolOccurrenceId,
};

use super::super::{CodeLexicalProjectionMetadataV1, LexicalFieldV1, ProjectedChunkV1};
use super::CodeLexicalArtifactErrorV1;
use super::schema::{CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, digest_domain_for_revision};

pub(super) use super::schema::{SERVING_INDEX_STEP_COUNT_V11, STATISTICS_STEP_COUNT_V11};

/// Revision 2 adds durable finalization/integrity state. Revision 1 artifacts
/// are branch-only staging files and must fail as incompatible rather than be
/// partially interpreted against this schema.
// Revision 3 replaces the branch-local computed finalization cursor with
// native table keys. Revision 4 adds document-leading indexes. Revision 5
// adds term-selective read indexes. Revision 6 makes the append authority
// immutable before one authenticated digest pass, defers every serving index
// until resumable finalization, and keeps ngram catch-up document-leading.
// Revision 7 replaces one row per document n-gram with deterministic
// source-page Roaring bitmap shards. Revision 8 adds source-page receipts for
// every append-only base section so sealing and reopening need not rescan the
// relational base after the private builder connection has admitted it.
// Revision 9 persists parser-attested symbol display identity with each row so
// graph-independent result hydration never needs the full sealed generation.
// Revision 10 adds a finalized n-gram selectivity projection so phrase reads
// can choose and page-prune by the rarest predicate without rescanning every
// source-page shard. Revision 11 interns terms to integer IDs, stores
// integer field codes, drops serving indexes that EXPLAIN QUERY PLAN never
// uses, and writes compact row payloads that omit identities already stored
// as columns or generation metadata. Revision 12 replaces the page-local
// n-gram shard header/fixed-width values with canonical delta varints and
// stores exact posting keys as collision-checked content-addressed term IDs.
// Revision 15 adds independently digested clone payload, occurrence, and
// exact-posting sections without changing lexical document integrity.
pub(super) const RECEIPT_RESERVATION_BYTES: usize = 16 * 1024;
pub(super) const SECTION_NAMES: [&str; 14] = [
    "source_pages",
    "document_integrity",
    "import_integrity",
    "import_evidence",
    "rows",
    "term_postings",
    "exact_postings",
    "ngram_postings",
    "field_stats",
    "vocabulary",
    "clone_occurrences",
    "clone_exact_postings",
    "clone_body_payloads",
    "clone_fingerprint_postings",
];
pub(super) const BASE_SECTION_NAMES: [&str; 7] = [
    "document_integrity",
    "import_integrity",
    "import_evidence",
    "rows",
    "term_postings",
    "exact_postings",
    "ngram_postings",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CodeLexicalArtifactPageBaseSectionsReceiptV1 {
    page_ordinal: u64,
    sections: Vec<CodeLexicalArtifactSectionDigestV1>,
}

pub(super) struct PageBaseSectionReceiptBuilderV1 {
    page_ordinal: u64,
    name: &'static str,
    row_count: u64,
    hasher: Sha256,
}

impl PageBaseSectionReceiptBuilderV1 {
    pub(super) fn new(
        page_ordinal: u64,
        name: &'static str,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.code-lexical-artifact-page-section.v1\0");
        hasher.update(page_ordinal.to_le_bytes());
        hasher.update(
            u64::try_from(name.len())
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
                .to_le_bytes(),
        );
        hasher.update(name.as_bytes());
        Ok(Self {
            page_ordinal,
            name,
            row_count: 0,
            hasher,
        })
    }

    pub(super) fn begin_row(&mut self) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.hasher.update(b"row\0");
        self.hasher.update(self.row_count.to_le_bytes());
        self.row_count = self.row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page-section row count overflowed".to_owned(),
            )
        })?;
        Ok(())
    }

    pub(super) fn integer(&mut self, value: i64) {
        self.hasher.update([1]);
        self.hasher.update(value.to_le_bytes());
    }

    pub(super) fn text(&mut self, value: &str) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.hasher.update([3]);
        hash_bytes(&mut self.hasher, value.as_bytes())
    }

    pub(super) fn blob(&mut self, value: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.hasher.update([4]);
        hash_bytes(&mut self.hasher, value)
    }

    pub(super) fn finish(
        mut self,
    ) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
        self.hasher.update(b"end\0");
        self.hasher.update(self.page_ordinal.to_le_bytes());
        self.hasher.update(self.row_count.to_le_bytes());
        let digest = ManifestDigest::from_sha256_bytes(&self.hasher.finalize())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        Ok(CodeLexicalArtifactSectionDigestV1 {
            name: self.name.to_owned(),
            row_count: self.row_count,
            digest,
        })
    }
}

pub(super) fn contract_number(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(error.to_string())
}

pub(super) fn hash_bytes(
    hasher: &mut Sha256,
    bytes: &[u8],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

pub(super) fn encode_page_base_sections_receipt(
    page_ordinal: u64,
    sections: Vec<CodeLexicalArtifactSectionDigestV1>,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    validate_page_base_sections(page_ordinal, &sections)?;
    serde_json::to_vec(&CodeLexicalArtifactPageBaseSectionsReceiptV1 {
        page_ordinal,
        sections,
    })
    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn decode_page_base_sections_receipt(
    expected_page_ordinal: u64,
    bytes: &[u8],
) -> Result<CodeLexicalArtifactPageBaseSectionsReceiptV1, CodeLexicalArtifactErrorV1> {
    let receipt: CodeLexicalArtifactPageBaseSectionsReceiptV1 = serde_json::from_slice(bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    validate_page_base_sections(expected_page_ordinal, &receipt.sections)?;
    if receipt.page_ordinal != expected_page_ordinal
        || serde_json::to_vec(&receipt)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
            != bytes
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact page base-section receipt is not canonical".to_owned(),
        ));
    }
    Ok(receipt)
}

impl CodeLexicalArtifactPageBaseSectionsReceiptV1 {
    pub(super) fn sections(&self) -> &[CodeLexicalArtifactSectionDigestV1] {
        &self.sections
    }
}

pub(super) fn initial_base_section_receipt_fold()
-> Result<(Vec<u64>, Vec<Vec<u8>>), CodeLexicalArtifactErrorV1> {
    let row_counts = vec![0; BASE_SECTION_NAMES.len()];
    let accumulators = BASE_SECTION_NAMES
        .into_iter()
        .map(|name| {
            let mut hasher = Sha256::new();
            hasher.update(b"tracedecay.code-lexical-artifact-base-receipt-fold.v1\0initial");
            hash_bytes(&mut hasher, name.as_bytes())?;
            Ok(hasher.finalize().to_vec())
        })
        .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()?;
    Ok((row_counts, accumulators))
}

pub(super) fn absorb_page_base_sections_receipt(
    page_ordinal: u64,
    bytes: &[u8],
    row_counts: &mut [u64],
    accumulators: &mut [Vec<u8>],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if row_counts.len() != BASE_SECTION_NAMES.len()
        || accumulators.len() != BASE_SECTION_NAMES.len()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact base-section receipt fold has the wrong width".to_owned(),
        ));
    }
    let receipt = decode_page_base_sections_receipt(page_ordinal, bytes)?;
    for (ordinal, section) in receipt.sections().iter().enumerate() {
        let previous: [u8; 32] = accumulators[ordinal].as_slice().try_into().map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact base-section receipt accumulator has the wrong length".to_owned(),
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.code-lexical-artifact-base-receipt-fold.v1\0page");
        hash_bytes(&mut hasher, section.name.as_bytes())?;
        hasher.update(page_ordinal.to_le_bytes());
        hasher.update(section.row_count.to_le_bytes());
        hash_bytes(&mut hasher, section.digest.as_str().as_bytes())?;
        hasher.update(previous);
        accumulators[ordinal] = hasher.finalize().to_vec();
        row_counts[ordinal] = row_counts[ordinal]
            .checked_add(section.row_count)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact base-section receipt row count overflowed".to_owned(),
                )
            })?;
    }
    Ok(())
}

pub(super) fn finish_base_section_receipt_fold(
    row_counts: &[u64],
    accumulators: &[Vec<u8>],
) -> Result<Vec<CodeLexicalArtifactSectionDigestV1>, CodeLexicalArtifactErrorV1> {
    if row_counts.len() != BASE_SECTION_NAMES.len()
        || accumulators.len() != BASE_SECTION_NAMES.len()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact base-section receipt fold has the wrong width".to_owned(),
        ));
    }
    BASE_SECTION_NAMES
        .into_iter()
        .enumerate()
        .map(|(ordinal, name)| {
            let accumulator: [u8; 32] =
                accumulators[ordinal].as_slice().try_into().map_err(|_| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact base-section receipt accumulator has the wrong length"
                            .to_owned(),
                    )
                })?;
            let mut hasher = Sha256::new();
            hasher.update(b"tracedecay.code-lexical-artifact-base-receipt-fold.v1\0final");
            hash_bytes(&mut hasher, name.as_bytes())?;
            hasher.update(row_counts[ordinal].to_le_bytes());
            hasher.update(accumulator);
            let digest = ManifestDigest::from_sha256_bytes(&hasher.finalize())
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
            Ok(CodeLexicalArtifactSectionDigestV1 {
                name: name.to_owned(),
                row_count: row_counts[ordinal],
                digest,
            })
        })
        .collect()
}

fn validate_page_base_sections(
    page_ordinal: u64,
    sections: &[CodeLexicalArtifactSectionDigestV1],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if sections.len() != BASE_SECTION_NAMES.len()
        || sections
            .iter()
            .zip(BASE_SECTION_NAMES)
            .any(|(section, expected)| section.name != expected)
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact page {page_ordinal} base-section receipt is malformed"
        )));
    }
    Ok(())
}

/// `(column, type, NOT NULL, primary-key ordinal)` as `pragma_table_xinfo`
/// reports it.
type ColumnShapeV1 = (&'static str, &'static str, i64, i64);

/// `(table, WITHOUT ROWID, columns)` for every table a staging or sealed
/// artifact always carries. Staging-only append tables are dropped by
/// finalization and carry no serving contract.
const ARTIFACT_TABLE_LAYOUT: [(&str, bool, &[ColumnShapeV1]); 13] = [
    (
        "source_pages",
        false,
        &[
            ("page_ordinal", "INTEGER", 0, 1),
            ("chunk_count", "INTEGER", 1, 0),
            ("import_count", "INTEGER", 1, 0),
            ("import_payload_bytes", "INTEGER", 1, 0),
            ("import_dictionary_digest", "TEXT", 1, 0),
            ("ngram_digest", "TEXT", 1, 0),
            ("base_sections_receipt", "BLOB", 1, 0),
        ],
    ),
    ("import_evidence", true, &[("canonical", "BLOB", 1, 1)]),
    (
        "row_blocks",
        false,
        &[
            ("first_document", "INTEGER", 0, 1),
            ("payload", "BLOB", 1, 0),
        ],
    ),
    (
        "row_chunks",
        true,
        &[("chunk_id", "BLOB", 1, 1), ("document_id", "INTEGER", 1, 0)],
    ),
    (
        "term_postings",
        true,
        &[
            ("term", "TEXT", 1, 1),
            ("in_fuzzy", "INTEGER", 1, 0),
            ("lists", "BLOB", 1, 0),
        ],
    ),
    (
        "exact_postings",
        true,
        &[
            ("term_id", "INTEGER", 1, 1),
            ("field", "INTEGER", 1, 2),
            ("documents", "BLOB", 1, 0),
        ],
    ),
    (
        "ngram_postings",
        true,
        &[
            ("kind", "INTEGER", 1, 1),
            ("ngram", "INTEGER", 1, 2),
            ("document_frequency", "INTEGER", 1, 0),
            ("documents", "BLOB", 1, 0),
        ],
    ),
    (
        "exact_vocabulary",
        false,
        &[("term_id", "INTEGER", 0, 1), ("term", "BLOB", 1, 0)],
    ),
    (
        "row_dictionary",
        false,
        &[("entry_id", "INTEGER", 0, 1), ("entry", "BLOB", 1, 0)],
    ),
    (
        "clone_body_payloads",
        false,
        &[
            ("ordinal", "INTEGER", 0, 1),
            ("payload_digest", "BLOB", 1, 0),
            ("payload", "BLOB", 1, 0),
        ],
    ),
    (
        "clone_occurrences",
        false,
        &[
            ("ordinal", "INTEGER", 0, 1),
            ("symbol_key", "BLOB", 1, 0),
            ("payload_ordinal", "INTEGER", 1, 0),
            ("path", "TEXT", 1, 0),
            ("body_start", "INTEGER", 1, 0),
            ("body_end", "INTEGER", 1, 0),
            ("eligibility", "BLOB", 1, 0),
        ],
    ),
    (
        "clone_exact_postings",
        true,
        &[
            ("class", "INTEGER", 1, 1),
            ("normalization_revision", "INTEGER", 1, 2),
            ("digest", "BLOB", 1, 3),
            ("occurrence_ordinal", "INTEGER", 1, 4),
        ],
    ),
    (
        "clone_fingerprint_postings",
        true,
        &[
            ("language", "TEXT", 1, 1),
            ("class", "INTEGER", 1, 2),
            ("normalization_revision", "INTEGER", 1, 3),
            ("fingerprint", "INTEGER", 1, 4),
            ("posting_count", "INTEGER", 1, 0),
            ("postings", "BLOB", 1, 0),
        ],
    ),
];

pub(super) fn verify_artifact_table_layout(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for (table, expected_without_rowid, expected_columns) in ARTIFACT_TABLE_LAYOUT {
        let without_rowid: Option<i64> = connection
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1 AND type = 'table'",
                [table],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                CodeLexicalArtifactErrorV1::Incompatible(format!(
                    "artifact {table} schema is unreadable: {error}"
                ))
            })?;
        let columns = table_columns(connection, table)?;
        if without_rowid != Some(i64::from(expected_without_rowid))
            || !table_column_shapes(&columns).eq(expected_columns.iter().copied())
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact {table} table has columns {columns:?} and without-rowid state {without_rowid:?}; revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} requires {expected_columns:?}"
            )));
        }
    }
    Ok(())
}

fn table_column_shapes(
    rows: &[(String, String, i64, i64)],
) -> impl Iterator<Item = (&str, &str, i64, i64)> {
    rows.iter().map(|(name, ty, not_null, primary_key)| {
        (name.as_str(), ty.as_str(), *not_null, *primary_key)
    })
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<Vec<(String, String, i64, i64)>, CodeLexicalArtifactErrorV1> {
    connection
        .prepare(&format!(
            "SELECT name, type, [notnull], pk FROM pragma_table_xinfo('{table}') WHERE hidden = 0 ORDER BY cid"
        ))
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact {table} columns are unreadable: {error}"
            ))
        })
}

pub(super) fn ngram_page_digest<'a>(
    page_ordinal: u64,
    rows: impl IntoIterator<Item = (i64, i64, &'a [u8], u64)>,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-ngram-page.v1\0");
    hasher.update(page_ordinal.to_le_bytes());
    let mut row_count = 0u64;
    for (kind, ngram, documents, cardinality) in rows {
        hasher.update(b"row\0");
        hasher.update(kind.to_le_bytes());
        hasher.update(ngram.to_le_bytes());
        hasher.update(
            u64::try_from(documents.len())
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
                .to_le_bytes(),
        );
        hasher.update(documents);
        hasher.update(cardinality.to_le_bytes());
        row_count = row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact ngram shard count overflowed".to_owned(),
            )
        })?;
    }
    hasher.update(b"end\0");
    hasher.update(row_count.to_le_bytes());
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

#[cfg(test)]
pub(super) fn encode_ngram_bitmap(
    bitmap: &RoaringBitmap,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut encoder = PostingListEncoderV1::new(false);
    for document in bitmap {
        encoder.push(document, 1)?;
    }
    encoder.finish()
}

pub(super) fn decode_ngram_bitmap(
    encoded: &[u8],
) -> Result<RoaringBitmap, CodeLexicalArtifactErrorV1> {
    if encoded.is_empty() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact document list is empty".to_owned(),
        ));
    }
    let mut bitmap = RoaringBitmap::new();
    for posting in PostingListDecoderV1::new(encoded, false) {
        let (document, _) = posting?;
        bitmap.try_push(document).map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact document list is not strictly ascending".to_owned(),
            )
        })?;
    }
    Ok(bitmap)
}

const DOCUMENT_SET_DELTAS: u8 = 0;
const DOCUMENT_SET_BITSET: u8 = 1;

/// [`document_set_from_deltas`] of a bitmap.
#[cfg(test)]
pub(super) fn encode_document_set(
    documents: &RoaringBitmap,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let Some(last) = documents.max() else {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact document set is empty".to_owned(),
        ));
    };
    document_set_from_deltas(encode_ngram_bitmap(documents)?, last)
}

/// A sealed n-gram document set in the smaller of two tagged encodings: the
/// delta-varint list, or its first document followed by a bitset over the
/// range it spans (bit `i` of byte `i / 8`, least significant first, is
/// document `first + i`). The bitset wins once a list holds more than about
/// one document in eight of that range, which the most common n-grams do.
/// `deltas` is a non-empty document list whose last document is `last`.
fn document_set_from_deltas(
    deltas: Vec<u8>,
    last: u32,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let first = u32::try_from(take_varint(&mut deltas.as_slice())?).map_err(contract_number)?;
    let mut prefix = Vec::with_capacity(6);
    prefix.push(DOCUMENT_SET_BITSET);
    encode_varint(u64::from(first), &mut prefix);
    let bitset_bytes = usize::try_from(u64::from(last - first) / 8 + 1)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    if prefix.len() + bitset_bytes > deltas.len() {
        let mut encoded = Vec::with_capacity(1 + deltas.len());
        encoded.push(DOCUMENT_SET_DELTAS);
        encoded.extend_from_slice(&deltas);
        return Ok(encoded);
    }
    let offset = prefix.len();
    prefix.resize(offset + bitset_bytes, 0);
    for posting in PostingListDecoderV1::new(&deltas, false) {
        let bit = posting?.0 - first;
        prefix[offset + (bit / 8) as usize] |= 1 << (bit % 8);
    }
    Ok(prefix)
}

pub(super) fn decode_document_set(
    encoded: &[u8],
) -> Result<RoaringBitmap, CodeLexicalArtifactErrorV1> {
    let corrupt = |detail: &str| {
        CodeLexicalArtifactErrorV1::Corrupt(format!("lexical artifact document set {detail}"))
    };
    match encoded.split_first() {
        Some((&DOCUMENT_SET_DELTAS, deltas)) => decode_ngram_bitmap(deltas),
        Some((&DOCUMENT_SET_BITSET, mut bitset)) => {
            let first = u32::try_from(take_varint(&mut bitset)?)
                .map_err(|_| corrupt("start overflows u32"))?;
            // Canonical: the range starts and ends on a member.
            if bitset.first().is_none_or(|byte| byte & 1 == 0)
                || bitset.last().is_some_and(|byte| *byte == 0)
            {
                return Err(corrupt("bitset is not canonical"));
            }
            let mut documents = RoaringBitmap::new();
            for (index, byte) in bitset.iter().enumerate() {
                for bit in 0..8u32 {
                    if byte & (1 << bit) == 0 {
                        continue;
                    }
                    let document = u32::try_from(index)
                        .ok()
                        .and_then(|index| index.checked_mul(8))
                        .and_then(|offset| offset.checked_add(bit))
                        .and_then(|offset| first.checked_add(offset))
                        .ok_or_else(|| corrupt("bitset overflows u32"))?;
                    documents
                        .try_push(document)
                        .map_err(|_| corrupt("bitset is not ascending"))?;
                }
            }
            Ok(documents)
        }
        _ => Err(corrupt("has an unknown encoding tag")),
    }
}

/// Stored bytes as one tag byte, the varint inflated length, and the raw
/// deflate stream of `bytes`.
pub(super) fn deflate_bytes(tag: u8, bytes: &[u8]) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut compressor = Compress::new(Compression::best(), false);
    let mut compressed = Vec::with_capacity(bytes.len() / 2 + 64);
    loop {
        if compressed.capacity() - compressed.len() < 1024 {
            compressed.reserve(bytes.len() / 4 + 1024);
        }
        let consumed = usize::try_from(compressor.total_in()).map_err(contract_number)?;
        let status = compressor
            .compress_vec(&bytes[consumed..], &mut compressed, FlushCompress::Finish)
            .map_err(contract_number)?;
        if status == Status::StreamEnd {
            break;
        }
    }
    let mut stored = Vec::with_capacity(compressed.len() + 11);
    stored.push(tag);
    encode_varint(
        u64::try_from(bytes.len()).map_err(contract_number)?,
        &mut stored,
    );
    stored.extend_from_slice(&compressed);
    Ok(stored)
}

/// Inverse of [`deflate_bytes`]: refuse another tag, an inflated length above
/// `maximum`, or a stream that does not inflate to exactly its length.
pub(super) fn inflate_bytes(
    tag: u8,
    stored: &[u8],
    maximum: usize,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let corrupt = |detail: &str| {
        CodeLexicalArtifactErrorV1::Corrupt(format!("lexical artifact deflated value {detail}"))
    };
    let Some((&stored_tag, mut rest)) = stored.split_first() else {
        return Err(corrupt("is empty"));
    };
    if stored_tag != tag {
        return Err(corrupt("has an unknown encoding tag"));
    }
    let length = usize::try_from(take_varint(&mut rest)?).map_err(|_| corrupt("is too long"))?;
    if length > maximum {
        return Err(corrupt("exceeds its inflated bound"));
    }
    let mut decompressor = Decompress::new(false);
    let mut inflated = Vec::with_capacity(length);
    let status = decompressor
        .decompress_vec(rest, &mut inflated, FlushDecompress::Finish)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if status != Status::StreamEnd
        || inflated.len() != length
        || usize::try_from(decompressor.total_in()).ok() != Some(rest.len())
    {
        return Err(corrupt("does not inflate to its length"));
    }
    Ok(inflated)
}

/// One fingerprint's sealed posting list: its `(occurrence ordinal, token
/// position)` postings sorted by ordinal then position, one group per
/// ordinal (the varint ordinal, absolute for the first group and a non-zero
/// delta after, the varint posting count, then the first position and each
/// later non-zero delta).
pub(super) fn encode_fingerprint_postings(
    postings: &[(u32, u32)],
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let unordered = || {
        CodeLexicalArtifactErrorV1::Contract(
            "clone fingerprint postings are not strictly ordered".to_owned(),
        )
    };
    let mut encoded = Vec::with_capacity(postings.len() * 4);
    let mut previous_ordinal: Option<u32> = None;
    let mut start = 0;
    while start < postings.len() {
        let ordinal = postings[start].0;
        let end = start
            + postings[start..]
                .iter()
                .take_while(|(candidate, _)| *candidate == ordinal)
                .count();
        let delta = match previous_ordinal {
            None => ordinal,
            Some(previous) if ordinal > previous => ordinal - previous,
            Some(_) => return Err(unordered()),
        };
        previous_ordinal = Some(ordinal);
        encode_varint(u64::from(delta), &mut encoded);
        encode_varint(
            u64::try_from(end - start).map_err(contract_number)?,
            &mut encoded,
        );
        let mut previous_position = None;
        for (_, position) in &postings[start..end] {
            let delta = match previous_position {
                None => *position,
                Some(previous) if *position > previous => position - previous,
                Some(_) => return Err(unordered()),
            };
            encode_varint(u64::from(delta), &mut encoded);
            previous_position = Some(*position);
        }
        start = end;
    }
    Ok(encoded)
}

/// Inverse of [`encode_fingerprint_postings`], failing closed on any
/// non-canonical order, empty group, overflow, or truncation.
pub(super) fn decode_fingerprint_postings(
    mut encoded: &[u8],
) -> Result<Vec<(u32, u32)>, CodeLexicalArtifactErrorV1> {
    let corrupt = || {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact clone fingerprint postings are malformed".to_owned(),
        )
    };
    let mut postings = Vec::new();
    let mut ordinal: Option<u32> = None;
    while !encoded.is_empty() {
        let delta = u32::try_from(take_varint(&mut encoded)?).map_err(|_| corrupt())?;
        let next = match ordinal {
            None => delta,
            Some(_) if delta == 0 => return Err(corrupt()),
            Some(previous) => previous.checked_add(delta).ok_or_else(corrupt)?,
        };
        ordinal = Some(next);
        let count = take_varint(&mut encoded)?;
        if count == 0 || count > encoded.len() as u64 {
            return Err(corrupt());
        }
        let mut position: Option<u32> = None;
        for _ in 0..count {
            let delta = u32::try_from(take_varint(&mut encoded)?).map_err(|_| corrupt())?;
            let value = match position {
                None => delta,
                Some(_) if delta == 0 => return Err(corrupt()),
                Some(previous) => previous.checked_add(delta).ok_or_else(corrupt)?,
            };
            position = Some(value);
            postings.push((next, value));
        }
    }
    Ok(postings)
}

/// One term's sealed `term_postings.lists`: for each field in strictly
/// ascending code order, the varint field code, the varint document
/// frequency, and the length-prefixed frequency posting list.
pub(super) fn encode_term_lists(
    lists: &[(i64, u64, Vec<u8>)],
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut encoded = Vec::with_capacity(lists.iter().map(|(_, _, list)| list.len() + 6).sum());
    let mut previous = None;
    for (field, document_frequency, list) in lists {
        if previous.is_some_and(|previous| previous >= *field)
            || *document_frequency == 0
            || list.is_empty()
        {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact term lists are not canonical".to_owned(),
            ));
        }
        previous = Some(*field);
        encode_varint(
            u64::try_from(*field).map_err(contract_number)?,
            &mut encoded,
        );
        encode_varint(*document_frequency, &mut encoded);
        encode_varint(
            u64::try_from(list.len()).map_err(contract_number)?,
            &mut encoded,
        );
        encoded.extend_from_slice(list);
    }
    Ok(encoded)
}

/// `(field code, document frequency, posting list)` of one field's list.
pub(super) type TermFieldListV1<'a> = (i64, u64, &'a [u8]);

/// Every field list one `term_postings.lists` value carries.
pub(super) fn decode_term_lists(
    mut encoded: &[u8],
) -> Result<Vec<TermFieldListV1<'_>>, CodeLexicalArtifactErrorV1> {
    let corrupt = || {
        CodeLexicalArtifactErrorV1::Corrupt("lexical artifact term lists are malformed".to_owned())
    };
    let mut lists = Vec::new();
    let mut previous = None;
    while !encoded.is_empty() {
        let field = i64::try_from(take_varint(&mut encoded)?).map_err(|_| corrupt())?;
        let document_frequency = take_varint(&mut encoded)?;
        let length = usize::try_from(take_varint(&mut encoded)?).map_err(|_| corrupt())?;
        if previous.is_some_and(|previous| previous >= field)
            || document_frequency == 0
            || length == 0
            || length > encoded.len()
        {
            return Err(corrupt());
        }
        previous = Some(field);
        let (list, rest) = encoded.split_at(length);
        encoded = rest;
        lists.push((field, document_frequency, list));
    }
    if lists.is_empty() {
        return Err(corrupt());
    }
    Ok(lists)
}

/// One sorted posting list: canonical LEB128 varints, the first document
/// absolute and every later one as its non-zero delta. With frequencies each
/// document varint is shifted left one bit and a set low bit announces a
/// following frequency varint (always at least 2), so the dominant frequency
/// of one costs no byte.
pub(super) struct PostingListEncoderV1 {
    bytes: Vec<u8>,
    previous: Option<u32>,
    len: u64,
    frequencies: bool,
}

impl PostingListEncoderV1 {
    pub(super) fn new(frequencies: bool) -> Self {
        Self {
            bytes: Vec::new(),
            previous: None,
            len: 0,
            frequencies,
        }
    }

    pub(super) fn push(
        &mut self,
        document: u32,
        frequency: u32,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        let delta = match self.previous {
            None => document,
            Some(previous) if document > previous => document - previous,
            Some(_) => {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact posting documents are not strictly ascending".to_owned(),
                ));
            }
        };
        if frequency == 0 || (!self.frequencies && frequency != 1) {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact posting frequency is out of range".to_owned(),
            ));
        }
        if self.frequencies {
            encode_varint(
                (u64::from(delta) << 1) | u64::from(frequency != 1),
                &mut self.bytes,
            );
            if frequency != 1 {
                encode_varint(u64::from(frequency), &mut self.bytes);
            }
        } else {
            encode_varint(u64::from(delta), &mut self.bytes);
        }
        self.previous = Some(document);
        self.len += 1;
        Ok(())
    }

    pub(super) fn len(&self) -> u64 {
        self.len
    }

    /// Heap bytes the encoded list holds, for memory accounting.
    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    pub(super) fn finish(self) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
        if self.len == 0 {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact posting list is empty".to_owned(),
            ));
        }
        Ok(self.bytes)
    }

    /// This document list in its sealed [`document_set_from_deltas`] form.
    pub(super) fn finish_document_set(self) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
        if self.frequencies {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact frequency list is not a document set".to_owned(),
            ));
        }
        let last = self.previous;
        let deltas = self.finish()?;
        let last = last.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact document set is empty".to_owned(),
            )
        })?;
        document_set_from_deltas(deltas, last)
    }
}

/// Streams `(document, frequency)` from a [`PostingListEncoderV1`] list,
/// failing closed on any non-canonical, zero-delta, or overflowing entry.
pub(super) struct PostingListDecoderV1<'a> {
    bytes: &'a [u8],
    previous: Option<u32>,
    frequencies: bool,
}

impl<'a> PostingListDecoderV1<'a> {
    pub(super) fn new(bytes: &'a [u8], frequencies: bool) -> Self {
        Self {
            bytes,
            previous: None,
            frequencies,
        }
    }

    fn decode_next(&mut self) -> Result<(u32, u32), CodeLexicalArtifactErrorV1> {
        let token = take_varint(&mut self.bytes)?;
        let (delta, frequency) = if self.frequencies {
            let frequency = if token & 1 == 1 {
                let frequency = take_varint(&mut self.bytes)?;
                if frequency < 2 {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact posting frequency is not canonical".to_owned(),
                    ));
                }
                frequency
            } else {
                1
            };
            (token >> 1, frequency)
        } else {
            (token, 1)
        };
        let corrupt = |_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact posting value overflows u32".to_owned(),
            )
        };
        let delta = u32::try_from(delta).map_err(corrupt)?;
        let frequency = u32::try_from(frequency).map_err(corrupt)?;
        let document = match self.previous {
            None => delta,
            Some(_) if delta == 0 => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact posting delta is zero".to_owned(),
                ));
            }
            Some(previous) => previous.checked_add(delta).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact posting delta overflowed".to_owned(),
                )
            })?,
        };
        self.previous = Some(document);
        Ok((document, frequency))
    }
}

impl Iterator for PostingListDecoderV1<'_> {
    type Item = Result<(u32, u32), CodeLexicalArtifactErrorV1>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }
        let decoded = self.decode_next();
        if decoded.is_err() {
            self.bytes = &[];
        }
        Some(decoded)
    }
}

pub(super) fn encode_varint(mut value: u64, encoded: &mut Vec<u8>) {
    while value >= 0x80 {
        encoded.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    encoded.push(value as u8);
}

pub(super) fn take_varint(encoded: &mut &[u8]) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let mut value = 0u64;
    for (ordinal, byte) in encoded.iter().copied().take(10).enumerate() {
        let payload = u64::from(byte & 0x7f);
        if ordinal == 9 && payload > 1 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact posting varint overflows u64".to_owned(),
            ));
        }
        value |= payload << (ordinal * 7);
        if byte & 0x80 == 0 {
            if ordinal > 0 && byte == 0 {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact posting varint is not canonical".to_owned(),
                ));
            }
            *encoded = &encoded[ordinal + 1..];
            return Ok(value);
        }
    }
    Err(CodeLexicalArtifactErrorV1::Corrupt(
        "lexical artifact posting varint is truncated".to_owned(),
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalArtifactSectionDigestV1 {
    pub name: String,
    pub row_count: u64,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCodeLexicalArtifactV1 {
    format_revision: u32,
    metadata_digest: ManifestDigest,
    source_format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: ManifestDigest,
    artifact_digest: ManifestDigest,
    section_digests: Vec<CodeLexicalArtifactSectionDigestV1>,
    file_size_bytes: u64,
}

impl VerifiedCodeLexicalArtifactV1 {
    pub fn artifact_digest(&self) -> &ManifestDigest {
        &self.artifact_digest
    }

    pub fn file_size_bytes(&self) -> u64 {
        self.file_size_bytes
    }

    pub fn page_count(&self) -> u64 {
        self.page_count
    }

    pub fn total_chunks(&self) -> u64 {
        self.total_chunks
    }

    pub fn total_payload_bytes(&self) -> u64 {
        self.total_payload_bytes
    }

    pub fn total_imports(&self) -> u64 {
        self.total_imports
    }

    pub fn import_payload_bytes(&self) -> u64 {
        self.import_payload_bytes
    }

    pub fn import_dictionary_digest(&self) -> &ManifestDigest {
        &self.import_dictionary_digest
    }

    pub fn section_digests(&self) -> &[CodeLexicalArtifactSectionDigestV1] {
        &self.section_digests
    }

    pub fn format_revision(&self) -> u32 {
        self.format_revision
    }

    pub(super) fn metadata_digest(&self) -> &ManifestDigest {
        &self.metadata_digest
    }

    pub(super) fn source_format_revision(&self) -> u32 {
        self.source_format_revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLexicalArtifactOccurrenceV1 {
    pub generation: CodeGenerationId,
    pub file: FileOccurrenceId,
    pub symbol: Option<SymbolOccurrenceId>,
    pub chunk: CodeSearchChunkId,
    pub source_span: SourceSpan,
    pub logical_path: String,
    pub sanitized_text: BoundedSanitizedText,
    pub simple_name: Option<String>,
    pub qualified_name: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLexicalImportMembershipWitnessV1 {
    pub artifact_digest: ManifestDigest,
    pub import_dictionary_digest: ManifestDigest,
    pub evidence: CodeIndexImportEvidenceV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactRowV1 {
    pub id: CodeSearchChunkId,
    pub anchor: CodeSearchChunkAnchorV1,
    pub language_descriptor_revision: LanguageDescriptorRevision,
    pub exact_terms: Vec<ExactTechnicalTermV1>,
    pub sanitized_text: BoundedSanitizedText,
    pub logical_path: String,
    pub symbol_simple_name: Option<String>,
    pub symbol_qualified_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub symbol_signature: Option<String>,
    pub symbol_documentation: Option<String>,
    pub field_lengths: BTreeMap<LexicalFieldV1, usize>,
    pub normalized_text: String,
}

impl From<ProjectedChunkV1> for ArtifactRowV1 {
    fn from(row: ProjectedChunkV1) -> Self {
        Self {
            id: row.id,
            anchor: row.anchor,
            language_descriptor_revision: row.language_descriptor_revision,
            exact_terms: row.exact_terms,
            sanitized_text: row.sanitized_text,
            logical_path: row.logical_path,
            symbol_simple_name: row.symbol_simple_name,
            symbol_qualified_name: row.symbol_qualified_name,
            symbol_kind: row.symbol_kind,
            symbol_signature: row.symbol_signature,
            symbol_documentation: row.symbol_documentation,
            field_lengths: row.field_lengths,
            normalized_text: row.normalized_text,
        }
    }
}

pub(super) fn manifest_digest<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    manifest_digest_of_bytes(domain, &bytes)
}

fn manifest_digest_of_bytes(
    domain: &[u8],
    bytes: &[u8],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
            .to_le_bytes(),
    );
    hasher.update(bytes);
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

/// The projection metadata an artifact's bytes depend on. Generation,
/// repository, and freshness name the route that opens the artifact, not its
/// content, so worktrees that index identical trees seal identical files and
/// each opener supplies its own route identity.
#[derive(Serialize)]
struct ArtifactContentMetadataV1<'a> {
    logical_paths: &'a BTreeMap<FileOccurrenceId, String>,
    exact_retriever_revision: &'a ComponentRevision,
    lexical_retriever_revision: &'a ComponentRevision,
    exact_score_domain: &'a ScoreDomainId,
}

impl<'a> ArtifactContentMetadataV1<'a> {
    fn of(metadata: &'a CodeLexicalProjectionMetadataV1) -> Self {
        Self {
            logical_paths: &metadata.logical_paths,
            exact_retriever_revision: &metadata.exact_retriever_revision,
            lexical_retriever_revision: &metadata.lexical_retriever_revision,
            exact_score_domain: &metadata.exact_score_domain,
        }
    }
}

/// The canonical bytes `artifact_state.metadata` stores for `metadata`.
pub(super) fn content_metadata_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    serde_json::to_vec(&ArtifactContentMetadataV1::of(metadata))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn metadata_digest(
    metadata: &CodeLexicalProjectionMetadataV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    stored_metadata_digest(&content_metadata_bytes(metadata)?)
}

/// The key a lexical artifact built from a source with `source_content_key`
/// under `metadata` is published with. Everything the artifact's bytes
/// depend on is in it (the format, the content metadata, and the source's
/// content), so an artifact published under a key serves any opener whose
/// source and projection produce the same key.
pub fn code_lexical_artifact_content_key(
    source_content_key: &ManifestDigest,
    metadata: &CodeLexicalProjectionMetadataV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(
        b"tracedecay.code-lexical-artifact-content-key.v1\0",
        &(
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
            metadata_digest(metadata)?.as_str(),
            source_content_key.as_str(),
        ),
    )
}

/// The digest of stored `artifact_state.metadata` bytes.
pub(super) fn stored_metadata_digest(
    bytes: &[u8],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest_of_bytes(b"tracedecay.code-lexical-artifact-metadata.v2\0", bytes)
}

#[allow(clippy::too_many_arguments)] // one committed digest tuple, spelled once
pub(super) fn artifact_digest(
    metadata_digest: &ManifestDigest,
    source_format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: &ManifestDigest,
    sections: &[CodeLexicalArtifactSectionDigestV1],
    format_revision: u32,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(
        digest_domain_for_revision(format_revision)?,
        &(
            metadata_digest.as_str(),
            source_format_revision,
            page_count,
            total_chunks,
            total_payload_bytes,
            total_imports,
            import_payload_bytes,
            import_dictionary_digest.as_str(),
            sections,
            format_revision,
        ),
    )
}

/// The artifact digest `receipt` binds, recomputed from its own fields over
/// `sections`.
pub(super) fn receipt_artifact_digest(
    receipt: &VerifiedCodeLexicalArtifactV1,
    sections: &[CodeLexicalArtifactSectionDigestV1],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    artifact_digest(
        &receipt.metadata_digest,
        receipt.source_format_revision,
        receipt.page_count,
        receipt.total_chunks,
        receipt.total_payload_bytes,
        receipt.total_imports,
        receipt.import_payload_bytes,
        &receipt.import_dictionary_digest,
        sections,
        receipt.format_revision,
    )
}

pub(super) fn encode_field(field: LexicalFieldV1) -> Result<String, CodeLexicalArtifactErrorV1> {
    serde_json::to_string(&field)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn encode_exact_field(
    field: ExactFieldV1,
) -> Result<String, CodeLexicalArtifactErrorV1> {
    serde_json::to_string(&field)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

pub(super) fn padded_receipt(
    receipt: &VerifiedCodeLexicalArtifactV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut bytes = serde_json::to_vec(receipt)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    if bytes.len() > RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact receipt exceeds its fixed reservation".to_owned(),
        ));
    }
    bytes.resize(RECEIPT_RESERVATION_BYTES, 0);
    Ok(bytes)
}

pub(super) fn decode_padded_receipt(
    bytes: &[u8],
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    if bytes.len() != RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt reservation has the wrong length".to_owned(),
        ));
    }
    let end = bytes.iter().position(|byte| *byte == 0).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is missing its reserved zero tail".to_owned(),
        )
    })?;
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt has nonzero bytes after its canonical payload".to_owned(),
        ));
    }
    if end == 0 {
        return Ok(None);
    }
    let receipt = serde_json::from_slice(&bytes[..end])
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if padded_receipt(&receipt)? != bytes {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is not canonically encoded".to_owned(),
        ));
    }
    Ok(Some(receipt))
}

/// Decode a fixed-size receipt while honoring the caller's canonical work
/// control. Reopen paths use this version so a corrupt or cold artifact never
/// turns an expired epoch into an unbounded padding scan.
pub(super) fn decode_padded_receipt_with_control(
    bytes: &[u8],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    super::checkpoint(control)?;
    if bytes.len() != RECEIPT_RESERVATION_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt reservation has the wrong length".to_owned(),
        ));
    }
    let mut end = None;
    for (ordinal, byte) in bytes.iter().enumerate() {
        if ordinal.is_multiple_of(1_024) {
            super::checkpoint(control)?;
        }
        if *byte == 0 {
            end = Some(ordinal);
            break;
        }
    }
    let end = end.ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is missing its reserved zero tail".to_owned(),
        )
    })?;
    for (ordinal, byte) in bytes[end..].iter().enumerate() {
        if ordinal.is_multiple_of(1_024) {
            super::checkpoint(control)?;
        }
        if *byte != 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact receipt has nonzero bytes after its canonical payload".to_owned(),
            ));
        }
    }
    if end == 0 {
        return Ok(None);
    }
    super::checkpoint(control)?;
    let receipt = serde_json::from_slice(&bytes[..end])
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    super::checkpoint(control)?;
    if padded_receipt(&receipt)? != bytes {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact receipt is not canonically encoded".to_owned(),
        ));
    }
    Ok(Some(receipt))
}

/// Seal `sections` for `source`. The receipt binds only content: the sealed
/// source's state and chunk-chain digests hash the building worktree's
/// generation into every chunk anchor and clone occurrence, so they stay
/// with the build that verified them. Chunk payload bytes count the
/// fixed-width generation id, not its value.
pub(super) fn new_verified_receipt(
    metadata_digest: ManifestDigest,
    source: &tracedecay_code_index::production::VerifiedSealedLexicalSourceReceiptV1,
    section_digests: Vec<CodeLexicalArtifactSectionDigestV1>,
    file_size_bytes: u64,
) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
    let artifact_digest = artifact_digest(
        &metadata_digest,
        source.format_revision(),
        source.page_count(),
        source.total_chunks(),
        source.total_payload_bytes(),
        source.total_imports(),
        source.import_payload_bytes(),
        source.import_dictionary_digest(),
        &section_digests,
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
    )?;
    Ok(VerifiedCodeLexicalArtifactV1 {
        format_revision: CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        metadata_digest,
        source_format_revision: source.format_revision(),
        page_count: source.page_count(),
        total_chunks: source.total_chunks(),
        total_payload_bytes: source.total_payload_bytes(),
        total_imports: source.total_imports(),
        import_payload_bytes: source.import_payload_bytes(),
        import_dictionary_digest: source.import_dictionary_digest().clone(),
        artifact_digest,
        section_digests,
        file_size_bytes,
    })
}

#[cfg(test)]
mod tests {
    use roaring::RoaringBitmap;

    use super::{
        DOCUMENT_SET_BITSET, DOCUMENT_SET_DELTAS, PostingListDecoderV1, PostingListEncoderV1,
        decode_document_set, decode_fingerprint_postings, decode_ngram_bitmap, decode_term_lists,
        encode_document_set, encode_fingerprint_postings, encode_ngram_bitmap, encode_term_lists,
    };

    #[test]
    fn v12_ngram_delta_varints_round_trip_sparse_and_dense_shards() {
        for documents in [
            vec![0],
            vec![63],
            vec![64, 65, 127],
            (400_000..400_064).collect::<Vec<_>>(),
        ] {
            let bitmap = RoaringBitmap::from_iter(documents);
            let encoded = encode_ngram_bitmap(&bitmap).expect("encode");
            let decoded = decode_ngram_bitmap(&encoded).expect("decode");
            assert_eq!(decoded, bitmap);
        }
    }

    #[test]
    fn v12_ngram_delta_varints_are_compact_for_measured_shard_shapes() {
        let singleton = RoaringBitmap::from_iter([400_000]);
        let dense = RoaringBitmap::from_iter(400_000..400_064);

        assert_eq!(
            encode_ngram_bitmap(&singleton).expect("singleton"),
            vec![0x80, 0xb5, 0x18]
        );
        assert_eq!(encode_ngram_bitmap(&dense).expect("dense").len(), 66);
    }

    #[test]
    fn v12_ngram_delta_varints_fail_closed_on_noncanonical_or_overflowing_input() {
        for malformed in [
            vec![],
            vec![0x80, 0x00],
            vec![0xff, 0xff, 0xff, 0xff, 0x10],
            vec![0x01, 0x00],
        ] {
            assert!(
                decode_ngram_bitmap(&malformed).is_err(),
                "accepted malformed delta-varint payload {malformed:?}"
            );
        }
    }

    #[test]
    fn posting_lists_round_trip_frequencies_and_spend_no_byte_on_frequency_one() {
        let postings = [(0u32, 1u32), (1, 1), (129, 7), (400_000, 1), (400_001, 300)];
        let mut encoder = PostingListEncoderV1::new(true);
        for (document, frequency) in postings {
            encoder
                .push(document, frequency)
                .expect("ascending posting");
        }
        assert_eq!(encoder.len(), 5);
        let encoded = encoder.finish().expect("encode");
        let decoded = PostingListDecoderV1::new(&encoded, true)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode");
        assert_eq!(decoded, postings);

        let mut ones = PostingListEncoderV1::new(true);
        for document in 0..100 {
            ones.push(document, 1).expect("ascending posting");
        }
        assert_eq!(ones.finish().expect("encode").len(), 100);

        let mut unordered = PostingListEncoderV1::new(true);
        unordered.push(5, 1).expect("first posting");
        assert!(
            unordered.push(5, 1).is_err(),
            "duplicate documents are refused"
        );
        let mut plain = PostingListEncoderV1::new(false);
        assert!(
            plain.push(1, 2).is_err(),
            "document sets carry no frequency"
        );
        assert!(PostingListEncoderV1::new(true).finish().is_err());
        // Flagged frequency one is not canonical.
        assert!(
            PostingListDecoderV1::new(&[0x03, 0x01], true)
                .collect::<Result<Vec<_>, _>>()
                .is_err()
        );
    }

    #[test]
    fn fingerprint_postings_round_trip_and_refuse_non_canonical_order() {
        let postings = [(0u32, 4u32), (0, 9), (3, 1), (300, 0), (300, 70_000)];
        let encoded = encode_fingerprint_postings(&postings).expect("encode");
        assert_eq!(
            decode_fingerprint_postings(&encoded).expect("decode"),
            postings
        );
        assert!(
            encoded.len() < postings.len() * 4,
            "ordinal deltas stay compact"
        );
        assert!(encode_fingerprint_postings(&[(2, 1), (1, 1)]).is_err());
        assert!(encode_fingerprint_postings(&[(1, 1), (1, 1)]).is_err());
        for malformed in [
            vec![0x01, 0x00],
            vec![0x01, 0x02, 0x05],
            vec![0x01, 0x01, 0x00, 0x00, 0x01, 0x00],
        ] {
            assert!(
                decode_fingerprint_postings(&malformed).is_err(),
                "accepted malformed fingerprint postings {malformed:?}"
            );
        }
    }

    #[test]
    fn term_lists_round_trip_and_refuse_non_canonical_fields() {
        let lists = vec![(1i64, 2u64, vec![0x02, 0x04]), (7, 1, vec![0x0a])];
        let encoded = encode_term_lists(&lists).expect("encode");
        let decoded = decode_term_lists(&encoded).expect("decode");
        assert_eq!(
            decoded,
            vec![
                (1, 2, [0x02u8, 0x04].as_slice()),
                (7, 1, [0x0au8].as_slice())
            ]
        );
        assert!(encode_term_lists(&[(7, 1, vec![1]), (7, 1, vec![2])]).is_err());
        assert!(encode_term_lists(&[(1, 0, vec![1])]).is_err());
        for malformed in [
            vec![],
            vec![0x07, 0x01, 0x02, 0x0a],
            vec![0x07, 0x01, 0x01, 0x0a, 0x01, 0x01, 0x01, 0x0b],
        ] {
            assert!(
                decode_term_lists(&malformed).is_err(),
                "accepted malformed term lists {malformed:?}"
            );
        }
    }

    #[test]
    fn document_sets_choose_the_smaller_encoding_and_round_trip() {
        let sparse = RoaringBitmap::from_iter([3u32, 90_000, 300_000]);
        let dense = (1_000u32..9_000).step_by(2).collect::<RoaringBitmap>();
        for (documents, tag) in [
            (&sparse, DOCUMENT_SET_DELTAS),
            (&dense, DOCUMENT_SET_BITSET),
        ] {
            let encoded = encode_document_set(documents).expect("encode");
            assert_eq!(encoded[0], tag);
            assert_eq!(&decode_document_set(&encoded).expect("decode"), documents);
        }
        for documents in [&sparse, &dense, &RoaringBitmap::from_iter([7u32])] {
            let mut list = PostingListEncoderV1::new(false);
            for document in documents {
                list.push(document, 1).expect("ascending");
            }
            assert_eq!(
                list.finish_document_set().expect("list encode"),
                encode_document_set(documents).expect("bitmap encode"),
            );
        }
        assert!(
            PostingListEncoderV1::new(false)
                .finish_document_set()
                .is_err()
        );
        assert!(
            PostingListEncoderV1::new(true)
                .finish_document_set()
                .is_err()
        );
        assert!(
            encode_document_set(&dense).expect("encode").len()
                < 1 + encode_ngram_bitmap(&dense).expect("deltas").len() / 3,
            "a half-dense list is stored as bits, not bytes"
        );
        for malformed in [
            vec![],
            vec![2, 0x01],
            vec![DOCUMENT_SET_BITSET, 0x00, 0x02],
            vec![DOCUMENT_SET_BITSET, 0x00, 0x01, 0x00],
            vec![DOCUMENT_SET_BITSET, 0x05],
        ] {
            assert!(
                decode_document_set(&malformed).is_err(),
                "accepted malformed document set {malformed:?}"
            );
        }
        assert!(encode_document_set(&RoaringBitmap::new()).is_err());
    }
}
