use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_contracts::CancellationSignal;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, ComplexityAnalysisV1,
    ContentDigest, EdgeAuthorityV1, FileOccurrenceId, LanguageDescriptorRevision, LanguageId,
    PolicyRevisionId, RelationEdgeKindV1, SanitizedCodeFileV1, SanitizerRevision,
    SensitivityDecision, SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphGenerationManifest, GraphNamespace, GraphProjectorRevision,
    NeverCancelled, VerifiedGraphSnapshot,
};

use crate::graph_projection::builder::ProductionCodeGraphInputs;
use crate::graph_projection::schema::SYMBOL_LABEL;
use crate::graph_projection::{
    CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionError, CodeGraphProjectionStore,
    CodeGraphSymbolSummaryV1, build_code_graph_manifest_inputs_checked,
    code_graph_projection_identity, current_generation_entity, has_label,
};
use crate::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
mod bundle_artifact;
mod imports;
mod warm_catalog;

struct CancelledNow;

impl GraphCancellation for CancelledNow {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct CancelAfter {
    observations: AtomicU64,
    allowed: u64,
}

impl GraphCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.observations.fetch_add(1, Ordering::Relaxed) >= self.allowed
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid fixture digest")
}

fn generation() -> CodeGenerationId {
    id("generation.interactive.1")
}

fn edge(from: &str, to: &str, kind: RelationEdgeKindV1, start: u64) -> CanonicalRelationEdgeV1 {
    CanonicalRelationEdgeV1 {
        from_occurrence: id(from),
        to_occurrence: id(to),
        kind,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: start,
            end_byte: start + 1,
        },
    }
}

fn chunk(symbol: &str, file: &str, ordinal: u32) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
        id: id(&format!("chunk.{symbol}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation(),
            file_occurrence_id: id(file),
            symbol_occurrence_id: Some(id(symbol)),
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal,
        },
        content_digest: digest::<ContentDigest>('c'),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("language.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new("code").expect("bounded fixture text"),
    }
}

fn file(occurrence: &str, path: &str) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: id(occurrence),
        logical_path: path.to_owned(),
        language: Some(LanguageId::new("rust").expect("valid language id")),
        content_digest: digest::<ContentDigest>('f'),
        disposition: SnapshotFileDispositionV1::Present,
    }
}

fn symbol_metadata(
    occurrence: &str,
    qualified_name: &str,
    kind: &str,
    identity_byte: char,
) -> LineageSymbolRecordV1 {
    LineageSymbolRecordV1 {
        occurrence: id(occurrence),
        identity: digest(identity_byte),
        qualified_name: qualified_name.to_owned(),
        simple_name: qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(qualified_name)
            .to_owned(),
        kind: kind.to_owned(),
        visibility: "private".to_owned(),
        branches: 0,
        loops: 0,
        max_nesting: 0,
        complexity_analysis: ComplexityAnalysisV1::Complete,
        line_span: 1,
        start_line: 0,
        signature: None,
        docstring: None,
        is_async: false,
        derives: Vec::new(),
        skip_test_coverage: false,
        file_identity: digest('e'),
        content_digest: digest('d'),
    }
}

/// alpha::run -Calls-> beta::run, gamma::main -Calls-> alpha::run,
/// beta::Runner -Uses-> beta::run.
fn fixture_edges() -> Vec<CanonicalRelationEdgeV1> {
    vec![
        edge(
            "sym.alpha.run",
            "sym.beta.run",
            RelationEdgeKindV1::Calls,
            0,
        ),
        edge(
            "sym.gamma.main",
            "sym.alpha.run",
            RelationEdgeKindV1::Calls,
            2,
        ),
        edge(
            "sym.beta.runner",
            "sym.beta.run",
            RelationEdgeKindV1::Uses,
            4,
        ),
    ]
}

fn fixture_chunks() -> Vec<Arc<CodeSearchChunkV1>> {
    vec![
        Arc::new(chunk("sym.alpha.run", "file.f1", 0)),
        Arc::new(chunk("sym.beta.run", "file.f2", 1)),
        Arc::new(chunk("sym.beta.runner", "file.f2", 2)),
        Arc::new(chunk("sym.gamma.main", "file.f3", 3)),
    ]
}

fn fixture_files() -> Vec<SanitizedCodeFileV1> {
    vec![
        file("file.f1", "src/alpha.rs"),
        file("file.f2", "src/beta.rs"),
        file("file.f3", "src/gamma.rs"),
    ]
}

fn fixture_symbols() -> GenerationSymbolIndexV1 {
    GenerationSymbolIndexV1::new(
        generation(),
        vec![
            Arc::new(symbol_metadata(
                "sym.alpha.run",
                "alpha::run",
                "function",
                '1',
            )),
            Arc::new(symbol_metadata(
                "sym.beta.run",
                "beta::run",
                "function",
                '2',
            )),
            Arc::new(symbol_metadata(
                "sym.beta.runner",
                "beta::Runner",
                "struct",
                '3',
            )),
            Arc::new(symbol_metadata(
                "sym.gamma.main",
                "gamma::main",
                "function",
                '4',
            )),
        ],
    )
    .expect("valid fixture symbol index")
}

fn production_manifest() -> GraphGenerationManifest {
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let files = fixture_files();
    let symbols = fixture_symbols();
    build_code_graph_manifest_inputs_checked(
        projection,
        &generation(),
        &fixture_edges(),
        &fixture_chunks(),
        Some(ProductionCodeGraphInputs {
            files: &files,
            symbols: &symbols,
            imports: &[],
        }),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("valid fixture manifest")
}

fn large_production_manifest(symbol_count: usize) -> GraphGenerationManifest {
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let files = vec![file("file.f1", "src/alpha.rs")];
    let mut chunks = Vec::with_capacity(symbol_count);
    let mut symbols = Vec::with_capacity(symbol_count);
    for index in 0..symbol_count {
        let occurrence = format!("sym.bulk.{index:05}");
        let qualified_name = if index % 2 == 0 {
            format!("bulk::Needle{index}")
        } else {
            format!("bulk::Other{index}")
        };
        chunks.push(Arc::new(chunk(&occurrence, "file.f1", index as u32)));
        let mut metadata = symbol_metadata(&occurrence, &qualified_name, "function", '1');
        metadata.identity = id(&format!("sha256:{index:064x}"));
        symbols.push(Arc::new(metadata));
    }
    let symbols =
        GenerationSymbolIndexV1::new(generation(), symbols).expect("large fixture symbol index");
    build_code_graph_manifest_inputs_checked(
        projection,
        &generation(),
        &[],
        &chunks,
        Some(ProductionCodeGraphInputs {
            files: &files,
            symbols: &symbols,
            imports: &[],
        }),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("large fixture manifest")
}

fn store_for(manifest: GraphGenerationManifest) -> CodeGraphProjectionStore {
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("open memory snapshot");
    CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation())
        .expect("open verified store")
}

fn reader(store: &CodeGraphProjectionStore) -> super::CodeGraphInteractiveReader {
    let cancellation =
        CancellationSignal::active("cancellation.interactive.fixture").expect("valid token");
    store
        .interactive_reader(&generation(), &cancellation)
        .expect("open interactive reader")
}

fn request() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn occurrences(summaries: &[CodeGraphSymbolSummaryV1]) -> Vec<String> {
    let mut names: Vec<_> = summaries
        .iter()
        .map(|summary| summary.occurrence.as_str().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn symbol_find_is_bounded_ordered_and_cancellable_during_large_scans() {
    let reader = reader(&store_for(large_production_manifest(5_000)));
    reader
        .symbols_page(None, 1, request())
        .expect("warm large fixture catalog");

    let hits = reader
        .find_symbols(
            &|_, _, metadata| {
                metadata.is_some_and(|metadata| metadata.simple_name.starts_with("Needle"))
            },
            3,
            request(),
        )
        .expect("bounded symbol find");
    assert_eq!(
        occurrences(&hits),
        vec![
            "sym.bulk.00000".to_owned(),
            "sym.bulk.00002".to_owned(),
            "sym.bulk.00004".to_owned(),
        ]
    );

    let cancellation = Arc::new(CancelAfter {
        observations: AtomicU64::new(0),
        allowed: 4,
    });
    let error = reader
        .find_symbols(&|_, _, _| false, 1, cancellation)
        .expect_err("a long catalog scan must observe cancellation");
    assert_eq!(error, CodeGraphProjectionError::Cancelled);
}

#[test]
fn symbol_find_denies_a_pre_cancelled_read() {
    let reader = reader(&store_for(production_manifest()));
    let error = reader
        .find_symbols(&|_, _, _| true, 1, Arc::new(CancelledNow))
        .expect_err("cancelled symbol find must be refused");
    assert_eq!(error, CodeGraphProjectionError::Cancelled);
}

#[test]
fn file_and_page_listings_cover_the_generation_exactly_once() {
    let reader = reader(&store_for(production_manifest()));
    let in_beta = reader
        .symbols_in_file(&id::<FileOccurrenceId>("file.f2"), 8, request())
        .expect("file listing");
    assert_eq!(
        occurrences(&in_beta),
        vec!["sym.beta.run".to_owned(), "sym.beta.runner".to_owned()]
    );

    let mut collected = Vec::new();
    let mut cursor: Option<SymbolOccurrenceId> = None;
    let mut pages = 0;
    loop {
        let page = reader
            .symbols_page(cursor.as_ref(), 3, request())
            .expect("symbol page");
        pages += 1;
        collected.extend(page.symbols.iter().cloned());
        if !page.has_more {
            break;
        }
        cursor = Some(
            page.symbols
                .last()
                .expect("non-empty page when more remain")
                .occurrence
                .clone(),
        );
    }
    assert_eq!(pages, 2, "four symbols paged by three need two pages");
    assert_eq!(
        occurrences(&collected),
        vec![
            "sym.alpha.run".to_owned(),
            "sym.beta.run".to_owned(),
            "sym.beta.runner".to_owned(),
            "sym.gamma.main".to_owned(),
        ]
    );
}

#[test]
fn logical_path_resolution_reports_an_unknown_path_as_empty_not_an_error() {
    let reader = reader(&store_for(production_manifest()));
    let hits = reader
        .symbols_in_logical_file("src/absent.rs", 8, request())
        .expect("an unpublished logical path must not be an error");
    assert!(
        hits.is_empty(),
        "no file at this path in the generation means no symbols, not a refusal"
    );
}

#[test]
fn logical_path_resolution_denies_a_cancelled_read() {
    let reader = reader(&store_for(production_manifest()));
    let error = reader
        .symbols_in_logical_file("src/beta.rs", 8, Arc::new(CancelledNow))
        .expect_err("cancelled logical path listing must be refused");
    assert_eq!(error, CodeGraphProjectionError::Cancelled);
}

#[test]
fn impact_expands_reverse_reachability_and_reports_truncation() {
    let reader = reader(&store_for(production_manifest()));
    let seed = vec![id::<SymbolOccurrenceId>("sym.beta.run")];

    let full = reader
        .impact(&seed, &[RelationEdgeKindV1::Calls], 3, 16, 64, request())
        .expect("full impact");
    assert!(full.complete);
    let depths: Vec<_> = full
        .impacted
        .iter()
        .map(|hit| (hit.summary.occurrence.as_str().to_owned(), hit.depth))
        .collect();
    assert_eq!(
        depths,
        vec![
            ("sym.alpha.run".to_owned(), 1),
            ("sym.gamma.main".to_owned(), 2),
        ]
    );

    let truncated = reader
        .impact(&seed, &[RelationEdgeKindV1::Calls], 3, 1, 64, request())
        .expect("truncated impact");
    assert_eq!(truncated.impacted.len(), 1);
    assert!(
        !truncated.complete,
        "symbol ceiling must mark the batch incomplete"
    );

    let depth_capped = reader
        .impact(&seed, &[RelationEdgeKindV1::Calls], 1, 16, 64, request())
        .expect("depth-capped impact");
    assert_eq!(depth_capped.impacted.len(), 1);
    assert!(
        !depth_capped.complete,
        "unexplored callers past the depth cap must mark the batch incomplete"
    );
}

#[test]
fn shortest_path_distinguishes_no_path_from_truncated_search() {
    let reader = reader(&store_for(production_manifest()));
    let main = id::<SymbolOccurrenceId>("sym.gamma.main");
    let beta = id::<SymbolOccurrenceId>("sym.beta.run");
    let runner = id::<SymbolOccurrenceId>("sym.beta.runner");

    let found = reader
        .shortest_path(&main, &beta, &[RelationEdgeKindV1::Calls], 4, 64, request())
        .expect("path search");
    let path = found.path.expect("two-hop path exists");
    assert!(found.complete);
    assert_eq!(path.len(), 2);
    assert_eq!(path[0].from_occurrence.as_str(), "sym.gamma.main");
    assert_eq!(path[1].to_occurrence.as_str(), "sym.beta.run");

    let unreachable = reader
        .shortest_path(
            &main,
            &runner,
            &[RelationEdgeKindV1::Calls],
            4,
            64,
            request(),
        )
        .expect("unreachable search");
    assert_eq!(unreachable.path, None);
    assert!(
        unreachable.complete,
        "exhausted search is a definitive no-path"
    );

    let capped = reader
        .shortest_path(&main, &beta, &[RelationEdgeKindV1::Calls], 1, 64, request())
        .expect("depth-capped search");
    assert_eq!(capped.path, None);
    assert!(
        !capped.complete,
        "depth cap with live frontier is not a no-path verdict"
    );
}

#[test]
fn generation_mismatch_is_refused_at_reader_construction() {
    let store = store_for(production_manifest());
    let cancellation =
        CancellationSignal::active("cancellation.interactive.mismatch").expect("valid token");
    let error = store
        .interactive_reader(&id::<CodeGenerationId>("generation.other"), &cancellation)
        .expect_err("foreign generation must be refused");
    assert_eq!(error, CodeGraphProjectionError::GenerationMismatch);
}

#[test]
fn stale_current_generation_marker_is_refused() {
    let mut manifest = production_manifest();
    let stale = current_generation_entity(
        &id::<CodeGenerationId>("generation.stale"),
        manifest.entities.len(),
    )
    .expect("stale marker entity");
    for entity in &mut manifest.entities {
        if entity.identity == stale.identity {
            *entity = stale.clone();
        }
    }
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("open forged snapshot");
    let store = CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation())
        .expect("store admits the manifest before reading the marker");
    let cancellation =
        CancellationSignal::active("cancellation.interactive.stale").expect("valid token");
    let error = store
        .interactive_reader(&generation(), &cancellation)
        .expect_err("stale current-generation marker must be refused");
    assert_eq!(error, CodeGraphProjectionError::GenerationMismatch);
}

#[test]
fn corrupt_symbol_payload_is_refused_not_skipped() {
    let mut manifest = production_manifest();
    // Swap the payloads of two symbol entities so each identity carries the
    // other's record.
    let mut symbol_indices = Vec::new();
    for (index, entity) in manifest.entities.iter().enumerate() {
        if has_label(entity, SYMBOL_LABEL) {
            symbol_indices.push(index);
        }
    }
    assert!(symbol_indices.len() >= 2);
    let (first, second) = (symbol_indices[0], symbol_indices[1]);
    let swapped = manifest.entities[first].properties.clone();
    manifest.entities[first].properties = manifest.entities[second].properties.clone();
    manifest.entities[second].properties = swapped;

    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("open forged snapshot");
    let store = CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation())
        .expect("store admits the manifest before payload reads");
    let reader = reader(&store);
    let error = reader
        .resolve_qualified_name("beta::run", None, 8, request())
        .expect_err("corrupt symbol payload must refuse the read");
    assert!(matches!(error, CodeGraphProjectionError::Corrupt(_)));
}

#[test]
fn cancellation_denies_catalog_and_adjacency_reads() {
    let reader = reader(&store_for(production_manifest()));
    let cancelled: Arc<dyn GraphCancellation> = Arc::new(CancelledNow);
    assert_eq!(
        reader
            .resolve_qualified_name("beta::run", None, 8, Arc::clone(&cancelled))
            .expect_err("cancelled resolve"),
        CodeGraphProjectionError::Cancelled
    );
    assert_eq!(
        reader
            .callers(
                &[id::<SymbolOccurrenceId>("sym.beta.run")],
                &[],
                16,
                cancelled,
            )
            .expect_err("cancelled callers"),
        CodeGraphProjectionError::Cancelled
    );
}

#[test]
fn exhausted_fanout_budget_is_a_typed_refusal() {
    let reader = reader(&store_for(production_manifest()));
    let error = reader
        .callers(
            &[id::<SymbolOccurrenceId>("sym.beta.run")],
            &[],
            0,
            request(),
        )
        .expect_err("zero fan-out budget must refuse");
    assert!(matches!(
        error,
        CodeGraphProjectionError::BudgetExhausted { .. }
    ));
}

#[test]
fn callers_truncated_returns_a_prefix_instead_of_refusing() {
    let reader = reader(&store_for(production_manifest()));
    let beta = vec![id::<SymbolOccurrenceId>("sym.beta.run")];
    let refused = reader
        .callers(&beta, &[], 1, request())
        .expect_err("two incoming edges exceed a unit refuse budget");
    assert!(
        matches!(refused, CodeGraphProjectionError::BudgetExhausted { .. }),
        "refuse must stay typed: {refused:?}"
    );
    let truncated = reader
        .callers_truncated(&beta, &[], 1, request())
        .expect("page-shaped callers stop at the budget");
    assert_eq!(truncated.len(), 1);
    assert_eq!(
        truncated[0].len(),
        1,
        "truncate must return the admitted prefix, not the full neighborhood"
    );
}

#[test]
fn retrieval_only_publication_serves_no_names_truthfully() {
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let manifest = build_code_graph_manifest_inputs_checked(
        projection,
        &generation(),
        &fixture_edges(),
        &fixture_chunks(),
        None,
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("valid retrieval-only manifest");
    let reader = reader(&store_for(manifest));
    let hits = reader
        .resolve_qualified_name("beta::run", None, 8, request())
        .expect("resolve against metadata-free generation");
    assert!(hits.is_empty(), "no published names means no name hits");
    let summary = reader
        .symbol_summary(&id::<SymbolOccurrenceId>("sym.beta.run"), request())
        .expect("summary read")
        .expect("symbol entity exists");
    assert_eq!(summary.metadata, None, "absent metadata stays absent");
}

/// A ranking over a prefix of the graph must never be reported as the graph's
/// ranking — the examination budget bounds the scan, and reaching it is
/// truthful truncation rather than a silent partial answer.
#[test]
fn an_exhausted_ranking_budget_is_reported_not_hidden() {
    let reader = reader(&store_for(production_manifest()));

    let ranking = reader
        .degree_ranking(16, 2, request())
        .expect("budget-truncated ranking");

    assert!(
        !ranking.complete,
        "a scan stopped by its examination budget is not a complete ranking"
    );
    assert_eq!(ranking.symbols_examined, 2);
    assert_eq!(ranking.ranked.len(), 2);
}

#[test]
fn degree_ranking_refuses_zero_sized_requests() {
    let reader = reader(&store_for(production_manifest()));

    assert!(matches!(
        reader.degree_ranking(0, 16, request()),
        Err(CodeGraphProjectionError::Contract(_))
    ));
    assert!(matches!(
        reader.degree_ranking(16, 0, request()),
        Err(CodeGraphProjectionError::Contract(_))
    ));
}

#[test]
fn degree_ranking_denies_a_cancelled_read() {
    let reader = reader(&store_for(production_manifest()));

    assert!(matches!(
        reader.degree_ranking(4, 16, Arc::new(CancelledNow)),
        Err(CodeGraphProjectionError::Cancelled)
    ));
}
