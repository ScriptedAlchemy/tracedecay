import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CodePage } from './CodePage.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));

/**
 * Zeros this page measured, against reads it never got.
 *
 * This suite used to assert the opposite — that a well-formed 200 reporting
 * zeros "is still not a measurement" — and the page was written to match:
 * every zero total, empty slice, and empty result set rendered as
 * "unverified", on the stated grounds that the legacy response could not tell
 * zero from a query failure.
 *
 * It can. `LegacyBoundary` invokes a surface's render function only for
 * `outcome: 'ok'`, which is a 2xx whose body satisfied the route's schema;
 * every other reading — offline, 401, 403, a canonical 404/503, an
 * undecodable body, and the 500 these graph routes raise when the query fails
 * — is rendered by the boundary as that failure instead. So a zero reaching
 * the page has been measured, and the guard was suppressing real figures. It
 * was also an `||`: one zero among the three withheld all three, so a freshly
 * indexed project with symbols but no resolved edges was shown no node count.
 *
 * What follows pins both halves: a measured zero prints, and a read that
 * failed still refuses to print anything.
 */
const wire = (path: string) => resolveFixture(path) as Record<string, unknown>;

function jsonOk(body: unknown) {
  return { ok: true, status: 200, json: async () => body } as Response;
}

/** Every graph route answers 200 with a genuinely empty index. */
function serveMeasuredZeros() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/overview')) {
      return jsonOk({
        ...wire('/api/plugins/graph/overview'),
        totals: { nodes: 0, edges: 0, files: 0 },
        top_connected: [],
      });
    }
    if (url.includes('/subgraph')) {
      return jsonOk({ ...wire('/api/plugins/graph/subgraph'), nodes: [], edges: [] });
    }
    return jsonOk({ ...wire('/api/plugins/graph/search'), total: 0, count: 0, results: [] });
  });
}

/** Symbols and files, but no edge resolved yet: the case the `||` erased. */
function serveZeroEdgesOnly() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/overview')) {
      return jsonOk({
        ...wire('/api/plugins/graph/overview'),
        totals: { nodes: 4_210, edges: 0, files: 187 },
      });
    }
    if (url.includes('/subgraph')) {
      return jsonOk({ ...wire('/api/plugins/graph/subgraph'), nodes: [], edges: [] });
    }
    return jsonOk({ ...wire('/api/plugins/graph/search'), total: 0, count: 0, results: [] });
  });
}

/** The failure the old comment claimed was indistinguishable from a zero. */
function serveReadFailure() {
  return vi.fn(
    async () => ({ ok: false, status: 500, json: async () => ({}) }) as Response,
  );
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

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('a graph this page measured as empty', () => {
  it('prints the zeros, and calls the empty index empty', async () => {
    vi.stubGlobal('fetch', serveMeasuredZeros());
    renderCode();

    expect(await screen.findByText(/0 symbols indexed/i)).toBeTruthy();
    expect(await screen.findByText(/no symbols are indexed for this project/i)).toBeTruthy();
    // The word the page used to reach for whenever a count was zero.
    expect(screen.queryByText(/unverified/i)).toBeNull();
  });

  it('reports no match as no match once a search has run', async () => {
    vi.stubGlobal('fetch', serveMeasuredZeros());
    const user = userEvent.setup();
    renderCode();

    await user.type(screen.getByRole('searchbox', { name: /symbol search/i }), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/no symbol matches missing/i)).toBeTruthy();
    expect(screen.queryByText(/unverified/i)).toBeNull();
  });

  /**
   * The specific regression: `nodes === 0 || edges === 0 || files === 0`
   * withheld the whole panel for a graph that had plenty of both other
   * figures. A project mid-index reads exactly like this.
   */
  it('shows the node and file counts of a graph whose edge count is zero', async () => {
    vi.stubGlobal('fetch', serveZeroEdgesOnly());
    renderCode();

    expect(await screen.findByText(/4,210 symbols indexed/i)).toBeTruthy();
    expect(screen.queryByText(/unverified/i)).toBeNull();
  });
});

describe('a graph read that failed', () => {
  it('prints no figure at all, rather than zero', async () => {
    vi.stubGlobal('fetch', serveReadFailure());
    renderCode();

    // The boundary's own error rendering, which is what makes printing a
    // measured zero safe: a failure never arrives at the render function.
    // Every graph plate on the page reports it, so this is `findAll`.
    expect(
      await screen.findAllByText(/the read failed and nothing is being invented/i),
    ).not.toHaveLength(0);
    expect(screen.queryByText(/symbols indexed/i)).toBeNull();
    expect(screen.queryByText(/no symbols are indexed/i)).toBeNull();
  });
});
