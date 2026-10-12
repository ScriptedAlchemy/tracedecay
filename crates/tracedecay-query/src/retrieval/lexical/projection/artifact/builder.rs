mod admission;
mod append;
mod finalization;
mod mutation;
#[cfg(test)]
mod tests;

use super::super::CodeLexicalProjectionMetadataV1;
use super::clone_census::read_clone_index_census;
#[cfg(test)]
use super::clone_codec::digest_key;
use super::format::{
    BASE_SECTION_NAMES, CodeLexicalArtifactSectionDigestV1, CodeLexicalSourceSummaryV1,
    RECEIPT_RESERVATION_BYTES, SECTION_NAMES, VerifiedCodeLexicalArtifactV1,
    absorb_page_base_sections_receipt, content_metadata_bytes, contract_number,
    decode_padded_receipt_with_control, finish_base_section_receipt_fold, hash_bytes,
    initial_base_section_receipt_fold, metadata_digest, new_verified_receipt, padded_receipt,
    receipt_artifact_digest, verify_artifact_table_layout,
};
#[cfg(test)]
use super::format::{PostingListDecoderV1, PostingListEncoderV1};
use super::increment::{CarriedFilePagesV1, carry_parent_rows};
#[cfg(test)]
use super::prepared::PreparedCloneBodyV1;
use super::prepared::{PreparedCodeLexicalArtifactPageV1, prepare_page as prepare_page_values};
use super::reader::hash_artifact_file;
use super::schema::{
    CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, require_served_revision, stage_row_dictionary,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, BuilderDurabilityV1,
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1, CodeLexicalArtifactErrorV1, checkpoint,
    enter_resumable_journal, leave_resumable_journal, open_builder_connection, sqlite_corrupt,
    sqlite_error,
};
#[cfg(test)]
use super::{
    CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1, CodeLexicalArtifactBatchLimitV1,
};
pub(super) use admission::page_transient_peak_bytes;
#[cfg(test)]
use admission::{CanonicalBatchLimitLedgerV1, TERM_INSERT_CONTROL_INTERVAL, sort_insert_plan};
use admission::{
    admit_page_batch_within_memory_budget, admit_prepared_page_batch, batch_ledger_overflow,
    batch_limit, largest_exact_prepared_prefix, page_batch_ledger_charge_bytes,
    page_preparation_upper_bound_bytes, page_prepared_retained_upper_bound_bytes,
    prepare_exact_insert_plan, prepare_page_batch_admission, prepare_term_insert_plan,
    prepared_batch_memory_with_posting_plans_required_bytes, validate_prepared_page_batch,
    validated_fixed_ledger_charge,
};
pub(super) use append::stored_digest_key;
#[cfg(test)]
use append::{
    PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK, stage_field_totals, verify_clone_payload_digests,
};
use append::{
    append_prepared_clone_bodies, append_prepared_postings, append_prepared_rows,
    insert_prepared_source_page,
};
use finalization::{
    FinalizationSectionV1, PersistedFinalizationPhaseV1, PersistedFinalizationStateV1,
    ServingIndexStepAuthorityV1, absorb_section_row, advance_pre_digest_work, advance_section_rows,
    base_section_walk_error, content_epoch, ensure_content_epoch, finalization_started,
    finish_persisted_section, finish_section, initial_section_accumulator, load_finalization_state,
    merge_wake_fits_memory_temp_store, release_free_pages, require_staged_revision,
    store_finalization_state, validate_finalization_state, verify_artifact_state_metadata,
    with_cancellable_sqlite_statement,
};
#[cfg(test)]
use finalization::{
    NgramKeyBucketsV1, NgramListPassV1, build_serving_index_step, derive_statistics_step,
    ngram_partition,
};
pub(super) use mutation::{BuilderMutationGuardV1, register_builder_mutation_gate};
use mutation::{
    admitted_documents, authenticated_authority_epoch, create_schema, install_base_freeze,
    table_exists, verify_builder_mutation_gate_schema,
};
use rayon::prelude::*;
use rusqlite::config::DbConfig;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, SealedLexicalFileReplacementV1, UninterruptibleCodeIndexControlV1,
    VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1, advance_import_dictionary_digest,
    initial_import_dictionary_digest,
};
#[cfg(test)]
use tracedecay_domain::CodeGenerationId;
use tracedecay_domain::ManifestDigest;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_parent_directory};
use tracedecay_private_fs::{create_private_file_retained, open_private_file};

const SQLITE_HEADER_HISTORY_OFFSETS: [u64; 3] = [24, 40, 92];
const SEALED_HEADER_HISTORY: u32 = 1;
const PROGRESS_TAIL_QUERY: &str = "SELECT page_ordinal, next_cursor \
     FROM source_page_cursors ORDER BY page_ordinal DESC LIMIT 1";
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

    #[tracing::instrument(name = "query.artifact.create", level = "trace", skip_all)]
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
        crate::observe::Residency::Cold.record("query.artifact.residency");
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
    #[tracing::instrument(name = "query.artifact.open_or_resume", level = "trace", skip_all)]
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
        let (connection, private_file, file_identity) = {
            let _span = tracing::trace_span!("query.artifact.open.sqlite_connect").entered();
            open_private_builder_connection(path, memory_budget_bytes)
        }?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        require_staged_revision(&connection)?;
        {
            let _span = tracing::trace_span!("query.artifact.open.schema_verify").entered();
            {
                require_integrity(&connection, control)?;
                verify_artifact_table_layout(&connection)?;
                verify_builder_mutation_gate_schema(&connection)
            }
        }?;
        let expected_digest = {
            let _span = tracing::trace_span!("query.artifact.open.metadata_restore").entered();
            {
                let expected_digest = metadata_digest(&expected_metadata)?;
                verify_artifact_state_metadata(
                    &connection,
                    &expected_metadata,
                    &expected_digest,
                    control,
                )?;
                Ok::<_, CodeLexicalArtifactErrorV1>(expected_digest)
            }
        }?;
        let (receipt, finalization) = {
            let _span = tracing::trace_span!("query.artifact.open.receipt_restore").entered();
            Ok::<_, CodeLexicalArtifactErrorV1>((
                read_receipt_with_control(&connection, control)?,
                load_finalization_state(&connection)?,
            ))
        }?;
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
        validate_contiguous_pages(&connection, control)?;
        checkpoint(control)?;
        crate::observe::Residency::Rebuilding.record("query.artifact.residency");
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

    /// Stage a successor over a byte copy of the sealed `parent`, whose bytes
    /// must hash to `parent_digest` and number `parent_size_bytes`, re-encoding
    /// only the successor files of `replacements` (ascending runs) through
    /// `stage_file_pages`. The result stands where a cold build of the same
    /// generation stands on entering digest verification, so
    /// `advance_started_finalization` seals the bytes a cold build seals.
    /// The copy is named only once complete; an interrupted carry leaves a
    /// staging file holding the parent's metadata, which a resume refuses as
    /// incompatible.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate authority the carry binds once"
    )]
    #[tracing::instrument(name = "query.artifact.carry", level = "trace", skip_all)]
    pub fn carry_parent_with_memory_budget(
        path: impl AsRef<Path>,
        parent: File,
        parent_digest: &ManifestDigest,
        parent_size_bytes: u64,
        metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
        source_state_digest: ManifestDigest,
        source_format_revision: u32,
        replacements: &[SealedLexicalFileReplacementV1],
        stage_file_pages: &mut CarriedFilePagesV1<'_>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
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
        {
            let _span = tracing::trace_span!("query.artifact.carry.copy").entered();
            copy_sealed_parent(parent, parent_digest, parent_size_bytes, path, control)
        }?;
        let (mut connection, private_file, file_identity) =
            open_private_builder_connection(path, memory_budget_bytes)?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        require_staged_revision(&connection)?;
        verify_artifact_table_layout(&connection)?;
        if read_receipt_with_control(&connection, control)?.is_none()
            || finalization_started(&connection)?
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "a carried lexical parent must be a sealed artifact".to_owned(),
            ));
        }
        enter_resumable_journal(&connection)?;
        // The sealed parent's freeze triggers stay in its schema unchanged,
        // since the layout rewrite copies the schema as it stands; the carry
        // only switches them off for its own connection.
        set_triggers_enabled(&connection, false)?;
        let transaction = connection.transaction().map_err(sqlite_error)?;
        let carried = carry_parent_rows(
            &transaction,
            &metadata,
            replacements,
            stage_file_pages,
            control,
        )?;
        let content_epoch =
            i64::try_from(carried.pages + carried.documents).map_err(contract_number)?;
        transaction
            .execute(
                "UPDATE artifact_state SET metadata = ?1, metadata_digest = ?2, receipt = ?3 WHERE singleton = 1",
                params![
                    content_metadata_bytes(&metadata)?,
                    metadata_digest.as_str(),
                    vec![0u8; RECEIPT_RESERVATION_BYTES]
                ],
            )
            .map_err(sqlite_error)?;
        transaction
            .execute(
                "UPDATE content_epoch SET epoch = ?1 WHERE singleton = 1",
                [content_epoch],
            )
            .map_err(sqlite_error)?;
        store_finalization_state(
            &transaction,
            &PersistedFinalizationStateV1::new(
                content_epoch,
                source_state_digest,
                source_format_revision,
                PersistedFinalizationPhaseV1::Digest,
            )?,
        )?;
        checkpoint(control)?;
        transaction.commit().map_err(sqlite_error)?;
        set_triggers_enabled(&connection, true)?;
        tracing::debug!(
            pages = carried.pages,
            re_encoded_pages = carried.re_encoded_pages,
            carried_pages = carried.carried_pages,
            "carried lexical artifact staged over its sealed parent"
        );
        crate::observe::Residency::Rebuilding.record("query.artifact.residency");
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

    pub fn progress(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        progress(&self.connection, control)
    }

    /// The receipt of a staging file that finished finalization and awaits
    /// publication. A sealed file keeps no source cursor, so a resumed
    /// publisher publishes it rather than rescanning its source.
    pub fn sealed_receipt(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Option<VerifiedCodeLexicalArtifactV1>, CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        read_receipt_with_control(&self.connection, control)
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

    /// Exact memory charge used to admit already-prepared pages, including
    /// the fixed builder ledger and both posting insertion plans.
    pub fn prepared_batch_ledger_charge_bytes(
        &self,
        pages: &[PreparedCodeLexicalArtifactPageV1],
    ) -> Result<usize, CodeLexicalArtifactErrorV1> {
        prepared_batch_memory_with_posting_plans_required_bytes(
            self.fixed_ledger_charge_bytes,
            pages,
        )
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
    #[tracing::instrument(name = "query.artifact.append_pages", level = "trace", skip_all)]
    pub fn append_pages(
        &mut self,
        pages: &[VerifiedSealedLexicalPageV1],
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        let prepared = self.prepare_pages(pages, control)?;
        self.append_prepared_pages(&prepared, control)
    }

    /// Prepare the fresh suffix of one ordered source batch outside SQLite.
    /// Work runs on the canonical bounded indexing pool, preserves input
    /// order, holds one background CPU permit per active unit, and drains all
    /// workers before returning any failure.
    #[tracing::instrument(name = "query.artifact.prepare_pages", level = "trace", skip_all)]
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
        if read_receipt_with_control(&self.connection, control)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts do not accept more source pages".to_owned(),
            ));
        }
        if finalization_started(&self.connection)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization has started; source pages are immutable".to_owned(),
            ));
        }

        let (current, fresh_start) = {
            let _span = tracing::trace_span!("query.artifact.batch.admission").entered();
            {
                prepare_page_batch_admission(
                    &self.connection,
                    &self.metadata,
                    self.fixed_ledger_charge_bytes,
                    self.memory_budget_bytes,
                    pages,
                    control,
                )
            }
        }?;
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
        let prepared = {
                           let _span = tracing::trace_span!("query.artifact.batch.parallel_prepare").entered();
                           {
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
            }
                       }
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
        checkpoint(control)?;
        self.verify_path_binding()?;
        if read_receipt_with_control(&self.connection, control)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts do not accept more source pages".to_owned(),
            ));
        }
        if finalization_started(&self.connection)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization has started; source pages are immutable".to_owned(),
            ));
        }
        let current = progress(&self.connection, control)?;
        if pages.is_empty() {
            return Ok(current);
        }
        validate_prepared_page_batch(&current, pages)?;
        admit_prepared_page_batch(
            self.fixed_ledger_charge_bytes,
            self.memory_budget_bytes,
            pages,
        )?;
        let term_insert_plan = {
            let _span = tracing::trace_span!("query.artifact.batch.term_order").entered();
            prepare_term_insert_plan(
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
                control,
            )
        }?;
        let exact_insert_plan = {
            let _span = tracing::trace_span!("query.artifact.batch.exact_order").entered();
            prepare_exact_insert_plan(
                self.fixed_ledger_charge_bytes,
                self.memory_budget_bytes,
                pages,
                control,
            )
        }?;
        enter_resumable_journal(&self.connection)?;
        let mutation_gate = &self.mutation_gate;
        super::with_memory_statement_journals(&mut self.connection, |connection| {
            let _span = tracing::trace_span!("query.artifact.batch.sqlite").entered();
            {
                let _mutation_authority = BuilderMutationGuardV1::enter(mutation_gate)?;
                let transaction = connection.transaction().map_err(sqlite_error)?;
                let mutation = (|| {
                    {
                        let _span =
                            tracing::trace_span!("query.artifact.batch.clone_bodies").entered();
                        append_prepared_clone_bodies(&transaction, pages, control)
                    }?;
                    {
                        let _span =
                            tracing::trace_span!("query.artifact.batch.rows.stage_dictionary")
                                .entered();
                        stage_row_dictionary(&transaction, pages, control)
                    }?;
                    {
                        let _span = tracing::trace_span!("query.artifact.batch.rows").entered();
                        append_prepared_rows(&transaction, pages, control)
                    }?;
                    {
                        let _span = tracing::trace_span!("query.artifact.batch.postings").entered();
                        append_prepared_postings(
                            &transaction,
                            pages,
                            &term_insert_plan,
                            &exact_insert_plan,
                            control,
                        )
                    }?;
                    {
                        let _span = tracing::trace_span!("query.artifact.batch.receipts").entered();
                        {
                            for page in pages {
                                insert_prepared_source_page(&transaction, page)?;
                            }
                            Ok::<(), CodeLexicalArtifactErrorV1>(())
                        }
                    }?;
                    checkpoint(control)
                })();
                if let Err(error) = mutation {
                    {
                        let _span = tracing::trace_span!("query.artifact.batch.rollback").entered();
                        transaction.rollback().map_err(sqlite_error)
                    }?;
                    return Err(error);
                }

                {
                    let _span = tracing::trace_span!("query.artifact.batch.commit").entered();
                    transaction.commit().map_err(sqlite_error)
                }
            }
        })?;
        // Do not observe cancellation between durable COMMIT and publishing
        // its exact progress. The source callback must be able to advance its
        // cursor once the whole batch has committed.
        let progress = progress(&self.connection, &UninterruptibleCodeIndexControlV1)?;

        Ok(progress)
    }

    /// Advance durable receipt construction without rereading the sealed
    /// generation. Before digest verification, one wake commits exactly one
    /// set-wise statistics or serving-index statement and SQLite VM progress
    /// observes cancellation during that statement. During digest
    /// verification, `maximum_work` bounds the number of staged rows (or empty
    /// section completions) this call may consume.
    #[tracing::instrument(
        name = "query.artifact.finalization.advance_wake",
        level = "trace",
        skip_all
    )]
    pub fn advance_finalization(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactErrorV1> {
        self.advance_finalization_from(Some(source), maximum_work, control)
    }

    /// Continue a finalization that has begun. Its source totals are folded
    /// from the staged pages, so a resumed or carried build needs no sealed
    /// source receipt.
    pub fn advance_started_finalization(
        &mut self,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactErrorV1> {
        self.advance_finalization_from(None, maximum_work, control)
    }

    /// Whether finalization has begun (or the artifact is sealed), so no
    /// further source page may be appended.
    pub fn finalization_started(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<bool, CodeLexicalArtifactErrorV1> {
        Ok(
            read_receipt_with_control(&self.connection, control)?.is_some()
                || finalization_started(&self.connection)?,
        )
    }

    fn advance_finalization_from(
        &mut self,
        source: Option<&VerifiedSealedLexicalSourceReceiptV1>,
        maximum_work: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactErrorV1> {
        if maximum_work == 0 {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact finalization work budget must be non-zero".to_owned(),
            ));
        }
        checkpoint(control)?;
        self.verify_path_binding()?;
        verify_artifact_state_metadata(
            &self.connection,
            &self.metadata,
            &self.metadata_digest,
            control,
        )?;
        if let Some(receipt) = read_receipt_with_control(&self.connection, control)? {
            let summary = match source {
                Some(source) => CodeLexicalSourceSummaryV1::of_receipt(source),
                None => CodeLexicalSourceSummaryV1::of_staged_pages(
                    &self.connection,
                    receipt.source_format_revision(),
                    control,
                )?,
            };
            verify_sealed_receipt_header(&receipt, &self.metadata_digest, &summary)?;
            self.canonicalize_sealed_header()?;
            let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
            record_finalization_step(&step);
            return Ok(step);
        }
        enter_resumable_journal(&self.connection)?;

        let pending_state = load_finalization_state(&self.connection)?;
        super::select_temp_store(
            &self.connection,
            merge_wake_fits_memory_temp_store(
                &self.connection,
                pending_state
                    .as_ref()
                    .map(|state| (state.phase, state.section_ordinal)),
                self.memory_budget_bytes
                    .saturating_sub(ARTIFACT_SQLITE_CACHE_BYTES),
            )?,
        )?;
        if pending_state.is_none() {
            let Some(source) = source else {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact finalization has not begun".to_owned(),
                ));
            };
            let transaction = self.connection.transaction().map_err(sqlite_error)?;
            verify_staged_source_chain(&transaction, source, control)?;
            let content_epoch = authenticated_authority_epoch(&transaction, source)?;
            install_base_freeze(&transaction)?;
            store_finalization_state(
                &transaction,
                &PersistedFinalizationStateV1::new(
                    content_epoch,
                    source.source_state_digest().clone(),
                    source.format_revision(),
                    PersistedFinalizationPhaseV1::Indexes,
                )?,
            )?;
            checkpoint(control)?;
            commit_finalization_transaction(transaction)?;
            let step = CodeLexicalArtifactFinalizationStepV1::Pending {
                phase: CodeLexicalArtifactFinalizationPhaseV1::IndexBuild,
                completed_sections: 0,
                completed_rows: 0,
            };
            record_finalization_step(&step);
            return Ok(step);
        }

        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let mut state = load_finalization_state(&transaction)?.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization marker disappeared".to_owned(),
            )
        })?;
        validate_finalization_state(&state)?;
        ensure_content_epoch(&transaction, state.content_epoch)?;
        if source.is_some_and(|source| &state.source_state_digest != source.source_state_digest()) {
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
            commit_finalization_transaction(transaction)?;
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
            let rows =
                advance_section_rows(&transaction, section, &mut state, remaining_work, control)?;
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
            commit_finalization_transaction(transaction)?;
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
        commit_finalization_transaction(transaction)?;
        {
            let _span =
                tracing::trace_span!("query.artifact.finalization.canonical_layout").entered();
            self.rewrite_canonical_layout(control)
        }?;
        checkpoint(control)?;
        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let sections = state.completed_sections;
        let summary = CodeLexicalSourceSummaryV1::of_staged_pages(
            &transaction,
            state.source_format_revision,
            control,
        )?;
        if source.is_some_and(|source| CodeLexicalSourceSummaryV1::of_receipt(source) != summary) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact staged pages disagree with the sealed source receipt".to_owned(),
            ));
        }
        verify_final_sections_against_source(&sections, &summary)?;
        let clone_index_census = {
            let _span = tracing::trace_span!("query.artifact.finalization.clone_census").entered();
            read_clone_index_census(&transaction, control)
        }?;
        // The resume state binds the building worktree's source; the sealed
        // file keeps none of it, not even the space its row occupied.
        transaction
            .execute_batch("DROP TABLE finalization_state;")
            .map_err(sqlite_error)?;
        release_free_pages(&transaction, control)?;
        let file_size_bytes = sqlite_file_size(&transaction)?;
        let receipt = new_verified_receipt(
            self.metadata_digest.clone(),
            &summary,
            sections,
            clone_index_census,
            file_size_bytes,
        )?;
        transaction
            .execute(
                "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
                params![padded_receipt(&receipt)?],
            )
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        commit_finalization_transaction(transaction)?;
        self.canonicalize_sealed_header()?;
        let step = CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(receipt));
        record_finalization_step(&step);
        Ok(step)
    }

    /// Rewrite the staging file from its content alone.
    ///
    /// Under the builder's rollback journal an in-place `VACUUM` writes the
    /// file three times (a temporary copy, a journal of every original page,
    /// and the copy back) with syncs between them, so it stretches whenever
    /// another writer shares the disk. `VACUUM INTO` writes a private sibling
    /// once, which then replaces the staging path. The sibling carries the
    /// finalization state, so a crash on either side of the rename resumes
    /// into this same rewrite.
    #[cfg(unix)]
    fn rewrite_canonical_layout(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        let compacting = compacting_staging_sibling(&self.path)?;
        match std::fs::remove_file(&compacting) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(private_staging_error(error)),
        }
        let compacted = create_private_file_retained(&compacting)
            .map_err(|failure| private_staging_error(failure.into_error()))?;
        let target = compacting.to_str().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path is not UTF-8".to_owned(),
            )
        })?;
        // The staging WAL is named after the path the compacted file takes
        // over, so it must be folded away before the rename.
        leave_resumable_journal(&self.connection)?;
        with_cancellable_sqlite_statement(&self.connection, control, || {
            self.connection
                .execute("VACUUM INTO ?1", [target])
                .map(drop)
                .map_err(sqlite_error)
        })?;
        compacted.sync_all().map_err(private_staging_error)?;
        std::fs::rename(&compacting, &self.path).map_err(private_staging_error)?;
        sync_parent_directory(&self.path, DirectorySyncPolicy::Strict)
            .map_err(private_staging_error)?;
        let (connection, private_file, file_identity) =
            open_private_builder_connection(&self.path, self.memory_budget_bytes)?;
        if file_identity != stable_file_identity(&compacted)? {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact staging path changed while its compacted file was installed"
                    .to_owned(),
            ));
        }
        self.mutation_gate = register_builder_mutation_gate(&connection)?;
        self.connection = connection;
        self.private_file = private_file;
        self.file_identity = file_identity;
        Ok(())
    }

    /// Windows refuses to rename onto a path this builder holds open, so the
    /// file is rewritten in place.
    #[cfg(windows)]
    fn rewrite_canonical_layout(
        &mut self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        with_cancellable_sqlite_statement(&self.connection, control, || {
            self.connection
                .execute_batch("VACUUM;")
                .map_err(sqlite_error)
        })
    }

    /// SQLite's file change counter (header offset 24) and version-valid-for
    /// number (offset 92) count commits, and its schema cookie (offset 40)
    /// counts schema edits, which the layout rewrite carries forward. One
    /// content staged through a different history (a cold build, or a
    /// carried parent) differs only there. Once sealed no connection writes
    /// the file again, and all three are set to one value.
    fn canonicalize_sealed_header(&self) -> Result<(), CodeLexicalArtifactErrorV1> {
        self.verify_path_binding()?;
        leave_resumable_journal(&self.connection)?;
        let mut file = &self.private_file;
        for offset in SQLITE_HEADER_HISTORY_OFFSETS {
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&SEALED_HEADER_HISTORY.to_be_bytes()))
                .map_err(private_staging_error)?;
        }
        file.sync_all().map_err(private_staging_error)
    }

    #[tracing::instrument(name = "query.artifact.finalize", level = "trace", skip_all)]
    pub fn finalize(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        self.verify_path_binding()?;
        if let Some(receipt) = read_receipt_with_control(&self.connection, control)? {
            verify_finalized_artifact(
                &self.connection,
                &self.path,
                &self.metadata_digest,
                source,
                &receipt,
                control,
            )?;
            crate::observe::Residency::Warm.record("query.artifact.residency");
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
    let (mut connection, private_file, _) =
        create_private_builder_connection(&initializing, memory_budget_bytes)?;
    let _mutation_gate = register_builder_mutation_gate(&connection)?;
    let initialization = connection.transaction().map_err(sqlite_error)?;
    create_schema(&initialization)?;
    verify_builder_mutation_gate_schema(&initialization)?;
    #[cfg(test)]
    if take_failed_staging_initialization() {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "injected lexical artifact staging initialization failure".to_owned(),
        ));
    }
    initialization
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
    initialization.commit().map_err(sqlite_error)?;
    // SQLite names a rollback journal after the path its connection opened, so
    // the initializing connection closes before the rename; appends run under a
    // connection bound to the visible staging path. The sibling's commit did
    // not sync, so this fsync is what makes the initialized state durable
    // before it is named.
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

/// Copy the sealed `parent` under the initializing sibling and name it the
/// staging path once its bytes are durable. The carry never re-reads the
/// rows it keeps, so the copy is held to the parent's content address: a
/// damaged parent is refused as incompatible rather than sealed into its
/// successor.
fn copy_sealed_parent(
    mut parent: File,
    parent_digest: &ManifestDigest,
    parent_size_bytes: u64,
    path: &Path,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let initializing = initializing_staging_sibling(path)?;
    match std::fs::remove_file(&initializing) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(private_staging_error(error)),
    }
    let mut target = create_private_file_retained(&initializing)
        .map_err(|failure| private_staging_error(failure.into_error()))?;
    let copied = (|| {
        let mut copied_bytes = 0u64;
        let digest = hash_artifact_file(&mut parent, control, |bytes| {
            copied_bytes += bytes.len() as u64;
            target
                .write_all(bytes)
                .map_err(|error| staging_initialization_error("copy its sealed parent", error))
        })?;
        if digest != *parent_digest || copied_bytes != parent_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "carried lexical parent does not match its content address".to_owned(),
            ));
        }
        target
            .sync_all()
            .map_err(|error| staging_initialization_error("sync its carried parent", error))
    })();
    drop(target);
    let named = copied.and_then(|()| {
        std::fs::rename(&initializing, path)
            .map_err(|error| staging_initialization_error("name the staging path", error))
    });
    if let Err(error) = named {
        // Remove the incomplete carry immediately; retention is only the
        // recovery authority when the process exits before this cleanup.
        if let Err(cleanup) = std::fs::remove_file(&initializing)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %initializing.display(),
                error = %cleanup,
                "could not remove an abandoned lexical carry copy"
            );
        }
        return Err(error);
    }
    sync_parent_directory(path, DirectorySyncPolicy::Strict)
        .map_err(|error| staging_initialization_error("sync the staging directory", error))
}

fn set_triggers_enabled(
    connection: &Connection,
    enabled: bool,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, enabled)
        .map(drop)
        .map_err(sqlite_error)
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
    staging_sibling(path, ".initializing")
}

/// Named like the staging database's SQLite sidecars, so staging retirement
/// and retention treat it as part of the same staging family.
#[cfg(unix)]
fn compacting_staging_sibling(path: &Path) -> Result<PathBuf, CodeLexicalArtifactErrorV1> {
    staging_sibling(path, "-compacting")
}

fn staging_sibling(path: &Path, suffix: &str) -> Result<PathBuf, CodeLexicalArtifactErrorV1> {
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path has no file name".to_owned(),
            )
        })?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn create_private_builder_connection(
    path: &Path,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = create_private_file_retained(path)
        .map_err(|failure| private_staging_error(failure.into_error()))?;
    open_bound_builder_connection(
        path,
        private_file,
        BuilderDurabilityV1::Unpublished,
        memory_budget_bytes,
    )
}

fn open_private_builder_connection(
    path: &Path,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let private_file = open_private_file(path).map_err(private_staging_error)?;
    open_bound_builder_connection(
        path,
        private_file,
        BuilderDurabilityV1::Resumable,
        memory_budget_bytes,
    )
}

fn open_bound_builder_connection(
    path: &Path,
    private_file: File,
    durability: BuilderDurabilityV1,
    memory_budget_bytes: usize,
) -> Result<(Connection, File, StableArtifactFileIdentityV1), CodeLexicalArtifactErrorV1> {
    let identity = stable_file_identity(&private_file)?;
    let connection = open_builder_connection(path, durability, memory_budget_bytes)?;
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
            "SELECT p.page_ordinal, c.cumulative_digest, p.chunk_count, p.payload_bytes, p.import_count, p.import_payload_bytes, p.import_digest, c.next_cursor FROM source_pages p LEFT JOIN source_page_cursors c ON c.page_ordinal = p.page_ordinal ORDER BY p.page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut expected_ordinal = 0u64;
    let mut chunks = 0u64;
    let mut payload_bytes = 0u64;
    let mut imports = 0u64;
    let mut import_payload_bytes = 0u64;
    let mut import_dictionary_digest = initial_import_dictionary_digest()
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let mut terminal = None;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        checkpoint(control)?;
        let ordinal =
            u64::try_from(row.get::<_, i64>(0).map_err(sqlite_error)?).map_err(contract_number)?;
        let page_payload: i64 = row.get(3).map_err(sqlite_error)?;
        let (Some(cumulative_digest), Some(cursor_bytes)) = (
            row.get::<_, Option<String>>(1).map_err(sqlite_error)?,
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
        let import_digest = ManifestDigest::new(row.get::<_, String>(6).map_err(sqlite_error)?)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        import_dictionary_digest =
            advance_import_dictionary_digest(&import_dictionary_digest, &import_digest)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
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
            || cursor.import_dictionary_digest() != &import_dictionary_digest
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
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
}

fn source_chain_overflow() -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Corrupt(
        "lexical artifact source-page chain counter overflowed".to_owned(),
    )
}

fn verify_sealed_receipt_header(
    receipt: &VerifiedCodeLexicalArtifactV1,
    expected_metadata_digest: &ManifestDigest,
    source: &CodeLexicalSourceSummaryV1,
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
    source: &CodeLexicalSourceSummaryV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    // Receipts count admitted documents, bounded by the source's chunks;
    // the freeze already matched them against the stored rows.
    let admitted_documents = sections
        .iter()
        .find(|section| section.name == "rows")
        .map(|section| section.row_count)
        .filter(|rows| *rows <= source.total_chunks)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact rows exceed the sealed source's chunks".to_owned(),
            )
        })?;
    let expected = [
        ("source_pages", source.page_count),
        ("document_integrity", admitted_documents),
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
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
    if read_receipt_with_control(connection, control)?.is_some() {
        return Err(CodeLexicalArtifactErrorV1::Contract(
            "a sealed lexical artifact keeps no source progress; publish it".to_owned(),
        ));
    }
    let cursor_bytes = match load_finalization_state(connection)? {
        Some(state) => {
            let summary = CodeLexicalSourceSummaryV1::of_staged_pages(
                connection,
                state.source_format_revision,
                control,
            )?;
            return Ok(CodeLexicalArtifactBuildProgressV1 {
                next_page_ordinal: summary.page_count,
                completed_chunks: summary.total_chunks,
                completed_payload_bytes: summary.total_payload_bytes,
                completed_imports: summary.total_imports,
                completed_import_payload_bytes: summary.import_payload_bytes,
                import_dictionary_digest: Some(summary.import_dictionary_digest),
                cumulative_source_digest: None,
                next_cursor: None,
            });
        }
        None => {
            let tail: Option<(i64, Vec<u8>)> = connection
                .query_row(PROGRESS_TAIL_QUERY, [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
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
            "SELECT c.page_digest, c.cumulative_digest, p.chunk_count, c.payload_bytes, p.import_count, p.import_payload_bytes, p.import_digest, c.next_cursor FROM source_pages p JOIN source_page_cursors c ON c.page_ordinal = p.page_ordinal WHERE p.page_ordinal = ?1",
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
        page.import_digest().as_str(),
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
    source: &CodeLexicalSourceSummaryV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if receipt.page_count() != source.page_count
        || receipt.total_chunks() != source.total_chunks
        || receipt.total_payload_bytes() != source.total_payload_bytes
        || receipt.total_imports() != source.total_imports
        || receipt.import_payload_bytes() != source.import_payload_bytes
        || receipt.import_dictionary_digest() != &source.import_dictionary_digest
        || receipt.source_format_revision() != source.format_revision
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "sealed lexical source receipt disagrees with finalized artifact".to_owned(),
        ));
    }
    Ok(())
}

fn record_finalization_step(step: &CodeLexicalArtifactFinalizationStepV1) {
    match step {
        CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {
            crate::observe::Residency::Rebuilding.record("query.artifact.residency");
        }
        CodeLexicalArtifactFinalizationStepV1::Ready(_) => {
            crate::observe::Residency::Warm.record("query.artifact.residency");
        }
    }
}

fn commit_finalization_transaction(
    transaction: Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _span = tracing::trace_span!("query.artifact.finalization.commit").entered();
    transaction.commit().map_err(sqlite_error)
}

#[tracing::instrument(
    name = "query.artifact.finalization.sealed_verify",
    level = "trace",
    skip_all
)]
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
    verify_source_receipt(receipt, &CodeLexicalSourceSummaryV1::of_receipt(source))?;
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
    if &read_clone_index_census(connection, control)? != receipt.clone_index_census() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "finalized lexical artifact clone census does not verify".to_owned(),
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
