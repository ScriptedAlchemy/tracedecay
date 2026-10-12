//! Page and prepared-batch admission, memory charges, and sorted posting plans.

use super::super::super::CodeLexicalProjectionMetadataV1;
use super::super::format::contract_number;
use super::super::postings::document_ngram_scratch;
use super::super::prepared::{PreparedCodeLexicalArtifactPageV1, PreparedTermPostingV1};
use super::super::schema::{exact_field_code_from_encoded, field_code, stable_exact_term_id};
use super::super::{
    ARTIFACT_SQLITE_CACHE_BYTES, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_CAP_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PREPARED_BATCH_ROWS_V1, CodeLexicalArtifactBatchLimitV1,
    CodeLexicalArtifactErrorV1, NGRAM_AGGREGATION_BYTES_PER_LOGICAL_POSTING_V1, checkpoint,
};
use super::{
    CodeLexicalArtifactBuildProgressV1, cursor_before_page, decode_cursor, encode_cursor, progress,
    verify_replayed_page,
};
use crate::retrieval::lexical::LexicalFieldV1;
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::HashMap;
use tracedecay_code_index::chunks::ExtractionAdmittedCodeSearchChunkV1;
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, VerifiedSealedLexicalPageV1, VerifiedSealedLexicalSymbolDisplayV1,
    advance_import_dictionary_digest, initial_import_dictionary_digest,
};
use tracedecay_domain::{
    CodeSearchChunkAnchorV1, CodeSearchChunkV1, ExactTechnicalTermV1, FileOccurrenceId,
};

// A plan entry owns its sort key (borrowed term text, integer field code,
// document id) and one posting reference. Five words match the 64-bit
// layout and cover the 32-bit one; general allocator metadata remains
// outside the ledger contract.
const TERM_INSERT_PLAN_BYTES_PER_REF: usize = 5 * std::mem::size_of::<usize>();
pub(super) const TERM_INSERT_CONTROL_INTERVAL: usize = 4_096;
// An exact-posting plan entry owns its document, field, and term identifiers.
// Eight words conservatively over-reserves that entry on both the 32-bit and
// 64-bit layouts; general allocator metadata remains outside the ledger
// contract.
const EXACT_INSERT_PLAN_BYTES_PER_REF: usize = 8 * std::mem::size_of::<usize>();
pub(super) const EXACT_INSERT_CONTROL_INTERVAL: usize = TERM_INSERT_CONTROL_INTERVAL;
/// Rows per multi-row `INSERT ... VALUES (...), (...)` statement in the
/// append phase. The per-row cost of the base-table inserts is statement
/// overhead plus the per-row builder-gate trigger, not I/O: on a
/// one-row-per-posting shape at production pragmas (120k sorted rows in one
/// transaction) one row per statement costs 1.47 µs/row and 32 rows per
/// statement 0.86 µs/row, the trigger still firing for every row. Five
/// columns × 32 rows stays under SQLite's 999-parameter floor.
pub(super) const INSERT_ROWS_PER_STATEMENT: usize = 32;
/// One planned posting, keyed `(term, field, document_id)` so the merged
/// stream yields each `(term, field)` run of the batch contiguously and in
/// document order, ready to encode as one `term_posting_runs` list.
#[derive(Clone, Copy)]
pub(super) struct PreparedTermInsertRefV1<'a> {
    pub(super) key: (&'a str, i64, i64),
    pub(super) posting: &'a PreparedTermPostingV1,
}

impl<'a> PreparedTermInsertRefV1<'a> {
    fn new(document_id: i64, field: i64, posting: &'a PreparedTermPostingV1) -> Self {
        Self {
            key: (posting.term.as_str(), field, document_id),
            posting,
        }
    }

    pub(super) fn key(&self) -> (&'a str, i64, i64) {
        self.key
    }
}

/// Every planned term posting of one batch, sorted by key. Keys are unique
/// (one posting per document, field, and term), so the order is total.
pub(super) struct PreparedTermInsertPlanV1<'a> {
    pub(super) entries: Vec<PreparedTermInsertRefV1<'a>>,
}

// Exact postings sort the same way as term postings, keyed
// `(term_id, field, document_id)` so each `exact_posting_runs` list is
// contiguous in the sorted stream.
#[derive(Clone, Copy)]
pub(super) struct PreparedExactInsertRefV1 {
    pub(super) document_id: i64,
    pub(super) field_code: i64,
    pub(super) term_id: i64,
}

impl PreparedExactInsertRefV1 {
    pub(super) fn key(&self) -> (i64, i64, i64) {
        (self.term_id, self.field_code, self.document_id)
    }
}

pub(super) struct PreparedExactInsertPlanV1<'a> {
    pub(super) entries: Vec<PreparedExactInsertRefV1>,
    /// The batch's distinct exact terms with the ids this plan
    /// content-addressed, ascending by id, the order `exact_vocabulary` was
    /// always interned in.
    pub(super) interned_terms: Vec<(&'a [u8], i64)>,
}
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
pub(super) fn validated_fixed_ledger_charge(
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

pub(super) fn page_batch_ledger_charge_bytes(
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
pub(super) struct CanonicalBatchLimitLedgerV1 {
    pub(super) estimated_rows: usize,
    pub(super) estimated_write_bytes: usize,
}

#[derive(Clone, Copy)]
pub(super) struct BatchLimitExceededV1 {
    pub(super) limit: CodeLexicalArtifactBatchLimitV1,
    pub(super) required: usize,
    pub(super) maximum: usize,
}

impl CanonicalBatchLimitLedgerV1 {
    pub(super) fn try_admit(
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

pub(super) fn largest_exact_prepared_prefix(
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
pub(super) fn admit_page_batch_within_memory_budget(
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

pub(super) fn prepared_batch_memory_with_posting_plans_required_bytes(
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

pub(super) fn admit_prepared_page_batch(
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

pub(super) fn prepare_term_insert_plan<'a>(
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
                    posting.field_code,
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
/// observed between them, and each wave takes a background CPU permit.
/// Pairwise merge rounds then double the sorted run length on the pool until
/// one run remains, checkpointing between rounds. Each round holds a second
/// copy of the plan until the first is dropped.
pub(super) fn sort_insert_plan<T, K>(
    entries: &mut Vec<T>,
    key: impl Fn(&T) -> K + Copy + Sync,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1>
where
    T: Copy + Send + Sync,
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
    mut run: usize,
    key: impl Fn(&T) -> K + Copy + Sync,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1>
where
    T: Copy + Send + Sync,
    K: Ord,
{
    let mut merged = Vec::new();
    merged.try_reserve_exact(entries.len()).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical insert plan merge allocation failed: {error}"
        ))
    })?;
    merged.extend_from_slice(entries);
    while run < entries.len() {
        checkpoint(control)?;
        let pair = run.saturating_mul(2);
        let source = entries.as_slice();
        tracedecay_code_index::parallelism::install(|| {
            source
                .par_chunks(pair)
                .zip(merged.par_chunks_mut(pair))
                .for_each(|(input, output)| {
                    tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                        let (left, right) = input.split_at(run.min(input.len()));
                        merge_two_runs(left, right, output, key);
                    });
                });
        })
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        std::mem::swap(entries, &mut merged);
        run = pair;
    }
    Ok(())
}

/// Stable merge of two sorted runs into `output`, which holds exactly
/// `left.len() + right.len()` slots.
fn merge_two_runs<T: Copy, K: Ord>(
    left: &[T],
    right: &[T],
    output: &mut [T],
    key: impl Fn(&T) -> K,
) {
    let (mut left_index, mut right_index) = (0, 0);
    for slot in output {
        let take_left = right_index == right.len()
            || (left_index < left.len() && key(&left[left_index]) <= key(&right[right_index]));
        if take_left {
            *slot = left[left_index];
            left_index += 1;
        } else {
            *slot = right[right_index];
            right_index += 1;
        }
    }
}

// Mirrors `prepare_term_insert_plan` for `exact_postings`,
// whose `PRIMARY KEY(field, term, document_id)` `WITHOUT ROWID` layout has
// the same clustered-index cost for out-of-order inserts as `term_postings`.
pub(super) fn prepare_exact_insert_plan<'a>(
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

pub(super) fn batch_limit(
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

pub(super) fn batch_ledger_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(
        "lexical artifact batch ledger charge overflowed".to_owned(),
    )
}

pub(super) fn prepare_page_batch_admission(
    connection: &Connection,
    metadata: &CodeLexicalProjectionMetadataV1,
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    pages: &[VerifiedSealedLexicalPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(CodeLexicalArtifactBuildProgressV1, usize), CodeLexicalArtifactErrorV1> {
    admit_page_batch_within_memory_budget(
        metadata,
        fixed_ledger_charge_bytes,
        memory_budget_bytes,
        pages,
    )?;
    let current = progress(connection, control)?;
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

/// The widest transient upper bound one staged chunk can require.
/// It is evaluated a record at a time without allocations and aborts once
/// `abort_above` is exceeded. The returned lower bound already fails admission.
pub(in super::super) fn page_transient_peak_bytes(
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
    Ok(peak)
}

/// Conservative output-plus-scratch upper bound before one page is prepared.
/// Every derived retained value coexists with that page's widest transient
/// record allocation.
pub(super) fn page_preparation_upper_bound_bytes(
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
pub(super) fn page_prepared_retained_upper_bound_bytes(
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
    chunk_bytes
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

/// Page-level prepared ownership that is not attributable to one chunk.
/// Duplicating the source page's complete retained charge covers
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

pub(super) fn validate_prepared_page_batch(
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
        if usize::try_from(page.chunk_count).map_err(contract_number)? < page.documents.len() {
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
        let previous_import_dictionary = match expected_previous.as_deref() {
            Some(previous) => decode_cursor(previous)?.import_dictionary_digest().clone(),
            None => initial_import_dictionary_digest()
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
        };
        let import_dictionary =
            advance_import_dictionary_digest(&previous_import_dictionary, &page.import_digest)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        if next_cursor.import_dictionary_digest() != &import_dictionary
            || next_cursor.next_page_ordinal()
                != expected_ordinal.checked_add(1).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "prepared lexical page ordinal overflowed".to_owned(),
                    )
                })?
            || next_cursor.emitted_chunks() != expected_document
            || next_cursor.cumulative_digest() != &page.cumulative_digest
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
