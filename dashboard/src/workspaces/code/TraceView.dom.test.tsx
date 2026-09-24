/**
 * The TRACE drill-in, against the wire-true neighbors fixture.
 *
 * What this suite is protecting is not the picture, jsdom has no 2D context
 * and draws nothing, but the three claims that make the picture admissible:
 * the caption tells the truth about what is left out, the accessible equivalent
 * carries every symbol the field would draw, and reduced motion is a rendering
 * mode with the same data rather than a switched-off feature.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
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

function renderTrace(onClose = vi.fn(), url = '/code?view=trace', onFocusChange?: (node: TraceFocus) => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const utils = render(
    <MemoryRouter initialEntries={[url]}>
      <QueryClientProvider client={client}>
        <TraceView focus={FOCUS} onClose={onClose} {...(onFocusChange ? { onFocusChange } : {})} />
      </QueryClientProvider>
    </MemoryRouter>,
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
      // The strip prints the absence as a reading in its own right, the word
      // `absent`, not a blank and not a zero, and refuses the inference that
      // would make it comfortable.
      const readout = container.querySelector('[data-testid="trace-readout"]')!.textContent ?? '';
      expect(readout).toMatch(/Types enteredabsent/);
      expect(readout).toContain('the payload carried no contains edges');
      expect(readout).toContain('not a claim about whether these symbols have types');
      const key = container.querySelector('figure > figcaption')!.textContent ?? '';
      expect(key).toContain('no enclosure is drawn on this frame');
    });
  });

  it('carries every drawn symbol on the canvas and in an accessible equivalent', async () => {
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

  describe('candidate renderers', () => {
    beforeEach(() => {
      // jsdom lays nothing out; give the host a real column width.
      vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
        width: 926,
        height: 600,
        x: 0,
        y: 0,
        top: 0,
        left: 0,
        right: 926,
        bottom: 600,
        toJSON: () => ({}),
      });
    });

    it('draws the anatomy plate from the same model, with every symbol focusable', async () => {
      mockFetch();
      const { container } = renderTrace(vi.fn(), '/code?view=trace&trace=plate');
      await screen.findByText(/symbols on the field/i);
      await waitFor(() => expect(container.querySelector('[data-trace-field="plate"] svg')).not.toBeNull());
      const field = container.querySelector('[data-trace-field="plate"] svg')!;
      expect(field.getAttribute('aria-label')).toMatch(/^Call neighbourhood of resolve_context as an anatomy plate/);
      const targets = field.querySelectorAll('[data-node]');
      const list = within(container.querySelector('ol')!).getAllByRole('listitem');
      expect(targets.length).toBe(list.length);
      expect(field.textContent).toContain('7 symbols · 45 sites');
      expect(field.textContent).toContain('ONE SCALE · CALL SITES PER CHANNEL');
      // The spring field and its motion control are not mounted.
      expect(container.querySelector('canvas')).toBeNull();
      expect(screen.queryByRole('radio', { name: 'Reduced' })).toBeNull();
    });

    it('inspects on hover and focus, and re-centres only on Enter', async () => {
      mockFetch();
      const onFocusChange = vi.fn();
      const { container } = renderTrace(vi.fn(), '/code?view=trace&trace=radial', onFocusChange);
      await waitFor(() => expect(container.querySelector('[data-trace-field="radial"] [data-ring="1"]')).not.toBeNull());
      const node = container.querySelector<SVGGElement>('[data-trace-field="radial"] [data-ring="1"]')!;
      const name = node.getAttribute('aria-label')!.split(' · ')[0]!;
      const readout = container.querySelector('[data-trace-inspect]')!;
      expect(readout.textContent).toMatch(/^hover or focus a symbol/);
      const user = userEvent.setup();
      await user.hover(node);
      expect(readout.textContent).toMatch(new RegExp(`^${name} · `));
      expect(onFocusChange).not.toHaveBeenCalled();
      node.focus();
      await user.keyboard('{Enter}');
      expect(onFocusChange).toHaveBeenCalledWith(expect.objectContaining({ id: node.getAttribute('data-node'), name }));
    });

    it('bands the transit map by the strata read and prints unmeasured depth as such', async () => {
      mockFetch();
      const { container } = renderTrace(vi.fn(), '/code?view=trace&trace=transit');
      await waitFor(() => expect(container.querySelector('[data-trace-field="transit"] svg')?.textContent).toContain('depth 4'));
      expect(requested.some((url) => url.includes('/api/plugins/graph/strata'))).toBe(true);
      const text = container.querySelector('[data-trace-field="transit"] svg')!.textContent ?? '';
      expect(text).toContain('DEPTH UNMEASURED · FILE NOT IN THE STRATA READ');
      expect(text).toContain('NO STATION');
    });

    it('switches renderer through the URL and back to the shipped field', async () => {
      mockFetch();
      const { container } = renderTrace(vi.fn(), '/code?view=trace&trace=plate');
      await waitFor(() => expect(container.querySelector('[data-trace-field="plate"]')).not.toBeNull());
      await userEvent.setup().click(screen.getByRole('radio', { name: 'Spring field' }));
      await waitFor(() => expect(container.querySelector('[data-testid="trace-canvas"]')).not.toBeNull());
      expect(container.querySelector('[data-trace-field]')).toBeNull();
    });
  });
});
