import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { ExecutionTopologyMetrics } from './ExecutionTopologyMetrics.tsx';

describe('the Observatory execution-topology metrics projection', () => {
  beforeEach(() => {
    useScope.getState().selectAllProjects();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        response({
          kind: 'success',
          value: {
            outcome: {
              outcome: 'evidence',
              value: { payload: topologyMetricsPayload() },
            },
          },
        }),
      ),
    );
  });

  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.unstubAllGlobals();
  });

  it('renders canonical cells and keeps a measured zero distinct from unavailable', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    render(
      <QueryClientProvider client={client}>
        <ExecutionTopologyMetrics />
      </QueryClientProvider>,
    );

    expect(await screen.findByRole('heading', { name: 'Execution-topology metrics' })).toBeTruthy();
    expect(
      await screen.findByText(/emitted 9 · delayed 2 · dropped ≥ 1 · sampled 4/),
    ).toBeTruthy();

    const measured = document.querySelector(
      '[data-dimension="work_execution_concurrency_width:concurrency phase active"]',
    );
    expect(measured?.getAttribute('data-dimension-available')).toBe('true');
    expect(measured?.querySelector('[data-cell="numeric"]')?.textContent).toBe('0');
    expect(measured?.textContent).toContain('ms');

    const unavailable = document.querySelector(
      '[data-dimension="work_duplicate_effects_total:duplicate outcome committed"]',
    );
    expect(unavailable?.getAttribute('data-dimension-available')).toBe('false');
    expect(unavailable?.querySelector('[data-cell="numeric"]')?.textContent).toBe('—');
    expect(unavailable?.textContent).toContain('support_floor_unmet');
    expect(unavailable?.textContent).not.toContain('0 effects');
  });
});

const HORIZON = {
  since_micros: 1_753_000_000_000_000,
  until_micros: 1_753_003_600_000_000,
};

function topologyMetricsPayload() {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: HORIZON,
    watermark: 'observability:topology:41',
    observed_at_micros: HORIZON.until_micros,
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
        value: 0,
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
        spec.value == null ? 0 : (spec.denominatorValue ?? 0),
        spec.value == null ? 0 : (spec.denominatorValue ?? 0),
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
      temporal: { horizon: HORIZON, baseline_watermark: null, delta: null },
      uncertainty: { lower: spec.value, upper: spec.value, reason: unavailable },
      calibration: null,
      unavailable_reason: unavailable,
    },
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

function response(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
