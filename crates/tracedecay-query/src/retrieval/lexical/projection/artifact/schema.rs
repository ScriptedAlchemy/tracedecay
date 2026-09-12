use std::collections::{BTreeMap, BTreeSet, HashSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::super::LexicalFieldV1;
use super::prepared::PreparedCodeLexicalArtifactPageV1;
use super::{CodeLexicalArtifactErrorV1, checkpoint};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::ExactFieldV1;

/// Revision 10 is the last TEXT-term posting layout. Revision 11 interns
/// terms, stores integer field codes, drops redundant serving indexes, and
/// writes compact row payloads. Revision 12 delta-encodes n-gram document
/// lists and interns exact terms. Revision 13 clusters `term_postings` by
/// `(document_id, term_id, field)` so every batch appends to the tail of the
/// tree instead of rewriting leaves across the whole hashed term-id space,
/// and derives the term-leading serving index once at finalization.
/// Revision 14 replaces the JSON row payload with a binary one whose
/// per-file and per-symbol strings (paths, occurrence identities, display
/// names, descriptor revisions) are interned once as `row_dictionary`
/// entries and referenced by content-addressed id. Readers accept all
/// shipped layouts; writers emit 14 unless an explicit benchmark revision is
/// selected.
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10: u32 = 10;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11: u32 = 11;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12: u32 = 12;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V13: u32 = 13;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14: u32 = 14;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1: u32 =
    CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14;

const DIGEST_DOMAIN_V10: &[u8] = b"tracedecay.code-lexical-artifact.v10\0";
const DIGEST_DOMAIN_V11: &[u8] = b"tracedecay.code-lexical-artifact.v11\0";
const DIGEST_DOMAIN_V12: &[u8] = b"tracedecay.code-lexical-artifact.v12\0";
const DIGEST_DOMAIN_V13: &[u8] = b"tracedecay.code-lexical-artifact.v13\0";
const DIGEST_DOMAIN_V14: &[u8] = b"tracedecay.code-lexical-artifact.v14\0";

const FIELD_SYMBOL_NAME: i64 = 1;
const FIELD_QUALIFIED_NAME: i64 = 2;
const FIELD_PATH: i64 = 3;
const FIELD_BODY_TEXT: i64 = 4;
const FIELD_PREAMBLE_TEXT: i64 = 5;
const FIELD_EXACT_TERM: i64 = 6;
const FIELD_SUBTOKEN: i64 = 7;

pub(super) const REQUIRED_ARTIFACT_INDEXES_V10: [(&str, &str, &[&str]); 7] = [
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

/// Serving indexes retained after EXPLAIN QUERY PLAN on the live read
/// shapes: chunk lookup, one document-leading posting probe, exact
/// document membership, and n-gram page shards. The revision-10
/// term-leading and duplicate document-term indexes are covered by the
/// interned primary key `(term_id, field, document_id)`.
pub(super) const REQUIRED_ARTIFACT_INDEXES_V11: [(&str, &str, &[&str]); 4] = [
    ("rows", "rows_by_chunk", &["chunk_id"]),
    (
        "term_postings",
        "term_postings_by_document",
        &["document_id", "term_id", "field", "frequency"],
    ),
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

pub(super) const REQUIRED_ARTIFACT_INDEXES_V12: [(&str, &str, &[&str]); 4] = [
    ("rows", "rows_by_chunk", &["chunk_id"]),
    (
        "term_postings",
        "term_postings_by_document",
        &["document_id", "term_id", "field", "frequency"],
    ),
    (
        "exact_postings",
        "exact_postings_by_document",
        &["document_id", "field", "term_id"],
    ),
    (
        "ngram_postings",
        "ngram_postings_by_ngram",
        &["kind", "ngram", "page_ordinal", "cardinality"],
    ),
];

/// Revision 13 keeps the interned exact layout of revision 12 but clusters
/// `term_postings` by document, so the term-leading probe index replaces the
/// document-leading one.
pub(super) const REQUIRED_ARTIFACT_INDEXES_V13: [(&str, &str, &[&str]); 4] = [
    ("rows", "rows_by_chunk", &["chunk_id"]),
    (
        "term_postings",
        "term_postings_by_term",
        &["term_id", "field", "document_id", "frequency"],
    ),
    (
        "exact_postings",
        "exact_postings_by_document",
        &["document_id", "field", "term_id"],
    ),
    (
        "ngram_postings",
        "ngram_postings_by_ngram",
        &["kind", "ngram", "page_ordinal", "cardinality"],
    ),
];

/// Statistics wakes stay at three steps (field, term, fuzzy flag). Index
/// wakes are the four serving indexes plus n-gram selectivity.
pub(super) const STATISTICS_STEP_COUNT_V11: u64 = 3;
pub(super) const SERVING_INDEX_STEP_COUNT_V11: u64 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeLexicalArtifactWriterRevisionV1 {
    V11,
    V12,
    V13,
    #[default]
    V14,
}

impl CodeLexicalArtifactWriterRevisionV1 {
    pub(super) const fn layout(self) -> LexicalArtifactLayoutV1 {
        match self {
            Self::V11 => LexicalArtifactLayoutV1::V11,
            Self::V12 => LexicalArtifactLayoutV1::V12,
            Self::V13 => LexicalArtifactLayoutV1::V13,
            Self::V14 => LexicalArtifactLayoutV1::V14,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LexicalArtifactLayoutV1 {
    V10,
    V11,
    V12,
    V13,
    V14,
}

impl LexicalArtifactLayoutV1 {
    pub(super) fn from_revision(revision: u32) -> Result<Self, CodeLexicalArtifactErrorV1> {
        match revision {
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10 => Ok(Self::V10),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11 => Ok(Self::V11),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12 => Ok(Self::V12),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V13 => Ok(Self::V13),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14 => Ok(Self::V14),
            _ => Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
                "format revision {revision} is unsupported"
            ))),
        }
    }

    pub(super) fn revision(self) -> u32 {
        match self {
            Self::V10 => CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10,
            Self::V11 => CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11,
            Self::V12 => CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12,
            Self::V13 => CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V13,
            Self::V14 => CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14,
        }
    }

    pub(super) fn digest_domain(self) -> &'static [u8] {
        match self {
            Self::V10 => DIGEST_DOMAIN_V10,
            Self::V11 => DIGEST_DOMAIN_V11,
            Self::V12 => DIGEST_DOMAIN_V12,
            Self::V13 => DIGEST_DOMAIN_V13,
            Self::V14 => DIGEST_DOMAIN_V14,
        }
    }

    pub(super) fn required_indexes(
        self,
    ) -> &'static [(&'static str, &'static str, &'static [&'static str])] {
        match self {
            Self::V10 => &REQUIRED_ARTIFACT_INDEXES_V10,
            Self::V11 => &REQUIRED_ARTIFACT_INDEXES_V11,
            Self::V12 => &REQUIRED_ARTIFACT_INDEXES_V12,
            // Revision 14 changes only the row payload and its string
            // dictionary; the serving indexes are revision 13's.
            Self::V13 | Self::V14 => &REQUIRED_ARTIFACT_INDEXES_V13,
        }
    }

    /// Revisions 12 and later intern exact terms through `exact_vocabulary`.
    pub(super) fn interns_exact_terms(self) -> bool {
        matches!(self, Self::V12 | Self::V13 | Self::V14)
    }

    /// Revisions 13 and later cluster `term_postings` by `(document_id,
    /// term_id, field)`; every earlier interned layout clusters by term.
    pub(super) fn clusters_term_postings_by_document(self) -> bool {
        matches!(self, Self::V13 | Self::V14)
    }

    /// Revision 14 rows reference `row_dictionary` entries for their per-file
    /// and per-symbol strings instead of carrying the text per chunk.
    pub(super) fn interns_row_dictionary(self) -> bool {
        self == Self::V14
    }

    /// Revision 14 keeps `document_integrity` as `(document_id, digest
    /// BLOB)`: the chunk id already lives in `rows` under the same key, and
    /// the 32 digest bytes replace their 71-byte tagged hex form.
    pub(super) fn stores_document_integrity_bytes(self) -> bool {
        self == Self::V14
    }
}

pub(super) fn digest_domain_for_revision(
    revision: u32,
) -> Result<&'static [u8], CodeLexicalArtifactErrorV1> {
    Ok(LexicalArtifactLayoutV1::from_revision(revision)?.digest_domain())
}

pub(super) fn field_code(field: LexicalFieldV1) -> i64 {
    match field {
        LexicalFieldV1::SymbolName => FIELD_SYMBOL_NAME,
        LexicalFieldV1::QualifiedName => FIELD_QUALIFIED_NAME,
        LexicalFieldV1::Path => FIELD_PATH,
        LexicalFieldV1::BodyText => FIELD_BODY_TEXT,
        LexicalFieldV1::PreambleText => FIELD_PREAMBLE_TEXT,
        LexicalFieldV1::ExactTerm => FIELD_EXACT_TERM,
        LexicalFieldV1::Subtoken => FIELD_SUBTOKEN,
    }
}

pub(super) fn field_from_code(code: i64) -> Result<LexicalFieldV1, CodeLexicalArtifactErrorV1> {
    match code {
        FIELD_SYMBOL_NAME => Ok(LexicalFieldV1::SymbolName),
        FIELD_QUALIFIED_NAME => Ok(LexicalFieldV1::QualifiedName),
        FIELD_PATH => Ok(LexicalFieldV1::Path),
        FIELD_BODY_TEXT => Ok(LexicalFieldV1::BodyText),
        FIELD_PREAMBLE_TEXT => Ok(LexicalFieldV1::PreambleText),
        FIELD_EXACT_TERM => Ok(LexicalFieldV1::ExactTerm),
        FIELD_SUBTOKEN => Ok(LexicalFieldV1::Subtoken),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact field code {code} is unknown"
        ))),
    }
}

pub(super) fn field_code_from_encoded(encoded: &str) -> Result<i64, CodeLexicalArtifactErrorV1> {
    let field: LexicalFieldV1 = serde_json::from_str(encoded)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    Ok(field_code(field))
}

pub(super) fn exact_field_code(field: ExactFieldV1) -> i64 {
    match field {
        ExactFieldV1::Identifier => 1,
        ExactFieldV1::QualifiedName => 2,
        ExactFieldV1::Path => 3,
        ExactFieldV1::QuotedPhrase => 4,
        ExactFieldV1::DiagnosticCode => 5,
        ExactFieldV1::DiagnosticText => 6,
        ExactFieldV1::CompilerOrRuntimeError => 7,
        ExactFieldV1::CliFlag => 8,
        ExactFieldV1::ToolName => 9,
        ExactFieldV1::ConfigurationKey => 10,
        ExactFieldV1::CommitIdentifier => 11,
        ExactFieldV1::TaskOrSessionId => 12,
        ExactFieldV1::ProtocolField => 13,
    }
}

pub(super) fn exact_field_code_from_encoded(
    encoded: &str,
) -> Result<i64, CodeLexicalArtifactErrorV1> {
    let field: ExactFieldV1 = serde_json::from_str(encoded)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    Ok(exact_field_code(field))
}

/// Content-addressed term primary key. Incrementing IDs follow first-seen
/// batch order, so one-page and multi-page commits of the same source would
/// disagree on `vocabulary` / `term_stats` section receipts.
pub(super) fn stable_term_id(term: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact.term-id.v11\0");
    hasher.update(term.as_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(prefix) & i64::MAX as u64) as i64
}

pub(super) fn stable_exact_term_id(term: &[u8]) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact.exact-term-id.v12\0");
    hasher.update(term);
    let digest = hasher.finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(prefix) & i64::MAX as u64) as i64
}

/// Content-addressed `row_dictionary` key over the encoded entry. Pages are
/// prepared in parallel, so a first-seen counter could not agree across batch
/// boundaries; a digest of the entry does, and lets a reader verify each
/// resolved entry against the id its row referenced.
pub(super) fn stable_row_dictionary_id(entry: &[u8]) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact.row-dictionary-id.v14\0");
    hasher.update(entry);
    let digest = hasher.finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(prefix) & i64::MAX as u64) as i64
}

/// Stage every dictionary entry the batch references under its page ordinal.
/// `row_dictionary_pages` is clustered by `(page_ordinal, entry_id)`, so a
/// batch appends at the tail like every other revision-13 base table; a
/// hash-keyed insert straight into `row_dictionary` would dirty most of that
/// tree on every batch (measured: +540 MiB of journal and page rewrites over
/// 37 batches on a 24 MB dictionary). Finalization derives the deduplicated
/// `row_dictionary` from the staging table in one sorted pass and checks id
/// collisions there.
pub(super) fn stage_row_dictionary(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO row_dictionary_pages(page_ordinal, entry_id, entry) VALUES (?1, ?2, ?3)",
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    for page in pages {
        let page_ordinal = i64::try_from(page.page_ordinal)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        for (entry_id, entry) in &page.row_dictionary {
            checkpoint(control)?;
            insert
                .execute(params![page_ordinal, entry_id, entry.as_slice()])
                .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        }
    }
    Ok(())
}

/// Derive the sealed `row_dictionary` from the staged pages: refuse any id
/// that two pages encoded differently, keep one entry per id, and drop the
/// staging table so the pages it held are reused by the serving indexes
/// built in the same finalization phase.
pub(super) fn derive_row_dictionary(
    transaction: &Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let collided: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM row_dictionary_pages GROUP BY entry_id HAVING MIN(entry) <> MAX(entry))",
            [],
            |row| row.get(0),
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    if collided {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact row dictionary identifier collided".to_owned(),
        ));
    }
    transaction
        .execute_batch(
            "INSERT INTO row_dictionary(entry_id, entry) SELECT entry_id, MIN(entry) FROM row_dictionary_pages GROUP BY entry_id;
             DROP TABLE row_dictionary_pages;
             CREATE TRIGGER frozen_row_dictionary_insert BEFORE INSERT ON row_dictionary BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary'); END;
             CREATE TRIGGER frozen_row_dictionary_update BEFORE UPDATE ON row_dictionary BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary'); END;
             CREATE TRIGGER frozen_row_dictionary_delete BEFORE DELETE ON row_dictionary BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary'); END;",
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))
}

pub(super) fn intern_exact_terms(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut terms = BTreeMap::<i64, &[u8]>::new();
    for page in pages {
        for document in &page.documents {
            for (_, term) in &document.exact_postings {
                let term_id = stable_exact_term_id(term);
                if let Some(previous) = terms.insert(term_id, term)
                    && previous != term.as_slice()
                {
                    return Err(CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact exact term identifier collided".to_owned(),
                    ));
                }
            }
        }
    }
    let mut insert = transaction
        .prepare(
            "INSERT INTO exact_vocabulary(term_id, term) VALUES (?1, ?2) ON CONFLICT(term_id) DO NOTHING",
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut lookup = transaction
        .prepare("SELECT term FROM exact_vocabulary WHERE term_id = ?1")
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    for (term_id, term) in terms {
        checkpoint(control)?;
        insert
            .execute(params![term_id, term])
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let stored: Vec<u8> = lookup
            .query_row([term_id], |row| row.get(0))
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        if stored != term {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact exact term identifier collided".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Intern the batch's distinct terms and return the ids now present in
/// `vocabulary`, so the posting writer can confirm every planned posting's
/// term was interned with one integer probe per row.
pub(super) fn intern_terms(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<HashSet<i64>, CodeLexicalArtifactErrorV1> {
    let mut terms = BTreeSet::new();
    for page in pages {
        for document in &page.documents {
            for posting in &document.term_postings {
                terms.insert(posting.term.as_str());
            }
        }
    }
    let mut assigned = HashSet::with_capacity(terms.len());
    let mut insert = transaction
        .prepare(
            "INSERT INTO vocabulary(term_id, term, in_fuzzy) VALUES (?1, ?2, 0) ON CONFLICT(term) DO NOTHING",
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    for term in terms {
        checkpoint(control)?;
        let term_id = stable_term_id(term);
        insert.execute(params![term_id, term]).map_err(|error| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact term identifier collided or vocabulary insert failed: {error}"
            ))
        })?;
        assigned.insert(term_id);
    }
    Ok(assigned)
}

pub(super) fn lookup_term_id(
    connection: &Connection,
    term: &str,
) -> Result<Option<i64>, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT term_id FROM vocabulary WHERE term = ?1",
            [term],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))
}

pub(super) fn lookup_term_ids(
    connection: &Connection,
    terms: &BTreeSet<String>,
) -> Result<BTreeMap<String, i64>, CodeLexicalArtifactErrorV1> {
    let mut assigned = BTreeMap::new();
    if terms.is_empty() {
        return Ok(assigned);
    }
    let placeholders = std::iter::repeat_n("?", terms.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT term, term_id FROM vocabulary WHERE term IN ({placeholders})");
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(terms.iter()))
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
    {
        assigned.insert(
            row.get(0)
                .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?,
            row.get(1)
                .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?,
        );
    }
    Ok(assigned)
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11,
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V13,
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14, LexicalArtifactLayoutV1, exact_field_code,
        field_code, field_from_code,
    };
    use crate::retrieval::lexical::LexicalFieldV1;
    use tracedecay_domain::ExactFieldV1;

    #[test]
    fn layout_accepts_open_revisions_and_fails_closed_otherwise() {
        assert_eq!(
            LexicalArtifactLayoutV1::from_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10)
                .expect("v10"),
            LexicalArtifactLayoutV1::V10
        );
        assert_eq!(
            LexicalArtifactLayoutV1::from_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11)
                .expect("v11"),
            LexicalArtifactLayoutV1::V11
        );
        assert_eq!(
            LexicalArtifactLayoutV1::from_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12)
                .expect("v12"),
            LexicalArtifactLayoutV1::V12
        );
        assert_eq!(
            LexicalArtifactLayoutV1::from_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V13)
                .expect("v13"),
            LexicalArtifactLayoutV1::V13
        );
        assert_eq!(
            LexicalArtifactLayoutV1::from_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V14)
                .expect("v14"),
            LexicalArtifactLayoutV1::V14
        );
        assert!(LexicalArtifactLayoutV1::from_revision(9).is_err());
        assert!(LexicalArtifactLayoutV1::from_revision(15).is_err());
    }

    #[test]
    fn row_dictionary_ids_are_deterministic_and_distinct_from_term_ids() {
        assert_eq!(
            super::stable_row_dictionary_id(b"src/lib.rs"),
            super::stable_row_dictionary_id(b"src/lib.rs")
        );
        assert_ne!(
            super::stable_row_dictionary_id(b"src/lib.rs"),
            super::stable_row_dictionary_id(b"src/lib.rs::main")
        );
        assert_ne!(
            super::stable_row_dictionary_id(b"return"),
            super::stable_term_id("return"),
            "dictionary entries and vocabulary terms hash under different domains"
        );
        assert!(super::stable_row_dictionary_id(b"src/lib.rs") >= 0);
    }

    #[test]
    fn field_codes_are_stable_and_bijective() {
        for field in [
            LexicalFieldV1::SymbolName,
            LexicalFieldV1::QualifiedName,
            LexicalFieldV1::Path,
            LexicalFieldV1::BodyText,
            LexicalFieldV1::PreambleText,
            LexicalFieldV1::ExactTerm,
            LexicalFieldV1::Subtoken,
        ] {
            let code = field_code(field);
            assert_eq!(field_from_code(code).expect("round-trip"), field);
        }
        assert!(field_from_code(0).is_err());
        assert!(field_from_code(99).is_err());
        assert_eq!(field_code(LexicalFieldV1::Subtoken), 7);
    }

    #[test]
    fn exact_field_codes_are_stable() {
        for (field, code) in [
            (ExactFieldV1::Identifier, 1),
            (ExactFieldV1::QualifiedName, 2),
            (ExactFieldV1::Path, 3),
            (ExactFieldV1::QuotedPhrase, 4),
            (ExactFieldV1::DiagnosticCode, 5),
            (ExactFieldV1::DiagnosticText, 6),
            (ExactFieldV1::CompilerOrRuntimeError, 7),
            (ExactFieldV1::CliFlag, 8),
            (ExactFieldV1::ToolName, 9),
            (ExactFieldV1::ConfigurationKey, 10),
            (ExactFieldV1::CommitIdentifier, 11),
            (ExactFieldV1::TaskOrSessionId, 12),
            (ExactFieldV1::ProtocolField, 13),
        ] {
            assert_eq!(exact_field_code(field), code);
        }
    }
}
