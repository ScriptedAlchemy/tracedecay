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
 * server reports its built-in TraceDecay provider and no external curator
 * tool host.
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
  created_at: number;
  updated_at: number;
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

export interface MemoryStatusRecord {
  fact_count: number;
  entity_count: number;
  bank_count: number;
  algebra_name: string;
  hrr_dim: number;
  estimated_capacity: number;
  trust_0_025_count: number;
  trust_025_050_count: number;
  trust_050_075_count: number;
  trust_075_100_count: number;
  below_default_recall_threshold_count: number;
  helpful_count: number;
  unhelpful_count: number;
  missing_vector_count: number;
  legacy_backfill_complete: boolean;
  repair: {
    missing_vectors_repaired: number;
    banks_rebuilt: number;
  };
}

export interface MemoryStatusResponse {
  path: string;
  exists: boolean;
  memory: MemoryStatusRecord;
  largest_bank_fact_count: number;
  largest_bank_utilization_pct: number;
  error: string;
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
  confidence?: number;
  fact_id?: number;
  duplicate_of?: number;
  entity_id?: number;
  loser?: number;
  winner?: number;
  loser_ids?: number[];
  winner_id?: number;
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

/** Deterministic rule-based hygiene candidates (never auto-applied). */
export interface MemoryCurateHygieneCandidates {
  secret_like: MemoryCurateHygieneCandidate[];
  transient: MemoryCurateHygieneCandidate[];
  supersession: MemoryCurateHygieneCandidate[];
}

/**
 * Wire contract for `POST /api/plugins/holographic/curate`: the curation
 * plan/report. With `dry_run` the actions are proposals; otherwise
 * `applied_counts`/`apply_errors` describe what was actually executed.
 */
export interface MemoryCurateResponse {
  provider?: string;
  ran: boolean;
  dry_run: boolean;
  actions: MemoryCurateAction[];
  llm_apply?: {
    ops?: MemoryCurateAction[];
    rejected_ops?: unknown[];
    note?: string;
    [key: string]: unknown;
  };
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

export type AutomationBackend = "disabled" | "codex_app_server" | "external_command";
export type SelectableAutomationBackend = Exclude<AutomationBackend, "external_command">;

export type AutomationHostMode = "standalone" | "delegated_host";

export interface AutomationBackendAvailability {
  backend: AutomationBackend;
  available: boolean;
  executable?: string | null;
  reason?: string | null;
}

export interface AutomationTaskConfig {
  enabled: boolean;
  schedule?: string | null;
  interval_secs?: number | null;
  cooldown_secs?: number | null;
  min_idle_secs?: number | null;
  stale_lock_secs?: number | null;
}

export interface AutomationTaskSet {
  memory_curator: AutomationTaskConfig;
  session_reflector: AutomationTaskConfig;
  skill_writer: AutomationTaskConfig;
}

export interface MemoryAutomationConfig {
  enabled: boolean;
  backend: AutomationBackend;
  host_mode: AutomationHostMode;
  model?: string | null;
  timeout_secs: number;
  scheduler_tick_secs: number;
  max_tokens?: number | null;
  temperature?: number | null;
  require_dashboard_approval: boolean;
  auto_apply_memory_ops: boolean;
  auto_enable_skills: boolean;
  tasks: AutomationTaskSet;
}

export interface AutomationTaskPatch {
  enabled?: boolean;
  schedule?: string | null;
  interval_secs?: number | null;
  cooldown_secs?: number | null;
  min_idle_secs?: number | null;
  stale_lock_secs?: number | null;
}

export interface MemoryAutomationConfigPatch {
  enabled?: boolean;
  backend?: SelectableAutomationBackend;
  host_mode?: AutomationHostMode;
  model?: string | null;
  timeout_secs?: number;
  scheduler_tick_secs?: number;
  max_tokens?: number | null;
  temperature?: number | null;
  require_dashboard_approval?: boolean;
  auto_apply_memory_ops?: boolean;
  auto_enable_skills?: boolean;
  memory_curator?: AutomationTaskPatch;
  session_reflector?: AutomationTaskPatch;
  skill_writer?: AutomationTaskPatch;
}

export interface MemoryAutomationConfigResponse {
  global: MemoryAutomationConfig;
  project: MemoryAutomationConfigPatch | null;
  effective: MemoryAutomationConfig;
  backend_availability?: AutomationBackendAvailability;
  project_config_path?: string;
}

export interface AutomationSchedulerTaskStatus {
  task: "memory_curator" | "session_reflector" | "skill_writer" | string;
  due: boolean;
  skip_reason?: string | null;
  last_scheduler_run?: MemoryAutomationRunRecord | null;
}

export interface AutomationSchedulerStatusResponse {
  status: string;
  paused: boolean;
  enabled: boolean;
  scheduler_tick_secs: number;
  now: number;
  /** Fact proposals awaiting review (additive; older servers omit it). */
  pending_fact_proposals?: number;
  /** Skill drafts/updates awaiting review (additive; older servers omit it). */
  pending_skills?: number;
  project_config_path?: string;
  control_path?: string;
  tasks: AutomationSchedulerTaskStatus[];
}

export interface MemoryAgentPlanResponse<TReport = Record<string, unknown>> {
  run_id: string;
  dry_run: true;
  status: string;
  report?: TReport;
  ledger_record: MemoryAutomationRunRecord;
  backend_response?: unknown;
  error?: string;
}

export interface AutomationRunRequest {
  dry_run?: true;
  provider?: string;
  query?: string;
  evidence_limit?: number;
  storage_scope?: "project_local" | "hermes_profile" | string;
  hermes_home?: string;
  scope?: "all" | "session" | "current" | string;
  session_id?: string;
  include_summaries?: boolean;
  sort?: "recency" | "relevance" | "hybrid" | string;
  source?: string;
  role?: string;
  start_time?: number;
  end_time?: number;
}

export type MemoryAutomationRunResponse<TReport = Record<string, unknown>> =
  MemoryAgentPlanResponse<TReport>;

export interface MemoryAutomationRunRecord {
  schema_version: number;
  run_id: string;
  trigger: "manual_cli" | "dashboard" | "scheduler" | string;
  task: "memory_curator" | "session_reflector" | "skill_writer" | string;
  task_key?: string | null;
  backend: string;
  host_mode?: "standalone" | "delegated_host" | string | null;
  prompt_version?: string | null;
  response_schema?: unknown;
  strict_json?: boolean | null;
  model?: string | null;
  status: "queued" | "running" | "succeeded" | "failed" | "skipped" | string;
  evidence_hash?: string | null;
  input_hash?: string | null;
  output_hash?: string | null;
  proposed_ops?: unknown;
  applied_ops?: unknown;
  rejected_ops?: unknown;
  validation_report?: unknown;
  reviewed_count?: number;
  accepted_count: number;
  rejected_count: number;
  skipped_count?: number;
  error?: string | null;
  error_classification?:
    | "retryable"
    | "permanent"
    | "timeout"
    | "unavailable"
    | "malformed_output"
    | string
    | null;
  error_retryable?: boolean | null;
  fallback_status?: string | null;
  report_ref?: unknown;
  artifacts?: MemoryAutomationRunArtifact[];
  started_at: string;
  completed_at: string;
}

export interface MemoryAutomationRunArtifact {
  schema_version: number;
  kind: string;
  path: string;
  sha256: string;
  summary?: string | null;
  created_at: string;
}

export interface MemoryAutomationRunArtifactsResponse {
  run_id: string;
  artifacts: MemoryAutomationRunArtifact[];
  artifact_chain?: {
    expected_kinds?: string[];
    present_kinds?: string[];
    complete?: boolean;
  };
  count: number;
  error?: string;
}

export interface MemoryAutomationRunArtifactPayloadResponse {
  run_id: string;
  artifact: MemoryAutomationRunArtifact;
  payload: unknown;
  error?: string;
}

export interface MemoryAutomationRunsResponse {
  records: MemoryAutomationRunRecord[];
  count: number;
  limit: number;
  error?: string;
}

export type ManagedSkillSource = "automation_run" | "user_draft" | "import" | string;

export type ManagedSkillState =
  | "pending_approval"
  | "active"
  | "disabled"
  | "archived"
  | string;

export type SkillInstallTarget =
  | "cursor"
  | "codex"
  | "claude"
  | "agents"
  | "opencode"
  | "kimi"
  | "kiro"
  | "hermes"
  | string;

export interface ManagedSkillProvenance {
  source: ManagedSkillSource;
  actor: string;
  run_id?: string | null;
}

export interface ManagedSkillMetadata {
  id: string;
  title: string;
  summary: string;
  category: string;
  targets: SkillInstallTarget[];
  state: ManagedSkillState;
  checksum: string;
  provenance: ManagedSkillProvenance;
  pinned: boolean;
  created_at: number;
  updated_at: number;
}

export interface ManagedSupportFile {
  path: string;
  bytes: number[];
}

export interface ManagedSkill {
  metadata: ManagedSkillMetadata;
  body_markdown: string;
  support_files: ManagedSupportFile[];
  pending_update?: ManagedSkillPendingUpdate | null;
}

export interface ManagedSkillPendingUpdate {
  base_checksum: string;
  staged_at: number;
  metadata: ManagedSkillMetadata;
  body_markdown: string;
  support_files: ManagedSupportFile[];
}

export interface SkillUsageSummary {
  schema_version: number;
  skill_id: string;
  title?: string | null;
  category?: string | null;
  state?: ManagedSkillState | null;
  pinned: boolean;
  created_by?: string | null;
  provenance_source?: ManagedSkillSource | null;
  targets: string[];
  view_count: number;
  use_count: number;
  patch_count: number;
  first_seen_at: number;
  last_activity_at: number;
  last_viewed_at?: number | null;
  last_used_at?: number | null;
  last_patched_at?: number | null;
}

export interface SkillStaleRecommendation {
  skill_id: string;
  stale: boolean;
  recommendation: string;
  reason: string;
  evidence: string[];
}

export interface SkillImprovementRecommendation {
  skill_id: string;
  improvement: boolean;
  recommendation: string;
  reason: string;
  priority: string;
  evidence: string[];
}

export interface ManagedSkillListResponse {
  profile_root: string;
  skills_root: string;
  count: number;
  skills: ManagedSkill[];
  skill_metadata: ManagedSkillMetadata[];
  usage_summaries?: SkillUsageSummary[];
  stale_recommendations?: SkillStaleRecommendation[];
  improvement_recommendations?: SkillImprovementRecommendation[];
  error?: string;
}

export interface ManagedSkillResponse {
  profile_root: string;
  skills_root: string;
  skill_dir: string;
  skill: ManagedSkill;
  usage_summary?: SkillUsageSummary;
  stale_recommendation?: SkillStaleRecommendation | null;
  improvement_recommendation?: SkillImprovementRecommendation | null;
  error?: string;
}

export type FactProposalState =
  | "pending_approval"
  | "applied"
  | "rejected"
  | string;

export interface FactProposalRecord {
  schema_version: number;
  proposal_id: string;
  run_id: string;
  evidence_hash?: string | null;
  state: FactProposalState;
  add_fact_request?: {
    content?: string;
    type?: string;
    category?: string;
    tags?: string[];
    [key: string]: unknown;
  } | null;
  proposal?: unknown;
  validation_reason?: string | null;
  validation?: unknown;
  reviewer?: string | null;
  applied_fact_id?: number | null;
  apply_outcome?: unknown;
  created_at: number;
  updated_at: number;
}

export interface FactProposalListResponse {
  proposals: FactProposalRecord[];
  count: number;
  limit: number;
  error?: string;
}

export interface FactProposalResponse {
  proposal: FactProposalRecord;
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
    id?: string;
    name: string;
    path: string;
    ts?: string | null;
    summary?: string | null;
    provider?: string;
    mode?: string;
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
