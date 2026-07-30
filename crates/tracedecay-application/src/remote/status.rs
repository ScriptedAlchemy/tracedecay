//! Canonical operational truth for the Remote Brain production surface.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};

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

fn invalid_status() -> ApplicationProblem {
    ApplicationProblem::Unavailable {
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
