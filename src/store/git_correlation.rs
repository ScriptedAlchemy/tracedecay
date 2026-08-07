//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods.

use std::borrow::Borrow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sha2::{Digest as _, Sha256};
use tracedecay_graph_db::GraphNamespace;
use tracedecay_store::StoreShardScopeV1;

use crate::db::engine::ReadSnapshot;
use crate::global_db::{
    ProjectGraphRuntimePortV1, RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction,
};
use crate::sessions::git_correlation::{
    AUTO_BACKFILL_WATERMARK_KEY, AnalyticsSessionTimestampSource, BackfillOptions, BackfillStats,
    BoundedBackfillOutcome, BoundedGitControl, CommitRelationFilter, CommitSessionRecord,
    CorrelationIndexHealth, DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError,
    GitCorrelationSessionStore, GitEvidenceGraphRuntimePort, GitEvidenceProjectionStore,
    GitReflogSource, SessionGitCorrelationHit, SessionGitSpan, SessionsForQuery, SpanObservation,
    git_evidence_projection_identity, graph_evidence_publication_key, normalize_worktree,
    observation_extends_span, providers_compatible, publish_graph_evidence, read_meta_value,
    recover_git_evidence_projection, run_backfill, run_bounded_history_index_page,
    run_incremental_backfill,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";

struct RegisteredProjectGraphRuntime(Arc<dyn ProjectGraphRuntimePortV1>);

impl GitEvidenceGraphRuntimePort for RegisteredProjectGraphRuntime {
    fn publish_verified_manifest(
        &self,
        manifest: &tracedecay_graph_db::GraphGenerationManifest,
        idempotency_key: tracedecay_graph_db::GraphIdempotencyKey,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<tracedecay_graph_db::VerifiedGraphSnapshot, tracedecay_graph_db::GraphDbError> {
        self.0
            .publish_verified_manifest(manifest, idempotency_key, cancelled)
    }

    fn verified_snapshot(
        &self,
        projection: &tracedecay_graph_db::GraphProjectionIdentity,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Option<tracedecay_graph_db::VerifiedGraphSnapshot>, tracedecay_graph_db::GraphDbError>
    {
        self.0.verified_snapshot(projection, cancelled)
    }
}

/// Adapter over an already-open project-sessions database.
///
/// The holder `D` is generic so callers that own an `Arc<RegisteredGlobalDb>`
/// can build a lifetime-free (`'static`) adapter. A borrowed adapter makes the
/// `GitCorrelationSessionStore` impl apply only "for some specific lifetime",
/// so any future that holds one across an await and must then prove `Send`
/// raises a higher-ranked `for<'0> GlobalDbGitCorrelationStore<'0>: …`
/// obligation the compiler cannot discharge. Owning the handle keeps the impl
/// lifetime-free. Borrowed holders remain supported for call sites that never
/// cross such a boundary.
pub struct GlobalDbGitCorrelationStore<D> {
    db: D,
    graph_runtime: Option<RegisteredProjectGraphRuntime>,
}

impl<D> GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    pub(crate) fn new(db: D) -> Self {
        let graph_runtime = db
            .borrow()
            .project_graph_runtime()
            .cloned()
            .map(RegisteredProjectGraphRuntime);
        Self { db, graph_runtime }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    pub(crate) fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
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

    pub(crate) async fn read_snapshot(&self) -> Result<ReadSnapshot, GitCorrelationError> {
        self.db()
            .read_snapshot()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    pub(crate) async fn open_write_transaction(
        &self,
    ) -> Result<RegisteredGlobalDbWriteTransaction<'_>, GitCorrelationError> {
        self.db()
            .begin_write_transaction()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    pub(crate) async fn record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64, GitCorrelationError> {
        let spans = self.transcript_spans(std::slice::from_ref(observation), merge_gap_secs)?;
        let publication_key = graph_evidence_publication_key("hook-route-span", &spans, &[])?;
        let (changed, _) = publish_graph_evidence(self, &publication_key, &spans, &[])?;
        i64::try_from(changed).map_err(|_| {
            GitCorrelationError::Contract(
                "Git evidence span publication count exceeds i64".to_owned(),
            )
        })
    }

    pub(crate) fn publish_transcript_evidence(
        &self,
        publication_prefix: &str,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> Result<(), GitCorrelationError> {
        if commit_records.is_empty() && span_observations.is_empty() {
            return Ok(());
        }
        let spans = self.transcript_spans(span_observations, DEFAULT_SPAN_MERGE_GAP_SECS)?;
        let publication_key =
            graph_evidence_publication_key(publication_prefix, &spans, commit_records)?;
        publish_graph_evidence(self, &publication_key, &spans, commit_records)?;
        Ok(())
    }

    fn transcript_spans(
        &self,
        observations: &[SpanObservation],
        merge_gap_secs: i64,
    ) -> Result<Vec<SessionGitSpan>, GitCorrelationError> {
        // A never-published projection is the typed empty start.
        let current = match self.git_evidence_projection()? {
            Some(store) => store.projection().spans().to_vec(),
            None => Vec::new(),
        };
        let mut candidates: Vec<SessionGitSpan> = Vec::new();
        for observation in observations {
            let worktree = normalize_worktree(&observation.worktree);
            let existing = candidates.iter().chain(current.iter()).find(|span| {
                providers_compatible(&span.provider, &observation.provider)
                    && span.session_id == observation.session_id
                    && span.thread_id == observation.thread_id
                    && span.branch == observation.branch
                    && span.worktree == worktree
                    && span.source == observation.source
                    && observation_extends_span(
                        span.first_ts,
                        span.last_ts,
                        observation.ts,
                        merge_gap_secs,
                    )
            });
            let span = match existing {
                Some(existing) => {
                    let mut span = existing.clone();
                    if span.provider.is_empty() && !observation.provider.is_empty() {
                        span.provider.clone_from(&observation.provider);
                    }
                    let extends = observation.ts < span.first_ts || observation.ts > span.last_ts;
                    span.first_ts = span.first_ts.min(observation.ts);
                    span.last_ts = span.last_ts.max(observation.ts);
                    if extends {
                        span.event_count = span.event_count.saturating_add(1);
                    }
                    span
                }
                None => SessionGitSpan {
                    span_id: transcript_span_id(observation, &worktree),
                    provider: observation.provider.clone(),
                    session_id: observation.session_id.clone(),
                    thread_id: observation.thread_id.clone(),
                    branch: observation.branch.clone(),
                    worktree,
                    first_ts: observation.ts,
                    last_ts: observation.ts,
                    event_count: 1,
                    source: observation.source,
                },
            };
            if let Some(candidate) = candidates
                .iter_mut()
                .find(|candidate| candidate.span_id == span.span_id)
            {
                *candidate = span;
            } else {
                candidates.push(span);
            }
        }
        Ok(candidates)
    }

    /// `Ok(None)` means the projection has never published a verified head:
    /// the project has no recorded Git evidence yet.
    fn git_evidence_projection(
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

    pub(crate) async fn run_backfill<E, G>(
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

    pub(crate) async fn run_incremental_backfill<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
        limit_sessions: usize,
    ) -> Result<BackfillStats, GitCorrelationError> {
        run_incremental_backfill(self, git, limit_sessions).await
    }

    pub(crate) async fn run_bounded_history_index_page(
        &self,
        opts: &BackfillOptions,
        control: &BoundedGitControl,
    ) -> Result<BoundedBackfillOutcome, GitCorrelationError> {
        run_bounded_history_index_page(self, opts, control).await
    }

    pub(crate) async fn correlation_index_health(
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

    pub(crate) async fn sessions_for_with_relation(
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

    pub(crate) fn session_ids_for_scope(
        &self,
        filter: &crate::sessions::git_correlation::GitScopeFilter,
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

fn transcript_span_id(observation: &SpanObservation, worktree: &str) -> String {
    let thread_id = observation.thread_id.as_deref().unwrap_or_default();
    let branch = observation.branch.as_deref().unwrap_or_default();
    let material = format!(
        "{}\0{}\0{thread_id}\0{branch}\0{}\0{}\0{:?}",
        observation.provider, observation.session_id, worktree, observation.ts, observation.source,
    );
    format!(
        "transcript:{}",
        hex::encode(Sha256::digest(material.as_bytes()))
    )
}

impl<D> GitCorrelationSessionStore for GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        GlobalDbGitCorrelationStore::require_project_sessions_authority(self)
    }

    async fn read_snapshot(&self) -> Result<ReadSnapshot, GitCorrelationError> {
        GlobalDbGitCorrelationStore::read_snapshot(self).await
    }

    async fn open_write_transaction(&self) -> Result<Self::WriteTxn<'_>, GitCorrelationError> {
        GlobalDbGitCorrelationStore::open_write_transaction(self).await
    }

    fn graph_runtime(&self) -> Result<&dyn GitEvidenceGraphRuntimePort, GitCorrelationError> {
        self.graph_runtime
            .as_ref()
            .map(|runtime| runtime as &dyn GitEvidenceGraphRuntimePort)
            .ok_or_else(|| {
                GitCorrelationError::Unavailable(
                    "registered project graph runtime is not mounted".to_owned(),
                )
            })
    }
}
