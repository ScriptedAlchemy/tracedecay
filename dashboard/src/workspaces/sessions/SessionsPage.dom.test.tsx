import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { SessionsPage } from './SessionsPage.tsx';

const TEMPORAL_RETRIEVAL_UNAVAILABLE = {
  schema_revision: 1,
  scope: { project_id: 'project.sessions', storage_mode: 'profile_sharded', store_root: '/data' },
  version: { entity_version: null, graph_version: null },
  time: { valid_time_micros: null, observation_time_micros: 1 },
  source_watermark: null,
  authorization: { outcome: 'authorized' },
  coverage: {
    completeness: 'unknown',
    eligible: null,
    examined: null,
    matched: null,
    excluded: null,
    omitted: null,
    unknown: null,
    denominator: null,
    unit: 'records',
    omission_reasons: ['lcm_temporal_retrieval_not_mounted'],
  },
  freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
  domain_state: 'unknown',
  legal_actions: [],
  payload: null,
};

afterEach(() => {
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

const ID_REFERENCES = ['aria-controls', 'aria-labelledby', 'aria-describedby'] as const;

/** Every id an ARIA reference on the page names but the page did not draw: an
 * `aria-controls` naming an absent element is a critical
 * `aria-valid-attr-value` failure. */
function danglingReferences(container: HTMLElement): string[] {
  const offences: string[] = [];
  const selector = ID_REFERENCES.map((attribute) => `[${attribute}]`).join(',');
  for (const element of Array.from(container.querySelectorAll(selector))) {
    for (const attribute of ID_REFERENCES) {
      const value = element.getAttribute(attribute);
      if (value === null) continue;
      for (const id of value.split(/\s+/).filter((token) => token !== '')) {
        if (element.ownerDocument.getElementById(id) === null) {
          offences.push(`${element.tagName.toLowerCase()} ${attribute}="${id}"`);
        }
      }
    }
  }
  return offences;
}

describe('SessionsPage typed authorities', () => {
  it('reports the unavailable canonical temporal authority on every LCM read without fake rows', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(TEMPORAL_RETRIEVAL_UNAVAILABLE)));
    renderPage();

    const detail = await screen.findAllByText(/lcm_temporal_retrieval_not_mounted/);
    // Timeline hero, index, and the overview register's three LCM sections
    // each report the daemon's reason on their own; none collapses into another.
    expect(detail.length).toBeGreaterThanOrEqual(3);
    expect(within(screen.getByRole('region', { name: 'Message volume timeline' })).getByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(within(screen.getByRole('region', { name: 'Sessions' })).getByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText(/no sessions in the current window/i)).toBeNull();
    expect(screen.queryByText(/0 across 0 days/i)).toBeNull();
    expect(document.querySelector('[data-session-row]')).toBeNull();
  });

  it('resolves every ARIA reference', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(TEMPORAL_RETRIEVAL_UNAVAILABLE)));
    const { container } = renderPage();
    await screen.findAllByText(/lcm_temporal_retrieval_not_mounted/);
    expect(danglingReferences(container)).toEqual([]);
  });
});

describe('SessionsPage index, selection and URL state', () => {
  it('reads the deep-linked page, rows, bucket and window and opens the selected session', async () => {
    const fetchMock = stubRoutes();
    renderPage(
      '/sessions?sessionId=sess-52&sessionProvider=codex&sessionsPage=2&sessionsRows=50&sessionsBucket=hour&sessionsWindow=2000',
    );

    await screen.findByText('Session provenance');
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map(([input]) => String(input));
      expect(urls.some((url) => url.includes('/api/loom/temporal?limit=50&offset=50'))).toBe(true);
      expect(urls.some((url) => url.includes('/timeline?bucket=hour&limit=2000'))).toBe(true);
    });
    expect(document.querySelector('[data-session-inspector-id]')?.textContent).toBe('sess-52');
    const identity = await screen.findByRole('region', { name: 'Identity' });
    await waitFor(() => expect(identity.querySelector('[data-identity="row"]')).not.toBeNull());
    expect(within(identity).getByText('codex')).toBeTruthy();
    expect(screen.getByRole('radio', { name: 'HOURLY' }).getAttribute('aria-checked')).toBe('true');
    expect(screen.getByRole('radio', { name: '2000' }).getAttribute('aria-checked')).toBe('true');
  });

  it('selects a row into the URL, opens its provenance, and closing keeps the page', async () => {
    stubRoutes();
    renderPage('/sessions?sessionsPage=2');
    const rows = await screen.findAllByRole('button', { name: /sess-/ });
    await userEvent.click(rows[0]!);

    await screen.findByText('Session provenance');
    expect(rows[0]!.getAttribute('aria-pressed')).toBe('true');
    // Page 2 at 25 rows starts at the store's 26th session.
    expect(currentSearch()).toContain('sessionId=sess-26');
    expect(currentSearch()).toContain('sessionProvider=claude');
    expect(currentSearch()).toContain('sessionsPage=2');

    await userEvent.click(screen.getByRole('button', { name: 'Close inspector' }));
    expect(screen.queryByText('Session provenance')).toBeNull();
    expect(screen.getByText('LCM overview')).toBeTruthy();
    expect(currentSearch()).not.toContain('sessionId=');
    expect(currentSearch()).toContain('sessionsPage=2');
  });

  it('marks a hovered row on the time field and dims rows outside a hovered bucket', async () => {
    stubRoutes();
    renderPage();
    const row = (await screen.findAllByText('sess-1'))[0]!.closest('[data-session-row]')!;
    fireEvent.mouseEnter(row);
    await waitFor(() => {
      expect(document.querySelector('[data-inspected-bucket="2026-08-05"]')).not.toBeNull();
    });
    fireEvent.mouseLeave(row);
    await waitFor(() => {
      expect(document.querySelector('[data-inspected-bucket]')).toBeNull();
    });
  });

  it('switches the timeline read when the bucket or window control changes', async () => {
    const fetchMock = stubRoutes();
    renderPage();
    await screen.findAllByText('sess-1');
    await userEvent.click(screen.getByRole('radio', { name: 'HOURLY' }));
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map(([input]) => String(input));
      expect(urls.some((url) => url.includes('/timeline?bucket=hour&limit=400'))).toBe(true);
    });
    expect(currentSearch()).toContain('sessionsBucket=hour');
    await userEvent.click(screen.getByRole('radio', { name: '2000' }));
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map(([input]) => String(input));
      expect(urls.some((url) => url.includes('/timeline?bucket=hour&limit=2000'))).toBe(true);
    });
  });

  it('pages the index through the real limit/offset and resets to page 1 on a rows change', async () => {
    const fetchMock = stubRoutes();
    renderPage();
    await screen.findAllByText('sess-1');
    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map(([input]) => String(input));
      expect(urls.some((url) => url.includes('/api/loom/temporal?limit=25&offset=25'))).toBe(true);
    });
    expect(currentSearch()).toContain('sessionsPage=2');
    await userEvent.selectOptions(screen.getByRole('combobox', { name: /rows per page/i }), '100');
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map(([input]) => String(input));
      expect(urls.some((url) => url.includes('/api/loom/temporal?limit=100&offset=0'))).toBe(true);
    });
    expect(currentSearch()).not.toContain('sessionsPage=');
  });

  it('opens a transcript search hit in the inspector and returns to the same index page on clear', async () => {
    stubRoutes();
    renderPage('/sessions?sessionsPage=2');
    await screen.findAllByText('sess-26');
    const field = screen.getByRole('searchbox', { name: 'Search transcripts' });
    await userEvent.type(field, 'scheduler{enter}');
    const hit = await screen.findByText('the scheduler was verified');
    await userEvent.click(hit.closest('button')!);
    await screen.findByText('Session provenance');
    expect(document.querySelector('[data-session-inspector-id]')?.textContent).toBe('hit-session');
    // The hit's session is not on the loaded index page, and the inspector says so.
    expect(document.querySelector('[data-identity="absent"]')).not.toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Clear search' }));
    await screen.findAllByText('sess-26');
    expect(currentSearch()).toContain('sessionsPage=2');
  });
});

/* -------------------------------------------------------------------------- */

let lastSearch = '';
function LocationProbe() {
  const location = useLocation();
  lastSearch = location.search;
  return null;
}
function currentSearch(): string {
  return lastSearch;
}

function renderPage(entry = '/sessions') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <SessionsPage />
        <LocationProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

const DAY = 1_785_888_000; // 2026-08-05T00:00:00Z

function indexRow(n: number, provider = 'claude') {
  return {
    session_id: `sess-${n}`,
    provider,
    title: n === 1 ? 'Verify QUERY scheduler' : null,
    started_at: DAY + 3_600 * n,
    ended_at: null,
    last_message_at: DAY + 3_600 * n + 900,
    messages: 10 * n,
    models: [{ model: 'gpt-5.6-sol-high' }],
    is_subagent: false,
    edited_files_recorded: false,
  };
}

function timelinePayload() {
  return {
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    exists: true,
    bucket: 'day',
    session_id: null,
    buckets: [
      {
        bucket: '2026-08-04',
        count: 5,
        token_count: null,
        token_count_provenance: 'unavailable',
        known_message_count: 0,
        unknown_message_count: 5,
      },
      {
        bucket: '2026-08-05',
        count: 2,
        token_count: 21,
        token_count_provenance: 'o200k_approximate',
        known_message_count: 2,
        unknown_message_count: 0,
      },
    ],
    node_buckets: [],
    undated: {
      count: 0,
      token_count: null,
      token_count_provenance: 'unavailable',
      known_message_count: 0,
      unknown_message_count: 0,
    },
    coverage: {
      limit: 400,
      returned_buckets: 2,
      total_dated_buckets: 2,
      truncated: false,
      ordering: 'most_recent',
      next_before_bucket: null,
    },
  };
}

function overviewPayload() {
  return {
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    exists: true,
    overview: {
      messages_total: 7,
      sessions_total: 3,
      summary_nodes_total: 0,
      summary_node_sessions_total: 0,
      max_summary_depth: 0,
      role_counts: [{ role: 'assistant', count: 7 }],
      source_counts: [{ source: 'claude', count: 7 }],
      depth_counts: [],
      compression: { source_token_count: null, token_count: null, ratio: null, node_count: 0 },
    },
    latest_sessions: [],
    latest_summary_nodes: [],
    matches: { messages: [], summary_nodes: [] },
    query: '',
    limit: 25,
  };
}

function indexPayload(url: URL) {
  const limit = Number(url.searchParams.get('limit') ?? '25');
  const offset = Number(url.searchParams.get('offset') ?? '0');
  const rows = Array.from({ length: limit }, (_, i) => {
    const ordinal = offset + i + 1;
    return indexRow(ordinal, ordinal % 50 === 2 ? 'codex' : 'claude');
  });
  return {
    available: true,
    total: 2_847,
    sessions: rows,
    commits: [],
    edited_files: [],
    branch_spans: [],
    source_statuses: [],
    temporal_refresh: {
      state: 'ready',
      active_generations: 1,
      latest_activated_at_micros: null,
      authority: 'temporal refresh scheduler',
    },
  };
}

function sessionPayload(sessionId: string) {
  return {
    exists: true,
    session_id: sessionId,
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    limit: 100,
    counts: { message_count: 1, source_token_count: 0, summary_node_count: 0, summary_token_count: 0 },
    messages: [
      {
        content: 'the scheduler was verified',
        message_id: 'm-1',
        metadata_json: null,
        ordinal: 1,
        pinned: 0,
        role: 'assistant',
        session_id: sessionId,
        snippet: null,
        source: 'claude',
        storage_kind: null,
        store_id: 7,
        summary_node_ids: [],
        timestamp: DAY + 60,
        token_count: 9,
        token_count_provenance: 'o200k_approximate',
        tool_name: null,
      },
    ],
    summary_nodes: [],
    has_more: false,
    has_more_messages: false,
    has_more_summary_nodes: false,
    next_cursor: null,
  };
}

function searchPayload(query: string) {
  return {
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    exists: true,
    query,
    limit: 50,
    next_cursor: null,
    engine: 'canonical_temporal',
    engine_detail: { messages: 'canonical_hydration', summary_nodes: 'canonical_temporal_relations' },
    total: { messages: 1, summary_nodes: 0 },
    filters: { role: null, source: null, session_id: null, since: null, until: null },
    matches: {
      messages: [{ ...sessionPayload('hit-session').messages[0], session_id: 'hit-session' }],
      summary_nodes: [],
    },
  };
}

function stubRoutes() {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), 'http://daemon.test');
    const path = url.pathname;
    if (path.endsWith('/hermes-lcm/timeline')) return jsonResponse(fixtureEnvelope(timelinePayload()));
    if (path.endsWith('/hermes-lcm/overview')) return jsonResponse(fixtureEnvelope(overviewPayload()));
    if (path.endsWith('/hermes-lcm/search')) {
      return jsonResponse(fixtureEnvelope(searchPayload(url.searchParams.get('q') ?? '')));
    }
    if (path.includes('/hermes-lcm/session/')) {
      const id = decodeURIComponent(path.slice(path.lastIndexOf('/') + 1));
      return jsonResponse(fixtureEnvelope(sessionPayload(id)));
    }
    if (path.endsWith('/loom/temporal')) return jsonResponse(fixtureEnvelope(indexPayload(url), 'partial'));
    return jsonResponse(TEMPORAL_RETRIEVAL_UNAVAILABLE);
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}
