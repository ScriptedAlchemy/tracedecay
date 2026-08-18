import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type {
  DashboardEnvelopeV1,
  ObservatoryReadModelV1,
  RejectedArgumentAnalyticsV1,
} from '../../contracts/generated.ts';
import type { ObservatoryAccountingReads } from './accountingReads.ts';
import { RejectedArguments } from './RejectedArguments.tsx';

const NOW_MICROS = 1_753_003_600_000_000;

describe('Observatory rejected arguments', () => {
  it('renders populated groups as labelled counts without fabricating a rate', () => {
    renderCard(
      analytics({
        coverage: knownCoverage(3),
        projector_revision: 'observatory-rejected-argument-projector.v1',
        watermark: 'analytics:9',
        eligible_attempts: null,
        rejected_total: 3,
        rejection_rate: null,
        redacted_name_count: 0,
        groups: [
          {
            surface: 'cli',
            operation: 'feedback_diagnostics',
            argument: 'request_body',
            error_class: 'invalid_shape',
            count: 2,
            rate: null,
          },
          {
            surface: 'mcp',
            operation: 'feedback_list',
            argument: 'operation',
            error_class: 'unauthorized',
            count: 1,
            rate: null,
          },
        ],
        unavailable_reason: null,
      }),
    );

    expect(screen.getByRole('rowheader', { name: 'cli' })).toBeTruthy();
    const cli = screen.getByRole('rowheader', { name: 'cli' }).closest('tr');
    expect(cli?.textContent).toContain('2');
    expect(cli?.textContent).toContain('feedback_diagnostics');
    expect(cli?.textContent).toContain('invalid_shape');
    expect(document.querySelector('[data-rejected-arguments="populated"]')).toBeTruthy();
    expect(screen.getAllByText('—').length).toBeGreaterThan(0);
    expect(screen.queryByText(/%/)).toBeNull();
  });

  it('renders a measured empty window instead of an unavailable chip', () => {
    renderCard(
      analytics({
        coverage: knownCoverage(0),
        projector_revision: 'observatory-rejected-argument-projector.v1',
        watermark: 'analytics:empty',
        eligible_attempts: null,
        rejected_total: 0,
        rejection_rate: null,
        redacted_name_count: 0,
        groups: [],
        unavailable_reason: null,
      }),
    );

    expect(screen.getByText(/no rejected-argument observations in this window/i)).toBeTruthy();
    expect(document.querySelector('[data-rejected-arguments="empty"]')).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('renders the unavailable reason when the family was not recorded', () => {
    renderCard(
      analytics({
        coverage: {
          state: 'unknown',
          eligible: null,
          observed: 0,
          completed: 0,
          censored: 0,
          unknown: 1,
          excluded: 0,
        },
        projector_revision: 'observatory-rejected-argument-projector.v1',
        watermark: 'analytics:unavailable',
        eligible_attempts: null,
        rejected_total: null,
        rejection_rate: null,
        redacted_name_count: 0,
        groups: [],
        unavailable_reason: 'rejected_argument_observations_not_recorded',
      }),
    );

    expect(screen.getByText(/rejected_argument_observations_not_recorded/)).toBeTruthy();
    expect(document.querySelector('[data-rejected-arguments="unavailable"]')).toBeTruthy();
    expect(screen.queryByText(/no rejected-argument observations in this window/i)).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
  });
});

function knownCoverage(observed: number) {
  return {
    state: 'known' as const,
    eligible: null,
    observed,
    completed: observed,
    censored: 0,
    unknown: 0,
    excluded: 0,
  };
}

function analytics(rejected: RejectedArgumentAnalyticsV1): ObservatoryReadModelV1 {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: NOW_MICROS - 30 * 86_400_000_000, until_micros: NOW_MICROS },
    watermark: 'analytics:9',
    observed_at_micros: NOW_MICROS,
    current: true,
    metrics: [],
    analytics_mode: {
      current: null,
      transition_watermark: null,
      coverage: knownCoverage(0),
      unavailable_reason: 'not_observed',
    },
    comparison: {
      baseline_build: null,
      candidate_build: null,
      workload: null,
      corpus: null,
      environment: null,
      oracle: null,
      configuration: null,
      platform: null,
      rollback_profile: null,
      eligible_outcomes: null,
      paired_outcomes: null,
      regression_observed: null,
      disposition: 'insufficient_evidence',
      coverage: knownCoverage(0),
      unavailable_reason: 'not_observed',
    },
    rejected_arguments: rejected,
  };
}

function renderCard(model: ObservatoryReadModelV1) {
  const reads: ObservatoryAccountingReads = {
    observatory: {
      result: {
        outcome: 'envelope',
        envelope: envelope(model),
      },
      pending: false,
      refreshing: false,
      refresh: () => undefined,
    },
    diagnostics: {
      result: undefined,
      pending: false,
      refreshing: false,
      refresh: () => undefined,
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <RejectedArguments reads={reads} />
    </QueryClientProvider>,
  );
}

function envelope(payload: ObservatoryReadModelV1): DashboardEnvelopeV1<ObservatoryReadModelV1> {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:9' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 1,
      examined: 1,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 1,
      unit: 'metrics',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:9' },
    domain_state: 'ready',
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' }],
    payload,
  };
}
