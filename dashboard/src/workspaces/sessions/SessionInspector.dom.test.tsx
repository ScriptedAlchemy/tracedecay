import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { SessionInspector } from './SessionInspector.tsx';

afterEach(() => {
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

describe('Session transcript drill-down', () => {
  it('reports canonical temporal retrieval as unavailable without rendering raw turns', async () => {
    const response = fixtureEnvelope(null, 'unknown');
    response.coverage = {
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
    };
    renderInspector(response);

    expect(await screen.findByText('Unknown')).toBeTruthy();
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('assistant')).toBeNull();
    expect(screen.queryByText(/raw messages/)).toBeNull();
  });

  it('names unavailable whole-session token metrics without inventing zeroes', async () => {
    renderInspector(
      fixtureEnvelope(
        sessionPage({
          counts: {
            message_count: 1,
            source_token_count: null,
            summary_node_count: 0,
            summary_token_count: null,
          },
        }),
      ),
    );

    expect(await screen.findByText('token counts shown per loaded message')).toBeTruthy();
    expect(screen.queryByText('~0 est. tokens')).toBeNull();
    expect(screen.getByText('token counts unavailable')).toBeTruthy();
  });

  it('labels visible-content tokenizer counts as approximate', async () => {
    renderInspector(
      fixtureEnvelope(
        sessionPage({
          messages: [
            {
              ...message('recorded answer'),
              token_count: 13,
              token_count_provenance: 'o200k_approximate',
            },
            {
              ...message('tokenized answer'),
              message_id: 'tokenized answer',
              token_count: 17,
              token_count_provenance: 'o200k_approximate',
            },
          ],
        }),
      ),
    );

    expect(await screen.findByText('~13 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('~17 tokens · o200k approximate')).toBeTruthy();
  });

  it('follows opaque cursors forward and its cursor stack backward', async () => {
    const first = fixtureEnvelope(
      sessionPage({
        messages: [message('first cursor page')],
        next_cursor: 'opaque+cursor/==',
      }),
    );
    const second = fixtureEnvelope(
      sessionPage({
        messages: [message('second cursor page')],
        next_cursor: null,
      }),
    );
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      return new Response(
        JSON.stringify(url.includes('cursor=opaque%2Bcursor%2F%3D%3D') ? second : first),
        { status: 200 },
      );
    });
    vi.stubGlobal('fetch', fetchMock);
    renderWith();

    expect(await screen.findByText('first cursor page')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
    expect(await screen.findByText('second cursor page')).toBeTruthy();

    useScope.getState().selectProject('different-project', 'Different project');
    expect(await screen.findByText('first cursor page')).toBeTruthy();
    expect(
      (screen.getByRole('button', { name: 'Previous page' }) as HTMLButtonElement).disabled,
    ).toBe(true);

    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
    expect(await screen.findByText('second cursor page')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Previous page' }));
    expect(await screen.findByText('first cursor page')).toBeTruthy();

    const requests = fetchMock.mock.calls.map(([input]) => String(input));
    expect(requests.some((url) => url.includes('cursor=opaque%2Bcursor%2F%3D%3D'))).toBe(true);
    expect(requests.some((url) => /[?&](?:offset|order)=/.test(url))).toBe(false);
  });
});

function message(content: string) {
  return {
    content,
    message_id: content,
    metadata_json: null,
    ordinal: null,
    pinned: 0,
    role: 'assistant',
    session_id: 'claude:035c8f3c',
    snippet: null,
    source: null,
    storage_kind: null,
    store_id: null,
    summary_node_ids: [],
    timestamp: null,
    token_count: null,
    token_count_provenance: null,
    tool_name: null,
  };
}

function sessionPage(over: Record<string, unknown> = {}) {
  return {
    exists: true,
    session_id: 'claude:035c8f3c',
    path: 'daemon://session-temporal',
    storage_scope: 'project',
    limit: 100,
    counts: {
      message_count: 1,
      source_token_count: 0,
      summary_node_count: 0,
      summary_token_count: 0,
    },
    messages: [message('first cursor page')],
    summary_nodes: [],
    has_more: false,
    has_more_messages: false,
    has_more_summary_nodes: false,
    next_cursor: null,
    ...over,
  };
}

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
