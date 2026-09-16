import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router';
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
 * "unverified", on the stated grounds that the response could not tell zero
 * from a query failure.
 *
 * It can. `ReadSection` invokes a surface's render function only after a 2xx
 * response satisfies the envelope and route schema; every other reading —
 * offline, 401, 403, a canonical 404/503, an undecodable body, and the 500
 * these graph routes raise when the query fails — renders as that failure
 * instead. So a zero reaching the page has been measured, and the guard was
 * suppressing real figures. It was also an `||`: one zero among the three
 * withheld all three, so a freshly indexed project with symbols but no
 * resolved edges was shown no node count.
 *
 * What follows pins both halves: a measured zero prints, and a read that
 * failed still refuses to print anything.
 */
const wire = (path: string) => resolveFixture(path) as Record<string, unknown>;

function patchEnvelopePayload(fixture: Record<string, unknown>, patch: Record<string, unknown>) {
  return {
    ...fixture,
    payload: { ...(fixture.payload as Record<string, unknown>), ...patch },
  };
}

function jsonOk(body: unknown) {
  return { ok: true, status: 200, json: async () => body } as Response;
}

/** Every graph route answers 200 with a genuinely empty index. */
function serveMeasuredZeros() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/overview')) {
      return jsonOk(
        patchEnvelopePayload(wire('/api/plugins/graph/overview'), {
          totals: { nodes: 0, edges: 0, files: 0 },
          top_connected: [],
        }),
      );
    }
    if (url.includes('/subgraph')) {
      return jsonOk(
        patchEnvelopePayload(wire('/api/plugins/graph/subgraph'), { nodes: [], edges: [] }),
      );
    }
    return jsonOk(
      patchEnvelopePayload(wire('/api/plugins/graph/search'), { total: 0, count: 0, results: [] }),
    );
  });
}

/** Symbols and files, but no edge resolved yet: the case the `||` erased. */
function serveZeroEdgesOnly() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/overview')) {
      return jsonOk(
        patchEnvelopePayload(wire('/api/plugins/graph/overview'), {
          totals: { nodes: 4_210, edges: 0, files: 187 },
        }),
      );
    }
    if (url.includes('/subgraph')) {
      return jsonOk(
        patchEnvelopePayload(wire('/api/plugins/graph/subgraph'), { nodes: [], edges: [] }),
      );
    }
    return jsonOk(
      patchEnvelopePayload(wire('/api/plugins/graph/search'), { total: 0, count: 0, results: [] }),
    );
  });
}

/** The failure the old comment claimed was indistinguishable from a zero. */
function serveReadFailure() {
  return vi.fn(
    async () => ({ ok: false, status: 500, json: async () => ({}) }) as Response,
  );
}

function renderCode(entry = '/code') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <CodePage />
      </MemoryRouter>
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

    // Panel chrome reports transport errors as state chips with the HTTP detail.
    expect((await screen.findAllByText(/HTTP 500/i)).length).toBeGreaterThan(0);
    expect(screen.queryByText(/symbols indexed/i)).toBeNull();
    expect(screen.queryByText(/no symbols are indexed/i)).toBeNull();
  });
});

describe('the URL-stable Code view shell', () => {
  it('exposes the five peer views with Topology selected by default', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        return jsonOk(resolveFixture(pathname, search));
      }),
    );

    renderCode();

    expect(await screen.findByRole('heading', { name: 'Topology' })).toBeTruthy();
    const switcher = screen.getByRole('navigation', { name: 'Code view' });
    expect(within(switcher).getAllByRole('button').map((button) => button.textContent)).toEqual([
      'Atlas',
      'Topology',
      'Trace',
      'Shared Code',
      'Compare',
    ]);
    expect(
      screen
        .getByRole('button', { name: 'Topology' })
        .getAttribute('aria-current'),
    ).toBe('page');
    expect(
      screen
        .getByRole('region', { name: 'Topology' })
        .getAttribute('aria-labelledby'),
    ).toBe('code-view-topology');
    // Atlas has no projection; Trace and Shared Code read one selected symbol
    // and none is selected. Compare carries its own revision selection.
    for (const name of ['Atlas', 'Trace', 'Shared Code']) {
      expect(screen.getByRole<HTMLButtonElement>('button', { name }).disabled).toBe(true);
    }
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Compare' }).disabled).toBe(
      false,
    );
  });

  it('restores Atlas as a disabled, truthful unavailable view', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=atlas');

    expect(await screen.findByText('Atlas is unavailable')).toBeTruthy();
    expect(screen.getByText(/fixed structural treemap projection/i)).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Atlas' }).disabled).toBe(
      true,
    );
    expect(screen.queryByRole('heading', { name: 'Topology' })).toBeNull();
  });

  it('moves and activates views with native keyboard controls', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    const user = userEvent.setup();
    renderCode('/code?symbol=sym-0');
    const topology = screen.getByRole('button', { name: 'Topology' });
    const trace = screen.getByRole<HTMLButtonElement>('button', { name: 'Trace' });
    await waitFor(() => {
      expect(trace.disabled).toBe(false);
    });

    topology.focus();
    await user.keyboard('{Tab}{Enter}');

    expect(document.activeElement).toBe(trace);
    expect(trace.getAttribute('aria-current')).toBe('page');
    expect(await screen.findByRole('heading', { name: /trace ·/i })).toBeTruthy();
  });

  it('maps a published Core URL into Trace without losing its symbol', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?structureLens=core&structureFocus=sym-0');

    expect(await screen.findByRole('heading', { name: /trace ·/i })).toBeTruthy();
    expect(
      screen.getByRole('button', { name: 'Trace' }).getAttribute('aria-current'),
    ).toBe('page');
  });

  it('restores Shared Code from the URL and gates it on a selected symbol', async () => {
    vi.stubGlobal('fetch', serveFixtures());
    renderCode('/code?view=shared-code');

    expect(await screen.findByText('Shared Code needs a selected symbol')).toBeTruthy();
    const control = screen.getByRole<HTMLButtonElement>('button', { name: 'Shared Code' });
    expect(control.getAttribute('aria-current')).toBe('page');
    expect(control.disabled).toBe(true);
  });
});

function serveFixtures() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const { pathname, search } = new URL(String(input), 'http://localhost');
    return jsonOk(resolveFixture(pathname, search));
  });
}

/** The family fixture's envelope, with the daemon's typed refusals patched in
 * the way `code_read_api::code_read_failed` / `family_response` produce them. */
function familyEnvelope(search: string) {
  return resolveFixture('/api/plugins/graph/shared-code/family', search) as Record<
    string,
    unknown
  >;
}

describe('Shared Code: verified exact families of the selected body', () => {
  it('lists both match classes as digest groups with their stitch marks', async () => {
    const fetchMock = serveFixtures();
    vi.stubGlobal('fetch', fetchMock);
    renderCode('/code?view=shared-code&symbol=sym-0');

    expect(await screen.findByRole('heading', { name: /shared code ·/i })).toBeTruthy();
    const conservative = await screen.findByRole('region', { name: 'Conservative exact' });
    const rename = screen.getByRole('region', { name: 'Rename-normalized exact' });
    // The route is read once per class, keyed by the selected occurrence.
    const familyReads = fetchMock.mock.calls
      .map((call) => new URL(String(call[0]), 'http://localhost'))
      .filter((url) => url.pathname.endsWith('/shared-code/family'));
    expect(familyReads.map((url) => url.searchParams.get('match_class')).sort()).toEqual([
      'conservative_exact',
      'rename_normalized_exact',
    ]);
    for (const url of familyReads) {
      expect(url.searchParams.get('symbol_occurrence_id')).toBe('sym-0');
    }
    // Groups, not pairs: one family per class, its authorized total printed
    // beside the page it lists, and the selected body itself not listed as
    // its own copy.
    // `member_count` is the page's authorized count, never a family total: an
    // incomplete family is counted "on this page" and says more follow, so a
    // reader is never told a total about a family the daemon has not finished
    // listing. The selected body is skipped by the serving read and is neither
    // counted nor listed.
    expect(await within(conservative).findByText('2')).toBeTruthy();
    expect(within(conservative).getByText(/members on this page/i)).toBeTruthy();
    expect(within(conservative).getByText(/family incomplete · more members follow/i)).toBeTruthy();
    expect(within(conservative).getByText('Coverage: partial')).toBeTruthy();
    expect(conservative.querySelectorAll('[data-member]')).toHaveLength(2);
    expect(conservative.querySelectorAll('[data-stitch="solid"]').length).toBeGreaterThan(0);
    await waitFor(() => {
      expect(rename.querySelector('[data-family-complete="true"]')).toBeTruthy();
    });
    expect(rename.querySelectorAll('[data-stitch="double"]').length).toBeGreaterThan(0);
    expect(screen.queryByText(/% similar/i)).toBeNull();
  });

  it('follows the family cursor without re-reading the first page', async () => {
    const fetchMock = serveFixtures();
    vi.stubGlobal('fetch', fetchMock);
    const user = userEvent.setup();
    renderCode('/code?view=shared-code&symbol=sym-0');

    const more = await screen.findByRole('button', { name: /load more members/i });
    await user.click(more);

    await waitFor(() => {
      const conservative = screen.getByRole('region', { name: 'Conservative exact' });
      // First page: sym-7, sym-14. Cursor page: sym-21, sym-28.
      expect(conservative.querySelectorAll('[data-member]')).toHaveLength(4);
    });
    // The continuation page's family is `complete` (postings ended there), yet
    // its count is still one page's slice and stays qualified.
    const continuation = screen.getByRole('region', { name: 'More members' });
    expect(within(continuation).getByText(/members on this page/i)).toBeTruthy();
    expect(within(continuation).queryByText(/family incomplete/i)).toBeNull();
    const cursorReads = fetchMock.mock.calls
      .map((call) => new URL(String(call[0]), 'http://localhost'))
      .filter((url) => url.pathname.endsWith('/shared-code/family') && url.searchParams.has('cursor'));
    expect(cursorReads).toHaveLength(1);
    expect(cursorReads[0]!.searchParams.get('cursor')).toBe('cursor.family.page-2');
  });

  it('does not promise another page when scope filtering, not a cursor, made the family incomplete', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/shared-code/family')) {
          const fixture = familyEnvelope(search);
          const payload = fixture.payload as Record<string, unknown>;
          const [family] = payload.families as Array<Record<string, unknown>>;
          return jsonOk({
            ...fixture,
            domain_state: 'partial',
            payload: {
              ...payload,
              coverage: { status: 'partial' },
              families: [{ ...family, complete: false, next_cursor: null }],
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=shared-code&symbol=sym-0');

    await waitFor(() => {
      expect(screen.getAllByText(/no further page in this scope/i)).toHaveLength(2);
    });
    expect(screen.queryByText(/more members follow/i)).toBeNull();
    expect(screen.queryByRole('button', { name: /load more members/i })).toBeNull();
  });

  it('does not call a partial page with no families a measured zero', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/shared-code/family')) {
          const fixture = familyEnvelope(search);
          return jsonOk({
            ...fixture,
            domain_state: 'partial',
            payload: {
              ...(fixture.payload as Record<string, unknown>),
              coverage: { status: 'partial' },
              families: [],
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=shared-code&symbol=sym-0');

    await waitFor(() => {
      expect(screen.getAllByText('No further families on this page.')).toHaveLength(2);
    });
    expect(screen.queryByText(/no verified copies/i)).toBeNull();
  });

  it('re-centres on a listed copy through the URL identity', async () => {
    vi.stubGlobal('fetch', serveFixtures());
    const user = userEvent.setup();
    renderCode('/code?view=shared-code&symbol=sym-0');

    const conservative = await screen.findByRole('region', { name: 'Conservative exact' });
    await waitFor(() => {
      expect(conservative.querySelector('[data-member="sym-7"]')).toBeTruthy();
    });
    const member = conservative.querySelector('[data-member="sym-7"]')!;
    await user.click(within(member as HTMLElement).getAllByRole('button')[0]!);

    // The source follows the new URL identity; the old in-memory row is not
    // kept as the source of the reading.
    await waitFor(() => {
      expect(document.querySelector('[data-shared-code-source="sym-7"]')).toBeTruthy();
    });
    expect(document.querySelector('[data-shared-code-source="sym-0"]')).toBeNull();
  });

  it('reports a missing source as the typed absence, not as zero copies', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/shared-code/family')) {
          const fixture = familyEnvelope(search);
          return jsonOk({
            ...fixture,
            domain_state: 'error',
            payload: null,
            coverage: {
              ...(fixture.coverage as Record<string, unknown>),
              completeness: 'unknown',
              omission_reasons: ['selected_source_not_found'],
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=shared-code&symbol=sym-0');

    expect(
      (await screen.findAllByText(/not in the retained clone index/i)).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText(/no verified copies/i)).toBeNull();
  });

  it('words a too-small body as an exclusion, never as a finding of zero', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/shared-code/family')) {
          const fixture = familyEnvelope(search);
          return jsonOk({
            ...fixture,
            domain_state: 'complete_zero_findings',
            payload: {
              ...(fixture.payload as Record<string, unknown>),
              coverage: { status: 'excluded_too_small', minimum_tokens: 30 },
              families: [],
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=shared-code&symbol=sym-0');

    await waitFor(() => {
      expect(screen.getAllByText('Excluded from automatic discovery')).toHaveLength(2);
    });
    expect(screen.getAllByText(/under the 30-token minimum/i)).toHaveLength(2);
    expect(screen.queryByText(/no verified copies/i)).toBeNull();
  });

  it('carries a budget-partial read as partial coverage beside its families', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/shared-code/family')) {
          const fixture = familyEnvelope(search);
          return jsonOk({
            ...fixture,
            domain_state: 'partial',
            payload: {
              ...(fixture.payload as Record<string, unknown>),
              coverage: { status: 'partial' },
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode('/code?view=shared-code&symbol=sym-0');

    await waitFor(() => {
      expect(screen.getAllByText('Coverage: partial')).toHaveLength(2);
    });
    expect(screen.getAllByText(/not every member of this body/i)).toHaveLength(2);
    expect(document.querySelectorAll('[data-member]').length).toBeGreaterThan(0);
  });
});

describe('Compare: two exact revisions in one union layout', () => {
  it('opens without a symbol and asks for two exact revisions', async () => {
    vi.stubGlobal('fetch', serveFixtures());
    renderCode('/code?view=compare');

    expect(await screen.findByText('Compare needs two exact revisions')).toBeTruthy();
    expect(screen.getByRole('form', { name: /revision selection/i })).toBeTruthy();
    const switcher = screen.getByRole('navigation', { name: 'Code view' });
    expect(
      within(switcher).getByRole('button', { name: 'Compare' }).getAttribute('aria-current'),
    ).toBe('page');
  });

  it('restores a URL selection, reads the union, and draws every change class in place', async () => {
    const fetchMock = serveFixtures();
    vi.stubGlobal('fetch', fetchMock);
    renderCode(
      `/code?view=compare&base=main&base_revision=${'1'.repeat(40)}&head=feature&head_revision=${'2'.repeat(40)}`,
    );

    const regions = await screen.findByRole('list', { name: /file regions in identity order/i });
    const unionReads = fetchMock.mock.calls
      .map((call) => new URL(String(call[0]), 'http://localhost'))
      .filter((url) => url.pathname.endsWith('/compare/union-layout'));
    expect(unionReads).toHaveLength(1);
    expect(unionReads[0]!.searchParams.get('base')).toBe('main');
    expect(unionReads[0]!.searchParams.get('head_revision')).toBe('2'.repeat(40));
    // Identity order is the daemon's: unchanged, changed, added, removed.
    expect(
      Array.from(regions.querySelectorAll('[data-region-change]:not(li)')).map((region) =>
        region.getAttribute('data-region-change'),
      ),
    ).toEqual(['unchanged', 'changed', 'added', 'removed']);
    // The removed region keeps its former space: a head cell that says so.
    const removed = regions.querySelector('[data-region-change="removed"]')!;
    expect(within(removed as HTMLElement).getByText('not in head')).toBeTruthy();
    const added = regions.querySelector('[data-region-change="added"]')!;
    expect(within(added as HTMLElement).getByText('not in base')).toBeTruthy();
    expect(screen.getByText('refs/heads/main')).toBeTruthy();
    expect(screen.getByText('refs/heads/feature')).toBeTruthy();
  });

  it('writes the submitted selection into the URL before reading', async () => {
    const fetchMock = serveFixtures();
    vi.stubGlobal('fetch', fetchMock);
    const user = userEvent.setup();
    renderCode('/code?view=compare');

    await screen.findByRole('form', { name: /revision selection/i });
    await user.type(screen.getByRole('textbox', { name: /base branch/i }), 'main');
    await user.type(screen.getByRole('textbox', { name: /base revision/i }), '1'.repeat(40));
    await user.type(screen.getByRole('textbox', { name: /head branch/i }), 'feature');
    await user.type(screen.getByRole('textbox', { name: /head revision/i }), '2'.repeat(40));
    const form = screen.getByRole('form', { name: /revision selection/i });
    await user.click(within(form).getByRole('button', { name: 'Compare' }));

    await screen.findByRole('list', { name: /file regions in identity order/i });
    expect(
      fetchMock.mock.calls.some((call) =>
        String(call[0]).includes('/compare/union-layout?base=main'),
      ),
    ).toBe(true);
  });

  it('reports a moved reference as stale, never as a comparison of another commit', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { pathname, search } = new URL(String(input), 'http://localhost');
        if (pathname.endsWith('/compare/union-layout')) {
          const fixture = resolveFixture(pathname, search) as Record<string, unknown>;
          return jsonOk({
            ...fixture,
            domain_state: 'stale',
            payload: null,
            coverage: {
              ...(fixture.coverage as Record<string, unknown>),
              completeness: 'unknown',
              omission_reasons: ['selected_revision_changed'],
            },
          });
        }
        return jsonOk(resolveFixture(pathname, search));
      }),
    );
    renderCode(
      `/code?view=compare&base=main&base_revision=${'1'.repeat(40)}&head=feature&head_revision=${'2'.repeat(40)}`,
    );

    expect(
      await screen.findByText(/no longer points at its expected revision/i),
    ).toBeTruthy();
    expect(screen.queryByRole('list', { name: /file regions/i })).toBeNull();
  });
});
