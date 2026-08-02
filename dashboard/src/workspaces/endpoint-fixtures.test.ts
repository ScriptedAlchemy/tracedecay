/**
 * Parse gate for the visual-audit / interaction fixtures.
 *
 * Every `/api` route the 12 workspaces consume is served from
 * `stories/fixtures/data.ts` during the visual audit and MSW/DOM tests. This
 * suite asserts that each fixture payload parses against the exact zod schema
 * its consuming workspace validates it with — either a generated wire schema
 * from `contracts/generated.ts`, or (for the few routes Rust still answers with a
 * bare `Value`, whose pages read them through a module-local const) a faithful
 * mirror of that page's schema with a source citation. If a fixture drifts
 * from a contract, this fails.
 *
 * A route read by two workspaces gets ONE test against ONE schema. It used to
 * get two, because `/api/projects` was modelled twice under two export names,
 * and two tests asserting two hand-written copies of one body is how the copies
 * drifted apart without either test noticing.
 *
 * The per-endpoint density assertions encode the fixture spec (e.g. ≥25 facts,
 * ≥30 sessions across 3 providers, ≥250 graph search rows) so a fixture that
 * parses but renders an empty surface still fails the gate.
 */
import { describe, expect, it } from 'vitest';
import { z } from 'zod';
import type { ZodType } from 'zod';

import { resolveFixture } from '../../stories/fixtures/data.ts';
import { MultiRootCapabilityV1Schema } from '../contracts/generated.ts';
import { AnyObject } from '../data/query/legacy.ts';
import {
  AnalyticsOverviewPayloadV1Schema,
  AnalyticsUsageSummaryV1Schema,
  AutomationSchedulerStatusV1Schema,
  CodeIndexFreshnessPayloadV1Schema,
  CostsReadModelV1Schema,
  DoctorEvidenceStateV1Schema,
  DoctorFindingsPayloadV1Schema,
  DoctorStorageFindingKindV1Schema,
  DashboardEnvelopeV1Schema,
  GraphOverviewPayloadV1Schema,
  GraphSearchPayloadV1Schema,
  GraphSubgraphPayloadV1Schema,
  LcmSessionPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  MemoryStatusPayloadV1Schema,
  ObservatoryReadModelV1Schema,
  ProjectContextPayloadV1Schema,
  ProjectsPayloadV1Schema,
  SavingsOverviewPayloadV1Schema,
  SavingsSessionsPayloadV1Schema,
  SettingsPayloadV1Schema,
  StorageFindingsPayloadV1Schema,
  StorageTelemetryPayloadV1Schema,
} from '../contracts/generated.ts';
import {
  ANALYTICS_EVENT_LIMIT,
  describeWindow,
  familiesSummary,
  familyVerdict,
  summarizeDominance,
  type FamilyRow,
} from './agents/usage.ts';
import { columnIndexFor, indexedMass } from './brain/field.ts';
import { composeDeliveryField } from './delivery/field.ts';
import { composeWeave, summarizeChain } from './loom/weave.ts';
import { composeTrustDistribution } from './knowledge/trust.ts';

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

// `capabilities` (src/dashboard/mod.rs:1231-1274). No workspace decodes this
// route, so this suite is the only thing pinning its shape — which is how the
// fixture drifted without anyone noticing. `strict()` throughout, so a field
// added or renamed on either side fails here rather than silently diverging.
//
// `AutomationBackend`/`AutomationHostMode` are snake_case serde enums
// (src/automation/config.rs:11-18, :30-37), and `AgentBackendAvailability`
// marks `executable`/`reason` `skip_serializing_if = "Option::is_none"`
// (src/automation/backend.rs:434-442), so those keys are absent, never null.
const AutomationBackendSchema = z.enum(['disabled', 'codex_app_server', 'external_command']);
const CapabilitiesSchema = z
  .object({
    name: z.literal('tracedecay-dashboard'),
    version: z.string().min(1),
    mode: z.literal('standalone'),
    project_id: z.string().nullable(),
    project_root: z.string(),
    storage_mode: z.string(),
    store_root: z.string(),
    dashboard_root: z.string(),
    memory_db: z.string().nullable(),
    graph_db: z.string().nullable(),
    lcm_db: z.string().nullable(),
    lcm_scope: z.string().nullable(),
    features: z
      .object({
        memory: z.boolean(),
        lcm: z.boolean(),
        lcm_gc: z.boolean(),
        lcm_payload_health: z.boolean(),
        graph: z.boolean(),
        analytics: z.boolean(),
        code_diagnostics: z.boolean(),
        curation: z.boolean(),
        automation: z.boolean(),
        llm_curation: z.boolean(),
        managed_skills: z.boolean(),
        savings: z.boolean(),
        settings: z.boolean(),
        // Mirrors `multi_root_available` in `mod.rs::capabilities`, the
        // boolean beside the typed capability below. Both were missing from
        // this schema and from the fixture, so a `.strict()` mirror of the
        // handler was passing without the member the handler always sends.
        multi_root: z.boolean(),
      })
      .strict(),
    automation: z
      .object({
        enabled: z.boolean(),
        mode: z.enum(['disabled', 'delegated_host', 'standalone_backend']),
        backend: AutomationBackendSchema,
        host_mode: z.enum(['standalone', 'delegated_host']),
        availability: z
          .object({
            backend: AutomationBackendSchema,
            available: z.boolean(),
            executable: z.string().optional(),
            reason: z.string().optional(),
          })
          .strict(),
      })
      .strict(),
    dashboards: z.array(z.string()),
    // The generated `MultiRootCapabilityV1`, held to the generated schema
    // rather than restated here.
    multi_root: MultiRootCapabilityV1Schema,
  })
  .strict();

// SessionsPage.tsx: OverviewPayload. (LoomPage no longer reads this route —
// `/api/plugins/hermes-lcm/overview` 500s on the real profile, so the weave
// draws from `/api/plugins/savings/sessions` instead.)
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

// The three automation list routes still answer with a bare `Value`, so these
// stay mirrors. Their scheduler sibling does not: `automation_scheduler_api.rs`
// is typed now, the page reads the generated `AutomationSchedulerStatusV1Schema`
// directly, and the hand-written copy that used to sit here — every field
// `optional()`, no `pending_review` at all — was mirroring a page schema that no
// longer exists. It is gone rather than rewritten; a generated contract needs no
// mirror, and keeping one would be the drift this suite exists to catch.
//
// Every field below is required because the handler's `json!` literal writes it
// unconditionally, matching the page after 73d1b85be. That requiredness is the
// contract under test: an optional collection is what let a body missing its
// real key parse clean and render as a queue read and found empty.

// AutomationsPage.tsx: JobsPayloadSchema (automation_jobs_api.rs::list).
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

// AutomationsPage.tsx: SkillsPayloadSchema (automation_skills_api.rs::list).
// `metadata.id`, `.title` and `.state` are plain required members of
// `ManagedSkillMetadata`; the page reads them directly rather than through the
// fallback chain it used to carry.
const SkillsPayloadSchema = z
  .object({
    skills: z.array(
      z
        .object({
          metadata: z
            .object({ id: z.string(), title: z.string(), state: z.string() })
            .passthrough(),
        })
        .passthrough(),
    ),
    count: z.number(),
  })
  .passthrough();

// AutomationsPage.tsx: FactProposalsPayloadSchema
// (automation_fact_proposals_api.rs::list). `add_fact_request` is the one
// optional member — `skip_serializing_if = "Option::is_none"` on
// `FactProposalRecord` — so a record without one omits the key entirely.
const FactProposalsPayloadSchema = z
  .object({
    proposals: z.array(
      z
        .object({
          proposal_id: z.string(),
          state: z.string(),
          add_fact_request: z.object({ content: z.string() }).passthrough().optional(),
        })
        .passthrough(),
    ),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();

describe('endpoint fixtures parse against their consuming contracts', () => {
  // One route, one schema, one test. Brain and Delivery both read
  // `/api/projects` and used to assert it against two hand-written copies of
  // its body under two different export names, which is how the copies drifted
  // apart without anything noticing. Both surfaces' density requirements now
  // sit on the one generated `ProjectsPayloadV1Schema`.
  it('GET /api/projects — brain + delivery registry (ProjectsPayloadV1Schema)', () => {
    const data = parse(ProjectsPayloadV1Schema, '/api/projects');
    const tree = data.project_tree ?? [];
    expect(tree.length).toBeGreaterThanOrEqual(2);

    // Density spec for Brain's field: the surface composes projects into five
    // recency columns against a mass axis, so a fixture that lands everything
    // in one column or at one mass renders a picture that cannot be reviewed.
    const entries = tree.flatMap((group) => group.projects);
    expect(entries.length).toBeGreaterThanOrEqual(20);
    const columns = new Set(entries.map((e) => columnIndexFor(e.last_seen_at, Date.now() / 1000)));
    expect(columns.size).toBe(5);
    const masses = entries.map(indexedMass);
    expect(Math.max(...masses) / Math.max(Math.min(...masses), 1)).toBeGreaterThan(20);
    expect(entries.some((entry) => entry.kind === 'worktree')).toBe(true);

    // The delivery field has to compose into something readable, and the
    // bounds below encode the SHAPE of the real registry, not just its size.
    const field = composeDeliveryField(tree);
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

  it('GET /api/projects/{id} — scoped brain backbone (ProjectContextPayloadV1Schema)', () => {
    const data = parse(ProjectContextPayloadV1Schema, '/api/projects/tracedecay');
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

  it('GET /api/plugins/graph/subgraph — scoped brain field (GraphSubgraphPayloadV1Schema)', () => {
    const data = parse(
      GraphSubgraphPayloadV1Schema,
      '/api/projects/tracedecay/plugins/graph/subgraph',
    );
    expect((data.nodes ?? []).length).toBeGreaterThanOrEqual(20);
    expect((data.edges ?? []).length).toBeGreaterThanOrEqual(20);
  });

  it('GET /api/plugins/holographic/status — scoped brain (MemoryStatusPayloadV1Schema)', () => {
    const data = parse(MemoryStatusPayloadV1Schema, '/api/plugins/holographic/status');
    expect(data.exists).toBe(true);
    expect(data.memory?.fact_count).toBeGreaterThan(0);
    expect(data.memory?.entity_count).toBeGreaterThan(0);
  });

  it('GET /api/plugins/holographic/status — knowledge trust fallback (MemoryStatusPayloadV1Schema)', () => {
    const data = parse(MemoryStatusPayloadV1Schema, '/api/plugins/holographic/status');
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

  it('GET /api/plugins/analytics/overview — scoped brain (AnalyticsOverviewPayloadV1Schema)', () => {
    const data = parse(AnalyticsOverviewPayloadV1Schema, '/api/plugins/analytics/overview');
    expect(data.usage?.event_count).toBeGreaterThan(0);
    expect((data.usage?.by_category ?? []).length).toBeGreaterThanOrEqual(3);
  });

  it('GET /api/plugins/holographic/ — knowledge (MemoryOverviewPayloadV1Schema)', () => {
    const data = parse(MemoryOverviewPayloadV1Schema, '/api/plugins/holographic/');
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

  it('GET /api/plugins/savings/sessions — loom threads (SavingsSessionsPayloadV1Schema)', () => {
    const data = parse(SavingsSessionsPayloadV1Schema, '/api/plugins/savings/sessions');
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

  it('GET /api/plugins/hermes-lcm/session/{id} — loom chain (LcmSessionPayloadV1Schema)', () => {
    const data = parse(
      LcmSessionPayloadV1Schema,
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

  it('GET /api/plugins/graph/overview — code (GraphOverviewPayloadV1Schema)', () => {
    const data = parse(GraphOverviewPayloadV1Schema, '/api/plugins/graph/overview');
    const hubs = (data.top_connected ?? []) as Array<Record<string, unknown>>;
    // `graph_queries::top_connected_rows` is a `LIMIT 12` subquery selecting
    // exactly five columns, so the fixture must serve twelve rows and no more.
    // The route decodes those rows into `GraphNodeV1`, whose other fields are
    // `Option` and serialize as explicit nulls — so the five selected columns
    // must be the only ones carrying a value. Asserting that, rather than the
    // key set, keeps the original guarantee: the Code workspace cannot be
    // designed or audited against `qualified_name`, `signature` or
    // `start_line`, because this route never populates them.
    expect(hubs.length).toBe(12);
    const SELECTED = ['id', 'name', 'kind', 'file_path', 'degree'];
    for (const hub of hubs) {
      expect(typeof hub['degree']).toBe('number');
      const populated = Object.keys(hub)
        .filter((key) => hub[key] !== null)
        .sort();
      expect(populated).toEqual([...SELECTED].sort());
    }
    // Degrees arrive already ranked, highest first.
    const degrees = hubs.map((hub) => hub['degree'] as number);
    expect([...degrees].sort((a, b) => b - a)).toEqual(degrees);
    expect(data.totals.nodes).toBeGreaterThan(0);
  });

  it('GET /api/plugins/graph/search — code (GraphSearchPayloadV1Schema)', () => {
    const data = parse(GraphSearchPayloadV1Schema, '/api/plugins/graph/search', '?q=service');
    expect((data.results ?? []).length).toBeGreaterThanOrEqual(250);
  });

  it('GET /api/plugins/graph/search — explorer (ListPayload)', () => {
    const data = parse(ListPayload, '/api/plugins/graph/search', '?q=service');
    expect((data.results ?? []).length).toBeGreaterThanOrEqual(250);
  });

  it('GET /api/plugins/graph/subgraph — code unseeded (GraphSubgraphPayloadV1Schema)', () => {
    const data = parse(GraphSubgraphPayloadV1Schema, '/api/plugins/graph/subgraph');
    expect(data.seed_id).toBeNull();
    expect(data.mode).toBe('default');
    expect(data.nodes.length).toBeGreaterThanOrEqual(30);
    expect(data.edges.length).toBeGreaterThanOrEqual(40);
  });

  it('GET /api/plugins/graph/subgraph?node_id= — code seeded (GraphSubgraphPayloadV1Schema)', () => {
    const data = parse(GraphSubgraphPayloadV1Schema, '/api/plugins/graph/subgraph', '?node_id=sym-0');
    expect(data.seed_id).toBe('sym-0');
    expect(data.mode).toBe('seeded');
    expect(data.nodes.some((n) => n.id === 'sym-0')).toBe(true);
    expect(data.nodes.length).toBeLessThan(40);
  });

  it('GET /api/plugins/savings/overview — costs (SavingsOverviewPayloadV1Schema)', () => {
    const data = parse(SavingsOverviewPayloadV1Schema, '/api/plugins/savings/overview');
    expect(data.savings.available).toBe(true);
    expect(data.savings.ledger?.today.saved_tokens).toBeGreaterThan(0);
    expect(data.savings.ledger?.all_time.saved_tokens).toBeGreaterThan(0);
    expect((data.savings.lifetime_counters?.projects ?? []).length).toBeGreaterThanOrEqual(4);
    expect(data.turns.available).toBe(true);
  });

  it('GET /api/plugins/analytics/usage — agents (AnalyticsUsageSummaryV1Schema)', () => {
    const data = parse(AnalyticsUsageSummaryV1Schema, '/api/plugins/analytics/usage');
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

  it('GET /api/automation/scheduler/status — automations (generated contract)', () => {
    const data = parse(AutomationSchedulerStatusV1Schema, '/api/automation/scheduler/status');
    expect(data.paused).toBe(false);
    expect(data.status.length).toBeGreaterThan(0);
    // Both queues measured. This is the reading a mounted profile produces, and
    // it is also what the list panels consult before they will call themselves
    // empty — an `unreadable` fixture here would put every automation
    // screenshot into the deferred state instead of the populated one.
    expect(data.pending_review.fact_proposals.state).toBe('measured');
    expect(data.pending_review.skills.state).toBe('measured');
  });

  // The three list gates below assert the wire invariant each panel's
  // completeness check keys off — the handler derives `count` from the very
  // vector it serializes — rather than re-deriving the page's reconciliation.
  // A fixture that broke the invariant would render a partial-list notice in
  // every screenshot of a healthy surface.
  it('GET /api/automation/jobs — automations (JobsPayloadSchema)', () => {
    const data = parse(JobsPayloadSchema, '/api/automation/jobs');
    expect(data.jobs.length).toBeGreaterThanOrEqual(3);
    expect(data.count).toBe(data.jobs.length);
  });

  it('GET /api/automation/skills — automations (SkillsPayloadSchema)', () => {
    const data = parse(SkillsPayloadSchema, '/api/automation/skills');
    expect(data.skills.length).toBeGreaterThanOrEqual(3);
    expect(data.count).toBe(data.skills.length);
  });

  it('GET /api/automation/fact-proposals — automations (FactProposalsPayloadSchema)', () => {
    const data = parse(FactProposalsPayloadSchema, '/api/automation/fact-proposals');
    expect(data.proposals.length).toBeGreaterThanOrEqual(3);
    expect(data.count).toBe(data.proposals.length);
    // Strictly under the cap the route ran the query with, so the fixture is a
    // complete answer rather than a full page that may have more behind it.
    expect(data.count).toBeLessThan(data.limit);
  });

  // Validated against the generated `SettingsPayloadV1Schema` inside the
  // envelope the route actually answers with. Under the previous `AnyObject`
  // check the fixture went on serving a bare payload after `/api/settings`
  // started wrapping one, and SettingsPage read envelope metadata as
  // configuration without a single test noticing.
  it('GET /api/settings — settings envelope', () => {
    const env = parse(DashboardEnvelopeV1Schema(SettingsPayloadV1Schema), '/api/settings');
    expect(env.payload.storage.store_root.length).toBeGreaterThan(0);
    // The two write scopes are advertised independently, so the editor can
    // disable one without claiming the other is unauthorized too.
    expect(
      env.legal_actions
        .filter((action) => action.kind === 'request_apply')
        .map((action) => action.operation)
        .sort(),
    ).toEqual(['configuration_batch', 'user_settings_mutate']);
  });

  it('GET /api/capabilities — capabilities gateway', () => {
    const data = parse(CapabilitiesSchema, '/api/capabilities');
    // The route hard-codes the single canonical bundle. The previous
    // `expect(data['features']).toBeDefined()` accepted any object at all, and
    // under it the fixture went on advertising the five plugin dashboards that
    // were replaced by one embedded app.
    expect(data.dashboards).toEqual(['tracedecay']);
  });

  it('GET /api/storage/telemetry — observatory envelope', () => {
    const env = parse(DashboardEnvelopeV1Schema(StorageTelemetryPayloadV1Schema), '/api/storage/telemetry');
    expect(env.payload.stores.length).toBeGreaterThan(0);
  });

  // Validated against the generated `StorageFindingsPayloadV1Schema`, which is
  // what `storage_findings_api.rs` serves. The producer set is read off the
  // generated `DoctorStorageFindingKindV1` union rather than written down as a
  // count: a sixth kind added in Rust must make this fixture incomplete, not
  // make a passing assertion silently wrong about what it covered.
  it('GET /api/storage/findings — observatory envelope', () => {
    const env = parse(
      DashboardEnvelopeV1Schema(StorageFindingsPayloadV1Schema),
      '/api/storage/findings',
    );
    const knownKinds = DoctorStorageFindingKindV1Schema.options.map((option) => option.value);
    expect(new Set(env.payload.kind_statuses.map((status) => status.kind))).toEqual(
      new Set(knownKinds),
    );
    // Every producer names its source state and why, so an omitted read can
    // never be presented as a clean one.
    for (const status of env.payload.kind_statuses) {
      expect(status.reason.length).toBeGreaterThan(0);
    }
  });

  it('GET /api/doctor/findings — observatory doctor envelope (populated report)', () => {
    const env = parse(DashboardEnvelopeV1Schema(DoctorFindingsPayloadV1Schema), '/api/doctor/findings');
    expect(env.payload.family_filter).toBeNull();
    expect(env.payload.known_families.length).toBe(7);

    // Every evidence state exactly once. This fixture is what puts the
    // inspector's badges on screen for the axe scan, and it used to be empty
    // precisely so they would not be — so "one badge per state" is the density
    // spec, not a nicety. A ninth state added in Rust makes this fail rather
    // than silently going unscanned.
    const states = env.payload.entries.map((e) => e.finding.state);
    expect(new Set(states).size).toBe(states.length);
    expect(new Set(states)).toEqual(new Set(DoctorEvidenceStateV1Schema.options.map((o) => o.value)));

    // The kernel invariant the projection enforces: only a healthy finding may
    // claim complete coverage of a healthy result.
    for (const { finding } of env.payload.entries) {
      if (finding.state === 'healthy_complete_coverage') {
        expect(finding.coverage.completeness).toBe('complete');
      }
      expect(finding.coverage.statement.length).toBeGreaterThan(0);
      expect(finding.evidence.length).toBeGreaterThan(0);
    }

    // Families that answered nothing are reported as unavailable rather than
    // dropped, which is what renders the coverage-gap chips.
    const unavailable =
      env.payload.report_coverage?.families.filter(
        (family) => family.consultation.status === 'unavailable',
      ) ?? [];
    expect(unavailable.length).toBeGreaterThan(0);
    expect(env.payload.report_coverage?.completeness).toBe('partial');
    expect(env.domain_state).toBe('partial');

    // Every finding that references a remediation resolves to a descriptor, and
    // at least one is non-dispatchable — the owning surface supplies the change.
    const operations = new Set(env.payload.remediations.map((r) => r.operation));
    for (const { finding } of env.payload.entries) {
      if (finding.remediation) expect(operations.has(finding.remediation.owning_operation)).toBe(true);
    }
    expect(env.payload.remediations.some((r) => r.target === null)).toBe(true);
    expect(env.payload.remediations.some((r) => r.target !== null)).toBe(true);
  });

  // The two Plan 26 canonical read models. The density assertions here are
  // about the MIX, not the count: a fixture where every metric carries a value
  // parses fine and renders a surface that never shows the unavailable plate,
  // which is the exact state these workspaces exist to render honestly.
  it('GET /api/observatory — canonical observations envelope', () => {
    const env = parse(DashboardEnvelopeV1Schema(ObservatoryReadModelV1Schema), '/api/observatory');
    const metrics = env.payload.metrics;
    // Both producing sources present, so the surface renders two groups.
    const sources = new Set(metrics.map((metric) => metric.provenance.source));
    expect(sources).toEqual(new Set(['observability_envelope', 'feedback_observations']));
    // Event flow and latency both reach the surface.
    expect(metrics.some((metric) => metric.metric === 'observability_events')).toBe(true);
    expect(
      metrics.some(
        (metric) => metric.metric === 'feedback_latency_p95' && metric.unit === 'microseconds',
      ),
    ).toBe(true);
    // At least one completed and at least one genuinely unavailable, each with
    // the coverage state that goes with it.
    const unavailable = metrics.filter((metric) => metric.value == null);
    expect(unavailable.length).toBeGreaterThan(0);
    expect(metrics.filter((metric) => metric.value != null).length).toBeGreaterThan(0);
    for (const metric of unavailable) {
      expect(metric.unavailable_reason).toBeTruthy();
      expect(metric.coverage.state).toBe('unknown');
      expect(metric.coverage.eligible).toBeNull();
      expect(metric.denominator_value).toBeNull();
    }
  });

  it('GET /api/costs — canonical cost envelope with an unpriced ledger', () => {
    const env = parse(DashboardEnvelopeV1Schema(CostsReadModelV1Schema), '/api/costs');
    expect(env.payload.usage.length).toBeGreaterThan(0);
    // Prices are recorded at ingest; a read over unpriced turns must arrive as
    // a null cost with its reason, or the surface can never be shown refusing
    // to print $0.00.
    const cost = env.payload.estimated_cost.find((metric) => metric.metric === 'provider_cost');
    expect(cost?.value).toBeNull();
    expect(cost?.unavailable_reason).toBe('pricing_revision_unavailable');
    expect(env.payload.pricing_revision).toBeNull();
    // All-time window, which the wire carries as an unbounded lower edge.
    expect(env.payload.horizon.since_micros).toBe(0);
  });

  it('GET /api/code-index/freshness — branch-aware generation envelope', () => {
    const env = parse(
      DashboardEnvelopeV1Schema(CodeIndexFreshnessPayloadV1Schema),
      '/api/code-index/freshness',
    );
    const worktree = env.payload.worktrees[0];
    expect(worktree).toBeTruthy();
    // The branch-aware part: a sealed generation names the exact reference it
    // was sealed against, so a graph read on another branch is visibly stale.
    expect(worktree?.source_reference).toMatch(/^refs\//);
    expect(worktree?.latest_generation_id).toBeTruthy();
    expect(worktree?.snapshot_content_identity).toBeTruthy();
    expect(worktree?.staleness_state).toBe('fresh');
    expect(worktree?.coverage).toBe('complete');
  });
});
