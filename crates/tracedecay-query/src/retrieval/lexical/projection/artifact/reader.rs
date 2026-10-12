mod family_report;
pub use family_report::{CloneExactFamilyArtifactCandidateV1, CloneExactFamilyArtifactPageV1};

mod query;
#[cfg(test)]
mod tests;

use super::super::{LexicalFieldTextV1, LexicalIndexedRow};
use super::builder::compute_section_digests;
use super::clone_codec::{
    CloneOccurrenceRouteV1, digest_key, routed_clone_body_row, stored_clone_occurrence,
};
use super::format::{
    ArtifactRowV1, CodeLexicalArtifactOccurrenceV1, VerifiedCodeLexicalArtifactV1,
    content_metadata_bytes, decode_padded_receipt_with_control, receipt_artifact_digest,
    stored_metadata_digest as stored_metadata_digest_of, verify_artifact_table_layout,
};
#[cfg(test)]
use super::postings::{NGRAM_NORMALIZED, query_ngrams};
use super::row_codec::{
    ConnectionRowDictionaryV1, RowBlocksV1, ScoringPrefaceIndexV1, decode_artifact_row,
    load_scoring_preface_index, stored_chunk_key, stored_symbol_key,
};
use super::schema::require_served_revision;
use super::{
    ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_CACHE_FLOOR_BYTES,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1, checkpoint,
    sqlite_corrupt, sqlite_error, sqlite_open_path,
};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::lexical::{LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RETRIEVAL_CANDIDATE_BATCH_SIZE, RetrievalExecutionControl, RetrievalPortError,
    retrieval_checkpoint,
};
#[cfg(test)]
use query::{
    ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1, ARTIFACT_NGRAM_MAX_CANDIDATES_V1,
    ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1, ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
    ArtifactQueryMetricsV1, RequestTermPostingsV1, TermFieldPostingsV1,
    charge_ngram_encoded_list_bytes, ensure_ngram_candidate_cardinality,
    ensure_sqlite_bind_capacity, ensure_sqlite_bound_value_bytes, ngram_bitmap_candidates,
    ngram_document_query, retain_bounded, term_frequency, visit_document_ids, visit_lexical_rows,
};
use query::{ArtifactQueryV1, FuzzyVocabularyV1};
#[cfg(test)]
use roaring::RoaringBitmap;
#[cfg(test)]
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::cmp::Reverse;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::{BTreeSet, BinaryHeap};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::ops::Deref;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::sync::{
    Arc, Condvar, Mutex as StdMutex, MutexGuard as StdMutexGuard, OnceLock, TryLockError,
};
use std::time::Duration;
use tracedecay_code_index::clones::{
    CloneBodyOccurrenceV1, CloneBodyPayloadV1, CloneExactKeyV1, CodeIndexCloneBodyV1,
};
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, CodeIndexInterruptionV1};
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
    LanguageDescriptorRevision, ManifestDigest, RetrieverBatch, RetrieverOutcome, SourceSpan,
    SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_private_fs::{RewriteWitness, open_private_file};
use tracedecay_runtime_core::resident_memory::ResidentMemoryReservationV1;

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
    connections: Arc<ArtifactReaders>,
    resident_memory: Option<Arc<ResidentMemoryReservationV1>>,
    metadata: super::super::CodeLexicalProjectionMetadataV1,
    receipt: VerifiedCodeLexicalArtifactV1,
    retained_owned_bytes: usize,
    /// Fuzzy expansion walks in-fuzzy terms; share one load across clones
    /// and later queries on this reader.
    fuzzy_vocabulary: Arc<OnceLock<Arc<FuzzyVocabularyV1>>>,
    /// Scoring prefaces for every document. A common term touches thousands
    /// of blocks; decoding that window on every query was the lexical walk.
    scoring_prefaces: Arc<StdMutex<Option<Arc<ScoringPrefaceIndexV1>>>>,
    /// The admitted reader ceiling left after metadata and page caches. The
    /// preface cache must fit here: it is held for the reader's lifetime.
    preface_ceiling_bytes: usize,
}

struct ArtifactReaders {
    connections: Vec<StdMutex<Connection>>,
    queue: StdMutex<()>,
    available: Condvar,
}

struct ArtifactReaderWakeup<'a>(&'a ArtifactReaders);

impl Drop for ArtifactReaderWakeup<'_> {
    fn drop(&mut self) {
        // Pair the availability check and sleep so a released handle cannot
        // be missed between them. Poison still wakes waiters to report it.
        let _queue = self.0.queue.lock();
        self.0.available.notify_all();
    }
}

struct ArtifactReadGuard<'a> {
    connection: StdMutexGuard<'a, Connection>,
    _scratch_reservation: Option<ResidentMemoryReservationV1>,
    // Fields drop in order: unlock and release memory before waking a waiter.
    _wakeup: ArtifactReaderWakeup<'a>,
}

impl Deref for ArtifactReadGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
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
    /// The ordinal of the last occurrence the page returned.
    pub(super) after: i64,
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct StableArtifactFileStateV1 {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change_seconds: i64,
    #[cfg(unix)]
    change_nanoseconds: i64,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(windows)]
    last_write_time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CodeLexicalArtifactRestoreWitnessBodyV1 {
    version: u32,
    file_digest: ManifestDigest,
    receipt_digest: ManifestDigest,
    file_state: StableArtifactFileStateV1,
}

/// Rebuildable proof that one publisher-verified artifact still has the exact
/// native file state observed by a prior full verification.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalArtifactRestoreWitnessV1 {
    body: CodeLexicalArtifactRestoreWitnessBodyV1,
    body_digest: ManifestDigest,
}

impl CodeLexicalArtifactRestoreWitnessV1 {
    const VERSION: u32 = 2;

    fn digest_body(
        body: &CodeLexicalArtifactRestoreWitnessBodyV1,
    ) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
        canonical_sha256(&("tracedecay.lexical-artifact-restore-witness.v1", body))
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
    }

    fn from_verified_file_state(
        file_digest: ManifestDigest,
        receipt_digest: ManifestDigest,
        file_state: StableArtifactFileStateV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let body = CodeLexicalArtifactRestoreWitnessBodyV1 {
            version: Self::VERSION,
            file_digest,
            receipt_digest,
            file_state,
        };
        let body_digest = Self::digest_body(&body)?;
        Ok(Self { body, body_digest })
    }

    pub fn encode(&self) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
        serde_json::to_vec(self)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let witness: Self = serde_json::from_slice(bytes)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if witness.body.version != Self::VERSION
            || Self::digest_body(&witness.body)? != witness.body_digest
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact restore witness does not verify".to_owned(),
            ));
        }
        Ok(witness)
    }
}

/// One already-opened private artifact file, plus the receipt, projection,
/// cache budget, and integrity mode used to restore its reader.
///
/// `artifact_path` and `artifact_file` are the same no-follow open the
/// connection was created from. Sibling connections reopen that path only
/// after the handle still names the same file.
struct OpenLexicalArtifactConnection<'a> {
    connection: Connection,
    artifact_path: &'a Path,
    artifact_file: &'a File,
    expected: &'a VerifiedCodeLexicalArtifactV1,
    authority: &'a super::super::CodeLexicalProjectionMetadataV1,
    cache_budget_bytes: usize,
    control: &'a dyn CodeIndexExecutionControlV1,
    integrity_authority: ReaderIntegrityAuthorityV1,
}

#[derive(Clone, Copy)]
enum ReaderIntegrityAuthorityV1 {
    /// The immutable artifact was fully verified before publication. Its
    /// durable descriptor supplies the content address, while this open binds
    /// SQLite to one unchanged private file and authenticates the finalized
    /// receipt. Corpus-sized re-verification remains an explicit operation.
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
    let integrity: String = {
        let _span = tracing::trace_span!("query.artifact.open.quick_check").entered();
        {
            connection
                .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
                .map_err(sqlite_corrupt)
        }
    }?;
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
    /// Bounded parallel scans share the existing SQLite cache ceiling. Each
    /// additional active reader must reserve scratch from the process owner.
    pub const MAX_CONCURRENT_READS: usize = 8;

    /// All clones retain the serving owner's admitted reader ceiling. A
    /// concurrent scan reserves its own scratch against that same authority.
    pub fn with_resident_memory_reservation(
        mut self,
        reservation: ResidentMemoryReservationV1,
    ) -> Self {
        self.resident_memory = Some(Arc::new(reservation));
        self
    }

    /// Explicit corpus-wide verification for diagnostics, repair, and
    /// publication checks. Unlike the bounded cold-restore path, this streams
    /// the whole file twice around SQLite's receipt and section verification.
    #[tracing::instrument(
        name = "query.artifact.open_content_addressed_full_verify",
        level = "trace",
        skip_all
    )]
    pub fn open_content_addressed_fully_verified(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::open_content_addressed_fully_verified_with_witness(
            path,
            expected_file_digest,
            expected_file_size_bytes,
            authority,
            cache_budget_bytes,
            RewriteWitness::NATIVE,
            control,
        )
        .map(|(reader, _)| reader)
    }

    /// Perform the explicit full verification and capture the restore witness
    /// from the same retained file handle around its final digest. No witness
    /// is captured when `rewrite_witness` cannot vouch for that file state.
    #[tracing::instrument(
        name = "query.artifact.open_content_addressed_full_verify_with_witness",
        level = "trace",
        skip_all
    )]
    pub fn open_content_addressed_fully_verified_with_witness(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        rewrite_witness: RewriteWitness,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(Self, Option<CodeLexicalArtifactRestoreWitnessV1>), CodeLexicalArtifactErrorV1>
    {
        checkpoint(control)?;
        validate_cache_budget(cache_budget_bytes)?;
        let path = path.as_ref();
        let mut file = open_private_file(path).map_err(map_private_artifact_file_error)?;
        let metadata = file.metadata().map_err(map_artifact_file_error)?;
        if !metadata.file_type().is_file() || metadata.len() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact file does not match the durable head size".to_owned(),
            ));
        }
        if digest_content_addressed_file(&mut file, control)? != *expected_file_digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "artifact file bytes do not match the durable head digest".to_owned(),
            ));
        }
        verify_named_path_identity(path, &file)?;
        let connection = Connection::open_with_flags(
            sqlite_open_path(path)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| map_reader_open_error(path, error))?;
        verify_named_path_identity(path, &file)?;
        // An unserved revision's receipt has another shape; classify the
        // revision before decoding it so an old artifact is Incompatible.
        verify_artifact_state_revision(&connection, control)?;
        let receipt_bytes: Vec<u8> = connection
            .query_row(
                "SELECT receipt FROM artifact_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_corrupt)?;
        let receipt =
            decode_padded_receipt_with_control(&receipt_bytes, control)?.ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "content-addressed lexical artifact has no finalized receipt".to_owned(),
                )
            })?;
        if receipt.file_size_bytes() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt disagrees with the durable head file size".to_owned(),
            ));
        }
        let reader = Self::open_connection_with_control(OpenLexicalArtifactConnection {
            connection,
            artifact_path: path,
            artifact_file: &file,
            expected: &receipt,
            authority,
            cache_budget_bytes,
            control,
            integrity_authority: ReaderIntegrityAuthorityV1::ReceiptOnly,
        })?;
        let witness = witness_verified_artifact(
            path,
            &mut file,
            expected_file_digest,
            reader.receipt.artifact_digest(),
            rewrite_witness,
            control,
        )?;
        Ok((reader, witness))
    }

    /// Open a published artifact whose trust anchor is its content address:
    /// the durable head names the artifact file's size and SHA-256 digest,
    /// the embedded receipt is decoded only after the whole file matches
    /// that digest, and the standard receipt-bound verification then runs
    /// unchanged. This is the reopen path for a durable text head that
    /// survived a daemon restart.
    #[tracing::instrument(
        name = "query.artifact.open_content_addressed",
        level = "trace",
        skip_all
    )]
    pub fn open_content_addressed(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        cache_budget_bytes: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::open_content_addressed_fully_verified(
            path,
            expected_file_digest,
            expected_file_size_bytes,
            authority,
            cache_budget_bytes,
            control,
        )
    }

    /// Restore a publisher-verified immutable artifact with bounded
    /// authentication progress under the query cache budget. The six checks
    /// are fixed-cost with respect to corpus size; no artifact bytes or
    /// section rows are streamed.
    #[tracing::instrument(
        name = "query.artifact.open_content_addressed_bounded",
        level = "trace",
        skip_all
    )]
    pub fn restore_content_addressed_with_progress(
        path: impl AsRef<Path>,
        expected_file_digest: &ManifestDigest,
        expected_file_size_bytes: u64,
        witness: &CodeLexicalArtifactRestoreWitnessV1,
        authority: &super::super::CodeLexicalProjectionMetadataV1,
        control: &dyn CodeIndexExecutionControlV1,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        const TOTAL_RESTORE_CHECKS: u64 = 6;
        progress(0, TOTAL_RESTORE_CHECKS);
        checkpoint(control)?;
        let path = path.as_ref();
        // A durable content address names only a private, no-follow file made
        // by the artifact publisher. Keep that exact handle through the open;
        // path metadata alone cannot bind the bytes SQLite serves.
        let file = open_private_file(path).map_err(map_private_artifact_file_error)?;
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
        let opened_state = stable_artifact_file_state(&file, &metadata)?;
        if witness.body.version != CodeLexicalArtifactRestoreWitnessV1::VERSION
            || CodeLexicalArtifactRestoreWitnessV1::digest_body(&witness.body)?
                != witness.body_digest
            || witness.body.file_digest != *expected_file_digest
            || witness.body.file_state != opened_state
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact restore witness does not match the durable artifact".to_owned(),
            ));
        }
        progress(1, TOTAL_RESTORE_CHECKS);
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        progress(2, TOTAL_RESTORE_CHECKS);
        let connection = {
            let _span = tracing::trace_span!("query.artifact.open.sqlite_connect").entered();
            {
                Connection::open_with_flags(
                    sqlite_open_path(path)?,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|error| map_reader_open_error(path, error))
            }
        }?;
        checkpoint(control)?;
        verify_named_path_identity(path, &file)?;
        {
            let _span = tracing::trace_span!("query.artifact.open.head_schema_verify").entered();
            {
                configure_reader_window(
                    &connection,
                    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
                    0,
                    expected_file_size_bytes,
                )?;
                connection
                    .pragma_update(None, "query_only", true)
                    .map_err(sqlite_error)?;
                verify_artifact_state_revision(&connection, control)?;
                verify_artifact_table_layout(&connection)
            }
        }?;
        progress(3, TOTAL_RESTORE_CHECKS);
        let receipt = {
            let _span = tracing::trace_span!("query.artifact.open.head_receipt_restore").entered();
            {
                let receipt_bytes: Vec<u8> = connection
                    .query_row(
                        "SELECT receipt FROM artifact_state WHERE singleton = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(sqlite_corrupt)?;
                decode_padded_receipt_with_control(&receipt_bytes, control)?.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "content-addressed lexical artifact has no finalized receipt".to_owned(),
                    )
                })
            }
        }?;
        if receipt.file_size_bytes() != expected_file_size_bytes {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt disagrees with the durable head file size".to_owned(),
            ));
        }
        if receipt_artifact_digest(&receipt, receipt.section_digests())?
            != *receipt.artifact_digest()
            || witness.body.receipt_digest != *receipt.artifact_digest()
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "embedded receipt content digest does not verify".to_owned(),
            ));
        }
        progress(4, TOTAL_RESTORE_CHECKS);
        let reader = {
            let _span = tracing::trace_span!("query.artifact.open.reader_restore").entered();
            Self::open_connection_with_control(OpenLexicalArtifactConnection {
                connection,
                artifact_path: path,
                artifact_file: &file,
                expected: &receipt,
                authority,
                cache_budget_bytes: CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
                control,
                integrity_authority: ReaderIntegrityAuthorityV1::ContentAddressedPublisherProof,
            })
        }?;
        progress(5, TOTAL_RESTORE_CHECKS);
        verify_stable_artifact_file_state(&file, &opened_state)?;
        verify_named_path_identity(path, &file)?;
        progress(6, TOTAL_RESTORE_CHECKS);
        crate::observe::Residency::Cold.record("query.artifact.residency");
        Ok(reader)
    }

    #[tracing::instrument(name = "query.artifact.open", level = "trace", skip_all)]
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
        let file = open_private_file(path).map_err(map_private_artifact_file_error)?;
        let metadata = file.metadata().map_err(map_artifact_file_error)?;
        let file_state = stable_artifact_file_state(&file, &metadata)?;
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
        let connection = {
            let _span = tracing::trace_span!("query.artifact.open.sqlite_connect").entered();
            {
                Connection::open_with_flags(
                    sqlite_open_path(path)?,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|error| map_reader_open_error(path, error))
            }
        }?;
        let reader = {
            let _span = tracing::trace_span!("query.artifact.open.reader_restore").entered();
            Self::open_connection_with_control(OpenLexicalArtifactConnection {
                connection,
                artifact_path: path,
                artifact_file: &file,
                expected,
                authority,
                cache_budget_bytes,
                control,
                integrity_authority: ReaderIntegrityAuthorityV1::ReceiptOnly,
            })
        }?;
        verify_stable_artifact_file_state(&file, &file_state)?;
        verify_named_path_identity(path, &file)?;
        crate::observe::Residency::Warm.record("query.artifact.residency");
        Ok(reader)
    }

    /// `authority` is the opener's projection: the artifact stores only its
    /// content part, and the reader serves the opener's generation,
    /// repository, freshness, and clone route.
    fn open_connection_with_control(
        OpenLexicalArtifactConnection {
            connection,
            artifact_path,
            artifact_file,
            expected,
            authority,
            cache_budget_bytes,
            control,
            integrity_authority,
        }: OpenLexicalArtifactConnection<'_>,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        let sealed_file_size_bytes = expected.file_size_bytes();
        {
            let _span = tracing::trace_span!("query.artifact.open.schema_verify").entered();
            {
                connection
                    .pragma_update(None, "query_only", true)
                    .map_err(sqlite_error)?;
                verify_artifact_state_revision(&connection, control)?;
                verify_artifact_table_layout(&connection)
            }
        }?;
        // Read the BLOB length first so the page cache can be configured
        // before metadata is materialized. The retained metadata copy plus
        // SQLite's cache therefore cannot exceed the caller's reservation.
        let (page_cache_bytes, stored_metadata_bytes, stored_metadata_digest, metadata) = {
            let _span = tracing::trace_span!("query.artifact.open.metadata_restore").entered();
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
                let width = Self::MAX_CONCURRENT_READS
                    .min(sqlite_budget / ARTIFACT_SQLITE_CACHE_FLOOR_BYTES);
                let page_cache_bytes = configure_reader_window(
                    &connection,
                    sqlite_budget.min(ARTIFACT_SQLITE_CACHE_BYTES) / width,
                    0,
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
        }?;
        // Content-addressed reopen has stronger authority than `quick_check`:
        // the builder ran SQLite integrity verification before publication,
        // and this reader hashes the exact immutable file both before and
        // after opening it. Repeating a corpus-wide SQLite scan added tens of
        // seconds without authenticating any bytes the two hashes did not.
        verify_reader_sqlite_integrity(&connection, integrity_authority)?;
        checkpoint(control)?;
        let stored = {
            let _span = tracing::trace_span!("query.artifact.open.receipt_restore").entered();
            {
                let receipt_bytes: Vec<u8> = connection
                    .query_row(
                        "SELECT receipt FROM artifact_state WHERE singleton = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
                decode_padded_receipt_with_control(&receipt_bytes, control)?.ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact has no finalized receipt".to_owned(),
                    )
                })
            }
        }?;
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
        let sections = if matches!(
            integrity_authority,
            ReaderIntegrityAuthorityV1::ContentAddressedPublisherProof
        ) {
            stored.section_digests().to_vec()
        } else {
            let sections = {
                let _span =
                    tracing::trace_span!("query.artifact.open.section_digest_verify").entered();
                compute_section_digests(&connection, control)
            }?;
            if sections != stored.section_digests() {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact section digests do not verify".to_owned(),
                ));
            }
            sections
        };
        let digest = {
            let _span =
                tracing::trace_span!("query.artifact.open.artifact_digest_verify").entered();
            receipt_artifact_digest(&stored, &sections)
        }?;
        if &digest != stored.artifact_digest() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact content digest does not verify".to_owned(),
            ));
        }
        checkpoint(control)?;
        let width = Self::MAX_CONCURRENT_READS.min(
            (cache_budget_bytes - stored_metadata_bytes.len()) / ARTIFACT_SQLITE_CACHE_FLOOR_BYTES,
        );
        let mut connections = vec![StdMutex::new(connection)];
        let sibling_open_path = sqlite_open_path(artifact_path)?;
        for _ in 1..width {
            checkpoint(control)?;
            verify_named_path_identity(artifact_path, artifact_file)?;
            let sibling = Connection::open_with_flags(
                &sibling_open_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| map_reader_open_error(artifact_path, error))?;
            verify_named_path_identity(artifact_path, artifact_file)?;
            sibling
                .pragma_update(None, "query_only", true)
                .map_err(sqlite_error)?;
            configure_reader_window(&sibling, page_cache_bytes, 0, sealed_file_size_bytes)?;
            connections.push(StdMutex::new(sibling));
        }
        let retained_owned_bytes = stored_metadata_bytes.len() + page_cache_bytes * width;
        Ok(Self {
            connections: Arc::new(ArtifactReaders {
                connections,
                queue: StdMutex::new(()),
                available: Condvar::new(),
            }),
            resident_memory: None,
            metadata,
            receipt: stored,
            retained_owned_bytes,
            fuzzy_vocabulary: Arc::new(OnceLock::new()),
            scoring_prefaces: Arc::new(StdMutex::new(None)),
            preface_ceiling_bytes: cache_budget_bytes.saturating_sub(retained_owned_bytes),
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
        let connection = self.lock_connection(None)?;
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
        let stored = RowBlocksV1::new(&connection, ScoringPrefaceIndexV1::empty()).row(
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
        let connection = self.lock_connection(Some(&|| checkpoint(control)))?;
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
                        after: *ordinal,
                    })
            })
            .flatten();
        members.truncate(limit);
        Ok(CloneArtifactPageV1 {
            members: members.into_iter().map(|(_, member)| member).collect(),
            next_cursor,
        })
    }

    pub fn clone_body(
        &self,
        symbol: &SymbolOccurrenceId,
    ) -> Result<Option<CodeIndexCloneBodyV1>, CodeLexicalArtifactErrorV1> {
        let route = self.clone_route()?;
        let connection = self.lock_connection(None)?;
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
        let connection = self.lock_connection(None)?;
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
                Ok(cursor.after)
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

    /// Handles and their cache are fixed at verified open. Waiters use any
    /// released handle; requests keep checking cancellation while parked.
    fn lock_connection(
        &self,
        check: Option<&dyn Fn() -> Result<(), CodeLexicalArtifactErrorV1>>,
    ) -> Result<ArtifactReadGuard<'_>, CodeLexicalArtifactErrorV1> {
        {
            let _span = tracing::trace_span!("query.artifact.reader.lock_wait").entered();
            {
                let poisoned = || {
                    CodeLexicalArtifactErrorV1::Io(
                        "lexical artifact reader lock is poisoned".to_owned(),
                    )
                };
                let readers = self.connections.as_ref();
                let mut queue = readers.queue.lock().map_err(|_| poisoned())?;
                loop {
                    for (index, connection) in readers.connections.iter().enumerate() {
                        let connection = match connection.try_lock() {
                            Ok(connection) => connection,
                            Err(TryLockError::WouldBlock) => continue,
                            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
                        };
                        let scratch_reservation = if index == 0 {
                            None
                        } else {
                            let Some(reservation) = &self.resident_memory else {
                                break;
                            };
                            let bytes = NonZeroU64::new(
                                CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 as u64,
                            )
                            .ok_or_else(|| {
                                CodeLexicalArtifactErrorV1::Contract(
                                    "reader reservation must be nonzero".to_owned(),
                                )
                            })?;
                            match reservation.reserve_additional(bytes) {
                                Ok(reservation) => Some(reservation),
                                Err(reason) => {
                                    tracing::debug!(%reason, "concurrent artifact read queues within its existing reservation");
                                    break;
                                }
                            }
                        };
                        return Ok(ArtifactReadGuard {
                            connection,
                            _scratch_reservation: scratch_reservation,
                            _wakeup: ArtifactReaderWakeup(readers),
                        });
                    }
                    queue = if let Some(check) = check {
                        check()?;
                        // The read-control trait exposes a checkpoint, not a wake
                        // signal. The condvar handles release; timed waits bound
                        // cancellation/deadline observation while all handles run.
                        readers
                            .available
                            .wait_timeout(queue, Duration::from_millis(10))
                            .map_err(|_| poisoned())?
                            .0
                    } else {
                        readers.available.wait(queue).map_err(|_| poisoned())?
                    };
                    if let Some(check) = check {
                        check()?;
                    }
                }
            }
        }
    }

    fn validate_generation(&self, generation: &CodeGenerationId) -> Result<(), RetrievalPortError> {
        if generation != &self.metadata.generation {
            Err(RetrievalPortError::GenerationMismatch)
        } else {
            Ok(())
        }
    }

    /// Every block's scoring preface, shared by later queries on this reader.
    /// The first lexical read pays the scan while concurrent first reads wait
    /// for it rather than each holding their own copy; a warm common-term
    /// walk then looks up field lengths without re-reading row blocks.
    fn scoring_preface_index(
        &self,
        connection: &Connection,
        control: &dyn RetrievalExecutionControl,
    ) -> Result<Arc<ScoringPrefaceIndexV1>, CodeLexicalArtifactErrorV1> {
        let mut slot = self.scoring_prefaces.lock().map_err(|_| {
            CodeLexicalArtifactErrorV1::Io("lexical artifact preface lock is poisoned".to_owned())
        })?;
        if let Some(cached) = slot.as_ref() {
            return Ok(Arc::clone(cached));
        }
        let loaded = Arc::new({
            let _span = tracing::trace_span!("query.artifact.preface.load").entered();
            {
                load_scoring_preface_index(connection, self.preface_ceiling_bytes, || {
                    retrieval_checkpoint(control).map_err(|_| {
                        CodeLexicalArtifactErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
                    })
                })
            }
        }?);

        *slot = Some(Arc::clone(&loaded));
        Ok(loaded)
    }
}

impl LexicalPostingReadPort for CodeLexicalArtifactReaderV1 {
    #[tracing::instrument(name = "query.lane.lexical.read", level = "trace", skip_all)]
    fn read_lexical_postings(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        self.validate_generation(&request.generation)?;
        if self.metadata.freshness.compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::observe::Residency::Rebuilding.record("query.lane.lexical.residency");
            return Ok(RetrieverOutcome::Stale(self.metadata.freshness.clone()));
        }
        let connection = self
            .lock_connection(Some(&|| {
                if request.control.is_cancelled() {
                    Err(CodeLexicalArtifactErrorV1::Interrupted(
                        CodeIndexInterruptionV1::Cancelled,
                    ))
                } else {
                    Ok(())
                }
            }))
            .map_err(map_query_artifact_error)?;
        let prefaces = self
            .scoring_preface_index(&connection, request.control)
            .map_err(map_query_artifact_error)?;
        let outcome = ArtifactQueryV1::new(
            &connection,
            &self.metadata,
            &self.receipt,
            &self.fuzzy_vocabulary,
            prefaces.as_ref(),
        )?
        .lexical_batch(request)?;
        crate::observe::record_lane("query.lane.lexical.residency", &outcome);
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
    #[tracing::instrument(name = "query.lane.exact.read", level = "trace", skip_all)]
    fn read_exact_postings(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.reader.validate_generation(&request.generation)?;
        if self.reader.metadata.freshness.compatibility
            != tracedecay_domain::FreshnessCompatibilityV1::Current
        {
            crate::observe::Residency::Rebuilding.record("query.lane.exact.residency");
            return Ok(RetrieverOutcome::Stale(
                self.reader.metadata.freshness.clone(),
            ));
        }
        let connection = self
            .reader
            .lock_connection(Some(&|| {
                if request.control.is_cancelled() {
                    Err(CodeLexicalArtifactErrorV1::Interrupted(
                        CodeIndexInterruptionV1::Cancelled,
                    ))
                } else {
                    Ok(())
                }
            }))
            .map_err(map_query_artifact_error)?;
        let outcome = ArtifactQueryV1::new(
            &connection,
            &self.reader.metadata,
            &self.reader.receipt,
            &self.reader.fuzzy_vocabulary,
            ScoringPrefaceIndexV1::empty(),
        )?
        .exact_batch(request, &self.authority)?;
        crate::observe::record_lane("query.lane.exact.residency", &outcome);
        Ok(outcome)
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
/// to I/O wait or to SHA-256 work. `record_bytes` sees each buffer read.
#[inline]
pub(super) fn hash_artifact_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
    mut record_bytes: impl FnMut(&[u8]) -> Result<(), CodeLexicalArtifactErrorV1>,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; ARTIFACT_DIGEST_READ_BUFFER_BYTES_V1];
    loop {
        checkpoint(control)?;
        let read = {
            let _span = tracing::trace_span!("query.artifact.digest.file_read").entered();
            {
                file.read(&mut buffer)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))
            }
        }?;
        if read == 0 {
            break;
        }
        record_bytes(&buffer[..read])?;
        {
            let _span = tracing::trace_span!("query.artifact.digest.sha256_update").entered();
            hasher.update(&buffer[..read]);
        }
    }
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
}

fn digest_content_addressed_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let _span = tracing::trace_span!("query.artifact.digest.content_address_preopen").entered();
    hash_artifact_file(file, control, |_| Ok(()))
}

fn digest_retained_artifact_file(
    file: &mut File,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<ManifestDigest, CodeLexicalArtifactErrorV1> {
    let _span = tracing::trace_span!("query.artifact.digest.retained_post_validation").entered();
    hash_artifact_file(file, control, |_| Ok(()))
}

fn stable_artifact_file_state(
    #[cfg_attr(unix, expect(unused_variables))] file: &File,
    metadata: &Metadata,
) -> Result<StableArtifactFileStateV1, CodeLexicalArtifactErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(StableArtifactFileStateV1 {
            len: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            change_seconds: metadata.ctime(),
            change_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(windows)]
    {
        let information = tracedecay_private_fs::windows_file::information(file)
            .map_err(map_artifact_file_error)?;
        Ok(StableArtifactFileStateV1 {
            len: metadata.len(),
            volume_serial_number: information.volume_serial_number,
            file_index: information.file_index,
            last_write_time: metadata.last_write_time(),
        })
    }
}

fn verify_stable_artifact_file_state(
    file: &File,
    expected: &StableArtifactFileStateV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let metadata = file.metadata().map_err(map_artifact_file_error)?;
    let actual = stable_artifact_file_state(file, &metadata)?;
    if actual != *expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "artifact file changed while its serving reader opened".to_owned(),
        ));
    }
    Ok(())
}

/// Re-verify a fully verified artifact's bytes on its retained handle and
/// issue the restore witness for the file state they were read under.
///
/// The witness asks whether that state can vouch before the final digest
/// reads the bytes: a change time still inside the kernel's clock quantum can
/// be shared by a same-size rewrite, so a restore that trusted it would serve
/// the rewritten file under the old digest. Such a state issues no witness and
/// the next open re-verifies the content digest instead.
fn witness_verified_artifact(
    path: &Path,
    file: &mut File,
    file_digest: &ManifestDigest,
    receipt_digest: &ManifestDigest,
    rewrite_witness: RewriteWitness,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Option<CodeLexicalArtifactRestoreWitnessV1>, CodeLexicalArtifactErrorV1> {
    let metadata = file.metadata().map_err(map_artifact_file_error)?;
    let verified_state = stable_artifact_file_state(file, &metadata)?;
    let vouched = rewrite_witness.vouches_for_unchanged_bytes(&metadata);
    verify_retained_artifact_digest(file, file_digest, control)?;
    verify_stable_artifact_file_state(file, &verified_state)?;
    verify_named_path_identity(path, file)?;
    vouched
        .then(|| {
            CodeLexicalArtifactRestoreWitnessV1::from_verified_file_state(
                file_digest.clone(),
                receipt_digest.clone(),
                verified_state,
            )
        })
        .transpose()
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
        grain: row.anchor.grain,
        parent_chunk: row.anchor.parent_chunk_id,
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
        CodeLexicalArtifactErrorV1::StaleCloneFamilyCursor => RetrievalPortError::StaleEvidence,
        CodeLexicalArtifactErrorV1::Unreserved(_)
        | CodeLexicalArtifactErrorV1::BatchTooLarge { .. } => RetrievalPortError::BudgetExceeded,
        CodeLexicalArtifactErrorV1::Corrupt(error)
        | CodeLexicalArtifactErrorV1::Io(error)
        | CodeLexicalArtifactErrorV1::Missing(error) => {
            RetrievalPortError::AuthorityUnavailable(error)
        }
    }
}
