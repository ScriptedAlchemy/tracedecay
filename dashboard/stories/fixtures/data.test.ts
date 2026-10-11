/**
 * The parse gate for `data.ts`.
 *
 * Every fixture is parsed against the generated contract for the route it
 * answers, straight out of `src/contracts/generated.ts`, so a healthy HTTP 200
 * never decodes as an unsupported schema in the visual audit. A route whose
 * generated payload sits inside a wrapper is parsed with the wrapper decoder
 * the browser itself uses, never a hand-written mirror: a mirror can be wrong
 * in the same direction as the fixture.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z, type ZodType } from 'zod';

import { resolveFixture, subagentTreeFixture } from './data.ts';
import {
  AnalyticsOverviewPayloadV1Schema,
  AnalyticsAgentsPayloadV1Schema,
  AnalyticsSubagentTreePayloadV1Schema,
  AnalyticsUsageSummaryV1Schema,
  AutomaticFactReceiptsPayloadV1Schema,
  AutomationJobsPayloadV1Schema,
  AutomationRunsPayloadV1Schema,
  AutomationSchedulerStatusV1Schema,
  AutomationSkillsPayloadV1Schema,
  CodeIndexFreshnessPayloadV1Schema,
  CostsReadModelV1Schema,
  DeliveryInboxV1Schema,
  DeliveryOverviewV1Schema,
  DoctorFindingsPayloadV1Schema,
  DashboardEnvelopeV1Schema,
  GraphNeighborsPayloadV1Schema,
  GraphOverviewPayloadV1Schema,
  GraphSearchPayloadV1Schema,
  GraphPathPayloadV1Schema,
  GraphSubgraphPayloadV1Schema,
  RevisionPairUnionLayoutV1Schema,
  SimilarResultV1Schema,
  LcmOverviewPayloadV1Schema,
  LcmSessionPayloadV1Schema,
  LcmTimelinePayloadV1Schema,
  LoomTemporalPayloadV1Schema,
  MemoryFactDetailPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  MemorySimilarityPayloadV1Schema,
  MemoryStatusPayloadV1Schema,
  ObservatoryReadModelV1Schema,
  ProjectContextPayloadV1Schema,
  ProjectsPayloadV1Schema,
  RemoteOperationalStatusPayloadV1Schema,
  SavingsModelsPayloadV1Schema,
  SavingsOverviewPayloadV1Schema,
  SettingsPayloadV1Schema,
  StorageTelemetryPayloadV1Schema,
  StructureReadV12Schema,
} from '../../src/contracts/generated.ts';
import { callWork, type WorkRoute } from '../../src/workspaces/work/workApi.ts';
import {
  WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE,
  WORK_VIEWS_ROUTE,
} from '../../src/workspaces/work/workRoutes.ts';
import {
  WORKFLOW_DEFINITION_HISTORY_ROUTE,
  WORKFLOW_GET_RUN_ROUTE,
  WORKFLOW_LIST_DEFINITIONS_ROUTE,
} from '../../src/workspaces/workflows/workflowRoutes.ts';
import { HANDOFF_LIST_TASK_ROUTE } from '../../src/workspaces/agents/handoffTokens.ts';
import { FEEDBACK_PROXIMITY_ROUTE } from '../../src/viz/proximity/proximityApi.ts';
import {
  AutomaticCuratorResponseSchema,
  AutomationOutcomesPayloadSchema,
} from '../../src/data/query/automation.ts';
import {
  ProjectionPayloadSchema,
  TrustHistoryPayloadSchema,
} from '../../src/data/query/memory.ts';

/** Parse one resolved fixture, surfacing zod's issues on failure. */
function expectParses(schema: ZodType<unknown>, pathname: string, search = ''): void {
  expectValue(schema, resolveFixture(pathname, search), pathname + search);
}

/** The same report, for a value already extracted from its wrapper. */
function expectValue(schema: ZodType<unknown>, value: unknown, what: string): void {
  const result = schema.safeParse(value);
  if (!result.success) {
    throw new Error(
      'fixture ' +
        what +
        ' failed its generated contract:\n' +
        JSON.stringify(result.error.issues, null, 2),
    );
  }
}

/**
 * Exact fixture route → the generated schema for the Rust handler bound to it
 * (route bindings in `src/dashboard/mod.rs`).
 */
const CONTRACTS: Readonly<Record<string, ZodType<unknown>>> = {
  '/api/projects': DashboardEnvelopeV1Schema(ProjectsPayloadV1Schema),
  '/api/storage/telemetry': DashboardEnvelopeV1Schema(StorageTelemetryPayloadV1Schema),
  '/api/doctor/findings': DashboardEnvelopeV1Schema(DoctorFindingsPayloadV1Schema),
  '/api/settings': DashboardEnvelopeV1Schema(SettingsPayloadV1Schema),
  '/api/plugins/holographic': DashboardEnvelopeV1Schema(MemoryOverviewPayloadV1Schema),
  '/api/plugins/holographic/status': DashboardEnvelopeV1Schema(MemoryStatusPayloadV1Schema),
  '/api/plugins/hermes-lcm/overview': DashboardEnvelopeV1Schema(LcmOverviewPayloadV1Schema),
  '/api/plugins/hermes-lcm/timeline': DashboardEnvelopeV1Schema(LcmTimelinePayloadV1Schema),
  '/api/plugins/graph/overview': DashboardEnvelopeV1Schema(GraphOverviewPayloadV1Schema),
  '/api/plugins/graph/search': DashboardEnvelopeV1Schema(GraphSearchPayloadV1Schema),
  '/api/plugins/graph/subgraph': DashboardEnvelopeV1Schema(GraphSubgraphPayloadV1Schema),
  '/api/plugins/graph/path': DashboardEnvelopeV1Schema(GraphPathPayloadV1Schema),
  // `StructureReadV12` is the schemars-deduplicated alias whose `measured`
  // variant carries `StrataMeasurementV1`.
  '/api/plugins/graph/strata': DashboardEnvelopeV1Schema(StructureReadV12Schema),
  '/api/plugins/graph/compare/union-layout': DashboardEnvelopeV1Schema(
    RevisionPairUnionLayoutV1Schema,
  ),
  '/api/loom/temporal': DashboardEnvelopeV1Schema(LoomTemporalPayloadV1Schema),
  '/api/delivery/overview': DashboardEnvelopeV1Schema(DeliveryOverviewV1Schema),
  '/api/delivery/inbox': DashboardEnvelopeV1Schema(DeliveryInboxV1Schema),
  '/api/plugins/savings/overview': DashboardEnvelopeV1Schema(SavingsOverviewPayloadV1Schema),
  '/api/plugins/savings/models': SavingsModelsPayloadV1Schema,
  '/api/plugins/analytics/overview': DashboardEnvelopeV1Schema(AnalyticsOverviewPayloadV1Schema),
  '/api/plugins/analytics/usage': DashboardEnvelopeV1Schema(AnalyticsUsageSummaryV1Schema),
  '/api/plugins/analytics/agents': DashboardEnvelopeV1Schema(AnalyticsAgentsPayloadV1Schema),
  '/api/plugins/analytics/subagent-tree': DashboardEnvelopeV1Schema(
    AnalyticsSubagentTreePayloadV1Schema,
  ),
  '/api/automation/scheduler/status': AutomationSchedulerStatusV1Schema,
  '/api/automation/jobs': AutomationJobsPayloadV1Schema,
  '/api/automation/skills': AutomationSkillsPayloadV1Schema,
  '/api/automation/automatic-fact-receipts': AutomaticFactReceiptsPayloadV1Schema,
  '/api/automation/runs': AutomationRunsPayloadV1Schema,
  '/api/automation/outcomes': AutomationOutcomesPayloadSchema,
  '/api/application/retained/fact_store_curate': AutomaticCuratorResponseSchema,
  '/api/observatory': DashboardEnvelopeV1Schema(ObservatoryReadModelV1Schema),
  '/api/costs': DashboardEnvelopeV1Schema(CostsReadModelV1Schema),
  '/api/code-index/freshness': DashboardEnvelopeV1Schema(CodeIndexFreshnessPayloadV1Schema),
  '/api/remote/status': DashboardEnvelopeV1Schema(RemoteOperationalStatusPayloadV1Schema),
};

const APPLICATION_ROUTES: Readonly<Record<string, WorkRoute<unknown, unknown>>> = Object.fromEntries(
  [
    WORK_VIEWS_ROUTE,
    WORKFLOW_LIST_DEFINITIONS_ROUTE,
    WORKFLOW_DEFINITION_HISTORY_ROUTE,
    WORKFLOW_GET_RUN_ROUTE,
    HANDOFF_LIST_TASK_ROUTE,
    WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE,
    FEEDBACK_PROXIMITY_ROUTE,
  ].map((route): [string, WorkRoute<unknown, unknown>] => [route.path, route]),
);

/**
 * The routes the resolver synthesizes rather than looking up in `FIXTURES`:
 * the project-context route, the project-scoped gateway rewrite, the LCM
 * session transcript, the graph neighborhood, and the seeded subgraph.
 */
const DYNAMIC: ReadonlyArray<{
  readonly label: string;
  readonly pathname: string;
  readonly search?: string;
  readonly schema: ZodType<unknown>;
}> = [
  {
    label: 'projects::context',
    pathname: '/api/projects/tracedecay',
    schema: DashboardEnvelopeV1Schema(ProjectContextPayloadV1Schema),
  },
  {
    label: 'project-scoped gateway rewrite',
    pathname: '/api/projects/tracedecay/plugins/graph/subgraph',
    schema: DashboardEnvelopeV1Schema(GraphSubgraphPayloadV1Schema),
  },
  {
    label: 'loom_api::temporal dense-fanout page',
    pathname: '/api/loom/temporal',
    search: '?limit=200&fixture=dense-fanout',
    schema: DashboardEnvelopeV1Schema(LoomTemporalPayloadV1Schema),
  },
  {
    label: 'lcm_api::session',
    pathname: '/api/plugins/hermes-lcm/session/035c8f3c-d4e6-4176-afea-6f52e770501e',
    schema: DashboardEnvelopeV1Schema(LcmSessionPayloadV1Schema),
  },
  {
    label: 'graph_api::neighbors',
    pathname: '/api/plugins/graph/node/sym-0/neighbors',
    schema: DashboardEnvelopeV1Schema(GraphNeighborsPayloadV1Schema),
  },
  {
    label: 'memory_api::fact_detail held fact',
    pathname: `/api/plugins/holographic/fact/${encodeURIComponent(`fact.${'a'.repeat(64)}.${'0'.repeat(64)}`)}`,
    schema: DashboardEnvelopeV1Schema(MemoryFactDetailPayloadV1Schema),
  },
  {
    label: 'memory_api::fact_detail unknown identity',
    pathname: '/api/plugins/holographic/fact/fact.unknown',
    schema: DashboardEnvelopeV1Schema(z.null()),
  },
  {
    label: 'memory_api::fact_trust_history',
    pathname: `/api/plugins/holographic/fact/${encodeURIComponent(`fact.${'a'.repeat(64)}.${'0'.repeat(64)}`)}/trust-history`,
    schema: TrustHistoryPayloadSchema,
  },
  {
    label: 'memory_api::projection',
    pathname: '/api/plugins/holographic/projection',
    search: '?limit=400',
    schema: ProjectionPayloadSchema,
  },
  {
    label: 'memory_api::projection filtered',
    pathname: '/api/plugins/holographic/projection',
    search: '?limit=400&q=decision',
    schema: ProjectionPayloadSchema,
  },
  {
    label: 'memory_api::similarity',
    pathname: '/api/plugins/holographic/similarity',
    search: '?min_similarity=0.85&limit=25',
    schema: MemorySimilarityPayloadV1Schema,
  },
  {
    label: 'graph_api::subgraph seeded',
    pathname: '/api/plugins/graph/subgraph',
    search: '?node_id=sym-0',
    schema: DashboardEnvelopeV1Schema(GraphSubgraphPayloadV1Schema),
  },
  {
    label: 'code_read_api::shared_family conservative, first page',
    pathname: '/api/plugins/graph/shared-code/family',
    search: '?symbol_occurrence_id=sym-0&match_class=conservative_exact&limit=100',
    schema: DashboardEnvelopeV1Schema(SimilarResultV1Schema),
  },
  {
    label: 'code_read_api::shared_family conservative, cursor page',
    pathname: '/api/plugins/graph/shared-code/family',
    search: '?symbol_occurrence_id=sym-0&match_class=conservative_exact&limit=100&cursor=cursor.family.page-2',
    schema: DashboardEnvelopeV1Schema(SimilarResultV1Schema),
  },
  {
    label: 'code_read_api::shared_family rename-normalized',
    pathname: '/api/plugins/graph/shared-code/family',
    search: '?symbol_occurrence_id=sym-0&match_class=rename_normalized_exact&limit=100',
    schema: DashboardEnvelopeV1Schema(SimilarResultV1Schema),
  },
  {
    label: 'savings_api::models today',
    pathname: '/api/plugins/savings/models',
    search: '?range=today',
    schema: SavingsModelsPayloadV1Schema,
  },
  {
    label: 'savings_api::models 30d',
    pathname: '/api/plugins/savings/models',
    search: '?range=30d',
    schema: SavingsModelsPayloadV1Schema,
  },
  {
    label: 'doctor_findings_api::findings storage family',
    pathname: '/api/doctor/findings',
    search: '?family=storage',
    schema: DashboardEnvelopeV1Schema(DoctorFindingsPayloadV1Schema),
  },
];

describe('fixtures parse against the generated contract for their route', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it.each(Object.keys(CONTRACTS))('GET %s', (pathname) => {
    expectParses(CONTRACTS[pathname]!, pathname);
  });

  it.each(DYNAMIC)('GET $pathname: $label', ({ pathname, search, schema }) => {
    expectParses(schema, pathname, search ?? '');
  });

  it.each(Object.keys(APPLICATION_ROUTES))(
    'POST %s: application envelope, generated payload',
    async (pathname) => {
      vi.stubGlobal('fetch', async () => Response.json(resolveFixture(pathname)));
      const route = { ...APPLICATION_ROUTES[pathname]!, request: z.unknown() };
      expect(await callWork(route, {}, pathname)).toMatchObject({ outcome: 'value' });
    },
  );

});

describe('memory geometry fixtures answer the request they were given', () => {
  it('bounds the projection by its limit and filters by q', () => {
    const page = ProjectionPayloadSchema.parse(
      resolveFixture('/api/plugins/holographic/projection', '?limit=400'),
    );
    expect(page.points).toHaveLength(400);
    expect(page.coverage).toEqual({
      completeness: 'bounded',
      examined: 400,
      limit: 400,
      omission_reasons: ['request_limit_reached'],
    });
    const filtered = ProjectionPayloadSchema.parse(
      resolveFixture('/api/plugins/holographic/projection', '?limit=400&q=decision'),
    );
    expect(filtered.points).toHaveLength(67);
    expect(new Set(filtered.points.map((point) => point.category))).toEqual(new Set(['decision']));
  });

  it('applies the similarity floor before the cap and bins every scored pair', () => {
    const at = (floor: number) =>
      MemorySimilarityPayloadV1Schema.parse(
        resolveFixture('/api/plugins/holographic/similarity', `?min_similarity=${floor}&limit=25`),
      );
    const strict = at(0.95);
    expect(strict.pairs).toHaveLength(5);
    expect(new Set(strict.pairs.map((pair) => pair.classification))).toEqual(new Set(['likely_duplicate']));
    expect(at(0.85).pairs).toHaveLength(16);
    expect(at(0.6).pairs).toHaveLength(25);
    expect(strict.total_pairs).toBe(79_800);
    expect(strict.score_distribution.bins.reduce((sum, bin) => sum + bin.count, 0)).toBe(79_800);
    expect(strict.score_distribution.bins[0]).toEqual({ start: -0.25, end: -0.188, count: 558 });
  });
});

describe('subagent-tree scenarios', () => {
  const schema = DashboardEnvelopeV1Schema(AnalyticsSubagentTreePayloadV1Schema);

  it('keeps the default tree at five sessions and serves the dense fan-out only when asked', () => {
    const route = '/api/plugins/analytics/subagent-tree';
    expect(schema.parse(resolveFixture(route)).payload.nodes).toHaveLength(5);
    expect(schema.parse(resolveFixture(route, '?fixture=dense-fanout')).payload.nodes).toHaveLength(124);
  });

  it('decodes the dense fan-out and reconciles its own counts', () => {
    const payload = schema.parse(subagentTreeFixture('dense-fanout')).payload;
    const agents = new Set(payload.nodes.map((node) => node.session_id));
    expect(agents.size).toBe(124);
    expect(payload.nodes.filter((node) => node.depth === 2)).toHaveLength(96);
    expect(payload.nodes.filter((node) => node.link === 'missing_parent')).toHaveLength(payload.missing_parent_count);
    expect(payload.nodes.filter((node) => node.ended_at === null)).toHaveLength(13);
    // Pre-order: every child follows its parent, and `descendants` counts it.
    const position = new Map(payload.nodes.map((node, index) => [node.session_id, index]));
    for (const node of payload.nodes) {
      if (node.depth > 0) expect(position.get(node.parent_session_id!)!).toBeLessThan(position.get(node.session_id)!);
    }
    expect(payload.nodes[0]!.descendants).toBe(120);
  });
});
