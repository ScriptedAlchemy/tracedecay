use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    CodeIndexImportEvidenceV1, ExtractionAdmittedCodeSearchChunkV1,
};
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, CodeIndexProductionErrorV1, VerifiedSealedLexicalCursorV1,
    VerifiedSealedLexicalPageReadV1, VerifiedSealedLexicalPageSourceV1,
    VerifiedSealedLexicalPageV1, VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_domain::{
    CodeSearchChunkAnchorV1, CodeSearchChunkV1, ExactFieldV1, ExactTechnicalTermV1,
    FileOccurrenceId, ManifestDigest,
};

use super::format::{
    ArtifactRowV1, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, CodeLexicalArtifactSectionDigestV1,
    RECEIPT_RESERVATION_BYTES, SECTION_NAMES, VerifiedCodeLexicalArtifactV1, artifact_digest,
    decode_padded_receipt, encode_field, metadata_digest, new_verified_receipt, padded_receipt,
};
use super::postings::{
    NGRAM_NORMALIZED, NGRAM_RAW_OVERRIDE, document_ngram_scratch, insert_document_ngrams,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1, CodeLexicalArtifactErrorV1, checkpoint,
    open_builder_connection, sqlite_corrupt, sqlite_error,
};
use crate::retrieval::lexical::LexicalFieldV1;

use super::super::{
    CodeLexicalProjectionMetadataV1, ProjectedChunkV1, canonical_projected_exact_term,
    exact_field_for_kind,
};

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

pub struct CodeLexicalArtifactBuilderV1 {
    path: PathBuf,
    connection: Connection,
    metadata: CodeLexicalProjectionMetadataV1,
    metadata_digest: ManifestDigest,
    memory_budget_bytes: usize,
    fixed_ledger_charge_bytes: usize,
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
        if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path already contains state".to_owned(),
            ));
        }
        let connection = open_builder_connection(path)?;
        create_schema(&connection)?;
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
        Ok(Self {
            path: path.to_path_buf(),
            connection,
            metadata,
            metadata_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    pub fn open_or_resume(
        path: impl AsRef<Path>,
        expected_metadata: CodeLexicalProjectionMetadataV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::open_or_resume_with_memory_budget(
            path,
            expected_metadata,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        )
    }

    pub fn open_or_resume_with_memory_budget(
        path: impl AsRef<Path>,
        expected_metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        expected_metadata
            .validate()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let fixed_ledger_charge_bytes =
            validated_fixed_ledger_charge(&expected_metadata, memory_budget_bytes)?;
        let path = path.as_ref();
        if !path.is_file() {
            return Err(CodeLexicalArtifactErrorV1::Io(
                "lexical artifact staging file is missing".to_owned(),
            ));
        }
        let connection = open_builder_connection(path)?;
        require_integrity(&connection)?;
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
        let stored_metadata: CodeLexicalProjectionMetadataV1 =
            serde_json::from_slice(&metadata_bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let expected_digest = metadata_digest(&expected_metadata)?;
        if stored_metadata != expected_metadata || stored_digest != expected_digest.as_str() {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "staging metadata does not match the requested generation".to_owned(),
            ));
        }
        validate_contiguous_pages(&connection)?;
        Ok(Self {
            path: path.to_path_buf(),
            connection,
            metadata: expected_metadata,
            metadata_digest: expected_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    pub fn progress(
        &self,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        progress(&self.connection)
    }

    /// The ledger bytes charged regardless of page content: the SQLite
    /// page-cache authority plus the builder-retained projection metadata.
    pub fn fixed_ledger_charge_bytes(&self) -> usize {
        self.fixed_ledger_charge_bytes
    }

    /// The deterministic ledger charge admitting `page` would add on top of
    /// the fixed charge: the page's retained owned bytes plus the measured
    /// per-chunk/per-import transient peak (chunk clone, projected row and
    /// field/token/frequency maps at capacity granularity, row and import
    /// JSON buffers, and pre-compaction n-gram scratch).
    pub fn page_ledger_charge_bytes(
        &self,
        page: &VerifiedSealedLexicalPageV1,
    ) -> Result<usize, CodeLexicalArtifactErrorV1> {
        let transient = page_transient_peak_bytes(&self.metadata, page, usize::MAX)?;
        page.retained_owned_bytes()
            .checked_add(transient)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact page ledger charge overflowed".to_owned(),
                )
            })
    }

    pub fn append_page(
        &mut self,
        page: &VerifiedSealedLexicalPageV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        if read_receipt(&self.connection)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts do not accept more source pages".to_owned(),
            ));
        }
        if page.retained_owned_bytes() > CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1 {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "sealed lexical page retained bytes exceed the {}-byte artifact input bound",
                CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
            )));
        }
        let current = progress(&self.connection)?;
        let previous = cursor_before_page(&self.connection, page.page_ordinal())?;
        page.verify_transition(previous.as_ref())
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if page.page_ordinal() < current.next_page_ordinal {
            verify_replayed_page(&self.connection, page)?;
            return Ok(current);
        }
        if page.page_ordinal() != current.next_page_ordinal {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "sealed lexical pages must be appended in exact ordinal order".to_owned(),
            ));
        }
        if let Some(cumulative) = &current.cumulative_source_digest
            && page.page_ordinal() > 0
            && cumulative == page.cumulative_digest()
        {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "sealed lexical page did not advance its cumulative digest".to_owned(),
            ));
        }
        // Ledger refusal precedes the staging transaction: a page that does
        // not fit the build memory budget leaves progress untouched.
        admit_page_within_memory_budget(
            &self.metadata,
            self.fixed_ledger_charge_bytes,
            self.memory_budget_bytes,
            page,
        )?;

        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        append_imports(&transaction, page, control)?;
        append_page_rows(&transaction, &self.metadata, page, control)?;
        insert_source_page(&transaction, page)?;
        checkpoint(control)?;
        transaction.commit().map_err(sqlite_error)?;
        progress(&self.connection)
    }

    pub fn finalize(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        if let Some(receipt) = read_receipt(&self.connection)? {
            verify_finalized_artifact(
                &self.connection,
                &self.path,
                &self.metadata_digest,
                source,
                &receipt,
                control,
            )?;
            return Ok(receipt);
        }
        Err(CodeLexicalArtifactErrorV1::Corrupt(
            "unsealed lexical artifacts require a verified source replay before finalization"
                .to_owned(),
        ))
    }

    pub fn rebuild_and_finalize<R: Read + Seek>(
        &mut self,
        source: &mut VerifiedSealedLexicalPageSourceV1<R>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        if read_receipt(&self.connection)?.is_some() {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "finalized lexical artifacts cannot be rebuilt in place".to_owned(),
            ));
        }
        // A prior interrupted replay may have left this source mid-stream;
        // the interrupted staging transaction rolled back, so the canonical
        // resume is a full replay of the same instance from page zero.
        source
            .rewind()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        // The source stages one decoded file plus one bounded page while
        // minting; that window is charged before any page is staged so the
        // replay never allocates outside the ledger claim.
        let source_window_bytes = source.staging_window_bytes();
        if self
            .fixed_ledger_charge_bytes
            .checked_add(source_window_bytes)
            .is_none_or(|charge| charge >= self.memory_budget_bytes)
        {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "the sealed source staging window needs {source_window_bytes} ledger bytes on top of the {}-byte fixed charge, exceeding the {}-byte build memory budget",
                self.fixed_ledger_charge_bytes, self.memory_budget_bytes
            )));
        }

        let metadata = self.metadata.clone();
        let metadata_digest = self.metadata_digest.clone();
        let metadata_bytes = serde_json::to_vec(&metadata)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let path = self.path.clone();
        let fixed_ledger_charge_bytes = self
            .fixed_ledger_charge_bytes
            .saturating_add(source_window_bytes);
        let memory_budget_bytes = self.memory_budget_bytes;
        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        clear_staged_projection(&transaction)?;
        checkpoint(control)?;
        let mut previous_cursor: Option<VerifiedSealedLexicalCursorV1> = None;
        let source_receipt = loop {
            checkpoint(control)?;
            // Admission AND staging run inside `next_page_if`: the source
            // cursor and hash authorities advance only after the page has
            // been fully staged into the transaction, so a bound or ledger
            // refusal, a cancellation, or a SQLite failure all leave the
            // source on the same source-minted page for the retry.
            let admitted = source
                .next_page_if(control, |page| {
                    if page.retained_owned_bytes()
                        > CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
                    {
                        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                            "sealed lexical page retained bytes exceed the {}-byte artifact input bound",
                            CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
                        )));
                    }
                    admit_page_within_memory_budget(
                        &metadata,
                        fixed_ledger_charge_bytes,
                        memory_budget_bytes,
                        page,
                    )?;
                    page.verify_transition(previous_cursor.as_ref())
                        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
                    append_imports(&transaction, page, control)?;
                    append_page_rows(&transaction, &metadata, page, control)?;
                    insert_source_page(&transaction, page)
                })
                .map_err(map_source_replay_error)?;
            let read = admitted?;
            match read {
                VerifiedSealedLexicalPageReadV1::Page(page) => {
                    previous_cursor = Some(page.next_cursor().clone());
                }
                VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
            }
        };
        source_receipt
            .verify_completion(previous_cursor.as_ref())
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let sections = compute_section_digests(&transaction, control)?;
        let artifact_digest = artifact_digest(
            &metadata_digest,
            source_receipt.source_state_digest(),
            source_receipt.format_revision(),
            source_receipt.page_count(),
            source_receipt.total_chunks(),
            source_receipt.total_payload_bytes(),
            source_receipt.total_imports(),
            source_receipt.import_payload_bytes(),
            source_receipt.import_dictionary_digest(),
            source_receipt.cumulative_digest(),
            &sections,
        )?;
        transaction
            .execute(
                "UPDATE artifact_state SET format_revision = ?1, metadata = ?2, metadata_digest = ?3 WHERE singleton = 1",
                params![
                    i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1),
                    metadata_bytes,
                    metadata_digest.as_str(),
                ],
            )
            .map_err(sqlite_error)?;
        let file_size_bytes = sqlite_file_size(&transaction)?;
        let receipt = new_verified_receipt(
            metadata.clone(),
            metadata_digest.clone(),
            &source_receipt,
            artifact_digest,
            sections,
            file_size_bytes,
        );
        let padded = padded_receipt(&receipt)?;
        transaction
            .execute(
                "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
                params![padded],
            )
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        require_integrity(&transaction)?;
        checkpoint(control)?;
        transaction.commit().map_err(sqlite_error)?;
        verify_committed_artifact_state(
            &self.connection,
            &path,
            &metadata,
            &metadata_digest,
            &source_receipt,
            &receipt,
            control,
        )?;
        Ok(receipt)
    }
}

fn map_source_replay_error(error: CodeIndexProductionErrorV1) -> CodeLexicalArtifactErrorV1 {
    match error {
        CodeIndexProductionErrorV1::Interrupted(interruption) => {
            CodeLexicalArtifactErrorV1::Interrupted(interruption)
        }
        error => CodeLexicalArtifactErrorV1::Corrupt(error.to_string()),
    }
}

/// Amortized per-entry b-tree node overhead (headers and edge pointers)
/// charged on top of each entry's key/value payload.
const BTREE_MAP_ENTRY_OVERHEAD_BYTES: usize = 16;

/// Validate a caller-selected build memory budget and return the fixed
/// ledger charge it must absorb before any page is admitted.
///
/// Create, open, and rebuild each hold up to two simultaneous metadata
/// structures (the retained copy plus a clone or the decoded stored copy)
/// and one serialized JSON copy, so the fixed charge covers all three.
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
    let serialized_bytes = serde_json::to_vec(metadata)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
        .capacity();
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

/// Refuse a page whose ledger charge does not fit the remaining budget.
///
/// Runs before the staging transaction on the append path and inside the
/// source's `next_page_if` admission on the rebuild path, so a refusal
/// never mutates staged progress and never advances the source.
fn admit_page_within_memory_budget(
    metadata: &CodeLexicalProjectionMetadataV1,
    fixed_ledger_charge_bytes: usize,
    memory_budget_bytes: usize,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let refusal = |needed: usize| {
        CodeLexicalArtifactErrorV1::Contract(format!(
            "sealed lexical page needs at least {needed} ledger bytes on top of the {fixed_ledger_charge_bytes}-byte fixed charge, exceeding the {memory_budget_bytes}-byte build memory budget"
        ))
    };
    let headroom = memory_budget_bytes.saturating_sub(fixed_ledger_charge_bytes);
    let retained = page.retained_owned_bytes();
    if retained > headroom {
        return Err(refusal(retained));
    }
    let transient_headroom = headroom - retained;
    let transient = page_transient_peak_bytes(metadata, page, transient_headroom)?;
    if transient > transient_headroom {
        return Err(refusal(retained.saturating_add(transient)));
    }
    Ok(())
}

/// The widest transient allocation staging this page performs, measured one
/// chunk or import at a time so the measurement itself never outgrows the
/// staging path. Aborts early once `abort_above` is exceeded; the returned
/// value is then a lower bound that already fails admission.
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

/// Transient owned bytes staging one admitted chunk allocates, measured by
/// performing the same clone, projection, and serialization the append path
/// performs and reading back real capacities. Components are summed as if
/// simultaneous (moved storage is counted on both sides of the move), which
/// upper-bounds every instantaneous peak on the staging path.
fn projected_chunk_transient_bytes(
    metadata: &CodeLexicalProjectionMetadataV1,
    admitted: &ExtractionAdmittedCodeSearchChunkV1,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let chunk = admitted.clone().into_chunk();
    let clone_bytes = chunk_owned_bytes(&chunk);
    let logical_path = metadata
        .logical_paths
        .get(&chunk.anchor.file_occurrence_id)
        .cloned()
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact metadata is missing path {}",
                chunk.anchor.file_occurrence_id
            ))
        })?;
    let (row, fields) = ProjectedChunkV1::new(chunk, logical_path);
    let field_bytes = fields.iter().fold(
        fields
            .len()
            .saturating_mul(std::mem::size_of::<(LexicalFieldV1, Vec<String>)>()),
        |bytes, (_, terms)| {
            terms.iter().fold(
                bytes.saturating_add(
                    terms
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                ),
                |bytes, term| bytes.saturating_add(term.capacity()),
            )
        },
    );
    // `insert_fields` compacts one per-field duplicate-frequency map at a
    // time; the ledger charges the widest such map.
    let frequency_bytes = fields
        .values()
        .map(|terms| {
            terms
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                .saturating_mul(std::mem::size_of::<(&str, u32)>())
        })
        .max()
        .unwrap_or(0);
    let (_, normalized_scratch) = document_ngram_scratch(row.normalized_text.len())?;
    let raw_scratch = if row.sanitized_text.as_str().as_bytes() != row.normalized_text.as_bytes() {
        document_ngram_scratch(row.sanitized_text.as_str().len())?.1
    } else {
        0
    };
    let row_bytes = projected_row_owned_bytes(&row);
    let artifact_row = ArtifactRowV1::from(row);
    let serialized_bytes = serde_json::to_vec(&artifact_row)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
        .capacity();
    Ok(clone_bytes
        .saturating_add(field_bytes)
        .saturating_add(frequency_bytes)
        .saturating_add(normalized_scratch)
        .saturating_add(raw_scratch)
        .saturating_add(row_bytes)
        .saturating_add(serialized_bytes))
}

fn projected_row_owned_bytes(row: &ProjectedChunkV1) -> usize {
    row.id
        .as_str()
        .len()
        .saturating_add(anchor_owned_bytes(&row.anchor))
        .saturating_add(row.language_descriptor_revision.as_str().len())
        .saturating_add(exact_terms_owned_bytes(
            row.exact_terms.capacity(),
            &row.exact_terms,
        ))
        .saturating_add(row.sanitized_text.as_str().len())
        .saturating_add(row.logical_path.capacity())
        .saturating_add(
            row.field_lengths
                .len()
                .saturating_mul(std::mem::size_of::<(LexicalFieldV1, usize)>()),
        )
        .saturating_add(row.normalized_text.capacity())
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
    serde_json::to_vec(evidence)
        .map(|bytes| bytes.capacity())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn clear_staged_projection(
    transaction: &Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(
            "
            DELETE FROM source_pages;
            DELETE FROM import_evidence;
            DELETE FROM rows;
            DELETE FROM term_postings;
            DELETE FROM term_stats;
            DELETE FROM field_stats;
            DELETE FROM exact_postings;
            DELETE FROM ngram_postings;
            DELETE FROM vocabulary;
            ",
        )
        .map_err(sqlite_error)
}

fn insert_source_page(
    transaction: &Transaction<'_>,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let cursor = encode_cursor(page.next_cursor())?;
    transaction
        .execute(
            "INSERT INTO source_pages(page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                i64::try_from(page.page_ordinal()).map_err(contract_number)?,
                page.page_digest().as_str(),
                page.cumulative_digest().as_str(),
                i64::try_from(page.chunk_count()).map_err(contract_number)?,
                i64::try_from(page.payload_bytes()).map_err(contract_number)?,
                i64::try_from(page.import_count()).map_err(contract_number)?,
                i64::try_from(page.import_payload_bytes()).map_err(contract_number)?,
                page.next_cursor().import_dictionary_digest().as_str(),
                cursor,
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
            CREATE TABLE source_pages (
                page_ordinal INTEGER PRIMARY KEY,
                page_digest TEXT NOT NULL,
                cumulative_digest TEXT NOT NULL,
                chunk_count INTEGER NOT NULL,
                payload_bytes INTEGER NOT NULL,
                import_count INTEGER NOT NULL,
                import_payload_bytes INTEGER NOT NULL,
                import_dictionary_digest TEXT NOT NULL,
                next_cursor BLOB NOT NULL
            );
            CREATE TABLE import_evidence (
                canonical BLOB PRIMARY KEY,
                evidence BLOB NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE rows (
                document_id INTEGER PRIMARY KEY,
                chunk_id TEXT NOT NULL UNIQUE,
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
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                document_id INTEGER NOT NULL,
                PRIMARY KEY(kind, ngram, document_id)
            ) WITHOUT ROWID;
            CREATE TABLE vocabulary (term TEXT PRIMARY KEY) WITHOUT ROWID;
            CREATE INDEX term_postings_by_term ON term_postings(term, field, document_id);
            ",
        )
        .map_err(sqlite_error)
}

fn append_imports(
    transaction: &Transaction<'_>,
    page: &VerifiedSealedLexicalPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for evidence in page.imports() {
        checkpoint(control)?;
        let canonical = serde_json::to_vec(evidence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO import_evidence(canonical, evidence) VALUES (?1, ?1)",
                params![canonical],
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    }
    Ok(())
}

fn append_page_rows(
    transaction: &Transaction<'_>,
    metadata: &CodeLexicalProjectionMetadataV1,
    page: &VerifiedSealedLexicalPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut document: i64 = transaction
        .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get(0))
        .map_err(sqlite_error)?;
    for admitted in page.chunks() {
        checkpoint(control)?;
        u32::try_from(document).map_err(|_| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact exceeds the posting document-id range".to_owned(),
            )
        })?;
        let chunk = admitted.clone().into_chunk();
        if chunk.anchor.generation_id != metadata.generation {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "sealed lexical page contains a foreign generation".to_owned(),
            ));
        }
        let logical_path = metadata
            .logical_paths
            .get(&chunk.anchor.file_occurrence_id)
            .cloned()
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(format!(
                    "lexical artifact metadata is missing path {}",
                    chunk.anchor.file_occurrence_id
                ))
            })?;
        let (row, fields) = ProjectedChunkV1::new(chunk, logical_path);
        insert_fields(transaction, document, &fields)?;
        insert_exact(transaction, document, &row)?;
        insert_document_ngrams(
            transaction,
            NGRAM_NORMALIZED,
            document,
            row.normalized_text.as_bytes(),
            control,
        )?;
        if row.sanitized_text.as_str().as_bytes() != row.normalized_text.as_bytes() {
            insert_document_ngrams(
                transaction,
                NGRAM_RAW_OVERRIDE,
                document,
                row.sanitized_text.as_str().as_bytes(),
                control,
            )?;
        }
        let artifact_row = ArtifactRowV1::from(row);
        let bytes = serde_json::to_vec(&artifact_row)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, ?2, ?3)",
                params![document, artifact_row.id.as_str(), bytes],
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        document = document.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact document id overflowed".to_owned(),
            )
        })?;
    }
    Ok(())
}

fn insert_fields(
    transaction: &Transaction<'_>,
    document: i64,
    fields: &BTreeMap<LexicalFieldV1, Vec<String>>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for (field, terms) in fields {
        let field = encode_field(*field)?;
        transaction
            .execute(
                "INSERT INTO field_stats(field, total_length) VALUES (?1, ?2) ON CONFLICT(field) DO UPDATE SET total_length = total_length + excluded.total_length",
                params![field, i64::try_from(terms.len()).map_err(contract_number)?],
            )
            .map_err(sqlite_error)?;
        let mut frequencies = BTreeMap::<&str, u32>::new();
        for term in terms {
            frequencies
                .entry(term)
                .and_modify(|frequency| *frequency = frequency.saturating_add(1))
                .or_insert(1);
        }
        for (term, frequency) in frequencies {
            transaction
                .execute(
                    "INSERT INTO term_postings(field, term, document_id, frequency) VALUES (?1, ?2, ?3, ?4)",
                    params![field, term, document, i64::from(frequency)],
                )
                .map_err(sqlite_error)?;
            transaction
                .execute(
                    "INSERT INTO term_stats(field, term, document_frequency) VALUES (?1, ?2, 1) ON CONFLICT(field, term) DO UPDATE SET document_frequency = document_frequency + 1",
                    params![field, term],
                )
                .map_err(sqlite_error)?;
            if field != encode_field(LexicalFieldV1::Subtoken)? {
                transaction
                    .execute("INSERT OR IGNORE INTO vocabulary(term) VALUES (?1)", [term])
                    .map_err(sqlite_error)?;
            }
        }
    }
    Ok(())
}

fn insert_exact(
    transaction: &Transaction<'_>,
    document: i64,
    row: &ProjectedChunkV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    insert_exact_posting(
        transaction,
        ExactFieldV1::Path,
        Cow::Borrowed(row.logical_path.as_bytes()),
        document,
    )?;
    for term in &row.exact_terms {
        insert_exact_posting(
            transaction,
            exact_field_for_kind(term.kind()),
            canonical_projected_exact_term(term),
            document,
        )?;
    }
    Ok(())
}

fn insert_exact_posting(
    transaction: &Transaction<'_>,
    field: ExactFieldV1,
    term: Cow<'_, [u8]>,
    document: i64,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let field = serde_json::to_string(&field)
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO exact_postings(field, term, document_id) VALUES (?1, ?2, ?3)",
            params![field, term.as_ref(), document],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

/// Aggregated `source_pages` progress: page count, chunk and payload sums,
/// import sums, then the latest dictionary digest, cumulative digest, and
/// persisted cursor.
type PersistedProgressRowV1 = (
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
);

/// One staged `source_pages` receipt row: page and cumulative digests, chunk
/// and payload counts, import counts, dictionary digest, and cursor bytes.
type StoredSourcePageRowV1 = (String, String, i64, i64, i64, i64, String, Vec<u8>);

fn progress(
    connection: &Connection,
) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1> {
    let progress: PersistedProgressRowV1 = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(chunk_count), 0), COALESCE(SUM(payload_bytes), 0), COALESCE(SUM(import_count), 0), COALESCE(SUM(import_payload_bytes), 0), (SELECT import_dictionary_digest FROM source_pages ORDER BY page_ordinal DESC LIMIT 1), (SELECT cumulative_digest FROM source_pages ORDER BY page_ordinal DESC LIMIT 1), (SELECT next_cursor FROM source_pages ORDER BY page_ordinal DESC LIMIT 1) FROM source_pages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        )
        .map_err(sqlite_error)?;
    let next_cursor = progress.7.as_deref().map(decode_cursor).transpose()?;
    let result = CodeLexicalArtifactBuildProgressV1 {
        next_page_ordinal: u64::try_from(progress.0).map_err(contract_number)?,
        completed_chunks: u64::try_from(progress.1).map_err(contract_number)?,
        completed_payload_bytes: u64::try_from(progress.2).map_err(contract_number)?,
        completed_imports: u64::try_from(progress.3).map_err(contract_number)?,
        completed_import_payload_bytes: u64::try_from(progress.4).map_err(contract_number)?,
        import_dictionary_digest: progress
            .5
            .map(ManifestDigest::new)
            .transpose()
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?,
        cumulative_source_digest: progress
            .6
            .map(ManifestDigest::new)
            .transpose()
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?,
        next_cursor,
    };
    if result.next_cursor.as_ref().is_some_and(|cursor| {
        cursor.next_page_ordinal() != result.next_page_ordinal
            || cursor.emitted_chunks() != result.completed_chunks
            || cursor.emitted_payload_bytes() != result.completed_payload_bytes
            || cursor.emitted_imports() != result.completed_imports
            || cursor.emitted_import_payload_bytes() != result.completed_import_payload_bytes
            || Some(cursor.import_dictionary_digest()) != result.import_dictionary_digest.as_ref()
            || Some(cursor.cumulative_digest()) != result.cumulative_source_digest.as_ref()
    }) || (result.next_page_ordinal == 0) != result.next_cursor.is_none()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "persisted lexical artifact progress disagrees with its exact source cursor".to_owned(),
        ));
    }
    Ok(result)
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

fn validate_contiguous_pages(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut statement = connection
        .prepare("SELECT page_ordinal FROM source_pages ORDER BY page_ordinal")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut expected = 0i64;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
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
    let queries = [
        "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages ORDER BY page_ordinal",
        "SELECT canonical, evidence FROM import_evidence ORDER BY canonical",
        "SELECT document_id, chunk_id, row FROM rows ORDER BY document_id",
        "SELECT field, term, document_id, frequency FROM term_postings ORDER BY field, term, document_id",
        "SELECT field, term, document_id FROM exact_postings ORDER BY field, term, document_id",
        "SELECT kind, ngram, document_id FROM ngram_postings ORDER BY kind, ngram, document_id",
        "SELECT 'field', field, '', total_length FROM field_stats UNION ALL SELECT 'term', field, term, document_frequency FROM term_stats UNION ALL SELECT 'vocabulary', '', term, 0 FROM vocabulary ORDER BY 1, 2, 3, 4",
    ];
    SECTION_NAMES
        .into_iter()
        .zip(queries)
        .map(|(name, query)| digest_query(connection, name, query, control))
        .collect()
}

fn digest_query(
    connection: &Connection,
    name: &str,
    query: &str,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-section.v1\0");
    hasher.update(name.as_bytes());
    let mut statement = connection.prepare(query).map_err(sqlite_error)?;
    let column_count = statement.column_count();
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut row_count = 0u64;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if row_count.is_multiple_of(4_096) {
            checkpoint(control)?;
        }
        for column in 0..column_count {
            hash_value(&mut hasher, row.get_ref(column).map_err(sqlite_error)?)?;
        }
        row_count = row_count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact section row count overflowed".to_owned(),
            )
        })?;
    }
    let digest = ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    Ok(CodeLexicalArtifactSectionDigestV1 {
        name: name.to_owned(),
        row_count,
        digest,
    })
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

fn verify_finalized_artifact(
    connection: &Connection,
    path: &Path,
    expected_metadata_digest: &ManifestDigest,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    receipt: &VerifiedCodeLexicalArtifactV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    require_integrity(connection)?;
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
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
        .len();
    if actual_size != receipt.file_size_bytes() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "artifact size changed from {} to {actual_size} while sealing",
            receipt.file_size_bytes()
        )));
    }
    Ok(())
}

fn verify_committed_artifact_state(
    connection: &Connection,
    path: &Path,
    expected_metadata: &CodeLexicalProjectionMetadataV1,
    expected_metadata_digest: &ManifestDigest,
    source: &VerifiedSealedLexicalSourceReceiptV1,
    expected_receipt: &VerifiedCodeLexicalArtifactV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let (format_revision, metadata_bytes, stored_metadata_digest, receipt_bytes): (
        i64,
        Vec<u8>,
        String,
        Vec<u8>,
    ) = connection
        .query_row(
            "SELECT format_revision, metadata, metadata_digest, receipt FROM artifact_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(sqlite_corrupt)?;
    let format_revision = u32::try_from(format_revision).map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "committed lexical artifact format revision is outside u32".to_owned(),
        )
    })?;
    let stored_metadata: CodeLexicalProjectionMetadataV1 = serde_json::from_slice(&metadata_bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    let recomputed_metadata_digest = metadata_digest(&stored_metadata)?;
    let stored_receipt = decode_padded_receipt(&receipt_bytes)?.ok_or_else(|| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "committed lexical artifact has no finalized receipt".to_owned(),
        )
    })?;
    if format_revision != CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1
        || &stored_metadata != expected_metadata
        || stored_metadata_digest != expected_metadata_digest.as_str()
        || &recomputed_metadata_digest != expected_metadata_digest
        || stored_receipt != *expected_receipt
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "committed lexical artifact singleton state does not verify".to_owned(),
        ));
    }
    verify_finalized_artifact(
        connection,
        path,
        expected_metadata_digest,
        source,
        expected_receipt,
        control,
    )
}

fn require_integrity(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
    let result: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(sqlite_corrupt)?;
    if result != "ok" {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(result));
    }
    Ok(())
}

fn contract_number(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(error.to_string())
}
