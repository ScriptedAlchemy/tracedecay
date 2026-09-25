use rusqlite::{Transaction, params};
use sha2::{Digest, Sha256};

use super::super::LexicalFieldV1;
use super::prepared::PreparedCodeLexicalArtifactPageV1;
use super::{CodeLexicalArtifactErrorV1, checkpoint};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{ExactFieldV1, nonnegative_sha256_prefix};

/// Revision 26 is the only layout this build serves: interned exact terms,
/// integer field codes, rows stored as deflated blocks of consecutive
/// documents (per-file and per-symbol strings interned once as
/// `row_dictionary` entries, a signature chunk's text stored as the prefix
/// it shares with its body chunk), one `term_postings` row per term text
/// carrying every field's delta-varint list, one `exact_postings` list per
/// exact term and field, one `ngram_postings` list per n-gram rebuilt from
/// the stored rows (the case-preserving kind holds only windows with an ASCII
/// uppercase byte; every other raw window is its normalized window), and the clone index (binary payloads keyed by 32-byte
/// digest, content-only occurrences keyed by symbol digest, and exact and
/// positional winnowed fingerprint postings, each naming its payload or
/// occurrence by integer ordinal) sealed by the same build. Batches append
/// page-ordered staging that finalization merges and drops, so no secondary
/// index duplicates a posting. Annotation uses mint no document. The sealed
/// file holds content only: route identity (generation, repository,
/// freshness, clone occurrence project/worktree/snapshot) and the sealed
/// source's resume cursors are supplied by the opener or dropped before the
/// seal, so identical trees in different worktrees seal byte-identical
/// files. Every other revision is refused as incompatible and rebuilt from
/// the sealed generation.
pub(super) const CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1: u32 = 26;

const DIGEST_DOMAIN: &[u8] = b"tracedecay.code-lexical-artifact.v26\0";

const FIELD_SYMBOL_NAME: i64 = 1;
const FIELD_QUALIFIED_NAME: i64 = 2;
const FIELD_PATH: i64 = 3;
const FIELD_BODY_TEXT: i64 = 4;
const FIELD_PREAMBLE_TEXT: i64 = 5;
const FIELD_EXACT_TERM: i64 = 6;
const FIELD_SUBTOKEN: i64 = 7;
const FIELD_SIGNATURE: i64 = 8;
const FIELD_DOCUMENTATION: i64 = 9;

/// Index wakes: row dictionary with the chunk lookup table, then the three
/// posting merges. Statistics wakes: field totals, then releasing every
/// dropped staging page.
pub(super) const STATISTICS_STEP_COUNT_V11: u64 = 2;
pub(super) const SERVING_INDEX_STEP_COUNT_V11: u64 = 4;

pub(super) fn require_served_revision(revision: u32) -> Result<(), CodeLexicalArtifactErrorV1> {
    if revision == CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1 {
        Ok(())
    } else {
        Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "format revision {revision} is unsupported"
        )))
    }
}

pub(super) fn digest_domain_for_revision(
    revision: u32,
) -> Result<&'static [u8], CodeLexicalArtifactErrorV1> {
    require_served_revision(revision)?;
    Ok(DIGEST_DOMAIN)
}

pub(super) fn field_code(field: LexicalFieldV1) -> i64 {
    match field {
        LexicalFieldV1::SymbolName => FIELD_SYMBOL_NAME,
        LexicalFieldV1::QualifiedName => FIELD_QUALIFIED_NAME,
        LexicalFieldV1::Path => FIELD_PATH,
        LexicalFieldV1::Signature => FIELD_SIGNATURE,
        LexicalFieldV1::Documentation => FIELD_DOCUMENTATION,
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
        FIELD_SIGNATURE => Ok(LexicalFieldV1::Signature),
        FIELD_DOCUMENTATION => Ok(LexicalFieldV1::Documentation),
        FIELD_BODY_TEXT => Ok(LexicalFieldV1::BodyText),
        FIELD_PREAMBLE_TEXT => Ok(LexicalFieldV1::PreambleText),
        FIELD_EXACT_TERM => Ok(LexicalFieldV1::ExactTerm),
        FIELD_SUBTOKEN => Ok(LexicalFieldV1::Subtoken),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact field code {code} is unknown"
        ))),
    }
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

pub(super) fn stable_exact_term_id(term: &[u8]) -> i64 {
    stable_prefixed_id(
        b"tracedecay.code-lexical-artifact.exact-term-id.v12\0",
        term,
    )
}

fn stable_prefixed_id(domain: &[u8], payload: &[u8]) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(payload);
    nonnegative_sha256_prefix(hasher.finalize().as_slice()) as i64
}

/// Content-addressed `row_dictionary` key over the encoded entry. Pages are
/// prepared in parallel, so a first-seen counter could not agree across batch
/// boundaries; a digest of the entry does, and lets a reader verify each
/// resolved entry against the id its row referenced.
pub(super) fn stable_row_dictionary_id(entry: &[u8]) -> i64 {
    stable_prefixed_id(
        b"tracedecay.code-lexical-artifact.row-dictionary-id.v14\0",
        entry,
    )
}

/// Stage every dictionary entry the batch references under its page ordinal.
/// `row_dictionary_pages` is clustered by `(page_ordinal, entry_id)`, so a
/// batch appends at the tail like every other base table; a
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

/// Intern the batch's distinct exact terms, which the insert plan already
/// deduplicated and content-addressed while ordering its rows. `terms` is
/// ascending by `term_id`, the order `exact_vocabulary` was always interned
/// in, so the sealed b-tree keeps the same page layout.
pub(super) fn intern_exact_terms(
    transaction: &Transaction<'_>,
    terms: &[(&[u8], i64)],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO exact_vocabulary(term_id, term) VALUES (?1, ?2) ON CONFLICT(term_id) DO NOTHING",
        )
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut lookup = transaction
        .prepare_cached("SELECT term FROM exact_vocabulary WHERE term_id = ?1")
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    for (term, term_id) in terms {
        checkpoint(control)?;
        insert
            .execute(params![term_id, term])
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        // An id already held by different bytes would silently redirect this
        // batch's postings at the stored term, so the readback stays.
        let stored: Vec<u8> = lookup
            .query_row([term_id], |row| row.get(0))
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        if stored != *term {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact exact term identifier collided".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, CodeLexicalArtifactErrorV1, field_code,
        field_from_code, require_served_revision,
    };
    use crate::retrieval::lexical::LexicalFieldV1;
    use rusqlite::Connection;
    use tracedecay_code_index::production::CodeIndexExecutionControlV1;

    struct ActiveControl;

    impl CodeIndexExecutionControlV1 for ActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    /// The insert plan hands `intern_exact_terms` ids it content-addressed
    /// itself, so a second term claiming an id already held by different bytes
    /// must be rejected by the stored-term readback: `ON CONFLICT DO NOTHING`
    /// would otherwise silently point this batch's postings at the other term.
    #[test]
    fn exact_term_interning_rejects_an_identifier_already_held_by_other_bytes() {
        let mut connection = Connection::open_in_memory().expect("open");
        connection
            .execute_batch(
                "CREATE TABLE exact_vocabulary (term_id INTEGER PRIMARY KEY, term BLOB NOT NULL)",
            )
            .expect("schema");
        let transaction = connection.transaction().expect("transaction");
        super::intern_exact_terms(&transaction, &[(b"alpha", 7)], &ActiveControl).expect("intern");
        // Idempotent for the same bytes: a replayed batch re-interns cleanly.
        super::intern_exact_terms(&transaction, &[(b"alpha", 7)], &ActiveControl).expect("replay");
        let error = super::intern_exact_terms(&transaction, &[(b"beta", 7)], &ActiveControl)
            .expect_err("colliding identifier must fail closed");
        assert!(
            matches!(error, CodeLexicalArtifactErrorV1::Contract(ref message)
                if message.contains("exact term identifier collided")),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn superseded_revisions_are_rejected() {
        require_served_revision(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1)
            .expect("the served revision opens");
        for revision in [16, 20, 22, 25, 27] {
            assert!(matches!(
                require_served_revision(revision),
                Err(CodeLexicalArtifactErrorV1::Incompatible(message))
                    if message == format!("format revision {revision} is unsupported")
            ));
        }
    }

    #[test]
    fn row_dictionary_ids_are_distinct_from_exact_term_ids() {
        assert_ne!(
            super::stable_row_dictionary_id(b"src/lib.rs"),
            super::stable_row_dictionary_id(b"src/lib.rs::main")
        );
        assert_ne!(
            super::stable_row_dictionary_id(b"return"),
            super::stable_exact_term_id(b"return"),
            "dictionary entries and exact terms hash under different domains"
        );
        assert!(super::stable_row_dictionary_id(b"src/lib.rs") >= 0);
    }

    #[test]
    fn field_codes_are_bijective() {
        for field in [
            LexicalFieldV1::SymbolName,
            LexicalFieldV1::QualifiedName,
            LexicalFieldV1::Path,
            LexicalFieldV1::Signature,
            LexicalFieldV1::Documentation,
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
    }
}
