import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LoomPage } from './LoomPage.tsx';
import { useScope } from '../../data/scope/store.ts';

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

/** Canonically hydrated chain message with truthful nullable token accounting. */
function chainMessage(over: Record<string, unknown>) {
  return {
    session_id: 'sess-open',
    role: null,
    content: null,
    snippet: null,
    ordinal: null,
    timestamp: null,
    tool_name: null,
    token_count: null,
    token_count_provenance: null,
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
  limit: 200,
  next_cursor: null,
  has_more: false,
  has_more_messages: false,
  has_more_summary_nodes: false,
  counts: {
    message_count: 405,
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
      token_count: 12,
      token_count_provenance: 'o200k_approximate',
    }),
    chainMessage({
      message_id: 'm1',
      role: 'assistant',
      content: 'Reading the reconciliation path.',
      ordinal: 1,
      tool_name: 'Read',
      token_count: 20,
      token_count_provenance: 'o200k_approximate',
    }),
    chainMessage({
      message_id: 'm2',
      role: 'assistant',
      content: 'Running the suite.',
      ordinal: 2,
      tool_name: 'Bash',
      token_count: null,
      token_count_provenance: null,
    }),
  ],
};

const TIMELINE = {
  path: 'daemon://session-temporal',
  storage_scope: 'project',
  exists: true,
  bucket: 'day',
  session_id: null,
  buckets: [
    {
      bucket: '2026-07-23',
      count: 1204,
      token_count: 41_000,
      token_count_provenance: 'o200k_approximate',
      known_message_count: 1204,
      unknown_message_count: 0,
    },
    {
      bucket: '2026-07-24',
      count: 8801,
      token_count: null,
      token_count_provenance: 'unavailable',
      known_message_count: 8700,
      unknown_message_count: 101,
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

const TEMPORAL_RETRIEVAL_UNAVAILABLE = {
  ...TEMPORAL,
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
  payload: null,
};

function readyEnvelope(payload: unknown) {
  return { ...TEMPORAL, domain_state: 'ready', payload };
}

function serve(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    // Project-scoped dashboard reads travel through the gateway prefix. The
    // fixtures name the canonical route after gateway resolution.
    const canonicalUrl = url.replace(/\/api\/projects\/[^/]+\//, '/api/');
    const hit = Object.entries(routes).find(([path]) => canonicalUrl.includes(path));
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
  '/api/plugins/hermes-lcm/session/': { status: 200, body: TEMPORAL_RETRIEVAL_UNAVAILABLE },
  '/api/plugins/hermes-lcm/timeline': { status: 200, body: TEMPORAL_RETRIEVAL_UNAVAILABLE },
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
  useScope.setState({ scope: { kind: 'all' } });
});

beforeEach(() => {
  // Loom's temporal authority is project-scoped. The dashboard no longer lets
  // the all-projects aggregate silently fall through to whichever project is
  // active, so this fixture selects the exact project represented by TEMPORAL.
  useScope.setState({
    scope: {
      kind: 'project',
      projectId: 'project-loom',
      label: 'project loom',
      activation: 'active',
    },
  });
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
    expect(urls.some((url) => url.includes('/loom/temporal'))).toBe(true);
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
        String(call[0]).includes('/loom/temporal'),
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

  it('reports canonical temporal retrieval as unavailable on selection', async () => {
    renderLoom();
    const row = await screen.findByText('Deliver Git primitive runtime');
    await userEvent.click(row);
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('ordinal order')).toBeNull();
  });

  it('renders chain token provenance and keeps unknown counts unknown', async () => {
    renderLoom({
      ...HAPPY,
      '/api/plugins/hermes-lcm/session/': {
        status: 200,
        body: readyEnvelope(CHAIN),
      },
    });
    await userEvent.click(await screen.findByText('Deliver Git primitive runtime'));

    expect(await screen.findByText('~12 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('~20 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('tokens unknown')).toBeTruthy();
  });

  it('replays only the canonical loaded raw-turn order and keeps compaction linked', async () => {
    const user = userEvent.setup();
    renderLoom({
      ...HAPPY,
      '/api/plugins/hermes-lcm/session/': {
        status: 200,
        body: readyEnvelope({
          ...CHAIN,
          has_more: true,
          has_more_messages: true,
          next_cursor: 'opaque-next-page',
          summary_nodes: [
            {
              category: 'checkpoint',
              created_at: NOW - 100,
              depth: 1,
              expand_hint: 'open the canonical transcript page',
              latest_at: null,
              node_id: 'summary-raw-0',
              recency: null,
              session_id: 'sess-open',
              snippet: 'compacted setup',
              source_token_count: 30,
              source_type: 'message',
              summary: 'The setup was compacted.',
              token_count: 8,
            },
          ],
          messages: [
            chainMessage({
              message_id: 'm2',
              role: 'assistant',
              content: 'Running the suite.',
              ordinal: 2,
              timestamp: NOW - 60,
            }),
            chainMessage({
              message_id: 'm0',
              role: 'user',
              content: 'Verify durable code-generation restart.',
              ordinal: 0,
              timestamp: NOW - 120,
              summary_node_ids: ['summary-raw-0'],
            }),
            chainMessage({
              message_id: 'm1',
              role: 'assistant',
              content: 'Reading the reconciliation path.',
              ordinal: 1,
              timestamp: NOW - 90,
            }),
          ],
        }),
      },
    });

    await user.click(await screen.findByText('Deliver Git primitive runtime'));
    expect(await screen.findByText('stored ordinal 2')).toBeTruthy();
    expect(screen.getByText('following loaded tail')).toBeTruthy();
    expect(screen.getByText(/later pages remain outside this replay/)).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Step to previous stored event' }));
    expect(screen.getByText('stored ordinal 1')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Return replay to latest loaded event' })).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Step to previous stored event' }));
    expect(screen.getByText('stored ordinal 0')).toBeTruthy();
    expect(screen.getByText('linked compaction boundaries')).toBeTruthy();
    expect(screen.getByText(/checkpoint · depth 1/)).toBeTruthy();

    await user.selectOptions(screen.getByLabelText('Replay speed'), '2');
    expect((screen.getByLabelText('Replay speed') as HTMLSelectElement).value).toBe('2');
    await user.click(screen.getByRole('button', { name: 'Play replay' }));
    expect(screen.getByRole('button', { name: 'Pause replay' })).toBeTruthy();
    await user.click(screen.getByRole('button', { name: 'Pause replay' }));
    await user.click(screen.getByRole('button', { name: 'Return replay to latest loaded event' }));
    expect(screen.getByText('following loaded tail')).toBeTruthy();
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
      '/api/plugins/hermes-lcm/timeline': {
        status: 200,
        body: readyEnvelope(TIMELINE),
      },
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
      '/api/plugins/hermes-lcm/timeline': {
        status: 200,
        body: readyEnvelope(TIMELINE),
      },
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
      '/api/plugins/hermes-lcm/timeline': {
        status: 200,
        body: readyEnvelope(TIMELINE),
      },
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
    // The busiest-day readout names the failed read as a failure — not as
    // "unread", which is also what loading looks like; the threads, which
    // come from a different endpoint, still draw.
    expect(screen.getByText('timeline read failed')).toBeTruthy();
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
