import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SessionInspector } from './SessionInspector.tsx';

/**
 * The transcript drill-down's job is to stay distinguishable from a summary of
 * itself. These assertions pin the three places that distinction is easiest to
 * lose: a page of turns must not read as the whole session, a compaction
 * boundary must state the token exchange it made, and a turn whose body the
 * store no longer holds must not render as an empty message.
 */

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Session transcript drill-down', () => {
  it('renders raw turns with provider, tool, storage kind, and token estimate', async () => {
    renderInspector(sessionPayload());

    expect(await screen.findByText('assistant')).toBeTruthy();
    expect(screen.getByText('tracedecay_context')).toBeTruthy();
    // Provider identity is carried by every turn, not only the first.
    expect(screen.getAllByText('claude-code').length).toBe(3);
    expect(screen.getByText('~412 tokens')).toBeTruthy();
    expect(screen.getByText(/Reading the call graph around/)).toBeTruthy();
  });

  it('says a page is a page, with its loaded range and whether more follow', async () => {
    renderInspector(sessionPayload());

    expect(await screen.findByText(/1–3 of 1,204 · asc order · page size 100 · more pages follow/))
      .toBeTruthy();
  });

  it('renders compaction boundaries with the exact token exchange they made', async () => {
    renderInspector(sessionPayload());

    expect(await screen.findByText('depth 1')).toBeTruthy();
    expect(screen.getByText('tool_activity')).toBeTruthy();
    // Summary tokens ← the source tokens they replaced.
    expect(screen.getByText('1,180 ← 24,900')).toBeTruthy();
    expect(screen.getByText(/lcm expand node:sn-1/)).toBeTruthy();
  });

  it('reports a turn whose body the store no longer holds instead of an empty line', async () => {
    renderInspector(sessionPayload());

    await screen.findByText('assistant');
    const offloaded = document.querySelector('[data-message="m-3"]');
    expect(offloaded?.textContent).toContain('body not held by the store (offloaded)');
  });

  it('labels the compaction ratio as derived from the two counts beside it', async () => {
    renderInspector(sessionPayload());

    expect(
      await screen.findByText(
        /Summaries hold 4\.7% of the source tokens they replaced — derived from the two counts above/,
      ),
    ).toBeTruthy();
  });

  it('withholds the ratio entirely when no source tokens were recorded', async () => {
    renderInspector({
      ...sessionPayload(),
      counts: {
        message_count: 3,
        summary_node_count: 0,
        source_token_count: 0,
        summary_token_count: 0,
        token_estimate_total: 900,
      },
      summary_nodes: [],
    });

    expect(await screen.findByText(/No source tokens are recorded/)).toBeTruthy();
    expect(screen.queryByText(/0\.0% of the source tokens/)).toBeNull();
  });

  it('distinguishes a session the compactor never cut from a page that carried no nodes', async () => {
    renderInspector({
      ...sessionPayload(),
      counts: { ...sessionPayload().counts, summary_node_count: 0 },
      summary_nodes: [],
      has_more_summary_nodes: false,
    });

    expect(await screen.findByText(/the compactor has not cut this session/)).toBeTruthy();
    expect(screen.getByText('Complete · zero findings')).toBeTruthy();
  });

  it('reports a missing transcript as unknown rather than as an empty session', async () => {
    renderInspector({ ...sessionPayload(), exists: false });

    expect(
      await screen.findByText(/the session store holds no transcript under this id/),
    ).toBeTruthy();
    expect(screen.queryByText(/raw messages/)).toBeNull();
  });

  it('pages the transcript through the server rather than loading the corpus', async () => {
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL) =>
        new Response(JSON.stringify({ ...sessionPayload(), requested: String(input) }), {
          status: 200,
        }),
    );
    vi.stubGlobal('fetch', fetchMock);
    renderWith();

    await screen.findByText('assistant');
    const first = String(fetchMock.mock.calls[0]?.[0]);
    expect(first).toContain('limit=100');
    expect(first).toContain('offset=0');
    expect(first).toContain('order=asc');

    await userEvent.click(screen.getByRole('button', { name: /Next page/ }));
    const requested = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(requested.some((url) => url.includes('offset=100'))).toBe(true);
  });
});

function renderInspector(payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <SessionInspector sessionId="claude:035c8f3c" onClose={() => {}} />
    </QueryClientProvider>,
  );
}

function sessionPayload() {
  return {
    exists: true,
    session_id: 'claude:035c8f3c',
    path: '/fast/projects/tracedecay/.tracedecay/sessions.db',
    storage_scope: 'project',
    limit: 100,
    offset: 0,
    order: 'asc',
    has_more: true,
    has_more_messages: true,
    has_more_summary_nodes: true,
    counts: {
      message_count: 1_204,
      summary_node_count: 9,
      source_token_count: 24_900,
      summary_token_count: 1_180,
      token_estimate_total: 431_002,
    },
    messages: [
      {
        message_id: 'm-1',
        session_id: 'claude:035c8f3c',
        ordinal: 1,
        role: 'user',
        content: 'Trace the callers of build_state.',
        tool_name: null,
        source: 'claude-code',
        storage_kind: 'raw',
        store_id: 1,
        summary_node_ids: [],
        timestamp: 1_753_000_000,
        token_estimate: 18,
        metadata_json: null,
        pinned: 0,
      },
      {
        message_id: 'm-2',
        session_id: 'claude:035c8f3c',
        ordinal: 2,
        role: 'assistant',
        content: 'Reading the call graph around build_state.',
        tool_name: 'tracedecay_context',
        source: 'claude-code',
        storage_kind: 'raw',
        store_id: 1,
        summary_node_ids: ['sn-1'],
        timestamp: 1_753_000_060,
        token_estimate: 412,
        metadata_json: null,
        pinned: 0,
      },
      {
        message_id: 'm-3',
        session_id: 'claude:035c8f3c',
        ordinal: 3,
        role: 'tool',
        content: null,
        tool_name: 'tracedecay_callers',
        source: 'claude-code',
        storage_kind: 'offloaded',
        store_id: 1,
        summary_node_ids: ['sn-1'],
        timestamp: null,
        token_estimate: null,
        metadata_json: null,
        pinned: 0,
      },
    ],
    summary_nodes: [
      {
        node_id: 'sn-1',
        session_id: 'claude:035c8f3c',
        category: 'tool_activity',
        depth: 1,
        summary: 'Traced build_state callers across the dashboard and daemon crates.',
        source_type: 'messages',
        source_token_count: 24_900,
        token_count: 1_180,
        created_at: 1_753_000_400,
        latest_at: 1_753_000_900,
        expand_hint: 'lcm expand node:sn-1',
      },
    ],
  };
}
