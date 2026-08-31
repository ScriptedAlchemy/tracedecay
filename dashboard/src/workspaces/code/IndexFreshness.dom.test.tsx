import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen } from '@testing-library/react';
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
  vi.useRealTimers();
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

  it('renders exact committed build progress from the mounted generation', async () => {
    renderFreshness('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: progress(),
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    const reading = await screen.findByRole('progressbar', { name: 'Code progress' });
    expect(reading.getAttribute('value')).toBe('50');
    expect(reading.getAttribute('max')).toBe('100');
    const panel = reading.closest('[data-code-index-progress]');
    const text = panel?.textContent ?? '';
    expect(text).toContain('bulk commit · 50.0%');
    expect(text).toContain('generation.catchup.01');
    expect(text).toContain('250 / 500 files');
    expect(text).toContain('16 pages committed');
    expect(text).toContain('10k chunks committed');
    expect(text).toContain('480 imports committed');
    expect(text).toContain('16.0 MiB payload committed');
    expect(text).toContain('250 files/s · 16.0 MiB lexical bytes/s');
    expect(text).toContain('ETA 2m');
    expect(text).toContain('last commit');
    expect(text).toContain('240ms');
  });

  it('does not render rate-dependent ETA without an established backend rate', async () => {
    renderFreshness('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: {
            ...progress(),
            files_per_second: null,
            lexical_bytes_per_second: null,
            estimated_remaining_seconds: 120,
            blocked_reason: 'retry_backoff',
          },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    const panel = (await screen.findByRole('progressbar', { name: 'Code progress' })).closest(
      '[data-code-index-progress]',
    );
    const text = panel?.textContent ?? '';
    expect(text).toContain('throughput unavailable');
    expect(text).toContain('ETA unavailable');
    expect(text).toContain('blocked: retry backoff');
    expect(text).not.toContain('0 files/s');
    expect(text).not.toContain('0 B/s');
  });

  it('removes progress when the generation is ready and has no active build', async () => {
    renderFreshness('ready', {
      worktrees: [{ ...worktree(), progress: null }],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });

    await screen.findByText('Ready');
    expect(screen.queryByRole('progressbar', { name: 'Code progress' })).toBeNull();
    expect(screen.queryByText(/throughput unavailable/)).toBeNull();
  });

  it('accepts a later replacement generation after epoch restart and rejects its delayed predecessor', async () => {
    vi.useFakeTimers();
    const first = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: progress(),
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const replacement = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: {
            ...progress(),
            generation_id: 'generation.catchup.02',
            // The daemon restarted before this generation began, so its
            // in-memory progress epoch is lower than the rendered generation.
            daemon_incarnation: 2,
            producer_incarnation: 1,
            progress_epoch: 0,
            last_progress_micros: NOW_MICROS - 1,
            completed_files: 1,
          },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(first), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(replacement), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(first), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('generation.catchup.01')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('generation.catchup.02')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('generation.catchup.01')).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it('accepts a same-generation publication after restart and rejects a delayed pre-restart epoch', async () => {
    vi.useFakeTimers();
    const beforeRestart = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: { ...progress(), progress_epoch: 8 },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const afterRestart = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: {
            ...progress(),
            daemon_incarnation: 2,
            producer_incarnation: 1,
            progress_epoch: 0,
            last_progress_micros: NOW_MICROS - 1,
            completed_files: 1,
          },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(beforeRestart), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(afterRestart), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(beforeRestart), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('250 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('1 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('250 / 500 files')).toBeNull();
  });

  it('accepts a same-daemon remounted producer and rejects its retired predecessor', async () => {
    vi.useFakeTimers();
    const retired = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: { ...progress(), progress_epoch: 100 },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const remounted = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: {
            ...progress(),
            producer_incarnation: 2,
            progress_epoch: 2,
            completed_files: 1,
          },
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(retired), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(remounted), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(retired), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('250 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('1 / 500 files')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.queryByText('250 / 500 files')).toBeNull();
  });

  it('polls an active build each second and returns to the ready cadence', async () => {
    vi.useFakeTimers();
    const active = envelope('loading', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: null,
          snapshot_content_identity: null,
          sealed_at_micros: null,
          staleness_state: 'indexing',
          progress: progress(),
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const ready = envelope('ready', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: 'generation.catchup.01',
          progress: null,
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(active), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('generation.catchup.01')).toBeTruthy();
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('Ready')).toBeTruthy();
    expect(fetch).toHaveBeenCalledTimes(2);
    await advanceTimers(29_998);
    expect(fetch).toHaveBeenCalledTimes(2);
    await advanceTimers(1);
    await advanceTimers(0);
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it('keeps polling ready progress until the freshness envelope is ready', async () => {
    vi.useFakeTimers();
    const readyProgress = {
      ...progress(),
      phase: 'ready',
      completed_files: 500,
      completed_lexical_bytes: 64 * 1024 * 1024,
      estimated_remaining_seconds: 0,
    };
    const transitioning = envelope('partial', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: readyProgress.generation_id,
          staleness_state: 'stale',
          coverage: 'partial',
          progress: readyProgress,
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const ready = envelope('ready', {
      worktrees: [
        {
          ...worktree(),
          latest_generation_id: readyProgress.generation_id,
          progress: readyProgress,
        },
      ],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(transitioning), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('Partial')).toBeTruthy();
    expect(screen.getByText('ready · 100.0%')).toBeTruthy();
    expect(fetch).toHaveBeenCalledTimes(1);
    await advanceTimers(1_001);
    await advanceTimers(0);
    expect(screen.getByText('Ready')).toBeTruthy();
    expect(screen.getByText('ready · 100.0%')).toBeTruthy();
    expect(fetch).toHaveBeenCalledTimes(2);
    await advanceTimers(29_998);
    expect(fetch).toHaveBeenCalledTimes(2);
    await advanceTimers(1);
    await advanceTimers(0);
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it('keeps a mounted ready worktree without progress on the 30-second cadence', async () => {
    vi.useFakeTimers();
    const ready = envelope('ready', {
      worktrees: [{ ...worktree(), progress: null }],
      note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
    });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    renderWith();

    await advanceTimers(0);
    expect(screen.getByText('Ready')).toBeTruthy();
    expect(fetch).toHaveBeenCalledTimes(1);
    await advanceTimers(29_999);
    expect(fetch).toHaveBeenCalledTimes(1);
    await advanceTimers(1);
    expect(fetch).toHaveBeenCalledTimes(2);
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
    latest_generation_id: 'generation.4f21c9',
    snapshot_content_identity: 'sha256:8ab31c',
    source_revision: null,
    sealed_at_micros: NOW_MICROS - 600_000_000,
    last_reconcile_micros: NOW_MICROS,
    staleness_state: 'fresh',
    rebuild_in_flight: false,
    hook_hint_count: 0,
    coverage: 'complete',
    progress: null,
    parked: null,
  };
}

function progress() {
  return {
    generation_id: 'generation.catchup.01',
    daemon_incarnation: 1,
    producer_incarnation: 1,
    progress_epoch: 1,
    sealed_source_digest: 'sha256:sealed-source-catchup',
    phase: 'bulk_commit',
    committed_pages: 16,
    committed_chunks: 10_000,
    committed_imports: 480,
    committed_payload_bytes: 16 * 1024 * 1024,
    completed_files: 250,
    total_files: 500,
    completed_lexical_bytes: 32 * 1024 * 1024,
    total_lexical_bytes: 64 * 1024 * 1024,
    current_batch_pages: 4,
    current_batch_payload_bytes: 4 * 1024 * 1024,
    elapsed_micros: 120_000_000,
    last_commit_latency_micros: 240_000,
    files_per_second: 250,
    lexical_bytes_per_second: 16 * 1024 * 1024,
    estimated_remaining_seconds: 120,
    last_progress_micros: NOW_MICROS,
    blocked_reason: null,
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

async function advanceTimers(milliseconds: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
  });
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
