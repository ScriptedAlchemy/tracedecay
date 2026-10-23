/**
 * The Cortex field's canvas contract: it asks the daemon for its full slice
 * budget, a browser that cannot draw is told so while the ledger keeps every
 * symbol, and the keyboard walks the field by degree, inspecting without
 * pinning until Enter.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, useLocation, useNavigate } from 'react-router';
import { CodePage } from './CodePage.tsx';
import { useStatusRegistersStore } from '../../data/shell/statusRegisters.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

function serve() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const { pathname, search } = new URL(String(input), 'http://localhost');
    return { ok: true, status: 200, json: async () => resolveFixture(pathname, search) } as Response;
  });
}

function UrlProbe() {
  const location = useLocation();
  const navigate = useNavigate();
  return <><output data-testid="url">{`${location.pathname}${location.search}`}</output><button onClick={() => navigate(-1)}>Back in history</button></>;
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

describe('the canvas field', () => {
  it('asks for the daemon ceiling of 250 symbols and 500 relations', async () => {
    const fetchMock = serve();
    vi.stubGlobal('fetch', fetchMock);
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code');
    await screen.findByText(/this browser gave no 2D canvas/);
    const reads = fetchMock.mock.calls
      .map((call) => String(call[0]))
      .filter((url) => url.includes('/subgraph'));
    expect(reads).toEqual(['/api/plugins/graph/subgraph?limit_nodes=250&limit_edges=500']);
  });

  it('prints a typed state, not a blank field, when the browser gives no 2D canvas', async () => {
    vi.stubGlobal('fetch', serve());
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code');
    const state = await screen.findByText(/this browser gave no 2D canvas, so the 250-symbol field/);
    expect(state.closest('[data-state]')?.getAttribute('data-state')).toBe('unavailable');
    const ledger = screen.getByRole('region', { name: 'Symbol ledger' });
    const rows = await within(ledger).findAllByRole('button');
    expect(rows[0]!.textContent).toBe('01path1,840degfunctiongraph_api.rssrc/dashboard');
  });
});

describe('canvas field keyboard', () => {
  it.each(['relief', 'plate'] as const)('walks %s symbols by degree, inspecting without pinning, and Enter pins', async (renderer) => {
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
    renderCode(`/code?render=${renderer}`);
    const field = await screen.findByRole('group', { name: /Code cortex/ });
    await waitFor(() => expect(field.getAttribute('data-layout')).toBe('ready'));

    field.focus();
    const user = userEvent.setup();
    await user.keyboard('{ArrowRight}');
    expect(await screen.findByText(FIRST)).toBeTruthy();
    expect(screen.getByTestId('url').textContent).toBe(`/code?render=${renderer}`);

    await user.keyboard('{ArrowRight}');
    expect(await screen.findByText(SECOND)).toBeTruthy();

    await user.keyboard('{Enter}');
    await waitFor(() => expect(screen.getByTestId('url').textContent).toBe(`/code?render=${renderer}&symbol=${SECOND_ID}`));
  });
});

describe('Cortex canvas modes', () => {
  it('exposes the complete Plate basis and slice/kind readouts as text even when drawing is unavailable', async () => {
    vi.stubGlobal('fetch', serve());
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code?render=plate');
    await screen.findByText(/this browser gave no 2D canvas/);
    const basis = screen.getByRole('region', { name: 'Plate layout basis' });
    await waitFor(() => expect(basis.textContent).toMatch(/longest.path/i));
    expect(basis.textContent).toMatch(/other relations not routed/);
    expect(screen.getByRole('group', { name: 'Slice' }).textContent).toContain('250');
    expect(within(screen.getByRole('group', { name: 'Symbol kind' })).getByRole('list', { name: 'drawn by kind' })).toBeTruthy();
  });

  it('defaults to luminous and exposes every mode even without a canvas context', async () => {
    vi.stubGlobal('fetch', serve());
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code');
    await screen.findByText(/this browser gave no 2D canvas/);
    const selector = screen.getByRole('combobox', { name: 'Cortex canvas view' }) as HTMLSelectElement;
    expect(selector.value).toBe('luminous');
    expect(within(selector).getAllByRole('option').map((option) => option.textContent)).toEqual(['Luminous', 'Plate', 'Relief']);
  });

  it('switches and restores modes through browser history without losing the selected symbol or scope', async () => {
    const fetchMock = serve();
    vi.stubGlobal('fetch', fetchMock);
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderCode('/code?render=relief&symbol=sym-0&scope=project-code&scopeLabel=Code&extra=keep');
    await screen.findByText(/this browser gave no 2D canvas/);
    const canvas = screen.getByRole('group', { name: /Code cortex/ }).querySelector('canvas');
    const selector = screen.getByRole('combobox', { name: 'Cortex canvas view' }) as HTMLSelectElement;
    const readUrl = () => new URL(screen.getByTestId('url').textContent!, 'http://dashboard.test');
    const subgraphReads = () => fetchMock.mock.calls.filter((call) => String(call[0]).includes('/subgraph')).length;
    const before = subgraphReads();
    const user = userEvent.setup();
    await user.selectOptions(selector, 'plate');
    expect(screen.getByRole('group', { name: /Code cortex/ }).querySelector('canvas')).toBe(canvas);
    expect(Object.fromEntries(readUrl().searchParams)).toEqual({ render: 'plate', symbol: 'sym-0', scope: 'project-code', scopeLabel: 'Code', extra: 'keep' });
    expect(await within(screen.getByRole('region', { name: 'Graph register' })).findByText('dependency layers')).toBeTruthy();
    await user.selectOptions(selector, 'luminous');
    expect(screen.getByRole('group', { name: /Code cortex/ }).querySelector('canvas')).toBe(canvas);
    expect(readUrl().searchParams.get('symbol')).toBe('sym-0');
    expect(within(screen.getByRole('region', { name: 'Graph register' })).getByText('force-directed')).toBeTruthy();
    expect(subgraphReads()).toBe(before);
    await user.click(screen.getByRole('button', { name: 'Back in history' }));
    expect(selector.value).toBe('plate');
    expect(readUrl().searchParams.get('render')).toBe('plate');
    expect(readUrl().searchParams.get('scope')).toBe('project-code');
    await user.click(screen.getByRole('button', { name: 'Back in history' }));
    expect(selector.value).toBe('relief');
    expect(within(screen.getByRole('region', { name: 'Graph register' })).getByText('module-packed')).toBeTruthy();
  });
});

const FIRST = 'ProjectRegistry, struct, degree 21, src/domain/identity. 1 of 250. Enter pins.';
const SECOND = 'rank_candidates, method, degree 18, dashboard/src/data/query. 2 of 250. Enter pins.';
const SECOND_ID = 'sym-225';
