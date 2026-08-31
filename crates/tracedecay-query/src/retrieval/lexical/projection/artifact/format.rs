use std::collections::BTreeMap;

use roaring::RoaringBitmap;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkId,
    ExactFieldV1, ExactTechnicalTermV1, FileOccurrenceId, LanguageDescriptorRevision,
    ManifestDigest, RepositoryId, SourceFreshness, SourceSpan, SymbolOccurrenceId,
};

use super::super::{CodeLexicalProjectionMetadataV1, LexicalFieldV1, ProjectedChunkV1};
use super::CodeLexicalArtifactErrorV1;

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
// source-page shard.
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1: u32 = 10;
const ARTIFACT_DIGEST_DOMAIN: &[u8] = b"tracedecay.code-lexical-artifact.v10\0";
const REQUIRED_ARTIFACT_INDEXES_V8: [(&str, &str, &[&str]); 7] = [
    ("rows", "rows_by_chunk", &["chunk_id"]),
    (
        "term_postings",
        "term_postings_by_term",
        &["term", "field", "document_id"],
    ),
    (
        "term_postings",
        "term_postings_by_document",
        &["document_id", "field", "term", "frequency"],
    ),
    (
        "term_postings",
        "term_postings_by_document_term",
        &["document_id", "term", "field", "frequency"],
    ),
    ("term_stats", "term_stats_by_term", &["term", "field"]),
    (
        "exact_postings",
        "exact_postings_by_document",
        &["document_id", "field", "term"],
    ),
    (
        "ngram_postings",
        "ngram_postings_by_ngram",
        &["kind", "ngram", "page_ordinal", "cardinality"],
    ),
];
pub(super) const RECEIPT_RESERVATION_BYTES: usize = 16 * 1024;
pub(super) const SECTION_NAMES: [&str; 11] = [
    "source_pages",
    "document_integrity",
    "import_integrity",
    "import_evidence",
    "rows",
    "term_postings",
    "exact_postings",
    "ngram_postings",
    "field_stats",
    "term_stats",
    "vocabulary",
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
        hash_receipt_bytes(&mut self.hasher, value.as_bytes())
    }

    pub(super) fn blob(&mut self, value: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.hasher.update([4]);
        hash_receipt_bytes(&mut self.hasher, value)
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

fn hash_receipt_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(value.len())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
            .to_le_bytes(),
    );
    hasher.update(value);
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
            hash_receipt_bytes(&mut hasher, name.as_bytes())?;
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
        hash_receipt_bytes(&mut hasher, section.name.as_bytes())?;
        hasher.update(page_ordinal.to_le_bytes());
        hasher.update(section.row_count.to_le_bytes());
        hash_receipt_bytes(&mut hasher, section.digest.as_str().as_bytes())?;
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
            hash_receipt_bytes(&mut hasher, name.as_bytes())?;
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

pub(super) fn verify_required_artifact_indexes(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    verify_artifact_table_layout(connection)?;
    let mut statement = connection
        .prepare("SELECT name, desc, coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno")
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact index schema is unreadable: {error}"
            ))
        })?;
    for (table, index, expected_columns) in REQUIRED_ARTIFACT_INDEXES_V8 {
        let partial: Option<i64> = connection
            .query_row(
                "SELECT partial FROM pragma_index_list(?1) WHERE name = ?2",
                [table, index],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                CodeLexicalArtifactErrorV1::Incompatible(format!(
                    "artifact index {index} is unreadable: {error}"
                ))
            })?;
        let columns = statement
            .query_map([index], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| {
                CodeLexicalArtifactErrorV1::Incompatible(format!(
                    "artifact index {index} is unreadable: {error}"
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                CodeLexicalArtifactErrorV1::Incompatible(format!(
                    "artifact index {index} is unreadable: {error}"
                ))
            })?;
        if partial != Some(0)
            || !columns
                .iter()
                .map(|(column, descending, collation)| {
                    (column.as_str(), *descending, collation.as_str())
                })
                .eq(expected_columns.iter().map(|column| (*column, 0, "BINARY")))
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact index {index} has columns {columns:?}; revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} requires {expected_columns:?}"
            )));
        }
    }
    Ok(())
}

pub(super) fn verify_artifact_table_layout(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let source_columns = connection
        .prepare(
            "SELECT name, type, [notnull], pk FROM pragma_table_xinfo('source_pages') WHERE hidden = 0 ORDER BY cid",
        )
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
                "artifact source-page columns are unreadable: {error}"
            ))
        })?;
    let expected_source_columns = [
        ("page_ordinal", "INTEGER", 0, 1),
        ("page_digest", "TEXT", 1, 0),
        ("cumulative_digest", "TEXT", 1, 0),
        ("chunk_count", "INTEGER", 1, 0),
        ("payload_bytes", "INTEGER", 1, 0),
        ("import_count", "INTEGER", 1, 0),
        ("import_payload_bytes", "INTEGER", 1, 0),
        ("import_dictionary_digest", "TEXT", 1, 0),
        ("ngram_digest", "TEXT", 1, 0),
        ("base_sections_receipt", "BLOB", 1, 0),
        ("next_cursor", "BLOB", 1, 0),
    ];
    if !source_columns
        .iter()
        .map(|(name, column_type, not_null, primary_key)| {
            (name.as_str(), column_type.as_str(), *not_null, *primary_key)
        })
        .eq(expected_source_columns)
    {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "artifact source-page table has columns {source_columns:?}; revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} requires append receipts"
        )));
    }
    let without_rowid: Option<i64> = connection
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = 'ngram_postings' AND type = 'table'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact ngram table schema is unreadable: {error}"
            ))
        })?;
    let mut statement = connection
        .prepare(
            "SELECT name, type, [notnull], pk FROM pragma_table_xinfo('ngram_postings') WHERE hidden = 0 ORDER BY cid",
        )
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact ngram columns are unreadable: {error}"
            ))
        })?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact ngram columns are unreadable: {error}"
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact ngram columns are unreadable: {error}"
            ))
        })?;
    let expected = [
        ("page_ordinal", "INTEGER", 1, 1),
        ("kind", "INTEGER", 1, 2),
        ("ngram", "INTEGER", 1, 3),
        ("documents", "BLOB", 1, 0),
        ("cardinality", "INTEGER", 1, 0),
    ];
    if without_rowid != Some(1)
        || !columns
            .iter()
            .map(|(name, column_type, not_null, primary_key)| {
                (name.as_str(), column_type.as_str(), *not_null, *primary_key)
            })
            .eq(expected)
    {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "artifact ngram table has columns {columns:?} and without-rowid state {without_rowid:?}; revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} requires source-page bitmap shards"
        )));
    }
    let statistics_without_rowid: Option<i64> = connection
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = 'ngram_statistics' AND type = 'table'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact ngram statistics schema is unreadable: {error}"
            ))
        })?;
    let statistics_columns = connection
        .prepare(
            "SELECT name, type, [notnull], pk FROM pragma_table_xinfo('ngram_statistics') WHERE hidden = 0 ORDER BY cid",
        )
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
                "artifact ngram statistics columns are unreadable: {error}"
            ))
        })?;
    let expected_statistics = [
        ("kind", "INTEGER", 1, 1),
        ("ngram", "INTEGER", 1, 2),
        ("document_frequency", "INTEGER", 1, 0),
    ];
    if statistics_without_rowid != Some(1)
        || !statistics_columns
            .iter()
            .map(|(name, column_type, not_null, primary_key)| {
                (name.as_str(), column_type.as_str(), *not_null, *primary_key)
            })
            .eq(expected_statistics)
    {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "artifact ngram statistics table has columns {statistics_columns:?} and without-rowid state {statistics_without_rowid:?}; revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} requires finalized selectivity statistics"
        )));
    }
    Ok(())
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

pub(super) fn encode_ngram_bitmap(
    bitmap: &RoaringBitmap,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let cardinality = bitmap.len();
    let mut range_count = 0u64;
    let mut previous: Option<u32> = None;
    for document in bitmap.iter() {
        if previous.is_none_or(|previous| document != previous.saturating_add(1)) {
            range_count = range_count.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact ngram range count overflowed".to_owned(),
                )
            })?;
        }
        previous = Some(document);
    }
    let list_bytes = cardinality.checked_mul(4).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact ngram document-list size overflowed".to_owned(),
        )
    })?;
    let range_bytes = range_count.checked_mul(8).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact ngram range size overflowed".to_owned(),
        )
    })?;
    let ranges = range_bytes < list_bytes;
    let payload_bytes = if ranges { range_bytes } else { list_bytes };
    let capacity = usize::try_from(payload_bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
        .checked_add(16)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact ngram bitmap size overflowed".to_owned(),
            )
        })?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(b"TDN1");
    encoded.push(u8::from(ranges));
    encoded.extend_from_slice(&[0u8; 3]);
    encoded.extend_from_slice(&cardinality.to_le_bytes());
    if ranges {
        let mut start: Option<u32> = None;
        let mut previous: Option<u32> = None;
        for document in bitmap.iter() {
            if previous.is_some_and(|previous| document != previous.saturating_add(1)) {
                let start_value = start.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact ngram range is missing its start".to_owned(),
                    )
                })?;
                let previous_value = previous.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact ngram range is missing its end".to_owned(),
                    )
                })?;
                encoded.extend_from_slice(&start_value.to_le_bytes());
                encoded.extend_from_slice(&(previous_value - start_value).to_le_bytes());
                start = Some(document);
            } else if start.is_none() {
                start = Some(document);
            }
            previous = Some(document);
        }
        if let (Some(start), Some(previous)) = (start, previous) {
            encoded.extend_from_slice(&start.to_le_bytes());
            encoded.extend_from_slice(&(previous - start).to_le_bytes());
        }
    } else {
        for document in bitmap.iter() {
            encoded.extend_from_slice(&document.to_le_bytes());
        }
    }
    Ok(encoded)
}

pub(super) fn decode_ngram_bitmap(
    encoded: &[u8],
) -> Result<RoaringBitmap, CodeLexicalArtifactErrorV1> {
    let header = encoded.get(..16).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact ngram bitmap header is truncated".to_owned(),
        )
    })?;
    if &header[..4] != b"TDN1" || header[5..8] != [0u8; 3] || header[4] > 1 {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact ngram bitmap header is invalid".to_owned(),
        ));
    }
    let cardinality = u64::from_le_bytes(header[8..16].try_into().map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact ngram bitmap cardinality is malformed".to_owned(),
        )
    })?);
    let width = if header[4] == 1 { 8usize } else { 4usize };
    if !(encoded.len() - 16).is_multiple_of(width) {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact ngram bitmap payload is truncated".to_owned(),
        ));
    }
    let mut bitmap = RoaringBitmap::new();
    let mut previous: Option<u32> = None;
    for item in encoded[16..].chunks_exact(width) {
        let start = u32::from_le_bytes(item[..4].try_into().map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact ngram bitmap document is malformed".to_owned(),
            )
        })?);
        let end = if width == 8 {
            let run = u32::from_le_bytes(item[4..8].try_into().map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact ngram bitmap run is malformed".to_owned(),
                )
            })?);
            start.checked_add(run).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact ngram bitmap run overflowed".to_owned(),
                )
            })?
        } else {
            start
        };
        if previous.is_some_and(|previous| start <= previous) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact ngram bitmap documents are not strictly ordered".to_owned(),
            ));
        }
        bitmap.insert_range(start..=end);
        previous = Some(end);
    }
    if bitmap.len() != cardinality {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact ngram bitmap cardinality does not verify".to_owned(),
        ));
    }
    Ok(bitmap)
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
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    metadata_digest: ManifestDigest,
    source_state_digest: ManifestDigest,
    source_cumulative_digest: ManifestDigest,
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

    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    pub fn repository_id(&self) -> Option<&RepositoryId> {
        self.repository_id.as_ref()
    }

    pub fn freshness(&self) -> &SourceFreshness {
        &self.freshness
    }

    pub fn source_state_digest(&self) -> &ManifestDigest {
        &self.source_state_digest
    }

    pub fn source_cumulative_digest(&self) -> &ManifestDigest {
        &self.source_cumulative_digest
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

    pub(super) fn format_revision(&self) -> u32 {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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

pub(super) fn metadata_digest(
    metadata: &CodeLexicalProjectionMetadataV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(b"tracedecay.code-lexical-artifact-metadata.v1\0", metadata)
}

#[allow(clippy::too_many_arguments)] // one committed digest tuple, spelled once
pub(super) fn artifact_digest(
    metadata_digest: &ManifestDigest,
    source_state_digest: &ManifestDigest,
    source_format_revision: u32,
    page_count: u64,
    total_chunks: u64,
    total_payload_bytes: u64,
    total_imports: u64,
    import_payload_bytes: u64,
    import_dictionary_digest: &ManifestDigest,
    source_cumulative_digest: &ManifestDigest,
    sections: &[CodeLexicalArtifactSectionDigestV1],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    manifest_digest(
        ARTIFACT_DIGEST_DOMAIN,
        &(
            metadata_digest.as_str(),
            source_state_digest.as_str(),
            source_format_revision,
            page_count,
            total_chunks,
            total_payload_bytes,
            total_imports,
            import_payload_bytes,
            import_dictionary_digest.as_str(),
            source_cumulative_digest.as_str(),
            sections,
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        ),
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

pub(super) fn new_verified_receipt(
    metadata: CodeLexicalProjectionMetadataV1,
    metadata_digest: ManifestDigest,
    source: &tracedecay_code_index::production::VerifiedSealedLexicalSourceReceiptV1,
    artifact_digest: ManifestDigest,
    section_digests: Vec<CodeLexicalArtifactSectionDigestV1>,
    file_size_bytes: u64,
) -> VerifiedCodeLexicalArtifactV1 {
    VerifiedCodeLexicalArtifactV1 {
        format_revision: CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        generation: metadata.generation,
        repository_id: metadata.repository_id,
        freshness: metadata.freshness,
        metadata_digest,
        source_state_digest: source.source_state_digest().clone(),
        source_cumulative_digest: source.cumulative_digest().clone(),
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
    }
}
