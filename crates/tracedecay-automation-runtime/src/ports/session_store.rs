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
//! inherent method) and registers [`register_canonical_project_key`] with
//! `RegisteredGlobalDb::canonical_project_key`.

use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;

use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_store::StoreRuntimeBindingV1;

pub use tracedecay_global_db::{AnalyticsEventQuery, AnalyticsEventRecord};

/// Boxed future returned by the port's asynchronous reads.
pub type StoreFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

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
    /// The scheduler uses this as a gate: no observed activity means nothing
    /// new to run against. The registered adapter maps a failed read to
    /// `None` with a logged warning — a store the scheduler cannot read has
    /// no observable new activity, so automation stays idle instead of
    /// running against a broken store.
    fn latest_session_activity_secs(&self) -> StoreFuture<'_, Option<i64>>;

    /// Opens a read snapshot for a bounded direct query.
    fn read_snapshot(&self) -> StoreFuture<'_, Result<DatabaseEngineReadSnapshot, String>>;

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
        Box::pin(async move {
            match self.latest_session_activity_secs().await {
                Ok(latest) => latest,
                Err(error) => {
                    tracing::warn!(
                        database = %self.db_path().display(),
                        %error,
                        "session-activity read failed; scheduler observes no new activity"
                    );
                    None
                }
            }
        })
    }

    fn read_snapshot(&self) -> StoreFuture<'_, Result<DatabaseEngineReadSnapshot, String>> {
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
        Box::pin(self.query_analytics_events(query))
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
