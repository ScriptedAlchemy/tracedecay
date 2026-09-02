use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tracedecay_application::RequestId;
use tracedecay_application::remote::recovery::{
    RecoveryAuthorityExpectationV1, RemoteRecoveryInterruptionV1, StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoveryPhysicalCommitV1, RemoteRecoveryPhysicalEffectErrorV1,
};
use tracedecay_store::{
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1,
};

use super::artifacts::safe_digest_suffix;
use super::{interruption_value, safe_suffix};

pub(super) fn validate_recovery_artifact_file(
    artifact_root: &Path,
    path: &Path,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let canonical_root = std::fs::canonicalize(artifact_root)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    let canonical_path = std::fs::canonicalize(path)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(())
}

pub(super) fn committed_restore(
    request: &StagedRestoreConfirmationV1,
    policy_digest: ManifestDigest,
    bytes_consumed: u64,
    interruption: Option<RemoteRecoveryInterruptionV1>,
) -> Result<
    RemoteRecoveryPhysicalCommitV1<StagedRestoreProgressV1>,
    RemoteRecoveryPhysicalEffectErrorV1,
> {
    let receipt_id = format!("remote.restore.{}", safe_suffix(&request.preview_id)?);
    let output = StagedRestoreProgressV1::Published { receipt_id };
    Ok(RemoteRecoveryPhysicalCommitV1 {
        committed_state_digest: canonical_sha256(&output)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        output,
        policy_digest,
        committed_at: tracedecay_application::clock::now_micros(),
        units_consumed: 1,
        bytes_consumed,
        interruption_observed_after_commit: interruption,
    })
}

pub(super) struct RecoveryRuntimeProbeV1 {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
    commit_started: AtomicBool,
}

impl RecoveryRuntimeProbeV1 {
    pub(super) fn new(
        request_id: &RequestId,
        interruption: Arc<AtomicU8>,
    ) -> Result<Self, RemoteRecoveryPhysicalEffectErrorV1> {
        let digest = canonical_sha256(&("tracedecay.remote-recovery-control.v1", request_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let suffix = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        Ok(Self {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
            },
            interruption,
            commit_started: AtomicBool::new(false),
        })
    }
}

impl RuntimeRequestProbeV1 for RecoveryRuntimeProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match interruption_value(&self.interruption) {
            Some(RemoteRecoveryInterruptionV1::Cancelled) => Some(RuntimeInterruptionV1::Cancelled),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded) => {
                Some(RuntimeInterruptionV1::DeadlineExceeded)
            }
            None => None,
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

pub(super) fn authority_key(
    expected: &RecoveryAuthorityExpectationV1,
) -> Result<ManifestDigest, RemoteRecoveryPhysicalEffectErrorV1> {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &expected.brain_id,
        &expected.shard_id,
        &expected.generation_id,
    ))
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn backup_id(
    operation_id: &str,
    expected: &RecoveryAuthorityExpectationV1,
) -> Result<String, RemoteRecoveryPhysicalEffectErrorV1> {
    let digest = canonical_sha256(&("tracedecay.remote-backup.v1", operation_id, expected))
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    Ok(format!("remote.backup.{}", safe_digest_suffix(&digest)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_artifact_identity_rejects_path_escape_and_symlink_aliases() {
        assert_eq!(
            super::super::safe_suffix("../outside"),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );
        assert_eq!(
            super::super::safe_suffix("/absolute"),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );

        let temporary = tempfile::tempdir().unwrap();
        let artifact = temporary.path().join("remote.backup.fixture.sqlite3");
        std::fs::write(&artifact, b"fixture").unwrap();
        assert_eq!(
            validate_recovery_artifact_file(temporary.path(), &artifact),
            Ok(())
        );

        let nested = temporary.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let escaped = nested.join("remote.backup.fixture.sqlite3");
        std::fs::write(&escaped, b"fixture").unwrap();
        assert_eq!(
            validate_recovery_artifact_file(temporary.path(), &escaped),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );

        #[cfg(unix)]
        {
            let alias = temporary.path().join("remote.backup.alias.sqlite3");
            std::os::unix::fs::symlink(&artifact, &alias).unwrap();
            assert_eq!(
                validate_recovery_artifact_file(temporary.path(), &alias),
                Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
            );
        }
    }
}
