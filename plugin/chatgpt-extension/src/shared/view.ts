// View state shared by the MCP server (producer) and the MCP App (consumer).
// Every variant is a truthful daemon-backed state; the app never synthesizes
// data the daemon did not report.
import type { NodeUnresolvedCallReasonV1, NodeUnresolvedCallReportV1, ProjectRegistrySummary } from "@tracedecay/sdk";

export type ProjectRef = {
  readonly project_id: string;
  readonly label: string;
  readonly project_root: string;
  readonly head_branch: string | null;
  readonly default_branch: string | null;
};

export type ProjectListing = {
  readonly projects: readonly ProjectRef[];
  readonly limit: number;
  readonly truncated: boolean;
  /** Canonical counts describe this returned page, not the entire registry. */
  readonly summary: ProjectRegistrySummary;
};

export type FreshnessState =
  | "current"
  | "stale"
  | "warming"
  | "restoring"
  | "parked"
  | "unavailable"
  | "possibly_stale"
  | "fresh";

export type Provenance = {
  readonly project_id: string;
  readonly project_root: string;
  readonly repository_id: string | null;
  readonly worktree_id: string | null;
  readonly reference: string | null;
  readonly branch: string | null;
  readonly commit: string | null;
  readonly generation: string | null;
  readonly freshness: { readonly state: FreshnessState; readonly detail: string | null };
  readonly coverage: { readonly recall: "full" | "partial"; readonly detail: string | null };
  readonly authority: {
    readonly profile_root: string;
    readonly daemon_version: string;
    readonly daemon_pid: number;
  } | null;
};

export type FailureKind =
  | "denied"
  | "not_found"
  | "unavailable"
  | "disconnected"
  | "stale"
  | "invalid_request"
  | "protocol";

export type Failure = {
  readonly kind: FailureKind;
  readonly code: string;
  readonly message: string;
};

export type Section<T> =
  | {
      readonly state: "ready";
      readonly data: T;
      /** Set when the daemon reported more rows than the returned page. */
      readonly truncated?: boolean;
      /** The code generation the daemon reported serving this read from. */
      readonly generation?: string;
    }
  | { readonly state: "empty"; readonly message: string }
  | { readonly state: "failed"; readonly failure: Failure };

export type SymbolSummary = {
  readonly node_id: string;
  readonly name: string;
  readonly qualified_name: string;
  readonly kind: string;
  readonly file: string;
  readonly start_line: number | null;
  readonly end_line: number | null;
  readonly signature: string | null;
};

export type SearchHit = SymbolSummary & {
  readonly score: number | null;
  readonly lanes: readonly string[];
};

export type SearchResults = {
  readonly hits: readonly SearchHit[];
  readonly truncated: boolean;
  /** `display_unavailable` reason of each returned row that cannot be shown. */
  readonly undisplayable: readonly string[];
  readonly recall: "full" | "partial";
};

export type Relation = {
  readonly symbol: SymbolSummary;
  readonly edge_kind: string;
  readonly depth: number;
  readonly dispatch_via_trait: boolean;
};

export type ImpactNode = {
  readonly node_id: string;
  readonly name: string;
  readonly kind: string;
  readonly file: string;
  readonly line: number;
  readonly depth: number;
};

export type ImpactReport = {
  readonly nodes: readonly ImpactNode[];
  readonly complete: boolean;
  readonly max_depth: number;
};

export type GraphRole = "focus" | "caller" | "callee";

export type GraphNode = SymbolSummary & { readonly role: GraphRole; readonly depth: number };

export type GraphEdge = {
  readonly from: string;
  readonly to: string;
  readonly edge_kind: string;
};

export type BoundedGraph = {
  readonly nodes: readonly GraphNode[];
  readonly edges: readonly GraphEdge[];
  readonly max_depth: number;
  readonly truncated: boolean;
};

export type SymbolDetail = SymbolSummary & {
  readonly complexity: number | null;
  readonly unresolved_calls: NodeUnresolvedCallReportV1;
  readonly unavailable_fields: readonly string[];
};

export const UNRESOLVED_CALL_REASON_LABELS: Readonly<Record<NodeUnresolvedCallReasonV1, string>> = {
  target_unresolved: "Target not resolved",
  ambiguous_local: "Multiple local matches",
  unmodeled_block_glob: "Block wildcard import limits resolution",
  unmodeled_inline_module_glob: "Inline module wildcard import limits resolution",
};

export const UNRESOLVED_CALL_COVERAGE = "Recorded evidence only; dynamic calls may be missing.";
export const UNRESOLVED_CALL_RANGES = "Byte ranges are zero-based and end-exclusive; some cover a whole declaration.";

export function unresolvedCallSummary(outcome: NodeUnresolvedCallReportV1["outcome"]): string {
  switch (outcome.state) {
    case "complete":
      return outcome.sites.length === 0 ? "No unresolved calls recorded." : "All recorded unresolved calls are shown.";
    case "truncated":
      return "Partial report; more unresolved calls may remain.";
    case "unavailable":
      return "Unresolved-call report unavailable.";
  }
}

export type Evidence = {
  readonly title: string;
  readonly markdown: string;
  readonly structured: Record<string, unknown>;
};

export type DaemonState =
  | {
      readonly state: "connected";
      readonly profile_root: string;
      readonly version: string;
      readonly pid: number;
    }
  | { readonly state: "disconnected"; readonly failure: Failure };

export type ViewState =
  | {
      readonly page: "projects";
      readonly daemon: DaemonState;
      /** An empty query selects the recent-project listing. */
      readonly query: string;
      readonly projects: Section<ProjectListing>;
    }
  | {
      readonly page: "search";
      readonly project: ProjectRef;
      readonly query: string;
      readonly results: Section<SearchResults>;
      readonly provenance: Provenance | null;
    }
  | {
      readonly page: "symbol";
      readonly project: ProjectRef;
      readonly symbol: Section<SymbolDetail>;
      readonly callers: Section<readonly Relation[]>;
      readonly callees: Section<readonly Relation[]>;
      readonly impact: Section<ImpactReport>;
      readonly graph: Section<BoundedGraph>;
      readonly provenance: Provenance | null;
      readonly evidence: Evidence | null;
    }
  | {
      readonly page: "failure";
      readonly project: ProjectRef | null;
      readonly failure: Failure;
    };

export type ViewPage = ViewState["page"];

const PAGES: ReadonlySet<string> = new Set<ViewPage>(["projects", "search", "symbol", "failure"]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isUnsignedInteger(value: unknown, maximum = Number.MAX_SAFE_INTEGER): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= maximum;
}

function isUnresolvedCallSite(value: unknown): boolean {
  if (!isRecord(value) || typeof value.reference_name !== "string" || !isRecord(value.evidence_span)) return false;
  const span = value.evidence_span;
  return isUnsignedInteger(span.start_byte) && isUnsignedInteger(span.end_byte) && span.end_byte >= span.start_byte
    && (value.argument_count == null || isUnsignedInteger(value.argument_count, 4_294_967_295))
    && Array.isArray(value.reasons)
    && value.reasons.every((reason: unknown) => typeof reason === "string" && Object.hasOwn(UNRESOLVED_CALL_REASON_LABELS, reason));
}

/** The host payload must carry the report the symbol renderer consumes. */
function isUnresolvedCallReport(value: unknown): value is NodeUnresolvedCallReportV1 {
  if (!isRecord(value) || !isRecord(value.outcome)) return false;
  for (const field of ["code_generation", "caller_node_id", "file_occurrence_id", "file"]) {
    const identity = value[field];
    if (typeof identity !== "string" || identity.length === 0) return false;
  }
  const outcome = value.outcome;
  switch (outcome.state) {
    case "complete":
      return Array.isArray(outcome.sites) && outcome.sites.every(isUnresolvedCallSite);
    case "truncated":
      return isUnsignedInteger(outcome.maximum_references, 4_294_967_295)
        && Array.isArray(outcome.sites) && outcome.sites.every(isUnresolvedCallSite);
    case "unavailable": {
      const evidence = outcome.evidence;
      return isRecord(evidence) && evidence.status === "unavailable"
        && typeof evidence.reason_code === "string" && typeof evidence.detail === "string"
        && typeof evidence.retryable === "boolean";
    }
    default:
      return false;
  }
}

export function isViewState(value: unknown): value is ViewState {
  if (!isRecord(value) || typeof value.page !== "string" || !PAGES.has(value.page)) return false;
  if (value.page !== "symbol") return true;
  const symbol = value.symbol;
  if (!isRecord(symbol)) return false;
  if (symbol.state !== "ready") return symbol.state === "empty" || symbol.state === "failed";
  const detail = symbol.data;
  if (!isRecord(detail) || !isUnresolvedCallReport(detail.unresolved_calls)) return false;
  return detail.unresolved_calls.caller_node_id === detail.node_id && detail.unresolved_calls.file === detail.file;
}

/** Deep-link paths understood by the app; the host supplies them as URLs. */
export type DeepLinkTarget =
  | { readonly kind: "projects" }
  | { readonly kind: "project"; readonly project_id: string }
  | { readonly kind: "search"; readonly project_id: string; readonly query: string }
  | { readonly kind: "symbol"; readonly project_id: string; readonly node_id: string };

export function parseDeepLink(url: string): DeepLinkTarget | null {
  let parsed: URL;
  try {
    parsed = new URL(url, "https://tracedecay.invalid");
  } catch {
    return null;
  }
  const parts = parsed.pathname.split("/").filter((part) => part.length > 0).map(decodeURIComponent);
  if (parts.length === 0) return { kind: "projects" };
  if (parts[0] !== "projects" || parts[1] === undefined) return null;
  const project_id = parts[1];
  if (parts.length === 2) return { kind: "project", project_id };
  if (parts[2] === "search" && parts.length === 3) {
    return { kind: "search", project_id, query: parsed.searchParams.get("q") ?? "" };
  }
  if (parts[2] === "symbols" && parts[3] !== undefined && parts.length === 4) {
    return { kind: "symbol", project_id, node_id: parts[3] };
  }
  return null;
}

export function deepLinkPath(target: DeepLinkTarget): string {
  switch (target.kind) {
    case "projects":
      return "/";
    case "project":
      return `/projects/${encodeURIComponent(target.project_id)}`;
    case "search":
      return `/projects/${encodeURIComponent(target.project_id)}/search?q=${encodeURIComponent(target.query)}`;
    case "symbol":
      return `/projects/${encodeURIComponent(target.project_id)}/symbols/${encodeURIComponent(target.node_id)}`;
  }
}
