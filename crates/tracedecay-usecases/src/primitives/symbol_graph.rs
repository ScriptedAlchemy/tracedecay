use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
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
use tracedecay_temporal_query::ports::TemporalExecutionSnapshot;

const MAX_COMPATIBILITY_RESULTS: usize = 500;
const MAX_IMPLEMENTATION_RESULTS: usize = 200;

pub type SymbolGraphCursorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PrimitiveFailure>> + Send + 'a>>;

/// A read's exclusive hold on one graph generation.
///
/// The claim carries the snapshot identity that was live when the read began,
/// so every page the read serves — and every continuation it mints — is
/// answered under that one generation or refused as stale. A page may never be
/// served under a generation other than the one its claim was minted against.
#[derive(Debug)]
pub struct SymbolGraphPageClaim {
    pub(super) snapshot: TemporalExecutionSnapshot,
    pub(super) offset: usize,
}

impl SymbolGraphPageClaim {
    /// Offset into the claimed generation's result set at which this page
    /// starts. Zero for a first page; the resumed cursor's offset otherwise.
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

/// Adapter into the existing authenticated opaque-cursor authority. This
/// module owns no cursor encoding, keyring, expiry, or resume logic.
///
/// Paging is two-phase on purpose. [`Self::claim_page`] binds the read to the
/// generation that is live before any row is read and resolves the resume
/// offset against it; [`Self::finish_page`] re-reads the live generation and
/// refuses to emit the page's continuation if it moved. Without the second
/// phase a cursor could be handed out for a page-set that no longer exists.
pub trait SymbolGraphCursorPort: Send + Sync {
    fn claim_page<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        cursor: Option<&'a OpaqueCursor>,
        observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, SymbolGraphPageClaim>;

    fn finish_page<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        claim: &'a SymbolGraphPageClaim,
        next_offset: usize,
        total: usize,
        has_more: bool,
        observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, Option<OpaqueCursor>>;
}

impl<T> SymbolGraphCursorPort for Arc<T>
where
    T: SymbolGraphCursorPort + ?Sized,
{
    fn claim_page<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        cursor: Option<&'a OpaqueCursor>,
        observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, SymbolGraphPageClaim> {
        (**self).claim_page(context, lane, cursor, observed_at)
    }

    fn finish_page<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        claim: &'a SymbolGraphPageClaim,
        next_offset: usize,
        total: usize,
        has_more: bool,
        observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, Option<OpaqueCursor>> {
        (**self).finish_page(
            context,
            lane,
            claim,
            next_offset,
            total,
            has_more,
            observed_at,
        )
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
            let claim = match claim_generation(&self.cursors, context, &request.meta.page, "search")
                .await
            {
                Ok(claim) => claim,
                Err(failure) => return failed_with(context, failure),
            };
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
                &claim,
                records,
                gaps,
                None,
            )
            .await
        })
    }

    fn exact_symbol<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ExactSymbolRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let claim =
                match claim_generation(&self.cursors, context, &request.meta.page, "exact").await {
                    Ok(claim) => claim,
                    Err(failure) => return failed_with(context, failure),
                };
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
                &claim,
                records,
                gaps,
                None,
            )
            .await
        })
    }

    fn signature_search<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a SignatureSearchRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let claim =
                match claim_generation(&self.cursors, context, &request.meta.page, "signature")
                    .await
                {
                    Ok(claim) => claim,
                    Err(failure) => return failed_with(context, failure),
                };
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
                &claim,
                records,
                Vec::new(),
                None,
            )
            .await
        })
    }

    fn implementations<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ImplementationsRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let claim = match claim_generation(
                &self.cursors,
                context,
                &request.meta.page,
                "implementations",
            )
            .await
            {
                Ok(claim) => claim,
                Err(failure) => return failed_with(context, failure),
            };
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
                &claim,
                records,
                Vec::new(),
                None,
            )
            .await
        })
    }

    fn type_hierarchy<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a TypeHierarchyRequest,
    ) -> SymbolGraphPortFuture<'a, TypeHierarchyRecord> {
        Box::pin(async move {
            let claim =
                match claim_generation(&self.cursors, context, &request.meta.page, "hierarchy")
                    .await
                {
                    Ok(claim) => claim,
                    Err(failure) => return failed_with(context, failure),
                };
            let root = match self.graph.get_node(&request.node_id).await {
                Ok(Some(node)) if in_scope(&node, &request.scope) => node,
                Ok(_) => {
                    return complete_or_failed(
                        &self.cursors,
                        context,
                        &request.meta.page,
                        "hierarchy",
                        &claim,
                        Vec::new(),
                        Vec::new(),
                        None,
                    )
                    .await;
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
                &claim,
                records,
                Vec::new(),
                None,
            )
            .await
        })
    }

    fn callers<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let claim =
                match claim_generation(&self.cursors, context, &request.meta.page, "callers").await
                {
                    Ok(claim) => claim,
                    Err(failure) => return failed_with(context, failure),
                };
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
                &claim,
                records,
                Vec::new(),
                None,
            )
            .await
        })
    }

    fn callees<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord> {
        Box::pin(async move {
            let claim =
                match claim_generation(&self.cursors, context, &request.meta.page, "callees").await
                {
                    Ok(claim) => claim,
                    Err(failure) => return failed_with(context, failure),
                };
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
                &claim,
                records,
                Vec::new(),
                None,
            )
            .await
        })
    }

    fn impact<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphImpactPrimitiveRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord> {
        Box::pin(async move {
            let claim = match claim_generation(&self.cursors, context, &request.meta.page, "impact")
                .await
            {
                Ok(claim) => claim,
                Err(failure) => return failed_with(context, failure),
            };
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
                &claim,
                records,
                Vec::new(),
                Some(edge_count),
            )
            .await
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

/// Binds a read to the live graph generation before any row is read, and
/// resolves the resume offset of an incoming cursor against that same
/// generation. Claiming first is what makes a mid-read generation change
/// observable at all: the identity the page-set came from is recorded before
/// the rows are gathered rather than re-derived after they are.
async fn claim_generation(
    cursors: &dyn SymbolGraphCursorPort,
    context: SymbolGraphPortContext<'_>,
    request: &PageRequest,
    lane: &str,
) -> Result<SymbolGraphPageClaim, PrimitiveFailure> {
    cursors
        .claim_page(
            context.request,
            lane,
            request.cursor.as_ref(),
            context.observed_at,
        )
        .await
}

async fn complete_or_failed<T: Send>(
    cursors: &dyn SymbolGraphCursorPort,
    context: SymbolGraphPortContext<'_>,
    request: &PageRequest,
    lane: &str,
    claim: &SymbolGraphPageClaim,
    items: Vec<T>,
    gaps: Vec<PrimitiveSupportGap>,
    related_edge_count: Option<u64>,
) -> SymbolGraphPortOutcome<T> {
    let mut page = match paginate(cursors, context, request, lane, claim, items).await {
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

async fn paginate<T: Send>(
    cursors: &dyn SymbolGraphCursorPort,
    context: SymbolGraphPortContext<'_>,
    request: &PageRequest,
    lane: &str,
    claim: &SymbolGraphPageClaim,
    items: Vec<T>,
) -> Result<SymbolGraphPage<T>, PrimitiveFailure> {
    let offset = claim.offset();
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
    // Re-reads the live generation and refuses the page if it moved while the
    // rows above were being gathered, so a continuation is never minted for a
    // page-set the caller can no longer be served.
    let next_cursor = cursors
        .finish_page(
            context.request,
            lane,
            claim,
            end,
            total,
            has_more,
            context.observed_at,
        )
        .await?;
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
    failed_with(
        context,
        primitive_failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.query-unavailable",
            reason,
        ),
    )
}

/// Surfaces a typed cursor failure — notably the stale answer a superseded
/// generation produces — without flattening it into the generic unavailable
/// reason [`failed`] carries.
fn failed_with<T>(
    context: SymbolGraphPortContext<'_>,
    failure: PrimitiveFailure,
) -> SymbolGraphPortOutcome<T> {
    SymbolGraphPortOutcome::Failed {
        failure,
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
