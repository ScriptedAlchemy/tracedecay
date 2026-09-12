import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { PerformanceComparisons } from './PerformanceComparisons.tsx';

/**
 * The Plan 26 `performance-comparisons` view.
 *
 * One rule dominates: exactly one disposition is asserted, and
 * `insufficient_evidence` is that disposition in its own right — not a quieter
 * `reject`. The DOM has to make that distinguishable without reading prose, so
 * the reached disposition carries its own attribute and the two it is not are
 * marked as not reached.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory performance comparisons', () => {

  it('renders the server disposition without re-deciding it in the browser', async () => {
    renderComparisons();

    const reached = await waitForDisposition();
    expect(reached.getAttribute('data-comparison-disposition')).toBe('insufficient_evidence');
    expect(reached.textContent).toContain('comparison_evidence_not_recorded');
    // The chip is the taxonomy's `unknown`, never `denied`.
    expect(reached.querySelector('[data-state="unknown"]')).toBeTruthy();
    expect(reached.querySelector('[data-state="denied"]')).toBeNull();
  });

  it('exposes the two evidence bands as named regions with list semantics', async () => {
    renderComparisons();

    await screen.findByText('baseline build');
    expect(
      screen.getByRole('region', { name: 'Baseline and candidate evidence dimensions' }),
    ).toBeTruthy();
    expect(screen.getByRole('region', { name: 'Evaluation results dimensions' })).toBeTruthy();
    expect(screen.getAllByRole('listitem').length).toBe(14);
  });

  it('reports a daemon that never answered as offline rather than as a reject', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    renderWith();

    expect(await screen.findByText('Offline')).toBeTruthy();
    expect(document.querySelector('[data-comparison-disposition]')).toBeNull();
  });
});

async function waitForDisposition(): Promise<Element> {
  await screen.findByRole('region', { name: 'Comparison disposition' });
  const reached = document.querySelector('[data-comparison-disposition]');
  if (reached == null) throw new Error('no disposition rendered');
  return reached;
}

function renderComparisons() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(envelope()), { status: 200 })),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <PerformanceComparisons />
    </QueryClientProvider>,
  );
}

function envelope() {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 0,
      examined: 0,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 0,
      unit: 'comparisons',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:4821' },
    domain_state: 'ready',
    legal_actions: [],
    payload: {
      authorized_scope_ref: 'project.tracedecay',
      horizon: { since_micros: 0, until_micros: NOW_MICROS },
      watermark: 'analytics:4821',
      observed_at_micros: NOW_MICROS,
      current: true,
      metrics: comparisonMetrics(),
      analytics_mode: {
        current: null,
        transition_watermark: null,
        coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
        unavailable_reason: 'not_observed',
      },
      comparison: {
        baseline_build: null, candidate_build: null, workload: null, corpus: null,
        environment: null, oracle: null, configuration: null, platform: null,
        rollback_profile: null, eligible_outcomes: null, paired_outcomes: null,
        regression_observed: null, disposition: 'insufficient_evidence',
        coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
        unavailable_reason: 'comparison_evidence_not_recorded',
      },
      rejected_arguments: {
        coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
        projector_revision: 'observatory-rejected-argument-projector.v1',
        watermark: 'analytics:4821',
        eligible_attempts: null,
        rejected_total: null,
        rejection_rate: null,
        redacted_name_count: 0,
        groups: [],
        unavailable_reason: 'rejected_argument_observations_not_recorded',
      },
    },
  };
}

function comparisonMetrics() {
  return [
    'comparison_baseline_build', 'comparison_candidate_build', 'comparison_workload_corpus',
    'comparison_environment_platform', 'comparison_oracle', 'comparison_rollback_profile',
    'comparison_outcome_counts', 'comparison_stratum_support', 'comparison_intervals',
    'comparison_calibration', 'comparison_risk_coverage', 'comparison_flaky_indeterminate',
    'comparison_deviations', 'comparison_paired_outcomes',
  ].map((metric) => ({
    descriptor_revision: 'performance-comparisons.v1', metric, value: null, unit: 'events',
    denominator: 'eligible_comparison_outcomes', denominator_value: null,
    coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
    evidence_class: 'measurement',
    provenance: { source: 'observability_envelope', source_revision: 'observability-envelope.v1', projector_revision: 'observatory-plan26-projector.v1', watermark: 'analytics:4821' },
    cohort: { descriptor_revision: 'eligible_comparison_outcomes.v1', eligible_population: 'eligible_comparison_outcomes' },
    temporal: { horizon: { since_micros: 0, until_micros: NOW_MICROS }, baseline_watermark: null, delta: null },
    uncertainty: { lower: null, upper: null, reason: 'comparison_evidence_not_recorded' },
    calibration: null, unavailable_reason: 'comparison_evidence_not_recorded',
  }));
}
