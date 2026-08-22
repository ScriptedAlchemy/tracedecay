use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

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
    CodeSearchChunkAnchorV1, CodeSearchChunkV1, ExactFieldV1, ExactTechnicalTermV1,
    FileOccurrenceId, ManifestDigest,
};
use tracedecay_private_fs::{create_private_file_retained, open_private_file};

use super::format::{
    ArtifactRowV1, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, CodeLexicalArtifactSectionDigestV1,
    RECEIPT_RESERVATION_BYTES, SECTION_NAMES, VerifiedCodeLexicalArtifactV1, artifact_digest,
    decode_padded_receipt, decode_padded_receipt_with_control, encode_field, metadata_digest,
    new_verified_receipt, padded_receipt,
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
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages ORDER BY page_ordinal"
            }
            Self::DocumentIntegrity => {
                "SELECT document_id, digest FROM document_integrity ORDER BY document_id"
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
                "SELECT kind, ngram, document_id FROM ngram_postings ORDER BY kind, ngram, document_id"
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
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages ORDER BY page_ordinal LIMIT ?1"
            }
            (Self::SourcePages, true) => {
                "SELECT page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor FROM source_pages WHERE page_ordinal > ?1 ORDER BY page_ordinal LIMIT ?2"
            }
            (Self::DocumentIntegrity, false) => {
                "SELECT document_id, digest FROM document_integrity ORDER BY document_id LIMIT ?1"
            }
            (Self::DocumentIntegrity, true) => {
                "SELECT document_id, digest FROM document_integrity WHERE document_id > ?1 ORDER BY document_id LIMIT ?2"
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
                "SELECT kind, ngram, document_id FROM ngram_postings ORDER BY kind, ngram, document_id LIMIT ?1"
            }
            (Self::NgramPostings, true) => {
                "SELECT kind, ngram, document_id FROM ngram_postings WHERE (kind, ngram, document_id) > (?1, ?2, ?3) ORDER BY kind, ngram, document_id LIMIT ?4"
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
        kind: i64,
        ngram: i64,
        document_id: i64,
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
pub enum CodeLexicalArtifactFinalizationStepV1 {
    Pending {
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
    completed_sections: Vec<CodeLexicalArtifactSectionDigestV1>,
    completed_rows: u64,
    content_epoch: i64,
    source_state_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedFinalizationPhaseV1 {
    Build,
    Verify,
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
    file_index_high: u32,
    #[cfg(windows)]
    file_index_low: u32,
}

pub struct CodeLexicalArtifactBuilderV1 {
    path: PathBuf,
    /// Keeps the exact no-follow/private file handle alive while the SQLite
    /// connection is in use. Every public transition rebinds the pathname to
    /// this identity before it trusts the connection's contents.
    _private_file: File,
    file_identity: StableArtifactFileIdentityV1,
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
        if path.try_exists().map_err(private_staging_error)? {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact staging path already contains state".to_owned(),
            ));
        }
        let (connection, private_file, file_identity) = create_private_builder_connection(path)?;
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
            _private_file: private_file,
            file_identity,
            connection,
            metadata,
            metadata_digest,
            memory_budget_bytes,
            fixed_ledger_charge_bytes,
        })
    }

    /// Reopen only the staged artifact authority while applying the caller's
    /// scheduler epoch/deadline control to integrity, metadata, receipt, and
    /// contiguous-cursor verification.
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
        let (connection, private_file, file_identity) = open_private_builder_connection(path)?;
        require_integrity(&connection, control)?;
        let expected_digest = metadata_digest(&expected_metadata)?;
        verify_artifact_state_metadata(&connection, &expected_metadata, &expected_digest, control)?;
        read_receipt_with_control(&connection, control)?;
        validate_contiguous_pages(&connection, control)?;
        checkpoint(control)?;
        Ok(Self {
            path: path.to_path_buf(),
            _private_file: private_file,
            file_identity,
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
    /// arithmetic per-chunk/per-import transient upper bound (chunk clone,
    /// projected row, field/token/frequency maps, JSON buffers, and n-gram
    /// scratch), without allocating during admission.
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

    /// Advance durable receipt construction without rereading the sealed
    /// generation. `maximum_work` bounds the number of staged rows (or empty
    /// section completions) this call may consume.
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
            return Ok(CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(
                receipt,
            )));
        }

        if load_finalization_state(&self.connection)?.is_none() {
            verify_staged_source_tail(&self.connection, source)?;
            let content_epoch = content_epoch(&self.connection)?;
            let transaction = self.connection.transaction().map_err(sqlite_error)?;
            store_finalization_state(
                &transaction,
                &PersistedFinalizationStateV1::new(content_epoch, source)?,
            )?;
            transaction.commit().map_err(sqlite_error)?;
        }

        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let mut state = load_finalization_state(&transaction)?.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact finalization marker disappeared".to_owned(),
            )
        })?;
        validate_finalization_state(&state)?;
        ensure_content_epoch(&transaction, state.content_epoch)?;
        if &state.source_state_digest != source.source_state_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "bounded lexical artifact finalization received a different source receipt"
                    .to_owned(),
            ));
        }
        let mut remaining_work = maximum_work;
        let section_count = u64::try_from(SECTION_NAMES.len()).map_err(contract_number)?;
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
            match state.phase {
                PersistedFinalizationPhaseV1::Build => {
                    state.completed_sections.push(section_digest)
                }
                PersistedFinalizationPhaseV1::Verify => {
                    let expected =
                        state
                            .completed_sections
                            .get(section_ordinal)
                            .ok_or_else(|| {
                                CodeLexicalArtifactErrorV1::Corrupt(
                                    "lexical artifact verification has no matching build section"
                                        .to_owned(),
                                )
                            })?;
                    if &section_digest != expected {
                        return Err(CodeLexicalArtifactErrorV1::Corrupt(
                            "lexical artifact changed between bounded finalization wakes"
                                .to_owned(),
                        ));
                    }
                }
            }
            state.section_ordinal = state.section_ordinal.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact finalization section ordinal overflowed".to_owned(),
                )
            })?;
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
            transaction.commit().map_err(sqlite_error)?;
            return Ok(CodeLexicalArtifactFinalizationStepV1::Pending {
                completed_sections: u64::try_from(state.completed_sections.len())
                    .map_err(contract_number)?,
                completed_rows: state.completed_rows,
            });
        }

        if state.phase == PersistedFinalizationPhaseV1::Build {
            state.phase = PersistedFinalizationPhaseV1::Verify;
            state.section_ordinal = 0;
            state.section_row_count = 0;
            state.section_last_key = None;
            state.section_accumulator = initial_section_accumulator(SECTION_NAMES[0])?.to_vec();
            store_finalization_state(&transaction, &state)?;
            checkpoint(control)?;
            transaction.commit().map_err(sqlite_error)?;
            return Ok(CodeLexicalArtifactFinalizationStepV1::Pending {
                completed_sections: u64::try_from(state.completed_sections.len())
                    .map_err(contract_number)?,
                completed_rows: state.completed_rows,
            });
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
        transaction.commit().map_err(sqlite_error)?;
        Ok(CodeLexicalArtifactFinalizationStepV1::Ready(Box::new(
            receipt,
        )))
    }

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
    let metadata = file.metadata().map_err(private_staging_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(StableArtifactFileIdentityV1 {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        Ok(StableArtifactFileIdentityV1 {
            volume_serial_number: metadata.volume_serial_number(),
            file_index_high: metadata.file_index_high(),
            file_index_low: metadata.file_index_low(),
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

/// Refuse a page whose ledger charge does not fit the remaining budget.
///
/// Runs before the staging transaction, so a refusal never mutates staged
/// progress and an upstream caller can decline the page before advancing its
/// sealed source cursor.
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
    // Normalization can expand one Unicode scalar to at most three scalars;
    // JSON can then escape each byte. This bound intentionally charges both
    // cloned and moved ownership before append allocates either representation.
    let text_bytes = chunk.sanitized_text.as_str().len();
    let subtoken_bytes = chunk
        .subtokens
        .iter()
        .fold(0usize, |total, term| total.saturating_add(term.len()));
    let exact_bytes = chunk.exact_terms.iter().fold(0usize, |total, term| {
        total.saturating_add(term.canonical_bytes().len())
    });
    let normalized_text_bytes = text_bytes.saturating_mul(3);
    let field_text_bytes = normalized_text_bytes
        .saturating_add(logical_path.len().saturating_mul(3))
        .saturating_add(subtoken_bytes.saturating_mul(3))
        .saturating_add(exact_bytes.saturating_mul(6));
    let field_entries = text_bytes
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
                next_cursor BLOB NOT NULL
            );
            -- Every derived document and import receives its digest in the
            -- same transaction that admits it. Bounded finalization verifies
            -- these receipts before it seals a self-contained artifact, so a
            -- pre-seal mutation cannot attest itself without rereading source.
            CREATE TABLE document_integrity (
                document_id INTEGER PRIMARY KEY,
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
            CREATE TRIGGER content_epoch_source_pages_insert AFTER INSERT ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_source_pages_update AFTER UPDATE ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_source_pages_delete AFTER DELETE ON source_pages BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_document_integrity_insert AFTER INSERT ON document_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_document_integrity_update AFTER UPDATE ON document_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_document_integrity_delete AFTER DELETE ON document_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_integrity_insert AFTER INSERT ON import_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_integrity_update AFTER UPDATE ON import_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_integrity_delete AFTER DELETE ON import_integrity BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_evidence_insert AFTER INSERT ON import_evidence BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_evidence_update AFTER UPDATE ON import_evidence BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_import_evidence_delete AFTER DELETE ON import_evidence BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_rows_insert AFTER INSERT ON rows BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_rows_update AFTER UPDATE ON rows BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_rows_delete AFTER DELETE ON rows BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_postings_insert AFTER INSERT ON term_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_postings_update AFTER UPDATE ON term_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_postings_delete AFTER DELETE ON term_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_stats_insert AFTER INSERT ON term_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_stats_update AFTER UPDATE ON term_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_term_stats_delete AFTER DELETE ON term_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_field_stats_insert AFTER INSERT ON field_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_field_stats_update AFTER UPDATE ON field_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_field_stats_delete AFTER DELETE ON field_stats BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_exact_postings_insert AFTER INSERT ON exact_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_exact_postings_update AFTER UPDATE ON exact_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_exact_postings_delete AFTER DELETE ON exact_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_ngram_postings_insert AFTER INSERT ON ngram_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_ngram_postings_update AFTER UPDATE ON ngram_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_ngram_postings_delete AFTER DELETE ON ngram_postings BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_vocabulary_insert AFTER INSERT ON vocabulary BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_vocabulary_update AFTER UPDATE ON vocabulary BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            CREATE TRIGGER content_epoch_vocabulary_delete AFTER DELETE ON vocabulary BEGIN UPDATE content_epoch SET epoch = epoch + 1 WHERE singleton = 1; END;
            ",
        )
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
        Ok(Self {
            phase: PersistedFinalizationPhaseV1::Build,
            section_ordinal: 0,
            section_row_count: 0,
            section_last_key: None,
            section_accumulator: initial_section_accumulator(SECTION_NAMES[0])?.to_vec(),
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
    let completed_section_count = match state.phase {
        PersistedFinalizationPhaseV1::Build => {
            usize::try_from(state.section_ordinal).map_err(contract_number)?
        }
        PersistedFinalizationPhaseV1::Verify => SECTION_NAMES.len(),
    };
    if state.section_ordinal > section_count
        || state.completed_sections.len() != completed_section_count
        || state.completed_sections.len() > SECTION_NAMES.len()
        || state.section_accumulator.len() != 32
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
                kind,
                ngram,
                document_id,
            }),
        ) => advance_native_section_rows(
            transaction,
            section,
            section.seek_query(true),
            params![kind, ngram, document_id, limit],
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
        if matches!(
            section,
            FinalizationSectionV1::DocumentIntegrity | FinalizationSectionV1::ImportIntegrity
        ) {
            verify_integrity_row(transaction, section, row)?;
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
                kind: row.get(0).map_err(sqlite_error)?,
                ngram: row.get(1).map_err(sqlite_error)?,
                document_id: row.get(2).map_err(sqlite_error)?,
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

fn verify_integrity_row(
    transaction: &Transaction<'_>,
    section: FinalizationSectionV1,
    row: &rusqlite::Row<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let expected: String = row.get(1).map_err(sqlite_error)?;
    let actual = match section {
        FinalizationSectionV1::DocumentIntegrity => match row.get_ref(0).map_err(sqlite_error)? {
            ValueRef::Integer(document) if document >= 0 => {
                document_integrity_digest(transaction, document)?
            }
            _ => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact document integrity receipt has an invalid key".to_owned(),
                ));
            }
        },
        FinalizationSectionV1::ImportIntegrity => match row.get_ref(0).map_err(sqlite_error)? {
            ValueRef::Blob(canonical) => {
                let evidence: Option<Vec<u8>> = transaction
                    .query_row(
                        "SELECT evidence FROM import_evidence WHERE canonical = ?1",
                        [canonical],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                let evidence = evidence.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact import integrity receipt has no evidence".to_owned(),
                    )
                })?;
                import_integrity_digest(canonical, &evidence)?
            }
            _ => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact import integrity receipt has an invalid key".to_owned(),
                ));
            }
        },
        _ => {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact integrity verification selected the wrong section".to_owned(),
            ));
        }
    };
    if actual.as_str() != expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "lexical artifact derived content differs from its append receipt".to_owned(),
        ));
    }
    Ok(())
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
    let digest = ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
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
fn verify_staged_source_tail(
    connection: &Connection,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let cursor = match source.page_count().checked_sub(1) {
        Some(page_ordinal) => connection
            .query_row(
                "SELECT next_cursor FROM source_pages WHERE page_ordinal = ?1",
                [i64::try_from(page_ordinal).map_err(contract_number)?],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .as_deref()
            .map(decode_cursor)
            .transpose()?,
        None => None,
    };
    source
        .verify_completion(cursor.as_ref())
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
    Ok(())
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
        let digest = import_integrity_digest(&canonical, &canonical)?;
        transaction
            .execute(
                "INSERT INTO import_integrity(canonical, digest) VALUES (?1, ?2)",
                params![canonical, digest.as_str()],
            )
            .map_err(sqlite_error)?;
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
        let digest = document_integrity_digest(transaction, document)?;
        transaction
            .execute(
                "INSERT INTO document_integrity(document_id, digest) VALUES (?1, ?2)",
                params![document, digest.as_str()],
            )
            .map_err(sqlite_error)?;
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

fn document_integrity_digest(
    transaction: &Transaction<'_>,
    document: i64,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-derived-document.v1\0");
    hasher.update(document.to_le_bytes());
    let row_count = hash_document_table(
        transaction,
        &mut hasher,
        "row",
        "SELECT row FROM rows WHERE document_id = ?1",
        document,
    )?;
    if row_count != 1 {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
            "lexical artifact document {document} is missing its derived row"
        )));
    }
    hash_document_table(
        transaction,
        &mut hasher,
        "term_posting",
        "SELECT field, term, frequency FROM term_postings WHERE document_id = ?1 ORDER BY field, term",
        document,
    )?;
    hash_document_table(
        transaction,
        &mut hasher,
        "exact_posting",
        "SELECT field, term FROM exact_postings WHERE document_id = ?1 ORDER BY field, term",
        document,
    )?;
    hash_document_table(
        transaction,
        &mut hasher,
        "ngram_posting",
        "SELECT kind, ngram FROM ngram_postings WHERE document_id = ?1 ORDER BY kind, ngram",
        document,
    )?;
    integrity_digest(hasher)
}

fn hash_document_table(
    transaction: &Transaction<'_>,
    hasher: &mut Sha256,
    table: &str,
    query: &str,
    document: i64,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    hasher.update(
        u64::try_from(table.len())
            .map_err(contract_number)?
            .to_le_bytes(),
    );
    hasher.update(table.as_bytes());
    let mut statement = transaction.prepare(query).map_err(sqlite_error)?;
    let column_count = statement.column_count();
    let mut rows = statement.query([document]).map_err(sqlite_error)?;
    let mut count = 0u64;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        hasher.update(b"row\0");
        for column in 0..column_count {
            hash_value(hasher, row.get_ref(column).map_err(sqlite_error)?)?;
        }
        count = count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact document integrity row count overflowed".to_owned(),
            )
        })?;
    }
    hasher.update(b"end\0");
    hasher.update(count.to_le_bytes());
    Ok(count)
}

fn import_integrity_digest(
    canonical: &[u8],
    evidence: &[u8],
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-lexical-artifact-derived-import.v1\0");
    hash_bytes(&mut hasher, canonical)?;
    hash_bytes(&mut hasher, evidence)?;
    integrity_digest(hasher)
}

fn integrity_digest(hasher: Sha256) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
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
    FinalizationSectionV1::ALL
        .into_iter()
        .map(|section| digest_query(connection, section, control))
        .collect()
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
    use super::*;

    #[test]
    fn bounded_finalization_resume_seeks_each_native_section_index() {
        let connection = Connection::open_in_memory().expect("open artifact database");
        create_schema(&connection).expect("create artifact schema");
        connection
            .execute(
                "INSERT INTO source_pages(page_ordinal, page_digest, cumulative_digest, chunk_count, payload_bytes, import_count, import_payload_bytes, import_dictionary_digest, next_cursor) VALUES (0, 'page', 'cumulative', 1, 1, 1, 1, 'imports', X'00')",
                [],
            )
            .expect("seed source page");
        connection
            .execute(
                "INSERT INTO document_integrity(document_id, digest) VALUES (0, 'document')",
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
        connection
            .execute(
                "INSERT INTO ngram_postings(kind, ngram, document_id) VALUES (1, 1, 0)",
                [],
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
