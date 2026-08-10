import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { IndexFreshness } from './IndexFreshness.tsx';

/**
 * `/api/code-index/freshness` distinguishes four kinds of "not fresh" that a
 * badge would collapse: no scheduler registry attached at all, a registry with
 * no mount for this project, a mount still indexing, and a mount whose sealed
 * generation exists but whose coverage is incomplete. Each keeps its own state
 * and its own sentence here, and none of them may render as fresh.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Code index freshness', () => {
  it('names the exact source reference and sealed generation a fresh read came from', async () => {
    renderFreshness('ready', {
      worktrees: [worktree()],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    expect(await screen.findByText('refs/heads/codex/tracedecay-total-redesign-plan')).toBeTruthy();
    expect(screen.getByText('generation.4f21c9')).toBeTruthy();
    expect(screen.getByText('sha256:8ab31c')).toBeTruthy();
    expect(screen.getByText('fresh')).toBeTruthy();
    expect(screen.getByText('Ready')).toBeTruthy();
    expect(document.querySelector('[data-index-freshness="ready"]')).toBeTruthy();
  });

  it('reports an unattached scheduler registry as unsupported, not as fresh', async () => {
    renderFreshness('unsupported', {
      worktrees: [],
      note: 'the dashboard is not attached to a daemon-owned code-index scheduler registry',
    });

    expect(await screen.findByText('Unsupported')).toBeTruthy();
    expect(screen.getByText(/not attached to a daemon-owned code-index scheduler registry/)).toBeTruthy();
    expect(screen.queryByText('fresh')).toBeNull();
    // Nothing may imply a generation exists.
    expect(screen.queryByText(/generation\./)).toBeNull();
  });

  it('separates an attached registry with no mount from an unattached one', async () => {
    renderFreshness('unknown', {
      worktrees: [],
      note: 'the daemon scheduler registry has no mounted scheduler for this project',
    });

    expect(await screen.findByText('Unknown')).toBeTruthy();
    expect(
      screen.getByText('the daemon scheduler registry has no mounted scheduler for this project'),
    ).toBeTruthy();
    expect(screen.queryByText(/not attached/)).toBeNull();
  });

  it('shows a mount that is still indexing without inventing a generation', async () => {
    renderFreshness('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    expect(await screen.findByText('Loading')).toBeTruthy();
    expect(screen.getByText('no sealed generation yet')).toBeTruthy();
    expect(screen.getByText('indexing')).toBeTruthy();
    // An absent seal time is unreported, never the epoch.
    expect(screen.queryByText(/1970-01-01/)).toBeNull();
    expect(screen.getAllByText('not reported').length).toBeGreaterThan(0);
  });

  it('reports a stale generation as stale against the reference it was sealed on', async () => {
    renderFreshness('partial', {
      worktrees: [{ ...worktree(), staleness_state: 'stale', coverage: 'partial' }],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    expect(await screen.findByText('Partial')).toBeTruthy();
    const mount = document.querySelector('[data-worktree-staleness="stale"]');
    expect(mount).toBeTruthy();
    expect(mount?.textContent).toContain('refs/heads/codex/tracedecay-total-redesign-plan');
    expect(mount?.textContent).toContain('partial');
  });

  it('reports an unreachable daemon as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    renderWith();

    expect(await screen.findByText('Offline')).toBeTruthy();
  });
});

function worktree() {
  return {
    worktree_root: '/fast/projects/tracedecay',
    repository_id: 'repository.tracedecay',
    worktree_id: 'worktree.primary',
    source_reference: 'refs/heads/codex/tracedecay-total-redesign-plan',
    source_revision: null,
    latest_generation_id: 'generation.4f21c9',
    snapshot_content_identity: 'sha256:8ab31c',
    source_revision: null,
    sealed_at_micros: NOW_MICROS - 600_000_000,
    last_reconcile_micros: NOW_MICROS,
    staleness_state: 'fresh',
    hook_hint_count: 0,
    coverage: 'complete',
  };
}

function renderFreshness(domainState: string, payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(envelope(domainState, payload)), { status: 200 }),
    ),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <IndexFreshness />
    </QueryClientProvider>,
  );
}

function envelope(domainState: string, payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: domainState === 'ready' ? 'complete' : 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: 'mounted_worktree',
      omission_reasons: [],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: domainState,
    legal_actions: [
      { kind: 'refresh', operation: 'use-case.dashboard.code-index.freshness.refresh' },
    ],
    payload,
  };
}
