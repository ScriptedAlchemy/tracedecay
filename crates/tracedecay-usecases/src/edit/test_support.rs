#![cfg(test)]

use std::collections::BTreeSet;
use std::fs;
use std::ops::Deref;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::{TempDir, tempdir};
use tracedecay_application::{
    ApiCompatibilityDispositionV1, ApiCompatibilityLifetimeV1, ApiDefinitionInsertionV1,
    ApiMigrationApplyResultV1, ApiMigrationOperationRequestV1, ApiMigrationPlanRequestV1,
    ApiMigrationPlanV1, ApiMigrationSiteDispositionV1, ApiMigrationSymbolV1, ApplicationOperation,
    AuthorityReceipt, CancellationContext, CancellationSignal, CancellationStage,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, IdempotencyKey, PolicyDecisionRef,
    RequestContext, RequestId, ResolvedScope, SourceEditAuthorizationFuture,
    SourceEditAuthorizationPort, SourceEditEffectProofV1, SourceEditEffectRequestV1,
    SourceEditKind, SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1,
    SourceEditRequest, api_migration_definition_digest, source_edit_operation,
    source_edit_reconciliation_operation,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};

use super::JOURNAL_VERSION;
use super::digest::effect_id;
use super::dispatch::run_source_edit;
use super::journal::{SourceEditDurableRequestV1, SourceEditJournalStateV1, SourceEditJournalV1};
use super::outcome::SourceEditOutcome;
use crate::tracedecay::TraceDecay;
use crate::tracedecay::TraceDecayOpenOptions;

pub(super) const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

pub(super) struct FixtureGraph {
    graph: TraceDecay,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

impl Deref for FixtureGraph {
    type Target = TraceDecay;

    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

pub(super) async fn fixture_graph(project_root: &Path) -> FixtureGraph {
    let profile_root = project_root.join(".tracedecay-test-profile");
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root).unwrap();
    let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "source-edit-test-runtime",
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
    tracedecay_runtime_core::storage::write_enrollment_marker(
        project_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
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
    FixtureGraph {
        graph: TraceDecay::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
        .unwrap(),
        _database_scope: database_scope,
    }
}

pub(super) fn git(project_root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} must succeed");
}

pub(super) async fn indexed_api_migration_fixture(initial_source: &str) -> (TempDir, FixtureGraph) {
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
    let graph = fixture_graph(project.path()).await;
    let indexed = graph.index_all().await.unwrap();
    assert!(indexed.node_count > 0);
    (project, graph)
}

pub(super) async fn api_migration_symbol(graph: &TraceDecay, name: &str) -> ApiMigrationSymbolV1 {
    let node = graph
        .get_nodes_by_name(name)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.file_path == "src/lib.rs")
        .unwrap_or_else(|| panic!("indexed fixture symbol {name}"));
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
    crate::api_migration::plan_api_migration(
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
    let plan_digest = plan.plan_digest.clone();
    let outcome = run_source_edit(
        graph,
        SourceEditRequest::ApiMigrationApply {
            plan,
            plan_digest,
            dry_run: false,
            verify: false,
        },
        None,
    )
    .await
    .unwrap();
    match outcome {
        SourceEditOutcome::ApiMigration(result) => result,
        unexpected => panic!("unexpected API migration outcome: {unexpected:?}"),
    }
}

pub(super) fn fixture_request() -> SourceEditEffectRequestV1 {
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
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
        RequestId::new("request.edit.fixture").unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active("cancel.edit.fixture").unwrap(),
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
        edit: SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: "old".to_owned(),
            new_str: "new".to_owned(),
            dry_run: false,
            verify: false,
        },
        idempotency_key: IdempotencyKey::new("source-edit.fixture").unwrap(),
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

pub(super) fn fixture_journal(
    request: &SourceEditEffectRequestV1,
    state: SourceEditJournalStateV1,
) -> SourceEditJournalV1 {
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let input_digest = request.input_digest().unwrap();
    SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id: effect_id(&request.idempotency_key, &input_digest).unwrap(),
        input_digest,
        expected_state: request.expected_state.clone(),
        predicted_state: None,
        candidate_files: vec!["src/lib.rs".to_owned()],
        recovery_files: Vec::new(),
        recovery_digest: None,
        request: SourceEditDurableRequestV1 {
            operation: operation.use_case_id().clone(),
            request_id: request.context.request_id().clone(),
            actor: request.context.actor().clone(),
            scope: request.context.scope().clone(),
            authority: request.authority.clone(),
            authority_proof: request.proof.clone(),
            idempotency_key: request.idempotency_key.clone(),
            deadline: request.context.deadline().clone(),
            started_at: request.observed_at,
            dry_run: request.edit.dry_run(),
            verification_requested: request.edit.verify(),
        },
        state,
    }
}

pub(super) fn fixture_reconciliation(
    request: &SourceEditEffectRequestV1,
    journal: &SourceEditJournalV1,
    disposition: SourceEditReconciliationDispositionV1,
) -> SourceEditReconciliationRequestV1 {
    SourceEditReconciliationRequestV1 {
        context: request.context.clone(),
        authority: request.authority.clone(),
        kind: request.edit.kind(),
        effect_id: journal.effect_id.clone(),
        idempotency_key: request.idempotency_key.clone(),
        attempt_idempotency_key: tracedecay_application::IdempotencyKey::new(
            "source-edit-reconciliation-attempt.fixture",
        )
        .unwrap(),
        input_digest: journal.input_digest.clone(),
        disposition,
        proof: request.proof.clone(),
        observed_at: UtcMicros(4),
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
