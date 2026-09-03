#[cfg(test)]
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;
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
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, CodeSearchChunkId, CompactCandidate,
    ComponentRevision, EvidenceRole, ExactAdmissionProof, ExactFieldV1, ExactTechnicalTermKindV1,
    FixedPointScore, LogicalEvidenceId, ManifestDigest, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceOccurrenceId,
};
use tracedecay_private_fs::open_private_file;

use super::builder::compute_section_digests;
use super::format::{
    ArtifactRowV1, CodeLexicalArtifactOccurrenceV1, CodeLexicalImportMembershipWitnessV1,
    VerifiedCodeLexicalArtifactV1, artifact_digest, decode_ngram_bitmap, decode_padded_receipt,
    encode_exact_field, encode_field, metadata_digest, verify_required_artifact_indexes,
};
use super::postings::{NGRAM_NORMALIZED, NGRAM_RAW_OVERRIDE, query_ngrams};
use super::row_codec::decode_artifact_row;
use super::schema::{
    LexicalArtifactLayoutV1, exact_field_code, field_code, field_from_code, lookup_term_id,
    lookup_term_ids, stable_exact_term_id,
};
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_CACHE_FLOOR_BYTES,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1, checkpoint,
    sqlite_corrupt, sqlite_error,
};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RetrievalPortError, contract_error, lane_candidate_cap,
};

use super::super::{
    ECHO_SCORE_MILLIS, ExactMatchRowViewV1, FUZZY_SCORE_MILLIS, FuzzyExpansionsV1,
    FuzzyQueryGroupV1, LexicalRowScoreV1, LiteralProofCacheV1, PHRASE_SCORE_MILLIS,
    PreparedLexicalQueryV1, add_score, bm25_score_micros, collect_term_kinds, exact_matches,
    field_weight_millis, fuzzy_distance_bound, normalize_lexical, retrieval_anchor,
    substring_count,
};
use crate::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    MAX_FUZZY_TERM_EXPANSIONS_V1, MAX_LEXICAL_QUERY_TERM_BYTES_V1, field_admitted,
};

#[derive(Clone)]
pub struct CodeLexicalArtifactReaderV1 {
    connection: Arc<ArtifactConnectionMutex<Connection>>,
    metadata: super::super::CodeLexicalProjectionMetadataV1,
    receipt: VerifiedCodeLexicalArtifactV1,
    layout: LexicalArtifactLayoutV1,
    retained_owned_bytes: usize,
    /// Fuzzy expansion walks every in-fuzzy term. Hash-ordered `term_id`
    /// rows make a fresh `ORDER BY term` scan random I/O; share one load
    /// across clones and later queries on this reader.
    fuzzy_vocabulary: Arc<OnceLock<Arc<Vec<String>>>>,
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
            .field("generation", self.receipt.generation())
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
            let layout = verify_artifact_state_revision(&connection, control)?;
            verify_required_artifact_indexes(&connection, layout)
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

    fn open_connection_with_control(
        connection: Connection,
        expected: &VerifiedCodeLexicalArtifactV1,
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
            let layout = verify_artifact_state_revision(&connection, control)?;
            verify_required_artifact_indexes(&connection, layout)
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
                let metadata: super::super::CodeLexicalProjectionMetadataV1 =
                    serde_json::from_slice(&stored_metadata_bytes)
                        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
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
        let layout = LexicalArtifactLayoutV1::from_revision(stored.format_revision())?;
        let state_layout = verify_artifact_state_revision(&connection, control)?;
        if layout != state_layout {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact receipt revision does not match artifact state".to_owned(),
            ));
        }
        let decoded_metadata_digest = metadata_digest(&metadata)?;
        if &decoded_metadata_digest != stored.metadata_digest()
            || stored_metadata_digest != stored.metadata_digest().as_str()
            || &metadata.generation != stored.generation()
            || metadata.repository_id.as_ref() != stored.repository_id()
            || &metadata.freshness != stored.freshness()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact metadata digest does not verify".to_owned(),
            ));
        }
        let sections = hotpath::measure_block!(
            "query.artifact.open.section_digest_verify",
            compute_section_digests(&connection, control, layout)
        )?;
        if sections != stored.section_digests() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact section digests do not verify".to_owned(),
            ));
        }
        let digest = hotpath::measure_block!(
            "query.artifact.open.artifact_digest_verify",
            artifact_digest(
                stored.metadata_digest(),
                stored.source_state_digest(),
                stored.source_format_revision(),
                stored.page_count(),
                stored.total_chunks(),
                stored.total_payload_bytes(),
                stored.total_imports(),
                stored.import_payload_bytes(),
                stored.import_dictionary_digest(),
                stored.source_cumulative_digest(),
                &sections,
                stored.format_revision(),
            )
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
            layout,
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
        let row: Option<Vec<u8>> = connection
            .query_row(
                "SELECT row FROM rows WHERE chunk_id = ?1",
                [chunk.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        row.map(|bytes| {
            decode_artifact_row(
                self.layout,
                self.receipt.generation(),
                chunk.as_str(),
                &bytes,
            )
            .map(row_occurrence)
        })
        .transpose()
    }

    pub fn occurrence_by_binding(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<Option<CodeLexicalArtifactOccurrenceV1>, CodeLexicalArtifactErrorV1> {
        if &binding.occurrence.generation != self.receipt.generation() {
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
                "SELECT evidence FROM import_evidence WHERE canonical = ?1",
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
        if generation != self.receipt.generation() {
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
        if self.receipt.freshness().compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::hotpath_metrics::Residency::Rebuilding.record("query.lane.lexical.residency");
            return Ok(RetrieverOutcome::Stale(self.receipt.freshness().clone()));
        }
        let connection = self.lock_connection().map_err(map_query_artifact_error)?;
        let batch = ArtifactQueryV1::new(
            &connection,
            &self.metadata,
            &self.receipt,
            self.layout,
            &self.fuzzy_vocabulary,
        )?
        .lexical_batch(request)?;
        let outcome = RetrieverOutcome::Complete(batch);
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
        if self.reader.receipt.freshness().compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::hotpath_metrics::Residency::Rebuilding.record("query.lane.exact.residency");
            return Ok(RetrieverOutcome::Stale(
                self.reader.receipt.freshness().clone(),
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
            self.reader.layout,
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
    receipt: &'a VerifiedCodeLexicalArtifactV1,
    layout: LexicalArtifactLayoutV1,
    document_count: usize,
    metrics: ArtifactQueryMetricsV1,
    fuzzy_vocabulary: &'a OnceLock<Arc<Vec<String>>>,
}

#[derive(Default)]
struct ArtifactQueryMetricsV1 {
    #[cfg(test)]
    probes: Cell<u64>,
    #[cfg(test)]
    fullscan_steps: Cell<u64>,
    #[cfg(test)]
    ngram_decoded_shards: Cell<u64>,
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
    fn observe_ngram_shard(&self) {
        self.ngram_decoded_shards
            .set(self.ngram_decoded_shards.get().saturating_add(1));
    }

    #[cfg(test)]
    fn observe_ngram_candidates(&self, candidates: u64) {
        self.ngram_peak_candidates
            .set(self.ngram_peak_candidates.get().max(candidates));
    }
}

/// A SQLite-owned candidate set. The query is evaluated row-by-row, so Rust
/// never retains one identifier per matching document. Query input is already
/// bounded by the lexical request contract; n-gram intersections additionally
/// have a fixed predicate ceiling below.
#[derive(Clone, Debug)]
struct DocumentQueryV1 {
    sql: Option<String>,
    parameters: Vec<Value>,
    maximum_bound_value_bytes: usize,
}

impl DocumentQueryV1 {
    fn empty() -> Self {
        Self {
            sql: None,
            parameters: Vec::new(),
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn term(field: String, term: String) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE field = ? AND term = ?".to_owned(),
            ),
            parameters: vec![Value::Text(field), Value::Text(term)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn term_except(term: String, excluded_field: String) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE term = ? AND field != ?".to_owned(),
            ),
            parameters: vec![Value::Text(term), Value::Text(excluded_field)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn exact(field: String, term: Vec<u8>) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM exact_postings WHERE field = ? AND term = ?".to_owned(),
            ),
            parameters: vec![Value::Text(field), Value::Blob(term)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn exact_id(field: ExactFieldV1, term: &[u8]) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM exact_postings WHERE field = ? AND term_id = ?".to_owned(),
            ),
            parameters: vec![
                Value::Integer(exact_field_code(field)),
                Value::Integer(stable_exact_term_id(term)),
            ],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn term_id(field: i64, term_id: i64) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE field = ? AND term_id = ?".to_owned(),
            ),
            parameters: vec![Value::Integer(field), Value::Integer(term_id)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }

    fn term_except_id(term_id: i64, excluded_field: i64) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE term_id = ? AND field != ?".to_owned(),
            ),
            parameters: vec![Value::Integer(term_id), Value::Integer(excluded_field)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        }
    }
}

/// The query engine, rather than an in-process bitmap, owns duplicate removal
/// and sorted candidate enumeration. SQLite's configured fixed page cache is
/// the only storage used for the set-operation work table.
const ARTIFACT_UNION_COMPOUND_ARMS_V1: usize = 64;

fn union_document_queries(
    queries: impl IntoIterator<Item = DocumentQueryV1>,
) -> Result<DocumentQueryV1, RetrievalPortError> {
    let queries = queries
        .into_iter()
        .filter(|query| query.sql.is_some())
        .collect::<Vec<_>>();
    if queries.is_empty() {
        return Ok(DocumentQueryV1::empty());
    }

    // Keep each compound arm below SQLite's expression limit, while sharing
    // equal bind values across arms. A large fuzzy/phrase request commonly
    // repeats its encoded field, so retaining one bind slot for every textual
    // occurrence needlessly crosses the portable 999-variable ceiling even
    // though the request's distinct values remain bounded. Named parameters
    // also remain safe when this query is embedded in the frequency probe.
    let maximum_bound_value_bytes = queries
        .iter()
        .map(|query| query.maximum_bound_value_bytes)
        .max()
        .unwrap_or(ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1);
    let mut parameters = Vec::new();
    let mut level = queries
        .into_iter()
        .map(|query| {
            let Some(sql) = query.sql else {
                return Ok(String::new());
            };
            let sql = rewrite_union_query_parameters(&sql, &query.parameters, &mut parameters)?;
            Ok(format!("SELECT document_id FROM ({sql})"))
        })
        .collect::<Result<Vec<_>, RetrievalPortError>>()?
        .into_iter()
        .filter(|query| !query.is_empty())
        .collect::<Vec<_>>();
    while level.len() > 1 {
        level = level
            .chunks(ARTIFACT_UNION_COMPOUND_ARMS_V1)
            .map(|queries| {
                let sql = queries.join(" UNION ALL ");
                format!("SELECT document_id FROM ({sql})")
            })
            .collect();
    }
    let Some(root) = level.pop() else {
        return Ok(DocumentQueryV1::empty());
    };
    Ok(DocumentQueryV1 {
        sql: Some(format!(
            "SELECT DISTINCT document_id FROM ({root}) ORDER BY document_id"
        )),
        parameters,
        maximum_bound_value_bytes,
    })
}

fn rewrite_union_query_parameters(
    sql: &str,
    query_parameters: &[Value],
    parameters: &mut Vec<Value>,
) -> Result<String, RetrievalPortError> {
    let mut rewritten = String::with_capacity(sql.len());
    let mut parameter_ordinal = 0usize;
    for character in sql.chars() {
        if character != '?' {
            rewritten.push(character);
            continue;
        }
        let Some(value) = query_parameters.get(parameter_ordinal) else {
            return Err(RetrievalPortError::Contract(
                "document query SQL has more bind placeholders than values".to_owned(),
            ));
        };
        let slot = if let Some(slot) = parameters.iter().position(|candidate| candidate == value) {
            slot
        } else {
            parameters.push(value.clone());
            parameters.len() - 1
        };
        rewritten.push_str(":d");
        rewritten.push_str(&slot.to_string());
        parameter_ordinal += 1;
    }
    if parameter_ordinal != query_parameters.len() {
        return Err(RetrievalPortError::Contract(
            "document query has values without bind placeholders".to_owned(),
        ));
    }
    Ok(rewritten)
}

fn visit_document_ids(
    connection: &Connection,
    query: &DocumentQueryV1,
    mut visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    hotpath::measure_block!("query.stream.visit_documents", {
        let Some(sql) = &query.sql else {
            return Ok(());
        };
        ensure_sqlite_bind_capacity(0, query.parameters.len())?;
        ensure_sqlite_bound_value_bytes(
            query.maximum_bound_value_bytes,
            &query.parameters,
            std::iter::empty(),
        )?;
        let mut statement = connection.prepare(sql).map_err(map_query_sql_error)?;
        let mut rows = statement
            .query(params_from_iter(query.parameters.iter()))
            .map_err(map_query_sql_error)?;
        let mut visited = 0u64;
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            let document = row.get::<_, i64>(0).map_err(map_query_sql_error)?;
            visitor(u32::try_from(document).map_err(contract_error)?)?;
            visited += 1;
        }
        hotpath::gauge!("query.stream.rows_total").inc(visited);
        Ok(())
    })
}

/// Stream each candidate row with all request-relevant term frequencies from
/// one SQLite statement. The correlated posting lookup seeks the maintained
/// document index; it never emits the row BLOB once per matching term.
fn visit_lexical_rows(
    connection: &Connection,
    documents: &DocumentQueryV1,
    terms: &BTreeSet<String>,
    metrics: &ArtifactQueryMetricsV1,
    layout: LexicalArtifactLayoutV1,
    mut visitor: impl FnMut(
        u32,
        String,
        Vec<u8>,
        LexicalTermFrequenciesV1,
    ) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    hotpath::measure_block!("query.stream.visit_lexical_rows", {
        let Some(document_sql) = documents.sql.as_deref() else {
            return Ok(());
        };
        let assigned_ids = match layout {
            LexicalArtifactLayoutV1::V10 => BTreeMap::new(),
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                lookup_term_ids(connection, terms).map_err(map_query_artifact_error)?
            }
        };
        let v11_ids = assigned_ids.values().copied().collect::<Vec<_>>();
        let dynamic_binds = match layout {
            LexicalArtifactLayoutV1::V10 => terms.len(),
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => v11_ids.len(),
        };
        ensure_sqlite_bind_capacity(documents.parameters.len(), dynamic_binds)?;
        ensure_sqlite_bound_value_bytes(
            documents.maximum_bound_value_bytes,
            &documents.parameters,
            terms.iter().map(String::as_str),
        )?;
        let mut parameters =
            Vec::with_capacity(documents.parameters.len().saturating_add(dynamic_binds));
        let frequencies = match layout {
            LexicalArtifactLayoutV1::V10 if terms.is_empty() => "'[]'".to_owned(),
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 if v11_ids.is_empty() => {
                "'[]'".to_owned()
            }
            LexicalArtifactLayoutV1::V10 => {
                let placeholders = std::iter::repeat_n("?", terms.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                parameters.extend(terms.iter().cloned().map(Value::Text));
                format!(
                    "COALESCE((SELECT json_group_array(json_array(posting.field, posting.term, posting.frequency)) \
                     FROM term_postings AS posting INDEXED BY term_postings_by_document_term \
                     WHERE posting.document_id = documents.document_id \
                     AND posting.term IN ({placeholders})), '[]')"
                )
            }
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                let placeholders = std::iter::repeat_n("?", v11_ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                parameters.extend(v11_ids.iter().copied().map(Value::Integer));
                format!(
                    "COALESCE((SELECT json_group_array(json_array(posting.field, vocabulary.term, posting.frequency)) \
                     FROM term_postings AS posting INDEXED BY term_postings_by_document \
                     JOIN vocabulary ON vocabulary.term_id = posting.term_id \
                     WHERE posting.document_id = documents.document_id \
                     AND posting.term_id IN ({placeholders})), '[]')"
                )
            }
        };
        // The frequency expression appears in the SELECT list before the
        // document subquery appears in FROM, so its placeholders bind first.
        // Keep the value vector in that exact textual order.
        parameters.extend(documents.parameters.iter().cloned());
        let sql = format!(
            "SELECT documents.document_id, stored.chunk_id, stored.row, {frequencies} \
             FROM ({document_sql}) AS documents \
             JOIN rows AS stored ON stored.document_id = documents.document_id \
             ORDER BY documents.document_id"
        );
        metrics.probe();
        let mut statement = connection.prepare(&sql).map_err(map_query_sql_error)?;
        let mut rows = statement
            .query(params_from_iter(parameters.iter()))
            .map_err(map_query_sql_error)?;
        let mut visited = 0u64;
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            let document = u32::try_from(row.get::<_, i64>(0).map_err(map_query_sql_error)?)
                .map_err(contract_error)?;
            let chunk_id: String = row.get(1).map_err(map_query_sql_error)?;
            let bytes: Vec<u8> = row.get(2).map_err(map_query_sql_error)?;
            let encoded_frequencies: String = row.get(3).map_err(map_query_sql_error)?;
            let mut entries = Vec::new();
            match layout {
                LexicalArtifactLayoutV1::V10 => {
                    let encoded: Vec<(String, String, i64)> =
                        serde_json::from_str(&encoded_frequencies).map_err(contract_error)?;
                    entries.reserve(encoded.len());
                    for (field, term, frequency) in encoded {
                        entries.push((
                            decode_field(&field)?,
                            term,
                            usize::try_from(frequency).map_err(contract_error)?,
                        ));
                    }
                }
                LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                    let encoded: Vec<(i64, String, i64)> =
                        serde_json::from_str(&encoded_frequencies).map_err(contract_error)?;
                    entries.reserve(encoded.len());
                    for (field, term, frequency) in encoded {
                        entries.push((
                            field_from_code(field).map_err(map_query_artifact_error)?,
                            term,
                            usize::try_from(frequency).map_err(contract_error)?,
                        ));
                    }
                }
            }
            visitor(document, chunk_id, bytes, LexicalTermFrequenciesV1(entries))?;
            visited = visited.saturating_add(1);
        }
        drop(rows);
        metrics.observe_statement(&statement)?;
        metrics.rows(visited);
        Ok(())
    })
}

const ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1: usize = 16;

/// SQLite distributions are required to support at least 999 variables. Keep
/// generated statements within that portable ceiling instead of depending on
/// the larger build-time limit of a particular linked SQLite library.
const ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1: usize = 999;
/// Caps request-projected text/blob input, and therefore the largest
/// request-relevant frequency aggregate SQLite can return as one row. Live
/// cancellation belongs above this read-port boundary, so the port keeps each
/// individual SQLite call deterministically bounded instead.
const ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1: usize =
    ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 * MAX_LEXICAL_QUERY_TERM_BYTES_V1;
/// A phrase prefilter may legitimately name more documents than request text
/// can occupy. Keep its one transient JSON1 bridge distinct from the generic
/// query-input bound and below one eighth of the reader cache authority.
const ARTIFACT_NGRAM_CANDIDATE_JSON_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 8;
/// Bitmap queries may inspect only this many source-page shards per port call.
/// The read port carries no execution-control handle, so this fixed work bound
/// is the cancellation/deadline yield authority before control returns to the
/// caller. A 4 KiB work unit leaves authority for blob decode and intersection.
const ARTIFACT_NGRAM_QUERY_MAX_SHARDS_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / (4 * 1024);
/// One encoded source-page shard is retained only while it is decoded and
/// intersected. Keep that transient allocation below one eighth of the cache.
const ARTIFACT_NGRAM_MAX_ENCODED_SHARD_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 8;
/// The synchronous query may inspect at most one quarter of the reader cache
/// in encoded shard bytes across all selected n-grams.
const ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
/// A sparse Roaring candidate can require containers and two-byte values in
/// addition to its identifiers. Eight bytes per admitted identifier is a
/// conservative authority that bounds the first (rarest) full union.
const ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
const ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1: usize = 8;
const ARTIFACT_NGRAM_MAX_CANDIDATES_V1: u64 = (ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1
    / ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1)
    as u64;

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

/// The first fixed number of distinct n-grams forms a selective, bounded
/// prefilter. It may admit a superset for a very long phrase; the row-level
/// substring check remains the correctness authority before scoring.
fn ngram_document_query(
    connection: &Connection,
    layout: LexicalArtifactLayoutV1,
    kind: i64,
    bytes: &[u8],
    metrics: &ArtifactQueryMetricsV1,
) -> Result<DocumentQueryV1, RetrievalPortError> {
    hotpath::measure_block!("query.artifact.ngram.bitmap_query", {
        let ngrams = query_ngrams(bytes)
            .into_iter()
            .take(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1)
            .collect::<Vec<_>>();
        if ngrams.is_empty() {
            return Ok(DocumentQueryV1::empty());
        }
        let candidates = ngram_bitmap_candidates(connection, layout, kind, &ngrams, metrics)?;
        let encoded =
            encode_ngram_candidate_json(&candidates, ARTIFACT_NGRAM_CANDIDATE_JSON_BYTES_V1)?;
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("query.artifact.ngram.query_candidates_total").inc(candidates.len());
        Ok(DocumentQueryV1 {
            sql: Some(
                "SELECT CAST(value AS INTEGER) AS document_id FROM json_each(?) ORDER BY document_id"
                    .to_owned(),
            ),
            parameters: vec![Value::Text(encoded)],
            maximum_bound_value_bytes: ARTIFACT_NGRAM_CANDIDATE_JSON_BYTES_V1,
        })
    })
}

#[derive(Clone, Copy)]
struct NgramSelectivityV1 {
    ngram: u32,
    cardinality: u64,
}

fn ngram_bitmap_candidates(
    connection: &Connection,
    layout: LexicalArtifactLayoutV1,
    kind: i64,
    ngrams: &[u32],
    _metrics: &ArtifactQueryMetricsV1,
) -> Result<RoaringBitmap, RetrievalPortError> {
    let mut remaining_shards = ARTIFACT_NGRAM_QUERY_MAX_SHARDS_V1;
    let mut remaining_encoded_bytes = ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1;
    let mut selectivities = Vec::with_capacity(ngrams.len());
    let mut selectivity_statement = connection
        .prepare_cached(
            "SELECT document_frequency FROM ngram_statistics WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(map_query_sql_error)?;
    for ngram in ngrams {
        let cardinality = selectivity_statement
            .query_row([kind, i64::from(*ngram)], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(map_query_sql_error)?;
        let Some(cardinality) = cardinality else {
            return Ok(RoaringBitmap::new());
        };
        let cardinality = u64::try_from(cardinality).map_err(contract_error)?;
        ensure_ngram_candidate_cardinality(cardinality)?;
        selectivities.push(NgramSelectivityV1 {
            ngram: *ngram,
            cardinality,
        });
    }
    drop(selectivity_statement);
    selectivities.sort_unstable_by_key(|selectivity| (selectivity.cardinality, selectivity.ngram));
    if let Some(selectivity) = selectivities.first() {
        ensure_ngram_candidate_cardinality(selectivity.cardinality)?;
    }

    let mut candidates = None::<BTreeMap<i64, RoaringBitmap>>;
    #[cfg(feature = "hotpath")]
    let mut observed_shards = 0u64;
    #[cfg(feature = "hotpath")]
    let mut observed_bytes = 0u64;
    let mut all_pages_statement = connection
        .prepare_cached(
            "SELECT page_ordinal, documents, cardinality FROM ngram_postings INDEXED BY ngram_postings_by_ngram WHERE kind = ?1 AND ngram = ?2 ORDER BY page_ordinal",
        )
        .map_err(map_query_sql_error)?;
    let mut candidate_pages_statement = connection
        .prepare_cached(
            "SELECT posting.page_ordinal, posting.documents, posting.cardinality \
             FROM json_each(?3) AS candidate_page \
             CROSS JOIN ngram_postings AS posting INDEXED BY ngram_postings_by_ngram \
             WHERE posting.kind = ?1 \
               AND posting.ngram = ?2 \
               AND posting.page_ordinal = CAST(candidate_page.value AS INTEGER)",
        )
        .map_err(map_query_sql_error)?;
    for selectivity in selectivities {
        let next = if let Some(current) = candidates.as_ref() {
            let candidate_pages = encode_ngram_candidate_pages(current)?;
            let mut rows = candidate_pages_statement
                .query((kind, i64::from(selectivity.ngram), candidate_pages))
                .map_err(map_query_sql_error)?;
            let next = intersect_ngram_shards(
                &mut rows,
                Some(current),
                layout,
                &mut remaining_shards,
                &mut remaining_encoded_bytes,
                _metrics,
                #[cfg(feature = "hotpath")]
                &mut observed_shards,
                #[cfg(feature = "hotpath")]
                &mut observed_bytes,
            )?;
            drop(rows);
            _metrics.observe_statement(&candidate_pages_statement)?;
            next
        } else {
            let mut rows = all_pages_statement
                .query([kind, i64::from(selectivity.ngram)])
                .map_err(map_query_sql_error)?;
            let next = intersect_ngram_shards(
                &mut rows,
                None,
                layout,
                &mut remaining_shards,
                &mut remaining_encoded_bytes,
                _metrics,
                #[cfg(feature = "hotpath")]
                &mut observed_shards,
                #[cfg(feature = "hotpath")]
                &mut observed_bytes,
            )?;
            drop(rows);
            _metrics.observe_statement(&all_pages_statement)?;
            next
        };
        candidates = Some(next);
        if candidates.as_ref().is_none_or(BTreeMap::is_empty) {
            break;
        }
    }
    let candidates = candidates.unwrap_or_default().into_values().fold(
        RoaringBitmap::new(),
        |mut all, shard| {
            all |= shard;
            all
        },
    );
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("query.artifact.ngram.query_shards_total").inc(observed_shards);
        hotpath::gauge!("query.artifact.ngram.query_bytes_total").inc(observed_bytes);
    }
    Ok(candidates)
}

fn intersect_ngram_shards(
    rows: &mut rusqlite::Rows<'_>,
    current: Option<&BTreeMap<i64, RoaringBitmap>>,
    layout: LexicalArtifactLayoutV1,
    remaining_shards: &mut usize,
    remaining_encoded_bytes: &mut usize,
    _metrics: &ArtifactQueryMetricsV1,
    #[cfg(feature = "hotpath")] observed_shards: &mut u64,
    #[cfg(feature = "hotpath")] observed_bytes: &mut u64,
) -> Result<BTreeMap<i64, RoaringBitmap>, RetrievalPortError> {
    let mut next = BTreeMap::new();
    let mut candidate_count = 0u64;
    while let Some(row) = rows.next().map_err(map_query_sql_error)? {
        if *remaining_shards == 0 {
            return Err(RetrievalPortError::BudgetExceeded);
        }
        let page_ordinal: i64 = row.get(0).map_err(map_query_sql_error)?;
        let encoded: Vec<u8> = row.get(1).map_err(map_query_sql_error)?;
        let cardinality: i64 = row.get(2).map_err(map_query_sql_error)?;
        charge_ngram_encoded_shard_bytes(
            remaining_encoded_bytes,
            encoded.len(),
            ARTIFACT_NGRAM_MAX_ENCODED_SHARD_BYTES_V1,
        )?;
        *remaining_shards = remaining_shards
            .checked_sub(1)
            .ok_or(RetrievalPortError::BudgetExceeded)?;
        let mut shard = decode_ngram_bitmap(layout, &encoded).map_err(map_query_artifact_error)?;
        if i64::try_from(shard.len()).map_err(contract_error)? != cardinality {
            return Err(RetrievalPortError::Contract(
                "lexical artifact ngram shard cardinality changed after verification".to_owned(),
            ));
        }
        if let Some(current) = current {
            let prior = current.get(&page_ordinal).ok_or_else(|| {
                RetrievalPortError::Contract(
                    "lexical artifact returned an ngram shard outside the candidate pages"
                        .to_owned(),
                )
            })?;
            shard &= prior;
        }
        if !shard.is_empty() {
            candidate_count = candidate_count
                .checked_add(shard.len())
                .ok_or(RetrievalPortError::BudgetExceeded)?;
            ensure_ngram_candidate_cardinality(candidate_count)?;
            #[cfg(test)]
            _metrics.observe_ngram_candidates(candidate_count);
            next.insert(page_ordinal, shard);
        }
        #[cfg(test)]
        _metrics.observe_ngram_shard();
        #[cfg(feature = "hotpath")]
        {
            *observed_shards = observed_shards.saturating_add(1);
            *observed_bytes = observed_bytes.saturating_add(encoded.len() as u64);
        }
    }
    Ok(next)
}

fn encode_ngram_candidate_pages(
    candidates: &BTreeMap<i64, RoaringBitmap>,
) -> Result<String, RetrievalPortError> {
    let mut encoded = String::with_capacity(candidates.len().saturating_mul(8));
    encoded.push('[');
    for (ordinal, page_ordinal) in candidates.keys().enumerate() {
        if ordinal > 0 {
            encoded.push(',');
        }
        write!(&mut encoded, "{page_ordinal}").map_err(|_| RetrievalPortError::BudgetExceeded)?;
        if encoded.len() > ARTIFACT_NGRAM_CANDIDATE_JSON_BYTES_V1 {
            return Err(RetrievalPortError::BudgetExceeded);
        }
    }
    encoded.push(']');
    if encoded.len() > ARTIFACT_NGRAM_CANDIDATE_JSON_BYTES_V1 {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(encoded)
}

fn ensure_ngram_candidate_cardinality(cardinality: u64) -> Result<(), RetrievalPortError> {
    if cardinality > ARTIFACT_NGRAM_MAX_CANDIDATES_V1 {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

fn charge_ngram_encoded_shard_bytes(
    remaining_bytes: &mut usize,
    shard_bytes: usize,
    maximum_shard_bytes: usize,
) -> Result<(), RetrievalPortError> {
    if shard_bytes > maximum_shard_bytes {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    *remaining_bytes = remaining_bytes
        .checked_sub(shard_bytes)
        .ok_or(RetrievalPortError::BudgetExceeded)?;
    Ok(())
}

fn encode_ngram_candidate_json(
    candidates: &RoaringBitmap,
    maximum_bytes: usize,
) -> Result<String, RetrievalPortError> {
    if maximum_bytes < 2 {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    let capacity = usize::try_from(candidates.len())
        .map_err(contract_error)?
        .checked_mul(11)
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or(RetrievalPortError::BudgetExceeded)?
        .min(maximum_bytes);
    let mut encoded = String::with_capacity(capacity);
    encoded.push('[');
    for (ordinal, document) in candidates.iter().enumerate() {
        let digits = if document == 0 {
            1
        } else {
            usize::try_from(document.ilog10()).map_err(contract_error)? + 1
        };
        let additional = digits + usize::from(ordinal != 0);
        if encoded
            .len()
            .checked_add(additional)
            .and_then(|bytes| bytes.checked_add(1))
            .is_none_or(|bytes| bytes > maximum_bytes)
        {
            return Err(RetrievalPortError::BudgetExceeded);
        }
        if ordinal != 0 {
            encoded.push(',');
        }
        write!(&mut encoded, "{document}").map_err(contract_error)?;
    }
    encoded.push(']');
    Ok(encoded)
}

impl<'a> ArtifactQueryV1<'a> {
    fn new(
        connection: &'a Connection,
        metadata: &'a super::super::CodeLexicalProjectionMetadataV1,
        receipt: &'a VerifiedCodeLexicalArtifactV1,
        layout: LexicalArtifactLayoutV1,
        fuzzy_vocabulary: &'a OnceLock<Arc<Vec<String>>>,
    ) -> Result<Self, RetrievalPortError> {
        Ok(Self {
            connection,
            metadata,
            receipt,
            layout,
            document_count: usize::try_from(receipt.total_chunks()).map_err(contract_error)?,
            metrics: ArtifactQueryMetricsV1::default(),
            fuzzy_vocabulary,
        })
    }

    fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverBatch<LexicalLaneEvidence>, RetrievalPortError> {
        let fuzzy = self.fuzzy_expansions(request)?;
        let prepared = PreparedLexicalQueryV1::new(request);
        let terms = lexical_terms(&prepared, &fuzzy);
        let stats = self.lexical_stats(&terms)?;
        let mut phrase_queries = BTreeMap::new();
        for (_, normalized) in &prepared.phrases {
            let query = ngram_document_query(
                self.connection,
                self.layout,
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
        let phrase_documents = union_document_queries(phrase_queries.values().cloned())?;
        visit_lexical_rows(
            self.connection,
            &phrase_documents,
            &BTreeSet::new(),
            &self.metrics,
            self.layout,
            |_, chunk_id, bytes, _| {
                let row =
                    decode_artifact_row(self.layout, self.receipt.generation(), &chunk_id, &bytes)
                        .map_err(map_query_artifact_error)?;
                for (phrase, frequency) in &mut phrase_frequencies {
                    if substring_count(&row.normalized_text, phrase) > 0 {
                        *frequency += 1;
                    }
                }
                Ok(())
            },
        )?;
        let documents = self.lexical_documents(request, &fuzzy, &phrase_queries)?;
        // The scan holds one transient row and retains complete rows only for
        // the cap-bounded winners. That avoids a second winner hydration pass
        // while preserving the same strict materialization ceiling.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut excluded = self.document_count as u64;
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::new();
        visit_lexical_rows(
            self.connection,
            &documents,
            &terms,
            &self.metrics,
            self.layout,
            |document, chunk_id, bytes, frequencies| {
                let row =
                    decode_artifact_row(self.layout, self.receipt.generation(), &chunk_id, &bytes)
                        .map_err(map_query_artifact_error)?;
                let score = self.score_row(
                    &row,
                    &prepared,
                    &fuzzy,
                    &phrase_frequencies,
                    &stats,
                    &frequencies,
                )?;
                let Some(ranking) = admitted_score_micros(&score, &request.field_filters)? else {
                    return Ok(());
                };
                eligible += 1;
                excluded = excluded.saturating_sub(1);
                retain_bounded(
                    &mut ranked,
                    cap,
                    RankedLexicalEntryV1 {
                        key: (Reverse(ranking), row.id.as_str().to_owned(), document),
                        score,
                        row,
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
            let RankedLexicalEntryV1 {
                key: (_, _, _),
                score,
                row,
            } = entry;
            let mut candidate = candidate(
                self.receipt,
                &row,
                RetrieverKind::Lexical,
                self.metadata.lexical_retriever_revision.clone(),
                request.score_domain.clone(),
                None,
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let evidence = LexicalLaneEvidence {
                binding: binding(&row, &candidate, score.matched_kinds),
                field_scores_micros: score.field_scores,
                matched_whole_terms: score.matched_whole_terms,
                matched_subtokens: score.matched_subtokens,
                matched_phrases: score.matched_phrases,
                typo_recovery_applied: score.typo_recovery_applied,
                echo_penalty_applied: score.echo_penalty_applied,
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        Ok(capped_batch(
            self.document_count,
            eligible,
            excluded,
            truncated,
            candidates,
            evidence_by_occurrence,
        ))
    }

    fn exact_batch<A: ExactAdmissionAuthority>(
        &self,
        request: &ExactLaneRequest,
        authority: &A,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
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
        self.visit_documents(&documents, |document| {
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
                RankedExactEntryV1 {
                    key: (
                        Reverse(matched_literals.len()),
                        row.id.as_str().to_owned(),
                        document,
                    ),
                    admitted_ordinal,
                    matched_literals,
                    matched_kinds,
                },
            );
            Ok(())
        })?;
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            let RankedExactEntryV1 {
                key: (_, _, document),
                admitted_ordinal,
                matched_literals,
                matched_kinds,
            } = entry;
            let proof = proofs.admitted_proof(admitted_ordinal)?;
            let matched_literals = matched_literals
                .iter()
                .map(|literal| request.literals[*literal].clone())
                .collect::<Vec<_>>();
            let row = self.row(document)?;
            let mut candidate = candidate(
                self.receipt,
                &row,
                RetrieverKind::ExactLiteral,
                self.metadata.exact_retriever_revision.clone(),
                self.metadata.exact_score_domain.clone(),
                Some(proof.clone()),
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let evidence = ExactLaneEvidence {
                binding: binding(&row, &candidate, matched_kinds),
                matched_literals,
                admission_proof: proof,
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
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
        let mut statement = self
            .connection
            .prepare_cached("SELECT chunk_id, row FROM rows WHERE document_id = ?1")
            .map_err(map_query_sql_error)?;
        let (chunk_id, bytes): (String, Vec<u8>) = statement
            .query_row([i64::from(document)], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(map_query_sql_error)?;
        decode_artifact_row(self.layout, self.receipt.generation(), &chunk_id, &bytes)
            .map_err(map_query_artifact_error)
    }

    #[hotpath::measure(label = "query.lane.lexical.select_documents")]
    fn lexical_documents(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_queries: &BTreeMap<String, DocumentQueryV1>,
    ) -> Result<DocumentQueryV1, RetrievalPortError> {
        let mut sources = Vec::new();
        match self.layout {
            LexicalArtifactLayoutV1::V10 => {
                let subtoken_field =
                    encode_field(LexicalFieldV1::Subtoken).map_err(map_query_artifact_error)?;
                for term in &request.whole_terms {
                    sources.push(DocumentQueryV1::term_except(
                        normalize_lexical(term),
                        subtoken_field.clone(),
                    ));
                    if let Some(expansions) = fuzzy.by_query.get(term) {
                        for expansion in expansions {
                            sources.push(DocumentQueryV1::term_except(
                                expansion.clone(),
                                subtoken_field.clone(),
                            ));
                        }
                    }
                }
                for subtoken in &request.subtokens {
                    sources.push(DocumentQueryV1::term(
                        subtoken_field.clone(),
                        normalize_lexical(subtoken),
                    ));
                }
            }
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                let subtoken_field = field_code(LexicalFieldV1::Subtoken);
                for term in &request.whole_terms {
                    if let Some(term_id) = lookup_term_id(self.connection, &normalize_lexical(term))
                        .map_err(map_query_artifact_error)?
                    {
                        sources.push(DocumentQueryV1::term_except_id(term_id, subtoken_field));
                    }
                    if let Some(expansions) = fuzzy.by_query.get(term) {
                        for expansion in expansions {
                            if let Some(term_id) = lookup_term_id(self.connection, expansion)
                                .map_err(map_query_artifact_error)?
                            {
                                sources
                                    .push(DocumentQueryV1::term_except_id(term_id, subtoken_field));
                            }
                        }
                    }
                }
                for subtoken in &request.subtokens {
                    if let Some(term_id) =
                        lookup_term_id(self.connection, &normalize_lexical(subtoken))
                            .map_err(map_query_artifact_error)?
                    {
                        sources.push(DocumentQueryV1::term_id(subtoken_field, term_id));
                    }
                }
            }
        }
        sources.extend(phrase_queries.values().cloned());
        union_document_queries(sources)
    }

    #[hotpath::measure(label = "query.lane.exact.select_documents")]
    fn exact_documents(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<DocumentQueryV1, RetrievalPortError> {
        let mut sources = Vec::new();
        for literal in &request.literals {
            if matches!(
                literal.field,
                ExactFieldV1::QuotedPhrase
                    | ExactFieldV1::DiagnosticText
                    | ExactFieldV1::CompilerOrRuntimeError
            ) {
                sources.push(ngram_document_query(
                    self.connection,
                    self.layout,
                    NGRAM_NORMALIZED,
                    &literal.original_bytes,
                    &self.metrics,
                )?);
                sources.push(ngram_document_query(
                    self.connection,
                    self.layout,
                    NGRAM_RAW_OVERRIDE,
                    &literal.original_bytes,
                    &self.metrics,
                )?);
            }
            match self.layout {
                LexicalArtifactLayoutV1::V10 | LexicalArtifactLayoutV1::V11 => {
                    let field =
                        encode_exact_field(literal.field).map_err(map_query_artifact_error)?;
                    sources.push(DocumentQueryV1::exact(
                        field,
                        literal.canonical_bytes.clone(),
                    ));
                }
                LexicalArtifactLayoutV1::V12 => {
                    sources.push(DocumentQueryV1::exact_id(
                        literal.field,
                        &literal.canonical_bytes,
                    ));
                }
            }
        }
        union_document_queries(sources)
    }

    fn visit_documents(
        &self,
        query: &DocumentQueryV1,
        visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
    ) -> Result<(), RetrievalPortError> {
        self.metrics.probe();
        visit_document_ids(self.connection, query, visitor)
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

    /// Edit-distance expansion does not need term order. `ORDER BY term`
    /// against hash-keyed `term_id` rows forces the UNIQUE(term) index plus
    /// one random primary-key lookup per vocabulary row.
    fn vocabulary_sql(layout: LexicalArtifactLayoutV1) -> &'static str {
        match layout {
            LexicalArtifactLayoutV1::V10 => "SELECT term FROM vocabulary",
            LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                "SELECT term FROM vocabulary WHERE in_fuzzy = 1"
            }
        }
    }

    fn load_vocabulary_from_sqlite(&self) -> Result<Arc<Vec<String>>, RetrievalPortError> {
        self.metrics.probe();
        let mut statement = self
            .connection
            .prepare_cached(Self::vocabulary_sql(self.layout))
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
            let field = match self.layout {
                LexicalArtifactLayoutV1::V10 => {
                    decode_field(&row.get::<_, String>(0).map_err(map_query_sql_error)?)?
                }
                LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                    field_from_code(row.get::<_, i64>(0).map_err(map_query_sql_error)?)
                        .map_err(map_query_artifact_error)?
                }
            };
            let total: i64 = row.get(1).map_err(map_query_sql_error)?;
            field_totals.insert(field, usize::try_from(total).map_err(contract_error)?);
        }
        drop(rows);
        self.metrics.observe_statement(&statement)?;
        self.metrics
            .rows(u64::try_from(field_totals.len()).map_err(contract_error)?);
        let mut document_frequencies = BTreeMap::<LexicalFieldV1, BTreeMap<String, usize>>::new();
        if !terms.is_empty() {
            match self.layout {
                LexicalArtifactLayoutV1::V10 => {
                    let placeholders = std::iter::repeat_n("?", terms.len())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let query = format!(
                        "SELECT field, term, document_frequency FROM term_stats INDEXED BY term_stats_by_term WHERE term IN ({placeholders})"
                    );
                    self.metrics.probe();
                    let mut statement = self
                        .connection
                        .prepare(&query)
                        .map_err(map_query_sql_error)?;
                    let mut rows = statement
                        .query(params_from_iter(terms.iter()))
                        .map_err(map_query_sql_error)?;
                    let mut observed_rows = 0u64;
                    while let Some(row) = rows.next().map_err(map_query_sql_error)? {
                        let field: String = row.get(0).map_err(map_query_sql_error)?;
                        let term: String = row.get(1).map_err(map_query_sql_error)?;
                        let frequency: i64 = row.get(2).map_err(map_query_sql_error)?;
                        document_frequencies
                            .entry(decode_field(&field)?)
                            .or_default()
                            .insert(term, usize::try_from(frequency).map_err(contract_error)?);
                        observed_rows = observed_rows.saturating_add(1);
                    }
                    drop(rows);
                    self.metrics.observe_statement(&statement)?;
                    self.metrics.rows(observed_rows);
                }
                LexicalArtifactLayoutV1::V11 | LexicalArtifactLayoutV1::V12 => {
                    let assigned = lookup_term_ids(self.connection, terms)
                        .map_err(map_query_artifact_error)?;
                    let term_ids = assigned.values().copied().collect::<Vec<_>>();
                    if !term_ids.is_empty() {
                        let placeholders = std::iter::repeat_n("?", term_ids.len())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let query = format!(
                            "SELECT field, term_id, document_frequency FROM term_stats WHERE term_id IN ({placeholders})"
                        );
                        self.metrics.probe();
                        let mut statement = self
                            .connection
                            .prepare(&query)
                            .map_err(map_query_sql_error)?;
                        let mut rows = statement
                            .query(params_from_iter(term_ids.iter()))
                            .map_err(map_query_sql_error)?;
                        let mut observed_rows = 0u64;
                        let id_to_term = assigned
                            .iter()
                            .map(|(term, id)| (*id, term.clone()))
                            .collect::<BTreeMap<_, _>>();
                        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
                            let field =
                                field_from_code(row.get::<_, i64>(0).map_err(map_query_sql_error)?)
                                    .map_err(map_query_artifact_error)?;
                            let term_id: i64 = row.get(1).map_err(map_query_sql_error)?;
                            let Some(term) = id_to_term.get(&term_id) else {
                                continue;
                            };
                            let frequency: i64 = row.get(2).map_err(map_query_sql_error)?;
                            document_frequencies.entry(field).or_default().insert(
                                term.clone(),
                                usize::try_from(frequency).map_err(contract_error)?,
                            );
                            observed_rows = observed_rows.saturating_add(1);
                        }
                        drop(rows);
                        self.metrics.observe_statement(&statement)?;
                        self.metrics.rows(observed_rows);
                    }
                }
            }
        }
        Ok(LexicalStatsCacheV1 {
            field_totals,
            document_frequencies,
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
    ) -> Result<LexicalRowScoreV1, RetrievalPortError> {
        crate::hotpath_metrics::measure_frequent("query.lane.lexical.score_row", || {
            self.score_row_inner(row, prepared, fuzzy, phrase_frequencies, stats, frequencies)
        })
    }

    fn score_row_inner(
        &self,
        row: &ArtifactRowV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
        stats: &LexicalStatsCacheV1,
        frequencies: &LexicalTermFrequenciesV1,
    ) -> Result<LexicalRowScoreV1, RetrievalPortError> {
        let mut field_scores = BTreeMap::new();
        let mut matched_whole_terms = BTreeSet::new();
        let mut matched_subtokens = BTreeSet::new();
        let mut matched_phrases = BTreeSet::new();
        let mut matched_kinds = BTreeSet::new();
        let mut typo_recovery_applied = false;
        for field in row.field_lengths.keys() {
            if *field != LexicalFieldV1::Subtoken {
                for (query_term, normalized) in &prepared.whole_terms {
                    let exact_tf = term_frequency(frequencies, *field, normalized);
                    if exact_tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, normalized, exact_tf, row, stats),
                        );
                        matched_whole_terms.insert((*query_term).to_owned());
                        collect_term_kinds(&row.exact_terms, normalized, &mut matched_kinds);
                    }
                    if let Some(expansions) = fuzzy.by_query.get(*query_term) {
                        for expansion in expansions {
                            let tf = term_frequency(frequencies, *field, expansion);
                            if tf == 0 {
                                continue;
                            }
                            let score = self
                                .term_score(*field, expansion, tf, row, stats)
                                .saturating_mul(FUZZY_SCORE_MILLIS)
                                / 1_000;
                            add_score(&mut field_scores, *field, score);
                            matched_whole_terms.insert((*query_term).to_owned());
                            typo_recovery_applied = true;
                            collect_term_kinds(&row.exact_terms, expansion, &mut matched_kinds);
                        }
                    }
                }
            } else {
                for (subtoken, normalized) in &prepared.subtokens {
                    let tf = term_frequency(frequencies, *field, normalized);
                    if tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, normalized, tf, row, stats),
                        );
                        matched_subtokens.insert((*subtoken).to_owned());
                    }
                }
            }
        }
        for (phrase, normalized) in &prepared.phrases {
            let tf = substring_count(&row.normalized_text, normalized);
            if tf == 0 {
                continue;
            }
            let field = if row.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
                LexicalFieldV1::PreambleText
            } else {
                LexicalFieldV1::BodyText
            };
            let score = self
                .term_score_with_df(
                    field,
                    tf,
                    row,
                    phrase_frequencies
                        .get(normalized)
                        .copied()
                        .unwrap_or_default(),
                    stats,
                )
                .saturating_mul(PHRASE_SCORE_MILLIS)
                / 1_000;
            add_score(&mut field_scores, field, score);
            matched_phrases.insert((*phrase).to_owned());
        }
        let echo_penalty_applied =
            !prepared.echo_query.is_empty() && prepared.echo_query == row.normalized_text.trim();
        if echo_penalty_applied {
            for score in field_scores.values_mut() {
                *score = score.saturating_mul(ECHO_SCORE_MILLIS) / 1_000;
            }
        }
        Ok(LexicalRowScoreV1 {
            field_scores: field_scores.into_iter().collect(),
            matched_whole_terms: matched_whole_terms.into_iter().collect(),
            matched_subtokens: matched_subtokens.into_iter().collect(),
            matched_phrases: matched_phrases.into_iter().collect(),
            matched_kinds: matched_kinds.into_iter().collect(),
            typo_recovery_applied,
            echo_penalty_applied,
        })
    }

    fn term_score(
        &self,
        field: LexicalFieldV1,
        term: &str,
        term_frequency: usize,
        row: &ArtifactRowV1,
        stats: &LexicalStatsCacheV1,
    ) -> u64 {
        self.term_score_with_df(
            field,
            term_frequency,
            row,
            stats.document_frequency(field, term),
            stats,
        )
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
}

fn candidate(
    receipt: &VerifiedCodeLexicalArtifactV1,
    row: &ArtifactRowV1,
    retriever: RetrieverKind,
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    exact_admission_proof: Option<ExactAdmissionProof>,
) -> Result<CompactCandidate, RetrievalPortError> {
    let lane = retriever.as_str();
    let chunk_id = row.id.as_str();
    let generation = row.anchor.generation_id.as_str();
    let evidence_id = row.anchor.symbol_occurrence_id.as_ref().map_or_else(
        || format!("code-chunk:{chunk_id}"),
        |symbol| format!("code-symbol:{}", symbol.as_str()),
    );
    Ok(CompactCandidate {
        anchor_id: retrieval_anchor(evidence_id.clone())?,
        logical_evidence_id: LogicalEvidenceId::new(evidence_id).map_err(contract_error)?,
        source_occurrence_id: SourceOccurrenceId::new(format!(
            "code-chunk:{generation}:{chunk_id}"
        ))
        .map_err(contract_error)?,
        file_occurrence_id: Some(row.anchor.file_occurrence_id.clone()),
        source_namespace: receipt.freshness().source_namespace.clone(),
        repository_id: receipt.repository_id().cloned(),
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever,
        retriever_revision,
        score_domain,
        raw_score: FixedPointScore::ZERO,
        ordinal_rank: 0,
        exact_admission_proof,
        retriever_evidence_anchor: retrieval_anchor(format!("code-lexical:{lane}:{chunk_id}"))?,
        freshness: receipt.freshness().clone(),
    })
}

fn binding(
    row: &ArtifactRowV1,
    candidate: &CompactCandidate,
    matched_term_kinds: Vec<ExactTechnicalTermKindV1>,
) -> CodeCandidateBindingV1 {
    CodeCandidateBindingV1 {
        candidate_anchor: candidate.anchor_id.clone(),
        occurrence: CodeOccurrenceRefV1 {
            generation: row.anchor.generation_id.clone(),
            file: row.anchor.file_occurrence_id.clone(),
            symbol: row.anchor.symbol_occurrence_id.clone(),
            chunk: Some(row.id.clone()),
        },
        language_descriptor_revision: row.language_descriptor_revision.clone(),
        matched_term_kinds,
        source_occurrence: candidate.source_occurrence_id.clone(),
    }
}

/// One admitted exact candidate retained during bounded selection: the
/// canonical ranking key plus ordinals into the request literals — the
/// admitting literal and every matched literal. Winner materialization
/// resolves the proof from the per-request cache and clones the literals
/// only then. Ordering is by key alone.
struct RankedExactEntryV1 {
    key: (Reverse<usize>, String, u32),
    admitted_ordinal: usize,
    matched_literals: Vec<usize>,
    matched_kinds: Vec<ExactTechnicalTermKindV1>,
}

/// One lexical winner retained during bounded selection. Carrying its decoded
/// row and score avoids both winner rehydration and score recomputation.
struct RankedLexicalEntryV1 {
    key: (Reverse<u64>, String, u32),
    score: LexicalRowScoreV1,
    row: ArtifactRowV1,
}

impl PartialEq for RankedLexicalEntryV1 {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for RankedLexicalEntryV1 {}

impl PartialOrd for RankedLexicalEntryV1 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedLexicalEntryV1 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl PartialEq for RankedExactEntryV1 {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for RankedExactEntryV1 {}

impl PartialOrd for RankedExactEntryV1 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedExactEntryV1 {
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
/// scores, or `None` when the typed field filters admit no scored field —
/// the same exclusion the lexical lane applies after the port returns.
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

fn decode_field(encoded: &str) -> Result<LexicalFieldV1, RetrievalPortError> {
    serde_json::from_str(encoded).map_err(contract_error)
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
) -> Result<LexicalArtifactLayoutV1, CodeLexicalArtifactErrorV1> {
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
    let layout = LexicalArtifactLayoutV1::from_revision(revision)?;
    checkpoint(control)?;
    Ok(layout)
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
    #[cfg(feature = "hotpath")]
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use roaring::RoaringBitmap;
    use rusqlite::hooks::{AuthAction, Authorization};
    use rusqlite::{Connection, OpenFlags, params};
    use sha2::{Digest, Sha256};
    use tracedecay_domain::ManifestDigest;
    use tracedecay_private_fs::open_private_file;

    use super::super::format::encode_ngram_bitmap;
    #[cfg(feature = "hotpath")]
    use super::ArtifactConnectionMutex;
    use super::{
        ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1, ARTIFACT_NGRAM_MAX_CANDIDATES_V1,
        ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1,
        ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1, ArtifactQueryMetricsV1,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1,
        CodeLexicalArtifactReaderV1, DocumentQueryV1, LexicalArtifactLayoutV1, NGRAM_NORMALIZED,
        charge_ngram_encoded_shard_bytes, configure_reader_window, encode_ngram_candidate_json,
        ensure_ngram_candidate_cardinality, map_query_artifact_error, ngram_bitmap_candidates,
        ngram_document_query, query_ngrams, retain_bounded, term_frequency, union_document_queries,
        visit_document_ids, visit_lexical_rows,
    };
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

    fn streamed_documents(connection: &Connection, query: &DocumentQueryV1) -> Vec<u32> {
        let mut documents = Vec::new();
        visit_document_ids(connection, query, |document| {
            documents.push(document);
            Ok(())
        })
        .expect("SQLite stream succeeds");
        documents
    }

    #[cfg(feature = "hotpath")]
    #[test]
    fn repeated_feature_on_reader_connections_use_plain_mutexes_and_preserve_queries() {
        for expected in 0..16i64 {
            let connection: ArtifactConnectionMutex<Connection> =
                StdMutex::new(Connection::open_in_memory().expect("in-memory SQLite"));

            let value = connection
                .lock()
                .expect("reader connection lock")
                .query_row("SELECT ?1", [expected], |row| row.get::<_, i64>(0))
                .expect("query through reader connection lock");

            assert_eq!(value, expected);
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

    #[test]
    fn phrase_ngram_stream_intersects_a_fixed_number_of_predicates() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE ngram_postings (
                    page_ordinal INTEGER NOT NULL,
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    documents BLOB NOT NULL,
                    cardinality INTEGER NOT NULL,
                    PRIMARY KEY(page_ordinal, kind, ngram)
                ) WITHOUT ROWID;
                CREATE UNIQUE INDEX ngram_postings_by_ngram ON ngram_postings(kind, ngram, page_ordinal, cardinality);
                CREATE TABLE ngram_statistics (
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    document_frequency INTEGER NOT NULL,
                    PRIMARY KEY(kind, ngram)
                ) WITHOUT ROWID;",
            )
            .expect("ngram fixture schema");
        let phrase = b"abcdefghijklmnopqrstuvw";
        let ngrams = query_ngrams(phrase)
            .into_iter()
            .take(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1)
            .collect::<Vec<_>>();
        assert_eq!(ngrams.len(), ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1);
        for (ordinal, ngram) in ngrams.iter().enumerate() {
            let documents = if ordinal + 1 < ngrams.len() {
                RoaringBitmap::from_iter([1, 2])
            } else {
                RoaringBitmap::from_iter([1])
            };
            let encoded = encode_ngram_bitmap(LexicalArtifactLayoutV1::V11, &documents)
                .expect("encode ngram shard");
            connection
                .execute(
                    "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (0, ?1, ?2, ?3, ?4)",
                    params![NGRAM_NORMALIZED, i64::from(*ngram), encoded, documents.len() as i64],
                )
                .expect("complete phrase posting");
            connection
                .execute(
                    "INSERT INTO ngram_statistics(kind, ngram, document_frequency) VALUES (?1, ?2, ?3)",
                    params![NGRAM_NORMALIZED, i64::from(*ngram), documents.len() as i64],
                )
                .expect("complete phrase statistics");
        }

        let metrics = ArtifactQueryMetricsV1::default();
        let query = ngram_document_query(
            &connection,
            LexicalArtifactLayoutV1::V11,
            NGRAM_NORMALIZED,
            phrase,
            &metrics,
        )
        .expect("build ngram bitmap query");

        assert_eq!(query.parameters.len(), 1);
        assert_eq!(streamed_documents(&connection, &query), vec![1]);
    }

    #[test]
    fn ngram_bitmap_query_processes_rare_shards_first_and_short_circuits_common_work() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE ngram_postings (
                    page_ordinal INTEGER NOT NULL,
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    documents BLOB NOT NULL,
                    cardinality INTEGER NOT NULL,
                    PRIMARY KEY(page_ordinal, kind, ngram)
                ) WITHOUT ROWID;
                CREATE UNIQUE INDEX ngram_postings_by_ngram ON ngram_postings(kind, ngram, page_ordinal, cardinality);
                CREATE TABLE ngram_statistics (
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    document_frequency INTEGER NOT NULL,
                    PRIMARY KEY(kind, ngram)
                ) WITHOUT ROWID;",
            )
            .expect("ngram fixture schema");
        for (page_ordinal, ngram, documents) in [
            (0i64, 10u32, vec![1u32, 2]),
            (1, 10, vec![3, 4]),
            (2, 10, vec![5, 6]),
            (0, 20, vec![2]),
            (1, 30, vec![3]),
        ] {
            let bitmap = RoaringBitmap::from_iter(documents);
            let encoded = encode_ngram_bitmap(LexicalArtifactLayoutV1::V11, &bitmap)
                .expect("encode ngram shard");
            connection
                .execute(
                    "INSERT INTO ngram_postings(page_ordinal, kind, ngram, documents, cardinality) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![page_ordinal, NGRAM_NORMALIZED, i64::from(ngram), encoded, bitmap.len() as i64],
                )
                .expect("seed ngram shard");
        }
        for (ngram, document_frequency) in [(10i64, 6i64), (20, 1), (30, 1)] {
            connection
                .execute(
                    "INSERT INTO ngram_statistics(kind, ngram, document_frequency) VALUES (?1, ?2, ?3)",
                    params![NGRAM_NORMALIZED, ngram, document_frequency],
                )
                .expect("seed ngram statistics");
        }

        let bounded_metrics = ArtifactQueryMetricsV1::default();
        let matching = ngram_bitmap_candidates(
            &connection,
            LexicalArtifactLayoutV1::V11,
            NGRAM_NORMALIZED,
            &[10, 20],
            &bounded_metrics,
        )
        .expect("intersect common and rare shards");
        assert_eq!(matching.iter().collect::<Vec<_>>(), [2]);
        assert_eq!(bounded_metrics.ngram_peak_candidates.get(), 1);
        assert_eq!(bounded_metrics.ngram_decoded_shards.get(), 2);
        assert_eq!(bounded_metrics.observed_fullscan_steps(), 0);

        let short_circuit_metrics = ArtifactQueryMetricsV1::default();
        let empty = ngram_bitmap_candidates(
            &connection,
            LexicalArtifactLayoutV1::V11,
            NGRAM_NORMALIZED,
            &[10, 20, 30],
            &short_circuit_metrics,
        )
        .expect("short-circuit disjoint rare shards");
        assert!(empty.is_empty());
        assert_eq!(short_circuit_metrics.ngram_peak_candidates.get(), 1);
        assert_eq!(
            short_circuit_metrics.ngram_decoded_shards.get(),
            1,
            "candidate-page pruning must avoid decoding unrelated ngram shards"
        );
    }

    #[test]
    fn ngram_candidate_json_honors_its_distinct_transient_byte_authority() {
        let candidates = RoaringBitmap::from_iter([1, 20, 300]);
        let exact = "[1,20,300]";
        assert_eq!(
            encode_ngram_candidate_json(&candidates, exact.len())
                .expect("exact candidate JSON boundary"),
            exact
        );
        assert_eq!(
            encode_ngram_candidate_json(&candidates, exact.len() - 1),
            Err(crate::retrieval::ports::RetrievalPortError::BudgetExceeded)
        );
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
    fn ngram_query_rejects_cumulative_encoded_shards_past_its_authority() {
        let mut remaining = 40usize;
        for _ in 0..8 {
            charge_ngram_encoded_shard_bytes(&mut remaining, 5, 5)
                .expect("individually valid encoded shard");
        }
        assert_eq!(remaining, 0);
        assert_eq!(
            charge_ngram_encoded_shard_bytes(&mut remaining, 1, 5),
            Err(crate::retrieval::ports::RetrievalPortError::BudgetExceeded)
        );
        assert_eq!(
            remaining, 0,
            "a refused shard must not consume the retained query authority"
        );
    }

    #[test]
    fn streamed_union_preserves_phrase_and_fuzzy_candidate_membership() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE term_postings (
                    field TEXT NOT NULL,
                    term TEXT NOT NULL,
                    document_id INTEGER NOT NULL
                );",
            )
            .expect("term fixture schema");
        for (field, term, document) in [
            ("body", "render", 1),
            ("body", "renderer", 2),
            ("subtoken", "render", 3),
            ("subtoken", "render", 3),
        ] {
            connection
                .execute(
                    "INSERT INTO term_postings(field, term, document_id) VALUES (?1, ?2, ?3)",
                    params![field, term, document],
                )
                .expect("term posting");
        }
        let query = union_document_queries([
            DocumentQueryV1::term_except("render".to_owned(), "subtoken".to_owned()),
            DocumentQueryV1::term_except("renderer".to_owned(), "subtoken".to_owned()),
            DocumentQueryV1::term("subtoken".to_owned(), "render".to_owned()),
        ])
        .expect("small document union");

        assert_eq!(streamed_documents(&connection, &query), vec![1, 2, 3]);
        assert_eq!(
            streamed_documents(
                &connection,
                &union_document_queries([DocumentQueryV1::term(
                    "subtoken".to_owned(),
                    "render".to_owned(),
                )])
                .expect("single document union"),
            ),
            vec![3],
            "one source query must preserve bitmap-like candidate deduplication"
        );
    }

    #[test]
    fn streamed_union_handles_more_sources_than_sqlite_compound_limit() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE term_postings (
                    field TEXT NOT NULL,
                    term TEXT NOT NULL,
                    document_id INTEGER NOT NULL
                );",
            )
            .expect("term fixture schema");
        let source_count = 513u32;
        let sources = (0..source_count)
            .map(|document| {
                let term = format!("term-{document:04}");
                connection
                    .execute(
                        "INSERT INTO term_postings(field, term, document_id) VALUES (?1, ?2, ?3)",
                        params!["body", term, i64::from(document)],
                    )
                    .expect("term posting");
                DocumentQueryV1::term("body".to_owned(), term)
            })
            .collect::<Vec<_>>();

        let query = union_document_queries(sources).expect("large document union");
        assert_eq!(
            query.parameters.len(),
            source_count as usize + 1,
            "repeated field binds share one bounded SQLite parameter slot"
        );

        assert_eq!(
            streamed_documents(&connection, &query),
            (0..source_count).collect::<Vec<_>>(),
            "nested streamed enumeration preserves the exact candidate order beyond SQLite's flat UNION ceiling"
        );
    }

    #[test]
    fn lexical_row_stream_batches_term_frequencies_in_one_indexed_probe_at_scale() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch(
                "CREATE TABLE rows (
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
                CREATE INDEX term_postings_by_document_term
                    ON term_postings(document_id, term, field, frequency);",
            )
            .expect("lexical row fixture schema");
        let field =
            super::encode_field(super::LexicalFieldV1::BodyText).expect("encoded lexical field");
        for document in 0..2_048i64 {
            connection
                .execute(
                    "INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, ?2, ?3)",
                    params![
                        document,
                        format!("chunk.{document}"),
                        (document as u32).to_le_bytes().as_slice()
                    ],
                )
                .expect("artifact row");
            connection
                .execute(
                    "INSERT INTO term_postings(field, term, document_id, frequency) VALUES (?1, 'alpha', ?2, ?3)",
                    params![field, document, document % 3 + 1],
                )
                .expect("matching posting");
            connection
                .execute(
                    "INSERT INTO term_postings(field, term, document_id, frequency) VALUES (?1, 'irrelevant', ?2, 99)",
                    params![field, document],
                )
                .expect("irrelevant posting");
        }
        let documents = DocumentQueryV1::term(field.clone(), "alpha".to_owned());
        let mut terms = (0..250)
            .map(|term| format!("absent-{term}"))
            .collect::<BTreeSet<_>>();
        terms.insert("alpha".to_owned());
        terms.insert("beta".to_owned());
        let metrics = ArtifactQueryMetricsV1::default();
        let mut visited = 0usize;

        visit_lexical_rows(
            &connection,
            &documents,
            &terms,
            &metrics,
            LexicalArtifactLayoutV1::V10,
            |document, _chunk_id, row, frequencies| {
                assert_eq!(row, document.to_le_bytes());
                assert_eq!(
                    term_frequency(&frequencies, super::LexicalFieldV1::BodyText, "alpha"),
                    usize::try_from(document).unwrap() % 3 + 1
                );
                assert_eq!(
                    term_frequency(&frequencies, super::LexicalFieldV1::BodyText, "beta"),
                    0,
                    "absent postings stay exact zeroes"
                );
                assert!(
                    term_frequency(&frequencies, super::LexicalFieldV1::BodyText, "irrelevant")
                        == 0,
                    "the batched probe must not hydrate unrelated terms"
                );
                visited += 1;
                Ok(())
            },
        )
        .expect("batched lexical row stream");

        assert_eq!(visited, 2_048);
        assert_eq!(
            metrics.probes(),
            1,
            "statement count must stay constant as documents and terms grow"
        );
        assert_eq!(
            metrics.observed_fullscan_steps(),
            0,
            "candidate and frequency lookups must stay on maintained indexes at scale"
        );
    }

    #[test]
    fn lexical_row_stream_refuses_more_than_the_portable_sqlite_bind_budget() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        let documents = DocumentQueryV1 {
            sql: Some("SELECT ? AS document_id".to_owned()),
            parameters: vec![rusqlite::types::Value::Integer(1)],
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        };
        let terms = (0..ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1)
            .map(|term| format!("term-{term}"))
            .collect::<BTreeSet<_>>();

        let error = visit_lexical_rows(
            &connection,
            &documents,
            &terms,
            &ArtifactQueryMetricsV1::default(),
            LexicalArtifactLayoutV1::V10,
            |_, _, _, _| Ok(()),
        )
        .expect_err("combined document and term binds must be request-bounded");

        assert_eq!(
            error,
            crate::retrieval::ports::RetrievalPortError::BudgetExceeded
        );
    }

    #[test]
    fn lexical_row_stream_refuses_aggregate_bound_text_over_budget() {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        let documents = DocumentQueryV1 {
            sql: Some("SELECT 1 AS document_id".to_owned()),
            parameters: Vec::new(),
            maximum_bound_value_bytes: ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
        };
        let per_term_bytes =
            ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1 / ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 + 1;
        let terms = (0..ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1)
            .map(|term| format!("{term:04}-{}", "x".repeat(per_term_bytes)))
            .collect::<BTreeSet<_>>();

        let error = visit_lexical_rows(
            &connection,
            &documents,
            &terms,
            &ArtifactQueryMetricsV1::default(),
            LexicalArtifactLayoutV1::V10,
            |_, _, _, _| Ok(()),
        )
        .expect_err("aggregate bound text must stay within a deterministic byte budget");

        assert_eq!(
            error,
            crate::retrieval::ports::RetrievalPortError::BudgetExceeded
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
    fn interned_v11_frequency_probe_matches_text_v10_and_uses_document_index() {
        let v10 = Connection::open_in_memory().expect("v10 fixture");
        v10.execute_batch(
            "CREATE TABLE rows (
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
            CREATE INDEX term_postings_by_document_term
                ON term_postings(document_id, term, field, frequency);",
        )
        .expect("v10 schema");
        let v11 = Connection::open_in_memory().expect("v11 fixture");
        v11.execute_batch(
            "CREATE TABLE rows (
                document_id INTEGER PRIMARY KEY,
                chunk_id TEXT NOT NULL,
                row BLOB NOT NULL
            );
            CREATE TABLE vocabulary (
                term_id INTEGER PRIMARY KEY,
                term TEXT NOT NULL UNIQUE,
                in_fuzzy INTEGER NOT NULL
            );
            CREATE TABLE term_postings (
                term_id INTEGER NOT NULL,
                field INTEGER NOT NULL,
                document_id INTEGER NOT NULL,
                frequency INTEGER NOT NULL,
                PRIMARY KEY(term_id, field, document_id)
            ) WITHOUT ROWID;
            CREATE INDEX term_postings_by_document
                ON term_postings(document_id, term_id, field, frequency);",
        )
        .expect("v11 schema");
        let field = super::encode_field(super::LexicalFieldV1::BodyText).expect("field");
        v11.execute(
            "INSERT INTO vocabulary(term_id, term, in_fuzzy) VALUES (1, 'alpha', 1), (2, 'beta', 1)",
            [],
        )
        .expect("intern terms");
        for document in 0..32i64 {
            let row = (document as u32).to_le_bytes();
            let chunk = format!("chunk.{document}");
            v10.execute(
                "INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, ?2, ?3)",
                params![document, &chunk, row.as_slice()],
            )
            .expect("v10 row");
            v11.execute(
                "INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, ?2, ?3)",
                params![document, &chunk, row.as_slice()],
            )
            .expect("v11 row");
            v10.execute(
                "INSERT INTO term_postings(field, term, document_id, frequency) VALUES (?1, 'alpha', ?2, ?3)",
                params![field, document, document % 3 + 1],
            )
            .expect("v10 posting");
            v11.execute(
                "INSERT INTO term_postings(term_id, field, document_id, frequency) VALUES (1, 4, ?1, ?2)",
                params![document, document % 3 + 1],
            )
            .expect("v11 posting");
        }
        let v10_query = DocumentQueryV1::term(field, "alpha".to_owned());
        let v11_query = DocumentQueryV1::term_id(4, 1);
        let terms = BTreeSet::from(["alpha".to_owned(), "beta".to_owned()]);
        let mut v10_hits = Vec::new();
        visit_lexical_rows(
            &v10,
            &v10_query,
            &terms,
            &ArtifactQueryMetricsV1::default(),
            LexicalArtifactLayoutV1::V10,
            |document, _, _, frequencies| {
                v10_hits.push((
                    document,
                    term_frequency(&frequencies, super::LexicalFieldV1::BodyText, "alpha"),
                ));
                Ok(())
            },
        )
        .expect("v10 stream");
        let mut v11_hits = Vec::new();
        visit_lexical_rows(
            &v11,
            &v11_query,
            &terms,
            &ArtifactQueryMetricsV1::default(),
            LexicalArtifactLayoutV1::V11,
            |document, _, _, frequencies| {
                v11_hits.push((
                    document,
                    term_frequency(&frequencies, super::LexicalFieldV1::BodyText, "alpha"),
                ));
                Ok(())
            },
        )
        .expect("v11 stream");
        assert_eq!(v10_hits, v11_hits);
        let plan = v11
            .prepare(
                "EXPLAIN QUERY PLAN SELECT document_id FROM term_postings WHERE field = ? AND term_id = ?",
            )
            .expect("prepare term equality plan")
            .query_map(params![4i64, 1i64], |row| row.get::<_, String>(3))
            .expect("query term equality plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect term equality plan");
        assert!(
            plan.iter().any(|detail| {
                detail.contains("PRIMARY KEY") || detail.contains("term_postings")
            }),
            "interned term equality must use the clustered key, got {plan:?}"
        );
        let frequency_plan = v11
            .prepare(
                "EXPLAIN QUERY PLAN SELECT posting.frequency \
                 FROM term_postings AS posting INDEXED BY term_postings_by_document \
                 WHERE posting.document_id = 0 AND posting.term_id IN (1)",
            )
            .expect("prepare frequency plan")
            .query_map([], |row| row.get::<_, String>(3))
            .expect("query frequency plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect frequency plan");
        assert!(
            frequency_plan
                .iter()
                .any(|detail| detail.contains("term_postings_by_document")),
            "frequency probe must use the document-leading index, got {frequency_plan:?}"
        );
    }

    #[test]
    fn fuzzy_vocabulary_sql_does_not_order_hash_keyed_rows() {
        assert!(
            !super::ArtifactQueryV1::vocabulary_sql(LexicalArtifactLayoutV1::V11)
                .to_ascii_uppercase()
                .contains("ORDER BY"),
            "in-fuzzy vocabulary load must not sort hash-keyed term_id rows"
        );
        let v11 = Connection::open_in_memory().expect("v11 vocab plan db");
        v11.execute_batch(
            "CREATE TABLE vocabulary (
                term_id INTEGER PRIMARY KEY,
                term TEXT NOT NULL UNIQUE,
                in_fuzzy INTEGER NOT NULL
            );
            INSERT INTO vocabulary(term_id, term, in_fuzzy) VALUES (1, 'alpha', 1), (2, 'beta', 0);",
        )
        .expect("seed vocabulary");
        let sql = format!(
            "EXPLAIN QUERY PLAN {}",
            super::ArtifactQueryV1::vocabulary_sql(LexicalArtifactLayoutV1::V11)
        );
        let plan = v11
            .prepare(&sql)
            .expect("prepare vocab plan")
            .query_map([], |row| row.get::<_, String>(3))
            .expect("query vocab plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect vocab plan");
        assert!(
            plan.iter().any(|detail| detail.contains("SCAN vocabulary")
                && !detail.contains("sqlite_autoindex_vocabulary_1")),
            "in-fuzzy load must table-scan, not bounce through UNIQUE(term), got {plan:?}"
        );
    }
}
