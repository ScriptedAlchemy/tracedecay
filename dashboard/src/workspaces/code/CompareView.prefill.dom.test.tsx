import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CompareView } from './CompareView.tsx';
import type { CompareSelection } from './compareLayout.ts';

/**
 * Head prefill is an assertion about what the daemon holds, so it is only made
 * when the freshness read leaves one answer: one mounted worktree reporting
 * both a reference and the revision it was sealed on. A head the reader
 * already typed is never replaced, and several mounts are several candidate
 * heads rather than a reason to fill the first one listed.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Compare head prefill', () => {
  it('fills an empty head from the one worktree that reports an exact revision', async () => {
    renderCompare(emptySelection(), [worktree()]);

    await waitFor(() => {
      expect(headFields().branch.value).toBe('codex/tracedecay-total-redesign-plan');
    });
    expect(headFields().revision.value).toBe('2f9c1b7a44d0');
  });

  it('leaves a head the reader typed alone', async () => {
    renderCompare(
      { ...emptySelection(), head: { branch: 'main', revision: 'abc123def456' } },
      [worktree()],
    );

    await screen.findByText(/main @ abc123def456/);
    const head = headFields();
    expect(head.branch.value).toBe('main');
    expect(head.revision.value).toBe('abc123def456');
  });

  it('fills nothing when several mounted worktrees report an exact revision', async () => {
    renderCompare(emptySelection(), [
      worktree(),
      {
        ...worktree(),
        worktree_root: '/fast/projects/tracedecay-review',
        worktree_id: 'worktree.review',
        source_reference: 'refs/heads/review',
        source_revision: '8ce40f1b2a77',
      },
    ]);

    expect(
      await screen.findByText(/Several mounted worktrees report an exact revision/),
    ).toBeTruthy();
    const head = headFields();
    expect(head.branch.value).toBe('');
    expect(head.revision.value).toBe('');
  });

  it('says nothing is prefilled when no worktree reports a revision', async () => {
    renderCompare(emptySelection(), [{ ...worktree(), source_revision: null }]);

    expect(
      await screen.findByText(/has not reported an indexed worktree revision/),
    ).toBeTruthy();
    expect(headFields().branch.value).toBe('');
  });
});

function headFields(): { branch: HTMLInputElement; revision: HTMLInputElement } {
  const fields = screen.getByRole('group', { name: 'Head' });
  const inputs = fields.querySelectorAll('input');
  const [branch, revision] = [...inputs] as HTMLInputElement[];
  if (branch === undefined || revision === undefined) {
    throw new Error('the head fieldset must offer a branch and a revision input');
  }
  return { branch, revision };
}

function emptySelection(): CompareSelection {
  return {
    base: { branch: '', revision: '' },
    head: { branch: '', revision: '' },
    file: '',
    kind: '',
  };
}

function worktree() {
  return {
    worktree_root: '/fast/projects/tracedecay',
    repository_id: 'repository.tracedecay',
    worktree_id: 'worktree.primary',
    source_reference: 'refs/heads/codex/tracedecay-total-redesign-plan',
    source_revision: '2f9c1b7a44d0',
    latest_generation_id: 'generation.4f21c9',
    snapshot_content_identity: 'sha256:8ab31c',
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

function renderCompare(selection: CompareSelection, worktrees: readonly unknown[]) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(
          JSON.stringify(
            envelope({
              worktrees,
              note: 'live daemon scheduler state; generation and scope come from the durable sealed generation',
            }),
          ),
          { status: 200 },
        ),
    ),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <CompareView selection={selection} onSelectionChange={() => {}} />
    </QueryClientProvider>,
  );
}

function envelope(payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
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
    domain_state: 'ready',
    legal_actions: [
      { kind: 'refresh', operation: 'use-case.dashboard.code-index.freshness.refresh' },
    ],
    payload,
  };
}
