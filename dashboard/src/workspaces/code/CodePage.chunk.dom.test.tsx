/**
 * The trace drill-in is its own chunk.
 *
 * `TraceView` is a thousand lines and pulls in the whole of `viz/trace` — the
 * canvas renderer, the spring integrator, the palette resolver — none of which
 * a reader who only looks at the spine ever needs. This suite holds the split
 * boundary to two claims that a bundle report cannot make for us:
 *
 *   1. rendering the Code page does not request the trace module at all, and
 *      the page is fully interactive in that state;
 *   2. while the chunk is in flight the surface says so, and says nothing else
 *      — no field, no plate, no figure, because no call edge has been read.
 *
 * The module is mocked behind a gate that is released by hand, which is what
 * makes "was it requested" and "what is on screen before it resolves"
 * observable at all. A static import trips that gate while this file's own
 * imports are still evaluating, so claim 1 is what fails if the boundary
 * regresses — verified by putting the static import back and watching it go
 * red on the request count.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router';

import { CodePage } from './CodePage.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

/** Held open until a test releases it, so the Suspense fallback can be read. */
const chunk = vi.hoisted(() => {
  let release: () => void = () => {};
  const arrived = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { arrived, release, requests: 0 };
});

vi.mock('./TraceView.tsx', async () => {
  chunk.requests += 1;
  // The gate is what makes the in-flight state observable at all. It is
  // BOUNDED because a regression to a static import reaches this factory while
  // this file's own imports are still evaluating, and an unbounded wait there
  // deadlocks the whole suite — reported as a timeout, with nothing pointing at
  // the cause. Bounded, the same regression falls through to the request-count
  // assertion below and names itself.
  let valve: ReturnType<typeof setTimeout> | undefined;
  await Promise.race([
    chunk.arrived,
    new Promise<void>((resolve) => {
      valve = setTimeout(resolve, 2_000);
    }),
  ]);
  clearTimeout(valve);
  return {
    TraceView: ({ focus }: { focus: { name?: string | null } }) => (
      <div data-testid="trace-view">{focus.name}</div>
    ),
  };
});

// Sigma wants WebGL, which jsdom does not have. Same stand-in the sibling
// suite uses; it is not the subject here.
vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));

/** Wire-true bodies from the shared fixture module. */
function mockFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const { pathname, search } = new URL(String(input), 'http://localhost');
      return new Response(JSON.stringify(resolveFixture(pathname, search)), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderCode() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <CodePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/**
 * Requests recorded by the time this file finished evaluating — which is after
 * `./CodePage.tsx` above has been imported and before any test body runs. A
 * static import makes this 1: importing the page is what pulls the trace module
 * in. It is read as an absolute figure rather than a per-test baseline, because
 * a baseline taken inside a test records the regression instead of catching it.
 */
const requestsAfterImport = chunk.requests;

/** A hub whose name is unique in the fixture, so the card is unambiguous. */
const HUB = /find_direct_child_by_kind/;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Code page trace chunk', () => {
  it('renders and is interactive without requesting the trace module', async () => {
    mockFetch();
    // Importing the page is not allowed to import the trace.
    expect(requestsAfterImport).toBe(0);
    const user = userEvent.setup();
    renderCode();

    // The spine is up: hub cards are drawn from the overview payload.
    expect(await screen.findByRole('button', { name: HUB })).toBeTruthy();
    expect(
      screen
        .getByRole('button', { name: /CORTEX.*repository/i })
        .getAttribute('aria-current'),
    ).toBe('step');
    // And the page takes input while the trace module is still unfetched.
    const search = screen.getByRole<HTMLInputElement>('searchbox', {
      name: /symbol search/i,
    });
    await user.type(search, 'resolve');
    expect(search.value).toBe('resolve');

    // And nothing about drawing the spine reaches for it either.
    expect(chunk.requests).toBe(0);
    expect(screen.queryByTestId('trace-chunk-fallback')).toBeNull();
    expect(screen.queryByTestId('trace-view')).toBeNull();
  });

  it('states that the chunk is loading without implying an empty trace', async () => {
    mockFetch();
    const user = userEvent.setup();
    renderCode();

    // Opening a hub is the one gesture that enters the trace, so it is also
    // the only thing that should fetch the module.
    await user.click(await screen.findByRole('button', { name: HUB }));
    const fallback = await screen.findByTestId('trace-chunk-fallback');
    expect(chunk.requests).toBe(1);
    expect(
      screen.getByRole('button', { name: /TRACE.*symbol/i }).getAttribute('aria-current'),
    ).toBe('step');

    // Says loading, and says outright that this is not an empty neighbourhood.
    const status = within(fallback).getByRole('status');
    expect(status.textContent).toMatch(/loading the trace view/i);
    expect(status.textContent).toMatch(/no call edge has been requested yet/i);

    // Nothing that reads as a measurement: no readout plate, no canvas, no
    // described field, and not a single numeric cell anywhere in the pane.
    expect(within(fallback).queryByTestId('trace-readout')).toBeNull();
    expect(within(fallback).queryByTestId('trace-canvas')).toBeNull();
    expect(within(fallback).queryByRole('img')).toBeNull();
    expect(fallback.querySelectorAll('[data-cell="numeric"]').length).toBe(0);
    expect(fallback.textContent).not.toMatch(/\d/);

    // The reader is not stranded in a pane that has no way out of it while the
    // chunk is in flight.
    expect(within(fallback).getByRole('button', { name: /back to spine/i })).toBeTruthy();

    // Only once the chunk lands does the trace itself appear.
    expect(screen.queryByTestId('trace-view')).toBeNull();
    chunk.release();
    await waitFor(() => {
      expect(screen.getByTestId('trace-view')).toBeTruthy();
    });
    expect(screen.queryByTestId('trace-chunk-fallback')).toBeNull();
  });
});
