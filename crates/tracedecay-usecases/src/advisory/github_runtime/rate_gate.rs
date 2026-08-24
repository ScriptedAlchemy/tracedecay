//! Pre-flight rate-limit gate for bursts of GitHub reads.
//!
//! A CI discovery pass issues up to twenty paged requests, then more for jobs,
//! check runs, and annotations. Without a gate the first eighteen succeed, the
//! nineteenth returns `403`, and the caller is left holding a half-populated
//! result it must throw away - having already spent the quota that produced it.
//!
//! This module keeps the last `x-ratelimit-*` checkpoint each client observed
//! and answers, before the burst starts, whether the planned request count
//! fits. It costs no provider request of its own: the observation is a byproduct
//! of responses the client already received. Nothing here is a credential, so
//! the ledger holds only counters and a reset instant.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;
use tracedecay_domain::UtcMicros;

/// Requests held back from any single burst so a concurrent read - a review
/// resume, a release page - is not starved by one CI discovery pass.
const GITHUB_RATE_LIMIT_RESERVE_V1: u32 = 5;
/// Bound on tracked repositories. The ledger is advisory: dropping the oldest
/// observations only returns the gate to [`GitHubRateLimitAdmissionV1::Unknown`].
const MAX_TRACKED_RATE_LIMIT_SCOPES_V1: usize = 512;

/// Exact scope one observation belongs to.
///
/// The credential generation is part of the key so an observation made under
/// one credential never gates a burst issued under another. Generation `0` is
/// the anonymous credential.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GitHubRateLimitScopeV1 {
    credential_generation: u64,
    repository_owner: String,
    repository_name: String,
}

/// Verdict returned before a burst is issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubRateLimitAdmissionV1 {
    /// Nothing usable has been observed for this scope, or the observed window
    /// has already reset. The burst proceeds unbounded.
    Unknown,
    /// The whole planned burst fits inside the observed remaining quota.
    Admitted { remaining: u32 },
    /// Some quota remains, but less than planned. The caller must issue at most
    /// `admitted_requests` requests.
    Degraded {
        admitted_requests: u32,
        checkpoint: GitHubReviewRateLimitCheckpointV1,
    },
    /// No usable quota remains before `checkpoint.reset_at`. The caller must
    /// issue no request at all.
    Exhausted {
        checkpoint: GitHubReviewRateLimitCheckpointV1,
    },
}

impl GitHubRateLimitAdmissionV1 {
    /// How many requests of the plan may actually be issued.
    #[must_use]
    pub fn admitted_requests(&self, planned_requests: u32) -> u32 {
        match self {
            Self::Unknown => planned_requests,
            Self::Admitted { .. } => planned_requests,
            Self::Degraded {
                admitted_requests, ..
            } => (*admitted_requests).min(planned_requests),
            Self::Exhausted { .. } => 0,
        }
    }

    /// The checkpoint proving a refusal or degradation, when there is one.
    #[must_use]
    pub fn checkpoint(&self) -> Option<&GitHubReviewRateLimitCheckpointV1> {
        match self {
            Self::Unknown | Self::Admitted { .. } => None,
            Self::Degraded { checkpoint, .. } | Self::Exhausted { checkpoint } => Some(checkpoint),
        }
    }
}

type GitHubRateLimitLedgerV1 = BTreeMap<GitHubRateLimitScopeV1, GitHubReviewRateLimitCheckpointV1>;

fn github_rate_limit_ledger_v1() -> &'static Mutex<GitHubRateLimitLedgerV1> {
    static LEDGER: OnceLock<Mutex<GitHubRateLimitLedgerV1>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn scope_v1(
    credential_generation: u64,
    repository_owner: &str,
    repository_name: &str,
) -> GitHubRateLimitScopeV1 {
    GitHubRateLimitScopeV1 {
        credential_generation,
        repository_owner: repository_owner.to_owned(),
        repository_name: repository_name.to_owned(),
    }
}

/// Retains one observed checkpoint. A malformed checkpoint is discarded rather
/// than allowed to gate a later burst.
pub(super) fn record_github_rate_limit_checkpoint_v1(
    credential_generation: u64,
    repository_owner: &str,
    repository_name: &str,
    checkpoint: &GitHubReviewRateLimitCheckpointV1,
) {
    if checkpoint.validate().is_err() {
        return;
    }
    let Ok(mut ledger) = github_rate_limit_ledger_v1().lock() else {
        return;
    };
    let scope = scope_v1(credential_generation, repository_owner, repository_name);
    if ledger.len() >= MAX_TRACKED_RATE_LIMIT_SCOPES_V1 && !ledger.contains_key(&scope) {
        ledger.clear();
    }
    ledger.insert(scope, checkpoint.clone());
}

/// Answers whether `planned_requests` may be issued for this scope right now.
///
/// A poisoned lock, an absent observation, or an already-reset window all
/// return [`GitHubRateLimitAdmissionV1::Unknown`]: the gate never invents a
/// refusal it cannot prove.
pub(super) fn admit_github_request_burst_v1(
    credential_generation: u64,
    repository_owner: &str,
    repository_name: &str,
    planned_requests: u32,
    now: UtcMicros,
) -> GitHubRateLimitAdmissionV1 {
    let Ok(mut ledger) = github_rate_limit_ledger_v1().lock() else {
        return GitHubRateLimitAdmissionV1::Unknown;
    };
    let scope = scope_v1(credential_generation, repository_owner, repository_name);
    let Some(checkpoint) = ledger.get(&scope).cloned() else {
        return GitHubRateLimitAdmissionV1::Unknown;
    };
    // The observed window has already refreshed, so the observation proves
    // nothing about the current one.
    if checkpoint.reset_at.0 <= now.0 {
        ledger.remove(&scope);
        return GitHubRateLimitAdmissionV1::Unknown;
    }
    drop(ledger);
    let usable = checkpoint.remaining.saturating_sub(GITHUB_RATE_LIMIT_RESERVE_V1);
    if usable == 0 {
        return GitHubRateLimitAdmissionV1::Exhausted { checkpoint };
    }
    if usable >= planned_requests {
        return GitHubRateLimitAdmissionV1::Admitted {
            remaining: checkpoint.remaining,
        };
    }
    GitHubRateLimitAdmissionV1::Degraded {
        admitted_requests: usable,
        checkpoint,
    }
}

/// Drops every observation for one scope.
#[cfg(any(test, feature = "test-transport"))]
pub(super) fn forget_github_rate_limit_scope_v1(
    credential_generation: u64,
    repository_owner: &str,
    repository_name: &str,
) {
    let Ok(mut ledger) = github_rate_limit_ledger_v1().lock() else {
        return;
    };
    ledger.remove(&scope_v1(
        credential_generation,
        repository_owner,
        repository_name,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(remaining: u32, reset_at: i64) -> GitHubReviewRateLimitCheckpointV1 {
        GitHubReviewRateLimitCheckpointV1 {
            limit: 5_000,
            remaining,
            reset_at: UtcMicros(reset_at),
        }
    }

    #[test]
    fn an_unobserved_scope_never_refuses_a_burst() {
        forget_github_rate_limit_scope_v1(0, "octo", "unobserved");
        assert_eq!(
            admit_github_request_burst_v1(0, "octo", "unobserved", 20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unknown
        );
    }

    #[test]
    fn an_exhausted_window_refuses_the_burst_before_any_request() {
        forget_github_rate_limit_scope_v1(0, "octo", "exhausted");
        record_github_rate_limit_checkpoint_v1(0, "octo", "exhausted", &checkpoint(0, 9_000));
        let admission =
            admit_github_request_burst_v1(0, "octo", "exhausted", 20, UtcMicros(1_000));
        assert!(matches!(
            admission,
            GitHubRateLimitAdmissionV1::Exhausted { .. }
        ));
        assert_eq!(
            admission.admitted_requests(20),
            0,
            "an exhausted window must admit no request at all"
        );
        forget_github_rate_limit_scope_v1(0, "octo", "exhausted");
    }

    #[test]
    fn a_reserve_sized_remainder_still_refuses_the_burst() {
        forget_github_rate_limit_scope_v1(0, "octo", "reserve");
        record_github_rate_limit_checkpoint_v1(
            0,
            "octo",
            "reserve",
            &checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1, 9_000),
        );
        assert_eq!(
            admit_github_request_burst_v1(0, "octo", "reserve", 20, UtcMicros(1_000))
                .admitted_requests(20),
            0,
            "the reserve must stay available to concurrent reads"
        );
        forget_github_rate_limit_scope_v1(0, "octo", "reserve");
    }

    #[test]
    fn a_partial_remainder_degrades_the_burst_instead_of_refusing_it() {
        forget_github_rate_limit_scope_v1(0, "octo", "partial");
        record_github_rate_limit_checkpoint_v1(
            0,
            "octo",
            "partial",
            &checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 3, 9_000),
        );
        let admission = admit_github_request_burst_v1(0, "octo", "partial", 20, UtcMicros(1_000));
        assert!(matches!(
            admission,
            GitHubRateLimitAdmissionV1::Degraded {
                admitted_requests: 3,
                ..
            }
        ));
        assert_eq!(admission.admitted_requests(20), 3);
        forget_github_rate_limit_scope_v1(0, "octo", "partial");
    }

    #[test]
    fn an_ample_remainder_admits_the_whole_burst() {
        forget_github_rate_limit_scope_v1(0, "octo", "ample");
        record_github_rate_limit_checkpoint_v1(0, "octo", "ample", &checkpoint(4_900, 9_000));
        let admission = admit_github_request_burst_v1(0, "octo", "ample", 20, UtcMicros(1_000));
        assert_eq!(
            admission,
            GitHubRateLimitAdmissionV1::Admitted { remaining: 4_900 }
        );
        assert_eq!(admission.admitted_requests(20), 20);
        forget_github_rate_limit_scope_v1(0, "octo", "ample");
    }

    #[test]
    fn an_already_reset_window_stops_gating() {
        forget_github_rate_limit_scope_v1(0, "octo", "reset");
        record_github_rate_limit_checkpoint_v1(0, "octo", "reset", &checkpoint(0, 1_000));
        assert_eq!(
            admit_github_request_burst_v1(0, "octo", "reset", 20, UtcMicros(2_000)),
            GitHubRateLimitAdmissionV1::Unknown,
            "a window that already reset must not gate the next burst"
        );
    }

    #[test]
    fn one_credential_observation_never_gates_another_credentials_burst() {
        forget_github_rate_limit_scope_v1(1, "octo", "isolated");
        forget_github_rate_limit_scope_v1(2, "octo", "isolated");
        record_github_rate_limit_checkpoint_v1(1, "octo", "isolated", &checkpoint(0, 9_000));
        assert!(matches!(
            admit_github_request_burst_v1(1, "octo", "isolated", 20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Exhausted { .. }
        ));
        assert_eq!(
            admit_github_request_burst_v1(2, "octo", "isolated", 20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unknown,
            "an observation under one credential must not gate another"
        );
        forget_github_rate_limit_scope_v1(1, "octo", "isolated");
    }

    #[test]
    fn a_malformed_checkpoint_is_never_retained() {
        forget_github_rate_limit_scope_v1(0, "octo", "malformed");
        let malformed = GitHubReviewRateLimitCheckpointV1 {
            limit: 10,
            remaining: 11,
            reset_at: UtcMicros(9_000),
        };
        assert!(malformed.validate().is_err());
        record_github_rate_limit_checkpoint_v1(0, "octo", "malformed", &malformed);
        assert_eq!(
            admit_github_request_burst_v1(0, "octo", "malformed", 20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unknown
        );
    }
}
