use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicUsize;

use tempfile::tempdir;
use tracedecay_contracts::{
    ApplicationOperation, CancellationSignal, CancellationStage, Deadline, EffectTermination,
    IdempotencyKey, RequestContext, SourceEditAuthorizationAdmissionV1,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditKind, SourceEditRequest,
    source_edit_operation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_query::request_graph_cancellation;

use super::test_support::{
    CancelBeforeEffectAuthorization, FixtureSourceEditAuthorization, fixture_authorization,
    fixture_graph, fixture_request, fixture_request_for_edit, fixture_symbol_code_graph, git,
};
use crate::execute::resolve_source_edit_preview;
use crate::{
    SourceEditApplicationResult, SourceEditEffectControlV1, SourceEditOutcome, execute_source_edit,
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
async fn move_symbol_publishes_and_replays_without_retaining_source_bodies() {
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

    let replay = execute_source_edit(
        &graph,
        &code_graph,
        &move_operation,
        move_request,
        &authorization,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect.as_ref().unwrap(), &moved_effect);
    let durability = crate::journal::SourceEditDurability::for_graph(&graph);
    assert!(!durability.journal_path().exists());
    assert!(!durability.root.join("rollback-records").exists());
    let receipt = fs::read_to_string(
        durability
            .receipt_path(&moved_effect.idempotency_key)
            .unwrap(),
    )
    .unwrap();
    assert!(!receipt.contains("Dependency"));
    assert!(!receipt.contains("pub fn"));
}

fn assert_import_fixture_compiles(project: &Path) {
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(compiler)
        .current_dir(project)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--crate-name=move_import_fixture",
            "--emit=metadata",
            "-o",
            "checked.rmeta",
            "src/lib.rs",
        ])
        .output()
        .expect("compile the independent consumer of the moved symbol's dependency");
    assert!(
        output.status.success(),
        "fixture must remain valid Rust: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_move_import_ownership(import: &str, binding: &str, preserve: bool) {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    let moved_symbol = format!("pub fn moved(value: {binding}) -> {binding} {{\n    value\n}}");
    let original_source = format!("{import}\n\n{moved_symbol}\n\npub fn remains() {{}}\n");
    let original_destination = "pub fn marker() {}\n";
    // This module never refers to `moved`; its dependency on the original
    // export path must survive moving that function to a different module.
    let consumer_type = if preserve {
        format!("crate::api::{binding}")
    } else {
        "crate::model::Widget".to_owned()
    };
    let consumer =
        format!("pub fn accept(value: {consumer_type}) -> {consumer_type} {{\n    value\n}}\n");
    for (path, content) in [
        (
            "src/lib.rs",
            "pub mod api;\npub mod consumer;\npub mod model;\npub mod relocated;\n",
        ),
        ("src/api.rs", original_source.as_str()),
        ("src/model.rs", "pub struct Widget;\n"),
        ("src/relocated.rs", original_destination),
        ("src/consumer.rs", consumer.as_str()),
    ] {
        fs::write(project.path().join(path), content).unwrap();
    }
    assert_import_fixture_compiles(project.path());

    let (graph, _) = fixture_graph(project.path()).await;
    let code_graph = fixture_symbol_code_graph(
        "src/api.rs",
        &original_source,
        &moved_symbol,
        "moved",
        "api::moved",
    );
    let mut request = fixture_request_for_edit(
        SourceEditRequest::MoveSymbol {
            symbol: "moved".to_owned(),
            dest_file: "src/relocated.rs".to_owned(),
            dry_run: false,
            update_references: false,
        },
        "source-edit.move-import-ownership",
    );
    let preview = resolve_source_edit_preview(
        &graph,
        &code_graph,
        &request.context,
        request.observed_at,
        request_graph_cancellation(&request.context),
        request.edit.clone(),
    )
    .await
    .unwrap();
    let SourceEditOutcome::Move(preview_move) = &preview.outcome else {
        panic!("expected MoveSymbol preview, got {:?}", preview.outcome);
    };
    assert!(preview_move.success, "{}", preview_move.message);
    let dependency_import = if binding == "Widget" {
        "use crate::model::Widget;".to_owned()
    } else {
        format!("use crate::model::Widget as {binding};")
    };
    assert_eq!(preview_move.applied_imports, vec![dependency_import]);
    let removed = preview_move
        .impact
        .iter()
        .filter(|hint| hint.kind == "orphaned_import_removed")
        .collect::<Vec<_>>();
    assert_eq!(removed.len(), usize::from(!preserve), "{import}");
    let planned_source = preview
        .planned_files
        .iter()
        .find(|file| file.relative_path == "src/api.rs")
        .unwrap();
    assert_eq!(
        planned_source.intended.lines().any(|line| line == import),
        preserve,
        "preview must preserve exported bindings and remove only private orphans: {import}"
    );
    assert_eq!(
        fs::read_to_string(project.path().join("src/api.rs")).unwrap(),
        original_source,
        "preview must not modify the source"
    );
    assert_eq!(
        fs::read_to_string(project.path().join("src/relocated.rs")).unwrap(),
        original_destination,
        "preview must not modify the destination"
    );

    request.expected_state = preview.expected_state.unwrap();
    let operation = source_edit_operation(SourceEditKind::MoveSymbol).unwrap();
    let authorization = fixture_authorization(&request);
    let applied = execute_source_edit(&graph, &code_graph, &operation, request, &authorization)
        .await
        .unwrap();
    assert!(applied.outcome.success(), "{}", applied.outcome.message());
    assert_eq!(
        applied.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    let SourceEditOutcome::Move(applied_move) = &applied.outcome else {
        panic!("expected MoveSymbol result, got {:?}", applied.outcome);
    };
    assert_eq!(applied_move.applied_imports, preview_move.applied_imports);
    assert_eq!(
        applied_move
            .impact
            .iter()
            .filter(|hint| hint.kind == "orphaned_import_removed")
            .count(),
        usize::from(!preserve)
    );
    for planned in &preview.planned_files {
        assert_eq!(
            fs::read_to_string(project.path().join(&planned.relative_path)).unwrap(),
            planned.intended,
            "apply must publish the exact preview for {}",
            planned.relative_path
        );
    }
    assert_eq!(
        fs::read_to_string(project.path().join("src/consumer.rs")).unwrap(),
        consumer,
        "the independent consumer is outside the requested edit"
    );
    assert_import_fixture_compiles(project.path());
}

#[tokio::test]
async fn move_symbol_preserves_public_reexport_in_preview_and_apply() {
    assert_move_import_ownership("pub use crate::model::Widget;", "Widget", true).await;
}

#[tokio::test]
async fn move_symbol_preserves_aliased_reexport_in_preview_and_apply() {
    assert_move_import_ownership(
        "pub use crate::model::Widget as PublicWidget;",
        "PublicWidget",
        true,
    )
    .await;
}

#[tokio::test]
async fn move_symbol_preserves_restricted_reexports_in_preview_and_apply() {
    for visibility in ["pub(crate)", "pub(in crate)"] {
        assert_move_import_ownership(
            &format!("{visibility} use crate::model::Widget;"),
            "Widget",
            true,
        )
        .await;
    }
}

#[tokio::test]
async fn move_symbol_still_removes_an_unused_private_import() {
    assert_move_import_ownership("use crate::model::Widget;", "Widget", false).await;
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
