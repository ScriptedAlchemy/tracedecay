#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    AuthorizedScopeSet, CancellationContext, Deadline, MultiRootContinuationStateV1,
    MultiRootContinuationV1, MultiRootExecuteRequestV1, MultiRootOperationV1, MultiRootQueryPageV1,
};
use tracedecay_domain::{CodeGenerationId, RootGenerationV1, RootScopeOutcomeV1, ScopeOutcome};
use tracedecay_domain::{ScopeUnavailableReasonV1, UtcMicros};
use tracedecay_temporal_query::ports::SessionCursorAuthenticator;

use super::super::{DaemonEngine, DaemonHandshake, execute_daemon_invocation};
use crate::daemon::service::invocation::{DaemonInvocationOutcome, DaemonInvocationRequest};

pub(super) async fn publish_generation_after(
    engine: &DaemonEngine,
    root: &Path,
    retained: &CodeGenerationId,
) {
    std::fs::write(root.join("lib.rs"), "pub fn value() -> u8 { 2 }\n")
        .expect("change retained root");
    assert!(
        engine
            .invocation
            .code_index_schedulers
            .notify_hook_paths(root, &["lib.rs".to_owned()])
            .await
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = engine
                .invocation
                .code_index_schedulers
                .latest_generation_id(root)
                .await;
            if current.as_ref().is_some_and(|current| current != retained) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publish generation two");
}

pub(super) async fn seal_missing_generation(
    database: &crate::global_db::RegisteredGlobalDb,
    state: &MultiRootContinuationStateV1,
    authenticator: &impl SessionCursorAuthenticator,
) -> MultiRootContinuationV1 {
    let key = database
        .ensure_active_session_cursor_key_result()
        .await
        .expect("active cursor key");
    let mut state = state.clone();
    replace_with_missing_generation(&mut state, 0, "generation.multi-root.retired");
    super::super::multi_root_continuation::seal(&state, &key, authenticator)
        .expect("authenticated unavailable continuation")
}

pub(super) async fn assert_unavailable_recovery_resumes_cursor(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    database: &crate::global_db::RegisteredGlobalDb,
    scope_set: &AuthorizedScopeSet,
    operation: &MultiRootOperationV1,
    page_one: &MultiRootQueryPageV1<serde_json::Value>,
    authenticator: &impl SessionCursorAuthenticator,
    expected: &MultiRootContinuationStateV1,
) {
    let unavailable = seal_missing_generation(database, expected, authenticator).await;
    let unavailable_at = now();
    let unavailable_response = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.unavailable-generation",
            MultiRootExecuteRequestV1::new(
                scope_set.scope_set_id().clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                operation.clone(),
                expected.next_page,
                Some(unavailable),
            )
            .expect("unavailable generation request"),
            unavailable_at,
            deadline(unavailable_at),
            CancellationContext::active("cancel.multi-root.unavailable-generation")
                .expect("cancellation"),
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
        ..
    } = unavailable_response.outcome
    else {
        panic!("missing retained generation must remain a typed result");
    };
    let page = packet.payload.expect("typed unavailable page");
    assert!(matches!(
        &page.roots[0].outcome,
        ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::AuthorityUnavailable
        }
    ));
    let continuation = page
        .continuation
        .as_ref()
        .expect("retryable unavailable root must retain a continuation");
    let resumed =
        super::super::multi_root_continuation::open(continuation, authenticator, unavailable_at)
            .expect("authenticated unavailable continuation");
    assert_eq!(resumed.root_cursors[0], expected.root_cursors[0]);

    let mut recovered = resumed;
    recovered.root_generations[0] = expected.root_generations[0].clone();
    let recovery_page = recovered.next_page;
    let key = database
        .ensure_active_session_cursor_key_result()
        .await
        .expect("active cursor key");
    let recovered = super::super::multi_root_continuation::seal(&recovered, &key, authenticator)
        .expect("authenticated recovered continuation");
    let recovered_at = now();
    let recovered_response = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.recovered-generation",
            MultiRootExecuteRequestV1::new(
                scope_set.scope_set_id().clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                operation.clone(),
                recovery_page,
                Some(recovered),
            )
            .expect("recovered generation request"),
            recovered_at,
            deadline(recovered_at),
            CancellationContext::active("cancel.multi-root.recovered-generation")
                .expect("cancellation"),
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
        ..
    } = recovered_response.outcome
    else {
        panic!("recovered retained generation must resume");
    };
    let recovered_page = packet.payload.expect("recovered page");
    assert!(matches!(
        &recovered_page.roots[0].outcome,
        ScopeOutcome::Exact(_)
    ));
    assert_ne!(&recovered_page.roots[0].outcome, &page_one.roots[0].outcome);

    let recovered_continuation = recovered_page
        .continuation
        .as_ref()
        .expect("one root still has a third Work item");
    let completed = super::super::multi_root_continuation::open(
        recovered_continuation,
        authenticator,
        recovered_at,
    )
    .expect("authenticated recovery continuation");
    assert!(matches!(
        completed.root_cursors[1].cursor.as_ref(),
        Some(tracedecay_application::MultiRootRootContinuationV1::Complete)
    ));
    let terminal_page = completed.next_page;
    let terminal = super::super::multi_root_continuation::seal(&completed, &key, authenticator)
        .expect("authenticated terminal-root continuation");
    let terminal_at = now();
    let terminal_response = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.terminal-root",
            MultiRootExecuteRequestV1::new(
                scope_set.scope_set_id().clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                operation.clone(),
                terminal_page,
                Some(terminal),
            )
            .expect("terminal-root request"),
            terminal_at,
            deadline(terminal_at),
            CancellationContext::active("cancel.multi-root.terminal-root").expect("cancellation"),
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
        ..
    } = terminal_response.outcome
    else {
        panic!("completed root must remain an exact terminal result");
    };
    let terminal_page = packet.payload.expect("terminal-root page");
    assert!(matches!(
        &terminal_page.roots[1].outcome,
        ScopeOutcome::Exact(values) if values.is_empty()
    ));
    let terminal_continuation = terminal_page
        .continuation
        .as_ref()
        .expect("other root must keep terminal state observable");
    let mut pruned = super::super::multi_root_continuation::open(
        terminal_continuation,
        authenticator,
        terminal_at,
    )
    .expect("authenticated terminal continuation");
    assert!(matches!(
        pruned.root_cursors[1].cursor.as_ref(),
        Some(tracedecay_application::MultiRootRootContinuationV1::Complete)
    ));

    replace_with_missing_generation(&mut pruned, 1, "generation.multi-root.pruned");
    let completed_page = pruned.next_page;
    let completed = super::super::multi_root_continuation::seal(&pruned, &key, authenticator)
        .expect("authenticated completed-root continuation");
    let completed_at = now();
    let completed_response = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::multi_root_execute(
            "request.multi-root.completed-pruned-generation",
            MultiRootExecuteRequestV1::new(
                scope_set.scope_set_id().clone(),
                scope_set.revision(),
                scope_set.digest().clone(),
                operation.clone(),
                completed_page,
                Some(completed),
            )
            .expect("completed pruned-generation request"),
            completed_at,
            deadline(completed_at),
            CancellationContext::active("cancel.multi-root.completed-pruned-generation")
                .expect("cancellation"),
        ),
    )
    .await;
    let DaemonInvocationOutcome::MultiRootQueryPage {
        outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
        ..
    } = completed_response.outcome
    else {
        panic!("completed root must not require its pruned generation");
    };
    let completed_page = packet.payload.expect("completed-root page");
    assert!(matches!(
        &completed_page.roots[1].outcome,
        ScopeOutcome::Exact(values) if values.is_empty()
    ));
}

fn replace_with_missing_generation(
    state: &mut MultiRootContinuationStateV1,
    ordinal: usize,
    generation_id: &str,
) {
    let ScopeOutcome::Exact(retained) = &state.root_generations[ordinal].outcome else {
        unreachable!("retained generation was exact");
    };
    let scope_digest = retained.scope_digest.clone();
    let missing = RootGenerationV1::new(
        scope_digest.clone(),
        CodeGenerationId::new(generation_id).expect("generation id"),
        retained.snapshot_digest.clone(),
        retained.graph_publication_digest.clone(),
    )
    .expect("missing retained generation");
    state.root_generations[ordinal] =
        RootScopeOutcomeV1::new(scope_digest, ScopeOutcome::Exact(missing))
            .expect("root generation outcome");
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn deadline(observed_at: UtcMicros) -> Deadline {
    Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline")
}
