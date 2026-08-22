use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter, types::Value};
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
    ArtifactRowV1, CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, CodeLexicalArtifactOccurrenceV1,
    CodeLexicalImportMembershipWitnessV1, VerifiedCodeLexicalArtifactV1, artifact_digest,
    decode_padded_receipt, encode_field, metadata_digest,
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
    PHRASE_SCORE_MILLIS, add_score, bm25_score_micros, collect_term_kinds, exact_matches,
    field_weight_millis, fuzzy_distance_bound, normalize_lexical, retrieval_anchor,
    substring_count,
};
use crate::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    MAX_FUZZY_TERM_EXPANSIONS_V1, field_admitted,
};

#[derive(Clone)]
pub struct CodeLexicalArtifactReaderV1 {
    connection: Arc<Mutex<Connection>>,
    metadata: super::super::CodeLexicalProjectionMetadataV1,
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
        let digest = digest_artifact_file(&mut file, control)?;
        if &digest != expected_file_digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact file bytes do not match the durable head digest".to_owned(),
            ));
        }
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| map_reader_open_error(path, error))?;
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        configure_reader_window(&connection, cache_budget_bytes, 0)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(sqlite_error)?;
        verify_artifact_state_revision(&connection, control)?;
        let receipt_bytes: Vec<u8> = connection
            .query_row(
                "SELECT receipt FROM artifact_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_corrupt)?;
        let receipt = decode_padded_receipt(&receipt_bytes)?.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "content-addressed lexical artifact has no finalized receipt".to_owned(),
            )
        })?;
        if receipt.file_size_bytes() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt disagrees with the durable head file size".to_owned(),
            ));
        }
        let reader =
            Self::open_connection_with_control(connection, &receipt, cache_budget_bytes, control)?;
        verify_retained_artifact_digest(&mut file, expected_file_digest, control)?;
        verify_named_path_identity(path, &file)?;
        Ok(reader)
    }

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
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| map_reader_open_error(path, error))?;
        Self::open_connection_with_control(connection, expected, cache_budget_bytes, control)
    }

    fn open_connection_with_control(
        connection: Connection,
        expected: &VerifiedCodeLexicalArtifactV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(sqlite_error)?;
        verify_artifact_state_revision(&connection, control)?;
        // Read the BLOB length first so the page cache can be configured
        // before metadata is materialized. The retained metadata copy plus
        // SQLite's cache therefore cannot exceed the caller's reservation.
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
        // Kernel SQLite window: no mmap grant, page cache clamped to
        // [2, 64] MiB. The caller budget covers the retained metadata copy
        // plus the cache actually granted; nothing else is claimed.
        let sqlite_budget = cache_budget_bytes - stored_metadata_len;
        if sqlite_budget < ARTIFACT_SQLITE_CACHE_FLOOR_BYTES {
            return Err(CodeLexicalArtifactErrorV1::Unreserved(format!(
                "lexical artifact reader budget leaves {sqlite_budget} bytes, under the {ARTIFACT_SQLITE_CACHE_FLOOR_BYTES}-byte kernel page-cache floor"
            )));
        }
        let page_cache_bytes =
            configure_reader_window(&connection, cache_budget_bytes, stored_metadata_len)?;
        let (stored_metadata_bytes, stored_metadata_digest): (Vec<u8>, String) = connection
            .query_row(
                "SELECT metadata, metadata_digest FROM artifact_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if stored_metadata_bytes.len() != stored_metadata_len {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact metadata changed while opening its sealed reader".to_owned(),
            ));
        }
        let metadata: super::super::CodeLexicalProjectionMetadataV1 =
            serde_json::from_slice(&stored_metadata_bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let integrity: String = connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(sqlite_corrupt)?;
        if integrity != "ok" {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(integrity));
        }
        checkpoint(control)?;
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
        let sections = compute_section_digests(&connection, control)?;
        if sections != stored.section_digests() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact section digests do not verify".to_owned(),
            ));
        }
        let digest = artifact_digest(
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
        )?;
        if &digest != stored.artifact_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact content digest does not verify".to_owned(),
            ));
        }
        checkpoint(control)?;
        let retained_owned_bytes = stored_metadata_bytes.len().saturating_add(page_cache_bytes);
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            metadata,
            receipt: stored,
            retained_owned_bytes,
        })
    }

    pub fn metadata(&self) -> &super::super::CodeLexicalProjectionMetadataV1 {
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

struct ArtifactQueryV1<'a> {
    connection: &'a Connection,
    metadata: &'a super::super::CodeLexicalProjectionMetadataV1,
    receipt: &'a VerifiedCodeLexicalArtifactV1,
    document_count: usize,
}

/// A SQLite-owned candidate set. The query is evaluated row-by-row, so Rust
/// never retains one identifier per matching document. Query input is already
/// bounded by the lexical request contract; n-gram intersections additionally
/// have a fixed predicate ceiling below.
#[derive(Clone, Debug)]
struct DocumentQueryV1 {
    sql: Option<String>,
    parameters: Vec<Value>,
}

impl DocumentQueryV1 {
    fn empty() -> Self {
        Self {
            sql: None,
            parameters: Vec::new(),
        }
    }

    fn term(field: String, term: String) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE field = ? AND term = ?".to_owned(),
            ),
            parameters: vec![Value::Text(field), Value::Text(term)],
        }
    }

    fn term_except(term: String, excluded_field: String) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM term_postings WHERE term = ? AND field != ?".to_owned(),
            ),
            parameters: vec![Value::Text(term), Value::Text(excluded_field)],
        }
    }

    fn exact(field: String, term: Vec<u8>) -> Self {
        Self {
            sql: Some(
                "SELECT document_id FROM exact_postings WHERE field = ? AND term = ?".to_owned(),
            ),
            parameters: vec![Value::Text(field), Value::Blob(term)],
        }
    }
}

/// The query engine, rather than an in-process bitmap, owns duplicate removal
/// and sorted candidate enumeration. SQLite's configured fixed page cache is
/// the only storage used for the set-operation work table.
const ARTIFACT_UNION_COMPOUND_ARMS_V1: usize = 64;

fn union_document_queries(queries: impl IntoIterator<Item = DocumentQueryV1>) -> DocumentQueryV1 {
    let mut level = queries
        .into_iter()
        .filter(|query| query.sql.is_some())
        .collect::<Vec<_>>();
    if level.is_empty() {
        return DocumentQueryV1::empty();
    }
    while level.len() > 1 {
        level = level
            .chunks(ARTIFACT_UNION_COMPOUND_ARMS_V1)
            .map(compound_union_document_queries)
            .collect();
    }
    let Some(root) = level.pop() else {
        return DocumentQueryV1::empty();
    };
    DocumentQueryV1 {
        sql: root
            .sql
            .map(|sql| format!("SELECT DISTINCT document_id FROM ({sql}) ORDER BY document_id")),
        parameters: root.parameters,
    }
}

fn compound_union_document_queries(queries: &[DocumentQueryV1]) -> DocumentQueryV1 {
    let mut sql = String::new();
    let mut parameters = Vec::new();
    for query in queries {
        let Some(query_sql) = query.sql.as_deref() else {
            continue;
        };
        if !sql.is_empty() {
            sql.push_str(" UNION ALL ");
        }
        sql.push_str("SELECT document_id FROM (");
        sql.push_str(query_sql);
        sql.push(')');
        parameters.extend(query.parameters.iter().cloned());
    }
    DocumentQueryV1 {
        sql: Some(sql),
        parameters,
    }
}

fn visit_document_ids(
    connection: &Connection,
    query: &DocumentQueryV1,
    mut visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    let Some(sql) = &query.sql else {
        return Ok(());
    };
    let mut statement = connection.prepare(sql).map_err(map_query_sql_error)?;
    let mut rows = statement
        .query(params_from_iter(query.parameters.iter()))
        .map_err(map_query_sql_error)?;
    while let Some(row) = rows.next().map_err(map_query_sql_error)? {
        let document = row.get::<_, i64>(0).map_err(map_query_sql_error)?;
        visitor(u32::try_from(document).map_err(contract_error)?)?;
    }
    Ok(())
}

const ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1: usize = 16;

/// The first fixed number of distinct n-grams forms a selective, bounded
/// prefilter. It may admit a superset for a very long phrase; the row-level
/// substring check remains the correctness authority before scoring.
fn ngram_document_query(kind: i64, bytes: &[u8]) -> DocumentQueryV1 {
    let ngrams = query_ngrams(bytes)
        .into_iter()
        .take(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1)
        .collect::<Vec<_>>();
    if ngrams.is_empty() {
        return DocumentQueryV1::empty();
    }
    let placeholders = std::iter::repeat_n("?", ngrams.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut parameters = Vec::with_capacity(ngrams.len() + 2);
    parameters.push(Value::Integer(kind));
    parameters.extend(ngrams.iter().map(|ngram| Value::Integer(i64::from(*ngram))));
    parameters.push(Value::Integer(ngrams.len() as i64));
    DocumentQueryV1 {
        sql: Some(format!(
            "SELECT document_id FROM ngram_postings \
             WHERE kind = ? AND ngram IN ({placeholders}) \
             GROUP BY document_id HAVING COUNT(DISTINCT ngram) = ? \
             ORDER BY document_id"
        )),
        parameters,
    }
}

impl<'a> ArtifactQueryV1<'a> {
    fn new(
        connection: &'a Connection,
        metadata: &'a super::super::CodeLexicalProjectionMetadataV1,
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
        let phrase_queries = request
            .phrases
            .iter()
            .map(|phrase| {
                let normalized = normalize_lexical(phrase);
                let query = ngram_document_query(NGRAM_NORMALIZED, normalized.as_bytes());
                (normalized, query)
            })
            .collect::<BTreeMap<_, _>>();
        let mut phrase_frequencies = BTreeMap::new();
        for (phrase, query) in &phrase_queries {
            let mut frequency = 0usize;
            self.visit_documents(query, |document| {
                let row = self.row(document)?;
                if substring_count(&row.normalized_text, phrase) > 0 {
                    frequency += 1;
                }
                Ok(())
            })?;
            phrase_frequencies.insert(phrase.clone(), frequency);
        }
        let documents = self.lexical_documents(request, &fuzzy, &phrase_queries)?;
        // Selection precedes hydration: the scan holds one transient row at
        // a time and a bounded worst-first heap of ranking keys, so retained
        // materialization never exceeds the lane candidate cap.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut excluded = self.document_count as u64;
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::new();
        self.visit_documents(&documents, |document| {
            let row = self.row(document)?;
            let score = self.score_row(document, &row, request, &fuzzy, &phrase_frequencies)?;
            let Some(ranking) = admitted_score_micros(&score, &request.field_filters)? else {
                return Ok(());
            };
            eligible += 1;
            excluded = excluded.saturating_sub(1);
            retain_bounded(
                &mut ranked,
                cap,
                (Reverse(ranking), row.id.as_str().to_owned(), document),
            );
            Ok(())
        })?;
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (_, _, document)) in selected.into_iter().enumerate() {
            let row = self.row(document)?;
            let score = self.score_row(document, &row, request, &fuzzy, &phrase_frequencies)?;
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
        // minted proofs alongside the ranking keys.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut excluded = self.document_count as u64;
        let mut eligible = 0u64;
        let mut ranked = BinaryHeap::new();
        self.visit_documents(&documents, |document| {
            let row = self.row(document)?;
            let (matched_literals, _) = exact_matches_artifact(&row, request);
            if matched_literals.is_empty() {
                return Ok(());
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
                    proof,
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
                proof,
            } = entry;
            let row = self.row(document)?;
            let (matched_literals, matched_kinds) = exact_matches_artifact(&row, request);
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
        let bytes: Vec<u8> = self
            .connection
            .query_row(
                "SELECT row FROM rows WHERE document_id = ?1",
                [i64::from(document)],
                |row| row.get(0),
            )
            .map_err(map_query_sql_error)?;
        decode_row(&bytes).map_err(map_query_artifact_error)
    }

    fn lexical_documents(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_queries: &BTreeMap<String, DocumentQueryV1>,
    ) -> Result<DocumentQueryV1, RetrievalPortError> {
        let mut sources = Vec::new();
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
        sources.extend(phrase_queries.values().cloned());
        Ok(union_document_queries(sources))
    }

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
                    NGRAM_NORMALIZED,
                    &literal.original_bytes,
                ));
                sources.push(ngram_document_query(
                    NGRAM_RAW_OVERRIDE,
                    &literal.original_bytes,
                ));
            }
            let field = serde_json::to_string(&literal.field).map_err(contract_error)?;
            sources.push(DocumentQueryV1::exact(
                field,
                literal.canonical_bytes.clone(),
            ));
        }
        Ok(union_document_queries(sources))
    }

    fn visit_documents(
        &self,
        query: &DocumentQueryV1,
        visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
    ) -> Result<(), RetrievalPortError> {
        visit_document_ids(self.connection, query, visitor)
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

/// One admitted exact candidate retained during bounded selection: the
/// canonical ranking key plus the proof the central authority already
/// minted for it. Ordering is by key alone.
struct RankedExactEntryV1 {
    key: (Reverse<usize>, String, u32),
    proof: ExactAdmissionProof,
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
    let projected = super::super::ProjectedChunkV1 {
        id: row.id.clone(),
        anchor: row.anchor.clone(),
        language_descriptor_revision: row.language_descriptor_revision.clone(),
        exact_terms: row.exact_terms.clone(),
        sanitized_text: row.sanitized_text.clone(),
        logical_path: row.logical_path.clone(),
        field_lengths: row.field_lengths.clone(),
        normalized_text: row.normalized_text.clone(),
    };
    exact_matches(&projected, request)
}

fn collect_term_kinds_artifact(
    row: &ArtifactRowV1,
    term: &str,
    matched: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    let projected = super::super::ProjectedChunkV1 {
        id: row.id.clone(),
        anchor: row.anchor.clone(),
        language_descriptor_revision: row.language_descriptor_revision.clone(),
        exact_terms: row.exact_terms.clone(),
        sanitized_text: row.sanitized_text.clone(),
        logical_path: row.logical_path.clone(),
        field_lengths: row.field_lengths.clone(),
        normalized_text: row.normalized_text.clone(),
    };
    collect_term_kinds(&projected, term, matched);
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

fn digest_artifact_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
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
    ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
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
    let actual = digest_artifact_file(file, control)?;
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
        use std::os::windows::fs::MetadataExt;

        let named_volume = named.volume_serial_number().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Incompatible(
                "artifact filesystem does not expose a stable volume identity".to_owned(),
            )
        })?;
        let opened_volume = opened.volume_serial_number().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Incompatible(
                "opened artifact does not expose a stable volume identity".to_owned(),
            )
        })?;
        let named_index = named.file_index().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Incompatible(
                "artifact filesystem does not expose a stable file identity".to_owned(),
            )
        })?;
        let opened_index = opened.file_index().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Incompatible(
                "opened artifact does not expose a stable file identity".to_owned(),
            )
        })?;
        if named_volume != opened_volume || named_index != opened_index {
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
    if revision != CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1 {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(format!(
            "artifact state format revision {revision} is unsupported"
        )));
    }
    checkpoint(control)
}

fn configure_reader_window(
    connection: &Connection,
    cache_budget_bytes: usize,
    retained_metadata_bytes: usize,
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
    connection
        .pragma_update(None, "mmap_size", 0i64)
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "temp_store", "FILE")
        .map_err(sqlite_error)?;
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
        CodeLexicalArtifactErrorV1::Unreserved(_) => RetrievalPortError::BudgetExceeded,
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
    use std::collections::BinaryHeap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rusqlite::{Connection, params};
    use tracedecay_domain::ManifestDigest;
    use tracedecay_private_fs::open_private_file;

    use super::{
        ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1, CodeLexicalArtifactErrorV1,
        CodeLexicalArtifactReaderV1, DocumentQueryV1, NGRAM_NORMALIZED, map_query_artifact_error,
        ngram_document_query, query_ngrams, retain_bounded, union_document_queries,
        visit_document_ids,
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

    fn streamed_documents(connection: &Connection, query: &DocumentQueryV1) -> Vec<u32> {
        let mut documents = Vec::new();
        visit_document_ids(connection, query, |document| {
            documents.push(document);
            Ok(())
        })
        .expect("SQLite stream succeeds");
        documents
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
        let expected = super::digest_artifact_file(&mut retained, &AlwaysActiveControl)
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
        let digest = super::digest_artifact_file(&mut file, &AlwaysActiveControl)
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
                    kind INTEGER NOT NULL,
                    ngram INTEGER NOT NULL,
                    document_id INTEGER NOT NULL
                );",
            )
            .expect("ngram fixture schema");
        let phrase = b"abcdefghijklmnopqrstuvw";
        let ngrams = query_ngrams(phrase)
            .into_iter()
            .take(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1)
            .collect::<Vec<_>>();
        assert_eq!(ngrams.len(), ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1);
        for (ordinal, ngram) in ngrams.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO ngram_postings(kind, ngram, document_id) VALUES (?1, ?2, 1)",
                    params![NGRAM_NORMALIZED, i64::from(*ngram)],
                )
                .expect("complete phrase posting");
            if ordinal + 1 < ngrams.len() {
                connection
                    .execute(
                        "INSERT INTO ngram_postings(kind, ngram, document_id) VALUES (?1, ?2, 2)",
                        params![NGRAM_NORMALIZED, i64::from(*ngram)],
                    )
                    .expect("incomplete phrase posting");
            }
        }

        let query = ngram_document_query(NGRAM_NORMALIZED, phrase);

        assert_eq!(
            query.parameters.len(),
            ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1 + 2
        );
        assert_eq!(streamed_documents(&connection, &query), vec![1]);
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
        ]);

        assert_eq!(streamed_documents(&connection, &query), vec![1, 2, 3]);
        assert_eq!(
            streamed_documents(
                &connection,
                &union_document_queries([DocumentQueryV1::term(
                    "subtoken".to_owned(),
                    "render".to_owned(),
                )]),
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

        let query = union_document_queries(sources);

        assert_eq!(
            streamed_documents(&connection, &query),
            (0..source_count).collect::<Vec<_>>(),
            "nested streamed enumeration preserves the exact candidate order beyond SQLite's flat UNION ceiling"
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
}
