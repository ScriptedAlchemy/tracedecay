/**
 * The DAG camera draws the fitted dependency board and keeps the board's
 * grammar: hover inspects without moving the address, click or Enter selects
 * into it. Projections retired by issue #4220 still arrive in saved links and
 * open the nearest surviving camera without losing the selected task.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../../data/scope/store.ts';
import { applicationProblem, applicationSuccess } from '../../../test/applicationEnvelope.ts';
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
        ? applicationSuccess('operation.work.views', payload)
        : applicationProblem('unavailable');
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

describe('the DAG camera', () => {
  it('DAG draws the fitted board; cards wear the typed-state family of their lane', async () => {
    const container = await renderBoard('/work?view=dag');
    expect(container.querySelector('[data-work-dag-field]')!.getAttribute('data-work-dag-view')).toBe('graph');
    expect(container.querySelector('[data-work-dag-fitted]')).not.toBeNull();
    const family = (taskId: string) => inField(container, taskId).getAttribute('data-work-lane-family');
    expect(['root', 'middle', 'side', 'leaf'].map(family)).toEqual(['ready', 'ready', 'degraded', 'disconnected']);
    expect(inField(container, 'side').style.backgroundImage).toContain('repeating-linear-gradient');
    expect(inField(container, 'leaf').className).toContain('border-dashed');
    fireEvent.pointerEnter(inField(container, 'side'));
    expect(container.querySelector('[data-work-dag-field]')!.getAttribute('data-work-dag-inspected')).toBe('side');
    expect(address.search).not.toContain('task=');
    fireEvent.click(inField(container, 'side'));
    await waitFor(() => expect(address.search).toContain('task=side'));
  });

  it.each([
    ['matrix', 'dag'],
    ['timeline', 'board'],
    ['causal', 'board'],
    ['workload', 'board'],
    ['topology', 'board'],
    ['concurrent-attempts', 'board'],
  ])('a saved ?view=%s link opens the %s and keeps the selected task', async (retired, opens) => {
    const page = render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <MemoryRouter initialEntries={[`/work?view=${retired}&task=middle`]}>
          <AddressProbe />
          <WorkPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await waitFor(() =>
      expect(page.container.querySelector('[data-work-header-camera]')?.getAttribute('data-work-header-camera')).toBe(opens),
    );
    expect(address.search).toContain('task=middle');
    expect([...page.container.querySelectorAll('[role="tab"]')].map((tab) => tab.textContent)).toEqual(['Board', 'DAG']);
  });
});
