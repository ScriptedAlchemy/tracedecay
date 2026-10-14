//! Owned task settlement and exact-source cursor helpers for code-index search.

use tracedecay_query::code_search;
use tracedecay_runtime_core::native_runtime;

use crate::code_index_scheduler;

pub fn code_index_search_unavailable_for_generation(
    code_generation: Option<String>,
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
    lane_reason: &'static str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    code_search::CodeIndexSearchOutcomeV1::Unavailable(code_search::CodeIndexSearchUnavailableV1 {
        code_generation,
        reason,
        coverage: code_search::CodeIndexSearchCoverageV1::unavailable(lane_reason),
    })
}

pub fn code_index_search_unavailable(
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
    lane_reason: &'static str,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    code_index_search_unavailable_for_generation(None, reason, lane_reason)
}

pub fn code_index_scope_unavailable(
    error: crate::mcp_admission::CodeIndexScopeUnavailableV1,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    let reason = error.search_reason();
    let lane_reason = match error {
        crate::mcp_admission::CodeIndexScopeUnavailableV1::AuthorityUnavailable => {
            "scope_unavailable"
        }
        crate::mcp_admission::CodeIndexScopeUnavailableV1::NotApplicable => reason.as_str(),
    };
    code_index_search_unavailable(reason, lane_reason)
}

pub async fn generation_for_hydration(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_contracts::ResolvedScope,
    generation_id: &tracedecay_domain::CodeGenerationId,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
) -> Result<
    code_index_scheduler::LatestCompleteCodeIndexV1,
    Box<tracedecay_query::code_search::CodeIndexSearchOutcomeV1>,
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
        Ok(None) => Err(Box::new(code_index_search_unavailable_for_generation(
            Some(generation_id.as_str().to_owned()),
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
            "generation_changed_before_hydration",
        ))),
        Err(reason) => Err(Box::new(code_index_search_unavailable_for_generation(
            Some(generation_id.as_str().to_owned()),
            reason,
            reason.as_str(),
        ))),
    }
}

/// Awaits `task` to completion, checking `termination` every `poll_interval`
/// while it runs. A terminal outcome is answered only once the task settles,
/// so the work never outlives the request that owns what it holds.
pub async fn settle_owned_blocking_task<T, O>(
    task: impl Future<Output = Result<T, String>>,
    poll_interval: std::time::Duration,
    mut termination: impl FnMut() -> Option<O>,
) -> Result<Result<T, String>, O> {
    let mut task = std::pin::pin!(task);
    loop {
        if let Some(outcome) = termination() {
            let _ = task.await;
            return Err(outcome);
        }
        match native_runtime::timeout(poll_interval, task.as_mut()).await {
            Ok(result) => return Ok(result),
            Err(native_runtime::TimeoutError::Elapsed) => {}
            Err(native_runtime::TimeoutError::TimerUnavailable(error)) => {
                tracing::warn!(%error, "owned task settles without termination polls");
                let result = task.await;
                return termination().map_or(Ok(result), Err);
            }
        }
    }
}

/// Starts `work` on the native blocking pool now, for work the caller does not
/// await right away or at all: [`native_runtime::spawn_blocking`] starts only
/// at its first poll. Dropping the task detaches the work. `None`, with `work`
/// dropped, when the native runtime is unavailable.
pub(crate) fn start_blocking(
    work: impl FnOnce() + Send + 'static,
) -> Option<native_runtime::NativeTask<Result<(), String>>> {
    native_runtime::NativeTask::spawn(native_runtime::spawn_blocking(work))
        .inspect_err(|error| tracing::warn!(%error, "native blocking work did not start"))
        .ok()
}

/// Starts `work` on the native runtime and detaches it. For work that never
/// reaches Tokio. A refused start is logged, and `work` is dropped.
pub(crate) fn spawn_detached(event: &'static str, work: impl Future<Output = ()> + Send + 'static) {
    if let Err(error) = native_runtime::NativeTask::spawn(work) {
        tracing::warn!(event, %error, "native task did not start");
    }
}

/// Awaits a task that [`native_runtime::NativeTask::spawn`] started, or
/// reports why it did not start, so the caller has one failure path.
pub(crate) async fn join_native<T: Clone + Send + Sync + 'static>(
    task: &Result<native_runtime::NativeTask<T>, String>,
) -> Result<T, String> {
    match task.as_ref().map_err(Clone::clone)?.wait().await {
        native_runtime::NativeTaskOutcome::Completed(value) => Ok(value),
        native_runtime::NativeTaskOutcome::Cancelled => Err("native task was cancelled".to_owned()),
        native_runtime::NativeTaskOutcome::Panicked(payload) => {
            Err(format!("native task panicked: {payload}"))
        }
    }
}

pub fn exact_source_is_complete(
    reference: Option<&tracedecay_domain::RefId>,
    commit: Option<&tracedecay_domain::GitOidV1>,
    tree: Option<&tracedecay_domain::GitOidV1>,
) -> bool {
    matches!(
        (reference, commit, tree),
        (None, None, None) | (Some(_), Some(_), Some(_))
    )
}

pub async fn verify_exact_source_cursor(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_contracts::ResolvedScope,
    cursor: Option<&tracedecay_domain::RetrievalCursor>,
    source: &tracedecay_domain::CodeSourceCursorBindingV1,
) -> Result<(), code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    let authority = schedulers.query_authority_for_scope(scope).await.ok_or(
        code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::AuthorityUnavailable,
    )?;
    authority
        .verify_code_source_cursor(cursor, source)
        .map_err(|_| {
            code_index_scheduler::query_runtime::QuerySearchExecutionErrorV1::ExactCursorInvalid
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactCursorPublicationErrorV1 {
    AuthorityUnavailable,
    BindingFailed,
}

pub async fn bind_exact_source_cursor(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_contracts::ResolvedScope,
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
        let worker = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
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
