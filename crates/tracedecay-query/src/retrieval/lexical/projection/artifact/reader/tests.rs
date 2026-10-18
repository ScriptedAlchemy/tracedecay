use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use roaring::RoaringBitmap;
use rusqlite::hooks::{AuthAction, Authorization};
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CodeGenerationId, ComponentRevision, FreshnessCompatibilityV1, ManifestDigest, ScoreDomainId,
    SourceFreshness, SourceInstanceKey, SourceNamespace, UtcMicros,
};
use tracedecay_private_fs::{RewriteWitness, open_private_file};

use super::super::format::{PostingListEncoderV1, encode_document_set};
use super::super::row_codec::{BlockRowV1, RowBlocksV1, ScoringPrefaceIndexV1, encode_row_blocks};
use super::{
    ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1, ARTIFACT_NGRAM_MAX_CANDIDATES_V1,
    ARTIFACT_SQLITE_CACHE_BYTES, ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1,
    ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1, ArtifactQueryMetricsV1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1,
    CodeLexicalArtifactReaderV1, CodeLexicalArtifactRestoreWitnessV1, LexicalFieldV1,
    NGRAM_NORMALIZED, RequestTermPostingsV1, TermFieldPostingsV1, charge_ngram_encoded_list_bytes,
    configure_reader_window, ensure_ngram_candidate_cardinality, ensure_sqlite_bind_capacity,
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
    let digest = ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).expect("digest fixture");

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

#[test]
fn restore_witness_decode_refuses_a_body_with_a_foreign_self_digest() {
    let directory = tempfile::tempdir().expect("witness tempdir");
    let path = directory.path().join("artifact.bin");
    std::fs::write(&path, b"artifact identity fixture").expect("write artifact fixture");
    let file = std::fs::File::open(&path).expect("open artifact fixture");
    let digest =
        ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).expect("artifact digest");
    let receipt =
        ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).expect("receipt digest");
    let witness = CodeLexicalArtifactRestoreWitnessV1::from_verified_file_state(
        digest,
        receipt,
        super::stable_artifact_file_state(&file, &file.metadata().expect("artifact metadata"))
            .expect("stable file state"),
    )
    .expect("create restore witness");
    let mut encoded: serde_json::Value =
        serde_json::from_slice(&witness.encode().expect("encode witness"))
            .expect("parse encoded witness");
    encoded["body"]["receipt_digest"] =
        serde_json::Value::String(format!("sha256:{}", "3".repeat(64)));

    let error = CodeLexicalArtifactRestoreWitnessV1::decode(
        &serde_json::to_vec(&encoded).expect("encode corrupt witness"),
    )
    .expect_err("the self digest must bind every witness field");

    assert!(matches!(error, CodeLexicalArtifactErrorV1::Corrupt(_)));
}

/// A private SQLite artifact verified the way a full verification ends:
/// its digest, size, and the restore witness issued under
/// `rewrite_witness`.
#[cfg(unix)]
fn witnessed_artifact(
    path: &std::path::Path,
    rewrite_witness: RewriteWitness,
) -> (
    ManifestDigest,
    u64,
    Option<CodeLexicalArtifactRestoreWitnessV1>,
) {
    use std::os::unix::fs::PermissionsExt;

    let connection = Connection::open(path).expect("create SQLite artifact");
    connection
        .pragma_update(None, "user_version", 1i64)
        .expect("seed artifact");
    drop(connection);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("make artifact private");
    let mut file = open_private_file(path).expect("open private artifact");
    let digest = super::digest_content_addressed_file(&mut file, &AlwaysActiveControl)
        .expect("digest artifact");
    let size = file.metadata().expect("artifact metadata").len();
    let receipt =
        ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).expect("receipt digest");
    let witness = super::witness_verified_artifact(
        path,
        &mut file,
        &digest,
        &receipt,
        rewrite_witness,
        &AlwaysActiveControl,
    )
    .expect("verify artifact and issue its witness");
    (digest, size, witness)
}

/// Rewrite the artifact's bytes in place at the same size and put its
/// modification time back, so only content or the change time can tell.
#[cfg(unix)]
fn rewrite_in_place_restoring_mtime(path: &std::path::Path) {
    use std::os::unix::fs::FileExt;

    /// The SQLite header's big-endian `user_version` field.
    const USER_VERSION_OFFSET: u64 = 60;
    let before = std::fs::metadata(path).expect("artifact metadata");
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("reopen artifact for mutation");
    file.write_all_at(&2u32.to_be_bytes(), USER_VERSION_OFFSET)
        .expect("rewrite the same inode");
    file.set_modified(before.modified().expect("artifact mtime"))
        .expect("restore artifact mtime");
    let after = file.metadata().expect("rewritten metadata");
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().ok(), before.modified().ok());
}

#[cfg(unix)]
const DIGEST_REFUSAL: &str =
    "lexical artifact is corrupt: artifact file bytes do not match the durable head digest";

/// Verify a fresh artifact under `rewrite_witness`, rewrite it in place,
/// and reopen it the way serving does: a restore witness takes the
/// bounded restore, no witness re-verifies the content digest. Returns
/// whether a witness was issued and the reopen's refusal.
#[cfg(unix)]
fn reopen_after_in_place_rewrite(rewrite_witness: RewriteWitness) -> (bool, String) {
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let path = directory.path().join("artifact.sqlite");
    let (digest, size, witness) = witnessed_artifact(&path, rewrite_witness);

    rewrite_in_place_restoring_mtime(&path);

    let error = match &witness {
        Some(witness) => CodeLexicalArtifactReaderV1::restore_content_addressed_with_progress(
            &path,
            &digest,
            size,
            witness,
            &opener(),
            &AlwaysActiveControl,
            |_, _| {},
        )
        .expect_err("a rewritten artifact must not restore"),
        None => CodeLexicalArtifactReaderV1::open_content_addressed_fully_verified(
            &path,
            &digest,
            size,
            &opener(),
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &AlwaysActiveControl,
        )
        .expect_err("a rewritten artifact must not verify"),
    };
    (witness.is_some(), error.to_string())
}

#[cfg(unix)]
#[test]
fn bounded_restore_refuses_a_same_inode_rewrite_witness() {
    let (witnessed, refusal) = reopen_after_in_place_rewrite(RewriteWitness::NATIVE);

    let expected = if witnessed {
        "lexical artifact is corrupt: lexical artifact restore witness does not match the \
         durable artifact"
    } else {
        DIGEST_REFUSAL
    };
    assert_eq!(refusal, expected);
}

#[cfg(unix)]
#[test]
fn a_state_the_witness_cannot_vouch_for_reverifies_the_rewritten_bytes() {
    assert_eq!(
        reopen_after_in_place_rewrite(RewriteWitness::Absent),
        (false, DIGEST_REFUSAL.to_owned())
    );
}

#[cfg(unix)]
#[test]
fn bounded_restore_refuses_path_replacement_even_with_identical_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("artifact tempdir");
    let path = directory.path().join("artifact.sqlite");
    let replacement = directory.path().join("replacement.sqlite");
    let connection = Connection::open(&path).expect("create SQLite artifact");
    connection
        .pragma_update(None, "user_version", 1i64)
        .expect("seed artifact");
    drop(connection);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("make artifact private");
    let mut file = open_private_file(&path).expect("open private artifact");
    let digest = super::digest_content_addressed_file(&mut file, &AlwaysActiveControl)
        .expect("digest artifact");
    let size = file.metadata().expect("artifact metadata").len();
    let receipt =
        ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).expect("receipt digest");
    let witness = CodeLexicalArtifactRestoreWitnessV1::from_verified_file_state(
        digest.clone(),
        receipt,
        super::stable_artifact_file_state(&file, &file.metadata().expect("artifact metadata"))
            .expect("stable file state"),
    )
    .expect("create restore witness");
    drop(file);
    std::fs::copy(&path, &replacement).expect("copy identical artifact bytes");
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600))
        .expect("make replacement private");
    std::fs::rename(&replacement, &path).expect("replace artifact pathname");

    let error = CodeLexicalArtifactReaderV1::restore_content_addressed_with_progress(
        &path,
        &digest,
        size,
        &witness,
        &opener(),
        &AlwaysActiveControl,
        |_, _| {},
    )
    .expect_err("native file identity must refuse path replacement");

    assert!(matches!(error, CodeLexicalArtifactErrorV1::Corrupt(_)));
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
            super::verify_retained_artifact_digest(&mut retained, &expected, &AlwaysActiveControl,),
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

    let one_shot = ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
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
            if message == "artifact file does not match the durable head size"
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
    let matching = ngram_bitmap_candidates(&connection, &normalized(&[10, 20]), &bounded_metrics)
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
    let lengths = vec![
        BTreeMap::<LexicalFieldV1, usize>::new();
        usize::try_from(documents).expect("document count")
    ];
    let rows = (0..documents)
        .map(|document| {
            let index = usize::try_from(document).expect("document index");
            BlockRowV1 {
                document_id: i64::from(document),
                chunk_id: &chunk_ids[index],
                parent_chunk_id: None,
                row: &payloads[index],
                text: "fn alpha() {}",
                field_lengths: &lengths[index],
                trimmed_normalized_len: 0,
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
        &RowBlocksV1::new(&connection, ScoringPrefaceIndexV1::empty()),
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
        &RowBlocksV1::new(&connection, ScoringPrefaceIndexV1::empty()),
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
        &RowBlocksV1::new(&connection, ScoringPrefaceIndexV1::empty()),
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

    let rows = RowBlocksV1::new(&connection, ScoringPrefaceIndexV1::empty());
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
fn fuzzy_length_window_keeps_load_order_and_drops_impossible_lengths() {
    let vocabulary = super::FuzzyVocabularyV1::from_terms(vec![
        "aa".to_owned(),
        "planningplanning".to_owned(),
        "ab".to_owned(),
        "abc".to_owned(),
    ])
    .expect("vocabulary");
    assert_eq!(
        vocabulary.indices_within(1, 3),
        vec![0, 2, 3],
        "a distance-1 window around a 2-character term skips the long term and stays in load order"
    );
    assert!(vocabulary.indices_within(8, 8).is_empty());
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
