use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_application::retrieval::{
    RetrievalOrder, RetrievalRequestMeta, SourceReadModeV1, SourceReadPortContext,
    SourceReadPortOutcome, SourceReadPrimitivePort, SourceReadPrimitiveRequest,
};
use tracedecay_application::{
    ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, PageRequest, RequestContext, RequestId, ResolvedScope,
    ResultContractRef, ResultProjection,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

use super::{TraceDecay, TraceDecayOpenOptions};
use crate::application::primitives::Pr12SourceReadAdapter;

const NOW: UtcMicros = UtcMicros(1_000);

#[tokio::test]
async fn source_reads_reuse_the_cross_session_cache() {
    let root = TempDir::new().expect("temporary project");
    fs::create_dir_all(root.path().join("src")).expect("source directory");
    fs::write(
        root.path().join("src/lib.rs"),
        "pub fn first() {}\npub fn second() {}\n",
    )
    .expect("fixture source");
    let profile_root = root.path().join(".tracedecay-test-profile");
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "source-read cache test",
    )
    .expect("exclusive lifecycle authority");
    let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "source-read cache test",
    )
    .expect("maintenance database authority");
    let graph = Arc::new(
        TraceDecay::init_with_exclusive_maintenance(root.path(), open_options, &lifecycle)
            .await
            .expect("initialize graph"),
    );
    let (scope, context, operation) = application_context("source-read");
    let adapter = Pr12SourceReadAdapter::new(graph, scope).expect("source adapter");
    let request = SourceReadPrimitiveRequest {
        file: "src/lib.rs".to_owned(),
        mode: SourceReadModeV1::Lines,
        lines: Some("2-2".to_owned()),
        include_symbols: false,
        meta: RetrievalRequestMeta::current(
            PageRequest::first(1).expect("page"),
            ResultProjection::Evidence,
            RetrievalOrder::SourcePosition,
        ),
    };

    let first = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &request,
        )
        .await;
    let second = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &request,
        )
        .await;

    let SourceReadPortOutcome::Completed { result: first, .. } = first else {
        panic!("first source read must complete");
    };
    let SourceReadPortOutcome::Completed { result: second, .. } = second else {
        panic!("cached source read must complete");
    };
    assert_eq!(first.body.as_deref(), Some("pub fn second() {}"));
    assert!(!first.unchanged);
    assert!(second.unchanged);
    assert!(second.body.is_none());
    assert_eq!(second.digest, first.digest);

    let invalid = SourceReadPrimitiveRequest {
        mode: SourceReadModeV1::Full,
        lines: Some("1-1".to_owned()),
        ..request
    };
    let outcome = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &invalid,
        )
        .await;
    assert!(
        matches!(outcome, SourceReadPortOutcome::Failed { .. }),
        "production source reads must reject mode/range mismatches"
    );
}

fn application_context(suffix: &str) -> (ResolvedScope, RequestContext, ApplicationOperation) {
    let scope = ResolvedScope::new(
        ProjectId::new("project.pr12").expect("project"),
        RepositoryId::new("repository.pr12").expect("repository"),
        WorktreeId::new("worktree.pr12").expect("worktree"),
        Some(RefId::new("refs/heads/pr12").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new(format!("capability.pr12.{suffix}")).expect("capability");
    let use_case = UseCaseId::new(format!("use-case.pr12.{suffix}")).expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.pr12.{suffix}")).expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.pr12.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability.clone()]),
        BTreeSet::from([use_case.clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.pr12.requester").expect("actor"),
        scope.clone(),
        grant,
        RequestId::new(format!("request.pr12.{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(10_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.pr12.{suffix}")).expect("cancellation"),
    )
    .expect("request context");
    let operation = ApplicationOperation::new(
        capability,
        use_case,
        ResultContractRef::new(
            SchemaId::new(format!("schema.pr12.{suffix}")).expect("schema"),
            1,
        )
        .expect("result contract"),
        true,
    );
    (scope, context, operation)
}
