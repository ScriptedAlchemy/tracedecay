//! Durable per-session Git evidence rows in the project sessions store.
//!
//! Every span and commit attribution is one row keyed by its session, written
//! in the caller's transaction and never rewritten wholesale: a write loads
//! only the sessions it touches, merges into them, and upserts the rows that
//! changed. The projection generation is metadata over those rows (a sequence,
//! a digest chained over each change, and row counts), so its size does not
//! grow with the number of sessions and nothing replays the whole projection.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

use super::attribution::{merge_commit, merge_span, transcript_spans_from_observations};
use super::{
    CommitRelationFilter, CommitSessionRecord, CorrelationIndexHealth, CorrelationIndexPresence,
    GitCorrelationError, GitRefFilter, GitScopeFilter, SessionGitCorrelationHit, SessionGitSpan,
    SessionsForQuery, SpanObservation, canonical_providers, commit_hits,
    commit_identities_with_producer_fallback, commit_record_matches_query, commit_record_order,
    digest_bytes, intersect_id_maps, normalize_worktree, parse_commit_sha, scope_session_ids,
    sessions_for_limit, span_hits, span_identities, span_matches_query, validate_commit_record,
    validate_span,
};

pub(super) const GIT_EVIDENCE_ROWS_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS git_evidence_span (
        span_id TEXT PRIMARY KEY CHECK(length(span_id) > 0),
        session_id TEXT NOT NULL CHECK(length(session_id) > 0),
        provider TEXT NOT NULL,
        branch TEXT,
        worktree TEXT NOT NULL,
        first_ts INTEGER NOT NULL,
        last_ts INTEGER NOT NULL,
        sequence INTEGER NOT NULL,
        record TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_git_evidence_span_session
        ON git_evidence_span(session_id);
    CREATE INDEX IF NOT EXISTS idx_git_evidence_span_branch
        ON git_evidence_span(branch, last_ts, span_id);
    CREATE INDEX IF NOT EXISTS idx_git_evidence_span_worktree
        ON git_evidence_span(worktree, last_ts, span_id);
    CREATE INDEX IF NOT EXISTS idx_git_evidence_span_sequence
        ON git_evidence_span(sequence);
    CREATE INDEX IF NOT EXISTS idx_git_evidence_span_last_ts
        ON git_evidence_span(last_ts);
    CREATE TABLE IF NOT EXISTS git_evidence_commit (
        commit_sha TEXT NOT NULL CHECK(length(commit_sha) > 0),
        session_id TEXT NOT NULL CHECK(length(session_id) > 0),
        record TEXT NOT NULL,
        PRIMARY KEY (commit_sha, session_id)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS idx_git_evidence_commit_session
        ON git_evidence_commit(session_id);
    CREATE TABLE IF NOT EXISTS git_evidence_generation (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        sequence INTEGER NOT NULL CHECK(sequence > 0),
        digest TEXT NOT NULL CHECK(length(digest) > 0),
        span_count INTEGER NOT NULL CHECK(span_count >= 0),
        commit_count INTEGER NOT NULL CHECK(commit_count >= 0),
        attributed_through INTEGER NOT NULL CHECK(attributed_through >= 0)
    );
";

/// Hub fan-out page width for newest-first span index reads.
const SPAN_INDEX_PAGE_ROWS: i64 = 256;

/// Evidence one write folds into the per-session rows.
#[derive(Debug, Default)]
pub struct GitEvidenceBatch {
    /// Raw transcript and hook observations, shaped into spans against the
    /// stored spans of their sessions.
    pub observations: Vec<SpanObservation>,
    pub spans: Vec<SessionGitSpan>,
    pub commits: Vec<CommitSessionRecord>,
    pub merge_gap_secs: i64,
}

impl GitEvidenceBatch {
    fn is_empty(&self) -> bool {
        self.observations.is_empty() && self.spans.is_empty() && self.commits.is_empty()
    }
}

/// Rows one write changed relative to what was stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitEvidenceWrite {
    pub spans_changed: usize,
    pub commits_changed: usize,
}

/// The projection generation: metadata over the evidence rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidenceGeneration {
    pub sequence: i64,
    pub digest: String,
    pub span_count: u64,
    pub commit_count: u64,
    /// Every span written at or before this sequence has been attributed.
    pub attributed_through: i64,
}

impl GitEvidenceGeneration {
    pub fn generation_id(&self) -> String {
        format!("session-git-evidence:{}", self.digest)
    }

    pub fn source_watermark(&self) -> String {
        format!("git-evidence-sequence:{}", self.sequence)
    }
}

/// Folds evidence into the rows inside one caller-owned write transaction and
/// installs one new generation for the whole transaction in [`Self::finish`].
pub struct GitEvidenceWriter<'t, T: Executor + ?Sized> {
    transaction: &'t T,
    base: Option<GitEvidenceGeneration>,
    sequence: i64,
    change_digest: Vec<String>,
    spans_added: u64,
    commits_added: u64,
    attributed_through: Option<i64>,
}

impl<'t, T: Executor + ?Sized> GitEvidenceWriter<'t, T> {
    pub async fn open(transaction: &'t T) -> Result<Self, GitCorrelationError> {
        let base = read_generation(transaction).await?;
        let sequence = base
            .as_ref()
            .map_or(1, |base| base.sequence.saturating_add(1));
        Ok(Self {
            transaction,
            base,
            sequence,
            change_digest: Vec::new(),
            spans_added: 0,
            commits_added: 0,
            attributed_through: None,
        })
    }

    /// Merges `batch` into the stored rows of the sessions it names and
    /// upserts every row whose canonical record changed.
    pub async fn apply(
        &mut self,
        batch: GitEvidenceBatch,
    ) -> Result<GitEvidenceWrite, GitCorrelationError> {
        if batch.is_empty() {
            return Ok(GitEvidenceWrite::default());
        }
        let GitEvidenceBatch {
            observations,
            spans: incoming_spans,
            commits: incoming_commits,
            merge_gap_secs,
        } = batch;
        let session_ids = observations
            .iter()
            .map(|observation| observation.session_id.clone())
            .chain(incoming_spans.iter().map(|span| span.session_id.clone()))
            .chain(
                incoming_commits
                    .iter()
                    .map(|record| record.session_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        let (stored_spans, stored_commits) =
            load_session_rows(self.transaction, &session_ids).await?;

        let mut spans = group_by_session(stored_spans.values().cloned(), |span| &span.session_id);
        let mut commits = group_by_session(stored_commits.values().cloned(), |record| {
            &record.session_id
        });
        let current = stored_spans.values().cloned().collect::<Vec<_>>();
        let candidates =
            transcript_spans_from_observations(&current, &observations, merge_gap_secs)
                .into_iter()
                .chain(incoming_spans);
        for mut incoming in candidates {
            validate_span(&incoming)?;
            incoming.worktree = normalize_worktree(&incoming.worktree);
            merge_span(
                spans.entry(incoming.session_id.clone()).or_default(),
                &incoming,
            );
        }
        for mut incoming in incoming_commits {
            validate_commit_record(&incoming)?;
            incoming.commit_sha = parse_commit_sha(&incoming.commit_sha)?;
            incoming.worktree = incoming.worktree.as_deref().map(normalize_worktree);
            merge_commit(
                commits.entry(incoming.session_id.clone()).or_default(),
                &incoming,
            );
        }

        let mut write = GitEvidenceWrite::default();
        for session_id in &session_ids {
            let session_spans = spans.entry(session_id.clone()).or_default();
            let session_commits = commits.entry(session_id.clone()).or_default();
            canonical_providers(session_spans, session_commits)?;
            if session_commits.iter().any(|record| {
                record.span_id.as_deref().is_some_and(|span_id| {
                    !session_spans.iter().any(|span| span.span_id == span_id)
                })
            }) {
                return Err(GitCorrelationError::Contract(
                    "Git evidence relation references an absent span".to_owned(),
                ));
            }
            for span in session_spans.iter() {
                if stored_spans.get(&span.span_id) == Some(span) {
                    continue;
                }
                if !stored_spans.contains_key(&span.span_id) {
                    self.spans_added = self.spans_added.saturating_add(1);
                }
                let record = serde_json::to_string(span)?;
                self.transaction
                    .execute(
                        "INSERT INTO git_evidence_span(
                             span_id, session_id, provider, branch, worktree,
                             first_ts, last_ts, sequence, record
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                         ON CONFLICT(span_id) DO UPDATE SET
                             session_id = excluded.session_id,
                             provider = excluded.provider,
                             branch = excluded.branch,
                             worktree = excluded.worktree,
                             first_ts = excluded.first_ts,
                             last_ts = excluded.last_ts,
                             sequence = excluded.sequence,
                             record = excluded.record",
                        params![
                            span.span_id.as_str(),
                            span.session_id.as_str(),
                            span.provider.as_str(),
                            span.branch.as_deref(),
                            span.worktree.as_str(),
                            span.first_ts,
                            span.last_ts,
                            self.sequence,
                            record.as_str()
                        ],
                    )
                    .await?;
                self.change_digest.push(record);
                write.spans_changed += 1;
            }
            for record in session_commits.iter() {
                let key = (record.commit_sha.clone(), record.session_id.clone());
                if stored_commits.get(&key) == Some(record) {
                    continue;
                }
                if !stored_commits.contains_key(&key) {
                    self.commits_added = self.commits_added.saturating_add(1);
                }
                let encoded = serde_json::to_string(record)?;
                self.transaction
                    .execute(
                        "INSERT INTO git_evidence_commit(commit_sha, session_id, record)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(commit_sha, session_id) DO UPDATE SET
                             record = excluded.record",
                        params![
                            record.commit_sha.as_str(),
                            record.session_id.as_str(),
                            encoded.as_str()
                        ],
                    )
                    .await?;
                self.change_digest.push(encoded);
                write.commits_changed += 1;
            }
        }
        Ok(write)
    }

    /// Spans no attribution has covered yet, plus every span whose window
    /// still reaches `open_since` (a commit made after its last observation
    /// may yet fall inside its merge gap).
    pub async fn spans_pending_attribution(
        &self,
        open_since: i64,
    ) -> Result<Vec<SessionGitSpan>, GitCorrelationError> {
        let attributed_through = self.base.as_ref().map_or(0, |base| base.attributed_through);
        let mut rows = self
            .transaction
            .query(
                "SELECT record FROM git_evidence_span
                 WHERE sequence > ?1 OR last_ts >= ?2
                 ORDER BY span_id",
                params![attributed_through, open_since],
            )
            .await?;
        let mut spans = Vec::new();
        while let Some(row) = rows.next().await? {
            spans.push(decode_record(&row)?);
        }
        Ok(spans)
    }

    /// Records that every stored span, including this transaction's writes,
    /// has been attributed.
    pub fn mark_attributed(&mut self) {
        self.attributed_through = Some(if self.change_digest.is_empty() {
            self.base.as_ref().map_or(0, |base| base.sequence)
        } else {
            self.sequence
        });
    }

    /// Installs this transaction's generation when a row changed. Attribution
    /// coverage alone moves the existing generation's coverage mark without
    /// minting a new generation of unchanged evidence.
    pub async fn finish(self) -> Result<Option<GitEvidenceGeneration>, GitCorrelationError> {
        let base_attributed = self.base.as_ref().map_or(0, |base| base.attributed_through);
        if self.change_digest.is_empty() {
            if let (Some(through), Some(_)) = (self.attributed_through, &self.base)
                && through > base_attributed
            {
                self.transaction
                    .execute(
                        "UPDATE git_evidence_generation SET attributed_through = ?1
                         WHERE singleton = 1",
                        params![through],
                    )
                    .await?;
            }
            return Ok(None);
        }
        let attributed_through = self.attributed_through.unwrap_or(base_attributed);
        let changed_rows = self.change_digest.len();
        let mut changes = self.change_digest;
        changes.sort_unstable();
        let previous = self
            .base
            .as_ref()
            .map_or_else(String::new, |base| base.digest.clone());
        let digest = digest_bytes(
            serde_json::to_string(&(
                "tracedecay.session-git-evidence-generation.v3",
                previous,
                self.sequence,
                changes,
            ))?
            .as_bytes(),
        );
        let generation = GitEvidenceGeneration {
            sequence: self.sequence,
            digest,
            span_count: self
                .base
                .as_ref()
                .map_or(0, |base| base.span_count)
                .saturating_add(self.spans_added),
            commit_count: self
                .base
                .as_ref()
                .map_or(0, |base| base.commit_count)
                .saturating_add(self.commits_added),
            attributed_through,
        };
        self.transaction
            .execute(
                "INSERT INTO git_evidence_generation(
                     singleton, sequence, digest, span_count, commit_count, attributed_through
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(singleton) DO UPDATE SET
                     sequence = excluded.sequence,
                     digest = excluded.digest,
                     span_count = excluded.span_count,
                     commit_count = excluded.commit_count,
                     attributed_through = excluded.attributed_through",
                params![
                    generation.sequence,
                    generation.digest.as_str(),
                    generation.span_count,
                    generation.commit_count,
                    generation.attributed_through
                ],
            )
            .await?;
        tracing::debug!(
            event = "git_evidence_generation_installed",
            sequence = generation.sequence,
            rows_changed = changed_rows,
            span_count = generation.span_count,
            commit_count = generation.commit_count,
            "installed a Git evidence generation over the changed session rows"
        );
        Ok(Some(generation))
    }
}

fn group_by_session<T>(
    rows: impl IntoIterator<Item = T>,
    session: impl Fn(&T) -> &String,
) -> HashMap<String, Vec<T>> {
    let mut grouped = HashMap::<String, Vec<T>>::new();
    for row in rows {
        grouped.entry(session(&row).clone()).or_default().push(row);
    }
    grouped
}

type StoredSpans = BTreeMap<String, SessionGitSpan>;
type StoredCommits = BTreeMap<(String, String), CommitSessionRecord>;

async fn load_session_rows(
    conn: &(impl QueryExecutor + ?Sized),
    session_ids: &BTreeSet<String>,
) -> Result<(StoredSpans, StoredCommits), GitCorrelationError> {
    let ids = serde_json::to_string(session_ids)?;
    let mut spans = BTreeMap::new();
    let mut rows = conn
        .query(
            "SELECT record FROM git_evidence_span
             WHERE session_id IN (SELECT value FROM json_each(?1))",
            params![ids.as_str()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let span: SessionGitSpan = decode_record(&row)?;
        spans.insert(span.span_id.clone(), span);
    }
    let mut commits = BTreeMap::new();
    let mut rows = conn
        .query(
            "SELECT record FROM git_evidence_commit
             WHERE session_id IN (SELECT value FROM json_each(?1))",
            params![ids.as_str()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let record: CommitSessionRecord = decode_record(&row)?;
        commits.insert(
            (record.commit_sha.clone(), record.session_id.clone()),
            record,
        );
    }
    Ok((spans, commits))
}

fn decode_record<T: serde::de::DeserializeOwned>(row: &Row) -> Result<T, GitCorrelationError> {
    let record = row.get::<String>(0)?;
    serde_json::from_str(&record).map_err(|error| {
        GitCorrelationError::Corrupt(format!("Git evidence row does not decode: {error}"))
    })
}

async fn read_generation(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<Option<GitEvidenceGeneration>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT sequence, digest, span_count, commit_count, attributed_through
             FROM git_evidence_generation WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let count = |index: i32| -> Result<u64, GitCorrelationError> {
        u64::try_from(row.get::<i64>(index)?).map_err(|_| {
            GitCorrelationError::Corrupt(
                "Git evidence generation records a negative count".to_owned(),
            )
        })
    };
    Ok(Some(GitEvidenceGeneration {
        sequence: row.get(0)?,
        digest: row.get(1)?,
        span_count: count(2)?,
        commit_count: count(3)?,
        attributed_through: row.get(4)?,
    }))
}

/// Bounded query view over the evidence rows of one read snapshot.
///
/// Every read is an indexed lookup: session queries page a branch or
/// worktree's newest-first span index and hydrate only the sessions that can
/// appear in the result; commit queries read one SHA-prefix range. Results
/// equal what the same aggregation computes over every stored row.
pub struct GitEvidenceView<'c, Q: QueryExecutor + ?Sized> {
    conn: &'c Q,
    generation: GitEvidenceGeneration,
}

/// Opens the view, answering `Ok(None)` when no evidence was ever written.
pub async fn open_git_evidence_view<Q: QueryExecutor + ?Sized>(
    conn: &Q,
) -> Result<Option<GitEvidenceView<'_, Q>>, GitCorrelationError> {
    Ok(read_generation(conn)
        .await?
        .map(|generation| GitEvidenceView { conn, generation }))
}

impl<Q: QueryExecutor + ?Sized> GitEvidenceView<'_, Q> {
    pub const fn generation(&self) -> &GitEvidenceGeneration {
        &self.generation
    }

    pub fn health(&self, backfill_watermark: Option<i64>) -> CorrelationIndexHealth {
        CorrelationIndexHealth {
            projection_available: true,
            generation: Some(self.generation.generation_id()),
            source_watermark: Some(self.generation.source_watermark()),
            span_count: self.generation.span_count,
            commit_count: self.generation.commit_count,
            backfill_watermark,
        }
    }

    pub fn presence(&self, backfill_watermark: Option<i64>) -> CorrelationIndexPresence {
        CorrelationIndexPresence {
            projection_available: true,
            generation: Some(self.generation.generation_id()),
            source_watermark: Some(self.generation.source_watermark()),
            spans_present: self.generation.span_count > 0,
            commits_present: self.generation.commit_count > 0,
            backfill_watermark,
        }
    }

    #[hotpath::measure(label = "sessions.git_correlation.rows.sessions_for", future = true)]
    pub async fn sessions_for(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
        let limit = sessions_for_limit(query);
        match &query.git_ref {
            GitRefFilter::Branch(_) | GitRefFilter::Worktree(_) => {
                let sessions = self.leading_sessions(query, limit).await?;
                let (spans, _) = self.session_rows(&sessions).await?;
                Ok(span_hits(
                    spans.iter().filter(|span| span_matches_query(span, query)),
                    limit,
                ))
            }
            GitRefFilter::Commit(sha) => {
                let records = self.commit_records_with_prefix(sha).await?;
                Ok(commit_hits(
                    records
                        .iter()
                        .filter(|record| commit_record_matches_query(record, sha, relation, query)),
                    limit,
                ))
            }
        }
    }

    /// `None` for an empty filter, otherwise the authoritative (possibly
    /// empty) intersection of every scoped selector.
    #[hotpath::measure(
        label = "sessions.git_correlation.rows.session_ids_for_scope",
        future = true
    )]
    pub async fn session_ids_for_scope(
        &self,
        filter: &GitScopeFilter,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        if filter.is_empty() {
            return Ok(None);
        }
        let mut selected: Option<BTreeMap<String, String>> = None;
        if let Some(branch) = &filter.branch {
            selected = Some(intersect_id_maps(
                selected,
                self.span_identities_where("branch = ?1", branch).await?,
            ));
        }
        if let Some(worktree) = &filter.worktree {
            selected = Some(intersect_id_maps(
                selected,
                self.span_identities_where("worktree = ?1", worktree)
                    .await?,
            ));
        }
        if let Some(commit) = &filter.commit {
            let records = self.commit_records_with_prefix(commit).await?;
            selected = Some(intersect_id_maps(
                selected,
                commit_identities_with_producer_fallback(records.iter()),
            ));
        }
        Ok(Some(scope_session_ids(selected)))
    }

    /// Resolves a single branch or worktree selector only far enough for a
    /// caller to detect that its own session bound was exceeded. Compound and
    /// commit selectors keep the complete intersection above.
    #[hotpath::measure(
        label = "sessions.git_correlation.rows.session_ids_for_scope_bounded",
        future = true
    )]
    pub async fn session_ids_for_scope_bounded(
        &self,
        filter: &GitScopeFilter,
        maximum: usize,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        let git_ref = match (&filter.branch, &filter.worktree, &filter.commit) {
            (Some(branch), None, None) => GitRefFilter::Branch(branch.clone()),
            (None, Some(worktree), None) => GitRefFilter::Worktree(worktree.clone()),
            _ => return self.session_ids_for_scope(filter).await,
        };
        let query = SessionsForQuery {
            git_ref,
            since: None,
            until: None,
            limit: maximum,
        };
        let sessions = self.leading_sessions(&query, maximum).await?;
        let (spans, _) = self.session_rows(&sessions).await?;
        Ok(Some(scope_session_ids(Some(span_identities(spans.iter())))))
    }

    /// Every span and commit attribution recorded for `session_ids`. Spans
    /// come back in `(provider, session_id, first_ts, span_id)` order and
    /// commits in canonical `(commit_sha, session_id)` order.
    #[hotpath::measure(
        label = "sessions.git_correlation.rows.session_evidence",
        future = true
    )]
    pub async fn session_evidence(
        &self,
        session_ids: &BTreeSet<String>,
    ) -> Result<(Vec<SessionGitSpan>, Vec<CommitSessionRecord>), GitCorrelationError> {
        let (mut spans, mut commits) = self.session_rows(session_ids).await?;
        spans.sort_by(|left, right| {
            (
                &left.provider,
                &left.session_id,
                left.first_ts,
                &left.span_id,
            )
                .cmp(&(
                    &right.provider,
                    &right.session_id,
                    right.first_ts,
                    &right.span_id,
                ))
        });
        commits.sort_by(commit_record_order);
        Ok((spans, commits))
    }

    async fn session_rows(
        &self,
        session_ids: &BTreeSet<String>,
    ) -> Result<(Vec<SessionGitSpan>, Vec<CommitSessionRecord>), GitCorrelationError> {
        if session_ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let (spans, commits) = load_session_rows(self.conn, session_ids).await?;
        Ok((
            spans.into_values().collect(),
            commits.into_values().collect(),
        ))
    }

    /// Sessions that can rank among the first `limit` hits: the span index is
    /// paged newest-`last_ts` first, so the first in-window span seen for a
    /// session carries that session's sort key. Paging stops once `limit`
    /// sessions are known and the next span can no longer tie the `limit`-th
    /// session's key, or once spans end before `since`.
    async fn leading_sessions(
        &self,
        query: &SessionsForQuery,
        limit: usize,
    ) -> Result<BTreeSet<String>, GitCorrelationError> {
        let (column, value) = match &query.git_ref {
            GitRefFilter::Branch(branch) => ("branch", branch),
            GitRefFilter::Worktree(worktree) => ("worktree", worktree),
            GitRefFilter::Commit(_) => return Ok(BTreeSet::new()),
        };
        let first_page = format!(
            "SELECT session_id, first_ts, last_ts, span_id FROM git_evidence_span
             WHERE {column} = ?1
             ORDER BY last_ts DESC, span_id DESC LIMIT ?2"
        );
        let next_page = format!(
            "SELECT session_id, first_ts, last_ts, span_id FROM git_evidence_span
             WHERE {column} = ?1 AND (last_ts < ?3 OR (last_ts = ?3 AND span_id < ?4))
             ORDER BY last_ts DESC, span_id DESC LIMIT ?2"
        );
        let mut selected = BTreeSet::new();
        let mut boundary: Option<i64> = None;
        let mut after: Option<(i64, String)> = None;
        loop {
            let mut rows = match &after {
                None => {
                    self.conn
                        .query(&first_page, params![value.as_str(), SPAN_INDEX_PAGE_ROWS])
                        .await?
                }
                Some((last_ts, span_id)) => {
                    self.conn
                        .query(
                            &next_page,
                            params![
                                value.as_str(),
                                SPAN_INDEX_PAGE_ROWS,
                                *last_ts,
                                span_id.as_str()
                            ],
                        )
                        .await?
                }
            };
            let mut page_rows = 0_i64;
            while let Some(row) = rows.next().await? {
                page_rows += 1;
                let session_id = row.get::<String>(0)?;
                let first_ts = row.get::<i64>(1)?;
                let last_ts = row.get::<i64>(2)?;
                after = Some((last_ts, row.get::<String>(3)?));
                if query.since.is_some_and(|since| last_ts < since)
                    || boundary.is_some_and(|boundary| last_ts < boundary)
                {
                    return Ok(selected);
                }
                if query.until.is_some_and(|until| first_ts > until) {
                    continue;
                }
                if selected.insert(session_id) && selected.len() == limit {
                    boundary = Some(last_ts);
                }
            }
            if page_rows < SPAN_INDEX_PAGE_ROWS {
                return Ok(selected);
            }
        }
    }

    async fn span_identities_where(
        &self,
        predicate: &str,
        value: &str,
    ) -> Result<BTreeMap<String, String>, GitCorrelationError> {
        let mut rows = self
            .conn
            .query(
                &format!("SELECT record FROM git_evidence_span WHERE {predicate}"),
                params![value],
            )
            .await?;
        let mut spans = Vec::new();
        while let Some(row) = rows.next().await? {
            spans.push(decode_record::<SessionGitSpan>(&row)?);
        }
        Ok(span_identities(spans.iter()))
    }

    /// Every commit/session record whose SHA starts with `sha`, in canonical
    /// order. SHAs are lowercase hex, so `[sha, sha + 'g')` is the prefix
    /// range.
    async fn commit_records_with_prefix(
        &self,
        sha: &str,
    ) -> Result<Vec<CommitSessionRecord>, GitCorrelationError> {
        let mut rows = self
            .conn
            .query(
                "SELECT record FROM git_evidence_commit
                 WHERE commit_sha >= ?1 AND commit_sha < ?2
                 ORDER BY commit_sha, session_id",
                params![sha, format!("{sha}g")],
            )
            .await?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(decode_record::<CommitSessionRecord>(&row)?);
        }
        Ok(records)
    }
}
