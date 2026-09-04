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
    AUTO_BACKFILL_WATERMARK_KEY, BackfillOptions, BackfillStats, BoundedBackfillOutcome,
    BoundedGitControl, CommitRelationFilter, CorrelationIndexHealth, CorrelationIndexPresence,
    DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT, GitCorrelationError, GitCorrelationSessionStore,
    GitEvidenceProjectionStore, GitReflogSource, SessionGitCorrelationHit, SessionsForQuery,
    SpanObservation, git_evidence_projection_identity, pending_git_evidence_publication_count,
    publish_transcript_graph_evidence, read_meta_value, recover_git_evidence_projection,
    replay_pending_git_evidence_publications, replay_pending_git_evidence_publications_outcome,
    run_bounded_history_index_page, run_incremental_backfill, run_incremental_backfill_outcome,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::runtime::git_correlation::{
    AnalyticsSessionTimestampSource, run_backfill,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";

type GitEvidencePublicationLock = Mutex<()>;

static GIT_EVIDENCE_PUBLICATION_LOCKS: OnceLock<
    Mutex<BTreeMap<String, Weak<GitEvidencePublicationLock>>>,
> = OnceLock::new();

/// Typed result of one bounded production convergence pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidenceConvergenceStats {
    pub replayed_publications: usize,
    /// Known pending receipt count after replay. `None` means that authority
    /// failed after another phase had already committed progress.
    pub pending_publications: Option<u64>,
    pub backfill: BackfillStats,
    /// Conservative signal: a full page means another retained-history page
    /// may exist and callers must not describe this pass as fully drained.
    pub backfill_page_saturated: bool,
}

impl GitEvidenceConvergenceStats {
    /// Whether this pass durably changed Git evidence or its session frontier.
    pub fn committed_progress(&self) -> bool {
        self.replayed_publications > 0 || self.backfill.committed_progress()
    }
}

/// Truthful result of one bounded convergence attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvidenceConvergenceOutcome {
    Complete(GitEvidenceConvergenceStats),
    Partial {
        progress: GitEvidenceConvergenceStats,
        later_failure: GitCorrelationError,
    },
}

impl GitEvidenceConvergenceOutcome {
    pub const fn stats(&self) -> &GitEvidenceConvergenceStats {
        match self {
            Self::Complete(stats)
            | Self::Partial {
                progress: stats, ..
            } => stats,
        }
    }

    pub const fn later_failure(&self) -> Option<&GitCorrelationError> {
        match self {
            Self::Complete(_) => None,
            Self::Partial { later_failure, .. } => Some(later_failure),
        }
    }

    pub fn committed_progress(&self) -> bool {
        self.stats().committed_progress()
    }
}

fn settle_git_evidence_convergence(
    progress: GitEvidenceConvergenceStats,
    later_failure: Option<GitCorrelationError>,
) -> Result<GitEvidenceConvergenceOutcome, GitCorrelationError> {
    match later_failure {
        Some(later_failure) if progress.committed_progress() => {
            Ok(GitEvidenceConvergenceOutcome::Partial {
                progress,
                later_failure,
            })
        }
        Some(error) => Err(error),
        None => Ok(GitEvidenceConvergenceOutcome::Complete(progress)),
    }
}

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

async fn converge_session_git_evidence<S, G>(
    session_store: &S,
    git: &G,
    backfill_session_limit: usize,
    publication_replay_limit: usize,
) -> Result<GitEvidenceConvergenceOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    G: GitReflogSource + ?Sized,
{
    session_store.require_project_sessions_authority()?;
    if backfill_session_limit == 0 {
        return Err(GitCorrelationError::InvalidArgument(
            "Git evidence convergence backfill limit must be positive".to_owned(),
        ));
    }
    if publication_replay_limit == 0 {
        return Err(GitCorrelationError::InvalidArgument(
            "Git evidence convergence replay limit must be positive".to_owned(),
        ));
    }
    let replay =
        replay_pending_git_evidence_publications_outcome(session_store, publication_replay_limit)
            .await?;
    let replayed_publications = replay.replayed_publications;
    let pending_publications = match pending_git_evidence_publication_count(session_store).await {
        Ok(pending) => Some(pending),
        Err(error) if replayed_publications > 0 => {
            return Ok(GitEvidenceConvergenceOutcome::Partial {
                progress: GitEvidenceConvergenceStats {
                    replayed_publications,
                    pending_publications: None,
                    backfill: BackfillStats::default(),
                    backfill_page_saturated: false,
                },
                later_failure: error,
            });
        }
        Err(error) => return Err(error),
    };
    if let Some(later_failure) = replay.later_failure {
        return settle_git_evidence_convergence(
            GitEvidenceConvergenceStats {
                replayed_publications,
                pending_publications,
                backfill: BackfillStats::default(),
                backfill_page_saturated: false,
            },
            Some(later_failure),
        );
    }
    let backfill_outcome =
        match run_incremental_backfill_outcome(session_store, git, backfill_session_limit).await {
            Ok(outcome) => outcome,
            Err(error) if replayed_publications > 0 => {
                return Ok(GitEvidenceConvergenceOutcome::Partial {
                    progress: GitEvidenceConvergenceStats {
                        replayed_publications,
                        pending_publications,
                        backfill: BackfillStats::default(),
                        backfill_page_saturated: false,
                    },
                    later_failure: error,
                });
            }
            Err(error) => return Err(error),
        };
    let progress = GitEvidenceConvergenceStats {
        replayed_publications,
        pending_publications,
        backfill_page_saturated: backfill_outcome.stats.sessions_scanned == backfill_session_limit,
        backfill: backfill_outcome.stats,
    };
    settle_git_evidence_convergence(progress, backfill_outcome.later_failure)
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

impl RegisteredGlobalDb {
    /// Concrete registered-database entry point for callers with a scoped
    /// borrow. The registered authority implements the session-store port
    /// directly, so host-admission futures retain their original lifetime.
    pub async fn converge_session_git_evidence<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
        backfill_session_limit: usize,
        publication_replay_limit: usize,
    ) -> Result<GitEvidenceConvergenceOutcome, GitCorrelationError> {
        converge_session_git_evidence(self, git, backfill_session_limit, publication_replay_limit)
            .await
    }

    pub async fn replay_pending_git_evidence_publications(
        &self,
    ) -> Result<usize, GitCorrelationError> {
        replay_pending_git_evidence_publications(
            self,
            DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT,
        )
        .await
    }
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

    #[hotpath::measure(label = "global_db.git_correlation.replay_publications", future = true)]
    pub async fn replay_pending_git_evidence_publications(
        &self,
    ) -> Result<usize, GitCorrelationError> {
        replay_pending_git_evidence_publications(
            self,
            DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT,
        )
        .await
    }

    /// Replays already-committed transcript publications first, then advances
    /// exactly one retained-history page. Both budgets are explicit so startup
    /// and admission never turn historical convergence into an unbounded wait.
    #[hotpath::measure(label = "global_db.git_correlation.converge", future = true)]
    pub async fn converge_session_git_evidence<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
        backfill_session_limit: usize,
        publication_replay_limit: usize,
    ) -> Result<GitEvidenceConvergenceOutcome, GitCorrelationError> {
        converge_session_git_evidence(self, git, backfill_session_limit, publication_replay_limit)
            .await
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

    fn git_evidence_publication_lock(&self) -> Result<Arc<Mutex<()>>, GitCorrelationError> {
        match &self.graph_publication_lock {
            Some(Ok(lock)) => Ok(Arc::clone(lock)),
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

impl GitCorrelationSessionStore for RegisteredGlobalDb {
    type ReadSnapshot = DatabaseEngineReadSnapshot;

    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        if matches!(
            &self.binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        ) {
            Ok(())
        } else {
            Err(GitCorrelationError::Db(
                "git correlation requires registered ProjectSessions authority".to_owned(),
            ))
        }
    }

    #[hotpath::skip]
    async fn read_snapshot(&self) -> Result<Self::ReadSnapshot, GitCorrelationError> {
        RegisteredGlobalDb::read_snapshot(self)
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    #[hotpath::skip]
    async fn open_write_transaction(&self) -> Result<Self::WriteTxn<'_>, GitCorrelationError> {
        RegisteredGlobalDb::begin_write_transaction(self)
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    fn git_evidence_publication_lock(&self) -> Result<Arc<Mutex<()>>, GitCorrelationError> {
        let runtime = self.project_graph_runtime().ok_or_else(|| {
            GitCorrelationError::Unavailable(
                "registered project graph runtime is not mounted".to_owned(),
            )
        })?;
        shared_git_evidence_publication_lock(runtime).map_err(GitCorrelationError::Unavailable)
    }

    fn graph_runtime(&self) -> Result<&dyn VerifiedGraphRuntimePortV1, GitCorrelationError> {
        self.project_graph_runtime()
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
    use super::{
        GitEvidenceConvergenceOutcome, GitEvidenceConvergenceStats, GlobalDbGitCorrelationStore,
        settle_git_evidence_convergence, shared_git_evidence_publication_lock_for_identity,
    };
    use crate::{
        ParseOffset, TranscriptPersistenceError, tests::harness::RegisteredGlobalDbHarness,
    };
    use std::sync::Arc;
    use tracedecay_sessions::runtime::SessionRecord;
    use tracedecay_sessions::runtime::git_correlation::{
        GitCorrelationError, SpanObservation, SpanSource, SystemGit,
    };

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

    #[test]
    fn later_failure_returns_committed_partial_convergence() {
        let progress = GitEvidenceConvergenceStats {
            replayed_publications: 1,
            pending_publications: Some(0),
            backfill: Default::default(),
            backfill_page_saturated: false,
        };
        let failure = GitCorrelationError::Unavailable("git log failed".to_owned());

        let outcome = settle_git_evidence_convergence(progress.clone(), Some(failure.clone()))
            .expect("committed work must be returned as partial progress");

        assert_eq!(
            outcome,
            GitEvidenceConvergenceOutcome::Partial {
                progress,
                later_failure: failure,
            }
        );
        assert!(outcome.committed_progress());
    }

    #[tokio::test]
    async fn profile_sessions_authority_cannot_replay_or_backfill_project_git_evidence() {
        let harness = RegisteredGlobalDbHarness::open("git-correlation-profile-isolation").await;
        let store = GlobalDbGitCorrelationStore::new(harness.registered.clone());

        assert!(matches!(
            store.replay_pending_git_evidence_publications().await,
            Err(GitCorrelationError::Db(message))
                if message.contains("ProjectSessions")
        ));
        assert!(matches!(
            store.converge_session_git_evidence(&SystemGit, 1, 1).await,
            Err(GitCorrelationError::Db(message))
                if message.contains("ProjectSessions")
        ));

        let session = SessionRecord {
            provider: "codex".to_owned(),
            session_id: "profile-git-evidence".to_owned(),
            project_key: "user".to_owned(),
            project_path: "user".to_owned(),
            title: None,
            started_at: Some(1),
            ended_at: Some(1),
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let error = harness
            .registered
            .persist_transcript_batch_with_git_evidence_result(
                &session,
                &[],
                "profile-git-evidence.jsonl",
                ParseOffset::default(),
                ParseOffset::default(),
                tracedecay_sessions::runtime::TranscriptGitEvidence::new(
                    "profile-git-evidence",
                    &[],
                    &[SpanObservation {
                        provider: "codex".to_owned(),
                        session_id: session.session_id.clone(),
                        thread_id: None,
                        branch: Some("main".to_owned()),
                        worktree: "/repo".to_owned(),
                        ts: 1,
                        source: SpanSource::Ingest,
                    }],
                ),
            )
            .await
            .expect_err("profile transcript authority must reject project Git evidence");
        assert!(matches!(
            error,
            TranscriptPersistenceError::Storage { operation, source }
                if operation == "stage transcript git evidence"
                    && source.to_string().contains("ProjectSessions")
        ));
        assert!(
            harness
                .registered
                .get_session("codex", "profile-git-evidence")
                .await
                .is_none(),
            "scope rejection must happen before transcript rows commit"
        );
    }
}
