/**
 * The Cortex lens's interaction and truth contract.
 *
 *   hover / focus  INSPECTS: the inspector previews the row, the URL does not
 *                  move, and no neighbours read is issued.
 *   click / Enter  PINS: the URL identity moves, the field re-seeds, and the
 *                  neighbours read runs once for the pinned symbol.
 *   Escape         drops a preview and nothing else.
 *
 * Every authority on the inspector and the register reports its own state, and
 * an absence is named rather than drawn as a zero or an empty list.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, useLocation } from 'react-router';
import { CodePage } from './CodePage.tsx';
import { useStatusRegistersStore } from '../../data/shell/statusRegisters.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));

function jsonOk(body: unknown) {
  return { ok: true, status: 200, json: async () => body } as Response;
}

type Patch = (pathname: string, search: string, fixture: Record<string, unknown>) => unknown;

/** Every route from the shared fixtures, with an optional per-route override. */
function serve(patch?: Patch) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const { pathname, search } = new URL(String(input), 'http://localhost');
    const fixture = resolveFixture(pathname, search) as Record<string, unknown>;
    return jsonOk(patch ? patch(pathname, search, fixture) : fixture);
  });
}

function UrlProbe() {
  const location = useLocation();
  return <output data-testid="url">{`${location.pathname}${location.search}`}</output>;
}

function renderCode(entry = '/code') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <CodePage />
        <UrlProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const neighborReads = (fetchMock: ReturnType<typeof vi.fn>) =>
  fetchMock.mock.calls.filter((call) => String(call[0]).includes('/neighbors')).length;

afterEach(() => {
  vi.unstubAllGlobals();
  useStatusRegistersStore.setState({ owners: new Map() });
});

describe('the register strip', () => {
  it('prints the seven cells, deriving modules and density from the served totals', async () => {
    vi.stubGlobal('fetch', serve());
    renderCode();

    const register = await screen.findByRole('region', { name: 'Graph register' });
    await waitFor(() => {
      expect(register.getAttribute('data-graph-register')).toBe('ready');
    });
    const cells = [...register.querySelectorAll('[data-register-cell]')].map((cell) => [
      cell.getAttribute('data-register-cell'),
      cell.getAttribute('data-reading'),
    ]);
    expect(cells).toEqual([
      ['nodes', 'measured'],
      ['edges', 'measured'],
      ['files', 'measured'],
      ['modules', 'measured'],
      ['density', 'measured'],
      ['layout', 'measured'],
      ['rank', 'measured'],
    ]);
    expect(within(register).getByText('force-directed')).toBeTruthy();
    expect(within(register).getByText('degree')).toBeTruthy();
    expect(within(register).queryByText(/eigenvector/i)).toBeNull();
  });

  it('names a missing module kind instead of printing zero modules', async () => {
    vi.stubGlobal(
      'fetch',
      serve((pathname, _search, fixture) =>
        pathname.endsWith('/overview')
          ? {
              ...fixture,
              payload: {
                ...(fixture.payload as Record<string, unknown>),
                nodes_by_kind: [{ kind: 'function', count: 40 }],
              },
            }
          : fixture,
      ),
    );
    renderCode();

    const register = await screen.findByRole('region', { name: 'Graph register' });
    const modules = await within(register).findByText('no module kind in this index');
    expect(modules.closest('[data-register-cell]')?.getAttribute('data-reading')).toBe('absent');
    expect(within(register).queryByText(/^0$/)).toBeNull();
  });
});

describe('hover inspects, click pins', () => {
  it('previews a hovered hub without moving the URL or reading its neighbours', async () => {
    const fetchMock = serve();
    vi.stubGlobal('fetch', fetchMock);
    const user = userEvent.setup();
    renderCode();

    expect(await screen.findByText('No symbol is pinned.')).toBeTruthy();
    const hub = await screen.findByRole('button', { name: /find_direct_child_by_kind/ });
    await user.hover(hub);

    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    expect(await within(inspector).findByText('hover preview · not pinned')).toBeTruthy();
    expect(
      within(inspector).getByRole('heading', { name: 'find_direct_child_by_kind' }),
    ).toBeTruthy();
    expect(screen.getByTestId('url').textContent).toBe('/code');
    expect(neighborReads(fetchMock)).toBe(0);
    // The preview says why callers are not listed rather than listing none.
    const callers = within(inspector).getByRole('region', { name: 'callers' });
    expect(callers.getAttribute('data-relation-state')).toBe('not_read');
    expect(within(callers).getByText(/pin the symbol to read/i)).toBeTruthy();

    await user.keyboard('{Escape}');
    expect(await screen.findByText('No symbol is pinned.')).toBeTruthy();
  });

  it('pins a clicked hub: URL identity, one neighbours read, callers grouped by site', async () => {
    const fetchMock = serve();
    vi.stubGlobal('fetch', fetchMock);
    const user = userEvent.setup();
    renderCode();

    const hub = await screen.findByRole('button', { name: /find_direct_child_by_kind/ });
    const hubId = hub.getAttribute('data-hub') ?? '';
    await user.click(hub);

    expect(screen.getByTestId('url').textContent).toBe(
      `/code?symbol=${encodeURIComponent(hubId)}`,
    );
    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    expect(await within(inspector).findByText('pinned · url identity')).toBeTruthy();
    expect(inspector.querySelector('[data-inspect-block="pinned"]')?.getAttribute('data-inspected')).toBe(hubId);
    const callers = within(inspector).getByRole('region', { name: 'callers' });
    await waitFor(() => {
      expect(callers.getAttribute('data-relation-state')).toBe('measured');
    });
    expect(neighborReads(fetchMock)).toBe(1);
    // One row per distinct caller, with its call-site count, never a decimal.
    const rows = callers.querySelectorAll('[data-neighbor]');
    expect(rows.length).toBeGreaterThan(0);
    expect(within(callers).getAllByText(/sites?$/).length).toBe(rows.length);
    expect(callers.textContent).not.toMatch(/0\.\d\d/);
    // The strip carries the pin.
    const registers = useStatusRegistersStore.getState().owners.get('code') ?? [];
    expect(registers.map((register) => register.id)).toEqual([
      'code:graph',
      'code:index',
      'code:selection',
    ]);
    expect(registers[2]).toMatchObject({ value: 'find_direct_child_by_kind', state: 'identity' });
  });

  it('stacks a preview above the pinned selection and keeps its callers listed', async () => {
    vi.stubGlobal('fetch', serve());
    const user = userEvent.setup();
    renderCode('/code?symbol=sym-0');

    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    await waitFor(() => {
      expect(
        within(inspector).getByRole('region', { name: 'callers' }).getAttribute('data-relation-state'),
      ).toBe('measured');
    });
    // Hover a different hub in the ledger.
    const otherHub = [...document.querySelectorAll<HTMLButtonElement>('[data-hub]')].find(
      (button) => button.getAttribute('data-hub') !== 'sym-0',
    )!;
    await user.hover(otherHub);

    expect(await within(inspector).findByText('hover preview · not pinned')).toBeTruthy();
    expect(inspector.querySelector('[data-inspect-block="preview"]')).toBeTruthy();
    const pinnedBlock = inspector.querySelector('[data-inspect-block="pinned"]');
    expect(pinnedBlock?.getAttribute('data-inspected')).toBe('sym-0');
    expect(within(pinnedBlock as HTMLElement).getByText('selection · pinned')).toBeTruthy();
    expect(
      within(pinnedBlock as HTMLElement).getByRole('button', { name: /trace call topography/i }),
    ).toBeTruthy();
    expect(
      within(inspector).getByRole('region', { name: 'callers' }).getAttribute('data-relation-state'),
    ).toBe('measured');
    expect(screen.getByTestId('url').textContent).toBe('/code?symbol=sym-0');
  });

  it('a search from Trace lands on the Cortex ledger with its matches', async () => {
    vi.stubGlobal('fetch', serve());
    const user = userEvent.setup();
    renderCode('/code?view=trace&symbol=sym-0');

    await screen.findByRole('button', { name: 'Trace' });
    await user.type(screen.getByRole('searchbox', { name: /symbol search/i }), 'resolve');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Cortex' }).getAttribute('aria-current')).toBe('page');
    });
    expect(screen.getByTestId('url').textContent).toBe('/code?symbol=sym-0');
    expect(await screen.findByText(/matches$/)).toBeTruthy();
  });
});

describe('typed absences on the inspector', () => {
  it('reports a failed neighbours read as its state, never as zero callers', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.includes('/neighbors')) {
          return { ok: false, status: 500, json: async () => ({}) } as Response;
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?symbol=sym-0');

    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    const callers = await within(inspector).findByRole('region', { name: 'callers' });
    await waitFor(() => {
      expect(callers.getAttribute('data-relation-state')).toBe('blocked');
    });
    expect(within(callers).getByText(/HTTP 500/)).toBeTruthy();
    expect(within(callers).queryByText(/0 callers/)).toBeNull();
  });

  it('words an unmeasured layering scan as unmeasured, not as depth zero', async () => {
    vi.stubGlobal(
      'fetch',
      serve((pathname, _search, fixture) =>
        pathname.endsWith('/strata')
          ? {
              ...fixture,
              payload: {
                status: 'unmeasured',
                reason: 'graph_warming',
                detail: 'the verified graph is still being sealed',
              },
            }
          : fixture,
      ),
    );
    renderCode('/code?symbol=sym-0');

    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    const strata = await within(inspector).findByRole('region', { name: 'strata' });
    expect(await within(strata).findByText(/graph_warming/)).toBeTruthy();
    expect(strata.querySelector('[data-strata]')).toBeNull();
  });

  it('places the pinned symbol in the layering by its exact file', async () => {
    vi.stubGlobal('fetch', serve());
    renderCode('/code?symbol=sym-0');

    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    const strata = await within(inspector).findByRole('region', { name: 'strata' });
    await waitFor(() => {
      expect(strata.querySelector('[data-strata]')).toBeTruthy();
    });
    expect(strata.textContent).toMatch(/of 7 deep · 5 ideal/);
    expect(strata.textContent).toMatch(/src\/dashboard/);
  });

  it('names a file the layering scan never laid out, with the scan\'s own extent', async () => {
    vi.stubGlobal('fetch', serve());
    const user = userEvent.setup();
    renderCode();

    // This hub fixture lives in a file, and a directory, the strata scan
    // never laid out, so the reading is the absence and the scan's extent.
    await user.click(await screen.findByRole('button', { name: /find_direct_child_by_kind/ }));
    const inspector = screen.getByRole('complementary', { name: 'Inspector' });
    const strata = await within(inspector).findByRole('region', { name: 'strata' });
    await waitFor(() => {
      expect(strata.textContent).toMatch(/not in the layering scan \(\d+ files laid out\)/);
    });
    // Never a silent zero: no depth figure is drawn for a file the scan lacks.
    expect(strata.querySelector('[data-strata]')).toBeNull();
  });
});
