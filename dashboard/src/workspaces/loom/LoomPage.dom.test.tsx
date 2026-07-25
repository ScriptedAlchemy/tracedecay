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
      title: 'Verify PR9 scheduler',
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

const CHAIN = {
  exists: true,
  session_id: 'sess-open',
  has_more_messages: false,
  counts: { message_count: 405, token_estimate_total: 5583 },
  messages: [
    {
      message_id: 'm0',
      role: 'user',
      content: 'Verify durable code-generation restart.',
      ordinal: 0,
      timestamp: null,
      tool_name: null,
      token_estimate: 12,
    },
    {
      message_id: 'm1',
      role: 'assistant',
      content: 'Reading the reconciliation path.',
      ordinal: 1,
      timestamp: null,
      tool_name: 'Read',
      token_estimate: 20,
    },
    {
      message_id: 'm2',
      role: 'assistant',
      content: 'Running the suite.',
      ordinal: 2,
      timestamp: null,
      tool_name: 'Bash',
      token_estimate: 26,
    },
  ],
};

const TIMELINE = {
  bucket: 'day',
  buckets: [
    { bucket: '2026-07-23', count: 1204, token_estimate: 41_000 },
    { bucket: '2026-07-24', count: 8801, token_estimate: 92_000 },
  ],
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
  '/api/plugins/savings/sessions': { status: 200, body: SESSIONS },
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
  it('draws the weave from the sessions rollup, not from the 500ing overview route', async () => {
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
    expect(urls.some((url) => url.includes('/api/plugins/savings/sessions'))).toBe(true);
    expect(urls.some((url) => url.includes('/api/plugins/hermes-lcm/overview'))).toBe(
      false,
    );
  });

  it('states that most threads have no served end, with the real count', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    // Two of the three fixtures are open-ended.
    expect(
      screen.getByText(/2 of 3 sessions have no\s+served end/),
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

  it('names every unserved causal crossing instead of drawing one', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getByText('Session ↔ commit')).toBeTruthy();
    expect(screen.getByText('Session → edited file')).toBeTruthy();
    expect(screen.getByText('Pull requests & review')).toBeTruthy();
    expect(screen.getByText('CI & release outcomes')).toBeTruthy();
    // The store that DOES hold the correlation is named, so the gap is
    // actionable rather than a shrug.
    expect(screen.getByText('src/sessions/git_correlation.rs')).toBeTruthy();
  });

  it('reports a session with no served end as "not served" in the table', async () => {
    renderLoom();
    await screen.findByText('Deliver Git primitive runtime');
    expect(screen.getAllByText('not served').length).toBeGreaterThanOrEqual(2);
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
    // the histogram and again against the turn that invoked it.
    expect(screen.getAllByText('Read').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('Bash').length).toBeGreaterThanOrEqual(1);
  });

  it('terminates the chain at the wire rather than trailing off into edits', async () => {
    renderLoom();
    const row = await screen.findByText('Deliver Git primitive runtime');
    await userEvent.click(row);
    await screen.findByText('→ edits → commits');
    expect(
      screen.getByText(/no session→file or session→commit route/),
    ).toBeTruthy();
  });

  it('distinguishes a store that reports itself unavailable from an empty one', async () => {
    renderLoom({
      '/api/plugins/savings/sessions': {
        status: 200,
        body: { available: false, sessions: [] },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
    });
    await screen.findByText(/reported its session store\s+unavailable/);
    expect(screen.queryByText(/No thread to weave/)).toBeNull();
  });

  it('renders the empty weave as an answered question when the store is genuinely empty', async () => {
    renderLoom({
      '/api/plugins/savings/sessions': {
        status: 200,
        body: { available: true, sessions: [] },
      },
      '/api/plugins/hermes-lcm/timeline': { status: 200, body: TIMELINE },
    });
    await screen.findByText('No thread to weave');
    expect(
      screen.getByText(/answered and holds no sessions in this scope/),
    ).toBeTruthy();
  });

  it('renders a distinct error state when the read fails, inventing nothing', async () => {
    renderLoom({
      '/api/plugins/savings/sessions': { status: 500, body: { error: 'boom' } },
    });
    await waitFor(() => {
      expect(
        screen.getByText(/nothing is being invented in its place/),
      ).toBeTruthy();
    });
    expect(screen.queryByText('No thread to weave')).toBeNull();
  });

  it('survives a timeline read failing without losing the weave', async () => {
    renderLoom({
      '/api/plugins/savings/sessions': { status: 200, body: SESSIONS },
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
      'No causal crossings are drawn',
    );
    expect(screen.getByRole('table')).toBeTruthy();
  });
});
