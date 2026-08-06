import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { LoomPage } from './LoomPage.tsx';

/**
 * What this suite guards is not layout — it is the surface's claims.
 *
 * The Loom draws a real quantity (session start times) beside several absent
 * ones (durations, commits, edits, PRs). Every one of those absences is stated
 * in words somewhere on the page, and every one of those sentences is a thing a
 * future refactor could quietly delete while leaving a page that still renders
 * and still looks correct. These tests exist so that deletion fails.
 */

const NOW = Math.floor(Date.now() / 1000);

const SESSIONS = {
  available: true,
  total: 6053,
  sessions: [
    {
      session_id: 'sess-open',
      provider: 'cursor',
      title: 'Deliver Git primitive runtime',
      started_at: NOW - 7200,
      last_message_at: null,
      messages: 405,
      is_subagent: false,
      models: [{ model: 'gpt-5.6-sol-high' }],
    },
    {
      session_id: 'sess-closed',
      provider: 'claude',
      title: 'Verify QUERY scheduler',
      started_at: NOW - 10_800,
      last_message_at: NOW - 9000,
      messages: 50,
      is_subagent: false,
      models: [],
    },
    {
      session_id: 'sess-hollow',
      provider: 'codex',
      title: null,
      started_at: NOW - 3600,
      last_message_at: null,
      messages: 0,
      is_subagent: true,
      models: [],
    },
  ],
};

/** `lcm_queries` selects a literal `0 AS pinned` and never resolves a source or
 * storage kind for these rows, so every column below the tool name comes back
 * as the null (or zero) the query left behind. */
function chainMessage(over: Record<string, unknown>) {
  return {
    session_id: 'sess-open',
    role: null,
    content: null,
    ordinal: null,
    timestamp: null,
    tool_name: null,
    token_estimate: null,
    pinned: 0,
    source: 'cursor',
    storage_kind: 'message',
    store_id: null,
    summary_node_ids: [],
    metadata_json: null,
    ...over,
  };
}

const CHAIN = {
  exists: true,
  session_id: 'sess-open',
  path: '/home/zack/.tracedecay/projects/project-loom/sessions.db',
  storage_scope: 'profile_sharded',
  order: 'asc',
  limit: 200,
  offset: 0,
  has_more: false,
  has_more_messages: false,
  has_more_summary_nodes: false,
  counts: {
    message_count: 405,
    token_estimate_total: 5583,
    source_token_count: 0,
    summary_node_count: 0,
    summary_token_count: 0,
  },
  summary_nodes: [],
  messages: [
    chainMessage({
      message_id: 'm0',
      role: 'user',
      content: 'Verify durable code-generation restart.',
      ordinal: 0,
      token_estimate: 12,
    }),
    chainMessage({
      message_id: 'm1',
      role: 'assistant',
      content: 'Reading the reconciliation path.',
      ordinal: 1,
      tool_name: 'Read',
      token_estimate: 20,
    }),
    chainMessage({
      message_id: 'm2',
      role: 'assistant',
      content: 'Running the suite.',
      ordinal: 2,
      tool_name: 'Bash',
      token_estimate: 26,
    }),
  ],
};

const TIMELINE = {
  bucket: 'day',
  buckets: [
    { bucket: '2026-07-23', count: 1204, token_estimate: 41_000 },
    { bucket: '2026-07-24', count: 8801, token_estimate: 92_000 },
  ],
};

const TEMPORAL = {
  schema_revision: 1,
  scope: {
    project_id: 'project-loom',
    storage_mode: 'profile_sharded',
    store_root: '/profile/project-loom',
  },
  version: { entity_version: null, graph_version: null },
  time: { valid_time_micros: null, observation_time_micros: 1_784_700_000_000_000 },
  source_watermark: null,
  authorization: { outcome: 'authorized' },
  coverage: {
    completeness: 'complete',
    eligible: 3,
    examined: 3,
    matched: 3,
    excluded: 0,
    omitted: 0,
    unknown: 0,
    denominator: 3,
    unit: 'sessions',
    omission_reasons: [],
  },
  freshness: {
    state: 'fresh',
    observed_at_micros: 1_784_700_000_000_000,
    watermark: null,
  },
  domain_state: 'partial',
  legal_actions: [],
  payload: {
    available: true,
    total: 3,
    sessions: SESSIONS.sessions.map((session, index) => ({
      ...session,
      ended_at: index === 0 ? session.started_at + 1800 : null,
      edited_files_recorded: index === 0,
    })),
    source_statuses: [
      {
        id: 'session_commit',
        label: 'Session ↔ commit',
        state: 'ready',
        authority: 'commit_sessions',
        granularity: 'commit attribution',
        providers: ['cursor'],
        item_count: 1,
        reason: null,
        required_authority: null,
        coverage: {
          completeness: 'complete',
          eligible: 1,
          examined: 1,
          matched: 1,
          omitted: 0,
          unit: 'stored relation rows',
          reason:
            'provider-qualified commit_sessions rows for the displayed session page',
        },
      },
      {
        id: 'session_file',
        label: 'Session → edited file',
        state: 'partial',
        authority: 'sessions.metadata_json $.edited_files[]',
        granularity: 'recorded file rollup',
        providers: ['cursor'],
        item_count: 1,
        reason: 'provider-native rollups only',
        required_authority: null,
        coverage: {
          completeness: 'partial',
          eligible: 3,
          examined: 1,
          matched: 1,
          omitted: 2,
          unit: 'displayed sessions',
          reason: 'only sessions carrying an edited_files rollup are examined',
        },
      },
      {
        id: 'branch_worktree',
        label: 'Branch & worktree spans',
        state: 'ready',
        authority: 'session_git_spans',
        granularity: 'coalesced activity span',
        providers: ['cursor'],
        item_count: 1,
        reason: null,
        required_authority: null,
        coverage: {
          completeness: 'complete',
          eligible: 1,
          examined: 1,
          matched: 1,
          omitted: 0,
          unit: 'stored relation rows',
          reason:
            'provider-qualified session_git_spans rows for the displayed session page',
        },
      },
    ],
    commits: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        commit_sha: 'abc123def456',
        committed_at: NOW - 5000,
        branch: 'main',
        worktree: '/work/tracedecay',
        relation: 'produced',
        evidence: 'transcript',
        confidence: 100,
        span_overlap_kind: 'within_span',
      },
    ],
    edited_files: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        path: 'src/runtime.rs',
        change_type: 'edit',
        hunks: 2,
      },
    ],
    branch_spans: [
      {
        provider: 'cursor',
        session_id: 'sess-open',
        branch: 'main',
        worktree: '/work/tracedecay',
        first_at: NOW - 7200,
        last_at: NOW - 5400,
        event_count: 4,
        source: 'transcript',
      },
    ],
    temporal_refresh: {
      state: 'ready',
      active_generations: 1,
      latest_activated_at_micros: (NOW - 100) * 1_000_000,
      authority: 'session_temporal_generations maintained by the temporal refresh scheduler',
    },
  },
};

function serve(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const hit = Object.entries(routes).find(([path]) => url.includes(path));
    const { status, body } = hit?.[1] ?? { status: 404, body: { status: 'not_found' } };
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    } as Response;
  });
}

const HAPPY = {
  '/api/loom/temporal': { status: 200, body: TEMPORAL },
  '/api/plugins/hermes-lcm/session/': { status: 200, body: CHAIN },
  '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
};

function renderLoom(routes: Record<string, { status: number; body: unknown }> = HAPPY) {
  vi.stubGlobal('fetch', serve(routes));
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <LoomPage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('LoomPage', () => {
  it('does not present unfed Delivery outcomes as a Loom relation', async () => {
    renderLoom();

    const row = await screen.findByText('Deliver Git primitive runtime');
    await userEvent.click(row);

    expect(screen.queryByText('→ delivery outcomes')).toBeNull();
  });

  it('draws the weave from the typed Loom temporal read', async () => {
    const fetchMock = serve(HAPPY);
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <LoomPage />
      </QueryClientProvider>,
    );
    await screen.findByText('Deliver Git primitive runtime');
    const urls = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(urls.some((url) => url.includes('/api/loom/temporal'))).toBe(true);
    expect(urls.some((url) => url.includes('/api/plugins/hermes-lcm/overview'))).toBe(
      false,
    );
  });

  it('uses the Loom temporal read for recorded ends and causal relations', async () => {
    const fetchMock = serve({
      ...HAPPY,
      '/api/loom/temporal': { status: 200, body: TEMPORAL },
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <LoomPage />
      </QueryClientProvider>,
    );

    const row = await screen.findByText('Deliver Git primitive runtime');
    expect(
      fetchMock.mock.calls.some((call) =>
        String(call[0]).includes('/api/loom/temporal'),
      ),
    ).toBe(true);
    await userEvent.click(row);
    expect(await screen.findByText('src/runtime.rs')).toBeTruthy();
    expect(screen.getByText('abc123def456')).toBeTruthy();
    // The extent is a formatted duration, so `getAllByText('30m').length >= 1`
    // would pass on any '30m' anywhere on the page — including one printed
    // against a different term. Read it through the term it belongs to, which
    // is also what pins `formatDurationSeconds` to Loom's epoch-second clock.
    expect(screen.getByText('extent', { selector: 'dt' }).nextElementSibling?.textContent).toBe(
      '30m',
    );
    expect(screen.getByText(/1 active temporal generations/)).toBeTruthy();
    expect(screen.getByText(/complete coverage · 3 examined/)).toBeTruthy();
    expect(
      screen.queryByText(/no session→file or session→commit route/),
    ).toBeNull();
  });

  it('states which threads have no recorded extent, with the real count', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    // One fixture has a durable end, one a last-message observation, and one is open.
    expect(
      screen.getByText(
        /1 of 3 sessions have no\s+recorded end or later message observation/,
      ),
    ).toBeTruthy();
    expect(screen.getByText(/the stub marks unmeasured extent/)).toBeTruthy();
  });

  it('prints a hollow-mark reading rather than hiding zero-message sessions', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    // Singular, because exactly one fixture session reports zero messages —
    // the sentence counts real threads, so its grammar has to follow them.
    expect(
      screen.getByText(/1 drawn hollow is a session the store reports at zero messages/),
    ).toBeTruthy();
  });

  it('says the sub-column offset encodes nothing, so packing is never read as data', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getByText(/it\s+encodes nothing/)).toBeTruthy();
  });

  it('reports each causal source with its real authority or dependency', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getByText('Session ↔ commit')).toBeTruthy();
    expect(screen.getByText('Session → edited file')).toBeTruthy();
    expect(screen.getByText('Branch & worktree spans')).toBeTruthy();
    expect(screen.getByText(/commit_sessions/)).toBeTruthy();
  });

  it('reports a session with no recorded extent as unrecorded', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getAllByText('unrecorded').length).toBeGreaterThanOrEqual(1);
  });

  it('pulls the chain on selection and marks it ordinal-ordered, not timed', async () => {
    renderLoom();
    const row = await screen.findByText('Deliver Git primitive runtime');
    await userEvent.click(row);
    await screen.findByText('ordinal order');
    expect(
      screen.getByText(/The store served no timestamp on any turn/),
    ).toBeTruthy();
    // The measured tools leg of the chain renders from real tool names — in
    // the histogram and again against the turn that invoked it. Two each, so
    // the count is asserted rather than merely "at least one": a name that
    // reached the histogram but never the turn would otherwise pass.
    expect(screen.getAllByText('Read')).toHaveLength(2);
    expect(screen.getAllByText('Bash')).toHaveLength(2);
  });

  it('continues the chain through durable causal rows', async () => {
    renderLoom();
    const row = await screen.findByText('Deliver Git primitive runtime');
    await userEvent.click(row);
    await screen.findByText('→ edited files');
    expect(screen.getByText('src/runtime.rs')).toBeTruthy();
    expect(screen.getByText('abc123def456')).toBeTruthy();
    expect(screen.getByText(/4 events/)).toBeTruthy();
  });

  it('does not render partial commit coverage as a zero result', async () => {
    const sourceStatuses = TEMPORAL.payload.source_statuses.map((source) =>
      source.id === 'session_commit'
        ? {
            ...source,
            state: 'partial',
            reason: 'one providerless legacy attribution was omitted',
            coverage: {
              ...source.coverage,
              completeness: 'partial',
              eligible: 1,
              examined: 1,
              matched: 0,
              omitted: 1,
            },
          }
        : source,
    );
    renderLoom({
      ...HAPPY,
      '/api/loom/temporal': {
        status: 200,
        body: {
          ...TEMPORAL,
          payload: {
            ...TEMPORAL.payload,
            commits: [],
            source_statuses: sourceStatuses,
          },
        },
      },
    });

    await userEvent.click(await screen.findByText('Deliver Git primitive runtime'));
    expect(
      screen.getAllByText(/one providerless legacy attribution was omitted/).length,
    ).toBeGreaterThanOrEqual(1);
    expect(
      screen.queryByText(/commit_sessions has no attribution for this session/),
    ).toBeNull();
  });

  it('distinguishes a store that reports itself unavailable from an empty one', async () => {
    renderLoom({
      '/api/loom/temporal': {
        status: 200,
        body: {
          ...TEMPORAL,
          payload: { ...TEMPORAL.payload, available: false, total: 0, sessions: [] },
        },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
    });
    await screen.findByText(/reported its session store\s+unavailable/);
    expect(screen.queryByText(/No thread to weave/)).toBeNull();
  });

  it('renders the empty weave as an answered question when the store is genuinely empty', async () => {
    renderLoom({
      '/api/loom/temporal': {
        status: 200,
        body: {
          ...TEMPORAL,
          payload: { ...TEMPORAL.payload, total: 0, sessions: [] },
        },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
    });
    await screen.findByText('No thread to weave');
    expect(
      screen.getByText(/answered and holds no sessions in this scope/),
    ).toBeTruthy();
  });

  it('keeps dated sessions visible when the backend also returns an undated row', async () => {
    renderLoom({
      '/api/loom/temporal': {
        status: 200,
        body: {
          ...TEMPORAL,
          payload: {
            ...TEMPORAL.payload,
            sessions: [
              TEMPORAL.payload.sessions[0],
              {
                ...TEMPORAL.payload.sessions[2],
                session_id: 'sess-undated',
                started_at: null,
              },
            ],
          },
        },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
    });

    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getByText(/1 row carried no usable start time/)).toBeTruthy();
    expect(screen.queryByText(/Loom temporal response unavailable/)).toBeNull();
  });

  it('renders a distinct error state when the read fails, inventing nothing', async () => {
    renderLoom({
      '/api/loom/temporal': { status: 500, body: { error: 'boom' } },
    });
    await waitFor(() => {
      expect(screen.getByText(/HTTP 500/)).toBeTruthy();
    });
    expect(screen.queryByText('No thread to weave')).toBeNull();
  });

  it('survives a timeline read failing without losing the weave', async () => {
    renderLoom({
      '/api/loom/temporal': { status: 200, body: TEMPORAL },
      '/api/plugins/hermes-lcm/timeline': { status: 500, body: { error: 'boom' } },
    });
    await screen.findByText('Deliver Git primitive runtime');
    // The busiest-day readout falls to its stated unread state; the threads,
    // which come from a different endpoint, still draw.
    expect(screen.getByText('timeline unread')).toBeTruthy();
  });

  it('gives the canvas an accessible description and a real table alongside', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    const figure = screen.getByRole('img', { name: /Weave:/ });
    expect(figure.getAttribute('aria-label')).toContain('drawn open');
    expect(figure.getAttribute('aria-label')).toContain(
      'Provider-qualified causal rows are served and listed',
    );
    expect(figure.getAttribute('aria-label')).toContain('not geometrically drawn');
    expect(screen.getByRole('table')).toBeTruthy();
  });
});
