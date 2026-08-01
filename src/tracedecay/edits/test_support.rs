#![cfg(test)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::{TempDir, tempdir};
use tracedecay_application::{
    ApiMigrationApplyResultV1, ApiMigrationOperationRequestV1, ApiMigrationPlanRequestV1,
    ApiMigrationPlanV1, ApiMigrationSymbolV1, ApplicationOperation, AuthorityReceipt,
    CancellationContext, CancellationSignal, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    EffectTermination, IdempotencyKey, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditEffectProofV1,
    SourceEditEffectRequestV1, SourceEditRequest, api_migration_definition_digest,
    source_edit_operation, source_edit_reconciliation_operation,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};

use crate::application::edit::{
    SourceEditApplicationResult, SourceEditOutcome, execute_source_edit,
    preview_source_edit_expected_state,
};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

pub(super) const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

pub(super) async fn fixture_graph(
    project_root: &Path,
) -> (TraceDecay, crate::db::DaemonDatabaseScope) {
    let profile_root = project_root.join(".tracedecay-test-profile");
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root).unwrap();
    let database_scope = crate::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "source-edit-owner-test-runtime",
    )
    .unwrap();
    let runtime_registry = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .unwrap(),
    );
    let profile_database = runtime_registry.profile_database().await.unwrap();
    let store_layout = TraceDecay::resolve_first_touch_configuration_layout(
        project_root,
        &open_options,
        profile_database.as_ref(),
        true,
    )
    .await
    .unwrap();
    let project_id = ProjectId::new(
        store_layout
            .identity
            .project_id
            .clone()
            .expect("fixture layout has a project identity"),
    )
    .unwrap();
    crate::storage::write_enrollment_marker(
        project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let configuration_database = runtime_registry
        .project_sessions(
            project_id,
            [
                project_root.to_path_buf(),
                store_layout.project_root.clone(),
            ],
        )
        .await
        .unwrap();
    let graph = TraceDecay::init_with_registered_configuration(
        project_root,
        open_options,
        store_layout,
        configuration_database,
        profile_database,
        runtime_registry,
    )
    .await
    .unwrap();
    (graph, database_scope)
}

pub(super) fn git(project_root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} must succeed");
}

pub(super) async fn indexed_api_migration_fixture(
    initial_source: &str,
) -> (TempDir, TraceDecay, crate::db::DaemonDatabaseScope) {
    let project = tempdir().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/lib.rs"), initial_source).unwrap();
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
    let (graph, database_scope) = fixture_graph(project.path()).await;
    let indexed = graph.index_all().await.unwrap();
    assert!(indexed.node_count > 0);
    (project, graph, database_scope)
}

pub(super) async fn api_migration_symbol(graph: &TraceDecay, name: &str) -> ApiMigrationSymbolV1 {
    api_migration_symbol_in(graph, name, "src/lib.rs").await
}

pub(super) async fn api_migration_symbol_in(
    graph: &TraceDecay,
    name: &str,
    file: &str,
) -> ApiMigrationSymbolV1 {
    let node = graph
        .get_nodes_by_name(name)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.file_path == file)
        .unwrap_or_else(|| panic!("indexed fixture symbol {name} in {file}"));
    ApiMigrationSymbolV1 {
        node_id: node.id,
        qualified_name: node.qualified_name,
        kind: node.kind.as_str().to_owned(),
        file: node.file_path,
        old_name: node.name,
    }
}

pub(super) async fn plan_api_migration_fixture(
    graph: &TraceDecay,
    family_id: &str,
    operation: ApiMigrationOperationRequestV1,
) -> ApiMigrationPlanV1 {
    crate::application::api_migration::plan_api_migration(
        graph,
        ApiMigrationPlanRequestV1 {
            family_id: family_id.to_owned(),
            operations: vec![operation],
        },
    )
    .await
    .unwrap()
}

pub(super) async fn apply_api_migration_fixture(
    graph: &TraceDecay,
    plan: ApiMigrationPlanV1,
) -> ApiMigrationApplyResultV1 {
    let edit = SourceEditRequest::ApiMigrationApply {
        plan: plan.clone(),
        plan_digest: plan.plan_digest.clone(),
        dry_run: false,
        verify: false,
    };
    let expected_state = preview_source_edit_expected_state(graph, edit.clone())
        .await
        .unwrap();
    let mut request = fixture_request_for_edit(edit, "source-edit.api-migration-fixture");
    request.expected_state = expected_state;
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let authorization = fixture_authorization(&request);
    let result = execute_source_edit(graph, &operation, request, &authorization)
        .await
        .unwrap();
    match result.outcome {
        SourceEditOutcome::ApiMigration(result) => result,
        unexpected => panic!("unexpected API migration outcome: {unexpected:?}"),
    }
}

pub(super) fn fixture_request() -> SourceEditEffectRequestV1 {
    fixture_request_for_edit(
        SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: "old".to_owned(),
            new_str: "new".to_owned(),
            dry_run: false,
            verify: false,
        },
        "source-edit.fixture",
    )
}

pub(super) fn fixture_request_for_edit(
    edit: SourceEditRequest,
    idempotency_key: &str,
) -> SourceEditEffectRequestV1 {
    let operation = source_edit_operation(edit.kind()).unwrap();
    let reconciliation_operation = source_edit_reconciliation_operation().unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.edit.fixture").unwrap(),
        RepositoryId::new("repository.edit.fixture").unwrap(),
        WorktreeId::new("worktree.edit.fixture").unwrap(),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new("grant.edit.fixture").unwrap(),
        1,
        digest(SHA256_A),
        ActorId::new("actor.edit.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([
            operation.capability_id().clone(),
            reconciliation_operation.capability_id().clone(),
        ]),
        BTreeSet::from([
            operation.use_case_id().clone(),
            reconciliation_operation.use_case_id().clone(),
        ]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let context = RequestContext::new(
        ActorId::new("actor.edit.requester").unwrap(),
        scope,
        grant,
        RequestId::new(format!("request.{idempotency_key}")).unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active(format!("cancel.{idempotency_key}")).unwrap(),
    )
    .unwrap();
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.edit.fixture",
            1,
            digest(SHA256_B),
            ComponentVersion::new("policy.edit.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap();
    SourceEditEffectRequestV1 {
        context,
        authority,
        edit,
        idempotency_key: IdempotencyKey::new(idempotency_key).unwrap(),
        expected_state: digest(SHA256_A),
        proof: SourceEditEffectProofV1 {
            policy_digest: digest(SHA256_B),
            configuration_revision_id:
                tracedecay_domain::configuration::ConfigurationRevisionId::new(
                    "configuration.edit.fixture.v1",
                )
                .unwrap(),
            configuration_digest: digest(SHA256_A),
            catalog_revision: 1,
            catalog_digest: digest(SHA256_A),
            privacy_domain_id: tracedecay_domain::PrivacyDomainId::new("privacy.edit.fixture")
                .unwrap(),
            privacy_key_epoch: 1,
            privacy_digest: digest(SHA256_A),
            external_proof: None,
        },
        observed_at: UtcMicros(3),
    }
}

#[derive(Clone)]
pub(super) struct FixtureSourceEditAuthorization(
    pub(super) tracedecay_application::SourceEditAuthorizationAdmissionV1,
);

pub(super) fn fixture_authorization(
    request: &SourceEditEffectRequestV1,
) -> FixtureSourceEditAuthorization {
    FixtureSourceEditAuthorization(
        tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
            request.authority.clone(),
            request.proof.clone(),
            request.context.scope(),
        )
        .unwrap(),
    )
}

impl SourceEditAuthorizationPort for FixtureSourceEditAuthorization {
    fn admit<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }

    fn recheck_effect<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

pub(super) struct CancelBeforeEffectAuthorization {
    pub(super) admission: tracedecay_application::SourceEditAuthorizationAdmissionV1,
    pub(super) cancellation: CancellationSignal,
    pub(super) rechecks: AtomicUsize,
}

impl SourceEditAuthorizationPort for CancelBeforeEffectAuthorization {
    fn admit<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.admission.clone()) })
    }

    fn recheck_effect<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            if self.rechecks.fetch_add(1, Ordering::AcqRel) == 1 {
                assert!(self.cancellation.cancel(UtcMicros(4)));
            }
            Ok(self.admission.clone())
        })
    }
}

pub(super) struct EffectUnknownFixture {
    pub(super) project: TempDir,
    pub(super) graph: TraceDecay,
    pub(super) _database_scope: crate::db::DaemonDatabaseScope,
    pub(super) request: SourceEditEffectRequestV1,
    pub(super) authorization: FixtureSourceEditAuthorization,
    pub(super) result: SourceEditApplicationResult,
}

pub(super) async fn effect_unknown_fixture() -> EffectUnknownFixture {
    const INITIAL_A: &str = "pub fn old_a() {}\n";
    const INTENDED_A: &str = "pub fn new_a() {}\n";
    const INITIAL_B: &str = "pub fn old_b() {}\n";
    const INTENDED_B: &str = "pub fn new_b() {}\n";

    let project = tempdir().unwrap();
    let locked_directory = project.path().join("src/locked");
    fs::create_dir_all(&locked_directory).unwrap();
    fs::write(project.path().join("src/a.rs"), INITIAL_A).unwrap();
    fs::write(locked_directory.join("b.rs"), INITIAL_B).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            project.path().join("src/a.rs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    git(
        project.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(project.path(), &["add", "src/a.rs", "src/locked/b.rs"]);
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
    let (graph, database_scope) = fixture_graph(project.path()).await;
    let indexed = graph.index_all().await.unwrap();
    assert!(indexed.node_count >= 2);
    let plan = crate::application::api_migration::plan_api_migration(
        &graph,
        ApiMigrationPlanRequestV1 {
            family_id: "family.recovery".to_owned(),
            operations: vec![
                ApiMigrationOperationRequestV1::ReplaceDefinition {
                    operation_id: "replace-a".to_owned(),
                    depends_on: Vec::new(),
                    symbol: api_migration_symbol_in(&graph, "old_a", "src/a.rs").await,
                    expected_definition_digest: api_migration_definition_digest(INITIAL_A).unwrap(),
                    replacement_definition: INTENDED_A.to_owned(),
                },
                ApiMigrationOperationRequestV1::ReplaceDefinition {
                    operation_id: "replace-b".to_owned(),
                    depends_on: Vec::new(),
                    symbol: api_migration_symbol_in(&graph, "old_b", "src/locked/b.rs").await,
                    expected_definition_digest: api_migration_definition_digest(INITIAL_B).unwrap(),
                    replacement_definition: INTENDED_B.to_owned(),
                },
            ],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        plan.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src/a.rs", "src/locked/b.rs"]
    );
    let edit = SourceEditRequest::ApiMigrationApply {
        plan: plan.clone(),
        plan_digest: plan.plan_digest.clone(),
        dry_run: false,
        verify: false,
    };
    let expected_state = preview_source_edit_expected_state(&graph, edit.clone())
        .await
        .unwrap();
    let mut request = fixture_request_for_edit(edit, "source-edit.recovery-fixture");
    request.expected_state = expected_state;
    let authorization = fixture_authorization(&request);
    let operation = source_edit_operation(request.edit.kind()).unwrap();

    let original_permissions = fs::metadata(&locked_directory).unwrap().permissions();
    let mut read_only_permissions = original_permissions.clone();
    read_only_permissions.set_readonly(true);
    fs::set_permissions(&locked_directory, read_only_permissions).unwrap();
    let execution = execute_source_edit(&graph, &operation, request.clone(), &authorization).await;
    fs::set_permissions(&locked_directory, original_permissions).unwrap();
    let result = execution.unwrap();

    assert!(matches!(
        result.outcome,
        SourceEditOutcome::EffectUnknown { .. }
    ));
    assert_eq!(
        result.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::EffectUnknown
    );
    assert_eq!(
        fs::read_to_string(project.path().join("src/a.rs")).unwrap(),
        INITIAL_A
    );
    assert_eq!(
        fs::read_to_string(project.path().join("src/locked/b.rs")).unwrap(),
        INITIAL_B
    );

    EffectUnknownFixture {
        project,
        graph,
        _database_scope: database_scope,
        request,
        authorization,
        result,
    }
}
