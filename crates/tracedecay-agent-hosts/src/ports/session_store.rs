//! The registered profile database, as automation reads it.
//!
//! A **port**. `global_db::RegisteredGlobalDb` is the user-level store holding
//! every project's sessions and analytics. It lives in `tracedecay-global-db`,
//! which sits beside this crate rather than beneath it, so automation names
//! the handful of reads it performs instead of the concrete handle.
//!
//! The scheduler asks when sessions were last active, skill-usage ingest
//! replays analytics rows, and evidence retrieval reads a snapshot and the
//! store's shard binding. Nothing here writes: automation's writes go through
//! the project store, not the profile database.
//!
//! Root wiring: the root implements [`AutomationSessionStore`] for
//! `RegisteredGlobalDb` (each method forwards to the identically named
//! inherent method, converting the analytics query/record field-for-field),
//! and registers [`register_canonical_project_key`] with
//! `RegisteredGlobalDb::canonical_project_key`.

use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;

use tracedecay_runtime_core::db::engine::ReadSnapshot;
use tracedecay_store::StoreRuntimeBindingV1;

/// Boxed future returned by the port's asynchronous reads.
pub type StoreFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Selector for a bounded analytics-event scan.
///
/// Every field is an optional narrowing except `limit`, which is mandatory:
/// automation must never issue an unbounded scan against the profile database.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalyticsEventQuery {
    pub provider: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub event_kind: Option<String>,
    /// Inclusive lower bound on `timestamp` (unix seconds). `None` = unbounded.
    pub since: Option<i64>,
    /// Exclusive upper bound on `timestamp` (unix seconds). `None` = unbounded.
    pub until: Option<i64>,
    /// Exclusive row-id cursor used by bounded reverse-chronological scans.
    pub before_id: Option<i64>,
    pub limit: usize,
}

/// One stored analytics event, as skill-usage ingest classifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventRecord {
    pub id: i64,
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

/// The reads automation performs against the registered profile database.
pub trait AutomationSessionStore: Send + Sync {
    /// Canonical path of the attached database file.
    fn database_path(&self) -> &Path;

    /// Typed shard binding this attachment serves.
    ///
    /// Evidence retrieval checks the binding's shard against the active
    /// profile identity before trusting a stored session as in-scope.
    fn binding(&self) -> &StoreRuntimeBindingV1;

    /// Unix seconds of the most recent session activity, or `None` when the
    /// store holds no timestamped messages.
    ///
    /// The scheduler uses this to decide whether an automation run has
    /// anything new to look at, so "no activity" and "read failed" both
    /// correctly collapse to `None`.
    fn latest_session_activity_secs(&self) -> StoreFuture<'_, Option<i64>>;

    /// Opens a read snapshot for a bounded direct query.
    fn read_snapshot(&self) -> StoreFuture<'_, Result<ReadSnapshot, String>>;

    /// Runs one bounded analytics-event scan.
    fn query_analytics_events<'a>(
        &'a self,
        query: &'a AnalyticsEventQuery,
    ) -> StoreFuture<'a, Result<Vec<AnalyticsEventRecord>, String>>;
}

impl AutomationSessionStore for tracedecay_global_db::RegisteredGlobalDb {
    fn database_path(&self) -> &Path {
        self.db_path()
    }

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.binding()
    }

    fn latest_session_activity_secs(&self) -> StoreFuture<'_, Option<i64>> {
        Box::pin(self.latest_session_activity_secs())
    }

    fn read_snapshot(&self) -> StoreFuture<'_, Result<ReadSnapshot, String>> {
        Box::pin(async move {
            self.read_snapshot()
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn query_analytics_events<'a>(
        &'a self,
        query: &'a AnalyticsEventQuery,
    ) -> StoreFuture<'a, Result<Vec<AnalyticsEventRecord>, String>> {
        Box::pin(async move {
            let records = self
                .query_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
                    provider: query.provider.clone(),
                    project_id: query.project_id.clone(),
                    session_id: query.session_id.clone(),
                    event_kind: query.event_kind.clone(),
                    since: query.since,
                    until: query.until,
                    before_id: query.before_id,
                    limit: query.limit,
                })
                .await?;
            Ok(records
                .into_iter()
                .map(|record| AnalyticsEventRecord {
                    id: record.id,
                    provider: record.provider,
                    project_id: record.project_id,
                    session_id: record.session_id,
                    timestamp: record.timestamp,
                    event_kind: record.event_kind,
                    hook_name: record.hook_name,
                    tool_name: record.tool_name,
                    tool_category: record.tool_category,
                    skill_name: record.skill_name,
                    hint_category: record.hint_category,
                    hint_id: record.hint_id,
                    outcome: record.outcome,
                    metadata_json: record.metadata_json,
                })
                .collect())
        })
    }
}

/// Derives the profile database's project key from a project root.
pub type CanonicalProjectKey = fn(&Path) -> String;

static CANONICAL_PROJECT_KEY: OnceLock<CanonicalProjectKey> = OnceLock::new();

/// Registers the root crate's project-key derivation.
///
/// Idempotent: the first registration wins.
pub fn register_canonical_project_key(canonical_project_key: CanonicalProjectKey) {
    let _ = CANONICAL_PROJECT_KEY.set(canonical_project_key);
}

/// The profile database's key for `project_root`.
///
/// Falls back to the lossy path string when the root never registered. That
/// matches the registered derivation for an already-canonical root, so an
/// unwired build still scopes its analytics query to one project rather than
/// silently querying every project's rows.
#[must_use]
pub fn canonical_project_key(project_root: &Path) -> String {
    CANONICAL_PROJECT_KEY.get().map_or_else(
        || project_root.to_string_lossy().into_owned(),
        |canonical| canonical(project_root),
    )
}
