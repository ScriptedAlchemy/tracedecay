/**
 * The TRACE drill-in, against the wire-true neighbors fixture.
 *
 * What this suite protects: the readouts tell the truth about what is left
 * out, every drawn symbol is a focusable plate control AND a list row, hover
 * and focus inspect without re-centring, and every call link is drawn.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { TraceView, type TraceFocus } from './TraceView.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

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

function renderTrace(onClose = vi.fn(), onFocusChange?: (node: TraceFocus) => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  const utils = render(
    <QueryClientProvider client={client}>
      <TraceView focus={FOCUS} onClose={onClose} {...(onFocusChange ? { onFocusChange } : {})} />
    </QueryClientProvider>,
  );
  return { ...utils, onClose };
}

beforeEach(() => {
  // jsdom lays nothing out; give the plate's host a real column width.
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
      const plate = container.querySelector('[data-trace-field="plate"] svg')!.textContent ?? '';
      expect(plate).toContain('ENCLOSUREabsent');
    });
  });

  it('draws every drawn symbol as a plate control and as a list row', async () => {
    mockFetch();
    const { container } = renderTrace();
    await screen.findByText(/symbols on the field/i);
    await waitFor(() => expect(container.querySelector('[data-trace-field="plate"] [data-ring="-2"]')).not.toBeNull());
    const field = container.querySelector('[data-trace-field="plate"] svg')!;
    expect(field.getAttribute('aria-label')).toMatch(
      /^Call neighbourhood of resolve_context as an anatomy plate, callers left and callees right on one call-site scale\./,
    );
    expect(field.getAttribute('aria-label')).not.toMatch(/tributar|delta/);
    const targets = field.querySelectorAll('[data-node]');
    const list = container.querySelector('ol')!;
    const items = within(list).getAllByRole('listitem');
    const drawnCount = Number(
      /(\d+) drawn/.exec(screen.getByText(/drawn · ordered by hop/).textContent ?? '')?.[1],
    );
    expect(items.length).toBe(drawnCount);
    expect(targets.length).toBe(drawnCount);
    expect(field.textContent).toContain('7 symbols · 45 sites');
    expect(field.textContent).toContain('ONE SCALE · CALL SITES PER CHANNEL');
    expect(field.textContent).toContain('52 of 90 links run between neighbours: connector only, no bar');
    // Every call link is a connector, and nothing on the surface animates.
    expect(field.querySelectorAll('[data-connectors] > path')).toHaveLength(90);
    expect(container.querySelector('canvas')).toBeNull();
    expect(screen.queryByRole('radiogroup')).toBeNull();
  });

  it('inspects on hover and focus, lifts the lit route, and re-centres only on Enter', async () => {
    mockFetch();
    const onFocusChange = vi.fn();
    const { container } = renderTrace(vi.fn(), onFocusChange);
    await waitFor(() => expect(container.querySelector('[data-trace-field="plate"] [data-ring="-2"]')).not.toBeNull());
    const node = container.querySelector<SVGGElement>('[data-trace-field="plate"] [data-ring="-2"]')!;
    const name = node.getAttribute('aria-label')!.split(' · ')[0]!;
    const readout = container.querySelector('[data-trace-inspect]')!;
    expect(readout.textContent).toMatch(/^Hover or focus a symbol/);
    expect(container.querySelectorAll('[data-lit]')).toHaveLength(0);
    const user = userEvent.setup();
    await user.hover(node);
    expect(readout.textContent).toMatch(new RegExp(`^${name} · `));
    // Its own links plus the hop-1 route inward are lifted in cyan.
    expect(container.querySelectorAll('[data-lit]').length).toBeGreaterThanOrEqual(2);
    expect(onFocusChange).not.toHaveBeenCalled();
    await user.unhover(node);
    // Focus inspects the same way; the 2px ring itself keys off
    // `:focus-visible`, which jsdom does not model, so it is checked in Chrome.
    act(() => node.focus());
    expect(readout.textContent).toMatch(new RegExp(`^${name} · `));
    await user.keyboard('{Enter}');
    expect(onFocusChange).toHaveBeenCalledWith(expect.objectContaining({ id: node.getAttribute('data-node'), name }));
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
    expect(container.querySelector('[data-trace-field]')).toBeNull();
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
