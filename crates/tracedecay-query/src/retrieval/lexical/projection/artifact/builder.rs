use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use rayon::prelude::*;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{ToSqlOutput, Value, ValueRef};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    CodeIndexImportEvidenceV1, ExtractionAdmittedCodeSearchChunkV1,
};
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1, VerifiedSealedLexicalSymbolDisplayV1,
};
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkV1, ExactTechnicalTermV1,
    FileOccurrenceId, ManifestDigest,
};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_parent_directory};
use tracedecay_private_fs::{create_private_file_retained, open_private_file};

use super::format::{
    BASE_SECTION_NAMES, CodeLexicalArtifactSectionDigestV1, PostingListDecoderV1,
    PostingListEncoderV1, RECEIPT_RESERVATION_BYTES, SECTION_NAMES, SERVING_INDEX_STEP_COUNT_V11,
    STATISTICS_STEP_COUNT_V11, VerifiedCodeLexicalArtifactV1, absorb_page_base_sections_receipt,
    content_metadata_bytes, contract_number, decode_fingerprint_postings, decode_ngram_bitmap,
    decode_padded_receipt, decode_padded_receipt_with_control, decode_page_base_sections_receipt,
    encode_document_set, encode_fingerprint_postings, encode_term_lists,
    finish_base_section_receipt_fold, hash_bytes, initial_base_section_receipt_fold,
    metadata_digest, new_verified_receipt, padded_receipt, receipt_artifact_digest,
    stored_metadata_digest, verify_artifact_table_layout,
};
use super::postings::document_ngram_scratch;
use super::prepared::document_ngram_keys;
use super::prepared::{
    PreparedCloneBodyV1, PreparedCodeLexicalArtifactPageV1, PreparedTermPostingV1,
    prepare_page as prepare_page_values,
};
use super::clone_codec::{digest_from_key, digest_key};
use super::row_codec::{
    ConnectionRowDictionaryV1, decode_artifact_row, decode_row_block, stored_chunk_key,
    stored_symbol_key,
};
use super::schema::{
    CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, derive_row_dictionary, exact_field_code_from_encoded,
    field_code, field_code_from_encoded, intern_exact_terms, require_served_revision,
    stable_exact_term_id, stage_row_dictionary,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1, CodeLexicalArtifactBatchLimitV1,
    CodeLexicalArtifactErrorV1, NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1, checkpoint,
    open_builder_connection, sqlite_corrupt, sqlite_error,
};
use crate::retrieval::lexical::LexicalFieldV1;

use super::super::{CodeLexicalProjectionMetadataV1, normalized_search_text};

const SQLITE_HEADER_COMMIT_COUNTER_OFFSETS: [u64; 2] = [24, 92];
const SEALED_COMMIT_COUNTER: u32 = 1;
const PROGRESS_TAIL_QUERY: &str = "SELECT page_ordinal, next_cursor \
     FROM source_page_cursors ORDER BY page_ordinal DESC LIMIT 1";
const FINALIZATION_PROGRESS_INTERVAL_OPS: i32 = 4_096;
const FINALIZATION_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(1);
// A plan entry owns its sort key (borrowed term text, integer field code,
// document id) and one posting reference. Five words match the 64-bit
// layout and cover the 32-bit one; general allocator metadata remains
// outside the ledger contract.
const TERM_INSERT_PLAN_BYTES_PER_REF: usize = 5 * std::mem::size_of::<usize>();
const TERM_INSERT_CONTROL_INTERVAL: usize = 4_096;
// An exact-posting plan entry owns its document, field, and term identifiers.
// Eight words conservatively over-reserves that entry on both the 32-bit and
// 64-bit layouts; general allocator metadata remains outside the ledger
// contract.
const EXACT_INSERT_PLAN_BYTES_PER_REF: usize = 8 * std::mem::size_of::<usize>();
const EXACT_INSERT_CONTROL_INTERVAL: usize = TERM_INSERT_CONTROL_INTERVAL;
/// Rows per multi-row `INSERT ... VALUES (...), (...)` statement in the
/// append phase. The per-row cost of the base-table inserts is statement
/// overhead plus the per-row builder-gate trigger, not I/O: on a
/// one-row-per-posting shape at production pragmas (120k sorted rows in one
/// transaction) one row per statement costs 1.47 µs/row and 32 rows per
/// statement 0.86 µs/row, the trigger still firing for every row. Five
/// columns × 32 rows stays under SQLite's 999-parameter floor.
const INSERT_ROWS_PER_STATEMENT: usize = 32;
// This gate serializes mutation within the private-profile/stable-handle
// authority. It denies ordinary second-connection DML, but is not a
// cryptographic defense against malicious same-UID code that deliberately
// registers a lookalike SQLite function.
const BUILDER_MUTATION_GATE_FUNCTION: &str = "tracedecay_lexical_builder_append_authorized";
const BUILDER_MUTATION_IDLE: u8 = 0;
const BUILDER_MUTATION_APPEND: u8 = 1;

/// One planned posting, keyed `(term, field, document_id)` so the merged
/// stream yields each `(term, field)` run of the batch contiguously and in
/// document order, ready to encode as one `term_posting_runs` list.
#[derive(Clone, Copy)]
struct PreparedTermInsertRefV1<'a> {
    key: (&'a str, i64, i64),
    posting: &'a PreparedTermPostingV1,
}

impl<'a> PreparedTermInsertRefV1<'a> {
    fn new(document_id: i64, field: i64, posting: &'a PreparedTermPostingV1) -> Self {
        Self {
            key: (posting.term.as_str(), field, document_id),
            posting,
        }
    }

    fn key(&self) -> (&'a str, i64, i64) {
        self.key
    }
}

/// Every planned term posting of one batch, sorted by key. Keys are unique
/// (one posting per document, field, and term), so the order is total.
struct PreparedTermInsertPlanV1<'a> {
    entries: Vec<PreparedTermInsertRefV1<'a>>,
}

// Exact postings sort the same way as term postings, keyed
// `(term_id, field, document_id)` so each `exact_posting_runs` list is
// contiguous in the sorted stream.
#[derive(Clone, Copy)]
struct PreparedExactInsertRefV1 {
    document_id: i64,
    field_code: i64,
    term_id: i64,
}

impl PreparedExactInsertRefV1 {
    fn key(&self) -> (i64, i64, i64) {
        (self.term_id, self.field_code, self.document_id)
    }
}

struct PreparedExactInsertPlanV1<'a> {
    entries: Vec<PreparedExactInsertRefV1>,
    /// The batch's distinct exact terms with the ids this plan
    /// content-addressed, ascending by id, the order `exact_vocabulary` was
    /// always interned in.
    interned_terms: Vec<(&'a [u8], i64)>,
}

const BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 15] = [
    ("builder_gate_source_pages_insert", "source_pages", "INSERT"),
    (
        "builder_gate_import_evidence_insert",
        "import_evidence",
        "INSERT",
    ),
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
const CLONE_FINGERPRINT_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 1] = [(
    "builder_gate_clone_fingerprint_postings_insert",
    "clone_fingerprint_postings",
    "INSERT",
)];
/// Revision 16 stages fingerprint postings per batch in arrival order
/// (`clone_fingerprint_postings_pages`) under the same private-builder gate
/// and immutability guards as the row dictionary pages; finalization sorts
/// them once into the keyed `clone_fingerprint_postings` tree and drops the
/// staging table.
const CLONE_FINGERPRINT_PAGES_BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 3] = [
    (
        "builder_gate_clone_fingerprint_postings_pages_insert",
        "clone_fingerprint_postings_pages",
        "INSERT",
    ),
    (
        "builder_gate_clone_fingerprint_postings_pages_update",
        "clone_fingerprint_postings_pages",
        "UPDATE",
    ),
    (
        "builder_gate_clone_fingerprint_postings_pages_delete",
        "clone_fingerprint_postings_pages",
        "DELETE",
    ),
];
const CLONE_FINGERPRINT_PAGES_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 2] = [
    (
        "immutable_clone_fingerprint_postings_pages_update",
        "clone_fingerprint_postings_pages",
        "UPDATE",
        "immutable clone fingerprint posting pages",
    ),
    (
        "immutable_clone_fingerprint_postings_pages_delete",
        "clone_fingerprint_postings_pages",
        "DELETE",
        "immutable clone fingerprint posting pages",
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
const CLONE_FINGERPRINT_IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 2] = [
    (
        "immutable_clone_fingerprint_postings_update",
        "clone_fingerprint_postings",
        "UPDATE",
        "immutable clone fingerprint postings",
    ),
    (
        "immutable_clone_fingerprint_postings_delete",
        "clone_fingerprint_postings",
        "DELETE",
        "immutable clone fingerprint postings",
    ),
];
const IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 6] = [
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
        "immutable_import_evidence_update",
        "import_evidence",
        "UPDATE",
        "immutable lexical import evidence",
    ),
    (
        "immutable_import_evidence_delete",
        "import_evidence",
        "DELETE",
        "immutable lexical import evidence",
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

pub(super) struct BuilderMutationGuardV1 {
    gate: Arc<AtomicU8>,
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

pub(super) fn register_builder_mutation_gate(
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

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_FINALIZATION_MONITOR_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_finalization_monitor_spawn() {
    FAIL_NEXT_FINALIZATION_MONITOR_SPAWN.with(|failure| failure.set(true));
}

// Stands in for a physical restart between the staging schema and its
// singleton `artifact_state` row.
#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_STAGING_INITIALIZATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_staging_initialization() {
    FAIL_NEXT_STAGING_INITIALIZATION.with(|failure| failure.set(true));
}

#[cfg(test)]
fn take_failed_staging_initialization() -> bool {
    FAIL_NEXT_STAGING_INITIALIZATION.with(|failure| failure.replace(false))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinalizationSectionV1 {
    SourcePages,
    DocumentIntegrity,
    ImportIntegrity,
    ImportEvidence,
    Rows,
    TermPostings,
    ExactPostings,
    NgramPostings,
    FieldStatistics,
    Vocabulary,
    CloneOccurrences,
    CloneExactPostings,
    CloneBodyPayloads,
    CloneFingerprintPostings,
}

impl FinalizationSectionV1 {
    const ALL: [Self; 14] = [
        Self::SourcePages,
        Self::DocumentIntegrity,
        Self::ImportIntegrity,
        Self::ImportEvidence,
        Self::Rows,
        Self::TermPostings,
        Self::ExactPostings,
        Self::NgramPostings,
        Self::FieldStatistics,
        Self::Vocabulary,
        Self::CloneOccurrences,
        Self::CloneExactPostings,
        Self::CloneBodyPayloads,
        Self::CloneFingerprintPostings,
    ];

    fn from_ordinal(ordinal: usize) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::ALL.get(ordinal).copied().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization selected an unknown section".to_owned(),
            )
        })
    }

    #[hotpath::skip]
    const fn name(self) -> &'static str {
        match self {
            Self::SourcePages => "source_pages",
            Self::DocumentIntegrity => "document_integrity",
            Self::ImportIntegrity => "import_integrity",
            Self::ImportEvidence => "import_evidence",
            Self::Rows => "rows",
            Self::TermPostings => "term_postings",
            Self::ExactPostings => "exact_postings",
            Self::NgramPostings => "ngram_postings",
            Self::CloneOccurrences => "clone_occurrences",
            Self::CloneExactPostings => "clone_exact_postings",
            Self::CloneBodyPayloads => "clone_body_payloads",
            Self::CloneFingerprintPostings => "clone_fingerprint_postings",
            Self::FieldStatistics => "field_stats",
            Self::Vocabulary => "vocabulary",
        }
    }

    /// Native digest query of every section the digest phase walks. Base
    /// sections carry none: their digests are adopted from page receipts.
    #[hotpath::skip]
    const fn full_query(self) -> Option<&'static str> {
        match self {
            Self::SourcePages => Some(
                "SELECT page_ordinal, chunk_count, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt FROM source_pages ORDER BY page_ordinal",
            ),
            Self::CloneOccurrences => Some(
                "SELECT ordinal, symbol_key, payload_ordinal, path, body_start, body_end, eligibility FROM clone_occurrences ORDER BY ordinal",
            ),
            Self::CloneExactPostings => Some(
                "SELECT class, normalization_revision, digest, occurrence_ordinal FROM clone_exact_postings ORDER BY class, normalization_revision, digest, occurrence_ordinal",
            ),
            Self::CloneBodyPayloads => Some(
                "SELECT ordinal, payload_digest, payload FROM clone_body_payloads ORDER BY ordinal",
            ),
            Self::CloneFingerprintPostings => Some(
                "SELECT language, class, normalization_revision, fingerprint, posting_count, postings FROM clone_fingerprint_postings ORDER BY language, class, normalization_revision, fingerprint",
            ),
            Self::FieldStatistics => {
                Some("SELECT field, total_length FROM field_stats ORDER BY field")
            }
            // The vocabulary is every sealed term with its fuzzy flag; its
            // posting lists belong to the adopted `term_postings` section.
            Self::Vocabulary => Some("SELECT term, in_fuzzy FROM term_postings ORDER BY term"),
            Self::DocumentIntegrity
            | Self::ImportIntegrity
            | Self::ImportEvidence
            | Self::Rows
            | Self::TermPostings
            | Self::ExactPostings
            | Self::NgramPostings => None,
        }
    }

    /// Bounded resumes seek a native table key, never a computed cursor.
    #[hotpath::skip]
    const fn seek_query(self, after: bool) -> Option<&'static str> {
        match (self, after) {
            (Self::SourcePages, false) => Some(
                "SELECT page_ordinal, chunk_count, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt FROM source_pages ORDER BY page_ordinal LIMIT ?1",
            ),
            (Self::SourcePages, true) => Some(
                "SELECT page_ordinal, chunk_count, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt FROM source_pages WHERE page_ordinal > ?1 ORDER BY page_ordinal LIMIT ?2",
            ),
            (Self::FieldStatistics, false) => {
                Some("SELECT field, total_length FROM field_stats ORDER BY field LIMIT ?1")
            }
            (Self::FieldStatistics, true) => Some(
                "SELECT field, total_length FROM field_stats WHERE field > ?1 ORDER BY field LIMIT ?2",
            ),
            (Self::Vocabulary, false) => {
                Some("SELECT term, in_fuzzy FROM term_postings ORDER BY term LIMIT ?1")
            }
            (Self::Vocabulary, true) => Some(
                "SELECT term, in_fuzzy FROM term_postings WHERE term > ?1 ORDER BY term LIMIT ?2",
            ),
            (Self::CloneOccurrences, false) => Some(
                "SELECT ordinal, symbol_key, payload_ordinal, path, body_start, body_end, eligibility FROM clone_occurrences ORDER BY ordinal LIMIT ?1",
            ),
            (Self::CloneOccurrences, true) => Some(
                "SELECT ordinal, symbol_key, payload_ordinal, path, body_start, body_end, eligibility FROM clone_occurrences WHERE ordinal > ?1 ORDER BY ordinal LIMIT ?2",
            ),
            (Self::CloneExactPostings, false) => Some(
                "SELECT class, normalization_revision, digest, occurrence_ordinal FROM clone_exact_postings ORDER BY class, normalization_revision, digest, occurrence_ordinal LIMIT ?1",
            ),
            (Self::CloneExactPostings, true) => Some(
                "SELECT class, normalization_revision, digest, occurrence_ordinal FROM clone_exact_postings WHERE (class, normalization_revision, digest, occurrence_ordinal) > (?1, ?2, ?3, ?4) ORDER BY class, normalization_revision, digest, occurrence_ordinal LIMIT ?5",
            ),
            (Self::CloneBodyPayloads, false) => Some(
                "SELECT ordinal, payload_digest, payload FROM clone_body_payloads ORDER BY ordinal LIMIT ?1",
            ),
            (Self::CloneBodyPayloads, true) => Some(
                "SELECT ordinal, payload_digest, payload FROM clone_body_payloads WHERE ordinal > ?1 ORDER BY ordinal LIMIT ?2",
            ),
            (Self::CloneFingerprintPostings, false) => Some(
                "SELECT language, class, normalization_revision, fingerprint, posting_count, postings FROM clone_fingerprint_postings ORDER BY language, class, normalization_revision, fingerprint LIMIT ?1",
            ),
            (Self::CloneFingerprintPostings, true) => Some(
                "SELECT language, class, normalization_revision, fingerprint, posting_count, postings FROM clone_fingerprint_postings WHERE (language, class, normalization_revision, fingerprint) > (?1, ?2, ?3, ?4) ORDER BY language, class, normalization_revision, fingerprint LIMIT ?5",
            ),
            (
                Self::DocumentIntegrity
                | Self::ImportIntegrity
                | Self::ImportEvidence
                | Self::Rows
                | Self::TermPostings
                | Self::ExactPostings
                | Self::NgramPostings,
                _,
            ) => None,
        }
    }

    fn walked_query(self, after: bool) -> Result<&'static str, CodeLexicalArtifactErrorV1> {
        self.seek_query(after).ok_or_else(base_section_walk_error)
    }
}

fn base_section_walk_error() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(
        "lexical artifact base sections are adopted from page receipts, never walked".to_owned(),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
enum PersistedFinalizationKeyV1 {
    Integer(i64),
    Text(String),
    ClonePosting {
        class: i64,
        normalization_revision: i64,
        digest: Vec<u8>,
        occurrence_ordinal: i64,
    },
    Fingerprint {
        language: String,
        class: i64,
        normalization_revision: i64,
        fingerprint: i64,
    },
}

impl PersistedFinalizationKeyV1 {
    fn matches_section(&self, section: FinalizationSectionV1) -> bool {
        matches!(
            (self, section),
            (
                Self::Integer(_),
                FinalizationSectionV1::SourcePages
                    | FinalizationSectionV1::FieldStatistics
                    | FinalizationSectionV1::CloneOccurrences
                    | FinalizationSectionV1::CloneBodyPayloads
            ) | (Self::Text(_), FinalizationSectionV1::Vocabulary)
                | (
                Self::ClonePosting { .. },
                FinalizationSectionV1::CloneExactPostings
            ) | (
                Self::Fingerprint { .. },
                FinalizationSectionV1::CloneFingerprintPostings
            )
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLexicalArtifactBuildProgressV1 {
    pub next_page_ordinal: u64,
    pub completed_chunks: u64,
    pub completed_payload_bytes: u64,
    pub completed_imports: u64,
    pub completed_import_payload_bytes: u64,
    pub import_dictionary_digest: Option<ManifestDigest>,
    pub cumulative_source_digest: Option<ManifestDigest>,
    pub next_cursor: Option<VerifiedSealedLexicalCursorV1>,
}

/// One bounded step while sealing an already-staged lexical artifact.
///
/// `Pending` persists its section and row cursor in the staging database, so
/// callers can yield, restart the process, and continue without reopening the
/// sealed source or replaying its pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeLexicalArtifactFinalizationPhaseV1 {
    IndexBuild,
    Verification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeLexicalArtifactFinalizationStepV1 {
    Pending {
        phase: CodeLexicalArtifactFinalizationPhaseV1,
        completed_sections: u64,
        completed_rows: u64,
    },
    Ready(Box<VerifiedCodeLexicalArtifactV1>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFinalizationStateV1 {
    phase: PersistedFinalizationPhaseV1,
    section_ordinal: u64,
    section_row_count: u64,
    section_last_key: Option<PersistedFinalizationKeyV1>,
    section_accumulator: Vec<u8>,
    base_section_row_counts: Vec<u64>,
    base_section_accumulators: Vec<Vec<u8>>,
    completed_sections: Vec<CodeLexicalArtifactSectionDigestV1>,
    completed_rows: u64,
    content_epoch: i64,
    source_state_digest: ManifestDigest,
    /// The accepted source's terminal cursor. Statistics drops the staged
    /// per-page cursors before the seal, and a resumed finalization restores
    /// its source to this cursor to re-mint the completion receipt.
    terminal_cursor: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedFinalizationPhaseV1 {
    Statistics,
    Indexes,
    Digest,
}

impl PersistedFinalizationPhaseV1 {
    #[hotpath::skip]
    const fn public(self) -> CodeLexicalArtifactFinalizationPhaseV1 {
        match self {
            Self::Statistics | Self::Indexes => CodeLexicalArtifactFinalizationPhaseV1::IndexBuild,
            Self::Digest => CodeLexicalArtifactFinalizationPhaseV1::Verification,
        }
    }
}

struct FinalizationWakeMetricsV1 {
    #[cfg(feature = "hotpath")]
    rows: u64,
}

struct FinalizationTransactionMetricsV1 {
    #[cfg(feature = "hotpath")]
    committed: bool,
}

impl FinalizationTransactionMetricsV1 {
    #[inline(always)]
    #[hotpath::skip]
    const fn new() -> Self {
        Self {
            #[cfg(feature = "hotpath")]
            committed: false,
        }
    }

    #[inline(always)]
    fn mark_committed(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            self.committed = true;
        }
    }
}

impl Drop for FinalizationTransactionMetricsV1 {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        if !self.committed {
            // Dropping an uncommitted rusqlite transaction rolls it back.
            hotpath::gauge!("query.artifact.finalization.rollback_total").inc(1u64);
        }
    }
}

impl FinalizationWakeMetricsV1 {
    #[inline(always)]
    fn new() -> Self {
        Self {
            #[cfg(feature = "hotpath")]
            rows: 0,
        }
    }

    #[inline(always)]
    fn digest_pass(&self, pass: PersistedFinalizationPhaseV1) {
        #[cfg(feature = "hotpath")]
        match pass {
            PersistedFinalizationPhaseV1::Statistics => {
                hotpath::gauge!("query.artifact.finalization.statistics_wakes_total").inc(1u64);
            }
            PersistedFinalizationPhaseV1::Indexes => {
                hotpath::gauge!("query.artifact.finalization.index_wakes_total").inc(1u64);
            }
            PersistedFinalizationPhaseV1::Digest => {
                hotpath::gauge!("query.artifact.finalization.digest_pass.authenticated_total")
                    .inc(1u64);
            }
        };
        #[cfg(not(feature = "hotpath"))]
        let _ = pass;
    }

    #[inline(always)]
    fn phase(&self, phase: FinalizationSectionV1) {
        #[cfg(feature = "hotpath")]
        match phase {
            FinalizationSectionV1::SourcePages => {
                hotpath::gauge!("query.artifact.finalization.phase.source_pages_total").inc(1u64);
            }
            FinalizationSectionV1::DocumentIntegrity => {
                hotpath::gauge!("query.artifact.finalization.phase.document_integrity_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::ImportIntegrity => {
                hotpath::gauge!("query.artifact.finalization.phase.import_integrity_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::ImportEvidence => {
                hotpath::gauge!("query.artifact.finalization.phase.import_evidence_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::Rows => {
                hotpath::gauge!("query.artifact.finalization.phase.rows_total").inc(1u64);
            }
            FinalizationSectionV1::TermPostings => {
                hotpath::gauge!("query.artifact.finalization.phase.term_postings_total").inc(1u64);
            }
            FinalizationSectionV1::ExactPostings => {
                hotpath::gauge!("query.artifact.finalization.phase.exact_postings_total").inc(1u64);
            }
            FinalizationSectionV1::NgramPostings => {
                hotpath::gauge!("query.artifact.finalization.phase.ngram_postings_total").inc(1u64);
            }
            FinalizationSectionV1::CloneOccurrences => {
                hotpath::gauge!("query.artifact.finalization.phase.clone_occurrences_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::CloneExactPostings => {
                hotpath::gauge!("query.artifact.finalization.phase.clone_exact_postings_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::CloneBodyPayloads => {
                hotpath::gauge!("query.artifact.finalization.phase.clone_body_payloads_total")
                    .inc(1u64);
            }
            FinalizationSectionV1::CloneFingerprintPostings => {
                hotpath::gauge!(
                    "query.artifact.finalization.phase.clone_fingerprint_postings_total"
                )
                .inc(1u64);
            }
            FinalizationSectionV1::FieldStatistics => {
                hotpath::gauge!("query.artifact.finalization.phase.field_stats_total").inc(1u64);
            }
            FinalizationSectionV1::Vocabulary => {
                hotpath::gauge!("query.artifact.finalization.phase.vocabulary_total").inc(1u64);
            }
        };
        #[cfg(not(feature = "hotpath"))]
        let _ = phase;
    }

    #[inline(always)]
    fn probe(&self) {
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.finalization.section_probes_total").inc(1u64);
    }

    #[inline(always)]
    fn add_rows(&mut self, rows: usize) -> Result<(), CodeLexicalArtifactErrorV1> {
        #[cfg(feature = "hotpath")]
        {
            self.rows = self
                .rows
                .checked_add(u64::try_from(rows).map_err(contract_number)?)
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact finalization wake row metric overflowed".to_owned(),
                    )
                })?;
        }
        #[cfg(not(feature = "hotpath"))]
        let _ = rows;
        Ok(())
    }
}

impl Drop for FinalizationWakeMetricsV1 {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("query.artifact.finalization.wakes_total").inc(1u64);
            hotpath::gauge!("query.artifact.finalization.rows_total").inc(self.rows);
        }
    }
}

/// Stable identity of the private staging authority, captured from an exact
/// no-follow file handle rather than mutable path metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StableArtifactFileIdentityV1 {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
}

pub struct CodeLexicalArtifactBuilderV1 {
    path: PathBuf,
    /// Keeps the exact no-follow/private file handle alive while the SQLite
    /// connection is in use. Every public transition rebinds the pathname to
    /// this identity before it trusts the connection's contents.
    private_file: File,
    file_identity: StableArtifactFileIdentityV1,
    connection: Connection,
    mutation_gate: Arc<AtomicU8>,
    metadata: CodeLexicalProjectionMetadataV1,
    metadata_digest: ManifestDigest,
    memory_budget_bytes: usize,
    fixed_ledger_charge_bytes: usize,
}

/// One source-prefix decision whose fresh relational values were prepared
/// exactly once. Replayed pages contribute to `accepted_prefix` but do not
/// appear in `prepared_pages` because they require no SQLite mutation.
pub struct PreparedCodeLexicalArtifactBatchV1 {
    accepted_prefix: NonZeroUsize,
    prepared_pages: Vec<PreparedCodeLexicalArtifactPageV1>,
}

impl PreparedCodeLexicalArtifactBatchV1 {
    pub fn accepted_prefix(&self) -> NonZeroUsize {
        self.accepted_prefix
    }

    pub fn prepared_pages(&self) -> &[PreparedCodeLexicalArtifactPageV1] {
        &self.prepared_pages
    }
}

impl CodeLexicalArtifactBuilderV1 {
    pub fn create(
        path: impl AsRef<Path>,
        metadata: CodeLexicalProjectionMetadataV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::create_with_memory_budget(
            path,
            metadata,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
    }

    #[hotpath::measure(label = "query.artifact.create")]
    pub fn create_with_memory_budget(
        path: impl AsRef<Path>,
        metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        metadata
            .validate()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let fixed_ledger_charge_bytes =
            validated_fixed_ledger_charge(&metadata, memory_budget_bytes)?;
        let path = path.as_ref();
        if path.try_exists().map_err(private_staging_error)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path already contains state".to_owned(),
            ));
        }
        let metadata_digest = metadata_digest(&metadata)?;
        publish_initialized_staging(path, &metadata, &metadata_digest, memory_budget_bytes)?;
        let (connection, private_file, file_identity) =
            open_private_builder_connection(path, memory_budget_bytes)?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        verify_builder_mutation_gate_schema(&connection)?;
        crate::hotpath_metrics::Residency::Cold.record("query.artifact.residency");
        Ok(Self {
            path: path.to_path_buf(),
            private_file,
            file_identity,
            connection,
            mutation_gate,
            metadata,
            metadata_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    /// Reopen only the staged artifact authority while applying the caller's
    /// scheduler epoch/deadline control to integrity, metadata, receipt, and
    /// contiguous-cursor verification.
    #[hotpath::measure(label = "query.artifact.open_or_resume")]
    pub fn open_or_resume_with_memory_budget_and_control(
        path: impl AsRef<Path>,
        expected_metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        expected_metadata
            .validate()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let fixed_ledger_charge_bytes =
            validated_fixed_ledger_charge(&expected_metadata, memory_budget_bytes)?;
        let path = path.as_ref();
        let (connection, private_file, file_identity) = hotpath::measure_block!(
            "query.artifact.open.sqlite_connect",
            open_private_builder_connection(path, memory_budget_bytes)
        )?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        require_staged_revision(&connection)?;
        hotpath::measure_block!("query.artifact.open.schema_verify", {
            require_integrity(&connection, control)?;
            verify_artifact_table_layout(&connection)?;
            verify_builder_mutation_gate_schema(&connection)
        })?;
        let expected_digest = hotpath::measure_block!("query.artifact.open.metadata_restore", {
            let expected_digest = metadata_digest(&expected_metadata)?;
            verify_artifact_state_metadata(
                &connection,
                &expected_metadata,
                &expected_digest,
                control,
            )?;
            Ok::<_, CodeLexicalArtifactErrorV1>(expected_digest)
        })?;
        let (receipt, finalization) = hotpath::measure_block!(
            "query.artifact.open.receipt_restore",
            Ok::<_, CodeLexicalArtifactErrorV1>((
                read_receipt_with_control(&connection, control)?,
                load_finalization_state(&connection)?,
            ))
        )?;
        // A staging file still accepting pages must carry the per-batch field
        // totals; one staged without them (an older builder) cannot seal
        // correct statistics and is not resumed.
        if receipt.is_none()
            && finalization.is_none()
            && !table_exists(&connection, "field_stats_staging")?
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "lexical artifact was staged without incremental field statistics".to_owned(),
            ));
        }
        // Likewise, a staging file still accepting pages must carry the
        // fingerprint staging table; one without it is refused as
        // incompatible so the scheduler discards and restages it instead of
        // retrying an `Io`.
        if receipt.is_none()
            && finalization.is_none()
            && !table_exists(&connection, "clone_fingerprint_postings_pages")?
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "lexical artifact was staged without fingerprint posting pages".to_owned(),
            ));
        }
        validate_contiguous_pages(&connection, control)?;
        checkpoint(control)?;
        crate::hotpath_metrics::Residency::Rebuilding.record("query.artifact.residency");
        Ok(Self {
            path: path.to_path_buf(),
            private_file,
            file_identity,
            connection,
            mutation_gate,
            metadata: expected_metadata,
            metadata_digest: expected_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    #[hotpath::skip]
    pub fn progress(
        &self,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        progress(&self.connection)
    }

    /// The receipt of a staging file that finished finalization and awaits
    /// publication. A sealed file keeps no source cursor, so a resumed
    /// publisher publishes it rather than rescanning its source.
    #[hotpath::skip]
    pub fn sealed_receipt(
        &self,
    ) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        read_receipt(&self.connection)
    }

    /// The ledger bytes charged regardless of page content: the SQLite
    /// page-cache authority plus the builder-retained projection metadata.
    #[hotpath::skip]
    pub fn fixed_ledger_charge_bytes(&self) -> usize {
        self.fixed_ledger_charge_bytes
    }

    /// The deterministic ledger charge admitting `page` would add on top of
    /// the fixed charge: the page's retained owned bytes plus the
    /// summed per-record preparation upper bound (projected rows, postings,
    /// serialization, and n-gram scratch), without allocating during
    /// admission.
    pub fn page_ledger_charge_bytes(
        &self,
        page: &VerifiedSealedLexicalPageV1,
    ) -> Result<usize, CodeLexicalArtifactErrorV1> {
        let transient = page_preparation_upper_bound_bytes(&self.metadata, page)?;
        page.retained_owned_bytes()
            .checked_add(transient)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact page ledger charge overflowed".to_owned(),
                )
            })
    }

    /// Conservative pre-preparation charge for retaining `pages` and every
    /// page's derived output/scratch upper bound. The exact post-preparation
    /// charge is carried by [`PreparedCodeLexicalArtifactPageV1`].
    pub fn page_batch_ledger_charge_bytes(
        &self,
        pages: &[VerifiedSealedLexicalPageV1],
    ) -> Result<usize, CodeLexicalArtifactErrorV1> {
        page_batch_ledger_charge_bytes(&self.metadata, pages)
    }

    /// Return the largest contiguous input prefix whose complete retained,
    /// prepared-output, and active-worker scratch claims fit the memory
    /// authority. Exact prepared-row and SQLite-write prefix selection occurs
    /// after this bound in [`Self::prepare_admissible_page_prefix`]. Zero is
    /// truthful when even the first page cannot be prepared within memory.
    pub fn largest_admissible_page_prefix(
        &self,
        pages: &[VerifiedSealedLexicalPageV1],
    ) -> Result<usize, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        let worker_limit = tracedecay_code_index::parallelism::indexing_workers();
        let mut retained = 0usize;
        let mut prepared = 0usize;
        let mut active_scratch = 0usize;
        let mut largest_scratch = BinaryHeap::<Reverse<usize>>::new();
        for (index, page) in pages.iter().enumerate() {
            if page.retained_owned_bytes() > CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1 {
                return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                    "sealed lexical page retained bytes exceed the {}-byte artifact input bound",
                    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
                )));
            }
            retained = retained
                .checked_add(page.retained_owned_bytes())
                .ok_or_else(batch_ledger_overflow)?;
            prepared = prepared
                .checked_add(page_prepared_retained_upper_bound_bytes(
                    &self.metadata,
                    page,
                )?)
                .ok_or_else(batch_ledger_overflow)?;
            let scratch = page_transient_peak_bytes(&self.metadata, page, usize::MAX)?;
            if largest_scratch.len() < worker_limit {
                largest_scratch.push(Reverse(scratch));
                active_scratch = active_scratch
                    .checked_add(scratch)
                    .ok_or_else(batch_ledger_overflow)?;
            } else if let Some(Reverse(smallest)) = largest_scratch.peek().copied()
                && scratch > smallest
            {
                largest_scratch.pop();
                largest_scratch.push(Reverse(scratch));
                active_scratch = active_scratch
                    .checked_sub(smallest)
                    .and_then(|bytes| bytes.checked_add(scratch))
                    .ok_or_else(batch_ledger_overflow)?;
            }
            let required = self
                .fixed_ledger_charge_bytes
                .checked_add(retained)
                .and_then(|bytes| bytes.checked_add(prepared))
                .and_then(|bytes| bytes.checked_add(active_scratch))
                .ok_or_else(batch_ledger_overflow)?;
            if required > self.memory_budget_bytes {
                return Ok(index);
            }
        }
        Ok(pages.len())
    }

    /// Append one page through the canonical atomic batch path.
    pub fn append_page(
        &mut self,
        page: &VerifiedSealedLexicalPageV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        self.append_pages(std::slice::from_ref(page), control)
    }

    /// Atomically append an ordered, contiguous batch of verified source
    /// pages. Replayed prefix pages are verified idempotently; every fresh
    /// page and its derived rows commit in one SQLite transaction.
    #[hotpath::measure(label = "query.artifact.append_pages")]
    pub fn append_pages(
        &mut self,
        pages: &[VerifiedSealedLexicalPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        let result = (|| {
            let prepared = self.prepare_pages(pages, control)?;
            self.append_prepared_pages_inner(&prepared, control)
        })();
        record_batch_outcome(&result);
        result
    }

    /// Prepare the fresh suffix of one ordered source batch outside SQLite.
    /// Work runs on the canonical bounded indexing pool, preserves input
    /// order, holds one background CPU permit per active unit, and drains all
    /// workers before returning any failure.
    #[hotpath::measure(label = "query.artifact.prepare_pages")]
    pub fn prepare_pages(
        &self,
        pages: &[VerifiedSealedLexicalPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Vec<PreparedCodeLexicalArtifactPageV1>, CodeLexicalArtifactErrorV1> {
        let (_, prepared) = self.prepare_pages_inner(pages, control)?;
        admit_prepared_page_batch(
            self.fixed_ledger_charge_bytes,
            self.memory_budget_bytes,
            &prepared,
        )?;
        record_prepared_batch_metrics(&prepared);
        Ok(prepared)
    }

    /// Memory-bound an offered source batch, prepare that prefix once, then
    /// select the largest exact prepared prefix admitted by the row and
    /// estimated-write authorities. This avoids conservative pre-dedup row
    /// estimates while preserving every exact post-preparation cap.
    pub fn prepare_admissible_page_prefix(
        &self,
        pages: &[VerifiedSealedLexicalPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<PreparedCodeLexicalArtifactBatchV1, CodeLexicalArtifactErrorV1> {
        if pages.is_empty() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page batches must be non-empty".to_owned(),
            ));
        }
        let memory_prefix = self.largest_admissible_page_prefix(pages)?;
        if memory_prefix == 0 {
            record_batch_prefix_limit(CodeLexicalArtifactBatchLimitV1::Memory);
            admit_page_batch_within_memory_budget(
                &self.metadata,
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                &pages[..1],
            )?;
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact memory prefix rejected a separately admissible first page"
                    .to_owned(),
            ));
        }
        let (replayed_prefix, mut prepared) =
            self.prepare_pages_inner(&pages[..memory_prefix], control)?;
        let (fresh_prefix, exact_limit) = largest_exact_prepared_prefix(
            &prepared,
            self.fixed_ledger_charge_bytes,
            self.memory_budget_bytes,
        )?;
        if fresh_prefix == 0 && !prepared.is_empty() {
            let exceeded = exact_limit.ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact exact prefix rejected a page without a limiting authority"
                        .to_owned(),
                )
            })?;
            record_batch_prefix_limit(exceeded.limit);
            return Err(batch_limit(
                exceeded.limit,
                exceeded.required,
                exceeded.maximum,
            ));
        }
        prepared.truncate(fresh_prefix);
        let accepted = replayed_prefix
            .checked_add(fresh_prefix)
            .ok_or_else(batch_ledger_overflow)?;
        let accepted_prefix = NonZeroUsize::new(accepted).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact admissible source prefix was empty".to_owned(),
            )
        })?;
        if let Some(exceeded) = exact_limit {
            record_batch_prefix_limit(exceeded.limit);
        } else if memory_prefix < pages.len() {
            record_batch_prefix_limit(CodeLexicalArtifactBatchLimitV1::Memory);
        }
        record_prepared_batch_metrics(&prepared);
        Ok(PreparedCodeLexicalArtifactBatchV1 {
            accepted_prefix,
            prepared_pages: prepared,
        })
    }

    fn prepare_pages_inner(
        &self,
        pages: &[VerifiedSealedLexicalPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(usize, Vec<PreparedCodeLexicalArtifactPageV1>), CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        self.verify_path_binding()?;
        if pages.is_empty() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page batches must be non-empty".to_owned(),
            ));
        }
        if read_receipt(&self.connection)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts do not accept more source pages".to_owned(),
            ));
        }
        if finalization_started(&self.connection)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization has started; source pages are immutable".to_owned(),
            ));
        }

        hotpath::gauge!("query.artifact.batch.admission_total").inc(1u64);
        let (current, fresh_start) = hotpath::measure_block!("query.artifact.batch.admission", {
            prepare_page_batch_admission(
                &self.connection,
                &self.metadata,
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
            )
        })?;
        let fresh_pages = &pages[fresh_start..];
        if fresh_pages.is_empty() {
            return Ok((fresh_start, Vec::new()));
        }
        let previous_cursors = fresh_pages
            .iter()
            .enumerate()
            .map(|(index, _)| {
                if index == 0 {
                    current.next_cursor.as_ref().map(encode_cursor).transpose()
                } else {
                    encode_cursor(fresh_pages[index - 1].next_cursor()).map(Some)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let scratch = fresh_pages
            .iter()
            .map(|page| page_transient_peak_bytes(&self.metadata, page, usize::MAX))
            .collect::<Result<Vec<_>, _>>()?;
        let metadata = &self.metadata;
        let prepared = hotpath::measure_block!("query.artifact.batch.parallel_prepare", {
            tracedecay_code_index::parallelism::install(|| {
                fresh_pages
                    .par_iter()
                    .zip(previous_cursors.into_par_iter())
                    .zip(scratch.into_par_iter())
                    .enumerate()
                    .map(|(index, ((page, previous_cursor), scratch_bytes))| {
                        tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                prepare_page_values(
                                    metadata,
                                    page,
                                    previous_cursor,
                                    scratch_bytes,
                                    control,
                                )
                            }))
                            .unwrap_or_else(|payload| {
                                Err(CodeLexicalArtifactErrorV1::Io(
                                    tracedecay_code_index::parallelism::CodeIndexParallelismErrorV1::from_panic_payload(
                                        index,
                                        &*payload,
                                    )
                                    .to_string(),
                                ))
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            })
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        Ok((fresh_start, prepared))
    }

    /// Atomically admit an ordered prepared batch. The values carry no
    /// durable authority until this method commits their rows and receipts.
    pub fn append_prepared_pages(
        &mut self,
        pages: &[PreparedCodeLexicalArtifactPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        let result = self.append_prepared_pages_inner(pages, control);
        record_batch_outcome(&result);
        result
    }

    fn append_prepared_pages_inner(
        &mut self,
        pages: &[PreparedCodeLexicalArtifactPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        self.verify_path_binding()?;
        if read_receipt(&self.connection)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts do not accept more source pages".to_owned(),
            ));
        }
        if finalization_started(&self.connection)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization has started; source pages are immutable".to_owned(),
            ));
        }
        let current = progress(&self.connection)?;
        if pages.is_empty() {
            record_artifact_progress(&current);
            return Ok(current);
        }
        validate_prepared_page_batch(&current, pages)?;
        admit_prepared_page_batch(
            self.fixed_ledger_charge_bytes,
            self.memory_budget_bytes,
            pages,
        )?;
        let term_insert_plan = hotpath::measure_block!(
            "query.artifact.batch.term_order",
            prepare_term_insert_plan(
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
                control,
            )
        )?;
        let exact_insert_plan = hotpath::measure_block!(
            "query.artifact.batch.exact_order",
            prepare_exact_insert_plan(
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
                control,
            )
        )?;
        hotpath::measure_block!("query.artifact.batch.sqlite", {
            let _mutation_authority = BuilderMutationGuardV1::enter(&self.mutation_gate)?;
            let transaction = self.connection.transaction().map_err(sqlite_error)?;
            let mutation = (|| {
                hotpath::measure_block!("query.artifact.batch.imports", {
                    for page in pages {
                        append_prepared_imports(&transaction, page, control)?;
                    }
                    Ok::<(), CodeLexicalArtifactErrorV1>(())
                })?;
                record_batch_import_metrics(pages);
                hotpath::measure_block!(
                    "query.artifact.batch.clone_bodies",
                    append_prepared_clone_bodies(&transaction, pages, control)
                )?;
                hotpath::measure_block!(
                    "query.artifact.batch.rows.stage_dictionary",
                    stage_row_dictionary(&transaction, pages, control)
                )?;
                hotpath::measure_block!(
                    "query.artifact.batch.rows",
                    append_prepared_rows(&transaction, pages, control)
                )?;
                record_batch_row_metrics(pages);
                hotpath::measure_block!(
                    "query.artifact.batch.postings",
                    append_prepared_postings(
                        &transaction,
                        pages,
                        &term_insert_plan,
                        &exact_insert_plan,
                        control,
                    )
                )?;
                record_batch_posting_metrics(pages);
                hotpath::measure_block!("query.artifact.batch.receipts", {
                    for page in pages {
                        insert_prepared_source_page(&transaction, page)?;
                    }
                    Ok::<(), CodeLexicalArtifactErrorV1>(())
                })?;
                record_batch_receipt_metrics(pages);
                checkpoint(control)
            })();
            if let Err(error) = mutation {
                hotpath::gauge!("query.artifact.batch.rollbacks_total").inc(1u64);
                hotpath::measure_block!(
                    "query.artifact.batch.rollback",
                    transaction.rollback().map_err(sqlite_error)
                )?;
                return Err(error);
            }
            hotpath::gauge!("query.artifact.batch.commit_attempts_total").inc(1u64);
            let commit = hotpath::measure_block!(
                "query.artifact.batch.commit",
                transaction.commit().map_err(sqlite_error)
            );
            if commit.is_ok() {
                hotpath::gauge!("query.artifact.batch.commit_succeeded_total").inc(1u64);
            }
            commit
        })?;
        // Do not observe cancellation between durable COMMIT and publishing
        // its exact progress. The source callback must be able to advance its
        // cursor once the whole batch has committed.
        let progress = progress(&self.connection)?;
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("query.artifact.batch.committed_pages_total")
                .inc(u64::try_from(pages.len()).map_err(contract_number)?);
            hotpath::gauge!("query.artifact.batch.committed_chunks_total").inc(
                pages
                    .iter()
                    .try_fold(0u64, |total, page| total.checked_add(page.chunk_count))
                    .ok_or_else(|| {
                        CodeLexicalArtifactErrorV1::Contract(
                            "lexical artifact committed chunk count overflowed".to_owned(),
                        )
                    })?,
            );
        }
        record_artifact_progress(&progress);
        Ok(progress)
    }

    /// Advance durable receipt construction without rereading the sealed
    /// generation. Before digest verification, one wake commits exactly one
    /// set-wise statistics or serving-index statement and SQLite VM progress
    /// observes cancellation during that statement. During digest
    /// verification, `maximum_work` bounds the number of staged rows (or empty
    /// section completions) this call may consume.
    #[hotpath::measure(label = "query.artifact.finalization.advance_wake")]
    pub fn advance_finalization(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactErrorV1> {
        if maximum_work == 0 {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization work budget must be non-zero".to_owned(),
            ));
        }
        let mut wake_metrics = FinalizationWakeMetricsV1::new();
        checkpoint(control)?;
        self.verify_path_binding()?;
        verify_artifact_state_metadata(
            &self.connection,
            &self.metadata,
            &self.metadata_digest,
            control,
        )?;
        if let Some(receipt) = read_receipt(&self.connection)? {
            verify_sealed_receipt_header(&receipt, &self.metadata_digest, source)?;
            self.canonicalize_sealed_header()?;
            let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
            record_finalization_step(&step);
            return Ok(step);
        }

        if load_finalization_state(&self.connection)?.is_none() {
            let transaction = self.connection.transaction().map_err(sqlite_error)?;
            let mut transaction_metrics = FinalizationTransactionMetricsV1::new();
            let terminal_cursor = verify_staged_source_chain(&transaction, source, control)?;
            // The sorted pass and the count aggregation are the heaviest
            // sorter statements in finalization; run them under the same
            // sorter CPU admission as the pre-digest index wakes.
            super::with_builder_sorter_cpu_admission(&transaction, || {
                hotpath::measure_block!(
                    "query.artifact.finalization.derive_clone_fingerprint_postings",
                    with_cancellable_sqlite_statement(&transaction, control, || {
                        derive_clone_fingerprint_postings(
                            &transaction,
                            &self.mutation_gate,
                            control,
                        )
                    })
                )
            })??;
            let content_epoch = authenticated_authority_epoch(&transaction, source, control)?;
            install_base_freeze(&transaction)?;
            store_finalization_state(
                &transaction,
                &PersistedFinalizationStateV1::new(content_epoch, source, terminal_cursor)?,
            )?;
            checkpoint(control)?;
            commit_finalization_transaction(transaction, &mut transaction_metrics)?;
            let step = CodeLexicalArtifactFinalizationStepV1::Pending {
                phase: CodeLexicalArtifactFinalizationPhaseV1::IndexBuild,
                completed_sections: 0,
                completed_rows: 0,
            };
            record_finalization_step(&step);
            return Ok(step);
        }

        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let mut transaction_metrics = FinalizationTransactionMetricsV1::new();
        let mut state = load_finalization_state(&transaction)?.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization marker disappeared".to_owned(),
            )
        })?;
        validate_finalization_state(&state)?;
        wake_metrics.digest_pass(state.phase);
        ensure_content_epoch(&transaction, state.content_epoch)?;
        if &state.source_state_digest != source.source_state_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "bounded lexical artifact finalization received a different source receipt"
                    .to_owned(),
            ));
        }
        if state.phase != PersistedFinalizationPhaseV1::Digest {
            super::with_builder_sorter_cpu_admission(&transaction, || {
                advance_pre_digest_work(
                    &transaction,
                    &mut state,
                    &ServingIndexStepAuthorityV1 {
                        mutation_gate: &self.mutation_gate,
                        generation: &self.metadata.generation,
                        ngram_memory_bytes: self.memory_budget_bytes / 4,
                    },
                    control,
                )
            })??;
            store_finalization_state(&transaction, &state)?;
            checkpoint(control)?;
            commit_finalization_transaction(transaction, &mut transaction_metrics)?;
            let step = CodeLexicalArtifactFinalizationStepV1::Pending {
                phase: state.phase.public(),
                completed_sections: 0,
                completed_rows: state.completed_rows,
            };
            record_finalization_step(&step);
            return Ok(step);
        }
        let mut remaining_work = maximum_work;
        let section_names = &SECTION_NAMES;
        let section_count = u64::try_from(section_names.len()).map_err(contract_number)?;
        while remaining_work > 0 && state.section_ordinal < section_count {
            checkpoint(control)?;
            let section_ordinal =
                usize::try_from(state.section_ordinal).map_err(contract_number)?;
            let section = FinalizationSectionV1::from_ordinal(section_ordinal)?;
            let section_name = section.name();
            wake_metrics.phase(section);
            wake_metrics.probe();
            let rows =
                advance_section_rows(&transaction, section, &mut state, remaining_work, control)?;
            wake_metrics.add_rows(rows)?;
            if rows > 0 {
                remaining_work = remaining_work.checked_sub(rows).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact finalization exceeded its work budget".to_owned(),
                    )
                })?;
                continue;
            }

            let section_digest = finish_persisted_section(section_name, &state)?;
            state.completed_sections.push(section_digest);
            if section == FinalizationSectionV1::SourcePages {
                let base_sections = finish_base_section_receipt_fold(
                    &state.base_section_row_counts,
                    &state.base_section_accumulators,
                )?;
                state.completed_rows = base_sections
                    .iter()
                    .try_fold(state.completed_rows, |total, section| {
                        total.checked_add(section.row_count)
                    })
                    .ok_or_else(|| {
                        CodeLexicalArtifactErrorV1::Contract(
                            "lexical artifact adopted base-section row count overflowed".to_owned(),
                        )
                    })?;
                state.completed_sections.extend(base_sections);
                state.section_ordinal =
                    u64::try_from(1 + BASE_SECTION_NAMES.len()).map_err(contract_number)?;
            } else {
                state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact finalization section ordinal overflowed".to_owned(),
                    )
                })?;
            }
            state.section_row_count = 0;
            state.section_last_key = None;
            if state.section_ordinal < section_count {
                let next = section_names
                    [usize::try_from(state.section_ordinal).map_err(contract_number)?];
                state.section_accumulator = initial_section_accumulator(next)?.to_vec();
            }
            remaining_work -= 1;
        }

        if state.section_ordinal < section_count {
            store_finalization_state(&transaction, &state)?;
            checkpoint(control)?;
            commit_finalization_transaction(transaction, &mut transaction_metrics)?;
            let step = CodeLexicalArtifactFinalizationStepV1::Pending {
                phase: state.phase.public(),
                completed_sections: u64::try_from(state.completed_sections.len())
                    .map_err(contract_number)?,
                completed_rows: state.completed_rows,
            };
            record_finalization_step(&step);
            return Ok(step);
        }

        if state.completed_sections.len() != section_names.len() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization completed with an invalid section receipt"
                    .to_owned(),
            ));
        }
        // Page placement follows the order batches arrived in, so one content
        // staged through different batch sizes lays out differently. Rewriting
        // the file from its content before the seal makes its bytes a function
        // of the content alone, which is what lets worktrees share one file.
        // A crash before the seal commits repeats this rewrite on resume.
        store_finalization_state(&transaction, &state)?;
        commit_finalization_transaction(transaction, &mut transaction_metrics)?;
        hotpath::measure_block!("query.artifact.finalization.canonical_layout", {
            with_cancellable_sqlite_statement(&self.connection, control, || {
                self.connection
                    .execute_batch("VACUUM;")
                    .map_err(sqlite_error)
            })
        })?;
        checkpoint(control)?;
        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let mut transaction_metrics = FinalizationTransactionMetricsV1::new();
        let sections = state.completed_sections;
        verify_final_sections_against_source(&sections, source)?;
        // The resume state binds the building worktree's source; the sealed
        // file keeps none of it, not even the space its row occupied.
        transaction
            .execute_batch("DROP TABLE finalization_state;")
            .map_err(sqlite_error)?;
        release_free_pages(&transaction, control)?;
        let file_size_bytes = sqlite_file_size(&transaction)?;
        let receipt = new_verified_receipt(
            self.metadata_digest.clone(),
            source,
            sections,
            file_size_bytes,
        )?;
        transaction
            .execute(
                "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
                params![padded_receipt(&receipt)?],
            )
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        commit_finalization_transaction(transaction, &mut transaction_metrics)?;
        self.canonicalize_sealed_header()?;
        let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
        record_finalization_step(&step);
        Ok(step)
    }

    /// SQLite's file change counter (header offset 24) and version-valid-for
    /// number (offset 92) count commits, so one content staged through a
    /// different number of transactions differs only there. Once sealed no
    /// connection writes the file again, and both are set to one value.
    fn canonicalize_sealed_header(&self) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        let mut file = &self.private_file;
        for offset in SQLITE_HEADER_COMMIT_COUNTER_OFFSETS {
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&SEALED_COMMIT_COUNTER.to_be_bytes()))
                .map_err(private_staging_error)?;
        }
        file.sync_all().map_err(private_staging_error)
    }

    #[hotpath::measure(label = "query.artifact.finalize")]
    pub fn finalize(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        self.verify_path_binding()?;
        if let Some(receipt) = read_receipt(&self.connection)? {
            verify_finalized_artifact(
                &self.connection,
                &self.path,
                &self.metadata_digest,
                source,
                &receipt,
                control,
            )?;
            crate::hotpath_metrics::Residency::Warm.record("query.artifact.residency");
            hotpath::gauge!("query.artifact.pages").set(receipt.page_count());
            hotpath::gauge!("query.artifact.bytes").set(receipt.file_size_bytes());
            return Ok(receipt);
        }
        Err(CodeLexicalArtifactErrorV1::Corrupt(
            "unsealed lexical artifacts require bounded finalization before verification"
                .to_owned(),
        ))
    }

    fn verify_path_binding(&self) -> Result<(), CodeLexicalArtifactErrorV1> {
        verify_staging_file_binding(&self.path, &self.file_identity)
    }
}

/// Initialize a staging artifact under a private sibling and rename it onto
/// the staging path only once its singleton `artifact_state` row is durable.
///
/// The staging path's existence is the resume signal, and the resume path reads
/// the singleton row as a mandatory authority. A staging file that becomes
/// visible before that row is committed therefore turns a physical restart into
/// a permanent contract violation on a projection that remains rederivable from
/// its immutable sealed generation.
fn publish_initialized_staging(
    path: &Path,
    metadata: &CodeLexicalProjectionMetadataV1,
    metadata_digest: &ManifestDigest,
    memory_budget_bytes: usize,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let initializing = initializing_staging_sibling(path)?;
    // An incarnation that exited inside initialization left a sibling that
    // never became a staging path, so it holds no progress to preserve.
    match std::fs::remove_file(&initializing) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(private_staging_error(error)),
    }
    let metadata_bytes = content_metadata_bytes(metadata)?;
    let (connection, private_file, _) =
        create_private_builder_connection(&initializing, memory_budget_bytes)?;
    let _mutation_gate = register_builder_mutation_gate(&connection)?;
    create_schema(&connection)?;
    verify_builder_mutation_gate_schema(&connection)?;
    #[cfg(test)]
    if take_failed_staging_initialization() {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "injected lexical artifact staging initialization failure".to_owned(),
        ));
    }
    connection
        .execute(
            "INSERT INTO artifact_state(singleton, format_revision, metadata, metadata_digest, receipt) VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1),
                metadata_bytes,
                metadata_digest.as_str(),
                vec![0u8; RECEIPT_RESERVATION_BYTES],
            ],
        )
        .map_err(sqlite_error)?;
    // SQLite names a rollback journal after the path its connection opened, so
    // the initializing connection closes before the rename; appends run under a
    // connection bound to the visible staging path.
    drop(connection);
    private_file
        .sync_all()
        .map_err(|error| staging_initialization_error("sync its initialized state", error))?;
    drop(private_file);
    std::fs::rename(&initializing, path)
        .map_err(|error| staging_initialization_error("name the staging path", error))?;
    sync_parent_directory(path, DirectorySyncPolicy::Strict)
        .map_err(|error| staging_initialization_error("sync the staging directory", error))
}

fn staging_initialization_error(
    step: &'static str,
    error: std::io::Error,
) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(format!(
        "lexical artifact staging initialization cannot {step}: {error}"
    ))
}

fn initializing_staging_sibling(path: &Path) -> Result<PathBuf, CodeLexicalArtifactErrorV1> {
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path has no file name".to_owned(),
            )
        })?
        .to_os_string();
    name.push(".initializing");
    Ok(path.with_file_name(name))
}

fn create_private_builder_connection(
    path: &Path,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = create_private_file_retained(path)
        .map_err(|failure| private_staging_error(failure.into_error()))?;
    open_bound_builder_connection(path, private_file, memory_budget_bytes)
}

fn open_private_builder_connection(
    path: &Path,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = open_private_file(path).map_err(private_staging_error)?;
    open_bound_builder_connection(path, private_file, memory_budget_bytes)
}

fn open_bound_builder_connection(
    path: &Path,
    private_file: File,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let identity = stable_file_identity(&private_file)?;
    let connection = open_builder_connection(path, memory_budget_bytes)?;
    let rebound = open_private_file(path).map_err(private_staging_error)?;
    if stable_file_identity(&rebound)? != identity {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact staging path changed while its SQLite connection opened".to_owned(),
        ));
    }
    Ok((connection, private_file, identity))
}

fn stable_file_identity(
    file: &File,
) -> Result<StableArtifactFileIdentityV1, CodeLexicalArtifactErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata().map_err(private_staging_error)?;
        Ok(StableArtifactFileIdentityV1 {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let information = tracedecay_private_fs::windows_file::information(file)
            .map_err(private_staging_error)?;

        Ok(StableArtifactFileIdentityV1 {
            volume_serial_number: information.volume_serial_number,
            file_index: information.file_index,
        })
    }
}

fn verify_staging_file_binding(
    path: &Path,
    expected: &StableArtifactFileIdentityV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let current = open_private_file(path).map_err(private_staging_error)?;
    if stable_file_identity(&current)? != *expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact staging path no longer names the opened private file".to_owned(),
        ));
    }
    Ok(())
}

fn private_staging_error(error: std::io::Error) -> CodeLexicalArtifactErrorV1 {
    if error.kind() == std::io::ErrorKind::NotFound {
        CodeLexicalArtifactErrorV1::Missing("lexical artifact staging file is missing".to_owned())
    } else {
        CodeLexicalArtifactErrorV1::Contract(format!(
            "lexical artifact staging path must be an owner-private regular file without links: {error}"
        ))
    }
}

/// Amortized per-entry b-tree node overhead (headers and edge pointers)
/// charged on top of each entry's key/value payload.
const BTREE_MAP_ENTRY_OVERHEAD_BYTES: usize = 16;
const PERSISTED_CURSOR_DIGEST_FIELDS: usize = 4;
const PERSISTED_CURSOR_U64_FIELDS: usize = 12;
const MAX_DECIMAL_U64_BYTES: usize = 20;
const PERSISTED_CURSOR_JSON_DELIMITERS_BYTES: usize = 64;
const PREPARED_PAGE_DIGEST_FIELDS: usize = 3;

/// Validate a caller-selected build memory budget and return the fixed
/// ledger charge it must absorb before any page is admitted.
///
/// Create and resume each hold up to two simultaneous metadata structures
/// (the retained copy plus the decoded stored copy) and one serialized JSON
/// copy, so the fixed charge covers all three.
fn validated_fixed_ledger_charge(
    metadata: &CodeLexicalProjectionMetadataV1,
    memory_budget_bytes: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    if memory_budget_bytes == 0
        || memory_budget_bytes > CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1
    {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "lexical artifact build memory budget must be within 1..={CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1} bytes"
        )));
    }
    let serialized_bytes = metadata_serialized_upper_bound(metadata);
    let fixed = ARTIFACT_SQLITE_CACHE_BYTES
        .checked_add(
            metadata_retained_bytes(metadata)
                .checked_mul(2)
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact metadata ledger charge overflowed".to_owned(),
                    )
                })?,
        )
        .and_then(|bytes| bytes.checked_add(serialized_bytes))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact fixed ledger charge overflowed".to_owned(),
            )
        })?;
    if fixed >= memory_budget_bytes {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "the {ARTIFACT_SQLITE_CACHE_BYTES}-byte SQLite cache authority and retained metadata exhaust the {memory_budget_bytes}-byte build memory budget"
        )));
    }
    Ok(fixed)
}

/// A conservative byte-only upper bound for serializing metadata. Admission
/// cannot allocate just to discover that the fixed ledger would not fit.
fn metadata_serialized_upper_bound(metadata: &CodeLexicalProjectionMetadataV1) -> usize {
    let path_bytes = metadata
        .logical_paths
        .iter()
        .fold(0usize, |total, (file, path)| {
            total
                .saturating_add(file.as_str().len())
                .saturating_add(path.len())
                .saturating_add(32)
        });
    metadata_retained_bytes(metadata)
        .saturating_add(path_bytes)
        .saturating_mul(6)
        .saturating_add(512)
}

/// Owned bytes one projection metadata structure retains: logical paths at
/// capacity with per-entry b-tree node overhead, and every scalar identity
/// string charged as its `String` header plus payload length.
fn metadata_retained_bytes(metadata: &CodeLexicalProjectionMetadataV1) -> usize {
    let path_bytes = metadata.logical_paths.iter().fold(
        metadata.logical_paths.len().saturating_mul(
            std::mem::size_of::<(FileOccurrenceId, String)>()
                .saturating_add(BTREE_MAP_ENTRY_OVERHEAD_BYTES),
        ),
        |bytes, (file, path)| {
            bytes
                .saturating_add(file.as_str().len())
                .saturating_add(path.capacity())
        },
    );
    let scalar_identities = [
        Some(metadata.generation.as_str()),
        metadata
            .repository_id
            .as_ref()
            .map(|repository| repository.as_str()),
        Some(metadata.freshness.source_namespace.as_str()),
        Some(metadata.freshness.source_instance.as_str()),
        Some(metadata.freshness.policy_revision.as_str()),
        Some(metadata.exact_retriever_revision.as_str()),
        Some(metadata.lexical_retriever_revision.as_str()),
        Some(metadata.exact_score_domain.as_str()),
    ];
    scalar_identities
        .into_iter()
        .flatten()
        .fold(path_bytes, |bytes, identity| {
            bytes
                .saturating_add(std::mem::size_of::<String>())
                .saturating_add(identity.len())
        })
}

fn page_batch_ledger_charge_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    pages: &[VerifiedSealedLexicalPageV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let retained = pages.iter().try_fold(0usize, |total, page| {
        total
            .checked_add(page.retained_owned_bytes())
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact batch retained-byte charge overflowed".to_owned(),
                )
            })
    })?;
    let prepared_retained = pages.iter().try_fold(0usize, |total, page| {
        page_prepared_retained_upper_bound_bytes(metadata, page).and_then(|page_bound| {
            total.checked_add(page_bound).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact batch prepared-retained charge overflowed".to_owned(),
                )
            })
        })
    })?;
    let active_workers = tracedecay_code_index::parallelism::indexing_workers().min(pages.len());
    let mut scratch = pages
        .iter()
        .map(|page| page_transient_peak_bytes(metadata, page, usize::MAX))
        .collect::<Result<Vec<_>, _>>()?;
    scratch.sort_unstable_by(|left, right| right.cmp(left));
    let active_scratch =
        scratch
            .into_iter()
            .take(active_workers)
            .try_fold(0usize, |total, charge| {
                total.checked_add(charge).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact batch preparation scratch charge overflowed".to_owned(),
                    )
                })
            })?;
    retained
        .checked_add(prepared_retained)
        .and_then(|bytes| bytes.checked_add(active_scratch))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact batch ledger charge overflowed".to_owned(),
            )
        })
}

#[derive(Default)]
struct CanonicalBatchLimitLedgerV1 {
    estimated_rows: usize,
    estimated_write_bytes: usize,
}

#[derive(Clone, Copy)]
struct BatchLimitExceededV1 {
    limit: CodeLexicalArtifactBatchLimitV1,
    required: usize,
    maximum: usize,
}

impl CanonicalBatchLimitLedgerV1 {
    fn try_admit(
        &mut self,
        page_rows: usize,
        page_write_bytes: usize,
    ) -> Result<Option<BatchLimitExceededV1>, CodeLexicalArtifactErrorV1> {
        let estimated_rows = self.estimated_rows.checked_add(page_rows).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact batch row preflight overflowed".to_owned(),
            )
        })?;
        let estimated_write_bytes = self
            .estimated_write_bytes
            .checked_add(page_write_bytes)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact batch write preflight overflowed".to_owned(),
                )
            })?;
        if estimated_rows > CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1 {
            return Ok(Some(BatchLimitExceededV1 {
                limit: CodeLexicalArtifactBatchLimitV1::PreparedRows,
                required: estimated_rows,
                maximum: CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1,
            }));
        }
        if estimated_write_bytes > CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1 {
            return Ok(Some(BatchLimitExceededV1 {
                limit: CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes,
                required: estimated_write_bytes,
                maximum: CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1,
            }));
        }
        self.estimated_rows = estimated_rows;
        self.estimated_write_bytes = estimated_write_bytes;
        Ok(None)
    }
}

fn largest_exact_prepared_prefix(
    pages: &[PreparedCodeLexicalArtifactPageV1],
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
) -> Result<(usize, Option<BatchLimitExceededV1>), CodeLexicalArtifactErrorV1> {
    let mut ledger = CanonicalBatchLimitLedgerV1::default();
    for (index, page) in pages.iter().enumerate() {
        if let Some(exceeded) =
            ledger.try_admit(page.estimated_write_rows(), page.estimated_write_bytes())?
        {
            return Ok((index, Some(exceeded)));
        }
        let required = prepared_batch_memory_with_posting_plans_required_bytes(
            fixed_ledger_charge_bytes,
            &pages[..=index],
        )?;
        if required > memory_budget_bytes {
            return Ok((
                index,
                Some(BatchLimitExceededV1 {
                    limit: CodeLexicalArtifactBatchLimitV1::Memory,
                    required,
                    maximum: memory_budget_bytes,
                }),
            ));
        }
    }
    Ok((pages.len(), None))
}

/// Refuse a batch unless its retained source pages, all prepared outputs, and
/// one scratch peak per active worker fit together. Admission runs before the
/// staging transaction, so refusal leaves builder and source progress intact.
fn admit_page_batch_within_memory_budget(
    metadata: &CodeLexicalProjectionMetadataV1,
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &[VerifiedSealedLexicalPageV1],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for page in pages {
        if page.retained_owned_bytes() > CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1 {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "sealed lexical page retained bytes exceed the {}-byte artifact input bound",
                CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
            )));
        }
    }
    let additional = page_batch_ledger_charge_bytes(metadata, pages)?;
    let required = fixed_ledger_charge_bytes
        .checked_add(additional)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact batch total ledger charge overflowed".to_owned(),
            )
        })?;
    if required > memory_budget_bytes {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            memory_budget_bytes,
        ));
    }
    Ok(())
}

fn prepared_batch_memory_required_bytes(
    fixed_ledger_charge_bytes: usize,
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let source_retained = pages.iter().try_fold(0usize, |total, page| {
        total
            .checked_add(page.source_retained_bytes())
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical batch source-retained charge overflowed".to_owned(),
                )
            })
    })?;
    let prepared_retained = pages.iter().try_fold(0usize, |total, page| {
        total
            .checked_add(page.retained_owned_bytes())
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical batch retained charge overflowed".to_owned(),
                )
            })
    })?;
    let active_workers = tracedecay_code_index::parallelism::indexing_workers().min(pages.len());
    let mut scratch = pages
        .iter()
        .map(PreparedCodeLexicalArtifactPageV1::preparation_scratch_bytes)
        .collect::<Vec<_>>();
    scratch.sort_unstable_by(|left, right| right.cmp(left));
    let active_scratch = scratch
        .into_iter()
        .take(active_workers)
        .try_fold(0usize, |total, charge| total.checked_add(charge))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "prepared lexical batch active-worker scratch charge overflowed".to_owned(),
            )
        })?;
    fixed_ledger_charge_bytes
        .checked_add(source_retained)
        .and_then(|bytes| bytes.checked_add(prepared_retained))
        .and_then(|bytes| bytes.checked_add(active_scratch))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "prepared lexical batch total ledger charge overflowed".to_owned(),
            )
        })
}

fn prepared_term_row_count(
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    pages
        .iter()
        .flat_map(|page| &page.documents)
        .try_fold(0usize, |rows, document| {
            rows.checked_add(document.term_postings.len())
                .ok_or_else(batch_ledger_overflow)
        })
}

fn term_insert_plan_ledger_bytes(term_rows: usize) -> Result<usize, CodeLexicalArtifactErrorV1> {
    term_rows
        .checked_mul(TERM_INSERT_PLAN_BYTES_PER_REF)
        .ok_or_else(batch_ledger_overflow)
}

fn prepared_exact_row_count(
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    pages
        .iter()
        .flat_map(|page| &page.documents)
        .try_fold(0usize, |rows, document| {
            rows.checked_add(document.exact_postings.len())
                .ok_or_else(batch_ledger_overflow)
        })
}

fn exact_insert_plan_ledger_bytes(exact_rows: usize) -> Result<usize, CodeLexicalArtifactErrorV1> {
    exact_rows
        .checked_mul(EXACT_INSERT_PLAN_BYTES_PER_REF)
        .ok_or_else(batch_ledger_overflow)
}

fn prepared_batch_memory_with_posting_plans_required_bytes(
    fixed_ledger_charge_bytes: usize,
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let base = prepared_batch_memory_required_bytes(fixed_ledger_charge_bytes, pages)?;
    let term_plan = term_insert_plan_ledger_bytes(prepared_term_row_count(pages)?)?;
    let exact_plan = exact_insert_plan_ledger_bytes(prepared_exact_row_count(pages)?)?;
    base.checked_add(term_plan)
        .and_then(|bytes| bytes.checked_add(exact_plan))
        .ok_or_else(batch_ledger_overflow)
}

fn admit_prepared_page_batch(
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let required =
        prepared_batch_memory_with_posting_plans_required_bytes(fixed_ledger_charge_bytes, pages)?;
    if required > memory_budget_bytes {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            memory_budget_bytes,
        ));
    }
    let estimated_rows = sum_prepared_metric(
        pages,
        PreparedCodeLexicalArtifactPageV1::estimated_write_rows,
        "prepared lexical batch row estimate overflowed",
    )?;
    if estimated_rows > CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1 {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::PreparedRows,
            estimated_rows,
            CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1,
        ));
    }
    let estimated_write_bytes = sum_prepared_metric(
        pages,
        PreparedCodeLexicalArtifactPageV1::estimated_write_bytes,
        "prepared lexical batch write estimate overflowed",
    )?;
    if estimated_write_bytes > CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1 {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes,
            estimated_write_bytes,
            CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1,
        ));
    }
    Ok(())
}

fn prepare_term_insert_plan<'a>(
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &'a [PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<PreparedTermInsertPlanV1<'a>, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let mut term_rows = 0usize;
    for page in pages {
        checkpoint(control)?;
        for document in &page.documents {
            checkpoint(control)?;
            term_rows = term_rows
                .checked_add(document.term_postings.len())
                .ok_or_else(batch_ledger_overflow)?;
            if term_rows > CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1 {
                return Err(batch_limit(
                    CodeLexicalArtifactBatchLimitV1::PreparedRows,
                    term_rows,
                    CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1,
                ));
            }
        }
    }
    let plan_bytes = term_insert_plan_ledger_bytes(term_rows)?;
    let required = prepared_batch_memory_required_bytes(fixed_ledger_charge_bytes, pages)?
        .checked_add(plan_bytes)
        .ok_or_else(batch_ledger_overflow)?;
    if required > memory_budget_bytes {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            memory_budget_bytes,
        ));
    }

    let mut entries = Vec::new();
    entries.try_reserve_exact(term_rows).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical term insert plan allocation failed: {error}"
        ))
    })?;
    for page in pages {
        checkpoint(control)?;
        for document in &page.documents {
            checkpoint(control)?;
            for posting in &document.term_postings {
                entries.push(PreparedTermInsertRefV1::new(
                    document.document_id,
                    field_code_from_encoded(&posting.field)?,
                    posting,
                ));
            }
        }
    }
    checkpoint(control)?;
    sort_insert_plan(&mut entries, PreparedTermInsertRefV1::key, control)?;
    checkpoint(control)?;
    Ok(PreparedTermInsertPlanV1 { entries })
}

/// Sort one batch's insert plan on the indexing pool.
///
/// Runs of [`TERM_INSERT_CONTROL_INTERVAL`] sort in waves so cancellation is
/// observed between them, and each wave takes a background CPU permit. A
/// k-way merge then materializes the total order, checkpointing on the same
/// interval. The merge holds a second copy of the plan until the first is
/// dropped.
fn sort_insert_plan<T, K>(
    entries: &mut Vec<T>,
    key: impl Fn(&T) -> K + Copy + Sync,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1>
where
    T: Copy + Send,
    K: Ord + Copy + Send,
{
    if entries.len() <= 1 {
        return Ok(());
    }
    let run = TERM_INSERT_CONTROL_INTERVAL;
    let workers = tracedecay_code_index::parallelism::indexing_workers().max(1);
    let wave = run.saturating_mul(workers);
    let mut start = 0;
    while start < entries.len() {
        checkpoint(control)?;
        let end = start.saturating_add(wave).min(entries.len());
        let slice = &mut entries[start..end];
        tracedecay_code_index::parallelism::install(|| {
            slice.par_chunks_mut(run).for_each(|chunk| {
                tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                    chunk.sort_unstable_by_key(key);
                });
            });
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        start = end;
    }
    if entries.len() <= run {
        return Ok(());
    }
    merge_sorted_runs(entries, run, key, control)
}

fn merge_sorted_runs<T, K>(
    entries: &mut Vec<T>,
    run: usize,
    key: impl Fn(&T) -> K,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1>
where
    T: Copy,
    K: Ord + Copy,
{
    let mut heap = BinaryHeap::new();
    let mut run_index = 0usize;
    let mut start = 0usize;
    while start < entries.len() {
        heap.push((Reverse(key(&entries[start])), run_index, start));
        run_index += 1;
        start = start.saturating_add(run);
    }
    let mut merged = Vec::new();
    merged.try_reserve_exact(entries.len()).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical insert plan merge allocation failed: {error}"
        ))
    })?;
    let mut emitted = 0usize;
    while let Some((Reverse(_), run_index, index)) = heap.pop() {
        if emitted.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        merged.push(entries[index]);
        emitted += 1;
        let next = index + 1;
        let run_end = run_index
            .saturating_add(1)
            .saturating_mul(run)
            .min(entries.len());
        if next < run_end {
            heap.push((Reverse(key(&entries[next])), run_index, next));
        }
    }
    *entries = merged;
    Ok(())
}

// Mirrors `prepare_term_insert_plan` for `exact_postings`,
// whose `PRIMARY KEY(field, term, document_id)` `WITHOUT ROWID` layout has
// the same clustered-index cost for out-of-order inserts as `term_postings`.
fn prepare_exact_insert_plan<'a>(
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &'a [PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<PreparedExactInsertPlanV1<'a>, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let mut exact_rows = 0usize;
    for page in pages {
        checkpoint(control)?;
        for document in &page.documents {
            checkpoint(control)?;
            exact_rows = exact_rows
                .checked_add(document.exact_postings.len())
                .ok_or_else(batch_ledger_overflow)?;
            if exact_rows > CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1 {
                return Err(batch_limit(
                    CodeLexicalArtifactBatchLimitV1::PreparedRows,
                    exact_rows,
                    CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1,
                ));
            }
        }
    }
    let plan_bytes = exact_insert_plan_ledger_bytes(exact_rows)?;
    let required = prepared_batch_memory_required_bytes(fixed_ledger_charge_bytes, pages)?
        .checked_add(plan_bytes)
        .ok_or_else(batch_ledger_overflow)?;
    if required > memory_budget_bytes {
        return Err(batch_limit(
            CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            memory_budget_bytes,
        ));
    }

    let mut entries = Vec::new();
    entries.try_reserve_exact(exact_rows).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical exact insert plan allocation failed: {error}"
        ))
    })?;
    // One digest per distinct term rather than per posting: the same term
    // repeats across documents, and the intern step needs the distinct set
    // anyway.
    let mut term_ids: HashMap<&[u8], i64> = HashMap::new();
    for page in pages {
        checkpoint(control)?;
        for document in &page.documents {
            checkpoint(control)?;
            for (field, term) in &document.exact_postings {
                let term = term.as_slice();
                let term_id = *term_ids
                    .entry(term)
                    .or_insert_with(|| stable_exact_term_id(term));
                entries.push(PreparedExactInsertRefV1 {
                    document_id: document.document_id,
                    field_code: exact_field_code_from_encoded(field)?,
                    term_id,
                });
            }
        }
    }
    let mut interned_terms: Vec<(&[u8], i64)> = term_ids.into_iter().collect();
    // Ascending by id, then by bytes: two distinct terms sharing an id is the
    // collision `intern_exact_terms` rejects, and it must reject the same one
    // on every run rather than whichever the hash map happened to yield first.
    interned_terms.sort_unstable_by_key(|(term, term_id)| (*term_id, *term));
    checkpoint(control)?;
    sort_insert_plan(&mut entries, PreparedExactInsertRefV1::key, control)?;
    checkpoint(control)?;
    Ok(PreparedExactInsertPlanV1 {
        entries,
        interned_terms,
    })
}

fn sum_prepared_metric(
    pages: &[PreparedCodeLexicalArtifactPageV1],
    metric: impl Fn(&PreparedCodeLexicalArtifactPageV1) -> usize,
    overflow: &str,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    pages.iter().try_fold(0usize, |total, page| {
        total
            .checked_add(metric(page))
            .ok_or_else(|| CodeLexicalArtifactErrorV1::Contract(overflow.to_owned()))
    })
}

fn batch_limit(
    limit: CodeLexicalArtifactBatchLimitV1,
    required: usize,
    maximum: usize,
) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::BatchTooLarge {
        limit,
        required,
        maximum,
    }
}

fn batch_ledger_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(
        "lexical artifact batch ledger charge overflowed".to_owned(),
    )
}

fn prepare_page_batch_admission(
    connection: &Connection,
    metadata: &CodeLexicalProjectionMetadataV1,
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &[VerifiedSealedLexicalPageV1],
) -> Result<(CodeLexicalArtifactBuildProgressV1, usize), CodeLexicalArtifactErrorV1> {
    admit_page_batch_within_memory_budget(
        metadata,
        fixed_ledger_charge_bytes,
        memory_budget_bytes,
        pages,
    )?;
    let current = progress(connection)?;
    let persisted_previous = pages
        .first()
        .map(|page| cursor_before_page(connection, page.page_ordinal()))
        .transpose()?
        .flatten();
    // Each page re-derives its chain from its own recorded predecessor, so the
    // transitions verify independently; the ordered checks below still report
    // the first failure in page order.
    let transitions = tracedecay_code_index::parallelism::install(|| {
        pages
            .par_iter()
            .enumerate()
            .map(|(index, page)| {
                let previous = match index.checked_sub(1) {
                    None => persisted_previous.as_ref(),
                    Some(previous) => pages
                        .get(previous)
                        .map(VerifiedSealedLexicalPageV1::next_cursor),
                };
                page.verify_transition(previous)
            })
            .collect::<Vec<_>>()
    })
    .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut fresh_start = pages.len();
    let mut expected_fresh_ordinal = current.next_page_ordinal;
    for ((index, page), transition) in pages.iter().enumerate().zip(transitions) {
        if let Some(previous_page) = index.checked_sub(1).and_then(|index| pages.get(index)) {
            let expected = previous_page.page_ordinal().checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "sealed lexical page ordinal overflowed".to_owned(),
                )
            })?;
            if page.page_ordinal() != expected {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "sealed lexical page batches must be contiguous and ordered".to_owned(),
                ));
            }
        }
        transition.map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if page.page_ordinal() < current.next_page_ordinal {
            verify_replayed_page(connection, page)?;
            continue;
        }
        if page.page_ordinal() != expected_fresh_ordinal {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "sealed lexical pages must be appended in exact ordinal order".to_owned(),
            ));
        }
        if fresh_start == pages.len() {
            fresh_start = index;
            if let Some(cumulative) = &current.cumulative_source_digest
                && page.page_ordinal() > 0
                && cumulative == page.cumulative_digest()
            {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "sealed lexical page did not advance its cumulative digest".to_owned(),
                ));
            }
        }
        expected_fresh_ordinal = expected_fresh_ordinal.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "sealed lexical page ordinal overflowed".to_owned(),
            )
        })?;
    }
    Ok((current, fresh_start))
}

fn symbol_field_size(display: Option<&VerifiedSealedLexicalSymbolDisplayV1>) -> (usize, usize) {
    display.map_or((0, 0), |display| {
        let bytes = display
            .simple_name()
            .len()
            .saturating_add(display.qualified_name().len())
            .saturating_add(display.signature().map_or(0, str::len))
            .saturating_add(display.documentation().map_or(0, str::len));
        // Source byte lengths bound token counts. Qualified names receive a
        // second budget for derived owner/member spelling.
        let entries = display
            .simple_name()
            .len()
            .saturating_add(display.qualified_name().len().saturating_mul(2))
            .saturating_add(display.signature().map_or(0, str::len))
            .saturating_add(display.documentation().map_or(0, str::len));
        (bytes, entries)
    })
}

/// The widest transient upper bound one staged chunk or import can require.
/// It is evaluated a record at a time without allocations and aborts once
/// `abort_above` is exceeded. The returned lower bound already fails admission.
fn page_transient_peak_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
    abort_above: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let mut peak = 0usize;
    for (admitted, display) in page.chunks().iter().zip(page.symbol_displays()) {
        let (symbol_field_bytes, symbol_field_entries) = symbol_field_size(display.as_ref());
        peak = peak.max(projected_chunk_transient_bytes(
            metadata,
            admitted,
            symbol_field_bytes,
            symbol_field_entries,
        )?);
        if peak > abort_above {
            return Ok(peak);
        }
    }
    for evidence in page.imports() {
        peak = peak.max(import_transient_bytes(evidence)?);
        if peak > abort_above {
            return Ok(peak);
        }
    }
    Ok(peak)
}

/// Conservative output-plus-scratch upper bound before one page is prepared.
/// Every derived retained value coexists with that page's widest transient
/// record allocation.
fn page_preparation_upper_bound_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    page_prepared_retained_upper_bound_bytes(metadata, page)?
        .checked_add(page_transient_peak_bytes(metadata, page, usize::MAX)?)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page preparation charge overflowed".to_owned(),
            )
        })
}

/// Conservative retained output for one fully prepared page. Every record's
/// owned projection remains live until the ordered batch commits, while only
/// the widest per-worker scratch allocation is charged separately.
fn page_prepared_retained_upper_bound_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let chunk_bytes = page.chunks().iter().zip(page.symbol_displays()).try_fold(
        0usize,
        |total, (admitted, display)| {
            let (symbol_field_bytes, symbol_field_entries) = symbol_field_size(display.as_ref());
            total
                .checked_add(projected_chunk_prepared_retained_upper_bound_bytes(
                    metadata,
                    admitted,
                    symbol_field_bytes,
                    symbol_field_entries,
                )?)
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact page preparation charge overflowed".to_owned(),
                    )
                })
        },
    )?;
    let record_bytes = page
        .imports()
        .iter()
        .try_fold(chunk_bytes, |total, evidence| {
            total
                .checked_add(import_transient_bytes(evidence)?)
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact page preparation charge overflowed".to_owned(),
                    )
                })
        })?;
    record_bytes
        .checked_add(prepared_page_authority_upper_bound_bytes(page)?)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact page preparation charge overflowed".to_owned(),
            )
        })
}

fn projected_chunk_prepared_retained_upper_bound_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    admitted: &ExtractionAdmittedCodeSearchChunkV1,
    symbol_field_bytes: usize,
    symbol_field_entries: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let transient = projected_chunk_transient_bytes(
        metadata,
        admitted,
        symbol_field_bytes,
        symbol_field_entries,
    )?;
    let text_bytes = admitted.chunk().sanitized_text.as_str().len();
    let normalized_text_bytes = text_bytes.saturating_add(symbol_field_bytes);
    let (_, normalized_scratch) = document_ngram_scratch(normalized_text_bytes)?;
    let (_, raw_scratch) = document_ngram_scratch(text_bytes)?;
    // Every authorized n-gram slot may become a distinct ordered-map key with
    // its own Roaring container while already-encoded shards accumulate.
    // The exact prepared ledger separately charges those encoded blobs.
    let ngram_aggregation_bytes = normalized_scratch
        .checked_add(raw_scratch)
        .and_then(|bytes| bytes.checked_div(std::mem::size_of::<u32>()))
        .and_then(|slots| slots.checked_mul(NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared n-gram aggregation charge overflowed".to_owned(),
            )
        })?;
    transient
        .checked_add(ngram_aggregation_bytes)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared document charge overflowed".to_owned(),
            )
        })
}

/// Page-level prepared ownership that is not attributable to one chunk or
/// import. Duplicating the source page's complete retained charge covers
/// vector capacities and typed identities; the explicit cursor envelope
/// covers both serialized cursor copies and their JSON framing without
/// allocating during admission.
fn prepared_page_authority_upper_bound_bytes(
    page: &VerifiedSealedLexicalPageV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let digest_bytes = page
        .page_digest()
        .as_str()
        .len()
        .max(page.cumulative_digest().as_str().len())
        .max(page.next_cursor().import_dictionary_digest().as_str().len());
    let numeric_bytes = PERSISTED_CURSOR_U64_FIELDS
        .checked_mul(MAX_DECIMAL_U64_BYTES)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared cursor numeric authority overflowed".to_owned(),
            )
        })?;
    let cursor_bytes = digest_bytes
        .checked_mul(PERSISTED_CURSOR_DIGEST_FIELDS)
        .and_then(|bytes| bytes.checked_add(numeric_bytes))
        .and_then(|bytes| bytes.checked_add(PERSISTED_CURSOR_JSON_DELIMITERS_BYTES))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared cursor authority overflowed".to_owned(),
            )
        })?;
    let prepared_digest_bytes = digest_bytes
        .checked_mul(PREPARED_PAGE_DIGEST_FIELDS)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared digest authority overflowed".to_owned(),
            )
        })?;
    let persisted_cursor_bytes = cursor_bytes.checked_mul(2).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract(
            "lexical artifact prepared cursor copies overflowed".to_owned(),
        )
    })?;
    page.retained_owned_bytes()
        .checked_add(std::mem::size_of::<PreparedCodeLexicalArtifactPageV1>())
        .and_then(|bytes| bytes.checked_add(prepared_digest_bytes))
        .and_then(|bytes| bytes.checked_add(persisted_cursor_bytes))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact prepared page authority overflowed".to_owned(),
            )
        })
}

/// Conservative transient bytes staging one admitted chunk may allocate.
///
/// This is intentionally arithmetic-only: budget refusal must not clone,
/// normalize, project, serialize, or reserve n-gram scratch merely to decide
/// that a page is too large. Components are charged as simultaneous, so the
/// upper bound covers the append path's clone, projection, token maps,
/// serialization, and both n-gram windows.
fn projected_chunk_transient_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    admitted: &ExtractionAdmittedCodeSearchChunkV1,
    symbol_field_bytes: usize,
    symbol_field_entries: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let chunk = admitted.chunk();
    let clone_bytes = chunk_owned_bytes(chunk);
    let logical_path = metadata
        .logical_paths
        .get(&chunk.anchor.file_occurrence_id)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact metadata is missing path {}",
                chunk.anchor.file_occurrence_id
            ))
        })?;
    // Canonical lexical normalization is ASCII lowercasing, so it preserves
    // the exact UTF-8 byte length. JSON escaping is charged separately below.
    // The bound intentionally charges both cloned and moved ownership before
    // append allocates either representation.
    let text_bytes = chunk.sanitized_text.as_str().len();
    let subtoken_bytes = chunk
        .subtokens
        .iter()
        .fold(0usize, |total, term| total.saturating_add(term.len()));
    let exact_bytes = chunk.exact_terms.iter().fold(0usize, |total, term| {
        total.saturating_add(term.canonical_bytes().len())
    });
    let normalized_text_bytes = text_bytes
        .saturating_add(logical_path.len())
        .saturating_add(symbol_field_bytes);
    // The projection indexes both the complete verified name and its symbol
    // suffix. The complete name length bounds each field string, frequency
    // entry and encoded posting.
    let field_text_bytes = normalized_text_bytes
        .saturating_add(logical_path.len())
        .saturating_add(subtoken_bytes)
        .saturating_add(exact_bytes.saturating_mul(2));
    let field_entries = lexical_token_count(chunk.sanitized_text.as_str())
        .saturating_add(symbol_field_entries)
        .saturating_add(1)
        .saturating_add(chunk.subtokens.len())
        .saturating_add(chunk.exact_terms.len().saturating_mul(2));
    let field_bytes = field_text_bytes
        .saturating_add(field_entries.saturating_mul(std::mem::size_of::<String>()))
        .saturating_add(
            9usize.saturating_mul(std::mem::size_of::<(LexicalFieldV1, Vec<String>)>()),
        );
    let frequency_bytes = field_entries
        .saturating_mul(std::mem::size_of::<(&str, u32)>() + BTREE_MAP_ENTRY_OVERHEAD_BYTES);
    let (_, normalized_scratch) = document_ngram_scratch(normalized_text_bytes)?;
    let (_, raw_scratch) = document_ngram_scratch(text_bytes)?;
    let row_bytes = clone_bytes
        .saturating_add(logical_path.len())
        .saturating_add(symbol_field_bytes)
        .saturating_add(normalized_text_bytes)
        .saturating_add(9usize.saturating_mul(std::mem::size_of::<(LexicalFieldV1, usize)>()));
    let serialized_bytes = row_bytes
        .saturating_add(field_bytes)
        .saturating_mul(6)
        .saturating_add(1_024);
    Ok(clone_bytes
        .saturating_add(field_bytes)
        .saturating_add(frequency_bytes)
        .saturating_add(normalized_scratch)
        .saturating_add(raw_scratch)
        .saturating_add(row_bytes)
        .saturating_add(serialized_bytes))
}

fn lexical_token_count(value: &str) -> usize {
    tracedecay_domain::technical_tokens(value).count()
}

fn chunk_owned_bytes(chunk: &CodeSearchChunkV1) -> usize {
    let subtoken_bytes = chunk.subtokens.iter().fold(
        chunk
            .subtokens
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
        |bytes, subtoken| bytes.saturating_add(subtoken.capacity()),
    );
    chunk
        .id
        .as_str()
        .len()
        .saturating_add(anchor_owned_bytes(&chunk.anchor))
        .saturating_add(chunk.content_digest.as_str().len())
        .saturating_add(chunk.language_descriptor_revision.as_str().len())
        .saturating_add(chunk.chunker_revision.as_str().len())
        .saturating_add(chunk.sanitizer_revision.as_str().len())
        .saturating_add(chunk.sensitivity.policy_revision.as_str().len())
        .saturating_add(exact_terms_owned_bytes(
            chunk.exact_terms.capacity(),
            &chunk.exact_terms,
        ))
        .saturating_add(subtoken_bytes)
        .saturating_add(chunk.sanitized_text.as_str().len())
}

fn exact_terms_owned_bytes(capacity: usize, terms: &[ExactTechnicalTermV1]) -> usize {
    terms.iter().fold(
        capacity.saturating_mul(std::mem::size_of::<ExactTechnicalTermV1>()),
        |bytes, term| {
            bytes
                .saturating_add(term.original_bytes().len())
                .saturating_add(term.canonical_bytes().len())
                .saturating_add(
                    term.symbol_occurrence_id()
                        .map_or(0, |occurrence| occurrence.as_str().len()),
                )
        },
    )
}

fn anchor_owned_bytes(anchor: &CodeSearchChunkAnchorV1) -> usize {
    anchor
        .generation_id
        .as_str()
        .len()
        .saturating_add(anchor.file_occurrence_id.as_str().len())
        .saturating_add(
            anchor
                .symbol_occurrence_id
                .as_ref()
                .map_or(0, |occurrence| occurrence.as_str().len()),
        )
        .saturating_add(
            anchor
                .parent_chunk_id
                .as_ref()
                .map_or(0, |parent| parent.as_str().len()),
        )
}

fn import_transient_bytes(
    evidence: &CodeIndexImportEvidenceV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    Ok(evidence
        .logical_path
        .len()
        .saturating_add(evidence.file_occurrence_id.as_str().len())
        .saturating_add(evidence.module_specifier.len())
        .saturating_add(evidence.imported_name.as_ref().map_or(0, String::len))
        .saturating_add(evidence.local_name.as_ref().map_or(0, String::len))
        .saturating_mul(6)
        .saturating_add(256))
}

fn validate_prepared_page_batch(
    current: &CodeLexicalArtifactBuildProgressV1,
    pages: &[PreparedCodeLexicalArtifactPageV1],
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut expected_ordinal = current.next_page_ordinal;
    let mut expected_document = current.completed_chunks;
    let mut expected_previous = current
        .next_cursor
        .as_ref()
        .map(encode_cursor)
        .transpose()?;
    for page in pages {
        if page.page_ordinal != expected_ordinal || page.previous_cursor != expected_previous {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "prepared lexical pages must continue the exact durable cursor in order".to_owned(),
            ));
        }
        if usize::try_from(page.chunk_count).map_err(contract_number)? < page.documents.len()
            || usize::try_from(page.import_count).map_err(contract_number)? != page.imports.len()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "prepared lexical page cardinality disagrees with its source receipt".to_owned(),
            ));
        }
        // A document keeps its source chunk ordinal; chunks the projection
        // does not admit leave gaps, never reorderings or out-of-page ids.
        let page_end = expected_document
            .checked_add(page.chunk_count)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical document count overflowed".to_owned(),
                )
            })?;
        let mut next_document = expected_document;
        for document in &page.documents {
            let document_id = u64::try_from(document.document_id).map_err(contract_number)?;
            if document_id < next_document || document_id >= page_end {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "prepared lexical document ids leave their page or repeat".to_owned(),
                ));
            }
            next_document = document_id + 1;
        }
        expected_document = page_end;
        let next_cursor = decode_cursor(&page.next_cursor)?;
        if next_cursor.next_page_ordinal()
            != expected_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical page ordinal overflowed".to_owned(),
                )
            })?
            || next_cursor.emitted_chunks() != expected_document
            || next_cursor.cumulative_digest() != &page.cumulative_digest
            || next_cursor.import_dictionary_digest() != &page.import_dictionary_digest
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "prepared lexical page receipt disagrees with its exact next cursor".to_owned(),
            ));
        }
        expected_ordinal = next_cursor.next_page_ordinal();
        expected_previous = Some(page.next_cursor.clone());
    }
    Ok(())
}

/// Buffers rows for one base table and flushes them as multi-row `INSERT`
/// statements: full statements of [`INSERT_ROWS_PER_STATEMENT`] rows while
/// rows keep arriving, one shorter statement for the remainder at `finish`.
/// Row order is preserved, so the clustered-key insert plans still append
/// at the tail of their trees.
struct MultiRowInsertV1<'transaction, 'row> {
    transaction: &'transaction Transaction<'transaction>,
    table_columns: &'static str,
    columns: usize,
    conflict_clause: &'static str,
    full_statement: rusqlite::CachedStatement<'transaction>,
    buffer: Vec<ToSqlOutput<'row>>,
    map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
}

impl<'transaction, 'row> MultiRowInsertV1<'transaction, 'row> {
    fn new(
        transaction: &'transaction Transaction<'transaction>,
        table_columns: &'static str,
        columns: usize,
        map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::new_with_conflict_clause(transaction, table_columns, columns, "", map_error)
    }

    /// Same as [`Self::new`], but every flushed statement carries
    /// `conflict_clause` (e.g. `" ON CONFLICT(payload_digest) DO NOTHING"`)
    /// after its `VALUES (...)` tuples, for tables whose rows may
    /// legitimately repeat an existing key (content-addressed dedup) rather
    /// than signal a bug.
    fn new_with_conflict_clause(
        transaction: &'transaction Transaction<'transaction>,
        table_columns: &'static str,
        columns: usize,
        conflict_clause: &'static str,
        map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let full_statement = transaction
            .prepare_cached(&multi_row_insert_sql(
                table_columns,
                columns,
                INSERT_ROWS_PER_STATEMENT,
                conflict_clause,
            ))
            .map_err(map_error)?;
        Ok(Self {
            transaction,
            table_columns,
            columns,
            conflict_clause,
            full_statement,
            buffer: Vec::with_capacity(columns * INSERT_ROWS_PER_STATEMENT),
            map_error,
        })
    }

    fn push(
        &mut self,
        values: impl IntoIterator<Item = ToSqlOutput<'row>>,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        let before = self.buffer.len();
        self.buffer.extend(values);
        if self.buffer.len() != before + self.columns {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact multi-row insert received the wrong column count".to_owned(),
            ));
        }
        if self.buffer.len() == self.columns * INSERT_ROWS_PER_STATEMENT {
            self.full_statement
                .execute(rusqlite::params_from_iter(self.buffer.iter()))
                .map_err(self.map_error)?;
            self.buffer.clear();
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), CodeLexicalArtifactErrorV1> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let rows = self.buffer.len() / self.columns;
        let mut tail = self
            .transaction
            .prepare_cached(&multi_row_insert_sql(
                self.table_columns,
                self.columns,
                rows,
                self.conflict_clause,
            ))
            .map_err(self.map_error)?;
        tail.execute(rusqlite::params_from_iter(self.buffer.iter()))
            .map_err(self.map_error)?;
        self.buffer.clear();
        Ok(())
    }
}

fn multi_row_insert_sql(
    table_columns: &str,
    columns: usize,
    rows: usize,
    conflict_clause: &str,
) -> String {
    let tuple = format!(
        "({})",
        std::iter::repeat_n("?", columns)
            .collect::<Vec<_>>()
            .join(", ")
    );
    format!(
        "INSERT INTO {table_columns} VALUES {}{conflict_clause}",
        std::iter::repeat_n(tuple.as_str(), rows)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sql_integer<'row>(value: i64) -> ToSqlOutput<'row> {
    ToSqlOutput::Owned(Value::Integer(value))
}

fn sql_text(value: &str) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes()))
}

fn sql_blob(value: &[u8]) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(ValueRef::Blob(value))
}

/// Digests checked against on-disk `clone_body_payloads` content in one
/// `IN (...)` query per chunk, instead of one `SELECT` per body. Five
/// columns of overhead at [`INSERT_ROWS_PER_STATEMENT`] rows stays well
/// clear of SQLite's 999-parameter floor for a single-column `IN` list too.
const PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK: usize = 512;

/// Verify that every payload digest about to be staged in this batch
/// agrees, byte for byte, with the payload already recorded under that
/// digest. Either earlier in this same batch (checked in memory, no I/O)
/// or in a prior batch already committed to `clone_body_payloads` (checked
/// with one batched `SELECT ... WHERE payload_digest IN (...)` per chunk of
/// unique digests, rather than a `SELECT` after every single insert). A
/// fresh digest with no prior occurrence anywhere needs no read at all: it
/// cannot conflict with content that was never stored.
fn verify_clone_payload_digests<'body>(
    transaction: &Transaction<'_>,
    bodies: &[&'body PreparedCloneBodyV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut staged: HashMap<&'body str, &'body [u8]> = HashMap::with_capacity(bodies.len());
    for body in bodies {
        checkpoint(control)?;
        match staged.entry(body.payload_digest.as_str()) {
            std::collections::hash_map::Entry::Occupied(existing) => {
                if *existing.get() != body.payload.as_slice() {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "clone payload digest collision".to_owned(),
                    ));
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(body.payload.as_slice());
            }
        }
    }
    let digests: Vec<&str> = staged.keys().copied().collect();
    let placeholders = std::iter::repeat_n("?", PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK)
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT payload_digest, payload FROM clone_body_payloads WHERE payload_digest IN ({placeholders})"
    );
    let mut statement = transaction.prepare_cached(&sql).map_err(sqlite_error)?;
    for chunk in digests.chunks(PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK) {
        checkpoint(control)?;
        let keys = chunk
            .iter()
            .map(|digest| stored_digest_key(digest))
            .collect::<Result<Vec<_>, _>>()?;
        let parameters = keys.iter().map(Some).chain(std::iter::repeat_n(
            None,
            PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK - chunk.len(),
        ));
        let mut rows = statement
            .query(rusqlite::params_from_iter(parameters))
            .map_err(sqlite_error)?;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            let key: Vec<u8> = row.get(0).map_err(sqlite_error)?;
            let digest = digest_from_key(&key)?;
            let stored: Vec<u8> = row.get(1).map_err(sqlite_error)?;
            if let Some(expected) = staged.get(digest.as_str())
                && *expected != stored.as_slice()
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone payload digest collision".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn append_prepared_clone_bodies(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let bodies: Vec<_> = pages
        .iter()
        .flat_map(|page| page.clone_bodies.iter())
        .collect();
    if !bodies.is_empty() {
        verify_clone_payload_digests(transaction, &bodies, control)?;
        let payload_keys = bodies
            .iter()
            .map(|body| stored_digest_key(&body.payload_digest))
            .collect::<Result<Vec<_>, _>>()?;
        let symbol_keys = bodies
            .iter()
            .map(|body| stored_symbol_key(&body.symbol_occurrence_id))
            .collect::<Vec<_>>();
        let exact_digests = bodies
            .iter()
            .map(|body| {
                body.exact_keys
                    .iter()
                    .map(|key| digest_key(&key.digest))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut payload_insert = MultiRowInsertV1::new_with_conflict_clause(
            transaction,
            "clone_body_payloads(payload_digest, payload)",
            2,
            " ON CONFLICT(payload_digest) DO NOTHING",
            sqlite_error,
        )?;
        for (body, key) in bodies.iter().zip(&payload_keys) {
            checkpoint(control)?;
            payload_insert.push([sql_blob(key), sql_blob(&body.payload)])?;
        }
        payload_insert.finish()?;
        // Occurrences name their payload, and postings their occurrence, by
        // the ordinal those rows were just assigned.
        let mut payload_ordinal = transaction
            .prepare_cached("SELECT ordinal FROM clone_body_payloads WHERE payload_digest = ?1")
            .map_err(sqlite_error)?;
        let mut occurrence_insert = MultiRowInsertV1::new(
            transaction,
            "clone_occurrences(symbol_key, payload_ordinal, path, body_start, body_end, eligibility)",
            6,
            sqlite_error,
        )?;
        for ((body, key), symbol_key) in bodies.iter().zip(&payload_keys).zip(&symbol_keys) {
            checkpoint(control)?;
            let ordinal: i64 = payload_ordinal
                .query_row([key.as_slice()], |row| row.get(0))
                .map_err(sqlite_error)?;
            occurrence_insert.push([
                ToSqlOutput::Borrowed(symbol_key.into()),
                sql_integer(ordinal),
                sql_text(&body.path),
                sql_integer(i64::try_from(body.body_start).map_err(contract_number)?),
                sql_integer(i64::try_from(body.body_end).map_err(contract_number)?),
                sql_blob(&body.eligibility),
            ])?;
        }
        occurrence_insert.finish()?;
        let mut occurrence_ordinal = transaction
            .prepare_cached("SELECT ordinal FROM clone_occurrences WHERE symbol_key = ?1")
            .map_err(sqlite_error)?;
        let mut posting_insert = MultiRowInsertV1::new(
            transaction,
            "clone_exact_postings(class, normalization_revision, digest, occurrence_ordinal)",
            4,
            sqlite_error,
        )?;
        for ((body, symbol_key), digests) in bodies.iter().zip(&symbol_keys).zip(&exact_digests) {
            checkpoint(control)?;
            if body.exact_keys.is_empty() {
                continue;
            }
            let ordinal: i64 = occurrence_ordinal
                .query_row([symbol_key], |row| row.get(0))
                .map_err(sqlite_error)?;
            for (key, digest) in body.exact_keys.iter().zip(digests) {
                posting_insert.push([
                    sql_integer(i64::from(key.class as u8)),
                    sql_integer(i64::from(key.normalization_revision)),
                    sql_blob(digest),
                    sql_integer(ordinal),
                ])?;
            }
        }
        posting_insert.finish()?;
    }
    append_prepared_clone_fingerprints(transaction, pages, control)
}

/// The key `clone_body_payloads.payload_digest` stores for `digest`.
fn stored_digest_key(digest: &str) -> Result<[u8; 32], CodeLexicalArtifactErrorV1> {
    digest_key(
        &ManifestDigest::new(digest.to_owned())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
    )
}

fn append_prepared_clone_fingerprints(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !pages.iter().any(|page| {
        page.clone_bodies
            .iter()
            .any(|body| body.fingerprint_stream.is_some())
    }) {
        return Ok(());
    }
    // Staged in arrival order, not into the keyed tree: fingerprints are
    // hashes, so a direct insert lands every row on a random leaf of
    // `clone_fingerprint_postings` and each batch commit rewrites (and
    // journals) most of that tree. Measured on the 1,560-file bench corpus:
    // 273k postings, 78 MiB final table, 2.1 GiB written through 28 commits
    // (27x amplification); staged and sorted once at finalization, 0.4 GiB.
    // Postings name their occurrence by its stored ordinal, which the
    // occurrence rows of this batch were just assigned.
    let mut ordinal = transaction
        .prepare_cached("SELECT ordinal FROM clone_occurrences WHERE symbol_key = ?1")
        .map_err(sqlite_error)?;
    let mut insert = MultiRowInsertV1::new(
        transaction,
        "clone_fingerprint_postings_pages(language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position)",
        6,
        sqlite_error,
    )?;
    for body in pages.iter().flat_map(|page| &page.clone_bodies) {
        checkpoint(control)?;
        let Some(stream) = &body.fingerprint_stream else {
            continue;
        };
        let occurrence: i64 = ordinal
            .query_row([stored_symbol_key(&body.symbol_occurrence_id)], |row| row.get(0))
            .map_err(sqlite_error)?;
        for position in &stream.positions {
            insert.push([
                sql_text(&stream.language),
                sql_integer(i64::from(stream.class as u8)),
                sql_integer(i64::from(stream.normalization_revision)),
                sql_integer(i64::try_from(position.fingerprint).map_err(contract_number)?),
                sql_integer(occurrence),
                sql_integer(i64::from(position.token_position)),
            ])?;
        }
    }
    insert.finish()
}

fn append_prepared_imports(
    transaction: &Transaction<'_>,
    page: &PreparedCodeLexicalArtifactPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    // The canonical encoding is the evidence itself, and its integrity digest
    // is a pure function of it, so the key is the only stored column.
    let mut evidence = transaction
        .prepare_cached("INSERT INTO import_evidence(canonical) VALUES (?1)")
        .map_err(sqlite_error)?;
    for import in &page.imports {
        checkpoint(control)?;
        evidence
            .execute(params![import.canonical.as_slice()])
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    }
    Ok(())
}

/// `(language, class, normalization revision, fingerprint)` of one sealed
/// fingerprint list.
type FingerprintKeyV1 = (String, i64, i64, i64);

/// Merge the staged fingerprint postings into one sealed list per
/// `(language, class, normalization revision, fingerprint)` in a single
/// sorted pass, so the keyed tree is written sequentially once, then drop the
/// staging table. The keyed table keeps its private-builder insert gate, so
/// the pass holds the mutation authority exactly as a batch append does.
fn derive_clone_fingerprint_postings(
    transaction: &Transaction<'_>,
    mutation_gate: &Arc<AtomicU8>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
    let mut select = transaction
        .prepare(
            "SELECT language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position
             FROM clone_fingerprint_postings_pages
             ORDER BY language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position",
        )
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare(
            "INSERT INTO clone_fingerprint_postings(language, class, normalization_revision, fingerprint, posting_count, postings) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(sqlite_error)?;
    let mut seal = |(language, class, revision, fingerprint): &FingerprintKeyV1,
                    postings: &[(u32, u32)]|
     -> Result<(), CodeLexicalArtifactErrorV1> {
        insert
            .execute(params![
                language,
                class,
                revision,
                fingerprint,
                i64::try_from(postings.len()).map_err(contract_number)?,
                encode_fingerprint_postings(postings)?,
            ])
            .map_err(sqlite_error)?;
        Ok(())
    };
    let mut rows = select.query([]).map_err(sqlite_error)?;
    let mut current: Option<(FingerprintKeyV1, Vec<(u32, u32)>)> = None;
    let mut visited = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if visited.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        visited += 1;
        let key = (
            row.get::<_, String>(0).map_err(sqlite_error)?,
            row.get::<_, i64>(1).map_err(sqlite_error)?,
            row.get::<_, i64>(2).map_err(sqlite_error)?,
            row.get::<_, i64>(3).map_err(sqlite_error)?,
        );
        let occurrence =
            u32::try_from(row.get::<_, i64>(4).map_err(sqlite_error)?).map_err(contract_number)?;
        let position =
            u32::try_from(row.get::<_, i64>(5).map_err(sqlite_error)?).map_err(contract_number)?;
        if let Some((sealed, postings)) = current.take_if(|(current, _)| *current != key) {
            seal(&sealed, &postings)?;
        }
        current
            .get_or_insert_with(|| (key, Vec::new()))
            .1
            .push((occurrence, position));
    }
    if let Some((sealed, postings)) = current {
        seal(&sealed, &postings)?;
    }
    drop(rows);
    drop(select);
    drop(insert);
    transaction
        .execute_batch("DROP TABLE clone_fingerprint_postings_pages;")
        .map_err(sqlite_error)
}

/// Append one posting list per `(term, field)` and per `(exact term, field)`
/// covering this batch, keyed by the batch's first page so every batch lands
/// at the tail of its run table. Finalization concatenates the runs of each
/// key in page order; n-gram lists are rebuilt from the stored rows instead.
fn append_prepared_postings(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    term_insert_plan: &PreparedTermInsertPlanV1<'_>,
    exact_insert_plan: &PreparedExactInsertPlanV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let batch_page = pages
        .first()
        .map(|page| i64::try_from(page.page_ordinal).map_err(contract_number))
        .transpose()?
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact posting batch has no pages".to_owned(),
            )
        })?;
    hotpath::measure_block!(
        "query.artifact.batch.postings.intern_exact",
        intern_exact_terms(transaction, &exact_insert_plan.interned_terms, control)
    )?;
    let mut term_insert = MultiRowInsertV1::new(
        transaction,
        "term_posting_runs(page_ordinal, term, field, postings)",
        4,
        sqlite_error,
    )?;
    // Plain INSERT, not `INSERT OR IGNORE`: every prepared document's exact
    // postings are deduplicated into a `BTreeSet<(field, term)>`
    // (`prepared.rs::prepare_document`), and `validate_prepared_page_batch`
    // admits only the fresh contiguous suffix of pages, so each
    // `(batch page, term, field)` run key is globally unique and a conflict
    // can only be a real corruption bug.
    let mut exact_insert = MultiRowInsertV1::new(
        transaction,
        "exact_posting_runs(page_ordinal, term_id, field, documents)",
        4,
        sqlite_error,
    )?;
    let mut field_totals = BTreeMap::new();
    hotpath::measure_block!("query.artifact.batch.postings.term_rows", {
        let mut run: Option<((&str, i64), PostingListEncoderV1)> = None;
        for (index, entry) in term_insert_plan.entries.iter().enumerate() {
            if index.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
                checkpoint(control)?;
            }
            let (term, field, document_id) = entry.key();
            if let Some(((term, field), encoder)) = run.take_if(|(key, _)| *key != (term, field)) {
                push_posting_run(&mut term_insert, batch_page, sql_text(term), field, encoder)?;
            }
            run.get_or_insert_with(|| ((term, field), PostingListEncoderV1::new(true)))
                .1
                .push(
                    u32::try_from(document_id).map_err(contract_number)?,
                    u32::try_from(entry.posting.frequency).map_err(contract_number)?,
                )?;
            let total: &mut i64 = field_totals.entry(field).or_default();
            *total = total.checked_add(entry.posting.frequency).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact field total overflowed".to_owned(),
                )
            })?;
        }
        if let Some(((term, field), encoder)) = run {
            push_posting_run(&mut term_insert, batch_page, sql_text(term), field, encoder)?;
        }
        term_insert.finish()
    })?;
    hotpath::measure_block!(
        "query.artifact.batch.postings.field_totals",
        stage_field_totals(transaction, &field_totals)
    )?;
    hotpath::measure_block!("query.artifact.batch.postings.exact_rows", {
        let mut run: Option<((i64, i64), PostingListEncoderV1)> = None;
        for (index, entry) in exact_insert_plan.entries.iter().enumerate() {
            if index.is_multiple_of(EXACT_INSERT_CONTROL_INTERVAL) {
                checkpoint(control)?;
            }
            let key = (entry.term_id, entry.field_code);
            if let Some(((term_id, field), encoder)) = run.take_if(|(run_key, _)| *run_key != key) {
                push_posting_run(
                    &mut exact_insert,
                    batch_page,
                    sql_integer(term_id),
                    field,
                    encoder,
                )?;
            }
            run.get_or_insert_with(|| (key, PostingListEncoderV1::new(false)))
                .1
                .push(
                    u32::try_from(entry.document_id).map_err(contract_number)?,
                    1,
                )?;
        }
        if let Some(((term_id, field), encoder)) = run {
            push_posting_run(
                &mut exact_insert,
                batch_page,
                sql_integer(term_id),
                field,
                encoder,
            )?;
        }
        exact_insert.finish()
    })?;
    Ok(())
}

fn push_posting_run<'a>(
    insert: &mut MultiRowInsertV1<'_, 'a>,
    batch_page: i64,
    term: ToSqlOutput<'a>,
    field: i64,
    encoder: PostingListEncoderV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    insert.push([
        sql_integer(batch_page),
        term,
        sql_integer(field),
        ToSqlOutput::Owned(Value::Blob(encoder.finish()?)),
    ])
}

fn append_prepared_rows(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut block_insert = MultiRowInsertV1::new(
        transaction,
        "row_blocks(first_document, payload)",
        2,
        |error| CodeLexicalArtifactErrorV1::Contract(error.to_string()),
    )?;
    for page in pages {
        for (first_document, payload) in &page.row_blocks {
            checkpoint(control)?;
            block_insert.push([sql_integer(*first_document), sql_blob(payload)])?;
        }
    }
    block_insert.finish()?;
    let mut chunk_insert = MultiRowInsertV1::new(
        transaction,
        "row_chunk_pages(document_id, chunk_id)",
        2,
        |error| CodeLexicalArtifactErrorV1::Contract(error.to_string()),
    )?;
    for page in pages {
        for document in &page.documents {
            checkpoint(control)?;
            chunk_insert.push([
                sql_integer(document.document_id),
                ToSqlOutput::Owned(stored_chunk_key(&document.chunk_id)),
            ])?;
        }
    }
    chunk_insert.finish()
}

fn insert_prepared_source_page(
    transaction: &Transaction<'_>,
    page: &PreparedCodeLexicalArtifactPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let page_ordinal = i64::try_from(page.page_ordinal).map_err(contract_number)?;
    transaction
        .prepare_cached(
            "INSERT INTO source_pages(page_ordinal, chunk_count, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(sqlite_error)?
        .execute(params![
            page_ordinal,
            i64::try_from(page.chunk_count).map_err(contract_number)?,
            i64::try_from(page.import_count).map_err(contract_number)?,
            i64::try_from(page.import_payload_bytes).map_err(contract_number)?,
            page.import_dictionary_digest.as_str(),
            page.ngram_digest.as_str(),
            page.base_sections_receipt.as_slice(),
        ])
        .map_err(sqlite_error)?;
    transaction
        .prepare_cached(
            "INSERT INTO source_page_cursors(page_ordinal, page_digest, cumulative_digest, payload_bytes, next_cursor) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sqlite_error)?
        .execute(params![
            page_ordinal,
            page.page_digest.as_str(),
            page.cumulative_digest.as_str(),
            i64::try_from(page.payload_bytes).map_err(contract_number)?,
            page.next_cursor.as_slice(),
        ])
        .map_err(sqlite_error)?;
    Ok(())
}

pub(super) fn sqlite_file_size(connection: &Connection) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(sqlite_error)?;
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(sqlite_error)?;
    let page_count = u64::try_from(page_count).map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact page count is negative or exceeds u64".to_owned(),
        )
    })?;
    let page_size = u64::try_from(page_size).map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact page size is negative or exceeds u64".to_owned(),
        )
    })?;
    page_count.checked_mul(page_size).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract("lexical artifact file size overflowed".to_owned())
    })
}

fn create_schema(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
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
            CREATE TABLE finalization_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                state BLOB NOT NULL
            );
            CREATE TABLE content_epoch (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                epoch INTEGER NOT NULL CHECK(epoch >= 0)
            );
            INSERT INTO content_epoch(singleton, epoch) VALUES (1, 0);
            CREATE TABLE source_pages (
                page_ordinal INTEGER PRIMARY KEY,
                chunk_count INTEGER NOT NULL,
                import_count INTEGER NOT NULL,
                import_payload_bytes INTEGER NOT NULL,
                import_dictionary_digest TEXT NOT NULL,
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
            CREATE TABLE import_evidence (
                canonical BLOB NOT NULL PRIMARY KEY
            ) WITHOUT ROWID;
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
                entry BLOB NOT NULL
            );
            CREATE TABLE row_dictionary_pages (
                page_ordinal INTEGER NOT NULL,
                entry_id INTEGER NOT NULL,
                entry BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, entry_id)
            ) WITHOUT ROWID;
            CREATE TRIGGER content_epoch_source_pages_insert AFTER INSERT ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_row_chunk_pages_insert AFTER INSERT ON row_chunk_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_evidence_insert AFTER INSERT ON import_evidence BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER immutable_source_pages_update BEFORE UPDATE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_source_pages_delete BEFORE DELETE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_source_page_cursors_update BEFORE UPDATE ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'immutable lexical source page cursors'); END;
            CREATE TRIGGER immutable_source_page_cursors_delete BEFORE DELETE ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'immutable lexical source page cursors'); END;
            CREATE TRIGGER immutable_import_evidence_update BEFORE UPDATE ON import_evidence BEGIN SELECT RAISE(ABORT, 'immutable lexical import evidence'); END;
            CREATE TRIGGER immutable_import_evidence_delete BEFORE DELETE ON import_evidence BEGIN SELECT RAISE(ABORT, 'immutable lexical import evidence'); END;
            CREATE TRIGGER immutable_ngram_postings_update BEFORE UPDATE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER immutable_ngram_postings_delete BEFORE DELETE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER immutable_exact_vocabulary_update BEFORE UPDATE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'immutable lexical exact vocabulary'); END;
            CREATE TRIGGER immutable_exact_vocabulary_delete BEFORE DELETE ON exact_vocabulary BEGIN SELECT RAISE(ABORT, 'immutable lexical exact vocabulary'); END;
            CREATE TRIGGER immutable_row_dictionary_pages_update BEFORE UPDATE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical row dictionary pages'); END;
            CREATE TRIGGER immutable_row_dictionary_pages_delete BEFORE DELETE ON row_dictionary_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical row dictionary pages'); END;
            CREATE TRIGGER builder_gate_source_pages_insert BEFORE INSERT ON source_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_source_page_cursors_insert BEFORE INSERT ON source_page_cursors WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_import_evidence_insert BEFORE INSERT ON import_evidence WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
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
                CREATE TABLE clone_fingerprint_postings (
                    language TEXT NOT NULL,
                    class INTEGER NOT NULL,
                    normalization_revision INTEGER NOT NULL,
                    fingerprint INTEGER NOT NULL,
                    posting_count INTEGER NOT NULL CHECK(posting_count > 0),
                    postings BLOB NOT NULL,
                    PRIMARY KEY(language, class, normalization_revision, fingerprint)
                ) WITHOUT ROWID;
                CREATE TABLE clone_fingerprint_postings_pages (
                    language TEXT NOT NULL,
                    class INTEGER NOT NULL,
                    normalization_revision INTEGER NOT NULL,
                    fingerprint INTEGER NOT NULL,
                    occurrence_ordinal INTEGER NOT NULL,
                    token_position INTEGER NOT NULL
                );
                CREATE TRIGGER builder_gate_clone_fingerprint_postings_insert BEFORE INSERT ON clone_fingerprint_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER builder_gate_clone_fingerprint_postings_pages_insert BEFORE INSERT ON clone_fingerprint_postings_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER builder_gate_clone_fingerprint_postings_pages_update BEFORE UPDATE ON clone_fingerprint_postings_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER builder_gate_clone_fingerprint_postings_pages_delete BEFORE DELETE ON clone_fingerprint_postings_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
                CREATE TRIGGER immutable_clone_fingerprint_postings_update BEFORE UPDATE ON clone_fingerprint_postings BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint postings'); END;
                CREATE TRIGGER immutable_clone_fingerprint_postings_delete BEFORE DELETE ON clone_fingerprint_postings BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint postings'); END;
                CREATE TRIGGER immutable_clone_fingerprint_postings_pages_update BEFORE UPDATE ON clone_fingerprint_postings_pages BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint posting pages'); END;
                CREATE TRIGGER immutable_clone_fingerprint_postings_pages_delete BEFORE DELETE ON clone_fingerprint_postings_pages BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint posting pages'); END;
                ",
        )
        .map_err(sqlite_error)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(sqlite_error)
}

fn verify_builder_mutation_gate_schema(
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
    let has_clone_fingerprints = table_exists(connection, "clone_fingerprint_postings")?;
    let has_clone_fingerprint_pages = table_exists(connection, "clone_fingerprint_postings_pages")?;
    let has_source_page_cursors = table_exists(connection, "source_page_cursors")?;
    let gated_layouts: [(bool, &[GateTriggerLayoutV1]); 10] = [
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
        (
            has_clone_fingerprints,
            &CLONE_FINGERPRINT_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
        (
            has_clone_fingerprint_pages,
            &CLONE_FINGERPRINT_PAGES_BUILDER_GATE_TRIGGER_LAYOUT,
        ),
    ];
    let immutable_layouts: [(bool, &[ImmutableTriggerLayoutV1]); 7] = [
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
        (
            has_clone_fingerprints,
            &CLONE_FINGERPRINT_IMMUTABLE_TRIGGER_LAYOUT,
        ),
        (
            has_clone_fingerprint_pages,
            &CLONE_FINGERPRINT_PAGES_IMMUTABLE_TRIGGER_LAYOUT,
        ),
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

fn install_base_freeze(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(
            "
            CREATE TRIGGER frozen_source_pages_insert BEFORE INSERT ON source_pages BEGIN SELECT RAISE(ABORT, 'frozen lexical source pages'); END;
            CREATE TRIGGER frozen_source_page_cursors_insert BEFORE INSERT ON source_page_cursors BEGIN SELECT RAISE(ABORT, 'frozen lexical source pages'); END;
            CREATE TRIGGER frozen_import_evidence_insert BEFORE INSERT ON import_evidence BEGIN SELECT RAISE(ABORT, 'frozen lexical import evidence'); END;
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
            CREATE TRIGGER frozen_clone_fingerprint_postings_insert BEFORE INSERT ON clone_fingerprint_postings BEGIN SELECT RAISE(ABORT, 'frozen clone fingerprint postings'); END;
            ",
        )
        .map_err(sqlite_error)
}

fn authenticated_authority_epoch(
    transaction: &Transaction<'_>,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<i64, CodeLexicalArtifactErrorV1> {
    verify_builder_mutation_gate_schema(transaction)?;
    let (pages, documents, imports): (i64, i64, i64) = transaction
        .query_row(
            "SELECT (SELECT COUNT(*) FROM source_pages), (SELECT COUNT(*) FROM row_chunk_pages), (SELECT COUNT(*) FROM import_evidence)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sqlite_error)?;
    let expected_epoch = pages
        .checked_add(documents)
        .and_then(|count| count.checked_add(imports))
        .ok_or_else(|| {
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
        || u64::try_from(imports).ok() != Some(source.total_imports())
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact authenticated authority disagrees with its source receipt".to_owned(),
        ));
    }
    verify_clone_rows(transaction, source, control)?;
    Ok(actual_epoch)
}

/// Documents the staged pages admitted, as their base-section receipts
/// count them: every source chunk except those the projection does not
/// index (annotation uses).
fn admitted_documents(connection: &Connection) -> Result<u64, CodeLexicalArtifactErrorV1> {
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

fn verify_clone_rows(
    connection: &Connection,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let (occurrences, missing_payloads, orphan_payloads, dangling_postings): (
        i64,
        i64,
        i64,
        i64,
    ) = connection
        .query_row(
            "SELECT
             (SELECT COUNT(*) FROM clone_occurrences),
             (SELECT COUNT(*) FROM clone_occurrences AS occurrence LEFT JOIN clone_body_payloads AS payload ON payload.ordinal = occurrence.payload_ordinal WHERE payload.ordinal IS NULL),
             (SELECT COUNT(*) FROM clone_body_payloads WHERE ordinal NOT IN (SELECT payload_ordinal FROM clone_occurrences)),
             (SELECT COUNT(*) FROM clone_exact_postings AS posting LEFT JOIN clone_occurrences AS occurrence ON occurrence.ordinal = posting.occurrence_ordinal WHERE occurrence.ordinal IS NULL)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sqlite_error)?;
    if u64::try_from(occurrences).map_err(contract_number)? != source.total_clone_bodies()
        || missing_payloads != 0
        || orphan_payloads != 0
        || dangling_postings != 0
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone rows disagree with their source receipt or payload bindings".to_owned(),
        ));
    }
    // Every fingerprint posting names a stored occurrence, and each list's
    // stored count is its length. The read path re-derives each candidate's
    // selected positions from its canonical payload before trusting one.
    let mut occurrence_ordinals = HashSet::new();
    let mut statement = connection
        .prepare("SELECT ordinal FROM clone_occurrences")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        occurrence_ordinals.insert(row.get::<_, i64>(0).map_err(sqlite_error)?);
    }
    drop(rows);
    drop(statement);
    let mut statement = connection
        .prepare("SELECT posting_count, postings FROM clone_fingerprint_postings")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut visited = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if visited.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        visited += 1;
        let count: i64 = row.get(0).map_err(sqlite_error)?;
        let encoded = row
            .get_ref(1)
            .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
            .map_err(sqlite_corrupt)?;
        let postings = decode_fingerprint_postings(encoded)?;
        if usize::try_from(count).ok() != Some(postings.len())
            || postings
                .iter()
                .any(|(occurrence, _)| !occurrence_ordinals.contains(&i64::from(*occurrence)))
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone fingerprint postings disagree with their occurrences or stored counts"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn advance_pre_digest_work(
    transaction: &Transaction<'_>,
    state: &mut PersistedFinalizationStateV1,
    authority: &ServingIndexStepAuthorityV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    match state.phase {
        PersistedFinalizationPhaseV1::Statistics => {
            with_cancellable_sqlite_statement(transaction, control, || {
                derive_statistics_step(transaction, state.section_ordinal, control)?;
                Ok(())
            })?;
            state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact statistics step overflowed".to_owned(),
                )
            })?;
            if state.section_ordinal == STATISTICS_STEP_COUNT_V11 {
                enter_digest_phase(state)?;
            }
        }
        PersistedFinalizationPhaseV1::Indexes => {
            with_cancellable_sqlite_statement(transaction, control, || {
                build_serving_index_step(transaction, state.section_ordinal, authority, control)?;
                Ok(())
            })?;
            state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact serving-index step overflowed".to_owned(),
                )
            })?;
            if state.section_ordinal == SERVING_INDEX_STEP_COUNT_V11 {
                state.phase = PersistedFinalizationPhaseV1::Statistics;
                state.section_ordinal = 0;
            }
        }
        PersistedFinalizationPhaseV1::Digest => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact selected pre-digest work after entering digest verification"
                    .to_owned(),
            ));
        }
    }
    checkpoint(control)?;
    Ok(())
}

fn enter_digest_phase(
    state: &mut PersistedFinalizationStateV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    state.phase = PersistedFinalizationPhaseV1::Digest;
    state.section_ordinal = 0;
    state.section_accumulator = initial_section_accumulator(SECTION_NAMES[0])?.to_vec();
    Ok(())
}

fn with_cancellable_sqlite_statement<T>(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
    operation: impl FnOnce() -> Result<T, CodeLexicalArtifactErrorV1>,
) -> Result<T, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let interruption = Arc::new(AtomicU8::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let progress_interruption = Arc::clone(&interruption);
    connection
        .progress_handler(
            FINALIZATION_PROGRESS_INTERVAL_OPS,
            Some(move || progress_interruption.load(Ordering::Acquire) != 0),
        )
        .map_err(sqlite_error)?;

    let monitored = std::thread::scope(|scope| {
        let monitor_interruption = Arc::clone(&interruption);
        let monitor_finished = Arc::clone(&finished);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(0);
        let monitor = spawn_finalization_control_monitor(scope, move || {
            let mut ready = false;
            loop {
                let reason = if control.is_cancelled() {
                    1
                } else if control.is_deadline_exceeded() {
                    2
                } else {
                    0
                };
                if reason != 0 {
                    monitor_interruption.store(reason, Ordering::Release);
                }
                if !ready {
                    let _ = ready_sender.send(());
                    ready = true;
                }
                if reason != 0 || monitor_finished.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(FINALIZATION_CONTROL_POLL_INTERVAL);
            }
        })?;
        let readiness = ready_receiver.recv();
        let outcome = readiness
            .as_ref()
            .ok()
            .map(|_| catch_unwind(AssertUnwindSafe(operation)));
        finished.store(true, Ordering::Release);
        Ok::<_, std::io::Error>((readiness, outcome, monitor.join()))
    });
    let clear = connection
        .progress_handler(FINALIZATION_PROGRESS_INTERVAL_OPS, None::<fn() -> bool>)
        .map_err(sqlite_error);
    clear?;

    let (readiness, outcome, monitor) = monitored.map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "lexical artifact finalization cancellation monitor could not start: {error}"
        ))
    })?;
    if let Err(payload) = monitor {
        return Err(CodeLexicalArtifactErrorV1::Io(
            tracedecay_code_index::parallelism::CodeIndexParallelismErrorV1::from_panic_payload(
                0, &*payload,
            )
            .to_string(),
        ));
    }
    readiness.map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "lexical artifact finalization cancellation monitor stopped before readiness: {error}"
        ))
    })?;

    let outcome = match outcome.ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Io(
            "lexical artifact finalization cancellation monitor produced no operation outcome"
                .to_owned(),
        )
    })? {
        Ok(outcome) => outcome,
        Err(payload) => resume_unwind(payload),
    };
    match interruption.load(Ordering::Acquire) {
        1 => Err(CodeLexicalArtifactErrorV1::Interrupted(
            tracedecay_code_index::production::CodeIndexInterruptionV1::Cancelled,
        )),
        2 => Err(CodeLexicalArtifactErrorV1::Interrupted(
            tracedecay_code_index::production::CodeIndexInterruptionV1::DeadlineExceeded,
        )),
        _ => outcome,
    }
}

fn spawn_finalization_control_monitor<'scope, 'environment>(
    scope: &'scope std::thread::Scope<'scope, 'environment>,
    monitor: impl FnOnce() + Send + 'scope,
) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, ()>>
where
    'environment: 'scope,
{
    #[cfg(test)]
    if FAIL_NEXT_FINALIZATION_MONITOR_SPAWN.with(std::cell::Cell::take) {
        return Err(std::io::Error::other(
            "injected finalization monitor spawn failure",
        ));
    }
    std::thread::Builder::new()
        .name("tracedecay-lexical-finalization-control".to_owned())
        .spawn_scoped(scope, monitor)
}

/// Fold one committed batch's per-field posting lengths into
/// `field_stats_staging`, inside that batch's transaction, so sealing
/// `field_stats` never rescans `term_postings`.
fn stage_field_totals(
    transaction: &Transaction<'_>,
    totals: &BTreeMap<i64, i64>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut upsert = transaction
        .prepare_cached(
            "INSERT INTO field_stats_staging(field, total_length) VALUES (?1, ?2)
             ON CONFLICT(field) DO UPDATE SET total_length = total_length + excluded.total_length",
        )
        .map_err(sqlite_error)?;
    for (field, total) in totals {
        upsert.execute([*field, *total]).map_err(sqlite_error)?;
    }
    Ok(())
}

fn derive_statistics_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    match ordinal {
        0 => hotpath::measure_block!("query.artifact.finalization.derive_field_stats", {
            // Every committed batch already folded its posting lengths into
            // the staging totals (`stage_field_totals`): sealing copies at
            // most nine rows and drops the staging table with its gates.
            transaction.execute_batch(
                "INSERT INTO field_stats(field, total_length) SELECT field, total_length FROM field_stats_staging ORDER BY field;
                 DROP TABLE field_stats_staging;
                 CREATE TRIGGER frozen_field_stats_insert BEFORE INSERT ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;
                 CREATE TRIGGER frozen_field_stats_update BEFORE UPDATE ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;
                 CREATE TRIGGER frozen_field_stats_delete BEFORE DELETE ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;",
            )
        }),
        // Every staging table is gone; return its pages to the filesystem
        // before the digest and the sealed size.
        1 => hotpath::measure_block!("query.artifact.finalization.release_staging_pages", {
            // The staged per-page cursors bind the building worktree's
            // source state; the finalization state keeps the terminal one
            // until the seal.
            transaction
                .execute_batch("DROP TABLE source_page_cursors;")
                .map_err(sqlite_error)?;
            release_free_pages(transaction, control)?;
            Ok(())
        }),
        _ => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact selected an unknown statistics step".to_owned(),
            ));
        }
    }
    .map_err(sqlite_error)?;
    Ok(())
}

/// Return every free page to the filesystem. The pragma yields one row per
/// released page, so it must be stepped to completion.
fn release_free_pages(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut statement = transaction
        .prepare("PRAGMA incremental_vacuum")
        .map_err(sqlite_error)?;
    let mut released = statement.query([]).map_err(sqlite_error)?;
    let mut pages = 0usize;
    while released.next().map_err(sqlite_error)?.is_some() {
        if pages.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        pages += 1;
    }
    Ok(())
}

/// What the pre-digest index steps need beyond the transaction: the
/// builder's mutation authority, the generation rows decode under, and the
/// memory the n-gram rebuild may hold at once.
struct ServingIndexStepAuthorityV1<'a> {
    mutation_gate: &'a Arc<AtomicU8>,
    generation: &'a CodeGenerationId,
    ngram_memory_bytes: usize,
}

fn build_serving_index_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
    authority: &ServingIndexStepAuthorityV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mutation_gate = authority.mutation_gate;
    match ordinal {
        0 => hotpath::measure_block!("query.artifact.finalization.index.row_chunks", {
            derive_row_dictionary(transaction)?;
            let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
            transaction
                .execute_batch(
                    "INSERT INTO row_chunks(chunk_id, document_id) SELECT chunk_id, document_id FROM row_chunk_pages ORDER BY chunk_id;
                     DROP TABLE row_chunk_pages;
                     CREATE TRIGGER frozen_row_chunks_insert BEFORE INSERT ON row_chunks BEGIN SELECT RAISE(ABORT, 'frozen lexical row chunks'); END;",
                )
                .map_err(sqlite_error)
        }),
        1 => hotpath::measure_block!(
            "query.artifact.finalization.merge.term_postings",
            derive_term_postings(transaction, mutation_gate, control)
        ),
        2 => hotpath::measure_block!(
            "query.artifact.finalization.merge.exact_postings",
            derive_exact_postings(transaction, mutation_gate, control)
        ),
        // Last, so its lists reuse the pages the dropped runs freed.
        3 => hotpath::measure_block!(
            "query.artifact.finalization.derive.ngram_postings",
            derive_ngram_postings(transaction, authority, control)
        ),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact selected an unknown serving-index step".to_owned(),
        )),
    }
}

/// Merge the term staging runs into one sealed `term_postings` row per term
/// in a single sorted pass: every field's list concatenates that key's runs
/// in page order, re-verified (canonical encoding, ascending documents
/// across runs) on the way. A term is fuzzy-eligible when it occurs in any
/// field but the subtoken field.
fn derive_term_postings(
    transaction: &Transaction<'_>,
    mutation_gate: &Arc<AtomicU8>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
    let subtoken = field_code(LexicalFieldV1::Subtoken);
    let mut select = transaction
        .prepare(
            "SELECT term, field, postings FROM term_posting_runs ORDER BY term, field, page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare("INSERT INTO term_postings(term, in_fuzzy, lists) VALUES (?1, ?2, ?3)")
        .map_err(sqlite_error)?;
    let mut seal = |term: &str,
                    lists: Vec<(i64, PostingListEncoderV1)>|
     -> Result<(), CodeLexicalArtifactErrorV1> {
        let in_fuzzy = lists.iter().any(|(field, _)| *field != subtoken);
        let lists = lists
            .into_iter()
            .map(|(field, encoder)| Ok((field, encoder.len(), encoder.finish()?)))
            .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()?;
        insert
            .execute(params![term, in_fuzzy, encode_term_lists(&lists)?])
            .map_err(sqlite_error)?;
        Ok(())
    };
    let mut rows = select.query([]).map_err(sqlite_error)?;
    let mut current: Option<(String, Vec<(i64, PostingListEncoderV1)>)> = None;
    let mut visited = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if visited.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        visited += 1;
        let term = row
            .get_ref(0)
            .and_then(|value| value.as_str().map_err(rusqlite::Error::from))
            .map_err(sqlite_corrupt)?;
        let field: i64 = row.get(1).map_err(sqlite_error)?;
        let staged = row
            .get_ref(2)
            .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
            .map_err(sqlite_corrupt)?;
        if let Some((sealed, lists)) = current.take_if(|(current, _)| current != term) {
            seal(&sealed, lists)?;
        }
        let lists = &mut current
            .get_or_insert_with(|| (term.to_owned(), Vec::new()))
            .1;
        if lists.last().is_none_or(|(last, _)| *last != field) {
            lists.push((field, PostingListEncoderV1::new(true)));
        }
        let encoder = lists
            .last_mut()
            .map(|(_, encoder)| encoder)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact term merge lost its field list".to_owned(),
                )
            })?;
        append_staged_run(encoder, staged, true)?;
    }
    if let Some((sealed, lists)) = current {
        seal(&sealed, lists)?;
    }
    drop(rows);
    drop(select);
    drop(insert);
    checkpoint(control)?;
    transaction
        .execute_batch(
            "DROP TABLE term_posting_runs;
             CREATE TRIGGER frozen_term_postings_insert BEFORE INSERT ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;
             CREATE TRIGGER frozen_term_postings_update BEFORE UPDATE ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;
             CREATE TRIGGER frozen_term_postings_delete BEFORE DELETE ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;",
        )
        .map_err(sqlite_error)
}

/// Merge the exact staging runs into one sealed list per `(term, field)`
/// the same way.
fn derive_exact_postings(
    transaction: &Transaction<'_>,
    mutation_gate: &Arc<AtomicU8>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
    let mut select = transaction
        .prepare(
            "SELECT term_id, field, documents FROM exact_posting_runs ORDER BY term_id, field, page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare("INSERT INTO exact_postings(term_id, field, documents) VALUES (?1, ?2, ?3)")
        .map_err(sqlite_error)?;
    let mut rows = select.query([]).map_err(sqlite_error)?;
    let mut list: Option<((i64, i64), PostingListEncoderV1)> = None;
    let mut visited = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if visited.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        visited += 1;
        let key: (i64, i64) = (
            row.get(0).map_err(sqlite_error)?,
            row.get(1).map_err(sqlite_error)?,
        );
        let staged = row
            .get_ref(2)
            .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
            .map_err(sqlite_corrupt)?;
        if let Some(((term_id, field), encoder)) = list.take_if(|(list_key, _)| *list_key != key) {
            insert
                .execute(params![term_id, field, encoder.finish()?])
                .map_err(sqlite_error)?;
        }
        let encoder = &mut list
            .get_or_insert_with(|| (key, PostingListEncoderV1::new(false)))
            .1;
        append_staged_run(encoder, staged, false)?;
    }
    if let Some(((term_id, field), encoder)) = list {
        insert
            .execute(params![term_id, field, encoder.finish()?])
            .map_err(sqlite_error)?;
    }
    drop(rows);
    drop(select);
    drop(insert);
    checkpoint(control)?;
    transaction
        .execute_batch(
            "DROP TABLE exact_posting_runs;
             CREATE TRIGGER frozen_exact_postings_insert BEFORE INSERT ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;
             CREATE TRIGGER frozen_exact_postings_update BEFORE UPDATE ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;
             CREATE TRIGGER frozen_exact_postings_delete BEFORE DELETE ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;",
        )
        .map_err(sqlite_error)
}

/// Append one staged run to its key's list; a run must be non-empty and
/// continue strictly after the documents already merged.
fn append_staged_run(
    encoder: &mut PostingListEncoderV1,
    staged: &[u8],
    frequencies: bool,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let before = encoder.len();
    for posting in PostingListDecoderV1::new(staged, frequencies) {
        let (document, frequency) = posting?;
        encoder.push(document, frequency).map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact staged posting runs overlap".to_owned(),
            )
        })?;
    }
    if encoder.len() == before {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact staged posting run is empty".to_owned(),
        ));
    }
    Ok(())
}

/// Per-list bookkeeping charged against the n-gram rebuild's memory: map
/// entry, key, and encoder header.
const NGRAM_LIST_ENTRY_BYTES: usize = 96;
/// Rows decoded between row-dictionary resets, so the memoised dictionary
/// entries of one rebuild pass stay bounded however large the corpus is.
const NGRAM_DICTIONARY_WINDOW_ROWS: usize = 4_096;

/// One sealed n-gram list under construction, keyed `(kind, ngram)`.
type NgramListV1 = ((i64, i64), PostingListEncoderV1);

/// One rebuild pass's n-gram lists: keys at or above `lower` (the previous
/// pass's cutoff) and below this pass's own cutoff. When the lists outgrow
/// the memory authority the highest keys are shed and the cutoff drops to
/// them, so a later pass rebuilds exactly those keys.
struct NgramListPassV1 {
    lower: Option<(i64, i64)>,
    cutoff: Option<(i64, i64)>,
    lists: HashMap<(i64, i64), PostingListEncoderV1>,
    held: usize,
    memory_bytes: usize,
}

impl NgramListPassV1 {
    fn new(lower: Option<(i64, i64)>, memory_bytes: usize) -> Self {
        Self {
            lower,
            cutoff: None,
            lists: HashMap::new(),
            held: 0,
            memory_bytes: memory_bytes.max(1),
        }
    }

    /// Documents arrive in ascending order, so every list grows by appending.
    fn add(&mut self, key: (i64, i64), document: u32) -> Result<(), CodeLexicalArtifactErrorV1> {
        if self.lower.is_some_and(|lower| key < lower)
            || self.cutoff.is_some_and(|cutoff| key >= cutoff)
        {
            return Ok(());
        }
        let held = &mut self.held;
        let list = self.lists.entry(key).or_insert_with(|| {
            *held += NGRAM_LIST_ENTRY_BYTES;
            PostingListEncoderV1::new(false)
        });
        let before = list.retained_bytes();
        list.push(document, 1)?;
        self.held += list.retained_bytes() - before;
        if self.held > self.memory_bytes {
            let mut keys = self.lists.keys().copied().collect::<Vec<_>>();
            keys.sort_unstable();
            while self.held > self.memory_bytes / 2 && keys.len() > 1 {
                let Some(shed) = keys.pop() else { break };
                if let Some(list) = self.lists.remove(&shed) {
                    self.held -= list.retained_bytes() + NGRAM_LIST_ENTRY_BYTES;
                }
                self.cutoff = Some(shed);
            }
        }
        Ok(())
    }

    /// This pass's lists in key order, and where the next pass starts.
    fn finish(self) -> (Vec<NgramListV1>, Option<(i64, i64)>) {
        let mut lists = self.lists.into_iter().collect::<Vec<_>>();
        lists.sort_unstable_by_key(|(key, _)| *key);
        (lists, self.cutoff)
    }
}

/// Row blocks inflated and keyed together by one parallel n-gram step.
const NGRAM_DERIVE_WINDOW_BLOCKS: usize = 64;

/// A window of stored row blocks awaiting n-gram derivation. Inflating a
/// block and keying a decoded row are independent, so both run on the
/// indexing pool; dictionary decoding (one SQLite connection) and list
/// appends (ascending documents) stay on the calling thread in row order.
#[derive(Default)]
struct NgramDeriveWindowV1 {
    blocks: Vec<(i64, Vec<u8>)>,
}

impl NgramDeriveWindowV1 {
    fn derive<'connection>(
        &mut self,
        generation: &CodeGenerationId,
        connection: &'connection Connection,
        dictionary: &mut ConnectionRowDictionaryV1<'connection>,
        visited: &mut usize,
        pass: &mut NgramListPassV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        if self.blocks.is_empty() {
            return Ok(());
        }
        let blocks = std::mem::take(&mut self.blocks);
        let inflated = tracedecay_code_index::parallelism::install(|| {
            blocks
                .par_iter()
                .map(|(first_document, payload)| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        decode_row_block(*first_document, payload)
                    })
                })
                .collect::<Vec<_>>()
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        drop(blocks);
        let mut decoded = Vec::new();
        for rows in inflated {
            checkpoint(control)?;
            for stored in rows? {
                if *visited > 0 && visited.is_multiple_of(NGRAM_DICTIONARY_WINDOW_ROWS) {
                    *dictionary = ConnectionRowDictionaryV1::new(connection);
                }
                *visited += 1;
                let row = decode_artifact_row(
                    generation,
                    &stored.chunk_id,
                    &stored.row,
                    &stored.text,
                    &*dictionary,
                )?;
                decoded.push((stored.document_id, row));
            }
        }
        let keys = tracedecay_code_index::parallelism::install(|| {
            decoded
                .par_chunks(NGRAM_DERIVE_ROWS_PER_TASK)
                .map(|rows| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        rows.iter()
                            .map(|(_, row)| {
                                document_ngram_keys(
                                    &normalized_search_text(row),
                                    row.sanitized_text.as_str(),
                                    &row.normalized_text,
                                    control,
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
                .collect::<Vec<_>>()
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        for (rows, keys) in decoded.chunks(NGRAM_DERIVE_ROWS_PER_TASK).zip(keys) {
            for ((document, _), keys) in rows.iter().zip(keys?) {
                for key in keys {
                    pass.add(key, *document)?;
                }
            }
        }
        Ok(())
    }
}

/// Decoded rows keyed by one pool task, amortizing its CPU permit.
const NGRAM_DERIVE_ROWS_PER_TASK: usize = 128;

/// Rebuild every sealed n-gram list from the stored rows, in key order,
/// without any n-gram staging. Each pass walks the rows in document order;
/// one pass suffices unless the lists outgrow `ngram_memory_bytes`, in which
/// case later passes rebuild the keys an earlier pass shed.
fn derive_ngram_postings(
    transaction: &Transaction<'_>,
    authority: &ServingIndexStepAuthorityV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _mutation_authority = BuilderMutationGuardV1::enter(authority.mutation_gate)?;
    let mut insert = transaction
        .prepare(
            "INSERT INTO ngram_postings(kind, ngram, document_frequency, documents) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(sqlite_error)?;
    let mut lower: Option<(i64, i64)> = None;
    loop {
        let mut pass = NgramListPassV1::new(lower, authority.ngram_memory_bytes);
        let mut statement = transaction
            .prepare("SELECT first_document, payload FROM row_blocks ORDER BY first_document")
            .map_err(sqlite_error)?;
        let mut blocks = statement.query([]).map_err(sqlite_error)?;
        let mut dictionary = ConnectionRowDictionaryV1::new(transaction);
        let mut visited = 0usize;
        let mut window = NgramDeriveWindowV1::default();
        while let Some(block) = blocks.next().map_err(sqlite_error)? {
            checkpoint(control)?;
            let first_document: i64 = block.get(0).map_err(sqlite_error)?;
            let payload = block
                .get_ref(1)
                .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
                .map_err(sqlite_corrupt)?;
            window.blocks.push((first_document, payload.to_vec()));
            if window.blocks.len() >= NGRAM_DERIVE_WINDOW_BLOCKS {
                window.derive(
                    authority.generation,
                    transaction,
                    &mut dictionary,
                    &mut visited,
                    &mut pass,
                    control,
                )?;
            }
        }
        window.derive(
            authority.generation,
            transaction,
            &mut dictionary,
            &mut visited,
            &mut pass,
            control,
        )?;
        drop(blocks);
        drop(statement);
        let (lists, cutoff) = pass.finish();
        for (ordinal, (key, list)) in lists.into_iter().enumerate() {
            if ordinal.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
                checkpoint(control)?;
            }
            let document_frequency = i64::try_from(list.len()).map_err(contract_number)?;
            let documents = encode_document_set(&decode_ngram_bitmap(&list.finish()?)?)?;
            insert
                .execute(params![key.0, key.1, document_frequency, documents])
                .map_err(sqlite_error)?;
        }
        match cutoff {
            Some(cutoff) => lower = Some(cutoff),
            None => break,
        }
    }
    drop(insert);
    transaction
        .execute_batch(
            "CREATE TRIGGER frozen_ngram_postings_insert BEFORE INSERT ON ngram_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram postings'); END;",
        )
        .map_err(sqlite_error)
}

impl PersistedFinalizationStateV1 {
    fn new(
        content_epoch: i64,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        terminal_cursor: Option<Vec<u8>>,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        if content_epoch < 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact mutation epoch is negative".to_owned(),
            ));
        }
        let (base_section_row_counts, base_section_accumulators) =
            initial_base_section_receipt_fold()?;
        Ok(Self {
            phase: PersistedFinalizationPhaseV1::Indexes,
            section_ordinal: 0,
            section_row_count: 0,
            section_last_key: None,
            section_accumulator: initial_section_accumulator(SECTION_NAMES[0])?.to_vec(),
            base_section_row_counts,
            base_section_accumulators,
            completed_sections: Vec::new(),
            completed_rows: 0,
            content_epoch,
            source_state_digest: source.source_state_digest().clone(),
            terminal_cursor,
        })
    }
}

fn verify_artifact_state_metadata(
    connection: &Connection,
    expected_metadata: &CodeLexicalProjectionMetadataV1,
    expected_digest: &ManifestDigest,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let (format_revision, metadata_bytes, stored_digest): (u32, Vec<u8>, String) = connection
        .query_row(
            "SELECT format_revision, metadata, metadata_digest FROM artifact_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| artifact_state_row_corrupt("metadata", error))?;
    if format_revision != CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1 {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "format revision {format_revision} is not supported"
        )));
    }
    checkpoint(control)?;
    let actual_digest = stored_metadata_digest(&metadata_bytes)?;
    if stored_digest != actual_digest.as_str() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact metadata digest does not verify".to_owned(),
        ));
    }
    if metadata_bytes != content_metadata_bytes(expected_metadata)?
        || &actual_digest != expected_digest
    {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(
            "staging metadata does not match the requested generation".to_owned(),
        ));
    }
    checkpoint(control)?;
    Ok(())
}

/// Name the mandatory staging row a failed read required. A parked convergence
/// reports this detail verbatim, and bare SQLite text ("Query returned no
/// rows") attributes the failure to no authority an operator can act on.
fn artifact_state_row_corrupt(
    columns: &'static str,
    error: rusqlite::Error,
) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(format!(
        "lexical artifact staging artifact_state singleton {columns} read failed: {error}"
    ))
}

fn require_staged_revision(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
    let revision: i64 = connection
        .query_row(
            "SELECT format_revision FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| artifact_state_row_corrupt("format_revision", error))?;
    let revision = u32::try_from(revision).map_err(|_| {
        CodeLexicalArtifactErrorV1::Incompatible(
            "lexical artifact staging revision is outside the supported range".to_owned(),
        )
    })?;
    require_served_revision(revision)
}

fn finalization_started(connection: &Connection) -> Result<bool, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM finalization_state WHERE singleton = 1)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(sqlite_error)
}

fn content_epoch(connection: &Connection) -> Result<i64, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT epoch FROM content_epoch WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_corrupt)
}

fn ensure_content_epoch(
    connection: &Connection,
    expected: i64,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let actual = content_epoch(connection)?;
    if actual != expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact content changed after bounded finalization began".to_owned(),
        ));
    }
    Ok(())
}

/// The persisted finalization state; `None` before finalization starts and
/// after the seal drops the table.
fn load_finalization_state(
    connection: &Connection,
) -> Result<Option<PersistedFinalizationStateV1>, CodeLexicalArtifactErrorV1> {
    if !table_exists(connection, "finalization_state")? {
        return Ok(None);
    }
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT state FROM finalization_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    bytes
        .as_deref()
        .map(|bytes| {
            serde_json::from_slice(bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
        })
        .transpose()
}

fn store_finalization_state(
    transaction: &Transaction<'_>,
    state: &PersistedFinalizationStateV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    transaction
        .execute(
            "INSERT INTO finalization_state(singleton, state) VALUES (1, ?1) ON CONFLICT(singleton) DO UPDATE SET state = excluded.state",
            params![bytes],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn validate_finalization_state(
    state: &PersistedFinalizationStateV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let section_names = &SECTION_NAMES;
    let section_count = u64::try_from(section_names.len()).map_err(contract_number)?;
    let completed_section_count = if state.phase == PersistedFinalizationPhaseV1::Digest {
        usize::try_from(state.section_ordinal).map_err(contract_number)?
    } else {
        0
    };
    let maximum_ordinal = match state.phase {
        PersistedFinalizationPhaseV1::Statistics => STATISTICS_STEP_COUNT_V11,
        PersistedFinalizationPhaseV1::Indexes => SERVING_INDEX_STEP_COUNT_V11,
        PersistedFinalizationPhaseV1::Digest => section_count,
    };
    if state.section_ordinal > maximum_ordinal
        || (state.phase == PersistedFinalizationPhaseV1::Digest
            && state.section_ordinal > 0
            && state.section_ordinal
                < u64::try_from(1 + BASE_SECTION_NAMES.len()).map_err(contract_number)?)
        || state.completed_sections.len() != completed_section_count
        || state.completed_sections.len() > section_names.len()
        || state.section_accumulator.len() != 32
        || state.base_section_row_counts.len() != BASE_SECTION_NAMES.len()
        || state.base_section_accumulators.len() != BASE_SECTION_NAMES.len()
        || state
            .base_section_accumulators
            .iter()
            .any(|accumulator| accumulator.len() != 32)
        || state.content_epoch < 0
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact finalization state is malformed".to_owned(),
        ));
    }
    if state
        .completed_sections
        .iter()
        .zip(section_names)
        .any(|(section, expected_name)| section.name != *expected_name)
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact finalization sections are out of order".to_owned(),
        ));
    }
    if let Some(key) = &state.section_last_key {
        if state.phase != PersistedFinalizationPhaseV1::Digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "persisted lexical artifact pre-digest state has a row key".to_owned(),
            ));
        }
        let section_ordinal = usize::try_from(state.section_ordinal).map_err(contract_number)?;
        let section = FinalizationSectionV1::from_ordinal(section_ordinal)?;
        if !key.matches_section(section) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "persisted lexical artifact finalization key has the wrong native shape".to_owned(),
            ));
        }
    }
    let completed_rows = state
        .completed_sections
        .iter()
        .try_fold(0u64, |total, section| total.checked_add(section.row_count))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "persisted lexical artifact finalization row count overflowed".to_owned(),
            )
        })?;
    if completed_rows > state.completed_rows {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact finalization progress regressed".to_owned(),
        ));
    }
    Ok(())
}

fn advance_section_rows(
    transaction: &Transaction<'_>,
    section: FinalizationSectionV1,
    state: &mut PersistedFinalizationStateV1,
    maximum_rows: usize,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let limit = i64::try_from(maximum_rows).map_err(contract_number)?;
    let last_key = state.section_last_key.clone();
    if matches!(
        section,
        FinalizationSectionV1::CloneOccurrences
            | FinalizationSectionV1::CloneExactPostings
            | FinalizationSectionV1::CloneBodyPayloads
            | FinalizationSectionV1::CloneFingerprintPostings
    ) {
        return advance_clone_section_rows(
            transaction,
            section,
            state,
            limit,
            last_key.as_ref(),
            control,
        );
    }
    match (section, last_key.as_ref()) {
        (
            FinalizationSectionV1::SourcePages
            | FinalizationSectionV1::FieldStatistics
            | FinalizationSectionV1::Vocabulary,
            None,
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(false)?,
            params![limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::SourcePages | FinalizationSectionV1::FieldStatistics,
            Some(PersistedFinalizationKeyV1::Integer(value)),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(true)?,
            params![value, limit],
            state,
            control,
        ),
        (FinalizationSectionV1::Vocabulary, Some(PersistedFinalizationKeyV1::Text(value))) => {
            advance_native_section_rows(
                transaction,
                section,
                section.walked_query(true)?,
                params![value, limit],
                state,
                control,
            )
        }
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact finalization key does not match its section".to_owned(),
        )),
    }
}

fn advance_clone_section_rows(
    transaction: &Transaction<'_>,
    section: FinalizationSectionV1,
    state: &mut PersistedFinalizationStateV1,
    limit: i64,
    last_key: Option<&PersistedFinalizationKeyV1>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    match (section, last_key) {
        (
            FinalizationSectionV1::CloneOccurrences
            | FinalizationSectionV1::CloneExactPostings
            | FinalizationSectionV1::CloneBodyPayloads
            | FinalizationSectionV1::CloneFingerprintPostings,
            None,
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(false)?,
            params![limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::CloneOccurrences | FinalizationSectionV1::CloneBodyPayloads,
            Some(PersistedFinalizationKeyV1::Integer(value)),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(true)?,
            params![value, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::CloneExactPostings,
            Some(PersistedFinalizationKeyV1::ClonePosting {
                class,
                normalization_revision,
                digest,
                occurrence_ordinal,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(true)?,
            params![
                class,
                normalization_revision,
                digest,
                occurrence_ordinal,
                limit
            ],
            state,
            control,
        ),
        (
            FinalizationSectionV1::CloneFingerprintPostings,
            Some(PersistedFinalizationKeyV1::Fingerprint {
                language,
                class,
                normalization_revision,
                fingerprint,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.walked_query(true)?,
            params![language, class, normalization_revision, fingerprint, limit],
            state,
            control,
        ),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact clone finalization key does not match its section"
                .to_owned(),
        )),
    }
}

fn advance_native_section_rows<P: rusqlite::Params>(
    transaction: &Transaction<'_>,
    section: FinalizationSectionV1,
    query: &str,
    parameters: P,
    state: &mut PersistedFinalizationStateV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let mut statement = transaction.prepare(query).map_err(sqlite_error)?;
    let column_count = statement.column_count();
    let mut rows = statement.query(parameters).map_err(sqlite_error)?;
    let mut advanced = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        // Cancellation remains bounded within every native-key scan.
        checkpoint(control)?;
        let key = native_row_key(section, row)?;
        if state
            .section_last_key
            .as_ref()
            .is_some_and(|previous| key <= *previous)
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization keyset did not advance".to_owned(),
            ));
        }
        if section == FinalizationSectionV1::SourcePages {
            let page_ordinal =
                u64::try_from(row.get::<_, i64>(0).map_err(sqlite_error)?).map_err(|_| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact base-section receipt has a negative page".to_owned(),
                    )
                })?;
            let receipt: Vec<u8> = row.get(6).map_err(sqlite_error)?;
            absorb_page_base_sections_receipt(
                page_ordinal,
                &receipt,
                &mut state.base_section_row_counts,
                &mut state.base_section_accumulators,
            )?;
        }
        absorb_section_row(
            section.name(),
            state.section_row_count,
            &mut state.section_accumulator,
            row,
            column_count,
        )?;
        state.section_last_key = Some(key);
        state.section_row_count = state.section_row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization row count overflowed".to_owned(),
            )
        })?;
        state.completed_rows = state.completed_rows.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalized row count overflowed".to_owned(),
            )
        })?;
        advanced = advanced.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization work count overflowed".to_owned(),
            )
        })?;
    }
    Ok(advanced)
}

fn native_row_key(
    section: FinalizationSectionV1,
    row: &rusqlite::Row<'_>,
) -> Result<PersistedFinalizationKeyV1, CodeLexicalArtifactErrorV1> {
    if matches!(
        section,
        FinalizationSectionV1::CloneOccurrences
            | FinalizationSectionV1::CloneExactPostings
            | FinalizationSectionV1::CloneBodyPayloads
            | FinalizationSectionV1::CloneFingerprintPostings
    ) {
        return clone_row_key(section, row);
    }
    match section {
        FinalizationSectionV1::SourcePages | FinalizationSectionV1::FieldStatistics => Ok(
            PersistedFinalizationKeyV1::Integer(row.get(0).map_err(sqlite_error)?),
        ),
        FinalizationSectionV1::Vocabulary => Ok(PersistedFinalizationKeyV1::Text(
            row.get(0).map_err(sqlite_error)?,
        )),
        FinalizationSectionV1::DocumentIntegrity
        | FinalizationSectionV1::ImportIntegrity
        | FinalizationSectionV1::ImportEvidence
        | FinalizationSectionV1::Rows
        | FinalizationSectionV1::TermPostings
        | FinalizationSectionV1::ExactPostings
        | FinalizationSectionV1::NgramPostings => Err(base_section_walk_error()),
        FinalizationSectionV1::CloneOccurrences
        | FinalizationSectionV1::CloneExactPostings
        | FinalizationSectionV1::CloneBodyPayloads
        | FinalizationSectionV1::CloneFingerprintPostings => {
            Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone row key bypassed its dedicated decoder".to_owned(),
            ))
        }
    }
}

fn clone_row_key(
    section: FinalizationSectionV1,
    row: &rusqlite::Row<'_>,
) -> Result<PersistedFinalizationKeyV1, CodeLexicalArtifactErrorV1> {
    if matches!(
        section,
        FinalizationSectionV1::CloneOccurrences | FinalizationSectionV1::CloneBodyPayloads
    ) {
        return Ok(PersistedFinalizationKeyV1::Integer(
            row.get(0).map_err(sqlite_error)?,
        ));
    }
    if section == FinalizationSectionV1::CloneExactPostings {
        return Ok(PersistedFinalizationKeyV1::ClonePosting {
            class: row.get(0).map_err(sqlite_error)?,
            normalization_revision: row.get(1).map_err(sqlite_error)?,
            digest: row.get(2).map_err(sqlite_error)?,
            occurrence_ordinal: row.get(3).map_err(sqlite_error)?,
        });
    }
    let language = row.get(0).map_err(sqlite_error)?;
    let class = row.get(1).map_err(sqlite_error)?;
    let normalization_revision = row.get(2).map_err(sqlite_error)?;
    let fingerprint = row.get(3).map_err(sqlite_error)?;
    match section {
        FinalizationSectionV1::CloneFingerprintPostings => {
            Ok(PersistedFinalizationKeyV1::Fingerprint {
                language,
                class,
                normalization_revision,
                fingerprint,
            })
        }
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "non-fingerprint section requested a fingerprint row key".to_owned(),
        )),
    }
}

fn initial_section_accumulator(name: &str) -> Result<[u8; 32], CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-section.v2\0initial");
    hasher.update(
        u64::try_from(name.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(name.as_bytes());
    Ok(hasher.finalize().into())
}

fn absorb_section_row(
    name: &str,
    row_ordinal: u64,
    accumulator: &mut Vec<u8>,
    row: &rusqlite::Row<'_>,
    column_count: usize,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let previous: [u8; 32] = accumulator.as_slice().try_into().map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact finalization accumulator has the wrong length".to_owned(),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-section.v2\0row");
    hasher.update(
        u64::try_from(name.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(name.as_bytes());
    hasher.update(row_ordinal.to_le_bytes());
    hasher.update(previous);
    for column in 0..column_count {
        hash_value(&mut hasher, row.get_ref(column).map_err(sqlite_error)?)?;
    }
    *accumulator = hasher.finalize().to_vec();
    Ok(())
}

fn finish_persisted_section(
    name: &str,
    state: &PersistedFinalizationStateV1,
) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
    finish_section(name, state.section_row_count, &state.section_accumulator)
}

fn finish_section(
    name: &str,
    row_count: u64,
    accumulator: &[u8],
) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
    let accumulator: [u8; 32] = accumulator.try_into().map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact finalization accumulator has the wrong length".to_owned(),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-section.v2\0final");
    hasher.update(
        u64::try_from(name.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(name.as_bytes());
    hasher.update(row_count.to_le_bytes());
    hasher.update(accumulator);
    let digest = ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    Ok(CodeLexicalArtifactSectionDigestV1 {
        name: name.to_owned(),
        row_count,
        digest,
    })
}

/// Confirm the sealed source receipt from its durable terminal cursor without
/// counting/replaying every staged page on each bounded finalization wake.
/// The final section receipts validate the full source-page cardinality before
/// a sealed artifact is published.
fn verify_staged_source_chain(
    connection: &Connection,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Option<Vec<u8>>, CodeLexicalArtifactErrorV1> {
    let mut statement = connection
        .prepare(
            "SELECT p.page_ordinal, c.cumulative_digest, p.chunk_count, c.payload_bytes, p.import_count, p.import_payload_bytes, p.import_dictionary_digest, c.next_cursor FROM source_pages p LEFT JOIN source_page_cursors c ON c.page_ordinal = p.page_ordinal ORDER BY p.page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut expected_ordinal = 0u64;
    let mut chunks = 0u64;
    let mut payload_bytes = 0u64;
    let mut imports = 0u64;
    let mut import_payload_bytes = 0u64;
    let mut terminal = None;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        checkpoint(control)?;
        let ordinal =
            u64::try_from(row.get::<_, i64>(0).map_err(sqlite_error)?).map_err(contract_number)?;
        let (Some(cumulative_digest), Some(page_payload), Some(cursor_bytes)) = (
            row.get::<_, Option<String>>(1).map_err(sqlite_error)?,
            row.get::<_, Option<i64>>(3).map_err(sqlite_error)?,
            row.get::<_, Option<Vec<u8>>>(7).map_err(sqlite_error)?,
        ) else {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact source page has no staged cursor".to_owned(),
            ));
        };
        let page_chunks =
            u64::try_from(row.get::<_, i64>(2).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_payload = u64::try_from(page_payload).map_err(contract_number)?;
        let page_imports =
            u64::try_from(row.get::<_, i64>(4).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_import_payload =
            u64::try_from(row.get::<_, i64>(5).map_err(sqlite_error)?).map_err(contract_number)?;
        let import_digest: String = row.get(6).map_err(sqlite_error)?;
        let cursor = decode_cursor(&cursor_bytes)?;
        chunks = chunks
            .checked_add(page_chunks)
            .ok_or_else(source_chain_overflow)?;
        payload_bytes = payload_bytes
            .checked_add(page_payload)
            .ok_or_else(source_chain_overflow)?;
        imports = imports
            .checked_add(page_imports)
            .ok_or_else(source_chain_overflow)?;
        import_payload_bytes = import_payload_bytes
            .checked_add(page_import_payload)
            .ok_or_else(source_chain_overflow)?;
        if ordinal != expected_ordinal
            || cursor.next_page_ordinal() != expected_ordinal + 1
            || cursor.emitted_chunks() != chunks
            || cursor.emitted_payload_bytes() != payload_bytes
            || cursor.emitted_imports() != imports
            || cursor.emitted_import_payload_bytes() != import_payload_bytes
            || cursor.cumulative_digest().as_str() != cumulative_digest
            || cursor.import_dictionary_digest().as_str() != import_digest
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact source-page cursor chain is inconsistent".to_owned(),
            ));
        }
        expected_ordinal = expected_ordinal
            .checked_add(1)
            .ok_or_else(source_chain_overflow)?;
        terminal = Some(cursor);
    }
    source
        .verify_completion(terminal.as_ref())
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    terminal.as_ref().map(encode_cursor).transpose()
}

fn source_chain_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(
        "lexical artifact source-page chain counter overflowed".to_owned(),
    )
}

fn verify_sealed_receipt_header(
    receipt: &VerifiedCodeLexicalArtifactV1,
    expected_metadata_digest: &ManifestDigest,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    verify_source_receipt(receipt, source)?;
    if receipt.metadata_digest() != expected_metadata_digest {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact metadata digest changed".to_owned(),
        ));
    }
    Ok(())
}

fn verify_final_sections_against_source(
    sections: &[CodeLexicalArtifactSectionDigestV1],
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    // Receipts count admitted documents, bounded by the source's chunks;
    // the freeze already matched them against the stored rows.
    let admitted_documents = sections
        .iter()
        .find(|section| section.name == "rows")
        .map(|section| section.row_count)
        .filter(|rows| *rows <= source.total_chunks())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact rows exceed the sealed source's chunks".to_owned(),
            )
        })?;
    let expected = [
        ("source_pages", source.page_count()),
        ("document_integrity", admitted_documents),
        ("import_integrity", source.total_imports()),
        ("import_evidence", source.total_imports()),
        ("rows", admitted_documents),
    ];
    for (name, expected_rows) in expected {
        let actual = sections
            .iter()
            .find(|section| section.name == name)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact finalization omitted a required section".to_owned(),
                )
            })?;
        if actual.row_count != expected_rows {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                "lexical artifact {name} rows disagree with the sealed source receipt"
            )));
        }
    }
    Ok(())
}

/// One staged `source_pages` receipt row: page and cumulative digests, chunk
/// and payload counts, import counts, dictionary digest, and cursor bytes.
type StoredSourcePageRowV1 = (String, String, i64, i64, i64, i64, String, Vec<u8>);

fn progress(
    connection: &Connection,
) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
    if read_receipt(connection)?.is_some() {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "a sealed lexical artifact keeps no source progress; publish it".to_owned(),
        ));
    }
    let cursor_bytes = match load_finalization_state(connection)? {
        Some(state) => state.terminal_cursor,
        None => {
            let tail: Option<(i64, Vec<u8>)> = connection
                .query_row(PROGRESS_TAIL_QUERY, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(sqlite_error)?;
            match tail {
                Some((page_ordinal, cursor_bytes)) => {
                    let next_page_ordinal = u64::try_from(page_ordinal)
                        .map_err(contract_number)?
                        .checked_add(1);
                    if Some(decode_cursor(&cursor_bytes)?.next_page_ordinal()) != next_page_ordinal
                    {
                        return Err(CodeLexicalArtifactErrorV1::Corrupt(
                            "persisted lexical artifact progress disagrees with its exact source cursor"
                                .to_owned(),
                        ));
                    }
                    Some(cursor_bytes)
                }
                None => None,
            }
        }
    };
    let Some(cursor_bytes) = cursor_bytes else {
        return Ok(CodeLexicalArtifactBuildProgressV1 {
            next_page_ordinal: 0,
            completed_chunks: 0,
            completed_payload_bytes: 0,
            completed_imports: 0,
            completed_import_payload_bytes: 0,
            import_dictionary_digest: None,
            cumulative_source_digest: None,
            next_cursor: None,
        });
    };
    let cursor = decode_cursor(&cursor_bytes)?;
    let next_page_ordinal = cursor.next_page_ordinal();
    Ok(CodeLexicalArtifactBuildProgressV1 {
        next_page_ordinal,
        completed_chunks: cursor.emitted_chunks(),
        completed_payload_bytes: cursor.emitted_payload_bytes(),
        completed_imports: cursor.emitted_imports(),
        completed_import_payload_bytes: cursor.emitted_import_payload_bytes(),
        import_dictionary_digest: Some(cursor.import_dictionary_digest().clone()),
        cumulative_source_digest: Some(cursor.cumulative_digest().clone()),
        next_cursor: Some(cursor),
    })
}

fn cursor_before_page(
    connection: &Connection,
    page_ordinal: u64,
) -> Result<Option<VerifiedSealedLexicalCursorV1>, CodeLexicalArtifactErrorV1> {
    if page_ordinal == 0 {
        return Ok(None);
    }
    let previous = page_ordinal.checked_sub(1).ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Contract("lexical page ordinal underflowed".to_owned())
    })?;
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT next_cursor FROM source_page_cursors WHERE page_ordinal = ?1",
            [i64::try_from(previous).map_err(contract_number)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    bytes.as_deref().map(decode_cursor).transpose()
}

fn verify_replayed_page(
    connection: &Connection,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let stored: Option<StoredSourcePageRowV1> = connection
        .query_row(
            "SELECT c.page_digest, c.cumulative_digest, p.chunk_count, c.payload_bytes, p.import_count, p.import_payload_bytes, p.import_dictionary_digest, c.next_cursor FROM source_pages p JOIN source_page_cursors c ON c.page_ordinal = p.page_ordinal WHERE p.page_ordinal = ?1",
            [i64::try_from(page.page_ordinal()).map_err(contract_number)?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let cursor = encode_cursor(page.next_cursor())?;
    let expected = (
        page.page_digest().as_str(),
        page.cumulative_digest().as_str(),
        i64::try_from(page.chunk_count()).map_err(contract_number)?,
        i64::try_from(page.payload_bytes()).map_err(contract_number)?,
        i64::try_from(page.import_count()).map_err(contract_number)?,
        i64::try_from(page.import_payload_bytes()).map_err(contract_number)?,
        page.next_cursor().import_dictionary_digest().as_str(),
        cursor.as_slice(),
    );
    if stored.as_ref().map(|stored| {
        (
            stored.0.as_str(),
            stored.1.as_str(),
            stored.2,
            stored.3,
            stored.4,
            stored.5,
            stored.6.as_str(),
            stored.7.as_slice(),
        )
    }) != Some(expected)
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "replayed lexical source page does not match its staged receipt".to_owned(),
        ));
    }
    Ok(())
}

fn validate_contiguous_pages(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut statement = connection
        .prepare("SELECT page_ordinal FROM source_pages ORDER BY page_ordinal")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut expected = 0i64;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        checkpoint(control)?;
        let ordinal: i64 = row.get(0).map_err(sqlite_error)?;
        if ordinal != expected {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact page receipts are not contiguous".to_owned(),
            ));
        }
        expected = expected.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact page ordinal overflowed".to_owned(),
            )
        })?;
    }
    Ok(())
}

fn encode_cursor(
    cursor: &tracedecay_code_index::production::VerifiedSealedLexicalCursorV1,
) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    cursor
        .persisted_bytes()
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn decode_cursor(
    bytes: &[u8],
) -> Result<VerifiedSealedLexicalCursorV1, CodeLexicalArtifactErrorV1> {
    VerifiedSealedLexicalCursorV1::restore_persisted(bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
}

pub(super) fn compute_section_digests(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<CodeLexicalArtifactSectionDigestV1>, CodeLexicalArtifactErrorV1> {
    let (source_pages, base_sections) = digest_source_pages_and_base_receipts(connection, control)?;
    let mut sections = Vec::with_capacity(SECTION_NAMES.len());
    sections.push(source_pages);
    sections.extend(base_sections);
    for section in [
        FinalizationSectionV1::FieldStatistics,
        FinalizationSectionV1::Vocabulary,
        FinalizationSectionV1::CloneOccurrences,
        FinalizationSectionV1::CloneExactPostings,
        FinalizationSectionV1::CloneBodyPayloads,
        FinalizationSectionV1::CloneFingerprintPostings,
    ] {
        sections.push(digest_query(connection, section, control)?);
    }
    Ok(sections)
}

fn digest_source_pages_and_base_receipts(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<
    (
        CodeLexicalArtifactSectionDigestV1,
        Vec<CodeLexicalArtifactSectionDigestV1>,
    ),
    CodeLexicalArtifactErrorV1,
> {
    let section = FinalizationSectionV1::SourcePages;
    let mut row_count = 0u64;
    let mut accumulator = initial_section_accumulator(section.name())?.to_vec();
    let (mut base_row_counts, mut base_accumulators) = initial_base_section_receipt_fold()?;
    let mut statement = connection
        .prepare(section.full_query().ok_or_else(base_section_walk_error)?)
        .map_err(|error| map_section_digest_sql_error(section.name(), error))?;
    let column_count = statement.column_count();
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        checkpoint(control)?;
        let page_ordinal =
            u64::try_from(row.get::<_, i64>(0).map_err(sqlite_error)?).map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact base-section receipt has a negative page".to_owned(),
                )
            })?;
        let receipt: Vec<u8> = row.get(6).map_err(sqlite_error)?;
        absorb_page_base_sections_receipt(
            page_ordinal,
            &receipt,
            &mut base_row_counts,
            &mut base_accumulators,
        )?;
        absorb_section_row(
            section.name(),
            row_count,
            &mut accumulator,
            row,
            column_count,
        )?;
        row_count = row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact source-page receipt count overflowed".to_owned(),
            )
        })?;
    }
    Ok((
        finish_section(section.name(), row_count, &accumulator)?,
        finish_base_section_receipt_fold(&base_row_counts, &base_accumulators)?,
    ))
}

fn digest_query(
    connection: &Connection,
    section: FinalizationSectionV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
    let mut row_count = 0u64;
    let mut accumulator = initial_section_accumulator(section.name())?.to_vec();
    let mut statement = connection
        .prepare(section.full_query().ok_or_else(base_section_walk_error)?)
        .map_err(|error| map_section_digest_sql_error(section.name(), error))?;
    let column_count = statement.column_count();
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if row_count.is_multiple_of(4_096) {
            checkpoint(control)?;
        }
        absorb_section_row(
            section.name(),
            row_count,
            &mut accumulator,
            row,
            column_count,
        )?;
        row_count = row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact section row count overflowed".to_owned(),
            )
        })?;
    }
    finish_section(section.name(), row_count, &accumulator)
}

fn map_section_digest_sql_error(
    section: &str,
    error: rusqlite::Error,
) -> CodeLexicalArtifactErrorV1 {
    let message = error.to_string();
    if message.contains("no such column") || message.contains("no such table") {
        CodeLexicalArtifactErrorV1::Incompatible(format!(
            "lexical artifact revision {CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1} cannot digest {section}: {message}"
        ))
    } else {
        sqlite_error(error)
    }
}

fn hash_value(hasher: &mut Sha256, value: ValueRef<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    match value {
        ValueRef::Null => hasher.update([0]),
        ValueRef::Integer(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hasher.update([2]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            hasher.update([3]);
            hash_bytes(hasher, value)?;
        }
        ValueRef::Blob(value) => {
            hasher.update([4]);
            hash_bytes(hasher, value)?;
        }
    }
    Ok(())
}

fn read_receipt(
    connection: &Connection,
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT receipt FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_corrupt)?;
    decode_padded_receipt(&bytes)
}

fn read_receipt_with_control(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT receipt FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_corrupt)?;
    decode_padded_receipt_with_control(&bytes, control)
}

fn verify_source_receipt(
    receipt: &VerifiedCodeLexicalArtifactV1,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if receipt.page_count() != source.page_count()
        || receipt.total_chunks() != source.total_chunks()
        || receipt.total_payload_bytes() != source.total_payload_bytes()
        || receipt.total_imports() != source.total_imports()
        || receipt.import_payload_bytes() != source.import_payload_bytes()
        || receipt.import_dictionary_digest() != source.import_dictionary_digest()
        || receipt.source_format_revision() != source.format_revision()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "sealed lexical source receipt disagrees with finalized artifact".to_owned(),
        ));
    }
    Ok(())
}

fn record_finalization_step(step: &CodeLexicalArtifactFinalizationStepV1) {
    #[cfg(feature = "hotpath")]
    {
        match step {
            CodeLexicalArtifactFinalizationStepV1::Pending { completed_rows, .. } => {
                hotpath::gauge!("query.artifact.finalization.outcome.pending_total").inc(1u64);
                crate::hotpath_metrics::Residency::Rebuilding.record("query.artifact.residency");
                hotpath::gauge!("query.artifact.rows").set(*completed_rows);
            }
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => {
                hotpath::gauge!("query.artifact.finalization.outcome.ready_total").inc(1u64);
                crate::hotpath_metrics::Residency::Warm.record("query.artifact.residency");
                hotpath::gauge!("query.artifact.pages").set(receipt.page_count());
                hotpath::gauge!("query.artifact.bytes").set(receipt.file_size_bytes());
            }
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = step;
}

fn record_batch_outcome(
    result: &Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1>,
) {
    #[cfg(feature = "hotpath")]
    {
        match result {
            Ok(_) => {
                hotpath::gauge!("query.artifact.batch.outcome.committed_total").inc(1u64);
            }
            Err(CodeLexicalArtifactErrorV1::Interrupted(_)) => {
                hotpath::gauge!("query.artifact.batch.outcome.interrupted_total").inc(1u64);
            }
            Err(_) => {
                hotpath::gauge!("query.artifact.batch.outcome.failed_total").inc(1u64);
            }
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = result;
}

fn record_prepared_batch_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    #[cfg(feature = "hotpath")]
    {
        let documents = pages.iter().map(|page| page.documents.len()).sum::<usize>();
        let source_bytes = pages
            .iter()
            .map(PreparedCodeLexicalArtifactPageV1::source_retained_bytes)
            .sum::<usize>();
        let prepared_bytes = pages
            .iter()
            .map(PreparedCodeLexicalArtifactPageV1::retained_owned_bytes)
            .sum::<usize>();
        let effective_workers =
            tracedecay_code_index::parallelism::indexing_workers().min(pages.len());
        let mut scratch = pages
            .iter()
            .map(PreparedCodeLexicalArtifactPageV1::preparation_scratch_bytes)
            .collect::<Vec<_>>();
        scratch.sort_unstable_by(|left, right| right.cmp(left));
        let active_scratch = scratch.into_iter().take(effective_workers).sum::<usize>();
        hotpath::gauge!("query.artifact.batch.prepared_pages_total").inc(pages.len() as u64);
        hotpath::gauge!("query.artifact.batch.prepared_documents_total").inc(documents as u64);
        hotpath::gauge!("query.artifact.batch.source_bytes_total").inc(source_bytes as u64);
        hotpath::gauge!("query.artifact.batch.prepared_bytes_total").inc(prepared_bytes as u64);
        hotpath::gauge!("query.artifact.batch.active_scratch_bytes_total")
            .inc(active_scratch as u64);
        hotpath::gauge!("query.artifact.batch.effective_workers").set(effective_workers as u64);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = pages;
}

fn record_batch_import_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    #[cfg(feature = "hotpath")]
    {
        let imports = pages.iter().map(|page| page.imports.len()).sum::<usize>();
        hotpath::gauge!("query.artifact.batch.import_rows_total").inc(imports as u64);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = pages;
}

fn record_batch_posting_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    #[cfg(feature = "hotpath")]
    {
        let relational_postings = pages
            .iter()
            .flat_map(|page| &page.documents)
            .map(|document| document.term_postings.len() + document.exact_postings.len())
            .sum::<usize>();
        let ngram_shards = pages
            .iter()
            .map(|page| page.ngram_shards.len())
            .sum::<usize>();
        let ngram_documents = pages
            .iter()
            .flat_map(|page| &page.ngram_shards)
            .map(|shard| shard.cardinality)
            .sum::<u64>();
        let ngram_bytes = pages
            .iter()
            .flat_map(|page| &page.ngram_shards)
            .map(|shard| shard.documents.len())
            .sum::<usize>();
        hotpath::gauge!("query.artifact.batch.posting_rows_total").inc(relational_postings as u64);
        hotpath::gauge!("query.artifact.batch.ngram_shard_rows_total").inc(ngram_shards as u64);
        hotpath::gauge!("query.artifact.batch.ngram_documents_total").inc(ngram_documents);
        hotpath::gauge!("query.artifact.batch.ngram_bytes_total").inc(ngram_bytes as u64);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = pages;
}

fn record_batch_row_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    #[cfg(feature = "hotpath")]
    {
        let rows = pages.iter().map(|page| page.documents.len()).sum::<usize>();
        hotpath::gauge!("query.artifact.batch.document_rows_total").inc(rows as u64);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = pages;
}

fn record_batch_receipt_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("query.artifact.batch.receipt_rows_total").inc(pages.len() as u64);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = pages;
}

fn record_batch_prefix_limit(limit: CodeLexicalArtifactBatchLimitV1) {
    #[cfg(feature = "hotpath")]
    {
        match limit {
            CodeLexicalArtifactBatchLimitV1::Memory => {
                hotpath::gauge!("query.artifact.batch.prefix_limited.memory_total").inc(1u64);
            }
            CodeLexicalArtifactBatchLimitV1::PreparedRows => {
                hotpath::gauge!("query.artifact.batch.prefix_limited.prepared_rows_total")
                    .inc(1u64);
            }
            CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes => {
                hotpath::gauge!("query.artifact.batch.prefix_limited.estimated_write_bytes_total")
                    .inc(1u64);
            }
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = limit;
}

fn record_artifact_progress(progress: &CodeLexicalArtifactBuildProgressV1) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("query.artifact.pages").set(progress.next_page_ordinal);
        hotpath::gauge!("query.artifact.rows").set(progress.completed_chunks);
        hotpath::gauge!("query.artifact.bytes").set(progress.completed_payload_bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = progress;
}

fn commit_finalization_transaction(
    transaction: Transaction<'_>,
    metrics: &mut FinalizationTransactionMetricsV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("query.artifact.finalization.commit_attempts_total").inc(1u64);
    let result = hotpath::measure_block!(
        "query.artifact.finalization.commit",
        transaction.commit().map_err(sqlite_error)
    );
    if result.is_ok() {
        metrics.mark_committed();
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.finalization.commit_succeeded_total").inc(1u64);
    }
    result
}

#[hotpath::measure(label = "query.artifact.finalization.sealed_verify")]
fn verify_finalized_artifact(
    connection: &Connection,
    path: &Path,
    expected_metadata_digest: &ManifestDigest,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    receipt: &VerifiedCodeLexicalArtifactV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    require_integrity(connection, control)?;
    verify_source_receipt(receipt, source)?;
    if receipt.metadata_digest() != expected_metadata_digest {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact metadata digest changed".to_owned(),
        ));
    }
    require_served_revision(receipt.format_revision())?;
    let sections = compute_section_digests(connection, control)?;
    if sections != receipt.section_digests() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact section digests do not verify".to_owned(),
        ));
    }
    if &receipt_artifact_digest(receipt, &sections)? != receipt.artifact_digest() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact content digest does not verify".to_owned(),
        ));
    }
    let actual_size = path
        .metadata()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CodeLexicalArtifactErrorV1::Missing(error.to_string())
            } else {
                CodeLexicalArtifactErrorV1::Io(error.to_string())
            }
        })?
        .len();
    if actual_size != receipt.file_size_bytes() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "artifact size changed from {} to {actual_size} while sealing",
            receipt.file_size_bytes()
        )));
    }
    Ok(())
}

fn require_integrity(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let result: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(sqlite_corrupt)?;
    checkpoint(control)?;
    if result != "ok" {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(result));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::format::decode_term_lists;
    use super::*;
    use rusqlite::StatementStatus;
    use rusqlite::hooks::{AuthAction, Authorization};
    use tracedecay_domain::{
        CodeGenerationId, ComponentRevision, FreshnessCompatibilityV1, ScoreDomainId,
        SourceFreshness, SourceInstanceKey, SourceNamespace, UtcMicros,
    };

    struct ActiveControl;

    impl CodeIndexExecutionControlV1 for ActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    #[test]
    fn clone_payload_conflict_checks_cover_full_and_partial_batches() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE clone_body_payloads(payload_digest BLOB PRIMARY KEY, payload BLOB NOT NULL)",
        ).unwrap();
        let transaction = connection.transaction().unwrap();
        let bodies: Vec<_> = (0..=PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK)
            .map(|index| PreparedCloneBodyV1 {
                payload_digest: format!("sha256:{index:064x}"),
                payload: vec![1],
                eligibility: Vec::new(),
                serialized_bytes: 1,
                symbol_occurrence_id: format!("symbol.{index}"),
                path: "src/lib.rs".to_owned(),
                body_start: 0,
                body_end: 1,
                exact_keys: Vec::new(),
                fingerprint_stream: None,
            })
            .collect();
        for body in &bodies {
            transaction
                .execute(
                    "INSERT INTO clone_body_payloads VALUES (?1, ?2)",
                    params![
                        stored_digest_key(&body.payload_digest).unwrap(),
                        body.payload
                    ],
                )
                .unwrap();
        }
        let references: Vec<_> = bodies.iter().collect();
        verify_clone_payload_digests(&transaction, &references, &ActiveControl).unwrap();
        verify_clone_payload_digests(&transaction, &references[..1], &ActiveControl).unwrap();
        transaction
            .execute("UPDATE clone_body_payloads SET payload = X'02'", [])
            .unwrap();
        for batch in [&references[..], &references[..1]] {
            assert!(matches!(
                verify_clone_payload_digests(&transaction, batch, &ActiveControl),
                Err(CodeLexicalArtifactErrorV1::Corrupt(_))
            ));
        }
    }

    fn test_metadata() -> CodeLexicalProjectionMetadataV1 {
        CodeLexicalProjectionMetadataV1 {
            generation: CodeGenerationId::new("generation.artifact-builder.v1")
                .expect("generation"),
            repository_id: None,
            logical_paths: Default::default(),
            freshness: SourceFreshness {
                source_namespace: SourceNamespace::new("namespace.artifact-builder")
                    .expect("namespace"),
                source_instance: SourceInstanceKey::new("instance.artifact-builder")
                    .expect("instance"),
                source_watermark: None,
                projection_watermark: None,
                observed_at: UtcMicros(0),
                source_generation: None,
                generation_lag: None,
                compatibility: FreshnessCompatibilityV1::Unknown,
                policy_revision: ComponentRevision::new("policy.artifact-builder.v1")
                    .expect("policy"),
            },
            exact_retriever_revision: ComponentRevision::new("retriever.exact.artifact.v1")
                .expect("exact retriever"),
            lexical_retriever_revision: ComponentRevision::new("retriever.lexical.artifact.v1")
                .expect("lexical retriever"),
            exact_score_domain: ScoreDomainId::new("score.exact.artifact.v1")
                .expect("score domain"),
            clone_route: None,
        }
    }

    fn create_mutable_test_schema(connection: &Connection) -> BuilderMutationGuardV1 {
        let gate =
            register_builder_mutation_gate(connection).expect("register builder mutation gate");
        create_schema(connection).expect("create artifact schema");
        BuilderMutationGuardV1::enter(&gate).expect("enter test builder mutation authority")
    }

    /// Fingerprint postings are appended to an arrival-ordered staging table
    /// and reach the keyed tree only through the sorted finalization pass:
    /// the staging table is gated like every other builder table, the pass
    /// seals one list per fingerprint holding every posting in key order, and
    /// nothing of the staging table survives it (so a finalized artifact
    /// verifies without it).
    #[test]
    fn fingerprint_postings_stage_in_arrival_order_and_seal_one_list_per_fingerprint() {
        let mut connection = Connection::open_in_memory().expect("fingerprint database");
        let gate =
            register_builder_mutation_gate(&connection).expect("register builder mutation gate");
        create_schema(&connection).expect("create artifact schema");
        verify_builder_mutation_gate_schema(&connection).expect("staging triggers present");

        let refused = connection.execute(
            "INSERT INTO clone_fingerprint_postings_pages(language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position) VALUES ('rust', 1, 1, 9, 2, 0)",
            [],
        );
        assert!(
            refused
                .expect_err("ungated staging insert must be refused")
                .to_string()
                .contains("private lexical builder mutation required")
        );

        // Arrival order deliberately disagrees with key order.
        let arrivals = [
            ("rust", 9_i64, 2_i64, 3_i64),
            ("rust", 2, 2, 1),
            ("go", 5, 1, 0),
            ("rust", 2, 1, 7),
            ("rust", 2, 1, 4),
        ];
        {
            let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
            for (language, fingerprint, occurrence, position) in arrivals {
                connection
                    .execute(
                        "INSERT INTO clone_fingerprint_postings_pages(language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position) VALUES (?1, 1, 1, ?2, ?3, ?4)",
                        params![language, fingerprint, occurrence, position],
                    )
                    .expect("stage fingerprint posting");
            }
        }
        let keyed_before: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM clone_fingerprint_postings",
                [],
                |row| row.get(0),
            )
            .expect("count keyed rows");
        assert_eq!(keyed_before, 0, "appends must not touch the keyed tree");

        let transaction = connection.transaction().expect("finalization transaction");
        derive_clone_fingerprint_postings(&transaction, &gate, &ActiveControl)
            .expect("sorted pass");
        transaction.commit().expect("commit sorted pass");

        assert!(
            !table_exists(&connection, "clone_fingerprint_postings_pages").expect("table probe"),
            "staging table must not survive finalization"
        );
        verify_builder_mutation_gate_schema(&connection)
            .expect("finalized layout verifies without the staging table");
        let mut statement = connection
            .prepare(
                "SELECT language, fingerprint, posting_count, postings FROM clone_fingerprint_postings ORDER BY language, class, normalization_revision, fingerprint",
            )
            .expect("prepare keyed read");
        let sealed = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .expect("read keyed rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect keyed rows")
            .into_iter()
            .map(|(language, fingerprint, count, postings)| {
                let postings = decode_fingerprint_postings(&postings).expect("canonical postings");
                (language, fingerprint, count, postings)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sealed,
            vec![
                ("go".to_owned(), 5, 1, vec![(1, 0)]),
                ("rust".to_owned(), 2, 3, vec![(1, 4), (1, 7), (2, 1)]),
                ("rust".to_owned(), 9, 1, vec![(2, 3)]),
            ]
        );
    }

    #[test]
    fn field_statistics_preserve_posting_sums_and_omit_absent_fields() {
        for empty in [false, true] {
            let mut connection = Connection::open_in_memory().expect("statistics database");
            let _authority = create_mutable_test_schema(&connection);
            if !empty {
                // Two committed batches: the staged totals must accumulate
                // across them exactly as a scan over every posting would.
                let batches: [&[(i64, i64, i64, i64)]; 2] = [
                    &[(1, 1, 1, 2), (2, 1, 1, 3), (1, 3, 1, 7)],
                    &[(1, 1, 2, 4), (2, 7, 2, 11)],
                ];
                for batch in batches {
                    let transaction = connection.transaction().expect("batch transaction");
                    let mut totals = BTreeMap::new();
                    for (_, field, _, frequency) in batch {
                        *totals.entry(*field).or_insert(0) += frequency;
                    }
                    stage_field_totals(&transaction, &totals).expect("stage field totals");
                    transaction.commit().expect("commit batch");
                }
            }
            let expected: Vec<(i64, i64)> = if empty {
                Vec::new()
            } else {
                vec![(1, 2 + 3 + 4), (3, 7), (7, 11)]
            };
            let transaction = connection.transaction().expect("statistics transaction");
            derive_statistics_step(&transaction, 0, &ActiveControl).expect("derive statistics");
            let actual = transaction
                .prepare("SELECT field, total_length FROM field_stats ORDER BY field")
                .expect("derived sums")
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
                .expect("derived rows")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("derived totals");
            assert_eq!(actual, expected);
            assert!(
                transaction
                    .execute("INSERT INTO field_stats VALUES (2, 5)", [])
                    .is_err()
            );
            assert!(
                !table_exists(&transaction, "field_stats_staging").expect("staging lookup"),
                "sealing the statistics must drop the staging totals"
            );
        }
    }

    #[test]
    fn resume_refuses_staging_without_incremental_field_statistics() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("no-field-stats-staging.sqlite");
        let metadata = test_metadata();
        drop(
            CodeLexicalArtifactBuilderV1::create(&path, metadata.clone())
                .expect("create current staging artifact"),
        );
        let connection = Connection::open(&path).expect("open staging artifact for fixture setup");
        connection
            .execute_batch("DROP TABLE field_stats_staging;")
            .expect("install pre-incremental staging layout");
        drop(connection);

        let error =
            match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &path,
                metadata,
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &ActiveControl,
            ) {
                Ok(_) => panic!("resume must not seal statistics without staged totals"),
                Err(error) => error,
            };
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)));
    }

    /// A revision-16 staging file written by the builder that inserted
    /// fingerprint postings straight into the keyed tree has no staging table;
    /// resuming it must be a typed incompatibility (discard and restage), not
    /// a missing-table `Io` the scheduler would retry forever.
    #[test]
    fn resume_refuses_staging_without_fingerprint_posting_pages() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("no-fingerprint-staging.sqlite");
        let metadata = test_metadata();
        drop(
            CodeLexicalArtifactBuilderV1::create(&path, metadata.clone())
                .expect("create current staging artifact"),
        );
        let connection = Connection::open(&path).expect("open staging artifact for fixture setup");
        connection
            .execute_batch("DROP TABLE clone_fingerprint_postings_pages;")
            .expect("install pre-staging fingerprint layout");
        drop(connection);

        let error =
            match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &path,
                metadata,
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &ActiveControl,
            ) {
                Ok(_) => panic!("resume must not accept a staging file without fingerprint pages"),
                Err(error) => error,
            };
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)));
    }

    #[test]
    fn canonical_write_limit_refuses_ngram_receipt_past_the_exact_boundary() {
        let ngram_receipt_bytes = "sha256:".len() + 64;
        let mut ledger = CanonicalBatchLimitLedgerV1::default();
        assert!(
            ledger
                .try_admit(
                    0,
                    CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1
                        - ngram_receipt_bytes,
                )
                .expect("admit bytes below the ngram receipt boundary")
                .is_none()
        );
        let exceeded = ledger
            .try_admit(0, ngram_receipt_bytes + 1)
            .expect("evaluate ngram receipt boundary")
            .expect("one byte past the write boundary must be refused");
        assert_eq!(
            exceeded.limit,
            CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes
        );
        assert_eq!(
            exceeded.required,
            CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1 + 1
        );
    }

    #[test]
    fn receipt_verification_does_not_read_append_only_base_tables() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        connection
            .authorizer(Some(
                |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                    // The vocabulary is the sealed terms and their fuzzy
                    // flags; their posting lists stay unread.
                    AuthAction::Read {
                        table_name: "term_postings",
                        column_name,
                    } if column_name != "lists" => Authorization::Allow,
                    AuthAction::Read { table_name, .. }
                        if BASE_SECTION_NAMES.contains(&table_name)
                            || table_name.ends_with("_runs") =>
                    {
                        Authorization::Deny
                    }
                    _ => Authorization::Allow,
                },
            ))
            .expect("deny exhaustive base-table verification reads");

        let sections = compute_section_digests(&connection, &ActiveControl)
            .expect("verify only source-page receipts and derived sections");
        assert_eq!(
            sections
                .iter()
                .map(|section| section.name.as_str())
                .collect::<Vec<_>>(),
            SECTION_NAMES
        );
    }

    #[test]
    fn finalization_monitor_spawn_failure_is_typed_and_leaves_sqlite_reusable() {
        let mut connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        let transaction = connection.transaction().expect("start transaction");
        fail_next_finalization_monitor_spawn();
        let operation_ran = AtomicBool::new(false);
        let error = with_cancellable_sqlite_statement(&transaction, &ActiveControl, || {
            operation_ran.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect_err("injected monitor spawn failure must be typed");
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Io(_)));
        assert!(
            !operation_ran.load(Ordering::SeqCst),
            "SQLite work must not start without its cancellation monitor"
        );
        with_cancellable_sqlite_statement(&transaction, &ActiveControl, || {
            transaction
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .map_err(sqlite_error)
        })
        .expect("progress handler is cleared after spawn failure");
    }

    #[test]
    fn posting_staging_keeps_source_page_order_without_a_serving_index() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        for (page_ordinal, term) in [(0i64, "omega"), (0, "zeta"), (1, "alpha"), (1, "beta")] {
            connection
                .execute(
                    "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, ?2, 4, X'02')",
                    params![page_ordinal, term],
                )
                .expect("seed page-ordered term run");
        }
        assert!(
            !table_exists(&connection, "ngram_posting_pages").expect("table probe"),
            "n-gram lists are rebuilt from rows and never staged"
        );
        for table in ["term_posting_runs", "exact_posting_runs", "row_chunk_pages"] {
            let secondary_indexes: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list(?1) WHERE origin != 'pk'",
                    [table],
                    |row| row.get(0),
                )
                .expect("inspect staging indexes");
            assert_eq!(
                secondary_indexes, 0,
                "{table}: serving-key maintenance must remain absent during catch-up"
            );
        }

        let mut statement = connection
            .prepare(
                "SELECT page_ordinal, term FROM term_posting_runs ORDER BY page_ordinal, term, field",
            )
            .expect("prepare staging-order scan");
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .expect("scan staging order")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect staging order");
        assert_eq!(
            rows,
            [
                (0, "omega".to_owned()),
                (0, "zeta".to_owned()),
                (1, "alpha".to_owned()),
                (1, "beta".to_owned())
            ]
        );
        assert_eq!(
            statement.get_status(StatementStatus::Sort),
            0,
            "source-page catch-up must be the maintained table order"
        );
    }

    /// A pass that outgrows its memory sheds its highest keys and a later
    /// pass rebuilds exactly them: the union of every pass equals one
    /// unbounded pass, list for list, in key order.
    #[test]
    fn bounded_ngram_passes_rebuild_exactly_the_unbounded_lists() {
        let postings = (0u32..600)
            .flat_map(|document| {
                (0i64..40)
                    .filter(move |ngram| (document as i64 + ngram) % 3 != 0)
                    .map(move |ngram| ((ngram % 2, ngram), document))
            })
            .collect::<Vec<_>>();
        let run = |memory_bytes: usize| {
            let mut sealed = Vec::new();
            let mut passes = 0;
            let mut lower = None;
            loop {
                passes += 1;
                let mut pass = NgramListPassV1::new(lower, memory_bytes);
                for (key, document) in &postings {
                    pass.add(*key, *document).expect("ascending documents");
                }
                let (lists, cutoff) = pass.finish();
                sealed.extend(
                    lists
                        .into_iter()
                        .map(|(key, list)| (key, list.finish().expect("non-empty list"))),
                );
                match cutoff {
                    Some(cutoff) => lower = Some(cutoff),
                    None => break,
                }
            }
            (sealed, passes)
        };
        let (unbounded, single) = run(usize::MAX);
        let (bounded, passes) = run(4 * 1024);
        assert_eq!(single, 1);
        assert!(
            passes > 2,
            "the bounded rebuild must actually spill: {passes} passes"
        );
        assert_eq!(bounded, unbounded);
        assert!(
            unbounded.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "sealed lists arrive in key order"
        );
    }

    fn encoded_postings(frequencies: bool, postings: &[(u32, u32)]) -> Vec<u8> {
        let mut encoder = PostingListEncoderV1::new(frequencies);
        for (document, frequency) in postings {
            encoder
                .push(*document, *frequency)
                .expect("ascending posting");
        }
        encoder.finish().expect("non-empty posting list")
    }

    fn decoded_postings(frequencies: bool, encoded: &[u8]) -> Vec<(u32, u32)> {
        PostingListDecoderV1::new(encoded, frequencies)
            .collect::<Result<Vec<_>, _>>()
            .expect("canonical posting list")
    }

    /// Finalization concatenates each key's page-ordered staging runs into
    /// exactly one sealed list, drops the staging tables, freezes the
    /// sealed tables, and serves every read from the clustered key alone.
    #[test]
    fn posting_merges_seal_one_list_per_serving_key() {
        let mut connection = Connection::open_in_memory().expect("open artifact database");
        let gate =
            register_builder_mutation_gate(&connection).expect("register builder mutation gate");
        create_schema(&connection).expect("create artifact schema");
        {
            let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
            for (page_ordinal, term, field, postings) in [
                (0i64, "render", 4i64, vec![(1u32, 1u32), (2, 3)]),
                (0, "render", 7, vec![(4, 1)]),
                (0, "widget", 7, vec![(2, 1)]),
                (2, "render", 4, vec![(7, 2)]),
            ] {
                connection
                    .execute(
                        "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, ?2, ?3, ?4)",
                        params![page_ordinal, term, field, encoded_postings(true, &postings)],
                    )
                    .expect("seed term run");
            }
            for (page_ordinal, documents) in [(0i64, vec![(1u32, 1u32)]), (2, vec![(7, 1)])] {
                connection
                    .execute(
                        "INSERT INTO exact_posting_runs(page_ordinal, term_id, field, documents) VALUES (?1, 9, 1, ?2)",
                        params![page_ordinal, encoded_postings(false, &documents)],
                    )
                    .expect("seed exact run");
            }
        }

        let transaction = connection.transaction().expect("merge transaction");
        let generation = test_metadata().generation;
        let authority = ServingIndexStepAuthorityV1 {
            mutation_gate: &gate,
            generation: &generation,
            ngram_memory_bytes: 1024 * 1024,
        };
        for ordinal in [1, 2, 3] {
            build_serving_index_step(&transaction, ordinal, &authority, &ActiveControl)
                .expect("merge posting family");
        }
        derive_statistics_step(&transaction, 1, &ActiveControl).expect("release staging pages");
        transaction.commit().expect("commit merges");

        for table in ["term_posting_runs", "exact_posting_runs"] {
            assert!(
                !table_exists(&connection, table).expect("table probe"),
                "{table} must not survive finalization"
            );
        }
        verify_builder_mutation_gate_schema(&connection)
            .expect("sealed layout verifies without the staging tables");
        let free_pages: i64 = connection
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .expect("read freelist");
        assert_eq!(
            free_pages, 0,
            "every page the dropped staging tables held leaves the file"
        );
        let ngram_lists: i64 = connection
            .query_row("SELECT COUNT(*) FROM ngram_postings", [], |row| row.get(0))
            .expect("count ngram lists");
        assert_eq!(ngram_lists, 0, "a corpus without rows seals no n-gram list");
        let terms = connection
            .prepare("SELECT term, in_fuzzy, lists FROM term_postings ORDER BY term")
            .expect("prepare term read")
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .expect("read term lists")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect term lists")
            .into_iter()
            .map(|(term, in_fuzzy, lists)| {
                let lists = decode_term_lists(&lists)
                    .expect("canonical term lists")
                    .into_iter()
                    .map(|(field, frequency, postings)| {
                        (field, frequency, decoded_postings(true, postings))
                    })
                    .collect::<Vec<_>>();
                (term, in_fuzzy, lists)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            terms,
            [
                (
                    "render".to_owned(),
                    true,
                    vec![(4, 3, vec![(1, 1), (2, 3), (7, 2)]), (7, 1, vec![(4, 1)])]
                ),
                // A term seen only as a subtoken is not fuzzy-eligible.
                ("widget".to_owned(), false, vec![(7, 1, vec![(2, 1)])]),
            ]
        );
        let exact: Vec<u8> = connection
            .query_row(
                "SELECT documents FROM exact_postings WHERE term_id = 9 AND field = 1",
                [],
                |row| row.get(0),
            )
            .expect("read exact list");
        assert_eq!(decoded_postings(false, &exact), [(1, 1), (7, 1)]);

        for table in ["term_postings", "exact_postings", "ngram_postings"] {
            let secondary_indexes: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list(?1) WHERE origin != 'pk'",
                    [table],
                    |row| row.get(0),
                )
                .expect("inspect sealed indexes");
            assert_eq!(secondary_indexes, 0, "{table} carries no duplicate index");
        }
        let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
        assert!(
            connection
                .execute(
                    "INSERT INTO term_postings(term, in_fuzzy, lists) VALUES ('late', 1, X'040102')",
                    [],
                )
                .is_err(),
            "sealed term postings must be frozen even under builder authority"
        );
        let plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT documents, document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
            )
            .expect("prepare ngram serving plan")
            .query_map(params![1i64, 10i64], |row| row.get::<_, String>(3))
            .expect("query ngram serving plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ngram serving plan");
        assert!(
            plan.iter()
                .any(|detail| detail.contains("USING PRIMARY KEY")),
            "phrase candidates must seek the clustered key, got {plan:?}"
        );
    }

    #[test]
    fn posting_merges_refuse_overlapping_staging() {
        for (runs, message) in [
            (
                [(0i64, vec![(1u32, 1u32), (4, 1)]), (1, vec![(3, 1)])],
                "overlap",
            ),
            ([(0, vec![(1, 1)]), (1, vec![(1, 1)])], "overlap"),
        ] {
            let mut connection = Connection::open_in_memory().expect("open artifact database");
            let gate = register_builder_mutation_gate(&connection).expect("register gate");
            create_schema(&connection).expect("create schema");
            {
                let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
                for (page_ordinal, postings) in &runs {
                    connection
                        .execute(
                            "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, 'render', 4, ?2)",
                            params![page_ordinal, encoded_postings(true, postings)],
                        )
                        .expect("seed term run");
                }
            }
            let transaction = connection.transaction().expect("merge transaction");
            let generation = test_metadata().generation;
            let authority = ServingIndexStepAuthorityV1 {
                mutation_gate: &gate,
                generation: &generation,
                ngram_memory_bytes: 1024 * 1024,
            };
            let error = build_serving_index_step(&transaction, 1, &authority, &ActiveControl)
                .expect_err("out-of-order staging must fail closed");
            assert!(
                matches!(&error, CodeLexicalArtifactErrorV1::Corrupt(detail) if detail.contains(message)),
                "unexpected error: {error:?}"
            );
        }
    }

    #[test]
    fn resume_refuses_the_superseded_ngram_staging_layout() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("superseded-ngram-layout.sqlite");
        let metadata = test_metadata();
        drop(
            CodeLexicalArtifactBuilderV1::create(&path, metadata.clone())
                .expect("create current staging artifact"),
        );
        let connection = Connection::open(&path).expect("open staging artifact for fixture setup");
        connection
            .execute_batch(
                "ALTER TABLE ngram_postings RENAME TO current_ngram_postings;
                 CREATE TABLE ngram_postings (
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    document_id INTEGER NOT NULL,
                    PRIMARY KEY(kind, ngram, document_id)
                 ) WITHOUT ROWID;
                 DROP TABLE current_ngram_postings;",
            )
            .expect("install superseded branch-local layout");
        drop(connection);

        let error =
            match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &path,
                metadata,
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &ActiveControl,
            ) {
                Ok(_) => panic!("resume must not mix superseded and current ngram layouts"),
                Err(error) => error,
            };
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)));
    }

    #[test]
    fn persisted_progress_tail_seeks_latest_maintained_cursor_without_full_scan() {
        let connection = Connection::open_in_memory().expect("open progress database");
        connection
            .execute_batch(
                "CREATE TABLE source_page_cursors (
                    page_ordinal INTEGER PRIMARY KEY,
                    next_cursor BLOB NOT NULL
                );",
            )
            .expect("create source progress table");
        for page in 0..4_096i64 {
            connection
                .execute(
                    "INSERT INTO source_page_cursors(page_ordinal, next_cursor) VALUES (?1, X'00')",
                    [page],
                )
                .expect("seed persisted progress");
        }

        let mut statement = connection
            .prepare(PROGRESS_TAIL_QUERY)
            .expect("prepare maintained progress lookup");
        let latest: i64 = statement
            .query_row([], |row| row.get(0))
            .expect("read latest progress row");

        assert_eq!(latest, 4_095);
        assert_eq!(
            statement.get_status(StatementStatus::FullscanStep),
            0,
            "progress must seek the latest maintained cursor regardless of page count"
        );
        assert_eq!(
            statement.get_status(StatementStatus::Sort),
            0,
            "the primary-key tail lookup must not build a temporary order"
        );
    }

    /// Finalization adopts the base sections from their page receipts, so a
    /// resumed digest wake walks only the source pages and the derived
    /// statistics natively.
    #[test]
    fn bounded_finalization_resume_seeks_each_native_section_index() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        connection
            .execute_batch(
                "INSERT INTO source_pages(page_ordinal, chunk_count, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt) VALUES (0, 1, 1, 1, 'imports', 'ngrams', X'00');
                 INSERT INTO field_stats(field, total_length) VALUES (1, 1);
                 INSERT INTO term_postings(term, in_fuzzy, lists) VALUES ('term', 1, X'01010102');",
            )
            .expect("seed natively walked sections");

        for section in [
            FinalizationSectionV1::SourcePages,
            FinalizationSectionV1::FieldStatistics,
            FinalizationSectionV1::Vocabulary,
        ] {
            let plan = explain_native_seek_plan(&connection, section)
                .expect("explain bounded finalization resume query");
            assert!(
                plan.iter().any(|detail| detail.contains("SEARCH")),
                "{section:?} must seek an indexed native key, got {plan:?}"
            );
            assert!(
                plan.iter().all(|detail| !detail.contains("SCAN")),
                "{section:?} must not rescan the section on a resumed wake, got {plan:?}"
            );
            assert!(
                plan.iter().all(|detail| !detail.contains("TEMP B-TREE")),
                "{section:?} must not sort the section on a resumed wake, got {plan:?}"
            );
        }
    }

    fn explain_native_seek_plan(
        connection: &Connection,
        section: FinalizationSectionV1,
    ) -> Result<Vec<String>, CodeLexicalArtifactErrorV1> {
        let query = format!("EXPLAIN QUERY PLAN {}", section.walked_query(true)?);
        let mut statement = connection.prepare(&query).map_err(sqlite_error)?;
        let mut rows = match section {
            FinalizationSectionV1::SourcePages | FinalizationSectionV1::FieldStatistics => {
                statement.query(params![0i64, 1i64])
            }
            FinalizationSectionV1::Vocabulary => statement.query(params!["", 1i64]),
            other => panic!("{other:?} is not walked natively"),
        }
        .map_err(sqlite_error)?;
        let mut details = Vec::new();
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            details.push(row.get(3).map_err(sqlite_error)?);
        }
        Ok(details)
    }

    #[cfg(unix)]
    #[test]
    fn staging_open_refuses_a_symlink_even_when_its_target_is_private() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("private staging directory");
        let target = directory.path().join("target.sqlite");
        drop(create_private_file_retained(&target).expect("create private target"));
        let linked = directory.path().join("linked.sqlite");
        symlink(&target, &linked).expect("link private target");

        assert!(matches!(
            open_private_builder_connection(
                &linked,
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            ),
            Err(CodeLexicalArtifactErrorV1::Contract(_))
        ));
    }

    #[test]
    fn staging_operations_refuse_same_name_replacement_after_open() {
        let directory = tempfile::tempdir().expect("private staging directory");
        let artifact = directory.path().join("artifact.sqlite");
        let replacement = directory.path().join("replacement.sqlite");
        let (connection, _retained, identity) = create_private_builder_connection(
            &artifact,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
        .expect("open artifact");
        drop(connection);
        drop(create_private_file_retained(&replacement).expect("create replacement"));
        std::fs::rename(&replacement, &artifact).expect("replace staged artifact");

        assert!(matches!(
            verify_staging_file_binding(&artifact, &identity),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
    }

    #[test]
    fn interrupted_staging_initialization_leaves_no_resumable_path() {
        let directory = tempfile::tempdir().expect("private staging directory");
        let staging = directory.path().join(".text-artifact-restart.staging");

        fail_next_staging_initialization();
        assert!(matches!(
            CodeLexicalArtifactBuilderV1::create(&staging, test_metadata()),
            Err(CodeLexicalArtifactErrorV1::Contract(_))
        ));
        // The staging path's existence is the resume signal, so it must never
        // appear without its singleton `artifact_state` row: a later
        // incarnation would read the empty row set as corruption and park a
        // projection that its sealed generation can still rederive.
        assert!(
            !staging.exists(),
            "staging path is visible without its committed state row"
        );

        drop(
            CodeLexicalArtifactBuilderV1::create(&staging, test_metadata())
                .expect("create a staging artifact over the interrupted attempt"),
        );
        drop(
            CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &staging,
                test_metadata(),
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &ActiveControl,
            )
            .expect("resume the published staging artifact"),
        );
    }
}
