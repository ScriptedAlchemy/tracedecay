import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { useQueryCancellation } from '../../data/query/activity.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { QueryActivityStatus, StatusStrip } from './StatusStrip.tsx';

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: 'live', lastEventAt: null }),
  useProjectionSync: () => ({ kind: 'synced' }) as const,
}));

function ActiveQuery({
  cancelable,
  onAbort,
}: {
  cancelable: boolean;
  onAbort: () => void;
}) {
  useQuery({
    queryKey: ['test', cancelable ? 'cancelable' : 'fixed'],
    meta: {
      dashboard: {
        activity: {
          id: cancelable ? 'graph-search' : 'graph-refresh',
          label: cancelable ? 'Searching the code graph' : 'Refreshing graph authority',
          cancelable,
        },
      },
    },
    queryFn: ({ signal }) =>
      new Promise<never>((_resolve, reject) => {
        signal.addEventListener(
          'abort',
          () => {
            onAbort();
            reject(signal.reason);
          },
          { once: true },
        );
      }),
  });
  return null;
}

function mount(cancelable: boolean, onAbort: () => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ActiveQuery cancelable={cancelable} onAbort={onAbort} />
      <StatusStrip queryActivity={<QueryActivityStatus />} />
    </QueryClientProvider>,
  );
}

function TrackedLegacyQuery() {
  useLegacy(
    ['graph', 'search', 'needle'],
    '/api/plugins/graph/search?q=needle',
    z.object({
      results: z.array(z.unknown()),
    }),
    {
      activity: {
        id: 'code-search:needle',
        label: 'Searching indexed symbols',
        cancelable: true,
      },
    },
  );
  return null;
}

afterEach(() => {
  useQueryCancellation.setState({ lastCancellation: null });
  vi.restoreAllMocks();
});

describe('status-strip query activity', () => {
  it('cancels only a query whose tracked function consumes the abort signal', async () => {
    let aborted = false;
    mount(true, () => {
      aborted = true;
    });

    expect(await screen.findByText('Searching the code graph')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel Searching the code graph' }));

    await waitFor(() => expect(aborted).toBe(true));
    expect(await screen.findByText('cancelled · Searching the code graph')).toBeTruthy();
  });

  it('reports non-cancelable background work without offering a false control', async () => {
    mount(false, () => {
      throw new Error('a non-cancelable query must not be aborted by the strip');
    });

    expect(await screen.findByText('Refreshing graph authority')).toBeTruthy();
    expect(
      screen.queryByRole('button', { name: 'Cancel Refreshing graph authority' }),
    ).toBeNull();
  });

  it('tracks and aborts the real shared query transport when a caller labels it', async () => {
    let aborted = false;
    vi.stubGlobal(
      'fetch',
      vi.fn((_url: string | URL | Request, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener(
            'abort',
            () => {
              aborted = true;
              reject(init.signal?.reason);
            },
            { once: true },
          );
        }),
      ),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    render(
      <QueryClientProvider client={client}>
        <TrackedLegacyQuery />
        <StatusStrip queryActivity={<QueryActivityStatus />} />
      </QueryClientProvider>,
    );

    expect(await screen.findByText('Searching indexed symbols')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel Searching indexed symbols' }));
    await waitFor(() => expect(aborted).toBe(true));
  });
});
