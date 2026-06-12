/**
 * Holographic-memory dashboard types.
 *
 * Ported from the core SPA's `web/src/lib/api.ts` (the `Memory*` / `Holographic*`
 * interfaces) so this plugin bundle owns its own contract independent of the
 * core app.
 */

/**
 * Provider/engine discovery block embedded in `GET /api/plugins/holographic/`
 * responses. Mirrors the Hermes `providers` payload; the standalone tracedecay
 * server fills it with a static "tracedecay" stub.
 */
export interface MemoryProviderStatus {
  memory_provider: string;
  memory_options: Array<{ name: string; description: string }>;
  context_engine: string;
  context_options: Array<{ name: string; description: string }>;
  plugin_context_engine: {
    name?: string;
    status?: Record<string, unknown>;
    tools?: string[];
    error?: string;
  } | null;
  curator_tools?: {
    enabled?: boolean;
    count?: number;
    available?: number;
    tools?: string[];
    mode?: string;
    max_tool_calls_per_batch?: number;
    agent_toolsets?: string[];
    agent_disabled_toolsets?: string[];
    max_agent_iterations?: number;
    error?: string;
  };
}

export interface HolographicFact {
  fact_id: number;
  content: string;
  category: string;
  tags: string;
  trust_score: number;
  retrieval_count: number;
  helpful_count: number;
  created_at: string;
  updated_at: string;
  has_hrr?: boolean | number;
  snippet?: string;
}

export interface HolographicEntity {
  entity_id: number;
  name: string;
  entity_type: string;
  aliases: string;
  created_at: string;
  fact_count: number;
}

export interface HolographicOverview {
  facts: number;
  entities: number;
  banks: number;
  categories: Array<{ category: string; count: number; avg_trust: number }>;
  entity_types: Array<{ entity_type: string; count: number }>;
  hrr_coverage: Array<{
    category: string;
    facts: number;
    hrr_vectors: number;
    coverage: number;
    bank_name: string;
    bank_fact_count: number;
    dim?: number | null;
    updated_at?: string | null;
    status: "ready" | "missing_vectors" | "missing_bank" | "stale_bank" | string;
  }>;
  memory_banks: Array<{
    bank_id: number;
    bank_name: string;
    dim: number;
    fact_count: number;
    updated_at: string;
  }>;
  trust_histogram?: Array<{ bucket: number; label: string; count: number }>;
  growth?: Array<{ date: string; facts: number; cumulative_facts?: number }>;
}

export interface HolographicGraphNode {
  id: string;
  kind: "category" | "bank" | "fact" | "entity" | string;
  label: string;
  category?: string;
  fact_id?: number;
  entity_id?: number;
  entity_type?: string;
  content?: string;
  trust_score?: number;
  retrieval_count?: number;
  helpful_count?: number;
  has_hrr?: boolean;
  dim?: number;
  fact_count?: number;
  updated_at?: string;
}

export interface HolographicGraphEdge {
  source: string;
  target: string;
  kind: "contains" | "mentions" | "bundles" | "bank" | string;
}

/**
 * Wire contract for `GET /api/plugins/holographic/` (overview): provider
 * status plus the holographic store snapshot (overview stats, facts,
 * entities, and the association graph) for the current query/limit.
 */
export interface MemoryDashboardResponse {
  providers: MemoryProviderStatus;
  query: string;
  limit: number;
  holographic: {
    path: string;
    exists: boolean;
    overview: HolographicOverview | null;
    facts: HolographicFact[];
    entities: HolographicEntity[];
    graph: {
      nodes: HolographicGraphNode[];
      edges: HolographicGraphEdge[];
    };
    error: string;
  };
}

/** Entity linked to a fact in the fact-detail payload. */
export interface MemoryFactDetailEntity {
  entity_id: number;
  name: string;
  entity_type?: string | null;
}

/**
 * Wire contract for `GET /api/plugins/holographic/fact/{id}`: the complete
 * fact row (untruncated content) plus linked entities. List/projection
 * payloads truncate `content` to 200 chars; detail panels fetch this instead.
 */
export interface MemoryFactDetailResponse {
  fact: {
    fact_id: number;
    content: string;
    category: string;
    tags?: string | null;
    trust_score: number;
    retrieval_count: number;
    helpful_count: number;
    created_at?: number | string | null;
    updated_at?: number | string | null;
    has_hrr?: boolean | number;
    entities: MemoryFactDetailEntity[];
  };
  error?: string;
}

/** One fact projected from its HRR vector into 2D semantic space. */
export interface MemoryProjectionPoint {
  fact_id: number;
  x: number;
  y: number;
  category: string;
  content: string;
  trust_score: number;
  retrieval_count: number;
  /** Newer servers attach bank + graph-degree context (optional for older payloads). */
  bank_id?: number | null;
  bank_name?: string | null;
  entity_count?: number;
  connection_count?: number;
}

/**
 * Wire contract for `GET /api/plugins/holographic/projection`: 2D PCA of
 * HRR phase vectors. `method` is "none" (with empty points) when fewer than
 * two vectored facts exist.
 */
export interface MemoryProjectionResponse {
  exists: boolean;
  dim: number;
  method: "pca" | "none";
  points: MemoryProjectionPoint[];
  error: string;
}

/** A pair of facts whose HRR vectors are similar (phase cosine similarity). */
export interface MemorySimilarityPair {
  a_id: number;
  b_id: number;
  a_content: string;
  b_content: string;
  a_category: string;
  b_category: string;
  similarity: number;
  token_overlap: number;
  overlap_coefficient: number;
  shared_token_count: number;
  a_token_count: number;
  b_token_count: number;
  shared_tokens: string[];
  classification: "related" | "merge_candidate" | "likely_duplicate" | string;
}

/**
 * Wire contract for `GET /api/plugins/holographic/similarity`: pairwise
 * phase-cosine similarity over vectored facts, capped/sorted server-side and
 * annotated with lexical-overlap stats and a duplicate classification.
 */
/** Server-side histogram over ALL computed pair scores (not just returned pairs). */
export interface MemorySimilarityDistribution {
  bin_count: number;
  bins: Array<{ start: number; end: number; count: number }>;
  min: number;
  max: number;
  min_score: number;
  max_score: number;
  average_score: number;
  total_pairs: number;
}

export interface MemorySimilarityResponse {
  exists: boolean;
  dim: number;
  count: number;
  threshold: number;
  pairs: MemorySimilarityPair[];
  error: string;
  /** Newer servers: effective floor, full-population stats (optional for older payloads). */
  min_similarity?: number;
  total_pairs?: number;
  score_distribution?: MemorySimilarityDistribution | null;
}

/**
 * One proposed (or applied) curation operation inside a curate report. `op`
 * selects which of the optional fields are meaningful (e.g. `loser`/`winner`
 * for merges, `fact_id` for delete/retag, `entity_id` for entity ops).
 * Deletion is permanent — there is no archive op or restore path.
 */
export interface MemoryCurateAction {
  op: string;
  tier?: string;
  reason?: string;
  fact_id?: number;
  duplicate_of?: number;
  entity_id?: number;
  loser?: number;
  winner?: number;
  loser_entity?: number;
  winner_entity?: number;
  name?: string;
  loser_name?: string;
  winner_name?: string;
  normalized_identity?: string;
  entity_type?: string;
  old_entity_type?: string;
  fact_links_moved?: number;
  fact_links_removed?: number;
  fact_count?: number;
  keep?: number;
  similarity?: number;
  category?: string;
  tags?: string;
  old_tags?: string;
  content?: string;
  supersedes?: number[];
}

/** One deterministic hygiene candidate; review must turn it into an op. */
export interface MemoryCurateHygieneCandidate {
  recommended_op: "delete" | "merge" | string;
  status: "candidate" | string;
  review_required: boolean;
  tier: "secret_like" | "transient" | "supersession" | string;
  reason?: string;
  confidence?: number;
  fact_id?: number;
  superseded_by?: number;
  similarity?: number;
  access_count?: number;
  content?: string;
}

/**
 * Wire contract for `POST /api/plugins/holographic/curate`: the curation
 * plan/report. With `dry_run` the actions are proposals; otherwise
 * `applied_counts`/`apply_errors` describe what was actually executed.
 */
/** Deterministic rule-based hygiene candidates (never auto-applied). */
export interface MemoryCurateHygieneCandidates {
  secret_like: MemoryCurateHygieneCandidate[];
  transient: MemoryCurateHygieneCandidate[];
  supersession: MemoryCurateHygieneCandidate[];
}

export interface MemoryCurateResponse {
  provider?: string;
  ran: boolean;
  dry_run: boolean;
  actions: MemoryCurateAction[];
  hygiene_candidates?: MemoryCurateHygieneCandidates;
  counts: Record<string, number>;
  applied_counts?: Record<string, number>;
  skipped_actions?: number;
  apply_errors?: Array<{ action: MemoryCurateAction; error: string }>;
  llm_calls: number;
  coverage?: {
    scanned: number;
    active_total: number;
    due_remaining: number;
    entities_scanned?: number;
    entity_total?: number;
    entity_scan_remaining?: number;
  };
  snapshot?: string | null;
  error?: string;
  mode?: string;
  dry_run_required?: boolean;
}

/**
 * Wire contract for `GET /api/plugins/holographic/curation/preview`: the last
 * saved dry-run report (if any) plus staleness info when the memory store has
 * changed since the preview was generated.
 */
export interface MemoryCuratorPreviewResponse {
  report: MemoryCurateResponse | null;
  saved_at?: string | null;
  stale?: boolean;
  stale_reason?: string;
  error?: string;
}

/**
 * Wire contract for `GET /api/plugins/holographic/curation/status`: scheduler
 * state, resolved curator configuration, and recent snapshot files.
 */
export interface MemoryCuratorStatusResponse {
  provider: string;
  state: {
    paused: boolean;
    last_run_at?: string | null;
    run_count: number;
    last_run_summary?: string | null;
    last_run_id?: string | null;
    last_preview_at?: string | null;
    last_preview_summary?: string | null;
    last_preview_run_id?: string | null;
  };
  config: {
    enabled: boolean;
    interval_hours?: number | string | null;
    min_idle_hours?: number | string | null;
    mode?: string | null;
    dry_run_first?: boolean | null;
    scan_cap?: number | string | null;
    scan_cap_grace?: number | string | null;
    max_candidates?: number | string | null;
    related_cluster_threshold?: number | string | null;
    max_tool_calls_per_batch?: number | string | null;
    batch_size?: number | string | null;
    max_llm_calls_per_run?: number | string | null;
    per_run_facts?: number | string | null;
    candidates_per_fact?: number | string | null;
    candidate_source_cap?: number | string | null;
    candidate_expansion_scan_cap?: number | string | null;
    max_parallel_llm?: number | string | null;
    max_entities_per_run?: number | string | null;
  };
  snapshots: Array<{
    name: string;
    path: string;
  }>;
}

export interface MemoryCuratorActivityEvent {
  ts: string;
  phase: string;
  message: string;
  level?: "debug" | "info" | "warning" | "error" | "success" | string;
  dry_run?: boolean;
  synthetic?: boolean;
  stale_seconds?: number;
  [key: string]: unknown;
}

/**
 * Wire contract for `GET /api/plugins/holographic/curation/activity`: recent
 * curator run events (oldest first), used by the live activity scroller.
 */
export interface MemoryCuratorActivityResponse {
  events: MemoryCuratorActivityEvent[];
  count: number;
  limit: number;
  error?: string;
}

/**
 * One row of `GET /api/plugins/holographic/oplog` — the append-only memory
 * operation audit (add/update/remove/feedback/curate_apply). Deletes carry a
 * content hash in `detail`, never the deleted content.
 */
export interface MemoryOplogEvent {
  id: number;
  ts: number;
  op: string;
  fact_id?: number | null;
  detail: Record<string, unknown>;
}

export interface MemoryOplogResponse {
  events: MemoryOplogEvent[];
  count: number;
  limit: number;
  error?: string;
}

/** One explicit curation operation for `POST /curate/apply`. */
export type MemoryCurateOp =
  | { op: "delete"; fact_id: number; reason?: string }
  | {
      op: "merge";
      winner_id: number;
      loser_ids: number[];
      merged_content?: string;
    };

export interface MemoryCurateApplyOpResult {
  op: string;
  status: "deleted" | "merged" | "error" | string;
  fact_id?: number;
  reason?: string;
  winner_id?: number;
  content_updated?: boolean;
  deleted_loser_ids?: number[];
  failed_losers?: Array<{ fact_id: number; error: string }>;
  error?: string;
  [key: string]: unknown;
}

/**
 * Wire contract for `POST /api/plugins/holographic/curate/apply`: per-op
 * results (partial failures are reported per-op, never as a whole-request
 * error) plus aggregate counts.
 */
export interface MemoryCurateApplyResponse {
  results: MemoryCurateApplyOpResult[];
  counts: { deleted: number; merged: number; errors: number };
}
