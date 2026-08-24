#![cfg(test)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::{TempDir, tempdir};
use tracedecay_application::{
    ApplicationOperation, AuthorityReceipt, CancellationContext, CancellationSignal,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, EffectTermination, IdempotencyKey,
    PolicyDecisionRef, RequestAdmission, RequestContext, RequestId, ResolvedScope,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditEffectProofV1,
    SourceEditEffectRequestV1, SourceEditRequest, source_edit_operation,
    source_edit_reconciliation_operation, source_edit_rollback_operation,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::{
    ActorId, BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkV1, ComponentVersion, ContentDigest, FileIdentityDigest,
    FileOccurrenceId, LanguageDescriptorRevision, LanguageId, ManifestDigest, PolicyRevisionId,
    ProjectId, RepositoryId, SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_usecases::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};

use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_usecases::edit::{
    SourceEditApplicationResult, SourceEditOutcome, execute_source_edit,
    preview_source_edit_expected_state,
};

pub(super) const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

fn fixture_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.edit.fixture").unwrap(),
        RepositoryId::new("repository.edit.fixture").unwrap(),
        WorktreeId::new("worktree.edit.fixture").unwrap(),
        None,
    )
    .unwrap()
}

#[derive(Clone)]
pub(super) struct FixtureCodeGraphReadPort {
    scope: ResolvedScope,
    store: Option<Arc<CodeGraphProjectionStore>>,
}

impl FixtureCodeGraphReadPort {
    fn unavailable() -> Self {
        Self {
            scope: fixture_scope(),
            store: None,
        }
    }

    fn ready(store: CodeGraphProjectionStore) -> Self {
        Self {
            scope: fixture_scope(),
            store: Some(Arc::new(store)),
        }
    }
}

impl CodeGraphProjectionReadPort for FixtureCodeGraphReadPort {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        Box::pin(async move {
            request
                .context
                .validate()
                .map_err(|error| CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                })?;
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return Err(CodeGraphReadError::Cancelled),
                RequestAdmission::TimedOut => return Err(CodeGraphReadError::TimedOut),
            }
            let store = self
                .store
                .as_ref()
                .map(Arc::clone)
                .ok_or(CodeGraphReadError::MissingRegistry)?;
            VerifiedCodeGraphRead::new(self.scope.clone(), store)
        })
    }
}

fn fixture_digest<T>(domain: &str, value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    let digest = tracedecay_domain::canonical_sha256(&(domain, value)).unwrap();
    T::try_from(digest.as_str().to_owned())
        .unwrap_or_else(|error| panic!("fixture digest must be valid: {error}"))
}

pub(super) fn fixture_symbol_code_graph(
    file_path: &str,
    source: &str,
    symbol_source: &str,
    simple_name: &str,
    qualified_name: &str,
) -> FixtureCodeGraphReadPort {
    let generation = CodeGenerationId::new("generation.source-edit-fixture.1").unwrap();
    let file = FileOccurrenceId::try_from(format!("file:{file_path}")).unwrap();
    let occurrence =
        SymbolOccurrenceId::try_from(format!("occurrence:source-edit:{simple_name}")).unwrap();
    let start = source.find(symbol_source).unwrap();
    let start_byte = u64::try_from(start).unwrap();
    let end_byte = start_byte + u64::try_from(symbol_source.len()).unwrap();
    let start_line = u32::try_from(
        source[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
    )
    .unwrap();
    let line_span = u32::try_from(
        symbol_source
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            .saturating_add(1),
    )
    .unwrap();
    let source_span = SourceSpan {
        start_byte,
        end_byte,
    };
    let content_digest = fixture_digest::<ContentDigest>("source-edit-file", source);
    let files = vec![SanitizedCodeFileV1 {
        file_occurrence_id: file.clone(),
        logical_path: file_path.to_owned(),
        language: Some(LanguageId::new("rust").unwrap()),
        content_digest: content_digest.clone(),
        disposition: SnapshotFileDispositionV1::Present,
    }];
    let symbols = GenerationSymbolIndexV1::new(
        generation.clone(),
        vec![LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: fixture_digest::<SymbolIdentityDigest>("source-edit-symbol", qualified_name),
            qualified_name: qualified_name.to_owned(),
            simple_name: simple_name.to_owned(),
            kind: "function".to_owned(),
            visibility: "pub".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span,
            start_line,
            signature: symbol_source.lines().next().map(str::to_owned),
            skip_test_coverage: false,
            file_identity: fixture_digest::<FileIdentityDigest>(
                "source-edit-file-identity",
                file_path,
            ),
            content_digest: fixture_digest::<ContentDigest>(
                "source-edit-symbol-content",
                symbol_source,
            ),
        }],
    )
    .unwrap();
    let chunks = vec![CodeSearchChunkV1 {
        id: tracedecay_domain::CodeSearchChunkId::new("chunk:source-edit:moved").unwrap(),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: file,
            symbol_occurrence_id: Some(occurrence),
            parent_chunk_id: None,
            source_span,
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal: 0,
        },
        content_digest: fixture_digest::<ContentDigest>("source-edit-chunk", symbol_source),
        language_descriptor_revision: LanguageDescriptorRevision::new("language.fixture.v1")
            .unwrap(),
        chunker_revision: ChunkerRevision::new("chunker.fixture.v1").unwrap(),
        sanitizer_revision: SanitizerRevision::new("sanitizer.fixture.v1").unwrap(),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: PolicyRevisionId::new("policy.fixture.v1").unwrap(),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new(symbol_source).unwrap(),
    }];
    let cancellation = CancellationSignal::active("cancel.source-edit-graph-fixture").unwrap();
    let hermetic = HermeticCodeGraphProjectionStore::memory(&cancellation).unwrap();
    hermetic
        .publish_indexed_with_cancellation(
            &generation,
            &[],
            &chunks,
            &files,
            &symbols,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    FixtureCodeGraphReadPort::ready(hermetic.verified_store(&generation).unwrap())
}

pub(super) async fn fixture_graph(
    project_root: &Path,
) -> (
    TraceDecay,
    FixtureCodeGraphReadPort,
    crate::db::DaemonDatabaseScope,
) {
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
    crate::storage::pin_fixture_repository_identity(project_root, project_id.as_str()).unwrap();
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
    (
        graph,
        FixtureCodeGraphReadPort::unavailable(),
        database_scope,
    )
}

pub(super) fn git(project_root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} must succeed");
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
    let rollback_operation = source_edit_rollback_operation().unwrap();
    let scope = fixture_scope();
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
            rollback_operation.capability_id().clone(),
        ]),
        BTreeSet::from([
            operation.use_case_id().clone(),
            reconciliation_operation.use_case_id().clone(),
            rollback_operation.use_case_id().clone(),
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
    pub(super) code_graph: FixtureCodeGraphReadPort,
    pub(super) _database_scope: crate::db::DaemonDatabaseScope,
    pub(super) request: SourceEditEffectRequestV1,
    pub(super) authorization: FixtureSourceEditAuthorization,
    pub(super) result: SourceEditApplicationResult,
}

const MOVE_SOURCE_PREIMAGE: &[u8] = b"pub fn keep() {}\n\npub fn moved() {}\n";
const MOVE_SOURCE_POSTIMAGE: &[u8] = b"pub fn keep() {}\n";
const MOVE_DESTINATION_PREIMAGE: &[u8] = b"pub fn existing() {}\n";
const MOVE_DESTINATION_POSTIMAGE: &[u8] = b"pub fn existing() {}\n\npub fn moved() {}\n";

impl EffectUnknownFixture {
    fn source_path(&self) -> std::path::PathBuf {
        self.project.path().join("src/locked/a.rs")
    }

    fn destination_path(&self) -> std::path::PathBuf {
        self.project.path().join("src/b.rs")
    }

    pub(super) fn write_partial_postimage(&self) {
        fs::write(self.destination_path(), MOVE_DESTINATION_POSTIMAGE).unwrap();
    }

    pub(super) fn write_all_postimages(&self) {
        fs::write(self.source_path(), MOVE_SOURCE_POSTIMAGE).unwrap();
        fs::write(self.destination_path(), MOVE_DESTINATION_POSTIMAGE).unwrap();
    }

    pub(super) fn assert_preimages(&self) {
        assert_eq!(fs::read(self.source_path()).unwrap(), MOVE_SOURCE_PREIMAGE);
        assert_eq!(
            fs::read(self.destination_path()).unwrap(),
            MOVE_DESTINATION_PREIMAGE
        );
    }

    pub(super) fn assert_postimages(&self) {
        assert_eq!(fs::read(self.source_path()).unwrap(), MOVE_SOURCE_POSTIMAGE);
        assert_eq!(
            fs::read(self.destination_path()).unwrap(),
            MOVE_DESTINATION_POSTIMAGE
        );
    }

    #[cfg(unix)]
    pub(super) fn permission_preserving_path(&self) -> std::path::PathBuf {
        self.destination_path()
    }
}

pub(super) async fn effect_unknown_fixture() -> EffectUnknownFixture {
    let project = tempdir().unwrap();
    let locked_directory = project.path().join("src/locked");
    fs::create_dir_all(&locked_directory).unwrap();
    fs::write(locked_directory.join("a.rs"), MOVE_SOURCE_PREIMAGE).unwrap();
    fs::write(project.path().join("src/b.rs"), MOVE_DESTINATION_PREIMAGE).unwrap();
    fs::write(
        project.path().join("src/caller.rs"),
        "use crate::locked::a::moved;\npub fn caller() { moved(); }\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub mod b;\npub mod caller;\npub mod locked;\n",
    )
    .unwrap();
    fs::write(locked_directory.join("mod.rs"), "pub mod a;\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            project.path().join("src/b.rs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    git(
        project.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    git(project.path(), &["add", "src"]);
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
    let (graph, _, database_scope) = fixture_graph(project.path()).await;
    let code_graph = fixture_symbol_code_graph(
        "src/locked/a.rs",
        std::str::from_utf8(MOVE_SOURCE_PREIMAGE).unwrap(),
        "pub fn moved() {}",
        "moved",
        "locked::a::moved",
    );
    let edit = SourceEditRequest::MoveSymbol {
        symbol: "moved".to_owned(),
        dest_file: "src/b.rs".to_owned(),
        dry_run: false,
        update_references: false,
    };
    let mut request = fixture_request_for_edit(edit, "source-edit.recovery-fixture");
    let expected_state = preview_source_edit_expected_state(
        &graph,
        &code_graph,
        &request.context,
        request.observed_at,
        request.edit.clone(),
    )
    .await
    .unwrap();
    request.expected_state = expected_state;
    let authorization = fixture_authorization(&request);
    let operation = source_edit_operation(request.edit.kind()).unwrap();

    let original_permissions = fs::metadata(&locked_directory).unwrap().permissions();
    let mut read_only_permissions = original_permissions.clone();
    read_only_permissions.set_readonly(true);
    fs::set_permissions(&locked_directory, read_only_permissions).unwrap();
    let execution = execute_source_edit(
        &graph,
        &code_graph,
        &operation,
        request.clone(),
        &authorization,
    )
    .await;
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
    let fixture = EffectUnknownFixture {
        project,
        graph,
        code_graph,
        _database_scope: database_scope,
        request,
        authorization,
        result,
    };
    fixture.assert_preimages();
    fixture
}
