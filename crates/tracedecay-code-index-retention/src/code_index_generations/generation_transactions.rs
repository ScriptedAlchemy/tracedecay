//! Durable generation-retention transactions and graph-replay pool exposure.
//!
//! Quarantined generations are journaled, then hard-linked into the replay pool before the receipt is durable.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};
use tracedecay_domain::CodeGenerationId;
use tracedecay_domain::canonical_text::{encode_lowercase_hex, is_lowercase_hex};

use super::graph_replay_release;
use super::journal::{
    BoundedJournalSpec, clear_journal, journal_path, load_journal, persist_journal,
};
use super::locking::{CodeGenerationStoreLockV1, try_acquire_code_generation_store_lock};
use super::receipt_store;
use super::receipt_store::ReceiptStoreSpec;
use super::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionGenerationV1,
    CodeGenerationRetentionReceiptV1, CodeGenerationRetentionTransactionV1, GENERATIONS_DIRECTORY,
    GRAPH_REPLAY_POOL_ACQUIRE_BUDGET, GRAPH_REPLAY_POOL_ACQUIRE_POLL, MAX_TRANSACTION_BYTES,
    QUARANTINE_DIRECTORY, RECEIPT_SCHEMA, RECEIPTS_DIRECTORY, RETENTION_POINTER_ROLLBACK_CONTEXT,
    RETENTION_POINTER_WRITE_CONTEXT, TRANSACTION_FILE, TRANSACTION_SCHEMA, observe_cancel,
    read_optional_active_pointer, storage, sync_directory, total_bytes, validate_generation_file,
    write_active_pointer,
};

const GENERATION_TRANSACTION_JOURNAL: BoundedJournalSpec<CodeGenerationRetentionTransactionV1> =
    BoundedJournalSpec {
        file_name: TRANSACTION_FILE,
        max_bytes: MAX_TRANSACTION_BYTES,
        label: "retention transaction",
        write_context: "code-generation-retention-transaction",
        validate: validate_transaction,
    };

pub(super) const GENERATION_RECEIPT_STORE: ReceiptStoreSpec = ReceiptStoreSpec {
    directory: RECEIPTS_DIRECTORY,
    label: "retention receipt",
};

pub(super) fn transaction_path(store_root: &Path) -> PathBuf {
    journal_path(store_root, &GENERATION_TRANSACTION_JOURNAL)
}
pub(super) fn transaction_stage_root(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> PathBuf {
    store_root
        .join(QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
}

pub(super) fn persist_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    persist_journal(store_root, &GENERATION_TRANSACTION_JOURNAL, transaction)
}

pub(super) fn load_transaction(
    store_root: &Path,
) -> Result<Option<CodeGenerationRetentionTransactionV1>, CodeGenerationRetentionErrorV1> {
    load_journal(store_root, &GENERATION_TRANSACTION_JOURNAL)
}

pub(super) fn validate_transaction(
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if transaction.schema != TRANSACTION_SCHEMA || transaction.receipt.schema != RECEIPT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction has an incompatible schema".to_owned(),
        ));
    }
    // Every writer emits lowercase digests, so mixed case is forgery or
    // corruption, never a legitimate journal.
    if !is_lowercase_hex(&transaction.receipt.receipt_digest, 64) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction receipt digest is not a SHA-256 file component".to_owned(),
        ));
    }
    let pointer_generation = transaction
        .active_pointer
        .as_ref()
        .map(|pointer| {
            validate_generation_file(&pointer.generation_file)?;
            CodeGenerationId::new(pointer.generation_id.clone()).map_err(|error| {
                CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "retention transaction active generation id is invalid: {error}"
                ))
            })
        })
        .transpose()?;
    if pointer_generation != transaction.receipt.active_generation_id {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction active pointer does not match its receipt".to_owned(),
        ));
    }
    validate_transaction_pointer_rewrite(transaction)?;
    let mut generation_ids = BTreeSet::new();
    let mut generation_files = BTreeSet::new();
    for generation in &transaction.receipt.deleted_generations {
        validate_generation_file(&generation.generation_file)?;
        if !generation_ids.insert(generation.generation_id.clone())
            || !generation_files.insert(generation.generation_file.clone())
        {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "retention transaction has duplicate generation identities".to_owned(),
            ));
        }
    }
    if generation_ids.is_empty()
        || transaction
            .receipt
            .active_generation_id
            .as_ref()
            .is_some_and(|active| generation_ids.contains(active))
        || !transaction
            .receipt
            .vector_readable_sources
            .is_disjoint(&generation_ids)
        || transaction.receipt.reclaimed_bytes
            != total_bytes(&transaction.receipt.deleted_generations)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction violates exact liveness or byte invariants".to_owned(),
        ));
    }
    Ok(())
}

/// The journal carries the pre-collection pointer and the exact deleted set,
/// so the post-rewrite pointer is derivable from it. Proving that derivation
/// here means a journal that survives a crash can never be replayed into a
/// pointer this transaction could not have published.
fn validate_transaction_pointer_rewrite(
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    transaction.rewritten_pointer().map(|_| ())
}

pub(super) fn receipt_is_durable(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    receipt_store::receipt_is_durable(
        store_root,
        &GENERATION_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

pub(super) fn write_receipt(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    receipt_store::write_receipt(
        store_root,
        &GENERATION_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

#[hotpath::measure(label = "usecases.retention.stage")]
pub(super) fn stage_collectable_generations(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    std::fs::create_dir_all(&stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState("retention quarantine has no parent".to_owned())
    })?)?;

    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        let staged = stage_root.join(&generation.generation_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {
                // No-follow: `regular_file_exists` already proved the path is
                // a regular file, and sizing through a racing symlink would
                // measure foreign bytes.
                let metadata = std::fs::symlink_metadata(&source).map_err(storage)?;
                if metadata.len() != generation.size_bytes {
                    return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                        "collectable generation '{}' changed after the mark phase",
                        generation.generation_file
                    )));
                }
                std::fs::rename(&source, &staged).map_err(storage)?;
                sync_directory(&generations_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' is missing before quarantine",
                    generation.generation_file
                )));
            }
            (false, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' was already quarantined",
                    generation.generation_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' exists in both source and quarantine",
                    generation.generation_file
                )));
            }
        }
    }
    crate::hotpath_observe::retention_quarantined(
        transaction
            .receipt
            .deleted_generations
            .iter()
            .map(|generation| generation.size_bytes)
            .sum(),
    );
    Ok(())
}

/// The same filesystem lock authority used by graph replay publication and
/// staged unlink. Keeping the guard typed with its canonical root prevents a
/// retention helper from accidentally operating under a different pool's
/// lock.
pub(super) struct GraphReplayPoolLockV1 {
    root: PathBuf,
    guard: Option<CodeGenerationStoreLockV1>,
}

impl Drop for GraphReplayPoolLockV1 {
    fn drop(&mut self) {
        self.release_exclusive();
    }
}

/// Convenience acquire for tests and recovery helpers that have no caller
/// deadline of their own. Production collection must call
/// [`acquire_graph_replay_pool_lock_checked`] with the carried deadline
/// and cancellation so a publisher that wins the probe-to-execute window
/// cannot park the executor on a blocking flock.
pub(super) fn acquire_graph_replay_pool_lock(
    pool_root: &Path,
) -> Result<GraphReplayPoolLockV1, CodeGenerationRetentionErrorV1> {
    acquire_graph_replay_pool_lock_checked(
        pool_root,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        &|| false,
    )
}

pub(super) fn acquire_graph_replay_pool_lock_checked(
    pool_root: &Path,
    deadline: Instant,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<GraphReplayPoolLockV1, CodeGenerationRetentionErrorV1> {
    GraphReplayPoolLockV1::acquire_exclusive(pool_root, deadline, is_cancelled)
}

impl GraphReplayPoolLockV1 {
    fn acquire_exclusive(
        pool_root: &Path,
        deadline: Instant,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        ensure_private_graph_replay_pool_root(pool_root)?;
        // Honor a sooner caller deadline, but never wait longer than the
        // executor budget. A 30s graph-operation deadline would still pin
        // the daemon writer gate for the whole seal-hash.
        let deadline = deadline.min(Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET);
        loop {
            if observe_cancel(is_cancelled) {
                crate::hotpath_observe::retention_replay_pool_acquire_cancelled();
                return Err(CodeGenerationRetentionErrorV1::Cancelled);
            }
            match try_acquire_code_generation_store_lock(pool_root)? {
                Some(guard) => {
                    crate::hotpath_observe::retention_replay_pool_acquired();
                    return Ok(Self {
                        root: guard.generation_store_root()?.to_path_buf(),
                        guard: Some(guard),
                    });
                }
                None if Instant::now() >= deadline => {
                    crate::hotpath_observe::retention_replay_pool_busy();
                    return Err(CodeGenerationRetentionErrorV1::GraphReplayPoolBusy);
                }
                None => Self::wait_for_exclusive(deadline),
            }
        }
    }

    fn wait_for_exclusive(deadline: Instant) {
        crate::hotpath_observe::retention_replay_pool_acquire_wait();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::park_timeout(remaining.min(GRAPH_REPLAY_POOL_ACQUIRE_POLL));
    }

    fn release_exclusive(&mut self) {
        if self.guard.take().is_some() {
            crate::hotpath_observe::retention_replay_pool_released();
        }
    }
}

/// Expose every quarantined generation to the graph replay pool by hard link
/// before its release event becomes durable. The pool entry is the sealed
/// generation's survival path once the canonical file is unlinked; linking
/// before `write_receipt` publishes the release events guarantees the replay
/// reconciler can never observe an event whose pool copy is still missing and
/// complete it early, which would strand a later-linked copy as an
/// unreclaimable orphan.
///
/// A destination that already exists is never trusted by name alone: the
/// digest-named path could hold a corrupt regular file, a symlink, or a
/// directory, and accepting it would let `write_receipt` publish deletion
/// evidence whose pool copy is unusable. The collision is verified byte for
/// byte against the staged sealed source and retention fails closed on any
/// mismatch, before any receipt is published.
///
/// Establish the graph replay pool root under the store-owned first-create
/// contract: retention creates the pool owner-private (0700) on first use,
/// and any pre-existing path must already validate as an owner-private
/// directory. The pool root sits directly beside the graph database
/// (`database_path().with_extension("graph-replay")`), so its parent always
/// exists and no ancestors are ever manufactured the way `create_dir_all`
/// would — under a permissive umask that would hard-link already-private
/// sealed generations into a world-readable pool.
pub(super) fn ensure_private_graph_replay_pool_root(
    pool_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let unsafe_pool_root = |error: &std::io::Error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "graph replay pool root '{}' is not an owner-private directory: {error}",
            pool_root.display()
        ))
    };
    match tracedecay_private_fs::create_private_directory(pool_root) {
        Ok(()) => Ok(()),
        // A concurrent creator may win the creation race; the existing path
        // is acceptable only if it is already an owner-private directory.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tracedecay_private_fs::validate_private_directory(pool_root).map_err(
                |error| match error.kind() {
                    std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::InvalidInput
                    | std::io::ErrorKind::NotADirectory => unsafe_pool_root(&error),
                    _ => storage(error),
                },
            )
        }
        Err(error) => Err(storage(error)),
    }
}

/// Lock order is the code-generation store lock first, then the pool lock:
/// every caller already holds the store lock, and the daemon's replay
/// reconciler serializes its pool unlinks behind this same canonical pool
/// lock (`lock_project_graph_replay_pool`), so an entry cannot be swapped or
/// retired between the collision probe and its identity verification.
pub(super) fn expose_staged_generations_under_graph_replay_pool_lock(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    pool_lock: &GraphReplayPoolLockV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    let pool_root = &pool_lock.root;
    let mut linked = false;
    for generation in &transaction.receipt.deleted_generations {
        let staged = stage_root.join(&generation.generation_file);
        if !regular_file_exists(&staged)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "staged generation '{}' is missing before graph replay exposure",
                generation.generation_file
            )));
        }
        match std::fs::hard_link(&staged, pool_root.join(&generation.generation_file)) {
            Ok(()) => linked = true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing_graph_replay_pool_entry(
                    &pool_root.join(&generation.generation_file),
                    &staged,
                    generation,
                )?;
            }
            Err(error) => return Err(storage(error)),
        }
    }
    if linked {
        sync_directory(pool_root)?;
    }
    Ok(())
}

/// Once a release event is durable, absence from the canonical pool namespace
/// is ambiguous: the graph reconciler may have staged the inode for unlink and
/// released the pool lock while verifying it. Re-linking in that interval
/// would create a second canonical name that its finalizer will not remove.
/// Preserve the transaction's staged bytes and fail closed until the event is
/// completed; a consumed event needs no replay-pool evidence from retention.
pub(super) fn verify_committed_graph_replay_pool_state(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    pool_lock: &GraphReplayPoolLockV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    for generation in &transaction.receipt.deleted_generations {
        if !graph_replay_release::release_event_exists(
            store_root,
            &transaction.receipt,
            generation,
        )? {
            continue;
        }
        let staged = stage_root.join(&generation.generation_file);
        if !regular_file_exists(&staged)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "graph replay release for '{}' is outstanding but its staged sealed bytes are missing",
                generation.generation_file
            )));
        }
        let pool_entry = pool_lock.root.join(&generation.generation_file);
        match std::fs::symlink_metadata(&pool_entry) {
            Ok(_) => verify_existing_graph_replay_pool_entry(&pool_entry, &staged, generation)?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "graph replay release for '{}' is outstanding while its canonical pool entry is unavailable",
                    generation.generation_file
                )));
            }
            Err(error) => return Err(storage(error)),
        }
    }
    Ok(())
}

/// Prove that a same-name pool collision is the retired generation's exact
/// sealed bytes. The destination must be a regular file (never a symlink or
/// directory), match the expected size, and be byte-identical to the staged
/// source; its identity must also be stable across the content check, so a
/// mid-verify swap fails closed instead of certifying stale evidence.
pub(super) fn verify_existing_graph_replay_pool_entry(
    pool_entry: &Path,
    staged: &Path,
    generation: &CodeGenerationRetentionGenerationV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let unsafe_entry = |reason: &str| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "graph replay pool entry '{}' {reason}",
            generation.generation_file
        ))
    };
    let before = std::fs::symlink_metadata(pool_entry).map_err(storage)?;
    if !before.file_type().is_file() {
        return Err(unsafe_entry("is not a regular file"));
    }
    if before.len() != generation.size_bytes {
        return Err(unsafe_entry("does not match the retired generation's size"));
    }
    let entry_file = File::open(pool_entry).map_err(storage)?;
    if !path_still_names_open_file(pool_entry, &entry_file, &before)? {
        return Err(unsafe_entry(
            "changed while its identity was being verified",
        ));
    }
    let staged_file = File::open(staged).map_err(storage)?;
    let staged_before = staged_file.metadata().map_err(storage)?;
    if !open_files_match_generation_identity(&entry_file, &staged_file, generation)? {
        return Err(unsafe_entry(
            "does not match the staged sealed bytes and digest-named generation",
        ));
    }
    if !path_still_names_open_file(pool_entry, &entry_file, &before)? {
        return Err(unsafe_entry(
            "changed while its identity was being verified",
        ));
    }
    let staged_after = staged_file.metadata().map_err(storage)?;
    if !metadata_identity_matches(&staged_before, &staged_after) {
        return Err(unsafe_entry(
            "was compared against staged bytes that changed during verification",
        ));
    }
    Ok(())
}

pub(super) fn path_still_names_open_file(
    path: &Path,
    opened: &File,
    admitted: &std::fs::Metadata,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let current = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(storage(error)),
    };
    if !current.file_type().is_file() {
        return Ok(false);
    }
    let opened_metadata = opened.metadata().map_err(storage)?;
    if !metadata_identity_matches(admitted, &opened_metadata)
        || !metadata_identity_matches(&current, &opened_metadata)
    {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let named = match File::open(path) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(storage(error)),
        };
        let named_identity =
            tracedecay_private_fs::windows_file::information(&named).map_err(storage)?;
        let opened_identity =
            tracedecay_private_fs::windows_file::information(opened).map_err(storage)?;
        if named_identity.volume_serial_number != opened_identity.volume_serial_number
            || named_identity.file_index != opened_identity.file_index
            || named_identity.number_of_links != opened_identity.number_of_links
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether two metadata snapshots name the same stable file identity. On
/// Unix the device and inode pair is exact; the type, length, and
/// modification time double as the cross-check that the content did not
/// change between the snapshots. Windows file-index identity is compared
/// from retained handles via `windows_file::information`, not MetadataExt.
pub(super) fn metadata_identity_matches(
    left: &std::fs::Metadata,
    right: &std::fs::Metadata,
) -> bool {
    #[cfg(unix)]
    {
        if left.dev() != right.dev()
            || left.ino() != right.ino()
            || left.ctime() != right.ctime()
            || left.ctime_nsec() != right.ctime_nsec()
            || left.mode() != right.mode()
        {
            return false;
        }
    }
    left.file_type() == right.file_type()
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
}

/// Compare two open files byte for byte while independently proving that the
/// bytes still hash to the digest in the content-addressed generation name.
/// A same-inode hard link needs one hash; distinct copies are compared and
/// hashed together in a single bounded pass.
pub(super) fn open_files_match_generation_identity(
    left: &File,
    right: &File,
    generation: &CodeGenerationRetentionGenerationV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let left_metadata = left.metadata().map_err(storage)?;
    let right_metadata = right.metadata().map_err(storage)?;
    if left_metadata.len() != generation.size_bytes || right_metadata.len() != generation.size_bytes
    {
        return Ok(false);
    }
    let expected_digest = generation
        .generation_file
        .strip_prefix("generation-")
        .and_then(|value| value.strip_suffix(".json"))
        .filter(|value| is_lowercase_hex(value, 64))
        .ok_or_else(|| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "retired generation file '{}' does not name a SHA-256 content digest",
                generation.generation_file
            ))
        })?;
    #[cfg(unix)]
    {
        if left_metadata.dev() == right_metadata.dev()
            && left_metadata.ino() == right_metadata.ino()
        {
            return Ok(open_file_sha256_hex(left)? == expected_digest);
        }
    }
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left_reader = left;
    let mut right_reader = right;
    let mut left_buffer = vec![0_u8; 64 * 1024];
    let mut right_buffer = vec![0_u8; 64 * 1024];
    let mut left_hasher = Sha256::new();
    let mut right_hasher = Sha256::new();
    loop {
        let left_read = read_full(&mut left_reader, &mut left_buffer)?;
        let right_read = read_full(&mut right_reader, &mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(
                encode_lowercase_hex(&left_hasher.finalize()) == expected_digest
                    && encode_lowercase_hex(&right_hasher.finalize()) == expected_digest,
            );
        }
        left_hasher.update(&left_buffer[..left_read]);
        right_hasher.update(&right_buffer[..right_read]);
    }
}

pub(super) fn open_file_sha256_hex(file: &File) -> Result<String, CodeGenerationRetentionErrorV1> {
    open_file_sha256_hex_cancellable(file, &|| false)
}

pub(super) fn open_file_sha256_hex_cancellable(
    file: &File,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    let mut reader = file;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let read = read_full(&mut reader, &mut buffer)?;
        if read == 0 {
            return Ok(encode_lowercase_hex(&hasher.finalize()));
        }
        let hashed = read as u64;
        crate::hotpath_observe::retention_inspected(hashed);
        crate::hotpath_observe::retention_hashed(hashed);
        hasher.update(&buffer[..read]);
    }
}

pub(super) fn read_full(
    reader: &mut impl Read,
    buffer: &mut [u8],
) -> Result<usize, CodeGenerationRetentionErrorV1> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(storage(error)),
        }
    }
    Ok(filled)
}

/// Withdraw a rolled-back transaction's pool exposure. The canonical files
/// are restored by the rollback rename before this runs, so the graph replay
/// path resolves them from the generation directory again. Only entries that
/// are provably that generation's sealed bytes are removed — normally the
/// very inode the rollback just renamed back, or a same-digest copy left by
/// the eager staging path. A foreign same-name entry (non-regular or with
/// different bytes) was never linked by this transaction and is left in
/// place so the rollback cannot destroy evidence it does not own.
pub(super) fn withdraw_generations_from_graph_replay_pool(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    pool_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(pool_root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
    }
    let _pool_lock = acquire_graph_replay_pool_lock(pool_root)?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let mut removed = false;
    for generation in &transaction.receipt.deleted_generations {
        let pool_entry = pool_root.join(&generation.generation_file);
        match std::fs::symlink_metadata(&pool_entry) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage(error)),
        }
        let canonical = generations_root.join(&generation.generation_file);
        if !regular_file_exists(&canonical)? {
            continue;
        }
        let pool_file = File::open(&pool_entry).map_err(storage)?;
        let canonical_file = File::open(&canonical).map_err(storage)?;
        if !open_files_match_generation_identity(&pool_file, &canonical_file, generation)? {
            continue;
        }
        match std::fs::remove_file(&pool_entry) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }
    if removed {
        sync_directory(pool_root)?;
    }
    Ok(())
}

pub(super) fn rollback_staged_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    graph_replay_pool_root: Option<&Path>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        let staged = stage_root.join(&generation.generation_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {}
            (false, true) => {
                std::fs::rename(&staged, &source).map_err(storage)?;
                sync_directory(&generations_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "retention rollback cannot find '{}'",
                    generation.generation_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "retention rollback found duplicate '{}'",
                    generation.generation_file
                )));
            }
        }
    }
    if let Some(pool_root) = graph_replay_pool_root {
        withdraw_generations_from_graph_replay_pool(store_root, transaction, pool_root)?;
    }
    graph_replay_release::remove_events(store_root, &transaction.receipt)?;
    // Files first, pointer second. Restoring the index while the generations
    // were still quarantined would publish a pointer naming missing files,
    // which is the exact unsafe state this transaction exists to avoid; the
    // reverse order is safe at every intermediate crash point because a
    // rewritten pointer names a strict subset of what is on disk.
    restore_pointer_for_rollback(store_root, transaction)?;
    remove_empty_stage_root(&stage_root)
}

/// Put the pre-collection `generation_index` back after a rolled-back unit.
///
/// Only a store still carrying this transaction's own rewrite is restored: a
/// publish that landed while the unit was interrupted owns the pointer, and
/// its index already excludes whatever it chose to exclude.
fn restore_pointer_for_rollback(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let (Some(rewritten), Some(active)) = (
        transaction.rewritten_pointer()?,
        transaction.active_pointer.as_ref(),
    ) else {
        return Ok(());
    };
    if read_optional_active_pointer(store_root)? != Some(rewritten) {
        return Ok(());
    }
    write_active_pointer(store_root, RETENTION_POINTER_ROLLBACK_CONTEXT, active)
}

/// Finish the durable index rewrite a committed unit may have crashed before.
///
/// The rewrite precedes the unlink, so a durable receipt normally implies a
/// rewritten pointer. Re-applying it when the store still carries the
/// pre-collection pointer keeps recovery order-independent instead of leaving
/// an index that names generations the receipt already released.
fn complete_pointer_rewrite(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let (Some(rewritten), Some(active)) = (
        transaction.rewritten_pointer()?,
        transaction.active_pointer.as_ref(),
    ) else {
        return Ok(());
    };
    if read_optional_active_pointer(store_root)?.as_ref() != Some(active) {
        return Ok(());
    }
    write_active_pointer(store_root, RETENTION_POINTER_WRITE_CONTEXT, &rewritten)
}

pub(super) fn cleanup_committed_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let graph_replay_pool_lock = match graph_replay_pool_root {
        Some(pool_root) => Some(acquire_graph_replay_pool_lock_checked(
            pool_root,
            Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
            is_cancelled,
        )?),
        None => None,
    };
    cleanup_committed_transaction_under_graph_replay_pool_lock(
        store_root,
        transaction,
        vector_readable_sources,
        graph_replay_pool_lock.as_ref(),
    )
}

pub(super) fn cleanup_committed_transaction_under_graph_replay_pool_lock(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_lock: Option<&GraphReplayPoolLockV1>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    ensure_transaction_liveness(store_root, transaction, vector_readable_sources)?;
    complete_pointer_rewrite(store_root, transaction)?;
    if let Some(pool_lock) = graph_replay_pool_lock {
        verify_committed_graph_replay_pool_state(store_root, transaction, pool_lock)?;
    }
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        if regular_file_exists(&source)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "retention receipt is durable but '{}' returned to the generation directory",
                generation.generation_file
            )));
        }
        let staged = stage_root.join(&generation.generation_file);
        if regular_file_exists(&staged)? {
            std::fs::remove_file(&staged).map_err(storage)?;
            sync_directory(&stage_root)?;
        }
    }
    remove_empty_stage_root(&stage_root)
}

pub(super) fn ensure_transaction_liveness(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    // Liveness is proven against the *current* pointer, not the journaled
    // snapshot: a publish may have landed since the transaction was staged
    // (including the first publish into a previously unpublished store), and
    // the current active generation must never be removed by recovery.
    let current = read_optional_active_pointer(store_root)?;
    let deleted_ids = transaction
        .receipt
        .deleted_generations
        .iter()
        .map(|generation| generation.generation_id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(current) = current.as_ref() {
        let current_generation =
            CodeGenerationId::new(current.generation_id.clone()).map_err(|error| {
                CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "current active generation id is invalid during retention recovery: {error}"
                ))
            })?;
        if deleted_ids.contains(&current_generation)
            || transaction
                .receipt
                .deleted_generations
                .iter()
                .any(|generation| generation.generation_file == current.generation_file)
        {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "retention recovery would remove an active or vector-readable generation"
                    .to_owned(),
            ));
        }
    }
    if !deleted_ids.is_disjoint(vector_readable_sources) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention recovery would remove an active or vector-readable generation".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn clear_transaction(store_root: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    clear_journal(store_root, &GENERATION_TRANSACTION_JOURNAL)
}

pub(super) fn remove_empty_stage_root(
    stage_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let mut entries = match std::fs::read_dir(stage_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
    };
    if entries.next().is_some() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention quarantine '{}' contains unexpected files",
            stage_root.display()
        )));
    }
    std::fs::remove_dir(stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState("retention quarantine has no parent".to_owned())
    })?)
}

pub(super) fn regular_file_exists(path: &Path) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention path '{}' is not a regular file",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(storage(error)),
    }
}
