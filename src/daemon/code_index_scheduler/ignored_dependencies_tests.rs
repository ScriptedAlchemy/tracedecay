use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, CodeIndexIgnoredSourceAdmissionV1,
    MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1,
};
use tracedecay_domain::{CodeGenerationId, ProjectId};
use tracedecay_graph_db::NeverCancelled;

use super::{
    CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexIgnoredDependencyRefusalV1,
    CodeIndexIgnoredDependencyRequestV1, CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
    LatestCompleteCodeIndexV1,
};

mod branch_publication_tests;
mod cancellation_tests;
mod flight_tests;
mod retained_roster_tests;

const PROJECT_ID: &str = "project.ignored-dependency-scheduler-tests";

struct GitFixture {
    root: TempDir,
}

impl GitFixture {
    fn new(source: &str) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        write(root.path(), ".gitignore", b"node_modules/\n");
        write(root.path(), "src/app.ts", source.as_bytes());
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        write(self.path(), path, contents.as_ref());
    }
}

#[derive(Clone)]
struct StaticControl {
    cancelled: bool,
    deadline_exceeded: bool,
}

impl StaticControl {
    fn active() -> Arc<Self> {
        Arc::new(Self {
            cancelled: false,
            deadline_exceeded: false,
        })
    }

    fn cancelled() -> Arc<Self> {
        Arc::new(Self {
            cancelled: true,
            deadline_exceeded: false,
        })
    }

    fn deadline_exceeded() -> Arc<Self> {
        Arc::new(Self {
            cancelled: false,
            deadline_exceeded: true,
        })
    }
}

impl CodeIndexExecutionControlV1 for StaticControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.deadline_exceeded
    }
}

fn project_id() -> ProjectId {
    ProjectId::new(PROJECT_ID).expect("valid project id")
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
    std::fs::create_dir_all(path.parent().expect("fixture file parent"))
        .expect("create fixture file parent");
    std::fs::write(path, contents).expect("write fixture file");
}

fn write_types_package(root: &Path, module: &str, declarations: &str) {
    write(
        root,
        &format!("node_modules/{module}/index.d.ts"),
        declarations.as_bytes(),
    );
}

async fn mount(
    fixture_root: &Path,
    store: &TempDir,
    capacity: usize,
) -> CodeIndexSchedulerRegistryV1 {
    let registry = CodeIndexSchedulerRegistryV1::new(capacity);
    registry
        .mount_worktree(project_id(), fixture_root, store.path().to_path_buf(), None)
        .await
        .expect("mount ignored-dependency fixture");
    wait_for_initial_generation(&registry, fixture_root).await;
    registry
}

async fn wait_for_initial_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(project_root).await {
                break generation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial generation publication")
}

async fn wait_for_generation_change(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    previous: &CodeGenerationId,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(project_root).await
                && &generation != previous
            {
                break generation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("changed generation publication")
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

async fn wait_for_reconciling(registry: &CodeIndexSchedulerRegistryV1, expected: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry.memory_stats().await.reconciling_worktrees == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background reconcile reaches expected state");
}

async fn reconcile_through_worker(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
) {
    registry.clear_pending_wake_for_scope(scope).await;
    let scheduler = registry
        .scheduler_handle(project_root)
        .await
        .expect("mounted scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler hold");
        release_rx.recv().expect("release scheduler hold");
    });
    held_rx.recv().expect("scheduler is held");
    assert!(registry.notify_hook_overflow(project_root).await);
    wait_for_reconciling(registry, 1).await;
    release_tx.send(()).expect("release scheduler");
    lock_thread.join().expect("scheduler holder joins");
    wait_for_reconciling(registry, 0).await;
}

fn request_for(
    latest: &LatestCompleteCodeIndexV1,
    module: &str,
) -> CodeIndexIgnoredDependencyRequestV1 {
    let generation = latest.generation();
    let import = generation
        .imports()
        .iter()
        .find(|import| import.module_specifier == module)
        .unwrap_or_else(|| panic!("verified import row for {module}"))
        .clone();
    let snapshot = generation.snapshot();
    let scope = ResolvedScope::new(
        generation.manifest().project_id.clone(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    CodeIndexIgnoredDependencyRequestV1 {
        scope,
        expected_generation: generation.manifest().generation_id.clone(),
        verified_imports: vec![import],
    }
}

fn roster_paths(latest: &LatestCompleteCodeIndexV1) -> Vec<&str> {
    latest
        .generation()
        .ignored_source_admissions()
        .iter()
        .map(|admission| admission.logical_path.as_str())
        .collect()
}

fn snapshot_paths(latest: &LatestCompleteCodeIndexV1) -> Vec<&str> {
    latest
        .generation()
        .snapshot()
        .files
        .iter()
        .map(|file| file.logical_path.as_str())
        .collect()
}

async fn index_dependency(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    request: CodeIndexIgnoredDependencyRequestV1,
    control: Arc<dyn CodeIndexExecutionControlV1 + Send + Sync>,
) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
    registry
        .index_verified_ignored_dependency(project_root, request, control)
        .await
}

fn assert_refusal(error: CodeIndexSchedulerErrorV1, expected: CodeIndexIgnoredDependencyRefusalV1) {
    assert!(
        matches!(&error, CodeIndexSchedulerErrorV1::IgnoredDependency(refusal) if refusal == &expected),
        "unexpected ignored-dependency refusal: {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_type_import_indexes_only_the_resolved_ignored_entrypoint() {
    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor() { return 1; }
"#,
    );
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface PublicWidget { value: string }\n",
    );
    fixture.write(
        "node_modules/pkg/private.d.ts",
        "export interface PrivateWidget { secret: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    crate::storage::pin_fixture_repository_identity(fixture.path(), PROJECT_ID)
        .expect("project enrollment");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 91, "ignored-dependency-graph")
            .expect("daemon database scope");
    let graph_runtime = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id(), [fixture.path().to_path_buf()])
        .await
        .expect("project graph database");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree_with_graph_runtime(
            project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
            Arc::clone(&graph_runtime),
            project_database,
        )
        .await
        .expect("mount persistent graph-backed scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let baseline = latest(&registry, fixture.path()).await;
    let baseline_generation = baseline.generation().manifest().generation_id.clone();

    let outcome = index_dependency(
        &registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect("verified ignored dependency indexes");
    let served = latest(&registry, fixture.path()).await;

    assert_ne!(outcome.generation_id, baseline_generation);
    assert_eq!(
        outcome.admission,
        CodeIndexIgnoredSourceAdmissionV1 {
            logical_path: "node_modules/pkg/index.d.ts".to_owned(),
        }
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(outcome.generation_id.clone()),
        "the call returns only after graph activation and serving swap"
    );
    assert_eq!(roster_paths(&served), ["node_modules/pkg/index.d.ts"]);
    assert_eq!(
        served.generation().repository_parse_identity().dirty,
        tracedecay_domain::RepositoryDirtyStateV1::Dirty,
        "an ignored-source roster is a truthful dirty snapshot"
    );
    assert!(
        served.generation().snapshot().source_revision.is_none(),
        "an ignored-source roster cannot claim immutable HEAD authority"
    );
    assert!(
        snapshot_paths(&served).contains(&"node_modules/pkg/index.d.ts"),
        "the exact resolved entrypoint joins the ordinary immutable snapshot"
    );
    assert!(
        !snapshot_paths(&served).contains(&"node_modules/pkg/private.d.ts"),
        "lazy admission never widens to the ignored dependency directory"
    );
    let graph_store = served
        .interactive_graph_store()
        .expect("admission returns only after persistent graph activation");
    assert_eq!(
        graph_store.interactive_catalog_is_warm(),
        Ok(true),
        "serving publication requires its generation-pinned catalog to be warm"
    );
    let graph_reader = graph_store
        .interactive_reader_with_cancellation(&outcome.generation_id, Arc::new(NeverCancelled))
        .expect("generation-pinned interactive graph reader");
    let dependency_file = graph_reader
        .file_by_logical_path("node_modules/pkg/index.d.ts", Arc::new(NeverCancelled))
        .expect("already-warm catalog lookup")
        .expect("admitted dependency file is in the verified graph catalog");
    assert_eq!(dependency_file.logical_path, "node_modules/pkg/index.d.ts");
    let symbols = graph_reader
        .resolve_simple_name("PublicWidget", None, 4, Arc::new(NeverCancelled))
        .expect("verified graph symbol lookup");
    assert!(symbols.iter().any(|symbol| {
        symbol
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.simple_name == "PublicWidget")
    }));
    registry.shutdown().await;
}

// Builds its escape fixture with unix symlinks; Windows symlink creation
// needs privileges the CI runner does not hold.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsafe_or_unverified_ignored_entrypoints_preserve_the_serving_head() {
    let fixture = GitFixture::new(
        r#"import type { TraversalType } from "traversal";
import type { SymlinkType } from "symlinked";
import type { TextType } from "unsupported";
import type { HugeType } from "oversized";
import type { PrivateType } from "privacy";
import { RuntimeOnly } from "runtime-only";
"#,
    );
    fixture.write(
        "node_modules/traversal/package.json",
        br#"{"types":"../../../outside.d.ts"}"#,
    );
    fixture.write(
        "node_modules/unsupported/package.json",
        br#"{"types":"index.txt"}"#,
    );
    fixture.write("node_modules/unsupported/index.txt", b"export TextType\n");
    fixture.write(
        "node_modules/oversized/index.d.ts",
        vec![b'x'; MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1 + 1],
    );
    fixture.write("node_modules/privacy/index.d.ts", br#"{"token":"#);
    fixture.write(
        "node_modules/runtime-only/index.d.ts",
        b"export const RuntimeOnly: number;\n",
    );
    let outside = TempDir::new().expect("outside symlink target");
    write(
        outside.path(),
        "index.d.ts",
        b"export interface SymlinkType {}\n",
    );
    std::fs::create_dir_all(fixture.path().join("node_modules/symlinked"))
        .expect("symlink package directory");
    std::os::unix::fs::symlink(
        outside.path().join("index.d.ts"),
        fixture.path().join("node_modules/symlinked/index.d.ts"),
    )
    .expect("outside dependency symlink");

    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let baseline_generation = baseline.generation().manifest().generation_id.clone();

    let mut unverified = request_for(&baseline, "traversal");
    unverified.verified_imports[0].imported_name = Some("FabricatedType".to_owned());
    let cases = [
        (
            unverified,
            CodeIndexIgnoredDependencyRefusalV1::UnverifiedImportEvidence,
        ),
        (
            request_for(&baseline, "runtime-only"),
            CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport,
        ),
        (
            request_for(&baseline, "traversal"),
            CodeIndexIgnoredDependencyRefusalV1::PathEscape,
        ),
        (
            request_for(&baseline, "symlinked"),
            CodeIndexIgnoredDependencyRefusalV1::SymlinkEscape,
        ),
        (
            request_for(&baseline, "unsupported"),
            CodeIndexIgnoredDependencyRefusalV1::UnsupportedLanguage,
        ),
        (
            request_for(&baseline, "oversized"),
            CodeIndexIgnoredDependencyRefusalV1::ByteLimitExceeded,
        ),
        (
            request_for(&baseline, "privacy"),
            CodeIndexIgnoredDependencyRefusalV1::PrivacyRefused,
        ),
    ];

    for (request, expected) in cases {
        let error = index_dependency(&registry, fixture.path(), request, StaticControl::active())
            .await
            .expect_err("unsafe ignored source must be refused");
        assert_refusal(error, expected);
        assert_eq!(
            registry.latest_generation_id(fixture.path()).await,
            Some(baseline_generation.clone()),
            "a refusal cannot replace the serving generation"
        );
    }
    assert!(
        roster_paths(&latest(&registry, fixture.path()).await).is_empty(),
        "refused paths never enter the durable roster"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_tracked_dependency_entrypoint_is_not_admitted_as_ignored_source() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    git(
        fixture.path(),
        &["add", "-f", "node_modules/pkg/index.d.ts"],
    );
    git(fixture.path(), &["commit", "-qm", "track dependency"]);
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let head = baseline.generation().manifest().generation_id.clone();

    let error = index_dependency(
        &registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect_err("tracked entrypoint is not an ignored-source admission");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::NotIgnored);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(head)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_request_cannot_admit_two_ignored_dependency_entrypoints() {
    let fixture = GitFixture::new(
        "import type { Alpha } from \"alpha\";\nimport type { Beta } from \"beta\";\n",
    );
    write_types_package(
        fixture.path(),
        "alpha",
        "export interface Alpha { value: string }\n",
    );
    write_types_package(
        fixture.path(),
        "beta",
        "export interface Beta { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let head = baseline.generation().manifest().generation_id.clone();
    let mut request = request_for(&baseline, "alpha");
    request
        .verified_imports
        .extend(request_for(&baseline, "beta").verified_imports);

    let error = index_dependency(&registry, fixture.path(), request, StaticControl::active())
        .await
        .expect_err("one request cannot widen to two dependency entrypoints");

    assert_refusal(
        error,
        CodeIndexIgnoredDependencyRefusalV1::EntryPointLimitExceeded,
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(head)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_ignored_dependency_admission_preserves_the_serving_head() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let head = baseline.generation().manifest().generation_id.clone();

    let error = index_dependency(
        &registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::cancelled(),
    )
    .await
    .expect_err("cancelled admission must abstain");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::Cancelled);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(head)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadline_exceeded_ignored_dependency_admission_preserves_the_serving_head() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let head = baseline.generation().manifest().generation_id.clone();

    let error = index_dependency(
        &registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::deadline_exceeded(),
    )
    .await
    .expect_err("expired admission must abstain");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::DeadlineExceeded);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(head)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superseded_real_generation_pin_preserves_the_successor_head() {
    let fixture =
        GitFixture::new("import type { Widget } from \"pkg\";\nexport const revision = 1;\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let stale_request = request_for(&baseline, "pkg");
    let old_generation = baseline.generation().manifest().generation_id.clone();

    fixture.write(
        "src/app.ts",
        "import type { Widget } from \"pkg\";\nexport const revision = 2;\n",
    );
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/app.ts"))
            .await
    );
    let successor = wait_for_generation_change(&registry, fixture.path(), &old_generation).await;
    let error = index_dependency(
        &registry,
        fixture.path(),
        stale_request,
        StaticControl::active(),
    )
    .await
    .expect_err("superseded real generation must fail closed");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::StaleGeneration);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(successor)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_concurrent_ignored_dependency_requests_publish_one_generation() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let request = request_for(&baseline, "pkg");
    let mut publications = registry.subscribe_generation_publications();

    let (left, right) = tokio::join!(
        index_dependency(
            &registry,
            fixture.path(),
            request.clone(),
            StaticControl::active(),
        ),
        index_dependency(&registry, fixture.path(), request, StaticControl::active(),),
    );
    let left = left.expect("first duplicate admission");
    let right = right.expect("second duplicate admission");

    assert_eq!(left.generation_id, right.generation_id);
    // Serving publication happens-before every admission return: the owner
    // broadcasts before returning, and coalesced followers settle only from
    // the owner's finished flight. Both admissions above have returned, so
    // the receiver subscribed before the join already holds every publication
    // these requests could produce — read it without a wall-clock bound.
    let publication = publications
        .try_recv()
        .expect("one generation publication is sealed before admission returns");
    assert_eq!(publication.generation_id, left.generation_id);
    assert!(
        matches!(
            publications.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "duplicate in-flight requests coalesce instead of publishing twice"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_reconcile_retains_the_exact_ignored_source_roster() {
    let fixture =
        GitFixture::new("import type { Widget } from \"pkg\";\nexport const revision = 1;\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let admitted = index_dependency(
        &registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect("initial ignored admission");

    fixture.write(
        "src/app.ts",
        "import type { Widget } from \"pkg\";\nexport const revision = 2;\n",
    );
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/app.ts"))
            .await
    );
    let changed =
        wait_for_generation_change(&registry, fixture.path(), &admitted.generation_id).await;
    let served = latest(&registry, fixture.path()).await;

    assert_eq!(changed, served.generation().manifest().generation_id);
    assert_eq!(roster_paths(&served), ["node_modules/pkg/index.d.ts"]);
    assert!(snapshot_paths(&served).contains(&"node_modules/pkg/index.d.ts"));
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_witness_restores_and_revalidates_the_exact_ignored_roster() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let first_registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&first_registry, fixture.path()).await;
    let admitted = index_dependency(
        &first_registry,
        fixture.path(),
        request_for(&baseline, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect("ignored dependency admission");
    first_registry.shutdown().await;

    let unchanged_registry = mount(fixture.path(), &store, 1).await;
    let unchanged = latest(&unchanged_registry, fixture.path()).await;
    assert_eq!(
        unchanged.generation().manifest().generation_id,
        admitted.generation_id,
        "an unchanged exact roster restores the sealed generation"
    );
    assert_eq!(roster_paths(&unchanged), ["node_modules/pkg/index.d.ts"]);
    unchanged_registry.shutdown().await;

    fixture.write(
        "node_modules/pkg/index.d.ts",
        "export interface Widget { changed: true }\n",
    );
    let changed_registry = mount(fixture.path(), &store, 1).await;
    let changed =
        wait_for_generation_change(&changed_registry, fixture.path(), &admitted.generation_id)
            .await;
    let served = latest(&changed_registry, fixture.path()).await;
    assert_eq!(changed, served.generation().manifest().generation_id);
    assert_eq!(roster_paths(&served), ["node_modules/pkg/index.d.ts"]);
    assert!(snapshot_paths(&served).contains(&"node_modules/pkg/index.d.ts"));
    changed_registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linked_worktree_owner_scope_is_refused_against_the_sibling_root() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { owner: true }\n",
    );
    let linked_parent = TempDir::new().expect("linked worktree parent");
    let linked = linked_parent.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        fixture.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-scope",
            linked_arg,
            "main",
        ],
    );
    write_types_package(
        &linked,
        "pkg",
        "export interface Widget { sibling: true }\n",
    );
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for root in [fixture.path(), linked.as_path()] {
        registry
            .mount_worktree(project_id(), root, store.path().to_path_buf(), None)
            .await
            .expect("mount linked worktree");
        wait_for_initial_generation(&registry, root).await;
    }
    let owner = latest(&registry, fixture.path()).await;
    let owner_head = owner.generation().manifest().generation_id.clone();
    let sibling_head = latest(&registry, &linked)
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();

    let error = index_dependency(
        &registry,
        &linked,
        request_for(&owner, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect_err("owner scope cannot authorize its sibling worktree root");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(owner_head)
    );
    assert_eq!(
        registry.latest_generation_id(&linked).await,
        Some(sibling_head)
    );
    assert!(roster_paths(&latest(&registry, fixture.path()).await).is_empty());
    assert!(roster_paths(&latest(&registry, &linked).await).is_empty());
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linked_worktrees_keep_ignored_dependency_rosters_isolated() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface Widget { owner: true }\n",
    );
    let linked_parent = TempDir::new().expect("linked worktree parent");
    let linked = linked_parent.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        fixture.path(),
        &["worktree", "add", "-q", "-b", "linked", linked_arg, "main"],
    );
    write_types_package(
        &linked,
        "pkg",
        "export interface Widget { sibling: true }\n",
    );

    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for root in [fixture.path(), linked.as_path()] {
        registry
            .mount_worktree(project_id(), root, store.path().to_path_buf(), None)
            .await
            .expect("mount linked worktree");
        wait_for_initial_generation(&registry, root).await;
    }
    let owner_before = latest(&registry, fixture.path()).await;
    let sibling_before = latest(&registry, &linked).await;
    let sibling_head = sibling_before.generation().manifest().generation_id.clone();

    index_dependency(
        &registry,
        fixture.path(),
        request_for(&owner_before, "pkg"),
        StaticControl::active(),
    )
    .await
    .expect("owner ignored admission");
    let owner_after = latest(&registry, fixture.path()).await;
    let sibling_after = latest(&registry, &linked).await;

    assert_eq!(roster_paths(&owner_after), ["node_modules/pkg/index.d.ts"]);
    assert!(roster_paths(&sibling_after).is_empty());
    assert_eq!(
        sibling_after.generation().manifest().generation_id,
        sibling_head,
        "admitting one linked worktree cannot replace its sibling head"
    );
    assert!(
        !snapshot_paths(&sibling_after).contains(&"node_modules/pkg/index.d.ts"),
        "the sibling never inherits another worktree's ignored source"
    );
    registry.shutdown().await;
}
