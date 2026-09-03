use std::collections::{BTreeMap, BTreeSet};

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
/// lists and interns exact terms. Readers accept all shipped layouts; writers
/// emit 12 unless an explicit benchmark revision is selected.
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10: u32 = 10;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11: u32 = 11;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12: u32 = 12;
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1: u32 =
    CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12;

const DIGEST_DOMAIN_V10: &[u8] = b"tracedecay.code-lexical-artifact.v10\0";
const DIGEST_DOMAIN_V11: &[u8] = b"tracedecay.code-lexical-artifact.v11\0";
const DIGEST_DOMAIN_V12: &[u8] = b"tracedecay.code-lexical-artifact.v12\0";

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

/// Statistics wakes stay at three steps (field, term, fuzzy flag). Index
/// wakes are the four serving indexes plus n-gram selectivity.
pub(super) const STATISTICS_STEP_COUNT_V11: u64 = 3;
pub(super) const SERVING_INDEX_STEP_COUNT_V11: u64 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeLexicalArtifactWriterRevisionV1 {
    V11,
    #[default]
    V12,
}

impl CodeLexicalArtifactWriterRevisionV1 {
    pub(super) const fn layout(self) -> LexicalArtifactLayoutV1 {
        match self {
            Self::V11 => LexicalArtifactLayoutV1::V11,
            Self::V12 => LexicalArtifactLayoutV1::V12,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LexicalArtifactLayoutV1 {
    V10,
    V11,
    V12,
}

impl LexicalArtifactLayoutV1 {
    pub(super) fn from_revision(revision: u32) -> Result<Self, CodeLexicalArtifactErrorV1> {
        match revision {
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V10 => Ok(Self::V10),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V11 => Ok(Self::V11),
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12 => Ok(Self::V12),
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
        }
    }

    pub(super) fn digest_domain(self) -> &'static [u8] {
        match self {
            Self::V10 => DIGEST_DOMAIN_V10,
            Self::V11 => DIGEST_DOMAIN_V11,
            Self::V12 => DIGEST_DOMAIN_V12,
        }
    }

    pub(super) fn required_indexes(
        self,
    ) -> &'static [(&'static str, &'static str, &'static [&'static str])] {
        match self {
            Self::V10 => &REQUIRED_ARTIFACT_INDEXES_V10,
            Self::V11 => &REQUIRED_ARTIFACT_INDEXES_V11,
            Self::V12 => &REQUIRED_ARTIFACT_INDEXES_V12,
        }
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

pub(super) fn intern_terms(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<BTreeMap<String, i64>, CodeLexicalArtifactErrorV1> {
    let mut terms = BTreeSet::new();
    for page in pages {
        for document in &page.documents {
            for posting in &document.term_postings {
                terms.insert(posting.term.as_str());
            }
        }
    }
    let mut assigned = BTreeMap::new();
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
        assigned.insert(term.to_owned(), term_id);
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
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V12, LexicalArtifactLayoutV1, exact_field_code,
        field_code, field_from_code, stable_exact_term_id,
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
        assert!(LexicalArtifactLayoutV1::from_revision(9).is_err());
        assert!(LexicalArtifactLayoutV1::from_revision(13).is_err());
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
    fn stable_term_ids_are_deterministic_and_content_addressed() {
        assert_eq!(
            super::stable_term_id("return"),
            super::stable_term_id("return")
        );
        assert_ne!(
            super::stable_term_id("return"),
            super::stable_term_id("value")
        );
        assert!(super::stable_term_id("return") >= 0);
    }

    #[test]
    fn exact_term_ids_are_deterministic_over_arbitrary_bytes() {
        assert_eq!(
            stable_exact_term_id(b"\xffreturn"),
            stable_exact_term_id(b"\xffreturn")
        );
        assert_ne!(
            stable_exact_term_id(b"\xffreturn"),
            stable_exact_term_id(b"return")
        );
        assert!(stable_exact_term_id(b"\xffreturn") >= 0);
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
