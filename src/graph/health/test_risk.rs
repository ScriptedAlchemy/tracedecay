use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

const ATTRIBUTION_DEPTH: usize = 3;
const MAX_TEST_RISK_SYMBOLS: usize = 500_000;
const MAX_TEST_RISK_RELATIONS: usize = 2_000_000;

#[derive(Debug, Serialize)]
pub struct TestRiskReport {
    pub risks: Vec<TestRiskEntry>,
    pub summary: TestRiskSummary,
}

#[derive(Debug, Serialize)]
pub struct TestRiskEntry {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub complexity: u32,
    pub fan_in: usize,
    pub has_test: bool,
    pub attribution_method: &'static str,
    pub attribution_depth: Option<usize>,
    pub risk: f64,
    pub churn: usize,
}

#[derive(Debug, Serialize)]
pub struct TestRiskSummary {
    pub total_functions: usize,
    pub tested: usize,
    pub skipped: usize,
    pub coverage_pct: f64,
    pub top_risk_untested: String,
    pub top_risk_unattributed: String,
    pub attribution: TestRiskAttributionSummary,
    pub buckets: TestRiskBucketSummary,
    pub confidence: &'static str,
    pub confidence_note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TestRiskAttributionSummary {
    pub depth: usize,
    pub direct_unit_attributed: usize,
    pub closure_attributed: usize,
    pub trait_resolved_attributed: usize,
    pub public_api_attributed: usize,
    pub cli_entry_attributed: usize,
    pub total_attributed: usize,
}

#[derive(Debug, Serialize)]
pub struct TestRiskBucketSummary {
    pub attributed: usize,
    pub reachable_unattributed: usize,
    pub orphan_entry: usize,
    pub excluded: usize,
}

struct RiskEntry {
    id: String,
    name: String,
    file: String,
    line: u32,
    complexity: u32,
    fan_in: usize,
    attribution_method: TestAttributionMethod,
    attribution_depth: Option<usize>,
    risk: f64,
    churn: usize,
}

impl RiskEntry {
    fn has_test(&self) -> bool {
        self.attribution_method != TestAttributionMethod::None
    }

    fn into_public(self) -> TestRiskEntry {
        let has_test = self.has_test();
        TestRiskEntry {
            id: self.id,
            name: self.name,
            file: self.file,
            line: self.line,
            complexity: self.complexity,
            fan_in: self.fan_in,
            has_test,
            attribution_method: self.attribution_method.as_str(),
            attribution_depth: self.attribution_depth,
            risk: (self.risk * 100.0).round() / 100.0,
            churn: self.churn,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestAttributionMethod {
    None,
    DirectUnit,
    Closure,
}

impl TestAttributionMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DirectUnit => "direct_unit",
            Self::Closure => "closure",
        }
    }

    fn risk_multiplier(self) -> f64 {
        match self {
            Self::None => 1.0,
            Self::DirectUnit => 0.1,
            Self::Closure => 0.4,
        }
    }
}

pub(crate) async fn analyze_test_risk(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    path_prefix: Option<&str>,
    include_tested: bool,
    limit: usize,
) -> Result<TestRiskReport> {
    let evidence = verified_test_evidence(graph)?;
    let eligible_fns: Vec<_> = evidence
        .symbols
        .iter()
        .filter(|n| {
            n.callable
                && !crate::tracedecay::is_test_file(&n.file)
                && !n.name.starts_with("test_")
                && !n.name.starts_with("test")
                && !n.file.contains("/test")
                && !evidence.test_annotated.contains(n.occurrence.as_str())
                && !n.skip_test_coverage
                && !n.qualified_name.contains("::tests::")
        })
        .filter(|n| crate::path_scope::path_matches_scope(&n.file, path_prefix))
        .collect();

    let excluded_count = eligible_fns
        .iter()
        .filter(|n| !n.file.starts_with("src/"))
        .count();
    let source_fns: Vec<_> = eligible_fns
        .iter()
        .copied()
        .filter(|n| n.file.starts_with("src/"))
        .collect();

    let mut fan_in: HashMap<String, usize> = HashMap::new();
    for (_, target) in &evidence.calls {
        *fan_in.entry(target.clone()).or_insert(0) += 1;
    }
    let attribution_depths = build_test_attribution_depths(
        &evidence.calls,
        &evidence.files,
        &evidence.test_annotated,
        ATTRIBUTION_DEPTH,
    );

    let total_functions = source_fns.len();
    let attributed_count = source_fns
        .iter()
        .filter(|n| attribution_depths.contains_key(n.occurrence.as_str()))
        .count();
    let direct_unit_attributed = source_fns
        .iter()
        .filter(|n| attribution_depths.get(n.occurrence.as_str()).copied() == Some(1))
        .count();
    let closure_attributed = source_fns
        .iter()
        .filter(|n| {
            attribution_depths
                .get(n.occurrence.as_str())
                .is_some_and(|depth| *depth >= 2)
        })
        .count();
    let skipped_count = evidence
        .symbols
        .iter()
        .filter(|n| {
            n.callable
                && n.skip_test_coverage
                && !crate::tracedecay::is_test_file(&n.file)
                && !n.qualified_name.contains("::tests::")
        })
        .count();

    let mut risks: Vec<RiskEntry> = source_fns
        .iter()
        .map(|n| {
            let complexity = n.complexity;
            let attribution_depth = attribution_depths.get(n.occurrence.as_str()).copied();
            let attribution_method = classify_test_attribution(attribution_depth);
            let fan_in = *fan_in.get(n.occurrence.as_str()).unwrap_or(&0);
            let risk = (f64::from(complexity) + 1.0)
                * (fan_in as f64 + 1.0)
                * attribution_method.risk_multiplier();
            RiskEntry {
                id: n.occurrence.as_str().to_owned(),
                name: n.name.clone(),
                file: n.file.clone(),
                line: n.line,
                complexity,
                fan_in,
                attribution_method,
                attribution_depth,
                risk,
                churn: 0,
            }
        })
        .filter(|risk| include_tested || !risk.has_test())
        .collect();

    let churn_map = crate::graph::git::file_churn(cg.project_root(), 90).await?;
    for risk in &mut risks {
        let churn = churn_map.get(&risk.file).copied().unwrap_or(0);
        risk.churn = churn;
        if churn > 0 {
            risk.risk *= (churn as f64 + 1.0).log2();
        }
    }
    risks.sort_by(|a, b| {
        b.risk
            .partial_cmp(&a.risk)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top_risk_untested = risks
        .iter()
        .find(|risk| !risk.has_test())
        .map(|risk| risk.name.clone())
        .unwrap_or_default();
    let reachable_unattributed = source_fns
        .iter()
        .filter(|n| {
            !attribution_depths.contains_key(n.occurrence.as_str())
                && fan_in.get(n.occurrence.as_str()).copied().unwrap_or(0) > 0
        })
        .count();
    let orphan_entry = source_fns
        .iter()
        .filter(|n| {
            !attribution_depths.contains_key(n.occurrence.as_str())
                && fan_in.get(n.occurrence.as_str()).copied().unwrap_or(0) == 0
        })
        .count();
    let coverage_pct = if total_functions == 0 {
        0.0
    } else {
        (attributed_count as f64 / total_functions as f64 * 100.0).round()
    };

    risks.truncate(limit);
    Ok(TestRiskReport {
        risks: risks.into_iter().map(RiskEntry::into_public).collect(),
        summary: TestRiskSummary {
            total_functions,
            tested: attributed_count,
            skipped: skipped_count,
            coverage_pct,
            top_risk_untested: top_risk_untested.clone(),
            top_risk_unattributed: top_risk_untested,
            attribution: TestRiskAttributionSummary {
                depth: ATTRIBUTION_DEPTH,
                direct_unit_attributed,
                closure_attributed,
                trait_resolved_attributed: 0,
                public_api_attributed: 0,
                cli_entry_attributed: 0,
                total_attributed: attributed_count,
            },
            buckets: TestRiskBucketSummary {
                attributed: attributed_count,
                reachable_unattributed,
                orphan_entry,
                excluded: excluded_count,
            },
            confidence: "static_lower_bound",
            confidence_note: "coverage_pct is a depth-3 static attribution lower bound over the admitted generation; complexity uses extraction-attested branches, loops, and maximum nesting; direct_unit is strongest, while closure retains higher residual risk.",
        },
    })
}

struct VerifiedTestSymbol {
    occurrence: SymbolOccurrenceId,
    name: String,
    qualified_name: String,
    file: String,
    line: u32,
    complexity: u32,
    callable: bool,
    skip_test_coverage: bool,
}

pub(crate) struct VerifiedTestEvidence {
    symbols: Vec<VerifiedTestSymbol>,
    calls: Vec<(String, String)>,
    files: HashMap<String, String>,
    pub(crate) test_annotated: HashSet<String>,
}

pub(crate) fn verified_test_evidence(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
) -> Result<VerifiedTestEvidence> {
    let page = graph.symbols_page(None, MAX_TEST_RISK_SYMBOLS)?;
    if page.has_more {
        return Err(test_risk_graph_problem(
            "verified test-risk symbol census exceeded its budget",
        ));
    }
    let occurrences = page
        .symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let mut symbols = Vec::with_capacity(page.symbols.len());
    let mut files = HashMap::with_capacity(page.symbols.len());
    let mut test_markers = HashSet::new();
    for symbol in page.symbols {
        let (metadata, file) = verified_test_symbol_parts(&symbol)?;
        if metadata.kind == "annotation_usage"
            && matches!(
                metadata.simple_name.as_str(),
                "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
            )
        {
            test_markers.insert(symbol.occurrence.clone());
        }
        files.insert(symbol.occurrence.as_str().to_owned(), file.to_owned());
        symbols.push(VerifiedTestSymbol {
            occurrence: symbol.occurrence.clone(),
            name: metadata.simple_name.clone(),
            qualified_name: metadata.qualified_name.clone(),
            file: file.to_owned(),
            line: metadata.start_line.saturating_add(1),
            complexity: metadata
                .branches
                .saturating_add(metadata.loops)
                .saturating_add(metadata.max_nesting),
            callable: NodeKind::from_str(&metadata.kind)
                .is_some_and(|kind| kind.is_callable_kind()),
            skip_test_coverage: metadata.skip_test_coverage,
        });
    }
    let edges = graph.edges_among(
        &occurrences,
        &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Annotates],
        MAX_TEST_RISK_RELATIONS,
    )?;
    let mut calls = Vec::new();
    let mut test_annotated = HashSet::new();
    for edge in edges {
        match edge.edge.kind {
            RelationEdgeKindV1::Calls => calls.push((
                edge.edge.from_occurrence.as_str().to_owned(),
                edge.edge.to_occurrence.as_str().to_owned(),
            )),
            RelationEdgeKindV1::Annotates if test_markers.contains(&edge.edge.from_occurrence) => {
                test_annotated.insert(edge.edge.to_occurrence.as_str().to_owned());
            }
            _ => {}
        }
    }
    Ok(VerifiedTestEvidence {
        symbols,
        calls,
        files,
        test_annotated,
    })
}

pub(crate) fn verified_test_symbol_parts(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<(&tracedecay_code_index::lineage::LineageSymbolRecordV1, &str)> {
    let metadata = symbol.metadata.as_ref().ok_or_else(|| {
        test_risk_graph_problem("verified test evidence is missing extraction metadata")
    })?;
    let file = symbol
        .binding
        .as_ref()
        .and_then(|binding| binding.logical_path.as_deref())
        .ok_or_else(|| {
            test_risk_graph_problem("verified test evidence is missing a file binding")
        })?;
    Ok((metadata, file))
}

fn test_risk_graph_problem(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-test-evidence-unavailable", false, detail)
}

fn build_test_attribution_depths(
    calls: &[(String, String)],
    node_to_file: &HashMap<String, String>,
    test_annotated_callers: &HashSet<String>,
    max_depth: usize,
) -> HashMap<String, usize> {
    let mut outgoing_calls: HashMap<String, Vec<String>> = HashMap::new();
    let mut seed_nodes: HashSet<String> = HashSet::new();

    for (source, target) in calls {
        outgoing_calls
            .entry(source.clone())
            .or_default()
            .push(target.clone());
        let is_test_seed = node_to_file
            .get(source)
            .is_some_and(|file| crate::tracedecay::is_test_file(file))
            || test_annotated_callers.contains(source);
        if is_test_seed {
            seed_nodes.insert(source.clone());
        }
    }

    let mut reached_depths: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = seed_nodes
        .into_iter()
        .map(|node_id| (node_id, 0usize))
        .collect();
    let mut best_seen: HashMap<String, usize> = queue.iter().cloned().collect();

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let next_depth = depth + 1;
        for target in outgoing_calls.get(&node_id).into_iter().flatten() {
            let should_visit = best_seen
                .get(target)
                .is_none_or(|seen_depth| next_depth < *seen_depth);
            if !should_visit {
                continue;
            }
            best_seen.insert(target.clone(), next_depth);
            reached_depths
                .entry(target.clone())
                .and_modify(|existing| *existing = (*existing).min(next_depth))
                .or_insert(next_depth);
            queue.push_back((target.clone(), next_depth));
        }
    }

    reached_depths
}

fn classify_test_attribution(depth: Option<usize>) -> TestAttributionMethod {
    match depth {
        Some(1) => TestAttributionMethod::DirectUnit,
        Some(depth) if depth >= 2 => TestAttributionMethod::Closure,
        None | Some(_) => TestAttributionMethod::None,
    }
}
