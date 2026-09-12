//! `tracedecay_test_risk` and `tracedecay_test_map`.

use std::collections::VecDeque;

use super::*;
use tracedecay_code_index::{is_test_file, is_test_marker};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

const MAX_TEST_MAP_FILE_SYMBOLS: usize = 50_000;
const MAX_TEST_MAP_IMPACT_SYMBOLS: usize = 20_000;
const MAX_TEST_MAP_RELATIONS_PER_HOP: usize = 20_000;

#[hotpath::measure(label = "mcp.health.test_risk.total")]
pub async fn handle_test_risk(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.min(200) as usize);
    let path_prefix = effective_path(&args, scope_prefix);
    let include_tested = args
        .get("include_tested")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let report = hotpath::future!(
        tracedecay_graph_query::test_risk::analyze_test_risk(
            graph,
            path_prefix,
            include_tested,
            limit,
        ),
        label = "mcp.health.test_risk.graph"
    )
    .await?;
    let output = hotpath::measure_block!(
        "mcp.health.test_risk.assemble",
        serde_json::to_value(report).map_err(|err| TraceDecayError::Config {
            message: format!("failed to serialize test risk report: {err}")
        })?
    );

    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        vec![],
    ))
}

#[hotpath::measure(label = "mcp.health.test_map.total")]
pub async fn handle_test_map(
    graph: &VerifiedGraphQuery,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let source_nodes = hotpath::measure_block!("mcp.health.test_map.graph", {
        match test_map_target(&args)? {
            TestMapTarget::File(file) => {
                let nodes = graph.symbols_in_logical_file(file, MAX_TEST_MAP_FILE_SYMBOLS + 1)?;
                if nodes.len() > MAX_TEST_MAP_FILE_SYMBOLS {
                    return Err(test_map_unavailable(
                        "verified test-map file census exceeded its symbol budget",
                    ));
                }
                nodes
            }
            TestMapTarget::NodeId(node_id) => {
                let occurrence = SymbolOccurrenceId::new(node_id.to_owned()).map_err(|error| {
                    TraceDecayError::Config {
                        message: format!("invalid test-map symbol occurrence: {error}"),
                    }
                })?;
                graph.symbol_summary(&occurrence)?.into_iter().collect()
            }
        }
    });

    let test_callers = batched_test_callers(graph, &source_nodes)?;

    let (coverage_map, uncovered, all_test_files) =
        hotpath::measure_block!("mcp.health.test_map.compute", {
            let mut coverage_map: Vec<Value> = Vec::new();
            let mut uncovered: Vec<Value> = Vec::new();
            let mut all_test_files: HashSet<String> = HashSet::new();

            for node in &source_nodes {
                let (metadata, source_file) =
                    tracedecay_graph_query::test_risk::verified_test_symbol_parts(node)?;
                if !NodeKind::from_str(&metadata.kind).is_some_and(|kind| kind.is_callable_kind()) {
                    continue;
                }
                let mut mapped_callers = Vec::new();
                for (caller, depth) in test_callers.get(&node.occurrence).into_iter().flatten() {
                    let (caller_metadata, caller_file) =
                        tracedecay_graph_query::test_risk::verified_test_symbol_parts(caller)?;
                    all_test_files.insert(caller_file.to_owned());
                    mapped_callers.push(json!({
                        "test_name": caller_metadata.simple_name,
                        "test_file": caller_file,
                        "test_line": caller_metadata.start_line.saturating_add(1),
                        "attribution_depth": depth,
                    }));
                }

                if mapped_callers.is_empty() {
                    uncovered.push(json!({
                        "id": node.occurrence.as_str(),
                        "name": metadata.simple_name,
                        "file": source_file,
                        "line": metadata.start_line.saturating_add(1),
                    }));
                } else {
                    coverage_map.push(json!({
                        "source_name": metadata.simple_name,
                        "source_id": node.occurrence.as_str(),
                        "source_file": source_file,
                        "source_line": metadata.start_line.saturating_add(1),
                        "tests": mapped_callers,
                    }));
                }
            }
            (coverage_map, uncovered, all_test_files)
        });

    let output = hotpath::measure_block!("mcp.health.test_map.assemble", {
        let mut test_file_list: Vec<String> = all_test_files.into_iter().collect();
        test_file_list.sort();
        json!({
            "covered_symbols": coverage_map.len(),
            "uncovered_symbols": uncovered.len(),
            "test_files": test_file_list,
            "coverage": coverage_map,
            "uncovered": uncovered,
        })
    });

    let touched_files = source_nodes
        .iter()
        .map(tracedecay_graph_query::test_risk::verified_test_symbol_parts)
        .collect::<Result<Vec<_>>>()?;
    let touched_files = unique_file_paths(touched_files.into_iter().map(|(_, file)| file));
    Ok(generic_tool_result(
        Some(graph.project_root()?),
        &args,
        &output,
        touched_files,
    ))
}

/// Expands callers for every requested symbol together. Each graph hop is one
/// batched adjacency read, and the small induced caller graph retains enough
/// provenance to map each reached test back to every source it covers. This
/// keeps a file-scoped request proportional to its caller closure instead of
/// hydrating the whole repository and then repeating the same walk per symbol.
fn batched_test_callers(
    graph: &VerifiedGraphQuery,
    source_nodes: &[tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1],
) -> Result<
    HashMap<
        SymbolOccurrenceId,
        Vec<(
            tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1,
            u32,
        )>,
    >,
> {
    const DEPTH: u32 = 3;
    let sources = source_nodes
        .iter()
        .filter_map(|node| {
            node.metadata
                .as_ref()
                .and_then(|metadata| NodeKind::from_str(&metadata.kind))
                .filter(NodeKind::is_callable_kind)
                .map(|_| node.occurrence.clone())
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Ok(HashMap::new());
    }

    let mut seen = sources.iter().cloned().collect::<HashSet<_>>();
    let mut frontier = sources.clone();
    let mut callers_by_callee: HashMap<SymbolOccurrenceId, Vec<SymbolOccurrenceId>> =
        HashMap::new();
    let mut summaries = source_nodes
        .iter()
        .map(|node| (node.occurrence.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    for _ in 0..DEPTH {
        let batches = graph.callers(
            &frontier,
            &[RelationEdgeKindV1::Calls],
            MAX_TEST_MAP_RELATIONS_PER_HOP,
        )?;
        let mut next = Vec::new();
        for (callee, edges) in frontier.iter().zip(batches) {
            let callers = callers_by_callee.entry(callee.clone()).or_default();
            for edge in edges {
                let caller = edge.neighbor;
                callers.push(caller.occurrence.clone());
                if seen.insert(caller.occurrence.clone()) {
                    if seen.len().saturating_sub(sources.len()) > MAX_TEST_MAP_IMPACT_SYMBOLS {
                        return Err(test_map_unavailable(
                            "verified test-map caller expansion exceeded its symbol budget",
                        ));
                    }
                    next.push(caller.occurrence.clone());
                    summaries.insert(caller.occurrence.clone(), caller);
                }
            }
            callers.sort();
            callers.dedup();
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    // Retain the fourth-hop edges in the induced graph. Completeness is a
    // per-source property: a node reached early from one source can still sit
    // at depth three from another, so a global `seen` set cannot decide it.
    if !frontier.is_empty() {
        let batches = graph.callers(
            &frontier,
            &[RelationEdgeKindV1::Calls],
            MAX_TEST_MAP_RELATIONS_PER_HOP,
        )?;
        for (callee, edges) in frontier.iter().zip(batches) {
            let callers = callers_by_callee.entry(callee.clone()).or_default();
            callers.extend(edges.into_iter().map(|edge| edge.neighbor.occurrence));
            callers.sort();
            callers.dedup();
        }
    }

    let reached = summaries.keys().cloned().collect::<Vec<_>>();
    let mut annotated = HashSet::new();
    if !reached.is_empty() {
        for (target, edges) in reached.iter().zip(graph.callers(
            &reached,
            &[RelationEdgeKindV1::Annotates],
            MAX_TEST_MAP_RELATIONS_PER_HOP,
        )?) {
            if edges
                .iter()
                .any(|edge| edge.neighbor.metadata.as_ref().is_some_and(is_test_marker))
            {
                annotated.insert(target.clone());
            }
        }
    }

    let mut tests = HashSet::new();
    for (occurrence, summary) in &summaries {
        let (_, file) = tracedecay_graph_query::test_risk::verified_test_symbol_parts(summary)?;
        if is_test_file(file) || annotated.contains(occurrence) {
            tests.insert(occurrence.clone());
        }
    }
    map_reached_tests(&sources, &callers_by_callee, &summaries, &tests).map_err(|()| {
        test_map_unavailable("verified test-map caller expansion exceeded its budget")
    })
}

fn map_reached_tests(
    sources: &[SymbolOccurrenceId],
    callers_by_callee: &HashMap<SymbolOccurrenceId, Vec<SymbolOccurrenceId>>,
    summaries: &HashMap<
        SymbolOccurrenceId,
        tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1,
    >,
    tests: &HashSet<SymbolOccurrenceId>,
) -> std::result::Result<
    HashMap<
        SymbolOccurrenceId,
        Vec<(
            tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1,
            u32,
        )>,
    >,
    (),
> {
    const DEPTH: u32 = 3;
    let mut mapped = HashMap::new();
    for source in sources {
        let mut queue = VecDeque::from([(source.clone(), 0_u32)]);
        let mut visited = HashSet::from([source.clone()]);
        let mut callers = Vec::new();
        while let Some((callee, depth)) = queue.pop_front() {
            if depth == DEPTH {
                if callers_by_callee
                    .get(&callee)
                    .into_iter()
                    .flatten()
                    .any(|caller| !visited.contains(caller))
                {
                    return Err(());
                }
                continue;
            }
            let caller_depth = depth + 1;
            for caller in callers_by_callee.get(&callee).into_iter().flatten() {
                if !visited.insert(caller.clone()) {
                    continue;
                }
                if tests.contains(caller)
                    && let Some(summary) = summaries.get(caller)
                {
                    callers.push((summary.clone(), caller_depth));
                }
                queue.push_back((caller.clone(), caller_depth));
            }
        }
        callers.sort_by(|(left, left_depth), (right, right_depth)| {
            left_depth
                .cmp(right_depth)
                .then_with(|| left.occurrence.cmp(&right.occurrence))
        });
        mapped.insert(source.clone(), callers);
    }
    Ok(mapped)
}

fn test_map_unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-test-evidence-unavailable", false, detail)
}

fn test_map_target(args: &Value) -> Result<TestMapTarget<'_>> {
    if let Some(file) = args.get("file").and_then(Value::as_str) {
        Ok(TestMapTarget::File(file))
    } else if let Some(node_id) = args
        .get("node_id")
        .or(args.get("id"))
        .and_then(Value::as_str)
    {
        Ok(TestMapTarget::NodeId(node_id))
    } else {
        Err(TraceDecayError::Config {
            message: "missing required parameter: 'file' or 'node_id'".to_string(),
        })
    }
}

#[derive(Debug)]
enum TestMapTarget<'a> {
    File(&'a str),
    NodeId(&'a str),
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{map_reached_tests, test_map_target, test_map_unavailable};
    use serde_json::json;
    use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
    use tracedecay_domain::SymbolOccurrenceId;

    fn occurrence(value: &str) -> SymbolOccurrenceId {
        SymbolOccurrenceId::new(value.to_owned()).expect("valid fixture occurrence")
    }

    fn summary(value: &str) -> CodeGraphSymbolSummaryV1 {
        CodeGraphSymbolSummaryV1 {
            occurrence: occurrence(value),
            binding: None,
            metadata: None,
        }
    }

    #[test]
    fn test_map_requires_file_or_node_id() {
        let error = test_map_target(&json!({})).expect_err("selector is required");
        assert!(
            error.to_string().contains("file") && error.to_string().contains("node_id"),
            "{error}"
        );
    }

    #[test]
    fn test_map_budget_exhaustion_is_a_typed_project_route() {
        let error =
            test_map_unavailable("verified test-map file census exceeded its symbol budget");
        assert_eq!(
            error
                .project_route_context()
                .map(|(reason, retryable, _)| (reason, retryable)),
            Some(("verified-test-evidence-unavailable", false))
        );
    }

    #[test]
    fn shared_batched_caller_maps_to_every_source_at_shortest_depth() {
        let first = occurrence("symbol.source.first");
        let second = occurrence("symbol.source.second");
        let helper = occurrence("symbol.helper");
        let test = occurrence("symbol.test");
        let adjacency = HashMap::from([
            (first.clone(), vec![helper.clone()]),
            (second.clone(), vec![helper.clone()]),
            (helper.clone(), vec![test.clone()]),
        ]);
        let summaries = HashMap::from([
            (helper.clone(), summary(helper.as_str())),
            (test.clone(), summary(test.as_str())),
        ]);
        let mapped = map_reached_tests(
            &[first.clone(), second.clone()],
            &adjacency,
            &summaries,
            &HashSet::from([test.clone()]),
        )
        .expect("complete induced graph");

        for source in [first, second] {
            let callers = mapped.get(&source).expect("source mapping");
            assert_eq!(callers.len(), 1);
            assert_eq!(callers[0].0.occurrence, test);
            assert_eq!(callers[0].1, 2);
        }
    }

    #[test]
    fn cross_source_shortcut_does_not_hide_a_depth_limited_source() {
        let first = occurrence("symbol.source.first");
        let second = occurrence("symbol.source.second");
        let one = occurrence("symbol.helper.one");
        let two = occurrence("symbol.helper.two");
        let three = occurrence("symbol.helper.three");
        let test = occurrence("symbol.test");
        let adjacency = HashMap::from([
            (first.clone(), vec![one.clone()]),
            (one.clone(), vec![two.clone()]),
            (two.clone(), vec![three.clone()]),
            (three.clone(), vec![test.clone()]),
            (second.clone(), vec![test.clone()]),
        ]);
        let summaries = [one, two, three, test.clone()]
            .into_iter()
            .map(|occurrence| (occurrence.clone(), summary(occurrence.as_str())))
            .collect();

        assert_eq!(
            map_reached_tests(
                &[first, second],
                &adjacency,
                &summaries,
                &HashSet::from([test]),
            ),
            Err(())
        );
    }
}
