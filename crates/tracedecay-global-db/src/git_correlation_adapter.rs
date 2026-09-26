//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods. Git evidence lives in the project sessions store's own
//! rows, so every read and write goes through its registered database.

use std::borrow::Borrow;
use std::collections::BTreeSet;

use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_store::StoreShardScopeV1;

use crate::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use tracedecay_sessions::runtime::git_correlation::{
    AUTO_BACKFILL_WATERMARK_KEY, BackfillOptions, BoundedBackfillOutcome, BoundedGitControl,
    CommitRelationFilter, CommitSessionRecord, CorrelationIndexHealth, CorrelationIndexPresence,
    GitCorrelationError, GitCorrelationSessionStore, GitCorrelationWriteTxn, GitEvidenceBatch,
    GitEvidencePassOutcome, GitEvidenceWriter, GitReflogSource, GitScopeFilter,
    SessionGitCorrelationHit, SessionGitSpan, SessionsForQuery, SpanObservation,
    converge_git_evidence_pass, open_git_evidence_view, read_meta_value,
    run_bounded_history_index_page,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::runtime::git_correlation::{
    AnalyticsSessionTimestampSource, BackfillStats, run_backfill,
};

/// Git evidence recorded for a bounded set of sessions, bound to the
/// generation that served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSessionEvidence {
    pub generation: String,
    pub spans: Vec<SessionGitSpan>,
    pub commits: Vec<CommitSessionRecord>,
}

fn require_project_sessions(database: &RegisteredGlobalDb) -> Result<(), GitCorrelationError> {
    if matches!(
        &database.binding().shard_id.scope,
        StoreShardScopeV1::ProjectSessions { .. }
    ) {
        Ok(())
    } else {
        Err(GitCorrelationError::Db(
            "git correlation requires registered ProjectSessions authority".to_owned(),
        ))
    }
}

impl RegisteredGlobalDb {
    /// Concrete registered-database entry point for callers with a scoped
    /// borrow. The registered authority implements the session-store port
    /// directly, so host-admission futures retain their original lifetime.
    pub async fn converge_session_git_evidence<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
    ) -> Result<GitEvidencePassOutcome, GitCorrelationError> {
        converge_git_evidence_pass(self, git).await
    }
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
}

impl<D> GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    pub fn new(db: D) -> Self {
        Self { db }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    pub fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        require_project_sessions(self.db())
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

    /// Records one hook-route observation into its session's span rows.
    #[hotpath::measure(label = "global_db.git_correlation.record_span", future = true)]
    pub async fn record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64, GitCorrelationError> {
        self.require_project_sessions_authority()?;
        let transaction = self.open_write_transaction().await?;
        let mut writer = GitEvidenceWriter::open(&transaction).await?;
        let written = writer
            .apply(GitEvidenceBatch {
                observations: vec![observation.clone()],
                merge_gap_secs,
                ..GitEvidenceBatch::default()
            })
            .await?;
        writer.finish().await?;
        GitCorrelationWriteTxn::commit(transaction).await?;
        i64::try_from(written.spans_changed).map_err(|_| {
            GitCorrelationError::Contract("Git evidence span write count exceeds i64".to_owned())
        })
    }

    /// The generation and every span and commit attribution recorded for
    /// `session_ids`. `Ok(None)` means no evidence was ever recorded.
    #[hotpath::measure(label = "global_db.git_correlation.session_evidence", future = true)]
    pub async fn git_evidence_for_sessions(
        &self,
        session_ids: &BTreeSet<String>,
    ) -> Result<Option<GitSessionEvidence>, GitCorrelationError> {
        self.require_project_sessions_authority()?;
        let snapshot = self.read_snapshot().await?;
        let Some(view) = open_git_evidence_view(&snapshot).await? else {
            return Ok(None);
        };
        let (spans, commits) = view.session_evidence(session_ids).await?;
        Ok(Some(GitSessionEvidence {
            generation: view.generation().generation_id(),
            spans,
            commits,
        }))
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

    /// Derives evidence for every retained session past the history frontier
    /// and appends it, with the commit attribution it enables, in one
    /// transaction.
    #[hotpath::measure(label = "global_db.git_correlation.converge", future = true)]
    pub async fn converge_session_git_evidence<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
    ) -> Result<GitEvidencePassOutcome, GitCorrelationError> {
        converge_git_evidence_pass(self, git).await
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
        self.require_project_sessions_authority()?;
        let snapshot = self.read_snapshot().await?;
        let backfill_watermark = read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY).await?;
        Ok(match open_git_evidence_view(&snapshot).await? {
            Some(view) => view.health(backfill_watermark),
            // Never recorded: truthfully report the projection as absent
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

    /// Executes the query and derives presence from the same snapshot, so
    /// both answers describe one generation.
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
        self.require_project_sessions_authority()?;
        let snapshot = self.read_snapshot().await?;
        let backfill_watermark = read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY).await?;
        Ok(match open_git_evidence_view(&snapshot).await? {
            Some(view) => {
                let results = view.sessions_for(query, relation).await?;
                (results, view.presence(backfill_watermark))
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
        self.require_project_sessions_authority()?;
        let snapshot = self.read_snapshot().await?;
        match open_git_evidence_view(&snapshot).await? {
            Some(view) => view.sessions_for(query, relation).await,
            // No evidence has ever been recorded, so no session correlates.
            None => Ok(Vec::new()),
        }
    }

    /// The sessions a Git scope selects. A store that never recorded
    /// evidence cannot prove that no durable session matches, so it answers
    /// typed unavailable rather than an empty set.
    #[hotpath::measure(label = "global_db.git_correlation.scope_session_ids", future = true)]
    pub async fn session_ids_for_scope(
        &self,
        filter: &GitScopeFilter,
        maximum: Option<usize>,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        self.require_project_sessions_authority()?;
        if filter.is_empty() {
            return Ok(None);
        }
        let snapshot = self.read_snapshot().await?;
        let Some(view) = open_git_evidence_view(&snapshot).await? else {
            return Err(GitCorrelationError::Unavailable(
                "Git evidence has not been recorded for this project".to_owned(),
            ));
        };
        match maximum {
            Some(maximum) => view.session_ids_for_scope_bounded(filter, maximum).await,
            None => view.session_ids_for_scope(filter).await,
        }
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
}

impl GitCorrelationSessionStore for RegisteredGlobalDb {
    type ReadSnapshot = DatabaseEngineReadSnapshot;

    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        require_project_sessions(self)
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
}

#[cfg(test)]
mod tests {
    use super::GlobalDbGitCorrelationStore;
    use crate::{
        ParseOffset, TranscriptPersistenceError,
        tests::harness::{RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime},
    };
    use tracedecay_domain::ProjectId;
    use tracedecay_sessions::runtime::SessionRecord;
    use tracedecay_sessions::runtime::git_correlation::{
        CommitRelationFilter, GitCorrelationError, GitRefFilter, GitScopeFilter, SessionsForQuery,
        SpanObservation, SpanSource, SystemGit,
    };

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

    fn branch_query(branch: &str) -> SessionsForQuery {
        SessionsForQuery {
            git_ref: GitRefFilter::Branch(branch.to_owned()),
            since: None,
            until: None,
            limit: 10,
        }
    }

    /// Hook-route observations land in the project store's own rows, and
    /// every read (health, presence, sessions_for, scope, session evidence)
    /// answers from the same generation.
    #[tokio::test]
    async fn recorded_spans_are_read_back_through_every_facade() {
        let root = tempfile::tempdir().unwrap();
        let registered = RegisteredGlobalDbTestRuntime::project(
            root.path().join("profile"),
            root.path().join("project"),
            ProjectId::new("project.git-evidence-rows").unwrap(),
        )
        .await
        .unwrap();
        let store = GlobalDbGitCorrelationStore::new(registered.project_database_arc().unwrap());

        let unrecorded = store.correlation_index_health().await.unwrap();
        assert!(!unrecorded.projection_available);
        assert!(matches!(
            store
                .session_ids_for_scope(
                    &GitScopeFilter {
                        branch: Some("main".to_owned()),
                        worktree: None,
                        commit: None,
                    },
                    None,
                )
                .await,
            Err(GitCorrelationError::Unavailable(_))
        ));

        assert_eq!(
            store
                .record_span_observation(&span_observation(10), 5)
                .await,
            Ok(1)
        );
        assert_eq!(
            store
                .record_span_observation(&span_observation(12), 5)
                .await,
            Ok(1)
        );
        assert_eq!(
            store
                .record_span_observation(&span_observation(12), 5)
                .await,
            Ok(0),
            "a repeated observation changes no row"
        );

        let health = store.correlation_index_health().await.unwrap();
        assert!(health.projection_available);
        assert_eq!((health.span_count, health.commit_count), (1, 0));
        assert_eq!(
            health.source_watermark.as_deref(),
            Some("git-evidence-sequence:2")
        );

        let (hits, presence) = store
            .sessions_for_with_relation_and_presence(
                &branch_query("main"),
                CommitRelationFilter::Produced,
            )
            .await
            .unwrap();
        assert!(presence.spans_present && !presence.commits_present);
        assert_eq!(presence.generation, health.generation);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "session.git-evidence-runtime");
        assert_eq!((hits[0].first_ts, hits[0].last_ts), (Some(10), Some(12)));
        assert_eq!(
            store
                .sessions_for_with_relation(&branch_query("elsewhere"), CommitRelationFilter::All)
                .await
                .unwrap(),
            Vec::new()
        );
        assert_eq!(
            store
                .session_ids_for_scope(
                    &GitScopeFilter {
                        branch: Some("main".to_owned()),
                        worktree: Some("/repo".to_owned()),
                        commit: None,
                    },
                    None,
                )
                .await
                .unwrap(),
            Some(vec![(
                "codex".to_owned(),
                "session.git-evidence-runtime".to_owned()
            )])
        );
        let evidence = store
            .git_evidence_for_sessions(&std::collections::BTreeSet::from([
                "session.git-evidence-runtime".to_owned(),
            ]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(Some(evidence.generation), health.generation);
        assert_eq!(evidence.spans.len(), 1);
        assert_eq!(evidence.commits, Vec::new());
    }

    #[tokio::test]
    async fn profile_sessions_authority_cannot_record_project_git_evidence() {
        let harness = RegisteredGlobalDbHarness::open("git-correlation-profile-isolation").await;
        let store = GlobalDbGitCorrelationStore::new(harness.registered.clone());

        assert!(matches!(
            store.converge_session_git_evidence(&SystemGit).await,
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
                if operation == "record transcript git evidence"
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
