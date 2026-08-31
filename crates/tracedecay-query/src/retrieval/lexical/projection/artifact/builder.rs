use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::BinaryHeap;
use std::fs::File;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use rayon::prelude::*;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    CodeIndexImportEvidenceV1, ExtractionAdmittedCodeSearchChunkV1,
};
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_domain::{
    CodeSearchChunkAnchorV1, CodeSearchChunkV1, ExactTechnicalTermV1, FileOccurrenceId,
    ManifestDigest,
};
use tracedecay_private_fs::{create_private_file_retained, open_private_file};

use super::format::{
    BASE_SECTION_NAMES, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
    CodeLexicalArtifactSectionDigestV1, RECEIPT_RESERVATION_BYTES, SECTION_NAMES,
    VerifiedCodeLexicalArtifactV1, absorb_page_base_sections_receipt, artifact_digest,
    decode_padded_receipt, decode_padded_receipt_with_control, encode_field,
    finish_base_section_receipt_fold, initial_base_section_receipt_fold, metadata_digest,
    new_verified_receipt, padded_receipt, verify_artifact_table_layout,
    verify_required_artifact_indexes,
};
use super::postings::document_ngram_scratch;
use super::prepared::{
    PreparedCodeLexicalArtifactPageV1, PreparedTermPostingV1, prepare_page as prepare_page_values,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1, CodeLexicalArtifactBatchLimitV1,
    CodeLexicalArtifactErrorV1, NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1, checkpoint,
    open_builder_connection, sqlite_corrupt, sqlite_error,
};
use crate::retrieval::lexical::LexicalFieldV1;

use super::super::CodeLexicalProjectionMetadataV1;

const PROGRESS_TAIL_QUERY: &str = "SELECT page_ordinal, import_dictionary_digest, cumulative_digest, next_cursor \
     FROM source_pages ORDER BY page_ordinal DESC LIMIT 1";
const FINALIZATION_PROGRESS_INTERVAL_OPS: i32 = 4_096;
const FINALIZATION_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(1);
// A plan entry owns one document id and one posting reference. Three words
// cover its portable 32-bit layout and conservatively exceed its 64-bit
// layout; general allocator metadata remains outside the ledger contract.
const TERM_INSERT_PLAN_BYTES_PER_REF: usize = 3 * std::mem::size_of::<usize>();
const TERM_INSERT_CONTROL_INTERVAL: usize = 4_096;
const TERM_INSERT_SORT_RUN_ROWS: usize = 4_096;
// An exact-posting plan entry owns one document id plus a field/term BLOB
// key carried as borrowed fat-pointer slices (`&str` and `&[u8]`, two words
// each) rather than the single thin reference a term-posting entry holds.
// Six words conservatively covers both the 32-bit and 64-bit layouts;
// general allocator metadata remains outside the ledger contract.
const EXACT_INSERT_PLAN_BYTES_PER_REF: usize = 6 * std::mem::size_of::<usize>();
const EXACT_INSERT_CONTROL_INTERVAL: usize = TERM_INSERT_CONTROL_INTERVAL;
const EXACT_INSERT_SORT_RUN_ROWS: usize = TERM_INSERT_SORT_RUN_ROWS;
// This gate serializes mutation within the private-profile/stable-handle
// authority. It denies ordinary second-connection DML, but is not a
// cryptographic defense against malicious same-UID code that deliberately
// registers a lookalike SQLite function.
const BUILDER_MUTATION_GATE_FUNCTION: &str = "tracedecay_lexical_builder_append_authorized";
const BUILDER_MUTATION_IDLE: u8 = 0;
const BUILDER_MUTATION_APPEND: u8 = 1;

#[derive(Clone, Copy)]
struct PreparedTermInsertRefV1<'a> {
    document_id: i64,
    posting: &'a PreparedTermPostingV1,
}

impl PreparedTermInsertRefV1<'_> {
    fn key(&self) -> (&str, &str, i64) {
        (
            self.posting.field.as_str(),
            self.posting.term.as_str(),
            self.document_id,
        )
    }
}

#[derive(Clone, Copy)]
struct PreparedTermMergeCursorV1<'a> {
    entry: PreparedTermInsertRefV1<'a>,
    run_index: usize,
    run_offset: usize,
}

impl PartialEq for PreparedTermMergeCursorV1<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.entry.key() == other.entry.key()
            && self.run_index == other.run_index
            && self.run_offset == other.run_offset
    }
}

impl Eq for PreparedTermMergeCursorV1<'_> {}

impl PartialOrd for PreparedTermMergeCursorV1<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for PreparedTermMergeCursorV1<'_> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.entry
            .key()
            .cmp(&other.entry.key())
            .then_with(|| self.run_index.cmp(&other.run_index))
            .then_with(|| self.run_offset.cmp(&other.run_offset))
    }
}

struct PreparedTermInsertPlanV1<'a> {
    entries: Vec<PreparedTermInsertRefV1<'a>>,
    merge_heap: BinaryHeap<Reverse<PreparedTermMergeCursorV1<'a>>>,
}

// `exact_postings` shares `term_postings`'s clustered key shape (field,
// term, document_id) so the same bounded k-way merge sort pattern applies:
// insert in `PRIMARY KEY`/`WITHOUT ROWID` clustered order instead of raw
// arrival order to avoid B-tree page splits on out-of-order inserts.
#[derive(Clone, Copy)]
struct PreparedExactInsertRefV1<'a> {
    document_id: i64,
    field: &'a str,
    term: &'a [u8],
}

impl PreparedExactInsertRefV1<'_> {
    fn key(&self) -> (&str, &[u8], i64) {
        (self.field, self.term, self.document_id)
    }
}

#[derive(Clone, Copy)]
struct PreparedExactMergeCursorV1<'a> {
    entry: PreparedExactInsertRefV1<'a>,
    run_index: usize,
    run_offset: usize,
}

impl PartialEq for PreparedExactMergeCursorV1<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.entry.key() == other.entry.key()
            && self.run_index == other.run_index
            && self.run_offset == other.run_offset
    }
}

impl Eq for PreparedExactMergeCursorV1<'_> {}

impl PartialOrd for PreparedExactMergeCursorV1<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for PreparedExactMergeCursorV1<'_> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.entry
            .key()
            .cmp(&other.entry.key())
            .then_with(|| self.run_index.cmp(&other.run_index))
            .then_with(|| self.run_offset.cmp(&other.run_offset))
    }
}

struct PreparedExactInsertPlanV1<'a> {
    entries: Vec<PreparedExactInsertRefV1<'a>>,
    merge_heap: BinaryHeap<Reverse<PreparedExactMergeCursorV1<'a>>>,
}

const BUILDER_GATE_TRIGGER_LAYOUT: [(&str, &str, &str); 14] = [
    ("builder_gate_source_pages_insert", "source_pages", "INSERT"),
    (
        "builder_gate_document_integrity_insert",
        "document_integrity",
        "INSERT",
    ),
    (
        "builder_gate_import_integrity_insert",
        "import_integrity",
        "INSERT",
    ),
    (
        "builder_gate_import_evidence_insert",
        "import_evidence",
        "INSERT",
    ),
    ("builder_gate_rows_insert", "rows", "INSERT"),
    ("builder_gate_rows_update", "rows", "UPDATE"),
    ("builder_gate_rows_delete", "rows", "DELETE"),
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
const IMMUTABLE_TRIGGER_LAYOUT: [(&str, &str, &str, &str); 10] = [
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
        "immutable_document_integrity_update",
        "document_integrity",
        "UPDATE",
        "immutable lexical document integrity",
    ),
    (
        "immutable_document_integrity_delete",
        "document_integrity",
        "DELETE",
        "immutable lexical document integrity",
    ),
    (
        "immutable_import_integrity_update",
        "import_integrity",
        "UPDATE",
        "immutable lexical import integrity",
    ),
    (
        "immutable_import_integrity_delete",
        "import_integrity",
        "DELETE",
        "immutable lexical import integrity",
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

struct BuilderMutationGuardV1 {
    gate: Arc<AtomicU8>,
}

impl BuilderMutationGuardV1 {
    fn enter(gate: &Arc<AtomicU8>) -> Result<Self, CodeLexicalArtifactErrorV1> {
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

fn register_builder_mutation_gate(
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
    TermStatistics,
    Vocabulary,
}

impl FinalizationSectionV1 {
    const ALL: [Self; 11] = [
        Self::SourcePages,
        Self::DocumentIntegrity,
        Self::ImportIntegrity,
        Self::ImportEvidence,
        Self::Rows,
        Self::TermPostings,
        Self::ExactPostings,
        Self::NgramPostings,
        Self::FieldStatistics,
        Self::TermStatistics,
        Self::Vocabulary,
    ];

    fn from_ordinal(ordinal: usize) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::ALL.get(ordinal).copied().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization selected an unknown section".to_owned(),
            )
        })
    }

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
            Self::FieldStatistics => "field_stats",
            Self::TermStatistics => "term_stats",
            Self::Vocabulary => "vocabulary",
        }
    }

    const fn full_query(self) -> &'static str {
        match self {
            Self::SourcePages => {
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt, next_cursor FROM source_pages ORDER BY page_ordinal"
            }
            Self::DocumentIntegrity => {
                "SELECT document_id, chunk_id, digest FROM document_integrity ORDER BY document_id"
            }
            Self::ImportIntegrity => {
                "SELECT canonical, digest FROM import_integrity ORDER BY canonical"
            }
            Self::ImportEvidence => {
                "SELECT canonical, evidence FROM import_evidence ORDER BY canonical"
            }
            Self::Rows => "SELECT document_id, chunk_id, row FROM rows ORDER BY document_id",
            Self::TermPostings => {
                "SELECT field, term, document_id, frequency FROM term_postings ORDER BY field, term, document_id"
            }
            Self::ExactPostings => {
                "SELECT field, term, document_id FROM exact_postings ORDER BY field, term, document_id"
            }
            Self::NgramPostings => {
                "SELECT page_ordinal, kind, ngram, documents, cardinality FROM ngram_postings ORDER BY page_ordinal, kind, ngram"
            }
            Self::FieldStatistics => "SELECT field, total_length FROM field_stats ORDER BY field",
            Self::TermStatistics => {
                "SELECT field, term, document_frequency FROM term_stats ORDER BY field, term"
            }
            Self::Vocabulary => "SELECT term FROM vocabulary ORDER BY term",
        }
    }

    /// Bounded resumes seek a native table key, never a computed cursor.
    const fn seek_query(self, after: bool) -> &'static str {
        match (self, after) {
            (Self::SourcePages, false) => {
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt, next_cursor FROM source_pages ORDER BY page_ordinal LIMIT ?1"
            }
            (Self::SourcePages, true) => {
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt, next_cursor FROM source_pages WHERE page_ordinal > ?1 ORDER BY page_ordinal LIMIT ?2"
            }
            (Self::DocumentIntegrity, false) => {
                "SELECT document_id, chunk_id, digest FROM document_integrity ORDER BY document_id LIMIT ?1"
            }
            (Self::DocumentIntegrity, true) => {
                "SELECT document_id, chunk_id, digest FROM document_integrity WHERE document_id > ?1 ORDER BY document_id LIMIT ?2"
            }
            (Self::ImportIntegrity, false) => {
                "SELECT canonical, digest FROM import_integrity ORDER BY canonical LIMIT ?1"
            }
            (Self::ImportIntegrity, true) => {
                "SELECT canonical, digest FROM import_integrity WHERE canonical > ?1 ORDER BY canonical LIMIT ?2"
            }
            (Self::ImportEvidence, false) => {
                "SELECT canonical, evidence FROM import_evidence ORDER BY canonical LIMIT ?1"
            }
            (Self::ImportEvidence, true) => {
                "SELECT canonical, evidence FROM import_evidence WHERE canonical > ?1 ORDER BY canonical LIMIT ?2"
            }
            (Self::Rows, false) => {
                "SELECT document_id, chunk_id, row FROM rows ORDER BY document_id LIMIT ?1"
            }
            (Self::Rows, true) => {
                "SELECT document_id, chunk_id, row FROM rows WHERE document_id > ?1 ORDER BY document_id LIMIT ?2"
            }
            (Self::TermPostings, false) => {
                "SELECT field, term, document_id, frequency FROM term_postings ORDER BY field, term, document_id LIMIT ?1"
            }
            (Self::TermPostings, true) => {
                "SELECT field, term, document_id, frequency FROM term_postings WHERE (field, term, document_id) > (?1, ?2, ?3) ORDER BY field, term, document_id LIMIT ?4"
            }
            (Self::ExactPostings, false) => {
                "SELECT field, term, document_id FROM exact_postings ORDER BY field, term, document_id LIMIT ?1"
            }
            (Self::ExactPostings, true) => {
                "SELECT field, term, document_id FROM exact_postings WHERE (field, term, document_id) > (?1, ?2, ?3) ORDER BY field, term, document_id LIMIT ?4"
            }
            (Self::NgramPostings, false) => {
                "SELECT page_ordinal, kind, ngram, documents, cardinality FROM ngram_postings ORDER BY page_ordinal, kind, ngram LIMIT ?1"
            }
            (Self::NgramPostings, true) => {
                "SELECT page_ordinal, kind, ngram, documents, cardinality FROM ngram_postings WHERE (page_ordinal, kind, ngram) > (?1, ?2, ?3) ORDER BY page_ordinal, kind, ngram LIMIT ?4"
            }
            (Self::FieldStatistics, false) => {
                "SELECT field, total_length FROM field_stats ORDER BY field LIMIT ?1"
            }
            (Self::FieldStatistics, true) => {
                "SELECT field, total_length FROM field_stats WHERE field > ?1 ORDER BY field LIMIT ?2"
            }
            (Self::TermStatistics, false) => {
                "SELECT field, term, document_frequency FROM term_stats ORDER BY field, term LIMIT ?1"
            }
            (Self::TermStatistics, true) => {
                "SELECT field, term, document_frequency FROM term_stats WHERE (field, term) > (?1, ?2) ORDER BY field, term LIMIT ?3"
            }
            (Self::Vocabulary, false) => "SELECT term FROM vocabulary ORDER BY term LIMIT ?1",
            (Self::Vocabulary, true) => {
                "SELECT term FROM vocabulary WHERE term > ?1 ORDER BY term LIMIT ?2"
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
enum PersistedFinalizationKeyV1 {
    Integer(i64),
    Blob(Vec<u8>),
    Text(String),
    TextTextInteger {
        field: String,
        term: String,
        document_id: i64,
    },
    TextBlobInteger {
        field: String,
        term: Vec<u8>,
        document_id: i64,
    },
    IntegerIntegerInteger {
        page_ordinal: i64,
        kind: i64,
        ngram: i64,
    },
    TextText {
        field: String,
        term: String,
    },
}

impl PersistedFinalizationKeyV1 {
    fn matches_section(&self, section: FinalizationSectionV1) -> bool {
        matches!(
            (self, section),
            (
                Self::Integer(_),
                FinalizationSectionV1::SourcePages
                    | FinalizationSectionV1::DocumentIntegrity
                    | FinalizationSectionV1::Rows
            ) | (
                Self::Blob(_),
                FinalizationSectionV1::ImportIntegrity | FinalizationSectionV1::ImportEvidence
            ) | (
                Self::TextTextInteger { .. },
                FinalizationSectionV1::TermPostings
            ) | (
                Self::TextBlobInteger { .. },
                FinalizationSectionV1::ExactPostings
            ) | (
                Self::IntegerIntegerInteger { .. },
                FinalizationSectionV1::NgramPostings
            ) | (
                Self::Text(_),
                FinalizationSectionV1::FieldStatistics | FinalizationSectionV1::Vocabulary
            ) | (Self::TextText { .. }, FinalizationSectionV1::TermStatistics)
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedFinalizationPhaseV1 {
    Statistics,
    Indexes,
    Digest,
}

impl PersistedFinalizationPhaseV1 {
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
            FinalizationSectionV1::FieldStatistics => {
                hotpath::gauge!("query.artifact.finalization.phase.field_stats_total").inc(1u64);
            }
            FinalizationSectionV1::TermStatistics => {
                hotpath::gauge!("query.artifact.finalization.phase.term_stats_total").inc(1u64);
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
    _private_file: File,
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
        let (connection, private_file, file_identity) = create_private_builder_connection(path)?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        create_schema(&connection)?;
        verify_builder_mutation_gate_schema(&connection)?;
        let metadata_digest = metadata_digest(&metadata)?;
        let metadata_bytes = serde_json::to_vec(&metadata)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
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
        crate::hotpath_metrics::Residency::Cold.record("query.artifact.residency");
        Ok(Self {
            path: path.to_path_buf(),
            _private_file: private_file,
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
            open_private_builder_connection(path)
        )?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
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
        if receipt.is_some()
            || finalization
                .as_ref()
                .is_some_and(|state| state.phase == PersistedFinalizationPhaseV1::Digest)
        {
            verify_required_artifact_indexes(&connection)?;
        }
        validate_contiguous_pages(&connection, control)?;
        checkpoint(control)?;
        crate::hotpath_metrics::Residency::Rebuilding.record("query.artifact.residency");
        Ok(Self {
            path: path.to_path_buf(),
            _private_file: private_file,
            file_identity,
            connection,
            mutation_gate,
            metadata: expected_metadata,
            metadata_digest: expected_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    pub fn progress(
        &self,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        progress(&self.connection)
    }

    /// The ledger bytes charged regardless of page content: the SQLite
    /// page-cache authority plus the builder-retained projection metadata.
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
        let mut term_insert_plan = hotpath::measure_block!(
            "query.artifact.batch.term_order",
            prepare_term_insert_plan(
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
                control,
            )
        )?;
        let mut exact_insert_plan = hotpath::measure_block!(
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
                    "query.artifact.batch.rows",
                    append_prepared_rows(&transaction, pages, control)
                )?;
                record_batch_row_metrics(pages);
                hotpath::measure_block!(
                    "query.artifact.batch.postings",
                    append_prepared_postings(
                        &transaction,
                        pages,
                        &mut term_insert_plan,
                        &mut exact_insert_plan,
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
            let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
            record_finalization_step(&step);
            return Ok(step);
        }

        if load_finalization_state(&self.connection)?.is_none() {
            let transaction = self.connection.transaction().map_err(sqlite_error)?;
            let mut transaction_metrics = FinalizationTransactionMetricsV1::new();
            verify_staged_source_chain(&transaction, source, control)?;
            let content_epoch = authenticated_authority_epoch(&transaction, source)?;
            install_base_freeze(&transaction)?;
            store_finalization_state(
                &transaction,
                &PersistedFinalizationStateV1::new(content_epoch, source)?,
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
                advance_pre_digest_work(&transaction, &mut state, control)
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
        let section_count = u64::try_from(SECTION_NAMES.len()).map_err(contract_number)?;
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
                let next = SECTION_NAMES
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

        if state.completed_sections.len() != SECTION_NAMES.len() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization completed with an invalid section receipt"
                    .to_owned(),
            ));
        }
        let sections = state.completed_sections;
        verify_final_sections_against_source(&sections, source)?;
        let artifact_digest = artifact_digest(
            &self.metadata_digest,
            source.source_state_digest(),
            source.format_revision(),
            source.page_count(),
            source.total_chunks(),
            source.total_payload_bytes(),
            source.total_imports(),
            source.import_payload_bytes(),
            source.import_dictionary_digest(),
            source.cumulative_digest(),
            &sections,
        )?;
        let file_size_bytes = sqlite_file_size(&transaction)?;
        let receipt = new_verified_receipt(
            self.metadata.clone(),
            self.metadata_digest.clone(),
            source,
            artifact_digest,
            sections,
            file_size_bytes,
        );
        transaction
            .execute(
                "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
                params![padded_receipt(&receipt)?],
            )
            .map_err(sqlite_error)?;
        transaction
            .execute("DELETE FROM finalization_state WHERE singleton = 1", [])
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        commit_finalization_transaction(transaction, &mut transaction_metrics)?;
        let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
        record_finalization_step(&step);
        Ok(step)
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

fn create_private_builder_connection(
    path: &Path,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = create_private_file_retained(path)
        .map_err(|failure| private_staging_error(failure.into_error()))?;
    open_bound_builder_connection(path, private_file)
}

fn open_private_builder_connection(
    path: &Path,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = open_private_file(path).map_err(private_staging_error)?;
    open_bound_builder_connection(path, private_file)
}

fn open_bound_builder_connection(
    path: &Path,
    private_file: File,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let identity = stable_file_identity(&private_file)?;
    let connection = open_builder_connection(path)?;
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
const PERSISTED_CURSOR_U64_FIELDS: usize = 9;
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
        || memory_budget_bytes > CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1
    {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "lexical artifact build memory budget must be within 1..={CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1} bytes"
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
    let entries = term_rows
        .checked_mul(TERM_INSERT_PLAN_BYTES_PER_REF)
        .ok_or_else(batch_ledger_overflow)?;
    let runs = term_rows.div_ceil(TERM_INSERT_SORT_RUN_ROWS);
    let merge_heap = runs
        .checked_mul(std::mem::size_of::<PreparedTermMergeCursorV1<'static>>())
        .ok_or_else(batch_ledger_overflow)?;
    entries
        .checked_add(merge_heap)
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
    let entries = exact_rows
        .checked_mul(EXACT_INSERT_PLAN_BYTES_PER_REF)
        .ok_or_else(batch_ledger_overflow)?;
    let runs = exact_rows.div_ceil(EXACT_INSERT_SORT_RUN_ROWS);
    let merge_heap = runs
        .checked_mul(std::mem::size_of::<PreparedExactMergeCursorV1<'static>>())
        .ok_or_else(batch_ledger_overflow)?;
    entries
        .checked_add(merge_heap)
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
            entries.extend(
                document
                    .term_postings
                    .iter()
                    .map(|posting| PreparedTermInsertRefV1 {
                        document_id: document.document_id,
                        posting,
                    }),
            );
        }
    }
    for run in entries.chunks_mut(TERM_INSERT_SORT_RUN_ROWS) {
        checkpoint(control)?;
        run.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
        checkpoint(control)?;
    }
    checkpoint(control)?;

    let run_count = term_rows.div_ceil(TERM_INSERT_SORT_RUN_ROWS);
    let mut merge_heap = BinaryHeap::new();
    merge_heap.try_reserve_exact(run_count).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical term merge heap allocation failed: {error}"
        ))
    })?;
    for (run_index, run) in entries.chunks(TERM_INSERT_SORT_RUN_ROWS).enumerate() {
        checkpoint(control)?;
        let Some(entry) = run.first().copied() else {
            continue;
        };
        merge_heap.push(Reverse(PreparedTermMergeCursorV1 {
            entry,
            run_index,
            run_offset: 0,
        }));
    }
    Ok(PreparedTermInsertPlanV1 {
        entries,
        merge_heap,
    })
}

fn next_term_insert<'a>(
    plan: &mut PreparedTermInsertPlanV1<'a>,
) -> Result<Option<PreparedTermInsertRefV1<'a>>, CodeLexicalArtifactErrorV1> {
    let Some(Reverse(cursor)) = plan.merge_heap.pop() else {
        return Ok(None);
    };
    let next_offset = cursor
        .run_offset
        .checked_add(1)
        .ok_or_else(batch_ledger_overflow)?;
    if next_offset < TERM_INSERT_SORT_RUN_ROWS {
        let run_start = cursor
            .run_index
            .checked_mul(TERM_INSERT_SORT_RUN_ROWS)
            .ok_or_else(batch_ledger_overflow)?;
        let next_index = run_start
            .checked_add(next_offset)
            .ok_or_else(batch_ledger_overflow)?;
        let run_end = run_start
            .checked_add(TERM_INSERT_SORT_RUN_ROWS)
            .ok_or_else(batch_ledger_overflow)?
            .min(plan.entries.len());
        if next_index < run_end {
            let entry = plan.entries.get(next_index).copied().ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical term merge cursor escaped its bounded run".to_owned(),
                )
            })?;
            plan.merge_heap.push(Reverse(PreparedTermMergeCursorV1 {
                entry,
                run_index: cursor.run_index,
                run_offset: next_offset,
            }));
        }
    }
    Ok(Some(cursor.entry))
}

// Mirrors `prepare_term_insert_plan`/`next_term_insert` for `exact_postings`,
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
    for page in pages {
        checkpoint(control)?;
        for document in &page.documents {
            checkpoint(control)?;
            entries.extend(document.exact_postings.iter().map(|(field, term)| {
                PreparedExactInsertRefV1 {
                    document_id: document.document_id,
                    field: field.as_str(),
                    term: term.as_slice(),
                }
            }));
        }
    }
    for run in entries.chunks_mut(EXACT_INSERT_SORT_RUN_ROWS) {
        checkpoint(control)?;
        run.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
        checkpoint(control)?;
    }
    checkpoint(control)?;

    let run_count = exact_rows.div_ceil(EXACT_INSERT_SORT_RUN_ROWS);
    let mut merge_heap = BinaryHeap::new();
    merge_heap.try_reserve_exact(run_count).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical exact merge heap allocation failed: {error}"
        ))
    })?;
    for (run_index, run) in entries.chunks(EXACT_INSERT_SORT_RUN_ROWS).enumerate() {
        checkpoint(control)?;
        let Some(entry) = run.first().copied() else {
            continue;
        };
        merge_heap.push(Reverse(PreparedExactMergeCursorV1 {
            entry,
            run_index,
            run_offset: 0,
        }));
    }
    Ok(PreparedExactInsertPlanV1 {
        entries,
        merge_heap,
    })
}

fn next_exact_insert<'a>(
    plan: &mut PreparedExactInsertPlanV1<'a>,
) -> Result<Option<PreparedExactInsertRefV1<'a>>, CodeLexicalArtifactErrorV1> {
    let Some(Reverse(cursor)) = plan.merge_heap.pop() else {
        return Ok(None);
    };
    let next_offset = cursor
        .run_offset
        .checked_add(1)
        .ok_or_else(batch_ledger_overflow)?;
    if next_offset < EXACT_INSERT_SORT_RUN_ROWS {
        let run_start = cursor
            .run_index
            .checked_mul(EXACT_INSERT_SORT_RUN_ROWS)
            .ok_or_else(batch_ledger_overflow)?;
        let next_index = run_start
            .checked_add(next_offset)
            .ok_or_else(batch_ledger_overflow)?;
        let run_end = run_start
            .checked_add(EXACT_INSERT_SORT_RUN_ROWS)
            .ok_or_else(batch_ledger_overflow)?
            .min(plan.entries.len());
        if next_index < run_end {
            let entry = plan.entries.get(next_index).copied().ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical exact merge cursor escaped its bounded run".to_owned(),
                )
            })?;
            plan.merge_heap.push(Reverse(PreparedExactMergeCursorV1 {
                entry,
                run_index: cursor.run_index,
                run_offset: next_offset,
            }));
        }
    }
    Ok(Some(cursor.entry))
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
    let mut fresh_start = pages.len();
    let mut expected_fresh_ordinal = current.next_page_ordinal;
    for (index, page) in pages.iter().enumerate() {
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
        let persisted_previous;
        let previous = if index == 0 {
            persisted_previous = cursor_before_page(connection, page.page_ordinal())?;
            persisted_previous.as_ref()
        } else {
            pages
                .get(index - 1)
                .map(VerifiedSealedLexicalPageV1::next_cursor)
        };
        page.verify_transition(previous)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
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

/// The widest transient upper bound one staged chunk or import can require.
/// It is evaluated a record at a time without allocations and aborts once
/// `abort_above` is exceeded; that returned lower bound already fails
/// admission.
fn page_transient_peak_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
    abort_above: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let mut peak = 0usize;
    for admitted in page.chunks() {
        peak = peak.max(projected_chunk_transient_bytes(metadata, admitted)?);
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
    let chunk_bytes = page.chunks().iter().try_fold(0usize, |total, admitted| {
        total
            .checked_add(projected_chunk_prepared_retained_upper_bound_bytes(
                metadata, admitted,
            )?)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact page preparation charge overflowed".to_owned(),
                )
            })
    })?;
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
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let transient = projected_chunk_transient_bytes(metadata, admitted)?;
    let text_bytes = admitted.chunk().sanitized_text.as_str().len();
    let normalized_text_bytes = text_bytes;
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
    let normalized_text_bytes = text_bytes;
    let field_text_bytes = normalized_text_bytes
        .saturating_add(logical_path.len())
        .saturating_add(subtoken_bytes)
        .saturating_add(exact_bytes.saturating_mul(2));
    let field_entries = lexical_token_count(chunk.sanitized_text.as_str())
        .saturating_add(1)
        .saturating_add(chunk.subtokens.len())
        .saturating_add(chunk.exact_terms.len().saturating_mul(2));
    let field_bytes = field_text_bytes
        .saturating_add(field_entries.saturating_mul(std::mem::size_of::<String>()))
        .saturating_add(
            6usize.saturating_mul(std::mem::size_of::<(LexicalFieldV1, Vec<String>)>()),
        );
    let frequency_bytes = field_entries
        .saturating_mul(std::mem::size_of::<(&str, u32)>() + BTREE_MAP_ENTRY_OVERHEAD_BYTES);
    let (_, normalized_scratch) = document_ngram_scratch(normalized_text_bytes)?;
    let (_, raw_scratch) = document_ngram_scratch(text_bytes)?;
    let row_bytes = clone_bytes
        .saturating_add(logical_path.len())
        .saturating_add(normalized_text_bytes)
        .saturating_add(6usize.saturating_mul(std::mem::size_of::<(LexicalFieldV1, usize)>()));
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
        if usize::try_from(page.chunk_count).map_err(contract_number)? != page.documents.len()
            || usize::try_from(page.import_count).map_err(contract_number)? != page.imports.len()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "prepared lexical page cardinality disagrees with its source receipt".to_owned(),
            ));
        }
        for document in &page.documents {
            if u64::try_from(document.document_id).map_err(contract_number)? != expected_document {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "prepared lexical document ids are not contiguous".to_owned(),
                ));
            }
            expected_document = expected_document.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "prepared lexical document count overflowed".to_owned(),
                )
            })?;
        }
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

fn append_prepared_imports(
    transaction: &Transaction<'_>,
    page: &PreparedCodeLexicalArtifactPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for import in &page.imports {
        checkpoint(control)?;
        transaction
            .execute(
                "INSERT INTO import_evidence(canonical, evidence) VALUES (?1, ?1)",
                params![import.canonical.as_slice()],
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO import_integrity(canonical, digest) VALUES (?1, ?2)",
                params![
                    import.canonical.as_slice(),
                    import.integrity_digest.as_str()
                ],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn append_prepared_postings(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    term_insert_plan: &mut PreparedTermInsertPlanV1<'_>,
    exact_insert_plan: &mut PreparedExactInsertPlanV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut term_statement = transaction
        .prepare_cached(
            "INSERT INTO term_postings(field, term, document_id, frequency) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(sqlite_error)?;
    // Plain INSERT, not `INSERT OR IGNORE`: `exact_postings` is
    // `PRIMARY KEY(field, term, document_id) WITHOUT ROWID`. Every prepared
    // document's `exact_postings` is deduplicated into a `BTreeSet<(field,
    // term)>` before this stage (`prepared.rs::prepare_document`), so within
    // one document the pair is unique; `document_id` is then unique across
    // the whole prepared batch because `validate_prepared_page_batch`
    // (this file) rejects any batch whose document ids are not a strictly
    // contiguous continuation of the already-committed cursor, and
    // `prepare_pages_inner` only ever prepares the fresh suffix of pages
    // that cursor has not yet accepted — a resumed or replayed call reuses
    // no document id that is already durable. Together those invariants
    // make every (field, term, document_id) key in a prepared batch globally
    // unique, both within the batch and against every already-committed
    // row, so `OR IGNORE` could only mask a real corruption bug. A plain
    // INSERT lets that surface as a constraint failure instead of vanishing.
    let mut exact_statement = transaction
        .prepare_cached("INSERT INTO exact_postings(field, term, document_id) VALUES (?1, ?2, ?3)")
        .map_err(sqlite_error)?;
    let mut ngram_statement = transaction
        .prepare_cached(
            "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sqlite_error)?;
    let expected_term_rows = term_insert_plan.entries.len();
    let mut inserted_term_rows = 0usize;
    while let Some(entry) = next_term_insert(term_insert_plan)? {
        if inserted_term_rows.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        term_statement
            .execute(params![
                entry.posting.field.as_str(),
                entry.posting.term.as_str(),
                entry.document_id,
                entry.posting.frequency
            ])
            .map_err(sqlite_error)?;
        inserted_term_rows = inserted_term_rows
            .checked_add(1)
            .ok_or_else(batch_ledger_overflow)?;
    }
    if inserted_term_rows != expected_term_rows {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical term merge omitted planned postings".to_owned(),
        ));
    }
    let expected_exact_rows = exact_insert_plan.entries.len();
    let mut inserted_exact_rows = 0usize;
    while let Some(entry) = next_exact_insert(exact_insert_plan)? {
        if inserted_exact_rows.is_multiple_of(EXACT_INSERT_CONTROL_INTERVAL) {
            checkpoint(control)?;
        }
        exact_statement
            .execute(params![entry.field, entry.term, entry.document_id])
            .map_err(sqlite_error)?;
        inserted_exact_rows = inserted_exact_rows
            .checked_add(1)
            .ok_or_else(batch_ledger_overflow)?;
    }
    if inserted_exact_rows != expected_exact_rows {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "lexical exact merge omitted planned postings".to_owned(),
        ));
    }
    for page in pages {
        for shard in &page.ngram_shards {
            checkpoint(control)?;
            ngram_statement
                .execute(params![
                    i64::try_from(page.page_ordinal).map_err(contract_number)?,
                    shard.kind,
                    shard.ngram,
                    shard.documents.as_slice(),
                    i64::try_from(shard.cardinality).map_err(contract_number)?,
                ])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

fn append_prepared_rows(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut row_statement = transaction
        .prepare_cached("INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, ?2, ?3)")
        .map_err(sqlite_error)?;
    let mut integrity_statement = transaction
        .prepare_cached(
            "INSERT INTO document_integrity(document_id, chunk_id, digest) VALUES (?1, ?2, ?3)",
        )
        .map_err(sqlite_error)?;
    for page in pages {
        for document in &page.documents {
            checkpoint(control)?;
            row_statement
                .execute(params![
                    document.document_id,
                    document.chunk_id.as_str(),
                    document.row.as_slice()
                ])
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
            integrity_statement
                .execute(params![
                    document.document_id,
                    document.chunk_id.as_str(),
                    document.integrity_digest.as_str()
                ])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

fn insert_prepared_source_page(
    transaction: &Transaction<'_>,
    page: &PreparedCodeLexicalArtifactPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute(
            "INSERT INTO source_pages(page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt, next_cursor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                i64::try_from(page.page_ordinal).map_err(contract_number)?,
                page.page_digest.as_str(),
                page.cumulative_digest.as_str(),
                i64::try_from(page.chunk_count).map_err(contract_number)?,
                i64::try_from(page.payload_bytes).map_err(contract_number)?,
                i64::try_from(page.import_count).map_err(contract_number)?,
                i64::try_from(page.import_payload_bytes).map_err(contract_number)?,
                page.import_dictionary_digest.as_str(),
                page.ngram_digest.as_str(),
                page.base_sections_receipt.as_slice(),
                page.next_cursor.as_slice(),
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn sqlite_file_size(connection: &Connection) -> Result<u64, CodeLexicalArtifactErrorV1> {
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
    connection
        .execute_batch(
            "
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
                page_digest TEXT NOT NULL,
                cumulative_digest TEXT NOT NULL,
                chunk_count INTEGER NOT NULL,
                payload_bytes INTEGER NOT NULL,
                import_count INTEGER NOT NULL,
                import_payload_bytes INTEGER NOT NULL,
                import_dictionary_digest TEXT NOT NULL,
                ngram_digest TEXT NOT NULL,
                base_sections_receipt BLOB NOT NULL,
                next_cursor BLOB NOT NULL
            );
            -- Every derived document and import receives its digest in the
            -- same private append transaction as its page-level base receipt.
            -- External connections cannot invoke that mutation authority.
            CREATE TABLE document_integrity (
                document_id INTEGER PRIMARY KEY,
                chunk_id TEXT NOT NULL,
                digest TEXT NOT NULL
            );
            CREATE TABLE import_integrity (
                canonical BLOB PRIMARY KEY,
                digest TEXT NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE import_evidence (
                canonical BLOB PRIMARY KEY,
                evidence BLOB NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE rows (
                document_id INTEGER PRIMARY KEY,
                chunk_id TEXT NOT NULL,
                row BLOB NOT NULL
            );
            CREATE TABLE term_postings (
                field TEXT NOT NULL,
                term TEXT NOT NULL,
                document_id INTEGER NOT NULL,
                frequency INTEGER NOT NULL,
                PRIMARY KEY(field, term, document_id)
            ) WITHOUT ROWID;
            CREATE TABLE term_stats (
                field TEXT NOT NULL,
                term TEXT NOT NULL,
                document_frequency INTEGER NOT NULL,
                PRIMARY KEY(field, term)
            ) WITHOUT ROWID;
            CREATE TABLE field_stats (
                field TEXT PRIMARY KEY,
                total_length INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE exact_postings (
                field TEXT NOT NULL,
                term BLOB NOT NULL,
                document_id INTEGER NOT NULL,
                PRIMARY KEY(field, term, document_id)
            ) WITHOUT ROWID;
            CREATE TABLE ngram_postings (
                page_ordinal INTEGER NOT NULL,
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                documents BLOB NOT NULL,
                cardinality INTEGER NOT NULL CHECK(cardinality > 0),
                PRIMARY KEY(page_ordinal, kind, ngram)
            ) WITHOUT ROWID;
            CREATE TABLE ngram_statistics (
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                document_frequency INTEGER NOT NULL CHECK(document_frequency > 0),
                PRIMARY KEY(kind, ngram)
            ) WITHOUT ROWID;
            CREATE TABLE vocabulary (term TEXT PRIMARY KEY) WITHOUT ROWID;
            CREATE TRIGGER content_epoch_source_pages_insert AFTER INSERT ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_document_integrity_insert AFTER INSERT ON document_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_integrity_insert AFTER INSERT ON import_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_evidence_insert AFTER INSERT ON import_evidence BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER immutable_source_pages_update BEFORE UPDATE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_source_pages_delete BEFORE DELETE ON source_pages BEGIN SELECT RAISE(ABORT, 'immutable lexical source pages'); END;
            CREATE TRIGGER immutable_document_integrity_update BEFORE UPDATE ON document_integrity BEGIN SELECT RAISE(ABORT, 'immutable lexical document integrity'); END;
            CREATE TRIGGER immutable_document_integrity_delete BEFORE DELETE ON document_integrity BEGIN SELECT RAISE(ABORT, 'immutable lexical document integrity'); END;
            CREATE TRIGGER immutable_import_integrity_update BEFORE UPDATE ON import_integrity BEGIN SELECT RAISE(ABORT, 'immutable lexical import integrity'); END;
            CREATE TRIGGER immutable_import_integrity_delete BEFORE DELETE ON import_integrity BEGIN SELECT RAISE(ABORT, 'immutable lexical import integrity'); END;
            CREATE TRIGGER immutable_import_evidence_update BEFORE UPDATE ON import_evidence BEGIN SELECT RAISE(ABORT, 'immutable lexical import evidence'); END;
            CREATE TRIGGER immutable_import_evidence_delete BEFORE DELETE ON import_evidence BEGIN SELECT RAISE(ABORT, 'immutable lexical import evidence'); END;
            CREATE TRIGGER immutable_ngram_postings_update BEFORE UPDATE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER immutable_ngram_postings_delete BEFORE DELETE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'immutable lexical ngram postings'); END;
            CREATE TRIGGER builder_gate_source_pages_insert BEFORE INSERT ON source_pages WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_document_integrity_insert BEFORE INSERT ON document_integrity WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_import_integrity_insert BEFORE INSERT ON import_integrity WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_import_evidence_insert BEFORE INSERT ON import_evidence WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_rows_insert BEFORE INSERT ON rows WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_rows_update BEFORE UPDATE ON rows WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_rows_delete BEFORE DELETE ON rows WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_insert BEFORE INSERT ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_update BEFORE UPDATE ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_term_postings_delete BEFORE DELETE ON term_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_insert BEFORE INSERT ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_update BEFORE UPDATE ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_exact_postings_delete BEFORE DELETE ON exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            CREATE TRIGGER builder_gate_ngram_postings_insert BEFORE INSERT ON ngram_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
            ",
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
    for (name, table, operation, message) in IMMUTABLE_TRIGGER_LAYOUT {
        let expected = format!(
            "CREATE TRIGGER {name} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, '{message}'); END"
        );
        verify_trigger_schema(connection, name, table, &expected)?;
    }
    Ok(())
}

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
            CREATE TRIGGER frozen_document_integrity_insert BEFORE INSERT ON document_integrity BEGIN SELECT RAISE(ABORT, 'frozen lexical document integrity'); END;
            CREATE TRIGGER frozen_import_integrity_insert BEFORE INSERT ON import_integrity BEGIN SELECT RAISE(ABORT, 'frozen lexical import integrity'); END;
            CREATE TRIGGER frozen_import_evidence_insert BEFORE INSERT ON import_evidence BEGIN SELECT RAISE(ABORT, 'frozen lexical import evidence'); END;
            CREATE TRIGGER frozen_rows_insert BEFORE INSERT ON rows BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_rows_update BEFORE UPDATE ON rows BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_rows_delete BEFORE DELETE ON rows BEGIN SELECT RAISE(ABORT, 'frozen lexical rows'); END;
            CREATE TRIGGER frozen_term_postings_insert BEFORE INSERT ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;
            CREATE TRIGGER frozen_term_postings_update BEFORE UPDATE ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;
            CREATE TRIGGER frozen_term_postings_delete BEFORE DELETE ON term_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical term postings'); END;
            CREATE TRIGGER frozen_exact_postings_insert BEFORE INSERT ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;
            CREATE TRIGGER frozen_exact_postings_update BEFORE UPDATE ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;
            CREATE TRIGGER frozen_exact_postings_delete BEFORE DELETE ON exact_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical exact postings'); END;
            CREATE TRIGGER frozen_ngram_postings_insert BEFORE INSERT ON ngram_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram postings'); END;
            CREATE TRIGGER frozen_ngram_postings_update BEFORE UPDATE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram postings'); END;
            CREATE TRIGGER frozen_ngram_postings_delete BEFORE DELETE ON ngram_postings BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram postings'); END;
            ",
        )
        .map_err(sqlite_error)
}

fn authenticated_authority_epoch(
    transaction: &Transaction<'_>,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<i64, CodeLexicalArtifactErrorV1> {
    verify_builder_mutation_gate_schema(transaction)?;
    let (pages, documents, import_integrity, import_evidence): (i64, i64, i64, i64) = transaction
        .query_row(
            "SELECT (SELECT COUNT(*) FROM source_pages), (SELECT COUNT(*) FROM document_integrity), (SELECT COUNT(*) FROM import_integrity), (SELECT COUNT(*) FROM import_evidence)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sqlite_error)?;
    let expected_epoch = pages
        .checked_add(documents)
        .and_then(|count| count.checked_add(import_integrity))
        .and_then(|count| count.checked_add(import_evidence))
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact authority row count overflowed".to_owned(),
            )
        })?;
    let actual_epoch = content_epoch(transaction)?;
    if actual_epoch != expected_epoch
        || u64::try_from(pages).ok() != Some(source.page_count())
        || u64::try_from(documents).ok() != Some(source.total_chunks())
        || u64::try_from(import_integrity).ok() != Some(source.total_imports())
        || import_integrity != import_evidence
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact authenticated authority disagrees with its source receipt".to_owned(),
        ));
    }
    Ok(actual_epoch)
}

fn advance_pre_digest_work(
    transaction: &Transaction<'_>,
    state: &mut PersistedFinalizationStateV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    match state.phase {
        PersistedFinalizationPhaseV1::Statistics => {
            with_cancellable_sqlite_statement(transaction, control, || {
                derive_statistics_step(transaction, state.section_ordinal)?;
                Ok(())
            })?;
            state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact statistics step overflowed".to_owned(),
                )
            })?;
            if state.section_ordinal == 3 {
                state.phase = PersistedFinalizationPhaseV1::Indexes;
                state.section_ordinal = 0;
            }
        }
        PersistedFinalizationPhaseV1::Indexes => {
            with_cancellable_sqlite_statement(transaction, control, || {
                build_serving_index_step(transaction, state.section_ordinal)?;
                Ok(())
            })?;
            state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact serving-index step overflowed".to_owned(),
                )
            })?;
            if state.section_ordinal == 8 {
                verify_required_artifact_indexes(transaction)?;
                state.phase = PersistedFinalizationPhaseV1::Digest;
                state.section_ordinal = 0;
                state.section_accumulator = initial_section_accumulator(SECTION_NAMES[0])?.to_vec();
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

fn with_cancellable_sqlite_statement<T>(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
    operation: impl FnOnce() -> Result<T, CodeLexicalArtifactErrorV1>,
) -> Result<T, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let interruption = Arc::new(AtomicU8::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let progress_interruption = Arc::clone(&interruption);
    transaction
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
    let clear = transaction
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

fn derive_statistics_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    match ordinal {
        0 => hotpath::measure_block!("query.artifact.finalization.derive_field_stats", {
            transaction.execute_batch(
                "INSERT INTO field_stats(field, total_length) SELECT field, SUM(frequency) FROM term_postings GROUP BY field;
                 CREATE TRIGGER frozen_field_stats_insert BEFORE INSERT ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;
                 CREATE TRIGGER frozen_field_stats_update BEFORE UPDATE ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;
                 CREATE TRIGGER frozen_field_stats_delete BEFORE DELETE ON field_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical field statistics'); END;",
            )
        }),
        1 => hotpath::measure_block!("query.artifact.finalization.derive_term_stats", {
            transaction.execute_batch(
                "INSERT INTO term_stats(field, term, document_frequency) SELECT field, term, COUNT(*) FROM term_postings GROUP BY field, term;
                 CREATE TRIGGER frozen_term_stats_insert BEFORE INSERT ON term_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical term statistics'); END;
                 CREATE TRIGGER frozen_term_stats_update BEFORE UPDATE ON term_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical term statistics'); END;
                 CREATE TRIGGER frozen_term_stats_delete BEFORE DELETE ON term_stats BEGIN SELECT RAISE(ABORT, 'frozen lexical term statistics'); END;",
            )
        }),
        2 => hotpath::measure_block!("query.artifact.finalization.derive_vocabulary", {
            let subtoken = encode_field(LexicalFieldV1::Subtoken)?;
            transaction
                .execute(
                    "INSERT INTO vocabulary(term) SELECT DISTINCT term FROM term_postings WHERE field != ?1",
                    [subtoken],
                )
                .and_then(|_| {
                    transaction.execute_batch(
                        "CREATE TRIGGER frozen_vocabulary_insert BEFORE INSERT ON vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical vocabulary'); END;
                         CREATE TRIGGER frozen_vocabulary_update BEFORE UPDATE ON vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical vocabulary'); END;
                         CREATE TRIGGER frozen_vocabulary_delete BEFORE DELETE ON vocabulary BEGIN SELECT RAISE(ABORT, 'frozen lexical vocabulary'); END;",
                    )
                })
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

fn build_serving_index_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    match ordinal {
        0 => hotpath::measure_block!(
            "query.artifact.finalization.index.rows_by_chunk",
            transaction.execute_batch("CREATE UNIQUE INDEX rows_by_chunk ON rows(chunk_id)")
        ),
        1 => hotpath::measure_block!(
            "query.artifact.finalization.index.term_postings_by_term",
            transaction.execute_batch(
                "CREATE INDEX term_postings_by_term ON term_postings(term, field, document_id)",
            )
        ),
        2 => hotpath::measure_block!(
            "query.artifact.finalization.index.term_postings_by_document",
            transaction.execute_batch(
                "CREATE INDEX term_postings_by_document ON term_postings(document_id, field, term, frequency)",
            )
        ),
        3 => hotpath::measure_block!(
            "query.artifact.finalization.index.term_postings_by_document_term",
            transaction.execute_batch(
                "CREATE INDEX term_postings_by_document_term ON term_postings(document_id, term, field, frequency)",
            )
        ),
        4 => hotpath::measure_block!(
            "query.artifact.finalization.index.term_stats_by_term",
            transaction.execute_batch(
                "CREATE INDEX term_stats_by_term ON term_stats(term, field)",
            )
        ),
        5 => hotpath::measure_block!(
            "query.artifact.finalization.index.exact_postings_by_document",
            transaction.execute_batch(
                "CREATE INDEX exact_postings_by_document ON exact_postings(document_id, field, term)",
            )
        ),
        // `cardinality` rides in the index purely so the statistics
        // aggregation below is covered. Without it the index carries only the
        // reordered WITHOUT ROWID key columns, and every `SUM(cardinality)`
        // fetch through it is one random main-tree lookup per posting row —
        // an N+1 access pattern over a tree dominated by `documents` blobs
        // that collapses once the corpus outgrows the bounded page cache
        // (measured on a 12M-row/2.9M-group synthetic at production pragmas:
        // 121 s warm and 537 s cold non-covering, ~240 s as a sort-backed
        // table scan, under 4 s covered in both regimes). Uniqueness of the
        // (kind, ngram, page_ordinal) prefix is already guaranteed by the
        // table primary key, so the wider UNIQUE declaration loses nothing.
        6 => hotpath::measure_block!(
            "query.artifact.finalization.index.ngram_postings_by_ngram",
            transaction.execute_batch(
                "CREATE UNIQUE INDEX ngram_postings_by_ngram ON ngram_postings(kind, ngram, page_ordinal, cardinality)",
            )
        ),
        7 => hotpath::measure_block!(
            "query.artifact.finalization.ngram_statistics",
            transaction.execute_batch(
                "INSERT INTO ngram_statistics(kind, ngram, document_frequency)
                 SELECT kind, ngram, SUM(cardinality)
                 FROM ngram_postings INDEXED BY ngram_postings_by_ngram
                 GROUP BY kind, ngram;
                 CREATE TRIGGER frozen_ngram_statistics_insert BEFORE INSERT ON ngram_statistics BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram statistics'); END;
                 CREATE TRIGGER frozen_ngram_statistics_update BEFORE UPDATE ON ngram_statistics BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram statistics'); END;
                 CREATE TRIGGER frozen_ngram_statistics_delete BEFORE DELETE ON ngram_statistics BEGIN SELECT RAISE(ABORT, 'frozen lexical ngram statistics'); END;",
            )
        ),
        _ => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact selected an unknown serving-index step".to_owned(),
            ));
        }
    }
    .map_err(sqlite_error)
}

impl PersistedFinalizationStateV1 {
    fn new(
        content_epoch: i64,
        source: &VerifiedSealedLexicalSourceReceiptV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        if content_epoch < 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact mutation epoch is negative".to_owned(),
            ));
        }
        let (base_section_row_counts, base_section_accumulators) =
            initial_base_section_receipt_fold()?;
        Ok(Self {
            phase: PersistedFinalizationPhaseV1::Statistics,
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
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if format_revision != CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1 {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "format revision {format_revision} is not supported"
        )));
    }
    checkpoint(control)?;
    let stored_metadata: CodeLexicalProjectionMetadataV1 = serde_json::from_slice(&metadata_bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let actual_digest = metadata_digest(&stored_metadata)?;
    if stored_digest != actual_digest.as_str() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact metadata digest does not verify".to_owned(),
        ));
    }
    if &stored_metadata != expected_metadata || &actual_digest != expected_digest {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(
            "staging metadata does not match the requested generation".to_owned(),
        ));
    }
    checkpoint(control)?;
    Ok(())
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

fn load_finalization_state(
    connection: &Connection,
) -> Result<Option<PersistedFinalizationStateV1>, CodeLexicalArtifactErrorV1> {
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
    let section_count = u64::try_from(SECTION_NAMES.len()).map_err(contract_number)?;
    let completed_section_count = if state.phase == PersistedFinalizationPhaseV1::Digest {
        usize::try_from(state.section_ordinal).map_err(contract_number)?
    } else {
        0
    };
    let maximum_ordinal = match state.phase {
        PersistedFinalizationPhaseV1::Statistics => 3,
        PersistedFinalizationPhaseV1::Indexes => 8,
        PersistedFinalizationPhaseV1::Digest => section_count,
    };
    if state.section_ordinal > maximum_ordinal
        || (state.phase == PersistedFinalizationPhaseV1::Digest
            && state.section_ordinal > 0
            && state.section_ordinal
                < u64::try_from(1 + BASE_SECTION_NAMES.len()).map_err(contract_number)?)
        || state.completed_sections.len() != completed_section_count
        || state.completed_sections.len() > SECTION_NAMES.len()
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
        .zip(SECTION_NAMES)
        .any(|(section, expected_name)| section.name != expected_name)
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
    match (section, last_key.as_ref()) {
        (FinalizationSectionV1::SourcePages, None)
        | (FinalizationSectionV1::DocumentIntegrity, None)
        | (FinalizationSectionV1::Rows, None)
        | (FinalizationSectionV1::ImportIntegrity, None)
        | (FinalizationSectionV1::ImportEvidence, None)
        | (FinalizationSectionV1::TermPostings, None)
        | (FinalizationSectionV1::ExactPostings, None)
        | (FinalizationSectionV1::NgramPostings, None)
        | (FinalizationSectionV1::FieldStatistics, None)
        | (FinalizationSectionV1::TermStatistics, None)
        | (FinalizationSectionV1::Vocabulary, None) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(false),
            params![limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::SourcePages
            | FinalizationSectionV1::DocumentIntegrity
            | FinalizationSectionV1::Rows,
            Some(PersistedFinalizationKeyV1::Integer(value)),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![value, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::ImportIntegrity | FinalizationSectionV1::ImportEvidence,
            Some(PersistedFinalizationKeyV1::Blob(value)),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![value, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::TermPostings,
            Some(PersistedFinalizationKeyV1::TextTextInteger {
                field,
                term,
                document_id,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![field, term, document_id, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::ExactPostings,
            Some(PersistedFinalizationKeyV1::TextBlobInteger {
                field,
                term,
                document_id,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![field, term, document_id, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::NgramPostings,
            Some(PersistedFinalizationKeyV1::IntegerIntegerInteger {
                page_ordinal,
                kind,
                ngram,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![page_ordinal, kind, ngram, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::FieldStatistics | FinalizationSectionV1::Vocabulary,
            Some(PersistedFinalizationKeyV1::Text(value)),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![value, limit],
            state,
            control,
        ),
        (
            FinalizationSectionV1::TermStatistics,
            Some(PersistedFinalizationKeyV1::TextText { field, term }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![field, term, limit],
            state,
            control,
        ),
        _ => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact finalization key does not match its section".to_owned(),
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
            let receipt: Vec<u8> = row.get(9).map_err(sqlite_error)?;
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
    match section {
        FinalizationSectionV1::SourcePages
        | FinalizationSectionV1::DocumentIntegrity
        | FinalizationSectionV1::Rows => Ok(PersistedFinalizationKeyV1::Integer(
            row.get(0).map_err(sqlite_error)?,
        )),
        FinalizationSectionV1::ImportIntegrity | FinalizationSectionV1::ImportEvidence => Ok(
            PersistedFinalizationKeyV1::Blob(row.get(0).map_err(sqlite_error)?),
        ),
        FinalizationSectionV1::TermPostings => Ok(PersistedFinalizationKeyV1::TextTextInteger {
            field: row.get(0).map_err(sqlite_error)?,
            term: row.get(1).map_err(sqlite_error)?,
            document_id: row.get(2).map_err(sqlite_error)?,
        }),
        FinalizationSectionV1::ExactPostings => Ok(PersistedFinalizationKeyV1::TextBlobInteger {
            field: row.get(0).map_err(sqlite_error)?,
            term: row.get(1).map_err(sqlite_error)?,
            document_id: row.get(2).map_err(sqlite_error)?,
        }),
        FinalizationSectionV1::NgramPostings => {
            Ok(PersistedFinalizationKeyV1::IntegerIntegerInteger {
                page_ordinal: row.get(0).map_err(sqlite_error)?,
                kind: row.get(1).map_err(sqlite_error)?,
                ngram: row.get(2).map_err(sqlite_error)?,
            })
        }
        FinalizationSectionV1::FieldStatistics | FinalizationSectionV1::Vocabulary => Ok(
            PersistedFinalizationKeyV1::Text(row.get(0).map_err(sqlite_error)?),
        ),
        FinalizationSectionV1::TermStatistics => Ok(PersistedFinalizationKeyV1::TextText {
            field: row.get(0).map_err(sqlite_error)?,
            term: row.get(1).map_err(sqlite_error)?,
        }),
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
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut statement = connection
        .prepare(
            "SELECT page_ordinal, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages ORDER BY page_ordinal",
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
        let cumulative_digest: String = row.get(1).map_err(sqlite_error)?;
        let page_chunks =
            u64::try_from(row.get::<_, i64>(2).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_payload =
            u64::try_from(row.get::<_, i64>(3).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_imports =
            u64::try_from(row.get::<_, i64>(4).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_import_payload =
            u64::try_from(row.get::<_, i64>(5).map_err(sqlite_error)?).map_err(contract_number)?;
        let import_digest: String = row.get(6).map_err(sqlite_error)?;
        let cursor_bytes: Vec<u8> = row.get(7).map_err(sqlite_error)?;
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
    Ok(())
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
    let expected = [
        ("source_pages", source.page_count()),
        ("document_integrity", source.total_chunks()),
        ("import_integrity", source.total_imports()),
        ("import_evidence", source.total_imports()),
        ("rows", source.total_chunks()),
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
    let tail: Option<(i64, String, String, Vec<u8>)> = connection
        .query_row(PROGRESS_TAIL_QUERY, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()
        .map_err(sqlite_error)?;
    let Some((page_ordinal, import_digest, cumulative_digest, cursor_bytes)) = tail else {
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
    let next_page_ordinal = u64::try_from(page_ordinal)
        .map_err(contract_number)?
        .checked_add(1)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "persisted lexical artifact page ordinal overflowed".to_owned(),
            )
        })?;
    if cursor.next_page_ordinal() != next_page_ordinal
        || cursor.import_dictionary_digest().as_str() != import_digest
        || cursor.cumulative_digest().as_str() != cumulative_digest
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact progress disagrees with its exact source cursor".to_owned(),
        ));
    }
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
            "SELECT next_cursor FROM source_pages WHERE page_ordinal = ?1",
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
            "SELECT page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages WHERE page_ordinal = ?1",
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
        FinalizationSectionV1::TermStatistics,
        FinalizationSectionV1::Vocabulary,
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
        .prepare(section.full_query())
        .map_err(sqlite_error)?;
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
        let receipt: Vec<u8> = row.get(9).map_err(sqlite_error)?;
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
        .prepare(section.full_query())
        .map_err(sqlite_error)?;
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

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(bytes);
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
    if receipt.source_state_digest() != source.source_state_digest()
        || receipt.source_cumulative_digest() != source.cumulative_digest()
        || receipt.page_count() != source.page_count()
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

#[cfg(feature = "hotpath")]
fn record_finalization_step(step: &CodeLexicalArtifactFinalizationStepV1) {
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
fn record_finalization_step(step: &CodeLexicalArtifactFinalizationStepV1) {
    let _ = step;
}

#[cfg(feature = "hotpath")]
fn record_batch_outcome(
    result: &Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1>,
) {
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
fn record_batch_outcome(
    result: &Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1>,
) {
    let _ = result;
}

#[cfg(feature = "hotpath")]
fn record_prepared_batch_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let documents = pages.iter().map(|page| page.documents.len()).sum::<usize>();
    let source_bytes = pages
        .iter()
        .map(PreparedCodeLexicalArtifactPageV1::source_retained_bytes)
        .sum::<usize>();
    let prepared_bytes = pages
        .iter()
        .map(PreparedCodeLexicalArtifactPageV1::retained_owned_bytes)
        .sum::<usize>();
    let effective_workers = tracedecay_code_index::parallelism::indexing_workers().min(pages.len());
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
    hotpath::gauge!("query.artifact.batch.active_scratch_bytes_total").inc(active_scratch as u64);
    hotpath::gauge!("query.artifact.batch.effective_workers").set(effective_workers as u64);
}

#[cfg(not(feature = "hotpath"))]
fn record_prepared_batch_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let _ = pages;
}

#[cfg(feature = "hotpath")]
fn record_batch_import_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let imports = pages.iter().map(|page| page.imports.len()).sum::<usize>();
    hotpath::gauge!("query.artifact.batch.import_rows_total").inc(imports as u64);
}

#[cfg(not(feature = "hotpath"))]
fn record_batch_import_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let _ = pages;
}

#[cfg(feature = "hotpath")]
fn record_batch_posting_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
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
fn record_batch_posting_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let _ = pages;
}

#[cfg(feature = "hotpath")]
fn record_batch_row_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let rows = pages.iter().map(|page| page.documents.len()).sum::<usize>();
    hotpath::gauge!("query.artifact.batch.document_rows_total").inc(rows as u64);
}

#[cfg(not(feature = "hotpath"))]
fn record_batch_row_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let _ = pages;
}

#[cfg(feature = "hotpath")]
fn record_batch_receipt_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    hotpath::gauge!("query.artifact.batch.receipt_rows_total").inc(pages.len() as u64);
}

#[cfg(not(feature = "hotpath"))]
fn record_batch_receipt_metrics(pages: &[PreparedCodeLexicalArtifactPageV1]) {
    let _ = pages;
}

#[cfg(feature = "hotpath")]
fn record_batch_prefix_limit(limit: CodeLexicalArtifactBatchLimitV1) {
    match limit {
        CodeLexicalArtifactBatchLimitV1::Memory => {
            hotpath::gauge!("query.artifact.batch.prefix_limited.memory_total").inc(1u64);
        }
        CodeLexicalArtifactBatchLimitV1::PreparedRows => {
            hotpath::gauge!("query.artifact.batch.prefix_limited.prepared_rows_total").inc(1u64);
        }
        CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes => {
            hotpath::gauge!("query.artifact.batch.prefix_limited.estimated_write_bytes_total")
                .inc(1u64);
        }
    }
}

#[cfg(not(feature = "hotpath"))]
fn record_batch_prefix_limit(limit: CodeLexicalArtifactBatchLimitV1) {
    let _ = limit;
}

#[cfg(feature = "hotpath")]
fn record_artifact_progress(progress: &CodeLexicalArtifactBuildProgressV1) {
    hotpath::gauge!("query.artifact.pages").set(progress.next_page_ordinal);
    hotpath::gauge!("query.artifact.rows").set(progress.completed_chunks);
    hotpath::gauge!("query.artifact.bytes").set(progress.completed_payload_bytes);
}

#[cfg(not(feature = "hotpath"))]
fn record_artifact_progress(progress: &CodeLexicalArtifactBuildProgressV1) {
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
    let progress = progress(connection)?;
    source
        .verify_completion(progress.next_cursor.as_ref())
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    if receipt.metadata_digest() != expected_metadata_digest {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact metadata digest changed".to_owned(),
        ));
    }
    let sections = compute_section_digests(connection, control)?;
    if sections != receipt.section_digests() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact section digests do not verify".to_owned(),
        ));
    }
    let digest = artifact_digest(
        receipt.metadata_digest(),
        receipt.source_state_digest(),
        receipt.source_format_revision(),
        receipt.page_count(),
        receipt.total_chunks(),
        receipt.total_payload_bytes(),
        receipt.total_imports(),
        receipt.import_payload_bytes(),
        receipt.import_dictionary_digest(),
        receipt.source_cumulative_digest(),
        &sections,
    )?;
    if &digest != receipt.artifact_digest() {
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

fn contract_number(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::format::encode_ngram_bitmap;
    use super::*;
    use roaring::RoaringBitmap;
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
        }
    }

    fn create_mutable_test_schema(connection: &Connection) -> BuilderMutationGuardV1 {
        let gate =
            register_builder_mutation_gate(connection).expect("register builder mutation gate");
        create_schema(connection).expect("create artifact schema");
        BuilderMutationGuardV1::enter(&gate).expect("enter test builder mutation authority")
    }

    #[test]
    fn canonical_batch_limits_select_a_multi_page_prefix_without_stalling() {
        let page_bounds = [
            (700_000usize, 80 * 1024 * 1024usize),
            (700_000, 80 * 1024 * 1024),
            (700_000, 80 * 1024 * 1024),
        ];
        let mut ledger = CanonicalBatchLimitLedgerV1::default();
        let selected = page_bounds
            .into_iter()
            .take_while(|(rows, bytes)| {
                ledger
                    .try_admit(*rows, *bytes)
                    .expect("extend canonical limit ledger")
                    .is_none()
            })
            .count();
        assert_eq!(
            selected, 2,
            "two pages fit both canonical caps and the third must remain for the next wake"
        );
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
                    AuthAction::Read { table_name, .. }
                        if BASE_SECTION_NAMES.contains(&table_name) =>
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
    fn ngram_staging_key_preserves_source_page_order_without_a_serving_index() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        for (page_ordinal, ngram, document) in
            [(0i64, 90i64, 0u32), (0, 100, 0), (1, 10, 1), (1, 20, 1)]
        {
            let bitmap = RoaringBitmap::from_iter([document]);
            let encoded = encode_ngram_bitmap(&bitmap).expect("encode ngram bitmap");
            connection
                .execute(
                    "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (?1, 1, ?2, ?3, 1)",
                    params![page_ordinal, ngram, encoded],
                )
                .expect("seed page-ordered ngram posting");
        }

        let serving_indexes: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('ngram_postings') WHERE name = 'ngram_postings_by_ngram'",
                [],
                |row| row.get(0),
            )
            .expect("inspect staging indexes");
        assert_eq!(
            serving_indexes, 0,
            "serving-key maintenance must remain absent during catch-up"
        );

        let mut statement = connection
            .prepare(
                "SELECT page_ordinal, kind, ngram FROM ngram_postings ORDER BY page_ordinal, kind, ngram",
            )
            .expect("prepare staging-order scan");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .expect("scan staging order")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect staging order");
        assert_eq!(rows, [(0, 1, 90), (0, 1, 100), (1, 1, 10), (1, 1, 20)]);
        assert_eq!(
            statement.get_status(StatementStatus::Sort),
            0,
            "source-page catch-up must be the maintained table order"
        );
    }

    #[test]
    fn deferred_ngram_serving_index_is_unique_and_query_selective() {
        let mut connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        for (page_ordinal, ngram, documents) in [
            (0i64, 10i64, vec![1u32, 2]),
            (0, 20, vec![1]),
            (1, 10, vec![3]),
        ] {
            let bitmap = RoaringBitmap::from_iter(documents);
            let encoded = encode_ngram_bitmap(&bitmap).expect("encode ngram bitmap");
            connection
                .execute(
                    "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (?1, 1, ?2, ?3, ?4)",
                    params![page_ordinal, ngram, encoded, bitmap.len() as i64],
                )
                .expect("seed ngram posting");
        }

        let transaction = connection
            .transaction()
            .expect("start serving-index transaction");
        build_serving_index_step(&transaction, 6).expect("build ngram serving index");
        build_serving_index_step(&transaction, 7).expect("build ngram serving statistics");
        transaction.commit().expect("commit ngram serving index");

        let unique: i64 = connection
            .query_row(
                "SELECT [unique] FROM pragma_index_list('ngram_postings') WHERE name = 'ngram_postings_by_ngram'",
                [],
                |row| row.get(0),
            )
            .expect("inspect ngram serving index");
        assert_eq!(
            unique, 1,
            "the serving index must preserve posting identity"
        );
        let columns = connection
            .prepare(
                "SELECT name FROM pragma_index_xinfo('ngram_postings_by_ngram') WHERE key = 1 ORDER BY seqno",
            )
            .expect("prepare serving-index columns")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query serving-index columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect serving-index columns");
        assert_eq!(columns, ["kind", "ngram", "page_ordinal", "cardinality"]);

        let query = "SELECT documents, cardinality FROM ngram_postings \
                     WHERE kind = ?1 AND ngram = ?2 ORDER BY page_ordinal";
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .expect("prepare ngram serving plan")
            .query_map(params![1i64, 10i64], |row| row.get::<_, String>(3))
            .expect("query ngram serving plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ngram serving plan");
        assert!(
            plan.iter()
                .any(|detail| detail.contains("USING INDEX ngram_postings_by_ngram")),
            "phrase candidates must use the deferred serving index, got {plan:?}"
        );
        let shard_cardinalities = connection
            .prepare(query)
            .expect("prepare ngram serving query")
            .query_map(params![1i64, 10i64], |row| row.get::<_, i64>(1))
            .expect("query ngram candidates")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ngram candidates");
        assert_eq!(shard_cardinalities, [2, 1]);
        let document_frequency: i64 = connection
            .query_row(
                "SELECT document_frequency FROM ngram_statistics WHERE kind = 1 AND ngram = 10",
                [],
                |row| row.get(0),
            )
            .expect("query finalized ngram statistics");
        assert_eq!(document_frequency, 3);
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
                "CREATE TABLE source_pages (
                    page_ordinal INTEGER PRIMARY KEY,
                    import_dictionary_digest TEXT NOT NULL,
                    cumulative_digest TEXT NOT NULL,
                    next_cursor BLOB NOT NULL
                );",
            )
            .expect("create source progress table");
        for page in 0..4_096i64 {
            connection
                .execute(
                    "INSERT INTO source_pages(page_ordinal, import_dictionary_digest, cumulative_digest, next_cursor) VALUES (?1, 'imports', 'cumulative', X'00')",
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

    #[test]
    fn one_document_integrity_row_does_not_visit_unrelated_relational_rows() {
        let mut connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        let transaction = connection.transaction().expect("start seed transaction");
        for document_id in 0..2_048i64 {
            transaction
                .execute(
                    "INSERT INTO term_postings(field, term, document_id, frequency) VALUES ('body', ?1, ?2, 1)",
                    params![format!("term-{document_id:04}"), document_id],
                )
                .expect("seed term posting");
            transaction
                .execute(
                    "INSERT INTO exact_postings(field, term, document_id) VALUES ('symbol', ?1, ?2)",
                    params![document_id.to_le_bytes().as_slice(), document_id],
                )
                .expect("seed exact posting");
        }
        transaction.commit().expect("commit seed transaction");
        let transaction = connection
            .transaction()
            .expect("start index-build transaction");
        for ordinal in [2, 5] {
            build_serving_index_step(&transaction, ordinal)
                .expect("build document integrity index");
        }
        transaction.commit().expect("commit integrity index");

        for query in [
            "SELECT field, term, frequency FROM term_postings INDEXED BY term_postings_by_document WHERE document_id = ?1 ORDER BY field, term",
            "SELECT field, term FROM exact_postings WHERE document_id = ?1 ORDER BY field, term",
        ] {
            let mut statement = connection.prepare(query).expect("prepare integrity query");
            {
                let mut rows = statement.query([1_024i64]).expect("query one document");
                assert!(rows.next().expect("read matching row").is_some());
                assert!(rows.next().expect("finish matching rows").is_none());
            }
            assert_eq!(
                statement.get_status(StatementStatus::FullscanStep),
                0,
                "one document must not visit unrelated generation rows"
            );
            assert_eq!(
                statement.get_status(StatementStatus::Sort),
                0,
                "one document must not sort unrelated generation rows"
            );
        }
    }

    #[test]
    fn bounded_finalization_resume_seeks_each_native_section_index() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        let _mutation_authority = create_mutable_test_schema(&connection);
        connection
            .execute(
                "INSERT INTO source_pages(page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, ngram_digest, base_sections_receipt, next_cursor) VALUES (0, 'page', 'cumulative', 1, 1, 1, 1, 'imports', 'ngrams', X'00', X'00')",
                [],
            )
            .expect("seed source page");
        connection
            .execute(
                "INSERT INTO document_integrity(document_id, chunk_id, digest) VALUES (0, 'chunk', 'document')",
                [],
            )
            .expect("seed document integrity");
        connection
            .execute(
                "INSERT INTO import_integrity(canonical, digest) VALUES (X'01', 'import')",
                [],
            )
            .expect("seed import integrity");
        connection
            .execute(
                "INSERT INTO import_evidence(canonical, evidence) VALUES (X'01', X'01')",
                [],
            )
            .expect("seed import evidence");
        connection
            .execute(
                "INSERT INTO rows(document_id, chunk_id, row) VALUES (0, 'chunk', X'00')",
                [],
            )
            .expect("seed row");
        connection
            .execute(
                "INSERT INTO term_postings(field, term, document_id, frequency) VALUES ('field', 'term', 0, 1)",
                [],
            )
            .expect("seed term posting");
        connection
            .execute(
                "INSERT INTO exact_postings(field, term, document_id) VALUES ('field', X'01', 0)",
                [],
            )
            .expect("seed exact posting");
        let encoded =
            encode_ngram_bitmap(&RoaringBitmap::from_iter([0])).expect("encode ngram bitmap");
        connection
            .execute(
                "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (0, 1, 1, ?1, 1)",
                [encoded],
            )
            .expect("seed ngram posting");
        connection
            .execute(
                "INSERT INTO field_stats(field, total_length) VALUES ('field', 1)",
                [],
            )
            .expect("seed field statistic");
        connection
            .execute(
                "INSERT INTO term_stats(field, term, document_frequency) VALUES ('field', 'term', 1)",
                [],
            )
            .expect("seed term statistic");
        connection
            .execute("INSERT INTO vocabulary(term) VALUES ('term')", [])
            .expect("seed vocabulary");
        let transaction = connection
            .unchecked_transaction()
            .expect("start ngram serving-index transaction");
        build_serving_index_step(&transaction, 6)
            .expect("build ngram serving index before digest verification");
        transaction.commit().expect("commit ngram serving index");

        for section in FinalizationSectionV1::ALL {
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
        let query = format!("EXPLAIN QUERY PLAN {}", section.seek_query(true));
        let mut statement = connection.prepare(&query).map_err(sqlite_error)?;
        let mut rows = match section {
            FinalizationSectionV1::SourcePages
            | FinalizationSectionV1::DocumentIntegrity
            | FinalizationSectionV1::Rows => statement.query(params![0i64, 1i64]),
            FinalizationSectionV1::ImportIntegrity | FinalizationSectionV1::ImportEvidence => {
                statement.query(params![vec![0u8], 1i64])
            }
            FinalizationSectionV1::TermPostings => {
                statement.query(params!["field", "term", 0i64, 1i64])
            }
            FinalizationSectionV1::TermStatistics => {
                statement.query(params!["field", "term", 1i64])
            }
            FinalizationSectionV1::ExactPostings => {
                statement.query(params!["field", vec![0u8], 0i64, 1i64])
            }
            FinalizationSectionV1::NgramPostings => {
                statement.query(params![0i64, 0i64, 0i64, 1i64])
            }
            FinalizationSectionV1::FieldStatistics | FinalizationSectionV1::Vocabulary => {
                statement.query(params!["", 1i64])
            }
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
            open_private_builder_connection(&linked),
            Err(CodeLexicalArtifactErrorV1::Contract(_))
        ));
    }

    #[test]
    fn staging_operations_refuse_same_name_replacement_after_open() {
        let directory = tempfile::tempdir().expect("private staging directory");
        let artifact = directory.path().join("artifact.sqlite");
        let replacement = directory.path().join("replacement.sqlite");
        let (connection, _retained, identity) =
            create_private_builder_connection(&artifact).expect("open artifact");
        drop(connection);
        drop(create_private_file_retained(&replacement).expect("create replacement"));
        std::fs::rename(&replacement, &artifact).expect("replace staged artifact");

        assert!(matches!(
            verify_staging_file_binding(&artifact, &identity),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
    }
}
