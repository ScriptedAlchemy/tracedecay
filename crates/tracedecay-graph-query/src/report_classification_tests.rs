//! TestRisk, dead code, and health share one classification of a generation.
//!
//! The fixture mixes production symbols whose names or directories merely
//! begin with `test` with parser-attested tests. Reports name the production
//! symbols and leave the tests out of the risk and dead-code censuses.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_contracts::{
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::{
    ActorId, BoundedSanitizedText, CanonicalRelationEdgeV1, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, ComplexityAnalysisV1,
    EdgeAuthorityV1, FileOccurrenceId, LanguageId, ManifestDigest, ProjectId, RefId,
    RelationEdgeKindV1, RepositoryId, SanitizedCodeFileV1, SensitivityDecision, SensitivityLevelV1,
    SnapshotFileDispositionV1, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::test_risk::analyze_test_risk;
use crate::{GraphQueryManager, SourceReadContext, VerifiedGraphQuery};

struct FixtureSymbol {
    path: &'static str,
    name: &'static str,
    qualified: &'static str,
    kind: &'static str,
    visibility: &'static str,
    skip_test_coverage: bool,
    signature: Option<&'static str>,
}

struct FixtureEdge {
    from: usize,
    to: usize,
    kind: RelationEdgeKindV1,
}

fn fixture_id<T>(value: impl Into<String>) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.into()).expect("valid fixture identity")
}

fn fixture_digest<T>(ordinal: usize) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    fixture_id(format!("sha256:{ordinal:064x}"))
}

fn publish(symbols: &[FixtureSymbol], edges: &[FixtureEdge]) -> CodeGraphInteractiveReader {
    let generation: CodeGenerationId = fixture_id("generation.classification.1");
    let mut files = Vec::new();
    let mut file_occurrences = HashMap::<&str, FileOccurrenceId>::new();
    let mut records = Vec::new();
    let mut chunks = Vec::new();
    let mut occurrences = Vec::new();

    for (ordinal, fixture) in symbols.iter().enumerate() {
        let file = file_occurrences
            .entry(fixture.path)
            .or_insert_with(|| {
                let occurrence: FileOccurrenceId = fixture_id(format!("file.{}", files.len()));
                files.push(SanitizedCodeFileV1 {
                    file_occurrence_id: occurrence.clone(),
                    logical_path: fixture.path.to_owned(),
                    language: Some(LanguageId::new("rust").expect("fixture language")),
                    content_digest: fixture_digest(5_000 + files.len()),
                    disposition: SnapshotFileDispositionV1::Present,
                });
                occurrence
            })
            .clone();
        let occurrence: SymbolOccurrenceId = fixture_id(format!("symbol.{ordinal}"));
        records.push(Arc::new(LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: fixture_digest(1_000 + ordinal),
            qualified_name: fixture.qualified.to_owned(),
            simple_name: fixture.name.to_owned(),
            kind: fixture.kind.to_owned(),
            visibility: fixture.visibility.to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: u32::try_from(ordinal).expect("fixture line"),
            signature: fixture
                .signature
                .map(str::to_owned)
                .or_else(|| Some(format!("fn {}()", fixture.name))),
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: fixture.skip_test_coverage,
            file_identity: fixture_digest(3_000 + ordinal),
            content_digest: fixture_digest(2_000 + ordinal),
        }));
        chunks.push(Arc::new(CodeSearchChunkV1 {
            id: fixture_id(format!("chunk.{ordinal}")),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: file,
                symbol_occurrence_id: Some(occurrence.clone()),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: ordinal as u64,
                    end_byte: ordinal as u64 + 1,
                },
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: u32::try_from(ordinal).expect("fixture ordinal"),
            },
            content_digest: fixture_digest(4_000 + ordinal),
            language_descriptor_revision: fixture_id("language.rust.fixture.v1"),
            chunker_revision: fixture_id("chunker.fixture.v1"),
            sanitizer_revision: fixture_id("sanitizer.fixture.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: fixture_id("policy.fixture.v1"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new("fixture symbol")
                .expect("bounded fixture text"),
        }));
        occurrences.push(occurrence);
    }

    let edges = edges
        .iter()
        .map(|edge| CanonicalRelationEdgeV1 {
            from_occurrence: occurrences[edge.from].clone(),
            to_occurrence: occurrences[edge.to].clone(),
            kind: edge.kind,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        })
        .collect::<Vec<_>>();
    let index =
        GenerationSymbolIndexV1::new(generation.clone(), records).expect("fixture symbol index");
    let cancellation =
        CancellationSignal::active("cancel.classification").expect("fixture cancellation");
    let store =
        HermeticCodeGraphProjectionStore::memory(&cancellation).expect("fixture projection");
    store
        .publish_indexed_with_cancellation(
            &generation,
            &edges,
            &chunks,
            &files,
            &index,
            Arc::new(NeverCancelled),
        )
        .expect("publish fixture generation");
    store
        .verified_store(&generation)
        .expect("open fixture generation")
        .interactive_reader_with_cancellation(&generation, Arc::new(NeverCancelled))
        .expect("fixture reader")
}

fn symbol(
    path: &'static str,
    name: &'static str,
    qualified: &'static str,
    kind: &'static str,
) -> FixtureSymbol {
    FixtureSymbol {
        path,
        name,
        qualified,
        kind,
        visibility: "private",
        skip_test_coverage: false,
        signature: None,
    }
}

fn classification_fixture() -> (CodeGraphInteractiveReader, Vec<FixtureSymbol>) {
    let mut symbols = vec![
        symbol("src/client.rs", "main", "src/client.rs::main", "function"),
        symbol(
            "src/client.rs",
            "called",
            "src/client.rs::called",
            "function",
        ),
        symbol(
            "src/client.rs",
            "test_connection",
            "src/client.rs::test_connection",
            "function",
        ),
        symbol(
            "src/client.rs",
            "testimony",
            "src/client.rs::testimony",
            "function",
        ),
        symbol(
            "src/testnet/client.rs",
            "connect",
            "src/testnet/client.rs::connect",
            "function",
        ),
        symbol(
            "src/client.rs",
            "exported",
            "src/client.rs::exported",
            "function",
        ),
        symbol(
            "src/client.rs",
            "annotated_case",
            "src/client.rs::annotated_case",
            "function",
        ),
        symbol(
            "src/client.rs",
            "test",
            "src/client.rs::@test",
            "annotation_usage",
        ),
        symbol(
            "src/client.rs",
            "qualified_case",
            "src/client.rs::qualified_case",
            "function",
        ),
        symbol(
            "src/client.rs",
            "tokio::test",
            "src/client.rs::@tokio::test",
            "annotation_usage",
        ),
        symbol(
            "src/client.rs",
            "helpers",
            "src/client.rs::helpers",
            "module",
        ),
        symbol(
            "src/client.rs",
            "cfg",
            "src/client.rs::@cfg",
            "annotation_usage",
        ),
        symbol(
            "src/client.rs",
            "fixture_fn",
            "src/client.rs::helpers::fixture_fn",
            "function",
        ),
        symbol("src/client.rs", "tests", "src/client.rs::tests", "module"),
        symbol(
            "src/client.rs",
            "module_helper",
            "src/client.rs::tests::module_helper",
            "function",
        ),
        symbol(
            "tests/real.rs",
            "covers_case",
            "tests/real.rs::covers_case",
            "function",
        ),
        symbol(
            "src/client.rs",
            "skipped_prod",
            "src/client.rs::skipped_prod",
            "function",
        ),
    ];
    symbols[5].visibility = "public";
    symbols[11].signature = Some("#[cfg(test)]");
    symbols[16].skip_test_coverage = true;

    let edges = [
        FixtureEdge {
            from: 0,
            to: 1,
            kind: RelationEdgeKindV1::Calls,
        },
        FixtureEdge {
            from: 7,
            to: 6,
            kind: RelationEdgeKindV1::Annotates,
        },
        FixtureEdge {
            from: 9,
            to: 8,
            kind: RelationEdgeKindV1::Annotates,
        },
        FixtureEdge {
            from: 11,
            to: 10,
            kind: RelationEdgeKindV1::Annotates,
        },
        FixtureEdge {
            from: 10,
            to: 12,
            kind: RelationEdgeKindV1::Contains,
        },
        FixtureEdge {
            from: 13,
            to: 14,
            kind: RelationEdgeKindV1::Contains,
        },
    ];
    (publish(&symbols, &edges), symbols)
}

fn names_of(symbols: &[CodeGraphSymbolSummaryV1]) -> BTreeSet<String> {
    symbols
        .iter()
        .map(|symbol| {
            symbol
                .metadata
                .as_ref()
                .expect("reported symbol has metadata")
                .simple_name
                .clone()
        })
        .collect()
}

fn request_context(project_id: &str, cancellation: &CancellationSignal) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new(project_id).expect("project"),
        RepositoryId::new("repository.classification").expect("repository"),
        WorktreeId::new("worktree.classification").expect("worktree"),
        Some(RefId::new("refs/heads/classification").expect("reference")),
    )
    .expect("scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.classification").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        ActorId::new("actor.classification.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.classification").expect("capability")]),
        BTreeSet::from([UseCaseId::new("use-case.classification").expect("use case")]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.classification.requester").expect("requester"),
        scope,
        grant,
        RequestId::new("request.classification").expect("request"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        cancellation.context(),
    )
    .expect("context")
}

async fn test_database(path: &Path) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "classification fixture")
        .expect("database authority");
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
        .await
        .expect("database")
        .0
}

#[tokio::test]
async fn production_symbols_named_like_tests_stay_visible_and_real_tests_do_not() {
    let (reader, _) = classification_fixture();
    let cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation> = Arc::new(NeverCancelled);
    let manager = GraphQueryManager::new(&reader, Arc::clone(&cancellation));

    let dead = manager
        .find_dead_code(
            &[NodeKind::Function, NodeKind::Method],
            false,
            None,
            Some(50),
        )
        .await
        .expect("dead code");
    let dead_names = names_of(&dead);
    let with_public = manager
        .find_dead_code(
            &[NodeKind::Function, NodeKind::Method],
            true,
            None,
            Some(50),
        )
        .await
        .expect("dead code including public");
    let health = manager.health_inputs(None).await.expect("health");
    let dead_functions: usize = health
        .aggregates
        .iter()
        .map(|aggregate| aggregate.dead_function_methods)
        .sum();
    let total_functions: usize = health
        .aggregates
        .iter()
        .map(|aggregate| aggregate.function_methods)
        .sum();
    let skipped_functions: usize = health
        .aggregates
        .iter()
        .map(|aggregate| aggregate.skipped_function_methods)
        .sum();

    let root = tempfile::tempdir().expect("project root");
    let db = test_database(&root.path().join("project.db")).await;
    let context_cancellation =
        CancellationSignal::active("cancel.classification.query").expect("cancellation");
    let project_id = "project.classification";
    let query = VerifiedGraphQuery::from_fixture_reader(
        reader,
        cancellation,
        request_context(project_id, &context_cancellation),
    )
    .with_source(SourceReadContext::new(
        root.path().to_path_buf(),
        db,
        true,
        project_id.to_owned(),
        tracedecay_domain::IndexPathPolicyV1::new(Vec::new(), Vec::new()).expect("path policy"),
    ));
    let risk = analyze_test_risk(&query, None, false, 50)
        .await
        .expect("test risk");
    let risk_names = risk
        .risks
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<BTreeSet<_>>();

    let production = BTreeSet::from([
        "connect".to_owned(),
        "skipped_prod".to_owned(),
        "test_connection".to_owned(),
        "testimony".to_owned(),
    ]);
    let expected_risk = BTreeSet::from([
        "called".to_owned(),
        "connect".to_owned(),
        "exported".to_owned(),
        "main".to_owned(),
        "test_connection".to_owned(),
        "testimony".to_owned(),
    ]);
    let public_included = with_public.iter().any(|symbol| {
        symbol
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.simple_name == "exported")
    });
    assert!(
        dead_names == production
            && public_included
            && total_functions == 12
            && dead_functions == 4
            && skipped_functions == 1
            && risk_names == expected_risk
            && risk.summary.skipped == 1,
        "dead={dead_names:?}\nhealth dead={dead_functions} total={total_functions} skipped={skipped_functions}\nrisk={risk_names:?} skipped={} excluded={}\npublic_included={public_included}",
        risk.summary.skipped,
        risk.summary.buckets.excluded
    );
}
