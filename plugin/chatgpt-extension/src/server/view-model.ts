import type {
  ImpactNodeV1,
  NodeDetailsV1,
  OperationApplicationCodeCalleesResult,
  OperationApplicationCodeCallersResult,
  OperationApplicationSearchResult,
  OperationApplicationStatusResult,
  ProjectStatusV1,
  SearchResultRowV1,
  SearchUnavailableV1,
  SymbolPrimitiveRecord,
  SymbolRelationRecord,
} from "@tracedecay/sdk";
import type {
  BoundedGraph,
  Evidence,
  Failure,
  FreshnessState,
  GraphEdge,
  GraphNode,
  ImpactReport,
  ProjectRef,
  Provenance,
  Relation,
  SearchHit,
  SearchResults,
  Section,
  SymbolDetail,
  SymbolSummary,
  ViewState,
} from "../shared/view.js";
import { deepLinkPath } from "../shared/view.js";
import type { DaemonAuthorityRecord } from "./authority.js";
import type { DaemonBridge } from "./bridge.js";
import { DaemonFailure, toFailure } from "./serve-client.js";

export const SEARCH_LIMIT = 20;
export const IMPACT_DEPTH = 3;
export const GRAPH_DEPTH = 1;

function failed<T>(error: unknown): Section<T> {
  return { state: "failed", failure: toFailure(error) };
}

async function section<T>(load: () => Promise<Section<T>>): Promise<Section<T>> {
  try {
    return await load();
  } catch (error) {
    return failed(error);
  }
}

export async function projectsView(bridge: DaemonBridge, signal?: AbortSignal): Promise<ViewState> {
  const projects = await section<readonly ProjectRef[]>(async () => {
    const listed = await bridge.listProjects(signal);
    return listed.length === 0
      ? { state: "empty", message: "No projects are registered in this TraceDecay profile. Run `tracedecay init` in a repository first." }
      : { state: "ready", data: listed };
  });
  // Read after the registry call: `tracedecay serve` may have brought the
  // daemon back, and the banner must describe the daemon that answered.
  const daemon = await bridge.daemonState();
  return { page: "projects", daemon, projects };
}

export async function failureView(bridge: DaemonBridge, projectId: string, error: unknown): Promise<ViewState> {
  const failure = toFailure(error);
  let project: ProjectRef | null = null;
  try {
    project = await bridge.resolveProject(projectId);
  } catch {
    project = null;
  }
  return { page: "failure", project, failure };
}

export async function searchView(bridge: DaemonBridge, projectId: string, query: string, signal?: AbortSignal): Promise<ViewState> {
  const project = await bridge.resolveProject(projectId, signal);
  const [status, results] = await Promise.all([
    settle(() => bridge.status(projectId, signal)),
    section<SearchResults>(async () => {
      if (query.trim().length === 0) return { state: "empty", message: "Enter a query to search this project." };
      const result = await bridge.search(projectId, query, SEARCH_LIMIT, signal);
      return searchSection(result);
    }),
  ]);
  return { page: "search", project, query, results, provenance: provenanceFrom(project, status) };
}

export async function symbolView(bridge: DaemonBridge, projectId: string, nodeId: string, signal?: AbortSignal): Promise<ViewState> {
  const project = await bridge.resolveProject(projectId, signal);
  const status = await settle(() => bridge.status(projectId, signal));
  const provenance = provenanceFrom(project, status);
  const [symbol, callers, callees, impact] = await Promise.all([
    section<SymbolDetail>(async () => {
      const node = await bridge.node(projectId, nodeId, signal);
      if (!isNodeDetails(node)) {
        return {
          state: "failed",
          failure: { kind: "not_found", code: node.reason_code, message: node.message },
        };
      }
      return { state: "ready", data: symbolDetail(node) };
    }),
    section<readonly Relation[]>(async () => relationSection(await bridge.callers(projectId, nodeId, GRAPH_DEPTH, signal), "callers")),
    section<readonly Relation[]>(async () => {
      const generation = provenance?.generation;
      if (generation === null || generation === undefined) {
        throw new DaemonFailure({
          kind: "unavailable",
          code: "generation_unknown",
          message: "The daemon has not reported a served code generation for this project yet, so callee reads cannot be scoped.",
        });
      }
      return relationSection(await bridge.callees(projectId, nodeId, generation, GRAPH_DEPTH, signal), "callees");
    }),
    section<ImpactReport>(async () => {
      const result = await bridge.impact(projectId, nodeId, IMPACT_DEPTH, signal);
      const nodes = result.nodes.map(impactNode);
      if (nodes.length === 0 && result.complete) {
        return { state: "empty", message: "No dependents reach this symbol within the traversed depth." };
      }
      return { state: "ready", data: { nodes, complete: result.complete, max_depth: IMPACT_DEPTH } };
    }),
  ]);
  const graph = graphSection(symbol, callers, callees);
  const evidence = symbol.state === "ready" ? buildEvidence(project, symbol.data, callers, callees, impact, provenance) : null;
  return { page: "symbol", project, symbol, callers, callees, impact, graph, provenance, evidence };
}

type Settled<T> = { ok: true; value: T } | { ok: false; failure: Failure };

async function settle<T>(load: () => Promise<T>): Promise<Settled<T>> {
  try {
    return { ok: true, value: await load() };
  } catch (error) {
    return { ok: false, failure: toFailure(error) };
  }
}

function isNodeDetails(node: { readonly id?: string } | { readonly status?: string }): node is NodeDetailsV1 {
  return "id" in node && typeof node.id === "string";
}

function isProjectStatus(status: OperationApplicationStatusResult): status is ProjectStatusV1 {
  return "code_index_freshness" in status;
}

export function provenanceFrom(project: ProjectRef, status: Settled<OperationApplicationStatusResult>): Provenance | null {
  if (!status.ok || !isProjectStatus(status.value)) return null;
  const value = status.value;
  const freshness = value.code_index_freshness;
  if (freshness.status === "unavailable") return null;
  const worktree = freshness.worktree;
  const stalenessDetail =
    worktree.staleness_state === null || worktree.staleness_state === undefined ? null : `worktree ${worktree.staleness_state}`;
  const coverage = worktree.coverage;
  const coverageDetail = typeof coverage === "string" ? coverage : JSON.stringify(coverage);
  const authority = value.server;
  return {
    project_id: project.project_id,
    project_root: worktree.worktree_root,
    repository_id: worktree.repository_id ?? null,
    worktree_id: worktree.worktree_id ?? null,
    reference: worktree.source_reference ?? null,
    branch: value.current_branch ?? value.active_branch ?? null,
    commit: worktree.source_revision ?? null,
    generation: worktree.latest_generation_id ?? null,
    freshness: {
      state: freshnessState(freshness.status),
      detail: "reason" in freshness && typeof freshness.reason === "string" ? freshness.reason : stalenessDetail,
    },
    coverage: { recall: coverageDetail === "complete" ? "full" : "partial", detail: coverageDetail },
    authority: authorityOf(authority),
  };
}

function authorityOf(server: unknown): Provenance["authority"] {
  const record = typeof server === "object" && server !== null ? (server as Record<string, unknown>) : {};
  return {
    profile_root: typeof record.profile_root === "string" ? record.profile_root : "",
    daemon_version: typeof record.version === "string" ? record.version : "",
    daemon_pid: typeof record.pid === "number" ? record.pid : 0,
  };
}

export function withAuthority(provenance: Provenance | null, record: DaemonAuthorityRecord | null): Provenance | null {
  if (provenance === null || record === null) return provenance;
  return {
    ...provenance,
    authority: { profile_root: record.profile_root, daemon_version: record.version, daemon_pid: record.pid },
  };
}

function freshnessState(status: string): FreshnessState {
  switch (status) {
    case "current":
    case "stale":
    case "warming":
    case "restoring":
    case "parked":
    case "unavailable":
      return status;
    default:
      return "unavailable";
  }
}

function searchSection(result: OperationApplicationSearchResult): Section<SearchResults> {
  const recall = result.coverage.recall;
  if ("status" in result && result.status === "unavailable") {
    return {
      state: "failed",
      failure: {
        kind: result.freshness.state === "possibly_stale" ? "stale" : "unavailable",
        code: result.reason,
        message: unavailableDetail(result.detail) ?? result.reason,
      },
    };
  }
  const hits: SearchHit[] = [];
  const undisplayable: string[] = [];
  for (const row of result.results) {
    const hit = searchHit(row);
    if (hit === null) undisplayable.push(row.display_unavailable ?? "undisplayable");
    else hits.push(hit);
  }
  const nextCursor = "next_cursor" in result ? result.next_cursor : null;
  if (hits.length === 0 && undisplayable.length === 0) {
    return { state: "empty", message: recall === "partial" ? "No results in the indexed portion of this project yet." : "No symbols match this query." };
  }
  return {
    state: "ready",
    data: { hits, truncated: nextCursor !== null && nextCursor !== undefined, undisplayable, recall },
    generation: typeof result.code_generation === "string" ? result.code_generation : undefined,
  };
}

function unavailableDetail(detail: SearchUnavailableV1["detail"]): string | null {
  if (detail === null || detail === undefined) return null;
  if (detail.kind === "parked") return `${detail.kind}: ${detail.cause} (${detail.remedy})`;
  if (detail.kind === "reset_required") return `${detail.kind}: ${detail.reason} (${detail.remedy})`;
  return detail.kind;
}

function searchHit(row: SearchResultRowV1): SearchHit | null {
  const display = row.display;
  if (row.node_id === null || row.node_id === undefined || display === null || display === undefined) return null;
  const lanes = (row.lexical_routes ?? []).map((route) => (typeof route === "object" && route !== null && "lane" in route ? String(route.lane) : "lexical"));
  return {
    node_id: row.node_id,
    name: display.name,
    qualified_name: display.qualified_name,
    kind: display.kind,
    file: display.path,
    start_line: null,
    end_line: null,
    signature: null,
    score: row.candidate.utility_micros / 1_000_000,
    lanes,
  };
}

function symbolSummary(record: SymbolPrimitiveRecord): SymbolSummary {
  return {
    node_id: record.node_id,
    name: record.name,
    qualified_name: record.qualified_name,
    kind: record.kind,
    file: record.file,
    start_line: record.line,
    end_line: record.end_line,
    signature: record.signature ?? null,
  };
}

function symbolDetail(node: NodeDetailsV1): SymbolDetail {
  return {
    node_id: node.id,
    name: node.name,
    qualified_name: node.qualified_name,
    kind: node.kind,
    file: node.file,
    start_line: node.start_line,
    end_line: node.end_line,
    signature: node.signature ?? null,
    complexity: node.cyclomatic_complexity ?? null,
    unavailable_fields: node.unavailable_fields,
  };
}

function relation(record: SymbolRelationRecord): Relation {
  return {
    symbol: symbolSummary(record.symbol),
    edge_kind: record.edge_kind,
    depth: record.depth ?? 1,
    dispatch_via_trait: record.dispatch_via_trait,
  };
}

function relationSection(
  result: OperationApplicationCodeCallersResult | OperationApplicationCodeCalleesResult,
  direction: "callers" | "callees",
): Section<readonly Relation[]> {
  const items = result.items.map(relation);
  if (items.length === 0) return { state: "empty", message: `No ${direction} recorded in the served graph.` };
  // `truncated` exists on callers results only; callees reports a cursor and
  // `total` instead. Any of them means more rows than the returned page.
  const truncated =
    ("truncated" in result && result.truncated) ||
    (result.next_cursor !== null && result.next_cursor !== undefined) ||
    (typeof result.total === "number" && result.total > items.length);
  return { state: "ready", data: items, truncated, generation: result.generation };
}

function impactNode(node: ImpactNodeV1) {
  return { node_id: node.id, name: node.name, kind: node.kind, file: node.file, line: node.line, depth: node.depth };
}

function graphSection(
  symbol: Section<SymbolDetail>,
  callers: Section<readonly Relation[]>,
  callees: Section<readonly Relation[]>,
): Section<BoundedGraph> {
  if (symbol.state !== "ready") {
    return symbol.state === "failed" ? { state: "failed", failure: symbol.failure } : { state: "empty", message: symbol.message };
  }
  if (callers.state === "failed" && callees.state === "failed") return { state: "failed", failure: callers.failure };
  const focus: GraphNode = { ...symbol.data, role: "focus", depth: 0 };
  const nodes: GraphNode[] = [focus];
  const edges: GraphEdge[] = [];
  const seen = new Set<string>([focus.node_id]);
  const add = (relations: readonly Relation[], role: "caller" | "callee") => {
    for (const item of relations) {
      if (!seen.has(item.symbol.node_id)) {
        seen.add(item.symbol.node_id);
        nodes.push({ ...item.symbol, role, depth: item.depth });
      }
      edges.push(
        role === "caller"
          ? { from: item.symbol.node_id, to: focus.node_id, edge_kind: item.edge_kind }
          : { from: focus.node_id, to: item.symbol.node_id, edge_kind: item.edge_kind },
      );
    }
  };
  if (callers.state === "ready") add(callers.data, "caller");
  if (callees.state === "ready") add(callees.data, "callee");
  if (nodes.length === 1) return { state: "empty", message: "This symbol has no recorded callers or callees." };
  // The graph is bounded when a side failed, when a side kept rows to a page,
  // or when the two sides were served from different generations and cannot
  // describe one consistent snapshot.
  const mixedGeneration =
    callers.state === "ready" &&
    callees.state === "ready" &&
    callers.generation !== undefined &&
    callees.generation !== undefined &&
    callers.generation !== callees.generation;
  const truncated =
    callers.state === "failed" ||
    callees.state === "failed" ||
    (callers.state === "ready" && callers.truncated === true) ||
    (callees.state === "ready" && callees.truncated === true) ||
    mixedGeneration;
  return { state: "ready", data: { nodes, edges, max_depth: GRAPH_DEPTH, truncated } };
}

function sectionLines<T>(title: string, value: Section<readonly T[]>, line: (item: T) => string): string[] {
  const out = [`### ${title}`];
  switch (value.state) {
    case "ready":
      out.push(...value.data.map((item) => `- ${line(item)}`));
      if (value.truncated === true) out.push("_First page only; more rows exist than shown._");
      break;
    case "empty":
      out.push(`_${value.message}_`);
      break;
    case "failed":
      out.push(`_Unavailable (${value.failure.kind}: ${value.failure.message})_`);
      break;
  }
  return out;
}

function relationLines(value: Section<readonly Relation[]>, title: string, generation: string | null): string[] {
  const lines = sectionLines(title, value, (item) => `${item.symbol.qualified_name} (${item.symbol.file}:${item.symbol.start_line ?? "?"}, ${item.edge_kind})`);
  if (value.state === "ready" && generation !== null && value.generation !== undefined && value.generation !== generation) {
    lines.push(`_Served from generation ${value.generation}, which differs from the reported project generation ${generation}._`);
  }
  return lines;
}

export function buildEvidence(
  project: ProjectRef,
  symbol: SymbolDetail,
  callers: Section<readonly Relation[]>,
  callees: Section<readonly Relation[]>,
  impact: Section<ImpactReport>,
  provenance: Provenance | null,
): Evidence {
  const location = `${symbol.file}:${symbol.start_line ?? "?"}-${symbol.end_line ?? "?"}`;
  const lines = [
    `## ${symbol.kind} \`${symbol.qualified_name}\``,
    `- Project: ${project.label} (${project.project_id})`,
    `- Location: ${location}`,
    ...(symbol.signature === null ? [] : [`- Signature: \`${symbol.signature}\``]),
    `- Node id: \`${symbol.node_id}\``,
    ...relationLines(callers, "Callers", provenance?.generation ?? null),
    ...relationLines(callees, "Callees", provenance?.generation ?? null),
    ...sectionLines(
      "Impact",
      impact.state === "ready" ? { state: "ready", data: impact.data.nodes } : impact,
      (item) => `depth ${item.depth}: ${item.name} (${item.file}:${item.line})`,
    ),
    ...(impact.state === "ready" && !impact.data.complete ? ["_Impact traversal is partial: the daemon hit its traversal budget._"] : []),
    "### Provenance",
    ...(provenance === null
      ? ["_Project status unavailable; provenance not verified._"]
      : [
          `- Repository: ${provenance.repository_id ?? "unknown"}`,
          `- Worktree: ${provenance.worktree_id ?? "unknown"} (${provenance.project_root})`,
          `- Ref: ${provenance.reference ?? "unknown"}${provenance.branch === null ? "" : ` (branch ${provenance.branch})`}`,
          `- Commit: ${provenance.commit ?? "unknown"}`,
          `- Generation: ${provenance.generation ?? "unknown"}`,
          `- Freshness: ${provenance.freshness.state}${provenance.freshness.detail === null ? "" : ` (${provenance.freshness.detail})`}`,
          `- Coverage: ${provenance.coverage.recall}${provenance.coverage.detail === null ? "" : ` (${provenance.coverage.detail})`}`,
        ]),
    `- Deep link: ${deepLinkPath({ kind: "symbol", project_id: project.project_id, node_id: symbol.node_id })}`,
  ];
  return {
    title: `${symbol.kind} ${symbol.qualified_name}`,
    markdown: lines.join("\n"),
    structured: {
      project,
      symbol,
      callers,
      callees,
      impact,
      provenance,
    },
  };
}

export function viewText(view: ViewState): string {
  switch (view.page) {
    case "projects":
      return view.projects.state === "ready"
        ? `TraceDecay projects available: ${view.projects.data.map((project) => `${project.label} (${project.project_id})`).join(", ")}`
        : view.projects.state === "empty"
          ? view.projects.message
          : `TraceDecay projects unavailable (${view.projects.failure.kind}): ${view.projects.failure.message}`;
    case "search":
      if (view.results.state === "ready") {
        const hits = view.results.data.hits
          .map((hit) => `- ${hit.kind} ${hit.qualified_name} (${hit.file}) node_id=${hit.node_id}`)
          .join("\n");
        const hidden = view.results.data.undisplayable.length;
        return `Search "${view.query}" in ${view.project.label}${view.results.data.recall === "partial" ? " (partial recall)" : ""}${hidden > 0 ? ` (${hidden} candidate(s) could not be displayed)` : ""}:\n${hits}`;
      }
      return view.results.state === "empty"
        ? `Search "${view.query}" in ${view.project.label}: ${view.results.message}`
        : `Search "${view.query}" in ${view.project.label} failed (${view.results.failure.kind}): ${view.results.failure.message}`;
    case "symbol":
      return view.evidence === null
        ? view.symbol.state === "failed"
          ? `Symbol lookup failed (${view.symbol.failure.kind}): ${view.symbol.failure.message}`
          : "Symbol unavailable"
        : view.evidence.markdown;
    case "failure":
      return `TraceDecay request failed (${view.failure.kind}): ${view.failure.message}`;
  }
}
