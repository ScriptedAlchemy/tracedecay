import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { EMPTY_COMPARE_SELECTION, type CompareSelection } from './compareLayout.ts';
import { CompareView } from './CompareView.tsx';

const NOW_MICROS = 1_753_003_600_000_000;
const REVISION = 'a'.repeat(40);

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Compare prefill', () => {
  it('prefills head from a unique fresh indexed worktree', async () => {
    stubFreshness([
      worktree({
        source_reference: 'refs/heads/feature/long-name',
        source_revision: REVISION,
        staleness_state: 'fresh',
      }),
    ]);
    renderCompare();

    expect(await screen.findByDisplayValue('feature/long-name')).toBeTruthy();
    expect(screen.getByDisplayValue(REVISION)).toBeTruthy();
  });

  it('does not auto-prefill when several fresh worktrees are present', async () => {
    stubFreshness([
      worktree({
        worktree_root: '/a',
        source_reference: 'refs/heads/main',
        source_revision: REVISION,
        staleness_state: 'fresh',
      }),
      worktree({
        worktree_root: '/b',
        source_reference: 'refs/heads/other',
        source_revision: 'b'.repeat(40),
        staleness_state: 'fresh',
      }),
    ]);
    renderCompare();

    await screen.findByText(/nothing is prefilled/i);
    expect(screen.getByRole('textbox', { name: /head branch/i })).toHaveProperty('value', '');
    expect(screen.getByRole('textbox', { name: /head revision/i })).toHaveProperty('value', '');
  });

  it('does not auto-prefill a stale worktree even when it is the only candidate', async () => {
    stubFreshness([
      worktree({
        source_reference: 'refs/heads/stale-branch',
        source_revision: REVISION,
        staleness_state: 'stale',
      }),
    ]);
    renderCompare();

    await screen.findByText(/nothing is prefilled/i);
    expect(screen.queryByDisplayValue('stale-branch')).toBeNull();
  });

  it('keeps a cleared head cleared across parent re-renders', async () => {
    stubFreshness([
      worktree({
        source_reference: 'refs/heads/feature/long-name',
        source_revision: REVISION,
        staleness_state: 'fresh',
      }),
    ]);
    const user = userEvent.setup();
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    const view = render(
      <QueryClientProvider client={client}>
        <CompareView selection={EMPTY_COMPARE_SELECTION} onSelectionChange={() => undefined} />
      </QueryClientProvider>,
    );

    const branch = await screen.findByDisplayValue('feature/long-name');
    await user.clear(branch);
    await user.clear(screen.getByRole('textbox', { name: /head revision/i }));

    // Same client, new parent render: primitive effect deps must not refill.
    view.rerender(
      <QueryClientProvider client={client}>
        <CompareView selection={EMPTY_COMPARE_SELECTION} onSelectionChange={() => undefined} />
      </QueryClientProvider>,
    );

    expect(screen.getByRole('textbox', { name: /head branch/i })).toHaveProperty('value', '');
    expect(screen.getByRole('textbox', { name: /head revision/i })).toHaveProperty('value', '');
  });

  it('wraps long branch values at a 320px-wide form', async () => {
    stubFreshness([
      worktree({
        source_reference: `refs/heads/${'very-long-branch-name-'.repeat(4)}tail`,
        source_revision: REVISION,
        staleness_state: 'fresh',
      }),
    ]);
    const { container } = renderCompare();
    Object.defineProperty(container.firstElementChild, 'clientWidth', {
      configurable: true,
      value: 320,
    });

    const longName = `${'very-long-branch-name-'.repeat(4)}tail`;
    const branch = await screen.findByDisplayValue(longName);
    expect(branch.className).toContain('break-all');
    expect(branch.className).toContain('max-w-full');
    const guidance = document.querySelector('[data-compare-guidance] dd:last-of-type');
    expect(guidance?.className).toContain('min-w-0');
    expect(guidance?.className).toContain('break-all');
    expect(guidance?.getAttribute('title')).toContain(longName);
    expect(guidance?.textContent).toContain(longName);
  });
});

function renderCompare(selection: CompareSelection = EMPTY_COMPARE_SELECTION) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <CompareView selection={selection} onSelectionChange={() => undefined} />
    </QueryClientProvider>,
  );
}

function stubFreshness(
  worktrees: ReturnType<typeof worktree>[],
): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      new Response(
        JSON.stringify({
          schema_revision: 1,
          scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
          version: { entity_version: null, graph_version: null },
          time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
          source_watermark: null,
          authorization: { outcome: 'authorized' },
          coverage: {
            completeness: 'complete',
            eligible: null,
            examined: null,
            matched: null,
            excluded: null,
            omitted: null,
            unknown: null,
            denominator: null,
            unit: 'mounted_worktree',
            omission_reasons: [],
          },
          freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
          domain_state: 'ready',
          legal_actions: [
            { kind: 'refresh', operation: 'use-case.dashboard.code-index.freshness.refresh' },
          ],
          payload: {
            worktrees,
            note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
          },
        }),
        { status: 200 },
      ),
    ),
  );
}

function worktree(
  overrides: Partial<{
    worktree_root: string;
    source_reference: string | null;
    source_revision: string | null;
    staleness_state: string | null;
  }> = {},
) {
  return {
    worktree_root: overrides.worktree_root ?? '/fast/projects/tracedecay',
    repository_id: 'repository.tracedecay',
    worktree_id: 'worktree.primary',
    source_reference: overrides.source_reference ?? 'refs/heads/main',
    latest_generation_id: 'generation.4f21c9',
    snapshot_content_identity: 'sha256:8ab31c',
    source_revision: overrides.source_revision ?? REVISION,
    sealed_at_micros: NOW_MICROS - 600_000_000,
    last_reconcile_micros: NOW_MICROS,
    staleness_state: overrides.staleness_state ?? 'fresh',
    rebuild_in_flight: false,
    hook_hint_count: 0,
    coverage: 'complete',
    progress: null,
    parked: null,
  };
}
