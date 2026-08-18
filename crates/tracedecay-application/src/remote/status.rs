//! Canonical operational truth for the Remote Brain production surface.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};

use crate::doctor::{
    DoctorCoverageCompletenessV1, RemoteAuthorityReadV1, RemoteListenerReadV1,
    RemoteOperationalReadV1,
};
use crate::{ApplicationProblem, LegalAction, RetryDirective, SafeDiagnostic};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperationalReadinessV1 {
    Unconfigured,
    Partial,
    Ready,
    RecoveryRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteSpoolOperationalStatusV1 {
    pub pending_count: u64,
    pub quarantined_count: u64,
    pub has_sequence_gap: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationalStatusV1 {
    pub readiness: RemoteOperationalReadinessV1,
    pub enrollment_configured: bool,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub spool: RemoteSpoolOperationalStatusV1,
    pub replay_coverage_complete: bool,
    pub current_backup_verified: bool,
    pub failover_in_progress: bool,
    pub recovery_required: bool,
    pub observed_at: UtcMicros,
}

impl RemoteOperationalStatusV1 {
    /// Composes the canonical operational status from directly observed
    /// authority evidence, deriving the one readiness value that satisfies
    /// [`Self::validate`].
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        enrollment_configured: bool,
        authority: CurrentRemoteAuthorityStateV1,
        spool: RemoteSpoolOperationalStatusV1,
        replay_coverage_complete: bool,
        current_backup_verified: bool,
        failover_in_progress: bool,
        recovery_required: bool,
        observed_at: UtcMicros,
    ) -> Result<Self, ApplicationProblem> {
        let ready = enrollment_configured
            && matches!(authority, CurrentRemoteAuthorityStateV1::Available(_))
            && spool.quarantined_count == 0
            && !spool.has_sequence_gap
            && replay_coverage_complete
            && current_backup_verified
            && !failover_in_progress
            && !recovery_required;
        let readiness = if recovery_required {
            RemoteOperationalReadinessV1::RecoveryRequired
        } else if ready {
            RemoteOperationalReadinessV1::Ready
        } else if !enrollment_configured {
            RemoteOperationalReadinessV1::Unconfigured
        } else {
            RemoteOperationalReadinessV1::Partial
        };
        let status = Self {
            readiness,
            enrollment_configured,
            authority,
            spool,
            replay_coverage_complete,
            current_backup_verified,
            failover_in_progress,
            recovery_required,
            observed_at,
        };
        status.validate()?;
        Ok(status)
    }

    pub fn validate(&self) -> Result<(), ApplicationProblem> {
        self.authority.validate().map_err(|_| invalid_status())?;
        let ready = self.enrollment_configured
            && matches!(self.authority, CurrentRemoteAuthorityStateV1::Available(_))
            && self.spool.quarantined_count == 0
            && !self.spool.has_sequence_gap
            && self.replay_coverage_complete
            && self.current_backup_verified
            && !self.failover_in_progress
            && !self.recovery_required;
        if (self.readiness == RemoteOperationalReadinessV1::Ready) != ready
            || (self.readiness == RemoteOperationalReadinessV1::RecoveryRequired)
                != self.recovery_required
        {
            return Err(invalid_status());
        }
        Ok(())
    }
}

/// Typed read of the Remote Brain operational plane as observed from the
/// mounted daemon authorities. Every operator surface (Doctor, CLI, MCP,
/// dashboard) reads this one shape; `Unavailable` is reserved for a genuinely
/// unmounted or unreadable authority, never a rendering shortcut.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RemoteOperationalStatusReadV1 {
    Observed {
        listener: RemoteListenerReadV1,
        status: RemoteOperationalStatusV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    Unconfigured,
    Unavailable,
}

impl RemoteOperationalStatusReadV1 {
    /// Projects the Doctor operational read from the same observation, so the
    /// Doctor plane and the richer operator surfaces cannot disagree.
    pub fn doctor_read(&self) -> RemoteOperationalReadV1 {
        match self {
            Self::Observed {
                listener,
                status,
                coverage,
            } => RemoteOperationalReadV1::Observed {
                listener: *listener,
                authority: match &status.authority {
                    CurrentRemoteAuthorityStateV1::Available(_) => RemoteAuthorityReadV1::Available,
                    CurrentRemoteAuthorityStateV1::Partial { .. } => RemoteAuthorityReadV1::Partial,
                    CurrentRemoteAuthorityStateV1::Unavailable { .. } => {
                        RemoteAuthorityReadV1::Unavailable
                    }
                },
                pending_spool_items: status.spool.pending_count,
                quarantined_spool_items: status.spool.quarantined_count,
                replay_coverage_complete: status.replay_coverage_complete,
                backup_verified: status.current_backup_verified,
                failover_in_progress: status.failover_in_progress,
                recovery_required: status.recovery_required,
                coverage: *coverage,
            },
            Self::Unconfigured => RemoteOperationalReadV1::Unconfigured,
            Self::Unavailable => RemoteOperationalReadV1::Unavailable,
        }
    }
}

fn invalid_status() -> ApplicationProblem {
    ApplicationProblem::Unavailable {
        classification: crate::ApplicationUnavailableClassV1::Authority,
        diagnostic: SafeDiagnostic::new(
            "remote_operational_status_invalid",
            "Remote operational readiness could not be verified.",
        )
        .expect("static Remote operational status diagnostic is valid"),
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh, LegalAction::Reconcile],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, UtcMicros,
    };

    use super::*;

    fn available_authority() -> CurrentRemoteAuthorityStateV1 {
        serde_json::from_value(serde_json::json!({
            "state": "available",
            "value": {
                "fence": {
                    "brain_id": "brain.status",
                    "shard_id": "shard.status",
                    "generation_id": "generation.status",
                    "placement_revision": 1,
                    "authority_epoch": 1,
                    "authority_node_id": "node.authority"
                },
                "credential_revision": 1,
                "observed_at": 10
            }
        }))
        .unwrap()
    }

    #[test]
    fn compose_derives_the_one_valid_readiness() {
        let clean_spool = RemoteSpoolOperationalStatusV1 {
            pending_count: 0,
            quarantined_count: 0,
            has_sequence_gap: false,
        };
        let ready = RemoteOperationalStatusV1::compose(
            true,
            available_authority(),
            clean_spool.clone(),
            true,
            true,
            false,
            false,
            UtcMicros(10),
        )
        .unwrap();
        assert_eq!(ready.readiness, RemoteOperationalReadinessV1::Ready);

        let recovery = RemoteOperationalStatusV1::compose(
            true,
            available_authority(),
            RemoteSpoolOperationalStatusV1 {
                pending_count: 1,
                quarantined_count: 2,
                has_sequence_gap: false,
            },
            false,
            false,
            false,
            true,
            UtcMicros(10),
        )
        .unwrap();
        assert_eq!(
            recovery.readiness,
            RemoteOperationalReadinessV1::RecoveryRequired
        );

        let unconfigured = RemoteOperationalStatusV1::compose(
            false,
            CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
                observed_at: UtcMicros(10),
            },
            clean_spool,
            true,
            false,
            false,
            false,
            UtcMicros(10),
        )
        .unwrap();
        assert_eq!(
            unconfigured.readiness,
            RemoteOperationalReadinessV1::Unconfigured
        );
    }

    #[test]
    fn doctor_read_projects_the_same_observation() {
        let status = RemoteOperationalStatusV1::compose(
            true,
            available_authority(),
            RemoteSpoolOperationalStatusV1 {
                pending_count: 3,
                quarantined_count: 0,
                has_sequence_gap: false,
            },
            false,
            true,
            false,
            false,
            UtcMicros(10),
        )
        .unwrap();
        let read = RemoteOperationalStatusReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            status,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
        assert_eq!(
            read.doctor_read(),
            RemoteOperationalReadV1::Observed {
                listener: RemoteListenerReadV1::Serving,
                authority: RemoteAuthorityReadV1::Available,
                pending_spool_items: 3,
                quarantined_spool_items: 0,
                replay_coverage_complete: false,
                backup_verified: true,
                failover_in_progress: false,
                recovery_required: false,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        assert_eq!(
            RemoteOperationalStatusReadV1::Unavailable.doctor_read(),
            RemoteOperationalReadV1::Unavailable
        );
        assert_eq!(
            RemoteOperationalStatusReadV1::Unconfigured.doctor_read(),
            RemoteOperationalReadV1::Unconfigured
        );
    }

    #[test]
    fn unavailable_authority_cannot_render_ready() {
        let status = RemoteOperationalStatusV1 {
            readiness: RemoteOperationalReadinessV1::Ready,
            enrollment_configured: true,
            authority: CurrentRemoteAuthorityStateV1::Partial {
                known_fence: None,
                missing: BTreeSet::from([RemoteAuthorityUnavailableReasonV1::FenceUnverified]),
                observed_at: UtcMicros(10),
            },
            spool: RemoteSpoolOperationalStatusV1 {
                pending_count: 0,
                quarantined_count: 0,
                has_sequence_gap: false,
            },
            replay_coverage_complete: true,
            current_backup_verified: true,
            failover_in_progress: false,
            recovery_required: false,
            observed_at: UtcMicros(10),
        };
        assert!(status.validate().is_err());
    }
}
