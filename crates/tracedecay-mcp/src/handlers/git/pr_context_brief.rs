//! One bounded orientation packet for the PR's discussion, state, and code.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Instant;

use serde_json::Value;
use tracedecay_application::git_intelligence::NativeGitIntelligence;
use tracedecay_contracts::git::GIT_QUERY_DEFAULT_MAX_BYTES;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    GitFileRoleV1, GitToolOperationV1, PrContextBriefBudgetV1, PrContextBriefV1,
    PrContextEntryPointV1, PrContextImpactV1, PrContextLocationV1, PrContextQualityV1,
    PrContextResultV1, PrContextSourceStateV1, PrContextSourceV1, PrContextSurfaceRequestV1,
    PrContextViewV1, ServedCodeGraphGenerationV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{CommitId, RelationEdgeKindV1};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_runtime_core::cancellation::CancellationToken;

use crate::handlers::graph::{graph_read_freshness, graph_tool_completion};
use crate::handlers::support::decode_primitive_request;
use crate::handlers::token_budget::estimated_tokens;
use crate::tool_context::McpToolContext;

use super::context::{
    CancelGitReadOnDrop, PrContextControls, blocking_git_span_controlled,
    graph_enrichment_is_transient, pr_context_graph_matches_head,
};
use super::pr_context_hosted::{project_hosted, selected_base};
use super::pr_context_source::compare_source;
use super::shell::{default_pr_base_ref, git_pr_head, git_pr_span_controlled};
use super::{context, git_failure};

const DEFAULT_BRIEF_TOKENS: u32 = 4_000;
const MAX_NAVIGATION_SEEDS: usize = 200;
const MAX_NAVIGATION_RELATIONS: usize = 128;

#[tracing::instrument(name = "mcp.pr_context.brief", level = "trace", skip_all)]
pub async fn compute_pr_context<F>(
    ctx: &McpToolContext<'_>,
    graph: F,
    args: Value,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<VerifiedGraphQuery>>,
{
    let request: PrContextSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_pr_context")?;
    if request.view == PrContextViewV1::Symbols {
        if request.refresh_github {
            return Err(TraceDecayError::Config {
                message: "refresh_github requires pr_context view: brief".to_owned(),
            });
        }
        if request.pull_request.is_some() {
            return Err(TraceDecayError::Config { message: "pull_request selects hosted context in view: brief; use explicit base_ref and head_ref for view: symbols".to_owned() });
        }
        return context::compute_pr_context(ctx, graph, args).await;
    }
    if request.cursor.is_some() || request.maximum_symbols.is_some() {
        return Err(TraceDecayError::Config { message: "cursor and maximum_symbols require pr_context view: symbols; use budget_tokens to size the briefing".to_owned() });
    }
    let controls = PrContextControls {
        deadline: ctx.deadline().cloned(),
        cancellation: ctx.cancellation().cloned(),
    };
    controls.checkpoint()?;
    let head = request.head_ref.unwrap_or_else(|| "HEAD".to_owned());
    let root = ctx.project_root().to_path_buf();
    let reference = head.clone();
    let head_oid = blocking_git_span_controlled(
        "PR head",
        controls.cancellation.clone(),
        controls.deadline.clone(),
        move |_| git_pr_head(&root, &reference),
    )
    .await?;
    controls.checkpoint()?;
    let head_oid = match head_oid {
        Ok(head) => head,
        Err(message) => return Ok(failed_git(message)),
    };
    let hosted_outcome = match ctx.pr_context_delivery() {
        Some(reader) => Some(
            reader(
                CommitId::new(head_oid.clone()).map_err(|error| TraceDecayError::Config {
                    message: error.to_string(),
                })?,
                request.refresh_github,
            )
            .await,
        ),
        None => None,
    };
    controls.checkpoint()?;
    let mut hosted = project_hosted(hosted_outcome.as_ref(), request.pull_request, &head_oid)?;
    if !request.refresh_github {
        hosted.source.detail.push_str(
            " No GitHub refresh was requested; available observations are retained evidence.",
        );
    }
    let base = request
        .base_ref
        .or_else(|| selected_base(hosted_outcome.as_ref(), request.pull_request, &head_oid))
        .unwrap_or_else(|| default_pr_base_ref(ctx.project_root()));
    let root = ctx.project_root().to_path_buf();
    let base_ref = base.clone();
    let pinned_head = head_oid.clone();
    let comparison = blocking_git_span_controlled(
        "PR comparison",
        controls.cancellation.clone(),
        controls.deadline.clone(),
        move |cancelled| git_pr_span_controlled(&root, &base_ref, &pinned_head, cancelled),
    )
    .await?;
    controls.checkpoint()?;
    let comparison = match comparison {
        Ok(comparison) => comparison,
        Err(message) => return Ok(failed_git(message)),
    };

    let cancel = CancellationToken::new();
    let _cancel_on_drop = CancelGitReadOnDrop(cancel.clone());
    let deadline = ctx
        .deadline()
        .and_then(tracedecay_daemon_protocol::deadline_remaining)
        .map(|remaining| Instant::now() + remaining);
    let git = NativeGitIntelligence::new(
        ctx.project_root(),
        ctx.admitted_scope().repository_id.clone(),
        ctx.admitted_scope().worktree_id.clone(),
    )
    .with_execution_bounds(
        deadline,
        Some(cancel.clone()),
        GIT_QUERY_DEFAULT_MAX_BYTES as usize,
    );
    let merge_base = comparison.merge_base.clone();
    let pinned_head = head_oid.clone();
    let source_policy = ctx.index_path_policy().clone();
    let mut work = std::pin::pin!(blocking_git_span_controlled(
        "PR declarations",
        controls.cancellation.clone(),
        controls.deadline.clone(),
        move |cancelled| {
            compare_source(&git, &merge_base, &pinned_head, &source_policy, cancelled)
        }
    ));
    let source = match ctx.cancellation() {
        Some(signal) => tokio::select! {
            biased;
            result = &mut work => result,
            () = signal.cancelled() => { cancel.cancel(); work.await }
        },
        None => work.await,
    }?;
    controls.checkpoint()?;
    let source = match source {
        Ok(source) => source,
        Err(message) => return Ok(failed_git(message)),
    };
    let touched_files = source
        .changes
        .iter()
        .map(|change| change.file.clone())
        .collect();
    let clean_at_head = source.clean_at_head;
    let mut brief = PrContextBriefV1 {
        view: PrContextViewV1::Brief,
        base,
        head: head.clone(),
        base_oid: comparison.base_oid,
        head_oid,
        merge_base: comparison.merge_base,
        files_changed: source.changes.len(),
        additions: source.additions,
        deletions: source.deletions,
        commits: comparison.commits,
        hosted,
        areas: source.areas,
        changes: source.changes,
        entry_points: source.entries,
        impact: Vec::new(),
        quality: source.quality,
        source_analysis: source.source,
        graph_analysis: unavailable("Graph enrichment has not been read."),
        next_steps: Vec::new(),
        budget: PrContextBriefBudgetV1 {
            requested_tokens: request.budget_tokens.unwrap_or(DEFAULT_BRIEF_TOKENS),
            estimated_tokens: 0,
            over_budget: false,
            files_omitted: 0,
            entry_points_omitted: 0,
            discussions_omitted: 0,
            checks_omitted: 0,
            impact_omitted: 0,
            commits_omitted: 0,
            areas_omitted: 0,
            quality_omitted: 0,
        },
        freshness: None,
    };
    prioritize_entries(&mut brief);
    let mut graph_cost = None;
    match graph.await {
        Ok(graph) => {
            ctx.verify_graph_scope(&graph)?;
            if !clean_at_head || graph.freshness().is_stale() {
                brief.graph_analysis = PrContextSourceV1 { state: PrContextSourceStateV1::Stale, detail: "The graph or checkout differs from the committed comparison. Direct-consumer navigation is omitted.".to_owned() };
            } else {
                match pr_context_graph_matches_head(
                    ctx,
                    "HEAD",
                    &brief.head_oid,
                    graph.generation().as_str(),
                )
                .await
                {
                    Ok(true) => match enrich_navigation(&mut brief, &graph, &controls) {
                        Ok(()) => {}
                        Err(error)
                            if graph_enrichment_is_transient(&error)
                                || matches!(
                                    error.project_route_context(),
                                    Some(("code-graph-budget-exhausted", _, _))
                                ) =>
                        {
                            brief.graph_analysis = unavailable(&error.to_string());
                        }
                        Err(error) => return Err(error),
                    },
                    Ok(false) => {
                        brief.graph_analysis = unavailable(
                            "The graph generation does not match the exact head snapshot.",
                        );
                    }
                    Err(reason) => {
                        brief.graph_analysis = unavailable(&format!(
                            "Exact head graph provenance is unavailable: {reason}."
                        ));
                    }
                }
            }
            brief.freshness = Some(graph_read_freshness(
                &ServedCodeGraphGenerationV1 {
                    generation: graph.generation().as_str().to_owned(),
                    freshness: graph.freshness(),
                    worktree: None,
                },
                ctx.freshness().await.as_ref(),
            ));
            graph_cost = Some(graph.read_cost());
        }
        Err(error)
            if graph_enrichment_is_transient(&error)
                || matches!(
                    error.project_route_context(),
                    Some(("code-graph-parked", _, _))
                ) =>
        {
            brief.graph_analysis = unavailable(&error.to_string());
        }
        Err(error) => return Err(error),
    }
    controls.checkpoint()?;
    add_quality_and_next_steps(&mut brief);
    fit_budget(&mut brief)?;
    let mut completion = graph_tool_completion(
        GraphToolResultV1::PrContext(PrContextResultV1::Brief(Box::new(brief))),
        touched_files,
    );
    completion.cost = graph_cost;
    Ok(completion)
}

fn failed_git(message: String) -> GraphToolCompletionV1 {
    graph_tool_completion(
        GraphToolResultV1::PrContext(PrContextResultV1::GitFailure(git_failure(
            GitToolOperationV1::Diff,
            message,
        ))),
        Vec::new(),
    )
}

fn unavailable(detail: &str) -> PrContextSourceV1 {
    PrContextSourceV1 {
        state: PrContextSourceStateV1::Unavailable,
        detail: detail.to_owned(),
    }
}

fn targets(entry: &PrContextEntryPointV1, file: &str, line: Option<u64>) -> bool {
    entry.location.file == file
        && line.is_none_or(|line| {
            (u64::from(entry.location.line)..=u64::from(entry.location.end_line)).contains(&line)
        })
}

fn prioritize_entries(brief: &mut PrContextBriefV1) {
    let priorities = brief
        .entry_points
        .iter_mut()
        .map(|entry| {
            let review = entry.revision == brief.head_oid
                && brief.hosted.discussions.iter().any(|discussion| {
                    discussion.matches_head
                        && !matches!(
                            discussion.lifecycle.as_str(),
                            "resolved" | "outdated" | "deleted"
                        )
                        && discussion
                            .file
                            .as_deref()
                            .is_some_and(|file| targets(entry, file, discussion.line))
                });
            let check = entry.revision == brief.head_oid
                && brief.hosted.checks.iter().any(|check| {
                    check.matches_head
                        && check.locations.iter().any(|location| {
                            targets(entry, &location.file, Some(u64::from(location.line)))
                        })
                });
            let priority = if review {
                entry.reason = format!(
                    "Retained review discussion for this head targets this location. {}",
                    entry.reason
                );
                0
            } else if check {
                entry.reason = format!(
                    "A retained CI annotation targets this location. {}",
                    entry.reason
                );
                1
            } else {
                match entry.change.as_str() {
                    "signature_changed" | "visibility_changed" => 2,
                    "removed" => 3,
                    "added" => 4,
                    "modified" => 5,
                    "changed_overload" => 6,
                    "file_change" => 7,
                    _ => 8,
                }
            };
            (
                (
                    entry.location.file.clone(),
                    entry.location.line,
                    entry.revision.clone(),
                    entry.symbol.clone(),
                ),
                priority,
            )
        })
        .collect::<HashMap<_, _>>();
    brief.entry_points.sort_by_key(|entry| {
        (
            priorities
                .get(&(
                    entry.location.file.clone(),
                    entry.location.line,
                    entry.revision.clone(),
                    entry.symbol.clone(),
                ))
                .copied()
                .unwrap_or(9),
            tracedecay_code_index::is_test_file(&entry.location.file),
            entry.location.file.clone(),
            entry.location.line,
        )
    });
    let mut file_order = HashMap::new();
    for (index, entry) in brief.entry_points.iter().enumerate() {
        file_order
            .entry(entry.location.file.as_str())
            .or_insert(index);
    }
    brief.changes.sort_by_key(|file| {
        file_order
            .get(file.file.as_str())
            .copied()
            .or_else(|| {
                file.previous_file
                    .as_deref()
                    .and_then(|path| file_order.get(path).copied())
            })
            .unwrap_or(usize::MAX)
    });
}

fn enrich_navigation(
    brief: &mut PrContextBriefV1,
    graph: &VerifiedGraphQuery,
    controls: &PrContextControls,
) -> Result<()> {
    let eligible = brief
        .entry_points
        .iter()
        .filter(|entry| entry.revision == brief.head_oid && entry.change != "file_change")
        .count();
    let entries = brief
        .entry_points
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.revision == brief.head_oid && entry.change != "file_change")
        .take(MAX_NAVIGATION_SEEDS)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let paths = entries
        .iter()
        .map(|&index| brief.entry_points[index].location.file.clone())
        .collect::<HashSet<_>>();
    let page = graph.symbols_in_logical_files_page(&paths, None, 500_000, 500_000)?;
    controls.checkpoint()?;
    let mut seeds = Vec::new();
    let mut names = Vec::new();
    let mut by_location = HashMap::<_, Vec<_>>::new();
    for symbol in &page.symbols {
        if let (Some(binding), Some(metadata)) = (&symbol.binding, &symbol.metadata)
            && let Some(path) = binding.logical_path.as_deref()
        {
            by_location
                .entry((
                    path,
                    metadata.qualified_name.as_str(),
                    metadata.start_line + 1,
                ))
                .or_default()
                .push(&symbol.occurrence);
        }
    }
    for index in entries {
        let entry = &mut brief.entry_points[index];
        if let Some(matches) = by_location.get(&(
            entry.location.file.as_str(),
            entry.symbol.as_str(),
            entry.location.line,
        )) && let [occurrence] = matches.as_slice()
        {
            seeds.push((*occurrence).clone());
            names.push(entry.symbol.clone());
            entry.node_id = Some(occurrence.as_str().to_owned());
        }
    }
    let relations = graph.callers_truncated(
        &seeds,
        &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
        MAX_NAVIGATION_RELATIONS,
    )?;
    let changed_paths = brief
        .changes
        .iter()
        .map(|change| change.file.as_str())
        .collect::<HashSet<_>>();
    for (name, relations) in names.iter().zip(relations) {
        controls.checkpoint()?;
        for relation in relations {
            let (Some(binding), Some(metadata)) =
                (relation.neighbor.binding, relation.neighbor.metadata)
            else {
                continue;
            };
            let Some(path) = binding.logical_path else {
                continue;
            };
            brief.impact.push(PrContextImpactV1 {
                changed_symbol: name.clone(),
                consumer: metadata.qualified_name.as_str().to_owned(),
                node_id: Some(relation.neighbor.occurrence.as_str().to_owned()),
                consumer_changed: changed_paths.contains(path.as_str()),
                location: PrContextLocationV1 {
                    file: path,
                    line: metadata.start_line + 1,
                    end_line: metadata
                        .start_line
                        .saturating_add(metadata.line_span)
                        .max(metadata.start_line + 1),
                },
                relation: relation.edge.kind.as_str().to_owned(),
            });
        }
    }
    brief.impact.sort_by_key(|impact| {
        (
            impact.consumer_changed,
            impact.location.file.clone(),
            impact.location.line,
            impact.changed_symbol.clone(),
        )
    });
    brief.impact.dedup();
    brief.graph_analysis = PrContextSourceV1 {
        state: PrContextSourceStateV1::Partial,
        detail: format!(
            "Exact head graph; direct consumer navigation for {} of {eligible} changed head declarations, up to {MAX_NAVIGATION_RELATIONS} relations. This is bounded navigation, not complete transitive impact or deleted-symbol coverage. Symbol page truncated: {}.",
            seeds.len(),
            page.has_more
        ),
    };
    Ok(())
}

fn add_quality_and_next_steps(brief: &mut PrContextBriefV1) {
    let test_paths = brief
        .changes
        .iter()
        .filter(|change| change.role == GitFileRoleV1::Test)
        .count();
    let contracts = brief
        .entry_points
        .iter()
        .filter(|entry| {
            matches!(
                entry.change.as_str(),
                "signature_changed" | "visibility_changed" | "removed"
            )
        })
        .count();
    brief.quality.insert(0, PrContextQualityV1 { signal: "test_changes".to_owned(), observation: format!("{test_paths} changed test paths; inline tests and test execution are not inferred from this count.") });
    brief.quality.insert(1, PrContextQualityV1 { signal: "declaration_contracts".to_owned(), observation: format!("{contracts} removed declarations or changed signatures/visibility. Inspect the reading entrypoints and their consumers.") });
    if !brief.hosted.required_checks_known {
        brief.quality.push(PrContextQualityV1 { signal: "merge_readiness_unknown".to_owned(), observation: "Retained CI observations do not enumerate all required checks; merge readiness is unknown.".to_owned() });
    }
    brief.next_steps.push("Read the ranked entry_points and their before/after excerpts first. Expand missing code with source_lines using the recorded full revision, file, start_line, and end_line; use source_body for an attached node_id.".to_owned());
    if !brief.hosted.discussions.is_empty() {
        brief.next_steps.push("Open supplied discussion URLs for provider context. body_anchor records retained provenance; feedback_expand requires a server-minted finding expansion handle.".to_owned());
    }
    if !brief.impact.is_empty() {
        brief.next_steps.push("Read the listed consumers, prioritizing those outside changed files; use impact on a specific changed symbol for deeper traversal.".to_owned());
    }
    brief.next_steps.push("Use refresh_github: true when a new hosted observation is needed. Use view: symbols for indexed symbol pages, or increase budget_tokens to expand this briefing.".to_owned());
}

/// Budget the entire structured result, including source states and omission
/// counters. Keep identity and one high-priority item from each evidence lane;
/// disclose when that minimum packet itself exceeds the requested budget.
pub(super) fn fit_budget(brief: &mut PrContextBriefV1) -> Result<()> {
    // Stored descriptions and conversations may be large. Presentation
    // excerpts preserve the original retained evidence and its retrieval URL.
    let prose_bytes = (brief.budget.requested_tokens as usize).clamp(128, 4_000);
    if let Some(pr) = &mut brief.hosted.pull_request
        && let Some(description) = &mut pr.description
    {
        pr.description_truncated |= shorten_text(description, prose_bytes);
    }
    for discussion in &mut brief.hosted.discussions {
        if let Some(text) = &mut discussion.text {
            discussion.text_truncated |= shorten_text(text, prose_bytes);
        }
    }
    let mut estimate = estimated_tokens(brief)?;
    loop {
        if estimate <= brief.budget.requested_tokens {
            brief.budget.estimated_tokens = estimated_tokens(brief)?;
            estimate = estimated_tokens(brief)?;
            if estimate <= brief.budget.requested_tokens {
                break;
            }
        }
        if brief.changes.len() > 3 {
            remove_last(&mut brief.changes, &mut estimate)?;
            brief.budget.files_omitted += 1;
        } else if brief.commits.len() > 1 {
            remove_last(&mut brief.commits, &mut estimate)?;
            brief.budget.commits_omitted += 1;
        } else if brief.impact.len() > 1 {
            remove_last(&mut brief.impact, &mut estimate)?;
            brief.budget.impact_omitted += 1;
        } else if brief.entry_points.len() > 3 {
            remove_last(&mut brief.entry_points, &mut estimate)?;
            brief.budget.entry_points_omitted += 1;
        } else if brief.quality.len() > 2 {
            remove_last(&mut brief.quality, &mut estimate)?;
            brief.budget.quality_omitted += 1;
        } else if brief.hosted.discussions.len() > 1 {
            remove_last(&mut brief.hosted.discussions, &mut estimate)?;
            brief.budget.discussions_omitted += 1;
        } else if brief.hosted.checks.len() > 1 {
            remove_last(&mut brief.hosted.checks, &mut estimate)?;
            brief.budget.checks_omitted += 1;
        } else if brief.areas.len() > 1 {
            remove_last(&mut brief.areas, &mut estimate)?;
            brief.budget.areas_omitted += 1;
        } else if brief.changes.len() > 1 {
            remove_last(&mut brief.changes, &mut estimate)?;
            brief.budget.files_omitted += 1;
        } else if brief.entry_points.len() > 1 {
            remove_last(&mut brief.entry_points, &mut estimate)?;
            brief.budget.entry_points_omitted += 1;
        } else if shorten_retained_prose(brief) {
            estimate = estimated_tokens(brief)?;
        } else {
            break;
        }
    }
    // The estimate field and over-budget bit are themselves part of the body.
    for _ in 0..3 {
        brief.budget.estimated_tokens = estimated_tokens(brief)?;
        brief.budget.over_budget = brief.budget.estimated_tokens > brief.budget.requested_tokens;
    }
    Ok(())
}

fn shorten_text(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

fn shorten_retained_prose(brief: &mut PrContextBriefV1) -> bool {
    let mut shortened = false;
    if let Some(pr) = &mut brief.hosted.pull_request
        && let Some(text) = &mut pr.description
        && text.len() > 128
    {
        let limit = (text.len() / 2).max(128);
        pr.description_truncated |= shorten_text(text, limit);
        shortened = true;
    }
    for discussion in &mut brief.hosted.discussions {
        if let Some(text) = &mut discussion.text
            && text.len() > 128
        {
            let limit = (text.len() / 2).max(128);
            discussion.text_truncated |= shorten_text(text, limit);
            shortened = true;
        }
    }
    shortened
}

fn remove_last<T: serde::Serialize>(rows: &mut Vec<T>, estimate: &mut u32) -> Result<()> {
    if let Some(row) = rows.pop() {
        // Count each removed row once, then verify the complete body near the
        // target. Re-serializing every remaining row after each removal would
        // make a small presentation budget quadratic in a large PR's size.
        *estimate = estimate.saturating_sub(estimated_tokens(&row)?);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use crate::tool_context::{McpRequestAuthoritiesV1, McpToolBinding, PrContextDeliveryReader};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tracedecay_application::delivery::ProjectDeliveryReadOutcomeV1;

    use super::super::test_support::{
        branched_repository, fixture_context, fixture_project, test_git,
    };
    use super::*;
    use tracedecay_contracts::retrieval::{
        PrContextCheckV1, PrContextDiscussionV1, PrContextPullRequestV1,
    };

    fn graph_pending() -> TraceDecayError {
        TraceDecayError::project_route("code-graph-unavailable", true, "index is warming")
    }

    async fn briefing(root: &std::path::Path, args: Value) -> PrContextBriefV1 {
        let project = fixture_project(root);
        let ctx = fixture_context(&project);
        let completion = compute_pr_context(&ctx, async { Err(graph_pending()) }, args)
            .await
            .expect("briefing");
        match completion.result {
            GraphToolResultV1::PrContext(PrContextResultV1::Brief(brief)) => *brief,
            other => panic!("expected a brief, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn brief_forwards_exact_head_and_explicit_refresh_without_hiding_source_denial() {
        let repo = branched_repository();
        let head = git_pr_head(repo.path(), "HEAD").unwrap();
        let project = fixture_project(repo.path());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let reader: PrContextDeliveryReader = Arc::new(move |head, refresh| {
            observed.lock().unwrap().push((head.to_string(), refresh));
            Box::pin(async { ProjectDeliveryReadOutcomeV1::Denied })
        });
        let ctx = McpToolContext::bind(McpToolBinding {
            project: &project,
            request: McpRequestAuthoritiesV1 {
                pr_context_delivery: Some(&reader),
                ..Default::default()
            },
        })
        .unwrap();
        for arguments in [
            json!({"base_ref":"main"}),
            json!({"base_ref":"main", "refresh_github":true}),
        ] {
            let refresh = arguments
                .get("refresh_github")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let completion = compute_pr_context(&ctx, async { Err(graph_pending()) }, arguments)
                .await
                .unwrap();
            let GraphToolResultV1::PrContext(PrContextResultV1::Brief(brief)) = completion.result
            else {
                panic!("expected local brief")
            };
            assert_eq!(brief.hosted.source.state, PrContextSourceStateV1::Denied);
            assert_eq!(
                brief
                    .hosted
                    .source
                    .detail
                    .contains("No GitHub refresh was requested"),
                !refresh
            );
            assert_eq!(brief.hosted.discussions, Vec::new());
            assert_eq!(brief.source_analysis.state, PrContextSourceStateV1::Ready);
            assert_ne!(brief.entry_points, Vec::new());
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(head.clone(), false), (head, true)]
        );
    }

    #[tokio::test]
    async fn symbols_rejects_github_refresh_before_reading_delivery() {
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let reader: PrContextDeliveryReader =
            Arc::new(|_, _| panic!("delivery must stay uncalled"));
        let ctx = McpToolContext::bind(McpToolBinding {
            project: &project,
            request: McpRequestAuthoritiesV1 {
                pr_context_delivery: Some(&reader),
                ..Default::default()
            },
        })
        .unwrap();
        let error = compute_pr_context(
            &ctx,
            async { Err(graph_pending()) },
            json!({"view":"symbols", "refresh_github":true}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("refresh_github"));
    }

    #[tokio::test]
    async fn parked_graph_preserves_the_completed_local_brief() {
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let ctx = fixture_context(&project);
        let error = tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::Parked {
                cause: "resource limit".to_owned(),
                remedy: "reduce the indexed scope".to_owned(),
                retries_on_wake: false,
            },
        );
        assert!(matches!(
            error.project_route_context(),
            Some(("code-graph-parked", false, _))
        ));
        let completion = compute_pr_context(&ctx, async { Err(error) }, json!({"base_ref":"main"}))
            .await
            .unwrap();
        let GraphToolResultV1::PrContext(PrContextResultV1::Brief(brief)) = completion.result
        else {
            panic!("expected local brief")
        };
        assert_eq!(brief.files_changed, 1);
        assert_eq!(brief.source_analysis.state, PrContextSourceStateV1::Ready);
        assert_eq!(
            brief.graph_analysis.state,
            PrContextSourceStateV1::Unavailable
        );
        assert_ne!(brief.entry_points, Vec::new());
    }

    #[tokio::test]
    async fn default_brief_uses_exact_blobs_without_a_ready_graph() {
        let repo = branched_repository();
        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        assert_eq!(brief.view, PrContextViewV1::Brief);
        assert_eq!(brief.files_changed, 1);
        assert_eq!(brief.source_analysis.state, PrContextSourceStateV1::Ready);
        assert_eq!(
            brief.graph_analysis.state,
            PrContextSourceStateV1::Unavailable
        );
        assert_eq!(brief.entry_points.len(), 1);
        assert!(brief.entry_points[0].symbol.ends_with("after"));
        assert_eq!(brief.entry_points[0].change, "added");
        assert_eq!(brief.entry_points[0].revision, brief.head_oid);
        assert_eq!(brief.entry_points[0].location.line, 2);
        assert!(brief.entry_points[0].node_id.is_none());
        assert!(!brief.hosted.required_checks_known);
        assert!(brief.hosted.pull_request.is_none());
        assert_eq!(
            brief.budget.estimated_tokens,
            estimated_tokens(&brief).unwrap()
        );
    }

    #[tokio::test]
    async fn signatures_deletions_and_same_line_neighbors_are_compared_independently() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        let unchanged = (0..40).fold(String::new(), |mut unchanged, index| {
            use std::fmt::Write as _;
            writeln!(unchanged, "fn unchanged_{index}() {{}}").unwrap();
            unchanged
        });
        std::fs::write(repo.path().join("lib.rs"), format!("{unchanged}pub fn changed(x: u32) -> u32 {{ x }} fn neighbor() {{}}\npub fn removed() {{}}\n")).unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "base declarations"]);
        test_git(repo.path(), &["switch", "-c", "changed-declarations"]);
        std::fs::write(
            repo.path().join("lib.rs"),
            format!("{unchanged}pub fn changed(x: u64) -> u64 {{ x + 1 }} fn neighbor() {{}}\n"),
        )
        .unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "change contract"]);
        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        assert_eq!(brief.entry_points.len(), 2);
        let changed = brief
            .entry_points
            .iter()
            .find(|entry| entry.change == "signature_changed")
            .unwrap();
        assert!(changed.before_signature.as_deref().unwrap().contains("u32"));
        assert!(changed.after_signature.as_deref().unwrap().contains("u64"));
        assert_eq!(changed.location.line, 41);
        assert!(
            changed
                .before_excerpt
                .as_ref()
                .unwrap()
                .text
                .contains("x: u32")
        );
        assert!(
            changed
                .after_excerpt
                .as_ref()
                .unwrap()
                .text
                .contains("x + 1")
        );
        assert!(
            !changed
                .after_excerpt
                .as_ref()
                .unwrap()
                .text
                .contains("neighbor")
        );
        let removed = brief
            .entry_points
            .iter()
            .find(|entry| entry.change == "removed")
            .unwrap();
        assert_eq!(removed.revision, brief.merge_base);
        assert!(removed.after_signature.is_none());
        assert!(
            !brief
                .entry_points
                .iter()
                .any(|entry| entry.symbol.contains("unchanged")
                    || entry.symbol.ends_with("neighbor"))
        );
    }

    #[tokio::test]
    async fn export_loss_and_module_wiring_survive_an_unrelated_body_change() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        std::fs::write(repo.path().join("api.ts"),
            "import { dependency } from './old';\nexport function api() { return 1; }\nfunction other() { return 2; }\n").unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "base exported API"]);
        test_git(repo.path(), &["switch", "-c", "visibility-change"]);
        std::fs::write(repo.path().join("api.ts"),
            "import { dependency } from './new';\nfunction api() { return 1; }\nfunction other() { return 3; }\n").unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(
            repo.path(),
            &["commit", "-m", "remove export and rewire import"],
        );

        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        let export = brief
            .entry_points
            .iter()
            .find(|entry| entry.symbol.ends_with("api") && entry.change == "visibility_changed")
            .expect("export removal");
        assert_eq!(export.location.line, 2);
        assert_eq!(export.revision, brief.head_oid);
        assert!(
            export.reason.contains("public to private"),
            "{}",
            export.reason
        );
        assert!(
            brief
                .entry_points
                .iter()
                .any(|entry| entry.symbol.ends_with("other") && entry.change == "modified")
        );
        let wiring = brief
            .entry_points
            .iter()
            .find(|entry| entry.change == "file_change")
            .expect("module wiring survives a declaration change in the same file");
        assert_eq!(wiring.location.line, 1);
        assert_eq!(wiring.revision, brief.head_oid);
        assert!(wiring.reason.contains("imports and module wiring"));
        assert!(
            brief
                .quality
                .iter()
                .any(|signal| signal.signal == "declaration_contracts"
                    && signal.observation.starts_with("1 "))
        );
    }

    #[tokio::test]
    async fn shared_line_wiring_is_retained_without_repeating_pure_body_edits() {
        for (before, after, file_entry, body_entry) in [
            (
                "import './old'; function f() { return 1; }\n",
                "import './new'; function f() { return 1; }\n",
                true,
                false,
            ),
            (
                "import './old'; function f() { return 1; }\n",
                "import './new'; function f() { return 2; }\n",
                true,
                true,
            ),
            (
                "function f() { return 1; }\n",
                "function f() { return 2; }\n",
                false,
                true,
            ),
            (
                "import './old'; function f() { return 1; }\n",
                "import './old'; function f() { return 2; }\n",
                false,
                true,
            ),
            (
                "export function f() { return 1; }\n",
                "export function f() { return 2; }\n",
                false,
                true,
            ),
        ] {
            let repo = branched_repository();
            test_git(repo.path(), &["switch", "main"]);
            std::fs::write(repo.path().join("shared.ts"), before).unwrap();
            test_git(repo.path(), &["add", "."]);
            test_git(repo.path(), &["commit", "-m", "base shared line"]);
            test_git(repo.path(), &["switch", "-c", "shared-line"]);
            std::fs::write(repo.path().join("shared.ts"), after).unwrap();
            test_git(repo.path(), &["add", "."]);
            test_git(repo.path(), &["commit", "-m", "edit shared line"]);

            let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
            assert_eq!(brief.source_analysis.state, PrContextSourceStateV1::Ready);
            assert_eq!(
                brief
                    .entry_points
                    .iter()
                    .any(|entry| entry.change == "file_change"),
                file_entry,
                "{before:?} -> {after:?}: {:?}",
                brief.entry_points
            );
            assert_eq!(
                brief
                    .entry_points
                    .iter()
                    .any(|entry| entry.symbol.ends_with('f') && entry.change == "modified"),
                body_entry,
                "{before:?} -> {after:?}: {:?}",
                brief.entry_points
            );
            assert_ne!(brief.entry_points, Vec::new());
            assert!(
                brief
                    .entry_points
                    .iter()
                    .all(|entry| entry.location.line == 1 && entry.revision == brief.head_oid)
            );
            if file_entry {
                assert!(brief.entry_points.iter().any(|entry| {
                    entry
                        .reason
                        .contains("does not isolate which portion changed")
                }));
            }
        }
    }

    #[tokio::test]
    async fn aggregate_patch_larger_than_transport_limit_still_produces_a_brief() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        // Each blob is below the historical-blob bound, while the complete
        // before/after patch is over the 4 MiB command-output allowance.
        let before = format!("{}\n", "a".repeat(1_024)).repeat(2_400);
        let after = format!("{}\n", "b".repeat(1_024)).repeat(2_400);
        assert!(before.len() + after.len() > 4 * 1024 * 1024);
        std::fs::write(repo.path().join("large.txt"), before).unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "base large text"]);
        test_git(repo.path(), &["switch", "-c", "large-comparison"]);
        std::fs::write(repo.path().join("large.txt"), after).unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "replace large text"]);

        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        assert_eq!(brief.files_changed, 1);
        assert_eq!(brief.additions, 2_400);
        assert_eq!(brief.deletions, 2_400);
        assert_eq!(brief.changes[0].additions, Some(2_400));
        assert_eq!(brief.changes[0].deletions, Some(2_400));
        assert_eq!(brief.source_analysis.state, PrContextSourceStateV1::Partial);
        assert_eq!(brief.entry_points[0].location.file, "large.txt");
        assert_eq!(brief.entry_points[0].revision, brief.head_oid);
        assert!(!brief.budget.over_budget);
    }

    #[tokio::test]
    async fn deleted_trailing_text_keeps_a_real_before_side_entrypoint() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        std::fs::write(
            repo.path().join("notes with  spaces.txt"),
            "retained\ndeleted\n",
        )
        .unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "base unsupported text"]);
        test_git(repo.path(), &["switch", "-c", "delete-trailing-text"]);
        std::fs::write(repo.path().join("notes with  spaces.txt"), "retained\n").unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "remove trailing text"]);

        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        assert_eq!(brief.entry_points.len(), 1);
        let entry = &brief.entry_points[0];
        assert_eq!(entry.location.file, "notes with  spaces.txt");
        assert_eq!(entry.location.line, 2);
        assert_eq!(entry.revision, brief.merge_base);
        assert_eq!(brief.additions, 0);
        assert_eq!(brief.deletions, 1);
        let rendered = super::super::pr_context_render::render_pr_context_brief(&brief);
        let read = rendered
            .lines()
            .find(|line| line.contains("tracedecay_source_lines"))
            .expect("historical read hint");
        let json_start = read.find('{').unwrap();
        let json_end = read.rfind('}').unwrap();
        let selector: Value = serde_json::from_str(&read[json_start..=json_end]).unwrap();
        assert_eq!(selector["revision"], brief.merge_base);
        assert_eq!(selector["file"], "notes with  spaces.txt");
        assert_eq!(selector["start_line"], 2);
        assert_eq!(selector["end_line"], 2);
    }

    #[tokio::test]
    async fn initializer_signatures_are_bounded_without_erasing_the_change_map() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("api.ts"),
            format!("export const payload = '{}';\n", "変更前".repeat(4_000)),
        )
        .unwrap();
        for index in 0..30 {
            std::fs::write(
                repo.path().join(format!("src/file_{index:02}.rs")),
                format!("pub fn entry_{index}() -> u32 {{ 1 }}\n"),
            )
            .unwrap();
        }
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "base initializers"]);
        test_git(repo.path(), &["switch", "-c", "bounded-signatures"]);
        std::fs::write(
            repo.path().join("api.ts"),
            format!("export const payload = '{}';\n", "変更後".repeat(4_000)),
        )
        .unwrap();
        for index in 0..30 {
            std::fs::write(
                repo.path().join(format!("src/file_{index:02}.rs")),
                format!("pub fn entry_{index}() -> u32 {{ 2 }}\n"),
            )
            .unwrap();
        }
        test_git(repo.path(), &["add", "."]);
        test_git(
            repo.path(),
            &["commit", "-m", "change initializers and callers"],
        );

        let brief = briefing(
            repo.path(),
            json!({"base_ref":"main", "budget_tokens":4_000}),
        )
        .await;
        let entry = brief
            .entry_points
            .iter()
            .find(|entry| entry.symbol.ends_with("payload"))
            .expect("highest-priority contract remains in the briefing");
        assert_eq!(entry.change, "signature_changed");
        assert!(entry.before_signature_truncated);
        assert!(entry.after_signature_truncated);
        assert!(entry.before_signature.as_ref().unwrap().len() <= 600);
        assert!(entry.after_signature.as_ref().unwrap().len() <= 600);
        assert_eq!(entry.revision, brief.head_oid);
        assert!(!brief.budget.over_budget);
        assert!(estimated_tokens(&brief).unwrap() <= 4_000);
        assert!(!brief.changes.is_empty(), "retain a usable file map");
        assert!(!brief.areas.is_empty(), "retain a usable area map");
        assert_eq!(brief.changes[0].file, "api.ts");
        assert_eq!(brief.changes.len() + brief.budget.files_omitted, 31);
        assert_eq!(brief.files_changed, 31);
    }

    #[tokio::test]
    async fn detached_head_and_advanced_base_keep_the_merge_base_comparison() {
        let repo = branched_repository();
        test_git(repo.path(), &["switch", "main"]);
        std::fs::write(repo.path().join("base_only.rs"), "pub fn unrelated() {}\n").unwrap();
        test_git(repo.path(), &["add", "."]);
        test_git(repo.path(), &["commit", "-m", "advance base"]);
        test_git(repo.path(), &["switch", "--detach", "feature"]);
        std::fs::write(repo.path().join("lib.rs"), "fn uncommitted_work() {}\n").unwrap();
        let brief = briefing(repo.path(), json!({"base_ref":"main"})).await;
        assert_ne!(brief.base_oid, brief.merge_base);
        assert_eq!(brief.files_changed, 1);
        assert_eq!(brief.changes[0].file, "lib.rs");
        assert!(
            brief
                .entry_points
                .iter()
                .any(|entry| entry.symbol.ends_with("after"))
        );
        assert!(!brief.entry_points.iter().any(
            |entry| entry.symbol.contains("uncommitted") || entry.symbol.contains("unrelated")
        ));
        assert!(
            brief
                .quality
                .iter()
                .any(|signal| signal.signal == "checkout_differs")
        );
    }

    #[tokio::test]
    async fn budget_covers_hosted_evidence_and_keeps_the_top_concern_and_entrypoint() {
        let repo = branched_repository();
        let mut brief = briefing(
            repo.path(),
            json!({"base_ref":"main", "budget_tokens":10000}),
        )
        .await;
        brief.hosted.pull_request = Some(PrContextPullRequestV1 {
            number: 42,
            title: "Explain this change".to_owned(),
            state: "open".to_owned(),
            draft: false,
            description: Some("変更の理由と互換性について。".repeat(4_000)),
            description_truncated: false,
            observed_head: brief.head_oid.clone(),
            observed_at: 1,
            matches_head: true,
        });
        for index in 0..30 {
            brief.hosted.discussions.push(PrContextDiscussionV1 {
                id: format!("comment-{index}"),
                thread_id: None,
                kind: "review".to_owned(),
                lifecycle: "current".to_owned(),
                review_state: "changes_requested".to_owned(),
                author: None,
                event_at: None,
                file: Some("lib.rs".to_owned()),
                line: Some(2),
                text: Some("This contract needs a test. ".repeat(12)),
                text_truncated: false,
                url: None,
                body_anchor: Some(format!("body-{index}")),
                observed_at: index,
                matches_head: true,
            });
            brief.hosted.checks.push(PrContextCheckV1 {
                name: format!("check-{index}"),
                workflow: "test".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("failure".to_owned()),
                failed_step: Some("unit tests".to_owned()),
                head: brief.head_oid.clone(),
                matches_head: true,
                observed_at: index,
                evidence_anchor: format!("failure-{index}"),
                locations: Vec::new(),
            });
        }
        brief.hosted.discussions_total = 30;
        brief.hosted.checks_total = 30;
        brief.hosted.discussions[0].text = Some("確認してください。".repeat(8_000));
        brief.budget.requested_tokens = 2_000;
        fit_budget(&mut brief).unwrap();
        assert!(!brief.budget.over_budget);
        assert!(estimated_tokens(&brief).unwrap() <= 2_000);
        assert_eq!(
            brief.hosted.discussions.len() + brief.budget.discussions_omitted,
            30
        );
        assert_eq!(brief.hosted.checks.len() + brief.budget.checks_omitted, 30);
        assert_eq!(brief.hosted.discussions[0].id, "comment-0");
        assert!(
            brief
                .hosted
                .pull_request
                .as_ref()
                .unwrap()
                .description_truncated
        );
        assert!(brief.hosted.discussions[0].text_truncated);
        assert_eq!(
            brief.hosted.discussions[0].body_anchor.as_deref(),
            Some("body-0")
        );
        assert!(
            brief
                .hosted
                .pull_request
                .as_ref()
                .unwrap()
                .description
                .as_ref()
                .unwrap()
                .len()
                <= 2_000
        );
        assert!(brief.hosted.discussions[0].text.as_ref().unwrap().len() <= 2_000);
        assert_eq!(brief.hosted.checks[0].name, "check-0");
        assert_eq!(brief.entry_points.len(), 1);
        assert_eq!(
            brief.budget.estimated_tokens,
            estimated_tokens(&brief).unwrap()
        );
        brief.budget.requested_tokens = 1;
        fit_budget(&mut brief).unwrap();
        assert!(
            brief.budget.over_budget,
            "a minimum evidence packet must not silently claim it fit"
        );
    }

    #[tokio::test]
    async fn brief_rejects_symbol_cursors_and_propagates_terminal_graph_failures() {
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let ctx = fixture_context(&project);
        for graph_error in [
            tracedecay_graph_query::CodeGraphReadError::Denied,
            tracedecay_graph_query::CodeGraphReadError::Cancelled,
            tracedecay_graph_query::CodeGraphReadError::Corrupt {
                detail: "invalid retained graph".to_owned(),
            },
        ] {
            let error = tracedecay_graph_query::map_code_graph_read_runtime_error(graph_error);
            assert!(
                compute_pr_context(&ctx, async { Err(error) }, json!({"base_ref":"main"}))
                    .await
                    .is_err()
            );
        }
        let error = compute_pr_context(
            &ctx,
            async { Err(graph_pending()) },
            json!({"base_ref":"main", "cursor":"cursor"}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("view: symbols"));
        let error = compute_pr_context(
            &ctx,
            async {
                Err(TraceDecayError::Config {
                    message: "denied graph".to_owned(),
                })
            },
            json!({"base_ref":"main"}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("denied graph"));
    }
}
