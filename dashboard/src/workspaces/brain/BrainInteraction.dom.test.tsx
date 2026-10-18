import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BrainPage } from './BrainPage.tsx';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';

const stream = vi.hoisted(() => ({ pulses: [] as LiveActivityPulse[], revision: 0 }));
vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'live', lastEventAt: null }),
  useLiveActivity: () => stream,
}));

const project = (id: string) => ({
  project_id: id,
  label: id,
  project_root: `/${id}`,
  canonical_root: `/${id}`,
  kind: 'primary',
  store_count: 1,
  artifact_count: 2,
  alias_count: 0,
  branches: [],
  default_branch: null,
  head_branch: null,
  last_seen_at: 1700000000,
});
const projects = [project('p1'), project('p2')];
const groups = [{ label: 'repo', git_common_dir: '/repo/.git', branches: [], project_count: 2, projects }];

function mount() {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(
          JSON.stringify(
            fixtureEnvelope({
              status: 'ok',
              error: null,
              limit: 100,
              truncated: false,
              active_project_id: 'p1',
              active_project_root: '/p1',
              summary: { project_count: 2, repo_count: 1, truncated: false },
              project_tree: groups,
              projects: [],
            }),
          ),
        ),
    ),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const content = (
    <QueryClientProvider client={client}>
      <BrainPage />
    </QueryClientProvider>
  );
  return { ...render(content), client };
}

afterEach(() => {
  stream.pulses = [];
  stream.revision = 0;
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

describe('Brain exact interaction identity', () => {
  it('shares pointer and keyboard inspection without scope changes', async () => {
    mount();
    fireEvent.pointerEnter(await screen.findByRole('button', { name: /^p1/ }));
    expect(within(screen.getByRole('region', { name: 'Inspected project' })).getByText('/p1')).toBeTruthy();
    expect(useScope.getState().scope.kind).toBe('all');
    fireEvent.focus(screen.getByRole('button', { name: /^p2/ }));
    expect(within(screen.getByRole('region', { name: 'Inspected project' })).getByText('/p2')).toBeTruthy();
    fireEvent.keyDown(screen.getByRole('button', { name: /^p2/ }), { key: 'Escape' });
    expect(screen.queryByRole('region', { name: 'Inspected project' })).toBeNull();
  });

  it('does not attribute an unscoped admitted event to the active project', async () => {
    const view = mount();
    await screen.findByRole('button', { name: /^p1/ });
    stream.pulses = [
      {
        eventId: 'run:heartbeat:1',
        observationTime: '1700000000000001',
        projectId: null,
        family: 'heartbeat',
        streamId: 'heartbeat',
        at: Date.now(),
      },
    ];
    stream.revision = 1;
    view.rerender(
      <QueryClientProvider client={view.client}>
        <BrainPage />
      </QueryClientProvider>,
    );
    fireEvent.click(screen.getByText(/Inspect admitted events/));
    fireEvent.click(screen.getByRole('button', { name: 'heartbeat · run:heartbeat:1' }));
    expect(screen.queryByRole('region', { name: 'Inspected project' })).toBeNull();
    expect(useScope.getState().scope.kind).toBe('all');
  });
});
