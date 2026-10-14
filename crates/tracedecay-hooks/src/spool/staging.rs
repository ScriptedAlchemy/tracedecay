//! Lock-free staging for hook callbacks that miss spool admission.
//!
//! A hook waits on another writer's spool lock for at most its synchronous
//! budget. When the writer lease or the commit lock stays held past that, the
//! hook stages its envelope in its own synced file beside the records file and
//! takes no lock. The drain imports staged envelopes under the writer lease and
//! removes each file only once its record is durable, so a crash between the
//! two re-imports an exact duplicate the spool deduplicates or the daemon
//! admits as one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_private_fs::framed_log::{atomic_write, read_bounded};

use crate::{
    HookContractError, HookEventEnvelopeV2, HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES,
    NativeContextScoutLifecycleV1,
};

use super::{
    DIRECTORY_POLICY, HookSpoolConfigV1, HookSpoolError, HookSpoolV1, ensure_root,
    remove_spool_member,
};

const STAGED_SUFFIX: &str = ".staged.v1.json";
const MAX_STAGED_BYTES: usize = 4 * MAX_HOOK_PAYLOAD_BYTES;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedEventV1 {
    envelope: HookEventEnvelopeV2,
    native_lifecycle: Option<NativeContextScoutLifecycleV1>,
    binding: HookScopeBindingV1,
}

/// Whether a spool directory entry is a published staged envelope. A staging
/// temporary starts with a dot and never matches.
pub fn hook_spool_staged_name(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(STAGED_SUFFIX)
}

impl HookSpoolV1 {
    /// Durably stage one envelope without taking the writer lease or the
    /// commit lock. The envelope must already be valid for `binding`, which
    /// is stored with it so the import appends exactly what the hook would
    /// have. At most one staging exists per event ID; a redelivery replaces it.
    #[tracing::instrument(name = "hooks.spool.stage", level = "trace", skip_all)]
    pub fn stage(
        root: &Path,
        config: HookSpoolConfigV1,
        envelope: HookEventEnvelopeV2,
        native_lifecycle: Option<NativeContextScoutLifecycleV1>,
        binding: &HookScopeBindingV1,
    ) -> Result<(), HookSpoolError> {
        config.validate()?;
        envelope
            .validate(binding)
            .map_err(HookSpoolError::EnvelopeRejected)?;
        if envelope.producer != config.host {
            return Err(HookSpoolError::EnvelopeRejected(
                HookContractError::BindingMismatch,
            ));
        }
        ensure_root(root)?;
        if staged_paths(root)?.len() >= config.limits.max_host_records as usize {
            return Err(HookSpoolError::SpoolFull);
        }
        let path = root.join(format!(
            "{}{STAGED_SUFFIX}",
            encode_lowercase_hex(&envelope.event_id)
        ));
        let bytes = serde_json::to_vec(&StagedEventV1 {
            envelope,
            native_lifecycle,
            binding: binding.clone(),
        })
        .map_err(|_| HookSpoolError::RecordTooLarge)?;
        if bytes.len() > MAX_STAGED_BYTES {
            return Err(HookSpoolError::RecordTooLarge);
        }
        atomic_write(&path, "staged", &bytes, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
    }

    /// Append every staged envelope, commit, then remove the stagings now
    /// durable in the records file. A staging that can never land (corrupt,
    /// rejected, or naming an event ID a different pending envelope holds) is
    /// dropped; a full spool leaves the rest staged for a later pass.
    #[tracing::instrument(name = "hooks.spool.import_staged", level = "trace", skip_all)]
    pub(crate) fn import_staged(&mut self, now: UtcMicros) -> Result<(), HookSpoolError> {
        let mut settled = Vec::new();
        for path in staged_paths(&self.root)? {
            let staged = match read_bounded(&path, MAX_STAGED_BYTES) {
                Ok(None) => continue,
                Ok(Some(bytes)) => serde_json::from_slice::<StagedEventV1>(&bytes).ok(),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => None,
                Err(_) => return Err(HookSpoolError::Io),
            };
            let Some(staged) = staged else {
                tracing::warn!(staging = %path.display(), "hook spool staging is corrupt; dropped");
                settled.push(path);
                continue;
            };
            match self.append_with_native_lifecycle(
                staged.envelope,
                staged.native_lifecycle,
                &staged.binding,
                now,
            ) {
                Ok(_) => {}
                Err(HookSpoolError::SpoolFull) => break,
                Err(
                    error @ (HookSpoolError::EventIdConflict
                    | HookSpoolError::EnvelopeRejected(_)
                    | HookSpoolError::RecordTooLarge),
                ) => {
                    tracing::warn!(staging = %path.display(), %error, "hook spool staging cannot land; dropped");
                }
                Err(error) => return Err(error),
            }
            settled.push(path);
        }
        if settled.is_empty() {
            return Ok(());
        }
        self.without_lease(now, |_| Ok(()))?;
        settled
            .iter()
            .try_for_each(|path| remove_spool_member(path))
    }
}

pub(super) fn has_staged(root: &Path) -> Result<bool, HookSpoolError> {
    Ok(!staged_paths(root)?.is_empty())
}

// ponytail: a hook killed between creating its staging temporary and renaming
// it leaves the dot-named temporary behind; reclaim it by age if that matters.
fn staged_paths(root: &Path) -> Result<Vec<PathBuf>, HookSpoolError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(HookSpoolError::Io),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| HookSpoolError::Io)?;
        let staged = entry
            .file_name()
            .to_str()
            .is_some_and(hook_spool_staged_name);
        if staged && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}
