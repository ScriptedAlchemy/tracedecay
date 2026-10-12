//! Resumable index derivation and bounded section verification.

use super::super::super::{CodeLexicalProjectionMetadataV1, normalized_search_text};
use super::super::format::{
    BASE_SECTION_NAMES, CodeLexicalArtifactSectionDigestV1, PostingListDecoderV1,
    PostingListEncoderV1, SECTION_NAMES, SERVING_INDEX_STEP_COUNT_V11, STATISTICS_STEP_COUNT_V11,
    absorb_page_base_sections_receipt, content_metadata_bytes, contract_number, encode_term_lists,
    initial_base_section_receipt_fold, stored_metadata_digest,
};
use super::super::prepared::document_ngram_keys;
use super::super::row_codec::{ConnectionRowDictionaryV1, decode_artifact_row, decode_row_block};
use super::super::schema::{
    CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, derive_row_dictionary, field_code,
    require_served_revision,
};
use super::super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_corrupt, sqlite_error};
#[cfg(test)]
use super::FAIL_NEXT_FINALIZATION_MONITOR_SPAWN;
use super::admission::TERM_INSERT_CONTROL_INTERVAL;
use super::mutation::{BuilderMutationGuardV1, table_exists};
use super::{CodeLexicalArtifactFinalizationPhaseV1, hash_value};
use crate::retrieval::lexical::LexicalFieldV1;
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_domain::{CodeGenerationId, ManifestDigest};

const FINALIZATION_PROGRESS_INTERVAL_OPS: i32 = 4_096;
const FINALIZATION_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FinalizationSectionV1 {
    SourcePages,
    DocumentIntegrity,
    Rows,
    TermPostings,
    ExactPostings,
    NgramPostings,
    FieldStatistics,
    Vocabulary,
    CloneOccurrences,
    CloneExactPostings,
    CloneBodyPayloads,
}

impl FinalizationSectionV1 {
    pub(super) const ALL: [Self; 11] = [
        Self::SourcePages,
        Self::DocumentIntegrity,
        Self::Rows,
        Self::TermPostings,
        Self::ExactPostings,
        Self::NgramPostings,
        Self::FieldStatistics,
        Self::Vocabulary,
        Self::CloneOccurrences,
        Self::CloneExactPostings,
        Self::CloneBodyPayloads,
    ];

    pub(super) fn from_ordinal(ordinal: usize) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::ALL.get(ordinal).copied().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization selected an unknown section".to_owned(),
            )
        })
    }

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::SourcePages => "source_pages",
            Self::DocumentIntegrity => "document_integrity",
            Self::Rows => "rows",
            Self::TermPostings => "term_postings",
            Self::ExactPostings => "exact_postings",
            Self::NgramPostings => "ngram_postings",
            Self::CloneOccurrences => "clone_occurrences",
            Self::CloneExactPostings => "clone_exact_postings",
            Self::CloneBodyPayloads => "clone_body_payloads",
            Self::FieldStatistics => "field_stats",
            Self::Vocabulary => "vocabulary",
        }
    }

    /// Native digest query of every section the digest phase walks. Base
    /// sections carry none: their digests are adopted from page receipts.
    pub(super) const fn full_query(self) -> Option<&'static str> {
        match self {
            Self::SourcePages => Some(
                "SELECT page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt FROM source_pages ORDER BY page_ordinal",
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
            Self::FieldStatistics => {
                Some("SELECT field, total_length FROM field_stats ORDER BY field")
            }
            // The vocabulary is every sealed term with its fuzzy flag; its
            // posting lists belong to the adopted `term_postings` section.
            Self::Vocabulary => Some("SELECT term, in_fuzzy FROM term_postings ORDER BY term"),
            Self::DocumentIntegrity
            | Self::Rows
            | Self::TermPostings
            | Self::ExactPostings
            | Self::NgramPostings => None,
        }
    }

    /// Bounded resumes seek a native table key, never a computed cursor.
    pub(super) const fn seek_query(self, after: bool) -> Option<&'static str> {
        match (self, after) {
            (Self::SourcePages, false) => Some(
                "SELECT page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt FROM source_pages ORDER BY page_ordinal LIMIT ?1",
            ),
            (Self::SourcePages, true) => Some(
                "SELECT page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt FROM source_pages WHERE page_ordinal > ?1 ORDER BY page_ordinal LIMIT ?2",
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
            (
                Self::DocumentIntegrity
                | Self::Rows
                | Self::TermPostings
                | Self::ExactPostings
                | Self::NgramPostings,
                _,
            ) => None,
        }
    }

    pub(super) fn walked_query(
        self,
        after: bool,
    ) -> Result<&'static str, CodeLexicalArtifactErrorV1> {
        self.seek_query(after).ok_or_else(base_section_walk_error)
    }
}

pub(super) fn base_section_walk_error() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(
        "lexical artifact base sections are adopted from page receipts, never walked".to_owned(),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub(super) enum PersistedFinalizationKeyV1 {
    Integer(i64),
    Text(String),
    ClonePosting {
        class: i64,
        normalization_revision: i64,
        digest: Vec<u8>,
        occurrence_ordinal: i64,
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
                )
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedFinalizationStateV1 {
    pub(super) phase: PersistedFinalizationPhaseV1,
    pub(super) section_ordinal: u64,
    pub(super) section_row_count: u64,
    pub(super) section_last_key: Option<PersistedFinalizationKeyV1>,
    pub(super) section_accumulator: Vec<u8>,
    pub(super) base_section_row_counts: Vec<u64>,
    pub(super) base_section_accumulators: Vec<Vec<u8>>,
    pub(super) completed_sections: Vec<CodeLexicalArtifactSectionDigestV1>,
    pub(super) completed_rows: u64,
    pub(super) content_epoch: i64,
    pub(super) source_state_digest: ManifestDigest,
    /// The sealed source's format revision; every other source total the
    /// receipt binds is folded from `source_pages`. The state holds no
    /// cursor, so a cold and a carried build of one generation write the
    /// same row before the layout rewrite.
    pub(super) source_format_revision: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PersistedFinalizationPhaseV1 {
    Statistics,
    Indexes,
    Digest,
}

impl PersistedFinalizationPhaseV1 {
    pub(super) const fn public(self) -> CodeLexicalArtifactFinalizationPhaseV1 {
        match self {
            Self::Statistics | Self::Indexes => CodeLexicalArtifactFinalizationPhaseV1::IndexBuild,
            Self::Digest => CodeLexicalArtifactFinalizationPhaseV1::Verification,
        }
    }
}
pub(super) fn verify_clone_rows(
    connection: &Connection,
    source: &VerifiedSealedLexicalSourceReceiptV1,
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
    Ok(())
}

/// Whether a finalization wake runs with memory-backed temporary storage.
/// A merge wake inserts every merged list into a builder-gated serving
/// table, so each insert journals its statement; under file-backed storage
/// the first journal to spill stays a temp file that every later insert of
/// the wake rewrites (1.1 GB of `write()` for the 198k term lists of the
/// 768-file journey). Memory storage keeps those journals in memory but also
/// holds the merge's whole `ORDER BY` input, so a merge over staged runs is
/// admitted only when those runs fit the sorter budget. The allowance per
/// run covers the sorter's record header and list node.
pub(super) fn merge_wake_fits_memory_temp_store(
    connection: &Connection,
    pending_step: Option<(PersistedFinalizationPhaseV1, u64)>,
    sorter_budget_bytes: usize,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    let Some((PersistedFinalizationPhaseV1::Indexes, ordinal)) = pending_step else {
        return Ok(false);
    };
    let staged_runs = match ordinal {
        1 => "SELECT coalesce(sum(length(term) + length(postings) + 64), 0) FROM term_posting_runs",
        2 => "SELECT coalesce(sum(length(documents) + 64), 0) FROM exact_posting_runs",
        // The n-gram rebuild orders its lists in Rust under its own budget.
        3 => return Ok(true),
        _ => return Ok(false),
    };
    let staged_bytes: i64 = connection
        .query_row(staged_runs, [], |row| row.get(0))
        .map_err(sqlite_error)?;
    let staged_bytes = u64::try_from(staged_bytes).map_err(contract_number)?;
    Ok(staged_bytes <= u64::try_from(sorter_budget_bytes).map_err(contract_number)?)
}

pub(super) fn advance_pre_digest_work(
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

pub(super) fn with_cancellable_sqlite_statement<T>(
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

pub(super) fn derive_statistics_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    match ordinal {
        0 => {
                 let _span = tracing::trace_span!("query.artifact.finalization.derive_field_stats").entered();
                 {
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
            }
             },
        // Every staging table is gone; return its pages to the filesystem
        // before the digest and the sealed size.
        1 => { let _span = tracing::trace_span!("query.artifact.finalization.release_staging_pages").entered(); {
            // The staged per-page cursors bind the building worktree's
            // source state; the finalization state keeps the terminal one
            // until the seal.
            transaction
                .execute_batch("DROP TABLE source_page_cursors;")
                .map_err(sqlite_error)?;
            release_free_pages(transaction, control)?;
            Ok(())
        } },
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
pub(super) fn release_free_pages(
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
pub(super) struct ServingIndexStepAuthorityV1<'a> {
    pub(super) mutation_gate: &'a Arc<AtomicU8>,
    pub(super) generation: &'a CodeGenerationId,
    pub(super) ngram_memory_bytes: usize,
}

pub(super) fn build_serving_index_step(
    transaction: &Transaction<'_>,
    ordinal: u64,
    authority: &ServingIndexStepAuthorityV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mutation_gate = authority.mutation_gate;
    match ordinal {
        0 => {
            let _span =
                tracing::trace_span!("query.artifact.finalization.index.row_chunks").entered();
            {
                derive_row_dictionary(transaction)?;
                let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
                transaction
                .execute_batch(
                    "INSERT INTO row_chunks(chunk_id, document_id) SELECT chunk_id, document_id FROM row_chunk_pages ORDER BY chunk_id;
                     DROP TABLE row_chunk_pages;
                     CREATE TRIGGER frozen_row_chunks_insert BEFORE INSERT ON row_chunks BEGIN SELECT RAISE(ABORT, 'frozen lexical row chunks'); END;",
                )
                .map_err(sqlite_error)
            }
        }
        1 => {
            let _span =
                tracing::trace_span!("query.artifact.finalization.merge.term_postings").entered();
            derive_term_postings(transaction, mutation_gate, control)
        }
        2 => {
            let _span =
                tracing::trace_span!("query.artifact.finalization.merge.exact_postings").entered();
            derive_exact_postings(transaction, mutation_gate, control)
        }
        // Last, so its lists reuse the pages the dropped runs freed.
        3 => {
            let _span =
                tracing::trace_span!("query.artifact.finalization.derive.ngram_postings").entered();
            derive_ngram_postings(transaction, authority, control)
        }
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

/// An n-gram list key, `(kind, ngram)`.
type NgramKeyV1 = (i64, i64);

/// One sealed n-gram list: its key, document frequency, and canonical
/// document-set encoding.
type SealedNgramListV1 = (NgramKeyV1, i64, Vec<u8>);

/// One window's `(key, document)` pairs from one pool task, bucketed by
/// [`ngram_partition`] and in document order within each bucket.
pub(super) type NgramKeyBucketsV1 = Vec<Vec<(NgramKeyV1, u32)>>;

/// The lists of one key partition. Only this partition's worker appends to
/// them, so every list still grows by appending ascending documents.
#[derive(Default)]
struct NgramListPartitionV1 {
    lists: HashMap<(i64, i64), PostingListEncoderV1>,
    held: usize,
}

impl NgramListPartitionV1 {
    /// Append one key's ascending documents from a window.
    fn add_run(
        &mut self,
        key: (i64, i64),
        documents: impl Iterator<Item = u32>,
        lower: Option<(i64, i64)>,
        cutoff: Option<(i64, i64)>,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        if lower.is_some_and(|lower| key < lower) || cutoff.is_some_and(|cutoff| key >= cutoff) {
            return Ok(());
        }
        let held = &mut self.held;
        let list = self.lists.entry(key).or_insert_with(|| {
            *held += NGRAM_LIST_ENTRY_BYTES;
            PostingListEncoderV1::new(false)
        });
        let before = list.retained_bytes();
        for document in documents {
            list.push(document, 1)?;
        }
        self.held += list.retained_bytes() - before;
        Ok(())
    }
}

/// The partition owning `key`. Any stable split works: each key's list is
/// built whole inside one partition and sealed lists are re-sorted by key.
pub(super) fn ngram_partition(key: (i64, i64), partitions: usize) -> usize {
    let mixed = (key.0 as u64)
        .rotate_left(32)
        .wrapping_add(key.1 as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    ((mixed >> 32) as usize) % partitions
}

/// One rebuild pass's n-gram lists: keys at or above `lower` (the previous
/// pass's cutoff) and below this pass's own cutoff. Keys are split across
/// partitions so the pool builds lists in parallel; after each window, lists
/// that outgrew the memory authority shed their highest keys and the cutoff
/// drops to them, so a later pass rebuilds exactly those keys. The authority
/// is therefore checked per window: a pass may exceed it by one window's
/// postings, which the window's decoded keys already hold.
pub(super) struct NgramListPassV1 {
    lower: Option<(i64, i64)>,
    cutoff: Option<(i64, i64)>,
    partitions: Vec<NgramListPartitionV1>,
    memory_bytes: usize,
}

impl NgramListPassV1 {
    pub(super) fn new(lower: Option<(i64, i64)>, memory_bytes: usize, partitions: usize) -> Self {
        Self {
            lower,
            cutoff: None,
            partitions: std::iter::repeat_with(NgramListPartitionV1::default)
                .take(partitions.max(1))
                .collect(),
            memory_bytes: memory_bytes.max(1),
        }
    }

    pub(super) fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    /// Append one window's bucketed keys. Buckets arrive in document order,
    /// so each partition appends ascending documents to its own lists.
    pub(super) fn add_window(
        &mut self,
        buckets: &[NgramKeyBucketsV1],
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        let (lower, cutoff) = (self.lower, self.cutoff);
        let partitions = &mut self.partitions;
        tracedecay_code_index::parallelism::install(|| {
            partitions
                .par_iter_mut()
                .enumerate()
                .try_for_each(|(index, partition)| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        // One hash lookup per key and window instead of per
                        // posting; a stable sort keeps each key's documents
                        // in the ascending order the tasks produced them.
                        let mut postings = buckets
                            .iter()
                            .flat_map(|task| task[index].iter().copied())
                            .collect::<Vec<_>>();
                        postings.sort_by_key(|(key, _)| *key);
                        for run in postings.chunk_by(|left, right| left.0 == right.0) {
                            let [(key, _), ..] = run else { continue };
                            partition.add_run(
                                *key,
                                run.iter().map(|(_, document)| *document),
                                lower,
                                cutoff,
                            )?;
                        }
                        Ok(())
                    })
                })
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))??;
        self.shed_to_memory();
        Ok(())
    }

    pub(super) fn shed_to_memory(&mut self) {
        let mut held = self
            .partitions
            .iter()
            .map(|partition| partition.held)
            .sum::<usize>();
        if held <= self.memory_bytes {
            return;
        }
        let partition_count = self.partition_count();
        let mut keys = self
            .partitions
            .iter()
            .flat_map(|partition| partition.lists.keys().copied())
            .collect::<Vec<_>>();
        keys.sort_unstable();
        while held > self.memory_bytes / 2 && keys.len() > 1 {
            let Some(shed) = keys.pop() else { break };
            let partition = &mut self.partitions[ngram_partition(shed, partition_count)];
            if let Some(list) = partition.lists.remove(&shed) {
                let released = list.retained_bytes() + NGRAM_LIST_ENTRY_BYTES;
                partition.held -= released;
                held -= released;
            }
            self.cutoff = Some(shed);
        }
    }

    /// This pass's sealed lists in key order, and where the next pass starts.
    pub(super) fn finish(
        self,
    ) -> Result<(Vec<SealedNgramListV1>, Option<NgramKeyV1>), CodeLexicalArtifactErrorV1> {
        let partitions = self.partitions;
        let sealed = tracedecay_code_index::parallelism::install(|| {
            partitions
                .into_par_iter()
                .map(|partition| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        partition
                            .lists
                            .into_iter()
                            .map(|(key, list)| {
                                let document_frequency =
                                    i64::try_from(list.len()).map_err(contract_number)?;
                                Ok((key, document_frequency, list.finish_document_set()?))
                            })
                            .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))??;
        let mut lists = sealed.into_iter().flatten().collect::<Vec<_>>();
        lists.sort_unstable_by_key(|(key, _, _)| *key);
        Ok((lists, self.cutoff))
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
        let partitions = pass.partition_count();
        let buckets = tracedecay_code_index::parallelism::install(|| {
            decoded
                .par_chunks(NGRAM_DERIVE_ROWS_PER_TASK)
                .map(|rows| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        let mut buckets: NgramKeyBucketsV1 = vec![Vec::new(); partitions];
                        for (document, row) in rows {
                            for key in document_ngram_keys(
                                &normalized_search_text(row),
                                row.sanitized_text.as_str(),
                                &row.normalized_text,
                                control,
                            )? {
                                buckets[ngram_partition(key, partitions)].push((key, *document));
                            }
                        }
                        Ok(buckets)
                    })
                })
                .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))??;
        drop(decoded);
        pass.add_window(&buckets)
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
    let partitions = tracedecay_code_index::parallelism::install(rayon::current_num_threads)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut lower: Option<(i64, i64)> = None;
    loop {
        let mut pass = NgramListPassV1::new(lower, authority.ngram_memory_bytes, partitions);
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
        let (lists, cutoff) = pass.finish()?;
        for (ordinal, (key, document_frequency, documents)) in lists.into_iter().enumerate() {
            if ordinal.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
                checkpoint(control)?;
            }
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
    pub(super) fn new(
        content_epoch: i64,
        source_state_digest: ManifestDigest,
        source_format_revision: u32,
        phase: PersistedFinalizationPhaseV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        if content_epoch < 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact mutation epoch is negative".to_owned(),
            ));
        }
        let (base_section_row_counts, base_section_accumulators) =
            initial_base_section_receipt_fold()?;
        Ok(Self {
            phase,
            section_ordinal: 0,
            section_row_count: 0,
            section_last_key: None,
            section_accumulator: initial_section_accumulator(SECTION_NAMES[0])?.to_vec(),
            base_section_row_counts,
            base_section_accumulators,
            completed_sections: Vec::new(),
            completed_rows: 0,
            content_epoch,
            source_state_digest,
            source_format_revision,
        })
    }
}

pub(super) fn verify_artifact_state_metadata(
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

pub(super) fn require_staged_revision(
    connection: &Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
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

pub(super) fn finalization_started(
    connection: &Connection,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    Ok(load_finalization_state(connection)?.is_some())
}

pub(super) fn content_epoch(connection: &Connection) -> Result<i64, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT epoch FROM content_epoch WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_corrupt)
}

pub(super) fn ensure_content_epoch(
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
pub(super) fn load_finalization_state(
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

pub(super) fn store_finalization_state(
    transaction: &Transaction<'_>,
    state: &PersistedFinalizationStateV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    // Created by the first finalization step on both build paths, so the
    // table sits after every content table when the layout is rewritten.
    transaction
        .execute_batch(FINALIZATION_STATE_SCHEMA)
        .map_err(sqlite_error)?;
    transaction
        .execute(
            "INSERT INTO finalization_state(singleton, state) VALUES (1, ?1) ON CONFLICT(singleton) DO UPDATE SET state = excluded.state",
            params![bytes],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

const FINALIZATION_STATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS finalization_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                state BLOB NOT NULL
            );";

pub(super) fn validate_finalization_state(
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

pub(super) fn advance_section_rows(
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
            | FinalizationSectionV1::CloneBodyPayloads,
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
    if matches!(
        section,
        FinalizationSectionV1::CloneOccurrences
            | FinalizationSectionV1::CloneExactPostings
            | FinalizationSectionV1::CloneBodyPayloads
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
        | FinalizationSectionV1::Rows
        | FinalizationSectionV1::TermPostings
        | FinalizationSectionV1::ExactPostings
        | FinalizationSectionV1::NgramPostings => Err(base_section_walk_error()),
        FinalizationSectionV1::CloneOccurrences
        | FinalizationSectionV1::CloneExactPostings
        | FinalizationSectionV1::CloneBodyPayloads => Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone row key bypassed its dedicated decoder".to_owned(),
        )),
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
    Err(CodeLexicalArtifactErrorV1::Corrupt(
        "non-clone section requested a clone row key".to_owned(),
    ))
}

pub(super) fn initial_section_accumulator(
    name: &str,
) -> Result<[u8; 32], CodeLexicalArtifactErrorV1> {
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

pub(super) fn absorb_section_row(
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

pub(super) fn finish_persisted_section(
    name: &str,
    state: &PersistedFinalizationStateV1,
) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
    finish_section(name, state.section_row_count, &state.section_accumulator)
}

pub(super) fn finish_section(
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
