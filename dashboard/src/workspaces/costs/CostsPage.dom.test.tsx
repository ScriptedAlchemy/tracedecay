import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { EChartsOption } from 'echarts';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createHash } from 'node:crypto';
import { FIXTURES, resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { CostsPage } from './CostsPage.tsx';

/** Every option the spend field handed its chart instance, in order. jsdom has
 * no canvas, so the registered ECharts build is replaced by a recorder; the
 * option itself is the claim under test, not the pixels. */
const appliedOptions: EChartsOption[] = [];

vi.mock('../../viz/chart/echarts.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../viz/chart/echarts.ts')>()),
  init: () => ({
    setOption: (option: EChartsOption) => {
      appliedOptions.push(option);
    },
    resize: () => {},
    dispose: () => {},
  }),
}));

afterEach(() => {
  vi.unstubAllGlobals();
  appliedOptions.length = 0;
});

type LineSeries = { name?: string; lineStyle?: { opacity?: number; type?: string }; data?: unknown[] };

function lastSeries(): LineSeries[] {
  const option = appliedOptions.at(-1);
  const series = option?.series;
  return (Array.isArray(series) ? series : series ? [series] : []) as LineSeries[];
}

describe('CostsPage provider spend attribution', () => {
  it('draws one priced-spend line per provider, distinguished by style as well as hue', async () => {
    renderCosts({ route: '/costs?range=7d' });
    await screen.findByRole('region', { name: 'Provider spend detail table' });
    await waitFor(() => expect(lastSeries().length).toBe(4));
    const series = lastSeries();
    expect(series.map((entry) => entry.name)).toEqual(['claude', 'codex', 'gemini', 'cursor']);
    expect(series.map((entry) => entry.lineStyle?.type)).toEqual(['solid', 'dashed', 'dotted', 'solid']);
    // Cursor is unpriced on every day: its line is all gaps, never a zero.
    expect(series[3]?.data?.every((value) => value === null)).toBe(true);
    expect(series[0]?.data?.some((value) => typeof value === 'number' && value > 0)).toBe(true);
    // Seven UTC day buckets for a seven-day window.
    const option = appliedOptions.at(-1) as { xAxis?: { data?: string[] } } | undefined;
    expect(option?.xAxis?.data?.length).toBe(7);
    expect(option?.xAxis?.data?.[0]).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  it('dims the other series while a provider is inspected and restores them after', async () => {
    const user = userEvent.setup();
    renderCosts({ route: '/costs?range=7d' });
    const legend = await screen.findByRole('group', { name: 'Provider legend and scope' });
    await waitFor(() => expect(lastSeries().length).toBe(4));

    await user.hover(within(legend).getByRole('button', { name: /^gemini,/ }));
    await waitFor(() => {
      const series = lastSeries();
      expect(series.find((entry) => entry.name === 'gemini')?.lineStyle?.opacity).toBe(1);
      expect(series.find((entry) => entry.name === 'claude')?.lineStyle?.opacity).toBe(0.25);
    });

    await user.unhover(within(legend).getByRole('button', { name: /^gemini,/ }));
    fireEvent.mouseLeave(legend);
    await waitFor(() => {
      expect(lastSeries().every((entry) => entry.lineStyle?.opacity === 1)).toBe(true);
    });
  });

  it('attributes priced spend by provider and discloses the coverage of every total', async () => {
    renderCosts();

    // The ledger renders the fixture's four providers with their pricing
    // classes kept apart: one priced, one partial, one unpriced, one priced.
    const table = await screen.findByRole('region', { name: 'Provider spend detail table' });
    const rows = within(table).getAllByRole('row').filter((row) => row.hasAttribute('data-provider'));
    expect(rows.map((row) => row.getAttribute('data-provider'))).toEqual([
      'claude',
      'codex',
      'gemini',
      'cursor',
    ]);
    expect(within(rows[1]!).getByText('partially priced')).toBeTruthy();
    expect(within(rows[3]!).getByText('unpriced')).toBeTruthy();
    // An unpriced provider prints no dollars, and no dollar cell says $0.00.
    expect(within(rows[3]!).getAllByText('—').length).toBeGreaterThan(0);
    expect(within(table).queryByText('$0.00')).toBeNull();

    // The total says what it includes.
    const coverage = screen.getByText(/The total includes .* observed usage events/);
    expect(coverage.textContent).toMatch(/pricing coverage/);
    expect(coverage.textContent).toMatch(/are unpriced and contribute no dollars/);

    // The authority panel files providers by class.
    const priced = document.querySelector('[data-pricing-class="priced"]');
    expect(priced?.textContent).toContain('claude');
    expect(priced?.textContent).toContain('gemini');
    const partial = document.querySelector('[data-pricing-class="partial"]');
    expect(partial?.textContent).toContain('codex');
    const unpriced = document.querySelector('[data-pricing-class="unpriced"]');
    expect(unpriced?.textContent).toContain('cursor');
  });

  it('inspects on hover without changing the query, and scopes on click', async () => {
    const user = userEvent.setup();
    const { location } = renderCosts();

    const legend = await screen.findByRole('group', { name: 'Provider legend and scope' });
    const codex = within(legend).getByRole('button', { name: /^codex,/ });

    await user.hover(codex);
    const inspector = document.querySelector('[data-costs-inspector]');
    expect(inspector?.getAttribute('data-costs-inspector')).toBe('inspected');
    expect(inspector?.getAttribute('data-provider')).toBe('codex');
    // Hover reveals exact model rows, priced and unpriced kept apart.
    const modelRows = document.querySelectorAll('[data-costs-model-rows] li');
    expect(modelRows.length).toBe(2);
    expect(document.querySelector('[data-model="gpt-5.3-codex-high"]')?.getAttribute('data-model-pricing')).toBe('unpriced');
    expect(document.querySelector('[data-model="gpt-5.3-codex-high"]')?.textContent).toMatch(
      /rate UNAVAILABLE · no applicable canonical rate/,
    );
    // Hover never touched the address.
    expect(location.current?.search).toBe('');
    expect(codex.getAttribute('aria-pressed')).toBe('false');

    await user.click(codex);
    expect(location.current?.search).toBe('?provider=codex');
    expect(codex.getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelector('[data-costs-selection]')?.getAttribute('data-costs-selection')).toBe('codex');
    // The ledger row carries the same selection: one selection, seen twice.
    const table = screen.getByRole('region', { name: 'Provider spend detail table' });
    expect(document.querySelector('tr[data-provider="codex"]')?.getAttribute('data-selected')).toBe('true');
    expect(document.querySelector('tr[data-provider="claude"]')?.getAttribute('data-selected')).toBeNull();

    // Clicking the scoped provider again clears the scope.
    await user.click(codex);
    expect(location.current?.search).toBe('');
  });

  it('traverses the legend with arrow keys, scopes with Enter, and clears with Escape', async () => {
    const user = userEvent.setup();
    const { location } = renderCosts();

    const legend = await screen.findByRole('group', { name: 'Provider legend and scope' });
    const buttons = within(legend).getAllByRole('button');
    // Roving tabindex: exactly one legend row is in the tab sequence.
    expect(buttons.filter((button) => button.tabIndex === 0)).toHaveLength(1);

    act(() => buttons[0]!.focus());
    await waitFor(() =>
      expect(document.querySelector('[data-costs-inspector]')?.getAttribute('data-provider')).toBe('claude'),
    );
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(buttons[1]);
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(buttons[3]);
    await user.keyboard('{Home}');
    expect(document.activeElement).toBe(buttons[0]);

    await user.keyboard('{Enter}');
    expect(location.current?.search).toBe('?provider=claude');
    await user.keyboard('{Escape}');
    expect(location.current?.search).toBe('');
  });

  it('reads the range from the address and asks the daemon for exactly that window', async () => {
    const user = userEvent.setup();
    const { location } = renderCosts({ route: '/costs?range=7d' });

    const tablist = await screen.findByRole('tablist', { name: 'Spend range' });
    expect(within(tablist).getByRole('tab', { name: '7 days' }).getAttribute('aria-selected')).toBe('true');
    const fetchMock = vi.mocked(fetch);
    const modelsCalls = () =>
      fetchMock.mock.calls
        .map(([input]) => new URL(String(input), 'http://localhost'))
        .filter((url) => url.pathname === '/api/plugins/savings/models')
        .map((url) => url.searchParams.get('range'));
    await screen.findByRole('region', { name: 'Provider spend detail table' });
    expect(modelsCalls()).toEqual(['7d']);

    await user.click(within(tablist).getByRole('tab', { name: 'Today' }));
    expect(location.current?.search).toBe('?range=today');
    await screen.findAllByText(/usage observed since 00:00 UTC today/);
    await waitFor(() => expect(modelsCalls()).toEqual(['7d', 'today']));

    // The all-time window is the address's default and is not written.
    await user.click(within(tablist).getByRole('tab', { name: 'All time' }));
    expect(location.current?.search).toBe('');
  });

  it('keeps a scoped provider the range does not carry visible as such', async () => {
    renderCosts({ route: '/costs?provider=mistral' });
    await screen.findByRole('region', { name: 'Provider spend detail table' });
    const selection = screen.getByText(/Costs \/ mistral/);
    expect(selection.textContent).toContain('not in this range');
    // Nothing can be scoped to it, so the inspector idles rather than inventing a row.
    expect(document.querySelector('[data-costs-inspector]')?.getAttribute('data-costs-inspector')).toBe('idle');
    expect(document.querySelector('tr[data-selected="true"]')).toBeNull();
  });

  it('exposes the drawn series as an exact table with gaps and unpriced buckets named', async () => {
    const user = userEvent.setup();
    renderCosts({ route: '/costs?range=today' });
    await user.click(await screen.findByText('series as table'));
    const table = screen.getByRole('region', { name: 'Priced spend series as a table' });
    const headers = within(table).getAllByRole('columnheader').map((cell) => cell.textContent);
    expect(headers).toEqual(['UTC day', 'claude', 'codex', 'gemini', 'cursor']);
    const cells = within(table).getAllByRole('cell').map((cell) => cell.textContent ?? '');
    // Cursor is unpriced every day: its bucket says so instead of drawing zero.
    expect(cells.some((text) => /^unpriced · [\d,]+ ev$/.test(text))).toBe(true);
    // Codex is partially priced: the drawn figure names what it excludes.
    expect(cells.some((text) => /^\$[\d,.]+ · [\d,]+ unpriced$/.test(text))).toBe(true);
  });

  it('renders a failed attribution read as an error, not an empty ledger', async () => {
    // `savings_api::models` answers an encoding fault with HTTP 500; the
    // payload ladder reports the status and nothing else is invented.
    renderCosts({
      modelsResponse: { status: 500, body: { status: 'contract_invalid', error: 'encode failed' } },
    });

    expect((await screen.findAllByText(/HTTP 500/)).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Error').length).toBeGreaterThan(0);
    expect(screen.queryByRole('region', { name: 'Provider spend detail table' })).toBeNull();
    expect(screen.queryByText('$0.00')).toBeNull();
    // The overview-fed panels still stand: pricing authority and savings windows.
    expect(await screen.findByText('bundled')).toBeTruthy();
    expect(screen.getByText(/saved · all time/i)).toBeTruthy();
  });

  it('renders a typed read_failed body as an error carrying the daemon sentence', async () => {
    // A 503 with the route's own `status`/`error` discriminant is the shape the
    // payload ladder admits as a typed refusal; the sentence reaches the reader.
    renderCosts({
      modelsResponse: {
        status: 503,
        body: {
          available: false,
          status: 'read_failed',
          error: 'session store locked by another writer',
          range: 'all',
          since: null,
          models: [],
          daily: [],
          provider_usage_coverage: null,
          provider_usage: {
            available: false,
            pricing_revision: null,
            undated_events: null,
            by_model: [],
            by_day: [],
            by_provider: [],
            by_provider_day: [],
          },
        },
      },
    });

    expect((await screen.findAllByText(/session store locked by another writer/)).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Error').length).toBeGreaterThan(0);
    expect(screen.queryByRole('region', { name: 'Provider spend detail table' })).toBeNull();
  });

  it('renders a partial provider-usage aggregate as partial, never as attributed zeros', async () => {
    const payload = structuredClone(resolveFixture('/api/plugins/savings/models', '?range=all')) as Record<string, unknown>;
    payload['provider_usage_coverage'] = 'partial';
    payload['provider_usage'] = {
      available: false,
      pricing_revision: null,
      undated_events: null,
      by_model: [],
      by_day: [],
      by_provider: [],
      by_provider_day: [],
    };
    renderCosts({ modelsResponse: { status: 200, body: payload } });

    expect(
      (await screen.findAllByText(/the provider usage aggregate is partial; exact per-provider attribution needs a complete aggregate/))
        .length,
    ).toBeGreaterThan(0);
    expect(screen.getAllByText('Partial').length).toBeGreaterThan(0);
    expect(screen.queryByText('$0.00')).toBeNull();
  });

  it('renders an unmounted session store as typed unavailable', async () => {
    renderCosts({
      modelsResponse: {
        status: 200,
        body: {
          available: false,
          status: null,
          error: null,
          range: 'all',
          since: 0,
          models: [],
          daily: [],
          provider_usage_coverage: null,
          provider_usage: {
            available: false,
            pricing_revision: null,
            undated_events: null,
            by_model: [],
            by_day: [],
            by_provider: [],
            by_provider_day: [],
          },
        },
      },
    });
    expect((await screen.findAllByText(/the session store is not mounted for this scope/)).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Source unavailable').length).toBeGreaterThan(0);
    expect(screen.queryByText('Error')).toBeNull();
  });

  it('keeps the attribution alive when the savings overview read fails, and vice versa', async () => {
    renderCosts({ overviewStatus: 503 });
    // Attribution still renders from its own read.
    expect(await screen.findByRole('region', { name: 'Provider spend detail table' })).toBeTruthy();
    // The overview-fed panels report their own failure rather than blanking.
    expect(screen.getAllByText('Error').length).toBeGreaterThan(0);
    expect(screen.getAllByText(/HTTP 503/).length).toBeGreaterThan(0);
  });

  it('renders failed and unmounted savings ledgers distinctly beside a healthy attribution', async () => {
    const failed = savingsOverviewPayload();
    failed['savings'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/savings.db',
      error: 'failed to read savings ledger',
      ledger: null,
      recording: null,
    };
    renderCosts({ overview: failed });
    expect(await screen.findByText(/savings ledger read failed: failed to read savings ledger/)).toBeTruthy();
    expect(screen.queryByText(/saved · all time/i)).toBeNull();
  });

  it('keeps the canonical cost and topology reads independent of the attribution', async () => {
    const { fetch: fetchMock } = renderCosts();
    expect(await screen.findByText('provider tokens')).toBeTruthy();
    expect(await screen.findByText('Execution topology accounting')).toBeTruthy();
    expect(await screen.findByText('work execution concurrency width')).toBeTruthy();
    expect(screen.getByText('support_floor_unmet')).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(
        ([input]) => new URL(String(input), 'http://localhost').pathname === '/api/work/topology-metrics',
      ),
    ).toBe(true);
  });

  it('reports the saved-token window matching the range as count only', async () => {
    renderCosts({ route: '/costs?range=30d' });
    const headline = await screen.findByText(/saved · 30d/i);
    expect(headline).toBeTruthy();
    expect(screen.getByText(/They are not priced/)).toBeTruthy();
    const active = document.querySelector('[data-saved-window="last_30d"]');
    expect(active?.querySelector('.text-accent')).toBeTruthy();
  });

  it('dims unrelated ledger rows while a provider is inspected from the table', async () => {
    renderCosts();
    const table = await screen.findByRole('region', { name: 'Provider spend detail table' });
    const claude = within(table).getByRole('button', { name: 'claude' });
    fireEvent.mouseEnter(claude.closest('tr')!);
    expect(document.querySelector('tr[data-provider="claude"]')?.getAttribute('data-inspected')).toBe('true');
    expect(document.querySelector('tr[data-provider="codex"]')?.className).toMatch(/opacity-50/);
    fireEvent.mouseLeave(table);
    expect(document.querySelector('tr[data-provider="claude"]')?.getAttribute('data-inspected')).toBeNull();
  });
});

/* ------------------------------------------------------------------------ */

function LocationProbe({ into }: { into: { current: ReturnType<typeof useLocation> | null } }) {
  into.current = useLocation();
  return null;
}

/**
 * The page issues independent reads. Each route is served its own fixture so
 * a failure injected into one cannot masquerade as a failure of another.
 */
function renderCosts(options: {
  route?: string;
  overview?: Record<string, unknown>;
  overviewStatus?: number;
  modelsResponse?: { status: number; body: unknown };
} = {}) {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), 'http://localhost');
    const pathname = url.pathname;
    if (pathname === '/api/plugins/savings/overview') {
      // A non-2xx overview is a proxy or daemon fault with no envelope behind
      // it; a 503 carrying a valid envelope would be read as that envelope.
      if (options.overviewStatus !== undefined && options.overviewStatus >= 400) {
        return new Response('{}', {
          status: options.overviewStatus,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(
        JSON.stringify(fixtureEnvelope(options.overview ?? savingsOverviewPayload())),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    if (pathname === '/api/plugins/savings/models') {
      const response = options.modelsResponse ?? {
        status: 200,
        body: resolveFixture(pathname, url.search),
      };
      return new Response(JSON.stringify(response.body), {
        status: response.status,
        headers: { 'content-type': 'application/json' },
      });
    }
    if (pathname === '/api/work/topology-metrics') {
      return new Response(JSON.stringify(workEnvelope(topologyMetricsPayload())), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }
    return new Response(JSON.stringify(resolveFixture(pathname, url.search)), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const location: { current: ReturnType<typeof useLocation> | null } = { current: null };
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[options.route ?? '/costs']}>
        <LocationProbe into={location} />
        <CostsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { fetch: fetchMock, location };
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
