import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CodeDiagnostics } from './CodeDiagnostics.tsx';

/**
 * `/api/plugins/code-diagnostics` is the broker's own snapshot. The rules
 * under test: engine states are the server's words, a broker with no mounted
 * engines is an honest empty rather than a zero-error claim, a 503 from an
 * absent authority renders as a failed read, and unread analyzer settings are
 * disclosed rather than silently defaulted.
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

function stubSnapshot(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    ),
  );
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <CodeDiagnostics />
    </QueryClientProvider>,
  );
}
