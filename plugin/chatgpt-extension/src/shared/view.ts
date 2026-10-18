// View state shared by the MCP server (producer) and the MCP App (consumer).
// Every variant is a truthful daemon-backed state; the app never synthesizes
// data the daemon did not report.

export type ProjectRef = {
  readonly project_id: string;
  readonly label: string;
  readonly project_root: string;
  readonly head_branch: string | null;
  readonly default_branch: string | null;
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
  readonly unavailable_fields: readonly string[];
};

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
      readonly projects: Section<readonly ProjectRef[]>;
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

export function isViewState(value: unknown): value is ViewState {
  return (
    typeof value === "object" &&
    value !== null &&
    "page" in value &&
    typeof value.page === "string" &&
    PAGES.has(value.page)
  );
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
