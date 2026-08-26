use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_domain::{
    ActorId, CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::code_index::{
    CodeIndexIgnoredDependencyAdmissionErrorV1, CodeIndexIgnoredDependencyAdmissionPortV1,
    CodeIndexIgnoredDependencyAdmissionRequestV1,
};

use super::project_code_index_ignored_dependency_admission_port;
use crate::daemon::code_index_scheduler::{
    CodeIndexSchedulerRegistryV1, LatestCompleteCodeIndexV1,
};

const PROJECT_ID: &str = "project.project-open-ignored-dependency";

struct Fixture {
    root: TempDir,
    _store: TempDir,
    registry: CodeIndexSchedulerRegistryV1,
    scope: ResolvedScope,
    generation: CodeGenerationId,
    import: CodeIndexImportEvidenceV1,
}

impl Fixture {
    async fn mount() -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        write(root.path(), ".gitignore", b"node_modules/\n");
        write(
            root.path(),
            "src/app.ts",
            br#"import type { PublicWidget } from "pkg";
export function generationAnchor() { return 1; }
"#,
        );
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        write(
            root.path(),
            "node_modules/pkg/index.d.ts",
            b"export interface PublicWidget { value: string }\n",
        );
        write(
            root.path(),
            "node_modules/pkg/private.d.ts",
            b"export interface PrivateWidget { secret: string }\n",
        );

        let store = TempDir::new().expect("store root");
        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(project_id(), root.path(), store.path().to_path_buf(), None)
            .await
            .expect("mount code-index scheduler");
        wait_for_initial_generation(&registry, root.path()).await;
        let baseline = latest(&registry, root.path()).await;
        let generation = baseline.generation();
        let snapshot = generation.snapshot();
        let scope = ResolvedScope::new(
            generation.manifest().project_id.clone(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree identity"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope");
        let import = generation
            .imports()
            .iter()
            .find(|import| import.module_specifier == "pkg")
            .expect("parser-verified package import")
            .clone();
        let generation = generation.manifest().generation_id.clone();

        Self {
            root,
            _store: store,
            registry,
            scope,
            generation,
            import,
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn port(&self, writable: bool) -> Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1> {
        project_code_index_ignored_dependency_admission_port(
            self.registry.clone(),
            self.root().to_path_buf(),
            self.scope.clone(),
            writable,
        )
    }

    fn request<'a>(
        &'a self,
        context: &'a RequestContext,
        generation: &'a CodeGenerationId,
    ) -> CodeIndexIgnoredDependencyAdmissionRequestV1<'a> {
        CodeIndexIgnoredDependencyAdmissionRequestV1::new(
            context,
            generation,
            std::slice::from_ref(&self.import),
        )
    }

    async fn serving(&self) -> LatestCompleteCodeIndexV1 {
        latest(&self.registry, self.root()).await
    }

    async fn assert_unchanged(&self) {
        let serving = self.serving().await;
        assert_eq!(
            serving.generation().manifest().generation_id,
            self.generation
        );
        assert!(
            serving.generation().ignored_source_admissions().is_empty(),
            "a refused project binding must not mutate the ignored-source roster"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_project_binding_refuses_before_scheduler_mutation() {
    let fixture = Fixture::mount().await;
    let context = request_context(fixture.scope.clone(), "read-only");

    let error = fixture
        .port(false)
        .admit(fixture.request(&context, &fixture.generation))
        .await
        .expect_err("a read-only project database cannot schedule source mutation");

    assert_eq!(error, CodeIndexIgnoredDependencyAdmissionErrorV1::ReadOnly);
    fixture.assert_unchanged().await;
    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_pin_reports_the_exact_active_generation_for_the_bound_root_and_scope() {
    let fixture = Fixture::mount().await;
    let context = request_context(fixture.scope.clone(), "stale");
    let stale = CodeGenerationId::new("generation.project-open.stale")
        .expect("syntactically valid stale generation");

    let error = fixture
        .port(true)
        .admit(fixture.request(&context, &stale))
        .await
        .expect_err("a superseded generation pin must fail closed");

    assert_eq!(
        error,
        CodeIndexIgnoredDependencyAdmissionErrorV1::Stale {
            active_generation: fixture.generation.clone(),
        }
    );
    fixture.assert_unchanged().await;
    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_request_context_scope_is_refused_before_scheduler_mutation() {
    let fixture = Fixture::mount().await;
    let foreign_scope = ResolvedScope::new(
        ProjectId::new("project.project-open-foreign").expect("foreign project"),
        RepositoryId::new("repository.project-open-foreign").expect("foreign repository"),
        WorktreeId::new("worktree.project-open-foreign").expect("foreign worktree"),
        None,
    )
    .expect("foreign scope");
    let context = request_context(foreign_scope, "foreign-scope");

    let error = fixture
        .port(true)
        .admit(fixture.request(&context, &fixture.generation))
        .await
        .expect_err("a foreign request scope cannot borrow this project binding");

    assert!(
        matches!(
            error,
            CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable { .. }
        ),
        "scope refusal must remain typed and cannot reach the scheduler: {error:?}"
    );
    fixture.assert_unchanged().await;
    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writable_binding_returns_only_after_exact_scope_generation_is_warm_and_serving() {
    let fixture = Fixture::mount().await;
    let context = request_context(fixture.scope.clone(), "writable");

    let admitted = fixture
        .port(true)
        .admit(fixture.request(&context, &fixture.generation))
        .await
        .expect("verified ignored dependency admission");
    let serving = fixture
        .registry
        .latest_complete_ready_decoded_for_root_scope(fixture.root(), &fixture.scope)
        .await
        .expect("the exact root and scope are serving when admission returns");

    assert_ne!(admitted, fixture.generation);
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
    assert!(
        !serving
            .generation()
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "node_modules/pkg/private.d.ts"),
        "the project binding cannot widen the scheduler beyond the exact entrypoint"
    );
    let graph = serving
        .interactive_graph_store()
        .expect("activated serving generation owns an interactive graph");
    assert_eq!(
        graph.interactive_catalog_is_warm(),
        Ok(true),
        "admission cannot return a cold graph generation"
    );
    fixture.registry.shutdown().await;
}

fn project_id() -> ProjectId {
    ProjectId::new(PROJECT_ID).expect("project id")
}

fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let capability =
        CapabilityId::new("capability.project-open-ignored-dependency").expect("capability");
    let use_case = UseCaseId::new("use-case.project-open-ignored-dependency").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.project-open-{suffix}")).expect("grant"),
        1,
        digest::<ManifestDigest>('a'),
        ActorId::new("actor.project-open-issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.project-open-requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.project-open-{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(format!("cancellation.project-open-{suffix}"))
            .expect("cancellation"),
    )
    .expect("request context")
}

async fn wait_for_initial_generation(registry: &CodeIndexSchedulerRegistryV1, project_root: &Path) {
    if registry.latest_generation_id(project_root).await.is_some() {
        return;
    }
    let canonical_root = project_root.canonicalize().expect("canonical fixture root");
    let mut publications = registry.subscribe_generation_publications();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root == canonical_root {
                break;
            }
        }
    })
    .await
    .expect("initial generation publication");
}

async fn latest(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> LatestCompleteCodeIndexV1 {
    registry
        .latest_complete_fresh(project_root)
        .await
        .expect("fresh serving generation")
}

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .current_dir(root)
        .args(arguments)
        .status()
        .expect("run git fixture command");
    assert!(
        status.success(),
        "git fixture command failed: {arguments:?}"
    );
}

fn write(root: &Path, logical_path: &str, contents: &[u8]) {
    let path = root.join(logical_path);
    std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    std::fs::write(path, contents).expect("write fixture file");
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}
