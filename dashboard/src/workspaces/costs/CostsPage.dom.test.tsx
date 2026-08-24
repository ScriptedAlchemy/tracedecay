import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createHash } from 'node:crypto';
import { FIXTURES, resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { CostsPage } from './CostsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('CostsPage truth claims', () => {
  it('separates provider usage from content sizing without inventing cache causality', async () => {
    const payload = savingsOverviewPayload();
    const sessions = payload['sessions'] as Record<string, unknown>;
    sessions['tokenized_messages'] = 300;
    sessions['estimated_messages'] = 700;
    sessions['messages'] = 1000;

    renderCosts(payload);

    expect(await screen.findByText('tokenized')).toBeTruthy();
    expect(screen.getByText(/provider-reported token breakdown/i)).toBeTruthy();
    expect(screen.getByText(/the wire does not report why/i)).toBeTruthy();
    expect(screen.queryByText(/they share one cache/i)).toBeNull();
  });

  it('reports an unreported message class as unreported, not as zero coverage', async () => {
    const payload = savingsOverviewPayload();
    const sessions = payload['sessions'] as Record<string, unknown>;
    // The block is available and holds messages, but the per-class counts and
    // the session count never came back. Coalescing them printed "0% of 41,204
    // messages carry token counts the provider reported" over four zeroes.
    sessions['messages'] = 41_204;
    sessions['tokenized_messages'] = null;
    sessions['estimated_messages'] = null;
    sessions['unknown_model_messages'] = null;
    sessions['session_count'] = null;

    renderCosts(payload);

    expect(await screen.findByText(/41,204 content messages/i)).toBeTruthy();
    expect(screen.getAllByText('not reported').length).toBe(3);
  });

  it('renders failed provider usage reads as unavailable instead of actual zero spend', async () => {
    const payload = savingsOverviewPayload();
    // `savings_api::read_failed_block` — the block reports the failure and
    // leaves every figure null rather than settling to zero.
    payload['provider_usage'] = {
      available: false,
      status: 'read_failed',
      error: 'failed to read priced provider usage',
      usage_event_count: null,
      total_cost_usd: null,
      total_tokens: null,
      cost_basis: null,
    };

    renderCosts(payload);

    expect(await screen.findByText(/priced provider usage read failed/i)).toBeTruthy();
    expect(screen.queryByText('$0.00')).toBeNull();
    expect(screen.queryByText(/0 across those usage events/i)).toBeNull();
  });

  it('renders a failed session aggregate separately from an empty ledger', async () => {
    const payload = savingsOverviewPayload();
    payload['sessions'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
      status: 'read_failed',
      error: 'failed to aggregate session tokens',
      scope: null,
      messages: null,
      provider_usage_events: null,
      tokenized_messages: null,
      estimated_messages: null,
      cost_basis: null,
      provider_actual: null,
      tokenized: null,
      estimated: null,
      session_count: null,
      model_count: null,
      unknown_model_messages: null,
      token_counting: null,
    };

    renderCosts(payload);

    expect(await screen.findAllByText(/session ledger read failed/i)).not.toHaveLength(0);
    expect(screen.queryByText(/reported no token breakdown/i)).toBeNull();
    expect(screen.queryByText(/reported no messages/i)).toBeNull();
  });

  it('renders an unmounted session source as typed unavailable, not as a read failure', async () => {
    const payload = savingsOverviewPayload();
    // The daemon's shape when the LCM store is simply not mounted: available
    // is false and there is no status and no error. Nothing failed, so the
    // page must not say "read failed".
    payload['sessions'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
      status: null,
      error: null,
      scope: null,
      messages: null,
      provider_usage_events: null,
      tokenized_messages: null,
      estimated_messages: null,
      cost_basis: null,
      provider_actual: null,
      tokenized: null,
      estimated: null,
      session_count: null,
      model_count: null,
      unknown_model_messages: null,
      token_counting: null,
    };

    renderCosts(payload);

    expect(await screen.findAllByText('Source unavailable')).not.toHaveLength(0);
    expect(
      screen.getAllByText(/the daemon reported this source unavailable without an error/i).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText(/session ledger read failed/i)).toBeNull();
  });

  it('keeps the canonical cost read alive when the savings ledger read fails', async () => {
    const payload = savingsOverviewPayload();
    // `savings_api::read_failed_block` shape: the block reports the failure and
    // leaves both summaries null rather than settling them to zero.
    payload['savings'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/savings.db',
      error: 'failed to read savings ledger',
      ledger: null,
      lifetime_counters: null,
      recording: null,
    };

    renderCosts(payload);

    // The failed payload read reports itself...
    expect(await screen.findAllByText(/Savings ledger read failed/i)).not.toHaveLength(0);
    // ...and the independent canonical projection still renders its own
    // measurements rather than being blanked by its neighbour.
    expect(await screen.findByText('provider tokens')).toBeTruthy();
    expect(screen.getByText('provider queue latency p50')).toBeTruthy();
    expect(screen.getAllByText('provider_latency_scope_unavailable').length).toBeGreaterThan(0);
  });

  it('discloses that project savings are a capped top slice', async () => {
    const payload = savingsOverviewPayload();
    const savings = payload['savings'] as Record<string, unknown>;
    const lifetime = savings['lifetime_counters'] as Record<string, unknown>;
    lifetime['project_total'] = 57;
    lifetime['projects_limit'] = 25;
    lifetime['projects_truncated'] = true;

    renderCosts(payload);

    expect(await screen.findByText(/top 25 of 57 projects/i)).toBeTruthy();
  });

  it('renders topology accounting from the canonical descriptor read without inventing a zero', async () => {
    const payload = savingsOverviewPayload();
    const topology = topologyMetricsPayload();
    expect(resolvedWorkScope().scope_digest).toBe(
      'sha256:e0f55213520e40ec75c565c7e153a8d6452d09ac4abac1a4a4312ca4abcd3bcb',
    );

    const fetch = renderCosts(payload, topology);

    expect(await screen.findByText('Execution topology accounting')).toBeTruthy();
    expect(await screen.findByText('work execution concurrency width')).toBeTruthy();
    expect(screen.getByText('27')).toBeTruthy();
    expect(screen.getByText('concurrency phase · active')).toBeTruthy();
    expect(screen.getByText('support_floor_unmet')).toBeTruthy();
    expect(screen.queryByText('0 effects')).toBeNull();
    expect(screen.getByText(/9 emitted · 2 delayed · 1 dropped · 4 sampled envelopes/i)).toBeTruthy();
    expect(
      fetch.mock.calls.some(
        ([input]) => new URL(String(input), 'http://localhost').pathname === '/api/work/topology-metrics',
      ),
    ).toBe(true);
  });

  it('keeps cost observations visible when the topology authority is unavailable', async () => {
    const payload = savingsOverviewPayload();

    renderCosts(payload, topologyMetricsPayload(), 503);

    expect(await screen.findByText('Execution topology accounting')).toBeTruthy();
    expect(await screen.findByText('Source unavailable')).toBeTruthy();
    expect(screen.getByText('provider tokens')).toBeTruthy();
    expect(screen.queryByText('0 effects')).toBeNull();
  });
});

/**
 * The page issues two independent reads. The savings overview is the payload
 * under test; every other route — the canonical `/api/costs` projection among
 * them — is served its own fixture, because a stub that answers every URL with
 * the savings body would make the canonical panel report a schema error and
 * hide whichever failure the case is actually about.
 */
function renderCosts(
  savingsOverview: unknown,
  topology = topologyMetricsPayload(),
  topologyStatus = 200,
) {
  const fetch = vi.fn(async (input: RequestInfo | URL) => {
    const pathname = new URL(String(input), 'http://localhost').pathname;
    const body =
      pathname === '/api/plugins/savings/overview'
        ? fixtureEnvelope(savingsOverview)
        : pathname === '/api/work/topology-metrics'
          ? workEnvelope(topology)
          : resolveFixture(pathname, '');
    return new Response(JSON.stringify(body), {
      status: pathname === '/api/work/topology-metrics' ? topologyStatus : 200,
    });
  });
  vi.stubGlobal(
    'fetch',
    fetch,
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <CostsPage />
    </QueryClientProvider>,
  );
  return fetch;
}

function savingsOverviewPayload(): Record<string, unknown> {
  const fixture = structuredClone(FIXTURES['/api/plugins/savings/overview']) as {
    payload: Record<string, unknown>;
  };
  return fixture.payload;
}

function workEnvelope(payload: unknown) {
  return {
    kind: 'success',
    value: {
      scope: resolvedWorkScope(),
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

/** Test equivalent of `ResolvedScope::compute_digest`: transparent domain ids
 * and the optional ref serialize as one canonical JSON tuple. */
function resolvedWorkScope() {
  const project_id = 'project.tracedecay';
  const repository_id = 'repository.tracedecay';
  const worktree_id = 'worktree.tracedecay';
  const reference = null;
  const canonical = JSON.stringify([
    'tracedecay.application.scope.v1',
    project_id,
    repository_id,
    worktree_id,
    reference,
  ]);
  return {
    project_id,
    repository_id,
    worktree_id,
    reference,
    scope_digest: `sha256:${createHash('sha256').update(canonical).digest('hex')}`,
  };
}

function topologyMetricsPayload() {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: 1_753_000_000_000_000, until_micros: 1_753_003_600_000_000 },
    watermark: 'observability:topology:41',
    observed_at_micros: 1_753_003_600_000_000,
    current: false,
    coverage: coverage(12, 9, 7, 2, 1, 'partial'),
    emission_coverage: { emitted: 9, delayed: 2, dropped: 1, sampled_events: 4 },
    github_stack_capability: {
      capability: null,
      standard_git_fallback_available: null,
      other_forge_fallback_available: null,
      coverage: coverage(null, 0, 0, 0, 1, 'unknown'),
      unavailable: 'no_eligible_evidence',
    },
    drill_anchors: [{ cursor: 'topology-observation-41' }],
    measurements: [
      topologyMeasurement({
        metric: 'work_execution_concurrency_width',
        value: 27_000,
        unit: 'microseconds',
        denominator: 'duration_weighted_topology_samples',
        denominatorValue: 12,
        dimensions: [{ dimension: 'concurrency_phase', value: 'active' }],
      }),
      topologyMeasurement({
        metric: 'work_duplicate_effects_total',
        value: null,
        unit: 'effects',
        denominator: 'observed_duplicate_effects',
        denominatorValue: null,
        unavailable: 'support_floor_unmet',
        dimensions: [{ dimension: 'duplicate_outcome', value: 'committed' }],
      }),
    ],
  };
}

function coverage(
  eligible: number | null,
  observed: number,
  completed: number,
  censored: number,
  unknown: number,
  state: string,
) {
  return { eligible, observed, completed, censored, unknown, excluded: 0, state };
}

function topologyMeasurement(spec: {
  metric: string;
  value: number | null;
  unit: string;
  denominator: string;
  denominatorValue: number | null;
  dimensions: unknown[];
  unavailable?: string;
}) {
  const unavailable = spec.unavailable ?? null;
  return {
    dimensions: spec.dimensions,
    unavailable,
    value: {
      descriptor_revision: 'execution-topology-metrics.v1',
      metric: spec.metric,
      value: spec.value,
      unit: spec.unit,
      denominator: spec.denominator,
      denominator_value: spec.denominatorValue,
      coverage: coverage(
        spec.denominatorValue,
        spec.value == null ? 0 : spec.denominatorValue ?? 0,
        spec.value == null ? 0 : spec.denominatorValue ?? 0,
        0,
        spec.value == null ? 1 : 0,
        spec.value == null ? 'unknown' : 'known',
      ),
      evidence_class: 'measurement',
      provenance: {
        source: 'observability_envelope',
        source_revision: 'observability-envelope.v1',
        projector_revision: 'execution-topology-projector.v1',
        watermark: 'observability:topology:41',
      },
      cohort: {
        descriptor_revision: `${spec.denominator}.v1`,
        eligible_population: spec.denominator,
      },
      temporal: {
        horizon: { since_micros: 1_753_000_000_000_000, until_micros: 1_753_003_600_000_000 },
        baseline_watermark: null,
        delta: null,
      },
      uncertainty: {
        lower: spec.value,
        upper: spec.value,
        reason: unavailable,
      },
      calibration: null,
      unavailable_reason: unavailable,
    },
  };
}
