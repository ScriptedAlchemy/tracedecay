/**
 * The dependency board's renderers over one graph version. Each is chosen
 * from the address, draws every task the layered board draws, and keeps the
 * board's grammar: hover inspects without moving the address, click or Enter
 * selects into it.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../../data/scope/store.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../../test/workGraphFixture.ts';
import { WorkPage } from '../WorkPage.tsx';

const GRAPH: WorkGraphVersionSpec = {
  tasks: [
    { taskId: 'root', title: 'Root task', effort: 2, lane: 'done' },
    { taskId: 'middle', title: 'Middle task', effort: 3, dependencies: ['root'], lane: 'ready' },
    { taskId: 'side', title: 'Side task', effort: 1, dependencies: ['root'], lane: 'blocked' },
    { taskId: 'leaf', title: 'Leaf task', effort: 5, dependencies: ['middle', 'side'], lane: 'cancelled' },
  ],
  criticalPath: ['root', 'middle', 'leaf'],
};

function serve() {
  const payload = workGraphRead(GRAPH);
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const views = String(url).includes('/work/views');
      const body = views
        ? {
            kind: 'success',
            value: {
              binding_id: 'binding.http.work.views',
              contract: { schema_id: 'schema.work.result', schema_revision: 1 },
              request_id: 'request-1',
              scope: { project_id: 'p', repository_id: 'r', worktree_id: 'w', reference: null, scope_digest: 'sha256:scope' },
              outcome: { outcome: 'evidence', value: { payload } },
            },
          }
        : { kind: 'problem', value: { problem: {} } };
      return new Response(JSON.stringify(body), { status: views ? 200 : 503, headers: { 'content-type': 'application/json' } });
    }),
  );
}

const address = { search: '' };
function AddressProbe() {
  address.search = useLocation().search;
  return null;
}

async function renderBoard(entry: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const page = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <AddressProbe />
        <WorkPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  await waitFor(() => expect(page.container.querySelector('[data-work-dag-board="drawn"]')).not.toBeNull());
  return page.container;
}

const inField = (container: HTMLElement, taskId: string) =>
  container.querySelector<HTMLElement>(`[data-work-dag-field] [data-work-task="${taskId}"]`)!;

beforeEach(serve);
afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('dependency board renderers', () => {
  it('switches renderer through the address and keeps the choice there', async () => {
    const container = await renderBoard('/work?view=dag');
    expect(container.querySelector('[data-work-dag-field]')!.getAttribute('data-work-dag-variant')).toBe('layered');
    fireEvent.click(container.querySelector('[data-work-dag-variant-option="matrix"]')!);
    await waitFor(() => expect(address.search).toContain('dag=matrix'));
    expect(container.querySelector('[data-work-dag-matrix]')!.getAttribute('data-work-dag-matrix')).toBe('4');
  });

  it('fitted: cards wear the typed-state family of their lane', async () => {
    const container = await renderBoard('/work?view=dag&dag=fitted');
    expect(container.querySelector('[data-work-dag-fitted]')).not.toBeNull();
    const family = (taskId: string) => inField(container, taskId).getAttribute('data-work-lane-family');
    expect(['root', 'middle', 'side', 'leaf'].map(family)).toEqual(['ready', 'ready', 'degraded', 'disconnected']);
    fireEvent.pointerEnter(inField(container, 'side'));
    expect(container.querySelector('[data-work-dag-field]')!.getAttribute('data-work-dag-inspected')).toBe('side');
    expect(address.search).not.toContain('task=');
    fireEvent.click(inField(container, 'side'));
    await waitFor(() => expect(address.search).toContain('task=side'));
  });

  it('swimlane: one milestone lane, the deepest chain marked, and a holder lane that names its absence', async () => {
    const container = await renderBoard('/work?view=dag&dag=swimlane');
    expect(container.querySelector('[data-work-dag-swimlane]')!.getAttribute('data-work-dag-swimlane')).toBe('1');
    expect(container.querySelector('[data-work-swimlane-chain]')!.textContent).toContain('3 deep');
    expect(
      [...container.querySelectorAll('[data-work-swimlane-chain-edge]')].map((node) => node.getAttribute('data-work-swimlane-edge')).sort(),
    ).toEqual(['gating:middle->leaf', 'gating:root->middle']);
    fireEvent.click(container.querySelector('[data-work-swimlane-key="holder"]')!);
    expect(container.querySelector('[data-work-swimlane-lane]')!.getAttribute('data-work-swimlane-lane')).toBe('no handoff recorded');
  });

  it('matrix: one listbox tab stop, arrows inspect, Enter selects', async () => {
    const container = await renderBoard('/work?view=dag&dag=matrix');
    const matrix = container.querySelector<HTMLElement>('[data-work-dag-matrix]')!;
    expect(matrix.getAttribute('role')).toBe('listbox');
    expect(matrix.getAttribute('data-work-dag-matrix-back')).toBe('0');
    expect(container.querySelectorAll('[data-work-dsm-cell]')).toHaveLength(4);
    expect(
      [...container.querySelectorAll('[data-work-dag-matrix] [role="option"]')].map((node) => node.getAttribute('tabindex')),
    ).toEqual(['-1', '-1', '-1', '-1']);
    fireEvent.focus(matrix);
    fireEvent.keyDown(matrix, { key: 'ArrowDown' });
    const inspected = container.querySelector('[data-work-dag-field]')!.getAttribute('data-work-dag-inspected');
    expect(inspected).not.toBeNull();
    expect(matrix.getAttribute('aria-activedescendant')).not.toBeNull();
    fireEvent.keyDown(matrix, { key: 'Enter' });
    await waitFor(() => expect(address.search).toContain(`task=${inspected}`));
  });
});
