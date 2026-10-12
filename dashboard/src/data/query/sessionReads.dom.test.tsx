import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { http, HttpResponse, type JsonBodyType } from 'msw';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from '../../workspaces/agents/AgentsPage.tsx';
import { LoomPage } from '../../workspaces/loom/LoomPage.tsx';
import { SessionsPage } from '../../workspaces/sessions/SessionsPage.tsx';
import { useLcmTimeline } from './sessionReads.ts';

const server = fixtureServer();
const requested: URL[] = [];
beforeAll(() => {
  server.listen({ onUnhandledFrame: 'error' });
  server.events.on('request:start', ({ request }) => {
    requested.push(new URL(request.url));
  });
});
beforeEach(() => {
  requested.length = 0;
  server.use(
    http.post('*/api/feedback/proximity', () =>
      HttpResponse.json(resolveFixture('/api/feedback/proximity') as JsonBodyType),
    ),
  );
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  server.resetHandlers();
});
afterAll(() => server.close());

function renderShared(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

const reads = (suffix: string) => requested.filter((url) => url.pathname.endsWith(suffix));

describe('session reads shared across workspaces', () => {
  it('reads the subagent tree once for Agents and Loom', async () => {
    renderShared(
      <>
        <AgentsPage />
        <LoomPage />
      </>,
    );
    await screen.findByText(/^\d+ sessions · \d+ drawn · \d+ generations$/);
    await screen.findAllByRole('button', { name: /^Select session / });
    expect(reads('/subagent-tree')).toHaveLength(1);
  });

  it('reads the day/400 timeline once for Sessions at its default view and Loom', async () => {
    renderShared(
      <>
        <SessionsPage />
        <LoomPage />
      </>,
    );
    await screen.findAllByRole('button', { name: /^Select session / });
    await waitFor(() => expect(reads('/hermes-lcm/timeline').length).toBeGreaterThan(0));
    const timeline = reads('/hermes-lcm/timeline');
    expect(timeline).toHaveLength(1);
    expect(timeline[0]!.searchParams.get('bucket')).toBe('day');
    expect(timeline[0]!.searchParams.get('limit')).toBe('400');
  });

  it('keeps timeline reads with differing windows separate', async () => {
    function Probe({ limit }: { limit: number }) {
      const read = useLcmTimeline('day', limit);
      return <output>{read.isPending ? 'pending' : `read ${limit}`}</output>;
    }
    renderShared(
      <>
        <Probe limit={400} />
        <Probe limit={90} />
      </>,
    );
    await screen.findByText('read 400');
    await screen.findByText('read 90');
    expect(reads('/hermes-lcm/timeline').map((url) => url.searchParams.get('limit')).sort()).toEqual([
      '400',
      '90',
    ]);
  });
});
