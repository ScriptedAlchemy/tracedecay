// @vitest-environment jsdom

/**
 * The strip must not let a healthy socket vouch for stale data.
 *
 * A canonical refresh can reject, and when it does the reducer stays stale on
 * purpose and does not retry by itself. The link is still `live` at that moment —
 * the transport is fine, the projection is not — so the two readings are
 * independent and the feed reading is the only thing standing between a reader
 * and a dashboard that looks current while knowingly being behind.
 */
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { SseReducerStats } from '../../data/sse/types.ts';
import { projectionSyncFrom, type ProjectionSync } from '../../data/sse/useEvents.tsx';
import { StatusStrip } from './StatusStrip.tsx';

const sync = vi.hoisted(() => ({ value: { kind: 'synced' } as ProjectionSync }));

vi.mock('../../data/sse/useEvents.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../data/sse/useEvents.tsx')>();
  return {
    ...actual,
    // The transport is deliberately pinned to its healthiest reading: every case
    // below then asserts what the strip says about the DATA while the link is
    // live, which is the confusion being guarded against.
    useEventStreamState: () => ({ state: 'live' as const, lastEventAt: null }),
    useProjectionSync: () => sync.value,
  };
});

function stats(over: Partial<SseReducerStats>): SseReducerStats {
  return {
    observedEvents: 0,
    observedIdentities: 0,
    queuedEvents: 0,
    queuedBytes: 0,
    stale: false,
    lastEventRevision: null,
    generation: null,
    canonicalEpoch: 0,
    supersededEpoch: 0,
    canonicalRefreshOutstanding: false,
    refetchReason: null,
    reseed: { phase: 'idle' },
    ...over,
  };
}

describe('the projection sync reading', () => {
  it('is synced only when nothing is owed and nothing failed', () => {
    expect(projectionSyncFrom(stats({}))).toEqual({ kind: 'synced' });
    expect(projectionSyncFrom(stats({ reseed: { phase: 'committed', epoch: 3 } }))).toEqual({
      kind: 'synced',
    });
  });

  it('never reports a rejected refresh as synced', () => {
    const failed = projectionSyncFrom(
      stats({
        stale: true,
        reseed: { phase: 'failed', epoch: 2, reason: 'invalidation rejected' },
      }),
    );

    expect(failed).toEqual({ kind: 'failed', reason: 'invalidation rejected' });
  });

  it('reports owed and superseded refreshes as behind, not as complete', () => {
    expect(projectionSyncFrom(stats({ stale: true, refetchReason: 'overflow' }))).toEqual({
      kind: 'stale',
      reason: 'overflow',
    });
    expect(
      projectionSyncFrom(
        stats({ canonicalRefreshOutstanding: true, refetchReason: 'revision_gap' }),
      ),
    ).toEqual({ kind: 'stale', reason: 'revision_gap' });
    expect(
      projectionSyncFrom(
        stats({
          refetchReason: 'generation_change',
          reseed: { phase: 'superseded', epoch: 1, outstandingEpoch: 2 },
        }),
      ),
    ).toEqual({ kind: 'stale', reason: 'generation_change' });
  });

  it('distinguishes a running refresh from a settled one', () => {
    expect(projectionSyncFrom(stats({ reseed: { phase: 'in_flight', epoch: 1 } }))).toEqual({
      kind: 'resyncing',
    });
  });

  it('does not claim a reading when no stream is mounted', () => {
    expect(projectionSyncFrom(null)).toEqual({ kind: 'unmounted' });
  });
});

describe('StatusStrip', () => {
  it('says the projection is behind even while the link is live', () => {
    sync.value = { kind: 'failed', reason: 'invalidation rejected' };
    render(<StatusStrip />);

    expect(screen.getByText('live')).toBeTruthy();
    expect(screen.getByText('resync failed')).toBeTruthy();
    expect(screen.getByText(/invalidation rejected/)).toBeTruthy();
    expect(screen.queryByText('synced')).toBeNull();
  });

  it('names why a refresh is owed rather than only that one is', () => {
    sync.value = { kind: 'stale', reason: 'overflow' };
    render(<StatusStrip />);

    expect(screen.getByText('stale')).toBeTruthy();
    expect(screen.getByText(/overflow/)).toBeTruthy();
  });

  it('claims synced only when the reducer does', () => {
    sync.value = { kind: 'synced' };
    render(<StatusStrip />);

    expect(screen.getByText('synced')).toBeTruthy();
    expect(screen.queryByText('stale')).toBeNull();
    expect(screen.queryByText('resync failed')).toBeNull();
  });

  it('reports an unmounted stream as having no stream, not as synced', () => {
    sync.value = { kind: 'unmounted' };
    render(<StatusStrip />);

    expect(screen.getByText('no stream')).toBeTruthy();
    expect(screen.queryByText('synced')).toBeNull();
  });

  /**
   * The state and the reason are one announcement.
   *
   * The live region used to wrap the status word by itself, with the sentence
   * that explains it in a sibling element outside. Both change at the same
   * moment, so a reader listening to the strip was told "resync failed" and
   * nothing else — the half that says what to do about it updated silently.
   */
  it('announces the reason with the state, not beside it', () => {
    sync.value = { kind: 'failed', reason: 'invalidation rejected' };
    render(<StatusStrip />);

    const announced = screen
      .getAllByRole('status')
      .map((region) => region.textContent ?? '')
      .find((text) => text.includes('resync failed'));

    expect(announced).toBeDefined();
    expect(announced).toContain('invalidation rejected');
  });

  it('announces the reason a refresh is owed inside the same region', () => {
    sync.value = { kind: 'stale', reason: 'overflow' };
    render(<StatusStrip />);

    const announced = screen
      .getAllByRole('status')
      .map((region) => region.textContent ?? '')
      .find((text) => text.includes('stale'));

    expect(announced).toContain('a refresh is owed: overflow');
  });

  it('leaves the link reading alone, which has no reason to carry', () => {
    // The two cells are separate regions: folding the feed's sentence into the
    // link's would announce a transport claim the transport did not make.
    sync.value = { kind: 'failed', reason: 'invalidation rejected' };
    render(<StatusStrip />);

    const link = screen
      .getAllByRole('status')
      .map((region) => region.textContent ?? '')
      .find((text) => text.includes('live'));

    expect(link).toBe('live');
  });
});
