use std::fs;
use std::sync::atomic::AtomicUsize;

use tempfile::tempdir;
use tracedecay_contracts::{
    ApplicationOperation, CancellationSignal, CancellationStage, Deadline, EffectTermination,
    IdempotencyKey, RequestContext, SourceEditAuthorizationAdmissionV1,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditKind, SourceEditRequest,
    SourceEditRollbackRequestV1, source_edit_operation, source_edit_rollback_operation,
};
use tracedecay_domain::UtcMicros;

use super::test_support::{
    CancelBeforeEffectAuthorization, FixtureSourceEditAuthorization, fixture_authorization,
    fixture_graph, fixture_request, fixture_request_for_edit, fixture_symbol_code_graph, git,
};
use crate::{
    SourceEditApplicationResult, SourceEditEffectControlV1, execute_source_edit,
    execute_source_edit_rollback, execute_source_edit_with_control,
    preview_source_edit_expected_state,
};

#[tokio::test]
async fn preview_apply_replay_and_expected_state_cas_preserve_exact_bytes() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    let initial = b"old\r\nunchanged \xE2\x98\x83\n";
    let applied = b"new\r\nunchanged \xE2\x98\x83\n";
    fs::write(project.path().join("src/lib.rs"), initial).unwrap();
    git(
        project.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(project.path(), &["add", "src/lib.rs"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    let (graph, code_graph) = fixture_graph(project.path()).await;
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
    let request = fixture_request();
    let authorization = fixture_authorization(&request);

    let mut preview_request = request.clone();
    preview_request.edit = preview_request.edit.clone().with_dry_run(true);
    let preview = execute_source_edit(
        &graph,
        &code_graph,
        &operation,
        preview_request,
        &authorization,
    )
    .await
    .unwrap();
    assert!(preview.dry_run);
    assert!(preview.outcome.success());
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        initial
    );

    let mut apply_request = request;
    apply_request.idempotency_key = IdempotencyKey::new("source-edit.apply-fixture").unwrap();
    apply_request.expected_state = preview.expected_state.clone();
    let applied_result = execute_source_edit(
        &graph,
        &code_graph,
        &operation,
        apply_request.clone(),
        &authorization,
    )
    .await
    .unwrap();
    assert!(applied_result.outcome.success());
    assert!(!applied_result.replayed);
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        applied
    );
    let replay = execute_source_edit(
        &graph,
        &code_graph,
        &operation,
        apply_request,
        &authorization,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        applied
    );
    fs::write(project.path().join("src/lib.rs"), initial).unwrap();
    let preview_request = fixture_request();
    let expected_state = preview_source_edit_expected_state(
        &graph,
        &code_graph,
        &preview_request.context,
        preview_request.observed_at,
        preview_request.edit,
    )
    .await
    .unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        b"old\r\nconcurrent change\n",
    )
    .unwrap();
    let mut stale_request = fixture_request();
    stale_request.idempotency_key = IdempotencyKey::new("source-edit.stale-fixture").unwrap();
    stale_request.expected_state = expected_state;
    let stale = execute_source_edit(
        &graph,
        &code_graph,
        &operation,
        stale_request,
        &authorization,
    )
    .await
    .unwrap();
    assert_eq!(
        stale.effect.unwrap().receipt.outcome,
        EffectTermination::Failed
    );
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        b"old\r\nconcurrent change\n"
    );
}

#[tokio::test]
async fn dry_run_cancellation_before_admission_skips_preview() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/lib.rs"), b"old").unwrap();
    let (graph, code_graph) = fixture_graph(project.path()).await;
    let mut request = fixture_request();
    request.edit = request.edit.clone().with_dry_run(true);
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let authorization = fixture_authorization(&request);
    let cancellation = CancellationSignal::active("cancel.edit.preview").unwrap();
    assert!(cancellation.cancel(UtcMicros(4)));
    let control =
        SourceEditEffectControlV1::new(Deadline::new(UtcMicros(i64::MAX)).unwrap(), cancellation);

    let result = execute_source_edit_with_control(
        &graph,
        &code_graph,
        &operation,
        request,
        &authorization,
        &control,
    )
    .await
    .unwrap();
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::Cancelled
    );
    assert_eq!(fs::read(project.path().join("src/lib.rs")).unwrap(), b"old");
}

#[tokio::test]
async fn live_cancellation_before_effect_keeps_source_unchanged_and_is_durable() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/lib.rs"), b"old").unwrap();
    let (graph, code_graph) = fixture_graph(project.path()).await;
    let mut request = fixture_request();
    request.expected_state = preview_source_edit_expected_state(
        &graph,
        &code_graph,
        &request.context,
        request.observed_at,
        request.edit.clone(),
    )
    .await
    .unwrap();
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let cancellation = CancellationSignal::active("cancel.edit.live").unwrap();
    let authorization = CancelBeforeEffectAuthorization {
        admission: fixture_authorization(&request).0,
        cancellation: cancellation.clone(),
        rechecks: AtomicUsize::new(0),
    };
    let control =
        SourceEditEffectControlV1::new(Deadline::new(UtcMicros(i64::MAX)).unwrap(), cancellation);

    let result = execute_source_edit_with_control(
        &graph,
        &code_graph,
        &operation,
        request,
        &authorization,
        &control,
    )
    .await
    .unwrap();
    let effect = result.effect.unwrap();

    assert_eq!(fs::read(project.path().join("src/lib.rs")).unwrap(), b"old");
    assert_eq!(effect.receipt.outcome, EffectTermination::Cancelled);
    assert_eq!(
        effect.execution.cancellation.unwrap().stage,
        CancellationStage::BeforeEffect
    );
}

#[tokio::test]
async fn move_symbol_rollback_restores_exact_preimages_without_semantic_inverse_drift() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    let original_source = b"pub struct Dependency;\n\npub fn moved(value: Dependency) -> Dependency {\n    value\n}\n\npub fn remains() {}\n";
    let original_destination = b"pub fn marker() {}\n";
    fs::write(project.path().join("src/lib.rs"), original_source).unwrap();
    fs::write(
        project.path().join("src/relocated.rs"),
        original_destination,
    )
    .unwrap();
    git(
        project.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(project.path(), &["add", "src/lib.rs", "src/relocated.rs"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    let (graph, _) = fixture_graph(project.path()).await;
    let code_graph = fixture_symbol_code_graph(
        "src/lib.rs",
        std::str::from_utf8(original_source).unwrap(),
        "pub fn moved(value: Dependency) -> Dependency {\n    value\n}",
        "moved",
        "moved",
    );
    let edit = SourceEditRequest::MoveSymbol {
        symbol: "moved".to_owned(),
        dest_file: "src/relocated.rs".to_owned(),
        dry_run: false,
        update_references: false,
    };
    let mut move_request = fixture_request_for_edit(edit, "source-edit.move-exact");
    let expected_state = preview_source_edit_expected_state(
        &graph,
        &code_graph,
        &move_request.context,
        move_request.observed_at,
        move_request.edit.clone(),
    )
    .await
    .unwrap();
    move_request.expected_state = expected_state;
    let move_operation = source_edit_operation(SourceEditKind::MoveSymbol).unwrap();
    let authorization = fixture_authorization(&move_request);
    let moved = execute_source_edit(
        &graph,
        &code_graph,
        &move_operation,
        move_request.clone(),
        &authorization,
    )
    .await
    .unwrap();
    let moved_effect = moved.effect.unwrap();
    assert_eq!(moved_effect.receipt.outcome, EffectTermination::Completed);
    assert_ne!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        original_source
    );
    assert_ne!(
        fs::read(project.path().join("src/relocated.rs")).unwrap(),
        original_destination
    );

    let rollback_request = SourceEditRollbackRequestV1 {
        context: move_request.context.clone(),
        authority: move_request.authority.clone(),
        effect_id: moved_effect.effect_id.clone(),
        original_idempotency_key: move_request.idempotency_key.clone(),
        idempotency_key: IdempotencyKey::new("source-edit.move-exact.rollback").unwrap(),
        original_input_digest: moved_effect.receipt.input_digest.clone(),
        expected_state: moved_effect.receipt.committed_state.clone().unwrap(),
        proof: move_request.proof.clone(),
        observed_at: move_request.observed_at,
    };
    let rollback_authorization = FixtureSourceEditAuthorization(
        SourceEditAuthorizationAdmissionV1::new(
            rollback_request.authority.clone(),
            rollback_request.proof.clone(),
            rollback_request.context.scope(),
        )
        .unwrap(),
    );
    let rollback_operation = source_edit_rollback_operation().unwrap();
    let rolled_back = execute_source_edit_rollback(
        &graph,
        &rollback_operation,
        rollback_request.clone(),
        &rollback_authorization,
    )
    .await
    .unwrap();

    assert_eq!(
        rolled_back.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        original_source
    );
    assert_eq!(
        fs::read(project.path().join("src/relocated.rs")).unwrap(),
        original_destination
    );
    let replay = execute_source_edit_rollback(
        &graph,
        &rollback_operation,
        rollback_request,
        &rollback_authorization,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
}

/// Admits like the fixture authorization, but parks the first admission until
/// released, holding that edit inside the store's critical section.
struct GatedAuthorization {
    inner: FixtureSourceEditAuthorization,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

impl SourceEditAuthorizationPort for GatedAuthorization {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.acquire().await.expect("gate open").forget();
            self.inner.admit(context, operation, observed_at).await
        })
    }

    fn recheck_effect<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a SourceEditAuthorizationAdmissionV1,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        self.inner
            .recheck_effect(context, operation, admission, observed_at)
    }
}

/// Two agents editing different files of one project at once both commit:
/// the second queues on the daemon-owned store instead of being refused
/// because the first holds its lock.
#[tokio::test]
async fn concurrent_same_project_edits_queue_and_both_commit() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/lib.rs"), b"old").unwrap();
    fs::write(project.path().join("src/other.rs"), b"first").unwrap();
    let (graph, code_graph) = fixture_graph(project.path()).await;
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();

    let mut first = fixture_request();
    let mut second = fixture_request_for_edit(
        SourceEditRequest::StrReplace {
            path: "src/other.rs".to_owned(),
            old_str: "first".to_owned(),
            new_str: "second".to_owned(),
            dry_run: false,
            verify: false,
        },
        "source-edit.concurrent-second",
    );
    for request in [&mut first, &mut second] {
        request.expected_state = preview_source_edit_expected_state(
            &graph,
            &code_graph,
            &request.context,
            request.observed_at,
            request.edit.clone(),
        )
        .await
        .unwrap();
    }
    let gated = GatedAuthorization {
        inner: fixture_authorization(&first),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    };
    let plain = fixture_authorization(&second);

    let (first, second) = tokio::join!(
        execute_source_edit(&graph, &code_graph, &operation, first, &gated),
        async {
            gated.entered.notified().await;
            let second = execute_source_edit(&graph, &code_graph, &operation, second, &plain);
            let release = async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                gated.release.add_permits(1);
            };
            tokio::join!(second, release).0
        },
    );

    let outcome = |result: tracedecay_domain::errors::Result<SourceEditApplicationResult>| {
        result
            .map(|applied| applied.effect.expect("effect").receipt.outcome)
            .map_err(|error| error.to_string())
    };
    assert_eq!(outcome(first), Ok(EffectTermination::Completed));
    assert_eq!(outcome(second), Ok(EffectTermination::Completed));
    assert_eq!(fs::read(project.path().join("src/lib.rs")).unwrap(), b"new");
    assert_eq!(
        fs::read(project.path().join("src/other.rs")).unwrap(),
        b"second"
    );
}
