import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BrainPage } from './BrainPage.tsx';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import type { ActivationField } from '../../viz/graph/activation.ts';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';

const stream = vi.hoisted(() => ({ pulses: [] as LiveActivityPulse[], revision: 0 }));
const graph = vi.hoisted(() => ({ activation: null as ActivationField | null }));
vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'live', lastEventAt: null }),
  useLiveActivity: () => stream,
}));
vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: (props: { activation: ActivationField; onInspect?: (id: string | null) => void }) => {
    graph.activation = props.activation;
    return <button type="button" onMouseEnter={() => props.onInspect?.('p1')}>Canvas project</button>;
  },
}));

const project = (id: string) => ({ project_id: id, label: id, project_root: `/${id}`, canonical_root: `/${id}`, kind: 'primary', store_count: 1, artifact_count: 2, alias_count: 0, branches: [], default_branch: null, last_seen_at: 1700000000 });
const projects = [project('p1'), project('p2')];
const groups = [{ label: 'repo', git_common_dir: '/repo/.git', branches: [], project_count: 2, projects }];

function mount() {
  vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify(fixtureEnvelope({
    status: 'ok', error: null, limit: 100, truncated: false, active_project_id: 'p1', active_project_root: '/p1',
    summary: { project_count: 2, repo_count: 1, truncated: false }, project_tree: groups, projects: [],
  })))));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const content = <QueryClientProvider client={client}><BrainPage /></QueryClientProvider>;
  return { ...render(content), client };
}

afterEach(() => {
  stream.pulses = [];
  stream.revision = 0;
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

describe('Brain exact interaction identity', () => {
  it('shares pointer and keyboard inspection without scope or activity changes', async () => {
    mount();
    fireEvent.mouseEnter(await screen.findByRole('button', { name: 'Canvas project' }));
    expect(within(screen.getByRole('region', { name: 'Inspected project' })).getByText('/p1')).toBeTruthy();
    expect(graph.activation?.warm).toBe(false);
    expect(useScope.getState().scope.kind).toBe('all');
    fireEvent.focus(screen.getByRole('button', { name: /^p2/ }));
    expect(within(screen.getByRole('region', { name: 'Inspected project' })).getByText('/p2')).toBeTruthy();
    fireEvent.keyDown(screen.getByRole('button', { name: /^p2/ }), { key: 'Escape' });
    expect(screen.queryByRole('region', { name: 'Inspected project' })).toBeNull();
    expect(graph.activation?.warm).toBe(false);
  });

  it('does not attribute an unscoped admitted event to the active project', async () => {
    const view = mount();
    await screen.findByRole('button', { name: 'Canvas project' });
    stream.pulses = [{ eventId: 'run:heartbeat:1', observationTime: '1700000000000001', projectId: null, family: 'heartbeat', streamId: 'heartbeat', at: Date.now() }];
    stream.revision = 1;
    view.rerender(<QueryClientProvider client={view.client}><BrainPage /></QueryClientProvider>);
    await waitFor(() => expect(graph.activation?.warm).toBe(false));
    stream.pulses = [...stream.pulses, { eventId: 'run:activity:1', observationTime: '1700000000000002', projectId: 'p1', family: 'hook_activity', streamId: 'activity', at: Date.now() }];
    stream.revision = 2;
    view.rerender(<QueryClientProvider client={view.client}><BrainPage /></QueryClientProvider>);
    await waitFor(() => expect(graph.activation?.heatOf('p1')).toBeGreaterThan(0));
    expect(graph.activation?.heatOf('repo:/repo/.git')).toBeGreaterThan(0);
    expect(graph.activation?.heatOf('p2')).toBe(0);
    fireEvent.click(screen.getByText(/Inspect admitted events/));
    fireEvent.click(screen.getByRole('button', { name: 'hook_activity · run:activity:1' }));
    expect(within(screen.getByRole('region', { name: 'Inspected project' })).getByText('/p1')).toBeTruthy();
    expect(useScope.getState().scope.kind).toBe('all');
    fireEvent.click(screen.getByRole('button', { name: 'heartbeat · run:heartbeat:1' }));
    expect(screen.queryByRole('region', { name: 'Inspected project' })).toBeNull();
  });
});
