/**
 * Parse gate for the visual-audit / interaction fixtures.
 *
 * Every `/api` route the 12 workspaces consume is served from
 * `stories/fixtures/data.ts` during the visual audit and MSW/DOM tests. This
 * suite asserts that each fixture payload parses against the exact zod schema
 * its consuming workspace validates it with — either an exported
 * per-workspace `contracts.ts` schema, an exported wire schema, or (for pages
 * whose schema is a module-local const) a faithful mirror of that page's schema
 * with a source citation. If a fixture drifts from a contract, this fails.
 *
 * The per-endpoint density assertions encode the fixture spec (e.g. ≥25 facts,
 * ≥30 sessions across 3 providers, ≥250 graph search rows) so a fixture that
 * parses but renders an empty surface still fails the gate.
 */
import { describe, expect, it } from 'vitest';
import { z } from 'zod';
import type { ZodType } from 'zod';

import { resolveFixture } from '../../stories/fixtures/data.ts';
import { AnyObject } from '../data/query/legacy.ts';
import {
  EnvelopeSchema,
  StorageTelemetryPayloadSchema,
  DoctorFindingsPayloadSchema,
} from '../contracts/wire.ts';
import { ObservatoryStorageFindingsPayloadSchema } from './observatory/contracts.ts';
import {
  ProjectContextPayloadSchema,
  ProjectsPayloadSchema,
  ScopedAnalyticsOverviewSchema,
  ScopedMemoryStatusSchema,
  ScopedSubgraphPayloadSchema,
} from './brain/contracts.ts';
import {
  ANALYTICS_EVENT_LIMIT,
  describeWindow,
  familiesSummary,
  familyVerdict,
  summarizeDominance,
  type FamilyRow,
} from './agents/usage.ts';
import { columnIndexFor, indexedMass } from './brain/field.ts';
import { DeliveryProjectsPayloadSchema } from './delivery/contracts.ts';
import { composeDeliveryField } from './delivery/field.ts';
import {
  LoomChainPayloadSchema,
  LoomSessionsPayloadSchema,
} from './loom/contracts.ts';
import { composeWeave, summarizeChain } from './loom/weave.ts';
import {
  GraphOverviewPayloadSchema,
  GraphSearchPayloadSchema,
  SubgraphPayloadSchema,
} from './code/contracts.ts';
import { MemoryOverviewPayloadSchema, MemoryStatusSchema } from './knowledge/contracts.ts';
import { composeTrustDistribution } from './knowledge/trust.ts';
import { SavingsOverviewPayloadSchema } from './costs/contracts.ts';

/** Parse a resolved fixture, surfacing zod issues on failure. */
function parse<T>(schema: ZodType<T>, pathname: string, search = ''): T {
  const fixture = resolveFixture(pathname, search);
  const result = schema.safeParse(fixture);
  if (!result.success) {
    const issues = JSON.stringify(result.error.issues, null, 2);
    throw new Error('fixture ' + pathname + search + ' failed its contract:\n' + issues);
  }
  return result.data;
}

/* --- Faithful mirrors of module-local page schemas (not exported) ---------- */

// SessionsPage.tsx: OverviewPayload. (LoomPage no longer reads this route —
// `/api/plugins/hermes-lcm/overview` 500s on the real profile, so the weave
// draws from `/api/plugins/savings/sessions` instead; see loom/contracts.ts.)
const OverviewPayload = z
  .object({ latest_sessions: z.array(AnyObject).optional() })
  .passthrough();

// SessionsPage.tsx: TimelinePayload.
const TimelinePayload = z
  .object({ buckets: z.array(AnyObject).optional() })
  .passthrough();

// ExplorerPage.tsx: ListPayload.
const ListPayload = z
  .object({
    results: z.array(AnyObject).optional(),
    items: z.array(AnyObject).optional(),
    nodes: z.array(AnyObject).optional(),
    facts: z.array(AnyObject).optional(),
  })
  .passthrough();

// ExplorerPage.tsx: MemoryListPayload.
const MemoryListPayload = z
  .object({
    holographic: z
      .object({ facts: z.array(AnyObject).optional() })
      .passthrough()
      .optional(),
  })
  .passthrough();

// AgentsPage.tsx: UsagePayload.
const UsagePayload = z
  .object({
    available: z.boolean(),
    event_count: z.number().optional(),
    by_category: z
      .array(
        z
          .object({ kind: z.string(), category: z.string(), events: z.number() })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();

// AgentsPage.tsx: HintsPayload.
const HintsPayload = z
  .object({
    available: z.boolean().optional(),
    families: z.array(AnyObject).optional(),
  })
  .passthrough();

// AgentsPage.tsx: DiagnosticsPayload.
const DiagnosticsPayload = z
  .object({
    available: z.boolean().optional(),
    event_count: z.number().optional(),
    events_per_hour: z.number().optional(),
    hook_call_count: z.number().optional(),
    mcp_tool_call_count: z.number().optional(),
    by_event_kind: z.array(AnyObject).optional(),
    by_outcome: z.array(AnyObject).optional(),
    by_mcp_tool: z.array(AnyObject).optional(),
    recent_events: z
      .array(
        z
          .object({
            timestamp: z.number(),
            event_kind: z.string().optional(),
            tool_name: z.string().optional(),
            outcome: z.string().optional(),
          })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();

// AutomationsPage.tsx: SchedulerStatusSchema.
const SchedulerStatusSchema = z
  .object({
    status: z.string(),
    paused: z.boolean(),
    enabled: z.boolean().optional(),
    scheduler_tick_secs: z.number().optional(),
    pending_fact_proposals: z.number().optional(),
    pending_skills: z.number().optional(),
    last_session_activity: z.number().nullable().optional(),
  })
  .passthrough();

// AutomationsPage.tsx: JobsPayloadSchema.
const JobsPayloadSchema = z
  .object({
    jobs: z.array(
      z
        .object({
          id: z.string(),
          name: z.string(),
          schedule: z.string().nullable().optional(),
          enabled: z.boolean(),
          interval_secs: z.number().nullable().optional(),
        })
        .passthrough(),
    ),
    count: z.number(),
  })
  .passthrough();

// AutomationsPage.tsx: SkillsPayloadSchema.
const SkillsPayloadSchema = z
  .object({ skills: z.array(AnyObject).optional(), items: z.array(AnyObject).optional() })
  .passthrough();

describe('endpoint fixtures parse against their consuming contracts', () => {
  it('GET /api/projects — brain (ProjectsPayloadSchema)', () => {
    const data = parse(ProjectsPayloadSchema, '/api/projects');
    expect(data.project_tree.length).toBeGreaterThanOrEqual(2);
    // Density spec for Brain's field: the surface composes projects into five
    // recency columns against a mass axis, so a fixture that lands everything
    // in one column or at one mass renders a picture that cannot be reviewed.
    const entries = data.project_tree.flatMap((group) => group.projects);
    expect(entries.length).toBeGreaterThanOrEqual(20);
    const columns = new Set(entries.map((e) => columnIndexFor(e.last_seen_at, Date.now() / 1000)));
    expect(columns.size).toBe(5);
    const masses = entries.map(indexedMass);
    expect(Math.max(...masses) / Math.max(Math.min(...masses), 1)).toBeGreaterThan(20);
  });

  it('GET /api/projects/{id} — scoped brain backbone (ProjectContextPayloadSchema)', () => {
    const data = parse(ProjectContextPayloadSchema, '/api/projects/tracedecay');
    expect(data.project?.project_id).toBe('tracedecay');
    const stores = data.stores ?? [];
    expect(stores.length).toBeGreaterThanOrEqual(1);
    expect((stores[0]?.graph_scopes ?? []).length).toBeGreaterThanOrEqual(2);
    // Artifact byte sizes drive the rail's magnitude meters; without them the
    // whole holdings panel renders em dashes.
    expect(
      (stores[0]?.artifacts ?? []).every((a) => (a.size_bytes ?? 0) > 0),
    ).toBe(true);
    expect((data.aliases ?? []).length).toBeGreaterThanOrEqual(2);
  });

  it('resolves the project-scoped gateway the way the daemon rewrites it', () => {
    // src/dashboard/mod.rs binds `/api/projects/{id}/{*tail}` and serves
    // `/api/{tail}` against that project's own state. If the fixture layer did
    // not mirror that, every scoped read would resolve to the registry payload
    // and the scoped surfaces would be audited against a shape the daemon never
    // sends.
    expect(resolveFixture('/api/projects/tracedecay/plugins/graph/overview')).toEqual(
      resolveFixture('/api/plugins/graph/overview'),
    );
    expect(resolveFixture('/api/projects/tracedecay/plugins/holographic/status')).toEqual(
      resolveFixture('/api/plugins/holographic/status'),
    );
  });

  it('GET /api/plugins/graph/subgraph — scoped brain field (ScopedSubgraphPayloadSchema)', () => {
    const data = parse(
      ScopedSubgraphPayloadSchema,
      '/api/projects/tracedecay/plugins/graph/subgraph',
    );
    expect((data.nodes ?? []).length).toBeGreaterThanOrEqual(20);
    expect((data.edges ?? []).length).toBeGreaterThanOrEqual(20);
  });

  it('GET /api/plugins/holographic/status — scoped brain (ScopedMemoryStatusSchema)', () => {
    const data = parse(ScopedMemoryStatusSchema, '/api/plugins/holographic/status');
    expect(data.exists).toBe(true);
    expect(data.memory?.fact_count).toBeGreaterThan(0);
    expect(data.memory?.entity_count).toBeGreaterThan(0);
  });

  it('GET /api/plugins/holographic/status — knowledge trust fallback (MemoryStatusSchema)', () => {
    const data = parse(MemoryStatusSchema, '/api/plugins/holographic/status');
    // These four bands are the ONLY trust distribution a real store serves
    // correctly, and KnowledgePage falls back to them when the overview's
    // ten-bucket histogram comes back all-zero (which it always does live).
    // They have to be present and to carry mass in more than one band, or the
    // fallback is untested.
    const distribution = composeTrustDistribution(undefined, data.memory, []);
    expect(distribution.source).toBe('status_bands');
    expect(distribution.total).toBe(data.memory?.fact_count);
    expect(distribution.occupied).toBeGreaterThanOrEqual(2);
    expect(distribution.degenerate).toBe(false);
  });

  it('GET /api/plugins/analytics/overview — scoped brain (ScopedAnalyticsOverviewSchema)', () => {
    const data = parse(ScopedAnalyticsOverviewSchema, '/api/plugins/analytics/overview');
    expect(data.usage?.event_count).toBeGreaterThan(0);
    expect((data.usage?.by_category ?? []).length).toBeGreaterThanOrEqual(3);
  });

  it('GET /api/projects — delivery (DeliveryProjectsPayloadSchema)', () => {
    const data = parse(DeliveryProjectsPayloadSchema, '/api/projects');
    const checkouts = (data.project_tree ?? []).flatMap((g) => g.projects);
    expect(checkouts.some((c) => c.kind === 'worktree')).toBe(true);

    // The delivery field has to compose into something readable, and the
    // bounds below encode the SHAPE of the real registry, not just its size.
    const field = composeDeliveryField(data.project_tree ?? []);
    expect(field.bodies.length).toBeGreaterThanOrEqual(10);
    // Branch counts must be skewed, or the log y axis is untested.
    expect(field.branchCeiling / Math.max(field.branchFloor, 1)).toBeGreaterThan(5);
    // Repositories must land in more than one recency column, or the x axis
    // renders as a single occupied stripe and proves nothing.
    expect(field.columns.filter((column) => column.count > 0).length).toBeGreaterThanOrEqual(2);
    // Multi-checkout repositories exercise the size channel, which the real
    // registry currently has nothing to spend.
    expect(field.multiCheckoutCount).toBeGreaterThan(0);
    // No body may be pushed out of its own recency column by packing.
    for (const body of field.bodies) {
      expect(Math.abs(body.offset)).toBeLessThanOrEqual(0.4);
    }
  });

  it('GET /api/plugins/holographic/ — knowledge (MemoryOverviewPayloadSchema)', () => {
    const data = parse(MemoryOverviewPayloadSchema, '/api/plugins/holographic/');
    const facts = data.holographic.facts ?? [];
    expect(facts.length).toBeGreaterThanOrEqual(25);
    // Trust spread: facts land in more than one histogram bucket.
    const buckets = new Set(facts.map((f) => Math.floor(f.trust_score * 10)));
    expect(buckets.size).toBeGreaterThanOrEqual(6);
    expect((data.holographic.overview?.trust_histogram ?? []).length).toBe(10);
    expect((data.holographic.entities ?? []).length).toBeGreaterThanOrEqual(6);
    // Category composition: more than one category, and counts that vary
    // enough to make a ranked rail meaningful rather than a flat line.
    const categories = data.holographic.overview?.categories ?? [];
    expect(categories.length).toBeGreaterThanOrEqual(3);
    const categoryCounts = new Set(categories.map((c) => c.count));
    expect(categoryCounts.size).toBeGreaterThanOrEqual(2);
    // Growth: enough periods to draw a trend, and a monotonically
    // non-decreasing running total (it is a cumulative counter).
    const growth = data.holographic.overview?.growth ?? [];
    expect(growth.length).toBeGreaterThanOrEqual(6);
    for (let i = 1; i < growth.length; i += 1) {
      expect(growth[i]!.cumulative_facts).toBeGreaterThanOrEqual(
        growth[i - 1]!.cumulative_facts,
      );
    }
  });

  it('GET /api/plugins/holographic/ — explorer (MemoryListPayload)', () => {
    const data = parse(MemoryListPayload, '/api/plugins/holographic/');
    expect((data.holographic?.facts ?? []).length).toBeGreaterThanOrEqual(25);
  });

  it('GET /api/plugins/hermes-lcm/overview — sessions/loom (OverviewPayload)', () => {
    const data = parse(OverviewPayload, '/api/plugins/hermes-lcm/overview');
    const sessions = data.latest_sessions ?? [];
    expect(sessions.length).toBeGreaterThanOrEqual(30);
    const providers = new Set(sessions.map((s) => String(s['provider'])));
    expect(providers.size).toBe(3);
    for (const s of sessions) {
      expect(typeof s['first_timestamp']).toBe('number');
      expect(typeof s['last_timestamp']).toBe('number');
      expect(typeof s['message_count']).toBe('number');
      expect(Number(s['first_timestamp'])).toBeLessThan(Number(s['last_timestamp']));
    }
  });

  it('GET /api/plugins/savings/sessions — loom threads (LoomSessionsPayloadSchema)', () => {
    const data = parse(LoomSessionsPayloadSchema, '/api/plugins/savings/sessions');
    const sessions = data.sessions ?? [];
    expect(sessions.length).toBeGreaterThanOrEqual(30);
    expect(new Set(sessions.map((s) => s.provider)).size).toBe(3);

    // The weave has to compose the fixture into something with structure in
    // it, or the audit shot is a picture of nothing. These bounds encode the
    // distribution the real store has (plan 11a finding 4), not just its size.
    const weave = composeWeave(sessions);
    expect(weave.hosts).toHaveLength(3);
    expect(weave.threads.length).toBe(sessions.length);

    // Most sessions carry no served end: the open-thread idiom is the whole
    // honesty story of this surface and must be exercised, not bypassed.
    expect(weave.openEndedCount).toBeGreaterThan(weave.threads.length / 2);
    // ...but not ALL of them, or the measured-extent state never renders.
    expect(weave.openEndedCount).toBeLessThan(weave.threads.length);

    // Zero-message sessions exist and are drawn hollow.
    expect(weave.hollowCount).toBeGreaterThan(0);
    // Subagents exist, so the head crossbar renders.
    expect(weave.threads.some((thread) => thread.isSubagent)).toBe(true);
    // Model attribution reaches the detail rail.
    expect(weave.threads.some((thread) => thread.models.length > 0)).toBe(true);

    // Skew, not merely magnitude: a uniform fixture would never show that the
    // width channel needed a log scale.
    expect(weave.messageCeiling).toBeGreaterThan(500);
    const median = [...weave.threads]
      .map((thread) => thread.messages)
      .sort((a, b) => a - b)[Math.floor(weave.threads.length / 2)]!;
    expect(weave.messageCeiling / Math.max(median, 1)).toBeGreaterThan(20);

    // Every row is placeable: a fixture row with no start would silently
    // shrink the field instead of failing here.
    expect(weave.undated).toBe(0);
    expect(weave.extent).not.toBeNull();
  });

  it('GET /api/plugins/hermes-lcm/session/{id} — loom chain (LoomChainPayloadSchema)', () => {
    const data = parse(
      LoomChainPayloadSchema,
      '/api/plugins/hermes-lcm/session/035c8f3c-d4e6-4176-afea-6f52e770501e',
    );
    expect(data.exists).toBe(true);
    const summary = summarizeChain(data.messages ?? [], data.counts, false);
    expect(summary.steps.length).toBeGreaterThanOrEqual(20);
    // Roles and tools both populate, so the composition and tool-histogram
    // sections of the rail render rather than falling to their zero states.
    expect(summary.roles.length).toBeGreaterThanOrEqual(2);
    expect(summary.tools.length).toBeGreaterThanOrEqual(3);
    // The daemon serves no per-message timestamp. The fixture must not invent
    // one, or the audit never shoots the "ordinal order" caption that the real
    // surface always shows.
    expect(summary.timestamped).toBe(false);
  });

  it('GET /api/plugins/hermes-lcm/timeline — sessions (TimelinePayload)', () => {
    const data = parse(TimelinePayload, '/api/plugins/hermes-lcm/timeline');
    expect((data.buckets ?? []).length).toBe(46);
  });

  it('GET /api/plugins/hermes-lcm/search — explorer (ListPayload)', () => {
    const data = parse(ListPayload, '/api/plugins/hermes-lcm/search', '?q=lynx');
    expect((data.results ?? []).length).toBeGreaterThan(0);
  });

  it('GET /api/plugins/graph/overview — code (GraphOverviewPayloadSchema)', () => {
    const data = parse(GraphOverviewPayloadSchema, '/api/plugins/graph/overview');
    const hubs = (data.top_connected ?? []) as Array<Record<string, unknown>>;
    // `graph_queries::top_connected_rows` is a `LIMIT 12` subquery selecting
    // exactly five columns, so the fixture must serve twelve rows of that
    // shape and no more. The old bound (>= 15) locked in a fixture that
    // emitted eighteen FULL node records — a payload the daemon cannot
    // produce — which meant the Code workspace was being designed and audited
    // against fields (`qualified_name`, `signature`, `start_line`) this route
    // never returns.
    expect(hubs.length).toBe(12);
    for (const hub of hubs) {
      expect(typeof hub['degree']).toBe('number');
      expect(Object.keys(hub).sort()).toEqual([
        'degree',
        'file_path',
        'id',
        'kind',
        'name',
      ]);
    }
    // Degrees arrive already ranked, highest first.
    const degrees = hubs.map((hub) => hub['degree'] as number);
    expect([...degrees].sort((a, b) => b - a)).toEqual(degrees);
    expect(data.totals.nodes).toBeGreaterThan(0);
  });

  it('GET /api/plugins/graph/search — code (GraphSearchPayloadSchema)', () => {
    const data = parse(GraphSearchPayloadSchema, '/api/plugins/graph/search', '?q=service');
    expect((data.results ?? []).length).toBeGreaterThanOrEqual(250);
  });

  it('GET /api/plugins/graph/search — explorer (ListPayload)', () => {
    const data = parse(ListPayload, '/api/plugins/graph/search', '?q=service');
    expect((data.results ?? []).length).toBeGreaterThanOrEqual(250);
  });

  it('GET /api/plugins/graph/subgraph — code unseeded (SubgraphPayloadSchema)', () => {
    const data = parse(SubgraphPayloadSchema, '/api/plugins/graph/subgraph');
    expect(data.seed_id).toBeNull();
    expect(data.mode).toBe('default');
    expect(data.nodes.length).toBeGreaterThanOrEqual(30);
    expect(data.edges.length).toBeGreaterThanOrEqual(40);
  });

  it('GET /api/plugins/graph/subgraph?node_id= — code seeded (SubgraphPayloadSchema)', () => {
    const data = parse(SubgraphPayloadSchema, '/api/plugins/graph/subgraph', '?node_id=sym-0');
    expect(data.seed_id).toBe('sym-0');
    expect(data.mode).toBe('seeded');
    expect(data.nodes.some((n) => n.id === 'sym-0')).toBe(true);
    expect(data.nodes.length).toBeLessThan(40);
  });

  it('GET /api/plugins/savings/overview — costs (SavingsOverviewPayloadSchema)', () => {
    const data = parse(SavingsOverviewPayloadSchema, '/api/plugins/savings/overview');
    expect(data.savings.available).toBe(true);
    expect(data.savings.ledger?.today.saved_tokens).toBeGreaterThan(0);
    expect(data.savings.ledger?.all_time.saved_tokens).toBeGreaterThan(0);
    expect((data.savings.lifetime_counters?.projects ?? []).length).toBeGreaterThanOrEqual(4);
    expect(data.turns.available).toBe(true);
  });

  it('GET /api/plugins/analytics/usage — agents (UsagePayload)', () => {
    const data = parse(UsagePayload, '/api/plugins/analytics/usage');
    expect(data.available).toBe(true);
    const rows = data.by_category ?? [];
    expect(rows.length).toBeGreaterThanOrEqual(3);
    // The density spec for this endpoint is NOT "many categories" — the real
    // payload has four. It is that the distribution is DEGENERATE, because
    // that is the property the plate is built to survive. A fixture that
    // spreads its events evenly lets a linear rail pass the audit while the
    // live surface renders slivers.
    const summary = summarizeDominance(rows);
    expect(summary.dominant).toBe(true);
    expect(summary.spread ?? 0).toBeGreaterThan(1000);
    // ...and that the count is the endpoint's own cap, not a true total, so
    // the caption that says so is always exercised.
    expect(data.event_count).toBe(ANALYTICS_EVENT_LIMIT);
    expect(summary.total).toBeLessThan(data.event_count!);
  });

  it('GET /api/plugins/analytics/hints — agents (HintsPayload)', () => {
    const data = parse(HintsPayload, '/api/plugins/analytics/hints');
    expect(data.available).toBe(true);
  });

  it('GET /api/plugins/analytics/underused — agents (HintsPayload)', () => {
    const data = parse(HintsPayload, '/api/plugins/analytics/underused');
    const families = (data.families ?? []) as unknown as FamilyRow[];
    expect(families.length).toBe(4);
    // One shot has to exercise the row vocabulary, so the fixture carries a
    // genuinely flagged family alongside the two that have no substitute
    // detector and therefore can never be flagged.
    const states = families.map((row) => familyVerdict(row).state);
    expect(states).toContain('underused');
    expect(states).toContain('covered');
    expect(states.filter((state) => state === 'unmeasurable')).toHaveLength(2);
    // With a family flagged, the one-line summary correctly yields to the rows.
    expect(familiesSummary(families)).toBeNull();
  });

  it('GET /api/plugins/analytics/diagnostics — agents (DiagnosticsPayload)', () => {
    const data = parse(DiagnosticsPayload, '/api/plugins/analytics/diagnostics');
    expect(data.available).toBe(true);
    const window = describeWindow(data.event_count, data.events_per_hour);
    expect(window.capped).toBe(true);
    expect(window.spanHours).toBeGreaterThan(1);
    // The tool ranking is the second degenerate distribution on this page; it
    // has to span orders of magnitude or the log rail is untested.
    const tools = (data.by_mcp_tool ?? []).map((row) => Number(row['count'] ?? 0));
    expect(tools.length).toBeGreaterThanOrEqual(12);
    expect(Math.max(...tools) / Math.min(...tools)).toBeGreaterThan(100);
    // The window's own kinds must account for MORE than the categorized total,
    // which is what the reconciliation plate exists to explain.
    const kinds = (data.by_event_kind ?? []).reduce(
      (sum, row) => sum + Number(row['count'] ?? 0),
      0,
    );
    expect(kinds).toBe(data.event_count);
    // Newest-first, with real second-resolution stamps.
    const stamps = (data.recent_events ?? []).map((row) => row.timestamp);
    expect(stamps.length).toBe(20);
    expect([...stamps].sort((a, b) => b - a)).toEqual(stamps);
  });

  it('GET /api/automation/scheduler/status — automations (SchedulerStatusSchema)', () => {
    const data = parse(SchedulerStatusSchema, '/api/automation/scheduler/status');
    expect(data.paused).toBe(false);
    expect(data.status.length).toBeGreaterThan(0);
  });

  it('GET /api/automation/jobs — automations (JobsPayloadSchema)', () => {
    const data = parse(JobsPayloadSchema, '/api/automation/jobs');
    expect(data.jobs.length).toBeGreaterThanOrEqual(3);
    expect(data.count).toBe(data.jobs.length);
  });

  it('GET /api/automation/skills — automations (SkillsPayloadSchema)', () => {
    const data = parse(SkillsPayloadSchema, '/api/automation/skills');
    expect((data.skills ?? []).length).toBeGreaterThanOrEqual(3);
  });

  it('GET /api/automation/fact-proposals — automations (AnyObject)', () => {
    const data = parse(AnyObject, '/api/automation/fact-proposals');
    expect(Array.isArray(data['proposals'])).toBe(true);
  });

  it('GET /api/settings — settings (AnyObject)', () => {
    const data = parse(AnyObject, '/api/settings');
    expect(data['storage']).toBeDefined();
  });

  it('GET /api/capabilities — capabilities gateway (AnyObject)', () => {
    const data = parse(AnyObject, '/api/capabilities');
    expect(data['features']).toBeDefined();
  });

  it('GET /api/storage/telemetry — observatory envelope', () => {
    const env = parse(EnvelopeSchema(StorageTelemetryPayloadSchema), '/api/storage/telemetry');
    expect(env.payload.stores.length).toBeGreaterThan(0);
  });

  // Validated against Observatory's route contract, not the generated
  // `StorageFindingsPayloadSchema`. That generated shape describes a
  // `{ kinds, note }` payload, but `storage_findings_api.rs` serves a Doctor
  // findings envelope with `payload.kind_statuses` — so the old assertion held
  // the fixture to a shape no real response has, and NavRail's health dot,
  // which parses this route, could never resolve anything but `unknown`.
  // The replacement is strictly stronger: the full Doctor payload plus exactly
  // five named producers, each with a real source state and a reason.
  it('GET /api/storage/findings — observatory envelope', () => {
    const env = parse(
      EnvelopeSchema(ObservatoryStorageFindingsPayloadSchema),
      '/api/storage/findings',
    );
    expect(env.payload.kind_statuses.length).toBe(5);
    // Every producer names its source state and why, so an omitted read can
    // never be presented as a clean one.
    for (const status of env.payload.kind_statuses) {
      expect(status.reason.length).toBeGreaterThan(0);
    }
  });

  it('GET /api/doctor/findings — observatory doctor envelope (wire-true unsupported)', () => {
    // Wire-true default: no admitted Doctor reader → typed unsupported envelope
    // with no entries (doctor_findings_api.rs). Populated findings are avoided
    // because the DoctorInspector badge tokens fail light-theme contrast.
    const env = parse(EnvelopeSchema(DoctorFindingsPayloadSchema), '/api/doctor/findings');
    expect(env.domain_state).toBe('unsupported');
    expect(env.payload.entries.length).toBe(0);
    expect(env.payload.family_filter).toBeNull();
    expect(env.payload.known_families.length).toBe(7);
  });
});
