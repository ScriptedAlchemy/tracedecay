//! Staging schema and transaction-scoped writer authority.

use super::super::format::{
    BASE_SECTION_NAMES, contract_number, decode_page_base_sections_receipt,
};
use super::super::{CodeLexicalArtifactErrorV1, sqlite_corrupt, sqlite_error};
use super::finalization::{content_epoch, verify_clone_rows};
use super::source_chain_overflow;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tracedecay_code_index::production::VerifiedSealedLexicalSourceReceiptV1;

// This gate serializes mutation within the private-profile/stable-handle
// authority. It denies ordinary second-connection DML, but is not a
// cryptographic defense against malicious same-UID code that deliberately
// registers a lookalike SQLite function.
const BUILDER_MUTATION_GATE_FUNCTION: &str = "tracedecay_lexical_builder_append_authorized";
const BUILDER_MUTATION_IDLE: u8 = 0;
const BUILDER_MUTATION_APPEND: u8 = 1;
const BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 14] = [
    ("builder_gate_source_pages_insert", "source_pages", "INSERT"),
    ("builder_gate_row_blocks_insert", "row_blocks", "INSERT"),
    ("builder_gate_row_blocks_update", "row_blocks", "UPDATE"),
    ("builder_gate_row_blocks_delete", "row_blocks", "DELETE"),
    ("builder_gate_row_chunks_insert", "row_chunks", "INSERT"),
    ("builder_gate_row_chunks_update", "row_chunks", "UPDATE"),
    ("builder_gate_row_chunks_delete", "row_chunks", "DELETE"),
    (
        "builder_gate_term_postings_insert",
        "term_postings",
        "INSERT",
    ),
    (
        "builder_gate_term_postings_update",
        "term_postings",
        "UPDATE",
    ),
    (
        "builder_gate_term_postings_delete",
        "term_postings",
        "DELETE",
    ),
    (
        "builder_gate_exact_postings_insert",
        "exact_postings",
        "INSERT",
    ),
    (
        "builder_gate_exact_postings_update",
        "exact_postings",
        "UPDATE",
    ),
    (
        "builder_gate_exact_postings_delete",
        "exact_postings",
        "DELETE",
    ),
    (
        "builder_gate_ngram_postings_insert",
        "ngram_postings",
        "INSERT",
    ),
];
/// Batches append each document's chunk id here in document order;
/// finalization sorts it into `row_chunks` and drops it, gates included.
const SOURCE_PAGE_CURSORS_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 1] = [(
    "builder_gate_source_page_cursors_insert",
    "source_page_cursors",
    "INSERT",
)];
const SOURCE_PAGE_CURSORS_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 2] = [
    (
        "immutable_source_page_cursors_update",
        "source_page_cursors",
        "UPDATE",
        "immutable lexical source page cursors",
    ),
    (
        "immutable_source_page_cursors_delete",
        "source_page_cursors",
        "DELETE",
        "immutable lexical source page cursors",
    ),
];
const ROW_CHUNK_PAGES_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_row_chunk_pages_insert",
        "row_chunk_pages",
        "INSERT",
    ),
    (
        "builder_gate_row_chunk_pages_update",
        "row_chunk_pages",
        "UPDATE",
    ),
    (
        "builder_gate_row_chunk_pages_delete",
        "row_chunk_pages",
        "DELETE",
    ),
];
/// Batches append postings in page order to these staging tables under the
/// private-builder gate; finalization merges each into its sealed serving
/// table and drops it, gates included.
const TERM_POSTING_RUNS_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_term_posting_runs_insert",
        "term_posting_runs",
        "INSERT",
    ),
    (
        "builder_gate_term_posting_runs_update",
        "term_posting_runs",
        "UPDATE",
    ),
    (
        "builder_gate_term_posting_runs_delete",
        "term_posting_runs",
        "DELETE",
    ),
];
const EXACT_POSTING_RUNS_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_exact_posting_runs_insert",
        "exact_posting_runs",
        "INSERT",
    ),
    (
        "builder_gate_exact_posting_runs_update",
        "exact_posting_runs",
        "UPDATE",
    ),
    (
        "builder_gate_exact_posting_runs_delete",
        "exact_posting_runs",
        "DELETE",
    ),
];
const EXACT_VOCABULARY_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_exact_vocabulary_insert",
        "exact_vocabulary",
        "INSERT",
    ),
    (
        "builder_gate_exact_vocabulary_update",
        "exact_vocabulary",
        "UPDATE",
    ),
    (
        "builder_gate_exact_vocabulary_delete",
        "exact_vocabulary",
        "DELETE",
    ),
];
const CLONE_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_clone_body_payloads_insert",
        "clone_body_payloads",
        "INSERT",
    ),
    (
        "builder_gate_clone_occurrences_insert",
        "clone_occurrences",
        "INSERT",
    ),
    (
        "builder_gate_clone_exact_postings_insert",
        "clone_exact_postings",
        "INSERT",
    ),
];
const CLONE_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 6] = [
    (
        "immutable_clone_body_payloads_update",
        "clone_body_payloads",
        "UPDATE",
        "immutable clone body payloads",
    ),
    (
        "immutable_clone_body_payloads_delete",
        "clone_body_payloads",
        "DELETE",
        "immutable clone body payloads",
    ),
    (
        "immutable_clone_occurrences_update",
        "clone_occurrences",
        "UPDATE",
        "immutable clone occurrences",
    ),
    (
        "immutable_clone_occurrences_delete",
        "clone_occurrences",
        "DELETE",
        "immutable clone occurrences",
    ),
    (
        "immutable_clone_exact_postings_update",
        "clone_exact_postings",
        "UPDATE",
        "immutable clone exact postings",
    ),
    (
        "immutable_clone_exact_postings_delete",
        "clone_exact_postings",
        "DELETE",
        "immutable clone exact postings",
    ),
];
const IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 4] = [
    (
        "immutable_source_pages_update",
        "source_pages",
        "UPDATE",
        "immutable lexical source pages",
    ),
    (
        "immutable_source_pages_delete",
        "source_pages",
        "DELETE",
        "immutable lexical source pages",
    ),
    (
        "immutable_ngram_postings_update",
        "ngram_postings",
        "UPDATE",
        "immutable lexical ngram postings",
    ),
    (
        "immutable_ngram_postings_delete",
        "ngram_postings",
        "DELETE",
        "immutable lexical ngram postings",
    ),
];
const EXACT_VOCABULARY_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 2] = [
    (
        "immutable_exact_vocabulary_update",
        "exact_vocabulary",
        "UPDATE",
        "immutable lexical exact vocabulary",
    ),
    (
        "immutable_exact_vocabulary_delete",
        "exact_vocabulary",
        "DELETE",
        "immutable lexical exact vocabulary",
    ),
];
/// Revision 14 stages row dictionary entries per page during the append
/// phase (`row_dictionary_pages`) under the same private-builder gate and
/// immutability guards as the exact vocabulary; finalization derives the
/// sealed `row_dictionary` from it and drops the staging table.
const ROW_DICTIONARY_PAGES_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_row_dictionary_pages_insert",
        "row_dictionary_pages",
        "INSERT",
    ),
    (
        "builder_gate_row_dictionary_pages_update",
        "row_dictionary_pages",
        "UPDATE",
    ),
    (
        "builder_gate_row_dictionary_pages_delete",
        "row_dictionary_pages",
        "DELETE",
    ),
];
const ROW_DICTIONARY_PAGES_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 2] = [
    (
        "immutable_row_dictionary_pages_update",
        "row_dictionary_pages",
        "UPDATE",
        "immutable lexical row dictionary pages",
    ),
    (
        "immutable_row_dictionary_pages_delete",
        "row_dictionary_pages",
        "DELETE",
        "immutable lexical row dictionary pages",
    ),
];
/// Per-field posting-length totals folded in by every committed batch and
/// sealed into `field_stats` (which drops this table) at finalization.
const FIELD_STATS_STAGING_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_field_stats_staging_insert",
        "field_stats_staging",
        "INSERT",
    ),
    (
        "builder_gate_field_stats_staging_update",
        "field_stats_staging",
        "UPDATE",
    ),
    (
        "builder_gate_field_stats_staging_delete",
        "field_stats_staging",
        "DELETE",
    ),
];

pub(in super::super) struct BuilderMutationGuardV1 {
    pub(super) gate: Arc<AtomicU8>,
}

impl BuilderMutationGuardV1 {
    pub(super) fn enter(gate: &Arc<AtomicU8>) -> Result<Self, CodeLexicalArtifactErrorV1> {
        gate.compare_exchange(
            BUILDER_MUTATION_IDLE,
            BUILDER_MUTATION_APPEND,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact builder mutation authority is already active".to_owned(),
            )
        })?;
        Ok(Self {
            gate: Arc::clone(gate),
        })
    }
}

impl Drop for BuilderMutationGuardV1 {
    fn drop(&mut self) {
        self.gate.store(BUILDER_MUTATION_IDLE, Ordering::Release);
    }
}

pub(in super::super) fn register_builder_mutation_gate(
    connection: &Connection,
) -> Result<Arc<AtomicU8>, CodeLexicalArtifactErrorV1> {
    let gate = Arc::new(AtomicU8::new(BUILDER_MUTATION_IDLE));
    let function_gate = Arc::clone(&gate);
    connection
        .create_scalar_function(
            BUILDER_MUTATION_GATE_FUNCTION,
            0,
            FunctionFlags::SQLITE_UTF8,
            move |_| {
                Ok(i64::from(
                    function_gate.load(Ordering::Acquire) == BUILDER_MUTATION_APPEND,
                ))
            },
        )
        .map_err(sqlite_error)?;
    Ok(gate)
}
pub(super) fn create_schema(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
    // Append tables (`*_runs`, `*_pages`, `field_stats_staging`) take batches
    // in page order; finalization derives each sealed serving table from its
    // staging table and drops it. Incremental auto-vacuum must be chosen
    // before the first table exists so those dropped pages leave the file.
    // `row_dictionary`, `row_chunks`, and the three posting tables stay empty
    // until then.
    connection
        .execute_batch(
            "
            PRAGMA auto_vacuum = INCREMENTAL;
            CREATE TABLE artifact_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                format_revision INTEGER NOT NULL,
                metadata BLOB NOT NULL,
                metadata_digest TEXT NOT NULL,
                receipt BLOB NOT NULL
            );
            CREATE TABLE content_epoch (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                epoch INTEGER NOT NULL CHECK(epoch >= 0)
            );
            INSERT INTO content_epoch(singleton, epoch) VALUES (1, 0);
            CREATE TABLE source_pages (
                page_ordinal INTEGER PRIMARY KEY,
                file_ordinal INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                payload_bytes INTEGER NOT NULL,
                import_count INTEGER NOT NULL,
                import_payload_bytes INTEGER NOT NULL,
                import_digest TEXT NOT NULL,
                clone_body_count INTEGER NOT NULL,
                ngram_digest TEXT NOT NULL,
                base_sections_receipt BLOB NOT NULL
            );
            CREATE TABLE source_page_cursors (
                page_ordinal INTEGER PRIMARY KEY,
                page_digest TEXT NOT NULL,
                cumulative_digest TEXT NOT NULL,
                payload_bytes INTEGER NOT NULL,
                next_cursor BLOB NOT NULL
            );
            CREATE TABLE row_blocks (
                first_document INTEGER PRIMARY KEY,
                payload BLOB NOT NULL
            );
            CREATE TABLE row_chunk_pages (
                document_id INTEGER PRIMARY KEY,
                chunk_id BLOB NOT NULL
            );
            CREATE TABLE row_chunks (
                chunk_id BLOB NOT NULL PRIMARY KEY,
                document_id INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE term_posting_runs (
                page_ordinal INTEGER NOT NULL,
                term TEXT NOT NULL,
                field INTEGER NOT NULL,
                postings BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, term, field)
            ) WITHOUT ROWID;
            CREATE TABLE term_postings (
                term TEXT NOT NULL PRIMARY KEY,
                in_fuzzy INTEGER NOT NULL CHECK(in_fuzzy IN (0, 1)),
                lists BLOB NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE field_stats (
                field INTEGER PRIMARY KEY,
                total_length INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE field_stats_staging (
                field INTEGER PRIMARY KEY,
                total_length INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE exact_vocabulary (
                term_id INTEGER PRIMARY KEY,
                term BLOB NOT NULL
            );
            CREATE TABLE exact_posting_runs (
                page_ordinal INTEGER NOT NULL,
                term_id INTEGER NOT NULL,
                field INTEGER NOT NULL,
                documents BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, term_id, field)
            ) WITHOUT ROWID;
            CREATE TABLE exact_postings (
                term_id INTEGER NOT NULL,
                field INTEGER NOT NULL,
                documents BLOB NOT NULL,
                PRIMARY KEY(term_id, field)
            ) WITHOUT ROWID;
            CREATE TABLE ngram_postings (
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                document_frequency INTEGER NOT NULL CHECK(document_frequency > 0),
                documents BLOB NOT NULL,
                PRIMARY KEY(kind, ngram)
            ) WITHOUT ROWID;
            CREATE TABLE row_dictionary (
                entry_id INTEGER PRIMARY KEY,
                entry BLOB NOT NULL,
                page_references INTEGER NOT NULL CHECK(page_references > 0)
            );
            CREATE TABLE row_dictionary_pages (
                page_ordinal INTEGER NOT NULL,
                entry_id INTEGER NOT NULL,
                entry BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, entry_id)
            ) WITHOUT ROWID;
            CREATE TRIGGER content_epoch_source_pages_insert AFTER INSERT ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_row_chunk_pages_insert AFTER INSERT ON row_chunk_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER immutable_source_pages_update BEFORE UPDATE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_source_pages_delete BEFORE DELETE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_source_page_cursors_update BEFORE UPDATE ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'immutable lexical source page cursors'); END;
            CREATE TRIGGER immutable_source_page_cursors_delete BEFORE DELETE ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'immutable lexical source page cursors'); END;
            CREATE TRIGGER immutable_ngram_postings_update BEFORE UPDATE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER immutable_ngram_postings_delete BEFORE DELETE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER immutable_exact_vocabulary_update BEFORE UPDATE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'immutable lexical exact vocabulary'); END;
            CREATE TRIGGER immutable_exact_vocabulary_delete BEFORE DELETE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'immutable lexical exact vocabulary'); END;
            CREATE TRIGGER immutable_row_dictionary_pages_update BEFORE UPDATE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical row dictionary pages'); END;
            CREATE TRIGGER immutable_row_dictionary_pages_delete BEFORE DELETE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical row dictionary pages'); END;
            CREATE TRIGGER builder_gate_source_pages_insert BEFORE INSERT ON source_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_source_page_cursors_insert BEFORE INSERT ON source_page_cursors WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_blocks_insert BEFORE INSERT ON row_blocks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_blocks_update BEFORE UPDATE ON row_blocks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_blocks_delete BEFORE DELETE ON row_blocks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunk_pages_insert BEFORE INSERT ON row_chunk_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunk_pages_update BEFORE UPDATE ON row_chunk_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunk_pages_delete BEFORE DELETE ON row_chunk_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunks_insert BEFORE INSERT ON row_chunks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunks_update BEFORE UPDATE ON row_chunks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_chunks_delete BEFORE DELETE ON row_chunks WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_posting_runs_insert BEFORE INSERT ON term_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_posting_runs_update BEFORE UPDATE ON term_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_posting_runs_delete BEFORE DELETE ON term_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_insert BEFORE INSERT ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_update BEFORE UPDATE ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_delete BEFORE DELETE ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_posting_runs_insert BEFORE INSERT ON exact_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_posting_runs_update BEFORE UPDATE ON exact_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_posting_runs_delete BEFORE DELETE ON exact_posting_runs WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_insert BEFORE INSERT ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_update BEFORE UPDATE ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_delete BEFORE DELETE ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_ngram_postings_insert BEFORE INSERT ON ngram_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_vocabulary_insert BEFORE INSERT ON exact_vocabulary WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_vocabulary_update BEFORE UPDATE ON exact_vocabulary WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_vocabulary_delete BEFORE DELETE ON exact_vocabulary WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_dictionary_pages_insert BEFORE INSERT ON row_dictionary_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_dictionary_pages_update BEFORE UPDATE ON row_dictionary_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_row_dictionary_pages_delete BEFORE DELETE ON row_dictionary_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_field_stats_staging_insert BEFORE INSERT ON field_stats_staging WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_field_stats_staging_update BEFORE UPDATE ON field_stats_staging WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_field_stats_staging_delete BEFORE DELETE ON field_stats_staging WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            ",
        )
        .map_err(sqlite_error)?;
    connection
        .execute_batch(
            "
                CREATE TABLE clone_body_payloads (
                    ordinal INTEGER PRIMARY KEY,
                    payload_digest BLOB NOT NULL UNIQUE,
                    payload BLOB NOT NULL
                );
                CREATE TABLE clone_occurrences (
                    ordinal INTEGER PRIMARY KEY,
                    symbol_key BLOB NOT NULL UNIQUE,
                    payload_ordinal INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    body_start INTEGER NOT NULL,
                    body_end INTEGER NOT NULL,
                    eligibility BLOB NOT NULL
                );
                CREATE TABLE clone_exact_postings (
                    class INTEGER NOT NULL,
                    normalization_revision INTEGER NOT NULL,
                    digest BLOB NOT NULL,
                    occurrence_ordinal INTEGER NOT NULL,
                    PRIMARY KEY(class, normalization_revision, digest, occurrence_ordinal)
                ) WITHOUT ROWID;
                CREATE TRIGGER builder_gate_clone_body_payloads_insert BEFORE INSERT ON clone_body_payloads WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER builder_gate_clone_occurrences_insert BEFORE INSERT ON clone_occurrences WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER builder_gate_clone_exact_postings_insert BEFORE INSERT ON clone_exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER immutable_clone_body_payloads_update BEFORE UPDATE ON clone_body_payloads BEGIN SELECT RAISE(ABORT, 'immutable clone body payloads'); END;
                CREATE TRIGGER immutable_clone_body_payloads_delete BEFORE DELETE ON clone_body_payloads BEGIN SELECT RAISE(ABORT, 'immutable clone body payloads'); END;
                CREATE TRIGGER immutable_clone_occurrences_update BEFORE UPDATE ON clone_occurrences BEGIN SELECT RAISE(ABORT, 'immutable clone occurrences'); END;
                CREATE TRIGGER immutable_clone_occurrences_delete BEFORE DELETE ON clone_occurrences BEGIN SELECT RAISE(ABORT, 'immutable clone occurrences'); END;
                CREATE TRIGGER immutable_clone_exact_postings_update BEFORE UPDATE ON clone_exact_postings BEGIN SELECT RAISE(ABORT, 'immutable clone exact postings'); END;
                CREATE TRIGGER immutable_clone_exact_postings_delete BEFORE DELETE ON clone_exact_postings BEGIN SELECT RAISE(ABORT, 'immutable clone exact postings'); END;
                ",
        )
        .map_err(sqlite_error)
}

pub(super) fn table_exists(
    connection: &Connection,
    table: &str,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(sqlite_error)
}

pub(super) fn verify_builder_mutation_gate_schema(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for (name, table, operation) in BUILDER_GATE_TRIGGER_LAYOUT {
        let expected = format!(
            "CREATE TRIGGER {name} BEFORE {operation} ON {table} WHEN {BUILDER_MUTATION_GATE_FUNCTION}() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END"
        );
        verify_trigger_schema(connection, name, table, &expected)?;
    }
    verify_layout_dependent_triggers(connection)
}

fn verify_layout_dependent_triggers(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    // Staging tables are verified only while present: finalization drops
    // each once its sealed table is derived.
    let has_exact_vocabulary = table_exists(connection, "exact_vocabulary")?;
    let has_row_dictionary_pages = table_exists(connection, "row_dictionary_pages")?;
    let has_field_stats_staging = table_exists(connection, "field_stats_staging")?;
    let has_row_chunk_pages = table_exists(connection, "row_chunk_pages")?;
    let has_term_posting_runs = table_exists(connection, "term_posting_runs")?;
    let has_exact_posting_runs = table_exists(connection, "exact_posting_runs")?;
    let has_clone_index = table_exists(connection, "clone_body_payloads")?;
    let has_source_page_cursors = table_exists(connection, "source_page_cursors")?;
    let gated_layouts: [(bool, &[GateTriggerLayoutV1]); 8] = [
        (
            has_source_page_cursors,
            &SOURCE_PAGE_CURSORS_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_row_chunk_pages,
            &ROW_CHUNK_PAGES_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_exact_vocabulary,
            &EXACT_VOCABULARY_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_row_dictionary_pages,
            &ROW_DICTIONARY_PAGES_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_field_stats_staging,
            &FIELD_STATS_STAGING_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_term_posting_runs,
            &TERM_POSTING_RUNS_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_exact_posting_runs,
            &EXACT_POSTING_RUNS_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (has_clone_index, &CLONE_BUILDER_GATE_TRIGGER_LAYOUT),
    ];
    let immutable_layouts: [(bool, &[ImmutableTriggerLayoutV1]); 5] = [
        (true, &IMMUTABLE_TRIGGER_LAYOUT),
        (
            has_source_page_cursors,
            &SOURCE_PAGE_CURSORS_IMMUTABLE_TRIGGER_LAYOUT,
        ),
        (
            has_exact_vocabulary,
            &EXACT_VOCABULARY_IMMUTABLE_TRIGGER_LAYOUT,
        ),
        (
            has_row_dictionary_pages,
            &ROW_DICTIONARY_PAGES_IMMUTABLE_TRIGGER_LAYOUT,
        ),
        (has_clone_index, &CLONE_IMMUTABLE_TRIGGER_LAYOUT),
    ];
    verify_trigger_layouts(
        connection,
        gated_layouts
            .iter()
            .filter(|(present, _)| *present)
            .flat_map(|(_, layout)| layout.iter().copied())
            .map(|(name, table, operation)| (name, table, operation, None))
            .chain(
                immutable_layouts
                    .iter()
                    .filter(|(present, _)| *present)
                    .flat_map(|(_, layout)| layout.iter().copied())
                    .map(|(name, table, operation, message)| {
                        (name, table, operation, Some(message))
                    }),
            ),
    )
}

fn verify_trigger_layouts<'a>(
    connection: &Connection,
    layouts: impl Iterator<Item = (&'a str, &'a str, &'a str, Option<&'a str>)>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for (name, table, operation, message) in layouts {
        let expected = match message {
            Some(message) => format!(
                "CREATE TRIGGER {name} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, '{message}'); END"
            ),
            None => format!(
                "CREATE TRIGGER {name} BEFORE {operation} ON {table} WHEN {BUILDER_MUTATION_GATE_FUNCTION}() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END"
            ),
        };
        verify_trigger_schema(connection, name, table, &expected)?;
    }
    Ok(())
}

/// `(trigger name, table, operation)` of one private-builder gate trigger.
type GateTriggerLayoutV1 = (&'static str, &'static str, &'static str);
/// `(trigger name, table, operation, abort message)` of one immutability
/// trigger.
type ImmutableTriggerLayoutV1 = (&'static str, &'static str, &'static str, &'static str);

fn verify_trigger_schema(
    connection: &Connection,
    name: &str,
    expected_table: &str,
    expected_sql: &str,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let stored: Option<(String, String)> = connection
        .query_row(
            "SELECT tbl_name, sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_corrupt)?;
    if stored
        .as_ref()
        .map(|(table, sql)| (table.as_str(), sql.as_str()))
        != Some((expected_table, expected_sql))
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact private builder trigger {name} is missing or malformed"
        )));
    }
    Ok(())
}

pub(super) fn install_base_freeze(
    transaction: &Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(
            "
            CREATE TRIGGER frozen_source_pages_insert BEFORE INSERT ON source_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical source pages'); END;
            CREATE TRIGGER frozen_source_page_cursors_insert BEFORE INSERT ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'frozen lexical source pages'); END;
            CREATE TRIGGER frozen_row_blocks_insert BEFORE INSERT ON row_blocks BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_row_blocks_update BEFORE UPDATE ON row_blocks BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_row_blocks_delete BEFORE DELETE ON row_blocks BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_row_chunk_pages_insert BEFORE INSERT ON row_chunk_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_row_chunk_pages_update BEFORE UPDATE ON row_chunk_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_row_chunk_pages_delete BEFORE DELETE ON row_chunk_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_term_posting_runs_insert BEFORE INSERT ON term_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical term posting runs'); END;
            CREATE TRIGGER frozen_term_posting_runs_update BEFORE UPDATE ON term_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical term posting runs'); END;
            CREATE TRIGGER frozen_term_posting_runs_delete BEFORE DELETE ON term_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical term posting runs'); END;
            CREATE TRIGGER frozen_exact_posting_runs_insert BEFORE INSERT ON exact_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical exact posting runs'); END;
            CREATE TRIGGER frozen_exact_posting_runs_update BEFORE UPDATE ON exact_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical exact posting runs'); END;
            CREATE TRIGGER frozen_exact_posting_runs_delete BEFORE DELETE ON exact_posting_runs BEGIN SELECT RAISE(ABORT, 'frozen lexical exact posting runs'); END;
            ",
        )
        .map_err(sqlite_error)?;
    transaction
        .execute_batch(
            "
            CREATE TRIGGER frozen_exact_vocabulary_insert BEFORE INSERT ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical exact vocabulary'); END;
            CREATE TRIGGER frozen_exact_vocabulary_update BEFORE UPDATE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical exact vocabulary'); END;
            CREATE TRIGGER frozen_exact_vocabulary_delete BEFORE DELETE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical exact vocabulary'); END;
            ",
        )
        .map_err(sqlite_error)?;
    transaction
        .execute_batch(
            "
            CREATE TRIGGER frozen_row_dictionary_pages_insert BEFORE INSERT ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary pages'); END;
            CREATE TRIGGER frozen_row_dictionary_pages_update BEFORE UPDATE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary pages'); END;
            CREATE TRIGGER frozen_row_dictionary_pages_delete BEFORE DELETE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical row dictionary pages'); END;
            ",
        )
        .map_err(sqlite_error)?;
    transaction
        .execute_batch(
            "
            CREATE TRIGGER frozen_clone_body_payloads_insert BEFORE INSERT ON clone_body_payloads BEGIN SELECT RAISE(ABORT, 'frozen clone body payloads'); END;
            CREATE TRIGGER frozen_clone_occurrences_insert BEFORE INSERT ON clone_occurrences BEGIN SELECT RAISE(ABORT, 'frozen clone occurrences'); END;
            CREATE TRIGGER frozen_clone_exact_postings_insert BEFORE INSERT ON clone_exact_postings BEGIN SELECT RAISE(ABORT, 'frozen clone exact postings'); END;
            ",
        )
        .map_err(sqlite_error)
}

pub(super) fn authenticated_authority_epoch(
    transaction: &Transaction<'_>,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<i64, CodeLexicalArtifactErrorV1> {
    verify_builder_mutation_gate_schema(transaction)?;
    let (pages, documents): (i64, i64) = transaction
        .query_row(
            "SELECT (SELECT COUNT(*) FROM source_pages), (SELECT COUNT(*) FROM row_chunk_pages)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let expected_epoch = pages.checked_add(documents).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact authority row count overflowed".to_owned(),
        )
    })?;
    let actual_epoch = content_epoch(transaction)?;
    let admitted = admitted_documents(transaction)?;
    if actual_epoch != expected_epoch
        || u64::try_from(pages).ok() != Some(source.page_count())
        || u64::try_from(documents).ok() != Some(admitted)
        || admitted > source.total_chunks()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact authenticated authority disagrees with its source receipt".to_owned(),
        ));
    }
    verify_clone_rows(transaction, source)?;
    Ok(actual_epoch)
}

/// Documents the staged pages admitted, as their base-section receipts
/// count them: every source chunk except those the projection does not
/// index (annotation uses).
pub(super) fn admitted_documents(
    connection: &Connection,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let rows_section = BASE_SECTION_NAMES
        .iter()
        .position(|name| *name == "rows")
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract("lexical rows section is unnamed".to_owned())
        })?;
    let mut statement = connection
        .prepare(
            "SELECT page_ordinal, base_sections_receipt FROM source_pages ORDER BY page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut pages = statement.query([]).map_err(sqlite_error)?;
    let mut admitted = 0u64;
    while let Some(page) = pages.next().map_err(sqlite_error)? {
        let page_ordinal =
            u64::try_from(page.get::<_, i64>(0).map_err(sqlite_error)?).map_err(contract_number)?;
        let receipt: Vec<u8> = page.get(1).map_err(sqlite_error)?;
        let receipt = decode_page_base_sections_receipt(page_ordinal, &receipt)?;
        admitted = admitted
            .checked_add(receipt.sections()[rows_section].row_count)
            .ok_or_else(source_chain_overflow)?;
    }
    Ok(admitted)
}
