//! Checkout-identity admission coverage for the project code-graph read port.
//!
//! The port retains the `ResolvedScope` that was live at project open. A
//! request scope is resolved against live HEAD, so after any ordinary
//! `git switch` the two carry different branch labels (and therefore
//! different scope digests) while naming the same physical checkout. The
//! port must keep serving that checkout and must still deny a genuinely
//! different worktree.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RefId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::graph::{CodeGraphReadError, CodeGraphReadRequest};

use super::project_code_graph_projection_read_port;
use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

const PROJECT_ID: &str = "project.project-open-code-graph-scope";

struct Fixture {
    root: TempDir,
    _store: TempDir,
    registry: CodeIndexSchedulerRegistryV1,
    /// The scope that was live when the route opened: the sealed label.
    retained_scope: ResolvedScope,
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
        write(
            root.path(),
            "src/lib.rs",
            b"pub fn scope_anchor() -> u32 { 7 }\n",
        );
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);

        let store = TempDir::new().expect("store root");
        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(project_id(), root.path(), store.path().to_path_buf(), None)
            .await
            .expect("mount code-index scheduler");
        wait_for_initial_generation(&registry, root.path()).await;
        let latest = wait_for_ready_root_generation(&registry, root.path()).await;
        let generation = latest.generation();
        let snapshot = generation.snapshot();
        let retained_scope = ResolvedScope::new(
            generation.manifest().project_id.clone(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree identity"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope");

        Self {
            root,
            _store: store,
            registry,
            retained_scope,
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// The same checkout under a branch label the retained scope has moved
    /// past — the shape every request has after an ordinary `git switch`.
    fn moved_label_scope(&self) -> ResolvedScope {
        ResolvedScope::new(
            self.retained_scope.project_id.clone(),
            self.retained_scope.repository_id.clone(),
            self.retained_scope.worktree_id.clone(),
            Some(RefId::new("refs/heads/moved-after-open").expect("moved reference")),
        )
        .expect("moved scope")
    }

    fn foreign_worktree_scope(&self) -> ResolvedScope {
        ResolvedScope::new(
            self.retained_scope.project_id.clone(),
            self.retained_scope.repository_id.clone(),
            WorktreeId::new("worktree.some-other-checkout").expect("worktree id"),
            self.retained_scope.reference.clone(),
        )
        .expect("foreign scope")
    }
}

/// A branch-label move on the same checkout must pass the port's scope
/// admission. This fixture runs a bare scheduler registry with no persistent
/// Grafeo activation authority, and interactive reads deliberately have no
/// in-memory fallback — so a fully served read is unreachable here and both
/// scopes terminate at the typed not-activated state. The assertion is the
/// admission decision itself: the moved label reaches activation (the same
/// terminal the retained scope gets) instead of the scope denial a foreign
/// worktree receives. The full served moved-label journey is proven against
/// real activation by
/// `daemon::code_index_scheduler::tests::moved_reference_label_still_serves_the_exact_worktree_as_current`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moved_branch_label_still_opens_the_exact_checkouts_graph_read() {
    let fixture = Fixture::mount().await;
    let port = project_code_graph_projection_read_port(
        fixture.registry.clone(),
        fixture.root().to_path_buf(),
        fixture.retained_scope.clone(),
    );
    let moved = fixture.moved_label_scope();
    assert_ne!(
        moved, fixture.retained_scope,
        "the moved label must produce a distinct full scope (label and digest)"
    );

    let retained_context = request_context(fixture.retained_scope.clone(), "retained-label");
    let retained_terminal = port
        .open(CodeGraphReadRequest::from_context(&retained_context, UtcMicros(1)))
        .await
        .expect_err("this fixture has no persistent activation, so even the retained scope stops there");
    assert!(
        !matches!(retained_terminal, CodeGraphReadError::Denied),
        "the retained scope must never be scope-denied: {retained_terminal:?}"
    );

    let context = request_context(moved, "moved-label");
    let moved_terminal = port
        .open(CodeGraphReadRequest::from_context(&context, UtcMicros(1)))
        .await
        .expect_err("same fixture, same activation terminal");
    assert!(
        !matches!(moved_terminal, CodeGraphReadError::Denied),
        "a branch-label move on the same worktree must pass scope admission: {moved_terminal:?}"
    );
    assert_eq!(
        format!("{retained_terminal:?}"),
        format!("{moved_terminal:?}"),
        "the moved label must terminate exactly where the retained scope does"
    );

    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_worktree_is_still_denied_the_retained_graph_read() {
    let fixture = Fixture::mount().await;
    let port = project_code_graph_projection_read_port(
        fixture.registry.clone(),
        fixture.root().to_path_buf(),
        fixture.retained_scope.clone(),
    );
    let context = request_context(fixture.foreign_worktree_scope(), "foreign-worktree");

    let error = port
        .open(CodeGraphReadRequest::from_context(&context, UtcMicros(1)))
        .await
        .expect_err("a different worktree identity cannot borrow this route's graph");
    assert!(
        matches!(error, CodeGraphReadError::Denied),
        "foreign checkout refusal must stay a typed denial: {error:?}"
    );

    fixture.registry.shutdown().await;
}

fn project_id() -> ProjectId {
    ProjectId::new(PROJECT_ID).expect("project id")
}

fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let capability =
        CapabilityId::new("capability.project-open-code-graph-scope").expect("capability");
    let use_case = UseCaseId::new("use-case.project-open-code-graph-scope").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.code-graph-scope-{suffix}")).expect("grant"),
        1,
        digest::<ManifestDigest>('a'),
        ActorId::new("actor.code-graph-scope-issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.code-graph-scope-requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.code-graph-scope-{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(format!("cancellation.code-graph-scope-{suffix}"))
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

/// The publication event races the serving swap that seats the generation
/// behind the root-scope ready gate, so poll the exact gate the port uses.
async fn wait_for_ready_root_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> crate::daemon::code_index_scheduler::LatestCompleteCodeIndexV1 {
    let latest = registry
        .latest_complete_fresh(project_root)
        .await
        .expect("fresh serving generation");
    let generation = latest.generation();
    let snapshot = generation.snapshot();
    let scope = ResolvedScope::new(
        generation.manifest().project_id.clone(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ready) = registry
                .latest_complete_ready_decoded_for_root_scope(project_root, &scope)
                .await
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the sealed generation becomes ready and serving for its exact root")
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
