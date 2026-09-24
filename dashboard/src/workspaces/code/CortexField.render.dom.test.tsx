/**
 * The Cortex renderer switch and the canvas renderers' shared contract:
 * the shipped Sigma field stays the default, a canvas renderer that cannot
 * draw says so while the ledger keeps every symbol, and the keyboard walks
 * the field by degree, inspecting without pinning until Enter.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, useLocation } from 'react-router';
import { CodePage } from './CodePage.tsx';
import { useStatusRegistersStore } from '../../data/shell/statusRegisters.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

vi.mock('../../viz/graph/GraphCanvas.tsx', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../viz/graph/GraphCanvas.tsx')>()),
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));

function serve() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const { pathname, search } = new URL(String(input), 'http://localhost');
    return { ok: true, status: 200, json: async () => resolveFixture(pathname, search) } as Response;
  });
}

function UrlProbe() {
  const location = useLocation();
  return <output data-testid="url">{`${location.pathname}${location.search}`}</output>;
}

function renderCode(entry: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <CodePage />
        <UrlProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/** A 2D context that accepts every call; jsdom ships none. */
function fakeContext(): CanvasRenderingContext2D {
  const target = {
    measureText: (text: string) => ({ width: text.length * 6 }),
    createImageData: (w: number, h: number) => ({ data: new Uint8ClampedArray(w * h * 4) }),
    createRadialGradient: () => ({ addColorStop: () => {} }),
  } as Record<string, unknown>;
  return new Proxy(target, {
    get: (object, key: string) => (key in object ? object[key] : () => {}),
    set: () => true,
  }) as unknown as CanvasRenderingContext2D;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  useStatusRegistersStore.setState({ owners: new Map() });
});

describe('renderer switch', () => {
  it('keeps the shipped Sigma field when no renderer is asked for', async () => {
    vi.stubGlobal('fetch', serve());
    renderCode('/code');
    expect(await screen.findByTestId('graph-canvas')).toBeTruthy();
    expect(screen.queryByRole('group', { name: /Code cortex/ })).toBeNull();
  });

  it('prints a typed state, not a blank field, when the browser gives no 2D canvas', async () => {
    vi.stubGlobal('fetch', serve());
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code?render=plate');
    const state = await screen.findByText(/this browser gave no 2D canvas, so the 40-symbol field/);
    expect(state.closest('[data-state]')?.getAttribute('data-state')).toBe('unavailable');
    expect(screen.queryByTestId('graph-canvas')).toBeNull();
    const ledger = screen.getByRole('region', { name: 'Symbol ledger' });
    await waitFor(() => expect(within(ledger).getAllByRole('button').length).toBeGreaterThan(0));
  });
});

describe('canvas field keyboard', () => {
  it('walks symbols by degree, inspecting without pinning, and Enter pins', async () => {
    vi.stubGlobal('fetch', serve());
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () => fakeContext() as unknown as RenderingContext,
    );
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      width: 800,
      height: 400,
      right: 800,
      bottom: 400,
      toJSON: () => ({}),
    } as DOMRect);
    renderCode('/code?render=relief');
    const field = await screen.findByRole('group', { name: /Code cortex/ });
    await waitFor(() => expect(field.getAttribute('data-layout')).toBe('ready'));

    field.focus();
    const user = userEvent.setup();
    await user.keyboard('{ArrowRight}');
    expect(
      await screen.findByText(
        'subgraph_payload, function, degree 16, src/dashboard. 1 of 40. Enter pins.',
      ),
    ).toBeTruthy();
    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    await waitFor(() => expect(within(inspector).getAllByText('subgraph_payload').length).toBeGreaterThan(0));
    expect(screen.getByTestId('url').textContent).toBe('/code?render=relief');

    await user.keyboard('{ArrowRight}');
    expect(
      await screen.findByText('resolve_scope, method, degree 15, src/dashboard. 2 of 40. Enter pins.'),
    ).toBeTruthy();

    await user.keyboard('{Enter}');
    await waitFor(() => expect(screen.getByTestId('url').textContent).toBe('/code?render=relief&symbol=sym-1'));
  });
});
