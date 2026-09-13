//! Test primitive port over managed test runs and annotation evidence.

use std::sync::{Arc, Mutex};

use tracedecay_contracts::OperationBudgetUsage;
use tracedecay_contracts::retrieval::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1, RankedAffectedTestV1,
    TestMapCoverageV1, TestMapPrimitiveRequest, TestMapPrimitiveResultV1, TestPrimitivePort,
    TestPrimitivePortContext, TestPrimitivePortFuture, TestPrimitivePortOutcome, TestReferenceV1,
    UncoveredSourceV1,
};
use tracedecay_domain::CodeGenerationId;
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_graph_query::queries::GraphQueryManager;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadRequest, request_graph_cancellation,
};

use super::super::support::{
    affected_test_proximity, collect_affected_test_files, rank_affected_tests,
};
use super::{
    files_for_occurrences, open_code_graph, test_annotation_evidence, test_primitive_failed,
};

pub struct TraceDecayTestPrimitivePortV1 {
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    annotation_evidence: Mutex<
        Option<(
            CodeGenerationId,
            std::collections::HashSet<tracedecay_domain::SymbolOccurrenceId>,
        )>,
    >,
}

impl TraceDecayTestPrimitivePortV1 {
    pub fn new(code_graph: Arc<dyn CodeGraphProjectionReadPort>) -> Self {
        Self {
            code_graph,
            annotation_evidence: Mutex::new(None),
        }
    }
}

impl TestPrimitivePort for TraceDecayTestPrimitivePortV1 {
    fn test_map<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a TestMapPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, TestMapPrimitiveResultV1> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let Ok(reader) = open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    context.observed_at,
                    Arc::clone(&cancellation),
                )
                .await
                else {
                    return test_primitive_failed(context);
                };
                let source_nodes = if let Some(file) = request.file.as_deref() {
                    let Ok(nodes) =
                        reader.symbols_in_logical_file(file, 100_000, Arc::clone(&cancellation))
                    else {
                        return test_primitive_failed(context);
                    };
                    nodes
                } else if let Some(node_id) = request.node_id.as_deref() {
                    let Ok(occurrence) =
                        tracedecay_domain::SymbolOccurrenceId::new(node_id.to_owned())
                    else {
                        return test_primitive_failed(context);
                    };
                    let Ok(node) = reader.symbol_summary(&occurrence, Arc::clone(&cancellation))
                    else {
                        return test_primitive_failed(context);
                    };
                    node.into_iter().collect::<Vec<_>>()
                } else {
                    return test_primitive_failed(context);
                };
                let Ok(test_evidence) = test_annotation_evidence(
                    &reader,
                    Arc::clone(&cancellation),
                    &self.annotation_evidence,
                ) else {
                    return test_primitive_failed(context);
                };
                let mut coverage_map = Vec::new();
                let mut uncovered = Vec::new();
                let mut test_files = std::collections::BTreeSet::new();
                let mut unread_symbols = false;
                for node in source_nodes {
                    let Some(metadata) = node.metadata.as_ref() else {
                        unread_symbols = true;
                        continue;
                    };
                    let Some(file_path) = node
                        .binding
                        .as_ref()
                        .and_then(|binding| binding.logical_path.as_deref())
                    else {
                        unread_symbols = true;
                        continue;
                    };
                    if !NodeKind::from_str(&metadata.kind)
                        .is_some_and(|kind| kind.is_callable_kind())
                    {
                        continue;
                    }
                    // A caller or annotation read that fails leaves this symbol
                    // unmeasured. Listing it as uncovered would report a tested
                    // function as untested, so it is omitted and the page reports
                    // itself partial.
                    let Ok(callers) = reader.impact(
                        std::slice::from_ref(&node.occurrence),
                        &[tracedecay_domain::RelationEdgeKindV1::Calls],
                        3,
                        50_000,
                        200_000,
                        Arc::clone(&cancellation),
                    ) else {
                        unread_symbols = true;
                        continue;
                    };
                    if !callers.complete {
                        unread_symbols = true;
                        continue;
                    }
                    let tests: Vec<TestReferenceV1> = callers
                        .impacted
                        .into_iter()
                        .filter_map(|caller| {
                            let caller_metadata = caller.summary.metadata.as_ref()?.clone();
                            let caller_file = caller
                                .summary
                                .binding
                                .as_ref()
                                .and_then(|binding| binding.logical_path.clone())?;
                            (tracedecay_code_index::is_test_file(&caller_file)
                                || test_evidence.contains(&caller.summary.occurrence))
                            .then_some((caller_metadata, caller_file))
                        })
                        .map(|(metadata, caller_file)| {
                            test_files.insert(caller_file.clone());
                            TestReferenceV1 {
                                test_name: metadata.simple_name,
                                test_file: caller_file,
                                test_line: metadata.start_line as usize,
                            }
                        })
                        .collect();
                    if tests.is_empty() {
                        uncovered.push(UncoveredSourceV1 {
                            id: node.occurrence.as_str().to_owned(),
                            name: metadata.simple_name.clone(),
                            file: file_path.to_owned(),
                            line: metadata.start_line as usize,
                        });
                    } else {
                        coverage_map.push(TestMapCoverageV1 {
                            source_name: metadata.simple_name.clone(),
                            source_id: node.occurrence.as_str().to_owned(),
                            source_file: file_path.to_owned(),
                            source_line: metadata.start_line as usize,
                            tests,
                        });
                    }
                }
                let covered_symbols = coverage_map.len();
                let uncovered_symbols = uncovered.len();
                let result = TestMapPrimitiveResultV1 {
                    covered_symbols,
                    uncovered_symbols,
                    test_files: test_files.into_iter().collect(),
                    coverage: coverage_map,
                    uncovered,
                    total: Some((covered_symbols + uncovered_symbols) as u64),
                    next_cursor: None,
                };
                let finished_at = context.observed_at;
                let budget = OperationBudgetUsage::default();
                if unread_symbols {
                    TestPrimitivePortOutcome::Partial {
                        result,
                        finished_at,
                        budget,
                    }
                } else {
                    TestPrimitivePortOutcome::Completed {
                        result,
                        finished_at,
                        budget,
                    }
                }
            },
            label = "usecases.primitives.test_map"
        ))
    }

    fn affected_file_tests<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a AffectedFileTestsPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, AffectedFileTestsPrimitiveResultV1> {
        Box::pin(hotpath::future!(
            async move {
                let custom_glob = request
                    .filter
                    .as_deref()
                    .and_then(|pattern| glob::Pattern::new(pattern).ok());
                let cancellation = request_graph_cancellation(context.request);
                let Ok(verified) = self
                    .code_graph
                    .open(CodeGraphReadRequest::new(
                        context.request,
                        context.observed_at,
                        Arc::clone(&cancellation),
                    ))
                    .await
                else {
                    return test_primitive_failed(context);
                };
                let Ok(reader) = verified.reader_with_cancellation(
                    context.request,
                    context.observed_at,
                    Arc::clone(&cancellation),
                ) else {
                    return test_primitive_failed(context);
                };
                let Ok(test_annotations) = test_annotation_evidence(
                    &reader,
                    Arc::clone(&cancellation),
                    &self.annotation_evidence,
                ) else {
                    return test_primitive_failed(context);
                };
                let Ok(files_with_inline_tests) =
                    files_for_occurrences(&reader, Arc::clone(&cancellation), &test_annotations)
                else {
                    return test_primitive_failed(context);
                };
                let graph = GraphQueryManager::new(&reader, cancellation);
                let Ok(traversal) = collect_affected_test_files(
                    &graph,
                    &request.files,
                    request.maximum_depth,
                    custom_glob.as_ref(),
                    &files_with_inline_tests,
                )
                .await
                else {
                    return test_primitive_failed(context);
                };
                let mut affected_tests =
                    traversal.test_distances.keys().cloned().collect::<Vec<_>>();
                affected_tests.sort();
                let ranked = rank_affected_tests(&traversal.test_distances);
                let ranked_tests = ranked
                    .iter()
                    .enumerate()
                    .map(|(index, test)| RankedAffectedTestV1 {
                        path: test.path.clone(),
                        rank: index + 1,
                        distance: test.distance,
                        proximity: affected_test_proximity(test.distance).to_owned(),
                    })
                    .collect::<Vec<_>>();
                let recommended_tests = ranked
                    .iter()
                    .filter(|test| test.distance <= 2)
                    .map(|test| test.path.clone())
                    .collect();
                let total = affected_tests.len() as u64;
                TestPrimitivePortOutcome::Completed {
                    result: AffectedFileTestsPrimitiveResultV1 {
                        changed_files: request.files.clone(),
                        affected_tests,
                        ranked_tests,
                        recommended_tests,
                        total: Some(total),
                        next_cursor: None,
                    },
                    finished_at: context.observed_at,
                    budget: OperationBudgetUsage::default(),
                }
            },
            label = "usecases.primitives.affected_file_tests"
        ))
    }
}
