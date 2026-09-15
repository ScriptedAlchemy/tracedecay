import { useCallback, useSyncExternalStore, type ReactNode } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import {
  useEventStreamState,
  useProjectionSync,
  type ProjectionSync,
} from '../../data/sse/useEvents.tsx';
import { cn } from '../../ui/cn';
import {
  cancelQueryActivity,
  useActiveQueryActivities,
  useQueryCancellation,
} from '../../data/query/activity.ts';

/**
 * What the transport is doing, and separately whether the data behind it has
 * been reconciled.
 *
 * These are two facts and the strip reports them as two, because a healthy
 * socket says nothing about whether the projection is current: a canonical
 * refresh can reject and leave the client knowingly behind while the link stays
 * `live`. Reading `live` alone in that state would be the strip claiming
 * freshness it has no evidence for.
 */
function feedReading(sync: ProjectionSync): {
  value: string;
  tone: string;
  detail: string | null;
} {
  switch (sync.kind) {
    case 'synced':
      return { value: 'synced', tone: 'bg-state-ready', detail: null };
    case 'resyncing':
      return { value: 'resyncing', tone: 'bg-state-loading', detail: null };
    case 'stale':
      return {
        value: 'stale',
        tone: 'bg-state-stale',
        detail: sync.reason === null ? 'a refresh is owed' : `a refresh is owed: ${sync.reason}`,
      };
    case 'failed':
      return {
        value: 'resync failed',
        tone: 'bg-state-error',
        detail: `the projection is behind and the refresh rejected: ${sync.reason}`,
      };
    case 'unmounted':
      return { value: 'no stream', tone: 'bg-state-offline', detail: null };
    default: {
      const unhandled: never = sync;
      return unhandled;
    }
  }
}

/**
 * Telemetry bar for the real `/api/events` connection state, drawn as a
 * segmented subsystem header: every cell is a numbered register (01 LINK,
 * 02 FEED, 03 SOURCE, 04 QUERY) so the strip reads as the instrument's own
 * status word rather than as a footer of prose.
 */
export function StatusStrip({ queryActivity }: { queryActivity?: ReactNode } = {}) {
  const { state } = useEventStreamState();
  const feed = feedReading(useProjectionSync());
  const link =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', live: true }
      : state === 'connecting'
        ? { value: 'connecting', tone: 'bg-state-loading', live: true }
        : { value: 'down', tone: 'bg-state-offline', live: false };
  return (
    <footer
      className="flex min-h-8 shrink-0 items-stretch border-t border-edge-subtle bg-surface-1"
      aria-label="Status"
    >
      <Cell code="01" label="Link">
        <span
          aria-hidden
          className={cn('size-2 shrink-0', link.tone, link.live && 'td-signal')}
        />
        <span className="td-value text-2xs uppercase" role="status">
          {link.value}
        </span>
      </Cell>
      <Cell code="02" label="Feed">
        <span aria-hidden className={cn('size-2 shrink-0', feed.tone)} />
        {/*
         * One region over the state and its reason together.
         *
         * They used to be two elements with the live region around the word
         * alone, so a reader listening to the strip was told "stale" and never
         * told why — the sentence that says a refresh was rejected sat outside
         * the announcement, updating silently beside it. Both change at the
         * same moment and only mean anything together, so they are announced
         * together.
         */}
        <span
          role="status"
          className="flex min-w-0 items-center gap-1.5"
          data-feed-state={feed.value}
        >
          <span className="td-value text-2xs uppercase">{feed.value}</span>
          {feed.detail !== null && (
            // The state is carried by the word, not the swatch; the reason is
            // the one thing a reader needs to know that the word cannot hold.
            <span className="td-value min-w-0 truncate text-3xs normal-case text-text-muted">
              {feed.detail}
            </span>
          )}
        </span>
      </Cell>
      {queryActivity}
      <span aria-hidden className="flex-1 border-r border-edge-subtle" />
    </footer>
  );
}

/**
 * Where the readings on screen actually come from, so the strip cannot lie by
 * omission: a healthy plate over a dead link is a CAPTURED read, not a live
 * one.
 *
 *   LIVE      the event stream is up; plates follow the daemon.
 *   CAPTURED  the stream is down but resolved reads are still on screen —
 *             fixtures, or the last answers before the link dropped. Stamped
 *             in the alert register because it is exactly the state a reader
 *             must not mistake for live.
 *   NO SOURCE the stream is down and nothing has answered; the plates are
 *             empty frames, which is its own honest state.
 */
export function SourceProvenance() {
  const { state } = useEventStreamState();
  const hasData = useAnyResolvedRead();
  // Only a LIVE stream earns the live stamp. A connecting stream is a stream
  // that is not delivering: whatever the plates show meanwhile is a captured
  // read, and stamping it anything softer would be the strip vouching for
  // freshness it cannot see.
  const source =
    state === 'live'
      ? { value: 'live', tone: 'bg-state-ready', ink: 'text-text-primary' }
      : hasData
        ? { value: 'captured', tone: 'bg-alert', ink: 'text-alert' }
        : { value: 'no source', tone: 'bg-state-offline', ink: 'text-text-muted' };
  return (
    <Cell code="03" label="Source">
      <span aria-hidden className={cn('size-2 shrink-0', source.tone)} />
      <span
        role="status"
        data-source-provenance={source.value}
        className={cn('td-value text-2xs uppercase', source.ink)}
      >
        {source.value}
      </span>
    </Cell>
  );
}

/** Whether any read model on screen has resolved with data — the difference
 * between CAPTURED (plates hold a real read) and NO SOURCE (empty frames). */
function useAnyResolvedRead(): boolean {
  const client = useQueryClient();
  const subscribe = useCallback(
    (onChange: () => void) => client.getQueryCache().subscribe(onChange),
    [client],
  );
  return useSyncExternalStore(subscribe, () =>
    client
      .getQueryCache()
      .getAll()
      .some((query) => query.state.data !== undefined),
  );
}

/** Query-aware cell mounted by Shell, which is inside QueryClientProvider.
 * Keeping it separate lets the transport strip remain usable in isolation
 * without inventing a second QueryClient. */
export function QueryActivityStatus() {
  const client = useQueryClient();
  const queryActivities = useActiveQueryActivities();
  const activeQuery = queryActivities[0];
  const lastCancellation = useQueryCancellation((entry) => entry.lastCancellation);

  if (activeQuery !== undefined) {
    return (
      <Cell code="04" label="Query">
        <span className="td-value max-w-64 truncate text-2xs" role="status">
          {activeQuery.label}
        </span>
        {queryActivities.length > 1 ? (
          <span className="td-value text-3xs text-text-muted">
            +{queryActivities.length - 1}
          </span>
        ) : null}
        {activeQuery.cancelable ? (
          <button
            type="button"
            aria-label={`Cancel ${activeQuery.label}`}
            onClick={() => void cancelQueryActivity(client, activeQuery)}
            className="flex min-h-[var(--touch-target-min)] min-w-11 items-center justify-center border-l border-edge-subtle px-2 text-2xs uppercase text-text-secondary hover:bg-surface-2 hover:text-text-primary"
          >
            Cancel
          </button>
        ) : null}
      </Cell>
    );
  }

  return lastCancellation === null ? null : (
    <Cell code="04" label="Query">
      <span className="td-value max-w-72 truncate text-2xs" role="status">
        cancelled · {lastCancellation.label}
      </span>
    </Cell>
  );
}

/** One numbered register of the strip: segment number engraved first, the
 * subsystem stamp beside it, the reading after the hairline. */
function Cell({
  code,
  label,
  children,
}: {
  code: string;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3">
      <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
        {code}
      </span>
      <span className="td-legend">{label}</span>
      <span className="flex min-w-0 items-center gap-1.5">{children}</span>
    </div>
  );
}
