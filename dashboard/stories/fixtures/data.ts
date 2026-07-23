/**
 * Canonical fixture payloads for the dashboard `/api` surfaces. These stand in
 * for a running daemon so the visual audit and DOM/MSW tests never require the
 * live API to be up (plan 11a). Both the MSW handlers (`handlers.ts`) and the
 * Playwright route interceptor (`route.ts`) resolve from this single source, so
 * fixtures stay consistent across test transports.
 *
 * Shapes are hand-matched to the wire contracts in `src/contracts/wire.ts` and
 * the per-workspace `contracts.ts` files. Where a surface only needs a truthful
 * "populated but small" render, minimal-yet-valid data is provided; unmapped
 * routes fall back to an empty object (a truthful empty/unsupported state).
 */

const nowSecs = Math.floor(Date.now() / 1000);
const nowMicros = Date.now() * 1000;

/** DashboardEnvelopeV1 wrapper (see EnvelopeSchema in wire.ts). */
function envelope<T>(payload: T, domainState = 'ready'): Record<string, unknown> {
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
    legal_actions: [{ kind: 'refresh', operation: 'storage.refresh' }],
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

/** GET /api/projects — brain registry (contracts.ts ProjectsPayloadSchema). */
const projects: Record<string, unknown> = {
  status: 'ok',
  truncated: false,
  active_project_id: 'tracedecay',
  active_project_root: '/fast/projects/tracedecay',
  summary: { project_count: 3, repo_count: 2, truncated: false },
  project_tree: [
    {
      label: 'tracedecay',
      git_common_dir: '/fast/projects/tracedecay/.git',
      project_count: 2,
      branches: ['master', 'codex/tracedecay-total-redesign-plan'],
      projects: [
        projectEntry('tracedecay', 'tracedecay', '/fast/projects/tracedecay', 900),
        projectEntry('tracedecay-wt', 'tracedecay (worktree)', '/fast/projects/tracedecay-wt', 6 * 86_400),
      ],
    },
    {
      label: 'lynx-module-federation',
      git_common_dir: '/fast/projects/lynx/.git',
      project_count: 1,
      branches: ['main'],
      projects: [
        projectEntry('lynx-mf', 'lynx-module-federation', '/fast/projects/lynx', 40 * 86_400),
      ],
    },
  ],
};

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
      state: 'healthy_complete_coverage',
      required_source: 'storage_telemetry',
      reason: 'All stores are within advisory budgets.',
    },
    {
      kind: 'orphan_store',
      state: 'healthy_complete_coverage',
      required_source: 'store_registry',
      reason: 'No orphaned stores detected.',
    },
    {
      kind: 'stale_branch_dbs',
      state: 'partial',
      required_source: 'branch_registry',
      reason: 'Two branch databases have not been observed in 30 days.',
    },
    {
      kind: 'incident_debris_present',
      state: 'absent',
      required_source: 'incident_log',
      reason: 'No incident debris.',
    },
    {
      kind: 'retention_backlog',
      state: 'healthy_complete_coverage',
      required_source: 'retention_runner',
      reason: 'Retention is caught up.',
    },
  ],
  note: 'Findings reflect the most recent doctor sweep.',
});

/** Loose overview payloads for the plugin-backed workspaces. These use the
 * AnyObject-style loose schemas in the pages, so a small object renders an
 * "ok" (rather than unsupported) truthful state. */
const genericOverview: Record<string, unknown> = {
  status: 'ok',
  generated_at: nowMicros,
  summary: 'Fixture overview payload (daemon not required).',
  items: [],
  count: 0,
};

/**
 * Exact-path fixture map. Keys are the pathname (query string stripped by the
 * resolver). Anything not listed resolves to {} — a truthful empty surface.
 */
export const FIXTURES: Readonly<Record<string, unknown>> = {
  '/api/projects': projects,
  '/api/storage/telemetry': storageTelemetry,
  '/api/storage/findings': storageFindings,
  '/api/settings': {
    status: 'ok',
    layers: [{ source: 'defaults', values: {} }],
    effective: {},
  },
  // Plugin overview surfaces (loose schemas → render an ok/empty state).
  '/api/plugins/graph/overview': genericOverview,
  '/api/plugins/savings/overview': genericOverview,
  '/api/plugins/holographic/overview': genericOverview,
  '/api/plugins/holographic': genericOverview,
  '/api/plugins/hermes-lcm/overview': genericOverview,
  '/api/plugins/hermes-lcm/timeline': genericOverview,
  '/api/plugins/analytics/usage': genericOverview,
  '/api/plugins/analytics/hints': genericOverview,
  '/api/automation/scheduler/status': genericOverview,
  '/api/automation/jobs': genericOverview,
  '/api/automation/skills': genericOverview,
  '/api/automation/fact-proposals': genericOverview,
};

/** Prefix fixtures for query-bearing / dynamic routes (search etc.). The
 * resolver falls back to these when there is no exact-path match. */
export const FIXTURE_PREFIXES: ReadonlyArray<readonly [string, unknown]> = [
  ['/api/plugins/graph/search', genericOverview],
  ['/api/plugins/hermes-lcm/search', genericOverview],
  ['/api/plugins/holographic', genericOverview],
  ['/api/plugins/graph', genericOverview],
  ['/api/plugins/savings', genericOverview],
  ['/api/projects/', projects],
];

/** Empty-but-valid fallback for any unmapped /api route. */
export const EMPTY_FIXTURE: Record<string, unknown> = {};

/**
 * Resolve a request pathname to its fixture payload. Query strings must be
 * stripped by the caller. Returns EMPTY_FIXTURE when nothing matches so the
 * app always receives a decodable (if empty) body rather than a daemon error.
 */
export function resolveFixture(pathname: string): unknown {
  if (pathname in FIXTURES) return FIXTURES[pathname];
  for (const [prefix, payload] of FIXTURE_PREFIXES) {
    if (pathname.startsWith(prefix)) return payload;
  }
  return EMPTY_FIXTURE;
}
