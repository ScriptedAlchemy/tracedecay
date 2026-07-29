import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { NavRail } from './NavRail.tsx';
import { ScopeBar } from './ScopeBar.tsx';
import { StatusStrip } from './StatusStrip.tsx';

const eventState = vi.hoisted(() => ({ state: 'connecting' as const }));

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: eventState.state, lastEventAt: null }),
  useEventsConnection: () => null,
  // No connection is mounted here, so there is no projection to reconcile
  // against — the same reading the real hook gives for a null connection.
  useProjectionSync: () => ({ kind: 'unmounted' }) as const,
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

function queryWrapper(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 0 } },
  });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

describe('shared shell truthfulness', () => {
  it('labels an unopened event stream as connecting, not synchronized', () => {
    const { getAllByRole, queryByText } = render(<StatusStrip />);

    // The strip reports the transport and the projection separately, so the
    // readings are collected rather than assumed to be one: what matters is that
    // neither of them claims synchronization for a stream that never opened.
    const readings = getAllByRole('status').map((node) => node.textContent);
    expect(readings).toContain('connecting');
    expect(readings.some((reading) => reading?.includes('sync'))).toBe(false);
    expect(queryByText('sync')).toBeNull();
  });

  it('does not render a receipt count when the event contract has no receipt family', () => {
    const { queryByText } = render(<StatusStrip />);

    expect(queryByText('Receipts')).toBeNull();
  });

  it('does not present the milestone name as the running build version', () => {
    const { queryByText } = render(<StatusStrip />);

    expect(queryByText('PR14')).toBeNull();
  });

  it('does not claim a local daemon when the backend is offline', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));

    const { queryByText } = render(queryWrapper(<MemoryRouter><NavRail /></MemoryRouter>));

    await waitFor(() => expect(queryByText('Local daemon')).toBeNull());
  });

  it('replaces an untrusted scope label with the registry-owned label', async () => {
    useScope.getState().selectProject('proj-real', 'fabricated label');
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            status: 'ok',
            projects: [
              {
                project_id: 'proj-real',
                label: 'Canonical project',
              },
            ],
          }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        ),
      ),
    );

    const { queryByText, findByText } = render(queryWrapper(<ScopeBar />));

    expect(queryByText('fabricated label')).toBeNull();
    expect(await findByText('Canonical project')).not.toBeNull();
  });
});
