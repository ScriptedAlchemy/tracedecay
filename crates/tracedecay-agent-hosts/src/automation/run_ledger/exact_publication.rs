use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::ManifestDigest;

use super::{
    AutomationRunLedgerRecord, run_ledger_path, sync_run_ledger_file_and_parent,
    validate_run_id_component,
};
use crate::automation::config_error;
use crate::errors::{Result, TraceDecayError};

const EXACT_RUN_SPOOL_DIR: &str = "automation_run_spool";
const EXACT_RUN_SPOOL_LOCK: &str = "automation_run_spool.lock";
const EXACT_RUN_APPEND_INTENT: &str = "automation_runs.jsonl.append-intent";
const EXACT_RUN_APPEND_INTENT_QUARANTINE_DIR: &str = "automation_run_append_intent_quarantine";
const MAX_APPEND_INTENT_BYTES: u64 = 16 * 1024;
const MAX_APPEND_INTENT_QUARANTINES: usize = 16;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

type AtomicFilePublisher<'a> = dyn Fn(&Path, &Path, &str) -> std::io::Result<()> + 'a;

fn replace_file_atomically(
    temporary: &Path,
    destination: &Path,
    label: &str,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let temporary_file =
            tracedecay_runtime_core::windows_security::make_private_file(temporary)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
    }
    crate::db::DatabaseAuthority::replace_file_atomically(temporary, destination, label)
        .map_err(std::io::Error::other)?;
    #[cfg(windows)]
    tracedecay_runtime_core::windows_security::validate_private_file(destination)?;
    Ok(())
}

pub(super) fn acquire_run_ledger_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let lock_path = crate::storage::append_lock_path(path);
    acquire_nofollow_lock(&lock_path)
}

fn acquire_nofollow_lock(lock_path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = lock_path.parent() {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)?;
    }
    crate::storage::reject_symlink_components(&lock_path, "automation run ledger lock")?;
    let parent = lock_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation run ledger lock has no parent",
        )
    })?;
    let name = lock_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation run ledger lock has no filename",
        )
    })?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())?;
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = directory.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "automation run ledger lock is not a regular file",
        ));
    }
    let file = file.into_std();
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

pub(super) fn open_run_ledger_nofollow(
    path: &Path,
    read: bool,
    write: bool,
    append: bool,
    create: bool,
) -> std::io::Result<Option<std::fs::File>> {
    if let Some(parent) = path.parent()
        && create
    {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)?;
    }
    crate::storage::reject_symlink_components(path, "automation run ledger")?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation run ledger has no parent",
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation run ledger has no filename",
        )
    })?;
    let directory = match Dir::open_ambient_dir(parent, ambient_authority()) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut options = CapOpenOptions::new();
    options
        .read(read)
        .write(write)
        .append(append)
        .create(create)
        .follow(FollowSymlinks::No);
    match directory.open_with(name, &options) {
        Ok(file) => {
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "automation run ledger is not a regular file",
                ));
            }
            Ok(Some(file.into_std()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactRunPublication {
    schema_version: u32,
    ledger_digest: ManifestDigest,
    payload_len: u64,
}

impl ExactRunPublication {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(config_error(
                "exact automation run publication has an unsupported schema",
            ));
        }
        self.ledger_digest
            .validate()
            .map_err(|error| config_error(format!("invalid exact run digest: {error}")))?;
        if self.payload_len == 0 {
            return Err(config_error(
                "exact automation run publication has an empty payload",
            ));
        }
        exact_run_framed_payload_len(self.payload_len)?;
        Ok(())
    }
}

fn exact_run_framed_payload_len(payload_len: u64) -> Result<u64> {
    payload_len
        .checked_add(1)
        .ok_or_else(|| config_error("exact automation run framed payload length overflowed"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LedgerAppendIntent {
    schema_version: u32,
    run_id: String,
    pre_append_eof: u64,
    publication: ExactRunPublication,
}

enum LedgerAppendIntentState {
    Missing,
    Valid(LedgerAppendIntent),
    Corrupt(Vec<u8>),
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct CorruptAppendIntentQuarantineIo<'a> {
    publish: &'a dyn Fn(&Path, &[u8]) -> Result<()>,
    republish_existing: &'a dyn Fn(&Path, &[u8]) -> Result<()>,
    remove_debris: &'a dyn Fn(&Path) -> Result<()>,
}

impl LedgerAppendIntent {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(config_error(
                "automation run append intent has an unsupported schema",
            ));
        }
        validate_run_id_component(&self.run_id)?;
        self.publication.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactRunPublishOutcome {
    Published,
    MissingPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactRunUnboundDiscardOutcome {
    Discarded,
    Retained,
}

/// Stages the canonical JSON row by borrowing the runner-owned record. The
/// record is streamed twice (digest, then durable file) but is never cloned or
/// collected into a payload-sized buffer.
pub fn bind_staged_run_record_exact<T>(
    dashboard_root: &Path,
    record: &AutomationRunLedgerRecord,
    bind: impl FnOnce(&ExactRunPublication) -> Result<T>,
) -> Result<(ExactRunPublication, T)> {
    bind_staged_run_record_exact_with_publisher(
        dashboard_root,
        record,
        &replace_file_atomically,
        bind,
    )
}

fn bind_staged_run_record_exact_with_publisher<T>(
    dashboard_root: &Path,
    record: &AutomationRunLedgerRecord,
    publish_file: &AtomicFilePublisher<'_>,
    bind: impl FnOnce(&ExactRunPublication) -> Result<T>,
) -> Result<(ExactRunPublication, T)> {
    validate_run_id_component(&record.run_id)?;
    super::validate_run_ledger_record_semantics(record)?;
    if !record.status.is_terminal() {
        return Err(config_error(
            "exact automation run staging requires a terminal record",
        ));
    }
    let (ledger_digest, payload_len) = run_record_exact_identity(record)?;
    let publication = ExactRunPublication {
        schema_version: 1,
        ledger_digest,
        payload_len,
    };
    let path = spool_path(dashboard_root, &record.run_id, &publication)?;
    with_spool_lock(dashboard_root, || {
        let ledger_path = run_ledger_path(dashboard_root);
        let ledger_lock = acquire_run_ledger_lock(&ledger_path).map_err(TraceDecayError::from)?;
        let result = (|| {
            ensure_no_exact_append_intent(dashboard_root).map_err(TraceDecayError::from)?;
            let durable_identity =
                match super::exact_lookup::open_stabilized_run_ledger(&ledger_path, false)? {
                    Some(ledger) => super::exact_lookup::read_exact_run_identity_from_file(
                        &ledger,
                        &ledger_path,
                        &record.run_id,
                    )?,
                    None => None,
                };
            let already_published = match durable_identity {
                Some(identity)
                    if identity.digest == publication.ledger_digest
                        && identity.payload_len == publication.payload_len =>
                {
                    true
                }
                Some(_) => {
                    return Err(config_error(format!(
                        "automation run '{}' has conflicting durable history",
                        record.run_id
                    )));
                }
                None => false,
            };
            validate_same_run_spool_history(dashboard_root, &record.run_id, &publication)?;
            match digest_regular_file(&path)? {
                Some((existing, len))
                    if existing == publication.ledger_digest && len == publication.payload_len =>
                {
                    publish_exact_run_spool(&path, record, &publication, publish_file)?
                }
                Some(_) => {
                    return Err(config_error(format!(
                        "automation run '{}' has conflicting staged history",
                        record.run_id
                    )));
                }
                None if !already_published => {
                    publish_exact_run_spool(&path, record, &publication, publish_file)?
                }
                None => {}
            }
            match bind(&publication) {
                Ok(bound) => Ok((publication.clone(), bound)),
                // Binding may have made Prepared durable before a later
                // directory sync/readback error surfaced. Preserve the
                // digest-owned spool on every uncertain bind failure;
                // Reserved recovery retires unbound spools by exact run
                // identity.
                Err(error) => Err(error),
            }
        })();
        let unlock = fs2::FileExt::unlock(&ledger_lock).map_err(TraceDecayError::from);
        result.and_then(|bound| unlock.map(|()| bound))
    })
}

fn publish_exact_run_spool(
    path: &Path,
    record: &AutomationRunLedgerRecord,
    publication: &ExactRunPublication,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    tracedecay_domain::with_owned_temp_publish(
        path,
        "automation-run-exact-spool",
        |temporary, destination| publish_file(temporary, destination, "automation run exact spool"),
        |output| serde_json::to_writer(output, record).map_err(std::io::Error::other),
        tracedecay_application::DirectorySyncPolicy::Strict,
    )
    .map_err(TraceDecayError::from)?;
    let (actual, actual_len) = digest_regular_file(path)?.ok_or_else(|| {
        config_error("exact automation run spool disappeared after durable publication")
    })?;
    if actual != publication.ledger_digest || actual_len != publication.payload_len {
        return Err(config_error(
            "exact automation run spool changed after durable publication",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn stage_run_record_exact(
    dashboard_root: &Path,
    record: &AutomationRunLedgerRecord,
) -> Result<ExactRunPublication> {
    bind_staged_run_record_exact(dashboard_root, record, |_| Ok(()))
        .map(|(publication, ())| publication)
}

fn run_record_exact_identity(record: &AutomationRunLedgerRecord) -> Result<(ManifestDigest, u64)> {
    let mut writer = DigestWriter::default();
    serde_json::to_writer(&mut writer, record).map_err(TraceDecayError::from)?;
    writer.finish_with_len()
}

/// Verifies or publishes the exact staged row under the canonical ledger lock.
/// A previously published digest succeeds even after its spool was retired.
pub async fn publish_staged_run_record_exact(
    dashboard_root: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
) -> Result<ExactRunPublishOutcome> {
    validate_run_id_component(run_id)?;
    publication.validate()?;
    let root = dashboard_root.to_path_buf();
    let run_id = run_id.to_owned();
    let publication = publication.clone();
    tokio::task::spawn_blocking(move || {
        publish_staged_run_record_exact_blocking(&root, &run_id, &publication)
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join exact automation run publication: {error}"
        ))
    })?
}

/// Removes only the digest-owned spool after its exact terminal binding has
/// been published or replay-verified.
pub async fn discard_staged_run_record_exact(
    dashboard_root: &Path,
    run_id: &str,
    expected: &ExactRunPublication,
) -> Result<()> {
    validate_run_id_component(run_id)?;
    expected.validate()?;
    let root = dashboard_root.to_path_buf();
    let run_id = run_id.to_owned();
    let expected = expected.clone();
    tokio::task::spawn_blocking(move || {
        discard_staged_run_record_exact_blocking(&root, &run_id, &expected)
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join exact automation run spool cleanup: {error}"
        ))
    })?
}

/// Removes run-id spools only while an owning authority revalidates that the
/// journal is still an abandoned, unbound reservation.
///
/// The validator runs under the spool lock. It may acquire the journal lock,
/// establishing the same `spool -> journal` order as exact binding. Returning
/// `false` retains every candidate and lets the caller resnapshot newer state.
pub async fn discard_unbound_staged_run_records_if<F>(
    dashboard_root: &Path,
    run_id: &str,
    validate_unbound: F,
) -> Result<ExactRunUnboundDiscardOutcome>
where
    F: FnOnce() -> Result<bool> + Send + 'static,
{
    validate_run_id_component(run_id)?;
    let root = dashboard_root.to_path_buf();
    let run_id = run_id.to_owned();
    tokio::task::spawn_blocking(move || {
        with_spool_lock(&root, || {
            if !validate_unbound()? {
                return Ok(ExactRunUnboundDiscardOutcome::Retained);
            }
            discard_unbound_spools_for_run(&root, &run_id)?;
            Ok(ExactRunUnboundDiscardOutcome::Discarded)
        })
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join conditional automation run spool cleanup: {error}"
        ))
    })?
}

pub fn publish_staged_run_record_exact_blocking(
    dashboard_root: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
) -> Result<ExactRunPublishOutcome> {
    publish_staged_run_record_exact_blocking_with_publisher(
        dashboard_root,
        run_id,
        publication,
        &replace_file_atomically,
    )
}

fn publish_staged_run_record_exact_blocking_with_publisher(
    dashboard_root: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<ExactRunPublishOutcome> {
    validate_run_id_component(run_id)?;
    publication.validate()?;
    let ledger = run_ledger_path(dashboard_root);
    if let Some(parent) = ledger.parent() {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)
            .map_err(TraceDecayError::from)?;
    }
    let spool = spool_path(dashboard_root, run_id, publication)?;
    with_spool_lock(dashboard_root, || {
        let lock = acquire_run_ledger_lock(&ledger).map_err(TraceDecayError::from)?;
        let result = publish_under_ledger_lock(
            dashboard_root,
            &ledger,
            &spool,
            run_id,
            publication,
            publish_file,
        );
        let unlock = fs2::FileExt::unlock(&lock).map_err(TraceDecayError::from);
        result.and_then(|outcome| unlock.map(|()| outcome))
    })
}

/// Repairs only a corrupt exact-append intent whose ledger state has one
/// independently provable outcome. Valid intents remain owned by their exact
/// publication and are not resolved without that journal binding.
pub fn repair_corrupt_run_ledger_append_intent_blocking(dashboard_root: &Path) -> Result<()> {
    repair_corrupt_run_ledger_append_intent_impl(
        dashboard_root,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
fn repair_corrupt_run_ledger_append_intent_with(
    dashboard_root: &Path,
    publish: impl Fn(&Path, &[u8]) -> Result<()>,
    republish_existing: impl Fn(&Path, &[u8]) -> Result<()>,
    remove_debris: impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    repair_corrupt_run_ledger_append_intent_impl(
        dashboard_root,
        Some(CorruptAppendIntentQuarantineIo {
            publish: &publish,
            republish_existing: &republish_existing,
            remove_debris: &remove_debris,
        }),
    )
}

fn repair_corrupt_run_ledger_append_intent_impl(
    dashboard_root: &Path,
    #[cfg(test)] quarantine_io: Option<CorruptAppendIntentQuarantineIo<'_>>,
) -> Result<()> {
    with_spool_lock(dashboard_root, || {
        let ledger_path = run_ledger_path(dashboard_root);
        let ledger_lock = acquire_run_ledger_lock(&ledger_path).map_err(TraceDecayError::from)?;
        let result = (|| {
            let state = read_append_intent_state(dashboard_root)?;
            let LedgerAppendIntentState::Corrupt(bytes) = state else {
                return Ok(());
            };
            quarantine_corrupt_append_intent_impl(
                dashboard_root,
                &bytes,
                #[cfg(test)]
                quarantine_io,
            )?;
            let mut ledger = super::exact_lookup::open_stabilized_run_ledger(&ledger_path, true)?
                .ok_or_else(|| {
                config_error("automation run ledger disappeared during corrupt-intent repair")
            })?;
            repair_corrupt_append_intent(dashboard_root, &ledger_path, &mut ledger, &bytes)
        })();
        let unlock = fs2::FileExt::unlock(&ledger_lock).map_err(TraceDecayError::from);
        result.and_then(|()| unlock)
    })
}

/// Blocking form of [`discard_staged_run_record_exact`] for owners that must
/// retain a claim across process-blocking durability work.
pub fn discard_staged_run_record_exact_blocking(
    dashboard_root: &Path,
    run_id: &str,
    expected: &ExactRunPublication,
) -> Result<()> {
    validate_run_id_component(run_id)?;
    expected.validate()?;
    let path = spool_path(dashboard_root, run_id, expected)?;
    with_spool_lock(dashboard_root, || {
        let Some((actual, actual_len)) = digest_regular_file(&path)? else {
            return Ok(());
        };
        if expected.ledger_digest != actual || expected.payload_len != actual_len {
            return Err(config_error(
                "automation run spool cleanup conflicts with its terminal binding",
            ));
        }
        remove_canonical_spool_durable(&path)
    })
}

/// Recovery-only cleanup for a stale or corrupt spool whose bytes are no
/// longer authoritative. The exact durable ledger row and the owning journal
/// terminal must both be reverified atomically before namespace retirement.
pub async fn discard_stale_staged_run_record_exact_after_terminal<F>(
    dashboard_root: &Path,
    run_id: &str,
    expected: &ExactRunPublication,
    validate_terminal: F,
) -> Result<()>
where
    F: FnOnce() -> Result<bool> + Send + 'static,
{
    validate_run_id_component(run_id)?;
    expected.validate()?;
    let root = dashboard_root.to_path_buf();
    let run_id = run_id.to_owned();
    let expected = expected.clone();
    tokio::task::spawn_blocking(move || {
        discard_stale_staged_run_record_exact_after_terminal_blocking(
            &root,
            &run_id,
            &expected,
            validate_terminal,
        )
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join stale automation run spool recovery: {error}"
        ))
    })?
}

fn discard_stale_staged_run_record_exact_after_terminal_blocking<F>(
    dashboard_root: &Path,
    run_id: &str,
    expected: &ExactRunPublication,
    validate_terminal: F,
) -> Result<()>
where
    F: FnOnce() -> Result<bool>,
{
    let path = spool_path(dashboard_root, run_id, expected)?;
    with_spool_lock(dashboard_root, || {
        let ledger_path = run_ledger_path(dashboard_root);
        let ledger_lock = acquire_run_ledger_lock(&ledger_path).map_err(TraceDecayError::from)?;
        let result = (|| {
            ensure_no_exact_append_intent(dashboard_root).map_err(TraceDecayError::from)?;
            let ledger = super::exact_lookup::open_stabilized_run_ledger(&ledger_path, false)?
                .ok_or_else(|| {
                    config_error(
                        "automation run spool cleanup has no durable exact ledger authority",
                    )
                })?;
            let exact = super::exact_lookup::read_exact_run_identity_from_file(
                &ledger,
                &ledger_path,
                run_id,
            )?
            .ok_or_else(|| {
                config_error("automation run spool cleanup has no exact published row")
            })?;
            if exact.digest != expected.ledger_digest || exact.payload_len != expected.payload_len {
                return Err(config_error(
                    "automation run spool cleanup conflicts with exact durable history",
                ));
            }
            if !validate_terminal()? {
                return Err(config_error(
                    "automation run spool cleanup lacks matching terminal authority",
                ));
            }
            remove_canonical_spool_durable(&path)
        })();
        let unlock = fs2::FileExt::unlock(&ledger_lock).map_err(TraceDecayError::from);
        result.and_then(|()| unlock)
    })
}

fn remove_canonical_spool_durable(path: &Path) -> Result<()> {
    if let Some(file) = open_regular_nofollow(path)? {
        drop(file);
    }
    crate::storage::PrivateStoreIo::remove_file_durable(path)
        .map(|_| ())
        .map_err(TraceDecayError::from)
}

fn publish_under_ledger_lock(
    dashboard_root: &Path,
    ledger_path: &Path,
    spool_path: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<ExactRunPublishOutcome> {
    let mut ledger = super::exact_lookup::open_stabilized_run_ledger(ledger_path, true)?
        .ok_or_else(|| config_error("automation run ledger disappeared during durable open"))?;
    recover_matching_append_intent(
        dashboard_root,
        ledger_path,
        &mut ledger,
        spool_path,
        run_id,
        publication,
        publish_file,
    )?;
    super::ensure_run_ledger_eof_guard(&mut ledger).map_err(TraceDecayError::from)?;
    match super::exact_lookup::read_exact_run_identity_from_file(&ledger, ledger_path, run_id)? {
        Some(existing)
            if existing.digest == publication.ledger_digest
                && existing.payload_len == publication.payload_len =>
        {
            return Ok(ExactRunPublishOutcome::Published);
        }
        Some(_) => {
            return Err(config_error(format!(
                "automation run '{run_id}' has inconsistent durable history"
            )));
        }
        None => {}
    }

    let Some(mut spool) = open_bound_spool(spool_path, run_id, publication)? else {
        return Ok(ExactRunPublishOutcome::MissingPayload);
    };
    let pre_append_eof = ledger.metadata().map_err(TraceDecayError::from)?.len();
    let intent = LedgerAppendIntent {
        schema_version: 1,
        run_id: run_id.to_owned(),
        pre_append_eof,
        publication: publication.clone(),
    };
    write_append_intent_with_publisher(dashboard_root, &intent, publish_file)?;

    ledger
        .seek(SeekFrom::Start(pre_append_eof))
        .map_err(TraceDecayError::from)?;
    copy_exact_payload(&mut spool, &mut ledger, publication.payload_len)?;
    ledger.write_all(b"\n").map_err(TraceDecayError::from)?;
    sync_run_ledger_file_and_parent(ledger_path, &ledger)?;
    verify_published_range(&mut ledger, &mut spool, &intent)?;
    clear_append_intent(dashboard_root)?;
    Ok(ExactRunPublishOutcome::Published)
}

fn recover_matching_append_intent(
    dashboard_root: &Path,
    ledger_path: &Path,
    ledger: &mut std::fs::File,
    spool_path: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    let intent = match read_append_intent_state(dashboard_root)? {
        LedgerAppendIntentState::Missing => return Ok(()),
        LedgerAppendIntentState::Valid(intent) => intent,
        LedgerAppendIntentState::Corrupt(bytes) => {
            quarantine_corrupt_append_intent(dashboard_root, &bytes)?;
            repair_corrupt_append_intent(dashboard_root, ledger_path, ledger, &bytes)?;
            return Ok(());
        }
    };
    if intent.run_id != run_id || intent.publication != *publication {
        return Err(config_error(format!(
            "automation run ledger has an unresolved append intent for '{}'",
            intent.run_id
        )));
    }
    publish_append_intent(dashboard_root, &intent, publish_file)?;
    let Some(mut spool) = open_bound_spool(spool_path, run_id, publication)? else {
        return Err(config_error(
            "automation run append intent lost its exact staged payload",
        ));
    };
    let ledger_len = ledger.metadata().map_err(TraceDecayError::from)?.len();
    if ledger_len < intent.pre_append_eof {
        return Err(config_error(
            "automation run ledger is shorter than its durable append intent",
        ));
    }
    let appended_len = ledger_len - intent.pre_append_eof;
    let complete_len = exact_run_framed_payload_len(publication.payload_len)?;
    if appended_len > complete_len {
        return Err(config_error(
            "automation run append intent has foreign bytes after its owned range",
        ));
    }
    compare_owned_prefix(ledger, &mut spool, &intent, appended_len)?;
    if appended_len == complete_len {
        verify_published_range(ledger, &mut spool, &intent)?;
        sync_run_ledger_file_and_parent(ledger_path, ledger)?;
        return clear_append_intent(dashboard_root);
    }
    ledger
        .set_len(intent.pre_append_eof)
        .map_err(TraceDecayError::from)?;
    sync_run_ledger_file_and_parent(ledger_path, ledger)?;
    clear_append_intent(dashboard_root)
}

fn compare_owned_prefix(
    ledger: &mut std::fs::File,
    spool: &mut std::fs::File,
    intent: &LedgerAppendIntent,
    appended_len: u64,
) -> Result<()> {
    ledger
        .seek(SeekFrom::Start(intent.pre_append_eof))
        .map_err(TraceDecayError::from)?;
    spool
        .seek(SeekFrom::Start(0))
        .map_err(TraceDecayError::from)?;
    let payload_prefix = appended_len.min(intent.publication.payload_len);
    compare_exact_bytes(ledger, spool, payload_prefix)?;
    if appended_len > intent.publication.payload_len {
        let mut delimiter = [0_u8; 1];
        ledger
            .read_exact(&mut delimiter)
            .map_err(TraceDecayError::from)?;
        if delimiter[0] != b'\n' {
            return Err(config_error(
                "automation run append intent has a foreign row delimiter",
            ));
        }
    }
    Ok(())
}

fn verify_published_range(
    ledger: &mut std::fs::File,
    spool: &mut std::fs::File,
    intent: &LedgerAppendIntent,
) -> Result<()> {
    let complete_len = exact_run_framed_payload_len(intent.publication.payload_len)?;
    let expected_end = intent
        .pre_append_eof
        .checked_add(complete_len)
        .ok_or_else(|| config_error("automation run append range overflowed"))?;
    if ledger.metadata().map_err(TraceDecayError::from)?.len() != expected_end {
        return Err(config_error(
            "automation run ledger changed outside its exact append range",
        ));
    }
    compare_owned_prefix(ledger, spool, intent, complete_len)
}

fn compare_exact_bytes(
    left: &mut std::fs::File,
    right: &mut std::fs::File,
    mut remaining: u64,
) -> Result<()> {
    let mut left_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut right_buffer = [0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(COPY_BUFFER_BYTES as u64))
            .map_err(|_| config_error("automation run comparison chunk is not representable"))?;
        left.read_exact(&mut left_buffer[..take])
            .map_err(TraceDecayError::from)?;
        right
            .read_exact(&mut right_buffer[..take])
            .map_err(TraceDecayError::from)?;
        if left_buffer[..take] != right_buffer[..take] {
            return Err(config_error(
                "automation run append intent does not own the ledger tail",
            ));
        }
        remaining -= take as u64;
    }
    Ok(())
}

fn copy_exact_payload(
    source: &mut std::fs::File,
    destination: &mut std::fs::File,
    payload_len: u64,
) -> Result<()> {
    source
        .seek(SeekFrom::Start(0))
        .map_err(TraceDecayError::from)?;
    let copied =
        std::io::copy(&mut source.take(payload_len), destination).map_err(TraceDecayError::from)?;
    if copied != payload_len {
        return Err(config_error(
            "automation run spool ended before its bound payload length",
        ));
    }
    Ok(())
}

fn open_bound_spool(
    path: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
) -> Result<Option<std::fs::File>> {
    crate::storage::reject_symlink_components(path, "automation run exact spool")
        .map_err(TraceDecayError::from)?;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    if !metadata.file_type().is_file() || metadata.len() != publication.payload_len {
        return Err(config_error(
            "automation run spool conflicts with its durable publication length",
        ));
    }
    let mut file = open_regular_nofollow(path)?.ok_or_else(|| {
        config_error("automation run spool disappeared while opening its bound descriptor")
    })?;
    if file.metadata().map_err(TraceDecayError::from)?.len() != publication.payload_len {
        return Err(config_error(
            "automation run spool changed while opening its bound descriptor",
        ));
    }
    let (actual, actual_len) = digest_file_handle(&mut file)?;
    if actual != publication.ledger_digest || actual_len != publication.payload_len {
        return Err(config_error(
            "automation run spool conflicts with its durable journal binding",
        ));
    }
    let Some(projection) = super::exact_lookup::scan_jsonl_row(&file, path, 0..actual_len)? else {
        return Err(config_error(
            "automation run spool does not contain a canonical ledger row",
        ));
    };
    if projection.run_id != run_id || !projection.status.is_terminal() {
        return Err(config_error(
            "automation run spool conflicts with its bound terminal identity",
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(TraceDecayError::from)?;
    Ok(Some(file))
}

fn spool_path(
    dashboard_root: &Path,
    run_id: &str,
    publication: &ExactRunPublication,
) -> Result<PathBuf> {
    validate_run_id_component(run_id)?;
    publication.validate()?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.automation-run.spool-key.v1");
    hasher.update((run_id.len() as u64).to_be_bytes());
    hasher.update(run_id.as_bytes());
    hasher.update(publication.ledger_digest.as_str().as_bytes());
    hasher.update(publication.payload_len.to_be_bytes());
    Ok(dashboard_root
        .join(EXACT_RUN_SPOOL_DIR)
        .join(format!("{}.json", hex::encode(hasher.finalize()))))
}

pub(super) fn ensure_no_exact_append_intent(dashboard_root: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(append_intent_path(dashboard_root)) {
        Ok(metadata) if metadata.file_type().is_file() => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "automation run ledger has an unresolved exact append intent",
        )),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "automation run append intent is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn append_intent_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(EXACT_RUN_APPEND_INTENT)
}

#[cfg(test)]
fn write_append_intent(dashboard_root: &Path, intent: &LedgerAppendIntent) -> Result<()> {
    write_append_intent_with_publisher(dashboard_root, intent, &replace_file_atomically)
}

fn write_append_intent_with_publisher(
    dashboard_root: &Path,
    intent: &LedgerAppendIntent,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    intent.validate()?;
    if read_append_intent(dashboard_root)?.is_some() {
        return Err(config_error(
            "automation run ledger append intent was not recovered before publication",
        ));
    }
    let bytes = serde_json::to_vec(intent).map_err(TraceDecayError::from)?;
    if bytes.len() as u64 > MAX_APPEND_INTENT_BYTES {
        return Err(config_error(
            "automation run ledger append intent exceeded its durable bound",
        ));
    }
    publish_append_intent_bytes(dashboard_root, &bytes, publish_file)
}

fn publish_append_intent(
    dashboard_root: &Path,
    intent: &LedgerAppendIntent,
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    intent.validate()?;
    let bytes = serde_json::to_vec(intent).map_err(TraceDecayError::from)?;
    if bytes.len() as u64 > MAX_APPEND_INTENT_BYTES {
        return Err(config_error(
            "automation run ledger append intent exceeded its durable bound",
        ));
    }
    publish_append_intent_bytes(dashboard_root, &bytes, publish_file)
}

fn publish_append_intent_bytes(
    dashboard_root: &Path,
    bytes: &[u8],
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    let path = append_intent_path(dashboard_root);
    tracedecay_domain::with_owned_temp_publish(
        &path,
        "automation-run-append-intent",
        |temporary, destination| {
            publish_file(
                temporary,
                destination,
                "automation run ledger append intent",
            )
        },
        |output| output.write_all(bytes),
        tracedecay_application::DirectorySyncPolicy::Strict,
    )
    .map_err(TraceDecayError::from)?;
    let actual = read_append_intent_bytes(dashboard_root)?.ok_or_else(|| {
        config_error("automation run append intent disappeared after durable publication")
    })?;
    if actual != bytes {
        return Err(config_error(
            "automation run append intent changed after durable publication",
        ));
    }
    Ok(())
}

fn read_append_intent(dashboard_root: &Path) -> Result<Option<LedgerAppendIntent>> {
    match read_append_intent_state(dashboard_root)? {
        LedgerAppendIntentState::Missing => Ok(None),
        LedgerAppendIntentState::Valid(intent) => Ok(Some(intent)),
        LedgerAppendIntentState::Corrupt(_) => {
            Err(config_error("automation run append intent is corrupt"))
        }
    }
}

fn read_append_intent_state(dashboard_root: &Path) -> Result<LedgerAppendIntentState> {
    let Some(bytes) = read_append_intent_bytes(dashboard_root)? else {
        return Ok(LedgerAppendIntentState::Missing);
    };
    let Ok(intent) = serde_json::from_slice::<LedgerAppendIntent>(&bytes) else {
        return Ok(LedgerAppendIntentState::Corrupt(bytes));
    };
    if intent.validate().is_err() {
        return Ok(LedgerAppendIntentState::Corrupt(bytes));
    }
    Ok(LedgerAppendIntentState::Valid(intent))
}

fn read_append_intent_bytes(dashboard_root: &Path) -> Result<Option<Vec<u8>>> {
    let path = append_intent_path(dashboard_root);
    crate::storage::reject_symlink_components(&path, "automation run append intent")
        .map_err(TraceDecayError::from)?;
    let Some(file) = open_regular_nofollow(&path)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(TraceDecayError::from)?;
    if metadata.len() > MAX_APPEND_INTENT_BYTES {
        return Err(config_error(
            "automation run append intent is not a bounded regular file",
        ));
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| {
            config_error("automation run append intent length is not representable")
        })?);
    file.take(MAX_APPEND_INTENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(TraceDecayError::from)?;
    if bytes.len() as u64 > MAX_APPEND_INTENT_BYTES {
        return Err(config_error(
            "automation run append intent grew beyond its durable bound",
        ));
    }
    Ok(Some(bytes))
}

fn clear_append_intent(dashboard_root: &Path) -> Result<()> {
    crate::storage::PrivateStoreIo::remove_file_durable(&append_intent_path(dashboard_root))
        .map(|_| ())
        .map_err(TraceDecayError::from)
}

fn quarantine_corrupt_append_intent(dashboard_root: &Path, bytes: &[u8]) -> Result<()> {
    quarantine_corrupt_append_intent_impl(
        dashboard_root,
        bytes,
        #[cfg(test)]
        None,
    )
}

fn quarantine_corrupt_append_intent_impl(
    dashboard_root: &Path,
    bytes: &[u8],
    #[cfg(test)] quarantine_io: Option<CorruptAppendIntentQuarantineIo<'_>>,
) -> Result<()> {
    let digest = Sha256::digest(bytes);
    let directory = dashboard_root.join(EXACT_RUN_APPEND_INTENT_QUARANTINE_DIR);
    crate::storage::PrivateStoreIo::create_dir_all_durable(&directory)
        .map_err(TraceDecayError::from)?;
    cleanup_abandoned_corrupt_append_intent_quarantine_temps(
        &directory,
        #[cfg(test)]
        quarantine_io,
    )?;
    let path = directory.join(format!("{}.json", hex::encode(digest)));
    crate::storage::reject_symlink_components(&path, "automation run append-intent quarantine")
        .map_err(TraceDecayError::from)?;
    let mut count = 0_usize;
    for entry in std::fs::read_dir(&directory).map_err(TraceDecayError::from)? {
        let entry = entry.map_err(TraceDecayError::from)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            config_error("automation run append-intent quarantine has a non-UTF-8 entry")
        })?;
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file()
            || !is_exact_spool_destination_name(name)
        {
            return Err(config_error(
                "automation run append-intent quarantine has a foreign entry",
            ));
        }
        count = count.saturating_add(1);
    }
    if open_regular_nofollow(&path)?.is_some() {
        verify_exact_corrupt_append_intent_quarantine(&path, bytes)?;
        #[cfg(test)]
        if let Some(quarantine_io) = quarantine_io {
            (quarantine_io.republish_existing)(&path, bytes)?;
        } else {
            publish_corrupt_append_intent_quarantine(&path, bytes)?;
        }
        #[cfg(not(test))]
        publish_corrupt_append_intent_quarantine(&path, bytes)?;
        verify_exact_corrupt_append_intent_quarantine(&path, bytes)?;
        return Ok(());
    }
    if count >= MAX_APPEND_INTENT_QUARANTINES {
        return Err(config_error(
            "automation run append-intent quarantine reached its bounded capacity",
        ));
    }
    #[cfg(test)]
    if let Some(quarantine_io) = quarantine_io {
        return (quarantine_io.publish)(&path, bytes);
    }
    publish_corrupt_append_intent_quarantine(&path, bytes)
}

fn cleanup_abandoned_corrupt_append_intent_quarantine_temps(
    directory: &Path,
    #[cfg(test)] quarantine_io: Option<CorruptAppendIntentQuarantineIo<'_>>,
) -> Result<()> {
    for entry in std::fs::read_dir(directory).map_err(TraceDecayError::from)? {
        let entry = entry.map_err(TraceDecayError::from)?;
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let owned_atomic_temp = is_corrupt_append_intent_quarantine_owned_temp_name(name);
        // This private directory is exclusively owned by corrupt append-intent
        // quarantine while the cooperative spool lock is held. An exact
        // runtime-core removal tombstone can therefore only derive from this
        // cleanup, including when the retired atomic temp contained zero or
        // partial bytes at the crash boundary.
        let recovered_windows_tombstone = is_private_store_durable_removal_tombstone_name(name)
            && quarantine_tombstone_is_bounded_regular(&path)?;
        if !owned_atomic_temp && !recovered_windows_tombstone {
            continue;
        }
        #[cfg(test)]
        if let Some(quarantine_io) = quarantine_io {
            (quarantine_io.remove_debris)(&path)?;
        } else {
            remove_corrupt_append_intent_quarantine_debris(&path)?;
        }
        #[cfg(not(test))]
        remove_corrupt_append_intent_quarantine_debris(&path)?;
    }
    Ok(())
}

fn remove_corrupt_append_intent_quarantine_debris(path: &Path) -> Result<()> {
    crate::storage::PrivateStoreIo::remove_file_durable(path)
        .map(|_| ())
        .map_err(TraceDecayError::from)
}

fn is_private_store_durable_removal_tombstone_name(name: &str) -> bool {
    let Some(random) = name.strip_prefix(crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX) else {
        return false;
    };
    random.len() == 6 && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn quarantine_tombstone_is_bounded_regular(path: &Path) -> Result<bool> {
    let Some(file) = open_regular_nofollow(path)? else {
        return Ok(false);
    };
    Ok(file.metadata().map_err(TraceDecayError::from)?.len() <= MAX_APPEND_INTENT_BYTES)
}

fn is_corrupt_append_intent_quarantine_owned_temp_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((prefix, nonce)) = body.rsplit_once('.') else {
        return false;
    };
    let Some((prefix, process_id)) = prefix.rsplit_once('.') else {
        return false;
    };
    let Some(destination) = prefix.strip_suffix(".automation-run-corrupt-append-intent") else {
        return false;
    };
    is_exact_spool_destination_name(destination)
        && is_canonical_decimal(process_id, u32::MAX as u64)
        && is_canonical_decimal(nonce, u64::MAX)
}

fn publish_corrupt_append_intent_quarantine(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_corrupt_append_intent_quarantine_with_publisher(path, bytes, &replace_file_atomically)
}

fn publish_corrupt_append_intent_quarantine_with_publisher(
    path: &Path,
    bytes: &[u8],
    publish_file: &AtomicFilePublisher<'_>,
) -> Result<()> {
    tracedecay_domain::with_owned_temp_publish(
        path,
        "automation-run-corrupt-append-intent",
        |temporary, destination| {
            publish_file(
                temporary,
                destination,
                "automation run corrupt append-intent quarantine",
            )
        },
        |output| output.write_all(bytes),
        tracedecay_application::DirectorySyncPolicy::Strict,
    )
    .map_err(TraceDecayError::from)?;
    verify_exact_corrupt_append_intent_quarantine(path, bytes)
}

fn verify_exact_corrupt_append_intent_quarantine(path: &Path, expected: &[u8]) -> Result<()> {
    let file = open_regular_nofollow(path)?.ok_or_else(|| {
        config_error("automation run append-intent quarantine disappeared after publication")
    })?;
    if file.metadata().map_err(TraceDecayError::from)?.len() != expected.len() as u64 {
        return Err(config_error(
            "automation run append-intent quarantine conflicts with its digest identity",
        ));
    }
    let mut actual = Vec::with_capacity(expected.len().saturating_add(1));
    file.take(MAX_APPEND_INTENT_BYTES + 1)
        .read_to_end(&mut actual)
        .map_err(TraceDecayError::from)?;
    if actual != expected {
        return Err(config_error(
            "automation run append-intent quarantine conflicts with its digest identity",
        ));
    }
    Ok(())
}

fn repair_corrupt_append_intent(
    dashboard_root: &Path,
    ledger_path: &Path,
    ledger: &mut std::fs::File,
    corrupt_bytes: &[u8],
) -> Result<()> {
    let ledger_len = ledger.metadata().map_err(TraceDecayError::from)?.len();
    if ledger_len == 0 || ledger_ends_with_newline(ledger, ledger_len)? {
        validate_committed_ledger(ledger, ledger_path)?;
        sync_run_ledger_file_and_parent(ledger_path, ledger)?;
        return clear_corrupt_append_intent_if_unchanged(dashboard_root, corrupt_bytes);
    }
    let clean_eof = last_committed_boundary(ledger, ledger_len)?;
    validate_committed_ledger_prefix(ledger, ledger_path, clean_eof)?;
    let suffix_len = ledger_len
        .checked_sub(clean_eof)
        .ok_or_else(|| config_error("automation run ledger suffix range is invalid"))?;
    let mut matched = None;
    let directory = dashboard_root.join(EXACT_RUN_SPOOL_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(config_error(
                "corrupt append intent has no exact spool authority for its ledger suffix",
            ));
        }
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    let mut count = 0_usize;
    for entry in entries {
        let entry = entry.map_err(TraceDecayError::from)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(config_error("automation run spool has a non-UTF-8 entry"));
        };
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file()
            || !is_exact_spool_destination_name(name)
        {
            return Err(config_error(
                "automation run spool directory has a foreign entry",
            ));
        }
        count = count.saturating_add(1);
        if count > 256 {
            return Err(config_error(
                "automation run spool directory exceeded its recovery bound",
            ));
        }
        let path = entry.path();
        let Some(mut spool) = open_regular_nofollow(&path)? else {
            return Err(config_error(
                "automation run spool disappeared during corrupt-intent recovery",
            ));
        };
        let spool_len = spool.metadata().map_err(TraceDecayError::from)?.len();
        let Some(identity) = super::exact_lookup::scan_jsonl_row(&spool, &path, 0..spool_len)?
        else {
            return Err(config_error(
                "automation run spool does not contain a canonical ledger row",
            ));
        };
        if suffix_len > spool_len {
            continue;
        }
        if !identity.status.is_terminal() {
            return Err(config_error(
                "automation run spool is not a terminal ledger row",
            ));
        }
        let (ledger_digest, payload_len) = digest_file_handle(&mut spool)?;
        let publication = ExactRunPublication {
            schema_version: 1,
            ledger_digest,
            payload_len,
        };
        if spool_path(dashboard_root, &identity.run_id, &publication)? != path {
            return Err(config_error(
                "automation run spool conflicts with its canonical identity",
            ));
        }
        ledger
            .seek(SeekFrom::Start(clean_eof))
            .map_err(TraceDecayError::from)?;
        spool
            .seek(SeekFrom::Start(0))
            .map_err(TraceDecayError::from)?;
        if exact_prefix_matches(ledger, &mut spool, suffix_len)? {
            if matched.replace(path).is_some() {
                return Err(config_error(
                    "corrupt append intent has ambiguous exact spool authority",
                ));
            }
        }
    }
    if matched.is_none() {
        return Err(config_error(
            "corrupt append intent ledger suffix is not owned by an exact spool",
        ));
    }
    ledger.set_len(clean_eof).map_err(TraceDecayError::from)?;
    sync_run_ledger_file_and_parent(ledger_path, ledger)?;
    clear_corrupt_append_intent_if_unchanged(dashboard_root, corrupt_bytes)
}

fn clear_corrupt_append_intent_if_unchanged(dashboard_root: &Path, expected: &[u8]) -> Result<()> {
    match read_append_intent_state(dashboard_root)? {
        LedgerAppendIntentState::Corrupt(actual) if actual == expected => {
            clear_append_intent(dashboard_root)
        }
        _ => Err(config_error(
            "automation run append intent changed during corrupt-intent recovery",
        )),
    }
}

fn ledger_ends_with_newline(ledger: &mut std::fs::File, len: u64) -> Result<bool> {
    ledger
        .seek(SeekFrom::Start(len.saturating_sub(1)))
        .map_err(TraceDecayError::from)?;
    let mut byte = [0_u8; 1];
    ledger
        .read_exact(&mut byte)
        .map_err(TraceDecayError::from)?;
    Ok(byte[0] == b'\n')
}

fn last_committed_boundary(ledger: &mut std::fs::File, len: u64) -> Result<u64> {
    let mut cursor = len;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    while cursor > 0 {
        let start = cursor.saturating_sub(COPY_BUFFER_BYTES as u64);
        let take = usize::try_from(cursor - start)
            .map_err(|_| config_error("automation run ledger tail chunk is not representable"))?;
        ledger
            .seek(SeekFrom::Start(start))
            .map_err(TraceDecayError::from)?;
        ledger
            .read_exact(&mut buffer[..take])
            .map_err(TraceDecayError::from)?;
        if let Some(offset) = buffer[..take].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + offset as u64 + 1);
        }
        cursor = start;
    }
    Ok(0)
}

fn validate_committed_ledger(ledger: &std::fs::File, path: &Path) -> Result<()> {
    let mut rows = super::exact_lookup::ForwardJsonlScanner::new(ledger, path)?;
    while let Some(span) = rows.next_span()? {
        if super::exact_lookup::scan_jsonl_row(ledger, path, span)?.is_none() {
            return Err(config_error(
                "automation run ledger contains an empty committed row",
            ));
        }
    }
    Ok(())
}

fn validate_committed_ledger_prefix(
    ledger: &std::fs::File,
    path: &Path,
    clean_eof: u64,
) -> Result<()> {
    let mut rows = super::exact_lookup::ForwardJsonlScanner::new_bounded(ledger, path, clean_eof)?;
    while let Some(span) = rows.next_span()? {
        if super::exact_lookup::scan_jsonl_row(ledger, path, span)?.is_none() {
            return Err(config_error(
                "automation run ledger contains an empty committed row",
            ));
        }
    }
    Ok(())
}

fn exact_prefix_matches(
    left: &mut std::fs::File,
    right: &mut std::fs::File,
    mut remaining: u64,
) -> Result<bool> {
    let mut left_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut right_buffer = [0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(COPY_BUFFER_BYTES as u64))
            .map_err(|_| config_error("automation run prefix chunk is not representable"))?;
        left.read_exact(&mut left_buffer[..take])
            .map_err(TraceDecayError::from)?;
        right
            .read_exact(&mut right_buffer[..take])
            .map_err(TraceDecayError::from)?;
        if left_buffer[..take] != right_buffer[..take] {
            return Ok(false);
        }
        remaining -= take as u64;
    }
    Ok(true)
}

fn discard_unbound_spools_for_run(dashboard_root: &Path, run_id: &str) -> Result<()> {
    validate_run_id_component(run_id)?;
    let directory = dashboard_root.join(EXACT_RUN_SPOOL_DIR);
    visit_spool_rows(&directory, |_| Ok(()))?;
    visit_spool_rows(&directory, |path, identity| {
        if identity.run_id == run_id {
            crate::storage::PrivateStoreIo::remove_file_durable(path)
                .map_err(TraceDecayError::from)?;
        }
        Ok(())
    })
}

fn visit_spool_rows(
    directory: &Path,
    mut visit: impl FnMut(&Path, &super::exact_lookup::RunLedgerRowProjection) -> Result<()>,
) -> Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    for entry in entries {
        let entry = entry.map_err(TraceDecayError::from)?;
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(file) = open_regular_nofollow(&path)? else {
            continue;
        };
        let len = file.metadata().map_err(TraceDecayError::from)?.len();
        let Some(identity) = super::exact_lookup::scan_jsonl_row(&file, &path, 0..len)? else {
            return Err(config_error(
                "automation run spool does not contain a canonical ledger row",
            ));
        };
        visit(&path, &identity)?;
    }
    Ok(())
}

fn validate_same_run_spool_history(
    dashboard_root: &Path,
    run_id: &str,
    expected: &ExactRunPublication,
) -> Result<()> {
    let directory = dashboard_root.join(EXACT_RUN_SPOOL_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    for entry in entries {
        let entry = entry.map_err(TraceDecayError::from)?;
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(mut file) = open_regular_nofollow(&path)? else {
            continue;
        };
        let len = file.metadata().map_err(TraceDecayError::from)?.len();
        let Some(identity) = super::exact_lookup::scan_jsonl_row(&file, &path, 0..len)? else {
            return Err(config_error(
                "automation run spool does not contain a canonical ledger row",
            ));
        };
        if identity.run_id != run_id {
            continue;
        }
        let (digest, payload_len) = digest_file_handle(&mut file)?;
        if digest != expected.ledger_digest || payload_len != expected.payload_len {
            return Err(config_error(format!(
                "automation run '{run_id}' has conflicting staged history"
            )));
        }
    }
    Ok(())
}

fn with_spool_lock<T>(dashboard_root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let directory = dashboard_root.join(EXACT_RUN_SPOOL_DIR);
    crate::storage::PrivateStoreIo::create_dir_all_durable(&directory)
        .map_err(TraceDecayError::from)?;
    let lock_path = dashboard_root.join(EXACT_RUN_SPOOL_LOCK);
    let lock = acquire_nofollow_lock(&lock_path).map_err(TraceDecayError::from)?;
    let result = cleanup_abandoned_exact_spool_temps(&directory).and_then(|()| operation());
    let unlock = fs2::FileExt::unlock(&lock).map_err(TraceDecayError::from);
    result.and_then(|value| unlock.map(|()| value))
}

fn cleanup_abandoned_exact_spool_temps(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory).map_err(TraceDecayError::from)? {
        let entry = entry.map_err(TraceDecayError::from)?;
        if !entry.file_type().map_err(TraceDecayError::from)?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_exact_spool_owned_temp_name(name) {
            continue;
        }
        crate::storage::PrivateStoreIo::remove_file_durable(&entry.path())
            .map_err(TraceDecayError::from)?;
    }
    Ok(())
}

fn is_exact_spool_owned_temp_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((prefix, nonce)) = body.rsplit_once('.') else {
        return false;
    };
    let Some((prefix, process_id)) = prefix.rsplit_once('.') else {
        return false;
    };
    let Some(destination) = prefix.strip_suffix(".automation-run-exact-spool") else {
        return false;
    };
    is_exact_spool_destination_name(destination)
        && is_canonical_decimal(process_id, u32::MAX as u64)
        && is_canonical_decimal(nonce, u64::MAX)
}

fn is_exact_spool_destination_name(name: &str) -> bool {
    let Some(digest) = name.strip_suffix(".json") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_canonical_decimal(value: &str, maximum: u64) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<u64>().is_ok_and(|parsed| parsed <= maximum)
}

fn digest_regular_file(path: &Path) -> Result<Option<(ManifestDigest, u64)>> {
    crate::storage::reject_symlink_components(path, "automation run exact spool")
        .map_err(TraceDecayError::from)?;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(config_error(
            "automation run exact spool is not a regular file",
        ));
    }
    let Some(mut file) = open_regular_nofollow(path)? else {
        return Ok(None);
    };
    digest_file_handle(&mut file).map(Some)
}

fn open_regular_nofollow(path: &Path) -> Result<Option<std::fs::File>> {
    let parent = path
        .parent()
        .ok_or_else(|| config_error("automation run spool has no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| config_error("automation run spool has no file name"))?;
    let directory = match Dir::open_ambient_dir(parent, ambient_authority()) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(TraceDecayError::from(error)),
    };
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    match directory.open_with(name, &options) {
        Ok(file) => {
            let metadata = file.metadata().map_err(TraceDecayError::from)?;
            if !metadata.is_file() {
                return Err(config_error(
                    "automation run exact spool is not a regular file",
                ));
            }
            Ok(Some(file.into_std()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TraceDecayError::from(error)),
    }
}

fn digest_file_handle(file: &mut std::fs::File) -> Result<(ManifestDigest, u64)> {
    file.seek(SeekFrom::Start(0))
        .map_err(TraceDecayError::from)?;
    let mut writer = DigestWriter::default();
    std::io::copy(file, &mut writer).map_err(TraceDecayError::from)?;
    writer.finish_with_len()
}

#[derive(Default)]
struct DigestWriter {
    hasher: Sha256,
    len: u64,
}

impl DigestWriter {
    fn finish_with_len(self) -> Result<(ManifestDigest, u64)> {
        let mut encoded = String::with_capacity(71);
        encoded.push_str("sha256:");
        for byte in self.hasher.finalize() {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}")
                .map_err(|error| config_error(format!("failed to encode run digest: {error}")))?;
        }
        let digest = ManifestDigest::new(encoded)
            .map_err(|error| config_error(format!("invalid automation run digest: {error}")))?;
        Ok((digest, self.len))
    }
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(bytes);
        self.len = self.len.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::backend::AgentTaskKind;
    use crate::automation::run_ledger::{AutomationRunStatus, AutomationTrigger};

    fn record(run_id: &str) -> AutomationRunLedgerRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "run_id": run_id,
            "trigger": AutomationTrigger::Dashboard,
            "task": AgentTaskKind::SkillWriter,
            "backend": "codex_app_server",
            "status": AutomationRunStatus::Skipped,
            "accepted_count": 0,
            "rejected_count": 0,
            "error": "no_skill_writer_evidence",
            "fallback_status": "no_skill_writer_evidence",
            "started_at": "1",
            "completed_at": "2"
        }))
        .expect("record")
    }

    #[tokio::test]
    async fn exact_spool_publishes_replays_and_rejects_conflict() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("spool-exact");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("publish"),
            ExactRunPublishOutcome::Published
        );
        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("replay"),
            ExactRunPublishOutcome::Published
        );

        let mut conflicting = record;
        conflicting.backend = "claude_cli".to_owned();
        assert!(stage_run_record_exact(temp.path(), &conflicting).is_err());
    }

    #[test]
    fn conflicting_same_run_stage_does_not_leave_a_second_spool() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = record("same-run-spool-conflict");
        let publication = stage_run_record_exact(temp.path(), &original).expect("stage original");
        let original_spool =
            spool_path(temp.path(), &original.run_id, &publication).expect("original spool");
        assert!(original_spool.exists());

        let mut conflicting = original;
        conflicting.backend = "claude_cli".to_owned();
        let error = stage_run_record_exact(temp.path(), &conflicting)
            .expect_err("conflicting row must not stage");
        assert!(error.to_string().contains("conflicting staged history"));
        let spool_files = std::fs::read_dir(temp.path().join(EXACT_RUN_SPOOL_DIR))
            .expect("spool directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert_eq!(spool_files, 1);
        assert!(original_spool.exists());
    }

    #[test]
    fn exact_stage_rejects_semantically_invalid_candidate_before_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut invalid = record("invalid-stage-candidate");
        invalid.schema_version = 99;

        let error = stage_run_record_exact(temp.path(), &invalid)
            .expect_err("unsupported schema must not stage");

        assert!(error.to_string().contains("schema version 99"));
        assert!(!temp.path().join(EXACT_RUN_SPOOL_DIR).exists());
    }

    #[test]
    fn bound_spool_rejects_semantically_invalid_exact_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let valid = record("invalid-bound-spool");
        let invalid_bytes = serde_json::to_string(&valid)
            .expect("record")
            .replace("\"completed_at\":\"2\"", "\"completed_at\":\"-1\"");
        let mut writer = DigestWriter::default();
        writer
            .write_all(invalid_bytes.as_bytes())
            .expect("digest invalid bytes");
        let (ledger_digest, payload_len) = writer.finish_with_len().expect("identity");
        let publication = ExactRunPublication {
            schema_version: 1,
            ledger_digest,
            payload_len,
        };
        let spool = spool_path(temp.path(), &valid.run_id, &publication).expect("bound spool path");
        std::fs::create_dir_all(spool.parent().expect("spool directory")).expect("spool directory");
        std::fs::write(&spool, invalid_bytes).expect("invalid bound spool");

        let error =
            publish_staged_run_record_exact_blocking(temp.path(), &valid.run_id, &publication)
                .expect_err("invalid bound spool must not publish");

        assert!(error.to_string().contains("predates the UNIX epoch"));
        assert_eq!(
            std::fs::metadata(run_ledger_path(temp.path()))
                .expect("empty ledger")
                .len(),
            0
        );
        assert!(!append_intent_path(temp.path()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn exact_spool_temp_is_mode_0600_before_atomic_replace() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("private-exact-spool-temp");
        let observed_private_temp = std::cell::Cell::new(false);
        let (publication, ()) = bind_staged_run_record_exact_with_publisher(
            temp.path(),
            &record,
            &|temporary, destination, label| {
                assert_eq!(label, "automation run exact spool");
                assert_eq!(
                    std::fs::metadata(temporary)?.permissions().mode() & 0o777,
                    0o600
                );
                observed_private_temp.set(true);
                replace_file_atomically(temporary, destination, label)
            },
            |_| Ok(()),
        )
        .expect("private exact spool publication");
        assert!(observed_private_temp.get());

        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool path");
        assert_eq!(
            std::fs::metadata(spool)
                .expect("published spool")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(windows)]
    #[test]
    fn exact_spool_production_publication_has_private_windows_acl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("private-windows-exact-spool");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool path");

        tracedecay_runtime_core::windows_security::validate_private_file(&spool)
            .expect("published spool must retain the canonical private Windows ACL");
    }

    #[test]
    fn visible_exact_spool_is_write_through_republished_before_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("visible-spool-write-through-retry");
        let bind_calls = std::cell::Cell::new(0_usize);

        let first = bind_staged_run_record_exact_with_publisher(
            temp.path(),
            &record,
            &|temporary, destination, label| {
                assert_eq!(label, "automation run exact spool");
                std::fs::rename(temporary, destination)?;
                Err(std::io::Error::other(
                    "injected uncertainty after visible spool rename",
                ))
            },
            |_| {
                bind_calls.set(bind_calls.get() + 1);
                Ok(())
            },
        );
        assert!(
            first
                .expect_err("uncertain publication must precede binding")
                .to_string()
                .contains("visible spool rename")
        );
        assert_eq!(bind_calls.get(), 0);

        let (ledger_digest, payload_len) = run_record_exact_identity(&record).expect("identity");
        let expected = ExactRunPublication {
            schema_version: 1,
            ledger_digest,
            payload_len,
        };
        let spool = spool_path(temp.path(), &record.run_id, &expected).expect("spool path");
        assert_eq!(
            digest_regular_file(&spool).expect("visible spool"),
            Some((expected.ledger_digest.clone(), expected.payload_len))
        );

        let retry_failure = bind_staged_run_record_exact_with_publisher(
            temp.path(),
            &record,
            &|_, _, _| {
                Err(std::io::Error::other(
                    "injected write-through spool republish failure",
                ))
            },
            |_| {
                bind_calls.set(bind_calls.get() + 1);
                Ok(())
            },
        );
        assert!(
            retry_failure
                .expect_err("failed redurability must retain the unbound spool")
                .to_string()
                .contains("republish failure")
        );
        assert_eq!(bind_calls.get(), 0);
        assert_eq!(
            digest_regular_file(&spool).expect("retained spool"),
            Some((expected.ledger_digest.clone(), expected.payload_len))
        );

        let republish_calls = std::cell::Cell::new(0_usize);
        let (actual, ()) = bind_staged_run_record_exact_with_publisher(
            temp.path(),
            &record,
            &|temporary, destination, label| {
                assert_eq!(label, "automation run exact spool");
                republish_calls.set(republish_calls.get() + 1);
                replace_file_atomically(temporary, destination, label)
            },
            |_| {
                bind_calls.set(bind_calls.get() + 1);
                Ok(())
            },
        )
        .expect("exact write-through retry and readback");
        assert_eq!(actual, expected);
        assert_eq!(republish_calls.get(), 1);
        assert_eq!(bind_calls.get(), 1);
        assert_eq!(
            digest_regular_file(&spool).expect("republished spool"),
            Some((actual.ledger_digest, actual.payload_len))
        );
    }

    #[cfg(unix)]
    #[test]
    fn bind_does_not_trust_visible_row_when_stabilized_open_fails() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("unstabilized-visible-ledger-row");
        let mut payload = serde_json::to_vec(&record).expect("payload");
        payload.push(b'\n');
        let target = temp.path().join("visible-ledger-target.jsonl");
        std::fs::write(&target, payload).expect("target");
        let ledger = run_ledger_path(temp.path());
        symlink(&target, &ledger).expect("ledger symlink");
        let bound = AtomicBool::new(false);

        let error = bind_staged_run_record_exact(temp.path(), &record, |_| {
            bound.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect_err("failed stabilization must precede durable binding");
        assert!(!bound.load(Ordering::SeqCst));
        assert!(error.to_string().contains("symlink"));
        let spool_files = std::fs::read_dir(temp.path().join(EXACT_RUN_SPOOL_DIR))
            .expect("spool directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert_eq!(spool_files, 0);
    }

    #[tokio::test]
    async fn published_row_replay_rejects_a_correct_digest_with_wrong_bound_length() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("published-wrong-bound-length");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("publish"),
            ExactRunPublishOutcome::Published
        );
        discard_staged_run_record_exact(temp.path(), &record.run_id, &publication)
            .await
            .expect("retire spool");

        let mut wrong_length = publication;
        wrong_length.payload_len = wrong_length
            .payload_len
            .checked_add(1)
            .expect("test payload length");
        let error = publish_staged_run_record_exact(temp.path(), &record.run_id, &wrong_length)
            .await
            .expect_err("matching digest with a different bound length must conflict");
        assert!(error.to_string().contains("inconsistent durable history"));
    }

    #[tokio::test]
    async fn published_identity_uses_trimmed_json_span_not_crlf_framing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("published-with-crlf-framing");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool");
        let payload = std::fs::read(spool).expect("payload");
        let mut framed = b" \t".to_vec();
        framed.extend_from_slice(&payload);
        framed.extend_from_slice(b" \r\n");
        std::fs::write(run_ledger_path(temp.path()), framed).expect("framed ledger");

        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("trimmed identity replay"),
            ExactRunPublishOutcome::Published
        );
    }

    #[tokio::test]
    async fn valid_json_without_newline_is_preserved_as_an_incomplete_durable_tail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("valid-json-without-newline");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool");
        let payload = std::fs::read(&spool).expect("payload");
        let ledger = run_ledger_path(temp.path());
        std::fs::write(&ledger, &payload).expect("unterminated exact row");

        let error = publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
            .await
            .expect_err("unterminated row must not be accepted as published");
        assert!(error.to_string().contains("incomplete durable tail"));
        assert_eq!(std::fs::read(&ledger).expect("preserved ledger"), payload);
        assert!(spool.exists());
        assert!(!append_intent_path(temp.path()).exists());
    }

    #[tokio::test]
    async fn overflowing_framed_payload_length_fails_before_publication_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let publication = ExactRunPublication {
            schema_version: 1,
            ledger_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
                .expect("digest"),
            payload_len: u64::MAX,
        };

        let error =
            publish_staged_run_record_exact(temp.path(), "overflowing-framed-row", &publication)
                .await
                .expect_err("framed length overflow must fail");
        assert!(
            error
                .to_string()
                .contains("framed payload length overflowed")
        );
        assert!(!run_ledger_path(temp.path()).exists());
        assert!(!append_intent_path(temp.path()).exists());
        assert!(!temp.path().join(EXACT_RUN_SPOOL_DIR).exists());
    }

    #[test]
    fn spool_lock_retires_only_exact_owned_temp_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("abandoned-owned-spool-temp");
        let (ledger_digest, payload_len) = run_record_exact_identity(&record).expect("identity");
        let publication = ExactRunPublication {
            schema_version: 1,
            ledger_digest,
            payload_len,
        };
        let destination = spool_path(temp.path(), &record.run_id, &publication).expect("spool");
        let directory = destination.parent().expect("spool directory");
        std::fs::create_dir_all(directory).expect("create spool directory");
        let destination_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .expect("destination name");
        let owned = directory.join(format!(
            ".{destination_name}.automation-run-exact-spool.123.456.tmp"
        ));
        std::fs::write(&owned, b"abandoned owned payload").expect("owned temp");
        let near_misses = [
            directory.join(format!(
                ".{destination_name}.automation-run-exact-spool.0123.456.tmp"
            )),
            directory.join(format!(
                ".{destination_name}.automation-run-exact-spool.123.0456.tmp"
            )),
            directory.join(format!(
                ".{destination_name}.foreign-spool-kind.123.456.tmp"
            )),
            directory.join(format!(
                ".{destination_name}.automation-run-exact-spool.123.456.tmp.foreign"
            )),
        ];
        for path in &near_misses {
            std::fs::write(path, b"foreign payload").expect("near-miss temp");
        }

        with_spool_lock(temp.path(), || Ok(())).expect("spool cleanup");

        assert!(!owned.exists());
        for path in near_misses {
            assert!(path.exists(), "foreign near-miss must be preserved");
        }
    }

    #[tokio::test]
    async fn exact_spool_streams_and_retires_a_multi_megabyte_row() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut record = record("spool-large-skill-writer");
        record.proposed_ops = Some(serde_json::json!({
            "skills": [{
                "id": "large-skill",
                "body_markdown": "x".repeat(256 * 1024),
                "support_files": (0..20)
                    .map(|index| serde_json::json!({
                        "path": format!("references/{index}.md"),
                        "text": "y".repeat(64 * 1024),
                    }))
                    .collect::<Vec<_>>(),
            }]
        }));
        assert!(serde_json::to_vec(&record).expect("encoded").len() > 1_500_000);

        let publication = stage_run_record_exact(temp.path(), &record).expect("stage large row");
        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("publish large row"),
            ExactRunPublishOutcome::Published
        );
        discard_staged_run_record_exact(temp.path(), &record.run_id, &publication)
            .await
            .expect("retire spool");
        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &record.run_id, &publication)
                .await
                .expect("ledger-only replay"),
            ExactRunPublishOutcome::Published
        );
    }

    #[tokio::test]
    async fn unbound_cleanup_retains_payload_until_authority_revalidates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("conditional-unbound-cleanup");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool");

        assert_eq!(
            discard_unbound_staged_run_records_if(temp.path(), &record.run_id, || Ok(false))
                .await
                .expect("retain"),
            ExactRunUnboundDiscardOutcome::Retained
        );
        assert!(spool.exists());

        assert_eq!(
            discard_unbound_staged_run_records_if(temp.path(), &record.run_id, || Ok(true))
                .await
                .expect("discard"),
            ExactRunUnboundDiscardOutcome::Discarded
        );
        assert!(!spool.exists());
    }

    #[tokio::test]
    async fn unbound_cleanup_rejects_semantically_invalid_spool_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = record("conditional-unbound-semantic-target");
        stage_run_record_exact(temp.path(), &target).expect("stage target");
        let invalid = record("conditional-unbound-semantic-invalid");
        let invalid_publication =
            stage_run_record_exact(temp.path(), &invalid).expect("stage invalid candidate");
        let invalid_spool = spool_path(temp.path(), &invalid.run_id, &invalid_publication)
            .expect("invalid spool path");
        let invalid_bytes = std::fs::read_to_string(&invalid_spool)
            .expect("valid spool")
            .replace("\"schema_version\":2", "\"schema_version\":99");
        std::fs::write(&invalid_spool, invalid_bytes).expect("corrupt spool semantics");

        let error = discard_unbound_staged_run_records_if(temp.path(), &target.run_id, || Ok(true))
            .await
            .expect_err("invalid spool history must block cleanup");

        assert!(error.to_string().contains("schema version 99"));
        let target_publication = run_record_exact_identity(&target)
            .map(|(ledger_digest, payload_len)| ExactRunPublication {
                schema_version: 1,
                ledger_digest,
                payload_len,
            })
            .expect("target publication");
        assert!(
            spool_path(temp.path(), &target.run_id, &target_publication)
                .expect("target spool")
                .exists()
        );
        assert!(invalid_spool.exists());
    }

    #[test]
    fn staging_rejects_semantically_invalid_existing_spool_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let existing = record("same-run-semantic-spool");
        let publication =
            stage_run_record_exact(temp.path(), &existing).expect("stage existing row");
        let spool =
            spool_path(temp.path(), &existing.run_id, &publication).expect("existing spool path");
        let invalid_bytes = std::fs::read_to_string(&spool)
            .expect("valid spool")
            .replace("\"started_at\":\"1\"", "\"started_at\":\"-1\"");
        std::fs::write(&spool, invalid_bytes).expect("corrupt spool semantics");

        let error = stage_run_record_exact(temp.path(), &existing)
            .expect_err("invalid existing spool must block staging");

        assert!(error.to_string().contains("predates the UNIX epoch"));
        assert!(spool.exists());
    }

    #[tokio::test]
    async fn intent_owned_partial_tail_is_repaired_but_foreign_tail_is_preserved() {
        let temp = tempfile::tempdir().expect("tempdir");
        let expected = record("owned-partial-tail");
        let publication = stage_run_record_exact(temp.path(), &expected).expect("stage");
        let spool = spool_path(temp.path(), &expected.run_id, &publication).expect("spool path");
        let bytes = std::fs::read(&spool).expect("spool bytes");
        let ledger = run_ledger_path(temp.path());
        let intent = LedgerAppendIntent {
            schema_version: 1,
            run_id: expected.run_id.clone(),
            pre_append_eof: 0,
            publication: publication.clone(),
        };
        write_append_intent(temp.path(), &intent).expect("intent");
        std::fs::write(&ledger, &bytes[..bytes.len() / 2]).expect("partial tail");

        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &expected.run_id, &publication)
                .await
                .expect("repair and publish"),
            ExactRunPublishOutcome::Published
        );
        assert_eq!(
            super::super::exact_lookup::read_exact_run_digest(&ledger, &expected.run_id)
                .expect("digest"),
            Some(publication.ledger_digest.clone())
        );

        let foreign = record("foreign-partial-tail");
        let foreign_bytes = serde_json::to_vec(&foreign).expect("foreign bytes");
        write_append_intent(temp.path(), &intent).expect("foreign-tail intent");
        std::fs::write(&ledger, &foreign_bytes[..foreign_bytes.len() / 2])
            .expect("foreign partial");
        let error = publish_staged_run_record_exact(temp.path(), &expected.run_id, &publication)
            .await
            .expect_err("foreign tail must not be truncated");
        assert!(error.to_string().contains("does not own"));
        assert_eq!(
            std::fs::read(&ledger).expect("preserved foreign tail"),
            foreign_bytes[..foreign_bytes.len() / 2]
        );
    }

    #[test]
    fn visible_append_intent_is_write_through_republished_before_clear() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("visible-append-intent-write-through-retry");
        let publication = stage_run_record_exact(temp.path(), &record).expect("stage");
        let spool = spool_path(temp.path(), &record.run_id, &publication).expect("spool path");
        let ledger = run_ledger_path(temp.path());

        let first = publish_staged_run_record_exact_blocking_with_publisher(
            temp.path(),
            &record.run_id,
            &publication,
            &|temporary, destination, label| {
                assert_eq!(label, "automation run ledger append intent");
                std::fs::rename(temporary, destination)?;
                Err(std::io::Error::other(
                    "injected uncertainty after visible append-intent rename",
                ))
            },
        );
        assert!(
            first
                .expect_err("visible intent uncertainty must remain an error")
                .to_string()
                .contains("append-intent rename")
        );
        let intent_path = append_intent_path(temp.path());
        let intent_before = std::fs::read(&intent_path).expect("visible append intent");
        assert!(spool.exists());
        assert_eq!(std::fs::metadata(&ledger).expect("empty ledger").len(), 0);

        let retry_failure = publish_staged_run_record_exact_blocking_with_publisher(
            temp.path(),
            &record.run_id,
            &publication,
            &|_, _, _| {
                Err(std::io::Error::other(
                    "injected write-through append-intent republish failure",
                ))
            },
        );
        assert!(
            retry_failure
                .expect_err("failed intent redurability must preserve authority")
                .to_string()
                .contains("republish failure")
        );
        assert_eq!(
            std::fs::read(&intent_path).expect("retained append intent"),
            intent_before
        );
        assert!(spool.exists());
        assert_eq!(std::fs::metadata(&ledger).expect("empty ledger").len(), 0);

        let republish_calls = std::cell::Cell::new(0_usize);
        assert_eq!(
            publish_staged_run_record_exact_blocking_with_publisher(
                temp.path(),
                &record.run_id,
                &publication,
                &|temporary, destination, label| {
                    assert_eq!(label, "automation run ledger append intent");
                    republish_calls.set(republish_calls.get() + 1);
                    replace_file_atomically(temporary, destination, label)
                },
            )
            .expect("write-through republish, readback, and exact append"),
            ExactRunPublishOutcome::Published
        );
        assert_eq!(republish_calls.get(), 1);
        assert!(!intent_path.exists());
        assert!(spool.exists());
        assert_eq!(
            super::super::exact_lookup::read_exact_run_digest(&ledger, &record.run_id)
                .expect("published digest"),
            Some(publication.ledger_digest)
        );
    }

    fn corrupt_intent_quarantine_path(dashboard_root: &Path, bytes: &[u8]) -> PathBuf {
        dashboard_root
            .join(EXACT_RUN_APPEND_INTENT_QUARANTINE_DIR)
            .join(format!("{}.json", hex::encode(Sha256::digest(bytes))))
    }

    #[test]
    fn corrupt_intent_repair_durably_retires_exact_owned_quarantine_temp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-with-abandoned-quarantine-temp";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        let directory = quarantine_path.parent().expect("quarantine directory");
        std::fs::create_dir_all(directory).expect("quarantine directory");
        let destination = quarantine_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("quarantine destination");
        let owned = directory.join(format!(
            ".{destination}.automation-run-corrupt-append-intent.123.456.tmp"
        ));
        std::fs::write(&owned, b"abandoned atomic-write payload").expect("owned temp");

        repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect("owned temp cleanup and corrupt-intent repair");

        assert!(!owned.exists());
        assert!(!intent_path.exists());
        assert_eq!(
            std::fs::read(&quarantine_path).expect("durable quarantine"),
            corrupt
        );
    }

    #[test]
    fn corrupt_intent_repair_recovers_windows_owned_temp_removal_tombstone() {
        let corrupt = b"corrupt-intent-with-windows-removal-tombstone";
        for (label, retired_bytes) in [
            ("empty", &b""[..]),
            ("partial", &corrupt[..corrupt.len() / 2]),
            ("full", corrupt.as_slice()),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let intent_path = append_intent_path(temp.path());
            std::fs::write(&intent_path, corrupt).expect("corrupt intent");
            let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
            let directory = quarantine_path.parent().expect("quarantine directory");
            std::fs::create_dir_all(directory).expect("quarantine directory");
            let destination = quarantine_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("quarantine destination");
            let owned = directory.join(format!(
                ".{destination}.automation-run-corrupt-append-intent.123.456.tmp"
            ));
            std::fs::write(&owned, retired_bytes).expect("owned temp");
            let tombstone = directory.join(format!(
                "{}Ab12Z9",
                crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
            ));

            let failure = repair_corrupt_run_ledger_append_intent_with(
                temp.path(),
                publish_corrupt_append_intent_quarantine,
                publish_corrupt_append_intent_quarantine,
                |path| {
                    assert_eq!(path, owned);
                    std::fs::rename(path, &tombstone).map_err(TraceDecayError::from)?;
                    Err(config_error(
                        "injected Windows deletion failure after durable retirement",
                    ))
                },
            );
            assert!(
                failure
                    .expect_err("Windows deletion failure must retain recovery authority")
                    .to_string()
                    .contains("deletion failure"),
                "{label} tombstone"
            );
            assert!(!owned.exists());
            assert_eq!(
                std::fs::read(&tombstone).expect("removal tombstone"),
                retired_bytes,
                "{label} tombstone"
            );
            assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);
            assert!(!quarantine_path.exists());

            let retry_tombstone = directory.join(format!(
                "{}Z9Ab12",
                crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
            ));
            let recursive_failure = repair_corrupt_run_ledger_append_intent_with(
                temp.path(),
                publish_corrupt_append_intent_quarantine,
                publish_corrupt_append_intent_quarantine,
                |path| {
                    assert_eq!(path, tombstone);
                    std::fs::rename(path, &retry_tombstone).map_err(TraceDecayError::from)?;
                    Err(config_error(
                        "injected repeated Windows deletion tombstone failure",
                    ))
                },
            );
            assert!(
                recursive_failure
                    .expect_err("recursive tombstone failure must retain recovery authority")
                    .to_string()
                    .contains("tombstone failure"),
                "{label} tombstone"
            );
            assert!(!tombstone.exists());
            assert_eq!(
                std::fs::read(&retry_tombstone).expect("retry removal tombstone"),
                retired_bytes,
                "{label} tombstone"
            );
            assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);

            repair_corrupt_run_ledger_append_intent_blocking(temp.path())
                .expect("retry retires exact Windows tombstone and repairs intent");
            assert!(!tombstone.exists());
            assert!(!retry_tombstone.exists());
            assert!(!intent_path.exists());
            assert_eq!(
                std::fs::read(&quarantine_path).expect("durable quarantine"),
                corrupt
            );
        }
    }

    #[test]
    fn corrupt_intent_quarantine_temp_near_misses_fail_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-with-foreign-quarantine-temp";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        let directory = quarantine_path.parent().expect("quarantine directory");
        std::fs::create_dir_all(directory).expect("quarantine directory");
        let destination = quarantine_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("quarantine destination");
        let near_misses = [
            (
                directory.join(format!(
                    ".{destination}.automation-run-corrupt-append-intent.0123.456.tmp"
                )),
                b"foreign temp payload".as_slice(),
            ),
            (
                directory.join(format!(
                    ".{destination}.automation-run-corrupt-append-intent.123.0456.tmp"
                )),
                b"foreign temp payload".as_slice(),
            ),
            (
                directory.join(format!(
                    ".{destination}.automation-run-corrupt-append-intents.123.456.tmp"
                )),
                b"foreign temp payload".as_slice(),
            ),
            (
                directory.join(format!(
                    ".{destination}.automation-run-corrupt-append-intent.123.456.tmp.foreign"
                )),
                b"foreign temp payload".as_slice(),
            ),
            (
                directory.join(format!(
                    "{}Ab12Z",
                    crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
                )),
                corrupt.as_slice(),
            ),
            (
                directory.join(format!(
                    "{}Ab12_9",
                    crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
                )),
                corrupt.as_slice(),
            ),
        ];
        for (path, payload) in &near_misses {
            std::fs::write(path, payload).expect("near-miss temp");
        }

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("foreign temp names must deny quarantine repair");
        assert!(error.to_string().contains("foreign entry"));
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);
        assert!(!quarantine_path.exists());
        for (path, _) in near_misses {
            assert!(path.exists(), "near-miss temp must be preserved");
        }
    }

    #[test]
    fn existing_corrupt_intent_quarantine_does_not_bypass_foreign_entry_denial() {
        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-with-existing-quarantine-and-foreign-entry";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        let directory = quarantine_path.parent().expect("quarantine directory");
        std::fs::create_dir_all(directory).expect("quarantine directory");
        std::fs::write(&quarantine_path, corrupt).expect("existing quarantine");
        let near_miss = directory.join(format!(
            "{}Ab12_9",
            crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
        ));
        std::fs::write(&near_miss, b"foreign tombstone near-miss").expect("foreign entry");

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("an existing exact quarantine must not bypass foreign-entry denial");
        assert!(error.to_string().contains("foreign entry"));
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);
        assert_eq!(
            std::fs::read(&quarantine_path).expect("existing quarantine"),
            corrupt
        );
        assert!(near_miss.exists());
    }

    #[test]
    fn corrupt_intent_quarantine_tombstone_oversize_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-with-oversized-removal-tombstone";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        let directory = quarantine_path.parent().expect("quarantine directory");
        std::fs::create_dir_all(directory).expect("quarantine directory");
        let tombstone = directory.join(format!(
            "{}Ab12Z9",
            crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
        ));
        std::fs::write(&tombstone, vec![b'x'; MAX_APPEND_INTENT_BYTES as usize + 1])
            .expect("oversized tombstone");

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("oversized exact tombstone must fail closed");
        assert!(error.to_string().contains("foreign entry"));
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);
        assert!(tombstone.exists());
        assert!(!quarantine_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn corrupt_intent_quarantine_tombstone_nonregular_fails_closed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-with-nonregular-removal-tombstone";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        let directory = quarantine_path.parent().expect("quarantine directory");
        std::fs::create_dir_all(directory).expect("quarantine directory");
        let tombstone = directory.join(format!(
            "{}Ab12Z9",
            crate::storage::DURABLE_REMOVAL_TOMBSTONE_PREFIX
        ));
        symlink(&intent_path, &tombstone).expect("nonregular tombstone");

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("nonregular exact tombstone must fail closed");
        assert!(error.to_string().contains("foreign entry"));
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);
        assert!(tombstone.exists());
        assert!(!quarantine_path.exists());
    }

    #[tokio::test]
    async fn corrupt_intent_at_clean_eof_is_quarantined_and_publication_continues_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let expected = record("corrupt-intent-clean-eof");
        let publication = stage_run_record_exact(temp.path(), &expected).expect("stage");
        let corrupt = br#"{"schema_version":1,"run_id":false}"#;
        std::fs::write(append_intent_path(temp.path()), corrupt).expect("corrupt intent");

        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &expected.run_id, &publication)
                .await
                .expect("repair and publish"),
            ExactRunPublishOutcome::Published
        );
        assert!(!append_intent_path(temp.path()).exists());
        assert_eq!(
            std::fs::read(corrupt_intent_quarantine_path(temp.path(), corrupt))
                .expect("exact corrupt-intent quarantine"),
            corrupt
        );
        let ledger = std::fs::read_to_string(run_ledger_path(temp.path())).expect("ledger");
        assert_eq!(ledger.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<AutomationRunLedgerRecord>(ledger.trim_end())
                .expect("published exact row"),
            expected
        );
    }

    #[test]
    fn corrupt_intent_repair_rejects_semantically_invalid_committed_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let invalid = serde_json::to_string(&record("corrupt-intent-invalid-committed"))
            .expect("record")
            .replace("\"schema_version\":2", "\"schema_version\":99");
        let ledger_path = run_ledger_path(temp.path());
        std::fs::write(&ledger_path, format!("{invalid}\n")).expect("invalid ledger row");
        let corrupt = b"corrupt-intent-invalid-committed-history";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let ledger_before = std::fs::read(&ledger_path).expect("ledger before");

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("invalid committed history must block intent repair");

        assert!(error.to_string().contains("schema version 99"));
        assert_eq!(std::fs::read(&ledger_path).expect("ledger"), ledger_before);
        assert_eq!(std::fs::read(&intent_path).expect("intent"), corrupt);
    }

    #[test]
    fn corrupt_intent_repair_rejects_semantically_invalid_spool_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let staged = record("corrupt-intent-invalid-spool");
        let publication = stage_run_record_exact(temp.path(), &staged).expect("stage row");
        let spool = spool_path(temp.path(), &staged.run_id, &publication).expect("spool path");
        let invalid = std::fs::read_to_string(&spool)
            .expect("valid spool")
            .replace("\"completed_at\":\"2\"", "\"completed_at\":\"-1\"");
        std::fs::write(&spool, &invalid).expect("corrupt spool semantics");
        let ledger_path = run_ledger_path(temp.path());
        std::fs::write(&ledger_path, &invalid.as_bytes()[..invalid.len() / 2])
            .expect("partial ledger suffix");
        let corrupt = b"corrupt-intent-invalid-spool-authority";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");
        let ledger_before = std::fs::read(&ledger_path).expect("ledger before");

        let error = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("invalid spool must not authorize truncation");

        assert!(error.to_string().contains("predates the UNIX epoch"));
        assert_eq!(std::fs::read(&ledger_path).expect("ledger"), ledger_before);
        assert_eq!(std::fs::read(&intent_path).expect("intent"), corrupt);
        assert!(spool.exists());
    }

    #[test]
    fn visible_quarantine_is_write_through_republished_before_intent_clear() {
        let temp = tempfile::tempdir().expect("tempdir");
        let corrupt = b"corrupt-intent-visible-after-rename";
        let intent_path = append_intent_path(temp.path());
        std::fs::write(&intent_path, corrupt).expect("corrupt intent");

        let first = repair_corrupt_run_ledger_append_intent_with(
            temp.path(),
            |path, bytes| {
                publish_corrupt_append_intent_quarantine_with_publisher(
                    path,
                    bytes,
                    &|temporary, destination, label| {
                        assert_eq!(label, "automation run corrupt append-intent quarantine");
                        std::fs::rename(temporary, destination)?;
                        Err(std::io::Error::other(
                            "injected uncertainty after visible quarantine rename",
                        ))
                    },
                )
            },
            |_, _| panic!("fresh quarantine must not use existing-file recovery"),
            remove_corrupt_append_intent_quarantine_debris,
        );
        assert!(
            first
                .expect_err("visible publish uncertainty must remain an error")
                .to_string()
                .contains("visible quarantine rename")
        );
        let quarantine_path = corrupt_intent_quarantine_path(temp.path(), corrupt);
        assert_eq!(
            std::fs::read(&quarantine_path).expect("visible quarantine"),
            corrupt
        );
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);

        let republish_failure = repair_corrupt_run_ledger_append_intent_with(
            temp.path(),
            |_, _| panic!("visible quarantine must be reopened"),
            |_, _| Err(config_error("injected write-through republish failure")),
            remove_corrupt_append_intent_quarantine_debris,
        );
        assert!(
            republish_failure
                .expect_err("failed redurability must retain the active intent")
                .to_string()
                .contains("write-through republish failure")
        );
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);

        let mut conflicting = corrupt.to_vec();
        conflicting[0] ^= 1;
        std::fs::write(&quarantine_path, &conflicting).expect("conflicting quarantine");
        let readback = repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect_err("exact readback must reject conflicting quarantine bytes");
        assert!(readback.to_string().contains("digest identity"));
        assert_eq!(std::fs::read(&intent_path).expect("active intent"), corrupt);

        std::fs::write(&quarantine_path, corrupt).expect("restore exact quarantine");
        repair_corrupt_run_ledger_append_intent_blocking(temp.path())
            .expect("retry redurabilizes exact quarantine before repair");
        assert!(!intent_path.exists());
        assert_eq!(
            std::fs::read(&quarantine_path).expect("durable exact quarantine"),
            corrupt
        );
    }

    #[tokio::test]
    async fn corrupt_intent_unique_spool_prefix_is_truncated_then_published_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prior = record("corrupt-intent-prior-row");
        super::super::append_run_record(temp.path(), &prior)
            .await
            .expect("prior row");
        let expected = record("corrupt-intent-unique-prefix");
        let publication = stage_run_record_exact(temp.path(), &expected).expect("stage");
        let spool = spool_path(temp.path(), &expected.run_id, &publication).expect("spool");
        let payload = std::fs::read(&spool).expect("spool payload");
        let ledger_path = run_ledger_path(temp.path());
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger_path)
            .expect("ledger append fixture");
        ledger
            .write_all(&payload[..payload.len() / 2])
            .expect("owned partial suffix");
        drop(ledger);
        let corrupt = b"not-json-append-intent";
        std::fs::write(append_intent_path(temp.path()), corrupt).expect("corrupt intent");

        assert_eq!(
            publish_staged_run_record_exact(temp.path(), &expected.run_id, &publication)
                .await
                .expect("repair and publish"),
            ExactRunPublishOutcome::Published
        );
        let rows = std::fs::read_to_string(&ledger_path)
            .expect("repaired ledger")
            .lines()
            .map(|line| {
                serde_json::from_str::<AutomationRunLedgerRecord>(line).expect("canonical row")
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![prior, expected]);
        assert!(!append_intent_path(temp.path()).exists());
        assert_eq!(
            std::fs::read(corrupt_intent_quarantine_path(temp.path(), corrupt))
                .expect("exact corrupt-intent quarantine"),
            corrupt
        );
    }

    #[tokio::test]
    async fn corrupt_intent_ambiguous_or_foreign_suffix_preserves_authoritative_bytes() {
        let ambiguous = tempfile::tempdir().expect("ambiguous tempdir");
        let first = record("corrupt-intent-ambiguous-first");
        let first_publication = stage_run_record_exact(ambiguous.path(), &first).expect("first");
        let second = record("corrupt-intent-ambiguous-second");
        stage_run_record_exact(ambiguous.path(), &second).expect("second");
        let ambiguous_ledger = run_ledger_path(ambiguous.path());
        std::fs::write(&ambiguous_ledger, b"{").expect("ambiguous prefix");
        let corrupt = b"ambiguous-corrupt-intent";
        std::fs::write(append_intent_path(ambiguous.path()), corrupt).expect("corrupt intent");
        let ledger_before = std::fs::read(&ambiguous_ledger).expect("ledger before");
        let intent_before = std::fs::read(append_intent_path(ambiguous.path())).expect("intent");

        let error =
            publish_staged_run_record_exact(ambiguous.path(), &first.run_id, &first_publication)
                .await
                .expect_err("ambiguous spool ownership must fail closed");
        assert!(error.to_string().contains("ambiguous exact spool"));
        assert_eq!(
            std::fs::read(&ambiguous_ledger).expect("ledger"),
            ledger_before
        );
        assert_eq!(
            std::fs::read(append_intent_path(ambiguous.path())).expect("intent"),
            intent_before
        );

        let foreign = tempfile::tempdir().expect("foreign tempdir");
        let expected = record("corrupt-intent-foreign-suffix");
        let publication = stage_run_record_exact(foreign.path(), &expected).expect("stage");
        let foreign_ledger = run_ledger_path(foreign.path());
        std::fs::write(&foreign_ledger, b"foreign-partial-tail").expect("foreign tail");
        std::fs::write(append_intent_path(foreign.path()), corrupt).expect("corrupt intent");
        let ledger_before = std::fs::read(&foreign_ledger).expect("ledger before");
        let intent_before = std::fs::read(append_intent_path(foreign.path())).expect("intent");
        let error = publish_staged_run_record_exact(foreign.path(), &expected.run_id, &publication)
            .await
            .expect_err("foreign suffix must fail closed");
        assert!(error.to_string().contains("not owned"));
        assert_eq!(
            std::fs::read(&foreign_ledger).expect("ledger"),
            ledger_before
        );
        assert_eq!(
            std::fs::read(append_intent_path(foreign.path())).expect("intent"),
            intent_before
        );
    }

    #[tokio::test]
    async fn ordinary_append_refuses_an_unresolved_exact_intent_at_clean_eof() {
        let temp = tempfile::tempdir().expect("tempdir");
        let expected = record("intent-blocks-ordinary");
        let publication = stage_run_record_exact(temp.path(), &expected).expect("stage");
        let intent = LedgerAppendIntent {
            schema_version: 1,
            run_id: expected.run_id.clone(),
            pre_append_eof: 0,
            publication,
        };
        write_append_intent(temp.path(), &intent).expect("intent");

        let ordinary = record("ordinary-blocked-by-intent");
        let error = super::super::append_run_record(temp.path(), &ordinary)
            .await
            .expect_err("ordinary append must not cross intent");
        assert!(error.to_string().contains("unresolved exact append intent"));
        assert!(!run_ledger_path(temp.path()).exists());
    }

    #[tokio::test]
    async fn ordinary_append_refuses_to_bury_an_incomplete_exact_tail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = record("ordinary-after-partial");
        let ledger = run_ledger_path(temp.path());
        std::fs::write(&ledger, b"{\"partial\":true}").expect("partial tail");

        let error = super::super::append_run_record(temp.path(), &record)
            .await
            .expect_err("ordinary append must fail closed");
        assert!(error.to_string().contains("incomplete durable tail"));
        assert_eq!(
            std::fs::read(&ledger).expect("preserved partial"),
            b"{\"partial\":true}"
        );
    }
}
