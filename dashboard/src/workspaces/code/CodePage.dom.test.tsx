import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CodePage } from './CodePage.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));

/** Wire-true bodies from the shared fixture module — gated against the
 * generated contracts by `endpoint-fixtures.test.ts` — emptied of the rows a
 * collapsed backend read would have dropped. The point of this suite is that a
 * WELL-FORMED response reporting zeros is still not a measurement, so the
 * envelope around those zeros has to be the one the daemon really sends. */
const wire = (path: string) => resolveFixture(path) as Record<string, unknown>;

function serveLegacyGraphZeros() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const body = url.includes('/overview')
      ? {
          ...wire('/api/plugins/graph/overview'),
          totals: { nodes: 0, edges: 0, files: 0 },
          top_connected: [],
        }
      : url.includes('/subgraph')
        ? { ...wire('/api/plugins/graph/subgraph'), nodes: [], edges: [] }
        : { ...wire('/api/plugins/graph/search'), total: 0, count: 0, results: [] };
    return {
      ok: true,
      status: 200,
      json: async () => body,
    } as Response;
  });
}

function renderCode() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <CodePage />
    </QueryClientProvider>,
  );
}

describe('CodePage legacy graph zero states', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('does not present backend-collapsed zero rows as measured empty results', async () => {
    vi.stubGlobal('fetch', serveLegacyGraphZeros());
    const user = userEvent.setup();
    renderCode();

    expect(await screen.findByText(/graph totals are unverified/i)).toBeTruthy();
    expect(screen.queryByText(/0 symbols indexed/i)).toBeNull();
    expect(await screen.findByText(/graph slice is unverified/i)).toBeTruthy();
    expect(screen.queryByTestId('graph-canvas')).toBeNull();

    await user.type(screen.getByRole('searchbox', { name: /symbol search/i }), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/search result is unverified/i)).toBeTruthy();
    expect(screen.queryByText(/no symbols matched/i)).toBeNull();
  });
});
