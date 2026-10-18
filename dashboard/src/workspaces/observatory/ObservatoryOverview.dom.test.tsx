import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { EVIDENCE_SOURCES } from './evidence.ts';
import { ObservatoryPage } from './ObservatoryPage.tsx';

/**
 * The system-evidence overview: eleven authorities on one time context, each
 * with its own typed state; hover previews, click selects, and no aggregate
 * health is ever spoken.
 */

const NOW = 1_753_003_600_000_000;
const EARLIER = NOW - 3_600_000_000;

describe('Observatory system evidence overview', () => {
  beforeEach(() => {
    useScope.getState().selectAllProjects();
  });

  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.unstubAllGlobals();
  });

  it('renders every authority as a typed absence when nothing is reachable, and no health', async () => {
    stubRoutes({});
    renderObservatory('/observatory');

    await waitFor(() => {
      for (const id of EVIDENCE_SOURCES.filter((source) => source !== 'observations')) {
        const panel = document.querySelector(`[data-evidence-panel="${id}"]`);
        expect(panel, id).toBeTruthy();
        expect(panel?.getAttribute('data-evidence-state'), id).toMatch(/^(failed|unavailable)$/);
      }
    });
    // A grid of failed reads draws nothing that could pass for a reading.
    expect(document.querySelectorAll('[data-evidence-blocked]').length).toBeGreaterThan(0);
    expect(document.querySelector('[data-evidence-state="measured"]')).toBeNull();
    expect(document.body.textContent).not.toMatch(/nominal|all systems|healthy/i);
    // Nothing is selected, so the inspector says so and no exact evidence is mounted.
    const inspector = screen.getByLabelText('Evidence inspector');
    expect(inspector.getAttribute('data-inspector-mode')).toBe('none');
    expect(inspector.querySelector('[data-inspector-empty]')).toBeTruthy();
    expect(document.querySelector('[data-exact-evidence]')).toBeNull();
    // No authority published an observation time, so the rail places nothing.
    const timeline = screen.getByLabelText('Canonical observations timeline');
    expect(timeline.querySelector('[data-timeline-extent]')?.getAttribute('data-timeline-extent')).toBe('none');
    expect(timeline.textContent).toContain('range filter unavailable');
  });

  it('reports each authority in its own state and places each read on the shared rail', async () => {
    stubRoutes(mixedRoutes());
    renderObservatory('/observatory');

    await waitFor(() => {
      expect(panelState('telemetry')).toBe('measured');
      expect(panelState('findings')).toBe('partial');
      expect(panelState('pipeline')).toBe('building');
      expect(panelState('hooks')).toBe('unavailable');
    });
    // The other authorities still failed, and a measured neighbour did not help them.
    expect(panelState('adoption')).toBe('failed');
    expect(panelState('budgets')).toBe('failed');

    // Coverage is printed only where both figures exist.
    const telemetry = document.querySelector('[data-evidence-panel="telemetry"]');
    expect(telemetry?.querySelector('[data-evidence-coverage]')?.textContent).toBe('coverage 100%');
    expect(telemetry?.querySelector('[data-evidence-as-of]')?.textContent).toBe(
      `as of ${new Date(NOW / 1000).toISOString()}`,
    );
    const findings = document.querySelector('[data-evidence-panel="findings"]');
    expect(findings?.querySelector('[data-evidence-coverage]')?.textContent).toBe('coverage 50%');

    // Four authorities answered with an observation time, the unavailable
    // hint source included, since its envelope still says when the daemon
    // looked. jsdom gives the rail no width, so all four fold into one cluster
    // that states its count; opening it lists each read in its own state. The
    // failed reads have no time and are listed as absences instead of placed.
    const timeline = screen.getByLabelText('Canonical observations timeline');
    expect(timeline.getAttribute('data-timeline-marks')).toBe('4');
    const cluster = timeline.querySelector<HTMLElement>('[data-timeline-cluster]');
    expect(cluster?.getAttribute('data-timeline-cluster-size')).toBe('4');
    expect(cluster?.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(cluster!);
    const marks = Array.from(document.querySelectorAll('[data-timeline-mark]'));
    expect(marks.map((mark) => mark.getAttribute('data-timeline-mark')).sort()).toEqual(
      ['findings', 'hooks', 'pipeline', 'telemetry'],
    );
    expect(
      document.querySelector('[data-timeline-mark="hooks"]')?.getAttribute('data-evidence-state'),
    ).toBe('unavailable');
    expect(
      document.querySelector('[data-timeline-mark="pipeline"]')?.getAttribute('data-evidence-state'),
    ).toBe('building');
    expect(screen.getByLabelText('Canonical observations timeline').textContent).toContain(
      'no observation time published:',
    );
  });

  it('previews on hover without selecting, then selects on click and opens the exact evidence', async () => {
    stubRoutes(mixedRoutes());
    const user = userEvent.setup();
    renderObservatory('/observatory');
    await waitFor(() => expect(panelState('telemetry')).toBe('measured'));

    const inspector = screen.getByLabelText('Evidence inspector');
    const panel = document.querySelector<HTMLElement>('[data-evidence-panel="telemetry"]')!;
    await user.hover(panel);
    expect(inspector.getAttribute('data-inspector-mode')).toBe('preview');
    expect(inspector.getAttribute('data-inspector-source')).toBe('telemetry');
    expect(inspector.querySelector('[data-inspector-fact="source"]')?.textContent).toContain(
      '/api/storage/telemetry',
    );
    // Hovering changed neither the address nor what is mounted.
    expect(location()).toBe('');
    expect(document.querySelector('[data-exact-evidence]')).toBeNull();

    await user.unhover(panel);
    expect(inspector.getAttribute('data-inspector-mode')).toBe('none');

    await user.click(screen.getByRole('button', { name: 'Storage telemetry' }));
    expect(location()).toContain('inspect=telemetry');
    expect(inspector.getAttribute('data-inspector-mode')).toBe('selected');
    expect(panel.getAttribute('data-evidence-selected')).toBe('true');
    const exact = await screen.findByLabelText('Exact evidence · Storage telemetry');
    expect(exact.textContent).toContain('graph');
    expect(screen.getByRole('heading', { name: 'Store telemetry' })).toBeTruthy();

    // The inspector states what the contracts do not carry, and offers no recovery.
    expect(inspector.querySelector('[data-inspector-fact="severity"]')?.textContent).toContain(
      'not published',
    );
    expect(inspector.querySelector('[data-recovery="unavailable"]')).toBeTruthy();
    expect(inspector.querySelector('[data-operation="use-case.dashboard.storage.telemetry.refresh"]')).toBeTruthy();
    expect(inspector.querySelector('[data-cross-link="settings"]')).toBeTruthy();
  });

  it('traverses panels with arrow keys, selects with Enter, and returns to the selection on Escape', async () => {
    stubRoutes(mixedRoutes());
    const user = userEvent.setup();
    renderObservatory('/observatory');
    await waitFor(() => expect(panelState('telemetry')).toBe('measured'));

    const doctor = screen.getByRole('button', { name: 'Doctor inspection' });
    const adoption = screen.getByRole('button', { name: 'Adoption coverage' });
    doctor.focus();
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(adoption);
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Storage findings' }));
    await user.keyboard('{Home}');
    expect(document.activeElement).toBe(doctor);

    await user.keyboard('{Enter}');
    expect(location()).toContain('inspect=doctor');

    // Moving focus to another panel previews it; Escape drops the preview and
    // puts focus back on the selected panel.
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(adoption);
    const inspector = screen.getByLabelText('Evidence inspector');
    expect(inspector.getAttribute('data-inspector-mode')).toBe('preview');
    expect(inspector.getAttribute('data-inspector-source')).toBe('adoption');
    await user.keyboard('{Escape}');
    expect(inspector.getAttribute('data-inspector-mode')).toBe('selected');
    expect(inspector.getAttribute('data-inspector-source')).toBe('doctor');
    expect(document.activeElement).toBe(doctor);
  });

  it('narrows the inspector to a chosen finding and keeps its recovery unavailable', async () => {
    stubRoutes(mixedRoutes());
    const user = userEvent.setup();
    renderObservatory('/observatory');
    await waitFor(() => expect(panelState('findings')).toBe('partial'));

    await user.click(document.querySelector<HTMLElement>('[data-evidence-finding="0"]')!);
    expect(location()).toContain('inspect=findings');
    expect(location()).toContain('finding=0');
    const inspector = screen.getByLabelText('Evidence inspector');
    const finding = inspector.querySelector('[data-inspector-finding="0"]');
    expect(finding?.textContent).toContain('one store over its soft budget');
    expect(finding?.getAttribute('data-inspector-finding')).toBe('0');
    expect(finding?.querySelector('[data-evidence-state="degraded"]')).toBeTruthy();
    expect(inspector.querySelector('[data-recovery="unavailable"]')).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Clear selected finding' }));
    expect(location()).not.toContain('finding=');
    expect(inspector.querySelector('[data-inspector-finding]')).toBeNull();
  });

  it('selects a source from its timeline mark', async () => {
    stubRoutes(mixedRoutes());
    renderObservatory('/observatory');
    await waitFor(() => expect(document.querySelector('[data-timeline-cluster]')).toBeTruthy());

    fireEvent.click(document.querySelector('[data-timeline-cluster]')!);
    fireEvent.click(document.querySelector('[data-timeline-mark="pipeline"]')!);
    expect(location()).toContain('inspect=pipeline');
    expect(await screen.findByLabelText('Exact evidence · Code-index pipeline')).toBeTruthy();
    expect(
      document.querySelector('[data-timeline-mark="pipeline"]')?.getAttribute('aria-pressed'),
    ).toBe('true');
  });

  it('drops a finding index the served report no longer carries', async () => {
    stubRoutes(mixedRoutes());
    renderObservatory('/observatory?inspect=findings&finding=7');
    await waitFor(() => expect(panelState('findings')).toBe('partial'));
    await waitFor(() => expect(location()).not.toContain('finding='));
    expect(screen.getByLabelText('Evidence inspector').querySelector('[data-inspector-finding]')).toBeNull();
  });
});

function panelState(id: string): string | null {
  return document.querySelector(`[data-evidence-panel="${id}"]`)?.getAttribute('data-evidence-state') ?? null;
}

function location(): string {
  return document.querySelector('[data-location]')?.textContent ?? '';
}

function LocationProbe() {
  const { search } = useLocation();
  return <output data-location>{search}</output>;
}

function renderObservatory(entry: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <QueryClientProvider client={client}>
        <ObservatoryPage />
        <LocationProbe />
      </QueryClientProvider>
    </MemoryRouter>,
  );
}

/** Routes not named answer 503 with an undecodable body: a transport failure
 * the page must render as `failed`, never as empty success. */
function stubRoutes(routes: Record<string, () => unknown>) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      const route = (url.pathname + url.search).replace(/^\/api\/projects\/[^/]+/, '/api');
      const body = routes[route];
      if (body) return json(body());
      return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
    }),
  );
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function mixedRoutes(): Record<string, () => unknown> {
  return {
    '/api/storage/telemetry': () =>
      envelope(telemetryPayload(), {
        domain_state: 'ready',
        coverage: coverage('complete', 1, 1, 'stores'),
        legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.storage.telemetry.refresh' }],
      }),
    '/api/doctor/findings?family=storage': () =>
      envelope(findingsPayload(), {
        domain_state: 'partial',
        coverage: coverage('partial', 1, 2, 'producers'),
        freshness: { state: 'fresh', observed_at_micros: EARLIER, watermark: null },
        time: { valid_time_micros: null, observation_time_micros: EARLIER },
      }),
    '/api/code-index/freshness': () =>
      envelope(freshnessPayload(), {
        domain_state: 'loading',
        coverage: coverage('partial', 0, 1, 'mounted_worktree'),
        freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
      }),
    '/api/plugins/analytics/hints': () =>
      envelope(
        { available: false, by_category: [], error: 'hint store unreadable', source: 'hook_analytics.jsonl' },
        { freshness: { state: 'absent', observed_at_micros: null, watermark: null } },
      ),
  };
}

function coverage(
  completeness: 'complete' | 'partial' | 'unknown',
  examined: number | null,
  denominator: number | null,
  unit: string,
) {
  return {
    completeness,
    eligible: denominator,
    examined,
    matched: examined,
    excluded: 0,
    omitted: denominator != null && examined != null ? denominator - examined : null,
    unknown: 0,
    denominator,
    unit,
    omission_reasons: [],
  };
}

function envelope(payload: unknown, overrides: Record<string, unknown> = {}) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: coverage('complete', 1, 1, 'reads'),
    freshness: { state: 'fresh', observed_at_micros: NOW, watermark: null },
    domain_state: 'ready',
    legal_actions: [],
    payload,
    ...overrides,
  };
}

function telemetryPayload() {
  return {
    budget_note: 'soft budgets come from sync.retention.v1',
    growth_note: 'growth requires an execution-owned sampler',
    table_growth_threshold: { absolute_bytes: 1_048_576, relative_floor_bytes: 262_144, relative_percent: 10 },
    table_growth_coverage: coverage('unknown', null, null, 'store_table_growth_reads'),
    stores: [
      {
        store: 'graph',
        role: 'graph',
        roles: ['graph'],
        path: '/store/graph.db',
        read: {
          kind: 'observed',
          sample: {
            store: 'graph',
            observed_at: NOW,
            page_size_bytes: 4096,
            page_count: 10_240,
            freelist_pages: 256,
          },
        },
        total_bytes: 41_943_040,
        free_bytes: 1_048_576,
        free_page_ratio: 0.025,
        budget: { state: 'unknown', reason: 'budget could not be determined' },
        growth: { state: 'unknown', reason: 'growth could not be determined' },
        table_growth: {
          state: 'unknown',
          omission_reasons: ['no baseline yet'],
          coverage: coverage('unknown', null, null, 'reads'),
        },
      },
    ],
  };
}

function findingsPayload() {
  return {
    family_filter: 'storage',
    entries: [
      {
        finding: {
          family: 'storage',
          state: 'degraded',
          evidence: [{ family: 'storage', reference: 'storage.over_budget.graph.db' }],
          coverage: { completeness: 'complete', statement: 'one store over its soft budget' },
        },
        storage_kind: 'over_budget_store',
      },
    ],
    storage_kind_statuses: [
      { kind: 'over_budget_store', state: 'real', reason: 'measured', observed_entries: 1 },
      { kind: 'orphan_store', state: 'partial', reason: 'one store unreadable', observed_entries: 0 },
    ],
    known_families: ['storage'],
    report_coverage: null,
    schema_convergences: [],
    note: 'storage retention and size authorities were consulted',
  };
}

function freshnessPayload() {
  return {
    note: 'live daemon scheduler state',
    worktrees: [
      {
        worktree_root: '/fast/projects/tracedecay',
        repository_id: 'repository.tracedecay',
        worktree_id: 'worktree.primary',
        source_reference: 'refs/heads/main',
        source_revision: null,
        latest_generation_id: null,
        code_graph_serving: { state: 'pending' },
        clone_index: null,
        snapshot_content_identity: null,
        sealed_at_micros: null,
        last_reconcile_micros: null,
        staleness_state: 'indexing',
        rebuild_in_flight: false,
        hook_hint_count: 0,
        coverage: 'partial_refresh_in_progress',
        parked: null,
        progress: {
          generation_id: 'generation.catchup.01',
          daemon_incarnation: 1,
          producer_incarnation: 1,
          progress_epoch: 1,
          sealed_source_digest: 'sha256:sealed',
          phase: 'index_build',
          committed_pages: 16,
          committed_chunks: 10_000,
          committed_imports: 480,
          committed_payload_bytes: 16_777_216,
          completed_files: 250,
          total_files: 500,
          completed_lexical_units: 32,
          total_lexical_units: 64,
          current_batch_pages: 4,
          current_batch_payload_bytes: 4_194_304,
          elapsed_micros: 120_000_000,
          last_commit_latency_micros: 240_000,
          files_per_second: 250,
          lexical_units_per_second: 16,
          estimated_remaining_seconds: 120,
          last_progress_micros: NOW,
          blocked_reason: null,
        },
      },
    ],
  };
}

