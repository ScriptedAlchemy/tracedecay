use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use super::branch_admin::StoreAdministration;

const COLD_STORE_PAGE_LIMIT: usize = 8;
/// Upper bound on mounted session databases + project graphs a single
/// maintenance tick may process. The tick holds the writer lane while it works,
/// so an unbounded loop over every mounted project×branch starved query/index
/// writers; this budget caps the per-tick work and a round-robin cursor
/// (`store_cursor`) guarantees every store is still reached across ticks.
const MAINTENANCE_STORE_PAGE_LIMIT: usize = 8;
const CHECKPOINT_DIRECTORY: &str = "maintenance";
const CHECKPOINT_FILE: &str = "retention-cold-store-cursor-v1.json";

#[derive(Debug)]
pub(super) struct MaintenanceCadence {
    interval: Duration,
    retry_delay: Duration,
    not_before: Option<Instant>,
    in_flight: bool,
}

impl MaintenanceCadence {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            retry_delay: interval.min(Duration::from_mins(1)),
            not_before: None,
            in_flight: false,
        }
    }

    pub(super) fn reserve(&mut self, now: Instant) -> bool {
        if self.in_flight || self.not_before.is_some_and(|not_before| now < not_before) {
            return false;
        }
        self.in_flight = true;
        true
    }

    pub(super) fn finish(&mut self, now: Instant, succeeded: bool) -> Duration {
        self.in_flight = false;
        let delay = if succeeded {
            self.interval
        } else {
            self.retry_delay
        };
        self.not_before = Some(now + delay);
        delay
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct ColdStoreCursorV1 {
    pub(super) after_project_id: Option<String>,
}

fn next_cold_store_cursor(
    previous: Option<&str>,
    project_ids: &[String],
    has_more: bool,
) -> Option<ColdStoreCursorV1> {
    if !has_more {
        return None;
    }
    Some(ColdStoreCursorV1 {
        after_project_id: project_ids
            .last()
            .cloned()
            .or_else(|| previous.map(str::to_owned)),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaintenanceStoreOutcomeV1 {
    Processed,
    Busy,
    Missing,
    Unreadable,
    Cancelled,
}

impl MaintenanceStoreOutcomeV1 {
    fn was_processed(self) -> bool {
        self == Self::Processed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct MaintenanceMetricsV1 {
    pub(super) ticks: u64,
    pub(super) processed_stores: u64,
    pub(super) deferred_stores: u64,
    pub(super) unavailable_stores: u64,
    pub(super) reclaimed_bytes: u64,
    pub(super) last_outcome: Option<MaintenanceStoreOutcomeV1>,
}

#[derive(Clone)]
pub(super) struct MaintenanceCoordinator {
    cancellation: crate::application::context::CancellationToken,
    wake: Arc<Notify>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    metrics: Arc<Mutex<MaintenanceMetricsV1>>,
    /// Round-robin fairness cursor over mounted stores: the sort key of the
    /// last store processed. The next tick resumes immediately after it so no
    /// store is starved when the mounted set exceeds `MAINTENANCE_STORE_PAGE_LIMIT`.
    store_cursor: Arc<Mutex<Option<String>>>,
}

impl Default for MaintenanceCoordinator {
    fn default() -> Self {
        Self {
            cancellation: crate::application::context::CancellationToken::new(),
            wake: Arc::new(Notify::new()),
            task: Arc::new(Mutex::new(None)),
            metrics: Arc::new(Mutex::new(MaintenanceMetricsV1::default())),
            store_cursor: Arc::new(Mutex::new(None)),
        }
    }
}

/// One unit of bounded per-tick maintenance work: either a mounted session
/// database or a mounted project graph. Arcs are cloned into the item so the
/// store stays alive for the duration of the writer-held critical section.
enum MaintenanceStoreWork {
    Session(Arc<crate::global_db::RegisteredGlobalDb>),
    Graph(Arc<crate::tracedecay::TraceDecay>),
}

/// Pure round-robin window selection over stably-sorted store keys.
///
/// Returns the indices to process this tick (at most `budget`, always
/// `min(budget, keys.len())`) and the cursor to resume after next tick. Sorting
/// the keys and resuming after the previous cursor guarantees that, across
/// `ceil(len / budget)` consecutive ticks, every store is processed at least
/// once — nothing that should be reclaimed is starved forever — while any
/// single tick touches no more than `budget` stores.
fn select_store_window(
    keys: &[String],
    after: Option<&str>,
    budget: usize,
) -> (Vec<usize>, Option<String>) {
    let count = keys.len();
    if count == 0 || budget == 0 {
        return (Vec::new(), after.map(str::to_owned));
    }
    let start = match after {
        Some(cursor) => keys.partition_point(|key| key.as_str() <= cursor) % count,
        None => 0,
    };
    let take = budget.min(count);
    let indices = (0..take).map(|offset| (start + offset) % count).collect::<Vec<_>>();
    let next = indices.last().map(|&index| keys[index].clone());
    (indices, next)
}

impl MaintenanceCoordinator {
    pub(super) async fn spawn(
        profile_root: PathBuf,
        profile_database: Arc<crate::global_db::RegisteredGlobalDb>,
        administration: StoreAdministration,
        retention: crate::config::RetentionConfig,
    ) -> Self {
        let coordinator = Self::default();
        if !retention_maintenance_enabled(&retention) {
            return coordinator;
        }
        let task_owner = coordinator.clone();
        let interval = Duration::from_secs(retention.interval_hours.max(1).saturating_mul(3_600));
        let handle = tokio::spawn(async move {
            task_owner
                .run(
                    profile_root,
                    profile_database,
                    administration,
                    retention,
                    interval,
                )
                .await;
        });
        *coordinator.task.lock().await = Some(handle);
        coordinator
    }

    #[cfg(unix)]
    pub(super) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(super) async fn shutdown(&self) {
        self.cancellation.cancel();
        self.wake.notify_waiters();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    async fn run(
        &self,
        profile_root: PathBuf,
        profile_database: Arc<crate::global_db::RegisteredGlobalDb>,
        administration: StoreAdministration,
        retention: crate::config::RetentionConfig,
        interval: Duration,
    ) {
        let mut cadence = MaintenanceCadence::new(interval);
        let mut next_delay = cadence.retry_delay;
        loop {
            tokio::select! {
                biased;
                () = self.cancellation.cancelled() => break,
                () = self.wake.notified() => {}
                () = tokio::time::sleep(next_delay) => {}
            }
            if self.cancellation.is_cancelled() {
                break;
            }
            let now = Instant::now();
            if !cadence.reserve(now) {
                continue;
            }
            let succeeded = self
                .run_tick(
                    &profile_root,
                    profile_database.as_ref(),
                    &administration,
                    &retention,
                )
                .await;
            next_delay = cadence.finish(Instant::now(), succeeded);
        }
    }

    async fn run_tick(
        &self,
        profile_root: &Path,
        profile_database: &crate::global_db::RegisteredGlobalDb,
        administration: &StoreAdministration,
        retention: &crate::config::RetentionConfig,
    ) -> bool {
        let session_databases = administration.mounted_registered_session_databases().await;
        let project_graphs = administration.mounted_project_graphs().await;

        // Build one stably-sorted work list across both store kinds so the
        // per-tick budget and round-robin cursor bound the total work, not each
        // loop independently. Keys are unique on-disk identities (session db
        // path; project root + serving branch), prefixed by kind so the order
        // is deterministic regardless of the mounted maps' iteration order.
        let mut work: Vec<(String, MaintenanceStoreWork)> =
            Vec::with_capacity(session_databases.len() + project_graphs.len());
        for database in &session_databases {
            work.push((
                format!("s:{}", database.db_path().display()),
                MaintenanceStoreWork::Session(Arc::clone(database)),
            ));
        }
        for graph in &project_graphs {
            work.push((
                format!(
                    "g:{}\u{1f}{}",
                    graph.project_root().display(),
                    graph.serving_branch().unwrap_or_default()
                ),
                MaintenanceStoreWork::Graph(Arc::clone(graph)),
            ));
        }
        work.sort_by(|left, right| left.0.cmp(&right.0));
        let keys = work.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
        let after = self.store_cursor.lock().await.clone();
        let (window, next_cursor) =
            select_store_window(&keys, after.as_deref(), MAINTENANCE_STORE_PAGE_LIMIT);

        let admitted = administration
            .try_with_writer(|| async {
                let mut succeeded = true;
                // Bounded, round-robin slice of mounted stores. The cursor
                // advances even on cancellation so the next tick resumes past
                // the stores already handled rather than restarting.
                for &index in &window {
                    if self.cancellation.is_cancelled() {
                        *self.store_cursor.lock().await = next_cursor.clone();
                        return false;
                    }
                    match &work[index].1 {
                        MaintenanceStoreWork::Session(database) => {
                            succeeded &= super::store_maintenance::run_session_retention(
                                database, retention,
                            )
                            .await;
                        }
                        MaintenanceStoreWork::Graph(graph) => {
                            succeeded &=
                                super::store_maintenance::run_code_generation_retention(graph)
                                    .await;
                            // Generation retention only ever sees the one scope
                            // root derived from this graph's project root. Scope
                            // reconciliation is the sibling pass that reaches
                            // whole scope directories whose project root no
                            // longer exists.
                            succeeded &=
                                super::store_maintenance::run_code_index_scope_reconciliation(graph)
                                    .await;
                            if let Some(compaction) = &retention.compaction {
                                succeeded &= super::store_maintenance::run_project_compaction(
                                    graph.db(),
                                    compaction,
                                )
                                .await;
                                succeeded &= super::store_maintenance::run_branch_compaction(
                                    graph, compaction,
                                )
                                .await;
                            }
                        }
                    }
                }
                *self.store_cursor.lock().await = next_cursor.clone();
                // Global (profile-wide) compaction is a single bounded op, not a
                // per-store loop, so it runs every tick outside the round-robin.
                if let Some(compaction) = &retention.compaction {
                    succeeded &= super::store_maintenance::run_global_compaction(
                        profile_database,
                        compaction,
                    )
                    .await;
                }
                match run_cold_store_page(
                    profile_root,
                    profile_database,
                    retention,
                    &self.cancellation,
                )
                .await
                {
                    Ok(page) => {
                        let mut metrics = self.metrics.lock().await;
                        metrics.processed_stores = metrics
                            .processed_stores
                            .saturating_add(page.processed_stores);
                        // Per-tick gauge, not a lifetime total: a store missing
                        // from disk is re-observed every census pass, so a
                        // cumulative sum grew without bound (the "148"). Report
                        // the current census page's unavailable count instead.
                        metrics.unavailable_stores = page.unavailable_stores;
                        metrics.reclaimed_bytes =
                            metrics.reclaimed_bytes.saturating_add(page.reclaimed_bytes);
                        metrics.last_outcome = Some(page.outcome);
                        succeeded &= page.outcome.was_processed();
                    }
                    Err(_) => succeeded = false,
                }
                succeeded
            })
            .await;

        let mut metrics = self.metrics.lock().await;
        metrics.ticks = metrics.ticks.saturating_add(1);
        let succeeded = match admitted {
            Some(succeeded) => succeeded,
            None => {
                metrics.deferred_stores = metrics.deferred_stores.saturating_add(1);
                metrics.last_outcome = Some(MaintenanceStoreOutcomeV1::Busy);
                false
            }
        };
        super::log_daemon_event(
            "retention_maintenance_tick",
            &[
                ("succeeded", succeeded.to_string()),
                ("processed_stores", metrics.processed_stores.to_string()),
                ("deferred_stores", metrics.deferred_stores.to_string()),
                ("unavailable_stores", metrics.unavailable_stores.to_string()),
                ("reclaimed_bytes", metrics.reclaimed_bytes.to_string()),
            ],
        );
        succeeded
    }
}

#[derive(Debug)]
struct ColdStorePageMetrics {
    processed_stores: u64,
    unavailable_stores: u64,
    reclaimed_bytes: u64,
    outcome: MaintenanceStoreOutcomeV1,
}

impl Default for ColdStorePageMetrics {
    fn default() -> Self {
        Self {
            processed_stores: 0,
            unavailable_stores: 0,
            reclaimed_bytes: 0,
            outcome: MaintenanceStoreOutcomeV1::Processed,
        }
    }
}

async fn run_cold_store_page(
    profile_root: &Path,
    profile_database: &crate::global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    cancellation: &crate::application::context::CancellationToken,
) -> crate::errors::Result<ColdStorePageMetrics> {
    let checkpoint_path = checkpoint_path(profile_root);
    let cursor = load_cursor(&checkpoint_path).unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    let page = crate::retention::orphan_stores::build_store_census_page(
        profile_database,
        profile_root,
        cursor.after_project_id.as_deref(),
        COLD_STORE_PAGE_LIMIT,
    )
    .await?;
    let mut metrics = ColdStorePageMetrics::default();
    for entry in &page.entries {
        let outcome = classify_cold_store_state(
            cancellation.is_cancelled(),
            entry.manifest_readable,
            entry.data_root.is_dir(),
        );
        match outcome {
            MaintenanceStoreOutcomeV1::Processed => {
                metrics.processed_stores = metrics.processed_stores.saturating_add(1);
            }
            MaintenanceStoreOutcomeV1::Cancelled => {
                metrics.outcome = outcome;
                return Ok(metrics);
            }
            MaintenanceStoreOutcomeV1::Busy
            | MaintenanceStoreOutcomeV1::Missing
            | MaintenanceStoreOutcomeV1::Unreadable => {
                if metrics.outcome == MaintenanceStoreOutcomeV1::Processed {
                    metrics.outcome = outcome;
                }
                metrics.unavailable_stores = metrics.unavailable_stores.saturating_add(1);
            }
        }
    }
    if let Some(days) = retention.orphan_store_gc_days {
        let findings =
            crate::retention::orphan_stores::classify_stores(&page.entries, now_secs_i64());
        let plan =
            crate::retention::orphan_stores::plan_collection(findings, retention_window_secs(days));
        let (outcome, _) = crate::retention::orphan_stores::execute_registered_collection(
            profile_database,
            &plan,
            profile_root,
        )
        .await?;
        metrics.reclaimed_bytes = metrics
            .reclaimed_bytes
            .saturating_add(outcome.reclaimed_bytes);
        metrics.unavailable_stores = metrics
            .unavailable_stores
            .saturating_add(outcome.errors.len() as u64);
        if !outcome.errors.is_empty() {
            metrics.outcome = MaintenanceStoreOutcomeV1::Unreadable;
        }
    }
    if let Some(days) = retention.incident_debris_retention_days {
        let report = crate::retention::incident_debris::sweep_incident_debris(
            &page.entries,
            profile_root,
            retention_window_secs(days),
            now_secs_i64(),
        );
        metrics.reclaimed_bytes = metrics
            .reclaimed_bytes
            .saturating_add(report.reclaimed_bytes);
        metrics.unavailable_stores = metrics
            .unavailable_stores
            .saturating_add(report.errors.len() as u64);
        if !report.errors.is_empty() {
            metrics.outcome = MaintenanceStoreOutcomeV1::Unreadable;
        }
    }
    let project_ids = page
        .entries
        .iter()
        .map(|entry| entry.project_id.clone())
        .collect::<Vec<_>>();
    let next_cursor = next_cold_store_cursor(
        cursor.after_project_id.as_deref(),
        &project_ids,
        page.next_cursor.is_some(),
    )
    .unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    persist_cursor(&checkpoint_path, &next_cursor).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("persist maintenance cold-store cursor: {error}"),
        }
    })?;
    Ok(metrics)
}

fn classify_cold_store_state(
    cancelled: bool,
    manifest_readable: bool,
    data_root_exists: bool,
) -> MaintenanceStoreOutcomeV1 {
    if cancelled {
        MaintenanceStoreOutcomeV1::Cancelled
    } else if !data_root_exists {
        MaintenanceStoreOutcomeV1::Missing
    } else if !manifest_readable {
        MaintenanceStoreOutcomeV1::Unreadable
    } else {
        MaintenanceStoreOutcomeV1::Processed
    }
}

fn checkpoint_path(profile_root: &Path) -> PathBuf {
    profile_root
        .join(CHECKPOINT_DIRECTORY)
        .join(CHECKPOINT_FILE)
}

fn load_cursor(path: &Path) -> Option<ColdStoreCursorV1> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn persist_cursor(path: &Path, cursor: &ColdStoreCursorV1) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("maintenance cursor has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(cursor).map_err(std::io::Error::other)?;
    let mut file = std::fs::File::create(&temporary)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

pub(super) fn retention_maintenance_enabled(retention: &crate::config::RetentionConfig) -> bool {
    retention.session_lcm.enabled
        || retention.observation.enabled
        || retention.orphan_store_gc_days.is_some()
        || retention.incident_debris_retention_days.is_some()
        || retention.compaction.is_some()
}

pub(super) fn retention_window_secs(days: u64) -> i64 {
    i64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .unwrap_or(i64::MAX)
}

fn now_secs_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        ColdStoreCursorV1, MAINTENANCE_STORE_PAGE_LIMIT, MaintenanceCadence,
        MaintenanceStoreOutcomeV1, checkpoint_path, classify_cold_store_state, load_cursor,
        next_cold_store_cursor, persist_cursor, select_store_window,
    };

    #[test]
    fn cadence_rate_limits_failures_and_successes() {
        let started = Instant::now();
        let mut cadence = MaintenanceCadence::new(Duration::from_mins(1));

        assert!(cadence.reserve(started));
        assert!(!cadence.reserve(started));
        assert_eq!(cadence.finish(started, false), Duration::from_mins(1));
        assert!(!cadence.reserve(started + Duration::from_secs(59)));
        let retried = started + Duration::from_mins(1);
        assert!(cadence.reserve(retried));
        assert_eq!(cadence.finish(retried, true), Duration::from_mins(1));
        assert!(!cadence.reserve(retried + Duration::from_secs(59)));
        assert!(cadence.reserve(retried + Duration::from_mins(1)));
    }

    fn store_keys(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("s:{index:03}")).collect()
    }

    #[test]
    fn store_window_is_bounded_by_the_per_tick_budget() {
        let keys = store_keys(50);
        let (window, _) = select_store_window(&keys, None, MAINTENANCE_STORE_PAGE_LIMIT);
        assert_eq!(window.len(), MAINTENANCE_STORE_PAGE_LIMIT);

        // A mounted set smaller than the budget is processed whole.
        let small = store_keys(3);
        let (window, _) = select_store_window(&small, None, MAINTENANCE_STORE_PAGE_LIMIT);
        assert_eq!(window, vec![0, 1, 2]);
    }

    #[test]
    fn store_window_round_robin_reaches_every_store_and_never_starves() {
        // With more stores than the budget, feeding each tick's cursor into the
        // next must cover every store within ceil(count / budget) ticks while
        // no tick exceeds the budget — nothing reclaimable is skipped forever.
        for &(count, budget) in &[(7usize, 3usize), (50, 8), (17, 5), (8, 8), (1, 8)] {
            let keys = store_keys(count);
            let ticks = count.div_ceil(budget);
            let mut cursor: Option<String> = None;
            let mut covered = std::collections::BTreeSet::new();
            for _ in 0..ticks {
                let (window, next) = select_store_window(&keys, cursor.as_deref(), budget);
                assert!(
                    window.len() <= budget,
                    "count={count} budget={budget}: tick exceeded budget"
                );
                for index in window {
                    covered.insert(index);
                }
                cursor = next;
            }
            assert_eq!(
                covered.len(),
                count,
                "count={count} budget={budget}: not every store reached within {ticks} ticks"
            );
        }
    }

    #[test]
    fn store_window_resumes_after_the_cursor() {
        let keys = store_keys(10);
        let (first, next) = select_store_window(&keys, None, 4);
        assert_eq!(first, vec![0, 1, 2, 3]);
        assert_eq!(next.as_deref(), Some("s:003"));
        let (second, next) = select_store_window(&keys, next.as_deref(), 4);
        assert_eq!(second, vec![4, 5, 6, 7]);
        assert_eq!(next.as_deref(), Some("s:007"));
        // The window wraps past the end back to the front.
        let (third, _) = select_store_window(&keys, next.as_deref(), 4);
        assert_eq!(third, vec![8, 9, 0, 1]);
    }

    #[test]
    fn store_window_empty_set_preserves_cursor() {
        let (window, next) = select_store_window(&[], Some("s:005"), 8);
        assert!(window.is_empty());
        assert_eq!(next.as_deref(), Some("s:005"));
    }

    #[test]
    fn cold_store_cursor_resumes_after_the_last_complete_project() {
        let first = next_cold_store_cursor(
            None,
            &["project-a".to_owned(), "project-b".to_owned()],
            true,
        )
        .expect("first page cursor");
        assert_eq!(
            first,
            ColdStoreCursorV1 {
                after_project_id: Some("project-b".to_owned()),
            }
        );

        assert_eq!(
            next_cold_store_cursor(
                first.after_project_id.as_deref(),
                &["project-c".to_owned()],
                false,
            ),
            None
        );
    }

    #[test]
    fn cold_store_outcomes_do_not_report_deferred_work_as_processed() {
        for outcome in [
            MaintenanceStoreOutcomeV1::Busy,
            MaintenanceStoreOutcomeV1::Missing,
            MaintenanceStoreOutcomeV1::Unreadable,
            MaintenanceStoreOutcomeV1::Cancelled,
        ] {
            assert!(!outcome.was_processed());
        }
        assert!(MaintenanceStoreOutcomeV1::Processed.was_processed());
    }

    #[test]
    fn cold_store_checkpoint_survives_restart() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-b".to_owned()),
        };

        persist_cursor(&path, &expected).expect("persist cursor");

        assert_eq!(load_cursor(&path), Some(expected));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn cold_store_state_distinguishes_missing_unreadable_and_cancelled() {
        assert_eq!(
            classify_cold_store_state(false, true, true),
            MaintenanceStoreOutcomeV1::Processed
        );
        assert_eq!(
            classify_cold_store_state(false, true, false),
            MaintenanceStoreOutcomeV1::Missing
        );
        assert_eq!(
            classify_cold_store_state(false, false, true),
            MaintenanceStoreOutcomeV1::Unreadable
        );
        assert_eq!(
            classify_cold_store_state(true, true, true),
            MaintenanceStoreOutcomeV1::Cancelled
        );
    }
}
