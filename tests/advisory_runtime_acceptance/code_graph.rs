use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_application::{CancellationSignal, RequestAdmission, ResolvedScope};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::feedback::FeedbackScopeV1;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
    ContentDigest, EdgeAuthorityV1, FileIdentityDigest, FileOccurrenceId,
    LanguageDescriptorRevision, LanguageId, PolicyRevisionId, RefId, RelationEdgeKindV1,
    SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
    SnapshotFileDispositionV1, SourceSpan, SymbolIdentityDigest, SymbolOccurrenceId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_usecases::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};

#[derive(Clone)]
struct HermeticAdvisoryCodeGraphV1 {
    scope: ResolvedScope,
    store: Arc<CodeGraphProjectionStore>,
}

impl CodeGraphProjectionReadPort for HermeticAdvisoryCodeGraphV1 {
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
            VerifiedCodeGraphRead::new(self.scope.clone(), Arc::clone(&self.store))
        })
    }
}

pub(super) fn hermetic_advisory_code_graph(
    scope: &FeedbackScopeV1,
    project_root: &Path,
    logical_path: &str,
) -> Arc<dyn CodeGraphProjectionReadPort> {
    let resolved_scope = resolved_scope(scope);
    let bytes = std::fs::read(project_root.join(logical_path)).expect("fixture graph source");
    let source_digest = hex::encode(Sha256::digest(&bytes));
    let generation = CodeGenerationId::new(format!(
        "generation.advisory.github.{}",
        &source_digest[..16]
    ))
    .expect("fixture graph generation");
    let files = [SanitizedCodeFileV1 {
        file_occurrence_id: FileOccurrenceId::new(format!(
            "file.advisory.github.{}",
            &source_digest[..16]
        ))
        .expect("fixture graph file identity"),
        logical_path: logical_path.to_owned(),
        language: None,
        content_digest: ContentDigest::new(format!("sha256:{source_digest}"))
            .expect("fixture graph content digest"),
        disposition: SnapshotFileDispositionV1::Present,
    }];
    publish_graph(resolved_scope, generation, &[], &[], &files, None)
}

pub(super) fn hermetic_ci_code_graph(
    scope: &FeedbackScopeV1,
    project_root: &Path,
) -> Arc<dyn CodeGraphProjectionReadPort> {
    let logical_path = "src/lib.rs";
    let source =
        std::fs::read_to_string(project_root.join(logical_path)).expect("fixture CI graph source");
    let digest = |domain: &str, value: &str| {
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(format!("{domain}\0{value}")))
        )
    };
    let generation = CodeGenerationId::new(format!(
        "generation.advisory.ci.{}",
        &digest("generation", &source)[7..23]
    ))
    .expect("fixture CI graph generation");
    let file = FileOccurrenceId::new("file.advisory.ci.src-lib").expect("fixture CI file");
    let file_identity = FileIdentityDigest::new(digest("file-identity", logical_path))
        .expect("fixture CI file identity");
    let mut records = Vec::new();
    let mut chunks = Vec::new();
    let mut occurrences = Vec::new();
    for (ordinal, (name, kind, declaration)) in [
        (
            "caller",
            "function",
            "pub fn caller() { failed_symbol(); }\n",
        ),
        ("failed_symbol", "function", "pub fn failed_symbol() {}\n"),
        (
            "failed_symbol_test",
            "test",
            "fn failed_symbol_test() { failed_symbol(); }\n",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let start = source.find(declaration).expect("fixture CI declaration");
        let span = SourceSpan {
            start_byte: u64::try_from(start).expect("fixture CI span start"),
            end_byte: u64::try_from(start + declaration.len()).expect("fixture CI span end"),
        };
        let occurrence = SymbolOccurrenceId::new(format!("symbol.advisory.ci.{name}"))
            .expect("fixture CI symbol occurrence");
        records.push(LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: SymbolIdentityDigest::new(digest("symbol-identity", name))
                .expect("fixture CI symbol identity"),
            qualified_name: name.to_owned(),
            simple_name: name.to_owned(),
            kind: kind.to_owned(),
            visibility: if declaration.starts_with("pub ") {
                "pub".to_owned()
            } else {
                "private".to_owned()
            },
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span: 1,
            start_line: u32::try_from(source[..start].lines().count() + 1)
                .expect("fixture CI start line"),
            signature: Some(declaration.trim().to_owned()),
            skip_test_coverage: false,
            file_identity: file_identity.clone(),
            content_digest: ContentDigest::new(digest("symbol-content", declaration))
                .expect("fixture CI symbol content"),
        });
        chunks.push(CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("chunk.advisory.ci.{name}"))
                .expect("fixture CI chunk"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: file.clone(),
                symbol_occurrence_id: Some(occurrence.clone()),
                parent_chunk_id: None,
                source_span: span,
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: u32::try_from(ordinal).expect("fixture CI chunk ordinal"),
            },
            content_digest: ContentDigest::new(digest("chunk-content", declaration))
                .expect("fixture CI chunk content"),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.advisory-ci.v1",
            )
            .expect("fixture CI language revision"),
            chunker_revision: ChunkerRevision::new("chunker.advisory-ci.v1")
                .expect("fixture CI chunker revision"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.advisory-ci.v1")
                .expect("fixture CI sanitizer revision"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.advisory-ci.v1")
                    .expect("fixture CI policy revision"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new(declaration.trim())
                .expect("fixture CI sanitized text"),
        });
        occurrences.push(occurrence);
    }
    let failed = occurrences[1].clone();
    let edges = [occurrences[0].clone(), occurrences[2].clone()]
        .into_iter()
        .map(|caller| CanonicalRelationEdgeV1 {
            from_occurrence: caller,
            to_occurrence: failed.clone(),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        })
        .collect::<Vec<_>>();
    let symbols =
        GenerationSymbolIndexV1::new(generation.clone(), records).expect("fixture CI symbol index");
    let files = [SanitizedCodeFileV1 {
        file_occurrence_id: file,
        logical_path: logical_path.to_owned(),
        language: Some(LanguageId::new("rust").expect("fixture CI language")),
        content_digest: ContentDigest::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(source.as_bytes()))
        ))
        .expect("fixture CI source digest"),
        disposition: SnapshotFileDispositionV1::Present,
    }];
    publish_graph(
        resolved_scope(scope),
        generation,
        &edges,
        &chunks,
        &files,
        Some(symbols),
    )
}

fn resolved_scope(scope: &FeedbackScopeV1) -> ResolvedScope {
    ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).expect("fixture branch reference")),
    )
    .expect("fixture graph scope")
}

fn publish_graph(
    scope: ResolvedScope,
    generation: CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    files: &[SanitizedCodeFileV1],
    symbols: Option<GenerationSymbolIndexV1>,
) -> Arc<dyn CodeGraphProjectionReadPort> {
    let symbols = symbols.unwrap_or_else(|| {
        GenerationSymbolIndexV1::new(generation.clone(), Vec::new())
            .expect("fixture graph symbol index")
    });
    let cancellation =
        CancellationSignal::active("cancel.advisory.graph").expect("fixture graph cancellation");
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation)
        .expect("hermetic advisory graph projection");
    projection
        .publish_indexed_with_cancellation(
            &generation,
            edges,
            chunks,
            files,
            &symbols,
            Arc::new(NeverCancelled),
        )
        .expect("publish fixture graph generation");
    let store = projection
        .verified_store(&generation)
        .expect("verify fixture graph generation");
    Arc::new(HermeticAdvisoryCodeGraphV1 {
        scope,
        store: Arc::new(store),
    })
}
