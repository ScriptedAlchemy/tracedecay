//! Immutable session/Git evidence projected into verified graph generations.
//!
//! Commit/session evidence and branch/worktree spans are not relational rows.
//! A complete [`GitEvidenceProjectionV1`] is published atomically through the
//! project graph runtime and every query is evaluated from its verified
//! snapshot. SQLite retains only resumable-history receipts and watermarks.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::SessionMessageRecord;

mod error;
pub use error::GitCorrelationError;

const MIGRATION_NAME: &str = "git_correlation";
const MESSAGE_WORKTREE_KEYS: [&str; 9] = [
    "codex_turn_worktree",
    "claude_message_worktree",
    "cursor_event_worktree",
    "kiro_workspace_worktree",
    "cline_like_task_worktree",
    "vibe_session_worktree",
    "codex_session_worktree",
    "claude_session_worktree",
    "hermes_session_worktree",
];

/// Receipt schema version. This schema owns no Git evidence rows.
pub const GIT_CORRELATION_SCHEMA_VERSION: i64 = 4;
pub const GIT_EVIDENCE_PROJECTOR_REVISION_V1: &str = "session-git-evidence-projector.v1";
pub const DEFAULT_SPAN_MERGE_GAP_SECS: i64 = 30 * 60;
pub const DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS: i64 = 30;
pub const MAX_SESSIONS_FOR_LIMIT: usize = 100;
pub const AUTO_BACKFILL_WATERMARK_KEY: &str = "auto_backfill_activity_watermark";
pub const GIT_HISTORY_ROWID_FRONTIER_KEY: &str = "git_history_session_rowid_frontier";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanSource {
    HookRoute,
    Ingest,
    Backfill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanOverlapKind {
    Direct,
    WithinSpan,
    ExtendedWindow,
    Reflog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitRelation {
    Produced,
    Observed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitEvidence {
    ToolResult,
    HostEvent,
    HeadObservation,
    ReflogOverlap,
    TimeOverlap,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommitRelationFilter {
    #[default]
    Produced,
    Observed,
    All,
}

impl CommitRelationFilter {
    pub fn parse(value: Option<&str>) -> Result<Self, GitCorrelationError> {
        match value.unwrap_or("produced") {
            "produced" => Ok(Self::Produced),
            "observed" => Ok(Self::Observed),
            "all" => Ok(Self::All),
            other => Err(GitCorrelationError::InvalidArgument(format!(
                "relation must be one of produced, observed, all (got `{other}`)"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Observed => "observed",
            Self::All => "all",
        }
    }

    const fn matches(self, relation: CommitRelation) -> bool {
        matches!(
            (self, relation),
            (Self::All, _)
                | (Self::Produced, CommitRelation::Produced)
                | (Self::Observed, CommitRelation::Observed)
        )
    }
}

/// One immutable activity span entity in the Git evidence projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGitSpan {
    /// Stable projector-issued identity, not a relational row id.
    pub span_id: String,
    pub provider: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub branch: Option<String>,
    pub worktree: String,
    pub first_ts: i64,
    pub last_ts: i64,
    pub event_count: i64,
    pub source: SpanSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanObservation {
    pub provider: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub branch: Option<String>,
    pub worktree: String,
    pub ts: i64,
    pub source: SpanSource,
}

/// Evidence carried by a session-to-commit graph relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitSessionRecord {
    pub commit_sha: String,
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub committed_at: i64,
    pub span_overlap_kind: SpanOverlapKind,
    pub span_id: Option<String>,
    pub relation: CommitRelation,
    pub evidence: CommitEvidence,
    pub confidence: i64,
    pub evidence_message_id: Option<String>,
}

/// Canonical, complete input to one immutable graph generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitEvidenceProjectionV1 {
    source_watermark: String,
    spans: Vec<SessionGitSpan>,
    commit_sessions: Vec<CommitSessionRecord>,
}

impl GitEvidenceProjectionV1 {
    pub fn new(
        source_watermark: impl Into<String>,
        mut spans: Vec<SessionGitSpan>,
        mut commit_sessions: Vec<CommitSessionRecord>,
    ) -> Result<Self, GitCorrelationError> {
        let source_watermark = source_watermark.into();
        if source_watermark.trim().is_empty() {
            return Err(GitCorrelationError::Contract(
                "Git evidence source watermark must not be empty".to_owned(),
            ));
        }
        for span in &mut spans {
            validate_span(span)?;
            span.worktree = normalize_worktree(&span.worktree);
        }
        for record in &mut commit_sessions {
            validate_commit_record(record)?;
            record.commit_sha = parse_commit_sha(&record.commit_sha)?;
            record.worktree = record.worktree.as_deref().map(normalize_worktree);
        }
        spans.sort_by(|left, right| left.span_id.cmp(&right.span_id));
        commit_sessions.sort_by(commit_record_order);
        if spans
            .windows(2)
            .any(|pair| pair[0].span_id == pair[1].span_id)
        {
            return Err(GitCorrelationError::Contract(
                "Git evidence span identities must be unique".to_owned(),
            ));
        }
        if commit_sessions.windows(2).any(|pair| {
            pair[0].commit_sha == pair[1].commit_sha && pair[0].session_id == pair[1].session_id
        }) {
            return Err(GitCorrelationError::Contract(
                "Git evidence commit/session relations must be unique".to_owned(),
            ));
        }
        let span_ids = spans
            .iter()
            .map(|span| span.span_id.as_str())
            .collect::<HashSet<_>>();
        if commit_sessions.iter().any(|record| {
            record
                .span_id
                .as_deref()
                .is_some_and(|span_id| !span_ids.contains(span_id))
        }) {
            return Err(GitCorrelationError::Contract(
                "Git evidence relation references an absent span".to_owned(),
            ));
        }
        canonical_providers(&mut spans, &mut commit_sessions)?;
        Ok(Self {
            source_watermark,
            spans,
            commit_sessions,
        })
    }

    pub fn source_watermark(&self) -> &str {
        &self.source_watermark
    }

    pub fn spans(&self) -> &[SessionGitSpan] {
        &self.spans
    }

    pub fn commit_sessions(&self) -> &[CommitSessionRecord] {
        &self.commit_sessions
    }

    pub fn sessions_for(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Vec<SessionGitCorrelationHit> {
        let limit = query.limit.clamp(1, MAX_SESSIONS_FOR_LIMIT);
        match &query.git_ref {
            GitRefFilter::Branch(branch) => {
                self.span_hits(|span| span.branch.as_deref() == Some(branch), query, limit)
            }
            GitRefFilter::Worktree(worktree) => {
                self.span_hits(|span| &span.worktree == worktree, query, limit)
            }
            GitRefFilter::Commit(commit) => self.commit_hits(commit, query, relation, limit),
        }
    }

    pub fn session_ids_for_scope(&self, filter: &GitScopeFilter) -> Option<Vec<(String, String)>> {
        if filter.is_empty() {
            return None;
        }
        let mut selected: Option<BTreeMap<String, String>> = None;
        if let Some(branch) = &filter.branch {
            selected = Some(intersect_id_maps(
                selected,
                self.span_identities(|span| span.branch.as_deref() == Some(branch)),
            ));
        }
        if let Some(worktree) = &filter.worktree {
            selected = Some(intersect_id_maps(
                selected,
                self.span_identities(|span| &span.worktree == worktree),
            ));
        }
        if let Some(commit) = &filter.commit {
            selected = Some(intersect_id_maps(
                selected,
                self.commit_identities_with_producer_fallback(commit),
            ));
        }
        Some(
            selected
                .unwrap_or_default()
                .into_iter()
                .map(|(session_id, provider)| (provider, session_id))
                .collect(),
        )
    }

    fn span_hits(
        &self,
        predicate: impl Fn(&SessionGitSpan) -> bool,
        query: &SessionsForQuery,
        limit: usize,
    ) -> Vec<SessionGitCorrelationHit> {
        let mut grouped = BTreeMap::<String, Vec<&SessionGitSpan>>::new();
        for span in self.spans.iter().filter(|span| {
            predicate(span)
                && query.since.is_none_or(|since| span.last_ts >= since)
                && query.until.is_none_or(|until| span.first_ts <= until)
        }) {
            grouped
                .entry(span.session_id.clone())
                .or_default()
                .push(span);
        }
        let mut hits = grouped
            .into_values()
            .map(|spans| span_hit(&spans))
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .last_ts
                .cmp(&left.last_ts)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        hits.truncate(limit);
        hits
    }

    fn commit_hits(
        &self,
        sha: &str,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
        limit: usize,
    ) -> Vec<SessionGitCorrelationHit> {
        let mut by_session = HashMap::<String, SessionGitCorrelationHit>::new();
        for record in self.commit_sessions.iter().filter(|record| {
            record.commit_sha.starts_with(sha)
                && relation.matches(record.relation)
                && query.since.is_none_or(|since| record.committed_at >= since)
                && query.until.is_none_or(|until| record.committed_at <= until)
        }) {
            let candidate = commit_hit(record);
            match by_session.get_mut(&record.session_id) {
                Some(existing)
                    if commit_hit_strength(&candidate) > commit_hit_strength(existing) =>
                {
                    *existing = candidate;
                }
                Some(existing)
                    if existing.provider.is_empty() && !candidate.provider.is_empty() =>
                {
                    existing.provider = candidate.provider;
                }
                Some(_) => {}
                None => {
                    by_session.insert(record.session_id.clone(), candidate);
                }
            }
        }
        let mut hits = by_session.into_values().collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .committed_at
                .cmp(&left.committed_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        hits.truncate(limit);
        hits
    }

    fn span_identities(
        &self,
        predicate: impl Fn(&SessionGitSpan) -> bool,
    ) -> BTreeMap<String, String> {
        self.spans
            .iter()
            .filter(|span| predicate(span))
            .fold(BTreeMap::new(), |mut ids, span| {
                ids.entry(span.session_id.clone())
                    .and_modify(|provider| {
                        if provider.is_empty() {
                            provider.clone_from(&span.provider);
                        }
                    })
                    .or_insert_with(|| span.provider.clone());
                ids
            })
    }

    fn commit_identities_with_producer_fallback(&self, sha: &str) -> BTreeMap<String, String> {
        let matching = self
            .commit_sessions
            .iter()
            .filter(|record| record.commit_sha.starts_with(sha))
            .collect::<Vec<_>>();
        let has_producer = matching
            .iter()
            .any(|record| record.relation == CommitRelation::Produced);
        matching
            .into_iter()
            .filter(|record| !has_producer || record.relation == CommitRelation::Produced)
            .map(|record| (record.session_id.clone(), record.provider.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRefFilter {
    Branch(String),
    Worktree(String),
    Commit(String),
}

impl GitRefFilter {
    pub fn parse(kind: &str, value: &str) -> Result<Self, GitCorrelationError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(GitCorrelationError::InvalidArgument(
                "value must be a non-empty string".to_owned(),
            ));
        }
        match kind {
            "branch" => Ok(Self::Branch(value.to_owned())),
            "worktree" => Ok(Self::Worktree(normalize_worktree(value))),
            "commit" => parse_commit_sha(value).map(Self::Commit),
            other => Err(GitCorrelationError::InvalidArgument(format!(
                "git_ref must be one of branch, worktree, commit (got `{other}`)"
            ))),
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Branch(_) => "branch",
            Self::Worktree(_) => "worktree",
            Self::Commit(_) => "commit",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Branch(value) | Self::Worktree(value) | Self::Commit(value) => value,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitScopeFilter {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
}

impl GitScopeFilter {
    pub fn from_args(
        branch: Option<&str>,
        worktree: Option<&str>,
        commit: Option<&str>,
    ) -> Result<Self, GitCorrelationError> {
        Ok(Self {
            branch: nonempty(branch).map(str::to_owned),
            worktree: nonempty(worktree).map(normalize_worktree),
            commit: nonempty(commit).map(parse_commit_sha).transpose()?,
        })
    }

    pub const fn is_empty(&self) -> bool {
        self.branch.is_none() && self.worktree.is_none() && self.commit.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsForQuery {
    pub git_ref: GitRefFilter,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGitCorrelationHit {
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub event_count: i64,
    pub span_count: i64,
    pub sources: Vec<String>,
    pub commit_sha: Option<String>,
    pub committed_at: Option<i64>,
    pub span_overlap_kind: Option<SpanOverlapKind>,
    pub relation: Option<CommitRelation>,
    pub evidence: Option<CommitEvidence>,
    pub confidence: Option<i64>,
    pub evidence_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorrelationIndexHealth {
    pub projection_available: bool,
    pub generation: Option<String>,
    pub source_watermark: Option<String>,
    pub span_count: u64,
    pub commit_count: u64,
    pub backfill_watermark: Option<i64>,
}

impl CorrelationIndexHealth {
    pub const fn is_empty(&self) -> bool {
        self.span_count == 0
    }

    pub const fn is_empty_for(&self, git_ref: &GitRefFilter) -> bool {
        match git_ref {
            GitRefFilter::Branch(_) | GitRefFilter::Worktree(_) => self.span_count == 0,
            GitRefFilter::Commit(_) => self.commit_count == 0,
        }
    }
}

pub fn normalize_worktree(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/UNC/") {
        normalized = format!("//{stripped}");
    } else if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_owned();
    }
    if let Some(stripped) = normalized.strip_prefix("/private/var/") {
        normalized = format!("/var/{stripped}");
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

pub fn observation_extends_span(first_ts: i64, last_ts: i64, ts: i64, gap_secs: i64) -> bool {
    ts >= first_ts.saturating_sub(gap_secs) && ts <= last_ts.saturating_add(gap_secs)
}

#[derive(Debug, Default)]
pub struct SpanObservationDebounce {
    last_write: HashMap<String, i64>,
}

impl SpanObservationDebounce {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn should_record(&mut self, key: &str, ts: i64, min_interval_secs: i64) -> bool {
        if self
            .last_write
            .get(key)
            .is_some_and(|last| ts >= *last && ts - *last < min_interval_secs)
        {
            return false;
        }
        self.last_write.insert(key.to_owned(), ts);
        true
    }
}

pub fn span_debounce_key(
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
) -> String {
    digest_bytes(
        format!(
            "{provider}\u{1f}{session_id}\u{1f}{}\u{1f}{worktree}",
            branch.unwrap_or("\u{0}")
        )
        .as_bytes(),
    )
}

pub fn direct_commit_records(
    messages: &[SessionMessageRecord],
    project_root: &std::path::Path,
) -> Vec<CommitSessionRecord> {
    let Ok(repo) = gix::discover(project_root) else {
        return Vec::new();
    };
    let mut records = BTreeMap::<(String, String), CommitSessionRecord>::new();
    for message in messages {
        let Some(metadata) = message
            .metadata_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|value| value.as_object().cloned())
        else {
            continue;
        };
        for (key, relation, default_evidence, confidence) in [
            (
                "produced_commit_candidates",
                CommitRelation::Produced,
                CommitEvidence::ToolResult,
                100,
            ),
            (
                "observed_commit_candidates",
                CommitRelation::Observed,
                CommitEvidence::HeadObservation,
                60,
            ),
        ] {
            let Some(candidates) = metadata.get(key).and_then(serde_json::Value::as_array) else {
                continue;
            };
            for candidate in candidates.iter().filter_map(serde_json::Value::as_str) {
                let Ok(spec) = repo.rev_parse_single(candidate) else {
                    continue;
                };
                let Ok(object) = spec.object() else {
                    continue;
                };
                let Ok(commit) = object.try_into_commit() else {
                    continue;
                };
                let sha = commit.id.to_string();
                let evidence = if relation == CommitRelation::Produced
                    && metadata
                        .get("produced_commit_evidence")
                        .and_then(serde_json::Value::as_str)
                        == Some("host_event")
                {
                    CommitEvidence::HostEvent
                } else {
                    default_evidence
                };
                let record = CommitSessionRecord {
                    commit_sha: sha.clone(),
                    provider: message.provider.clone(),
                    session_id: message.session_id.clone(),
                    branch: metadata
                        .get("git_branch")
                        .or_else(|| metadata.get("codex_git_branch"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    worktree: metadata_worktree(&metadata)
                        .map(normalize_worktree)
                        .or_else(|| Some(normalize_worktree(&project_root.to_string_lossy()))),
                    committed_at: commit
                        .time()
                        .map_or(message.timestamp.unwrap_or_default(), |time| time.seconds),
                    span_overlap_kind: SpanOverlapKind::Direct,
                    span_id: None,
                    relation,
                    evidence,
                    confidence,
                    evidence_message_id: Some(message.message_id.clone()),
                };
                let slot = (sha, message.session_id.clone());
                if records
                    .get(&slot)
                    .is_none_or(|existing| record_strength(&record) > record_strength(existing))
                {
                    records.insert(slot, record);
                }
            }
        }
    }
    records.into_values().collect()
}

pub fn ingest_span_observations(messages: &[SessionMessageRecord]) -> Vec<SpanObservation> {
    messages
        .iter()
        .filter_map(|message| {
            let timestamp = message.timestamp?;
            let metadata =
                serde_json::from_str::<serde_json::Value>(message.metadata_json.as_deref()?)
                    .ok()?;
            let metadata = metadata.as_object()?;
            let worktree = metadata_worktree(metadata).filter(|path| !path.is_empty())?;
            Some(SpanObservation {
                provider: message.provider.clone(),
                session_id: message.session_id.clone(),
                thread_id: metadata
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                branch: metadata
                    .get("git_branch")
                    .or_else(|| metadata.get("codex_git_branch"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                worktree: normalize_worktree(worktree),
                ts: timestamp,
                source: SpanSource::Ingest,
            })
        })
        .collect()
}

/// Installs only relational receipts used by bounded history convergence.
pub async fn ensure_git_correlation_receipt_schema_in_transaction(
    conn: &(impl Executor + ?Sized),
) -> Result<(), GitCorrelationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS git_correlation_meta (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );",
    )
    .await?;
    backfill::history_progress::install_final_schema(conn).await?;
    backfill::history_failures::install_final_schema(conn).await?;
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET version = excluded.version",
        params![MIGRATION_NAME, GIT_CORRELATION_SCHEMA_VERSION],
    )
    .await?;
    Ok(())
}

pub async fn read_meta_value(
    conn: &(impl QueryExecutor + ?Sized),
    key: &str,
) -> Result<Option<i64>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT value FROM git_correlation_meta WHERE key = ?1",
            params![key],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| row.get(0).map_err(GitCorrelationError::from))
        .transpose()
}

pub async fn write_meta_value(
    conn: &(impl Executor + ?Sized),
    key: &str,
    value: i64,
) -> Result<(), GitCorrelationError> {
    conn.execute(
        "INSERT INTO git_correlation_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
        params![key, value],
    )
    .await?;
    Ok(())
}

/// Whether two span provider labels can identify the same session lineage.
///
/// An empty provider is an unattributed observation (hook routes cannot
/// always name the provider); it matches any canonical provider, and the
/// canonical map in [`GitEvidenceProjectionV1::new`] settles the final
/// label. Two distinct non-empty providers never match — one session
/// carrying both is rejected by `canonical_provider_map`.
pub fn providers_compatible(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

fn validate_span(span: &SessionGitSpan) -> Result<(), GitCorrelationError> {
    if span.span_id.is_empty()
        || span.session_id.is_empty()
        || span.worktree.trim().is_empty()
        || span.first_ts > span.last_ts
        || span.event_count <= 0
    {
        return Err(GitCorrelationError::Contract(
            "Git evidence span is incomplete or has invalid bounds".to_owned(),
        ));
    }
    Ok(())
}

fn validate_commit_record(record: &CommitSessionRecord) -> Result<(), GitCorrelationError> {
    parse_commit_sha(&record.commit_sha)?;
    if record.session_id.is_empty() || !(0..=100).contains(&record.confidence) {
        return Err(GitCorrelationError::Contract(
            "Git commit evidence has an invalid session or confidence".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_providers(
    spans: &mut [SessionGitSpan],
    commits: &mut [CommitSessionRecord],
) -> Result<(), GitCorrelationError> {
    let providers = canonical_provider_map(spans, commits)?;
    for span in spans.iter_mut() {
        if let Some(provider) = providers.get(&span.session_id) {
            span.provider.clone_from(provider);
        }
    }
    for record in commits.iter_mut() {
        if let Some(provider) = providers.get(&record.session_id) {
            record.provider.clone_from(provider);
        }
    }
    Ok(())
}

fn canonical_provider_map(
    spans: &[SessionGitSpan],
    commits: &[CommitSessionRecord],
) -> Result<BTreeMap<String, String>, GitCorrelationError> {
    let mut providers = BTreeMap::new();
    for (session_id, provider) in spans
        .iter()
        .map(|span| (&span.session_id, &span.provider))
        .chain(
            commits
                .iter()
                .map(|record| (&record.session_id, &record.provider)),
        )
    {
        let current = providers
            .entry(session_id.clone())
            .or_insert_with(String::new);
        if current.is_empty() {
            current.clone_from(provider);
        } else if !provider.is_empty() && current != provider {
            return Err(GitCorrelationError::Contract(format!(
                "session `{session_id}` has conflicting providers"
            )));
        }
    }
    Ok(providers)
}

fn commit_record_order(
    left: &CommitSessionRecord,
    right: &CommitSessionRecord,
) -> std::cmp::Ordering {
    left.commit_sha
        .cmp(&right.commit_sha)
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn metadata_worktree(metadata: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    MESSAGE_WORKTREE_KEYS
        .into_iter()
        .find_map(|key| metadata.get(key).and_then(serde_json::Value::as_str))
}

fn parse_commit_sha(value: &str) -> Result<String, GitCorrelationError> {
    if !(6..=64).contains(&value.len()) || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(GitCorrelationError::InvalidArgument(
            "commit must be 6-64 hexadecimal characters".to_owned(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn intersect_id_maps(
    accumulated: Option<BTreeMap<String, String>>,
    next: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    accumulated.map_or(next.clone(), |existing| {
        existing
            .into_iter()
            .filter(|(session_id, _)| next.contains_key(session_id))
            .collect()
    })
}

fn span_hit(spans: &[&SessionGitSpan]) -> SessionGitCorrelationHit {
    let providers = spans
        .iter()
        .map(|span| span.provider.as_str())
        .filter(|provider| !provider.is_empty())
        .collect::<BTreeSet<_>>();
    let branches = spans
        .iter()
        .filter_map(|span| span.branch.as_deref())
        .collect::<BTreeSet<_>>();
    let worktrees = spans
        .iter()
        .map(|span| span.worktree.as_str())
        .collect::<BTreeSet<_>>();
    let sources = spans
        .iter()
        .map(|span| format!("{:?}", span.source).to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    SessionGitCorrelationHit {
        provider: providers
            .iter()
            .next()
            .copied()
            .unwrap_or_default()
            .to_owned(),
        session_id: spans[0].session_id.clone(),
        branch: one_value(branches),
        worktree: one_value(worktrees),
        first_ts: spans.iter().map(|span| span.first_ts).min(),
        last_ts: spans.iter().map(|span| span.last_ts).max(),
        event_count: spans.iter().map(|span| span.event_count).sum(),
        span_count: i64::try_from(spans.len()).unwrap_or(i64::MAX),
        sources: sources.into_iter().collect(),
        commit_sha: None,
        committed_at: None,
        span_overlap_kind: None,
        relation: None,
        evidence: None,
        confidence: None,
        evidence_message_id: None,
    }
}

fn one_value(values: BTreeSet<&str>) -> Option<String> {
    (values.len() == 1)
        .then(|| values.into_iter().next().map(str::to_owned))
        .flatten()
}

fn commit_hit(record: &CommitSessionRecord) -> SessionGitCorrelationHit {
    SessionGitCorrelationHit {
        provider: record.provider.clone(),
        session_id: record.session_id.clone(),
        branch: record.branch.clone(),
        worktree: record.worktree.clone(),
        first_ts: None,
        last_ts: None,
        event_count: 0,
        span_count: 0,
        sources: Vec::new(),
        commit_sha: Some(record.commit_sha.clone()),
        committed_at: Some(record.committed_at),
        span_overlap_kind: Some(record.span_overlap_kind),
        relation: Some(record.relation),
        evidence: Some(record.evidence),
        confidence: Some(record.confidence),
        evidence_message_id: record.evidence_message_id.clone(),
    }
}

fn record_strength(record: &CommitSessionRecord) -> (u8, i64) {
    (
        u8::from(record.relation == CommitRelation::Produced),
        record.confidence,
    )
}

fn commit_hit_strength(hit: &SessionGitCorrelationHit) -> (u8, i64) {
    (
        u8::from(hit.relation == Some(CommitRelation::Produced)),
        hit.confidence.unwrap_or_default(),
    )
}

mod attribution;
mod backfill;
mod store;
pub use attribution::{
    MISSING_VERIFIED_HEAD, ScannedCommit, SpanScanTarget, SpanWindow, TargetScan,
    commit_overlap_kind, graph_evidence_publication_key, match_commit_to_spans,
    publish_graph_evidence, run_commit_attribution_sweep,
};
pub use backfill::{
    BackfillOptions, BackfillSkipReason, BackfillStats, BoundedBackfillInterruption,
    BoundedBackfillOutcome, BoundedGitControl, BranchTimelineEntry,
    DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS, GitHistoryIndexFrontier, GitReflogSource,
    SessionActivityRow, SystemGit, WindowBranchSegment, branch_timeline_from_reflog,
    parse_commit_log, run_bounded_history_index_page, window_branch_segments,
};
pub use backfill::{run_backfill, run_incremental_backfill};
pub use store::{
    AnalyticsSessionTimestamp, AnalyticsSessionTimestampSource, GitCorrelationSessionStore,
    GitCorrelationWriteTxn, GitEvidenceGraphRuntimePort, GitEvidenceProjectionStore,
    build_git_evidence_manifest_checked, git_evidence_generation_id,
    git_evidence_projection_identity, publish_git_evidence_projection,
    recover_git_evidence_projection,
};

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
