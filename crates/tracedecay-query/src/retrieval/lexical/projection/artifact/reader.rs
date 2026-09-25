mod family_report;

pub use family_report::{CloneExactFamilyArtifactCandidateV1, CloneExactFamilyArtifactPageV1};

#[cfg(test)]
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, OnceLock};

use roaring::RoaringBitmap;
#[cfg(any(test, feature = "hotpath"))]
use rusqlite::StatementStatus;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params_from_iter, types::Value};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::clones::{
    CloneBodyOccurrenceV1, CloneBodyPayloadV1, CloneExactKeyV1, CloneSelectedBlockV1,
    CodeIndexCloneBodyV1,
};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
    CompactCandidate, ExactFieldV1, ExactTechnicalTermKindV1, LanguageDescriptorRevision,
    ManifestDigest, RetrieverBatch, RetrieverCoverage, RetrieverKind, RetrieverOutcome,
    SourceOccurrenceId, SourceSpan, SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_private_fs::open_private_file;

use super::builder::compute_section_digests;
use super::clone_codec::{
    CloneOccurrenceRouteV1, digest_key, routed_clone_body_row, stored_clone_occurrence,
};
use super::fingerprints::{
    CloneFingerprintArtifactReadV1, CloneFingerprintReadRequestV1,
    CloneSelectedBlockArtifactCandidateV1, CloneSelectedBlockArtifactReadV1,
    read_clone_fingerprint_page,
};
use super::format::{
    ArtifactRowV1, CodeLexicalArtifactOccurrenceV1, CodeLexicalImportMembershipWitnessV1,
    PostingListDecoderV1, VerifiedCodeLexicalArtifactV1, content_metadata_bytes,
    decode_document_set, decode_ngram_bitmap, decode_padded_receipt, decode_term_lists,
    receipt_artifact_digest, stored_metadata_digest as stored_metadata_digest_of,
    verify_artifact_table_layout,
};
use super::postings::{NGRAM_NORMALIZED, query_ngrams, raw_override_query_ngrams};
use super::row_codec::{
    ConnectionRowDictionaryV1, ROW_BLOCK_BY_DOCUMENT_SQL, RowBlocksV1, StoredRowV1,
    decode_artifact_row, stored_chunk_key, stored_symbol_key,
};
use super::schema::{
    exact_field_code, field_from_code, require_served_revision, stable_exact_term_id,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_CACHE_FLOOR_BYTES,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1, checkpoint,
    sqlite_corrupt, sqlite_error,
};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::ports::RetrievalExecutionControl;
use crate::retrieval::ports::{
    CodeCandidateBindingV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RETRIEVAL_CANDIDATE_BATCH_SIZE, RetrievalPortError, contract_error, lane_candidate_cap,
    retrieval_checkpoint,
};

use super::super::{
    ExactMatchRowViewV1, FuzzyExpansionsV1, FuzzyQueryGroupV1, LexicalFieldTextV1,
    LexicalIndexedRow, LexicalRowScoreV1, LiteralProofCacheV1, PreparedLexicalQueryV1,
    bm25_score_micros, exact_matches, field_weight_millis, fuzzy_distance_bound,
    lexical_lane_binding, lexical_lane_candidate, matches_phrase, normalize_lexical,
    score_lexical_row,
};
use crate::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    MAX_FUZZY_TERM_EXPANSIONS_V1, MAX_LEXICAL_QUERY_TERM_BYTES_V1, admit_candidate_sources,
    candidate_admission_outcome, field_admitted,
};

impl LexicalFieldTextV1 for ArtifactRowV1 {
    fn grain(&self) -> CodeSearchChunkGrainV1 {
        self.anchor.grain
    }

    fn normalized_text(&self) -> &str {
        &self.normalized_text
    }

    fn logical_path(&self) -> &str {
        &self.logical_path
    }

    fn field_lengths(&self) -> &BTreeMap<LexicalFieldV1, usize> {
        &self.field_lengths
    }

    fn symbol_simple_name(&self) -> Option<&str> {
        self.symbol_simple_name.as_deref()
    }

    fn symbol_qualified_name(&self) -> Option<&str> {
        self.symbol_qualified_name.as_deref()
    }

    fn symbol_signature(&self) -> Option<&str> {
        self.symbol_signature.as_deref()
    }

    fn symbol_documentation(&self) -> Option<&str> {
        self.symbol_documentation.as_deref()
    }
}

impl LexicalIndexedRow for ArtifactRowV1 {
    fn chunk_id(&self) -> &CodeSearchChunkId {
        &self.id
    }

    fn anchor(&self) -> &CodeSearchChunkAnchorV1 {
        &self.anchor
    }

    fn language_descriptor_revision(&self) -> &LanguageDescriptorRevision {
        &self.language_descriptor_revision
    }
}

#[derive(Clone)]
pub struct CodeLexicalArtifactReaderV1 {
    connection: Arc<ArtifactConnectionMutex<Connection>>,
    metadata: super::super::CodeLexicalProjectionMetadataV1,
    receipt: VerifiedCodeLexicalArtifactV1,
    retained_owned_bytes: usize,
    /// Fuzzy expansion walks every in-fuzzy term; share one load across
    /// clones and later queries on this reader.
    fuzzy_vocabulary: Arc<OnceLock<Arc<Vec<String>>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneExactArtifactMemberV1 {
    pub payload: CloneBodyPayloadV1,
    pub occurrence: CloneBodyOccurrenceV1,
}

pub const MAX_CLONE_EXACT_PAGE_MEMBERS_V1: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CloneArtifactCursorV1 {
    pub(super) artifact_digest: ManifestDigest,
    pub(super) generation: CodeGenerationId,
    pub(super) request_digest: ManifestDigest,
    pub(super) after: CloneArtifactCursorPositionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) enum CloneArtifactCursorPositionV1 {
    /// The ordinal of the last occurrence the page returned.
    Exact(i64),
    Fingerprint {
        body_digest: ManifestDigest,
        payload_digest: ManifestDigest,
    },
}

impl CloneArtifactCursorV1 {
    pub fn encode(&self) -> Result<String, CodeLexicalArtifactErrorV1> {
        serde_json::to_vec(self)
            .map(hex::encode)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
    }

    pub fn decode(encoded: &str) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let bytes = hex::decode(encoded)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneArtifactPageV1<T> {
    pub members: Vec<T>,
    pub next_cursor: Option<CloneArtifactCursorV1>,
}

fn clone_authority_digest(
    authority: &CloneBodyOccurrenceV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    canonical_sha256(&(
        "tracedecay.clone-exact-authority.v1",
        &authority.project_id,
        &authority.repository_id,
        &authority.worktree_id,
        &authority.source_generation,
        &authority.snapshot_digest,
    ))
    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

type ArtifactConnectionMutex<T> = StdMutex<T>;

#[derive(Clone, Copy)]
enum ReaderIntegrityAuthorityV1 {
    /// The immutable artifact was SQLite-verified before publication and both
    /// whole-file hashes still match that exact published byte identity.
    ContentAddressedPublisherProof,
    /// The caller binds only the embedded receipt, so SQLite must verify its
    /// own page structure before any rows are trusted.
    ReceiptOnly,
}

fn verify_reader_sqlite_integrity(
    connection: &Connection,
    authority: ReaderIntegrityAuthorityV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if matches!(
        authority,
        ReaderIntegrityAuthorityV1::ContentAddressedPublisherProof
    ) {
        return Ok(());
    }
    let integrity: String = hotpath::measure_block!("query.artifact.open.quick_check", {
        connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(sqlite_corrupt)
    })?;
    if integrity != "ok" {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(integrity));
    }
    Ok(())
}

impl std::fmt::Debug for CodeLexicalArtifactReaderV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeLexicalArtifactReaderV1")
            .field("generation", &self.metadata.generation)
            .field("artifact_digest", self.receipt.artifact_digest())
            .field("retained_owned_bytes", &self.retained_owned_bytes)
            .finish_non_exhaustive()
    }
}

impl CodeLexicalArtifactReaderV1 {
    /// Open a published artifact whose trust anchor is its content address:
    /// the durable head names the artifact file's size and SHA-256 digest,
    /// the embedded receipt is decoded only after the whole file matches
    /// that digest, and the standard receipt-bound verification then runs
    /// unchanged. This is the reopen path for a durable text head that
    /// survived a daemon restart.
    #[hotpath::measure(label = "query.artifact.open_content_addressed")]
    pub fn open_content_addressed(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        validate_cache_budget(cache_budget_bytes)?;
        let path = path.as_ref();
        // A durable content address names only a private, no-follow file made
        // by the artifact publisher. Keep that exact handle through both
        // digests; path metadata alone cannot bind the bytes SQLite serves.
        let mut file = open_private_file(path).map_err(map_private_artifact_file_error)?;
        let metadata = file.metadata().map_err(map_artifact_file_error)?;
        if !metadata.file_type().is_file() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact path is not a regular file".to_owned(),
            ));
        }
        let file_size = metadata.len();
        if file_size != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                "artifact file has {file_size} bytes; the durable head names {expected_file_size_bytes}"
            )));
        }
        let digest = digest_content_addressed_file(&mut file, control)?;
        if &digest != expected_file_digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact file bytes do not match the durable head digest".to_owned(),
            ));
        }
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        let connection = hotpath::measure_block!("query.artifact.open.sqlite_connect", {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| map_reader_open_error(path, error))
        })?;
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        hotpath::measure_block!("query.artifact.open.head_schema_verify", {
            configure_reader_window(&connection, cache_budget_bytes, 0, expected_file_size_bytes)?;
            connection
                .pragma_update(None, "query_only", true)
                .map_err(sqlite_error)?;
            verify_artifact_state_revision(&connection, control)?;
            verify_artifact_table_layout(&connection)
        })?;
        let receipt = hotpath::measure_block!("query.artifact.open.head_receipt_restore", {
            let receipt_bytes: Vec<u8> = connection
                .query_row(
                    "SELECT receipt FROM artifact_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(sqlite_corrupt)?;
            decode_padded_receipt(&receipt_bytes)?.ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "content-addressed lexical artifact has no finalized receipt".to_owned(),
                )
            })
        })?;
        if receipt.file_size_bytes() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt disagrees with the durable head file size".to_owned(),
            ));
        }
        let reader = hotpath::measure_block!(
            "query.artifact.open.reader_restore",
            Self::open_connection_with_control(
                connection,
                &receipt,
                authority,
                cache_budget_bytes,
                expected_file_size_bytes,
                control,
                ReaderIntegrityAuthorityV1::ContentAddressedPublisherProof,
            )
        )?;
        verify_retained_artifact_digest(&mut file, expected_file_digest, control)?;
        verify_named_path_identity(path, &file)?;
        crate::hotpath_metrics::Residency::Cold.record("query.artifact.residency");
        hotpath::gauge!("query.artifact.bytes").set(expected_file_size_bytes);
        hotpath::gauge!("query.artifact.pages").set(reader.receipt.page_count());
        Ok(reader)
    }

    #[hotpath::measure(label = "query.artifact.open")]
    pub fn open_with_control(
        path: impl AsRef<Path>,
        expected: &VerifiedCodeLexicalArtifactV1,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        validate_cache_budget(cache_budget_bytes)?;
        let path = path.as_ref();
        let metadata = path.symlink_metadata().map_err(map_artifact_file_error)?;
        if !metadata.file_type().is_file() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact path is not a regular file".to_owned(),
            ));
        }
        let file_size = metadata.len();
        if file_size != expected.file_size_bytes() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                "artifact file has {file_size} bytes; receipt binds {}",
                expected.file_size_bytes()
            )));
        }
        let connection = hotpath::measure_block!("query.artifact.open.sqlite_connect", {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| map_reader_open_error(path, error))
        })?;
        let reader = hotpath::measure_block!(
            "query.artifact.open.reader_restore",
            Self::open_connection_with_control(
                connection,
                expected,
                authority,
                cache_budget_bytes,
                expected.file_size_bytes(),
                control,
                ReaderIntegrityAuthorityV1::ReceiptOnly,
            )
        )?;
        crate::hotpath_metrics::Residency::Warm.record("query.artifact.residency");
        hotpath::gauge!("query.artifact.bytes").set(expected.file_size_bytes());
        hotpath::gauge!("query.artifact.pages").set(expected.page_count());
        Ok(reader)
    }

    /// `authority` is the opener's projection: the artifact stores only its
    /// content part, and the reader serves the opener's generation,
    /// repository, freshness, and clone route.
    fn open_connection_with_control(
        connection: Connection,
        expected: &VerifiedCodeLexicalArtifactV1,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        sealed_file_size_bytes: u64,
        control: &dyn CodeIndexExecutionControlV1,
        integrity_authority: ReaderIntegrityAuthorityV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        hotpath::measure_block!("query.artifact.open.schema_verify", {
            connection
                .pragma_update(None, "query_only", true)
                .map_err(sqlite_error)?;
            verify_artifact_state_revision(&connection, control)?;
            verify_artifact_table_layout(&connection)
        })?;
        // Read the BLOB length first so the page cache can be configured
        // before metadata is materialized. The retained metadata copy plus
        // SQLite's cache therefore cannot exceed the caller's reservation.
        let (page_cache_bytes, stored_metadata_bytes, stored_metadata_digest, metadata) = hotpath::measure_block!(
            "query.artifact.open.metadata_restore",
            {
                let stored_metadata_len: i64 = connection
                    .query_row(
                        "SELECT length(metadata) FROM artifact_state WHERE singleton = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
                let stored_metadata_len = usize::try_from(stored_metadata_len)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
                if stored_metadata_len >= cache_budget_bytes {
                    return Err(CodeLexicalArtifactErrorV1::Unreserved(
                        "lexical artifact metadata exhausts the reader cache budget".to_owned(),
                    ));
                }
                // Kernel SQLite window: page cache clamped to [2, 64] MiB.
                // Sealed readers also mmap the immutable file (file-backed,
                // not part of this heap claim) so n-gram serving does not
                // re-pread the same posting pages on every tool call.
                let sqlite_budget = cache_budget_bytes - stored_metadata_len;
                if sqlite_budget < ARTIFACT_SQLITE_CACHE_FLOOR_BYTES {
                    return Err(CodeLexicalArtifactErrorV1::Unreserved(format!(
                        "lexical artifact reader budget leaves {sqlite_budget} bytes, under the {ARTIFACT_SQLITE_CACHE_FLOOR_BYTES}-byte kernel page-cache floor"
                    )));
                }
                let page_cache_bytes = configure_reader_window(
                    &connection,
                    cache_budget_bytes,
                    stored_metadata_len,
                    sealed_file_size_bytes,
                )?;
                let (stored_metadata_bytes, stored_metadata_digest): (Vec<u8>, String) = connection
                    .query_row(
                        "SELECT metadata, metadata_digest FROM artifact_state WHERE singleton = 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
                if stored_metadata_bytes.len() != stored_metadata_len {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact metadata changed while opening its sealed reader"
                            .to_owned(),
                    ));
                }
                if stored_metadata_bytes != content_metadata_bytes(authority)? {
                    return Err(CodeLexicalArtifactErrorV1::Incompatible(
                        "lexical artifact content does not match the opening projection".to_owned(),
                    ));
                }
                let metadata = authority.clone();
                Ok::<_, CodeLexicalArtifactErrorV1>((
                    page_cache_bytes,
                    stored_metadata_bytes,
                    stored_metadata_digest,
                    metadata,
                ))
            }
        )?;
        // Content-addressed reopen has stronger authority than `quick_check`:
        // the builder ran SQLite integrity verification before publication,
        // and this reader hashes the exact immutable file both before and
        // after opening it. Repeating a corpus-wide SQLite scan added tens of
        // seconds without authenticating any bytes the two hashes did not.
        verify_reader_sqlite_integrity(&connection, integrity_authority)?;
        checkpoint(control)?;
        let stored = hotpath::measure_block!("query.artifact.open.receipt_restore", {
            let receipt_bytes: Vec<u8> = connection
                .query_row(
                    "SELECT receipt FROM artifact_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            decode_padded_receipt(&receipt_bytes)?.ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact has no finalized receipt".to_owned(),
                )
            })
        })?;
        if stored != *expected {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact receipt does not match its verified seat".to_owned(),
            ));
        }
        require_served_revision(stored.format_revision())?;
        verify_artifact_state_revision(&connection, control)?;
        if &stored_metadata_digest_of(&stored_metadata_bytes)? != stored.metadata_digest()
            || stored_metadata_digest != stored.metadata_digest().as_str()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact metadata digest does not verify".to_owned(),
            ));
        }
        let sections = hotpath::measure_block!(
            "query.artifact.open.section_digest_verify",
            compute_section_digests(&connection, control)
        )?;
        if sections != stored.section_digests() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact section digests do not verify".to_owned(),
            ));
        }
        let digest = hotpath::measure_block!(
            "query.artifact.open.artifact_digest_verify",
            receipt_artifact_digest(&stored, &sections)
        )?;
        if &digest != stored.artifact_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact content digest does not verify".to_owned(),
            ));
        }
        checkpoint(control)?;
        let retained_owned_bytes = stored_metadata_bytes.len().saturating_add(page_cache_bytes);
        Ok(Self {
            // Every clone shares one rusqlite handle. Readers are replaced on
            // remount, while Hotpath 0.24 retains every instrumented mutex
            // identity for the process lifetime, so this per-reader lock must
            // remain plain. Static query spans retain operation visibility.
            connection: Arc::new(StdMutex::new(connection)),
            metadata,
            receipt: stored,
            retained_owned_bytes,
            fuzzy_vocabulary: Arc::new(OnceLock::new()),
        })
    }

    #[hotpath::skip]
    pub fn metadata(&self) -> &super::super::CodeLexicalProjectionMetadataV1 {
        &self.metadata
    }

    #[hotpath::skip]
    pub fn verified_artifact(&self) -> &VerifiedCodeLexicalArtifactV1 {
        &self.receipt
    }

    #[hotpath::skip]
    pub fn retained_owned_bytes(&self) -> usize {
        self.retained_owned_bytes
    }

    pub fn occurrence_by_chunk(
        &self,
        chunk: &CodeSearchChunkId,
    ) -> Result<Option<CodeLexicalArtifactOccurrenceV1>, CodeLexicalArtifactErrorV1> {
        let connection = self.lock_connection()?;
        let document: Option<i64> = connection
            .query_row(
                "SELECT document_id FROM row_chunks WHERE chunk_id = ?1",
                [stored_chunk_key(chunk.as_str())],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some(document) = document else {
            return Ok(None);
        };
        let stored = RowBlocksV1::new(&connection).row(
            u32::try_from(document)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?,
        )?;
        if stored.chunk_id != chunk.as_str() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact chunk lookup names another document's row".to_owned(),
            ));
        }
        decode_artifact_row(
            &self.metadata.generation,
            &stored.chunk_id,
            &stored.row,
            &stored.text,
            &ConnectionRowDictionaryV1::new(&connection),
        )
        .map(row_occurrence)
        .map(Some)
    }

    pub fn occurrence_by_binding(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<Option<CodeLexicalArtifactOccurrenceV1>, CodeLexicalArtifactErrorV1> {
        if binding.occurrence.generation != self.metadata.generation {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "candidate binding belongs to another generation".to_owned(),
            ));
        }
        let chunk = binding.occurrence.chunk.as_ref().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical candidate binding has no chunk identity".to_owned(),
            )
        })?;
        let occurrence = self.occurrence_by_chunk(chunk)?;
        if occurrence.as_ref().is_some_and(|occurrence| {
            occurrence.file != binding.occurrence.file
                || occurrence.symbol != binding.occurrence.symbol
        }) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "candidate binding disagrees with lexical artifact row".to_owned(),
            ));
        }
        Ok(occurrence)
    }

    pub fn import_membership(
        &self,
        evidence: &CodeIndexImportEvidenceV1,
    ) -> Result<Option<CodeLexicalImportMembershipWitnessV1>, CodeLexicalArtifactErrorV1> {
        let canonical = serde_json::to_vec(evidence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let connection = self.lock_connection()?;
        let stored: Option<Vec<u8>> = connection
            .query_row(
                "SELECT canonical FROM import_evidence WHERE canonical = ?1",
                [canonical],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let stored: CodeIndexImportEvidenceV1 = serde_json::from_slice(&stored)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if &stored != evidence {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "import dictionary key does not match its evidence".to_owned(),
            ));
        }
        Ok(Some(CodeLexicalImportMembershipWitnessV1 {
            artifact_digest: self.receipt.artifact_digest().clone(),
            import_dictionary_digest: self.receipt.import_dictionary_digest().clone(),
            evidence: stored,
        }))
    }

    pub fn exact_adapter<A>(&self, authority: A) -> CodeExactLexicalArtifactReaderV1<A>
    where
        A: ExactAdmissionAuthority,
    {
        CodeExactLexicalArtifactReaderV1 {
            reader: self.clone(),
            authority,
        }
    }

    pub fn clone_exact_page(
        &self,
        authority: &CloneBodyOccurrenceV1,
        key: &CloneExactKeyV1,
        cursor: Option<&CloneArtifactCursorV1>,
        limit: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CloneArtifactPageV1<CloneExactArtifactMemberV1>, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        let route = self.validate_clone_lookup_authority(authority)?;
        if limit == 0 || limit > MAX_CLONE_EXACT_PAGE_MEMBERS_V1 {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "clone exact page limit must be within 1..={MAX_CLONE_EXACT_PAGE_MEMBERS_V1}"
            )));
        }
        let authority_digest = clone_authority_digest(authority)?;
        let request_digest = canonical_sha256(&(
            "tracedecay.clone-exact-request.v1",
            self.receipt.artifact_digest(),
            &authority_digest,
            key,
        ))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let after = self.clone_exact_after(cursor, &request_digest)?;
        let fetch = limit.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract("clone exact page limit overflowed".to_owned())
        })?;
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare_cached(
                "SELECT posting.occurrence_ordinal, \
                 occurrence.symbol_key, payload.payload_digest, occurrence.path, \
                 occurrence.body_start, occurrence.body_end, occurrence.eligibility, payload.payload \
                 FROM clone_exact_postings AS posting \
                 LEFT JOIN clone_occurrences AS occurrence ON occurrence.ordinal = posting.occurrence_ordinal \
                 LEFT JOIN clone_body_payloads AS payload ON payload.ordinal = occurrence.payload_ordinal \
                 WHERE posting.class = ?1 AND posting.normalization_revision = ?2 AND posting.digest = ?3 \
                 AND posting.occurrence_ordinal > ?5 \
                 AND (occurrence.symbol_key IS NULL OR occurrence.symbol_key != ?4) \
                 ORDER BY posting.occurrence_ordinal LIMIT ?6",
            )
            .map_err(sqlite_error)?;
        let mut rows = statement
            .query(rusqlite::params![
                i64::from(key.class as u8),
                i64::from(key.normalization_revision),
                digest_key(&key.digest)?.as_slice(),
                stored_symbol_key(authority.symbol_occurrence_id.as_str()),
                after,
                i64::try_from(fetch)
                    .map_err(|error| { CodeLexicalArtifactErrorV1::Contract(error.to_string()) })?,
            ])
            .map_err(sqlite_error)?;
        let mut members = Vec::with_capacity(fetch);
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            if members.len().is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                checkpoint(control)?;
            }
            members.push(Self::verified_clone_member(&route, key, row)?);
        }
        checkpoint(control)?;
        let next_cursor = (members.len() > limit)
            .then(|| {
                members
                    .get(limit - 1)
                    .map(|(ordinal, _)| CloneArtifactCursorV1 {
                        artifact_digest: self.receipt.artifact_digest().clone(),
                        generation: self.metadata.generation.clone(),
                        request_digest,
                        after: CloneArtifactCursorPositionV1::Exact(*ordinal),
                    })
            })
            .flatten();
        members.truncate(limit);
        Ok(CloneArtifactPageV1 {
            members: members.into_iter().map(|(_, member)| member).collect(),
            next_cursor,
        })
    }

    pub fn clone_fingerprint_page(
        &self,
        authority: &CloneBodyOccurrenceV1,
        source: &CloneBodyPayloadV1,
        cursor: Option<&CloneArtifactCursorV1>,
        limit: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CloneFingerprintArtifactReadV1, CodeLexicalArtifactErrorV1> {
        let route = self.validate_clone_lookup_authority(authority)?;
        let authority_digest = clone_authority_digest(authority)?;
        let connection = self.lock_connection()?;
        read_clone_fingerprint_page(
            &connection,
            CloneFingerprintReadRequestV1 {
                receipt: &self.receipt,
                route: &route,
                authority_digest: &authority_digest,
                authority,
                source,
                selected_block: None,
                cursor,
                limit,
                control,
            },
        )
    }

    pub fn clone_body(
        &self,
        symbol: &SymbolOccurrenceId,
    ) -> Result<Option<CodeIndexCloneBodyV1>, CodeLexicalArtifactErrorV1> {
        let route = self.clone_route()?;
        let connection = self.lock_connection()?;
        let row = connection
            .query_row(
                "SELECT occurrence.symbol_key, payload.payload_digest, occurrence.path, \
                 occurrence.body_start, occurrence.body_end, occurrence.eligibility, payload.payload \
                 FROM clone_occurrences AS occurrence \
                 LEFT JOIN clone_body_payloads AS payload \
                 ON payload.ordinal = occurrence.payload_ordinal \
                 WHERE occurrence.symbol_key = ?1",
                [stored_symbol_key(symbol.as_str())],
                routed_clone_body_row,
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let body = route.clone_body(row)?;
        if body.occurrence.symbol_occurrence_id != *symbol {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone body lookup failed canonical validation".to_owned(),
            ));
        }
        Ok(Some(body))
    }

    pub fn clone_body_by_source_range(
        &self,
        path: &str,
        span: SourceSpan,
    ) -> Result<Option<CodeIndexCloneBodyV1>, CodeLexicalArtifactErrorV1> {
        span.validate()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let route = self.clone_route()?;
        let connection = self.lock_connection()?;
        let row = connection
            .query_row(
                "SELECT occurrence.symbol_key, payload.payload_digest, occurrence.path, \
                 occurrence.body_start, occurrence.body_end, occurrence.eligibility, payload.payload \
                 FROM clone_occurrences AS occurrence \
                 LEFT JOIN clone_body_payloads AS payload \
                 ON payload.ordinal = occurrence.payload_ordinal \
                 WHERE occurrence.path = ?1 AND occurrence.body_start <= ?2 \
                 AND occurrence.body_end >= ?3 \
                 ORDER BY occurrence.body_end - occurrence.body_start, \
                 occurrence.symbol_key \
                 LIMIT 1",
                rusqlite::params![
                    path,
                    i64::try_from(span.start_byte).map_err(|error| {
                        CodeLexicalArtifactErrorV1::Contract(error.to_string())
                    })?,
                    i64::try_from(span.end_byte).map_err(|error| {
                        CodeLexicalArtifactErrorV1::Contract(error.to_string())
                    })?,
                ],
                routed_clone_body_row,
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let body = route.clone_body(row)?;
        if body.occurrence.path != path
            || body.occurrence.body_span.start_byte > span.start_byte
            || body.occurrence.body_span.end_byte < span.end_byte
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "source range lookup disagrees with its clone occurrence".to_owned(),
            ));
        }
        Ok(Some(body))
    }

    /// The route clone occurrences are served under: the opener's project,
    /// repository, worktree, generation, and snapshot.
    fn clone_route(&self) -> Result<CloneOccurrenceRouteV1, CodeLexicalArtifactErrorV1> {
        match (&self.metadata.clone_route, &self.metadata.repository_id) {
            (Some(route), Some(repository_id)) => Ok(CloneOccurrenceRouteV1 {
                project_id: route.project_id.clone(),
                repository_id: repository_id.clone(),
                worktree_id: route.worktree_id.clone(),
                source_generation: self.metadata.generation.clone(),
                snapshot_digest: route.snapshot_digest.clone(),
            }),
            _ => Err(CodeLexicalArtifactErrorV1::Missing(
                "lexical artifact opener carries no clone route authority".to_owned(),
            )),
        }
    }

    fn validate_clone_lookup_authority(
        &self,
        authority: &CloneBodyOccurrenceV1,
    ) -> Result<CloneOccurrenceRouteV1, CodeLexicalArtifactErrorV1> {
        let route = self.clone_route()?;
        if !route.owns(authority) {
            return Err(CodeLexicalArtifactErrorV1::Missing(
                "clone lookup route authority is unavailable".to_owned(),
            ));
        }
        if route.source_generation != authority.source_generation {
            return Err(CodeLexicalArtifactErrorV1::Missing(format!(
                "clone lookup generation {} is stale; the artifact serves {}",
                authority.source_generation.as_str(),
                route.source_generation.as_str()
            )));
        }
        Ok(route)
    }

    pub fn clone_selected_block_page(
        &self,
        authority: &CloneBodyOccurrenceV1,
        source: &CloneBodyPayloadV1,
        selected_block: &CloneSelectedBlockV1,
        cursor: Option<&CloneArtifactCursorV1>,
        limit: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CloneSelectedBlockArtifactReadV1, CodeLexicalArtifactErrorV1> {
        let route = self.validate_clone_lookup_authority(authority)?;
        let authority_digest = clone_authority_digest(authority)?;
        let connection = self.lock_connection()?;
        let read = read_clone_fingerprint_page(
            &connection,
            CloneFingerprintReadRequestV1 {
                receipt: &self.receipt,
                route: &route,
                authority_digest: &authority_digest,
                authority,
                source,
                selected_block: Some(selected_block),
                cursor,
                limit,
                control,
            },
        )?;
        let stream = read.stream.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "selected clone block has no fingerprint stream".to_owned(),
            )
        })?;
        let members = read
            .page
            .members
            .into_iter()
            .map(|candidate| {
                let containment = candidate.selected_block_containment.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "selected clone block candidate has no containment class".to_owned(),
                    )
                })?;
                Ok(CloneSelectedBlockArtifactCandidateV1 {
                    payload: candidate.payload,
                    occurrences: candidate.occurrences,
                    anchors: candidate.ordered_anchors,
                    containment,
                })
            })
            .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()?;
        Ok(CloneSelectedBlockArtifactReadV1 {
            page: CloneArtifactPageV1 {
                members,
                next_cursor: read.page.next_cursor,
            },
            stream,
            coverage: read.coverage,
            partial_reasons: read.partial_reasons,
            accounting: read.accounting,
        })
    }

    fn clone_exact_after(
        &self,
        cursor: Option<&CloneArtifactCursorV1>,
        request_digest: &ManifestDigest,
    ) -> Result<i64, CodeLexicalArtifactErrorV1> {
        match cursor {
            Some(cursor)
                if cursor.artifact_digest == *self.receipt.artifact_digest()
                    && cursor.generation == self.metadata.generation
                    && cursor.request_digest == *request_digest =>
            {
                match &cursor.after {
                    CloneArtifactCursorPositionV1::Exact(ordinal) => Ok(*ordinal),
                    CloneArtifactCursorPositionV1::Fingerprint { .. } => {
                        Err(CodeLexicalArtifactErrorV1::Contract(
                            "clone cursor position does not match an exact read".to_owned(),
                        ))
                    }
                }
            }
            Some(_) => Err(CodeLexicalArtifactErrorV1::Contract(
                "clone exact cursor does not match its artifact, key, or authority".to_owned(),
            )),
            None => Ok(0),
        }
    }

    fn verified_clone_member(
        route: &CloneOccurrenceRouteV1,
        key: &CloneExactKeyV1,
        row: &rusqlite::Row<'_>,
    ) -> Result<(i64, CloneExactArtifactMemberV1), CodeLexicalArtifactErrorV1> {
        let ordinal: i64 = row.get(0).map_err(sqlite_error)?;
        let Some(stored) = stored_clone_occurrence(row, 1)? else {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone exact posting is missing its occurrence".to_owned(),
            ));
        };
        let payload_bytes: Option<Vec<u8>> = row.get(7).map_err(sqlite_error)?;
        let (occurrence, payload) = route.occurrence_and_payload((stored, payload_bytes))?;
        if !payload
            .exact_keys(occurrence.eligibility)
            .iter()
            .any(|candidate| candidate == key)
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone exact posting does not match its payload and occurrence".to_owned(),
            ));
        }
        Ok((
            ordinal,
            CloneExactArtifactMemberV1 {
                payload,
                occurrence,
            },
        ))
    }

    /// Reader queries serialize on this one connection; the wait span makes
    /// cross-query contention (concurrent searches, hydration reads during
    /// staging) attributable instead of vanishing into lane wall time.
    fn lock_connection(&self) -> Result<StdMutexGuard<'_, Connection>, CodeLexicalArtifactErrorV1> {
        hotpath::measure_block!("query.artifact.reader.lock_wait", {
            self.connection.lock().map_err(|_| {
                CodeLexicalArtifactErrorV1::Io(
                    "lexical artifact reader lock is poisoned".to_owned(),
                )
            })
        })
    }

    fn validate_generation(&self, generation: &CodeGenerationId) -> Result<(), RetrievalPortError> {
        if generation != &self.metadata.generation {
            Err(RetrievalPortError::GenerationMismatch)
        } else {
            Ok(())
        }
    }
}

impl LexicalPostingReadPort for CodeLexicalArtifactReaderV1 {
    #[hotpath::measure(label = "query.lane.lexical.read")]
    fn read_lexical_postings(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        self.validate_generation(&request.generation)?;
        if self.metadata.freshness.compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::hotpath_metrics::Residency::Rebuilding.record("query.lane.lexical.residency");
            return Ok(RetrieverOutcome::Stale(self.metadata.freshness.clone()));
        }
        let connection = self.lock_connection().map_err(map_query_artifact_error)?;
        let outcome = ArtifactQueryV1::new(
            &connection,
            &self.metadata,
            &self.receipt,
            &self.fuzzy_vocabulary,
        )?
        .lexical_batch(request)?;
        crate::hotpath_metrics::record_lane(
            "query.lane.lexical.candidates",
            "query.lane.lexical.examined",
            "query.lane.lexical.results",
            "query.lane.lexical.residency",
            &outcome,
        );
        Ok(outcome)
    }
}

#[derive(Clone, Debug)]
pub struct CodeExactLexicalArtifactReaderV1<A> {
    reader: CodeLexicalArtifactReaderV1,
    authority: A,
}

impl<A> ExactTermPostingReadPort for CodeExactLexicalArtifactReaderV1<A>
where
    A: ExactAdmissionAuthority,
{
    #[hotpath::measure(label = "query.lane.exact.read")]
    fn read_exact_postings(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.reader.validate_generation(&request.generation)?;
        if self.reader.metadata.freshness.compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::hotpath_metrics::Residency::Rebuilding.record("query.lane.exact.residency");
            return Ok(RetrieverOutcome::Stale(
                self.reader.metadata.freshness.clone(),
            ));
        }
        let connection = self
            .reader
            .lock_connection()
            .map_err(map_query_artifact_error)?;
        let outcome = ArtifactQueryV1::new(
            &connection,
            &self.reader.metadata,
            &self.reader.receipt,
            &self.reader.fuzzy_vocabulary,
        )?
        .exact_batch(request, &self.authority)?;
        crate::hotpath_metrics::record_lane(
            "query.lane.exact.candidates",
            "query.lane.exact.examined",
            "query.lane.exact.results",
            "query.lane.exact.residency",
            &outcome,
        );
        Ok(outcome)
    }
}

struct ArtifactQueryV1<'a> {
    connection: &'a Connection,
    metadata: &'a super::super::CodeLexicalProjectionMetadataV1,
    document_count: usize,
    metrics: ArtifactQueryMetricsV1,
    fuzzy_vocabulary: &'a OnceLock<Arc<Vec<String>>>,
    /// Row dictionary entries resolved during this query.
    row_dictionary: ConnectionRowDictionaryV1<'a>,
    /// The row block this query decoded last.
    row_blocks: RowBlocksV1<'a>,
}

#[derive(Default)]
struct ArtifactQueryMetricsV1 {
    #[cfg(test)]
    probes: Cell<u64>,
    #[cfg(test)]
    fullscan_steps: Cell<u64>,
    #[cfg(test)]
    ngram_decoded_lists: Cell<u64>,
    #[cfg(test)]
    ngram_peak_candidates: Cell<u64>,
}

impl ArtifactQueryMetricsV1 {
    #[inline(always)]
    fn probe(&self) {
        #[cfg(test)]
        self.probes.set(self.probes.get().saturating_add(1));
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.sql.probes_total").inc(1u64);
    }

    #[inline(always)]
    fn rows(&self, rows: u64) {
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.sql.rows_total").inc(rows);
        #[cfg(not(feature = "hotpath"))]
        let _ = rows;
    }

    #[inline(always)]
    fn observe_statement(
        &self,
        statement: &rusqlite::Statement<'_>,
    ) -> Result<(), RetrievalPortError> {
        #[cfg(any(test, feature = "hotpath"))]
        let steps = u64::try_from(statement.get_status(StatementStatus::FullscanStep))
            .map_err(contract_error)?;
        #[cfg(test)]
        self.fullscan_steps
            .set(self.fullscan_steps.get().saturating_add(steps));
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.sql.observed_fullscan_steps_total").inc(steps);
        #[cfg(not(any(test, feature = "hotpath")))]
        let _ = statement;
        Ok(())
    }

    #[cfg(test)]
    fn probes(&self) -> u64 {
        self.probes.get()
    }

    #[cfg(test)]
    fn observed_fullscan_steps(&self) -> u64 {
        self.fullscan_steps.get()
    }

    #[cfg(test)]
    fn observe_ngram_list(&self) {
        self.ngram_decoded_lists
            .set(self.ngram_decoded_lists.get().saturating_add(1));
    }

    #[cfg(test)]
    fn observe_ngram_candidates(&self, candidates: u64) {
        self.ngram_peak_candidates
            .set(self.ngram_peak_candidates.get().max(candidates));
    }
}

/// Visit a candidate set in ascending document order. The set holds at most
/// one bit per artifact document however many sources contributed; request
/// authority is consulted before each candidate batch and at completion.
fn visit_document_ids(
    documents: &RoaringBitmap,
    control: &dyn RetrievalExecutionControl,
    mut visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    hotpath::measure_block!("query.stream.visit_documents", {
        for (visited, document) in documents.iter().enumerate() {
            if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(control)?;
            }
            visitor(document)?;
        }
        retrieval_checkpoint(control)?;
        hotpath::gauge!("query.stream.rows_total").inc(documents.len());
        Ok(())
    })
}

/// Stream each candidate row with every request-term frequency it carries.
/// The request's posting lists advance once in document order alongside the
/// ascending candidates, so a row costs at most one keyed block read and no
/// posting probe.
fn visit_lexical_rows(
    connection: &Connection,
    rows: &RowBlocksV1<'_>,
    documents: &RoaringBitmap,
    postings: &RequestTermPostingsV1,
    metrics: &ArtifactQueryMetricsV1,
    control: &dyn RetrievalExecutionControl,
    mut visitor: impl FnMut(
        u32,
        StoredRowV1,
        LexicalTermFrequenciesV1,
    ) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    hotpath::measure_block!("query.stream.visit_lexical_rows", {
        let mut cursors = postings.cursors().map_err(map_query_artifact_error)?;
        metrics.probe();
        let mut visited = 0u64;
        for document in documents {
            if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE as u64) {
                retrieval_checkpoint(control)?;
            }
            let stored = rows.row(document).map_err(map_query_artifact_error)?;
            let mut entries = Vec::new();
            for cursor in &mut cursors {
                if let Some(frequency) = cursor
                    .frequency_at(document)
                    .map_err(map_query_artifact_error)?
                {
                    entries.push((
                        cursor.field,
                        cursor.term.to_owned(),
                        usize::try_from(frequency).map_err(contract_error)?,
                    ));
                }
            }
            visitor(document, stored, LexicalTermFrequenciesV1(entries))?;
            visited = visited.saturating_add(1);
        }
        retrieval_checkpoint(control)?;
        if !documents.is_empty() {
            let statement = connection
                .prepare_cached(ROW_BLOCK_BY_DOCUMENT_SQL)
                .map_err(map_query_sql_error)?;
            metrics.observe_statement(&statement)?;
        }
        metrics.rows(visited);
        Ok(())
    })
}

/// Every sealed `(term, field)` posting list of one request's terms, read
/// once: scoring statistics, candidate sources, and per-row frequencies all
/// come from these lists.
#[derive(Default)]
struct RequestTermPostingsV1 {
    by_term: BTreeMap<String, Vec<TermFieldPostingsV1>>,
}

struct TermFieldPostingsV1 {
    field: LexicalFieldV1,
    postings: Vec<u8>,
}

impl RequestTermPostingsV1 {
    fn cursors(&self) -> Result<Vec<PostingCursorV1<'_>>, CodeLexicalArtifactErrorV1> {
        self.by_term
            .iter()
            .flat_map(|(term, lists)| lists.iter().map(move |list| (term, list)))
            .map(|(term, list)| PostingCursorV1::new(term, list))
            .collect()
    }

    /// Documents holding `term` in any field `admit` accepts.
    fn documents(
        &self,
        term: &str,
        admit: impl Fn(LexicalFieldV1) -> bool,
    ) -> Result<RoaringBitmap, CodeLexicalArtifactErrorV1> {
        let mut documents = RoaringBitmap::new();
        for list in self.by_term.get(term).into_iter().flatten() {
            if !admit(list.field) {
                continue;
            }
            for posting in PostingListDecoderV1::new(&list.postings, true) {
                documents.insert(posting?.0);
            }
        }
        Ok(documents)
    }
}

struct PostingCursorV1<'a> {
    field: LexicalFieldV1,
    term: &'a str,
    decoder: PostingListDecoderV1<'a>,
    current: Option<(u32, u32)>,
}

impl<'a> PostingCursorV1<'a> {
    fn new(
        term: &'a str,
        list: &'a TermFieldPostingsV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let mut decoder = PostingListDecoderV1::new(&list.postings, true);
        let current = decoder.next().transpose()?;
        Ok(Self {
            field: list.field,
            term,
            decoder,
            current,
        })
    }

    /// Frequency of this list's term at `document`; callers ask in strictly
    /// ascending document order.
    fn frequency_at(&mut self, document: u32) -> Result<Option<u32>, CodeLexicalArtifactErrorV1> {
        while let Some((current, frequency)) = self.current {
            if current >= document {
                return Ok((current == document).then_some(frequency));
            }
            self.current = self.decoder.next().transpose()?;
        }
        Ok(None)
    }
}

const ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1: usize = 16;

/// SQLite distributions are required to support at least 999 variables. Keep
/// generated statements within that portable ceiling instead of depending on
/// the larger build-time limit of a particular linked SQLite library.
const ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1: usize = 999;
/// Caps request-projected text/blob input, and therefore the largest
/// request-relevant frequency aggregate SQLite can return as one row. The
/// lexical row stream checks the request control between rows, never inside
/// one SQLite call, so each individual call stays deterministically bounded.
const ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1: usize =
    ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 * MAX_LEXICAL_QUERY_TERM_BYTES_V1;
/// One encoded n-gram list is retained only while it is decoded and
/// intersected. Keep that transient allocation below one eighth of the cache.
const ARTIFACT_NGRAM_MAX_ENCODED_LIST_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 8;
/// The synchronous query may inspect at most one quarter of the reader cache
/// in encoded list bytes across all selected n-grams.
const ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
/// A sparse Roaring candidate can require containers and two-byte values in
/// addition to its identifiers. Eight bytes per admitted identifier is a
/// conservative authority that bounds the first (rarest) full list.
const ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
const ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1: usize = 8;
const ARTIFACT_NGRAM_MAX_CANDIDATES_V1: u64 = (ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1
    / ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1)
    as u64;
/// One request's term posting lists are held encoded for the whole row
/// stream; bound them by the same quarter of the reader cache.
const ARTIFACT_TERM_POSTING_QUERY_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
fn ensure_sqlite_bind_capacity(
    fixed_parameters: usize,
    dynamic_parameters: usize,
) -> Result<(), RetrievalPortError> {
    let parameters = fixed_parameters
        .checked_add(dynamic_parameters)
        .ok_or(RetrievalPortError::BudgetExceeded)?;
    if parameters > ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(())
}

fn ensure_sqlite_bound_value_bytes<'a>(
    maximum_bytes: usize,
    fixed_parameters: &[Value],
    dynamic_text: impl IntoIterator<Item = &'a str>,
) -> Result<(), RetrievalPortError> {
    let fixed_bytes = fixed_parameters.iter().try_fold(0usize, |bytes, value| {
        let value_bytes = match value {
            Value::Text(value) => value.len(),
            Value::Blob(value) => value.len(),
            Value::Null | Value::Integer(_) | Value::Real(_) => 0,
        };
        bytes
            .checked_add(value_bytes)
            .ok_or(RetrievalPortError::BudgetExceeded)
    })?;
    let total_bytes = dynamic_text
        .into_iter()
        .try_fold(fixed_bytes, |bytes, value| {
            bytes
                .checked_add(value.len())
                .ok_or(RetrievalPortError::BudgetExceeded)
        })?;
    if total_bytes > maximum_bytes {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(())
}

/// The rarest fixed number of distinct n-grams forms a selective, bounded
/// prefilter. It may admit a superset for a very long phrase; the row-level
/// substring check remains the correctness authority before scoring.
fn ngram_document_query(
    connection: &Connection,
    kind: i64,
    bytes: &[u8],
    metrics: &ArtifactQueryMetricsV1,
) -> Result<RoaringBitmap, RetrievalPortError> {
    hotpath::measure_block!("query.artifact.ngram.bitmap_query", {
        let ngrams = query_ngrams(bytes)
            .into_iter()
            .map(|ngram| (kind, ngram))
            .collect::<Vec<_>>();
        if ngrams.is_empty() {
            return Ok(RoaringBitmap::new());
        }
        let candidates = ngram_bitmap_candidates(connection, &ngrams, metrics)?;
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.ngram.query_candidates_total").inc(candidates.len());
        Ok(candidates)
    })
}

#[derive(Clone, Copy)]
struct NgramSelectivityV1 {
    kind: i64,
    ngram: u32,
    cardinality: u64,
}

/// One query's n-gram budget: the encoded-byte allowance every intersection
/// charges against, plus (under `hotpath`) the totals it consumed.
struct NgramListBudgetV1 {
    remaining_encoded_bytes: usize,
    #[cfg(feature = "hotpath")]
    observed_lists: u64,
    #[cfg(feature = "hotpath")]
    observed_bytes: u64,
}

impl NgramListBudgetV1 {
    fn for_query() -> Self {
        Self {
            remaining_encoded_bytes: ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1,
            #[cfg(feature = "hotpath")]
            observed_lists: 0,
            #[cfg(feature = "hotpath")]
            observed_bytes: 0,
        }
    }

    #[inline(always)]
    fn observe_list(&mut self, encoded_bytes: usize) {
        #[cfg(feature = "hotpath")]
        {
            self.observed_lists = self.observed_lists.saturating_add(1);
            self.observed_bytes = self.observed_bytes.saturating_add(encoded_bytes as u64);
        }
        #[cfg(not(feature = "hotpath"))]
        let _ = encoded_bytes;
    }

    #[inline(always)]
    fn report(&self) {
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("query.artifact.ngram.query_lists_total").inc(self.observed_lists);
            hotpath::gauge!("query.artifact.ngram.query_bytes_total").inc(self.observed_bytes);
        }
    }
}

fn ngram_bitmap_candidates(
    connection: &Connection,
    ngrams: &[(i64, u32)],
    _metrics: &ArtifactQueryMetricsV1,
) -> Result<RoaringBitmap, RetrievalPortError> {
    let mut budget = NgramListBudgetV1::for_query();
    let mut selectivities = Vec::with_capacity(ngrams.len());
    let mut selectivity_statement = connection
        .prepare_cached(
            "SELECT document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(map_query_sql_error)?;
    for &(kind, ngram) in ngrams {
        let cardinality = selectivity_statement
            .query_row([kind, i64::from(ngram)], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(map_query_sql_error)?;
        let Some(cardinality) = cardinality else {
            return Ok(RoaringBitmap::new());
        };
        let cardinality = u64::try_from(cardinality).map_err(contract_error)?;
        ensure_ngram_candidate_cardinality(cardinality)?;
        selectivities.push(NgramSelectivityV1 {
            kind,
            ngram,
            cardinality,
        });
    }
    drop(selectivity_statement);
    selectivities.sort_unstable_by_key(|selectivity| {
        (selectivity.cardinality, selectivity.kind, selectivity.ngram)
    });
    selectivities.truncate(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1);

    let mut candidates = None::<RoaringBitmap>;
    let mut list_statement = connection
        .prepare_cached(
            "SELECT documents, document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(map_query_sql_error)?;
    for selectivity in selectivities {
        let (encoded, cardinality): (Vec<u8>, i64) = list_statement
            .query_row([selectivity.kind, i64::from(selectivity.ngram)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(map_query_sql_error)?;
        charge_ngram_encoded_list_bytes(
            &mut budget.remaining_encoded_bytes,
            encoded.len(),
            ARTIFACT_NGRAM_MAX_ENCODED_LIST_BYTES_V1,
        )?;
        let mut list = decode_document_set(&encoded).map_err(map_query_artifact_error)?;
        if i64::try_from(list.len()).map_err(contract_error)? != cardinality {
            return Err(RetrievalPortError::Contract(
                "lexical artifact ngram list cardinality changed after verification".to_owned(),
            ));
        }
        if let Some(current) = candidates.as_ref() {
            list &= current;
        }
        ensure_ngram_candidate_cardinality(list.len())?;
        #[cfg(test)]
        {
            _metrics.observe_ngram_list();
            _metrics.observe_ngram_candidates(list.len());
        }
        budget.observe_list(encoded.len());
        let exhausted = list.is_empty();
        candidates = Some(list);
        if exhausted {
            break;
        }
    }
    budget.report();
    Ok(candidates.unwrap_or_default())
}

fn ensure_ngram_candidate_cardinality(cardinality: u64) -> Result<(), RetrievalPortError> {
    if cardinality > ARTIFACT_NGRAM_MAX_CANDIDATES_V1 {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

fn charge_ngram_encoded_list_bytes(
    remaining_bytes: &mut usize,
    list_bytes: usize,
    maximum_list_bytes: usize,
) -> Result<(), RetrievalPortError> {
    if list_bytes > maximum_list_bytes {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    *remaining_bytes = remaining_bytes
        .checked_sub(list_bytes)
        .ok_or(RetrievalPortError::BudgetExceeded)?;
    Ok(())
}

impl<'a> ArtifactQueryV1<'a> {
    fn new(
        connection: &'a Connection,
        metadata: &'a super::super::CodeLexicalProjectionMetadataV1,
        receipt: &'a VerifiedCodeLexicalArtifactV1,
        fuzzy_vocabulary: &'a OnceLock<Arc<Vec<String>>>,
    ) -> Result<Self, RetrievalPortError> {
        Ok(Self {
            connection,
            metadata,
            // Admitted documents, which the sealed `rows` section counts;
            // source chunks the projection does not index are not documents.
            document_count: receipt
                .section_digests()
                .iter()
                .find(|section| section.name == "rows")
                .map(|section| usize::try_from(section.row_count).map_err(contract_error))
                .transpose()?
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "lexical artifact receipt has no rows section".to_owned(),
                    )
                })?,
            metrics: ArtifactQueryMetricsV1::default(),
            fuzzy_vocabulary,
            row_dictionary: ConnectionRowDictionaryV1::new(connection),
            row_blocks: RowBlocksV1::new(connection),
        })
    }

    fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        let control = request.control;
        let fuzzy = self.fuzzy_expansions(request)?;
        let prepared = PreparedLexicalQueryV1::new(request);
        let terms = lexical_terms(&prepared, &fuzzy);
        let stats = self.lexical_stats(&terms)?;
        retrieval_checkpoint(control)?;
        let mut phrase_queries = BTreeMap::new();
        for (_, normalized) in &prepared.phrases {
            let query = ngram_document_query(
                self.connection,
                NGRAM_NORMALIZED,
                normalized.as_bytes(),
                &self.metrics,
            )?;
            phrase_queries.insert(normalized.clone(), query);
        }
        let mut phrase_frequencies = phrase_queries
            .keys()
            .cloned()
            .map(|phrase| (phrase, 0usize))
            .collect::<BTreeMap<_, _>>();
        let phrase_documents = phrase_queries
            .values()
            .fold(RoaringBitmap::new(), |union, documents| union | documents);
        visit_lexical_rows(
            self.connection,
            &self.row_blocks,
            &phrase_documents,
            &RequestTermPostingsV1::default(),
            &self.metrics,
            control,
            |_, stored, _| {
                let row = self.decode_row(&stored).map_err(map_query_artifact_error)?;
                for (phrase, frequency) in &mut phrase_frequencies {
                    if matches_phrase(&row, phrase) {
                        *frequency += 1;
                    }
                }
                Ok(())
            },
        )?;
        let mut pruned = Vec::new();
        let documents =
            self.lexical_documents(request, &fuzzy, &stats, &phrase_queries, &mut pruned)?;
        // The scan holds one transient row and retains complete rows only for
        // the cap-bounded winners. That avoids a second winner hydration pass
        // while preserving the same strict materialization ceiling.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut excluded = self.document_count as u64;
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::new();
        visit_lexical_rows(
            self.connection,
            &self.row_blocks,
            &documents,
            &stats.postings,
            &self.metrics,
            control,
            |document, stored, frequencies| {
                let row = self.decode_row(&stored).map_err(map_query_artifact_error)?;
                let score = self.score_row(
                    &row,
                    &prepared,
                    &fuzzy,
                    &phrase_frequencies,
                    &stats,
                    &frequencies,
                );
                let Some(ranking) = admitted_score_micros(&score, &request.field_filters)? else {
                    return Ok(());
                };
                eligible += 1;
                excluded = excluded.saturating_sub(1);
                retain_bounded(
                    &mut ranked,
                    cap,
                    Keyed {
                        key: (Reverse(ranking), row.id.as_str().to_owned(), document),
                        value: (score, row),
                    },
                );
                Ok(())
            },
        )?;
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            if ordinal.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(control)?;
            }
            let Keyed {
                value: (score, row),
                ..
            } = entry;
            let mut candidate = lexical_lane_candidate(
                &row,
                &self.metadata.freshness,
                self.metadata.repository_id.clone(),
                RetrieverKind::Lexical,
                self.metadata.lexical_retriever_revision.clone(),
                request.score_domain.clone(),
                None,
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let evidence = LexicalLaneEvidence {
                binding: lexical_lane_binding(&row, &candidate, score.matched_kinds),
                field_scores_micros: score.field_scores,
                matched_whole_terms: score.matched_whole_terms,
                matched_subtokens: score.matched_subtokens,
                matched_phrases: score.matched_phrases,
                matched_proximities: score.matched_proximities,
                spelling_variants: score.spelling_variants,
                typo_recovery_applied: score.typo_recovery_applied,
                echo_penalty_applied: score.echo_penalty_applied,
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        retrieval_checkpoint(control)?;
        Ok(candidate_admission_outcome(
            capped_batch(
                self.document_count,
                eligible,
                excluded,
                truncated,
                candidates,
                evidence_by_occurrence,
            ),
            pruned,
        ))
    }

    fn exact_batch<A: ExactAdmissionAuthority>(
        &self,
        request: &ExactLaneRequest,
        authority: &A,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        retrieval_checkpoint(request.control)?;
        let documents = self.exact_documents(request)?;
        // Same bounded selection as the lexical lane: keys mirror the exact
        // lane's canonical order (admitted literal count, then occurrence),
        // and only the selected winners are rehydrated into evidence.
        // Central admission runs BEFORE heap eligibility: a document whose
        // matched literals are all denied is excluded, never selected, so a
        // denied best match can never displace an admitted candidate or
        // fail the batch. Retained state stays bounded: at most `cap`
        // ranking keys with matched-literal ordinals; proofs are admitted at
        // most once per request literal and cloned only for winners.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut excluded = self.document_count as u64;
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::new();
        let mut proofs = LiteralProofCacheV1::new(request.literals.len());
        self.visit_documents(&documents, request.control, |document| {
            let row = self.row(document)?;
            let (matched_literals, matched_kinds) = exact_matches_artifact(&row, request);
            if matched_literals.is_empty() {
                return Ok(());
            }
            let Some((admitted_ordinal, _)) =
                proofs.first_admitted(&matched_literals, request, authority)?
            else {
                return Ok(());
            };
            eligible += 1;
            excluded = excluded.saturating_sub(1);
            retain_bounded(
                &mut ranked,
                cap,
                Keyed {
                    key: (
                        Reverse(matched_literals.len()),
                        row.id.as_str().to_owned(),
                        document,
                    ),
                    value: (admitted_ordinal, matched_literals, matched_kinds),
                },
            );
            Ok(())
        })?;
        retrieval_checkpoint(request.control)?;
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        // Winners re-read in document order so each row block inflates once.
        let mut winner_documents = selected.iter().map(|entry| entry.key.2).collect::<Vec<_>>();
        winner_documents.sort_unstable();
        let mut winner_rows = BTreeMap::new();
        for document in winner_documents {
            winner_rows.insert(document, self.row(document)?);
        }
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            if ordinal.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(request.control)?;
            }
            let Keyed {
                key: (_, _, document),
                value: (admitted_ordinal, matched_literals, matched_kinds),
            } = entry;
            let proof = proofs.admitted_proof(admitted_ordinal)?;
            let matched_literals = matched_literals
                .iter()
                .map(|literal| request.literals[*literal].clone())
                .collect::<Vec<_>>();
            let row = winner_rows.remove(&document).ok_or_else(|| {
                RetrievalPortError::Contract("exact lane winner row was not read".to_owned())
            })?;
            let mut candidate = lexical_lane_candidate(
                &row,
                &self.metadata.freshness,
                self.metadata.repository_id.clone(),
                RetrieverKind::ExactLiteral,
                self.metadata.exact_retriever_revision.clone(),
                self.metadata.exact_score_domain.clone(),
                Some(proof.clone()),
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let evidence = ExactLaneEvidence {
                binding: lexical_lane_binding(&row, &candidate, matched_kinds),
                matched_literals,
                admission_proof: proof,
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        retrieval_checkpoint(request.control)?;
        Ok(RetrieverOutcome::Complete(capped_batch(
            self.document_count,
            eligible,
            excluded,
            truncated,
            candidates,
            evidence_by_occurrence,
        )))
    }

    fn row(&self, document: u32) -> Result<ArtifactRowV1, RetrievalPortError> {
        self.metrics.probe();
        let stored = self
            .row_blocks
            .row(document)
            .map_err(map_query_artifact_error)?;
        self.decode_row(&stored).map_err(map_query_artifact_error)
    }

    fn decode_row(
        &self,
        stored: &StoredRowV1,
    ) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
        decode_artifact_row(
            &self.metadata.generation,
            &stored.chunk_id,
            &stored.row,
            &stored.text,
            &self.row_dictionary,
        )
    }

    /// The candidate document set for one lexical request: every phrase
    /// match plus the term sources `admit_candidate_sources` keeps under
    /// `MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1`.
    fn lexical_documents(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        stats: &LexicalStatsCacheV1,
        phrase_queries: &BTreeMap<String, RoaringBitmap>,
        pruned: &mut Vec<(String, u64)>,
    ) -> Result<RoaringBitmap, RetrievalPortError> {
        let mut whole_terms = Vec::new();
        for term in request.whole_terms.iter() {
            whole_terms.push(normalize_lexical(term));
            if let Some(expansions) = fuzzy.by_query.get(term) {
                whole_terms.extend(expansions.iter().cloned());
            }
        }
        whole_terms.extend(
            request
                .proximities
                .iter()
                .flat_map(|proximity| &proximity.terms)
                .map(|term| normalize_lexical(term)),
        );
        let subtokens = request
            .subtokens
            .iter()
            .map(|subtoken| normalize_lexical(subtoken))
            .collect::<Vec<_>>();
        let mut sources = Vec::with_capacity(whole_terms.len() + subtokens.len());
        for term in whole_terms {
            if stats.postings.by_term.contains_key(&term) {
                sources.push((stats.whole_term_documents(&term), (term, false)));
            }
        }
        for subtoken in subtokens {
            if stats.postings.by_term.contains_key(&subtoken) {
                sources.push((
                    stats.document_frequency(LexicalFieldV1::Subtoken, &subtoken),
                    (subtoken, true),
                ));
            }
        }
        let mut documents = phrase_queries
            .values()
            .fold(RoaringBitmap::new(), |union, documents| union | documents);
        for (term, subtoken) in admit_candidate_sources(sources, |frequency, (term, _)| {
            pruned.push((term.clone(), frequency as u64));
        }) {
            documents |= stats
                .postings
                .documents(&term, |field| {
                    (field == LexicalFieldV1::Subtoken) == subtoken
                })
                .map_err(map_query_artifact_error)?;
        }
        Ok(documents)
    }

    fn exact_documents(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RoaringBitmap, RetrievalPortError> {
        let mut documents = RoaringBitmap::new();
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT documents FROM exact_postings WHERE term_id = ?1 AND field = ?2",
            )
            .map_err(map_query_sql_error)?;
        for literal in &request.literals {
            if matches!(
                literal.field,
                ExactFieldV1::QuotedPhrase
                    | ExactFieldV1::DiagnosticText
                    | ExactFieldV1::CompilerOrRuntimeError
            ) {
                documents |= ngram_document_query(
                    self.connection,
                    NGRAM_NORMALIZED,
                    &literal.original_bytes,
                    &self.metrics,
                )?;
                if let Some(ngrams) = raw_override_query_ngrams(&literal.original_bytes) {
                    documents |= ngram_bitmap_candidates(self.connection, &ngrams, &self.metrics)?;
                }
            }
            self.metrics.probe();
            let encoded: Option<Vec<u8>> = statement
                .query_row(
                    [
                        stable_exact_term_id(&literal.canonical_bytes),
                        exact_field_code(literal.field),
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_query_sql_error)?;
            if let Some(encoded) = encoded {
                documents |= decode_ngram_bitmap(&encoded).map_err(map_query_artifact_error)?;
            }
        }
        Ok(documents)
    }

    fn visit_documents(
        &self,
        documents: &RoaringBitmap,
        control: &dyn RetrievalExecutionControl,
        visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
    ) -> Result<(), RetrievalPortError> {
        self.metrics.probe();
        visit_document_ids(documents, control, visitor)
    }

    #[hotpath::measure(label = "query.lane.fuzzy.expand")]
    fn fuzzy_expansions(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<FuzzyExpansionsV1, RetrievalPortError> {
        if request.fuzzy_budget == 0 {
            return Ok(FuzzyExpansionsV1::default());
        }
        let limit = request.fuzzy_budget.min(MAX_FUZZY_TERM_EXPANSIONS_V1) as usize;
        let mut group_by_query = BTreeMap::<String, usize>::new();
        let mut groups = Vec::<FuzzyQueryGroupV1>::new();
        for (query_ordinal, query) in request.whole_terms.iter().enumerate() {
            let normalized_query = normalize_lexical(query);
            let bound = fuzzy_distance_bound(&normalized_query);
            if bound == 0 {
                continue;
            }
            if let Some(group) = group_by_query.get(&normalized_query).copied() {
                groups[group].queries.insert(query.clone());
            } else {
                let group = groups.len();
                group_by_query.insert(normalized_query.clone(), group);
                groups.push(FuzzyQueryGroupV1 {
                    first_ordinal: query_ordinal,
                    normalized_query,
                    queries: BTreeSet::from([query.clone()]),
                    bound,
                    seen: BTreeSet::new(),
                });
            }
        }
        if groups.is_empty() {
            return Ok(FuzzyExpansionsV1::default());
        }
        groups.sort_by_key(|group| group.first_ordinal);
        let maximum_distance = groups.iter().map(|group| group.bound).max().unwrap_or(0);
        let vocabulary = self.load_vocabulary()?;
        let mut selected = Vec::with_capacity(limit);
        let mut scratch = EditDistanceScratchV1::default();
        'distance: for distance in 1..=maximum_distance {
            for (group_index, group) in groups.iter_mut().enumerate() {
                let remaining = limit.saturating_sub(selected.len());
                if remaining == 0 {
                    break 'distance;
                }
                if distance > group.bound {
                    continue;
                }
                scratch.prepare_query(&group.normalized_query);
                let mut added = 0usize;
                for term in vocabulary.iter() {
                    if added >= remaining {
                        break;
                    }
                    if term != &group.normalized_query
                        && scratch.bounded_edit_distance(term, distance) == Some(distance)
                        && group.seen.insert(term.clone())
                    {
                        selected.push((group_index, term.clone()));
                        added += 1;
                    }
                }
            }
        }
        let mut by_query = BTreeMap::<String, BTreeSet<String>>::new();
        let expansion_count = selected.len();
        for (group_index, term) in selected {
            for query in &groups[group_index].queries {
                by_query
                    .entry(query.clone())
                    .or_default()
                    .insert(term.clone());
            }
        }
        hotpath::gauge!("query.lane.fuzzy.expansions_total").inc(expansion_count);
        Ok(FuzzyExpansionsV1 { by_query })
    }

    #[hotpath::measure(label = "query.artifact.vocabulary.load")]
    fn load_vocabulary(&self) -> Result<Arc<Vec<String>>, RetrievalPortError> {
        if let Some(cached) = self.fuzzy_vocabulary.get() {
            return Ok(Arc::clone(cached));
        }
        let loaded = self.load_vocabulary_from_sqlite()?;
        Ok(Arc::clone(self.fuzzy_vocabulary.get_or_init(|| loaded)))
    }

    /// One pass over the term-keyed table; its lists sit after both columns
    /// read here, so the walk never follows a list's overflow pages.
    const VOCABULARY_SQL: &'static str = "SELECT term FROM term_postings WHERE in_fuzzy = 1";

    fn load_vocabulary_from_sqlite(&self) -> Result<Arc<Vec<String>>, RetrievalPortError> {
        self.metrics.probe();
        let mut statement = self
            .connection
            .prepare_cached(Self::VOCABULARY_SQL)
            .map_err(map_query_sql_error)?;
        let mut rows = statement.query([]).map_err(map_query_sql_error)?;
        let mut vocabulary = Vec::new();
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            vocabulary.push(row.get(0).map_err(map_query_sql_error)?);
        }
        drop(rows);
        self.metrics.observe_statement(&statement)?;
        self.metrics
            .rows(u64::try_from(vocabulary.len()).map_err(contract_error)?);
        hotpath::gauge!("query.lane.fuzzy.vocabulary_terms").set(vocabulary.len());
        Ok(Arc::new(vocabulary))
    }

    /// Read the document-independent scoring statistics once per request.
    ///
    /// Per-field totals and per-(field, term) document frequencies depend
    /// only on the artifact corpus and the query terms, so one upfront read
    /// replaces the two SQL probes each scored document would otherwise
    /// repeat per term.
    #[hotpath::measure(label = "query.artifact.stats.read")]
    fn lexical_stats(
        &self,
        terms: &BTreeSet<String>,
    ) -> Result<LexicalStatsCacheV1, RetrievalPortError> {
        ensure_sqlite_bind_capacity(0, terms.len())?;
        ensure_sqlite_bound_value_bytes(
            ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
            &[],
            terms.iter().map(String::as_str),
        )?;
        let mut field_totals = BTreeMap::new();
        self.metrics.probe();
        let mut statement = self
            .connection
            .prepare_cached("SELECT field, total_length FROM field_stats")
            .map_err(map_query_sql_error)?;
        let mut rows = statement.query([]).map_err(map_query_sql_error)?;
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            let field = field_from_code(row.get::<_, i64>(0).map_err(map_query_sql_error)?)
                .map_err(map_query_artifact_error)?;
            let total: i64 = row.get(1).map_err(map_query_sql_error)?;
            field_totals.insert(field, usize::try_from(total).map_err(contract_error)?);
        }
        drop(rows);
        self.metrics.observe_statement(&statement)?;
        self.metrics
            .rows(u64::try_from(field_totals.len()).map_err(contract_error)?);
        let mut document_frequencies = BTreeMap::<LexicalFieldV1, BTreeMap<String, usize>>::new();
        let mut postings = RequestTermPostingsV1::default();
        if !terms.is_empty() {
            for term in terms {
                postings.by_term.insert(term.clone(), Vec::new());
            }
            let placeholders = std::iter::repeat_n("?", terms.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query =
                format!("SELECT term, lists FROM term_postings WHERE term IN ({placeholders})");
            self.metrics.probe();
            let mut statement = self
                .connection
                .prepare(&query)
                .map_err(map_query_sql_error)?;
            let mut rows = statement
                .query(params_from_iter(terms.iter()))
                .map_err(map_query_sql_error)?;
            let mut observed_rows = 0u64;
            let mut remaining_bytes = ARTIFACT_TERM_POSTING_QUERY_BYTES_V1;
            while let Some(row) = rows.next().map_err(map_query_sql_error)? {
                let term: String = row.get(0).map_err(map_query_sql_error)?;
                let encoded = row
                    .get_ref(1)
                    .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
                    .map_err(map_query_sql_error)?;
                remaining_bytes = remaining_bytes
                    .checked_sub(encoded.len())
                    .ok_or(RetrievalPortError::BudgetExceeded)?;
                let Some(lists) = postings.by_term.get_mut(&term) else {
                    continue;
                };
                for (field, document_frequency, list) in
                    decode_term_lists(encoded).map_err(map_query_artifact_error)?
                {
                    let field = field_from_code(field).map_err(map_query_artifact_error)?;
                    document_frequencies.entry(field).or_default().insert(
                        term.clone(),
                        usize::try_from(document_frequency).map_err(contract_error)?,
                    );
                    lists.push(TermFieldPostingsV1 {
                        field,
                        postings: list.to_vec(),
                    });
                    observed_rows = observed_rows.saturating_add(1);
                }
            }
            drop(rows);
            self.metrics.observe_statement(&statement)?;
            self.metrics.rows(observed_rows);
        }
        Ok(LexicalStatsCacheV1 {
            field_totals,
            document_frequencies,
            postings,
        })
    }

    fn score_row(
        &self,
        row: &ArtifactRowV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
        stats: &LexicalStatsCacheV1,
        frequencies: &LexicalTermFrequenciesV1,
    ) -> LexicalRowScoreV1 {
        crate::hotpath_metrics::measure_frequent("query.lane.lexical.score_row", || {
            score_lexical_row(
                row,
                &row.exact_terms,
                prepared,
                fuzzy,
                phrase_frequencies,
                |field, term| term_frequency(frequencies, field, term),
                |field, term| stats.document_frequency(field, term),
                |field, term_frequency, document_frequency| {
                    self.term_score_with_df(field, term_frequency, row, document_frequency, stats)
                },
            )
        })
    }

    fn term_score_with_df(
        &self,
        field: LexicalFieldV1,
        term_frequency: usize,
        row: &ArtifactRowV1,
        document_frequency: usize,
        stats: &LexicalStatsCacheV1,
    ) -> u64 {
        let total = stats.field_total(field);
        let average = total.div_ceil(self.document_count.max(1)).max(1);
        let document_length = row.field_lengths.get(&field).copied().unwrap_or(0).max(1);
        bm25_score_micros(
            self.document_count,
            document_frequency,
            term_frequency,
            document_length,
            average,
            field_weight_millis(field),
        )
    }
}

/// Request-relevant per-row term frequencies decoded from one SQLite JSON
/// aggregate. Absent entries mean the artifact holds no posting for that key
/// and score as zero, exactly like the SQL probes they replace. One row
/// carries at most the request's term count, so a linear scan of the decoded
/// entries beats rebuilding two nested maps per visited row.
struct LexicalTermFrequenciesV1(Vec<(LexicalFieldV1, String, usize)>);

struct LexicalStatsCacheV1 {
    field_totals: BTreeMap<LexicalFieldV1, usize>,
    document_frequencies: BTreeMap<LexicalFieldV1, BTreeMap<String, usize>>,
    postings: RequestTermPostingsV1,
}

fn lexical_terms(
    prepared: &PreparedLexicalQueryV1<'_>,
    fuzzy: &FuzzyExpansionsV1,
) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for (_, normalized) in &prepared.whole_terms {
        terms.insert(normalized.clone());
    }
    for expansions in fuzzy.by_query.values() {
        terms.extend(expansions.iter().cloned());
    }
    for (_, normalized) in &prepared.subtokens {
        terms.insert(normalized.clone());
    }
    for proximity in &prepared.proximities {
        terms.extend(proximity.terms.iter().cloned());
    }
    terms
}

fn term_frequency(
    frequencies: &LexicalTermFrequenciesV1,
    field: LexicalFieldV1,
    term: &str,
) -> usize {
    frequencies
        .0
        .iter()
        .find_map(|(entry_field, entry_term, frequency)| {
            (*entry_field == field && entry_term == term).then_some(*frequency)
        })
        .unwrap_or_default()
}

impl LexicalStatsCacheV1 {
    fn field_total(&self, field: LexicalFieldV1) -> usize {
        self.field_totals.get(&field).copied().unwrap_or_default()
    }

    fn document_frequency(&self, field: LexicalFieldV1, term: &str) -> usize {
        self.document_frequencies
            .get(&field)
            .and_then(|frequencies| frequencies.get(term))
            .copied()
            .unwrap_or_default()
    }

    /// Upper bound on the documents a whole-term source enumerates: the
    /// term's frequency summed over every non-subtoken field.
    fn whole_term_documents(&self, term: &str) -> usize {
        self.document_frequencies
            .iter()
            .filter(|(field, _)| **field != LexicalFieldV1::Subtoken)
            .map(|(_, frequencies)| frequencies.get(term).copied().unwrap_or_default())
            .fold(0usize, usize::saturating_add)
    }
}

/// Heap entry ordered by `key` alone. Payload is excluded from equality so a
/// worst-first `BinaryHeap` ranks capped winners without comparing row
/// material.
struct Keyed<K, V> {
    key: K,
    value: V,
}

impl<K: PartialEq, V> PartialEq for Keyed<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Eq, V> Eq for Keyed<K, V> {}

impl<K: Ord, V> PartialOrd for Keyed<K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: Ord, V> Ord for Keyed<K, V> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// Retain at most `cap` best-ranked entries in a worst-first max-heap.
///
/// The heap key ranks worst entries greatest, so popping after an over-cap
/// push always evicts the current worst. Between calls the heap never holds
/// more than `cap` entries, which bounds retained materialization to the
/// lane candidate cap before any winner is hydrated.
fn retain_bounded<K: Ord>(ranked: &mut BinaryHeap<K>, cap: usize, entry: K) {
    if cap == 0 {
        return;
    }
    ranked.push(entry);
    if ranked.len() > cap {
        ranked.pop();
    }
}

/// The lexical ranking key: the checked sum of the filter-admitted field
/// scores, or `None` when the typed field filters admit no scored field.
/// The same exclusion the lexical lane applies after the port returns.
fn admitted_score_micros(
    score: &LexicalRowScoreV1,
    filters: &[LexicalFieldFilterV1],
) -> Result<Option<u64>, RetrievalPortError> {
    let mut total: Option<u64> = None;
    for (field, micros) in &score.field_scores {
        if !field_admitted(filters, *field) {
            continue;
        }
        let sum = total.unwrap_or(0).checked_add(*micros).ok_or_else(|| {
            RetrievalPortError::Contract("lexical artifact ranking score overflowed".to_owned())
        })?;
        total = Some(sum);
    }
    Ok(total)
}

fn capped_batch<E>(
    examined: usize,
    eligible: u64,
    excluded: u64,
    truncated: u64,
    candidates: Vec<CompactCandidate>,
    evidence_by_occurrence: BTreeMap<SourceOccurrenceId, E>,
) -> RetrieverBatch<E> {
    RetrieverBatch {
        coverage: RetrieverCoverage {
            examined: examined as u64,
            eligible,
            excluded,
            capped: truncated,
            unknown: 0,
        },
        candidates,
        evidence_by_occurrence,
        continuation: None,
    }
}

/// Matched literal ordinals into `request.literals` plus matched term kinds.
fn exact_matches_artifact(
    row: &ArtifactRowV1,
    request: &ExactLaneRequest,
) -> (Vec<usize>, Vec<ExactTechnicalTermKindV1>) {
    exact_matches(
        ExactMatchRowViewV1 {
            sanitized_text: row.sanitized_text.as_str(),
            logical_path: &row.logical_path,
            exact_terms: &row.exact_terms,
        },
        request,
    )
}

/// Reusable buffers for the vocabulary edit-distance sweep. One expansion
/// pass compares the query against every vocabulary term per distance level;
/// per-comparison `Vec` allocations dominated that sweep.
#[derive(Default)]
struct EditDistanceScratchV1 {
    query_chars: Vec<char>,
    term_chars: Vec<char>,
    previous: Vec<usize>,
    current: Vec<usize>,
}

impl EditDistanceScratchV1 {
    fn prepare_query(&mut self, query: &str) {
        self.query_chars.clear();
        self.query_chars.extend(query.chars());
    }

    /// Levenshtein distance of the prepared query to `right` when it is at
    /// most `limit`, without per-call allocation. Byte-length prechecks prune
    /// most of the vocabulary before any character walk: one UTF-8 character
    /// is one to four bytes, so a term shorter than `chars(query) - limit`
    /// bytes or longer than `(chars(query) + limit) * 4` bytes cannot be
    /// within `limit` edits.
    fn bounded_edit_distance(&mut self, right: &str, limit: usize) -> Option<usize> {
        let query_len = self.query_chars.len();
        if right.len() < query_len.saturating_sub(limit)
            || right.len() > query_len.saturating_add(limit).saturating_mul(4)
        {
            return None;
        }
        self.term_chars.clear();
        self.term_chars.extend(right.chars());
        if query_len.abs_diff(self.term_chars.len()) > limit {
            return None;
        }
        let width = self.term_chars.len() + 1;
        self.previous.clear();
        self.previous.extend(0..width);
        self.current.clear();
        self.current.resize(width, 0);
        for (left_index, left_character) in self.query_chars.iter().enumerate() {
            self.current[0] = left_index + 1;
            let mut row_minimum = self.current[0];
            for (right_index, right_character) in self.term_chars.iter().enumerate() {
                let value = (self.previous[right_index + 1] + 1)
                    .min(self.current[right_index] + 1)
                    .min(
                        self.previous[right_index] + usize::from(left_character != right_character),
                    );
                self.current[right_index + 1] = value;
                row_minimum = row_minimum.min(value);
            }
            // The minimum of a Levenshtein DP row never decreases in later
            // rows, so a row already past the limit can never come back.
            if row_minimum > limit {
                return None;
            }
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        (self.previous[width - 1] <= limit).then_some(self.previous[width - 1])
    }
}

fn validate_cache_budget(cache_budget_bytes: usize) -> Result<(), CodeLexicalArtifactErrorV1> {
    if cache_budget_bytes == 0
        || cache_budget_bytes > CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1
    {
        return Err(CodeLexicalArtifactErrorV1::Unreserved(format!(
            "lexical artifact cache must be within 1..={CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1} bytes"
        )));
    }
    Ok(())
}

/// One reused read buffer for the whole-file digest passes. The TOCTOU
/// contract hashes a corpus-sized artifact twice per content-addressed
/// reopen, so each pass must stay I/O-shaped: 64 KiB chunks cost tens of
/// thousands of read syscalls and cancellation probes per gibibyte and pass.
/// Four mebibytes keeps the syscall count negligible while the transient
/// buffer stays far below the reader's page-cache authority.
const ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1: usize = 4 * 1024 * 1024;

/// Hash every byte the retained handle serves. Cancellation and deadline are
/// checked once per buffer, so interruption latency is bounded by one
/// [`ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1`] read-and-hash step. The read and
/// hash phases carry separate spans so a profile can attribute a slow pass
/// to I/O wait or to SHA-256 work.
#[inline]
fn hash_artifact_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
    mut record_bytes: impl FnMut(u64),
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1];
    loop {
        checkpoint(control)?;
        let read = hotpath::measure_block!("query.artifact.digest.file_read", {
            file.read(&mut buffer)
                .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))
        })?;
        if read == 0 {
            break;
        }
        record_bytes(read as u64);
        hotpath::measure_block!("query.artifact.digest.sha256_update", {
            hasher.update(&buffer[..read]);
        });
    }
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn digest_content_addressed_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    hotpath::measure_block!("query.artifact.digest.content_address_preopen", {
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.digest.content_address_preopen.passes_total").inc(1u64);
        hash_artifact_file(file, control, |bytes| {
            hotpath::gauge!("query.artifact.digest.content_address_preopen.bytes_total").inc(bytes);
        })
    })
}

fn digest_retained_artifact_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    hotpath::measure_block!("query.artifact.digest.retained_post_validation", {
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.digest.retained_post_validation.passes_total").inc(1u64);
        hash_artifact_file(file, control, |bytes| {
            hotpath::gauge!("query.artifact.digest.retained_post_validation.bytes_total")
                .inc(bytes);
        })
    })
}

/// The content-addressed head is the immutable authority for the bytes served
/// by SQLite. Rehash the retained file only after the SQLite handle has
/// completed its full validation, so an in-place mutation cannot retain its
/// inode and still be returned as the original content address.
fn verify_retained_artifact_digest(
    file: &mut File,
    expected: &ManifestDigest,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let actual = digest_retained_artifact_file(file, control)?;
    if &actual != expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "artifact bytes changed after SQLite opened the verified file".to_owned(),
        ));
    }
    Ok(())
}

/// Make the content-addressed reader refuse a replacement at the published
/// name. It runs immediately before and after SQLite opens that name; after
/// the latter check, SQLite holds the verified file's own handle.
fn verify_named_path_identity(path: &Path, file: &File) -> Result<(), CodeLexicalArtifactErrorV1> {
    let named = path.symlink_metadata().map_err(map_artifact_file_error)?;
    if !named.file_type().is_file() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "artifact path changed from a regular file while opening".to_owned(),
        ));
    }
    let opened = file.metadata().map_err(map_artifact_file_error)?;
    if named.len() != opened.len() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "artifact path changed size while opening".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact path was atomically replaced while opening".to_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let named_file = File::open(path).map_err(map_artifact_file_error)?;
        let named_identity = tracedecay_private_fs::windows_file::information(&named_file)
            .map_err(map_artifact_file_error)?;
        let opened_identity = tracedecay_private_fs::windows_file::information(file)
            .map_err(map_artifact_file_error)?;
        if named_identity.volume_serial_number != opened_identity.volume_serial_number
            || named_identity.file_index != opened_identity.file_index
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact path was atomically replaced while opening".to_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(CodeLexicalArtifactErrorV1::Incompatible(
            "the platform does not expose a stable artifact file identity".to_owned(),
        ))
    }
}

fn verify_artifact_state_revision(
    connection: &Connection,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let revision: i64 = connection
        .query_row(
            "SELECT format_revision FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            CodeLexicalArtifactErrorV1::Incompatible(format!(
                "artifact state has no readable format revision: {error}"
            ))
        })?;
    let revision = u32::try_from(revision).map_err(|_| {
        CodeLexicalArtifactErrorV1::Incompatible(
            "artifact state format revision is outside the supported range".to_owned(),
        )
    })?;
    require_served_revision(revision)?;
    checkpoint(control)
}

fn sealed_reader_mmap_bytes(file_size_bytes: u64) -> Result<i64, CodeLexicalArtifactErrorV1> {
    i64::try_from(file_size_bytes).map_err(|error| {
        CodeLexicalArtifactErrorV1::Contract(format!(
            "sealed lexical artifact is larger than SQLite's mmap_size domain: {error}"
        ))
    })
}

fn configure_reader_window(
    connection: &Connection,
    cache_budget_bytes: usize,
    retained_metadata_bytes: usize,
    sealed_file_size_bytes: u64,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let available = cache_budget_bytes
        .checked_sub(retained_metadata_bytes)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Unreserved(
                "lexical artifact metadata exceeds the reader reservation".to_owned(),
            )
        })?;
    if available == 0 {
        return Err(CodeLexicalArtifactErrorV1::Unreserved(
            "lexical artifact reader has no SQLite cache reservation".to_owned(),
        ));
    }
    let page_cache_bytes = available.min(ARTIFACT_SQLITE_CACHE_BYTES);
    connection
        .pragma_update(
            None,
            "cache_size",
            -i64::try_from(page_cache_bytes / 1024)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
        )
        .map_err(sqlite_error)?;
    // Content-addressed readers are SQLITE_OPEN_READ_ONLY over an immutable
    // file. The kernel SQLite window that disables mmap exists for writer /
    // WAL coherence on graph and staging connections; applying it here forced
    // sqlite3OsRead to re-pread ~174 MB of posting pages on every tool call
    // against a multi-gigabyte artifact whose 64 MiB heap cache cannot retain
    // the working set.
    let mmap_bytes = sealed_reader_mmap_bytes(sealed_file_size_bytes)?;
    connection
        .pragma_update(None, "mmap_size", mmap_bytes)
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "temp_store", "FILE")
        .map_err(sqlite_error)?;
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("query.artifact.mmap_bytes").set(sealed_file_size_bytes);
        hotpath::gauge!("query.artifact.page_cache_bytes").set(page_cache_bytes);
    }
    Ok(page_cache_bytes)
}

fn map_artifact_file_error(error: std::io::Error) -> CodeLexicalArtifactErrorV1 {
    if error.kind() == std::io::ErrorKind::NotFound {
        CodeLexicalArtifactErrorV1::Missing(error.to_string())
    } else {
        CodeLexicalArtifactErrorV1::Io(error.to_string())
    }
}

fn map_private_artifact_file_error(error: std::io::Error) -> CodeLexicalArtifactErrorV1 {
    if error.kind() == std::io::ErrorKind::NotFound {
        CodeLexicalArtifactErrorV1::Missing(error.to_string())
    } else {
        CodeLexicalArtifactErrorV1::Corrupt(format!(
            "content-addressed lexical artifact does not satisfy the private-file authority: {error}"
        ))
    }
}

fn map_reader_open_error(path: &Path, error: rusqlite::Error) -> CodeLexicalArtifactErrorV1 {
    match path.try_exists() {
        Ok(false) => CodeLexicalArtifactErrorV1::Missing(error.to_string()),
        Ok(true) | Err(_) => sqlite_error(error),
    }
}

fn row_occurrence(row: ArtifactRowV1) -> CodeLexicalArtifactOccurrenceV1 {
    CodeLexicalArtifactOccurrenceV1 {
        generation: row.anchor.generation_id,
        file: row.anchor.file_occurrence_id,
        symbol: row.anchor.symbol_occurrence_id,
        chunk: row.id,
        source_span: row.anchor.source_span,
        logical_path: row.logical_path,
        sanitized_text: row.sanitized_text,
        simple_name: row.symbol_simple_name,
        qualified_name: row.symbol_qualified_name,
        kind: row.symbol_kind,
    }
}

fn map_query_sql_error(error: rusqlite::Error) -> RetrievalPortError {
    RetrievalPortError::AuthorityUnavailable(format!("lexical artifact read failed: {error}"))
}

fn map_query_artifact_error(error: CodeLexicalArtifactErrorV1) -> RetrievalPortError {
    match error {
        CodeLexicalArtifactErrorV1::Interrupted(_) => RetrievalPortError::Cancelled,
        CodeLexicalArtifactErrorV1::Incompatible(_) => RetrievalPortError::IncompatibleProjection,
        CodeLexicalArtifactErrorV1::Contract(error) => RetrievalPortError::Contract(error),
        CodeLexicalArtifactErrorV1::Unreserved(_)
        | CodeLexicalArtifactErrorV1::BatchTooLarge { .. } => RetrievalPortError::BudgetExceeded,
        CodeLexicalArtifactErrorV1::Corrupt(error)
        | CodeLexicalArtifactErrorV1::Io(error)
        | CodeLexicalArtifactErrorV1::Missing(error) => {
            RetrievalPortError::AuthorityUnavailable(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Reverse;
    use std::collections::{BTreeSet, BinaryHeap};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use roaring::RoaringBitmap;
    use rusqlite::hooks::{AuthAction, Authorization};
    use rusqlite::{Connection, OpenFlags, params};
    use sha2::{Digest, Sha256};
    use tracedecay_domain::{
        CodeGenerationId, ComponentRevision, FreshnessCompatibilityV1, ManifestDigest,
        ScoreDomainId, SourceFreshness, SourceInstanceKey, SourceNamespace, UtcMicros,
    };
    use tracedecay_private_fs::open_private_file;

    use super::super::format::{PostingListEncoderV1, encode_document_set};
    use super::super::row_codec::{BlockRowV1, RowBlocksV1, encode_row_blocks};
    use super::{
        ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1, ARTIFACT_NGRAM_MAX_CANDIDATES_V1,
        ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1,
        ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1, ArtifactQueryMetricsV1,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1,
        CodeLexicalArtifactReaderV1, LexicalFieldV1, NGRAM_NORMALIZED, RequestTermPostingsV1,
        TermFieldPostingsV1, charge_ngram_encoded_list_bytes, configure_reader_window,
        ensure_ngram_candidate_cardinality, ensure_sqlite_bind_capacity,
        ensure_sqlite_bound_value_bytes, map_query_artifact_error, ngram_bitmap_candidates,
        ngram_document_query, query_ngrams, retain_bounded, term_frequency, visit_document_ids,
        visit_lexical_rows,
    };
    use crate::retrieval::lexical::CodeLexicalProjectionMetadataV1;
    use crate::retrieval::ports::RetrievalExecutionControl;
    use crate::retrieval::ports::RetrievalPortError;
    use tracedecay_code_index::production::CodeIndexExecutionControlV1;

    struct AlwaysActiveControl;

    impl CodeIndexExecutionControlV1 for AlwaysActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    impl RetrievalExecutionControl for AlwaysActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn elapsed_micros(&self) -> u64 {
            0
        }
    }

    /// An opener projection for reads refused before its content is compared.
    fn opener() -> CodeLexicalProjectionMetadataV1 {
        CodeLexicalProjectionMetadataV1 {
            generation: CodeGenerationId::new("generation.reader.v1").expect("generation"),
            repository_id: None,
            logical_paths: Default::default(),
            freshness: SourceFreshness {
                source_namespace: SourceNamespace::new("namespace.reader").expect("namespace"),
                source_instance: SourceInstanceKey::new("instance.reader").expect("instance"),
                source_watermark: None,
                projection_watermark: None,
                observed_at: UtcMicros(0),
                source_generation: None,
                generation_lag: None,
                compatibility: FreshnessCompatibilityV1::Unknown,
                policy_revision: ComponentRevision::new("policy.reader.v1").expect("policy"),
            },
            exact_retriever_revision: ComponentRevision::new("retriever.exact.reader.v1")
                .expect("exact retriever"),
            lexical_retriever_revision: ComponentRevision::new("retriever.lexical.reader.v1")
                .expect("lexical retriever"),
            exact_score_domain: ScoreDomainId::new("score.exact.reader.v1").expect("score domain"),
            clone_route: None,
        }
    }

    /// A request authority that reports cancellation from its `cancel_at`-th
    /// consultation onwards, counting every consultation it receives.
    struct CancelAtObservation {
        observations: AtomicUsize,
        cancel_at: usize,
    }

    impl CancelAtObservation {
        fn new(cancel_at: usize) -> Self {
            Self {
                observations: AtomicUsize::new(0),
                cancel_at,
            }
        }

        fn observations(&self) -> usize {
            self.observations.load(Ordering::SeqCst)
        }
    }

    impl RetrievalExecutionControl for CancelAtObservation {
        fn is_cancelled(&self) -> bool {
            self.observations.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
        }

        fn elapsed_micros(&self) -> u64 {
            0
        }
    }

    #[test]
    fn sealed_reader_window_mmaps_the_immutable_file() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("sealed.sqlite");
        let seed = Connection::open(&path).expect("create sealed fixture");
        seed.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
            .expect("seed sealed fixture");
        drop(seed);
        let file_size = std::fs::metadata(&path).expect("stat sealed fixture").len();
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open sealed reader");
        let page_cache_bytes = configure_reader_window(
            &connection,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            0,
            file_size,
        )
        .expect("configure sealed reader window");
        assert_eq!(page_cache_bytes, ARTIFACT_SQLITE_CACHE_BYTES);
        let _: i64 = connection
            .query_row("SELECT x FROM t", [], |row| row.get(0))
            .expect("touch the mapped file");
        let mmap: i64 = connection
            .pragma_query_value(None, "mmap_size", |row| row.get(0))
            .expect("read mmap pragma");
        assert!(
            mmap >= i64::try_from(file_size).expect("fixture fits mmap_size"),
            "sealed readers must mmap the immutable file so serving does not re-pread it: mmap={mmap} file={file_size}"
        );
    }

    #[test]
    fn content_addressed_integrity_reuses_the_publisher_proof() {
        let connection = Connection::open_in_memory().expect("open SQLite fixture");
        let quick_check_observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer = std::sync::Arc::clone(&quick_check_observed);
        connection
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Pragma { pragma_name, .. }
                        if pragma_name.eq_ignore_ascii_case("quick_check")
                ) {
                    observer.store(true, Ordering::SeqCst);
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))
            .expect("install quick-check observer");

        super::verify_reader_sqlite_integrity(
            &connection,
            super::ReaderIntegrityAuthorityV1::ContentAddressedPublisherProof,
        )
        .expect("a content-addressed reopen reuses its publisher integrity proof");
        assert!(
            !quick_check_observed.load(Ordering::SeqCst),
            "content-addressed reopen must not rescan the whole SQLite artifact"
        );

        let error = super::verify_reader_sqlite_integrity(
            &connection,
            super::ReaderIntegrityAuthorityV1::ReceiptOnly,
        )
        .expect_err("a receipt-only reopen still requires SQLite integrity verification");
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Io(_)));
        assert!(quick_check_observed.load(Ordering::SeqCst));
    }

    struct MutateSqliteHeaderAtObservation {
        path: PathBuf,
        mutation_observation: usize,
        observations: AtomicUsize,
    }

    impl MutateSqliteHeaderAtObservation {
        fn new(path: PathBuf, mutation_observation: usize) -> Self {
            Self {
                path,
                mutation_observation,
                observations: AtomicUsize::new(0),
            }
        }
    }

    impl CodeIndexExecutionControlV1 for MutateSqliteHeaderAtObservation {
        fn is_cancelled(&self) -> bool {
            let observation = self
                .observations
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            if observation == self.mutation_observation {
                let connection = Connection::open(&self.path)
                    .expect("open the artifact through a real SQLite writer");
                connection
                    .pragma_update(None, "user_version", 2i64)
                    .expect("mutate the same artifact inode through SQLite");
            }
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    struct CancelFromObservation {
        cancel_from_observation: usize,
        observations: AtomicUsize,
    }

    impl CancelFromObservation {
        fn new(cancel_from_observation: usize) -> Self {
            Self {
                cancel_from_observation,
                observations: AtomicUsize::new(0),
            }
        }
    }

    impl CodeIndexExecutionControlV1 for CancelFromObservation {
        fn is_cancelled(&self) -> bool {
            let observation = self
                .observations
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            observation >= self.cancel_from_observation
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    #[test]
    fn invalid_content_addressed_budget_is_rejected_before_path_touch() {
        let missing = std::env::temp_dir().join(format!(
            "tracedecay-reader-budget-missing-{}",
            std::process::id()
        ));
        let digest =
            ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).expect("digest fixture");

        let error = CodeLexicalArtifactReaderV1::open_content_addressed(
            &missing,
            &digest,
            0,
            &opener(),
            0,
            &AlwaysActiveControl,
        )
        .expect_err("an invalid budget wins before the missing path is observed");

        assert!(matches!(error, CodeLexicalArtifactErrorV1::Unreserved(_)));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_identity_refuses_replacement_after_sqlite_open() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join("artifact.sqlite");
        let replacement_path = directory.path().join("replacement.sqlite");
        let connection = Connection::open(&artifact_path).expect("create artifact SQLite file");
        connection
            .pragma_update(None, "user_version", 1i64)
            .expect("seed original SQLite header");
        drop(connection);
        std::fs::copy(&artifact_path, &replacement_path).expect("copy replacement SQLite file");
        let replacement = Connection::open(&replacement_path).expect("open replacement SQLite");
        replacement
            .pragma_update(None, "user_version", 2i64)
            .expect("mutate replacement SQLite header");
        drop(replacement);

        let opened = std::fs::File::open(&artifact_path).expect("retain original file handle");
        super::verify_named_path_identity(&artifact_path, &opened)
            .expect("named path initially identifies the retained file");
        let served = Connection::open_with_flags(
            &artifact_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("SQLite opens the retained artifact name");
        std::fs::rename(&replacement_path, &artifact_path)
            .expect("atomically replace the artifact after SQLite open");

        assert!(matches!(
            super::verify_named_path_identity(&artifact_path, &opened),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
        let served_version: i64 = served
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("the opened SQLite connection remains bound to the original file");
        assert_eq!(served_version, 1);
    }

    #[cfg(unix)]
    #[test]
    fn same_inode_mutation_after_hash_must_not_pass_artifact_validation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join("artifact.sqlite");
        let connection = Connection::open(&artifact_path).expect("create artifact SQLite file");
        connection
            .pragma_update(None, "user_version", 1i64)
            .expect("seed original SQLite header");
        drop(connection);
        std::fs::set_permissions(&artifact_path, std::fs::Permissions::from_mode(0o600))
            .expect("make the artifact private");

        let mut retained = open_private_file(&artifact_path).expect("retain private artifact file");
        let expected = super::digest_content_addressed_file(&mut retained, &AlwaysActiveControl)
            .expect("hash the original artifact bytes");
        let mutation = MutateSqliteHeaderAtObservation::new(artifact_path.clone(), 1);
        super::checkpoint(&mutation).expect("run the reader's post-hash checkpoint");
        let served = Connection::open_with_flags(
            &artifact_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("SQLite opens the same inode after mutation");
        let served_version: i64 = served
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read the served SQLite header");

        assert!(
            matches!(
                super::verify_retained_artifact_digest(
                    &mut retained,
                    &expected,
                    &AlwaysActiveControl,
                ),
                Err(CodeLexicalArtifactErrorV1::Corrupt(_))
            ),
            "SQLite served same-inode user_version {served_version} after digest {expected}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn content_addressed_open_refuses_non_private_artifact_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join("artifact.sqlite");
        let connection = Connection::open(&artifact_path).expect("create artifact SQLite file");
        connection
            .pragma_update(None, "user_version", 1i64)
            .expect("seed SQLite header");
        drop(connection);
        std::fs::set_permissions(&artifact_path, std::fs::Permissions::from_mode(0o644))
            .expect("make the artifact publicly readable");

        let mut file = std::fs::File::open(&artifact_path).expect("open artifact for digest");
        let digest = super::digest_content_addressed_file(&mut file, &AlwaysActiveControl)
            .expect("hash the publicly readable artifact");
        let size = file.metadata().expect("artifact metadata").len();

        let error = CodeLexicalArtifactReaderV1::open_content_addressed(
            &artifact_path,
            &digest,
            size,
            &opener(),
            1024 * 1024,
            &AlwaysActiveControl,
        )
        .expect_err("content-addressed reader must require the private artifact authority");

        assert!(matches!(error, CodeLexicalArtifactErrorV1::Corrupt(_)));
    }

    /// The buffered digest loop must hash exactly the bytes each read
    /// returns: a fixture larger than two read buffers with an odd tail
    /// exposes stale-tail reuse, whole-buffer hashing, or dropped chunks.
    #[test]
    fn whole_file_digest_matches_a_one_shot_hash_across_read_buffer_boundaries() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("multi-buffer.bin");
        let mut bytes = vec![0u8; super::ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1 * 2 + 4097];
        for (ordinal, byte) in bytes.iter_mut().enumerate() {
            *byte = (ordinal % 251) as u8;
        }
        std::fs::write(&path, &bytes).expect("write multi-buffer fixture");
        let mut file = std::fs::File::open(&path).expect("open multi-buffer fixture");

        let chunked = super::digest_content_addressed_file(&mut file, &AlwaysActiveControl)
            .expect("hash the fixture through the buffered loop");

        let one_shot =
            ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
                .expect("one-shot fixture digest");
        assert_eq!(
            chunked, one_shot,
            "the buffered digest loop must hash exactly the bytes each read returns"
        );
    }

    /// Cancellation is observed between read buffers: a control that cancels
    /// from its second observation must interrupt a digest spanning multiple
    /// buffers instead of completing it.
    #[test]
    fn digest_cancellation_interrupts_between_read_buffers() {
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let path = directory.path().join("cancel-mid-digest.bin");
        std::fs::write(
            &path,
            vec![7u8; super::ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1 * 2 + 1],
        )
        .expect("write fixture spanning multiple read buffers");
        let mut file = std::fs::File::open(&path).expect("open multi-buffer fixture");
        let control = CancelFromObservation::new(2);

        let error = super::digest_content_addressed_file(&mut file, &control)
            .expect_err("cancellation after the first buffer must interrupt the digest");

        assert!(matches!(error, CodeLexicalArtifactErrorV1::Interrupted(_)));
    }

    #[cfg(unix)]
    #[test]
    fn content_addressed_open_refuses_a_durable_head_digest_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join("artifact.sqlite");
        std::fs::write(&artifact_path, b"durable artifact bytes").expect("write artifact fixture");
        std::fs::set_permissions(&artifact_path, std::fs::Permissions::from_mode(0o600))
            .expect("make the artifact private");
        let size = std::fs::metadata(&artifact_path)
            .expect("artifact metadata")
            .len();
        let foreign =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("foreign digest");

        let error = CodeLexicalArtifactReaderV1::open_content_addressed(
            &artifact_path,
            &foreign,
            size,
            &opener(),
            1024 * 1024,
            &AlwaysActiveControl,
        )
        .expect_err("bytes that miss the durable head digest must be refused before SQLite opens");

        assert!(matches!(
            &error,
            CodeLexicalArtifactErrorV1::Corrupt(message)
                if message.contains("do not match the durable head digest")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn content_addressed_open_refuses_a_truncated_artifact_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join("artifact.sqlite");
        std::fs::write(&artifact_path, b"durable artifact bytes before truncation")
            .expect("write artifact fixture");
        std::fs::set_permissions(&artifact_path, std::fs::Permissions::from_mode(0o600))
            .expect("make the artifact private");
        let mut intact = open_private_file(&artifact_path).expect("retain the intact artifact");
        let digest = super::digest_content_addressed_file(&mut intact, &AlwaysActiveControl)
            .expect("hash the intact artifact");
        let size = intact.metadata().expect("intact artifact metadata").len();
        drop(intact);
        let truncating = std::fs::OpenOptions::new()
            .write(true)
            .open(&artifact_path)
            .expect("reopen the artifact for truncation");
        truncating.set_len(size - 1).expect("truncate the artifact");
        drop(truncating);

        let error = CodeLexicalArtifactReaderV1::open_content_addressed(
            &artifact_path,
            &digest,
            size,
            &opener(),
            1024 * 1024,
            &AlwaysActiveControl,
        )
        .expect_err("a truncated artifact must be refused before SQLite opens it");

        assert!(matches!(
            &error,
            CodeLexicalArtifactErrorV1::Corrupt(message)
                if message.contains("the durable head names")
        ));
    }

    #[test]
    fn typed_artifact_availability_never_becomes_an_empty_result() {
        assert!(matches!(
            map_query_artifact_error(CodeLexicalArtifactErrorV1::Missing(
                "sealed artifact is absent".to_owned()
            )),
            crate::retrieval::ports::RetrievalPortError::AuthorityUnavailable(_)
        ));
        assert_eq!(
            map_query_artifact_error(CodeLexicalArtifactErrorV1::Unreserved(
                "reader has no cache reservation".to_owned()
            )),
            crate::retrieval::ports::RetrievalPortError::BudgetExceeded
        );
    }

    fn ngram_fixture(lists: &[(u32, &[u32])]) -> Connection {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE ngram_postings (
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    document_frequency INTEGER NOT NULL,
                    documents BLOB NOT NULL,
                    PRIMARY KEY(kind, ngram)
                ) WITHOUT ROWID;",
            )
            .expect("ngram fixture schema");
        for (ngram, documents) in lists {
            let bitmap = RoaringBitmap::from_iter(documents.iter().copied());
            connection
                .execute(
                    "INSERT INTO ngram_postings(kind, ngram, document_frequency, documents) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        NGRAM_NORMALIZED,
                        i64::from(*ngram),
                        bitmap.len() as i64,
                        encode_document_set(&bitmap).expect("encode ngram list")
                    ],
                )
                .expect("seed ngram list");
        }
        connection
    }

    #[test]
    fn phrase_ngram_stream_selects_the_rarest_fixed_predicates_from_the_whole_phrase() {
        let phrase = b"abcdefghijklmnopqrstuvw";
        let ngrams = query_ngrams(phrase).into_iter().collect::<Vec<_>>();
        assert!(ngrams.len() > ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1);
        let lists = ngrams
            .iter()
            .enumerate()
            .map(|(ordinal, ngram)| {
                // The only selective predicate sorts beyond the fixed
                // intersection count in packed-ngram order.
                let documents: &[u32] = if ordinal + 1 < ngrams.len() {
                    &[1, 2]
                } else {
                    &[1]
                };
                (*ngram, documents)
            })
            .collect::<Vec<_>>();
        let connection = ngram_fixture(&lists);

        let metrics = ArtifactQueryMetricsV1::default();
        let documents = ngram_document_query(&connection, NGRAM_NORMALIZED, phrase, &metrics)
            .expect("build ngram bitmap query");

        assert_eq!(documents.iter().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            metrics.ngram_decoded_lists.get(),
            ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1 as u64,
            "selectivity must not increase the fixed list-work bound"
        );
    }

    fn normalized(ngrams: &[u32]) -> Vec<(i64, u32)> {
        ngrams
            .iter()
            .map(|ngram| (NGRAM_NORMALIZED, *ngram))
            .collect()
    }

    #[test]
    fn ngram_bitmap_query_processes_rare_lists_first_and_short_circuits_common_work() {
        let connection = ngram_fixture(&[(10, &[1, 2, 3, 4, 5, 6]), (20, &[2]), (30, &[3])]);

        let bounded_metrics = ArtifactQueryMetricsV1::default();
        let matching =
            ngram_bitmap_candidates(&connection, &normalized(&[10, 20]), &bounded_metrics)
                .expect("intersect common and rare lists");
        assert_eq!(matching.iter().collect::<Vec<_>>(), [2]);
        assert_eq!(bounded_metrics.ngram_peak_candidates.get(), 1);
        assert_eq!(bounded_metrics.ngram_decoded_lists.get(), 2);
        assert_eq!(bounded_metrics.observed_fullscan_steps(), 0);

        let short_circuit_metrics = ArtifactQueryMetricsV1::default();
        let empty = ngram_bitmap_candidates(
            &connection,
            &normalized(&[10, 20, 30]),
            &short_circuit_metrics,
        )
        .expect("short-circuit disjoint rare lists");
        assert!(empty.is_empty());
        assert_eq!(short_circuit_metrics.ngram_peak_candidates.get(), 1);
        assert_eq!(
            short_circuit_metrics.ngram_decoded_lists.get(),
            2,
            "disjoint rare lists must end the query before the common list is decoded"
        );

        let missing = ngram_bitmap_candidates(
            &connection,
            &normalized(&[10, 40]),
            &ArtifactQueryMetricsV1::default(),
        )
        .expect("absent ngram");
        assert!(missing.is_empty(), "an absent ngram admits no document");
    }

    #[test]
    fn ngram_candidate_bitmap_honors_its_reader_memory_authority() {
        assert_eq!(
            ensure_ngram_candidate_cardinality(ARTIFACT_NGRAM_MAX_CANDIDATES_V1),
            Ok(())
        );
        assert_eq!(
            ensure_ngram_candidate_cardinality(ARTIFACT_NGRAM_MAX_CANDIDATES_V1 + 1),
            Err(crate::retrieval::ports::RetrievalPortError::BudgetExceeded)
        );
    }

    #[test]
    fn ngram_query_rejects_cumulative_encoded_lists_past_its_authority() {
        let mut remaining = 40usize;
        for _ in 0..8 {
            charge_ngram_encoded_list_bytes(&mut remaining, 5, 5)
                .expect("individually valid encoded list");
        }
        assert_eq!(remaining, 0);
        assert_eq!(
            charge_ngram_encoded_list_bytes(&mut remaining, 1, 5),
            Err(crate::retrieval::ports::RetrievalPortError::BudgetExceeded)
        );
        assert_eq!(
            remaining, 0,
            "a refused list must not consume the retained query authority"
        );
        let mut remaining = 40usize;
        assert_eq!(
            charge_ngram_encoded_list_bytes(&mut remaining, 6, 5),
            Err(crate::retrieval::ports::RetrievalPortError::BudgetExceeded),
            "one list past the per-list ceiling is refused outright"
        );
    }

    fn term_list(field: LexicalFieldV1, postings: &[(u32, u32)]) -> TermFieldPostingsV1 {
        let mut encoder = PostingListEncoderV1::new(true);
        for (document, frequency) in postings {
            encoder
                .push(*document, *frequency)
                .expect("ascending posting");
        }
        TermFieldPostingsV1 {
            field,
            postings: encoder.finish().expect("non-empty list"),
        }
    }

    #[test]
    fn whole_term_and_subtoken_sources_split_one_terms_lists_by_field() {
        let mut postings = RequestTermPostingsV1::default();
        postings.by_term.insert(
            "render".to_owned(),
            vec![
                term_list(LexicalFieldV1::BodyText, &[(1, 1)]),
                term_list(LexicalFieldV1::Subtoken, &[(3, 2)]),
            ],
        );
        postings.by_term.insert(
            "renderer".to_owned(),
            vec![term_list(LexicalFieldV1::BodyText, &[(2, 1)])],
        );
        let whole = |term| {
            postings
                .documents(term, |field| field != LexicalFieldV1::Subtoken)
                .expect("decode whole-term lists")
        };
        let subtoken = postings
            .documents("render", |field| field == LexicalFieldV1::Subtoken)
            .expect("decode subtoken list");

        assert_eq!(whole("render").iter().collect::<Vec<_>>(), [1]);
        assert_eq!(subtoken.iter().collect::<Vec<_>>(), [3]);
        assert_eq!(
            (whole("render") | whole("renderer") | subtoken)
                .iter()
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(whole("absent").is_empty());
    }

    const ALPHA: &str = "alpha";

    /// `documents` rows whose payload is the little-endian document id,
    /// stored in row blocks, and the request postings of `alpha` (every
    /// document, frequency `document % 3 + 1`).
    fn lexical_row_stream_fixture(documents: u32) -> (Connection, RequestTermPostingsV1) {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE row_blocks (
                    first_document INTEGER PRIMARY KEY,
                    payload BLOB NOT NULL
                );",
            )
            .expect("lexical row fixture schema");
        let chunk_ids = (0..documents)
            .map(|document| format!("chunk.{document}"))
            .collect::<Vec<_>>();
        let payloads = (0..documents)
            .map(|document| document.to_le_bytes())
            .collect::<Vec<_>>();
        let rows = (0..documents)
            .map(|document| {
                let index = usize::try_from(document).expect("document index");
                BlockRowV1 {
                    document_id: i64::from(document),
                    chunk_id: &chunk_ids[index],
                    parent_chunk_id: None,
                    row: &payloads[index],
                    text: "fn alpha() {}",
                }
            })
            .collect::<Vec<_>>();
        for (first_document, payload) in encode_row_blocks(&rows).expect("row blocks") {
            connection
                .execute(
                    "INSERT INTO row_blocks(first_document, payload) VALUES (?1, ?2)",
                    params![first_document, payload],
                )
                .expect("artifact row block");
        }
        let mut postings = RequestTermPostingsV1::default();
        postings.by_term.insert(
            ALPHA.to_owned(),
            vec![term_list(
                LexicalFieldV1::BodyText,
                &(0..documents)
                    .map(|document| (document, document % 3 + 1))
                    .collect::<Vec<_>>(),
            )],
        );
        (connection, postings)
    }

    #[test]
    fn lexical_candidate_batches_bound_cancellation_probes() {
        let (connection, postings) = lexical_row_stream_fixture(256);
        let documents = RoaringBitmap::from_iter(0..256);
        let control = CancelAtObservation::new(usize::MAX);
        let mut visited = 0;
        visit_lexical_rows(
            &connection,
            &RowBlocksV1::new(&connection),
            &documents,
            &postings,
            &ArtifactQueryMetricsV1::default(),
            &control,
            |_, _, _| {
                visited += 1;
                Ok(())
            },
        )
        .expect("active request visits its candidates");
        assert_eq!(visited, 256);
        assert!(
            control.observations() <= 5,
            "request authority must be consulted per batch, not per candidate: {} probes",
            control.observations()
        );
    }

    /// A request cancelled between batches unwinds before decoding the next
    /// page's candidates, with a typed error rather than partial success.
    /// The same stream under an active control visits every row, so the
    /// checkpoint changes nothing for an uncancelled request.
    #[test]
    fn lexical_row_stream_unwinds_at_the_first_checkpoint_after_cancellation() {
        let (connection, postings) = lexical_row_stream_fixture(512);
        let documents = RoaringBitmap::from_iter(0..512);

        let control = CancelAtObservation::new(2);
        let mut visited = 0usize;
        let error = visit_lexical_rows(
            &connection,
            &RowBlocksV1::new(&connection),
            &documents,
            &postings,
            &ArtifactQueryMetricsV1::default(),
            &control,
            |_, _, _| {
                visited += 1;
                Ok(())
            },
        )
        .expect_err("a cancelled request must not stream to completion");
        assert_eq!(error, RetrievalPortError::Cancelled);
        assert_eq!(
            visited, 128,
            "every row before the cancelling checkpoint is visited and none after it"
        );
        assert_eq!(
            control.observations(),
            2,
            "the stream stops consulting the control once it reports cancellation"
        );

        let mut complete = 0usize;
        visit_lexical_rows(
            &connection,
            &RowBlocksV1::new(&connection),
            &documents,
            &postings,
            &ArtifactQueryMetricsV1::default(),
            &AlwaysActiveControl,
            |_, _, _| {
                complete += 1;
                Ok(())
            },
        )
        .expect("an uncancelled request streams every candidate row");
        assert_eq!(complete, 512);
    }

    #[test]
    fn exact_document_stream_cancels_before_the_next_candidate_batch() {
        let documents = RoaringBitmap::from_iter(0..512);
        let control = CancelAtObservation::new(2);
        let mut visited = Vec::new();
        let error = visit_document_ids(&documents, &control, |document| {
            visited.push(document);
            Ok(())
        })
        .expect_err("the next candidate batch must not start");
        assert_eq!(error, RetrievalPortError::Cancelled);
        assert_eq!(visited, (0..128).collect::<Vec<_>>());
        assert_eq!(control.observations(), 2);
    }

    #[test]
    fn lexical_row_stream_reads_term_frequencies_from_one_list_walk_at_scale() {
        let (connection, postings) = lexical_row_stream_fixture(2_048);
        // Every third document is a candidate: cursors must skip the
        // postings between candidates rather than report neighbours.
        let documents = (0..2_048).step_by(3).collect::<RoaringBitmap>();
        let metrics = ArtifactQueryMetricsV1::default();
        let mut visited = 0usize;

        let rows = RowBlocksV1::new(&connection);
        visit_lexical_rows(
            &connection,
            &rows,
            &documents,
            &postings,
            &metrics,
            &AlwaysActiveControl,
            |document, stored, frequencies| {
                assert_eq!(stored.row, document.to_le_bytes());
                assert_eq!(stored.chunk_id, format!("chunk.{document}"));
                assert_eq!(
                    term_frequency(&frequencies, LexicalFieldV1::BodyText, ALPHA),
                    usize::try_from(document).unwrap() % 3 + 1
                );
                assert_eq!(
                    term_frequency(&frequencies, LexicalFieldV1::Subtoken, ALPHA),
                    0,
                    "a field without a list stays an exact zero"
                );
                assert_eq!(
                    term_frequency(&frequencies, LexicalFieldV1::BodyText, "beta"),
                    0,
                    "absent terms stay exact zeroes"
                );
                visited += 1;
                Ok(())
            },
        )
        .expect("lexical row stream");

        assert_eq!(visited, documents.len() as usize);
        assert_eq!(
            metrics.probes(),
            1,
            "statement count must stay constant as documents and terms grow"
        );
        assert_eq!(
            metrics.observed_fullscan_steps(),
            0,
            "row lookups must stay on the row key at scale"
        );
        let blocks: i64 = connection
            .query_row("SELECT COUNT(*) FROM row_blocks", [], |row| row.get(0))
            .expect("block count");
        assert!(
            blocks >= 2_048 / 32,
            "rows must be stored in bounded blocks, not one blob: {blocks} blocks"
        );
    }

    #[test]
    fn request_term_reads_refuse_more_than_the_portable_sqlite_bind_and_byte_budgets() {
        assert_eq!(
            ensure_sqlite_bind_capacity(0, ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1),
            Ok(())
        );
        assert_eq!(
            ensure_sqlite_bind_capacity(0, ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 + 1),
            Err(RetrievalPortError::BudgetExceeded)
        );
        let per_term_bytes =
            ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1 / ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 + 1;
        let terms = (0..ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1)
            .map(|term| format!("{term:04}-{}", "x".repeat(per_term_bytes)))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ensure_sqlite_bound_value_bytes(
                ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
                &[],
                terms.iter().map(String::as_str),
            ),
            Err(RetrievalPortError::BudgetExceeded),
            "aggregate bound text must stay within a deterministic byte budget"
        );
    }

    #[test]
    fn bounded_selection_retains_at_most_the_cap_and_matches_a_full_sort() {
        let cap = 7usize;
        let mut ranked = BinaryHeap::new();
        let mut all = Vec::new();
        for ordinal in 0..100u32 {
            // Scores collide on purpose so ties fall through to the stable
            // identity component, exactly like the lane's canonical order.
            let entry = (
                Reverse(u64::from((ordinal * 37) % 11)),
                format!("chunk.{:03}", (ordinal * 53) % 100),
                ordinal,
            );
            all.push(entry.clone());
            retain_bounded(&mut ranked, cap, entry);
            assert!(
                ranked.len() <= cap,
                "bounded selection retained {} entries over the {cap}-entry cap",
                ranked.len()
            );
        }
        let selected = ranked.into_sorted_vec();
        all.sort();
        all.truncate(cap);
        assert_eq!(
            selected, all,
            "bounded selection must equal a full sort truncated to the cap"
        );
    }

    #[test]
    fn fuzzy_vocabulary_is_one_unsorted_pass_over_the_term_keyed_table() {
        assert!(
            !super::ArtifactQueryV1::VOCABULARY_SQL
                .to_ascii_uppercase()
                .contains("ORDER BY"),
            "the in-fuzzy vocabulary load needs no order"
        );
        let connection = Connection::open_in_memory().expect("vocabulary plan db");
        connection
            .execute_batch(
                "CREATE TABLE term_postings (
                    term TEXT NOT NULL PRIMARY KEY,
                    in_fuzzy INTEGER NOT NULL,
                    lists BLOB NOT NULL
                ) WITHOUT ROWID;
                INSERT INTO term_postings(term, in_fuzzy, lists) VALUES ('alpha', 1, x'00'), ('beta', 0, x'00');",
            )
            .expect("seed vocabulary");
        let sql = format!(
            "EXPLAIN QUERY PLAN {}",
            super::ArtifactQueryV1::VOCABULARY_SQL
        );
        let plan = connection
            .prepare(&sql)
            .expect("prepare vocab plan")
            .query_map([], |row| row.get::<_, String>(3))
            .expect("query vocab plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect vocab plan");
        assert!(
            plan.iter()
                .any(|detail| detail.contains("SCAN term_postings")),
            "in-fuzzy load must be one table scan, got {plan:?}"
        );
    }
}
