use std::fs;
use std::sync::atomic::AtomicUsize;

use tempfile::tempdir;
use tracedecay_application::{
    CancellationSignal, CancellationStage, Deadline, EffectTermination, IdempotencyKey,
    SourceEditKind, source_edit_operation,
};
use tracedecay_domain::UtcMicros;

use super::test_support::{
    CancelBeforeEffectAuthorization, fixture_authorization, fixture_graph, fixture_request,
};
use crate::application::edit::{
    SourceEditEffectControlV1, execute_source_edit, execute_source_edit_with_control,
    preview_source_edit_expected_state,
};

#[tokio::test]
async fn preview_apply_replay_and_expected_state_cas_preserve_exact_bytes() {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    let initial = b"old\r\nunchanged \xE2\x98\x83\n";
    let applied = b"new\r\nunchanged \xE2\x98\x83\n";
    fs::write(project.path().join("src/lib.rs"), initial).unwrap();
    let (graph, _database_scope) = fixture_graph(project.path()).await;
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
    let request = fixture_request();
    let authorization = fixture_authorization(&request);

    let mut preview_request = request.clone();
    preview_request.edit = preview_request.edit.clone().with_dry_run(true);
    let preview = execute_source_edit(&graph, &operation, preview_request, &authorization)
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
    let applied_result =
        execute_source_edit(&graph, &operation, apply_request.clone(), &authorization)
            .await
            .unwrap();
    assert!(applied_result.outcome.success());
    assert!(!applied_result.replayed);
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        applied
    );

    let replay = execute_source_edit(&graph, &operation, apply_request, &authorization)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        applied
    );

    fs::write(project.path().join("src/lib.rs"), initial).unwrap();
    let expected_state = preview_source_edit_expected_state(&graph, fixture_request().edit)
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
    let stale = execute_source_edit(&graph, &operation, stale_request, &authorization)
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
    let (graph, _database_scope) = fixture_graph(project.path()).await;
    let mut request = fixture_request();
    request.edit = request.edit.clone().with_dry_run(true);
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let authorization = fixture_authorization(&request);
    let cancellation = CancellationSignal::active("cancel.edit.preview").unwrap();
    assert!(cancellation.cancel(UtcMicros(4)));
    let control =
        SourceEditEffectControlV1::new(Deadline::new(UtcMicros(i64::MAX)).unwrap(), cancellation);

    let result =
        execute_source_edit_with_control(&graph, &operation, request, &authorization, &control)
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
    let (graph, _database_scope) = fixture_graph(project.path()).await;
    let mut request = fixture_request();
    request.expected_state = preview_source_edit_expected_state(&graph, request.edit.clone())
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

    let result =
        execute_source_edit_with_control(&graph, &operation, request, &authorization, &control)
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
