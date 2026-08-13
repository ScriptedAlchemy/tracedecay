//! Finalization of shipped proposal history after the main typed terminal.

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use std::ffi::CString;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_agent_hosts::automation::automatic_facts::{
    MAX_SHIPPED_FACT_PROPOSAL_BYTES, ShippedFactProposalDisposition,
    read_shipped_fact_proposal_bytes,
};
use tracedecay_application::{DirectorySyncPolicy, retained_surfaces::AutomationTaskV1};

use crate::errors::{Result, TraceDecayError};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RetirementBinding {
    pub(super) source_digest: String,
    pub(super) archive_name: String,
}

pub(super) struct RetirementPlan {
    pub(super) binding: RetirementBinding,
    source_path: PathBuf,
    source_bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct RetirementClosure {
    source_path: PathBuf,
    source_digest: String,
    capture_path: Option<PathBuf>,
}

impl RetirementClosure {
    pub(super) fn source_digest(&self) -> &str {
        &self.source_digest
    }

    pub(super) fn capture_expected(&self) -> bool {
        self.capture_path.is_some()
    }
}

pub(super) fn classify(
    disposition: ShippedFactProposalDisposition,
) -> Result<RetirementClassification> {
    match disposition {
        ShippedFactProposalDisposition::Absent => Ok(RetirementClassification::Absent),
        ShippedFactProposalDisposition::ResetRequired {
            source_path,
            source_digest,
            reason,
        } => {
            validate_digest(&source_digest)?;
            Ok(RetirementClassification::ResetRequired {
                source_digest,
                detail: format!(
                    "{reason} at '{}'; final-V2 will not approve, import, archive, or delete unresolved shipped proposal state",
                    source_path.display()
                ),
            })
        }
        ShippedFactProposalDisposition::TerminalHistory {
            source_path,
            source_digest,
            source_bytes,
        } => {
            if source_bytes.len() > MAX_SHIPPED_FACT_PROPOSAL_BYTES {
                return Err(contract_error(
                    "live shipped proposal history exceeds its retirement byte bound",
                ));
            }
            require_digest(&source_bytes, &source_digest)?;
            let archive_name = format!(
                "fact_proposals.{}.json",
                canonical_digest_body(&source_digest)?
            );
            Ok(RetirementClassification::Terminal(RetirementPlan {
                binding: RetirementBinding {
                    source_digest,
                    archive_name,
                },
                source_path,
                source_bytes,
            }))
        }
    }
}

pub(super) async fn classify_for_task(
    task: AutomationTaskV1,
    dashboard_root: &Path,
) -> Result<RetirementClassification> {
    if task != AutomationTaskV1::SessionReflector {
        return Ok(RetirementClassification::Absent);
    }
    classify(
        tracedecay_agent_hosts::automation::automatic_facts::inspect_shipped_fact_proposals(
            dashboard_root,
        )
        .await?,
    )
}

pub(super) enum RetirementClassification {
    Absent,
    ResetRequired {
        source_digest: String,
        detail: String,
    },
    Terminal(RetirementPlan),
}

pub(super) fn verify_plan_matches_binding(
    plan: &RetirementPlan,
    binding: &RetirementBinding,
) -> Result<()> {
    if plan.binding == *binding {
        Ok(())
    } else {
        Err(contract_error(
            "live shipped proposal history conflicts with its admitted retirement",
        ))
    }
}

/// Completes only after the caller has durably persisted the main typed
/// zero-effect `AutomationRun` terminal that contains this binding in
/// its admitted input digest.
pub(super) fn finalize_after_terminal(
    dashboard_root: &Path,
    binding: &RetirementBinding,
    live_plan: Option<&RetirementPlan>,
) -> Result<RetirementClosure> {
    finalize_after_terminal_impl(
        dashboard_root,
        binding,
        live_plan,
        publish_archive_bytes,
        capture_exact_source,
    )
}

fn finalize_after_terminal_impl(
    dashboard_root: &Path,
    binding: &RetirementBinding,
    live_plan: Option<&RetirementPlan>,
    publish: impl FnOnce(&Path, &[u8]) -> Result<()>,
    capture_source: impl FnOnce(&Path, &[u8]) -> Result<Option<PathBuf>>,
) -> Result<RetirementClosure> {
    validate_binding(binding)?;
    let source_path = dashboard_root.join("fact_proposals.json");
    let archive_path = archive_path(dashboard_root, binding)?;
    if let Some(captured) = recover_retirement_capture(&source_path, &binding.source_digest)? {
        let archived = read_retirement_bytes(&archive_path, "archive")?.ok_or_else(|| {
            contract_error(
                "captured proposal history cannot retire without its exact durable archive",
            )
        })?;
        require_digest(&archived, &binding.source_digest)?;
        if archived != captured {
            return Err(contract_error(
                "captured proposal history conflicts with its durable archive",
            ));
        }
        let capture_path = capture_path_for_digest(
            source_path
                .parent()
                .ok_or_else(|| contract_error("shipped proposal source has no parent directory"))?,
            &hex::decode(canonical_digest_body(&binding.source_digest)?).map_err(|error| {
                contract_error(format!("retirement digest decode failed: {error}"))
            })?,
        );
        return Ok(RetirementClosure {
            source_path,
            source_digest: binding.source_digest.clone(),
            capture_path: Some(capture_path),
        });
    }
    let (bytes, capture_live_source) = match live_plan {
        Some(plan) => {
            verify_plan_matches_binding(plan, binding)?;
            if plan.source_path != source_path {
                return Err(contract_error(
                    "shipped proposal retirement source escaped dashboard authority",
                ));
            }
            match read_retirement_bytes(&source_path, "source")? {
                Some(bytes) => {
                    require_digest(&bytes, &binding.source_digest)?;
                    if bytes != plan.source_bytes {
                        return Err(contract_error(
                            "live shipped proposal source changed after retirement admission",
                        ));
                    }
                    (bytes, true)
                }
                None => {
                    let bytes = read_retirement_bytes(&archive_path, "archive")?.ok_or_else(|| {
                        contract_error(
                            "admitted proposal history is absent from both live and archive paths",
                        )
                    })?;
                    require_digest(&bytes, &binding.source_digest)?;
                    if bytes != plan.source_bytes {
                        return Err(contract_error(
                            "retirement archive conflicts with the admitted live plan",
                        ));
                    }
                    (bytes, false)
                }
            }
        }
        None => match read_retirement_bytes(&archive_path, "archive")? {
            Some(bytes) => {
                require_digest(&bytes, &binding.source_digest)?;
                let capture_live_source = match read_retirement_bytes(&source_path, "source")? {
                    Some(source) => source == bytes,
                    None => false,
                };
                (bytes, capture_live_source)
            }
            None => {
                let bytes = read_retirement_bytes(&source_path, "source")?.ok_or_else(|| {
                    contract_error(
                        "retired proposal history is absent from both live and archive paths",
                    )
                })?;
                require_digest(&bytes, &binding.source_digest)?;
                (bytes, true)
            }
        },
    };
    publish_archive_with(&archive_path, &bytes, publish)?;
    let capture_path = if capture_live_source {
        capture_source(&source_path, &bytes)?
    } else {
        None
    };
    Ok(RetirementClosure {
        source_path,
        source_digest: binding.source_digest.clone(),
        capture_path,
    })
}

pub(super) fn complete_after_pending_removal(closure: &RetirementClosure) -> Result<()> {
    complete_after_pending_removal_with(closure, |_| Ok(()))
}

fn complete_after_pending_removal_with(
    closure: &RetirementClosure,
    after_retirement: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let Some(capture_path) = closure.capture_path else {
        return Ok(());
    };
    let lock_path = crate::storage::append_lock_path(&closure.source_path);
    crate::storage::reject_symlink_components(&lock_path, "shipped proposal retirement lock")
        .map_err(contract_error)?;
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(|error| {
        contract_error(format!("shipped proposal retirement lock failed: {error}"))
    })?;
    let result = (|| {
        let parent_path = closure.source_path.parent().ok_or_else(|| {
            contract_error("shipped proposal retirement source has no parent directory")
        })?;
        let parent = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|error| {
            contract_error(format!(
                "shipped proposal retirement parent open failed: {error}"
            ))
        })?;
        let digest = canonical_digest_body(&closure.source_digest)?;
        let retired_path = retired_path_for_digest(parent_path, digest);
        let captured = read_retirement_bytes(&capture_path, "captured source")?;
        let retired = read_retirement_bytes(&retired_path, "retired source witness")?;
        match (captured, retired) {
            (Some(captured), None) => {
                require_digest(&captured, &closure.source_digest)?;
                let captured_name = capture_path.file_name().ok_or_else(|| {
                    contract_error("captured shipped proposal source has no filename")
                })?;
                let retired_name = retired_path.file_name().ok_or_else(|| {
                    contract_error("retired shipped proposal witness has no filename")
                })?;
                rename_noreplace(&parent, captured_name, retired_name).map_err(|error| {
                    contract_error(format!(
                        "captured shipped proposal source retirement failed: {error}"
                    ))
                })?;
                tracedecay_application::sync_parent_directory(
                    &closure.source_path,
                    DirectorySyncPolicy::Strict,
                )
                .map_err(contract_error)?;
                after_retirement(&retired_path)?;
                let retired = read_retirement_bytes(&retired_path, "retired source witness")?
                    .ok_or_else(|| {
                        contract_error("retired shipped proposal witness disappeared")
                    })?;
                require_digest(&retired, &closure.source_digest)?;
                if read_retirement_bytes(&capture_path, "captured source")?.is_some() {
                    return Err(contract_error(
                        "captured shipped proposal source remained after retirement",
                    ));
                }
                Ok(())
            }
            (None, Some(retired)) => {
                require_digest(&retired, &closure.source_digest)?;
                Ok(())
            }
            (None, None) => Ok(()),
            (Some(_), Some(_)) => Err(contract_error(
                "captured and retired shipped proposal witnesses both exist",
            )),
        }
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| {
        contract_error(format!(
            "shipped proposal retirement unlock failed: {error}"
        ))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub(super) fn reconcile_orphaned_retirement_capture(dashboard_root: &Path) -> Result<()> {
    let source_path = dashboard_root.join("fact_proposals.json");
    let lock_path = crate::storage::append_lock_path(&source_path);
    crate::storage::reject_symlink_components(&lock_path, "shipped proposal retirement lock")
        .map_err(contract_error)?;
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(|error| {
        contract_error(format!("shipped proposal retirement lock failed: {error}"))
    })?;
    let result = (|| {
        let Some((witness_path, kind)) = orphaned_retirement_witness(dashboard_root)? else {
            return Ok(());
        };
        let digest = retirement_witness_digest(&witness_path)?;
        let archived_path = dashboard_root
            .join("fact_proposals.archive")
            .join(format!("fact_proposals.{digest}.json"));
        let captured = read_retirement_bytes(&witness_path, "orphaned retirement witness")?
            .ok_or_else(|| contract_error("orphaned retirement witness disappeared"))?;
        let archived = read_retirement_bytes(&archived_path, "orphaned capture archive")?
            .ok_or_else(|| contract_error("orphaned retirement capture has no exact archive"))?;
        if captured != archived {
            return Err(contract_error(
                "orphaned retirement capture conflicts with its archive",
            ));
        }
        require_digest(&captured, &format!("sha256:{digest}"))?;
        let retired_path = retired_path_for_digest(dashboard_root, digest);
        if kind == RetirementWitnessKind::Captured {
            let parent = Dir::open_ambient_dir(dashboard_root, ambient_authority()).map_err(
                |error| {
                    contract_error(format!(
                        "shipped proposal retirement parent open failed: {error}"
                    ))
                },
            )?;
            let captured_name = witness_path.file_name().ok_or_else(|| {
                contract_error("orphaned captured source has no filename")
            })?;
            let retired_name = retired_path.file_name().ok_or_else(|| {
                contract_error("orphaned retired witness has no filename")
            })?;
            rename_noreplace(&parent, captured_name, retired_name).map_err(|error| {
                contract_error(format!("orphaned retirement witness move failed: {error}"))
            })?;
            tracedecay_application::sync_parent_directory(
                &source_path,
                DirectorySyncPolicy::Strict,
            )
            .map_err(contract_error)?;
        }
        remove_retired_witness(&retired_path, &source_path)
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| {
        contract_error(format!(
            "shipped proposal retirement unlock failed: {error}"
        ))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetirementWitnessKind {
    Captured,
    Retired,
}

fn orphaned_retirement_witness(
    dashboard_root: &Path,
) -> Result<Option<(PathBuf, RetirementWitnessKind)>> {
    const MAX_RETIREMENT_ROOT_ENTRIES: usize = 1_024;
    let entries = match std::fs::read_dir(dashboard_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(contract_error(format!(
                "retirement capture inventory failed: {error}"
            )));
        }
    };
    let mut witness = None;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_RETIREMENT_ROOT_ENTRIES {
            return Err(contract_error(format!(
                "retirement capture inventory exceeds its {MAX_RETIREMENT_ROOT_ENTRIES}-entry bound"
            )));
        }
        let entry = entry
            .map_err(|error| contract_error(format!("retirement capture entry failed: {error}")))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(".fact_proposals.retirement-") {
            continue;
        }
        let (_, kind) = retirement_witness_digest_name(name)?;
        if witness.replace((entry.path(), kind)).is_some() {
            return Err(contract_error(
                "multiple orphaned retirement witnesses require operator repair",
            ));
        }
    }
    Ok(witness)
}

fn retirement_witness_digest(path: &Path) -> Result<&str> {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| contract_error("retirement capture name is not UTF-8"))?;
    retirement_witness_digest_name(name).map(|(digest, _)| digest)
}

fn capture_digest_name(name: &str) -> Result<&str> {
    retirement_witness_digest_name(name).and_then(|(digest, kind)| {
        if kind == RetirementWitnessKind::Captured {
            Ok(digest)
        } else {
            Err(contract_error("retirement capture name is not canonical"))
        }
    })
}

fn retirement_witness_digest_name(name: &str) -> Result<(&str, RetirementWitnessKind)> {
    let value = name
        .strip_prefix(".fact_proposals.retirement-")
        .ok_or_else(|| contract_error("retirement witness name is not canonical"))?;
    let (digest, kind) = if let Some(digest) = value.strip_suffix(".captured") {
        (digest, RetirementWitnessKind::Captured)
    } else if let Some(digest) = value.strip_suffix(".retired") {
        (digest, RetirementWitnessKind::Retired)
    } else {
        return Err(contract_error(
            "retirement witness name is not canonical",
        ));
    };
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok((digest, kind))
    } else {
        Err(contract_error(
            "retirement capture digest is not canonical SHA-256",
        ))
    }
}

fn remove_retired_witness(retired_path: &Path, source_path: &Path) -> Result<()> {
    let removal = std::fs::remove_file(retired_path);
    let sync = tracedecay_application::sync_parent_directory(
        source_path,
        DirectorySyncPolicy::Strict,
    )
    .map_err(contract_error);
    let absent = read_retirement_bytes(retired_path, "retired source witness")?.is_none();
    if absent && sync.is_ok() {
        return Ok(());
    }
    match (removal, sync, absent) {
        (Err(error), _, false) => Err(contract_error(format!(
            "retired shipped proposal witness removal failed: {error}"
        ))),
        (_, Err(error), _) => Err(error),
        (_, _, false) => Err(contract_error(
            "retired shipped proposal witness remained after removal",
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
fn finalize_after_terminal_with_io(
    dashboard_root: &Path,
    binding: &RetirementBinding,
    live_plan: Option<&RetirementPlan>,
    publish: impl FnOnce(&Path, &[u8]) -> Result<()>,
    remove_source: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let closure = finalize_after_terminal_impl(
        dashboard_root,
        binding,
        live_plan,
        publish,
        |path, bytes| {
            remove_source(path, bytes)?;
            Ok(None)
        },
    )?;
    complete_after_pending_removal(&closure)
}

fn publish_archive_with(
    path: &Path,
    bytes: &[u8],
    publish: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    match read_retirement_bytes(path, "archive")? {
        Some(existing) if existing == bytes => {}
        Some(_) => {
            return Err(contract_error(
                "shipped proposal retirement archive conflicts with admitted bytes",
            ));
        }
        None => {}
    }

    // An exact visible archive may be the commit-uncertain residue of a
    // Windows replacement that surfaced an error after changing the namespace.
    // Republish the admitted bytes through the write-through authority on every
    // retry, then require exact readback before source retirement.
    publish(path, bytes)?;
    match read_retirement_bytes(path, "archive")? {
        Some(existing) if existing == bytes => Ok(()),
        Some(_) => Err(contract_error(
            "shipped proposal retirement archive changed after write-through publication",
        )),
        None => Err(contract_error(
            "shipped proposal retirement archive disappeared after write-through publication",
        )),
    }
}

fn publish_archive_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_archive_bytes_with(path, bytes, replace_archive_file)
}

fn publish_archive_bytes_with(
    path: &Path,
    bytes: &[u8],
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| contract_error("shipped proposal archive has no parent directory"))?;
    crate::storage::PrivateStoreIo::create_dir_all_durable(parent).map_err(|error| {
        contract_error(format!(
            "shipped proposal archive directory creation failed: {error}"
        ))
    })?;
    tracedecay_domain::with_owned_temp_publish(
        path,
        "shipped-proposal-retirement-archive",
        publish,
        |output| output.write_all(bytes),
        DirectorySyncPolicy::Strict,
    )
    .map_err(contract_error)
}

fn replace_archive_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let temporary_file =
            tracedecay_runtime_core::windows_security::make_private_file(temporary)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
    }

    crate::db::DatabaseAuthority::replace_file_atomically(
        temporary,
        destination,
        "shipped proposal retirement archive",
    )
    .map_err(std::io::Error::other)?;

    #[cfg(windows)]
    tracedecay_runtime_core::windows_security::validate_private_file(destination)?;

    Ok(())
}

fn capture_exact_source(path: &Path, expected: &[u8]) -> Result<Option<PathBuf>> {
    capture_exact_source_with_capture(path, expected, |_| Ok(()))
}

#[cfg(test)]
fn remove_exact_source(path: &Path, expected: &[u8]) -> Result<()> {
    complete_after_pending_removal(&RetirementClosure {
        source_path: path.to_path_buf(),
        source_digest: format!("sha256:{}", hex::encode(Sha256::digest(expected))),
        capture_path: capture_exact_source(path, expected)?,
    })
}

#[cfg(test)]
fn remove_exact_source_with_capture(
    path: &Path,
    expected: &[u8],
    after_capture: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    complete_after_pending_removal(&RetirementClosure {
        source_path: path.to_path_buf(),
        source_digest: format!("sha256:{}", hex::encode(Sha256::digest(expected))),
        capture_path: capture_exact_source_with_capture(path, expected, after_capture)?,
    })
}

fn recover_retirement_capture(path: &Path, expected_digest: &str) -> Result<Option<Vec<u8>>> {
    let parent_path = path
        .parent()
        .ok_or_else(|| contract_error("shipped proposal source has no parent directory"))?;
    let expected_raw = hex::decode(canonical_digest_body(expected_digest)?)
        .map_err(|error| contract_error(format!("retirement digest decode failed: {error}")))?;
    let tombstone_path = capture_path_for_digest(parent_path, &expected_raw);
    let Some(captured) = read_retirement_bytes(&tombstone_path, "captured source")? else {
        return Ok(None);
    };
    if require_digest(&captured, expected_digest).is_err() {
        restore_existing_capture(path, &tombstone_path)?;
        return Err(contract_error(
            "captured shipped proposal source changed before retirement replay",
        ));
    }
    Ok(Some(captured))
}

fn restore_existing_capture(source_path: &Path, captured_path: &Path) -> Result<()> {
    let parent_path = source_path
        .parent()
        .ok_or_else(|| contract_error("shipped proposal source has no parent directory"))?;
    let source_name = source_path
        .file_name()
        .ok_or_else(|| contract_error("shipped proposal source has no filename"))?;
    let captured_name = captured_path
        .file_name()
        .ok_or_else(|| contract_error("captured shipped proposal source has no filename"))?;
    let lock_path = crate::storage::append_lock_path(source_path);
    crate::storage::reject_symlink_components(&lock_path, "shipped proposal retirement lock")
        .map_err(contract_error)?;
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(|error| {
        contract_error(format!("shipped proposal retirement lock failed: {error}"))
    })?;
    let result = (|| {
        let parent = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|error| {
            contract_error(format!(
                "shipped proposal retirement parent open failed: {error}"
            ))
        })?;
        restore_captured_source(&parent, captured_name, source_name, source_path)
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| {
        contract_error(format!(
            "shipped proposal retirement unlock failed: {error}"
        ))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn capture_exact_source_with_capture(
    path: &Path,
    expected: &[u8],
    after_capture: impl FnOnce(&Path) -> Result<()>,
) -> Result<Option<PathBuf>> {
    let parent_path = path
        .parent()
        .ok_or_else(|| contract_error("shipped proposal source has no parent directory"))?;
    let source_name = path
        .file_name()
        .ok_or_else(|| contract_error("shipped proposal source has no filename"))?;
    let lock_path = crate::storage::append_lock_path(path);
    crate::storage::reject_symlink_components(&lock_path, "shipped proposal retirement lock")
        .map_err(contract_error)?;
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(|error| {
        contract_error(format!("shipped proposal retirement lock failed: {error}"))
    })?;
    let result = (|| {
        let parent = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|error| {
            contract_error(format!(
                "shipped proposal retirement parent open failed: {error}"
            ))
        })?;
        let (tombstone_name, tombstone_path) = capture_path(parent_path, expected);
        if let Some(captured) = read_retirement_bytes(&tombstone_path, "captured source")? {
            if captured == expected {
                return Ok(Some(tombstone_path));
            }
            restore_captured_source(&parent, &tombstone_name, source_name, path)?;
            return Err(contract_error(
                "captured shipped proposal source changed before typed retirement capture",
            ));
        }
        match rename_noreplace(&parent, source_name, &tombstone_name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracedecay_application::sync_parent_directory(path, DirectorySyncPolicy::Strict)
                    .map_err(contract_error)?;
                return Ok(None);
            }
            Err(error) => {
                return Err(contract_error(format!(
                    "shipped proposal source capture failed: {error}"
                )));
            }
        }
        tracedecay_application::sync_parent_directory(path, DirectorySyncPolicy::Strict)
            .map_err(contract_error)?;
        after_capture(&tombstone_path)?;
        let captured = match read_retirement_bytes(&tombstone_path, "captured source") {
            Ok(Some(captured)) => captured,
            Ok(None) => {
                return Err(contract_error(
                    "captured shipped proposal source disappeared",
                ));
            }
            Err(error) => {
                restore_captured_source(&parent, &tombstone_name, source_name, path)?;
                return Err(error);
            }
        };
        if captured != expected {
            restore_captured_source(&parent, &tombstone_name, source_name, path)?;
            return Err(contract_error(
                "captured shipped proposal source changed after typed retirement terminal",
            ));
        }
        Ok(Some(tombstone_path))
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| {
        contract_error(format!(
            "shipped proposal retirement unlock failed: {error}"
        ))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn capture_path(parent: &Path, expected: &[u8]) -> (std::ffi::OsString, PathBuf) {
    let digest = Sha256::digest(expected);
    let path = capture_path_for_digest(parent, &digest);
    let name = std::ffi::OsString::from(format!(
        ".fact_proposals.retirement-{}.captured",
        hex::encode(digest)
    ));
    (name, path)
}

fn capture_path_for_digest(parent: &Path, digest: &[u8]) -> PathBuf {
    parent.join(format!(
        ".fact_proposals.retirement-{}.captured",
        hex::encode(digest)
    ))
}

fn retired_path_for_digest(parent: &Path, digest: &str) -> PathBuf {
    parent.join(format!(
        ".fact_proposals.retirement-{digest}.retired"
    ))
}

fn restore_captured_source(
    parent: &Dir,
    captured: &OsStr,
    source: &OsStr,
    source_path: &Path,
) -> Result<()> {
    rename_noreplace(parent, captured, source).map_err(|error| {
        contract_error(format!(
            "captured shipped proposal source conflicts with a replacement and could not be restored: {error}"
        ))
    })?;
    tracedecay_application::sync_parent_directory(source_path, DirectorySyncPolicy::Strict)
        .map_err(contract_error)
}

/// Atomically renames sibling paths without replacing an occupied destination.
/// Platforms without a suitable primitive fail closed.
fn rename_noreplace(parent: &Dir, from: &OsStr, to: &OsStr) -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both names are single-component C strings and both dirfds
        // reference the same already-open retirement parent capability.
        let result = unsafe {
            libc::renameat2(
                parent.as_raw_fd(),
                from.as_ptr(),
                parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        return if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both names are single-component C strings and RENAME_EXCL
        // refuses an occupied destination.
        let result = unsafe {
            libc::renameatx_np(
                parent.as_raw_fd(),
                from.as_ptr(),
                parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        return if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::AsRawHandle;

        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        const FILE_NAME_NORMALIZED: u32 = 0;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
            fn GetFinalPathNameByHandleW(
                file: *mut std::ffi::c_void,
                file_path: *mut u16,
                file_path_length: u32,
                flags: u32,
            ) -> u32;
        }

        let parent_path = windows_parent_path(|buffer, length| unsafe {
            GetFinalPathNameByHandleW(parent.as_raw_handle(), buffer, length, FILE_NAME_NORMALIZED)
        })?;
        let from = parent_path
            .join(from)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let to = parent_path
            .join(to)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        return crate::storage::retry_transient_file_op(|| {
            // SAFETY: both UTF-16 strings are NUL-terminated. Omitting
            // MOVEFILE_REPLACE_EXISTING makes an occupied destination fail.
            let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
            if moved != 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        windows
    )))]
    {
        let _ = (parent, from, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace retirement capture is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
fn windows_parent_path(get_path: impl Fn(*mut u16, u32) -> u32) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    let required = get_path(std::ptr::null_mut(), 0);
    if required == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let capacity = required.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "retirement parent path length overflowed",
        )
    })?;
    let mut buffer = vec![0_u16; capacity as usize];
    let written = get_path(buffer.as_mut_ptr(), buffer.len() as u32);
    if written == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if written as usize >= buffer.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "retirement parent path changed while resolving its capability",
        ));
    }
    buffer.truncate(written as usize);
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
}

fn read_retirement_bytes(path: &Path, label: &str) -> Result<Option<Vec<u8>>> {
    read_shipped_fact_proposal_bytes(path).map_err(|error| {
        contract_error(format!(
            "shipped proposal {label} bounded private read failed: {error}"
        ))
    })
}

fn archive_path(dashboard_root: &Path, binding: &RetirementBinding) -> Result<PathBuf> {
    if binding.archive_name.contains('/') || binding.archive_name.contains('\\') {
        return Err(contract_error("retirement archive name is not a basename"));
    }
    Ok(dashboard_root
        .join("fact_proposals.archive")
        .join(&binding.archive_name))
}

fn validate_binding(binding: &RetirementBinding) -> Result<()> {
    let raw = canonical_digest_body(&binding.source_digest)?;
    if binding.archive_name != format!("fact_proposals.{raw}.json") {
        return Err(contract_error(
            "retirement archive basename is not digest-derived",
        ));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<()> {
    canonical_digest_body(digest).map(|_| ())
}

fn canonical_digest_body(digest: &str) -> Result<&str> {
    let raw = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| contract_error("retirement digest is not canonical SHA-256"))?;
    if raw.len() == 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(raw)
    } else {
        Err(contract_error("retirement digest is not canonical SHA-256"))
    }
}

fn require_digest(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    if actual == expected {
        Ok(())
    } else {
        Err(contract_error(
            "shipped proposal bytes do not match admitted retirement digest",
        ))
    }
}

fn contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("shipped proposal retirement is invalid: {error}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn terminal_shipped_sidecar() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "proposals": [
                {
                    "schema_version": 1,
                    "proposal_id": "fact_0123456789abcdef",
                    "run_id": "run-shipped-sidecar",
                    "evidence_hash": "shipped-evidence-hash",
                    "state": "applied",
                    "proposal": {
                        "content": "Preserve shipped proposal provenance",
                        "source_span": {"message_id": "msg-shipped"}
                    },
                    "validation": {"status": "accepted"},
                    "applied_fact_id": 42,
                    "apply_outcome": {"state": "applied", "fact_id": 42},
                    "created_at": 1_700_000_000,
                    "updated_at": 1_700_000_001,
                    "duplicate_count": 2,
                    "last_duplicate_run_id": "run-shipped-duplicate",
                    "folded_contents": ["Earlier wording"]
                },
                {
                    "schema_version": 1,
                    "proposal_id": "fact_fedcba9876543210",
                    "run_id": "run-shipped-sidecar",
                    "state": "rejected",
                    "proposal": {"content": "Transient rejected item"},
                    "validation_reason": "not durable",
                    "reviewer": "validator",
                    "created_at": 1_700_000_002,
                    "updated_at": 1_700_000_003
                }
            ]
        })
    }

    fn plan(root: &Path, bytes: &[u8]) -> RetirementPlan {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        RetirementPlan {
            binding: RetirementBinding {
                source_digest: digest.clone(),
                archive_name: format!(
                    "fact_proposals.{}.json",
                    digest.strip_prefix("sha256:").unwrap()
                ),
            },
            source_path: root.join("fact_proposals.json"),
            source_bytes: bytes.to_vec(),
        }
    }

    fn write_private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(windows)]
        drop(tracedecay_runtime_core::windows_security::make_private_file(path).unwrap());
    }

    #[test]
    fn retirement_plan_rejects_unbounded_terminal_bytes() {
        let root = tempfile::tempdir().unwrap();
        let source_bytes = vec![b'x'; MAX_SHIPPED_FACT_PROPOSAL_BYTES + 1];
        let source_digest = format!("sha256:{}", hex::encode(Sha256::digest(&source_bytes)));

        let error = match classify(ShippedFactProposalDisposition::TerminalHistory {
            source_path: root.path().join("fact_proposals.json"),
            source_digest,
            source_bytes,
        }) {
            Err(error) => error,
            Ok(_) => panic!("an unbounded disposition must not become a live retirement plan"),
        };

        assert!(error.to_string().contains("retirement byte bound"));
    }

    #[test]
    fn retirement_binding_rejects_repeated_sha256_prefix() {
        let raw = "a".repeat(64);
        let binding = RetirementBinding {
            source_digest: format!("sha256:sha256:{raw}"),
            archive_name: format!("fact_proposals.{raw}.json"),
        };

        let error = validate_binding(&binding)
            .expect_err("a canonical retirement digest has exactly one algorithm prefix");

        assert!(error.to_string().contains("canonical SHA-256"));
    }

    #[test]
    fn typed_terminal_finalizer_archives_then_removes_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        write_private_file(&plan.source_path, bytes);

        complete_after_pending_removal(
            &finalize_after_terminal(root.path(), &plan.binding, Some(&plan)).unwrap(),
        )
        .unwrap();

        assert!(!plan.source_path.exists());
        assert_eq!(
            std::fs::read(archive_path(root.path(), &plan.binding).unwrap()).unwrap(),
            bytes
        );
    }

    #[test]
    fn archive_uncertainty_retains_source_until_exact_republication_readback() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        write_private_file(&plan.source_path, bytes);

        let visible_error = finalize_after_terminal_with_io(
            root.path(),
            &plan.binding,
            Some(&plan),
            |path, payload| {
                publish_archive_bytes(path, payload)?;
                Err(contract_error(
                    "injected uncertainty after visible archive publication",
                ))
            },
            |_, _| panic!("uncertain archive publication must retain the source"),
        )
        .expect_err("visible archive publication uncertainty must surface");
        assert!(visible_error.to_string().contains("injected uncertainty"));
        assert_eq!(std::fs::read(&plan.source_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&archive).unwrap(), bytes);

        let redurability_attempted = std::cell::Cell::new(false);
        let redurability_error = finalize_after_terminal_with_io(
            root.path(),
            &plan.binding,
            Some(&plan),
            |path, payload| {
                redurability_attempted.set(true);
                assert_eq!(std::fs::read(path).unwrap(), payload);
                Err(contract_error(
                    "injected exact archive redurability failure",
                ))
            },
            |_, _| panic!("failed exact redurability must retain the source"),
        )
        .expect_err("an exact visible archive still requires write-through republication");
        assert!(
            redurability_error
                .to_string()
                .contains("redurability failure")
        );
        assert!(redurability_attempted.get());
        assert_eq!(std::fs::read(&plan.source_path).unwrap(), bytes);

        let removal_attempted = std::cell::Cell::new(false);
        let readback_error = finalize_after_terminal_with_io(
            root.path(),
            &plan.binding,
            Some(&plan),
            |path, payload| {
                publish_archive_bytes(path, payload)?;
                crate::storage::PrivateStoreIo::remove_file_durable(path)
                    .map(|_| ())
                    .map_err(contract_error)
            },
            |_, _| {
                removal_attempted.set(true);
                Ok(())
            },
        )
        .expect_err("archive disappearance after publication must fail exact readback");
        assert!(readback_error.to_string().contains("disappeared"));
        assert!(!removal_attempted.get());
        assert_eq!(std::fs::read(&plan.source_path).unwrap(), bytes);

        finalize_after_terminal_with_io(
            root.path(),
            &plan.binding,
            Some(&plan),
            publish_archive_bytes,
            |path, expected| {
                assert_eq!(std::fs::read(&archive).unwrap(), bytes);
                remove_exact_source(path, expected)
            },
        )
        .unwrap();
        assert!(!plan.source_path.exists());
        assert_eq!(std::fs::read(&archive).unwrap(), bytes);
    }

    #[cfg(windows)]
    #[test]
    fn production_archive_publisher_creates_a_private_windows_file() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();

        publish_archive_bytes(&archive, bytes).unwrap();

        tracedecay_runtime_core::windows_security::validate_private_file(&archive).unwrap();
        assert_eq!(std::fs::read(archive).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn archive_publisher_stages_a_mode_0600_unix_temp_file() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        let observed_mode = std::cell::Cell::new(None);

        publish_archive_bytes_with(&archive, bytes, |temporary, destination| {
            observed_mode.set(Some(
                std::fs::metadata(temporary)?.permissions().mode() & 0o777,
            ));
            replace_archive_file(temporary, destination)
        })
        .unwrap();

        assert_eq!(observed_mode.get(), Some(0o600));
        assert_eq!(std::fs::read(archive).unwrap(), bytes);
    }

    #[test]
    fn replay_recovers_when_source_was_removed_after_archive() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        publish_archive_bytes(&archive, bytes).unwrap();

        complete_after_pending_removal(
            &finalize_after_terminal(root.path(), &plan.binding, None).unwrap(),
        )
        .unwrap();

        assert!(!plan.source_path.exists());
        assert_eq!(std::fs::read(&archive).unwrap(), bytes);
    }

    #[test]
    fn atomic_capture_never_deletes_a_post_capture_replacement() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);

        remove_exact_source_with_capture(&plan.source_path, admitted, |captured| {
            assert_eq!(
                read_retirement_bytes(captured, "test capture")?,
                Some(admitted.to_vec())
            );
            write_private_file(&plan.source_path, replacement);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );
        let captured = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".fact_proposals.retirement-")
            })
            .count();
        assert_eq!(captured, 0);
    }

    #[test]
    fn exact_captured_source_is_recovered_after_commit_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);

        let error = remove_exact_source_with_capture(&plan.source_path, admitted, |_| {
            Err(contract_error(
                "injected uncertainty after durable source capture",
            ))
        })
        .expect_err("capture uncertainty must remain visible");
        assert!(error.to_string().contains("injected uncertainty"));
        assert!(!plan.source_path.exists());

        remove_exact_source(&plan.source_path, admitted).unwrap();

        assert!(!plan.source_path.exists());
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".fact_proposals.retirement-")
                })
                .count(),
            0
        );
    }

    #[test]
    fn exact_capture_recovery_preserves_a_post_capture_replacement() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);

        remove_exact_source_with_capture(&plan.source_path, admitted, |_| {
            write_private_file(&plan.source_path, replacement);
            Err(contract_error(
                "injected crash after replacement followed durable capture",
            ))
        })
        .expect_err("the simulated crash leaves exact captured bytes for retry");

        publish_archive_bytes(&archive_path(root.path(), &plan.binding).unwrap(), admitted)
            .unwrap();
        complete_after_pending_removal(
            &finalize_after_terminal(root.path(), &plan.binding, Some(&plan)).unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );
        assert_eq!(retirement_capture_count(root.path()), 0);
    }

    #[test]
    fn pending_cleanup_boundary_retains_witness_and_retry_preserves_replacement() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);

        let uncompleted = finalize_after_terminal(root.path(), &plan.binding, Some(&plan)).unwrap();
        assert!(!plan.source_path.exists());
        assert_eq!(retirement_capture_count(root.path()), 1);

        write_private_file(&plan.source_path, replacement);
        let recovered = finalize_after_terminal(root.path(), &plan.binding, None).unwrap();
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );
        assert_eq!(retirement_capture_count(root.path()), 1);

        drop(uncompleted);
        complete_after_pending_removal(&recovered).unwrap();
        assert_eq!(retirement_capture_count(root.path()), 0);
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );
    }

    #[test]
    fn durable_archive_replay_still_captures_the_exact_live_source() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);
        publish_archive_bytes(&archive_path(root.path(), &plan.binding).unwrap(), admitted)
            .unwrap();

        let closure = finalize_after_terminal(root.path(), &plan.binding, None).unwrap();

        assert!(!plan.source_path.exists());
        assert_eq!(retirement_capture_count(root.path()), 1);
        complete_after_pending_removal(&closure).unwrap();
        assert_eq!(retirement_capture_count(root.path()), 0);
        assert!(!plan.source_path.exists());
    }

    #[test]
    fn orphaned_capture_requires_an_exact_archive_and_preserves_replacement_bytes() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        write_private_file(&captured_path, admitted);
        write_private_file(&plan.source_path, replacement);

        let missing = reconcile_orphaned_retirement_capture(root.path())
            .expect_err("an orphaned capture without its archive must fail closed");
        assert!(missing.to_string().contains("no exact archive"));
        assert_eq!(
            read_retirement_bytes(&captured_path, "capture").unwrap(),
            Some(admitted.to_vec())
        );
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );

        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        write_private_file(&archive, replacement);
        let mismatch = reconcile_orphaned_retirement_capture(root.path())
            .expect_err("an orphaned capture with a mismatched archive must fail closed");
        assert!(mismatch.to_string().contains("conflicts with its archive"));
        assert_eq!(
            read_retirement_bytes(&captured_path, "capture").unwrap(),
            Some(admitted.to_vec())
        );
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );

        write_private_file(&archive, admitted);
        reconcile_orphaned_retirement_capture(root.path()).unwrap();
        assert!(!captured_path.exists());
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "replacement").unwrap(),
            Some(replacement.to_vec())
        );
    }

    #[test]
    fn malformed_orphaned_capture_name_is_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let malformed = root
            .path()
            .join(".fact_proposals.retirement-not-a-digest.captured");
        write_private_file(&malformed, b"captured");

        let error = reconcile_orphaned_retirement_capture(root.path())
            .expect_err("a malformed capture name must fail closed");

        assert!(error.to_string().contains("canonical SHA-256"));
        assert_eq!(std::fs::read(&malformed).unwrap(), b"captured");
    }

    #[test]
    fn mismatched_capture_recovery_restores_the_captured_replacement() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        write_private_file(&captured_path, replacement);

        let error = remove_exact_source(&plan.source_path, admitted)
            .expect_err("a captured replacement must be restored, never deleted");

        assert!(
            error
                .to_string()
                .contains("changed before typed retirement capture")
        );
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "restored replacement").unwrap(),
            Some(replacement.to_vec())
        );
        assert_eq!(retirement_capture_count(root.path()), 0);
    }

    #[test]
    fn exact_capture_without_archive_stays_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        write_private_file(&captured_path, admitted);

        let error = finalize_after_terminal(root.path(), &plan.binding, Some(&plan))
            .expect_err("captured bytes alone cannot prove an exact durable archive");

        assert!(error.to_string().contains("exact durable archive"));
        assert_eq!(retirement_capture_count(root.path()), 1);
        assert_eq!(
            read_retirement_bytes(&captured_path, "retained capture").unwrap(),
            Some(admitted.to_vec())
        );
        assert!(!plan.source_path.exists());
    }

    #[test]
    fn finalizer_replay_restores_a_mismatched_capture_to_an_absent_source() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let replacement = br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        write_private_file(&captured_path, replacement);

        let error = finalize_after_terminal(root.path(), &plan.binding, Some(&plan))
            .expect_err("replay must restore a mismatched capture before reporting conflict");

        assert!(
            error
                .to_string()
                .contains("changed before retirement replay")
        );
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "restored replacement").unwrap(),
            Some(replacement.to_vec())
        );
        assert_eq!(retirement_capture_count(root.path()), 0);
    }

    #[test]
    fn finalizer_replay_never_replaces_a_recreated_source() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let captured_replacement =
            br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let recreated_source = br#"{"schema_version":1,"proposals":[{"state":"applied"}]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        write_private_file(&captured_path, captured_replacement);
        write_private_file(&plan.source_path, recreated_source);

        let error = finalize_after_terminal(root.path(), &plan.binding, Some(&plan))
            .expect_err("replay must not replace an occupied source destination");

        assert!(error.to_string().contains("conflicts with a replacement"));
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "recreated source").unwrap(),
            Some(recreated_source.to_vec())
        );
        assert_eq!(
            read_retirement_bytes(&captured_path, "retained captured replacement").unwrap(),
            Some(captured_replacement.to_vec())
        );
    }

    #[test]
    fn occupied_digest_tombstone_blocks_capture_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let admitted = br#"{"schema_version":1,"proposals":[]}"#;
        let tombstone_blocker =
            br#"{"schema_version":1,"proposals":[{"state":"pending_approval"}]}"#;
        let plan = plan(root.path(), admitted);
        let (_, captured_path) = capture_path(root.path(), admitted);
        write_private_file(&plan.source_path, admitted);
        write_private_file(&captured_path, tombstone_blocker);

        let error = remove_exact_source(&plan.source_path, admitted)
            .expect_err("an occupied digest tombstone must never be replaced");

        assert!(error.to_string().contains("conflicts with a replacement"));
        assert_eq!(
            read_retirement_bytes(&plan.source_path, "admitted source").unwrap(),
            Some(admitted.to_vec())
        );
        assert_eq!(
            read_retirement_bytes(&captured_path, "tombstone blocker").unwrap(),
            Some(tombstone_blocker.to_vec())
        );
    }

    fn retirement_capture_count(root: &Path) -> usize {
        std::fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".fact_proposals.retirement-")
            })
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn live_retirement_rejects_a_symlink_source_before_archive_publication() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let target = root.path().join("outside-source.json");
        write_private_file(&target, bytes);
        symlink(&target, &plan.source_path).unwrap();
        let archive = archive_path(root.path(), &plan.binding).unwrap();

        let error = finalize_after_terminal(root.path(), &plan.binding, Some(&plan))
            .expect_err("a changed source symlink must fail before retirement publication");

        assert!(error.to_string().contains("bounded private read failed"));
        assert!(!archive.exists());
        assert_eq!(std::fs::read(target).unwrap(), bytes);
        assert!(
            std::fs::symlink_metadata(&plan.source_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn retirement_replay_rejects_a_symlink_archive_without_reading_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let target = root.path().join("outside-archive.json");
        write_private_file(&target, bytes);
        symlink(&target, &archive).unwrap();

        let error = finalize_after_terminal(root.path(), &plan.binding, None)
            .expect_err("retirement replay must never follow an archive symlink");

        assert!(error.to_string().contains("bounded private read failed"));
        assert_eq!(std::fs::read(target).unwrap(), bytes);
        assert!(
            std::fs::symlink_metadata(archive)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn retirement_replay_rejects_an_oversized_sparse_archive_before_digest() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        write_private_file(&archive, b"");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&archive)
            .unwrap()
            .set_len(MAX_SHIPPED_FACT_PROPOSAL_BYTES as u64 + 1)
            .unwrap();

        let error = finalize_after_terminal(root.path(), &plan.binding, None)
            .expect_err("an oversized replay archive must fail before digest or publication");

        assert!(error.to_string().contains("byte limit"));
        assert_eq!(
            std::fs::metadata(archive).unwrap().len(),
            MAX_SHIPPED_FACT_PROPOSAL_BYTES as u64 + 1
        );
    }

    #[test]
    fn changed_source_is_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let original = br#"{"schema_version":1,"proposals":[]}"#;
        let changed = br#"{"schema_version":1,"proposals":[{}]}"#;
        let plan = plan(root.path(), original);
        write_private_file(&plan.source_path, changed);

        assert!(finalize_after_terminal(root.path(), &plan.binding, None).is_err());
        assert_eq!(std::fs::read(&plan.source_path).unwrap(), changed);
    }

    #[tokio::test]
    async fn memory_curator_leaves_terminal_sidecar_for_session_reflector_retirement() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("fact_proposals.json");
        let source_bytes = serde_json::to_vec_pretty(&terminal_shipped_sidecar()).unwrap();
        write_private_file(&source_path, &source_bytes);

        let curator = classify_for_task(
            tracedecay_application::retained_surfaces::AutomationTaskV1::MemoryCurator,
            root.path(),
        )
        .await
        .unwrap();
        assert!(matches!(curator, RetirementClassification::Absent));
        assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);

        let reflector = classify_for_task(
            tracedecay_application::retained_surfaces::AutomationTaskV1::SessionReflector,
            root.path(),
        )
        .await
        .unwrap();
        assert!(matches!(reflector, RetirementClassification::Terminal(_)));
        assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    }
}
