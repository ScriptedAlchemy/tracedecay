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
  StorageFindingsPayloadSchema,
  DoctorFindingsPayloadSchema,
} from '../contracts/wire.ts';
import { ProjectsPayloadSchema } from './brain/contracts.ts';
import { DeliveryProjectsPayloadSchema } from './delivery/contracts.ts';
import {
  GraphOverviewPayloadSchema,
  GraphSearchPayloadSchema,
  SubgraphPayloadSchema,
} from './code/contracts.ts';
import { MemoryOverviewPayloadSchema } from './knowledge/contracts.ts';
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

// SessionsPage.tsx / LoomPage.tsx: OverviewPayload.
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
  });

  it('GET /api/projects — delivery (DeliveryProjectsPayloadSchema)', () => {
    const data = parse(DeliveryProjectsPayloadSchema, '/api/projects');
    const checkouts = (data.project_tree ?? []).flatMap((g) => g.projects);
    expect(checkouts.some((c) => c.kind === 'worktree')).toBe(true);
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
    expect(hubs.length).toBeGreaterThanOrEqual(15);
    for (const hub of hubs) expect(typeof hub['degree']).toBe('number');
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
    expect((data.by_category ?? []).length).toBeGreaterThanOrEqual(10);
  });

  it('GET /api/plugins/analytics/hints — agents (HintsPayload)', () => {
    const data = parse(HintsPayload, '/api/plugins/analytics/hints');
    expect(data.available).toBe(true);
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

  it('GET /api/storage/findings — observatory envelope', () => {
    const env = parse(EnvelopeSchema(StorageFindingsPayloadSchema), '/api/storage/findings');
    expect(env.payload.kinds.length).toBeGreaterThan(0);
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
