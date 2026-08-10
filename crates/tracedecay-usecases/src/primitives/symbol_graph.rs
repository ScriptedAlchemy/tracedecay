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
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId, UtcMicros};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::types::NodeKind;
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

    #[allow(clippy::too_many_arguments)]
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
/// to the admitted, generation-pinned graph projection.
pub struct CanonicalSymbolGraphAdapter<C> {
    code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
    cursors: C,
}

impl<C> CanonicalSymbolGraphAdapter<C> {
    pub fn new(code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>, cursors: C) -> Self {
        Self {
            code_graph,
            cursors,
        }
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "canonical symbol search failed");
            };
            let Ok(symbols) = all_symbols(&graph.reader, Arc::clone(&graph.cancellation)) else {
                return failed(context, "canonical symbol search failed");
            };
            let query = request.query.as_str().to_ascii_lowercase();
            let records = symbols
                .into_iter()
                .filter(|symbol| in_scope(symbol, &request.scope))
                .filter(|symbol| {
                    symbol.metadata.as_ref().is_some_and(|metadata| {
                        metadata.simple_name.to_ascii_lowercase().contains(&query)
                            || metadata
                                .qualified_name
                                .to_ascii_lowercase()
                                .contains(&query)
                    })
                })
                .take(MAX_COMPATIBILITY_RESULTS)
                .map(|symbol| symbol_record(symbol, None))
                .collect::<Result<Vec<_>, _>>();
            let Ok(records) = records else {
                return failed(context, "canonical symbol search evidence was incomplete");
            };
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "exact symbol lookup failed");
            };
            let Ok(nodes) = graph.reader.resolve_simple_name(
                &request.name,
                None,
                MAX_COMPATIBILITY_RESULTS,
                Arc::clone(&graph.cancellation),
            ) else {
                return failed(context, "exact symbol lookup failed");
            };
            let records = nodes
                .into_iter()
                .filter(|node| in_scope(node, &request.scope))
                .map(|node| symbol_record(node, None))
                .collect::<Result<Vec<_>, _>>();
            let Ok(records) = records else {
                return failed(context, "exact symbol evidence was incomplete");
            };
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "signature symbol lookup failed");
            };
            let Ok(nodes) = all_symbols(&graph.reader, Arc::clone(&graph.cancellation)) else {
                return failed(context, "signature symbol lookup failed");
            };

            let mut records = Vec::new();
            for node in nodes {
                let Some(metadata) = node.metadata.as_ref() else {
                    return failed(context, "signature symbol evidence was incomplete");
                };
                if !matches!(
                    NodeKind::from_str(&metadata.kind),
                    Some(NodeKind::Function | NodeKind::Method)
                ) {
                    continue;
                }
                if !in_scope(&node, &request.scope) || !signature_matches(&node, request) {
                    continue;
                }
                let Ok(record) = symbol_record(node, None) else {
                    return failed(context, "signature symbol evidence was incomplete");
                };
                records.push(record);
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "implementation lookup failed");
            };
            let records = match &request.selector {
                ImplementationSelector::Trait { name } => {
                    match trait_implementations(
                        &graph.reader,
                        Arc::clone(&graph.cancellation),
                        name,
                        &request.scope,
                    ) {
                        Ok(records) => records,
                        Err(()) => return failed(context, "trait implementation lookup failed"),
                    }
                }
                ImplementationSelector::Method { name } => {
                    let Ok(nodes) = graph.reader.resolve_simple_name(
                        name,
                        None,
                        MAX_IMPLEMENTATION_RESULTS,
                        Arc::clone(&graph.cancellation),
                    ) else {
                        return failed(context, "method implementation lookup failed");
                    };
                    let mut records = Vec::new();
                    for node in nodes {
                        if !node.metadata.as_ref().is_some_and(|metadata| {
                            matches!(
                                NodeKind::from_str(&metadata.kind),
                                Some(NodeKind::Function | NodeKind::Method)
                            )
                        }) || !in_scope(&node, &request.scope)
                        {
                            continue;
                        }
                        let Ok(symbol) = symbol_record(node, None) else {
                            return failed(
                                context,
                                "method implementation evidence was incomplete",
                            );
                        };
                        records.push(SymbolRelationRecord {
                            symbol,
                            edge_kind: "implementation".to_owned(),
                            dispatch_via_trait: false,
                            dispatch_from: None,
                            depth: None,
                        });
                    }
                    records
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "type hierarchy graph admission failed");
            };
            let Ok(root_id) = SymbolOccurrenceId::new(request.node_id.clone()) else {
                return failed(context, "type hierarchy node identity was invalid");
            };
            let root = match graph
                .reader
                .symbol_summary(&root_id, Arc::clone(&graph.cancellation))
            {
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
            let Ok(root_record) = symbol_record(root, None) else {
                return failed(context, "type hierarchy root evidence was incomplete");
            };
            let mut records = vec![TypeHierarchyRecord {
                parent_node_id: root_id.as_str().to_owned(),
                symbol: root_record,
                edge_kind: "root".to_owned(),
                depth: 0,
            }];
            let mut seen = HashSet::from([request.node_id.clone()]);
            let mut frontier = vec![(root_id, 0_u32)];

            while let Some((parent_id, depth)) = frontier.pop() {
                if depth >= request.maximum_depth || records.len() >= MAX_COMPATIBILITY_RESULTS {
                    continue;
                }
                let Ok(edges) = graph.reader.callers(
                    std::slice::from_ref(&parent_id),
                    &[RelationEdgeKindV1::Implements, RelationEdgeKindV1::Extends],
                    MAX_COMPATIBILITY_RESULTS,
                    Arc::clone(&graph.cancellation),
                ) else {
                    return failed(context, "type hierarchy traversal failed");
                };
                for edge in edges.into_iter().flatten() {
                    let child = edge.neighbor;
                    if !seen.insert(child.occurrence.as_str().to_owned()) {
                        continue;
                    }
                    if !in_scope(&child, &request.scope) {
                        continue;
                    }
                    let child_id = child.occurrence.clone();
                    let Ok(symbol) = symbol_record(child, None) else {
                        return failed(context, "type hierarchy node evidence was incomplete");
                    };
                    records.push(TypeHierarchyRecord {
                        symbol,
                        parent_node_id: parent_id.as_str().to_owned(),
                        edge_kind: relation_kind_name(edge.edge.kind).to_owned(),
                        depth: depth + 1,
                    });
                    frontier.push((child_id, depth + 1));
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "caller traversal failed");
            };
            let records = match relation_traversal(
                &graph.reader,
                Arc::clone(&graph.cancellation),
                &request.node_id,
                request.maximum_depth,
                true,
                &request.scope,
            ) {
                Ok(records) => records,
                Err(()) => return failed(context, "caller traversal failed"),
            };
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "callee traversal failed");
            };
            let mut records = match relation_traversal(
                &graph.reader,
                Arc::clone(&graph.cancellation),
                &request.node_id,
                request.maximum_depth,
                false,
                &request.scope,
            ) {
                Ok(records) => records,
                Err(()) => return failed(context, "callee traversal failed"),
            };

            if request.resolve_trait_dispatch {
                let mut seen = records
                    .iter()
                    .map(|record| record.symbol.node_id.clone())
                    .collect::<HashSet<_>>();
                let callee_ids = records
                    .iter()
                    .map(|record| record.symbol.node_id.clone())
                    .collect::<Vec<_>>();
                for callee_id in callee_ids {
                    let Ok(occurrence) = SymbolOccurrenceId::new(callee_id.clone()) else {
                        return failed(context, "trait dispatch identity was invalid");
                    };
                    let Ok(Some(callee)) = graph
                        .reader
                        .symbol_summary(&occurrence, Arc::clone(&graph.cancellation))
                    else {
                        return failed(context, "trait dispatch symbol lookup failed");
                    };
                    let Ok(targets) = trait_dispatch_targets(
                        &graph.reader,
                        Arc::clone(&graph.cancellation),
                        &callee,
                    ) else {
                        return failed(context, "trait dispatch resolution failed");
                    };
                    for target in targets {
                        if !in_scope(&target, &request.scope)
                            || !seen.insert(target.occurrence.as_str().to_owned())
                        {
                            continue;
                        }
                        let Ok(symbol) = symbol_record(target, None) else {
                            return failed(context, "trait dispatch evidence was incomplete");
                        };
                        records.push(SymbolRelationRecord {
                            symbol,
                            edge_kind: relation_kind_name(RelationEdgeKindV1::Calls).to_owned(),
                            dispatch_via_trait: true,
                            dispatch_from: Some(callee_id.clone()),
                            depth: None,
                        });
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
            let Ok(graph) = open_graph(&self.code_graph, context).await else {
                return failed(context, "impact traversal failed");
            };
            let Ok(seed) = SymbolOccurrenceId::new(request.node_id.clone()) else {
                return failed(context, "impact seed identity was invalid");
            };
            let Ok(impact) = graph.reader.impact(
                &[seed],
                &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                request.maximum_depth,
                MAX_COMPATIBILITY_RESULTS,
                MAX_COMPATIBILITY_RESULTS.saturating_mul(16),
                Arc::clone(&graph.cancellation),
            ) else {
                return failed(context, "impact traversal failed");
            };
            let edge_count = impact.impacted.len() as u64;
            let records = impact
                .impacted
                .into_iter()
                .map(|node| node.summary)
                .filter(|node| in_scope(node, &request.scope))
                .take(MAX_COMPATIBILITY_RESULTS)
                .map(|node| symbol_record(node, None))
                .collect::<Result<Vec<_>, _>>();
            let Ok(records) = records else {
                return failed(context, "impact evidence was incomplete");
            };
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

struct OpenSymbolGraph {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

async fn open_graph(
    port: &Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
    context: SymbolGraphPortContext<'_>,
) -> Result<OpenSymbolGraph, ()> {
    let cancellation = crate::graph::request_graph_cancellation(context.request);
    let verified = port
        .open(crate::graph::CodeGraphReadRequest::new(
            context.request,
            context.observed_at,
            Arc::clone(&cancellation),
        ))
        .await
        .map_err(|_| ())?;
    let reader = verified
        .reader_with_cancellation(
            context.request,
            context.observed_at,
            Arc::clone(&cancellation),
        )
        .map_err(|_| ())?;
    Ok(OpenSymbolGraph {
        reader,
        cancellation,
    })
}

fn all_symbols(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, ()> {
    const MAX_SYMBOLS: usize = 500_000;
    const PAGE_SIZE: usize = 4_096;
    let mut after = None;
    let mut symbols = Vec::new();
    loop {
        let page = graph
            .symbols_page(after.as_ref(), PAGE_SIZE, Arc::clone(&cancellation))
            .map_err(|_| ())?;
        if symbols.len().saturating_add(page.symbols.len()) > MAX_SYMBOLS {
            return Err(());
        }
        after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
        symbols.extend(page.symbols);
        if !page.has_more {
            return Ok(symbols);
        }
    }
}

fn trait_implementations(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    name: &str,
    scope: &SymbolGraphScope,
) -> Result<Vec<SymbolRelationRecord>, ()> {
    let candidates = graph
        .resolve_simple_name(
            name,
            None,
            MAX_IMPLEMENTATION_RESULTS,
            Arc::clone(&cancellation),
        )
        .map_err(|_| ())?;
    let mut records = Vec::new();
    for trait_node in candidates.into_iter().filter(|node| {
        node.metadata.as_ref().is_some_and(|metadata| {
            matches!(
                NodeKind::from_str(&metadata.kind),
                Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
            )
        })
    }) {
        let edges = graph
            .callers(
                std::slice::from_ref(&trait_node.occurrence),
                &[RelationEdgeKindV1::Implements],
                MAX_IMPLEMENTATION_RESULTS,
                Arc::clone(&cancellation),
            )
            .map_err(|_| ())?;
        for edge in edges.into_iter().flatten() {
            let implementation = edge.neighbor;
            if !in_scope(&implementation, scope) {
                continue;
            }
            records.push(SymbolRelationRecord {
                symbol: symbol_record(implementation, None)?,
                edge_kind: relation_kind_name(edge.edge.kind).to_owned(),
                dispatch_via_trait: false,
                dispatch_from: Some(trait_node.occurrence.as_str().to_owned()),
                depth: None,
            });
            if records.len() >= MAX_IMPLEMENTATION_RESULTS {
                return Ok(records);
            }
        }
    }
    Ok(records)
}

fn signature_matches(node: &CodeGraphSymbolSummaryV1, request: &SignatureSearchRequest) -> bool {
    let Some(metadata) = node.metadata.as_ref() else {
        return false;
    };
    if request
        .is_async
        .is_some_and(|want_async| signature_is_async(metadata.signature.as_deref()) != want_async)
    {
        return false;
    }
    let Some(signature) = metadata.signature.as_deref() else {
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

fn signature_is_async(signature: Option<&str>) -> bool {
    signature.is_some_and(|signature| {
        let signature = signature.trim_start();
        signature.starts_with("async ") || signature.contains("async fn ")
    })
}

fn relation_traversal(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    seed: &str,
    maximum_depth: u32,
    incoming: bool,
    scope: &SymbolGraphScope,
) -> Result<Vec<SymbolRelationRecord>, ()> {
    let seed = SymbolOccurrenceId::new(seed.to_owned()).map_err(|_| ())?;
    let mut seen = HashSet::from([seed.clone()]);
    let mut frontier = vec![seed];
    let mut records = Vec::new();
    for depth in 1..=maximum_depth {
        if frontier.is_empty() || records.len() >= MAX_COMPATIBILITY_RESULTS {
            break;
        }
        let batches = if incoming {
            graph.callers(
                &frontier,
                &[RelationEdgeKindV1::Calls],
                MAX_COMPATIBILITY_RESULTS.saturating_mul(16),
                Arc::clone(&cancellation),
            )
        } else {
            graph.callees(
                &frontier,
                &[RelationEdgeKindV1::Calls],
                MAX_COMPATIBILITY_RESULTS.saturating_mul(16),
                Arc::clone(&cancellation),
            )
        }
        .map_err(|_| ())?;
        let mut next = Vec::new();
        for edge in batches.into_iter().flatten() {
            if !seen.insert(edge.neighbor.occurrence.clone()) {
                continue;
            }
            let occurrence = edge.neighbor.occurrence.clone();
            if in_scope(&edge.neighbor, scope) {
                records.push(SymbolRelationRecord {
                    symbol: symbol_record(edge.neighbor, None)?,
                    edge_kind: relation_kind_name(edge.edge.kind).to_owned(),
                    dispatch_via_trait: false,
                    dispatch_from: None,
                    depth: Some(depth),
                });
            }
            next.push(occurrence);
            if records.len() >= MAX_COMPATIBILITY_RESULTS {
                break;
            }
        }
        frontier = next;
    }
    Ok(records)
}

fn trait_dispatch_targets(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    method: &CodeGraphSymbolSummaryV1,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, ()> {
    let metadata = method.metadata.as_ref().ok_or(())?;
    let parents = graph
        .callers(
            std::slice::from_ref(&method.occurrence),
            &[RelationEdgeKindV1::Contains],
            MAX_COMPATIBILITY_RESULTS,
            Arc::clone(&cancellation),
        )
        .map_err(|_| ())?;
    let traits = parents
        .into_iter()
        .flatten()
        .map(|edge| edge.neighbor)
        .filter(|parent| {
            parent.metadata.as_ref().is_some_and(|metadata| {
                matches!(
                    NodeKind::from_str(&metadata.kind),
                    Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
                )
            })
        })
        .collect::<Vec<_>>();
    let trait_occurrences = traits
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    if trait_occurrences.is_empty() {
        return Ok(Vec::new());
    }
    let implementors = graph
        .callers(
            &trait_occurrences,
            &[RelationEdgeKindV1::Implements],
            MAX_COMPATIBILITY_RESULTS,
            Arc::clone(&cancellation),
        )
        .map_err(|_| ())?
        .into_iter()
        .flatten()
        .map(|edge| edge.neighbor.occurrence)
        .collect::<Vec<_>>();
    let children = graph
        .callees(
            &implementors,
            &[RelationEdgeKindV1::Contains],
            MAX_COMPATIBILITY_RESULTS,
            cancellation,
        )
        .map_err(|_| ())?;
    Ok(children
        .into_iter()
        .flatten()
        .map(|edge| edge.neighbor)
        .filter(|child| {
            child.metadata.as_ref().is_some_and(|child_metadata| {
                matches!(
                    NodeKind::from_str(&child_metadata.kind),
                    Some(NodeKind::Method | NodeKind::Function)
                ) && child_metadata.simple_name == metadata.simple_name
            })
        })
        .collect())
}

pub(crate) fn symbol_record(
    node: CodeGraphSymbolSummaryV1,
    score: Option<f64>,
) -> Result<SymbolPrimitiveRecord, ()> {
    let metadata = node.metadata.ok_or(())?;
    let file = node
        .binding
        .and_then(|binding| binding.logical_path)
        .ok_or(())?;
    let end_line = metadata
        .start_line
        .checked_add(metadata.line_span.checked_sub(1).ok_or(())?)
        .ok_or(())?;
    Ok(SymbolPrimitiveRecord {
        node_id: node.occurrence.as_str().to_owned(),
        name: metadata.simple_name,
        qualified_name: metadata.qualified_name,
        kind: metadata.kind,
        file,
        start_line_zero_based: metadata.start_line,
        end_line_zero_based: end_line,
        line: metadata.start_line.saturating_add(1),
        end_line: end_line.saturating_add(1),
        is_async: signature_is_async(metadata.signature.as_deref()),
        signature: metadata.signature,
        score,
    })
}

fn in_scope(node: &CodeGraphSymbolSummaryV1, scope: &SymbolGraphScope) -> bool {
    let Some(file) = node
        .binding
        .as_ref()
        .and_then(|binding| binding.logical_path.as_deref())
    else {
        return false;
    };
    scope.path_prefix.as_deref().is_none_or(|path_prefix| {
        tracedecay_runtime_core::path_scope::path_matches_scope(file, Some(path_prefix))
    })
}

fn relation_kind_name(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
        RelationEdgeKindV1::Returns => "returns",
        RelationEdgeKindV1::Receives => "receives",
    }
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

#[allow(clippy::too_many_arguments)]
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
