//! Project the retained delivery evidence onto one exact PR comparison.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use serde::Serialize;
use tracedecay_application::delivery::{
    ProjectDeliveryCiCheckV1, ProjectDeliveryCiSourceV1, ProjectDeliveryCiTimelineV1,
    ProjectDeliveryGitHubOperationSnapshotV1, ProjectDeliveryGitHubSourceV1,
    ProjectDeliveryGitHubTimelineV1, ProjectDeliveryProviderMountGateV1,
    ProjectDeliveryPullRequestV1, ProjectDeliveryReadOutcomeV1, ProjectDeliveryReviewItemV1,
    ProjectDeliveryReviewObservationKindV1, ProjectDeliveryReviewObservationV1,
};
use tracedecay_contracts::retrieval::{
    PrContextCheckV1, PrContextDiscussionV1, PrContextHostedV1, PrContextLocationV1,
    PrContextPullRequestV1, PrContextSourceStateV1, PrContextSourceV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::feedback::{
    GitHubPullRequestReviewV1, GitHubReviewCoverageV1, GitHubReviewIngressProviderOutcomeV1,
    GitHubReviewReadOperationV1, GitHubReviewRemapStateV1, GitHubReviewStateV1,
};

fn source(state: PrContextSourceStateV1, detail: impl Into<String>) -> PrContextSourceV1 {
    PrContextSourceV1 {
        state,
        detail: detail.into(),
    }
}

fn label(value: &impl Serialize) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(TraceDecayError::Config {
            message: "PR briefing state did not serialize as a label".to_owned(),
        }),
    }
}

fn github_source(
    value: &ProjectDeliveryGitHubSourceV1,
) -> (PrContextSourceV1, Option<&ProjectDeliveryGitHubTimelineV1>) {
    use ProjectDeliveryGitHubSourceV1 as State;
    let (state, detail, timeline) = match value {
        State::Ready { timeline } => (
            PrContextSourceStateV1::Ready,
            "Retained GitHub observations; observation times and commit identities are reported below.",
            Some(timeline),
        ),
        State::Partial { timeline } => (
            PrContextSourceStateV1::Partial,
            "Retained GitHub observations are partial; absence is not evidence that no discussion exists.",
            Some(timeline),
        ),
        State::Stale { timeline } => (
            PrContextSourceStateV1::Stale,
            "Retained GitHub observations are stale; they are historical evidence.",
            Some(timeline),
        ),
        State::RateLimited { timeline, .. } => (
            PrContextSourceStateV1::Partial,
            "GitHub refresh was rate limited; retained observations may be out of date.",
            Some(timeline),
        ),
        State::Failed { timeline } => (
            PrContextSourceStateV1::Unavailable,
            "GitHub refresh failed; any retained observations remain historical evidence.",
            Some(timeline),
        ),
        State::Denied { .. } => (
            PrContextSourceStateV1::Denied,
            "GitHub refresh was denied; retained GitHub observations are withheld from this briefing.",
            None,
        ),
        State::NotPublished => (
            PrContextSourceStateV1::NotPublished,
            "No GitHub observations have been retained for this checkout.",
            None,
        ),
        State::Unavailable { timeline } => (
            PrContextSourceStateV1::Unavailable,
            "GitHub observation source is unavailable; any retained rows are incomplete evidence.",
            Some(timeline),
        ),
    };
    (source(state, detail), timeline)
}

fn ci_source(
    value: &ProjectDeliveryCiSourceV1,
) -> (PrContextSourceV1, Option<&ProjectDeliveryCiTimelineV1>) {
    use ProjectDeliveryCiSourceV1 as State;
    let (state, detail, timeline) = match value {
        State::Ready { timeline } => (
            PrContextSourceStateV1::Ready,
            "Retained CI observations only; required checks and a complete current check inventory are not known.",
            Some(timeline),
        ),
        State::Denied { timeline } => (
            PrContextSourceStateV1::Denied,
            "CI refresh was denied; retained rows cannot establish the current check inventory.",
            Some(timeline),
        ),
        State::NotPublished => (
            PrContextSourceStateV1::NotPublished,
            "No CI observations have been retained; check status is unknown.",
            None,
        ),
        State::Unavailable { timeline } => (
            PrContextSourceStateV1::Unavailable,
            "CI source is unavailable; retained rows cannot establish the current check inventory.",
            Some(timeline),
        ),
    };
    (source(state, detail), timeline)
}

fn latest_snapshot(
    pr: &ProjectDeliveryPullRequestV1,
) -> Option<&ProjectDeliveryGitHubOperationSnapshotV1> {
    pr.operations
        .iter()
        .flat_map(|operation| {
            operation
                .last_complete
                .iter()
                .map(|snapshot| (snapshot, false))
                .chain(
                    operation
                        .latest_attempt
                        .iter()
                        .map(|snapshot| (snapshot, true)),
                )
        })
        .max_by_key(|(snapshot, latest)| (snapshot.fetched_at.0, *latest))
        .map(|(snapshot, _)| snapshot)
}

enum Selection<'a> {
    Selected(&'a ProjectDeliveryPullRequestV1),
    Ambiguous,
    Missing,
}

fn select<'a>(
    timeline: &'a ProjectDeliveryGitHubTimelineV1,
    number: Option<u64>,
    head: &str,
) -> Selection<'a> {
    let mut matches = timeline.pull_requests.iter().filter(|pr| match number {
        Some(number) => pr
            .identity
            .as_ref()
            .is_some_and(|identity| identity.number == number),
        None => latest_snapshot(pr)
            .is_some_and(|snapshot| snapshot.provider_head_commit_id.as_str() == head),
    });
    match (matches.next(), matches.next()) {
        (Some(pr), None) => Selection::Selected(pr),
        (Some(_), Some(_)) => Selection::Ambiguous,
        _ => Selection::Missing,
    }
}

pub(super) fn selected_base(
    outcome: Option<&ProjectDeliveryReadOutcomeV1>,
    selected_pr: Option<u64>,
    head: &str,
) -> Option<String> {
    let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = outcome? else {
        return None;
    };
    let (_, timeline) = github_source(&snapshot.github_reviews);
    let Selection::Selected(pr) = select(timeline?, selected_pr, head) else {
        return None;
    };
    let latest = latest_snapshot(pr)?;
    let identity = pr.identity.as_ref()?;
    if latest.provider_head_commit_id.as_str() != head
        || identity.observed_head_commit_id.as_str() != head
    {
        return None;
    }
    pr.operations
        .iter()
        .filter(|operation| operation.operation == GitHubReviewReadOperationV1::RestGetPullRequest)
        .flat_map(|operation| {
            operation
                .latest_attempt
                .iter()
                .chain(operation.last_complete.iter())
        })
        .find(|observed| {
            matches!(
                (observed.outcome, observed.coverage),
                (
                    GitHubReviewIngressProviderOutcomeV1::Complete,
                    GitHubReviewCoverageV1::Complete
                ) | (
                    GitHubReviewIngressProviderOutcomeV1::Partial,
                    GitHubReviewCoverageV1::Partial
                )
            ) && observed.quarantined.is_empty()
                && observed.fetched_at == identity.observed_at
                && observed.provider_head_commit_id == identity.observed_head_commit_id
                && observed.provider_base_commit_id == identity.observed_base_commit_id
        })
        .map(|observed| observed.provider_base_commit_id.to_string())
}

fn empty(state: PrContextSourceV1) -> PrContextHostedV1 {
    PrContextHostedV1 {
        source: state.clone(),
        ci_source: state,
        pull_request: None,
        discussions: Vec::new(),
        checks: Vec::new(),
        discussions_total: 0,
        checks_total: 0,
        required_checks_known: false,
    }
}

fn review_observation(
    item: &ProjectDeliveryReviewItemV1,
) -> Option<&ProjectDeliveryReviewObservationV1> {
    item.observations.iter().max_by_key(|observation| {
        (
            observation.item.observed_at.0,
            observation.kind == ProjectDeliveryReviewObservationKindV1::LatestAttempt,
            observation.operation
                == GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        )
    })
}

fn discussion(
    observation: &ProjectDeliveryReviewObservationV1,
    head: &str,
) -> Result<PrContextDiscussionV1> {
    let item = &observation.item;
    let matches_head = item.remap.current_scope.head_commit_id.as_str() == head
        && item.remap.state == GitHubReviewRemapStateV1::ExactCurrent
        && item
            .remap
            .current
            .as_ref()
            .is_some_and(|anchor| anchor.commit_id.as_str() == head);
    let kind = match observation.operation {
        GitHubReviewReadOperationV1::RestListPullRequestReviews => "review",
        _ => "review_comment",
    };
    Ok(PrContextDiscussionV1 {
        id: item.comment_id.to_string(),
        thread_id: item.thread_id.as_ref().map(ToString::to_string),
        kind: kind.to_owned(),
        lifecycle: label(&item.lifecycle)?,
        review_state: label(&item.review_state)?,
        author: None,
        event_at: None,
        file: (!item.path.is_empty()).then(|| item.path.clone()),
        line: if matches_head {
            item.line
        } else {
            item.original_line.or(item.line)
        },
        text: observation
            .body_preview
            .as_ref()
            .map(|preview| preview.text.clone()),
        text_truncated: observation
            .body_preview
            .as_ref()
            .is_some_and(|preview| preview.truncated),
        url: item.safe_url.clone(),
        body_anchor: Some(item.body_anchor.to_string()),
        observed_at: item.observed_at.0,
        matches_head,
    })
}

fn discussion_priority(
    item: &PrContextDiscussionV1,
) -> (u8, Reverse<Option<i64>>, Reverse<i64>, &str) {
    let active = !matches!(
        item.lifecycle.as_str(),
        "resolved" | "outdated" | "deleted" | "superseded" | "retained"
    );
    let priority = match (item.matches_head, active, item.review_state.as_str()) {
        (true, true, "changes_requested") => 0,
        (true, true, _) => 1,
        (false, true, _) => 2,
        _ => 3,
    };
    (
        priority,
        Reverse(item.event_at),
        Reverse(item.observed_at),
        &item.id,
    )
}

fn provider_event_at(value: &str) -> Option<i64> {
    // strptime accepts numeric offsets; GitHub spells UTC as `Z`.
    let utc_offset = value
        .strip_suffix('Z')
        .map(|value| format!("{value}+00:00"));
    gix::date::Zoned::strptime(
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        utc_offset.as_deref().unwrap_or(value),
    )
    .ok()
    .map(|time| time.timestamp().as_microsecond())
}

fn check(check: &ProjectDeliveryCiCheckV1, head: &str) -> Result<PrContextCheckV1> {
    Ok(PrContextCheckV1 {
        name: format!(
            "check {} / job {}",
            check.run.check_run_id, check.run.job_id
        ),
        workflow: check.workflow_path.clone(),
        status: label(&check.check_status)?,
        conclusion: check.check_conclusion.as_ref().map(label).transpose()?,
        failed_step: check.failed_step.clone(),
        head: check.provider_head_sha.clone(),
        matches_head: check.provider_head_sha == head,
        observed_at: check.observed_at.0,
        evidence_anchor: check.failure_anchor.to_string(),
        locations: check
            .annotations
            .iter()
            .map(|annotation| PrContextLocationV1 {
                file: annotation.path.clone(),
                line: annotation.start_line,
                end_line: annotation.end_line,
            })
            .collect(),
    })
}

pub(super) fn project_hosted(
    outcome: Option<&ProjectDeliveryReadOutcomeV1>,
    selected_pr: Option<u64>,
    head: &str,
) -> Result<PrContextHostedV1> {
    let snapshot = match outcome {
        Some(ProjectDeliveryReadOutcomeV1::Ready { snapshot }) => snapshot,
        Some(ProjectDeliveryReadOutcomeV1::Denied) => {
            return Ok(empty(source(
                PrContextSourceStateV1::Denied,
                "Hosted PR evidence is denied for this checkout.",
            )));
        }
        Some(ProjectDeliveryReadOutcomeV1::NotMounted { gate }) => {
            let detail = match gate {
                ProjectDeliveryProviderMountGateV1::NoGitRemote => {
                    "No GitHub remote is configured for this checkout."
                }
                ProjectDeliveryProviderMountGateV1::GitHubAccessRefused => {
                    "The configured GitHub read credential was refused."
                }
                ProjectDeliveryProviderMountGateV1::GitHubSourceAccessUnavailable => {
                    "The configured GitHub read authority is unavailable."
                }
            };
            return Ok(empty(source(PrContextSourceStateV1::NotMounted, detail)));
        }
        Some(ProjectDeliveryReadOutcomeV1::Unavailable) => {
            return Ok(empty(source(
                PrContextSourceStateV1::Unavailable,
                "The hosted PR evidence source is unavailable.",
            )));
        }
        None => {
            return Ok(empty(source(
                PrContextSourceStateV1::NotMounted,
                "Hosted PR evidence is not mounted; local change analysis remains available.",
            )));
        }
    };
    let (github_state, github) = github_source(&snapshot.github_reviews);
    let (ci_state, ci) = ci_source(&snapshot.ci_checks);
    let mut result = empty(github_state);
    result.ci_source = ci_state;
    if let Some(timeline) = ci {
        let mut latest: BTreeMap<(&String, &String), &ProjectDeliveryCiCheckV1> = BTreeMap::new();
        for row in &timeline.checks {
            let key = (&row.provider_head_sha, &row.run.check_run_id);
            match latest.get(&key) {
                Some(previous) if row.observed_at.0 <= previous.observed_at.0 => {}
                _ => {
                    latest.insert(key, row);
                }
            }
        }
        result.checks = latest
            .into_values()
            .map(|row| check(row, head))
            .collect::<Result<_>>()?;
        result.checks.sort_by_key(|row| {
            (
                !row.matches_head,
                !matches!(
                    row.conclusion.as_deref(),
                    Some("failure" | "timed_out" | "action_required")
                ),
                Reverse(row.observed_at),
                row.workflow.clone(),
                row.name.clone(),
            )
        });
        result.checks_total = timeline.total_retained;
        if timeline.truncated {
            if result.ci_source.state == PrContextSourceStateV1::Ready {
                result.ci_source.state = PrContextSourceStateV1::Partial;
            }
            result
                .ci_source
                .detail
                .push_str(" The retained inventory was truncated.");
        }
    }
    let Some(timeline) = github else {
        return Ok(result);
    };
    let pr = match select(timeline, selected_pr, head) {
        Selection::Selected(pr) => pr,
        Selection::Ambiguous => {
            if matches!(
                result.source.state,
                PrContextSourceStateV1::Ready
                    | PrContextSourceStateV1::Partial
                    | PrContextSourceStateV1::Stale
            ) {
                result.source.state = PrContextSourceStateV1::Ambiguous;
            }
            result.source.detail.push_str(" Multiple retained pull requests match; pass pull_request with the PR number to select one.");
            return Ok(result);
        }
        Selection::Missing => {
            let detail = "No matching PR was found in the retained inventory; this does not establish that no hosted PR exists.";
            if result.source.state == PrContextSourceStateV1::Ready {
                result.source = source(PrContextSourceStateV1::NotFound, detail);
            } else {
                result.source.detail.push(' ');
                result.source.detail.push_str(detail);
            }
            return Ok(result);
        }
    };
    if let Some(identity) = pr.identity.as_ref() {
        let matches_head = identity.observed_head_commit_id.as_str() == head;
        result.pull_request = Some(PrContextPullRequestV1 {
            number: identity.number,
            title: identity.title.clone(),
            state: label(&identity.state)?,
            draft: identity.draft,
            description: identity.description.clone(),
            description_truncated: false,
            observed_head: identity.observed_head_commit_id.to_string(),
            observed_at: identity.observed_at.0,
            matches_head,
        });
        if !matches_head {
            if matches!(
                result.source.state,
                PrContextSourceStateV1::Ready | PrContextSourceStateV1::Partial
            ) {
                result.source.state = PrContextSourceStateV1::Stale;
            }
            result
                .source
                .detail
                .push_str(" The selected PR's retained identity is for a different head commit.");
        }
    } else {
        if result.source.state == PrContextSourceStateV1::Ready {
            result.source.state = PrContextSourceStateV1::Partial;
        }
        result.source.detail.push_str(" Matching PR observations exist, but PR title, description, and state were not retained.");
    }
    result.discussions = timeline
        .review_items
        .iter()
        .filter(|item| item.provider == pr.provider && item.pull_request_id == pr.pull_request_id)
        .filter_map(review_observation)
        .map(|observation| discussion(observation, head))
        .collect::<Result<_>>()?;
    project_conversation(pr, head, &mut result.discussions)?;
    result
        .discussions
        .sort_by(|left, right| discussion_priority(left).cmp(&discussion_priority(right)));
    result.discussions_total = result.discussions.len();
    if timeline.review_items_truncated || timeline.pull_requests_truncated {
        if result.source.state == PrContextSourceStateV1::Ready {
            result.source.state = PrContextSourceStateV1::Partial;
        }
        result.source.detail.push_str(" The retained PR/discussion inventory was truncated; discussion counts are only the selected rows inspected.");
    }
    if result
        .discussions
        .iter()
        .any(|item| item.text.is_none() && item.body_anchor.is_some())
    {
        result.source.detail.push_str(
            " Some comment bodies were not expanded; open supplied provider URLs for context. Body anchors record retained provenance.",
        );
    }
    Ok(result)
}

fn project_conversation(
    pr: &ProjectDeliveryPullRequestV1,
    head: &str,
    discussions: &mut Vec<PrContextDiscussionV1>,
) -> Result<()> {
    let mut latest = BTreeMap::new();
    let mut review_observations: BTreeMap<
        &str,
        (
            &GitHubPullRequestReviewV1,
            &ProjectDeliveryGitHubOperationSnapshotV1,
            bool,
        ),
    > = BTreeMap::new();
    for operation in &pr.operations {
        if !matches!(
            operation.operation,
            GitHubReviewReadOperationV1::RestListPullRequestConversationComments
                | GitHubReviewReadOperationV1::RestListPullRequestReviews
        ) {
            continue;
        }
        let retained = operation
            .last_partial_collection
            .as_ref()
            .or(operation.last_complete.as_ref());
        let newest = retained
            .into_iter()
            .chain(operation.latest_attempt.iter())
            .max_by_key(|snapshot| snapshot.fetched_at.0);
        let Some(newest) = newest else { continue };
        let owns_membership = newest.outcome == GitHubReviewIngressProviderOutcomeV1::Complete
            && newest.coverage == GitHubReviewCoverageV1::Complete;
        for snapshot in retained.into_iter().chain(operation.latest_attempt.iter()) {
            if owns_membership && !std::ptr::eq(snapshot, newest) {
                continue;
            }
            let is_latest = std::ptr::eq(snapshot, newest)
                && matches!(
                    snapshot.outcome,
                    GitHubReviewIngressProviderOutcomeV1::Complete
                        | GitHubReviewIngressProviderOutcomeV1::Partial
                );
            for item in &snapshot.conversation {
                if operation.latest_attempt.as_ref().is_some_and(|latest| {
                    latest
                        .quarantined
                        .binary_search_by(|withheld| {
                            withheld.comment_id.as_str().cmp(item.comment_id.as_str())
                        })
                        .is_ok()
                }) || operation
                    .last_partial_collection
                    .as_ref()
                    .is_some_and(|baseline| {
                        snapshot.fetched_at > baseline.fetched_at
                            && baseline
                                .conversation
                                .binary_search_by(|prior| {
                                    prior.comment_id.as_str().cmp(item.comment_id.as_str())
                                })
                                .is_ok_and(|index| baseline.conversation[index] == *item)
                    })
                {
                    continue;
                }
                let id = format!("conversation:{}", item.comment_id);
                let value = PrContextDiscussionV1 {
                    id: id.clone(),
                    thread_id: None,
                    kind: "conversation".to_owned(),
                    lifecycle: if is_latest { "observed" } else { "retained" }.to_owned(),
                    review_state: "not_applicable".to_owned(),
                    author: item.author.clone(),
                    event_at: provider_event_at(&item.updated_at)
                        .or_else(|| provider_event_at(&item.created_at)),
                    file: None,
                    line: None,
                    text: Some(item.body.clone()),
                    text_truncated: false,
                    url: Some(item.url.clone()),
                    body_anchor: None,
                    observed_at: snapshot.fetched_at.0,
                    matches_head: is_latest && snapshot.provider_head_commit_id.as_str() == head,
                };
                retain_newest(&mut latest, id, value);
            }
            for item in &snapshot.reviews {
                if operation
                    .last_partial_collection
                    .as_ref()
                    .is_some_and(|baseline| {
                        snapshot.fetched_at > baseline.fetched_at
                            && baseline
                                .reviews
                                .binary_search_by(|prior| {
                                    prior.review_id.as_str().cmp(item.review_id.as_str())
                                })
                                .is_ok_and(|index| baseline.reviews[index] == *item)
                    })
                {
                    continue;
                }
                match review_observations.get(item.review_id.as_str()) {
                    Some((_, previous, _)) if previous.fetched_at > snapshot.fetched_at => {}
                    _ => {
                        review_observations
                            .insert(item.review_id.as_str(), (item, snapshot, is_latest));
                    }
                }
            }
        }
    }
    // Reconcile each review's current state before comparing votes across
    // retained and partial observations. A dismissed older ID cannot supersede
    // another vote merely because a complete snapshot once called it decisive.
    let mut decisive_by_author: BTreeMap<String, i64> = BTreeMap::new();
    for (item, _, _) in review_observations.values() {
        if !matches!(
            item.state,
            GitHubReviewStateV1::Approved | GitHubReviewStateV1::ChangesRequested
        ) {
            continue;
        }
        if let (Some(author), Some(submitted_at)) = (
            item.author.as_deref(),
            item.submitted_at.as_deref().and_then(provider_event_at),
        ) {
            decisive_by_author
                .entry(author.to_ascii_lowercase())
                .and_modify(|latest| *latest = (*latest).max(submitted_at))
                .or_insert(submitted_at);
        }
    }
    for (item, snapshot, is_latest) in review_observations.values() {
        let superseded = matches!(
            item.state,
            GitHubReviewStateV1::Approved | GitHubReviewStateV1::ChangesRequested
        ) && item
            .author
            .as_deref()
            .zip(item.submitted_at.as_deref().and_then(provider_event_at))
            .is_some_and(|(author, submitted_at)| {
                decisive_by_author
                    .get(&author.to_ascii_lowercase())
                    .is_some_and(|latest| *latest > submitted_at)
            });
        let id = format!("review:{}", item.review_id);
        latest.insert(
            id.clone(),
            PrContextDiscussionV1 {
                id,
                thread_id: None,
                kind: "review".to_owned(),
                lifecycle: if superseded {
                    "superseded"
                } else if *is_latest {
                    "submitted"
                } else {
                    "retained"
                }
                .to_owned(),
                review_state: label(&item.state)?,
                author: item.author.clone(),
                event_at: item.submitted_at.as_deref().and_then(provider_event_at),
                file: None,
                line: None,
                text: item.body.clone(),
                text_truncated: false,
                url: item.url.clone(),
                body_anchor: None,
                observed_at: snapshot.fetched_at.0,
                matches_head: *is_latest
                    && item
                        .commit_id
                        .as_ref()
                        .is_some_and(|commit| commit.as_str() == head),
            },
        );
    }
    discussions.extend(latest.into_values());
    Ok(())
}

fn retain_newest(
    rows: &mut BTreeMap<String, PrContextDiscussionV1>,
    id: String,
    value: PrContextDiscussionV1,
) {
    match rows.get(&id) {
        Some(previous) if previous.observed_at > value.observed_at => {}
        _ => {
            rows.insert(id, value);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use tracedecay_application::delivery::{
        ProjectDeliveryCiConclusionV1, ProjectDeliveryCiStatusV1,
        ProjectDeliveryFailureLocalizationSourceV1, ProjectDeliveryPullRequestIdentityV1,
        ProjectDeliveryPullRequestOperationV1, ProjectDeliveryPullRequestStateV1,
        ProjectDeliveryReleaseSourceV1, ProjectDeliverySnapshotV1,
    };
    use tracedecay_domain::feedback::{
        CiFailureKindV1, CiFailureRunIdentityV1, FeedbackScopeV1, GitHubConversationCommentV1,
        GitHubPullRequestIdV1, GitHubPullRequestReviewV1, GitHubReviewAuthorClassV1,
        GitHubReviewCommentIdV1, GitHubReviewIdV1, GitHubReviewReadCheckpointV1,
        GitHubReviewStateV1,
    };
    use tracedecay_domain::{
        CanonicalObservationIdV1, CommitId, ProjectId, ProviderId, RepositoryId, RetrievalAnchorId,
        UtcMicros, WorktreeId, canonical_sha256,
    };

    use super::*;

    fn observation(head: &str, at: i64) -> ProjectDeliveryGitHubOperationSnapshotV1 {
        ProjectDeliveryGitHubOperationSnapshotV1 {
            provider_base_commit_id: CommitId::new("commit.parent").unwrap(),
            provider_head_commit_id: CommitId::new(head).unwrap(),
            merge_base_commit_id: CommitId::new("commit.merge-base").unwrap(),
            outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
            coverage: GitHubReviewCoverageV1::Complete,
            quarantined: Vec::new(),
            conversation: Vec::new(),
            reviews: Vec::new(),
            fetched_at: UtcMicros(at),
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    fn pull_request(number: u64, head: &str) -> ProjectDeliveryPullRequestV1 {
        ProjectDeliveryPullRequestV1 {
            provider: ProviderId::new("github").unwrap(),
            pull_request_id: GitHubPullRequestIdV1::new(format!("id-{number}")).unwrap(),
            identity: Some(ProjectDeliveryPullRequestIdentityV1 {
                number,
                title: format!("PR {number}"),
                description: Some("Share startup across remote machines.".to_owned()),
                observed_base_commit_id: CommitId::new("commit.parent").unwrap(),
                observed_head_commit_id: CommitId::new(head).unwrap(),
                observed_at: UtcMicros(20),
                state: ProjectDeliveryPullRequestStateV1::Open,
                draft: false,
                additions: 10,
                deletions: 3,
                changed_files: 2,
            }),
            operations: vec![ProjectDeliveryPullRequestOperationV1 {
                last_partial_collection: None,
                operation: GitHubReviewReadOperationV1::RestGetPullRequest,
                latest_attempt: Some(observation(head, 20)),
                last_complete: None,
            }],
        }
    }

    fn outcome(
        prs: Vec<ProjectDeliveryPullRequestV1>,
        checks: Vec<ProjectDeliveryCiCheckV1>,
    ) -> ProjectDeliveryReadOutcomeV1 {
        ProjectDeliveryReadOutcomeV1::Ready {
            snapshot: Box::new(ProjectDeliverySnapshotV1 {
                scope: FeedbackScopeV1 {
                    project_id: ProjectId::new("project.brief").unwrap(),
                    repository_id: RepositoryId::new("repository.brief").unwrap(),
                    worktree_id: WorktreeId::new("worktree.brief").unwrap(),
                    branch_ref: "refs/heads/topic".to_owned(),
                    head_commit_id: CommitId::new("commit.head").unwrap(),
                },
                expected_head_commit_id: CommitId::new("commit.head").unwrap(),
                github_reviews: ProjectDeliveryGitHubSourceV1::Ready {
                    timeline: ProjectDeliveryGitHubTimelineV1 {
                        pull_requests_total: prs.len(),
                        pull_requests: prs,
                        review_items: Vec::new(),
                        review_items_total: 0,
                        pull_requests_truncated: false,
                        review_items_truncated: false,
                    },
                },
                ci_checks: ProjectDeliveryCiSourceV1::Ready {
                    timeline: ProjectDeliveryCiTimelineV1 {
                        total_retained: checks.len(),
                        checks,
                        truncated: false,
                    },
                },
                failure_localization: ProjectDeliveryFailureLocalizationSourceV1::NotConfigured,
                releases: ProjectDeliveryReleaseSourceV1::Unavailable,
            }),
        }
    }

    #[test]
    fn explicit_selection_resolves_shared_head_and_keeps_identity_and_base_together() {
        let evidence = outcome(
            vec![
                pull_request(42, "commit.head"),
                pull_request(43, "commit.head"),
            ],
            Vec::new(),
        );
        let ambiguous = project_hosted(Some(&evidence), None, "commit.head").unwrap();
        assert_eq!(ambiguous.source.state, PrContextSourceStateV1::Ambiguous);
        assert!(ambiguous.pull_request.is_none());
        assert_eq!(selected_base(Some(&evidence), None, "commit.head"), None);
        let selected = project_hosted(Some(&evidence), Some(43), "commit.head").unwrap();
        let identity = selected.pull_request.unwrap();
        assert_eq!(
            (
                identity.number,
                identity.title.as_str(),
                identity.description.as_deref()
            ),
            (43, "PR 43", Some("Share startup across remote machines."))
        );
        assert!(identity.matches_head);
        assert_eq!(
            selected_base(Some(&evidence), Some(43), "commit.head"),
            Some("commit.parent".to_owned())
        );
        assert_eq!(
            project_hosted(Some(&evidence), Some(99), "commit.head")
                .unwrap()
                .source
                .state,
            PrContextSourceStateV1::NotFound
        );
    }

    #[test]
    fn failed_refresh_cannot_rebind_an_old_description_or_supply_a_stack_base() {
        let mut pr = pull_request(42, "commit.old");
        let mut failure = observation("commit.head", 30);
        failure.outcome = GitHubReviewIngressProviderOutcomeV1::Failed;
        failure.coverage = GitHubReviewCoverageV1::Unavailable;
        failure.provider_base_commit_id = CommitId::new("commit.unproven-fallback").unwrap();
        pr.operations[0].last_complete = pr.operations[0].latest_attempt.take();
        pr.operations[0].latest_attempt = Some(failure);
        let evidence = outcome(vec![pr], Vec::new());
        let result = project_hosted(Some(&evidence), Some(42), "commit.head").unwrap();
        let identity = result.pull_request.unwrap();
        assert_eq!(identity.observed_head, "commit.old");
        assert_eq!(identity.observed_at, 20);
        assert!(!identity.matches_head);
        assert_eq!(result.source.state, PrContextSourceStateV1::Stale);
        assert_eq!(
            selected_base(Some(&evidence), Some(42), "commit.head"),
            None
        );
    }

    #[test]
    fn withheld_description_keeps_the_validated_current_pr_base() {
        let mut pr = pull_request(42, "commit.head");
        pr.identity.as_mut().unwrap().description = None;
        let identity_read = pr.operations[0].latest_attempt.as_mut().unwrap();
        identity_read.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
        identity_read.coverage = GitHubReviewCoverageV1::Partial;
        let mut evidence = outcome(vec![pr], Vec::new());
        let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = &mut evidence else {
            unreachable!();
        };
        let ProjectDeliveryGitHubSourceV1::Ready { timeline } = std::mem::replace(
            &mut snapshot.github_reviews,
            ProjectDeliveryGitHubSourceV1::NotPublished,
        ) else {
            unreachable!();
        };
        snapshot.github_reviews = ProjectDeliveryGitHubSourceV1::Partial { timeline };
        let brief = project_hosted(Some(&evidence), Some(42), "commit.head").unwrap();
        assert_eq!(brief.source.state, PrContextSourceStateV1::Partial);
        let identity = brief.pull_request.unwrap();
        assert_eq!(identity.number, 42);
        assert!(identity.description.is_none());
        assert!(identity.matches_head);
        assert_eq!(
            selected_base(Some(&evidence), Some(42), "commit.head"),
            Some("commit.parent".to_owned()),
        );
        assert_eq!(
            selected_base(Some(&evidence), Some(42), "commit.other"),
            None
        );
    }

    #[test]
    fn unsuccessful_first_use_refresh_preserves_evidence_without_claiming_ready() {
        use tracedecay_application::advisory::GitHubReviewRefreshOutcomeV1;

        use crate::handlers::dashboard_delivery::with_github_refresh_outcome;

        for (refresh, expected_state) in [
            (
                GitHubReviewRefreshOutcomeV1::Unavailable,
                PrContextSourceStateV1::Unavailable,
            ),
            (
                GitHubReviewRefreshOutcomeV1::Cancelled,
                PrContextSourceStateV1::Unavailable,
            ),
            (
                GitHubReviewRefreshOutcomeV1::Denied,
                PrContextSourceStateV1::Denied,
            ),
            (
                GitHubReviewRefreshOutcomeV1::Stale,
                PrContextSourceStateV1::Stale,
            ),
        ] {
            let mut pr = pull_request(42, "commit.head");
            let mut discussion = observation("commit.head", 25);
            discussion
                .conversation
                .push(conversation("Keep the existing route."));
            pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
                last_partial_collection: None,
                operation: GitHubReviewReadOperationV1::RestListPullRequestConversationComments,
                last_complete: None,
                latest_attempt: Some(discussion),
            });
            let retained = outcome(
                vec![pr],
                vec![ci_check(
                    "commit.head",
                    30,
                    ProjectDeliveryCiConclusionV1::Failure,
                )],
            );
            let before = project_hosted(Some(&retained), Some(42), "commit.head").unwrap();
            assert!(before.pull_request.unwrap().description.is_some());
            assert_eq!(before.discussions.len(), 1);
            let evidence = with_github_refresh_outcome(retained, refresh.clone());
            let brief = project_hosted(Some(&evidence), Some(42), "commit.head").unwrap();
            assert_eq!(brief.source.state, expected_state);
            if refresh == GitHubReviewRefreshOutcomeV1::Denied {
                assert!(brief.pull_request.is_none());
                assert!(brief.discussions.is_empty());
                assert_eq!(
                    selected_base(Some(&evidence), Some(42), "commit.head"),
                    None
                );
            } else {
                let identity = brief.pull_request.unwrap();
                assert_eq!(identity.number, 42);
                assert_eq!(identity.observed_at, 20);
                assert!(identity.matches_head);
                assert_eq!(
                    identity.description.as_deref(),
                    Some("Share startup across remote machines.")
                );
                assert_eq!(brief.discussions.len(), 1);
                assert_eq!(
                    brief.discussions[0].text.as_deref(),
                    Some("Keep the existing route.")
                );
            }
            assert_eq!(brief.ci_source.state, PrContextSourceStateV1::Ready);
            assert_eq!(brief.checks.len(), 1);
            assert_eq!(brief.checks[0].observed_at, 30);
            assert_eq!(brief.checks[0].conclusion.as_deref(), Some("failure"));
            assert!(!brief.required_checks_known);

            let mut no_retained_evidence = outcome(Vec::new(), Vec::new());
            let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = &mut no_retained_evidence else {
                unreachable!();
            };
            snapshot.github_reviews = ProjectDeliveryGitHubSourceV1::NotPublished;
            let first_use = with_github_refresh_outcome(no_retained_evidence, refresh);
            let brief = project_hosted(Some(&first_use), None, "commit.head").unwrap();
            assert_eq!(brief.source.state, expected_state);
            assert!(brief.pull_request.is_none());
            assert!(brief.discussions.is_empty());
        }

        for (state, expected, detail) in [
            (
                ProjectDeliveryGitHubSourceV1::Denied {
                    timeline: Default::default(),
                },
                PrContextSourceStateV1::Denied,
                "denied",
            ),
            (
                ProjectDeliveryGitHubSourceV1::RateLimited {
                    timeline: Default::default(),
                    checkpoint: None,
                },
                PrContextSourceStateV1::Partial,
                "rate limited",
            ),
            (
                ProjectDeliveryGitHubSourceV1::Failed {
                    timeline: Default::default(),
                },
                PrContextSourceStateV1::Unavailable,
                "failed",
            ),
        ] {
            let mut retained = outcome(Vec::new(), Vec::new());
            let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = &mut retained else {
                unreachable!();
            };
            snapshot.github_reviews = state;
            let evidence =
                with_github_refresh_outcome(retained, GitHubReviewRefreshOutcomeV1::Unavailable);
            let brief = project_hosted(Some(&evidence), None, "commit.head").unwrap();
            assert_eq!(brief.source.state, expected);
            assert!(brief.source.detail.contains(detail));
        }
    }

    fn conversation(body: &str) -> GitHubConversationCommentV1 {
        GitHubConversationCommentV1 {
            comment_id: GitHubReviewCommentIdV1::new("comment-7").unwrap(),
            author: Some("reviewer".to_owned()),
            author_class: GitHubReviewAuthorClassV1::Maintainer,
            body: body.to_owned(),
            url: "https://github.com/owner/repo/pull/42#issuecomment-7".to_owned(),
            created_at: "2026-10-26T18:00:00Z".to_owned(),
            updated_at: "2026-10-26T18:10:00Z".to_owned(),
        }
    }

    #[test]
    fn conversation_edits_and_review_votes_are_visible_without_inline_comments() {
        let mut pr = pull_request(42, "commit.head");
        let mut old = observation("commit.old", 21);
        old.conversation
            .push(conversation("Use a second migration."));
        let mut current = observation("commit.head", 30);
        current
            .conversation
            .push(conversation("Keep a single migration for this release."));
        pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
            last_partial_collection: None,
            operation: GitHubReviewReadOperationV1::RestListPullRequestConversationComments,
            last_complete: Some(old),
            latest_attempt: Some(current),
        });
        let mut reviews = observation("commit.head", 31);
        for (id, state, commit, submitted) in [
            (
                "review-1",
                GitHubReviewStateV1::ChangesRequested,
                "commit.old",
                "2026-10-26T18:00:00Z",
            ),
            (
                "review-2",
                GitHubReviewStateV1::Approved,
                "commit.head",
                "2026-10-26T18:10:00Z",
            ),
        ] {
            reviews.reviews.push(GitHubPullRequestReviewV1 {
                review_id: GitHubReviewIdV1::new(id).unwrap(),
                state,
                author: Some("reviewer".to_owned()),
                body: None,
                commit_id: Some(CommitId::new(commit).unwrap()),
                submitted_at: Some(submitted.to_owned()),
                url: None,
            });
        }
        pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
            last_partial_collection: None,
            operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
            last_complete: None,
            latest_attempt: Some(reviews),
        });
        let result =
            project_hosted(Some(&outcome(vec![pr], Vec::new())), None, "commit.head").unwrap();
        assert_eq!(result.discussions.len(), 3);
        let comment = result
            .discussions
            .iter()
            .find(|row| row.kind == "conversation")
            .unwrap();
        assert_eq!(
            comment.text.as_deref(),
            Some("Keep a single migration for this release.")
        );
        assert!(comment.matches_head);
        assert!(comment.body_anchor.is_none());
        assert!(comment.file.is_none());
        let approval = result
            .discussions
            .iter()
            .find(|row| row.review_state == "approved")
            .unwrap();
        assert!(approval.matches_head);
        assert_eq!(approval.lifecycle, "submitted");
        let old_vote = result
            .discussions
            .iter()
            .find(|row| row.review_state == "changes_requested")
            .unwrap();
        assert!(!old_vote.matches_head);
        assert_eq!(old_vote.lifecycle, "superseded");
        assert_eq!(result.discussions_total, 3);
    }

    #[test]
    fn only_later_decisive_reviews_supersede_votes() {
        for first_state in [
            GitHubReviewStateV1::ChangesRequested,
            GitHubReviewStateV1::Approved,
        ] {
            for later_state in [GitHubReviewStateV1::Commented, GitHubReviewStateV1::Pending] {
                let review = |id: &str, state, minute| GitHubPullRequestReviewV1 {
                    review_id: GitHubReviewIdV1::new(id).unwrap(),
                    state,
                    author: Some("reviewer".to_owned()),
                    body: None,
                    commit_id: Some(CommitId::new("commit.head").unwrap()),
                    submitted_at: Some(format!("2026-10-26T18:{minute:02}:00Z")),
                    url: None,
                };
                let mut observations = observation("commit.head", 30);
                observations.reviews = vec![
                    review("vote", first_state, 0),
                    review("non-decisive", later_state, 1),
                ];
                let mut pr = pull_request(42, "commit.head");
                pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
                    last_partial_collection: None,
                    operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
                    last_complete: None,
                    latest_attempt: Some(observations),
                });
                let mut rows = Vec::new();
                project_conversation(&pr, "commit.head", &mut rows).unwrap();
                let vote = rows.iter().find(|row| row.id == "review:vote").unwrap();
                assert_eq!(vote.lifecycle, "submitted");
                assert_eq!(vote.review_state, label(&first_state).unwrap());

                let opposite = match first_state {
                    GitHubReviewStateV1::Approved => GitHubReviewStateV1::ChangesRequested,
                    _ => GitHubReviewStateV1::Approved,
                };
                let operation = pr.operations.last_mut().unwrap();
                let latest = operation.latest_attempt.as_mut().unwrap();
                latest.reviews.push(review("decisive", opposite, 2));
                rows.clear();
                project_conversation(&pr, "commit.head", &mut rows).unwrap();
                assert_eq!(
                    rows.iter()
                        .find(|row| row.id == "review:vote")
                        .unwrap()
                        .lifecycle,
                    "superseded"
                );
                assert_eq!(
                    rows.iter()
                        .find(|row| row.id == "review:decisive")
                        .unwrap()
                        .lifecycle,
                    "submitted"
                );

                // A later partial observation may dismiss the same review ID.
                // Its observed state wins over an older complete collection.
                let operation = pr.operations.last_mut().unwrap();
                operation.last_complete = operation.latest_attempt.clone();
                let latest = operation.latest_attempt.as_mut().unwrap();
                latest.fetched_at = UtcMicros(40);
                latest.coverage = GitHubReviewCoverageV1::Partial;
                latest.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
                latest.reviews = vec![review("vote", GitHubReviewStateV1::Dismissed, 0)];
                rows.clear();
                project_conversation(&pr, "commit.head", &mut rows).unwrap();
                let vote = rows.iter().find(|row| row.id == "review:vote").unwrap();
                assert_eq!(vote.review_state, "dismissed");
                assert_eq!(vote.observed_at, 40);
            }
        }
    }

    #[test]
    fn partial_review_votes_supersede_retained_votes_after_reconciling_dismissals() {
        let review = |id: &str, state, minute| GitHubPullRequestReviewV1 {
            review_id: GitHubReviewIdV1::new(id).unwrap(),
            state,
            author: Some("reviewer".to_owned()),
            body: None,
            commit_id: Some(CommitId::new("commit.head").unwrap()),
            submitted_at: Some(format!("2026-10-26T18:{minute:02}:00Z")),
            url: None,
        };
        let mut retained = observation("commit.head", 30);
        retained
            .reviews
            .push(review("old-vote", GitHubReviewStateV1::ChangesRequested, 0));
        retained.reviews[0].author = Some("Reviewer".to_owned());
        let mut partial = observation("commit.head", 40);
        partial.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
        partial.coverage = GitHubReviewCoverageV1::Partial;
        partial
            .reviews
            .push(review("approval", GitHubReviewStateV1::Approved, 1));
        partial.reviews[0].submitted_at = Some("2026-10-26T11:01:00-07:00".to_owned());
        let mut pr = pull_request(42, "commit.head");
        pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
            last_partial_collection: None,
            operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
            last_complete: Some(retained),
            latest_attempt: Some(partial),
        });
        let mut rows = Vec::new();
        project_conversation(&pr, "commit.head", &mut rows).unwrap();
        let old = rows.iter().find(|row| row.id == "review:old-vote").unwrap();
        assert_eq!(old.lifecycle, "superseded");
        assert_eq!(old.observed_at, 30);
        assert!(!old.matches_head);
        let approval = rows.iter().find(|row| row.id == "review:approval").unwrap();
        assert_eq!(approval.lifecycle, "submitted");
        assert!(approval.matches_head);

        // If the retained complete collection also has that approval, its
        // later observed dismissal must remove its superseding authority.
        let operation = pr.operations.last_mut().unwrap();
        operation
            .last_complete
            .as_mut()
            .unwrap()
            .reviews
            .push(review("approval", GitHubReviewStateV1::Approved, 1));
        operation.latest_attempt.as_mut().unwrap().reviews[0].state =
            GitHubReviewStateV1::Dismissed;
        rows.clear();
        project_conversation(&pr, "commit.head", &mut rows).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|row| row.id == "review:old-vote")
                .unwrap()
                .lifecycle,
            "retained"
        );
        let dismissed = rows.iter().find(|row| row.id == "review:approval").unwrap();
        assert_eq!(dismissed.review_state, "dismissed");
        assert_eq!(dismissed.observed_at, 40);
    }

    #[test]
    fn agreed_partial_membership_excludes_deleted_votes_and_preserves_observation_time() {
        let review = |id: &str, state, author: &str, minute| GitHubPullRequestReviewV1 {
            review_id: GitHubReviewIdV1::new(id).unwrap(),
            state,
            author: Some(author.to_owned()),
            body: None,
            commit_id: Some(CommitId::new("commit.head").unwrap()),
            submitted_at: Some(format!("2026-10-26T18:{minute:02}:00Z")),
            url: None,
        };
        let mut complete = observation("commit.head", 10);
        complete.reviews = vec![
            review("1", GitHubReviewStateV1::ChangesRequested, "alice", 0),
            review("2", GitHubReviewStateV1::Approved, "bob", 1),
            review("3", GitHubReviewStateV1::Approved, "alice", 2),
        ];
        let mut baseline = complete.clone();
        baseline.fetched_at = UtcMicros(20);
        baseline.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
        baseline.coverage = GitHubReviewCoverageV1::Partial;
        baseline.reviews.truncate(2);
        let mut pr = pull_request(42, "commit.head");
        pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
            operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
            last_complete: Some(complete),
            last_partial_collection: Some(baseline.clone()),
            latest_attempt: Some(baseline),
        });
        let mut rows = Vec::new();
        project_conversation(&pr, "commit.head", &mut rows).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["review:1", "review:2"]
        );
        assert!(
            rows.iter()
                .all(|row| row.lifecycle == "submitted" && row.observed_at == 20)
        );

        let operation = pr.operations.last_mut().unwrap();
        let latest = operation.latest_attempt.as_mut().unwrap();
        latest.outcome = GitHubReviewIngressProviderOutcomeV1::RateLimited;
        latest.fetched_at = UtcMicros(30);
        rows.clear();
        project_conversation(&pr, "commit.head", &mut rows).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.lifecycle == "retained" && row.observed_at == 20)
        );
        assert!(rows.iter().all(|row| !row.matches_head));

        let operation = pr.operations.last_mut().unwrap();
        let latest = operation.latest_attempt.as_mut().unwrap();
        latest.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
        latest.fetched_at = UtcMicros(40);
        latest
            .reviews
            .push(review("4", GitHubReviewStateV1::Commented, "carol", 3));
        rows.clear();
        project_conversation(&pr, "commit.head", &mut rows).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["review:1", "review:2", "review:4"]
        );
        assert_eq!(
            rows[0].lifecycle, "retained",
            "the deleted approval must not supersede Alice's current vote"
        );
        assert_eq!(rows[0].review_state, "changes_requested");
        assert_eq!(rows[0].observed_at, 20);
        assert_eq!(rows[1].observed_at, 20);
        assert_eq!(rows[2].lifecycle, "submitted");
        assert_eq!(rows[2].observed_at, 40);
        assert_eq!(
            pr.operations
                .last()
                .unwrap()
                .latest_attempt
                .as_ref()
                .unwrap()
                .coverage,
            GitHubReviewCoverageV1::Partial
        );
    }

    fn ci_check(
        head: &str,
        at: i64,
        conclusion: ProjectDeliveryCiConclusionV1,
    ) -> ProjectDeliveryCiCheckV1 {
        ProjectDeliveryCiCheckV1 {
            observation_id: CanonicalObservationIdV1::new(
                canonical_sha256(&("pr-context-ci-check", head, at))
                    .unwrap()
                    .as_str(),
            )
            .unwrap(),
            run: CiFailureRunIdentityV1 {
                workflow_id: "workflow-1".to_owned(),
                job_id: "job-1".to_owned(),
                check_suite_id: "suite-1".to_owned(),
                check_run_id: "check-1".to_owned(),
                run_id: "run-1".to_owned(),
                attempt_id: "attempt-1".to_owned(),
            },
            workflow_path: ".github/workflows/test.yml".to_owned(),
            workflow_status: ProjectDeliveryCiStatusV1::Completed,
            workflow_conclusion: Some(conclusion),
            job_status: ProjectDeliveryCiStatusV1::Completed,
            job_conclusion: Some(conclusion),
            check_status: ProjectDeliveryCiStatusV1::Completed,
            check_conclusion: Some(conclusion),
            failed_step: None,
            annotations: Vec::new(),
            annotation_count: 0,
            provider_head_sha: head.to_owned(),
            failure_anchor: RetrievalAnchorId::new(format!("anchor.{at}")).unwrap(),
            failure_kind: CiFailureKindV1::Unknown,
            observed_at: UtcMicros(at),
        }
    }

    #[test]
    fn recent_discussion_events_rank_before_older_comments_from_the_same_fetch() {
        let mut old = conversation("Earlier discussion.");
        old.comment_id = GitHubReviewCommentIdV1::new("1").unwrap();
        old.author = Some("EarlierReviewer".to_owned());
        old.created_at = "2026-10-26T17:00:00Z".to_owned();
        old.updated_at = "2026-10-26T18:00:00Z".to_owned();
        let mut recent = conversation("Latest clarification.");
        recent.comment_id = GitHubReviewCommentIdV1::new("99").unwrap();
        recent.author = Some("Maintainer".to_owned());
        recent.updated_at = "2026-10-26T11:30:00.123456-07:00".to_owned();
        let mut unknown = conversation("Legacy comment with no usable event time.");
        unknown.comment_id = GitHubReviewCommentIdV1::new("0").unwrap();
        unknown.created_at = "unknown".to_owned();
        unknown.updated_at = "unknown".to_owned();
        let mut observations = observation("commit.head", 40);
        observations.conversation = vec![old, recent, unknown];
        let mut pr = pull_request(42, "commit.head");
        pr.operations.push(ProjectDeliveryPullRequestOperationV1 {
            last_partial_collection: None,
            operation: GitHubReviewReadOperationV1::RestListPullRequestConversationComments,
            last_complete: None,
            latest_attempt: Some(observations),
        });
        let result =
            project_hosted(Some(&outcome(vec![pr], Vec::new())), None, "commit.head").unwrap();
        let rows = result.discussions;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "conversation:99");
        assert_eq!(rows[0].author.as_deref(), Some("Maintainer"));
        assert!(rows[0].event_at.is_some());
        assert_eq!(
            rows[0].event_at,
            provider_event_at("2026-10-26T18:30:00.123456Z")
        );
        assert!(rows[0].event_at > rows[1].event_at);
        assert_eq!(rows[1].id, "conversation:1");
        assert_eq!(rows[2].id, "conversation:0");
        assert!(rows[2].event_at.is_none());
        assert!(rows.iter().all(|row| row.observed_at == 40));
    }

    #[test]
    fn check_updates_are_deduplicated_per_commit_and_never_imply_all_required_checks_pass() {
        let evidence = outcome(
            vec![pull_request(42, "commit.head")],
            vec![
                ci_check("commit.old", 10, ProjectDeliveryCiConclusionV1::Failure),
                ci_check("commit.head", 20, ProjectDeliveryCiConclusionV1::Failure),
                ci_check("commit.head", 30, ProjectDeliveryCiConclusionV1::Success),
            ],
        );
        let result = project_hosted(Some(&evidence), None, "commit.head").unwrap();
        assert_eq!(result.checks.len(), 2);
        assert_eq!(result.checks[0].conclusion.as_deref(), Some("success"));
        assert!(result.checks[0].matches_head);
        assert_eq!(result.checks[1].conclusion.as_deref(), Some("failure"));
        assert!(!result.checks[1].matches_head);
        assert!(!result.required_checks_known);
        assert!(result.ci_source.detail.contains("required checks"));
    }

    #[test]
    fn absent_and_denied_hosted_sources_are_not_empty_ready_snapshots() {
        assert_eq!(
            project_hosted(None, None, "commit.head")
                .unwrap()
                .source
                .state,
            PrContextSourceStateV1::NotMounted
        );
        assert_eq!(
            project_hosted(
                Some(&ProjectDeliveryReadOutcomeV1::Denied),
                None,
                "commit.head"
            )
            .unwrap()
            .source
            .state,
            PrContextSourceStateV1::Denied
        );
        assert_eq!(selected_base(None, None, "commit.head"), None);

        let mut retained = outcome(
            vec![pull_request(42, "commit.head")],
            vec![ci_check(
                "commit.head",
                30,
                ProjectDeliveryCiConclusionV1::Success,
            )],
        );
        let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = &mut retained else {
            unreachable!();
        };
        let ProjectDeliveryGitHubSourceV1::Ready { timeline } = std::mem::replace(
            &mut snapshot.github_reviews,
            ProjectDeliveryGitHubSourceV1::NotPublished,
        ) else {
            unreachable!();
        };
        assert!(
            timeline.pull_requests[0]
                .identity
                .as_ref()
                .unwrap()
                .description
                .is_some()
        );
        snapshot.github_reviews = ProjectDeliveryGitHubSourceV1::Denied { timeline };
        let denied = project_hosted(Some(&retained), Some(42), "commit.head").unwrap();
        assert_eq!(denied.source.state, PrContextSourceStateV1::Denied);
        assert!(denied.pull_request.is_none());
        assert!(denied.discussions.is_empty());
        assert_eq!(denied.ci_source.state, PrContextSourceStateV1::Ready);
        assert_eq!(denied.checks.len(), 1);
        assert_eq!(
            selected_base(Some(&retained), Some(42), "commit.head"),
            None
        );
    }
}
