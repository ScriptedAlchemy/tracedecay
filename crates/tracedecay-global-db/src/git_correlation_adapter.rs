//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::{Semaphore, oneshot};
use tracedecay_graph_db::GraphNamespace;
use tracedecay_runtime_core::RuntimeOperationTaskOwnerV1;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_store::StoreShardScopeV1;

use crate::{
    RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, VerifiedGraphRuntimePortV1,
    VerifiedGraphRuntimeWeakProxyV1,
};
use tracedecay_sessions::runtime::git_correlation::{
    AUTO_BACKFILL_WATERMARK_KEY, BackfillOptions, BackfillStats, BoundedBackfillOutcome,
    BoundedGitControl, CommitRelationFilter, CommitSessionRecord, CorrelationIndexHealth,
    CorrelationIndexPresence, DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT, GitCorrelationError,
    GitCorrelationSessionStore, GitEvidenceProjectionStore, GitReflogSource,
    SessionGitCorrelationHit, SessionGitSpan, SessionsForQuery, SpanObservation,
    git_evidence_projection_identity, pending_git_evidence_publication_count, read_meta_value,
    recover_git_evidence_projection, replay_pending_git_evidence_publications,
    replay_pending_git_evidence_publications_outcome, run_bounded_history_index_page,
    run_incremental_backfill, run_incremental_backfill_outcome,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::runtime::git_correlation::{
    AnalyticsSessionTimestampSource, run_backfill,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";
const GIT_EVIDENCE_PUBLICATION_ADMISSION: usize = 1;

type GitEvidencePublicationLock = Mutex<()>;

struct GitEvidencePublicationAuthority {
    lock: Arc<GitEvidencePublicationLock>,
    admission: Arc<Semaphore>,
}

struct GitEvidencePublicationAuthorityRoutes {
    lock: Weak<GitEvidencePublicationLock>,
    admission: Weak<Semaphore>,
}

static GIT_EVIDENCE_PUBLICATION_AUTHORITIES: OnceLock<
    Mutex<BTreeMap<String, GitEvidencePublicationAuthorityRoutes>>,
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

fn shared_git_evidence_publication_authority(
    runtime: &VerifiedGraphRuntimeWeakProxyV1,
) -> Result<Arc<GitEvidencePublicationAuthority>, String> {
    let identity = serde_json::to_string(&(
        runtime.relational_binding(),
        runtime.relational_verified_locator(),
    ))
    .map_err(|error| format!("encode Git evidence graph runtime identity: {error}"))?;
    shared_git_evidence_publication_authority_for_identity(identity)
}

fn shared_git_evidence_publication_authority_for_identity(
    identity: String,
) -> Result<Arc<GitEvidencePublicationAuthority>, String> {
    let registry = GIT_EVIDENCE_PUBLICATION_AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut authorities = registry
        .lock()
        .map_err(|_| "Git evidence publication authority registry is poisoned".to_owned())?;
    authorities
        .retain(|_, routes| routes.lock.strong_count() > 0 || routes.admission.strong_count() > 0);
    if let Some(routes) = authorities.get_mut(&identity)
        && let Some(lock) = routes.lock.upgrade()
    {
        let admission = routes
            .admission
            .upgrade()
            .unwrap_or_else(|| Arc::new(Semaphore::new(GIT_EVIDENCE_PUBLICATION_ADMISSION)));
        routes.admission = Arc::downgrade(&admission);
        return Ok(Arc::new(GitEvidencePublicationAuthority {
            lock,
            admission,
        }));
    }
    let authority = Arc::new(GitEvidencePublicationAuthority {
        lock: Arc::new(Mutex::new(())),
        admission: Arc::new(Semaphore::new(GIT_EVIDENCE_PUBLICATION_ADMISSION)),
    });
    authorities.insert(
        identity,
        GitEvidencePublicationAuthorityRoutes {
            lock: Arc::downgrade(&authority.lock),
            admission: Arc::downgrade(&authority.admission),
        },
    );
    Ok(authority)
}

async fn publish_owned_git_evidence<T>(
    publication_authority: Arc<GitEvidencePublicationAuthority>,
    operation_task_owner: Arc<RuntimeOperationTaskOwnerV1>,
    operation: impl FnOnce(&GitEvidencePublicationLock) -> Result<T, GitCorrelationError>
    + Send
    + 'static,
) -> Result<T, GitCorrelationError>
where
    T: Send + 'static,
{
    let permit = Arc::clone(&publication_authority.admission)
        .acquire_owned()
        .await
        .map_err(|_| {
            GitCorrelationError::Unavailable(
                "Git evidence publication admission is closed".to_owned(),
            )
        })?;
    let publication_lock = Arc::clone(&publication_authority.lock);
    let (result_tx, result_rx) = oneshot::channel();
    // Keep private owners alive after every registered facade and caller-side
    // receiver has been dropped, until this wrapper joins its blocking child.
    let retained_operation_task_owner = Arc::clone(&operation_task_owner);
    if !operation_task_owner.retain(async move {
        let blocking_child = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(publication_lock.as_ref())
        });
        let joined = blocking_child.await;
        if let Err(detached) = result_tx.send(joined) {
            match detached {
                Ok(Err(error)) => {
                    tracing::error!(
                        event = "git_evidence_operation_detached_failed",
                        error = %error,
                        "detached Git evidence operation returned a domain error while lifecycle ownership settled it"
                    );
                }
                Err(error) => {
                    tracing::error!(
                        event = "git_evidence_operation_detached_join_failed",
                        error = %error,
                        panic = error.is_panic(),
                        "detached Git evidence operation failed while lifecycle ownership settled it"
                    );
                }
                Ok(Ok(_)) => {}
            }
        }
        drop(retained_operation_task_owner);
    }) {
        return Err(GitCorrelationError::Unavailable(
            "Git evidence operation settlement admission is closed".to_owned(),
        ));
    }
    let joined = result_rx
        .await
        .map_err(|_| GitCorrelationError::Cancelled)?;
    settle_git_evidence_blocking_join(joined)?
}

fn settle_git_evidence_blocking_join<T>(
    joined: Result<T, tokio::task::JoinError>,
) -> Result<T, GitCorrelationError> {
    match joined {
        Ok(outcome) => Ok(outcome),
        Err(error) => match error.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => Err(GitCorrelationError::Cancelled),
        },
    }
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
    graph_publication_authority: Option<Result<Arc<GitEvidencePublicationAuthority>, String>>,
    operation_task_owner: Arc<RuntimeOperationTaskOwnerV1>,
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
        let graph_publication_authority = graph_runtime
            .as_ref()
            .map(shared_git_evidence_publication_authority);
        let operation_task_owner = db.borrow().operation_task_owner();
        Self {
            db,
            graph_runtime,
            graph_publication_authority,
            operation_task_owner,
        }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    fn owned_publication_authority(
        &self,
    ) -> Result<
        (
            VerifiedGraphRuntimeWeakProxyV1,
            Arc<GitEvidencePublicationAuthority>,
            Arc<RuntimeOperationTaskOwnerV1>,
        ),
        GitCorrelationError,
    > {
        self.require_project_sessions_authority()?;
        let runtime = self.graph_runtime.clone().ok_or_else(|| {
            GitCorrelationError::Unavailable(
                "registered project graph runtime is not mounted".to_owned(),
            )
        })?;
        let publication_authority = match &self.graph_publication_authority {
            Some(Ok(authority)) => Arc::clone(authority),
            Some(Err(detail)) => {
                return Err(GitCorrelationError::Unavailable(detail.clone()));
            }
            None => {
                return Err(GitCorrelationError::Unavailable(
                    "registered project graph runtime is not mounted".to_owned(),
                ));
            }
        };
        Ok((
            runtime,
            publication_authority,
            Arc::clone(&self.operation_task_owner),
        ))
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
        let (changed, _) = self
            .publish_transcript_graph_evidence_owned(
                "hook-route-span".to_owned(),
                vec![observation.clone()],
                Vec::new(),
                merge_gap_secs,
            )
            .await?;
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

    fn publish_graph_evidence_owned(
        &self,
        publication_prefix: String,
        new_spans: Vec<SessionGitSpan>,
        new_commits: Vec<CommitSessionRecord>,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send {
        let publication_authority = self.owned_publication_authority();
        async move {
            let (runtime, publication_authority, operation_task_owner) = publication_authority?;
            publish_owned_git_evidence(
                publication_authority,
                operation_task_owner,
                move |publication_lock| {
                    GitEvidenceProjectionStore::publish_graph_evidence_with_runtime(
                        &runtime,
                        publication_lock,
                        &publication_prefix,
                        &new_spans,
                        &new_commits,
                        Arc::new(AtomicBool::new(false)),
                    )
                },
            )
            .await
        }
    }

    fn publish_transcript_graph_evidence_owned(
        &self,
        publication_prefix: String,
        observations: Vec<SpanObservation>,
        new_commits: Vec<CommitSessionRecord>,
        merge_gap_secs: i64,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send {
        let publication_authority = self.owned_publication_authority();
        async move {
            let (runtime, publication_authority, operation_task_owner) = publication_authority?;
            publish_owned_git_evidence(
                publication_authority,
                operation_task_owner,
                move |publication_lock| {
                    GitEvidenceProjectionStore::publish_transcript_graph_evidence_with_runtime(
                        &runtime,
                        publication_lock,
                        &publication_prefix,
                        &observations,
                        &new_commits,
                        merge_gap_secs,
                    )
                },
            )
            .await
        }
    }

    fn git_evidence_publication_lock(&self) -> Result<Arc<Mutex<()>>, GitCorrelationError> {
        match &self.graph_publication_authority {
            Some(Ok(authority)) => Ok(Arc::clone(&authority.lock)),
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

    fn publish_graph_evidence_owned(
        &self,
        publication_prefix: String,
        new_spans: Vec<SessionGitSpan>,
        new_commits: Vec<CommitSessionRecord>,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send {
        let publication_authority = self.require_project_sessions_authority().and_then(|()| {
            let runtime = self.project_graph_runtime().cloned().ok_or_else(|| {
                GitCorrelationError::Unavailable(
                    "registered project graph runtime is not mounted".to_owned(),
                )
            })?;
            let publication_authority = shared_git_evidence_publication_authority(&runtime)
                .map_err(GitCorrelationError::Unavailable)?;
            Ok((runtime, publication_authority, self.operation_task_owner()))
        });
        async move {
            let (runtime, publication_authority, operation_task_owner) = publication_authority?;
            publish_owned_git_evidence(
                publication_authority,
                operation_task_owner,
                move |publication_lock| {
                    GitEvidenceProjectionStore::publish_graph_evidence_with_runtime(
                        &runtime,
                        publication_lock,
                        &publication_prefix,
                        &new_spans,
                        &new_commits,
                        Arc::new(AtomicBool::new(false)),
                    )
                },
            )
            .await
        }
    }

    fn publish_transcript_graph_evidence_owned(
        &self,
        publication_prefix: String,
        observations: Vec<SpanObservation>,
        new_commits: Vec<CommitSessionRecord>,
        merge_gap_secs: i64,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send {
        let publication_authority = self.require_project_sessions_authority().and_then(|()| {
            let runtime = self.project_graph_runtime().cloned().ok_or_else(|| {
                GitCorrelationError::Unavailable(
                    "registered project graph runtime is not mounted".to_owned(),
                )
            })?;
            let publication_authority = shared_git_evidence_publication_authority(&runtime)
                .map_err(GitCorrelationError::Unavailable)?;
            Ok((runtime, publication_authority, self.operation_task_owner()))
        });
        async move {
            let (runtime, publication_authority, operation_task_owner) = publication_authority?;
            publish_owned_git_evidence(
                publication_authority,
                operation_task_owner,
                move |publication_lock| {
                    GitEvidenceProjectionStore::publish_transcript_graph_evidence_with_runtime(
                        &runtime,
                        publication_lock,
                        &publication_prefix,
                        &observations,
                        &new_commits,
                        merge_gap_secs,
                    )
                },
            )
            .await
        }
    }

    fn git_evidence_publication_lock(&self) -> Result<Arc<Mutex<()>>, GitCorrelationError> {
        let runtime = self.project_graph_runtime().ok_or_else(|| {
            GitCorrelationError::Unavailable(
                "registered project graph runtime is not mounted".to_owned(),
            )
        })?;
        shared_git_evidence_publication_authority(runtime)
            .map(|authority| Arc::clone(&authority.lock))
            .map_err(GitCorrelationError::Unavailable)
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
        publish_owned_git_evidence, settle_git_evidence_convergence,
        shared_git_evidence_publication_authority_for_identity,
    };
    use crate::{
        ParseOffset, TranscriptPersistenceError,
        tests::harness::{RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime},
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::Notify;
    use tracedecay_domain::ProjectId;
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
        NeverCancelled, VerifiedGraphSnapshot,
    };
    use tracedecay_runtime_core::RuntimeOperationTaskOwnerV1;
    use tracedecay_runtime_core::db::{
        Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
        TestRuntimeProfileIdentityV1,
    };
    use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
    use tracedecay_sessions::runtime::SessionRecord;
    use tracedecay_sessions::runtime::git_correlation::{
        GitCorrelationError, SpanObservation, SpanSource, SystemGit,
    };
    use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

    const BLOCKING_GRAPH_TEST_DEADLINE: Duration = Duration::from_secs(5);

    struct BlockingGitEvidenceRuntime {
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        snapshot: Mutex<Option<VerifiedGraphSnapshot>>,
        snapshot_release: Mutex<Option<mpsc::Receiver<()>>>,
        snapshot_started: Notify,
        fail_publication: bool,
        publication_failed: AtomicBool,
    }

    impl VerifiedGraphRuntimePortV1 for BlockingGitEvidenceRuntime {
        fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.locator
        }

        fn cancel_reconciliation(&self) {}

        fn publish_verified_manifest(
            &self,
            manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
            _cancelled: Arc<AtomicBool>,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            if self.fail_publication {
                self.publication_failed.store(true, Ordering::Release);
                return Err(GraphDbError::unavailable(
                    "injected detached Git evidence publication failure",
                ));
            }
            let snapshot =
                VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
            *self.snapshot.lock().unwrap() = Some(snapshot.clone());
            Ok(snapshot)
        }

        fn reconcile_verified_manifest(
            &self,
            manifest: &GraphGenerationManifest,
            idempotency_key: GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            self.publish_verified_manifest(
                manifest,
                idempotency_key,
                Arc::new(AtomicBool::new(false)),
            )
        }

        fn verified_snapshot(
            &self,
            projection: &GraphProjectionIdentity,
            read_control: FactReadControl,
        ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
            if let Some(release) = self.snapshot_release.lock().unwrap().take() {
                self.snapshot_started.notify_one();
                release
                    .recv_timeout(BLOCKING_GRAPH_TEST_DEADLINE)
                    .map_err(|_| GraphDbError::DeadlineExceeded)?;
            }
            if read_control.interrupted() {
                return Err(GraphDbError::Cancelled);
            }
            Ok(self
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .filter(|snapshot| snapshot.projection() == projection)
                .cloned())
        }
    }

    struct GitEvidenceRuntimeFixture {
        _root: TempDir,
        _registered: RegisteredGlobalDbTestRuntime,
        _graph_database: Database,
        runtime: Arc<BlockingGitEvidenceRuntime>,
        store: GlobalDbGitCorrelationStore<crate::RegisteredGlobalDbLeaseV1>,
        operation_task_owner: Arc<RuntimeOperationTaskOwnerV1>,
    }

    impl GitEvidenceRuntimeFixture {
        async fn open(
            label: &str,
            snapshot_release: mpsc::Receiver<()>,
            fail_publication: bool,
        ) -> Self {
            let root = tempfile::tempdir().unwrap();
            let project_id = ProjectId::new(format!("project.git-evidence-{label}")).unwrap();
            let profile_root = root.path().join("profile");
            let project_root = root.path().join("project");
            let registered = RegisteredGlobalDbTestRuntime::project(
                &profile_root,
                &project_root,
                project_id.clone(),
            )
            .await
            .unwrap();
            let database = registered.project_database_arc().unwrap();
            let shard = &database.binding().shard_id;
            let profile_identity =
                TestRuntimeProfileIdentityV1::new(shard.brain_id.clone(), shard.profile_id.clone());
            let graph_path = root.path().join("project-graph.db");
            let graph_authority =
                DatabaseAuthority::acquire_test(&graph_path, "Git evidence runtime fixture")
                    .unwrap();
            let (graph_database, _) =
                Database::publish_registered_test_runtime_for_profile_identity(
                    &graph_path,
                    &graph_authority,
                    TestDatabaseRuntimeMode::Initialize,
                    profile_identity,
                    TestDatabaseRuntimeScope::Project { project_id },
                )
                .await
                .unwrap();
            let runtime = Arc::new(BlockingGitEvidenceRuntime {
                binding: graph_database.registered_binding().clone(),
                locator: graph_database.registered_verified_locator().clone(),
                snapshot: Mutex::new(None),
                snapshot_release: Mutex::new(Some(snapshot_release)),
                snapshot_started: Notify::new(),
                fail_publication,
                publication_failed: AtomicBool::new(false),
            });
            let graph_runtime: Arc<dyn VerifiedGraphRuntimePortV1> = runtime.clone();
            graph_database
                .bind_memory_graph_runtime(graph_runtime)
                .unwrap();
            assert!(
                database
                    .bind_project_graph_runtime(graph_database.memory_graph_runtime().unwrap())
                    .is_ok(),
                "bind Git evidence graph runtime"
            );
            let operation_task_owner = database.operation_task_owner();
            let store = GlobalDbGitCorrelationStore::new(database);
            Self {
                _root: root,
                _registered: registered,
                _graph_database: graph_database,
                runtime,
                store,
                operation_task_owner,
            }
        }
    }

    fn span_observation(ts: i64) -> SpanObservation {
        SpanObservation {
            provider: "codex".to_owned(),
            session_id: "session.git-evidence-runtime".to_owned(),
            thread_id: Some("thread.git-evidence-runtime".to_owned()),
            branch: Some("main".to_owned()),
            worktree: "/repo".to_owned(),
            ts,
            source: SpanSource::HookRoute,
        }
    }

    #[test]
    fn publication_authority_registry_is_exact_identity_scoped() {
        let first = shared_git_evidence_publication_authority_for_identity(
            "git-evidence-lock-test:shared".to_owned(),
        )
        .unwrap();
        let same = shared_git_evidence_publication_authority_for_identity(
            "git-evidence-lock-test:shared".to_owned(),
        )
        .unwrap();
        let foreign = shared_git_evidence_publication_authority_for_identity(
            "git-evidence-lock-test:foreign".to_owned(),
        )
        .unwrap();

        assert!(Arc::ptr_eq(&first.lock, &same.lock));
        assert!(Arc::ptr_eq(&first.admission, &same.admission));
        assert!(!Arc::ptr_eq(&first.lock, &foreign.lock));

        let retained_lock = Arc::clone(&first.lock);
        drop(first);
        drop(same);
        let rebound = shared_git_evidence_publication_authority_for_identity(
            "git-evidence-lock-test:shared".to_owned(),
        )
        .unwrap();
        assert!(
            Arc::ptr_eq(&retained_lock, &rebound.lock),
            "a synchronous lock holder must keep the canonical publication lock"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publication_admission_precedes_blocking_child_spawn() {
        let authority = shared_git_evidence_publication_authority_for_identity(
            "git-evidence-admission-test".to_owned(),
        )
        .unwrap();
        let held = Arc::clone(&authority.admission)
            .acquire_owned()
            .await
            .unwrap();
        let operation_task_owner = Arc::new(RuntimeOperationTaskOwnerV1::new());
        let started = Arc::new(AtomicBool::new(false));
        let operation_started = Arc::clone(&started);
        let publication =
            publish_owned_git_evidence(authority, Arc::clone(&operation_task_owner), move |_| {
                operation_started.store(true, Ordering::Release);
                Ok(())
            });
        tokio::pin!(publication);
        tokio::select! {
            result = &mut publication => {
                panic!("publication bypassed held admission: {result:?}");
            }
            _ = tokio::task::yield_now() => {}
        }
        assert!(
            !started.load(Ordering::Acquire),
            "blocking child must not spawn before Git publication admission"
        );

        drop(held);
        publication.await.unwrap();
        operation_task_owner.shutdown().await.unwrap();
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

    #[tokio::test(flavor = "current_thread")]
    async fn span_observation_recovery_merge_and_cas_leave_current_thread_runtime_live() {
        let (release, blocked) = mpsc::channel();
        let fixture = GitEvidenceRuntimeFixture::open("current-thread", blocked, false).await;
        let runtime = Arc::clone(&fixture.runtime);
        let holder = tokio::spawn(async move {
            runtime.snapshot_started.notified().await;
            release.send(()).is_ok()
        });

        assert_eq!(
            fixture
                .store
                .record_span_observation(&span_observation(10), 5)
                .await,
            Ok(1)
        );
        assert!(holder.await.unwrap());
        assert_eq!(
            fixture
                .store
                .record_span_observation(&span_observation(12), 5)
                .await,
            Ok(1)
        );

        let projection = fixture
            .store
            .git_evidence_projection()
            .unwrap()
            .expect("verified Git evidence");
        assert_eq!(projection.projection().spans().len(), 1);
        assert_eq!(projection.projection().spans()[0].first_ts, 10);
        assert_eq!(projection.projection().spans()[0].last_ts, 12);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn cancelled_one_worker_span_caller_is_joined_before_owner_shutdown() {
        let (release, blocked) = mpsc::channel();
        let fixture =
            GitEvidenceRuntimeFixture::open("one-worker-cancellation", blocked, true).await;
        let GitEvidenceRuntimeFixture {
            _root,
            _registered,
            _graph_database,
            runtime,
            store,
            operation_task_owner,
        } = fixture;
        let caller = tokio::spawn(async move {
            store
                .record_span_observation(&span_observation(20), 5)
                .await
        });
        runtime.snapshot_started.notified().await;

        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        operation_task_owner.begin_shutdown();
        let shutdown = tokio::spawn({
            let operation_task_owner = Arc::clone(&operation_task_owner);
            async move { operation_task_owner.shutdown().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_finished(),
            "operation-owner shutdown must retain the blocked Git publication"
        );

        release.send(()).unwrap();
        shutdown
            .await
            .unwrap()
            .expect("operation-owner shutdown joins detached Git publication");
        assert!(
            runtime.publication_failed.load(Ordering::Acquire),
            "detached domain failure must occur before shutdown reports settlement"
        );
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
