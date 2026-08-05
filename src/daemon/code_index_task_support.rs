//! Owned task settlement and exact-source cursor helpers for code-index search.

use super::code_index_scheduler;

pub(super) fn code_index_search_unavailable_for_generation(
    code_generation: Option<String>,
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
    semantic_reason: &'static str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    use tracedecay_query::code_search;

    code_search::CodeIndexSearchOutcomeV1::Unavailable(code_search::CodeIndexSearchUnavailableV1 {
        code_generation,
        reason,
        semantic: code_search::CodeIndexSemanticStatusV1::Unavailable {
            reason: semantic_reason,
        },
        coverage: code_search::CodeIndexSearchCoverageV1::unavailable(semantic_reason),
    })
}

pub(super) fn code_index_search_unavailable(
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
    semantic_reason: &'static str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    code_index_search_unavailable_for_generation(None, reason, semantic_reason)
}

pub(super) fn code_index_scope_unavailable()
-> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    code_index_search_unavailable(
        tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
        "scope_unavailable",
    )
}

pub(super) fn code_index_search_hydration_budget(
    accepted_semantic_budget: Option<&tracedecay_domain::RetrievalBudget>,
    query_budget: &tracedecay_domain::RetrievalBudget,
) -> tracedecay_domain::RetrievalBudget {
    accepted_semantic_budget.copied().unwrap_or(*query_budget)
}

pub(super) async fn generation_for_hydration(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_application::ResolvedScope,
    generation_id: &tracedecay_domain::CodeGenerationId,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<
    code_index_scheduler::LatestCompleteCodeIndexV1,
    tracedecay_query::code_search::CodeIndexSearchOutcomeV1,
> {
    let generation = schedulers
        .generation_for_controlled(
            scope,
            generation_id,
            Some(
                code_index_scheduler::branch_generations::BranchGenerationReadControlV1 {
                    deadline,
                    cancellation,
                },
            ),
        )
        .await;
    match generation {
        Ok(Some(generation)) => Ok(generation),
        Ok(None) => Err(code_index_search_unavailable_for_generation(
            Some(generation_id.as_str().to_owned()),
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
            "generation_changed_before_hydration",
        )),
        Err(reason) => {
            let semantic_reason = reason.as_str();
            Err(code_index_search_unavailable_for_generation(
                Some(generation_id.as_str().to_owned()),
                reason,
                semantic_reason,
            ))
        }
    }
}

pub(crate) async fn settle_owned_blocking_task<T, O>(
    mut task: tokio::task::JoinHandle<T>,
    poll_interval: std::time::Duration,
    mut termination: impl FnMut() -> Option<O>,
) -> Result<Result<T, tokio::task::JoinError>, O> {
    let mut terminal = None;
    let mut control_poll = tokio::time::interval(poll_interval);
    control_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut task => {
                return match terminal {
                    Some(outcome) => Err(outcome),
                    None => Ok(result),
                };
            }
            _ = control_poll.tick(), if terminal.is_none() => {
                terminal = termination();
            }
        }
    }
}

pub(super) fn exact_source_is_complete(
    reference: Option<&tracedecay_domain::RefId>,
    commit: Option<&tracedecay_domain::GitOidV1>,
    tree: Option<&tracedecay_domain::GitOidV1>,
) -> bool {
    matches!(
        (reference, commit, tree),
        (None, None, None) | (Some(_), Some(_), Some(_))
    )
}

pub(super) async fn verify_exact_source_cursor(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_application::ResolvedScope,
    cursor: Option<&tracedecay_domain::RetrievalCursor>,
    source: &tracedecay_domain::CodeSourceCursorBindingV1,
) -> Result<(), code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    let authority = schedulers.query_authority_for_scope(scope).await.ok_or(
        code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query(
            code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::AuthorityUnavailable,
        ),
    )?;
    authority
        .verify_code_source_cursor(cursor, source)
        .map_err(|_| {
            code_index_scheduler::semantic_query_runtime::QuerySemanticSearchExecutionErrorV1::Query(
                code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::ExactCursorInvalid,
            )
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExactCursorPublicationErrorV1 {
    AuthorityUnavailable,
    BindingFailed,
}

pub(super) async fn bind_exact_source_cursor(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_application::ResolvedScope,
    cursor: Option<&mut tracedecay_domain::RetrievalCursor>,
    source: Option<tracedecay_domain::CodeSourceCursorBindingV1>,
) -> Result<(), ExactCursorPublicationErrorV1> {
    let (Some(cursor), Some(source)) = (cursor, source) else {
        return Ok(());
    };
    let authority = schedulers
        .query_authority_for_scope(scope)
        .await
        .ok_or(ExactCursorPublicationErrorV1::AuthorityUnavailable)?;
    authority
        .bind_code_source_cursor(cursor, source)
        .map_err(|_| ExactCursorPublicationErrorV1::BindingFailed)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::settle_owned_blocking_task;

    #[tokio::test]
    async fn terminal_search_owns_blocking_worker_until_it_settles() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&admission)
            .try_acquire_owned()
            .expect("search permit");
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            blocked.recv().expect("worker release");
            7_u8
        });
        let settlement =
            settle_owned_blocking_task(worker, std::time::Duration::from_millis(1), || {
                Some("cancelled")
            });
        tokio::pin!(settlement);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut settlement)
                .await
                .is_err(),
            "terminal observation must not detach the still-running blocking worker"
        );
        assert!(
            admission.try_acquire().is_err(),
            "the execution permit remains owned until the blocking worker settles"
        );
        release.send(()).expect("release worker");
        assert!(matches!(settlement.await, Err("cancelled")));
        assert!(
            admission.try_acquire().is_ok(),
            "settlement releases the execution permit"
        );
    }
}
