/**
 * The parse gate `data.ts` has always claimed to have.
 *
 * `data.ts` says its payloads are "gated against each route's single decoding
 * schema by `data.test.ts`". That file did not exist. Under the missing gate
 * the Automations scheduler fixture went on omitting `pending_review` after the
 * Rust handler made it required, so a healthy HTTP 200 decoded as
 * `unsupported_schema` — and the visual audit screenshotted that failure plate
 * for the Automations scheduler without anything noticing.
 *
 * What this suite pins is the DAEMON side: every fixture is parsed against the
 * generated contract for the route it answers, straight out of
 * `src/contracts/generated.ts`. That is deliberately a different question from
 * `src/workspaces/endpoint-fixtures.test.ts`, which parses the same fixtures
 * against what their *consuming workspace* decodes — including, for the routes
 * Rust still answers with a bare `Value`, hand-written mirrors of page-local
 * schemas. A mirror can be wrong in the same direction as the fixture; the
 * generated contract cannot, because it is derived from the Rust type.
 *
 * Coverage is enforced rather than assumed. Every key in `FIXTURES` must be
 * listed either in `CONTRACTS` (a generated schema exists for its handler) or
 * in `UNCONTRACTED` (it does not, with the reason), and an entry naming a route
 * that no longer has a fixture fails too. So a new fixture cannot be added
 * outside this gate by omission.
 */
import { describe, expect, it } from 'vitest';
import type { ZodType } from 'zod';

import { FIXTURES, FIXTURE_PREFIXES, resolveFixture } from './data.ts';
import {
  AnalyticsOverviewPayloadSchema,
  AnalyticsUsageSummarySchema,
  AutomationSchedulerStatusV1Schema,
  CodeIndexFreshnessPayloadSchema,
  CostsReadModelSchema,
  DoctorFindingsPayloadSchema,
  EnvelopeSchema,
  GraphNeighborsPayloadSchema,
  GraphOverviewPayloadSchema,
  GraphSearchPayloadSchema,
  GraphSubgraphPayloadSchema,
  LcmSessionPayloadSchema,
  LcmTimelinePayloadSchema,
  MemoryOverviewPayloadSchema,
  MemoryStatusPayloadSchema,
  ObservatoryReadModelSchema,
  ProjectContextPayloadSchema,
  ProjectsPayloadSchema,
  SavingsOverviewPayloadSchema,
  SavingsSessionsPayloadSchema,
  SettingsPayloadV1Schema,
  StorageFindingsPayloadSchema,
  StorageTelemetryPayloadSchema,
} from '../../src/contracts/generated.ts';

/** Parse one resolved fixture, surfacing zod's issues on failure — the same
 * reporting shape `endpoint-fixtures.test.ts` uses, so a drift report reads the
 * same whichever gate catches it. */
function expectParses(schema: ZodType<unknown>, pathname: string, search = ''): void {
  const result = schema.safeParse(resolveFixture(pathname, search));
  if (!result.success) {
    throw new Error(
      'fixture ' +
        pathname +
        search +
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
  '/api/projects': ProjectsPayloadSchema,
  '/api/storage/telemetry': EnvelopeSchema(StorageTelemetryPayloadSchema),
  '/api/storage/findings': EnvelopeSchema(StorageFindingsPayloadSchema),
  '/api/doctor/findings': EnvelopeSchema(DoctorFindingsPayloadSchema),
  '/api/settings': EnvelopeSchema(SettingsPayloadV1Schema),
  // `memory_api::overview` is bound at both the trailing-slash and bare paths.
  // The `/overview` key is a fixture convenience with no route behind it; it
  // holds the same payload, so it is held to the same contract.
  '/api/plugins/holographic/': MemoryOverviewPayloadSchema,
  '/api/plugins/holographic': MemoryOverviewPayloadSchema,
  '/api/plugins/holographic/overview': MemoryOverviewPayloadSchema,
  '/api/plugins/holographic/status': MemoryStatusPayloadSchema,
  '/api/plugins/hermes-lcm/timeline': LcmTimelinePayloadSchema,
  '/api/plugins/graph/overview': GraphOverviewPayloadSchema,
  '/api/plugins/graph/search': GraphSearchPayloadSchema,
  '/api/plugins/graph/subgraph': GraphSubgraphPayloadSchema,
  '/api/plugins/savings/overview': SavingsOverviewPayloadSchema,
  '/api/plugins/savings/sessions': SavingsSessionsPayloadSchema,
  '/api/plugins/analytics/overview': AnalyticsOverviewPayloadSchema,
  '/api/plugins/analytics/usage': AnalyticsUsageSummarySchema,
  '/api/automation/scheduler/status': AutomationSchedulerStatusV1Schema,
  '/api/observatory': EnvelopeSchema(ObservatoryReadModelSchema),
  '/api/costs': EnvelopeSchema(CostsReadModelSchema),
  '/api/code-index/freshness': EnvelopeSchema(CodeIndexFreshnessPayloadSchema),
};

/**
 * Fixture routes whose handler has no generated contract, each with why.
 *
 * These are the handlers that still build their response with `json!` or a
 * bare `serde_json::Value`, so there is no Rust type for `contract_schema.rs`
 * to export and nothing here to parse against. They are not exempt from
 * review — `endpoint-fixtures.test.ts` holds each to a mirror of its page's
 * own decoder — but that mirror is hand-written, so this list is the standing
 * record of which routes are still outside the generated boundary.
 */
const UNCONTRACTED: Readonly<Record<string, string>> = {
  '/api/capabilities': 'mod.rs `capabilities` builds the bundle with `json!`',
  '/api/plugins/hermes-lcm/overview': 'lcm_api::overview answers with a bare Value',
  '/api/plugins/hermes-lcm/search': 'lcm_api::search answers with a bare Value',
  '/api/plugins/analytics/hints': 'analytics_api::hints answers with a bare Value',
  '/api/plugins/analytics/underused': 'analytics_api::underused answers with a bare Value',
  '/api/plugins/analytics/diagnostics':
    'analytics_api::diagnostics_summary answers with a bare Value',
  '/api/automation/jobs': 'automation_jobs_api::list answers with a bare Value',
  '/api/automation/skills': 'automation_skills_api::list answers with a bare Value',
  '/api/automation/fact-proposals':
    'automation_fact_proposals_api::list answers with a bare Value',
};

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
    schema: ProjectContextPayloadSchema,
  },
  {
    label: 'project-scoped gateway rewrite',
    pathname: '/api/projects/tracedecay/plugins/graph/subgraph',
    schema: GraphSubgraphPayloadSchema,
  },
  {
    label: 'lcm_api::session',
    pathname: '/api/plugins/hermes-lcm/session/035c8f3c-d4e6-4176-afea-6f52e770501e',
    schema: LcmSessionPayloadSchema,
  },
  {
    label: 'graph_api::neighbors',
    pathname: '/api/plugins/graph/node/sym-0/neighbors',
    schema: GraphNeighborsPayloadSchema,
  },
  {
    label: 'graph_api::subgraph seeded',
    pathname: '/api/plugins/graph/subgraph',
    search: '?node_id=sym-0',
    schema: GraphSubgraphPayloadSchema,
  },
];

describe('fixtures parse against the generated contract for their route', () => {
  it.each(Object.keys(CONTRACTS))('GET %s', (pathname) => {
    expectParses(CONTRACTS[pathname]!, pathname);
  });

  it.each(DYNAMIC)('GET $pathname — $label', ({ pathname, search, schema }) => {
    expectParses(schema, pathname, search ?? '');
  });

  it('holds every fixture route to a contract or a recorded reason', () => {
    const classified = new Set([...Object.keys(CONTRACTS), ...Object.keys(UNCONTRACTED)]);
    const routes = Object.keys(FIXTURES);
    // A fixture added without a decision about its contract.
    expect(routes.filter((route) => !classified.has(route))).toEqual([]);
    // A decision left behind by a fixture that was removed or renamed, which
    // would otherwise read as coverage that no longer exists.
    expect([...classified].filter((route) => !(route in FIXTURES)).sort()).toEqual([]);
    // Neither map may claim a route the other one owns.
    for (const route of Object.keys(CONTRACTS)) expect(UNCONTRACTED[route]).toBeUndefined();
  });

  it('serves only already-gated payloads from the prefix fallbacks', () => {
    // A prefix fixture answers any path under it, so an ungated payload behind
    // one would reach the audit and the MSW tests without ever being parsed.
    // Every prefix therefore has to serve the same body some exact route above
    // already holds to a contract.
    const gated = Object.values(FIXTURES);
    for (const [prefix, payload] of FIXTURE_PREFIXES) {
      const matched = gated.some((fixture) => {
        try {
          expect(fixture).toEqual(payload);
          return true;
        } catch {
          return false;
        }
      });
      // The LCM session transcript is the one prefix with no exact route of
      // its own; it is parsed above as a dynamic route instead.
      if (prefix === '/api/plugins/hermes-lcm/session/') {
        expect(matched).toBe(false);
        continue;
      }
      expect(matched, `prefix ${prefix} serves a payload no exact route gates`).toBe(true);
    }
  });
});
