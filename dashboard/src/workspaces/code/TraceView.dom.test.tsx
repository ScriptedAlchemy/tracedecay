/**
 * The TRACE drill-in, against the wire-true neighbors fixture.
 *
 * What this suite is protecting is not the picture — jsdom has no 2D context
 * and draws nothing — but the three claims that make the picture admissible:
 * the caption tells the truth about what is left out, the accessible equivalent
 * carries every symbol the field would draw, and reduced motion is a rendering
 * mode with the same data rather than a switched-off feature.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { TraceView, type TraceFocus } from './TraceView.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { setMotionPreference } from '../../viz/trace/reducedMotion.ts';

const FOCUS: TraceFocus = {
  id: 'sym-0',
  kind: 'function',
  name: 'resolve_context',
  file_path: 'src/dashboard/graph_service.rs',
  start_line: 212,
  degree: 24,
};

/** Requests actually issued, so the depth claim can be checked against them. */
let requested: string[] = [];

function mockFetch(override?: (url: string) => Response | undefined) {
  requested = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requested.push(url);
      const forced = override?.(url);
      if (forced) return forced;
      const { pathname, search } = new URL(url, 'http://localhost');
      return new Response(JSON.stringify(resolveFixture(pathname, search)), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderTrace(onClose = vi.fn()) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const utils = render(
    <QueryClientProvider client={client}>
      <TraceView focus={FOCUS} onClose={onClose} />
    </QueryClientProvider>,
  );
  return { ...utils, onClose };
}

beforeEach(() => {
  setMotionPreference('full');
  // jsdom ships no 2D context and logs a "not implemented" notice on every
  // probe. Returning null explicitly is the same answer with none of the noise,
  // and it is the case the surface has to survive: the canvas draws nothing and
  // the accessible list carries the whole field.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  // jsdom has neither; the surface must not depend on either existing.
  vi.stubGlobal('ResizeObserver', undefined);
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
});

afterEach(() => {
  setMotionPreference('system');
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('TraceView', () => {
  it('fetches one hop for the focus and then expands its neighbours for hop 2', async () => {
    mockFetch();
    renderTrace();
    await screen.findByText(/symbols on the field/i);
    const neighborCalls = requested.filter((url) => url.includes('/neighbors'));
    expect(neighborCalls[0]).toContain('/node/sym-0/neighbors');
    // The endpoint's own hard cap is asked for explicitly rather than left to
    // the server default, so the coverage figures are computed against a
    // stated limit.
    expect(neighborCalls[0]).toContain('limit=200');
    await waitFor(() => {
      expect(requested.filter((url) => url.includes('/neighbors')).length).toBeGreaterThan(1);
    });
  });

  it('states the depth and what it is leaving out, in counted figures', async () => {
    mockFetch();
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);

    // The readout strip above the field. These were a prose caption once; the
    // claims did not change when they became layout, so the assertions did not
    // weaken either — each figure now has to appear under its own label.
    const readout = container.querySelector('[data-testid="trace-readout"]')!.textContent ?? '';
    expect(readout).toContain('Depth limit');
    expect(readout).toContain('2 ↑ / 2 ↓');
    expect(readout).toContain('Beyond the limit');
    expect(readout).toMatch(/\d+named, not drawn/);
    expect(readout).toContain('past hop 2, nothing was named to this view');
    expect(readout).toMatch(/Callers ≤ 2 hops/);
    expect(readout).toMatch(/\d+ call sites/);

    // Every encoding the field uses is named in the key below it, including
    // the ones that are felt rather than seen.
    const key = container.querySelector('figure > figcaption')!.textContent ?? '';
    expect(key).toContain('hop distance from the focus — not elevation, not importance');
    expect(key).toContain('call sites on that one edge');
    expect(key).toContain("the symbol's degree, straight off the payload");
    expect(key).toContain('symbol kind, off the same arc as the connectivity spine');
    expect(key).toContain('edges this frame does not draw');
    expect(key).toContain('hover latency, bloom depth and settle time');
    expect(key).toContain('dragging deforms the neighbourhood');
  });

  it('derives the key from the same rows the field draws, so the two cannot drift', async () => {
    mockFetch();
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);

    // A legend is where a second source of truth grows: the picture comes from
    // the payload and the legend gets typed by hand. Here the up/down split is
    // printed twice, once on the strip and once in the key, and both are
    // counted from `model.nodes` — so if they ever disagree, one of them has
    // stopped reading the data.
    const reading = (label: string): string => {
      const cells = [...container.querySelectorAll('[data-testid="trace-readout"] dl > div')];
      const cell = cells.find((c) => (c.querySelector('dt')?.textContent ?? '').startsWith(label));
      return cell?.querySelector('dd .td-value')?.textContent ?? '';
    };
    const callers = reading('Callers');
    const callees = reading('Callees');
    expect(callers).toMatch(/^\d+$/);
    expect(callees).toMatch(/^\d+$/);

    const key = container.querySelector('figure > figcaption')!.textContent ?? '';
    expect(key).toContain(`${callers} ↑ / ${callees} ↓`);
  });

  it('says the wire carried no contains edges rather than inventing membranes', async () => {
    mockFetch((url) => {
      if (!url.includes('/neighbors')) return undefined;
      const { pathname, search } = new URL(url, 'http://localhost');
      const fixture = resolveFixture(pathname, search) as Record<string, unknown>;
      const payload = fixture.payload as {
        edges?: Array<{ kind?: string }>;
        edges_by_kind?: Array<{ kind?: string }>;
      };
      return new Response(
        JSON.stringify({
          ...fixture,
          payload: {
            ...payload,
            edges: (payload.edges ?? []).filter((edge) => edge.kind !== 'contains'),
            edges_by_kind: (payload.edges_by_kind ?? []).filter((e) => e.kind !== 'contains'),
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    });
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);
    await waitFor(() => {
      // The strip prints the absence as a reading in its own right — the word
      // `absent`, not a blank and not a zero — and refuses the inference that
      // would make it comfortable.
      const readout = container.querySelector('[data-testid="trace-readout"]')!.textContent ?? '';
      expect(readout).toMatch(/Types enteredabsent/);
      expect(readout).toContain('the payload carried no contains edges');
      expect(readout).toContain('not a claim about whether these symbols have types');
      const key = container.querySelector('figure > figcaption')!.textContent ?? '';
      expect(key).toContain('no enclosure is drawn on this frame');
    });
  });

  it('carries every drawn symbol in an accessible equivalent, not just on the canvas', async () => {
    mockFetch();
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);

    // One role="img" with a description, and the canvas itself hidden from AT.
    const field = await screen.findByRole('img');
    const description = field.getAttribute('aria-label') ?? '';
    expect(description).toMatch(/Call topography of resolve_context/);
    expect(description).toMatch(/tributaries/);
    expect(description).toMatch(/delta/);
    expect(description).toMatch(/The ranked list below carries the same symbols as text/);
    expect(container.querySelector('canvas')?.getAttribute('aria-hidden')).toBe('true');

    // The list is that equivalent: the focus plus every drawn neighbour, each
    // with the numbers the field encodes as position and width.
    const list = container.querySelector('ol')!;
    const items = within(list).getAllByRole('listitem');
    const drawnCount = Number(
      /(\d+) drawn/.exec(screen.getByText(/drawn · ordered by hop/).textContent ?? '')?.[1],
    );
    expect(items.length).toBe(drawnCount);
    expect(within(list).getByText('resolve_context')).toBeTruthy();
    expect(within(list).getAllByText(/call sites/).length).toBe(items.length);
    expect(within(list).getAllByText(/hops? (up|down)/).length).toBeGreaterThan(0);
  });

  it('renders reduced motion from settled positions instead of animating', async () => {
    mockFetch();
    const raf = vi.spyOn(globalThis, 'requestAnimationFrame');
    setMotionPreference('reduced');
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);

    expect(screen.getByText(/settled once; tension drawn as rail thickness/)).toBeTruthy();
    // The animated path is the only caller of requestAnimationFrame here, so a
    // reduced-motion mount that scheduled frames would be animating anyway.
    expect(raf).not.toHaveBeenCalled();
    // And the reader still gets the whole field as text.
    expect(container.querySelector('ol')!.querySelectorAll('li').length).toBeGreaterThan(1);

    const control = screen.getByRole('radio', { name: 'Reduced' });
    expect(control.getAttribute('aria-checked')).toBe('true');
  });

  it('lets the reader pin motion on or off regardless of the OS setting', async () => {
    mockFetch();
    const user = userEvent.setup();
    renderTrace();
    await screen.findByText(/symbols on the field/i);

    await user.click(screen.getByRole('radio', { name: 'Reduced' }));
    await waitFor(() => {
      expect(screen.getByText(/settled once; tension drawn as rail thickness/)).toBeTruthy();
    });
    await user.click(screen.getByRole('radio', { name: 'Full' }));
    await waitFor(() => {
      expect(screen.getByText(/hover a symbol to feel its weight/)).toBeTruthy();
    });
  });

  it('returns to the spine on Escape and on the back control', async () => {
    mockFetch();
    const user = userEvent.setup();
    const { onClose } = renderTrace();
    await screen.findByText(/symbols on the field/i);

    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole('button', { name: /back to spine/i }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it('shows a truthful state instead of an empty field when the read fails', async () => {
    mockFetch((url) =>
      url.includes('/neighbors')
        ? new Response('nope', { status: 500, statusText: 'boom' })
        : undefined,
    );
    const { container } = renderTrace();
    await waitFor(() => {
      expect(container.querySelector('[data-state="error"]')).toBeTruthy();
    });
    expect(screen.getByText(/nothing is being invented in its place/i)).toBeTruthy();
    expect(container.querySelector('canvas')).toBeNull();
  });

  it('does not treat an empty neighbor envelope as a measured zero', async () => {
    mockFetch((url) => {
      if (!url.includes('/neighbors')) return undefined;
      const { pathname, search } = new URL(url, 'http://localhost');
      const fixture = resolveFixture(pathname, search) as Record<string, unknown>;
      return new Response(
        JSON.stringify({
          ...fixture,
          payload: {
            ...(fixture.payload as Record<string, unknown>),
            callers: [],
            callees: [],
            edges: [],
            edges_by_kind: [],
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    });
    const { container } = renderTrace();
    await waitFor(() => {
      expect(container.querySelector('[data-state="partial"]')).toBeTruthy();
    });
    expect(screen.getByText(/call-edge result is unverified/i)).toBeTruthy();
    expect(screen.queryByText(/measured zero/i)).toBeNull();
  });
});
