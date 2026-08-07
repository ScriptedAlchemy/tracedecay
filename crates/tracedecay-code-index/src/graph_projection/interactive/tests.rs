use std::fmt::Debug;
use std::sync::Arc;

use tracedecay_application::CancellationSignal;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, ContentDigest,
    EdgeAuthorityV1, FileOccurrenceId, LanguageDescriptorRevision, LanguageId, PolicyRevisionId,
    RelationEdgeKindV1, SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphGenerationManifest, GraphNamespace, GraphProjectorRevision,
    NeverCancelled, VerifiedGraphSnapshot,
};

use crate::graph_projection::builder::ProductionCodeGraphInputs;
use crate::graph_projection::{
    CODE_GRAPH_PROJECTOR_REVISION_V2, CodeGraphProjectionError, CodeGraphProjectionStore,
    CodeGraphSymbolSummaryV1, build_code_graph_manifest_inputs_checked,
    code_graph_projection_identity, current_generation_entity, has_label,
};
use crate::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};

struct CancelledNow;

impl GraphCancellation for CancelledNow {
    fn is_cancelled(&self) -> bool {
        true
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
        kind: kind.to_owned(),
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

fn fixture_chunks() -> Vec<CodeSearchChunkV1> {
    vec![
        chunk("sym.alpha.run", "file.f1", 0),
        chunk("sym.beta.run", "file.f2", 1),
        chunk("sym.beta.runner", "file.f2", 2),
        chunk("sym.gamma.main", "file.f3", 3),
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
            symbol_metadata("sym.alpha.run", "alpha::run", "function", '1'),
            symbol_metadata("sym.beta.run", "beta::run", "function", '2'),
            symbol_metadata("sym.beta.runner", "beta::Runner", "struct", '3'),
            symbol_metadata("sym.gamma.main", "gamma::main", "function", '4'),
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
        }),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION_V2.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("valid fixture manifest")
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
fn qualified_name_resolution_is_exact_and_kind_filtered() {
    let reader = reader(&store_for(production_manifest()));
    let hits = reader
        .resolve_qualified_name("beta::run", None, 8, request())
        .expect("resolve qualified name");
    assert_eq!(occurrences(&hits), vec!["sym.beta.run".to_owned()]);
    let metadata = hits[0].metadata.as_ref().expect("production metadata");
    assert_eq!(metadata.kind, "function");
    assert!(
        hits[0].binding.is_some(),
        "resolved symbol keeps its binding"
    );

    let struct_hits = reader
        .resolve_qualified_name("beta::Runner", Some("struct"), 8, request())
        .expect("resolve struct");
    assert_eq!(
        occurrences(&struct_hits),
        vec!["sym.beta.runner".to_owned()]
    );
    let kind_mismatch = reader
        .resolve_qualified_name("beta::Runner", Some("function"), 8, request())
        .expect("kind filter");
    assert!(
        kind_mismatch.is_empty(),
        "kind filter must exclude, not coerce"
    );
    let unknown = reader
        .resolve_qualified_name("delta::absent", None, 8, request())
        .expect("unknown name");
    assert!(unknown.is_empty());
}

#[test]
fn simple_name_resolution_matches_trailing_segment_case_insensitively() {
    let reader = reader(&store_for(production_manifest()));
    let hits = reader
        .resolve_simple_name("RUN", None, 8, request())
        .expect("resolve simple name");
    assert_eq!(
        occurrences(&hits),
        vec!["sym.alpha.run".to_owned(), "sym.beta.run".to_owned()]
    );
    let runner = reader
        .resolve_simple_name("runner", Some("struct"), 8, request())
        .expect("resolve runner");
    assert_eq!(occurrences(&runner), vec!["sym.beta.runner".to_owned()]);
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
fn adjacency_reads_are_kind_filtered_and_endpoint_checked() {
    let reader = reader(&store_for(production_manifest()));
    let beta = vec![id::<SymbolOccurrenceId>("sym.beta.run")];

    let callers = reader
        .callers(&beta, &[RelationEdgeKindV1::Calls], 16, request())
        .expect("callers");
    assert_eq!(callers.len(), 1);
    assert_eq!(
        occurrences(
            &callers[0]
                .iter()
                .map(|edge| edge.neighbor.clone())
                .collect::<Vec<_>>()
        ),
        vec!["sym.alpha.run".to_owned()]
    );

    let all_callers = reader
        .callers(&beta, &[], 16, request())
        .expect("all callers");
    assert_eq!(all_callers[0].len(), 2, "unfiltered callers include Uses");

    let callees = reader
        .callees(
            &[id::<SymbolOccurrenceId>("sym.alpha.run")],
            &[RelationEdgeKindV1::Calls],
            16,
            request(),
        )
        .expect("callees");
    assert_eq!(callees[0].len(), 1);
    assert_eq!(callees[0][0].edge.to_occurrence.as_str(), "sym.beta.run");
}

#[test]
fn degrees_and_kind_counts_report_true_totals() {
    let reader = reader(&store_for(production_manifest()));
    let degrees = reader
        .degrees(
            &[
                id::<SymbolOccurrenceId>("sym.beta.run"),
                id::<SymbolOccurrenceId>("sym.alpha.run"),
            ],
            request(),
        )
        .expect("degrees");
    assert_eq!((degrees[0].outgoing, degrees[0].incoming), (0, 2));
    assert_eq!((degrees[1].outgoing, degrees[1].incoming), (1, 1));

    let counts = reader
        .edge_kind_counts(&id::<SymbolOccurrenceId>("sym.beta.run"), request())
        .expect("kind counts");
    assert!(counts.outgoing.is_empty());
    assert_eq!(counts.incoming.get(&RelationEdgeKindV1::Calls), Some(&1));
    assert_eq!(counts.incoming.get(&RelationEdgeKindV1::Uses), Some(&1));
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
fn induced_edges_stay_inside_the_member_set() {
    let reader = reader(&store_for(production_manifest()));
    let members = vec![
        id::<SymbolOccurrenceId>("sym.gamma.main"),
        id::<SymbolOccurrenceId>("sym.alpha.run"),
        id::<SymbolOccurrenceId>("sym.beta.run"),
    ];
    let edges = reader
        .edges_among(&members, &[RelationEdgeKindV1::Calls], 64, request())
        .expect("induced edges");
    assert_eq!(edges.len(), 2);

    let without_gamma = reader
        .edges_among(&members[1..], &[RelationEdgeKindV1::Calls], 64, request())
        .expect("smaller induced set");
    assert_eq!(without_gamma.len(), 1);
    assert_eq!(
        without_gamma[0].edge.from_occurrence.as_str(),
        "sym.alpha.run"
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
        if has_label(entity, super::SYMBOL_LABEL) {
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
    assert_eq!(error, CodeGraphProjectionError::BudgetExhausted);
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
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION_V2.to_owned())
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

/// The dashboard's degree-pool and top-connected panels aggregated the whole
/// `edges` table twice per read — the same whole-graph scan class that broke
/// strata at scale. The bounded replacement must rank deterministically and
/// must say so when its examination budget stopped the scan.
#[test]
fn degree_ranking_is_deterministic_and_bounded() {
    let reader = reader(&store_for(production_manifest()));

    // alpha::run 1 out + 1 in, beta::run 0 out + 2 in, beta::Runner 1 out,
    // gamma::main 1 out. Total degree descending, then qualified name.
    let ranking = reader
        .degree_ranking(2, 16, request())
        .expect("bounded degree ranking");
    assert!(ranking.complete, "a budget above the symbol count is complete");
    assert_eq!(ranking.symbols_examined, 4);
    assert_eq!(
        ranking
            .ranked
            .iter()
            .map(|entry| entry.occurrence.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["sym.alpha.run".to_owned(), "sym.beta.run".to_owned()],
        "equal-degree symbols break ties by qualified name, not by scan order"
    );
    assert_eq!(
        (ranking.ranked[0].outgoing, ranking.ranked[0].incoming),
        (1, 1)
    );

    let full = reader
        .degree_ranking(16, 16, request())
        .expect("full degree ranking");
    assert_eq!(
        full.ranked
            .iter()
            .map(|entry| entry.occurrence.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec![
            "sym.alpha.run".to_owned(),
            "sym.beta.run".to_owned(),
            "sym.beta.runner".to_owned(),
            "sym.gamma.main".to_owned(),
        ],
        "a top size past the symbol count ranks every symbol, still totally ordered"
    );
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
