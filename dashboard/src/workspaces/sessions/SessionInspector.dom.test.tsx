import type { ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DashboardEnvelopeV1, LoomSessionRowV1, LoomTemporalPayloadV1 } from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import type { ReadState } from '../../ui/ReadSection.tsx';
import { SessionInspector, SessionTranscript } from './SessionInspector.tsx';

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
    renderTranscript(response);

    expect(await screen.findByText('Unknown')).toBeTruthy();
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('assistant')).toBeNull();
    expect(screen.queryByText(/raw messages/)).toBeNull();
  });

  /** The two governed refusals the LCM read routes actually serve
   * (`DashboardLcmReadStateV1::Locked` / `::Redacted` → the envelope's own
   * `domain_state` with a null payload). Each renders as its own chip with the
   * daemon's reason — never as an empty transcript. */
  it.each([
    { state: 'locked', label: 'Locked', reason: 'session_store_sync_lease_held' },
    { state: 'redacted', label: 'Redacted', reason: 'session_content_redacted_by_policy' },
  ])('renders a $state read as its governed chip, not an empty transcript', async ({
    state,
    label,
    reason,
  }) => {
    const response = fixtureEnvelope(null, state);
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
      omission_reasons: [reason],
    };
    renderTranscript(response);

    expect(await screen.findByText(label)).toBeTruthy();
    expect(screen.getByText(new RegExp(reason))).toBeTruthy();
    const chip = document.querySelector(`[data-state="${state}"]`);
    expect(chip).not.toBeNull();
    expect(screen.queryByText(/raw messages/)).toBeNull();
    expect(screen.queryByText(/no transcript/i)).toBeNull();
  });

  it('names unavailable whole-session token metrics without inventing zeroes', async () => {
    renderTranscript(
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

  it('labels visible-content tokenizer counts as approximate and tallies the page', async () => {
    renderTranscript(
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
            {
              ...message('uncounted answer'),
              message_id: 'uncounted answer',
              token_count: null,
              token_count_provenance: 'unavailable',
            },
          ],
        }),
      ),
    );

    expect(await screen.findByText('~13 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('~17 tokens · o200k approximate')).toBeTruthy();
    expect(screen.getByText('token count unavailable')).toBeTruthy();
    expect(document.querySelector('[data-page-token-provenance]')?.textContent).toContain(
      '2 counted (o200k approximate) · 1 unavailable',
    );
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
    renderWith(<SessionTranscript sessionId="claude:035c8f3c" />);

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

describe('Session provenance inspector', () => {
  it('joins the selection to its index row and grades every relation from the daemon evidence', async () => {
    stubTranscript();
    renderWith(
      <SessionInspector
        selection={{ provider: 'claude', sessionId: 'claude:035c8f3c' }}
        index={ready(indexPayload())}
        indexPage={1}
        onSelectProvider={() => {}}
        onClose={() => {}}
      />,
    );

    const identity = screen.getByRole('region', { name: 'Identity' });
    expect(within(identity).getByText('claude')).toBeTruthy();
    expect(within(identity).getByText('gpt-5.6-sol-high')).toBeTruthy();
    expect(within(identity).getByText('no recorded end')).toBeTruthy();
    expect(within(identity).getByText('Verify QUERY scheduler')).toBeTruthy();
    expect(identity.querySelector('[data-evidence-grade="EXACT"]')).not.toBeNull();

    const relations = screen.getByRole('region', { name: 'Git relations' });
    const exact = relations.querySelector('[data-commit="' + 'a'.repeat(40) + '"]');
    const inferred = relations.querySelector('[data-commit="' + 'b'.repeat(40) + '"]');
    expect(exact?.querySelector('[data-evidence-grade="EXACT"]')?.textContent).toContain('TOOL RESULT');
    expect(inferred?.querySelector('[data-evidence-grade="INFERRED"]')?.textContent).toContain(
      'TIME OVERLAP',
    );
    // The daemon's heuristic score is not a grade and is not printed as one.
    expect(relations.textContent).not.toMatch(/confidence/i);
    expect(within(relations).getByText('src/a.ts')).toBeTruthy();
    expect(within(relations).getByText(/feat\/spans · \/fast\/projects\/tracedecay/)).toBeTruthy();

    const pivot = screen.getByRole('link', { name: /Open in Loom/ });
    expect(pivot.getAttribute('href')).toContain('/loom?');
    expect(decodeURIComponent(pivot.getAttribute('href') ?? '')).toContain(
      'loomSession=["claude","claude:035c8f3c"]',
    );
    expect(pivot.getAttribute('href')).toContain('scope=proj-1');

    expect(await screen.findByText('first cursor page')).toBeTruthy();
    expect(screen.getByText(/private chain-of-thought is not a persisted source class/)).toBeTruthy();
  });

  it('types a session that is not on the loaded index page instead of inventing a row', async () => {
    stubTranscript();
    renderWith(
      <SessionInspector
        selection={{ provider: 'codex', sessionId: 'elsewhere' }}
        index={ready(indexPayload())}
        indexPage={3}
        onSelectProvider={() => {}}
        onClose={() => {}}
      />,
    );
    const identity = screen.getByRole('region', { name: 'Identity' });
    expect(identity.querySelector('[data-identity="absent"]')).not.toBeNull();
    expect(within(identity).getByText(/not on loaded index page 3 \(2 of 2,847 sessions\)/)).toBeTruthy();
    expect(identity.querySelector('[data-evidence-grade="UNAVAILABLE"]')).not.toBeNull();
    const relations = screen.getByRole('region', { name: 'Git relations' });
    expect(within(relations).getByText(/relations are read with index page 3/)).toBeTruthy();
    // The transcript is read directly by id regardless of the index page.
    expect(await screen.findByText('first cursor page')).toBeTruthy();
  });

  it('keeps a provider-less id that two providers answer to ambiguous until the reader chooses', async () => {
    stubTranscript();
    const onSelectProvider = vi.fn();
    renderWith(
      <SessionInspector
        selection={{ provider: null, sessionId: 'claude:035c8f3c' }}
        index={ready(
          indexPayload({
            sessions: [indexRow(), indexRow({ provider: 'codex', started_at: null, messages: 4 })],
          }),
        )}
        indexPage={1}
        onSelectProvider={onSelectProvider}
        onClose={() => {}}
      />,
    );
    const identity = screen.getByRole('region', { name: 'Identity' });
    expect(identity.querySelector('[data-identity="ambiguous"]')).not.toBeNull();
    expect(identity.querySelector('[data-evidence-grade="AMBIGUOUS"]')).not.toBeNull();
    expect(screen.queryByRole('link', { name: /Open in Loom/ })).toBeNull();
    await userEvent.click(within(identity).getByRole('button', { name: /codex/ }));
    expect(onSelectProvider).toHaveBeenCalledWith('codex');
  });

  it('reports a blocked index read as the reason identity and relations are missing', () => {
    stubTranscript();
    renderWith(
      <SessionInspector
        selection={{ provider: 'claude', sessionId: 'claude:035c8f3c' }}
        index={{ kind: 'blocked', state: 'offline', detail: 'daemon unreachable' }}
        indexPage={1}
        onSelectProvider={() => {}}
        onClose={() => {}}
      />,
    );
    const identity = screen.getByRole('region', { name: 'Identity' });
    expect(identity.querySelector('[data-state="offline"]')).not.toBeNull();
    const relations = screen.getByRole('region', { name: 'Git relations' });
    expect(relations.querySelector('[data-state="offline"]')).not.toBeNull();
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

function indexRow(over: Partial<LoomSessionRowV1> = {}): LoomSessionRowV1 {
  return {
    session_id: 'claude:035c8f3c',
    provider: 'claude',
    title: 'Verify QUERY scheduler',
    started_at: 1_754_400_000,
    ended_at: null,
    last_message_at: 1_754_403_600,
    messages: 41,
    models: [{ model: null }, { model: 'gpt-5.6-sol-high' }],
    is_subagent: false,
    edited_files_recorded: true,
    ...over,
  };
}

function indexPayload(over: Partial<LoomTemporalPayloadV1> = {}): LoomTemporalPayloadV1 {
  const row = indexRow();
  return {
    available: true,
    total: 2_847,
    sessions: [row, indexRow({ provider: 'cursor', session_id: 'other' })],
    commits: [
      {
        session_id: row.session_id,
        provider: row.provider,
        commit_sha: 'a'.repeat(40),
        committed_at: 1_754_401_000,
        branch: 'master',
        worktree: '/fast/projects/tracedecay',
        relation: 'produced',
        evidence: 'tool_result',
        span_overlap_kind: 'direct',
        confidence: 0.99,
      },
      {
        session_id: row.session_id,
        provider: row.provider,
        commit_sha: 'b'.repeat(40),
        committed_at: 1_754_402_000,
        branch: null,
        worktree: null,
        relation: 'observed',
        evidence: 'time_overlap',
        span_overlap_kind: 'extended_window',
        confidence: 0.31,
      },
    ],
    edited_files: [
      { session_id: row.session_id, provider: row.provider, path: 'src/a.ts', change_type: 'modified', hunks: 2 },
    ],
    branch_spans: [
      {
        session_id: row.session_id,
        provider: row.provider,
        source: 'git_watch',
        branch: 'feat/spans',
        worktree: '/fast/projects/tracedecay',
        first_at: 1_754_400_000,
        last_at: 1_754_403_600,
        event_count: 7,
      },
    ],
    source_statuses: [
      status('session_commit', 'Session → commit', 'ready', 'git correlation graph'),
      status('session_file', 'Session → edited file', 'partial', 'sessions.metadata_json'),
      status('branch_worktree', 'Session → branch/worktree', 'ready', 'git correlation graph'),
    ],
    temporal_refresh: {
      state: 'ready',
      active_generations: 2,
      latest_activated_at_micros: null,
      authority: 'temporal refresh scheduler',
    },
    ...over,
  };
}

function status(id: string, label: string, state: 'ready' | 'partial', authority: string) {
  return {
    id,
    label,
    state,
    authority,
    granularity: 'session',
    providers: ['claude'],
    item_count: 1,
    reason: null,
    required_authority: null,
    coverage: {
      completeness: state === 'ready' ? 'complete' : 'partial',
      eligible: 2,
      examined: 2,
      matched: 1,
      omitted: 0,
      unit: 'sessions',
      reason: 'every eligible session was read',
    },
  };
}

function ready(payload: LoomTemporalPayloadV1): ReadState<DashboardEnvelopeV1<LoomTemporalPayloadV1>> {
  return {
    kind: 'ready',
    value: fixtureEnvelope(payload) as unknown as DashboardEnvelopeV1<LoomTemporalPayloadV1>,
  };
}

function stubTranscript() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(fixtureEnvelope(sessionPage())), { status: 200 })),
  );
}

function renderTranscript(payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
  );
  renderWith(<SessionTranscript sessionId="claude:035c8f3c" />);
}

function renderWith(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/sessions?scope=proj-1&scopeLabel=Proj']}>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}
