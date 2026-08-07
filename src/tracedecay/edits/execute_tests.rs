use std::fs;
use std::sync::atomic::AtomicUsize;

use tempfile::tempdir;
use tracedecay_application::{
    CancellationSignal, CancellationStage, Deadline, EffectTermination, IdempotencyKey,
    SourceEditAuthorizationAdmissionV1, SourceEditKind, SourceEditRequest,
    SourceEditRollbackRequestV1, source_edit_operation, source_edit_rollback_operation,
};
use tracedecay_domain::UtcMicros;

use super::test_support::{
    CancelBeforeEffectAuthorization, FixtureSourceEditAuthorization, fixture_authorization,
    fixture_graph, fixture_request, fixture_request_for_edit, git, graph_publication_epoch,
};
use crate::application::edit::{
    SourceEditEffectControlV1, execute_source_edit, execute_source_edit_rollback,
    execute_source_edit_with_control, preview_source_edit_expected_state,
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
    let (graph, _database_scope) = fixture_graph(project.path()).await;
    graph.index_all().await.unwrap();
    let indexed_epoch = graph_publication_epoch(&graph).await;
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
    let applied_epoch = graph_publication_epoch(&graph).await;
    assert_eq!(applied_epoch, indexed_epoch + 1);

    let replay = execute_source_edit(&graph, &operation, apply_request, &authorization)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        fs::read(project.path().join("src/lib.rs")).unwrap(),
        applied
    );
    assert_eq!(graph_publication_epoch(&graph).await, applied_epoch);

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
    let (graph, _database_scope) = fixture_graph(project.path()).await;
    assert!(graph.index_all().await.unwrap().node_count > 0);
    let edit = SourceEditRequest::MoveSymbol {
        symbol: "moved".to_owned(),
        dest_file: "src/relocated.rs".to_owned(),
        dry_run: false,
        update_references: false,
    };
    let expected_state = preview_source_edit_expected_state(&graph, edit.clone())
        .await
        .unwrap();
    let mut move_request = fixture_request_for_edit(edit, "source-edit.move-exact");
    move_request.expected_state = expected_state;
    let move_operation = source_edit_operation(SourceEditKind::MoveSymbol).unwrap();
    let authorization = fixture_authorization(&move_request);
    let moved = execute_source_edit(
        &graph,
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
