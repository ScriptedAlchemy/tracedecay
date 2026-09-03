import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SymbolPath } from './SymbolPath.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

/**
 * `/api/plugins/graph/path` answers a bidirectional, any-edge-kind search
 * between two node IDs. The rules under test are the three claims this panel
 * must never overstate:
 *
 *   - `found: false` is a measurement AT a depth, so the negative prints that
 *     depth and never reads as "these two are unconnected";
 *   - the walk is bidirectional, so a hop carried by a reversed edge is drawn
 *     as reversed rather than flattened into an undirected line that would
 *     assert a relationship the index does not hold;
 *   - a hop the producer measured but could not hydrate keeps its raw ID,
 *     because dropping it would silently shorten the route.
 */

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Symbol connection panel', () => {
  it('prints the depth searched instead of claiming the symbols are unconnected', async () => {
    stub({
      from: 'n-a',
      to: 'n-b',
      found: false,
      path: [],
      nodes: [],
      edges: [],
      max_depth: 6,
    });
    await choosePair();

    // The depth is on the same sentence as the negative, and the disclaimer
    // that follows is what keeps a bounded search from reading as a proof.
    const negative = await screen.findByText(/no route within 6 hops/i);
    expect(negative.textContent).toMatch(/searched in either direction/i);
    expect(negative.textContent).toMatch(/a longer route is not excluded/i);
    expect(negative.textContent).toMatch(/not a statement that the two are unconnected/i);
  });

  it('draws each hop with its edge kind and the direction the edge actually runs', async () => {
    stub({
      from: 'n-a',
      to: 'n-c',
      found: true,
      path: ['n-a', 'n-b', 'n-c'],
      nodes: [node('n-a', 'alpha'), node('n-b', 'beta'), node('n-c', 'gamma')],
      edges: [
        { kind: 'calls', line: 3, source: 'n-a', source_name: 'alpha', target: 'n-b', target_name: 'beta' },
        // Reversed: gamma imports beta, so the route reaches gamma by walking
        // this edge backwards. The panel must say so.
        { kind: 'imports', line: null, source: 'n-c', source_name: 'gamma', target: 'n-b', target_name: 'beta' },
      ],
      max_depth: 6,
    });
    await choosePair();

    expect(await screen.findByText(/2 hops/)).toBeTruthy();
    expect(screen.getByLabelText(/calls edge running forward along the route/i)).toBeTruthy();
    expect(screen.getByLabelText(/imports edge running backward along the route/i)).toBeTruthy();
    // Twice on purpose: once as the chosen endpoint, once as the route's last
    // hop. The panel keeps both, so the reader can see the route ends where
    // they asked rather than having to trust that it does.
    expect(screen.getAllByText('gamma').length).toBe(2);
  });

  it('keeps a measured hop the index could not name, rather than shortening the route', async () => {
    stub({
      from: 'n-a',
      to: 'n-c',
      found: true,
      path: ['n-a', 'n-orphan', 'n-c'],
      nodes: [node('n-a', 'alpha'), node('n-c', 'gamma')],
      edges: [],
      max_depth: 6,
    });
    await choosePair();

    expect(await screen.findByText('n-orphan')).toBeTruthy();
    expect(screen.getByText(/2 hops/)).toBeTruthy();
  });
});

function node(id: string, name: string) {
  return {
    assertions: null,
    attrs_start_line: null,
    branches: null,
    degree: null,
    doc: null,
    edge_kind: null,
    edge_line: null,
    end_column: null,
    end_line: null,
    file_path: `src/${name}.rs`,
    id,
    is_async: null,
    kind: 'function',
    loops: null,
    max_nesting: null,
    name,
    parent_id: null,
    qualified_name: null,
    returns: null,
    signature: null,
    span: null,
    start_column: null,
    start_line: null,
    unchecked_calls: null,
    unsafe_blocks: null,
    updated_at: null,
    visibility: null,
  };
}

/** The envelope header the daemon actually stamps, taken from the fixture the
 * contract gate parses rather than hand-built here — a second hand-written
 * envelope is exactly how a test starts passing against a shape the daemon does
 * not send. */
function envelope(payload: unknown) {
  const shell = resolveFixture('/api/plugins/graph/path') as Record<string, unknown>;
  return { ...shell, payload };
}

/** Both endpoint pickers search the same route, so one search body serves
 * both; the two ends are told apart by which result the reader clicks. */
function stub(path: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const body = url.includes('/graph/path')
        ? envelope(path)
        : envelope({
            query: 'x',
            limit: 6,
            offset: 0,
            total: 2,
            count: 2,
            results: [node('n-a', 'alpha'), node('n-c', 'gamma')],
          });
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

/** Search from both ends and pick one result each, which is the only way this
 * panel can reach a state where it has two node IDs to ask about. */
async function choosePair() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <SymbolPath />
    </QueryClientProvider>,
  );
  await userEvent.type(screen.getByRole('searchbox', { name: /search for the from symbol/i }), 'a{Enter}');
  await userEvent.click(await screen.findByRole('button', { name: /alpha/ }));
  await userEvent.type(screen.getByRole('searchbox', { name: /search for the to symbol/i }), 'g{Enter}');
  await userEvent.click(await screen.findByRole('button', { name: /gamma/ }));
}
