export interface GraphNode {
  id: string;
  kind: string;
  name: string;
  qualified_name: string;
  file_path: string;
  signature?: string | null;
  doc?: string | null;
  visibility?: string;
  /** Total in+out edge count in the full graph (not just the visible slice). */
  degree?: number;
  span?: {
    start_line: number;
    end_line: number;
    start_column: number;
    end_column: number;
    attrs_start_line: number;
  };
}

export interface GraphEdge {
  source: string;
  target: string;
  kind: string;
  line?: number | null;
}

export interface GraphOverview {
  path: string;
  totals: { nodes: number; edges: number; files: number };
  nodes_by_kind: Array<{ kind: string; count: number }>;
  edges_by_kind: Array<{ kind: string; count: number }>;
  files_by_language: Array<{ language: string; count: number }>;
  top_connected: Array<{ id: string; name: string; kind: string; file_path: string; degree: number }>;
  largest_files: Array<{ path: string; node_count: number; size: number }>;
}

export interface GraphSearchResponse {
  query: string;
  limit: number;
  offset: number;
  total: number;
  count: number;
  results: GraphNode[];
}

export interface GraphNodeResponse {
  node: GraphNode;
}

export interface GraphNeighborsResponse {
  node_id: string;
  depth: number;
  limit: number;
  callers: GraphNode[];
  callees: GraphNode[];
  edges: GraphEdge[];
  edges_by_kind: Array<{ kind: string; count: number }>;
}

export interface GraphSubgraphResponse {
  seed_id: string | null;
  /** "seeded" for a node/query neighborhood, "default" for the seedless hub slice. */
  mode?: "seeded" | "default";
  nodes: GraphNode[];
  edges: GraphEdge[];
  capped: { nodes: boolean; edges: boolean };
  limits: { nodes: number; edges: number };
}

export interface GraphPathResponse {
  from: string;
  to: string;
  found: boolean;
  path: string[];
  nodes: GraphNode[];
  edges: GraphEdge[];
  max_depth: number;
}

/** Maps a file path to a coarse language bucket (mirrors the backend mapping). */
export function languageForPath(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = path.slice(dot + 1).toLowerCase();
  const table: Record<string, string> = {
    rs: "rust",
    ts: "typescript", tsx: "typescript",
    js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    py: "python", go: "go", java: "java", scala: "scala", sc: "scala",
    c: "c", h: "c",
    cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", hxx: "cpp",
    kt: "kotlin", kts: "kotlin", cs: "csharp", swift: "swift", rb: "ruby",
    php: "php", lua: "lua", zig: "zig",
    sh: "shell", bash: "shell", zsh: "shell",
    md: "markdown", mdx: "markdown",
    json: "json", toml: "toml", yaml: "yaml", yml: "yaml", sql: "sql",
    html: "web", css: "web",
  };
  return table[ext] || "other";
}

/** Buckets the many backend node kinds into a small set of visual families. */
export function kindFamily(kind: string): string {
  if (/^(function|method|arrow_function|procedure|constructor|struct_method|abstract_method|macro)$/.test(kind)) {
    return "fn";
  }
  if (/^(struct|class|enum|enum_variant|union|record|data_class|case_class|sealed_class|inner_class|typedef|type_alias|pascal_record)$/.test(kind)) {
    return "type";
  }
  if (/^(trait|interface|interface_type|annotation|delegate)$/.test(kind)) {
    return "trait";
  }
  if (/^(module|namespace|package|file|library|go_package|scala_package|kotlin_package|pascal_unit|pascal_program)$/.test(kind)) {
    return "module";
  }
  if (/^(const|static|field|property|val|var|csharp_property|event)$/.test(kind)) {
    return "value";
  }
  if (kind === "impl") return "impl";
  return "other";
}

/**
 * Family → [design token, dark-theme fallback]. The canvas samples the token
 * at draw time (canvas 2D can't resolve `var()`); DOM swatches use
 * `KIND_FAMILY_COLORS` below, which rides the token directly so light theme
 * gets the shell's higher-contrast overrides.
 */
export const KIND_FAMILY_TOKENS: Record<string, [string, string]> = {
  fn: ["--ts-cyan", "#75f4d2"],
  type: ["--ts-amber", "#f7c76a"],
  trait: ["--ts-pink", "#ff7ab6"],
  module: ["--ts-blue", "#7aa7ff"],
  value: ["--ts-green", "#67e8a9"],
  impl: ["--ts-text-2", "#a8c8c0"],
  other: ["--ts-text-3", "#6f9189"],
};

export const KIND_FAMILY_COLORS: Record<string, string> = Object.fromEntries(
  Object.entries(KIND_FAMILY_TOKENS).map(([family, [token, fallback]]) => [
    family,
    `var(${token}, ${fallback})`,
  ]),
);

export const KIND_FAMILY_LABELS: Record<string, string> = {
  fn: "functions",
  type: "types",
  trait: "traits / interfaces",
  module: "modules / files",
  value: "consts / fields",
  impl: "impls",
  other: "other",
};

export function colorForKind(kind: string): string {
  return KIND_FAMILY_COLORS[kindFamily(kind)] || KIND_FAMILY_COLORS.other;
}
