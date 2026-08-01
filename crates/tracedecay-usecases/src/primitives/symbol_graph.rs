use std::collections::HashSet;
use std::sync::Arc;

use tracedecay_application::retrieval::{
    ExactSymbolRequest, GraphImpactPrimitiveRequest, GraphRelationRequest, ImplementationSelector,
    ImplementationsRequest, PrimitiveFailure, PrimitiveFailureKind, PrimitiveSupportGap,
    SignatureSearchRequest, SymbolGraphPage, SymbolGraphPortContext, SymbolGraphPortFuture,
    SymbolGraphPortOutcome, SymbolGraphPrimitivePort, SymbolGraphScope, SymbolPrimitiveRecord,
    SymbolRelationRecord, SymbolSearchPrimitiveRequest, TypeHierarchyRecord, TypeHierarchyRequest,
};
use tracedecay_application::{OpaqueCursor, OperationBudgetUsage, PageRequest, RequestContext};
use tracedecay_domain::UtcMicros;

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::types::{EdgeKind, Node, NodeKind};

const MAX_COMPATIBILITY_RESULTS: usize = 500;
const MAX_IMPLEMENTATION_RESULTS: usize = 200;

/// Adapter into the existing authenticated opaque-cursor authority. This
/// module owns no cursor encoding, keyring, expiry, or resume logic.
pub trait SymbolGraphCursorPort: Send + Sync {
    fn resume_offset(
        &self,
        context: &RequestContext,
        lane: &str,
        cursor: &OpaqueCursor,
        observed_at: UtcMicros,
    ) -> Result<usize, PrimitiveFailure>;

    fn issue_cursor(
        &self,
        context: &RequestContext,
        lane: &str,
        next_offset: usize,
        total: usize,
        observed_at: UtcMicros,
    ) -> Result<OpaqueCursor, PrimitiveFailure>;
}

impl<T> SymbolGraphCursorPort for Arc<T>
where
    T: SymbolGraphCursorPort + ?Sized,
{
    fn resume_offset(
        &self,
        context: &RequestContext,
        lane: &str,
        cursor: &OpaqueCursor,
        observed_at: UtcMicros,
    ) -> Result<usize, PrimitiveFailure> {
        (**self).resume_offset(context, lane, cursor, observed_at)
    }

    fn issue_cursor(
        &self,
        context: &RequestContext,
        lane: &str,
        next_offset: usize,
        total: usize,
        observed_at: UtcMicros,
    ) -> Result<OpaqueCursor, PrimitiveFailure> {
        (**self).issue_cursor(context, lane, next_offset, total, observed_at)
    }
}

/// Production adapter from the transport-neutral application primitive family
/// to the existing graph/query authorities owned by [`TraceDecay`].
pub struct CanonicalSymbolGraphAdapter<C> {
    graph: Arc<TraceDecay>,
    cursors: C,
}

impl<C> CanonicalSymbolGraphAdapter<C> {
    pub fn new(graph: Arc<TraceDecay>, cursors: C) -> Self {
        Self { graph, cursors }
    }
}

impl<C> SymbolGraphPrimitivePort for CanonicalSymbolGraphAdapter<C>
where
    C: SymbolGraphCursorPort,
{
    fn symbol_search<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a SymbolSearchPrimitiveRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let Ok(results) = self
                .graph
                .search(request.query.as_str(), MAX_COMPATIBILITY_RESULTS)
                .await
            else {
                return failed(context, "canonical symbol search failed");
            };
            let records = results
                .into_iter()
                .filter(|result| in_scope(&result.node, &request.scope))
                .map(|result| symbol_record(result.node, Some(result.score)))
                .collect();
            let gaps = request
                .lazy_index_ignored_dependencies
                .then(|| {
                    support_gap(
                        Some("ignored-dependency-lazy-index"),
                        None,
                        "lazy dependency indexing remains provider-owned",
                    )
                })
                .into_iter()
                .collect();
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "search",
                records,
                gaps,
                None,
            )
        })
    }

    fn exact_symbol<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ExactSymbolRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let Ok(nodes) = self.graph.get_nodes_by_name(&request.name).await else {
                return failed(context, "exact symbol lookup failed");
            };
            let records = nodes
                .into_iter()
                .filter(|node| in_scope(node, &request.scope))
                .map(|node| symbol_record(node, None))
                .collect();
            let gaps = request
                .lazy_index_ignored_dependencies
                .then(|| {
                    support_gap(
                        Some("ignored-dependency-lazy-index"),
                        None,
                        "lazy dependency indexing remains provider-owned",
                    )
                })
                .into_iter()
                .collect();
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "exact",
                records,
                gaps,
                None,
            )
        })
    }

    fn signature_search<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a SignatureSearchRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let Ok(functions) = self.graph.db().get_nodes_by_kind(NodeKind::Function).await else {
                return failed(context, "signature function lookup failed");
            };
            let Ok(methods) = self.graph.db().get_nodes_by_kind(NodeKind::Method).await else {
                return failed(context, "signature method lookup failed");
            };

            let mut records = Vec::new();
            for node in functions.into_iter().chain(methods) {
                if !in_scope(&node, &request.scope) || !signature_matches(&node, request) {
                    continue;
                }
                records.push(symbol_record(node, None));
                if records.len() >= MAX_COMPATIBILITY_RESULTS {
                    break;
                }
            }
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "signature",
                records,
                Vec::new(),
                None,
            )
        })
    }

    fn implementations<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ImplementationsRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let records = match &request.selector {
                ImplementationSelector::Trait { name } => {
                    match trait_implementations(self.graph.as_ref(), name, &request.scope).await {
                        Ok(records) => records,
                        Err(()) => return failed(context, "trait implementation lookup failed"),
                    }
                }
                ImplementationSelector::Method { name } => {
                    let Ok(nodes) = self.graph.get_nodes_by_name(name).await else {
                        return failed(context, "method implementation lookup failed");
                    };
                    nodes
                        .into_iter()
                        .filter(|node| {
                            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                                && in_scope(node, &request.scope)
                        })
                        .take(MAX_IMPLEMENTATION_RESULTS)
                        .map(|node| SymbolRelationRecord {
                            symbol: symbol_record(node, None),
                            edge_kind: "implementation".to_owned(),
                            dispatch_via_trait: false,
                            dispatch_from: None,
                            depth: None,
                        })
                        .collect()
                }
            };
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "implementations",
                records,
                Vec::new(),
                None,
            )
        })
    }

    fn type_hierarchy<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a TypeHierarchyRequest,
    ) -> SymbolGraphPortFuture<'a, TypeHierarchyRecord> {
        Box::pin(async move {
            let root = match self.graph.get_node(&request.node_id).await {
                Ok(Some(node)) if in_scope(&node, &request.scope) => node,
                Ok(_) => {
                    return complete_or_failed(
                        &self.cursors,
                        context,
                        &request.meta.page,
                        "hierarchy",
                        Vec::new(),
                        Vec::new(),
                        None,
                    );
                }
                Err(_) => return failed(context, "type hierarchy root lookup failed"),
            };
            let mut records = vec![TypeHierarchyRecord {
                parent_node_id: root.id.clone(),
                symbol: symbol_record(root, None),
                edge_kind: "root".to_owned(),
                depth: 0,
            }];
            let mut seen = HashSet::from([request.node_id.clone()]);
            let mut frontier = vec![(request.node_id.clone(), 0_u32)];

            while let Some((parent_id, depth)) = frontier.pop() {
                if depth >= request.maximum_depth || records.len() >= MAX_COMPATIBILITY_RESULTS {
                    continue;
                }
                let Ok(edges) = self.graph.get_incoming_edges(&parent_id).await else {
                    return failed(context, "type hierarchy traversal failed");
                };
                for edge in edges
                    .into_iter()
                    .filter(|edge| matches!(edge.kind, EdgeKind::Implements | EdgeKind::Extends))
                {
                    if !seen.insert(edge.source.clone()) {
                        continue;
                    }
                    let child = match self.graph.get_node(&edge.source).await {
                        Ok(Some(node)) if in_scope(&node, &request.scope) => node,
                        Ok(_) => continue,
                        Err(_) => return failed(context, "type hierarchy node lookup failed"),
                    };
                    records.push(TypeHierarchyRecord {
                        symbol: symbol_record(child, None),
                        parent_node_id: parent_id.clone(),
                        edge_kind: edge.kind.as_str().to_owned(),
                        depth: depth + 1,
                    });
                    frontier.push((edge.source, depth + 1));
                    if records.len() >= MAX_COMPATIBILITY_RESULTS {
                        break;
                    }
                }
            }

            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "hierarchy",
                records,
                Vec::new(),
                None,
            )
        })
    }

    fn callers<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let Ok(values) = self
                .graph
                .get_callers(&request.node_id, request.maximum_depth as usize)
                .await
            else {
                return failed(context, "caller traversal failed");
            };
            let records = values
                .into_iter()
                .filter(|(node, _)| in_scope(node, &request.scope))
                .take(MAX_COMPATIBILITY_RESULTS)
                .map(|(node, edge)| relation_record(node, edge.kind, false, None))
                .collect();
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "callers",
                records,
                Vec::new(),
                None,
            )
        })
    }

    fn callees<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let Ok(values) = self
                .graph
                .get_callees(&request.node_id, request.maximum_depth as usize)
                .await
            else {
                return failed(context, "callee traversal failed");
            };
            let mut seen = HashSet::new();
            let mut records = Vec::new();
            let mut callee_nodes = Vec::new();
            for (node, edge) in values {
                if !in_scope(&node, &request.scope) || !seen.insert(node.id.clone()) {
                    continue;
                }
                records.push(relation_record(node.clone(), edge.kind, false, None));
                callee_nodes.push(node);
                if records.len() >= MAX_COMPATIBILITY_RESULTS {
                    break;
                }
            }

            if request.resolve_trait_dispatch {
                for callee in callee_nodes {
                    let Ok(targets) = self.graph.get_trait_dispatch_targets(&callee).await else {
                        return failed(context, "trait dispatch resolution failed");
                    };
                    for target in targets {
                        if !in_scope(&target, &request.scope) || !seen.insert(target.id.clone()) {
                            continue;
                        }
                        records.push(relation_record(
                            target,
                            EdgeKind::Calls,
                            true,
                            Some(callee.id.clone()),
                        ));
                        if records.len() >= MAX_COMPATIBILITY_RESULTS {
                            break;
                        }
                    }
                    if records.len() >= MAX_COMPATIBILITY_RESULTS {
                        break;
                    }
                }
            }

            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "callees",
                records,
                Vec::new(),
                None,
            )
        })
    }

    fn impact<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphImpactPrimitiveRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let Ok(subgraph) = self
                .graph
                .get_impact_radius(&request.node_id, request.maximum_depth as usize)
                .await
            else {
                return failed(context, "impact traversal failed");
            };
            let edge_count = subgraph.edges.len() as u64;
            let records = subgraph
                .nodes
                .into_iter()
                .filter(|node| in_scope(node, &request.scope))
                .take(MAX_COMPATIBILITY_RESULTS)
                .map(|node| symbol_record(node, None))
                .collect();
            complete_or_failed(
                &self.cursors,
                context,
                &request.meta.page,
                "impact",
                records,
                Vec::new(),
                Some(edge_count),
            )
        })
    }
}

async fn trait_implementations(
    graph: &TraceDecay,
    name: &str,
    scope: &SymbolGraphScope,
) -> Result<Vec<SymbolRelationRecord>, ()> {
    let candidates = graph
        .db()
        .search_nodes_by_exact_name(&[name.to_owned()], 50)
        .await
        .map_err(|_| ())?;
    let mut records = Vec::new();
    for trait_node in candidates.into_iter().filter(|node| {
        matches!(
            node.kind,
            NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType
        )
    }) {
        let edges = graph
            .db()
            .get_incoming_edges(&trait_node.id, &[EdgeKind::Implements])
            .await
            .map_err(|_| ())?;
        for edge in edges {
            let Some(implementation) = graph.get_node(&edge.source).await.map_err(|_| ())? else {
                continue;
            };
            if !in_scope(&implementation, scope) {
                continue;
            }
            records.push(SymbolRelationRecord {
                symbol: symbol_record(implementation, None),
                edge_kind: edge.kind.as_str().to_owned(),
                dispatch_via_trait: false,
                dispatch_from: Some(trait_node.id.clone()),
                depth: None,
            });
            if records.len() >= MAX_IMPLEMENTATION_RESULTS {
                return Ok(records);
            }
        }
    }
    Ok(records)
}

fn signature_matches(node: &Node, request: &SignatureSearchRequest) -> bool {
    if request
        .is_async
        .is_some_and(|want_async| node.is_async != want_async)
    {
        return false;
    }
    let Some(signature) = node.signature.as_deref() else {
        return false;
    };
    if request
        .returns
        .as_deref()
        .is_some_and(|returns| !return_region(signature).contains(returns))
    {
        return false;
    }
    let parameters = parameter_region(signature);
    request
        .params
        .iter()
        .all(|param| parameters.contains(param))
}

fn parameter_region(signature: &str) -> &str {
    let Some(start) = signature.find('(') else {
        return "";
    };
    let end = signature.rfind(')').unwrap_or(signature.len());
    signature.get(start + 1..end).unwrap_or("")
}

fn return_region(signature: &str) -> &str {
    signature
        .split_once("->")
        .map_or("", |(_, returns)| returns.trim())
}

fn relation_record(
    node: Node,
    edge_kind: EdgeKind,
    dispatch_via_trait: bool,
    dispatch_from: Option<String>,
) -> SymbolRelationRecord {
    SymbolRelationRecord {
        symbol: symbol_record(node, None),
        edge_kind: edge_kind.as_str().to_owned(),
        dispatch_via_trait,
        dispatch_from,
        depth: None,
    }
}

pub(crate) fn symbol_record(node: Node, score: Option<f64>) -> SymbolPrimitiveRecord {
    SymbolPrimitiveRecord {
        node_id: node.id,
        name: node.name,
        qualified_name: node.qualified_name,
        kind: node.kind.as_str().to_owned(),
        file: node.file_path,
        start_line_zero_based: node.start_line,
        end_line_zero_based: node.end_line,
        line: node.start_line.saturating_add(1),
        end_line: node.end_line.saturating_add(1),
        signature: node.signature,
        is_async: node.is_async,
        score,
    }
}

fn in_scope(node: &Node, scope: &SymbolGraphScope) -> bool {
    scope.path_prefix.as_deref().is_none_or(|path_prefix| {
        tracedecay_runtime_core::path_scope::path_matches_scope(&node.file_path, Some(path_prefix))
    })
}

fn complete_or_failed<T>(
    cursors: &dyn SymbolGraphCursorPort,
    context: SymbolGraphPortContext<'_>,
    request: &PageRequest,
    lane: &str,
    items: Vec<T>,
    gaps: Vec<PrimitiveSupportGap>,
    related_edge_count: Option<u64>,
) -> SymbolGraphPortOutcome<T> {
    let mut page = match paginate(cursors, context, request, lane, items) {
        Ok(page) => page,
        Err(failure) => {
            return SymbolGraphPortOutcome::Failed {
                failure,
                finished_at: context.observed_at,
                budget: OperationBudgetUsage::default(),
            };
        }
    };
    page.related_edge_count = related_edge_count;
    page.support_gaps = gaps;
    if page.support_gaps.is_empty() {
        SymbolGraphPortOutcome::Completed {
            page,
            finished_at: context.observed_at,
            budget: OperationBudgetUsage::default(),
        }
    } else {
        SymbolGraphPortOutcome::Partial {
            page,
            finished_at: context.observed_at,
            budget: OperationBudgetUsage::default(),
        }
    }
}

fn paginate<T>(
    cursors: &dyn SymbolGraphCursorPort,
    context: SymbolGraphPortContext<'_>,
    request: &PageRequest,
    lane: &str,
    items: Vec<T>,
) -> Result<SymbolGraphPage<T>, PrimitiveFailure> {
    let offset = match request.cursor.as_ref() {
        Some(cursor) => {
            cursors.resume_offset(context.request, lane, cursor, context.observed_at)?
        }
        None => 0,
    };
    let total = items.len();
    if offset > total {
        return Err(primitive_failure(
            PrimitiveFailureKind::InvalidRequest,
            "application.symbol-graph.cursor-set-mismatch",
            "cursor continuation is outside the frozen result set",
        ));
    }
    let page_size = request.page_size as usize;
    let end = offset.saturating_add(page_size).min(total);
    let has_more = end < total;
    let page_items = items.into_iter().skip(offset).take(page_size).collect();
    let next_cursor = if has_more {
        Some(cursors.issue_cursor(context.request, lane, end, total, context.observed_at)?)
    } else {
        None
    };
    Ok(SymbolGraphPage::complete(
        page_items,
        Some(total as u64),
        next_cursor,
    ))
}

fn failed<T>(
    context: SymbolGraphPortContext<'_>,
    reason: &'static str,
) -> SymbolGraphPortOutcome<T> {
    SymbolGraphPortOutcome::Failed {
        failure: primitive_failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.query-unavailable",
            reason,
        ),
        finished_at: context.observed_at,
        budget: OperationBudgetUsage::default(),
    }
}

fn primitive_failure(
    kind: PrimitiveFailureKind,
    code: &'static str,
    message: &'static str,
) -> PrimitiveFailure {
    PrimitiveFailure::new(kind, code, message)
        .unwrap_or_else(|_| panic!("static primitive failure is valid"))
}

fn support_gap(
    provider: Option<&str>,
    language: Option<&str>,
    reason: &'static str,
) -> PrimitiveSupportGap {
    PrimitiveSupportGap::unsupported(
        provider.map(str::to_owned),
        language.map(str::to_owned),
        reason,
    )
    .unwrap_or_else(|_| panic!("static support gap is valid"))
}
