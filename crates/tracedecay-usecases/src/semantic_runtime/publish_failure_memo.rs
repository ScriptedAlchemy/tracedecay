//! Terminal semantic-publish failure memo: a scheduling guard, not a store.
//!
//! Every published code generation schedules a full semantic projection when no
//! compatible vector generation can be restored. When that projection's
//! publication fails for a reason that is a property of the
//! (projection key, corpus-size) pair rather than of the individual attempt —
//! a whole-corpus commit exceeding the runtime request limit is the live
//! example — the next generation re-embeds the same corpus inside the shared
//! reservation and fails identically, forever, at full corpus cost.
//!
//! This memo remembers that outcome and suppresses the reschedule under
//! exponential backoff. It never decides correctness: suppression only skips
//! work already proven to fail, and anything that could change the outcome
//! drops the entry so the next generation schedules normally —
//!
//! - a different projection key (model/profile/schema change) — part of the key,
//! - a different corpus-size class (the corpus grew or shrank materially) —
//!   part of the key,
//! - a changed witness (crate version bump, store root migration, resource
//!   ceiling reconfiguration),
//! - an observed successful publication,
//! - elapsed backoff, which always re-admits eventually.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracedecay_domain::ProjectionKeyV1;

use crate::config::SemanticResourceCeilings;

/// First suppression window after a terminal publish failure.
pub const DEFAULT_PUBLISH_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(60);
/// Longest suppression window; backoff never grows past this.
pub const DEFAULT_PUBLISH_FAILURE_BACKOFF_CEILING: Duration = Duration::from_secs(30 * 60);

/// Identity of a projection attempt whose publish outcome is reproducible.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SemanticPublishFailureKeyV1 {
    pub projection_key: ProjectionKeyV1,
    pub corpus_size_class: u32,
}

impl SemanticPublishFailureKeyV1 {
    pub fn new(projection_key: ProjectionKeyV1, corpus_chunks: usize) -> Self {
        Self {
            projection_key,
            corpus_size_class: corpus_size_class(corpus_chunks),
        }
    }
}

/// Power-of-two bucket of a corpus chunk count.
///
/// A corpus that crossed into another class is a materially different commit
/// (the failing constraint is a byte ceiling on one whole-corpus document), so
/// it is admitted once rather than inheriting the memo of a different size.
#[must_use]
pub fn corpus_size_class(corpus_chunks: usize) -> u32 {
    usize::BITS - corpus_chunks.leading_zeros()
}

/// Witness of everything outside the key that could change the publish outcome.
#[must_use]
pub fn publish_failure_witness(store_root: &Path, resources: &SemanticResourceCeilings) -> String {
    format!(
        "version={};store_root={};ceilings={:?}",
        env!("CARGO_PKG_VERSION"),
        store_root.display(),
        resources
    )
}

/// Why a schedule was refused, for the operator-facing warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressedSemanticPublishV1 {
    pub reason: String,
    pub failures: u32,
    pub retry_after: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticPublishAdmissionV1 {
    Admitted,
    Suppressed(SuppressedSemanticPublishV1),
}

#[derive(Debug)]
struct MemoEntryV1 {
    witness: String,
    reason: String,
    failures: u32,
    retry_at: Instant,
}

/// Process-wide memo of terminal semantic publish failures.
#[derive(Debug)]
pub struct SemanticPublishFailureMemoV1 {
    base: Duration,
    ceiling: Duration,
    entries: Mutex<HashMap<SemanticPublishFailureKeyV1, MemoEntryV1>>,
}

impl Default for SemanticPublishFailureMemoV1 {
    fn default() -> Self {
        Self::new(
            DEFAULT_PUBLISH_FAILURE_BACKOFF_BASE,
            DEFAULT_PUBLISH_FAILURE_BACKOFF_CEILING,
        )
    }
}

impl SemanticPublishFailureMemoV1 {
    #[must_use]
    pub fn new(base: Duration, ceiling: Duration) -> Self {
        Self {
            base,
            ceiling,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn backoff(&self, failures: u32) -> Duration {
        let steps = failures.saturating_sub(1).min(32);
        self.base
            .saturating_mul(1_u32.checked_shl(steps).unwrap_or(u32::MAX))
            .min(self.ceiling)
    }

    /// Decide whether a projection for `key` may be scheduled now.
    pub fn admit(
        &self,
        key: &SemanticPublishFailureKeyV1,
        witness: &str,
    ) -> SemanticPublishAdmissionV1 {
        self.admit_at(key, witness, Instant::now())
    }

    pub fn admit_at(
        &self,
        key: &SemanticPublishFailureKeyV1,
        witness: &str,
        now: Instant,
    ) -> SemanticPublishAdmissionV1 {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = entries.get(key) else {
            return SemanticPublishAdmissionV1::Admitted;
        };
        if entry.witness != witness {
            entries.remove(key);
            return SemanticPublishAdmissionV1::Admitted;
        }
        if now >= entry.retry_at {
            return SemanticPublishAdmissionV1::Admitted;
        }
        SemanticPublishAdmissionV1::Suppressed(SuppressedSemanticPublishV1 {
            reason: entry.reason.clone(),
            failures: entry.failures,
            retry_after: entry.retry_at.saturating_duration_since(now),
        })
    }

    /// Record a publish failure that is reproducible for this key.
    pub fn record_failure(&self, key: &SemanticPublishFailureKeyV1, witness: &str, reason: &str) {
        self.record_failure_at(key, witness, reason, Instant::now());
    }

    pub fn record_failure_at(
        &self,
        key: &SemanticPublishFailureKeyV1,
        witness: &str,
        reason: &str,
        now: Instant,
    ) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let failures = match entries.get(key) {
            Some(entry) if entry.witness == witness => entry.failures.saturating_add(1),
            _ => 1,
        };
        let retry_at = now
            .checked_add(self.backoff(failures))
            .unwrap_or_else(|| now + self.ceiling);
        entries.insert(
            key.clone(),
            MemoEntryV1 {
                witness: witness.to_owned(),
                reason: reason.to_owned(),
                failures,
                retry_at,
            },
        );
    }

    /// Forget any memo for `key`: the outcome it recorded is no longer true.
    pub fn record_success(&self, key: &SemanticPublishFailureKeyV1) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// Shared memo used by production scheduling.
pub fn semantic_publish_failure_memo() -> &'static SemanticPublishFailureMemoV1 {
    static MEMO: OnceLock<SemanticPublishFailureMemoV1> = OnceLock::new();
    MEMO.get_or_init(SemanticPublishFailureMemoV1::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{ManifestDigest, ProjectionKindV1};

    fn projection_key(schema_revision: &str) -> ProjectionKeyV1 {
        ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: schema_revision.to_owned(),
            profile_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("canonical test digest"),
        }
    }

    fn key(schema_revision: &str, corpus_chunks: usize) -> SemanticPublishFailureKeyV1 {
        SemanticPublishFailureKeyV1::new(projection_key(schema_revision), corpus_chunks)
    }

    fn memo() -> SemanticPublishFailureMemoV1 {
        SemanticPublishFailureMemoV1::new(Duration::from_secs(60), Duration::from_secs(600))
    }

    #[test]
    fn corpus_size_class_buckets_by_power_of_two() {
        assert_eq!(corpus_size_class(0), 0);
        assert_eq!(corpus_size_class(1), 1);
        assert_eq!(corpus_size_class(2), 2);
        assert_eq!(corpus_size_class(3), 2);
        assert_eq!(corpus_size_class(4), 3);
        assert_eq!(corpus_size_class(150_000), corpus_size_class(160_000));
        assert_ne!(corpus_size_class(150_000), corpus_size_class(1_500));
    }

    #[test]
    fn recorded_failure_suppresses_the_next_schedule() {
        let memo = memo();
        let now = Instant::now();
        let key = key("rev-1", 150_000);
        assert_eq!(
            memo.admit_at(&key, "witness", now),
            SemanticPublishAdmissionV1::Admitted
        );
        memo.record_failure_at(&key, "witness", "Publication", now);
        let SemanticPublishAdmissionV1::Suppressed(suppressed) =
            memo.admit_at(&key, "witness", now)
        else {
            panic!("second schedule must be suppressed");
        };
        assert_eq!(suppressed.failures, 1);
        assert_eq!(suppressed.reason, "Publication");
        assert!(suppressed.retry_after <= Duration::from_secs(60));
    }

    #[test]
    fn repeated_failures_back_off_exponentially_to_a_ceiling() {
        let memo = memo();
        let mut now = Instant::now();
        let key = key("rev-1", 150_000);
        let mut windows = Vec::new();
        for _ in 0..6 {
            memo.record_failure_at(&key, "witness", "Publication", now);
            let SemanticPublishAdmissionV1::Suppressed(suppressed) =
                memo.admit_at(&key, "witness", now)
            else {
                panic!("must be suppressed right after a failure");
            };
            windows.push(suppressed.retry_after);
            now += suppressed.retry_after;
            assert_eq!(
                memo.admit_at(&key, "witness", now),
                SemanticPublishAdmissionV1::Admitted,
                "elapsed backoff must re-admit"
            );
        }
        assert_eq!(windows[0], Duration::from_secs(60));
        assert_eq!(windows[1], Duration::from_secs(120));
        assert_eq!(windows[2], Duration::from_secs(240));
        assert_eq!(windows[3], Duration::from_secs(480));
        assert_eq!(windows[4], Duration::from_secs(600));
        assert_eq!(windows[5], Duration::from_secs(600));
    }

    #[test]
    fn a_different_projection_key_is_admitted() {
        let memo = memo();
        let now = Instant::now();
        memo.record_failure_at(&key("rev-1", 150_000), "witness", "Publication", now);
        assert_eq!(
            memo.admit_at(&key("rev-2", 150_000), "witness", now),
            SemanticPublishAdmissionV1::Admitted
        );
    }

    #[test]
    fn a_different_corpus_size_class_is_admitted() {
        let memo = memo();
        let now = Instant::now();
        memo.record_failure_at(&key("rev-1", 150_000), "witness", "Publication", now);
        assert_eq!(
            memo.admit_at(&key("rev-1", 1_500), "witness", now),
            SemanticPublishAdmissionV1::Admitted
        );
    }

    #[test]
    fn a_changed_witness_clears_the_memo() {
        let memo = memo();
        let now = Instant::now();
        let key = key("rev-1", 150_000);
        memo.record_failure_at(&key, "witness", "Publication", now);
        assert_eq!(
            memo.admit_at(&key, "migrated-witness", now),
            SemanticPublishAdmissionV1::Admitted
        );
        assert_eq!(memo.tracked_keys(), 0, "stale entry must be dropped");
    }

    #[test]
    fn a_success_clears_the_memo() {
        let memo = memo();
        let now = Instant::now();
        let key = key("rev-1", 150_000);
        memo.record_failure_at(&key, "witness", "Publication", now);
        memo.record_success(&key);
        assert_eq!(
            memo.admit_at(&key, "witness", now),
            SemanticPublishAdmissionV1::Admitted
        );
        assert_eq!(memo.tracked_keys(), 0);
    }

    #[test]
    fn witness_tracks_store_root_and_resource_ceilings() {
        let resources = SemanticResourceCeilings::default();
        let base = publish_failure_witness(Path::new("/a"), &resources);
        assert_ne!(base, publish_failure_witness(Path::new("/b"), &resources));
        let mut widened = resources;
        widened.max_resident_bytes = resources.max_resident_bytes * 2;
        assert_ne!(base, publish_failure_witness(Path::new("/a"), &widened));
    }
}
