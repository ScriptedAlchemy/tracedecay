use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::json;
use tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore;
use tracedecay_contracts::{
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope, now_micros,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros,
    WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::verified_query_test_support::{
    ImmediateAdmission, ImmediateProjection, fixture_scope, fixture_store, graph_operation,
};
use super::{
    CodeGraphSourceAuthorityPort, CodeGraphSourceBindFuture, CodeGraphSourceBindRequest,
    VerifiedGraphQuery, VerifiedGraphQueryRequest, open_verified_graph_query,
};
use crate::SourceReadContext;
use crate::context::read_modes::ReadMode;
use crate::context::source_read::SourceReadRequest;
use tracedecay_session_memory::context::read_cache::{self, GLOBAL_SESSION};

/// Same-identity, same-root provider that swaps its database answer after the
/// flip. The source authority must bind once and freeze that answer.
struct SwitchingSourceBind {
    project_root: PathBuf,
    project_id: String,
    bound_db: Database,
    forged_db: Database,
    forged: AtomicBool,
    bound_hits: AtomicUsize,
    forged_hits: AtomicUsize,
}

impl CodeGraphSourceAuthorityPort for SwitchingSourceBind {
    fn bind<'a>(
        &'a self,
        _request: CodeGraphSourceBindRequest<'a>,
    ) -> CodeGraphSourceBindFuture<'a> {
        if self.forged.load(Ordering::SeqCst) {
            self.forged_hits.fetch_add(1, Ordering::SeqCst);
            let source = SourceReadContext::new(
                self.project_root.clone(),
                self.forged_db.clone(),
                true,
                self.project_id.clone(),
            );
            Box::pin(async move { Ok(source) })
        } else {
            self.bound_hits.fetch_add(1, Ordering::SeqCst);
            let source = SourceReadContext::new(
                self.project_root.clone(),
                self.bound_db.clone(),
                true,
                self.project_id.clone(),
            );
            Box::pin(async move { Ok(source) })
        }
    }
}

struct FixtureSourceBind {
    source: SourceReadContext,
}

impl CodeGraphSourceAuthorityPort for FixtureSourceBind {
    fn bind<'a>(
        &'a self,
        _request: CodeGraphSourceBindRequest<'a>,
    ) -> CodeGraphSourceBindFuture<'a> {
        let source = self.source.clone();
        Box::pin(async move { Ok(source) })
    }
}

async fn test_database(path: &Path) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "verified query source forge")
        .expect("database authority");
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
        .await
        .expect("database")
        .0
}

fn fixture_context(project_id: &str, cancellation: &CancellationSignal) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new(project_id).expect("project"),
        RepositoryId::new("repository.verified-query-source").expect("repository"),
        WorktreeId::new("worktree.verified-query-source").expect("worktree"),
        Some(RefId::new("refs/heads/verified-query-source").expect("reference")),
    )
    .expect("scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.verified-query-source").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        ActorId::new("actor.verified-query-source.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from(
            [CapabilityId::new("capability.verified-query-source").expect("capability")],
        ),
        BTreeSet::from([UseCaseId::new("use-case.verified-query-source").expect("use case")]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.verified-query-source.requester").expect("requester"),
        scope,
        grant,
        RequestId::new("request.verified-query-source").expect("request"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        cancellation.context(),
    )
    .expect("context")
}

fn fixture_query(project_id: &str) -> VerifiedGraphQuery {
    let cancellation =
        CancellationSignal::active("cancel.verified-query-source").expect("cancellation");
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation).expect("projection");
    let generation =
        CodeGenerationId::new("generation.verified-query-source.1").expect("generation");
    projection
        .publish_with_cancellation(&generation, &[], &[], Arc::new(NeverCancelled))
        .expect("publish");
    let store = projection.verified_store(&generation).expect("store");
    let graph_cancellation = super::application_graph_cancellation(&cancellation);
    let reader = store
        .interactive_reader_with_cancellation(&generation, Arc::clone(&graph_cancellation))
        .expect("reader");
    VerifiedGraphQuery::from_fixture_reader(
        reader,
        graph_cancellation,
        fixture_context(project_id, &cancellation),
    )
}

fn assert_denied(error: tracedecay_domain::errors::TraceDecayError) {
    let (code, retryable, _) = error
        .project_route_context()
        .expect("typed denied source route");
    assert_eq!(code, "code-graph-denied");
    assert!(!retryable);
}

fn full_read_request(project_id: &str) -> SourceReadRequest<'_> {
    SourceReadRequest {
        file: "src/lib.rs",
        mode: ReadMode::Full,
        line_range: None,
        raw_lines: None,
        include_symbols: false,
        project_id,
    }
}

#[test]
fn unbound_query_refuses_source_reads() {
    let query = fixture_query("project.verified-query-source.a");
    let error = query
        .resolve_indexed_source_file("src/lib.rs")
        .expect_err("unbound source must fail closed");
    assert_denied(error);
}

#[tokio::test]
async fn resolve_rejects_absolute_path_under_another_project_root() {
    let home = tempfile::tempdir().expect("temp");
    let project_a = home.path().join("project-a");
    let project_b = home.path().join("project-b");
    std::fs::create_dir_all(project_a.join("src")).expect("project a");
    std::fs::create_dir_all(project_b.join("src")).expect("project b");
    std::fs::write(project_b.join("src/secret.rs"), "fn secret() {}\n").expect("foreign file");
    let db = test_database(&project_a.join("bound.db")).await;
    let query =
        fixture_query("project.verified-query-source.a").with_source(SourceReadContext::new(
            project_a,
            db,
            true,
            "project.verified-query-source.a".to_owned(),
        ));
    let error = query
        .resolve_indexed_source_file(project_b.join("src/secret.rs").to_str().expect("utf8"))
        .expect_err("foreign root must be denied");
    assert!(
        error.to_string().contains("escapes project root")
            || error
                .project_route_context()
                .is_some_and(|(code, _, _)| code == "code-graph-denied"),
        "foreign root must fail closed, got {error}"
    );
}

#[tokio::test]
async fn read_source_rejects_request_project_id_outside_bound_source() {
    let home = tempfile::tempdir().expect("temp");
    let project_a = home.path().join("project-a");
    std::fs::create_dir_all(&project_a).expect("project a");
    let db = test_database(&project_a.join("bound.db")).await;
    let query =
        fixture_query("project.verified-query-source.a").with_source(SourceReadContext::new(
            project_a,
            db,
            true,
            "project.verified-query-source.a".to_owned(),
        ));
    let error = match query
        .read_source(full_read_request("project.verified-query-source.b"))
        .await
    {
        Ok(_) => panic!("foreign request project id must be denied"),
        Err(error) => error,
    };
    assert_denied(error);
}

#[tokio::test]
async fn open_denies_cross_project_source_at_bind() {
    let home = tempfile::tempdir().expect("temp");
    let admission = ImmediateAdmission {
        scope: fixture_scope("verified-query-source-deny"),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope("verified-query-source-deny"),
        store: fixture_store("verified-query-source-deny"),
    };
    let db = test_database(&home.path().join("bound.db")).await;
    let bind = FixtureSourceBind {
        source: SourceReadContext::new(
            home.path().to_path_buf(),
            db,
            true,
            "project.verified-query-source-other".to_owned(),
        ),
    };
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-source.bind-deny").expect("signal");
    let operation = graph_operation();
    let error = match open_verified_graph_query(
        &admission,
        &projection,
        VerifiedGraphQueryRequest::new(
            &operation,
            RequestId::new("request.verified-query-source.bind-deny").expect("request"),
            deadline,
            &cancellation,
        ),
        Some(&bind),
    )
    .await
    {
        Ok(_) => panic!("cross-project source bind must be denied"),
        Err(error) => error,
    };
    assert_denied(error);
}

#[tokio::test]
async fn forged_runtime_cannot_redirect_reads_after_admitted_open() {
    let home = tempfile::tempdir().expect("temp");
    let project = home.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project");
    let source_file = project.join("src/lib.rs");
    std::fs::write(&source_file, "fn bound() {}\n").expect("file");
    let project_id = "project.verified-query-source-swap";
    let bound_db = test_database(&project.join("bound.db")).await;
    let forged_db = test_database(&project.join("forged.db")).await;
    let mtime_ns = read_cache::file_mtime_ns(&source_file).expect("mtime");
    let args_hash = read_cache::args_hash(&json!({
        "lines": serde_json::Value::Null,
        "last_sync_at": serde_json::Value::Null,
    }))
    .expect("args hash");
    read_cache::put(
        &forged_db,
        project_id,
        GLOBAL_SESSION,
        "src/lib.rs",
        mtime_ns,
        "full",
        &args_hash,
        "forged-cache-digest",
        b"forged-body",
        1,
    )
    .await
    .expect("poison forged cache");
    let source = Arc::new(SwitchingSourceBind {
        project_root: project,
        project_id: project_id.to_owned(),
        bound_db,
        forged_db,
        forged: AtomicBool::new(false),
        bound_hits: AtomicUsize::new(0),
        forged_hits: AtomicUsize::new(0),
    });
    let admission = ImmediateAdmission {
        scope: fixture_scope("verified-query-source-swap"),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope("verified-query-source-swap"),
        store: fixture_store("verified-query-source-swap"),
    };
    let deadline =
        Deadline::new(UtcMicros(now_micros().0.saturating_add(60_000_000))).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-source.swap").expect("signal");
    let operation = graph_operation();
    let query = open_verified_graph_query(
        &admission,
        &projection,
        VerifiedGraphQueryRequest::new(
            &operation,
            RequestId::new("request.verified-query-source.swap").expect("request"),
            deadline,
            &cancellation,
        ),
        Some(source.as_ref()),
    )
    .await
    .expect("admitted open with bound source");
    assert_eq!(
        source.bound_hits.load(Ordering::SeqCst),
        1,
        "the database authority is captured exactly once at admitted open"
    );
    // Flip the provider after admission: a second bind would now answer with
    // the forged database, but the frozen authority must never consult it.
    source.forged.store(true, Ordering::SeqCst);
    let output = query
        .read_source(full_read_request(project_id))
        .await
        .expect("bound source read");
    assert_eq!(
        source.forged_hits.load(Ordering::SeqCst),
        0,
        "same-id/same-root forged provider must not be rebound"
    );
    assert_eq!(
        source.bound_hits.load(Ordering::SeqCst),
        1,
        "reads use the frozen authority without rebinding the provider"
    );
    assert_ne!(output.digest, "forged-cache-digest");
    assert!(!output.unchanged);
    assert_eq!(output.body.as_deref(), Some("fn bound() {}\n"));
}
