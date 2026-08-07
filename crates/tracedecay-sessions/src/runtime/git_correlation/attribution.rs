use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sha2::{Digest as _, Sha256};
use tracedecay_graph_db::{GraphIdempotencyKey, GraphNamespace, GraphProjectorRevision};

use super::{
    CommitEvidence, CommitRelation, CommitSessionRecord, GIT_EVIDENCE_PROJECTOR_REVISION_V1,
    GitCorrelationError, GitCorrelationSessionStore, GitEvidenceProjectionV1, SessionGitSpan,
    SpanOverlapKind, git_evidence_projection_identity, normalize_worktree,
    providers_compatible, publish_git_evidence_projection, recover_git_evidence_projection,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";
/// Exact unavailability message the graph registry emits when a projection
/// has no installed verified head (see graph-db `publication_support`).
/// Callers that treat "nothing published yet" as an empty start match on
/// this sentinel; keep every copy pointed at this one constant.
pub const MISSING_VERIFIED_HEAD: &str =
    "graph projection is not recovered into an installed verified head";

/// A `(branch, worktree)` pair a session was observed on, with the widest span
/// window recorded for it. Commit scans run once per pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanScanTarget {
    pub branch: Option<String>,
    pub worktree: String,
    pub window_start: i64,
    pub window_end: i64,
}

/// One span row a candidate commit may fall inside. Kept minimal so the
/// matching logic ([`match_commit_to_spans`]) is a pure function testable
/// without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanWindow {
    pub span_id: String,
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: String,
    pub first_ts: i64,
    pub last_ts: i64,
}

/// Classifies a commit at `committed_at` against one span: `Some(WithinSpan)`
/// when strictly inside `[first_ts, last_ts]`, `Some(ExtendedWindow)` when
/// inside the span widened by `gap_secs` on either edge, `None` otherwise.
pub fn commit_overlap_kind(
    first_ts: i64,
    last_ts: i64,
    committed_at: i64,
    gap_secs: i64,
) -> Option<SpanOverlapKind> {
    if committed_at >= first_ts && committed_at <= last_ts {
        Some(SpanOverlapKind::WithinSpan)
    } else if committed_at >= first_ts.saturating_sub(gap_secs)
        && committed_at <= last_ts.saturating_add(gap_secs)
    {
        Some(SpanOverlapKind::ExtendedWindow)
    } else {
        None
    }
}

/// Records that every matching span observed a commit. Time overlap is
/// candidate evidence only: concurrent sessions must never be labelled as
/// producers without a direct tool/host event.
pub fn match_commit_to_spans(
    commit_sha: &str,
    branch: Option<&str>,
    worktree: &str,
    committed_at: i64,
    spans: &[SpanWindow],
    gap_secs: i64,
) -> Vec<CommitSessionRecord> {
    let mut records = Vec::new();
    for span in spans {
        if span.branch.as_deref() != branch || span.worktree != worktree {
            continue;
        }
        let Some(kind) = commit_overlap_kind(span.first_ts, span.last_ts, committed_at, gap_secs)
        else {
            continue;
        };
        records.push(CommitSessionRecord {
            commit_sha: commit_sha.to_string(),
            provider: span.provider.clone(),
            session_id: span.session_id.clone(),
            branch: span.branch.clone(),
            worktree: Some(span.worktree.clone()),
            committed_at,
            span_overlap_kind: kind,
            span_id: Some(span.span_id.clone()),
            relation: CommitRelation::Observed,
            evidence: CommitEvidence::TimeOverlap,
            confidence: match kind {
                SpanOverlapKind::Direct => 100,
                SpanOverlapKind::WithinSpan => 20,
                SpanOverlapKind::ExtendedWindow => 10,
                SpanOverlapKind::Reflog => 30,
            },
            evidence_message_id: None,
        });
    }
    records
}

fn scan_targets(spans: &[SessionGitSpan]) -> Vec<SpanScanTarget> {
    let mut targets = std::collections::BTreeMap::new();
    for span in spans {
        let key = (span.branch.clone(), span.worktree.clone());
        targets
            .entry(key)
            .and_modify(|target: &mut SpanScanTarget| {
                target.window_start = target.window_start.min(span.first_ts);
                target.window_end = target.window_end.max(span.last_ts);
            })
            .or_insert_with(|| SpanScanTarget {
                branch: span.branch.clone(),
                worktree: span.worktree.clone(),
                window_start: span.first_ts,
                window_end: span.last_ts,
            });
    }
    targets.into_values().collect()
}

fn span_windows_for(
    spans: &[SessionGitSpan],
    branch: Option<&str>,
    worktree: &str,
) -> Vec<SpanWindow> {
    spans
        .iter()
        .filter(|span| span.branch.as_deref() == branch && span.worktree == worktree)
        .map(|span| SpanWindow {
            span_id: span.span_id.clone(),
            provider: span.provider.clone(),
            session_id: span.session_id.clone(),
            branch: span.branch.clone(),
            worktree: span.worktree.clone(),
            first_ts: span.first_ts,
            last_ts: span.last_ts,
        })
        .collect()
}

/// One commit observed by the bounded git scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedCommit {
    pub sha: String,
    pub committed_at: i64,
}

/// Outcome of scanning one span target's git history for candidate commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetScan {
    /// The scan ran; these are the commits it found (possibly none).
    Scanned(Vec<ScannedCommit>),
    /// The scan could not run — the worktree is gone, `git log` failed, or the
    /// repository was unreadable. Distinct from `Scanned(vec![])`: the target's
    /// commits are unknown, not absent, so the sweep watermark must not move
    /// past it or the target would never be revisited.
    Unavailable,
}

pub fn stable_backfill_span(
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
    first_ts: i64,
    last_ts: i64,
) -> SessionGitSpan {
    let worktree = normalize_worktree(worktree);
    let identity = format!(
        "{provider}\0{session_id}\0{}\0{worktree}\0{first_ts}\0{last_ts}",
        branch.unwrap_or("\0")
    );
    SessionGitSpan {
        span_id: format!(
            "backfill:{}",
            hex::encode(Sha256::digest(identity.as_bytes()))
        ),
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        thread_id: None,
        branch: branch.map(str::to_owned),
        worktree,
        first_ts,
        last_ts,
        event_count: 2,
        source: super::SpanSource::Backfill,
    }
}

pub fn publish_graph_evidence<S: GitCorrelationSessionStore>(
    session_store: &S,
    publication_prefix: &str,
    new_spans: &[SessionGitSpan],
    new_commits: &[CommitSessionRecord],
) -> Result<(usize, usize), GitCorrelationError> {
    let runtime = session_store.graph_runtime()?;
    let identity =
        git_evidence_projection_identity(GraphNamespace::new(GIT_EVIDENCE_GRAPH_NAMESPACE)?)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let (mut spans, mut commits) =
        match recover_git_evidence_projection(runtime, &identity, Arc::clone(&cancelled)) {
            Ok(store) => (
                store.projection().spans().to_vec(),
                store.projection().commit_sessions().to_vec(),
            ),
            Err(GitCorrelationError::Unavailable(message)) if message == MISSING_VERIFIED_HEAD => {
                (Vec::new(), Vec::new())
            }
            Err(error) => return Err(error),
        };
    let mut spans_changed = 0;
    for incoming in new_spans {
        if merge_span(&mut spans, incoming) {
            spans_changed += 1;
        }
    }
    let mut commits_changed = 0;
    for incoming in new_commits {
        if merge_commit(&mut commits, incoming) {
            commits_changed += 1;
        }
    }
    let publication_key = graph_evidence_publication_key(publication_prefix, &spans, &commits)?;
    let projection = GitEvidenceProjectionV1::new(&publication_key, spans, commits)?;
    let revision = GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned())?;
    publish_git_evidence_projection(
        runtime,
        identity,
        &projection,
        &revision,
        GraphIdempotencyKey::new(publication_key)?,
        cancelled,
    )?;
    Ok((spans_changed, commits_changed))
}

pub fn graph_evidence_publication_key(
    prefix: &str,
    spans: &[SessionGitSpan],
    commits: &[CommitSessionRecord],
) -> Result<String, GitCorrelationError> {
    let bytes = serde_json::to_vec(&(prefix, spans, commits))?;
    Ok(format!("{prefix}:{}", hex::encode(Sha256::digest(bytes))))
}

fn merge_span(spans: &mut Vec<SessionGitSpan>, incoming: &SessionGitSpan) -> bool {
    if spans.iter().any(|span| span == incoming) {
        return false;
    }
    if let Some(existing) = spans.iter_mut().find(|span| {
        providers_compatible(&span.provider, &incoming.provider)
            && span.session_id == incoming.session_id
            && span.thread_id == incoming.thread_id
            && span.branch == incoming.branch
            && span.worktree == incoming.worktree
            && span.source == incoming.source
            && incoming.first_ts <= span.last_ts
            && incoming.last_ts >= span.first_ts
    }) {
        let previous = existing.clone();
        if existing.provider.is_empty() && !incoming.provider.is_empty() {
            existing.provider.clone_from(&incoming.provider);
        }
        existing.first_ts = existing.first_ts.min(incoming.first_ts);
        existing.last_ts = existing.last_ts.max(incoming.last_ts);
        existing.event_count = existing.event_count.max(incoming.event_count);
        return *existing != previous;
    } else {
        spans.push(incoming.clone());
    }
    true
}

fn merge_commit(commits: &mut Vec<CommitSessionRecord>, incoming: &CommitSessionRecord) -> bool {
    let Some(existing) = commits.iter_mut().find(|record| {
        record.commit_sha == incoming.commit_sha && record.session_id == incoming.session_id
    }) else {
        commits.push(incoming.clone());
        return true;
    };
    if existing == incoming {
        return false;
    }
    if (
        incoming.relation == CommitRelation::Produced,
        incoming.confidence,
    ) > (
        existing.relation == CommitRelation::Produced,
        existing.confidence,
    ) {
        existing.clone_from(incoming);
        true
    } else {
        false
    }
}

/// Runs commit attribution against the currently verified span projection.
///
/// Every span is rescanned because the immutable graph projection has no SQL
/// `updated_at` surrogate. Content-addressed graph publication makes replay a
/// no-op while still admitting late historical spans.
pub async fn run_commit_attribution_sweep<S, F>(
    session_store: &S,
    gap_secs: i64,
    mut scan: F,
) -> Result<usize, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    F: FnMut(&SpanScanTarget) -> TargetScan,
{
    let runtime = session_store.graph_runtime()?;
    let identity =
        git_evidence_projection_identity(GraphNamespace::new(GIT_EVIDENCE_GRAPH_NAMESPACE)?)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let projection = match recover_git_evidence_projection(runtime, &identity, cancelled) {
        Ok(store) => store.projection().clone(),
        Err(GitCorrelationError::Unavailable(message)) if message == MISSING_VERIFIED_HEAD => {
            return Ok(0);
        }
        Err(error) => return Err(error),
    };
    let targets = scan_targets(projection.spans());
    let mut records = Vec::new();
    for target in &targets {
        let spans = span_windows_for(
            projection.spans(),
            target.branch.as_deref(),
            &target.worktree,
        );
        if spans.is_empty() {
            continue;
        }
        let TargetScan::Scanned(commits) = scan(target) else {
            continue;
        };
        for commit in commits {
            records.extend(match_commit_to_spans(
                &commit.sha,
                target.branch.as_deref(),
                &target.worktree,
                commit.committed_at,
                &spans,
                gap_secs,
            ));
        }
    }
    if records.is_empty() {
        return Ok(0);
    }
    let (_, inserted) = publish_graph_evidence(session_store, "git-attribution", &[], &records)?;
    Ok(inserted)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn repeated_bounded_span_merge_is_a_noop() {
        let span = stable_backfill_span("codex", "session-1", Some("main"), "/repo", 10, 20);
        let mut spans = Vec::new();
        assert!(merge_span(&mut spans, &span));
        assert!(!merge_span(&mut spans, &span));
        assert_eq!(spans, vec![span]);
    }

    #[test]
    fn publication_key_covers_the_complete_evidence() {
        let first = stable_backfill_span("codex", "session-1", Some("main"), "/repo", 10, 20);
        let second = stable_backfill_span("codex", "session-1", Some("main"), "/repo", 10, 21);
        assert_ne!(
            graph_evidence_publication_key("bounded", &[first], &[]).unwrap(),
            graph_evidence_publication_key("bounded", &[second], &[]).unwrap()
        );
    }
}
