use std::{collections::BTreeSet, sync::Arc, time::Duration};

use tempfile::TempDir;
use tracedecay_application::code_index::{
    CodeIndexIgnoredDependencyAdmissionErrorV1, CodeIndexIgnoredDependencyAdmissionRequestV1,
};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_contracts::clock::now_micros;
use tracedecay_contracts::{
    CallableCodeOperationKind, Deadline, RequestContext, ResolvedScope, callable_code_operation,
};
use tracedecay_domain::{CodeGenerationId, UtcMicros};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use super::{
    GitFixture, application_context, test_project_id, wait_for_live_complete_generation, write,
};
use crate::code_index_scheduler::{
    CodeIndexIgnoredDependencyRequestV1, CodeIndexSchedulerRegistryV1, LatestCompleteCodeIndexV1,
};
use crate::project_reads::project_code_index_ignored_dependency_admission_port;

fn request_context(scope: ResolvedScope, budget: Duration) -> RequestContext {
    let operation = callable_code_operation(CallableCodeOperationKind::Callers).expect("operation");
    let budget = i64::try_from(budget.as_micros()).expect("budget micros");
    application_context(&operation, scope.repository_id, scope.worktree_id)
        .with_deadline(Deadline::new(UtcMicros(now_micros().0 + budget)).expect("deadline"))
}

fn ignored_package_fixture() -> GitFixture {
    let fixture = GitFixture::new(&[
        (".gitignore", "node_modules/\n"),
        (
            "src/app.ts",
            "import type { PublicWidget } from \"pkg\";\nexport const anchor = 1;\n",
        ),
    ]);
    write(
        fixture.path(),
        "node_modules/pkg/index.d.ts",
        "export interface PublicWidget { value: string }\n",
    );
    fixture
}

/// What a graph read of `published` that found its verified `pkg` import
/// hands to admission.
fn admission_inputs(
    published: &LatestCompleteCodeIndexV1,
) -> (CodeGenerationId, CodeIndexImportEvidenceV1, ResolvedScope) {
    let mut request =
        CodeIndexIgnoredDependencyRequestV1::for_verified_import_for_test(published, "pkg")
            .expect("verified package import");
    (
        request.expected_generation,
        request.verified_imports.remove(0),
        request.scope,
    )
}

/// A first publication serves from its text owner before the worker's graph
/// tail seats the decoded generation. Admission in that window waits on the
/// seat within its budget: an expired budget is a typed timeout, never an
/// unavailable scheduler, and the seat lets the retry admit the dependency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignored_dependency_admission_waits_for_the_pre_seat_graph_tail() {
    let fixture = ignored_package_fixture();
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let canonical_root = canonical_existing_identity(fixture.path()).expect("canonical fixture");
    let (swap_entered, release_swap) = registry.pause_next_serving_swap(canonical_root);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    assert!(registry.request_complete_generation(fixture.path()).await);
    tokio::time::timeout(Duration::from_secs(10), swap_entered)
        .await
        .expect("publication did not reach its serving swap")
        .expect("serving swap gate stays armed");
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_none(),
        "the decoded generation is not seated yet"
    );

    let scheduler = registry
        .scheduler_for_root(fixture.path())
        .await
        .expect("mounted scheduler");
    let published = tokio::task::spawn_blocking(move || {
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest_complete()
    })
    .await
    .expect("read the published generation")
    .expect("published generation");
    let (source_generation, import, scope) = admission_inputs(&published);
    let port = project_code_index_ignored_dependency_admission_port(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
        true,
    );

    let held = request_context(scope.clone(), Duration::from_millis(300));
    let refused = port
        .admit(CodeIndexIgnoredDependencyAdmissionRequestV1::new(
            &held,
            &source_generation,
            std::slice::from_ref(&import),
        ))
        .await
        .expect_err("the seat is held past the request budget");
    assert_eq!(
        refused,
        CodeIndexIgnoredDependencyAdmissionErrorV1::TimedOut
    );

    let retry_port = Arc::clone(&port);
    let retry_scope = scope.clone();
    let retry_source = source_generation.clone();
    let retry_import = import.clone();
    let retry = tokio::spawn(async move {
        let context = request_context(retry_scope, Duration::from_secs(30));
        retry_port
            .admit(CodeIndexIgnoredDependencyAdmissionRequestV1::new(
                &context,
                &retry_source,
                std::slice::from_ref(&retry_import),
            ))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !retry.is_finished(),
        "admission inside its budget waits for the held seat instead of refusing"
    );
    release_swap.send(()).expect("release serving swap");
    let admitted = retry
        .await
        .expect("retry task joins")
        .expect("the seated generation admits the dependency");
    assert_ne!(admitted, source_generation);

    let serving = registry
        .latest_complete_serving_for_test(fixture.path())
        .await
        .expect("admission seats its generation");
    assert_eq!(serving.generation().manifest().generation_id, admitted);
    assert_eq!(
        serving
            .generation()
            .ignored_source_admissions()
            .iter()
            .map(|admission| admission.logical_path.as_str())
            .collect::<Vec<_>>(),
        ["node_modules/pkg/index.d.ts"]
    );
    let dependency_symbols = serving
        .lexical()
        .iter()
        .filter(|chunk| {
            chunk
                .sanitized_text
                .as_str()
                .contains("interface PublicWidget")
        })
        .map(|chunk| chunk.anchor.symbol_occurrence_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        dependency_symbols.len(),
        1,
        "the dependency declaration is served once: {dependency_symbols:?}"
    );
    registry.shutdown().await;
}

/// A worktree parked on a failure a wake does not clear installs no decoded
/// seat. Admission answers with that park and its remedy instead of spending
/// the request budget on a seat that is not coming.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignored_dependency_admission_answers_a_parked_worktree_with_its_park() {
    let fixture = ignored_package_fixture();
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    first
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount first index");
    assert!(first.request_complete_generation(fixture.path()).await);
    let (source_generation, import, scope) =
        admission_inputs(&wait_for_live_complete_generation(&first, fixture.path()).await);
    first.shutdown().await;

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let _no_pass = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold the remounted worker before its first pass");
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("remount the published store");
    assert!(
        registry
            .plant_terminal_publication_authority_park_for_test(
                fixture.path(),
                "publication authority corrupt before admission",
            )
            .await
    );
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_none(),
        "the remounted worktree has no decoded seat"
    );

    let port = project_code_index_ignored_dependency_admission_port(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
        true,
    );
    let context = request_context(scope, Duration::from_secs(10));
    let refused = port
        .admit(CodeIndexIgnoredDependencyAdmissionRequestV1::new(
            &context,
            &source_generation,
            std::slice::from_ref(&import),
        ))
        .await
        .expect_err("a parked worktree has no generation to admit against");
    let CodeIndexIgnoredDependencyAdmissionErrorV1::Parked(parked) = refused else {
        panic!("admission must answer with the park, observed {refused:?}");
    };
    assert_eq!(
        parked.reason,
        "publication authority corrupt before admission"
    );
    assert!(!parked.retries_on_wake);
    assert_eq!(
        Some(parked),
        registry.convergence_park(fixture.path()).await,
        "the answer is the worktree's live park, remedy included"
    );
    registry.shutdown().await;
}
