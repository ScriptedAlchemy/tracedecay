use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, CodeSearchChunkId, CompactCandidate,
    ComponentRevision, EvidenceRole, ExactAdmissionProof, ExactFieldV1, ExactTechnicalTermKindV1,
    FixedPointScore, LogicalEvidenceId, ManifestDigest, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceOccurrenceId,
};

use super::builder::compute_section_digests;
use super::format::{
    ArtifactRowV1, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, CodeLexicalArtifactMountMetadataV1,
    CodeLexicalArtifactOccurrenceV1, CodeLexicalImportMembershipWitnessV1,
    VerifiedCodeLexicalArtifactV1, artifact_digest, decode_padded_receipt, encode_field,
    mount_metadata_digest,
};
use super::postings::{NGRAM_NORMALIZED, NGRAM_RAW_OVERRIDE, query_ngrams};
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
    ECHO_SCORE_MILLIS, FUZZY_SCORE_MILLIS, FuzzyExpansionsV1, FuzzyQueryGroupV1, LexicalRowScoreV1,
    PHRASE_SCORE_MILLIS, add_score, bm25_score_micros, collect_term_kinds_fields,
    exact_matches_fields, field_weight_millis, fuzzy_distance_bound, normalize_lexical,
    retrieval_anchor, substring_count,
};
use crate::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    MAX_FUZZY_TERM_EXPANSIONS_V1, field_admitted,
};

/// Corpus-independent verifier and decoded mount-header reservation.
/// Admission of this fixed window and the SQLite cache happens before the
/// path is touched, so a denied mount cannot open or allocate from an
/// unreserved artifact.
const ARTIFACT_READER_METADATA_RESERVATION_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub struct CodeLexicalArtifactReaderV1 {
    connection: Arc<Mutex<Connection>>,
    metadata: CodeLexicalArtifactMountMetadataV1,
    receipt: VerifiedCodeLexicalArtifactV1,
    retained_owned_bytes: usize,
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
    pub fn open(
        path: impl AsRef<Path>,
        expected: &VerifiedCodeLexicalArtifactV1,
        cache_budget_bytes: usize,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::open_with_control(path, expected, cache_budget_bytes, &NeverInterrupted)
    }

    /// Open a published artifact whose trust anchor is its content address:
    /// the durable head names the artifact file's size and SHA-256 digest,
    /// the embedded receipt is decoded only after the whole file matches
    /// that digest, and the standard receipt-bound verification then runs
    /// unchanged. This is the reopen path for a durable text head that
    /// survived a daemon restart.
    pub fn open_content_addressed(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        let page_cache_bytes = validate_reader_reservation(cache_budget_bytes)?;
        let path = path.as_ref();
        let file_size = path
            .metadata()
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
            .len();
        if file_size != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                "artifact file has {file_size} bytes; the durable head names {expected_file_size_bytes}"
            )));
        }
        let mut file = std::fs::File::open(path)
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 1 << 16];
        loop {
            checkpoint(control)?;
            let read = file
                .read(&mut buffer)
                .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let digest = ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        if &digest != expected_file_digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact file bytes do not match the durable head digest".to_owned(),
            ));
        }
        let receipt = {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(sqlite_error)?;
            connection
                .pragma_update(None, "query_only", true)
                .map_err(sqlite_error)?;
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
            })?
        };
        if receipt.file_size_bytes() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt disagrees with the durable head file size".to_owned(),
            ));
        }
        Self::open_reserved(
            path,
            &receipt,
            cache_budget_bytes,
            page_cache_bytes,
            control,
        )
    }

    pub fn open_with_control(
        path: impl AsRef<Path>,
        expected: &VerifiedCodeLexicalArtifactV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        let page_cache_bytes = validate_reader_reservation(cache_budget_bytes)?;
        Self::open_reserved(
            path,
            expected,
            cache_budget_bytes,
            page_cache_bytes,
            control,
        )
    }

    fn open_reserved(
        path: impl AsRef<Path>,
        expected: &VerifiedCodeLexicalArtifactV1,
        cache_budget_bytes: usize,
        page_cache_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let path = path.as_ref();
        let file_size = path
            .metadata()
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
            .len();
        if file_size != expected.file_size_bytes() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                "artifact file has {file_size} bytes; receipt binds {}",
                expected.file_size_bytes()
            )));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sqlite_error)?;
        checkpoint(control)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(sqlite_error)?;
        let receipt_bytes: Vec<u8> = connection
            .query_row(
                "SELECT receipt FROM artifact_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let stored = decode_padded_receipt(&receipt_bytes)?.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact has no finalized receipt".to_owned(),
            )
        })?;
        if stored != *expected {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact receipt does not match its verified seat".to_owned(),
            ));
        }
        if stored.format_revision() != CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1 {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
                "format revision {} is unsupported",
                stored.format_revision()
            )));
        }
        let (stored_metadata_bytes, stored_metadata_digest): (Vec<u8>, String) = connection
            .query_row(
                "SELECT metadata, metadata_digest FROM artifact_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if stored_metadata_bytes.len() > ARTIFACT_READER_METADATA_RESERVATION_BYTES {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact mount metadata exceeds its reserved verifier window".to_owned(),
            ));
        }
        let metadata: CodeLexicalArtifactMountMetadataV1 =
            serde_json::from_slice(&stored_metadata_bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let metadata_owned_bytes = mount_metadata_owned_bytes(&metadata);
        let verifier_peak_bytes = stored_metadata_bytes
            .capacity()
            .saturating_add(metadata_owned_bytes)
            .saturating_add(super::format::RECEIPT_RESERVATION_BYTES);
        if verifier_peak_bytes > ARTIFACT_READER_METADATA_RESERVATION_BYTES {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact mount verification needs {verifier_peak_bytes} bytes, exceeding its {}-byte reservation",
                ARTIFACT_READER_METADATA_RESERVATION_BYTES
            )));
        }
        connection
            .pragma_update(
                None,
                "cache_size",
                -i64::try_from(page_cache_bytes / 1024)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
            )
            .map_err(sqlite_error)?;
        connection
            .pragma_update(None, "mmap_size", 0i64)
            .map_err(sqlite_error)?;
        let integrity: String = connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(sqlite_corrupt)?;
        if integrity != "ok" {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(integrity));
        }
        checkpoint(control)?;
        let decoded_mount_metadata_digest = mount_metadata_digest(&metadata)?;
        if stored_metadata_digest != stored.metadata_digest().as_str()
            || &decoded_mount_metadata_digest != stored.mount_metadata_digest()
            || &metadata.generation != stored.generation()
            || metadata.repository_id.as_ref() != stored.repository_id()
            || &metadata.freshness != stored.freshness()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact metadata digest does not verify".to_owned(),
            ));
        }
        let sections = compute_section_digests(&connection, control)?;
        if sections != stored.section_digests() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact section digests do not verify".to_owned(),
            ));
        }
        let digest = artifact_digest(
            stored.metadata_digest(),
            stored.mount_metadata_digest(),
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
        )?;
        if &digest != stored.artifact_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact content digest does not verify".to_owned(),
            ));
        }
        checkpoint(control)?;
        let retained_owned_bytes = metadata_owned_bytes.saturating_add(page_cache_bytes);
        debug_assert!(retained_owned_bytes <= cache_budget_bytes);
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            metadata,
            receipt: stored,
            retained_owned_bytes,
        })
    }

    pub fn metadata(&self) -> &CodeLexicalArtifactMountMetadataV1 {
        &self.metadata
    }

    pub fn verified_artifact(&self) -> &VerifiedCodeLexicalArtifactV1 {
        &self.receipt
    }

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
        row.map(|bytes| decode_row(&bytes).map(row_occurrence))
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

    fn lock_connection(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Connection>, CodeLexicalArtifactErrorV1> {
        self.connection.lock().map_err(|_| {
            CodeLexicalArtifactErrorV1::Io("lexical artifact reader lock is poisoned".to_owned())
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
    fn read_lexical_postings(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        self.validate_generation(&request.generation)?;
        if self.receipt.freshness().compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            return Ok(RetrieverOutcome::Stale(self.receipt.freshness().clone()));
        }
        let connection = self.lock_connection().map_err(map_query_artifact_error)?;
        ArtifactQueryV1::new(&connection, &self.metadata, &self.receipt)?
            .lexical_batch(request)
            .map(RetrieverOutcome::Complete)
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
    fn read_exact_postings(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.reader.validate_generation(&request.generation)?;
        if self.reader.receipt.freshness().compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            return Ok(RetrieverOutcome::Stale(
                self.reader.receipt.freshness().clone(),
            ));
        }
        let connection = self
            .reader
            .lock_connection()
            .map_err(map_query_artifact_error)?;
        ArtifactQueryV1::new(&connection, &self.reader.metadata, &self.reader.receipt)?
            .exact_batch(request, &self.authority)
    }
}

fn validate_reader_reservation(
    cache_budget_bytes: usize,
) -> Result<usize, CodeLexicalArtifactErrorV1> {
    if cache_budget_bytes > CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "lexical artifact cache must not exceed {CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1} bytes"
        )));
    }
    let cache_headroom = cache_budget_bytes
        .checked_sub(ARTIFACT_READER_METADATA_RESERVATION_BYTES)
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(format!(
                "lexical artifact reader needs a {ARTIFACT_READER_METADATA_RESERVATION_BYTES}-byte verifier reservation before opening the artifact"
            ))
        })?;
    if cache_headroom < ARTIFACT_SQLITE_CACHE_FLOOR_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "lexical artifact reader budget leaves {cache_headroom} bytes, under the {ARTIFACT_SQLITE_CACHE_FLOOR_BYTES}-byte kernel page-cache floor"
        )));
    }
    Ok(cache_headroom.min(ARTIFACT_SQLITE_CACHE_BYTES))
}

fn mount_metadata_owned_bytes(metadata: &CodeLexicalArtifactMountMetadataV1) -> usize {
    [
        Some(metadata.generation.as_str()),
        metadata.repository_id.as_ref().map(|value| value.as_str()),
        Some(metadata.freshness.source_namespace.as_str()),
        Some(metadata.freshness.source_instance.as_str()),
        Some(metadata.freshness.policy_revision.as_str()),
        Some(metadata.exact_retriever_revision.as_str()),
        Some(metadata.lexical_retriever_revision.as_str()),
        Some(metadata.exact_score_domain.as_str()),
    ]
    .into_iter()
    .flatten()
    .fold(0usize, |bytes, value| {
        bytes
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(value.len())
    })
}

/// Maximum bound values admitted into one SQLite compound candidate query.
/// Request terms are already byte-bounded; this additional cardinality bound
/// prevents query text and bind storage from becoming corpus-sized scratch.
const ARTIFACT_QUERY_PARAMETER_LIMIT_V1: usize = 4_096;
/// Stay below SQLite's default 500-arm compound-select ceiling with room for
/// future fixed authority clauses. Exceeding this bounded query shape is a
/// typed request-budget denial, never an authority outage from SQL prepare.
const ARTIFACT_QUERY_CLAUSE_LIMIT_V1: usize = 256;

#[derive(Default)]
struct ArtifactDocumentQueryV1 {
    clauses: Vec<String>,
    parameters: Vec<Value>,
}

impl ArtifactDocumentQueryV1 {
    fn select_rows(&self, columns: &str) -> Option<String> {
        (!self.clauses.is_empty()).then(|| {
            format!(
                "SELECT {columns} FROM rows INNER JOIN ({}) AS candidate_documents ON candidate_documents.document_id = rows.document_id ORDER BY rows.document_id",
                self.clauses.join(" UNION ")
            )
        })
    }

    fn reserve_clause(&self, additional_parameters: usize) -> Result<(), RetrievalPortError> {
        if self.clauses.len() >= ARTIFACT_QUERY_CLAUSE_LIMIT_V1
            || self.parameters.len().saturating_add(additional_parameters)
                > ARTIFACT_QUERY_PARAMETER_LIMIT_V1
        {
            Err(RetrievalPortError::BudgetExceeded)
        } else {
            Ok(())
        }
    }

    fn push_term(&mut self, field: &str, term: &str) -> Result<(), RetrievalPortError> {
        self.reserve_clause(2)?;
        self.clauses
            .push("SELECT document_id FROM term_postings WHERE field = ? AND term = ?".to_owned());
        self.parameters.push(Value::Text(field.to_owned()));
        self.parameters.push(Value::Text(term.to_owned()));
        Ok(())
    }

    fn push_term_except(
        &mut self,
        term: &str,
        excluded_field: &str,
    ) -> Result<(), RetrievalPortError> {
        self.reserve_clause(2)?;
        self.clauses
            .push("SELECT document_id FROM term_postings WHERE term = ? AND field != ?".to_owned());
        self.parameters.push(Value::Text(term.to_owned()));
        self.parameters.push(Value::Text(excluded_field.to_owned()));
        Ok(())
    }

    fn push_exact(&mut self, field: &str, term: &[u8]) -> Result<(), RetrievalPortError> {
        self.reserve_clause(2)?;
        self.clauses
            .push("SELECT document_id FROM exact_postings WHERE field = ? AND term = ?".to_owned());
        self.parameters.push(Value::Text(field.to_owned()));
        self.parameters.push(Value::Blob(term.to_vec()));
        Ok(())
    }

    fn push_ngram_clause(&mut self, kind: i64, bytes: &[u8]) -> Result<(), RetrievalPortError> {
        let ngrams = query_ngrams(bytes).into_iter().collect::<Vec<_>>();
        if ngrams.is_empty() {
            return Ok(());
        }
        self.reserve_clause(ngrams.len().saturating_add(2))?;
        let placeholders = std::iter::repeat_n("?", ngrams.len())
            .collect::<Vec<_>>()
            .join(", ");
        self.clauses.push(format!(
            "SELECT document_id FROM ngram_postings WHERE kind = ? AND ngram IN ({placeholders}) GROUP BY document_id HAVING COUNT(DISTINCT ngram) = ?"
        ));
        self.parameters.push(Value::Integer(kind));
        self.parameters
            .extend(ngrams.iter().map(|ngram| Value::Integer(i64::from(*ngram))));
        self.parameters.push(Value::Integer(
            i64::try_from(ngrams.len()).map_err(contract_error)?,
        ));
        Ok(())
    }
}

struct ArtifactQueryV1<'a> {
    connection: &'a Connection,
    metadata: &'a CodeLexicalArtifactMountMetadataV1,
    receipt: &'a VerifiedCodeLexicalArtifactV1,
    document_count: usize,
}

impl<'a> ArtifactQueryV1<'a> {
    fn new(
        connection: &'a Connection,
        metadata: &'a CodeLexicalArtifactMountMetadataV1,
        receipt: &'a VerifiedCodeLexicalArtifactV1,
    ) -> Result<Self, RetrievalPortError> {
        Ok(Self {
            connection,
            metadata,
            receipt,
            document_count: usize::try_from(receipt.total_chunks()).map_err(contract_error)?,
        })
    }

    fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverBatch<LexicalLaneEvidence>, RetrievalPortError> {
        let fuzzy = self.fuzzy_expansions(request)?;
        let mut phrase_frequencies = BTreeMap::new();
        for phrase in &request.phrases {
            let normalized = normalize_lexical(phrase);
            if phrase_frequencies.contains_key(&normalized) {
                continue;
            }
            let frequency = self.phrase_document_frequency(&normalized)?;
            phrase_frequencies.insert(normalized, frequency);
        }
        let query = self.lexical_document_query(request, &fuzzy)?;
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::<RankedLexicalEntryV1>::new();
        let Some(sql) = query.select_rows("rows.document_id, rows.row") else {
            return Ok(capped_batch(
                self.document_count,
                0,
                self.document_count as u64,
                0,
                Vec::new(),
                BTreeMap::new(),
            ));
        };
        let mut statement = self.connection.prepare(&sql).map_err(map_query_sql_error)?;
        let mut rows = statement
            .query(params_from_iter(query.parameters.iter()))
            .map_err(map_query_sql_error)?;
        while let Some(candidate_row) = rows.next().map_err(map_query_sql_error)? {
            let document = u32::try_from(
                candidate_row
                    .get::<_, i64>(0)
                    .map_err(map_query_sql_error)?,
            )
            .map_err(contract_error)?;
            let row_bytes: Vec<u8> = candidate_row.get(1).map_err(map_query_sql_error)?;
            let row = decode_row(&row_bytes).map_err(map_query_artifact_error)?;
            let score = self.score_row(document, &row, request, &fuzzy, &phrase_frequencies)?;
            let Some(ranking) = admitted_score_micros(&score, &request.field_filters)? else {
                continue;
            };
            eligible += 1;
            let candidate = candidate(
                self.receipt,
                &row,
                RetrieverKind::Lexical,
                self.metadata.lexical_retriever_revision.clone(),
                request.score_domain.clone(),
                None,
            )?;
            let key = (Reverse(ranking), row.id.as_str().to_owned(), document);
            let evidence = LexicalLaneEvidence {
                binding: binding(&row, &candidate, score.matched_kinds),
                field_scores_micros: score.field_scores,
                matched_whole_terms: score.matched_whole_terms,
                matched_subtokens: score.matched_subtokens,
                matched_phrases: score.matched_phrases,
                typo_recovery_applied: score.typo_recovery_applied,
                echo_penalty_applied: score.echo_penalty_applied,
            };
            retain_bounded(
                &mut ranked,
                cap,
                RankedLexicalEntryV1 {
                    key,
                    candidate,
                    evidence,
                },
            );
        }
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            let RankedLexicalEntryV1 {
                mut candidate,
                evidence,
                ..
            } = entry;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        Ok(capped_batch(
            self.document_count,
            eligible,
            self.document_count as u64 - eligible,
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
        let query = self.exact_document_query(request)?;
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::<RankedExactEntryV1>::new();
        let Some(sql) = query.select_rows("rows.document_id, rows.row") else {
            return Ok(RetrieverOutcome::Complete(capped_batch(
                self.document_count,
                0,
                self.document_count as u64,
                0,
                Vec::new(),
                BTreeMap::new(),
            )));
        };
        let mut statement = self.connection.prepare(&sql).map_err(map_query_sql_error)?;
        let mut rows = statement
            .query(params_from_iter(query.parameters.iter()))
            .map_err(map_query_sql_error)?;
        while let Some(candidate_row) = rows.next().map_err(map_query_sql_error)? {
            let document = u32::try_from(
                candidate_row
                    .get::<_, i64>(0)
                    .map_err(map_query_sql_error)?,
            )
            .map_err(contract_error)?;
            let row_bytes: Vec<u8> = candidate_row.get(1).map_err(map_query_sql_error)?;
            let row = decode_row(&row_bytes).map_err(map_query_artifact_error)?;
            let (matched_literals, matched_kinds) = exact_matches_artifact(&row, request);
            if matched_literals.is_empty() {
                continue;
            }
            let proof = matched_literals
                .iter()
                .find_map(|literal| {
                    authority
                        .admit(literal.field, &literal.original_bytes, &request.base)
                        .transpose()
                })
                .transpose()
                .map_err(contract_error)?;
            let Some(proof) = proof else {
                continue;
            };
            eligible += 1;
            let candidate = candidate(
                self.receipt,
                &row,
                RetrieverKind::ExactLiteral,
                self.metadata.exact_retriever_revision.clone(),
                self.metadata.exact_score_domain.clone(),
                Some(proof.clone()),
            )?;
            let evidence = ExactLaneEvidence {
                binding: binding(&row, &candidate, matched_kinds),
                matched_literals,
                admission_proof: proof,
            };
            retain_bounded(
                &mut ranked,
                cap,
                RankedExactEntryV1 {
                    key: (
                        Reverse(evidence.matched_literals.len()),
                        row.id.as_str().to_owned(),
                        document,
                    ),
                    candidate,
                    evidence,
                },
            );
        }
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            let RankedExactEntryV1 {
                mut candidate,
                evidence,
                ..
            } = entry;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        Ok(RetrieverOutcome::Complete(capped_batch(
            self.document_count,
            eligible,
            self.document_count as u64 - eligible,
            truncated,
            candidates,
            evidence_by_occurrence,
        )))
    }

    fn phrase_document_frequency(&self, phrase: &str) -> Result<usize, RetrievalPortError> {
        let mut query = ArtifactDocumentQueryV1::default();
        query.push_ngram_clause(NGRAM_NORMALIZED, phrase.as_bytes())?;
        let Some(sql) = query.select_rows("rows.normalized_text") else {
            return Ok(0);
        };
        let mut statement = self.connection.prepare(&sql).map_err(map_query_sql_error)?;
        let mut rows = statement
            .query(params_from_iter(query.parameters.iter()))
            .map_err(map_query_sql_error)?;
        let mut frequency = 0usize;
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            let normalized_text: String = row.get(0).map_err(map_query_sql_error)?;
            if substring_count(&normalized_text, phrase) > 0 {
                frequency = frequency.saturating_add(1);
            }
        }
        Ok(frequency)
    }

    fn lexical_document_query(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
    ) -> Result<ArtifactDocumentQueryV1, RetrievalPortError> {
        let mut query = ArtifactDocumentQueryV1::default();
        let subtoken_field =
            encode_field(LexicalFieldV1::Subtoken).map_err(map_query_artifact_error)?;
        for term in &request.whole_terms {
            query.push_term_except(&normalize_lexical(term), &subtoken_field)?;
            if let Some(expansions) = fuzzy.by_query.get(term) {
                for expansion in expansions {
                    query.push_term_except(expansion, &subtoken_field)?;
                }
            }
        }
        for subtoken in &request.subtokens {
            query.push_term(&subtoken_field, &normalize_lexical(subtoken))?;
        }
        for phrase in &request.phrases {
            let normalized = normalize_lexical(phrase);
            query.push_ngram_clause(NGRAM_NORMALIZED, normalized.as_bytes())?;
        }
        Ok(query)
    }

    fn exact_document_query(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<ArtifactDocumentQueryV1, RetrievalPortError> {
        let mut query = ArtifactDocumentQueryV1::default();
        for literal in &request.literals {
            if matches!(
                literal.field,
                ExactFieldV1::QuotedPhrase
                    | ExactFieldV1::DiagnosticText
                    | ExactFieldV1::CompilerOrRuntimeError
            ) {
                query.push_ngram_clause(NGRAM_NORMALIZED, &literal.original_bytes)?;
                query.push_ngram_clause(NGRAM_RAW_OVERRIDE, &literal.original_bytes)?;
            }
            query.push_exact(
                &serde_json::to_string(&literal.field).map_err(contract_error)?,
                &literal.canonical_bytes,
            )?;
        }
        Ok(query)
    }

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
            let bound = fuzzy_distance_bound(normalized_query.chars().count());
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
        groups.sort_by_key(|group| group.first_ordinal);
        let maximum_distance = groups.iter().map(|group| group.bound).max().unwrap_or(0);
        let mut selected = Vec::with_capacity(limit);
        'distance: for distance in 1..=maximum_distance {
            for (group_index, group) in groups.iter_mut().enumerate() {
                let remaining = limit.saturating_sub(selected.len());
                if remaining == 0 {
                    break 'distance;
                }
                if distance > group.bound {
                    continue;
                }
                let mut statement = self
                    .connection
                    .prepare("SELECT term FROM vocabulary ORDER BY term")
                    .map_err(map_query_sql_error)?;
                let mut rows = statement.query([]).map_err(map_query_sql_error)?;
                let mut added = 0usize;
                while added < remaining {
                    let Some(row) = rows.next().map_err(map_query_sql_error)? else {
                        break;
                    };
                    let term: String = row.get(0).map_err(map_query_sql_error)?;
                    if term != group.normalized_query
                        && bounded_edit_distance(&group.normalized_query, &term, distance)
                            == Some(distance)
                        && group.seen.insert(term.clone())
                    {
                        selected.push((group_index, term));
                        added += 1;
                    }
                }
            }
        }
        let mut by_query = BTreeMap::<String, BTreeSet<String>>::new();
        for (group_index, term) in selected {
            for query in &groups[group_index].queries {
                by_query
                    .entry(query.clone())
                    .or_default()
                    .insert(term.clone());
            }
        }
        Ok(FuzzyExpansionsV1 { by_query })
    }

    fn score_row(
        &self,
        document: u32,
        row: &ArtifactRowV1,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
    ) -> Result<LexicalRowScoreV1, RetrievalPortError> {
        let mut field_scores = BTreeMap::new();
        let mut matched_whole_terms = BTreeSet::new();
        let mut matched_subtokens = BTreeSet::new();
        let mut matched_phrases = BTreeSet::new();
        let mut matched_kinds = BTreeSet::new();
        let mut typo_recovery_applied = false;
        for field in row.field_lengths.keys() {
            if *field != LexicalFieldV1::Subtoken {
                for query_term in &request.whole_terms {
                    let normalized = normalize_lexical(query_term);
                    let exact_tf = self.term_frequency(*field, &normalized, document)?;
                    if exact_tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, &normalized, exact_tf, row)?,
                        );
                        matched_whole_terms.insert(query_term.clone());
                        collect_term_kinds_artifact(row, &normalized, &mut matched_kinds);
                    }
                    if let Some(expansions) = fuzzy.by_query.get(query_term) {
                        for expansion in expansions {
                            let tf = self.term_frequency(*field, expansion, document)?;
                            if tf == 0 {
                                continue;
                            }
                            let score = self
                                .term_score(*field, expansion, tf, row)?
                                .saturating_mul(FUZZY_SCORE_MILLIS)
                                / 1_000;
                            add_score(&mut field_scores, *field, score);
                            matched_whole_terms.insert(query_term.clone());
                            typo_recovery_applied = true;
                            collect_term_kinds_artifact(row, expansion, &mut matched_kinds);
                        }
                    }
                }
            } else {
                for subtoken in &request.subtokens {
                    let normalized = normalize_lexical(subtoken);
                    let tf = self.term_frequency(*field, &normalized, document)?;
                    if tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, &normalized, tf, row)?,
                        );
                        matched_subtokens.insert(subtoken.clone());
                    }
                }
            }
        }
        for phrase in &request.phrases {
            let normalized = normalize_lexical(phrase);
            let tf = substring_count(&row.normalized_text, &normalized);
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
                        .get(&normalized)
                        .copied()
                        .unwrap_or_default(),
                )?
                .saturating_mul(PHRASE_SCORE_MILLIS)
                / 1_000;
            add_score(&mut field_scores, field, score);
            matched_phrases.insert(phrase.clone());
        }
        let normalized_query = normalize_lexical(request.query_view.as_str().trim_matches('"'));
        let echo_penalty_applied =
            !normalized_query.is_empty() && normalized_query == row.normalized_text.trim();
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

    fn term_frequency(
        &self,
        field: LexicalFieldV1,
        term: &str,
        document: u32,
    ) -> Result<usize, RetrievalPortError> {
        let field = encode_field(field).map_err(map_query_artifact_error)?;
        self.connection
            .query_row(
                "SELECT frequency FROM term_postings WHERE field = ?1 AND term = ?2 AND document_id = ?3",
                params![field, term, i64::from(document)],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(map_query_sql_error)?
            .unwrap_or_default()
            .try_into()
            .map_err(contract_error)
    }

    fn term_score(
        &self,
        field: LexicalFieldV1,
        term: &str,
        term_frequency: usize,
        row: &ArtifactRowV1,
    ) -> Result<u64, RetrievalPortError> {
        let encoded = encode_field(field).map_err(map_query_artifact_error)?;
        let document_frequency: i64 = self
            .connection
            .query_row(
                "SELECT document_frequency FROM term_stats WHERE field = ?1 AND term = ?2",
                params![encoded, term],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_query_sql_error)?
            .unwrap_or_default();
        self.term_score_with_df(
            field,
            term_frequency,
            row,
            usize::try_from(document_frequency).map_err(contract_error)?,
        )
    }

    fn term_score_with_df(
        &self,
        field: LexicalFieldV1,
        term_frequency: usize,
        row: &ArtifactRowV1,
        document_frequency: usize,
    ) -> Result<u64, RetrievalPortError> {
        let encoded = encode_field(field).map_err(map_query_artifact_error)?;
        let total: i64 = self
            .connection
            .query_row(
                "SELECT total_length FROM field_stats WHERE field = ?1",
                [encoded],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_query_sql_error)?
            .unwrap_or_default();
        let total = usize::try_from(total).map_err(contract_error)?;
        let average = total.div_ceil(self.document_count.max(1)).max(1);
        let document_length = row.field_lengths.get(&field).copied().unwrap_or(0).max(1);
        Ok(bm25_score_micros(
            self.document_count,
            document_frequency,
            term_frequency,
            document_length,
            average,
            field_weight_millis(field),
        ))
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

/// One fully scored lexical candidate retained by the bounded top-K heap.
/// Carrying its evidence avoids re-decoding and re-scoring winners after the
/// streaming pass.
struct RankedLexicalEntryV1 {
    key: (Reverse<u64>, String, u32),
    candidate: CompactCandidate,
    evidence: LexicalLaneEvidence,
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

/// One admitted exact candidate retained during bounded selection. Central
/// admission and evidence construction happen before heap eligibility, so a
/// denied row can never displace an admitted winner and winners are not
/// rehydrated after selection.
struct RankedExactEntryV1 {
    key: (Reverse<usize>, String, u32),
    candidate: CompactCandidate,
    evidence: ExactLaneEvidence,
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

fn exact_matches_artifact(
    row: &ArtifactRowV1,
    request: &ExactLaneRequest,
) -> (
    Vec<crate::retrieval::exact::ExactLiteralV1>,
    Vec<ExactTechnicalTermKindV1>,
) {
    exact_matches_fields(
        &row.exact_terms,
        &row.sanitized_text,
        &row.logical_path,
        request,
    )
}

fn collect_term_kinds_artifact(
    row: &ArtifactRowV1,
    term: &str,
    matched: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    collect_term_kinds_fields(&row.exact_terms, term, matched);
}

fn bounded_edit_distance(left: &str, right: &str, limit: usize) -> Option<usize> {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left.len().abs_diff(right.len()) > limit {
        return None;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0usize; right.len() + 1];
    for (left_index, left_character) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.iter().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_character != right_character));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= limit).then_some(previous[right.len()])
}

fn decode_row(bytes: &[u8]) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
    serde_json::from_slice(bytes)
        .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
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
        CodeLexicalArtifactErrorV1::Corrupt(error) | CodeLexicalArtifactErrorV1::Io(error) => {
            RetrievalPortError::AuthorityUnavailable(error)
        }
    }
}

struct NeverInterrupted;

impl CodeIndexExecutionControlV1 for NeverInterrupted {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    use super::retain_bounded;

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
}
