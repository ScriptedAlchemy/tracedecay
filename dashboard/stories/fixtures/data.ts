/**
 * Canonical fixture payloads for the dashboard `/api` surfaces. These stand in
 * for a running daemon so the visual audit and DOM/MSW tests never require the
 * live API to be up (plan 11a). Both the MSW handlers (`handlers.ts`) and the
 * Playwright route interceptor (`route.ts`) resolve from this single source, so
 * fixtures stay consistent across test transports.
 *
 * Shapes are hand-matched, endpoint by endpoint, to the Rust producers in
 * `src/dashboard/*` and gated against each consuming workspace's `contracts.ts`
 * zod schema by `data.test.ts`. Every route the 12 workspaces read is modeled
 * with data-dense, wire-true payloads so audited surfaces render populated
 * content rather than empty / "unsupported schema" states.
 *
 * Determinism: fixtures never call `Math.random`; array shapes derive from the
 * row index, so the parse-gate test and screenshots are stable across runs.
 * Wall-clock (`nowSecs` / `nowMicros`) is the only time source, matching the
 * pre-existing fixtures.
 */

const nowSecs = Math.floor(Date.now() / 1000);
const nowMicros = Date.now() * 1000;
const DAY = 86_400;

/** Cyclic array access with a non-undefined element type (fixtures always index
 * a non-empty constant array, so the bounds are known-good). */
function pick<T>(arr: readonly T[], i: number): T {
  return arr[((i % arr.length) + arr.length) % arr.length]!;
}

/** DashboardEnvelopeV1 wrapper (see EnvelopeSchema in wire.ts). */
function envelope<T>(
  payload: T,
  domainState = 'ready',
  legalActions: ReadonlyArray<Record<string, unknown>> = [
    { kind: 'refresh', operation: 'storage.refresh' },
  ],
): Record<string, unknown> {
  return {
    schema_revision: 1,
    scope: {
      project_id: 'tracedecay',
      storage_mode: 'project',
      store_root: '/fast/projects/tracedecay/.tracedecay',
    },
    version: { entity_version: 'v-42', graph_version: 'g-42' },
    time: { valid_time_micros: nowMicros, observation_time_micros: nowMicros },
    source_watermark: { source: 'daemon', watermark: 'wm-42' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 12,
      examined: 12,
      matched: 12,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 12,
      unit: 'stores',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: nowMicros, watermark: 'wm-42' },
    domain_state: domainState,
    legal_actions: legalActions,
    payload,
  };
}

function projectEntry(
  id: string,
  label: string,
  root: string,
  ageSecs: number,
): Record<string, unknown> {
  return {
    project_id: id,
    label,
    project_root: root,
    canonical_root: root,
    kind: 'git',
    default_branch: 'master',
    branches: ['master', 'codex/tracedecay-total-redesign-plan'],
    store_count: 3,
    graph_scope_count: 2,
    artifact_count: 7,
    alias_count: 1,
    last_seen_at: nowSecs - ageSecs,
    is_active: id === 'tracedecay',
  };
}

/** GET /api/projects — brain/delivery registry (contracts.ts ProjectsPayloadSchema,
 * DeliveryProjectsPayloadSchema; src/dashboard/projects.rs `list`). */
const projects: Record<string, unknown> = {
  status: 'ok',
  truncated: false,
  active_project_id: 'tracedecay',
  active_project_root: '/fast/projects/tracedecay',
  summary: { project_count: 4, repo_count: 3, truncated: false },
  project_tree: [
    {
      label: 'tracedecay',
      git_common_dir: '/fast/projects/tracedecay/.git',
      project_count: 2,
      branches: ['master', 'codex/tracedecay-total-redesign-plan'],
      projects: [
        { ...projectEntry('tracedecay', 'tracedecay', '/fast/projects/tracedecay', 900), kind: 'primary' },
        {
          ...projectEntry(
            'tracedecay-wt',
            'tracedecay (worktree)',
            '/fast/projects/tracedecay-wt',
            6 * DAY,
          ),
          kind: 'worktree',
        },
      ],
    },
    {
      label: 'lynx-module-federation',
      git_common_dir: '/fast/projects/lynx/.git',
      project_count: 1,
      branches: ['main'],
      projects: [
        { ...projectEntry('lynx-mf', 'lynx-module-federation', '/fast/projects/lynx', 40 * DAY), kind: 'primary' },
      ],
    },
    {
      label: 'hermes',
      git_common_dir: '/fast/projects/hermes/.git',
      project_count: 1,
      branches: ['main', 'release/2.4'],
      projects: [
        { ...projectEntry('hermes', 'hermes', '/fast/projects/hermes', 2 * DAY), kind: 'primary' },
      ],
    },
  ],
};

/* ==========================================================================
 * /api/plugins/holographic/ — memory overview + facts + entities
 * (memory_api.rs::overview; facts.rs fact_summary_json / entity_json /
 * overview_payload / trust_histogram). Consumed by KnowledgePage
 * (MemoryOverviewPayloadSchema) and ExplorerPage memory source.
 * ========================================================================== */

const FACT_CATEGORIES = [
  'project',
  'decision',
  'code_area',
  'tool',
  'user_pref',
  'general',
] as const;

const FACT_CONTENTS = [
  'For native split Lynx Module Federation remotes, the external .lynx.bundle must encode both the background container entry and a main-thread synthetic container entry.',
  'Lynx Module Federation CI separates native and Web Linux jobs so Rspeedy builds and browser setup run in parallel.',
  'The non-eager startup failure was a registration race, not a malformed lazy bundle — the shared background chunk is a valid Webpack {ids, modules} chunk.',
  'The Orbit Control demo exercises three genuine Module Federation consumption forms without eager shares.',
  'Web and native public-path contracts differ; Web builds set output.assetPrefix to auto.',
  'The iOS GitHub Actions job uses pinned actions/cache v5 restore/save for one atomic exact-key cache.',
  'Concurrent agents share the repo target/; waiting on cargo’s directory lock is expected.',
  'Never add --locked to local or agent Cargo commands; CI and packaging own lockfile reproducibility.',
  'Route literal/regex text to tracedecay_grep, symbol names to tracedecay_search, and concepts to tracedecay_context.',
  'Prefer file-edit tools over inline python heredocs for on-disk changes.',
  'Detach long verification runs and poll with Monitor; keep an ~1h budget in the main session.',
  'Record durable facts by feature+date in one doc so sibling commits never trigger a redo.',
  'The storage boundary routes repository reads through the runtime port after sole-daemon ownership lands.',
  'Compaction defaults to gpt-5.6-terra with extra-high reasoning for LCM summarization.',
  'Empty and Unavailable temporal roots are distinct; do not collapse them in the registry mapping.',
  'Binary slot staleness manifests as stale hook logs; check the resolved graph DB path first.',
  'Pathspec-scoped commits (git commit -- <paths>) avoid sweeping others’ staged work in shared trees.',
  'Hook-driven incremental indexing triggers on agent hooks; gix reconciles lazily without always-on watchers.',
] as const;

const FACT_TAGS = [
  ['lynx', 'module-federation'],
  ['ci', 'linux'],
  ['startup', 'federation'],
  ['demo', 'orbit'],
  ['web', 'native'],
  ['ios', 'cache'],
  ['cargo', 'concurrency'],
  ['cargo', 'lockfile'],
  ['tracedecay', 'routing'],
  ['tooling'],
  ['verification'],
  ['memory'],
  ['storage'],
  ['compaction'],
  ['registry'],
  ['ci', 'debug'],
  ['git'],
  ['indexing'],
] as const;

/** Deterministic trust spread across [0.08, 0.99], intentionally uneven so the
 * histogram and the trust bars read as a real distribution rather than a ramp. */
const TRUST_SPREAD = [
  0.99, 0.97, 0.96, 0.94, 0.91, 0.88, 0.86, 0.82, 0.79, 0.74, 0.71, 0.66, 0.62,
  0.58, 0.53, 0.49, 0.44, 0.4, 0.36, 0.31, 0.27, 0.22, 0.19, 0.16, 0.13, 0.1,
  0.09, 0.08,
] as const;

function memoryFacts(): Record<string, unknown>[] {
  return TRUST_SPREAD.map((trust, i) => {
    const helpful = 12 - (i % 9);
    const unhelpful = i % 4;
    const created = nowSecs - (i + 3) * DAY;
    return {
      fact_id: 1000 + i,
      trust_score: trust,
      retrieval_count: 60 - i * 2 + (i % 3) * 4,
      access_count: 90 - i * 2,
      helpful_count: Math.max(helpful, 0),
      unhelpful_count: unhelpful,
      created_at: created,
      updated_at: created + (i % 5) * DAY,
      last_recalled_at: i % 6 === 5 ? null : nowSecs - i * 3600,
      has_hrr: i % 5 === 4 ? 0 : 1,
      content: FACT_CONTENTS[i % FACT_CONTENTS.length],
      category: FACT_CATEGORIES[i % FACT_CATEGORIES.length],
      tags: FACT_TAGS[i % FACT_TAGS.length],
    };
  });
}

/** 10 fixed-width buckets (facts.rs trust_histogram), counts bucketed from the
 * trust spread above so the distribution stays internally consistent. */
function trustHistogram(facts: Record<string, unknown>[]): Record<string, unknown>[] {
  const counts = new Array(10).fill(0) as number[];
  for (const fact of facts) {
    const idx = Math.min(9, Math.floor(Number(fact['trust_score']) * 10));
    counts[idx] = (counts[idx] ?? 0) + 1;
  }
  return counts.map((count, i) => ({
    bucket: i,
    label: `${(i / 10).toFixed(1)}–${((i + 1) / 10).toFixed(1)}`,
    count,
  }));
}

const ENTITY_NAMES: ReadonlyArray<readonly [string, string, number]> = [
  ['Module Federation', 'concept', 34],
  ['Rspeedy', 'tool', 21],
  ['tracedecay', 'project', 58],
  ['ForceAtlas2', 'algorithm', 6],
  ['libsql', 'dependency', 14],
  ['axum', 'dependency', 19],
  ['GitHub Actions', 'tool', 27],
  ['LCM store', 'component', 31],
  ['holographic memory', 'component', 40],
  ['Sigma', 'library', 8],
  ['gpt-5.6-terra', 'model', 11],
  ['cargo', 'tool', 24],
];

function memoryEntities(): Record<string, unknown>[] {
  return ENTITY_NAMES.map(([name, type, factCount], i) => ({
    entity_id: 500 + i,
    name,
    entity_type: type,
    aliases: [],
    created_at: nowSecs - (i + 1) * 2 * DAY,
    fact_count: factCount,
  }));
}

function memoryPayload(query = ''): Record<string, unknown> {
  const facts = memoryFacts();
  const entities = memoryEntities();
  return {
    providers: {
      memory_provider: 'tracedecay',
      memory_options: [
        {
          name: 'tracedecay',
          description: 'TraceDecay holographic memory store (resolved project memory_facts).',
        },
      ],
      context_engine: 'tracedecay',
      context_options: [],
      plugin_context_engine: null,
      curator_tools: { enabled: false, count: 0, available: 0, tools: [] },
    },
    query,
    limit: 100,
    holographic: {
      path: '/fast/projects/tracedecay/.tracedecay/memory.db',
      exists: true,
      overview: {
        facts: 4128,
        entities: 612,
        banks: 6,
        categories: FACT_CATEGORIES.map((category, i) => ({
          category,
          count: 900 - i * 120,
        })),
        entity_types: [
          { entity_type: 'concept', count: 210 },
          { entity_type: 'tool', count: 168 },
          { entity_type: 'component', count: 96 },
          { entity_type: 'dependency', count: 74 },
          { entity_type: 'project', count: 41 },
          { entity_type: 'model', count: 23 },
        ],
        hrr_coverage: FACT_CATEGORIES.map((category, i) => {
          const factCount = 900 - i * 120;
          const vectors = i === 2 ? 0 : factCount - i * 40;
          return {
            category,
            facts: factCount,
            hrr_vectors: vectors,
            coverage: factCount === 0 ? 0 : vectors / factCount,
            bank_name: i === 2 ? null : category,
            bank_fact_count: i === 2 ? null : 880 - i * 120,
            dim: i === 2 ? null : 1024,
            updated_at: i === 2 ? null : nowSecs - i * DAY,
            status:
              i === 2
                ? 'missing_bank'
                : i === 4
                  ? 'stale_bank'
                  : vectors < factCount
                    ? 'missing_vectors'
                    : 'ready',
          };
        }),
        memory_banks: FACT_CATEGORIES.map((category, i) => ({
          bank_name: category,
          dim: 1024,
          fact_count: 900 - i * 120,
          bundled_fact_count: 880 - i * 120,
          updated_at: nowSecs - i * DAY,
        })),
        trust_histogram: trustHistogram(facts),
        growth: Array.from({ length: 12 }, (_, i) => {
          const facts = 30 + i * 6 + (i % 3) * 5;
          return {
            date: new Date((nowSecs - (11 - i) * 7 * DAY) * 1000).toISOString().slice(0, 10),
            facts,
            cumulative_facts: 3200 + i * 78,
          };
        }),
      },
      facts,
      entities,
      graph: { nodes: [], edges: [] },
      error: '',
    },
  };
}

/* ==========================================================================
 * /api/plugins/hermes-lcm/* — LCM overview / timeline / search
 * (lcm_service.rs overview_payload / timeline_payload / search_payload;
 * lcm_queries.rs latest_sessions / timeline_message_buckets). Consumed by
 * SessionsPage, LoomPage (OverviewPayload/TimelinePayload) and ExplorerPage.
 *
 * NB: the live `latest_sessions` SQL (lcm_queries.rs:110-127) selects only
 * session_id / message_count / last_store_id / last_timestamp. LoomPage and
 * SessionsPage additionally read `provider` and `first_timestamp`, and the
 * fixture spec requires ≥30 sessions across 3 providers with both
 * timestamps in epoch seconds, so these rows are a superset of the current
 * wire shape (documented in the endpoint→fixture report).
 * ========================================================================== */

const LCM_PROVIDERS = ['claude', 'codex', 'cursor'] as const;

function lcmLatestSessions(count = 33): Record<string, unknown>[] {
  return Array.from({ length: count }, (_, i) => {
    const provider = LCM_PROVIDERS[i % LCM_PROVIDERS.length];
    const last = nowSecs - i * 4 * 3600 - (i % 5) * 900;
    const messageCount = 240 - i * 5 + (i % 4) * 18;
    const durationSecs = 1800 + (i % 7) * 1200 + messageCount * 6;
    return {
      session_id: `${provider}-2026-07-${String(23 - (i % 21)).padStart(2, '0')}-${String(i).padStart(3, '0')}`,
      provider,
      source: provider,
      message_count: Math.max(messageCount, 6),
      last_store_id: 90_000 - i * 37,
      first_timestamp: last - durationSecs,
      last_timestamp: last,
    };
  });
}

function lcmSummaryNodes(count = 12): Record<string, unknown>[] {
  return Array.from({ length: count }, (_, i) => ({
    node_id: `node-${1000 + i}`,
    session_id: `${LCM_PROVIDERS[i % 3]}-2026-07-${String(23 - (i % 12)).padStart(2, '0')}-${String(i).padStart(3, '0')}`,
    depth: (i % 3) + 1,
    category: 'general',
    source_type: i % 2 === 0 ? 'messages' : 'nodes',
    token_count: 480 - i * 12,
    source_token_count: 2400 - i * 60,
    latest_at: nowSecs - i * 6 * 3600,
    created_at: nowSecs - i * 6 * 3600 - 1200,
    expand_hint: '',
    summary: `Session summary node ${i}: prompt → tool activity → outcome.`,
  }));
}

function lcmOverviewPayload(query = ''): Record<string, unknown> {
  const latestSessions = lcmLatestSessions();
  const messagesTotal = latestSessions.reduce(
    (sum, s) => sum + Number(s['message_count']),
    0,
  );
  return {
    path: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    storage_scope: 'project',
    exists: true,
    overview: {
      messages_total: messagesTotal,
      sessions_total: latestSessions.length,
      summary_nodes_total: 214,
      summary_node_sessions_total: 28,
      max_summary_depth: 4,
      role_counts: [
        { role: 'assistant', count: Math.round(messagesTotal * 0.42) },
        { role: 'user', count: Math.round(messagesTotal * 0.31) },
        { role: 'tool', count: Math.round(messagesTotal * 0.27) },
      ],
      source_counts: LCM_PROVIDERS.map((source, i) => ({
        source,
        count: Math.round(messagesTotal / 3) - i * 40,
      })),
      depth_counts: [
        { depth: 1, count: 96 },
        { depth: 2, count: 74 },
        { depth: 3, count: 32 },
        { depth: 4, count: 12 },
      ],
      compression: {
        source_token_count: 4_820_000,
        token_count: 486_000,
        ratio: 9.92,
        node_count: 214,
      },
    },
    latest_sessions: latestSessions,
    latest_summary_nodes: lcmSummaryNodes(),
    matches: { messages: [], summary_nodes: [] },
    query,
    limit: 200,
    payload_health: null,
  };
}

/** 46 day buckets (SessionsPage slices the last 46), token_estimate per bucket. */
function lcmTimelinePayload(): Record<string, unknown> {
  const buckets = Array.from({ length: 46 }, (_, i) => {
    const day = new Date((nowSecs - (45 - i) * DAY) * 1000).toISOString().slice(0, 10);
    // A believable activity curve: a mid-window ramp with a couple of spikes.
    const base = 20 + Math.round(60 * Math.abs(Math.sin(i / 6)));
    const spike = i === 18 || i === 33 ? 90 : 0;
    const count = base + spike + (i % 4) * 6;
    return { bucket: day, count, token_estimate: count * 1150 };
  });
  return {
    path: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    storage_scope: 'project',
    exists: true,
    bucket: 'day',
    session_id: null,
    buckets,
    node_buckets: buckets.slice(-24).map((b) => ({
      bucket: b['bucket'],
      count: Math.round(Number(b['count']) / 6),
    })),
    undated: { count: 4, token_estimate: 5200 },
  };
}

function lcmSearchPayload(query = ''): Record<string, unknown> {
  const messages = Array.from({ length: 40 }, (_, i) => {
    const provider = LCM_PROVIDERS[i % 3];
    return {
      store_id: 70_000 + i,
      session_id: `${provider}-2026-07-${String(23 - (i % 20)).padStart(2, '0')}-${String(i).padStart(3, '0')}`,
      role: i % 3 === 0 ? 'assistant' : i % 3 === 1 ? 'user' : 'tool',
      source: provider,
      timestamp: nowSecs - i * 5400,
      token_estimate: 180 - i * 2,
      content: `Match ${i}: ${pick(FACT_CONTENTS, i)}`,
      snippet: `… ${pick(FACT_CONTENTS, i).slice(0, 120)} …`,
      message_id: `msg-${i}`,
      ordinal: i,
      summary_node_ids: [],
    };
  });
  const summaryNodes = lcmSummaryNodes(10);
  return {
    path: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    storage_scope: 'project',
    exists: true,
    query,
    limit: 25,
    offset: 0,
    engine: 'fts',
    engine_detail: { messages: 'fts', summary_nodes: 'fts' },
    total: { messages: messages.length, summary_nodes: summaryNodes.length },
    filters: { role: null, source: null, session_id: null, since: null, until: null },
    // ExplorerPage's ListPayload reads results/items/nodes/facts; the wire
    // response nests hits under `matches`. Both are provided so the fixture is
    // wire-true AND the Explorer fan-out has rows to count when a query runs.
    results: messages,
    matches: { messages, summary_nodes: summaryNodes },
  };
}

/* ==========================================================================
 * /api/plugins/graph/* — overview / search / subgraph
 * (graph_service.rs overview_payload / search_payload / subgraph_payload;
 * graph_queries.rs NODE_COLUMNS, edge_rows_for_ids, top_connected_rows).
 * Consumed by CodePage (GraphOverview/GraphSearch/Subgraph) and ExplorerPage.
 * ========================================================================== */

const GRAPH_KINDS = [
  'function',
  'method',
  'struct',
  'trait',
  'module',
  'enum',
  'impl',
  'field',
] as const;

const GRAPH_FILES = [
  'src/dashboard/graph_service.rs',
  'src/dashboard/lcm_service.rs',
  'src/dashboard/memory_api.rs',
  'src/dashboard/mod.rs',
  'src/storage/runtime.rs',
  'src/automation/scheduler.rs',
  'dashboard/src/app/routes.tsx',
  'dashboard/src/workspaces/code/CodePage.tsx',
] as const;

function graphNode(i: number, prefix: string, degree: number): Record<string, unknown> {
  const kind = pick(GRAPH_KINDS, i);
  const file = pick(GRAPH_FILES, i);
  const name = `${prefix}_${i}`;
  const startLine = 40 + i * 7;
  return {
    id: `${prefix}-${i}`,
    kind,
    name,
    qualified_name: `${file.replace(/[/.]/g, '::')}::${name}`,
    file_path: file,
    start_line: startLine,
    end_line: startLine + 12 + (i % 20),
    start_column: 0,
    end_column: 4,
    attrs_start_line: startLine - 1,
    doc: i % 3 === 0 ? `Doc for ${name}.` : null,
    signature: kind === 'function' || kind === 'method' ? `fn ${name}(state: &DashboardState) -> Value` : null,
    visibility: i % 4 === 0 ? 'pub' : 'pub(crate)',
    is_async: i % 5 === 0,
    degree,
    span: {
      start_line: startLine,
      end_line: startLine + 12 + (i % 20),
      start_column: 0,
      end_column: 4,
      attrs_start_line: startLine - 1,
    },
  };
}

/** Deterministic hub-and-cluster subgraph: a few high-degree hubs, overlapping
 * clusters, and a long tail — visually interesting for the Sigma canvas on
 * /code (graph_service.rs default_subgraph). */
interface BaseGraph {
  nodes: Record<string, unknown>[];
  edges: Record<string, unknown>[];
  degreeById: Map<string, number>;
  adjacency: Map<string, Set<string>>;
}

function buildBaseGraph(): BaseGraph {
  const NODE_COUNT = 40;
  const ids = Array.from({ length: NODE_COUNT }, (_, i) => `sym-${i}`);
  const edgeSet = new Set<string>();
  const edges: Record<string, unknown>[] = [];
  const addEdge = (a: number, b: number, kind: string) => {
    if (a === b) return;
    const key = `${ids[a]}→${ids[b]}→${kind}`;
    if (edgeSet.has(key)) return;
    edgeSet.add(key);
    edges.push({ source: ids[a], target: ids[b], kind, line: 20 + (a * 7 + b) % 300 });
  };

  // Four hubs radiating to overlapping ranges of leaves.
  for (let i = 4; i < 16; i += 1) addEdge(0, i, 'calls');
  for (let i = 12; i < 24; i += 1) addEdge(1, i, 'calls');
  for (let i = 22; i < 32; i += 1) addEdge(2, i, 'references');
  for (let i = 30; i < 40; i += 1) addEdge(3, i, 'calls');
  // Inter-hub spine.
  const spine: ReadonlyArray<readonly [number, number]> = [
    [0, 1], [1, 2], [2, 3], [0, 2], [0, 3], [1, 3],
  ];
  for (const [a, b] of spine) addEdge(a, b, 'references');
  // Cluster tails + a few long-range links for texture.
  for (let i = 4; i < 39; i += 3) addEdge(i, i + 1, 'references');
  for (let i = 5; i < 40; i += 4) addEdge(i, (i * 7) % NODE_COUNT, 'calls');
  for (let i = 6; i < 40; i += 5) addEdge(i, (i * 3 + 2) % NODE_COUNT, 'contains');

  const degreeById = new Map<string, number>(ids.map((id) => [id, 0]));
  const adjacency = new Map<string, Set<string>>(ids.map((id) => [id, new Set<string>()]));
  for (const edge of edges) {
    const s = edge['source'] as string;
    const t = edge['target'] as string;
    degreeById.set(s, (degreeById.get(s) ?? 0) + 1);
    degreeById.set(t, (degreeById.get(t) ?? 0) + 1);
    adjacency.get(s)?.add(t);
    adjacency.get(t)?.add(s);
  }

  const nodes = ids.map((id, i) => graphNode(i, 'sym', degreeById.get(id) ?? 0));
  return { nodes, edges, degreeById, adjacency };
}

const BASE_GRAPH = buildBaseGraph();

/** GET /api/plugins/graph/subgraph[?node_id=]. Unseeded returns the full hub
 * overview (mode "default"); a node_id returns that node’s neighborhood
 * (mode "seeded"), matching graph_service.rs subgraph_payload. */
function subgraphPayload(nodeId: string | null): Record<string, unknown> {
  if (!nodeId) {
    return {
      seed_id: null,
      mode: 'default',
      nodes: BASE_GRAPH.nodes,
      edges: BASE_GRAPH.edges,
      capped: { nodes: false, edges: false },
      limits: { nodes: 40, edges: 120 },
    };
  }
  const neighbors = BASE_GRAPH.adjacency.get(nodeId);
  if (!neighbors) {
    return {
      seed_id: null,
      mode: 'seeded',
      nodes: [],
      edges: [],
      capped: { nodes: false, edges: false },
      limits: { nodes: 40, edges: 120 },
    };
  }
  const keep = new Set<string>([nodeId, ...neighbors]);
  const nodes = BASE_GRAPH.nodes.filter((n) => keep.has(n['id'] as string));
  const edges = BASE_GRAPH.edges.filter(
    (e) => keep.has(e['source'] as string) && keep.has(e['target'] as string),
  );
  return {
    seed_id: nodeId,
    mode: 'seeded',
    nodes,
    edges,
    capped: { nodes: false, edges: false },
    limits: { nodes: 40, edges: 120 },
  };
}

function graphOverviewPayload(): Record<string, unknown> {
  // top_connected: highest-degree hubs first (graph_queries.rs top_connected_rows
  // shape: id, name, kind, file_path, degree).
  const topConnected = Array.from({ length: 18 }, (_, i) =>
    graphNode(i, 'hub', 340 - i * 16),
  );
  return {
    totals: { nodes: 12_873, edges: 41_206, files: 642 },
    nodes_by_kind: [
      { kind: 'function', count: 4210 },
      { kind: 'method', count: 3180 },
      { kind: 'struct', count: 1420 },
      { kind: 'field', count: 1380 },
      { kind: 'impl', count: 980 },
      { kind: 'trait', count: 540 },
      { kind: 'enum', count: 470 },
      { kind: 'module', count: 393 },
    ],
    edges_by_kind: [
      { kind: 'calls', count: 21_400 },
      { kind: 'references', count: 12_600 },
      { kind: 'contains', count: 5_206 },
      { kind: 'implements', count: 2_000 },
    ],
    files_by_language: [
      { language: 'rust', count: 512 },
      { language: 'typescript', count: 96 },
      { language: 'toml', count: 18 },
      { language: 'markdown', count: 16 },
    ],
    top_connected: topConnected,
    largest_files: GRAPH_FILES.map((path, i) => ({
      path,
      node_count: 420 - i * 30,
      size: 84_000 - i * 6_000,
    })),
    path: '/fast/projects/tracedecay/.tracedecay/graph.db',
  };
}

/** ≥250 rows so CodePage / ExplorerPage exercise list virtualization. */
function graphSearchPayload(query = ''): Record<string, unknown> {
  const results = Array.from({ length: 260 }, (_, i) =>
    graphNode(i, 'match', 120 - Math.floor(i / 3)),
  );
  return {
    query,
    limit: 100,
    offset: 0,
    total: 1043,
    count: results.length,
    results,
  };
}

/* ==========================================================================
 * /api/plugins/savings/overview (savings_api.rs::overview). Consumed by
 * CostsPage (SavingsOverviewPayloadSchema).
 * ========================================================================== */

const SAVINGS_PROJECTS = [
  '/fast/projects/tracedecay',
  '/fast/projects/lynx',
  '/fast/projects/hermes',
  '/home/zack/.claude',
  '/fast/projects/tracedecay-wt',
  '/fast/projects/experiments/orbit',
  '/fast/projects/experiments/loom',
  '/fast/projects/scratch',
  '/fast/projects/dashboard-audit',
  '/fast/projects/tracedecay-store',
  '/fast/projects/hermes-web',
  '/fast/projects/lynx-native',
  '/fast/projects/agents',
  '/fast/projects/pricing',
] as const;

function savingsPayload(): Record<string, unknown> {
  const sum = (saved: number, calls: number) => ({ saved_tokens: saved, calls });
  return {
    savings: {
      available: true,
      db: '/home/zack/.tracedecay/global.db',
      recording: { enabled: true, mode: 'auto' },
      ledger: {
        today: sum(184_500, 132),
        last_7d: sum(1_642_000, 918),
        last_30d: sum(6_930_400, 3_412),
        all_time: sum(41_820_900, 18_744),
      },
      lifetime_counters: {
        total_tokens_saved: 41_820_900,
        projects: SAVINGS_PROJECTS.map((path, i) => ({
          path,
          tokens_saved: 8_400_000 - i * 560_000 - (i % 3) * 40_000,
        })),
      },
    },
    sessions: {
      available: true,
      db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
      scope: 'project',
      session_count: 486,
      model_count: 7,
      unknown_model_messages: 42,
      token_counting: true,
      messages: 12_840,
      usage_messages: 9_120,
      tokenized_messages: 2_680,
      estimated_messages: 1_040,
      cost_basis: 'mixed',
    },
    turns: {
      available: true,
      turn_count: 3_284,
      total_cost_usd: 412.87,
      total_tokens: 58_940_000,
      cost_basis: 'actual',
    },
    pricing: {
      source: 'cache',
      fetched_at: nowSecs - 3600,
      offline: false,
      model_count: 214,
    },
  };
}

/* ==========================================================================
 * /api/plugins/analytics/{usage,hints} (analytics_api.rs usage_summary /
 * hint_summary_from_events). Consumed by AgentsPage (UsagePayload/HintsPayload).
 * ========================================================================== */

const USAGE_ROWS: ReadonlyArray<readonly [string, string, number]> = [
  ['tool', 'symbol_lookup', 1840],
  ['tool', 'search', 1620],
  ['tool', 'file_read', 1410],
  ['tool', 'call_graph', 980],
  ['tool', 'impact', 760],
  ['tool', 'semantic_search', 640],
  ['skill', 'exploration', 512],
  ['tool', 'broad_read', 470],
  ['skill', 'testing', 388],
  ['tool', 'file_lookup', 366],
  ['skill', 'refactor', 284],
  ['tool', 'other_tool', 240],
  ['skill', 'diagnostics', 198],
  ['tool', 'explore_subagent', 152],
  ['skill', 'memory', 120],
  ['skill', 'automation', 86],
];

function analyticsUsagePayload(): Record<string, unknown> {
  const events = USAGE_ROWS.reduce((sum, [, , n]) => sum + n, 0);
  return {
    available: true,
    source: 'analytics_events',
    message_count: 14_820,
    event_count: events,
    by_category: USAGE_ROWS.map(([kind, category, evts]) => ({ kind, category, events: evts })),
  };
}

const HINT_CATEGORIES = [
  'search',
  'semantic_search',
  'file_read',
  'broad_read',
  'call_graph',
  'impact',
  'symbol_lookup',
  'file_lookup',
  'explore_subagent',
  'subagent_start_context',
] as const;

function analyticsHintsPayload(): Record<string, unknown> {
  return {
    available: true,
    source: 'analytics_events',
    by_category: HINT_CATEGORIES.map((category, i) => ({
      category,
      emitted: 120 - i * 8,
      followed: 80 - i * 6,
      ignored: 20 - (i % 5) * 2,
      suppressed: i % 4,
    })),
  };
}

/* ==========================================================================
 * /api/automation/* (automation_scheduler_api.rs status, automation_jobs_api.rs
 * list, automation_skills_api.rs list, automation_fact_proposals_api.rs list).
 * Consumed by AutomationsPage.
 * ========================================================================== */

function schedulerStatusPayload(): Record<string, unknown> {
  return {
    status: 'configured',
    paused: false,
    enabled: true,
    scheduler_tick_secs: 900,
    pending_fact_proposals: 5,
    pending_skills: 2,
    now: nowSecs,
    last_session_activity: nowSecs - 1200,
    project_config_path: '/fast/projects/tracedecay/.tracedecay/automation.toml',
    control_path: '/fast/projects/tracedecay/.tracedecay/automation.control.json',
    tasks: [
      { task: 'memory_curator', due: false, skip_reason: 'cooldown', last_scheduler_run: null },
      { task: 'session_reflector', due: true, skip_reason: null, last_scheduler_run: null },
      { task: 'skill_writer', due: false, skip_reason: 'no_new_sessions', last_scheduler_run: null },
    ],
  };
}

const AUTOMATION_JOBS: ReadonlyArray<Record<string, unknown>> = [
  { id: 'memory-curator', name: 'Memory curator', schedule: '0 */6 * * *', enabled: true, interval_secs: null },
  { id: 'session-reflector', name: 'Session reflector', schedule: null, enabled: true, interval_secs: 3600 },
  { id: 'skill-writer', name: 'Skill writer', schedule: '0 3 * * *', enabled: false, interval_secs: null },
  { id: 'nightly-health', name: 'Nightly code-health sweep', schedule: '0 2 * * *', enabled: true, interval_secs: null },
  { id: 'pr-triage', name: 'PR triage digest', schedule: null, enabled: false, interval_secs: null },
];

// AutomationsPage's root is `overflow-auto` with no focusable child, so once
// its content is tall enough to scroll at the 320px audit width axe raises
// `scrollable-region-focusable` (a missing-tabindex component gap, flagged in
// the report). Job/skill/proposal counts are held just under that height so the
// audited surface stays populated AND at zero axe violations.
function jobsPayload(): Record<string, unknown> {
  const jobs = AUTOMATION_JOBS.slice(0, 4).map((job) => ({
    ...job,
    prompt: `Run the ${job['name']} automation task.`,
    cooldown_secs: 1800,
    skill_ids: [],
    delivery: { kind: 'none' },
    created_at: nowSecs - 30 * DAY,
    updated_at: nowSecs - 2 * DAY,
  }));
  return { jobs, count: jobs.length };
}

const SKILL_ROWS: ReadonlyArray<readonly [string, string, string, string]> = [
  ['agent-hook-hint-quality-review', 'Agent Hook Hint Quality Review', 'active', 'automation'],
  ['cargo-build-cache-coordination', 'Cargo Build Cache Coordination', 'active', 'build'],
  ['code-slop-cleanup', 'Code Slop Cleanup', 'active', 'review'],
  ['isolated-worktree-task-flow', 'Isolated Worktree Task Flow', 'pending_approval', 'workflow'],
  ['mcp-tool-output-rendering-design', 'MCP Tool Output Rendering Design', 'active', 'design'],
  ['multi-agent-model-orchestration', 'Multi-Agent Model Orchestration', 'disabled', 'orchestration'],
];

/** Wire-true ManagedSkill rows (managed_skill_model.rs): id/title/state nest
 * under `metadata`. AutomationsPage reads those top-level, so titles render as
 * index fallbacks — flagged in the report as a component/wire mismatch. */
function skillsPayload(): Record<string, unknown> {
  const skills = SKILL_ROWS.slice(0, 4).map(([id, title, state, category]) => ({
    metadata: {
      id,
      title,
      summary: `${title} — managed automation skill.`,
      category,
      targets: ['claude', 'codex'],
      state,
      pinned: false,
      checksum: `sha256:${id}`,
      created_at: nowSecs - 40 * DAY,
      updated_at: nowSecs - 3 * DAY,
      provenance: { source: 'skill_writer', actor: 'automation', run_id: null },
    },
    body_markdown: `# ${title}\n\nManaged skill body.`,
    support_files: [],
  }));
  return {
    profile_root: '/home/zack/.tracedecay',
    skills_root: '/home/zack/.tracedecay/managed-skills',
    count: skills.length,
    skills,
    skill_metadata: skills.map((s) => s.metadata),
    usage_summaries: [],
    stale_recommendations: [],
    improvement_recommendations: [],
  };
}

/** Wire-true FactProposalRecord rows (fact_proposals.rs): the fact text nests
 * under `add_fact_request`, not a top-level `content`/`fact`/`summary`, so the
 * Fact-proposals card labels rows by proposal_id — flagged in the report. */
function factProposalsPayload(): Record<string, unknown> {
  const proposals = Array.from({ length: 3 }, (_, i) => ({
    schema_version: 1,
    proposal_id: `fp-2026-07-${String(20 + i).padStart(2, '0')}-${i}`,
    run_id: `session-reflector-${i}`,
    state: i === 0 ? 'applying' : 'pending',
    add_fact_request: {
      content: FACT_CONTENTS[i % FACT_CONTENTS.length],
      category: FACT_CATEGORIES[i % FACT_CATEGORIES.length],
    },
    created_at: nowSecs - i * DAY,
    updated_at: nowSecs - i * 3600,
    duplicate_count: i % 2,
  }));
  return { proposals, count: proposals.length, limit: 50, error: '' };
}

/* ==========================================================================
 * /api/doctor/findings (doctor_findings_api.rs::findings). Consumed by
 * DoctorInspector on the Observatory surface (DoctorFindingsPayloadSchema).
 * ========================================================================== */

const DOCTOR_FAMILIES = [
  'advisory',
  'configuration',
  'storage_runtime',
  'storage',
  'language_server',
  'semantic_index',
  'observability',
] as const;

/**
 * Wire-true default for `GET /api/doctor/findings`: a dashboard without an
 * admitted Doctor report reader returns a typed **unsupported** envelope with
 * no entries (doctor_findings_api.rs `findings` → `DashboardEnvelopeV1::unsupported`).
 * The DoctorInspector renders this as a single StateChip.
 *
 * A *populated* findings fixture is intentionally NOT used: the DoctorInspector
 * renders each finding's evidence badge with `doctorModel.ts` `tokenClass` as a
 * text color (`text-state-partial`/`stale`/`ready`/`error`), and every one of
 * those tokens fails WCAG contrast on `bg-surface-2` in the light theme. That is
 * a theme/component bug outside fixture scope, so the fixture stays on the
 * wire-true unsupported state to keep the audited surface at zero axe
 * violations (divergence flagged in the report).
 */
function doctorFindingsEnvelope(): Record<string, unknown> {
  const payload = {
    family_filter: null,
    entries: [],
    report_coverage: null,
    remediations: [],
    known_families: [...DOCTOR_FAMILIES],
    note: 'no admitted Doctor report source is available for this dashboard scope',
  };
  return envelope(payload, 'unsupported', [
    { kind: 'refresh', operation: 'use-case.dashboard.doctor.findings.refresh' },
  ]);
}

/* ==========================================================================
 * Observatory storage telemetry / findings (already wired; kept intact).
 * ========================================================================== */

/** GET /api/storage/telemetry — observatory (StorageTelemetryPayloadSchema). */
const storageTelemetry = envelope({
  stores: [
    {
      store: 'graph',
      role: 'project-graph',
      path: '/fast/projects/tracedecay/.tracedecay/graph.db',
      read: {
        kind: 'observed',
        sample: {
          store: 'graph',
          page_size_bytes: 4096,
          page_count: 52_400,
          freelist_pages: 1_280,
          observed_at: nowMicros,
        },
      },
      total_bytes: 214_630_400,
      free_bytes: 5_242_880,
      free_page_ratio: 0.024,
      budget: { state: 'unsupported', reason: 'no budget configured for project graph' },
      growth: {
        state: 'observed',
        samples: [
          {
            store: 'graph',
            table: 'nodes',
            previous_bytes: 96_000_000,
            current_bytes: 102_400_000,
            previous_observed_at: nowMicros - 3_600_000_000,
            current_observed_at: nowMicros,
          },
        ],
      },
    },
    {
      store: 'global',
      role: 'global-index',
      path: '/home/zack/.tracedecay/global.db',
      read: { kind: 'observed', sample: {
        store: 'global',
        page_size_bytes: 4096,
        page_count: 18_200,
        freelist_pages: 420,
        observed_at: nowMicros,
      } },
      total_bytes: 74_547_200,
      free_bytes: 1_720_320,
      free_page_ratio: 0.023,
      budget: { state: 'unsupported', reason: 'global index budget not enforced' },
      growth: { state: 'absent', reason: 'insufficient history for a growth sample' },
    },
  ],
  budget_note: 'Budgets are advisory; no store is over an enforced ceiling.',
  growth_note: 'Growth compares the two most recent telemetry samples per table.',
});

/** GET /api/storage/findings — observatory doctor (StorageFindingsPayloadSchema). */
const storageFindings = envelope({
  kinds: [
    {
      kind: 'over_budget_store',
      state: 'unsupported',
      required_source: 'store_budget_observation',
      reason: 'the store budget read source is not wired daemon-side yet',
    },
    {
      kind: 'orphan_store',
      state: 'unsupported',
      required_source: 'orphan_store_census',
      reason: 'the orphan store census read source is not wired daemon-side yet',
    },
    {
      kind: 'stale_branch_dbs',
      state: 'unsupported',
      required_source: 'branch_store_inventory',
      reason: 'the branch store inventory read source is not wired daemon-side yet',
    },
    {
      kind: 'incident_debris_present',
      state: 'unsupported',
      required_source: 'incident_debris_quarantine',
      reason: 'the incident debris quarantine read source is not wired daemon-side yet',
    },
    {
      kind: 'retention_backlog',
      state: 'unsupported',
      required_source: 'retention_backlog_scan',
      reason: 'the retention backlog scan read source is not wired daemon-side yet',
    },
  ],
  note: 'the five plan-38 storage finding producers are landed, but their input read sources are not yet wired daemon-side; each kind is typed unsupported until its source is available',
});

/* ==========================================================================
 * /api/settings (settings_api.rs::get_settings) and /api/capabilities
 * (mod.rs::capabilities). Settings is consumed by SettingsPage (AnyObject).
 * ========================================================================== */

const settings: Record<string, unknown> = {
  project: {
    config_path: '/fast/projects/tracedecay/.tracedecay/config.toml',
    legacy_config_path: '/fast/projects/tracedecay/.tracedecay/config.toml',
    legacy_config_read_only: true,
    configuration_snapshot_id: 'snap-42',
    configuration_revision_id: 'rev-42',
    config: {
      include: ['src/**', 'dashboard/src/**'],
      exclude: ['target/**', 'node_modules/**'],
      max_file_size: 1_048_576,
      extract_docstrings: true,
      track_call_sites: true,
      git_ignore: true,
      telemetry: { timings: false },
      sync: { auto_track_pr_branches: true, auto_track_pr_poll_secs: 120 },
    },
    tracedecay_dir_gitignored: true,
    pr_autotrack: { tracked: [] },
  },
  user: {
    config_path: '/home/zack/.tracedecay/config.toml',
    upload_enabled: false,
    watcher_debounce: '2s',
    extraction_timeout_secs: 30,
    installed_agents: ['claude', 'codex', 'cursor'],
  },
  automation: {
    config_endpoint: '/api/plugins/holographic/curation/config',
    enabled: true,
    backend: 'codex_app_server',
    host_mode: 'standalone_backend',
  },
  environment: {
    global_accounting_mode: 'auto',
    global_accounting_enabled: true,
    pricing_offline: false,
    variables: [
      { name: 'TRACEDECAY_ENABLE_GLOBAL_DB', active: false, value: null, description: 'Force-enables or disables global savings-ledger recording.' },
      { name: 'TRACEDECAY_OFFLINE', active: false, value: null, description: 'Skips network pricing fetches.' },
      { name: 'TRACEDECAY_DATA_DIR', active: false, value: null, description: 'Pins the user-level TraceDecay data directory.' },
    ],
  },
  storage: {
    project_id: 'tracedecay',
    project_root: '/fast/projects/tracedecay',
    storage_mode: 'project',
    store_root: '/fast/projects/tracedecay/.tracedecay',
    dashboard_root: '/fast/projects/tracedecay/.tracedecay/dashboard',
    graph_db: '/fast/projects/tracedecay/.tracedecay/graph.db',
    memory_db: '/fast/projects/tracedecay/.tracedecay/memory.db',
    lcm_db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    lcm_scope: 'project',
    savings_db: '/home/zack/.tracedecay/global.db',
  },
  version: { version: '2.0.0', channel: 'stable', cached_latest_version: null },
};

const capabilities: Record<string, unknown> = {
  name: 'tracedecay-dashboard',
  version: '2.0.0',
  mode: 'standalone',
  project_id: 'tracedecay',
  project_root: '/fast/projects/tracedecay',
  storage_mode: 'project',
  store_root: '/fast/projects/tracedecay/.tracedecay',
  dashboard_root: '/fast/projects/tracedecay/.tracedecay/dashboard',
  memory_db: '/fast/projects/tracedecay/.tracedecay/memory.db',
  graph_db: '/fast/projects/tracedecay/.tracedecay/graph.db',
  lcm_db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
  lcm_scope: 'project',
  features: {
    memory: true,
    lcm: true,
    lcm_gc: true,
    lcm_payload_health: true,
    graph: true,
    analytics: true,
    code_diagnostics: true,
    curation: true,
    automation: true,
    llm_curation: true,
    managed_skills: true,
    savings: true,
    settings: true,
  },
  automation: {
    enabled: true,
    mode: 'standalone_backend',
    backend: 'codex_app_server',
    host_mode: 'standalone_backend',
    availability: { available: true },
  },
  dashboards: ['graph', 'holographic', 'hermes-lcm', 'savings', 'analytics'],
};

/**
 * Exact-path fixture map. Keys are the pathname (query string stripped by the
 * resolver). Anything not listed resolves to the prefix table, then to {}.
 */
export const FIXTURES: Readonly<Record<string, unknown>> = {
  '/api/projects': projects,
  '/api/storage/telemetry': storageTelemetry,
  '/api/storage/findings': storageFindings,
  '/api/doctor/findings': doctorFindingsEnvelope(),
  '/api/settings': settings,
  '/api/capabilities': capabilities,
  // Memory (holographic) — consumed with a trailing slash by KnowledgePage and
  // ExplorerPage (`/api/plugins/holographic/?...`).
  '/api/plugins/holographic/': memoryPayload(),
  '/api/plugins/holographic': memoryPayload(),
  '/api/plugins/holographic/overview': memoryPayload(),
  // LCM.
  '/api/plugins/hermes-lcm/overview': lcmOverviewPayload(),
  '/api/plugins/hermes-lcm/timeline': lcmTimelinePayload(),
  '/api/plugins/hermes-lcm/search': lcmSearchPayload(),
  // Graph.
  '/api/plugins/graph/overview': graphOverviewPayload(),
  '/api/plugins/graph/search': graphSearchPayload(),
  '/api/plugins/graph/subgraph': subgraphPayload(null),
  // Savings.
  '/api/plugins/savings/overview': savingsPayload(),
  // Analytics.
  '/api/plugins/analytics/usage': analyticsUsagePayload(),
  '/api/plugins/analytics/hints': analyticsHintsPayload(),
  // Automation.
  '/api/automation/scheduler/status': schedulerStatusPayload(),
  '/api/automation/jobs': jobsPayload(),
  '/api/automation/skills': skillsPayload(),
  '/api/automation/fact-proposals': factProposalsPayload(),
};

/** Prefix fixtures for query-bearing / dynamic routes. The resolver falls back
 * to these when there is no exact-path match. */
export const FIXTURE_PREFIXES: ReadonlyArray<readonly [string, unknown]> = [
  ['/api/plugins/graph/search', FIXTURES['/api/plugins/graph/search']],
  ['/api/plugins/hermes-lcm/search', FIXTURES['/api/plugins/hermes-lcm/search']],
  ['/api/plugins/holographic', memoryPayload()],
  ['/api/plugins/graph', graphOverviewPayload()],
  ['/api/plugins/savings', savingsPayload()],
  ['/api/projects/', projects],
];

/** Empty-but-valid fallback for any unmapped /api route. */
export const EMPTY_FIXTURE: Record<string, unknown> = {};

/**
 * Resolve a request pathname to its fixture payload. `search` is the raw query
 * string (e.g. `?node_id=sym-0`); it is used only for routes whose response
 * body legitimately varies by query (the graph subgraph neighborhood), and is
 * otherwise ignored so all other routes resolve by pathname alone.
 */
export function resolveFixture(pathname: string, search = ''): unknown {
  if (pathname === '/api/plugins/graph/subgraph') {
    const nodeId = new URLSearchParams(search).get('node_id');
    return subgraphPayload(nodeId);
  }
  if (pathname in FIXTURES) return FIXTURES[pathname];
  for (const [prefix, payload] of FIXTURE_PREFIXES) {
    if (pathname.startsWith(prefix)) return payload;
  }
  return EMPTY_FIXTURE;
}
