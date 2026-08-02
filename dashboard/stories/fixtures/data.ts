/**
 * Canonical fixture payloads for the dashboard `/api` surfaces. These stand in
 * for a running daemon so the visual audit and DOM/MSW tests never require the
 * live API to be up (plan 11a). Both the MSW handlers (`handlers.ts`) and the
 * Playwright route interceptor (`route.ts`) resolve from this single source, so
 * fixtures stay consistent across test transports.
 *
 * Shapes are hand-matched, endpoint by endpoint, to the Rust producers in
 * `src/dashboard/*`, and two suites hold them there: `data.test.ts` parses
 * every fixture against the generated contract for its route, and
 * `src/workspaces/endpoint-fixtures.test.ts` parses it against what the
 * consuming workspace decodes and how densely the surface needs it populated.
 * Every route the 12 workspaces read is modeled with data-dense, wire-true
 * payloads so audited surfaces render populated content rather than empty /
 * "unsupported schema" states.
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

/** DashboardEnvelopeV1 wrapper (see DashboardEnvelopeV1Schema in wire.ts). */
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
  mass?: { stores: number; scopes: number; artifacts: number },
): Record<string, unknown> {
  return {
    project_id: id,
    label,
    project_root: root,
    canonical_root: root,
    kind: 'git',
    default_branch: 'master',
    branches: ['master', 'codex/tracedecay-total-redesign-plan'],
    store_count: mass?.stores ?? 3,
    graph_scope_count: mass?.scopes ?? 2,
    artifact_count: mass?.artifacts ?? 7,
    alias_count: 1,
    last_seen_at: nowSecs - ageSecs,
    is_active: id === 'tracedecay',
  };
}

/**
 * The real registry is dozens of repositories with one checkout each, spread
 * across months of last-contact and three orders of magnitude of indexed mass.
 * Brain's field is a composition OF that spread, so a three-repo fixture could
 * not exercise it: every audited screenshot would show one column and tell a
 * reviewer nothing about the layout being reviewed. These entries are generated
 * from the same `projectEntry` shape as the hand-written ones above (so they
 * stay gated by the parse test), and their ages and masses are derived from the
 * index — deterministic, never random — to land bodies in every recency column
 * and across the mass axis.
 */
const SYNTHETIC_REPOS: ReadonlyArray<{
  name: string;
  ageSecs: number;
  mass: number;
  branches: number;
}> =
  [
    'rslint', 'vite-rsbuild', 'mold', 'core', 'ai-train', 'browser-linux',
    'cargo-slot', 'ci-runner-orchestrator', 'claude-code', 'codex-cli',
    'graphology-fork', 'hermes-lcm', 'lynx-stack', 'module-federation',
    'nextjs-app', 'rspack', 'sigma-fork', 'turbo-cache', 'wasm-host',
    'zed-extensions', 'polars-bench', 'sqlite-vfs', 'tokio-probe',
  ].map((name, index) => ({
    name,
    // 0.6h · 1.9 ^ index — a geometric spread from "minutes ago" out past a
    // year, so every recency column is occupied and none is crowded.
    ageSecs: Math.round(2_160 * 1.9 ** index),
    // Masses that cycle through four magnitudes rather than tracking age, so
    // the two axes stay independent and the field is not a diagonal line.
    mass: [1, 4, 9, 22, 58, 140, 310][index % 7]!,
    // Branch counts spanning the real registry's range (1 to 242 on the
    // owner's profile) rather than the flat `['main']` this fixture used to
    // give every repository. The Delivery field's y axis is a log of this
    // number; with every repo on one branch the axis was a single line and the
    // scale was never exercised (plan 11a real-profile finding 4). Cycled on a
    // different period from `mass` so branch count and indexed mass stay
    // independent measurements.
    branches: [1, 3, 71, 8, 242, 20, 2, 56, 5, 36, 10][index % 11]!,
  }));

/** Branch names for a repository, shaped like a real one: `main`, then a
 * spread of the prefixes this registry actually carries. */
function branchNames(count: number): string[] {
  const prefixes = ['feat', 'fix', 'refactor', 'test', 'wt', 'integrate'];
  return [
    'main',
    ...Array.from(
      { length: Math.max(count - 1, 0) },
      (_, index) => `${prefixes[index % prefixes.length]}/branch-${index}`,
    ),
  ];
}

function syntheticGroup(repo: {
  name: string;
  ageSecs: number;
  mass: number;
  branches: number;
}) {
  const root = `/fast/projects/${repo.name}`;
  return {
    label: repo.name,
    git_common_dir: `${root}/.git`,
    project_count: 1,
    branches: branchNames(repo.branches),
    projects: [
      {
        ...projectEntry(`proj_${repo.name}`, repo.name, root, repo.ageSecs, {
          stores: 1,
          scopes: Math.max(1, Math.round(repo.mass * 0.35)),
          artifacts: Math.max(1, Math.round(repo.mass * 0.65)),
        }),
        kind: 'primary',
      },
    ],
  };
}

/** GET /api/projects — brain/delivery registry (ProjectsPayloadV1Schema;
 * src/dashboard/projects.rs `list`). */
const projectTree: ReadonlyArray<Record<string, unknown>> = [
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
    ...SYNTHETIC_REPOS.map(syntheticGroup),
    // Registry entries that are NOT git checkouts. TraceDecay indexes plain
    // directories too, and eight of the forty-four entries on the owner's real
    // profile are in this class. Their branch count is UNKNOWN, not zero, and
    // the Delivery field draws them in a fenced band below the measured plot —
    // so the fixture has to contain some, or that band never renders under
    // audit and the distinction goes unverified.
    {
      label: '.hermes',
      git_common_dir: null,
      project_count: 1,
      branches: [],
      projects: [
        {
          ...projectEntry('proj_hermes_home', '.hermes', '/home/zack/.hermes', 20 * DAY, {
            stores: 1,
            scopes: 0,
            artifacts: 3,
          }),
          kind: 'project',
          default_branch: null,
        },
      ],
    },
    {
      label: 'notes',
      git_common_dir: null,
      project_count: 1,
      branches: [],
      projects: [
        {
          ...projectEntry('proj_notes', 'notes', '/home/zack/notes', 3 * DAY, {
            stores: 1,
            scopes: 0,
            artifacts: 1,
          }),
          kind: 'project',
          default_branch: null,
        },
      ],
    },
];

/** `projects.rs` answers with BOTH shapes: the grouped `project_tree` the Brain
 * field draws, and a flat `projects` list of `PublicCodeProject` — a narrower
 * record with `created_at`, `display_root` and `git_common_dir` that the
 * registry entries do not carry. Derived from the tree so the two views can
 * never disagree about which projects exist. */
const flatProjects: ReadonlyArray<Record<string, unknown>> = projectTree.flatMap((group) =>
  (group['projects'] as ReadonlyArray<Record<string, unknown>>).map((entry) => ({
    project_id: entry['project_id'],
    label: entry['label'],
    project_root: entry['project_root'],
    canonical_root: entry['canonical_root'],
    display_root: entry['project_root'],
    git_common_dir: group['git_common_dir'],
    default_branch: entry['default_branch'],
    created_at: (entry['last_seen_at'] as number) - 30 * DAY,
    last_seen_at: entry['last_seen_at'],
    is_active: entry['is_active'],
  })),
);

const projects: Record<string, unknown> = {
  status: 'ok',
  limit: 100,
  truncated: false,
  projects: flatProjects,
  active_project_id: 'tracedecay',
  active_project_root: '/fast/projects/tracedecay',
  summary: {
    // tracedecay (2 checkouts) + lynx + hermes + the two non-git entries.
    project_count: 6 + SYNTHETIC_REPOS.length,
    repo_count: 5 + SYNTHETIC_REPOS.length,
    truncated: false,
  },
  project_tree: projectTree,
};

/* ==========================================================================
 * /api/plugins/holographic/ — memory overview + facts + entities
 * (memory_api.rs::overview; facts.rs fact_summary_json / entity_json /
 * overview_payload / trust_histogram). Consumed by KnowledgePage
 * (MemoryOverviewPayloadV1Schema) and ExplorerPage memory source.
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
      // `fact_summary_json` never attaches entities to a summary row; the typed
      // row carries the null the decode left behind.
      entities: null,
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
  ['rusqlite', 'dependency', 14],
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
      // Per-read outcome, seeded `pending` and overwritten as each of the three
      // reads lands (memory_api.rs::overview). All three succeeded here.
      reads: {
        facts: { state: 'ready' },
        entities: { state: 'ready' },
        graph: { state: 'ready' },
      },
      // The fact list is bounded by `limit`, and the query — empty here — is
      // applied after that bound, so `bounded` is what the route reports.
      facts_coverage: { completeness: 'bounded', limit: 100, query_applied_after_limit: query !== '' },
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
    // `lcm_service::timeline_payload` always attaches this alongside a real
    // read — it is only absent when there is no LCM store at all, and this
    // fixture says `exists: true`. `limit` is the route's own default
    // (`coerce_limit(params.limit, 400, 2000)`), and with every dated bucket
    // returned the window is untruncated, so there is no next page cursor.
    coverage: {
      limit: 400,
      returned_buckets: buckets.length,
      total_dated_buckets: buckets.length,
      truncated: false,
      ordering: 'most_recent',
      next_before_bucket: null,
    },
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

/**
 * Every `GraphNodeV1` key at its absent value.
 *
 * A query that selects a subset of `NODE_COLUMNS` still deserializes into the
 * full struct, so the columns it left out reach the browser as explicit nulls.
 * Spreading this first is what keeps a partial row wire-true instead of merely
 * short.
 */
const GRAPH_NODE_ABSENT = {
  name: null,
  qualified_name: null,
  file_path: null,
  start_line: null,
  end_line: null,
  start_column: null,
  end_column: null,
  attrs_start_line: null,
  doc: null,
  signature: null,
  visibility: null,
  is_async: null,
  branches: null,
  loops: null,
  returns: null,
  max_nesting: null,
  unsafe_blocks: null,
  unchecked_calls: null,
  assertions: null,
  updated_at: null,
  parent_id: null,
  degree: null,
  span: null,
  edge_kind: null,
  edge_line: null,
} as const;

/**
 * One node exactly as `GraphNodeV1` serializes it.
 *
 * Every key is present because none of the Rust fields is
 * `skip_serializing_if`: an absent column reaches the browser as an explicit
 * null, not as a missing key. The metric columns are SQLite integers, so
 * `is_async` is 0/1 rather than a boolean — a fixture that sent `true` here
 * would be testing the surface against a payload the daemon cannot produce.
 * `edge_kind` and `edge_line` are null on every row except the caller/callee
 * rows of the neighbors route, which set them per edge.
 */
function graphNode(i: number, prefix: string, degree: number): Record<string, unknown> {
  const kind = pick(GRAPH_KINDS, i);
  const file = pick(GRAPH_FILES, i);
  const name = `${prefix}_${i}`;
  const startLine = 40 + i * 7;
  const callable = kind === 'function' || kind === 'method';
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
    signature: callable ? `fn ${name}(state: &DashboardState) -> Value` : null,
    visibility: i % 4 === 0 ? 'pub' : 'pub(crate)',
    is_async: i % 5 === 0 ? 1 : 0,
    // Complexity columns are extracted for callables only; everything else
    // carries the nulls the extractor left behind.
    branches: callable ? i % 7 : null,
    loops: callable ? i % 3 : null,
    returns: callable ? 1 + (i % 2) : null,
    max_nesting: callable ? 1 + (i % 4) : null,
    unsafe_blocks: callable ? 0 : null,
    unchecked_calls: callable ? i % 5 : null,
    assertions: callable ? i % 2 : null,
    updated_at: 1_784_000_000 + i * 37,
    parent_id: i % 6 === 0 ? null : `${prefix}-${Math.max(i - (i % 6), 0)}`,
    degree,
    span: {
      start_line: startLine,
      end_line: startLine + 12 + (i % 20),
      start_column: 0,
      end_column: 4,
      attrs_start_line: startLine - 1,
    },
    edge_kind: null,
    edge_line: null,
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
    // `edge_rows_for_ids` groups on (source, target, kind) and never joins
    // `nodes`, so the subgraph's edges carry null names — unlike the neighbors
    // route, which does resolve them.
    edges.push({
      source: ids[a],
      target: ids[b],
      kind,
      line: 20 + ((a * 7 + b) % 300),
      source_name: null,
      target_name: null,
    });
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

/* ---- GET /api/plugins/graph/node/{id}/neighbors -------------------------
 *
 * Wire-true against `graph_service.rs::neighbors_payload`, which composes
 * three `graph_queries.rs` reads. The shape details this fixture exists to
 * reproduce, because the TRACE drill-in derives every figure it prints from
 * them:
 *
 *  - `caller_rows` / `callee_rows` select `NODE_COLUMNS_N` plus `edge_kind`
 *    and `edge_line`, filtered to `e.kind = 'calls'`, joined through `edges`.
 *    So they emit ONE ROW PER EDGE: a caller with four call sites appears
 *    four times, same node columns, different `edge_line`. The call-site
 *    count of a pair is the number of its rows, and nothing else on the wire
 *    carries it.
 *  - each row then passes through `node_with_span` (adds `span`) and
 *    `attach_degrees` (adds `degree`, the node's total in+out edge count over
 *    ALL edge kinds — 0 when the node has none).
 *  - `neighborhood_edge_rows` returns `source, target, kind, line,
 *    source_name, target_name` for every edge kind where `source = ?1 OR
 *    target = ?1`. That WHERE clause is why a `contains` row in this payload
 *    always has the requested node as one endpoint: it is the container OF
 *    the requested node, never a sibling's container.
 *  - `neighborhood_edge_counts` groups the same incident set by kind.
 *
 * The generated neighbourhood is deterministic in the node id, so hop-2
 * expansion (the drill-in fetches its hop-1 neighbours' neighbours) resolves
 * to a stable field for the audit and the DOM tests.
 */

function fixtureHash(value: string): number {
  let hash = 0;
  for (let i = 0; i < value.length; i += 1) hash = (hash * 31 + value.charCodeAt(i)) >>> 0;
  return hash;
}

/** Container pool, so several neighbours share an enclosure and the drill-in
 * can derive a membrane from real `contains` rows rather than from guesswork. */
const GRAPH_CONTAINERS = [
  { id: 'impl-retrieval', name: 'impl RetrievalService' },
  { id: 'impl-graph-routes', name: 'impl GraphRoutes' },
  { id: 'trait-context-source', name: 'trait ContextSource' },
] as const;

interface NeighborPair {
  readonly index: number;
  readonly id: string;
  readonly calls: number;
}

/** The distinct neighbours of a node on one side, with their call-site counts. */
function neighborPairs(nodeId: string, side: 'callers' | 'callees'): NeighborPair[] {
  const hash = fixtureHash(`${nodeId}:${side}`);
  const count = 3 + (hash % 5);
  const pairs: NeighborPair[] = [];
  const seen = new Set<number>();
  for (let i = 0; i < count; i += 1) {
    const index = (hash + i * 11 + (side === 'callers' ? 0 : 5)) % 40;
    if (seen.has(index)) continue;
    seen.add(index);
    // Call sites per pair: power-law-ish, one dominant channel then a tail —
    // the shape a real `calls` edge multiset has, and what makes the channel
    // widths and spring stiffnesses on this surface tellable apart.
    const id = `sym-${index}`;
    pairs.push({ index, id, calls: 1 + (fixtureHash(`${nodeId}->${id}:${side}`) % 11) });
  }
  return pairs;
}

/** GET /api/plugins/graph/node/{node_id}/neighbors. */
function neighborsPayload(nodeId: string, limit: number): Record<string, unknown> {
  const callerPairs = neighborPairs(nodeId, 'callers');
  const calleePairs = neighborPairs(nodeId, 'callees');
  const edges: Record<string, unknown>[] = [];
  const byKind = new Map<string, number>();
  const bump = (kind: string) => byKind.set(kind, (byKind.get(kind) ?? 0) + 1);

  const expand = (pairs: NeighborPair[], side: 'callers' | 'callees') =>
    pairs.flatMap((pair) => {
      const degree = BASE_GRAPH.degreeById.get(pair.id) ?? 0;
      const base = graphNode(pair.index, 'sym', degree);
      return Array.from({ length: pair.calls }, (_, site) => {
        const line = 60 + pair.index * 13 + site * 4;
        edges.push({
          source: side === 'callers' ? pair.id : nodeId,
          target: side === 'callers' ? nodeId : pair.id,
          kind: 'calls',
          line,
          source_name: side === 'callers' ? base['name'] : nodeId,
          target_name: side === 'callers' ? nodeId : base['name'],
        });
        bump('calls');
        return { ...base, edge_kind: 'calls', edge_line: line };
      });
    });

  const callers = expand(callerPairs, 'callers').slice(0, limit);
  const callees = expand(calleePairs, 'callees').slice(0, limit);

  // The container OF this node — one `contains` row, with this node as the
  // target, exactly as the endpoint's `source = ?1 OR target = ?1` filter
  // allows. Membranes on the drill-in are the transitive result of collecting
  // these across the focus and its expanded neighbours.
  const container = pick(GRAPH_CONTAINERS, fixtureHash(nodeId) % 3);
  edges.push({
    source: container.id,
    target: nodeId,
    kind: 'contains',
    line: 12,
    source_name: container.name,
    target_name: nodeId,
  });
  bump('contains');
  // One non-call, non-contains kind, so a consumer that assumes `edges` is
  // homogeneous fails here rather than in production.
  edges.push({
    source: nodeId,
    target: `sym-${(fixtureHash(nodeId) + 3) % 40}`,
    kind: 'references',
    line: 240,
    source_name: nodeId,
    target_name: `sym_${(fixtureHash(nodeId) + 3) % 40}`,
  });
  bump('references');

  return {
    node_id: nodeId,
    depth: 1,
    limit,
    callers,
    callees,
    edges: edges.slice(0, limit),
    edges_by_kind: [...byKind]
      .map(([kind, count]) => ({ kind, count }))
      .sort((a, b) => b.count - a.count || a.kind.localeCompare(b.kind)),
  };
}

/** GET /api/plugins/graph/subgraph[?node_id=]. Unseeded returns the full hub
 * overview (mode "default"); a node_id returns that node’s neighborhood
 * (mode "seeded"), matching graph_service.rs subgraph_payload. */
// `coerce_limit(params.limit_nodes, 80, 250)` / `(params.limit_edges, 120,
// 500)` in graph_api.rs: the defaults are 80 and 120, not 40. The Code
// workspace now prints these limits in the canvas caption, so a wrong number
// here would be a wrong number on screen.
function subgraphPayload(nodeId: string | null): Record<string, unknown> {
  if (!nodeId) {
    return {
      seed_id: null,
      mode: 'default',
      nodes: BASE_GRAPH.nodes,
      edges: BASE_GRAPH.edges,
      capped: { nodes: false, edges: false },
      limits: { nodes: 80, edges: 120 },
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
      limits: { nodes: 80, edges: 120 },
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
  // top_connected: highest-degree hubs first. This row set had drifted away
  // from its own stated contract — it emitted 18 full node records, while
  // `graph_queries::top_connected_rows` selects exactly FIVE columns from a
  // `LIMIT 12` subquery and never joins `qualified_name`. The Code workspace
  // renders these rows directly, so the audit was judging a payload the daemon
  // cannot produce. Restored to the real shape, including the real curve:
  // degree in a symbol graph is power-law, not linear — one run-away hub, a
  // steep fall, then near-ties bunching at the bottom of the twelve.
  //
  // The row still deserializes into the whole `GraphNodeV1`, so the twenty-two
  // columns the query never asked for go out as nulls rather than as absent
  // keys. Only those five carry a value.
  //
  // The NAMES matter as much as the degrees, and `hub_0 … hub_11` hid the
  // single hardest thing about this row set. On a real Rust graph the most
  // connected symbols are language primitives and one-word generics — `path`,
  // `json`, `u64`, `Value`, `trim`, `kind` — and two of the owner's twelve are
  // literally the same word in different files. `top_connected_rows` does not
  // serve `qualified_name`, so the file is the ONLY thing that can tell them
  // apart, and a fixture of unique invented names meant the card never had to.
  const HUBS: ReadonlyArray<readonly [string, string, string, number]> = [
    ['path', 'function', 'src/dashboard/graph_api.rs', 1840],
    ['path', 'method', 'src/automation/skill_materialization.rs', 1461],
    ['json', 'function', 'crates/tracedecay-rusqlite-runtime/src/repair/sqlite.rs', 1218],
    ['Value', 'enum_variant', 'src/dashboard/code_diagnostics_api.rs', 1093],
    ['u64', 'method', 'src/application/session/refresh.rs', 837],
    ['as_str', 'method', 'src/memory/types.rs', 811],
    ['trim', 'function', 'scripts/render-codex-hook-inputs.py', 704],
    ['i64', 'method', 'src/application/session/refresh.rs', 551],
    ['kind', 'method', 'crates/tracedecay-tool-catalog/src/profile.rs', 546],
    ['find_direct_child_by_kind', 'function', 'src/extraction/traversal.rs', 545],
    ['test', 'annotation_usage', 'src/branch/admin/tests.rs', 399],
    ['u32', 'impl', 'src/db/engine/value.rs', 390],
  ];
  const topConnected = HUBS.map(([name, kind, file_path, degree], i) => ({
    ...GRAPH_NODE_ABSENT,
    id: `${kind}:hub-${i}`,
    name,
    kind,
    file_path,
    degree,
  }));
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
 * CostsPage (SavingsOverviewPayloadV1Schema).
 * ========================================================================== */

/**
 * Per-project lifetime savings at the SHAPE the real ledger has, not a smooth
 * ramp.
 *
 * On a machine where every worktree of one repository shares a cache, every
 * worktree records almost exactly the same lifetime saving: twenty of the
 * owner's twenty-five rows sit within a few percent of 1.80B. The fixture used
 * to ramp evenly from 8.4M down to 1.1M, which made twenty-five equal-length
 * rails look like a legitimate ranking in every audit shot. Two rows genuinely
 * deviate — the primary checkout above and a small unrelated repository well
 * below — and those are the only rows worth drawing.
 */
const SAVINGS_PROJECTS: ReadonlyArray<readonly [string, number]> = [
  ['/fast/projects/tracedecay', 2_939_894_592],
  ['/fast/projects/tracedecay/.worktrees/sqlite-storage-runtime', 2_140_723_247],
  ['/fast/projects/tracedecay/.worktrees/live-repair', 2_078_590_272],
  ['/fast/projects/tracedecay/.worktrees/runtime-hardening', 1_946_100_344],
  ['/fast/projects/tracedecay/.worktrees/pr8-migration', 1_831_192_520],
  ['/fast/projects/tracedecay/.worktrees/pr8-acceptance-runner', 1_824_171_535],
  ['/fast/projects/tracedecay/.worktrees/pr8-live-tools', 1_824_065_209],
  ['/fast/projects/tracedecay/.worktrees/pr8-move-symbol', 1_802_722_260],
  ['/fast/projects/tracedecay/.worktrees/pr8-kernel', 1_801_796_023],
  ['/fast/projects/tracedecay/.worktrees/plan-topology-integration', 1_799_923_909],
  ['/fast/projects/tracedecay/.worktrees/pr8-refresh', 1_799_813_188],
  ['/fast/projects/tracedecay/.worktrees/pr8-runtime', 1_799_356_160],
  ['/fast/projects/tracedecay/.worktrees/pr8-compat', 1_796_821_496],
  ['/fast/projects/tracedecay/.worktrees/plan-dashboard', 1_799_400_112],
  ['/fast/projects/tracedecay/.worktrees/plan-task-runtime', 1_799_402_004],
  ['/fast/projects/tracedecay/.worktrees/plan-lsp-hooks', 1_799_398_771],
  ['/fast/projects/tracedecay/.worktrees/plan-git-stack', 1_799_401_330],
  ['/fast/projects/tracedecay/.worktrees/plan-policy-anchors', 1_799_399_006],
  ['/fast/projects/tracedecay/.worktrees/pr8-automation', 1_799_400_845],
  ['/fast/projects/tracedecay/.worktrees/pr8-benchmark', 1_799_397_612],
  ['/fast/projects/tracedecay/.worktrees/pr8-transport', 1_799_400_501],
  ['/fast/projects/tracedecay/.worktrees/pr8-context', 1_799_400_009],
  ['/fast/projects/lynx', 1_802_004_118],
  ['/fast/projects/hermes', 1_796_100_530],
  ['/fast/projects/tracedecay-astgrep', 380_112_004],
];

function savingsPayload(): Record<string, unknown> {
  const sum = (saved: number, calls: number) => ({ saved_tokens: saved, calls });
  return {
    savings: {
      available: true,
      db: '/home/zack/.tracedecay/global.db',
      error: null,
      recording: { enabled: true, mode: 'auto' },
      ledger: {
        today: sum(27_897_298, 222),
        last_7d: sum(1_850_569_717, 52_371),
        last_30d: sum(4_410_909_252, 134_767),
        all_time: sum(4_902_796_408, 147_230),
      },
      lifetime_counters: {
        total_tokens_saved: SAVINGS_PROJECTS.reduce((sum, [, saved]) => sum + saved, 0),
        projects: SAVINGS_PROJECTS.map(([path, tokens_saved]) => ({ path, tokens_saved })),
        // `savings_api` lists at most `PROJECT_LIMIT` (25) project rows and
        // reports the true distinct-project count beside them, so the surface
        // can say how many it is not showing.
        project_total: SAVINGS_PROJECTS.length,
        projects_limit: 25,
        projects_truncated: false,
      },
    },
    // The session ledger's own accounting, which CostsPage now reads for the
    // token mix and the measured/estimated split. Both were absent from this
    // fixture, so both plates were unrenderable under audit.
    //
    // The `actual` split carries the real profile's proportions: cache reads
    // are 98% of every token, which is the whole reason the plate states its
    // leader instead of drawing it. And `usage_messages` is a SMALL fraction of
    // `messages` — the previous fixture had it at 71%, which made a "mixed"
    // cost basis look almost fully measured when in practice it is the reverse.
    sessions: {
      available: true,
      db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
      scope: 'profile_sharded',
      // `status`/`error` carry `read_failed` and a message when the session
      // ledger read fails; a successful read leaves both null.
      status: null,
      error: null,
      session_count: 6_054,
      model_count: 41,
      unknown_model_messages: 187_066,
      token_counting: true,
      messages: 1_751_214,
      usage_messages: 138_317,
      tokenized_messages: 0,
      estimated_messages: 1_612_897,
      cost_basis: 'mixed',
      actual: {
        cache_read_tokens: 365_936_726_111,
        cache_write_tokens: 243_694_418,
        input_tokens: 7_256_407_982,
        output_tokens: 858_386_349,
      },
      estimated: {
        input_tokens: 118_484_292,
        output_tokens: 51_104_049,
      },
      tokenized: { input_tokens: 0, output_tokens: 0 },
    },
    turns: {
      available: true,
      status: null,
      error: null,
      turn_count: 57_704,
      total_cost_usd: 8148.9744974,
      total_tokens: 683_965_063,
      cost_basis: 'actual',
    },
    pricing: {
      source: 'cache',
      fetched_at: nowSecs - 3600,
      offline: false,
      model_count: 214,
    },
    costs: costsReadModel(),
  };
}

/** The Plan 26 Costs projection `savings_api::overview` embeds
 * (`application::observability::costs_read_model`, called unscoped and from
 * epoch). Its numbers are the same ledger totals the summaries above report,
 * because they are read from the same store.
 *
 * `provider_cost` is deliberately valueless: prices are recorded at ingest and
 * this projection never recomputes them, so with no pricing revision it reports
 * `unavailable_reason` rather than a dollar figure. That one unknown-coverage
 * metric is what makes the model `current: false`. */
function costsReadModel(): Record<string, unknown> {
  const observedAtMicros = nowMicros;
  const horizon = { since_micros: 0, until_micros: observedAtMicros };
  const accountingWatermark = 'turns:57704:1784052000';
  const savingsWatermark = 'savings:1784052117';
  const known = (eligible: number) => ({
    eligible,
    observed: eligible,
    completed: eligible,
    censored: 0,
    unknown: 0,
    excluded: 0,
    state: 'known',
  });
  const measurement = (
    metric: string,
    value: number | null,
    unit: string,
    denominator: string,
    coverage: Record<string, unknown>,
    source: string,
    sourceRevision: string,
    watermark: string,
    unavailableReason: string | null,
  ) => ({
    descriptor_revision: 'provider-costs.v1',
    metric,
    value,
    unit,
    denominator,
    denominator_value: coverage['eligible'],
    coverage,
    evidence_class: 'measurement',
    provenance: {
      source,
      source_revision: sourceRevision,
      projector_revision: 'costs-projector.v1',
      watermark,
    },
    cohort: { descriptor_revision: `${denominator}.v1`, eligible_population: denominator },
    temporal: { horizon, baseline_watermark: null, delta: null },
    uncertainty:
      value === null
        ? { lower: null, upper: null, reason: unavailableReason }
        : { lower: value, upper: value, reason: null },
    calibration: null,
    unavailable_reason: unavailableReason,
  });
  return {
    authorized_scope_ref: 'all',
    horizon,
    watermark: `${accountingWatermark};${savingsWatermark}`,
    observed_at_micros: observedAtMicros,
    current: false,
    usage: [
      measurement(
        'provider_tokens',
        683_965_063,
        'tokens',
        'ingested_provider_turns',
        known(57_704),
        'accounting_turn',
        'accounting-turn.v1',
        accountingWatermark,
        null,
      ),
      measurement(
        'saved_tokens',
        4_902_796_408,
        'tokens',
        'eligible_savings_calls',
        known(147_230),
        'savings_ledger',
        'savings-ledger.v1',
        savingsWatermark,
        null,
      ),
    ],
    estimated_cost: [
      measurement(
        'provider_cost',
        null,
        'usd',
        'priced_provider_turns',
        {
          eligible: null,
          observed: 57_704,
          completed: 57_704,
          censored: 0,
          unknown: 1,
          excluded: 0,
          state: 'unknown',
        },
        'accounting_turn',
        'accounting-turn.v1',
        accountingWatermark,
        'pricing_revision_unavailable',
      ),
    ],
    pricing_revision: null,
  };
}

/* ==========================================================================
 * /api/plugins/analytics/{usage,hints} (analytics_api.rs usage_summary /
 * hint_summary_from_events). Consumed by AgentsPage (UsagePayload/HintsPayload).
 * ========================================================================== */

/**
 * The categories `usage_summary_from_events` actually emits, at the
 * proportions a real store actually holds them in (captured 2026-07-25).
 *
 * This replaces a sixteen-row fixture that ramped smoothly from 1,840 to 86 —
 * a distribution no analytics store produces, and one that let a linear bar
 * chart look perfectly reasonable in every audit shot while the real payload
 * (6,774 against 1) rendered eleven invisible slivers. `record_event_usage`
 * only ever categorizes `tool`, `mcp_tool_call` and `skill` events, and on
 * this profile that resolves to exactly four buckets, one of which carries
 * nine tenths of them.
 */
const USAGE_ROWS: ReadonlyArray<readonly [string, string, number]> = [
  ['tool', 'tracedecay_mcp', 6774],
  ['tool', 'memory', 643],
  ['tool', 'lcm_session', 52],
  ['skill', 'workflow_skill', 1],
];

function analyticsUsagePayload(): Record<string, unknown> {
  return {
    available: true,
    source: 'analytics_events',
    // `ANALYTICS_EVENT_LIMIT`, not a total, and deliberately larger than the
    // categorized sum: the remaining events are hook routing, which carries no
    // tool or skill name to bucket.
    message_count: 10_000,
    event_count: 10_000,
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

/**
 * `AutomationSchedulerStatusV1` (automation_scheduler_api.rs).
 *
 * `pending_review` is required and is the authority AutomationsPage reads; the
 * flat `pending_*` fields are the pre-union mirrors the same handler still
 * emits. This fixture carried only the mirrors, so a perfectly healthy 200
 * failed the generated contract and the scheduler plate rendered
 * `unsupported_schema` — including in every visual-audit screenshot of it.
 * Both queues are `measured` here, which is the reading a mounted profile
 * produces; the `unreadable` arm is exercised by the page's own DOM tests.
 */
function schedulerStatusPayload(): Record<string, unknown> {
  return {
    status: 'configured',
    paused: false,
    enabled: true,
    scheduler_tick_secs: 900,
    pending_fact_proposals: 5,
    pending_skills: 2,
    pending_review: {
      fact_proposals: { state: 'measured', count: 5, reason: null },
      skills: { state: 'measured', count: 2, reason: null },
    },
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
 * DoctorInspector on the Observatory surface (DoctorFindingsPayloadV1Schema).
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

/** Owner operation references, verbatim from
 * `tracedecay_application::doctor::operations` (remediation.rs:30-53). The
 * dashboard route resolves a finding's reference through the kernel registry,
 * so a fixture naming an operation the registry does not seed would produce a
 * descriptor the real route can never emit. */
const DOCTOR_OPERATIONS = {
  retentionCollect: 'use-case.application.storage.retention-collect',
  branchGc: 'use-case.application.storage.branch-gc',
  protectedApply: 'use-case.application.configuration.protected-apply',
  codeIndexRemount: 'use-case.application.code-index.remount',
} as const;

/**
 * `GET /api/doctor/findings` with an admitted report reader — the populated
 * report, not the empty one.
 *
 * This fixture used to be deliberately empty, and said so: the inspector
 * painted each evidence badge's label in `doctorModel.ts` `tokenClass`, those
 * indicator hues miss WCAG AA as 11px text on `--surface-2`, and an empty
 * envelope was the only way to keep the audited surface at zero axe violations.
 * It kept the gate green by keeping the defective markup off the page. The
 * badge now follows the `StateChip` idiom — hue on the lamp and glyph, label on
 * an AA token — so the findings can be served and actually scanned.
 *
 * All eight `DoctorEvidenceStateV1` values appear exactly once, so every badge
 * variant is on screen for the axe scan rather than a representative few. Only
 * `healthy_complete_coverage` carries complete coverage, which is the invariant
 * `DoctorFindingV1::new` enforces: missing or partial truth never presents as
 * healthy.
 *
 * The descriptors mirror the kernel's seeded registry verbatim (summary,
 * surface, preview_available, action_confirmation), and `target` follows
 * `DoctorRemediationTargetV1::for_operation` — null for the protected
 * configuration apply, which needs a concrete key, value and base revision the
 * route cannot build from a finding alone.
 */
function doctorFindingsEnvelope(): Record<string, unknown> {
  const entry = (
    family: (typeof DOCTOR_FAMILIES)[number],
    state: string,
    completeness: 'complete' | 'partial' | 'unknown',
    statement: string,
    evidence: ReadonlyArray<string>,
    options: { storageKind?: string; operation?: string } = {},
  ): Record<string, unknown> => ({
    finding: {
      family,
      state,
      coverage: { completeness, statement },
      evidence: evidence.map((reference) => ({ family, reference })),
      remediation: options.operation
        ? { kind: 'action', owning_operation: options.operation }
        : null,
    },
    storage_kind: options.storageKind ?? null,
  });

  const entries = [
    entry(
      'storage',
      'partial',
      'partial',
      'two of three stores reported a size; the third could not be opened for measurement',
      ['store:lcm:size-observation:wm-41', 'store:memory:size-observation:wm-41'],
      { storageKind: 'over_budget_store', operation: DOCTOR_OPERATIONS.retentionCollect },
    ),
    entry(
      'storage',
      'stale',
      'complete',
      'branch databases were last reconciled against git refs 19 days ago',
      ['store:branch-db:reconcile-watermark:wm-22'],
      { storageKind: 'stale_branch_dbs', operation: DOCTOR_OPERATIONS.branchGc },
    ),
    entry(
      'configuration',
      'degraded',
      'complete',
      'effective configuration diverges from the desired revision on two protected keys',
      ['configuration:revision:r-317', 'configuration:revision:r-318'],
      { operation: DOCTOR_OPERATIONS.protectedApply },
    ),
    entry(
      'semantic_index',
      'unknown',
      'unknown',
      'the semantic index did not report a mount state, so its freshness is unknown',
      ['semantic-index:mount-probe:absent'],
      { operation: DOCTOR_OPERATIONS.codeIndexRemount },
    ),
    entry(
      'storage_runtime',
      'denied',
      'unknown',
      'this identity is not permitted to read the storage runtime health surface',
      ['storage-runtime:authority:denied'],
    ),
    entry(
      'language_server',
      'absent',
      'unknown',
      'no language server evidence source is present in this scope',
      ['language-server:probe:absent'],
    ),
    entry(
      'observability',
      'unsupported',
      'unknown',
      'the observability envelope source is not wired for this dashboard scope',
      ['observability:envelope:unwired'],
    ),
    entry(
      'advisory',
      'healthy_complete_coverage',
      'complete',
      'every advisory source was consulted and none reported a finding',
      ['advisory:feedback-finding:none', 'advisory:policy:none'],
    ),
  ];

  // Two families answered nothing. These render as coverage-gap chips above the
  // cards, which is the report saying which sources it never reached — a report
  // that dropped them would read as a clean bill of health for all seven.
  const consulted = ['advisory', 'configuration', 'storage_runtime', 'storage', 'semantic_index'];
  const payload = {
    family_filter: null,
    entries,
    report_coverage: {
      completeness: 'partial',
      statement: {
        completeness: 'partial',
        statement: 'five of seven finding families were consulted for this report',
      },
      families: DOCTOR_FAMILIES.map((family) => ({
        family,
        consultation: consulted.includes(family)
          ? { status: 'consulted' }
          : {
              status: 'unavailable',
              reason: family === 'language_server' ? 'absent' : 'unwired',
            },
      })),
    },
    remediations: [
      {
        operation: DOCTOR_OPERATIONS.retentionCollect,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'collect retention-eligible rows or reclaim an over-budget store',
        target: { owner_operation: 'storage_retention_collect' },
      },
      {
        operation: DOCTOR_OPERATIONS.branchGc,
        surface: 'storage_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'remove branch-scoped databases whose git refs are gone',
        target: { owner_operation: 'storage_branch_gc' },
      },
      {
        operation: DOCTOR_OPERATIONS.protectedApply,
        surface: 'configuration_control_plane',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'apply desired configuration to reconcile effective drift',
        // `for_operation` returns None here: the owning surface supplies the
        // key, value and base revision. The card says so instead of offering a
        // button that could not be dispatched.
        target: null,
      },
      {
        operation: DOCTOR_OPERATIONS.codeIndexRemount,
        surface: 'semantic_index_runtime',
        preview_available: true,
        action_confirmation: 'required',
        summary: 'remount or rebuild a code/semantic index that is unmounted or stale',
        target: { owner_operation: 'code_index_remount' },
      },
    ],
    known_families: [...DOCTOR_FAMILIES],
    note: 'five of seven finding families were consulted; two reported no evidence source',
  };
  // `partial`, because two families were never reached. The owner authorizes a
  // dry run and an apply on the three dispatchable operations; the protected
  // configuration apply is authorized too, and is simply not dispatchable from
  // here.
  return envelope(payload, 'partial', [
    { kind: 'refresh', operation: 'use-case.dashboard.doctor.findings.refresh' },
    ...Object.values(DOCTOR_OPERATIONS).flatMap((operation) => [
      { kind: 'request_dry_run', operation },
      { kind: 'request_apply', operation },
    ]),
  ]);
}

/* ==========================================================================
 * Observatory storage telemetry / findings (already wired; kept intact).
 * ========================================================================== */

/** The owner setting a soft store budget comes from, and the wording the daemon
 * emits for the unset/baseline/coverage cases — copied verbatim from
 * `src/dashboard/storage_telemetry_api.rs` so these fixtures stay wire-true. */
const BUDGET_SETTING_KEY = 'sync.retention.v1 store_soft_budgets_bytes';
const BUDGET_UNSET_REASON =
  'no soft size budget is configured by the owner for this store (set sync.retention.v1 store_soft_budgets_bytes for the store key to configure one)';
const GROWTH_COVERAGE =
  'since-daemon-start: bounded in-process watermark ring recorded on each telemetry sample, not a persisted historical series';
const GROWTH_BASELINE_REASON =
  'first watermark recorded in this daemon lifetime; a growth delta needs a second sample';
const TABLE_GROWTH_COVERAGE = {
  completeness: 'complete',
  eligible: 1,
  examined: 1,
  matched: null,
  excluded: null,
  omitted: 0,
  unknown: null,
  denominator: 1,
  unit: 'store_table_growth_reads',
  omission_reasons: [],
};
const TABLE_GROWTH_STATES = [
  {
    state: 'observed',
    // Two of this store's three current tables had a previous watermark, so the
    // observed read is partial and says so.
    coverage: {
      ...TABLE_GROWTH_COVERAGE,
      completeness: 'partial',
      eligible: 3,
      examined: 2,
      omitted: 1,
      denominator: 3,
      unit: 'current_tables',
      omission_reasons: ['embeddings: no previous table watermark exists; baseline pending'],
    },
    significant_samples: [
      {
        table: 'messages',
        previous_bytes: 10_485_760,
        current_bytes: 11_534_336,
        growth_bytes: 1_048_576,
        previous_observed_at: nowMicros - 3_600_000_000,
        current_observed_at: nowMicros,
      },
    ],
    omissions: [
      {
        kind: 'below_threshold',
        table: 'metadata',
        previous_bytes: 104_857_600,
        current_bytes: 105_381_888,
        growth_bytes: 524_288,
        previous_observed_at: nowMicros - 3_600_000_000,
        current_observed_at: nowMicros,
        reason: 'observed growth was below the informational significance threshold',
      },
      {
        kind: 'baseline_pending',
        table: 'embeddings',
        current_bytes: 4_194_304,
        observed_at: nowMicros,
        reason: 'embeddings: no previous table watermark exists; baseline pending',
      },
    ],
    omission_reasons: [
      'metadata: observed growth was below the informational significance threshold',
      'embeddings: no previous table watermark exists; baseline pending',
    ],
  },
  {
    state: 'denied',
    coverage: { ...TABLE_GROWTH_COVERAGE, completeness: 'partial', examined: 0, omitted: 1 },
    omission_reasons: ['per-table payload growth measurement was denied for this store'],
  },
  {
    state: 'baseline_established',
    coverage: { ...TABLE_GROWTH_COVERAGE, completeness: 'partial', examined: 0, omitted: 1 },
    observed_at: nowMicros,
    tables_observed: 7,
    omission_reasons: [
      'no baseline yet; this read established the first per-table payload watermark',
    ],
  },
  {
    state: 'unsupported',
    coverage: { ...TABLE_GROWTH_COVERAGE, completeness: 'partial', examined: 0, omitted: 1 },
    omission_reasons: ['per-table payload growth measurement is unsupported for this store'],
  },
  {
    state: 'unknown',
    coverage: { ...TABLE_GROWTH_COVERAGE, completeness: 'partial', examined: 0, omitted: 1 },
    omission_reasons: ['per-table payload growth measurement is unavailable for this store'],
  },
] as const;

/** GET /api/storage/telemetry — observatory (StorageTelemetryPayloadV1Schema).
 *
 * One entry per distinct store *file*: the graph and project-memory roles share
 * a database in project storage mode and are therefore one card carrying both
 * roles, not two cards with byte-identical sizes. The five entries below model
 * every state the endpoint can emit: an evaluated budget within and over its
 * soft limit, an unset budget, an unknown budget, and the baseline / observed /
 * unknown growth states. */
const storageTelemetry = envelope({
  stores: [
    {
      // Shared store file: graph + project memory, budget within its soft
      // limit, growth observed across the daemon-lifetime watermark ring.
      store: 'graph.db',
      role: 'graph',
      roles: ['graph', 'memory'],
      path: '/fast/projects/tracedecay/.tracedecay/graph.db',
      read: {
        kind: 'observed',
        sample: {
          store: 'graph.db',
          page_size_bytes: 4096,
          page_count: 52_400,
          freelist_pages: 1_280,
          observed_at: nowMicros,
        },
      },
      total_bytes: 214_630_400,
      free_bytes: 5_242_880,
      free_page_ratio: 0.024,
      budget: {
        state: 'evaluated',
        evaluation: {
          state: 'within_budget',
          observed: 214_630_400,
          soft_limit: 536_870_912,
        },
        setting_key: BUDGET_SETTING_KEY,
        reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
      },
      growth: {
        state: 'observed',
        coverage: GROWTH_COVERAGE,
        first_measured_at: nowMicros - 3_600_000_000,
        last_measured_at: nowMicros,
        sample_count: 12,
        first_total_bytes: 208_207_872,
        current_total_bytes: 214_630_400,
        growth_bytes: 6_422_528,
        samples: [
          { measured_at: nowMicros - 3_600_000_000, total_bytes: 208_207_872, free_bytes: 4_112_384 },
          { measured_at: nowMicros - 1_800_000_000, total_bytes: 211_419_136, free_bytes: 4_820_992 },
          { measured_at: nowMicros, total_bytes: 214_630_400, free_bytes: 5_242_880 },
        ],
      },
    },
    {
      // Over its owner-configured soft limit, with a real overage.
      store: 'lcm.db',
      role: 'lcm',
      roles: ['lcm'],
      path: '/home/zack/.tracedecay/lcm.db',
      read: {
        kind: 'observed',
        sample: {
          store: 'lcm.db',
          page_size_bytes: 4096,
          page_count: 180_224,
          freelist_pages: 2_048,
          observed_at: nowMicros,
        },
      },
      total_bytes: 738_197_504,
      free_bytes: 8_388_608,
      free_page_ratio: 0.011,
      budget: {
        state: 'evaluated',
        evaluation: {
          state: 'over_budget',
          observed: 738_197_504,
          soft_limit: 536_870_912,
          overage: 201_326_592,
        },
        setting_key: BUDGET_SETTING_KEY,
        reason: 'evaluated against the owner-configured soft limit of 536870912 bytes',
      },
      growth: {
        state: 'observed',
        coverage: GROWTH_COVERAGE,
        first_measured_at: nowMicros - 7_200_000_000,
        last_measured_at: nowMicros,
        sample_count: 24,
        first_total_bytes: 742_391_808,
        current_total_bytes: 738_197_504,
        // A shrinking store reports a negative delta rather than zero growth.
        growth_bytes: -4_194_304,
        samples: [
          { measured_at: nowMicros - 7_200_000_000, total_bytes: 742_391_808, free_bytes: 12_582_912 },
          { measured_at: nowMicros, total_bytes: 738_197_504, free_bytes: 8_388_608 },
        ],
      },
    },
    {
      // No owner entry: a missing *setting*, never a fabricated pass. First
      // watermark of this daemon lifetime, so growth is baseline, not zero.
      store: 'savings.db',
      role: 'savings',
      roles: ['savings'],
      path: '/home/zack/.tracedecay/savings.db',
      read: {
        kind: 'observed',
        sample: {
          store: 'savings.db',
          page_size_bytes: 4096,
          page_count: 18_200,
          freelist_pages: 420,
          observed_at: nowMicros,
        },
      },
      total_bytes: 74_547_200,
      free_bytes: 1_720_320,
      free_page_ratio: 0.023,
      budget: {
        state: 'unset',
        reason: BUDGET_UNSET_REASON,
        setting_key: BUDGET_SETTING_KEY,
      },
      growth: {
        state: 'baseline',
        coverage: GROWTH_COVERAGE,
        measured_at: nowMicros,
        total_bytes: 74_547_200,
        reason: GROWTH_BASELINE_REASON,
      },
    },
    {
      // The configured budget is unreadable, so the budget is unknown — the
      // dashboard never renders that as "within budget".
      store: 'sessions.db',
      role: 'sessions',
      roles: ['sessions'],
      path: '/home/zack/.tracedecay/sessions.db',
      read: {
        kind: 'observed',
        sample: {
          store: 'sessions.db',
          page_size_bytes: 4096,
          page_count: 9_600,
          freelist_pages: 96,
          observed_at: nowMicros,
        },
      },
      total_bytes: 39_321_600,
      free_bytes: 393_216,
      free_page_ratio: 0.01,
      budget: {
        state: 'unknown',
        reason:
          'the resolved runtime configuration could not be read, so a configured budget could not be determined',
      },
      growth: {
        state: 'baseline',
        coverage: GROWTH_COVERAGE,
        measured_at: nowMicros,
        total_bytes: 39_321_600,
        reason: GROWTH_BASELINE_REASON,
      },
    },
    {
      // The pragma read failed: sizes stay null and both dimensions are typed
      // unknown rather than collapsing to zero.
      store: 'incident.db',
      role: 'incident',
      roles: ['incident'],
      path: '/home/zack/.tracedecay/incident.db',
      read: { kind: 'unknown', store: 'incident.db' },
      total_bytes: null,
      free_bytes: null,
      free_page_ratio: null,
      budget: {
        state: 'unknown',
        reason: 'no observed size sample, so a configured budget could not be evaluated',
      },
      growth: {
        state: 'unknown',
        reason:
          'no watermark could be recorded because the store size read did not produce a sample',
      },
    },
  ].map((store, index) => ({ ...store, table_growth: TABLE_GROWTH_STATES[index] })),
  budget_note:
    'budgets are owner configuration: sync.retention.v1 store_soft_budgets_bytes, keyed by store key; a store with no entry reports unset (no budget configured), never a fabricated pass',
  growth_note:
    'growth is measured over the store-size watermarks this daemon has recorded since it started; no persisted historical watermark series exists, so the window is not historical',
  table_growth_threshold: {
    absolute_bytes: 67_108_864,
    relative_floor_bytes: 1_048_576,
    relative_percent: 10,
  },
  table_growth_coverage: {
    completeness: 'partial',
    eligible: 5,
    examined: 1,
    matched: null,
    excluded: null,
    omitted: 4,
    unknown: null,
    denominator: 5,
    unit: 'store_table_growth_reads',
    omission_reasons: [
      'lcm.db: denied',
      'savings.db: no baseline yet',
      'sessions.db: unsupported',
      'incident.db: unavailable',
    ],
  },
});

/** One of the five stores failed its pragma read, so the endpoint's coverage is
 * partial over the enumerated store set — wire-true to
 * `DashboardCoverageV1::partial` and the endpoint's own refresh legal action. */
const storageTelemetryEnvelope = {
  ...storageTelemetry,
  coverage: {
    completeness: 'partial',
    eligible: 5,
    examined: 4,
    matched: null,
    excluded: null,
    omitted: 1,
    unknown: null,
    denominator: 5,
    unit: 'stores',
    omission_reasons: ['store telemetry read failed (pragma unavailable)'],
  },
  legal_actions: [
    { kind: 'refresh', operation: 'use-case.dashboard.storage.telemetry.refresh' },
  ],
};

/** GET /api/storage/findings — observatory canonical Doctor projection plus
 * per-producer source coverage. This fixture follows the production parser
 * path; source state is not inferred from an empty finding list. */
const storageFindings = envelope({
  family_filter: 'storage',
  entries: [],
  report_coverage: null,
  remediations: [],
  known_families: [
    'advisory',
    'configuration',
    'storage_runtime',
    'storage',
    'language_server',
    'semantic_index',
    'observability',
  ],
  note: 'storage producers reported independent source coverage',
  kind_statuses: [
    {
      kind: 'over_budget_store',
      state: 'unset',
      observed_entries: 0,
      reason: 'No owner budget configured · sync.retention.v1 store_soft_budgets_bytes',
    },
    {
      kind: 'orphan_store',
      state: 'real',
      observed_entries: 1,
      reason: 'canonical Doctor producer returned one observed entry with complete coverage',
    },
    {
      kind: 'stale_branch_dbs',
      state: 'partial',
      observed_entries: 0,
      reason: 'branch-store inventory was consulted but per-producer coverage was incomplete',
    },
    {
      kind: 'incident_debris_present',
      state: 'unsupported',
      observed_entries: 0,
      reason: 'canonical Doctor storage source is unavailable (unsupported)',
    },
    {
      kind: 'retention_backlog',
      state: 'real',
      observed_entries: 0,
      reason: 'owner retention windows were evaluated with complete coverage',
    },
    {
      kind: 'table_growth',
      state: 'partial',
      observed_entries: 1,
      reason: 'canonical Doctor producer returned table growth evidence with partial coverage',
    },
  ],
});

/* ==========================================================================
 * /api/settings (settings_api.rs::get_settings) and /api/capabilities
 * (mod.rs::capabilities). Settings answers a DashboardEnvelopeV1 whose
 * payload is `SettingsPayloadV1`, and whose legal actions name the two write
 * scopes separately: `configuration_batch` appears only when the daemon-owned
 * configuration control plane is mounted, `user_settings_mutate` always.
 * ========================================================================== */

const settingsPayload: Record<string, unknown> = {
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
    user_settings_revision_id: 'user-rev-7',
    upload_enabled: false,
    watcher_debounce: '2s',
    extraction_timeout_secs: 30,
    installed_agents: ['claude', 'codex', 'cursor'],
  },
  automation: {
    config_endpoint: '/api/plugins/holographic/curation/config',
    availability: { available: true, reason: null, required_authority: null },
    source_coverage: {
      global: 'available',
      project: 'available',
      effective: 'available',
    },
    enabled: true,
    backend: 'codex_app_server',
    host_mode: 'standalone_backend',
  },
  environment: {
    global_accounting_mode: 'auto',
    global_accounting_enabled: true,
    // `pricing_offline` is derived daemon-side from TRACEDECAY_OFFLINE, so the
    // active variable below and this value are deliberately consistent: the
    // fixture exercises an environment override that is actually in force,
    // which is the one per-value provenance state /api/settings reports.
    pricing_offline: true,
    variables: [
      { name: 'TRACEDECAY_ENABLE_GLOBAL_DB', active: false, value: null, description: 'Force-enables or disables global savings-ledger recording.' },
      { name: 'TRACEDECAY_OFFLINE', active: true, value: '1', description: 'Skips network pricing fetches.' },
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

const settings: Record<string, unknown> = envelope(settingsPayload, 'ready', [
  { kind: 'request_apply', operation: 'configuration_batch' },
  { kind: 'request_apply', operation: 'user_settings_mutate' },
  { kind: 'refresh', operation: 'configuration_list' },
]);

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
  // `MultiRootCapabilityV1`, which `mod.rs::capabilities` has always sent and
  // this fixture omitted. Mounted, because the state worth screenshotting is
  // the one with figures in it; the `unavailable` arm is a state chip whose
  // only content is the daemon's sentence, and `MultiRootPanel.dom.test.tsx`
  // covers it directly.
  multi_root: {
    status: 'mounted',
    scope_set_id: 'scope-set.tracedecay.primary',
    revision: 7,
    scope_set_digest: 'sha256:0b91f7c4a2e85d31c6470fb2e9d18a5c',
    root_count: 3,
  },
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
    multi_root: true,
  },
  automation: {
    enabled: true,
    mode: 'standalone_backend',
    backend: 'codex_app_server',
    // `host_mode` is an `AutomationHostMode` — `standalone` or
    // `delegated_host`. `standalone_backend` belongs to the sibling `mode`
    // field and is not a value this key can hold.
    host_mode: 'standalone',
    // `AgentBackendAvailability` always carries `backend`; `executable` and
    // `reason` are skipped when unset rather than sent as null.
    availability: { backend: 'codex_app_server', available: true, executable: 'codex' },
  },
  // One canonical embedded dashboard. The five entries here were the legacy
  // plugin bundles that `44ef9e182` deleted.
  dashboards: ['tracedecay'],
};

/* ==========================================================================
 * /api/plugins/savings/sessions (savings_api.rs::sessions) and
 * /api/plugins/hermes-lcm/session/{id} (lcm_api.rs::session).
 *
 * The Loom weave's two sources, mirrored from a real daemon response captured
 * on 2026-07-25 (`tracedecay dashboard --port 7341`, profile-sharded store,
 * 6,053 sessions). Shapes are exact; the population is shaped to the same
 * DISTRIBUTION the real store has, per plan 11a real-profile finding 4 —
 * fixtures that differ only in size systematically under-test the surface:
 *
 *   - Message counts are heavily skewed (a handful in the hundreds, a long
 *     tail in the tens), because the weave's width channel is log-scaled and a
 *     uniform fixture would never show that it needed to be.
 *   - `last_message_at` is null on most rows. On the real profile only 14 of
 *     100 sessions carry an end later than their start, and drawing open
 *     threads correctly is the single most load-bearing honesty behaviour on
 *     the surface — a fixture where every session has an end would render a
 *     weave that cannot exist.
 *   - Some rows report zero messages, which the weave draws hollow.
 *   - Two rows are subagents.
 *
 * One deliberate deviation from the captured payload, recorded here rather
 * than hidden: the real store served a single provider ("cursor"), so its host
 * axis has one column. The fixture carries three providers so the audit
 * actually exercises column layout, dividers and the per-host readout row.
 * ========================================================================== */

const LOOM_PROVIDERS = ['claude', 'codex', 'cursor'] as const;
const LOOM_MODELS = [
  'gpt-5.6-sol-high',
  'composer-2.5-fast',
  'cursor-grok-4.5-high-fast',
  'gpt-5.6-terra-max',
] as const;
const LOOM_TITLES = [
  'Verify QUERY scheduler',
  'Deliver Git primitive runtime',
  'Normalize daemon service logging',
  'Bound and validate Hermes snapshots',
  'Preserve curated correction provenance',
  'Bind managed test runs to content',
] as const;

/** Deterministic session id, so screenshots and manifests are stable. */
function loomSessionId(index: number): string {
  return `0${(35 + index).toString(16).padStart(2, '0')}c8f3c-d4e6-4176-afea-${String(
    770_501 + index,
  ).padStart(12, '0')}`;
}

function loomModelRow(model: string | null, messages: number) {
  return {
    model,
    messages,
    estimated_messages: messages,
    usage_messages: 0,
    tokenized_messages: 0,
    cost_basis: 'estimated',
    actual: {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
    },
    estimated: { input_tokens: messages * 11, output_tokens: messages * 14 },
    tokenized: { input_tokens: 0, output_tokens: 0 },
    tokenizer: { encoder: 'o200k_base', exact: false },
  };
}

function loomSessionRows(count = 34): Record<string, unknown>[] {
  return Array.from({ length: count }, (_, i) => {
    const provider = LOOM_PROVIDERS[i % LOOM_PROVIDERS.length]!;
    const startedAt = nowSecs - i * 5 * 3600 - (i % 7) * 1300;
    // Skewed: three heavy sessions, then a long tail.
    const messages = i === 0 ? 998 : i === 3 ? 405 : i === 7 ? 169 : Math.max(2, 44 - i);
    // Only every fourth session carries an end later than its start.
    const hasEnd = i % 4 === 1;
    return {
      session_id: loomSessionId(i),
      provider,
      title: i % 5 === 4 ? null : LOOM_TITLES[i % LOOM_TITLES.length],
      started_at: startedAt,
      last_message_at: hasEnd ? startedAt + 900 + (i % 6) * 1500 : null,
      messages: i === 11 || i === 23 ? 0 : messages,
      is_subagent: i === 5 || i === 17,
      cost_basis: 'estimated',
      estimated_messages: messages,
      usage_messages: 0,
      tokenized_messages: 0,
      models: [
        loomModelRow(null, Math.max(messages - 5, 0)),
        loomModelRow(LOOM_MODELS[i % LOOM_MODELS.length]!, Math.min(messages, 5)),
      ],
    };
  });
}

function loomSessionsPayload(): Record<string, unknown> {
  return {
    available: true,
    db: '/home/zack/.tracedecay/projects/proj_a5b3d7e3ebe14ca7/sessions.db',
    scope: 'profile_sharded',
    range: 'all',
    since: 0,
    total: 6053,
    sessions: loomSessionRows(),
  };
}

const LOOM_CHAIN_TOOLS = [
  'Read',
  'Bash',
  'Grep',
  'Edit',
  'tracedecay_context',
  null,
] as const;

/** One session's transcript. `timestamp` is null on every message, exactly as
 * the daemon serves it — the chain rail reads that and prints "ordinal order",
 * so a fixture with timestamps would hide the behaviour under audit. */
function loomChainPayload(): Record<string, unknown> {
  const sessionId = loomSessionId(0);
  const messages = Array.from({ length: 46 }, (_, i) => {
    const tool = i === 0 ? null : LOOM_CHAIN_TOOLS[i % LOOM_CHAIN_TOOLS.length];
    return {
      message_id: `${sessionId}:${String(i).padStart(4, '0')}`,
      session_id: sessionId,
      role: i === 0 ? 'user' : i === 1 ? 'system' : 'assistant',
      content:
        i === 0
          ? 'Verify durable code-generation restart and worktree reconciliation. Fix gaps. No git mutations.'
          : tool
            ? `Invoking ${tool} against the workspace to confirm the reconciliation path.`
            : 'Summarising the reconciliation result and the remaining gap.',
      ordinal: i,
      timestamp: null,
      tool_name: tool,
      token_estimate: 18 + (i % 9) * 7,
      // `lcm_queries` selects a literal `0 AS pinned`: pinning is not tracked
      // yet, and the column is an integer, not a boolean.
      pinned: 0,
      source: 'cursor',
      storage_kind: 'message',
      store_id: 90_000 + i,
      summary_node_ids: [],
      metadata_json: '{"provider":"cursor","version":1}',
    };
  });
  return {
    exists: true,
    session_id: sessionId,
    path: '/home/zack/.tracedecay/projects/proj_a5b3d7e3ebe14ca7/sessions.db',
    storage_scope: 'profile_sharded',
    order: 'asc',
    limit: 200,
    offset: 0,
    has_more: false,
    has_more_messages: false,
    has_more_summary_nodes: false,
    counts: {
      message_count: 998,
      source_token_count: 18_400,
      summary_node_count: LOOM_CHAIN_SUMMARY_NODES.length,
      summary_token_count: 1_020,
      token_estimate_total: 21_460,
    },
    messages,
    summary_nodes: LOOM_CHAIN_SUMMARY_NODES.map((node) => ({ ...node, session_id: sessionId })),
  };
}

/**
 * LCM compaction boundaries for the transcript above.
 *
 * The Sessions drill-down reads these as the compactor's cuts: each names the
 * source tokens it replaced and the token count it replaced them with, so the
 * audit exercises a real boundary rather than the zero state. `expand_hint` is
 * the producer's own recovery instruction and is rendered verbatim.
 */
const LOOM_CHAIN_SUMMARY_NODES = [
  {
    node_id: 'sn-recon-0001',
    category: 'tool_activity',
    depth: 1,
    summary:
      'Read and grepped the worktree reconciliation path, then confirmed the durable restart branch against the scheduler registry.',
    source_type: 'messages',
    source_token_count: 9_800,
    token_count: 540,
    created_at: 1_752_988_400,
    latest_at: 1_752_989_050,
    expand_hint: 'lcm expand node:sn-recon-0001',
  },
  {
    node_id: 'sn-recon-0002',
    category: 'code_change',
    depth: 1,
    summary:
      'Edits to the generation-publication path and the reconciliation guard, with the tests that cover them.',
    source_type: 'messages',
    source_token_count: 6_200,
    token_count: 330,
    created_at: 1_752_989_600,
    latest_at: 1_752_990_100,
    expand_hint: 'lcm expand node:sn-recon-0002',
  },
  {
    node_id: 'sn-recon-0003',
    category: 'outcome',
    depth: 2,
    summary:
      'Session outcome: reconciliation verified, one gap left open against the hook-hint queue drain.',
    source_type: 'summary_nodes',
    source_token_count: 2_400,
    token_count: 150,
    created_at: 1_752_990_400,
    latest_at: null,
    expand_hint: 'lcm expand node:sn-recon-0003',
  },
] as const;

/** Thirty days back, matching `analytics_api::observatory_model`. Declared
 * above the fixture map because the map is built at module init and a `const`
 * is not hoisted the way the builder functions below it are. */
const OBSERVATORY_SINCE_MICROS = nowMicros - 30 * 86_400 * 1_000_000;

/** The Work projection generation both read routes report. Declared here for
 * the same hoisting reason as the constant above it. */
const WORK_GENERATION_ID = 'work-generation-0007';

const ANALYTICS_DESCRIPTOR = 'analytics-observability.v1';
const FEEDBACK_DESCRIPTOR = 'feedback-system-quality.v1';
const COST_DESCRIPTOR = 'accounting-cost.v1';

/**
 * Exact-path fixture map. Keys are the pathname (query string stripped by the
 * resolver). Anything not listed resolves to the prefix table, then to {}.
 */
export const FIXTURES: Readonly<Record<string, unknown>> = {
  '/api/projects': projects,
  '/api/storage/telemetry': storageTelemetryEnvelope,
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
  // Savings. `sessions` is the Loom weave's thread source, not a costs route.
  '/api/plugins/savings/overview': savingsPayload(),
  '/api/plugins/savings/sessions': loomSessionsPayload(),
  // Memory bank status (memory_api.rs::status) — the scoped Brain's fact and
  // entity readouts. Distinct from the overview payload above.
  '/api/plugins/holographic/status': memoryStatusPayload(),
  // Analytics.
  '/api/plugins/analytics/overview': analyticsOverviewPayload(),
  '/api/plugins/analytics/usage': analyticsUsagePayload(),
  '/api/plugins/analytics/hints': analyticsHintsPayload(),
  '/api/plugins/analytics/underused': analyticsUnderusedPayload(),
  '/api/plugins/analytics/diagnostics': analyticsDiagnosticsPayload(),
  // Automation.
  '/api/automation/scheduler/status': schedulerStatusPayload(),
  '/api/automation/jobs': jobsPayload(),
  '/api/automation/skills': skillsPayload(),
  '/api/automation/fact-proposals': factProposalsPayload(),
  // Plan 26 canonical read models. These are the projections the CLI and MCP
  // also serve, so their fixtures carry the mixed available/unavailable metric
  // set the real projector emits rather than a fully-populated one.
  '/api/observatory': observatoryEnvelope(),
  '/api/costs': costsEnvelope(),
  // Code-index freshness. Served against a mounted daemon scheduler, which is
  // the state the audit needs to shoot — the unattached case is a state chip
  // with no reading behind it.
  '/api/code-index/freshness': codeIndexFreshnessEnvelope(),
  // Work. The two mounted read routes. Unlike every other fixture here these
  // are wrapped in the application's `HttpJsonEnvelope` rather than
  // `DashboardEnvelopeV1`, because `mod.rs` nests the Work routes straight
  // onto the application router — see `workApi.ts`, which walks that wrapper.
  '/api/work/snapshot': workEnvelope(workSnapshotPayload()),
  '/api/work/delta': workEnvelope(workDeltaPayload()),
};

/** Prefix fixtures for query-bearing / dynamic routes. The resolver falls back
 * to these when there is no exact-path match. */
export const FIXTURE_PREFIXES: ReadonlyArray<readonly [string, unknown]> = [
  ['/api/plugins/graph/search', FIXTURES['/api/plugins/graph/search']],
  ['/api/plugins/hermes-lcm/search', FIXTURES['/api/plugins/hermes-lcm/search']],
  // Dynamic: `/session/{session_id}` — the Loom thread chain. One transcript
  // answers for every id, which is what a fixture can honestly be.
  ['/api/plugins/hermes-lcm/session/', loomChainPayload()],
  ['/api/plugins/holographic', memoryPayload()],
  ['/api/plugins/graph', graphOverviewPayload()],
  ['/api/plugins/savings', savingsPayload()],
];

/* ==========================================================================
 * Work (`/api/work/snapshot`, `/api/work/delta`).
 *
 * The Work routes are the one family on this dashboard that does not answer
 * with `DashboardEnvelopeV1`. `src/dashboard/mod.rs` nests them onto the
 * application router, so they carry the application's own `HttpJsonEnvelope`
 * — a `kind`/`value` union whose outcome packet holds the generated contract
 * under `payload`. `workApi.ts` walks exactly that structure and hands what it
 * finds to the generated schema, so a fixture that wrapped the payload any
 * other way would be refused as `unsupported_schema` and the audit would
 * screenshot a boundary plate for a surface that works.
 * ========================================================================== */

/** The application envelope, in the shape `workPayload()` walks. */
function workEnvelope(payload: unknown): Record<string, unknown> {
  return {
    kind: 'success',
    value: {
      // Reads answer as evidence; commands as effects. Both put the contract
      // in the same place, which is why `workApi.ts` checks the tag for
      // presence and does not branch on it.
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

/** One `WorkProjection`. The four booleans and the accepted proposal are what
 * `workModel.ts` reads a stage from, so the set below walks the lifecycle:
 * proposed, accepted, admitted, and admitted with evidence attached. */
function workProjection(spec: {
  taskId: string;
  title: string;
  version: number;
  acceptedProposal?: string | null;
  taskAccepted?: boolean;
  executionAdmitted?: boolean;
  dependencies?: readonly string[];
  runtimeEvidence?: readonly Record<string, unknown>[];
  historyLen?: number;
}): Record<string, unknown> {
  return {
    accepted_proposal: spec.acceptedProposal ?? null,
    authority: {
      actor_id: 'actor.agent.opus',
      policy_digest: 'sha256:9f2c41d6',
      project_id: 'project.tracedecay',
      repository_id: 'repository.tracedecay',
      worktree_id: 'worktree.primary',
    },
    dependencies: spec.dependencies ?? [],
    execution_admitted: spec.executionAdmitted ?? false,
    history_len: spec.historyLen ?? 1,
    runtime_evidence: spec.runtimeEvidence ?? [],
    task_accepted: spec.taskAccepted ?? false,
    task_id: spec.taskId,
    title: spec.title,
    version: spec.version,
  };
}

/** A function rather than a `const` for the reason
 * `OBSERVATORY_SINCE_MICROS` is declared where it is: the fixture map is built
 * at module init, and a `const` below it is not hoisted. */
function workProjections(): readonly Record<string, unknown>[] {
  return [
  workProjection({
    taskId: 'task.contract-drift-gate',
    title: 'Gate fixture payloads against their generated contract',
    version: 4,
    acceptedProposal: 'proposal.contract-drift-gate.2',
    taskAccepted: true,
    executionAdmitted: true,
    historyLen: 9,
    // `RuntimeEvidenceRef`: the run it came from, the digest that seals it,
    // and whether it is the attempt's last word.
    runtimeEvidence: [
      {
        evidence_digest: 'sha256:4c1b8a0e77d2f5936ab41e0c9d5f2731',
        run_id: 'run.contract-drift-gate.1',
        terminal: true,
      },
    ],
  }),
  workProjection({
    taskId: 'task.storage-findings-authority',
    title: 'Give Doctor findings one hook and one poll',
    version: 3,
    acceptedProposal: 'proposal.storage-findings-authority.1',
    taskAccepted: true,
    historyLen: 6,
  }),
  workProjection({
    taskId: 'task.scope-cache-token',
    title: 'Key scoped reads by the request the scope rewrites',
    version: 2,
    acceptedProposal: 'proposal.scope-cache-token.1',
    dependencies: ['task.storage-findings-authority'],
    historyLen: 4,
  }),
  workProjection({
    taskId: 'task.attempt-inventory',
    title: 'Name every withheld runtime-attempt operation',
    version: 1,
    historyLen: 1,
  }),
  ];
}

/**
 * A capped snapshot rather than a complete one.
 *
 * The board's coverage line and its continuation request are only exercised
 * when the daemon reports that it withheld rows, and a fixture that always
 * said `complete` would leave both undrawn in every screenshot.
 */
function workSnapshotPayload(): Record<string, unknown> {
  return {
    coverage: {
      state: 'capped',
      cap: 4,
      cursor: { generation_id: WORK_GENERATION_ID, token: 'cursor.work.4' },
      range: { start_exclusive: 0, end_inclusive: 4 },
      returned: workProjections().length,
      total: 6,
    },
    generation_id: WORK_GENERATION_ID,
    projections: workProjections(),
    sequence: 4,
  };
}

/** The continuation the capped snapshot above resumes into: the remaining two
 * tasks, and a coverage line that completes the board. */
function workDeltaPayload(): Record<string, unknown> {
  return {
    changed: [
      workProjection({
        taskId: 'task.command-palette-identity',
        title: 'Give palette options ids no path can break',
        version: 2,
        acceptedProposal: 'proposal.command-palette-identity.1',
        taskAccepted: true,
        historyLen: 5,
      }),
      workProjection({
        taskId: 'task.zero-graph-counts',
        title: 'Print the graph zeros Code measured',
        version: 1,
        historyLen: 2,
      }),
    ],
    coverage: { state: 'complete', returned: 2, total: 6 },
    from_sequence: 4,
    generation_id: WORK_GENERATION_ID,
    removed: [],
    to_sequence: 6,
  };
}

/* ==========================================================================
 * Plan 26 canonical read models (`/api/observatory`, `/api/costs`).
 *
 * Shapes come from `src/application/observability.rs`. Two of its behaviours
 * are deliberately reproduced here rather than smoothed away, because they are
 * the behaviours the workspaces exist to render honestly:
 *
 *   - a metric the projector could not complete carries `value: null` and an
 *     `unavailable_reason`, and its coverage goes `unknown` with a null
 *     eligible population;
 *   - every completed metric carries a degenerate uncertainty interval
 *     (`lower == upper == value`), which the plate must drop rather than draw.
 * ========================================================================== */

/** One `MetricValueV1`. `value: null` also nulls the coverage denominator, as
 * the projector's `coverage(None, 0, 1, Unknown)` does. */
function metricValue(spec: {
  metric: string;
  value: number | null;
  unit: string;
  denominator: string;
  eligible: number | null;
  source: string;
  sourceRevision: string;
  projectorRevision: string;
  watermark: string;
  descriptorRevision: string;
  unavailableReason?: string | null;
}): Record<string, unknown> {
  const available = spec.value != null;
  return {
    descriptor_revision: spec.descriptorRevision,
    metric: spec.metric,
    value: spec.value,
    unit: spec.unit,
    denominator: spec.denominator,
    denominator_value: available ? spec.eligible : null,
    coverage: {
      state: available ? 'known' : 'unknown',
      eligible: available ? spec.eligible : null,
      observed: available ? (spec.eligible ?? 0) : 0,
      completed: available ? (spec.eligible ?? 0) : 0,
      censored: 0,
      excluded: 0,
      unknown: available ? 0 : 1,
    },
    evidence_class: 'measurement',
    provenance: {
      source: spec.source,
      source_revision: spec.sourceRevision,
      projector_revision: spec.projectorRevision,
      watermark: spec.watermark,
    },
    cohort: {
      descriptor_revision: `${spec.denominator}.v1`,
      eligible_population: spec.denominator,
    },
    temporal: {
      horizon: { since_micros: OBSERVATORY_SINCE_MICROS, until_micros: nowMicros },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: available
      ? { lower: spec.value, upper: spec.value, reason: null }
      : { lower: null, upper: null, reason: spec.unavailableReason ?? null },
    calibration: null,
    unavailable_reason: spec.unavailableReason ?? null,
  };
}

function observabilityMetric(
  metric: string,
  value: number | null,
  reason: string | null = null,
): Record<string, unknown> {
  return metricValue({
    metric,
    value,
    unit: 'events',
    denominator: 'eligible_observability_events',
    eligible: 6_142,
    source: 'observability_envelope',
    sourceRevision: 'observability-envelope.v1',
    projectorRevision: 'observatory-projector.v1',
    watermark: 'analytics:918422',
    descriptorRevision: ANALYTICS_DESCRIPTOR,
    unavailableReason: reason,
  });
}

function feedbackMetric(spec: {
  metric: string;
  value: number | null;
  unit: string;
  denominator: string;
  eligible: number | null;
  reason?: string | null;
}): Record<string, unknown> {
  return metricValue({
    ...spec,
    source: 'feedback_observations',
    sourceRevision: 'feedback-observations.v1',
    projectorRevision: 'feedback-system-quality-projector.v1',
    watermark: 'feedback:31204',
    descriptorRevision: FEEDBACK_DESCRIPTOR,
    unavailableReason: spec.reason ?? null,
  });
}

/** GET /api/observatory (src/dashboard/analytics_api.rs `observatory`). */
function observatoryEnvelope(): Record<string, unknown> {
  const metrics = [
    observabilityMetric('observability_events', 6_142),
    observabilityMetric('observability_failures', 23),
    observabilityMetric('telemetry_drops_lower_bound', 0),
    feedbackMetric({
      metric: 'feedback_coverage',
      value: 0.9127,
      unit: 'ratio',
      denominator: 'eligible_observations',
      eligible: 1_884,
    }),
    feedbackMetric({
      metric: 'feedback_relevance',
      value: 0.7431,
      unit: 'ratio',
      denominator: 'relevance_labels',
      eligible: 612,
    }),
    feedbackMetric({
      metric: 'feedback_diversity',
      value: 0.6208,
      unit: 'ratio',
      denominator: 'eligible_source_families',
      eligible: 8,
    }),
    feedbackMetric({
      metric: 'feedback_latency_p95',
      value: 214_800,
      unit: 'microseconds',
      denominator: 'latency_samples',
      eligible: 1_884,
    }),
    feedbackMetric({
      metric: 'feedback_omission_rate',
      value: 0.0412,
      unit: 'ratio',
      denominator: 'returned_and_omitted_items',
      eligible: 9_260,
    }),
    feedbackMetric({
      metric: 'feedback_denial_rate',
      value: 0.0,
      unit: 'ratio',
      denominator: 'outcome_observations',
      eligible: 1_884,
    }),
    feedbackMetric({
      metric: 'feedback_staleness_rate',
      value: 0.0217,
      unit: 'ratio',
      denominator: 'outcome_observations',
      eligible: 1_884,
    }),
    // Two the projector genuinely could not complete on a store with no
    // revocation or stack-transition observations. The audit needs these: a
    // fully-populated fixture never shoots the unavailable plate.
    feedbackMetric({
      metric: 'feedback_revocation_propagation_p95',
      value: null,
      unit: 'microseconds',
      denominator: 'revocation_observations',
      eligible: null,
      reason: 'no_revocation_observations',
    }),
    feedbackMetric({
      metric: 'feedback_stack_transitions',
      value: null,
      unit: 'transitions',
      denominator: 'stack_transition_observations',
      eligible: null,
      reason: 'no_stack_transition_observations',
    }),
  ];
  const payload = {
    authorized_scope_ref: 'proj_a5b3d7e3ebe14ca7',
    horizon: { since_micros: OBSERVATORY_SINCE_MICROS, until_micros: nowMicros },
    watermark: 'analytics:918422;feedback:31204',
    observed_at_micros: nowMicros,
    current: false,
    metrics,
  };
  return {
    ...envelope(payload, 'partial', [
      { kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' },
    ]),
    coverage: {
      completeness: 'partial',
      eligible: metrics.length,
      examined: metrics.length - 2,
      matched: null,
      excluded: null,
      omitted: 2,
      unknown: null,
      denominator: metrics.length,
      unit: 'metrics',
      omission_reasons: ['incomplete_metric_coverage'],
    },
  };
}

/** GET /api/costs (src/dashboard/savings_api.rs `costs`). The projector asks
 * for an all-time window, which reaches the wire as `since_micros: 0`. */
function costsEnvelope(): Record<string, unknown> {
  const usage = [
    metricValue({
      metric: 'provider_tokens',
      value: 41_882_140,
      unit: 'tokens',
      denominator: 'ingested_provider_turns',
      eligible: 27_401,
      source: 'accounting_turn',
      sourceRevision: 'accounting-turn.v1',
      projectorRevision: 'costs-projector.v1',
      watermark: 'turns:27401:1752990400',
      descriptorRevision: COST_DESCRIPTOR,
    }),
    metricValue({
      metric: 'saved_tokens',
      value: 30_144_802,
      unit: 'tokens',
      denominator: 'eligible_savings_calls',
      eligible: 12_608,
      source: 'savings_ledger',
      sourceRevision: 'savings-ledger.v1',
      projectorRevision: 'costs-projector.v1',
      watermark: 'savings:1752990400',
      descriptorRevision: COST_DESCRIPTOR,
    }),
  ];
  // Prices are recorded at ingest. Turns counted without a pricing revision
  // produce a null cost with this exact reason — never a zero bill.
  const estimatedCost = [
    metricValue({
      metric: 'provider_cost',
      value: null,
      unit: 'usd',
      denominator: 'priced_provider_turns',
      eligible: null,
      source: 'accounting_turn',
      sourceRevision: 'accounting-turn.v1',
      projectorRevision: 'costs-projector.v1',
      watermark: 'turns:27401:1752990400',
      descriptorRevision: COST_DESCRIPTOR,
      unavailableReason: 'pricing_revision_unavailable',
    }),
  ];
  const payload = {
    authorized_scope_ref: 'all',
    horizon: { since_micros: 0, until_micros: nowMicros },
    watermark: 'turns:27401:1752990400;savings:1752990400',
    observed_at_micros: nowMicros,
    current: false,
    usage,
    estimated_cost: estimatedCost,
    pricing_revision: null,
  };
  return {
    ...envelope(payload, 'partial', [
      { kind: 'refresh', operation: 'use-case.dashboard.costs.refresh' },
    ]),
    coverage: {
      completeness: 'partial',
      eligible: 3,
      examined: 2,
      matched: null,
      excluded: null,
      omitted: 1,
      unknown: null,
      denominator: 3,
      unit: 'metrics',
      omission_reasons: ['incomplete_metric_coverage'],
    },
  };
}

/** GET /api/code-index/freshness (src/dashboard/code_index_freshness_api.rs). */
function codeIndexFreshnessEnvelope(): Record<string, unknown> {
  const payload = {
    worktrees: [
      {
        worktree_root: '/fast/projects/tracedecay',
        repository_id: 'repository.b41f2c9d',
        worktree_id: 'worktree.primary',
        source_reference: 'refs/heads/codex/tracedecay-total-redesign-plan',
        latest_generation_id: 'generation.2f8c41ab',
        snapshot_content_identity: 'sha256:9c1f4a2e7b05',
        sealed_at_micros: nowMicros - 214_000_000,
        last_reconcile_micros: nowMicros - 8_400_000,
        staleness_state: 'fresh',
        hook_hint_count: 0,
        coverage: 'complete',
      },
    ],
    note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
  };
  return {
    ...envelope(payload, 'ready', [
      { kind: 'refresh', operation: 'use-case.dashboard.code-index.freshness.refresh' },
    ]),
    coverage: {
      completeness: 'complete',
      eligible: 1,
      examined: 1,
      matched: 1,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 1,
      unit: 'mounted_worktree',
      omission_reasons: [],
    },
  };
}

/** GET /api/plugins/holographic/status (src/dashboard/memory_api.rs `status`). */
function memoryStatusPayload(): Record<string, unknown> {
  return {
    error: '',
    exists: true,
    path: '/fast/projects/tracedecay/.tracedecay/memory.db',
    largest_bank_fact_count: 169,
    largest_bank_utilization_pct: 0.0477,
    // A store past its legacy backfill with no outstanding feedback repair —
    // the steady state, and the one the Brain readout is designed against.
    feedback_history_repair: { state: 'not_required', processed: 0, remaining: null },
    memory: {
      algebra_name: 'amari_fhrr',
      bank_count: 7,
      // 2048-dimension FHRR (the `hrr_dim` migration default), which is what
      // sets `estimated_capacity` below.
      hrr_dim: 2048,
      entity_count: 1186,
      estimated_capacity: 354_304,
      fact_count: 173,
      below_default_recall_threshold_count: 4,
      missing_vector_count: 0,
      repair: { banks_rebuilt: 0, missing_vectors_repaired: 0 },
      // The four coarse trust bands. KnowledgePage reads these as its FALLBACK
      // trust distribution, because on a real store the overview's ten-bucket
      // `trust_histogram` comes back all-zero — `dashboard_compatibility_named_
      // counts_tx` emits row names of the form `trust-<n>` and
      // `facts.rs::trust_histogram` reads them with `parse::<usize>()`, which
      // fails and skips every row.
      //
      // The overview fixture deliberately keeps a POPULATED histogram, so the
      // audit renders the preferred source and the endpoint gate can assert the
      // shape the route is specified to serve. The fallback path is covered by
      // trust.test.ts against the exact all-zero payload the daemon sends.
      trust_0_025_count: 0,
      trust_025_050_count: 6,
      trust_050_075_count: 21,
      trust_075_100_count: 146,
      helpful_count: 153,
      unhelpful_count: 2,
      feedback_funnel: {
        access_count_total: 2275,
        feedback_total: 155,
        rated_fact_count: 56,
        retrieval_count_total: 3207,
        retrieved_fact_count: 170,
        seen_to_feedback_ratio: 35,
      },
    },
  };
}

/** GET /api/plugins/analytics/overview (src/dashboard/analytics_api.rs). */
function analyticsOverviewPayload(): Record<string, unknown> {
  return {
    available: true,
    db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    scope: 'profile_sharded',
    usage: {
      available: true,
      source: 'analytics_events',
      event_count: 10_000,
      message_count: 10_000,
      by_category: [
        { category: 'tracedecay_mcp', events: 6837, kind: 'tool' },
        { category: 'memory', events: 610, kind: 'tool' },
        { category: 'lcm_session', events: 22, kind: 'tool' },
      ],
    },
    observatory: observatoryReadModel(),
  };
}

/** The Plan 26 Observatory projection `analytics_api::overview` embeds
 * (`application::observability::observatory_read_model`). It is never absent on
 * this route — a missing store yields the `analytics:unavailable` model with
 * `current: false`, not a null — so the fixture carries the complete-coverage
 * variant: every envelope in the horizon parsed, so each metric reports an
 * exact value and `unavailable_reason` stays null. */
function observatoryReadModel(): Record<string, unknown> {
  const horizon = {
    since_micros: nowMicros - 30 * DAY * 1_000_000,
    until_micros: nowMicros,
  };
  const observed = 4182;
  const watermark = 'analytics:evt_9f31c4';
  // `complete` coverage: nothing invalid, nothing dropped, so `eligible` is the
  // exact observed count and the interval collapses onto the value.
  const coverage = {
    eligible: observed,
    observed,
    completed: observed,
    censored: 0,
    unknown: 0,
    excluded: 0,
    state: 'known',
  };
  const metric = (name: string, value: number) => ({
    descriptor_revision: 'analytics-events.v1',
    metric: name,
    value,
    unit: 'events',
    denominator: 'eligible_observability_events',
    denominator_value: coverage.eligible,
    coverage,
    evidence_class: 'measurement',
    provenance: {
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      projector_revision: 'observatory-projector.v1',
      watermark,
    },
    cohort: {
      descriptor_revision: 'eligible_observability_events.v1',
      eligible_population: 'eligible_observability_events',
    },
    temporal: { horizon, baseline_watermark: null, delta: null },
    uncertainty: { lower: value, upper: value, reason: null },
    calibration: null,
    unavailable_reason: null,
  });
  return {
    authorized_scope_ref: 'proj_a5b3d7e3ebe14ca7',
    horizon,
    watermark,
    observed_at_micros: nowMicros,
    current: true,
    metrics: [
      metric('observability_events', observed),
      metric('observability_failures', 37),
      metric('telemetry_drops_lower_bound', 0),
    ],
  };
}

/** Empty-but-valid fallback for any unmapped /api route. */
export const EMPTY_FIXTURE: Record<string, unknown> = {};

/**
 * GET /api/projects/{project_id} — the registry backbone (src/dashboard/
 * projects.rs `context`). Resolves for every registered project regardless of
 * whether its graph is mounted, which is exactly the property the scoped Brain
 * depends on, so the fixture answers for any id rather than only known ones.
 */
function projectContextPayload(projectId: string): Record<string, unknown> {
  // `context` answers with a `PublicCodeProject`, the same narrow record the
  // list route's flat `projects` carries — not the registry entry the tree
  // holds — so the two routes are read from one source here.
  const known = flatProjects.find((entry) => entry['project_id'] === projectId);
  const root = `/fast/projects/${projectId}`;
  const entry = known ?? {
    project_id: projectId,
    label: projectId,
    project_root: root,
    canonical_root: root,
    display_root: root,
    git_common_dir: `${root}/.git`,
    default_branch: 'master',
    created_at: nowSecs - 33 * DAY,
    last_seen_at: nowSecs - 3 * DAY,
    is_active: false,
  };
  const canonicalRoot = entry['canonical_root'] as string;
  const lastSeen = entry['last_seen_at'] as number;
  const branches = ['master', 'codex/tracedecay-total-redesign-plan', 'release/2.4'];
  return {
    status: 'ok',
    is_active: entry['is_active'] === true,
    project: entry,
    aliases: [
      { project_id: projectId, alias_path: canonicalRoot, last_seen_at: lastSeen },
      ...branches.slice(1).map((branch, i) => ({
        project_id: projectId,
        alias_path: `${canonicalRoot}/.worktrees/${branch.replace(/\//g, '-')}`,
        last_seen_at: lastSeen - (i + 1) * 4 * 3600,
      })),
    ],
    stores: [
      {
        store: {
          store_id: `store:${projectId}:profile_sharded`,
          project_id: projectId,
          store_kind: 'code_project',
          storage_mode: 'profile_sharded',
          store_relpath: `projects/${projectId}`,
          manifest_relpath: `projects/${projectId}/store_manifest.json`,
          created_at: lastSeen - 90 * DAY,
          last_verified_at: lastSeen,
          last_write_at: lastSeen,
        },
        graph_scopes: branches.map((branch, i) => ({
          graph_scope_id: `store:${projectId}:branch:${branch}`,
          project_id: projectId,
          store_id: `store:${projectId}:profile_sharded`,
          branch_name: branch,
          db_relpath: `projects/${projectId}/branches/${branch.replace(/\//g, '-')}.db`,
          parent_scope_id: null,
          last_synced_at: lastSeen - i * 6 * 3600,
          writable: i === 0,
        })),
        artifacts: [
          {
            store_id: `store:${projectId}:profile_sharded`,
            artifact_kind: 'graph_db',
            relpath: `projects/${projectId}/tracedecay.db`,
            schema_version: null,
            size_bytes: 131_088_384,
            updated_at: lastSeen,
          },
          {
            store_id: `store:${projectId}:profile_sharded`,
            artifact_kind: 'sessions_db',
            relpath: `projects/${projectId}/sessions.db`,
            schema_version: null,
            size_bytes: 42_930_176,
            updated_at: lastSeen,
          },
          {
            store_id: `store:${projectId}:profile_sharded`,
            artifact_kind: 'memory_db',
            relpath: `projects/${projectId}/memory.db`,
            schema_version: null,
            size_bytes: 9_027_584,
            updated_at: lastSeen,
          },
          {
            store_id: `store:${projectId}:profile_sharded`,
            artifact_kind: 'branch_meta',
            relpath: `projects/${projectId}/branch-meta.json`,
            schema_version: null,
            size_bytes: 14_704,
            updated_at: lastSeen,
          },
        ],
      },
    ],
  };
}

/**
 * Resolve a request pathname to its fixture payload. `search` is the raw query
 * string (e.g. `?node_id=sym-0`); it is used only for routes whose response
 * body legitimately varies by query (the graph subgraph neighborhood), and is
 * otherwise ignored so all other routes resolve by pathname alone.
 */
export function resolveFixture(pathname: string, search = ''): unknown {
  // The project-scoped gateway. The daemon binds `/api/projects/{id}/{tail}`
  // and serves `/api/{tail}` against that project's own state
  // (src/dashboard/mod.rs `project_scoped_api_gateway`), so the fixture layer
  // has to perform the same rewrite — otherwise every scoped read a workspace
  // makes would resolve to the registry payload and the scoped surfaces would
  // be audited against a shape the daemon never sends. `/api/projects/{id}`
  // with no tail is a different route (`projects::context`) and is handled
  // below, not here.
  const scoped = /^\/api\/projects\/([^/]+)\/(.+)$/.exec(pathname);
  if (scoped) return resolveFixture(`/api/${scoped[2]}`, search);
  const contextMatch = /^\/api\/projects\/([^/]+)$/.exec(pathname);
  if (contextMatch) return projectContextPayload(contextMatch[1]!);

  if (pathname === '/api/plugins/graph/subgraph') {
    const nodeId = new URLSearchParams(search).get('node_id');
    return subgraphPayload(nodeId);
  }
  // Must precede the FIXTURE_PREFIXES sweep: `/api/plugins/graph` is a prefix
  // fixture, so without this branch every neighbors read would resolve to the
  // overview payload and the TRACE drill-in would be audited against a shape
  // the daemon never sends on this route.
  const neighbors = /^\/api\/plugins\/graph\/node\/([^/]+)\/neighbors$/.exec(pathname);
  if (neighbors) {
    // `coerce_limit(params.limit, 50, 200)` in graph_api.rs: default 50, hard
    // cap 200, and a non-positive or unparsable value falls back to default.
    const raw = Number(new URLSearchParams(search).get('limit'));
    const limit = Number.isFinite(raw) && raw > 0 ? Math.min(200, Math.trunc(raw)) : 50;
    return neighborsPayload(decodeURIComponent(neighbors[1]!), limit);
  }
  if (pathname in FIXTURES) return FIXTURES[pathname];
  for (const [prefix, payload] of FIXTURE_PREFIXES) {
    if (pathname.startsWith(prefix)) return payload;
  }
  return EMPTY_FIXTURE;
}

/* ==========================================================================
 * /api/plugins/analytics/{underused,diagnostics} (analytics_api.rs
 * `underused` / `diagnostics_summary`). Consumed by AgentsPage.
 *
 * Both were previously unmapped and resolved to `{}`, so the audit never once
 * rendered the plates that depend on them. Shapes and — more importantly —
 * DISTRIBUTIONS are taken from a real daemon response captured on 2026-07-25
 * (profile-sharded store, 10,000-event window):
 *
 *   - `event_count` is exactly `ANALYTICS_EVENT_LIMIT`, because on any store
 *     with real traffic it always is. The page has to render the capped case.
 *   - `by_mcp_tool` spans 1,945 calls down to 1 across a long tail. A fixture
 *     with an even spread would never show that a linear rail cannot draw it.
 *   - `by_event_kind` sums to the window while the usage payload's categories
 *     sum to less, because hook-routing events carry nothing to categorize.
 *     The plate that reconciles those two totals only exists because of it.
 *   - `recent_events` is newest-first with real second-resolution stamps.
 *
 * The families payload deliberately carries three of the four verdict states
 * (under-used, covered, and the two families that have no substitute detector
 * at all and therefore cannot ever be flagged) so one audit shot exercises the
 * whole row vocabulary.
 * ========================================================================== */

function analyticsUnderusedPayload(): Record<string, unknown> {
  return {
    available: true,
    db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    families: [
      // Has a detector, and it fired more often than the family was used.
      {
        family: 'code_context',
        usage_events: 138,
        relevant_events: 191,
        missed_events: 53,
        underused: true,
      },
      // Has a detector; the family outran it.
      {
        family: 'code_search',
        usage_events: 226,
        relevant_events: 84,
        missed_events: -142,
        underused: false,
      },
      // No detector exists for these two, so `relevant_events` is structurally
      // zero and `underused` can never become true however they are used.
      {
        family: 'call_graph',
        usage_events: 66,
        relevant_events: 0,
        missed_events: -66,
        underused: false,
      },
      {
        family: 'impact_analysis',
        usage_events: 0,
        relevant_events: 0,
        missed_events: 0,
        underused: false,
      },
    ],
  };
}

function analyticsDiagnosticsPayload(): Record<string, unknown> {
  const AGENT_TOOL_CALLS: ReadonlyArray<readonly [string, number]> = [
    ['tracedecay_grep', 1945],
    ['tracedecay_read', 1180],
    ['tracedecay_body', 1152],
    ['tracedecay_fact_store', 644],
    ['tracedecay_hook_runtime', 587],
    ['tracedecay_outline', 460],
    ['tracedecay_search', 306],
    ['tracedecay_context', 200],
    ['tracedecay_status', 183],
    ['tracedecay_diagnostics', 81],
    ['tracedecay_files', 80],
    ['tracedecay_retrieve', 74],
    ['tracedecay_callers', 60],
    ['tracedecay_impact', 41],
    ['tracedecay_active_project', 27],
    ['tracedecay_affected', 18],
    ['tracedecay_health', 9],
    ['tracedecay_call_chain', 6],
    ['tracedecay_circular', 3],
    ['tracedecay_recursion', 1],
  ];
  const AGENT_TAPE: ReadonlyArray<readonly [number, string, string]> = [
    [0, 'tracedecay_fact_store', 'success'],
    [0, 'tracedecay_fact_store', 'success'],
    [4, 'tracedecay_hook_runtime', 'success'],
    [11, 'tracedecay_grep', 'success'],
    [17, 'tracedecay_body', 'success'],
    [23, 'tracedecay_grep', 'error'],
    [29, 'tracedecay_read', 'success'],
    [38, 'tracedecay_outline', 'success'],
    [44, 'tracedecay_context', 'success'],
    [51, 'tracedecay_grep', 'success'],
    [63, 'tracedecay_body', 'success'],
    [70, 'tracedecay_search', 'success'],
    [82, 'tracedecay_read', 'success'],
    [96, 'tracedecay_hook_runtime', 'success'],
    [109, 'tracedecay_grep', 'success'],
    [124, 'tracedecay_body', 'success'],
    [141, 'tracedecay_status', 'success'],
    [163, 'tracedecay_read', 'error'],
    [188, 'tracedecay_hook_runtime', 'success'],
    [222, 'tracedecay_status', 'success'],
  ];
  /** Anchored to a fixed offset so the tape's stamps are stable across a
   * screenshot pair. */
  const AGENT_TAPE_ANCHOR = nowSecs - 240;
  const toolCalls = AGENT_TOOL_CALLS.reduce((sum, [, count]) => sum + count, 0);
  return {
    available: true,
    source: 'analytics_events',
    event_count: 10_000,
    message_count: 10_000,
    events_per_hour: 135.36531714965764,
    hook_call_count: 444_038,
    mcp_tool_call_count: toolCalls,
    tool_call_count: toolCalls,
    tracedecay_call_count: toolCalls,
    by_event_kind: [
      { event_kind: 'mcp_tool_call', count: toolCalls },
      { event_kind: 'hook_route', count: 10_000 - toolCalls },
    ],
    // Outcomes partition the window exactly: every tool call succeeded or
    // errored, and every hook-routing event is 'observed'. Derived from the
    // tool total rather than hard-coded so the three always sum to 10,000.
    by_outcome: [
      { outcome: 'success', count: toolCalls - 268 },
      { outcome: 'observed', count: 10_000 - toolCalls },
      { outcome: 'error', count: 268 },
    ],
    by_mcp_tool: AGENT_TOOL_CALLS.map(([tool_name, count]) => ({ tool_name, count })),
    by_tool: AGENT_TOOL_CALLS.map(([tool_name, count]) => ({ tool_name, count })),
    recent_events: AGENT_TAPE.map(([ago, tool_name, outcome]) => ({
      event_kind: 'mcp_tool_call',
      hook_name: '',
      outcome,
      timestamp: AGENT_TAPE_ANCHOR - ago,
      tool_name,
    })),
  };
}
