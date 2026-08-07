//! Bounded freshness coverage for repositories refused at the watcher
//! capacity caps.
//!
//! A `Capacity` admission is a typed refusal, not an excuse to strand the
//! repository: the roster retains its resolved identity so the backstop can
//! keep submitting freshness requests through the scheduler ingress and
//! re-attempt in-memory admission once a slot frees. The roster itself is
//! bounded; saturation is a typed, logged state rather than silent loss.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::Instant;
use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

use super::{GitWatcher, GitWatcherAdmission, log_daemon_event};
use crate::config::SyncConfig;

/// Upper bound on capacity-refused repositories retained for coverage.
pub(super) const MAX_OVERFLOW_ROOTS: usize = 64;

/// Typed outcome of admitting one capacity-refused repository to the roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OverflowAdmission {
    /// Retained: the backstop covers this root until a slot frees.
    Covered,
    /// Already retained; nothing changed.
    AlreadyCovered,
    /// The roster is at its bound. The repository has NO coverage until the
    /// next handshake — a truthful degraded state, logged by the caller.
    RosterFull,
}

pub(super) struct OverflowEntry {
    pub(super) identity: GitRepositoryIdentity,
    pub(super) config: SyncConfig,
    pub(super) due: Instant,
}

#[derive(Default)]
pub(super) struct OverflowRoster {
    entries: BTreeMap<PathBuf, OverflowEntry>,
}

impl OverflowRoster {
    pub(super) fn admit(
        &mut self,
        identity: GitRepositoryIdentity,
        config: SyncConfig,
        now: Instant,
    ) -> OverflowAdmission {
        self.admit_bounded(identity, config, now, MAX_OVERFLOW_ROOTS)
    }

    /// Bound-parameterized admission so the saturation contract is testable
    /// without constructing `MAX_OVERFLOW_ROOTS` identities.
    pub(super) fn admit_bounded(
        &mut self,
        identity: GitRepositoryIdentity,
        config: SyncConfig,
        now: Instant,
        bound: usize,
    ) -> OverflowAdmission {
        let root = identity.worktree_root.clone();
        if self.entries.contains_key(&root) {
            return OverflowAdmission::AlreadyCovered;
        }
        if self.entries.len() >= bound {
            return OverflowAdmission::RosterFull;
        }
        let due = next_due(now, &config);
        self.entries.insert(
            root,
            OverflowEntry {
                identity,
                config,
                due,
            },
        );
        OverflowAdmission::Covered
    }

    pub(super) fn remove(&mut self, root: &Path) {
        self.entries.remove(root);
    }

    pub(super) fn contains(&self, root: &Path) -> bool {
        self.entries.contains_key(root)
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries whose backstop interval has elapsed; each taken entry's next
    /// due instant is advanced before it is returned so a slow pass never
    /// double-schedules.
    pub(super) fn take_due(
        &mut self,
        now: Instant,
    ) -> Vec<(PathBuf, GitRepositoryIdentity, SyncConfig)> {
        let mut due = Vec::new();
        for (root, entry) in &mut self.entries {
            if now < entry.due {
                continue;
            }
            entry.due = next_due(now, &entry.config);
            due.push((root.clone(), entry.identity.clone(), entry.config.clone()));
        }
        due
    }
}

/// The next coverage instant for one entry: the same per-root backstop
/// interval registered roots use. A zero interval disables the backstop and
/// therefore overflow coverage with it.
fn next_due(now: Instant, config: &SyncConfig) -> Instant {
    let mins = config.backstop_interval_mins.max(1);
    now + Duration::from_secs(mins.saturating_mul(60))
}

impl GitWatcher {
    /// Retains a capacity-refused repository on the bounded overflow roster,
    /// logging the typed outcome. Saturation is truthful: the repository gets
    /// no coverage until the next handshake.
    pub(super) fn cover_capacity_overflow(
        &self,
        identity: GitRepositoryIdentity,
        config: SyncConfig,
    ) {
        let root = identity.worktree_root.clone();
        let admission = self
            .inner
            .overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admit(identity, config, Instant::now());
        match admission {
            OverflowAdmission::Covered => {
                log_daemon_event(
                    "git_watch_overflow_covered",
                    &[("project", root.display().to_string())],
                );
            }
            OverflowAdmission::AlreadyCovered => {}
            OverflowAdmission::RosterFull => {
                log_daemon_event(
                    "git_watch_overflow_saturated",
                    &[
                        ("project", root.display().to_string()),
                        ("roster_bound", MAX_OVERFLOW_ROOTS.to_string()),
                    ],
                );
            }
        }
    }
}

/// One backstop pass over the overflow roster: re-attempt in-memory admission
/// (a slot may have freed), and while still refused keep the repository on
/// the scheduler-ingress freshness floor.
pub(super) async fn cover_overflowed_repositories(watcher: &GitWatcher) {
    let due = {
        let mut roster = watcher
            .inner
            .overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        roster.take_due(Instant::now())
    };
    for (root, identity, config) in due {
        if watcher.inner.cancellation.is_cancelled() {
            return;
        }
        match watcher.admit_resolved(identity.clone(), &config).await {
            GitWatcherAdmission::Ready => {
                watcher
                    .inner
                    .overflow
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&root);
                log_daemon_event(
                    "git_watch_overflow_recovered",
                    &[("project", root.display().to_string())],
                );
            }
            GitWatcherAdmission::Capacity => {
                if let Some(schedulers) = watcher.inner.code_index_schedulers.as_ref() {
                    // Accepted or Busy both leave the entry covered; a Busy
                    // registry is retried on the next due pass. Unmounted and
                    // IdentityMismatch mean the scheduler is not serving this
                    // root — coverage stays typed on the roster, and the next
                    // real handshake re-resolves identity.
                    let _ = schedulers.request_for_root(&identity);
                }
                log_daemon_event(
                    "git_watch_backstop",
                    &[
                        ("project", root.display().to_string()),
                        ("action", "backstop_overflow".to_string()),
                    ],
                );
            }
            GitWatcherAdmission::NotRepository | GitWatcherAdmission::Disabled => {
                watcher
                    .inner
                    .overflow
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&root);
            }
            GitWatcherAdmission::ShuttingDown => return,
            // admit_resolved performs no discovery, so it can never report an
            // unavailable identity; keep the entry covered if it ever does.
            GitWatcherAdmission::IdentityUnavailable => {}
        }
    }
}
