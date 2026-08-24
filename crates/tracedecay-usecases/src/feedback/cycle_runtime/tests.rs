use super::*;
use std::path::Path;

use tracedecay_application::CancellationSignal;
use tracedecay_application::feedback::FeedbackBudgetUsage;
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::feedback::{
    FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleResultV1, FeedbackCycleTerminationV1, FeedbackDiagnosticClassificationV1,
    FeedbackFindingLifecycleV1, FeedbackScopeV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
    CommitId, ContentDigest, EdgeAuthorityV1, FileIdentityDigest, FileOccurrenceId, HostInstanceId,
    LanguageDescriptorRevision, LanguageId, ManifestDigest, PolicyRevisionId, ProjectId,
    RelationEdgeKindV1, RepositoryId, SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SessionId, SnapshotFileDispositionV1, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;

const SHA_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).expect("digest")
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.canonical-feedback").unwrap(),
        repository_id: RepositoryId::new("repository.canonical-feedback").unwrap(),
        worktree_id: WorktreeId::new("worktree.canonical-feedback").unwrap(),
        branch_ref: "refs/heads/canonical-feedback".to_owned(),
        head_commit_id: CommitId::new("commit.canonical-feedback").unwrap(),
    }
}

fn request(content: FeedbackContentIdentityV1) -> FeedbackCycleRequestV1 {
    FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.canonical-feedback").unwrap(),
        scope(),
        content,
        FeedbackTriggerV1::ExplicitDiagnostics,
        digest(SHA_A),
        digest(SHA_B),
        FeedbackBudgetV1::bounded(100, 100, 1_024, 100),
    )
    .unwrap()
}

fn execution(cycle: FeedbackCycleResultV1) -> FeedbackCycleExecutionResult {
    FeedbackCycleExecutionResult {
        cycle,
        dedupe_key: None,
        authority: None,
        usage: FeedbackBudgetUsage {
            completed_at: UtcMicros(10),
            tokens_consumed: 0,
            cost_microunits: 0,
        },
        publication: None,
    }
}

#[tokio::test]
async fn daemon_impact_adapter_reports_missing_identity_without_minting_paths() {
    assert!(matches!(
        resolve_affected_files_for_published_generation(
            None,
            Path::new("/project"),
            &CodeGenerationId::new("generation.canonical-feedback").unwrap(),
            &["src/lib.rs".to_owned()],
        )
        .await,
        ResolvedAffectedFiles::IdentityUnavailable
    ));
}

#[test]
fn uses_only_predecessor_contributes_a_file_but_not_an_affected_caller() {
    let reader = impact_reader();
    let target = SymbolOccurrenceId::new("symbol.feedback.target").unwrap();

    let evidence = read_verified_impact_evidence_v1(&reader, &target, Arc::new(NeverCancelled))
        .expect("verified impact")
        .expect("target symbol");

    assert!(evidence.file_paths.contains(&"src/uses.rs".to_owned()));
    assert_eq!(
        evidence.affected_callers,
        vec![SymbolOccurrenceId::new("symbol.feedback.caller").unwrap()]
    );
    assert!(evidence.complete);
}

fn impact_reader() -> CodeGraphInteractiveReader {
    let generation = CodeGenerationId::new("generation.feedback.impact").unwrap();
    let symbols = [
        ("symbol.feedback.target", "src/target.rs", '1'),
        ("symbol.feedback.uses", "src/uses.rs", '2'),
        ("symbol.feedback.caller", "src/caller.rs", '3'),
    ];
    let files = symbols
        .iter()
        .map(|(symbol, path, digest)| SanitizedCodeFileV1 {
            file_occurrence_id: FileOccurrenceId::new(format!("file.{symbol}")).unwrap(),
            logical_path: (*path).to_owned(),
            language: Some(LanguageId::new("rust").unwrap()),
            content_digest: digest_value::<ContentDigest>(*digest),
            disposition: SnapshotFileDispositionV1::Present,
        })
        .collect::<Vec<_>>();
    let records = symbols
        .iter()
        .map(|(symbol, _, digest)| LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(*symbol).unwrap(),
            identity: digest_value::<SymbolIdentityDigest>(*digest),
            qualified_name: (*symbol).to_owned(),
            simple_name: symbol.rsplit('.').next().unwrap().to_owned(),
            kind: "function".to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span: 1,
            start_line: 1,
            signature: None,
            skip_test_coverage: false,
            file_identity: FileIdentityDigest::new(format!(
                "sha256:{}",
                digest.to_string().repeat(64)
            ))
            .unwrap(),
            content_digest: ContentDigest::new(format!("sha256:{}", digest.to_string().repeat(64)))
                .unwrap(),
        })
        .collect::<Vec<_>>();
    let chunks = symbols
        .iter()
        .enumerate()
        .map(|(ordinal, (symbol, _, digest))| CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("chunk.{symbol}")).unwrap(),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: FileOccurrenceId::new(format!("file.{symbol}")).unwrap(),
                symbol_occurrence_id: Some(SymbolOccurrenceId::new(*symbol).unwrap()),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: u32::try_from(ordinal).unwrap(),
            },
            content_digest: digest_value::<ContentDigest>(*digest),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.feedback-impact.v1",
            )
            .unwrap(),
            chunker_revision: ChunkerRevision::new("chunker.feedback-impact.v1").unwrap(),
            sanitizer_revision: SanitizerRevision::new("sanitizer.feedback-impact.v1").unwrap(),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.feedback-impact.v1").unwrap(),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new("fn fixture() {}").unwrap(),
        })
        .collect::<Vec<_>>();
    let target = SymbolOccurrenceId::new("symbol.feedback.target").unwrap();
    let edges = vec![
        relation("symbol.feedback.uses", &target, RelationEdgeKindV1::Uses),
        relation("symbol.feedback.caller", &target, RelationEdgeKindV1::Calls),
    ];
    let index = GenerationSymbolIndexV1::new(generation.clone(), records).unwrap();
    let cancellation = CancellationSignal::active("cancel.feedback-impact-fixture").unwrap();
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation).unwrap();
    projection
        .publish_indexed_with_cancellation(
            &generation,
            &edges,
            &chunks,
            &files,
            &index,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    projection
        .verified_store(&generation)
        .unwrap()
        .interactive_reader(&generation, &cancellation)
        .unwrap()
}

fn relation(
    from: &str,
    target: &SymbolOccurrenceId,
    kind: RelationEdgeKindV1,
) -> CanonicalRelationEdgeV1 {
    CanonicalRelationEdgeV1 {
        from_occurrence: SymbolOccurrenceId::new(from).unwrap(),
        to_occurrence: target.clone(),
        kind,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    }
}

fn digest_value<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

#[test]
fn lsp_method_state_event_is_bounded_and_measured() {
    assert_eq!(
        lsp_method_state_event(
            FeedbackLspStateV1::MethodCompleted,
            FeedbackOutcomeV1::Completed,
            1,
            42,
        ),
        FeedbackSourceEventV1::LspState {
            state: FeedbackLspStateV1::MethodCompleted,
            method: Some(FeedbackLspMethodClassV1::Diagnostics),
            outcome: FeedbackOutcomeV1::Completed,
            item_count: 1,
            duration_micros: Some(42),
        }
    );
}

#[test]
fn dirty_overlay_result_cannot_gain_durable_outputs_or_handles() {
    let request = request(FeedbackContentIdentityV1::EphemeralOverlay {
        session_id: SessionId::new("session.overlay").unwrap(),
        owner_client_id: HostInstanceId::new("host.overlay").unwrap(),
        agent_id: None,
        document_version: 1,
        overlay_digest: digest(SHA_A),
    });
    let cycle = FeedbackCycleResultV1::new(
        &request,
        FeedbackCycleTerminationV1::UserStop,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        Vec::new(),
        0,
        0,
        0,
    )
    .unwrap();
    let execution = execution(cycle);
    assert!(
        CanonicalFeedbackResultV1::new(execution.clone(), Vec::new()).is_ok(),
        "session-only results remain usable in their owner session"
    );

    let mut leaked = execution;
    leaked.dedupe_key =
        Some(tracedecay_domain::feedback::FeedbackDedupeKeyV1::new("dedupe.overlay").unwrap());
    assert!(CanonicalFeedbackResultV1::new(leaked, Vec::new()).is_err());
}

#[test]
fn durable_finding_expansion_preserves_identity_and_exact_anchor() {
    let request = request(FeedbackContentIdentityV1::SavedContent {
        generation_digest: digest(SHA_A),
        file_digest: digest(SHA_B),
    });
    let anchor = RetrievalAnchorId::new("anchor.canonical-feedback").unwrap();
    let finding = FeedbackFindingV1 {
        finding_id: FeedbackFindingId::new("finding.canonical-feedback").unwrap(),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(anchor.clone()),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: None,
        diagnostic_projection: None,
    };
    let cycle = FeedbackCycleResultV1::new(
        &request,
        FeedbackCycleTerminationV1::Blocked,
        vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
        Vec::new(),
        None,
        None,
        None,
        vec![finding.clone()],
        1,
        1,
        0,
    )
    .unwrap();
    let execution = execution(cycle);
    let expansion = feedback_expansion_request(&finding)
        .unwrap()
        .expect("anchored finding expands");

    assert_eq!(expansion.finding_id, finding.finding_id);
    assert_eq!(expansion.expansion.anchor, anchor);
    assert_eq!(
        expansion.expansion.meta.projection,
        ResultProjection::ReferencesOnly
    );
    assert_eq!(
        feedback_handle_request_id("get", &execution, &finding).unwrap(),
        feedback_handle_request_id("get", &execution, &finding).unwrap()
    );
    assert_ne!(
        feedback_handle_request_id("get", &execution, &finding).unwrap(),
        feedback_handle_request_id("expand", &execution, &finding).unwrap()
    );
}
