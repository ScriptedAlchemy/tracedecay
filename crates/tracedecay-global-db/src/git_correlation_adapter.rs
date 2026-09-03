//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tracedecay_graph_db::GraphNamespace;
use tracedecay_store::StoreShardScopeV1;

use crate::{
    RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, VerifiedGraphRuntimePortV1,
    VerifiedGraphRuntimeWeakProxyV1,
};
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_sessions::runtime::git_correlation::{
    AUTO_BACKFILL_WATERMARK_KEY, BackfillOptions, BoundedBackfillOutcome, BoundedGitControl,
    CommitRelationFilter, CommitSessionRecord, CorrelationIndexHealth, CorrelationIndexPresence,
    DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError, GitCorrelationSessionStore,
    GitEvidenceProjectionStore, SessionGitCorrelationHit, SessionsForQuery, SpanObservation,
    git_evidence_projection_identity, publish_transcript_graph_evidence, read_meta_value,
    recover_git_evidence_projection, run_bounded_history_index_page,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::runtime::git_correlation::{
    AnalyticsSessionTimestampSource, BackfillStats, GitReflogSource, run_backfill,
    run_incremental_backfill,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";

type GitEvidencePublicationLock = Mutex<()>;

static GIT_EVIDENCE_PUBLICATION_LOCKS: OnceLock<
    Mutex<BTreeMap<String, Weak<GitEvidencePublicationLock>>>,
> = OnceLock::new();

fn shared_git_evidence_publication_lock(
    runtime: &VerifiedGraphRuntimeWeakProxyV1,
) -> Result<Arc<GitEvidencePublicationLock>, String> {
    let identity = serde_json::to_string(&(
        runtime.relational_binding(),
        runtime.relational_verified_locator(),
    ))
    .map_err(|error| format!("encode Git evidence graph runtime identity: {error}"))?;
    shared_git_evidence_publication_lock_for_identity(identity)
}

fn shared_git_evidence_publication_lock_for_identity(
    identity: String,
) -> Result<Arc<GitEvidencePublicationLock>, String> {
    let registry = GIT_EVIDENCE_PUBLICATION_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = registry
        .lock()
        .map_err(|_| "Git evidence publication lock registry is poisoned".to_owned())?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&identity).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(identity, Arc::downgrade(&lock));
    Ok(lock)
}

/// Adapter over an already-open project-sessions database.
///
/// The holder `D` is generic so callers that own a `RegisteredGlobalDbLeaseV1`
/// can build a lifetime-free (`'static`) adapter. A borrowed adapter makes the
/// `GitCorrelationSessionStore` impl apply only "for some specific lifetime",
/// so any future that holds one across an await and must then prove `Send`
/// raises a higher-ranked `for<'0> GlobalDbGitCorrelationStore<'0>: …`
/// obligation the compiler cannot discharge. Owning the handle keeps the impl
/// lifetime-free. Borrowed holders remain supported for call sites that never
/// cross such a boundary.
pub struct GlobalDbGitCorrelationStore<D> {
    db: D,
    graph_runtime: Option<VerifiedGraphRuntimeWeakProxyV1>,
    graph_publication_lock: Option<Result<Arc<GitEvidencePublicationLock>, String>>,
}

impl<D> GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    pub fn new(db: D) -> Self {
        let graph_runtime = db.borrow().project_graph_runtime().cloned();
        let graph_publication_lock = graph_runtime
            .as_ref()
            .map(shared_git_evidence_publication_lock);
        Self {
            db,
            graph_runtime,
            graph_publication_lock,
        }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    pub fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        if matches!(
            &self.db().binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        ) {
            Ok(())
        } else {
            Err(GitCorrelationError::Db(
                "git correlation requires registered ProjectSessions authority".to_string(),
            ))
        }
    }

    #[hotpath::measure(label = "global_db.git_correlation.read_snapshot", future = true)]
    pub async fn read_snapshot(&self) -> Result<DatabaseEngineReadSnapshot, GitCorrelationError> {
        self.db()
            .read_snapshot()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    #[hotpath::measure(label = "global_db.git_correlation.write_txn", future = true)]
    pub async fn open_write_transaction(
        &self,
    ) -> Result<RegisteredGlobalDbWriteTransaction<'_>, GitCorrelationError> {
        self.db()
            .begin_write_transaction()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    #[hotpath::measure(label = "global_db.git_correlation.record_span", future = true)]
    pub async fn record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64, GitCorrelationError> {
        let (changed, _) = publish_transcript_graph_evidence(
            self,
            "hook-route-span",
            std::slice::from_ref(observation),
            &[],
            merge_gap_secs,
        )?;
        i64::try_from(changed).map_err(|_| {
            GitCorrelationError::Contract(
                "Git evidence span publication count exceeds i64".to_owned(),
            )
        })
    }

    #[hotpath::measure(label = "global_db.git_correlation.publish_evidence")]
    pub fn publish_transcript_evidence(
        &self,
        publication_prefix: &str,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> Result<(), GitCorrelationError> {
        if commit_records.is_empty() && span_observations.is_empty() {
            return Ok(());
        }
        publish_transcript_graph_evidence(
            self,
            publication_prefix,
            span_observations,
            commit_records,
            DEFAULT_SPAN_MERGE_GAP_SECS,
        )?;
        Ok(())
    }

    /// `Ok(None)` means the projection has never published a verified head:
    /// the project has no recorded Git evidence yet.
    #[hotpath::measure(label = "global_db.git_correlation.projection")]
    pub fn git_evidence_projection(
        &self,
    ) -> Result<Option<GitEvidenceProjectionStore>, GitCorrelationError> {
        let identity =
            git_evidence_projection_identity(GraphNamespace::new(GIT_EVIDENCE_GRAPH_NAMESPACE)?)?;
        recover_git_evidence_projection(
            self.graph_runtime()?,
            &identity,
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::measure(label = "global_db.git_correlation.backfill", future = true)]
    pub async fn run_backfill<E, G>(
        &self,
        analytics_events: &[E],
        git: &G,
        opts: &BackfillOptions,
    ) -> Result<BackfillStats, GitCorrelationError>
    where
        E: AnalyticsSessionTimestampSource,
        G: GitReflogSource + ?Sized,
    {
        run_backfill(self, analytics_events, git, opts).await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::measure(
        label = "global_db.git_correlation.incremental_backfill",
        future = true
    )]
    pub async fn run_incremental_backfill<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
        limit_sessions: usize,
    ) -> Result<BackfillStats, GitCorrelationError> {
        run_incremental_backfill(self, git, limit_sessions).await
    }

    #[hotpath::measure(label = "global_db.git_correlation.bounded_history", future = true)]
    pub async fn run_bounded_history_index_page(
        &self,
        opts: &BackfillOptions,
        control: &BoundedGitControl,
    ) -> Result<BoundedBackfillOutcome, GitCorrelationError> {
        run_bounded_history_index_page(self, opts, control).await
    }

    #[hotpath::measure(label = "global_db.git_correlation.health", future = true)]
    pub async fn correlation_index_health(
        &self,
    ) -> Result<CorrelationIndexHealth, GitCorrelationError> {
        let snapshot = self.read_snapshot().await?;
        let backfill_watermark = read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY).await?;
        Ok(match self.git_evidence_projection()? {
            Some(store) => store.health(backfill_watermark),
            // Never published: truthfully report the projection as absent
            // instead of failing the health read.
            None => CorrelationIndexHealth {
                projection_available: false,
                generation: None,
                source_watermark: None,
                span_count: 0,
                commit_count: 0,
                backfill_watermark,
            },
        })
    }

    /// Executes the query and derives bounded presence from the same recovered
    /// projection. This avoids a second projection recovery solely to compute
    /// exact counts before every `sessions_for` read.
    #[hotpath::measure(
        label = "global_db.git_correlation.sessions_for_with_presence",
        future = true
    )]
    pub async fn sessions_for_with_relation_and_presence(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Result<(Vec<SessionGitCorrelationHit>, CorrelationIndexPresence), GitCorrelationError>
    {
        let snapshot = self.read_snapshot().await?;
        let backfill_watermark = read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY).await?;
        Ok(match self.git_evidence_projection()? {
            Some(store) => {
                let presence = store.presence(backfill_watermark);
                let results = store.sessions_for_with_relation(query, relation);
                (results, presence)
            }
            None => (
                Vec::new(),
                CorrelationIndexPresence {
                    projection_available: false,
                    generation: None,
                    source_watermark: None,
                    spans_present: false,
                    commits_present: false,
                    backfill_watermark,
                },
            ),
        })
    }

    #[hotpath::measure(label = "global_db.git_correlation.sessions_for", future = true)]
    pub async fn sessions_for_with_relation(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
        Ok(match self.git_evidence_projection()? {
            Some(store) => store.sessions_for_with_relation(query, relation),
            // No evidence has ever been recorded, so no session correlates.
            None => Vec::new(),
        })
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::measure(label = "global_db.git_correlation.session_ids")]
    pub fn session_ids_for_scope(
        &self,
        filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> Result<std::collections::BTreeSet<(String, String)>, GitCorrelationError> {
        // No published evidence: a valid scope truthfully matches no session.
        let Some(store) = self.git_evidence_projection()? else {
            return Ok(std::collections::BTreeSet::new());
        };
        store
            .session_ids_for_scope(filter)
            .map(|ids| ids.into_iter().collect())
            .ok_or_else(|| {
                GitCorrelationError::Unavailable(
                    "Git evidence scope could not be resolved".to_owned(),
                )
            })
    }
}

impl<D> GitCorrelationSessionStore for GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    type ReadSnapshot = DatabaseEngineReadSnapshot;

    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        GlobalDbGitCorrelationStore::require_project_sessions_authority(self)
    }

    #[hotpath::skip]
    async fn read_snapshot(&self) -> Result<Self::ReadSnapshot, GitCorrelationError> {
        GlobalDbGitCorrelationStore::read_snapshot(self).await
    }

    #[hotpath::skip]
    async fn open_write_transaction(&self) -> Result<Self::WriteTxn<'_>, GitCorrelationError> {
        GlobalDbGitCorrelationStore::open_write_transaction(self).await
    }

    fn git_evidence_publication_lock(&self) -> Result<&Mutex<()>, GitCorrelationError> {
        match &self.graph_publication_lock {
            Some(Ok(lock)) => Ok(lock.as_ref()),
            Some(Err(detail)) => Err(GitCorrelationError::Unavailable(detail.clone())),
            None => Err(GitCorrelationError::Unavailable(
                "registered project graph runtime is not mounted".to_owned(),
            )),
        }
    }

    fn graph_runtime(&self) -> Result<&dyn VerifiedGraphRuntimePortV1, GitCorrelationError> {
        self.graph_runtime
            .as_ref()
            .map(|runtime| runtime as &dyn VerifiedGraphRuntimePortV1)
            .ok_or_else(|| {
                GitCorrelationError::Unavailable(
                    "registered project graph runtime is not mounted".to_owned(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::shared_git_evidence_publication_lock_for_identity;
    use std::sync::Arc;

    #[test]
    fn publication_lock_registry_is_exact_identity_scoped() {
        let first = shared_git_evidence_publication_lock_for_identity(
            "git-evidence-lock-test:shared".to_owned(),
        )
        .unwrap();
        let same = shared_git_evidence_publication_lock_for_identity(
            "git-evidence-lock-test:shared".to_owned(),
        )
        .unwrap();
        let foreign = shared_git_evidence_publication_lock_for_identity(
            "git-evidence-lock-test:foreign".to_owned(),
        )
        .unwrap();

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &foreign));
    }
}
