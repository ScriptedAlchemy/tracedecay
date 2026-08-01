//! Free-page compaction for tracked branch databases (plan 38 §6).
//!
//! [`crate::daemon::git_watch::store_maintenance::run_project_compaction`] and
//! `run_global_compaction` already compact the live graph store and
//! `global.db` off the hot path, through the daemon's writer-actor runtime.
//! Every *other* tracked branch gets its own `SQLite` family under
//! `branches/`, cloned wholesale from an ancestor at `branch add` time and
//! then never revisited by any compaction pass. This is the exact bloat class
//! the owner's dogfood audit measured directly: 2.4 GB of free-page bloat
//! with individual branch databases sitting at 87-91% free pages.
//!
//! Those files are not mounted by any live daemon runtime between syncs, so
//! this module compacts them directly: open a short-lived, best-effort
//! `rusqlite` connection, sample the free-page ratio, and run
//! `PRAGMA incremental_vacuum(N)` when the same
//! [`tracedecay_application::storage::compaction::CompactionTriggerPolicyV1`]
//! threshold used for the live stores is met. This never competes with a live
//! writer: a busy/locked file is skipped, not an error, and the next
//! maintenance tick retries it.
//!
//! Branch databases inherit `PRAGMA auto_vacuum = INCREMENTAL` from the
//! ancestor they were cloned from (every fresh store is created with it, see
//! `src/db/migrations.rs::configure_fresh_auto_vacuum`), so `incremental_vacuum`
//! reclaims pages here exactly as it does on the live graph store. A branch
//! database predating that migration carries `auto_vacuum = NONE`, which makes
//! `incremental_vacuum` a *silent no-op* -- reclaiming its free pages would
//! need a full `VACUUM` rewrite, out of scope for a bounded, hot-path-safe
//! pass. That case is precisely the one the owner's audit measured, so this
//! pass refuses to report it as work done: the mode is checked up front and a
//! database that cannot be incrementally vacuumed is skipped with
//! [`BranchCompactionSkipReason::IncrementalVacuumUnavailable`], which the
//! daemon logs. Silently "compacting" zero pages would have reported success
//! over exactly the bloat this exists to remove.
//!
//! Nothing here is destructive: `incremental_vacuum` only returns already-free
//! pages to the filesystem and never touches live rows, so this pass has no
//! dry-run mode to gate -- there is no state it can destroy. It deliberately
//! does not consult
//! [`crate::migrate::durability`]: branch databases are project graph stores
//! (durable `memory_*` tables and all), and compaction is safe on them
//! precisely because it preserves every row.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use tracedecay_application::storage::compaction::CompactionTriggerPolicyV1;
use tracedecay_application::storage::identity::{FreePageRatioV1, StorageByteSizeV1, StoreKeyV1};
use tracedecay_application::storage::telemetry::StoreSizeSampleV1;
use tracedecay_domain::UtcMicros;
use tracedecay_runtime_core::sqlite_read_snapshot::{BOUNDED_PROBE_BUSY_TIMEOUT, pragma_u64};

use crate::config::CompactionThresholdConfig;

/// `PRAGMA auto_vacuum` mode in which `incremental_vacuum` actually reclaims
/// pages. `0` is `NONE` and `1` is `FULL`; only `2` (`INCREMENTAL`) responds.
const AUTO_VACUUM_INCREMENTAL: u64 = 2;

/// One tracked branch database file, other than the currently-active/mounted
/// one, eligible for direct-file compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDbCandidate {
    pub branch: String,
    pub db_path: PathBuf,
}

/// One branch database this pass actually vacuumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCompactionOutcome {
    pub branch: String,
    pub db_path: PathBuf,
    pub freed_pages: u64,
}

/// Why a candidate was left untouched this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchCompactionSkipReason {
    /// The file could not be opened or locked briefly enough to sample —
    /// almost always a concurrent writer (branch add/sync/gc). Expected and
    /// transient; the next tick retries.
    Busy,
    /// The file opened but its page/freelist pragmas could not be read.
    SampleFailed,
    /// The threshold was met but `PRAGMA incremental_vacuum` failed.
    VacuumFailed,
    /// The threshold was met but the database is not in
    /// `auto_vacuum = INCREMENTAL` mode, so `incremental_vacuum` would
    /// reclaim nothing. Reclaiming this file needs a full `VACUUM` rewrite,
    /// which this bounded pass deliberately does not do -- see module docs.
    IncrementalVacuumUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCompactionSkip {
    pub branch: String,
    pub db_path: PathBuf,
    pub reason: BranchCompactionSkipReason,
}

#[derive(Debug, Clone, Default)]
pub struct BranchCompactionReport {
    pub compacted: Vec<BranchCompactionOutcome>,
    pub skipped: Vec<BranchCompactionSkip>,
    /// The configured thresholds could not be turned into a valid trigger
    /// policy, so no candidate was even sampled. Surfaced rather than
    /// swallowed: a mistyped `free_page_ratio_threshold` (say `90` for a
    /// percentage) would otherwise disable branch compaction permanently and
    /// silently, and look identical to "nothing needed compacting".
    pub policy_invalid: bool,
}

/// Selects every tracked branch database file other than `active_db_path`
/// (the currently-mounted store, already compacted through the live daemon
/// runtime — see module docs).
///
/// The active store is excluded by *resolved* path, not by string equality:
/// `active_db_path` comes from a mounted handle while candidates are rebuilt
/// from `branch-meta.json`, and the two can name the same file through
/// different symlinks or non-normalized components. A missed exclusion would
/// not corrupt anything (`SQLite` locking is sound across processes) but it
/// would put this pass in contention with the daemon's own writer actor over
/// the one file it was written to stay away from.
pub fn select_branch_db_candidates(
    tracedecay_dir: &Path,
    meta: &crate::branch_meta::BranchMeta,
    active_db_path: &Path,
) -> Vec<BranchDbCandidate> {
    let active_resolved = active_db_path.canonicalize().ok();
    let is_active = |db_path: &Path| {
        if db_path == active_db_path {
            return true;
        }
        match (db_path.canonicalize().ok(), active_resolved.as_ref()) {
            (Some(resolved), Some(active)) => &resolved == active,
            _ => false,
        }
    };
    let mut candidates: Vec<_> = meta
        .branches
        .iter()
        .filter_map(|(name, entry)| {
            let db_path = tracedecay_dir.join(&entry.db_file);
            if is_active(&db_path) {
                return None;
            }
            Some(BranchDbCandidate {
                branch: name.clone(),
                db_path,
            })
        })
        .collect();
    candidates.sort_by(|left, right| left.branch.cmp(&right.branch));
    candidates
}

/// Runs bounded incremental-vacuum compaction over every candidate whose
/// free-page ratio crosses `config`'s threshold. Each file is handled
/// independently: one busy or failing file never blocks the rest.
pub fn compact_branch_databases(
    candidates: &[BranchDbCandidate],
    config: &CompactionThresholdConfig,
) -> BranchCompactionReport {
    let mut report = BranchCompactionReport::default();
    let Some(policy) = resolve_policy(config) else {
        report.policy_invalid = true;
        return report;
    };
    for candidate in candidates {
        if !candidate.db_path.is_file() {
            continue;
        }
        match compact_one(candidate, &policy, config.max_pages_per_tick) {
            Ok(Some(outcome)) => report.compacted.push(outcome),
            Ok(None) => {}
            Err(reason) => report.skipped.push(BranchCompactionSkip {
                branch: candidate.branch.clone(),
                db_path: candidate.db_path.clone(),
                reason,
            }),
        }
    }
    report
}

/// Turns the configured thresholds into a validated trigger policy once for
/// the whole pass. `None` means the configuration itself is unusable — the
/// caller reports that rather than silently treating every database as
/// ineligible.
///
/// Both rejections matter and neither is hypothetical: a ratio outside
/// `[0.0, 1.0]` fails [`FreePageRatioV1::new`], and a ratio of exactly `0.0`
/// fails [`CompactionTriggerPolicyV1::validate`] (it would schedule every
/// store on every pass). Validating here rather than per-file is what turns
/// the second case from "silently compacts nothing, forever" into a reported
/// configuration error.
fn resolve_policy(config: &CompactionThresholdConfig) -> Option<CompactionTriggerPolicyV1> {
    let policy = CompactionTriggerPolicyV1 {
        free_page_ratio_threshold: FreePageRatioV1::new(config.free_page_ratio_threshold).ok()?,
        minimum_reclaimable_bytes: StorageByteSizeV1(config.minimum_reclaimable_bytes),
    };
    policy.validate().ok()?;
    Some(policy)
}

/// This one is read-*write* on purpose — `incremental_vacuum` has to write —
/// so it cannot use the shared read-only probe, but it holds the same bounded
/// busy timeout: a locked branch database is skipped, never waited on.
fn open_bounded(path: &Path) -> Result<Connection, BranchCompactionSkipReason> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| BranchCompactionSkipReason::Busy)?;
    connection
        .busy_timeout(BOUNDED_PROBE_BUSY_TIMEOUT)
        .map_err(|_| BranchCompactionSkipReason::Busy)?;
    Ok(connection)
}

fn compact_one(
    candidate: &BranchDbCandidate,
    policy: &CompactionTriggerPolicyV1,
    max_pages_per_tick: u32,
) -> Result<Option<BranchCompactionOutcome>, BranchCompactionSkipReason> {
    let connection = open_bounded(&candidate.db_path)?;
    let page_size =
        pragma_u64(&connection, "page_size").ok_or(BranchCompactionSkipReason::SampleFailed)?;
    let page_count =
        pragma_u64(&connection, "page_count").ok_or(BranchCompactionSkipReason::SampleFailed)?;
    let freelist = pragma_u64(&connection, "freelist_count")
        .ok_or(BranchCompactionSkipReason::SampleFailed)?;
    if page_size == 0 || page_count == 0 {
        return Ok(None);
    }
    if !is_compaction_scheduled(&candidate.branch, page_size, page_count, freelist, policy) {
        return Ok(None);
    }
    // Only `auto_vacuum = INCREMENTAL` responds to `incremental_vacuum`.
    // Checking before instead of inferring from "freed nothing" keeps a
    // legacy branch database from being reported as successfully compacted
    // when its free pages are in fact unreachable to this pass.
    if pragma_u64(&connection, "auto_vacuum").ok_or(BranchCompactionSkipReason::SampleFailed)?
        != AUTO_VACUUM_INCREMENTAL
    {
        return Err(BranchCompactionSkipReason::IncrementalVacuumUnavailable);
    }
    let pages = max_pages_per_tick.max(1);
    connection
        .execute_batch(&format!("PRAGMA incremental_vacuum({pages})"))
        .map_err(|_| BranchCompactionSkipReason::VacuumFailed)?;
    let freelist_after = pragma_u64(&connection, "freelist_count").unwrap_or(freelist);
    Ok(Some(BranchCompactionOutcome {
        branch: candidate.branch.clone(),
        db_path: candidate.db_path.clone(),
        freed_pages: freelist.saturating_sub(freelist_after),
    }))
}

fn is_compaction_scheduled(
    branch: &str,
    page_size: u64,
    page_count: u64,
    freelist: u64,
    policy: &CompactionTriggerPolicyV1,
) -> bool {
    // The logical store key convention for a branch-scoped store, so telemetry
    // built from this sample names the branch rather than a shared constant.
    let Ok(store_key) = StoreKeyV1::new(format!("branches/{branch}")) else {
        return false;
    };
    let Ok(page_size_bytes) = u32::try_from(page_size) else {
        return false;
    };
    let sample = StoreSizeSampleV1 {
        store: store_key,
        page_size_bytes,
        page_count,
        freelist_pages: freelist,
        observed_at: UtcMicros(0),
    };
    policy
        .decide(&sample)
        .is_ok_and(|decision| decision.is_scheduled())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(threshold: f64) -> CompactionThresholdConfig {
        CompactionThresholdConfig {
            free_page_ratio_threshold: threshold,
            minimum_reclaimable_bytes: 0,
            max_pages_per_tick: 1024,
        }
    }

    fn bloated_db(path: &Path) {
        bloated_db_with_auto_vacuum(path, "INCREMENTAL");
    }

    fn bloated_db_with_auto_vacuum(path: &Path, mode: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(&format!("PRAGMA auto_vacuum = {mode};"))
            .unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY, payload BLOB);")
            .unwrap();
        let payload = vec![7u8; 64 * 1024];
        for id in 0..32i64 {
            connection
                .execute(
                    "INSERT INTO fixture (id, payload) VALUES (?1, ?2)",
                    rusqlite::params![id, payload],
                )
                .unwrap();
        }
        connection.execute_batch("DELETE FROM fixture;").unwrap();
    }

    #[test]
    fn select_branch_db_candidates_excludes_active_and_includes_others() {
        let dir = tempfile::tempdir().unwrap();
        let mut branches = HashMap::new();
        branches.insert(
            "main".to_string(),
            crate::branch_meta::BranchEntry {
                db_file: crate::config::DB_FILENAME.to_string(),
                parent: None,
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: false,
            },
        );
        branches.insert(
            "feature".to_string(),
            crate::branch_meta::BranchEntry {
                db_file: "branches/feature.db".to_string(),
                parent: Some("main".to_string()),
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: false,
            },
        );
        let meta = crate::branch_meta::BranchMeta {
            default_branch: "main".to_string(),
            branches,
        };
        let active = dir.path().join(crate::config::DB_FILENAME);
        let candidates = select_branch_db_candidates(dir.path(), &meta, &active);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].branch, "feature");
        assert_eq!(
            candidates[0].db_path,
            dir.path().join("branches/feature.db")
        );
    }

    #[test]
    fn compacts_a_bloated_branch_database_over_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feature.db");
        bloated_db(&path);
        let candidates = vec![BranchDbCandidate {
            branch: "feature".to_string(),
            db_path: path.clone(),
        }];

        let before = Connection::open(&path).unwrap();
        let freelist_before = pragma_u64(&before, "freelist_count").unwrap();
        assert!(freelist_before > 0, "fixture must create reclaimable pages");
        drop(before);

        let report = compact_branch_databases(&candidates, &config(0.5));
        assert!(report.skipped.is_empty(), "skipped: {:?}", report.skipped);
        assert_eq!(report.compacted.len(), 1);
        assert!(report.compacted[0].freed_pages > 0);

        let after = Connection::open(&path).unwrap();
        let freelist_after = pragma_u64(&after, "freelist_count").unwrap();
        assert!(freelist_after < freelist_before);
    }

    /// A *valid* threshold the database does not meet leaves it alone. The
    /// earlier version of this test used a threshold of `1.5`, which
    /// `FreePageRatioV1::new` rejects outright — it passed because the policy
    /// failed to build, not because the database was under threshold, and so
    /// asserted nothing about the policy at all.
    #[test]
    fn leaves_a_database_under_threshold_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feature.db");
        bloated_db(&path);
        let candidates = vec![BranchDbCandidate {
            branch: "feature".to_string(),
            db_path: path.clone(),
        }];

        let before = Connection::open(&path).unwrap();
        let freelist_before = pragma_u64(&before, "freelist_count").unwrap();
        drop(before);

        // Ratio met, but the reclaimable-bytes floor is unreachable, so the
        // real policy returns NotEligible.
        let under = CompactionThresholdConfig {
            free_page_ratio_threshold: 0.5,
            minimum_reclaimable_bytes: u64::MAX,
            max_pages_per_tick: 1024,
        };
        let report = compact_branch_databases(&candidates, &under);
        assert!(!report.policy_invalid);
        assert!(report.compacted.is_empty());
        assert!(report.skipped.is_empty());

        let after = Connection::open(&path).unwrap();
        assert_eq!(
            pragma_u64(&after, "freelist_count").unwrap(),
            freelist_before,
            "an ineligible database must not be vacuumed"
        );
    }

    /// A threshold outside `[0.0, 1.0]` (`90` meaning "90%", say) must be
    /// reported, not silently swallowed into "nothing needed compacting".
    #[test]
    fn an_invalid_threshold_is_reported_rather_than_disabling_the_pass_silently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feature.db");
        bloated_db(&path);
        let candidates = vec![BranchDbCandidate {
            branch: "feature".to_string(),
            db_path: path.clone(),
        }];

        let report = compact_branch_databases(&candidates, &config(90.0));

        assert!(
            report.policy_invalid,
            "an out-of-range threshold must surface as policy_invalid"
        );
        assert!(report.compacted.is_empty());
        assert!(report.skipped.is_empty());
    }

    /// A threshold of exactly `0.0` is in range for `FreePageRatioV1` but
    /// rejected by `CompactionTriggerPolicyV1::validate` (it would schedule
    /// every store on every pass). The inherited version of this module built
    /// the policy per file and swallowed that rejection into "not eligible",
    /// so a zero threshold compacted nothing and looked like success — the
    /// same bug that left this module's own headline test failing unnoticed.
    #[test]
    fn a_zero_threshold_is_reported_invalid_rather_than_compacting_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feature.db");
        bloated_db(&path);
        let candidates = vec![BranchDbCandidate {
            branch: "feature".to_string(),
            db_path: path.clone(),
        }];

        let report = compact_branch_databases(&candidates, &config(0.0));

        assert!(report.policy_invalid);
        assert!(report.compacted.is_empty());
        assert!(report.skipped.is_empty());
    }

    /// The legacy-bloat case the module exists for: `auto_vacuum = NONE`
    /// makes `incremental_vacuum` a no-op, and reporting that as a successful
    /// compaction would claim to have reclaimed the exact free pages it
    /// cannot reach.
    #[test]
    fn a_database_without_incremental_auto_vacuum_is_skipped_not_reported_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        bloated_db_with_auto_vacuum(&path, "NONE");
        let candidates = vec![BranchDbCandidate {
            branch: "legacy".to_string(),
            db_path: path.clone(),
        }];

        let report = compact_branch_databases(&candidates, &config(0.5));

        assert!(
            report.compacted.is_empty(),
            "compacted: {:?}",
            report.compacted
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            report.skipped[0].reason,
            BranchCompactionSkipReason::IncrementalVacuumUnavailable
        );
    }

    /// The active store is the daemon writer-actor's; excluding it only by
    /// string equality misses a path that reaches the same file through a
    /// symlinked profile directory.
    #[cfg(unix)]
    #[test]
    fn the_active_database_is_excluded_through_a_symlinked_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(crate::config::DB_FILENAME), b"").unwrap();

        let linked = dir.path().join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        #[cfg(not(unix))]
        return;

        let mut branches = HashMap::new();
        branches.insert(
            "main".to_string(),
            crate::branch_meta::BranchEntry {
                db_file: crate::config::DB_FILENAME.to_string(),
                parent: None,
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: false,
            },
        );
        let meta = crate::branch_meta::BranchMeta {
            default_branch: "main".to_string(),
            branches,
        };

        // The mounted handle names the file through `real/`; branch-meta
        // rebuilds it through the symlinked `linked/`.
        let candidates =
            select_branch_db_candidates(&linked, &meta, &real.join(crate::config::DB_FILENAME));

        assert!(
            candidates.is_empty(),
            "the active store must be excluded through a symlinked path: {candidates:?}"
        );
    }

    #[test]
    fn missing_file_is_silently_skipped_not_errored() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = vec![BranchDbCandidate {
            branch: "gone".to_string(),
            db_path: dir.path().join("does-not-exist.db"),
        }];
        let report = compact_branch_databases(&candidates, &config(0.5));
        assert!(report.compacted.is_empty());
        assert!(report.skipped.is_empty());
    }
}
