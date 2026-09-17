import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CodeDiagnostics } from './CodeDiagnostics.tsx';

/**
 * `/api/plugins/code-diagnostics` is the broker's own snapshot. The rules
 * under test: engine states are the server's words, a broker with no mounted
 * engines is an honest empty rather than a zero-error claim, a 503 from an
 * absent authority renders as a failed read, and unread analyzer settings are
 * disclosed rather than silently defaulted.
 *
 * The controls add three more, and they are the ones worth guarding: a refresh
 * repaints from the snapshot the SERVER returned rather than from an assumed
 * one, a settings write carries the compare-and-set revision of the reading it
 * was issued against, and the broker's refusal of a stale revision is reported
 * as the distinct thing it is instead of as a generic failure.
 */

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Code diagnostics panel', () => {
  it('renders summary figures, engine states, and attributed diagnostics', async () => {
    stubSnapshot(
      snapshot({
        summary: {
          total_errors: 3,
          total_warnings: 12,
          pending_refreshes: 1,
          last_refresh_age_seconds: 40,
        },
        engines: [
          engine('rust', 'ready'),
          { ...engine('typescript', 'crashed'), last_error: 'tsserver exited with code 1' },
        ],
        diagnostics: [
          {
            language: 'rust',
            source: 'rust-analyzer',
            file: 'crates/tracedecay-graph-db/src/state.rs',
            line_start: 41,
            line_end: 41,
            severity: 'error',
            code: 'E0308',
            message: 'mismatched types',
            enclosing_node: 'GraphState::open',
            updated_at: 1_753_003_600,
          },
        ],
      }),
    );
    renderPanel();

    expect(await screen.findByText('mismatched types')).toBeTruthy();
    expect(screen.getByText('in GraphState::open')).toBeTruthy();
    expect(screen.getByText('[E0308]')).toBeTruthy();
    expect(screen.getByText('3')).toBeTruthy();
    expect(screen.getByText('12')).toBeTruthy();
    // Engine states are the server's words: the crashed engine keeps its error.
    expect(screen.getByText('rust')).toBeTruthy();
    expect(screen.getByText(/tsserver exited with code 1/)).toBeTruthy();
  });

  it('says no engines are mounted instead of claiming zero diagnostics', async () => {
    stubSnapshot(snapshot({}));
    renderPanel();
    expect(
      await screen.findByText(/no diagnostic engines are mounted for this project/i),
    ).toBeTruthy();
    expect(screen.queryByText(/report no diagnostics/i)).toBeNull();
  });

  it('distinguishes a ready engine with nothing to report from an empty broker', async () => {
    stubSnapshot(snapshot({ engines: [engine('rust', 'ready')] }));
    renderPanel();
    expect(
      await screen.findByText(/the mounted engines report no diagnostics/i),
    ).toBeTruthy();
  });

  it('discloses unread analyzer settings', async () => {
    stubSnapshot(
      snapshot({ settings_unavailable: { reason: 'settings file is not valid JSON' } }),
    );
    renderPanel();
    expect(await screen.findByText(/settings file is not valid JSON/)).toBeTruthy();
  });

  it('renders an unavailable authority as a failed read, never a clean report', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ detail: 'canonical daemon diagnostics authority is unavailable' }),
            { status: 503 },
          ),
      ),
    );
    renderPanel();
    // No canonical discriminant on this 503, so the boundary reports a plain
    // failed read; what matters is that no success body renders.
    expect(
      await screen.findByText(/the read failed and nothing is being invented/i),
    ).toBeTruthy();
    expect(screen.queryByText(/report no diagnostics/i)).toBeNull();
    expect(screen.queryByText(/no diagnostic engines are mounted/i)).toBeNull();
  });
});

describe('Code diagnostics controls', () => {
  it('repaints a refresh from the snapshot the server returned, not an assumed one', async () => {
    const calls: Array<{ url: string; method: string }> = [];
    const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, method: init?.method ?? 'GET' });
      // The read reports a broker with nothing measured; the refresh answers
      // with the reading the server took after running the analyzers. If the
      // panel painted optimistically these two would be indistinguishable.
      const body =
        (init?.method ?? 'GET') === 'GET'
          ? snapshot({ engines: [engine('rust', 'ready')] })
          : snapshot({
              engines: [engine('rust', 'ready')],
              summary: {
                total_errors: 2,
                total_warnings: 0,
                pending_refreshes: 0,
                last_refresh_age_seconds: 1,
              },
            });
      return jsonResponse(body);
    });
    vi.stubGlobal('fetch', fetchMock);
    renderPanel();

    await screen.findByText(/the mounted engines report no diagnostics/i);
    await userEvent.click(screen.getByRole('button', { name: /refresh every analyzer engine/i }));

    await waitFor(() => expect(screen.getByText('2')).toBeTruthy());
    expect(
      calls.some((c) => c.method === 'POST' && c.url.endsWith('/api/plugins/code-diagnostics/refresh')),
    ).toBe(true);
  });

  it('refreshes one language through its own route', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init?: RequestInit) => {
        if ((init?.method ?? 'GET') !== 'GET') calls.push(url);
        return jsonResponse(snapshot({ engines: [engine('rust', 'ready')] }));
      }),
    );
    renderPanel();

    await userEvent.click(
      await screen.findByRole('button', { name: /refresh the rust analyzer/i }),
    );

    await waitFor(() =>
      expect(calls).toContain('/api/plugins/code-diagnostics/refresh/rust'),
    );
  });

  it('sends the revision of the reading it was issued against with a settings write', async () => {
    const bodies: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) => {
        if (init?.method === 'PATCH') bodies.push(String(init.body));
        return jsonResponse(
          snapshot({ engines: [engine('rust', 'ready')], settings_revision: 'sha256:abc' }),
        );
      }),
    );
    renderPanel();

    await userEvent.click(
      await screen.findByRole('checkbox', { name: /run the rust analyzer/i }),
    );

    await waitFor(() => expect(bodies.length).toBe(1));
    expect(JSON.parse(bodies[0] ?? '{}')).toEqual({
      expected_revision: 'sha256:abc',
      // Only the key that changed: an omitted field is "leave this alone", so
      // a toggle must not round-trip a whole settings document this panel does
      // not fully render.
      languages: { rust: { enabled: false } },
    });
  });

  it('reports a refused compare-and-set as a changed reading, not a generic failure', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init?: RequestInit) => {
        if (init?.method === 'PATCH') {
          return new Response(
            JSON.stringify({ code: 'code_diagnostics_revision_conflict', detail: 'stale' }),
            { status: 409, headers: { 'content-type': 'application/json' } },
          );
        }
        return jsonResponse(snapshot({ engines: [engine('rust', 'ready')] }));
      }),
    );
    renderPanel();

    await userEvent.click(
      await screen.findByRole('checkbox', { name: /run the rust analyzer/i }),
    );

    expect(
      await screen.findByText(/the analyzer settings changed since this reading/i),
    ).toBeTruthy();
  });

  it('withholds the settings controls when the settings could not be read, keeping refresh', async () => {
    stubSnapshot(
      snapshot({
        engines: [engine('rust', 'ready')],
        settings_unavailable: { reason: 'settings file is not valid JSON' },
      }),
    );
    renderPanel();

    // A patch here would write this panel's defaults over a file nobody has
    // read, so the write controls are disabled. A refresh carries no revision
    // and overwrites nothing, so it stays available.
    expect(await screen.findByRole('checkbox', { name: /run the rust analyzer/i })).toHaveProperty(
      'disabled',
      true,
    );
    expect(screen.getByRole('combobox', { name: /idle backfill mode/i })).toHaveProperty(
      'disabled',
      true,
    );
    expect(
      screen.getByRole('button', { name: /refresh every analyzer engine/i }),
    ).toHaveProperty('disabled', false);
  });
});

function engine(language: string, state: string) {
  return {
    language,
    language_id: language,
    command: `${language}-analyzer`,
    default_command: `${language}-analyzer`,
    args: [],
    enabled: true,
    state,
    install_options: [],
    last_error: null,
    last_diagnostic_update: null,
  };
}

function snapshot(overrides: Record<string, unknown>) {
  return {
    summary: {
      total_errors: 0,
      total_warnings: 0,
      pending_refreshes: 0,
      last_refresh_age_seconds: null,
    },
    engines: [],
    diagnostics: [],
    backfill: {},
    settings: { idle_backfill: 'idle', languages: {}, custom_adapters: [] },
    settings_revision: 'r1',
    ...overrides,
  };
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function stubSnapshot(body: unknown) {
  vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(body)));
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <CodeDiagnostics />
    </QueryClientProvider>,
  );
}
