use super::{
    CommitEvidence, CommitRelation, CommitSessionRecord, GitCorrelationError, SessionGitSpan,
    SpanObservation, SpanOverlapKind, digest_bytes, normalize_worktree, observation_extends_span,
    providers_compatible,
};

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
    /// The retained span names a branch that is no longer present in the
    /// repository. Historical attribution is unavailable for this target,
    /// but retrying cannot restore the archived ref.
    MissingReference,
    /// The scan could not run, the worktree is gone, `git log` failed, or the
    /// repository was unreadable. Distinct from `Scanned(vec![])`: the target's
    /// commits are unknown, not absent, so the sweep watermark must not move
    /// past it or the target would never be revisited.
    Unavailable,
}

/// Commits matched against a set of spans, before any publication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitAttribution {
    pub records: Vec<CommitSessionRecord>,
    pub unavailable_references: usize,
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
        span_id: format!("backfill:{}", digest_bytes(identity.as_bytes())),
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

pub(super) fn transcript_spans_from_observations(
    current: &[SessionGitSpan],
    observations: &[SpanObservation],
    merge_gap_secs: i64,
) -> Vec<SessionGitSpan> {
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
    candidates
}

fn transcript_span_id(observation: &SpanObservation, worktree: &str) -> String {
    let thread_id = observation.thread_id.as_deref().unwrap_or_default();
    let branch = observation.branch.as_deref().unwrap_or_default();
    let material = format!(
        "{}\0{}\0{thread_id}\0{branch}\0{}\0{}\0{:?}",
        observation.provider, observation.session_id, worktree, observation.ts, observation.source,
    );
    format!("transcript:{}", digest_bytes(material.as_bytes()))
}

pub(super) fn merge_span(spans: &mut Vec<SessionGitSpan>, incoming: &SessionGitSpan) -> bool {
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

pub(super) fn merge_commit(
    commits: &mut Vec<CommitSessionRecord>,
    incoming: &CommitSessionRecord,
) -> bool {
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

/// Matches every commit a span target's history holds against `spans`.
///
/// Every target is rescanned because the immutable graph projection has no
/// SQL `updated_at` surrogate; merging the records into the head is a no-op
/// for commits it already holds.
pub fn attribute_commits<F>(
    spans: &[SessionGitSpan],
    gap_secs: i64,
    mut scan: F,
) -> Result<CommitAttribution, GitCorrelationError>
where
    F: FnMut(&SpanScanTarget) -> TargetScan,
{
    let mut attribution = CommitAttribution::default();
    for target in &scan_targets(spans) {
        let windows = span_windows_for(spans, target.branch.as_deref(), &target.worktree);
        if windows.is_empty() {
            continue;
        }
        let commits = match scan(target) {
            TargetScan::Scanned(commits) => commits,
            TargetScan::MissingReference => {
                attribution.unavailable_references =
                    attribution.unavailable_references.saturating_add(1);
                continue;
            }
            TargetScan::Unavailable => {
                return Err(GitCorrelationError::Unavailable(format!(
                    "cannot scan Git history for retained span target {}",
                    target.worktree
                )));
            }
        };
        for commit in commits {
            attribution.records.extend(match_commit_to_spans(
                &commit.sha,
                target.branch.as_deref(),
                &target.worktree,
                commit.committed_at,
                &windows,
                gap_secs,
            ));
        }
    }
    Ok(attribution)
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
}
